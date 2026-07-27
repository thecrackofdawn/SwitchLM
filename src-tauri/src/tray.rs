//! System-tray menu: inline account quota % + tooltip.
//!
//! The menu structure (`tray_menu_spec`) is pure data built from `AppState`, so the label /
//! quota logic is unit-tested without a Tauri app handle. `build_tray` turns the spec into a
//! native `TrayIcon`; the menu is rebuilt on a timer so quota % stays current - Tauri v2 has no
//! "menu about-to-show" hook, so a periodic refresh is the robust substitute. Route editing
//! lives only in the app window (the 路由 tab); the tray surfaces accounts + tooltip + controls.

use std::collections::{HashMap, HashSet};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::commands;
use crate::config::AppConfig;
use crate::proxy::AppState;
use crate::usage::{UsageSnapshot, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY};

const TRAY_ID: &str = "main";

// ---- Pure menu spec (unit-tested) ----

#[derive(Debug, PartialEq)]
pub struct TrayMenuSpec {
    pub accounts: Vec<AccountUsageSpec>,
    /// Native tray-icon tooltip text (套餐用量 summary). "SwitchLM" when no usage.
    pub tooltip: String,
}

#[derive(Debug, PartialEq)]
pub struct AccountUsageSpec {
    pub provider_id: String,
    /// Display label, e.g. "智谱 80%" / "DS工作号 CNY 48.77" / "火山号A [volcengine] 45%".
    pub label: String,
}

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
/// Plan only - consumption is handled separately in `account_label`.
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

/// Primary value for the tray-icon tooltip: plan -> "80%" (5h used, falling back to top-level used);
/// consumption -> "CNY 48.77". `None` when no value can be derived.
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

/// Compact 套餐用量 summary for the tray icon tooltip, one account per line. `lines` are
/// pre-formatted "name value" strings, already ordered & filtered by the caller. Joined with
/// "\n" (native tray tooltips render it as a line break), hard-capped at 120 chars; whole
/// lines only, with " …(+N)" when lines are dropped. Empty -> app name.
fn usage_tooltip(lines: &[String]) -> String {
    if lines.is_empty() {
        return "SwitchLM".to_string();
    }
    const CAP: usize = 120;
    const SEP: &str = "\n";
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

/// Tray label for one account's usage: display name + (multi-account) vendor tag + usage display.
/// `None` when the account has no concrete usage value - the caller hides that account.
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

/// Account-row usage text (no leading name): plan -> tier summary; consumption -> "CNY 48.77".
/// `None` hides the row.
fn account_display(u: &UsageSnapshot) -> Option<String> {
    if u.billing_model == "consumption" {
        u.remaining.map(|r| format!("{} {:.2}", u.unit, r))
    } else {
        // Plan rows without tier data are hidden (tier_summary returns None).
        tier_summary(u)
    }
}

/// Build the tray menu spec from the current config + usage (pure).
pub fn tray_menu_spec(
    cfg: &AppConfig,
    usage: &HashMap<String, UsageSnapshot>,
) -> TrayMenuSpec {
    // Vendors with >=2 providers: their accounts are ambiguous by name alone, so the label shows
    // ` [vendor]` to disambiguate. Single-account vendors stay clutter-free.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in &cfg.providers {
        *counts.entry(p.vendor.as_str()).or_default() += 1;
    }
    let multi: HashSet<&str> = counts.iter().filter(|(_, n)| **n >= 2).map(|(v, _)| *v).collect();
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
    TrayMenuSpec { accounts, tooltip }
}

// ---- Native tray (Tauri v2) ----

fn build_menu(app: &AppHandle, spec: &TrayMenuSpec) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;

    // 套餐用量：无数据时不显示 ">" 箭头，渲染为禁用扁平项。
    if spec.accounts.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "usage_empty",
            "套餐用量（无数据）",
            false,
            None::<&str>,
        )?)?;
    } else {
        let usage_sub = Submenu::new(app, "套餐用量", true)?;
        for acc in &spec.accounts {
            usage_sub.append(&MenuItem::with_id(
                app,
                format!("acct:{}", acc.provider_id),
                acc.label.clone(),
                true,
                None::<&str>,
            )?)?;
        }
        menu.append(&usage_sub)?;
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?)?;
    Ok(menu)
}

/// Create the system-tray icon + menu. `cfg` seeds the initial menu (usage is empty at
/// startup; the periodic refresh repopulates quota %).
pub fn build_tray(app: &AppHandle, cfg: &AppConfig) -> tauri::Result<()> {
    let spec = tray_menu_spec(cfg, &HashMap::new());
    let menu = build_menu(app, &spec)?;
    tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().expect("app has no window icon").clone())
        .tooltip("SwitchLM")
        .menu(&menu)
        // Left-click toggles the main window (see on_tray_icon_event). The tray menu is modal on
        // Windows (TrackPopupMenu blocks the message loop), so showing it on left-click would
        // swallow the click event; the menu is reached via right-click instead.
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_icon_event)
        .build(app)?;
    Ok(())
}

/// Gather per-provider usage snapshots (60s-cached) for tray labels.
async fn gather_usage(state: &AppState) -> HashMap<String, UsageSnapshot> {
    let ids: Vec<String> = state.config.read().await.providers.iter().map(|p| p.id.clone()).collect();
    let mut map = HashMap::new();
    for id in ids {
        if let Ok(snap) = commands::query_usage(state, &id).await {
            map.insert(id, snap);
        }
    }
    map
}

/// Rebuild the tray menu from the live AppState (config + cached usage).
pub async fn refresh_tray_menu(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let state = app.state::<AppState>().inner().clone();
    let spec = {
        let cfg = state.config.read().await.clone();
        let usage = gather_usage(&state).await;
        tray_menu_spec(&cfg, &usage)
    };
    match build_menu(app, &spec) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
            let _ = tray.set_tooltip(Some(spec.tooltip.as_str()));
        }
        Err(e) => tracing::warn!("tray menu rebuild failed: {e}"),
    }
}

/// Tray left-click action decided from the window's visibility + minimized state.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ToggleAction {
    Hide,
    Summon,
}

/// Decide the tray left-click action from the window's visibility + minimized state.
///
/// `visible` / `minimized` are `Option<bool>`: `None` means the underlying Tauri query errored.
///
/// **Why not `is_focused()` (the original 2026-08-04 design):** clicking the system-tray icon
/// transfers focus to the shell, so by the time the click handler runs `is_focused()` is already
/// `false` even for a genuinely foreground window. That made the "foreground -> hide" branch
/// unreachable: the first click summoned the window, but the second click read `is_focused()==false`
/// (the tray click had just stolen focus) and re-ran the summon path instead of hiding. Visibility
/// and minimized state are *not* perturbed by a tray click, so they are a stable signal at click
/// time. Trade-off: a window that is visible but covered by another app now hides on click instead
/// of being raised (click again to show it) — the focus-based "covered -> raise" behaviour cannot
/// be made reliable, so reliable hide wins.
///
/// Errors never hide (safe default = Summon): accidentally hiding a window the user wanted is
/// worse than a redundant show + focus.
fn toggle_action(visible: Option<bool>, minimized: Option<bool>) -> ToggleAction {
    match (visible, minimized) {
        // Shown on screen (visible and not minimized) -> hide it.
        (Some(true), Some(false)) => ToggleAction::Hide,
        // Hidden, minimized, or any query error -> (re)show + focus.
        _ => ToggleAction::Summon,
    }
}

/// Toggle the main window (used by the tray-icon left-click): hide it when it is shown on screen
/// (visible and not minimized); otherwise summon it — unminimize if minimized, show and focus.
/// Decides via `toggle_action` (visibility/minimized) rather than `is_focused()`, because the tray
/// click itself steals focus (see `toggle_action`). Query errors default to Summon: never hide.
fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let visible = window.is_visible().ok();
    let minimized = window.is_minimized().ok();
    match toggle_action(visible, minimized) {
        ToggleAction::Hide => {
            let _ = window.hide();
        }
        ToggleAction::Summon => {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

/// Left-click the tray icon -> toggle the main window (hidden / minimized -> summon + focus;
/// shown on screen -> hide). See `toggle_main_window` / `toggle_action` for why focus is not used
/// (a tray click steals focus). The menu opens via right-click.
fn on_tray_icon_event(tray: &TrayIcon, event: TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        toggle_main_window(tray.app_handle());
    }
}

fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    match id {
        // `app.exit(0)` doesn't hard-exit: it posts an `ExitRequested` event to the main event
        // loop (see `AppHandle::exit` in tauri). This tray callback runs synchronously ON the main
        // thread (Windows' modal TrackPopupMenu), and Tauri cannot reliably deliver exit events
        // issued from the main thread. Spawn onto the async runtime (a worker thread) so the
        // request is delivered - same reason the async `quit_app` IPC command works. Returning
        // here lets TrackPopupMenu close; the worker's request_exit is then pumped and the app
        // quits. Do NOT "simplify" back to a direct `app.exit(0)` - it silently no-ops on Windows.
        "quit" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                app.exit(0);
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::usage::UsageTier;

    // ---- toggle_action (left-click hide/summon decision) ----

    #[test]
    fn toggle_action_hides_when_shown_on_screen() {
        // Visible and not minimized -> the user sees the window -> hide it.
        assert_eq!(toggle_action(Some(true), Some(false)), ToggleAction::Hide);
    }

    #[test]
    fn toggle_action_summons_when_hidden() {
        assert_eq!(toggle_action(Some(false), Some(false)), ToggleAction::Summon);
        // Hidden window with unknown/minimized state still summons.
        assert_eq!(toggle_action(Some(false), None), ToggleAction::Summon);
    }

    #[test]
    fn toggle_action_summons_when_minimized() {
        // On Windows a minimized window still reports visible=true; minimized routes to summon so
        // the click restores it instead of hiding.
        assert_eq!(toggle_action(Some(true), Some(true)), ToggleAction::Summon);
    }

    #[test]
    fn toggle_action_summons_on_query_error() {
        // Never hide when a state query failed - safe default is to summon.
        assert_eq!(toggle_action(None, Some(false)), ToggleAction::Summon);
        assert_eq!(toggle_action(Some(true), None), ToggleAction::Summon);
        assert_eq!(toggle_action(None, None), ToggleAction::Summon);
    }

    fn mk_provider(id: &str, name: &str) -> Provider {
        Provider {
            id: id.into(),
            vendor: id.into(),
            display_name: name.into(),
            openai_base_url: Some("https://x/v1".into()),
            anthropic_base_url: None,
            usage_creds: None,
        }
    }

    #[test]
    fn account_label_plan_shows_tiers() {
        let u = snap_tiers(Some(80.0), Some(40.0), Some(10.0));
        assert_eq!(account_label("智谱", None, &u).unwrap(), "智谱 5h:80% 周:40% 月:10%");
    }

    #[test]
    fn account_label_consumption_shows_balance() {
        let u = UsageSnapshot {
            used: Some(51.23),
            total: Some(100.0),
            remaining: Some(48.77),
            reset_at: None,
            unit: "CNY".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "consumption".into(),
            plan_info: None,
        };
        assert_eq!(account_label("DS工作号", None, &u).unwrap(), "DS工作号 CNY 48.77");
    }

    #[test]
    fn account_label_appends_vendor_tag() {
        let u = snap_tiers(Some(45.0), Some(30.0), Some(10.0));
        assert_eq!(
            account_label("火山号A", Some("volcengine"), &u).unwrap(),
            "火山号A [volcengine] 5h:45% 周:30% 月:10%"
        );
    }

    #[test]
    fn account_label_none_when_no_usage_value() {
        let none = UsageSnapshot {
            used: None,
            total: None,
            remaining: None,
            reset_at: None,
            unit: "%".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "plan".into(),
            plan_info: None,
        };
        assert!(account_label("自定义", None, &none).is_none());
    }

    #[test]
    fn spec_accounts_lists_usage_per_provider_in_order() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        cfg.providers.push(mk_provider("deepseek", "DS工作号"));
        let mut usage = HashMap::new();
        usage.insert("zhipu".into(), snap_tiers(Some(80.0), Some(40.0), Some(10.0)));
        usage.insert(
            "deepseek".into(),
            UsageSnapshot {
                used: Some(51.23),
                total: Some(100.0),
                remaining: Some(48.77),
                reset_at: None,
                unit: "CNY".into(),
                raw_summary: None,
                plan: None,
                tiers: vec![],
                billing_model: "consumption".into(),
                plan_info: None,
            },
        );

        let spec = tray_menu_spec(&cfg, &usage);
        assert_eq!(spec.accounts.len(), 2);
        assert_eq!(spec.accounts[0].provider_id, "zhipu");
        assert_eq!(spec.accounts[0].label, "智谱 5h:80% 周:40% 月:10%");
        assert_eq!(spec.accounts[1].provider_id, "deepseek");
        assert_eq!(spec.accounts[1].label, "DS工作号 CNY 48.77");
    }

    #[test]
    fn spec_accounts_hides_providers_without_usage() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        cfg.providers.push(mk_provider("custom", "自定义")); // no adapter / absent from map
        let mut usage = HashMap::new();
        usage.insert("zhipu".into(), snap_tiers(Some(80.0), Some(40.0), Some(10.0)));
        // "custom" deliberately absent

        let spec = tray_menu_spec(&cfg, &usage);
        assert_eq!(spec.accounts.len(), 1);
        assert_eq!(spec.accounts[0].provider_id, "zhipu");
    }

    #[test]
    fn spec_accounts_hides_present_but_empty_snapshot() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        cfg.providers.push(mk_provider("degraded", "降级账号"));
        let empty = UsageSnapshot {
            used: None,
            total: None,
            remaining: None,
            reset_at: None,
            unit: "%".into(),
            raw_summary: Some("raw fallback text".into()),
            plan: None,
            tiers: vec![],
            billing_model: "plan".into(),
            plan_info: None,
        };
        let mut usage = HashMap::new();
        usage.insert("zhipu".into(), snap_tiers(Some(80.0), Some(40.0), Some(10.0)));
        usage.insert("degraded".into(), empty); // present in map, but no concrete value

        let spec = tray_menu_spec(&cfg, &usage);
        assert_eq!(spec.accounts.len(), 1);
        assert_eq!(spec.accounts[0].provider_id, "zhipu");
        // "degraded" is in the usage map but yields no usage_display -> hidden
        assert!(spec.accounts.iter().all(|a| a.provider_id != "degraded"));
    }

    #[test]
    fn spec_accounts_vendor_tag_only_for_multi_account_vendor() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("volc1", "火山号A"));
        cfg.providers.push(mk_provider("volc2", "火山号B"));
        cfg.providers[0].vendor = "volcengine".into();
        cfg.providers[1].vendor = "volcengine".into();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        let mut usage = HashMap::new();
        usage.insert("volc1".into(), snap_tiers(Some(40.0), Some(20.0), Some(5.0)));
        usage.insert("volc2".into(), snap_tiers(Some(70.0), Some(50.0), Some(15.0)));
        usage.insert("zhipu".into(), snap_tiers(Some(80.0), Some(40.0), Some(10.0)));

        let spec = tray_menu_spec(&cfg, &usage);
        let by_id: HashMap<&str, &AccountUsageSpec> =
            spec.accounts.iter().map(|a| (a.provider_id.as_str(), a)).collect();
        assert_eq!(by_id["volc1"].label, "火山号A [volcengine] 5h:40% 周:20% 月:5%");
        assert_eq!(by_id["volc2"].label, "火山号B [volcengine] 5h:70% 周:50% 月:15%");
        assert_eq!(by_id["zhipu"].label, "智谱 5h:80% 周:40% 月:10%"); // single-account vendor: no tag
    }

    #[test]
    fn spec_accounts_empty_when_no_provider_has_usage() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("custom", "自定义"));
        let spec = tray_menu_spec(&cfg, &HashMap::new());
        assert!(spec.accounts.is_empty());
    }

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

        let spec = tray_menu_spec(&cfg, &usage);
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

        let spec = tray_menu_spec(&cfg, &usage);
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
        let spec = tray_menu_spec(&cfg, &usage);
        assert_eq!(spec.tooltip, "DS CNY 48.77\n智谱 80%");
    }

    #[test]
    fn spec_tooltip_is_switchlm_when_no_usage() {
        let cfg = AppConfig::default();
        let spec = tray_menu_spec(&cfg, &HashMap::new());
        assert_eq!(spec.tooltip, "SwitchLM");
    }

    // ---- Tier/tooltip helpers ----

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
    fn usage_tooltip_one_line_per_account() {
        let lines = vec!["智谱 80%".to_string(), "DeepSeek CNY 48.77".to_string()];
        assert_eq!(usage_tooltip(&lines), "智谱 80%\nDeepSeek CNY 48.77");
    }

    #[test]
    fn usage_tooltip_truncates_many_accounts_with_ellipsis() {
        // 15 accounts (10 chars each). Joined by "\n", all 15 = 164 chars > the 120 cap, so
        // truncation is forced and a " …(+N)" tail must appear; every included line must be
        // whole (no line cut mid-name). Note: with the 1-char "\n" separator 10 accounts fit
        // in 109 chars and would NOT truncate, so 15 are needed to hit the cap.
        let lines: Vec<String> = (0..15).map(|i| format!("账号编号{:02} 80%", i)).collect();
        let out = usage_tooltip(&lines);
        assert!(out.chars().count() <= 120, "len={} out={out}", out.chars().count());
        assert!(out.contains("…(+"), "missing ellipsis: {out}");
        // The head (before the ellipsis) is newline-joined whole input lines.
        let head = out.split(" …(+").next().unwrap();
        for unit in head.split('\n') {
            assert!(lines.contains(&unit.to_string()), "partial/unknown unit in tooltip: {unit:?}");
        }
    }
}
