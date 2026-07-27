# Silent Autostart Design

**Date:** 2026-08-05
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

## Overview

SwitchLM already boots at login when **开机自启** is on (`tauri-plugin-autostart`,
`toggle_autostart` in `commands.rs`). Today, however, a boot-launched instance is
indistinguishable from a manually-clicked one, so the **main window pops up every
reboot**. For a tray-resident proxy this is noise: the user enabled autostart so the
proxy + tray are ready, not so a window appears.

This work makes **boot-launched instances start silent** — the main window stays
hidden to tray while the proxy and tray keep running exactly as they do today. The
scope is deliberately narrow: **only** an autostart launch is silent. A manual launch
(double-clicking the icon) still shows the window as it does now, regardless of whether
autostart is enabled. No new setting is introduced — the existing 开机自启 toggle is the
sole control, and enabling it now means "start silent at boot."

## Background

### How autostart works today

- `tauri-plugin-autostart` is registered in `src-tauri/src/lib.rs:60` with
  `init(MacosLauncher::LaunchAgent, None)`. The second argument is the **extra args**
  appended to the OS autostart command; it is currently `None`, so a boot-launched
  process receives the same argv as a manually-launched one.
- The main window is `"visible": true` in `src-tauri/tauri.conf.json:21`, so **every**
  launch shows the window immediately at process start.
- `toggle_autostart` (`commands.rs:805`) only flips the OS entry and persists
  `settings.autostart: bool`; it has no notion of window visibility.
- The close button already hides to tray (`lib.rs:136-143`), and the tray exposes a
  **打开主窗口** item to reveal it again — so the "reveal a hidden window" path already
  exists and is reused unchanged.

### How to tell a boot launch apart from a manual one

The only robust, cross-platform signal is a **launch argument**. The autostart plugin
appends its `args` to the autostart command on every platform — the Windows `Run`
registry value, the macOS `LaunchAgent` `ProgramArguments`, and the Linux `.desktop`
`Exec` line alike. Registering the plugin with `Some(["--autostart"])` therefore marks
every boot launch, and only a boot launch, with `--autostart` in `std::env::args()`. A
manual launch (and `tauri dev`) never carries the flag.

Runtime registry/parent-process introspection was rejected: platform-specific, fragile,
and with no test surface.

### Why `visible` flips to `false`

If the window stays `"visible": true`, a boot-launched instance would paint briefly
before `setup` hides it — a startup flash. Setting `"visible": false` means the window
is **created hidden** on every launch; `setup` then calls `show()` explicitly for
non-autostart launches. The autostart branch simply skips the `show()`, so no flash is
possible.

## Requirements

### Functional

1. **Silent on autostart.** When the OS launches SwitchLM at login (autostart on), the
   main window stays hidden. The proxy binds its port and the tray is available, exactly
   as on a normal launch.
2. **Windowed on manual launch.** Double-clicking the app icon (or running the binary
   directly) shows the main window as today — whether or not autostart is enabled.
3. **Reveal path unchanged.** The existing tray **打开主窗口** item and the
   single-instance second-launch callback (show + focus the running window) both bring a
   hidden window to the foreground with no new code.
4. **No new setting.** The 开机自启 switch remains the only user-facing control.
   Enabling it means "start silent at boot"; there is no separate silent/windowed choice.
5. **No flash.** A boot-launched instance never paints its window before being hidden.
6. **Dev unaffected.** `tauri dev` shows the dev window as usual.

### Non-functional

- **Backend-only, minimal.** Three edits: `tauri.conf.json` (`visible: false`), `lib.rs`
  (autostart `args` + a visibility decision at the top of `setup`), and a pure helper
  `is_autostart_launch` with co-located tests. No frontend, config-type, capability, or
  dependency change.
- **Testable detection.** The `--autostart` check lives in a pure function over an args
  iterator, unit-tested for present / absent / multi-arg cases (mirrors the repo's
  pure-core + co-located-test convention).

## Design

### `src-tauri/tauri.conf.json`

Main window `"visible": true` → `"visible": false`. No other window field changes; the
default size, `minWidth`/`minHeight`, and `dragDropEnabled` are untouched.

### `src-tauri/src/lib.rs`

**1. Register autostart with the marker arg:**

```rust
.plugin(tauri_plugin_autostart::init(
    tauri_plugin_autostart::MacosLauncher::LaunchAgent,
    Some(vec!["--autostart"]),
))
```

The flag is baked into the plugin init, so every `manager.enable()` (from
`toggle_autostart` or anywhere else) writes an autostart entry that launches with
`--autostart`. There is no per-call arg to manage.

**2. Decide window visibility at the top of `setup`**, right after logging is
initialized and before any fallible config / keyring work:

```rust
if is_autostart_launch(std::env::args()) {
    tracing::info!("开机自启启动，主窗口保持隐藏（托盘/代理正常运行）");
} else if let Some(w) = app.get_webview_window("main") {
    let _ = w.show();
}
```

The main window is created from config before `setup` runs, so `get_webview_window("main")`
resolves. Because `visible` is now `false`, doing this first thing means a manual launch
shows the window within microseconds of `setup` starting, and a boot launch never shows
it. Placing the decision before the fallible block means a later setup failure cannot
strand the user with a hidden window on a manual launch (and a panic propagates to
`run().expect()` regardless).

**3. Pure helper + co-located tests:**

```rust
/// True when this process was launched by the OS autostart entry, which appends
/// `--autostart` to the command line. Used to keep the window hidden on boot launches.
fn is_autostart_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter().any(|a| a.as_ref() == "--autostart")
}

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
}
```

`is_autostart_launch` is defined at module scope in `lib.rs` so the test module can reach
it via `super::`, matching the repo's co-located-test convention.

### What is explicitly not changed

- `toggle_autostart` / `settings.autostart` — the toggle's meaning is unchanged; only the
  consequence (boot launches now carry `--autostart` and start hidden) is new.
- The single-instance callback (`lib.rs:50-56`) — on a second manual launch it already
  calls `show()` + `set_focus()` on the existing window, which correctly reveals an
  autostarted-hidden instance. No edit needed.
- The tray menu and its **打开主窗口** handler — the existing reveal path.
- `src-tauri/capabilities/default.json` — Rust-side `window.show()/hide()` need no
  permission token; the existing `core:window:allow-{hide,show,close}` entries serve the
  frontend and are unaffected.
- The frontend generally — no Settings.vue, store, types, or command change. The
  startup-window-fit logic (`Dashboard.vue` `onMounted`) still runs in the hidden webview
  and resizes the (hidden) window once; when the user later reveals it, it is already at
  the fitted size. Harmless.

## Edge cases

| Case | Behavior |
|---|---|
| Reboot with autostart **on** | OS launches with `--autostart` → window hidden → proxy + tray run. |
| Reboot with autostart **off** | App is not launched at all; nothing changes. |
| Double-click icon, autostart on or off | No `--autostart` arg → window shows normally. |
| Autostarted & hidden, then user clicks icon while running | single-instance callback fires → existing window `show()` + `set_focus()`. |
| `tauri dev` (debug) | No flag → `show()` → dev window appears. single-instance is debug-disabled, so this is independent. |
| Linux, no keyring (PendingStore) + autostart hidden | The keyring-consent modal renders in the hidden webview and is revealed when the user opens the window from the tray. Accepted edge — on Windows/macOS the keyring is available so the modal never appears. |
| `setup` fails after the visibility decision on a manual launch | Window was already `show()`-ed first, so the user sees it; a panic propagates to `run().expect()` and exits the app regardless. |
| Old autostart entries written before this change | The OS re-reads its autostart config on next login only after `toggle_autostart` re-writes it. An entry written by a prior version lacks `--autostart`, so the **first** post-upgrade boot still shows the window; subsequent boots are silent once the entry is rewritten. No migration needed — acceptable, self-healing. |

## Out of scope

- **A separate "静默启动" toggle** (autostart-with-window vs autostart-silent). The chosen
  scope is silent-only-on-autostart with no new setting; a toggle can be added later if
  needed.
- **Persisting "start hidden" across manual launches** — a manual launch always shows the
  window.
- **Migrating pre-existing autostart registry entries** to include the flag — see the
  edge case above; self-healing on next toggle.
- **Changing the autostart toggle's UI, the tray menu, or any frontend surface.**

## Testing

- **Unit (deterministic):** `cargo test --manifest-path src-tauri/Cargo.toml is_autostart`
  covers the helper's present / absent / empty / multi-arg cases.
- **Compile/type gate:** `cargo build` confirms the `init(.., Some(..))` shape and the
  `setup` edits type-check; the existing `app_config_roundtrips` and settings tests are
  untouched and must still pass (`cargo test`).
- **Manual QA (Windows):**
  1. Enable 开机自启 in Settings (or via the tray), confirm it persists.
  2. Reboot / re-login → confirm **no** window appears and the tray icon is present.
  3. Confirm the proxy is reachable on its port (e.g. the agent env snippet still works).
  4. Tray → 打开主窗口 → the window appears and is functional.
  5. With the app running hidden, double-click the app icon → single-instance brings the
     existing window to the foreground.
  6. Disable 开机自启, reboot → app does not launch at all.
- **Manual QA (dev):** `pnpm tauri dev` → dev window shows as before.
