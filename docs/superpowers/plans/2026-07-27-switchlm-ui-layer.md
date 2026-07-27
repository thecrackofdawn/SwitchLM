# SwitchLM Plan 4 - UI & Tray Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build the management-channel UI on top of the completed data + resilience layer (Plans 1-3): a Tauri system-tray (per-Profile backing switch + inline quota + cooling indicators), the remaining backend management commands (provider/model/profile CRUD, model discovery, connection test, server status/port/env, autostart, quit), graceful shutdown / minimize-to-tray, and a Vue 3 + Naive UI 6-page settings app. Visual/wireframe design is produced via the `frontend-design` skill (deferred to Phase 2).

**Architecture:** Phase 1 (Rust/Tauri) fills the command gap from spec §7.3, wires the tray + autostart plugin + graceful-shutdown, and tracks the actual bound port. Phase 2 (Vue) adds vue-router + Pinia + Naive UI, a design system (via `frontend-design` skill), and the 6 pages consuming the command surface. The tray menu is Rust-defined (native menu items) and reads the same `AppState` as the window.

**Tech Stack:** Tauri v2 (tray, `tauri-plugin-autostart`), Vue 3, vue-router, Pinia, Naive UI. New Rust crate: `tauri-plugin-autostart`. New npm deps: `vue-router`, `pinia`, `naive-ui`.

## Scope of THIS plan

**Phase 1 - backend/tray (do now, no visual dependency):**
- §7.3 command gap: upsert/delete provider/model/profile, `test_provider_connection`, `discover_models`, `get_server_status`/`get_port`/`restart_server`, `get_env_snippet`, `quit_app`, `toggle_autostart`.
- §7.1 tray menu (per-Profile backing switch, inline quota %, cooling marker, open window, autostart toggle, quit).
- §3.6/§7 port-change warning data (`actual_port` tracking, `get_env_snippet`).
- Graceful shutdown: window close -> hide to tray; tray quit -> `app.exit` (in-flight requests keep their snapshot; axum serve stops on drop).
- Capabilities: tray, autostart, window hide permissions.

**Phase 2 - frontend (after `frontend-design` skill installed):**
- vue-router + Pinia + Naive UI + layout shell + IPC bindings to all commands.
- Visual/wireframe design via `frontend-design` skill (design system: colors, type, spacing, components).
- 6 pages (§7.2): Dashboard (status + port warning + copy-env + quick switch), Provider 管理, 真实模型 (Model) 管理 (discover + manual), 自定义模型 (Profile), Fallback 配置, 用量查看. (Settings folded into Dashboard or a 7th page per wireframe.)
- Tray-driven flows validated end-to-end against the Rust tray.

**DEFERRED / out of scope:**
- Real 火山 usage adapter (still Plan 3 follow-up).
- Background usage polling (MVP: on-demand refresh, §11).
- i18n framework (MVP: 中文 UI strings inline; structure allows future i18n).

## Global Constraints

- Built on Plans 1-3 (`master`): `AppState`, all Plan 1-3 commands, `dispatch`, breaker, usage.
- **Tray state = `AppState`**: tray menu and window read/write the same `Arc<AppStateInner>` (§7.4).
- **Port**: `serve()` already returns the actual bound port; Phase 1 stores it in `AppState` so `get_port`/`get_env_snippet`/tray can report it and warn when ≠ `settings.port`.
- **Graceful shutdown**: close window -> hide (tray keeps running); tray "退出" -> `app.exit(0)`. In-flight requests hold their config snapshot (§3.6) - no special drain needed for MVP (axum connections close on server drop).
- **Commits**: conventional, one logical change per commit. `cargo test --manifest-path src-tauri/Cargo.toml` green + `npm run build` (vue-tsc) green before each commit that touches frontend.

## File Structure (Phase 1)

```
src-tauri/
├─ Cargo.toml              (MODIFY: +tauri-plugin-autostart)
├─ tauri.conf.json         (MODIFY: window label "main", tray icon, visibility)
├─ capabilities/default.json (MODIFY: tray + autostart + window permissions)
├─ src/
│  ├─ lib.rs               (MODIFY: register autostart plugin, tray setup, close->hide, actual_port)
│  ├─ commands.rs          (MODIFY: +~13 commands; split if large)
│  ├─ tray.rs              (CREATE: tray menu build + refresh + handlers)
│  └─ proxy/state.rs       (MODIFY: +actual_port: Mutex<Option<u16>>)
```

## File Structure (Phase 2, after skill)

```
src/
├─ main.ts                 (MODIFY: router + pinia + naive-ui providers)
├─ App.vue                 (REPLACE scaffold: layout shell + <router-view>)
├─ router.ts               (CREATE: 6-7 routes)
├─ stores/                 (CREATE: config/usage/health Pinia stores wrapping invoke)
├─ views/                  (CREATE: Dashboard/Provider/Models/Profiles/Fallback/Usage/Settings)
├─ components/             (CREATE: shared per design system)
└─ styles/                 (CREATE: design tokens from frontend-design skill)
```

---

### Task 1 (Phase 1): Infra - autostart plugin, tray capability, actual_port, window config

**Files:** `Cargo.toml`, `tauri.conf.json`, `capabilities/default.json`, `src/proxy/state.rs`, `src/lib.rs`
- Add `tauri-plugin-autostart` dep; register plugin in `lib.rs`.
- `AppStateInner`: add `actual_port: std::sync::Mutex<Option<u16>>`; `serve()` stores the bound port; `server.rs` signature unchanged (caller stores).
- `tauri.conf.json`: window `label: "main"`, `visible: true`, (tray icon ref).
- `capabilities/default.json`: add autostart + tray + `core:window:allow-hide`/`allow-show`/`allow-close` permissions.
- [ ] TDD: unit test that `actual_port` stores/reports; build green.

### Task 2 (Phase 1): Provider/Model/Profile CRUD + discover + connection-test commands

**Files:** `src/commands.rs` (+ split into `commands/config.rs` if large)
- `upsert_provider`/`delete_provider`/`upsert_model`/`delete_model`/`upsert_profile`/`delete_profile` (persist after write; reset breaker on model edit per §4.1).
- `discover_models(provider_id) -> Vec<DiscoveredModel>`: GET `{base}/models` with bearer key; return upstream model ids.
- `test_provider_connection(provider_id) -> Result<bool, String>`: GET `{base}/models` (or a cheap HEAD) - true if 2xx.
- [ ] TDD: `discover_models` against wiremock `/models`; CRUD round-trips via the config store.

### Task 3 (Phase 1): Server/app commands - status, port, restart, env-snippet, quit, autostart

**Files:** `src/commands.rs`, `src/lib.rs`
- `get_server_status() -> { running: bool, port: Option<u16> }`, `get_port() -> Option<u16>` (actual), `restart_server()` (rebind on settings.port), `get_env_snippet() -> { anthropic_base_url, openai_base_url }` (with actual port), `quit_app(app)` (`app.exit(0)`), `toggle_autostart(enabled) -> bool` (via autostart plugin manager + persist `settings.autostart`).
- Register all in `invoke_handler`.
- [ ] TDD: `get_env_snippet` builds correct URLs with actual port; `toggle_autostart` updates settings.

### Task 4 (Phase 1): Tray menu (per-Profile backing switch + inline quota + cooling)

**Files:** `src/tray.rs` (CREATE), `src/lib.rs`
- `build_tray(app, state) -> TrayIcon`: menu = per-Profile submenu (radios of backing Models, checked = current, label includes provider + quota% + cooling marker), separator, "打开主窗口", "开机自启" (checkable), "退出".
- Click handler: backing-model radio -> `set_profile_backing`; "打开主窗口" -> show+focus; "开机自启" -> toggle_autostart; "退出" -> quit_app.
- Menu rebuilt on show (refresh usage/health) - Tauri 2 `on_tray_icon_event` / menu event.
- [ ] TDD: a `tray_menu(state)` pure fn returns the menu structure (Profile -> models -> label) - unit test the label/quota/cooling assembly.

### Task 5 (Phase 1): Graceful shutdown + window hide-on-close

**Files:** `src/lib.rs`
- `WindowEvent::CloseRequested` for "main" -> `api.prevent_close()` + `window.hide()` (minimize to tray, service keeps running).
- Tray "退出" -> `app.exit(0)` (server task dropped; in-flight requests finish their snapshot).
- [ ] Manual: window close hides; tray quit exits.

---

## Phase 2 - Frontend (after `frontend-design` skill installed)

### Task 6: Frontend foundation
- Add deps: `vue-router`, `pinia`, `naive-ui`. `main.ts`: install router/pinia/naive-ui. `App.vue`: layout shell (`<n-layout>` + sidebar nav + `<router-view>`). `router.ts`: routes for the 6-7 pages. `stores/`: Pinia stores wrapping `invoke` for providers/models/profiles/usage/health/server.
- [ ] `npm run build` (vue-tsc) green; nav renders empty pages.

### Task 7: Visual/wireframe design (via `frontend-design` skill)
- Invoke the `frontend-design` skill to produce the design system + wireframes for the 6-7 pages (colors, typography, spacing, component styling, light/dark). Output `styles/` tokens + page wireframes.
- **Gated on the `frontend-design` skill being installed.**

### Tasks 8-13: The pages
- Dashboard, Provider, Models (discover+manual), Profiles, Fallback, Usage (+ Settings) - per wireframes, consuming the Pinia stores/commands. Port-change warning + copy-env button on Dashboard.
- [ ] Each page: CRUD against the commands; `npm run build` green.

### Task 14: Tray/UI integration + DoD
- Tray-driven backing switches reflected in the window (event broadcast or refresh-on-focus); port warning; copy-env. End-to-end manual: switch model via tray -> new requests use it; trigger 429 -> tray shows cooling; quit -> exits.

---

## Definition of Done (Plan 4)

- [ ] `cargo test` green; `npm run build` (vue-tsc) green.
- [ ] All spec §7.3 commands implemented + registered.
- [ ] Tray: per-Profile backing switch (hot-swap, in-flight unaffected), inline quota %, cooling marker, open-window, autostart toggle, quit.
- [ ] Window close -> hide to tray; tray quit -> exit.
- [ ] Port-change warning + copy-env (actual port in `ANTHROPIC_BASE_URL`/`OPENAI_BASE_URL`).
- [ ] 6-7 pages functional: CRUD providers/models/profiles, discover models, fallback config, usage view, dashboard status.
- [ ] E2E manual: real Claude Code -> SwitchLM (running via tray) -> 智谱, one coding task completes; tray switching + cooling observed.

## Hand-off

Plan 4 completes SwitchLM. Post-MVP follow-ups: real 火山 usage adapter, background usage polling, i18n, proactive quota pre-emptive tripping (§11).
