# Provider Multi-Account Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user configure multiple accounts per vendor (e.g. two Volcengine Coding accounts), each fully functional and distinguishable, by splitting a new `vendor` routing field from the opaque `id` primary key.

**Architecture:** Add a `vendor` field to `Provider` (the routing key); `id` becomes an opaque per-instance PK (random for new providers, unchanged slug for legacy). All seven backend routing sites + the frontend switch from matching `id` to matching `vendor`. A backend-enforced `(vendor, display_name)` uniqueness constraint plus a same-account credential-conflict check (warn → overwrite in place) give safe, distinguishable multi-account config. Legacy configs migrate by deriving `vendor = id` at load — no keyring/FK changes.

**Tech Stack:** Rust (Tauri v2 backend, crate `switchlm`, edition 2021, axum/reqwest/tokio/keyring), Vue 3 + Pinia + naive-ui + vuedraggable (frontend). Rust tests via `cargo test`; the frontend has **no JS test runner** — it is verified by `npm run build` (vue-tsc type-check) plus manual steps.

**Spec:** `docs/superpowers/specs/2026-07-30-provider-multi-account-design.md`

## Global Constraints

- **Branch:** implement on `feat/provider-multi-account` (or a worktree) — do not commit to `main`.
- **Rust tests:** run a single test with `cargo test --manifest-path src-tauri/Cargo.toml <test_name_substring>`; run the whole suite with `cargo test --manifest-path src-tauri/Cargo.toml`. Working directory is the repo root.
- **Frontend verification:** no vitest/jest exists. Gate frontend tasks on `npm run build` (runs `vue-tsc --noEmit && vite build`) succeeding, then the listed manual checks.
- **`id` is opaque:** after this work, no routing/lookup/display reads `id` for vendor semantics — only `vendor`. `id` is used solely as PK / model FK / keyring key / cache key.
- **User-facing strings are Chinese** (match existing style, e.g. `该厂商下已有同名账号「…」，请改名`).
- **Commit style:** conventional, scoped — `feat(providers): …`, `refactor(proxy): …`, `test(…): …` (match recent history).
- **TDD for Rust:** write the failing test, see it fail, implement, see it pass, commit. Keep steps bite-sized.
- ** serde default:** the new `vendor` field is `#[serde(default)]` so legacy `app_config.json` (which omits it) deserializes to `""`, which the migration (Task 2) fills.

## File Structure

**Backend (`src-tauri/src/`)**
- `config/types.rs` — `Provider` gains `vendor`; round-trip / default-deserialize tests.
- `config/store.rs` — `normalize_legacy_vendors`; called at both load sites.
- `config/catalog.rs` — `context_size` first param renamed `provider_id` → `vendor` (struct field stays).
- `commands.rs` — uniqueness+trim in `upsert_provider`; new `check_provider_conflict`; vendor threading in `volcengine_plan_action`/`discover_*`/`query_usage`/`recognized_context_size`/`effective_size`/`classify_fallback`/`validate_fallback_context`; pure helpers `conflicting_same_name`, `same_account_conflict`.
- `usage/mod.rs` — `usage_provider_for(vendor)`.
- `proxy/error_adapter.rs` — match on `vendor`.
- `proxy/dispatch.rs` — `ModelSnapshot.vendor`; rate-limit + realtime-usage call sites use `snap.vendor`.
- `proxy/state.rs`, `lib.rs` — invoke `normalize_legacy_vendors` after `store::load`.
- `tray.rs` — `model_label` appends a vendor tag when a vendor has ≥2 providers.

**Frontend (`src/`)**
- `lib/types.ts` — `vendor?: string` on `Provider`.
- `lib/commands.ts` — `checkProviderConflict`.
- `lib/selectLabel.ts` — `vendorLabel`, `providerLabel`.
- `lib/id.ts` — reused unchanged (`genId("prov", …)`).
- `views/Provider.vue` — vendor select + account-name input + auto-suffix + save flow (conflict dialog → overwrite) + uniqueness error + vendor tag.
- `views/{Models,Profiles,Dashboard,Fallback,Usage}.vue` — apply `providerLabel`.

---

### Task 1: Add `vendor` field to `Provider` and fix every literal

This is the foundation: every later task depends on `vendor` existing. Adding the field breaks every `Provider { … }` literal, so all of them are updated in this one task to keep the build green. Routing still matches on `id` here (unchanged), so all existing tests keep passing.

**Files:**
- Modify: `src-tauri/src/config/types.rs` (struct + test literal)
- Modify: `src-tauri/src/commands.rs:932` (`mk_provider`)
- Modify: `src-tauri/src/proxy/dispatch.rs:758,765,773,924,925`
- Modify: `src-tauri/src/proxy/anthropic_edge.rs:29`
- Modify: `src-tauri/src/proxy/openai_edge.rs:31`
- Modify: `src-tauri/src/proxy/state.rs:176`
- Modify: `src-tauri/src/tray.rs:229` (`mk_provider`)

**Interfaces:**
- Produces: `Provider.vendor: String` (serde default `""`), present on every literal. Later tasks read it.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/config/types.rs` `#[cfg(test)] mod tests`, extend the existing `app_config_roundtrips` provider literal with `vendor` and add a default-deserialize test:

```rust
    // inside app_config_roundtrips's Provider { … }:
    //   id: "zhipu".into(), vendor: "zhipu".into(), display_name: "智谱".into(), …
    // and after the existing asserts, add:
    assert_eq!(back.providers[0].vendor, "zhipu");
```

```rust
    #[test]
    fn legacy_provider_without_vendor_defaults_empty() {
        // Legacy app_config.json omits `vendor`; serde must fill "" so the Task 2
        // migration can derive it from the id.
        let json = r#"{"id":"zhipu","display_name":"智谱","openai_base_url":"https://x"}"#;
        let p: Provider = serde_json::from_str(json).unwrap();
        assert_eq!(p.vendor, "");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml legacy_provider_without_vendor_defaults_empty`
Expected: compile error — `Provider` has no field named `vendor`.

- [ ] **Step 3: Add the field + fix every literal**

Add the field in `src-tauri/src/config/types.rs` (between `id` and `display_name`):

```rust
pub struct Provider {
    pub id: String,
    /// Vendor kind slug — the routing key (usage adapter, Volcengine plan discovery, catalog
    /// lookup, rate-limit classification) and source of base_url defaults + the list-row tag.
    /// Known: `zhipu`, `volcengine-agent`, `volcengine-coding`; else custom. `id` is the opaque PK.
    #[serde(default)]
    pub vendor: String,
    pub display_name: String,
    #[serde(default)]
    pub openai_base_url: Option<String>,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    #[serde(default)]
    pub usage_creds: Option<UsageCreds>,
}
```

Add `vendor: id.into()` (so legacy id==vendor keeps routing correct until later tasks flip it) to **every** `Provider { … }` literal:
- `commands.rs` `mk_provider` — add `vendor: id.into(),`
- `dispatch.rs` lines 758, 765, 773 (the `chain_state` builder) and 924, 925 — add `vendor:` matching each literal's `id` value (e.g. `vendor: "zhipu".into(),` / `vendor: "pb".into(),` / `vendor: "pc".into(),` / `vendor: "pa".into(),` / `vendor: "pb".into(),`)
- `anthropic_edge.rs:29` and `openai_edge.rs:31` — add `vendor: "zhipu".into(),` (these tests use a `zhipu` provider)
- `state.rs:176` — add `vendor:` matching its `id`
- `tray.rs` `mk_provider` — add `vendor: id.into(),`

- [ ] **Step 4: Run the full suite to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all existing tests still green; `vendor` present but not yet used for routing).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/commands.rs src-tauri/src/proxy/dispatch.rs src-tauri/src/proxy/anthropic_edge.rs src-tauri/src/proxy/openai_edge.rs src-tauri/src/proxy/state.rs src-tauri/src/tray.rs
git commit -m "feat(providers): add vendor field to Provider (routing key)"
```

---

### Task 2: Migrate legacy configs — `normalize_legacy_vendors` + load-site wiring

Fill `vendor` for legacy providers (`vendor = id`) at startup, persisting only if something changed. Idempotent; touches neither keyring nor model FKs.

**Files:**
- Modify: `src-tauri/src/config/store.rs` (new fn + test)
- Modify: `src-tauri/src/lib.rs:47` and `src-tauri/src/proxy/state.rs:41` (call after `store::load`)

**Interfaces:**
- Produces: `pub fn normalize_legacy_vendors(cfg: &mut crate::config::AppConfig) -> bool` in `store.rs`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/config/store.rs` `#[cfg(test)] mod tests`:

```rust
    use crate::config::{Provider, AppConfig}; // add Provider if not already imported

    #[test]
    fn normalize_legacy_vendors_fills_empty_from_id_and_is_idempotent() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "zhipu".into(), vendor: String::new(), display_name: "智谱".into(),
            openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        });
        cfg.providers.push(Provider {
            id: "volcengine-coding".into(), vendor: "volcengine-coding".into(),
            display_name: "火山".into(), openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        });
        // First pass: fills the empty one -> changed.
        assert!(normalize_legacy_vendors(&mut cfg));
        assert_eq!(cfg.providers[0].vendor, "zhipu");
        assert_eq!(cfg.providers[1].vendor, "volcengine-coding"); // already set, untouched
        // Second pass: nothing empty -> unchanged.
        assert!(!normalize_legacy_vendors(&mut cfg));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml normalize_legacy_vendors_fills_empty`
Expected: FAIL — `normalize_legacy_vendors` not found.

- [ ] **Step 3: Implement the normalizer + wire it at load**

In `src-tauri/src/config/store.rs`, add (the `AppConfig`/`Provider` types come from `crate::config`):

```rust
/// Fill `vendor` for legacy providers that lack it. Legacy configs used the vendor slug as the
/// provider `id`, so `vendor = id` is exactly correct and touches nothing else (no keyring, no FK).
/// Idempotent: once `vendor` is non-empty it is left alone. Returns whether any provider changed.
pub fn normalize_legacy_vendors(cfg: &mut crate::config::AppConfig) -> bool {
    let mut changed = false;
    for p in &mut cfg.providers {
        if p.vendor.is_empty() {
            p.vendor = p.id.clone();
            changed = true;
        }
    }
    changed
}
```

Wire it at **both** load sites. In `src-tauri/src/lib.rs` right after `let cfg = config::store::load(&dir)?;` (≈ line 47):

```rust
            if config::store::normalize_legacy_vendors(&mut cfg) {
                let _ = config::store::save(&dir, &cfg);
            }
```

Make `cfg` `mut` (`let mut cfg = …`). In `src-tauri/src/proxy/state.rs` right after `let cfg = store::load(dir)?;` (≈ line 41), do the same (`let mut cfg`, normalize, `if changed { store::save(dir, &cfg)?; }`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml normalize_legacy_vendors`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/store.rs src-tauri/src/lib.rs src-tauri/src/proxy/state.rs
git commit -m "feat(providers): migrate legacy configs — derive vendor from id at load"
```

---

### Task 3: Route catalog context-size lookups on `vendor`

Flip catalog-based context resolution from `provider_id` to `vendor`. This is self-contained in `commands.rs` + `catalog.rs`. A provider whose `id ≠ vendor` must still resolve the right size — that is the regression guard.

**Files:**
- Modify: `src-tauri/src/config/catalog.rs:28` (param rename)
- Modify: `src-tauri/src/commands.rs` — `effective_size` (`:683`), `classify_fallback` (`:690`), `validate_fallback_context` (`:400`), `recognized_context_size` (`:420`); their tests
- Test: `src-tauri/src/commands.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub fn effective_size(m: &Model, vendor: &str, catalog: &ModelCatalog) -> Option<u32>`; `pub fn classify_fallback(primary: &Model, fallback: &Model, primary_vendor: &str, fallback_vendor: &str, catalog: &ModelCatalog) -> ContextCheckResult`.
- Consumes: `Model.provider_id` is now an opaque FK (Task 1).

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/commands.rs` tests (alongside the existing `classify_*` tests):

```rust
    #[test]
    fn classify_resolves_context_via_vendor_not_provider_id() {
        // provider_id is an OPAQUE fk now ("prov_abc"), but the catalog is keyed by vendor.
        let cat = cat();
        let p = m("p", "prov_abc", "glm-4.6", None);       // vendor zhipu → 200000
        let f = m("f", "prov_xyz", "glm-4.5-air", None);    // vendor zhipu → 128000
        let r = classify_fallback(&p, &f, "zhipu", "zhipu", &cat);
        assert_eq!(r.status, ContextCheckStatus::Smaller); // 128000 < 200000
        assert_eq!(r.primary_size, Some(200000));
        assert_eq!(r.fallback_size, Some(128000));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml classify_resolves_context_via_vendor`
Expected: FAIL — `classify_fallback` takes 3 args, not 5 (signature not yet changed).

- [ ] **Step 3: Thread `vendor` through catalog resolution**

In `src-tauri/src/config/catalog.rs`, rename the lookup param for clarity (behavior identical — the catalog field already holds vendor slugs):

```rust
    pub fn context_size(&self, vendor: &str, upstream_model_id: &str) -> Option<u32> {
        self.models
            .iter()
            .find(|m| m.provider_id == vendor && m.upstream_model_id == upstream_model_id)
            .map(|m| m.context_size)
    }
```

In `src-tauri/src/commands.rs`:

```rust
pub fn effective_size(m: &Model, vendor: &str, catalog: &ModelCatalog) -> Option<u32> {
    m.context_size.or_else(|| catalog.context_size(vendor, &m.upstream_model_id))
}

pub fn classify_fallback(
    primary: &Model,
    fallback: &Model,
    primary_vendor: &str,
    fallback_vendor: &str,
    catalog: &ModelCatalog,
) -> ContextCheckResult {
    let p = effective_size(primary, primary_vendor, catalog);
    let f = effective_size(fallback, fallback_vendor, catalog);
    let status = match (p, f) {
        (Some(ps), Some(fs)) if fs < ps => ContextCheckStatus::Smaller,
        (Some(_), Some(_)) => ContextCheckStatus::Ok,
        _ => ContextCheckStatus::Unknown,
    };
    ContextCheckResult { status, primary_size: p, fallback_size: f }
}
```

Update `validate_fallback_context` to resolve each model's vendor from its provider, then call the new `classify_fallback`:

```rust
pub async fn validate_fallback_context(
    state: State<'_, AppState>,
    primary_id: String,
    fallback_id: String,
) -> Result<ContextCheckResult, String> {
    let (primary, fallback, pv, fv) = {
        let cfg = state.config.read().await;
        let p = cfg.models.iter().find(|m| m.id == primary_id)
            .ok_or_else(|| format!("model {primary_id} not found"))?;
        let f = cfg.models.iter().find(|m| m.id == fallback_id)
            .ok_or_else(|| format!("model {fallback_id} not found"))?;
        let vendor_of = |mid: &str| cfg.providers.iter().find(|pr| pr.id == mid).map(|pr| pr.vendor.as_str()).unwrap_or("");
        (p.clone(), f.clone(), vendor_of(&p.provider_id).to_string(), vendor_of(&f.provider_id).to_string())
    };
    Ok(classify_fallback(&primary, &fallback, &pv, &fv, &state.catalog))
}
```

Update `recognized_context_size` to resolve the provider's vendor before the catalog call:

```rust
pub fn recognized_context_size(
    state: State<'_, AppState>,
    provider_id: String,
    upstream_model_id: String,
) -> Option<u32> {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner());
    // blocking read is fine: this is a sync command; mirror the lock style of the original
    let cfg = state.config.try_read().ok()?;
    let vendor = cfg.providers.iter().find(|p| p.id == provider_id).map(|p| p.vendor.clone()).unwrap_or_default();
    drop(cfg);
    state.catalog.context_size(&vendor, &upstream_model_id)
}
```

> Note: `recognized_context_size` is a **sync** `#[tauri::command]` (`pub fn`, no async). The original read the catalog directly without a lock; now it needs the provider→vendor lookup, which requires reading config. Use `state.config.try_read()` (non-blocking) and return `None` if locked (rare; the UI degrades to "unknown" gracefully). If `try_read` is unavailable on this `RwLock`, wrap the lookup in `state.config.blocking_read()` instead.

Update the existing `classify_*` tests in `commands.rs` to pass the two vendor args (they use provider_id `"zhipu"` and catalog vendor `"zhipu"` — pass `"zhipu", "zhipu"`), e.g. `classify_fallback(&p, &f, "zhipu", "zhipu", &cat)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml classify_`
Expected: PASS (new + existing `classify_*` tests green).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/catalog.rs src-tauri/src/commands.rs
git commit -m "refactor(proxy): resolve fallback context sizes via vendor, not provider_id"
```

---

### Task 4: Route usage-adapter selection on `vendor` (+ carry `vendor` in `ModelSnapshot`)

`usage_provider_for` selects the usage adapter by vendor. Its dispatch caller reads `snap.provider_id`; to pass vendor correctly (and avoid a compiling-but-wrong `&str` swap), `ModelSnapshot` gains a `vendor` field populated in `snapshot_model`.

**Files:**
- Modify: `src-tauri/src/usage/mod.rs:153` (`usage_provider_for`) + its test
- Modify: `src-tauri/src/commands.rs:647` (`query_usage`)
- Modify: `src-tauri/src/proxy/dispatch.rs:601` (`ModelSnapshot`), `:609` (`snapshot_model`), `:661` (`usage_snapshot_realtime`)

**Interfaces:**
- Produces: `pub fn usage_provider_for(vendor: &str)`; `ModelSnapshot { vendor: String, … }`.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/usage/mod.rs` tests:

```rust
    #[test]
    fn usage_provider_for_matches_on_vendor() {
        assert!(usage_provider_for("zhipu").is_some());
        assert!(usage_provider_for("volcengine-coding").is_some());
        assert!(usage_provider_for("volcengine-agent").is_some());
        assert!(usage_provider_for("prov_abc").is_none()); // an opaque id is NOT a vendor
    }
```

In `src-tauri/src/proxy/dispatch.rs` tests (add `use super::snapshot_model;` near the existing `use super::exhausted_reset_at;`):

```rust
    #[tokio::test]
    async fn snapshot_model_carries_vendor_distinct_from_id() {
        let clock: Arc<dyn Clock> = Arc::new(FakeClock);
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "prov_abc".into(), vendor: "zhipu".into(), display_name: "工作号".into(),
            openai_base_url: Some("https://x/v1".into()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "prov_abc", None));
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: ModelCatalog::default(),
            secrets: Arc::new(MemoryStore::default()),
            health: Default::default(), clock,
            usage_cache: Default::default(),
            actual_port: std::sync::Mutex::new(None), server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None), polling: std::sync::Mutex::new(None),
        });
        let snap = snapshot_model(&state, "m_a").await.unwrap();
        assert_eq!(snap.provider_id, "prov_abc"); // opaque key preserved
        assert_eq!(snap.vendor, "zhipu");         // routing key carried
    }
```

> If `AppStateInner` has additional fields (check the struct def — `bind_error`/`polling` were added by the port-recovery work), mirror exactly what `chain_state` constructs at `dispatch.rs:786`; copy that initializer and only change the provider. If unsure, run the test once and fix the reported missing fields.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml usage_provider_for_matches_on_vendor snapshot_model_carries_vendor`
Expected: FAIL — `usage_provider_for` still takes the old arg / `ModelSnapshot` has no `vendor`.

- [ ] **Step 3: Implement**

In `src-tauri/src/usage/mod.rs`:

```rust
pub fn usage_provider_for(vendor: &str) -> Option<Box<dyn UsageProvider>> {
    match vendor {
        "zhipu" => Some(Box::new(zhipu::ZhipuUsageProvider)),
        "volcengine" | "volcengine-agent" | "volcengine-coding" => {
            Some(Box::new(volcengine::VolcengineUsageProvider))
        }
        _ => None,
    }
}
```

In `src-tauri/src/proxy/dispatch.rs`, add `vendor` to the struct + populate it:

```rust
struct ModelSnapshot {
    upstream_model_id: String,
    openai_base_url: Option<String>,
    anthropic_base_url: Option<String>,
    provider_id: String,
    vendor: String,
    cooldown_seconds: Option<u64>,
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
    })
}
```

In `usage_snapshot_realtime` (dispatch.rs ≈ :662), change the adapter selection to vendor (keep the opaque-key uses of `snap.provider_id` for keyring/cache/creds):

```rust
    let provider = usage_provider_for(&snap.vendor)?;
```

In `src-tauri/src/commands.rs` `query_usage` (≈ :647), resolve the vendor from the provider it already looks up and pass it:

```rust
    let (base_url, vendor) = {
        let cfg = state.config.read().await;
        let p = cfg.providers.iter().find(|p| p.id == provider_id)
            .ok_or_else(|| format!("provider {provider_id} not found"))?;
        (p.openai_base_url.clone().unwrap_or_default(), p.vendor.clone())
    };
    // … unchanged keyring/creds reads …
    let provider = usage_provider_for(&vendor)
        .ok_or_else(|| format!("usage query unsupported for vendor '{vendor}'"))?;
```

(Adjust the surrounding lines so `base_url` and `vendor` come from the single read lock; the rest of `query_usage` stays the same.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — including the new tests and the full dispatch suite.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/usage/mod.rs src-tauri/src/proxy/dispatch.rs src-tauri/src/commands.rs
git commit -m "refactor(proxy): select usage adapter by vendor; carry vendor in ModelSnapshot"
```

---

### Task 5: Route Volcengine plan-model discovery on `vendor`

`volcengine_plan_action` decides which Ark plan API to call; flip it from the id to the vendor.

**Files:**
- Modify: `src-tauri/src/commands.rs:188` (`volcengine_plan_action`), `:174` (`discover_models`)
- Test: `src-tauri/src/commands.rs` `volcengine_plan_action_mapping`

**Interfaces:**
- Produces: `fn volcengine_plan_action(vendor: &str) -> Option<&'static str>`.

- [ ] **Step 1: Write the failing test**

Extend the existing `volcengine_plan_action_mapping` test in `commands.rs`:

```rust
    #[test]
    fn volcengine_plan_action_mapping() {
        assert_eq!(volcengine_plan_action("volcengine-agent"), Some("ListArkAgentPlanModel"));
        assert_eq!(volcengine_plan_action("volcengine-coding"), Some("ListArkCodingPlanModel"));
        assert_eq!(volcengine_plan_action("zhipu"), None);
        assert_eq!(volcengine_plan_action("prov_abc"), None); // opaque id is not a vendor
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml volcengine_plan_action_mapping`
Expected: the new last assertion FAILs if the caller still passes an id — but the function itself takes a `&str`; it passes today because the existing call passes the slug. It will compile. To make this test meaningful, first do Step 3 (change the signature/caller) — so run it now expecting PASS-by-coincidence, then verify the caller change in Step 3 keeps it green. (If it already passes, the regression value is in the caller change below.)

- [ ] **Step 3: Rename the param + fix the caller**

```rust
fn volcengine_plan_action(vendor: &str) -> Option<&'static str> {
    match vendor {
        "volcengine-agent" => Some("ListArkAgentPlanModel"),
        "volcengine-coding" => Some("ListArkCodingPlanModel"),
        _ => None,
    }
}
```

In `discover_models` (commands.rs ≈ :174), resolve the provider's vendor and branch on it:

```rust
pub async fn discover_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<Vec<DiscoveredModel>, String> {
    let vendor = {
        let cfg = state.config.read().await;
        cfg.providers.iter().find(|p| p.id == provider_id).map(|p| p.vendor.clone())
            .ok_or_else(|| format!("provider {provider_id} not found"))?
    };
    if let Some(action) = volcengine_plan_action(&vendor) {
        return discover_volcengine_models(&state, &provider_id, action).await;
    }
    let (base_url, api_key) = provider_endpoint(&state, &provider_id).await?;
    fetch_discovered_models(&base_url, api_key.as_deref()).await
}
```

(`discover_volcengine_models` still keys its keyring/config reads on the opaque `provider_id` — correct, unchanged.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml volcengine_plan_action`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "refactor(proxy): branch Volcengine plan discovery on vendor"
```

---

### Task 6: Route rate-limit error classification on `vendor`

`is_rate_limit_error` / `rate_limit_in_json` match vendor-specific error codes; flip them to vendor and thread `snap.vendor` through the dispatch call sites.

**Files:**
- Modify: `src-tauri/src/proxy/error_adapter.rs:8,22,26` (+ tests)
- Modify: `src-tauri/src/proxy/dispatch.rs` — every call site + the helper fns that take `provider_id: &str` for classification

**Interfaces:**
- Produces: `pub fn is_rate_limit_error(vendor: &str, status: Option<u16>, body: &str) -> bool`; `pub fn sse_event_is_rate_limit(vendor: &str, event_data: &str) -> bool`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/proxy/error_adapter.rs` tests (find a representative body the existing tests use for a volcengine hit; copy it). Add:

```rust
    #[test]
    fn rate_limit_classifies_by_vendor_not_id() {
        // Same body, vendor vs opaque id: only the vendor routes to the provider-specific code.
        let body = /* a volcengine-style rate-limit body used by an existing test, e.g. */
            r#"{"Code":"QuotaExceeded","Message":"rate limited"}"#;
        assert!(is_rate_limit_error("volcengine-coding", Some(429), body));
        assert!(!is_rate_limit_error("prov_abc", Some(429), body)); // opaque id → keyword fallback only
    }
```

(Use the exact body + status one of the existing `volcengine`/provider-specific tests asserts `true` on; if that test passes `provider_id = "volcengine-coding"`, the body works as-is.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml rate_limit_classifies_by_vendor`
Expected: FAIL — the function still treats its first arg as `provider_id`; the second assertion may already pass (keyword fallback) — if so, tighten the body so only the vendor-specific path matches. Confirm the FIRST assertion passes and the function signature is renamed before proceeding.

- [ ] **Step 3: Rename params to `vendor` + thread through dispatch**

In `src-tauri/src/proxy/error_adapter.rs`, rename `provider_id` → `vendor` in `is_rate_limit_error`, `sse_event_is_rate_limit`, and `rate_limit_in_json` (signatures + the `match vendor { … }` and recursive call). Behavior is identical — the values passed were always vendor slugs.

In `src-tauri/src/proxy/dispatch.rs`, every call site that classifies an error currently passes `provider_id` (a local) or `&snap.provider_id`. Change each to pass the **vendor**:
- Where the call is inside a function that already has `snap: &ModelSnapshot`, use `&snap.vendor`.
- Where a helper signature is `fn handle_x(provider_id: &str, …)` and it forwards to `is_rate_limit_error(provider_id, …)`, rename the param to `vendor: &str` throughout that helper and update **its** callers to pass `&snap.vendor`.

Concretely, grep-locate every `is_rate_limit_error(` and `sse_event_is_rate_limit(` call in `dispatch.rs` and ensure each first argument is the vendor (from `snap.vendor` or a renamed `vendor` param), never `snap.provider_id`. Update the existing dispatch tests' expectations only if a test constructed a provider whose `id ≠ vendor` (the `chain_state` helper uses `id == vendor`, so existing tests stay green).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — error_adapter tests + full dispatch suite green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/error_adapter.rs src-tauri/src/proxy/dispatch.rs
git commit -m "refactor(proxy): classify rate-limit errors by vendor"
```

---

### Task 7: Enforce `(vendor, display_name)` uniqueness (+ trim) in `upsert_provider`

The authoritative backend rule: no two providers share `(vendor, display_name)`; the name is trimmed before compare and persist. Exposed as a pure helper so it is unit-testable without `AppState`.

**Files:**
- Modify: `src-tauri/src/commands.rs` — `upsert_provider` (`:64`) + new `conflicting_same_name` + tests

**Interfaces:**
- Produces: `pub fn conflicting_same_name<'c>(cfg: &'c AppConfig, provider: &Provider) -> Option<&'c Provider>`.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/commands.rs` tests:

```rust
    fn pv(id: &str, vendor: &str, name: &str) -> Provider {
        Provider {
            id: id.into(), vendor: vendor.into(), display_name: name.into(),
            openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        }
    }

    #[test]
    fn conflicting_same_name_detects_dup_and_trim_and_self_exclude() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(pv("p1", "volcengine-coding", "工作号"));

        // same vendor + same name, different id -> conflict
        assert!(conflicting_same_name(&cfg, &pv("p2", "volcengine-coding", "工作号")).is_some());
        // trailing whitespace is trimmed -> still a conflict
        assert!(conflicting_same_name(&cfg, &pv("p2", "volcengine-coding", " 工作号 ")).is_some());
        // same id (edit) -> excluded
        assert!(conflicting_same_name(&cfg, &pv("p1", "volcengine-coding", "工作号")).is_none());
        // different vendor -> ok
        assert!(conflicting_same_name(&cfg, &pv("p2", "zhipu", "工作号")).is_none());
        // same vendor, different name -> ok
        assert!(conflicting_same_name(&cfg, &pv("p2", "volcengine-coding", "个人号")).is_none());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml conflicting_same_name_detects_dup`
Expected: FAIL — `conflicting_same_name` not found.

- [ ] **Step 3: Implement the helper + wire into `upsert_provider`**

```rust
/// Another provider (different id) that already owns the same `(vendor, display_name)`, comparing
/// trimmed names. `None` when the name is free (or when the only match is `provider` itself).
pub fn conflicting_same_name<'c>(cfg: &'c AppConfig, provider: &Provider) -> Option<&'c Provider> {
    let name = provider.display_name.trim();
    cfg.providers.iter().find(|p| {
        p.id != provider.id && p.vendor == provider.vendor && p.display_name.trim() == name
    })
}
```

In `upsert_provider` (commands.rs ≈ :64), trim then check before the existing `upsert`:

```rust
pub async fn upsert_provider(
    state: State<'_, AppState>,
    mut provider: Provider,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        provider.display_name = provider.display_name.trim().to_string();
        let mut cfg = state.config.write().await;
        if let Some(_) = conflicting_same_name(&cfg, &provider) {
            return Err(format!("该厂商下已有同名账号「{}」，请改名", provider.display_name));
        }
        upsert(&mut cfg.providers, provider);
        persist(&app, &cfg)?;
    }
    Ok(())
}
```

(`mut provider` so the trim mutates the incoming value before it is stored.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml conflicting_same_name`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(providers): enforce (vendor, display_name) uniqueness with trim"
```

---

### Task 8: Same-account conflict detection — `check_provider_conflict`

A new command the add-form calls before saving: does another provider (same vendor) already hold these credentials? Credentials are secret, so the comparison lives in the backend. Pure core is extracted for testing.

**Files:**
- Modify: `src-tauri/src/commands.rs` — new `ProviderConflict`, `same_account_conflict`, `check_provider_conflict`; register in `lib.rs` `invoke_handler`
- Modify: `src-tauri/src/lib.rs` (register command)

**Interfaces:**
- Produces: `#[tauri::command] pub async fn check_provider_conflict(state, vendor: String, access_key_id: Option<String>, api_key: Option<String>) -> Result<Option<ProviderConflict>, String>`, where `ProviderConflict { id, display_name }`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/commands.rs` tests:

```rust
    #[test]
    fn same_account_conflict_matches_credentials() {
        // Two providers, same vendor, different accounts.
        let providers = vec![
            pv("p1", "volcengine-coding", "工作号"), // AK ak1
            pv("p2", "volcengine-coding", "个人号"), // AK ak2
        ];
        let mut p1 = pv("p1", "volcengine-coding", "工作号");
        p1.usage_creds = Some(UsageCreds { access_key_id: Some("ak1".into()), secret_access_key: None });
        let mut p2 = pv("p2", "volcengine-coding", "个人号");
        p2.usage_creds = Some(UsageCreds { access_key_id: Some("ak2".into()), secret_access_key: None });
        let providers = vec![p1, p2];

        // Volcengine: AK match hits p1.
        assert_eq!(
            same_account_conflict(&providers, "volcengine-coding", Some("ak1"), |_| None, None).map(|p| p.id),
            Some("p1".to_string())
        );
        // Different AK -> no conflict.
        assert!(same_account_conflict(&providers, "volcengine-coding", Some("akX"), |_| None, None).is_none());
        // Blank AK -> cannot match.
        assert!(same_account_conflict(&providers, "volcengine-coding", None, |_| None, None).is_none());

        // Non-volcengine: api_key (via key_of closure) match hits.
        let zhipu = vec![{ let mut p = pv("p1", "zhipu", "工作号"); p }];
        assert_eq!(
            same_account_conflict(&zhipu, "zhipu", None, |_| Some("sk-1".into()), Some("sk-1")).map(|p| p.id),
            Some("p1".to_string())
        );
    }
```

- [ ] **Step 2: Run the test to verify it fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml same_account_conflict_matches_credentials`
Expected: FAIL — `same_account_conflict` not found. (`UsageCreds` is already in scope via `use crate::config::*;`.)

- [ ] **Step 3: Implement the pure core + the command**

```rust
#[derive(Serialize)]
pub struct ProviderConflict {
    pub id: String,
    pub display_name: String,
}

/// Pure core: the existing provider (same vendor) that already holds the given credential.
/// `key_of(provider_id)` reads the inference key so tests can inject a fake keyring. Volcengine
/// matches on AccessKey ID; others on the inference api_key. Blank/absent credentials never match.
pub fn same_account_conflict<'c>(
    providers: &'c [Provider],
    vendor: &str,
    access_key_id: Option<&str>,
    key_of: impl Fn(&str) -> Option<String>,
    api_key: Option<&str>,
) -> Option<&'c Provider> {
    let ak = access_key_id.map(str::trim).filter(|s| !s.is_empty());
    let key = api_key.map(str::trim).filter(|s| !s.is_empty());
    providers.iter().find(|p| {
        if p.vendor != vendor {
            return false;
        }
        if vendor.starts_with("volcengine") {
            ak.is_some() && p.usage_creds.as_ref()
                .and_then(|c| c.access_key_id.as_deref()).map(str::trim) == ak
        } else {
            let existing = key_of(&p.id).map(|s| s.trim().to_string());
            key.is_some() && existing.as_deref() == key
        }
    })
}

/// Whether the credentials being saved already belong to a configured provider (same account).
/// Volcengine matches on AccessKey ID; others on the inference api_key read from the keyring.
#[tauri::command]
pub async fn check_provider_conflict(
    state: State<'_, AppState>,
    vendor: String,
    access_key_id: Option<String>,
    api_key: Option<String>,
) -> Result<Option<ProviderConflict>, String> {
    let providers = state.config.read().await.providers.clone();
    let secrets = &state.secrets;
    let found = same_account_conflict(
        &providers, &vendor, access_key_id.as_deref(),
        |pid| secrets.get_key(pid).ok().flatten(),
        api_key.as_deref(),
    );
    Ok(found.map(|p| ProviderConflict { id: p.id.clone(), display_name: p.display_name.clone() }))
}
```

Register `check_provider_conflict` in the `tauri::generate_handler!` list in `src-tauri/src/lib.rs` (next to `upsert_provider`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml same_account_conflict`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(providers): add same-account conflict check command"
```

---

### Task 9: Disambiguate tray labels by vendor (only when a vendor has ≥2 providers)

The tray shows `model · provider 80%`. Same-vendor accounts are already distinguishable by their auto-suffixed name; show the vendor tag only when a vendor actually has multiple providers (no clutter for the common single-account case).

**Files:**
- Modify: `src-tauri/src/tray.rs` — `model_label` (`:62`), the builder (`:84`), `mk_provider`/tests
- Test: `src-tauri/src/tray.rs` tests

**Interfaces:**
- Produces: `fn model_label(model, provider_name: Option<&str>, vendor_tag: Option<&str>, usage, cooling) -> String`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/tray.rs` tests:

```rust
    #[test]
    fn model_label_appends_vendor_tag_only_when_given() {
        let m = mk_model("m1", "zhipu", "GLM-4.6");
        // tag present (ambiguous vendor) -> appended
        assert!(model_label(&m, Some("工作号"), Some("volcengine-coding"), Some(&snap(80.0)), false)
            .contains("[volcengine-coding]"));
        // no tag (single account) -> clean label
        let clean = model_label(&m, Some("智谱"), None, Some(&snap(80.0)), false);
        assert!(!clean.contains('['));
        assert!(clean.contains("智谱"));
    }
```

- [ ] **Step 2: Run the test to verify it fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml model_label_appends_vendor_tag`
Expected: FAIL — `model_label` does not take `vendor_tag`.

- [ ] **Step 3: Add the tag param + compute ambiguity in the builder**

```rust
fn model_label(
    model: &Model,
    provider_name: Option<&str>,
    vendor_tag: Option<&str>,
    usage: Option<&UsageSnapshot>,
    cooling: bool,
) -> String {
    let mut s = model.display_name.clone();
    if let Some(name) = provider_name {
        s.push_str(" · ");
        s.push_str(name);
    }
    if let Some(v) = vendor_tag {
        s.push_str(&format!(" [{v}]"));
    }
    // …existing quota % + cooling append logic stays unchanged…
    s
}
```

(Preserve the existing quota/cooling suffix code that follows; only insert the `vendor_tag` block in the right place.)

In the tray menu builder (≈ `:84`), compute the set of vendors with ≥2 providers, then pass a tag only for those:

```rust
    use std::collections::HashSet;
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for p in &cfg.providers { *counts.entry(p.vendor.as_str()).or_default() += 1; }
    let multi: HashSet<&str> = counts.iter().filter(|(_, n)| **n >= 2).map(|(v, _)| *v).collect();
    let provider_spec = |pid: &str| {
        cfg.providers.iter().find(|p| p.id == pid).map(|p| {
            (p.display_name.as_str(), if multi.contains(p.vendor.as_str()) { Some(p.vendor.as_str()) } else { None })
        })
    };
```

Then where `model_label(m, provider_name(&m.provider_id), usage.get(&m.provider_id), cooling)` is called today, pass the tag:

```rust
    let (name, tag) = provider_spec(&m.provider_id).unzip();
    label: model_label(m, name, tag, usage.get(&m.provider_id), cooling),
```

Update the existing `label_includes_provider_and_quota` / `label_without_provider_is_just_name` tests to pass `None` for the new `vendor_tag` arg.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — tray tests green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): show vendor tag only when a vendor has multiple accounts"
```

---

### Task 10: Frontend — `vendor` type, `checkProviderConflict`, and the label helpers

Wire the new backend surface into the frontend and add the shared label helper. No JS test runner — verify by type-check.

**Files:**
- Modify: `src/lib/types.ts` (`Provider`)
- Modify: `src/lib/commands.ts` (add `checkProviderConflict`)
- Modify: `src/lib/selectLabel.ts` (add `vendorLabel`, `providerLabel`)

**Interfaces:**
- Produces (TS): `Provider.vendor?: string`; `checkProviderConflict(vendor, accessKeyId?, apiKey?)`; `vendorLabel(vendor)`, `providerLabel(p)`.

- [ ] **Step 1: Add the type + command binding**

In `src/lib/types.ts`, add `vendor` to `Provider` (after `id`):

```ts
export interface Provider {
  id: string;
  vendor?: string;
  display_name: string;
  openai_base_url?: string | null;
  anthropic_base_url?: string | null;
  usage_creds?: UsageCreds | null;
}
```

In `src/lib/commands.ts`, add (mirroring the existing `invoke` style; check the file for the exact import of `@tauri-apps/api/core` `invoke`):

```ts
export interface ProviderConflict {
  id: string;
  display_name: string;
}

export async function checkProviderConflict(
  vendor: string,
  accessKeyId?: string | null,
  apiKey?: string | null,
): Promise<ProviderConflict | null> {
  return invoke<ProviderConflict | null>("check_provider_conflict", {
    vendor,
    accessKeyId: accessKeyId ?? null,
    apiKey: apiKey ?? null,
  });
}
```

> Tauri converts JS camelCase args to Rust snake_case by default — confirm the existing commands in this file pass args as camelCase (e.g. `providerId`) and match that. If the file uses snake_case keys, use `access_key_id` / `api_key` instead.

- [ ] **Step 2: Add the label helpers**

`vendorOptions` currently lives inside `Provider.vue`. To share it, export it from a small module. In `src/lib/selectLabel.ts` (which already exports `ellipsisLabel`), append:

```ts
import type { Provider } from "./types";

// Known vendors (label/value). Kept here so both Provider.vue and the label helpers share one source.
export const vendorOptions = [
  { label: "智谱", value: "zhipu" },
  { label: "火山 · agent plan", value: "volcengine-agent" },
  { label: "火山 · coding plan", value: "volcengine-coding" },
];

export function vendorLabel(vendor: string): string {
  return vendorOptions.find((o) => o.value === vendor)?.label ?? vendor;
}

// "工作号（火山 · coding plan）" — but suppress the suffix when the name already IS the vendor
// label (the default), so a default single account reads "火山 · coding plan", not redundantly.
export function providerLabel(p: Provider): string {
  const v = vendorLabel(p.vendor ?? "");
  return p.display_name === v ? p.display_name : `${p.display_name}（${v}）`;
}
```

Then in `src/views/Provider.vue`, **delete** the local `vendorOptions` const and import it instead: `import { ellipsisLabel, vendorOptions } from "../lib/selectLabel";`.

- [ ] **Step 3: Type-check**

Run: `npm run build`
Expected: succeeds (no TS errors). If `vendorOptions` is reported as a duplicate identifier, ensure the local const in `Provider.vue` was removed.

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/lib/selectLabel.ts src/views/Provider.vue
git commit -m "feat(providers): add vendor type, conflict-check binding, shared label helpers"
```

---

### Task 11: `Provider.vue` multi-account refactor

Split the single id-select into a vendor select + an editable account-name; add auto-suffix, the save flow with conflict→overwrite, uniqueness-error handling, and the vendor tag in the list.

**Files:**
- Modify: `src/views/Provider.vue` (form state, `onVendorChange`, `openNew`/`openEdit`, `save`, list row)

**Interfaces:**
- Consumes: `genId` (`src/lib/id.ts`), `checkProviderConflict`, `vendorOptions`/`providerLabel`/`vendorLabel` (`src/lib/selectLabel.ts`), `config.saveProvider`/`setKey`/`setUsageSk`.

- [ ] **Step 1: Extend the form state + vendor-default logic**

Update the `FormState` interface and `blank()` to carry `vendor` + `displayName` (replacing the old single `id` driving both):

```ts
interface FormState {
  id: string;            // opaque PK (existing on edit; genId on new; conflict.id on overwrite)
  vendor: string;        // routing kind
  displayName: string;   // account name (list title)
  openai_base_url: string;
  anthropic_base_url: string;
  apiKey: string;
  access_key_id: string;
  usageSk: string;
}
const blank = (): FormState => ({
  id: "", vendor: "", displayName: "",
  openai_base_url: "", anthropic_base_url: "", apiKey: "", access_key_id: "", usageSk: "",
});
```

Keep the existing `vendorDefaults` map keyed by vendor slug (it already is). Add the auto-suffix default and rewire `onVendorChange`:

```ts
function defaultDisplayName(vendor: string): string {
  const label = vendorOptions.find((o) => o.value === vendor)?.label ?? vendor;
  const taken = new Set(
    config.providers.filter((p) => p.vendor === vendor).map((p) => p.display_name),
  );
  if (!taken.has(label)) return label;
  for (let n = 2; ; n++) { const cand = `${label} ${n}`; if (!taken.has(cand)) return cand; }
}

function onVendorChange(vendor: string | number) {
  form.vendor = String(vendor);
  const d = vendorDefaults[form.vendor];
  if (d) { form.openai_base_url = d.openai_base_url; form.anthropic_base_url = d.anthropic_base_url; }
  if (!editing.value) form.displayName = defaultDisplayName(form.vendor);
}

const needsUsageCreds = computed(() => form.vendor.startsWith("volcengine"));
```

- [ ] **Step 2: Rewrite `openNew` / `openEdit` / `save`**

```ts
function openNew() {
  Object.assign(form, blank());
  editing.value = false;
  showModal.value = true;
}
function openEdit(p: Provider) {
  form.id = p.id;
  form.vendor = p.vendor ?? "";
  form.displayName = p.display_name;
  form.openai_base_url = p.openai_base_url ?? "";
  form.anthropic_base_url = p.anthropic_base_url ?? "";
  form.apiKey = ""; form.access_key_id = p.usage_creds?.access_key_id ?? ""; form.usageSk = "";
  editing.value = true;
  showModal.value = true;
}

async function save() {
  if (!form.vendor.trim()) { msg.warning("请选择或输入服务商"); return; }
  if (!form.displayName.trim()) { msg.warning("请填写账号名称"); return; }
  if (!form.openai_base_url.trim() && !form.anthropic_base_url.trim()) {
    msg.warning("至少填一个 base_url（OpenAI 或 Anthropic）"); return;
  }
  const usage_creds: UsageCreds | null =
    needsUsageCreds.value && form.access_key_id ? { access_key_id: form.access_key_id } : null;
  const provider: Provider = {
    id: editing.value ? form.id : genId("prov", config.providers.map((p) => p.id)),
    vendor: form.vendor.trim(),
    display_name: form.displayName.trim(),
    openai_base_url: form.openai_base_url.trim() || null,
    anthropic_base_url: form.anthropic_base_url.trim() || null,
    usage_creds,
  };
  try {
    // Same-account check — NEW providers only (edit keeps a fixed id).
    if (!editing.value) {
      const conflict = await checkProviderConflict(
        provider.vendor,
        usage_creds?.access_key_id ?? null,
        form.apiKey || null,
      );
      if (conflict) {
        const overwrite = await new Promise<boolean>((resolve) =>
          dialog.warning({
            title: "检测到同一账号",
            content: `与现有「${conflict.display_name}」疑似同一账号（同密钥），继续将覆盖该服务商配置。`,
            positiveText: "覆盖",
            negativeText: "取消",
            onPositiveClick: () => resolve(true),
            onNegativeClick: () => resolve(false),
            onMaskClick: () => resolve(false),
          }),
        );
        if (!overwrite) return;
        provider.id = conflict.id; // overwrite in place (keeps FK + keyring valid)
      }
    }
    await config.saveProvider(provider); // backend enforces (vendor, display_name) uniqueness
    if (form.apiKey) await config.setKey(provider.id, form.apiKey);     // after id finalized
    if (form.usageSk) await config.setUsageSk(provider.id, form.usageSk); // after id finalized
    msg.success(editing.value ? "已保存" : "已新增服务商配置");
    showModal.value = false;
  } catch (e) {
    msg.error(`保存失败：${String(e)}`); // shows the uniqueness error from the backend
  }
}
```

Add the imports: `import { genId } from "../lib/id";` and `import { checkProviderConflict } from "../lib/commands";`.

- [ ] **Step 3: Update the template**

In the modal form, replace the single vendor/id `NSelect` binding so the select drives `form.vendor` (its value is the vendor slug) and is disabled on edit; add an account-name input right after it:

```vue
        <NFormItem label="模型服务商（驱动用量/发现路由；智谱/火山可查用量，其它可手填）">
          <NSelect
            :value="form.vendor"
            :options="vendorOptions"
            :render-label="ellipsisLabel"
            :disabled="editing"
            filterable
            tag
            placeholder="选择或输入服务商"
            @update:value="onVendorChange"
          />
        </NFormItem>
        <NFormItem label="账号名称（用于区分同服务商的多个账号）">
          <NInput v-model:value="form.displayName" placeholder="如：工作号 / 个人号" />
        </NFormItem>
```

In the list row, keep `{{ p.display_name }}` as the title and add a vendor tag beside the existing tags:

```vue
                  <NTag size="small" type="info">{{ vendorLabel(p.vendor ?? p.id) }}</NTag>
```

(Import `vendorLabel` from `../lib/selectLabel`.) Remove any now-unused references to the old `form.id`-as-vendor logic.

- [ ] **Step 4: Type-check**

Run: `npm run build`
Expected: succeeds.

- [ ] **Step 5: Manual verification**

Run the app (`npm run tauri dev`) and confirm:
1. Add one Volcengine Coding provider (AK + SK + api_key) named "工作号" → appears with a "火山 · coding plan" tag.
2. Add a **second** Volcengine Coding provider with a **different** AK → account name auto-fills "火山 · coding plan 2"; both coexist; both show独立用量 after refresh.
3. Try to add a third with the **same AK** as #1 → the "检测到同一账号 / 覆盖" dialog appears; Cancel keeps both; confirm 覆盖 replaces #1 in place (its models/keys still resolve).
4. Manually rename #2's account name to equal #1's → save → shows "该厂商下已有同名账号「…」，请改名" and stays open.
5. Edit #1, change only the base_url, leave keys blank → saves; keys unchanged.

- [ ] **Step 6: Commit**

```bash
git add src/views/Provider.vue
git commit -m "feat(providers): multi-account add/edit flow with conflict overwrite"
```

---

### Task 12: Apply `providerLabel` across the remaining views

Replace bare `provider.display_name` labels with the disambiguating `providerLabel(p)` so same-named cross-vendor accounts are distinguishable wherever a provider is surfaced.

**Files:**
- Modify: `src/views/Models.vue:33`
- Modify: `src/views/Profiles.vue:32,109`
- Modify: `src/views/Dashboard.vue:83,101`
- Modify: `src/views/Fallback.vue:19`
- Modify: `src/views/Usage.vue:39`

**Interfaces:**
- Consumes: `providerLabel`, `vendorLabel` from `src/lib/selectLabel.ts` (Task 10).

- [ ] **Step 1: Swap each label call site to `providerLabel`**

- `src/views/Models.vue:33` — `const providerOptions = computed(() => config.providers.map((p) => ({ label: providerLabel(p), value: p.id })));` (import `providerLabel`).
- `src/views/Profiles.vue:32` — `const label = provider ? \`${m.display_name} (${providerLabel(provider)})\` : m.display_name;` and the same at `:109`.
- `src/views/Dashboard.vue:101` — `const label = provider ? \`${m.display_name}（${providerLabel(provider)}）\` : m.display_name;` and at `:83` use `providerLabel(provider)` for the chain's `provider` field.
- `src/views/Fallback.vue:19` — `function providerName(id: string) { const p = config.providers.find((q) => q.id === id); return p ? providerLabel(p) : id; }`.
- `src/views/Usage.vue:39` — `name: config.providers.find((p) => p.id === entry.provider_id) ? providerLabel(config.providers.find((p) => p.id === entry.provider_id)!) : entry.provider_id` (or hoist the lookup to a const for readability).

Add `import { providerLabel } from "../lib/selectLabel";` to each file that uses it.

- [ ] **Step 2: Type-check**

Run: `npm run build`
Expected: succeeds.

- [ ] **Step 3: Manual verification**

In the running app, with two same-named custom accounts on different vendors:
1. **Models** add/edit → provider dropdown shows `工作号（智谱）` and `工作号（火山 · coding plan）`, distinguishable.
2. **Profiles** model dropdown and **Dashboard** quick-switch + fallback route line show the vendor-suffixed provider.
3. **Fallback** page model rows show the vendor-suffixed provider.
4. **Usage** page shows one card per provider with the vendor-suffixed title.

- [ ] **Step 4: Commit**

```bash
git add src/views/Models.vue src/views/Profiles.vue src/views/Dashboard.vue src/views/Fallback.vue src/views/Usage.vue
git commit -m "feat(ui): disambiguate provider labels with vendor across views"
```

---

## Self-Review

**Spec coverage** — every spec section maps to a task:
- Data model (`vendor` field, opaque `id`) → Task 1.
- Migration (`normalize_legacy_vendors`, load sites) → Task 2.
- Routing decoupling (7 sites): catalog → Task 3; usage adapter → Task 4; volcengine discovery → Task 5; rate-limit adapter → Task 6; (`usage_provider_for`/`volcengine_plan_action`/`context_size`/`is_rate_limit_error` all covered); `ModelSnapshot.vendor` → Task 4.
- `(vendor, display_name)` uniqueness + trim → Task 7.
- `check_provider_conflict` (same-account, warn→overwrite) → Task 8 (command) + Task 11 (UI flow + keyring-after-id ordering).
- `Provider.vue` UX (vendor select, account name, auto-suffix, vendor tag, genId) → Task 11.
- Provider labels across UI (incl. tray) → Tasks 9 (tray) + 10 (helpers) + 12 (apply).
- TS `vendor` type + command binding → Task 10.
- `conflict-check only when !editing` + `keyring writes after id finalized` → Task 11 (`save`).

**Placeholder scan** — no TBD/TODO; every code step has real code; manual steps have concrete actions.

**Type/name consistency** — `normalize_legacy_vendors`, `effective_size(m, vendor, catalog)`, `classify_fallback(…, primary_vendor, fallback_vendor, catalog)`, `usage_provider_for(vendor)`, `volcengine_plan_action(vendor)`, `is_rate_limit_error(vendor, …)`, `conflicting_same_name`, `same_account_conflict`, `ProviderConflict`, `vendorOptions`, `vendorLabel`, `providerLabel`, `checkProviderConflict`, `defaultDisplayName`, `model_label(…, vendor_tag, …)` — names match across tasks and the spec.

**Notes / risks for the implementer:**
- `recognized_context_size` is a **sync** command needing a config read it didn't need before — use `try_read()`/`blocking_read()` (Task 3). Verify the lock API name.
- The `&str` aliasing between `provider_id` and `vendor` is the main correctness risk; `snapshot_model_carries_vendor_distinct_from_id` (Task 4) + `classify_resolves_context_via_vendor_not_provider_id` (Task 3) + `rate_limit_classifies_by_vendor_not_id` (Task 6) are the regression guards that an opaque id no longer routes.
- After Task 1, the build is green but `vendor` is unused for routing — that's intentional; routing flips in Tasks 3–6.
- Tauri arg camelCase↔snake_case: confirm against existing commands in `commands.ts` (Task 10).
