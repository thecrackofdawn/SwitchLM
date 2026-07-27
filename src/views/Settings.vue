<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import {
  NButton,
  NCard,
  NInputNumber,
  NSelect,
  NSpace,
  NSwitch,
  useDialog,
  useMessage,
} from "naive-ui";
import { useSystemStore } from "../stores/system";
import {
  LOG_LEVEL_OPTIONS,
  MAX_USAGE_REFRESH_SECS,
  MIN_USAGE_REFRESH_SECS,
} from "../lib/types";

const msg = useMessage();
const dialog = useDialog();
const system = useSystemStore();

const portInput = ref<number | null>(null);

watch(
  () => system.settings,
  (s) => {
    if (s) portInput.value = s.port;
  },
  { immediate: true },
);

const refreshInput = ref<number | null>(null);
watch(
  () => system.settings,
  (s) => {
    if (s) refreshInput.value = s.usage_refresh_interval_secs;
  },
  { immediate: true },
);

const levelOptions = LOG_LEVEL_OPTIONS.map((v) => ({ label: v, value: v }));
const levelInput = ref<string>("info");
watch(
  () => system.settings,
  (s) => {
    if (s) levelInput.value = s.log_level;
  },
  { immediate: true },
);

async function saveRefresh() {
  if (refreshInput.value == null) return;
  try {
    await system.saveUsageRefreshInterval(refreshInput.value);
    msg.success("刷新间隔已保存");
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}

async function saveLevel() {
  if (!levelInput.value) return;
  try {
    await system.saveLogLevel(levelInput.value);
    msg.success("日志级别已保存");
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}

async function openLogs() {
  try {
    await system.openLogDir();
  } catch (e) {
    msg.error(`打开失败：${String(e)}`);
  }
}

async function savePort() {
  if (portInput.value == null) return;
  try {
    await system.savePort(portInput.value);
    msg.success("端口已保存");
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}
async function toggleAutostart(on: boolean) {
  try {
    await system.setAutostart(on);
    msg.success(on ? "已开启开机自启" : "已关闭开机自启");
  } catch (e) {
    msg.error(`设置失败：${String(e)}`);
  }
}
async function restart() {
  try {
    await system.restart();
    msg.success("服务已重启");
  } catch (e) {
    msg.error(`重启失败：${String(e)}`);
  }
}
function quit() {
  dialog.warning({
    title: "退出 SwitchLM",
    content: "确认退出？代理与托盘将停止运行。",
    positiveText: "退出",
    negativeText: "取消",
    onPositiveClick: () => system.quit(),
  });
}

onMounted(async () => {
  await Promise.all([system.refresh(), system.loadSettings()]);
});
</script>

<template>
  <NSpace vertical :size="16">
    <NCard title="端口" size="small">
      <NSpace vertical :size="10">
        <NSpace align="center" :size="12">
          <NInputNumber v-model:value="portInput" :min="1" :max="65535" :show-button="false" style="width: 160px" />
          <NButton type="primary" @click="savePort">保存</NButton>
        </NSpace>
      </NSpace>
    </NCard>

    <NCard title="开机自启" size="small">
      <NSpace align="center" :size="12">
        <NSwitch
          :value="system.settings?.autostart ?? false"
          @update:value="(v: boolean) => toggleAutostart(v)"
        />
        <span class="muted">系统登录时自动启动 SwitchLM</span>
      </NSpace>
    </NCard>

    <NCard title="用量刷新间隔" size="small">
      <NSpace align="center" :size="12">
        <NInputNumber
          v-model:value="refreshInput"
          :min="MIN_USAGE_REFRESH_SECS"
          :max="MAX_USAGE_REFRESH_SECS"
          :show-button="false"
          style="width: 160px"
        >
          <template #suffix>秒</template>
        </NInputNumber>
        <NButton type="primary" @click="saveRefresh">保存</NButton>
        <span class="muted">最小 {{ MIN_USAGE_REFRESH_SECS }}s · 默认 60s · 停留用量/概览页面时自动刷新的频率</span>
      </NSpace>
    </NCard>

    <NCard title="日志" size="small">
      <NSpace align="center" :size="12">
        <NSelect v-model:value="levelInput" :options="levelOptions" style="width: 140px" />
        <NButton type="primary" @click="saveLevel">保存</NButton>
        <NButton @click="openLogs">打开日志文件夹</NButton>
        <span class="muted">默认 info · 向 trace 调整可见更详细日志（立即生效），用于排查问题</span>
      </NSpace>
    </NCard>

    <NCard title="服务" size="small">
      <NSpace>
        <NButton :loading="system.refreshing" @click="restart">重启服务</NButton>
        <NButton type="error" ghost @click="quit">退出 SwitchLM</NButton>
      </NSpace>
    </NCard>
  </NSpace>
</template>

<style scoped>
.mono {
  font-family: var(--sl-font-mono);
}
.muted {
  color: var(--sl-text-2);
  font-size: 13px;
}
</style>
