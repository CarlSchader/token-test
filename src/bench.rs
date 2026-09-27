use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, Semaphore};

use crate::client::execute_request;
use crate::config::BenchConfig;

/// Result of a single request.
#[derive(Debug, Clone)]
pub struct RequestResult {
    pub index: u32,
    pub warmup: bool,
    pub ok: bool,
    pub error: Option<String>,
    /// Time to the first content token.
    pub ttft: Option<Duration>,
    /// Time to the first token of any kind (content or reasoning).
    pub first_token: Option<Duration>,
    pub latency: Duration,
    /// True when the response was an SSE stream.
    pub streamed: bool,
    pub prompt_tokens: u64,
    /// Visible completion tokens (excludes reasoning).
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub estimated_tokens: bool,
    /// Offset from benchmark start when the request was sent.
    pub started_at: Duration,
    /// Offset from benchmark start when the request finished.
    pub ended_at: Duration,
}

/// Aggregated benchmark report.
pub struct BenchReport {
    pub config: BenchConfig,
    pub results: Vec<RequestResult>,
    pub wall_time: Duration,
}

impl BenchReport {
    /// Results excluding warm-up requests (all statistics are based on these).
    fn measured<'a>(&'a self) -> impl Iterator<Item = &'a RequestResult> + 'a {
        self.results.iter().filter(|r| !r.warmup)
    }

    pub fn success_count(&self) -> usize {
        self.measured().filter(|r| r.ok).count()
    }

    pub fn failure_count(&self) -> usize {
        self.measured().filter(|r| !r.ok).count()
    }

    pub fn total_completion_tokens(&self) -> u64 {
        self.measured().filter(|r| r.ok).map(|r| r.completion_tokens).sum()
    }

    pub fn total_reasoning_tokens(&self) -> u64 {
        self.measured().filter(|r| r.ok).map(|r| r.reasoning_tokens).sum()
    }

    pub fn total_prompt_tokens(&self) -> u64 {
        self.measured().filter(|r| r.ok).map(|r| r.prompt_tokens).sum()
    }

    /// All completion tokens: visible + reasoning.
    pub fn total_all_completion_tokens(&self) -> u64 {
        self.total_completion_tokens() + self.total_reasoning_tokens()
    }

    /// Number of successful measured requests whose token counts are
    /// estimates (the response carried no authoritative usage).
    pub fn estimated_count(&self) -> usize {
        self.measured().filter(|r| r.ok && r.estimated_tokens).count()
    }

    /// Wall-clock window spanning the measured (non-warmup) requests:
    /// earliest measured start to latest measured end. Steady-state rates
    /// use this, so warm-up requests don't drag on the denominator.
    pub fn measured_window(&self) -> Option<Duration> {
        let mut start: Option<Duration> = None;
        let mut end: Option<Duration> = None;
        for r in self.measured() {
            start = Some(start.map_or(r.started_at, |s| s.min(r.started_at)));
            end = Some(end.map_or(r.ended_at, |e| e.max(r.ended_at)));
        }
        match (start, end) {
            (Some(s), Some(e)) if e > s => Some(e - s),
            _ => None,
        }
    }

    /// Visible completion tokens / total wall time.
    pub fn aggregate_tokens_per_second(&self) -> f64 {
        let secs = self.wall_time.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        self.total_completion_tokens() as f64 / secs
    }

    /// (visible + reasoning) completion tokens / total wall time.
    pub fn aggregate_tokens_per_second_incl_reasoning(&self) -> f64 {
        let secs = self.wall_time.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        self.total_all_completion_tokens() as f64 / secs
    }

    /// Visible completion tokens over the measured window (steady state:
    /// warm-up excluded from both numerator and denominator).
    pub fn steady_tokens_per_second(&self) -> f64 {
        self.rate_over_window(|| self.total_completion_tokens())
    }

    /// (visible + reasoning) tokens over the measured window (steady state).
    pub fn steady_tokens_per_second_incl_reasoning(&self) -> f64 {
        self.rate_over_window(|| self.total_all_completion_tokens())
    }

    fn rate_over_window(&self, tokens: impl FnOnce() -> u64) -> f64 {
        match self.measured_window() {
            Some(window) if window.as_secs_f64() > 0.0 => tokens() as f64 / window.as_secs_f64(),
            _ => 0.0,
        }
    }

    pub fn requests_per_second(&self) -> f64 {
        let secs = self.wall_time.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        self.success_count() as f64 / secs
    }

    /// Per-request generation rate, including the thinking phase, in
    /// tok/s. For streamed responses the window is (stream end − first
    /// token of any kind); for non-stream responses the whole response is
    /// the generation, so the full latency is used. Requests whose window
    /// is ≤ 1 ms are dropped: over sub-millisecond windows the latency
    /// resolution noise dominates the ratio, so the value is meaningless.
    pub fn per_request_rates(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok)
            .filter_map(|r| {
                per_request_rate(
                    r.streamed,
                    r.first_token,
                    r.latency,
                    r.completion_tokens + r.reasoning_tokens,
                )
            })
            .collect()
    }

    /// Time to the first content token, in ms.
    pub fn ttft_values_ms(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok && r.ttft.is_some())
            .map(|r| r.ttft.unwrap().as_millis() as f64)
            .collect()
    }

    /// Time to the first token of any kind (content or reasoning), in ms.
    pub fn first_token_values_ms(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok && r.first_token.is_some())
            .map(|r| r.first_token.unwrap().as_millis() as f64)
            .collect()
    }

    pub fn latency_values_ms(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok)
            .map(|r| r.latency.as_millis() as f64)
            .collect()
    }
}

/// Per-request generation rate including reasoning, or `None` when the
/// request produced no tokens or its generation window is too small
/// (< 1 ms) to be meaningful (latency resolution dominates the ratio).
fn per_request_rate(
    streamed: bool,
    first: Option<Duration>,
    latency: Duration,
    tokens: u64,
) -> Option<f64> {
    if tokens == 0 {
        return None;
    }
    // Streamed: window runs from the first token of any kind to stream end.
    // Non-streamed: the whole response *is* the generation (the "first
    // token" arrives at the end), so use the full latency.
    let window = if streamed {
        latency.saturating_sub(first.unwrap_or(Duration::ZERO))
    } else {
        latency
    };
    if window > Duration::from_millis(1) {
        Some(tokens as f64 / window.as_secs_f64())
    } else {
        None
    }
}

/// Run the full benchmark.
pub async fn run_bench(config: &BenchConfig, progress: bool) -> Result<BenchReport> {
    config.validate()?;

    let http = reqwest::Client::builder()
        .connect_timeout(config.connect_timeout)
        // `.timeout()` spans the whole request *including* the full stream
        // read, so it must cover the entire thinking + generation phase.
        .timeout(config.timeout)
        .pool_max_idle_per_host(0)
        .build()?;

    let sem = Arc::new(Semaphore::new(config.concurrency));
    let (tx, mut rx) = mpsc::channel::<RequestResult>(config.concurrency + 1);
    let total = config.total_requests as usize;
    let start = std::time::Instant::now();

    for i in 0..total {
        let http = http.clone();
        let config = config.clone();
        let sem = sem.clone();
        let tx = tx.clone();
        let is_warmup = (i as u32) < config.warmup;
        let bench_start = start;

        tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let started_at = bench_start.elapsed();
            let result = execute_request(&http, &config).await;
            let ended_at = bench_start.elapsed();

            if progress && !is_warmup {
                match &result {
                    Ok(c) => {
                        let tokens = c.completion_tokens + c.reasoning_tokens;
                        let rate = per_request_rate(c.streamed, c.first_token, c.latency, tokens)
                            .unwrap_or(0.0);
                        eprintln!(
                            "  [{}] ok  {}+{} tok  {:.1} tok/s  1st {}  lat {}ms",
                            i,
                            c.completion_tokens,
                            c.reasoning_tokens,
                            rate,
                            c.first_token
                                .map(|t| format!("{}ms", t.as_millis()))
                                .unwrap_or_else(|| "n/a".into()),
                            c.latency.as_millis(),
                        );
                    }
                    Err(e) => {
                        eprintln!("  [{}] FAIL  {}", i, e);
                    }
                }
            }

            let rr = match result {
                Ok(c) => RequestResult {
                    index: i as u32,
                    warmup: is_warmup,
                    ok: true,
                    error: None,
                    ttft: c.ttft,
                    first_token: c.first_token,
                    latency: c.latency,
                    streamed: c.streamed,
                    prompt_tokens: c.prompt_tokens,
                    completion_tokens: c.completion_tokens,
                    reasoning_tokens: c.reasoning_tokens,
                    estimated_tokens: c.estimated_tokens,
                    started_at,
                    ended_at,
                },
                Err(e) => RequestResult {
                    index: i as u32,
                    warmup: is_warmup,
                    ok: false,
                    error: Some(e),
                    ttft: None,
                    first_token: None,
                    latency: Duration::ZERO,
                    streamed: false,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    reasoning_tokens: 0,
                    estimated_tokens: false,
                    started_at,
                    ended_at,
                },
            };

            let _ = tx.send(rr).await;
        });
    }
    drop(tx);

    let mut results = Vec::with_capacity(total);
    while let Some(r) = rx.recv().await {
        results.push(r);
    }
    results.sort_by_key(|r| r.index);

    let wall_time = start.elapsed();

    Ok(BenchReport {
        config: config.clone(),
        results,
        wall_time,
    })
}
