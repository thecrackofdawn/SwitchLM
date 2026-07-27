# Per-Account Model Uniqueness + `display_name` Removal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a model unique per `(provider_id, upstream_model_id)`, delete the redundant `Model.display_name` (and the dead catalog `ModelDesc.display_name`), and relabel the add-model form's "服务商配置" → "账号".

**Architecture:** Backend-first. (1) Drop `Model.display_name` and migrate every read site to `upstream_model_id`. (2) Drop the unused catalog `ModelDesc.display_name` + JSON column. (3) Add a pure `conflicting_model` helper (mirroring `conflicting_same_name`) and enforce uniqueness in `upsert_model`. (4) Frontend: drop `display_name` from the TS `Model` + all .vue reads. (5) Relabel + add a `save()` pre-check. The uniqueness key IS the model identity (`upstream_model_id`), which is why removing `display_name` and adding uniqueness are one coherent change.

**Tech Stack:** Rust + Tauri + serde + tokio + tracing (backend, `src-tauri/`); Vue 3 + TypeScript + naive-ui (frontend, root). Backend tested with `cargo test` (Rust unit tests, TDD). Frontend has **no component-test harness** — verified by TypeScript typecheck (`npm run build`) + manual walkthrough.

## Global Constraints

- **Never touch `Provider.display_name`** — it is the account name (per the multi-account design). Only `Model.display_name` and `ModelDesc.display_name` are removed.
- `Model` must NOT have `#[serde(deny_unknown_fields)]` (it does not today) — legacy `app_config.json` models still carrying `display_name` must load via serde's default ignore-unknown-field behavior. No explicit migration.
- Known **pre-existing failure**: `config::catalog::tests::lookup_finds_bundled_models` already fails before this work (an unrelated in-flight edit to `vendor_model_desc.json` changed some `context_size` values, e.g. expects `1024000` but JSON has `1000000`). It is **not** introduced or fixed by this plan. Verify each task by confirming **no NEW** failures appear — run the specific module tests called out, not the whole suite blindly.
- Cargo commands run from `src-tauri/`; npm commands run from the project root.
- Keep commits frequent (one per task) and confined to that task's files.

---

### Task 1: Backend — remove `Model.display_name`

**Files:**
- Modify: `src-tauri/src/config/types.rs` (struct field `:122`; roundtrip literal `:194`; deser test `:243-250`; new test)
- Modify: `src-tauri/src/tray.rs:80`
- Modify: `src-tauri/src/proxy/dispatch.rs:65-66`
- Modify test literals/helpers: `src-tauri/src/commands.rs:1289-1295` (`m` helper), `src-tauri/src/proxy/dispatch.rs:819-830` (`mk_model` helper), `src-tauri/src/proxy/resolve.rs:41`, `src-tauri/src/proxy/openai_edge.rs:42`, `src-tauri/src/proxy/anthropic_edge.rs:40`, `tests/e2e_zhipu.rs:58`

**Interfaces:**
- Produces: `Model` struct **without** `display_name`; `upstream_model_id` is the sole name field. All later tasks rely on this shape.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/config/types.rs` `mod tests`, add (after `model_context_size_defaults_none_when_absent`):

```rust
#[test]
fn model_loads_without_display_name() {
    // display_name is removed; a model JSON that omits it (the new shape) must deserialize.
    let json = r#"{"id":"m","provider_id":"zhipu","upstream_model_id":"glm-4.6"}"#;
    let model: Model = serde_json::from_str(json).unwrap();
    assert_eq!(model.upstream_model_id, "glm-4.6");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run (from `src-tauri/`): `cargo test --lib config::types::tests::model_loads_without_display_name`
Expected: FAIL — panic at `.unwrap()` with `missing field display_name` (the field is currently required).

- [ ] **Step 3: Remove the field and migrate all read sites / literals**

1. `src-tauri/src/config/types.rs` — delete the line `pub display_name: String,` from the `Model` struct (the field between `provider_id` and `source`).
2. `src-tauri/src/tray.rs:80` — `let mut s = model.display_name.clone();` → `let mut s = model.upstream_model_id.clone();`
3. `src-tauri/src/proxy/dispatch.rs:65-66` — replace:
   ```rust
           // Spec §3.4: echo the requested name; backfill display_name when inbound has none.
           requested_model.map(str::to_string).unwrap_or_else(|| model.display_name.clone()),
   ```
   with:
   ```rust
           // Spec §3.4: echo the requested name; backfill upstream_model_id when inbound has none.
           requested_model.map(str::to_string).unwrap_or_else(|| model.upstream_model_id.clone()),
   ```
4. Remove the `display_name: …` line from each `Model { … }` literal / helper:
   - `src-tauri/src/config/types.rs:194` (`display_name: "GLM-4.6".into(),` in `app_config_roundtrips`)
   - `src-tauri/src/commands.rs:1291` (`display_name: id.into(),` in the `m()` helper)
   - `src-tauri/src/proxy/dispatch.rs:823` (`display_name: id.into(),` in `mk_model`)
   - `src-tauri/src/proxy/resolve.rs:41` (`display_name: "GLM-4.6".into(),`)
   - `src-tauri/src/proxy/openai_edge.rs:42` (`display_name: "GLM-4.6".into(),`)
   - `src-tauri/src/proxy/anthropic_edge.rs:40` (`display_name: "GLM-4.6".into(),`)
   - `tests/e2e_zhipu.rs:58` (`display_name: "GLM".into(),`)
5. `src-tauri/src/config/types.rs` test `model_context_size_defaults_none_when_absent` (`:243-250`) — **leave its JSON unchanged**. Its JSON still contains `"display_name":"GLM"`; after the field is removed this becomes the legacy-compat guard (serde ignores the unknown key). Update only its leading comment to:
   ```rust
     // Old config files (pre-context_size) omit the field; serde default must fill None
     // so existing installs keep working. Also exercises legacy display_name (now ignored).
   ```

- [ ] **Step 4: Build + run the relevant tests**

Run (from `src-tauri/`):
- `cargo build --lib` — Expected: compiles (proves every `display_name` read/literal was migrated).
- `cargo test --lib config::types` — Expected: PASS (incl. the new `model_loads_without_display_name`).
- `cargo test --lib proxy::` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/tray.rs src-tauri/src/proxy/dispatch.rs \
        src-tauri/src/commands.rs src-tauri/src/proxy/resolve.rs src-tauri/src/proxy/openai_edge.rs \
        src-tauri/src/proxy/anthropic_edge.rs src-tauri/tests/e2e_zhipu.rs
git commit -m "refactor(config): 删除 Model.display_name，统一用 upstream_model_id"
```

---

### Task 2: Backend — remove catalog `ModelDesc.display_name`

**Files:**
- Modify: `src-tauri/src/config/catalog.rs:17-24` (struct), `:142,143,146,147` (test fixtures)
- Modify: `src-tauri/assets/vendor_model_desc.json` (drop the `display_name` column from every entry)

**Interfaces:**
- Produces: `ModelDesc` without `display_name`; catalog JSON trimmed. `context_size` lookups unchanged.

- [ ] **Step 1: Remove the field from the struct**

In `src-tauri/src/config/catalog.rs`, delete from `ModelDesc`:
```rust
    #[serde(default)]
    pub display_name: Option<String>,
```

- [ ] **Step 2: Update the four `ModelDesc { … }` test fixtures**

In `src-tauri/src/config/catalog.rs` `merge_adds_missing_keeps_existing` (`:140-148`), remove `, display_name: None` from each of the four literals (lines ~142, 143, 146, 147), e.g.:
```rust
ModelDesc { provider_id: "zhipu".into(), upstream_model_id: "glm-4.6".into(), context_size: 999 },
```

- [ ] **Step 3: Strip the `display_name` column from the bundled JSON**

`display_name` is the last key on every entry line, formatted as `, "display_name": "<Value>"`. From `src-tauri/`, run (Git Bash):
```bash
sed -i -E 's/, "display_name": "[^"]*"//g' assets/vendor_model_desc.json
```
Then spot-check: every entry line should now end `... "context_size": <N> },` with no `display_name`.

- [ ] **Step 4: Build + run the catalog parse/merge tests**

Run (from `src-tauri/`):
- `cargo build --lib` — Expected: compiles.
- `cargo test --lib config::catalog::embedded_catalog_parses_and_has_sizes` — Expected: PASS (the trimmed JSON still parses).
- `cargo test --lib config::catalog::merge_adds_missing_keeps_existing` — Expected: PASS.

> Note: `config::catalog::tests::lookup_finds_bundled_models` remains a pre-existing failure (unrelated `context_size` mismatch) — do not try to fix it here.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/catalog.rs src-tauri/assets/vendor_model_desc.json
git commit -m "refactor(catalog): 删除未使用的 ModelDesc.display_name 及 JSON 列"
```

---

### Task 3: Backend — `upsert_model` per-account uniqueness (TDD)

**Files:**
- Modify: `src-tauri/src/commands.rs` — `conflicting_model` helper (add near `conflicting_same_name`, `:744`); `upsert_model` (`:107-122`); new test near `:1106`.

**Interfaces:**
- Consumes: `Model` shape from Task 1 (no `display_name`); the `m(id, pid, up, ctx)` test helper (`:1289`).
- Produces: `pub fn conflicting_model<'c>(cfg: &'c AppConfig, model: &Model) -> Option<&'c Model>`; `upsert_model` rejects duplicates with `该账号下已存在模型「<upstream>」`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/commands.rs` `mod tests`, add (reusing the existing `m(id, pid, up, ctx)` helper):

```rust
#[test]
fn conflicting_model_detects_dup_self_exclude_other_provider_and_trim() {
    let mut cfg = AppConfig::default();
    cfg.models.push(m("m1", "zhipu", "glm-4.6", None));

    // same provider + same upstream, different id -> conflict
    assert!(conflicting_model(&cfg, &m("m2", "zhipu", "glm-4.6", None)).is_some());
    // same id (edit) -> self excluded -> no conflict
    assert!(conflicting_model(&cfg, &m("m1", "zhipu", "glm-4.6", None)).is_none());
    // same upstream, different provider -> no conflict
    assert!(conflicting_model(&cfg, &m("m2", "deepseek", "glm-4.6", None)).is_none());
    // trailing whitespace is trimmed -> collides
    assert!(conflicting_model(&cfg, &m("m2", "zhipu", "glm-4.6 ", None)).is_some());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run (from `src-tauri/`): `cargo test --lib commands::tests::conflicting_model_detects_dup_self_exclude_other_provider_and_trim`
Expected: FAIL — `cannot find function conflicting_model`.

- [ ] **Step 3: Implement the helper + wire it into `upsert_model`**

Add next to `conflicting_same_name` (`src-tauri/src/commands.rs`, ~`:751`):

```rust
/// Another model (different id) that already has the same `(provider_id, upstream_model_id)`,
/// comparing trimmed upstream ids. `None` when the pair is free (or the only match is `model` itself).
pub fn conflicting_model<'c>(cfg: &'c AppConfig, model: &Model) -> Option<&'c Model> {
    let up = model.upstream_model_id.trim();
    cfg.models.iter().find(|m| {
        m.id != model.id && m.provider_id == model.provider_id && m.upstream_model_id.trim() == up
    })
}
```

Replace `upsert_model` (`:107-122`) with:

```rust
/// Insert-or-replace a model by id. Rejects a duplicate `(provider_id, upstream_model_id)`
/// (trimmed); editing the same id is exempt. Editing a model resets its breaker (§4.1).
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
        if conflicting_model(&cfg, &model).is_some() {
            return Err(format!("该账号下已存在模型「{}」", model.upstream_model_id));
        }
        upsert(&mut cfg.models, model);
        persist(&app, &cfg)?;
    }
    state.health.reset(&id);
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run (from `src-tauri/`):
- `cargo test --lib commands::tests::conflicting_model_detects_dup_self_exclude_other_provider_and_trim` — Expected: PASS.
- `cargo test --lib commands::tests::upsert_provider_inserts_then_replaces` — Expected: PASS (regression: provider path untouched).
- `cargo build --lib` — Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(models): upsert_model 按账号+上游模型去重（conflicting_model）"
```

---

### Task 4: Frontend — remove `Model.display_name`

**Files:**
- Modify: `src/lib/types.ts:22` (the `Model` mirror; line `:11` is `Provider`'s — leave it)
- Modify: `src/views/Models.vue` (`:42, :54, :77, :92, :118, :205-206`)
- Modify: `src/views/Profiles.vue:32,109`
- Modify: `src/views/Dashboard.vue:101,112,124`
- Modify: `src/views/Fallback.vue:25`

**Interfaces:**
- Consumes: backend `Model` no longer has `display_name` (Task 1).
- Produces: TS `Model` without `display_name`; every model read uses `upstream_model_id`.

> The TypeScript compiler is the test here — removing the field makes any remaining `m.display_name` a type error, which catches every site.

- [ ] **Step 1: Remove the field from the TS type**

In `src/lib/types.ts`, delete `display_name: string;` from the `Model` interface (~line 22). **Do not** touch the `Provider` interface's `display_name` (~line 11).

- [ ] **Step 2: Migrate every model `display_name` read**

- `src/views/Profiles.vue:32` and `:109` — `m.display_name` → `m.upstream_model_id`
- `src/views/Dashboard.vue:101` (`target.display_name`), `:112` (`m.display_name`), `:124` (`?.display_name`) → `…upstream_model_id`
- `src/views/Fallback.vue:25` — `m.display_name` → `m.upstream_model_id`

- [ ] **Step 3: Update `Models.vue` (form + list + delete)**

- `FormState` (`:42`): delete `display_name: string;`
- `blank()` (`:54`): delete `display_name: "",`
- `openEdit` (`:77`): delete the line `form.display_name = m.display_name;`
- `buildModel()` (`:92`): delete `display_name: form.display_name.trim() || upstream,` (the returned object no longer has the field)
- delete-confirm (`:118`): `` `确认删除「${m.display_name}」？` `` → `` `确认删除「${m.upstream_model_id}」？` ``
- list row (`:205-206`) — collapse the two identical spans into one. Replace:
  ```html
                <span class="name">{{ m.display_name }}</span>
                <span class="mono upstream">{{ m.upstream_model_id }}</span>
  ```
  with:
  ```html
                <span class="name">{{ m.upstream_model_id }}</span>
  ```

- [ ] **Step 4: Typecheck / build**

Run (from project root): `npm run build`
Expected: succeeds with **no** TS errors. If it errors on `display_name`, a read site was missed — fix it.

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/views/Models.vue src/views/Profiles.vue src/views/Dashboard.vue src/views/Fallback.vue
git commit -m "refactor(ui): 删除 Model.display_name，列表/选择器统一用 upstream_model_id"
```

---

### Task 5: Frontend — "账号" relabel + `save()` uniqueness pre-check

**Files:**
- Modify: `src/views/Models.vue` (label `:229`, placeholder `:230`, warning `:103`, hint `:188`, `save()` `:100-113`)

**Interfaces:**
- Produces: add-model form labeled "账号"; `save()` warns on a duplicate `(provider_id, upstream_model_id)` before calling the backend (which remains authoritative via Task 3).

- [ ] **Step 1: Relabel the form text**

In `src/views/Models.vue`:
- `:229` — `<NFormItem label="服务商配置">` → `<NFormItem label="账号">`
- `:230` — `placeholder="选择服务商配置"` → `placeholder="选择账号"`
- `:103` (inside `save()`) — `msg.warning("服务商 / 模型名称 不能为空");` → `msg.warning("账号 / 模型名称 不能为空");`
- `:188` — `模型配置：新增模型时自动拉取可选模型；协议与端点继承服务商配置` → `模型配置：新增模型时自动拉取可选模型；协议与端点继承账号配置`

- [ ] **Step 2: Add the duplicate pre-check in `save()`**

In `src/views/Models.vue` `save()`, insert this block **immediately after** the existing non-empty check (`if (!form.provider_id || !form.upstream_model_id.trim()) { … }`) and **before** `try { await config.saveModel(...) }`:

```ts
  const upTrim = form.upstream_model_id.trim();
  const dup = config.models.some(
    (m) => m.id !== form.id && m.provider_id === form.provider_id && m.upstream_model_id.trim() === upTrim,
  );
  if (dup) {
    msg.warning(`该账号下已存在模型「${upTrim}」`);
    return;
  }
```

- [ ] **Step 3: Typecheck / build**

Run (from project root): `npm run build`
Expected: succeeds.

- [ ] **Step 4: Manual verification (no component-test harness)**

Run the app, open 模型配置 → 新增模型:
1. Add `glm-4.6` under account A → succeeds.
2. Try adding `glm-4.6` under account A again → warning `该账号下已存在模型「glm-4.6」`, modal stays open.
3. Add `glm-4.6` under account B → succeeds.
4. Edit the existing account-A `glm-4.6` (change only context size) → saves without false duplicate warning.
5. Add `glm-4.6 ` (trailing space) under account A → warning (trim collides).
6. The form's provider selector is labeled "账号"; the top hint says "继承账号配置".

- [ ] **Step 5: Commit**

```bash
git add src/views/Models.vue
git commit -m "feat(ui): 新增模型表单改文案为「账号」+ 同账号同模型查重预检"
```

---

## Self-Review

**Spec coverage:** spec §1 (data model) → Task 1; §2 (catalog) → Task 2; §3 (uniqueness) → Task 3; §4 (frontend pre-check + relabel) → Task 5; §5 (read-site migration) → Tasks 1+4; §6 (migration) → Global Constraints + Task 1 legacy-compat test. All requirements mapped.

**Placeholder scan:** none — every step has real code or an exact old→new edit. The one shell snippet (sed) is concrete and verified against the JSON's actual line format.

**Type consistency:** `conflicting_model` (Task 3) matches `conflicting_same_name`'s signature shape (`<'c>(cfg: &'c AppConfig, x: &X) -> Option<&'c X>`); the `m(id, pid, up, ctx)` helper is the existing one (post-Task-1, no `display_name`). Frontend uses `m.upstream_model_id` consistently across Tasks 4–5.

**Pre-existing failure note:** carried in Global Constraints so no task is blocked by the unrelated `lookup_finds_bundled_models` failure.
