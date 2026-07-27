# Fallback Context-Size Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Warn the user (soft dialog) when they set a fallback model whose context window is smaller than the primary's, backed by a bundled vendor/model context-size catalog that seeds into AppData on first run and merges new entries on later runs.

**Architecture:** A new `config/catalog.rs` module owns a `ModelCatalog` (parsed from an `include_str!`-embedded JSON). At startup `ensure_catalog()` seeds/merges it to `%APPDATA%\com.switchlm.app\model_desc.json` and loads it into `AppStateInner`. A backend `validate_fallback_context` command resolves each model's effective size (manual `Model.context_size` → catalog → unknown) and returns `Ok | Smaller | Unknown`; the `Fallback.vue` dropdown shows a confirm/cancel dialog for `Smaller` and an info note for `Unknown`.

**Tech Stack:** Rust + Tauri v2 (backend), serde; Vue 3 + naive-ui + Pinia + TypeScript (frontend). No new dependencies (embedding via stdlib `include_str!`; tests use the already-present `tempfile` dev-dep).

**Spec:** `docs/superpowers/specs/2026-07-29-fallback-context-size-check-design.md`

## Global Constraints

- **Context-size unit** = the input/total context window in tokens (the figure that decides whether a fallback holds the in-flight conversation). Catalog values are the vendors' advertised token counts (K = 1000, M = 1 000 000): e.g. 200K → 200000, 1024k → 1024000, 1M → 1000000.
- **serde snake_case, no rename**; TS mirrors in `src/lib/types.ts` stay in sync with `src-tauri/src/config/types.rs` + `commands.rs`.
- **Embedding** = `include_str!("../../assets/vendor_model_desc.json")` from `src/config/catalog.rs` (no new crate).
- **Catalog merge semantics** = union by `(provider_id, upstream_model_id)`; AppData/user value wins on conflict; embedded-only entries are added; `version` is passthrough.
- **Validation UX** = `Smaller` → blocking confirm/cancel dialog (cancel reverts the dropdown); `Unknown` → non-blocking info message; both still allow saving.
- **No runtime change** — `proxy/dispatch.rs` fallback walk is untouched; this is a config-time guard.
- **Tests use a real temp dir** (`tempfile::tempdir`) for file I/O — never a mocked store.
- Run Rust tests from the repo root with `cargo test --manifest-path src-tauri/Cargo.toml <filter>`; typecheck frontend with `npx vue-tsc --noEmit`.
- `provider_id` is the vendor discriminator: `zhipu`, `volcengine-agent`, `volcengine-coding`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/assets/vendor_model_desc.json` | **new** — bundled default catalog (3 vendors). |
| `src-tauri/src/config/catalog.rs` | **new** — `ModelCatalog`/`ModelDesc` structs, embedded const, lookup, merge, seed/merge I/O. |
| `src-tauri/src/config/mod.rs` | register `pub mod catalog;` + re-export. |
| `src-tauri/src/config/types.rs` | `Model.context_size` field. |
| `src-tauri/src/proxy/state.rs` | `catalog` field on `AppStateInner`; `load()` seeds it. |
| `src-tauri/src/lib.rs` | `ensure_catalog` in setup; register 2 commands. |
| `src-tauri/src/commands.rs` | `ContextCheckStatus`/`Result`, `effective_size`, `validate_fallback_context`, `recognized_context_size`. |
| `src/lib/types.ts` | `context_size` on `Model`; `ContextCheckStatus`/`ContextCheckResult`. |
| `src/lib/commands.ts` | `validateFallbackContext`, `recognizedContextSize`. |
| `src/views/Fallback.vue` | validated `onChange` + confirm dialog + info note + size formatter. |
| `src/views/Models.vue` | optional `context_size` field + recognized-size hint. |

---

### Task 1: Bundled catalog data + `ModelCatalog`/`ModelDesc` structs + in-memory ops

**Files:**
- Create: `src-tauri/assets/vendor_model_desc.json`
- Create: `src-tauri/src/config/catalog.rs`
- Modify: `src-tauri/src/config/mod.rs:1-6`

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces: `ModelCatalog { version, models: Vec<ModelDesc> }`, `ModelDesc { provider_id, upstream_model_id, context_size, display_name }`, `ModelCatalog::context_size(provider_id, upstream_model_id) -> Option<u32>`, `ModelCatalog::merge_new_from(&mut self, &ModelCatalog)`, and `pub const EMBEDDED_CATALOG: &str`.

- [ ] **Step 1: Create the bundled catalog JSON**

Create `src-tauri/assets/vendor_model_desc.json` with exactly this content (context windows in tokens; upstream ids are the documented model names lowercased — verify casing against discovered ids in Task 6; mismatches degrade gracefully to "unknown"):

```json
{
  "version": 1,
  "models": [
    { "provider_id": "zhipu", "upstream_model_id": "glm-5.2", "context_size": 1000000, "display_name": "GLM-5.2" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-5.1", "context_size": 200000, "display_name": "GLM-5.1" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-5", "context_size": 200000, "display_name": "GLM-5" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-5-turbo", "context_size": 200000, "display_name": "GLM-5-Turbo" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.7", "context_size": 200000, "display_name": "GLM-4.7" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.7-flashx", "context_size": 200000, "display_name": "GLM-4.7-FlashX" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.7-flash", "context_size": 200000, "display_name": "GLM-4.7-Flash" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.6", "context_size": 200000, "display_name": "GLM-4.6" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.5-air", "context_size": 128000, "display_name": "GLM-4.5-Air" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.5-airx", "context_size": 128000, "display_name": "GLM-4.5-AirX" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4.5-flash", "context_size": 128000, "display_name": "GLM-4.5-Flash" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4-long", "context_size": 1000000, "display_name": "GLM-4-Long" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4-flashx-250414", "context_size": 128000, "display_name": "GLM-4-FlashX-250414" },
    { "provider_id": "zhipu", "upstream_model_id": "glm-4-flash-250414", "context_size": 128000, "display_name": "GLM-4-Flash-250414" },
    { "provider_id": "zhipu", "upstream_model_id": "codegeex-4", "context_size": 128000, "display_name": "CodeGeeX-4" },

    { "provider_id": "volcengine-agent", "upstream_model_id": "doubao-seed-evolving", "context_size": 1024000, "display_name": "Doubao-Seed-Evolving" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "doubao-seed-2.1-turbo", "context_size": 256000, "display_name": "Doubao-Seed-2.1-Turbo" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "doubao-seed-2.0-pro", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Pro" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "doubao-seed-2.0-lite", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Lite" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "doubao-seed-2.0-code", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Code" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "doubao-seed-2.0-mini", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Mini" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "deepseek-v4-pro", "context_size": 1024000, "display_name": "DeepSeek-V4-Pro" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "deepseek-v4-flash", "context_size": 1024000, "display_name": "DeepSeek-V4-Flash" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "kimi-k3", "context_size": 1024000, "display_name": "Kimi-K3" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "kimi-k2.7-code", "context_size": 256000, "display_name": "Kimi-K2.7-Code" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "kimi-k2.6", "context_size": 256000, "display_name": "Kimi-K2.6" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "minimax-m3", "context_size": 512000, "display_name": "MiniMax-M3" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "minimax-m2.7", "context_size": 200000, "display_name": "MiniMax-M2.7" },
    { "provider_id": "volcengine-agent", "upstream_model_id": "glm-5.2", "context_size": 1024000, "display_name": "GLM-5.2" },

    { "provider_id": "volcengine-coding", "upstream_model_id": "auto", "context_size": 1024000, "display_name": "Auto" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "doubao-seed-2.1-turbo", "context_size": 256000, "display_name": "Doubao-Seed-2.1-Turbo" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "doubao-seed-2.0-code", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Code" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "doubao-seed-code", "context_size": 256000, "display_name": "Doubao-Seed-Code" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "doubao-seed-2.0-pro", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Pro" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "doubao-seed-2.0-lite", "context_size": 256000, "display_name": "Doubao-Seed-2.0-Lite" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "deepseek-v4-pro", "context_size": 1024000, "display_name": "DeepSeek-V4-Pro" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "deepseek-v4-flash", "context_size": 1024000, "display_name": "DeepSeek-V4-Flash" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "kimi-k2.7-code", "context_size": 256000, "display_name": "Kimi-K2.7-Code" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "kimi-k2.6", "context_size": 256000, "display_name": "Kimi-K2.6" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "minimax-m3", "context_size": 512000, "display_name": "MiniMax-M3" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "minimax-m2.7", "context_size": 200000, "display_name": "MiniMax-M2.7" },
    { "provider_id": "volcengine-coding", "upstream_model_id": "glm-5.2", "context_size": 1024000, "display_name": "GLM-5.2" }
  ]
}
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/config/catalog.rs` with just the test module (the structs/methods don't exist yet → compile fails, which is the "fail"):

```rust
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_has_sizes() {
        let cat: ModelCatalog = serde_json::from_str(EMBEDDED_CATALOG).unwrap();
        assert!(cat.version >= 1);
        assert!(!cat.models.is_empty());
        for m in &cat.models {
            assert!(m.context_size > 0, "{:?} has zero size", m.upstream_model_id);
        }
    }

    #[test]
    fn lookup_finds_bundled_models() {
        let cat: ModelCatalog = serde_json::from_str(EMBEDDED_CATALOG).unwrap();
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(200000));
        assert_eq!(cat.context_size("volcengine-agent", "doubao-seed-evolving"), Some(1024000));
        assert_eq!(cat.context_size("volcengine-coding", "glm-5.2"), Some(1024000));
    }

    #[test]
    fn lookup_misses_unknown() {
        let cat: ModelCatalog = serde_json::from_str(EMBEDDED_CATALOG).unwrap();
        assert_eq!(cat.context_size("zhipu", "nope"), None);
        assert_eq!(cat.context_size("unknown-vendor", "glm-4.6"), None);
    }

    #[test]
    fn merge_adds_missing_keeps_existing() {
        // disk has glm-4.6 overridden to 999 (user edit) + a user-only entry.
        let mut disk = ModelCatalog { version: 1, models: vec![
            ModelDesc { provider_id: "zhipu".into(), upstream_model_id: "glm-4.6".into(), context_size: 999, display_name: None },
            ModelDesc { provider_id: "zhipu".into(), upstream_model_id: "custom-user".into(), context_size: 5000, display_name: None },
        ]};
        let embedded = ModelCatalog { version: 1, models: vec![
            ModelDesc { provider_id: "zhipu".into(), upstream_model_id: "glm-4.6".into(), context_size: 200000, display_name: None },
            ModelDesc { provider_id: "zhipu".into(), upstream_model_id: "glm-5.2".into(), context_size: 1000000, display_name: None },
        ]};
        disk.merge_new_from(&embedded);
        // conflict -> disk value retained
        assert_eq!(disk.context_size("zhipu", "glm-4.6"), Some(999));
        // new embedded entry added
        assert_eq!(disk.context_size("zhipu", "glm-5.2"), Some(1000000));
        // user-only entry preserved
        assert_eq!(disk.context_size("zhipu", "custom-user"), Some(5000));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml catalog`
Expected: COMPILE ERROR — `ModelCatalog`, `ModelDesc`, `EMBEDDED_CATALOG`, `context_size`, `merge_new_from` not defined.

- [ ] **Step 4: Write minimal implementation**

Add to `src-tauri/src/config/catalog.rs` (above the test module):

```rust
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::store::StoreError;

/// Bundled default vendor/model description catalog (compiled into the binary).
pub const EMBEDDED_CATALOG: &str = include_str!("../../assets/vendor_model_desc.json");

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelCatalog {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub models: Vec<ModelDesc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelDesc {
    pub provider_id: String,
    pub upstream_model_id: String,
    pub context_size: u32,
    #[serde(default)]
    pub display_name: Option<String>,
}

impl ModelCatalog {
    /// Catalog-only lookup (ignores any per-Model manual override). None if not bundled.
    pub fn context_size(&self, provider_id: &str, upstream_model_id: &str) -> Option<u32> {
        self.models
            .iter()
            .find(|m| m.provider_id == provider_id && m.upstream_model_id == upstream_model_id)
            .map(|m| m.context_size)
    }

    /// Union-merge: append `embedded` entries whose (provider_id, upstream_model_id) key is absent;
    /// never overwrite an existing entry (AppData/user value wins). `version` is passthrough.
    pub fn merge_new_from(&mut self, embedded: &ModelCatalog) {
        for m in &embedded.models {
            let exists = self
                .models
                .iter()
                .any(|e| e.provider_id == m.provider_id && e.upstream_model_id == m.upstream_model_id);
            if !exists {
                self.models.push(m.clone());
            }
        }
    }
}
```

(The `use super::store::StoreError;` and `Path`/`PathBuf` imports are used in Task 2; leave them — Rust will warn about unused imports until Task 2, which is fine, or prefix with `#[allow(unused_imports)]` if the build is `-D warnings`. Check Task 9 before finalizing.)

- [ ] **Step 5: Register the module**

In `src-tauri/src/config/mod.rs`, add `pub mod catalog;` and re-export. The file becomes:

```rust
pub mod catalog;
pub mod secrets;
pub mod store;
pub mod types;

pub use catalog::*;
pub use secrets::*;
pub use types::*;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml catalog`
Expected: PASS (4 tests).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/assets/vendor_model_desc.json src-tauri/src/config/catalog.rs src-tauri/src/config/mod.rs
git commit -m "feat(catalog): add bundled vendor model context-size catalog + struct/lookup/merge"
```

---

### Task 2: Seed/merge disk I/O (`ensure_catalog`)

**Files:**
- Modify: `src-tauri/src/config/catalog.rs` (add `catalog_path`, `parse_embedded`, `save_catalog`, `ensure_catalog`)

**Interfaces:**
- Consumes: `EMBEDDED_CATALOG`, `ModelCatalog`, `merge_new_from` (Task 1); `StoreError` from `config::store`.
- Produces: `catalog_path(dir: &Path) -> PathBuf`, `ensure_catalog(dir: &Path) -> ModelCatalog`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `catalog.rs`:

```rust
    use tempfile::tempdir;

    fn write_catalog(dir: &Path, body: &str) {
        std::fs::write(catalog_path(dir), body).unwrap();
    }

    #[test]
    fn ensure_seeds_on_first_run() {
        let dir = tempdir().unwrap();
        assert!(!catalog_path(dir.path()).exists());
        let cat = ensure_catalog(dir.path());
        assert!(catalog_path(dir.path()).exists(), "seeded file written");
        // seeded content == embedded
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(200000));
    }

    #[test]
    fn ensure_preserves_and_merges_existing() {
        let dir = tempdir().unwrap();
        // disk: user-overridden glm-4.6 (999) + user-only entry; no glm-5.2
        write_catalog(dir.path(), r#"{"version":1,"models":[
            {"provider_id":"zhipu","upstream_model_id":"glm-4.6","context_size":999},
            {"provider_id":"zhipu","upstream_model_id":"user-only","context_size":7000}
        ]}"#);
        let cat = ensure_catalog(dir.path());
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(999), "disk value wins");
        assert_eq!(cat.context_size("zhipu", "user-only"), Some(7000), "user entry kept");
        assert_eq!(cat.context_size("zhipu", "glm-5.2"), Some(1000000), "new embedded merged in");
        // merged result persisted back to disk
        let on_disk: ModelCatalog =
            serde_json::from_str(&std::fs::read_to_string(catalog_path(dir.path())).unwrap()).unwrap();
        assert_eq!(on_disk.context_size("zhipu", "glm-5.2"), Some(1000000));
    }

    #[test]
    fn ensure_reseeds_when_disk_malformed() {
        let dir = tempdir().unwrap();
        write_catalog(dir.path(), "{ not valid json ");
        let cat = ensure_catalog(dir.path());
        // self-heal: falls back to embedded
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(200000));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml catalog`
Expected: COMPILE ERROR — `catalog_path`, `ensure_catalog` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `catalog.rs` (below the `impl ModelCatalog` block):

```rust
pub fn catalog_path(dir: &Path) -> PathBuf {
    dir.join("model_desc.json")
}

fn parse_embedded() -> ModelCatalog {
    serde_json::from_str(EMBEDDED_CATALOG).unwrap_or_else(|e| {
        tracing::error!("内嵌 catalog 解析失败，降级为空：{e}");
        ModelCatalog::default()
    })
}

pub fn save_catalog(dir: &Path, cat: &ModelCatalog) -> Result<(), StoreError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(cat)?;
    std::fs::write(catalog_path(dir), text)?;
    Ok(())
}

/// First run -> seed embedded. Otherwise read the AppData copy, merge in any new embedded
/// entries (AppData/user value wins), persist back, and return it. Malformed disk file ->
/// self-heal to embedded. Never fails: on any I/O error it logs and returns the in-memory catalog.
pub fn ensure_catalog(dir: &Path) -> ModelCatalog {
    let embedded = parse_embedded();
    let path = catalog_path(dir);
    if !path.exists() {
        let _ = save_catalog(dir, &embedded);
        return embedded;
    }
    let disk = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<ModelCatalog>(&t).ok())
    {
        Some(c) => c,
        None => {
            tracing::warn!("model_desc.json 解析失败，已用内嵌默认重置");
            let _ = save_catalog(dir, &embedded);
            return embedded;
        }
    };
    let mut disk = disk;
    disk.merge_new_from(&embedded);
    let _ = save_catalog(dir, &disk);
    disk
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml catalog`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/catalog.rs
git commit -m "feat(catalog): seed/merge model_desc.json to AppData on startup"
```

---

### Task 3: `Model.context_size` field

**Files:**
- Modify: `src-tauri/src/config/types.rs:91-106` (struct) and `:125-164` (test)
- Modify: `src/lib/types.ts:18-26`

**Interfaces:**
- Consumes: nothing new.
- Produces: `Model.context_size: Option<u32>` (Rust) / `context_size?: number | null` (TS).

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/config/types.rs`, extend the `Model` literal in `app_config_roundtrips` (around line 139) to include the new field, and add an assertion. Add `context_size: Some(200000),` to the `Model { ... }` in the test, then after the roundtrip add:

```rust
        assert_eq!(back.models[0].context_size, Some(200000));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app_config_roundtrips`
Expected: COMPILE ERROR — `context_size` field not on `Model`.

- [ ] **Step 3: Write minimal implementation**

Add the field to the `Model` struct in `types.rs` (after `fallback_target_model_id`):

```rust
    #[serde(default)]
    pub fallback_target_model_id: Option<String>,
    /// Explicit context-window override / value for non-catalog models (input tokens). Effective
    /// size = this, else catalog lookup, else unknown. NOT auto-filled from the catalog (would go
    /// stale after a startup-merge) — set only when the user opts in.
    #[serde(default)]
    pub context_size: Option<u32>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app_config_roundtrips`
Expected: PASS.

- [ ] **Step 5: Mirror the TS type**

In `src/lib/types.ts`, add the field to the `Model` interface (after `fallback_target_model_id`):

```ts
  fallback_target_model_id?: string | null;
  context_size?: number | null;
}
```

- [ ] **Step 6: Typecheck frontend**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/config/types.rs src/lib/types.ts
git commit -m "feat(models): add optional context_size field to Model"
```

---

### Task 4: Load catalog into `AppState` + startup wiring

**Files:**
- Modify: `src-tauri/src/proxy/state.rs:13-50` (struct + `load`)
- Modify: `src-tauri/src/lib.rs:42-66` (setup hook)

**Interfaces:**
- Consumes: `config::catalog::ensure_catalog` (Task 2), `ModelCatalog` (Task 1).
- Produces: `AppStateInner.catalog: ModelCatalog` (read by the commands in Task 5).

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/proxy/state.rs`, extend the `load_builds_state_from_config` test (around line 168) to assert the catalog is seeded into the temp dir. After `let state = AppStateInner::load(dir.path(), secrets).unwrap();` add:

```rust
        // load() seeds model_desc.json and exposes the catalog.
        assert!(dir.path().join("model_desc.json").exists());
        assert_eq!(state.catalog.context_size("zhipu", "glm-4.6"), Some(200000));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml state::tests::load_builds_state_from_config`
Expected: COMPILE ERROR — no `catalog` field on `AppStateInner`.

- [ ] **Step 3: Write minimal implementation — add the field + seed in `load`**

In `state.rs`:
1. Add the import: `use crate::config::catalog::{ensure_catalog, ModelCatalog};`
2. Add the field to `AppStateInner` (after `config`):
   ```rust
   pub struct AppStateInner {
       pub config: RwLock<AppConfig>,
       /// Bundled+AppData-merged vendor/model context-size catalog (immutable for the session).
       pub catalog: ModelCatalog,
       pub secrets: Arc<dyn SecretStore>,
   ```
3. In `AppStateInner::load`, seed it:
   ```rust
       pub fn load(dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self, StoreError> {
           let cfg = store::load(dir)?;
           let catalog = ensure_catalog(dir);
           Ok(Self {
               config: RwLock::new(cfg),
               catalog,
               secrets,
               health: HealthRegistry::default(),
               clock: Arc::new(SystemClock),
               usage_cache: UsageCache::default(),
               actual_port: Mutex::new(None),
               server_handle: Mutex::new(None),
               bind_error: Mutex::new(None),
               polling_handle: Mutex::new(None),
           })
       }
   ```

- [ ] **Step 4: Wire the inline construction in `lib.rs` setup hook**

In `src-tauri/src/lib.rs`, after `let cfg = config::store::load(&dir)?;` (line 47) add `let catalog = config::catalog::ensure_catalog(&dir);`, and add `catalog,` to the `AppStateInner { ... }` literal (right after `config: tokio::sync::RwLock::new(cfg),`).

The setup block becomes:

```rust
            let cfg = config::store::load(&dir)?;
            let catalog = config::catalog::ensure_catalog(&dir);
            let preferred = cfg.settings.port;
            let cfg_for_tray = cfg.clone();
            let state: proxy::AppState = Arc::new(AppStateInner {
                config: tokio::sync::RwLock::new(cfg),
                catalog,
                secrets,
                health: proxy::HealthRegistry::default(),
                clock: Arc::new(proxy::SystemClock),
                usage_cache: usage::UsageCache::default(),
                actual_port: Mutex::new(None),
                server_handle: Mutex::new(None),
                bind_error: Mutex::new(None),
                polling_handle: Mutex::new(None),
            });
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (whole suite — the new field is now on every construction site).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/proxy/state.rs src-tauri/src/lib.rs
git commit -m "feat(catalog): load merged catalog into AppState at startup"
```

---

### Task 5: Validation commands (`validate_fallback_context`, `recognized_context_size`)

**Files:**
- Modify: `src-tauri/src/commands.rs` (add types + commands + tests; register in `lib.rs`)

**Interfaces:**
- Consumes: `AppState` (incl. `.config`, `.catalog`), `Model.context_size` (Task 3), `ModelCatalog::context_size` (Task 1).
- Produces: Tauri commands `validate_fallback_context(primary_id, fallback_id) -> ContextCheckResult`, `recognized_context_size(provider_id, upstream_model_id) -> Option<u32>`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src-tauri/src/commands.rs`. These are **plain sync `#[test]`s**
that exercise the pure `classify_fallback` directly — no `AppState`, no Tauri `State`, no temp dir.
They build `Model` values + parse the embedded catalog (so they stay green if the JSON changes):

```rust
    use crate::config::catalog::{EMBEDDED_CATALOG, ModelCatalog};
    use crate::config::{ContextCheckStatus, Model, ModelSource};

    fn m(id: &str, pid: &str, up: &str, ctx: Option<u32>) -> Model {
        Model {
            id: id.into(), provider_id: pid.into(), display_name: id.into(),
            source: ModelSource::Manual, upstream_model_id: up.into(),
            cooldown_seconds: None, fallback_target_model_id: None, context_size: ctx,
        }
    }
    fn cat() -> ModelCatalog {
        serde_json::from_str(EMBEDDED_CATALOG).unwrap()
    }

    #[test]
    fn classify_ok_when_fallback_larger_or_equal() {
        let cat = cat();
        let p = m("p", "zhipu", "glm-4.5-air", None);    // catalog: 128000
        let f = m("f", "zhipu", "glm-4.6", None);         // catalog: 200000
        let r = classify_fallback(&p, &f, &cat);
        assert_eq!(r.status, ContextCheckStatus::Ok);
        assert_eq!(r.primary_size, Some(128000));
        assert_eq!(r.fallback_size, Some(200000));
    }

    #[test]
    fn classify_smaller_when_fallback_too_small() {
        let cat = cat();
        let p = m("p", "zhipu", "glm-4.6", None);         // 200000
        let f = m("f", "zhipu", "glm-4.5-air", None);     // 128000
        let r = classify_fallback(&p, &f, &cat);
        assert_eq!(r.status, ContextCheckStatus::Smaller);
    }

    #[test]
    fn classify_unknown_when_size_missing() {
        let cat = cat();
        let p = m("p", "zhipu", "glm-4.6", None);         // 200000
        let f = m("f", "custom", "weird-model", None);    // not in catalog, no manual
        let r = classify_fallback(&p, &f, &cat);
        assert_eq!(r.status, ContextCheckStatus::Unknown);
        assert_eq!(r.fallback_size, None);
    }

    #[test]
    fn manual_override_flips_verdict() {
        // fallback catalog=128000, but user manually sets it to 300000 -> Ok
        let cat = cat();
        let p = m("p", "zhipu", "glm-4.6", None);         // 200000
        let f = m("f", "zhipu", "glm-4.5-air", Some(300000));
        let r = classify_fallback(&p, &f, &cat);
        assert_eq!(r.status, ContextCheckStatus::Ok);
        assert_eq!(r.fallback_size, Some(300000));
    }
```

> The async `validate_fallback_context` wrapper (Step 3) is just lock + lookup + `classify_fallback`,
> so these pure tests fully cover the classification logic; the wrapper is verified by the build +
> the manual end-to-end check in Task 9.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml classify_`
Expected: COMPILE ERROR — `ContextCheckStatus`, `classify_fallback` not defined.

- [ ] **Step 3: Write minimal implementation**

In `src-tauri/src/commands.rs`:

1. Near the other result structs (e.g. by `SettingsView` / `UsageEntry`), add:

```rust
use crate::config::{ContextCheckStatus, ContextCheckResult, Model, ModelCatalog};

/// Effective context size for a model: explicit `context_size` wins, else catalog lookup, else None.
pub fn effective_size(m: &Model, catalog: &ModelCatalog) -> Option<u32> {
    m.context_size
        .or_else(|| catalog.context_size(&m.provider_id, &m.upstream_model_id))
}

/// Pure classification (sync, unit-testable): no locks, no AppState — just the two models + the
/// catalog. The async command below resolves the models under the config lock, then calls this.
pub fn classify_fallback(
    primary: &Model,
    fallback: &Model,
    catalog: &ModelCatalog,
) -> ContextCheckResult {
    let p = effective_size(primary, catalog);
    let f = effective_size(fallback, catalog);
    let status = match (p, f) {
        (Some(ps), Some(fs)) if fs < ps => ContextCheckStatus::Smaller,
        (Some(_), Some(_)) => ContextCheckStatus::Ok,
        _ => ContextCheckStatus::Unknown,
    };
    ContextCheckResult { status, primary_size: p, fallback_size: f }
}
```

2. Add the thin `#[tauri::command]` wrappers. The async command resolves both models under the
   config read lock (`.read().await` — safe on the runtime; do **not** use `blocking_read()` from
   an async command, it panics inside the runtime), then calls the pure `classify_fallback`.
   `State` and `AppState` are already imported in `commands.rs` (used by existing commands), so no
   new import is needed.

```rust
#[tauri::command]
pub async fn validate_fallback_context(
    state: State<'_, AppState>,
    primary_id: String,
    fallback_id: String,
) -> Result<ContextCheckResult, String> {
    let (primary, fallback) = {
        let cfg = state.config.read().await;
        let p = cfg.models.iter().find(|m| m.id == primary_id)
            .ok_or_else(|| format!("model {primary_id} not found"))?.clone();
        let f = cfg.models.iter().find(|m| m.id == fallback_id)
            .ok_or_else(|| format!("model {fallback_id} not found"))?.clone();
        (p, f)
    };
    Ok(classify_fallback(&primary, &fallback, &state.catalog))
}

#[tauri::command]
pub fn recognized_context_size(
    state: State<'_, AppState>,
    provider_id: String,
    upstream_model_id: String,
) -> Option<u32> {
    state.catalog.context_size(&provider_id, &upstream_model_id)
}
```

4. Define the result types. Add to `src-tauri/src/config/types.rs` (so they're re-exported via `pub use types::*` and live with the other domain types):

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ContextCheckStatus {
    Ok,
    Smaller,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextCheckResult {
    pub status: ContextCheckStatus,
    pub primary_size: Option<u32>,
    pub fallback_size: Option<u32>,
}
```

(`commands.rs` then imports them via `use crate::config::{ContextCheckResult, ContextCheckStatus, ...}` — drop them from the local `use` if duplicated.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml classify_`
Expected: PASS (the four `classify_*` tests).

- [ ] **Step 5: Register the commands**

In `src-tauri/src/lib.rs` `generate_handler!` list, add (alphabetically near the other model commands):

```rust
            commands::set_model_fallback,
            commands::validate_fallback_context,
            commands::recognized_context_size,
```

- [ ] **Step 6: Build to confirm the binary compiles with the handler wired**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: compiles (warnings OK; fix any `-D warnings` issues from unused imports introduced in Task 1).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/config/types.rs src-tauri/src/lib.rs
git commit -m "feat(fallback): add context-size validation command + recognized-size hint"
```

---

### Task 6: Frontend types + invoke wrappers

**Files:**
- Modify: `src/lib/types.ts` (add `ContextCheckStatus` / `ContextCheckResult`)
- Modify: `src/lib/commands.ts` (add two wrappers)

**Interfaces:**
- Consumes: the Rust command signatures (Task 5).
- Produces: `validateFallbackContext(primaryId, fallbackId)`, `recognizedContextSize(providerId, upstreamModelId)`; `ContextCheckResult`/`ContextCheckStatus` types for Tasks 7–8.

- [ ] **Step 1: Add the TS types**

In `src/lib/types.ts`, append:

```ts
export type ContextCheckStatus = "ok" | "smaller" | "unknown";

export interface ContextCheckResult {
  status: ContextCheckStatus;
  primary_size?: number | null;
  fallback_size?: number | null;
}
```

- [ ] **Step 2: Add the invoke wrappers**

In `src/lib/commands.ts`, add to the imports from `./types`: `ContextCheckResult`. Then add (in the `// ---- backing + fallback ----` section):

```ts
export const validateFallbackContext = (primaryId: string, fallbackId: string) =>
  invoke<ContextCheckResult>("validate_fallback_context", { primaryId, fallbackId });
export const recognizedContextSize = (providerId: string, upstreamModelId: string) =>
  invoke<number | null>("recognized_context_size", { providerId, upstreamModelId });
```

- [ ] **Step 3: Typecheck**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts
git commit -m "feat(api): add fallback context-check + recognized-size wrappers"
```

---

### Task 7: `Fallback.vue` validated change flow

**Files:**
- Create: `src/lib/format.ts`
- Modify: `src/views/Fallback.vue` (script imports + `onChange`; no template change needed — `NSelect` already `:value`-bound)

**Interfaces:**
- Consumes: `validateFallbackContext` (Task 6), `config.setFallback` (store), `useDialog`/`useMessage`.
- Produces: a soft warning on `Smaller` (cancel reverts), an info note on `Unknown`, otherwise save.

- [ ] **Step 1: Create the shared formatter + update script imports**

Create `src/lib/format.ts` (shared by Fallback.vue and Models.vue):

```ts
export function fmtTokens(n?: number | null): string {
  if (n == null) return "未知";
  if (n >= 1_000_000) return `${(n / 1_000_000).toLocaleString()}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}K`;
  return String(n);
}
```

In `src/views/Fallback.vue`, change the imports:
- Add `useDialog` to the `naive-ui` import.
- Add `import { validateFallbackContext } from "../lib/commands";`.
- Add `import { fmtTokens } from "../lib/format";`.

```ts
import { computed, onMounted } from "vue";
import { NCard, NEmpty, NInputNumber, NSelect, NSpace, useDialog, useMessage } from "naive-ui";
import { useConfigStore } from "../stores/config";
import type { Model } from "../lib/types";
import { validateFallbackContext } from "../lib/commands";
import { fmtTokens } from "../lib/format";
import { ellipsisLabel } from "../lib/selectLabel";
import draggable from "vuedraggable";
import { useOrdered } from "../lib/useOrdered";

const msg = useMessage();
const dialog = useDialog();
```

- [ ] **Step 2: Replace `onChange` with the validated flow**

Replace the existing `onChange` function (lines 29–36) with (note: `fmtTokens` is imported, not local):

```ts
async function commitFallback(modelId: string, value: string | null) {
  try {
    await config.setFallback(modelId, value);
    msg.success("已更新 fallback");
  } catch (e) {
    msg.error(`更新失败：${String(e)}`);
  }
}

async function onChange(modelId: string, value: string) {
  if (!value) {
    await commitFallback(modelId, null);   // clearing fallback: no check
    return;
  }
  let res;
  try {
    res = await validateFallbackContext(modelId, value);
  } catch (e) {
    msg.error(`校验失败：${String(e)}`);
    return;
  }
  if (res.status === "smaller") {
    dialog.warning({
      title: "回退模型上下文较小",
      content: `回退模型上下文（${fmtTokens(res.fallback_size)}）小于主模型（${fmtTokens(res.primary_size)}），降级时可能截断上下文。仍要设置该回退吗？`,
      positiveText: "仍要设置",
      negativeText: "取消",
      onPositiveClick: () => commitFallback(modelId, value),
      // onNegativeClick / dismiss: do nothing -> NSelect reverts to config.fallback[modelId]
    });
    return;
  }
  if (res.status === "unknown") {
    msg.info("无法确定上下文大小（不在目录且未手动设置），已跳过该校验");
  }
  await commitFallback(modelId, value);
}
```

The `NSelect` `@update:value="(v) => onChange(m.id, String(v))"` binding stays unchanged — because it is one-way `:value`-bound to `config.fallback[m.id]`, not committing on cancel naturally reverts the dropdown to the stored value.

- [ ] **Step 3: Typecheck**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/lib/format.ts src/views/Fallback.vue
git commit -m "feat(fallback): warn when fallback context < primary; note on unknown"
```

---

### Task 8: `Models.vue` manual `context_size` field + recognized hint

**Files:**
- Modify: `src/views/Models.vue` (`FormState`, `blank`, `openEdit`, `buildModel`, imports, template)

**Interfaces:**
- Consumes: `Model.context_size` (Task 3), `recognizedContextSize` (Task 6).
- Produces: an optional 上下文大小 input + a read-only "从目录识别：…" hint.

- [ ] **Step 1: Extend the form state**

In `src/views/Models.vue`:
- Add `NInputNumber` to the `naive-ui` import.
- Add `context_size: number | null;` to the `FormState` interface.
- Set `context_size: null,` in `blank()`.
- In `openEdit`, load it: `form.context_size = m.context_size ?? null;`.
- In `buildModel`, carry it: add `context_size: form.context_size,` to the returned object (and keep `fallback_target_model_id: null` as-is — out of scope here).

```ts
interface FormState {
  id: string;
  provider_id: string;
  upstream_model_id: string;
  display_name: string;
  source: ModelSource;
  cooldown: number | null;
  context_size: number | null;
}
const blank = (): FormState => ({
  id: "",
  provider_id: config.providers[0]?.id ?? "",
  upstream_model_id: "",
  display_name: "",
  source: "manual",
  cooldown: null,
  context_size: null,
});
```

In `openEdit` add after `form.cooldown = m.cooldown_seconds ?? null;`:
```ts
  form.context_size = m.context_size ?? null;
```

In `buildModel` add to the returned object (before the closing `}`):
```ts
    context_size: form.context_size,
```

- [ ] **Step 2: Add the recognized-size hint**

Add a reactive hint + watcher. In `<script setup>`, after the `availableModels` block. `fmtTokens`
comes from the shared `src/lib/format.ts` created in Task 7:

```ts
import { recognizedContextSize } from "../lib/commands";
import { fmtTokens } from "../lib/format";

const recognizedHint = ref<string>("");
async function refreshRecognized() {
  const pid = form.provider_id;
  const up = form.upstream_model_id.trim();
  if (!pid || !up || form.context_size != null) { recognizedHint.value = ""; return; }
  try {
    const n = await recognizedContextSize(pid, up);
    recognizedHint.value = n == null ? "" : fmtTokens(n);
  } catch {
    recognizedHint.value = "";
  }
}
watch(() => [form.provider_id, form.upstream_model_id, form.context_size], refreshRecognized);
```

- [ ] **Step 3: Add the form item to the template**

Inside the `<NForm>` in the modal (after the upstream-model `NFormItem`), add:

```vue
        <NFormItem label="上下文大小（可选，token）">
          <NInputNumber
            v-model:value="form.context_size"
            :min="0"
            :show-button="false"
            placeholder="留空则按目录自动识别"
            clearable
            style="width: 200px"
          />
          <span v-if="!form.context_size && recognizedHint" class="muted" style="margin-left: 8px">
            从目录识别：{{ recognizedHint }}
          </span>
        </NFormItem>
```

- [ ] **Step 4: Typecheck**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/views/Models.vue
git commit -m "feat(models): optional context_size field with catalog-recognized hint"
```

---

### Task 9: Full verification + warnings cleanup

**Files:** none (verification only, plus any fix-ups).

- [ ] **Step 1: Run the full Rust test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all catalog + state + commands + types tests).

- [ ] **Step 2: Check for unused-import / dead-code warnings introduced by this work**

Run: `cargo build --manifest-path src-tauri/Cargo.toml 2>&1`
Inspect the output for `unused import` / `field is never read` / `function is never used` warnings introduced in Tasks 1–5 and fix them (e.g. the `use std::path::{Path, PathBuf};` and `use super::store::StoreError;` in `catalog.rs` are used by Task 2; the `AppStateInner` import was deliberately not added to `commands.rs`; if `recognised_context_size`/`classify_fallback`/`effective_size` show "never used" they are used via the registered command). If the project builds with `-D warnings`, this is mandatory.

- [ ] **Step 3: Frontend typecheck + build**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Manual end-to-end check (run the app)**

Launch the app (`npm run tauri dev` or the project's run mechanism). Verify:
1. First launch writes `%APPDATA%\com.switchlm.app\model_desc.json` (open it; it matches the bundled catalog).
2. **Smaller**: set a 200K Zhipu primary's fallback to a 128K model → warning dialog appears with "128K / 200K"; Cancel reverts the dropdown, Confirm saves.
3. **Unknown**: set a fallback to a custom model not in the catalog (no manual size) → info note, then saves.
4. **Ok**: set a 128K primary's fallback to a 200K model → saves silently with success toast.
5. **Manual override**: in Models, set a model's 上下文大小 to a large value; set it as a fallback for a smaller-catalog primary → result reflects the manual value.
6. **Merge**: hand-edit `model_desc.json` to add a custom entry + tweak a size; restart → custom entry preserved, bundled new entries (if any in a future version) added, tweaked value retained.
7. **Malformed**: corrupt `model_desc.json` (e.g. `{ bad`); restart → file re-seeded to the bundled default, app starts normally.

- [ ] **Step 5: Commit any fix-ups**

```bash
git add -A
git commit -m "chore(fallback): verification fix-ups"   # only if something changed
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** every spec section maps to a task — catalog data (T1), embed/seed/merge (T1+T2), `Model.context_size` (T3), AppState/startup (T4), validation command + recognized hint (T5), registration (T5), frontend types/wrappers (T6), Fallback UX (T7), Models field+hint (T8), error/edge cases covered by T2/T9 (malformed re-seed, merge conflict, missing size). ✅
- **Placeholders:** none — every code/test step shows concrete content; the only "verify" items are external-data casing (Volcengine/Zhipu upstream ids), which degrade gracefully to "unknown" if wrong. ✅
- **Type consistency:** `ContextCheckStatus`/`ContextCheckResult` defined once in `types.rs`, re-exported, imported in `commands.rs`, mirrored in `types.ts`. `effective_size`, `classify_fallback`, `validate_fallback_context`, `recognized_context_size`, `ensure_catalog`, `catalog_path`, `merge_new_from`, `context_size` (method) names are consistent across tasks. `Model.context_size: Option<u32>` matches TS `number | null`. ✅
