# Fallback Context-Size Check Design

**Date:** 2026-07-29
**Status:** Approved
**Author:** Claude (SwitchLM Project)

> **Catalog persistence — superseded 2026-08-04.** The §3 "bundled catalog seeded to `<app_data>/model_desc.json` and union-merged (AppData wins)" design was replaced: the asset file is now `src-tauri/assets/provider_desc.json` (provider-nested `{ providers: [{ provider_id, models: [{ upstream_model_id, context_size }] }] }`), loaded as an in-memory baseline on every launch (never persisted), with user overrides in a separate sparse `<app_data>/custom_provider_desc.json` that overlays the baseline (at startup, and in real-time when the Models page edits a context size via `set_custom_context_size` — which writes disk and re-derives the in-memory catalog under a write lock in one step). `Model` no longer carries a per-instance `context_size` field; the catalog is the single source. This fixes the original bug where a changed bundled `context_size` for an already-known model was silently kept stale on disk. The fallback context-size *check* logic itself (the rest of this spec) is unchanged.

## Overview

When a model trips its circuit breaker on a 429/quota error, SwitchLM walks the per-model
`fallback_target_model_id` chain (`src-tauri/src/proxy/dispatch.rs`) to a backup model. If the
fallback's **context window is smaller than the primary's**, a conversation that fit on the primary
can be truncated (or rejected) on the fallback — a silent failure the user only notices mid-outage.

This design adds a **configuration-time guard**: when the user sets a fallback, compare the two
models' context sizes and warn if the fallback is smaller. It also ships a vendor/model description
catalog (`vendor_model_desc.json`) so the app *knows* each bundled model's context size, embedded in
the binary and seeded into AppData on first run.

Behaviour, confirmed with the user:

- **Context-size source** — catalog lookup **+ manual override**. The `Model` struct gains an
  optional `context_size`; the effective size is `model.context_size` (if set) → else catalog lookup
  → else *unknown*.
- **Fallback smaller (both sizes known)** — **soft warning dialog**, allow on confirm.
- **Either size unknown** — **allow, but note the gap** (non-blocking info message).
- **Catalog across app versions** — **merge** new embedded entries into the existing AppData file on
  startup; the AppData/user value wins on conflict.

## Requirements

### Functional Requirements

1. **Bundled catalog** — Ship `vendor_model_desc.json` with context sizes for the 3 bundled
   vendor/plans (智谱, 火山 agent-plan, 火山 coding-plan), embedded into the binary at compile time.
2. **First-run seed** — On first launch, write the embedded catalog to
   `%APPDATA%\com.switchlm.app\model_desc.json` if it does not exist.
3. **AppData is authoritative** — On subsequent launches, read `model_desc.json` from AppData; merge
   in any new embedded entries (keyed by `provider_id` + `upstream_model_id`), never overwriting an
   existing AppData entry. User-added / cloud-updated entries are preserved.
4. **Manual override field** — `Model.context_size: Option<u32>` (input/total context window in
   tokens), editable in the Models add/edit modal, persisted in `app_config.json`.
5. **Fallback validation command** — A backend command resolves both models' effective sizes and
   returns `Ok | Smaller | Unknown`.
6. **Warning UX** — Setting a fallback whose size is smaller pops a confirm/cancel dialog (cancel
   reverts the dropdown); an unknown size shows a non-blocking note; both still allow saving.
7. **Backward compatible** — Old `app_config.json` without `context_size` loads as `None`
  (`#[serde(default)]`); old/missing `model_desc.json` is seeded.

### Non-Functional Requirements

1. **Single source of truth** — the size-resolution rule and the catalog live in Rust; the frontend
   only renders the verdict (catalog data is not shipped to the frontend bundle).
2. **No new dependencies** — embedding uses stdlib `include_str!` (single file).
3. **Graceful degradation** — a malformed or missing catalog never crashes startup; validation just
   reports `Unknown`.
4. **Follows existing conventions** — `dir: &Path` functions (temp-dir testable), per-field serde
   defaults, typed invoke wrappers, store actions, `NSelect`/`NDialog`/`NMessage` UI.
5. **No runtime change** — the dispatch fallback walk is untouched; this is purely a config-time guard.

## Effective-Size Resolution (the one rule)

```
effective_size(model):
    model.context_size                 // Some -> explicit override / custom-model value wins
    ?? catalog.lookup(provider_id, upstream_model_id)   // else bundled catalog
    ?? None                            // else unknown -> validation reports Unknown
```

The catalog value is **never auto-snapshotted** into `model.context_size` on creation — that would go
stale after a startup-merge updates the catalog. The field is purely an explicit override / value for
non-catalog models. The catalog-derived size is still *shown* as a read-only hint on the Models page.

## Data Model

### Catalog file (`src-tauri/assets/vendor_model_desc.json`)

Flat list keyed by `(provider_id, upstream_model_id)` — the same identity SwitchLM already uses — with
a `version` reserved for future schema migration:

```json
{
  "version": 1,
  "models": [
    {
      "provider_id": "zhipu",
      "upstream_model_id": "glm-4.6",
      "context_size": 131072,
      "display_name": "GLM-4.6"
    }
  ]
}
```

A flat list (vs. a nested vendor→model map) makes the union-merge and lookup trivial — both are keyed
on the same 2-tuple — and is easy to extend with per-model metadata.

**`context_size` = the input/total context window in tokens** (the figure that decides whether a
fallback can hold the in-flight conversation). At extraction time I will confirm each source page
exposes a single comparable context-window number; if a page splits input vs. output, I use the
input/total window and note it in the commit.

> **Assumption to verify at extraction:** `provider_id` doubles as the vendor discriminator today
> (`zhipu`, `volcengine-agent`, `volcengine-coding`), confirmed by `usage/mod.rs::usage_provider_for`
> and `Provider.vue::vendorOptions`. The catalog is keyed on `provider_id`. If provider id can drift
> from vendor, a `vendor` field is added instead; otherwise this stays.

**Source pages** (context sizes to extract):

| Vendor | provider_id | Source URL |
|---|---|---|
| 智谱 | `zhipu` | https://docs.bigmodel.cn/cn/guide/start/model-overview#模型一览 |
| 火山 agent-plan | `volcengine-agent` | https://console.volcengine.com/ark/region:cn-beijing/docs/82379/2366394?lang=zh#c90d28c2 |
| 火山 coding-plan | `volcengine-coding` | https://console.volcengine.com/ark/region:cn-beijing/docs/82379/1925114?lang=zh |

### Rust catalog structs

```rust
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
    /// Catalog-only lookup (ignores any manual override). Returns None if not bundled.
    pub fn context_size(&self, provider_id: &str, upstream_model_id: &str) -> Option<u32> { ... }

    /// Union-merge: append embedded entries whose (provider_id, upstream_model_id) key is absent;
    /// never overwrite an existing entry (AppData/user value wins). `version` is passthrough
    /// (disk value retained) — only relevant once real schema migration is added.
    pub fn merge_new_from(&mut self, embedded: &ModelCatalog) { ... }
}
```

### `Model` struct change (`src-tauri/src/config/types.rs:91`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    // ... existing fields ...
    /// Explicit context-window override / value for non-catalog models (input tokens).
    /// Effective size = this, else catalog lookup, else unknown. Not auto-filled from the catalog
    /// (would go stale after a startup-merge) — set only when the user opts in.
    #[serde(default)]
    pub context_size: Option<u32>,
}
```

Update `app_config_roundtrips` to include `context_size`.

### Validation result (`src-tauri/src/commands.rs`)

```rust
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextCheckStatus { Ok, Smaller, Unknown }

#[derive(Serialize)]
pub struct ContextCheckResult {
    pub status: ContextCheckStatus,
    pub primary_size: Option<u32>,
    pub fallback_size: Option<u32>,
}
```

- `Unknown` if either size is `None`.
- `Smaller` if both `Some` and `fallback_size < primary_size`.
- `Ok` if both `Some` and `fallback_size >= primary_size`.

## Embedding & First-Run / Merge Logic

**Embedding** — in `config/catalog.rs`:
```rust
const EMBEDDED_CATALOG: &str = include_str!("../../assets/vendor_model_desc.json");
```
(Path resolves from `src/config/` up to `src-tauri/assets/`.)

**`config/catalog.rs` API** (all take `dir: &Path`, temp-dir testable like `store.rs`):

```rust
pub fn catalog_path(dir: &Path) -> PathBuf { dir.join("model_desc.json") }

fn parse_embedded() -> ModelCatalog {
    // Compiled-in, so malformed => build/test failure; defensively log + empty on parse error.
    serde_json::from_str(EMBEDDED_CATALOG).unwrap_or_else(|e| {
        tracing::error!("内嵌 catalog 解析失败，降级为空：{e}");
        ModelCatalog::default()
    })
}

pub fn save_catalog(dir: &Path, cat: &ModelCatalog) -> Result<(), StoreError> {
    if !dir.exists() { std::fs::create_dir_all(dir)?; }
    let text = serde_json::to_string_pretty(cat)?;
    std::fs::write(catalog_path(dir), text)?;
    Ok(())
}

pub fn ensure_catalog(dir: &Path) -> ModelCatalog {
    let embedded = parse_embedded();
    if !catalog_path(dir).exists() {
        // FIRST RUN: write embedded, return it.
        let _ = save_catalog(dir, &embedded);
        return embedded;
    }
    // EXISTS: read AppData copy. Malformed => re-seed from embedded (self-heal).
    let mut disk = match std::fs::read_to_string(catalog_path(dir))
        .ok()
        .and_then(|t| serde_json::from_str::<ModelCatalog>(&t).ok())
    {
        Some(c) => c,
        None => { let _ = save_catalog(dir, &embedded); return embedded; }
    };
    disk.merge_new_from(&embedded);   // add new app-version entries; keep user/cloud ones
    let _ = save_catalog(dir, &disk); // persist so disk stays current
    disk
}
```

`save_catalog` mirrors `store::save` (`create_dir_all` then pretty-write). Errors during seed/merge
write are logged (`let _ =`) and ignored — the in-memory catalog is still returned so startup never
fails over a catalog issue.

**Startup integration** (`src-tauri/src/lib.rs` setup hook, right after `config::store::load`):
```rust
let cfg = config::store::load(&dir)?;
let catalog = config::catalog::ensure_catalog(&dir);   // <-- new
...
let state: proxy::AppState = Arc::new(AppStateInner {
    config: tokio::sync::RwLock::new(cfg),
    catalog,                                            // <-- new field
    ...
});
```

`AppStateInner` gains `pub catalog: ModelCatalog` (immutable for the session — it only changes via
the startup merge or a future cloud update). `AppStateInner::load` (`proxy/state.rs:37`) also calls
`ensure_catalog(dir)` so tests stay consistent; existing temp-dir tests are unaffected.

## Validation Command + UI

### Backend commands (`src-tauri/src/commands.rs`)

```rust
#[tauri::command]
pub async fn validate_fallback_context(
    state: State<'_, AppState>,
    primary_id: String,
    fallback_id: String,
) -> Result<ContextCheckResult, String> {
    let (cfg, catalog) = {
        let c = state.config.read().await;
        (c.clone(), state.catalog.clone())   // cheap clones; catalog is small
    };
    let primary = cfg.models.iter().find(|m| m.id == primary_id)
        .ok_or_else(|| format!("model {primary_id} not found"))?;
    let fallback = cfg.models.iter().find(|m| m.id == fallback_id)
        .ok_or_else(|| format!("model {fallback_id} not found"))?;
    let p = effective_size(primary, &catalog);
    let f = effective_size(fallback, &catalog);
    let status = match (p, f) {
        (Some(ps), Some(fs)) if fs < ps => ContextCheckStatus::Smaller,
        (Some(_), Some(_)) => ContextCheckStatus::Ok,
        _ => ContextCheckStatus::Unknown,
    };
    Ok(ContextCheckResult { status, primary_size: p, fallback_size: f })
}

/// Catalog-only size hint for the Models add/edit modal ("从目录识别: 128K").
#[tauri::command]
pub fn recognized_context_size(
    state: State<'_, AppState>,
    provider_id: String,
    upstream_model_id: String,
) -> Option<u32> {
    state.catalog.context_size(&provider_id, &upstream_model_id)
}
```

where
```rust
fn effective_size(m: &Model, catalog: &ModelCatalog) -> Option<u32> {
    m.context_size
        .or_else(|| catalog.context_size(&m.provider_id, &m.upstream_model_id))
}
```

Register both in `generate_handler!` (`src-tauri/src/lib.rs`).

### Frontend (`src/views/Fallback.vue`)

Add `useDialog` to the imports, and import the read wrapper directly from the api module
(`import { validateFallbackContext } from "../lib/commands"` — read commands need no store wrapper,
matching how other reads are wired). Replace `onChange` with a validated flow (skip validation when
clearing — `value === ""`):

```ts
async function onChange(modelId: string, value: string) {
  if (!value) {                                  // clearing fallback — no check
    await commitFallback(modelId, null);
    return;
  }
  const res = await validateFallbackContext(modelId, value);
  if (res.status === "smaller") {
    dialog.warning({
      title: "回退模型上下文较小",
      content: `回退模型上下文（${fmt(res.fallback_size)}）小于主模型（${fmt(res.primary_size)}），`
             + `降级时可能截断上下文。仍要设置该回退吗？`,
      positiveText: "仍要设置",
      negativeText: "取消",
      onPositiveClick: () => commitFallback(modelId, value),
      onNegativeClick: () => {},                  // dropdown reverts (value not committed)
    });
    return;
  }
  if (res.status === "unknown") {
    msg.info("无法确定上下文大小（不在目录且未手动设置），已跳过该校验");
  }
  await commitFallback(modelId, value);
}

async function commitFallback(modelId: string, value: string | null) {
  try {
    await config.setFallback(modelId, value);
    msg.success("已更新 fallback");
  } catch (e) {
    msg.error(`更新失败：${String(e)}`);
  }
}
```

`fmt(size?: number | null)` renders `131072` → `128K`. Because `NSelect` is `:value`-bound to
`config.fallback[m.id]` (not `v-model`), not committing on cancel naturally reverts the dropdown to
the stored value — matching the existing one-way binding.

### Frontend (`src/views/Models.vue`)

Add an optional **上下文大小** field to the modal. `FormState` gains `context_size: number | null`;
`openEdit` loads `m.context_size ?? null`; `buildModel` carries `context_size: form.context_size`.
A hint line shows the catalog-recognized size (via `api.recognizedContextSize(provider_id, upstream)`)
when the manual field is empty and a value is recognized.

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

### Frontend types + wrappers

- `src/lib/types.ts`: `context_size?: number | null` on `Model`; new `ContextCheckStatus` /
  `ContextCheckResult` types.
- `src/lib/commands.ts`:
  ```ts
  export const validateFallbackContext = (primaryId: string, fallbackId: string) =>
    invoke<ContextCheckResult>("validate_fallback_context", { primaryId, fallbackId });
  export const recognizedContextSize = (providerId: string, upstreamModelId: string) =>
    invoke<number | null>("recognized_context_size", { providerId, upstreamModelId });
  ```

## Error Handling

| Situation | Behavior |
|---|---|
| Embedded JSON malformed | Compiled-in/CI-checked; defensively → log + empty catalog. App runs, checks report `Unknown`. |
| `model_desc.json` missing | First-run seed from embedded. |
| `model_desc.json` malformed (hand-edit/corruption) | Re-seed from embedded (self-heal), log warning. |
| Unknown catalog fields (future `version` schema) | `#[serde(default)]` → ignored gracefully. |
| Seed/merge write fails (disk full/permissions) | Log (`let _ =`), keep in-memory catalog for session; startup continues. |
| `validate_fallback_context` given a missing model id | `Err(String)` (UI never sends one; defensive). |
| `dir` missing on save | `create_dir_all` (mirrors `store::save`). |

## Testing Strategy

All catalog tests use a **real temp dir** (existing pattern in `proxy/state.rs` tests with
`tempfile::tempdir`), exercising actual file I/O — not a mocked store (per the project's
"test real impl, not just the fake" rule).

### Backend unit tests (`config/catalog.rs`)

- `ensure_catalog` first run (no file) → file created, content == embedded, returned catalog matches.
- `ensure_catalog` existing file → disk entries preserved.
- **Merge — new embedded entry added**: disk lacks a key embedded has → present after `ensure_catalog`;
  persisted back to disk.
- **Merge — conflict, disk wins**: disk and embedded both have a key with different sizes → disk value
  retained.
- **Merge — user-only entry preserved**: disk has a key embedded lacks → still present.
- Malformed disk file → re-seeded to embedded.
- `effective_size`: manual `Some` overrides catalog; catalog used when manual `None`; `None` when
  neither.
- `EMBEDDED_CATALOG` parses and every entry has `context_size > 0` (guards the shipped file).

### Validation tests (`commands.rs`, fixed `AppState` + catalog)

- Both known, `fallback >= primary` → `Ok`.
- Both known, `fallback < primary` → `Smaller` with correct sizes.
- Either unknown → `Unknown`.
- Manual override flips the verdict (e.g. primary manually raised above catalog → `Smaller`).

### Frontend

- `vue-tsc --noEmit` passes.
- Manual: set a smaller fallback → warning dialog appears, cancel reverts / confirm saves; unknown →
  info note then saves; Models modal hint shows recognized size.

## Files Changed

### Backend
1. `src-tauri/assets/vendor_model_desc.json` — **new** bundled catalog.
2. `src-tauri/src/config/catalog.rs` — **new** module: structs, embedding, `ensure_catalog`, merge,
   lookup, tests.
3. `src-tauri/src/config/mod.rs` — `pub mod catalog;`.
4. `src-tauri/src/config/types.rs` — `Model.context_size` + roundtrip test.
5. `src-tauri/src/proxy/state.rs` — `catalog` field on `AppStateInner`; `load()` calls `ensure_catalog`.
6. `src-tauri/src/commands.rs` — `ContextCheckStatus`/`ContextCheckResult`, `effective_size`,
   `validate_fallback_context`, `recognized_context_size`, validation tests.
7. `src-tauri/src/lib.rs` — `ensure_catalog` in setup; register both commands.

### Frontend
1. `src/lib/types.ts` — `context_size` on `Model`; `ContextCheckStatus` / `ContextCheckResult`.
2. `src/lib/commands.ts` — `validateFallbackContext`, `recognizedContextSize`.
3. `src/views/Fallback.vue` — validated `onChange` + confirm dialog + info note + size formatter.
4. `src/views/Models.vue` — optional `context_size` field + recognized-size hint.

## Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Validation location | Backend resolves, returns verdict | Single source of truth; catalog stays in Rust; matches existing "backend resolves, frontend renders" pattern. |
| Context-size source | Catalog + manual override field | Covers the 3 bundled plans *and* arbitrary custom models; manual value wins to avoid staleness. |
| Auto-fill catalog into field | No | A snapshot would go stale after a startup-merge; field is explicit-override only. |
| Smaller size | Soft warning, allow on confirm | User retains choice; non-blocking. |
| Unknown size | Allow + non-blocking note | Avoids false positives; keeps the user aware without blocking. |
| Catalog across versions | Merge new entries, AppData wins | New app-version models reach existing users; user/cloud additions never clobbered. |
| Embedding | `include_str!` | Single file, stdlib, no new crate. |
| Dispatch walk | Untouched | This is a config-time guard; runtime fallback-on-429 is unchanged. |

## Success Criteria

1. ✅ `vendor_model_desc.json` is embedded and seeded to `model_desc.json` on first run.
2. ✅ Existing `model_desc.json` is read authoritatively; new embedded entries merge in; user entries
   are preserved across app updates.
3. ✅ Malformed/missing catalog never crashes startup (validation reports `Unknown`).
4. ✅ `Model.context_size` persists in `app_config.json`; old files load with `None`.
5. ✅ Setting a smaller fallback shows a confirm/cancel dialog; cancel reverts, confirm saves.
6. ✅ Unknown size allows saving with a non-blocking note.
7. ✅ Manual `context_size` overrides the catalog value in the check.
8. ✅ Catalog + validation unit tests pass; `vue-tsc --noEmit` passes.
