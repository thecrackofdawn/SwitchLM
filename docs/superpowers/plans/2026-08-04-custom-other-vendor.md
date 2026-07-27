# "其他" (Custom) Vendor + base_url Label/Placeholder Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users configure a custom ("其他") provider with no preset defaults, and fix the misleading base_url label/placeholder in the provider edit form.

**Architecture:** Frontend-only. Add an "其他" entry to the vendor dropdown backed by a fixed sentinel `vendor: "other"`, excluded from the usage-visibility predicate (no usage chip / tray entry). In the provider edit form, selecting "其他" clears the base_url + account-name fields; the two base_url labels are corrected to convey "one of two required" and the OpenAI placeholder becomes a generic example. Zero backend changes — `"other"` degrades gracefully on every vendor-routed path (verified against `config/types.rs` / `store.rs`).

**Tech Stack:** Vue 3 `<script setup>` + Naive UI + TypeScript (`vue-tsc` type-check). No frontend unit-test runner exists in this repo (`package.json` has no `test` script / no vitest), so the automated gate is **type-check** and runtime behavior is verified **manually** via `pnpm tauri dev`.

## Global Constraints

- **Frontend package manager is `pnpm`.** Never use `npm`/`npx` — use `pnpm`/`pnpm exec` (e.g. `pnpm exec vue-tsc --noEmit`). `npm install` would desync `pnpm-lock.yaml`.
- **Automated frontend gate:** `pnpm exec vue-tsc --noEmit` must pass with no errors. There is no frontend test runner; do not invent one.
- **Commits use explicit `git add <path>` — never `git add -A`** (would sweep the many untracked docs in the working tree). The repo develops directly on `main`; the user prefers batched commits, so per-task commits below may be collapsed into one at the end if the user asks.
- **UI copy is Chinese**, matching the rest of the form.
- **Spec:** `docs/superpowers/specs/2026-08-04-custom-other-vendor-design.md` — read it first; this plan implements it.

---

## File Structure

- **`src/lib/selectLabel.ts`** (Modify) — the single source of vendor options/labels and the usage-visibility predicate. Gains the `OTHER_VENDOR` sentinel, the "其他" option, and the predicate exclusion.
- **`src/views/Provider.vue`** (Modify) — the provider edit modal. `onVendorChange` gains an "其他" branch (clears defaults); two form-item labels and one placeholder are corrected.

No new files. No backend files. No catalog/data changes.

---

## Task 1: Add the "其他" vendor option + exclude it from usage visibility

**Files:**
- Modify: `src/lib/selectLabel.ts` (the `vendorOptions` array ~line 26, the `isUsageSupportedVendor` function + its comment ~lines 47-54)

**Interfaces:**
- Consumes: nothing new.
- Produces: `export const OTHER_VENDOR = "other"` (string); `vendorOptions` now includes `{ label: "其他", value: OTHER_VENDOR }`; `isUsageSupportedVendor("other")` now returns `false`. Task 2 imports `OTHER_VENDOR`.

- [ ] **Step 1: Add the `OTHER_VENDOR` sentinel and the "其他" option**

In `src/lib/selectLabel.ts`, replace the `vendorOptions` declaration (the comment block above it + the array):

```ts
// Known vendors (label/value). Kept here so both Provider.vue and the label
// helpers share one source. Vendors the usage adapter recognizes (can query
// quota); others can be typed in (inference only).
// OTHER_VENDOR is the sentinel for a user-defined ("其他") provider: it is a known dropdown
// option but is NOT usage-supported (no quota to query), so isUsageSupportedVendor returns
// false for it.
export const OTHER_VENDOR = "other";

export const vendorOptions = [
  { label: "智谱", value: "zhipu" },
  { label: "DeepSeek", value: "deepseek" },
  { label: "火山 · agent plan", value: "volcengine-agent" },
  { label: "火山 · coding plan", value: "volcengine-coding" },
  { label: "千问 · token plan", value: "qianwen-token" },
  { label: "其他", value: OTHER_VENDOR },
];
```

- [ ] **Step 2: Exclude `OTHER_VENDOR` from the usage-visibility predicate**

In the same file, replace the `isUsageSupportedVendor` function and the comment immediately above it:

```ts
// True when the vendor is a *known* vendor eligible for a usage chip slot — it appears in the
// vendorOptions dropdown, so it may render a real value, a "查询失败", or a "不支持" placeholder.
// The "其他" sentinel is in vendorOptions but is excluded here: a custom provider has no quota to
// query, so it gets no chip (and no tray usage entry). NOT the same as having a usage adapter: a
// known vendor may still lack an adapter (then a null snapshot renders "不支持"). Use hasUsageAdapter
// for the data-availability check; this gates only chip visibility/skipping.
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return (
    !!vendor &&
    vendor !== OTHER_VENDOR &&
    vendorOptions.some((o) => o.value === vendor)
  );
}
```

- [ ] **Step 3: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS, no errors. (`OTHER_VENDOR` is exported and used within the same file; `hasUsageAdapter` and `USAGE_ADAPTER_VENDORS` are untouched — `"other"` is correctly absent from the adapter list.)

- [ ] **Step 4: Reason about the predicate (no runner exists)**

Confirm by reading: `isUsageSupportedVendor("other")` → `vendor !== OTHER_VENDOR` is `false` → short-circuits to `false`. ✓. `isUsageSupportedVendor("zhipu")` → still `true`. ✓. `vendorLabel("other")` → `vendorOptions.find(...)?.label` → `"其他"` (unchanged function, now finds the new entry). ✓.

- [ ] **Step 5: Commit**

```bash
git add src/lib/selectLabel.ts
git commit -m "feat(frontend): vendorOptions 新增「其他」(vendor=other) 并排除出用量可见性判定"
```

---

## Task 2: Provider form — no defaults for "其他", fix base_url label/placeholder

**Files:**
- Modify: `src/views/Provider.vue` (the `selectLabel` import ~line 20; `onVendorChange` ~lines 100-108; the two base_url `NFormItem`s in the template ~lines 361-366)

**Interfaces:**
- Consumes: `OTHER_VENDOR` (Task 1).
- Produces: the provider edit modal now clears base_url + account name when "其他" is picked, and shows corrected base_url copy.

- [ ] **Step 1: Import `OTHER_VENDOR`**

In `src/views/Provider.vue`, change the `selectLabel` import (line 20):

```ts
import { ellipsisLabel, vendorLabel, vendorOptions, OTHER_VENDOR } from "../lib/selectLabel";
```

- [ ] **Step 2: Add the "其他" branch to `onVendorChange`**

Replace the existing `onVendorChange` (lines 100-108):

```ts
function onVendorChange(vendor: string | number) {
  form.vendor = String(vendor);
  if (form.vendor === OTHER_VENDOR) {
    // 自定义服务商：不预填任何默认值（base_url / 账号名均留空，由用户填写）
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

The early `return` ensures `defaultDisplayName("other")` (which would otherwise yield "其他") never runs for the sentinel. Edit mode is unaffected: the vendor `NSelect` is `:disabled="editing"`, so `onVendorChange` is never called when editing and existing values are preserved.

- [ ] **Step 3: Fix the two base_url form items (label + placeholder)**

Replace the OpenAI + Anthropic base_url `NFormItem` block (lines 361-366):

```vue
<NFormItem label="OpenAI base_url（与 Anthropic 二选一）">
  <NInput v-model:value="form.openai_base_url" placeholder="https://api.example.com/v1" />
</NFormItem>
<NFormItem label="Anthropic base_url">
  <NInput v-model:value="form.anthropic_base_url" placeholder="https://…/anthropic" />
</NFormItem>
```

Changes: OpenAI label gains "（与 Anthropic 二选一）"; Anthropic label drops "（可选）"; OpenAI placeholder changes from the bigmodel URL to `https://api.example.com/v1`. The save-time guard at line 137 (`if (!form.openai_base_url.trim() && !form.anthropic_base_url.trim())`) is unchanged and already enforces "at least one".

- [ ] **Step 4: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS, no errors. (Confirms `OTHER_VENDOR` is imported and used; the new template strings are plain literals.)

- [ ] **Step 5: Manual verification via `pnpm tauri dev`**

Boot the app (`pnpm tauri dev`), open the **服务商** tab, and check:

1. **"其他" appears** in the vendor dropdown alongside the 5 presets.
2. New provider → pick **"其他"** → **both base_url fields are empty** and **account name is empty** (no auto-fill). The OpenAI placeholder reads `https://api.example.com/v1`; the labels read "OpenAI base_url（与 Anthropic 二选一）" and "Anthropic base_url".
3. Save with both base_urls blank → still **rejected** with "至少填一个 base_url（OpenAI 或 Anthropic）".
4. Fill one base_url + an account name → **save succeeds**; the provider list shows the **"其他"** tag + the account name.
5. On the **概览/Dashboard**, that "其他" provider renders **no usage chip** (and the tray usage submenu omits it).
6. **Edit** that provider → base_urls + name are **preserved** (the vendor select is disabled).
7. **Regression:** new provider → pick a known vendor (e.g. 智谱) → both base_urls **auto-fill** as before; then switch the dropdown to **"其他"** → the fields are **cleared**.

- [ ] **Step 6: Commit**

```bash
git add src/views/Provider.vue
git commit -m "feat(frontend): 选「其他」时不预填默认值；base_url 标签改二选一、OpenAI 占位换通用示例"
```

---

## Self-Review

**1. Spec coverage** (each spec requirement → task):
- Req "其他 appears in dropdown" → Task 1 Step 1. ✓
- "Selecting 其他 pre-fills nothing (clear base_urls + blank name)" → Task 2 Step 2. ✓
- "No usage chip / tray entry" → Task 1 Step 2 (predicate exclusion); observable in Task 2 Step 5.5. ✓
- "base_url labels: 二选一 / drop 可选" → Task 2 Step 3. ✓
- "OpenAI placeholder → generic example" → Task 2 Step 3. ✓
- "Zero backend changes" → no backend tasks; stated in Architecture + Global Constraints. ✓
- "No custom vendor-name field / no Models.vue·Profiles.vue changes" → Non-goal, correctly absent from tasks. ✓
- Spec Verification section (type-check + 7 manual checks) → Task 2 Step 5 enumerates them. ✓

**2. Placeholder scan:** No TBD/TODO/"add error handling"/"similar to Task N". Every code step shows the exact replacement. The one "reason about the predicate" step (Task 1 Step 4) is explicit reasoning, not a placeholder for unwritten code. ✓

**3. Type consistency:** `OTHER_VENDOR` is defined in Task 1 Step 1 (`export const OTHER_VENDOR = "other"`) and consumed in Task 2 Step 1 (import) + Step 2 (`form.vendor === OTHER_VENDOR`) — same identifier, same string value. `isUsageSupportedVendor` signature (`(vendor?: string | null) => boolean`) is unchanged. ✓

No issues found.
