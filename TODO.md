# token-test — code review findings

Code review for support of **token throughput measurement including thinking/reasoning tokens** under concurrent load.
All file paths are relative to the repo root.

## High priority: thinking tokens are not counted at all

- [x] **Fix `completion_tokens` object parsing (OpenAI o1/o3-style)** — `src/client.rs::stream_completion` reads `usage.completion_tokens` with `.as_u64()` only. For thinking models the final usage chunk carries `completion_tokens` as an object (`{"reasoning_tokens": N, "tokens": M}`), so the authoritative count is silently discarded. Parse both a number and an object form; for the object, sum `reasoning_tokens + tokens` into the total and record reasoning separately.
- [x] **Read top-level `usage.reasoning_tokens`** — flat-format servers report reasoning tokens there alongside `completion_tokens`; currently ignored.
- [x] **Count `delta.reasoning_content` in the no-usage fallback** — reasoning tokens currently only ever arrive as `delta.reasoning_content` when `--include-usage` is off (the default), and are never counted. Count non-empty `reasoning_content` deltas as one token each (consistent with the existing content-delta fallback) when no authoritative usage is present.
- [x] **Add `reasoning_tokens` to `CompletionResult` / `RequestResult`** (`src/client.rs`, `src/bench.rs`) and expose it in the report (JSON + table).
- [x] **Add a `--reasoning-tokens` flag to the mock server** (`src/bin/mock.rs`) — emit `delta.reasoning_content` for N tokens and report the o1-style object-form `usage.completion_tokens`. Needed so the counting math can be verified end-to-end (expected aggregate tokens/s should match the mock's emitted rate).
- [x] **Fix the non-streaming path too** — `src/client.rs::nonstream_completion` has the same number-only `completion_tokens` parse; an object-form response there yields `completion_tokens = 0`.

## High priority: metrics must include the thinking phase

- [x] **Track first token of *any* kind** (`src/client.rs::stream_completion`) — TTFT is currently time-to-first *content* token, so the whole thinking phase is invisible. Record first-reasoning-token time (keep content-TTFT as a second metric if useful).
- [x] **Compute a thinking-inclusive per-request rate** — current `per_request_rates` in `src/bench.rs` uses `completion_tokens / (latency − ttft)`, i.e. visible tokens over a window that starts after thinking ends. For the "including thinking" goal the rate should be `(visible + reasoning) / (stream_end − first_any_token)` or over full `latency`.
- [x] **Report aggregate `tokens/s (incl. reasoning)`** — `(total completion + reasoning) / wall time` in both the table (`src/report.rs::print_report`) and JSON (`Report` struct).

## Medium priority: accuracy/consistency bugs

- [x] **`estimated_tokens` is never surfaced** — `CompletionResult.estimated_tokens` is set (and is `true` for all o1-style requests, since `as_u64()` fails) but the `Report` struct and printed table never show it, so fallback counts look authoritative. Add a per-request or aggregate "estimated" indicator.
- [x] **Use `usage.total_tokens` as fallback** — never read in `src/client.rs`. `total_tokens − prompt_tokens` is a robust completion count when neither `completion_tokens` number nor object is present.
- [x] **Fix warmup inconsistency in `src/report.rs::build_report`** — `success_count`/`failure_count` are computed over *all* results (warmup included) while every rate/token metric uses `BenchReport::measured()` (warmup excluded). The printed "X total, Y ok, Z failed (W warmup excluded)" line mixes the two. Use `report.success_count()`/`failure_count()` (measured) or report both sets explicitly.
- [x] **Wall clock includes warmup** — `tokens_per_second = measured tokens / total wall_time` underestimates steady-state throughput. Consider excluding warmup from the denominator (or reporting both).
- [x] **Client timeout covers the whole stream** — reqwest `Client::timeout` (default 120s) spans the entire body read; a long thinking response fails as a timeout error instead of measuring. Consider a longer default, or splitting into `connect_timeout` + per-read timeout.
- [x] **Note: delta-count fallback assumes 1 token per SSE delta** — batched tokens per delta undercount (documented in the code comment). Flag `--include-usage` as the accurate mode in the README/help when testing thinking models.

## Low priority / optional

- [x] **Mock server** also lacks reasoning in non-stream mode — extend `usage` for the object form so both modes can be verified.
- [x] **`per_request_rates` drops responses with generation time ≤ 1 ms** (filter in `src/bench.rs`) — fine as-is, but document it.
- [x] **`SseStream::utf8_tail_len` edge case** (`src/sse.rs`) — a buffer ending in >4 bytes of dangling continuation bytes is dropped in pieces; only matters for invalid input, SSE JSON is ASCII, so this is theoretical.

## Verification plan (after fixes)

1. Start mock with reasoning: `cargo run --release --bin token-test-mock -- --tokens 40 --reasoning-tokens 800 --token-ms 1`.
2. Bench at high concurrency: `cargo run --release -- http://127.0.0.1:8901/v1 -m mock-model -c 16 -n 20 --include-usage`.
3. Assert aggregate `tokens/s (incl. reasoning)` ≈ (measured requests × 840) / wall time — with `-c 16 -n 20 --warmup 2` that is 18 × 840 ≈ 15120 tokens over ≈ 1.8 s of wall clock ≈ 8.4–8.6 k tok/s (≈ concurrency × the ~840 tok/s per-stream rate; the per-stream rate itself is ~1000 tok/s since the first token is free and 840 tokens stream in 840 ms).
4. Assert per-request `reasoning_tokens` = 800 and thinking-inclusive per-request rate ≈ 840 / (stream end − first reasoning token) ≈ 1000 tok/s. Non-stream requests use the full latency as their generation window (their "first token" *is* the response).
5. Repeat with `--no-stream` and without `--include-usage` to exercise the fallback paths.
6. `cargo test` — existing SSE/stats/config tests should pass unchanged.
