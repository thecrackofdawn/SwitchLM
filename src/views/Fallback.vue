<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import {
  NButton,
  NCard,
  NEmpty,
  NForm,
  NFormItem,
  NInputNumber,
  NModal,
  NSelect,
  NSpace,
  NSwitch,
  NTag,
  useDialog,
  useMessage,
} from "naive-ui";
import { useConfigStore } from "../stores/config";
import type { Model, Strategy } from "../lib/types";
import StrategyEditor, { type StrategyForm } from "../components/StrategyEditor.vue";
import { validateFallbackContext } from "../lib/commands";
import { fmtTokens } from "../lib/format";
import { ellipsisLabel, providerLabel } from "../lib/selectLabel";
import draggable from "vuedraggable";
import { useOrdered } from "../lib/useOrdered";
import { usePolling } from "../lib/usePolling";

const msg = useMessage();
const dialog = useDialog();
const config = useConfigStore();

// "Configured" = has a default failover target and/or time strategies — only those get a card.
const configured = computed(() =>
  config.models.filter((m) => m.fallback_target_model_id || m.fallback_strategies?.length),
);
// Not-yet-configured models are the 新增 picker options (a model is configured at most once).
const unconfigured = computed(() =>
  config.models.filter((m) => !m.fallback_target_model_id && !m.fallback_strategies?.length),
);

// Cosmetic localStorage drag order (no persisted failover order on the backend; same
// pattern + key this page used before the redesign, so the old order carries over).
const { ordered, commit } = useOrdered("switchlm:order:models", () => configured.value, (m) => m.id);

function modelDisplayName(m: Model) {
  const provider = config.providers.find((p) => p.id === m.provider_id);
  return provider ? `${m.upstream_model_id} (${providerLabel(provider)})` : m.upstream_model_id;
}

function effectiveFallbackOf(m: Model) {
  return config.modelEffectiveFallbacks.find((e) => e.model_id === m.id);
}
/** 当前生效转移目标 label; falls back to the static default target until the next refresh. */
function fbLabel(m: Model): string {
  const eff = effectiveFallbackOf(m);
  const id = eff ? eff.effective_fallback_model_id : m.fallback_target_model_id ?? null;
  if (!id) return "无";
  const t = config.models.find((x) => x.id === id);
  return t ? modelDisplayName(t) : id;
}

async function onToggle(m: Model, enabled: boolean) {
  try {
    await config.setModelFallbackStrategiesEnabled(m.id, enabled);
  } catch (e) {
    msg.error(`切换失败：${String(e)}`);
  }
}

// ---- add/edit modal ----
interface FormState {
  model_id: string;
  fallback_target_model_id: string; // "" = no default target
  strategies_enabled: boolean;
  strategies: StrategyForm[];
  cooldown_seconds: number | null; // null = backend default
  retry_count: number; // required: Rust u32, always serialized (spec §5)
  retry_delay_secs: number;
}
const blank = (): FormState => ({
  model_id: "",
  fallback_target_model_id: "",
  strategies_enabled: true,
  strategies: [],
  cooldown_seconds: null,
  retry_count: 2,
  retry_delay_secs: 5,
});
const form = reactive<FormState>(blank());
const showModal = ref(false);
const editing = ref(false);

const unconfiguredOptions = computed(() =>
  unconfigured.value.map((m) => ({ label: modelDisplayName(m), value: m.id })),
);
// A model cannot fail over to itself (dispatch would skip it via the visited-set anyway);
// exclude the configured model from the default-target select and the strategy editor.
const targetOptions = computed(() => [
  { label: "（无默认转移目标）", value: "" },
  ...config.models
    .filter((m) => m.id !== form.model_id)
    .map((m) => ({ label: modelDisplayName(m), value: m.id })),
]);
const strategyModelOptions = computed(() =>
  config.models
    .filter((m) => m.id !== form.model_id)
    .map((m) => ({ label: modelDisplayName(m), value: m.id })),
);

function fromModel(m: Model): StrategyForm[] {
  return (m.fallback_strategies ?? []).map((s) => ({
    id: s.id, priority: s.priority, enabled: s.enabled,
    days_of_week: [...s.kind.days_of_week],
    time_start: s.kind.time_start, time_end: s.kind.time_end, model_id: s.kind.model_id,
  }));
}

function openNew() {
  Object.assign(form, blank());
  editing.value = false;
  showModal.value = true;
}
function openEdit(m: Model) {
  form.model_id = m.id;
  form.fallback_target_model_id = m.fallback_target_model_id ?? "";
  form.strategies_enabled = m.fallback_strategies_enabled ?? true;
  form.strategies = fromModel(m);
  form.cooldown_seconds = m.cooldown_seconds ?? null;
  form.retry_count = m.retry_count ?? 2;
  form.retry_delay_secs = m.retry_delay_secs ?? 5;
  editing.value = true;
  showModal.value = true;
}

function buildStrategies(): Strategy[] {
  return form.strategies.map((s) => ({
    id: s.id, priority: s.priority, enabled: s.enabled,
    kind: { type: "time", days_of_week: [...s.days_of_week], time_start: s.time_start, time_end: s.time_end, model_id: s.model_id },
  }));
}

async function doSave(modelId: string, target: string | null, strategies: Strategy[]) {
  try {
    await config.setModelFailover(modelId, target, strategies, form.strategies_enabled, form.cooldown_seconds, form.retry_count, form.retry_delay_secs);
    msg.success(editing.value ? "已保存" : "已新增转移配置");
    showModal.value = false;
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}

// Context-size guard (mirrors the old page's onChange check): compare the primary model
// against the default target AND every strategy target; warn-dialog on `smaller`.
async function save() {
  if (!editing.value && !form.model_id) {
    msg.warning("请选择要配置转移的模型");
    return;
  }
  const modelId = form.model_id;
  const target = form.fallback_target_model_id || null;
  const strategies = buildStrategies();
  if (target === modelId || strategies.some((s) => s.kind.model_id === modelId)) {
    msg.warning("转移目标不能是模型自身");
    return;
  }
  const targets = [...new Set(
    [target, ...strategies.map((s) => s.kind.model_id)].filter((x): x is string => !!x),
  )];
  let checks: { id: string; status: "ok" | "smaller" | "unknown"; primary?: number | null; fallback?: number | null }[];
  try {
    checks = await Promise.all(targets.map(async (t) => {
      const res = await validateFallbackContext(modelId, t);
      return { id: t, status: res.status, primary: res.primary_size, fallback: res.fallback_size };
    }));
  } catch (e) {
    msg.error(`校验失败：${String(e)}`);
    return;
  }
  const smaller = checks.filter((c) => c.status === "smaller");
  if (smaller.length) {
    const label = (id: string) => {
      const t = config.models.find((m) => m.id === id);
      return t ? modelDisplayName(t) : id;
    };
    dialog.warning({
      title: "转移目标上下文较小",
      content: `以下转移目标的上下文小于主模型：${smaller
        .map((c) => `${label(c.id)}（${fmtTokens(c.fallback)} < ${fmtTokens(c.primary)}）`)
        .join("、")}。降级时可能截断上下文。仍要保存吗？`,
      positiveText: "仍要保存",
      negativeText: "取消",
      onPositiveClick: () => doSave(modelId, target, strategies),
    });
    return;
  }
  if (checks.some((c) => c.status === "unknown")) {
    msg.info("部分目标无法确定上下文大小（不在目录且未手动设置），已跳过该校验");
  }
  await doSave(modelId, target, strategies);
}

function remove(m: Model) {
  dialog.warning({
    title: "删除转移配置",
    content: `确认删除「${modelDisplayName(m)}」的转移配置（默认目标与时段策略）？`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        // Clear the config so the card disappears; keep the model's existing cooldown.
        await config.setModelFailover(m.id, null, [], true, m.cooldown_seconds ?? null, m.retry_count ?? 2, m.retry_delay_secs ?? 5);
        msg.success("已删除");
      } catch (e) {
        msg.error(`删除失败：${String(e)}`);
      }
    },
  });
}

// Fold into the store's existing effective refresh (route + model failover), same as 路由页.
usePolling(() => config.refreshEffective(), () => 60_000);

onMounted(() => config.loadAll());
</script>

<template>
  <NSpace vertical :size="16">
    <NSpace justify="space-between" align="center">
      <span class="muted">模型遇到限流、欠费或额度耗尽时，自动转移到备用模型；可按时段切换目标</span>
      <NButton type="primary" :disabled="!unconfigured.length" @click="openNew">+ 新增</NButton>
    </NSpace>

    <draggable v-model="ordered" item-key="id" class="drag-list" handle=".drag-handle" :animation="150" @end="commit">
      <template #item="{ element: m }">
        <NCard size="small">
          <NSpace align="center" justify="space-between" wrap>
            <NSpace align="center" :size="10" wrap>
              <span class="drag-handle" title="拖动排序">⠿</span>
              <span class="name">{{ modelDisplayName(m) }}</span>
              <span class="arrow">当前生效</span>
              <span class="backing">{{ fbLabel(m) }}<span v-if="effectiveFallbackOf(m)?.via_fallback_strategy_id">⏰</span></span>
              <NTag v-if="(m.fallback_strategies?.length ?? 0) > 0" size="small" round :bordered="false"
                :type="m.fallback_strategies_enabled ? 'info' : 'default'">
                ⏰ 策略 {{ m.fallback_strategies!.length }} 条
                <NSwitch :value="m.fallback_strategies_enabled" size="small" @update:value="(v: boolean) => onToggle(m, v)" />
              </NTag>
            </NSpace>
            <NSpace>
              <NButton size="small" @click="openEdit(m)">编辑</NButton>
              <NButton size="small" type="error" ghost @click="remove(m)">删除</NButton>
            </NSpace>
          </NSpace>
        </NCard>
      </template>
    </draggable>

    <NEmpty v-if="!ordered.length" description="还没有转移配置，点右上「+ 新增」" />

    <NModal
      v-model:show="showModal"
      preset="card"
      :mask-closable="false"
      :title="editing ? '编辑转移配置' : '新增转移配置'"
      style="max-width: 560px"
    >
      <NForm label-placement="top">
        <NFormItem v-if="!editing" label="模型">
          <NSelect v-model:value="form.model_id" :options="unconfiguredOptions" :render-label="ellipsisLabel" placeholder="选择要配置转移的 Model" />
        </NFormItem>
        <NFormItem label="默认转移目标">
          <NSelect v-model:value="form.fallback_target_model_id" :options="targetOptions" :render-label="ellipsisLabel" placeholder="限流 / 欠费 / 额度耗尽时转移到" />
        </NFormItem>
        <NFormItem label="熔断冷却（秒）">
          <NInputNumber v-model:value="form.cooldown_seconds" :min="0" :show-button="false" placeholder="300（默认）" clearable style="width: 180px" />
        </NFormItem>
        <NFormItem label="临时限流重试">
          <NSpace align="center" :size="8">
            <NInputNumber v-model:value="form.retry_count" :min="0" :show-button="false" placeholder="2" style="width: 96px" />
            <span class="muted">次，每次间隔</span>
            <NInputNumber v-model:value="form.retry_delay_secs" :min="0" :show-button="false" placeholder="5" style="width: 96px" />
            <span class="muted">秒</span>
          </NSpace>
        </NFormItem>
        <div class="muted" style="margin: -4px 0 8px; font-size: 12px">
          仅临时限流（额度未耗尽）时原地等待重试；额度耗尽或重试耗尽后仍走熔断 fallback。0 = 关闭。
        </div>

        <StrategyEditor
          v-model="form.strategies"
          v-model:master-enabled="form.strategies_enabled"
          :model-options="strategyModelOptions"
          :scope-label="'转移'"
          id-scope="failover"
        />
      </NForm>
      <template #footer>
        <NSpace justify="end">
          <NButton @click="showModal = false">取消</NButton>
          <NButton type="primary" @click="save">保存</NButton>
        </NSpace>
      </template>
    </NModal>
  </NSpace>
</template>

<style scoped>
.name {
  font-weight: 600;
}
.arrow {
  color: var(--sl-text-3);
  font-size: 12px;
}
.backing {
  color: var(--sl-accent);
  font-weight: 600;
}
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
</style>
