//! A minimal OpenAI-compatible mock server for testing token-test locally.
//!
//! Serves:
//!   GET  /v1/models
//!   POST /v1/chat/completions   (streaming SSE + non-streaming JSON)
//!
//! Example:
//!   cargo run --bin token-test-mock -- --port 8901 --tokens 128 --token-ms 20
//!   cargo run --bin token-test-mock -- --tokens 40 --reasoning-tokens 800 --token-ms 1
//!   cargo run --bin token-test -- http://127.0.0.1:8901/v1 -m mock-model --include-usage

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;

#[derive(Parser, Debug, Clone)]
#[command(name = "token-test-mock", about = "Mock OpenAI-compatible LLM server for token-test")]
struct Cli {
    /// Address to bind.
    #[arg(short, long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on.
    #[arg(short, long, default_value_t = 8901)]
    port: u16,

    /// Model name to advertise.
    #[arg(long, default_value = "mock-model")]
    model: String,

    /// Number of visible tokens to generate per request.
    #[arg(long, default_value_t = 128)]
    tokens: u32,

    /// Number of reasoning (thinking) tokens to emit before the visible
    /// tokens, as `delta.reasoning_content` chunks in streaming mode and as
    /// object-form `usage.completion_tokens` in usage. Set to 0 to disable
    /// thinking-model behaviour.
    #[arg(long, default_value_t = 0)]
    reasoning_tokens: u32,

    /// Delay between emitted tokens, in milliseconds.
    #[arg(long, default_value_t = 20)]
    token_ms: u32,

    /// Initial delay before the first token, in milliseconds (simulated TTFT).
    #[arg(long, default_value_t = 50)]
    ttft_ms: u32,

    /// Simulated prompt token count reported in usage.
    #[arg(long, default_value_t = 32)]
    prompt_tokens: u32,

    /// Fail 1 in N requests with a 500 (0 = never fail).
    #[arg(long, default_value_t = 0)]
    fail_every: u32,
}

#[derive(Deserialize)]
struct ChatRequest {
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
}

#[derive(Deserialize)]
struct StreamOptions {
    #[serde(default)]
    include_usage: Option<bool>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let app = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(cli.clone());

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("mock openai-compatible server listening on {addr} (model: {})", cli.model);

    axum::serve(listener, app).await.unwrap();
}

async fn list_models(State(cli): State<Cli>) -> Json<serde_json::Value> {
    Json(json!({
        "object": "list",
        "data": [{ "id": cli.model, "object": "model", "owned_by": "mock" }]
    }))
}

async fn chat_completions(
    State(cli): State<Cli>,
    Json(req): Json<ChatRequest>,
) -> Response {
    // Simple deterministic failure simulation.
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if cli.fail_every > 0 && n % cli.fail_every == 0 {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "message": "simulated server error" } })),
        )
            .into_response();
    }

    if req.stream.unwrap_or(true) {
        let include_usage = req
            .stream_options
            .and_then(|s| s.include_usage)
            .unwrap_or(false);
        stream_response(&cli, include_usage).into_response()
    } else {
        // Simulate generation latency, then return the full JSON response.
        let total = Duration::from_millis(
            cli.ttft_ms as u64 + cli.token_ms as u64 * (cli.tokens + cli.reasoning_tokens) as u64,
        );
        tokio::time::sleep(total).await;
        Json(json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "model": cli.model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "mock completion" },
                "finish_reason": "stop"
            }],
            "usage": usage_json(&cli)
        }))
        .into_response()
    }
}

fn stream_response(cli: &Cli, include_usage: bool) -> Response {
    let total = cli.tokens + cli.reasoning_tokens;
    let reasoning = cli.reasoning_tokens;
    let interval_ms = cli.token_ms.max(1) as u64;
    let ttft_ms = cli.ttft_ms as u64;
    let start = tokio::time::Instant::now();

    // One token per event: reasoning tokens first (the thinking phase),
    // then visible tokens. Token i is due at `start + ttft + i * interval`;
    // sleeping to an absolute deadline keeps the emit rate faithful to
    // --token-ms, where plain per-token sleeps drift by the timer
    // resolution and emit much slower than promised.
    let model = cli.model.clone();
    let chunks = futures::stream::unfold(0u32, move |i: u32| {
        let model = model.clone();
        async move {
            if i >= total {
                return None;
            }
            let due = start + Duration::from_millis(ttft_ms + i as u64 * interval_ms);
            tokio::time::sleep_until(due).await;
            let is_last = i + 1 == total;
            let finish = if is_last { Some("stop") } else { None };
            let (content, reasoning_content) = if i < reasoning {
                (None, Some(format!("r{i}")))
            } else {
                (Some(format!("tok{}", i - reasoning)), None)
            };
            Some((
                Ok::<_, axum::Error>(Event::default().data(chunk_json(
                    &model, content.as_deref(), reasoning_content.as_deref(), finish, None,
                ))),
                i + 1,
            ))
        }
    });

    // Final usage chunk (only when requested via stream_options) + [DONE].
    let tail_model = cli.model.clone();
    let mut tail = vec![
        Ok::<_, axum::Error>(Event::default().data("[DONE]")),
    ];
    if include_usage {
        tail.insert(
            0,
            Ok(Event::default().data(chunk_json(
                &tail_model,
                None,
                None,
                Some("stop"),
                Some(usage_json(cli)),
            ))),
        );
    }

    let stream = chunks.chain(futures::stream::iter(tail));
    axum::response::sse::Sse::new(stream).into_response()
}

/// Usage object as OpenAI reports it: a plain `completion_tokens` number
/// without reasoning, and the thinking-model object form
/// `{"reasoning_tokens": N, "tokens": M}` with it.
fn usage_json(cli: &Cli) -> serde_json::Value {
    let completion = if cli.reasoning_tokens > 0 {
        json!({ "reasoning_tokens": cli.reasoning_tokens, "tokens": cli.tokens })
    } else {
        json!(cli.tokens)
    };
    json!({
        "prompt_tokens": cli.prompt_tokens,
        "completion_tokens": completion,
        "total_tokens": cli.prompt_tokens + cli.tokens + cli.reasoning_tokens
    })
}

/// Build one chat.completion.chunk JSON payload.
fn chunk_json(
    model: &str,
    content: Option<&str>,
    reasoning: Option<&str>,
    finish: Option<&str>,
    usage: Option<serde_json::Value>,
) -> String {
    let mut delta = serde_json::Map::new();
    if let Some(c) = content.filter(|c| !c.is_empty()) {
        delta.insert("content".into(), json!(c));
    }
    if let Some(r) = reasoning.filter(|r| !r.is_empty()) {
        delta.insert("reasoning_content".into(), json!(r));
    }
    if delta.is_empty() {
        delta.insert("content".into(), serde_json::Value::Null);
    }
    let mut obj = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish
        }]
    });
    if let Some(usage) = usage {
        obj["usage"] = usage;
    }
    serde_json::to_string(&obj).unwrap()
}
