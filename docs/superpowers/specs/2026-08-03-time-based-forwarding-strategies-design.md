# 时段转发策略（Time-based Forwarding Strategies）— 设计规格

> 日期：2026-08-03  
> 状态：设计完成（简化版，待评审）  
> 关联：主规格 `2026-07-27-switchlm-llm-proxy-design.md`（§3 请求流、§4 熔断/降级）

## 1. 背景与业务场景

国内大模型厂商（阿里云百炼、智谱、火山等）在不同时段采用不同套餐计费倍率（闲时降价、忙时加价）。当前路由（Profile）只能固定绑定一个 Model，无法按时间窗口动态切换。

**核心场景**：
1. **GLM 工作日避峰**：周一至周五 14:00–18:00 高峰倍率极高 → 自动转发至备用模型（如 `qwen-max`）；其余时间走 GLM 兜底。
2. **通义千问夜间打折**：每天 22:00–次日 08:00（**跨夜**）优惠时段 → 优先切到高阶打折模型（如 `qwen3-max-preview`）；其余时间走兜底。

**目标**：一个路由配置多条转发策略，按时间窗口选本次入口模型；整组策略提供一键启停的「总开关」，便于异常时一键回退兜底而不删策略。

**成功标准**：高峰/闲时窗口命中 → 走策略模型；不命中/总开关关 → 走兜底；策略入口模型被限流/熔断 → 沿其既有 `fallback_target_model_id` 链降级（弹性不变）。

## 2. 设计原则与关键决策

| 决策 | 结论 | 理由 |
|---|---|---|
| 时间模式 | **按天重复（支持跨夜）**；不引入"跨周连续"模式 | 零学习成本，覆盖现实全部场景（避峰、夜间打折）。 |
| 星期表达 | **多选集合** `days_of_week: Vec<u8>`（1=周一..=7=周日） | 比"周几区间+跨界"直观；UI 用 7 个 Toggle + 工作日/全选快捷键。 |
| 跨夜归属 | 上半夜看"今天"、下半夜看"**昨天**"是否在选中集合 | 22:00–08:00 窗口：23:00 看今天、次日 02:00 看昨天（窗口的起始日）。 |
| 匹配时钟 | **本机系统时间**，经 `Clock` trait 取（用户选定本地时区） | 符合直觉；中国用户系统时区通常即 Asia/Shanghai = 厂商时段。 |
| 时区与可测性 | 时间源走 `Clock::now_local()`；`FakeClock` 返回固定值 | 策略集成测试**与机器时区无关**（同熔断测试用 `FakeClock` 的既有模式）。 |
| 兜底字段 | **复用 `Profile.backing_model_id`** | 无策略命中/失效/总开关关 → 走它；向后完全兼容。 |
| 注入位置 | **解析层**（`resolve_model`，A 方案） | 策略只决定入口模型；熔断/降级链零改动。 |
| 策略与熔断 | 策略选入口，熔断管降级 | 入口 cooling/限流 → 沿该入口模型既有降级链。 |
| 启停粒度 | **总开关**（`strategies_enabled`）+ **单策略 `enabled`** | 总开关一键回退兜底；单策略可临时静音而不删。 |
| 配置入口 | **仅「路由」tab** | 概览只读；托盘移除路由区块。三界面职责单一。 |

## 3. 数据结构与校验（`config/types.rs`）

```rust
/// 一条转发策略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Strategy {
    pub id: String,
    /// 1..=10，数字越小优先级越高。多条命中取最小；并列按列表顺序（靠前胜）。
    pub priority: u8,
    /// 单策略启停（默认 true）。总开关 strategies_enabled 才是"一键回退"。
    #[serde(default = "default_strategy_enabled")]
    pub enabled: bool,
    pub kind: StrategyKind,
}
fn default_strategy_enabled() -> bool { true }

/// 策略类型（tag，序列化为 {"type":"time", ...}），预留扩展。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StrategyKind {
    Time(TimeStrategy),
}

/// 按天重复的时间窗口（支持跨夜）。minute_of_day 0..=1439。
/// start 含、end 不含（半开区间）；start > end 表示跨夜（自当天 start 起，至次日 end 止）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeStrategy {
    /// 命中此窗口的"起始日"星期集合（窗口下半夜归属该日的次日）。1=周一..=7=周日。
    pub days_of_week: Vec<u8>,   // 子集 of {1..=7}，非空
    pub time_start: u16,         // 0..=1439
    pub time_end: u16,           // 0..=1439；== time_start 仅允许 0:00–0:00（全天）
    pub model_id: String,        // 任意已配置 Model
}
```

`Profile` 增两个字段，均 `#[serde(default)]`，**向后兼容**（旧配置无这两字段 → `[]` / `true`，即今日行为）：

```rust
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 兜底模型：无策略命中/失效/总开关关时的入口模型。
    pub backing_model_id: String,
    #[serde(default)]
    pub strategies: Vec<Strategy>,
    /// 总开关：false 跳过全部策略，直接走兜底（一键回退，不删策略）。默认 true（空集即无操作）。
    #[serde(default = "default_strategies_enabled")]
    pub strategies_enabled: bool,
}
fn default_strategies_enabled() -> bool { true }
```

**校验（`upsert_profile` 落盘前拒绝）**：
1. `priority ∈ 1..=10`。
2. `days_of_week` 非空，且每个值 `∈ 1..=7`（保存时去重排序，纯整理）。
3. `time_start`、`time_end ∈ 0..=1439`；**`time_start == time_end` 仅允许 `0:00–0:00`**（= 全天，命中选中星期每一分钟），其它起止相等属空区间、拒绝。
4. 每个 `model_id` 必须指向已存在 Model。
（priority 不必唯一；并列按列表顺序，确定性。）

## 4. 请求解析与路由判定

### 4.1 时间源：`Clock` trait 扩展（确定性 seam）

`Clock` trait（`proxy/health.rs`，已为熔断抽象）新增一个方法，避免在热路径直接调 `chrono::Local`：

```rust
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> i64;                                   // 既有
    fn now_local(&self) -> LocalNow;                             // 新增
}

pub struct LocalNow { pub weekday: u8 /*1=Mon..=7=Sun*/, pub minute: u16 /*0..=1439*/ }
```

- `SystemClock::now_local()`：用 `chrono::Local` 由 `now_secs()` 换算（依赖系统时区）。
- `FakeClock::now_local()`：返回一个**可设置的固定 `LocalNow`**（测试设；默认如 周一 00:00）。

→ 策略集成测试设 `FakeClock` 的 `LocalNow` 即可钉死"当前周几/分钟"，**与机器时区无关**。

### 4.2 纯匹配模块 `proxy/strategies.rs`

```rust
pub fn days_contains(days: &[u8], weekday: u8) -> bool;          // 集合归属
pub fn prev_weekday(weekday: u8) -> u8;                          // 周一→周日(7)，否则 -1

/// 单条时间策略是否命中 (weekday, minute)。封装全天/同天/跨夜三种语义：
///  - start == end（仅 0:00–0:00）：全天，days_contains(today)
///  - start < end（同天）：days_contains(today) && minute ∈ [start, end)
///  - start > end（跨夜）：
///      minute >= start（上半夜）⇒ days_contains(today)
///      minute <  end（下半夜）⇒ days_contains(prev_weekday(today))   // 窗口起始日=昨天
pub fn strategy_matches(s: &TimeStrategy, weekday: u8, minute: u16) -> bool;

/// 已启用且命中的策略中选胜者：min priority，并列取列表靠前。无命中→None。
pub fn select_strategy(strategies: &[Strategy], now: &LocalNow) -> Option<&Strategy>;

/// 单一真相：profile 当前入口模型 id + 命中策略 id。
/// 总开关关 / 胜者模型已删 → 退回 backing_model_id（via=None）。
pub fn profile_start_model<'c>(profile: &'c Profile, cfg: &'c AppConfig, now: &LocalNow)
    -> (&'c str /*model_id*/, Option<&'c str /*strategy_id*/>);
```

`select_strategy` 并列处理显式编码（遍历，以 `(priority, index)` 比较，留首个最小者；不依赖 `min_by_key` 并列语义）。`select_strategy` 把 `LocalNow` 拆成 `(weekday, minute)` 传给 `strategy_matches`。

### 4.3 接入 `resolve_model`（热路径唯一改动点）

`resolve_model` 增加 `now: &LocalNow` 参数；用 `profile_start_model` 取模型 id（而非无条件取 `backing_model_id`）；其后 `dispatch` 的熔断/降级走查**零改动**。

`dispatch.rs` 在既有 config 读锁块内、调用 resolve 前：`let now = state.clock.now_local();`。

**执行流**：
1. 总开关关 → 直接返回兜底模型。
2. 遍历已启用策略，用当前本地时间匹配；命中者按优先级选胜；无命中 → 兜底。
3. 胜者模型已被删 → 容错退回兜底。
4. 入口模型 cooling/被限流/超额 → 沿其 `fallback_target_model_id` 链降级（既有弹性不变）。

### 4.4 日志（遵循"日志须识别厂商+模型"约定）

策略命中重定向时发一条：
```
tracing::info!(req=%req_id, profile=%name, strategy=%strategy_id, model=%tag, "time-strategy matched");
```
既有终态 `forward ok`/`forward failed` 已记录实际服务模型，不变。

### 4.5 展示用的单一真相命令

新增 `route_effective_models() -> Vec<RouteEffective { profile_id, effective_model_id, via_strategy_id: Option<String> }>`，调用 `profile_start_model` 即时计算。概览链路头部、路由 tab 卡片"当前→"均读它——Rust 匹配器是唯一真相，前端不做匹配（避免漂移；托盘路由区块已移除，不消费此命令）。随既有 ~60s 用量轮询刷新，且每次 profile 编辑后重取。

## 5. 命令（`commands.rs`）

- **新增** `route_effective_models`（见 §4.5）。
- **新增** `set_profile_strategies_enabled(profile_id, enabled)`：总开关轻量切换（路由卡片一键切换，不必提交整个 profile）。仿 `set_profile_backing` 形态：写 cfg → `persist` → 刷托盘。
- `upsert_profile` 增策略校验（见 §3）。校验通过后对每条 `TimeStrategy.days_of_week` 做标准清洗（防前端意外传 `[1,1,2]`，保证存储一致与体积）：
  ```rust
  strategy.kind.time.days_of_week.sort_unstable();
  strategy.kind.time.days_of_week.dedup();
  ```
- **清理（待确认）**：移除 `set_profile_backing` + `set_profile_backing_core`。移除托盘路由区块 + 概览快捷编辑后，二者已无调用方（路由 tab 编辑走 `upsert_profile`）。应用无外部 API 面，删未用命令属常规清理。

## 6. 前端

### 6.1 类型与桥（`types.ts` / `commands.ts`）
- `types.ts`：加 `Strategy` / `StrategyKind` / `TimeStrategy`；`Profile` 加 `strategies` + `strategies_enabled`；加 `RouteEffective`。
- `commands.ts`：加 `routeEffectiveModels`、`setProfileStrategiesEnabled`；移除 `setBacking`（随后端清理）。
- `config` store：拉取并暴露 `route_effective_models`（随轮询 + 保存后刷新）。

### 6.2 「路由」tab（`Profiles.vue`）—— 唯一编辑面
**编辑弹窗**：
```
名称:                [glm-5.2]
兜底模型(默认/回退):  [glm-4.6 (智谱)]      ← 原"接入的真实 Model"改名
────────── 时段策略 ──────────
启用时段策略  [●──── 开]                   ← 总开关；关时高亮"已暂停策略,所有请求将直接走兜底模型"
[ + 添加策略 ]
 ┌ 启用[●] 优先级[2]              [删除] ┐
 │ 重复: [一][二][三][四][五][六][日]    │   ← 7 Toggle；旁附【工作日】【全选】快捷键
 │ 时间从[22:00] 到[08:00]  🌙已跨夜(于次日08:00结束) │  ← end≤start 自动显示跨夜提示
 │ 调用模型 [glm-4.5 (火山)]             │
 └──────────────────────────────────────┘
```
- 星期：7 个独立 Toggle（一二三四五六日）+【工作日】（勾周一–周五）+【全选】（周一–周日）快捷键，绑定 `days_of_week` 集合。
- 时间：两个 24h `NTimePicker`（HH:mm，绑 minute）。**仅当 `time_start > time_end`** 时右侧高亮 `🌙 已跨夜 (于次日 HH:mm 结束)`；`00:00→08:00`（start<end）属同天凌晨窗口，**不**触发跨夜提示，避免"算成昨天"的困惑。
- 目标：与兜底相同的模型下拉。单策略行：启停 + 优先级(1–10) + 删除（确认）。行内编辑；保存即 `upsert_profile` 整个 profile。

**路由列表卡片**：
```
⠿ glm-5.2   兜底→ glm-4.6 (智谱)   ⏰策略 3条  总开关[●─]  当前→ glm-4.5 (火山)⏰   [编辑][删除]
```
- `兜底→` = backing；`当前→` = 有效模型（来自 `route_effective_models`），策略命中带 ⏰，总开关关显示 `(策略已停用)`。
- 卡片上 **总开关** → `set_profile_strategies_enabled`（即存）。`⏰策略 N条` 徽章可点 → 打开编辑弹窗。单策略 `enabled` 留在弹窗内。

### 6.3 「概览」（`Dashboard.vue`）—— 只读
- 移除快捷编辑：删 `modelOptions`、`switchModel`，不再向 `RoutePipeline` 传 `model-options`/`@switch-model`。
- 链路头部：`backingModelFor(p)` → **策略感知的有效模型**（来自 `route_effective_models`）；降级链走查（`fallback_target_model_id`）与既有一致，保留 cooling ❄ 标记。
- `RoutePipeline` 组件转为只读（去掉节点切换/select 交互）。展示"生效模型 → 降级模型"，无任何编辑入口。

### 6.4 托盘（`tray.rs`）—— 移除路由区块
- 删 `ProfileMenuSpec`、`ModelOptionSpec`、`TrayMenuSpec.profiles`、profile 子菜单构建、`backing:` 点击处理分支。
- 保留「套餐用量」/账号区块与 tooltip。空路由不再渲染占位项（区块整体消失）。
- 更新 `tray.rs` 测试（去掉 profiles 相关用例）。

## 7. 测试

| 层 | 用例 |
|---|---|
| `proxy/strategies.rs`（纯函数，确定性） | `days_contains`/`prev_weekday`（周一↔周日环绕）；**同天**窗口 `[start,end)` 边界（含起不含终）；**跨夜**窗口：上半夜(23:00)看今天、下半夜(02:00)看**昨天**是否在集合（验证 `prev_weekday` 推算，含周一→周日跨界）；**半开区间边界点钉死**：`minute=end-1` 命中 / `minute=end` 落空（下半夜），`minute=start` 命中 / `minute=start-1` 落空（上半夜）；`select_strategy` 优先级序、并列按 index、`enabled` 过滤、无命中→None；`profile_start_model` 总开关关→兜底、策略模型被删→兜底 |
| `config/types.rs`（serde） | 带 strategies 的 Profile 往返；**旧配置**（无 `strategies`/`strategies_enabled`）→ `[]`/`true`（证明向后兼容） |
| `Clock` seam | `FakeClock::now_local()` 返回设定值；`SystemClock::now_local()` 与 `chrono::Local` 一致（松验） |
| `proxy/resolve.rs` + `dispatch.rs`（集成，**时区无关**） | 设 `FakeClock` 的 `LocalNow` 钉死时间：策略命中→走策略模型；总开关关→兜底（短路，不依赖时间）；策略模型被删→兜底；策略模型**被限流→沿其熔断链降级**（组合性） |
| `commands.rs` | `upsert_profile` 校验（拒 priority/weekday/time 非法、`start==end`、空 days、悬空 model_id）；`route_effective_models` 正确性 |
| `tray.rs` | spec 不再含 profiles 区块 |
| 前端 | `npx vue-tsc --noEmit` + `npm run build` 通过 |

## 8. 不在本次范围

- 其它策略类型（按额度、按负载、按客户端）—— `StrategyKind` 已预留 tag，后续加 variant 即可。
- 应用级时区配置（当前用系统时区；如需，后续在 Settings 加时区下拉）。
- "跨周连续"模式（如"周五晚持续到周一"的连贯窗口）—— 现实场景用"按天重复+跨夜"已足够；如确需再议。
