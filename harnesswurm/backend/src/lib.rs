use axum::{
    body::Body,
    extract::{Path, Query, State, Request},
    http::{HeaderMap, Method, StatusCode},
    response::{sse::{Event, Sse}, IntoResponse, Response},
    routing::{get, put},
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
pub mod fingerprints;
pub mod pricing;
pub mod providers;
pub mod rate_limits;
pub mod session_state;

use db::TaskOutcome;
use providers::{ApiStyle, ProviderTable, WireApi};

/// Capacity of the live-event fan-out. A slow dashboard that falls this far
/// behind gets lagged out rather than holding memory: it only ever misses
/// change *pings*, and its next poll re-reads the true state anyway.
const EVENT_CHANNEL_CAPACITY: usize = 256;

const DEFAULT_AGENTS_YAML: &str = include_str!("../agents.yaml");
const DEFAULT_PRICING_YAML: &str = include_str!("../pricing.yaml");
const DEFAULT_PROVIDERS_YAML: &str = include_str!("../providers.yaml");
const DEFAULT_FINGERPRINTS_YAML: &str = include_str!("../fingerprints.yaml");

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct AgentsFile {
    agents: Vec<AgentConfig>,
}

/// One resolved upstream for a single call: which endpoint the request is
/// forwarded to, and under which name it is recorded. How the response is
/// *parsed* is a property of the endpoint (see `Endpoint::wire`), not of
/// the provider — the aux endpoints broke that coupling.
#[derive(Debug, Clone)]
struct Upstream {
    /// The name every call to this provider is recorded under (what the
    /// Traffic view shows, and what `provider:` in `pricing.yaml` matches).
    name: String,
    /// The complete URL this specific call is forwarded to.
    url: String,
}

impl Upstream {
    fn for_endpoint(config: &providers::ProviderConfig, endpoint: Endpoint) -> Self {
        Self {
            name: config.name.clone(),
            url: config.url_for(endpoint.upstream_path(config.api)),
        }
    }
}

/// One of the API endpoints the proxy fronts. The first three carry agent
/// turns and are recorded as tasks; the aux ones exist because real agents
/// call them as part of their loop (a connectivity probe against
/// `/v1/models`, a context pre-count against `count_tokens`) and deserve a
/// forward rather than a 404 that makes the agent think its provider is
/// down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    ChatCompletions,
    Messages,
    Responses,
    CountTokens,
    Models,
}

impl Endpoint {
    /// The wire format responses on this endpoint are parsed as. `None` for
    /// the pass-through endpoints, which are forwarded but not interpreted.
    fn wire(self) -> Option<WireApi> {
        match self {
            Endpoint::ChatCompletions => Some(WireApi::OpenAi),
            Endpoint::Messages => Some(WireApi::Anthropic),
            Endpoint::Responses => Some(WireApi::Responses),
            Endpoint::CountTokens | Endpoint::Models => None,
        }
    }

    /// The provider style that serves this endpoint. `None` when both do
    /// (`/v1/models` exists on both APIs), in which case a bare call picks
    /// by the caller's auth headers — see `models_style_hint`.
    fn required_style(self) -> Option<ApiStyle> {
        match self {
            Endpoint::ChatCompletions | Endpoint::Responses => Some(ApiStyle::OpenAI),
            Endpoint::Messages | Endpoint::CountTokens => Some(ApiStyle::Anthropic),
            Endpoint::Models => None,
        }
    }

    /// The path appended to the provider's base URL. Style-dependent for
    /// `/v1/models`, the one endpoint both APIs serve: an OpenAI-style base
    /// URL already ends in `/v1` (the client appends `/models`), an
    /// Anthropic-style base does not (the client appends `/v1/models`).
    fn upstream_path(self, style: ApiStyle) -> &'static str {
        match self {
            Endpoint::ChatCompletions => "/chat/completions",
            Endpoint::Messages => "/v1/messages",
            Endpoint::Responses => "/responses",
            Endpoint::CountTokens => "/v1/messages/count_tokens",
            Endpoint::Models => match style {
                ApiStyle::OpenAI => "/models",
                ApiStyle::Anthropic => "/v1/models",
            },
        }
    }

    /// Whether a call here is recorded as a task with telemetry. `/v1/models`
    /// and `count_tokens` answer questions *about* a call the agent is about
    /// to make, so they are forwarded but contribute no row of their own.
    fn is_recorded(self) -> bool {
        self.wire().is_some()
    }
}

/// The URL tails that identify an endpoint. Order matters: a longer tail
/// sharing a suffix with a shorter one (`/v1/messages/count_tokens` vs
/// `/v1/messages`) must come first, since matching is by `strip_suffix`.
const ENDPOINT_TAILS: [(&str, Endpoint); 5] = [
    ("/v1/messages/count_tokens", Endpoint::CountTokens),
    ("/v1/chat/completions", Endpoint::ChatCompletions),
    ("/v1/messages", Endpoint::Messages),
    ("/v1/models", Endpoint::Models),
    ("/v1/responses", Endpoint::Responses),
];

/// Agent/experiment/session attribution carried in the URL path — the one
/// channel every agent can be pointed through, because it needs nothing
/// beyond the base URL all of them let you configure. Built by
/// `build_run_prefix`, used by `parse_proxy_path`.
struct RunAttribution {
    agent: String,
    /// Present in the three-segment form `/r/<agent>/<experiment>/<session>`;
    /// the two-segment form leaves the experiment to headers (or to none).
    experiment: Option<String>,
    session: String,
}

/// Everything the request path says about a proxied call: which endpoint it
/// hits, which named provider it asked for, and the run attribution it
/// carries. Parsed by `parse_proxy_path` — one pure function, so every
/// prefix shape provably agrees with every other.
struct ProxyPath {
    run: Option<RunAttribution>,
    provider: Option<String>,
    endpoint: Endpoint,
}

/// Parses a proxy route into its parts. The endpoint is recognized by its
/// tail, so `/v1/messages/count_tokens` must be listed before `/v1/messages`
/// to not be swallowed as a messages call with a stray suffix. Everything
/// before the tail is prefix segments: nothing, `/p/<provider>`,
/// `/r/<agent>/<session>`, `/r/<agent>/<experiment>/<session>`, or a `/r/…`
/// form with `/p/<provider>` appended — the combinations the
/// `harnesswurm run` wrapper generates.
fn parse_proxy_path(path: &str) -> Result<ProxyPath, String> {
    let usage = "Run prefixes are /r/<agent>/<session> or /r/<agent>/<experiment>/<session>, \
                 provider prefixes are /p/<provider>, and both combine: \
                 /r/<agent>/<experiment>/<session>/p/<provider>/v1/…";
    let (prefix, endpoint) = ENDPOINT_TAILS
        .iter()
        .find_map(|(tail, endpoint)| path.strip_suffix(tail).map(|prefix| (prefix, *endpoint)))
        .ok_or_else(|| {
            format!(
                "Unknown proxy route '{path}'. Known endpoints: {} — each optionally under \
                 /r/<agent>[/<experiment>]/<session> and /p/<provider>.",
                ENDPOINT_TAILS.iter().map(|(tail, _)| *tail).collect::<Vec<_>>().join(", "),
            )
        })?;

    let mut segments: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();

    // A trailing `/p/<name>` names the provider, on its own or after a run
    // prefix.
    let mut provider = None;
    if segments.len() >= 2 && segments[segments.len() - 2] == "p" {
        let name = segments.pop().expect("length checked above");
        segments.pop();
        if name.is_empty() {
            return Err(format!("Empty provider name in '{path}'. {usage}"));
        }
        provider = Some(name.to_string());
    }

    let mut run = None;
    if segments.first() == Some(&"r") {
        segments.remove(0);
        let attribution = match segments.as_slice() {
            [agent, session] => RunAttribution {
                agent: agent.to_string(),
                experiment: None,
                session: session.to_string(),
            },
            [agent, experiment, session] => RunAttribution {
                agent: agent.to_string(),
                experiment: Some(experiment.to_string()),
                session: session.to_string(),
            },
            _ => return Err(format!("Unknown run prefix in '{path}'. {usage}")),
        };
        run = Some(attribution);
        segments.clear();
    }

    if !segments.is_empty() {
        return Err(format!("Unknown proxy route '{path}'. {usage}"));
    }

    Ok(ProxyPath { run, provider, endpoint })
}

/// The `/r/…` path prefix the `harnesswurm run` wrapper points an agent's
/// base URL at — one function shared by the wrapper and the tests, so what
/// the wrapper generates is by construction what the proxy parses.
/// Segments are restricted to URL-safe characters because they travel in a
/// base URL pasted into other tools' configs.
pub fn build_run_prefix(
    agent: &str,
    experiment: Option<&str>,
    session: &str,
    provider: Option<&str>,
) -> Result<String, String> {
    let mut segments = vec![agent.to_string()];
    if let Some(experiment) = experiment {
        segments.push(experiment.to_string());
    }
    segments.push(session.to_string());
    if let Some(provider) = provider {
        segments.extend(["p".to_string(), provider.to_string()]);
    }

    for segment in &segments {
        let safe = !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !safe {
            return Err(format!(
                "'{segment}' can only contain letters, digits, '-', '_' and '.' — it travels inside a base URL"
            ));
        }
    }

    Ok(format!("/r/{}", segments.join("/")))
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
    async fn record_rate_limits(&self, state: &AppState, provider: &str, headers: &HeaderMap) {
        let snapshot = rate_limits::extract(headers);
        if !snapshot.is_empty() {
            let _ = state.db.save_rate_limits(self.task_id, provider, &snapshot).await;
        }
    }
}

pub struct AppState {
    pub db: db::Database,
    pub client: reqwest::Client,
    pub agents: Vec<AgentConfig>,
    pub pricing: pricing::PricingTable,
    /// Where each provider's API lives, from `providers.yaml`. Read per
    /// call so a request can name its own upstream (path prefix or
    /// `X-Provider`) instead of everything going to one hardcoded host,
    /// and behind a lock so an edit in the app's Settings tab takes effect
    /// on the next call rather than the next restart.
    pub providers: std::sync::RwLock<ProviderTable>,
    /// Where that file lives, so a saved edit lands next to the rest of the
    /// state instead of in whatever the process's working directory is.
    pub providers_path: PathBuf,
    /// How calls without `X-Agent-ID` are attributed anyway: recognizable
    /// `User-Agent` headers and system prompts, from `fingerprints.yaml`.
    /// Loaded once at startup; hand edits to the file apply on the next
    /// start, like every non-provider config.
    pub fingerprints: fingerprints::FingerprintTable,
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

/// How long captured request/response bodies are kept, from
/// `HARNESSWURM_TRAFFIC_RETENTION_DAYS`. Taken as the raw string so the
/// parser stays a pure function the tests can exercise without touching
/// the process env, which parallel tests share.
fn parse_retention_days(value: Option<&str>) -> i64 {
    const DEFAULT_DAYS: i64 = 30;
    let Some(value) = value else { return DEFAULT_DAYS };
    match value.trim().parse::<i64>() {
        Ok(days) if days < 0 => {
            eprintln!("Ignoring negative HARNESSWURM_TRAFFIC_RETENTION_DAYS '{value}'");
            DEFAULT_DAYS
        }
        Ok(days) => days,
        Err(_) => {
            eprintln!(
                "Ignoring unparseable HARNESSWURM_TRAFFIC_RETENTION_DAYS '{value}' (expected a number of days)"
            );
            DEFAULT_DAYS
        }
    }
}

fn retention_days_from_env() -> i64 {
    parse_retention_days(std::env::var("HARNESSWURM_TRAFFIC_RETENTION_DAYS").ok().as_deref())
}

pub async fn run(config: ServerConfig) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;

    let agents_path = config.data_dir.join("agents.yaml");
    let pricing_path = config.data_dir.join("pricing.yaml");
    let providers_path = config.data_dir.join("providers.yaml");
    let fingerprints_path = config.data_dir.join("fingerprints.yaml");
    seed_default_file(&agents_path, DEFAULT_AGENTS_YAML)?;
    seed_default_file(&pricing_path, DEFAULT_PRICING_YAML)?;
    seed_default_file(&providers_path, DEFAULT_PROVIDERS_YAML)?;
    seed_default_file(&fingerprints_path, DEFAULT_FINGERPRINTS_YAML)?;

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
    let fingerprints = fingerprints::FingerprintTable::load(&fingerprints_path);

    let mut provider_table = ProviderTable::load(&providers_path);
    provider_table.apply_env_overrides();

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
        providers: std::sync::RwLock::new(provider_table),
        providers_path: providers_path.clone(),
        fingerprints,
        events,
    });

    // Captured bodies are the bulk of the database and the only part with
    // privacy weight, so they age out on their own; the counts and costs
    // derived from them stay. The loop rather than a one-shot keeps a
    // long-running desktop app from growing without bound, not just a
    // server that restarts daily.
    let retention_days = retention_days_from_env();
    if retention_days > 0 {
        let db = state.db.clone();
        tokio::spawn(async move {
            loop {
                match db.prune_traffic_bodies(retention_days).await {
                    Ok(0) => {}
                    Ok(n) => println!("Pruned {n} traffic bodie(s) older than {retention_days} days"),
                    Err(e) => eprintln!("Could not prune traffic bodies: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
            }
        });
    }

    // Permissive: this server only ever binds to loopback, and its callers
    // are the Vite dev server (http://localhost:5173) and the Tauri webview
    // (whose origin varies by OS/version) fetching analytics data — without
    // this, both send cross-origin requests the browser blocks outright, so
    // the Traffic tab and Analytics dashboard stay empty.
    let cors = tower_http::cors::CorsLayer::permissive();

    let state_for_log = state.clone();

    let app = Router::new()
        // Every proxy route is matched by the fallback dispatcher below:
        // the main endpoints, the aux endpoints, and the /p/<provider> and
        // /r/… prefixes share one handler that parses the path itself, so
        // a new prefix shape can't drift out of sync with the routing.
        .route("/v1/providers", get(get_providers).put(put_providers))
        .route("/v1/analytics/experiments", get(get_experiments))
        .route("/v1/analytics/experiments/:id/metrics", get(get_experiment_metrics))
        .route("/v1/analytics/experiments/:id/comparison", get(get_experiment_comparison))
        .route("/v1/analytics/experiments/:id/breakdown", get(get_experiment_breakdown))
        .route("/v1/analytics/sessions/verdict", put(put_session_verdict))
        .route("/v1/analytics/tasks", get(get_recent_tasks))
        .route("/v1/analytics/tasks/:id/traffic", get(get_task_traffic))
        .route("/v1/analytics/sessions", get(get_sessions))
        .route("/v1/analytics/limits", get(get_rate_limits))
        .route("/v1/analytics/events", get(get_events))
        .fallback(proxy_dispatch)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    println!("Proxy server listening on http://{}", config.bind_addr);
    // Where state actually lives is not guessable from outside when the
    // server is embedded — the desktop app puts it in a per-OS app-data
    // directory — so print it rather than making people hunt for the database.
    println!("State directory: {}", config.data_dir.display());
    // Forwarding to the wrong host is otherwise invisible until a call
    // fails, so say up front where each configured provider actually goes.
    for provider in state_for_log.providers.read().unwrap().all() {
        let default_marker = if provider.default { " (default)" } else { "" };
        let override_marker = provider.env_override
            .map(|var| format!(" (base URL from {var})"))
            .unwrap_or_default();
        println!(
            "Provider {} [{}] -> {}{}{}",
            provider.name,
            provider.api.as_str(),
            provider.target_url(),
            default_marker,
            override_marker
        );
    }
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

/// Per-run totals for one experiment — the side-by-side that answers "which
/// agent got there cheaper". `.../metrics` is the same calls as a time
/// series; this is the same calls folded down to one row per agent+session.
async fn get_experiment_comparison(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(query): Query<GroupingQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let runs = state.db.get_experiment_comparison(id, query.grouping()).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(runs))
}

/// `?group=session` (the default) or `?group=agent` — see `db::RunGrouping`.
/// Shared by the comparison and the breakdown so the two never disagree
/// about what a run is.
#[derive(serde::Deserialize, Default)]
struct GroupingQuery {
    group: Option<String>,
}

impl GroupingQuery {
    fn grouping(&self) -> db::RunGrouping {
        db::RunGrouping::parse(self.group.as_deref())
    }
}

/// Where each run's money went — across the arc of the task, and through
/// which tools. Both halves answer "why did this run cost that", which the
/// comparison's single total cannot. Served together because the UI shows
/// them side by side and one round trip is one loading state.
async fn get_experiment_breakdown(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(query): Query<GroupingQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let grouping = query.grouping();
    let phases = state.db.get_experiment_phases(id, grouping).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let tools = state.db.get_experiment_tool_usage(id, grouping).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(axum::Json(json!({ "phases": phases, "tools": tools })))
}

#[derive(serde::Deserialize)]
struct VerdictRequest {
    agent_name: String,
    session_id: Option<String>,
    /// Set instead of `session_id` to judge a *merged* run: the verdict lands
    /// on every session this agent has under the experiment. Sending both is
    /// rejected rather than silently picking one.
    experiment_id: Option<i64>,
    /// `"solved"`, `"failed"`, or null to clear a previous verdict.
    verdict: Option<String>,
    note: Option<String>,
}

/// Marks whether a run solved its task. Nothing in the proxied traffic can
/// answer that, so the comparison takes it from whoever read the diff.
async fn put_session_verdict(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<VerdictRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    if let Some(verdict) = request.verdict.as_deref() {
        if !db::is_valid_verdict(verdict) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    let updated = match request.experiment_id {
        Some(_) if request.session_id.is_some() => return Err(StatusCode::BAD_REQUEST),
        Some(experiment_id) => state.db.set_experiment_verdict(
            &request.agent_name,
            experiment_id,
            request.verdict.as_deref(),
            request.note.as_deref(),
        ).await,
        None => state.db.set_session_verdict(
            &request.agent_name,
            request.session_id.as_deref(),
            request.verdict.as_deref(),
            request.note.as_deref(),
        ).await,
    }.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(axum::Json(json!({ "ok": true })))
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

/// The configured providers, so the UI (and `curl`) can show what a call
/// would be forwarded to without reading the YAML off disk.
async fn get_providers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let list = provider_list(&state);
    axum::Json(json!({ "providers": list }))
}

fn provider_list(state: &AppState) -> Vec<Value> {
    state.providers.read().unwrap().all().iter().map(|p| json!({
        "name": p.name,
        "api": p.api.as_str(),
        "base_url": p.base_url,
        "target_url": p.target_url(),
        "default": p.default,
        // Non-null means an env var is supplying the base URL in effect, so
        // an editor can show that the file's value isn't the one being used
        // rather than silently disagreeing with the traffic.
        "env_override": p.env_override,
    })).collect()
}

/// Replaces the whole provider list: what the editor sends is the new file.
/// A rejected edit changes nothing — neither on disk nor in memory — so a
/// typo can't leave the proxy half-configured.
async fn put_providers(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, axum::Json<Value>)> {
    let bad_request = |message: String| (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({ "error": { "message": message } })),
    );

    let incoming = body.get("providers").cloned().unwrap_or(Value::Null);
    let mut providers: Vec<providers::ProviderConfig> = serde_json::from_value(incoming)
        .map_err(|e| bad_request(format!("Could not read the provider list: {e}")))?;

    for provider in &mut providers {
        provider.name = provider.name.trim().to_string();
        provider.base_url = provider.base_url.trim().to_string();
    }

    ProviderTable::validate(&providers).map_err(bad_request)?;

    // Write first: a table that only ever lived in memory would come back
    // as the old one on the next start, which is worse than not saving.
    let mut candidate = ProviderTable::from_list(providers);
    candidate.apply_env_overrides();
    candidate.save(&state.providers_path).map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({ "error": { "message": format!(
            "Could not write {}: {e}", state.providers_path.display()
        ) } })),
    ))?;

    *state.providers.write().unwrap() = candidate;
    Ok(axum::Json(json!({ "providers": provider_list(&state) })))
}

/// Which upstream this call goes to. A name given explicitly — in the path
/// (`/p/<name>/…`) or in `X-Provider` — wins over the endpoint's default,
/// and an unknown one is refused rather than quietly forwarded to the
/// hosted API under a name the caller didn't ask for.
fn resolve_upstream(
    providers: &ProviderTable,
    endpoint: Endpoint,
    named: Option<&str>,
    headers: &HeaderMap,
) -> Result<Upstream, (StatusCode, String)> {
    let requested = named
        .or_else(|| headers.get("X-Provider").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let style = endpoint
        .required_style()
        .unwrap_or_else(|| models_style_hint(headers));

    let Some(name) = requested else {
        return Ok(Upstream::for_endpoint(providers.default_for(style), endpoint));
    };

    let config = providers.by_name(name).ok_or_else(|| (
        StatusCode::NOT_FOUND,
        format!(
            "Unknown provider '{name}'. Configured providers: {}. Add it to providers.yaml.",
            providers.names().join(", ")
        ),
    ))?;

    // Forwarding an OpenAI-shaped body to an Anthropic endpoint (or the
    // reverse) can only fail upstream, with a confusing error; say which
    // route the provider actually belongs on instead. `/v1/models` is
    // served by both styles, so it skips the check.
    if let Some(required) = endpoint.required_style() {
        if config.api != required {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Provider '{}' speaks the {} API, but this call came in on an endpoint the {} API serves. \
                     Use {} instead.",
                    config.name,
                    config.api.as_str(),
                    required.as_str(),
                    match config.api {
                        ApiStyle::OpenAI => "/p/<provider>/v1/chat/completions",
                        ApiStyle::Anthropic => "/p/<provider>/v1/messages",
                    },
                ),
            ));
        }
    }

    Ok(Upstream::for_endpoint(config, endpoint))
}

/// Which API family a bare `/v1/models` call belongs to. The endpoint exists
/// on both, so the caller's own auth headers decide: `x-api-key` /
/// `anthropic-version` mean an Anthropic-style client, anything else — a
/// bearer token, or no auth at all from a local server — reads as
/// OpenAI-style, the more common shape.
fn models_style_hint(headers: &HeaderMap) -> ApiStyle {
    for header in ["x-api-key", "anthropic-version", "anthropic-beta"] {
        if headers.contains_key(header) {
            return ApiStyle::Anthropic;
        }
    }
    ApiStyle::OpenAI
}

/// Everything a call is recorded under. Three sources, most specific first:
/// the run prefix in the URL (what the `harnesswurm run` wrapper sets), the
/// `X-*-ID` headers, and — needing no cooperation from the agent at all —
/// fingerprints of what it unavoidably sends: its User-Agent, and its
/// system prompt.
struct Attribution {
    agent: String,
    session: String,
    experiment: Option<String>,
}

fn resolve_attribution(
    run: Option<&RunAttribution>,
    headers: &HeaderMap,
    body: Option<&Value>,
    wire: Option<WireApi>,
    fingerprints: &fingerprints::FingerprintTable,
) -> Attribution {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from)
    };

    let agent = run
        .map(|r| r.agent.clone())
        .or_else(|| header("X-Agent-ID"))
        .or_else(|| {
            let user_agent = headers.get("user-agent").and_then(|v| v.to_str().ok())?;
            fingerprints.match_user_agent(user_agent).map(String::from)
        })
        .or_else(|| {
            let prompt = fingerprints::system_prompt_text(wire?, body?)?;
            fingerprints.match_system_prompt(&prompt).map(String::from)
        })
        .unwrap_or_else(|| "unknown_agent".to_string());

    // With no labelled session, calls of the same task still belong
    // together: agents resend the whole conversation each turn, so the
    // first user message is a stable per-task key (see
    // `fingerprints::auto_session_id`).
    let session = run
        .map(|r| r.session.clone())
        .or_else(|| header("X-Session-ID"))
        .or_else(|| body.and_then(fingerprints::auto_session_id))
        .unwrap_or_else(|| "default_session".to_string());

    let experiment = run
        .and_then(|r| r.experiment.clone())
        .or_else(|| header("X-Experiment-ID"));

    Attribution { agent, session, experiment }
}

/// Catch-all for every proxy route. The main endpoints, the aux endpoints
/// and the `/p/<provider>` / `/r/…` prefixes all share one handler because
/// they share one job: the path (not the route registration) says where the
/// call goes and who it belongs to, and a path parser is easier to keep
/// honest than a dozen near-identical route handlers.
async fn proxy_dispatch(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> Result<Response, StatusCode> {
    let method = req.method().clone();
    let headers = req.headers().clone();
    let path = req.uri().path().to_string();
    proxy_handler(&path, state, headers, method, req).await
}

async fn proxy_handler(
    path_text: &str,
    state: Arc<AppState>,
    headers: HeaderMap,
    method: Method,
    req: Request,
) -> Result<Response, StatusCode> {
    let start = Instant::now();

    let path = match parse_proxy_path(path_text) {
        Ok(path) => path,
        Err(message) => {
            return Ok((
                StatusCode::NOT_FOUND,
                axum::Json(json!({ "error": { "message": message } })),
            ).into_response())
        }
    };

    // Resolved before anything is recorded: a call that can't be routed is
    // a configuration mistake to report back, not a task to log as failed.
    let resolved = {
        // Scoped so the lock is released before anything awaits on it.
        let providers = state.providers.read().unwrap();
        resolve_upstream(&providers, path.endpoint, path.provider.as_deref(), &headers)
    };
    let upstream = match resolved {
        Ok(upstream) => upstream,
        Err((status, message)) => {
            return Ok((status, axum::Json(json!({ "error": { "message": message } }))).into_response());
        }
    };

    let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX).await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let parsed_body: Option<Value> = serde_json::from_slice(&body_bytes).ok();

    let attribution = resolve_attribution(
        path.run.as_ref(),
        &headers,
        parsed_body.as_ref(),
        path.endpoint.wire(),
        &state.fingerprints,
    );

    if !path.endpoint.is_recorded() {
        return forward_pass_through(state, upstream, headers, method, body_bytes.to_vec()).await;
    }
    let wire = path.endpoint.wire().expect("recorded endpoints parse a wire format");

    let agent_id = state.db.get_or_create_agent(&attribution.agent).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let experiment_id = match attribution.experiment.as_deref() {
        Some(name) => Some(state.db.get_or_create_experiment(name, None).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?),
        None => None,
    };

    let is_streaming = parsed_body.as_ref().and_then(|j| j["stream"].as_bool()).unwrap_or(false);
    let model_name = parsed_body.as_ref().and_then(|j| j["model"].as_str()).map(String::from);
    let task_preview = parsed_body.as_ref().and_then(extract_task_preview);

    let task_id = state.db.create_task(
        agent_id,
        experiment_id,
        task_preview,
        Some(attribution.session.clone()),
        model_name.clone(),
        Some(upstream.name.clone()),
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let call = CallContext {
        task_id,
        agent_name: attribution.agent.clone(),
        session_id: attribution.session.clone(),
        started: start,
    };

    let request_text = String::from_utf8_lossy(&body_bytes).to_string();
    let _ = state.db.save_traffic_request(task_id, &request_text).await;
    call.notify(&state, "task_started", session_state::STATUS_IN_FLIGHT);

    let forward_body: Vec<u8> = match (&parsed_body, wire, is_streaming) {
        (Some(json), WireApi::OpenAi, true) => {
            // OpenAI only includes `usage` on the final streamed chunk when
            // asked for it; without this, streamed token counts are 0. The
            // Responses API needs no such nudge — its terminal event always
            // carries usage.
            let mut j = json.clone();
            j["stream_options"] = json!({"include_usage": true});
            serde_json::to_vec(&j).unwrap_or_else(|_| body_bytes.to_vec())
        }
        _ => body_bytes.to_vec(),
    };

    let mut proxy_req = state.client.request(method, &upstream.url);
    for (name, value) in headers.iter() {
        if forwards_upstream(name.as_str()) {
            proxy_req = proxy_req.header(name.clone(), value.clone());
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
        handle_streaming(state, upstream, wire, response, task_id, model_name, latency, status, call).await
    } else {
        Ok(handle_unary(state, upstream, wire, response, task_id, model_name, latency, status, call).await)
    }
}

/// The aux endpoints (`/v1/models`, `count_tokens`): forwarded verbatim and
/// recorded nowhere. They are questions about a call the agent is about to
/// make, not calls themselves — answering them with a 404 is what used to
/// make agents decide their provider was down.
async fn forward_pass_through(
    state: Arc<AppState>,
    upstream: Upstream,
    headers: HeaderMap,
    method: Method,
    body: Vec<u8>,
) -> Result<Response, StatusCode> {
    let mut proxy_req = state.client.request(method, &upstream.url);
    for (name, value) in headers.iter() {
        if forwards_upstream(name.as_str()) {
            proxy_req = proxy_req.header(name.clone(), value.clone());
        }
    }
    // A GET carries no body and needs no content-type; forcing one on
    // every call would be harmless for the hosted APIs but is noise a
    // strict local server may reject.
    if !body.is_empty() {
        proxy_req = proxy_req.header("Content-Type", "application/json");
    }
    let proxy_req = proxy_req.body(body);

    let response = proxy_req.send().await.map_err(|e| {
        eprintln!("Proxy error ({}): {}", upstream.url, e);
        StatusCode::BAD_GATEWAY
    })?;

    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let response_headers = response.headers().clone();
    let bytes = response.bytes().await.unwrap_or_default();

    let mut response_builder = Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        response_builder = response_builder.header(key, value);
    }
    response_builder
        .body(axum::body::Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Whether an incoming request header is passed on to the provider.
///
/// An allowlist of the two hosted APIs' auth headers used to stand here,
/// which silently dropped the credential of anything else — an
/// OpenAI-compatible gateway authenticating with `api-key` got an auth
/// failure on every call, with nothing in the traffic to explain it. Since
/// any provider can be configured now, the rule is inverted: everything is
/// forwarded except headers that belong to *this* hop or to the proxy
/// itself.
fn forwards_upstream(name: &str) -> bool {
    // Hop-by-hop headers describe the client↔proxy connection, and `host` /
    // `content-length` are recomputed for the new request (the body can
    // differ — a streamed OpenAI call gets `stream_options` added).
    const CONNECTION_SCOPED: [&str; 11] = [
        "host",
        "content-length",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        // Responses are forwarded with the provider's own headers intact,
        // so asking upstream for a compressed body risks handing the caller
        // bytes whose `content-encoding` no longer describes them.
        "accept-encoding",
    ];
    // Harnesswurm's own routing and attribution headers. They mean nothing
    // to a provider, and a strict gateway can reject unknown ones.
    const PROXY_LOCAL: [&str; 4] = ["x-agent-id", "x-session-id", "x-experiment-id", "x-provider"];

    let name = name.to_ascii_lowercase();
    // `content-type` is set explicitly on the forwarded request below.
    if name == "content-type" {
        return false;
    }
    !CONNECTION_SCOPED.contains(&name.as_str()) && !PROXY_LOCAL.contains(&name.as_str())
}

/// Best-effort "what is this call actually asking for" summary, taken from
/// the most recent user turn — OpenAI and Anthropic request bodies use a
/// `messages: [{role, content}]` shape, the Responses API the same shape
/// under `input` (or a bare string). `content` is either a plain string or
/// a list of content blocks (only the text ones are used here).
fn extract_task_preview(body: &Value) -> Option<String> {
    let messages = body.get("messages").and_then(Value::as_array);
    let last_user = messages
        .and_then(|ms| ms.iter().rev().find(|m| m["role"] == "user").cloned())
        .or_else(|| {
            body.get("input")
                .and_then(Value::as_array)
                .and_then(|items| items.iter().rev().find(|m| m["role"] == "user").cloned())
        })
        .or_else(|| {
            body.get("input").and_then(Value::as_str).map(|input| {
                json!({ "role": "user", "content": input })
            })
        })?;
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

fn extract_usage(wire: WireApi, json: &Value) -> UsageInfo {
    match wire {
        WireApi::OpenAi => {
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
        WireApi::Anthropic => {
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
        WireApi::Responses => {
            // The Responses API reports `input_tokens` including cached ones,
            // mirroring `prompt_tokens` on chat/completions. Tool calls ride
            // in the output *items* rather than a `tool_calls` array and are
            // not parsed yet, so they count as 0 — visible honesty rather
            // than a guessed number.
            let usage = &json["usage"];
            let total_input = usage["input_tokens"].as_i64().unwrap_or(0);
            let cache_read = usage["input_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
            UsageInfo {
                prompt_tokens: (total_input - cache_read).max(0),
                completion_tokens: usage["output_tokens"].as_i64().unwrap_or(0),
                cache_creation_tokens: 0,
                cache_read_tokens: cache_read,
                tool_calls_count: 0,
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
/// The Responses API has no such field; its `status` only names failure
/// modes, so anything other than a completed response is surfaced.
fn extract_stop_reason(wire: WireApi, json: &Value) -> Option<String> {
    match wire {
        WireApi::OpenAi => json["choices"][0]["finish_reason"].as_str().map(String::from),
        WireApi::Anthropic => json["stop_reason"].as_str().map(String::from),
        WireApi::Responses => json["status"]
            .as_str()
            .filter(|status| *status != "completed")
            .map(String::from),
    }
}

/// Whether the turn came back to the human. An explicit question tool says
/// so outright; otherwise a turn that ended without asking for a tool means
/// the agent produced its answer and is now sitting at a prompt.
fn turn_awaits_human(stop_reason: Option<&str>, has_question: bool) -> bool {
    has_question || matches!(stop_reason, Some("end_turn") | Some("stop"))
}

/// Every tool name a unary response called, in order and with repeats kept
/// — the same tool twice in one turn is two calls, and the attribution in
/// `get_experiment_tool_usage` depends on that count being right.
///
/// Unnamed tool calls are skipped rather than recorded as an empty name: a
/// malformed fragment should not become a phantom tool in the breakdown.
/// Responses-API calls name no tools (see `extract_usage`) and yield none.
fn extract_tool_names(wire: WireApi, json: &Value) -> Vec<String> {
    let names: Vec<&str> = match wire {
        WireApi::OpenAi => json["choices"][0]["message"]["tool_calls"].as_array().map(|calls| {
            calls.iter().filter_map(|tc| tc["function"]["name"].as_str()).collect()
        }),
        WireApi::Anthropic => json["content"].as_array().map(|blocks| {
            blocks.iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| b["name"].as_str())
                .collect()
        }),
        WireApi::Responses => None,
    }.unwrap_or_default();

    names.into_iter().map(String::from).collect()
}

/// Scans the same tool-calls/content a response already carries for one
/// matching a known "ask the human a question" convention. Unary twin of
/// the streaming accumulate-then-scan path in handle_streaming.
fn extract_agent_question(wire: WireApi, json: &Value) -> Option<(String, String)> {
    match wire {
        WireApi::OpenAi => {
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
        WireApi::Anthropic => {
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
        // No tool-call parsing on the Responses API yet, so no question can
        // be recognized either — see `extract_usage`.
        WireApi::Responses => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_unary(
    state: Arc<AppState>,
    upstream: Upstream,
    wire: WireApi,
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
        .map(|json| extract_usage(wire, json))
        .unwrap_or_default();
    let agent_question = parsed_response.as_ref()
        .and_then(|json| extract_agent_question(wire, json));
    let stop_reason = parsed_response.as_ref().and_then(|json| extract_stop_reason(wire, json));
    let (error_type, error_message) = parsed_response.as_ref()
        .map(extract_error_details)
        .unwrap_or((None, None));

    let tool_names = parsed_response.as_ref()
        .map(|json| extract_tool_names(wire, json))
        .unwrap_or_default();

    let response_text = String::from_utf8_lossy(&res_bytes).to_string();
    let _ = state.db.save_traffic_response(task_id, &response_text).await;
    if !tool_names.is_empty() {
        let _ = state.db.save_tool_calls(task_id, &tool_names).await;
    }
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
    call.record_rate_limits(&state, &upstream.name, &headers).await;
    call.notify(&state, "task_finished", &outcome.status);

    let cost = model_name.as_deref().and_then(|m| state.pricing.estimate_cost(
        m,
        &upstream.name,
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
    /// The tail of the last chunk, when it did not end on a line boundary.
    /// SSE events are line-framed but TCP chunks are not: a `data:` line
    /// split across two chunks used to have each half parsed as nothing.
    line_carry: String,
}

impl StreamAccumulator {
    /// Every tool the stream named, in first-seen order and with repeats
    /// kept. A fragment whose name never arrived is dropped rather than
    /// recorded blank — see `extract_tool_names` for the unary twin.
    fn tool_names(&self) -> Vec<String> {
        self.tool_calls.iter().filter_map(|(_, acc)| acc.name.clone()).collect()
    }

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

    /// Feeds one raw chunk into the accumulator, parsing only the lines it
    /// completes and carrying the remainder into the next chunk.
    fn feed_chunk(&mut self, wire: WireApi, text: &str) {
        self.line_carry.push_str(text);
        while let Some(newline) = self.line_carry.find('\n') {
            let rest = self.line_carry.split_off(newline + 1);
            let line = std::mem::replace(&mut self.line_carry, rest);
            self.feed_line(wire, line.trim_end_matches(['\r', '\n']));
        }
    }

    /// Flushes whatever was still mid-line when the stream ended, so a
    /// final event sent without a trailing newline is not lost.
    fn finish(&mut self, wire: WireApi) {
        if !self.line_carry.is_empty() {
            let line = std::mem::take(&mut self.line_carry);
            self.feed_line(wire, line.trim_end_matches('\r'));
        }
    }

    fn feed_line(&mut self, wire: WireApi, line: &str) {
        let Some(data) = line.strip_prefix("data: ") else { return };
        if data.trim() == "[DONE]" {
            self.saw_terminal = true;
            return;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else { return };
        match wire {
            WireApi::OpenAi => self.process_openai_event(&json),
            WireApi::Anthropic => self.process_anthropic_event(&json),
            WireApi::Responses => self.process_responses_event(&json),
        }
    }

    fn process_openai_event(&mut self, json: &Value) {
        if !json["error"].is_null() {
            let (error_type, message) = extract_error_details(json);
            self.error_type = error_type;
            self.error_message = message;
        }

        if let Some(usage_obj) = json.get("usage").filter(|u| !u.is_null()) {
            let total_prompt = usage_obj["prompt_tokens"].as_i64().unwrap_or(0);
            let cache_read = usage_obj["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
            self.usage.prompt_tokens = (total_prompt - cache_read).max(0);
            self.usage.cache_read_tokens = cache_read;
            self.usage.completion_tokens = usage_obj["completion_tokens"].as_i64().unwrap_or(self.usage.completion_tokens);
        }

        // Sent once, on the final content chunk of the turn.
        if let Some(reason) = json["choices"][0]["finish_reason"].as_str() {
            self.stop_reason = Some(reason.to_string());
        }

        if let Some(tool_calls) = json["choices"][0]["delta"]["tool_calls"].as_array() {
            for tc in tool_calls {
                // Key by index, not id: OpenAI only sends `id` on a tool
                // call's first delta fragment, while `index` stays stable
                // across every fragment of that same call.
                let key = tc.get("index").map(|v| v.to_string())
                    .or_else(|| tc.get("id").and_then(|v| v.as_str()).map(String::from));
                if let Some(key) = key {
                    self.tool_ids.insert(key.clone());

                    let entry = agent_question::tool_call_entry(&mut self.tool_calls, &key);
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

    fn process_anthropic_event(&mut self, json: &Value) {
        match json["type"].as_str() {
            Some("message_start") => {
                let usage_obj = &json["message"]["usage"];
                self.usage.prompt_tokens = usage_obj["input_tokens"].as_i64().unwrap_or(0);
                self.usage.cache_creation_tokens = usage_obj["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                self.usage.cache_read_tokens = usage_obj["cache_read_input_tokens"].as_i64().unwrap_or(0);
            }
            Some("content_block_start") => {
                if json["content_block"]["type"].as_str() == Some("tool_use") {
                    if let Some(id) = json["content_block"]["id"].as_str() {
                        self.tool_ids.insert(id.to_string());
                    }
                    // Keyed by the block's stream index (not its id) so it
                    // lines up with content_block_delta below, which only
                    // carries the index, not the id, on each fragment.
                    if let Some(index) = json["index"].as_i64() {
                        let entry = agent_question::tool_call_entry(&mut self.tool_calls, &index.to_string());
                        if let Some(name) = json["content_block"]["name"].as_str() {
                            entry.name = Some(name.to_string());
                        }
                    }
                }
            }
            Some("content_block_delta") => {
                if json["delta"]["type"].as_str() == Some("input_json_delta") {
                    if let (Some(index), Some(fragment)) = (json["index"].as_i64(), json["delta"]["partial_json"].as_str()) {
                        agent_question::tool_call_entry(&mut self.tool_calls, &index.to_string()).arguments.push_str(fragment);
                    }
                }
            }
            Some("message_delta") => {
                if let Some(out) = json["usage"]["output_tokens"].as_i64() {
                    self.usage.completion_tokens = out;
                }
                if let Some(reason) = json["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            Some("message_stop") => self.saw_terminal = true,
            // Anthropic can fail mid-stream (an overload after the headers
            // already said 200), which is only visible as an error event.
            Some("error") => {
                let (error_type, message) = extract_error_details(json);
                self.error_type = error_type;
                self.error_message = message;
            }
            _ => {}
        }
    }

    fn process_responses_event(&mut self, json: &Value) {
        match json["type"].as_str() {
            // The terminal events of a Responses stream; the final response
            // object rides along and is where usage lives.
            Some("response.completed") | Some("response.incomplete") => {
                self.saw_terminal = true;
                self.usage = extract_usage(WireApi::Responses, &json["response"]);
                if json["type"] == "response.incomplete" {
                    self.stop_reason = Some("incomplete".to_string());
                }
            }
            Some("response.failed") => {
                self.saw_terminal = true;
                let (error_type, message) = extract_error_details(&json["response"]);
                self.error_type = error_type;
                self.error_message = message;
            }
            Some("error") => {
                let (error_type, message) = extract_error_details(json);
                self.error_type = error_type;
                self.error_message = message;
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
    upstream: Upstream,
    wire: WireApi,
    res: reqwest::Response,
    task_id: i64,
    model_name: Option<String>,
    latency: i64,
    status: StatusCode,
    call: CallContext,
) -> Result<Response, StatusCode> {
    let headers = res.headers().clone();
    call.record_rate_limits(&state, &upstream.name, &headers).await;

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
            acc.feed_chunk(wire, &chunk_text);
        }
        acc.finish(wire);
        // The loop above ends when the last chunk has been forwarded, which
        // is the only moment the true wall-clock cost of a streamed call is
        // known — `latency` was measured back when the headers arrived.
        let duration_ms = call.started.elapsed().as_millis() as i64;
        acc.usage.tool_calls_count = acc.tool_ids.len() as i64;

        let agent_question = acc.agent_question();
        let outcome = classify_stream_outcome(&acc, &raw_text, status, agent_question.is_some(), latency, duration_ms);

        let cost = model_name.as_deref().and_then(|m| state.pricing.estimate_cost(
            m,
            &upstream.name,
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
        let tool_names = acc.tool_names();
        if !tool_names.is_empty() {
            let _ = state.db.save_tool_calls(task_id, &tool_names).await;
        }
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

    const TEST_PROVIDERS: &str = "providers:\n\
        \x20 - name: openai\n    api: openai\n    base_url: https://api.openai.com/v1\n    default: true\n\
        \x20 - name: ollama\n    api: openai\n    base_url: http://localhost:11434/v1\n\
        \x20 - name: anthropic\n    api: anthropic\n    base_url: https://api.anthropic.com\n    default: true\n";

    fn provider_table() -> ProviderTable {
        ProviderTable::parse(TEST_PROVIDERS).expect("test providers parse")
    }

    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn a_custom_providers_auth_header_is_forwarded() {
        // The case that motivated inverting the rule: an OpenAI-compatible
        // gateway that authenticates with `api-key` rather than a bearer
        // token used to have its credential dropped silently.
        for header in ["authorization", "x-api-key", "api-key", "anthropic-version", "anthropic-beta", "openai-organization", "x-goog-api-key"] {
            assert!(forwards_upstream(header), "{header} carries auth or provider intent");
        }
    }

    #[test]
    fn connection_scoped_and_proxy_local_headers_stay_on_this_hop() {
        for header in ["host", "content-length", "connection", "transfer-encoding", "accept-encoding", "content-type"] {
            assert!(!forwards_upstream(header), "{header} describes this hop, not the upstream call");
        }
        // Attribution headers are Harnesswurm's own vocabulary; a provider
        // has no use for them and a strict gateway may reject them.
        for header in ["x-agent-id", "X-Session-ID", "x-experiment-id", "x-provider"] {
            assert!(!forwards_upstream(header), "{header} is proxy-local");
        }
    }

    #[test]
    fn an_unnamed_call_goes_to_the_style_default() {
        let upstream = resolve_upstream(&provider_table(), Endpoint::ChatCompletions, None, &HeaderMap::new()).unwrap();
        assert_eq!(upstream.name, "openai");
        assert_eq!(upstream.url, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn a_named_provider_is_forwarded_to_its_own_base_url() {
        let upstream = resolve_upstream(&provider_table(), Endpoint::ChatCompletions, Some("ollama"), &HeaderMap::new()).unwrap();
        assert_eq!(upstream.name, "ollama");
        assert_eq!(upstream.url, "http://localhost:11434/v1/chat/completions");
    }

    #[test]
    fn the_x_provider_header_selects_an_upstream_too() {
        let headers = header_map(&[("x-provider", "ollama")]);
        let upstream = resolve_upstream(&provider_table(), Endpoint::ChatCompletions, None, &headers).unwrap();
        assert_eq!(upstream.name, "ollama");
    }

    #[test]
    fn the_path_wins_over_the_header() {
        let headers = header_map(&[("x-provider", "ollama")]);
        let upstream = resolve_upstream(&provider_table(), Endpoint::ChatCompletions, Some("openai"), &headers).unwrap();
        assert_eq!(upstream.name, "openai");
    }

    #[test]
    fn an_unknown_provider_is_refused_rather_than_sent_to_the_hosted_api() {
        let (status, message) = resolve_upstream(
            &provider_table(), Endpoint::ChatCompletions, Some("typo"), &HeaderMap::new(),
        ).unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(message.contains("ollama"), "the error should list what is configured: {message}");
    }

    #[test]
    fn a_provider_reached_on_the_wrong_style_endpoint_is_refused() {
        let (status, message) = resolve_upstream(
            &provider_table(), Endpoint::Messages, Some("ollama"), &HeaderMap::new(),
        ).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(message.contains("chat/completions"), "the error should name the right route: {message}");
    }

    #[test]
    fn an_empty_provider_header_falls_back_to_the_default() {
        let headers = header_map(&[("x-provider", "   ")]);
        let upstream = resolve_upstream(&provider_table(), Endpoint::ChatCompletions, None, &headers).unwrap();
        assert_eq!(upstream.name, "openai");
    }

    #[test]
    fn aux_endpoints_forward_to_their_own_upstream_paths() {
        let models = resolve_upstream(&provider_table(), Endpoint::Models, None, &HeaderMap::new()).unwrap();
        assert_eq!(models.url, "https://api.openai.com/v1/models");

        let count = resolve_upstream(&provider_table(), Endpoint::CountTokens, None, &HeaderMap::new()).unwrap();
        assert_eq!(count.url, "https://api.anthropic.com/v1/messages/count_tokens");

        let responses = resolve_upstream(&provider_table(), Endpoint::Responses, Some("openai"), &HeaderMap::new()).unwrap();
        assert_eq!(responses.url, "https://api.openai.com/v1/responses");
    }

    #[test]
    fn a_bare_models_call_follows_the_callers_own_auth_style() {
        // An Anthropic client authenticates with x-api-key and should reach
        // the Anthropic default, not have its key forwarded to OpenAI.
        let anthropic = header_map(&[("x-api-key", "sk-ant")]);
        let upstream = resolve_upstream(&provider_table(), Endpoint::Models, None, &anthropic).unwrap();
        assert_eq!(upstream.name, "anthropic");

        // A bearer token (or no auth at all, e.g. a local server) reads as
        // OpenAI-style, the more common shape of the two.
        let openai = header_map(&[("authorization", "Bearer sk-xyz")]);
        let upstream = resolve_upstream(&provider_table(), Endpoint::Models, None, &openai).unwrap();
        assert_eq!(upstream.name, "openai");
    }

    #[test]
    fn a_named_models_provider_is_served_regardless_of_style() {
        // Both APIs serve /v1/models, so an anthropic provider is reachable
        // on it without a style complaint.
        let upstream = resolve_upstream(&provider_table(), Endpoint::Models, Some("anthropic"), &HeaderMap::new()).unwrap();
        assert_eq!(upstream.url, "https://api.anthropic.com/v1/models");
    }

    #[test]
    fn the_bundled_default_providers_yaml_parses() {
        // It ships as a first-run default, so a typo in it would leave every
        // fresh install silently on the fallback rather than its own config.
        let table = ProviderTable::parse(DEFAULT_PROVIDERS_YAML).expect("bundled providers.yaml parses");
        assert_eq!(table.default_for(ApiStyle::OpenAI).name, "openai");
        assert_eq!(table.default_for(ApiStyle::Anthropic).name, "anthropic");
    }

    #[test]
    fn the_bundled_default_fingerprints_yaml_parses() {
        // Same first-run argument as the providers file.
        let table = fingerprints::FingerprintTable::parse(DEFAULT_FINGERPRINTS_YAML)
            .expect("bundled fingerprints.yaml parses");
        assert_eq!(table.match_user_agent("claude-cli/1.0.55"), Some("claude-code"));
    }

    #[test]
    fn proxy_paths_parse_into_endpoint_provider_and_run() {
        let bare = parse_proxy_path("/v1/chat/completions").unwrap();
        assert_eq!(bare.endpoint, Endpoint::ChatCompletions);
        assert!(bare.provider.is_none() && bare.run.is_none());

        let named = parse_proxy_path("/p/ollama/v1/messages").unwrap();
        assert_eq!(named.endpoint, Endpoint::Messages);
        assert_eq!(named.provider.as_deref(), Some("ollama"));

        let run = parse_proxy_path("/r/kilo/issue-1284/issue-1284-kilo/v1/messages").unwrap();
        assert_eq!(run.endpoint, Endpoint::Messages);
        let run = run.run.expect("run attribution present");
        assert_eq!(run.agent, "kilo");
        assert_eq!(run.experiment.as_deref(), Some("issue-1284"));
        assert_eq!(run.session, "issue-1284-kilo");

        let run_no_experiment = parse_proxy_path("/r/kilo/kilo-1/v1/chat/completions").unwrap();
        let run = run_no_experiment.run.expect("run attribution present");
        assert_eq!(run.agent, "kilo");
        assert_eq!(run.experiment, None);
        assert_eq!(run.session, "kilo-1");
    }

    #[test]
    fn run_and_provider_prefixes_combine() {
        let path = parse_proxy_path("/r/kilo/issue-1284/s-1/p/ollama/v1/chat/completions").unwrap();
        assert_eq!(path.endpoint, Endpoint::ChatCompletions);
        assert_eq!(path.provider.as_deref(), Some("ollama"));
        let run = path.run.expect("run attribution present");
        assert_eq!(run.experiment.as_deref(), Some("issue-1284"));
    }

    #[test]
    fn count_tokens_is_not_swallowed_by_the_messages_tail() {
        let path = parse_proxy_path("/v1/messages/count_tokens").unwrap();
        assert_eq!(path.endpoint, Endpoint::CountTokens);

        let nested = parse_proxy_path("/r/claude-code/e-1/s-1/v1/messages/count_tokens").unwrap();
        assert_eq!(nested.endpoint, Endpoint::CountTokens);
    }

    #[test]
    fn unknown_routes_and_prefixes_are_refused() {
        assert!(parse_proxy_path("/v1/embeddings").is_err());
        assert!(parse_proxy_path("/v1/messages/subscribe").is_err());
        assert!(parse_proxy_path("/p/ollama/whatever/v1/messages").is_err());
        // A run prefix needs two or three labelled segments, not one.
        assert!(parse_proxy_path("/r/kilo/v1/messages").is_err());
        assert!(parse_proxy_path("/favicon.ico").is_err());
    }

    #[test]
    fn run_prefixes_round_trip_from_the_builder_the_wrapper_uses() {
        let prefix = build_run_prefix("kilo", Some("issue-1284"), "issue-1284-kilo", None).unwrap();
        assert_eq!(prefix, "/r/kilo/issue-1284/issue-1284-kilo");

        let path = parse_proxy_path(&format!("{prefix}/v1/messages")).unwrap();
        let run = path.run.expect("run attribution present");
        assert_eq!((run.agent.as_str(), run.experiment.as_deref(), run.session.as_str()), ("kilo", Some("issue-1284"), "issue-1284-kilo"));

        let with_provider = build_run_prefix("kilo", None, "s-1", Some("ollama")).unwrap();
        let path = parse_proxy_path(&format!("{with_provider}/v1/chat/completions")).unwrap();
        assert_eq!(path.provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn run_prefixes_reject_segments_that_cannot_travel_in_a_url() {
        assert!(build_run_prefix("my agent", None, "s", None).is_err());
        assert!(build_run_prefix("a/b", None, "s", None).is_err());
        assert!(build_run_prefix("", None, "s", None).is_err());
        assert!(build_run_prefix("a", None, "s p a c e", None).is_err());
    }

    fn fingerprints_table() -> fingerprints::FingerprintTable {
        fingerprints::FingerprintTable::parse(
            "fingerprints:\n  - agent: claude-code\n    user_agents: [\"claude-cli\"]\n    system_prompts: [\"You are Claude Code\"]\n",
        )
        .expect("test fingerprints parse")
    }

    #[test]
    fn attribution_prefers_the_run_prefix_over_everything() {
        let run = RunAttribution {
            agent: "kilo".to_string(),
            experiment: Some("e-1".to_string()),
            session: "s-1".to_string(),
        };
        let headers = header_map(&[
            ("X-Agent-ID", "header-agent"),
            ("X-Session-ID", "header-session"),
            ("X-Experiment-ID", "header-experiment"),
            ("user-agent", "claude-cli/1.0"),
        ]);
        let attribution = resolve_attribution(Some(&run), &headers, None, None, &fingerprints_table());
        assert_eq!((attribution.agent.as_str(), attribution.session.as_str()), ("kilo", "s-1"));
        assert_eq!(attribution.experiment.as_deref(), Some("e-1"));
    }

    #[test]
    fn attribution_headers_beat_fingerprints() {
        let headers = header_map(&[
            ("X-Agent-ID", "explicit"),
            ("X-Session-ID", "explicit-session"),
            ("user-agent", "claude-cli/1.0"),
        ]);
        let attribution = resolve_attribution(None, &headers, None, None, &fingerprints_table());
        assert_eq!((attribution.agent.as_str(), attribution.session.as_str()), ("explicit", "explicit-session"));
    }

    #[test]
    fn attribution_fingerprints_an_unlabelled_agent_from_what_it_sends() {
        // A Claude Code request: no attribution headers, but a signature
        // User-Agent and a signature system prompt.
        let headers = header_map(&[("user-agent", "claude-cli/1.0.55 (external, cli)")]);
        let body: Value = serde_json::from_str(
            r#"{"system": "You are Claude Code, Anthropic's official CLI for Claude.",
                "messages": [{"role": "user", "content": "fix the login bug"}]}"#,
        )
        .unwrap();

        let by_user_agent = resolve_attribution(None, &headers, Some(&body), Some(WireApi::Anthropic), &fingerprints_table());
        assert_eq!(by_user_agent.agent, "claude-code");

        // SDK User-Agent that names no agent: the system prompt still
        // identifies it.
        let generic_headers = header_map(&[("user-agent", "python-requests/2.31")]);
        let by_prompt = resolve_attribution(None, &generic_headers, Some(&body), Some(WireApi::Anthropic), &fingerprints_table());
        assert_eq!(by_prompt.agent, "claude-code");
    }

    #[test]
    fn attribution_without_any_signal_falls_back_to_an_auto_session() {
        let headers = HeaderMap::new();
        let body: Value = serde_json::from_str(
            r#"{"messages": [{"role": "user", "content": "fix the login bug"}]}"#,
        )
        .unwrap();

        let attribution = resolve_attribution(None, &headers, Some(&body), Some(WireApi::OpenAi), &fingerprints_table());
        assert_eq!(attribution.agent, "unknown_agent");
        assert!(attribution.session.starts_with("auto-"), "session {}", attribution.session);
        assert_eq!(attribution.experiment, None);
    }

    #[test]
    fn attribution_with_no_signals_at_all_uses_the_legacy_buckets() {
        let attribution = resolve_attribution(None, &HeaderMap::new(), None, None, &fingerprints_table());
        assert_eq!((attribution.agent.as_str(), attribution.session.as_str()), ("unknown_agent", "default_session"));
    }

    #[test]
    fn retention_days_parse_leniently() {
        assert_eq!(parse_retention_days(None), 30);
        assert_eq!(parse_retention_days(Some("7")), 7);
        assert_eq!(parse_retention_days(Some(" 0 ")), 0);
        assert_eq!(parse_retention_days(Some("forever")), 30);
        assert_eq!(parse_retention_days(Some("-1")), 30);
    }

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

        let usage = extract_usage(WireApi::OpenAi, &json);
        assert_eq!(usage.prompt_tokens, 70);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.tool_calls_count, 2);
    }

    #[test]
    fn unary_tool_names_keep_repeats_from_either_provider() {
        let openai: Value = serde_json::from_str(r#"{
            "choices": [{"message": {"tool_calls": [
                {"function": {"name": "read_file"}},
                {"function": {"name": "read_file"}},
                {"function": {"name": "bash"}}
            ]}}]
        }"#).unwrap();
        assert_eq!(
            extract_tool_names(WireApi::OpenAi, &openai),
            vec!["read_file", "read_file", "bash"],
        );

        let anthropic: Value = serde_json::from_str(r#"{
            "content": [
                {"type": "text", "text": "let me look"},
                {"type": "tool_use", "name": "read_file", "input": {}},
                {"type": "tool_use", "name": "read_file", "input": {}}
            ]
        }"#).unwrap();
        assert_eq!(
            extract_tool_names(WireApi::Anthropic, &anthropic),
            vec!["read_file", "read_file"],
        );
    }

    #[test]
    fn unary_tool_names_skip_calls_that_never_named_a_tool() {
        let json: Value = serde_json::from_str(r#"{
            "choices": [{"message": {"tool_calls": [
                {"function": {"arguments": "{}"}},
                {"function": {"name": "bash"}}
            ]}}]
        }"#).unwrap();
        assert_eq!(extract_tool_names(WireApi::OpenAi, &json), vec!["bash"]);
    }

    #[test]
    fn a_response_with_no_tools_names_none() {
        let json: Value = serde_json::from_str(r#"{"choices": [{"message": {"content": "done"}}]}"#).unwrap();
        assert!(extract_tool_names(WireApi::OpenAi, &json).is_empty());
        assert!(extract_tool_names(WireApi::Anthropic, &json).is_empty());
    }

    #[test]
    fn streamed_tool_names_come_from_the_accumulated_fragments() {
        let mut acc = StreamAccumulator::default();
        for chunk in [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_file\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"bash\"}}]}}]}\n\n",
        ] {
            acc.feed_chunk(WireApi::OpenAi, chunk);
        }

        assert_eq!(acc.tool_names(), vec!["read_file", "bash"]);
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

        let usage = extract_usage(WireApi::Anthropic, &json);
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
        acc.feed_chunk(WireApi::OpenAi, "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"foo\"}}]}}]}\n\n");
        acc.feed_chunk(WireApi::OpenAi, "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n");
        acc.feed_chunk(WireApi::OpenAi, "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_2\",\"function\":{\"name\":\"bar\"}}]}}]}\n\n");
        acc.feed_chunk(WireApi::OpenAi, "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":25,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\ndata: [DONE]\n\n");

        assert_eq!(acc.tool_ids.len(), 2);
        assert_eq!(acc.usage.prompt_tokens, 80);
        assert_eq!(acc.usage.cache_read_tokens, 40);
        assert_eq!(acc.usage.completion_tokens, 25);
    }

    #[test]
    fn anthropic_stream_accumulates_across_events() {
        let mut acc = StreamAccumulator::default();

        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":50,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5}}}\n\n");
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\"}}\n\n");
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n");

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

        let result = extract_agent_question(WireApi::OpenAi, &json);
        assert_eq!(result, Some(("ask_followup_question".to_string(), "Which file?".to_string())));
    }

    #[test]
    fn openai_unary_no_question_tool_yields_none() {
        let json: Value = serde_json::from_str(r#"{
            "choices": [{"message": {"role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}
            ]}}]
        }"#).unwrap();

        assert_eq!(extract_agent_question(WireApi::OpenAi, &json), None);
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

        let result = extract_agent_question(WireApi::Anthropic, &json);
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
            acc.feed_chunk(WireApi::OpenAi, &line);
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

        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"ask_followup_question\"}}\n\n");
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"question\\\": \\\"Use \"}}\n\n");
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"TypeScript?\\\"}\"}}\n\n");

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
        assert_eq!(extract_stop_reason(WireApi::OpenAi, &openai).as_deref(), Some("tool_calls"));

        let anthropic: Value = serde_json::from_str(r#"{"stop_reason":"end_turn"}"#).unwrap();
        assert_eq!(extract_stop_reason(WireApi::Anthropic, &anthropic).as_deref(), Some("end_turn"));
    }

    #[test]
    fn openai_stream_records_the_finish_reason() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(WireApi::OpenAi, "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");

        assert_eq!(acc.stop_reason.as_deref(), Some("stop"));
        assert!(acc.saw_terminal);
    }

    #[test]
    fn anthropic_stream_records_stop_reason_and_terminal_event() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\n");
        assert_eq!(acc.stop_reason.as_deref(), Some("tool_use"));
        assert!(!acc.saw_terminal, "the stream has not ended yet");

        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_stop\"}\n\n");
        assert!(acc.saw_terminal);
    }

    #[test]
    fn an_error_event_mid_stream_is_captured_despite_a_200() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n");

        let outcome = classify_stream_outcome(&acc, "", StatusCode::OK, false, 10, 20);
        assert_eq!(outcome.status, session_state::STATUS_OVERLOADED);
        assert_eq!(outcome.error_message.as_deref(), Some("Overloaded"));
        assert!(!outcome.awaiting_input);
    }

    #[test]
    fn a_stream_that_stops_without_its_terminal_marker_is_interrupted() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n");

        let outcome = classify_stream_outcome(&acc, "", StatusCode::OK, false, 10, 900);
        assert_eq!(outcome.status, session_state::STATUS_INTERRUPTED);
        assert_eq!(outcome.duration_ms, Some(900));
    }

    #[test]
    fn a_completed_stream_that_ended_its_turn_is_waiting_on_the_human() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n");

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

    /// The bug this catches: a `data:` line split across two TCP chunks used
    /// to be missed entirely — the first half parsed as nothing, the second
    /// as garbage — so token counts and stop reasons silently came out as 0.
    #[test]
    fn a_data_line_split_across_chunks_is_still_parsed() {
        let mut acc = StreamAccumulator::default();
        let event = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":20}}}"#;
        let (first, second) = event.split_at(event.len() / 2);

        acc.feed_chunk(WireApi::OpenAi, first);
        assert!(acc.usage.prompt_tokens == 0, "nothing is parsed from a partial line");
        acc.feed_chunk(WireApi::OpenAi, second);
        acc.feed_chunk(WireApi::OpenAi, "\n\ndata: [DONE]\n\n");

        assert_eq!(acc.usage.prompt_tokens, 80);
        assert_eq!(acc.usage.cache_read_tokens, 20);
        assert_eq!(acc.usage.completion_tokens, 7);
        assert_eq!(acc.stop_reason.as_deref(), Some("stop"));
        assert!(acc.saw_terminal);
    }

    #[test]
    fn an_anthropic_line_split_across_chunks_is_still_parsed() {
        let mut acc = StreamAccumulator::default();
        let event = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":42}}}"#;
        let (first, second) = event.split_at(event.len() / 2);

        acc.feed_chunk(WireApi::Anthropic, first);
        acc.feed_chunk(WireApi::Anthropic, second);
        acc.feed_chunk(WireApi::Anthropic, "\n\n");

        assert_eq!(acc.usage.prompt_tokens, 42);
    }

    #[test]
    fn a_final_event_without_a_trailing_newline_is_not_lost() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(WireApi::Anthropic, "data: {\"type\":\"message_stop\"}");
        acc.finish(WireApi::Anthropic);
        assert!(acc.saw_terminal);
    }

    #[test]
    fn responses_stream_usage_arrives_on_the_terminal_event() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(
            WireApi::Responses,
            "event: response.completed\n",
        );
        acc.feed_chunk(
            WireApi::Responses,
            &format!(
                "data: {}\n\n",
                json!({
                    "type": "response.completed",
                    "response": {"status": "completed", "usage": {
                        "input_tokens": 210, "output_tokens": 33,
                        "input_tokens_details": {"cached_tokens": 60}
                    }}
                })
            ),
        );

        assert!(acc.saw_terminal);
        assert_eq!(acc.usage.prompt_tokens, 150);
        assert_eq!(acc.usage.cache_read_tokens, 60);
        assert_eq!(acc.usage.completion_tokens, 33);

        // A stream that never reached its terminal event is interrupted,
        // same rule as the other two formats.
        let cut = StreamAccumulator::default();
        let outcome = classify_stream_outcome(&cut, "", StatusCode::OK, false, 10, 500);
        assert_eq!(outcome.status, session_state::STATUS_INTERRUPTED);
    }

    #[test]
    fn responses_stream_failure_is_classified_from_its_event() {
        let mut acc = StreamAccumulator::default();
        acc.feed_chunk(
            WireApi::Responses,
            &format!(
                "data: {}\n\n",
                json!({
                    "type": "response.failed",
                    "response": {"error": {"type": "invalid_api_key", "message": "bad key"}}
                })
            ),
        );

        assert!(acc.saw_terminal);
        let outcome = classify_stream_outcome(&acc, "", StatusCode::OK, false, 10, 20);
        assert_eq!(outcome.status, session_state::STATUS_ERROR);
        assert_eq!(outcome.error_type.as_deref(), Some("invalid_api_key"));
    }

    #[test]
    fn responses_unary_usage_splits_out_cached_tokens() {
        let json: Value = serde_json::from_str(
            r#"{"status": "completed", "usage": {
                "input_tokens": 300, "output_tokens": 12,
                "input_tokens_details": {"cached_tokens": 250}
            }}"#,
        )
        .unwrap();

        let usage = extract_usage(WireApi::Responses, &json);
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 250);
        assert_eq!(usage.completion_tokens, 12);
    }

    #[test]
    fn task_preview_reads_the_responses_input_field() {
        let array: Value = serde_json::from_str(
            r#"{"input": [
                {"role": "user", "content": "fix the login bug"},
                {"role": "assistant", "content": "done"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(extract_task_preview(&array).as_deref(), Some("fix the login bug"));

        let string: Value = serde_json::from_str(r#"{"input": "write the README"}"#).unwrap();
        assert_eq!(extract_task_preview(&string).as_deref(), Some("write the README"));
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
