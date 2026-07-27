# Provider Multi-Account Support Design

**Date:** 2026-07-30
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

## Overview

Allow a user to configure **more than one account per vendor** (e.g. two Volcengine Coding
accounts, or two Zhipu accounts), each fully functional — its own base URLs, inference key, and
usage credentials, independently selectable as a model's provider.

Today this is impossible because the `Provider.id` field serves two conflicting roles at once: it
is both the **opaque storage primary key** (FK target for models, keyring key, cache key) **and**
the **vendor-kind slug** that every piece of routing matches on (`zhipu`, `volcengine-agent`,
`volcengine-coding`). Adding a second account of the same vendor reuses the same slug, so
`upsert` **silently overwrites** the first — config, inference key, and usage SK all gone.

The fix separates the two roles: a new **`vendor`** field carries the routing kind, `id` becomes a
pure opaque per-instance key, and a **`(vendor, display_name)` uniqueness constraint** plus a
**same-account conflict check** give the user safe, distinguishable multi-account config.

## Background: the coupling, and why "just change the id" doesn't work

`upsert` is insert-or-replace by `id` (`commands.rs:751`), and the add form sets `id` straight
from the vendor slug (`Provider.vue` `onVendorChange` → `form.id = slug`; `save` →
`provider.id = form.id`). There is no `genId` for providers (unlike models/profiles).

Seven backend sites (plus the frontend) match the **slug semantics** of the provider, not its
identity — so they must read the vendor kind, not the id:

| Site | Today matches on |
|---|---|
| `usage::usage_provider_for(id)` `usage/mod.rs:153` | id slug |
| `commands::volcengine_plan_action(id)` `commands.rs:188` | id slug |
| `catalog::context_size(id, up)` `catalog.rs:28` | id (= vendor slug in catalog data) |
| `recognized_context_size` `commands.rs:425` | provider_id |
| `effective_size` / `classify_fallback` `commands.rs:683`/`690` | `m.provider_id` → catalog |
| `error_adapter::is_rate_limit_error` `error_adapter.rs:8` (`rate_limit_in_json` `:26`) | id slug |
| `dispatch` rate-limit + realtime-usage call sites `dispatch.rs:251`/`366`/`442`/`662` | `snap.provider_id` |
| Frontend `Provider.vue` `needsUsageCreds` | `form.id.startsWith("volcengine")` |

Because the routing matches the slug, a user cannot even work around the bug by typing a custom id
(the `NSelect` is `tag`-able): a custom id like `volcengine-coding-2` is unrecognized by
`usage_provider_for` / `volcengine_plan_action`, so usage queries and discovery silently break.

## Requirements

### Functional

1. **Multiple accounts per vendor.** Two providers with the same `vendor` but different credentials
   coexist; both can be a model's provider, query usage, discover models, and classify rate-limit
   errors correctly.
2. **Accounts are distinguishable in the UI.** Each provider has a user-editable account name
   (`display_name`) shown as the list row's title; the vendor is shown as a small tag. Within a
   vendor, `(vendor, display_name)` is unique.
3. **Safe rename.** Renaming an account changes only its `display_name`; it never orphans models,
   inference keys, or usage SKs (the storage key is a stable random `id`, not the name).
4. **Same-account conflict → warn, then overwrite.** When the add form's credentials already
   belong to a configured provider (Volcengine: same AccessKey ID; others: same inference
   `api_key`), the UI warns that continuing will overwrite that provider's config; confirming
   replaces it in place. Different credentials at the same vendor = a new, separate account.
5. **Non-destructive migration.** Existing configs (where `id == slug`) keep working unchanged:
   they gain a `vendor` field derived from the legacy id. The keyring and model FKs are **not**
   touched.

### Non-Functional

1. **Zero keyring risk on migration.** Legacy provider ids are preserved as-is; no re-keying.
2. **`id` becomes opaque.** No routing, lookup, or display logic reads `id` for anything but an
   equality key (PK / FK / keyring / cache).
3. **Authoritative validation in the backend.** The `(vendor, display_name)` uniqueness rule is
   enforced in `upsert_provider` so it cannot be bypassed by a frontend bug; the UI adds a
   pre-check and sensible defaults for nicer UX.

## Implementation Architecture

### 1. Data model — split `vendor` from `id`

**File: `src-tauri/src/config/types.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    /// Opaque per-instance primary key. Stable for the life of the account (never renamed).
    /// New providers get a random id (`genId("prov", …)` on the frontend, matching models/profiles);
    /// legacy providers keep their slug here. Keyring keys, model FKs, and the usage cache hang off
    /// this. NOT used for routing — use `vendor`.
    pub id: String,
    /// Vendor kind slug — the routing key. Drives usage-adapter selection, Volcengine plan-model
    /// discovery, catalog context-size lookup, and rate-limit classification; also supplies the
    /// base_url defaults and the list-row tag. Known slugs: `zhipu`, `volcengine-agent`,
    /// `volcengine-coding`; anything else is a custom OpenAI-compatible provider.
    #[serde(default)]
    pub vendor: String,
    /// User-editable account name (the list-row title). Unique within `(vendor, display_name)`.
    pub display_name: String,
    #[serde(default)]
    pub openai_base_url: Option<String>,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    #[serde(default)]
    pub usage_creds: Option<UsageCreds>,
}
```

`#[serde(default)]` on `vendor` lets legacy `app_config.json` (which omits the field) deserialize
with `vendor == ""`; the startup migration (§2) fills it. The TS mirror `src/lib/types.ts` gains
`vendor?: string` on `Provider`.

> **Mechanical churn:** every `Provider { … }` literal must now supply `vendor`. Most test sites
> go through helpers (`commands.rs::mk_provider`, dispatch's model builder) which are updated once;
> ~6 direct literals remain (`dispatch.rs:758/765/773/924/925`, `anthropic_edge.rs:29`,
> `openai_edge.rs:31`, `types.rs:156`, `state.rs:176`). The catalog JSON field
> (`ModelDesc.provider_id`, = the vendor slug) is **not** renamed.

### 2. Migration — derive `vendor` from the legacy id, in place

**File: `src-tauri/src/config/store.rs`** — a pure normalizer, plus a persist-if-changed wrapper:

```rust
/// Fill `vendor` for any legacy provider that lacks it. Legacy configs used the vendor slug as the
/// provider `id`, so `vendor = id` is exactly correct and touches nothing else (no keyring, no FK).
/// Idempotent: once `vendor` is non-empty it is left alone. Returns whether any provider changed.
pub fn normalize_legacy_vendors(cfg: &mut AppConfig) -> bool {
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

Called at **both** config load sites — `lib.rs:47` (app build) and `state.rs:41`
(`AppStateInner::new`) — immediately after `store::load`, persisting via `store::save` when it
returns `true`. This mirrors the existing `migrate_usage_sk_to_keyring` pattern (runs at startup,
idempotent, rewrites the file only when needed). Legacy provider ids stay slugs; new providers get
random ids — both coexist fine because `id` is opaque.

### 3. Routing decoupling — read `vendor`, not `id`

Each slug-semantic site resolves the provider's `vendor` (it already has the provider, or looks it
up by id) and passes that instead of the id:

| Site | Change |
|---|---|
| `usage::usage_provider_for` | param `provider_id` → `vendor`; match on vendor |
| `commands::volcengine_plan_action` | param `provider_id` → `vendor` |
| `catalog::context_size` | first param renamed `provider_id` → `vendor` (struct field unchanged) |
| `recognized_context_size` | resolve provider by id → pass `provider.vendor` to catalog |
| `effective_size` / `classify_fallback` | take `vendor` (resolved from each model's provider) |
| `error_adapter::is_rate_limit_error` / `rate_limit_in_json` | match on `vendor` |
| `dispatch` (see §4) | use `snap.vendor` |

`query_usage` (`commands.rs:647`) and `discover_models`/`discover_volcengine_models`
(`commands.rs:174`/`199`) already look the provider up by id; they read `provider.vendor` there and
forward it. Everything that uses `provider_id` as an **opaque key** (keyring `set_key`/`get_key`,
`usage_cache`, `Model.provider_id` FK, `delete_by_id`, `find(|p| p.id == …)`) is unchanged.

### 4. `ModelSnapshot` carries `vendor` (dispatch threading)

**File: `src-tauri/src/proxy/dispatch.rs`**

```rust
struct ModelSnapshot {
    upstream_model_id: String,
    openai_base_url: Option<String>,
    anthropic_base_url: Option<String>,
    provider_id: String, // opaque key — keyring / cache / FK
    vendor: String,      // NEW — routing (usage adapter + rate-limit classification)
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

`usage_snapshot_realtime` (`:662`) calls `usage_provider_for(&snap.vendor)`; the rate-limit
classification call sites (`:251`/`:366`/`:442`/`:456`/`:492`/`:538`/`:583`) pass `&snap.vendor`.
The opaque-key uses of `snap.provider_id` (keyring, cache, `usage_creds`) are untouched.

> **Lock granularity (unchanged).** `snapshot_model` already holds the read lock only for the
> snapshot build (no `.await` / HTTP inside it) and releases it on return. This change adds one
> `String::clone` (`vendor`); it adds **no new lookup and no async work**, so the lock scope — and
> the pre-existing two linear `find`s — are unchanged. (The double `find` is existing behavior at a
> scale of tens of providers/models, not introduced here.)

### 5. Uniqueness constraint + same-account conflict detection (backend)

**File: `src-tauri/src/commands.rs`**

`upsert_provider` enforces `(vendor, display_name)` uniqueness (excluding self by id), so the rule
cannot be bypassed:

```rust
#[tauri::command]
pub async fn upsert_provider(
    state: State<'_, AppState>,
    provider: Provider,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        // Normalize the account name: trim before BOTH the uniqueness comparison and persistence,
        // so "工作号 " and "工作号" cannot sneak past the check. (Case is intentionally preserved —
        // users may rely on it to differentiate accounts.)
        provider.display_name = provider.display_name.trim().to_string();
        let mut cfg = state.config.write().await;
        let dup = cfg.providers.iter().any(|p| {
            p.id != provider.id && p.vendor == provider.vendor && p.display_name == provider.display_name
        });
        if dup {
            return Err(format!("该厂商下已有同名账号「{}」，请改名", provider.display_name));
        }
        upsert(&mut cfg.providers, provider);
        persist(&app, &cfg)?;
    }
    Ok(())
}
```

A new command detects a **same-account** conflict (credentials are secret, so the frontend cannot
compare them itself):

```rust
#[derive(Serialize)]
pub struct ProviderConflict {
    pub id: String,
    pub display_name: String,
}

/// Whether the credentials being saved already belong to a configured provider (= same account).
/// Volcengine matches on AccessKey ID; others match on the inference api_key read from the keyring.
/// Credentials absent from the form cannot match → `None` (a fresh account the user will key later).
#[tauri::command]
pub async fn check_provider_conflict(
    state: State<'_, AppState>,
    vendor: String,
    access_key_id: Option<String>,
    api_key: Option<String>,
) -> Result<Option<ProviderConflict>, String> {
    let ak = access_key_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let key = api_key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let providers = state.config.read().await.providers.clone();
    for p in providers.iter().filter(|p| p.vendor == vendor) {
        let same = if vendor.starts_with("volcengine") {
            ak.is_some() && p.usage_creds.as_ref()
                .and_then(|c| c.access_key_id.as_deref()).map(str::trim) == ak
        } else {
            // api_key lives in the keyring under the provider id (opaque key).
            key.is_some() && state.secrets.get_key(&p.id).ok().flatten().as_deref().map(str::trim) == key
        };
        if same {
            return Ok(Some(ProviderConflict { id: p.id.clone(), display_name: p.display_name.clone() }));
        }
    }
    Ok(None)
}
```

Both commands are registered in the Tauri `invoke_handler` (`lib.rs`) and mirrored in
`src/lib/commands.ts`.

### 6. Provider.vue UX

**File: `src/views/Provider.vue`** (+ `src/lib/types.ts`, `src/lib/commands.ts`)

- **Form gains two controls**, split from the old single id-select:
  - **Vendor select** (`form.vendor`, the existing `vendorOptions` + `tag` for custom) — drives
    `vendorDefaults` base_url prefill, same as today.
  - **Account name input** (`form.displayName`) — the list-row title.
- **List row**: title = `p.display_name`; a small vendor tag = `vendorOptions` label (or `p.vendor`
  for custom) is shown beside it (replacing the old auto-derived name).
- **`needsUsageCreds`** keys off `form.vendor.startsWith("volcengine")` (was `form.id`).
- **Default account name + auto-suffix** (so the uniqueness constraint is satisfied by default):

```ts
function defaultDisplayName(vendor: string): string {
  const label = vendorOptions.find((o) => o.value === vendor)?.label ?? vendor;
  const taken = new Set(
    config.providers.filter((p) => p.vendor === vendor).map((p) => p.display_name),
  );
  if (!taken.has(label)) return label;
  for (let n = 2; ; n++) { const cand = `${label} ${n}`; if (!taken.has(cand)) return cand; }
}
```
  `onVendorChange` prefills `form.displayName = defaultDisplayName(vendor)`.

- **id generation**: new providers get `genId("prov", config.providers.map((p) => p.id))`
  (`src/lib/id.ts`, the existing helper models/profiles use).

### 7. Save flow ordering (frontend `save()`)

```
1. Validate: vendor, display_name, and ≥1 base_url non-empty.
2. Build `provider`:
     id          = editing ? existingId : genId("prov", existingIds)
     vendor, display_name, base_urls, usage_creds  ← from form
3. Same-account check — **only when `!editing`** (a brand-new account). Editing an existing account
   keeps its fixed `id` and goes straight to upsert; it never runs the cross-account credential
   check, so a blank key on edit (user only changing the base_url) is never misread as a conflict:
     if (!editing) {
       conflict = await checkProviderConflict(vendor, ak?, apiKey?)
       if conflict:
          dialog.warning("与现有「{conflict.display_name}」疑似同一账号（同密钥），"
                         "继续将覆盖该服务商配置。")  [覆盖] / [取消]
          on 覆盖: provider.id = conflict.id   // → upsert replaces that provider in place
          on 取消: abort
     }
4. await upsertProvider(provider)
     on Err (uniqueness) → msg.error(Err) and stay open   // user renames and retries
5. setKey / setUsageSk only if the form field is non-empty
     (blank on the overwrite path ⇒ existing keys preserved, same as edit)
```

> **Keyring-write ordering guarantee.** The temporary `genId` from step 2 is only a placeholder —
> it is never used to write the keyring. Keyring writes (step 5) run **after** the overwrite
> decision finalizes `provider.id`, so they always key under the final id (the new random id, or
> `conflict.id` on overwrite). No orphaned temp-id key is ever created.

Step 4's uniqueness check is the backstop: auto-suffix normally prevents a clash, but if the user
manually typed a name that collides with a *third* provider, the backend rejects it with a clear
message.

### 8. Provider labels across the UI

With several providers able to share a vendor, every place that surfaces a provider must render a
**distinguishable label**, not the bare `display_name` — two *different* vendors could be
custom-named identically (e.g. both "工作号"), and `Models.vue:33` today labels the provider
selector with `p.display_name` alone. Introduce one shared helper and reuse it everywhere:

```ts
// src/lib/selectLabel.ts (alongside the existing ellipsisLabel)
function vendorLabel(vendor: string): string {
  return vendorOptions.find((o) => o.value === vendor)?.label ?? vendor; // Chinese label, else slug
}
// "工作号（火山 · coding plan）" — but suppress the parenthetical when the name already IS the
// vendor label (the default), so a freshly-added single account reads "火山 · coding plan", not
// "火山 · coding plan（火山 · coding plan）".
function providerLabel(p: Provider): string {
  const v = vendorLabel(p.vendor);
  return p.display_name === v ? p.display_name : `${p.display_name}（${v}）`;
}
```

| Site | Today | After |
|---|---|---|
| `Models.vue:33` provider selector | `p.display_name` | `providerLabel(p)` |
| `Profiles.vue:32`/`:109` model options | `…(${provider.display_name})` | `…(${providerLabel(provider)})` |
| `Dashboard.vue:83`/`:101` (fallback chain, modelOptions) | `provider.display_name` | `providerLabel(provider)` |
| `Fallback.vue:19` `providerName` | `p.display_name` | `providerLabel(p)` |
| `Usage.vue:39` card title | `p.display_name` | `providerLabel(p)` |
| `tray.rs:62` `model_label` (backend) | `… · {provider.display_name}` | append vendor (e.g. `[volcengine-coding]`) when name ≠ vendor |
| `Provider.vue` list row (§6) | account-name title + vendor tag | unchanged (already vendor-tagged) |

Same-vendor accounts are already distinguishable by their auto-suffixed `display_name`; the vendor
suffix mainly disambiguates the rare cross-vendor same-custom-name case and makes the vendor
explicit inside model/provider selectors.

### Data flow

```
Provider.vue  save()
   │
   ├─ checkProviderConflict(vendor, ak?, apiKey?) ──▶ commands::check_provider_conflict
   │        volcengine: match vendor + AK (config)        │ reads keyring api_key for non-volcengine
   │        others:     match vendor + api_key (keyring)  ◀── Option<ProviderConflict>
   │   conflict? ──▶ overwrite dialog ──▶ provider.id = conflict.id
   │
   ├─ provider.id = genId("prov", …)  (new)  /  conflict.id or existing (edit/overwrite)
   │
   └─ upsertProvider(provider) ──▶ commands::upsert_provider
            │ uniqueness: (vendor, display_name) not taken by another id? else Err
            │ upsert by id (insert or replace)
            │ persist
            ◀── Ok / Err
```

Every routing consumer resolves `vendor` from the provider (looked up by the opaque `id`) once, then
uses `vendor` for adapter/discovery/catalog/rate-limit selection and `id` only as a key.

## Error Handling

| Condition | Behavior |
|---|---|
| `(vendor, display_name)` already used by another provider | `upsert_provider` returns `该厂商下已有同名账号「…」，请改名`; UI shows it, form stays open |
| Same-account credentials detected on add | UI dialog warns continuing overwrites; **覆盖** replaces in place, **取消** aborts |
| Overwrite target's new name collides with a third provider | Uniqueness Err at step 4 (same message); user renames |
| Credentials blank on add | No conflict detectable → normal new add (random id); user keys it later |
| Legacy provider missing `vendor` | Startup migration sets `vendor = id`; transparent |
| Unknown vendor (custom) | Routes through the generic OpenAI path; uniqueness still applies per `(vendor, display_name)` |

## Testing Strategy

### Rust unit tests (deterministic; no live HTTP — matches existing style)

**`config/store.rs`:**
1. `normalize_legacy_vendors` fills empty `vendor` from `id`; leaves non-empty alone (idempotent on
   second call); reports `changed` correctly.

**`commands.rs`:**
2. `upsert_provider` uniqueness: a second provider with same `(vendor, display_name)` but different
   id is rejected; same id (edit) is allowed; same vendor + different name is allowed.
3. `check_provider_conflict` (factored as a pure helper over a providers slice + an injectable
   key-reader, so it is unit-testable without a real keyring): Volcengine AK match hits; non-Volcengine
   api_key match hits; different vendor never hits; blank credentials never hit.
4. `volcengine_plan_action` / `usage_provider_for` map on **vendor** (`volcengine-coding` → coding
   action; `zhipu` → zhipu adapter; unknown → `None`).

**`proxy/dispatch.rs`:**
5. `snapshot_model` populates `vendor` from the provider; rate-limit classification uses vendor (a
   provider whose `id ≠ vendor` still classifies correctly).

**`config/catalog.rs`:**
6. `context_size` resolves via vendor (existing tests already pass vendor slugs — unchanged intent).

**`commands.rs` (uniqueness):**
7. `upsert_provider` rejects a same-`(vendor, display_name)` provider (different id); allows the
   same id (edit); allows same vendor + different name; **trailing-whitespace variants collide**
   after trim (`"x "` vs `"x"` → rejected).

### Frontend

8. `defaultDisplayName` returns the vendor label when free, `… 2` / `… 3` when taken within the
   vendor (pure function).
9. `providerLabel` suppresses the vendor suffix when `display_name === vendorLabel` (default name),
   appends it otherwise (pure function).
10. The save-flow dialog branches (overwrite vs cancel vs uniqueness error; conflict-check skipped
    on edit) are covered by manual walkthrough against a fake config; no component-test harness
    exists today, so this is noted as manual.

### Not separately tested

- **Live multi-account correctness** (two real Volcengine accounts routing independently) cannot be
  unit-tested without creds; verified manually by the user — same caveat the usage/Volcengine-discovery
  specs document ("真实正确性靠用户实测").

## Files Changed

### Backend
1. `src-tauri/src/config/types.rs` — add `vendor` to `Provider`; update test literal.
2. `src-tauri/src/config/store.rs` — `normalize_legacy_vendors`; call at load sites + persist.
3. `src-tauri/src/lib.rs` + `src-tauri/src/proxy/state.rs` — invoke the normalizer after `store::load`.
4. `src-tauri/src/commands.rs` — uniqueness in `upsert_provider` (**trim `display_name`** before compare + persist); new `check_provider_conflict` +
   `ProviderConflict`; `volcengine_plan_action`/`discover_*`/`query_usage`/`recognized_context_size`/
   `effective_size`/`classify_fallback`/`validate_fallback_context` resolve + thread vendor; update
   `mk_provider` + register new command in `invoke_handler`.
5. `src-tauri/src/usage/mod.rs` — `usage_provider_for(vendor)`.
6. `src-tauri/src/config/catalog.rs` — `context_size` param `provider_id` → `vendor` (field name stays).
7. `src-tauri/src/proxy/error_adapter.rs` — match on `vendor`.
8. `src-tauri/src/proxy/dispatch.rs` — `ModelSnapshot.vendor` + populate; call sites use `snap.vendor`;
   update provider literals in tests.
9. `src-tauri/src/tray.rs` — `model_label` appends vendor for disambiguation; update `mk_provider`
   helper.

### Frontend
10. `src/lib/types.ts` — `vendor?: string` on `Provider`.
11. `src/lib/commands.ts` — `checkProviderConflict`; (provider payload now carries `vendor`).
12. `src/lib/selectLabel.ts` — `vendorLabel` + `providerLabel` helpers.
13. `src/views/Provider.vue` — vendor select + account-name input + auto-suffix + vendor tag + save
    flow (conflict dialog → overwrite) + uniqueness error handling; `needsUsageCreds` on `vendor`.
14. Apply `providerLabel` across `Models.vue`, `Profiles.vue`, `Dashboard.vue`, `Fallback.vue`,
    `Usage.vue` (per the §8 table).

## Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Routing key | new `vendor` field; `id` stays opaque | Seven sites match vendor kind, not identity; separating them is the only way multi-account works |
| Primary key | stable random `id` (new) / legacy slug (existing) | A PK must be stable & canonical; the user only ever sees `(vendor, display_name)`, so the pair is a **uniqueness constraint**, not the PK — rename stays safe |
| Account identity in UI | `(vendor, display_name)` unique | That pair is exactly how the user tells accounts apart; enforcing uniqueness delivers the intent without a mutable PK |
| Legacy migration | `vendor = id`, keep id; no keyring touch | Legacy id already equals the vendor slug; re-randomizing would force re-keying secrets for zero benefit (rejected as too risky) |
| Same-account conflict | same vendor + same credentials (AK / api_key) | Within a vendor the credential identifies the account; base_url is vendor-determined so it need not be compared |
| Conflict behavior | warn → overwrite in place (keep target id) | User's stated intent ("覆盖"); in-place keeps FK/keyring valid; blank creds preserved as in edit |
| Uniqueness enforcement | backend `upsert_provider` (authoritative) + UI pre-check | Single source of truth the UI cannot bypass; auto-suffix + pre-check for UX |
| Conflict detection | backend command (reads keyring) | api_key/AK are secret — the frontend cannot compare them |
| Vendor mutability | disabled on edit (like the old id-select) | Changing vendor changes all routing; treat as "delete + re-add" |
| `display_name` normalization | trim before compare + persist; case preserved | Stops whitespace-only duplicates that look identical in the UI; case kept so users can differentiate with it |
| Conflict-check scope | only `!editing` (new accounts) | Edit keeps a fixed id; a blank key on edit must not be misread as "no conflict" or a false conflict |
| Keyring-write timing | after `provider.id` is finalized | Guarantees keys are written under the final id; no orphaned temp-id key on overwrite |
| Provider labels in UI | `display_name` + vendor suffix when customized | Same-vendor accounts already differ by auto-suffix; the vendor suffix disambiguates cross-vendor same-name |

## Success Criteria

1. ✅ Two providers of the same vendor (different credentials) coexist; both can back a model,
   query usage, discover models, and classify rate-limit errors correctly.
2. ✅ Each account is distinguishable in the list (account-name title + vendor tag); renaming an
   account never orphans its models, inference key, or usage SK.
3. ✅ Adding a provider whose credentials match an existing one warns and, on confirm, overwrites
   that provider in place; adding different credentials at the same vendor creates a separate account.
4. ✅ A duplicate `(vendor, display_name)` is rejected with a clear message; auto-suffix prevents it
   by default.
5. ✅ Existing configs migrate transparently on first launch (gain `vendor`; keyring and FKs intact).
6. ✅ No routing site reads `id` for vendor semantics; `id` is used only as an opaque key.
7. ✅ Unit tests pass for migration, uniqueness, conflict detection, vendor-based routing, and
   `ModelSnapshot.vendor`.

## Out of Scope

- Re-randomizing legacy provider ids / re-keying the keyring (explicitly rejected — unnecessary risk).
- Renaming the catalog JSON field `ModelDesc.provider_id` (it holds the vendor slug; left as-is).
- Changing an existing provider's vendor via the UI (disabled on edit; delete + re-add instead).
- Bulk import / export of providers.
- A provider "type" enum (the slug string + known-set select + custom tag is sufficient and matches
  the existing pattern).
