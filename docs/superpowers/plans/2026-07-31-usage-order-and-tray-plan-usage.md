# Unified Usage Order & Tray Plan-Usage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the 概览「可用额度」chips, the 套餐用量 tab, and the tray「账户用量」submenu one shared, persisted account order; show three-tier plan usage in tray rows; and show a 套餐用量 summary on the tray-icon hover tooltip.

**Architecture:** Persist the user's drag-order as `AppConfig.usage_order` (backend, single source of truth), migrated once from the legacy `switchlm:order:usage` localStorage key. All three surfaces read it. Tray rows switch from a single primary % to the 5h/周/月 tier breakdown via a new pure helper (the shared `usage_display` stays single-% for the 路由 submenu). A pure `usage_tooltip` builds a length-capped summary set on every tray refresh. Frontend drag commits are debounced so rapid reorders coalesce into one native menu rebuild.

**Tech Stack:** Tauri v2 (Rust backend, `axum`), Vue 3 `<script setup>` + Pinia, `vuedraggable`, Naive UI. Tests co-located as `#[cfg(test)] mod tests`; HTTP-less pure logic tested directly.

## Global Constraints

- Tier window literals are exactly `"five_hour"`, `"weekly_limit"`, `"monthly"`. Rust code uses the re-exported constants `crate::usage::{TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY}`; TS code uses the string literals.
- `billing_model` (`"plan"` | `"consumption"`) is the discriminator for every display branch — never branch on the vendor string.
- Tray tooltip is hard-capped at **120 chars**; whole-account units only, `…(+N)` suffix when accounts are dropped.
- Tooltip join format is **single-line (` · `)** — the Windows-safe default (classic `szTip` doesn't render newlines and the 128-char buffer binds either way). The spec's multiline preference is obtainable later by switching the separator to `\n` if Tauri is verified to render it on the target OS; the 120-char cap applies to both.
- Drag-reorder persistence is **debounced ~400 ms trailing**, in `Usage.vue`'s commit handler only (the store action stays direct so the one-time migration is never skipped).
- Overview「可用额度」chips keep the **remaining %** metric and urgency **color**; only their **order** changes.
- `LOW_BALANCE_THRESHOLD` (10.0) and all chip/node color logic are untouched.
- Ordering rule everywhere: rank by `usage_order` index; ids **not** in the list sink to the end, preserving their `providers` config order (stable sort).
- Backend setters use the existing shape: `state.config.write().await` → mutate → `persist(&app, &config)?`, taking `app: tauri::AppHandle`.
- Run backend tests with `cargo test --manifest-path src-tauri/Cargo.toml`; frontend type-check with `npm run build` (or `npx vue-tsc --noEmit`).
- Reference spec: `docs/superpowers/specs/2026-07-31-usage-order-and-tray-plan-usage-design.md`.

---

### Task 1: Add `usage_order` to `AppConfig`

**Files:**
- Modify: `src-tauri/src/config/types.rs` (`AppConfig` struct + `Default` impl)
- Test: `src-tauri/src/config/types.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `AppConfig.usage_order: Vec<String>` (`#[serde(default)]`, defaults to `[]`). Later tasks read `cfg.usage_order`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/config/types.rs`, add (to the existing `#[cfg(test)] mod tests`, creating one at the file's end if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_order_defaults_empty_when_absent() {
        // An old config JSON that never had a usage_order field must deserialize to [].
        let json = r#"{"providers":[],"models":[],"profiles":[],"settings":{}}"#;
        let cfg: AppConfig = serde_json::from_str(json).expect("deserialize");
        assert!(cfg.usage_order.is_empty());
    }

    #[test]
    fn usage_order_round_trips() {
        let mut cfg = AppConfig::default();
        cfg.usage_order = vec!["p_b".into(), "p_a".into()];
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.usage_order, vec!["p_b".to_string(), "p_a".to_string()]);
    }
}
```

If `serde_json` is not already a dev-dependency in the crate, it is available (the crate uses serde throughout); if the test fails to compile for a missing dev-dep, add `serde_json = { workspace = true }` (or the version already used elsewhere in `src-tauri/Cargo.toml`) to `[dev-dependencies]`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib config::types::tests`
Expected: COMPILE ERROR — `no field` on `usage_order` (struct doesn't have it yet).

- [ ] **Step 3: Write minimal implementation**

In `src-tauri/src/config/types.rs`, add the field to `AppConfig` (currently `providers` / `models` / `profiles` / `settings`, each `#[serde(default)]`):

```rust
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// User's drag-order for usage-display surfaces (provider ids, most-significant first).
    /// Empty = fall back to `providers` config order. Shared by 套餐用量 tab, 概览 chips, tray.
    #[serde(default)]
    pub usage_order: Vec<String>,
    #[serde(default)]
    pub settings: Settings,
}
```

Keep whatever `derive`s and `serde` attributes the struct already has — only **add** the `usage_order` field with `#[serde(default)]`. The existing `Default` impl (if it is hand-written rather than derived) must also set `usage_order: Vec::new()`; if `Default` is derived, the field's own `Vec::new()` default applies automatically.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib config::types::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs
git commit -m "feat(config): add usage_order field to AppConfig"
```

---

### Task 2: `get_usage_order` / `set_usage_order` commands

**Files:**
- Modify: `src-tauri/src/commands.rs` (add pure helper + two commands)
- Modify: `src-tauri/src/lib.rs` (register commands in `generate_handler!` at line 126)
- Test: `src-tauri/src/commands.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `AppConfig.usage_order` (Task 1); `persist(&app, &config)`; `crate::tray::refresh_tray_menu`.
- Produces: Tauri commands `get_usage_order(state) -> Vec<String>` and `set_usage_order(state, ids, app) -> ()`; pure `fn normalize_usage_order(ids: Vec<String>, known_ids: &[String]) -> Vec<String>`.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/commands.rs` (create `#[cfg(test)] mod tests { use super::*; ... }` at the file end if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_usage_order_filters_unknown_and_dedupes() {
        let known = ["a".to_string(), "b".to_string(), "c".to_string()];
        // "x" is unknown -> dropped; "b" duplicated -> kept once, first position wins.
        let ids = vec!["b".into(), "x".into(), "c".into(), "a".into(), "b".into()];
        assert_eq!(normalize_usage_order(ids, &known), vec!["b".to_string(), "c", "a"]);
    }

    #[test]
    fn normalize_usage_order_empty_for_all_unknown() {
        let known = ["a".to_string()];
        assert!(normalize_usage_order(vec!["z".into()], &known).is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests`
Expected: COMPILE ERROR — `cannot find function normalize_usage_order`.

- [ ] **Step 3: Write minimal implementation**

In `src-tauri/src/commands.rs`, add the helper and two commands. Place them immediately after `get_profiles` (currently around line 29). `persist` (line 872) and the `State`/`AppHandle` types are already in scope.

```rust
/// Provider-id ordering for usage-display surfaces: drop ids that are no longer known
/// providers and collapse duplicates (first occurrence wins). Pure — unit-tested directly.
fn normalize_usage_order(ids: Vec<String>, known_ids: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let known: HashSet<&str> = known_ids.iter().map(|s| s.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    ids.into_iter()
        .filter(|id| known.contains(id.as_str()) && seen.insert(id.clone()))
        .collect()
}

/// The user's drag-order for usage surfaces (套餐用量 tab / 概览 chips / tray). Empty = config order.
#[tauri::command]
pub async fn get_usage_order(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.config.read().await.usage_order.clone())
}

/// Persist a new usage display order (provider ids). Unknown/duplicate ids are dropped.
/// Persists, then refreshes the tray so its reordered 账户用量 rows + tooltip apply immediately.
#[tauri::command]
pub async fn set_usage_order(
    state: State<'_, AppState>,
    ids: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        let known: Vec<String> = config.providers.iter().map(|p| p.id.clone()).collect();
        config.usage_order = normalize_usage_order(ids, &known);
        persist(&app, &config)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}
```

- [ ] **Step 4: Register the commands**

In `src-tauri/src/lib.rs`, add the two commands to the `generate_handler!` list (starts at line 126). Insert them right after `commands::get_profiles,`:

```rust
            commands::get_profiles,
            commands::get_usage_order,
            commands::set_usage_order,
            commands::set_profile_backing,
```

- [ ] **Step 5: Run tests to verify they pass + build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests`
Expected: PASS.
Then confirm the crate still compiles (registers resolve):
Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds cleanly.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(cmd): get/set_usage_order commands + tray refresh"
```

---

### Task 3: Pure tray helpers — `tier_summary`, `tooltip_value`, `usage_tooltip`

These are new pure functions. They do **not** touch the shared `usage_display` (which `model_label` still uses for single-% 路由 labels).

**Files:**
- Modify: `src-tauri/src/tray.rs` (add helpers + test fixture `snap_tiers`; extend the `usage` import)
- Test: `src-tauri/src/tray.rs` (`mod tests`)

**Interfaces:**
- Consumes: `UsageSnapshot`, `UsageTier`, `crate::usage::{TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY}`.
- Produces:
  - `fn tier_summary(u: &UsageSnapshot) -> Option<String>` — plan only, `"5h:80% 周:40% 月:10%"`; `None` when no tier has a `used_pct`.
  - `fn tooltip_value(u: &UsageSnapshot) -> Option<String>` — plan → `"80%"` (5h `used_pct`, falling back to top-level `used`); consumption → `"CNY 48.77"`; `None` when no value.
  - `fn usage_tooltip(lines: &[String]) -> String` — joins pre-formatted `"name value"` lines with `" · "`, caps at 120 chars, appends `" …(+N)"` when some are dropped; `[]` → `"SwitchLM"`.

- [ ] **Step 1: Extend the `usage` import**

In `src-tauri/src/tray.rs`, change:

```rust
use crate::usage::UsageSnapshot;
```

to:

```rust
use crate::usage::{UsageSnapshot, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY};
```

- [ ] **Step 2: Write the failing tests**

In the existing `#[cfg(test)] mod tests` block in `tray.rs`, add a tiered-snapshot fixture and the tests:

```rust
    fn snap_tiers(five_hour: Option<f64>, weekly: Option<f64>, monthly: Option<f64>) -> UsageSnapshot {
        let mut tiers = Vec::new();
        for (w, p) in [
            (TIER_FIVE_HOUR, five_hour),
            (TIER_WEEKLY_LIMIT, weekly),
            (TIER_MONTHLY, monthly),
        ] {
            if let Some(pct) = p {
                tiers.push(UsageTier { window: (*w).into(), used_pct: Some(pct), reset_at: None });
            }
        }
        let any = !tiers.is_empty();
        UsageSnapshot {
            used: five_hour,
            total: if any { Some(100.0) } else { None },
            remaining: five_hour.map(|p| 100.0 - p),
            reset_at: None,
            unit: "%".into(),
            raw_summary: None,
            plan: None,
            tiers,
            billing_model: "plan".into(),
            plan_info: None,
        }
    }

    #[test]
    fn tier_summary_three_windows() {
        let s = tier_summary(&snap_tiers(Some(79.6), Some(40.0), Some(10.0)));
        assert_eq!(s.as_deref(), Some("5h:80% 周:40% 月:10%")); // 79.6 rounds to 80
    }

    #[test]
    fn tier_summary_marks_missing_window() {
        let s = tier_summary(&snap_tiers(Some(80.0), None, None));
        assert_eq!(s.as_deref(), Some("5h:80% 周:– 月:–"));
    }

    #[test]
    fn tier_summary_none_when_no_tier_data() {
        assert!(tier_summary(&snap_tiers(None, None, None)).is_none());
    }

    #[test]
    fn tooltip_value_plan_uses_five_hour_then_used() {
        assert_eq!(tooltip_value(&snap_tiers(Some(80.0), None, None)).as_deref(), Some("80%"));
        // No five_hour tier but top-level used present -> fall back.
        let mut u = snap_tiers(None, Some(40.0), None);
        u.used = Some(55.0);
        assert_eq!(tooltip_value(&u).as_deref(), Some("55%"));
    }

    #[test]
    fn tooltip_value_consumption_shows_balance() {
        let u = UsageSnapshot {
            used: Some(51.23), total: Some(100.0), remaining: Some(48.77),
            reset_at: None, unit: "CNY".into(), raw_summary: None, plan: None,
            tiers: vec![], billing_model: "consumption".into(), plan_info: None,
        };
        assert_eq!(tooltip_value(&u).as_deref(), Some("CNY 48.77"));
    }

    #[test]
    fn usage_tooltip_empty_is_app_name() {
        assert_eq!(usage_tooltip(&[]), "SwitchLM");
    }

    #[test]
    fn usage_tooltip_joins_few_accounts() {
        let lines = vec!["智谱 80%".to_string(), "DeepSeek CNY 48.77".to_string()];
        assert_eq!(usage_tooltip(&lines), "智谱 80% · DeepSeek CNY 48.77");
    }

    #[test]
    fn usage_tooltip_truncates_many_accounts_with_ellipsis() {
        // 10 accounts -> must cap at <= 120 chars, drop some with " …(+N)", and every
        // included unit must be whole (no line cut mid-name).
        let lines: Vec<String> = (0..10).map(|i| format!("账号编号{:02} 80%", i)).collect();
        let out = usage_tooltip(&lines);
        assert!(out.chars().count() <= 120, "len={} out={out}", out.chars().count());
        assert!(out.contains("…(+"), "missing ellipsis: {out}");
        // The head (before the ellipsis) is " · "-joined whole input lines.
        let head = out.split(" …(+").next().unwrap();
        for unit in head.split(" · ") {
            assert!(lines.contains(&unit.to_string()), "partial/unknown unit in tooltip: {unit:?}");
        }
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::tests::tier_summary`
Expected: COMPILE ERROR — `cannot find function tier_summary` (and the others).

- [ ] **Step 4: Write minimal implementation**

In `src-tauri/src/tray.rs`, add these three pure functions (e.g. just below the existing `quota_pct`):

```rust
/// Short tray label for one tier window id.
fn tier_window_label(window: &str) -> &str {
    match window {
        TIER_FIVE_HOUR => "5h",
        TIER_WEEKLY_LIMIT => "周",
        TIER_MONTHLY => "月",
        _ => "？",
    }
}

/// Three-window plan-usage summary for an account row, e.g. "5h:80% 周:40% 月:10%".
/// A window without data renders "–". `None` when no window has a concrete value (row hidden).
/// Plan only — consumption is handled separately in `account_label`.
fn tier_summary(u: &UsageSnapshot) -> Option<String> {
    let any = u.tiers.iter().any(|t| t.used_pct.is_some());
    if !any {
        return None;
    }
    let parts: Vec<String> = [TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY]
        .iter()
        .map(|&w| {
            let pct = u.tiers.iter().find(|t| t.window == w).and_then(|t| t.used_pct);
            match pct {
                Some(p) => format!("{}:{}%", tier_window_label(w), p.round() as u32),
                None => format!("{}:–", tier_window_label(w)),
            }
        })
        .collect();
    Some(parts.join(" "))
}

/// Primary value for the tray-icon tooltip: plan → "80%" (5h used, falling back to top-level used);
/// consumption → "CNY 48.77". `None` when no value can be derived.
fn tooltip_value(u: &UsageSnapshot) -> Option<String> {
    if u.billing_model == "consumption" {
        u.remaining.map(|r| format!("{} {:.2}", u.unit, r))
    } else {
        let pct = u
            .tiers
            .iter()
            .find(|t| t.window == TIER_FIVE_HOUR)
            .and_then(|t| t.used_pct)
            .or(u.used)?;
        Some(format!("{}%", pct.round() as u32))
    }
}

/// Compact 套餐用量 summary for the tray icon tooltip. `lines` are pre-formatted "name value"
/// strings, already ordered & filtered by the caller. Joined with " · ", hard-capped at 120 chars;
/// whole lines only, with " …(+N)" when lines are dropped. Empty -> app name.
fn usage_tooltip(lines: &[String]) -> String {
    if lines.is_empty() {
        return "SwitchLM".to_string();
    }
    const CAP: usize = 120;
    const SEP: &str = " · ";
    let total = lines.len();
    // Largest k whose joined head fits (with a " …(+N)" tail when k < total).
    for k in (1..=total).rev() {
        let head: String = lines[..k].join(SEP);
        let head_len = head.chars().count();
        if k == total {
            if head_len <= CAP {
                return head;
            }
        } else {
            let tail = format!(" …(+{})", total - k);
            if head_len + tail.chars().count() <= CAP {
                return format!("{head}{tail}");
            }
        }
    }
    // Even one line is too long: hard-truncate the first line.
    let mut s: String = lines[0].chars().take(CAP).collect();
    if total > 1 {
        s.push_str(" …");
    }
    if s.chars().count() > CAP {
        s = s.chars().take(CAP).collect();
    }
    s
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::tests`
Expected: PASS (all new tests, existing tests unaffected).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): pure tier_summary/tooltip_value/usage_tooltip helpers"
```

---

### Task 4: Account rows show three-tier plan usage

Rewire `account_label` to the new 3-tier format. The shared `usage_display` and `model_label` (路由 submenu) are **untouched**. Update the existing plan-account tests that asserted the old single-% format.

**Files:**
- Modify: `src-tauri/src/tray.rs` (`account_label` body + update 4 existing tests)
- Test: `src-tauri/src/tray.rs` (`mod tests`)

**Interfaces:**
- Consumes: `tier_summary` (Task 3).
- Produces: `account_label` now emits 3-tier plan rows (consumption unchanged).

- [ ] **Step 1: Update the affected existing tests first (they encode the new behavior)**

In `src-tauri/src/tray.rs` `mod tests`:

Replace `account_label_plan_shows_pct`:
```rust
    #[test]
    fn account_label_plan_shows_tiers() {
        let u = snap_tiers(Some(80.0), Some(40.0), Some(10.0));
        assert_eq!(account_label("智谱", None, &u).unwrap(), "智谱 5h:80% 周:40% 月:10%");
    }
```

Replace `account_label_appends_vendor_tag`:
```rust
    #[test]
    fn account_label_appends_vendor_tag() {
        let u = snap_tiers(Some(45.0), Some(30.0), Some(10.0));
        assert_eq!(
            account_label("火山号A", Some("volcengine"), &u).unwrap(),
            "火山号A [volcengine] 5h:45% 周:30% 月:10%"
        );
    }
```

In `spec_accounts_lists_usage_per_provider_in_order`, change the 智谱 snapshot from `snap(80.0)` to `snap_tiers(Some(80.0), Some(40.0), Some(10.0))` and its assertion to:
```rust
        assert_eq!(spec.accounts[0].label, "智谱 5h:80% 周:40% 月:10%");
```
(The DeepSeek consumption entry and its assertion stay unchanged.)

In `spec_accounts_vendor_tag_only_for_multi_account_vendor`, replace the three `snap(...)` inserts with tiered snapshots and the three assertions:
```rust
        usage.insert("volc1".into(), snap_tiers(Some(40.0), Some(20.0), Some(5.0)));
        usage.insert("volc2".into(), snap_tiers(Some(70.0), Some(50.0), Some(15.0)));
        usage.insert("zhipu".into(), snap_tiers(Some(80.0), Some(40.0), Some(10.0)));
```
```rust
        assert_eq!(by_id["volc1"].label, "火山号A [volcengine] 5h:40% 周:20% 月:5%");
        assert_eq!(by_id["volc2"].label, "火山号B [volcengine] 5h:70% 周:50% 月:15%");
        assert_eq!(by_id["zhipu"].label, "智谱 5h:80% 周:40% 月:10%");
```

Leave `account_label_consumption_shows_balance` and `account_label_none_when_no_usage_value` unchanged (consumption path and empty path still hold).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::tests::account_label`
Expected: FAIL — current `account_label` still produces single `%` (e.g. `"智谱 80%"`), and tiered snapshots' `used`/`total` drive `quota_pct` but the test now expects the 3-tier string.

- [ ] **Step 3: Rewrite `account_label` to use the tier summary**

In `src-tauri/src/tray.rs`, replace the body of `account_label`:

```rust
/// Tray label for one account's usage: display name + (multi-account) vendor tag + usage display.
/// Plan → three-window tier summary; consumption → balance. `None` when there is no concrete
/// value — the caller hides that account.
fn account_label(display_name: &str, vendor_tag: Option<&str>, u: &UsageSnapshot) -> Option<String> {
    let display = account_display(u)?;
    let mut s = display_name.to_string();
    if let Some(v) = vendor_tag {
        s.push_str(&format!(" [{v}]"));
    }
    s.push(' ');
    s.push_str(&display);
    Some(s)
}

/// Account-row usage text (no leading name): plan → tier summary; consumption → "CNY 48.77".
/// `None` hides the row. (Distinct from `usage_display`, which stays single-% for 路由 model labels.)
fn account_display(u: &UsageSnapshot) -> Option<String> {
    if u.billing_model == "consumption" {
        u.remaining.map(|r| format!("{} {:.2}", u.unit, r))
    } else {
        tier_summary(u)
    }
}
```

- [ ] **Step 4: Run the full tray test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray`
Expected: PASS — all account tests reflect 3-tier plan rows; 路由 `model_label` tests unchanged.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): account rows show 5h/周/月 tier usage"
```

---

### Task 5: Order account rows by `usage_order` + tray-icon tooltip

Add the shared ordering, produce the tooltip in the spec, set it on refresh, and rename the submenu to 「套餐用量」.

**Files:**
- Modify: `src-tauri/src/tray.rs` (`TrayMenuSpec.tooltip`, `tray_menu_spec`, `build_menu` submenu title, `refresh_tray_menu` set_tooltip)
- Test: `src-tauri/src/tray.rs` (`mod tests`)

**Interfaces:**
- Consumes: `cfg.usage_order` (Task 1); `usage_tooltip`, `tooltip_value` (Task 3).
- Produces: `TrayMenuSpec { profiles, accounts, tooltip }`; the tray icon tooltip is updated on every refresh.

- [ ] **Step 1: Write the failing tests**

Add to `tray.rs` `mod tests`:

```rust
    #[test]
    fn spec_accounts_ordered_by_usage_order() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("a", "A"));
        cfg.providers.push(mk_provider("b", "B"));
        cfg.providers.push(mk_provider("c", "C"));
        cfg.usage_order = vec!["c".into(), "a".into()]; // "b" unlisted -> sinks to end
        let mut usage = HashMap::new();
        usage.insert("a".into(), snap_tiers(Some(10.0), None, None));
        usage.insert("b".into(), snap_tiers(Some(20.0), None, None));
        usage.insert("c".into(), snap_tiers(Some(30.0), None, None));

        let spec = tray_menu_spec(&cfg, &usage, &HashMap::new());
        let ids: Vec<&str> = spec.accounts.iter().map(|a| a.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]); // ordered, then unlisted in config order
    }

    #[test]
    fn spec_accounts_keep_config_order_when_usage_order_empty() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("a", "A"));
        cfg.providers.push(mk_provider("b", "B"));
        let mut usage = HashMap::new();
        usage.insert("a".into(), snap_tiers(Some(10.0), None, None));
        usage.insert("b".into(), snap_tiers(Some(20.0), None, None));

        let spec = tray_menu_spec(&cfg, &usage, &HashMap::new());
        let ids: Vec<&str> = spec.accounts.iter().map(|a| a.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn spec_tooltip_lists_primary_values_in_order() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        cfg.providers.push(mk_provider("deepseek", "DS"));
        cfg.usage_order = vec!["deepseek".into(), "zhipu".into()];
        let mut usage = HashMap::new();
        usage.insert("zhipu".into(), snap_tiers(Some(80.0), None, None));
        usage.insert(
            "deepseek".into(),
            UsageSnapshot {
                used: Some(51.23), total: Some(100.0), remaining: Some(48.77),
                reset_at: None, unit: "CNY".into(), raw_summary: None, plan: None,
                tiers: vec![], billing_model: "consumption".into(), plan_info: None,
            },
        );
        let spec = tray_menu_spec(&cfg, &usage, &HashMap::new());
        assert_eq!(spec.tooltip, "DS CNY 48.77 · 智谱 80%");
    }

    #[test]
    fn spec_tooltip_is_switchlm_when_no_usage() {
        let cfg = AppConfig::default();
        let spec = tray_menu_spec(&cfg, &HashMap::new(), &HashMap::new());
        assert_eq!(spec.tooltip, "SwitchLM");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::tests::spec_accounts_ordered_by_usage_order`
Expected: COMPILE ERROR — `TrayMenuSpec` has no field `tooltip`; accounts are not ordered.

- [ ] **Step 3: Add `tooltip` to the spec struct**

In `src-tauri/src/tray.rs`:

```rust
#[derive(Debug, PartialEq)]
pub struct TrayMenuSpec {
    pub profiles: Vec<ProfileMenuSpec>,
    pub accounts: Vec<AccountUsageSpec>,
    /// Native tray-icon tooltip text (套餐用量 summary). "SwitchLM" when no usage.
    pub tooltip: String,
}
```

- [ ] **Step 4: Rebuild `tray_menu_spec` — order accounts + build tooltip**

Replace the `accounts` block (currently a `filter_map` over `cfg.providers`) and the final `TrayMenuSpec { ... }` return with an order-aware version. The new `tray_menu_spec` tail (replacing from `let accounts = cfg.providers.iter().filter_map(...)` through the end of the fn):

```rust
    // Visible accounts (concrete usage value), in config order.
    let mut visible: Vec<(&crate::config::Provider, Option<&str>)> = cfg
        .providers
        .iter()
        .filter_map(|p| {
            let tag = if multi.contains(p.vendor.as_str()) { Some(p.vendor.as_str()) } else { None };
            let has_value = usage
                .get(&p.id)
                .map(|u| account_display(u).is_some())
                .unwrap_or(false);
            if has_value { Some((p, tag)) } else { None }
        })
        .collect();
    // Order by usage_order rank; unlisted sink to the end, preserving config order (stable).
    visible.sort_by_key(|(p, _)| cfg.usage_order.iter().position(|id| id == &p.id).unwrap_or(usize::MAX));

    let accounts = visible
        .iter()
        .map(|(p, tag)| AccountUsageSpec {
            provider_id: p.id.clone(),
            label: account_label(p.display_name.as_str(), *tag, usage.get(&p.id).unwrap()).unwrap(),
        })
        .collect();
    let tooltip_lines: Vec<String> = visible
        .iter()
        .filter_map(|(p, _)| {
            tooltip_value(usage.get(&p.id).unwrap()).map(|v| format!("{} {v}", p.display_name))
        })
        .collect();
    let tooltip = usage_tooltip(&tooltip_lines);
    TrayMenuSpec { profiles, accounts, tooltip }
```

Note: `account_label` returns `Option<String>` but every entry in `visible` already has a concrete value (guaranteed by the `has_value` filter), so `.unwrap()` on its result is safe; likewise `usage.get(&p.id).unwrap()` is safe because `has_value` required a present snapshot.

- [ ] **Step 5: Rename the submenu title**

In `build_menu` (around line 195), change:

```rust
    let usage_sub = Submenu::new(app, "账户用量", true)?;
```

to:

```rust
    let usage_sub = Submenu::new(app, "套餐用量", true)?;
```

- [ ] **Step 6: Set the tooltip on refresh**

In `refresh_tray_menu`, update the `Ok(menu)` arm (around line 264) to also set the tooltip:

```rust
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
            let _ = tray.set_tooltip(Some(spec.tooltip.as_str()));
        }
```

(`TrayIcon::set_tooltip(&self, Option<&str>) -> tauri::Result<()>` in Tauri v2. If the compiler reports a different signature, adjust to match — e.g. `tray.set_tooltip(Some(&spec.tooltip))`.)

- [ ] **Step 7: Run the full tray test suite + build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray`
Expected: PASS (new ordering + tooltip tests, all prior tests still green).
Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds cleanly.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): order accounts by usage_order + icon tooltip + rename submenu"
```

---

### Task 6: Frontend — command wrappers, `usageOrder` in config store, migration

**Files:**
- Modify: `src/lib/commands.ts` (two wrappers)
- Modify: `src/stores/config.ts` (`usageOrder` ref, fetch in `loadAll`, `setUsageOrder` action, one-time migration)
- Test: manual / type-check (no unit-test harness for stores in this repo; verify via `npm run build`).

**Interfaces:**
- Consumes: backend `get_usage_order` / `set_usage_order` (Task 2).
- Produces: `api.getUsageOrder()`, `api.setUsageOrder(ids)`; store `config.usageOrder: string[]` and `config.setUsageOrder(ids)`.

- [ ] **Step 1: Add typed command wrappers**

In `src/lib/commands.ts`, add to the config-reads section (near `getProfiles`):

```ts
export const getUsageOrder = () => invoke<string[]>("get_usage_order");
export const setUsageOrder = (ids: string[]) => invoke<void>("set_usage_order", { ids });
```

- [ ] **Step 2: Extend the config store**

In `src/stores/config.ts`:

Add a ref alongside the others (near `const profiles = ref<Profile[]>([]);`):
```ts
  const usageOrder = ref<string[]>([]);
```

In `loadAll`, fetch `usageOrder` and run the one-time migration. Replace the existing `loadAll` body's try-block:
```ts
  async function loadAll() {
    loading.value = true;
    try {
      [providers.value, models.value, profiles.value, fallback.value] = await Promise.all([
        api.getProviders(),
        api.getModels(),
        api.getProfiles(),
        api.getFallbackMap(),
      ]);
      usageOrder.value = await api.getUsageOrder();
      await refreshKeys();
      await migrateUsageOrderIfNeeded();
    } finally {
      loading.value = false;
    }
  }
```

Add the `setUsageOrder` action and the migration helper:
```ts
  /** Persist a new usage display order, then re-fetch (canonical re-fetch-after-mutate). */
  async function setUsageOrder(ids: string[]) {
    await api.setUsageOrder(ids);
    usageOrder.value = await api.getUsageOrder();
  }

  /** One-time: seed `usage_order` from the legacy localStorage drag-order key, then drop it.
   *  Idempotent — no-ops once `usage_order` is non-empty. */
  async function migrateUsageOrderIfNeeded() {
    if (usageOrder.value.length > 0) return;
    try {
      const raw = localStorage.getItem("switchlm:order:usage");
      if (!raw) return;
      const ids = JSON.parse(raw);
      if (!Array.isArray(ids) || ids.length === 0) return;
      await setUsageOrder(ids as string[]);
      localStorage.removeItem("switchlm:order:usage");
    } catch {
      // Malformed legacy key — leave it; backend order simply stays empty (config order).
    }
  }
```

Expose them in the returned object (add `usageOrder` and `setUsageOrder`):
```ts
  return {
    providers,
    models,
    profiles,
    fallback,
    keySet,
    usageSkSet,
    usageOrder,
    loading,
    loadAll,
    saveProvider,
    removeProvider,
    setKey,
    setUsageSk,
    testConnection,
    testConnectionWithPrompt,
    discover,
    saveModel,
    removeModel,
    setFallback,
    saveProfile,
    removeProfile,
    setBacking,
    setUsageOrder,
  };
```

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/lib/commands.ts src/stores/config.ts
git commit -m "feat(store): usageOrder state + setUsageOrder + legacy migration"
```

---

### Task 7: 套餐用量 tab — config-backed reorder + debounced commit

Replace the localStorage `useOrdered` with a config-backed reorder whose drag commit is debounced.

**Files:**
- Modify: `src/views/Usage.vue` (replace `useOrdered` usage; add `sortByUsageOrder` + debounced `commit`)
- Test: manual (drag a card, confirm order persists across reload and matches tray/overview).

**Interfaces:**
- Consumes: `config.usageOrder`, `config.setUsageOrder` (Task 6).
- Produces: an `ordered` ref + `commit` used by the existing `<draggable v-model="ordered" @end="commit">`.

- [ ] **Step 1: Swap the reorder source**

In `src/views/Usage.vue`, remove the `useOrdered` import usage for usage order. Replace this line:
```ts
const { ordered, commit } = useOrdered("switchlm:order:usage", () => cards.value, (c) => c.provider_id);
```
with a config-backed version:
```ts
// Config-backed drag order (single source of truth shared with tray + 概览). Local `ordered`
// updates instantly for a responsive drag; persistence is debounced to coalesce rapid reorder
// into one backend save + tray rebuild.
const ordered = ref<UsageCard[]>([]);
function sortByUsageOrder(items: UsageCard[]): UsageCard[] {
  const rank = new Map<string, number>();
  config.usageOrder.forEach((id, i) => rank.set(id, i));
  return [...items].sort((a, b) => {
    const ia = rank.get(a.provider_id);
    const ib = rank.get(b.provider_id);
    if (ia === undefined && ib === undefined) return 0;
    if (ia === undefined) return 1; // unlisted sink to end
    if (ib === undefined) return -1;
    return ia - ib;
  });
}
watch(
  () => [cards.value, config.usageOrder] as const,
  () => {
    ordered.value = sortByUsageOrder(cards.value);
  },
  { deep: true, immediate: true },
);
let persistTimer: ReturnType<typeof setTimeout> | null = null;
function commit() {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    void config.setUsageOrder(ordered.value.map((c) => c.provider_id));
    persistTimer = null;
  }, 400);
}
```

Ensure `watch` and `ref` are imported (the file already imports `computed, onMounted` from `vue`; add `ref, watch`). The existing `import { useOrdered } from "../lib/useOrdered";` line can stay (it is unused now) or be removed — remove it to avoid a dead import that `vue-tsc`/lint may flag:
```ts
// (remove) import { useOrdered } from "../lib/useOrdered";
```
The template `<draggable v-model="ordered" ... @end="commit">` is unchanged.

- [ ] **Step 2: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors (if `useOrdered` import was removed and nothing else uses it in this file).

- [ ] **Step 3: Commit**

```bash
git add src/views/Usage.vue
git commit -m "feat(usage): config-backed reorder with debounced commit"
```

---

### Task 8: Overview chips ordered by `usage_order`

**Files:**
- Modify: `src/lib/quotaUtils.ts` (`deriveQuotaChips` gains an `order` param; drop the dead `sortKey`)
- Modify: `src/views/Dashboard.vue` (pass `config.usageOrder`)

**Interfaces:**
- Consumes: `config.usageOrder` (Task 6).
- Produces: `deriveQuotaChips(usage, providers, order)` ordering chips by `usage_order` (color/urgency logic untouched).

- [ ] **Step 1: Update `deriveQuotaChips`**

In `src/lib/quotaUtils.ts`:

Remove `sortKey` from the `QuotaChip` interface (it becomes dead once sorting is order-based):
```ts
export interface QuotaChip {
  /** provider_id — stable Vue v-for key. */
  key: string;
  /** provider.display_name (concise; vendor is NOT appended). */
  label: string;
  /** Chip body: "剩 8%" | "CNY 48.77" | "无数据" | "查询失败". */
  text: string;
  /** Hover detail. */
  tooltip?: string;
  /** Drives NTag type. */
  status: ChipStatus;
}
```

Remove the `sortKey: ...` lines from the three `chips.push({ ... })` call sites (the query-failed, no-data, and normal chips). Then change the signature + final sort:
```ts
export function deriveQuotaChips(
  usage: UsageEntry[],
  providers: Provider[],
  order: string[],
): QuotaChip[] {
  const chips: QuotaChip[] = [];
  for (const entry of usage) {
    const provider = providers.find((p) => p.id === entry.provider_id);
    if (!provider || !isUsageSupportedVendor(provider.vendor)) continue;

    const label = provider.display_name;
    const snap = entry.snapshot;

    // Query failed (supported vendor, but no snapshot).
    if (!snap) {
      chips.push({
        key: entry.provider_id,
        label,
        text: "查询失败",
        tooltip: entry.error ?? undefined,
        status: "neutral",
      });
      continue;
    }

    const m = quotaMathFor(snap);
    if (!m) {
      chips.push({
        key: entry.provider_id,
        label,
        text: "无数据",
        tooltip: NO_DATA_TOOLTIP,
        status: "neutral",
      });
      continue;
    }

    chips.push({
      key: entry.provider_id,
      label,
      text: textForChip(m),
      tooltip: tooltipFor(m),
      status: statusFor(m),
    });
  }

  // Order by the shared usage_order; unlisted sink to the end (stable — config order preserved).
  const rank = new Map<string, number>();
  order.forEach((id, i) => rank.set(id, i));
  return chips.sort((a, b) => {
    const ia = rank.get(a.key);
    const ib = rank.get(b.key);
    if (ia === undefined && ib === undefined) return 0;
    if (ia === undefined) return 1;
    if (ib === undefined) return -1;
    return ia - ib;
  });
}
```

Also delete the now-stale doc sentence `* Sort: stable, ascending by sortKey → red plan chips first...` in the `deriveQuotaChips` JSDoc and replace with: `* Sort: by the shared usage_order (unlisted sink to end); chip color still reflects urgency.`

- [ ] **Step 2: Pass the order from Dashboard**

In `src/views/Dashboard.vue`, change (line 51):
```ts
const quotaChips = computed(() => deriveQuotaChips(runtime.usage, config.providers, config.usageOrder));
```

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/lib/quotaUtils.ts src/views/Dashboard.vue
git commit -m "feat(dashboard): order 可用额度 chips by usage_order"
```

---

## Final verification

- [ ] **Backend tests green:** `cargo test --manifest-path src-tauri/Cargo.toml`
- [ ] **Frontend type-checks + builds:** `npm run build`
- [ ] **Manual (run `npm run tauri dev`):**
  - On the 套餐用量 tab, drag an account card. Within ~0.4 s the order persists (reload the tab — order survives).
  - The tray「套餐用量」submenu rows show `5h:% 周:% 月:%` per account, in the same order as the tab.
  - The 概览「可用额度」chips are in that same order.
  - Hover the tray icon: the tooltip lists each account's primary usage (e.g. `智谱 80% · DS CNY 48.77`), ordered the same way; with many accounts it stays ≤ 120 chars with a `…(+N)` tail.
  - Quit/reopen: the order is retained (now persisted in `app_config.json`); the legacy `switchlm:order:usage` localStorage key is gone.
