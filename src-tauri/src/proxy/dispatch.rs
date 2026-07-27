use std::collections::HashSet;
use std::error::Error;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::http::Response;
use eventsource_stream::Eventsource;
use futures::StreamExt;

use crate::config::SecretStore;
use crate::proxy::error_adapter::{is_rate_limit_error, sse_event_is_rate_limit};
use crate::proxy::health::{LocalNow, TripReason};
use crate::proxy::resolve::resolve_model;
use crate::proxy::strategies::model_fallback_target;
use crate::proxy::{AppState, ProxyError};
use crate::translate::request::anthropic_to_openai;
use crate::translate::response::openai_to_anthropic;
use crate::usage::{usage_provider_for, UsageSnapshot};

/// Max models tried in a fallback chain (backstop; cycles are caught by the visited set).
const MAX_FALLBACK_HOPS: usize = 8;
/// Default cooldown (secs) when a model has no `cooldown_seconds` and no usage `reset_at`.
const DEFAULT_COOLDOWN_SECS: u64 = 300;

/// Which protocol the inbound client speaks (which edge called `dispatch`).
#[derive(Clone, Copy)]
pub enum ClientProtocol {
    OpenAI,
    Anthropic,
}

/// Result of one upstream attempt. `RateLimited` is only ever returned before any content has
/// been forwarded to the client (non-stream: buffered body; stream: first event).
enum AttemptOutcome {
    /// Forward this response to the client (success or non-rate-limit upstream error passthrough).
    Respond(Response<Body>),
    /// Provider rate-limited / quota exhausted -> trip breaker + walk to fallback.
    RateLimited,
}

/// What a fallback walk produced, for the terminal log line. The walk fills it via `&mut`; always
/// meaningful even when the walk returns `Err`.
struct DispatchOutcome {
    hops: usize,                     // models visited in the chain
    served_model_id: Option<String>, // Some when a model produced a response (success OR a
                                     // passthrough error like 401); None when the walk failed outright.
}

/// Unified request dispatch for both edges. Resolves Profile -> Model, then (non-stream) walks
/// the fallback chain with the circuit breaker, or (stream) does a single attempt (Task 6 adds
/// the stream walk).
pub async fn dispatch(
    state: AppState,
    body: Bytes,
    protocol: ClientProtocol,
) -> Result<Response<Body>, ProxyError> {
    let req: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ProxyError::Translation(format!("invalid json: {e}")))?;

    let requested_model = req.get("model").and_then(|v| v.as_str());
    // One local-time snapshot spans the whole request: the resolve layer (entry strategy) and
    // the fallback walk (per-hop time strategy) must agree on "now" so a window can't flip mid-
    // request. `Clock::now_local` is the single tz-conversion point.
    let now_local = state.clock.now_local();
    let (start_model_id, echo_model, is_stream, vendor, upstream_model) = {
        let cfg = state.config.read().await;
        if cfg.profiles.is_empty() || cfg.models.is_empty() {
            return Err(ProxyError::NotConfigured);
        }
        let model = resolve_model(&cfg, requested_model, &now_local)?;
        // 日志须能脱离本机配置识别厂商与模型：记 vendor + upstream 真实模型名，而非内部
        // 主键 id（m_xxx 在别人机器上无法反查）。vendor 取自模型所属 provider。
        let vendor = cfg
            .providers
            .iter()
            .find(|p| p.id == model.provider_id)
            .map(|p| p.vendor.clone())
            .unwrap_or_default();
        (
            model.id.clone(),
            // Spec §3.4: echo the requested name; backfill upstream_model_id when inbound has none.
            requested_model.map(str::to_string).unwrap_or_else(|| model.upstream_model_id.clone()),
            req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
            vendor,
            model.upstream_model_id.clone(),
        )
    };

    let requested = requested_model.unwrap_or("-").to_string();
    let req_id = fmt_req_id(fastrand::u16(0..=0xffff));
    tracing::info!(
        target: "switchlm::proxy",
        req = %req_id,
        inbound = path_for(protocol),
        requested = %requested,
        vendor = %vendor,
        model = %upstream_model,
        stream = is_stream,
        "forward request",
    );

    let start = Instant::now();
    let mut outcome = DispatchOutcome { hops: 0, served_model_id: None };
    let result = if is_stream {
        dispatch_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id, &mut outcome, &now_local).await
    } else {
        dispatch_non_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id, &mut outcome, &now_local).await
    };

    let ms = start.elapsed().as_millis();
    let served = match &outcome.served_model_id {
        Some(id) => model_tag(&state, id).await,
        None => "-".to_string(),
    };
    match &result {
        Ok(resp) => tracing::info!(
            target: "switchlm::proxy",
            req = %req_id,
            model = %served,
            status = resp.status().as_u16(),
            hops = outcome.hops,
            ms,
            "forward ok"
        ),
        Err(e) => tracing::warn!(
            target: "switchlm::proxy",
            req = %req_id,
            error = %e,
            hops = outcome.hops,
            ms,
            "forward failed"
        ),
    }
    result
}

/// Log one fallback hop: from→to (vendor/model via `model_tag`) + a `reason` string that already
/// encodes the detail (cooling: exhausted/transient + time-to-recover; rate-limit; etc.). `to_id`
/// is `None` when there is no configured fallback target (logged as `to=-`). Tagged with `req` so
/// `grep req=<id>` reconstructs one request's whole chain under concurrency.
async fn log_hop(
    state: &AppState,
    req_id: &str,
    from_id: &str,
    to_id: Option<&str>,
    reason_detail: String,
) {
    let from = model_tag(state, from_id).await;
    let to = match to_id {
        Some(id) => model_tag(state, id).await,
        None => "-".to_string(),
    };
    tracing::info!(
        target: "switchlm::proxy",
        req = %req_id,
        from = %from,
        to = %to,
        reason = %reason_detail,
        "fallback hop"
    );
}

/// Non-stream path: breaker + fallback walk (Task 5).
async fn dispatch_non_stream(
    state: &AppState,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    start_model_id: &str,
    echo_model: &str,
    req_id: &str,
    outcome: &mut DispatchOutcome,
    now_local: &LocalNow,
) -> Result<Response<Body>, ProxyError> {
    let now = state.clock.now_secs();
    let mut visited: HashSet<String> = HashSet::new();
    let mut tried: Vec<String> = Vec::new();
    let mut current = start_model_id.to_string();
    let mut last_error = "no model succeeded".to_string();

    loop {
        if tried.len() >= MAX_FALLBACK_HOPS {
            return Err(ProxyError::FallbackExhausted { tried, last_error });
        }
        if !visited.insert(current.clone()) {
            return Err(ProxyError::FallbackExhausted {
                tried,
                last_error: format!("fallback cycle detected at {current}"),
            });
        }
        tried.push(current.clone());
        outcome.hops = tried.len();
        state.health.recover_if_due(&current, now);

        // Cooling model: bypass without probing (§4.3).
        if state.health.is_cooling(&current, now) {
            let h = state.health.get(&current);
            last_error = "cooling down".to_string();
            let next = next_fallback_id(state, &current, now_local).await;
            log_hop(
                state, req_id, &current, next.as_deref(),
                cooling_reason_detail(h.trip_reason, h.recover_at, now),
            ).await;
            match next {
                Some(next) => { current = next; continue; }
                None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
            }
        }

        let model_snap = match snapshot_model(state, &current).await {
            Some(s) => s,
            None => {
                // Dangling model id -> skip to its fallback.
                last_error = "model missing".to_string();
                let next = next_fallback_id(state, &current, now_local).await;
                log_hop(state, req_id, &current, next.as_deref(), "model missing".to_string()).await;
                match next {
                    Some(next) => { current = next; continue; }
                    None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
                }
            }
        };

        // No matching backend for this protocol (reverse translate deferred) -> skip to fallback.
        if !has_backend(protocol, &model_snap) {
            last_error = "no backend for this protocol".to_string();
            let next = next_fallback_id(state, &current, now_local).await;
            log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
            match next {
                Some(next) => { current = next; continue; }
                None => return Err(ProxyError::NoBackend(current.clone())),
            }
        }

        // Missing key -> unusable, skip to fallback (resilient: a sibling model may have its key).
        let key = match key_for(state, &model_snap.provider_id) {
            Some(k) => k,
            None => {
                last_error = "missing api key".to_string();
                let next = next_fallback_id(state, &current, now_local).await;
                log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
                match next {
                    Some(next) => { current = next; continue; }
                    None => return Err(ProxyError::NoApiKey(model_snap.provider_id.clone())),
                }
            }
        };

        match attempt_with_retry(
            state, &model_snap, req, now, req_id, echo_model, &key,
            AttemptKind::NonStream { protocol },
            model_snap.retry_count, model_snap.retry_delay_secs,
        ).await {
            Ok(AttemptWithRetry::Respond(resp)) => {
                outcome.served_model_id = Some(current.clone());
                return Ok(resp);
            }
            Ok(AttemptWithRetry::RateLimited(rec)) => {
                state.health.trip(&current, rec.recover_at, now, rec.reason);
                last_error = "rate-limited".to_string();
                let next = next_fallback_id(state, &current, now_local).await;
                log_hop(state, req_id, &current, next.as_deref(), ratelimit_reason_detail(rec.reason)).await;
                match next {
                    Some(next) => { current = next; continue; }
                    None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
                }
            }
            // Upstream send/transport error -> respond as 502 (§8: network errors passthrough,
            // no fallback, no trip).
            Err(e) => return Err(e),
        }
    }
}

/// Stream path: breaker + fallback walk. Rate-limit is detected from the upstream HTTP status
/// (all paths), from the buffered body of a non-2xx response (all paths - catches 火山 400
/// InvalidSubscription / 403 AccountOverdueError / 404 ModelNotOpen, which arrive without 429),
/// and, for a 2xx translate stream, from the first SSE event (§3.5 "no content forwarded yet"
/// boundary). A non-2xx response is buffered (its body is the server's error, not a translatable
/// stream), logged with an excerpt, and forwarded raw with the upstream status; a 2xx passthrough
/// stream forwards raw bytes once the status check passes (first-event detection for 2xx
/// passthrough is a parked limitation - would need fragile SSE re-serialization).
async fn dispatch_stream(
    state: &AppState,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    start_model_id: &str,
    echo_model: &str,
    req_id: &str,
    outcome: &mut DispatchOutcome,
    now_local: &LocalNow,
) -> Result<Response<Body>, ProxyError> {
    let now = state.clock.now_secs();
    let mut visited: HashSet<String> = HashSet::new();
    let mut tried: Vec<String> = Vec::new();
    let mut current = start_model_id.to_string();
    let mut last_error = "no model succeeded".to_string();

    macro_rules! hop_and_advance {
        ($detail:expr) => {{
            let next = next_fallback_id(state, &current, now_local).await;
            log_hop(state, req_id, &current, next.as_deref(), $detail).await;
            match next {
                Some(n) => { current = n; continue; }
                None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
            }
        }};
    }

    loop {
        if tried.len() >= MAX_FALLBACK_HOPS {
            return Err(ProxyError::FallbackExhausted { tried, last_error });
        }
        if !visited.insert(current.clone()) {
            return Err(ProxyError::FallbackExhausted {
                tried,
                last_error: format!("cycle at {current}"),
            });
        }
        tried.push(current.clone());
        outcome.hops = tried.len();
        state.health.recover_if_due(&current, now);

        if state.health.is_cooling(&current, now) {
            let h = state.health.get(&current);
            last_error = "cooling down".to_string();
            hop_and_advance!(cooling_reason_detail(h.trip_reason, h.recover_at, now));
        }

        let snap = match snapshot_model(state, &current).await {
            Some(s) => s,
            None => {
                last_error = "model missing".to_string();
                hop_and_advance!("model missing".to_string());
            }
        };
        let path = match stream_path(protocol, &snap) {
            Some(p) => p,
            None => {
                last_error = "no backend for protocol".to_string();
                let next = next_fallback_id(state, &current, now_local).await;
                log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
                match next {
                    Some(n) => { current = n; continue; }
                    None => return Err(ProxyError::NoBackend(current.clone())),
                }
            }
        };
        let key = match key_for(state, &snap.provider_id) {
            Some(k) => k,
            None => {
                last_error = "missing api key".to_string();
                let next = next_fallback_id(state, &current, now_local).await;
                log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
                match next {
                    Some(n) => { current = n; continue; }
                    None => return Err(ProxyError::NoApiKey(snap.provider_id.clone())),
                }
            }
        };

        match attempt_with_retry(
            state, &snap, req, now, req_id, echo_model, &key,
            AttemptKind::Stream { path },
            snap.retry_count, snap.retry_delay_secs,
        ).await {
            Ok(AttemptWithRetry::Respond(resp)) => {
                outcome.served_model_id = Some(current.clone());
                return Ok(resp);
            }
            Ok(AttemptWithRetry::RateLimited(rec)) => {
                state.health.trip(&current, rec.recover_at, now, rec.reason);
                last_error = "rate-limited".to_string();
                hop_and_advance!(ratelimit_reason_detail(rec.reason));
            }
            Err(e) => return Err(e),
        }
    }
}

/// One streaming upstream attempt: send → status-level rate-limit check → non-2xx body
/// buffer+rate-limit check (else forward raw) → 2xx commit (translate first-event / passthrough).
/// Returns `RateLimited` only before any content is forwarded (§3.5 boundary). Transport errors
/// → `Err`. Logging of the non-2xx body stays here; `outcome.served_model_id` is set by the caller.
async fn stream_attempt(
    snap: &ModelSnapshot,
    req: &serde_json::Value,
    path: StreamPath,
    key: &str,
    echo_model: &str,
    req_id: &str,
) -> Result<AttemptOutcome, ProxyError> {
    let resp = send_stream(snap, req, path, key).await.map_err(ProxyError::Upstream)?;
    let vendor = snap.vendor.clone();
    let upstream_status = resp.status();

    // Status-level rate-limit (429 / DeepSeek 402).
    if is_rate_limit_error(&vendor, Some(upstream_status.as_u16()), "") {
        return Ok(AttemptOutcome::RateLimited);
    }

    // Non-2xx: buffer body; catch a body-level rate-limit the status-only check missed
    // (火山 400 InvalidSubscription / 403 AccountOverdueError / 404 ModelNotOpen); else forward raw.
    if !upstream_status.is_success() {
        let url = resp.url().to_string();
        let ct = resp.headers().get("content-type").cloned();
        let bytes = resp.bytes().await.map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;
        if is_rate_limit_error(&vendor, Some(upstream_status.as_u16()), &String::from_utf8_lossy(&bytes)) {
            return Ok(AttemptOutcome::RateLimited);
        }
        tracing::warn!(
            target: "switchlm::proxy", req = %req_id,
            status = upstream_status.as_u16(), vendor = %snap.vendor,
            url = %url, body = %body_excerpt(&bytes), "upstream non-2xx (stream)",
        );
        let mut out = Response::builder().status(upstream_status);
        if let Some(ct) = ct { out = out.header("content-type", ct); }
        return Ok(AttemptOutcome::Respond(out.body(Body::from(bytes)).unwrap()));
    }

    // 2xx: commit.
    match path {
        StreamPath::Translate => match translate_stream_commit(resp.bytes_stream(), echo_model.to_string(), &vendor).await {
            StreamCommit::RateLimited => Ok(AttemptOutcome::RateLimited),
            StreamCommit::Respond(r) => Ok(AttemptOutcome::Respond(r)),
            StreamCommit::TransportErr(e) => Err(ProxyError::Upstream(e)),
        },
        StreamPath::OpenAiPassthrough | StreamPath::AnthropicPassthrough => {
            let status = resp.status();
            let ct = resp.headers().get("content-type").cloned();
            let mut out = Response::builder().status(status);
            if let Some(ct) = ct { out = out.header("content-type", ct); }
            Ok(AttemptOutcome::Respond(out.body(Body::from_stream(resp.bytes_stream())).unwrap()))
        }
    }
}

#[derive(Clone, Copy)]
enum StreamPath {
    OpenAiPassthrough,
    AnthropicPassthrough,
    Translate,
}

/// Outcome of peeking a translate stream's first event.
enum StreamCommit {
    Respond(Response<Body>),
    RateLimited,
    TransportErr(String),
}

fn stream_path(protocol: ClientProtocol, snap: &ModelSnapshot) -> Option<StreamPath> {
    match protocol {
        ClientProtocol::OpenAI => snap.openai_base_url.as_ref().map(|_| StreamPath::OpenAiPassthrough),
        ClientProtocol::Anthropic => {
            if snap.anthropic_base_url.is_some() {
                Some(StreamPath::AnthropicPassthrough)
            } else if snap.openai_base_url.is_some() {
                Some(StreamPath::Translate)
            } else {
                None
            }
        }
    }
}

/// Send a streaming upstream request for the given path; returns the raw response.
async fn send_stream(
    snap: &ModelSnapshot,
    req: &serde_json::Value,
    path: StreamPath,
    key: &str,
) -> Result<reqwest::Response, String> {
    let upstream = snap.upstream_model_id.clone();
    let (body, url): (Vec<u8>, String) = match path {
        StreamPath::OpenAiPassthrough => {
            let mut fwd = req.clone();
            fwd["model"] = serde_json::Value::String(upstream.clone());
            fwd["stream"] = serde_json::Value::Bool(true);
            (serde_json::to_vec(&fwd).unwrap_or_default(), join_url(snap.openai_base_url.as_deref().unwrap(), BackendProtocol::OpenAI))
        }
        StreamPath::AnthropicPassthrough => {
            let mut fwd = req.clone();
            fwd["model"] = serde_json::Value::String(upstream.clone());
            fwd["stream"] = serde_json::Value::Bool(true);
            (serde_json::to_vec(&fwd).unwrap_or_default(), join_url(snap.anthropic_base_url.as_deref().unwrap(), BackendProtocol::Anthropic))
        }
        StreamPath::Translate => {
            let mut oai = anthropic_to_openai(req);
            oai["model"] = serde_json::Value::String(upstream.clone());
            oai["stream"] = serde_json::Value::Bool(true);
            (serde_json::to_vec(&oai).unwrap_or_default(), join_url(snap.openai_base_url.as_deref().unwrap(), BackendProtocol::OpenAI))
        }
    };
    reqwest::Client::new()
        .post(&url)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| fmt_send_error(&e))
}

/// Peek the first upstream SSE event of a translate stream. If it is a rate-limit error (before
/// any content forwarded) -> `RateLimited` (caller trips + falls back). Otherwise commit: feed the
/// first event to the translator, then stream the rest. Realizes §3.5's boundary for the Claude
/// Code (Anthropic -> OpenAI translate) path.
async fn translate_stream_commit(
    upstream: impl futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    echo_model: String,
    vendor: &str,
) -> StreamCommit {
    let mut sse = Box::pin(upstream.eventsource());
    let first = match sse.next().await {
        None => return StreamCommit::Respond(empty_sse_response()),
        Some(Err(e)) => return StreamCommit::TransportErr(e.to_string()),
        Some(Ok(ev)) => ev,
    };

    if sse_event_is_rate_limit(vendor, &first.data) {
        return StreamCommit::RateLimited;
    }

    let mut t = crate::translate::stream::StreamTranslator::new(echo_model);
    let first_frames: Vec<String> = if first.data == "[DONE]" {
        t.ingest(None)
    } else {
        let chunk: serde_json::Value = serde_json::from_str(&first.data).unwrap_or_default();
        t.ingest(Some(&chunk))
    };
    let stream = async_stream::stream! {
        for f in first_frames {
            yield Ok::<_, std::io::Error>(Bytes::from(f));
        }
        while let Some(item) = sse.next().await {
            match item {
                Ok(ev) => {
                    if ev.data == "[DONE]" {
                        for f in t.ingest(None) {
                            yield Ok(Bytes::from(f));
                        }
                        break;
                    }
                    let chunk: serde_json::Value = serde_json::from_str(&ev.data).unwrap_or_default();
                    for f in t.ingest(Some(&chunk)) {
                        yield Ok(Bytes::from(f));
                    }
                }
                Err(e) => {
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
                    break;
                }
            }
        }
    };
    StreamCommit::Respond(
        Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap(),
    )
}

fn empty_sse_response() -> Response<Body> {
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::empty())
        .unwrap()
}

fn has_backend(protocol: ClientProtocol, snap: &ModelSnapshot) -> bool {
    match protocol {
        ClientProtocol::OpenAI => snap.openai_base_url.is_some(),
        ClientProtocol::Anthropic => snap.anthropic_base_url.is_some() || snap.openai_base_url.is_some(),
    }
}

/// Inbound client path (the edge that called `dispatch`) — routing metadata only, for logging.
fn path_for(p: ClientProtocol) -> &'static str {
    match p {
        ClientProtocol::OpenAI => "/v1/chat/completions",
        ClientProtocol::Anthropic => "/v1/messages",
    }
}

/// Select a backend by protocol and call upstream (non-stream). Precondition: `has_backend`.
async fn select_and_call(
    snap: &ModelSnapshot,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    echo_model: &str,
    key: &str,
) -> Result<AttemptOutcome, ProxyError> {
    match protocol {
        ClientProtocol::OpenAI => {
            openai_passthrough(
                snap.openai_base_url.as_deref().unwrap(),
                &snap.upstream_model_id,
                req,
                key,
                echo_model,
                &snap.vendor,
            )
            .await
        }
        ClientProtocol::Anthropic => {
            if let Some(url) = snap.anthropic_base_url.as_deref() {
                anthropic_passthrough(url, &snap.upstream_model_id, req, key, echo_model, &snap.vendor).await
            } else {
                translate_via_openai(
                    snap.openai_base_url.as_deref().unwrap(),
                    &snap.upstream_model_id,
                    req,
                    key,
                    echo_model,
                    &snap.vendor,
                )
                .await
            }
        }
    }
}

/// OpenAI client -> OpenAI backend: near-passthrough (non-stream only; stream path is handled
/// by `dispatch_stream`).
async fn openai_passthrough(
    base_url: &str,
    upstream_model_id: &str,
    req: &serde_json::Value,
    key: &str,
    echo_model: &str,
    vendor: &str,
) -> Result<AttemptOutcome, ProxyError> {
    let mut fwd = req.clone();
    fwd["model"] = serde_json::Value::String(upstream_model_id.to_string());
    let url = join_url(base_url, BackendProtocol::OpenAI);
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&fwd).unwrap_or_default())
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;

    let status = resp.status();
    let ct = resp.headers().get("content-type").cloned();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;
    if is_rate_limit_error(vendor, Some(status.as_u16()), &String::from_utf8_lossy(&bytes)) {
        return Ok(AttemptOutcome::RateLimited);
    }
    if !status.is_success() {
        log_upstream_non_2xx(status, &url, &bytes);
    }

    let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let body = if v.is_object() {
        v["model"] = serde_json::Value::String(echo_model.to_string());
        Body::from(serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec()))
    } else {
        Body::from(bytes)
    };
    let mut out = Response::builder().status(status);
    if let Some(ct) = ct {
        out = out.header("content-type", ct);
    }
    Ok(AttemptOutcome::Respond(
        out.body(body).map_err(|e| ProxyError::Upstream(e.to_string()))?,
    ))
}

/// Anthropic client -> Anthropic backend: same-protocol passthrough (non-stream only).
async fn anthropic_passthrough(
    base_url: &str,
    upstream_model_id: &str,
    req: &serde_json::Value,
    key: &str,
    echo_model: &str,
    vendor: &str,
) -> Result<AttemptOutcome, ProxyError> {
    let mut fwd = req.clone();
    fwd["model"] = serde_json::Value::String(upstream_model_id.to_string());
    let url = join_url(base_url, BackendProtocol::Anthropic);
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&fwd).unwrap_or_default())
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;
    if is_rate_limit_error(vendor, Some(status.as_u16()), &String::from_utf8_lossy(&bytes)) {
        return Ok(AttemptOutcome::RateLimited);
    }
    if !status.is_success() {
        log_upstream_non_2xx(status, &url, &bytes);
    }

    let mut v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| ProxyError::Upstream(e.to_string()))?;
    if v.is_object() {
        v["model"] = serde_json::Value::String(echo_model.to_string());
    }
    Ok(AttemptOutcome::Respond(
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&v).unwrap_or_default()))
            .unwrap(),
    ))
}

/// Anthropic client -> OpenAI backend: translate request/response (non-stream only).
async fn translate_via_openai(
    base_url: &str,
    upstream_model_id: &str,
    req: &serde_json::Value,
    key: &str,
    echo_model: &str,
    vendor: &str,
) -> Result<AttemptOutcome, ProxyError> {
    let mut oai = anthropic_to_openai(req);
    oai["model"] = serde_json::Value::String(upstream_model_id.to_string());
    oai["stream"] = serde_json::Value::Bool(false);
    let url = join_url(base_url, BackendProtocol::OpenAI);
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&oai).unwrap_or_default())
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ProxyError::Upstream(fmt_send_error(&e)))?;
    if is_rate_limit_error(vendor, Some(status.as_u16()), &String::from_utf8_lossy(&bytes)) {
        return Ok(AttemptOutcome::RateLimited);
    }
    if !status.is_success() {
        log_upstream_non_2xx(status, &url, &bytes);
    }

    let oai_resp: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| ProxyError::Upstream(e.to_string()))?;
    let an = openai_to_anthropic(&oai_resp, echo_model);
    Ok(AttemptOutcome::Respond(
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&an).unwrap_or_default()))
            .unwrap(),
    ))
}

// ---- fallback walk helpers ----

/// 4-hex-lowercase per-request tag, e.g. "a3f2".
fn fmt_req_id(n: u16) -> String {
    format!("{:04x}", n)
}

/// Compact human duration for log lines: "4h12m", "5m", "30s", "<1s" for ≤0.
fn human_dur(secs: i64) -> String {
    if secs <= 0 {
        return "<1s".into();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        if m > 0 { format!("{h}h{m}m") } else { format!("{h}h") }
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Hop reason for a *cooling* skip (trip happened on an earlier request): states exhausted vs
/// transient plus a human time-to-recover derived from `recover_at - now`.
fn cooling_reason_detail(reason: TripReason, recover_at: Option<i64>, now: i64) -> String {
    let dur = recover_at
        .map(|r| human_dur(r - now))
        .unwrap_or_else(|| "unknown".to_string());
    match reason {
        TripReason::Exhausted => format!("cooling down (quota exhausted, resets in {dur})"),
        TripReason::Transient => format!("cooling down (transient throttle, retries in {dur})"),
    }
}

/// Hop reason for a *rate-limit* trip (this request just got limited): states exhausted vs transient.
/// The HTTP status is intentionally omitted — 智谱 signals rate limits in a 200 body, so a status
/// would print the confusing "rate-limited (200, …)".
fn ratelimit_reason_detail(reason: TripReason) -> String {
    match reason {
        TripReason::Exhausted => "rate-limited (quota exhausted)".to_string(),
        TripReason::Transient => "rate-limited (transient)".to_string(),
    }
}

struct ModelSnapshot {
    upstream_model_id: String,
    openai_base_url: Option<String>,
    anthropic_base_url: Option<String>,
    provider_id: String,
    vendor: String,
    cooldown_seconds: Option<u64>,
    retry_count: u32,
    retry_delay_secs: u64,
}

async fn snapshot_model(state: &AppState, model_id: &str) -> Option<ModelSnapshot> {
    let cfg = state.config.read().await;
    let m = cfg.models.iter().find(|m| m.id == model_id)?;
    let p = cfg.providers.iter().find(|p| p.id == m.provider_id);
    Some(ModelSnapshot {
        upstream_model_id: m.upstream_model_id.clone(),
        openai_base_url: p.and_then(|p| p.openai_base_url.clone()),
        anthropic_base_url: p.and_then(|p| p.anthropic_base_url.clone()),
        provider_id: m.provider_id.clone(),
        vendor: p.map(|p| p.vendor.clone()).unwrap_or_default(),
        cooldown_seconds: m.cooldown_seconds,
        retry_count: m.retry_count,
        retry_delay_secs: m.retry_delay_secs,
    })
}

/// "{vendor}/{upstream_model_id}" for logging (vendor + upstream name, never the opaque id).
/// A dangling (unresolvable) id degrades to "unknown/{model_id}" so config churn is still
/// traceable; "-" fills an empty vendor/model on a resolved model.
async fn model_tag(state: &AppState, model_id: &str) -> String {
    match snapshot_model(state, model_id).await {
        Some(s) => format!(
            "{}/{}",
            if s.vendor.is_empty() { "-".to_string() } else { s.vendor },
            if s.upstream_model_id.is_empty() { "-".to_string() } else { s.upstream_model_id },
        ),
        None => format!("unknown/{model_id}"),
    }
}

fn key_for(state: &AppState, provider_id: &str) -> Option<String> {
    state.secrets.get_key(provider_id).ok().flatten()
}

/// Time-aware next failover target for `model_id`, if configured and existing. Reads the FAILING
/// model's own `fallback_strategies` (via `model_fallback_target`) so each hop in the chain
/// consults the model that just failed — not the entry model. Master off / no strategy match /
/// strategy model deleted → falls back to `fallback_target_model_id`; no default → `None`.
async fn next_fallback_id(state: &AppState, model_id: &str, now: &LocalNow) -> Option<String> {
    let cfg = state.config.read().await;
    let m = cfg.models.iter().find(|m| m.id == model_id)?;
    let (target, _via) = model_fallback_target(m, &cfg, now);
    let target = target?;
    if cfg.models.iter().any(|x| x.id == target) {
        Some(target.to_string())
    } else {
        None
    }
}

/// Quota utilization at/above which a window counts as exhausted (-> defer recovery to the
/// package reset time). Below this a 429 is treated as a transient throttle and the model
/// retries on the short `cooldown_seconds`.
const EXHAUSTED_PCT: f64 = 100.0;

/// Result of `compute_recover_at`: when to retry, and whether the trip was caused by actual quota
/// exhaustion (→ wait for the package reset) or a transient throttle (→ short cooldown). Both
/// fields are `Copy`, so the recovery info decided on the first rate-limit can be reused across
/// in-place retries without re-querying quota (spec §4.2).
#[derive(Clone, Copy)]
struct Recover {
    recover_at: Option<i64>,
    reason: TripReason,
}

/// `recover_at` for a tripped model + *why*. Queries the provider's quota **in real time**
/// (bypassing the usage cache so the exhaustion check is accurate at trip time): if the quota is
/// actually exhausted (any window at 100%), defer to the package reset time (Exhausted);
/// otherwise this is a transient throttle (quota still available) and the model retries on
/// `cooldown_seconds` (default 300s) (Transient). `recover_at` is always `Some`.
async fn compute_recover_at(state: &AppState, snap: &ModelSnapshot, now: i64) -> Recover {
    if let Some(usage) = usage_snapshot_realtime(state, snap, now).await {
        if let Some(reset) = exhausted_reset_at(&usage) {
            return Recover { recover_at: Some(reset), reason: TripReason::Exhausted };
        }
    }
    let cooldown = snap.cooldown_seconds.unwrap_or(DEFAULT_COOLDOWN_SECS) as i64;
    Recover { recover_at: Some(now + cooldown), reason: TripReason::Transient }
}

/// Which per-path attempt `attempt_with_retry` drives.
#[derive(Clone, Copy)]
enum AttemptKind {
    NonStream { protocol: ClientProtocol },
    Stream { path: StreamPath },
}

/// Result of `attempt_with_retry`: either a response to forward, or a rate-limit that survived
/// all in-place retries, carrying the `Recover` decided once on the first rate-limit (so the
/// caller trips the breaker with it without re-querying quota). Spec §4.2.
enum AttemptWithRetry {
    Respond(Response<Body>),
    RateLimited(Recover),
}

/// One upstream attempt, looping in-place on a pre-content rate-limit when the trip is `Transient`
/// and `retry_count > 0`. Decides Exhausted/Transient ONCE (on the first rate-limit) via
/// `compute_recover_at` and reuses that `Recover` for every subsequent rate-limit and for the
/// final trip — never re-queries quota. `Exhausted` and `retry_count == 0` return `RateLimited`
/// immediately (today's behavior). On recovery the response is served and the breaker is NOT
/// tripped. `tokio::time::sleep` auto-advances under paused test time (no wall-clock delay).
///
/// `recover_at` note (spec §8): `now` is captured once per request (frozen at dispatch start),
/// so the transient `recover_at` from `compute_recover_at` is anchored to the first rate-limit
/// instant T0, not the post-sleep trip instant — the effective breaker cooldown from trip is
/// ≈ `cooldown_seconds − (retry_count × retry_delay_secs)`.
async fn attempt_with_retry(
    state: &AppState,
    snap: &ModelSnapshot,
    req: &serde_json::Value,
    now: i64,
    req_id: &str,
    echo_model: &str,
    key: &str,
    kind: AttemptKind,
    retry_count: u32,
    retry_delay_secs: u64,
) -> Result<AttemptWithRetry, ProxyError> {
    let mut attempt: u32 = 0;
    let mut first_rec: Option<Recover> = None;
    loop {
        let outcome = match kind {
            AttemptKind::NonStream { protocol } => {
                select_and_call(snap, req, protocol, echo_model, key).await?
            }
            AttemptKind::Stream { path } => {
                stream_attempt(snap, req, path, key, echo_model, req_id).await?
            }
        };
        match outcome {
            AttemptOutcome::Respond(r) => return Ok(AttemptWithRetry::Respond(r)),
            AttemptOutcome::RateLimited => {
                // Decide transient vs exhausted exactly once; reuse for every retry + final trip.
                let rec = if attempt == 0 {
                    let r = compute_recover_at(state, snap, now).await;
                    first_rec = Some(r);
                    r
                } else {
                    first_rec.expect("compute_recover_at ran on the first rate-limit")
                };
                if rec.reason == TripReason::Exhausted {
                    return Ok(AttemptWithRetry::RateLimited(rec)); // no retry on real exhaustion
                }
                if attempt >= retry_count {
                    return Ok(AttemptWithRetry::RateLimited(rec)); // retries exhausted → trip + fallback
                }
                tracing::info!(
                    target: "switchlm::proxy", req = %req_id,
                    vendor = %snap.vendor, model = %snap.upstream_model_id,
                    attempt = attempt + 1, max = retry_count,
                    secs = retry_delay_secs, "retry (transient)",
                );
                tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)).await;
                attempt += 1;
            }
        }
    }
}

/// Real-time usage query: bypasses the read cache (a stale "85%" could already be 100% at trip
/// time) but writes the fresh result back for the UI. `None` if the provider has no adapter /
/// base url, or the query fails (caller falls back to `cooldown_seconds`).
async fn usage_snapshot_realtime(state: &AppState, snap: &ModelSnapshot, now: i64) -> Option<UsageSnapshot> {
    let provider = usage_provider_for(&snap.vendor)?;
    let base_url = {
        let cfg = state.config.read().await;
        cfg.providers.iter().find(|p| p.id == snap.provider_id)?.openai_base_url.clone()?
    };
    // AK from config + SK from keyring (the SK is never persisted in config).
    let usage_creds = state.usage_creds(&snap.provider_id).await;
    let key = key_for(state, &snap.provider_id);
    match provider.query(key.as_deref(), usage_creds.as_ref(), &base_url).await {
        Ok(usage) => {
            state.usage_cache.set(&snap.provider_id, usage.clone(), now);
            Some(usage)
        }
        Err(_) => None,
    }
}

/// If the usage snapshot shows the quota is actually exhausted (any window at 100%), return the
/// reset time to wait for; otherwise `None` (= transient throttle, caller uses `cooldown_seconds`).
///
/// Multi-window providers (5h / weekly / monthly) may exhaust a non-primary window while the
/// primary reads healthy, so every tier is scanned. The reset time is the latest `reset_at`
/// among exhausted windows (the binding constraint - all exhausted windows must reset before
/// requests succeed again). Single-aggregate providers defer to the primary `reset_at`.
fn exhausted_reset_at(usage: &UsageSnapshot) -> Option<i64> {
    let exhausted = |used_pct: Option<f64>| used_pct.map(|u| u >= EXHAUSTED_PCT).unwrap_or(false);
    if !usage.tiers.is_empty() {
        return usage
            .tiers
            .iter()
            .filter(|t| exhausted(t.used_pct))
            .filter_map(|t| t.reset_at)
            .max();
    }
    if exhausted(usage.used) {
        return usage.reset_at;
    }
    None
}

#[derive(Clone, Copy)]
enum BackendProtocol {
    OpenAI,
    Anthropic,
}

/// OpenAI backend -> `/chat/completions`; Anthropic backend -> `/v1/messages`.
fn join_url(base_url: &str, protocol: BackendProtocol) -> String {
    let base = base_url.trim_end_matches('/');
    match protocol {
        BackendProtocol::OpenAI => format!("{base}/chat/completions"),
        BackendProtocol::Anthropic => format!("{base}/v1/messages"),
    }
}

/// Format a reqwest error with its category + source chain so the failure log shows the *cause*
/// (e.g. "timeout error for <url>: operation timed out"), not reqwest's opaque
/// "error sending request for url (<url>)" which hides whether it was a timeout, DNS, connect
/// reset, etc. Used at every upstream send/bytes error site; the resulting string flows into
/// `ProxyError::Upstream`, which is logged with `req=<id>` at the dispatch boundary.
fn fmt_send_error(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_body() {
        "body read"
    } else if e.is_decode() {
        "decode"
    } else if e.is_request() {
        "request"
    } else {
        "upstream"
    };
    let url = e.url().map(|u| u.as_str()).unwrap_or("-");
    // Walk the source chain (reqwest -> hyper -> io) to surface the real cause (e.g.
    // "operation timed out", "Connection refused", "dns error") instead of reqwest's
    // opaque "error sending request for url".
    let mut cause: Vec<String> = Vec::new();
    let mut cur: Option<&(dyn Error + 'static)> = e.source();
    while let Some(src) = cur {
        let s = src.to_string();
        if !s.is_empty() {
            cause.push(s);
        }
        cur = src.source();
    }
    if cause.is_empty() {
        format!("{kind} error for {url}")
    } else {
        format!("{kind} error for {url}: {}", cause.join(" · "))
    }
}

/// First ~1 KB of an upstream response body (lossy UTF-8) for error logs - enough to see the
/// vendor's error message without dumping a huge body. The response never carries the api key
/// (that's a request header), so this is safe to log.
fn body_excerpt(bytes: &[u8]) -> String {
    const MAX: usize = 1024;
    let mut s = String::from_utf8_lossy(if bytes.len() > MAX {
        &bytes[..MAX]
    } else {
        bytes
    })
    .into_owned();
    if bytes.len() > MAX {
        s.push('…');
    }
    s
}

/// Log an upstream non-2xx (non-rate-limit) response: status + url + body excerpt. Rate-limits
/// are excluded (they trip the breaker and get their own fallback-hop log). The companion
/// `forward ok req=<id> status=<n>` line carries the request id for correlation.
fn log_upstream_non_2xx(status: reqwest::StatusCode, url: &str, body: &[u8]) {
    tracing::warn!(
        target: "switchlm::proxy",
        status = status.as_u16(),
        url = %url,
        body = %body_excerpt(body),
        "upstream non-2xx response",
    );
}

#[cfg(test)]
mod tests {
    use crate::config::*;
    use crate::proxy::health::{FakeClock, TripReason};
    use crate::proxy::server::build_router;
    use crate::proxy::{AppState, AppStateInner, Clock};
    use crate::usage::{UsageSnapshot, UsageTier};
    use super::exhausted_reset_at;
    use super::snapshot_model;
    use super::{body_excerpt, fmt_req_id, human_dur, cooling_reason_detail, ratelimit_reason_detail, model_tag};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mk_model(id: &str, provider_id: &str, fb: Option<&str>) -> Model {
        Model {
            id: id.into(),
            provider_id: provider_id.into(),
            source: ModelSource::Manual,
            upstream_model_id: id.into(),
            cooldown_seconds: Some(300),
            retry_count: 0, // retry opt-out for the fallback-chain suite (spec §7)
            retry_delay_secs: 5,
            fallback_target_model_id: fb.map(String::from),
            ..Default::default()
        }
    }

    /// Build state with a chain m_a -> m_b? -> m_c?, profile "glm-5.2" -> m_a, all on provider
    /// "zhipu" (key "sk-test"). `FakeClock` fixed at epoch 1000.
    async fn chain_state(
        clock: Arc<dyn Clock>,
        a: &str,
        b: Option<&str>,
        c: Option<&str>,
    ) -> AppState {
        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        // Each model lives on its own provider so the fallback chain can target distinct
        // upstreams (base_url is now per-provider, not per-model).
        cfg.providers.push(Provider {
            id: "zhipu".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
            openai_base_url: Some(a.into()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "zhipu", b.map(|_| "m_b")));
        secrets.set_key("zhipu", "sk-test").unwrap();
        if let Some(bu) = b {
            cfg.providers.push(Provider {
                id: "pb".into(), vendor: "pb".into(), display_name: "pb".into(),
                openai_base_url: Some(bu.into()), anthropic_base_url: None, usage_creds: None,
            });
            cfg.models.push(mk_model("m_b", "pb", c.map(|_| "m_c")));
            secrets.set_key("pb", "sk-test").unwrap();
        }
        if let Some(cu) = c {
            cfg.providers.push(Provider {
                id: "pc".into(), vendor: "pc".into(), display_name: "pc".into(),
                openai_base_url: Some(cu.into()), anthropic_base_url: None, usage_creds: None,
            });
            cfg.models.push(mk_model("m_c", "pc", None));
            secrets.set_key("pc", "sk-test").unwrap();
        }
        cfg.profiles.push(Profile {
            id: "p".into(),
            name: "glm-5.2".into(),
            aliases: vec![],
            backing_model_id: "m_a".into(),
            ..Default::default()
        });
        Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: Default::default(),
            secrets,
            health: Default::default(),
            clock,
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn snapshot_model_carries_vendor_distinct_from_id() {
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1000));
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "prov_abc".into(), vendor: "zhipu".into(), display_name: "工作号".into(),
            openai_base_url: Some("https://x/v1".into()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "prov_abc", None));
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: Default::default(),
            secrets: SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring),
            health: Default::default(),
            clock,
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });
        let snap = snapshot_model(&state, "m_a").await.unwrap();
        assert_eq!(snap.provider_id, "prov_abc"); // opaque key preserved
        assert_eq!(snap.vendor, "zhipu");         // routing key carried
    }

    #[tokio::test]
    async fn model_tag_formats_vendor_model_and_unknown_for_dangling() {
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1000));
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "prov".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
            openai_base_url: Some("https://x/v1".into()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "prov", None));
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: Default::default(),
            secrets: SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring),
            health: Default::default(),
            clock,
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });
        assert_eq!(model_tag(&state, "m_a").await, "zhipu/m_a");
        assert_eq!(model_tag(&state, "m_missing").await, "unknown/m_missing");
    }

    fn oai_post() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"model":"glm-5.2","messages":[{"role":"user","content":"hi"}]})
                    .to_string(),
            ))
            .unwrap()
    }

    async fn body_str(resp: axum::http::Response<Body>) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn fallback_on_rate_limit() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}]}),
            ))
            .mount(&mock_b).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-b"));
        assert!(state.health.is_cooling("m_a", 1000)); // tripped
        assert!(!state.health.is_cooling("m_b", 1000)); // served, healthy
    }

    #[tokio::test]
    async fn all_rate_limit_exhausted() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        let mock_c = MockServer::start().await;
        for m in [&mock_a, &mock_b, &mock_c] {
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"message":"rate limited"}})))
                .mount(m).await;
        }
        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), Some(&mock_c.uri())).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY); // FallbackExhausted -> 502
        assert!(state.health.is_cooling("m_a", 1000));
        assert!(state.health.is_cooling("m_b", 1000));
        assert!(state.health.is_cooling("m_c", 1000));
    }

    #[tokio::test]
    async fn non_rate_limit_error_passthrough() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"unauthorized"})))
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x"})))
            .mount(&mock_b).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED); // forwarded, no fallback
        assert!(!state.health.is_cooling("m_a", 1000)); // not tripped
        assert_eq!(mock_b.received_requests().await.unwrap().len(), 0); // b not tried
    }

    #[tokio::test]
    async fn deepseek_402_falls_back() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(402).set_body_json(serde_json::json!({"error":{"message":"Insufficient balance"}})))
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}]}),
            ))
            .mount(&mock_b).await;

        // Custom state: provider "pa" with vendor "deepseek", fallback chain m_a → m_b.
        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider {
            id: "pa".into(),
            vendor: "deepseek".into(),
            display_name: "DeepSeek".into(),
            openai_base_url: Some(mock_a.uri()),
            anthropic_base_url: None,
            usage_creds: None,
        });
        cfg.providers.push(Provider {
            id: "pb".into(),
            vendor: "pb".into(),
            display_name: "pb".into(),
            openai_base_url: Some(mock_b.uri()),
            anthropic_base_url: None,
            usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "pa", Some("m_b")));
        cfg.models.push(mk_model("m_b", "pb", None));
        cfg.profiles.push(Profile {
            id: "p".into(),
            name: "glm-5.2".into(),
            aliases: vec![],
            backing_model_id: "m_a".into(),
            ..Default::default()
        });
        secrets.set_key("pa", "sk-test").unwrap();
        secrets.set_key("pb", "sk-test").unwrap();
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            secrets,
            catalog: Default::default(),
            health: Default::default(),
            clock: Arc::new(FakeClock::new(1000)),
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });

        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-b")); // served by fallback
        assert!(state.health.is_cooling("m_a", 1000)); // tripped
        assert!(!state.health.is_cooling("m_b", 1000)); // fallback healthy
    }

    #[tokio::test]
    async fn cooling_model_skipped_to_fallback() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-a"}}]})))
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}]})))
            .mount(&mock_b).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
        state.health.trip("m_a", Some(2000), 1000, TripReason::Transient); // pre-trip a
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_str(resp).await;
        assert!(body.contains("from-b"));
        assert!(!body.contains("from-a"));
        assert_eq!(mock_a.received_requests().await.unwrap().len(), 0); // a bypassed, no probe
        assert_eq!(mock_b.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fallback_cycle_breaks() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_b).await;

        // m_a -> m_b -> m_a (cycle)
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider { id: "pa".into(), vendor: "pa".into(), display_name: "pa".into(), openai_base_url: Some(mock_a.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pb".into(), vendor: "pb".into(), display_name: "pb".into(), openai_base_url: Some(mock_b.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.models.push(mk_model("m_a", "pa", Some("m_b")));
        cfg.models.push(mk_model("m_b", "pb", Some("m_a")));
        cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_a".into(), ..Default::default() });
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        secrets.set_key("pa", "sk-test").unwrap();
        secrets.set_key("pb", "sk-test").unwrap();
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg), secrets,
            catalog: Default::default(),
            health: Default::default(), clock: Arc::new(FakeClock::new(1000)), usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });

        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY); // cycle -> FallbackExhausted
        assert!(state.health.is_cooling("m_a", 1000));
        assert!(state.health.is_cooling("m_b", 1000));
    }

    fn anthropic_stream_post() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"model":"glm-5.2","stream":true,"max_tokens":16,"messages":[{"role":"user","content":"hi"}]})
                    .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn stream_first_event_rate_limit_falls_back() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        // A: first (and only) SSE event is a 智谱 rate-limit error (HTTP 200, error in event).
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(b"data: {\"error\":{\"code\":1302}}\n\n".to_vec()),
            )
            .mount(&mock_a).await;
        // B: normal text stream.
        let sse_b = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                     data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n\
                     data: [DONE]\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(sse_b.as_bytes().to_vec()),
            )
            .mount(&mock_b).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(anthropic_stream_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_str(resp).await;
        assert!(body.contains("\"text\":\"Hel\"")); // served by B
        assert!(body.contains("\"text\":\"lo\""));
        assert!(state.health.is_cooling("m_a", 1000)); // A tripped on first-event rate-limit
    }

    #[tokio::test]
    async fn stream_content_then_error_no_retry() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        // A: first event is real content (commit), then a mid-stream error event -> forwarded.
        let sse = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
                   data: {\"error\":{\"code\":1214}}\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(sse.as_bytes().to_vec()),
            )
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x"})))
            .mount(&mock_b).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(anthropic_stream_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK); // committed
        assert!(body_str(resp).await.contains("\"text\":\"hi\"")); // content forwarded
        assert!(!state.health.is_cooling("m_a", 1000)); // not tripped (content committed)
        assert_eq!(mock_b.received_requests().await.unwrap().len(), 0); // B not tried
    }

    #[tokio::test]
    async fn stream_non_2xx_rate_limit_body_trips_and_falls_back() {
        // 火山 coding plan returns 400 InvalidSubscription (NOT 429) when the plan is expired /
        // not subscribed. On the stream path the status-only check (429 / DeepSeek 402) misses it;
        // the buffered body must catch it -> trip + fallback. Previously the body was never inspected
        // on the stream path, so this passed through / got swallowed without tripping the breaker.
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({"error":{"code":"InvalidSubscription","message":"plan expired"}}),
            ))
            .mount(&mock_a).await;
        let sse_b = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                     data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n\
                     data: [DONE]\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(sse_b.as_bytes().to_vec()),
            )
            .mount(&mock_b).await;

        // volcengine-coding provider; no usage_creds -> realtime usage query returns Err immediately
        // (NotConfigured) -> compute_recover_at falls back to the transient cooldown. No hang.
        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider {
            id: "pa".into(), vendor: "volcengine-coding".into(), display_name: "火山".into(),
            openai_base_url: Some(mock_a.uri()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.providers.push(Provider {
            id: "pb".into(), vendor: "pb".into(), display_name: "pb".into(),
            openai_base_url: Some(mock_b.uri()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "pa", Some("m_b")));
        cfg.models.push(mk_model("m_b", "pb", None));
        cfg.profiles.push(Profile {
            id: "p".into(), name: "glm-5.2".into(), aliases: vec![],
            backing_model_id: "m_a".into(), ..Default::default()
        });
        secrets.set_key("pa", "sk-test").unwrap();
        secrets.set_key("pb", "sk-test").unwrap();
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg), secrets,
            catalog: Default::default(),
            health: Default::default(), clock: Arc::new(FakeClock::new(1000)), usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });

        let app = build_router(state.clone());
        let resp = app.oneshot(anthropic_stream_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_str(resp).await;
        assert!(body.contains("\"text\":\"Hel\"")); // served by B (translated)
        assert!(body.contains("\"text\":\"lo\""));
        assert!(state.health.is_cooling("m_a", 1000)); // A tripped on body-level rate-limit
        assert!(!state.health.is_cooling("m_b", 1000));
    }

    #[tokio::test]
    async fn stream_non_2xx_non_rate_limit_body_forwarded() {
        // A non-rate-limit upstream error (401) on the stream path: the body is buffered, logged
        // with an excerpt, and forwarded to the client with the upstream status - not swallowed
        // into a 200/502 (the old translate path hard-coded status 200 and dropped the body).
        let mock_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"error":{"code":"AuthenticationError","message":"invalid api key"}}),
            ))
            .mount(&mock_a).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(anthropic_stream_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED); // forwarded, not 200/502
        let body = body_str(resp).await;
        assert!(body.contains("invalid api key")); // upstream error body preserved (and logged)
        assert!(!state.health.is_cooling("m_a", 1000)); // not tripped (non-rate-limit)
    }

    // ---- compute_recover_at: quota-exhausted vs transient-throttle ----

    fn snap_used(used: Option<f64>, reset_at: Option<i64>) -> UsageSnapshot {
        UsageSnapshot {
            used,
            total: used.map(|_| 100.0),
            remaining: used.map(|u| (100.0 - u).max(0.0)),
            reset_at,
            unit: "%".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "plan".into(),
            plan_info: None,
        }
    }

    fn snap_tiers(tiers: Vec<(Option<f64>, Option<i64>)>) -> UsageSnapshot {
        UsageSnapshot {
            used: None,
            total: None,
            remaining: None,
            reset_at: None,
            unit: "%".into(),
            raw_summary: None,
            plan: None,
            tiers: tiers
                .into_iter()
                .map(|(used_pct, reset_at)| UsageTier { window: "x".into(), used_pct, reset_at })
                .collect(),
            billing_model: "plan".into(),
            plan_info: None,
        }
    }

    #[test]
    fn exhausted_reset_at_single_aggregate_exhausted() {
        let usage = snap_used(Some(100.0), Some(1_700_000_000));
        assert_eq!(exhausted_reset_at(&usage), Some(1_700_000_000));
    }

    #[test]
    fn exhausted_reset_at_single_aggregate_available() {
        // 85% used -> not exhausted -> None (transient throttle).
        assert_eq!(exhausted_reset_at(&snap_used(Some(85.0), Some(1_700_000_000))), None);
        // Unknown usage -> can't confirm exhaustion -> None.
        assert_eq!(exhausted_reset_at(&snap_used(None, Some(1_700_000_000))), None);
    }

    #[test]
    fn exhausted_reset_at_multi_window_non_primary_exhausted() {
        // 5h healthy (85%), weekly exhausted (100%) -> wait for the weekly reset (the later one).
        let usage = snap_tiers(vec![
            (Some(85.0), Some(1_700_000_000)),
            (Some(100.0), Some(1_800_000_000)),
        ]);
        assert_eq!(exhausted_reset_at(&usage), Some(1_800_000_000));
    }

    #[test]
    fn exhausted_reset_at_multi_window_none_exhausted() {
        let usage = snap_tiers(vec![
            (Some(85.0), Some(1_700_000_000)),
            (Some(50.0), Some(1_800_000_000)),
        ]);
        assert_eq!(exhausted_reset_at(&usage), None);
    }

    #[tokio::test]
    async fn rate_limit_with_exhausted_quota_uses_reset_at() {
        let mock_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        // Quota: 100% exhausted, reset far in the future.
        Mock::given(method("GET"))
            .and(path("/api/monitor/usage/quota/limit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":100,"nextResetTime":"2023-11-14T22:13:20Z"}]}
            })))
            .mount(&mock_a).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY); // no fallback -> FallbackExhausted
        // Exhausted -> recover_at = package reset (1_700_000_000), NOT now+cooldown (1300).
        assert_eq!(state.health.get("m_a").recover_at, Some(1_700_000_000));
        assert_eq!(state.health.get("m_a").trip_reason, TripReason::Exhausted);
    }

    #[tokio::test]
    async fn rate_limit_with_available_quota_uses_cooldown() {
        let mock_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        // Quota: 85% used (available), reset far in the future -- must be IGNORED.
        Mock::given(method("GET"))
            .and(path("/api/monitor/usage/quota/limit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":85,"nextResetTime":"2023-11-14T22:13:20Z"}]}
            })))
            .mount(&mock_a).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        // Available quota -> transient throttle -> recover_at = now + cooldown (1300).
        assert_eq!(state.health.get("m_a").recover_at, Some(1300));
        assert_eq!(state.health.get("m_a").trip_reason, TripReason::Transient);
    }

    // ---- Task 2: pure log-string helpers ----

    #[test]
    fn fmt_req_id_formats_4_hex_lower() {
        assert_eq!(fmt_req_id(0), "0000");
        assert_eq!(fmt_req_id(0xa3f2), "a3f2");
        assert_eq!(fmt_req_id(0xffff), "ffff");
    }

    #[test]
    fn human_dur_cases() {
        assert_eq!(human_dur(0), "<1s");      // exactly at/over the boundary
        assert_eq!(human_dur(-1), "<1s");     // recover expired but not yet cleared
        assert_eq!(human_dur(30), "30s");
        assert_eq!(human_dur(300), "5m");
        assert_eq!(human_dur(900), "15m");
        assert_eq!(human_dur(3600), "1h");
        assert_eq!(human_dur(15120), "4h12m");
    }

    #[test]
    fn cooling_reason_detail_variants() {
        assert_eq!(
            cooling_reason_detail(TripReason::Exhausted, Some(2000), 1000),
            "cooling down (quota exhausted, resets in 16m)"
        );
        assert_eq!(
            cooling_reason_detail(TripReason::Transient, Some(1300), 1000),
            "cooling down (transient throttle, retries in 5m)"
        );
        assert_eq!(
            cooling_reason_detail(TripReason::Exhausted, None, 1000),
            "cooling down (quota exhausted, resets in unknown)"
        );
    }

    #[test]
    fn ratelimit_reason_detail_variants() {
        assert_eq!(ratelimit_reason_detail(TripReason::Exhausted), "rate-limited (quota exhausted)");
        assert_eq!(ratelimit_reason_detail(TripReason::Transient), "rate-limited (transient)");
    }

    // ---- Task 3: transient rate-limit in-place retry (attempt_with_retry) ----

    // NOTE on wiremock 0.6 sequential responses: mocks are matched in mount order (stable sort by
    // priority, all default 5 → the FIRST-mounted matching mock wins; a mock exhausted via
    // `up_to_n_times` stops matching and the next-mounted one takes over). So the transient
    // response is mounted FIRST and the follow-up response SECOND (opposite of the task brief's
    // draft, which assumed last-mounted-highest-priority).
    #[tokio::test(start_paused = true)]
    async fn transient_rate_limit_retries_in_place_then_succeeds() {
        let mock_a = MockServer::start().await;
        // Two 429s (mounted first → matched first; exhausted after 2 → falls through to the 200).
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .up_to_n_times(2)
            .mount(&mock_a).await;
        // Recovery success (mounted second; serves once the 429 mock is exhausted).
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-a"}}]})))
            .mount(&mock_a).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None).await;
        // mk_model defaults retry_count:0 (see Step 8); enable retry on m_a for this test.
        {
            let mut cfg = state.config.write().await;
            let a = cfg.models.iter_mut().find(|m| m.id == "m_a").unwrap();
            a.retry_count = 2;
            a.retry_delay_secs = 5;
        }
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-a"));          // served by A after retries
        assert!(!state.health.is_cooling("m_a", 1000));            // recovered → NOT tripped
        assert_eq!(mock_a.received_requests().await.unwrap().len(), 3); // 2 rate-limited + 1 success
    }

    /// Transient + retries exhausted → trip (Transient, recover_at = now+cooldown) + fallback B.
    #[tokio::test(start_paused = true)]
    async fn transient_retry_exhausted_trips_and_falls_back() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await; // always 429
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}]})))
            .mount(&mock_b).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
        {
            let mut cfg = state.config.write().await;
            let a = cfg.models.iter_mut().find(|m| m.id == "m_a").unwrap();
            a.retry_count = 2;
            a.retry_delay_secs = 5;
        }
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-b"));     // served by fallback B
        assert!(state.health.is_cooling("m_a", 1000));        // tripped after retries exhausted
        assert_eq!(state.health.get("m_a").trip_reason, TripReason::Transient);
        assert_eq!(state.health.get("m_a").recover_at, Some(1300)); // now(1000)+cooldown(300); sleeps don't move FakeClock
        assert_eq!(mock_a.received_requests().await.unwrap().len(), 3); // 1 + 2 retries
    }

    /// A non-rate-limit error arriving on a retry is passed through (not retried further, not
    /// tripped). Sequential responses on one server: 429 mounted FIRST (matched first, once), 401
    /// SECOND (serves the retry).
    #[tokio::test(start_paused = true)]
    async fn non_rate_limit_on_retry_is_passed_through() {
        let mock_a = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .up_to_n_times(1)
            .mount(&mock_a).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"unauthorized"})))
            .mount(&mock_a).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None).await;
        {
            let mut cfg = state.config.write().await;
            let a = cfg.models.iter_mut().find(|m| m.id == "m_a").unwrap();
            a.retry_count = 2;
            a.retry_delay_secs = 5;
        }
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);  // 401 passed through
        assert!(body_str(resp).await.contains("unauthorized"));
        assert!(!state.health.is_cooling("m_a", 1000));        // not tripped (non-rate-limit)
        assert_eq!(mock_a.received_requests().await.unwrap().len(), 2); // 429 then 401
    }

    /// Stream/translate path: first SSE event is a rate-limit → retry → normal stream served, not
    /// tripped. Sequential responses on one server: rate-limit SSE mounted FIRST (once), the real
    /// stream SECOND (serves the retry).
    #[tokio::test(start_paused = true)]
    async fn stream_first_event_rate_limit_retries_then_streams() {
        let mock_a = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_bytes(b"data: {\"error\":{\"code\":1302}}\n\n".to_vec()))
            .up_to_n_times(1)
            .mount(&mock_a).await;
        let sse_ok = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                      data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n\
                      data: [DONE]\n\n";
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_bytes(sse_ok.as_bytes().to_vec()))
            .mount(&mock_a).await;

        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None).await;
        {
            let mut cfg = state.config.write().await;
            let a = cfg.models.iter_mut().find(|m| m.id == "m_a").unwrap();
            a.retry_count = 2;
            a.retry_delay_secs = 5;
        }
        let app = build_router(state.clone());
        let resp = app.oneshot(anthropic_stream_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_str(resp).await;
        assert!(body.contains("\"text\":\"Hel\"") && body.contains("\"text\":\"lo\"")); // translated stream served
        assert!(!state.health.is_cooling("m_a", 1000));  // recovered → not tripped
        assert_eq!(mock_a.received_requests().await.unwrap().len(), 2); // rate-limit event then real stream
    }

    #[tokio::test]
    async fn strategy_entry_model_rate_limits_walks_its_chain() {
        let mock_a = MockServer::start().await; // strategy model: rate-limits
        let mock_b = MockServer::start().await; // a's fallback: succeeds
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}]})))
            .mount(&mock_b).await;

        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider { id: "pa".into(), vendor: "zhipu".into(), display_name: "A".into(), openai_base_url: Some(mock_a.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pb".into(), vendor: "pb".into(), display_name: "B".into(), openai_base_url: Some(mock_b.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.models.push(mk_model("m_a", "pa", Some("m_b"))); // strategy model, falls back to m_b
        cfg.models.push(mk_model("m_b", "pb", None));
        secrets.set_key("pa", "sk-test").unwrap();
        secrets.set_key("pb", "sk-test").unwrap();
        cfg.profiles.push(Profile {
            id: "p".into(), name: "glm-5.2".into(), aliases: vec![],
            backing_model_id: "m_b".into(), strategies_enabled: true,
            strategies: vec![Strategy {
                id: "s".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_a".into() }),
            }],
            ..Default::default()
        });
        let clock = Arc::new(FakeClock::new(1000));
        clock.set_local(2, 500); // Tuesday → strategy matches → entry m_a
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg), secrets,
            catalog: Default::default(), health: Default::default(), clock,
            usage_cache: Default::default(), bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None), bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-b"));    // served by fallback m_b
        assert!(state.health.is_cooling("m_a", 1000));        // strategy entry tripped
    }

    #[test]
    fn body_excerpt_short_body_passes_through() {
        assert_eq!(body_excerpt(b"hello"), "hello");
        assert_eq!(body_excerpt(b""), "");
    }

    #[test]
    fn body_excerpt_truncates_over_1kb_with_ellipsis() {
        let big = vec![b'a'; 2000];
        let s = body_excerpt(&big);
        // 1024 bytes of ASCII = 1024 chars, then the ellipsis.
        assert_eq!(s.chars().count(), 1025);
        assert!(s.starts_with('a'));
        assert!(s.ends_with('…'));
    }

    // ---- Task 4: time-aware chained failover (each hop reads the failing model's own strategy) ----

    /// GLM 429s. GLM has `fallback_target_model_id = m_volcano` (default) AND a time strategy
    /// 22:00–06:00 → m_qwen. m_qwen and m_volcano serve distinct success bodies, so the served
    /// upstream is asserted directly. Pins `FakeClock::set_local` at the three boundaries
    /// (21:59 / 22:00 / 06:00) and expects volcano / qwen / volcano respectively — i.e.
    /// `next_fallback_id` returns the time-strategy target only inside the window.
    #[tokio::test]
    async fn failover_time_window_22_picks_qwen_else_volcano() {
        let mock_glm = MockServer::start().await;    // 429
        let mock_qwen = MockServer::start().await;   // 200 "from-qwen"
        let mock_volcano = MockServer::start().await; // 200 "from-volcano"
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_glm).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-qwen"}}]}),
            ))
            .mount(&mock_qwen).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-volcano"}}]}),
            ))
            .mount(&mock_volcano).await;

        // Each call builds a FRESH state so the breaker from one request cannot leak into the next.
        async fn run_at(
            mock_glm: &MockServer,
            mock_qwen: &MockServer,
            mock_volcano: &MockServer,
            weekday: u8,
            minute: u16,
        ) -> String {
            let mut cfg = AppConfig::default();
            let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
            cfg.providers.push(Provider { id: "pg".into(), vendor: "zhipu".into(), display_name: "GLM".into(), openai_base_url: Some(mock_glm.uri()), anthropic_base_url: None, usage_creds: None });
            cfg.providers.push(Provider { id: "pq".into(), vendor: "pq".into(), display_name: "Qwen".into(), openai_base_url: Some(mock_qwen.uri()), anthropic_base_url: None, usage_creds: None });
            cfg.providers.push(Provider { id: "pv".into(), vendor: "pv".into(), display_name: "Volcano".into(), openai_base_url: Some(mock_volcano.uri()), anthropic_base_url: None, usage_creds: None });
            // GLM: default fallback m_volcano, plus a cross-midnight 22:00-06:00 strategy -> m_qwen.
            cfg.models.push(Model {
                id: "m_glm".into(), provider_id: "pg".into(), source: ModelSource::Manual, upstream_model_id: "glm".into(),
                cooldown_seconds: Some(300), retry_count: 0, fallback_target_model_id: Some("m_volcano".into()),
                fallback_strategies: vec![Strategy {
                    id: "s".into(), priority: 1, enabled: true,
                    kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![1,2,3,4,5,6,7], time_start: 1320, time_end: 360, model_id: "m_qwen".into() }),
                }],
                fallback_strategies_enabled: true,
                ..Default::default()
            });
            cfg.models.push(mk_model("m_qwen", "pq", None));
            cfg.models.push(mk_model("m_volcano", "pv", None));
            for pid in ["pg","pq","pv"] { secrets.set_key(pid, "sk-test").unwrap(); }
            cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_glm".into(), ..Default::default() });

            let clock = Arc::new(FakeClock::new(1000));
            clock.set_local(weekday, minute);
            let state: AppState = Arc::new(AppStateInner {
                config: tokio::sync::RwLock::new(cfg), secrets,
                catalog: Default::default(), health: Default::default(), clock,
                usage_cache: Default::default(),
                bound_port: std::sync::Mutex::new(None),
                server_handle: std::sync::Mutex::new(None),
                bind_error: std::sync::Mutex::new(None),
                polling_handle: std::sync::Mutex::new(None),
            });
            let app = build_router(state.clone());
            let resp = app.oneshot(oai_post()).await.unwrap();
            body_str(resp).await
        }

        // 21:59(1319): window not yet open -> default fallback m_volcano.
        assert!(run_at(&mock_glm, &mock_qwen, &mock_volcano, 2, 1319).await.contains("from-volcano"));
        // 22:00(1320): window opens (evening, today in set) -> strategy target m_qwen.
        assert!(run_at(&mock_glm, &mock_qwen, &mock_volcano, 2, 1320).await.contains("from-qwen"));
        // 06:00(360): window closed (end is exclusive) -> default fallback m_volcano.
        assert!(run_at(&mock_glm, &mock_qwen, &mock_volcano, 2, 360).await.contains("from-volcano"));
    }

    /// GLM 429 → (GLM's own all-day strategy) → m_b; m_b 429 → (m_b's OWN all-day strategy) → m_c;
    /// m_c 200. Neither m_glm nor m_b has a default `fallback_target_model_id`, so the ONLY path
    /// to m_c is for each hop to read the FAILING model's own `fallback_strategies`. Asserts the
    /// served body is m_c's and that both m_glm and m_b tripped (m_c healthy).
    #[tokio::test]
    async fn failover_chain_uses_each_models_own_strategy() {
        let mock_glm = MockServer::start().await; // 429
        let mock_b = MockServer::start().await;   // 429
        let mock_c = MockServer::start().await;   // 200 "from-c"
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_glm).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_b).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-c"}}]})))
            .mount(&mock_c).await;

        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider { id: "pg".into(), vendor: "zhipu".into(), display_name: "GLM".into(), openai_base_url: Some(mock_glm.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pb".into(), vendor: "pb".into(), display_name: "B".into(), openai_base_url: Some(mock_b.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pc".into(), vendor: "pc".into(), display_name: "C".into(), openai_base_url: Some(mock_c.uri()), anthropic_base_url: None, usage_creds: None });
        // m_glm: NO default fallback_target — its all-day (Tue) strategy -> m_b is the only path.
        cfg.models.push(Model {
            id: "m_glm".into(), provider_id: "pg".into(), source: ModelSource::Manual, upstream_model_id: "glm".into(),
            cooldown_seconds: Some(300), retry_count: 0, fallback_target_model_id: None,
            fallback_strategies: vec![Strategy {
                id: "s1".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_b".into() }),
            }],
            fallback_strategies_enabled: true,
            ..Default::default()
        });
        // m_b: NO default fallback_target — its OWN all-day (Tue) strategy -> m_c is the only path.
        cfg.models.push(Model {
            id: "m_b".into(), provider_id: "pb".into(), source: ModelSource::Manual, upstream_model_id: "b".into(),
            cooldown_seconds: Some(300), retry_count: 0, fallback_target_model_id: None,
            fallback_strategies: vec![Strategy {
                id: "s2".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_c".into() }),
            }],
            fallback_strategies_enabled: true,
            ..Default::default()
        });
        cfg.models.push(mk_model("m_c", "pc", None));
        for pid in ["pg","pb","pc"] { secrets.set_key(pid, "sk-test").unwrap(); }
        cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_glm".into(), ..Default::default() });

        let clock = Arc::new(FakeClock::new(1000));
        clock.set_local(2, 500); // Tuesday → both all-day strategies match
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg), secrets,
            catalog: Default::default(), health: Default::default(), clock,
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-c")); // served by m_c (end of chain)
        assert!(state.health.is_cooling("m_glm", 1000)); // GLM tripped, walked its own strategy -> m_b
        assert!(state.health.is_cooling("m_b", 1000));   // m_b tripped, walked its OWN strategy -> m_c
        assert!(!state.health.is_cooling("m_c", 1000));  // m_c served, healthy
    }
}
