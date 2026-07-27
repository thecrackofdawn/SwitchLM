# 概览路由：兜底模型展示

**日期**：2026-08-03
**范围**：仅前端（`src/views/Dashboard.vue` + `src/components/RouteLine.vue`），无后端改动
**状态**：已评审待实现

## 目标

在「概览」页面的「路由」卡片中，每条路由流水线在「当前生效」模型之后追加展示该路由的**兜底模型**（profile 的默认/回退模型），让用户一眼看到「当前正在跑的模型」与「该路由的默认模型」的差异。

## 背景与术语澄清

本仓库存在两个互不相同、容易混淆的「回退」概念：

| 概念 | 字段 | 含义 | 在哪里叫什么 |
|---|---|---|---|
| **路由默认模型** | `profile.backing_model_id` | 请求在「没有时段策略命中」时的起点模型 | 路由标签里叫「兜底模型（默认/回退）」 |
| **限流回退链** | `model.fallback_target_model_id` | 当前模型被 429/冷却后 `dispatch` 切到的下一个模型 | Fallback 标签里叫「回退模型」 |

请求实际链路（`proxy/strategies.rs::profile_start_model` + `proxy/dispatch.rs::next_fallback_id`）：

```
profile_start_model() → 当前生效（策略命中→策略模型；否则→兜底模型）
                     → 限流时沿 model.fallback_target_model_id 链向下走
```

本设计展示的是**前者**：路由的默认模型（`backing_model_id`），与「路由」标签里的「兜底模型」术语一致。

### 为什么不违反现有「概览不展示降级链」的设计

`RouteLine.vue` / `Dashboard.vue` 现有注释写道：「降级链不在概览展示（策略使链路随时间变化）」。那条顾虑针对的是**动态限流回退链**——它会随熔断器状态实时变化，展示会误导。而兜底模型是**静态**的（配置即定，不随运行时熔断状态变化），展示它是稳定且有意义的。因此本设计与该顾虑不冲突；动态 429 链仍然不在概览展示。

## 行为

每条路由流水线变为：

```
[路由] profile  ▶  [当前生效] 生效模型(+额度角标/❄)  ▶  [兜底] 兜底模型
```

**展示条件**：仅当「当前生效模型」与「兜底模型」**不同**时展示兜底节点。两者相同的情形（无策略、或策略已停用、或当前无策略命中、或策略模型恰好等于兜底模型）下，兜底节点隐藏，避免重复。

判定以模型 id 为准：`effectiveModelFor(p)?.id !== p.backing_model_id`。这恰好等价于「当前有一条生效策略正在覆盖默认模型」，因此兜底节点的出现本身就传达了「此刻有策略在生效」。

**节点内容**：仅模型标签（`upstream_model_id（服务商）`，全角括号），复用 Dashboard 既有的 `modelLabelOf`。不带额度角标、不带冷却 ❄——兜底模型不是当前在跑的模型，保持轻量、避免被误读为活跃模型。

## 视觉设计

兜底节点采用**弱化框节点**，与概览既有的「框装节点」语言一致，但明显次于「当前生效」：

- 复用基础 `.node` 样式（`--sl-panel` 背景 + `--sl-line` 边框 + `--sl-ink` 文字），即路由节点那种中性外观，**不**用 `.node-primary` 的 accent 强调色。
- 新增 `.tag-fallback` 标签：弱化/中性配色（如 `--sl-text-2` 字色 + 中性背景），与 `.tag-primary`（accent 底白字）和 `.tag-route`（accent-weak/accent）区分开，传达「次要/默认」语义。
- 模型标签复用 `.val-model`（含 160px 省略号截断），保证与「当前生效」节点宽度行为一致。

示意（当前生效=glm-4.5，兜底=glm-4.6）：

```
┌─路由──┐  ┌──当前生效──────────┐  ┌──兜底──────────┐
│glm-5.2│▶ │glm-4.5（智谱）62%❄ │▶ │glm-4.6（智谱）  │
└───────┘  └────────────────────┘  └─────────────────┘
 accent      accent-weak+badge         panel+灰边（雅淡）
```

## 数据流与组件改动

### `RouteLine.vue`

- 新增可选 prop：`fallbackModelLabel?: string`（默认 `""`）。
- 模板：当 `fallbackModelLabel` 非空时，在「当前生效」节点后渲染 `▶` + 一个 `.node`（带 `.tag-fallback`「兜底」标签 + `.val-model` 标签文本）。
- 更新顶部注释：说明兜底（静态默认）模型现在会展示，仅动态限流回退链仍隐藏。

### `Dashboard.vue`

- 已有 `effectiveModelFor(p)` 与 `modelLabelOf(m)`，无需新增函数。
- 在模板的 `<RouteLine>` 上：计算兜底模型标签——
  - `const eff = effectiveModelFor(p)`
  - 当 `eff && eff.id !== p.backing_model_id` 时，传入 `:fallback-model-label="modelLabelOf(config.models.find(m => m.id === p.backing_model_id))"`；否则传 `""`（或不传，走默认）。
- 为可读性，建议抽一个小帮手 `backingLabelIfOverride(p): string`，封装上述判定。
- 更新 `effectiveModelFor` 上方注释里「降级链不在概览展示」一句，澄清静态兜底模型会展示。

### 窗口自适应

`fitWindowToRoutes` 在挂载时测量 `.route-card .pipeline` 的实际渲染宽度并撑大窗口；兜底节点加宽了流水线，但该函数已按实际宽度自适应，且 `.pipeline` 带 `flex-wrap: wrap`，窄窗口下自动换行。无需改动。

## 边界情况

- **兜底模型被删除但仍被 profile 引用**：`config.models.find(...)` 返回 `undefined` → `modelLabelOf` 返回 `"-"`。与「当前生效」节点找不到模型时的行为一致。
- **策略启用但当前无命中**：`profile_start_model` 回落到 `backing_model_id`，`effective.id === backing_model_id` → 兜底节点隐藏。正确（此刻确实没有覆盖）。
- **策略模型恰好等于兜底模型**：同上，隐藏。正确。
- **多条策略**：与展示无关——只看「当前生效」最终解析到的模型 id。

## 不在本范围内

- 限流回退链（`fallback_target_model_id`）的展示——仍不在概览展示。
- 兜底节点的额度角标 / 冷却状态。
- 在兜底节点上标记「策略生效」的 ⏰ 记号——当前生效节点本身未带该记号，保持一致；兜底节点的出现已隐含此意。如后续需要可单独追加。

## 测试

- 前端类型检查：`pnpm exec vue-tsc --noEmit` 通过（新 prop 类型正确）。
- 手动验证（`pnpm tauri dev`）：
  1. 无策略路由 → 兜底节点不出现。
  2. 有策略且当前命中 → 兜底节点出现，显示兜底模型名；当前生效显示策略模型。
  3. 有策略但当前未命中（或策略停用）→ 兜底节点隐藏。
  4. 兜底模型与生效模型属不同服务商 → 标签正确显示各自服务商。
  5. 窄窗口下流水线优雅换行，窗口启动自适应当宽度增加。
