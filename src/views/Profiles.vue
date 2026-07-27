<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import {
  NButton,
  NCard,
  NEmpty,
  NForm,
  NFormItem,
  NInput,
  NModal,
  NSelect,
  NSpace,
  NSwitch,
  NTag,
  useDialog,
  useMessage,
} from "naive-ui";
import { useConfigStore } from "../stores/config";
import type { Profile } from "../lib/types";
import StrategyEditor, { type StrategyForm } from "../components/StrategyEditor.vue";
import { genId } from "../lib/id";
import { ellipsisLabel, providerLabel } from "../lib/selectLabel";
import draggable from "vuedraggable";
import { usePolling } from "../lib/usePolling";

const msg = useMessage();
const dialog = useDialog();
const config = useConfigStore();

// Config-backed drag order (single source shared with tray + 概览). Local `ordered` updates
// instantly for a responsive drag; persistence is debounced to coalesce rapid reorder into
// one backend save + tray rebuild.
const ordered = ref<Profile[]>([]);
const dragging = ref(false);
watch(
  () => config.profilesOrdered,
  (list) => {
    if (!dragging.value) ordered.value = [...list];
  },
  { immediate: true },
);
let persistTimer: ReturnType<typeof setTimeout> | null = null;
function commit() {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    config.setRouteOrder(ordered.value.map((p) => p.id)).catch((e) => msg.error(`顺序保存失败：${String(e)}`));
    persistTimer = null;
  }, 400);
}
function onDragEnd() {
  dragging.value = false;
  commit();
}

const modelOptions = computed(() =>
  config.models.map((m) => {
    const provider = config.providers.find((p) => p.id === m.provider_id);
    const label = provider ? `${m.upstream_model_id} (${providerLabel(provider)})` : m.upstream_model_id;
    return { label, value: m.id };
  }),
);

interface FormState {
  id: string;
  name: string;
  aliases: string[];
  backing_model_id: string;
  strategies_enabled: boolean;
  strategies: StrategyForm[];
}
const blank = (): FormState => ({
  id: "",
  name: "",
  aliases: [],
  backing_model_id: config.models[0]?.id ?? "",
  strategies_enabled: true,
  strategies: [],
});
function fromProfile(p: Profile): StrategyForm[] {
  return (p.strategies ?? []).map((s) => ({
    id: s.id, priority: s.priority, enabled: s.enabled,
    days_of_week: [...s.kind.days_of_week],
    time_start: s.kind.time_start, time_end: s.kind.time_end, model_id: s.kind.model_id,
  }));
}
const form = reactive<FormState>(blank());
const showModal = ref(false);
const editing = ref(false);

function openNew() {
  Object.assign(form, blank());
  editing.value = false;
  showModal.value = true;
}
function openEdit(p: Profile) {
  form.id = p.id;
  form.name = p.name;
  form.aliases = [...(p.aliases ?? [])];
  form.backing_model_id = p.backing_model_id;
  form.strategies_enabled = p.strategies_enabled ?? true;
  form.strategies = fromProfile(p);
  editing.value = true;
  showModal.value = true;
}

async function save() {
  if (!editing.value && !form.id) form.id = genId("p", config.profiles.map((p) => p.id));
  if (!form.name.trim() || !form.backing_model_id) {
    msg.warning("名称 / 接入模型 不能为空");
    return;
  }
  const profile: Profile = {
    id: form.id.trim(),
    name: form.name.trim(),
    aliases: form.aliases,
    backing_model_id: form.backing_model_id,
    strategies_enabled: form.strategies_enabled,
    strategies: form.strategies.map((s) => ({
      id: s.id, priority: s.priority, enabled: s.enabled,
      kind: { type: "time", days_of_week: [...s.days_of_week], time_start: s.time_start, time_end: s.time_end, model_id: s.model_id },
    })),
  };
  try {
    await config.saveProfile(profile);
    msg.success(editing.value ? "已保存" : "已新增路由");
    showModal.value = false;
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}

function remove(p: Profile) {
  dialog.warning({
    title: "删除路由",
    content: `确认删除「${p.name}」？`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        await config.removeProfile(p.id);
        msg.success("已删除");
      } catch (e) {
        msg.error(`删除失败：${String(e)}`);
      }
    },
  });
}

function backingName(id: string) {
  const m = config.models.find((m) => m.id === id);
  if (!m) return id;
  const provider = config.providers.find((p) => p.id === m.provider_id);
  return provider ? `${m.upstream_model_id} (${providerLabel(provider)})` : m.upstream_model_id;
}

function effectiveOf(p: Profile) {
  return config.routeEffective.find((e) => e.profile_id === p.id);
}
function effectiveLabel(p: Profile): string {
  const eff = effectiveOf(p);
  const id = eff?.effective_model_id ?? p.backing_model_id;
  return backingName(id);
}
async function toggleMaster(p: Profile, enabled: boolean) {
  try { await config.setStrategiesEnabled(p.id, enabled); }
  catch (e) { msg.error(`切换失败：${String(e)}`); }
}

usePolling(() => config.refreshEffective(), () => 60_000);

onMounted(() => config.loadAll());
</script>

<template>
  <NSpace vertical :size="16">
    <NSpace justify="space-between" align="center">
      <span class="muted">agent 请求里的模型名，指向一个服务商模型</span>
      <NButton type="primary" @click="openNew">+ 新增路由</NButton>
    </NSpace>

    <draggable v-model="ordered" item-key="id" class="drag-list" handle=".drag-handle" :animation="150" @start="dragging = true" @end="onDragEnd">
      <template #item="{ element: p }">
        <NCard size="small">
          <NSpace align="center" justify="space-between" wrap>
            <NSpace align="center" :size="10" wrap>
              <span class="drag-handle" title="拖动排序">⠿</span>
              <span class="name">{{ p.name }}</span>
              <span class="arrow">当前生效</span>
              <span class="backing">{{ effectiveLabel(p) }}<span v-if="effectiveOf(p)?.via_strategy_id">⏰</span></span>
              <NTag v-if="(p.strategies?.length ?? 0) > 0" size="small" round :bordered="false"
                :type="p.strategies_enabled ? 'info' : 'default'">
                ⏰ 策略 {{ p.strategies!.length }} 条
                <NSwitch :value="p.strategies_enabled" size="small" @update:value="(v: boolean) => toggleMaster(p, v)" />
              </NTag>
            </NSpace>
            <NSpace>
              <NButton size="small" @click="openEdit(p)">编辑</NButton>
              <NButton size="small" type="error" ghost @click="remove(p)">删除</NButton>
            </NSpace>
          </NSpace>
        </NCard>
      </template>
    </draggable>

    <NEmpty v-if="!ordered.length" description="还没有路由" />

    <NModal
      v-model:show="showModal"
      preset="card"
      :mask-closable="false"
      :title="editing ? '编辑路由' : '新增路由'"
      style="max-width: 560px"
    >
      <NForm label-placement="top">
        <NFormItem label="名称">
          <NInput v-model:value="form.name" placeholder="glm-5.2" />
        </NFormItem>
        <NFormItem label="默认模型">
          <NSelect v-model:value="form.backing_model_id" :options="modelOptions" :render-label="ellipsisLabel" placeholder="选择 Model" />
        </NFormItem>

        <StrategyEditor
          v-model="form.strategies"
          v-model:master-enabled="form.strategies_enabled"
          :model-options="modelOptions"
          :scope-label="'路由'"
          id-scope="route"
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
.mono {
  font-family: var(--sl-font-mono);
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
