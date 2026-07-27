<script setup lang="ts">
import { NTooltip } from "naive-ui";
import type { NodeQuota } from "../lib/quotaUtils";

// Per-route pipeline (strategy-aware, time-based forwarding + failover):
//   [路由] profile  ▶  [当前生效] effective model  ▶  [故障转移] effective failover target (optional)
// Read-only here - switching happens on the 路由/模型 tabs. The 故障转移 node shows the
// route's TIME-AWARE effective failover model (effective_fallback_model_id, resolved by
// the backend), with ⏰ when a time strategy selected it; hidden when the route has no
// failover target. Quota badge on 当前生效 shows REMAINING
// (plan %) / balance (consumption), health-colored - same number/source/thresholds as the
// 可用额度 chips above. Styles use --sl-* tokens.
withDefaults(
  defineProps<{
    profileName?: string;
    /** Effective model quota (remaining % / balance). null -> no badge. */
    quota?: NodeQuota | null;
    /** Effective model circuit-breaker cooling down -> amber tint + ❄. */
    cooling?: boolean;
    /** Effective model label (upstream id + provider), shown as static text. */
    primaryModelLabel?: string;
    /** Time-aware effective failover model label, shown in a 故障转移 node.
     *  Empty -> node hidden. */
    failoverModelLabel?: string;
    /** Failover target chosen by a time strategy -> append ⏰. */
    failoverViaStrategy?: boolean;
  }>(),
  {
    cooling: false,
    quota: null,
    primaryModelLabel: "",
    failoverModelLabel: "",
    failoverViaStrategy: false,
  },
);
</script>

<template>
  <div class="pipeline mono">
    <!-- 路由 (Profile alias the agent is configured with) -->
    <div class="node">
      <span class="tag tag-route">路由</span>
      <span class="val">{{ profileName || "-" }}</span>
    </div>
    <span class="arrow">▶</span>

    <!-- 当前生效 (strategy-aware effective model actually receiving the request) -->
    <div class="node node-primary" :class="{ cooling }">
      <span class="tag tag-primary">当前生效</span>
      <NTooltip v-if="primaryModelLabel" placement="top">
        <template #trigger>
          <span class="val val-model">{{ primaryModelLabel }}</span>
        </template>
        {{ primaryModelLabel }}
      </NTooltip>
      <span v-else class="val val-model">-</span>
      <span v-if="cooling" class="mark" title="冷却中">❄</span>
      <NTooltip v-if="quota" placement="top">
        <template #trigger>
          <span class="badge" :class="quota.status">{{ quota.text }}</span>
        </template>
        {{ quota.tooltip }}
      </NTooltip>
    </div>

    <!-- 故障转移 (route's time-aware effective failover target) — hidden when
         failoverModelLabel is empty; ⏰ marks a target chosen by a time strategy. -->
    <template v-if="failoverModelLabel">
      <span class="arrow">▶</span>
      <div class="node">
        <span class="tag tag-fallback">故障转移</span>
        <NTooltip placement="top">
          <template #trigger>
            <span class="val val-model">{{ failoverModelLabel }}<span v-if="failoverViaStrategy">⏰</span></span>
          </template>
          {{ failoverModelLabel }}
        </NTooltip>
      </div>
    </template>
  </div>
</template>

<style scoped>
.pipeline {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  font-size: 13px;
}
.node {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border-radius: var(--sl-radius-sm);
  background: var(--sl-panel);
  border: 1px solid var(--sl-line);
  color: var(--sl-ink);
}
.node-primary {
  border-color: var(--sl-accent);
  background: var(--sl-accent-weak);
}
.node-primary.cooling {
  border-color: var(--sl-cool);
  background: var(--sl-cool-weak);
}
.val {
  font-weight: 500;
}
.val-model {
  display: inline-block;
  max-width: 160px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.mark {
  color: var(--sl-cool);
}
.arrow {
  color: var(--sl-text-3);
  font-size: 11px;
}
/* 前缀标签 */
.tag {
  font-size: 10.5px;
  font-weight: 700;
  letter-spacing: 0.02em;
  padding: 2px 6px;
  border-radius: 3px;
  line-height: 1.4;
}
.tag-route {
  background: var(--sl-accent-weak);
  color: var(--sl-accent);
}
.tag-primary {
  background: var(--sl-accent);
  color: #fff;
}
.tag-fallback {
  background: var(--sl-panel-2);
  color: var(--sl-text-2);
}
/* 用量角标：颜色随已用百分比变化（绿/琥珀/红） */
.badge {
  font-size: 10.5px;
  font-weight: 700;
  padding: 1px 5px;
  border-radius: 3px;
}
.badge.ok {
  color: var(--sl-ok);
  background: color-mix(in srgb, var(--sl-ok) 16%, transparent);
}
.badge.warn {
  color: var(--sl-cool);
  background: color-mix(in srgb, var(--sl-cool) 18%, transparent);
}
.badge.danger {
  color: var(--sl-trip);
  background: color-mix(in srgb, var(--sl-trip) 18%, transparent);
}
</style>
