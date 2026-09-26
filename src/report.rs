//! Human-readable and JSON report rendering.

use serde::Serialize;

use crate::bench::{BenchReport, RequestResult};

/// A summary of a finished benchmark, serialized to JSON when requested.
#[derive(Serialize)]
pub struct Report {
    pub model: String,
    pub url: String,
    pub concurrency: usize,
    pub total_requests: u32,
    pub warmup: u32,
    pub success_count: u64,
    pub failure_count: u64,
    pub wall_seconds: f64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub requests_per_second: f64,
    pub tokens_per_second: f64,
    pub per_request_tokens_per_second: Percentiles,
    pub ttft_ms: Percentiles,
    pub latency_ms: Percentiles,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
pub struct Percentiles {
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub min: f64,
    pub max: f64,
}

fn percentiles(values: &[f64]) -> Percentiles {
    use crate::stats::percentile;
    if values.is_empty() {
        return Percentiles {
            p50: 0.0,
            p90: 0.0,
            p99: 0.0,
            min: 0.0,
            max: 0.0,
        };
    }
    Percentiles {
        p50: percentile(values, 50.0).unwrap_or(0.0),
        p90: percentile(values, 90.0).unwrap_or(0.0),
        p99: percentile(values, 99.0).unwrap_or(0.0),
        min: values.iter().cloned().fold(f64::MAX, f64::min),
        max: values.iter().cloned().fold(0.0, f64::max),
    }
}


pub fn build_report(report: &BenchReport) -> Report {
    let success: Vec<&RequestResult> = report.results.iter().filter(|r| r.ok).collect();
    let errors: Vec<String> = report
        .results
        .iter()
        .filter(|r| !r.ok)
        .map(|r| format!("[{}] {}", r.index, r.error.as_deref().unwrap_or("unknown")))
        .collect();

    let wall_secs = report.wall_time.as_secs_f64().max(f64::EPSILON);
    Report {
        model: report.config.model.clone(),
        url: report.config.completions_url(),
        concurrency: report.config.concurrency,
        total_requests: report.config.total_requests,
        warmup: report.config.warmup,
        success_count: success.len() as u64,
        failure_count: report.failure_count() as u64,
        wall_seconds: wall_secs,
        total_prompt_tokens: report.total_prompt_tokens(),
        total_completion_tokens: report.total_completion_tokens(),
        total_tokens: report.total_prompt_tokens() + report.total_completion_tokens(),
        requests_per_second: report.success_count() as f64 / wall_secs,
        tokens_per_second: report.total_completion_tokens() as f64 / wall_secs,
        per_request_tokens_per_second: percentiles(&report.per_request_rates()),
        ttft_ms: percentiles(&report.ttft_values_ms()),
        latency_ms: percentiles(&report.latency_values_ms()),
        errors,
    }
}

/// Pretty-print the report to stdout.
pub fn print_report(report: &BenchReport) {
    let r = build_report(report);

    println!("=== token-test report ===");
    println!("target        : {}", r.url);
    println!("model         : {}", r.model);
    println!("concurrency   : {}", r.concurrency);
    println!(
        "requests      : {} total, {} ok, {} failed ({} warmup excluded)",
        r.total_requests, r.success_count, r.failure_count, r.warmup
    );
    println!("wall clock    : {:.2} s", r.wall_seconds);
    println!();
    println!("throughput");
    println!("  total tokens  : {} (prompt {} + completion {})",
        r.total_tokens, r.total_prompt_tokens, r.total_completion_tokens);
    println!("  requests/s    : {:.2}", r.requests_per_second);
    println!("  tokens/s (out): {:.1}", r.tokens_per_second);
    println!();
    let p = &r.per_request_tokens_per_second;
    println!(
        "per-request tok/s: p50 {:>7.1}  p90 {:>7.1}  p99 {:>7.1}  min {:>7.1}  max {:>7.1}",
        p.p50, p.p90, p.p99, p.min, p.max
    );
    let t = &r.ttft_ms;
    println!(
        "TTFT (ms)       : p50 {:>7.0}  p90 {:>7.0}  p99 {:>7.0}  min {:>7.0}  max {:>7.0}",
        t.p50, t.p90, t.p99, t.min, t.max
    );
    let l = &r.latency_ms;
    println!(
        "total latency   : p50 {:>7}  p90 {:>7}  p99 {:>7}  min {:>7}  max {:>7}",
        fmt_ms(l.p50),
        fmt_ms(l.p90),
        fmt_ms(l.p99),
        fmt_ms(l.min),
        fmt_ms(l.max)
    );

    if !r.errors.is_empty() {
        println!();
        println!("errors (up to 5):");
        for e in r.errors.iter().take(5) {
            println!("  - {}", e.chars().take(160).collect::<String>());
        }
    }
}

fn fmt_ms(v: f64) -> String {
    if v < 1000.0 {
        format!("{:.0} ms", v)
    } else {
        format!("{:.2} s", v / 1000.0)
    }
}

/// Serialize the full report to a pretty JSON string.
pub fn to_json(report: &BenchReport) -> String {
    let r = build_report(report);
    let json = serde_json::to_string_pretty(&r).unwrap_or_default();
    json
}
