use axum::{
    body::Body,
    extract::{Path, State, Request},
    http::{HeaderMap, Method, StatusCode},
    response::{sse::{Event, Sse}, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use futures::StreamExt as _;
use tokio::sync::{broadcast, mpsc};

pub mod agent_question;
pub mod db;
pub mod pricing;
pub mod rate_limits;
pub mod session_state;

use db::TaskOutcome;

/// Capacity of the live-event fan-out. A slow dashboard that falls this far
/// behind gets lagged out rather than holding memory: it only ever misses
/// change *pings*, and its next poll re-reads the true state anyway.
const EVENT_CHANNEL_CAPACITY: usize = 256;

const DEFAULT_AGENTS_YAML: &str = include_str!("../agents.yaml");
const DEFAULT_PRICING_YAML: &str = include_str!("../pricing.yaml");

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct AgentsFile {
    agents: Vec<AgentConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    OpenAI,
    Anthropic,
}

impl Provider {
    fn as_str(&self) -> &'static str {
        match self {
            Provider::OpenAI => "openai",
            Provider::Anthropic => "anthropic",
        }
    }

    fn target_url(&self) -> &'static str {
        match self {
            Provider::OpenAI => "https://api.openai.com/v1/chat/completions",
            Provider::Anthropic => "https://api.anthropic.com/v1/messages",
        }
    }
}

/// Token/tool-call totals for one proxied call. `prompt_tokens` always means
/// "regular-priced input tokens" (cache tokens are broken out separately),
/// so this maps 1:1 onto PricingTable::estimate_cost's arguments regardless
/// of which provider's accounting style it came from.
#[derive(Debug, Clone, Default)]
struct UsageInfo {
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    tool_calls_count: i64,
}

/// Identity and timing of one proxied call, carried through the response
/// handlers so they can close the task out and announce it without threading
/// four more positional arguments through every signature.
#[derive(Clone)]
struct CallContext {
    task_id: i64,
    agent_name: String,
    session_id: String,
    /// When the request was forwarded. `Instant::elapsed()` on this is the
    /// full call duration; the `latency` passed around separately is only
    /// time-to-headers, which for a stream is a fraction of the real wait.
    started: Instant,
}

impl CallContext {
    fn notify(&self, state: &AppState, kind: &str, status: &str) {
        state.notify(kind, self.task_id, &self.agent_name, &self.session_id, status);
    }

    /// Stores the provider's quota headers, if it sent any. Skipping empty
    /// snapshots keeps the table to actual readings, so "no data" and
    /// "unlimited" stay distinguishable.
    async fn record_rate_limits(&self, state: &AppState, provider: Provider, headers: &HeaderMap) {
        let snapshot = rate_limits::extract(headers);
        if !snapshot.is_empty() {
            let _ = state.db.save_rate_limits(self.task_id, provider.as_str(), &snapshot).await;
        }
    }
}

pub struct AppState {
    pub db: db::Database,
    pub client: reqwest::Client,
    pub agents: Vec<AgentConfig>,
    pub pricing: pricing::PricingTable,
    /// Fan-out for "something changed" pings to any connected dashboard.
    /// Deliberately carries no state of its own: a subscriber re-reads the
    /// analytics endpoints on a ping, so there is exactly one definition of
    /// each number (the SQL) instead of a second one drifting in here.
    pub events: broadcast::Sender<String>,
}

impl AppState {
    /// Publishes a change ping. Sending fails only when nobody is listening,
    /// which is the normal case for a headless proxy — hence the ignored
    /// result rather than an error path.
    fn notify(&self, kind: &str, task_id: i64, agent: &str, session: &str, status: &str) {
        let payload = json!({
            "type": kind,
            "task_id": task_id,
            "agent_name": agent,
            "session_id": session,
            "status": status,
        });
        let _ = self.events.send(payload.to_string());
    }
}

/// Where this run's server should listen and where it should keep its state
/// (database, agents.yaml, pricing.yaml). The standalone binary points
/// `data_dir` at the current directory (unchanged behavior); an embedding
/// host such as the Tauri desktop shell points it at a proper per-OS app
/// data directory instead, since it can't rely on a predictable CWD.
pub struct ServerConfig {
    pub bind_addr: String,
    pub data_dir: PathBuf,
}

/// Writes `contents` to `path` only if nothing is there yet, so a config
/// file bundled as a first-run default stays user-editable afterward and is
/// never silently overwritten on a later launch.
fn seed_default_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::write(path, contents)?;
    }
    Ok(())
}

/// One-time upgrade for installs from before the project (and its db file)
/// were renamed from agent-turn: if `db_path` doesn't exist yet but the old
/// `agent_turn.db` does, rename it into place so `Database::new`'s
/// schema-upgrade logic runs against the caller's existing agents,
/// experiments, tasks, and metrics instead of silently starting empty.
fn migrate_legacy_db(data_dir: &std::path::Path, db_path: &std::path::Path) -> std::io::Result<()> {
    let legacy_path = data_dir.join("agent_turn.db");
    if !db_path.exists() && legacy_path.exists() {
        std::fs::rename(&legacy_path, db_path)?;
    }
    Ok(())
}

pub async fn run(config: ServerConfig) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;

    let agents_path = config.data_dir.join("agents.yaml");
    let pricing_path = config.data_dir.join("pricing.yaml");
    seed_default_file(&agents_path, DEFAULT_AGENTS_YAML)?;
    seed_default_file(&pricing_path, DEFAULT_PRICING_YAML)?;

    let db_path = config.data_dir.join("harnesswurm.db");
    migrate_legacy_db(&config.data_dir, &db_path)?;
    let db = db::Database::new(&format!("sqlite:{}?mode=rwc", db_path.display())).await?;

    let agents: Vec<AgentConfig> = {
        let content = std::fs::read_to_string(&agents_path)?;
        let parsed: AgentsFile = serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!(e))?;
        parsed.agents
    };

    // Pre-populate agents from config
    for agent in &agents {
        let _ = db.get_or_create_agent(&agent.name).await;
    }

    let pricing = pricing::PricingTable::load(&pricing_path);

    // Any call still marked in-flight belongs to a previous process and can
    // never complete, so close those out before serving — otherwise they'd
    // show as agents perpetually "Thinking".
    match db.reap_in_flight_tasks().await {
        Ok(0) => {}
        Ok(n) => println!("Closed out {n} call(s) left in-flight by a previous run"),
        Err(e) => eprintln!("Could not reap in-flight calls: {e}"),
    }

    let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    let state = Arc::new(AppState {
        db,
        client: reqwest::Client::new(),
        agents,
        pricing,
        events,
    });

    // Permissive: this server only ever binds to loopback, and its callers
    // are the Vite dev server (http://localhost:5173) and the Tauri webview
    // (whose origin varies by OS/version) fetching analytics data — without
    // this, both send cross-origin requests the browser blocks outright, so
    // the Traffic tab and Analytics dashboard stay empty.
    let cors = tower_http::cors::CorsLayer::permissive();

    let app = Router::new()
        .route("/v1/chat/completions", post(openai_proxy_handler))
        .route("/v1/messages", post(anthropic_proxy_handler))
        .route("/v1/analytics/experiments", get(get_experiments))
        .route("/v1/analytics/experiments/:id/metrics", get(get_experiment_metrics))
        .route("/v1/analytics/tasks", get(get_recent_tasks))
        .route("/v1/analytics/tasks/:id/traffic", get(get_task_traffic))
        .route("/v1/analytics/sessions", get(get_sessions))
        .route("/v1/analytics/limits", get(get_rate_limits))
        .route("/v1/analytics/events", get(get_events))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    println!("Proxy server listening on http://{}", config.bind_addr);
    // Where state actually lives is not guessable from outside when the
    // server is embedded — the desktop app puts it in a per-OS app-data
    // directory — so print it rather than making people hunt for the database.
    println!("State directory: {}", config.data_dir.display());
    axum::serve(listener, app).await?;

    Ok(())
}

async fn get_experiments(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let experiments = state.db.get_all_experiments().await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let res = experiments.into_iter().map(|(id, name, desc)| {
        json!({
            "id": id,
            "name": name,
            "description": desc
        })
    }).collect::<Vec<Value>>();

    Ok(axum::Json(res))
}

async fn get_experiment_metrics(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    let metrics = state.db.get_experiment_metrics(id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(metrics))
}

async fn get_recent_tasks(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let tasks = state.db.get_recent_tasks(200).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(tasks))
}

async fn get_task_traffic(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    let traffic = state.db.get_task_traffic(id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match traffic {
        Some(t) => Ok(axum::Json(t)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn get_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let sessions = state.db.get_sessions(100).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(sessions))
}

async fn get_rate_limits(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let limits = state.db.get_latest_rate_limits().await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(limits))
}

/// Live change feed, so the dashboard reflects a call starting or finishing
/// the moment it happens instead of on the next poll — the difference
/// between a report and a monitor when the question is "is it stuck?".
///
/// A lagged subscriber (dashboard tabbed away, then back) is not an error
/// worth ending the stream over: it re-reads full state on the next ping, so
/// the missed pings cost nothing and the stream is kept alive.
async fn get_events(State(state): State<Arc<AppState>>) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let stream = tokio_stream::wrappers::BroadcastStream::new(state.events.subscribe())
        .filter_map(|msg| async move { msg.ok() })
        .map(|msg| Ok(Event::default().data(msg)));

    // Without a keep-alive an idle feed looks indistinguishable from a dead
    // one to any proxy or browser in between.
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn openai_proxy_handler(
    state: State<Arc<AppState>>,
    headers: HeaderMap,
    method: Method,
    req: Request,
) -> Result<Response, StatusCode> {
    proxy_handler(Provider::OpenAI, state.0, headers, method, req).await
}

async fn anthropic_proxy_handler(
    state: State<Arc<AppState>>,
    headers: HeaderMap,
    method: Method,
    req: Request,
) -> Result<Response, StatusCode> {
    proxy_handler(Provider::Anthropic, state.0, headers, method, req).await
}

async fn proxy_handler(
    provider: Provider,
    state: Arc<AppState>,
    headers: HeaderMap,
    method: Method,
    req: Request,
) -> Result<Response, StatusCode> {
    let start = Instant::now();

    let agent_name = headers.get("X-Agent-ID")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown_agent");

    let session_id = headers.get("X-Session-ID")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("default_session");

    let agent_id = state.db.get_or_create_agent(agent_name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let experiment_id = match headers.get("X-Experiment-ID").and_then(|v| v.to_str().ok()) {
        Some(name) => Some(state.db.get_or_create_experiment(name, None).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?),
        None => None,
    };

    let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX).await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let parsed_body: Option<Value> = serde_json::from_slice(&body_bytes).ok();
    let is_streaming = parsed_body.as_ref().and_then(|j| j["stream"].as_bool()).unwrap_or(false);
    let model_name = parsed_body.as_ref().and_then(|j| j["model"].as_str()).map(String::from);
    let task_preview = parsed_body.as_ref().and_then(extract_task_preview);

    let task_id = state.db.create_task(
        agent_id,
        experiment_id,
        task_preview,
        Some(session_id.to_string()),
        model_name.clone(),
        Some(provider.as_str().to_string()),
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let call = CallContext {
        task_id,
        agent_name: agent_name.to_string(),
        session_id: session_id.to_string(),
        started: start,
    };

    let request_text = String::from_utf8_lossy(&body_bytes).to_string();
    let _ = state.db.save_traffic_request(task_id, &request_text).await;
    call.notify(&state, "task_started", session_state::STATUS_IN_FLIGHT);

    let forward_body: Vec<u8> = match (&parsed_body, provider, is_streaming) {
        (Some(json), Provider::OpenAI, true) => {
            // OpenAI only includes `usage` on the final streamed chunk when
            // asked for it; without this, streamed token counts are 0.
            let mut j = json.clone();
            j["stream_options"] = json!({"include_usage": true});
            serde_json::to_vec(&j).unwrap_or_else(|_| body_bytes.to_vec())
        }
        _ => body_bytes.to_vec(),
    };

    let mut proxy_req = state.client.request(method, provider.target_url());
    for header_name in ["authorization", "x-api-key", "anthropic-version", "anthropic-beta"] {
        if let Some(value) = headers.get(header_name) {
            proxy_req = proxy_req.header(header_name, value.clone());
        }
    }
    let proxy_req = proxy_req
        .header("Content-Type", "application/json")
        .body(forward_body);

    let response = match proxy_req.send().await {
        Ok(response) => response,
        Err(e) => {
            // The provider was never reached (DNS, TLS, connection refused,
            // timeout). Close the task out rather than returning early and
            // leaving it in-flight forever — an unreachable provider is
            // exactly the kind of stuck the dashboard exists to surface.
            eprintln!("Proxy error: {}", e);
            let _ = state.db.finish_task(task_id, &TaskOutcome {
                status: session_state::STATUS_ERROR.to_string(),
                error_type: Some("upstream_unreachable".to_string()),
                error_message: Some(e.to_string().chars().take(500).collect()),
                ttfb_ms: Some(start.elapsed().as_millis() as i64),
                duration_ms: Some(start.elapsed().as_millis() as i64),
                ..Default::default()
            }).await;
            call.notify(&state, "task_finished", session_state::STATUS_ERROR);
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let latency = start.elapsed().as_millis() as i64;

    if is_streaming {
        handle_streaming(state, provider, response, task_id, model_name, latency, status, call).await
    } else {
        Ok(handle_unary(state, provider, response, task_id, model_name, latency, status, call).await)
    }
}

/// Best-effort "what is this call actually asking for" summary, taken from
/// the most recent user turn — both OpenAI and Anthropic request bodies use
/// a `messages: [{role, content}]` shape, and `content` is either a plain
/// string or a list of content blocks (only the text ones are used here).
fn extract_task_preview(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    let last_user = messages.iter().rev().find(|m| m["role"] == "user")?;
    let content = &last_user["content"];

    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else {
        content.as_array()?
            .iter()
            .find_map(|block| block.get("text").and_then(|t| t.as_str()))?
            .to_string()
    };

    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(200).collect())
}

fn extract_usage(provider: Provider, json: &Value) -> UsageInfo {
    match provider {
        Provider::OpenAI => {
            let usage = &json["usage"];
            let total_prompt = usage["prompt_tokens"].as_i64().unwrap_or(0);
            let cache_read = usage["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
            UsageInfo {
                prompt_tokens: (total_prompt - cache_read).max(0),
                completion_tokens: usage["completion_tokens"].as_i64().unwrap_or(0),
                cache_creation_tokens: 0,
                cache_read_tokens: cache_read,
                tool_calls_count: json["choices"][0]["message"]["tool_calls"].as_array().map(|a| a.len() as i64).unwrap_or(0),
            }
        }
        Provider::Anthropic => {
            let usage = &json["usage"];
            UsageInfo {
                prompt_tokens: usage["input_tokens"].as_i64().unwrap_or(0),
                completion_tokens: usage["output_tokens"].as_i64().unwrap_or(0),
                cache_creation_tokens: usage["cache_creation_input_tokens"].as_i64().unwrap_or(0),
                cache_read_tokens: usage["cache_read_input_tokens"].as_i64().unwrap_or(0),
                tool_calls_count: json["content"].as_array()
                    .map(|blocks| blocks.iter().filter(|b| b["type"] == "tool_use").count() as i64)
                    .unwrap_or(0),
            }
        }
    }
}

/// Maps an upstream HTTP status onto the call outcome the dashboard keys
/// off. The distinction that matters most is 429 vs everything else: a rate
/// limit is a wait, an auth or request error is a thing the human must go
/// fix, and an overload is the provider's problem that the agent will
/// usually retry through on its own.
fn classify_http_status(status: u16) -> &'static str {
    match status {
        200..=299 => session_state::STATUS_OK,
        429 => session_state::STATUS_RATE_LIMITED,
        503 | 529 => session_state::STATUS_OVERLOADED,
        _ => session_state::STATUS_ERROR,
    }
}

/// Pulls `type` and `message` out of the error envelope both providers use
/// (`{"error": {"type": ..., "message": ...}}`), falling back to OpenAI's
/// `code` when it sends that instead of a type.
fn extract_error_details(json: &Value) -> (Option<String>, Option<String>) {
    let error = &json["error"];
    let error_type = error["type"].as_str().or_else(|| error["code"].as_str()).map(String::from);
    let message = error["message"].as_str().map(|m| m.chars().take(500).collect());
    (error_type, message)
}

/// Why the model stopped — the single most informative field for telling
/// "the agent went off to run a tool" apart from "the agent is done and
/// waiting for you". OpenAI calls it `finish_reason`, Anthropic
/// `stop_reason`, with different vocabularies for the same three outcomes.
fn extract_stop_reason(provider: Provider, json: &Value) -> Option<String> {
    match provider {
        Provider::OpenAI => json["choices"][0]["finish_reason"].as_str().map(String::from),
        Provider::Anthropic => json["stop_reason"].as_str().map(String::from),
    }
}

/// Whether the turn came back to the human. An explicit question tool says
/// so outright; otherwise a turn that ended without asking for a tool means
/// the agent produced its answer and is now sitting at a prompt.
fn turn_awaits_human(stop_reason: Option<&str>, has_question: bool) -> bool {
    has_question || matches!(stop_reason, Some("end_turn") | Some("stop"))
}

/// Scans the same tool-calls/content a response already carries for one
/// matching a known "ask the human a question" convention. Unary twin of
/// the streaming accumulate-then-scan path in handle_streaming.
fn extract_agent_question(provider: Provider, json: &Value) -> Option<(String, String)> {
    match provider {
        Provider::OpenAI => {
            let tool_calls = json["choices"][0]["message"]["tool_calls"].as_array()?;
            tool_calls.iter().find_map(|tc| {
                let name = tc["function"]["name"].as_str().unwrap_or("");
                if !agent_question::is_agent_question_tool(name) {
                    return None;
                }
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments: Value = serde_json::from_str(args_str).ok()?;
                agent_question::extract_question_text(&arguments).map(|text| (name.to_string(), text))
            })
        }
        Provider::Anthropic => {
            let content = json["content"].as_array()?;
            content.iter().find_map(|block| {
                if block["type"] != "tool_use" {
                    return None;
                }
                let name = block["name"].as_str().unwrap_or("");
                if !agent_question::is_agent_question_tool(name) {
                    return None;
                }
                agent_question::extract_question_text(&block["input"]).map(|text| (name.to_string(), text))
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_unary(
    state: Arc<AppState>,
    provider: Provider,
    res: reqwest::Response,
    task_id: i64,
    model_name: Option<String>,
    latency: i64,
    status: StatusCode,
    call: CallContext,
) -> Response {
    let headers = res.headers().clone();
    let res_bytes = res.bytes().await.unwrap_or_default();
    let duration_ms = call.started.elapsed().as_millis() as i64;
    let parsed_response: Option<Value> = serde_json::from_slice(&res_bytes).ok();

    let usage = parsed_response.as_ref()
        .map(|json| extract_usage(provider, json))
        .unwrap_or_default();
    let agent_question = parsed_response.as_ref()
        .and_then(|json| extract_agent_question(provider, json));
    let stop_reason = parsed_response.as_ref().and_then(|json| extract_stop_reason(provider, json));
    let (error_type, error_message) = parsed_response.as_ref()
        .map(extract_error_details)
        .unwrap_or((None, None));

    let response_text = String::from_utf8_lossy(&res_bytes).to_string();
    let _ = state.db.save_traffic_response(task_id, &response_text).await;
    if let Some((tool, text)) = &agent_question {
        let _ = state.db.save_agent_question(task_id, tool, text).await;
    }

    let call_status = classify_http_status(status.as_u16());
    let outcome = TaskOutcome {
        status: call_status.to_string(),
        http_status: Some(status.as_u16() as i64),
        error_type,
        error_message,
        awaiting_input: call_status == session_state::STATUS_OK
            && turn_awaits_human(stop_reason.as_deref(), agent_question.is_some()),
        stop_reason,
        ttfb_ms: Some(latency),
        duration_ms: Some(duration_ms),
    };
    let _ = state.db.finish_task(task_id, &outcome).await;
    call.record_rate_limits(&state, provider, &headers).await;
    call.notify(&state, "task_finished", &outcome.status);

    let cost = model_name.as_deref().and_then(|m| state.pricing.estimate_cost(
        m,
        provider.as_str(),
        usage.prompt_tokens,
        usage.cache_creation_tokens,
        usage.cache_read_tokens,
        usage.completion_tokens,
    ));

    let _ = state.db.log_metric(
        task_id,
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.cache_creation_tokens,
        usage.cache_read_tokens,
        usage.tool_calls_count,
        latency,
        cost,
    ).await;

    let mut response_builder = axum::response::Response::builder().status(status);
    for (key, value) in headers.iter() {
        response_builder = response_builder.header(key, value);
    }
    response_builder.body(axum::body::Bytes::from(res_bytes).into()).unwrap()
}

/// Everything a streamed response reveals, folded together as chunks arrive.
/// One struct rather than a handful of `&mut` parameters because the set kept
/// growing — usage, tool calls, and now why the turn ended and whether it
/// ended at all.
#[derive(Debug, Default)]
struct StreamAccumulator {
    usage: UsageInfo,
    tool_ids: HashSet<String>,
    tool_calls: Vec<(String, agent_question::ToolCallAccumulator)>,
    stop_reason: Option<String>,
    /// Whether the provider's own end-of-stream marker arrived. Its absence
    /// is the only evidence available that a stream was cut short — the
    /// agent was cancelled, or the connection dropped mid-answer — which
    /// otherwise looks identical to a clean finish with fewer tokens.
    saw_terminal: bool,
    error_type: Option<String>,
    error_message: Option<String>,
}

impl StreamAccumulator {
    /// Reconstructs the question the agent asked, if it asked one.
    fn agent_question(&self) -> Option<(String, String)> {
        self.tool_calls.iter().find_map(|(_, acc)| {
            let name = acc.name.as_deref()?;
            if !agent_question::is_agent_question_tool(name) {
                return None;
            }
            let arguments: Value = serde_json::from_str(&acc.arguments).ok()?;
            agent_question::extract_question_text(&arguments).map(|text| (name.to_string(), text))
        })
    }
}

/// Folds one SSE chunk's `data: ` lines into the running totals. Chunk
/// boundaries aren't guaranteed to land on line boundaries, so a `data: `
/// line split across two chunks is missed — acceptable for a best-effort
/// telemetry sidecar that isn't the billing source of truth.
fn accumulate_openai_stream_chunk(text: &str, acc: &mut StreamAccumulator) {
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        if data.trim() == "[DONE]" {
            acc.saw_terminal = true;
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else { continue };

        if !json["error"].is_null() {
            let (error_type, message) = extract_error_details(&json);
            acc.error_type = error_type;
            acc.error_message = message;
        }

        if let Some(usage_obj) = json.get("usage").filter(|u| !u.is_null()) {
            let total_prompt = usage_obj["prompt_tokens"].as_i64().unwrap_or(0);
            let cache_read = usage_obj["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
            acc.usage.prompt_tokens = (total_prompt - cache_read).max(0);
            acc.usage.cache_read_tokens = cache_read;
            acc.usage.completion_tokens = usage_obj["completion_tokens"].as_i64().unwrap_or(acc.usage.completion_tokens);
        }

        // Sent once, on the final content chunk of the turn.
        if let Some(reason) = json["choices"][0]["finish_reason"].as_str() {
            acc.stop_reason = Some(reason.to_string());
        }

        if let Some(tool_calls) = json["choices"][0]["delta"]["tool_calls"].as_array() {
            for tc in tool_calls {
                // Key by index, not id: OpenAI only sends `id` on a tool
                // call's first delta fragment, while `index` stays stable
                // across every fragment of that same call.
                let key = tc.get("index").map(|v| v.to_string())
                    .or_else(|| tc.get("id").and_then(|v| v.as_str()).map(String::from));
                if let Some(key) = key {
                    acc.tool_ids.insert(key.clone());

                    let entry = agent_question::tool_call_entry(&mut acc.tool_calls, &key);
                    if let Some(name) = tc["function"]["name"].as_str() {
                        entry.name = Some(name.to_string());
                    }
                    if let Some(args_fragment) = tc["function"]["arguments"].as_str() {
                        entry.arguments.push_str(args_fragment);
                    }
                }
            }
        }
    }
}

fn accumulate_anthropic_stream_chunk(text: &str, acc: &mut StreamAccumulator) {
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(json) = serde_json::from_str::<Value>(data) else { continue };

        match json["type"].as_str() {
            Some("message_start") => {
                let usage_obj = &json["message"]["usage"];
                acc.usage.prompt_tokens = usage_obj["input_tokens"].as_i64().unwrap_or(0);
                acc.usage.cache_creation_tokens = usage_obj["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                acc.usage.cache_read_tokens = usage_obj["cache_read_input_tokens"].as_i64().unwrap_or(0);
            }
            Some("content_block_start") => {
                if json["content_block"]["type"].as_str() == Some("tool_use") {
                    if let Some(id) = json["content_block"]["id"].as_str() {
                        acc.tool_ids.insert(id.to_string());
                    }
                    // Keyed by the block's stream index (not its id) so it
                    // lines up with content_block_delta below, which only
                    // carries the index, not the id, on each fragment.
                    if let Some(index) = json["index"].as_i64() {
                        let entry = agent_question::tool_call_entry(&mut acc.tool_calls, &index.to_string());
                        if let Some(name) = json["content_block"]["name"].as_str() {
                            entry.name = Some(name.to_string());
                        }
                    }
                }
            }
            Some("content_block_delta") => {
                if json["delta"]["type"].as_str() == Some("input_json_delta") {
                    if let (Some(index), Some(fragment)) = (json["index"].as_i64(), json["delta"]["partial_json"].as_str()) {
                        agent_question::tool_call_entry(&mut acc.tool_calls, &index.to_string()).arguments.push_str(fragment);
                    }
                }
            }
            Some("message_delta") => {
                if let Some(out) = json["usage"]["output_tokens"].as_i64() {
                    acc.usage.completion_tokens = out;
                }
                if let Some(reason) = json["delta"]["stop_reason"].as_str() {
                    acc.stop_reason = Some(reason.to_string());
                }
            }
            Some("message_stop") => acc.saw_terminal = true,
            // Anthropic can fail mid-stream (an overload after the headers
            // already said 200), which is only visible as an error event.
            Some("error") => {
                let (error_type, message) = extract_error_details(&json);
                acc.error_type = error_type;
                acc.error_message = message;
            }
            _ => {}
        }
    }
}

/// Decides how a streamed call ended.
///
/// A stream is harder to classify than a unary call because the HTTP status
/// is decided before any content exists: a 200 only means the request was
/// accepted, not that an answer arrived. So three things can each override
/// it — a non-2xx status (the body is a plain JSON error, not SSE), an error
/// event mid-stream, and a stream that simply stopped without its terminal
/// marker.
fn classify_stream_outcome(
    acc: &StreamAccumulator,
    raw_text: &str,
    status: StatusCode,
    has_question: bool,
    ttfb_ms: i64,
    duration_ms: i64,
) -> TaskOutcome {
    let http_class = classify_http_status(status.as_u16());

    let (status_str, error_type, error_message) = if http_class != session_state::STATUS_OK {
        // Rejected outright: the body is a single JSON error document.
        let parsed: Option<Value> = serde_json::from_str(raw_text).ok();
        let (error_type, message) = parsed.as_ref().map(extract_error_details).unwrap_or((None, None));
        (http_class, error_type, message)
    } else if acc.error_type.is_some() || acc.error_message.is_some() {
        // Accepted, then failed part-way through. An overload event here is
        // still an overload, not a generic error, so it keeps its own state.
        let is_overload = acc.error_type.as_deref() == Some("overloaded_error");
        let class = if is_overload { session_state::STATUS_OVERLOADED } else { session_state::STATUS_ERROR };
        (class, acc.error_type.clone(), acc.error_message.clone())
    } else if !acc.saw_terminal {
        (session_state::STATUS_INTERRUPTED, None, None)
    } else {
        (session_state::STATUS_OK, None, None)
    };

    TaskOutcome {
        status: status_str.to_string(),
        http_status: Some(status.as_u16() as i64),
        error_type,
        error_message,
        // Only a cleanly finished turn can be waiting on the human: a cut-off
        // stream isn't waiting for an answer, it needs to be retried.
        awaiting_input: status_str == session_state::STATUS_OK
            && turn_awaits_human(acc.stop_reason.as_deref(), has_question),
        stop_reason: acc.stop_reason.clone(),
        ttfb_ms: Some(ttfb_ms),
        duration_ms: Some(duration_ms),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_streaming(
    state: Arc<AppState>,
    provider: Provider,
    res: reqwest::Response,
    task_id: i64,
    model_name: Option<String>,
    latency: i64,
    status: StatusCode,
    call: CallContext,
) -> Result<Response, StatusCode> {
    let headers = res.headers().clone();
    call.record_rate_limits(&state, provider, &headers).await;

    // Unbounded and non-blocking so a slow telemetry consumer can never add
    // latency to (or drop chunks from) the response the caller actually sees.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Forward each chunk's raw bytes unchanged: both providers already frame
    // a streaming body as SSE `event:`/`data:` lines, so re-encoding a chunk
    // through `Event::data` would prefix every one of those lines with
    // `data:` again, producing nested, undecodable frames such as
    // `data: event: message_start` and `data: data: {...}`.
    let stream = res.bytes_stream().map(move |chunk_result| {
        match chunk_result {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                let _ = tx.send(text);
                Ok(bytes)
            }
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        }
    });

    tokio::spawn(async move {
        let mut acc = StreamAccumulator::default();
        let mut raw_text = String::new();

        while let Some(chunk_text) = rx.recv().await {
            raw_text.push_str(&chunk_text);
            match provider {
                Provider::OpenAI => accumulate_openai_stream_chunk(&chunk_text, &mut acc),
                Provider::Anthropic => accumulate_anthropic_stream_chunk(&chunk_text, &mut acc),
            }
        }
        // The loop above ends when the last chunk has been forwarded, which
        // is the only moment the true wall-clock cost of a streamed call is
        // known — `latency` was measured back when the headers arrived.
        let duration_ms = call.started.elapsed().as_millis() as i64;
        acc.usage.tool_calls_count = acc.tool_ids.len() as i64;

        let agent_question = acc.agent_question();
        let outcome = classify_stream_outcome(&acc, &raw_text, status, agent_question.is_some(), latency, duration_ms);

        let cost = model_name.as_deref().and_then(|m| state.pricing.estimate_cost(
            m,
            provider.as_str(),
            acc.usage.prompt_tokens,
            acc.usage.cache_creation_tokens,
            acc.usage.cache_read_tokens,
            acc.usage.completion_tokens,
        ));

        let _ = state.db.log_metric(
            task_id,
            acc.usage.prompt_tokens,
            acc.usage.completion_tokens,
            acc.usage.cache_creation_tokens,
            acc.usage.cache_read_tokens,
            acc.usage.tool_calls_count,
            latency,
            cost,
        ).await;
        let _ = state.db.save_traffic_response(task_id, &raw_text).await;
        if let Some((tool, text)) = &agent_question {
            let _ = state.db.save_agent_question(task_id, tool, text).await;
        }
        let _ = state.db.finish_task(task_id, &outcome).await;
        call.notify(&state, "task_finished", &outcome.status);
    });

    // Preserve the upstream status/headers rather than always answering 200:
    // a rejected streaming request (bad key, invalid model, rate limit) gets
    // its provider status and error body forwarded as-is instead of looking
    // like a successful stream to the client SDK.
    let mut response_builder = Response::builder().status(status);
    for (key, value) in headers.iter() {
        response_builder = response_builder.header(key, value);
    }
    response_builder
        .body(Body::from_stream(stream))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_preview_prefers_last_user_message_string_content() {
        let body: Value = serde_json::from_str(r#"{
            "messages": [
                {"role": "user", "content": "first ask"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "  refactor   the auth\nmodule please "}
            ]
        }"#).unwrap();

        assert_eq!(extract_task_preview(&body).as_deref(), Some("refactor the auth module please"));
    }

    #[test]
    fn task_preview_handles_content_block_arrays() {
        let body: Value = serde_json::from_str(r#"{
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "look at this file"},
                    {"type": "image", "source": {}}
                ]}
            ]
        }"#).unwrap();

        assert_eq!(extract_task_preview(&body).as_deref(), Some("look at this file"));
    }

    #[test]
    fn task_preview_none_without_messages() {
        let body: Value = serde_json::from_str(r#"{"model": "gpt-4o"}"#).unwrap();
        assert_eq!(extract_task_preview(&body), None);
    }

    #[test]
    fn openai_unary_usage_splits_out_cached_tokens() {
        let json: Value = serde_json::from_str(r#"{
            "choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "foo", "arguments": "{}"}},
                {"id": "call_2", "type": "function", "function": {"name": "bar", "arguments": "{}"}}
            ]}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": {"cached_tokens": 30}}
        }"#).unwrap();

        let usage = extract_usage(Provider::OpenAI, &json);
        assert_eq!(usage.prompt_tokens, 70);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.tool_calls_count, 2);
    }

    #[test]
    fn anthropic_unary_usage_counts_only_tool_use_blocks() {
        let json: Value = serde_json::from_str(r#"{
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "toolu_1", "name": "foo", "input": {}}
            ],
            "usage": {"input_tokens": 50, "output_tokens": 15, "cache_creation_input_tokens": 200, "cache_read_input_tokens": 400}
        }"#).unwrap();

        let usage = extract_usage(Provider::Anthropic, &json);
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.cache_creation_tokens, 200);
        assert_eq!(usage.cache_read_tokens, 400);
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.tool_calls_count, 1);
    }

    #[test]
    fn openai_stream_dedupes_tool_call_fragments_by_index() {
        let mut acc = StreamAccumulator::default();

        // First fragment carries `id`; the continuation fragment for the
        // same call only carries `index` — both must resolve to one call.
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"foo\"}}]}}]}\n\n",
            &mut acc,
        );
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
            &mut acc,
        );
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_2\",\"function\":{\"name\":\"bar\"}}]}}]}\n\n",
            &mut acc,
        );
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":25,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\ndata: [DONE]\n\n",
            &mut acc,
        );

        assert_eq!(acc.tool_ids.len(), 2);
        assert_eq!(acc.usage.prompt_tokens, 80);
        assert_eq!(acc.usage.cache_read_tokens, 40);
        assert_eq!(acc.usage.completion_tokens, 25);
    }

    #[test]
    fn anthropic_stream_accumulates_across_events() {
        let mut acc = StreamAccumulator::default();

        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":50,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5}}}\n\n",
            &mut acc,
        );
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\"}}\n\n",
            &mut acc,
        );
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n",
            &mut acc,
        );

        assert_eq!(acc.usage.prompt_tokens, 50);
        assert_eq!(acc.usage.cache_creation_tokens, 10);
        assert_eq!(acc.usage.cache_read_tokens, 5);
        assert_eq!(acc.usage.completion_tokens, 42);
        assert_eq!(acc.tool_ids.len(), 1);
    }

    #[test]
    fn openai_unary_agent_question_matches_second_tool_call() {
        let json: Value = serde_json::from_str(r#"{
            "choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}},
                {"id": "call_2", "type": "function", "function": {"name": "ask_followup_question", "arguments": "{\"question\":\"Which file?\"}"}}
            ]}}]
        }"#).unwrap();

        let result = extract_agent_question(Provider::OpenAI, &json);
        assert_eq!(result, Some(("ask_followup_question".to_string(), "Which file?".to_string())));
    }

    #[test]
    fn openai_unary_no_question_tool_yields_none() {
        let json: Value = serde_json::from_str(r#"{
            "choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}
            ]}}]
        }"#).unwrap();

        assert_eq!(extract_agent_question(Provider::OpenAI, &json), None);
    }

    #[test]
    fn anthropic_unary_agent_question_from_ask_user_question() {
        let json: Value = serde_json::from_str(r#"{
            "content": [
                {"type": "text", "text": "Let me check."},
                {"type": "tool_use", "id": "toolu_1", "name": "AskUserQuestion", "input": {
                    "questions": [{"question": "Use TypeScript?", "header": "Language"}]
                }}
            ]
        }"#).unwrap();

        let result = extract_agent_question(Provider::Anthropic, &json);
        assert_eq!(result, Some(("AskUserQuestion".to_string(), "Use TypeScript?".to_string())));
    }

    #[test]
    fn openai_stream_reconstructs_fragmented_question_arguments() {
        let mut acc = StreamAccumulator::default();

        // Built via json!() rather than hand-escaped string literals — the
        // "arguments" field is itself a fragment of a *different* JSON
        // object arriving as plain text, so a literal would need three
        // layers of escaping (Rust string, this chunk's JSON, the nested
        // arguments text) at once, which is exactly what to avoid by hand.
        let chunks = [
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "ask_followup_question", "arguments": ""}}
            ]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"question\": \"Add "}}
            ]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "tests?\"}"}}
            ]}}]}),
        ];

        for chunk in &chunks {
            let line = format!("data: {}\n\n", chunk);
            accumulate_openai_stream_chunk(&line, &mut acc);
        }

        assert_eq!(acc.tool_calls.len(), 1);
        let (_, entry) = &acc.tool_calls[0];
        assert_eq!(entry.name.as_deref(), Some("ask_followup_question"));
        let arguments: Value = serde_json::from_str(&entry.arguments).unwrap();
        assert_eq!(agent_question::extract_question_text(&arguments).as_deref(), Some("Add tests?"));
    }

    #[test]
    fn anthropic_stream_reconstructs_fragmented_question_input() {
        let mut acc = StreamAccumulator::default();

        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"ask_followup_question\"}}\n\n",
            &mut acc,
        );
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"question\\\": \\\"Use \"}}\n\n",
            &mut acc,
        );
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"TypeScript?\\\"}\"}}\n\n",
            &mut acc,
        );

        assert_eq!(acc.tool_calls.len(), 1);
        let (_, entry) = &acc.tool_calls[0];
        assert_eq!(entry.name.as_deref(), Some("ask_followup_question"));
        let arguments: Value = serde_json::from_str(&entry.arguments).unwrap();
        assert_eq!(agent_question::extract_question_text(&arguments).as_deref(), Some("Use TypeScript?"));
    }

    #[test]
    fn http_statuses_map_onto_the_outcomes_the_dashboard_distinguishes() {
        assert_eq!(classify_http_status(200), session_state::STATUS_OK);
        assert_eq!(classify_http_status(429), session_state::STATUS_RATE_LIMITED);
        assert_eq!(classify_http_status(529), session_state::STATUS_OVERLOADED);
        assert_eq!(classify_http_status(503), session_state::STATUS_OVERLOADED);
        assert_eq!(classify_http_status(401), session_state::STATUS_ERROR);
        assert_eq!(classify_http_status(400), session_state::STATUS_ERROR);
        assert_eq!(classify_http_status(500), session_state::STATUS_ERROR);
    }

    #[test]
    fn error_envelope_is_read_from_either_providers_shape() {
        let anthropic: Value = serde_json::from_str(
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"quota exhausted"}}"#
        ).unwrap();
        assert_eq!(
            extract_error_details(&anthropic),
            (Some("rate_limit_error".to_string()), Some("quota exhausted".to_string()))
        );

        // OpenAI sometimes sends `code` where Anthropic sends `type`.
        let openai: Value = serde_json::from_str(
            r#"{"error":{"code":"insufficient_quota","message":"You exceeded your quota"}}"#
        ).unwrap();
        assert_eq!(
            extract_error_details(&openai),
            (Some("insufficient_quota".to_string()), Some("You exceeded your quota".to_string()))
        );
    }

    #[test]
    fn a_tool_using_turn_is_not_waiting_on_the_human() {
        assert!(!turn_awaits_human(Some("tool_use"), false));
        assert!(!turn_awaits_human(Some("tool_calls"), false));
        assert!(turn_awaits_human(Some("end_turn"), false));
        assert!(turn_awaits_human(Some("stop"), false));
        // ...unless the tool it used was a question for the human.
        assert!(turn_awaits_human(Some("tool_use"), true));
    }

    #[test]
    fn stop_reason_is_read_from_each_providers_field() {
        let openai: Value = serde_json::from_str(r#"{"choices":[{"finish_reason":"tool_calls"}]}"#).unwrap();
        assert_eq!(extract_stop_reason(Provider::OpenAI, &openai).as_deref(), Some("tool_calls"));

        let anthropic: Value = serde_json::from_str(r#"{"stop_reason":"end_turn"}"#).unwrap();
        assert_eq!(extract_stop_reason(Provider::Anthropic, &anthropic).as_deref(), Some("end_turn"));
    }

    #[test]
    fn openai_stream_records_the_finish_reason() {
        let mut acc = StreamAccumulator::default();
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            &mut acc,
        );

        assert_eq!(acc.stop_reason.as_deref(), Some("stop"));
        assert!(acc.saw_terminal);
    }

    #[test]
    fn anthropic_stream_records_stop_reason_and_terminal_event() {
        let mut acc = StreamAccumulator::default();
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\n",
            &mut acc,
        );
        assert_eq!(acc.stop_reason.as_deref(), Some("tool_use"));
        assert!(!acc.saw_terminal, "the stream has not ended yet");

        accumulate_anthropic_stream_chunk("data: {\"type\":\"message_stop\"}\n\n", &mut acc);
        assert!(acc.saw_terminal);
    }

    #[test]
    fn an_error_event_mid_stream_is_captured_despite_a_200() {
        let mut acc = StreamAccumulator::default();
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
            &mut acc,
        );

        let outcome = classify_stream_outcome(&acc, "", StatusCode::OK, false, 10, 20);
        assert_eq!(outcome.status, session_state::STATUS_OVERLOADED);
        assert_eq!(outcome.error_message.as_deref(), Some("Overloaded"));
        assert!(!outcome.awaiting_input);
    }

    #[test]
    fn a_stream_that_stops_without_its_terminal_marker_is_interrupted() {
        let mut acc = StreamAccumulator::default();
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n",
            &mut acc,
        );

        let outcome = classify_stream_outcome(&acc, "", StatusCode::OK, false, 10, 900);
        assert_eq!(outcome.status, session_state::STATUS_INTERRUPTED);
        assert_eq!(outcome.duration_ms, Some(900));
    }

    #[test]
    fn a_completed_stream_that_ended_its_turn_is_waiting_on_the_human() {
        let mut acc = StreamAccumulator::default();
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
            &mut acc,
        );

        let outcome = classify_stream_outcome(&acc, "", StatusCode::OK, false, 10, 20);
        assert_eq!(outcome.status, session_state::STATUS_OK);
        assert!(outcome.awaiting_input);
    }

    /// A 429 on a streaming request never produces SSE at all — the body is
    /// one JSON error document, and the HTTP status must win over the
    /// "no terminal marker seen" rule.
    #[test]
    fn a_rejected_streaming_request_is_classified_from_its_status_not_its_body() {
        let acc = StreamAccumulator::default();
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;

        let outcome = classify_stream_outcome(&acc, body, StatusCode::TOO_MANY_REQUESTS, false, 5, 5);
        assert_eq!(outcome.status, session_state::STATUS_RATE_LIMITED);
        assert_eq!(outcome.error_type.as_deref(), Some("rate_limit_error"));
        assert_eq!(outcome.http_status, Some(429));
        assert!(!outcome.awaiting_input);
    }

    #[test]
    fn seed_default_file_does_not_overwrite_existing_content() {
        let dir = std::env::temp_dir().join(format!("harnesswurm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agents.yaml");

        std::fs::write(&path, "custom user content").unwrap();
        seed_default_file(&path, "default content").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "custom user content");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrate_legacy_db_renames_when_new_db_absent() {
        let dir = std::env::temp_dir().join(format!("harnesswurm-test-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_path = dir.join("agent_turn.db");
        let db_path = dir.join("harnesswurm.db");
        std::fs::write(&legacy_path, "legacy telemetry").unwrap();

        migrate_legacy_db(&dir, &db_path).unwrap();

        assert!(!legacy_path.exists());
        assert_eq!(std::fs::read_to_string(&db_path).unwrap(), "legacy telemetry");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrate_legacy_db_does_not_overwrite_an_existing_new_db() {
        let dir = std::env::temp_dir().join(format!("harnesswurm-test-migrate2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_path = dir.join("agent_turn.db");
        let db_path = dir.join("harnesswurm.db");
        std::fs::write(&legacy_path, "legacy telemetry").unwrap();
        std::fs::write(&db_path, "current telemetry").unwrap();

        migrate_legacy_db(&dir, &db_path).unwrap();

        assert!(legacy_path.exists());
        assert_eq!(std::fs::read_to_string(&db_path).unwrap(), "current telemetry");

        std::fs::remove_dir_all(&dir).ok();
    }
}
