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
| `--include-usage` | off | Request authoritative token counts in final chunk |
| `--timeout` | `120` | Per-request timeout (seconds) |
| `--list-models` | — | List models and exit |
| `--json` | off | Output report as JSON |
| `--quiet` | off | Suppress progress lines |

## Metrics

- **tokens/s (out)** — total completion tokens / wall-clock time (aggregate throughput)
- **requests/s** — successful requests / wall-clock time
- **per-request tok/s** — completion_tokens / (latency − TTFT), reported as p50/p90/p99
- **TTFT** — time from request start to first content token
- **total latency** — time from request start to stream end

## Mock server (for local testing)

```sh
# Start the mock
nix run .#mock -- --tokens 128 --token-ms 20
# or: cargo run --release --bin token-test-mock -- --tokens 128 --token-ms 20

# Run the bench against it
nix run . -- http://127.0.0.1:8901/v1 -m mock-model -c 4 -n 10
```

The mock simulates a fixed token rate and supports `--fail-every N` to inject 500s.
