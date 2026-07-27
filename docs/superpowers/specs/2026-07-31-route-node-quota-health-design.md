# Route-Node Quota = "Remaining / Health" View Design

**Date:** 2026-07-31
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)
**Supersedes:** the "Known, accepted trade-off" section of
`2026-07-30-dashboard-available-quota-chips-design.md`.

## Overview

Make every quota badge on the Dashboard **route** answer the same question the
`可用额度` chip strip directly above it answers — *"how much is left / how healthy is this
node"* — driven by the **same data source and the same thresholds** as the chips.

Concretely, a plan-billed route node (智谱 / 火山) that today shows **used %** (e.g. `72%`)
will instead show **remaining %** as a compact, health-colored number (e.g. `28%`, amber),
with the `已用 72%` detail on hover. Consumption nodes (DeepSeek) keep showing the balance
(`CNY 47.37`) but stop being force-greened — they color by health like everything else.

This removes the framing split the prior chips spec deliberately documented as a trade-off,
and also fixes a deeper data-source divergence (see Background).

## Background: two layers of inconsistency

### 1. The framing split (documented, now reversed)

`2026-07-30-dashboard-available-quota-chips-design.md` §"Known, accepted trade-off" states:
chips show **remaining %** (`智谱 剩 8%`), route badges show **used %** (`智谱 92%`), and this
is "intentional … different questions, different number." The user's feedback is that this
split is itself the problem — the two areas use incommensurable framings, and the route node
should answer the *same* "how much is left" question.

### 2. The data-source divergence (latent, less obvious)

The route badge and the chip can disagree even on the *used* number, because they read
different fields:

| Surface | Source | Where |
|---|---|---|
| Route badge (today) | top-level `snapshot.used / snapshot.total` | `Dashboard.vue:36-40` (`providerUsedPct`) |
| Chips + Usage page | `snapshot.tiers[five_hour].used_pct` | `quotaUtils.ts:94`, `Usage.vue:46` |

The top-level `used`/`remaining` are populated from the **first** tier by `snapshot_from_tiers`
(`usage/mod.rs:72-99`), so they only equal the `five_hour` value when the first tier *happens
to be* `five_hour`. Unifying the framing therefore also requires unifying the **source**: route
nodes must read the `five_hour` tier, exactly as chips do.

### 3. The consumption force-green bug

`RouteLine.vue:84` renders `<span v-if="usageLabel" class="badge ok">` — when a consumption
balance is shown (e.g. `CNY 47.37`), the badge is unconditionally green, regardless of how low
the balance is. A near-zero DeepSeek balance thus reports "healthy" on the route while the same
provider's chip can already be `warn`/`danger`. Fixed as part of this work (consumption colors
by balance, like chips).

## Requirements

### Functional

1. **Plan route nodes show remaining %.** For `zhipu` / `volcengine-*`, read
   `snapshot.tiers[five_hour].used_pct` and display **remaining** = `max(0, 100 − used)` as a
   compact number (e.g. `28%`) — **no** `剩` prefix (that stays chip-only). Color and tooltip
   are derived from the same core as chips (see Design).
2. **Consumption route nodes show balance, colored.** `billing_model === "consumption"`
   displays `unit + remaining` (e.g. `CNY 47.37`) — unchanged text — but the badge color now
   follows balance thresholds instead of being force-green.
3. **One data source.** Both the chip and the route badge read the `five_hour` tier (plan) /
   `snapshot.remaining` (consumption) via one shared function. The `used/total` path is deleted.
4. **One threshold rule.** Health boundaries are identical on both surfaces (see table below).
5. **Hover carries the reconciling detail.** Every route badge is wrapped in an `NTooltip`
   showing the full breakdown — plan: `5 小时窗口：可用 28%（已用 72%），重置于 <ts>`; consumption:
   `按量付费余额（耗尽停机）`. (Route badges have no tooltip today.)
6. **Absent data → no badge.** A node whose provider is usage-unsupported (custom /
   OpenAI-compatible), whose usage query failed, or whose `five_hour` tier / `remaining` is
   missing renders **no badge** (not a `查询失败`/`无数据` tag). Failures are already surfaced
   per-provider in the chip strip above and on the Usage page; the route node stays a clean
   pipeline element.
7. **Overflow list shows remaining.** The fallback `+N` tooltip lists each hop with
   `· {quota.text}` (remaining), not `· {usedPct}%`. Per-line coloring stays absent — only the
   hop[0] trailing badge is colored, as today.
8. **Cooling mark is orthogonal.** The circuit-breaker `❄` / amber border (`cooling` prop) is
   independent of quota and unchanged; a node may show both a quota badge and a cooling tint.

### Non-functional

- **Frontend-only.** No backend, `UsageSnapshot`, or `UsageTier` changes — all required fields
  already exist.
- **Single source of truth.** Threshold and tooltip logic lives in `quotaUtils.ts`; `RouteLine`
  and `Usage.vue`-adjacent code consume it rather than re-deriving. (`Usage.vue` itself is out
  of scope and keeps its own inline thresholds — see Out of scope.)
- **`LOW_BALANCE_THRESHOLD` raised 5.0 → 10.0.** Because the constant is shared, this also
  shifts the **already-shipped chips** from warning below ¥5 to warning below ¥10. Intentional:
  one rule for both surfaces.

## Design

### Shared core — `src/lib/quotaUtils.ts`

Extract the per-snapshot math so chips and nodes cannot drift. Three private pieces plus two
public entry points:

```ts
// Private — the shared math. Returns null when no number can be parsed
// (query failed, tier missing, remaining null). Billing keys off billing_model, never vendor.
interface QuotaMath {
  billing: "plan" | "consumption";
  remaining: number;        // plan: 0-100 remaining %; consumption: balance
  used?: number;            // plan only — surfaces in tooltip
  unit?: string;            // consumption only
  resetAt?: number | null;  // plan: five_hour.reset_at
}
function quotaMathFor(snap: UsageSnapshot): QuotaMath | null;

// Shared thresholds (one rule). NodeStatus ⊂ ChipStatus.
export type NodeStatus = "ok" | "warn" | "danger";
function statusFor(m: QuotaMath): NodeStatus;
function tooltipFor(m: QuotaMath): string;
```

`statusFor` encodes the unified table:

| Billing | `ok` (green) | `warn` (amber) | `danger` (red) |
|---|---|---|---|
| plan — remaining % | `≥ 30` | `10–29` | `< 10` |
| consumption — balance | `≥ LOW_BALANCE_THRESHOLD` | `0 < b < LOW_BALANCE_THRESHOLD` | `≤ 0` |

with `LOW_BALANCE_THRESHOLD = 10.0` (was `5.0`).

Two thin text formatters — the **only** place chip and node presentation diverge:

```ts
// Chip keeps the explicit "剩" prefix on plan; node is the bare number.
function textForChip(m): string =>
  m.billing === "consumption" ? `${m.unit} ${m.remaining.toFixed(2)}` : `剩 ${round1(m.remaining)}%`;
function textForNode(m): string =>
  m.billing === "consumption" ? `${m.unit} ${m.remaining.toFixed(2)}` : `${round1(m.remaining)}%`;
```

#### New public entry point — node

```ts
/** Quota for a route node. null → caller renders no badge
 *  (unsupported vendor / query failed / tier or balance missing). */
export interface NodeQuota {
  text: string;          // "28%" | "CNY 47.37"
  status: NodeStatus;    // → badge CSS class directly (ok | warn | danger)
  tooltip?: string;      // hover detail — same copy as the chip
}
export function deriveNodeQuota(entry: UsageEntry, provider: Provider): NodeQuota | null;
```

`deriveNodeQuota` returns `null` if `!isUsageSupportedVendor(provider.vendor)`, if
`entry.snapshot` is null, or if `quotaMathFor` returns null. Otherwise returns
`{ text: textForNode(m), status: statusFor(m), tooltip: tooltipFor(m) }`.

#### Refactored entry point — chips

`deriveQuotaChips` is rewritten to delegate to the same core. Per entry:
- unsupported vendor → skip (unchanged);
- `snapshot == null` → `查询失败` chip (status `neutral`, tooltip = `entry.error`);
- `quotaMathFor == null` → `无数据` chip (status `neutral`);
- else chip with `text: textForChip(m)`, `status: statusFor(m)`, `tooltip: tooltipFor(m)`,
  and the existing sortKey (plan = `remaining`, consumption/no-data = `200`, failed = `300`).

Behavior matches the shipped chips **except** two intended changes: the raised low-balance
threshold, and plan chip text gains a `剩` prefix (`8%` → `剩 8%`) — mirroring the original
`2026-07-30` chips spec's intent and the remaining/health framing shared with the node.

### `RouteLine.vue`

- **Props:** remove `quotaPct` and `usageLabel`; add `quota?: NodeQuota | null`. `FallbackHop`
  removes `usedPct` / `usageLabel`, adds `quota?: NodeQuota | null`.
- **Delete `badgeClassFor`** (`:47-52`) — `NodeQuota.status` is already the CSS class.
- **Primary badge** (was `:84-85`): one path, no force-`ok` branch —
  ```html
  <NTooltip v-if="quota" placement="top">
    <template #trigger>
      <span class="badge" :class="quota.status">{{ quota.text }}</span>
    </template>
    {{ quota.tooltip }}
  </NTooltip>
  ```
- **Fallback hop[0] badge** (was `:116-126`): same shape using `fallbackChain[0].quota`.
- **Overflow per-hop list** (was `:108-112`): `<template v-if="f.quota"> · {{ f.quota.text }}</template>`.
- `NTooltip` is already imported (`:2`); no new imports. CSS tokens unchanged.

### `Dashboard.vue`

- **Delete** `providerUsedPct`, `providerUsageLabel`, `quotaPctFor`, `usageLabelFor`
  (`:36-63`) — the `used/total` path is gone entirely.
- **Add:**
  ```ts
  function nodeQuotaForProvider(providerId: string): NodeQuota | null {
    const entry = runtime.usage.find((u) => u.provider_id === providerId);
    const provider = config.providers.find((p) => p.id === providerId);
    return entry && provider ? deriveNodeQuota(entry, provider) : null;
  }
  function nodeQuotaForProfile(p: Profile | undefined): NodeQuota | null {
    const m = backingModelFor(p);
    return m ? nodeQuotaForProvider(m.provider_id) : null;
  }
  ```
- `fallbackChainFor` pushes `quota: nodeQuotaForProvider(target.provider_id)` per hop (replaces
  `usedPct` / `usageLabel`).
- `<RouteLine>` wiring (`:270-282`): `:quota="nodeQuotaForProfile(p)"` replaces `:quota-pct` and
  `:usage-label`.

## Edge cases

| Case | Behavior |
|---|---|
| Custom / OpenAI-compatible vendor (no usage adapter) | no badge (`deriveNodeQuota` → null) |
| Usage query errored (`snapshot == null`) | no badge; the chip strip above shows `查询失败` |
| Plan provider returns `raw_summary` fallback (no `five_hour` tier) | no badge |
| Consumption balance missing (`remaining == null`) | no badge |
| Empty fallback chain | unchanged `未配置` |
| Multiple models on one provider | each node reads the same provider quota (badge repeats) — correct, since the quota is per-account |
| Cooling model | `❄` + amber border unchanged, quota badge shown alongside |

## Removed code

- `providerUsedPct` (`Dashboard.vue:36-40`), `providerUsageLabel` (`:43-51`),
  `quotaPctFor` (`:56-59`), `usageLabelFor` (`:60-63`). **Keep** `backingModelFor` (`:53-55`) —
  `nodeQuotaForProfile` still uses it.
- `badgeClassFor` (`RouteLine.vue:47-52`) and the `usageLabel` force-`ok` branch (`RouteLine.vue:84`).
- The `usedPct` / `usageLabel` fields on `FallbackHop`.

## Out of scope

- **Usage page** (`Usage.vue` `NProgress` bars) — stays used-%-fill (the right viz for a detailed
  per-window breakdown); its inline `status()` thresholds are left as-is.
- Backend / `UsageSnapshot` / `UsageTier` shape changes — none needed.
- Making chips clickable, `CNY` → `¥`, or weekly/monthly tiers on the overview.
- Adding a frontend test runner (Vitest). `deriveNodeQuota` / `quotaMathFor` are pure and are the
  natural unit-test targets if a runner is introduced, but adding one is a separate infra decision.

## Testing

- **Type safety:** `npm run build` (`vue-tsc --noEmit`) covers the new/changed module + wiring.
- **Manual:** with 智谱 + 火山 + DeepSeek configured, confirm on the Dashboard:
  - each route node badge shows the **same number + color** as its chip directly above (plan
    remaining %, consumption balance);
  - hovering a plan badge shows `可用 …%（已用 …%）`;
  - a DeepSeek balance `< ¥10` shows amber and `≤ ¥0` shows red (not the old forced green);
  - a custom-vendor model shows no badge;
  - the `+N` overflow lists hops with remaining numbers.
- Because `LOW_BALANCE_THRESHOLD` is shared, also confirm the **chips** now amber below ¥10.
