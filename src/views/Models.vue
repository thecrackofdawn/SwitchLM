<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import {
  NButton,
  NCard,
  NEmpty,
  NForm,
  NFormItem,
  NInput,
  NInputNumber,
  NModal,
  NSelect,
  NSpace,
  useDialog,
  useMessage,
} from "naive-ui";
import { useConfigStore } from "../stores/config";
import type { Model, ModelSource, Strategy } from "../lib/types";
import { genId } from "../lib/id";
import { ellipsisLabel, providerLabel } from "../lib/selectLabel";
import { recognizedContextSize, setCustomContextSize } from "../lib/commands";
import draggable from "vuedraggable";
import { useOrdered } from "../lib/useOrdered";

const msg = useMessage();
const dialog = useDialog();
const config = useConfigStore();

// Drag-to-reorder models (cosmetic, localStorage; shared with the Fallback tab).
const { ordered, commit } = useOrdered("switchlm:order:models", () => config.models, (m) => m.id);

const providerOptions = computed(() => config.providers.map((p) => ({ label: providerLabel(p), value: p.id })));

// ---- edit form ----
// The user enters the upstream model name (= upstream_model_id).
// Protocol + base_url are inherited from the provider; the internal id is auto-generated.
interface FormState {
  id: string;
  provider_id: string;
  upstream_model_id: string;
  source: ModelSource;
  cooldown: number | null;
  context_size: number | null;
  // Preserved on edit (fallback is configured on the Fallback page, not this modal);
  // without this, buildModel would reset it to null every time a model is edited.
  fallback_target_model_id: string | null;
  // Same hazard: failover time strategies live on the 故障转移 page; upsert_model
  // replaces the whole struct, so we must carry these through or they get wiped.
  fallback_strategies: Strategy[];
  fallback_strategies_enabled: boolean;
  // Same hazard as cooldown: retry config is edited on the 故障转移 page; upsert_model
  // replaces the whole struct, so we round-trip these to avoid wiping them.
  retry_count: number;
  retry_delay_secs: number;
}
const blank = (): FormState => ({
  id: "",
  provider_id: config.providers[0]?.id ?? "",
  upstream_model_id: "",
  source: "manual",
  cooldown: null,
  context_size: null,
  fallback_target_model_id: null,
  fallback_strategies: [],
  fallback_strategies_enabled: true,
  retry_count: 2,
  retry_delay_secs: 5,
});
const form = reactive<FormState>(blank());
const showModal = ref(false);
const editing = ref(false);

function openNew() {
  Object.assign(form, blank());
  editing.value = false;
  showModal.value = true;
  // Auto-fetch available models for the default provider
  if (form.provider_id) {
    void fetchModelsForProvider(form.provider_id);
  }
}
function openEdit(m: Model) {
  form.id = m.id;
  form.provider_id = m.provider_id;
  form.upstream_model_id = m.upstream_model_id;
  form.source = m.source ?? "manual";
  form.cooldown = m.cooldown_seconds ?? null;
  form.context_size = null;
  form.fallback_target_model_id = m.fallback_target_model_id ?? null;
  form.fallback_strategies = m.fallback_strategies ?? [];
  form.fallback_strategies_enabled = m.fallback_strategies_enabled ?? true;
  form.retry_count = m.retry_count ?? 2;
  form.retry_delay_secs = m.retry_delay_secs ?? 5;
  editing.value = true;
  showModal.value = true;
  void fetchContextSize();
}

function buildModel(): Model {
  const upstream = form.upstream_model_id.trim();
  return {
    id: form.id.trim(),
    provider_id: form.provider_id,
    upstream_model_id: upstream,
    source: form.source,
    cooldown_seconds: form.cooldown,
    fallback_target_model_id: form.fallback_target_model_id,
    fallback_strategies: form.fallback_strategies,
    fallback_strategies_enabled: form.fallback_strategies_enabled,
    retry_count: form.retry_count,
    retry_delay_secs: form.retry_delay_secs,
  };
}

async function save() {
  if (!editing.value && !form.id) form.id = genId("m", config.models.map((m) => m.id));
  if (!form.provider_id || !form.upstream_model_id.trim()) {
    msg.warning("账号 / 模型名称 不能为空");
    return;
  }
  const upTrim = form.upstream_model_id.trim();
  const dup = config.models.some(
    (m) => m.id !== form.id && m.provider_id === form.provider_id && m.upstream_model_id.trim() === upTrim,
  );
  if (dup) {
    msg.warning(`该账号下已存在模型「${upTrim}」`);
    return;
  }
  try {
    await config.saveModel(buildModel());
    // context_size is a catalog (vendor+upstream) property, written separately and real-time
    // (memory + custom_provider_desc.json together). Skip if unchanged from the recognized value.
    if (form.context_size !== lastRecognized.value) {
      try {
        await setCustomContextSize(form.provider_id, form.upstream_model_id.trim(), form.context_size);
      } catch (e) {
        msg.warning(`上下文大小保存失败：${String(e)}`);
      }
    }
    msg.success(editing.value ? "已保存" : "已新增模型");
    showModal.value = false;
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}

function remove(m: Model) {
  dialog.warning({
    title: "删除模型",
    content: `确认删除「${m.upstream_model_id}」？`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        await config.removeModel(m.id);
        msg.success("已删除");
      } catch (e) {
        msg.error(`删除失败：${String(e)}`);
      }
    },
  });
}

// ---- auto-discover for new model form ----
const availableModels = ref<string[]>([]);
const fetchingModels = ref(false);

async function fetchModelsForProvider(providerId: string) {
  fetchingModels.value = true;
  availableModels.value = [];
  try {
    const found = await config.discover(providerId);
    availableModels.value = found.map((f) => f.id);
  } catch (e) {
    // Silently fail - user can still manually enter model name
    console.error('Failed to fetch models:', e);
  } finally {
    fetchingModels.value = false;
  }
}

// ---- discover removed (legacy) ----

// Watch provider changes in form to fetch available models (for new models only)
watch(() => form.provider_id, (newProviderId) => {
  if (newProviderId && !editing.value) {
    void fetchModelsForProvider(newProviderId);
  }
});

// ---- context size: catalog value for (provider, upstream), edited in-place and written
// real-time to custom_provider_desc.json via set_custom_context_size. The input shows the
// effective size (custom override if set, else bundled default); `lastRecognized` gates the
// save so an unchanged value isn't re-written. ----
const lastRecognized = ref<number | null>(null);
async function fetchContextSize() {
  const pid = form.provider_id;
  const up = form.upstream_model_id.trim();
  if (!pid || !up) {
    form.context_size = null;
    lastRecognized.value = null;
    return;
  }
  try {
    const n = await recognizedContextSize(pid, up);
    form.context_size = n;
    lastRecognized.value = n;
  } catch {
    form.context_size = null;
    lastRecognized.value = null;
  }
}
watch(() => [form.provider_id, form.upstream_model_id], fetchContextSize);

function providerName(id: string) {
  const p = config.providers.find((q) => q.id === id);
  return p ? providerLabel(p) : id;
}

onMounted(() => config.loadAll());
</script>

<template>
  <NSpace vertical :size="16">
    <NSpace justify="space-between" align="center">
      <span class="muted">模型配置：新增模型时自动拉取可选模型；协议与端点继承账号配置</span>
      <NButton type="primary" @click="openNew">+ 新增模型</NButton>
    </NSpace>

    <draggable
      v-model="ordered"
      item-key="id"
      class="drag-list"
      handle=".drag-handle"
      :animation="150"
      @end="commit"
    >
      <template #item="{ element: m }">
        <NCard size="small">
          <NSpace align="center" justify="space-between">
            <NSpace align="center" :size="10">
              <span class="drag-handle" title="拖动排序">⠿</span>
              <span class="name">{{ m.upstream_model_id }}</span>
              <span class="muted">{{ providerName(m.provider_id) }}</span>
            </NSpace>
            <NSpace>
              <NButton size="small" @click="openEdit(m)">编辑</NButton>
              <NButton size="small" type="error" ghost @click="remove(m)">删除</NButton>
            </NSpace>
          </NSpace>
        </NCard>
      </template>
    </draggable>

    <NEmpty v-if="!config.models.length" description="还没有模型，用「新增模型」添加" />

    <!-- edit / new -->
    <NModal
      v-model:show="showModal"
      preset="card"
      :mask-closable="false"
      :title="editing ? '编辑模型' : '新增模型'"
      style="max-width: 480px"
    >
      <NForm label-placement="top">
        <NFormItem label="账号">
          <NSelect v-model:value="form.provider_id" :options="providerOptions" :render-label="ellipsisLabel" placeholder="选择账号" />
        </NFormItem>
        <NFormItem label="模型名称（即 upstream_model_id，发给上游）">
          <NSelect
            v-if="availableModels.length && !editing"
            v-model:value="form.upstream_model_id"
            :options="availableModels.map(m => ({ label: m, value: m }))"
            :render-label="ellipsisLabel"
            :loading="fetchingModels"
            placeholder="选择或输入模型名称"
            filterable
            tag
          />
          <NInput
            v-else
            v-model:value="form.upstream_model_id"
            placeholder="glm-4.6"
          />
        </NFormItem>
        <NFormItem label="上下文大小（token，目录共享值）">
          <NInputNumber
            v-model:value="form.context_size"
            :min="0"
            :show-button="false"
            placeholder="未识别"
            clearable
            style="width: 200px"
          />
          <span class="muted" style="margin-left: 8px">同厂商共享，清空回退默认</span>
        </NFormItem>
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
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
.discover-list {
  max-height: 260px;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
</style>
