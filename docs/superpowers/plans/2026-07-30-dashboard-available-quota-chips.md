# Dashboard Per-Provider Quota Chip Strip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Dashboard's single averaged "当前可用额度" headline with a per-provider chip strip, each provider shown in its own unit (plan → 5h remaining %, consumption → balance).

**Architecture:** A pure function `deriveQuotaChips(usage, providers)` lives in a new `src/lib/quotaUtils.ts` and produces a sorted list of `QuotaChip` (label / text / tooltip / status / sortKey). `Dashboard.vue` calls it from one `computed` and renders one `NTag` per chip (wrapped in `NTooltip`). The old `availableQuotaPct` / `availableStatus` / `.big.*` styles are deleted. Frontend-only — no backend or type changes.

**Tech Stack:** Vue 3 `<script setup lang="ts">`, naive-ui (`NTag` / `NTooltip` / `NSpace` / `NCard`), Pinia stores, `--sl-*` CSS tokens.

**Spec:** `docs/superpowers/specs/2026-07-30-dashboard-available-quota-chips-design.md`

## Global Constraints

- **Frontend-only.** Touch only `src/`. No Rust, no `commands.rs`, no edits to `UsageSnapshot` / `UsageEntry` / `UsageTier` in `src/lib/types.ts` — every field the logic needs already exists.
- **No frontend test runner exists** (`package.json` has only `dev`/`build`/`preview`/`tauri`; no Vitest/Jest, zero `*.test.ts`). **Do NOT add one** — out of scope. Verification per task = `npm run build` (which runs `vue-tsc --noEmit && vite build`, catching type *and* `.vue` template errors) + the manual UI checks written into the steps. `deriveQuotaChips` is kept pure so unit tests are a cheap add later.
- **TypeScript mirrors Rust structs** (snake_case, no rename_all). Keep `UsageEntry` / `UsageSnapshot` field names exactly as in `src/lib/types.ts`.
- **Match existing style:** Vue 3 `<script setup lang="ts">`, naive-ui components, `--sl-*` CSS custom properties, 2-space indent, double quotes.
- **Commit on a feature branch**, not `main`. Each task ends with a commit of only the files that task touched.
- The chip metric is **remaining** (e.g. `剩 8%`), not used — this is intentional and differs from the route badge's used%; do not "fix" the mismatch (hover reconciles it).

---

## File Structure

- **Create `src/lib/quotaUtils.ts`** — pure module: `LOW_BALANCE_THRESHOLD`, `ChipStatus`, `QuotaChip`, `deriveQuotaChips(usage, providers)`. No Vue, no store imports; depends only on `src/lib/types.ts` (types) and `src/lib/selectLabel.ts` (`isUsageSupportedVendor`).
- **Modify `src/lib/selectLabel.ts`** — add `isUsageSupportedVendor(vendor)`. One small exported predicate; everything else in the file is unchanged.
- **Modify `src/views/Dashboard.vue`** — import `NTooltip` + `deriveQuotaChips`/`ChipStatus`; replace the `availableQuotaPct`/`availableStatus` computed with `quotaChips` + `tagType`; swap the right grid cell's template; drop `.big.*` styles, add `.quota*` styles. The route section, `providerUsedPct` / `providerUsageLabel` / `providerLabel` / `NStatistic` are all **kept** (still used elsewhere on the page).

---

## Task 1: Pure `deriveQuotaChips` module + `isUsageSupportedVendor`

**Files:**
- Modify: `src/lib/selectLabel.ts` (append one export at end of file)
- Create: `src/lib/quotaUtils.ts`

**Interfaces:**
- Consumes: `vendorOptions` (already in `selectLabel.ts:26`); types `Provider`, `UsageEntry` from `src/lib/types.ts`.
- Produces (used by Task 2):
  - `isUsageSupportedVendor(vendor?: string | null): boolean` — in `selectLabel.ts`
  - `deriveQuotaChips(usage: UsageEntry[], providers: Provider[]): QuotaChip[]` — in `quotaUtils.ts`
  - `export type ChipStatus = "ok" | "warn" | "danger" | "neutral"`
  - `export interface QuotaChip { key: string; label: string; text: string; tooltip?: string; status: ChipStatus; sortKey: number }`
  - `export const LOW_BALANCE_THRESHOLD = 5.0`

- [ ] **Step 1: Add `isUsageSupportedVendor` to `selectLabel.ts`**

Append at the end of `src/lib/selectLabel.ts` (after the existing `providerLabel` function). It uses `vendorOptions` already defined at the top of that file.

```ts
// True when the vendor has a usage adapter (the vendorOptions set); false for custom/inference-only.
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return !!vendor && vendorOptions.some((o) => o.value === vendor);
}
```

- [ ] **Step 2: Create `src/lib/quotaUtils.ts`**

Create the file with exactly this content:

```ts
import type { Provider, UsageEntry } from "./types";
import { isUsageSupportedVendor } from "./selectLabel";

// Low-balance warn floor for consumption providers, in the same units as `snapshot.remaining`.
// Currency-specific (CNY today) — revisit if a non-CNY consumption vendor is added.
export const LOW_BALANCE_THRESHOLD = 5.0;

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

/**
 * Build the Dashboard's per-provider quota chips. Pure & order-stable.
 *
 * - plan providers → five_hour window remaining % (status by remaining thresholds)
 * - consumption providers → balance (neutral; red only at ≤0 / below LOW_BALANCE_THRESHOLD)
 * - unsupported vendor → skipped; query failure → 查询失败; parseable but missing tier → 无数据
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

    // Consumption billing (e.g. DeepSeek): show balance. Keys off billing_model, never vendor.
    if (snap.billing_model === "consumption") {
      const remaining = snap.remaining;
      if (remaining == null) {
        chips.push({
          key: entry.provider_id,
          label,
          text: "无数据",
          tooltip: NO_DATA_TOOLTIP,
          status: "neutral",
          sortKey: 200,
        });
      } else {
        const status: ChipStatus =
          remaining <= 0 ? "danger" : remaining < LOW_BALANCE_THRESHOLD ? "warn" : "neutral";
        chips.push({
          key: entry.provider_id,
          label,
          text: `${snap.unit} ${remaining.toFixed(2)}`,
          tooltip: "按量付费余额（耗尽停机）",
          status,
          sortKey: 200,
        });
      }
      continue;
    }

    // Plan billing: five_hour window remaining %.
    const fiveHour = snap.tiers?.find((t) => t.window === "five_hour");
    const used = fiveHour?.used_pct;
    if (used == null) {
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
    const remaining = Math.max(0, 100 - used);
    const status: ChipStatus = remaining < 10 ? "danger" : remaining < 30 ? "warn" : "ok";
    let tooltip = `5 小时窗口：剩余 ${round1(remaining)}%（已用 ${round1(used)}%）`;
    const reset = fiveHour?.reset_at;
    if (reset != null) tooltip += `，重置于 ${fmtReset(reset)}`;
    chips.push({
      key: entry.provider_id,
      label,
      text: `剩 ${round1(remaining)}%`,
      tooltip,
      status,
      sortKey: remaining,
    });
  }

  // Array#sort is stable in modern engines → most urgent first, config order preserved within a group.
  return chips.sort((a, b) => a.sortKey - b.sortKey);
}
```

- [ ] **Step 3: Type-check + build**

Run: `npm run build`
Expected: succeeds with no errors. (`vue-tsc --noEmit` verifies the new module type-checks against `types.ts`; `vite build` confirms it imports cleanly.)

If `vue-tsc` complains that `UsageEntry.error` is possibly `null`: the `?? undefined` already coerces it — re-check the `tooltip: entry.error ?? undefined` line is present. `QuotaChip.tooltip` is `string | undefined`, and `string | null` is not assignable, which is exactly why the `?? undefined` is there.

- [ ] **Step 4: Commit**

```bash
git add src/lib/selectLabel.ts src/lib/quotaUtils.ts
git commit -m "feat(dashboard): extract per-provider quota chip derivation to quotaUtils"
```

---

## Task 2: Wire the chip strip into Dashboard, remove the averaged headline

**Files:**
- Modify: `src/views/Dashboard.vue` (script imports, replace two computed with `quotaChips`/`tagType`, replace the right `<NGi>` template, swap `.big.*` styles for `.quota*`)

**Interfaces:**
- Consumes (from Task 1): `deriveQuotaChips(usage, providers)`, `type ChipStatus`.
- Produces: the rendered "可用额度" chip strip in the Dashboard overview.

- [ ] **Step 1: Add imports**

In `src/views/Dashboard.vue`, add `NTooltip` to the naive-ui import block (keep `NStatistic`, `NTag`, etc. — all still used):

```ts
import {
  NAlert,
  NButton,
  NCard,
  NEmpty,
  NGi,
  NGrid,
  NSpace,
  NStatistic,
  NTag,
  NTooltip,
  useMessage,
} from "naive-ui";
```

Add the quotaUtils import next to the other `../lib` imports (after the `selectLabel` import line):

```ts
import { deriveQuotaChips, type ChipStatus } from "../lib/quotaUtils";
```

- [ ] **Step 2: Replace the two computed with `quotaChips` + `tagType`**

Delete the `availableQuotaPct` computed and the `availableStatus` computed (the block currently around lines 62–79, starting with the comment `/** 当前可用额度：…平均值… */` and ending after the `availableStatus` closing `});`). Replace that whole block with:

```ts
/** 概览「可用额度」标签条：每家服务商一个 chip——plan 显示 5h 剩余%、消费显示余额。
 *  推导逻辑在 lib/quotaUtils（pure），此处只做响应式包装。 */
const quotaChips = computed(() => deriveQuotaChips(runtime.usage, config.providers));

/** chip 状态 → NTag type。 */
function tagType(s: ChipStatus): "success" | "warning" | "error" | "default" {
  switch (s) {
    case "ok":
      return "success";
    case "warn":
      return "warning";
    case "danger":
      return "error";
    default:
      return "default";
  }
}
```

Leave `providerUsedPct`, `providerUsageLabel`, `quotaPctFor`, `usageLabelFor` untouched — the route section below still uses them.

- [ ] **Step 3: Replace the right grid-cell template**

Find the second `<NGi>` in the template (the one containing `<NStatistic label="当前可用额度">` with the `<span class="mono big" …>`). Replace that entire `<NGi>…</NGi>` with:

```html
      <NGi>
        <NCard>
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

Keep the first `<NGi>` (运行状态) exactly as-is.

- [ ] **Step 4: Swap styles — remove `.big.*`, add `.quota*`**

In the `<style scoped>` block, delete these four rules:

```css
.big {
  font-size: 22px;
  font-weight: 600;
  color: var(--sl-ink);
}
.big.ok {
  color: var(--sl-ok);
}
.big.warn {
  color: var(--sl-cool);
}
.big.danger {
  color: var(--sl-trip);
}
```

Add (the `.mono` rule already exists — do not duplicate it):

```css
.quota__title {
  display: block;
  font-size: 13px;
  color: var(--sl-text-2);
  margin-bottom: 8px;
}
.quota__empty {
  font-size: 16px;
  color: var(--sl-text-3);
}
```

- [ ] **Step 5: Type-check + build**

Run: `npm run build`
Expected: succeeds. This catches: missing `NTooltip` import, any leftover reference to the deleted `availableQuotaPct`/`availableStatus`, template compile errors, and the `.big` class no longer being referenced (unused CSS is fine, but a still-referenced deleted class would be a template error only if used — it isn't after Step 3).

- [ ] **Step 6: Manual verification**

Run the app in dev mode (e.g. `npm run tauri dev`) and open the 概览 (Dashboard) page. With at least one 智谱/火山 provider and one DeepSeek provider configured:

- [ ] The right cell now reads "可用额度" with one chip per usage-supported provider (no chip for any custom/OpenAI-compatible vendor).
- [ ] A plan provider chip shows `剩 N%` (remaining); a DeepSeek chip shows `CNY N.NN`.
- [ ] The most-used plan provider (lowest remaining) is first; its chip is red when remaining < 10%, amber 10–29%, green ≥ 30%.
- [ ] Hovering a plan chip shows `5 小时窗口：剩余 X%（已用 Y%）` (+ reset time when available) — X and Y sum to ~100.
- [ ] Hovering a `查询失败` chip shows the backend error string. (To force one: temporarily set an invalid inference key on a supported provider, refresh, then revert.)
- [ ] Set a DeepSeek balance to ≤ 0 (or mock) → chip is red; `0 < balance < 5` → amber; otherwise neutral/default.
- [ ] The 运行状态 cell (left) and the route section below are unchanged.

- [ ] **Step 7: Commit**

```bash
git add src/views/Dashboard.vue
git commit -m "feat(dashboard): replace averaged quota headline with per-provider chips"
```

---

## Notes for the implementer

- `get_all_usage` (`commands.rs:359`) returns one `UsageEntry` per configured provider — supported vendors carry a `snapshot`; unsupported/errored ones carry `snapshot: null` + `error`. That's why the loop handles both `!snap` and missing tiers.
- `provider.display_name` is unique within a vendor by construction (the add flow auto-suffixes ` 2`, ` 3` and enforces `(vendor, display_name)` uniqueness), so the chip needs no vendor suffix to distinguish accounts.
- If `npm run build` is slow, `npx vue-tsc --noEmit` alone gives the type gate faster, but run the full `npm run build` before committing since it also validates the `.vue` template.
