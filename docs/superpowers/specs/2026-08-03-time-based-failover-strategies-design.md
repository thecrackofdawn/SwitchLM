# 时段故障转移策略（Time-based Failover Strategies）+ 呈现统一 — 设计规格

> 日期：2026-08-03
> 状态：设计完成（待评审）
> 关联：主规格 `2026-07-27-switchlm-llm-proxy-design.md`（§3 请求流、§4 熔断/降级）；
> 姊妹篇 `2026-08-03-time-based-forwarding-strategies-design.md`（入口层时段策略，本文**复用其全部原语**）

## 1. 背景与目标

入口层时段策略（姊妹篇）让路由能按时间选**入口**模型。本规格把同一套"按时段选模型"的能力扩展到**故障转移层**：当入口模型 429/用量故障时，下一个模型也按时段选择；并把路由页 / 故障转移页 / 概览的呈现统一，让"当前生效的是谁、出错会转给谁"一眼可见。

**核心场景**：先用 GLM 套餐（避开 14:00–18:00 高倍率——入口层已支持）；GLM 一旦故障，**22:00 后先切 qwen（夜间打折），其余时间切火山**。

**目标**：
1. 故障转移目标从单一静态 `fallback_target_model_id` 扩展为"默认目标 + 时段策略覆盖"，与入口层 `backing_model_id + strategies` 完全对称。
2. 故障转移页**整体镜像路由页**（说明行 + 新增 + 卡片列表 + 弹窗增删改），卡片直接显示"当前生效的转移模型"。
3. 路由卡片 / 故障转移卡片 / 概览管线**统一用"当前生效"**措辞，清理"兜底/回退/故障转移"词汇撞车。

**成功标准**：入口模型限流/熔断 → 沿时间感知的故障转移链**逐级**降级，每一级用**该级模型自己**的故障转移策略（GLM 故障→按 GLM 的策略转 qwen；qwen 再故障→按 qwen 自己的策略转它的目标→…），直到成功或链耗尽；无策略命中/总开关关 → 走默认 `fallback_target_model_id`；路由卡 / 故障转移卡 / 概览都能实时看到"当前生效"的入口与转移模型。

## 2. 设计原则与关键决策

| 决策 | 结论 | 理由 |
|---|---|---|
| 故障转移策略归属 | **per-Model**（`Model.fallback_strategies`） | 与既有 per-model 反应式链一致；同一模型用于多路由时故障转移自动共享；链式降级天然成立。 |
| 数据结构 | 复用 `Strategy`/`TimeStrategy`，**零新类型** | 与入口层同构；`strategy_matches`/`select_strategy` 直接复用。 |
| 默认目标 | 保留 `fallback_target_model_id` 作"默认转移目标" | 对称于入口层 `backing_model_id`；向后兼容。 |
| 总开关 | `Model.fallback_strategies_enabled` | 一键回退到默认转移目标而不删策略（对称入口层 `strategies_enabled`）。 |
| 注入位置 | dispatch 的 `next_fallback_id` 时间感知 | 链式降级/冷却跳过/防环**零改动**，只改"下一个是谁"。 |
| **链式转移语义** | 每跳读**当前失败模型自己**的故障转移配置，逐级递归（GLM→qwen→qwen 的目标→…） | 每个模型只声明"我故障后按我的策略转给谁"，链由 dispatch 逐级串起；与既有 per-model 链一致。 |
| 弹窗持久化 | 新命令 `set_model_failover` 一次存默认目标+策略+开关+冷却 | 原子、聚焦；不走 `upsert_model`（避免重名冲突检查/整模型拼装）。 |
| 卡片开关 | 轻量命令 `set_model_fallback_strategies_enabled` | 仿 `set_profile_strategies_enabled`；卡片一键切换不必开弹窗。 |
| 生效值来源 | **Rust 单一真相**：`route_effective_models`（入口+转移）+ 新 `model_effective_fallbacks`（per-model 转移） | 已时段化，前端不得本地算时间（避免漂移）。 |
| 故障转移页形态 | **镜像路由页**：说明行+`[+新增]`，卡片列表，弹窗增删改 | 与路由页心智一致；卡片直接显示"当前生效的转移模型"。 |
| 故障转移列表范围 | **只列已配置**的模型；新增弹窗的模型下拉**只给未配置**的 | 镜像路由页"显式增删"；避免全量模型铺满。 |
| 卡片内容 | `模型 → 当前生效转移模型 → [策略N条+一键开关] → 编辑 → 删除`；冷却/默认目标**收进弹窗** | 行只做展示+入口，与路由页一致。 |
| 生效模型措辞 | **统一"当前生效"**（路由卡 / 故障转移卡 / 概览管线） | 消除"转发模型/当前生效/当前"多叫法。 |
| 术语切割 | 路由侧"兜底模型"→"**默认模型**"；"故障转移/回退"专属反应式 | 消除两套"兜底/回退"词汇撞车。 |
| 策略编辑器 | 抽共享组件 `StrategyEditor.vue` | 入口弹窗与故障转移弹窗复用，消除重复。 |

## 3. 数据结构（`config/types.rs`）

`Model` 增两字段，均 `#[serde(default)]`，向后兼容（旧配置无这两字段 → `[]`/`true`，即今日行为）：

```rust
pub struct Model {
    // ... 既有字段 ...
    /// 默认转移目标（= 入口层 backing_model_id 的对应物）。
    pub fallback_target_model_id: Option<String>,
    /// 时段故障转移策略（复用 Strategy，与入口层同构）。
    #[serde(default)]
    pub fallback_strategies: Vec<Strategy>,
    /// 总开关：false 跳过时段策略，直接走 fallback_target_model_id。默认 true。
    #[serde(default = "default_fb_strategies_enabled")]
    pub fallback_strategies_enabled: bool,
}
fn default_fb_strategies_enabled() -> bool { true }
```

复用入口层已定义的 `Strategy { id, priority, enabled, kind: StrategyKind::Time(TimeStrategy) }` 与
`TimeStrategy { days_of_week, time_start, time_end, model_id }` —— 本规格**不新增任何类型**。

**"已配置故障转移"的判定**（前端列表/新增下拉用）：`fallback_target_model_id.is_some() || !fallback_strategies.is_empty()`。

### 3.1 持久化

- **位置**：路由（`profiles`）与故障转移（`models` 的 `fallback_*` 字段）同属 `AppConfig`，持久化到 Tauri `app_data_dir` 下的 `app_config.json`（Windows：`%APPDATA%\com.switchlm.app\app_config.json`，identifier 见 `tauri.conf.json`）。密钥（推理 key、用量 SK）**不**在此文件，走 OS keyring（service `SwitchLM`）。
- **写入**：每个变更命令经 `commands.rs::persist()` → `config/store.rs::save()` 整体重写该文件。新增的 `fallback_strategies` / `fallback_strategies_enabled` 挂在 `Model` 上、随 `AppConfig` **自动落盘，无需新持久化代码**。
- **向后兼容**：两新字段均 `#[serde(default)]`，旧配置加载即得 `[]`/`true`（见 §7 serde 往返测试）。

## 4. 选择与 dispatch

### 4.1 纯函数 `model_fallback_target`（`proxy/strategies.rs`）

`profile_start_model` 的故障转移对应物（单一真相）：

```rust
/// 模型当前的转移目标 id + 命中的转移策略 id。
/// 总开关关 / 无命中 / 策略目标已删 → fallback_target_model_id（via=None）；
/// 无默认目标 → (None, None)。
pub fn model_fallback_target<'c>(
    model: &'c Model,
    cfg: &'c AppConfig,
    now: &LocalNow,
) -> (Option<&'c str /*target_id*/>, Option<&'c str /*strategy_id*/>);
```

逻辑与 `profile_start_model` 同构：`fallback_strategies_enabled` 且 `select_strategy` 命中且目标存在 → `(Some(策略 model_id), Some(strategy.id))`；否则退回 `(fallback_target_model_id, None)`。实现上可与 `profile_start_model` 抽出共用的泛型选择器以去重。

### 4.2 dispatch 时间感知 —— 链式转移

`next_fallback_id(state, model_id)` → `next_fallback_id(state, model_id, now: &LocalNow)`，内部改用 `model_fallback_target`。`dispatch` 顶部已有 `let now = state.clock.now_local();`，将其线程化传入 `dispatch_non_stream`/`dispatch_stream`，再传入 `next_fallback_id`。

**链式转移（核心语义，务必明确）**：每一跳的"下一个"由**当前失败模型自己**的故障转移配置决定——`next_fallback_id(current)` 调 `model_fallback_target(current_model)`，读的是 `current_model` 的 `fallback_strategies` / `fallback_target_model_id`，**不是**入口路由、也不是任何全局配置。因此转移是**逐级递归**的：

```
GLM 故障   →(GLM 自己的策略: 22:00后→qwen, 其余→火山)→  qwen
qwen 故障  →(qwen 自己的策略/默认转移目标)→  下一个模型
下一个故障 →(它自己的策略/默认)→  …
直到某模型成功，或链耗尽（visited-set 防环 + MAX_FALLBACK_HOPS=8 兜底）
```

即：**每个模型只负责声明"我故障后按我的策略转给谁"，链由 dispatch 逐级串起来**。给 qwen 配的策略只在"qwen 故障"这一跳生效，与 GLM 的策略互不影响；qwen 没配策略时就走 qwen 的默认转移目标，若连默认也没有则链在此终止。

`now` 在 `dispatch` 顶部取一次并贯穿整条链——**同一请求内所有跳共用同一时间快照**，一次请求的链式判定确定一致（不会中途因跨过时间点而变）。

**链式降级、冷却跳过、visited-set 防环、`MAX_FALLBACK_HOPS` 全部不变**——只是每一跳的"下一个"由时间感知选择。时间感知不破坏环检测（visited 按 model id 去重；某时刻成环仍在单请求内被捕获）。

### 4.3 命令（`commands.rs`）

- **新增** `set_model_failover(model_id, fallback_target_model_id, fallback_strategies, fallback_strategies_enabled, cooldown_seconds)`：故障转移弹窗的保存入口。泛化校验（§5）→ 一次写入默认目标/策略/开关/冷却 → `persist` → `health.reset(model_id)`。
- **新增** `set_model_fallback_strategies_enabled(model_id, enabled)`：卡片"一键开关"轻量切换（仿 `set_profile_strategies_enabled`），`persist` 即返回。
- **新增** `model_effective_fallbacks() -> Vec<ModelEffectiveFallback { model_id, effective_fallback_model_id: Option<String>, via_fallback_strategy_id: Option<String> }>`：对每个模型调 `model_fallback_target`，供故障转移卡片显示"当前生效的转移模型"。
- **扩展** `RouteEffective` 增 `effective_fallback_model_id: Option<String>` + `via_fallback_strategy_id: Option<String>`；`route_effective_models` 在算出入口模型后追加 `model_fallback_target(entry_model, cfg, now)`，供概览管线"故障转移"节点使用。
- **泛化** `normalize_and_validate_strategies`：从作用于 `Profile.strategies` 泛化为作用于任意 `&mut Vec<Strategy>`（策略 `model_id` 存在性校验需 `&AppConfig`）；`upsert_profile` 与 `set_model_failover` 共用。

## 5. 校验与边界

- 策略校验与入口层完全同款：`priority ∈ 1..=10`；`days_of_week` 非空且每个值 `∈ 1..=7`（保存时 `sort_unstable` + `dedup`）；`time_start/time_end ∈ 0..=1439`，`start == end` 仅允许 `0:00–0:00`（= 全天）；每个 `model_id` 须指向已存在 Model。
- 上下文大小校验（既有 `validate_fallback_context`）扩展覆盖策略目标：每个 `TimeStrategy.model_id` 也是一个转移目标，过小会在降级时截断上下文。
- 环检测：visited-set 按 model id 去重，时间感知不影响。

## 6. 前端

### 6.1 类型与桥（`types.ts` / `commands.ts` / store）
- `types.ts`：`Model` 加 `fallback_strategies` + `fallback_strategies_enabled`；`RouteEffective` 加 `effective_fallback_model_id` + `via_fallback_strategy_id`；加 `ModelEffectiveFallback`。
- `commands.ts`：加 `setModelFailover`、`setModelFallbackStrategiesEnabled`、`modelEffectiveFallbacks`；`routeEffectiveModels` 返回值变宽。
- `config` store：暴露 `model_effective_fallbacks`（随轮询刷新 + 保存后重取）；沿用"变更即重取受影响切片"。

### 6.2 共享组件 `StrategyEditor.vue`（新增）
从 `Profiles.vue` 抽出策略编辑块：星期 7 Toggle + 【工作日】【全选】、两个时间 Picker（含跨夜 🌙/全天 🕐 提示）、优先级（1–10）、启停、调用模型下拉、增删。对容器与数据源无感：入参 `strategies`（表单态数组），事件回写。**入口弹窗（`Profiles.vue`）与故障转移弹窗（`Fallback.vue`）共用。**

### 6.3 路由列表卡片（`Profiles.vue`）
```
⠿ glm-5.2  当前生效→ glm-4.5(火山)⏰  ┌⏰ 策略 3 条 [●─]┐  [编辑][删除]
```
- **去掉**「兜底→backing」显示；"当前"→"**当前生效**"，值取 `route_effective_models`（策略命中带 ⏰）。
- **策略计数 + 总开关合并进同一个 `NTag`**（`NSwitch size="tiny"` 嵌入），一眼看出开关管的是策略；开关 → `set_profile_strategies_enabled`。策略数 0 时不显示该 tag（无策略可开关）。
- 随既有 ~60s 轮询刷新"当前生效"。

### 6.4 故障转移页（`Fallback.vue`）—— 镜像路由页
```
说明：为模型配置 429/用量故障时的转移目标与时段策略            [+ 新增]
┌──────────────────────────────────────────────────────────────────┐
│ ⠿ glm-4.6(智谱)  当前生效→ qwen-max(阿里)⏰  ┌⏰策略2条 [●─]┐  [编辑][删除] │
└──────────────────────────────────────────────────────────────────┘
```
- **列表只列已配置故障转移的模型**（判定见 §3），可拖动排序（沿用既有 cosmetic 顺序）。
- 每行：`模型 → 当前生效的转移模型 → [策略N条+一键开关] → 编辑 → 删除`。
  - "当前生效的转移模型"取 `model_effective_fallbacks`（策略命中带 ⏰），随轮询刷新。
  - `[策略N条+一键开关]` 合并进一个 `NTag`；开关 → `set_model_fallback_strategies_enabled`。
- **`[+新增]`**（说明行右侧）→ 弹窗：模型下拉**只列未配置故障转移的模型** + 默认转移目标 + `StrategyEditor` + 总开关 + 熔断冷却。保存 → `set_model_failover`。
- **`[编辑]`** → 同款弹窗（模型固定）。
- **`[删除]`** → 确认后清除该模型的故障转移配置（默认目标置 None + 策略清空），该行消失；模型本身不受影响。
- **行内不再有**冷却/默认目标的行内控件（都收进弹窗）。

### 6.5 概览管线（`Dashboard.vue` + `RouteLine.vue`）
```
[路由] ▶ [当前生效] ▶ [故障转移]
```
- 第二节点 tag 保持「**当前生效**」（不再改"转发模型"），值 = `effective_model_id`（仍带 quota 角标 / 冷却 ❄ / 策略 ⏰）。
- 新增「**故障转移**」节点：值 = `effective_fallback_model_id`（`via_fallback_strategy_id` 存在则带 ⏰），为 `None` 则隐藏。
- **去掉**原"兜底"（静态 backing）节点及其 `fallbackModelLabel` prop（`RouteLine` 仅被 `Dashboard` 使用，移除安全）。
- `Dashboard` 从 `routeEffective` 取 `effective_fallback_model_id` 传入。

### 6.6 术语切割
- **生效模型统一"当前生效"**：路由卡、故障转移卡（"当前生效的转移模型"）、概览管线一致。
- 路由弹窗 backing 字段「兜底模型（默认/回退）」→「**默认模型**」；故障转移弹窗默认目标字段叫「**默认转移目标**」。
- 保留：「故障转移」「回退」等反应式用词（语境清晰）。

## 7. 测试

| 层 | 用例 |
|---|---|
| `proxy/strategies.rs` | `model_fallback_target`：策略命中→策略目标；总开关关→默认；策略目标被删→默认；无默认且无命中→None。**时段切替临界点**：策略 `22:00–06:00→model_B`、默认 `model_C`，钉 `LocalNow` 于 21:59 / 22:00 / 05:59 / 06:00（分钟粒度 1319/1320/359/360），断言 `(target, via_strategy_id)` 依次为 `(model_C,None)` / `(model_B,Some)` / `(model_B,Some)` / `(model_C,None)`（跨夜下半夜按 `prev_weekday` 归属，`days_of_week` 需含相应星期） |
| `proxy/dispatch.rs`（FakeClock 钉时间，时区无关） | GLM 429 → 22:00 走 qwen / 其余时间走 火山，并在 21:59/22:00/05:59/06:00 临界点端到端验证服务模型随窗口跳变；策略目标被删→默认；**链式**：GLM→(GLM策略)→qwen→(qwen自己的策略)→第三模型，逐级验证每跳读的是各自模型的配置；环检测仍生效 |
| `config/types.rs` | 带 `fallback_strategies` 的 Model 往返。**反序列化兼容**：一份不含 `fallback_strategies`/`fallback_strategies_enabled` 的旧版 JSON 经 `serde_json::from_str` 加载 → `fallback_strategies == []` 且 `fallback_strategies_enabled == true`（向后兼容） |
| `commands.rs` | `set_model_failover` 校验（拒非法 priority/weekday/time、`start==end`、空 days、悬空 model_id）；`model_effective_fallbacks` 与 `route_effective_models` 的 `effective_fallback_model_id` 正确；`set_model_fallback_strategies_enabled` 生效 |
| 前端 | `pnpm exec vue-tsc --noEmit` + `pnpm build` 通过 |

## 8. 不在本次范围

- 不合并入口/故障转移数据模型（per-route 主动 / per-model 反应式，各自正确）。
- 生效值不做前端本地时间预测（单一真相在 Rust）。
- 应用级时区配置仍走系统时区（同入口层 spec）。
- 其它策略类型（按额度/负载/按客户端）—— `StrategyKind` 已预留 tag。
