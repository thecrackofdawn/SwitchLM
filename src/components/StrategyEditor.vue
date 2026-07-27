<script lang="ts">
// StrategyEditor — shared time-strategy editor used by the route modal (Profiles.vue,
// idScope="route") and the failover page (Fallback.vue, idScope="failover").
// Owns the StrategyForm shape + all add/remove/toggle/time helpers. The parent binds the
// strategies array with v-model; modelValue is treated as read-only input — every change
// (add, remove, and all field edits) emits a new array via update:modelValue with the
// edited element shallow-cloned. No in-place mutation of props.modelValue[i].
export interface StrategyForm {
  id: string;
  priority: number;
  enabled: boolean;
  days_of_week: number[];
  time_start: number; // minutes-of-day
  time_end: number; // minutes-of-day
  model_id: string;
}
</script>

<script setup lang="ts">
import { computed } from "vue";
import { NButton, NSelect, NSpace, NSwitch, NTag, NTimePicker, NTooltip } from "naive-ui";
import { genId } from "../lib/id";
import { ellipsisLabel } from "../lib/selectLabel";

const props = withDefaults(
  defineProps<{
    modelValue: StrategyForm[];
    modelOptions: { label: string; value: string }[];
    idScope: string;
    masterEnabled: boolean;
    scopeLabel?: string;
  }>(),
  { scopeLabel: "路由" }
);
const emit = defineEmits<{
  "update:modelValue": [StrategyForm[]];
  "update:masterEnabled": [boolean];
}>();

// On/off hint beneath the header. On-state differs by one word (路由/转移);
// off-state differs by structure, so branch on scopeLabel (exactly two callers).
const hintOn = computed(() => `按策略时段${props.scopeLabel}`);
const hintOff = computed(() =>
  props.scopeLabel === "路由"
    ? "已暂停策略，所有请求将直接走默认模型"
    : "已暂停策略，仅走默认转移目标"
);

// New strategies always default to priority 5 (mid-range of the 1..=5 scale);
// lower number = higher priority. Does not grow with strategy count.
const DEFAULT_STRATEGY_PRIORITY = 5;
function blankStrategy(): StrategyForm {
  return {
    // idScope namespaces the genId prefix so route vs failover ids never collide.
    id: genId(`${props.idScope}_s`, props.modelValue.map((s) => s.id)),
    priority: DEFAULT_STRATEGY_PRIORITY,
    enabled: true,
    days_of_week: [1, 2, 3, 4, 5],
    time_start: 22 * 60,
    time_end: 8 * 60,
    model_id: props.modelOptions[0]?.value ?? "",
  };
}

function addStrategy() {
  emit("update:modelValue", [...props.modelValue, blankStrategy()]);
}
function removeStrategy(id: string) {
  emit("update:modelValue", props.modelValue.filter((s) => s.id !== id));
}
// Every field edit emits a new array with the edited element shallow-cloned;
// modelValue is never mutated in place (v-model contract).
function patch(id: string, partial: Partial<StrategyForm>) {
  emit("update:modelValue", props.modelValue.map((x) => (x.id === id ? { ...x, ...partial } : x)));
}

function toggleDay(s: StrategyForm, d: number) {
  patch(s.id, {
    days_of_week: s.days_of_week.includes(d)
      ? s.days_of_week.filter((x) => x !== d)
      : [...s.days_of_week, d].sort((a, b) => a - b),
  });
}
function setWeekdays(s: StrategyForm, days: number[]) {
  patch(s.id, { days_of_week: [...days] });
}
function isCrossNight(s: StrategyForm) {
  return s.time_start > s.time_end;
}
// 0:00–0:00 = 全天（校验层仅允许这一种起止相等）。
function isAllDay(s: StrategyForm) {
  return s.time_start === 0 && s.time_end === 0;
}
// NTimePicker value is a ms timestamp; we store minutes-of-day.
function minToTs(min: number): number {
  const d = new Date();
  d.setHours(Math.floor(min / 60), min % 60, 0, 0);
  return d.getTime();
}
function tsToMin(ts: number | null): number {
  if (ts == null) return 0;
  const d = new Date(ts);
  return d.getHours() * 60 + d.getMinutes();
}
</script>

<template>
  <div class="section-head">
    <span class="section-title">时段策略</span>
    <span class="master-toggle">
      <span class="muted">启用时段策略</span>
      <NSwitch
        :value="masterEnabled"
        size="small"
        aria-label="启用时段策略"
        @update:value="(v: boolean) => emit('update:masterEnabled', v)"
      />
    </span>
  </div>
  <div class="muted master-hint">{{ masterEnabled ? hintOn : hintOff }}</div>

  <div class="strategy-list" :class="{ 'is-locked': !masterEnabled }" :inert="!masterEnabled">
    <div v-for="s in modelValue" :key="s.id" class="strategy-card">
    <div class="card-head">
      <NSpace align="center" :size="8">
        <span class="muted">优先级<NTooltip><template #trigger><span class="hint-q" role="img" aria-label="优先级说明">?</span></template>数字越小优先级越高</NTooltip></span>
        <NSelect
          :value="s.priority"
          :disabled="!s.enabled"
          size="small"
          style="width: 70px"
          :options="Array.from({ length: 5 }, (_, i) => ({ label: String(i + 1), value: i + 1 }))"
          @update:value="(v: number) => patch(s.id, { priority: v })"
        />
      </NSpace>
      <NSpace align="center" :size="8">
        <NSwitch
          :value="s.enabled"
          size="small"
          aria-label="启用该策略"
          @update:value="(v: boolean) => patch(s.id, { enabled: v })"
        />
        <NButton size="tiny" type="error" ghost @click="removeStrategy(s.id)">删除</NButton>
      </NSpace>
    </div>

    <div class="card-body" :class="{ 'is-off': !s.enabled }" :inert="!s.enabled">
      <div class="row">
        <span class="muted">重复</span>
        <NSpace :size="4" align="center">
          <NButton
            v-for="d in 7"
            :key="d"
            size="tiny"
            :type="s.days_of_week.includes(d) ? 'primary' : 'default'"
            @click="toggleDay(s, d)"
          >{{ "一二三四五六日"[d - 1] }}</NButton>
          <NButton size="tiny" quaternary @click="setWeekdays(s, [1, 2, 3, 4, 5])">工作日</NButton>
          <NButton size="tiny" quaternary @click="setWeekdays(s, [1, 2, 3, 4, 5, 6, 7])">全选</NButton>
        </NSpace>
      </div>

      <div class="row">
        <span class="muted">时间</span>
        <NTimePicker
          :value="minToTs(s.time_start)"
          format="HH:mm"
          size="small"
          style="width: 110px"
          @update:value="(v: number | null) => patch(s.id, { time_start: tsToMin(v) })"
        />
        <span class="muted">到</span>
        <NTimePicker
          :value="minToTs(s.time_end)"
          format="HH:mm"
          size="small"
          style="width: 110px"
          @update:value="(v: number | null) => patch(s.id, { time_end: tsToMin(v) })"
        />
        <NTag v-if="isAllDay(s)" type="success" size="small" round>🕐 全天</NTag>
        <NTag
          v-else-if="isCrossNight(s)"
          type="warning"
          size="small"
          round
        >🌙 已跨夜 (于次日 {{ String(Math.floor(s.time_end / 60)).padStart(2, "0") }}:{{ String(s.time_end % 60).padStart(2, "0") }} 结束)</NTag>
      </div>

      <div class="row">
        <span class="muted">调用模型</span>
        <NSelect
          :value="s.model_id"
          :options="modelOptions"
          :render-label="ellipsisLabel"
          size="small"
          placeholder="选择 Model"
          @update:value="(v: string) => patch(s.id, { model_id: v })"
        />
      </div>
    </div>
  </div>

    <span v-if="!modelValue.length" class="muted empty-hint">暂无时段策略，点击下方「添加策略」开始配置</span>
    <NButton class="add-btn" block dashed @click="addStrategy">+ 添加策略</NButton>
  </div>
</template>

<style scoped>
.strategy-card {
  border: 1px solid var(--sl-border);
  border-radius: 8px;
  padding: 10px;
  margin-top: 8px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.strategy-card .row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.card-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.card-body.is-off {
  opacity: 0.5;
}
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
.hint-q {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  margin-left: 2px;
  border: 1px solid var(--sl-text-3);
  border-radius: 50%;
  font-size: 11px;
  line-height: 1;
  color: var(--sl-text-3);
  cursor: help;
  vertical-align: middle;
}
.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-bottom: 6px;
  margin-bottom: 4px;
  border-bottom: 1px solid var(--sl-border);
}
.section-title {
  font-size: 14px;
  font-weight: 500;
}
.master-toggle {
  display: flex;
  align-items: center;
  gap: 8px;
}
.master-hint {
  font-size: 12px;
  margin-bottom: 8px;
}
.strategy-list.is-locked {
  opacity: 0.5;
}
.empty-hint {
  display: block;
  margin-top: 8px;
}
.add-btn {
  margin-top: 8px;
}
</style>
