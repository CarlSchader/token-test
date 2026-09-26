//! A minimal OpenAI-compatible mock server for testing token-test locally.
//!
//! Serves:
//!   GET  /v1/models
//!   POST /v1/chat/completions   (streaming SSE + non-streaming JSON)
//!
//! Example:
//!   cargo run --bin token-test-mock -- --port 8901 --tokens 128 --token-ms 20
//!   cargo run --bin token-test -- http://127.0.0.1:8901/v1 -m mock --include-usage

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

    /// Number of tokens to generate per request.
    #[arg(long, default_value_t = 128)]
    tokens: u32,

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
        stream_response(&cli).into_response()
    } else {
        // Simulate generation latency, then return the full JSON response.
        let total =
            Duration::from_millis(cli.ttft_ms as u64 + cli.token_ms as u64 * cli.tokens as u64);
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
            "usage": {
                "prompt_tokens": cli.prompt_tokens,
                "completion_tokens": cli.tokens,
                "total_tokens": cli.prompt_tokens + cli.tokens
            }
        }))
        .into_response()
    }
}

fn stream_response(cli: &Cli) -> Response {
    let tokens = cli.tokens;
    let ttft = Duration::from_millis(cli.ttft_ms as u64);
    let interval = Duration::from_millis(cli.token_ms.max(1) as u64);
    let prompt_tokens = cli.prompt_tokens;

    // First token arrives after the simulated TTFT.
    let first_model = cli.model.clone();
    let first = futures::stream::once(async move {
        tokio::time::sleep(ttft).await;
        Ok::<_, axum::Error>(Event::default().data(chunk_json(
            &first_model, "The", None, None,
        )))
    });

    // Remaining tokens, one per interval tick, starting at index 1.
    let rest_model = cli.model.clone();
    let rest = futures::stream::unfold(1u32, move |i: u32| {
        let m = rest_model.clone();
        async move {
            if i > tokens.saturating_sub(1) {
                return None;
            }
            tokio::time::sleep(interval).await;
            let is_last = i == tokens.saturating_sub(1);
            let finish = if is_last { Some("stop") } else { None };
            Some((
                Ok::<_, axum::Error>(Event::default().data(chunk_json(
                    &m,
                    &format!("tok{}", i),
                    finish,
                    None,
                ))),
                i + 1,
            ))
        }
    });

    // Final usage chunk + [DONE], as OpenAI-compatible servers do.
    let tail_model = cli.model.clone();
    let tail = futures::stream::iter(vec![
        Ok::<_, axum::Error>(Event::default().data(chunk_json(
            &tail_model,
            "",
            Some("stop"),
            Some(json!({
                "prompt_tokens": prompt_tokens,
                "completion_tokens": tokens,
                "total_tokens": prompt_tokens + tokens
            })),
        ))),
        Ok::<_, axum::Error>(Event::default().data("[DONE]")),
    ]);

    let stream = first.chain(rest).chain(tail);
    axum::response::sse::Sse::new(stream).into_response()
}

/// Build one chat.completion.chunk JSON payload.
fn chunk_json(
    model: &str,
    content: &str,
    finish: Option<&str>,
    usage: Option<serde_json::Value>,
) -> String {
    let mut obj = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "content": if content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content.to_string()) } },
            "finish_reason": finish
        }]
    });
    if let Some(usage) = usage {
        obj["usage"] = usage;
    }
    serde_json::to_string(&obj).unwrap()
}
