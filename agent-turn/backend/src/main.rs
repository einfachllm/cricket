use axum::{
    extract::{Path, State, Request},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response, Sse},
    routing::{get, post},
    Router,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::Instant;
use futures::StreamExt as _;
use tokio::sync::mpsc;

mod db;
mod pricing;

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

pub struct AppState {
    pub db: db::Database,
    pub client: reqwest::Client,
    pub agents: Vec<AgentConfig>,
    pub pricing: pricing::PricingTable,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let db = db::Database::new("sqlite:agent_turn.db?mode=rwc").await?;

    let agent_config_file = std::path::Path::new("agents.yaml");
    let agents: Vec<AgentConfig> = if agent_config_file.exists() {
        let content = std::fs::read_to_string(agent_config_file)?;
        let parsed: AgentsFile = serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!(e))?;
        parsed.agents
    } else {
        Vec::new()
    };

    // Pre-populate agents from config
    for agent in &agents {
        let _ = db.get_or_create_agent(&agent.name).await;
    }

    let pricing = pricing::PricingTable::load(std::path::Path::new("pricing.yaml"));

    let state = Arc::new(AppState {
        db,
        client: reqwest::Client::new(),
        agents,
        pricing,
    });

    let app = Router::new()
        .route("/v1/chat/completions", post(openai_proxy_handler))
        .route("/v1/messages", post(anthropic_proxy_handler))
        .route("/v1/analytics/experiments", get(get_experiments))
        .route("/v1/analytics/experiments/:id/metrics", get(get_experiment_metrics))
        .route("/v1/analytics/tasks", get(get_recent_tasks))
        .route("/v1/analytics/tasks/:id/traffic", get(get_task_traffic))
        .with_state(state);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8081".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("Proxy server listening on http://{}", bind_addr);
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

    let request_text = String::from_utf8_lossy(&body_bytes).to_string();
    let _ = state.db.save_traffic_request(task_id, &request_text).await;

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

    let response = proxy_req.send().await.map_err(|e| {
        eprintln!("Proxy error: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let latency = start.elapsed().as_millis() as i64;

    if is_streaming {
        handle_streaming(state, provider, response, task_id, model_name, latency).await
    } else {
        Ok(handle_unary(state, provider, response, task_id, model_name, latency, status).await)
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

async fn handle_unary(
    state: Arc<AppState>,
    provider: Provider,
    res: reqwest::Response,
    task_id: i64,
    model_name: Option<String>,
    latency: i64,
    status: StatusCode,
) -> Response {
    let headers = res.headers().clone();
    let res_bytes = res.bytes().await.unwrap_or_default();

    let usage = serde_json::from_slice::<Value>(&res_bytes)
        .map(|json| extract_usage(provider, &json))
        .unwrap_or_default();

    let response_text = String::from_utf8_lossy(&res_bytes).to_string();
    let _ = state.db.save_traffic_response(task_id, &response_text).await;

    let cost = model_name.as_deref().and_then(|m| state.pricing.estimate_cost(
        m,
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

/// Folds one SSE chunk's `data: ` lines into the running totals. Chunk
/// boundaries aren't guaranteed to land on line boundaries, so a `data: `
/// line split across two chunks is missed — acceptable for a best-effort
/// telemetry sidecar that isn't the billing source of truth.
fn accumulate_openai_stream_chunk(text: &str, usage: &mut UsageInfo, tool_ids: &mut HashSet<String>) {
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        if data == "[DONE]" {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else { continue };

        if let Some(usage_obj) = json.get("usage").filter(|u| !u.is_null()) {
            let total_prompt = usage_obj["prompt_tokens"].as_i64().unwrap_or(0);
            let cache_read = usage_obj["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
            usage.prompt_tokens = (total_prompt - cache_read).max(0);
            usage.cache_read_tokens = cache_read;
            usage.completion_tokens = usage_obj["completion_tokens"].as_i64().unwrap_or(usage.completion_tokens);
        }

        if let Some(tool_calls) = json["choices"][0]["delta"]["tool_calls"].as_array() {
            for tc in tool_calls {
                // Key by index, not id: OpenAI only sends `id` on a tool
                // call's first delta fragment, while `index` stays stable
                // across every fragment of that same call.
                let key = tc.get("index").map(|v| v.to_string())
                    .or_else(|| tc.get("id").and_then(|v| v.as_str()).map(String::from));
                if let Some(key) = key {
                    tool_ids.insert(key);
                }
            }
        }
    }
}

fn accumulate_anthropic_stream_chunk(text: &str, usage: &mut UsageInfo, tool_ids: &mut HashSet<String>) {
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(json) = serde_json::from_str::<Value>(data) else { continue };

        match json["type"].as_str() {
            Some("message_start") => {
                let usage_obj = &json["message"]["usage"];
                usage.prompt_tokens = usage_obj["input_tokens"].as_i64().unwrap_or(0);
                usage.cache_creation_tokens = usage_obj["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                usage.cache_read_tokens = usage_obj["cache_read_input_tokens"].as_i64().unwrap_or(0);
            }
            Some("content_block_start") => {
                if json["content_block"]["type"].as_str() == Some("tool_use") {
                    if let Some(id) = json["content_block"]["id"].as_str() {
                        tool_ids.insert(id.to_string());
                    }
                }
            }
            Some("message_delta") => {
                if let Some(out) = json["usage"]["output_tokens"].as_i64() {
                    usage.completion_tokens = out;
                }
            }
            _ => {}
        }
    }
}

async fn handle_streaming(
    state: Arc<AppState>,
    provider: Provider,
    res: reqwest::Response,
    task_id: i64,
    model_name: Option<String>,
    latency: i64,
) -> Result<Response, StatusCode> {
    // Unbounded and non-blocking so a slow telemetry consumer can never add
    // latency to (or drop chunks from) the response the caller actually sees.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let stream = res.bytes_stream().map(move |chunk_result| {
        match chunk_result {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                let _ = tx.send(text.clone());
                Ok(axum::response::sse::Event::default().data(text))
            }
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        }
    });

    tokio::spawn(async move {
        let mut usage = UsageInfo::default();
        let mut tool_ids: HashSet<String> = HashSet::new();
        let mut raw_text = String::new();

        while let Some(chunk_text) = rx.recv().await {
            raw_text.push_str(&chunk_text);
            match provider {
                Provider::OpenAI => accumulate_openai_stream_chunk(&chunk_text, &mut usage, &mut tool_ids),
                Provider::Anthropic => accumulate_anthropic_stream_chunk(&chunk_text, &mut usage, &mut tool_ids),
            }
        }
        usage.tool_calls_count = tool_ids.len() as i64;

        let cost = model_name.as_deref().and_then(|m| state.pricing.estimate_cost(
            m,
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
        let _ = state.db.save_traffic_response(task_id, &raw_text).await;
    });

    Ok(Sse::new(stream).into_response())
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
        let mut usage = UsageInfo::default();
        let mut tool_ids = HashSet::new();

        // First fragment carries `id`; the continuation fragment for the
        // same call only carries `index` — both must resolve to one call.
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"foo\"}}]}}]}\n\n",
            &mut usage, &mut tool_ids,
        );
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
            &mut usage, &mut tool_ids,
        );
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_2\",\"function\":{\"name\":\"bar\"}}]}}]}\n\n",
            &mut usage, &mut tool_ids,
        );
        accumulate_openai_stream_chunk(
            "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":25,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\ndata: [DONE]\n\n",
            &mut usage, &mut tool_ids,
        );

        assert_eq!(tool_ids.len(), 2);
        assert_eq!(usage.prompt_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.completion_tokens, 25);
    }

    #[test]
    fn anthropic_stream_accumulates_across_events() {
        let mut usage = UsageInfo::default();
        let mut tool_ids = HashSet::new();

        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":50,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5}}}\n\n",
            &mut usage, &mut tool_ids,
        );
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\"}}\n\n",
            &mut usage, &mut tool_ids,
        );
        accumulate_anthropic_stream_chunk(
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n",
            &mut usage, &mut tool_ids,
        );

        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.cache_read_tokens, 5);
        assert_eq!(usage.completion_tokens, 42);
        assert_eq!(tool_ids.len(), 1);
    }
}
