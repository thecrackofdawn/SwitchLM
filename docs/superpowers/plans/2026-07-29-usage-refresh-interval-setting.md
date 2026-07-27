# Usage Refresh Interval Setting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Overview/Usage pages' usage auto-refresh interval user-configurable in Settings (default 60s, min 30s, max 3600s), with the 30s floor enforced at the code layer so a hand-edited `app_config.json` cannot lower it.

**Architecture:** Add a persisted `usage_refresh_interval_secs` field to the backend `Settings` struct (serde-defaulted for backward compat). The backend clamps it to `[30, 3600]` both on save (reject out-of-range) and on read (`get_settings` projection) — the read clamp is the tamper guard. The frontend reads the clamped value from the store and uses it as the `usePolling` interval, re-clamping at execution time. The backend usage-cache TTL stays fixed at 60s (decoupled — it is the real upstream-pressure limiter, not the poll cadence).

**Tech Stack:** Rust + Tauri v2 (backend), Vue 3 + Pinia + naive-ui + vue-tsc (frontend).

## Global Constraints

- **Bounds (seconds):** min `30`, default `60`, max `3600`. Verbatim on both layers.
- **Unit:** seconds (`u32` in Rust, `number` in TS). The polling timer multiplies by 1000 for ms.
- **Serde:** snake_case field names, **no** `rename_all` (matches the rest of `config/types.rs`). New persisted field uses `#[serde(default = "fn")]` so old config files load at 60.
- **Tauri arg conversion:** camelCase JS arg keys → snake_case Rust params. The new command uses a single-word param `seconds`, so the JS key is `seconds` on both sides (no conversion ambiguity).
- **Cache TTL (`DEFAULT_USAGE_TTL_SECS = 60`) is NOT changed** by this plan.
- **No new dependencies.** Frontend has no unit-test runner (no vitest/jest in `package.json`); frontend tasks verify via `npx vue-tsc --noEmit` + manual check. Backend tasks use `cargo test`.
- **Commit only when the plan's task says to commit.** Each task ends with one conventional-commit commit.

---

## File Structure

| File | Responsibility | Action |
|------|----------------|--------|
| `src-tauri/src/config/types.rs` | Persisted `Settings` model + bounds constants + pure `clamp_usage_refresh_secs` helper + unit tests | Modify |
| `src-tauri/src/commands.rs` | `SettingsView` DTO (add field), `get_settings` (clamp on read), new `set_usage_refresh_interval` command | Modify |
| `src-tauri/src/lib.rs` | Register the new command in `generate_handler!` | Modify |
| `src/lib/types.ts` | Mirror field into `Settings` + `SettingsView`; export mirrored bounds constants | Modify |
| `src/lib/commands.ts` | Typed `setUsageRefreshInterval` invoke wrapper | Modify |
| `src/stores/system.ts` | `saveUsageRefreshInterval` action (set then reload) | Modify |
| `src/lib/usePolling.ts` | Accept `number \| (() => number)`; re-arm on change | Modify |
| `src/views/Settings.vue` | "用量刷新间隔" card (NInputNumber + 保存) | Modify |
| `src/views/Usage.vue` | Poll at configured interval; load settings; dynamic hint text | Modify |
| `src/views/Dashboard.vue` | Poll at configured interval | Modify |

---

### Task 1: Backend persisted model + bounds + clamp helper (TDD)

**Files:**
- Modify: `src-tauri/src/config/types.rs` (struct `Settings` lines 26-40; `tests` mod lines 105-144)

**Interfaces:**
- Produces (consumed by Task 2): `pub const MIN_USAGE_REFRESH_SECS: u32 = 30`, `pub const DEFAULT_USAGE_REFRESH_SECS: u32 = 60`, `pub const MAX_USAGE_REFRESH_SECS: u32 = 3600`, field `Settings.usage_refresh_interval_secs: u32`, and `pub fn clamp_usage_refresh_secs(secs: u32) -> u32`. All re-exported by `config/mod.rs` (`pub use types::*`), so they are reachable as `crate::config::clamp_usage_refresh_secs` etc.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/config/types.rs`, inside the existing `#[cfg(test)] mod tests` block (after `app_config_roundtrips`), add two tests. Also update the `app_config_roundtrips` struct literal + assertion for the new field.

Update the `app_config_roundtrips` settings construction (currently line 134) from:
```rust
            settings: Settings { port: 6950, autostart: false },
```
to:
```rust
            settings: Settings { port: 6950, autostart: false, usage_refresh_interval_secs: 60 },
```
And add inside `app_config_roundtrips` (after the `assert_eq!(back.settings.port, 6950);` line):
```rust
        assert_eq!(back.settings.usage_refresh_interval_secs, 60);
```

Then append these two tests to the `tests` mod:
```rust
    #[test]
    fn clamp_usage_refresh_secs_enforces_range() {
        assert_eq!(clamp_usage_refresh_secs(0), 30);
        assert_eq!(clamp_usage_refresh_secs(5), 30);
        assert_eq!(clamp_usage_refresh_secs(29), 30);
        assert_eq!(clamp_usage_refresh_secs(30), 30);
        assert_eq!(clamp_usage_refresh_secs(60), 60);
        assert_eq!(clamp_usage_refresh_secs(120), 120);
        assert_eq!(clamp_usage_refresh_secs(3600), 3600);
        assert_eq!(clamp_usage_refresh_secs(99999), 3600);
    }

    #[test]
    fn settings_default_when_field_absent() {
        // Old config files (pre-feature) omit usage_refresh_interval_secs;
        // serde default must fill 60 so existing installs keep working.
        let json = r#"{"port": 7000, "autostart": true}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.usage_refresh_interval_secs, DEFAULT_USAGE_REFRESH_SECS);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml clamp_usage_refresh_secs settings_default_when_field_absent app_config_roundtrips`
Expected: FAIL (compile errors: `clamp_usage_refresh_secs` not found, `usage_refresh_interval_secs` field missing on `Settings`).

- [ ] **Step 3: Implement the model, constants, and clamp helper**

In `src-tauri/src/config/types.rs`, add the constants just above the `Settings` struct (before line 26) and extend the struct + `Default`:

```rust
pub const MIN_USAGE_REFRESH_SECS: u32 = 30;
pub const DEFAULT_USAGE_REFRESH_SECS: u32 = 60;
pub const MAX_USAGE_REFRESH_SECS: u32 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_usage_refresh_interval_secs")]
    pub usage_refresh_interval_secs: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            port: default_port(),
            autostart: false,
            usage_refresh_interval_secs: DEFAULT_USAGE_REFRESH_SECS,
        }
    }
}
fn default_port() -> u16 {
    6950
}
fn default_usage_refresh_interval_secs() -> u32 {
    DEFAULT_USAGE_REFRESH_SECS
}

/// Clamp a usage-refresh interval (seconds) into the allowed [30, 3600] range.
/// Applied on read (`get_settings`) so a hand-edited config file can never drive
/// polling below the floor.
pub fn clamp_usage_refresh_secs(secs: u32) -> u32 {
    secs.clamp(MIN_USAGE_REFRESH_SECS, MAX_USAGE_REFRESH_SECS)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml clamp_usage_refresh_secs settings_default_when_field_absent app_config_roundtrips`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs
git commit -m "feat(usage): add usage_refresh_interval_secs to Settings + clamp helper"
```

---

### Task 2: Backend get_settings clamp + set command + registration

**Files:**
- Modify: `src-tauri/src/commands.rs` (import line 7; `SettingsView` lines 514-519; `get_settings` lines 521-525; new command near `set_port` ~578)
- Modify: `src-tauri/src/lib.rs` (`generate_handler!` line 123)

**Interfaces:**
- Consumes: `crate::config::{clamp_usage_refresh_secs, MIN_USAGE_REFRESH_SECS, MAX_USAGE_REFRESH_SECS}` (from Task 1), `persist(app, &cfg)` helper (existing, `commands.rs:638`).
- Produces: Tauri command `set_usage_refresh_interval(state, seconds: u32, app) -> Result<(), String>`; `SettingsView` gains `usage_refresh_interval_secs: u32`; `get_settings` returns the clamped value.

Note: the new command mirrors `set_port`'s validate-then-persist pattern and is not separately unit-tested (it needs a Tauri `State`/`AppHandle`, like `set_port`). The tamper-protection logic it depends on (`clamp_usage_refresh_secs`) is already covered by Task 1's tests. Verification here is compile + the existing test suite still passing.

- [ ] **Step 1: Add the config imports**

In `src-tauri/src/commands.rs`, change line 7 from:
```rust
use crate::config::{AppConfig, Model, Profile, Provider};
```
to:
```rust
use crate::config::{
    clamp_usage_refresh_secs, AppConfig, MAX_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, Model, Profile, Provider,
};
```

- [ ] **Step 2: Extend `SettingsView` and clamp in `get_settings`**

Replace the `SettingsView` struct + `get_settings` (lines 514-525) with:
```rust
/// Read-only view of app settings (port + autostart + usage refresh interval).
#[derive(Serialize)]
pub struct SettingsView {
    pub port: u16,
    pub autostart: bool,
    pub usage_refresh_interval_secs: u32,
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<SettingsView, String> {
    let cfg = state.config.read().await;
    Ok(SettingsView {
        port: cfg.settings.port,
        autostart: cfg.settings.autostart,
        usage_refresh_interval_secs: clamp_usage_refresh_secs(cfg.settings.usage_refresh_interval_secs),
    })
}
```

- [ ] **Step 3: Add the `set_usage_refresh_interval` command**

Insert immediately after the `set_port` function (after its closing `}` at line 578):
```rust
/// Update the usage auto-refresh interval (seconds). Validates [30, 3600] then persists.
/// The floor is also re-clamped on read (`get_settings`), so a tampered config file
/// can never drive polling below the minimum.
#[tauri::command]
pub async fn set_usage_refresh_interval(
    state: State<'_, AppState>,
    seconds: u32,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if seconds < MIN_USAGE_REFRESH_SECS {
        return Err(format!("刷新间隔不能小于 {MIN_USAGE_REFRESH_SECS} 秒"));
    }
    if seconds > MAX_USAGE_REFRESH_SECS {
        return Err(format!("刷新间隔不能大于 {MAX_USAGE_REFRESH_SECS} 秒"));
    }
    {
        let mut config = state.config.write().await;
        config.settings.usage_refresh_interval_secs = seconds;
        persist(&app, &config)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Register the command**

In `src-tauri/src/lib.rs`, inside `generate_handler!`, add a line after `commands::set_port,` (line 123):
```rust
            commands::set_usage_refresh_interval,
```

- [ ] **Step 5: Verify it compiles and existing tests pass**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: compiles with no errors.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests PASS (including Task 1's).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(usage): add set_usage_refresh_interval command + clamp on read"
```

---

### Task 3: Frontend types + constants + invoke wrapper + store action

**Files:**
- Modify: `src/lib/types.ts` (`Settings` lines 35-38; `SettingsView` lines 89-92)
- Modify: `src/lib/commands.ts` (after `setPort` line 71)
- Modify: `src/stores/system.ts` (after `savePort` lines 38-41; in return object lines 57-70)

**Interfaces:**
- Produces: `Settings.usage_refresh_interval_secs` / `SettingsView.usage_refresh_interval_secs` (number); exported consts `MIN_USAGE_REFRESH_SECS`, `DEFAULT_USAGE_REFRESH_SECS`, `MAX_USAGE_REFRESH_SECS`; `api.setUsageRefreshInterval(seconds)`; store action `system.saveUsageRefreshInterval(seconds)`.

- [ ] **Step 1: Mirror the field + export bounds constants in `types.ts`**

In `src/lib/types.ts`, update `Settings` (lines 35-38):
```ts
export interface Settings {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
}
```
Update `SettingsView` (lines 89-92):
```ts
export interface SettingsView {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
}
```
Append at the end of the file (after the `SettingsView` interface):
```ts
// Mirror src-tauri/src/config/types.rs — keep in sync.
export const MIN_USAGE_REFRESH_SECS = 30;
export const DEFAULT_USAGE_REFRESH_SECS = 60;
export const MAX_USAGE_REFRESH_SECS = 3600;
```

- [ ] **Step 2: Add the invoke wrapper in `commands.ts`**

In `src/lib/commands.ts`, after the `setPort` line (line 71), add:
```ts
export const setUsageRefreshInterval = (seconds: number) =>
  invoke<void>("set_usage_refresh_interval", { seconds });
```

- [ ] **Step 3: Add the store action in `system.ts`**

In `src/stores/system.ts`, after `savePort` (lines 38-41), add:
```ts
  async function saveUsageRefreshInterval(seconds: number) {
    await api.setUsageRefreshInterval(seconds);
    await loadSettings();
  }
```
And add `saveUsageRefreshInterval,` to the returned object (e.g. after the `savePort,` line inside the `return { ... }`).

- [ ] **Step 4: Verify type-check passes**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts
git commit -m "feat(usage): frontend types + setUsageRefreshInterval plumbing"
```

---

### Task 4: Make `usePolling` accept a reactive interval

**Files:**
- Modify: `src/lib/usePolling.ts` (whole file; currently a static-interval composable)

**Interfaces:**
- Produces: `usePolling(fn, intervalMs: number | (() => number))`. Existing number callers keep working unchanged; getter callers re-arm the timer whenever the returned value changes (via `watch`).

- [ ] **Step 1: Rewrite `usePolling` to support a getter**

Replace the entire contents of `src/lib/usePolling.ts` with:
```ts
import { onUnmounted, watch } from "vue";

/**
 * Repeatedly calls `fn` every `intervalMs` while the host component is mounted,
 * stopping automatically on unmount. The first call happens after the interval
 * (not immediately), so a refresh already triggered in `onMounted` is not
 * duplicated right away.
 *
 * `intervalMs` may be a number or a `() => number` getter. When it is a getter,
 * the timer is re-armed (clear + restart) whenever the value changes — e.g. once
 * settings finish loading after mount, or when the user changes the interval.
 *
 * Errors are swallowed — background polling must never spam the UI with toasts.
 * Callers that surface failures (e.g. a manual "刷新" button) keep their own
 * try/catch around the same store action.
 *
 * There is no `<keep-alive>` in the app shell, so views go fully dormant while
 * the user is on another tab; this interval is cleared on unmount and re-armed
 * on the next mount, so tab-hopping never leaks a dangling timer.
 */
export function usePolling(
  fn: () => void | Promise<void>,
  intervalMs: number | (() => number),
): void {
  const getMs = typeof intervalMs === "number" ? () => intervalMs : intervalMs;
  let handle: number | null = null;
  const stop = () => {
    if (handle != null) {
      clearInterval(handle);
      handle = null;
    }
  };
  const arm = () => {
    stop();
    handle = window.setInterval(() => {
      Promise.resolve(fn()).catch(() => {});
    }, getMs());
  };
  arm();
  if (typeof intervalMs !== "number") {
    watch(intervalMs, arm);
  }
  onUnmounted(stop);
}
```

- [ ] **Step 2: Verify type-check passes (existing number callers still valid)**

Run: `npx vue-tsc --noEmit`
Expected: no errors (`Usage.vue` and `Dashboard.vue` still pass `60_000` as a number).

- [ ] **Step 3: Commit**

```bash
git add src/lib/usePolling.ts
git commit -m "refactor(usage): usePolling accepts a reactive interval getter"
```

---

### Task 5: Settings UI card

**Files:**
- Modify: `src/views/Settings.vue` (script after the `portInput` watch ~line 27; template after the "开机自启" card ~line 98)

**Interfaces:**
- Consumes: `system.saveUsageRefreshInterval(seconds)` (Task 3), bounds consts (Task 3), `system.settings.usage_refresh_interval_secs` (backend Task 2).

- [ ] **Step 1: Add the constants import**

In `src/views/Settings.vue`, add to the existing import from `../lib/types` — but `Settings.vue` does not currently import from types, so add a new line after the `useSystemStore` import (line 13):
```ts
import { MAX_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS } from "../lib/types";
```

- [ ] **Step 2: Add `refreshInput` state + `saveRefresh` handler**

In the `<script setup>` block, after the existing `portInput` `watch` block (after line 27), add:
```ts
const refreshInput = ref<number | null>(null);
watch(
  () => system.settings,
  (s) => {
    if (s) refreshInput.value = s.usage_refresh_interval_secs;
  },
  { immediate: true },
);

async function saveRefresh() {
  if (refreshInput.value == null) return;
  try {
    await system.saveUsageRefreshInterval(refreshInput.value);
    msg.success("刷新间隔已保存");
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}
```
(`ref`, `watch`, `NInputNumber`, `NButton`, `NCard`, `NSpace`, `useMessage` are all already imported in `Settings.vue`.)

- [ ] **Step 3: Add the card to the template**

In the `<template>`, insert a new card between the "开机自启" card (ends line 98) and the "服务" card (starts line 100):
```html
    <NCard title="用量刷新间隔" size="small">
      <NSpace align="center" :size="12">
        <NInputNumber
          v-model:value="refreshInput"
          :min="MIN_USAGE_REFRESH_SECS"
          :max="MAX_USAGE_REFRESH_SECS"
          style="width: 160px"
        >
          <template #suffix>秒</template>
        </NInputNumber>
        <NButton type="primary" @click="saveRefresh">保存</NButton>
        <span class="muted">最小 {{ MIN_USAGE_REFRESH_SECS }}s · 默认 60s · 停留用量/概览页面时自动刷新的频率</span>
      </NSpace>
    </NCard>
```

- [ ] **Step 4: Verify type-check passes**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/views/Settings.vue
git commit -m "feat(usage): add refresh-interval setting card"
```

---

### Task 6: Consume the setting in Usage + Dashboard pages

**Files:**
- Modify: `src/views/Usage.vue` (imports lines 4-8; store setup ~line 12; onMounted ~line 64; usePolling ~line 71; template hint line 72)
- Modify: `src/views/Dashboard.vue` (types import line 20; usePolling ~line 183)

**Interfaces:**
- Consumes: reactive `usePolling` (Task 4), bounds consts + `system.settings.usage_refresh_interval_secs` (Tasks 2-3).

- [ ] **Step 1: Usage.vue — wire the system store + reactive interval + dynamic hint**

(a) Imports: change the types import (line 6) from:
```ts
import type { UsageEntry } from "../lib/types";
```
to:
```ts
import { DEFAULT_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, type UsageEntry } from "../lib/types";
```
And add the system store import after the runtime store import (after line 5):
```ts
import { useSystemStore } from "../stores/system";
```

(b) Store setup: after `const config = useConfigStore();` (line 12), add:
```ts
const system = useSystemStore();
```

(c) Effective interval (add after the `refresh` function, before `onMounted`):
```ts
/** Effective auto-refresh interval shown in the UI (clamped to the 30s floor). */
const refreshSecs = computed(() =>
  Math.max(MIN_USAGE_REFRESH_SECS, system.settings?.usage_refresh_interval_secs ?? DEFAULT_USAGE_REFRESH_SECS),
);
```

(d) Load settings on mount — change `onMounted` from:
```ts
onMounted(async () => {
  await Promise.all([config.loadAll(), runtime.refresh()]);
});
```
to:
```ts
onMounted(async () => {
  await Promise.all([config.loadAll(), runtime.refresh(), system.loadSettings()]);
});
```

(e) Reactive polling — replace the existing `usePolling(...)` call + its comment with:
```ts
// Auto-refresh usage while the user stays on this page, at the configured interval
// (read from settings; clamped to the 30s floor both here and in the backend).
usePolling(() => runtime.refresh(), () => refreshSecs.value * 1000);
```

(f) Dynamic hint text — in the template, change:
```html
      <span class="muted">套餐额度明细（每 60s 自动刷新）</span>
```
to:
```html
      <span class="muted">套餐额度明细（每 {{ refreshSecs }}s 自动刷新）</span>
```

- [ ] **Step 2: Dashboard.vue — reactive interval**

(a) Imports: change line 20 from:
```ts
import type { Profile } from "../lib/types";
```
to:
```ts
import { DEFAULT_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, type Profile } from "../lib/types";
```
(`system` is already imported and instantiated in `Dashboard.vue`, and `system.loadSettings()` is already called in its `onMounted`.)

(b) Replace the existing `usePolling(...)` call + its comment (the one that reads `60_000`) with:
```ts
// Auto-refresh usage while the user stays on this page (drives "当前可用额度" and
// per-route quota), at the configured interval (clamped to the 30s floor).
usePolling(
  () => runtime.refresh(),
  () =>
    Math.max(
      MIN_USAGE_REFRESH_SECS,
      system.settings?.usage_refresh_interval_secs ?? DEFAULT_USAGE_REFRESH_SECS,
    ) * 1000,
);
```

- [ ] **Step 3: Verify type-check passes**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Manual verification**

Run the app, then:
1. Open 设置 → "用量刷新间隔" card → confirm it shows `60`, change to e.g. `45`, click 保存 → success toast.
2. Open 套餐用量 → confirm hint reads "每 45s 自动刷新"; stay on the page ~50s and confirm the data refreshes (loading spinner blips).
3. Confirm 概览 also refreshes at the new cadence.
4. Tamper test: edit `app_config.json` `"usage_refresh_interval_secs": 1`, reload app, open 套餐用量 → confirm hint reads "每 30s 自动刷新" (clamped, not 1s).

- [ ] **Step 5: Commit**

```bash
git add src/views/Usage.vue src/views/Dashboard.vue
git commit -m "feat(usage): poll at the configured refresh interval on Usage + Dashboard"
```

---

## Verification Summary

- **Backend tamper-protection:** `clamp_usage_refresh_secs` unit test (Task 1) + clamp-on-read in `get_settings` (Task 2) + execution-time `Math.max` in both pages (Task 6).
- **Backward compat:** `settings_default_when_field_absent` test (Task 1) + `#[serde(default)]`.
- **Bounds UI:** `NInputNumber :min/:max` (Task 5) + backend save rejection (Task 2).
- **Full type-check:** `npx vue-tsc --noEmit` after each frontend task; full `cargo test` after Task 2.
