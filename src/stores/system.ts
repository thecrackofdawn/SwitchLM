import { defineStore } from "pinia";
import { ref } from "vue";
import type { EnvSnippet, SecretStatusView, ServerStatus, SettingsView } from "../lib/types";
import * as api from "../lib/commands";

// Proxy server status / port / env snippet / settings / restart / quit / autostart.
export const useSystemStore = defineStore("system", () => {
  const status = ref<ServerStatus | null>(null);
  const envSnippet = ref<EnvSnippet | null>(null);
  const settings = ref<SettingsView | null>(null);
  const refreshing = ref(false);
  // Last port-bind error, surfaced so the UI can show "port occupied,
  // auto-retrying" instead of a silent failure. `null` when there is no error.
  const bindError = ref<string | null>(null);
  // Secret-store backend status — drives the global consent modal on Linux
  // when no keyring is available (`consent_required` is true in `pending`).
  const secretStatus = ref<SecretStatusView | null>(null);

  async function refresh() {
    refreshing.value = true;
    try {
      const [statusResult, errorResult] = await Promise.all([
        api.getServerStatus(),
        api.getBindError(),
      ]);
      status.value = statusResult;
      bindError.value = errorResult;
      // 授权状态独立拉取：它的失败不得连累 server-status / bindError（后者是安全关键提示）。
      try {
        secretStatus.value = await api.getSecretStatus();
      } catch {
        // 授权门不可用——不显示弹窗，应用其余功能仍可使用。
      }
    } finally {
      refreshing.value = false;
    }
  }

  async function loadSecretStatus() {
    secretStatus.value = await api.getSecretStatus();
  }
  async function grantConsent() {
    await api.grantSecretConsent();
    await loadSecretStatus(); // swap 后 mode→file, consent_required→false
  }

  async function loadEnvSnippet() {
    envSnippet.value = await api.getEnvSnippet();
  }

  async function loadSettings() {
    settings.value = await api.getSettings();
  }

  async function savePort(port: number) {
    await api.setPort(port);
    await loadSettings();
  }

  async function saveUsageRefreshInterval(seconds: number) {
    await api.setUsageRefreshInterval(seconds);
    await loadSettings();
  }

  async function saveLogLevel(level: string) {
    await api.setLogLevel(level);
    await loadSettings();
  }

  async function saveRequestRecording(enabled: boolean) {
    await api.setRequestRecording(enabled);
    await loadSettings();
  }

  async function saveBackgroundDestroy(enabled: boolean) {
    await api.setBackgroundDestroy(enabled);
    await loadSettings();
  }

  async function clearRequestLog() {
    await api.clearRequestLog();
  }

  async function openLogDir() {
    await api.openLogDir();
  }

  async function restart() {
    await api.restartServer();
    await Promise.all([refresh(), loadEnvSnippet()]);
  }

  async function quit() {
    await api.quitApp();
  }

  async function setAutostart(enabled: boolean) {
    await api.toggleAutostart(enabled);
    await loadSettings();
  }

  return {
    status,
    envSnippet,
    settings,
    refreshing,
    bindError,
    secretStatus,
    refresh,
    loadSecretStatus,
    grantConsent,
    loadEnvSnippet,
    loadSettings,
    savePort,
    saveUsageRefreshInterval,
    saveLogLevel,
    saveRequestRecording,
    saveBackgroundDestroy,
    clearRequestLog,
    openLogDir,
    restart,
    quit,
    setAutostart,
  };
});
