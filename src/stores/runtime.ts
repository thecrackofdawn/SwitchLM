import { defineStore } from "pinia";
import { ref } from "vue";
import type { ModelHealth, UsageEntry } from "../lib/types";
import * as api from "../lib/commands";

// Runtime observability: per-provider usage + per-model breaker health.
export const useRuntimeStore = defineStore("runtime", () => {
  const usage = ref<UsageEntry[]>([]);
  const health = ref<Record<string, ModelHealth>>({});
  const loading = ref(false);

  async function refresh() {
    loading.value = true;
    try {
      [usage.value, health.value] = await Promise.all([api.getAllUsage(), api.getModelHealth()]);
    } finally {
      loading.value = false;
    }
  }

  return { usage, health, loading, refresh };
});
