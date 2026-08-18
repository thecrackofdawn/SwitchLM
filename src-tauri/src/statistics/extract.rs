use crate::proxy::dispatch::ClientProtocol;
use axum::body::Bytes;
use futures::{Stream, StreamExt};
use serde_json::Value;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use super::TokenUsage;

/// Extract (input, output) from an already-parsed upstream response body.
/// Pure — no I/O, no stream consumption. Missing fields degrade to (None, None);
/// total-only vendors get (Some(total), Some(0)) (spec §Extraction Implementation).
pub fn extract_tokens_from_json(body: &Value, upstream_protocol: ClientProtocol) -> TokenUsage {
    let usage = &body["usage"];
    let (input, output) = match upstream_protocol {
        ClientProtocol::OpenAI => (usage["prompt_tokens"].as_u64(), usage["completion_tokens"].as_u64()),
        ClientProtocol::Anthropic => (usage["input_tokens"].as_u64(), usage["output_tokens"].as_u64()),
    };
    match (input, output) {
        (i @ Some(_), o @ Some(_)) => (i, o),
        (None, None) => match usage["total_tokens"].as_u64() {
            Some(total) => (Some(total), Some(0)),
            None => (None, None),
        },
        partial => partial,
    }
}

/// Accumulates token usage from upstream SSE chunks. Fed by the forwarding
/// pipeline (tap) — never reads streams itself. `ingest` returns true when
/// both halves are known; further calls are cheap no-ops.
pub struct StreamingTokenCollector {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    upstream_protocol: ClientProtocol,
}

impl StreamingTokenCollector {
    pub fn new(upstream_protocol: ClientProtocol) -> Self {
        Self { input_tokens: None, output_tokens: None, upstream_protocol }
    }

    pub fn ingest(&mut self, chunk: &Value) -> bool {
        match self.upstream_protocol {
            // OpenAI spec puts usage in a final empty-choices chunk, but some
            // non-standard gateways attach it to the last content chunk —
            // accept usage wherever it appears, as long as a number comes out.
            // Intermediate chunks' "usage": null yield None → no false fire.
            ClientProtocol::OpenAI => {
                if let Some(usage) = chunk.get("usage") {
                    let prompt = usage["prompt_tokens"].as_u64();
                    let completion = usage["completion_tokens"].as_u64();
                    if prompt.is_some() || completion.is_some() {
                        self.input_tokens = prompt;
                        self.output_tokens = completion;
                        return true;
                    }
                }
                false
            }
            ClientProtocol::Anthropic => match chunk.get("type").and_then(|t| t.as_str()) {
                Some("message_start") => {
                    self.input_tokens = chunk["message"]["usage"]["input_tokens"].as_u64();
                    false
                }
                Some("message_delta") => {
                    self.output_tokens = chunk["usage"]["output_tokens"].as_u64();
                    self.input_tokens.is_some() // complete when both present
                }
                _ => false,
            },
        }
    }

    pub fn tokens(&self) -> TokenUsage {
        (self.input_tokens, self.output_tokens)
    }
}

/// Inject `stream_options.include_usage = true` into an upstream OpenAI-family
/// streaming request (clients never send it; without it no usage chunk comes
/// back and every stream degrades to RequestsOnly — spec §stream_options
/// injection). Merge semantics: keeps other client-set options; forces
/// include_usage; no-op on a malformed non-object body.
pub fn inject_stream_options(fwd: &mut Value) {
    if let Some(obj) = fwd.as_object_mut() {
        let mut opts = obj
            .get("stream_options")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        opts.insert("include_usage".to_string(), Value::Bool(true));
        obj.insert("stream_options".to_string(), Value::Object(opts));
    }
}

/// Tap for a passthrough (raw-byte) SSE stream: forwards every byte
/// **unchanged** while reassembling complete SSE events (separator `\n\n`)
/// to feed a `StreamingTokenCollector`. Events split across chunks are
/// buffered until complete. The collector's result is delivered through the
/// returned oneshot-like handle when the stream ends (spec §Streaming
/// integration point — tap, never consume).
pub struct SseTap {
    collector: Arc<Mutex<StreamingTokenCollector>>,
}

impl SseTap {
    /// Returns (tap, handle). `handle` — a shared slot — resolves to the
    /// collected tokens once the wrapped stream has ended.
    pub fn new(upstream_protocol: ClientProtocol) -> (Self, SseTapHandle) {
        let collector = Arc::new(Mutex::new(StreamingTokenCollector::new(upstream_protocol)));
        (
            Self { collector: collector.clone() },
            SseTapHandle { collector, finished: Arc::new(Mutex::new(false)) },
        )
    }

    pub fn wrap<S, E>(&self, inner: S) -> TappedStream<S>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
    {
        TappedStream {
            inner,
            buf: Vec::new(),
            collector: self.collector.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SseTapHandle {
    collector: Arc<Mutex<StreamingTokenCollector>>,
    finished: Arc<Mutex<bool>>,
}

impl SseTapHandle {
    /// Mark the stream ended; returns the collected tokens. Idempotent.
    pub fn finish(&self) -> TokenUsage {
        *self.finished.lock().unwrap() = true;
        self.collector.lock().unwrap().tokens()
    }
    pub fn is_finished(&self) -> bool {
        *self.finished.lock().unwrap()
    }
}

/// The wrapping stream: yields the inner bytes untouched, feeds the collector.
pub struct TappedStream<S> {
    inner: S,
    buf: Vec<u8>,
    collector: Arc<Mutex<StreamingTokenCollector>>,
}

impl<S, E> Stream for TappedStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(item)) => {
                if let Ok(bytes) = &item {
                    self.buf.extend_from_slice(bytes);
                    // Feed every COMPLETE event now in the buffer.
                    while let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
                        let event: Vec<u8> = self.buf.drain(..=pos + 1).collect();
                        if let Some(data) = sse_event_data(&event) {
                            if let Ok(chunk) = serde_json::from_str::<Value>(&data) {
                                self.collector.lock().unwrap().ingest(&chunk);
                            }
                        }
                    }
                }
                Poll::Ready(Some(item))
            }
        }
    }
}

/// `data: <payload>` line of one SSE event (ignores comments/event: lines).
fn sse_event_data(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with("data:"))
        .map(|l| l["data:".len()..].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_non_stream_paths() {
        let body = json!({"usage": {"prompt_tokens": 15, "completion_tokens": 30, "total_tokens": 45}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::OpenAI), (Some(15), Some(30)));
    }

    #[test]
    fn anthropic_non_stream_paths() {
        let body = json!({"usage": {"input_tokens": 25, "output_tokens": 50}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::Anthropic), (Some(25), Some(50)));
    }

    #[test]
    fn total_only_vendor_falls_back() {
        // Non-standard vendor: only total_tokens → (Some(total), Some(0)) so the
        // displayed input+output total stays exact (spec §Extraction).
        let body = json!({"usage": {"total_tokens": 100}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::OpenAI), (Some(100), Some(0)));
    }

    #[test]
    fn missing_usage_degrades_to_none() {
        assert_eq!(extract_tokens_from_json(&json!({"choices": []}), ClientProtocol::OpenAI), (None, None));
        assert_eq!(extract_tokens_from_json(&json!({"usage": null}), ClientProtocol::OpenAI), (None, None));
    }

    #[test]
    fn half_present_keeps_half() {
        let body = json!({"usage": {"prompt_tokens": 12}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::OpenAI), (Some(12), None));
    }

    #[test]
    fn openai_stream_usage_chunk_completes() {
        let mut c = StreamingTokenCollector::new(ClientProtocol::OpenAI);
        assert!(!c.ingest(&json!({"choices": [{"delta": {"content": "hi"}}]})));
        assert!(c.ingest(&json!({"choices": [], "usage": {"prompt_tokens": 15, "completion_tokens": 30}})));
        assert_eq!(c.tokens(), (Some(15), Some(30)));
    }

    #[test]
    fn openai_stream_null_usage_does_not_complete() {
        // OpenAI sends "usage": null on every intermediate chunk — must not fire.
        let mut c = StreamingTokenCollector::new(ClientProtocol::OpenAI);
        assert!(!c.ingest(&json!({"choices": [{"delta": {}}], "usage": null})));
        assert_eq!(c.tokens(), (None, None));
    }

    #[test]
    fn openai_stream_nonstandard_gateway_usage_on_content_chunk() {
        // Some gateways attach usage to the last CONTENT chunk (choices non-empty)
        // — accept usage wherever it appears (spec v1.9 #3).
        let mut c = StreamingTokenCollector::new(ClientProtocol::OpenAI);
        assert!(c.ingest(&json!({"choices": [{"delta": {"content": "hi"}}], "usage": {"prompt_tokens": 5, "completion_tokens": 7}})));
        assert_eq!(c.tokens(), (Some(5), Some(7)));
    }

    #[test]
    fn anthropic_stream_events() {
        let mut c = StreamingTokenCollector::new(ClientProtocol::Anthropic);
        assert!(!c.ingest(&json!({"type": "message_start", "message": {"usage": {"input_tokens": 25, "output_tokens": 1}}})));
        assert_eq!(c.tokens(), (Some(25), None));
        assert!(c.ingest(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 50}})));
        assert_eq!(c.tokens(), (Some(25), Some(50)));
    }

    #[test]
    fn anthropic_missing_message_start_does_not_complete() {
        // message_delta without a prior message_start: input unknown → not complete.
        let mut c = StreamingTokenCollector::new(ClientProtocol::Anthropic);
        assert!(!c.ingest(&json!({"type": "message_delta", "usage": {"output_tokens": 50}})));
    }

    #[test]
    fn inject_merges_into_existing_options() {
        let mut fwd = json!({"model": "x", "stream": true, "stream_options": {"other": 1}});
        inject_stream_options(&mut fwd);
        assert_eq!(fwd["stream_options"], json!({"other": 1, "include_usage": true}));
    }

    #[test]
    fn inject_overrides_client_false() {
        // Statistics are proxy-side; a client "false" cannot opt the proxy out.
        let mut fwd = json!({"stream_options": {"include_usage": false}});
        inject_stream_options(&mut fwd);
        assert_eq!(fwd["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn inject_on_missing_key_creates_it() {
        let mut fwd = json!({"model": "x", "stream": true});
        inject_stream_options(&mut fwd);
        assert_eq!(fwd["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn inject_on_non_object_body_is_a_noop() {
        // Defensive: malformed non-object body must not panic (IndexMut would).
        let mut fwd = json!([1, 2, 3]);
        inject_stream_options(&mut fwd);
        assert_eq!(fwd, json!([1, 2, 3]));
    }

    // ---- Task 7: SseTap (byte-verbatim forwarding stream wrapper) ----

    use futures::StreamExt;

    fn byte_stream(
        chunks: Vec<&'static str>,
    ) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
        futures::stream::iter(
            chunks.into_iter().map(|c| Ok(Bytes::from_static(c.as_bytes()))),
        )
    }

    #[tokio::test]
    async fn sse_tap_forwards_bytes_verbatim_and_collects() {
        // Chunks deliberately split mid-event: the tap must reassemble events
        // for parsing while forwarding the ORIGINAL bytes unchanged.
        let chunks = vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"He\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}], \"usage\":",
            " {\"prompt_tokens\": 15, \"completion_tokens\": 30}}\n\n",
            "data: [DONE]\n\n",
        ];
        let expected: Vec<u8> = chunks.concat().into_bytes();
        let (tap, handle) = SseTap::new(ClientProtocol::OpenAI);
        let mut out: Vec<u8> = Vec::new();
        let mut s = tap.wrap(byte_stream(chunks));
        while let Some(item) = s.next().await {
            out.extend_from_slice(&item.unwrap());
        }
        assert_eq!(out, expected); // byte-identical forwarding
        assert_eq!(handle.finish(), (Some(15), Some(30))); // resolves after drain
    }

    #[tokio::test]
    async fn sse_tap_aborted_stream_still_resolves() {
        // Stream ends without usage → handle resolves (None, None), no hang.
        let chunks = vec!["data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n"];
        let (tap, handle) = SseTap::new(ClientProtocol::OpenAI);
        let mut s = tap.wrap(byte_stream(chunks));
        while let Some(item) = s.next().await {
            let _ = item.unwrap();
        }
        assert_eq!(handle.finish(), (None, None));
    }
}
