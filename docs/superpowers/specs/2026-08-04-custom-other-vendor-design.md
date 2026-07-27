# "其他" (Custom) Vendor + base_url Label/Placeholder Fixes — Design

**Date:** 2026-08-04
**Status:** Approved (amended 2026-08-04 — decoupled `isUsageSupportedVendor` from `vendorOptions` into a positive `USAGE_SUPPORTED_VENDORS` list; see Design §`src/lib/selectLabel.ts`)
**Author:** Claude (SwitchLM Project)

## Overview

Add an **"其他" (Other)** entry to the provider vendor dropdown so a user can configure a provider
that is not one of the five presets (智谱 / DeepSeek / 火山×2 / 千问). Selecting "其他" stores the
fixed sentinel `vendor: "other"` and leaves every form field blank — no pre-filled base_urls, no
auto-filled account name. The account `display_name` is the only disambiguator between multiple
"其他" providers.

Two related base_url UX fixes land in the same change:

1. The OpenAI base_url placeholder is no longer the 智谱 (bigmodel) URL — it becomes a simple
   generic example.
2. The Anthropic base_url field is no longer labeled "（可选）", which falsely implied the OpenAI
   base_url was required. Both fields now convey the real rule: **at least one of the two is
   required** (a rule the save validation already enforces).

## Background: why this is a zero-backend change

`Provider.vendor` is a plain `String` (`config/types.rs`); there is **no vendor whitelist**.
`normalize_legacy_vendors` (`config/store.rs`) only backfills an empty vendor from the legacy `id`,
and remaps the old `bailian-token` slug to `qianwen-token`. An `"other"` vendor is untouched and
degrades gracefully on every vendor-routed path — the same property CLAUDE.md documents for "a
simple Bearer-key provider":

| Path | `"other"` behavior | Backend change? |
|---|---|---|
| Protocol selection (`proxy/dispatch.rs`) | `anthropic_base_url` set → Anthropic passthrough/translate; `openai_base_url` set → OpenAI. Existing logic. | None |
| Usage adapter (`usage_provider_for`) | `"other"` → `None` → usage N/A. | None |
| Model discovery (`discover_models`) | Not Volcengine → generic `GET {openai_base_url}/models`; connection test has a prompt fallback if `/models` is unsupported. | None |
| Same-account conflict (`same_account_conflict`) | Not Volcengine → match on `api_key`. | None |
| Rate-limit detection (`is_rate_limit_error`) | HTTP 429 + keyword fallback. | None |
| Catalog lookup (`context_size`) | No entry for `"other"` → no recognized-size hint. | None |
| Legacy migration | New sentinel, no legacy config. | None |

So the entire change is **frontend-only**: `src/lib/selectLabel.ts` + `src/views/Provider.vue`.

## Requirements

### Functional

1. **"其他" appears in the vendor dropdown.** `vendorOptions` gains
   `{ label: "其他", value: "other" }`. It shows in the `NSelect` exactly like the presets.

2. **Selecting "其他" pre-fills nothing.** In `Provider.vue::onVendorChange`, the `"other"` branch:
   - clears `openai_base_url` and `anthropic_base_url` to `""`;
   - leaves `displayName` blank (no auto-suffixed account name) when creating a new provider.
   Known vendors keep their current auto-fill behavior unchanged. (Edit mode never calls
   `onVendorChange` — the vendor select is `:disabled="editing"` — so an existing provider's
   values are always preserved on open.)

3. **No usage chip / tray usage entry for custom providers.** `isUsageSupportedVendor` is defined
   by a **positive list** (`USAGE_SUPPORTED_VENDORS` — the 5 known vendors), independent of
   `vendorOptions`, so `isUsageSupportedVendor("other")` is `false` because "其他" is simply not in
   that list. The Dashboard quota chip and the tray usage submenu skip "其他" providers entirely (an
   inference-only custom provider has no quota to query). This decoupling is the point:
   `vendorOptions` is a UI concern (dropdown presets, which now includes the "其他" sentinel) while
   usage-eligibility is a data concern — deriving the latter from the former would force an
   `!== OTHER_VENDOR` exclusion special-case; the positive list makes "其他 is not usage-eligible"
   a property of list membership instead. `hasUsageAdapter("other")` is `false` (not in
   `USAGE_ADAPTER_VENDORS`); `vendorLabel("other")` returns "其他" via the existing
   `vendorOptions.find` — both unchanged.

4. **base_url label reflects "one of two required".**
   - OpenAI base_url form-item label → `"OpenAI base_url（与 Anthropic 二选一）"` (states the rule
     once, on the first field).
   - Anthropic base_url form-item label → `"Anthropic base_url"` (drops "（可选）").
   The save-time guard (`Provider.vue::save`) already rejects an empty pair with
   "至少填一个 base_url（OpenAI 或 Anthropic）" — **unchanged**.

5. **OpenAI base_url placeholder is a generic example.** The `NInput` placeholder changes from
   `https://open.bigmodel.cn/api/coding/paas/v4` to `https://api.example.com/v1`. (It is only
   visible when the field is empty — e.g. for "其他" or before a vendor is picked — because a
   known vendor selection fills the real default.) The Anthropic placeholder (`https://…/anthropic`)
   is already generic and is left alone.

### Non-goals

- **No custom vendor *name* field.** "其他" is a fixed sentinel; multiple custom providers all show
  the "其他" tag and are distinguished by `display_name`. (A free-text vendor-name option was
  considered and rejected as unnecessary given the account name already disambiguates.)
- **No changes to Models.vue / Profiles.vue.** Neither has vendor-dependent pre-fill — they inherit
   protocol/endpoint from the provider. The static model-name placeholder `"glm-4.6"` is a hint,
   not a pre-filled value, and is left alone.
- **No backend, catalog, or spec-§changes.**

## Design

### `src/lib/selectLabel.ts`

Add the option and a named sentinel constant; define usage-eligibility as a **positive list
independent of `vendorOptions`** (so the "其他" preset is a dropdown entry but not usage-eligible,
with no exclusion special-case):

```ts
export const OTHER_VENDOR = "other";

export const vendorOptions = [
  { label: "智谱", value: "zhipu" },
  { label: "DeepSeek", value: "deepseek" },
  { label: "火山 · agent plan", value: "volcengine-agent" },
  { label: "火山 · coding plan", value: "volcengine-coding" },
  { label: "千问 · token plan", value: "qianwen-token" },
  { label: "其他", value: OTHER_VENDOR },
];

// ... vendorLabel / providerLabel unchanged ...

// Positive list — independent of vendorOptions (which also holds the "其他" sentinel).
const USAGE_SUPPORTED_VENDORS = ["zhipu", "deepseek", "volcengine-agent", "volcengine-coding", "qianwen-token"];
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return !!vendor && USAGE_SUPPORTED_VENDORS.includes(vendor);
}
```

`hasUsageAdapter` and `USAGE_ADAPTER_VENDORS` are unchanged (`"other"` is not listed). Today
`USAGE_SUPPORTED_VENDORS` and `USAGE_ADAPTER_VENDORS` hold the same slugs, but they are
intentionally separate: a future vendor could be usage-*supported* (its chip renders "不支持")
before it gets a usage *adapter* (real data). `isUsageSupportedVendor` gates chip visibility;
`hasUsageAdapter` gates data availability.

### `src/views/Provider.vue`

`onVendorChange` gains an explicit `"other"` branch (clears fields, no default name); the known
vendor path is unchanged:

```ts
function onVendorChange(vendor: string | number) {
  form.vendor = String(vendor);
  if (form.vendor === OTHER_VENDOR) {
    // 自定义服务商：不预填任何默认值（base_url / 账号名均留空）
    form.openai_base_url = "";
    form.anthropic_base_url = "";
    if (!editing.value) form.displayName = "";
    return;
  }
  const d = vendorDefaults[form.vendor];
  if (d) {
    form.openai_base_url = d.openai_base_url;
    form.anthropic_base_url = d.anthropic_base_url;
  }
  if (!editing.value) form.displayName = defaultDisplayName(form.vendor);
}
```

(`OTHER_VENDOR` is imported from `./lib/selectLabel`.)

Template — the two base_url form items:

```vue
<NFormItem label="OpenAI base_url（与 Anthropic 二选一）">
  <NInput v-model:value="form.openai_base_url" placeholder="https://api.example.com/v1" />
</NFormItem>
<NFormItem label="Anthropic base_url">
  <NInput v-model:value="form.anthropic_base_url" placeholder="https://…/anthropic" />
</NFormItem>
```

Nothing else in the template changes.

## Verification

- **Type-check:** `pnpm exec vue-tsc --noEmit` — covers the imported `OTHER_VENDOR` symbol and the
  predicate.
- **Manual (Provider tab):**
  1. New provider → pick "其他" → both base_urls empty, account name empty. Save with both blank →
     still rejected with "至少填一个 base_url…".
  2. Fill one base_url + a name → save → success; list shows the "其他" tag + the account name.
  3. After saving an "其他" provider, the Dashboard shows **no** usage chip for it, and the tray
     usage submenu omits it.
  4. Edit that "其他" provider → base_urls/name preserved (select disabled).
  5. Regression: pick a known vendor (e.g. 智谱) → both base_urls still auto-fill; switching from a
     known vendor to "其他" clears them.
