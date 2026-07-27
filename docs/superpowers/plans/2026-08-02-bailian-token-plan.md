# Bailian (阿里云百炼) token plan Provider — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make 阿里云百炼 "token plan" a routable provider — an agent hits the local proxy and SwitchLM forwards to one of two Bailian native endpoints via raw protocol passthrough, with no usage query for now.

**Architecture:** Bailian is a simple Bearer-key, dual-protocol vendor (same shape as DeepSeek), so the proxy needs **zero backend code changes** — `dispatch.rs` already does Anthropic passthrough when `anthropic_base_url` is set and OpenAI passthrough when `openai_base_url` is set. The work is: seed the catalog (backend, TDD), register the vendor in the frontend dropdown + base_url defaults, and make the usage chip honest ("不支持" vs "查询失败") for a vendor that has no usage adapter yet.

**Tech Stack:** Rust (axum, serde, co-located `#[cfg(test)]` tests), Vue 3 `<script setup>` + Pinia + Naive UI, Tauri v2. Frontend has **no unit-test framework** — its gate is `vue-tsc --noEmit` + manual verification (per CLAUDE.md). Backend uses `cargo test`.

## Global Constraints

- Vendor slug (the routing key): **`bailian-token`**. Dropdown label: **`百炼 · token plan`**.
- `openai_base_url` default: **`https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`** (dispatch appends `/chat/completions`).
- `anthropic_base_url` default: **`https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic`** (dispatch appends `/v1/messages`).
- Catalog entries are keyed by `provider_id == vendor == "bailian-token"`. The 7 models and exact `context_size` values (tokens) are:
  - `qwen3.8-max-preview` → `1000000`
  - `qwen3.7-max` → `1000000`
  - `qwen3.7-plus` → `1000000`
  - `qwen3.6-flash` → `128000`
  - `deepseek-v4-pro` → `1048576`
  - `deepseek-v4-flash-0731` → `1048576`
  - `glm-5.2` → `1000000`
- **No backend dispatch / breaker / translate / usage-adapter changes.** Every vendor-routed path already degrades sensibly for an unknown vendor; this plan adds only catalog data + a guard test + a doc comment on the backend.
- Bearer-key only → `needsUsageCreds` stays false for `bailian-token` (no AK/SK form). Auth key lives in the OS keyring like every provider.
- UI is Chinese-language; chip copy is **"不支持"** (known vendor, no adapter) vs **"查询失败"** (adapter present, query errored).
- Conventional-commit messages match the repo (`feat(scope): …`). Per the project workflow, work is on `main` directly; commits are batched at the user's direction, nothing is pushed automatically.

**Spec:** `docs/superpowers/specs/2026-08-02-bailian-token-plan-design.md`

---

### Task 1: Backend — seed Bailian catalog data, guard test, and known-vendor doc comment

TDD: the failing assertion is written first, then the catalog data that makes it pass. This task is the entire backend change set (no Rust logic changes — dispatch/breaker/translate are untouched).

**Files:**
- Modify: `src-tauri/assets/vendor_model_desc.json` (append 7 entries)
- Modify: `src-tauri/src/config/catalog.rs:119-127` (extend `lookup_finds_bundled_models`)
- Modify: `src-tauri/src/config/types.rs:102-104` (`Provider.vendor` doc comment)

**Interfaces:**
- Produces: catalog entries queryable as `context_size("bailian-token", "<upstream_model_id>")` (consumed by the Models-page context hint and `validate_fallback_context` at runtime — no code call sites to add).

- [ ] **Step 1: Write the failing guard assertions**

In `src-tauri/src/config/catalog.rs`, inside `fn lookup_finds_bundled_models` (after the existing `volcengine-coding` assertion at line 126), append:

```rust
        assert_eq!(cat.context_size("bailian-token", "qwen3.8-max-preview"), Some(1000000));
        assert_eq!(cat.context_size("bailian-token", "qwen3.7-max"), Some(1000000));
        assert_eq!(cat.context_size("bailian-token", "qwen3.7-plus"), Some(1000000));
        assert_eq!(cat.context_size("bailian-token", "qwen3.6-flash"), Some(128000));
        assert_eq!(cat.context_size("bailian-token", "deepseek-v4-pro"), Some(1048576));
        assert_eq!(cat.context_size("bailian-token", "deepseek-v4-flash-0731"), Some(1048576));
        assert_eq!(cat.context_size("bailian-token", "glm-5.2"), Some(1000000));
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml lookup_finds_bundled_models`
Expected: FAIL — `context_size("bailian-token", …)` returns `None` because the entries are not yet in the catalog JSON.

- [ ] **Step 3: Add the catalog data**

In `src-tauri/assets/vendor_model_desc.json`, the file currently ends with the `volcengine-coding` `glm-5.2` entry then `]}`. Add a trailing comma to that entry and append the seven `bailian-token` entries, so the tail of the `"models"` array becomes:

```json
    { "provider_id": "volcengine-coding", "upstream_model_id": "glm-5.2", "context_size": 1000000 },

    { "provider_id": "bailian-token", "upstream_model_id": "qwen3.8-max-preview", "context_size": 1000000 },
    { "provider_id": "bailian-token", "upstream_model_id": "qwen3.7-max", "context_size": 1000000 },
    { "provider_id": "bailian-token", "upstream_model_id": "qwen3.7-plus", "context_size": 1000000 },
    { "provider_id": "bailian-token", "upstream_model_id": "qwen3.6-flash", "context_size": 128000 },
    { "provider_id": "bailian-token", "upstream_model_id": "deepseek-v4-pro", "context_size": 1048576 },
    { "provider_id": "bailian-token", "upstream_model_id": "deepseek-v4-flash-0731", "context_size": 1048576 },
    { "provider_id": "bailian-token", "upstream_model_id": "glm-5.2", "context_size": 1000000 }
  ]
}
```

(No `display_name` field — match the existing entries' shape exactly. The blank line separates vendors, consistent with the file's existing grouping.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml lookup_finds_bundled_models`
Expected: PASS.

- [ ] **Step 5: Run the whole catalog module to confirm no regressions**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config::catalog`
Expected: PASS — including `embedded_catalog_parses_and_has_sizes` (all new `context_size` values are `> 0`) and the merge/ensure tests.

- [ ] **Step 6: Update the known-vendor doc comment**

In `src-tauri/src/config/types.rs`, replace the `Provider.vendor` doc comment (lines 102-104):

```rust
    /// Vendor kind slug — the routing key (usage adapter, Volcengine plan discovery, catalog
    /// lookup, rate-limit classification) and source of base_url defaults + the list-row tag.
    /// Known: `zhipu`, `deepseek`, `volcengine-agent`, `volcengine-coding`; else custom. `id` is
    /// the opaque PK. `deepseek` (like `zhipu`) is Bearer-key only - no AKSK usage_creds.
```

with:

```rust
    /// Vendor kind slug — the routing key (usage adapter, Volcengine plan discovery, catalog
    /// lookup, rate-limit classification) and source of base_url defaults + the list-row tag.
    /// Known: `zhipu`, `deepseek`, `volcengine-agent`, `volcengine-coding`, `bailian-token`;
    /// else custom. `id` is the opaque PK. `deepseek`/`zhipu`/`bailian-token` are Bearer-key only
    /// (no AKSK usage_creds); `bailian-token` has no usage adapter yet (cookie-based, deferred).
```

- [ ] **Step 7: Commit**

```bash
git add src-tauri/assets/vendor_model_desc.json src-tauri/src/config/catalog.rs src-tauri/src/config/types.rs
git commit -m "feat(catalog): seed bailian-token models + lookup guard test"
```

---

### Task 2: Frontend — register the bailian-token vendor (dropdown + base_url defaults)

This makes Bailian selectable in the provider form and auto-fills both endpoints. After this task the vendor is fully routable (an agent can be pointed at a Bailian-backed profile). `needsUsageCreds` is unchanged and stays false for `bailian-token` → the AK/SK fields stay hidden.

**Files:**
- Modify: `src/lib/selectLabel.ts:26-31` (`vendorOptions`)
- Modify: `src/views/Provider.vue:60-77` (`vendorDefaults`)

**Interfaces:**
- Produces: `"bailian-token"` in `vendorOptions` (so `vendorLabel`/`providerLabel` resolve the label automatically) and in `vendorDefaults` (so `onVendorChange` auto-fills the two base_urls). No new exports.

- [ ] **Step 1: Add the dropdown option**

In `src/lib/selectLabel.ts`, add the Bailian entry to `vendorOptions` (after the `volcengine-coding` line):

```ts
export const vendorOptions = [
  { label: "智谱", value: "zhipu" },
  { label: "DeepSeek", value: "deepseek" },
  { label: "火山 · agent plan", value: "volcengine-agent" },
  { label: "火山 · coding plan", value: "volcengine-coding" },
  { label: "百炼 · token plan", value: "bailian-token" },
];
```

- [ ] **Step 2: Add the base_url defaults**

In `src/views/Provider.vue`, add the `bailian-token` entry to `vendorDefaults` (after the `"volcengine-coding"` entry, before the closing `};`):

```ts
  "bailian-token": {
    openai_base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
    anthropic_base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
  },
```

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: PASS (no type errors).

- [ ] **Step 4: Manual verification**

Run `npm run tauri dev`. In the app: open 服务商 (Sources) → 新增服务商 → pick vendor **百炼 · token plan**. Confirm:
- Both base_url fields auto-fill to the exact URLs in Global Constraints.
- The AccessKey ID / Secret (AK/SK) fields do **not** appear (Bearer-only).
- Save, then set an API key. Run 连接测试 (connection test) — it hits `GET {openai_base_url}/models`.

- [ ] **Step 5: Commit**

```bash
git add src/lib/selectLabel.ts src/views/Provider.vue
git commit -m "feat(provider): register bailian-token vendor + base_url defaults"
```

---

### Task 3: Frontend — honest "不支持" usage chip for vendors without a usage adapter

With Task 2 done, `get_all_usage` creates a usage entry for the Bailian provider whose query errors (`"服务商 'bailian-token' 不支持用量查询"`), so `snapshot` is `null`. Today that renders a **"查询失败"** chip — which conflates "unsupported" with "actually failed". This task introduces `hasUsageAdapter(vendor)` and uses it so a known-but-unadapted vendor shows **"不支持"** while an adapted vendor whose query errored still shows **"查询失败"**. Route-node badges stay absent for Bailian (`deriveNodeQuota` already returns `null` on a null snapshot — no change).

**Files:**
- Modify: `src/lib/selectLabel.ts:47-50` (refine `isUsageSupportedVendor` comment + add `hasUsageAdapter`)
- Modify: `src/lib/quotaUtils.ts:2` (import) and `src/lib/quotaUtils.ts:111-121` (the `!snap` branch in `deriveQuotaChips`)

**Interfaces:**
- Produces: `export function hasUsageAdapter(vendor?: string | null): boolean` in `selectLabel.ts` — true only for `["zhipu", "deepseek", "volcengine-agent", "volcengine-coding"]`. Must be kept in sync with `usage_provider_for` in `src-tauri/src/usage/mod.rs`.
- Consumes: nothing from other tasks (self-contained).

- [ ] **Step 1: Add the `hasUsageAdapter` predicate and refine the sibling comment**

In `src/lib/selectLabel.ts`, replace the `isUsageSupportedVendor` block (the comment + function at lines 47-50):

```ts
// True when the vendor has a usage adapter (the vendorOptions set); false for custom/inference-only.
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return !!vendor && vendorOptions.some((o) => o.value === vendor);
}
```

with:

```ts
// True when the vendor is a *known* vendor eligible for a usage chip slot — it appears in the
// vendorOptions dropdown, so it may render a real value, a "查询失败", or a "不支持" placeholder.
// NOT the same as having a usage adapter: bailian-token is known but has no adapter yet. Use
// hasUsageAdapter for the data-availability check; this gates only chip visibility/skipping.
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return !!vendor && vendorOptions.some((o) => o.value === vendor);
}

// Vendors that have a backend usage adapter (can actually query quota). Distinct from
// isUsageSupportedVendor: bailian-token is a known vendor without an adapter, so a null snapshot
// for it means "不支持", whereas for an adapted vendor it means "查询失败". Keep in sync with
// `usage_provider_for` in src-tauri/src/usage/mod.rs.
const USAGE_ADAPTER_VENDORS = ["zhipu", "deepseek", "volcengine-agent", "volcengine-coding"];
export function hasUsageAdapter(vendor?: string | null): boolean {
  return !!vendor && USAGE_ADAPTER_VENDORS.includes(vendor);
}
```

- [ ] **Step 2: Use it in the chip's null-snapshot branch**

In `src/lib/quotaUtils.ts`, update the import on line 2:

```ts
import { hasUsageAdapter, isUsageSupportedVendor } from "./selectLabel";
```

Then replace the `!snap` branch in `deriveQuotaChips`:

```ts
    // Query failed (supported vendor, but no snapshot).
    if (!snap) {
      chips.push({
        key: entry.provider_id,
        label,
        text: "查询失败",
        tooltip: entry.error ?? undefined,
        status: "neutral",
      });
      continue;
    }
```

with:

```ts
    // No snapshot: an adapted vendor's query actually failed → "查询失败"; a known vendor without an
    // adapter (e.g. bailian-token) → "不支持". Both render a neutral placeholder chip.
    if (!snap) {
      const supported = hasUsageAdapter(provider.vendor);
      chips.push({
        key: entry.provider_id,
        label,
        text: supported ? "查询失败" : "不支持",
        tooltip: supported ? (entry.error ?? undefined) : "该服务商暂不支持用量查询",
        status: "neutral",
      });
      continue;
    }
```

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: PASS.

- [ ] **Step 4: Manual verification**

With a Bailian provider configured (Task 2) and at least one adapted vendor (e.g. 智谱/DeepSeek) also present, open 概览 (Dashboard). Confirm:
- The Bailian quota chip reads **"不支持"** (neutral grey), tooltip **"该服务商暂不支持用量查询"**.
- An adapted vendor whose usage loaded shows its real value; if you temporarily break its key, it shows **"查询失败"** (not "不支持") — the two states are now distinct.
- In 路由 (Routes), a Bailian-backed route node shows **no** quota badge (absent = honest "n/a", since `NodeStatus` has no neutral state).

- [ ] **Step 5: Commit**

```bash
git add src/lib/selectLabel.ts src/lib/quotaUtils.ts
git commit -m "feat(usage): honest 不支持 chip for vendors without a usage adapter"
```

---

## Self-Review (spec coverage)

| Spec section / requirement | Task |
|---|---|
| Dual-protocol native passthrough (Anthropic→`/apps/anthropic`, OpenAI→`/compatible-mode/v1`) | No code — existing dispatch; verified by Task 2 Step 4 manual send |
| `vendorOptions` entry `百炼 · token plan` / `bailian-token` | Task 2 Step 1 |
| `vendorDefaults` base_url defaults | Task 2 Step 2 |
| Bearer-only (no AK/SK form; `needsUsageCreds` unchanged) | Task 2 Step 4 (verifies AK/SK hidden) |
| Catalog: 7 models with exact sizes | Task 1 Step 3 |
| `catalog.rs` guard test | Task 1 Steps 1-2 (TDD) |
| `types.rs` known-vendor doc comment | Task 1 Step 6 |
| `hasUsageAdapter` predicate + honest "不支持"/"查询失败" chip | Task 3 |
| Route-node badge stays absent (no `NodeStatus::neutral`) | Task 3 Step 4 (verifies; no code change) |
| Tray shows the honest backend error string | No change — tray consumes the same `UsageEntry` (spec Out-of-scope #3 notes optional parity tweak) |
| Runtime fallback wiring (no code) | No task — runtime config (spec *Runtime: fallback chains*) |
| Follow-ups: cookie usage adapter, non-429 rate-limit codes, `/models` 404 prompt fallback | No task — tracked in spec Out-of-scope |

Placeholder scan: none (every code step shows exact code). Type consistency: `hasUsageAdapter` is defined in Task 3 Step 1 and consumed in Task 3 Step 2 with the same name/signature; `context_size("bailian-token", …)` matches between Task 1's test and data.
