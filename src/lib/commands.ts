// Typed Tauri invoke wrappers for every spec §7.3 command.
// Tauri converts camelCase JS arg keys -> snake_case Rust params.
import { invoke } from "@tauri-apps/api/core";
import type {
  ConnectionTest,
  ContextCheckResult,
  DiscoveredModel,
  EnvSnippet,
  Model,
  ModelEffectiveFallback,
  ModelHealth,
  Profile,
  Provider,
  RouteEffective,
  SecretStatusView,
  ServerStatus,
  SettingsView,
  Strategy,
  UsageEntry,
  UsageSnapshot,
} from "./types";

// ---- config reads ----
export const getProviders = () => invoke<Provider[]>("get_providers");
export const getModels = () => invoke<Model[]>("get_models");
export const getProfiles = () => invoke<Profile[]>("get_profiles");
export const getUsageOrder = () => invoke<string[]>("get_usage_order");
export const setUsageOrder = (ids: string[]) => invoke<void>("set_usage_order", { ids });
export const getRouteOrder = () => invoke<string[]>("get_route_order");
export const setRouteOrder = (ids: string[]) => invoke<void>("set_route_order", { ids });

// ---- CRUD (persist after write) ----
export const upsertProvider = (provider: Provider) => invoke<void>("upsert_provider", { provider });
export const deleteProvider = (providerId: string) => invoke<void>("delete_provider", { providerId });
export const upsertModel = (model: Model) => invoke<void>("upsert_model", { model });
export const deleteModel = (modelId: string) => invoke<void>("delete_model", { modelId });
export const upsertProfile = (profile: Profile) => invoke<void>("upsert_profile", { profile });
export const deleteProfile = (profileId: string) => invoke<void>("delete_profile", { profileId });

// ---- strategies (time-based forwarding) ----
export const getRouteEffectiveModels = () => invoke<RouteEffective[]>("route_effective_models");
export const setProfileStrategiesEnabled = (profileId: string, enabled: boolean) =>
  invoke<void>("set_profile_strategies_enabled", { profileId, enabled });

// ---- fallback ----
export const getFallbackMap = () => invoke<Record<string, string | null>>("get_fallback_map");
export const setModelFallback = (modelId: string, target: string | null) =>
  invoke<void>("set_model_fallback", { modelId, target });
// Full failover config (modal save): default target + time strategies + switch + cooldown.
// Edit resets the breaker (health), so this is not just a partial update of setModelFallback.
export const setModelFailover = (
  modelId: string,
  fallbackTargetModelId: string | null,
  fallbackStrategies: Strategy[],
  fallbackStrategiesEnabled: boolean,
  cooldownSeconds: number | null,
  retryCount: number,
  retryDelaySecs: number,
) =>
  invoke<void>("set_model_failover", {
    modelId,
    fallbackTargetModelId,
    fallbackStrategies,
    fallbackStrategiesEnabled,
    cooldownSeconds,
    retryCount,
    retryDelaySecs,
  });
export const setModelFallbackStrategiesEnabled = (modelId: string, enabled: boolean) =>
  invoke<void>("set_model_fallback_strategies_enabled", { modelId, enabled });
export const modelEffectiveFallbacks = () =>
  invoke<ModelEffectiveFallback[]>("model_effective_fallbacks");
export const validateFallbackContext = (primaryId: string, fallbackId: string) =>
  invoke<ContextCheckResult>("validate_fallback_context", { primaryId, fallbackId });
export const recognizedContextSize = (providerId: string, upstreamModelId: string) =>
  invoke<number | null>("recognized_context_size", { providerId, upstreamModelId });

/** Set/clear a custom context-size override for a (provider, upstream_model) pair.
 *  Writes custom_provider_desc.json + updates the in-memory catalog (real-time, same-vendor).
 *  `null` clears the override -> reverts to the bundled default. */
export const setCustomContextSize = (
  providerId: string,
  upstreamModelId: string,
  contextSize: number | null,
) => invoke<void>("set_custom_context_size", { providerId, upstreamModelId, contextSize });

// ---- discover + connection test ----
export const discoverModels = (providerId: string) =>
  invoke<DiscoveredModel[]>("discover_models", { providerId });
export const testProviderConnection = (providerId: string) =>
  invoke<ConnectionTest>("test_provider_connection", { providerId });
export const testProviderConnectionWithPrompt = (providerId: string) =>
  invoke<ConnectionTest>("test_provider_connection_with_prompt", { providerId });
export const setProviderKey = (providerId: string, apiKey: string | null) =>
  invoke<void>("set_provider_key", { providerId, apiKey });
export const providerHasKey = (providerId: string) =>
  invoke<boolean>("provider_has_key", { providerId });
// Volcengine usage-query SK (stored in the OS keyring, not config).
export const setProviderUsageSk = (providerId: string, secretAccessKey: string | null) =>
  invoke<void>("set_provider_usage_sk", { providerId, secretAccessKey });
export const providerHasUsageSk = (providerId: string) =>
  invoke<boolean>("provider_has_usage_sk", { providerId });
// 千问 (Qianwen / qianwenai) console-cookie login. An in-app WebView window captures the HttpOnly
// session cookie for usage queries; the cookie is stored backend-side (keyring-first). The
// frontend only observes presence + a status event (the cookie value never crosses IPC).
export const openQianwenLogin = (providerId: string) =>
  invoke<void>("open_qianwen_login", { providerId });
export const finishQianwenLogin = (providerId: string) =>
  invoke<void>("finish_qianwen_login", { providerId });
export const providerHasUsageCookie = (providerId: string) =>
  invoke<boolean>("provider_has_usage_cookie", { providerId });
export const clearQianwenCookie = (providerId: string) =>
  invoke<void>("clear_qianwen_cookie", { providerId });

// ---- usage + health ----
export const getUsage = (providerId: string) => invoke<UsageSnapshot>("get_usage", { providerId });
export const getAllUsage = () => invoke<UsageEntry[]>("get_all_usage");
export const getModelHealth = () => invoke<Record<string, ModelHealth>>("get_model_health");

// ---- server / app ----
export const getServerStatus = () => invoke<ServerStatus>("get_server_status");
export const getPort = () => invoke<number | null>("get_port");
// Last port-bind error, if the proxy failed to bind its preferred port and is
// retrying. `null` once a bind succeeds (or no bind has been attempted).
export const getBindError = () => invoke<string | null>("get_bind_error");
export const getEnvSnippet = () => invoke<EnvSnippet>("get_env_snippet");
export const restartServer = () => invoke<number | null>("restart_server");
export const quitApp = () => invoke<void>("quit_app");
export const toggleAutostart = (enabled: boolean) => invoke<boolean>("toggle_autostart", { enabled });
export const getSettings = () => invoke<SettingsView>("get_settings");
export const setPort = (port: number) => invoke<void>("set_port", { port });
export const setUsageRefreshInterval = (seconds: number) =>
  invoke<void>("set_usage_refresh_interval", { seconds });
export const setLogLevel = (level: string) => invoke<void>("set_log_level", { level });
export const openLogDir = () => invoke<void>("open_log_dir");

// ---- secret store / consent (Linux fallback) ----
export const getSecretStatus = () => invoke<SecretStatusView>("get_secret_status");
export const grantSecretConsent = () => invoke<void>("grant_secret_consent");

// ---- provider conflict check ----
// Mirrors src-tauri `ProviderConflict { id, display_name }`. Returns null when
// the vendor + credentials don't collide with an existing provider.
export interface ProviderConflict {
  id: string;
  display_name: string;
}

export async function checkProviderConflict(
  vendor: string,
  accessKeyId?: string | null,
  apiKey?: string | null,
): Promise<ProviderConflict | null> {
  return invoke<ProviderConflict | null>("check_provider_conflict", {
    vendor,
    accessKeyId: accessKeyId ?? null,
    apiKey: apiKey ?? null,
  });
}
