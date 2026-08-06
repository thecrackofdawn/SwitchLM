# 套餐主值选取统一 + 移除 `used` 字段

**日期：** 2026-08-06
**状态：** 设计稿（待评审）
**关联：** 主设计 spec §3.4（echoed model）、§6.3（breaker `recover_at`）、§6.4（usage cache）

## 1. 背景 / 根因

一个套餐（plan）账号的「主已用百分比」目前在三处各算各的，回落语义不一致：

| 消费点 | 位置 | 现行选取逻辑 |
|---|---|---|
| 托盘 tooltip | `tooltip_value` (`src-tauri/src/tray.rs:71`) | plan → 5h 档，回落顶层 `used` |
| 概览额度 chip | `quotaMathFor` (`src/lib/quotaUtils.ts:49`) | plan → **仅** 5h 档（无回落） |
| 熔断器 | `exhausted_reset_at` (`src-tauri/src/proxy/dispatch.rs:1014`) | 扫描**所有** tier（任一窗口 ≥100%）—— 这是另一回事，不在本次统一范围内 |

顶层聚合字段 `UsageSnapshot.used` 的赋值也不一致：`snapshot_from_tiers` 设为首档值（智谱/火山），DeepSeek 设为 CNY 已花费（consumption），**千问 `map_usage` 写死 `None`**（`src-tauri/src/usage/qianwen.rs:79`）。

**触发的 bug：** 当千问 API 返回中 `per5HourPercentage` 缺失/为 null、但 `per1WeekPercentage` 有值时——

- 右键菜单用 `tier_summary`（`tray.rs:51`，**任一档有值即显示**）→ 千问显示成 `5h:– 周:9.x%`，菜单 **5 条**。
- 悬浮 tooltip 用 `tooltip_value`（5h 档或 `used`，**两者皆 None**）→ 千问被丢弃，tooltip **4 条**。
- 概览 chip 用 `quotaMathFor`（仅认 5h 档）→ 千问显示「无数据」（同一根因的另一表现）。

> ⚠️ **实施后更正（见 §8.1）**：上述"千问被 tooltip 丢弃"的推断**前提不成立**——千问 5h 档实测为 0%（有值），始终在 tooltip 列表里。用户"tooltip 只显 4 个"的真正根因是 Windows 托盘 tooltip 的 64 字符上限把 85 字符的 5 行从中间截断了。§1 描述的不对称是真实潜在 bug（已修、保留），但**非**本次症状成因。

**附带发现的潜在隐患：** `exhausted_reset_at` 的无-tier 分支用 `exhausted(usage.used)` 判断耗尽，但 DeepSeek 的 `used` 是 **CNY 绝对值**（如 51.23），不是 0–100% 百分比，却与 `EXHAUSTED_PCT = 100.0`（`dispatch.rs:880`）比较。该分支**今天行为中立**：唯一会走到它的真实厂商是 DeepSeek，而其 `reset_at` 写死 `None`（`deepseek.rs:91`），故 `if exhausted(used) { return reset_at; }` 无论真假都返回 `None` → 落回 cooldown（这对"无重置时间可等"的余额耗尽恰好是对的）。但代码具有误导性，且未来若出现带 `reset_at` 的 consumption 厂商会误判。详见 §5。

## 2. 目标 / 非目标

**目标**

1. 用一个 helper 统一 plan 主值选取：**5h → 周 → 月**（首个有值的窗口）。托盘 tooltip 与概览 chip 共用此语义。
2. 移除 `UsageSnapshot.used` 字段——在把它的两个读者（tooltip、breaker 无-tier 分支）改走新逻辑后，该字段运行期零读者。
3. 修正 `exhausted_reset_at` 的 consumption 耗尽判断为 `remaining <= 0`。

**非目标**

- 不改熔断器 recover 语义：consumption 余额耗尽仍回落 cooldown（无重置时间可等）。"耗尽后更长退避 / 提示充值"属于后续增强，不在本次范围。
- 不移除 `total` / `remaining` 字段（它们对 consumption 仍有效；plan 侧虽展示层不读，但保留无害，且超出本次范围）。
- 不改熔断器的 tier 扫描逻辑（多窗口"任一耗尽"判断保持原样）。

## 3. 设计

### 3.1 Rust：`primary_used` helper

新增于 `src-tauri/src/usage/mod.rs`（与 `snapshot_from_tiers` 同处），导出供 `tray.rs` 使用。

```rust
/// plan 主值窗口的选取顺序：最紧、最可操作的窗口优先。
const PRIMARY_WINDOW_ORDER: &[&str] = &[TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY];

/// plan 账号的主已用百分比：按 5h → 周 → 月 取首个有具体值的窗口，连同该窗口名一起返回
/// （供展示层标注窗口）。无任何窗口有值时返回 `None`（账号从单值展示中隐藏）。
pub fn primary_used(tiers: &[UsageTier]) -> Option<(f64, &str)> {
    for &window in PRIMARY_WINDOW_ORDER {
        if let Some(t) = tiers.iter().find(|t| t.window == window) {
            if let Some(pct) = t.used_pct {
                return Some((pct, window));
            }
        }
    }
    None
}
```

返回窗口名是为了让概览 chip 在回落到周/月时能正确标注（§3.4）。

### 3.2 托盘 tooltip：改用 `primary_used`

> ⚠️ **本节已被 §8.2 取代**：实施后发现 tooltip 列全部套餐会撞 Windows 64 字符上限（§8.1），最终改为只显示"生效套餐"（1 行）。`primary_used` 仍用于该行的数值。

`tooltip_value`（`tray.rs:71`）plan 分支去掉 `.or(u.used)`，改用 helper。tooltip 保持紧凑、**不带窗口名**（详细分档见右键菜单 / 用量页）：

```rust
fn tooltip_value(u: &UsageSnapshot) -> Option<String> {
    if u.billing_model == "consumption" {
        u.remaining.map(|r| format!("{} {:.2}", u.unit, r))
    } else {
        let (pct, _) = primary_used(&u.tiers)?;
        Some(format!("{}%", pct.round() as u32))
    }
}
```

`account_display`（菜单行，`tray.rs:137`）**不动**——它用 `tier_summary` 展示完整 `5h:.. 周:.. 月:..` 分档，本就是详细视图。修复后 tooltip 与菜单在"有任一档数据即出现该账号"上达成一致，根除千问 bug。

### 3.3 概览 chip：TS 镜像 `primaryUsed`

新增于 `src/lib/quotaUtils.ts`（与 `quotaMathFor` 同处）：

```ts
const PRIMARY_WINDOW_ORDER = ["five_hour", "weekly_limit", "monthly"] as const;

/** plan 主值：5h → 周 → 月 首个有值窗口的 used_pct 与窗口名；都没有则 null。 */
function primaryUsed(
  tiers: UsageTier[] | undefined,
): { pct: number; window: string; resetAt?: number | null } | null {
  for (const w of PRIMARY_WINDOW_ORDER) {
    const t = tiers?.find((x) => x.window === w);
    if (t?.used_pct != null) return { pct: t.used_pct, window: w, resetAt: t.reset_at };
  }
  return null;
}
```

`quotaMathFor`（`quotaUtils.ts:49`）plan 分支由"仅 5h"改为 `primaryUsed`：`used = pct`、`remaining = max(0, 100 - pct)`、`resetAt` 取所选窗口。consumption 分支不变。

窗口标注：`tooltipFor`（`quotaUtils.ts:75`）当前写死"5 小时窗口"。改为按窗口映射——`five_hour → "5 小时窗口"`、`weekly_limit → "每周窗口"`、`monthly → "每月窗口"`。chip 正文 `剩 X%` 不带窗口名（保持紧凑）。

效果：千问（5h 缺、周档有）在概览 chip 由「无数据」变为显示周的剩余%，tooltip 标注"每周窗口"。

### 3.4 移除 `used` 字段

读者改完后，`used` 运行期零读者（确认：`\.used` 全仓 grep 仅命中 `tray.rs:80` 与 `dispatch.rs:1024` 两处运行期读取，其余皆为测试）。

- **结构体** `UsageSnapshot`（`usage/mod.rs:24`）：删除 `used` 字段。
- `snapshot_from_tiers`（`usage/mod.rs:103`）：`total`/`remaining` 继续由首档派生（智谱/火山首档恒有值，行为不变），仅删去 `used` 赋值。
- DeepSeek（`deepseek.rs:87`）：从字面量删去 `used: Some(used)`；其 `used` 局部变量（`topped_up - remaining`）仅服务于该字段，一并删除。`total`/`remaining` 保留（consumption 展示与 §3.5 熔断判断都用 `remaining`）。
- 千问（`qianwen.rs:79`）、智谱 raw 兜底（`zhipu.rs:294`）：从字面量删去 `used: None`。
- **IPC/类型同步**：`UsageSnapshot` 是 `Serialize`（经 Tauri IPC 发往前端）。前端 `src/lib/types.ts:92` 的 `used?: number | null` 同步删除。无持久化顾虑（`UsageCache` 纯内存，从不对该结构反序列化——见 `mod.rs` `default_billing_model` 注释）。

### 3.5 修正 consumption 熔断耗尽判断

`exhausted_reset_at`（`dispatch.rs:1014`）无-tier 分支：

```rust
fn exhausted_reset_at(usage: &UsageSnapshot) -> Option<i64> {
    let exhausted = |used_pct: Option<f64>| used_pct.map(|u| u >= EXHAUSTED_PCT).unwrap_or(false);
    if !usage.tiers.is_empty() {
        return usage.tiers.iter()
            .filter(|t| exhausted(t.used_pct))
            .filter_map(|t| t.reset_at)
            .max();
    }
    // 无 tier 明细 → consumption（余额）型。耗尽 = 余额见底。
    // DeepSeek 的 reset_at 为 None，故此处仍返回 None → 调用方回落 cooldown（行为不变）；
    // 但判断本身现在正确，未来出现带 reset_at 的 consumption 厂商不会再误判。
    let depleted = usage.remaining.map(|r| r <= 0.0).unwrap_or(false);
    if depleted { return usage.reset_at; }
    None
}
```

## 4. 行为变化 / 风险

| 变化 | 影响 | 风险 |
|---|---|---|
| 千问（5h 缺、周档有）出现在托盘 tooltip | **bug 修复** | 无 |
| 任一 plan 在 5h 缺、周/月有值时，概览 chip 由「无数据」变为显示主值；chip tooltip 按实际窗口标注 | 改善（一致性） | 低 |
| 删除 `used` 字段 | IPC 载荷少一字段；前端本就不读 `snapshot.used` | 低 |
| `exhausted_reset_at` consumption 判断由 `used>=100` 改 `remaining<=0` | **当前零可观测变化**（DeepSeek `reset_at=None`） | 低 |

整体风险低：均为展示层修正 + 一处行为中立的熔断清理 + 零读者字段移除。

## 5. 关于 §1 "潜在隐患" 的严重度说明

为避免误判：`exhausted(usage.used)` 这条判断**今天不产生任何错误行为**——DeepSeek 的 `reset_at` 恒为 `None`，使整条 `if exhausted(used) { return reset_at; }` 分支无论 `used` 真假都返回 `None`，最终落回 cooldown，这对"无重置时间"的余额耗尽恰为正确处理。它是**机制错误、结果碰巧正确**的潜在隐患（latent smell），非正在发作的 bug。§3.5 的修正是正确性 + 防未来 + 删 `used` 的前提，并非行为修复。不计划引入"耗尽后更长退避"等新行为（见 §2 非目标）。

## 6. 测试

**Rust（`cargo test`）：**

- `primary_used`：5h 有→取 5h；5h 缺、周有→取周；皆无→`None`；窗口顺序正确（不取月而当有周）。
- `tooltip_value`（tray）：plan + 5h 缺 + 周有 → `Some("X%")`（**回归千问 bug**，此前为 `None`）；plan 全无 → `None`；consumption 不变。
- `exhausted_reset_at`：用 consumption 形态快照（`tiers=[]`、`remaining` 与 `reset_at` 可控）验证——耗尽(`remaining<=0`)+有 reset → `Some(reset)`；未耗尽 → `None`；耗尽但无 reset → `None`。**替换**原基于合成 `snap_used(Some(100.0), …)` 的两个单测（该 helper 无真实适配器支撑，随 `used` 一并移除）。
- 更新/移除所有 `assert_eq!(snap.used, …)` 断言（智谱/火山/DeepSeek/mod 的 cache 测试）与 tray 测试里的 `u.used = Some(55.0)` 用例（改为构造带 used_pct 的 tier）。

**前端：** 项目无 JS 测试框架（`package.json` 无 test/vitest，`src` 下无 `*.test.ts`）。TS 改动（`primaryUsed`/`quotaMathFor`/`tooltipFor`）以 `pnpm build`（vue-tsc 类型检查）+ 运行 app 行为核对（千问 chip 与 tooltip）验证。

## 7. 实施顺序（概要，详见后续 plan）

1. 加 Rust `primary_used` + 单测；改 `tooltip_value` 用它（+ 回归测试）。
2. 加 TS `primaryUsed`；改 `quotaMathFor`/`tooltipFor`（窗口标注）；`pnpm build` 过类型。
3. 删 `used`：结构体 + 4 处赋值 + `types.ts`；收拾受影响测试。
4. 改 `exhausted_reset_at` consumption 分支；替换合成单测。
5. 全量 `cargo test` + `pnpm build`；运行 app 核对千问 tooltip/chip。

---

## 8. 后续修订（2026-08-06，实施后）：托盘 tooltip 改为只显示"生效套餐"

> **本节是对 §1 / §3.2 的重要更正。** 实施 §1–§7 后用户实测发现 tooltip 仍只显示 4 个套餐，由此定位到真正的根因。§1–§7 的改动（`primary_used` / 删 `used` / 熔断 `remaining<=0`）作为真实的潜在-bug 清理**保留**，但它们**不是**用户原始症状的成因。

### 8.1 更正：原始症状的真正根因是 Windows 64 字符 tooltip 上限

§1 推断"千问被 tooltip 丢弃"是 `tooltip_value` 的 5h/`used` 不对称所致（5h 缺 + `used=None`）。**实测推翻了这个前提**：千问的 5h 档有值（`per5HourPercentage` 返回 0%，用量页显示 `5小时 0%`），所以 `tooltip_value` 对千问返回 `Some("0%")`——千问一直在 tooltip 行列表里。真正发生的是：

- 5 个套餐都在列表 → 拼接后 **85 字符**（千问 `· token plan 0%` 因 `usage_order` 为空、排在最后，是第 5 行）。
- Windows 托盘 tooltip（`NOTIFYICONDATA.szTip`，本机 `tray-icon` 0.24 + Tauri 2.11）实际只渲染前 **64 字符**——实测截断点精确落在第 64 字符："DeepSeek CNY 45." 被从值中间砍断（"68" 没了），千问整行落在 64 之后被丢弃。
- 因此代码里的 120 字符 `usage_tooltip` cap **从未生效**——OS 先在 64 处截断了。

即"字数限制"成立，但限制在 **OS 层 64 字符**，不是我们 120 的 cap。`primary_used` 的不对称（§1）是真实的潜在 code bug（已修、保留），但**不是**本次症状的成因——千问的 5h 有值，根本没踩到那个不对称分支。

### 8.2 修法：tooltip 只显示"最近一次请求实际用的模型"所属套餐

64 字符装不下 5 个套餐全名（要 85）。与其硬塞或截断，tooltip 改为只显示**当前生效那一个套餐**的余量——这恰是悬停时最该看的（"正在用的还剩多少"）。

- **`AppState`** 新增 `last_served_provider: Mutex<Option<String>>`（最后产出响应的模型所属 `provider_id`；`None` 直到首个请求产出响应）。runtime-only，不持久化。
- **`dispatch`**：请求结束、`served_model_id` 为 `Some` 时，查该模型的 `provider_id` 写入 `last_served_provider`；`FallbackExhausted`（全限流无响应）时**保留上一个**，避免 tooltip 中途变空。
- **`tray_menu_spec`** 新增 `last_served: Option<&str>` 参数；tooltip 由新函数 `last_served_tooltip` 生成——单行 `{display_name} {value}`（复用 `tooltip_value`，如 `火山 · coding plan 21%`，~22 字符，稳进 64）。`None` / 未知 provider / 该套餐无余量 → `SwitchLM`。
- **删除 `usage_tooltip`**（原 120 字符拼接器）：单行 tooltip 用不上它了。
- **右键菜单「套餐用量」不变**：仍列全部套餐（原生菜单项无字符上限）。

### 8.3 最终行为

| 场景 | tooltip |
|---|---|
| 智谱正常 | `智谱 80%` |
| 智谱限流、回落到火山 | `火山 · coding plan 21%`（生效的是回落方） |
| 全部限流（无响应） | 保留上一个生效套餐 |
| 刚启动、尚无请求 | `SwitchLM` |

### 8.4 `primary_used` 仍被使用

单行 tooltip 的数值（§8.2）与概览 chip（§3.3）都复用 `primary_used`。只是 tooltip **不再列全部套餐**——§3.2 描述的"tooltip 列全部、与菜单一致"已被 §8.2 取代。

### 8.5 测试补充

- `dispatch`：成功回落 → `last_served_provider` = 服务方 provider（非主路由）；全限流 → 保持上一个值。
- `tray`：`last_served=Some(pid)` 且有余量 → `"{name} {value}"`；`None` / 未知 / 无余量 → `SwitchLM`。旧的 `usage_tooltip` 单测随函数删除。
