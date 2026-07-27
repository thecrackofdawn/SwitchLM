# Traceable Per-Request Fallback Logging Design

**Date:** 2026-08-01
**Status:** Approved
**Author:** Claude (SwitchLM Project)

## Overview

Today a fallback hop is logged as a single opaque line:

```
INFO switchlm::proxy: fallback hop vendor=zhipu model=glm-5.2 reason=cooling down
INFO switchlm::proxy: forward ok status=200 ms=26169
```

This is not debuggable:

- **No *why* cooling** — was the quota window actually exhausted (→ wait for the package reset),
  or was it a transient 429 while quota remained (→ short cooldown)? The log says neither.
- **No direction** — the hop names only the model being skipped, not which model it falls back *to*.
- **No outcome** — `forward ok` does not say which model ultimately served the request.
- **No correlation** — under concurrency the lines of different requests interleave, and nothing
  ties one request's chain together.

This design makes the fallback chain readable and reconstructable, with four changes:

1. **Per-request correlation id** — a short 4-hex id on every dispatch log line.
2. **Hop direction** — structured `from=` / `to=` fields on every hop.
3. **Stored trip reason** — `ModelHealth` records *why* a breaker tripped (exhausted vs transient),
  so a later "cooling down" skip can log the real reason + a human-readable time-to-recover.
4. **Terminal enrichment** — `forward ok` names the serving model + hop count; `forward failed`
  carries hop count.

The chosen chain style is **enriched per-hop lines + an enriched terminal line** (no separate
end-of-request summary line); a chain is reassembled under concurrency by `grep req=<id>`.

## Requirements

### Functional Requirements

1. **Correlation id** — every dispatch log line (`forward request`, each `fallback hop`,
   `forward ok` / `forward failed`) carries a structured `req=<4-hex-lowercase>` field, generated
   once at the start of `dispatch()` and constant for the lifetime of that request.
2. **Hop direction** — every `fallback hop` logs structured `from=<vendor>/<upstream_model>` and
   `to=<vendor>/<upstream_model>` fields. `to=-` when there is no configured fallback target.
3. **Cooling reason** — a "cooling down" hop states whether the breaker tripped because the quota
   window was exhausted (`quota exhausted, resets in <dur>`) or because of a transient throttle
   (`transient throttle, retries in <dur>`), where `<dur>` is a compact human duration to
   `recover_at`.
4. **Rate-limit reason** — a "rate-limited" hop (tripped by *this* request) states
   `rate-limited (quota exhausted)` or `rate-limited (transient)`.
5. **Terminal outcome** — `forward ok` logs the `model=` that produced the response and `hops=`
   (models visited); `forward failed` logs `hops=`.
6. **Concurrency correlation** — because every line shares the request's `req` id,
   `grep req=<id>` reconstructs one request's full chain regardless of interleaving.

### Non-Functional Requirements

1. **No frontend changes** — the new trip-reason field is `#[serde(skip)]` on `ModelHealth`, so the
   serialized wire format and `src/lib/types.ts` are untouched.
2. **Minimal dependency** — add `fastrand` (a zero-transitive-dep, non-crypto PRNG). A 4-hex log tag
   does not need `rand`'s default ChaCha CSPRNG, which is heavier than required.
3. **Log levels unchanged** — hops stay INFO, terminal failure stays WARN. The complaint was
   content/clarity, not severity.
4. **Testable cores** — the log-string logic lives in small pure functions that are unit-tested
   directly; no log-capture harness is added. Existing behavior tests stay green.
5. **Negligible cost** — one `u16` RNG draw per request + a couple of cheap config-lock reads per
   hop (the config is already read on every hop today).

## Why Storing the Trip Reason Is Necessary

The exhausted-vs-transient distinction is decided **only at trip time**, inside
`compute_recover_at` (`proxy/dispatch.rs`): if `exhausted_reset_at(&usage)` finds a window at
100%, `recover_at` is the package reset time (Exhausted); otherwise it is `now + cooldown_seconds`
(Transient).

A "cooling down" skip happens on request *N+1* for a model tripped by request *N*. At skip time
only `recover_at` / `tripped_at` are available (`proxy/health.rs` `ModelHealth`) — the original
reason is gone. Inferring it from `recover_at - tripped_at` is fuzzy (a large `cooldown_seconds`
looks like exhaustion). So the reason is recorded on `ModelHealth` at trip time and read back at
skip time. `compute_recover_at` already computes it, so the cost is one extra field.

`serde(skip)` keeps the field off the wire: `ModelHealth` is serialized for the UI
(`get_model_health` / `snapshot`), but the trip reason is a logging concern, not a UI concern.

## Implementation Architecture

### Data Flow

```
dispatch()
  ├── req_id = fmt_req_id(rand::random::<u16>())        # e.g. "a3f2"
  ├── info!(req=req_id, …, "forward request")
  ├── (outcome, result) = dispatch_{non_stream,stream}(&req_id, …)
  │       loop:
  │         resolve next = next_fallback_id(current)     # resolve BEFORE logging
  │         on cooling:   detail = cooling_reason_detail(health.trip_reason, recover_at, now)
  │         on rate-limit: detail = ratelimit_reason_detail(recover.reason)
  │         else:         detail = "<short reason>"
  │         log_hop(req_id, from=current, to=next, detail)   # from=/to= structured fields
  │         advance or return Err(...)
  │         on success: outcome.served_model_id = Some(current); return Ok(resp)
  └── info!/warn!(req=req_id, model=<served>, hops, status/ms/error, "forward ok"/"forward failed")
```

### 1. Per-request id

**File: `src-tauri/Cargo.toml`** — add `fastrand = "2"` to `[dependencies]`.

**File: `src-tauri/src/proxy/dispatch.rs`**

```rust
/// 4-hex-lowercase request tag, e.g. "a3f2". Generated once per dispatch.
fn fmt_req_id(n: u16) -> String { format!("{:04x}", n) }
```

At the top of `dispatch()`, after the request is parsed:

```rust
let req_id = fmt_req_id(fastrand::u16(0..=0xffff));
```

`req_id` is threaded as `&str` into `dispatch_non_stream` / `dispatch_stream` and into `log_hop`.
The `forward request` line gains `req = %req_id` as its first field.

> **Mechanism note:** the id is an explicit structured field on each event (not a `tracing` span).
> This is predictable with the existing plain `fmt::layer()` stdout+file layers
> (`logging/mod.rs`) and matches the existing trailing `key=value` field style; it does not rely
> on span-context rendering.

### 2. Inner walks report an outcome

**File: `src-tauri/src/proxy/dispatch.rs`**

```rust
/// What the fallback walk produced, for the terminal log. Always returned, even on `Err`.
struct DispatchOutcome {
    hops: usize,                   // models visited in the chain
    served_model_id: Option<String>, // Some when a model produced a response (success OR
                                     // passthrough error); None when the walk failed outright.
}
```

`dispatch_non_stream` / `dispatch_stream` change their return type to
`(DispatchOutcome, Result<Response<Body>, ProxyError>)`. `hops` increments once per visited model;
`served_model_id = Some(current.clone())` is set immediately before each `return Ok(resp)`.

`dispatch()` consumes it:

```rust
let (outcome, result) = if is_stream {
    dispatch_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id).await
} else {
    dispatch_non_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id).await
};
let ms = start.elapsed().as_millis();
let served = match &outcome.served_model_id {
    Some(id) => model_tag(&state, id).await,
    None => "-".into(),
};
match &result {
    Ok(resp) => tracing::info!(
        target: "switchlm::proxy", req = %req_id, model = %served,
        status = resp.status().as_u16(), hops = outcome.hops, ms, "forward ok"
    ),
    Err(e) => tracing::warn!(
        target: "switchlm::proxy", req = %req_id, error = %e,
        hops = outcome.hops, ms, "forward failed"
    ),
}
```

> **Semantics unchanged for passthrough errors:** a non-rate-limit upstream error (e.g. 401) is
> still `Ok` from the proxy's perspective (it forwarded and got an answer), so it remains
> `forward ok` INFO — now with `model=` naming *who* answered and the real `status=`. Renaming the
> level is explicitly out of scope.

### 3. Enriched `log_hop` (structured `from`/`to`)

```rust
/// "{vendor}/{upstream_model_id}". An unresolvable (dangling) id degrades to
/// "unknown/{model_id}" — preserving the raw id for debugging config churn — rather than "-",
/// which would erase it. "-" is used only for an empty vendor/model string on a *resolved* model.
async fn model_tag(state: &AppState, model_id: &str) -> String {
    match snapshot_model(state, model_id).await {
        Some(s) => format!(
            "{}/{}",
            if s.vendor.is_empty() { "-".into() } else { s.vendor },
            if s.upstream_model_id.is_empty() { "-".into() } else { s.upstream_model_id },
        ),
        None => format!("unknown/{model_id}"),
    }
}

async fn log_hop(
    state: &AppState,
    req_id: &str,
    from_id: &str,
    to_id: Option<&str>,
    reason_detail: String,
) {
    let from = model_tag(state, from_id).await;
    let to = match to_id { Some(id) => model_tag(state, id).await, None => "-".into() };
    tracing::info!(
        target: "switchlm::proxy",
        req = %req_id, from = %from, to = %to, reason = %reason_detail,
        "fallback hop"
    );
}
```

Every call site is restructured to **resolve `next` first**, pass it as `to_id`, then branch:

```rust
let next = next_fallback_id(state, &current).await;
log_hop(state, req_id, &current, next.as_deref(), reason_detail).await;
match next {
    Some(n) => { current = n; continue; }
    None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
}
```

The `advance_or_exhausted!` macro in `dispatch_stream` is rewritten around this same shape (resolve
`next`, log with `to=next`, advance or return exhausted), taking the `reason_detail` string as its
argument; `last_error` is assigned by the caller just before.

### 4. Trip reason on `ModelHealth`

**File: `src-tauri/src/proxy/health.rs`**

```rust
/// Why a breaker tripped. Logging-only (serde-skip ⇒ not sent to the UI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TripReason {
    /// Quota still available → transient 429; retry after `cooldown_seconds`.
    #[default]
    Transient,
    /// A quota window hit 100% → wait for the package reset time.
    Exhausted,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ModelHealth {
    pub cooling_down: bool,
    pub recover_at: Option<i64>,
    pub tripped_at: Option<i64>,
    #[serde(skip)]
    pub trip_reason: TripReason,
}
```

`trip()` gains a `reason` argument:

```rust
pub fn trip(&self, model_id: &str, recover_at: Option<i64>, now: i64, reason: TripReason) {
    self.inner.lock().unwrap().insert(
        model_id.into(),
        ModelHealth { cooling_down: true, recover_at, tripped_at: Some(now), trip_reason: reason },
    );
}
```

`recover_if_due` and `snapshot_recovered` reset `trip_reason` to `TripReason::default()` alongside
the other fields when they clear a recovered model.

### 5. `compute_recover_at` returns the reason

**File: `src-tauri/src/proxy/dispatch.rs`** — it already branches on exhaustion, so the reason is
free:

```rust
struct Recover { recover_at: Option<i64>, reason: TripReason }

async fn compute_recover_at(state: &AppState, snap: &ModelSnapshot, now: i64) -> Recover {
    if let Some(usage) = usage_snapshot_realtime(state, snap, now).await {
        if let Some(reset) = exhausted_reset_at(&usage) {
            return Recover { recover_at: Some(reset), reason: TripReason::Exhausted };
        }
    }
    let cooldown = snap.cooldown_seconds.unwrap_or(DEFAULT_COOLDOWN_SECS) as i64;
    Recover { recover_at: Some(now + cooldown), reason: TripReason::Transient }
}
```

The three trip call sites (non-stream `RateLimited`, stream status-level, stream first-event) use
`rec.recover_at` for `trip(...)` and `rec.reason` for the hop reason detail:

```rust
let rec = compute_recover_at(state, &model_snap, now).await;
state.health.trip(&current, rec.recover_at, now, rec.reason);
let next = next_fallback_id(state, &current).await;
log_hop(state, req_id, &current, next.as_deref(), ratelimit_reason_detail(rec.reason)).await;
```

The cooling-skip arm reads the stored reason back:

```rust
if state.health.is_cooling(&current, now) {
    let h = state.health.get(&current);
    last_error = "cooling down".to_string();
    let next = next_fallback_id(state, &current).await;
    log_hop(state, req_id, &current, next.as_deref(),
            cooling_reason_detail(h.trip_reason, h.recover_at, now)).await;
    match next { Some(n) => { current = n; continue; } None => return Err(…) }
}
```

### 6. Pure helper functions (testable cores)

**File: `src-tauri/src/proxy/dispatch.rs`** — the reason/duration *strings* are produced by pure
functions, so they are unit-testable without capturing log output:

```rust
fn human_dur(secs: i64) -> String {
    // "4h12m", "5m", "30s", "<1s" for ≤0
}

fn cooling_reason_detail(reason: TripReason, recover_at: Option<i64>, now: i64) -> String {
    let dur = recover_at.map(|r| human_dur(r - now)).unwrap_or_else(|| "unknown".into());
    match reason {
        TripReason::Exhausted => format!("cooling down (quota exhausted, resets in {dur})"),
        TripReason::Transient => format!("cooling down (transient throttle, retries in {dur})"),
    }
}

fn ratelimit_reason_detail(reason: TripReason) -> String {
    match reason {
        TripReason::Exhausted => "rate-limited (quota exhausted)".into(),
        TripReason::Transient => "rate-limited (transient)".into(),
    }
}
```

> **HTTP status omitted from the reason** on purpose: "rate-limited" + exhausted/transient already
> explains the hop, and 智谱 signals rate limits in a **200** body, so including the status would
> print the confusing `rate-limited (200, …)`.

### Resulting log lines

Same scenario (Anthropic request; `zhipu/glm-5.2` cooling because quota is exhausted; falls back
to `deepseek/deepseek-chat`, 200 OK):

```
INFO req=a3f2 forward request inbound=/v1/messages requested=pro vendor=zhipu model=glm-5.2 stream=true
INFO req=a3f2 fallback hop from=zhipu/glm-5.2 to=deepseek/deepseek-chat reason="cooling down (quota exhausted, resets in 4h12m)"
INFO req=a3f2 forward ok model=deepseek/deepseek-chat status=200 ms=26169 hops=2
```

`grep req=a3f2` returns exactly this request's chain, regardless of other concurrent requests.

## Testing Strategy

### Unit tests — pure functions (`dispatch.rs`)

| Function | Cases |
|----------|-------|
| `human_dur` | `0` → `"<1s"`; `-1` (recover expired but not yet cleared) → `"<1s"`; `30` → `"30s"`; `300` → `"5m"`; `900` → `"15m"`; `3600` → `"1h"`; `15120` → `"4h12m"`. |
| `cooling_reason_detail` | Exhausted + reset → contains `"quota exhausted"` + the dur; Transient + cooldown → contains `"transient throttle"`; `recover_at=None` → `"unknown"`. |
| `ratelimit_reason_detail` | Exhausted → `"rate-limited (quota exhausted)"`; Transient → `"rate-limited (transient)"`. |
| `fmt_req_id` | `0` → `"0000"`; `0xa3f2` → `"a3f2"`; `0xffff` → `"ffff"`. |

### Unit tests — `health.rs`

- New `TripReason` is `Default == Transient`.
- `trip(..., Exhausted)` then `get()` returns `trip_reason == Exhausted`; likewise Transient.
- `recover_if_due` / `snapshot_recovered` reset `trip_reason` to `Transient` when clearing.
- Update existing `trip_*` tests for the new `reason` argument.

### Behavior tests — `dispatch.rs` (extend existing)

- `rate_limit_with_exhausted_quota_uses_reset_at`: also assert
  `state.health.get("m_a").trip_reason == TripReason::Exhausted`.
- `rate_limit_with_available_quota_uses_cooldown`: also assert
  `state.health.get("m_a").trip_reason == TripReason::Transient`.
- Existing fallback/walk tests (`fallback_on_rate_limit`, `cooling_model_skipped_to_fallback`,
  `all_rate_limit_exhausted`, `deepseek_402_falls_back`, `fallback_cycle_breaks`, stream tests)
  stay green — they assert behavior (status, body, cooling state), not log text, so they are
  unaffected by the log enrichment. (`trip()` call sites in tests that pre-trip a model — e.g.
  `cooling_model_skipped_to_fallback` calls `state.health.trip("m_a", Some(2000), 1000)` — gain a
  `TripReason::Transient` 4th argument.)

### Manual / dev-run

Eyeball the log output in `npm run tauri dev` against a real rate-limited plan to confirm the
`from`/`to`/`reason` fields and the `req` correlation read as intended (the pure functions cover
the string logic; this confirms wiring + the `tracing` field rendering).

## Files Changed

### Backend
1. `src-tauri/Cargo.toml` — `+fastrand = "2"`.
2. `src-tauri/src/proxy/health.rs` — `TripReason` enum; `ModelHealth.trip_reason` (`#[serde(skip)]`);
   `trip()` `reason` argument; reset in `recover_if_due` / `snapshot_recovered`; tests updated + added.
3. `src-tauri/src/proxy/dispatch.rs` — `fmt_req_id` + id gen; thread `req_id`; `DispatchOutcome`;
   enriched `log_hop` + `model_tag`; restructured hop call sites + `advance_or_exhausted!` macro;
   `compute_recover_at` → `Recover`; cooling-skip reads stored reason; enriched terminal line;
   pure helpers `human_dur` / `cooling_reason_detail` / `ratelimit_reason_detail` + their tests;
   existing behavior-test `trip(...)` calls updated.

### Frontend
None. `trip_reason` is `#[serde(skip)]`, so the `ModelHealth` JSON shape and `src/lib/types.ts`
are unchanged.

## Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Id format | 4-hex random | User pick; short, distinguishes concurrent requests within a tight window (collision ~300 requests for file-wide grep is acceptable for this app's traffic). |
| Id mechanism | explicit `req=` field, not a span | Predictable with the plain `fmt::layer()` layers; matches existing `key=value` style; no reliance on span-context rendering. |
| `from`/`to` | structured fields | User pick; enables `grep "from=zhipu"` across hops. |
| Chain reassembly | per-hop + terminal only (no summary line) | User pick; `grep req=<id>` reconstructs under concurrency without extra lines or chain-accumulation state. |
| Cooling reason | stored on `ModelHealth` (`#[serde(skip)]`) | Only trip time knows exhausted-vs-transient; skip time reads it back. `serde(skip)` ⇒ zero frontend/wire churn. |
| Randomness dep | add `fastrand` | A non-crypto log tag does not need `rand`'s default ChaCha CSPRNG; `fastrand` is a zero-transitive-dep fast PRNG purpose-built for this (a no-dep `chrono`-nanos + atomic-counter mix was also considered). |
| Log levels | unchanged (INFO hops, WARN terminal failure) | Complaint was content/clarity, not severity. |
| HTTP status in reason | omitted | "rate-limited" + exhausted/transient already explains the hop; status would confuse in 智谱's 200-body case. |
| `forward ok` for passthrough errors | kept INFO (unchanged semantics) | The new `model=`/`status=` fields clarify who answered; renaming the level is out of scope. |

## Success Criteria

1. ✅ Every dispatch log line (`forward request`, each `fallback hop`, `forward ok`/`forward failed`)
   carries `req=<4-hex>`.
2. ✅ Every `fallback hop` logs structured `from=` / `to=` and a `reason` whose cooling and
   rate-limit cases state exhausted-vs-transient (+ a human duration for cooling).
3. ✅ `forward ok` names the serving `model=` and `hops=`; `forward failed` carries `hops=`.
4. ✅ A model tripped under exhausted quota stores `trip_reason = Exhausted`; under available quota,
   `Transient` (asserted in extended dispatch tests + health tests).
5. ✅ No frontend changes; `ModelHealth` wire format and `src/lib/types.ts` unchanged.
6. ✅ `cargo test --manifest-path src-tauri/Cargo.toml` green — existing behavior assertions
   unchanged, new pure-function + trip-reason tests pass.
