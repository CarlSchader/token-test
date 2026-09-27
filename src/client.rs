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
    /// Visible completion tokens (excludes reasoning/thinking tokens).
    pub completion_tokens: u64,
    /// Reasoning/thinking tokens, when the server reports or streams them.
    pub reasoning_tokens: u64,
    /// Time to the first *content* token.
    pub ttft: Option<Duration>,
    /// Time to the first token of *any* kind (content or reasoning). For
    /// thinking models this marks the start of the thinking phase.
    pub first_token: Option<Duration>,
    pub latency: Duration,
    /// True when the response was an SSE stream (false: single JSON body).
    /// Affects how the per-request generation window is computed.
    pub streamed: bool,
    /// True when completion token count was estimated (no authoritative
    /// usage in response).
    pub estimated_tokens: bool,
}

/// Parse an OpenAI-compatible `usage` object.
///
/// Returns `(prompt, completion, reasoning, total, has_completion)`.
/// `completion_tokens` appears in two shapes:
/// - a plain number (classic models); some flat-format servers add a
///   top-level `reasoning_tokens` alongside it;
/// - an object `{"reasoning_tokens": N, "tokens": M}` on thinking models
///   (o1/o3 style), where the total is `reasoning_tokens + tokens`.
fn parse_usage(usage: &serde_json::Value) -> (u64, u64, u64, u64, bool) {
    let prompt = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut completion = 0u64;
    let mut reasoning = 0u64;
    let mut has_completion = false;
    let mut object_form = false;

    if let Some(ct) = usage.get("completion_tokens") {
        match ct {
            serde_json::Value::Number(n) => {
                if let Some(v) = n.as_u64() {
                    completion = v;
                    has_completion = true;
                }
            }
            serde_json::Value::Object(map) => {
                completion = map.get("tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                reasoning = map.get("reasoning_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                has_completion = true;
                object_form = true;
            }
            _ => {}
        }
    }

    // Flat-format servers report reasoning tokens at the top level. Only
    // used when the object form didn't already provide a value (even an
    // explicit 0 counts as "provided").
    if !object_form && reasoning == 0 {
        reasoning = usage.get("reasoning_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    }

    (prompt, completion, reasoning, total, has_completion)
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
/// Token counting, in priority order:
/// 1. `usage.completion_tokens` (final chunk, when
///    `stream_options.include_usage` was requested) — authoritative, in
///    either the number form or the thinking-model object form;
/// 2. `usage.total_tokens - usage.prompt_tokens`, when only `total_tokens`
///    is present;
/// 3. one token per content/reasoning delta (estimate — servers that batch
///    tokens per delta undercount; use `--include-usage` for accurate
///    counts).
async fn stream_completion(
    resp: reqwest::Response,
    start: std::time::Instant,
) -> Result<CompletionResult, String> {
    let mut events = SseStream::new(resp.bytes_stream());

    let mut completion_tokens: u64 = 0;
    let mut reasoning_tokens: u64 = 0;
    let mut prompt_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut has_total = false;
    let mut has_authoritative_completion = false;
    let mut ttft: Option<std::time::Instant> = None;
    let mut first_token: Option<std::time::Instant> = None;

    while let Some(event) = events.next().await {
        let data = event.map_err(|e| e.to_string())?;
        if data == "[DONE]" {
            break;
        }

        let chunk: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| format!("invalid JSON chunk: {e}"))?;

        if let Some(usage) = chunk.get("usage") {
            let (p, c, r, t, has) = parse_usage(usage);
            if has {
                completion_tokens = c;
                reasoning_tokens = r;
                has_authoritative_completion = true;
            }
            if p > 0 {
                prompt_tokens = p;
            }
            if t > 0 {
                total_tokens = t;
                has_total = true;
            }
        }

        // Reasoning/thinking deltas: `delta.reasoning_content`.
        if let Some(delta) = chunk
            .pointer("/choices/0/delta/reasoning_content")
            .and_then(|v| v.as_str())
        {
            if !delta.is_empty() {
                if first_token.is_none() {
                    first_token = Some(std::time::Instant::now());
                }
                if !has_authoritative_completion {
                    reasoning_tokens = reasoning_tokens.saturating_add(1);
                }
            }
        }

        if let Some(delta) = chunk.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
            if !delta.is_empty() {
                if first_token.is_none() {
                    first_token = Some(std::time::Instant::now());
                }
                if ttft.is_none() {
                    ttft = Some(std::time::Instant::now());
                }
                if !has_authoritative_completion {
                    completion_tokens = completion_tokens.saturating_add(1);
                }
            }
        }
    }

    if !has_authoritative_completion && has_total && total_tokens >= prompt_tokens {
        // No `completion_tokens` (number or object): derive the completion
        // count from the authoritative `total_tokens - prompt_tokens`.
        completion_tokens = total_tokens - prompt_tokens;
        has_authoritative_completion = true;
    }

    let latency = start.elapsed();
    Ok(CompletionResult {
        prompt_tokens,
        completion_tokens,
        reasoning_tokens,
        ttft: ttft.map(|t| t - start),
        first_token: first_token.map(|t| t - start),
        latency,
        streamed: true,
        estimated_tokens: !has_authoritative_completion,
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

    let (prompt_tokens, mut completion_tokens, reasoning_tokens, total_tokens, has_completion) =
        match value.get("usage") {
            Some(usage) => parse_usage(usage),
            None => (0, 0, 0, 0, false),
        };

    let mut estimated_tokens = !has_completion;
    if !has_completion && total_tokens >= prompt_tokens {
        completion_tokens = total_tokens - prompt_tokens;
        estimated_tokens = false;
    }

    Ok(CompletionResult {
        prompt_tokens,
        completion_tokens,
        reasoning_tokens,
        // No streamed first token in non-stream mode; whole response is one.
        ttft: Some(latency),
        first_token: Some(latency),
        latency,
        streamed: false,
        estimated_tokens,
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
