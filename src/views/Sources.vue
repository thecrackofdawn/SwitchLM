<script setup lang="ts">
import { ref } from "vue";
import { NTabPane, NTabs } from "naive-ui";
import Models from "./Models.vue";
import Provider from "./Provider.vue";

// Merged "服务商": 服务商配置 (account/credentials) + 模型配置 (catalog/routing) in tabs.
const tab = ref<"provider" | "models">("provider");
</script>

<template>
  <NTabs v-model:value="tab" type="line" animated display-directive="show">
    <NTabPane name="provider" tab="服务商配置">
      <Provider />
    </NTabPane>
    <NTabPane name="models" tab="模型配置">
      <Models />
    </NTabPane>
  </NTabs>
</template>
