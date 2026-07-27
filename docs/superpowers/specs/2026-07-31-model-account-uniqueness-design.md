# Per-Account Model Uniqueness + `display_name` Removal

**Date:** 2026-07-31
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

## Overview

Two related cleanups to the model configuration, both flowing from the multi-account model
(a Provider *is* an account):

1. **One model per account.** The same `upstream_model_id` may be added to a given account
   (provider) only once. Different accounts may each have it.
2. **Drop the redundant `Model.display_name` field** (and the dead catalog `ModelDesc.display_name`).
   `Model.display_name` is never user-editable and is always forced equal to `upstream_model_id`,
   so it is pure duplication. With it gone, `upstream_model_id` is the model's single identity —
   which is also the uniqueness key.
3. **Terminology:** in the "新增模型" (add-model) form, relabel "服务商配置" → "账号", since the
   selector picks which account backs the model.

## Background

- `Models.vue`'s add-model form has **no** display-name input. `buildModel` sets
  `display_name: form.display_name.trim() || upstream` and `form.display_name` is always `""`
  (`blank()`), so every model's `display_name` is byte-identical to its `upstream_model_id`.
- The bundled catalog (`vendor_model_desc.json` → `ModelDesc`) has a `display_name` column too,
  but it is **parsed and never read** — `catalog.rs` only consumes `context_size`. It is dead data.
- `upsert_model` (`commands.rs:109`) is insert-or-replace by id with **no** uniqueness check, so the
  same `(provider_id, upstream_model_id)` can be added repeatedly today.
- `Provider.display_name` is the **account name** (per the multi-account design) and is **unaffected**.

## Requirements

### Functional

1. **Per-account model uniqueness.** A second model with the same `provider_id` AND the same
   `upstream_model_id` (after trim) as an existing model is rejected. Same `upstream_model_id` under
   a *different* provider is allowed. Editing a model (same `id`) is exempt from its own check.
2. **No `display_name` on models.** The field is removed from the persisted model and from the UI;
   every place that displayed it shows `upstream_model_id` instead.
3. **No `display_name` in the catalog.** The column is removed from `vendor_model_desc.json` and the
   field from `ModelDesc`.
4. **"账号" terminology in the add-model form.** The provider selector's label/placeholder and the
   related validation/hint text say "账号", not "服务商配置".
5. **Authoritative backend check.** The uniqueness rule is enforced in `upsert_model` so it cannot be
   bypassed by a frontend bug; the UI adds a pre-check for nicer UX (mirrors the `upsert_provider`
   pattern established by the multi-account design).

### Non-Functional

1. **Graceful migration.** Existing `app_config.json` models still carrying `display_name` load
   unchanged — serde ignores the now-unknown field. No explicit migration, no data loss (the value
   always equaled `upstream_model_id`).
2. **Single model identity.** `upstream_model_id` is the one name shown, echoed, and keyed on.

## Implementation Architecture

### 1. Data model — remove `Model.display_name`

**`src-tauri/src/config/types.rs`** — drop the `display_name: String` field from `Model` (and the
`#[serde(default)]`/positioning is moot once removed). `Provider.display_name` stays.

**`src/lib/types.ts`** — drop `display_name: string` from the `Model` mirror (line ~22; line ~11 is
`Provider`'s — keep).

Removing the field is safe on load: serde does **not** set `deny_unknown_fields` on `Model`, so a
legacy `"display_name":"…"` is silently dropped.

### 2. Catalog — remove `ModelDesc.display_name`

**`src-tauri/src/config/catalog.rs`** — remove `pub display_name: Option<String>` from `ModelDesc`
(line ~23); update the four `ModelDesc { … }` test fixtures (lines ~142/143/146/147).

**`src-tauri/assets/vendor_model_desc.json`** — remove the `"display_name": "…"` key from every
entry. `EMBEDDED_CATALOG` parses the trimmed JSON; `context_size` lookups are unchanged. (Note: this
file is already modified in the working tree for unrelated context-size tweaks — those edits remain.)

### 3. Uniqueness in `upsert_model` (backend, authoritative)

**`src-tauri/src/commands.rs`** — mirror `upsert_provider`'s pattern:

```rust
#[tauri::command]
pub async fn upsert_model(
    state: State<'_, AppState>,
    mut model: Model,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let id = model.id.clone();
    {
        model.upstream_model_id = model.upstream_model_id.trim().to_string();
        let mut cfg = state.config.write().await;
        let dup = cfg.models.iter().any(|m| {
            m.id != model.id
                && m.provider_id == model.provider_id
                && m.upstream_model_id == model.upstream_model_id
        });
        if dup {
            return Err(format!(
                "该账号下已存在模型「{}」",
                model.upstream_model_id
            ));
        }
        upsert(&mut cfg.models, model);
        persist(&app, &cfg)?;
    }
    state.health.reset(&id);
    Ok(())
}
```

Trim runs **before** both the comparison and persistence, so `"glm-4.6 "` and `"glm-4.6"` collide
(case preserved — upstream ids are case-sensitive). Editing a model keeps its `id`, so it is excluded
from its own check and can re-save freely. No new command is needed; no frontend command-signature
change.

### 4. Frontend pre-check + relabel

**`src/views/Models.vue`**

- **`save()` pre-check** (before `config.saveModel`): if a model with the same `provider_id` +
  trimmed `upstream_model_id` exists (excluding `form.id` when editing), `msg.warning(…)` and return
  without closing the modal. The backend remains the backstop.
- **Relabel:** form item label `服务商配置` → `账号`; select placeholder `选择服务商配置` →
  `选择账号`; the `msg.warning("服务商 / 模型名称 不能为空")` → `账号 / 模型名称 不能为空`; the
  top hint `…协议与端点继承服务商配置` → `…继承账号配置`.
- **Drop `display_name` from the form model:** remove it from `FormState` (line ~42), `blank()`
  (~54), `openEdit` (~77), and `buildModel` (~92 — no longer falls back to a display name).
- **List row (~205-206):** today it shows `<display_name>` then `<upstream_model_id>` as two spans.
  With them identical, collapse to a single span showing `upstream_model_id` (drop the duplicate).
- **Delete confirm (~118):** `确认删除「${m.display_name}」？` → `确认删除「${m.upstream_model_id}」？`.

### 5. Read-site migration (`display_name` → `upstream_model_id`)

**Backend** (every `Model.display_name` read — `Provider.display_name` untouched):
- `tray.rs:80` — `let mut s = model.display_name.clone();` → `model.upstream_model_id.clone()`.
- `dispatch.rs:65-66` — the "backfill display_name" comment + `unwrap_or_else(|| model.display_name.clone())`
  → `model.upstream_model_id.clone()` (the inbound-echo backfill).
- Test `Model { … }` literals that set `display_name`: `types.rs:194`, `commands.rs:1291`,
  `dispatch.rs:823`, `resolve.rs:41`, `openai_edge.rs:42`, `anthropic_edge.rs:40`, `tests/e2e_zhipu.rs:58`
  — drop the field.
- `types.rs:247` — the model JSON deserialization test string contains `"display_name":"GLM"`; drop
  that key (and any assertion on it) so the test stays consistent with the new shape.

**Frontend** (every model `display_name` read — provider reads untouched):
- `Profiles.vue:32,109`, `Dashboard.vue:101,112,124`, `Fallback.vue:25` — `m.display_name` →
  `m.upstream_model_id`.
- Provider reads to **leave alone**: `Usage.vue:42`, `selectLabel.ts:42,44`, `quotaUtils.ts:50`,
  `Provider.vue` (all), `commands.ts:87` (`ProviderConflict`).

### 6. Config migration

None required. Legacy `app_config.json` models with `display_name` deserialize fine (field ignored);
the next save writes the new shape. Because `display_name` always equaled `upstream_model_id`, no
distinguishable information is lost. (If an exotic legacy model had a *different* display_name, that
pretty label is dropped — accepted, since the field is being intentionally removed.)

## Error Handling

| Condition | Behavior |
|---|---|
| Duplicate `(provider_id, upstream_model_id)` on add | `upsert_model` returns `该账号下已存在模型「…」`; UI pre-check also warns and keeps the modal open |
| Duplicate on edit (same `id`) | Allowed — excluded from its own check |
| Same `upstream_model_id`, different provider | Allowed |
| Trailing whitespace (`"glm-4.6 "` vs `"glm-4.6"`) | Trimmed before compare → collides |
| Legacy model with `display_name` in config | Silently dropped on load; no error |

## Testing Strategy

### Rust unit tests (deterministic; no live HTTP)

**`commands.rs`:**
1. `upsert_model` rejects a same-`(provider_id, upstream_model_id)` model with a different `id`.
2. Same `id` (edit) is allowed even if `(provider_id, upstream_model_id)` is unchanged.
3. Same `upstream_model_id` under a different `provider_id` is allowed.
4. Trailing-whitespace variants collide after trim.

**`config/catalog.rs`:**
5. `vendor_model_desc.json` still parses after the `display_name` column removal; `context_size`
   lookups unchanged; existing catalog tests pass with updated `ModelDesc` fixtures.

**`config/types.rs`:**
6. A model JSON **without** `display_name` deserializes; a legacy JSON **with** it also deserializes
   (ignored) — regression guard for the migration.

### Frontend

7. `Models.vue save()` duplicate branch (warn + stay open) and the edit-exemption are covered by
   manual walkthrough against a fake config (no component-test harness today).

### Not separately tested

- The `display_name` → `upstream_model_id` text substitutions are mechanical and verified by
  compile (Rust) / type-check (TS) + the existing list/label rendering.

## Files Changed

### Backend
1. `src-tauri/src/config/types.rs` — remove `Model.display_name`; update model test literal + JSON
   deser test.
2. `src-tauri/src/config/catalog.rs` — remove `ModelDesc.display_name`; update 4 test fixtures.
3. `src-tauri/src/commands.rs` — uniqueness + trim in `upsert_model`; update model test literal.
4. `src-tauri/src/tray.rs` — `model.display_name` → `model.upstream_model_id`.
5. `src-tauri/src/proxy/dispatch.rs` — echo backfill `display_name` → `upstream_model_id` (+comment);
   update model test literal.
6. `src-tauri/src/proxy/resolve.rs`, `openai_edge.rs`, `anthropic_edge.rs` — update model test literals.
7. `tests/e2e_zhipu.rs` — update model literal.
8. `src-tauri/assets/vendor_model_desc.json` — drop `display_name` from all entries.

### Frontend
9. `src/lib/types.ts` — remove `display_name` from `Model`.
10. `src/views/Models.vue` — relabel + `save()` pre-check + drop `display_name` from form/build/list/delete.
11. `src/views/Profiles.vue`, `Dashboard.vue`, `Fallback.vue` — `m.display_name` → `m.upstream_model_id`.

## Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Uniqueness key | `(provider_id, upstream_model_id)` | User intent: "same upstream model under same account only once"; upstream id IS the model identity |
| Enforcement | backend `upsert_model` (authoritative) + UI pre-check | Matches the multi-account design's principle; UI cannot bypass |
| `upstream_model_id` normalization | trim before compare + persist; case preserved | Stops whitespace-only dupes; upstream ids are case-sensitive |
| Edit exemption | exclude self by `id` | Re-saving a model must not trip its own uniqueness |
| `display_name` removal | drop from `Model` AND dead catalog `ModelDesc` | Redundant on Model (always = upstream); dead on catalog; user requested both gone |
| Identity display | show `upstream_model_id` everywhere `display_name` was | Single source of truth; the two were already identical |
| Migration | none (serde ignores legacy field) | Field was always = upstream_model_id; no data loss; no keyring/FK risk |
| "账号" relabel scope | add-model form only (label/placeholder/warning/hint) | Matches multi-account terminology where Provider = account |

## Success Criteria

1. ✅ Adding a model whose `(provider_id, upstream_model_id)` already exists is rejected (backend +
   UI); editing a model re-saves fine; same upstream id under another account is allowed.
2. ✅ No `display_name` field remains on `Model` or `ModelDesc`; the catalog JSON column is gone; all
   displays show `upstream_model_id`.
3. ✅ The add-model form says "账号" instead of "服务商配置".
4. ✅ Existing configs with legacy `display_name` load without error.
5. ✅ Unit tests pass for uniqueness (incl. trim + edit-exempt), catalog parse, and model deser.

## Out of Scope

- Changing `Provider.display_name` (the account name) — it stays.
- Renaming the catalog JSON field `ModelDesc.provider_id` (holds the vendor slug; left as-is, per the
  multi-account design).
- Context-size values in `vendor_model_desc.json` (an unrelated in-flight edit; preserved).
- Adding a user-editable model alias (explicitly removed from scope — the user chose to drop
  `display_name`, not make it editable).
