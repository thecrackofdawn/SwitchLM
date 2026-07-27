# Dashboard "Available Quota" → Per-Provider Chip Strip Design

**Date:** 2026-07-30
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

## Overview

Replace the Dashboard overview's single averaged "当前可用额度" headline number with a
**per-provider chip strip** — one small colored tag per usage-supported provider, each shown in
its own native unit. This removes a metric that cannot be meaningfully aggregated, and lets every
provider kind (plan *and* consumption) appear.

## Background: why the current number is wrong

`Dashboard.vue` computes `availableQuotaPct` (`:63`): for every provider it takes the `five_hour`
window's `used_pct`, converts to "remaining" (`100 − used`), then **averages across providers** and
renders one big percentage with a health color.

The average is not meaningful because the inputs are **incommensurable**:

1. **Plan providers (智谱 / 火山) only expose a percentage.** Each plan tier has a different absolute
   token ceiling (Max / Pro / …), so "100%" means a different quantity per provider. Averaging
   percentages across providers with different denominators has no reference value.
2. **Consumption providers (DeepSeek) report a money balance (CNY), not a percentage.** They have no
   `five_hour` tier (`tiers` is empty), so they are **silently excluded** from the average — they
   cannot be added to a percentage at all.

Rejected alternatives:
- **Weighted average** — there is no known absolute quota for plan tiers to weight by; dead end.
- **`min` remaining across providers** — meaningful for plans, but still excludes consumption, so it
  only half-fixes the problem and keeps the misleading "one number" framing.

The honest fix is to stop aggregating: show each provider's own number in its own unit.

## Requirements

### Functional

1. **One chip per usage-supported provider.** The strip lists every configured provider whose
   `vendor` is recognized by the usage adapter (the `vendorOptions` set), in a single wrapping row
   of tags.
2. **Plan providers show remaining %.** For `zhipu` / `volcengine-agent` / `volcengine-coding`, read
   the `five_hour` tier's `used_pct` and display **remaining** = `100 − used_pct`, e.g. `智谱 剩 8%`.
   The "5h" window is the data source but is **not** written in the chip text.
3. **Consumption providers show balance.** For consumption-billed providers
   (`snapshot.billing_model === "consumption"`; currently DeepSeek), display `unit + remaining`,
   e.g. `DeepSeek CNY 48.77` (same format the Usage page and `providerUsageLabel` already use). The
   branch keys off `billing_model` **only, never the vendor string** — so a future consumption
   vendor (SiliconFlow, OpenRouter, Moonshot, …) renders correctly the moment its adapter is
   registered and it is added to `vendorOptions`.
4. **Hide inference-only providers.** A provider whose `vendor` is not in `vendorOptions` (custom /
   OpenAI-compatible) produces no usage and is **not shown**.
5. **Surface query failures.** A supported provider whose snapshot is missing (query errored) shows
   a muted `查询失败` chip, so the user knows something they expect to track is broken.
6. **Health color (plan only).** Chip color follows remaining thresholds, matching the existing
   badge/`availableStatus` semantics: remaining `< 10%` → red, `< 30%` → amber, else green.
7. **Balance is neutral except a low-balance floor.** Consumption chips are uncolored by default,
   but surface an actionable alert near depletion — a drained balance causes 402/403 mid-session and
   breaks the flow, even though it is not a circuit-breaker event. So: `remaining <= 0` → `danger`
   (欠费停机); `0 < remaining < LOW_BALANCE_THRESHOLD` → `warn`; otherwise neutral.
   `LOW_BALANCE_THRESHOLD` (default `5.0`, defined in `quotaUtils.ts`) is in the same units as
   `remaining` and is **currency-specific (CNY today)** — revisit if a non-CNY consumption vendor is
   added. It is a single named constant, easy to tune or remove.
8. **Urgency sort.** Chips are ordered most-urgent first: plan chips by remaining ascending (red on
   the left), then consumption chips, then `查询失败` chips last. Config order is preserved within a
   group (stable sort).
9. **Keep the "可用额度" framing.** The section label stays "可用额度" (available quota); the metric
   shown is *remaining*, directly answering "how much is left".

### Non-functional

- **Frontend-only.** No backend, no `UsageSnapshot` / `UsageEntry` type changes — all required
  fields already exist. Auto-refresh reuses the page's existing `usePolling`; no new polling.
- **Reuse existing helpers where possible.** The per-provider unit logic mirrors
  `providerUsageLabel` (`Dashboard.vue:41`); the `vendorOptions` set already enumerates
  usage-supported vendors.

## Design

### Data source

`runtime.usage: UsageEntry[]` (from `get_all_usage`, `commands.rs:359`) already contains **one entry
per configured provider**: supported providers carry a `snapshot`; unsupported/errored providers
carry `snapshot: null` with an `error` string. No backend change is needed — the frontend filters
and classifies.

A provider is "usage-supported" iff its `vendor` appears in `vendorOptions`
(`selectLabel.ts:26` = `zhipu`, `deepseek`, `volcengine-agent`, `volcengine-coding`), which is
exactly the set `usage_provider_for` recognizes (`usage/mod.rs:165`).

### New helper — `src/lib/selectLabel.ts`

```ts
// True when the vendor has a usage adapter (the vendorOptions set); false for custom/inference-only.
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return !!vendor && vendorOptions.some((o) => o.value === vendor);
}
```

### Extracted module — `src/lib/quotaUtils.ts`

The chip derivation is **pure** (input: `UsageEntry[]` + `Provider[]`; output: chips). It is
extracted into its own module so `Dashboard.vue` stays lean and the same logic can be reused later
by another frontend surface (e.g. a header status bar). It exports the types, the low-balance
constant, and `deriveQuotaChips`:

```ts
// Low-balance warn floor for consumption providers, in the same units as `remaining`.
// Currency-specific (CNY today) — revisit if a non-CNY consumption vendor is added.
export const LOW_BALANCE_THRESHOLD = 5.0;

export type ChipStatus = "ok" | "warn" | "danger" | "neutral";

export interface QuotaChip {
  key: string;          // provider_id
  label: string;        // provider.display_name — see "label" below
  text: string;         // "剩 8%" | "CNY 48.77" | "无数据" | "查询失败"
  tooltip?: string;     // hover detail (reconciles remaining/used, reset time, or the error)
  status: ChipStatus;   // drives NTag type + sort rank
  sortKey: number;      // ascending → most urgent first
}

/** Build the Dashboard's per-provider quota chips. Pure & order-stable. */
export function deriveQuotaChips(usage: UsageEntry[], providers: Provider[]): QuotaChip[] { /* … */ }
```

Per entry, `deriveQuotaChips`:
- **Skip** if no matching provider or `!isUsageSupportedVendor(provider.vendor)` (hides inference-only).
- **`label`** = `provider.display_name` — kept deliberately concise; the vendor is **not** appended.
  Multi-account display names are already unique within a vendor by construction (the add flow
  auto-suffixes ` 2`, ` 3`, … and enforces `(vendor, display_name)` uniqueness), so the account name
  alone distinguishes accounts. The metric type hints at the vendor (`¥` = consumption, `%` = plan);
  full vendor context stays on the Usage page.
- **`查询失败`** if `entry.snapshot` is null: status `neutral`, sortKey `300`;
  `tooltip = entry.error` (shows *why* on hover).
- **Consumption** (`billing_model === "consumption"`):
  - `remaining == null` → `无数据`, status `neutral`, sortKey `200`, tooltip `暂无可解析的用量数据（详见「用量」页）`.
  - else `text = "${unit} ${remaining.toFixed(2)}"`; `tooltip = "按量付费余额（耗尽停机）"`;
    `status = remaining <= 0 ? "danger" : remaining < LOW_BALANCE_THRESHOLD ? "warn" : "neutral"`;
    sortKey `200`.
- **Plan**: read `snapshot.tiers?.find(t => t.window === "five_hour")?.used_pct`.
  - Absent → `无数据`, status `neutral`, sortKey `200`, tooltip `暂无可解析的用量数据（详见「用量」页）`.
  - Else `remaining = max(0, 100 − used)`; `text = "剩 " + round(remaining) + "%"`;
    `tooltip = "5 小时窗口：剩余 " + remaining + "%（已用 " + round(used) + "%）"` plus
    `，重置于 <reset_at>` when the tier carries `reset_at`;
    `status = remaining < 10 ? "danger" : remaining < 30 ? "warn" : "ok"`; `sortKey = remaining`.

Final list = stable sort ascending by `sortKey` (red plan chips first → neutral → failed).

### Wiring — `src/views/Dashboard.vue`

Dashboard imports `deriveQuotaChips` and exposes it as a single `computed`:

```ts
import { deriveQuotaChips } from "../lib/quotaUtils";
const quotaChips = computed(() => deriveQuotaChips(runtime.usage, config.providers));
```

### Chip text

- Plan: `{label} 剩{n}%` — e.g. `智谱 剩 8%`. (No "5h" in the text; the window is named in the tooltip.)
- Consumption: `{label} {unit} {balance}` — e.g. `DeepSeek CNY 48.77`.
- Failed / no-data: `{label} 查询失败` / `{label} 无数据` (muted).

### Template — replace the right grid cell

The top `NGrid :cols="2"` is unchanged. The second `NGi` (currently `<NStatistic label="当前可用额度">`
+ the big `<span class="mono big …">`) is replaced by a titled chip strip. `NStatistic` is **not**
reused (its large-value styling would inflate the tags); a plain label + `NSpace` is used instead:

```html
<NGi>
  <NCard size="small">
    <div class="quota">
      <span class="quota__title">可用额度</span>
      <NSpace v-if="quotaChips.length" :size="6" wrap align="center">
        <NTooltip v-for="c in quotaChips" :key="c.key" placement="top">
          <template #trigger>
            <NTag :type="tagType(c.status)" size="small" round>{{ c.label }} {{ c.text }}</NTag>
          </template>
          {{ c.tooltip }}
        </NTooltip>
      </NSpace>
      <span v-else class="quota__empty mono">—</span>
    </div>
  </NCard>
</NGi>
```

`tagType` maps `ok→success`, `warn→warning`, `danger→error`, `neutral→default`. Import `NTooltip`
alongside `NTag`. Every chip sets a `tooltip`, so hover always carries detail — and for plan chips
that detail spells out both numbers (`剩余 8%（已用 92%）`), reconciling the chip's remaining % with the
route badge's used % on hover (see Known trade-off).

### Removed code

- `availableQuotaPct` and `availableStatus` computed properties (`Dashboard.vue:62` / `:73`).
- The `.big` / `.big.ok` / `.big.warn` / `.big.danger` styles (now unused).

`providerUsedPct` / `providerUsageLabel` / `quotaPctFor` / `usageLabelFor` are **kept** — the route
section below still uses them (route badges show *used*%, which is intentional and different from
the chip's *remaining*%).

### Edge cases

| Case | Behavior |
|---|---|
| No usage-supported provider configured | strip shows muted `—` |
| First load (usage not yet fetched) | muted `—` (matches prior behavior) |
| Plan provider returns `raw_summary` fallback (no `five_hour` tier) | muted `无数据` chip |
| Consumption balance missing (`remaining == null`) | muted `无数据` chip |
| Many providers | chips wrap to additional rows; layout is not broken |

### Known, accepted trade-off

The route badges directly below show **used %** (e.g. `92%`), while the quota chips show **remaining
%** (e.g. `剩 8%`) for the same provider/window. The two numbers do not match (8 vs 92). This is
intentional: the chip answers "how much is left" (the "可用额度" framing the user asked for), while
the route badge answers "how loaded is this model". Different questions, different number; both turn
red at the same threshold. Hovering a plan chip surfaces both (`剩余 8%（已用 92%）`), so the apparent
mismatch is one hover away from being reconciled.

## Out of scope

- Clickable chips that jump to the Usage tab (possible future enhancement; omitted for v1).
- Converting the balance currency code (`CNY`) to a symbol (`¥`) — kept as `unit` for consistency
  with the Usage page.
- Showing weekly / monthly tiers on the overview (the Usage page already does; the overview stays a
  glance summary).
- Any backend or type changes.
- Introducing a frontend test runner (Vitest). `deriveQuotaChips` is pure so this is easy later, but
  adding the dependency/config is a separate decision (none exists today).

## Testing

- **No frontend test runner exists today** (`package.json` ships only `dev`/`build`/`preview`/`tauri`;
  no Vitest/Jest, zero `*.test.ts`). Because `deriveQuotaChips` is a pure exported function, adding
  unit tests for it is low-effort *if* a runner is introduced — but introducing one is a separate
  infrastructure decision, **out of scope for this feature** unless you want it. Candidate cases if
  added: plan remaining %, consumption balance + low-balance thresholds, hide unsupported vendor,
  `查询失败` on null snapshot (tooltip = error), `无数据` on missing tier, and the urgency ordering
  (danger → warn → ok → neutral → failed).
- **Type safety:** `vue-tsc --noEmit` (the `build` script) type-checks the new module + wiring.
- **Manual:** with a 智谱 + 火山 + DeepSeek config, confirm each chip shows the right unit/color and
  tooltip, and the red/low-balance chip sorts to the front; a custom-vendor provider is absent; a
  provider with a bad key shows `查询失败` (and the error on hover); a DeepSeek balance ≤ 0 shows the
  red 欠费 state and `0 < balance < 5` shows amber.
