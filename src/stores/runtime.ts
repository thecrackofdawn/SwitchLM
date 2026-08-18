import { defineStore } from "pinia";
import { ref } from "vue";
import type { ModelHealth, ProviderStats, UsageEntry } from "../lib/types";
import * as api from "../lib/commands";

// Runtime observability: per-provider usage + per-model breaker health + usage statistics.
export const useRuntimeStore = defineStore("runtime", () => {
  const usage = ref<UsageEntry[]>([]);
  const health = ref<Record<string, ModelHealth>>({});
  const stats = ref<ProviderStats[]>([]);
  const loading = ref(false);

  async function refresh() {
    loading.value = true;
    try {
      [usage.value, health.value, stats.value] = await Promise.all([
        api.getAllUsage(),
        api.getModelHealth(),
        api.getUsageStatistics(),
      ]);
    } finally {
      loading.value = false;
    }
  }

  return { usage, health, stats, loading, refresh };
});
