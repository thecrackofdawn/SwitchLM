import type { Provider, UsageEntry, UsageSnapshot, UsageTier } from "./types";
import { hasUsageAdapter, isUsageSupportedVendor } from "./selectLabel";

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
  /** Chip body: "剩 8%" | "CNY 48.77" | "无数据" | "查询失败" | "不支持". */
  text: string;
  /** Hover detail. */
  tooltip?: string;
  /** Drives NTag type. */
  status: ChipStatus;
}

const NO_DATA_TOOLTIP = "暂无可解析的用量数据（详见「用量」页）";

function round1(n: number): number {
  return Math.round(n * 10) / 10;
}

function fmtReset(epochSecs: number): string {
  return new Date(epochSecs * 1000).toLocaleString();
}

const PRIMARY_WINDOW_ORDER = ["five_hour", "weekly_limit", "monthly"] as const;

/** plan 主值：5h -> 周 -> 月 首个有值窗口；都没有则 null。 */
function primaryUsed(
  tiers: UsageTier[] | undefined,
): { pct: number; window: string; resetAt: number | null } | null {
  for (const w of PRIMARY_WINDOW_ORDER) {
    const t = tiers?.find((x) => x.window === w);
    if (t?.used_pct != null) {
      return { pct: t.used_pct, window: w, resetAt: t.reset_at ?? null };
    }
  }
  return null;
}

function windowLabel(w?: string): string {
  switch (w) {
    case "five_hour": return "5 小时窗口";
    case "weekly_limit": return "每周窗口";
    case "monthly": return "每月窗口";
    default: return "套餐窗口";
  }
}

// --- Shared core: per-snapshot quota math used by both chips and route nodes. ---

interface QuotaMath {
  billing: "plan" | "consumption";
  remaining: number; // plan: 0-100 remaining %; consumption: balance
  used?: number; // plan only — surfaces in tooltip
  window?: string; // plan only - which window the value came from (tooltip label)
  unit?: string; // consumption only
  resetAt?: number | null;
}

/**
 * Parse a snapshot into the shared quota math. Returns null when no number can be
 * derived (no usable tier for plan, or null remaining for consumption).
 * Plan uses the primary window (5h -> 周 -> 月, first present); consumption reads snapshot.remaining.
 */
function quotaMathFor(snap: UsageSnapshot): QuotaMath | null {
  if (snap.billing_model === "consumption") {
    const remaining = snap.remaining;
    if (remaining == null) return null;
    return { billing: "consumption", remaining, unit: snap.unit };
  }
  const primary = primaryUsed(snap.tiers);
  if (!primary) return null;
  return {
    billing: "plan",
    remaining: Math.max(0, 100 - primary.pct),
    used: primary.pct,
    window: primary.window,
    resetAt: primary.resetAt,
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
  let tip = `${windowLabel(m.window)}：可用 ${round1(m.remaining)}%（已用 ${round1(m.used!)}%）`;
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
 * - plan providers → primary window (5h → 周 → 月) remaining % (status by remaining thresholds)
 * - consumption providers → balance (red at ≤0 / below LOW_BALANCE_THRESHOLD, else green)
 * - unsupported vendor → skipped; query failure → 查询失败; parseable but missing data → 无数据; known vendor without a usage adapter → 不支持
 *
 * Sort: by the shared usage_order (unlisted sink to end); chip color still reflects urgency.
 */
export function deriveQuotaChips(
  usage: UsageEntry[],
  providers: Provider[],
  order: string[],
): QuotaChip[] {
  const chips: QuotaChip[] = [];
  for (const entry of usage) {
    const provider = providers.find((p) => p.id === entry.provider_id);
    if (!provider || !isUsageSupportedVendor(provider.vendor)) continue;

    const label = provider.display_name;
    const snap = entry.snapshot;

    // No snapshot: an adapted vendor's query actually failed → "查询失败"; a known vendor without an
    // adapter → "不支持". Both render a neutral placeholder chip.
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

    const m = quotaMathFor(snap);
    if (!m) {
      chips.push({
        key: entry.provider_id,
        label,
        text: "无数据",
        tooltip: NO_DATA_TOOLTIP,
        status: "neutral",
      });
      continue;
    }

    chips.push({
      key: entry.provider_id,
      label,
      text: textForChip(m),
      tooltip: tooltipFor(m),
      status: statusFor(m),
    });
  }

  // Order by the shared usage_order; unlisted sink to the end (stable — config order preserved).
  const rank = new Map<string, number>();
  order.forEach((id, i) => rank.set(id, i));
  return chips.sort((a, b) => {
    const ia = rank.get(a.key);
    const ib = rank.get(b.key);
    if (ia === undefined && ib === undefined) return 0;
    if (ia === undefined) return 1;
    if (ib === undefined) return -1;
    return ia - ib;
  });
}

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
