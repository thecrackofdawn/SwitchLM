# 时段故障转移策略（Time-based Failover Strategies）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend time-based model selection from the route entry layer to the failover layer, so a 429'd model fails over to a time-strategy-selected target, chained per-model; plus unify route/failover/overview display.

**Architecture:** Add `fallback_strategies` + `fallback_strategies_enabled` to `Model` (symmetric to `Profile.strategies`), reuse the existing `Strategy`/`strategy_matches`/`select_strategy` primitives. A new pure selector `model_fallback_target` (mirror of `profile_start_model`) is called by dispatch's `next_fallback_id` at every hop → time-aware chained failover. Three new commands expose config + the time-aware effective value. Frontend: extract a shared `StrategyEditor.vue`; redesign the route card + failover page (mirrors route page) + overview pipeline; unify "当前生效" wording.

**Tech Stack:** Rust (axum, tokio, serde), Tauri v2 commands; Vue 3 `<script setup>` + Pinia + Naive UI + pnpm.

**Spec:** `docs/superpowers/specs/2026-08-03-time-based-failover-strategies-design.md` (committed `f687b10`).

## Global Constraints

- Backend tests: `cargo test --manifest-path src-tauri/Cargo.toml` (co-located `#[cfg(test)] mod tests`; HTTP via `wiremock`; time via `FakeClock::set_local`, time-zone-independent).
- Frontend: package manager is **pnpm** — use `pnpm exec vue-tsc --noEmit` and `pnpm build` (never `npm`/`npx`).
- Logs must identify vendor + upstream model name, never the opaque `id`.
- Secrets (api keys, usage SKs) never touch `app_config.json` — OS keyring only. Route + failover config persist to `<app_data>/app_config.json` via existing `persist()`.
- Work directly on `main`; commit each task. Use explicit `git add <paths>` (never `git add -A` — sweeps untracked docs).
- New `fallback_*` fields are `#[serde(default)]` → old configs load as `[]`/`true` (backward compat is a tested requirement).

## File Structure

**Backend (Phase A — independently testable, cargo):**
- `src-tauri/src/config/types.rs` — `Model` gains `fallback_strategies` + `fallback_strategies_enabled` + `Default`.
- `src-tauri/src/proxy/strategies.rs` — new pure `model_fallback_target` (+ tests, incl. boundary).
- `src-tauri/src/proxy/dispatch.rs` — `next_fallback_id` time-aware; thread `now_local` (+ chain/boundary tests).
- `src-tauri/src/commands.rs` — generalize `normalize_and_validate_strategies`; `set_model_failover`, `set_model_fallback_strategies_enabled`, `model_effective_fallbacks`; `ModelEffectiveFallback`; extend `RouteEffective`.

**Frontend (Phase B — depends on Phase A commands):**
- `src/lib/types.ts` / `src/lib/commands.ts` / `src/stores/config.ts` — mirror types + command wrappers + effective-fallbacks state.
- `src/components/StrategyEditor.vue` — **NEW**, extracted shared strategy editor.
- `src/views/Profiles.vue` — route card redesign + terminology.
- `src/views/Fallback.vue` — page redesign mirroring the route page.
- `src/components/RouteLine.vue` + `src/views/Dashboard.vue` — overview pipeline `[路由]▶[当前生效]▶[故障转移]`.

---

# Phase A — Backend

## Task 1: `Model` data fields + backward-compat serde

**Files:**
- Modify: `src-tauri/src/config/types.rs` (`Model` struct ~L140; add `Default` to its derive line; add 2 fields; add default fn)
- Test: `src-tauri/src/config/types.rs` (append to `#[cfg(test)] mod tests`)
- Modify: every `Model { ... }` struct literal the compiler flags (≈13 sites: `commands.rs`, `dispatch.rs`, `resolve.rs`, `strategies.rs`, `anthropic_edge.rs`, `openai_edge.rs`, `e2e_zhipu.rs`, `types.rs` test) — append `, ..Default::default()`.

**Interfaces:**
- Produces: `Model { fallback_strategies: Vec<Strategy>, fallback_strategies_enabled: bool }` (both serde-defaulted); `Model: Default`.

- [ ] **Step 1: Add the failing test**

Append to `types.rs` `mod tests`:

```rust
    #[test]
    fn model_fallback_strategies_default_when_absent() {
        // Old config JSON (pre-feature) omits both fields; serde default fills [] / true.
        let json = r#"{"id":"m","provider_id":"zhipu","upstream_model_id":"glm-4.6"}"#;
        let model: Model = serde_json::from_str(json).unwrap();
        assert!(model.fallback_strategies.is_empty());
        assert!(model.fallback_strategies_enabled);
    }

    #[test]
    fn model_with_fallback_strategies_roundtrips() {
        let m = Model {
            id: "m".into(), provider_id: "zhipu".into(), upstream_model_id: "glm".into(),
            fallback_target_model_id: Some("m2".into()),
            fallback_strategies: vec![Strategy {
                id: "s".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy {
                    days_of_week: vec![1], time_start: 1320, time_end: 360, model_id: "m2".into(),
                }),
            }],
            fallback_strategies_enabled: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config::types::tests::model_fallback_strategies_default_when_absent`
Expected: compile error — no field `fallback_strategies` / no `Default` for `Model`.

- [ ] **Step 3: Implement — add fields + Default derive**

In `types.rs`, change the `Model` derive line and add the two fields + default fn:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Model {
    pub id: String,
    pub provider_id: String,
    #[serde(default)]
    pub source: ModelSource,
    #[serde(default)]
    pub upstream_model_id: String,
    #[serde(default)]
    pub cooldown_seconds: Option<u64>,
    #[serde(default)]
    pub fallback_target_model_id: Option<String>,
    /// 时段故障转移策略（复用 Strategy，与入口层 Profile.strategies 同构）。
    #[serde(default)]
    pub fallback_strategies: Vec<Strategy>,
    /// 总开关：false 跳过时段策略，直接走 fallback_target_model_id。默认 true。
    #[serde(default = "default_fb_strategies_enabled")]
    pub fallback_strategies_enabled: bool,
    #[serde(default)]
    pub context_size: Option<u32>,
}
fn default_fb_strategies_enabled() -> bool { true }
```

- [ ] **Step 4: Fix every flagged `Model { ... }` literal**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
For each error `missing field ... in initializer of Model`, append `, ..Default::default()` to that literal (the `mk_model` test helpers in `dispatch.rs:1010`, `commands.rs:1344` & `:1483`, `strategies.rs:155`, and the inline literals in `commands.rs:1222/1231`, `resolve.rs:51/110`, `anthropic_edge.rs:37`, `openai_edge.rs:39`, `e2e_zhipu.rs:56`, `types.rs:269`).
Expected: `cargo build` succeeds.

- [ ] **Step 5: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (incl. the two new tests + all existing `config::types` + `dispatch` + `strategies` tests, now green again).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/commands.rs src-tauri/src/proxy/dispatch.rs src-tauri/src/proxy/resolve.rs src-tauri/src/proxy/strategies.rs src-tauri/src/proxy/anthropic_edge.rs src-tauri/src/proxy/openai_edge.rs src-tauri/tests/e2e_zhipu.rs
git commit -m "feat(config): Model 加 fallback_strategies + fallback_strategies_enabled（serde 默认，向后兼容）"
```

---

## Task 2: `model_fallback_target` pure selector + boundary tests

**Files:**
- Modify: `src-tauri/src/proxy/strategies.rs` (add `model_fallback_target` after `profile_start_model`)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `select_strategy` (existing, same file), `Model.fallback_strategies` / `fallback_strategies_enabled` / `fallback_target_model_id` (Task 1), `LocalNow` (`proxy::health`).
- Produces: `pub fn model_fallback_target<'c>(model: &'c Model, cfg: &'c AppConfig, now: &LocalNow) -> (Option<&'c str>, Option<&'c str>)` — `(target_id, via_strategy_id)`. Used by Task 4 (`next_fallback_id`) and Task 6 (`model_effective_fallbacks`).

- [ ] **Step 1: Add the failing tests**

In `strategies.rs` `mod tests`, the existing `use crate::config::{AppConfig, Profile, ModelSource};` — add `Model` to it. Append:

```rust
    use crate::config::StrategyKind;

    fn fb_model(id: &str, target: Option<&str>, strats: Vec<Strategy>, enabled: bool) -> Model {
        Model {
            id: id.into(), provider_id: "pr".into(), source: ModelSource::Manual,
            upstream_model_id: id.into(), cooldown_seconds: None,
            fallback_target_model_id: target.map(String::from),
            fallback_strategies: strats, fallback_strategies_enabled: enabled,
            context_size: None,
        }
    }
    fn fb_strat(id: &str, days: &[u8], start: u16, end: u16, model: &str) -> Strategy {
        Strategy {
            id: id.into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: days.to_vec(), time_start: start, time_end: end, model_id: model.into(),
            }),
        }
    }

    #[test]
    fn model_fallback_target_strategy_hit() {
        // Strategy matches Tue all-day -> mB; default mC.
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mB", "pr", "uB"));
        cfg.models.push(mk_model("mC", "pr", "uC"));
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[2], 0, 1439, "mB")], true);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, Some("mB"));
        assert_eq!(via, Some("s"));
    }

    #[test]
    fn model_fallback_target_master_off_uses_default() {
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mB", "pr", "uB"));
        cfg.models.push(mk_model("mC", "pr", "uC"));
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[2], 0, 1439, "mB")], false);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, Some("mC"));
        assert_eq!(via, None);
    }

    #[test]
    fn model_fallback_target_deleted_strategy_target_falls_to_default() {
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mC", "pr", "uC")); // mB does not exist
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[2], 0, 1439, "mB")], true);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, Some("mC"));
        assert_eq!(via, None);
    }

    #[test]
    fn model_fallback_target_no_default_no_hit_is_none() {
        let cfg = cfg_with(Profile::default());
        let m = fb_model("mA", None, vec![], true);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, None);
        assert_eq!(via, None);
    }

    #[test]
    fn model_fallback_target_boundary_22_to_06() {
        // Window 22:00(1320)–06:00(360) -> mB, all weekdays; default mC.
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mB", "pr", "uB"));
        cfg.models.push(mk_model("mC", "pr", "uC"));
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[1,2,3,4,5,6,7], 1320, 360, "mB")], true);
        // 21:59(1319) -> default mC; 22:00(1320) -> mB (evening, today in set)
        assert_eq!(model_fallback_target(&m, &cfg, &now(2, 1319)), (Some("mC"), None));
        assert_eq!(model_fallback_target(&m, &cfg, &now(2, 1320)), (Some("mB"), Some("s")));
        // 05:59(359) -> mB (morning, prev_weekday in set since all days); 06:00(360) -> default mC
        assert_eq!(model_fallback_target(&m, &cfg, &now(3, 359)), (Some("mB"), Some("s")));
        assert_eq!(model_fallback_target(&m, &cfg, &now(3, 360)), (Some("mC"), None));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml strategies::tests::model_fallback_target`
Expected: FAIL — `cannot find function model_fallback_target`.

- [ ] **Step 3: Implement `model_fallback_target`**

In `strategies.rs`, immediately after `profile_start_model`:

```rust
/// Single source of truth for "which model does this model fail over to, and why".
/// Master off / no match / strategy model deleted → `fallback_target_model_id` (via = None);
/// no default target → (None, None). Mirror of `profile_start_model` for the failover layer.
pub fn model_fallback_target<'c>(
    model: &'c Model,
    cfg: &'c AppConfig,
    now: &LocalNow,
) -> (Option<&'c str>, Option<&'c str>) {
    if model.fallback_strategies_enabled {
        if let Some(s) = select_strategy(&model.fallback_strategies, now) {
            let mid: &str = match &s.kind { StrategyKind::Time(t) => &t.model_id };
            if cfg.models.iter().any(|m| m.id == mid) {
                return (Some(mid), Some(&s.id));
            }
        }
    }
    (model.fallback_target_model_id.as_deref(), None)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml strategies::tests::model_fallback_target`
Expected: PASS (all 5 cases incl. boundary).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/strategies.rs
git commit -m "feat(proxy): model_fallback_target 时间感知故障转移选择器（含 22:00-06:00 临界点测试）"
```

---

## Task 3: Generalize `normalize_and_validate_strategies` to any `&mut [Strategy]`

**Files:**
- Modify: `src-tauri/src/commands.rs` — change `normalize_and_validate_strategies` signature + the `upsert_profile` call site (L198).
- Test: same file `#[cfg(test)] mod tests`.

**Interfaces:**
- Consumes: `Strategy`/`StrategyKind`/`AppConfig` (existing).
- Produces: `fn normalize_and_validate_strategies(cfg: &AppConfig, strategies: &mut [Strategy]) -> Result<(), String>` — used by `upsert_profile` (this task) and `set_model_failover` (Task 5).

- [ ] **Step 1: Add a regression + standalone test**

In `commands.rs` `mod tests` (near other validation tests). Find an existing test that asserts `upsert_profile` rejects bad strategies; if none exists, add:

```rust
    #[test]
    fn normalize_and_validate_strategies_rejects_and_normalizes() {
        let cfg = AppConfig::default(); // no models -> any strategy model_id is "not found"
        let mut s = vec![Strategy {
            id: "s".into(), priority: 11, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2, 2, 1], time_start: 1320, time_end: 1320, model_id: "mX".into(),
            }),
        }];
        // priority out of range
        assert!(normalize_and_validate_strategies(&cfg, &mut s).is_err());
        // start==end (non-zero) -> rejected
        s[0].priority = 1;
        assert!(normalize_and_validate_strategies(&cfg, &mut s).is_err());
        // fix window, but model_id still missing -> rejected
        s[0].kind.as_time().unwrap().time_end = 360;
        assert!(normalize_and_validate_strategies(&cfg, &mut s).is_err());
        // days dedup+sort applied (no error path beyond model_id here, but days are normalized)
        assert_eq!(s[0].kind.as_time().unwrap().days_of_week, vec![1, 2]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml commands::tests::normalize_and_validate_strategies_rejects_and_normalizes`
Expected: FAIL — signature mismatch (`&mut Profile` vs `&mut [Strategy]`).

- [ ] **Step 3: Generalize the function + call site**

Change `normalize_and_validate_strategies` (commands.rs L903) to take `strategies: &mut [Strategy]` and iterate that slice directly (replace `for s in &mut profile.strategies` with `for s in strategies.iter_mut()`):

```rust
fn normalize_and_validate_strategies(cfg: &AppConfig, strategies: &mut [Strategy]) -> Result<(), String> {
    for s in strategies.iter_mut() {
        let t = match &mut s.kind { StrategyKind::Time(t) => t };
        if !(1..=10).contains(&s.priority) {
            return Err("策略优先级必须在 1~10 之间".into());
        }
        t.days_of_week.sort_unstable();
        t.days_of_week.dedup();
        if t.days_of_week.is_empty() {
            return Err("策略至少需选择一个星期".into());
        }
        if t.days_of_week.iter().any(|d| !(1..=7).contains(d)) {
            return Err("星期必须在 1~7 之间".into());
        }
        if t.time_start > 1439 || t.time_end > 1439 {
            return Err("时间必须在 00:00~23:59 之间".into());
        }
        if t.time_start == t.time_end && t.time_start != 0 {
            return Err("开始时间不能等于结束时间（如需全天，请将起止均设为 00:00）".into());
        }
        if !cfg.models.iter().any(|m| m.id == t.model_id) {
            return Err(format!("策略目标模型「{}」不存在", t.model_id));
        }
    }
    Ok(())
}
```

Update the `upsert_profile` call site (L198):
```rust
        normalize_and_validate_strategies(&cfg, &mut profile.strategies)?;
```

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (new test + all existing `commands` tests; profile strategy validation unchanged).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "refactor(commands): normalize_and_validate_strategies 泛化到任意 Vec<Strategy>"
```

---

## Task 4: dispatch time-aware `next_fallback_id` (chained failover)

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` — `dispatch` (hoist `now_local`), `dispatch_non_stream` + `dispatch_stream` (accept `now_local: &LocalNow`), `next_fallback_id` (use `model_fallback_target`); update all `next_fallback_id(state, &current)` call sites.
- Test: same file `#[cfg(test)] mod tests`.

**Interfaces:**
- Consumes: `model_fallback_target` (Task 2), `LocalNow` (`proxy::health`).
- Produces: time-aware chained failover on the dispatch hot path.

- [ ] **Step 1: Add the failing integration tests**

In `dispatch.rs` `mod tests`, append (reuse `mk_model`, `chain_state` helpers; `FakeClock::set_local`):

```rust
    #[tokio::test]
    async fn failover_time_window_22_picks_qwen_else_volcano() {
        // GLM default fallback target = m_volcano; GLM also has a time strategy 22:00-06:00 -> m_qwen.
        let mock_glm = MockServer::start().await;     // 429
        let mock_qwen = MockServer::start().await;     // 200
        let mock_volcano = MockServer::start().await;  // 200
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_glm).await;
        for m in [&mock_qwen, &mock_volcano] {
            Mock::given(method("POST")).and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"ok"}}]})))
                .mount(m).await;
        }

        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider { id: "pg".into(), vendor: "zhipu".into(), display_name: "GLM".into(), openai_base_url: Some(mock_glm.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pq".into(), vendor: "pq".into(), display_name: "Qwen".into(), openai_base_url: Some(mock_qwen.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pv".into(), vendor: "pv".into(), display_name: "Volcano".into(), openai_base_url: Some(mock_volcano.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.models.push(Model {
            id: "m_glm".into(), provider_id: "pg".into(), source: ModelSource::Manual, upstream_model_id: "glm".into(),
            cooldown_seconds: Some(300), fallback_target_model_id: Some("m_volcano".into()),
            fallback_strategies: vec![Strategy { id: "s".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![1,2,3,4,5,6,7], time_start: 1320, time_end: 360, model_id: "m_qwen".into() }) }],
            fallback_strategies_enabled: true, context_size: None,
        });
        cfg.models.push(Model { id: "m_qwen".into(), provider_id: "pq".into(), source: ModelSource::Manual, upstream_model_id: "qwen".into(), cooldown_seconds: None, fallback_target_model_id: None, fallback_strategies: vec![], fallback_strategies_enabled: true, context_size: None });
        cfg.models.push(Model { id: "m_volcano".into(), provider_id: "pv".into(), source: ModelSource::Manual, upstream_model_id: "volcano".into(), cooldown_seconds: None, fallback_target_model_id: None, fallback_strategies: vec![], fallback_strategies_enabled: true, context_size: None });
        for pid in ["pg","pq","pv"] { secrets.set_key(pid, "sk-test").unwrap(); }
        cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_glm".into(), ..Default::default() });

        async fn run_at(cfg: AppConfig, secrets: SecretStoreHandle, w: u8, min: u16) -> (StatusCode, String) {
            let clock = Arc::new(FakeClock::new(1000)); clock.set_local(w, min);
            let state: AppState = Arc::new(AppStateInner { config: tokio::sync::RwLock::new(cfg), secrets, catalog: ModelCatalog::default(), health: Default::default(), clock, usage_cache: Default::default(), bound_port: std::sync::Mutex::new(None), server_handle: std::sync::Mutex::new(None), bind_error: std::sync::Mutex::new(None), polling_handle: std::sync::Mutex::new(None) });
            let resp = build_router(state.clone()).oneshot(oai_post()).await.unwrap();
            let body = body_str(resp).await; // borrow body_str helper from this module
            // re-extract status by re-running is not possible; return (..) placeholder — see note below
            unimplemented!()
        }
        // NOTE: oai_post/body_str return only body; assert via the served model tag in logs is brittle.
        // Instead assert the served upstream by giving qwen/volcano distinct success bodies:
        // (set qwen body "from-qwen", volcano body "from-volcano" above instead of the shared loop)
    }
```

> **Implementer note:** the helper sketch above is intentionally not finalized — the cleanest assertion is to give `m_qwen` and `m_volcano` **distinct success bodies** (`from-qwen` / `from-volcano`) instead of the shared mount loop, then assert `body.contains("from-qwen")` at 22:00 and `body.contains("from-volcano")` at 21:59/06:00. Finalize the two mock bodies + 3 assertions (21:59→volcano, 22:00→qwen, 06:00→volcano) following the existing `fallback_on_rate_limit` test's shape. The point under test: `next_fallback_id` returns the time-strategy target at 22:00, the default at 21:59/06:00.

Also add a chained test:

```rust
    #[tokio::test]
    async fn failover_chain_uses_each_models_own_strategy() {
        // GLM 429 -> (GLM strategy, all-day) -> mB; mB 429 -> (mB strategy, all-day) -> mC; mC 200.
        // Verifies each hop reads the FAILING model's own fallback config.
        // Build 3 providers (glm/mB/mC) each on its own MockServer; glm & mB return 429, mC 200.
        // Configure: m_glm.fallback_strategies=[all-day->mB], mB.fallback_strategies=[all-day->mC].
        // Assert served body is mC's, and both m_glm & mB tripped.
        // Follow the existing `strategy_entry_model_rate_limits_walks_its_chain` test's structure.
    }
```

(Flesh out both tests fully per the note + the existing `strategy_entry_model_rate_limits_walks_its_chain` / `fallback_on_rate_limit` patterns — distinct success bodies, `state.health.is_cooling` assertions.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml dispatch::tests::failover_time_window`
Expected: FAIL — `next_fallback_id` still returns the static `fallback_target_model_id` at all times (assertion mismatch: 22:00 returns volcano, not qwen).

- [ ] **Step 3: Thread `now_local` + make `next_fallback_id` time-aware**

(a) In `dispatch()` (L51), hoist the local time out of the resolve block so it spans the whole request. Change:
```rust
    let (start_model_id, echo_model, is_stream, vendor, upstream_model) = {
        let now = state.clock.now_local();
        let cfg = state.config.read().await;
        ...
        let model = resolve_model(&cfg, requested_model, &now)?;
        ...
    };
```
to compute `now_local` once before the block and reuse it:
```rust
    let now_local = state.clock.now_local();
    let (start_model_id, echo_model, is_stream, vendor, upstream_model) = {
        let cfg = state.config.read().await;
        ...
        let model = resolve_model(&cfg, requested_model, &now_local)?;
        ...
    };
```
Then pass `&now_local` into both `dispatch_stream(...)` and `dispatch_non_stream(...)` calls (add the arg).

(b) Add `now_local: &LocalNow` param to `dispatch_non_stream` and `dispatch_stream` signatures (after `outcome`). Import `LocalNow` (`use crate::proxy::health::LocalNow;` — already imported via strategies if not, add it).

(c) Change every `next_fallback_id(state, &current)` / `next_fallback_id(state, &current).await` call site (in both walk functions and the `hop_and_advance!` macro) to `next_fallback_id(state, &current, now_local)`.

(d) Rewrite `next_fallback_id` (L826):
```rust
/// Time-aware next failover target for `model_id`, if configured and existing. Reads the FAILING
/// model's own `fallback_strategies`/`fallback_target_model_id` via `model_fallback_target`.
async fn next_fallback_id(state: &AppState, model_id: &str, now: &LocalNow) -> Option<String> {
    let cfg = state.config.read().await;
    let m = cfg.models.iter().find(|m| m.id == model_id)?;
    let (target, _via) = model_fallback_target(m, &cfg, now);
    let target = target?;
    if cfg.models.iter().any(|x| x.id == target) { Some(target.to_string()) } else { None }
}
```
Add `use crate::proxy::strategies::model_fallback_target;` to dispatch.rs imports (it already imports `resolve_model` from `proxy::resolve`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — boundary (22:00→qwen, 21:59/06:00→volcano), chain (each hop its own config), and all existing dispatch tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): 故障转移链式时间感知——每跳读失败模型自己的 fallback 策略"
```

---

## Task 5: `set_model_failover` + `set_model_fallback_strategies_enabled` commands

**Files:**
- Modify: `src-tauri/src/commands.rs` — add 2 commands near `set_model_fallback` (L481); register both in `src-tauri/src/lib.rs` `invoke_handler`.
- Test: same file `#[cfg(test)] mod tests` (pure helper).

**Interfaces:**
- Consumes: `normalize_and_validate_strategies` (Task 3), `persist`, `state.health.reset`.
- Produces: `set_model_failover(model_id, fallback_target_model_id, fallback_strategies, fallback_strategies_enabled, cooldown_seconds)`; `set_model_fallback_strategies_enabled(model_id, enabled)`.

- [ ] **Step 1: Add a pure-helper test**

Extract the apply logic into a pure fn so it's testable without `State`/`AppHandle` (mirrors `fallback_map`/`conflicting_model`). In `commands.rs` `mod tests`:

```rust
    #[test]
    fn apply_model_failover_validates_and_writes() {
        let mut cfg = AppConfig::default();
        cfg.models.push(Model { id: "m".into(), provider_id: "p".into(), upstream_model_id: "u".into(), ..Default::default() });
        cfg.models.push(Model { id: "t".into(), provider_id: "p".into(), upstream_model_id: "ut".into(), ..Default::default() });
        // bad priority -> rejected, cfg unchanged
        let bad = vec![Strategy { id: "s".into(), priority: 99, enabled: true,
            kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![1], time_start: 0, time_end: 1439, model_id: "t".into() }) }];
        assert!(apply_model_failover(&mut cfg, "m", Some("t".into()), bad.clone(), true, None).is_err());
        // good -> written
        let mut good = bad; good[0].priority = 1;
        apply_model_failover(&mut cfg, "m", Some("t".into()), good, true, Some(120)).unwrap();
        let m = &cfg.models[0];
        assert_eq!(m.fallback_target_model_id.as_deref(), Some("t"));
        assert_eq!(m.fallback_strategies.len(), 1);
        assert!(m.fallback_strategies_enabled);
        assert_eq!(m.cooldown_seconds, Some(120));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml commands::tests::apply_model_failover_validates_and_writes`
Expected: FAIL — `apply_model_failover` not defined.

- [ ] **Step 3: Implement the pure helper + 2 commands**

In `commands.rs` (near `fallback_map`/`set_model_fallback`):

```rust
/// Pure core: validate + write a model's failover config (default target + strategies + switch +
/// cooldown) into `cfg`. Mutates `strategies` (dedup/sort) during validation. Err on bad strategy.
pub fn apply_model_failover(
    cfg: &mut AppConfig,
    model_id: &str,
    fallback_target_model_id: Option<String>,
    mut fallback_strategies: Vec<Strategy>,
    fallback_strategies_enabled: bool,
    cooldown_seconds: Option<u64>,
) -> Result<(), String> {
    normalize_and_validate_strategies(cfg, &mut fallback_strategies)?;
    let m = cfg.models.iter_mut().find(|m| m.id == model_id)
        .ok_or_else(|| format!("model {model_id} not found"))?;
    m.fallback_target_model_id = fallback_target_model_id;
    m.fallback_strategies = fallback_strategies;
    m.fallback_strategies_enabled = fallback_strategies_enabled;
    m.cooldown_seconds = cooldown_seconds;
    Ok(())
}

/// Set a model's full failover config (modal save): default target + time strategies + switch +
/// cooldown. Validates, persists, resets the breaker (edit resets health, §4.1).
#[tauri::command]
pub async fn set_model_failover(
    state: State<'_, AppState>,
    model_id: String,
    fallback_target_model_id: Option<String>,
    fallback_strategies: Vec<Strategy>,
    fallback_strategies_enabled: bool,
    cooldown_seconds: Option<u64>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        apply_model_failover(&mut cfg, &model_id, fallback_target_model_id, fallback_strategies, fallback_strategies_enabled, cooldown_seconds)?;
        persist(&app, &cfg)?;
    }
    state.health.reset(&model_id);
    Ok(())
}

/// One-click card switch: toggle a model's `fallback_strategies_enabled` and persist.
#[tauri::command]
pub async fn set_model_fallback_strategies_enabled(
    state: State<'_, AppState>,
    model_id: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        let m = cfg.models.iter_mut().find(|m| m.id == model_id)
            .ok_or_else(|| format!("model {model_id} not found"))?;
        m.fallback_strategies_enabled = enabled;
        persist(&app, &cfg)?;
    }
    Ok(())
}
```

Register both in `src-tauri/src/lib.rs` `invoke_handler` (find the `generate_handler![...]` list, add `commands::set_model_failover` and `commands::set_model_fallback_strategies_enabled` next to `set_model_fallback`).

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (incl. `apply_model_failover_validates_and_writes`).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): set_model_failover + set_model_fallback_strategies_enabled"
```

---

## Task 6: `model_effective_fallbacks` + extend `RouteEffective`

**Files:**
- Modify: `src-tauri/src/commands.rs` — add `ModelEffectiveFallback` struct + `model_effective_fallbacks` command; extend `RouteEffective` (L223) + `route_effective_models` (L231); register `model_effective_fallbacks` in `lib.rs`.
- Test: same file `#[cfg(test)] mod tests`.

**Interfaces:**
- Consumes: `model_fallback_target` (Task 2), `route_effective_models`/`profile_start_model` (existing).
- Produces: `RouteEffective { effective_fallback_model_id: Option<String>, via_fallback_strategy_id: Option<String> }` (Overview's 故障转移 node); `model_effective_fallbacks() -> Vec<ModelEffectiveFallback>` (failover card's 当前生效).

- [ ] **Step 1: Add the failing tests**

In `commands.rs` `mod tests`:

```rust
    #[test]
    fn route_effective_models_includes_failover_target() {
        // profile -> m_glm (entry); m_glm has a time strategy 22:00-06:00 -> m_qwen.
        let mut cfg = AppConfig::default();
        cfg.models.push(Model { id: "m_glm".into(), provider_id: "p".into(), upstream_model_id: "glm".into(), fallback_target_model_id: Some("m_vol".into()), fallback_strategies: vec![Strategy { id: "s".into(), priority: 1, enabled: true, kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![1,2,3,4,5,6,7], time_start: 1320, time_end: 360, model_id: "m_qwen".into() }) }], fallback_strategies_enabled: true, ..Default::default() });
        cfg.models.push(Model { id: "m_qwen".into(), provider_id: "p".into(), upstream_model_id: "qwen".into(), ..Default::default() });
        cfg.models.push(Model { id: "m_vol".into(), provider_id: "p".into(), upstream_model_id: "vol".into(), ..Default::default() });
        cfg.profiles.push(Profile { id: "p1".into(), name: "glm".into(), aliases: vec![], backing_model_id: "m_glm".into(), ..Default::default() });
        let now = LocalNow { weekday: 2, minute: 1320 }; // 22:00 -> strategy m_qwen
        let eff = route_effective_models_core(&cfg, &now);
        assert_eq!(eff[0].effective_model_id, "m_glm");
        assert_eq!(eff[0].effective_fallback_model_id.as_deref(), Some("m_qwen"));
        assert_eq!(eff[0].via_fallback_strategy_id.as_deref(), Some("s"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml commands::tests::route_effective_models_includes_failover_target`
Expected: FAIL — no `effective_fallback_model_id` field / no `route_effective_models_core`.

- [ ] **Step 3: Implement**

Extend `RouteEffective` (commands.rs L223):
```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteEffective {
    pub profile_id: String,
    pub effective_model_id: String,
    pub via_strategy_id: Option<String>,
    pub effective_fallback_model_id: Option<String>,
    pub via_fallback_strategy_id: Option<String>,
}
```

Extract a pure core (testable without `State`) and have the command call it:
```rust
/// Pure core of `route_effective_models`: entry model + its time-aware failover, per profile.
fn route_effective_models_core(cfg: &AppConfig, now: &LocalNow) -> Vec<RouteEffective> {
    cfg.profiles.iter().map(|p| {
        let (mid, via) = crate::proxy::strategies::profile_start_model(p, cfg, now);
        let (fb, fb_via) = cfg.models.iter().find(|m| m.id == mid)
            .map(|m| crate::proxy::strategies::model_fallback_target(m, cfg, now))
            .unwrap_or((None, None));
        RouteEffective {
            profile_id: p.id.clone(),
            effective_model_id: mid.to_string(),
            via_strategy_id: via.map(str::to_string),
            effective_fallback_model_id: fb.map(str::to_string),
            via_fallback_strategy_id: fb_via.map(str::to_string),
        }
    }).collect()
}
```
Rewrite `route_effective_models` (L231) to: `let cfg = state.config.read().await; let now = state.clock.now_local(); Ok(route_effective_models_core(&cfg, &now))`.

Add the failover-page read command:
```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelEffectiveFallback {
    pub model_id: String,
    pub effective_fallback_model_id: Option<String>,
    pub via_fallback_strategy_id: Option<String>,
}

/// Per-model currently-effective failover target (time-aware), for the 故障转移 page cards.
#[tauri::command]
pub async fn model_effective_fallbacks(state: State<'_, AppState>) -> Result<Vec<ModelEffectiveFallback>, String> {
    let cfg = state.config.read().await;
    let now = state.clock.now_local();
    Ok(cfg.models.iter().map(|m| {
        let (fb, via) = crate::proxy::strategies::model_fallback_target(m, &cfg, &now);
        ModelEffectiveFallback { model_id: m.id.clone(), effective_fallback_model_id: fb.map(str::to_string), via_fallback_strategy_id: via.map(str::to_string) }
    }).collect())
}
```
Register `commands::model_effective_fallbacks` in `lib.rs` `invoke_handler`.

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): model_effective_fallbacks + RouteEffective 扩展故障转移目标"
```

---

# Phase B — Frontend

> Phase A commands must be merged first (frontend calls them). Each frontend task's "test" is the type-check + build.

## Task 7: TS bridge — types, command wrappers, store

**Files:**
- Modify: `src/lib/types.ts` (mirror `Model` + `RouteEffective`; add `ModelEffectiveFallback`).
- Modify: `src/lib/commands.ts` (add wrappers).
- Modify: `src/stores/config.ts` (expose `modelEffectiveFallbacks`).

**Interfaces:**
- Produces (TS): `Model.fallback_strategies` + `fallback_strategies_enabled`; `RouteEffective.effective_fallback_model_id` + `via_fallback_strategy_id`; `ModelEffectiveFallback`; `setModelFailover()`, `setModelFallbackStrategiesEnabled()`, `modelEffectiveFallbacks()`.

- [ ] **Step 1: Mirror types changes**

In `types.ts`, add to `Model` (keep snake_case to match serde):
```ts
  fallback_strategies?: Strategy[];
  fallback_strategies_enabled?: boolean;
```
Add to `RouteEffective`:
```ts
  effective_fallback_model_id?: string | null;
  via_fallback_strategy_id?: string | null;
```
Add the struct:
```ts
export interface ModelEffectiveFallback {
  model_id: string;
  effective_fallback_model_id: string | null;
  via_fallback_strategy_id: string | null;
}
```
Ensure `Strategy`/`StrategyKind`/`TimeStrategy` already exist (they do, from the entry-layer feature).

- [ ] **Step 2: Add command wrappers**

In `commands.ts`:
```ts
export const setModelFailover = (
  modelId: string,
  fallbackTargetModelId: string | null,
  fallbackStrategies: Strategy[],
  fallbackStrategiesEnabled: boolean,
  cooldownSeconds: number | null,
) => invoke<void>("set_model_failover", {
  modelId, fallbackTargetModelId, fallbackStrategies, fallbackStrategiesEnabled, cooldownSeconds,
});
export const setModelFallbackStrategiesEnabled = (modelId: string, enabled: boolean) =>
  invoke<void>("set_model_fallback_strategies_enabled", { modelId, enabled });
export const modelEffectiveFallbacks = () =>
  invoke<ModelEffectiveFallback[]>("model_effective_fallbacks");
```

- [ ] **Step 3: Store — expose effective fallbacks**

In `stores/config.ts`, add `const modelEffectiveFallbacks = ref<ModelEffectiveFallback[]>([]);`, fetch it in `loadAll` (alongside `routeEffective`), expose in the return. Refresh it after `setModelFailover`/`setModelFallbackStrategiesEnabled` succeed (canonical re-fetch pattern).

- [ ] **Step 4: Type-check + build**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS (no type errors).

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/config.ts
git commit -m "feat(frontend): 类型/命令/store 桥接故障转移策略与有效转移目标"
```

---

## Task 8: Extract shared `StrategyEditor.vue`

**Files:**
- Create: `src/components/StrategyEditor.vue`
- Modify: `src/views/Profiles.vue` — replace the inline strategy-editor block with `<StrategyEditor>`.
- Test: `pnpm exec vue-tsc --noEmit`.

**Interfaces:**
- Produces: `<StrategyEditor v-model="strategies" :model-options="modelOptions" :id-scope="idScope" />` — emits the edited `StrategyForm[]`. Used by `Profiles.vue` (route modal) and `Fallback.vue` (Task 10).

- [ ] **Step 1: Create `StrategyEditor.vue`**

Move the `StrategyForm` interface + the strategy-card template + helpers (`toggleDay`, `setWeekdays`, `isCrossNight`, `isAllDay`, `minToTs`, `tsToMin`) out of `Profiles.vue` into a new `StrategyEditor.vue` that takes `modelValue: StrategyForm[]` (v-model), `modelOptions`, `idScope` (for `genId` uniqueness). Props/emit:
```ts
const props = defineProps<{ modelValue: StrategyForm[]; modelOptions: { label: string; value: string }[]; idScope: string }>();
const emit = defineEmits<{ "update:modelValue": [StrategyForm[]] }>();
```
Template = the existing `.strategy-card` loop + `+ 添加策略` button (lift verbatim from `Profiles.vue` L266-304). Add/remove mutate a local copy and emit.

- [ ] **Step 2: Use it in `Profiles.vue`**

In `Profiles.vue`, import `StrategyEditor`, delete the moved helpers/interface, and replace the `<div v-for="s in form.strategies">...</div>` + add button with:
```html
<StrategyEditor v-model="form.strategies" :model-options="modelOptions" id-scope="route" />
```

- [ ] **Step 3: Type-check + build**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS; route modal strategy editing behaves exactly as before.

- [ ] **Step 4: Commit**

```bash
git add src/components/StrategyEditor.vue src/views/Profiles.vue
git commit -m "refactor(frontend): 抽出共享 StrategyEditor 组件"
```

---

## Task 9: Profiles.vue — route card redesign + terminology

**Files:**
- Modify: `src/views/Profiles.vue` (card template L218-239; field label L253; helper text L263).
- Test: `pnpm exec vue-tsc --noEmit`.

- [ ] **Step 1: Redesign the route card**

Replace the card's model-display row so it shows `当前生效` (not 兜底/当前) and folds the switch into the strategy tag. New card body:
```html
<NSpace align="center" :size="10" wrap>
  <span class="drag-handle" title="拖动排序">⠿</span>
  <span class="name">{{ p.name }}</span>
  <span class="arrow">当前生效</span>
  <span class="backing">{{ effectiveLabel(p) }}<span v-if="effectiveOf(p)?.via_strategy_id">⏰</span></span>
  <NTag v-if="(p.strategies?.length ?? 0) > 0" size="small" round :bordered="false"
    :type="p.strategies_enabled ? 'info' : 'default'">
    ⏰ 策略 {{ p.strategies!.length }} 条
    <NSwitch :value="p.strategies_enabled" size="tiny" @update:value="(v: boolean) => toggleMaster(p, v)" />
  </NTag>
</NSpace>
```
(Drop the old `兜底→` span and the separate `<NSwitch>`.)

- [ ] **Step 2: Terminology — backing field label + helper**

- L253 `<NFormItem label="兜底模型（默认/回退）">` → `label="默认模型"`.
- L263 helper `…将直接走兜底模型` → `…将直接走默认模型`.

- [ ] **Step 3: Type-check + build**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/views/Profiles.vue
git commit -m "feat(frontend): 路由卡片显示当前生效 + 策略开关合并 tag；兜底→默认术语"
```

---

## Task 10: Fallback.vue — page redesign mirroring the route page

**Files:**
- Modify: `src/views/Fallback.vue` (rewrite: description+新增 row; list only configured models; card = model→当前生效转移→策略tag→编辑/删除; add/edit modal).
- Test: `pnpm exec vue-tsc --noEmit`.

**Interfaces:**
- Consumes: `config.models`, `config.modelEffectiveFallbacks`, `setModelFailover`, `setModelFallbackStrategiesEnabled`, `validateFallbackContext`, `<StrategyEditor>`.

- [ ] **Step 1: Rewrite the page**

Mirror `Profiles.vue`'s structure. Script:
- `const configured = computed(() => config.models.filter(m => m.fallback_target_model_id || (m.fallback_strategies?.length)))` — rows to show.
- `const unconfigured = computed(() => config.models.filter(m => !m.fallback_target_model_id && !(m.fallback_strategies?.length)))` — picker options for 新增.
- `effectiveFallbackOf(m)` → `config.modelEffectiveFallbacks.find(e => e.model_id === m.id)`.
- A modal form: `model_id` (select, only when adding — options = unconfigured), `fallback_target_model_id` (select), `strategies` (`<StrategyEditor>`), `fallback_strategies_enabled` (switch), `cooldown_seconds` (number). On save → `config.setModelFailover(...)`. Delete → `config.setModelFailover(id, null, [], true, <existing cooldown>)` (clears config → row disappears).
- Card switch → `config.setModelFallbackStrategiesEnabled(m.id, v)`.
- Poll `config.modelEffectiveFallbacks` via `usePolling(() => config.refreshEffectiveFallbacks(), () => 60_000)` (add `refreshEffectiveFallbacks` to store, or fold into existing refresh).

Card template:
```html
<NCard size="small">
  <NSpace align="center" justify="space-between">
    <NSpace align="center" :size="10" wrap>
      <span class="drag-handle" title="拖动排序">⠿</span>
      <span class="name">{{ modelDisplayName(m) }}</span>
      <span class="arrow">当前生效</span>
      <span class="backing">{{ fbLabel(m) }}<span v-if="effectiveFallbackOf(m)?.via_fallback_strategy_id">⏰</span></span>
      <NTag v-if="(m.fallback_strategies?.length ?? 0) > 0" size="small" round :bordered="false" :type="m.fallback_strategies_enabled ? 'info' : 'default'">
        ⏰ 策略 {{ m.fallback_strategies!.length }} 条
        <NSwitch :value="m.fallback_strategies_enabled" size="tiny" @update:value="(v:boolean) => onToggle(m, v)" />
      </NTag>
    </NSpace>
    <NSpace>
      <NButton size="small" @click="openEdit(m)">编辑</NButton>
      <NButton size="small" type="error" ghost @click="remove(m)">删除</NButton>
    </NSpace>
  </NSpace>
</NCard>
```
Header row: `<NButton type="primary" @click="openNew">+ 新增</NButton>` (disabled/empty-state when `unconfigured.length === 0`). Description: "为模型配置 429/用量故障时的转移目标与时段策略".

- [ ] **Step 2: Reuse the existing context-size guard**

For each strategy `model_id` and the default target, call `validateFallbackContext` before save and warn on `smaller` (mirror the current `Fallback.vue` `onChange` dialog logic).

- [ ] **Step 3: Type-check + build**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/views/Fallback.vue
git commit -m "feat(frontend): 故障转移页镜像路由页（只列已配置 + 新增/编辑弹窗 + 当前生效转移模型）"
```

---

## Task 11: Overview pipeline — `[路由]▶[当前生效]▶[故障转移]`

**Files:**
- Modify: `src/components/RouteLine.vue` (rename node, add 故障转移 node, drop 兜底 node + prop).
- Modify: `src/views/Dashboard.vue` (L234-242: pass failover label, drop `fallback-model-label`/`backingLabelIfOverride`).
- Test: `pnpm exec vue-tsc --noEmit`.

**Interfaces:**
- Consumes: `RouteEffective.effective_fallback_model_id` / `via_fallback_strategy_id` (Task 6/7).

- [ ] **Step 1: Update `RouteLine.vue`**

Replace props: drop `fallbackModelLabel`; add `failoverModelLabel?: string` and `failoverViaStrategy?: boolean`. Template: keep 路由 + 当前生效 nodes (tag stays `当前生效`), drop the 兜底 node, add after 当前生效:
```html
<template v-if="failoverModelLabel">
  <span class="arrow">▶</span>
  <div class="node">
    <span class="tag tag-fallback">故障转移</span>
    <span class="val val-model">{{ failoverModelLabel }}<span v-if="failoverViaStrategy">⏰</span></span>
  </div>
</template>
```

- [ ] **Step 2: Update `Dashboard.vue`**

Pass the failover label from `routeEffective`. At L234-242, replace `:fallback-model-label="backingLabelIfOverride(p)"` with:
```html
:failover-model-label="modelLabelOf(effectiveFallbackOf(p)?.effective_fallback_model_id)"
:failover-via-strategy="!!effectiveFallbackOf(p)?.via_fallback_strategy_id"
```
Add `function effectiveFallbackOf(p) { return config.routeEffective.find(e => e.profile_id === p.id); }` and delete `backingLabelIfOverride`.

- [ ] **Step 3: Type-check + build**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS; overview shows `[路由]▶[当前生效…▶[故障转移…` (故障转移 hidden when no fallback).

- [ ] **Step 4: Final full verification**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && pnpm exec vue-tsc --noEmit && pnpm build`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add src/components/RouteLine.vue src/views/Dashboard.vue
git commit -m "feat(frontend): 概览管线 路由▶当前生效▶故障转移（去掉静态兜底节点）"
```

---

## Self-Review (done)

- **Spec coverage:** §3 fields→T1; §4.1 `model_fallback_target`→T2; §4.2 chained dispatch→T4; §4.3 commands (`set_model_failover`, `set_model_fallback_strategies_enabled`, `model_effective_fallbacks`, `RouteEffective`)→T5/T6; §5 validation generalize→T3; §6.1 bridge→T7; §6.2 `StrategyEditor`→T8; §6.3 route card→T9; §6.4 failover page→T10; §6.5 overview→T11; §6.6 terminology→T9; §7 tests covered per-task; §3.1 persistence rides existing `persist()` (no new code).
- **Placeholder scan:** Task 4's two dispatch tests carry an explicit implementer note (distinct success bodies + 3 boundary assertions) rather than hand-waving — the note pins the exact assertion shape; finalize following the named existing test. No other TBD/TODO.
- **Type consistency:** `model_fallback_target -> (Option<&str>, Option<&str>)` consistent across T2/T4/T6; `RouteEffective` fields consistent across T6 (backend) and T7/T11 (frontend); `setModelFailover` arg order matches the command's.
