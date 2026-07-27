# SwitchLM Plan 3 - Resilience & Usage Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the resilience + usage layer on top of Plan 2's protocol layer: a per-model circuit breaker, an `ErrorAdapter` that detects provider-specific rate-limit signals, a fallback walk over `fallback_target_model_id`, usage adapters (智谱 real, 火山 stubbed), and a shared `dispatch` that both edges call - fixing the per-protocol upstream URL and wiring breaker + fallback into the data path so Claude Code keeps working when a model 429s.

**Architecture:** A new `proxy/health.rs` owns in-memory breaker state (`ModelHealth` per model, separate from `AppConfig`). A new `proxy/error_adapter.rs` classifies upstream errors. A new `proxy/dispatch.rs` unifies the two edges: resolve Profile -> Model -> check breaker -> pick backend (same-protocol passthrough vs translate) -> call upstream -> on rate-limit (pre-content) trip the breaker + walk the fallback chain. A new `usage/` module exposes `UsageProvider` (智谱 real via the BigModel quota endpoint, 火山 `UnsupportedByProvider`); its `reset_at` feeds the breaker's `recover_at`. Both edges become thin wrappers over `dispatch`.

**Tech Stack:** Rust (edition 2021), axum 0.7, reqwest 0.12, serde_json, plus existing Plan 2 deps (`eventsource-stream`, `futures`, `async-stream`). No new crates.

## Scope of THIS plan

**Covers:**
- §3.5 Fallback + `ErrorAdapter` (provider-specific rate-limit detection; boundary = "no content forwarded yet"; first-SSE-event rate-limit still fallback-able).
- §4 Circuit breaker (`ModelHealth`, `cooling_down`/`recover_at`/`tripped_at`, time-based auto-recover, reset on edit, route-around cooling models).
- §6 Usage subsystem (`UsageProvider` trait + `UsageSnapshot`/`UsageError` + 60s cache + registry; `ZhipuUsageProvider` real; `VolcengineUsageProvider` stub; `reset_at` -> breaker `recover_at`).
- §3.3 re-match protocol per fallback model; cycle detection + max hops.
- Common-dispatch refactor (both edges call `dispatch`); fixes `join_url` per-protocol path (OpenAI `/chat/completions`, Anthropic `/v1/messages`).
- Management commands: `get_usage` / `get_all_usage` / `get_fallback_map` / `set_model_fallback` / `get_model_health`.

**DEFERRED (later plans / future):**
- OpenAI-client ↔ Anthropic-backend **reverse** translation (Plan 2 deferral; dispatch treats "OpenAI client + Anthropic-only backend" as `NoBackend`).
- Real 火山 usage impl (user still researching the endpoint) - `VolcengineUsageProvider` returns `UnsupportedByProvider` for now.
- Tauri tray / Vue UI -> **Plan 4**.
- Proactive quota pre-emptive tripping, multi-key rotation, network-error fallback -> §11 out of scope.

> **Credential note:** 智谱's quota endpoint uses the inference `api_key` (Bearer), NOT `UsageCreds` - so 智谱's `Provider.usage_creds` stays `None`. `VolcengineUsageProvider` would use `UsageCreds` (AK/SK) when implemented. The `UsageProvider::query` signature takes both `api_key: Option<&str>` and `usage_creds: Option<&UsageCreds>` so each provider uses what it needs.

## Global Constraints

- Built on **Plan 2** (`master`): `AppState`/`AppStateInner`, `resolve_model`, `BackendConfig`, `ProxyError`, both edges, `translate/*`. Branch from `master`.
- **Default port 6950** unchanged.
- **No new secrets/config schema** - reuses Plan 1's `Provider.usage_creds`/`UsageCreds`, `Model.cooldown_seconds`, `Model.fallback_target_model_id` (all already modeled).
- **Breaker state is runtime-only**, in `AppState` (NOT in `AppConfig`/disk). Restart -> all `cooling_down=false` (§4.1). Editing a model resets its health (§4.1) - handled in `set_model_fallback`/upsert (best-effort: reset on config write paths added here; full reset-on-edit lands with Plan 4 CRUD).
- **Response `model`** ALWAYS echoes the agent's incoming name (Profile name) on every path, even when fallback served the request.
- **Outbound `model`** ALWAYS the resolved backend's `upstream_model_id`.
- **Time**: inject a `Clock` trait (epoch secs, i64) so breaker logic is deterministic in tests; production uses `SystemClock` (`SystemTime::now()`).
- **Max fallback hops**: 8 (configurable constant). Cycle detection via a `visited: HashSet<String>` of model ids.
- **Commits**: conventional commits, one logical change per commit. `cargo test --manifest-path src-tauri/Cargo.toml` green before each commit.

## File Structure

```
src-tauri/src/
├─ proxy/
│  ├─ mod.rs               (MODIFY: declare health, error_adapter, dispatch)
│  ├─ error.rs             (MODIFY: add FallbackExhausted + RateLimited variants)
│  ├─ health.rs            (CREATE: Clock, ModelHealth, HealthRegistry)
│  ├─ error_adapter.rs     (CREATE: is_rate_limit_error + SSE-event helper)
│  ├─ dispatch.rs          (CREATE: unified dispatch + fallback walk + stream boundary)
│  ├─ openai_edge.rs       (MODIFY: thin wrapper -> dispatch)
│  ├─ anthropic_edge.rs    (MODIFY: thin wrapper -> dispatch)
│  └─ state.rs             (MODIFY: add health, clock, usage_cache to AppStateInner)
├─ usage/
│  ├─ mod.rs               (CREATE: UsageProvider trait, UsageSnapshot, UsageError, UsageCache, registry)
│  ├─ zhipu.rs             (CREATE: ZhipuUsageProvider - real)
│  └─ volcengine.rs        (CREATE: VolcengineUsageProvider - UnsupportedByProvider stub)
├─ lib.rs                  (MODIFY: pub mod usage; wire AppState fields)
└─ commands.rs             (MODIFY: add usage/fallback/health commands)
```

Responsibilities: `health.rs` + `error_adapter.rs` + `usage/*` = pure/unit-tested (usage does HTTP but is mock-tested); `dispatch.rs` = the integration point (HTTP + routing + breaker + fallback); edges = thin.

---

### Task 1: Circuit breaker state + Clock (`proxy/health.rs`)

**Files:**
- Create: `src-tauri/src/proxy/health.rs`
- Modify: `src-tauri/src/proxy/mod.rs`, `src-tauri/src/proxy/state.rs`, `src-tauri/src/lib.rs`, edge test helpers (add `clock`/`health` to constructed `AppStateInner`)

**Interfaces:**
- Produces:
  - `pub trait Clock: Send + Sync { fn now_secs(&self) -> i64; }`
  - `pub struct SystemClock;` (impl Clock via `SystemTime::now()`)
  - `pub struct FakeClock { inner: Mutex<i64> }` (impl Clock; `pub fn new(secs)`, `pub fn advance(&self, secs)`)
  - `#[derive(Clone, Debug, Default, PartialEq)] pub struct ModelHealth { pub cooling_down: bool, pub recover_at: Option<i64>, pub tripped_at: Option<i64> }`
  - `#[derive(Default)] pub struct HealthRegistry { inner: Mutex<HashMap<String, ModelHealth>> }` with:
    - `pub fn get(&self, model_id: &str) -> ModelHealth` (default if absent)
    - `pub fn is_cooling(&self, model_id: &str, now: i64) -> bool` (false if `!cooling_down`; true if `cooling_down && now < recover_at`)
    - `pub fn recover_if_due(&self, model_id: &str, now: i64) -> bool` (if `cooling_down && recover_at <= Some(now)` -> set `cooling_down=false`, clear recover_at/tripped_at, return true; else false)
    - `pub fn trip(&self, model_id: &str, recover_at: Option<i64>, now: i64)` (set `cooling_down=true`, `recover_at`, `tripped_at=Some(now)`)
    - `pub fn reset(&self, model_id: &str)` (remove entry -> back to default healthy)
    - `pub fn snapshot(&self) -> HashMap<String, ModelHealth>` (for the `get_model_health` command)

- [ ] **Step 1: Write failing tests** - `trip` -> `is_cooling` true; `recover_if_due` past `recover_at` -> cooling false; `reset` -> healthy; absent model -> default healthy; `FakeClock` advance changes `is_cooling`.
- [ ] **Step 2: Run, verify RED.**
- [ ] **Step 3: Implement** `health.rs` per interfaces.
- [ ] **Step 4: Wire into `AppStateInner`**: add `pub health: HealthRegistry` and `pub clock: Arc<dyn Clock>`. Update `state.rs` `load`, `lib.rs` `run` (construct `SystemClock` + default `HealthRegistry`), and all test helpers (`state_with_openai_backend`, `test_state`, `proxy::state::tests`, `proxy::resolve` unaffected). Add `pub mod health;` + re-exports to `proxy/mod.rs`.
- [ ] **Step 5: Run all tests, verify GREEN.**
- [ ] **Step 6: Commit** `feat(proxy): per-model circuit breaker (ModelHealth + Clock + HealthRegistry)`.

---

### Task 2: ErrorAdapter (`proxy/error_adapter.rs`)

**Files:**
- Create: `src-tauri/src/proxy/error_adapter.rs`, modify `proxy/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn is_rate_limit_error(provider_id: &str, status: Option<u16>, body: &str) -> bool`
    - `status == Some(429)` -> true
    - parse `body` as JSON; 智谱: `error.code == 1214` or top-level `code == 1214`; 火山: `error.code == "RateLimitExceeded"` / `"ModelAccessDenied"` / `"_quota_exceeded"`; keywords (`rate_limit`, `insufficient`, `usage limited`, `资源耗尽`, `quota`) in `error.message` -> true (best-effort)
    - non-JSON body -> only the 429 status check applies
  - `pub fn sse_event_is_rate_limit(provider_id: &str, event_data: &str) -> bool` - same logic on one SSE `data:` payload (an OpenAI/Anthropic error event is itself a JSON object).

- [ ] **Step 1: Write failing tests**: 智谱 200 + `{"error":{"code":1214}}` -> true; 429 -> true; 401 -> false; 火山 `{"error":{"code":"RateLimitExceeded"}}` -> true; keyword `usage limited` -> true; non-JSON + 500 -> false; SSE event variant.
- [ ] **Step 2: Run, verify RED.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests, verify GREEN.**
- [ ] **Step 5: Commit** `feat(proxy): ErrorAdapter - provider-specific rate-limit detection`.

---

### Task 3: Usage subsystem (`usage/` module)

**Files:**
- Create: `src-tauri/src/usage/mod.rs`, `src-tauri/src/usage/zhipu.rs`, `src-tauri/src/usage/volcengine.rs`; modify `src-tauri/src/lib.rs` (`pub mod usage;`), `src-tauri/src/proxy/state.rs` (add `usage_cache`).

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, Serialize)] pub struct UsageSnapshot { pub used: Option<f64>, pub total: Option<f64>, pub remaining: Option<f64>, pub reset_at: Option<i64>, pub unit: String, pub raw_summary: Option<String> }`
  - `#[derive(Debug, Error)] pub enum UsageError { NotConfigured, UnsupportedByProvider, Network(String), AuthFailed, Parse(String) }`
  - `#[async_trait] pub trait UsageProvider: Send + Sync { async fn query(&self, api_key: Option<&str>, usage_creds: Option<&UsageCreds>, base_url: &str) -> Result<UsageSnapshot, UsageError>; }`
    - (add `async-trait` dep if not present; check Cargo.toml - if absent, use it; it's a common dep. If avoiding new deps, model as a fn pointer / enum dispatch instead - **decision: add `async-trait = "0.1"`**.)
  - `pub struct UsageCache { inner: Mutex<HashMap<String, (Instant, UsageSnapshot)>>, ttl: Duration }` - `get(provider_id, now)`, `set(provider_id, snapshot, now)`, `invalidate(provider_id)`.
  - `pub fn usage_provider_for(provider_id: &str) -> Option<Box<dyn UsageProvider>>` - `"zhipu"` -> `ZhipuUsageProvider`, `"volcengine"` -> `VolcengineUsageProvider`, else None.
  - `ZhipuUsageProvider`: `GET {host}/api/monitor/usage/quota/limit` (host derived from `base_url`: scheme://host[:port]); `Authorization: Bearer {api_key}`; `api_key` None -> `NotConfigured`. Parse `data.limits[]` for `type=="TOKENS_LIMIT"`; `percentage` -> `used=percentage, total=100, remaining=100-percentage, unit="%"`; `nextResetTime` (ISO 8601 string) -> parse to epoch secs -> `reset_at`; `raw_summary` = the limit JSON. HTTP/parse failure -> `Network`/`Parse`/`AuthFailed` (401/403).
  - `VolcengineUsageProvider`: `query` returns `Err(UsageError::UnsupportedByProvider)` (TODO comment: real impl needs AK/SK signing; user researching).
- [ ] **Step 1: Write failing tests** (wiremock for 智谱): mock quota endpoint returning `{"data":{"limits":[{"type":"TOKENS_LIMIT","percentage":42,"nextResetTime":"2026-07-27T12:00:00Z"}]}}` -> snapshot `used=42, remaining=58, reset_at=Some(<epoch>), unit="%"`; missing TOKENS_LIMIT -> `Parse`; 401 -> `AuthFailed`; `VolcengineUsageProvider` -> `UnsupportedByProvider`; `UsageCache` TTL hit/miss.
- [ ] **Step 2: Run, verify RED.**
- [ ] **Step 3: Implement.** Add `usage_cache: UsageCache` to `AppStateInner` (default 60s TTL); update constructors.
- [ ] **Step 4: Run all tests, verify GREEN.**
- [ ] **Step 5: Commit** `feat(usage): UsageProvider trait + Zhipu quota adapter + cache (Volcengine stubbed)`.

---

### Task 4: Shared dispatch + per-protocol URL (`proxy/dispatch.rs`) - refactor, no behavior change

**Files:**
- Create: `src-tauri/src/proxy/dispatch.rs`; modify `proxy/mod.rs`, `openai_edge.rs`, `anthropic_edge.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy)] pub enum ClientProtocol { OpenAI, Anthropic }`
  - `pub async fn dispatch(state: AppState, body: Bytes, protocol: ClientProtocol) -> Result<Response<Body>, ProxyError>` - the unified entry. **Task 4 scope: move the existing edge logic into `dispatch` WITHOUT breaker/fallback** (those land in Tasks 5-6), preserving exact current behavior. Both edges shrink to:
    ```rust
    pub async fn chat_completions(State(state): State<AppState>, body: Bytes) -> Result<Response<Body>, ProxyError> {
        dispatch::dispatch(state, body, ClientProtocol::OpenAI).await
    }
    pub async fn messages(...) -> ... { dispatch::dispatch(state, body, ClientProtocol::Anthropic).await }
    ```
  - `fn join_url(base_url: &str, protocol: BackendProtocol) -> String` - **FIX**: OpenAI backend -> `{base}/chat/completions`; Anthropic backend -> `{base}/v1/messages`. (Replaces the shared `/chat/completions`-only `join_url`; the passthrough-path bug from Plan 2 is fixed here.)

- `dispatch` (Task 4 shape, no fallback yet):
  1. Parse body JSON (invalid -> `Translation`).
  2. Snapshot config: empty -> `NotConfigured`; `resolve_model`; capture `requested_name` (echo), `is_stream`.
  3. Pick backend by `ClientProtocol` × model backends (§3.3, minus reverse-translate): OpenAI-client+openai -> OpenAI passthrough; Anthropic-client+anthropic -> Anthropic passthrough; Anthropic-client+openai -> translate (`anthropic_to_openai`); else `NoBackend`.
  4. Build upstream request (model -> `upstream_model_id`, `stream` set, `join_url` per backend protocol).
  5. Fetch key; call upstream.
  6. Non-stream: buffer, echo model, return (OpenAI edge rewrites model via the `is_object()` guard from Plan 2; Anthropic translate uses `openai_to_anthropic`; Anthropic passthrough rewrites `model`).
  7. Stream: translate path -> `translate_stream`; passthrough -> verbatim bytes. (Same as Plan 2.)

- [ ] **Step 1: No new test** - this is a pure refactor; the existing edge integration tests (`rewrites_model_forwards_and_passes_body`, `openai_edge_echoes_requested_model_nonstream`, `anthropic_edge_translates_non_stream`, `anthropic_edge_translates_stream`) are the regression net.
- [ ] **Step 2: Implement `dispatch`** by moving logic out of both edges; add `join_url` per-protocol. If a test helper breaks on the `join_url` change (passthrough now posts `/v1/messages`), update the mock path - but note there's no passthrough integration test yet (the passthrough path was untested in Plan 2), so this should be safe.
- [ ] **Step 3: Run all tests, verify GREEN** (24 tests, no regressions).
- [ ] **Step 4: Commit** `refactor(proxy): unify both edges into dispatch + per-protocol join_url`.

---

### Task 5: Breaker + fallback walk (non-stream)

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs`, `src-tauri/src/proxy/error.rs`

**Interfaces:**
- Adds to `ProxyError`: `#[error("all models exhausted (tried {tried:?}): {last_error}")] FallbackExhausted { tried: Vec<String>, last_error: String }` -> 502.
- `dispatch` non-stream path becomes a walk:
  ```
  visited = {}, model = resolved, hops = 0
  loop:
    if hops >= MAX_HOPS or model.id in visited -> FallbackExhausted (tried so far)
    visited.insert(model.id); hops += 1
    health.recover_if_due(model.id, now)
    if health.is_cooling(model.id, now):
        model = next_fallback(cfg, model)?  // None -> FallbackExhausted
        continue
    select backend; call upstream; buffer bytes
    if error_adapter::is_rate_limit_error(provider_id, status, body):
        recover_at = usage_reset_at(state, &model) or now + cooldown_seconds
        health.trip(model.id, recover_at, now)
        model = next_fallback(cfg, model)?  // None -> FallbackExhausted
        continue
    else:
        return echo_model(response)   // success or non-rate-limit passthrough
  ```
  - `next_fallback(cfg, model) -> Option<&Model>`: `model.fallback_target_model_id` -> find Model; None/missing -> None. (Cycle detection is the `visited` set; max hops the counter.)
  - `usage_reset_at(state, model) -> Option<i64>`: look up `usage_provider_for(provider_id)`, `query()` (via cache, ignore errors), return `snapshot.reset_at`. Failure -> None (caller falls back to `cooldown_seconds`).
  - Non-rate-limit upstream error (401/400/500/network): **return it as-is to the client** (no trip, no fallback) - §3.5 "透传".

- [ ] **Step 1: Write failing tests** (wiremock, FakeClock): model A 429 -> fallback model B 200, response served + A tripped (cooling); A->B->C all 429 -> `FallbackExhausted` (502) + all tripped; A 401 -> returned as-is (no trip, no fallback); A cooling (pre-tripped) -> skipped straight to B; cycle A->B->A breaks (FallbackExhausted, tried=[A,B]).
- [ ] **Step 2: Run, verify RED.**
- [ ] **Step 3: Implement** the walk + `usage_reset_at` + `FallbackExhausted`. Ensure existing non-stream tests still pass (they exercise the no-fallback happy path).
- [ ] **Step 4: Run all tests, verify GREEN.**
- [ ] **Step 5: Commit** `feat(proxy): circuit breaker + fallback walk (non-stream)`.

---

### Task 6: Stream boundary ("no content forwarded yet")

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs`

**Interfaces:**
- Streaming dispatch applies the same fallback walk, but the rate-limit check happens on the **first upstream SSE event** (before any content is forwarded):
  - Read the first event from `upstream.eventsource()`. If `sse_event_is_rate_limit(provider_id, &ev.data)` (and no content event was emitted yet) -> trip breaker + walk to next fallback model (re-call upstream, re-peek).
  - Once a non-rate-limit first event is seen -> **commit**: build the streaming response (translate via `StreamTranslator` seeded with the already-seen first chunk, or passthrough prepending the first event bytes) and return it. Subsequent mid-stream errors are forwarded to the client (no retry, §3.5).
  - `[DONE]` only (empty/no content) -> treat as success (return the stream).
- [ ] **Step 1: Write failing tests**: upstream SSE first event = `{"error":{"code":1214}}` (rate-limit) -> fallback to B whose stream is normal text -> response contains B's text + A tripped; upstream first event = normal content delta -> committed, second event a synthetic error -> forwarded (no fallback); non-stream tests unaffected.
- [ ] **Step 2: Run, verify RED.**
- [ ] **Step 3: Implement** the peek-then-commit stream path. Reuse `StreamTranslator`; for the translate path, feed the peeked first chunk into the translator before yielding subsequent frames. For passthrough, prepend the peeked event bytes to the forwarded stream.
- [ ] **Step 4: Run all tests, verify GREEN.**
- [ ] **Step 5: Commit** `feat(proxy): streaming fallback boundary (first-event rate-limit)`.

---

### Task 7: Management commands + AppState wiring + DoD

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`

**Interfaces:**
- Commands (thin wrappers over Plan 3 machinery):
  - `get_usage(state, provider_id) -> Result<UsageSnapshot, String>` - query via provider + cache; `UnsupportedByProvider`/`NotConfigured` -> Err string for UI.
  - `get_all_usage(state) -> Result<Vec<(String, UsageResult)>, String>` - all providers.
  - `get_fallback_map(state) -> Result<HashMap<String, Option<String>>, String>` - `{ model_id -> fallback_target_model_id }` from config (aggregated view, §7.3).
  - `set_model_fallback(state, model_id, target: Option<String>, app) -> Result<(), String>` - set `Model.fallback_target_model_id`, **reset that model's health**, persist.
  - `get_model_health(state) -> Result<HashMap<String, ModelHealthView>, String>` - `health.snapshot()` (for tray/UI cooling indicators).
- Register all in `lib.rs` `invoke_handler`.
- [ ] **Step 1: Write failing tests** for `get_fallback_map` + `set_model_fallback` (config round-trip + health reset) + `get_usage` (mocked provider via a test UsageProvider, or test the command with a zhipu wiremock). `get_model_health` reflects a tripped model.
- [ ] **Step 2: Run, verify RED.**
- [ ] **Step 3: Implement** commands + register.
- [ ] **Step 4: Run all tests, verify GREEN.**
- [ ] **Step 5: Commit** `feat(app): usage/fallback/health management commands`.

---

## Definition of Done (Plan 3)

- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` - all green (Plan 1+2's 24 + Plan 3's breaker/error-adapter/usage/dispatch/fallback/stream tests).
- [ ] Breaker: 429 trips -> cooling model skipped on next request -> auto-recovers at `recover_at`; `reset_at` (智谱 `nextResetTime`) used when available, else `cooldown_seconds`.
- [ ] ErrorAdapter: 智谱 200+`code:1214` / 429 / SSE first-event rate-limit -> fallback; 401/400/500 -> passthrough (no trip).
- [ ] Fallback: chain walk A->B->C; cycle + max-hops safe; exhaustion returns `FallbackExhausted` (502); response `model` still echoes the requested name.
- [ ] Stream boundary: first-event rate-limit -> fallback; content-then-error -> forwarded (no retry).
- [ ] `join_url` per-protocol: OpenAI `/chat/completions`, Anthropic `/v1/messages` (Plan 2 passthrough bug fixed).
- [ ] Usage: 智谱 quota query returns `percentage` + `reset_at`; 火山 returns `UnsupportedByProvider`; 60s cache; breaker consumes `reset_at`.
- [ ] Commands: `get_usage` / `get_all_usage` / `get_fallback_map` / `set_model_fallback` / `get_model_health` registered.
- [ ] E2E smoke (manual, real key): real Claude Code -> SwitchLM -> 智谱; force a 429 (or wait for window) -> observe fallback + tray cooling state.

## Hand-off to Plan 4

Plan 4 builds the UI on top of the now-complete data + resilience layer:
- Tauri tray (per-Profile backing switch + inline quota + cooling indicator via `get_model_health`/`get_all_usage`).
- Vue 6 pages consuming the Plan 3 commands.
- Autostart, port-change warning + copy-env, graceful shutdown.
- UI visual/wireframe design via `frontend-design` skill (deferred to this phase).
- Real 火山 usage impl (when the user provides the endpoint) slots into `VolcengineUsageProvider`.
