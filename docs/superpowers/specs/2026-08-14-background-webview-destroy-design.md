# Background Webview Destruction (memory saving)

**Date:** 2026-08-14
**Status:** Design — pending implementation plan
**Branch:** `mem_opt`

## 1. Problem

When SwitchLM is minimized to the tray, the close handler calls `prevent_close()` + `window.hide()`. This keeps the webview render process (WebView2 on Windows, WebKitGTK on Linux) **resident in memory** — typically hundreds of MB — even though the UI is invisible and the proxy + tray are doing all the real work. For a long-running tray proxy, this is wasteful.

## 2. Goal

Add an opt-in setting that, after the main window is **hidden to tray** for a fixed **5 minutes**, destroys the webview render process to reclaim its memory — while the proxy, tray, and all background behavior keep running untouched. Reopening the window recreates it and restores the user's last view.

## 3. Decisions (locked in brainstorming)

| Decision | Choice |
|---|---|
| What counts as "background" | **Hidden to tray only.** A visible-but-unfocused window is left alone. Minimized is *not* a trigger. |
| How memory is freed | **Real `destroy()`** of the window (tears down the OS window + WebView2/WebKit render process), then **recreate** on demand. Not a plain `hide()`. |
| Config scope | **Single boolean toggle**, fixed 5-minute delay. No configurable duration. |
| On recreate, land where? | **Restore the last route** the user was on before destroy. |
| Countdown visibility | **Silent** — internal countdown only; no toast / live indicator. |

## 4. Background — current window lifecycle

Relevant facts established from the codebase (Tauri v2 **2.11.5**):

- Main window declared statically in `src-tauri/tauri.conf.json` (`label: "main"`, `visible: false`). No `WindowBuilder` in Rust; created from config, obtained later via `app.get_webview_window("main")`.
- `src-tauri/src/lib.rs:194-202` — global `on_window_event`: on `CloseRequested` for `"main"`, `api.prevent_close()` + `window.hide()`. The webview is therefore kept alive across hide-to-tray.
- `src-tauri/src/tray.rs:296-312` — `toggle_main_window`: tray left-click hides (shown) or summons (hidden/minimized) the window. `get_webview_window("main")` returning `None` causes an early `return` — there is **no recreate path**.
- `src-tauri/src/lib.rs:105-113` — `setup` show logic: non-autostart launches `w.show()`; autostart (`--autostart`) leaves the window hidden (tray/proxy still run).
- `src-tauri/src/lib.rs:84-90` — single-instance plugin: 2nd launch shows + focuses the main window.
- `src-tauri/src/tray.rs:328-346` — tray menu "quit" → spawns `app.exit(0)`. `src-tauri/src/commands.rs:796-801` — `quit_app` IPC → `app.exit(0)`. These are the only real-exit paths.
- Settings: `Settings` struct in `src-tauri/src/config/types.rs:43-72` (every field `#[serde(default)]`), persisted to `<app_data>/app_config.json` via `config/store.rs`. Dedicated `set_*` commands in `commands.rs`; `set_request_recording` (`commands.rs:1037`) is the template for a toggle with a **live side effect**. Frontend `system` store uses mutate-then-`loadSettings()`; `Settings.vue` renders toggles via `NSwitch` (instant-commit).
- **There is no `WindowEvent` for visibility change**, and **no focus/visibility event handling exists** today. The timer must be driven from the hide/show *call sites*.
- The Vue app uses **no** `listen()` event subscriptions (the only `getCurrentWindow()` use is a one-time resize in `src/lib/fitRouteWindow.ts`).

## 5. Verified Tauri 2.11.5 behavior (authoritative — traced through source)

- `WebviewWindow::destroy()` → drops the OS window + the WebView2/WebKit render process. **Frees the webview memory.** `hide()` does not. `close()` fires `CloseRequested` first (cancellable); `destroy()` force-closes with no event.
- **Label reuse works**: after the `Destroyed` event is processed, the `"main"` label is freed from the runtime's `windows_lock()` registry and `WebviewWindowBuilder` rebuilds it. Empirically proven in this codebase by `src-tauri/src/qianwen_login.rs` (destroy at `:166`, recreate same-label at `:223` guarded by `is_none()` at `:205`/`:284`).
- **Registry cleanup is asynchronous**: the `Destroyed` event is pumped on the event loop, not synchronously when `destroy()` returns. **Never rebuild in the same tick as destroy** — gate rebuild on `get_webview_window("main").is_none()`.
- **Destroying the last window emits `RunEvent::ExitRequested`** (not `CloseRequested`). With `code == None`, this is the "last window gone" signal. Without handling it, destroying the webview would quit the **entire app** (proxy + tray). Today this never fires because the app only ever *hides*.
- Statically-declared (`tauri.conf.json`) windows are created via the same `WebviewWindowBuilder` path at startup; the static declaration is not a reservation. Runtime rebuild reproducing the config attributes is supported.
- **JS-listener leak (`tauri#15583`):** destroying does not purge JS event listeners. **Not applicable to SwitchLM** — the frontend has no `listen()` subscriptions.
- **Linux WebKit:** destroying the webview frees page/render memory but WebKit keeps the network process alive for the app lifetime (wry design, `tauri#14626`); recreate reuses it. Platform reality, not a leak.

## 6. Architecture

### 6.1 Trigger model (no visibility event — drive from call sites)

The timer is armed/cancelled at the existing hide/show call sites. There are **6**:

| # | Site | File | Today | With feature |
|---|---|---|---|---|
| 1 | Close button | `lib.rs:197` | `prevent_close + hide` | unchanged, then **arm** |
| 2 | Tray click → hide | `tray.rs:304` | `hide` | then **arm** |
| 3 | Tray click → summon | `tray.rs:306` | `show + focus` | **cancel** + recreate-if-None |
| 4 | 2nd-instance launch | `lib.rs:86` | `show + focus` | **cancel** + recreate-if-None |
| 5 | First launch (non-autostart) | `lib.rs:111` | `show` | **cancel** (no arm) |
| 6 | Autostart boot (hidden) | `lib.rs:109` | stays hidden | **arm** once |

Sites #3/#4 (and the show in #5) consolidate into a single `summon_main_window(app)` helper; sites #1/#2/#6 share `arm_idle_destroy(app)`.

### 6.2 The synchronous-handler constraint

`on_window_event` runs **synchronously** and cannot `.await` the `RwLock<AppConfig>`. Therefore the close handler **never reads config** — it always `prevent_close + hide`, then arms the timer unconditionally. The armed task (async) reads the setting **at fire time** and aborts itself if the feature was disabled during the wait. This sidesteps the sync-config-read problem entirely and is self-correcting.

### 6.3 The `ExitRequested` guard (make-or-break)

Switch `.run(context)` → `.build(context)?` + `app.run(|handle, event| …)`. The `RunEvent::ExitRequested` variant carries `code: Option<i32>` and `api: ExitRequestApi`. Handle:

```rust
tauri::RunEvent::ExitRequested { code, api, .. } => {
    // Per Tauri 2.11.5 docs: code == None  => exit requested by user interaction
    //                          (window close/X-click we let through after destroy).
    //                        code == Some  => programmatic AppHandle::exit / ::restart
    //                          (tray 退出 -> app.exit(0); quit_app IPC -> app.exit(0)).
    // Keep the app (proxy + tray) alive for the None case; let explicit quits through.
    if code.is_none() {
        api.prevent_exit();
    }
}
_ => {}
```

Without this, the timer firing would kill the proxy. This is the single most important correctness requirement and is explicitly tested.

## 7. Components

### 7.1 New: `src-tauri/src/idle_destroy.rs` (~80 lines)

Owns the timer and recreate logic; single responsibility; testable in isolation.

- `pub const IDLE_DESTROY_SECS: u64 = 300;` (5 min, fixed).
- `pub struct IdleDestroyHandle { task: Arc<Mutex<Option<JoinHandle<()>>>> }` — managed via `app.manage()`, cheaply cloned to call sites. `Default` impl → `None`.
- `pub async fn arm_idle_destroy(app: &AppHandle)` — reads config; if `background_destroy == false` OR window visible → return. Else: lock, `abort()` any prior task, spawn a task that `tokio::time::sleep(300s)`, then **re-checks** `background_destroy` + `is_visible()` on wake (guards against toggle-off or re-show mid-wait), and calls `window.destroy()`. Logs `info!("主窗口进入后台 {} 秒，已销毁 webview 以节省内存", IDLE_DESTROY_SECS)` on destroy.
- `pub fn cancel_idle_destroy(app: &AppHandle)` — lock, `abort()`, set `None`. Idempotent; safe if no task.
- `pub async fn summon_main_window(app: &AppHandle)` — `cancel_idle_destroy(app)`. If `get_webview_window("main").is_none()` → `recreate_main_window(app)`; else `show + unminimize + set_focus`. Always `set_focus`.
- `fn recreate_main_window(app: &AppHandle)` — `WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))` reproducing `tauri.conf.json` attrs (title `SwitchLM`, width 960, height 680, minWidth 940, minHeight 600, visible true, dragDropEnabled false), then `show + set_focus`.
- `pub async fn destroy_in_background_mode(app: &AppHandle) -> bool` — helper reading the setting bool; used by `arm_idle_destroy` and `summon_main_window`.

### 7.2 Modified: `src-tauri/src/lib.rs`

- `on_window_event` close handler (`:194-202`): keep `prevent_close + hide`, then spawn `idle_destroy::arm_idle_destroy(app)` (clone `AppHandle` into the task).
- `.run(generate_context!())` → `.build(generate_context!())?` + `.run(|_handle, event| match event { RunEvent::ExitRequested { code, api, .. } => { if code.is_none() { api.prevent_exit() } } _ => {} })`. (Exact `RunEvent` shape confirmed in Tauri 2.11.5 `src/app.rs`: `ExitRequested { code: Option<i32>, api: ExitRequestApi }`; the `code.is_none()` guard is the contract described in §6.3.)
- `setup`: `app.manage(idle_destroy::IdleDestroyHandle::default())`. For non-autostart first launch (`:111`): `cancel_idle_destroy` (harmless; window shown). For autostart (`:109`): spawn `arm_idle_destroy` (boots hidden → eligible).
- Single-instance block (`:84-90`): replace `show + set_focus` with `idle_destroy::summon_main_window(app).await` (spawned onto the async runtime).
- `invoke_handler` (`:203`): register `commands::set_background_destroy`.

### 7.3 Modified: `src-tauri/src/tray.rs`

- `toggle_main_window` (`:296`): Hide branch keeps `hide()` then spawns `arm_idle_destroy(app)`. Summon branch spawns `idle_destroy::summon_main_window(app)` onto the async runtime (the tray click handler runs synchronously on the main thread, so the `async fn` is spawned, not awaited — same pattern as the existing `on_menu_event` quit at `tray.rs:340`). The early `return` on `None` is removed — `summon_main_window`'s `is_none()` check supersedes it.

### 7.4 Modified: `src-tauri/src/config/types.rs`

- `Settings` adds `#[serde(default)] pub background_destroy: bool` (defaults `false` → opt-in). Add to `impl Default`.
- Update the `app_config_roundtrips` test literal (`:323`) with `background_destroy: false`.

### 7.5 Modified: `src-tauri/src/commands.rs`

- Add `background_destroy: bool` to `SettingsView` (`:863`) and to the `get_settings` mapping (`:874`).
- New `#[tauri::command] async fn set_background_destroy(state: State<'_, AppState>, app: AppHandle, enabled: bool) -> Result<(), String>`: validate (bool, always valid), write-lock `cfg.settings.background_destroy = enabled`, `persist`, then live-apply: if `enabled` → spawn `arm_idle_destroy(app)`, else `cancel_idle_destroy(app)`. Follows the `set_request_recording` shape.

### 7.6 Modified: frontend

- `src/lib/types.ts` — add `background_destroy: boolean` to **both** the `Settings` and `SettingsView` interfaces.
- `src/lib/commands.ts` — add `export const setBackgroundDestroy = (enabled: boolean) => invoke<void>("set_background_destroy", { enabled });`.
- `src/stores/system.ts` — add `async function saveBackgroundDestroy(enabled: boolean) { await api.setBackgroundDestroy(enabled); await loadSettings(); }`, export it.
- `src/views/Settings.vue` — one new `NCard` titled **"后台内存优化"** with an instant-commit `NSwitch` (clone the autostart card pattern), muted explainer: `"窗口隐藏到托盘 5 分钟后自动释放界面进程以节省内存，代理与托盘不受影响"`.
- **Last-route persistence** — in `src/router.ts`: `router.afterEach((to) => localStorage.setItem("switchlm:lastRoute", to.path))`; on boot, read `localStorage["switchlm:lastRoute"]` and, if present and resolvable, `router.replace()` to it (else Dashboard). Persist only the path string; validate on read.

## 8. Edge cases & error handling

1. **Toggle flipped mid-countdown.** `set_background_destroy(off)` → `cancel_idle_destroy` aborts the task. The task's own on-wake re-check is the backstop. No destroy when off.
2. **Window re-shown before 5 min (hot path).** `summon_main_window` → `cancel_idle_destroy` → no destroy. Must be fast and never rebuild unnecessarily.
3. **Re-arm while already counting.** `arm_idle_destroy` aborts the prior handle first → idempotent, no task accumulation, no double-fire.
4. **Destroy fires while user interacting.** Cannot happen — trigger requires a hidden window, which is non-interactive. If reopened at 4:59, cancel (synchronous) wins over the task's wake-check.
5. **`ExitRequested` from destroy (proxy must survive).** Guard `code.is_none() → prevent_exit()`. Explicit quits (`code == Some(0)` from tray 退出 / `quit_app`) pass through. Explicitly tested.
6. **Recreate race (registry cleanup async).** `summon_main_window` gates on `get_webview_window("main").is_none()`; if `Destroyed` not yet processed, window is `Some` → just `show()` (correct). Proven pattern from `qianwen_login.rs`.
7. **Autostart hidden-from-boot.** No hide event occurs; `setup` explicitly `arm`s for autostart launches so the timer starts from boot. Non-autostart shows the window → no arm.
8. **Stale last-route.** Persist only the path; on read, if the router cannot resolve it, fall back to Dashboard (existing catch-all/guard). Never `replace` blindly.
9. **Linux WebKit network process.** Destroy frees page/render memory; network process persists (wry design). Recreate reuses it. Documented, not a leak.
10. **JS-listener leak (`#15583`).** Not applicable — frontend has no `listen()` subscriptions. `localStorage` last-route is window-instance-agnostic.
11. **Default off (opt-in).** Existing users: field absent → `#[serde(default)]` → `false` → behavior identical to today. Zero migration risk.

## 9. Testing

### 9.1 Backend unit tests (co-located `#[cfg(test)]`)

- `types.rs`: update `app_config_roundtrips` literal; add `background_destroy_defaults_off`.
- `idle_destroy.rs`: `arm_then_cancel_is_noop` (arm → cancel → handle is `None`), `arm_idempotent` (double-arm → single live task), `disabled_setting_never_fires` (setting off → `arm` is a no-op / returns without spawning).
- `commands.rs`: `set_background_destroy` round-trip (set true → `get_settings` reflects → persist survives reload).
- The actual `destroy()`/`recreate()` require a live window/runtime — covered by the manual smoke below (consistent with how the other window commands are validated).

### 9.2 Manual smoke (`pnpm tauri dev`)

1. Toggle on → hide to tray → wait 5 min → confirm process memory drops (Task Manager), tray live, proxy still serving.
2. Click tray → window returns, lands on last route.
3. Toggle off → hide → wait 6 min → window still resident (no destroy).
4. Tray 退出 → whole app quits (`ExitRequested` guard lets `Some(0)` through).
5. Autostart hidden boot (`--autostart`) → destroyed after 5 min with no manual hide.

## 10. Out of scope

- Configurable delay duration (fixed 5 min by decision).
- Destroy on minimize (trigger is hide-to-tray only) or on focus loss.
- Visible countdown / toasts.
- Any change to proxy, tray menu, fallback, or usage behavior.
