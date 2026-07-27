# 时段转发策略（Time-based Forwarding Strategies）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a Profile route to different Models by time-of-day/day-of-week (day-repeat, cross-midnight supported), with a per-profile master switch and a backing 兜底 model — strategy picks the entry model, the existing breaker/fallback chain is untouched.

**Architecture:** New pure module `proxy/strategies.rs` does all time matching. `Clock` trait gains `now_local()` (FakeClock returns a fixed value → TZ-independent tests). `resolve_model` becomes time-aware and calls `profile_start_model`. Three UI surfaces: 路由 tab (only edit surface), 概览 (read-only effective chain), tray (route section removed).

**Tech Stack:** Rust (axum, tokio, serde, chrono 0.4), Tauri v2, Vue 3 + Pinia + Naive UI (Chinese UI).

**Spec:** `docs/superpowers/specs/2026-08-03-time-based-forwarding-strategies-design.md`

## Global Constraints

- **Timezone:** match against local system time only (user decision). No app-level TZ config.
- **chrono:** use fully-qualified paths (`chrono::Local`, `chrono::Datelike`) — there are ZERO `use chrono::` imports in `src-tauri/src/`; follow that convention (function-local `use` for `Datelike`/`Timelike` is OK).
- **serde:** snake_case, no `rename_all` on structs — EXCEPT `StrategyKind` which is `#[serde(tag = "type", rename_all = "snake_case")]` (internally tagged: `{"type":"time",...}`).
- **Backward compat:** every new `Profile` field is `#[serde(default ...)]`; an old `app_config.json` without the fields deserializes to `strategies: []`, `strategies_enabled: true`.
- **Logs identify vendor + upstream model, never opaque ids** — the strategy-match log line uses profile name + strategy id (config-level, not a model id).
- **UI is Chinese-language.** Error/validation messages are Chinese user-facing strings.
- **Canonical frontend pattern:** after every mutating action, re-fetch the affected slice from backend (no optimistic local mutation).
- **Tauri IPC:** camelCase JS arg keys auto-convert to snake_case Rust params.
- **Test commands:** backend `cargo test --manifest-path src-tauri/Cargo.toml <name>`; frontend `npx vue-tsc --noEmit` then `npm run build`.
- **Commit policy:** work directly on `main`; commit per task with `git add <explicit paths>` (never `git add -A`); push in batches later, not per-task.

---

## File Structure

**Backend (Rust) — create/modify:**
- `src-tauri/src/config/types.rs` — `Strategy`, `StrategyKind`, `TimeStrategy`, `Profile` fields + `impl Default for Profile`.
- `src-tauri/src/proxy/strategies.rs` — **CREATE.** Pure time-matching (`LocalNow`, `days_contains`, `prev_weekday`, `strategy_matches`, `select_strategy`, `profile_start_model`).
- `src-tauri/src/proxy/health.rs` — `LocalNow` struct + `Clock::now_local()` on trait/SystemClock/FakeClock.
- `src-tauri/src/proxy/mod.rs` — re-export `strategies` module + `LocalNow`.
- `src-tauri/src/proxy/resolve.rs` — `resolve_model` gains `now: &LocalNow`, calls `profile_start_model`.
- `src-tauri/src/proxy/dispatch.rs` — compute `state.clock.now_local()`, pass to `resolve_model`.
- `src-tauri/src/commands.rs` — strategy validation+normalization in `upsert_profile`; new `route_effective_models`, `set_profile_strategies_enabled`; (Task 12) remove `set_profile_backing*`.
- `src-tauri/src/lib.rs` — register new commands; (Task 12) unregister `set_profile_backing`.
- `src-tauri/src/tray.rs` — remove the route section entirely.

**Frontend (Vue/TS) — create/modify:**
- `src/lib/types.ts` — `Strategy`/`StrategyKind`/`TimeStrategy`/`RouteEffective`, `Profile` fields.
- `src/lib/commands.ts` — `getRouteEffectiveModels`, `setProfileStrategiesEnabled`; (Task 12) remove `setProfileBacking`.
- `src/stores/config.ts` — `routeEffective` state + `refreshEffective` + `setStrategiesEnabled`; (Task 12) remove `setBacking`.
- `src/views/Profiles.vue` — strategies editor (modal) + effective-model/master-switch (list card).
- `src/views/Dashboard.vue` — read-only chain, head = effective model; drop `switchModel`/`modelOptions`.
- `src/components/RouteLine.vue` — primary node read-only (drop `<NSelect>` swap affordance).

---

### Task 1: Data model — Strategy types + Profile fields

**Files:**
- Modify: `src-tauri/src/config/types.rs`
- Test: `src-tauri/src/config/types.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub struct Strategy { id, priority, enabled, kind }`, `pub enum StrategyKind` (tagged), `pub struct TimeStrategy { days_of_week, time_start, time_end, model_id }`, `Profile.strategies: Vec<Strategy>`, `Profile.strategies_enabled: bool`, `impl Default for Profile`. Later tasks import these from `crate::config`.

- [ ] **Step 1: Write failing tests** — append to the `tests` module in `types.rs`:

```rust
    #[test]
    fn profile_strategies_default_when_absent() {
        // Old config JSON (pre-feature) omits strategies + strategies_enabled.
        let json = r#"{"id":"p","name":"glm","backing_model_id":"m"}"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert!(p.strategies.is_empty());
        assert!(p.strategies_enabled); // default true → empty set is a no-op
    }

    #[test]
    fn profile_with_strategy_roundtrips() {
        let p = Profile {
            id: "p".into(), name: "glm".into(), aliases: vec![],
            backing_model_id: "m".into(),
            strategies: vec![Strategy {
                id: "s1".into(), priority: 2, enabled: true,
                kind: StrategyKind::Time(TimeStrategy {
                    days_of_week: vec![1, 2, 3, 4, 5],
                    time_start: 1320, time_end: 480, model_id: "m2".into(),
                }),
            }],
            strategies_enabled: true,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        // Internally-tagged kind serializes as {"type":"time",...}
        assert!(json.contains(r#""type":"time""#));
        assert!(json.contains(r#""days_of_week":[1,2,3,4,5]"#));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: COMPILE FAIL (`Strategy` / `StrategyKind` / `TimeStrategy` not defined; `Profile` has no `strategies` field).

- [ ] **Step 3: Add the types + extend Profile** — insert before the `ContextCheckStatus` enum (around line 178) in `types.rs`:

```rust
/// 一条转发策略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Strategy {
    pub id: String,
    /// 1..=10，数字越小优先级越高。多条命中取最小；并列按列表顺序（靠前胜）。
    pub priority: u8,
    /// 单策略启停（默认 true）。总开关 `Profile.strategies_enabled` 才是"一键回退"。
    #[serde(default = "default_strategy_enabled")]
    pub enabled: bool,
    pub kind: StrategyKind,
}
fn default_strategy_enabled() -> bool { true }

/// 策略类型（tag 表示，序列化为 {"type":"time", ...}），预留扩展。
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
    pub days_of_week: Vec<u8>,
    pub time_start: u16,
    pub time_end: u16,
    pub model_id: String,
}
```

Then extend the `Profile` struct (around line 170) — replace the existing struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 兜底模型：无策略命中/失效/总开关关时的入口模型。
    pub backing_model_id: String,
    #[serde(default)]
    pub strategies: Vec<Strategy>,
    /// 总开关：false 跳过全部策略，直接走兜底。默认 true（空集即无操作）。
    #[serde(default = "default_strategies_enabled")]
    pub strategies_enabled: bool,
}
fn default_strategies_enabled() -> bool { true }
```

Add a manual `Default` impl immediately after the struct (default `strategies_enabled = true`, which `#[derive(Default)]` would NOT give since bool defaults to false):

```rust
impl Default for Profile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            aliases: vec![],
            backing_model_id: String::new(),
            strategies: vec![],
            strategies_enabled: true,
        }
    }
}
```

- [ ] **Step 4: Fix every `Profile { ... }` literal** — adding fields broke them. Compile tests to find them all:

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`

For each compile error on a `Profile { ... }` literal, append `, ..Default::default()` before the closing brace. Known sites (the compiler will confirm the full list):
- `src-tauri/src/config/types.rs` — `app_config_roundtrips` test
- `src-tauri/src/proxy/resolve.rs` — `sample()` helper
- `src-tauri/src/proxy/dispatch.rs` — `chain_state()`, the `deepseek_402_falls_back` test, the `fallback_cycle_breaks` test
- `src-tauri/src/commands.rs` — `mk_profile()` helper
- `src-tauri/src/tray.rs` — every test `Profile { ... }` (≈5 sites)

Example fix (`resolve.rs` `sample()`):
```rust
            profiles: vec![Profile {
                id: "p_main".into(),
                name: "glm-5.2".into(),
                aliases: vec!["claude-sonnet-4".into()],
                backing_model_id: "m_glm46".into(),
                ..Default::default()
            }],
```

- [ ] **Step 5: Run all tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (new tests pass; existing tests unaffected — profiles have no strategies → behave as before).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/proxy/resolve.rs src-tauri/src/proxy/dispatch.rs src-tauri/src/commands.rs src-tauri/src/tray.rs
git commit -m "feat(config): add Strategy/TimeStrategy types + Profile.strategies fields"
```

---

### Task 2: Clock::now_local() + LocalNow

**Files:**
- Modify: `src-tauri/src/proxy/health.rs:8-37` (Clock trait + SystemClock + FakeClock)
- Modify: `src-tauri/src/proxy/mod.rs:12` (re-export `LocalNow`)

**Interfaces:**
- Produces: `pub struct LocalNow { pub weekday: u8, pub minute: u16 }` (in `health.rs`, `Copy`), `Clock::now_local(&self) -> LocalNow`, `FakeClock::set_local(&self, weekday: u8, minute: u16)`. Consumed by Task 3/4/5.

- [ ] **Step 1: Write failing test** — append to the `tests` module in `health.rs` (create one if absent; mirror the repo's `#[cfg(test)] mod tests` style):

```rust
    #[test]
    fn fake_clock_now_local_is_settable() {
        let clock = FakeClock::new(1000);
        assert_eq!(clock.now_secs(), 1000);
        clock.set_local(3, 1350); // Wednesday 22:30
        let ln = clock.now_local();
        assert_eq!(ln.weekday, 3);
        assert_eq!(ln.minute, 1350);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml fake_clock_now_local_is_settable`
Expected: FAIL (`now_local` not defined / `set_local` not found).

- [ ] **Step 3: Add LocalNow + extend Clock, SystemClock, FakeClock**

In `health.rs`, add `LocalNow` near the top (after the `use` lines):

```rust
/// 本地时间部件（时区转换仅在 `Clock::now_local` 一处完成）。1=周一..=7=周日；minute 0..=1439。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalNow {
    pub weekday: u8,
    pub minute: u16,
}
```

Extend the trait + impls (replace lines 8-37):

```rust
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> i64;
    fn now_local(&self) -> LocalNow;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
    fn now_local(&self) -> LocalNow {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        LocalNow {
            weekday: now.weekday().number_from_monday() as u8, // Mon=1..Sun=7
            minute: now.hour() as u16 * 60 + now.minute() as u16,
        }
    }
}

pub struct FakeClock {
    inner: Mutex<i64>,
    local: Mutex<LocalNow>,
}
impl FakeClock {
    pub fn new(secs: i64) -> Self {
        Self { inner: Mutex::new(secs), local: Mutex::new(LocalNow { weekday: 1, minute: 0 }) }
    }
    pub fn advance(&self, secs: i64) {
        *self.inner.lock().unwrap() += secs;
    }
    pub fn set_local(&self, weekday: u8, minute: u16) {
        *self.local.lock().unwrap() = LocalNow { weekday, minute };
    }
}
impl Clock for FakeClock {
    fn now_secs(&self) -> i64 {
        *self.inner.lock().unwrap()
    }
    fn now_local(&self) -> LocalNow {
        *self.local.lock().unwrap()
    }
}
```

Re-export `LocalNow` from `proxy/mod.rs:12`:

```rust
pub use health::{Clock, HealthRegistry, LocalNow, ModelHealth, SystemClock};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml fake_clock_now_local_is_settable`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/health.rs src-tauri/src/proxy/mod.rs
git commit -m "feat(proxy): Clock::now_local() + LocalNow (FakeClock-settable)"
```

---

### Task 3: strategies.rs — pure matchers

**Files:**
- Create: `src-tauri/src/proxy/strategies.rs`
- Modify: `src-tauri/src/proxy/mod.rs` (add `pub mod strategies;`)

**Interfaces:**
- Consumes: `LocalNow` (Task 2), `Strategy`/`StrategyKind`/`TimeStrategy` (Task 1).
- Produces: `days_contains(&[u8], u8) -> bool`, `prev_weekday(u8) -> u8`, `strategy_matches(&TimeStrategy, u8, u16) -> bool`.

- [ ] **Step 1: Write failing tests** — create `src-tauri/src/proxy/strategies.rs` with the test module first:

```rust
use crate::config::{Strategy, StrategyKind, TimeStrategy};
use crate::proxy::health::LocalNow;

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(days: &[u8], start: u16, end: u16) -> TimeStrategy {
        TimeStrategy { days_of_week: days.to_vec(), time_start: start, time_end: end, model_id: "m".into() }
    }

    // ---- prev_weekday (周一↔周日 wrap) ----
    #[test]
    fn prev_weekday_wraps_monday_to_sunday() {
        assert_eq!(prev_weekday(1), 7); // Mon ← Sun
        assert_eq!(prev_weekday(2), 1);
        assert_eq!(prev_weekday(7), 6);
    }

    // ---- same-day window [start, end): include start, exclude end ----
    #[test]
    fn same_day_window_half_open() {
        let s = ts(&[2], 840, 1080); // Tue 14:00–18:00
        assert!(strategy_matches(&s, 2, 840));  // 14:00 include
        assert!(strategy_matches(&s, 2, 1079)); // 17:59 include
        assert!(!strategy_matches(&s, 2, 1080)); // 18:00 exclude
        assert!(!strategy_matches(&s, 2, 839));  // 13:59 before
        assert!(!strategy_matches(&s, 3, 900));  // wrong day
    }

    // ---- cross-midnight: evening looks at today, morning looks at YESTERDAY ----
    #[test]
    fn cross_midnight_evening_uses_today() {
        let s = ts(&[2], 1320, 480); // start Tue 22:00 → Wed 08:00
        assert!(strategy_matches(&s, 2, 1320));  // Tue 22:00 (today=Tue ✓)
        assert!(strategy_matches(&s, 2, 1439));  // Tue 23:59
        assert!(!strategy_matches(&s, 3, 1320)); // Wed 22:00? today=Wed not in [2]
    }

    #[test]
    fn cross_midnight_morning_uses_yesterday() {
        let s = ts(&[2], 1320, 480); // start Tue 22:00 → Wed 08:00
        assert!(strategy_matches(&s, 3, 0));    // Wed 00:00: yesterday=Tue ✓
        assert!(strategy_matches(&s, 3, 479));  // Wed 07:59 (< 480)
        assert!(!strategy_matches(&s, 3, 480)); // Wed 08:00 exclude
        assert!(!strategy_matches(&s, 4, 100)); // Thu 01:00: yesterday=Wed not in [2]
    }

    #[test]
    fn cross_midnight_morning_monday_looks_at_sunday() {
        let s = ts(&[7], 1320, 480); // start Sun 22:00 → Mon 08:00
        assert!(strategy_matches(&s, 1, 100));  // Mon 01:00: yesterday=Sun ✓
    }

    #[test]
    fn days_contains_membership() {
        assert!(days_contains(&[1, 3, 5], 3));
        assert!(!days_contains(&[1, 3, 5], 2));
        assert!(!days_contains(&[], 1));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: COMPILE FAIL (`days_contains` etc. not defined).

- [ ] **Step 3: Add the module declaration + matchers.** In `proxy/mod.rs` add (with the other `pub mod` lines):

```rust
pub mod strategies;
```

Append the implementations to `strategies.rs` (above the `#[cfg(test)]` block):

```rust
/// `weekday` (1=Mon..=7=Sun) is in the `days` set.
pub fn days_contains(days: &[u8], weekday: u8) -> bool {
    days.iter().any(|d| *d == weekday)
}

/// Previous day, wrapping Monday→Sunday. 1→7, else n-1.
pub fn prev_weekday(weekday: u8) -> u8 {
    if weekday <= 1 { 7 } else { weekday - 1 }
}

/// Does this time strategy match `(weekday, minute)`?
///  - `start < end` (same-day): `days_contains(weekday) && minute ∈ [start, end)`
///  - `start > end` (cross-midnight; equality rejected at validation):
///      `minute >= start` (evening) ⇒ `days_contains(weekday)`
///      `minute <  end` (morning)  ⇒ `days_contains(prev_weekday(weekday))` (window started yesterday)
pub fn strategy_matches(s: &TimeStrategy, weekday: u8, minute: u16) -> bool {
    if s.time_start < s.time_end {
        days_contains(&s.days_of_week, weekday) && minute >= s.time_start && minute < s.time_end
    } else if minute >= s.time_start {
        days_contains(&s.days_of_week, weekday)
    } else {
        days_contains(&s.days_of_week, prev_weekday(weekday))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml strategies`
Expected: all PASS (boundary points, cross-midnight yesterday, wrap).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/strategies.rs src-tauri/src/proxy/mod.rs
git commit -m "feat(proxy): pure time-strategy matchers (day-set + cross-midnight)"
```

---

### Task 4: strategies.rs — select_strategy + profile_start_model

**Files:**
- Modify: `src-tauri/src/proxy/strategies.rs`

**Interfaces:**
- Consumes: `strategy_matches` (Task 3), `AppConfig`/`Profile`/`Strategy` (Task 1).
- Produces: `select_strategy(&[Strategy], &LocalNow) -> Option<&Strategy>`, `profile_start_model(&Profile, &AppConfig, &LocalNow) -> (&str, Option<&str>)`. Consumed by Task 5 (`resolve_model`) and Task 6 (`route_effective_models`).

- [ ] **Step 1: Write failing tests** — append to the `tests` module in `strategies.rs`:

```rust
    use crate::config::{AppConfig, Profile};

    fn strat(id: &str, priority: u8, enabled: bool, days: &[u8], start: u16, end: u16, model: &str) -> Strategy {
        Strategy {
            id: id.into(), priority, enabled,
            kind: StrategyKind::Time(TimeStrategy { days_of_week: days.to_vec(), time_start: start, time_end: end, model_id: model.into() }),
        }
    }
    fn now(w: u8, m: u16) -> LocalNow { LocalNow { weekday: w, minute: m } }

    #[test]
    fn select_strategy_min_priority_wins() {
        let s = vec![
            strat("a", 5, true, &[2], 0, 1439, "ma"),   // matches Tue
            strat("b", 2, true, &[2], 0, 1439, "mb"),   // matches Tue, higher prio
        ];
        let win = select_strategy(&s, &now(2, 500)).unwrap();
        assert_eq!(win.id, "b");
    }

    #[test]
    fn select_strategy_tie_breaks_by_list_order() {
        let s = vec![
            strat("a", 3, true, &[2], 0, 1439, "ma"),
            strat("b", 3, true, &[2], 0, 1439, "mb"),  // same prio, later
        ];
        assert_eq!(select_strategy(&s, &now(2, 500)).unwrap().id, "a");
    }

    #[test]
    fn select_strategy_skips_disabled_and_non_matching() {
        let s = vec![
            strat("a", 1, false, &[2], 0, 1439, "ma"),  // disabled
            strat("b", 1, true, &[3], 0, 1439, "mb"),   // wrong day
        ];
        assert!(select_strategy(&s, &now(2, 500)).is_none());
    }

    fn cfg_with(profile: Profile) -> AppConfig {
        let mut c = AppConfig::default();
        c.profiles.push(profile);
        c
    }

    #[test]
    fn profile_start_model_uses_strategy_when_matched() {
        let mut p = Profile { id: "p".into(), name: "n".into(), aliases: vec![], backing_model_id: "mback".into(), ..Default::default() };
        p.strategies_enabled = true;
        p.strategies = vec![strat("s", 1, true, &[2], 0, 1439, "mstrat")];
        let mut cfg = cfg_with(p);
        cfg.models.push(crate::config::Model { id: "mstrat".into(), provider_id: "pr".into(), upstream_model_id: "u".into(), ..Default::default() });
        cfg.models.push(crate::config::Model { id: "mback".into(), provider_id: "pr".into(), upstream_model_id: "ub".into(), ..Default::default() });
        let (mid, via) = profile_start_model(&cfg.profiles[0], &cfg, &now(2, 500));
        assert_eq!(mid, "mstrat");
        assert_eq!(via, Some("s"));
    }

    #[test]
    fn profile_start_model_master_off_ignores_strategies() {
        let mut p = Profile { id: "p".into(), name: "n".into(), aliases: vec![], backing_model_id: "mback".into(), ..Default::default() };
        p.strategies_enabled = false;
        p.strategies = vec![strat("s", 1, true, &[2], 0, 1439, "mstrat")];
        let cfg = cfg_with(p);
        let (mid, via) = profile_start_model(&cfg.profiles[0], &cfg, &now(2, 500));
        assert_eq!(mid, "mback");
        assert_eq!(via, None);
    }

    #[test]
    fn profile_start_model_falls_back_when_strategy_model_deleted() {
        let mut p = Profile { id: "p".into(), name: "n".into(), aliases: vec![], backing_model_id: "mback".into(), ..Default::default() };
        p.strategies_enabled = true;
        p.strategies = vec![strat("s", 1, true, &[2], 0, 1439, "mgone")]; // no model "mgone"
        let mut cfg = cfg_with(p);
        cfg.models.push(crate::config::Model { id: "mback".into(), provider_id: "pr".into(), upstream_model_id: "ub".into(), ..Default::default() });
        let (mid, via) = profile_start_model(&cfg.profiles[0], &cfg, &now(2, 500));
        assert_eq!(mid, "mback");
        assert_eq!(via, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: COMPILE FAIL (`select_strategy` / `profile_start_model` not defined).

- [ ] **Step 3: Implement.** Append to `strategies.rs` (above the test module):

```rust
use crate::config::{AppConfig, Profile, Strategy};

/// Among enabled strategies matching `now`, pick the winner: min `priority`;
/// ties broken by lowest list index. `None` if none match. Explicit tie-break
/// (do not rely on `min_by_key`'s tie semantics).
pub fn select_strategy<'a>(strategies: &'a [Strategy], now: &LocalNow) -> Option<&'a Strategy> {
    let mut best: Option<(&Strategy, usize)> = None;
    for (i, s) in strategies.iter().enumerate() {
        if !s.enabled { continue; }
        let matches = match &s.kind {
            StrategyKind::Time(t) => strategy_matches(t, now.weekday, now.minute),
        };
        if !matches { continue; }
        match best {
            None => best = Some((s, i)),
            Some((bs, bi)) => if (s.priority, i) < (bs.priority, bi) { best = Some((s, i)); },
        }
    }
    best.map(|(s, _)| s)
}

/// Single source of truth for "which model does this profile start at, and why".
/// Master off / winner model deleted → backing_model_id (via = None).
pub fn profile_start_model<'c>(
    profile: &'c Profile,
    cfg: &'c AppConfig,
    now: &LocalNow,
) -> (&'c str, Option<&'c str>) {
    if profile.strategies_enabled {
        if let Some(s) = select_strategy(&profile.strategies, now) {
            let mid: &str = match &s.kind { StrategyKind::Time(t) => &t.model_id };
            if cfg.models.iter().any(|m| m.id == mid) {
                return (mid, Some(&s.id));
            }
        }
    }
    (&profile.backing_model_id, None)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml strategies`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/strategies.rs
git commit -m "feat(proxy): select_strategy + profile_start_model (single source of truth)"
```

---

### Task 5: Wire resolve_model + dispatch

**Files:**
- Modify: `src-tauri/src/proxy/resolve.rs`
- Modify: `src-tauri/src/proxy/dispatch.rs`

**Interfaces:**
- Consumes: `profile_start_model` (Task 4), `Clock::now_local` (Task 2).
- Produces: `resolve_model(cfg, requested, now)` — the time-aware entry-model resolver used by `dispatch`.

- [ ] **Step 1: Write failing resolve test** — append to `resolve.rs` tests:

```rust
    use crate::config::{Strategy, StrategyKind, TimeStrategy};
    use crate::proxy::health::LocalNow;

    fn cfg_with_strategy() -> AppConfig {
        let mut cfg = sample(); // profile "glm-5.2" -> m_glm46 (backing)
        cfg.models.push(Model {
            id: "m_strat".into(), provider_id: "zhipu".into(),
            source: ModelSource::Manual, upstream_model_id: "glm-air".into(),
            cooldown_seconds: None, fallback_target_model_id: None, context_size: None,
        });
        cfg.profiles[0].strategies_enabled = true;
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_strat".into(),
            }),
        }];
        cfg
    }

    #[test]
    fn resolve_uses_strategy_model_when_matched() {
        let cfg = cfg_with_strategy();
        // Tuesday within [0,1439) → strategy model
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 2, minute: 500 }).unwrap();
        assert_eq!(m.id, "m_strat");
    }

    #[test]
    fn resolve_falls_to_backing_when_no_match() {
        let cfg = cfg_with_strategy();
        // Wednesday not in [2] → backing
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 3, minute: 500 }).unwrap();
        assert_eq!(m.id, "m_glm46");
    }

    #[test]
    fn resolve_uses_backing_when_master_off() {
        let mut cfg = cfg_with_strategy();
        cfg.profiles[0].strategies_enabled = false;
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 2, minute: 500 }).unwrap();
        assert_eq!(m.id, "m_glm46");
    }
```

Also update the existing 4 `resolve.rs` tests (`hits_by_name`, `hits_by_alias`, `miss_lists_available`, `missing_model_name_errors`) to pass a `&LocalNow` — e.g. `resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 1, minute: 0 })`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: COMPILE FAIL (`resolve_model` takes 2 args, not 3).

- [ ] **Step 3: Update `resolve_model`.** Replace the body of `resolve_model` in `resolve.rs`:

```rust
use crate::config::{AppConfig, Model};
use crate::proxy::health::LocalNow;
use crate::proxy::strategies::profile_start_model;
use thiserror::Error;

// ... ResolveError unchanged ...

pub fn resolve_model<'c>(
    cfg: &'c AppConfig,
    requested: Option<&str>,
    now: &LocalNow,
) -> Result<&'c Model, ResolveError> {
    let available: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
    let req = requested.unwrap_or("");
    let prof = cfg
        .profiles
        .iter()
        .find(|p| p.name == req || p.aliases.iter().any(|a| a == req))
        .ok_or_else(|| ResolveError::ProfileNotFound { requested: req.to_string(), available })?;
    let (model_id, via_strategy) = profile_start_model(prof, cfg, now);
    if let Some(sid) = via_strategy {
        tracing::info!(
            target: "switchlm::proxy",
            profile = %prof.name,
            strategy = %sid,
            "time-strategy matched"
        );
    }
    let model = cfg
        .models
        .iter()
        .find(|m| m.id == model_id)
        .ok_or_else(|| ResolveError::MissingBackingModel(model_id.to_string()))?;
    Ok(model)
}
```

- [ ] **Step 4: Run resolve tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml resolve`
Expected: PASS.

- [ ] **Step 5: Update dispatch to pass `now`.** In `dispatch.rs`, the config-read block (around line 59) — add `now` and pass it to `resolve_model`. Replace:

```rust
    let (start_model_id, echo_model, is_stream, vendor, upstream_model) = {
        let cfg = state.config.read().await;
        if cfg.profiles.is_empty() || cfg.models.is_empty() {
            return Err(ProxyError::NotConfigured);
        }
        let model = resolve_model(&cfg, requested_model)?;
```

with:

```rust
    let (start_model_id, echo_model, is_stream, vendor, upstream_model) = {
        let now = state.clock.now_local();
        let cfg = state.config.read().await;
        if cfg.profiles.is_empty() || cfg.models.is_empty() {
            return Err(ProxyError::NotConfigured);
        }
        let model = resolve_model(&cfg, requested_model, &now)?;
```

- [ ] **Step 6: Add the composition integration test** — a strategy-selected entry model that rate-limits must walk its own `fallback_target_model_id` chain. Append to `dispatch.rs` tests (mirrors the `deepseek_402_falls_back` pattern — custom cfg):

```rust
    #[tokio::test]
    async fn strategy_entry_model_rate_limits_walks_its_chain() {
        let mock_a = MockServer::start().await; // strategy model: rate-limits
        let mock_b = MockServer::start().await; // a's fallback: succeeds
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}]})))
            .mount(&mock_b).await;

        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider { id: "pa".into(), vendor: "zhipu".into(), display_name: "A".into(), openai_base_url: Some(mock_a.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.providers.push(Provider { id: "pb".into(), vendor: "pb".into(), display_name: "B".into(), openai_base_url: Some(mock_b.uri()), anthropic_base_url: None, usage_creds: None });
        cfg.models.push(mk_model("m_a", "pa", Some("m_b"))); // strategy model, falls back to m_b
        cfg.models.push(mk_model("m_b", "pb", None));
        secrets.set_key("pa", "sk-test").unwrap();
        secrets.set_key("pb", "sk-test").unwrap();
        cfg.profiles.push(Profile {
            id: "p".into(), name: "glm-5.2".into(), aliases: vec![],
            backing_model_id: "m_b".into(), strategies_enabled: true,
            strategies: vec![Strategy {
                id: "s".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_a".into() }),
            }],
            ..Default::default()
        });
        let clock = Arc::new(FakeClock::new(1000));
        clock.set_local(2, 500); // Tuesday → strategy matches → entry m_a
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg), secrets,
            catalog: ModelCatalog::default(), health: Default::default(), clock,
            usage_cache: Default::default(), bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None), bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
        });
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("from-b"));    // served by fallback m_b
        assert!(state.health.is_cooling("m_a", 1000));        // strategy entry tripped
    }
```

Add the missing imports at the top of the dispatch tests module if not already present: `use crate::config::{Strategy, StrategyKind, TimeStrategy};`.

- [ ] **Step 7: Run all dispatch tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml dispatch`
Expected: PASS (new composition test + existing fallback tests unchanged — their profiles have no strategies).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/proxy/resolve.rs src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): resolve_model time-aware; strategy picks entry, breaker chain walks"
```

---

### Task 6: Commands — validation, route_effective_models, set_profile_strategies_enabled

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs:151-199` (register two new commands)

**Interfaces:**
- Consumes: `profile_start_model` (Task 4), `Clock::now_local` (Task 2).
- Produces: `route_effective_models`, `set_profile_strategies_enabled` (Tauri commands); private `normalize_and_validate_strategies`.

- [ ] **Step 1: Write failing validation test** — append to `commands.rs` tests:

```rust
    use crate::config::{Strategy, StrategyKind, TimeStrategy};

    fn profile_with_strategy(start: u16, end: u16, days: Vec<u8>, priority: u8, model: &str) -> Profile {
        Profile {
            id: "p".into(), name: "p".into(), aliases: vec![], backing_model_id: "m".into(),
            strategies_enabled: true,
            strategies: vec![Strategy {
                id: "s".into(), priority, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: days, time_start: start, time_end: end, model_id: model.into() }),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn validate_rejects_start_equal_end() {
        let cfg = AppConfig::default();
        let mut p = profile_with_strategy(500, 500, vec![1], 1, "m");
        assert!(normalize_and_validate_strategies(&cfg, &mut p).is_err());
    }

    #[test]
    fn validate_rejects_bad_priority_and_days_and_dangling_model() {
        let cfg = AppConfig::default();
        let mut p = profile_with_strategy(0, 100, vec![1], 11, "m"); // priority 11
        assert!(normalize_and_validate_strategies(&cfg, &mut p).is_err());
        let mut p = profile_with_strategy(0, 100, vec![8], 1, "m"); // weekday 8
        assert!(normalize_and_validate_strategies(&cfg, &mut p).is_err());
        let mut p = profile_with_strategy(0, 100, vec![], 1, "m"); // empty days
        assert!(normalize_and_validate_strategies(&cfg, &mut p).is_err());
        let mut p = profile_with_strategy(0, 100, vec![1], 1, "nope"); // dangling model
        assert!(normalize_and_validate_strategies(&cfg, &mut p).is_err());
    }

    #[test]
    fn validate_normalizes_days_dedup_sort() {
        let cfg = AppConfig::default();
        let mut p = profile_with_strategy(0, 100, vec![3, 1, 1, 2], 1, "m");
        normalize_and_validate_strategies(&cfg, &mut p).unwrap();
        assert_eq!(p.strategies[0].kind.as_time().unwrap().days_of_week, vec![1, 2, 3]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: COMPILE FAIL (`normalize_and_validate_strategies` not defined; `as_time` not defined).

- [ ] **Step 3: Implement validation + an `as_time` helper.** Add to `commands.rs` (near the other helpers, ~line 890):

```rust
use crate::config::{Strategy, StrategyKind, TimeStrategy};

/// Normalize (dedup+sort days_of_week) then validate every strategy on the profile.
/// Returns Err(Chinese message) on the first violation. Mutates `profile.strategies`.
fn normalize_and_validate_strategies(cfg: &AppConfig, profile: &mut Profile) -> Result<(), String> {
    for s in &mut profile.strategies {
        let t = match &mut s.kind {
            StrategyKind::Time(t) => t,
        };
        if !(1..==10).contains(&s.priority) {
            return Err("策略优先级必须在 1~10 之间".into());
        }
        t.days_of_week.sort_unstable();
        t.days_of_week.dedup();
        if t.days_of_week.is_empty() {
            return Err("策略至少需选择一个星期".into());
        }
        if t.days_of_week.iter().any(|d| !(1..=7).contains(d)) {
            return Err("星期必须在 1~7 之间".into());
        }
        if t.time_start > 1439 || t.time_end > 1439 {
            return Err("时间必须在 00:00~23:59 之间".into());
        }
        if t.time_start == t.time_end {
            return Err("策略开始时间不能等于结束时间".into());
        }
        if !cfg.models.iter().any(|m| m.id == t.model_id) {
            return Err(format!("策略目标模型「{}」不存在", t.model_id));
        }
    }
    Ok(())
}
```

Add an `as_time` accessor on `StrategyKind` (in `config/types.rs`, near the enum):

```rust
impl StrategyKind {
    pub fn as_time(&self) -> Option<&TimeStrategy> {
        match self { StrategyKind::Time(t) => Some(t) }
    }
}
```

Wire it into `upsert_profile` (replace lines 221-234):

```rust
#[tauri::command]
pub async fn upsert_profile(
    state: State<'_, AppState>,
    mut profile: Profile,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        normalize_and_validate_strategies(&cfg, &mut profile)?;
        upsert(&mut cfg.profiles, profile);
        persist(&app, &cfg)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}
```

- [ ] **Step 4: Run validation tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml validate_`
Expected: PASS.

- [ ] **Step 5: Add `route_effective_models` + `set_profile_strategies_enabled`.** Add to `commands.rs` (profile cluster, ~line 250):

```rust
/// Per-profile currently-effective entry model + the strategy that matched (if any).
/// Single source of truth for 概览 / 路由 list display (computed via `profile_start_model`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteEffective {
    pub profile_id: String,
    pub effective_model_id: String,
    pub via_strategy_id: Option<String>,
}

#[tauri::command]
pub async fn route_effective_models(state: State<'_, AppState>) -> Result<Vec<RouteEffective>, String> {
    let cfg = state.config.read().await;
    let now = state.clock.now_local();
    Ok(cfg.profiles.iter().map(|p| {
        let (mid, via) = crate::proxy::strategies::profile_start_model(p, &cfg, &now);
        RouteEffective { profile_id: p.id.clone(), effective_model_id: mid.to_string(), via_strategy_id: via.map(str::to_string) }
    }).collect())
}

/// One-click master switch: toggle a profile's `strategies_enabled` and persist.
#[tauri::command]
pub async fn set_profile_strategies_enabled(
    state: State<'_, AppState>,
    profile_id: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        let prof = cfg.profiles.iter_mut().find(|p| p.id == profile_id)
            .ok_or_else(|| format!("profile {profile_id} not found"))?;
        prof.strategies_enabled = enabled;
        persist(&app, &cfg)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}
```

- [ ] **Step 6: Register both commands** in `lib.rs` — add to the `generate_handler!` list (near the profile commands, ~line 166 after `delete_profile`):

```rust
            commands::delete_profile,
            commands::route_effective_models,
            commands::set_profile_strategies_enabled,
```

- [ ] **Step 7: Build + run all backend tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no warnings about unused.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/config/types.rs src-tauri/src/lib.rs
git commit -m "feat(cmds): strategy validation + route_effective_models + master switch"
```

---

### Task 7: Remove the tray route section

**Files:**
- Modify: `src-tauri/src/tray.rs`

**Interfaces:** None (purely subtractive; the tray no longer surfaces routes or accepts model-swap clicks).

- [ ] **Step 1: Update tray spec tests to expect no route section.** In the `tray.rs` test module, replace assertions that reference `spec.profiles` with assertions that routes are gone. For each test that built a `TrayMenuSpec` and checked `spec.profiles`, change it to assert the accounts/tooltip path only (or delete the now-irrelevant profile assertions). Concretely, remove/replace these tests: any `spec_profiles_*` test, and the `❄` cooling-flag test, and the multi-vendor profile test — they exercised profile menu options that no longer exist. Keep `spec_accounts_*`-style tests if present.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: COMPILE FAIL (`ProfileMenuSpec`/`ModelOptionSpec` fields still referenced; tests reference removed fields).

- [ ] **Step 3: Remove the route section from the tray.** In `tray.rs`:
  - Delete `ProfileMenuSpec`, `ModelOptionSpec` structs and their fields on `TrayMenuSpec` (keep `accounts` + `tooltip`).
  - Delete the `BACKING_PREFIX`, `ID_JOIN`, `backing_menu_id` helper, the `provider_spec`/`model_label` helpers **only if they become unused** (the compiler will tell you — `provider_spec` is also used by accounts; keep what accounts still need).
  - Delete the `profiles` building block in the spec builder (the `cfg.profiles.iter().map(...)` → `ProfileMenuSpec` block, ~lines 234-257) and the `profiles.sort_by_key(...)` line.
  - Delete the `profiles_sub` rendering block in `build_menu` (~lines 300-326, the whole `if spec.profiles.is_empty() / else { profiles_sub ... }`).
  - Delete the `backing:` click-handler branch (~line 446-452) in the menu event handler.

  The `TrayMenuSpec` builder now produces only `accounts` + `tooltip`; `build_menu` appends only the 套餐用量 section.

- [ ] **Step 4: Fix the spec builder signature + callers**

The builder no longer needs `health` or `usage` for profiles, but it still needs `usage` for accounts. Keep the existing `health`/`usage` params if accounts still use `usage` (they do). Remove now-unused imports/vars the compiler flags.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "refactor(tray): remove route quick-config section (read-only app-window only)"
```

---

### Task 8: Frontend bridge + store plumbing (add new; keep setBacking for now)

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/commands.ts`
- Modify: `src/stores/config.ts`

**Interfaces:**
- Produces: TS `Strategy`/`StrategyKind`/`TimeStrategy`/`RouteEffective`, `Profile.strategies?` + `strategies_enabled?`; `getRouteEffectiveModels`, `setProfileStrategiesEnabled` wrappers; config store `routeEffective` + `refreshEffective` + `setStrategiesEnabled`. (Do NOT remove `setBacking`/`setProfileBacking` yet — Dashboard still uses them until Task 11.)

- [ ] **Step 1: Add TS types** in `src/lib/types.ts` — extend `Profile` (replace lines 29-34) and append the new interfaces:

```ts
export interface Profile {
  id: string;
  name: string;
  aliases?: string[];
  backing_model_id: string;
  strategies?: Strategy[];
  strategies_enabled?: boolean;
}

// Mirrors src-tauri StrategyKind (internally tagged: {"type":"time", ...}).
export interface TimeStrategy {
  days_of_week: number[]; // 1=Mon..=7=Sun
  time_start: number; // minutes 0..=1439
  time_end: number; // minutes 0..=1439
  model_id: string;
}
export interface StrategyKind extends TimeStrategy {
  type: "time";
}
export interface Strategy {
  id: string;
  priority: number; // 1..=10
  enabled: boolean;
  kind: StrategyKind;
}

// Mirrors src-tauri RouteEffective.
export interface RouteEffective {
  profile_id: string;
  effective_model_id: string;
  via_strategy_id?: string | null;
}
```

- [ ] **Step 2: Add command wrappers** in `src/lib/commands.ts` (near the profile block, ~line 39):

```ts
export const getRouteEffectiveModels = () => invoke<RouteEffective[]>("route_effective_models");
export const setProfileStrategiesEnabled = (profileId: string, enabled: boolean) =>
  invoke<void>("set_profile_strategies_enabled", { profileId, enabled });
```

Add `RouteEffective` to the `types` import at the top of the file.

- [ ] **Step 3: Add store state + actions** in `src/stores/config.ts`:

State (near line 16):
```ts
  const routeEffective = ref<RouteEffective[]>([]);
```

New action (near the profile actions, ~line 105):
```ts
  async function refreshEffective() {
    routeEffective.value = await api.getRouteEffectiveModels();
  }
  async function setStrategiesEnabled(profileId: string, enabled: boolean) {
    await api.setProfileStrategiesEnabled(profileId, enabled);
    profiles.value = await api.getProfiles();
    await refreshEffective();
  }
```

Re-fetch after profile mutations — in `loadAll` (after `routeOrder.value = ...`, ~line 41) add:
```ts
      await refreshEffective();
```
In `saveProfile` and `removeProfile`, after `profiles.value = await api.getProfiles();` add:
```ts
      await refreshEffective();
```

Add `routeEffective` and the two actions to the returned store surface (lines 166-196).

- [ ] **Step 4: Type-check + build**

Run: `npx vue-tsc --noEmit && npm run build`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/config.ts
git commit -m "feat(fe): strategy types + routeEffective store plumbing"
```

---

### Task 9: Profiles.vue — strategies editor (modal)

**Files:**
- Modify: `src/views/Profiles.vue`

**Interfaces:**
- Consumes: `Strategy`/`TimeStrategy` (Task 8), `config.saveProfile`.
- Produces: a profile modal that edits `strategies_enabled` + `strategies[]` (add/edit/delete each).

- [ ] **Step 1: Extend the form model.** Replace `FormState` + `blank()` + `openEdit` + the `save()` profile construction (lines 59-108):

```ts
interface StrategyForm {
  id: string;
  priority: number;
  enabled: boolean;
  days_of_week: number[];
  time_start: number; // minutes
  time_end: number;   // minutes
  model_id: string;
}
interface FormState {
  id: string;
  name: string;
  aliases: string[];
  backing_model_id: string;
  strategies_enabled: boolean;
  strategies: StrategyForm[];
}
const blank = (): FormState => ({
  id: "",
  name: "",
  aliases: [],
  backing_model_id: config.models[0]?.id ?? "",
  strategies_enabled: true,
  strategies: [],
});
function blankStrategy(): StrategyForm {
  return {
    id: genId("s", config.profiles.flatMap((p) => p.strategies ?? []).map((s) => s.id)),
    priority: 1, enabled: true, days_of_week: [1, 2, 3, 4, 5],
    time_start: 22 * 60, time_end: 8 * 60, model_id: config.models[0]?.id ?? "",
  };
}
function fromProfile(p: Profile): StrategyForm[] {
  return (p.strategies ?? []).map((s) => ({
    id: s.id, priority: s.priority, enabled: s.enabled,
    days_of_week: [...s.kind.days_of_week],
    time_start: s.kind.time_start, time_end: s.kind.time_end, model_id: s.kind.model_id,
  }));
}
```

Update `openEdit(p)` to set `form.strategies_enabled = p.strategies_enabled ?? true;` and `form.strategies = fromProfile(p);`.

Update `save()` to build strategies into the profile:
```ts
  const profile: Profile = {
    id: form.id.trim(),
    name: form.name.trim(),
    aliases: form.aliases,
    backing_model_id: form.backing_model_id,
    strategies_enabled: form.strategies_enabled,
    strategies: form.strategies.map((s) => ({
      id: s.id, priority: s.priority, enabled: s.enabled,
      kind: { type: "time", days_of_week: [...s.days_of_week], time_start: s.time_start, time_end: s.time_end, model_id: s.model_id },
    })),
  };
```

Add strategy-row handlers + NTimePicker minute↔timestamp helpers:
```ts
function addStrategy() { form.strategies.push(blankStrategy()); }
function removeStrategy(id: string) { form.strategies = form.strategies.filter((s) => s.id !== id); }
function toggleDay(s: StrategyForm, d: number) {
  s.days_of_week = s.days_of_week.includes(d)
    ? s.days_of_week.filter((x) => x !== d)
    : [...s.days_of_week, d].sort((a, b) => a - b);
}
function setWeekdays(s: StrategyForm, days: number[]) { s.days_of_week = [...days]; }
function isCrossNight(s: StrategyForm) { return s.time_start > s.time_end; }
// NTimePicker value is a ms timestamp; we store minutes-of-day.
function minToTs(min: number): number {
  const d = new Date(); d.setHours(Math.floor(min / 60), min % 60, 0, 0); return d.getTime();
}
function tsToMin(ts: number | null): number {
  if (ts == null) return 0; const d = new Date(ts); return d.getHours() * 60 + d.getMinutes();
}
```

- [ ] **Step 2: Add the strategies section to the modal template** — replace the `<NForm>` block (lines 171-178). Add the Naive UI components (`NSwitch, NTimePicker, NCheckbox, NTag, NDivider`) to the import list:

```vue
      <NForm label-placement="top">
        <NFormItem label="名称">
          <NInput v-model:value="form.name" placeholder="glm-5.2" />
        </NFormItem>
        <NFormItem label="兜底模型（默认/回退）">
          <NSelect v-model:value="form.backing_model_id" :options="modelOptions" :render-label="ellipsisLabel" placeholder="选择 Model" />
        </NFormItem>

        <NDivider style="margin: 8px 0">时段策略</NDivider>
        <NSpace align="center" justify="space-between">
          <NSwitch v-model:value="form.strategies_enabled">
            <template #checked>已启用</template>
            <template #unchecked>已停用</template>
          </NSwitch>
          <span class="muted">{{ form.strategies_enabled ? "按策略时段路由" : "已暂停策略，所有请求将直接走兜底模型" }}</span>
        </NSpace>

        <div v-for="s in form.strategies" :key="s.id" class="strategy-card">
          <NSpace align="center" justify="space-between">
            <NSpace align="center" :size="8">
              <NSwitch v-model:value="s.enabled" size="small" />
              <span class="muted">优先级</span>
              <NSelect v-model:value="s.priority" size="small" style="width: 70px"
                :options="Array.from({ length: 10 }, (_, i) => ({ label: String(i + 1), value: i + 1 }))" />
            </NSpace>
            <NButton size="tiny" type="error" ghost @click="removeStrategy(s.id)">删除</NButton>
          </NSpace>

          <div class="row">
            <span class="muted">重复</span>
            <NSpace :size="4" align="center">
              <NButton v-for="d in 7" :key="d" size="tiny"
                :type="s.days_of_week.includes(d) ? 'primary' : 'default'"
                @click="toggleDay(s, d)">{{ "一二三四五六日"[d - 1] }}</NButton>
              <NButton size="tiny" quaternary @click="setWeekdays(s, [1,2,3,4,5])">工作日</NButton>
              <NButton size="tiny" quaternary @click="setWeekdays(s, [1,2,3,4,5,6,7])">全选</NButton>
            </NSpace>
          </div>

          <div class="row">
            <span class="muted">时间</span>
            <NTimePicker :value="minToTs(s.time_start)" @update:value="(v: number | null) => (s.time_start = tsToMin(v))" format="HH:mm" size="small" style="width: 110px" />
            <span class="muted">到</span>
            <NTimePicker :value="minToTs(s.time_end)" @update:value="(v: number | null) => (s.time_end = tsToMin(v))" format="HH:mm" size="small" style="width: 110px" />
            <NTag v-if="isCrossNight(s)" type="warning" size="small" round>🌙 已跨夜 (于次日 {{ String(Math.floor(s.time_end / 60)).padStart(2, "0") }}:{{ String(s.time_end % 60).padStart(2, "0") }} 结束)</NTag>
          </div>

          <div class="row">
            <span class="muted">调用模型</span>
            <NSelect v-model:value="s.model_id" :options="modelOptions" :render-label="ellipsisLabel" size="small" placeholder="选择 Model" />
          </div>
        </div>

        <span v-if="!form.strategies.length" class="muted" style="display:block; margin-top:8px">暂无策略</span>
        <NButton style="margin-top: 8px" block dashed @click="addStrategy">+ 添加策略</NButton>
      </NForm>
```

Also widen the modal `style="max-width: 460px"` → `560px` (line 169) and add CSS:
```css
.strategy-card { border: 1px solid var(--sl-border); border-radius: 8px; padding: 10px; margin-top: 8px; display: flex; flex-direction: column; gap: 8px; }
.strategy-card .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
```

- [ ] **Step 3: Type-check + build**

Run: `npx vue-tsc --noEmit && npm run build`
Expected: PASS.

- [ ] **Step 4: Manual check** — `npm run tauri dev`; open 路由 tab, edit a route, add a strategy (pick 工作日, 22:00→08:00 → 🌙 appears), save, reopen → values persist; toggle a day off; delete a strategy.

- [ ] **Step 5: Commit**

```bash
git add src/views/Profiles.vue
git commit -m "feat(fe): profile modal strategies editor (day toggles + cross-midnight)"
```

---

### Task 10: Profiles.vue — list card (effective model + master switch)

**Files:**
- Modify: `src/views/Profiles.vue`

**Interfaces:**
- Consumes: `config.routeEffective`, `config.setStrategiesEnabled`.
- Produces: the route list card shows 兜底→, ⏰N条, a master NSwitch, and 当前→ effective model.

- [ ] **Step 1: Add helpers + master-switch handler** in the `<script setup>` (after `backingName`):

```ts
function effectiveOf(p: Profile) {
  return config.routeEffective.find((e) => e.profile_id === p.id);
}
function effectiveLabel(p: Profile): string {
  const eff = effectiveOf(p);
  const id = eff?.effective_model_id ?? p.backing_model_id;
  return backingName(id);
}
async function toggleMaster(p: Profile, enabled: boolean) {
  try { await config.setStrategiesEnabled(p.id, enabled); }
  catch (e) { msg.error(`切换失败：${String(e)}`); }
}
```

Add a `usePolling` import + a 60s refresh so 当前→ stays fresh while viewing the list:
```ts
import { usePolling } from "../lib/usePolling";
usePolling(() => config.refreshEffective(), () => 60_000);
```

- [ ] **Step 2: Update the list-card template** — replace the inner `<NSpace align="center" justify="space-between">` of the draggable item (lines 147-158):

```vue
        <NCard size="small">
          <NSpace align="center" justify="space-between" wrap>
            <NSpace align="center" :size="10" wrap>
              <span class="drag-handle" title="拖动排序">⠿</span>
              <span class="name">{{ p.name }}</span>
              <span class="arrow">兜底</span>
              <span class="backing">{{ backingName(p.backing_model_id) }}</span>
              <NTag v-if="(p.strategies?.length ?? 0) > 0" size="small" round :bordered="false"
                :type="p.strategies_enabled ? 'info' : 'default'">⏰ 策略 {{ p.strategies!.length }} 条</NTag>
              <NSwitch :value="p.strategies_enabled" size="small" @update:value="(v: boolean) => toggleMaster(p, v)" />
              <span class="arrow">当前</span>
              <span class="backing">{{ effectiveLabel(p) }}<span v-if="effectiveOf(p)?.via_strategy_id">⏰</span><span v-if="!p.strategies_enabled" class="muted"> (策略已停用)</span></span>
            </NSpace>
            <NSpace>
              <NButton size="small" @click="openEdit(p)">编辑</NButton>
              <NButton size="small" type="error" ghost @click="remove(p)">删除</NButton>
            </NSpace>
          </NSpace>
        </NCard>
```

Add `NTag` to the Naive UI imports.

- [ ] **Step 3: Type-check + build**

Run: `npx vue-tsc --noEmit && npm run build`
Expected: PASS.

- [ ] **Step 4: Manual check** — with a strategy active, 当前→ shows the strategy model + ⏰; flip the master switch → 当前→ shows 兜底 + (策略已停用); the ⏰策略 tag tint changes.

- [ ] **Step 5: Commit**

```bash
git add src/views/Profiles.vue
git commit -m "feat(fe): route list card shows effective model + master switch"
```

---

### Task 11: Dashboard.vue + RouteLine.vue — read-only effective chain

**Files:**
- Modify: `src/views/Dashboard.vue`
- Modify: `src/components/RouteLine.vue`

**Interfaces:** None new — removes the quick-edit affordance; the chain head becomes the strategy-aware effective model.

- [ ] **Step 1: Make RouteLine's primary node read-only.** In `RouteLine.vue`:
  - Replace the props block (lines 16-40): remove `modelOptions`, `switchModelId`, `switching`; add `primaryModelLabel?: string`. Keep `profileName`, `quota`, `cooling`, `fallbackChain`.
  - Remove the `emit` (line 42) and `onPickModel` (lines 44-46).
  - Replace the `<NSelect>` (lines 61-72) with a static span:
    ```vue
          <span class="val val-model">{{ primaryModelLabel || "—" }}</span>
    ```
    (the `.val`/`.val-model` class is already used by 降级 nodes per the existing template).

- [ ] **Step 2: Update Dashboard to use the effective model + drop quick-edit.** In `Dashboard.vue`:
  - Delete `modelOptions` (lines 89-95), `switching` (line 97), and `switchModel` (lines 100-111).
  - Add `effectiveModelFor` + a label helper (near `backingModelFor`):
    ```ts
    function effectiveModelFor(p: Profile | undefined) {
      if (!p) return undefined;
      const eff = config.routeEffective.find((e) => e.profile_id === p.id);
      return config.models.find((m) => m.id === (eff?.effective_model_id ?? p.backing_model_id));
    }
    function modelLabelOf(m: Model | undefined): string {
      if (!m) return "—";
      const provider = config.providers.find((p) => p.id === m.provider_id);
      return provider ? `${m.upstream_model_id}（${providerLabel(provider)}）` : m.upstream_model_id;
    }
    ```
  - Change `fallbackChainFor` (line 72) and `nodeQuotaForProfile` (line 42) and any `coolingFor` to start from `effectiveModelFor(p)` instead of `backingModelFor(p)`.
  - Update the `<RouteLine>` usage (lines 257-268): remove `:model-options`, `:switch-model-id`, `:switching`, `@switch-model`; add `:primary-model-label="modelLabelOf(effectiveModelFor(p))"`.
  - Extend the existing `usePolling` (line 181) to also refresh effective models:
    ```ts
    usePolling(
      async () => { await runtime.refresh(); await config.refreshEffective(); },
      () => Math.max(MIN_USAGE_REFRESH_SECS, system.settings?.usage_refresh_interval_secs ?? DEFAULT_USAGE_REFRESH_SECS) * 1000,
    );
    ```

- [ ] **Step 3: Type-check + build**

Run: `npx vue-tsc --noEmit && npm run build`
Expected: PASS. (If `backingModelFor` becomes unused, delete it.)

- [ ] **Step 4: Manual check** — 概览 route card: head node shows the effective model (changes with time/strategy); no dropdown; cooling ❄ + quota badges still render; chain walks the fallback tail.

- [ ] **Step 5: Commit**

```bash
git add src/views/Dashboard.vue src/components/RouteLine.vue
git commit -m "refactor(fe): 概览 route chain read-only, head = effective model"
```

---

### Task 12: Cleanup — remove dead set_profile_backing* (backend + frontend)

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs:159`
- Modify: `src/lib/commands.ts`
- Modify: `src/stores/config.ts`

**Context:** After Task 7 (tray gone) and Task 11 (Dashboard quick-edit gone), `set_profile_backing`/`set_profile_backing_core` and the frontend `setProfileBacking`/`setBacking` have **no remaining callers**. Confirm before deleting.

- [ ] **Step 1: Confirm zero callers**

Run: `git grep -n "set_profile_backing\|setProfileBacking\|setBacking"` — expect matches ONLY in their definitions (commands.rs, lib.rs registration, commands.ts, stores/config.ts), nowhere else.

- [ ] **Step 2: Remove backend.** In `commands.rs` delete `set_profile_backing` + `set_profile_backing_core` (lines 105-135). In `lib.rs` delete the `commands::set_profile_backing,` line (~159).

- [ ] **Step 3: Remove frontend.** In `src/lib/commands.ts` delete `setProfileBacking`. In `src/stores/config.ts` delete the `setBacking` action and remove it from the returned surface.

- [ ] **Step 4: Build everything**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run && npx vue-tsc --noEmit && npm run build`
Expected: PASS, no `unused` warnings referencing the removed items.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs src/lib/commands.ts src/stores/config.ts
git commit -m "chore: remove dead set_profile_backing (no callers after tray/概览 removal)"
```

---

### Task 13: Final verification

- [ ] **Step 1: Full backend test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 2: Frontend type-check + build**

Run: `npx vue-tsc --noEmit && npm run build`
Expected: PASS.

- [ ] **Step 3: End-to-end manual smoke** — `npm run tauri dev`:
  - Add a route with a strategy covering "now" (e.g. today's weekday, current minute inside the window) → send a request to `http://localhost:6950/v1/...` with that route name → logs show `time-strategy matched` + the strategy model's vendor/upstream in `forward request`; 概览 head node = strategy model.
  - Flip the master switch off → next request logs only `forward request` with the 兜底 model; 概览 head = 兜底.
  - Set the strategy model to a rate-limiting upstream → request falls through to the strategy model's `fallback_target_model_id`; logs show the hop; breaker trips the strategy entry.
  - Tray: no 路由 section; 套餐用量 still present.

- [ ] **Step 4: Commit any final touch-ups** (if the smoke test surfaced fixes) with explicit paths.

---

## Self-Review (completed)

**Spec coverage:** §3 data model → T1; §4.1 Clock seam → T2; §4.2 matchers → T3/T4; §4.3 resolve+dispatch → T5; §4.4 logging → T5; §4.5 route_effective_models → T6; §5 validation + master-switch command → T6; §5 cleanup → T12; §6.2 modal+card → T9/T10; §6.3 概览 read-only → T11; §6.4 tray removal → T7; §7 tests → embedded in T1-T7; §8 out-of-scope respected (no app TZ, no cross-week). **Half-open boundary points** (spec review point 1) → T3 tests. **days_of_week dedup** (point 2) → T6 validation + test. **🌙 strictly `start > end`** (point 3) → T9 `isCrossNight`.

**Type consistency:** `profile_start_model` signature is identical in T4 (definition), T5 (resolve), T6 (route_effective_models). `LocalNow { weekday, minute }` consistent across T2/T3/T4/T5. `StrategyKind` internally-tagged shape consistent between Rust (T1 `#[serde(tag="type")]`) and TS (T8 `extends TimeStrategy { type: "time" }`). `RouteEffective` field names match Rust ↔ TS (T6/T8).

No placeholders remain.
