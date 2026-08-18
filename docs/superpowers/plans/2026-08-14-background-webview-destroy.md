# Background Webview Destruction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in setting that destroys the main window's webview render process after the window is hidden to tray for 5 minutes (saving memory), while the proxy + tray keep running, and recreates the window (restoring the last route) when the user reopens it.

**Architecture:** A Rust-side `tokio` timer in a new `idle_destroy.rs` module is armed at the hide-to-tray call sites and cancelled at the show call sites. On fire it calls `WebviewWindow::destroy()`. A `RunEvent::ExitRequested` guard (`code.is_none() → prevent_exit()`) keeps the app alive after the destroy, since Tauri treats destroying the last window as a quit request. The recreate path rebuilds the `"main"` window via `WebviewWindowBuilder` (proven pattern from the existing `qianwen_login.rs`). A single boolean setting wires it; the frontend persists the last route in `localStorage` so a fresh webview lands where the user left off.

**Tech Stack:** Rust (Tauri 2.11.5, tokio async runtime), Vue 3 `<script setup>` + Pinia + Naive UI frontend.

## Global Constraints

- **Tauri version is exactly 2.11.5.** `RunEvent::ExitRequested` carries `code: Option<i32>, api: ExitRequestApi`. `code == None` ⇒ exit from user interaction (window close); `code == Some` ⇒ programmatic `AppHandle::exit`/`restart`. (Tray 退出 and `quit_app` IPC both call `app.exit(0)` ⇒ `Some(0)`.)
- **Fixed 5-minute delay** — constant `IDLE_DESTROY_SECS: u64 = 300`, not configurable.
- **Trigger is hide-to-tray only** — never minimize, never focus loss.
- **Setting defaults to `false`** (opt-in) via `#[serde(default)]`.
- **Logs must identify context in Chinese** (the file is commented in Chinese; match it).
- **Frontend package manager is pnpm** — use `pnpm exec vue-tsc --noEmit` for type-check, never `npm`/`npx`.
- **Backend tests** are co-located `#[cfg(test)] mod tests`; run via `cargo test --manifest-path src-tauri/Cargo.toml`.
- **Cross-platform:** target Windows + Linux. The recreate path uses the same `WebviewWindowBuilder` calls on both.
- **Commit convention:** `type(scope): subject` (e.g. `feat(idle): ...`). Commit only the explicit files listed in each task's `git add`.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src-tauri/src/idle_destroy.rs` | **Create** | The timer holder, `arm`/`cancel`, `summon_main_window`, `recreate_main_window`. Single responsibility: webview lifecycle for memory saving. |
| `src-tauri/src/lib.rs` | Modify | `app.manage(IdleDestroyHandle)`, arm/cancel at call sites, `.build`+`.run` with `ExitRequested` guard, register command. |
| `src-tauri/src/tray.rs` | Modify | `toggle_main_window`: hide→arm, summon→`summon_main_window`. |
| `src-tauri/src/config/types.rs` | Modify | Add `background_destroy: bool` to `Settings` + `Default`; update test literal. |
| `src-tauri/src/config/store.rs` | Modify | Update test literal (add field). |
| `src-tauri/src/commands.rs` | Modify | Add field to `SettingsView` + `get_settings`; new `set_background_destroy` command. |
| `src/lib/types.ts` | Modify | Add field to `Settings` + `SettingsView` interfaces. |
| `src/lib/commands.ts` | Modify | Add `setBackgroundDestroy` wrapper. |
| `src/stores/system.ts` | Modify | Add `saveBackgroundDestroy` action. |
| `src/router.ts` | Modify | `afterEach` persists last route; boot restore. |
| `src/views/Settings.vue` | Modify | New "后台内存优化" `NCard` with `NSwitch`. |

**Task ordering rationale:** backend config first (the data model, fully testable with no window), then the new module, then wire it into `lib.rs`/`tray.rs` (the runtime behavior), then the IPC command, then the frontend vertical slice. Each task compiles and its tests pass independently.

---

### Task 1: Add `background_destroy` setting to the config struct

**Files:**
- Modify: `src-tauri/src/config/types.rs:43-72` (`Settings` struct + `Default`)
- Modify: `src-tauri/src/config/types.rs:323` (`app_config_roundtrips` test literal)
- Modify: `src-tauri/src/config/store.rs:113` (test literal)
- Test: `src-tauri/src/config/types.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `Settings.background_destroy: bool` field (serde key `background_destroy`, defaults `false`). Later tasks read it via `cfg.settings.background_destroy`.

- [ ] **Step 1: Write the failing test**

Add this test to the existing `#[cfg(test)] mod tests` in `src-tauri/src/config/types.rs` (place it just before the closing `}` of the `tests` module, after `app_config_roundtrips`):

```rust
    #[test]
    fn background_destroy_defaults_off() {
        // Opt-in: a fresh Settings (and a config file that omits the field) must default to false.
        assert!eq!(Settings::default().background_destroy, false);

        // A config JSON that predates this feature (no background_destroy key) must deserialize
        // to false via #[serde(default)].
        let json = r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info","request_recording":false}"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.background_destroy, false);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml background_destroy_defaults_off`
Expected: compile error — `no field background_destroy on type Settings`.

- [ ] **Step 3: Add the field to `Settings`**

In `src-tauri/src/config/types.rs`, add this field to the `Settings` struct, immediately after the `request_recording` field (line 59):

```rust
    /// 后台隐藏到托盘 5 分钟后是否销毁 webview 以释放内存（默认关）。代理与托盘不受影响。
    /// 见 docs/superpowers/specs/2026-08-14-background-webview-destroy-design.md。
    #[serde(default)]
    pub background_destroy: bool,
```

Add the field to `impl Default for Settings` (after `request_recording: false,` at line 69):

```rust
            background_destroy: false,
```

- [ ] **Step 4: Update the two test literals that construct `Settings` by name**

In `src-tauri/src/config/types.rs` line 323, change the `settings: Settings { ... }` literal to append the new field:

```rust
            settings: Settings { port: 6950, autostart: false, usage_refresh_interval_secs: 60, log_level: "info".into(), secret_store_fallback: None, request_recording: false, background_destroy: false },
```

In `src-tauri/src/config/store.rs` line 113, change the literal to append the new field:

```rust
            settings: Settings { port: 7000, autostart: true, usage_refresh_interval_secs: 60, log_level: "info".into(), secret_store_fallback: None, request_recording: false, background_destroy: false },
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config`
Expected: PASS — `app_config_roundtrips`, `background_destroy_defaults_off`, and all store tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/config/store.rs
git commit -m "feat(config): add background_destroy setting (default off)"
```

---

### Task 2: Create the `idle_destroy` module — holder + arm/cancel logic

**Files:**
- Create: `src-tauri/src/idle_destroy.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod idle_destroy;` declaration)

**Interfaces:**
- Consumes: `Settings.background_destroy` (Task 1), `AppHandle`, `AppState` (`crate::proxy::AppState`), `tokio::async_runtime`.
- Produces:
  - `pub const IDLE_DESTROY_SECS: u64 = 300;`
  - `pub struct IdleDestroyHandle` (fields private), `impl Default for IdleDestroyHandle`
  - `pub async fn arm_idle_destroy(app: &tauri::AppHandle)`
  - `pub fn cancel_idle_destroy(app: &tauri::AppHandle)`
  - `pub async fn destroy_in_background_mode(app: &tauri::AppHandle) -> bool`

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/idle_destroy.rs` with the test module first. This task covers the holder + arm/cancel state management; the actual `destroy()` needs a live window (covered by manual smoke), so the unit tests cover holder lifecycle only.

```rust
//! 后台 webview 销毁：窗口隐藏到托盘满 IDLE_DESTROY_SECS 后销毁 webview 以释放内存。
//! 代理/托盘不受影响；重新唤起时由 summon_main_window 重建窗口。见
//! docs/superpowers/specs/2026-08-14-background-webview-destroy-design.md。

use std::sync::{Arc, Mutex};
use tauri::Manager;

/// 隐藏到托盘多少秒后销毁 webview（固定 5 分钟，按设计决策不可配置）。
pub const IDLE_DESTROY_SECS: u64 = 300;

/// 持有当前在途的销毁计时任务。arm 时替换；cancel 时置空。经 app.manage 注入。
#[derive(Clone, Default)]
pub struct IdleDestroyHandle {
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl IdleDestroyHandle {
    /// 用新任务替换当前任务：先 abort 旧的，再写入新的。返回旧任务（供测试断言）。
    fn swap(&self, next: Option<tokio::task::JoinHandle<()>>) -> Option<tokio::task::JoinHandle<()>> {
        let mut guard = self.task.lock().expect("idle destroy handle poisoned");
        let prev = guard.take();
        if let Some(h) = &prev {
            h.abort();
        }
        *guard = next;
        prev
    }

    /// 取消当前任务并返回它（供测试断言），不做 abort 之外的清理。
    fn take(&self) -> Option<tokio::task::JoinHandle<()>> {
        let mut guard = self.task.lock().expect("idle destroy handle poisoned");
        let prev = guard.take();
        if let Some(h) = &prev {
            h.abort();
        }
        prev
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        self.task.lock().expect("idle destroy handle poisoned").is_none()
    }
}

/// 读取后台销毁是否开启。
pub async fn destroy_in_background_mode(app: &tauri::AppHandle) -> bool {
    use crate::proxy::AppState;
    match app.try_state::<AppState>() {
        Some(state) => state.config.read().await.settings.background_destroy,
        None => false,
    }
}

/// 当窗口隐藏到托盘时调用：若设置开启且窗口当前隐藏，启动一个 5 分钟计时任务，
/// 到点后再次核对设置与可见性，若仍满足则销毁 webview。已存在计时则先取消再重建（幂等）。
pub async fn arm_idle_destroy(app: &tauri::AppHandle) {
    if !destroy_in_background_mode(app).await {
        return;
    }
    // 仅在窗口确实隐藏时计 armed：可见窗口无内存浪费可省。
    let hidden = match app.get_webview_window("main") {
        Some(w) => !w.is_visible().unwrap_or(false),
        None => true, // 窗口已被销毁——无需再 armed（无可销毁之物）。
    };
    if !hidden {
        return;
    }
    let handle = app.state::<IdleDestroyHandle>();
    let app_cloned = app.clone();
    let next = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(IDLE_DESTROY_SECS)).await;
        // 到点再次核对：用户可能在此期间关闭了开关或重新打开了窗口。
        if !destroy_in_background_mode(&app_cloned).await {
            return;
        }
        if let Some(w) = app_cloned.get_webview_window("main") {
            if !w.is_visible().unwrap_or(false) {
                tracing::info!(
                    "主窗口进入后台 {} 秒，已销毁 webview 以节省内存",
                    IDLE_DESTROY_SECS
                );
                let _ = w.destroy();
            }
        }
    });
    handle.swap(Some(next));
}

/// 当窗口被唤起（或开关关闭）时调用：取消任何在途的销毁计时。幂等。
pub fn cancel_idle_destroy(app: &tauri::AppHandle) {
    if let Some(handle) = app.try_state::<IdleDestroyHandle>() {
        handle.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn holder_starts_idle_and_take_is_noop() {
        let h = IdleDestroyHandle::default();
        assert!(h.is_idle());
        assert!(h.take().is_none()); // 空状态下取消是 no-op，不 panic
        assert!(h.is_idle());
    }

    #[tokio::test]
    async fn swap_replaces_and_aborts_prior_task() {
        let h = IdleDestroyHandle::default();
        let forever = tauri::async_runtime::spawn(async { /* 永不自然结束 */ });
        let prev = h.swap(Some(forever));
        assert!(prev.is_none()); // 首次 swap 之前是空
        assert!(!h.is_idle());

        let forever2 = tauri::async_runtime::spawn(async {});
        let prev2 = h.swap(Some(forever2));
        assert!(prev2.is_some()); // 第二次 swap 返回并 abort 了第一个任务
        assert!(!h.is_idle());

        let prev3 = h.take();
        assert!(prev3.is_some()); // take 返回并 abort 了第二个任务
        assert!(h.is_idle());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (module not yet declared)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: the module compiles standalone but is not yet wired into the crate. We declare it next so it compiles. (If you skip straight to Step 3, run the tests after.)

- [ ] **Step 3: Declare the module**

In `src-tauri/src/lib.rs`, add near the other top-level `mod` declarations (find the existing `mod tray;` / `mod commands;` block and add):

```rust
mod idle_destroy;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml idle_destroy`
Expected: PASS — `holder_starts_idle_and_take_is_noop` and `swap_replaces_and_aborts_prior_task` pass. (The crate must compile; `destroy_in_background_mode`/`arm_idle_destroy` reference `crate::proxy::AppState` and `app.state::<IdleDestroyHandle>()` — the latter is not yet managed, but those functions are not invoked in these unit tests.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/idle_destroy.rs src-tauri/src/lib.rs
git commit -m "feat(idle): add IdleDestroyHandle holder + arm/cancel logic"
```

---

### Task 3: Add `summon_main_window` + `recreate_main_window` to the module

**Files:**
- Modify: `src-tauri/src/idle_destroy.rs` (add two functions)

**Interfaces:**
- Consumes: `cancel_idle_destroy` (Task 2), `tauri::webview::WebviewWindowBuilder`, `WebviewUrl`.
- Produces:
  - `pub async fn summon_main_window(app: &tauri::AppHandle)` — cancels timer, then shows-or-recreates.
  - `fn recreate_main_window(app: &tauri::AppHandle)` — rebuilds the `"main"` window from `tauri.conf.json` attrs.

- [ ] **Step 1: Add the two functions to `idle_destroy.rs`**

Append these to `src-tauri/src/idle_destroy.rs`, immediately before the `#[cfg(test)] mod tests` block:

```rust
/// 用 tauri.conf.json 中声明的 "main" 窗口属性重建窗口（销毁后唤起路径）。
/// 复现 title/尺寸/min 尺寸/visible/dragDrop，与静态声明保持一致。
fn recreate_main_window(app: &tauri::AppHandle) {
    use tauri::webview::{WebviewUrl, WebviewWindowBuilder};
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("SwitchLM")
        .inner_size(960.0, 680.0)
        .min_inner_size(940.0, 600.0)
        .visible(true)
        .drag_drop_enabled(false);
    if let Err(e) = builder.build() {
        // 极少情况：Destroyed 事件尚未处理完，label 仍占用。记录并放弃——
        // 用户下次点击托盘时 get_webview_window 会命中已存在的窗口走 show 分支。
        tracing::warn!("重建主窗口失败（label 可能仍被占用）：{e}");
    }
}

/// 唤起主窗口：先取消任何在途销毁计时；若窗口已被销毁则重建，否则显示并聚焦。
/// 供托盘点击、第二实例启动等所有“显示窗口”路径统一调用。
pub async fn summon_main_window(app: &tauri::AppHandle) {
    cancel_idle_destroy(app);
    match app.get_webview_window("main") {
        None => recreate_main_window(app),
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}
```

- [ ] **Step 2: Verify the crate compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: compiles cleanly. (These functions are not yet called anywhere; the build verifies the `WebviewWindowBuilder` API calls are correct for Tauri 2.11.5. If a builder method name differs, fix it here — the goal is a compiling recreate path.)

- [ ] **Step 3: Run existing module tests still pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml idle_destroy`
Expected: PASS (unchanged — no new tests; recreate needs a live window for manual smoke).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/idle_destroy.rs
git commit -m "feat(idle): add summon/recreate main window path"
```

---

### Task 4: Wire the timer into `lib.rs` call sites + the `ExitRequested` guard

**Files:**
- Modify: `src-tauri/src/lib.rs:84-90` (single-instance block)
- Modify: `src-tauri/src/lib.rs:105-113` (setup show logic)
- Modify: `src-tauri/src/lib.rs:157-177` (manage IdleDestroyHandle)
- Modify: `src-tauri/src/lib.rs:194-202` (on_window_event close handler)
- Modify: `src-tauri/src/lib.rs:259-260` (`.run` → `.build` + `.run` with guard)

**Interfaces:**
- Consumes: `idle_destroy::{IdleDestroyHandle, arm_idle_destroy, cancel_idle_destroy, summon_main_window}` (Tasks 2–3).
- Produces: the timer is armed/cancelled at runtime; the app survives webview destroy via the `ExitRequested` guard.

> **No new unit tests here** — this task changes synchronous app-lifecycle wiring that requires a live Tauri runtime. Correctness is verified by the manual smoke test (Task 10) and by the build. Do not mark the task complete until `cargo build` passes and the manual smoke of Task 10 (done after all backend tasks) confirms behavior.

- [ ] **Step 1: Manage `IdleDestroyHandle` in setup**

In `src-tauri/src/lib.rs`, inside the `.setup(|app| { … })` closure, immediately **before** `app.manage(state);` (line 176), add:

```rust
            app.manage(crate::idle_destroy::IdleDestroyHandle::default());
```

- [ ] **Step 2: Wire the first-launch / autostart show logic**

In `src-tauri/src/lib.rs`, replace the show-logic block at lines 109-113:

```rust
            if is_autostart_launch(std::env::args()) {
                tracing::info!("开机自启启动，主窗口保持隐藏（托盘/代理正常运行）");
                // 自启以隐藏态启动：armed 销毁计时（5 分钟后若无唤起则释放 webview）。
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    crate::idle_destroy::arm_idle_destroy(&app_handle).await;
                });
            } else if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                // 手动启动窗口可见：取消任何（理论上不会有）在途计时。
                crate::idle_destroy::cancel_idle_destroy(app.handle());
            }
```

- [ ] **Step 3: Wire the single-instance block**

In `src-tauri/src/lib.rs`, replace lines 84-90 (the single-instance plugin closure body). Change the inner block from:

```rust
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
```
to:
```rust
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::idle_destroy::summon_main_window(&app).await;
            });
```

- [ ] **Step 4: Wire the close handler (arm on hide-to-tray)**

In `src-tauri/src/lib.rs`, replace the `on_window_event` closure body at lines 194-202:

```rust
        .on_window_event(|window, event| {
            // Close button -> hide to tray (proxy + tray keep running); quit via tray 退出.
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    // 隐藏到托盘后起销毁计时（若开关开启，5 分钟后释放 webview）。
                    let app = window.app_handle().clone();
                    tauri::async_runtime::spawn(async move {
                        crate::idle_destroy::arm_idle_destroy(&app).await;
                    });
                }
            }
        })
```

- [ ] **Step 5: Replace `.run` with `.build` + `.run` carrying the `ExitRequested` guard**

In `src-tauri/src/lib.rs`, the final two lines are currently:

```rust
        .run(tauri::generate_context!())
        .expect("error while running SwitchLM");
```

Replace them with:

```rust
        .build(tauri::generate_context!())
        .expect("error while building SwitchLM")
        .run(|_handle, event| {
            // 销毁最后一个窗口（含后台 webview 销毁）会触发 ExitRequested 且 code == None。
            // 此时必须 prevent_exit，否则代理 + 托盘会随 webview 一起退出。
            // 真正的退出（托盘 退出 / quit_app IPC）走 app.exit(0)，code == Some，放行。
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
```

- [ ] **Step 6: Build to verify the wiring compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: compiles cleanly. Fix any `app_handle()`/`AppHandle` clone borrow issues inline (the closures receive `&AppHandle`/`&Window`; `.clone()` is the correct way to get an owned handle for the spawned task).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(idle): wire arm/cancel at call sites + ExitRequested guard"
```

---

### Task 5: Wire the tray toggle to arm/summon

**Files:**
- Modify: `src-tauri/src/tray.rs:296-312` (`toggle_main_window`)

**Interfaces:**
- Consumes: `crate::idle_destroy::{arm_idle_destroy, summon_main_window}` (Tasks 2–3).
- Produces: tray left-click hide→arms timer; summon→recreates window.

- [ ] **Step 1: Rewrite `toggle_main_window`**

In `src-tauri/src/tray.rs`, replace the `toggle_main_window` function (lines 296-312) with:

```rust
/// Toggle the main window (used by the tray-icon left-click): hide it when it is shown on screen
/// (visible and not minimized); otherwise summon it. Summoning cancels any in-flight idle-destroy
/// timer and rebuilds the window if it was previously destroyed for memory saving.
/// Decides via `toggle_action` (visibility/minimized) rather than `is_focused()`, because the tray
/// click itself steals focus (see `toggle_action`). Query errors default to Summon: never hide.
fn toggle_main_window(app: &AppHandle) {
    // 先尝试读取已有窗口的状态决定 hide 还是 summon；窗口已销毁则直接 summon（会重建）。
    let action = match app.get_webview_window("main") {
        Some(window) => {
            let visible = window.is_visible().ok();
            let minimized = window.is_minimized().ok();
            toggle_action(visible, minimized)
        }
        None => ToggleAction::Summon, // webview 已被后台销毁 -> 重建。
    };
    match action {
        ToggleAction::Hide => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
            // 隐藏到托盘后起销毁计时（若开关开启，5 分钟后释放 webview）。
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::idle_destroy::arm_idle_destroy(&app).await;
            });
        }
        ToggleAction::Summon => {
            // 该回调在主线程同步执行：spawn 到异步运行时执行 async summon_main_window
            // （与 on_menu_event 的退出处理同模式）。summon 会先取消在途销毁计时。
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::idle_destroy::summon_main_window(&app).await;
            });
        }
    }
}
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: compiles cleanly. (`ToggleAction` and `toggle_action` remain unchanged and in scope.)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(idle): tray toggle arms timer on hide, summons (recreates) on show"
```

---

### Task 6: Add the `set_background_destroy` IPC command + `SettingsView` field

**Files:**
- Modify: `src-tauri/src/commands.rs:861-881` (`SettingsView` + `get_settings`)
- Modify: `src-tauri/src/commands.rs` (new `set_background_destroy` command, place after `set_request_recording` ~line 1057)
- Modify: `src-tauri/src/lib.rs:203-258` (register the command in `invoke_handler`)

**Interfaces:**
- Consumes: `Settings.background_destroy` (Task 1), `crate::idle_destroy::{arm_idle_destroy, cancel_idle_destroy}` (Task 2).
- Produces: Tauri command `set_background_destroy(state, app, enabled) -> Result<(), String>`; `SettingsView.background_destroy: bool`.

- [ ] **Step 1: Add the field to `SettingsView` + `get_settings`**

In `src-tauri/src/commands.rs`, add a field to the `SettingsView` struct (after `request_recording`, line 868):

```rust
    pub background_destroy: bool,
```

Update the `get_settings` mapping (add after `request_recording: cfg.settings.request_recording,` at line 879):

```rust
        background_destroy: cfg.settings.background_destroy,
```

Update the doc comment on `SettingsView` (line 861) to:

```rust
/// Read-only view of app settings (port + autostart + usage refresh interval + log level
/// + request recording + background webview destroy).
```

- [ ] **Step 2: Write the `set_background_destroy` command**

In `src-tauri/src/commands.rs`, add this command immediately after the `set_request_recording` function (after line 1057):

```rust
/// 开关后台 webview 销毁：持久化设置 + 立即生效（开启且当前隐藏则起计时；关闭则取消在途计时）。
/// 见 docs/superpowers/specs/2026-08-14-background-webview-destroy-design.md。
#[tauri::command]
pub async fn set_background_destroy(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        config.settings.background_destroy = enabled;
        persist(&app, &config)?;
    }
    if enabled {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::idle_destroy::arm_idle_destroy(&app).await;
        });
    } else {
        crate::idle_destroy::cancel_idle_destroy(&app);
    }
    Ok(())
}
```

- [ ] **Step 3: Register the command**

In `src-tauri/src/lib.rs`, add to the `invoke_handler` list (after `commands::set_request_recording,` at line 256):

```rust
            commands::set_background_destroy,
```

- [ ] **Step 4: Build + run all backend tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests PASS, crate compiles. (The command itself isn't unit-tested in isolation — it needs a managed `AppState` + `IdleDestroyHandle`, exercised in the manual smoke. The build + existing tests verify no regression.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): add set_background_destroy command + SettingsView field"
```

---

### Task 7: Frontend types + IPC wrapper + Pinia action

**Files:**
- Modify: `src/lib/types.ts:75-81` (`Settings` interface) and `:144-150` (`SettingsView` interface)
- Modify: `src/lib/commands.ts` (add `setBackgroundDestroy`)
- Modify: `src/stores/system.ts:70-73` (add `saveBackgroundDestroy`) and `:97-118` (export it)

**Interfaces:**
- Consumes: backend command `set_background_destroy` (Task 6).
- Produces: `api.setBackgroundDestroy(enabled)`, `system.saveBackgroundDestroy(enabled)`, `system.settings.background_destroy`.

- [ ] **Step 1: Add the field to both TS interfaces**

In `src/lib/types.ts`, add to the `Settings` interface (after `request_recording: boolean;`):

```ts
  background_destroy: boolean;
```

Add the same line to the `SettingsView` interface (after its `request_recording: boolean;`).

- [ ] **Step 2: Add the IPC wrapper**

In `src/lib/commands.ts`, add immediately after the `setRequestRecording` export (around line 137):

```ts
export const setBackgroundDestroy = (enabled: boolean) =>
  invoke<void>("set_background_destroy", { enabled });
```

- [ ] **Step 3: Add the Pinia action + export**

In `src/stores/system.ts`, add after `saveRequestRecording` (after line 73):

```ts
  async function saveBackgroundDestroy(enabled: boolean) {
    await api.setBackgroundDestroy(enabled);
    await loadSettings();
  }
```

Add `saveBackgroundDestroy,` to the returned object (after `saveRequestRecording,` around line 112).

- [ ] **Step 4: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts
git commit -m "feat(frontend): add background_destroy type + IPC + store action"
```

---

### Task 8: Settings UI card

**Files:**
- Modify: `src/views/Settings.vue` (template: new `NCard`; script: new `toggleBackgroundDestroy` handler)

**Interfaces:**
- Consumes: `system.saveBackgroundDestroy` (Task 7), `system.settings.background_destroy`.

- [ ] **Step 1: Add the handler in the `<script setup>`**

In `src/views/Settings.vue`, add this handler near the other `toggle*`/`save*` functions (e.g. after `toggleRecording`):

```ts
async function toggleBackgroundDestroy(on: boolean) {
  try {
    await system.saveBackgroundDestroy(on);
    msg.success(on ? "已开启后台内存优化" : "已关闭后台内存优化");
  } catch (e) {
    msg.error(`设置失败：${String(e)}`);
  }
}
```

- [ ] **Step 2: Add the card to the template**

In `src/views/Settings.vue`, add this card immediately after the 请求记录 card (after line 205, before the 服务 card):

```vue
    <NCard title="后台内存优化" size="small">
      <NSpace align="center" :size="12">
        <NSwitch
          :value="system.settings?.background_destroy ?? false"
          @update:value="(v: boolean) => toggleBackgroundDestroy(v)"
        />
        <span class="muted">窗口隐藏到托盘 5 分钟后自动释放界面进程以节省内存，代理与托盘不受影响</span>
      </NSpace>
    </NCard>
```

- [ ] **Step 3: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/views/Settings.vue
git commit -m "feat(frontend): add background memory optimization toggle card"
```

---

### Task 9: Last-route persistence so a recreated window lands where the user left off

**Files:**
- Modify: `src/router.ts:44-47` (add `afterEach` persistence + boot restore)

**Interfaces:**
- Consumes: `localStorage`, the `router` instance.
- Produces: on every navigation the path is saved to `localStorage["switchlm:lastRoute"]`; on boot the SPA restores it.

- [ ] **Step 1: Add persistence + restore to `router.ts`**

In `src/router.ts`, replace the bottom block (lines 44-47):

```ts
export const router = createRouter({
  history: createWebHashHistory(),
  routes,
});
```

with:

```ts
export const router = createRouter({
  history: createWebHashHistory(),
  routes,
});

const LAST_ROUTE_KEY = "switchlm:lastRoute";

// 记录最后访问的路由：webview 被后台销毁后重建时，SPA 重新加载，据此回到用户离开的页面。
router.afterEach((to) => {
  try {
    localStorage.setItem(LAST_ROUTE_KEY, to.path);
  } catch {
    // localStorage 不可用时静默放弃（不影响核心功能）。
  }
});

// 启动时恢复上次路由（仅当存在且能解析到真实路由，否则保持默认 /dashboard）。
export function restoreLastRoute() {
  try {
    const last = localStorage.getItem(LAST_ROUTE_KEY);
    if (last && router.resolve(last).matched.length > 0) {
      void router.replace(last);
    }
  } catch {
    // 忽略：默认停在 /dashboard。
  }
}
```

- [ ] **Step 2: Call `restoreLastRoute()` at boot**

`src/main.ts` currently ends with `createApp(App).use(createPinia()).use(router).mount("#app");`. Add the import at the top and the restore call after the mount. The resulting relevant lines of `src/main.ts`:

```ts
import { createApp } from "vue";
// ...existing imports...
import { restoreLastRoute } from "./router";

createApp(App).use(createPinia()).use(router).mount("#app");
restoreLastRoute();
```

- [ ] **Step 3: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/router.ts src/main.ts
git commit -m "feat(frontend): persist + restore last route across webview recreate"
```

---

### Task 10: Manual smoke test + full build verification

**Files:** none (verification only)

This task validates the runtime behavior that unit tests cannot (real window lifecycle, real memory, real tray). It is the gate for Tasks 4–6.

- [ ] **Step 1: Full backend + frontend build**

Run: `cargo build --manifest-path src-tauri/Cargo.toml && pnpm build`
Expected: both succeed. `pnpm build` runs `vue-tsc --noEmit` + vite build.

- [ ] **Step 2: Launch dev mode**

Run: `pnpm tauri dev`
Expected: app boots, window appears, proxy + tray start. Open Settings → the new "后台内存优化" card is visible, default OFF.

- [ ] **Step 3: Toggle ON → hide → wait → confirm destroy**

In Settings, toggle 后台内存优化 ON. Close the window (hide to tray). Note the dev process memory in Task Manager (Windows) / `top` (Linux). Wait 5 minutes. Expected:
- A log line appears: `主窗口进入后台 300 秒，已销毁 webview 以节省内存`.
- The webview render process memory is freed (process count or RSS drops noticeably).
- The tray icon remains; the proxy still serves (verify by sending a test request to `http://localhost:6950`).

- [ ] **Step 4: Recreate + last route**

Click the tray icon. Expected: the window reappears and lands on the **last route** you were on (Settings), not Dashboard. Toggle the setting off and on again — no errors.

- [ ] **Step 5: OFF behavior (no destroy)**

Toggle the setting OFF. Hide to tray. Wait 6 minutes. Expected: **no** destroy log line; window still resident (reopens instantly via tray, no recreate flash).

- [ ] **Step 6: Real quit still works**

Click tray → 退出. Expected: the **entire** app quits (proxy + tray + window all exit). This confirms the `ExitRequested` guard lets `code == Some(0)` through.

- [ ] **Step 7: Autostart hidden boot (if feasible)**

If you can simulate autostart: run the built binary with `--autostart`. Expected: window stays hidden on boot; after 5 min the destroy log appears with no manual hide. If autostart can't be easily simulated, note this as deferred-manual and rely on the code review of Task 4 Step 2.

- [ ] **Step 8: Document results**

Append a short note to the plan (or the spec) recording the observed memory before/after and confirming each smoke step. If any step fails, file it as a follow-up before merging.

---

## Self-Review

**1. Spec coverage** (spec section → task):
- §6.1 trigger model / 6 call sites → Task 4 (sites 1,4,5,6) + Task 5 (sites 2,3). ✓
- §6.2 sync-handler constraint (handler never reads config) → Task 4 Step 4 (handler always hides + arms; the task re-checks). ✓
- §6.3 ExitRequested guard → Task 4 Step 5. ✓
- §7.1 `idle_destroy.rs` (holder, arm, cancel, summon, recreate, const) → Tasks 2 + 3. ✓
- §7.2 `lib.rs` manage + build/run + show logic → Task 4. ✓
- §7.3 `tray.rs` toggle → Task 5. ✓
- §7.4 `config/types.rs` field + default + test → Task 1. ✓
- §7.5 `commands.rs` SettingsView + set command + register → Task 6. ✓
- §7.6 frontend types/commands/store/UI + last-route → Tasks 7, 8, 9. ✓
- §8 edge cases 1–11 → handled by design: #1 set command live-effect (Task 6); #2/#4 summon cancels (Tasks 3,5); #3 swap aborts prior (Task 2); #5 guard (Task 4); #6 is_none() gate (Task 3); #7 autostart arm (Task 4 Step 2); #8 resolve().matched guard (Task 9); #9/#10/#11 are inherent/default-off. ✓
- §9 testing → Tasks 1,2 unit tests + Task 10 manual smoke. ✓

**2. Placeholder scan:** None. Every code step contains actual code. Task 9 Step 2's bootstrap location is pinned to `src/main.ts` (confirmed: it ends with `createApp(App).use(createPinia()).use(router).mount("#app")`). Task 10 Step 7 has an "if feasible" because autostart simulation is environment-dependent — documented, not deferred.

**3. Type/name consistency:**
- `IdleDestroyHandle` defined Task 2, used Task 4 (`app.manage(IdleDestroyHandle::default())`), Task 6 (`cancel_idle_destroy(&app)` via `try_state`). ✓
- `arm_idle_destroy` / `cancel_idle_destroy` / `summon_main_window` signatures consistent across Tasks 2/3/4/5/6. ✓
- `background_destroy` field name identical in Rust (Task 1), `SettingsView` (Task 6), TS (Task 7), store (Task 7), Vue (Task 8). ✓
- `set_background_destroy` command name consistent Rust (Task 6) ↔ TS invoke (Task 7). ✓
- `saveBackgroundDestroy` store action (Task 7) ↔ Vue handler (Task 8). ✓
- `IDLE_DESTROY_SECS` defined once (Task 2), referenced in log string (Task 2) and manual smoke (Task 10). ✓
