import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type {
  DiscoveredModel,
  Model,
  ModelEffectiveFallback,
  Profile,
  Provider,
  RouteEffective,
  Strategy,
} from "../lib/types";
import * as api from "../lib/commands";

// Providers / models / profiles + their CRUD, discover, connection test, fallback.
export const useConfigStore = defineStore("config", () => {
  const providers = ref<Provider[]>([]);
  const models = ref<Model[]>([]);
  const profiles = ref<Profile[]>([]);
  const fallback = ref<Record<string, string | null>>({});
  const keySet = ref<Record<string, boolean>>({});
  const usageSkSet = ref<Record<string, boolean>>({});
  const cookieSet = ref<Record<string, boolean>>({});
  const usageOrder = ref<string[]>([]);
  const routeOrder = ref<string[]>([]);
  const routeEffective = ref<RouteEffective[]>([]);
  const modelEffectiveFallbacks = ref<ModelEffectiveFallback[]>([]);
  const loading = ref(false);

  async function refreshKeys() {
    const ids = providers.value.map((p) => p.id);
    const [keyResults, skResults, cookieResults] = await Promise.all([
      Promise.all(ids.map((id) => api.providerHasKey(id).catch(() => false))),
      Promise.all(ids.map((id) => api.providerHasUsageSk(id).catch(() => false))),
      Promise.all(ids.map((id) => api.providerHasUsageCookie(id).catch(() => false))),
    ]);
    keySet.value = Object.fromEntries(ids.map((id, i) => [id, keyResults[i]]));
    usageSkSet.value = Object.fromEntries(ids.map((id, i) => [id, skResults[i]]));
    cookieSet.value = Object.fromEntries(ids.map((id, i) => [id, cookieResults[i]]));
  }

  async function loadAll() {
    loading.value = true;
    try {
      [providers.value, models.value, profiles.value, fallback.value] = await Promise.all([
        api.getProviders(),
        api.getModels(),
        api.getProfiles(),
        api.getFallbackMap(),
      ]);
      usageOrder.value = await api.getUsageOrder();
      routeOrder.value = await api.getRouteOrder();
      await refreshEffective();
      await refreshKeys();
      await migrateUsageOrderIfNeeded();
      await migrateRouteOrderIfNeeded();
    } finally {
      loading.value = false;
    }
  }

  // providers
  async function saveProvider(p: Provider) {
    await api.upsertProvider(p);
    providers.value = await api.getProviders();
    await refreshKeys();
  }
  async function removeProvider(id: string) {
    await api.deleteProvider(id);
    providers.value = await api.getProviders();
    await refreshKeys();
  }
  async function setKey(providerId: string, key: string | null) {
    await api.setProviderKey(providerId, key);
    keySet.value = { ...keySet.value, [providerId]: key !== null };
  }
  async function setUsageSk(providerId: string, sk: string | null) {
    await api.setProviderUsageSk(providerId, sk);
    usageSkSet.value = { ...usageSkSet.value, [providerId]: sk !== null };
  }
  async function testConnection(id: string) {
    return api.testProviderConnection(id);
  }
  async function testConnectionWithPrompt(id: string) {
    return api.testProviderConnectionWithPrompt(id);
  }
  async function discover(id: string): Promise<DiscoveredModel[]> {
    return api.discoverModels(id);
  }

  // models
  async function saveModel(m: Model) {
    await api.upsertModel(m);
    models.value = await api.getModels();
    await refreshEffective();
  }
  async function removeModel(id: string) {
    await api.deleteModel(id);
    models.value = await api.getModels();
    await refreshEffective();
  }
  async function setFallback(modelId: string, target: string | null) {
    await api.setModelFallback(modelId, target);
    fallback.value = await api.getFallbackMap();
  }
  /** Full failover modal save: default target + time strategies + switch + cooldown.
   *  Edit resets the breaker; refresh effective fallbacks after persist (canonical re-fetch). */
  async function setModelFailover(
    modelId: string,
    fallbackTargetModelId: string | null,
    fallbackStrategies: Strategy[],
    fallbackStrategiesEnabled: boolean,
    cooldownSeconds: number | null,
    retryCount: number,
    retryDelaySecs: number,
  ) {
    await api.setModelFailover(
      modelId,
      fallbackTargetModelId,
      fallbackStrategies,
      fallbackStrategiesEnabled,
      cooldownSeconds,
      retryCount,
      retryDelaySecs,
    );
    models.value = await api.getModels();
    fallback.value = await api.getFallbackMap();
    await refreshEffective();
  }
  /** One-card switch: toggle a model's `fallback_strategies_enabled`. */
  async function setModelFallbackStrategiesEnabled(modelId: string, enabled: boolean) {
    await api.setModelFallbackStrategiesEnabled(modelId, enabled);
    models.value = await api.getModels();
    await refreshEffective();
  }

  // profiles
  async function saveProfile(p: Profile) {
    await api.upsertProfile(p);
    profiles.value = await api.getProfiles();
    await refreshEffective();
  }
  async function removeProfile(id: string) {
    await api.deleteProfile(id);
    profiles.value = await api.getProfiles();
    await refreshEffective();
  }

  async function refreshEffective() {
    [routeEffective.value, modelEffectiveFallbacks.value] = await Promise.all([
      api.getRouteEffectiveModels(),
      api.modelEffectiveFallbacks(),
    ]);
  }
  async function setStrategiesEnabled(profileId: string, enabled: boolean) {
    await api.setProfileStrategiesEnabled(profileId, enabled);
    profiles.value = await api.getProfiles();
    await refreshEffective();
  }

  /** Persist a new usage display order, then re-fetch (canonical re-fetch-after-mutate). */
  async function setUsageOrder(ids: string[]) {
    await api.setUsageOrder(ids);
    usageOrder.value = await api.getUsageOrder();
  }

  /** One-time: seed `usage_order` from the legacy localStorage drag-order key, then drop it.
   *  Idempotent — no-ops once `usage_order` is non-empty. */
  async function migrateUsageOrderIfNeeded() {
    if (usageOrder.value.length > 0) return;
    try {
      const raw = localStorage.getItem("switchlm:order:usage");
      if (!raw) return;
      const ids = JSON.parse(raw);
      if (!Array.isArray(ids) || ids.length === 0) return;
      await setUsageOrder(ids as string[]);
      localStorage.removeItem("switchlm:order:usage");
    } catch {
      // Malformed legacy key — leave it; backend order simply stays empty (config order).
    }
  }

  /** Profiles ordered by `routeOrder` rank (unlisted sink to end). Single frontend impl of the
   *  route_order semantics shared by 路由 tab, 概览 路由 card (mirrors the backend tray sort). */
  const profilesOrdered = computed(() => {
    const rank = new Map<string, number>();
    routeOrder.value.forEach((id, i) => rank.set(id, i));
    return [...profiles.value].sort((a, b) => {
      const ia = rank.get(a.id);
      const ib = rank.get(b.id);
      if (ia === undefined && ib === undefined) return 0;
      if (ia === undefined) return 1; // unlisted sink to end
      if (ib === undefined) return -1;
      return ia - ib;
    });
  });

  /** Persist a new route-list order, then re-fetch (canonical re-fetch-after-mutate). */
  async function setRouteOrder(ids: string[]) {
    await api.setRouteOrder(ids);
    routeOrder.value = await api.getRouteOrder();
  }

  /** One-time: seed `route_order` from the legacy localStorage drag-order key, then drop it.
   *  Idempotent — no-ops once `routeOrder` is non-empty. */
  async function migrateRouteOrderIfNeeded() {
    if (routeOrder.value.length > 0) return;
    try {
      const raw = localStorage.getItem("switchlm:order:profiles");
      if (!raw) return;
      const ids = JSON.parse(raw);
      if (!Array.isArray(ids) || ids.length === 0) return;
      await setRouteOrder(ids as string[]);
      localStorage.removeItem("switchlm:order:profiles");
    } catch {
      // Malformed legacy key — leave it; backend order simply stays empty (config order).
    }
  }

  return {
    providers,
    models,
    profiles,
    fallback,
    keySet,
    usageSkSet,
    cookieSet,
    usageOrder,
    routeOrder,
    routeEffective,
    modelEffectiveFallbacks,
    loading,
    profilesOrdered,
    loadAll,
    refreshKeys,
    saveProvider,
    removeProvider,
    setKey,
    setUsageSk,
    testConnection,
    testConnectionWithPrompt,
    discover,
    saveModel,
    removeModel,
    setFallback,
    setModelFailover,
    setModelFallbackStrategiesEnabled,
    saveProfile,
    removeProfile,
    refreshEffective,
    setStrategiesEnabled,
    setUsageOrder,
    setRouteOrder,
  };
});
