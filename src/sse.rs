//! SSE (Server-Sent Events) parsing.
//!
//! Lines starting with `data:` carry payload; multiple `data:` lines in one
//! event are joined with `\n`; a blank line terminates an event. The
//! terminal `data: [DONE]` sentinel ends the stream.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Wraps a byte stream, decoding raw bytes into SSE `data` payloads.
pub struct SseStream<S> {
    inner: S,
    /// Bytes not yet decodable as UTF-8 (partial multi-byte sequence).
    leftover: Vec<u8>,
    /// Partial line waiting for its newline.
    buf: String,
    /// Accumulated `data:` lines for the current event.
    data_lines: Vec<String>,
    /// True once the inner stream is exhausted.
    done: bool,
}

impl<S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin> SseStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            leftover: Vec::new(),
            buf: String::new(),
            data_lines: Vec::new(),
            done: false,
        }
    }
}

impl<S> Stream for SseStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<String, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.done {
                return Poll::Ready(None);
            }

            // Process complete lines already in the buffer.
            loop {
                let pos = match self.buf.find('\n') {
                    Some(p) => p,
                    None => break,
                };
                let line: String = self.buf.drain(..=pos).collect();
                // Strip the trailing \n (always present, since we drained up
                // to it) and any \r before it.
                let line = line.trim_end_matches(|c| c == '\r' || c == '\n');
                if self.on_line(line) {
                    break;
                }
            }

            // If we collected data lines and hit a blank line, emit the event.
            if let Some(payload) = self.take_event() {
                if payload.trim() == "[DONE]" {
                    self.done = true;
                    return Poll::Ready(None);
                }
                return Poll::Ready(Some(Ok(payload)));
            }

            // No complete event yet: pull more bytes.
            match self.inner.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => self.push_bytes(&bytes),
                Poll::Ready(Some(Err(e))) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    // Process any remaining partial line (e.g. "data: x"
                    // without a trailing newline).
                    let remaining = self.buf.drain(..).collect::<String>();
                    let remaining = remaining.trim_end_matches(|c| c == '\r' || c == '\n');
                    if !remaining.is_empty() {
                        self.on_line(remaining);
                    }
                    if let Some(payload) = self.take_event() {
                        return Poll::Ready(Some(Ok(payload)));
                    }
                    return Poll::Ready(None);
                }
            }
        }
    }
}

impl<S> SseStream<S> {
    /// Handle one complete line. Returns true when an event boundary (blank
    /// line) was hit with a pending event.
    fn on_line(&mut self, line: &str) -> bool {
        if line.is_empty() {
            return !self.data_lines.is_empty();
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                self.data_lines.push(data.to_string());
            }
        }
        // Ignore `event:`, `id:`, `retry:`, and comment (`: ...`) lines.
        false
    }

    fn take_event(&mut self) -> Option<String> {
        if self.data_lines.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.data_lines).join("\n"))
        }
    }

    /// Append bytes, retaining any trailing partial UTF-8 sequence.
    fn push_bytes(&mut self, bytes: &[u8]) {
        self.leftover.extend_from_slice(bytes);
        let take = self.leftover.len() - utf8_tail_len(&self.leftover);
        if take > 0 {
            // SSE payloads are ASCII in practice; Utf8Chunks handles
            // malformed input by dropping only the invalid bytes instead
            // of the whole buffer.
            for chunk in self.leftover[..take].utf8_chunks() {
                if !chunk.valid().is_empty() {
                    self.buf.push_str(chunk.valid());
                }
                // Invalid bytes are dropped.
            }
        }
        if take < self.leftover.len() {
            self.leftover = self.leftover.split_off(take);
        } else {
            self.leftover.clear();
        }
        // Safety cap against pathological invalid input.
        if self.leftover.len() > 8 {
            self.leftover = self.leftover.split_off(self.leftover.len() - 8);
        }
    }
}

/// Number of trailing bytes that form an incomplete UTF-8 sequence.
fn utf8_tail_len(b: &[u8]) -> usize {
    for i in (0..b.len()).rev().take(4) {
        let byte = b[i];
        if byte >= 0b1000_0000 && byte <= 0b1011_1111 {
            continue; // continuation byte; sequence started earlier
        }
        let needed = if byte < 0b1100_0000 {
            1
        } else if byte < 0b1110_0000 {
            2
        } else if byte < 0b1111_0000 {
            3
        } else {
            4
        };
        let have = b.len() - i;
        return if have >= needed { 0 } else { have };
    }
    // All four scanned bytes are continuation bytes: a valid UTF-8
    // sequence is at most 4 bytes long, so the entire trailing run of
    // continuation bytes is invalid. Report the whole run so it is held
    // (and eventually dropped by the safety cap in `push_bytes`) rather
    // than fragmented across pushes.
    let mut run = 0;
    for &byte in b.iter().rev() {
        if (0b1000_0000..=0b1011_1111).contains(&byte) {
            run += 1;
        } else {
            break;
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn byte_stream(input: &str) -> impl Stream<Item = Result<Bytes, reqwest::Error>> {
        stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::copy_from_slice(
            input.as_bytes(),
        ))])
    }

    async fn parse(input: &str) -> Vec<String> {
        let mut s = SseStream::new(byte_stream(input));
        let mut out = Vec::new();
        while let Some(Ok(payload)) = s.next().await {
            out.push(payload);
        }
        out
    }

    #[tokio::test]
    async fn parses_single_event() {
        assert_eq!(parse("data: {\"a\":1}\n\n").await, vec![r#"{"a":1}"#]);
    }

    #[tokio::test]
    async fn parses_multi_line_data() {
        assert_eq!(
            parse("data: line1\ndata: line2\n\n").await,
            vec!["line1\nline2"]
        );
    }

    #[tokio::test]
    async fn ignores_comments_and_fields() {
        assert_eq!(
            parse(": keepalive\nevent: x\nid: 5\ndata: hi\n\n").await,
            vec!["hi"]
        );
    }

    #[tokio::test]
    async fn crlf_handling() {
        assert_eq!(parse("data: hello\r\n\r\n").await, vec!["hello"]);
    }

    #[tokio::test]
    async fn flushes_trailing_event_without_final_blank_line() {
        assert_eq!(parse("data: last").await, vec!["last"]);
    }

    #[tokio::test]
    async fn done_terminates_stream() {
        assert_eq!(
            parse("data: a\n\ndata: [DONE]\n\n").await,
            vec!["a"]
        );
    }

    #[tokio::test]
    async fn multiple_events() {
        assert_eq!(
            parse("data: a\n\ndata: b\n\ndata: c\n\n").await,
            vec!["a", "b", "c"]
        );
    }

    #[tokio::test]
    async fn empty_stream() {
        assert_eq!(parse("").await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn malformed_bytes_are_dropped_without_losing_the_payload() {
        // A run of seven dangling continuation bytes (>4) after a valid
        // payload; the next chunk completes the event.
        let s = futures::stream::iter(vec![
            Ok::<_, reqwest::Error>(Bytes::from_static(
                b"data: ok\x80\x80\x80\x80\x80\x80\x80\n",
            )),
            Ok::<_, reqwest::Error>(Bytes::from_static(b"\n")),
        ]);
        let mut st = SseStream::new(s);
        let event = st.next().await.expect("event expected");
        assert_eq!(event.unwrap(), "ok");
        assert!(st.next().await.is_none());
    }
}
