<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { NButton, NCard, NEmpty, NProgress, NSpace, NTag, NTooltip, useMessage } from "naive-ui";
import { useConfigStore } from "../stores/config";
import { useRuntimeStore } from "../stores/runtime";
import { useSystemStore } from "../stores/system";
import { DEFAULT_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, type ProviderStats, type StatWindow, type UsageEntry } from "../lib/types";
import draggable from "vuedraggable";
import { usePolling } from "../lib/usePolling";
import { vendorLabel } from "../lib/selectLabel";
import { getProviderUsageUrl } from "../lib/commands";
import { openUrl } from '@tauri-apps/plugin-opener';

const msg = useMessage();
const runtime = useRuntimeStore();
const config = useConfigStore();
const system = useSystemStore();

// 三个固定窗口列（顺序即展示顺序），按 tier.window 匹配；该窗口缺数据 -> 显示 N/A。
const windows = [
  { key: "five_hour", label: "5小时" },
  { key: "weekly_limit", label: "1周" },
  { key: "monthly", label: "1月" },
] as const;

interface TierRow {
  key: string;   // "five_hour" | "weekly_limit" | "monthly" — joins to ProviderStats
  label: string;
  pct: number | null;
  reset: number | null;
}

interface UsageCard extends UsageEntry {
  name: string;
  vendor: string | null;
  rows: TierRow[];
  usageUrl?: string;
}

const cards = computed<UsageCard[]>(() =>
  runtime.usage.map((entry) => {
    const provider = config.providers.find((p) => p.id === entry.provider_id);
    return {
      ...entry,
      name: provider ? provider.display_name : entry.provider_id,
      vendor: provider?.vendor ?? null,
      rows: windows.map((w) => {
        const t = entry.snapshot?.tiers?.find((x) => x.window === w.key);
        return { key: w.key, label: w.label, pct: t?.used_pct ?? null, reset: t?.reset_at ?? null };
      }),
    };
  }),
);

/** 窗口 key(与后端 tiers 一致)→ stats 字段。 */
function statsWindow(stats: ProviderStats | undefined, key: string): StatWindow | null {
  if (!stats) return null;
  const w = key === "five_hour" ? stats.last_5h : key === "weekly_limit" ? stats.last_1w : stats.last_1m;
  return w.is_active ? w : null;
}

function fmtNum(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "K";
  return String(n);
}

function findStats(providerId: string): ProviderStats | undefined {
  return runtime.stats.find((s) => s.provider_id === providerId);
}

// Config-backed drag order (single source of truth shared with tray + 概览). Local `ordered`
// updates instantly for a responsive drag; persistence is debounced to coalesce rapid reorder
// into one backend save + tray rebuild.
const ordered = ref<UsageCard[]>([]);
// True while a drag is in progress so a usage refresh landing mid-drag can't yank the list.
const dragging = ref(false);
function sortByUsageOrder(items: UsageCard[]): UsageCard[] {
  const rank = new Map<string, number>();
  config.usageOrder.forEach((id, i) => rank.set(id, i));
  return [...items].sort((a, b) => {
    const ia = rank.get(a.provider_id);
    const ib = rank.get(b.provider_id);
    if (ia === undefined && ib === undefined) return 0;
    if (ia === undefined) return 1; // unlisted sink to end
    if (ib === undefined) return -1;
    return ia - ib;
  });
}
watch(
  () => [cards.value, config.usageOrder] as const,
  () => {
    if (!dragging.value) ordered.value = sortByUsageOrder(cards.value);
  },
  { deep: true, immediate: true },
);
let persistTimer: ReturnType<typeof setTimeout> | null = null;
function commit() {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    config
      .setUsageOrder(ordered.value.map((c) => c.provider_id))
      .catch((e) => msg.error(`顺序保存失败：${String(e)}`));
    persistTimer = null;
  }, 400);
}
function onDragEnd() {
  dragging.value = false;
  commit();
}

// Load usage URLs for each provider
async function loadUsageUrls() {
  for (const card of ordered.value) {
    try {
      const url = await getProviderUsageUrl(card.provider_id);
      if (url) {
        card.usageUrl = url;
      }
    } catch (e) {
      // Silently fail if URL lookup fails - non-critical feature
      console.warn(`Failed to load usage URL for ${card.provider_id}:`, e);
    }
  }
}

// Open external URL in default browser
async function openExternalUrl(url: string) {
  try {
    await openUrl(url);
  } catch (e) {
    msg.error(`无法打开链接：${String(e)}`);
  }
}

function fmtPct(p: number): number {
  return Math.round(p * 10) / 10;
}
function fmtBalance(n: number): string {
  return n.toFixed(2);
}
function status(p: number): "success" | "warning" | "error" {
  return p >= 90 ? "error" : p >= 70 ? "warning" : "success";
}
/** 重置时间的完整精确表示（悬浮展示用），如 2026/8/15 16:10:09。 */
function resetFull(at: number | null): string {
  if (at == null) return "-";
  const d = new Date(at * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
/** 重置时间的动态/相对表示：
 *  < 24h → "2小时10分后重置"；> 1天 → "8月17日重置"；过期 → "已重置"。
 *  依赖 `now` ref 每分钟重算（见下方 nowTick）。 */
function resetRelative(at: number | null, now: number): string {
  if (at == null) return "-";
  const diffSec = Math.floor(at - now / 1000);
  if (diffSec <= 0) return "已重置";
  if (diffSec < 60) return `${diffSec}秒后重置`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}分后重置`;
  if (diffSec < 86_400) {
    const h = Math.floor(diffSec / 3600);
    const m = Math.floor((diffSec % 3600) / 60);
    return m > 0 ? `${h}小时${m}分后重置` : `${h}小时后重置`;
  }
  const d = new Date(at * 1000);
  return `${d.getMonth() + 1}月${d.getDate()}日重置`;
}
/** 当前时间戳（ms），每分钟跳动一次，驱动 resetRelative 重算。 */
const now = ref(Date.now());
let nowTimer: ReturnType<typeof setInterval> | null = null;
onMounted(() => {
  nowTimer = setInterval(() => (now.value = Date.now()), 60_000);
});
onUnmounted(() => {
  if (nowTimer) clearInterval(nowTimer);
});
/** 格式化套餐起始时间：ISO "2026-07-30T00:00:00+08:00" -> "2026-07-30 00:00"
 *  （截取前 16 字符、T 换空格，保留原时区表示，避免按本地时区偏移）。 */
function fmtPlanTime(iso?: string | null): string {
  if (!iso) return "-";
  return iso.slice(0, 16).replace("T", " ");
}
/** 格式化套餐有效期：start ~ end；任一缺失只显示存在的那个，都缺失返回 "-"。 */
function fmtPlanRange(start?: string | null, end?: string | null): string {
  const s = fmtPlanTime(start);
  const e = fmtPlanTime(end);
  if (s === "-" && e === "-") return "-";
  if (s === "-") return e;
  if (e === "-") return s;
  return `${s} ~ ${e}`;
}

async function refresh() {
  try {
    await runtime.refresh();
  } catch (e) {
    msg.error(`刷新失败：${String(e)}`);
  }
}

/** Effective auto-refresh interval shown in the UI (clamped to the 30s floor). */
const refreshSecs = computed(() =>
  Math.max(MIN_USAGE_REFRESH_SECS, system.settings?.usage_refresh_interval_secs ?? DEFAULT_USAGE_REFRESH_SECS),
);

onMounted(async () => {
  await Promise.all([config.loadAll(), runtime.refresh(), system.loadSettings()]);
  // Load usage URLs after initial data is loaded
  await loadUsageUrls();
});

// Watch for changes in ordered cards and reload URLs
watch(ordered, () => {
  loadUsageUrls();
}, { deep: true });

// Auto-refresh usage while the user stays on this page, at the configured interval
// (read from settings; clamped to the 30s floor both here and in the backend).
usePolling(() => runtime.refresh(), () => refreshSecs.value * 1000);
</script>

<template>
  <NSpace vertical :size="12">
    <NSpace justify="space-between" align="center">
      <span class="muted">套餐用量</span>
      <NTooltip>
        <template #trigger>
          <NSpace align="center" :size="6" :wrap="false">
            <span class="auto-refresh-hint" title="">每 {{ refreshSecs }}s 自动刷新</span>
            <NButton size="small" :loading="runtime.loading" @click="refresh">刷新</NButton>
          </NSpace>
        </template>
        <div>用量数据与统计数据每 {{ refreshSecs }} 秒自动刷新一次</div>
      </NTooltip>
    </NSpace>

    <draggable v-model="ordered" item-key="provider_id" class="drag-list" handle=".drag-handle" :animation="150" @start="dragging = true" @end="onDragEnd">
      <template #item="{ element: c }">
        <NCard size="small">
          <NSpace vertical :size="8">
            <NSpace align="center" justify="space-between">
              <NSpace align="center" :size="8">
                <span class="drag-handle" title="拖动排序">⠿</span>
                <!-- Make provider name clickable if usage URL is available -->
                <span
                  v-if="c.usageUrl"
                  class="name clickable"
                  @click="openExternalUrl(c.usageUrl)"
                  title="点击跳转到官方用量页面"
                >{{ c.name }}</span>
                <span v-else class="name">{{ c.name }}</span>
                <NTag v-if="c.vendor" size="small" type="info">{{ vendorLabel(c.vendor) }}</NTag>
                <NTooltip v-if="c.snapshot?.plan && c.snapshot.plan_info" placement="top">
                  <template #trigger>
                    <NTag size="small" type="info">{{ c.snapshot.plan }}</NTag>
                  </template>
                  <div style="display: flex; flex-direction: column; gap: 2px">
                    <span v-if="c.snapshot.plan_info.start_time || c.snapshot.plan_info.end_time"
                      >有效期：{{ fmtPlanRange(c.snapshot.plan_info.start_time, c.snapshot.plan_info.end_time) }}</span
                    >
                    <span v-if="c.snapshot.plan_info.auto_renew != null"
                      >自动续费：{{ c.snapshot.plan_info.auto_renew ? "已开启" : "未开启" }}</span
                    >
                  </div>
                </NTooltip>
                <NTag v-else-if="c.snapshot?.plan" size="small" type="info">{{ c.snapshot.plan }}</NTag>
              </NSpace>
              <NTag v-if="c.error" type="error" size="small">{{ c.error }}</NTag>
            </NSpace>

        <!-- Consumption billing (e.g. DeepSeek): show balance only -->
        <template v-if="c.snapshot">
          <template v-if="c.snapshot.billing_model === 'consumption'">
            <div class="balance-row">
              <span class="balance-row__label">余额</span>
              <span class="balance-row__value mono">{{ c.snapshot.unit }} {{ fmtBalance(c.snapshot.remaining ?? 0) }}</span>
            </div>
            <div v-if="findStats(c.provider_id)" class="balance-row">
              <span class="balance-row__label">累计消耗</span>
              <NTooltip>
                <template #trigger>
                  <span class="balance-row__value mono">
                    {{ fmtNum(findStats(c.provider_id)!.total_tokens) }}
                    <span class="tier__stat-unit">token</span>
                  </span>
                </template>
                <div style="display: flex; flex-direction: column; gap: 2px">
                  <span>输入：{{ fmtNum(findStats(c.provider_id)!.total_input_tokens) }} token</span>
                  <span>输出：{{ fmtNum(findStats(c.provider_id)!.total_output_tokens) }} token</span>
                  <span>请求：{{ fmtNum(findStats(c.provider_id)!.total_requests) }} 次</span>
                </div>
              </NTooltip>
            </div>
          </template>

          <!-- Plan billing (default): tier windows with progress bars -->
          <template v-else>
            <div v-for="row in c.rows" :key="row.label" class="tier">
              <span class="tier__label">{{ row.label }}</span>
              <template v-if="row.pct != null">
                <span class="tier__pct mono">{{ fmtPct(row.pct) }}%</span>
                <NProgress
                  class="tier__bar tier__bar--short"
                  :percentage="fmtPct(row.pct)"
                  :status="status(row.pct)"
                  :show-indicator="false"
                />
                <!-- 本机统计:token 消耗(悬浮显示输入/输出拆分) + 请求次数 -->
                <template v-if="statsWindow(findStats(c.provider_id), row.key)">
                  <NTooltip>
                    <template #trigger>
                      <span class="tier__stat mono" title="token 消耗（本机统计）">
                        {{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.input_tokens +
                                  statsWindow(findStats(c.provider_id), row.key)!.output_tokens) }}
                        <span class="tier__stat-unit">token</span>
                      </span>
                    </template>
                    <div style="display: flex; flex-direction: column; gap: 2px">
                      <div>本窗口期内请求token数</div>
                      <span>输入：{{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.input_tokens) }} token</span>
                      <span>输出：{{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.output_tokens) }} token</span>
                    </div>
                  </NTooltip>
                  <NTooltip>
                    <template #trigger>
                      <span class="tier__stat mono" title="请求次数（本机统计）">
                        {{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.requests) }}
                        <span class="tier__stat-unit">次</span>
                      </span>
                    </template>
                    <div>本窗口期成功请求次数</div>
                  </NTooltip>
                </template>
                <NTooltip v-if="row.reset != null">
                  <template #trigger>
                    <span class="tier__reset mono">{{ resetRelative(row.reset, now) }}</span>
                  </template>
                  <div>重置时间：{{ resetFull(row.reset) }}</div>
                </NTooltip>
                <span v-else class="tier__reset mono">重置时间 -</span>
              </template>
              <span v-else class="tier__na">N/A</span>
            </div>
          </template>
        </template>

        <div v-if="c.snapshot?.raw_summary && c.snapshot?.billing_model !== 'consumption'" class="mono raw">
          原始返回：{{ c.snapshot.raw_summary }}
        </div>

        <div v-if="findStats(c.provider_id)" class="lifetime mono">
          累计消耗：{{ fmtNum(findStats(c.provider_id)!.total_tokens) }} token ·
          {{ fmtNum(findStats(c.provider_id)!.total_requests) }} 次请求
        </div>
      </NSpace>
    </NCard>
      </template>
    </draggable>

    <NEmpty v-if="!ordered.length" description="还没有服务商配置或用量数据" />
  </NSpace>
</template>

<style scoped>
.name {
  font-weight: 600;
}
.name.clickable {
  cursor: pointer;
  text-decoration: none;
  transition: all 0.2s;
}
.name.clickable:hover {
  text-decoration: underline;
  text-decoration-color: var(--sl-color-accent-6);
  text-decoration-thickness: 2px;
  text-underline-offset: 2px;
  color: var(--sl-color-accent-7);
}
.mono {
  font-family: var(--sl-font-mono);
}
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
.auto-refresh-hint {
  color: var(--sl-text-3);
  font-size: 12px;
  white-space: nowrap;
}
.raw {
  font-size: 12px;
  color: var(--sl-text-3);
  word-break: break-all;
}
.tier {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 13px;
}
.tier__label {
  width: 42px;
  flex-shrink: 0;
  color: var(--sl-text-2);
}
.tier__pct {
  width: 48px;
  flex-shrink: 0;
  text-align: right;
  font-weight: 600;
}
.tier__bar {
  flex: 1 1 auto;
  min-width: 60px;
}
.tier__bar--short {
  flex: 1 1 auto; /* fill the space left over by the fixed-width stats + reset time */
  min-width: 120px;
}
.tier__stat {
  flex-shrink: 0;
  font-size: 12px;
  color: var(--sl-text-2);
  white-space: nowrap;
}
.tier__stat-unit {
  font-size: 11px;
  color: var(--sl-text-3);
}
.tier__reset {
  flex-shrink: 0;
  font-size: 12px;
  color: var(--sl-text-3);
  white-space: nowrap;
}
.tier__na {
  color: var(--sl-text-3);
}
.balance-row {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 14px;
}
.balance-row__label {
  width: 64px;
  flex-shrink: 0;
  color: var(--sl-text-2);
}
.balance-row__value {
  font-weight: 600;
}
.lifetime {
  font-size: 12px;
  color: var(--sl-text-3);
}
</style>
