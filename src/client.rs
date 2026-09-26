//! HTTP client for OpenAI-compatible endpoints.

use std::time::Duration;

use futures::StreamExt;
use reqwest::Client;

use crate::config::BenchConfig;
use crate::sse::SseStream;

/// Timing and token-accounting results for one completed request.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub ttft: Option<Duration>,
    pub latency: Duration,
    /// True when completion token count was estimated (no usage in response).
    pub estimated_tokens: bool,
}

fn request_body(config: &BenchConfig) -> serde_json::Value {
    let mut messages = Vec::new();
    if let Some(system) = &config.system {
        messages.push(serde_json::json!({ "role": "system", "content": system }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": config.prompt }));

    let mut body = serde_json::json!({
        "model": config.model,
        "messages": messages,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "stream": config.stream,
    });
    if config.stream && config.include_stream_usage {
        body["stream_options"] = serde_json::json!({ "include_usage": true });
    }
    body
}

/// Execute a single chat-completion request and measure it.
pub async fn execute_request(
    client: &Client,
    config: &BenchConfig,
) -> Result<CompletionResult, String> {
    let start = std::time::Instant::now();
    let mut req = client.post(config.completions_url()).json(&request_body(config));
    if let Some(key) = &config.api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, truncate(&body, 300)));
    }

    if config.stream {
        stream_completion(resp, start).await
    } else {
        nonstream_completion(resp, start).await
    }
}

/// Consume an SSE stream, measuring TTFT and token counts.
///
/// Token counting: when the server sends `usage.completion_tokens` (final
/// chunk, when `stream_options.include_usage` was requested), that value is
/// authoritative. Otherwise each content-bearing delta is counted as one
/// token, which matches typical decoder behaviour.
async fn stream_completion(
    resp: reqwest::Response,
    start: std::time::Instant,
) -> Result<CompletionResult, String> {
    let mut events = SseStream::new(resp.bytes_stream());

    let mut completion_tokens: u64 = 0;
    let mut prompt_tokens: u64 = 0;
    let mut ttft: Option<std::time::Instant> = None;
    let mut usage_seen = false;

    while let Some(event) = events.next().await {
        let data = event.map_err(|e| e.to_string())?;
        if data == "[DONE]" {
            break;
        }

        let chunk: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| format!("invalid JSON chunk: {e}"))?;

        if let Some(usage) = chunk.get("usage") {
            if let Some(n) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                completion_tokens = n;
                usage_seen = true;
            }
            if let Some(n) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                prompt_tokens = n;
            }
        }

        if let Some(delta) = chunk.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
            if !delta.is_empty() {
                if ttft.is_none() {
                    ttft = Some(std::time::Instant::now());
                }
                if !usage_seen {
                    completion_tokens = completion_tokens.saturating_add(1);
                }
            }
        }
    }

    let latency = start.elapsed();
    Ok(CompletionResult {
        prompt_tokens,
        completion_tokens,
        ttft: ttft.map(|t| t - start),
        latency,
        estimated_tokens: !usage_seen,
    })
}

/// Handle a non-streaming (single JSON response) completion.
async fn nonstream_completion(
    resp: reqwest::Response,
    start: std::time::Instant,
) -> Result<CompletionResult, String> {
    let latency = start.elapsed();
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid JSON response: {e}"))?;

    let prompt_tokens = value
        .pointer("/usage/prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = value
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    Ok(CompletionResult {
        prompt_tokens,
        completion_tokens,
        // No streamed first token in non-stream mode; whole response is one.
        ttft: Some(latency),
        latency,
        estimated_tokens: prompt_tokens == 0 && completion_tokens == 0,
    })
}

/// List available models from the server.
pub async fn list_models(config: &BenchConfig) -> Result<Vec<String>, String> {
    let mut req = reqwest::Client::new().get(config.models_url());
    if let Some(key) = &config.api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, truncate(&body, 300)));
    }

    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid JSON response: {e}"))?;
    let models = value
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

fn truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}
