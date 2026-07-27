# Route-Node Quota "Remaining / Health" View — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Dashboard route-node quota badges show the same remaining/health view (same number, source, and thresholds) as the `可用额度` chip strip above them.

**Architecture:** Extract one shared pure core in `src/lib/quotaUtils.ts` (`quotaMathFor` / `statusFor` / `tooltipFor`) consumed by both the existing chip builder and a new `deriveNodeQuota` entry point. `RouteLine.vue` drops its `used%`+`usageLabel` props for a single `quota: NodeQuota | null` prop and renders one health-colored badge (with tooltip). `Dashboard.vue` replaces its four `used/total` helpers with `nodeQuotaForProvider` / `nodeQuotaForProfile` built on `deriveNodeQuota`.

**Tech Stack:** Vue 3 `<script setup>` + TypeScript, Naive UI (`NTooltip`), Pinia. No backend, no new deps.

## Global Constraints

- **Frontend-only.** No Rust, no `UsageSnapshot`/`UsageTier`/`types.ts` changes — every field needed already exists.
- **No frontend test runner exists** (no Vitest/Jest in `package.json`; zero `*.test.ts`). Do **not** add one — out of scope per spec. The automated gate is `npx vue-tsc --noEmit`; behavior is verified manually with `npm run tauri dev`. The pure functions (`quotaMathFor`, `statusFor`, `deriveNodeQuota`) are the unit-test surface if a runner is introduced later.
- **One shared threshold constant.** `LOW_BALANCE_THRESHOLD` lives in `quotaUtils.ts` and is read by both chips and nodes. Bumping it 5.0 → 10.0 intentionally also shifts the already-shipped chips (warn below ¥10 instead of ¥5).
- **UI copy is Chinese; reuse the existing `--sl-ok` / `--sl-cool` / `--sl-trip` color tokens** — do not introduce new colors. Badge CSS classes are exactly `ok` / `warn` / `danger`.
- **Code style:** 2-space indent, double quotes, trailing commas in multiline — match the surrounding files.
- **Workflow:** develop directly on `main`. Commit steps below are logical checkpoints; batch or hold them per the user's commit-when-asked preference.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `src/lib/quotaUtils.ts` | Pure quota derivation (single source of truth for thresholds/text/tooltip) | Modify: extract shared core, refactor `deriveQuotaChips`, add `deriveNodeQuota`, bump constant |
| `src/components/RouteLine.vue` | Renders one profile's route pipeline + quota badges | Modify: prop contract, badge template, overflow list |
| `src/views/Dashboard.vue` | Mounts `RouteLine` per profile, feeds it data | Modify: replace `used/total` helpers with `nodeQuotaFor*`, update chain + wiring |

No new files. `src/lib/types.ts` is unchanged.

---

### Task 1: Extract shared quota core; refactor chips; raise low-balance threshold

Refactor `quotaUtils.ts` so chip and node derivation share one private core. Behavior of `deriveQuotaChips` is unchanged **except** the raised `LOW_BALANCE_THRESHOLD` (5.0 → 10.0). No new exports yet.

**Files:**
- Modify: `src/lib/quotaUtils.ts` (whole file)

**Interfaces:**
- Produces (private, used internally this task): `quotaMathFor(snap: UsageSnapshot): QuotaMath | null`, `statusFor(m: QuotaMath): "ok" | "warn" | "danger"`, `tooltipFor(m: QuotaMath): string`, `textForChip(m: QuotaMath): string`.
- Produces (existing, signature unchanged): `deriveQuotaChips(usage, providers)`, `LOW_BALANCE_THRESHOLD` (now `10.0`), `ChipStatus`, `QuotaChip`.

- [ ] **Step 1: Replace `src/lib/quotaUtils.ts` with the refactored content**

```ts
import type { Provider, UsageEntry, UsageSnapshot } from "./types";
import { isUsageSupportedVendor } from "./selectLabel";

// Low-balance warn floor for consumption providers, in the same units as `snapshot.remaining`.
// Currency-specific (CNY today) — revisit if a non-CNY consumption vendor is added.
// Shared by the 可用额度 chips AND the route-node badges, so both surfaces always agree.
export const LOW_BALANCE_THRESHOLD = 10.0;

export type ChipStatus = "ok" | "warn" | "danger" | "neutral";

export interface QuotaChip {
  /** provider_id — stable Vue v-for key. */
  key: string;
  /** provider.display_name (concise; vendor is NOT appended). */
  label: string;
  /** Chip body: "剩 8%" | "CNY 48.77" | "无数据" | "查询失败". */
  text: string;
  /** Hover detail. */
  tooltip?: string;
  /** Drives NTag type + sort rank. */
  status: ChipStatus;
  /** Ascending → most urgent first. */
  sortKey: number;
}

const NO_DATA_TOOLTIP = "暂无可解析的用量数据（详见「用量」页）";

function round1(n: number): number {
  return Math.round(n * 10) / 10;
}

function fmtReset(epochSecs: number): string {
  return new Date(epochSecs * 1000).toLocaleString();
}

// --- Shared core: per-snapshot quota math used by both chips and route nodes. ---

interface QuotaMath {
  billing: "plan" | "consumption";
  remaining: number; // plan: 0-100 remaining %; consumption: balance
  used?: number; // plan only — surfaces in tooltip
  unit?: string; // consumption only
  resetAt?: number | null; // plan: five_hour.reset_at
}

/**
 * Parse a snapshot into the shared quota math. Returns null when no number can be
 * derived (missing five_hour tier for plan, or null remaining for consumption).
 * Plan reads the five_hour window specifically; consumption reads snapshot.remaining.
 */
function quotaMathFor(snap: UsageSnapshot): QuotaMath | null {
  if (snap.billing_model === "consumption") {
    const remaining = snap.remaining;
    if (remaining == null) return null;
    return { billing: "consumption", remaining, unit: snap.unit };
  }
  const fiveHour = snap.tiers?.find((t) => t.window === "five_hour");
  const used = fiveHour?.used_pct;
  if (used == null) return null;
  return {
    billing: "plan",
    remaining: Math.max(0, 100 - used),
    used,
    resetAt: fiveHour?.reset_at,
  };
}

/** Shared health status from the shared math. (Chips also use "neutral"; this never does.) */
function statusFor(m: QuotaMath): "ok" | "warn" | "danger" {
  if (m.billing === "consumption") {
    return m.remaining <= 0 ? "danger" : m.remaining < LOW_BALANCE_THRESHOLD ? "warn" : "ok";
  }
  return m.remaining < 10 ? "danger" : m.remaining < 30 ? "warn" : "ok";
}

/** Shared tooltip body — same copy on chip and node. `used` is always set for plan. */
function tooltipFor(m: QuotaMath): string {
  if (m.billing === "consumption") return "按量付费余额（耗尽停机）";
  let tip = `5 小时窗口：可用 ${round1(m.remaining)}%（已用 ${round1(m.used!)}%）`;
  if (m.resetAt != null) tip += `，重置于 ${fmtReset(m.resetAt)}`;
  return tip;
}

/** Chip text: plan keeps the explicit "剩" prefix. */
function textForChip(m: QuotaMath): string {
  return m.billing === "consumption"
    ? `${m.unit} ${m.remaining.toFixed(2)}`
    : `剩 ${round1(m.remaining)}%`;
}

/**
 * Build the Dashboard's per-provider quota chips. Pure & order-stable.
 *
 * - plan providers → five_hour window remaining % (status by remaining thresholds)
 * - consumption providers → balance (red at ≤0 / below LOW_BALANCE_THRESHOLD, else green)
 * - unsupported vendor → skipped; query failure → 查询失败; parseable but missing data → 无数据
 *
 * Sort: stable, ascending by sortKey → red plan chips first, then neutral, then 查询失败.
 */
export function deriveQuotaChips(usage: UsageEntry[], providers: Provider[]): QuotaChip[] {
  const chips: QuotaChip[] = [];
  for (const entry of usage) {
    const provider = providers.find((p) => p.id === entry.provider_id);
    if (!provider || !isUsageSupportedVendor(provider.vendor)) continue;

    const label = provider.display_name;
    const snap = entry.snapshot;

    // Query failed (supported vendor, but no snapshot).
    if (!snap) {
      chips.push({
        key: entry.provider_id,
        label,
        text: "查询失败",
        tooltip: entry.error ?? undefined,
        status: "neutral",
        sortKey: 300,
      });
      continue;
    }

    const m = quotaMathFor(snap);
    if (!m) {
      chips.push({
        key: entry.provider_id,
        label,
        text: "无数据",
        tooltip: NO_DATA_TOOLTIP,
        status: "neutral",
        sortKey: 200,
      });
      continue;
    }

    chips.push({
      key: entry.provider_id,
      label,
      text: textForChip(m),
      tooltip: tooltipFor(m),
      status: statusFor(m),
      sortKey: m.billing === "plan" ? m.remaining : 200,
    });
  }

  // Array#sort is stable in modern engines → most urgent first, config order preserved within a group.
  return chips.sort((a, b) => a.sortKey - b.sortKey);
}
```

- [ ] **Step 2: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors. (Only `quotaUtils.ts` changed; its existing consumers — `Dashboard.vue` chips — still compile.)

- [ ] **Step 3: Manual smoke — chips unchanged except the low-balance floor**

Run: `npm run tauri dev`. On the Dashboard, confirm the `可用额度` chips still render as before (智谱 `剩 X%`, 火山 `剩 Y%`, DeepSeek `CNY Z`). The only behavioral change is the raised threshold: a DeepSeek balance between ¥5 and ¥10 now renders amber where it was green. (If you don't have such a balance handy, the logic is a one-line constant change — visual confirmation of the other chips rendering correctly is sufficient here.)

- [ ] **Step 4: Commit**

```bash
git add src/lib/quotaUtils.ts
git commit -m "refactor(quota): extract shared quota core; raise low-balance threshold to 10"
```

---

### Task 2: Add the route-node entry point `deriveNodeQuota`

Add the new types and the node-facing entry point. They consume Task 1's private core. Nothing uses them yet (Task 3 wires them in), so this task is purely additive and type-checks green on its own.

**Files:**
- Modify: `src/lib/quotaUtils.ts` (append at end of file)

**Interfaces:**
- Consumes: Task 1's private `quotaMathFor` / `statusFor` / `tooltipFor`.
- Produces (new public exports): `type NodeStatus = "ok" | "warn" | "danger"`; `interface NodeQuota { text: string; status: NodeStatus; tooltip?: string }`; `deriveNodeQuota(entry: UsageEntry, provider: Provider): NodeQuota | null`.

- [ ] **Step 1: Append the node entry point to `src/lib/quotaUtils.ts`**

Add at the end of the file:

```ts

// --- Route-node entry point. -----------------------------------------------

/** Health status for a route badge. (ChipStatus is NodeStatus | "neutral".) */
export type NodeStatus = "ok" | "warn" | "danger";

/** Quota for a single route node. `null` → caller renders no badge. */
export interface NodeQuota {
  /** "28%" | "CNY 47.37" — bare number, no "剩" prefix (that's chip-only). */
  text: string;
  /** Drives the badge CSS class directly (ok | warn | danger). */
  status: NodeStatus;
  /** Hover detail — same copy as the chip. */
  tooltip?: string;
}

/** Node text: plan is the bare remaining number (no "剩"). */
function textForNode(m: QuotaMath): string {
  return m.billing === "consumption"
    ? `${m.unit} ${m.remaining.toFixed(2)}`
    : `${round1(m.remaining)}%`;
}

/**
 * Quota for a route node. Returns null when there's no usable number
 * (unsupported vendor / query failed / tier or balance missing) → render no badge.
 * Pure; consumes the same core as deriveQuotaChips, so chip and node cannot drift.
 */
export function deriveNodeQuota(entry: UsageEntry, provider: Provider): NodeQuota | null {
  if (!isUsageSupportedVendor(provider.vendor)) return null;
  const snap = entry.snapshot;
  if (!snap) return null;
  const m = quotaMathFor(snap);
  if (!m) return null;
  return { text: textForNode(m), status: statusFor(m), tooltip: tooltipFor(m) };
}
```

- [ ] **Step 2: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors. (`statusFor` returns `"ok" | "warn" | "danger"`, which is assignable to `NodeStatus`; `deriveNodeQuota` is exported but currently unused — that is fine.)

- [ ] **Step 3: Commit**

```bash
git add src/lib/quotaUtils.ts
git commit -m "feat(quota): add deriveNodeQuota entry point for route nodes"
```

---

### Task 3: Switch route badges to `NodeQuota` (RouteLine + Dashboard)

This is one task because `RouteLine`'s prop contract and `Dashboard`'s call site must change together for the app to type-check. The intermediate state (RouteLine edited, Dashboard not yet) will **not** type-check — that is expected; the gate runs at the end.

**Files:**
- Modify: `src/components/RouteLine.vue` (script + template)
- Modify: `src/views/Dashboard.vue` (script + template)

**Interfaces:**
- Consumes: `deriveNodeQuota`, `NodeQuota` (from Task 2).
- Produces: route badges that show remaining % / balance, health-colored, with tooltip — matching the chips.

- [ ] **Step 1: Update `RouteLine.vue` `<script setup>`**

Replace lines 1–57 (the entire `<script setup>` block) with:

```ts
<script setup lang="ts">
import { NSelect, NTooltip, type SelectOption } from "naive-ui";
import { ellipsisLabel } from "../lib/selectLabel";
import type { NodeQuota } from "../lib/quotaUtils";

// Pipeline view of the live route a request takes through the proxy:
//   [路由] profile  ▶  [主用] model (switchable)  ▶  [降级] ↳ model (+N)
// The 主用 node carries a dropdown so the backing model can be switched in place.
// Quota badges show REMAINING (plan %) / balance (consumption), health-colored —
// same number/source/thresholds as the 可用额度 chips above. Styles use --sl-* tokens.
interface FallbackHop {
  provider: string;
  model: string;
  quota?: NodeQuota | null;
}
withDefaults(
  defineProps<{
    profileName?: string;
    /** Primary node quota (remaining % / balance). null → no badge. */
    quota?: NodeQuota | null;
    /** Primary model circuit-breaker cooling down → amber tint + ❄. */
    cooling?: boolean;
    /** Ordered fallback chain (primary's fallback, then its fallback, …). */
    fallbackChain?: FallbackHop[];
    /** Selectable models for the 主用 quick-switch dropdown (label embeds provider). */
    modelOptions?: SelectOption[];
    /** Currently active model id (the dropdown's selected value). */
    switchModelId?: string | null;
    /** Switch request in flight → dropdown shows loading. */
    switching?: boolean;
  }>(),
  {
    cooling: false,
    quota: null,
    fallbackChain: () => [],
    modelOptions: () => [],
    switchModelId: null,
    switching: false,
  },
);

const emit = defineEmits<{ (e: "switch-model", modelId: string): void }>();

function onPickModel(v: string | null) {
  if (v != null) emit("switch-model", v);
}
</script>
```

This drops the `quotaPct` / `usageLabel` props and the `badgeClassFor` function (the badge class is now just `quota.status`).

- [ ] **Step 2: Update `RouteLine.vue` primary badge (was lines 84–85)**

Replace the two badge `<span>` lines inside the 主用 node:

```html
      <span v-if="cooling" class="mark" title="冷却中">❄</span>
      <NTooltip v-if="quota" placement="top">
        <template #trigger>
          <span class="badge" :class="quota.status">{{ quota.text }}</span>
        </template>
        {{ quota.tooltip }}
      </NTooltip>
```

- [ ] **Step 3: Update `RouteLine.vue` overflow list + hop[0] badge (was lines 103–126)**

Replace the inner content of the `+N` `NTooltip` and the trailing hop[0] badge with:

```html
          <div style="display: flex; flex-direction: column; gap: 2px">
            <span
              v-for="(f, i) in fallbackChain"
              :key="i"
              style="font-family: var(--sl-font-mono)"
              >{{ i + 1 }}. {{ f.model }}（{{ f.provider }}）<template v-if="f.quota">
 · {{ f.quota.text }}</template></span
            >
          </div>
        </NTooltip>
        <NTooltip v-if="fallbackChain[0].quota" placement="top">
          <template #trigger>
            <span class="badge" :class="fallbackChain[0].quota.status">{{
              fallbackChain[0].quota.text
            }}</span>
          </template>
          {{ fallbackChain[0].quota.tooltip }}
        </NTooltip>
```

(Leave the surrounding `<NTooltip v-if="fallbackChain.length > 1">` opening tag, the `+N` trigger span, and the `</template>` / `未配置` else-branch untouched.)

- [ ] **Step 4: Update `Dashboard.vue` imports + helpers**

In `src/views/Dashboard.vue`, change the `quotaUtils` import (line 21) to:

```ts
import { deriveQuotaChips, deriveNodeQuota, type ChipStatus, type NodeQuota } from "../lib/quotaUtils";
```

Then **delete** these four functions (keep `backingModelFor` between them):
- `providerUsedPct` (around `:36-40`)
- `providerUsageLabel` (around `:43-51`)
- `quotaPctFor` (around `:56-59`)
- `usageLabelFor` (around `:60-63`)

In their place, add:

```ts
/** Provider → route-node quota (remaining % / balance), or null when no usable number. */
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

- [ ] **Step 5: Update `Dashboard.vue` `fallbackChainFor`**

Replace the function (around `:86-108`) with:

```ts
/** 降级链条：主用模型的 fallback，再 fallback，……（带环检测）。 */
function fallbackChainFor(
  p: Profile | undefined,
): { provider: string; model: string; quota: NodeQuota | null }[] {
  const chain: { provider: string; model: string; quota: NodeQuota | null }[] = [];
  const seen = new Set<string>();
  let cur = backingModelFor(p);
  while (cur) {
    const targetId = config.fallback[cur.id] ?? cur.fallback_target_model_id ?? null;
    if (!targetId || seen.has(targetId)) break;
    seen.add(targetId);
    const target = config.models.find((m) => m.id === targetId);
    if (!target) break;
    const provider = config.providers.find((pr) => pr.id === target.provider_id);
    chain.push({
      provider: provider ? providerLabel(provider) : target.provider_id,
      model: target.upstream_model_id,
      quota: nodeQuotaForProvider(target.provider_id),
    });
    cur = target;
  }
  return chain;
}
```

- [ ] **Step 6: Update `Dashboard.vue` `<RouteLine>` wiring**

In the template (around `:270-282`), replace the `:quota-pct` and `:usage-label` lines with a single `:quota`:

```html
        <RouteLine
          v-for="p in orderedProfiles"
          :key="p.id"
          :profile-name="p.name"
          :quota="nodeQuotaForProfile(p)"
          :cooling="coolingFor(p)"
          :fallback-chain="fallbackChainFor(p)"
          :model-options="modelOptions"
          :switch-model-id="backingModelFor(p)?.id ?? null"
          :switching="switching"
          @switch-model="(modelId) => switchModel(p, modelId)"
        />
```

- [ ] **Step 7: Verify no residual references**

Run: `grep -nE "providerUsedPct|providerUsageLabel|quotaPctFor|usageLabelFor|quotaPct|usageLabel|badgeClassFor" src/views/Dashboard.vue src/components/RouteLine.vue`
Expected: no matches. (Any match is a missed call site — remove it.)

- [ ] **Step 8: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 9: Manual verification**

Run: `npm run tauri dev`. With 智谱 + 火山 + DeepSeek configured, on the Dashboard confirm:
- Each route node badge shows the **same number and color** as its chip directly above (plan: remaining %, e.g. `28%`; consumption: `CNY 47.37`).
- Hovering a plan badge shows `5 小时窗口：可用 …%（已用 …%）` (+ reset time when present).
- A DeepSeek balance `< ¥10` shows amber; `≤ ¥0` shows red (no longer the old forced green).
- A custom / OpenAI-compatible-vendor model shows **no** badge.
- The `+N` fallback overflow lists hops with remaining numbers (`· 28%` / `· CNY 47.37`).

- [ ] **Step 10: Commit**

```bash
git add src/components/RouteLine.vue src/views/Dashboard.vue
git commit -m "feat(ui): route-node badges show remaining/health, unified with quota chips"
```

---

## Self-Review

**Spec coverage:** every spec requirement maps to a task —
- Req 1 (plan remaining %) → Task 2 `textForNode` + Task 3 badge.
- Req 2 (consumption balance colored) → Task 2 `statusFor` (consumption branch) + Task 3 (force-green branch deleted in Step 2/3).
- Req 3 (one data source = `five_hour`/`remaining`) → Task 1 `quotaMathFor`.
- Req 4 (one threshold rule) → Task 1 `statusFor` + `LOW_BALANCE_THRESHOLD`.
- Req 5 (tooltip on every badge) → Task 3 Steps 2 & 3 (`NTooltip`).
- Req 6 (absent data → no badge) → Task 2 `deriveNodeQuota` returns null; Task 3 `v-if="quota"`.
- Req 7 (overflow shows remaining) → Task 3 Step 3.
- Req 8 (cooling orthogonal) → unchanged `cooling` prop, untouched.
- Background #2 (data-source divergence) → Task 1 deletes the `used/total` path implicitly; Task 3 Step 4 deletes `providerUsedPct`.
- Background #3 (force-green bug) → Task 3 Step 2 removes the `usageLabel` force-`ok` span.

**Placeholder scan:** none — every code step contains the exact code.

**Type consistency:** `NodeQuota` (Task 2) is imported in both `RouteLine.vue` and `Dashboard.vue` (Task 3). `statusFor` returns `"ok" | "warn" | "danger"`, assignable to `NodeStatus`. `FallbackHop.quota` (RouteLine) and the chain element's `quota` (Dashboard) are both `NodeQuota | null`. `deriveNodeQuota(entry, provider)` signature matches its callers in `nodeQuotaForProvider`.
