<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { RouterView, useRoute, useRouter } from "vue-router";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  darkTheme,
  NButton,
  NConfigProvider,
  NDialogProvider,
  NLayout,
  NLayoutContent,
  NLayoutHeader,
  NLayoutSider,
  NMenu,
  NMessageProvider,
  NModal,
  NSpace,
  NText,
  type GlobalThemeOverrides,
  type MenuOption,
} from "naive-ui";
import { darkOverrides, lightOverrides } from "./styles/theme";
import { useSystemStore } from "./stores/system";

const router = useRouter();
const route = useRoute();

const system = useSystemStore();
const consentOpen = computed(() => system.secretStatus?.consent_required === true);
const consentError = ref("");
const version = ref("");
const RELEASES_URL = "https://github.com/thecrackofdawn/SwitchLM/releases";
function openReleases() {
  openUrl(RELEASES_URL).catch(() => {});
}
async function onConsent() {
  consentError.value = "";
  try {
    await system.grantConsent();
  } catch (e) {
    consentError.value = String(e) || "授权失败，请重试或退出。";
  }
}
async function onQuit() {
  await system.quit();
}

// Auto-follow the OS color scheme (light root + dark media query in tokens.css).
const isDark = ref(window.matchMedia("(prefers-color-scheme: dark)").matches);
let mql: MediaQueryList | null = null;
function onScheme(e: MediaQueryListEvent) {
  isDark.value = e.matches;
}
onMounted(() => {
  mql = window.matchMedia("(prefers-color-scheme: dark)");
  mql.addEventListener("change", onScheme);
  system.refresh();
  getVersion().then((v) => (version.value = v)).catch(() => {});
});
onBeforeUnmount(() => {
  mql?.removeEventListener("change", onScheme);
});

const theme = computed(() => (isDark.value ? darkTheme : null));
const overrides = computed<GlobalThemeOverrides>(() => (isDark.value ? darkOverrides : lightOverrides));

const menuOptions: MenuOption[] = [
  { label: "概览", key: "dashboard" },
  { label: "服务商", key: "sources" },
  { label: "路由", key: "profiles" },
  { label: "故障转移", key: "fallback" },
  { label: "套餐用量", key: "usage" },
  { label: "设置", key: "settings" },
];
const activeKey = computed(() => (route.name as string) ?? "dashboard");
const title = computed(() => (route.meta.title as string | undefined) ?? "SwitchLM");
function onSelect(key: string | number) {
  router.push({ name: String(key) });
}
</script>

<template>
  <NConfigProvider :theme="theme" :theme-overrides="overrides">
    <NMessageProvider>
      <NDialogProvider>
        <NModal
          :show="consentOpen"
          :mask-closable="false"
          :close-on-esc="false"
          preset="card"
          style="max-width: 480px"
          title="密钥存储授权"
        >
          <NSpace vertical :size="12">
            <NText>未检测到系统密钥环（如 gnome-keyring / KWallet / seahorse-daemon）。</NText>
            <NText>是否同意将访问密钥以<strong>明文</strong>保存到本地文件 <code>secrets.json</code>？</NText>
            <NText depth="3" style="font-size: 13px">明文存储存在泄露风险，建议安装密钥环以获得更安全的存储。</NText>
            <NText v-if="consentError" type="error" style="font-size: 13px">{{ consentError }}</NText>
            <NSpace justify="end" :size="8">
              <NButton @click="onQuit">退出</NButton>
              <NButton type="primary" @click="onConsent">同意并继续</NButton>
            </NSpace>
          </NSpace>
        </NModal>
        <NLayout has-sider class="shell">
          <NLayoutSider bordered :width="132">
            <div class="sider-col">
              <div class="brand">
                <span class="brand-mark mono">⇄</span>
                <span class="brand-name">SwitchLM</span>
              </div>
              <NMenu :options="menuOptions" :value="activeKey" :indent="18" @update:value="onSelect" />
              <div v-if="version" class="version" title="查看发布页面" @click="openReleases">v{{ version }}</div>
            </div>
          </NLayoutSider>
          <NLayout>
            <NLayoutHeader bordered class="header">{{ title }}</NLayoutHeader>
            <NLayoutContent class="content">
              <RouterView />
            </NLayoutContent>
          </NLayout>
        </NLayout>
      </NDialogProvider>
    </NMessageProvider>
  </NConfigProvider>
</template>

<style scoped>
.shell {
  height: 100vh;
}
.sider-col {
  display: flex;
  flex-direction: column;
  height: 100%;
}
.version {
  margin-top: auto;
  padding: 12px 18px;
  color: var(--sl-text-3);
  font-size: 13px;
  cursor: pointer;
  transition: color 0.15s ease;
}
.version:hover {
  color: var(--sl-accent);
  text-decoration: underline;
}
.brand {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 16px 18px 12px;
}
.brand-mark {
  color: var(--sl-accent);
  font-size: 16px;
  font-weight: 700;
}
.brand-name {
  font-weight: 700;
  font-size: 15px;
  letter-spacing: -0.01em;
}
.header {
  padding: 12px 24px;
  font-weight: 600;
  font-size: 15px;
}
.content {
  padding: 24px;
}
</style>
