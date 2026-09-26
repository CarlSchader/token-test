//! token-test — an OpenAI-compatible LLM load-testing CLI.
//!
//! Measures tokens/second and request throughput under concurrent load
//! against any OpenAI-compatible `/v1/chat/completions` endpoint.

use std::process;
use std::time::Duration;

use clap::Parser;
use token_test::bench::run_bench;
use token_test::client::list_models;
use token_test::config::BenchConfig;
use token_test::report;

/// Default prompt used when none is supplied.
const DEFAULT_PROMPT: &str = "Write a four-line haiku about the ocean.";

/// Load-test an OpenAI-compatible LLM server.
///
/// Measures throughput (tokens/s), request rate, and latency percentiles
/// under configurable concurrency.
#[derive(Parser, Debug)]
#[command(name = "token-test", version, about)]
struct Cli {
    /// Base URL of the server, e.g. http://localhost:8000/v1
    url: String,

    /// Model name to request.
    #[arg(short, long, default_value = "gpt-4o-mini")]
    model: String,

    /// API key. Falls back to the LLMLOAD_API_KEY environment variable.
    #[arg(long, env = "LLMLOAD_API_KEY")]
    api_key: Option<String>,

    /// Number of concurrent in-flight requests.
    #[arg(short = 'c', long, default_value_t = 4)]
    concurrency: usize,

    /// Total number of requests to issue.
    #[arg(short = 'n', long, default_value_t = 20)]
    requests: u32,

    /// Number of warmup requests to discard from stats.
    #[arg(long, default_value_t = 2)]
    warmup: u32,

    /// User prompt to send with each request.
    #[arg(long, default_value = DEFAULT_PROMPT)]
    prompt: String,

    /// Optional system prompt.
    #[arg(long)]
    system: Option<String>,

    /// Max completion tokens per request.
    #[arg(long, default_value_t = 256)]
    max_tokens: u32,

    /// Sampling temperature.
    #[arg(long, default_value_t = 1.0)]
    temperature: f32,

    /// Use streaming responses (SSE). Needed for real TTFT measurement.
    #[arg(long, default_value_t = true)]
    stream: bool,

    /// Request the server to include usage stats in the final stream chunk.
    #[arg(long)]
    include_usage: bool,

    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 120)]
    timeout: u64,

    /// List available models and exit.
    #[arg(long, conflicts_with = "requests")]
    list_models: bool,

    /// Output the report as JSON instead of a table.
    #[arg(long)]
    json: bool,

    /// Suppress per-request progress lines on stderr.
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() -> process::ExitCode {
    let cli = Cli::parse();

    let config = BenchConfig {
        base_url: cli.url,
        api_key: cli.api_key,
        model: cli.model,
        concurrency: cli.concurrency,
        total_requests: cli.requests,
        warmup: cli.warmup,
        prompt: cli.prompt,
        system: cli.system,
        max_tokens: cli.max_tokens,
        temperature: cli.temperature,
        stream: cli.stream,
        include_stream_usage: cli.include_usage,
        timeout: Duration::from_secs(cli.timeout),
    };

    match config.validate() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {e}");
            return process::ExitCode::FAILURE;
        }
    }

    if cli.list_models {
        match list_models(&config).await {
            Ok(models) => {
                if models.is_empty() {
                    eprintln!("no models returned by server");
                }
                for m in models {
                    println!("{m}");
                }
            }
            Err(e) => {
                eprintln!("error listing models: {e}");
                return process::ExitCode::FAILURE;
            }
        }
        return process::ExitCode::SUCCESS;
    }

    match run_bench(&config, !cli.quiet).await {
        Ok(report) => {
            if cli.json {
                println!("{}", report::to_json(&report));
            } else {
                report::print_report(&report);
            }
            process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            process::ExitCode::FAILURE
        }
    }
}
