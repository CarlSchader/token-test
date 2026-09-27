//! Benchmark configuration.

use std::time::Duration;

/// Configuration for a benchmark run.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Base URL of the OpenAI-compatible server, e.g. `http://localhost:8000/v1`.
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    /// Maximum number of concurrent requests in flight.
    pub concurrency: usize,
    /// Total number of requests to issue.
    pub total_requests: u32,
    /// Leading requests excluded from aggregate stats (warm-up).
    pub warmup: u32,
    pub prompt: String,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Use SSE streaming (required for meaningful TTFT).
    pub stream: bool,
    /// Request `stream_options.include_usage` for authoritative token counts.
    pub include_stream_usage: bool,
    /// Per-request timeout. Spans the entire request, including reading
    /// the whole stream (i.e. the full thinking + generation time).
    pub timeout: Duration,
    /// TCP/TLS connect timeout (the stream read is bounded by `timeout`).
    pub connect_timeout: Duration,
}

impl BenchConfig {
    pub fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.concurrency == 0 {
            anyhow::bail!("concurrency must be at least 1");
        }
        if self.total_requests == 0 {
            anyhow::bail!("total requests must be at least 1");
        }
        if self.warmup >= self.total_requests {
            anyhow::bail!(
                "warmup ({}) must be less than total requests ({})",
                self.warmup,
                self.total_requests
            );
        }
        if self.model.is_empty() {
            anyhow::bail!("model must not be empty");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BenchConfig {
        BenchConfig {
            base_url: "http://localhost:8000/v1".into(),
            api_key: None,
            model: "m".into(),
            concurrency: 4,
            total_requests: 10,
            warmup: 2,
            prompt: "hi".into(),
            system: None,
            max_tokens: 64,
            temperature: 1.0,
            stream: true,
            include_stream_usage: false,
            timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn urls() {
        let c = cfg();
        assert_eq!(c.completions_url(), "http://localhost:8000/v1/chat/completions");
        assert_eq!(c.models_url(), "http://localhost:8000/v1/models");
    }

    #[test]
    fn strips_trailing_slash() {
        let mut c = cfg();
        c.base_url = "http://localhost:8000/v1/".into();
        assert_eq!(c.completions_url(), "http://localhost:8000/v1/chat/completions");
    }

    #[test]
    fn rejects_bad_warmup() {
        let mut c = cfg();
        c.warmup = 10;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_zero_requests() {
        let mut c = cfg();
        c.total_requests = 0;
        assert!(c.validate().is_err());
    }
}
