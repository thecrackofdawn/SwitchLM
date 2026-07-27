# 概览路由兜底模型展示 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在「概览」路由卡片中，每条路由在「当前生效」模型之后追加展示其兜底（默认）模型——仅当一条时段策略正在覆盖默认模型时显示。

**Architecture:** 纯前端改动。`RouteLine.vue` 增加一个可选 `fallbackModelLabel` prop，非空时在流水线末尾渲染一个弱化的「兜底」框节点；`Dashboard.vue` 计算该标签——仅当 `effectiveModelFor(p).id !== p.backing_model_id`（即策略覆盖了默认模型）时给出非空值，否则给空串（节点隐藏）。无后端改动。

**Tech Stack:** Vue 3 `<script setup>` + TypeScript + Naive UI；类型检查用 `vue-tsc --noEmit`。

## Global Constraints

- 包管理器是 **pnpm**（见 `pnpm-lock.yaml`）。禁用 `npm`/`npx`；用 `pnpm` / `pnpm exec`。
- 前端唯一的自动化校验是类型检查：`pnpm exec vue-tsc --noEmit`（本仓库无前端组件测试运行器）。视觉行为靠 `pnpm tauri dev` 手动验证。
- 样式只用 `--sl-*` 设计 token（见 `styles/tokens.css`），与既有 `.node` / `.tag-*` 语言保持一致。
- **提交规范**：在 `main` 上开发；**仅当用户要求时才提交**，批量提交。若派发子代理提交，必须给出精确的 `git add <paths>`，**绝不** `git add -A`（会扫进未跟踪的 docs）。本计划各任务的提交步骤给出精确路径，但执行时需等用户开口再提交。
- UI 文案为中文。「兜底」= 路由默认模型（`profile.backing_model_id`），与「路由」标签术语一致；**不是**限流回退链（`fallback_target_model_id`，本仓库叫「回退」）。

## File Structure

- **Modify** `src/components/RouteLine.vue` —— 增加可选 prop `fallbackModelLabel`；模板在「当前生效」节点后渲染可选「兜底」节点；新增 `.tag-fallback` 样式；更新顶部注释。
- **Modify** `src/views/Dashboard.vue` —— 新增 `backingLabelIfOverride(p)` 帮手；在 `<RouteLine>` 上传入 `:fallback-model-label`；更新注释。

两个文件各自单一职责，改动各自自洽：Task 1 让组件具备能力（prop 可选，不破坏现有调用），Task 2 喂数据让特性可见。

---

### Task 1: RouteLine 增加可选「兜底」节点

**Files:**
- Modify: `src/components/RouteLine.vue`（props 块 11-26 行；模板 38-50 行；样式 `.tag-primary` 后 109-112 行；顶部注释 5-10 行）

**Interfaces:**
- Produces: 新 prop `fallbackModelLabel?: string`（默认 `""`）。空串 → 不渲染兜底节点；非空 → 在「当前生效」节点后渲染 `▶` + `.node`（含 `.tag-fallback`「兜底」标签 + 模型文本）。下游 `Dashboard.vue`（Task 2）按此契约传入。

- [ ] **Step 1: 在 props 块增加 `fallbackModelLabel`**

把 `src/components/RouteLine.vue` 的 `withDefaults(defineProps<{...}>(), {...})` 改为新增一个可选 prop 及其默认值。在 `primaryModelLabel?: string;` 之后插入字段，并在默认值对象里加 `fallbackModelLabel: "",`。

修改后的 props 块：

```ts
withDefaults(
  defineProps<{
    profileName?: string;
    /** Effective model quota (remaining % / balance). null -> no badge. */
    quota?: NodeQuota | null;
    /** Effective model circuit-breaker cooling down -> amber tint + ❄. */
    cooling?: boolean;
    /** Effective model label (upstream id + provider), shown as static text. */
    primaryModelLabel?: string;
    /** Backing (default) model label, shown in a muted 兜底 node ONLY when a
     *  strategy overrides the effective model. Empty -> node hidden. */
    fallbackModelLabel?: string;
  }>(),
  {
    cooling: false,
    quota: null,
    primaryModelLabel: "",
    fallbackModelLabel: "",
  },
);
```

- [ ] **Step 2: 模板在「当前生效」节点后渲染可选兜底节点**

在「当前生效」`<div class="node node-primary">...</div>` 之后、`.pipeline` 容器闭合 `</div>` 之前，插入一个 `v-if` 片段。注意用 `<template v-if>` 包裹，使 `▶` 与 `.node` 成为 `.pipeline`（flex）的直接子节点，gap 正常生效。

修改后的模板尾部（从「当前生效」注释到 `</template>`）：

```html
    <!-- 当前生效 (strategy-aware effective model actually receiving the request) -->
    <div class="node node-primary" :class="{ cooling }">
      <span class="tag tag-primary">当前生效</span>
      <span class="val val-model">{{ primaryModelLabel || "-" }}</span>
      <span v-if="cooling" class="mark" title="冷却中">❄</span>
      <NTooltip v-if="quota" placement="top">
        <template #trigger>
          <span class="badge" :class="quota.status">{{ quota.text }}</span>
        </template>
        {{ quota.tooltip }}
      </NTooltip>
    </div>

    <!-- 兜底 (route's static backing model) — shown only when a strategy
         overrides the effective model; empty fallbackModelLabel -> hidden.
         The dynamic 429 fallback chain is still NOT shown here. -->
    <template v-if="fallbackModelLabel">
      <span class="arrow">▶</span>
      <div class="node">
        <span class="tag tag-fallback">兜底</span>
        <span class="val val-model">{{ fallbackModelLabel }}</span>
      </div>
    </template>
  </div>
</template>
```

- [ ] **Step 3: 新增 `.tag-fallback` 样式**

在 `<style scoped>` 里 `.tag-primary { ... }` 规则之后，新增 `.tag-fallback`。兜底节点复用基础 `.node`（panel 背景 + line 边框，即「路由」节点那种中性外观），仅靠标签色传达「次要/默认」语义。

新增样式（紧跟 `.tag-primary` 块之后）：

```css
.tag-fallback {
  background: var(--sl-panel-2);
  color: var(--sl-text-2);
}
```

（`--sl-panel-2`、`--sl-text-2` 均为既有 token，`Dashboard.vue` 的 `.route-card` 与多处文字已在用。）

- [ ] **Step 4: 更新顶部注释，反映兜底节点**

把 `RouteLine.vue` 顶部说明流水线与「不展示降级链」的注释，改为说明：流水线现在可含一个可选「兜底」节点（静态默认模型，策略覆盖时出现），而动态限流回退链仍不展示。

修改后的注释（替换原 5-10 行注释块）：

```ts
// Per-route pipeline (strategy-aware, time-based forwarding):
//   [路由] profile  ▶  [当前生效] effective model  ▶  [兜底] backing model (optional)
// Read-only here - switching happens on the 路由/模型 tabs. The 兜底 node shows the
// route's STATIC backing model, and only when a strategy overrides the effective model.
// The DYNAMIC 429 fallback chain (model.fallback_target_model_id) is intentionally NOT
// shown here (breaker state makes it situational). Quota badge on 当前生效 shows REMAINING
// (plan %) / balance (consumption), health-colored - same number/source/thresholds as the
// 可用额度 chips above. Styles use --sl-* tokens.
```

- [ ] **Step 5: 类型检查**

Run: `pnpm exec vue-tsc --noEmit`
Expected: 通过，无错误。（`fallbackModelLabel` 可选且默认 `""`，现有 `<RouteLine>` 调用不传它也不报错。）

- [ ] **Step 6: 提交（当用户要求时）**

仅当用户要求提交时执行；给出精确路径：

```bash
git add src/components/RouteLine.vue
git commit -m "feat(ui): RouteLine 增加可选兜底节点（策略覆盖默认模型时显示）"
```

---

### Task 2: Dashboard 计算并传入兜底标签

**Files:**
- Modify: `src/views/Dashboard.vue`（帮手函数区 39-44 行后；`<RouteLine>` 用法 223-230 行；注释 31-33 行）

**Interfaces:**
- Consumes: Task 1 的 `RouteLine` prop `fallbackModelLabel?: string`。
- Produces: 新帮手 `backingLabelIfOverride(p: Profile | undefined): string` —— 当 `effectiveModelFor(p)?.id !== p.backing_model_id` 时返回兜底模型标签，否则空串。

- [ ] **Step 1: 新增 `backingLabelIfOverride` 帮手**

在 `src/views/Dashboard.vue` 的 `modelLabelOf` 函数（约 39-44 行）之后，新增帮手。它复用既有 `effectiveModelFor` 与 `modelLabelOf`。

新增代码（紧跟 `modelLabelOf` 之后）：

```ts
/** 兜底模型标签：仅当「当前生效」被策略覆盖（与 profile 的 backing_model_id 不同）
 *  时返回兜底模型标签；否则返回空串（RouteLine 的兜底节点随之隐藏，避免与当前生效重复）。 */
function backingLabelIfOverride(p: Profile | undefined): string {
  if (!p) return "";
  const eff = effectiveModelFor(p);
  if (!eff || eff.id === p.backing_model_id) return "";
  const backing = config.models.find((m) => m.id === p.backing_model_id);
  return modelLabelOf(backing);
}
```

- [ ] **Step 2: 在 `<RouteLine>` 上传入兜底标签**

在「路由」卡片的 `<RouteLine v-for="p in config.profilesOrdered" ... />` 上增加 `:fallback-model-label` 绑定。

修改后的 `<RouteLine>`（约 223-230 行）：

```html
        <RouteLine
          v-for="p in config.profilesOrdered"
          :key="p.id"
          :profile-name="p.name"
          :quota="nodeQuotaForProfile(p)"
          :cooling="coolingFor(p)"
          :primary-model-label="modelLabelOf(effectiveModelFor(p))"
          :fallback-model-label="backingLabelIfOverride(p)"
        />
```

- [ ] **Step 3: 更新注释，澄清兜底展示**

把 `effectiveModelFor` 上方注释里「降级链不在概览展示（策略使链路随时间变化）」一句，改为：静态兜底模型在策略覆盖时会展示，仅动态限流回退链仍隐藏。

修改后的注释（替换原 31-33 行注释块）：

```ts
/** 概览展示全部路由：以下按 profile 计算各自的 当前生效模型 / 用量 / 熔断。
 *  Strategy-aware effective model: prefers the time-based effective_model_id,
 *  falls back to the profile's static backing_model_id. 当策略覆盖默认模型时，额外展示
 *  静态「兜底」模型（backing_model_id）；仅动态限流回退链（fallback_target_model_id）
 *  仍不在概览展示（熔断状态使其随时间变化）。 */
```

- [ ] **Step 4: 类型检查**

Run: `pnpm exec vue-tsc --noEmit`
Expected: 通过，无错误。

- [ ] **Step 5: 手动视觉验证**

Run: `pnpm tauri dev`，进入「概览」页「路由」卡片，逐项验证：

1. **无策略路由**：流水线为 `[路由] ▶ [当前生效]`，**无**兜底节点。
2. **有策略且当前命中**（如工作日白天策略切到另一模型）：兜底节点出现，显示兜底模型名（含服务商全角括号）；「当前生效」显示策略模型。两者不同。
3. **有策略但当前未命中，或策略停用**：兜底节点隐藏（`effective == backing`）。
4. **兜底与生效属不同服务商**：两个节点各自正确显示服务商名。
5. **样式**：兜底节点为中性框（panel 底 + 灰边），「兜底」标签为弱化色，明显次于「当前生效」的 accent 强调。
6. **窄窗口**：流水线优雅换行；启动时窗口自适应已含更宽流水线（`fitWindowToRoutes` 按实际渲染宽度测量，无需改）。

Expected: 全部通过。

- [ ] **Step 6: 提交（当用户要求时）**

仅当用户要求提交时执行；给出精确路径：

```bash
git add src/views/Dashboard.vue
git commit -m "feat(ui): 概览路由在策略覆盖时于当前生效后展示兜底模型"
```

---

## Self-Review

**1. Spec coverage：**
- 「展示兜底 = backing_model_id」→ Task 2 Step 1 (`config.models.find(m => m.id === p.backing_model_id)`)。✓
- 「仅当 effective != backing 时展示」→ Task 2 Step 1 (`if (!eff || eff.id === p.backing_model_id) return ""`)。✓
- 「仅模型标签，无额度/冷却」→ Task 1 Step 2（兜底节点只有 tag + 文本，无 badge/❄）。✓
- 「弱化框节点 + muted tag」→ Task 1 Step 2（`class="node"` 复用中性外观）+ Step 3（`.tag-fallback`）。✓
- 「复用 modelLabelOf / 全角括号」→ Task 2 Step 1 (`return modelLabelOf(backing)`)。✓
- 「更新两处过时注释」→ Task 1 Step 4 + Task 2 Step 3。✓
- 「fitWindowToRoutes 自适应，无需改」→ Task 2 Step 5 验证项 6 + Global Constraints 说明。✓
- 边界（无命中→隐藏；策略模型==兜底→隐藏；兜底被删→`modelLabelOf(undefined)` 返 `"-"`）→ Task 2 Step 1 的判定覆盖前两项；`modelLabelOf` 既有行为覆盖第三项（见 Dashboard.vue:40-44）。✓

**2. Placeholder scan：** 无 TBD/TODO；每个代码步骤都给了完整代码；手动验证给了具体场景与期望。✓

**3. Type consistency：**
- prop 名：Task 1 定义 `fallbackModelLabel`（camel），Task 2 模板用 `:fallback-model-label`（kebab → camel 自动映射，与既有 `:primary-model-label` 一致）。✓
- 帮手签名：Task 2 Step 1 `backingLabelIfOverride(p: Profile | undefined): string`，Step 2 调用 `backingLabelIfOverride(p)`，`p` 来自 `v-for="p in config.profilesOrdered"`（`Profile[]`）。✓
- `effectiveModelFor` 返回 `Model | undefined`，`.id` 访问前用 `!eff` 守卫。✓

无问题，计划可执行。
