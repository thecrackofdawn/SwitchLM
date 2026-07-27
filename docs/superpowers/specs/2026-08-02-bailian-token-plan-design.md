# Bailian (阿里云百炼) token plan Provider Support — Design

**Date:** 2026-08-02
**Status:** Approved
**Author:** Claude (SwitchLM Project)

## Overview

Add **阿里云百炼 (Bailian / Aliyun Model Studio) "token plan"** as a routable provider. An agent
points at the local proxy as usual; SwitchLM forwards to one of two Bailian native endpoints:

| Client protocol | Bailian endpoint |
|---|---|
| OpenAI (Cursor / Cline) | `https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1` |
| Anthropic (Claude Code) | `https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic` |

Both are **native** to their respective client protocols, so SwitchLM does **raw passthrough** on
each — no Anthropic↔OpenAI translation is involved for Bailian. The "token plan" is a subscription
套餐 like the others; its model list spans multiple brands (Qwen, DeepSeek, GLM).

**Usage / balance query is intentionally out of scope for this change.** Bailian exposes balance
only through an authenticated (cookie-based) console session, not a Bearer/AKSK API. The UI will
show an honest "不支持" placeholder until a cookie-based usage adapter is added later.

## Background: why this is (almost) a zero-backend change

SwitchLM routes every vendor-differentiated behavior off `Provider.vendor` (see
`dev-reference/adding-a-provider.md`). Bailian is a **simple Bearer-key, dual-protocol** vendor —
the same shape as DeepSeek — so every vendor-routed path already degrades sensibly without new
backend code:

| Path | Bailian behavior | Backend change? |
|---|---|---|
| Protocol selection (`proxy/dispatch.rs`) | `anthropic_base_url` set → Anthropic **passthrough**; `openai_base_url` set → OpenAI passthrough. Already implemented (DeepSeek uses it). | None |
| Usage adapter (`usage_provider_for`) | `bailian-token` → `None` → usage N/A (intended). | None |
| Model discovery (`discover_models`) | Not Volcengine → generic `GET {openai_base_url}/models`; connection test has a prompt fallback if `/models` is unsupported. | None |
| Same-account conflict (`same_account_conflict`) | Not Volcengine → match on `api_key`. | None |
| Rate-limit detection (`is_rate_limit_error`) | HTTP 429 + keyword fallback covers Bailian's standard rate limits. | None (see *Follow-ups*) |
| Catalog lookup | Keyed by `provider_id == vendor` once catalog entries are added. | Data only |
| base_url defaults / list tag | From `vendorDefaults` / `vendorOptions` once registered. | Frontend only |
| Legacy migration (`normalize_legacy_vendors`) | New vendor, no legacy config. | None |

**Endpoint join rules** (`dispatch.rs::join_url`: trim trailing `/`, then append):
- OpenAI backend → `{openai_base_url}/chat/completions` → `…/compatible-mode/v1/chat/completions` ✓
- Anthropic backend → `{anthropic_base_url}/v1/messages` → `…/apps/anthropic/v1/messages` ✓

## Requirements

### Functional

1. **Selectable known vendor.** "千问 · token plan" appears in the provider vendor dropdown
   (`vendorOptions`) with slug `bailian-token`. Selecting it auto-fills both base_url defaults.
   Multiple Bailian accounts are supported (each gets its own opaque provider `id`), like every
   other vendor.

2. **Dual-protocol passthrough forwarding.** A request whose resolved Profile is backed by a
   Bailian model is forwarded to the matching native endpoint:
   - Anthropic client → raw passthrough to `…/apps/anthropic/v1/messages` (no translation).
   - OpenAI client → raw passthrough to `…/compatible-mode/v1/chat/completions`.
   No new dispatch code; this is the existing `anthropic_passthrough` / `openai_passthrough` path,
   selected because both base_urls are set.

3. **Bearer-key auth.** The Bailian API key is the inference key, stored in the OS keyring like
   every provider (never in `app_config.json`). No AK/SK `usage_creds` form for Bailian.

4. **Catalog context-size hints.** The seven token-plan models are seeded into
   `vendor_model_desc.json` under `provider_id = "bailian-token"` so the Models page shows a
   recognized context size and fallback-capacity checks are accurate.

5. **Honest usage UI.** Because there is no usage adapter yet, Bailian must render an accurate
   **"不支持"** placeholder on usage surfaces — distinct from **"查询失败"** (which means an adapted
   vendor's query actually errored). Concretely:
   - **Dashboard quota chip** (`deriveQuotaChips`): Bailian → neutral "不支持" chip, tooltip
     "该服务商暂不支持用量查询". An adapted vendor (zhipu/deepseek/volcengine-*) whose
     query errors → "查询失败" (unchanged).
   - **Route-node badge** (`deriveNodeQuota`): Bailian → **no badge**. `NodeStatus` has only
     `ok | warn | danger` (no neutral), so "不支持" has no honest status to render; an absent badge
     is the clean truthful state. (This is the existing behavior for any null-snapshot node and
     requires no change.)
   - **Tray usage surfaces** consume the same `UsageEntry`; Bailian's entry carries the backend
     error string and is shown as-is.

6. **Forward-compatible usage.** Adding a cookie-based Bailian usage adapter later is a backend-only
   change plus adding `bailian-token` to the `hasUsageAdapter` set; the UI then flips automatically
   from "不支持" to real data.

### Non-functional

- No new backend dependencies, no new HTTP signing, no new serde shapes.
- No change to the dispatch / breaker / translate code paths.
- Backward compatible: existing configs are untouched; `bailian-token` simply did not exist before.

## Design

### Vendor registration (frontend)

**`src/lib/selectLabel.ts`**
- Add to `vendorOptions`: `{ label: "千问 · token plan", value: "bailian-token" }`.
  (`vendorLabel` / `providerLabel` pick it up automatically.)
- Add a new exported predicate:
  ```ts
  // Vendors that have a backend usage adapter (can actually query quota). Distinct from
  // vendorOptions (known/selectable vendors): bailian-token is known but has no adapter yet, so it
  // is NOT usage-adapter-supported and renders an "不支持" placeholder rather than "查询失败".
  const USAGE_ADAPTER_VENDORS = ["zhipu", "deepseek", "volcengine-agent", "volcengine-coding"];
  export function hasUsageAdapter(vendor?: string | null): boolean {
    return !!vendor && USAGE_ADAPTER_VENDORS.includes(vendor);
  }
  ```
- `isUsageSupportedVendor` is **unchanged** (still "in `vendorOptions`"). Its meaning is refined in
  its doc comment to "known vendor eligible for a usage chip slot (real data, 查询失败, or 不支持
  placeholder)" — it gates *visibility*, not *data availability*. (`hasUsageAdapter` gates the
  latter.) Keeping the name avoids churning its existing call sites and the specs that reference it;
  the added comment removes the surprise.

**`src/views/Provider.vue`** — `vendorDefaults`:
```ts
"bailian-token": {
  openai_base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
  anthropic_base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
},
```
`needsUsageCreds` is unchanged — `form.vendor.startsWith("volcengine")` stays false for
`bailian-token`, so the AK/SK fields are correctly hidden (Bearer-only).

### Honest usage placeholder (frontend)

**`src/lib/quotaUtils.ts`** — `deriveQuotaChips`, in the `!snap` (null-snapshot) branch, replace
the hard-coded `"查询失败"` text with a vendor-aware choice:
```ts
if (!snap) {
  chips.push({
    key: entry.provider_id,
    label,
    text: hasUsageAdapter(provider.vendor) ? "查询失败" : "不支持",
    tooltip: hasUsageAdapter(provider.vendor)
      ? (entry.error ?? undefined)
      : "该服务商暂不支持用量查询",
    status: "neutral",
  });
  continue;
}
```
(The import of `hasUsageAdapter` from `./selectLabel` is added.) `deriveNodeQuota` is **unchanged**:
it already returns `null` for a null snapshot, which is the desired no-badge behavior for Bailian.

> Note: the literal `"查询失败"` also appears in the tray usage rendering. The tray reads the same
> `UsageEntry`; this design keeps the tray as-is for now (it shows the backend error string), since
> the user's requirement is specifically the 概览/路由 chip honesty. If the tray should also say
> "不支持", that is a one-line follow-up tracked below.

### Catalog data

**`src-tauri/assets/vendor_model_desc.json`** — append seven entries (matching the existing
`{ provider_id, upstream_model_id, context_size }` shape, no `display_name`):
```json
{ "provider_id": "bailian-token", "upstream_model_id": "qwen3.8-max-preview", "context_size": 1000000 },
{ "provider_id": "bailian-token", "upstream_model_id": "qwen3.7-max", "context_size": 1000000 },
{ "provider_id": "bailian-token", "upstream_model_id": "qwen3.7-plus", "context_size": 1000000 },
{ "provider_id": "bailian-token", "upstream_model_id": "qwen3.6-flash", "context_size": 128000 },
{ "provider_id": "bailian-token", "upstream_model_id": "deepseek-v4-pro", "context_size": 1048576 },
{ "provider_id": "bailian-token", "upstream_model_id": "deepseek-v4-flash-0731", "context_size": 1048576 },
{ "provider_id": "bailian-token", "upstream_model_id": "glm-5.2", "context_size": 1000000 }
```
`context_size` units follow each model's vendor convention (Qwen/GLM in 1000-base → `1_000_000`;
DeepSeek in 1024-base → `1_048_576`), as given by the user. Duplicate `upstream_model_id`s across
vendors (e.g. `glm-5.2`, `deepseek-v4-pro`) are fine — the catalog is keyed by
`(provider_id, upstream_model_id)`.

### Catalog guard test

**`src-tauri/src/config/catalog.rs`** — in `lookup_finds_bundled_models`, add:
```rust
assert_eq!(cat.context_size("bailian-token", "qwen3.8-max-preview"), Some(1000000));
assert_eq!(cat.context_size("bailian-token", "deepseek-v4-pro"), Some(1048576));
assert_eq!(cat.context_size("bailian-token", "glm-5.2"), Some(1000000));
assert_eq!(cat.context_size("bailian-token", "qwen3.6-flash"), Some(128000));
```

### Known-vendor doc comment

**`src-tauri/src/config/types.rs`** — extend the `Provider.vendor` doc comment's known-vendor list
to include `bailian-token` (documentation only; no runtime effect).

## Runtime: fallback chains (no code change)

Per `dev-reference/adding-a-provider.md`, wiring a Bailian model into a rate-limit fallback chain
is **runtime configuration, not code**: once a Bailian model exists (catalog + Models page done),
the user connects it on the Fallback page (`Model.fallback_target_model_id`). Cross-vendor fallback
works because each model uses its own provider's protocol/base_url — e.g. an Anthropic client whose
primary is a Zhipu model can fall back to a Bailian model (Bailian's `anthropic_base_url` is set →
Anthropic passthrough). The generic 429 rate-limit detection (Follow-up #2's caveat aside) is what
triggers the hop. No implementation work here; called out only for completeness vs. the checklist.

## Testing

- **Backend:** `cargo test --manifest-path src-tauri/Cargo.toml config::catalog` — the new
  assertions guard the seeded sizes.
- **Frontend:** `npx vue-tsc --noEmit` — covers the new `hasUsageAdapter` predicate and the
  `quotaUtils` change.
- **Unit (optional but recommended):** add a `quotaUtils` test asserting a Bailian provider with a
  null snapshot yields a "不支持" neutral chip, while a DeepSeek provider with a null snapshot still
  yields "查询失败" (guards the honesty distinction from regressing).
- **Manual:** add a Bailian provider → vendor selected → both base_urls auto-fill → connection test
  passes → add a model (e.g. `qwen3.7-max`) and confirm the recognized context-size hint → send a
  real Anthropic and OpenAI request through the proxy and confirm native passthrough → confirm the
  dashboard chip reads "不支持".

## Out of scope / Follow-ups

1. **Usage / balance query (cookie-based).** The user explicitly deferred this. When added: a new
   `src-tauri/src/usage/bailian.rs` adapter implementing `UsageProvider` (cookie auth), registered
   in `usage_provider_for`, and `bailian-token` added to `USAGE_ADAPTER_VENDORS`. The UI then flips
   from "不支持" to real data with no further frontend change.
2. **Non-429 rate-limit / plan-exhaustion codes.** If Bailian signals 套餐到期/欠费 with a special
   non-429 code (so it would currently passthrough instead of tripping the breaker and falling
   back), add the per-vendor arm in `proxy/error_adapter.rs::rate_limit_in_json`. The generic 429 +
   keyword fallback is the v1 behavior.
3. **Tray "不支持" label parity** if desired (one-line frontend tweak; see note above).
4. **Model discovery via `/models`.** If Bailian's `compatible-mode/v1` does not answer `GET
   /models`, discovery returns an error and the user adds models manually. Connection test calls
   `GET {openai_base_url}/models`; the frontend offers a prompt-based re-test **only when that
   returns HTTP 404** (user-confirmed, consumes minimal quota) — other non-2xx statuses (401/403/…)
   surface as a plain failure with no fallback. No code change required either way; the only
   implication is that Bailian connection testing is smoothest when `/models` is supported.
