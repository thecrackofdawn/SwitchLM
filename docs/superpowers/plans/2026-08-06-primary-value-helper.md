# 套餐主值选取统一 + 移除 `used` 字段 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 plan 套餐的「主已用百分比」选取统一成一个 helper（5h→周→月），修好千问在托盘 tooltip / 概览 chip 漏显示的 bug，并移除冗余的 `UsageSnapshot.used` 字段（顺带把 consumption 熔断耗尽判断改正确）。

**Architecture:** 新增 `primary_used(tiers)`（Rust）+ `primaryUsed(tiers)`（TS 镜像），分别接入托盘 tooltip（`tooltip_value`）与概览 chip（`quotaMathFor`）。两个旧读者改走新逻辑后，`used` 运行期零读者，整体删除（结构体 + 4 处赋值 + IPC + `types.ts`）。熔断 `exhausted_reset_at` 的无-tier 分支由 `used>=100` 改为 `remaining<=0`。

**Tech Stack:** Rust（axum/tokio，`#[cfg(test)] mod tests`，wiremock）、Vue 3 + TS（Naive UI，无 JS 测试框架）。

**关联 spec：** `docs/superpowers/specs/2026-08-06-primary-value-helper-design.md`

## Global Constraints

- **包管理器是 pnpm**：前端用 `pnpm build` / `pnpm exec vue-tsc --noEmit`，禁止 `npm`/`npx`。
- **后端测试**：`cargo test --manifest-path src-tauri/Cargo.toml`；单测以 `#[cfg(test)] mod tests` 内联在每个 `.rs`。
- **跨平台**：Windows + Linux；本改动无平台相关代码。
- **直接在 main 上开发**：每个 task 末尾用**显式路径** `git add <paths>` 提交（绝不 `git add -A`，以免扫入未跟踪文档/工作区里已改的 `CLAUDE.md`、`README.md`）。不 push（批量后续再推）。
- **TDD**：能写失败测试的 task 先写测试、看失败、再实现、看通过、提交。

---

## File Structure

| 文件 | 职责 / 改动 |
|---|---|
| `src-tauri/src/usage/mod.rs` | 新增 `primary_used` + `PRIMARY_WINDOW_ORDER`；删 `UsageSnapshot.used` 字段；`snapshot_from_tiers` 去掉 `used` 赋值；更新 cache 测试 |
| `src-tauri/src/tray.rs` | `tooltip_value` 改用 `primary_used`；改写回落测试；删各测试字面量里的 `used:` |
| `src-tauri/src/proxy/dispatch.rs` | `exhausted_reset_at` 无-tier 分支改 `remaining<=0`；新增 `snap_consumption` 测试 helper + 失败测试；删 helper 里的 `used` |
| `src-tauri/src/usage/qianwen.rs` | 删字面量 `used: None` |
| `src-tauri/src/usage/zhipu.rs` | 删 raw 兜底字面量 `used: None`；3 处 `snap.used` 断言改写 |
| `src-tauri/src/usage/deepseek.rs` | 删字面量 `used` + 局部 `used` 变量；2 处 `snap.used` 断言删除 |
| `src-tauri/src/usage/volcengine.rs` | 2 处 `snap.used` 断言改为 `tiers[0].used_pct` |
| `src/lib/types.ts` | 删 `UsageSnapshot.used` 字段 |
| `src/lib/quotaUtils.ts` | 新增 `primaryUsed` + 窗口标注；改 `quotaMathFor` / `tooltipFor`；`QuotaMath` 加 `window` |

---

## Task 1: Rust — `primary_used` helper

**Files:**
- Modify: `src-tauri/src/usage/mod.rs`（在 `snapshot_from_tiers` 附近新增函数；测试加在文件末尾 `mod tests` 内）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub fn primary_used(tiers: &[UsageTier]) -> Option<(f64, &'static str)>` —— 返回(已用%, 窗口名)，按 5h→周→月 取首个有值窗口；无则 `None`。常量 `PRIMARY_WINDOW_ORDER: &[&str] = &[TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY]`。

- [ ] **Step 1: 写失败测试**（追加到 `src-tauri/src/usage/mod.rs` 的 `mod tests`，紧跟现有 cache 测试之后）

```rust
    #[test]
    fn primary_used_picks_five_hour_first() {
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: Some(80.0), reset_at: None },
            UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: Some(40.0), reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), Some((80.0, TIER_FIVE_HOUR)));
    }

    #[test]
    fn primary_used_falls_back_to_weekly_when_no_five_hour() {
        // 千问 regression：5h 缺、周档有 -> 周档为主值。
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: None, reset_at: None },
            UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: Some(9.0), reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), Some((9.0, TIER_WEEKLY_LIMIT)));
    }

    #[test]
    fn primary_used_falls_back_to_monthly() {
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: None, reset_at: None },
            UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: None, reset_at: None },
            UsageTier { window: TIER_MONTHLY.to_string(), used_pct: Some(10.0), reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), Some((10.0, TIER_MONTHLY)));
    }

    #[test]
    fn primary_used_none_when_no_window_has_value() {
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: None, reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), None);
        assert_eq!(primary_used(&[]), None);
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml primary_used`
Expected: 编译失败，`cannot find function primary_used`（或 `PRIMARY_WINDOW_ORDER`）。

- [ ] **Step 3: 实现 helper**（在 `snapshot_from_tiers` 函数之后新增）

```rust
/// plan 主值窗口的选取顺序：最紧、最可操作的窗口优先。
const PRIMARY_WINDOW_ORDER: &[&str] = &[TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY];

/// plan 账号的主已用百分比：按 5h → 周 → 月 取首个有具体值的窗口，连同窗口名一起返回
/// （供展示层标注窗口）。无任何窗口有值时返回 `None`（账号从单值展示中隐藏）。
pub fn primary_used(tiers: &[UsageTier]) -> Option<(f64, &'static str)> {
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

- [ ] **Step 4: 跑测试看通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml primary_used`
Expected: 4 passed.

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/usage/mod.rs
git commit -m "feat(usage): add primary_used tier-selection helper"
```

---

## Task 2: Rust — 托盘 tooltip 改用 `primary_used`（修千问 bug）

**Files:**
- Modify: `src-tauri/src/tray.rs`（`tooltip_value` 函数 ~L71；import 行 L18；改写测试 ~L665）
- Test: 同文件 `mod tests`

**Interfaces:**
- Consumes: Task 1 的 `primary_used`。

- [ ] **Step 1: 改写失败测试**（替换现有 `tooltip_value_plan_uses_five_hour_then_used` 测试，~L665）

把整个该测试函数替换为：

```rust
    #[test]
    fn tooltip_value_uses_five_hour_then_falls_back_weekly() {
        // 5h 有 -> 5h 为主值。
        assert_eq!(tooltip_value(&snap_tiers(Some(80.0), None, None)).as_deref(), Some("80%"));
        // 5h 缺、周档有 -> 周档为主值（千问 regression：此前因 used=None 被 tooltip 丢弃）。
        assert_eq!(tooltip_value(&snap_tiers(None, Some(40.0), None)).as_deref(), Some("40%"));
        // 所有窗口都无值 -> None（账号从 tooltip 隐藏）。
        assert_eq!(tooltip_value(&snap_tiers(None, None, None)).as_deref(), None);
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tooltip_value_uses_five_hour_then_falls_back_weekly`
Expected: FAIL —— 第二个断言得 `None`（旧逻辑：5h 缺 + `.or(u.used)`，而 `snap_tiers(None,…)` 的 `used=None`），期望 `Some("40%")`。

- [ ] **Step 3: 改 `tooltip_value`**（~L71）

把 plan 分支替换为（去掉 `.or(u.used)`，改用 helper）：

```rust
/// Primary value for the tray-icon tooltip: plan -> "80%" (primary window used %, falling back
/// 5h -> 周 -> 月); consumption -> "CNY 48.77". `None` when no value can be derived.
fn tooltip_value(u: &UsageSnapshot) -> Option<String> {
    if u.billing_model == "consumption" {
        u.remaining.map(|r| format!("{} {:.2}", u.unit, r))
    } else {
        let (pct, _) = primary_used(&u.tiers)?;
        Some(format!("{}%", pct.round() as u32))
    }
}
```

并把顶部 import（~L18）改为引入 `primary_used`：

```rust
use crate::usage::{
    primary_used, UsageSnapshot, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY,
};
```

- [ ] **Step 4: 跑测试看通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tooltip_value`
Expected: 所有 tooltip_value 相关测试 passed（含新回落测试）。再跑全 tray 模块：`cargo test --manifest-path src-tauri/Cargo.toml tray::` —— 全绿。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/tray.rs
git commit -m "fix(tray): pick plan tooltip value via primary_used (5h->周->月)"
```

---

## Task 3: Rust — 熔断 consumption 耗尽判断改 `remaining<=0`

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs`（`exhausted_reset_at` ~L1014；测试模块新增 helper + 测试 ~L1670 附近）
- Test: 同文件 `mod tests`

**Interfaces:** 无新公开接口；仅修正 `exhausted_reset_at` 内部分支语义。

**说明（重要）：** 此改动对当前真实厂商**行为中立**（DeepSeek `reset_at=None`，耗尽与否都回落 cooldown）。它是正确性 + 防未来 + 删 `used` 的前提，不是行为修复。仍按 TDD：先写一条在新语义下才成立的测试。

- [ ] **Step 1: 写失败测试**（在 dispatch.rs `mod tests` 内、现有 `snap_used` helper 旁新增 helper，并在 `exhausted_reset_at_*` 测试组末尾新增测试）

先新增 helper（紧跟 `snap_used` 之后）：

```rust
    fn snap_consumption(remaining: Option<f64>, reset_at: Option<i64>) -> UsageSnapshot {
        UsageSnapshot {
            used: None,
            total: None,
            remaining,
            reset_at,
            unit: "CNY".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "consumption".into(),
            plan_info: None,
        }
    }
```

再新增测试：

```rust
    #[test]
    fn exhausted_reset_at_consumption_depleted_uses_remaining_not_used() {
        // tiers 为空 -> consumption 分支。`used` 为 None（CNY 余额没有意义的 used-%），
        // 但 remaining<=0 表示余额见底 -> 耗尽。旧代码读 `used`(None) -> 漏判。
        let usage = snap_consumption(Some(0.0), Some(1_700_000_000));
        assert_eq!(exhausted_reset_at(&usage), Some(1_700_000_000));
        // 余额仍为正 -> 未耗尽 -> None。
        assert_eq!(
            exhausted_reset_at(&snap_consumption(Some(48.77), Some(1_700_000_000))),
            None
        );
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exhausted_reset_at_consumption_depleted_uses_remaining_not_used`
Expected: FAIL —— 第一断言得 `None`（旧 `if exhausted(usage.used)`：`used=None` → false → None），期望 `Some(1_700_000_000)`。

- [ ] **Step 3: 改 `exhausted_reset_at`**（~L1014）

把无-tier 分支替换为：

```rust
fn exhausted_reset_at(usage: &UsageSnapshot) -> Option<i64> {
    let exhausted = |used_pct: Option<f64>| used_pct.map(|u| u >= EXHAUSTED_PCT).unwrap_or(false);
    if !usage.tiers.is_empty() {
        return usage
            .tiers
            .iter()
            .filter(|t| exhausted(t.used_pct))
            .filter_map(|t| t.reset_at)
            .max();
    }
    // 无 tier 明细 → consumption（余额）型。耗尽 = 余额见底。
    // （旧：`exhausted(usage.used)` —— 把 CNY 金额当百分比比较；DeepSeek reset_at=None，
    // 故此处仍返回 None → 调用方回落 cooldown，行为不变；但判断现在对带 reset_at 的未来
    // consumption 厂商也正确。）
    let depleted = usage.remaining.map(|r| r <= 0.0).unwrap_or(false);
    if depleted {
        return usage.reset_at;
    }
    None
}
```

- [ ] **Step 4: 跑测试看通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exhausted_reset_at`
Expected: 全 passed（新测试 + 原 `single_aggregate_*` / `multi_window_*` —— 原 `snap_used` 的 `remaining` 由 `used` 一致派生，仍满足 `remaining<=0` 当且仅当 `used>=100`）。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "fix(dispatch): consumption exhaustion checks remaining<=0 not used%"
```

---

## Task 4: Rust — 删除 `UsageSnapshot.used` 字段

> 此时 `used` 运行期零读者（tooltip 走 helper、breaker 走 remaining）。本 task 是机械重构：删除结构体字段 + 所有字面量/断言里的 `used`。**一个 task 内全部改完再编译**（部分删除会编译失败）。验证靠既有测试套件保持全绿。

**Files:**
- Modify: `src-tauri/src/usage/mod.rs`（结构体 + `snapshot_from_tiers` 字面量 + cache 测试 helper/断言）
- Modify: `src-tauri/src/usage/qianwen.rs`（字面量）
- Modify: `src-tauri/src/usage/zhipu.rs`（raw 兜底字面量 + 3 处断言）
- Modify: `src-tauri/src/usage/deepseek.rs`（字面量 + 局部变量 + 2 处断言）
- Modify: `src-tauri/src/usage/volcengine.rs`（2 处断言）
- Modify: `src-tauri/src/tray.rs`（6 处测试字面量）
- Modify: `src-tauri/src/proxy/dispatch.rs`（`snap_used` / `snap_consumption` / `snap_tiers` 三个 helper 字面量）

- [ ] **Step 1: 删结构体字段与 `snapshot_from_tiers` 赋值**（`src-tauri/src/usage/mod.rs`）

- 从 `pub struct UsageSnapshot { ... }`（~L23）删除一行：`pub used: Option<f64>,`
- 在 `snapshot_from_tiers`（~L119 的 `UsageSnapshot { ... }`）删除字面量里的 `used,` 这一项（保留局部变量 `used`，它仍驱动 `total`/`remaining`）。

- [ ] **Step 2: 更新 mod.rs 测试**（`src-tauri/src/usage/mod.rs` 的 `mod tests`）

- `fn snap(used: f64)`（~L299）字面量删除 `used: Some(used),`（保留参数名与 `remaining: Some(100.0 - used)`）。
- `cache_hit_within_ttl`（~L262）断言改为读 `remaining`：
  ```rust
      assert_eq!(c.get("zhipu", 1059).map(|s| s.remaining), Some(Some(50.0)));
  ```

- [ ] **Step 3: qianwen.rs**（`src-tauri/src/usage/qianwen.rs`）

- `map_usage` 字面量（~L79）删除 `used: None,`。

- [ ] **Step 4: zhipu.rs**（`src-tauri/src/usage/zhipu.rs`）

- raw 兜底字面量（~L294）删除 `used: None,`。
- 测试 `zhipu_parses_tokens_limit`（~L439）：`assert_eq!(snap.used, Some(42.0));` 改为
  ```rust
      assert_eq!(snap.tiers[0].used_pct, Some(42.0));
  ```
- 测试 `zhipu_missing_tokens_limit_returns_raw`（~L476）：删除 `assert_eq!(snap.used, None);` 这一行（保留 `raw_summary` 断言）。
- 测试 `zhipu_query_subscription_failure_leaves_plan_info_none`（~L600）：`assert_eq!(snap.used, Some(42.0));` 改为
  ```rust
      assert_eq!(snap.tiers[0].used_pct, Some(42.0)); // usage tiers intact
  ```

- [ ] **Step 5: deepseek.rs**（`src-tauri/src/usage/deepseek.rs`）

- 删除局部变量 `let used = (topped_up - total_balance).max(0.0);`（~L84）。
- 字面量（~L87）删除 `used: Some(used),`。
- 测试 ~L167：删除 `assert_eq!(snap.used, Some(51.23));`（保留 total/remaining 断言）。
- 测试 ~L326：删除 `assert_eq!(snap.used, Some(100.0));`。

- [ ] **Step 6: volcengine.rs**（`src-tauri/src/usage/volcengine.rs`）

- 测试 `snapshot_picks_primary_and_lists_tiers`（~L1004）：`assert_eq!(snap.used, Some(25.0));` 改为
  ```rust
      assert_eq!(snap.tiers[0].used_pct.unwrap(), 25.0);
  ```
- 测试 `snapshot_single_tier`（~L1032）：`assert_eq!(snap.used, Some(42.0));` 改为
  ```rust
      assert_eq!(snap.tiers[0].used_pct.unwrap(), 42.0);
  ```

- [ ] **Step 7: tray.rs 测试字面量**（`src-tauri/src/tray.rs`）—— 删除每处 `UsageSnapshot { ... }` 字面量里的 `used:` 项：

- ~L418（`account_label_consumption_shows_balance`）：删 `used: Some(51.23),`
- ~L444（`account_label_none_when_no_usage_value`）：删 `used: None,`
- ~L468（`spec_accounts_lists_usage_per_provider_in_order`）：删 `used: Some(51.23),`
- ~L509（`spec_accounts_hides_present_but_empty_snapshot`）：删 `used: None,`
- ~L602（`spec_tooltip_lists_primary_values_in_order`）：删 `used: Some(51.23),`
- `snap_tiers` helper（~L633 字面量）：删 `used: five_hour,`（保留 `remaining: five_hour.map(|p| 100.0 - p)` 等）

- [ ] **Step 8: dispatch.rs 测试 helper**（`src-tauri/src/proxy/dispatch.rs`）

- `snap_used`（~L1640 字面量）：删 `used,`（保留参数 `used` 与 `remaining: used.map(|u| (100.0 - u).max(0.0))`）。
- `snap_consumption`（Task 3 新增）：删 `used: None,`。
- `snap_tiers`（~L1655 字面量）：删 `used: None,`。

- [ ] **Step 9: 编译 + 全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过（无残留 `used` 字段引用），全部测试 passed。若编译报错指向某处仍引用 `.used` 或字面量含 `used:`，回到对应 Step 修正。

- [ ] **Step 10: 提交**

```bash
git add src-tauri/src/usage/mod.rs src-tauri/src/usage/qianwen.rs src-tauri/src/usage/zhipu.rs src-tauri/src/usage/deepseek.rs src-tauri/src/usage/volcengine.rs src-tauri/src/tray.rs src-tauri/src/proxy/dispatch.rs
git commit -m "refactor(usage): remove vestigial UsageSnapshot.used field"
```

---

## Task 5: TS — 概览 chip 改用 `primaryUsed` + 删 `types.ts.used`

**Files:**
- Modify: `src/lib/types.ts`（删 `UsageSnapshot.used` 字段 ~L92）
- Modify: `src/lib/quotaUtils.ts`（import 加 `UsageTier`；新增 `primaryUsed` + `windowLabel`；`QuotaMath` 加 `window`；改 `quotaMathFor` / `tooltipFor`）
- Test: 无 JS 测试框架；以 `pnpm build` 类型检查 + 运行 app 行为核对

**Interfaces:**
- Consumes: TS `UsageTier`（`types.ts`，已导出）。

- [ ] **Step 1: 改 `quotaUtils.ts`**（`src/lib/quotaUtils.ts`）

(a) 顶部 import 加 `UsageTier`：

```ts
import type { Provider, UsageEntry, UsageSnapshot, UsageTier } from "./types";
```

(b) 在 `round1`/`fmtReset` 附近（低-balance 常量之后）新增：

```ts
const PRIMARY_WINDOW_ORDER = ["five_hour", "weekly_limit", "monthly"] as const;

/** plan 主值：5h → 周 → 月 首个有值窗口；都没有则 null。 */
function primaryUsed(
  tiers: UsageTier[] | undefined,
): { pct: number; window: string; resetAt: number | null } | null {
  for (const w of PRIMARY_WINDOW_ORDER) {
    const t = tiers?.find((x) => x.window === w);
    if (t?.used_pct != null) {
      return { pct: t.used_pct, window: w, resetAt: t.reset_at ?? null };
    }
  }
  return null;
}

function windowLabel(w?: string): string {
  switch (w) {
    case "five_hour": return "5 小时窗口";
    case "weekly_limit": return "每周窗口";
    case "monthly": return "每月窗口";
    default: return "套餐窗口";
  }
}
```

(c) `QuotaMath` 接口（~L36）加一字段：

```ts
interface QuotaMath {
  billing: "plan" | "consumption";
  remaining: number; // plan: 0-100 remaining %; consumption: balance
  used?: number; // plan only — surfaces in tooltip
  window?: string; // plan only — which window the value came from (tooltip label)
  unit?: string; // consumption only
  resetAt?: number | null;
}
```

(d) `quotaMathFor`（~L49）plan 分支改为用 `primaryUsed`（替换原"仅 5h"逻辑）：

```ts
function quotaMathFor(snap: UsageSnapshot): QuotaMath | null {
  if (snap.billing_model === "consumption") {
    const remaining = snap.remaining;
    if (remaining == null) return null;
    return { billing: "consumption", remaining, unit: snap.unit };
  }
  const primary = primaryUsed(snap.tiers);
  if (!primary) return null;
  return {
    billing: "plan",
    remaining: Math.max(0, 100 - primary.pct),
    used: primary.pct,
    window: primary.window,
    resetAt: primary.resetAt,
  };
}
```

(e) `tooltipFor`（~L75）按窗口标注：

```ts
function tooltipFor(m: QuotaMath): string {
  if (m.billing === "consumption") return "按量付费余额（耗尽停机）";
  let tip = `${windowLabel(m.window)}：可用 ${round1(m.remaining)}%（已用 ${round1(m.used!)}%）`;
  if (m.resetAt != null) tip += `，重置于 ${fmtReset(m.resetAt)}`;
  return tip;
}
```

- [ ] **Step 2: 删 `types.ts.used`**（`src/lib/types.ts` ~L92）

删除一行：`used?: number | null;`（`UsageSnapshot` 接口内）。

- [ ] **Step 3: 类型检查 + 构建**

Run: `pnpm build`
Expected: vue-tsc 无类型错误（确认没有别处读 `snapshot.used`；此前 grep 仅命中 quotaUtils 局部变量与 types.ts）。vite 构建成功。

- [ ] **Step 4: 行为核对（运行 app）**

Run: `pnpm tauri dev`，登录千问套餐（触发 5h 缺、周档有的用量快照），核对：
- 托盘悬浮 tooltip 含千问一行（显示周档百分比）。
- 右键托盘「套餐用量」千问行依旧显示 `5h:– 周:..`。
- 概览页千问 chip 由「无数据」变为显示剩余%，悬浮 tooltip 标注「每周窗口：…」。

（若当前不便跑 app，至少保证 Step 3 类型检查通过即可提交；行为核对可在合并前补。）

- [ ] **Step 5: 提交**

```bash
git add src/lib/types.ts src/lib/quotaUtils.ts
git commit -m "fix(ui): quota chip uses primary window value (5h->周->月); drop snapshot.used"
```

---

## Self-Review（spec 覆盖核对）

| spec 章节 | 实现 task |
|---|---|
| §3.1 Rust `primary_used` | Task 1 |
| §3.2 托盘 tooltip 改 helper | Task 2（含千问回归测试） |
| §3.3 TS `primaryUsed` + chip + 窗口标注 | Task 5 |
| §3.4 删 `used`（结构体/赋值/IPC/类型） | Task 4（Rust）+ Task 5（types.ts） |
| §3.5 熔断 consumption 判断 `remaining<=0` | Task 3 |
| §6 测试 | Task 1–4 内联；Task 5 以 `pnpm build` + 行为核对 |
| §2 非目标（不改 recover 语义 / 不删 total/remaining / 不改 tier 扫描） | 均未触碰 ✓ |

- **Placeholder 扫描：** 无 TBD/TODO；每步含具体代码或确切命令。
- **类型一致性：** `primary_used: &[UsageTier] -> Option<(f64, &'static str)>` 在 Task 1/2 一致；TS `primaryUsed` 返回 `{pct,window,resetAt}|null` 在 Task 5 内 `quotaMathFor`/`tooltipFor` 一致；`QuotaMath.window` 新增后被两者使用。
- **编译顺序：** Task 1–3 期间 `used` 字段仍在 → 每步可独立编译/测试；Task 4 一次性删字段并修所有引用 → 编译通过；Task 5 独立于 Rust。
