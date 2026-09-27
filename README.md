# token-test

A Rust CLI for load-testing OpenAI-compatible LLM servers. Measures tokens/second, request throughput, TTFT, and latency percentiles under configurable concurrency.

## Build

With Nix (recommended, reproducible):

```sh
nix build                      # builds token-test and token-test-mock
nix run . -- --help            # run without installing
nix develop                    # dev shell with rustc + cargo
```

With cargo:

```sh
cargo build --release
```

## Usage

`nix run .` runs the main CLI directly; `./result/bin/token-test` and `./target/release/token-test` are the built binaries. In the examples below, `./token-test` is the binary from any of the builds.

```sh
# Basic: 4 concurrent requests, 20 total, streaming
nix run . -- http://localhost:8000/v1 -m gpt-4o-mini -c 4 -n 20
./token-test http://localhost:8000/v1 -m gpt-4o-mini -c 4 -n 20

# Non-streaming mode
./token-test http://localhost:8000/v1 -m llama-3 -c 8 -n 50 --no-stream

# Thinking models: request authoritative usage so reasoning tokens are counted
./token-test http://localhost:8000/v1 -m o3 -c 4 -n 20 --include-usage

# Custom prompt, model, and token limit
./token-test http://localhost:8000/v1 \
  -m "llama-3-70b" \
  --prompt "Write a poem" \
  --system "You are a poet." \
  --max-tokens 1024 \
  -c 8 -n 30

# JSON output for scripting
./token-test http://localhost:8000/v1 -m gpt-4o --json -c 2 -n 10

# List available models
./token-test http://localhost:8000/v1 --list-models
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `url` (positional) | required | Base URL, e.g. `http://localhost:8000/v1` |
| `-m, --model` | `gpt-4o-mini` | Model name |
| `--api-key` | `$LLMLOAD_API_KEY` | Bearer token |
| `-c, --concurrency` | `4` | Max concurrent requests |
| `-n, --requests` | `20` | Total requests to issue |
| `--warmup` | `2` | Requests excluded from stats |
| `--prompt` | haiku prompt | User message sent each request |
| `--system` | none | System prompt |
| `--max-tokens` | `256` | `max_tokens` per request |
| `--temperature` | `1.0` | Sampling temperature |
| `--stream` / `--no-stream` | `true` | SSE streaming mode |
| `--include-usage` | off | Request authoritative token counts in the final chunk (recommended for thinking models; without it, counts are estimated as one per SSE delta) |
| `--timeout` | `300` | Per-request timeout (seconds). Spans the entire request including the whole stream, i.e. the full thinking + generation time; raise it for long generations |
| `--connect-timeout` | `10` | TCP/TLS connect timeout (seconds) |
| `--list-models` | — | List models and exit |
| `--json` | off | Output report as JSON |
| `--quiet` | off | Suppress progress lines |

## Metrics

- **tokens/s (out)** — visible completion tokens / wall-clock time
- **tokens/s (incl. reasoning)** — (visible + reasoning tokens) / wall-clock time
- **tokens/s (steady, ...)** — the same ratios over the *measured window* (first measured request start to last measured request end), so warm-up requests don't drag on the denominator
- **requests/s** — successful requests / wall-clock time
- **per-request tok/s** — (visible + reasoning tokens) / (stream end − first token of any kind), reported as p50/p90/p99. Requests with a generation window ≤ 1 ms are dropped (latency resolution dominates sub-millisecond windows).
- **TTFT** — time from request start to the first *content* token
- **first token** — time from request start to the first token of *any* kind (content or reasoning). For thinking models this marks the start of the thinking phase.
- **total latency** — time from request start to stream end

### Token counting

Counts prefer the server's `usage` when present (`--include-usage` in
streaming mode). `usage.completion_tokens` may be a plain number or, on
thinking models, an object `{"reasoning_tokens": N, "tokens": M}`; a
flat top-level `usage.reasoning_tokens` is also read. When `completion_tokens`
is missing but `total_tokens` is present, the completion count is derived as
`total_tokens − prompt_tokens`. Without any usage, each SSE delta (both
`content` and `reasoning_content`) is estimated as one token — servers that
batch several tokens per delta undercount — and the report flags how many
requests were estimated. Use `--include-usage` when testing thinking models
for accurate numbers.

## Mock server (for local testing)

```sh
# Start the mock
nix run .#mock -- --tokens 128 --token-ms 20
# or: cargo run --release --bin token-test-mock -- --tokens 128 --token-ms 20

# Run the bench against it
nix run . -- http://127.0.0.1:8901/v1 -m mock-model -c 4 -n 10
```

The mock simulates a fixed token rate and supports `--fail-every N` to inject
500s.

To simulate a thinking model, add `--reasoning-tokens N`: N reasoning tokens
are streamed as `delta.reasoning_content` before the visible tokens, and the
reported `usage.completion_tokens` uses the thinking-model object form
`{"reasoning_tokens": N, "tokens": M}` (streaming and non-streaming alike):

```sh
nix run .#mock -- --tokens 40 --reasoning-tokens 800 --token-ms 1
```

Note: the mock only sends the usage chunk in streaming mode when the request
sets `stream_options.include_usage`, matching real servers.
