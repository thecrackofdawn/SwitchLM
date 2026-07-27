# Transient Throttle In-Place Retry — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a model returns a transient rate-limit (quota query says NOT exhausted), wait + retry the same model in-place up to `retry_count` times before tripping the breaker and falling back.

**Architecture:** Per-model `retry_count` / `retry_delay_secs` config (default 2 / 5). A unified `AttemptOutcome` lets both the non-stream and stream paths share one `attempt_with_retry` wrapper that loops on `RateLimited` only when `compute_recover_at` reports `TripReason::Transient`. `Exhausted` and `retry_count == 0` preserve today's trip-and-fallback behavior exactly. Spec: `docs/superpowers/specs/2026-08-04-transient-throttle-retry-design.md`.

**Tech Stack:** Rust (axum 0.7, reqwest 0.12, tokio 1), Vue 3 + Pinia + Naive UI, Tauri 2 IPC. Tests: `wiremock 0.6`, `FakeClock`, `tokio::time::pause`.

## Global Constraints

- **Package manager is pnpm.** Never use `npm`/`npx` — use `pnpm`/`pnpm exec` (e.g. `pnpm build`, `pnpm exec vue-tsc`). `npm install` writes a foreign lockfile.
- **Backend tests** are co-located `#[cfg(test)] mod tests` in each `.rs`. Run one module: `cargo test --manifest-path src-tauri/Cargo.toml proxy::dispatch` (or by test name).
- **Logs must show vendor + upstream model name, never the opaque `m_xxx` id.** New retry log lines follow the existing `req=<4-hex>` correlation convention.
- **Frontend is Chinese-language.** New UI labels are Chinese.
- **`src/lib/types.ts` is a hand-maintained TS mirror of the Rust serde structs** — keep field names snake_case, in sync with `config/types.rs` + `commands.rs`.
- **No compatibility concern** — the app is unreleased; new `Model` fields default to 2/5 (retry ON), not opt-in.
- **Spec §-references** in code comments point at `docs/superpowers/specs/2026-07-27-switchlm-llm-proxy-design.md`.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `src-tauri/src/config/types.rs` | `Model` struct + serde + `Default` | Add `retry_count`, `retry_delay_secs` fields, default fns, manual `Default` entries |
| `src-tauri/src/proxy/dispatch.rs` | Request dispatch, fallback walk, breaker | Rename `CallOutcome`→`AttemptOutcome`; extract `stream_attempt`; add `AttemptKind` + `attempt_with_retry`; add retry fields to `ModelSnapshot`; wire both paths; add retry tests; set `mk_model` retry to 0 |
| `src-tauri/src/commands.rs` | `set_model_failover` / `apply_model_failover` | Add `retry_count` / `retry_delay_secs` params; update callers + tests |
| `src/lib/types.ts` | TS mirror of Rust structs | Add `retry_count: number; retry_delay_secs: number;` (required, non-nullable) |
| `src/lib/commands.ts` | Typed Tauri invoke wrappers | Add two params to `setModelFailover` |
| `src/views/Fallback.vue` | Failover config modal | Add two `NInputNumber`s; round-trip in `FormState`/`openEdit`/`doSave` |
| `src/views/Models.vue` | Model list + edit modal | Round-trip the two new fields like `cooldown_seconds`; optional list-row chip |

---

### Task 1: Add `retry_count` / `retry_delay_secs` to `Model` (config, inert)

These fields are added but **not yet read by dispatch** — purely additive, no behavior change. Dispatch starts using them in Task 3.

**Files:**
- Modify: `src-tauri/src/config/types.rs` (Model struct ~L141-160, manual `impl Default` ~L168-181)

**Interfaces:**
- Produces: `Model.retry_count: u32`, `Model.retry_delay_secs: u64`; free fns `default_retry_count() -> u32` (2), `default_retry_delay() -> u64` (5). Later tasks read these.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/config/types.rs` `mod tests`, add (mirrors the existing serde round-trip tests at ~L381-407):

```rust
#[test]
fn model_retry_defaults_align_between_default_and_serde() {
    // Code default and serde default must agree (spec §3 / §7).
    let m = Model::default();
    assert_eq!(m.retry_count, default_retry_count());
    assert_eq!(m.retry_count, 2);
    assert_eq!(m.retry_delay_secs, default_retry_delay());
    assert_eq!(m.retry_delay_secs, 5);

    // A Model JSON that OMITS both fields deserializes to the same defaults.
    let json = r#"{
        "id":"m","provider_id":"p","upstream_model_id":"glm",
        "source":"manual","cooldown_seconds":null,"fallback_target_model_id":null,
        "fallback_strategies":[],"fallback_strategies_enabled":true
    }"#;
    let m: Model = serde_json::from_str(json).unwrap();
    assert_eq!(m.retry_count, 2);
    assert_eq!(m.retry_delay_secs, 5);

    // Round-trips through serialize→deserialize.
    let json = serde_json::to_string(&Model::default()).unwrap();
    let back: Model = serde_json::from_str(&json).unwrap();
    assert_eq!(back.retry_count, 2);
    assert_eq!(back.retry_delay_secs, 5);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml model_retry_defaults`
Expected: compile error — `default_retry_count` / fields not defined.

- [ ] **Step 3: Add the fields + default fns + Default entries**

In the `Model` struct, after `fallback_strategies_enabled`:

```rust
    /// 临时限流（额度未耗尽）时的原地重试次数。0 = 关闭（立即熔断 + fallback）。默认 2。
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
    /// 每次重试前等待秒数。默认 5。
    #[serde(default = "default_retry_delay")]
    pub retry_delay_secs: u64,
```

Next to `default_fb_strategies_enabled`:

```rust
fn default_retry_count() -> u32 { 2 }
fn default_retry_delay() -> u64 { 5 }
```

In the existing manual `impl Default for Model`, add (same fns — keeps code default == serde default):

```rust
            retry_count: default_retry_count(),
            retry_delay_secs: default_retry_delay(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml model_retry_defaults`
Expected: PASS.

- [ ] **Step 5: Confirm the wider suite still compiles + passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (fields are inert; no dispatch change yet).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/config/types.rs
git commit -m "feat(config): Model.retry_count/retry_delay_secs (inert, default 2/5)"
```

---

### Task 2: Refactor — unify `AttemptOutcome` + extract `stream_attempt` (no behavior change)

Pure refactor. Both dispatch paths produce one `AttemptOutcome` via a single per-path attempt function. This creates the clean injection point Task 3 wraps with retry. **All existing dispatch tests must stay green** — they are the safety net.

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` (`CallOutcome` enum ~L34-39, `dispatch_stream` loop body ~L358-436, `dispatch_non_stream` rate-limit arm ~L248-267)

**Interfaces:**
- Produces: enum `AttemptOutcome { Respond(Response<Body>), RateLimited }` (renamed from `CallOutcome`); `async fn stream_attempt(snap, req, path, key, echo_model, req_id) -> Result<AttemptOutcome, ProxyError>`. `select_and_call` return type becomes `Result<AttemptOutcome, ProxyError>`.

- [ ] **Step 1: Establish green baseline**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::dispatch`
Expected: PASS. (These tests guard the refactor: `stream_first_event_rate_limit_falls_back`, `stream_content_then_error_no_retry`, `stream_non_2xx_rate_limit_body_trips_and_falls_back`, `stream_non_2xx_non_rate_limit_body_forwarded`, `fallback_on_rate_limit`, etc.)

- [ ] **Step 2: Rename `CallOutcome` → `AttemptOutcome`**

Rename the enum and its two construction sites in `select_and_call`'s callees (`openai_passthrough`, `anthropic_passthrough`, `translate_via_openai` each `return Ok(CallOutcome::…)`) and in `dispatch_non_stream`'s match arms. The enum doc-comment already says "Result of one upstream attempt" — keep it.

```rust
/// Result of one upstream attempt. `RateLimited` is only ever returned before any content has
/// been forwarded to the client (non-stream: buffered body; stream: first event).
enum AttemptOutcome {
    Respond(Response<Body>),
    RateLimited,
}
```

- [ ] **Step 3: Extract `stream_attempt` from `dispatch_stream`**

Move the body of `dispatch_stream`'s loop that starts at `let resp = send_stream(...)` (current ~L358) through the three rate-limit detection points and the 2xx commit (through ~L436) into a new function. The caller's loop body becomes a single match on `stream_attempt(...)`:

```rust
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
```

`dispatch_stream`'s loop body (after the cooling/missing/backend/key checks) now becomes:

```rust
        match stream_attempt(&snap, req, path, &key, echo_model, req_id).await {
            Ok(AttemptOutcome::Respond(resp)) => {
                outcome.served_model_id = Some(current.clone());
                return Ok(resp);
            }
            Ok(AttemptOutcome::RateLimited) => {
                let rec = compute_recover_at(state, &snap, now).await;
                state.health.trip(&current, rec.recover_at, now, rec.reason);
                last_error = "rate-limited".to_string();
                hop_and_advance!(ratelimit_reason_detail(rec.reason));
            }
            Err(e) => return Err(e),
        }
```

(Delete the now-duplicated inline logic from `dispatch_stream`. `StreamPath`/`StreamCommit`/`send_stream`/`translate_stream_commit` stay where they are.)

- [ ] **Step 4: Run the dispatch suite — must be 100% green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::dispatch`
Expected: PASS (identical behavior; only structure changed).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "refactor(dispatch): unify AttemptOutcome + extract stream_attempt (no behavior change)"
```

---

### Task 3: `attempt_with_retry` wrapper + wire both paths + tests

This is the behavioral core. Retry loops on `RateLimited` only when `Transient` and `retry_count > 0`. `Exhausted` and `retry_count == 0` behave exactly as today. To keep the existing "429 → immediate fallback" tests green, `mk_model` (the fallback-chain test helper) sets `retry_count: 0`; retry tests construct models with `retry_count > 0`.

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` (`ModelSnapshot` ~L819, `snapshot_model` ~L828, `dispatch_non_stream` rate-limit arm, `dispatch_stream` rate-limit arm, test helper `mk_model` ~L1049, `mod tests`)

**Interfaces:**
- Produces: `ModelSnapshot.retry_count: u32`, `ModelSnapshot.retry_delay_secs: u64`; enum `AttemptKind { NonStream { protocol: ClientProtocol }, Stream { path: StreamPath } }`; enum `AttemptWithRetry { Respond(Response<Body>), RateLimited(Recover) }`; `async fn attempt_with_retry(state, snap, req, now, req_id, echo_model, key, kind, retry_count, retry_delay_secs) -> Result<AttemptWithRetry, ProxyError>`.

- [ ] **Step 1: Carry retry config in `ModelSnapshot`**

In `struct ModelSnapshot`, add:
```rust
    retry_count: u32,
    retry_delay_secs: u64,
```
In `snapshot_model`, populate them: `retry_count: m.retry_count, retry_delay_secs: m.retry_delay_secs,`

- [ ] **Step 2: Write the failing retry-recovery test (non-stream)**

In `dispatch.rs` `mod tests`, add. `start_paused = true` auto-advances `tokio::time::sleep` with no real wall-clock delay. No usage endpoint is mounted → the realtime usage query 404s → `compute_recover_at` returns `Transient` (this is the "query-failed also retries" decision, spec §2). wiremock 0.6 is last-mounted-highest-priority: mount the 200 first, then the 429 `up_to_n_times(2)` so calls 1–2 get 429 and call 3 falls through to 200. **If the response sequence comes out wrong, swap the two `mount` calls** (the assertions will fail loudly if so).

```rust
#[tokio::test(start_paused = true)]
async fn transient_rate_limit_retries_in_place_then_succeeds() {
    let mock_a = MockServer::start().await;
    // Fallback success (lower priority — mounted first).
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-a"}}]})))
        .mount(&mock_a).await;
    // Two 429s (higher priority — mounted last; exhausted after 2 → falls through to 200).
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
        .up_to_n_times(2)
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transient_rate_limit_retries_in_place`
Expected: FAIL — no retry yet; the request falls back (or 502s since no fallback B here) and `mock_a` gets 1 request.

- [ ] **Step 4: Add `AttemptKind` + `AttemptWithRetry` + `attempt_with_retry`**

Near the other dispatch helpers (after `compute_recover_at`):

```rust
/// Which per-path attempt `attempt_with_retry` drives.
#[derive(Clone, Copy)]
enum AttemptKind {
    NonStream { protocol: ClientProtocol },
    Stream { path: StreamPath },
}

/// `AttemptOutcome` + the recovery info computed on the first rate-limit (passed back so the
/// caller trips without re-querying quota). Spec §4.2.
enum AttemptWithRetry {
    Respond(Response<Body>),
    RateLimited(Recover),
}

/// One upstream attempt, looping in-place on a pre-content rate-limit when the trip is Transient
/// and `retry_count > 0`. Decides Exhausted/Transient ONCE (first rate-limit) via
/// `compute_recover_at`; reuses that `Recover` for every subsequent rate-limit and for the final
/// trip — never re-queries quota. `Exhausted` and `retry_count == 0` return `RateLimited`
/// immediately (today's behavior). `tokio::time::sleep` is auto-advanced under paused test time.
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
                // Decide transient vs exhausted exactly once.
                let rec = if attempt == 0 {
                    let r = compute_recover_at(state, snap, now).await;
                    first_rec = Some(clone_recover(&r));
                    r
                } else {
                    clone_recover(first_rec.as_ref().expect("set on first rate-limit"))
                };
                if rec.reason == TripReason::Exhausted {
                    return Ok(AttemptWithRetry::RateLimited(rec)); // no retry on real exhaustion
                }
                if attempt >= retry_count {
                    return Ok(AttemptWithRetry::RateLimited(rec)); // retries exhausted → trip + fallback
                }
                tracing::info!(
                    target: "switchlm::proxy", req = %req_id,
                    attempt = attempt + 1, of = retry_count,
                    secs = retry_delay_secs, "retry (transient)",
                );
                tokio::time::sleep(std::time::Duration::from_secs(retry_delay_secs)).await;
                attempt += 1;
            }
        }
    }
}

/// `Recover` carries no refs but isn't `Copy` (has `Option<i64>` + enum — cheap to clone). Helper
/// to read it out of an `&Recover` when reusing across attempts.
fn clone_recover(r: &Recover) -> Recover {
    Recover { recover_at: r.recover_at, reason: r.reason }
}
```

(If `Recover` can be made `#[derive(Clone, Copy)]` instead, prefer that and drop `clone_recover`. `TripReason` is already `Copy`; `Option<i64>` is `Copy`. So adding `#[derive(Clone, Copy)]` to `struct Recover` is cleanest — do that and use `first_rec.unwrap()` directly. Pick one; remove the helper if you derive Copy.)

- [ ] **Step 5: Wire `attempt_with_retry` into `dispatch_non_stream`**

Replace the `match select_and_call(...)` arm (current ~L248-267) with:

```rust
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
            Err(e) => return Err(e),
        }
```

(`compute_recover_at` is no longer called here — it moved into the wrapper.)

- [ ] **Step 6: Wire `attempt_with_retry` into `dispatch_stream`**

Replace the `match stream_attempt(...)` arm from Task 2 with the same shape, using `AttemptKind::Stream { path }`:

```rust
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
```

- [ ] **Step 7: Run the recovery test**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transient_rate_limit_retries_in_place`
Expected: PASS.

- [ ] **Step 8: Set `mk_model` retry to 0 so existing fallback tests stay green**

In `mk_model` (~L1049), add `retry_count: 0, retry_delay_secs: 5,` so the existing 429-fallback suite (`fallback_on_rate_limit`, `all_rate_limit_exhausted`, `deepseek_402_falls_back`, `fallback_cycle_breaks`, `strategy_entry_model_rate_limits_walks_its_chain`, `failover_*`, `rate_limit_with_available_quota_uses_cooldown`) keeps asserting immediate trip+fallback.

```rust
fn mk_model(id: &str, provider_id: &str, fb: Option<&str>) -> Model {
    Model {
        id: id.into(),
        provider_id: provider_id.into(),
        source: ModelSource::Manual,
        upstream_model_id: id.into(),
        cooldown_seconds: Some(300),
        retry_count: 0,         // retry opt-out for the fallback-chain suite (spec §7)
        retry_delay_secs: 5,
        fallback_target_model_id: fb.map(String::from),
        ..Default::default()
    }
}
```

- [ ] **Step 9: Run the full dispatch suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::dispatch`
Expected: PASS — recovery test + all pre-existing tests. (`rate_limit_with_exhausted_quota_uses_reset_at` still asserts Exhausted → no retry; `rate_limit_with_available_quota_uses_cooldown` still asserts recover_at == 1300 because `mk_model` retry is 0.)

- [ ] **Step 10: Add the remaining retry-boundary tests**

```rust
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

/// A non-rate-limit error arriving on a retry is passed through (not retried further, not tripped).
#[tokio::test(start_paused = true)]
async fn non_rate_limit_on_retry_is_passed_through() {
    let mock_a = MockServer::start().await;
    // Call 2 (the retry) → 401 (lower priority, mounted first); call 1 → 429 (higher, up_to_n_times 1).
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"unauthorized"})))
        .mount(&mock_a).await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
        .up_to_n_times(1)
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

/// Stream/translate path: first SSE event is a rate-limit → retry → normal stream served, not tripped.
#[tokio::test(start_paused = true)]
async fn stream_first_event_rate_limit_retries_then_streams() {
    let mock_a = MockServer::start().await;
    // Normal translated stream (lower priority, mounted first).
    let sse_ok = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                  data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n\
                  data: [DONE]\n\n";
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_bytes(sse_ok.as_bytes().to_vec()))
        .mount(&mock_a).await;
    // First call: 智谱 rate-limit inside a 200 SSE event (higher priority, once).
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_bytes(b"data: {\"error\":{\"code\":1302}}\n\n".to_vec()))
        .up_to_n_times(1)
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
```

- [ ] **Step 11: Run the whole backend test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS. If any non-dispatch test that drives a 429 through dispatch fails (e.g. `tests/e2e_zhipu.rs`), set `retry_count: 0` on that test's model (same reason as `mk_model`).

- [ ] **Step 12: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(dispatch): transient rate-limit in-place retry (attempt_with_retry, both paths)"
```

---

### Task 4: Expose retry config via `set_model_failover` / `apply_model_failover`

The Rust command + pure core gain two params. Frontend (Task 5) must land before the app is run, but `cargo test` passes after this task alone.

**Files:**
- Modify: `src-tauri/src/commands.rs` (`apply_model_failover` ~L546, `set_model_failover` ~L570, and their test callers ~L1845-1869)

**Interfaces:**
- Produces: `apply_model_failover(cfg, model_id, fallback_target_model_id, fallback_strategies, fallback_strategies_enabled, cooldown_seconds, retry_count: u32, retry_delay_secs: u64)`; `set_model_failover` gains matching Tauri params.

- [ ] **Step 1: Write the failing test**

In `commands.rs` `mod tests`, extend the existing failover test (~L1845 asserts `cooldown_seconds == Some(120)`). After the existing `apply_model_failover(...)` call in that test, also assert the retry fields are written:

```rust
    // (after the existing apply_model_failover call that sets cooldown to 120)
    assert_eq!(m.retry_count, 2);
    assert_eq!(m.retry_delay_secs, 7);
```

(Adjust the call site to pass `2, 7` — see Step 3. If the existing test uses different values for the other fields, keep them; only add the two new trailing args.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml apply_model_failover` (or the existing test name)
Expected: FAIL — compile error: `apply_model_failover` takes 6 args, not 8.

- [ ] **Step 3: Add the params + write them**

`apply_model_failover`:
```rust
pub fn apply_model_failover(
    cfg: &mut AppConfig,
    model_id: &str,
    fallback_target_model_id: Option<String>,
    mut fallback_strategies: Vec<Strategy>,
    fallback_strategies_enabled: bool,
    cooldown_seconds: Option<u64>,
    retry_count: u32,
    retry_delay_secs: u64,
) -> Result<(), String> {
    normalize_and_validate_strategies(cfg, &mut fallback_strategies)?;
    let m = cfg.models.iter_mut().find(|m| m.id == model_id)
        .ok_or_else(|| format!("model {model_id} not found"))?;
    m.fallback_target_model_id = fallback_target_model_id;
    m.fallback_strategies = fallback_strategies;
    m.fallback_strategies_enabled = fallback_strategies_enabled;
    m.cooldown_seconds = cooldown_seconds;
    m.retry_count = retry_count;
    m.retry_delay_secs = retry_delay_secs;
    Ok(())
}
```

`set_model_failover` — add the two params and forward them:
```rust
pub async fn set_model_failover(
    state: State<'_, AppState>,
    model_id: String,
    fallback_target_model_id: Option<String>,
    fallback_strategies: Vec<Strategy>,
    fallback_strategies_enabled: bool,
    cooldown_seconds: Option<u64>,
    retry_count: u32,
    retry_delay_secs: u64,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        apply_model_failover(
            &mut cfg, &model_id, fallback_target_model_id, fallback_strategies,
            fallback_strategies_enabled, cooldown_seconds, retry_count, retry_delay_secs,
        )?;
        persist(&app, &cfg)?;
    }
    state.health.reset(&model_id);
    Ok(())
}
```

Update every existing `apply_model_failover(...)` / `set_model_failover(...)` call site in `commands.rs` tests (search the file) to pass the two trailing args — use `2, 5` (defaults) unless the test asserts otherwise.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml commands`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(commands): set_model_failover accepts retry_count/retry_delay_secs"
```

---

### Task 5: Frontend — types, command wrapper, Fallback modal, Models round-trip

**Files:**
- Modify: `src/lib/types.ts` (`Model` ~L19-27), `src/lib/commands.ts` (`setModelFailover` ~L51-60), `src/views/Fallback.vue` (`FormState` ~L72, `blank` ~L79, `openEdit` ~L120, `doSave` ~L137, modal `cooldown` input ~L268), `src/views/Models.vue` (`FormState` ~L38, `blank` ~L53, `openEdit` ~L77, `buildModel` ~L94)

**Interfaces:**
- Consumes: Rust `set_model_failover` now requires `retryCount` / `retryDelaySecs` (Tauri camelCases JS arg keys to the snake_case Rust params).
- Produces: `Model.retry_count: number; Model.retry_delay_secs: number;` (required); `setModelFailover(modelId, target, strategies, enabled, cooldownSeconds, retryCount, retryDelaySecs)`.

- [ ] **Step 1: Add the TS fields (required, non-nullable)**

`src/lib/types.ts`, in `interface Model` (after `cooldown_seconds?: number | null;`):
```ts
  retry_count: number;       // required: Rust u32, always serialized (spec §5)
  retry_delay_secs: number;
```
(Intentionally non-optional, unlike `cooldown_seconds?: number | null` which mirrors an `Option<u64>`.)

- [ ] **Step 2: Extend the invoke wrapper**

`src/lib/commands.ts`, `setModelFailover`:
```ts
export const setModelFailover = (
  modelId: string,
  fallbackTargetModelId: string | null,
  fallbackStrategies: Strategy[],
  fallbackStrategiesEnabled: boolean,
  cooldownSeconds: number | null,
  retryCount: number,
  retryDelaySecs: number,
) =>
  invoke<void>("set_model_failover", {
    modelId,
    fallbackTargetModelId,
    fallbackStrategies,
    fallbackStrategiesEnabled,
    cooldownSeconds,
    retryCount,
    retryDelaySecs,
  });
```

- [ ] **Step 3: Fallback modal — form state, read, save, inputs**

`src/views/Fallback.vue`:
- `interface FormState` (~L72): add `retry_count: number; retry_delay_secs: number;`
- `blank()` (~L79): add `retry_count: 2, retry_delay_secs: 5,`
- `openEdit(m)` (~L120): add `form.retry_count = m.retry_count ?? 2; form.retry_delay_secs = m.retry_delay_secs ?? 5;`
- `doSave(...)` (~L137): pass them through — change the `config.setModelFailover(modelId, target, strategies, form.strategies_enabled, form.cooldown_seconds)` call to append `form.retry_count, form.retry_delay_secs`.
- Also the delete-confirm call (~L206 `config.setModelFailover(m.id, null, [], true, m.cooldown_seconds ?? null)`) must pass the model's existing retry values (`m.retry_count ?? 2, m.retry_delay_secs ?? 5`) so delete doesn't wipe them.
- Modal markup (~L268, under the 熔断冷却 NFormItem):
```vue
        <NFormItem label="临时限流重试">
          <NSpace align="center" :size="8">
            <NInputNumber v-model:value="form.retry_count" :min="0" :show-button="false" placeholder="2" style="width: 96px" />
            <span class="muted">次，每次间隔</span>
            <NInputNumber v-model:value="form.retry_delay_secs" :min="0" :show-button="false" placeholder="5" style="width: 96px" />
            <span class="muted">秒</span>
          </NSpace>
        </NFormItem>
        <div class="muted" style="margin: -4px 0 8px; font-size: 12px">
          仅临时限流（额度未耗尽）时原地等待重试；额度耗尽或重试耗尽后仍走熔断 fallback。0 = 关闭。
        </div>
```

- [ ] **Step 4: Models.vue — round-trip the new fields (don't wipe on edit)**

`src/views/Models.vue` (it already round-trips `cooldown_seconds`; mirror it):
- `FormState` (~L38): add `retry_count: number; retry_delay_secs: number;`
- `blank()` (~L53): add `retry_count: 2, retry_delay_secs: 5,`
- `openEdit(m)` (~L77): add `form.retry_count = m.retry_count ?? 2; form.retry_delay_secs = m.retry_delay_secs ?? 5;`
- `buildModel()` (~L94): add `retry_count: form.retry_count, retry_delay_secs: form.retry_delay_secs,` to the returned object.
- (Optional) list-row chip next to `cooldown {{ m.cooldown_seconds }}s` (~L235): `<span v-if="m.retry_count" class="muted mono">重试 {{ m.retry_count }}×{{ m.retry_delay_secs }}s</span>`.

- [ ] **Step 5: Type-check + build**

Run: `pnpm build`
Expected: PASS (`vue-tsc --noEmit` + vite build; the required TS fields are satisfied because every construction site now sets them).

- [ ] **Step 6: Manual smoke check (optional but recommended)**

`pnpm tauri dev` → Sources → 故障转移 → edit a model → set 重试 3×8s, save, reopen (value persists); Models edit modal preserves it. (Not automated; the type-check + backend tests cover correctness.)

- [ ] **Step 7: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/views/Fallback.vue src/views/Models.vue
git commit -m "feat(ui): per-model transient-limit retry config (Fallback modal + Models round-trip)"
```

---

## Self-Review (run before handoff)

1. **Spec coverage:**
   - §3 data model → Task 1. §4.1 `AttemptOutcome` + `stream_attempt` → Task 2. §4.2 `attempt_with_retry` → Task 3 (Steps 4–6). §4.3 per-hop wiring → Task 3 (Steps 5–6). §5 commands + frontend → Tasks 4–5. §6 retry log → Task 3 Step 4 (`tracing::info! "retry (transient)"`). §7 tests → Task 3 Steps 1, 10 (+ relies on existing Exhausted/`retry_count:0` tests). §8 boundaries → encoded in `attempt_with_retry` logic (Exhausted/no-retry-on-content/no-re-query) + `mk_model`. No spec section unowned.
2. **Placeholder scan:** none — every code step shows real code; wiremock ordering caveat is explicit with a fallback action.
3. **Type consistency:** `AttemptOutcome` (Task 2) used by `select_and_call`, `stream_attempt`, `attempt_with_retry` (Task 3) — same name throughout. `AttemptKind` / `AttemptWithRetry` defined and consumed in Task 3. `Recover` reused, not re-derived. TS `retry_count`/`retry_delay_secs` match Rust field names (snake_case, no serde rename). `setModelFailover` arg order matches the Rust command param order.
