# Port Occupation Auto-Recovery Design

**Date:** 2026-07-29
**Status:** Approved
**Author:** Claude (SwitchLM Project)

## Overview

When the user-configured proxy port (default 6950) is occupied, the application should:
1. Not exit or crash
2. Not try alternative ports
3. Display clear error messages in the Dashboard overview
4. Show running status as "未运行" (not running)
5. Automatically retry binding every 10 seconds in the background
6. Automatically start the service when binding succeeds

## Requirements

### Functional Requirements

1. **Port Binding Strategy**
   - Try ONLY the user-configured port once
   - Do NOT attempt alternative ports
   - Default port: 6950 (user-configurable)

2. **Error Display**
   - Show clear error message in Dashboard overview
   - Use both NAlert component and status card
   - Display running status as "未运行"

3. **Background Auto-Recovery**
   - Retry port binding every 10 seconds
   - Automatically start service when binding succeeds
   - Update status to "运行中" (running) when successful
   - Clear error messages when service starts

4. **Startup Behavior**
   - Application starts normally even if port is occupied
   - Silent failure - no popup dialogs or notifications
   - Background polling begins immediately

5. **Port Configuration Changes**
   - When user changes port setting: stop old polling, try new port
   - If new port is occupied: start new polling with new port
   - Update error messages to reflect new port number

### Non-Functional Requirements

1. **Performance**
   - Polling interval: 10 seconds (balance between responsiveness and system load)
   - Minimal CPU/memory overhead during polling

2. **Resource Management**
   - Clean up polling tasks when successful
   - Stop polling on application exit
   - Stop polling when port configuration changes

3. **User Experience**
   - Application remains responsive during polling
   - Error messages are clear and actionable
   - Auto-recovery is transparent to user

## Implementation Architecture

### Overall Architecture

```
┌─────────────────┐
│   Frontend      │
│  (Dashboard)    │
└────────┬────────┘
         │ Tauri commands
         ▼
┌─────────────────────────────┐
│   Rust Backend (AppState)   │
│                             │
│  ┌──────────────────────┐   │
│  │  Port Binding Logic  │   │
│  │  - serve_once()      │   │
│  │  - start_server()    │   │
│  └──────────────────────┘   │
│             │               │
│             ▼               │
│  ┌──────────────────────┐   │
│  │  Polling Task        │   │
│  │  - 10s interval      │   │
│  │  - auto-retry        │   │
│  │  - cleanup on exit   │   │
│  └──────────────────────┘   │
│             │               │
│             ▼               │
│  ┌──────────────────────┐   │
│  │  State Management    │   │
│  │  - running: bool     │   │
│  │  - bind_error: Option<String>
│  │  - polling_handle: Option<JoinHandle>│
│  └──────────────────────┘   │
└─────────────────────────────┘
```

### Backend Implementation (Rust)

#### 1. Data Structure Changes

**File: `src-tauri/src/proxy/state.rs`**

Add new fields to `AppState`:

```rust
pub struct AppState {
    // Existing fields...
    pub inner: Arc<AppStateInner>,
    pub server_handle: Mutex<Option<JoinHandle<()>>>,
    pub actual_port: Mutex<Option<u16>>,

    // New fields
    pub bind_error: Mutex<Option<String>>,              // Port binding error message
    pub polling_handle: Mutex<Option<JoinHandle<()>>>, // Polling task handle
}
```

#### 2. New Function: `serve_once()`

**File: `src-tauri/src/proxy/server.rs`**

Replace current auto-increment behavior with single attempt:

```rust
/// Try to bind to the specified port (only once, no auto-increment)
/// Returns detailed error on failure
pub async fn serve_once(state: AppState, port: u16) -> std::io::Result<(JoinHandle<()>, u16)> {
    match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => {
            let app = build_router(state);
            let handle = tokio::spawn(async move {
                let _ = axum::serve(listener, app.into_make_service()).await;
            });
            Ok((handle, port))
        }
        Err(e) => {
            // Return specific error (AddrInuse or other)
            Err(e)
        }
    }
}
```

#### 3. Polling Management

**File: `src-tauri/src/proxy/state.rs`**

Add polling control methods:

```rust
impl AppState {
    /// Start polling task (if not already running)
    pub async fn start_polling(&self, port: u16) {
        let mut handle = self.polling_handle.lock().await;
        if handle.is_some() {
            return; // Already polling
        }

        let state = self.clone();
        let task = tokio::spawn(async move {
            Self::polling_loop(state, port).await;
        });
        *handle = Some(task);
    }

    /// Stop polling task
    pub async fn stop_polling(&self) {
        let mut handle = self.polling_handle.lock().await;
        if let Some(task) = handle.take() {
            task.abort();
        }
    }

    /// Polling loop - retries every 10 seconds
    async fn polling_loop(state: AppState, port: u16) {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;

            // Try to bind and start
            match server::serve_once(state.inner().clone(), port).await {
                Ok((handle, actual_port)) => {
                    // Success: update state
                    state.set_server_handle(handle).await;
                    state.set_actual_port(Some(actual_port)).await;
                    state.clear_bind_error().await;
                    state.stop_polling().await;
                    tracing::info!("自动恢复成功：服务已在端口 {actual_port} 上启动");
                    break;
                }
                Err(e) => {
                    // Failure: continue polling
                    tracing::debug!("自动重试绑定失败：{e}");
                }
            }
        }
    }

    /// Set binding error
    pub async fn set_bind_error(&self, error: String) {
        *self.bind_error.lock().await = Some(error);
    }

    /// Clear binding error
    pub async fn clear_bind_error(&self) {
        *self.bind_error.lock().await = None;
    }

    /// Get binding error
    pub async fn get_bind_error(&self) -> Option<String> {
        self.bind_error.lock().await.clone()
    }
}
```

#### 4. Application Startup Logic

**File: `src-tauri/src/lib.rs`**

```rust
async fn start_proxy_with_retry(state: AppState, port: u16) {
    match server::serve_once(state.inner().clone(), port).await {
        Ok((handle, actual_port)) => {
            state.set_server_handle(handle).await;
            state.set_actual_port(Some(actual_port)).await;
            tracing::info!("代理已在端口 {actual_port} 上启动");
        }
        Err(e) => {
            // Port binding failed
            let error_msg = format!(
                "端口 {} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。",
                port
            );
            state.set_bind_error(error_msg).await;
            state.start_polling(port).await;
            tracing::warn!("端口 {port} 绑定失败：{e}，已启动自动重试");
        }
    }
}
```

#### 5. Modify `restart_server` Command

**File: `src-tauri/src/commands.rs`**

```rust
#[tauri::command]
pub async fn restart_server(state: State<'_, AppState>) -> Result<Option<u16>, String> {
    let preferred = state.config.read().await.settings.port;

    // Stop existing service and polling
    state.stop_polling().await;
    if let Some(old) = state.take_server_handle().await {
        old.abort();
        let _ = old.await;
    }
    state.clear_actual_port().await;

    // Try to start
    match server::serve_once(state.inner().clone(), preferred).await {
        Ok((handle, port)) => {
            state.set_actual_port(port).await;
            state.set_server_handle(handle).await;
            tracing::info!("服务已在端口 {port} 上重启");
            Ok(Some(port))
        }
        Err(e) => {
            let error_msg = format!(
                "端口 {} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。",
                preferred
            );
            state.set_bind_error(error_msg).await;
            state.start_polling(preferred).await;
            tracing::warn!("重启失败：{e}，已启动自动重试");
            Err(error_msg)
        }
    }
}
```

#### 6. Modify `set_port` Command

**File: `src-tauri/src/commands.rs`**

```rust
#[tauri::command]
pub async fn set_port(state: State<'_, AppState>, port: u16) -> Result<(), String> {
    // Validate port range
    if !(1024..=65535).contains(&port) {
        return Err("端口必须在 1024-65535 范围内".into());
    }

    // Stop existing polling
    state.stop_polling().await;

    // Update configuration
    {
        let mut config = state.config.write().await;
        config.settings.port = port;
        store::save(&config).map_err(|e| e.to_string())?;
    }

    // Try to bind new port
    match server::serve_once(state.inner().clone(), port).await {
        Ok((handle, actual_port)) => {
            // Stop old service
            if let Some(old) = state.take_server_handle().await {
                old.abort();
                let _ = old.await;
            }
            state.set_server_handle(handle).await;
            state.set_actual_port(actual_port).await;
            tracing::info!("端口已更改为 {port}，服务已启动");
        }
        Err(e) => {
            let error_msg = format!(
                "端口 {} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。",
                port
            );
            state.set_bind_error(error_msg).await;
            state.start_polling(port).await;
            tracing::warn!("新端口 {port} 绑定失败：{e}，已启动自动重试");
        }
    }

    Ok(())
}
```

#### 7. New Tauri Command

**File: `src-tauri/src/commands.rs`**

```rust
#[tauri::command]
pub async fn get_bind_error(state: State<'_, AppState>) -> Option<String> {
    state.get_bind_error().await
}
```

### Frontend Implementation (TypeScript/Vue)

#### 1. Extend `system` Store

**File: `src/stores/system.ts`**

```typescript
import { defineStore } from 'pinia';
import { ref } from 'vue';

export const useSystemStore = defineStore('system', () => {
  const status = ref<ServerStatus | null>(null);
  const settings = ref<Settings | null>(null);
  const envSnippet = ref<EnvSnippet | null>(null);

  // New: binding error information
  const bindError = ref<string | null>(null);

  async function refresh() {
    try {
      const [statusResult, settingsResult, errorResult] = await Promise.all([
        invoke<ServerStatus>('get_server_status'),
        invoke<Settings>('get_settings'),
        invoke<Option<string>>('get_bind_error'),
      ]);

      status.value = statusResult;
      settings.value = settingsResult;
      bindError.value = errorResult; // New
    } catch (e) {
      console.error('Failed to refresh system status:', e);
    }
  }

  return {
    status,
    settings,
    envSnippet,
    bindError,  // New
    refresh,
  };
});
```

#### 2. Modify `Dashboard.vue`

**File: `src/views/Dashboard.vue`**

Add error display and auto-refresh:

```vue
<template>
  <NSpace vertical :size="16">
    <!-- New: Port binding error alert -->
    <NAlert v-if="system.bindError" type="error" :show-icon="true" closable>
      {{ system.bindError }}
    </NAlert>

    <!-- Existing: Port mismatch warning -->
    <NAlert v-if="hasContent && portMismatch && !system.bindError" type="warning" :show-icon="true">
      实际端口 {{ system.status?.port }} 与设置 {{ system.settings?.port }} 不符 —— 前往「设置」重启服务以应用新端口。
    </NAlert>

    <!-- Running status card - Modified to show detailed error -->
    <NGrid v-if="hasContent" :cols="2" :x-gap="16" :y-gap="16" responsive="screen">
      <NGi>
        <NCard>
          <NStatistic label="运行状态">
            <template v-if="system.bindError">
              <!-- Display when port is occupied -->
              <NTag type="error" round size="small">
                端口被占用
              </NTag>
              <div class="error-detail">
                {{ system.bindError }}
              </div>
            </template>
            <template v-else>
              <!-- Normal status display -->
              <NTag :type="system.status?.running ? 'success' : 'default'" round size="small">
                {{ system.status?.running ? "运行中" : "未运行" }}
              </NTag>
            </template>
          </NStatistic>
        </NCard>
      </NGi>

      <!-- Other cards remain unchanged -->
      <NGi>
        <NCard>
          <NStatistic label="当前可用额度">
            <span class="mono big" :class="availableStatus">
              {{ availableQuotaPct != null ? `${availableQuotaPct}%` : "—" }}
            </span>
          </NStatistic>
        </NCard>
      </NGi>
    </NGrid>

    <!-- Other content remains unchanged -->
  </NSpace>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';

// ... existing imports ...

const refreshInterval = ref<number | null>(null);

onMounted(async () => {
  await Promise.all([
    config.loadAll(),
    system.refresh(),
    runtime.refresh(),
    system.loadEnvSnippet(),
    system.loadSettings(),
  ]);

  // If there's a binding error, start periodic refresh (every 5s)
  // to detect backend auto-recovery
  if (system.bindError) {
    refreshInterval.value = window.setInterval(async () => {
      await system.refresh();
      // Clear interval when error is gone
      if (!system.bindError) {
        if (refreshInterval.value) {
          clearInterval(refreshInterval.value);
          refreshInterval.value = null;
        }
      }
    }, 5000);
  }
});

onUnmounted(() => {
  if (refreshInterval.value) {
    clearInterval(refreshInterval.value);
  }
});

// ... rest of existing code ...
</script>

<style scoped>
/* Existing styles remain unchanged */

/* New styles */
.error-detail {
  margin-top: 8px;
  font-size: 12px;
  color: var(--sl-text-2);
  line-height: 1.4;
}
</style>
```

## User Experience Flow

```
User starts application
    ↓
Port 6950 is occupied
    ↓
Application starts normally
    ↓
Dashboard displays:
  - Red NAlert: error message
  - Running status: red "端口被占用"
  - Details: Auto-retry every 10 seconds
    ↓
(Background: retry every 10 seconds)
    ↓
User releases port 6950
    ↓
(Within 10 seconds) Background binding succeeds
    ↓
Service starts automatically
    ↓
Frontend detects state change (within 5 seconds)
    ↓
Dashboard updates:
  - NAlert disappears
  - Running status: green "运行中"
```

## Testing Strategy

### Unit Tests

1. **Test `serve_once()` only tries once**
   - Verify it doesn't auto-increment port
   - Returns error on AddrInuse

2. **Test polling task lifecycle**
   - Polling starts on bind failure
   - Polling stops on success
   - Polling stops on port change

### Integration Tests

1. **Scenario: Port occupied → Auto-recovery**
   - Occupy port 6950
   - Start service (should fail and start polling)
   - Verify error state
   - Release port
   - Wait 11 seconds
   - Verify service started automatically

2. **Scenario: Change port while polling**
   - Start polling on port 6950
   - Change to port 6951
   - Verify old polling stopped
   - Verify new behavior

### Manual Test Scenarios

| Scenario | Action | Expected Result |
|----------|--------|----------------|
| **Port occupied on startup** | Start app with 6950 occupied | App starts normally, shows "未运行", red NAlert with error |
| **Auto-recovery** | Wait 10s, release port | Service auto-starts, status becomes "运行中", error disappears |
| **Change port** | Change port to 6951 in settings | Stop 6950 polling, try 6951 |
| **New port also occupied** | 6951 also occupied | Show new error (port 6951), start new polling |
| **Restart service** | Click "restart service" button | Stop current polling, retry binding current port |
| **Recovery after window close** | Close window, wait 10s, release port, reopen | Service already running, status shows "运行中" |

## Edge Cases and Considerations

### Concurrent Port Changes

- Add operation lock to prevent race conditions
- Ensure only one polling task runs at a time

### Application Exit During Polling

- Polling tasks auto-abort when AppState is dropped
- No explicit cleanup needed

### Rapid Port Changes

- Each change stops old polling and tries new port
- Last change takes effect

### Performance

- Polling overhead: one `TcpListener::bind()` every 10 seconds
- Memory: one tokio task + a few Mutex
- CPU: nearly zero (most time spent in sleep)

## Files Changed

### Backend Files:
1. `src-tauri/src/proxy/server.rs` - Add `serve_once()`
2. `src-tauri/src/proxy/state.rs` - Extend `AppState`, add polling management
3. `src-tauri/src/lib.rs` - Modify startup logic
4. `src-tauri/src/commands.rs` - Modify `restart_server()`, `set_port()`, add `get_bind_error()`

### Frontend Files:
1. `src/stores/system.ts` - Add `bindError` state
2. `src/views/Dashboard.vue` - Display error messages, add periodic refresh
3. `src/lib/types.ts` - Add `get_bind_error` type definition (optional)

## Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Polling location | Backend (Rust) | Auto-recovery works even when window is closed |
| Polling interval | 10 seconds | Balance between responsiveness and system load |
| Port attempt strategy | Only user-configured port | Clear error messages, no ambiguity |
| Error display location | NAlert + Status card | Both prominent and detailed |
| Startup behavior | Silent failure | Don't disturb user, graceful degradation |
| Auto-recovery | Auto-start service | Transparent to user, best experience |

## Success Criteria

1. ✅ Application doesn't exit when port is occupied
2. ✅ Clear error messages shown in Dashboard
3. ✅ Status displays as "未运行" when port is occupied
4. ✅ Background polling retries every 10 seconds
5. ✅ Service auto-starts when port becomes available
6. ✅ Status updates to "运行中" when service starts
7. ✅ Error messages clear when service starts
8. ✅ Polling stops when service starts successfully
9. ✅ Polling stops when port configuration changes
10. ✅ No resource leaks (tasks cleaned up properly)

## Future Enhancements

Out of scope for this implementation, but potential future improvements:

1. **Configurable polling interval** - Allow users to adjust retry frequency
2. **Manual retry button** - Allow users to trigger immediate retry
3. **Detailed error diagnostics** - Show which process is occupying the port
4. **Polling statistics** - Show how many retry attempts have been made
5. **Notification on success** - Optional toast when service auto-recovers
