<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
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
  NTag,
  useDialog,
  useMessage,
} from "naive-ui";
import { useConfigStore } from "../stores/config";
import type { QianwenLoginStatus, Provider, UsageCreds } from "../lib/types";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ellipsisLabel, vendorLabel, vendorOptions, OTHER_VENDOR } from "../lib/selectLabel";
import { genId } from "../lib/id";
import * as api from "../lib/commands";
import draggable from "vuedraggable";
import { useOrdered } from "../lib/useOrdered";

const msg = useMessage();
const dialog = useDialog();
const config = useConfigStore();

const { ordered, commit } = useOrdered("switchlm:order:providers", () => config.providers, (p) => p.id);

interface FormState {
  id: string;            // opaque PK (existing on edit; genId on new; conflict.id on overwrite)
  vendor: string;        // routing kind
  displayName: string;   // account name (list title)
  openai_base_url: string;
  anthropic_base_url: string;
  apiKey: string; // inference key; empty on edit = keep existing
  access_key_id: string;
  usageSk: string; // Volcengine usage SK; empty on edit = keep existing (stored in keyring)
}
const blank = (): FormState => ({
  id: "",
  vendor: "",
  displayName: "",
  openai_base_url: "",
  anthropic_base_url: "",
  apiKey: "",
  access_key_id: "",
  usageSk: "",
});
const form = reactive<FormState>(blank());
const showModal = ref(false);
const editing = ref(false);

// 火山用量查询走控制面 OpenAPI（AK/SK 签名），需要 usage_creds；智谱等用推理 api_key 即可，
// 不展示这两项以免误填。
const needsUsageCreds = computed(() => form.vendor.startsWith("volcengine"));
// 千问（qianwen-token）用量查询走控制台 Cookie（HttpOnly），需在应用内弹窗登录后由后端抓取。
const needsQianwenLogin = computed(() => form.vendor === "qianwen-token");

// Default protocol endpoints per known vendor (filled when the vendor is selected).
const vendorDefaults: Record<string, { openai_base_url: string; anthropic_base_url: string }> = {
  zhipu: {
    openai_base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
    anthropic_base_url: "https://open.bigmodel.cn/api/anthropic",
  },
  deepseek: {
    openai_base_url: "https://api.deepseek.com",
    anthropic_base_url: "https://api.deepseek.com/anthropic",
  },
  "volcengine-agent": {
    openai_base_url: "https://ark.cn-beijing.volces.com/api/plan/v3",
    anthropic_base_url: "https://ark.cn-beijing.volces.com/api/plan",
  },
  "volcengine-coding": {
    openai_base_url: "https://ark.cn-beijing.volces.com/api/coding/v3",
    anthropic_base_url: "https://ark.cn-beijing.volces.com/api/coding",
  },
  "qianwen-token": {
    openai_base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
    anthropic_base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
  },
};

// Default account name for a vendor: the vendor label, auto-suffixed " 2", " 3"…
// when an existing same-vendor provider already has that name.
function defaultDisplayName(vendor: string): string {
  const label = vendorOptions.find((o) => o.value === vendor)?.label ?? vendor;
  const taken = new Set(
    config.providers.filter((p) => p.vendor === vendor).map((p) => p.display_name),
  );
  if (!taken.has(label)) return label;
  for (let n = 2; ; n++) {
    const cand = `${label} ${n}`;
    if (!taken.has(cand)) return cand;
  }
}

function onVendorChange(vendor: string | number) {
  form.vendor = String(vendor);
  if (form.vendor === OTHER_VENDOR) {
    // 自定义服务商：不预填任何默认值（base_url / 账号名均留空，由用户填写）
    form.openai_base_url = "";
    form.anthropic_base_url = "";
    if (!editing.value) form.displayName = "";
    return;
  }
  const d = vendorDefaults[form.vendor];
  if (d) {
    form.openai_base_url = d.openai_base_url;
    form.anthropic_base_url = d.anthropic_base_url;
  }
  if (!editing.value) form.displayName = defaultDisplayName(form.vendor);
}

function openNew() {
  Object.assign(form, blank());
  editing.value = false;
  showModal.value = true;
}
function openEdit(p: Provider) {
  form.id = p.id;
  form.vendor = p.vendor ?? "";
  form.displayName = p.display_name;
  form.openai_base_url = p.openai_base_url ?? "";
  form.anthropic_base_url = p.anthropic_base_url ?? "";
  form.apiKey = ""; // never surface the stored key
  form.access_key_id = p.usage_creds?.access_key_id ?? "";
  form.usageSk = ""; // never surface the stored SK (kept in keyring, like the api_key)
  editing.value = true;
  showModal.value = true;
}

async function save() {
  if (!form.vendor.trim()) {
    msg.warning("请选择服务商");
    return;
  }
  if (!form.displayName.trim()) {
    msg.warning("请填写账号名称");
    return;
  }
  if (!form.openai_base_url.trim() && !form.anthropic_base_url.trim()) {
    msg.warning("至少填一个 base_url（OpenAI 或 Anthropic）");
    return;
  }
  const usage_creds: UsageCreds | null =
    needsUsageCreds.value && form.access_key_id
      ? { access_key_id: form.access_key_id }
      : null;
  const provider: Provider = {
    id: editing.value ? form.id : genId("prov", config.providers.map((p) => p.id)),
    vendor: form.vendor.trim(),
    display_name: form.displayName.trim(),
    openai_base_url: form.openai_base_url.trim() || null,
    anthropic_base_url: form.anthropic_base_url.trim() || null,
    usage_creds,
  };
  try {
    // Same-account check - NEW providers only (edit keeps a fixed id).
    if (!editing.value) {
      const conflict = await api.checkProviderConflict(
        form.vendor.trim(),
        usage_creds?.access_key_id ?? null,
        form.apiKey || null,
      );
      if (conflict) {
        const overwrite = await new Promise<boolean>((resolve) =>
          dialog.warning({
            title: "检测到同一账号",
            content: `与现有「${conflict.display_name}」疑似同一账号（同密钥），继续将覆盖该服务商配置。`,
            positiveText: "覆盖",
            negativeText: "取消",
            onPositiveClick: () => resolve(true),
            onNegativeClick: () => resolve(false),
            onMaskClick: () => resolve(false),
            // ESC and the X close button dismiss the dialog without firing onMaskClick,
            // so settle the Promise (cancel) explicitly - otherwise save() hangs at the await.
            onEsc: () => resolve(false),
            onClose: () => resolve(false),
          }),
        );
        if (!overwrite) return;
        provider.id = conflict.id; // overwrite in place (keeps FK + keyring valid)
      }
    }
    await config.saveProvider(provider); // backend enforces (vendor, display_name) uniqueness
    if (form.apiKey) await config.setKey(provider.id, form.apiKey); // after id finalized
    if (form.usageSk) await config.setUsageSk(provider.id, form.usageSk); // after id finalized
    msg.success(editing.value ? "已保存" : "已新增服务商配置");
    showModal.value = false;
  } catch (e) {
    msg.error(`保存失败：${String(e)}`); // shows the uniqueness error from the backend
  }
}

async function testConn(p: Provider) {
  try {
    const r = await config.testConnection(p.id);

    if (r.ok) {
      msg.success(`${p.display_name} 连接正常`);
      return;
    }

    // If we get a 404, the provider might not support /models endpoint
    // Offer to test with a simple prompt instead
    if (r.status === 404) {
      dialog.warning({
        title: "连接测试失败 - 尝试备用方案",
        content: `「${p.display_name}」的 /models 接口返回 404，可能不支持模型列表查询。

是否发送一个简单的测试请求来验证连接？

⚠️ 此测试将消耗少量请求额度`,
        positiveText: "继续测试",
        negativeText: "取消",
        onPositiveClick: async () => {
          try {
            const promptR = await config.testConnectionWithPrompt(p.id);
            promptR.ok
              ? msg.success(`${p.display_name} 连接正常（通过请求测试）`)
              : msg.warning(`${p.display_name} 连接失败（HTTP ${promptR.status}）：${promptR.detail}`);
          } catch (e) {
            msg.error(`${p.display_name} 连接出错：${String(e)}`);
          }
        },
      });
    } else {
      msg.warning(`${p.display_name} 连接失败（HTTP ${r.status}）：${r.detail}`);
    }
  } catch (e) {
    msg.error(`${p.display_name} 连接出错：${String(e)}`);
  }
}

function remove(p: Provider) {
  dialog.warning({
    title: "删除服务商配置",
    content: `确认删除「${p.display_name}」？其下模型将变为悬空配置。`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        await config.removeProvider(p.id);
        msg.success("已删除");
      } catch (e) {
        msg.error(`删除失败：${String(e)}`);
      }
    },
  });
}

async function openQianwenLogin() {
  try {
    await api.openQianwenLogin(form.id);
  } catch (e) {
    msg.error(`打开登录窗口失败：${String(e)}`);
  }
}
async function finishQianwenLogin() {
  try {
    await api.finishQianwenLogin(form.id);
  } catch (e) {
    msg.error(`完成登录失败：${String(e)}`);
  }
}

// 火山 AK/SK 申请指引（在系统浏览器打开）。
function openVolcDocs() {
  openUrl("https://docs.volcengine.com/docs/6257/64983?redirect=1&lang=zh").catch(() => {});
}

let unlistenStatus: UnlistenFn | undefined;
onMounted(async () => {
  await config.loadAll();
  unlistenStatus = await listen<QianwenLoginStatus>("qianwen-login-status", (e) => {
    const s = e.payload;
    void config.refreshKeys(); // re-fetch cookieSet (+ key/usage maps)
    const name = config.providers.find((p) => p.id === s.provider_id)?.display_name ?? s.provider_id;
    if (s.status === "captured") {
      msg.success(`「${name}」登录成功，Cookie 已获取`);
    } else if (s.status === "error") {
      msg.error(`获取 Cookie 失败：${s.message ?? "未知错误"}`);
    }
    // "closed-empty": user closed without logging in -> silent
  });
});
onUnmounted(() => unlistenStatus?.());
</script>

<template>
  <NSpace vertical :size="16">
    <NSpace justify="space-between" align="center">
      <span class="muted">配置国内 LLM 服务商的接入地址与推理密钥；新增服务商后，可在「模型配置」中配置其提供的模型</span>
      <NButton type="primary" @click="openNew">+ 新增服务商配置</NButton>
    </NSpace>

    <draggable
      v-model="ordered"
      item-key="id"
      class="drag-list"
      handle=".drag-handle"
      :animation="150"
      @end="commit"
    >
      <template #item="{ element: p }">
        <NCard size="small">
          <NSpace align="center" justify="space-between">
            <NSpace align="center" :size="8">
              <span class="drag-handle" title="拖动排序">⠿</span>
              <div class="provider-head">
                <span class="name">{{ p.display_name }}</span>
                <NSpace :size="6" align="center">
                  <NTag size="small" type="info">{{ vendorLabel(p.vendor ?? p.id) }}</NTag>
                  <NTag v-if="p.openai_base_url" size="small" type="info">OpenAI接口</NTag>
                  <NTag v-if="p.anthropic_base_url" size="small" type="info">Anthropic接口</NTag>
                  <NTag v-if="config.keySet[p.id]" size="small" type="success">密钥</NTag>
                  <NTag v-else size="small" type="warning">未设密钥</NTag>
                  <NTag
                    v-if="(p.vendor ?? p.id).startsWith('volcengine')"
                    size="small"
                    :type="config.usageSkSet[p.id] ? 'success' : 'warning'"
                  >
                    {{ config.usageSkSet[p.id] ? "用量SK" : "未设用量SK" }}
                  </NTag>
                  <NTag
                    v-if="(p.vendor ?? p.id) === 'qianwen-token'"
                    size="small"
                    :type="config.cookieSet[p.id] ? 'success' : 'warning'"
                  >
                    {{ config.cookieSet[p.id] ? "已登录" : "未登录" }}
                  </NTag>
                </NSpace>
              </div>
            </NSpace>
            <NSpace>
              <NButton size="small" @click="testConn(p)">连接测试</NButton>
              <NButton size="small" @click="openEdit(p)">编辑</NButton>
              <NButton size="small" type="error" ghost @click="remove(p)">删除</NButton>
            </NSpace>
          </NSpace>
        </NCard>
      </template>
    </draggable>

    <NEmpty v-if="!ordered.length" description="还没有服务商配置" />

    <NModal
      v-model:show="showModal"
      preset="card"
      :mask-closable="false"
      :title="editing ? '编辑服务商配置' : '新增服务商配置'"
      style="max-width: 480px"
    >
      <NForm label-placement="top">
        <NFormItem label="模型服务商（驱动用量/发现路由；智谱/火山可查用量）">
          <NSelect
            :value="form.vendor"
            :options="vendorOptions"
            :render-label="ellipsisLabel"
            :disabled="editing"
            filterable
            placeholder="选择服务商"
            @update:value="onVendorChange"
          />
        </NFormItem>
        <NFormItem label="账号名称（用于区分同服务商的多个账号）">
          <NInput v-model:value="form.displayName" placeholder="如：工作号 / 个人号" />
        </NFormItem>
        <NFormItem label="OpenAI base_url（与 Anthropic 二选一）">
          <NInput v-model:value="form.openai_base_url" placeholder="https://api.example.com/v1" />
        </NFormItem>
        <NFormItem label="Anthropic base_url">
          <NInput v-model:value="form.anthropic_base_url" placeholder="https://…/anthropic" />
        </NFormItem>
        <NFormItem label="推理 api_key">
          <NInput
            v-model:value="form.apiKey"
            type="password"
            show-password-on="click"
            :placeholder="editing ? '留空保留现有密钥' : 'sk-...'"
          />
        </NFormItem>
        <template v-if="editing && needsQianwenLogin">
          <NFormItem label="用量查询（千问 · Cookie 登录）">
            <NSpace vertical :size="8" style="width: 100%">
              <NSpace :size="8" align="center">
                <NButton size="small" type="primary" @click="openQianwenLogin">登录千问控制台</NButton>
                <NButton size="small" @click="finishQianwenLogin">完成登录</NButton>
                <NTag size="small" :type="config.cookieSet[form.id] ? 'success' : 'warning'">
                  {{ config.cookieSet[form.id] ? "已登录" : "未登录" }}
                </NTag>
              </NSpace>
              <span class="muted">点击「登录千问控制台」在弹窗中完成千问登录，登录成功后自动获取用量查询所需的 Cookie；自动检测失败时点「完成登录」。</span>
            </NSpace>
          </NFormItem>
        </template>
        <template v-if="needsUsageCreds">
          <NFormItem label="usage_creds · access_key_id（火山用量查询用）">
            <NInput v-model:value="form.access_key_id" placeholder="火山 AK" />
          </NFormItem>
          <NFormItem label="usage_creds · secret_access_key（火山用量查询用）">
            <NInput
              v-model:value="form.usageSk"
              type="password"
              show-password-on="click"
              :placeholder="editing ? '留空保留现有密钥' : '火山 SK'"
            />
          </NFormItem>
          <span class="muted docs-hint">AK/SK 仅用于查询火山套餐用量，在火山引擎控制台创建访问密钥获取。<a class="link" @click="openVolcDocs">申请指引 ↗</a></span>
        </template>
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
.provider-head {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.mono {
  font-family: var(--sl-font-mono);
}
.url {
  color: var(--sl-text-2);
  font-size: 12.5px;
}
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
.docs-hint {
  display: block;
}
.link {
  color: var(--sl-accent);
  cursor: pointer;
}
</style>
