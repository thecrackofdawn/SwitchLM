import { h, type VNodeChild } from "vue";
import { NEllipsis } from "naive-ui";
import type { Provider } from "./types";

/**
 * Wraps a select option's label in naive-ui's NEllipsis: long labels truncate
 * with an ellipsis and reveal the full text on hover. Pass as NSelect's `render-label`
 * so over-long dropdown items show their full name without widening the menu.
 *
 * The hover tooltip is forced onto a single line (white-space: nowrap) so it widens
 * to fit the full text instead of wrapping after a certain length.
 */
export function ellipsisLabel(option: { label?: unknown }): VNodeChild {
  const label = option?.label;
  if (typeof label !== "string") return undefined;
  if (!label) return undefined;
  return h(NEllipsis, null, {
    default: () => label,
    tooltip: () => h("span", { style: { whiteSpace: "nowrap" } }, label),
  });
}

// The provider vendor dropdown presets (label/value), shared by Provider.vue and the label
// helpers. This list is a UI concern — which vendors appear as selectable presets — and is
// intentionally independent of usage eligibility (see USAGE_SUPPORTED_VENDORS): the "其他"
// sentinel is a selectable preset but is NOT usage-eligible. Vendors not listed here can still
// be typed in via the NSelect `tag` mode (free-text; inference only).
// OTHER_VENDOR is the sentinel value for a user-defined ("其他") provider.
export const OTHER_VENDOR = "other";

export const vendorOptions = [
  { label: "智谱", value: "zhipu" },
  { label: "DeepSeek", value: "deepseek" },
  { label: "火山 · agent plan", value: "volcengine-agent" },
  { label: "火山 · coding plan", value: "volcengine-coding" },
  { label: "千问 · token plan", value: "qianwen-token" },
  { label: "其他", value: OTHER_VENDOR },
];

// Display label for a vendor value, falling back to the raw value when unknown.
export function vendorLabel(vendor: string): string {
  return vendorOptions.find((o) => o.value === vendor)?.label ?? vendor;
}

// Label for a provider/account wherever a model is shown bound to one: the account name only.
// A model is always displayed next to the account it belongs to, so the account name is the
// disambiguator — the vendor is NOT appended (it stays visible as a tag in the Provider list
// and 套餐用量). Avoids e.g. "百炼 · token plan（千问 · token plan）".
export function providerLabel(p: Provider): string {
  return p.display_name;
}

// Vendors eligible for a usage chip slot — they may render a real value, a "查询失败", or a
// "不支持" placeholder. This is a POSITIVE list, intentionally independent of `vendorOptions`:
// the dropdown also holds the "其他" sentinel (a selectable preset with no quota to query), which
// is absent here so custom providers get no chip (and no tray usage entry). Distinct from
// hasUsageAdapter: a supported vendor without an adapter renders "不支持", whereas an adapted
// vendor renders real data or "查询失败". Keep in sync with `usage_provider_for` in
// src-tauri/src/usage/mod.rs. Use hasUsageAdapter for the data-availability check; this gates
// only chip visibility/skipping.
const USAGE_SUPPORTED_VENDORS = ["zhipu", "deepseek", "volcengine-agent", "volcengine-coding", "qianwen-token"];
export function isUsageSupportedVendor(vendor?: string | null): boolean {
  return !!vendor && USAGE_SUPPORTED_VENDORS.includes(vendor);
}

// Vendors that have a backend usage adapter (can actually query quota). Distinct from
// isUsageSupportedVendor (USAGE_SUPPORTED_VENDORS): both lists hold the same slugs today, but they
// are intentionally separate — a future vendor could be usage-*supported* (chip renders "不支持")
// before it has an *adapter* (real data). A known vendor that lacks an adapter renders "不支持" for
// a null snapshot, whereas an adapted vendor (all current ones, incl. qianwen-token) renders
// "查询失败". Keep in sync with `usage_provider_for` in src-tauri/src/usage/mod.rs.
const USAGE_ADAPTER_VENDORS = ["zhipu", "deepseek", "volcengine-agent", "volcengine-coding", "qianwen-token"];
export function hasUsageAdapter(vendor?: string | null): boolean {
  return !!vendor && USAGE_ADAPTER_VENDORS.includes(vendor);
}
