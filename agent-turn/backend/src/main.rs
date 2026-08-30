use axum::{
    extract::{Path, State, Request},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response, Sse},
    routing::{get, post},
    Router,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::time::Instant;
use futures::StreamExt as _;
use tokio::sync::mpsc;

mod db;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
struct StreamMetrics {
    task_id: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    tool_calls_count: i64,
    latency_ms: i64,
}

pub struct AppState {
    pub db: db::Database,
    pub client: reqwest::Client,
    pub agents: Vec<AgentConfig>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    let db = db::Database::new("sqlite:agent_turn.db?mode=rwc").await?;
    
    let agent_config_file = std::path::Path::new("agents.yaml");
    let agents: Vec<AgentConfig> = if agent_config_file.exists() {
        let content = std::fs::read_to_string(agent_config_file)?;
        serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!(e))?
    } else {
        Vec::new()
    };
    
    // Pre-populate agents from config
    for agent in &agents {
        let _ = db.get_or_create_agent(&agent.name).await;
    }
    
    let state = Arc::new(AppState {
        db,
        client: reqwest::Client::new(),
        agents,
    });
    
    let app = Router::new()
        .route("/v1/chat/completions", post(proxy_handler))
        .route("/v1/analytics/experiments", get(get_experiments))
        .route("/v1/analytics/experiments/:id/metrics", get(get_experiment_metrics))
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

async fn proxy_handler(
    State(state): State<Arc<AppState>>,
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

    let experiment_id = headers.get("X-Experiment-ID")
        .and_then(|v| v.to_str().ok())
        .map(|name| {
            let state = state.clone();
            async move { state.db.get_or_create_experiment(name, None).await }
        });

    let (experiment_id, task_id) = match experiment_id {
        Some(fut) => {
            let eid = fut.await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let tid = state.db.create_task(agent_id, Some(eid), None, Some(session_id.to_string()), None).await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            (Some(eid), tid)
        },
        None => {
            let tid = state.db.create_task(agent_id, None, None, Some(session_id.to_string()), None).await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            (None, tid)
        }
    };
    
    let target_url = "https://api.openai.com/v1/chat/completions";
    let auth_header = headers.get("Authorization").cloned().unwrap_or_else(|| HeaderValue::from_static(""));
    
    let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX).await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(json!({}));
    let is_streaming = body_json["stream"].as_bool().unwrap_or(false);
    
    let proxy_req = state.client.request(method, target_url)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json")
        .body(body_bytes);
    
    let response = proxy_req.send().await.map_err(|e| {
        eprintln!("Proxy error: {}", e);
        StatusCode::BAD_GATEWAY
    })?;
    
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let latency = start.elapsed().as_millis() as i64;
    
    if is_streaming {
        handle_streaming(state, response, task_id, latency, status).await
    } else {
        Ok(handle_unary(state, response, task_id, latency, status).await)
    }
}

async fn handle_unary(
    state: Arc<AppState>,
    res: reqwest::Response,
    task_id: i64,
    latency: i64,
    status: StatusCode,
) -> Response {
    let headers = res.headers().clone();
    let res_bytes = res.bytes().await.unwrap_or_default();
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;
    let mut tool_calls_count = 0;

    if let Ok(json) = serde_json::from_slice::<Value>(&res_bytes) {
        prompt_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        completion_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);
        tool_calls_count = json["choices"][0]["message"]["tool_calls"].as_array().map(|a| a.len() as i64).unwrap_or(0);
    }

    let _ = state.db.log_metric(task_id, prompt_tokens, completion_tokens, tool_calls_count, latency).await;

    let mut response_builder = axum::response::Response::builder().status(status);
    for (key, value) in headers.iter() {
        response_builder = response_builder.header(key, value);
    }
    response_builder.body(axum::body::Bytes::from(res_bytes).into()).unwrap()
}

async fn handle_streaming(
    state: Arc<AppState>,
    res: reqwest::Response,
    task_id: i64,
    latency: i64,
    _status: StatusCode,
) -> Result<Response, StatusCode> {
    let (tx, mut rx) = mpsc::channel::<StreamMetrics>(10);

    let stream = res.bytes_stream().then(move |chunk_result| {
        let tx = tx.clone();
        async move {
            let mut prompt_tokens = 0;
            let mut completion_tokens = 0;
            let mut tool_calls_count = 0;

            match chunk_result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    for line in text.lines() {
                        if line.starts_with("data: ") {
                            let data_part = &line[6..];
                            if data_part == "[DONE]" {
                                continue;
                            }
                            if let Ok(json) = serde_json::from_str::<Value>(data_part) {
                                if let Some(usage) = json["usage"].as_object() {
                                    prompt_tokens += usage.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                                    completion_tokens += usage.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                                }
                                if let Some(tool_calls) = json["choices"][0]["message"]["tool_calls"].as_array() {
                                    tool_calls_count = tool_calls.len() as i64;
                                }
                            }
                        }
                    }
                    
                    if prompt_tokens > 0 || completion_tokens > 0 || tool_calls_count > 0 {
                        let _ = tx.send(StreamMetrics {
                            task_id,
                            prompt_tokens,
                            completion_tokens,
                            tool_calls_count,
                            latency_ms: latency,
                        }).await;
                    }
                    
                    Ok(axum::response::sse::Event::default().data(text))
                }
                Err(e) => {
                    let _ = tx.send(StreamMetrics {
                        task_id,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        tool_calls_count: 0,
                        latency_ms: latency,
                    }).await;
                    Err(std::io::Error::new(std::io::ErrorKind::Other, e))
                }
            }
        }
    });

    let db = state.db.clone();
    tokio::spawn(async move {
        let mut last_prompt = 0;
        let mut last_completion = 0;
        let mut last_tool_calls = 0;

        while let Some(metrics) = rx.recv().await {
            if metrics.prompt_tokens != last_prompt || metrics.completion_tokens != last_completion || metrics.tool_calls_count != last_tool_calls {
                let _ = db.log_metric(
                    metrics.task_id,
                    metrics.prompt_tokens,
                    metrics.completion_tokens,
                    metrics.tool_calls_count,
                    metrics.latency_ms,
                ).await;
                last_prompt = metrics.prompt_tokens;
                last_completion = metrics.completion_tokens;
                last_tool_calls = metrics.tool_calls_count;
            }
        }
    });

    Ok(Sse::new(stream).into_response())
}
