# Silent Autostart Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a boot-launched (autostart) SwitchLM instance start with its main window hidden to tray, while a manually-launched instance still shows the window.

**Architecture:** The OS autostart entry is registered to launch the app with an extra `--autostart` arg (via `tauri-plugin-autostart::init`'s `args`). In `setup`, a pure helper inspects `std::env::args()`; when the flag is present the window is left hidden (it is created hidden because `tauri.conf.json` sets `visible: false`), and when absent `setup` calls `window.show()` explicitly. No new setting, no frontend change.

**Tech Stack:** Rust / Tauri v2, `tauri-plugin-autostart` 2.5.1 (already a dependency).

## Global Constraints

- **Backend-only.** No frontend, config-type, capability, or dependency changes. No new setting field.
- **Tests are co-located** as `#[cfg(test)] mod tests` in the `.rs` file; run the whole suite with `cargo test --manifest-path src-tauri/Cargo.toml`, one module with `cargo test --manifest-path src-tauri/Cargo.toml <name>`.
- **Package manager is pnpm** for any frontend/dev-server work (`pnpm tauri dev`); do not use `npm`/`npx`. (This plan touches no frontend code, so it mostly does not apply — but if you boot the app for manual QA, use `pnpm tauri dev`.)
- **Commit explicit paths only.** There is a pre-existing unrelated modification (`src-tauri/src/config/secrets.rs`) in the working tree — never `git add -A`; add only the paths each task lists.
- **`tauri.conf.json` is compile-validated** by `tauri::generate_context!()` in `lib.rs`'s `.run(...)` call, so `cargo build` catches any malformed config change.

---

## File Structure

- **Modify** `src-tauri/src/lib.rs` — add the pure `is_autostart_launch` helper at module scope + a co-located `#[cfg(test)] mod tests`; change the autostart plugin `init` call to pass `Some(vec!["--autostart"])`; add a window-visibility decision at the top of `setup`.
- **Modify** `src-tauri/tauri.conf.json` — main window `"visible": true` → `"visible": false`.

No files are created. No other files change.

---

### Task 1: `is_autostart_launch` detection helper (TDD)

**Files:**
- Modify: `src-tauri/src/lib.rs` (add helper at module scope + test module at EOF)

**Interfaces:**
- Produces: `fn is_autostart_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool` — `true` when the process was launched with the `--autostart` arg. Task 2 consumes this.

- [ ] **Step 1: Write the failing tests**

Append this test module to the **end** of `src-tauri/src/lib.rs` (after the closing `}` of `pub fn run()`):

```rust
#[cfg(test)]
mod tests {
    use super::is_autostart_launch;

    #[test]
    fn autostart_flag_present() {
        assert!(is_autostart_launch(["switchlm", "--autostart"]));
    }

    #[test]
    fn autostart_flag_absent() {
        assert!(!is_autostart_launch(["switchlm"]));
        assert!(!is_autostart_launch(Vec::<String>::new()));
    }

    #[test]
    fn autostart_flag_among_other_args() {
        assert!(is_autostart_launch(["switchlm", "--foo", "--autostart", "bar"]));
    }

    #[test]
    fn no_false_positive_on_lookalike() {
        // a flag that merely contains the substring must not match
        assert!(!is_autostart_launch(["switchlm", "--no-autostart", "--autostart-x"]));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml is_autostart`
Expected: compile error — `cannot find function \`is_autostart_launch\`` (the helper does not exist yet).

- [ ] **Step 3: Write the minimal implementation**

Add this function at module scope in `src-tauri/src/lib.rs`, placed just **above** the `#[cfg_attr(mobile, tauri::mobile_entry_point)]` / `pub fn run()` definition (i.e. after the existing `async fn start_proxy_with_retry(...)` function):

```rust
/// True when this process was launched by the OS autostart entry, which appends
/// `--autostart` to the command line. Used in `setup` to keep the main window hidden
/// on boot launches while showing it on every other launch (manual open, dev).
fn is_autostart_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter().any(|a| a.as_ref() == "--autostart")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml is_autostart`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(autostart): add is_autostart_launch arg detector"
```

---

### Task 2: Wire silent autostart (conf + plugin arg + setup visibility)

**Files:**
- Modify: `src-tauri/tauri.conf.json:21`
- Modify: `src-tauri/src/lib.rs` (the `.plugin(tauri_plugin_autostart::init(...))` call ~line 60, and the top of the `.setup(|app| { ... })` block ~line 64)

**Interfaces:**
- Consumes: `is_autostart_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool` from Task 1.

- [ ] **Step 1: Flip the main window to create-hidden**

In `src-tauri/tauri.conf.json`, the `app.windows[0]` object currently has:

```jsonc
        "visible": true,
```

Change it to:

```jsonc
        "visible": false,
```

(Leave `dragDropEnabled` etc. untouched. The window is now created hidden on every launch; Task 2 step 3 re-enables visibility for non-autostart launches.)

- [ ] **Step 2: Make the autostart entry pass the marker arg**

In `src-tauri/src/lib.rs`, the autostart plugin registration currently reads:

```rust
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
```

Change the `None` to `Some(vec!["--autostart"])` (this matches the plugin's signature `args: Option<Vec<&'static str>>`; `"--autostart"` is a `&'static str`):

```rust
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
```

- [ ] **Step 3: Decide window visibility at the top of `setup`**

In `src-tauri/src/lib.rs`, inside `.setup(|app| { ... })`, the first statements are currently:

```rust
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let log_dir = dir.join("logs");
            let log_handle = crate::logging::init_logging(
                &log_dir,
                tracing_subscriber::filter::LevelFilter::INFO,
            );
            // 探测系统密钥环 → 选后端……
```

Insert the visibility decision **immediately after the `init_logging(...)` statement** (so the autostart launch is logged) and **before the `// 探测系统密钥环` comment**. The block to insert is:

```rust
            // 开机自启启动的实例（argv 含 `--autostart`）保持主窗口隐藏，托盘/代理正常运行；
            // 其余启动（手动打开、`tauri dev`）显式 show——tauri.conf.json 已将 visible 置为
            // false 以免开机时先闪一下窗口再隐藏。放在 setup 最前（fallible 配置/密钥环之前），
            // 这样即使后续步骤失败，手动启动也已 show 过窗口，不会卡在隐藏状态。
            if is_autostart_launch(std::env::args()) {
                tracing::info!("开机自启启动，主窗口保持隐藏（托盘/代理正常运行）");
            } else if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
```

Rationale for placement: `get_webview_window("main")` resolves because the main window is created from config before `setup` runs. Placing this before the fallible keyring/config block means a manual launch has already shown its window even if a later step errors.

- [ ] **Step 4: Build to verify everything compiles (and the conf change validates)**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: clean build. (`generate_context!()` validates `tauri.conf.json`; the autostart `Some(vec!["--autostart"])` type-checks against `Option<Vec<&'static str>>`; the new `setup` block borrows `app` immutably via `get_webview_window`, which is fine inside the `|app|` setup closure.)

- [ ] **Step 5: Run the full backend test suite to confirm nothing regressed**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass (the existing `app_config_roundtrips` / settings tests are untouched; Task 1's `is_autostart_launch` tests still pass).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/tauri.conf.json src-tauri/src/lib.rs
git commit -m "feat(autostart): start silent (window hidden) on boot launch"
```

---

### Task 3: Manual verification (no commit)

End-to-end behavior (boot launch hidden, manual launch shown, reveal path) is not unit-testable; verify it by hand.

- [ ] **Step 1: Dev launch still shows the window**

Run: `pnpm tauri dev`
Expected: the dev window appears as before (no `--autostart` arg → `setup` calls `show()`). Close the dev app when done.

- [ ] **Step 2: Enable autostart and inspect the OS entry**

In the running app: Settings → 开机自启 → enable. Then confirm the OS autostart entry now launches with the marker arg:

- **Windows:** `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v SwitchLM` (or `com.switchlm`) — the value should contain `--autostart` after the exe path. If the value name differs, `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run"` to list entries and find the SwitchLM one.

Expected: the registry Run value ends with `--autostart`.

- [ ] **Step 3: Reboot / re-login and confirm silent start**

Sign out and back in (or reboot). Expected: **no** SwitchLM window appears; the tray icon is present. Confirm the proxy is live by hitting it (e.g. the agent env snippet URL on the bound port responds, or Settings shows the server running once you open the window).

- [ ] **Step 4: Reveal path works**

From the tray, click **打开主窗口**. Expected: the main window appears and is functional.

Then, with the window hidden again (close → hides to tray), double-click the SwitchLM shortcut. Expected (release build only — single-instance is debug-disabled): the existing instance's window is brought to the foreground (no second instance, no second proxy/port).

- [ ] **Step 5: Disable autostart reverts to no-launch**

Settings → 开机自启 → disable. Re-sign-in/reboot. Expected: SwitchLM does not launch at all.

- [ ] **Step 6: Note the first-boot-after-upgrade caveat (no action needed)**

An autostart entry written by a prior (flag-less) version lacks `--autostart`, so the very first boot after this upgrade still shows the window once. It self-heals the next time `toggle_autostart` rewrites the entry (Step 2 above already did). No migration code is required; this is expected.

---

## Notes for the implementer

- **Do not touch** `toggle_autostart`, `settings.autostart`, the tray menu, the single-instance callback, or `src-tauri/capabilities/default.json` — they are correct as-is and compose with this change automatically (see the spec's "How the cases land" table).
- **Do not `git add -A`.** Add only the two paths in Task 2's commit step (plus `src-tauri/src/lib.rs` in Task 1). The unrelated `secrets.rs` working-tree change must stay out of these commits.
