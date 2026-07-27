# Usage Refresh Interval Setting Design

**Date:** 2026-07-29
**Status:** Approved
**Author:** Claude (SwitchLM Project)

## Overview

The Overview (概览) and Usage (套餐用量) pages auto-refresh usage data while the user stays on
them (added in a prior change), currently hardcoded to every 60s. This design makes that interval
**user-configurable** in the Settings page, with:

- **Default 60s, minimum 30s, maximum 3600s.**
- The 30s minimum enforced at the **code layer** (not just the UI), so a hand-edited `app_config.json`
  cannot lower the interval below 30s and cause excessive polling.

The backend usage cache TTL (`DEFAULT_USAGE_TTL_SECS = 60`) stays fixed and **decoupled** from this
setting — see "Why this is server-safe" below.

## Requirements

### Functional Requirements

1. **Configurable interval** — A new persisted setting `usage_refresh_interval_secs` controls how
   often the Overview and Usage pages poll `runtime.refresh()` while mounted.
2. **Bounds** — Default 60s, minimum 30s, maximum 3600s. Values outside the range are rejected on save.
3. **Backward compatible** — Old `app_config.json` files without the field load with the 60s default
   (`#[serde(default)]`), never breaking existing installs.
4. **Tamper-proof minimum** — Even if the config file is edited directly to a value below 30s, the
   effective polling interval never drops below 30s.
5. **Settings UI** — A "用量刷新间隔" card with a number input (seconds) and a "保存" button, matching
   the existing port-setting card pattern.
6. **Live effect** — Saving a new interval takes effect on the next page mount (no `<keep-alive>`, so
   navigating to/from a page re-reads the current setting); and re-arms if the setting loads after
   the page is already mounted.

### Non-Functional Requirements

1. **Server safety** — The 60s backend cache is the real upstream-pressure limiter; the polling
   interval only changes how often the UI re-reads (cached or fresh), never how often the upstream
   provider API is hit.
2. **No new dependencies.**
3. **Follows existing settings conventions** exactly (per-field serde default, decoupled `SettingsView`
   DTO, dedicated `set_*` command with inline validation + `persist`, typed invoke wrapper, store
   action, `NInputNumber` + "保存").

## Why This Is Server-Safe (Key Design Point)

`query_usage` consults a 60s cache (`src-tauri/src/usage/mod.rs`, `DEFAULT_USAGE_TTL_SECS = 60`): a
poll landing inside the cache window returns instantly from memory and makes **no upstream request**.
So even at the 30s floor, each provider gets at most **one real upstream fetch per 60s**.

```
poll at t=0s  → cache miss → upstream fetch (sets cache at t=0)
poll at t=30s → cache hit  → memory read, NO upstream call
poll at t=60s → cache miss → upstream fetch
```

Coupling the cache TTL to the poll interval would *increase* upstream pressure (e.g. 30s polls with a
30s cache → upstream every 30s), which contradicts the goal. The cache therefore stays fixed at 60s;
the poll interval is purely a UI-refresh cadence control. The 30s minimum is an additional guard
against UI churn, not the primary pressure limiter.

## The 30s Minimum — Four-Layer Enforcement

The user's explicit concern is config-file tampering. The minimum is enforced at multiple layers so
the value reaching `setInterval` is always ≥ 30s:

| Layer | Where | Catches |
|-------|-------|---------|
| UI input | `Settings.vue` `NInputNumber :min="30"` | Accidental low entry |
| Backend write | `set_usage_refresh_interval` rejects `< 30` (or `> 3600`) | Normal API calls passing a bad value |
| **Backend read** | `get_settings` projection runs `clamp_usage_refresh_secs()` on the stored value | **Hand-edited `app_config.json`** (e.g. `"1"`) |
| **Frontend execution** | the polling interval getter does `Math.max(30, …)` before `setInterval` | **Final code-layer backstop**, independent of backend |

The last two are the "代码层级检查" (code-layer checks): even if the persisted file says `1`, the backend
returns 30, and the frontend clamps again — `window.setInterval` can never receive `< 30s`.

## Implementation Architecture

### Data Flow

```
Settings.usage_refresh_interval_secs  (persisted in app_config.json, u32)
        │
        │  get_settings() → SettingsView: clamp_usage_refresh_secs(value)   ← layer 3
        ▼
system.settings.usage_refresh_interval_secs  (Pinia store)
        │
        │  usePolling getter: Math.max(MIN, value ?? DEFAULT) × 1000        ← layer 4
        ▼
window.setInterval(() => runtime.refresh(), ms)
```

### Backend (Rust)

#### 1. Persisted model + constants

**File: `src-tauri/src/config/types.rs`**

Add the field with a custom serde default (mirrors `port`), plus module constants and a pure clamp
helper:

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

fn default_usage_refresh_interval_secs() -> u32 {
    DEFAULT_USAGE_REFRESH_SECS
}

/// Clamp a usage-refresh interval (seconds) into the allowed range.
/// Applied on read so a tampered config file can never drive polling below the floor.
pub fn clamp_usage_refresh_secs(secs: u32) -> u32 {
    secs.clamp(MIN_USAGE_REFRESH_SECS, MAX_USAGE_REFRESH_SECS)
}
```

Update the `app_config_roundtrips` test to include the new field.

#### 2. Read DTO + projection with clamp

**File: `src-tauri/src/commands.rs`**

```rust
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

#### 3. Write command with validation

**File: `src-tauri/src/commands.rs`** — follows the `set_port` validation-then-persist template:

```rust
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

#### 4. Command registration

**File: `src-tauri/src/lib.rs`** — add `commands::set_usage_refresh_interval` to `generate_handler!`.

### Frontend (TypeScript / Vue)

#### 1. Types + mirrored constants

**File: `src/lib/types.ts`** — mirror the field into both interfaces and export matching constants:

```ts
export interface Settings {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
}

export interface SettingsView {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
}

// Mirror src-tauri/src/config/types.rs — keep in sync.
export const MIN_USAGE_REFRESH_SECS = 30;
export const DEFAULT_USAGE_REFRESH_SECS = 60;
export const MAX_USAGE_REFRESH_SECS = 3600;
```

#### 2. Invoke wrapper

**File: `src/lib/commands.ts`**

```ts
export const setUsageRefreshInterval = (seconds: number) =>
  invoke<void>("set_usage_refresh_interval", { seconds });
```

#### 3. Store action

**File: `src/stores/system.ts`** — same call-setter-then-reload pattern as `savePort`:

```ts
async function saveUsageRefreshInterval(seconds: number) {
  await api.setUsageRefreshInterval(seconds);
  await loadSettings();
}
```

#### 4. Settings UI

**File: `src/views/Settings.vue`** — new card mirroring the port card (number input + "保存"):

```vue
<NCard title="用量刷新间隔" size="small">
  <NSpace align="center" :size="12">
    <NInputNumber v-model:value="refreshInput" :min="30" :max="3600" style="width: 160px">
      <template #suffix>秒</template>
    </NInputNumber>
    <NButton type="primary" @click="saveRefresh">保存</NButton>
    <span class="muted">最小 30s · 默认 60s · 停留用量/概览页面时自动刷新的频率</span>
  </NSpace>
</NCard>
```

```ts
const refreshInput = ref<number | null>(null);
watch(
  () => system.settings,
  (s) => { if (s) refreshInput.value = s.usage_refresh_interval_secs; },
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

#### 5. Reactive polling interval

**File: `src/lib/usePolling.ts`** — accept `number | (() => number)`; when a getter is passed, re-arm
the timer via `watch` whenever the interval changes (e.g. once settings finish loading after mount):

```ts
import { onUnmounted, watch } from "vue";

export function usePolling(
  fn: () => void | Promise<void>,
  intervalMs: number | (() => number),
): void {
  const getMs = typeof intervalMs === "number" ? () => intervalMs : intervalMs;
  let handle: number | null = null;
  const stop = () => {
    if (handle != null) { clearInterval(handle); handle = null; }
  };
  const arm = () => {
    stop();
    handle = window.setInterval(() => { Promise.resolve(fn()).catch(() => {}); }, getMs());
  };
  arm();
  if (typeof intervalMs !== "number") watch(intervalMs, arm);
  onUnmounted(stop);
}
```

#### 6. Consume the setting in both pages

**Files: `src/views/Usage.vue`, `src/views/Dashboard.vue`**

```ts
import { DEFAULT_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS } from "../lib/types";

usePolling(
  () => runtime.refresh(),
  () =>
    Math.max(
      MIN_USAGE_REFRESH_SECS,
      system.settings?.usage_refresh_interval_secs ?? DEFAULT_USAGE_REFRESH_SECS,
    ) * 1000,
);
```

`Usage.vue` also adds `system.loadSettings()` to its `onMounted` (it currently does not load settings)
so the configured interval is available. Its hint text changes from the hardcoded "每 60s 自动刷新" to
the live value: `套餐额度明细（每 {{ refreshSecs }}s 自动刷新）`.

## Testing Strategy

### Unit Tests (backend, no Tauri handle needed)

`clamp_usage_refresh_secs` — the core tamper-protection logic:

| Input | Expected |
|-------|----------|
| `0`   | `30` |
| `5`   | `30` |
| `29`  | `30` |
| `30`  | `30` |
| `60`  | `60` |
| `120` | `120` |
| `3600`| `3600` |
| `99999` | `3600` |

Plus update `app_config_roundtrips` to assert the field round-trips and defaults to 60 when absent.

### Type Check / Manual (frontend)

- `vue-tsc --noEmit` passes.
- Manual: set interval in Settings → stay on Usage/Overview → confirm the refresh cadence matches;
  hand-edit `app_config.json` to `"usage_refresh_interval_secs": 1`, reload → confirm polling stays
  ≥ 30s (the displayed hint shows 30s, not 1s).

## Files Changed

### Backend
1. `src-tauri/src/config/types.rs` — field + constants + `clamp_usage_refresh_secs` + roundtrip test.
2. `src-tauri/src/commands.rs` — `SettingsView` + `get_settings` clamp + `set_usage_refresh_interval`.
3. `src-tauri/src/lib.rs` — register command.

### Frontend
1. `src/lib/types.ts` — field in both interfaces + exported constants.
2. `src/lib/commands.ts` — `setUsageRefreshInterval` wrapper.
3. `src/stores/system.ts` — `saveUsageRefreshInterval` action.
4. `src/views/Settings.vue` — "用量刷新间隔" card.
5. `src/lib/usePolling.ts` — reactive interval support.
6. `src/views/Usage.vue` — consume setting + load settings + dynamic hint text.
7. `src/views/Dashboard.vue` — consume setting.

## Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Unit | seconds (`u32`) | Matches the 60s cache TTL constant; human-readable in the JSON file. |
| Cache TTL | Fixed 60s, decoupled | Coupling it to the interval would *raise* upstream pressure; the cache is the real pressure limiter. |
| Min enforcement | 3 layers (UI / backend save / backend-read clamp / frontend-exec clamp) | User explicitly wants tamper-proofing at the code layer, not just UI. |
| Interval reactivity | `usePolling` accepts a getter + `watch` | Handles settings loading after mount and live updates without breaking the synchronous `onUnmounted` registration. |
| Max interval | 3600s | Prevents absurd values; a 1h upper bound is generous without effectively disabling refresh. |
| Disable auto-refresh | Not offered | Out of scope; min 30s floor keeps it always-on. |

## Success Criteria

1. ✅ Setting is persisted in `app_config.json` as `usage_refresh_interval_secs` (seconds).
2. ✅ Default 60s; old config files without the field load cleanly at 60s.
3. ✅ Saving a value < 30 or > 3600 is rejected with a clear error.
4. ✅ Editing `app_config.json` to a sub-30 value cannot lower the effective polling interval below 30s.
5. ✅ Overview and Usage pages poll at the configured interval while mounted.
6. ✅ No resource leak (timer cleared on unmount; re-armed cleanly on interval change).
7. ✅ `clamp_usage_refresh_secs` unit tests pass; `vue-tsc --noEmit` passes.
