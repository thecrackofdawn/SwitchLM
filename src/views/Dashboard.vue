<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
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
import RouteLine from "../components/RouteLine.vue";
import { fitWindowToRoutes } from "../lib/fitRouteWindow";
import { useConfigStore } from "../stores/config";
import { usePolling } from "../lib/usePolling";
import { providerLabel } from "../lib/selectLabel";
import { deriveQuotaChips, deriveNodeQuota, type ChipStatus, type NodeQuota } from "../lib/quotaUtils";
import { useRuntimeStore } from "../stores/runtime";
import { useSystemStore } from "../stores/system";
import { DEFAULT_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, type Model, type Profile } from "../lib/types";

const msg = useMessage();
const system = useSystemStore();
const config = useConfigStore();
const runtime = useRuntimeStore();

/** 概览展示全部路由：以下按 profile 计算各自的 当前生效模型 / 用量 / 熔断。
 *  Strategy-aware effective model: prefers the time-based effective_model_id,
 *  falls back to the profile's static backing_model_id. 故障转移节点展示后端解析的
 *  时间感知有效转移目标（effective_fallback_model_id），由策略选中时附加 ⏰；
 *  无转移目标时节点隐藏。 */
function effectiveModelFor(p: Profile | undefined): Model | undefined {
  if (!p) return undefined;
  const eff = config.routeEffective.find((e) => e.profile_id === p.id);
  return config.models.find((m) => m.id === (eff?.effective_model_id ?? p.backing_model_id));
}
/** Human-readable label for a model node: upstream id + provider (fullwidth parens). */
function modelLabelOf(m: Model | undefined): string {
  if (!m) return "-";
  const provider = config.providers.find((p) => p.id === m.provider_id);
  return provider ? `${m.upstream_model_id}（${providerLabel(provider)}）` : m.upstream_model_id;
}
/** Route 的有效故障转移信息（时间感知，来自 routeEffective）。 */
function effectiveFallbackOf(p: Profile | undefined) {
  if (!p) return undefined;
  return config.routeEffective.find((e) => e.profile_id === p.id);
}
/** 故障转移模型标签：解析 effective_fallback_model_id → 模型标签；
 *  无转移目标时返回空串（RouteLine 的故障转移节点随之隐藏）。 */
function failoverLabelOf(p: Profile | undefined): string {
  const id = effectiveFallbackOf(p)?.effective_fallback_model_id;
  if (!id) return "";
  return modelLabelOf(config.models.find((m) => m.id === id));
}
/** Provider → route-node quota (remaining % / balance), or null when no usable number. */
function nodeQuotaForProvider(providerId: string): NodeQuota | null {
  const entry = runtime.usage.find((u) => u.provider_id === providerId);
  const provider = config.providers.find((p) => p.id === providerId);
  return entry && provider ? deriveNodeQuota(entry, provider) : null;
}
function nodeQuotaForProfile(p: Profile | undefined): NodeQuota | null {
  const m = effectiveModelFor(p);
  return m ? nodeQuotaForProvider(m.provider_id) : null;
}
/** 概览「可用额度」标签条：每家服务商一个 chip——plan 显示 5h 可用%、消费显示余额。
 *  推导逻辑在 lib/quotaUtils（pure），此处只做响应式包装。 */
const quotaChips = computed(() => deriveQuotaChips(runtime.usage, config.providers, config.usageOrder));

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
function coolingFor(p: Profile | undefined): boolean {
  const m = effectiveModelFor(p);
  return m ? (runtime.health[m.id]?.cooling_down ?? false) : false;
}

/** 后台自动恢复轮询句柄；存在 bindError 时启动，恢复后清除。 */
const refreshInterval = ref<number | null>(null);

async function copyUrl(label: string, field: "anthropic_base_url" | "openai_base_url") {
  try {
    await system.loadEnvSnippet();
  } catch {
    // fall back to whatever is currently displayed
  }
  const url = system.envSnippet?.[field];
  if (!url) {
    msg.warning("代理尚未就绪，无法获取地址");
    return;
  }
  try {
    await navigator.clipboard.writeText(url);
    msg.success(`已复制${label}地址`);
  } catch (e) {
    msg.error(`复制失败：${String(e)}`);
  }
}

onMounted(async () => {
  await Promise.all([
    config.loadAll(),
    system.refresh(),
    runtime.refresh(),
    system.loadEnvSnippet(),
    system.loadSettings(),
  ]);

  // One-shot startup fit: grow the window so the widest route pipeline doesn't wrap.
  // Runs only once per session (module-level guard inside fitWindowToRoutes).
  await nextTick(); // pipelines + quota badges are painted
  await fitWindowToRoutes(
    Array.from(document.querySelectorAll<HTMLElement>(".route-card .pipeline")),
  );

  // If there's a binding error, start periodic refresh (every 5s)
  // to detect backend auto-recovery.
  if (system.bindError) {
    refreshInterval.value = window.setInterval(async () => {
      await system.refresh();
      // Clear interval when error is gone (recovery succeeded).
      if (!system.bindError) {
        // Recovery succeeded: refresh the env snippet (with the new actual
        // port) so the displayed service URLs are no longer stale.
        try {
          await system.loadEnvSnippet();
        } catch {
          // env snippet unavailable (e.g. proxy not yet ready); ignore --
          // the UI falls back to the "…" placeholder.
        }
        if (refreshInterval.value) {
          clearInterval(refreshInterval.value);
          refreshInterval.value = null;
        }
      }
    }, 5000);
  }
});

onUnmounted(() => {
  if (refreshInterval.value) {
    clearInterval(refreshInterval.value);
    refreshInterval.value = null;
  }
});

// Auto-refresh usage + effective models while the user stays on this page
// (drives "可用额度", per-route quota, and the strategy-aware effective model),
// at the configured interval (clamped to the 30s floor).
usePolling(
  async () => { await runtime.refresh(); await config.refreshEffective(); },
  () =>
    Math.max(
      MIN_USAGE_REFRESH_SECS,
      system.settings?.usage_refresh_interval_secs ?? DEFAULT_USAGE_REFRESH_SECS,
    ) * 1000,
);

/** 概览展示门槛：添加了服务商即展示（可用额度等服务随之可见）；无服务商时退回空状态引导。 */
const hasProviders = computed(() => config.providers.length > 0);
/** 路由（Profile）是否已配置--驱动「路由」卡片的未配置提示与运行状态告警。 */
const hasRoutes = computed(() => config.profiles.length > 0);
</script>

<template>
  <NSpace vertical :size="16">
    <!-- Port binding error alert -->
    <NAlert v-if="system.bindError" type="error" :show-icon="true" closable>
      {{ system.bindError }}
    </NAlert>

    <NGrid v-if="hasProviders" :cols="3" :x-gap="16" :y-gap="16" responsive="screen">
      <NGi>
        <NCard>
          <NStatistic label="运行状态">
            <template v-if="system.bindError">
              <!-- Display when port is occupied -->
              <NTag type="error" round size="small">
                端口被占用
              </NTag>
              <div class="error-detail">
                {{ system.bindError }}
              </div>
            </template>
            <template v-else-if="!hasRoutes">
              <!-- No routes configured: the proxy has nothing to forward to. -->
              <NTag type="error" round size="small">
                未配置路由
              </NTag>
              <div class="error-detail">
                尚未配置任何路由，代理无法转发请求。
              </div>
            </template>
            <template v-else>
              <!-- Normal status display -->
              <NTag :type="system.status?.running ? 'success' : 'default'" round size="small">
                {{ system.status?.running ? "运行中" : "未运行" }}
              </NTag>
            </template>
          </NStatistic>
        </NCard>
      </NGi>
      <NGi :span="2">
        <NCard>
          <span class="info-title">服务地址</span>
          <div class="addr-list">
            <div class="addr-row">
              <span class="addr-label">Anthropic协议：</span>
              <span class="mono addr-url">{{ system.envSnippet?.anthropic_base_url ?? "…" }}</span>
              <NButton size="tiny" @click="copyUrl('Anthropic', 'anthropic_base_url')">复制</NButton>
            </div>
            <div class="addr-row">
              <span class="addr-label">OpenAI协议：</span>
              <span class="mono addr-url">{{ system.envSnippet?.openai_base_url ?? "…" }}</span>
              <NButton size="tiny" @click="copyUrl('OpenAI', 'openai_base_url')">复制</NButton>
            </div>
          </div>
        </NCard>
      </NGi>
    </NGrid>

    <NCard v-if="hasProviders" title="可用额度">
      <NSpace v-if="quotaChips.length" :size="6" wrap align="center">
        <NTooltip v-for="c in quotaChips" :key="c.key" placement="top">
          <template #trigger>
            <NTag :type="tagType(c.status)" size="small" round><span class="quota__name">{{ c.label }}</span> {{ c.text }}</NTag>
          </template>
          {{ c.tooltip }}
        </NTooltip>
      </NSpace>
      <span v-else class="quota__empty mono">—</span>
    </NCard>

    <NCard v-if="hasProviders" class="route-card" title="路由" :bordered="true">
      <NSpace v-if="hasRoutes" vertical :size="12">
        <RouteLine
          v-for="p in config.profilesOrdered"
          :key="p.id"
          :profile-name="p.name"
          :quota="nodeQuotaForProfile(p)"
          :cooling="coolingFor(p)"
          :primary-model-label="modelLabelOf(effectiveModelFor(p))"
          :failover-model-label="failoverLabelOf(p)"
          :failover-via-strategy="!!effectiveFallbackOf(p)?.via_fallback_strategy_id"
        />
      </NSpace>
      <div v-else class="route-unconfigured">
        <NTag type="warning" round size="small">未配置</NTag>
        <span class="muted">尚未配置路由，前往「路由」页面添加</span>
      </div>
    </NCard>

    <NEmpty v-if="!hasProviders" description="还没有配置服务商 / 模型 / 路由">
      <template #extra>
        <span class="muted">前往「服务商」页面完成配置。</span>
      </template>
    </NEmpty>
  </NSpace>
</template>

<style scoped>
.route-card {
  background: var(--sl-panel-2);
}
.route-unconfigured {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}
.mono {
  font-family: var(--sl-font-mono);
}
.info-title {
  display: block;
  font-size: 13px;
  color: var(--sl-text-2);
  margin-bottom: 8px;
}
.quota__name {
  color: var(--sl-ink);
  margin-right: 4px;
}
.quota__empty {
  font-size: 16px;
  color: var(--sl-text-3);
}
.addr-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.addr-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.addr-label {
  color: var(--sl-text-2);
  font-size: 13px;
  flex-shrink: 0;
}
.addr-url {
  word-break: break-all;
  font-size: 13px;
}
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
.error-detail {
  margin-top: 8px;
  font-size: 12px;
  color: var(--sl-text-2);
  line-height: 1.4;
}
</style>
