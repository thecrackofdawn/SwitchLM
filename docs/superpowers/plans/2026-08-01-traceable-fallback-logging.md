# Traceable Per-Request Fallback Logging — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the proxy's fallback chain log readable & reconstructable — a short per-request `req` id on every line, `from`/`to` direction + a *why* on every hop, and the serving model + hop count on the terminal line.

**Architecture:** A 4-hex `req` id (generated once per `dispatch()`, threaded as `&str`) tags every dispatch log line. Each fallback hop resolves the next model *first*, then logs structured `from=`/`to=` (`vendor/model` via a new `model_tag`) and a `reason` whose cooling/rate-limit cases state exhausted-vs-transient (read from a new `TripReason` stored on `ModelHealth` at trip time). The terminal `forward ok`/`forward failed` lines name the serving model + hop count via a `&mut DispatchOutcome` the walks fill in.

**Tech Stack:** Rust / axum / tracing (existing `fmt` layers in `src-tauri/src/logging/mod.rs`), `fastrand` (new, non-crypto PRNG for the id). No frontend changes.

**Spec:** `docs/superpowers/specs/2026-08-01-traceable-fallback-logging-design.md`

## Global Constraints

- **RNG dep:** add `fastrand = "2"` (NOT `rand` — a 4-hex log tag does not need a CSPRNG; `fastrand` has zero transitive deps).
- **No frontend / wire changes:** `trip_reason` on `ModelHealth` MUST be `#[serde(skip)]` — `src/lib/types.ts` and the serialized `ModelHealth` shape stay untouched.
- **Log levels unchanged:** hops stay INFO; the terminal failure stays WARN. Do not change severity.
- **Log identity convention (existing):** logs identify models by **vendor + upstream model name, never the opaque `m_xxx` id**. The new `from`/`to`/`model` fields go through `model_tag`, which preserves this.
- **Tests:** `cargo test --manifest-path src-tauri/Cargo.toml`. Existing behavior tests (they assert HTTP status / body / breaker state, never log text) MUST stay green after every task.
- **Each task leaves the build compiling and tests green.** The dispatch.rs changes are tightly coupled (a `log_hop`/`trip` signature change forces every call site), so each task groups a compile-stable concern.

---

## Task 1: Store the trip reason (Exhausted vs Transient)

**Why first:** the cooling-skip log (Task 3) needs to read *why* a model is cooling, but that distinction is only known at trip time inside `compute_recover_at`. This task stores it end-to-end. It is the only task that touches `health.rs` and the only one that changes `trip()`'s signature, so it also updates every `trip()` caller to keep the build green.

**Files:**
- Modify: `src-tauri/src/proxy/health.rs` (enum + `ModelHealth` field + `trip()` sig + two reset sites + tests)
- Modify: `src-tauri/src/proxy/dispatch.rs` (`compute_recover_at` returns reason; 3 trip call sites; 1 test `trip()` call; 2 extended assertions; new import)

**Interfaces:**
- Produces: `pub enum TripReason { Transient (default), Exhausted }` in `health.rs`; `ModelHealth.trip_reason: TripReason` (`#[serde(skip)]`); `HealthRegistry::trip(model_id, recover_at, now, reason)`; `compute_recover_at(...) -> Recover { recover_at: Option<i64>, reason: TripReason }`.

- [ ] **Step 1: Write the failing health tests**

In `src-tauri/src/proxy/health.rs`, append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn trip_reason_defaults_to_transient() {
    assert_eq!(ModelHealth::default().trip_reason, TripReason::Transient);
}

#[test]
fn trip_stores_reason_and_get_reads_it() {
    let reg = HealthRegistry::default();
    reg.trip("m1", Some(2000), 1000, TripReason::Exhausted);
    assert_eq!(reg.get("m1").trip_reason, TripReason::Exhausted);
    reg.trip("m2", Some(2000), 1000, TripReason::Transient);
    assert_eq!(reg.get("m2").trip_reason, TripReason::Transient);
}

#[test]
fn recover_if_due_resets_reason_to_default() {
    let reg = HealthRegistry::default();
    reg.trip("m1", Some(2000), 1000, TripReason::Exhausted);
    assert!(reg.recover_if_due("m1", 2000));
    assert_eq!(reg.get("m1").trip_reason, TripReason::Transient); // cleared back to default
}
```

- [ ] **Step 2: Run to verify they fail (compile error = red)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml health::`
Expected: FAIL — `cannot find type \`TripReason\`` / `no field \`trip_reason\`` / `this method takes 4 arguments but 3 were supplied`.

- [ ] **Step 3: Implement `TripReason` + `ModelHealth.trip_reason` + `trip()` signature**

In `src-tauri/src/proxy/health.rs`:

Add the enum just above the `ModelHealth` struct definition:

```rust
/// Why a circuit breaker tripped. Logging-only: `#[serde(skip)]` on `ModelHealth` keeps it off the
/// wire (the UI does not need it). `compute_recover_at` decides the variant at trip time; the
/// cooling-skip log reads it back on a later request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TripReason {
    /// Quota still available → a transient 429; retry after `cooldown_seconds`.
    #[default]
    Transient,
    /// A quota window reached 100% → wait for the package reset time.
    Exhausted,
}
```

Change the `ModelHealth` struct to add the field (keep all existing fields & derives):

```rust
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ModelHealth {
    pub cooling_down: bool,
    pub recover_at: Option<i64>,
    pub tripped_at: Option<i64>,
    /// Why the breaker tripped. Logging-only; never serialized to the UI.
    #[serde(skip)]
    pub trip_reason: TripReason,
}
```

Change `trip()` to take and store `reason`:

```rust
/// Mark `model_id` as cooling until `recover_at` (epoch secs). `now` records `tripped_at`.
/// `reason` records whether this was a real quota exhaustion or a transient throttle, so a later
/// cooling-skip log can explain itself.
pub fn trip(&self, model_id: &str, recover_at: Option<i64>, now: i64, reason: TripReason) {
    self.inner.lock().unwrap().insert(
        model_id.into(),
        ModelHealth { cooling_down: true, recover_at, tripped_at: Some(now), trip_reason: reason },
    );
}
```

In `recover_if_due`, the clear branch (currently `h.cooling_down = false; h.recover_at = None; h.tripped_at = None;`) becomes:

```rust
                let h = map.get_mut(model_id).unwrap();
                h.cooling_down = false;
                h.recover_at = None;
                h.tripped_at = None;
                h.trip_reason = TripReason::default();
                return true;
```

In `snapshot_recovered`, the clear branch (currently `h.cooling_down = false; h.recover_at = None; h.tripped_at = None;`) becomes:

```rust
            if h.cooling_down && h.recover_at.map_or(false, |r| r <= now) {
                h.cooling_down = false;
                h.recover_at = None;
                h.tripped_at = None;
                h.trip_reason = TripReason::default();
            }
```

- [ ] **Step 4: Update existing health.rs tests for the new `trip()` argument**

Every existing `reg.trip("…", Some(…), …)` call in `health.rs` tests now needs a 4th `TripReason` argument. Add `, TripReason::Transient` to each (the calls in `trip_then_cooling_until_recover_at`, `recover_if_due_clears_cooling`, `reset_returns_to_healthy`, `snapshot_reflects_tripped`, `snapshot_recovered_clears_expired`, `snapshot_recovered_mutates_registry`). Example:

```rust
reg.trip("m1", Some(2000), 1000, TripReason::Transient);
```

- [ ] **Step 5: Run health tests — verify green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml health::`
Expected: PASS (all health tests, including the 3 new ones).

- [ ] **Step 6: Make `compute_recover_at` return the reason**

In `src-tauri/src/proxy/dispatch.rs`, add the import near the other `use crate::proxy::…` lines:

```rust
use crate::proxy::health::TripReason;
```

Replace the existing `compute_recover_at` function (it currently returns `Option<i64>`) with:

```rust
/// Result of `compute_recover_at`: when to retry, and whether the trip was caused by actual quota
/// exhaustion (→ wait for the package reset) or a transient throttle (→ short cooldown).
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
```

- [ ] **Step 7: Update the 3 production `trip()` call sites in dispatch.rs**

The three sites currently read `let recover_at = compute_recover_at(state, &<snap>, now).await;` then `state.health.trip(&current, recover_at, now);`. Change each to destructure `Recover` and pass the reason.

Non-stream rate-limit arm (`dispatch_non_stream`, the `Ok(CallOutcome::RateLimited) =>` branch):

```rust
            Ok(CallOutcome::RateLimited) => {
                last_error = "rate-limited".to_string();
                log_hop(state, &current, &last_error).await;
                let rec = compute_recover_at(state, &model_snap, now).await;
                state.health.trip(&current, rec.recover_at, now, rec.reason);
                match next_fallback_id(state, &current).await {
```

Stream status-level rate-limit (the `if is_rate_limit_error(&vendor, Some(resp.status().as_u16()), "") {` block in `dispatch_stream`):

```rust
        if is_rate_limit_error(&vendor, Some(resp.status().as_u16()), "") {
            let rec = compute_recover_at(state, &snap, now).await;
            state.health.trip(&current, rec.recover_at, now, rec.reason);
            advance_or_exhausted!(format!("rate-limited (status {})", resp.status()));
        }
```

Stream first-event rate-limit (the `StreamCommit::RateLimited =>` arm in `dispatch_stream`):

```rust
                    StreamCommit::RateLimited => {
                        let rec = compute_recover_at(state, &snap, now).await;
                        state.health.trip(&current, rec.recover_at, now, rec.reason);
                        advance_or_exhausted!("rate-limited (first event)".to_string());
                    }
```

(`log_hop` itself is unchanged in this task — only `trip`/`compute_recover_at` change. The hop-content enrichment is Task 3.)

- [ ] **Step 8: Update the dispatch.rs test `trip()` call + extend the two quota tests**

In the dispatch.rs `#[cfg(test)]` block, the test imports currently have `use crate::proxy::health::FakeClock;` — change to:

```rust
    use crate::proxy::health::{FakeClock, TripReason};
```

In `cooling_model_skipped_to_fallback`, the direct trip call:

```rust
        state.health.trip("m_a", Some(2000), 1000, TripReason::Transient); // pre-trip a
```

Extend `rate_limit_with_exhausted_quota_uses_reset_at` — add this assertion after the existing `recover_at` assertion:

```rust
        assert_eq!(state.health.get("m_a").trip_reason, TripReason::Exhausted);
```

Extend `rate_limit_with_available_quota_uses_cooldown` — add this assertion after the existing `recover_at` assertion:

```rust
        assert_eq!(state.health.get("m_a").trip_reason, TripReason::Transient);
```

- [ ] **Step 9: Run the full suite — verify green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all tests, including the two extended quota tests (the end-to-end gate that `compute_recover_at → trip` stores the right `TripReason`).

- [ ] **Step 10: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/proxy/health.rs src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): store trip reason (Exhausted vs Transient) on ModelHealth"
```

(No Cargo.toml change in this task — `fastrand` lands in Task 3.)

---

## Task 2: Pure log-string helpers

**Why:** the reason/duration *strings* are the testable cores. Isolating them as pure functions lets us unit-test the exact log wording without a log-capture harness (the project has none). Task 3 wires them into the hops.

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` (4 free functions + tests)

**Interfaces:**
- Consumes: `TripReason` (Task 1).
- Produces: `fmt_req_id(u16) -> String`, `human_dur(i64) -> String`, `cooling_reason_detail(TripReason, Option<i64>, now: i64) -> String`, `ratelimit_reason_detail(TripReason) -> String`. (Unused until Task 3 — expect a `dead_code` warning after this task's commit; it clears once Task 3 wires them.)

- [ ] **Step 1: Write the failing tests**

In the dispatch.rs `#[cfg(test)] mod tests` block, add:

```rust
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml dispatch::tests::fmt_req_id_formats dispatch::tests::human_dur_cases dispatch::tests::cooling_reason dispatch::tests::ratelimit_reason`
Expected: FAIL — `cannot find function \`fmt_req_id\`` etc.

- [ ] **Step 3: Implement the four pure functions**

In `src-tauri/src/proxy/dispatch.rs`, add these free functions (e.g. just above `compute_recover_at`, among the other helpers):

```rust
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
```

- [ ] **Step 4: Run the tests — verify green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml dispatch::tests::fmt_req_id_formats dispatch::tests::human_dur_cases dispatch::tests::cooling_reason dispatch::tests::ratelimit_reason`
Expected: PASS.

- [ ] **Step 5: Run the full suite (confirm nothing else broke) + commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS. (`dead_code` warnings on the 4 new fns are expected — Task 3 wires them.)

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): add pure log-string helpers (req id, human dur, reason detail)"
```

---

## Task 3: Correlation id + enriched hop logs

**Why:** the visible payoff — every hop gets `req`/`from`/`to`/`reason`, and a `req` id ties a request's chain together. This is the largest task because changing `log_hop`'s signature forces every call site to change in the same commit (the build must stay green).

**Files:**
- Modify: `src-tauri/Cargo.toml` (`+fastrand = "2"`)
- Modify: `src-tauri/src/proxy/dispatch.rs` (id gen + threading, `model_tag`, `log_hop` rewrite, all hop call sites in both walks, the `advance_or_exhausted!` macro, `req` on the start + terminal lines, `model_tag` test)

**Interfaces:**
- Consumes: `fmt_req_id`, `cooling_reason_detail`, `ratelimit_reason_detail` (Task 2); `ModelHealth.trip_reason` / `Recover.reason` (Task 1).
- Produces: `async fn model_tag(&AppState, &str) -> String`; new `log_hop(state, req_id, from_id, to_id: Option<&str>, reason_detail: String)`; `dispatch_non_stream`/`dispatch_stream` gain a `req_id: &str` parameter.

- [ ] **Step 1: Add the `fastrand` dependency**

In `src-tauri/Cargo.toml`, add to `[dependencies]` (anywhere in the list):

```toml
fastrand = "2"
```

- [ ] **Step 2: Generate the id + tag the start line**

In `src-tauri/src/proxy/dispatch.rs`, inside `dispatch()`, just before the `forward request` `tracing::info!` (currently around the `let requested = requested_model…` line), add:

```rust
    let req_id = fmt_req_id(fastrand::u16(0..=0xffff));
```

Add `req = %req_id,` as the FIRST field of the `forward request` `tracing::info!`:

```rust
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
```

- [ ] **Step 3: Add `req_id` parameters to both inner walks**

`dispatch_non_stream` signature gains `req_id: &str`:

```rust
async fn dispatch_non_stream(
    state: &AppState,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    start_model_id: &str,
    echo_model: &str,
    req_id: &str,
) -> Result<Response<Body>, ProxyError> {
```

`dispatch_stream` signature gains `req_id: &str`:

```rust
async fn dispatch_stream(
    state: &AppState,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    start_model_id: &str,
    echo_model: &str,
    req_id: &str,
) -> Result<Response<Body>, ProxyError> {
```

Update the two call sites in `dispatch()` (the `if is_stream { … } else { … }`) to pass `&req_id`:

```rust
    let result = if is_stream {
        dispatch_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id).await
    } else {
        dispatch_non_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id).await
    };
```

- [ ] **Step 4: Add `req` to the terminal lines**

In `dispatch()`, add `req = %req_id,` as the first field of BOTH terminal logs (Task 4 adds `model`/`hops` later):

```rust
    match &result {
        Ok(resp) => tracing::info!(
            target: "switchlm::proxy",
            req = %req_id,
            status = resp.status().as_u16(),
            ms,
            "forward ok"
        ),
        Err(e) => tracing::warn!(
            target: "switchlm::proxy",
            req = %req_id,
            error = %e,
            ms,
            "forward failed"
        ),
    }
```

- [ ] **Step 5: Add `model_tag`**

In `src-tauri/src/proxy/dispatch.rs` (near `snapshot_model`), add:

```rust
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
```

- [ ] **Step 6: Write the `model_tag` test**

In the dispatch.rs `#[cfg(test)]` block, add:

```rust
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
            catalog: ModelCatalog::default(),
            secrets: Arc::new(MemoryStore::default()),
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
```

(`mk_model`, `Provider`, `AppConfig`, `AppState`, `AppStateInner`, `MemoryStore`, `Clock`, `FakeClock` are all already imported/defined in the existing test module.)

- [ ] **Step 7: Rewrite `log_hop` to structured `from`/`to`/`reason`**

Replace the entire current `log_hop` function with:

```rust
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
```

- [ ] **Step 8: Restructure the `dispatch_non_stream` hop call sites**

Every hop call site now resolves `next` FIRST, logs with `to = next`, then branches. Replace each block:

**Cooling** (the `if state.health.is_cooling(&current, now) {` block):

```rust
        if state.health.is_cooling(&current, now) {
            let h = state.health.get(&current);
            last_error = "cooling down".to_string();
            let next = next_fallback_id(state, &current).await;
            log_hop(
                state, req_id, &current, next.as_deref(),
                cooling_reason_detail(h.trip_reason, h.recover_at, now),
            ).await;
            match next {
                Some(next) => { current = next; continue; }
                None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
            }
        }
```

**Model missing** (the `None =>` arm of `let model_snap = match snapshot_model(…)`):

```rust
            None => {
                last_error = "model missing".to_string();
                let next = next_fallback_id(state, &current).await;
                log_hop(state, req_id, &current, next.as_deref(), "model missing".to_string()).await;
                match next {
                    Some(next) => { current = next; continue; }
                    None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
                }
            }
```

**No backend** (the `if !has_backend(protocol, &model_snap) {` block):

```rust
        if !has_backend(protocol, &model_snap) {
            last_error = "no backend for this protocol".to_string();
            let next = next_fallback_id(state, &current).await;
            log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
            match next {
                Some(next) => { current = next; continue; }
                None => return Err(ProxyError::NoBackend(current.clone())),
            }
        }
```

**Missing key** (the `None =>` arm of `let key = match key_for(…)`):

```rust
            None => {
                last_error = "missing api key".to_string();
                let next = next_fallback_id(state, &current).await;
                log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
                match next {
                    Some(next) => { current = next; continue; }
                    None => return Err(ProxyError::NoApiKey(model_snap.provider_id.clone())),
                }
            }
```

**Rate-limited** (the `Ok(CallOutcome::RateLimited) =>` arm — note the `log_hop`/`trip`/`next` reorder; `rec` came from Task 1):

```rust
            Ok(CallOutcome::RateLimited) => {
                last_error = "rate-limited".to_string();
                let rec = compute_recover_at(state, &model_snap, now).await;
                state.health.trip(&current, rec.recover_at, now, rec.reason);
                let next = next_fallback_id(state, &current).await;
                log_hop(state, req_id, &current, next.as_deref(), ratelimit_reason_detail(rec.reason)).await;
                match next {
                    Some(next) => { current = next; continue; }
                    None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
                }
            }
```

(Delete the old standalone `log_hop(state, &current, &last_error).await;` line that used to sit at the top of this arm — logging now happens after `next` is known.)

- [ ] **Step 9: Rewrite the `dispatch_stream` macro + hop call sites**

Replace the `advance_or_exhausted!` macro definition with `hop_and_advance!` (same scope-capture behavior; resolves `next` first):

```rust
    macro_rules! hop_and_advance {
        ($detail:expr) => {{
            let next = next_fallback_id(state, &current).await;
            log_hop(state, req_id, &current, next.as_deref(), $detail).await;
            match next {
                Some(n) => { current = n; continue; }
                None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
            }
        }};
    }
```

**Cooling** (the `if state.health.is_cooling(&current, now) {` block):

```rust
        if state.health.is_cooling(&current, now) {
            let h = state.health.get(&current);
            last_error = "cooling down".to_string();
            hop_and_advance!(cooling_reason_detail(h.trip_reason, h.recover_at, now));
        }
```

**Model missing** (the `None =>` arm of `let snap = match snapshot_model(…)`):

```rust
            None => {
                last_error = "model missing".to_string();
                hop_and_advance!("model missing".to_string());
            }
```

**No backend** (the `None =>` arm of `let path = match stream_path(…)`) — keeps its `NoBackend` error:

```rust
                None => {
                    last_error = "no backend for protocol".to_string();
                    let next = next_fallback_id(state, &current).await;
                    log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
                    match next {
                        Some(n) => { current = n; continue; }
                        None => return Err(ProxyError::NoBackend(current.clone())),
                    }
                }
```

**Missing key** (the `None =>` arm of `let key = match key_for(…)`) — keeps its `NoApiKey` error:

```rust
            None => {
                last_error = "missing api key".to_string();
                let next = next_fallback_id(state, &current).await;
                log_hop(state, req_id, &current, next.as_deref(), last_error.clone()).await;
                match next {
                    Some(n) => { current = n; continue; }
                    None => return Err(ProxyError::NoApiKey(snap.provider_id.clone())),
                }
            }
```

**Status-level rate-limit** (the `if is_rate_limit_error(…) {` block):

```rust
        if is_rate_limit_error(&vendor, Some(resp.status().as_u16()), "") {
            let rec = compute_recover_at(state, &snap, now).await;
            state.health.trip(&current, rec.recover_at, now, rec.reason);
            last_error = "rate-limited".to_string();
            hop_and_advance!(ratelimit_reason_detail(rec.reason));
        }
```

**First-event rate-limit** (the `StreamCommit::RateLimited =>` arm):

```rust
                    StreamCommit::RateLimited => {
                        let rec = compute_recover_at(state, &snap, now).await;
                        state.health.trip(&current, rec.recover_at, now, rec.reason);
                        last_error = "rate-limited".to_string();
                        hop_and_advance!(ratelimit_reason_detail(rec.reason));
                    }
```

- [ ] **Step 10: Build + run the full suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — `model_tag_formats_vendor_model_and_unknown_for_dangling` plus all existing behavior tests. (The existing tests assert status/body/cooling, not log text, so the rewrite is transparent to them. No `dead_code` warnings remain — all helpers are now used.)

- [ ] **Step 11: Manual log check**

Run the app (`npm run tauri dev`), point an agent at the proxy, and trigger a fallback (e.g. a model with no key, or a rate-limited plan). Confirm a hop line looks like:

```
INFO req=a3f2 fallback hop from=zhipu/glm-5.2 to=deepseek/deepseek-chat reason="cooling down (quota exhausted, resets in 4h12m)"
```

with the same `req=` on the `forward request` and `forward ok` lines. (If you cannot trigger a real rate limit, at minimum confirm the build runs and a normal request logs `req=` on all three line types.)

- [ ] **Step 12: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): per-request req id + from/to/reason on every fallback hop"
```

---

## Task 4: Terminal outcome — serving model + hop count

**Why:** `forward ok` should name which model actually served, and both terminal lines should show how many models the chain visited. The walks fill a `&mut DispatchOutcome` (avoids changing every `return` — they keep returning `Result` as today).

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` (`DispatchOutcome` struct; `&mut` param on both walks; set `hops`/`served_model_id`; enrich terminal lines)

**Interfaces:**
- Consumes: `model_tag` (Task 3).
- Produces: the walks now also fill `&mut DispatchOutcome { hops, served_model_id }`; the terminal `forward ok` gains `model=` + `hops=`, `forward failed` gains `hops=`.

- [ ] **Step 1: Add the `DispatchOutcome` struct**

In `src-tauri/src/proxy/dispatch.rs` (near the top, after `CallOutcome`), add:

```rust
/// What a fallback walk produced, for the terminal log line. The walk fills it via `&mut`; always
/// meaningful even when the walk returns `Err`.
struct DispatchOutcome {
    hops: usize,                     // models visited in the chain
    served_model_id: Option<String>, // Some when a model produced a response (success OR a
                                     // passthrough error like 401); None when the walk failed outright.
}
```

- [ ] **Step 2: Thread `&mut DispatchOutcome` into both walks**

In `dispatch()`, declare the outcome and pass it mutably:

```rust
    let mut outcome = DispatchOutcome { hops: 0, served_model_id: None };
    let result = if is_stream {
        dispatch_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id, &mut outcome).await
    } else {
        dispatch_non_stream(&state, &req, protocol, &start_model_id, &echo_model, &req_id, &mut outcome).await
    };
```

`dispatch_non_stream` signature gains `outcome: &mut DispatchOutcome` (after `req_id`):

```rust
async fn dispatch_non_stream(
    state: &AppState,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    start_model_id: &str,
    echo_model: &str,
    req_id: &str,
    outcome: &mut DispatchOutcome,
) -> Result<Response<Body>, ProxyError> {
```

`dispatch_stream` signature gains `outcome: &mut DispatchOutcome` (after `req_id`):

```rust
async fn dispatch_stream(
    state: &AppState,
    req: &serde_json::Value,
    protocol: ClientProtocol,
    start_model_id: &str,
    echo_model: &str,
    req_id: &str,
    outcome: &mut DispatchOutcome,
) -> Result<Response<Body>, ProxyError> {
```

- [ ] **Step 3: Record `hops` + `served_model_id` inside the walks**

In BOTH `dispatch_non_stream` and `dispatch_stream`, immediately after the `tried.push(current.clone());` line, add:

```rust
        outcome.hops = tried.len();
```

In `dispatch_non_stream`, set `served_model_id` on the success return (the `Ok(CallOutcome::Respond(resp)) =>` arm):

```rust
            Ok(CallOutcome::Respond(resp)) => {
                outcome.served_model_id = Some(current.clone());
                return Ok(resp)
            }
```

In `dispatch_stream`, set `served_model_id` at both success returns:

The translate-commit success (`StreamCommit::Respond(r) =>`):

```rust
                    StreamCommit::Respond(r) => {
                        outcome.served_model_id = Some(current.clone());
                        return Ok(r);
                    }
```

The passthrough success (the `StreamPath::OpenAiPassthrough | StreamPath::AnthropicPassthrough =>` arm's `return Ok(out.body(…)…)`):

```rust
                outcome.served_model_id = Some(current.clone());
                return Ok(out.body(Body::from_stream(resp.bytes_stream())).unwrap());
```

(`TransportErr`, `send_stream(…)?`, and all `Err` returns leave `served_model_id` as `None` — correct: no model produced a response.)

- [ ] **Step 4: Enrich the terminal lines with `model` + `hops`**

Replace the terminal `match &result { … }` block in `dispatch()` with:

```rust
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
```

(If you no longer reference the old `let ms = start.elapsed().as_millis();` that sat above the match, make sure `ms` is declared exactly once — move/keep it as shown.)

- [ ] **Step 5: Build + run the full suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all existing behavior tests (they don't call the inner walks directly, so the new `&mut` param is transparent; they assert status/body/cooling, unaffected by the terminal enrichment).

- [ ] **Step 6: Manual log check**

Run the app, trigger a successful fallback, and confirm the terminal line names the server + hops:

```
INFO req=a3f2 forward ok model=deepseek/deepseek-chat status=200 ms=26169 hops=2
```

Confirm a passthrough error (e.g. a 401 from the first model) logs `forward ok model=<that model> status=401 hops=1`, and an exhausted chain logs `WARN req=… forward failed error="…" hops=3 ms=…`.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): log serving model + hop count on the terminal forward line"
```

---

## Self-Review (completed inline)

**1. Spec coverage** — every spec requirement maps to a task:
- Correlation id (FR1) → Task 3 steps 2–4.
- Hop direction `from`/`to` (FR2) → Task 3 steps 5, 7–9.
- Cooling reason exhausted/transient + duration (FR3) → Task 1 (storage) + Task 2 (`cooling_reason_detail`) + Task 3 step 8 (wired at the cooling call site).
- Rate-limit reason (FR4) → Task 2 (`ratelimit_reason_detail`) + Task 3 steps 8–9 (wired).
- Terminal outcome `model`/`hops` (FR5) → Task 4.
- Concurrency correlation (FR6) → `req` everywhere (Task 3).
- NFR no-frontend (`#[serde(skip)]`) → Task 1 step 3.
- NFR `fastrand` → Task 3 step 1.
- NFR log levels unchanged → no level changes anywhere.
- NFR testable cores → Task 2; `model_tag` test → Task 3 step 6.
- `model_tag` `unknown/{id}` fallback (review point 2) → Task 3 steps 5–6.
- `human_dur` `0`/`-1` coverage (review point 3) → Task 2 step 1 test.

**2. Placeholder scan** — none. Every code step contains real code; every test contains real assertions.

**3. Type consistency** — `TripReason` (Task 1) is the name used in Tasks 2–3; `Recover { recover_at, reason }` (Task 1) consumed in Task 3; `model_tag(state, model_id) -> String` (Task 3) consumed in Task 4; `log_hop(state, req_id, from_id, to_id: Option<&str>, reason_detail: String)` consistent across the macro and all call sites; `DispatchOutcome { hops, served_model_id }` (Task 4) consistent at declare/fill/read sites; `trip(model_id, recover_at, now, reason)` consistent across health.rs and all 3 production + test call sites.
