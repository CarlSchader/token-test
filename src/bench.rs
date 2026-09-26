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
    pub ttft: Option<Duration>,
    pub latency: Duration,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub estimated_tokens: bool,
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

    pub fn total_prompt_tokens(&self) -> u64 {
        self.measured().filter(|r| r.ok).map(|r| r.prompt_tokens).sum()
    }

    pub fn aggregate_tokens_per_second(&self) -> f64 {
        let secs = self.wall_time.as_secs_f64();
        if secs <= 0.0 { return 0.0; }
        self.total_completion_tokens() as f64 / secs
    }

    pub fn requests_per_second(&self) -> f64 {
        let secs = self.wall_time.as_secs_f64();
        if secs <= 0.0 { return 0.0; }
        self.success_count() as f64 / secs
    }

    pub fn per_request_rates(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok && r.completion_tokens > 0)
            .filter_map(|r| {
                let ttft = r.ttft.unwrap_or(Duration::ZERO);
                let gen_time = r.latency.saturating_sub(ttft);
                let secs = gen_time.as_secs_f64();
                if secs > 0.001 {
                    Some(r.completion_tokens as f64 / secs)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn ttft_values_ms(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok && r.ttft.is_some())
            .map(|r| r.ttft.unwrap().as_millis() as f64)
            .collect()
    }

    pub fn latency_values_ms(&self) -> Vec<f64> {
        self.measured()
            .filter(|r| r.ok)
            .map(|r| r.latency.as_millis() as f64)
            .collect()
    }
}

/// Run the full benchmark.
pub async fn run_bench(config: &BenchConfig, progress: bool) -> Result<BenchReport> {
    config.validate()?;

    let http = reqwest::Client::builder()
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

        tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let result = execute_request(&http, &config).await;

            if progress && !is_warmup {
                match &result {
                    Ok(c) => {
                        let rate = if c.ttft.is_some() {
                            let gen = c.latency.saturating_sub(c.ttft.unwrap());
                            if gen.as_secs_f64() > 0.001 {
                                c.completion_tokens as f64 / gen.as_secs_f64()
                            } else {
                                0.0
                            }
                        } else {
                            c.completion_tokens as f64 / c.latency.as_secs_f64()
                        };
                        eprintln!(
                            "  [{}] ok  {} tok  {:.1} tok/s  ttft {}  lat {}ms",
                            i,
                            c.completion_tokens,
                            rate,
                            c.ttft.map(|t| format!("{}ms", t.as_millis())).unwrap_or_else(|| "n/a".into()),
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
                    latency: c.latency,
                    prompt_tokens: c.prompt_tokens,
                    completion_tokens: c.completion_tokens,
                    estimated_tokens: c.estimated_tokens,
                },
                Err(e) => RequestResult {
                    index: i as u32,
                    warmup: is_warmup,
                    ok: false,
                    error: Some(e),
                    ttft: None,
                    latency: Duration::ZERO,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    estimated_tokens: false,
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
