# Tray Click Toggle + Multi-Line Tooltip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Left-clicking the tray icon toggles the main window (foreground-aware), and the tray hover tooltip shows one account per line.

**Architecture:** Both changes live in `src-tauri/src/tray.rs`. The tooltip change is a pure-function separator swap (`" · "` → `"\n"`), fully covered by the existing unit-test suite. The click change replaces `show_main_window` with a `toggle_main_window` that queries `is_visible()`/`is_focused()` — Tauri runtime calls, so it stays a thin handler verified manually.

**Tech Stack:** Rust / Tauri v2 (`tauri::tray`, `WebviewWindow` API), co-located `#[cfg(test)]` unit tests, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-08-04-tray-click-toggle-and-multiline-tooltip-design.md`

## Global Constraints

- Only `src-tauri/src/tray.rs` is modified. Do NOT touch the single-instance callback in `src-tauri/src/lib.rs:51-56` (it must stay unconditional show+focus), the right-click menu behavior, or close-to-tray in `lib.rs` `on_window_event`.
- Window-state query errors default to "not in the foreground" → summon the window (never hide on an error).
- Tooltip keeps: 120-char cap, whole-line-only truncation with ` …(+N)` tail, `"SwitchLM"` when no usage, `usage_order` ordering, no-value filter, primary-value content (`tooltip_value`).
- Backend tests: `cargo test --manifest-path src-tauri/Cargo.toml`. Frontend is untouched — no `pnpm` build needed.
- Commit steps are included per the repo's spec-driven convention, but the user commits on request and pushes in batches — run a commit step only if the user has asked to commit.

---

### Task 1: Multi-line tooltip (separator `" · "` → `"\n"`)

**Files:**
- Modify: `src-tauri/src/tray.rs` — `usage_tooltip` (lines 85-119, incl. doc comment), tests `spec_tooltip_lists_primary_values_in_order` (513-531), `usage_tooltip_joins_few_accounts` (609-613), `usage_tooltip_truncates_many_accounts_with_ellipsis` (615-628)

**Interfaces:**
- Consumes: existing pure fn `usage_tooltip(lines: &[String]) -> String`, called by `tray_menu_spec`
- Produces: same signature; only the joining separator changes. Later tasks do not depend on this one.

- [ ] **Step 1: Update the three tests to the new expected behavior**

In `src-tauri/src/tray.rs`, replace the three test bodies/assertions:

```rust
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
```

```rust
    #[test]
    fn usage_tooltip_one_line_per_account() {
        let lines = vec!["智谱 80%".to_string(), "DeepSeek CNY 48.77".to_string()];
        assert_eq!(usage_tooltip(&lines), "智谱 80%\nDeepSeek CNY 48.77");
    }
```

```rust
    #[test]
    fn usage_tooltip_truncates_many_accounts_with_ellipsis() {
        // 15 accounts -> must cap at <= 120 chars, drop some with " …(+N)", and every
        // included unit must be whole (no line cut mid-name). 10-char lines joined by
        // 1-char "\n": 12 accounts already fit (131 > 120 only at 12+), so 15 are
        // needed to force truncation.
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
```

(`usage_tooltip_empty_is_app_name` stays as-is.)

- [ ] **Step 2: Run the tests to verify they fail for the right reason**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::`
Expected: 3 FAIL — `spec_tooltip_lists_primary_values_in_order`, `usage_tooltip_one_line_per_account` (currently named `usage_tooltip_joins_few_accounts`; replace it), `usage_tooltip_truncates_many_accounts_with_ellipsis`. Failures are assertion mismatches on `" · "` vs `"\n"` output, not compile errors.

- [ ] **Step 3: Change the separator and the doc comment**

In `usage_tooltip`, change `const SEP: &str = " · ";` to `const SEP: &str = "\n";`, and update the doc comment to:

```rust
/// Compact 套餐用量 summary for the tray icon tooltip, one account per line. `lines` are
/// pre-formatted "name value" strings, already ordered & filtered by the caller. Joined with
/// "\n" (native tray tooltips render it as a line break), hard-capped at 120 chars; whole
/// lines only, with " …(+N)" when lines are dropped. Empty -> app name.
```

No other logic changes — the k-fit loop and the single-line hard-truncate fallback work unchanged with the shorter separator.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::`
Expected: all PASS, including `usage_tooltip_empty_is_app_name`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): 悬停文本按账户换行（分隔符 \" · \" → 换行）"
```

---

### Task 2: Foreground-aware left-click toggle

**Files:**
- Modify: `src-tauri/src/tray.rs` — `show_main_window` (lines 274-280), `on_tray_icon_event` (282-292), `build_tray` doc comment (232-235)

**Interfaces:**
- Consumes: `tauri::WebviewWindow` methods `is_visible()`, `is_focused()`, `hide()`, `show()`, `unminimize()`, `set_focus()` (all `tauri::Result`)
- Produces: `fn toggle_main_window(app: &AppHandle)`, called from `on_tray_icon_event`

- [ ] **Step 1: Replace `show_main_window` with `toggle_main_window`**

Delete `show_main_window` and add, in its place:

```rust
/// Toggle the main window (used by the tray-icon left-click): hide it when it's genuinely in
/// the foreground (visible AND focused); otherwise summon it - unminimize if minimized, show
/// and focus. Query errors count as "not in the foreground": better to summon than to hide.
fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let in_foreground =
        window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false);
    if in_foreground {
        let _ = window.hide();
    } else {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
```

Note: on Windows a minimized window still reports `is_visible() == true`; the `is_focused()` check routes it to the summon branch, where `unminimize()` restores it (no-op otherwise).

- [ ] **Step 2: Point the event handler at the toggle**

In `on_tray_icon_event`, replace `show_main_window(tray.app_handle());` with `toggle_main_window(tray.app_handle());` and update its doc comment:

```rust
/// Left-click the tray icon -> toggle the main window (hidden / covered / minimized ->
/// summon + focus; in the foreground -> hide). The menu opens via right-click.
```

- [ ] **Step 3: Update the `build_tray` comment**

Change the comment above `.show_menu_on_left_click(false)` from "Left-click opens the main window (see on_tray_icon_event)." to:

```rust
        // Left-click toggles the main window (see on_tray_icon_event). The tray menu is modal on
        // Windows (TrackPopupMenu blocks the message loop), so showing it on left-click would
        // swallow the click event; the menu is reached via right-click instead.
```

- [ ] **Step 4: Compile + full backend suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: compiles, all tests PASS (this task adds no unit tests — the handler wraps Tauri runtime window calls that have no headless equivalent).

- [ ] **Step 5: Manual verification (GUI)**

Run: `pnpm tauri dev`, then check each case via the tray icon:

| Setup | Left-click | Expected |
|---|---|---|
| Window closed-to-tray (hidden) | click | window shows + focused |
| Window visible and focused | click | window hides to tray |
| Window visible but another app in front | click | window jumps to front (not hidden) |
| Window minimized | click | window restores + focused |
| Right-click | — | menu still opens, 退出 still works |

Also confirm the hover tooltip shows one account per line (needs ≥1 account with usage data; otherwise it shows "SwitchLM").

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): 左键单击前台感知切换主窗口（在前台→隐藏，否则唤起）"
```
