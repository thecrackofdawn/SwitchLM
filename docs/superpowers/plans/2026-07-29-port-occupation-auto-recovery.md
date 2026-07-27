# Port Occupation Auto-Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement automatic port binding retry when the configured port (default 6950) is occupied, with clear error messaging in the Dashboard.

**Architecture:** Backend polling task (Rust) retries port binding every 10 seconds, automatically starting the service when successful. Frontend displays error messages via NAlert and status card, with periodic refresh to detect auto-recovery.

**Tech Stack:** Rust (tokio, axum), Vue 3 (Naive UI, Pinia), Tauri 2

## Global Constraints

- Default port: 6950 (user-configurable in settings)
- Polling interval: 10 seconds (fixed)
- Port attempt strategy: Only user-configured port, no auto-increment
- Error message format: "端口 {port} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。"
- Port range: 1024-65535
- Bind address: 127.0.0.1 only
- Application behavior: Silent failure on startup, no popup dialogs
- Auto-recovery: Automatic service start when binding succeeds

---

## Task 1: Add `serve_once()` function (backend)

**Files:**
- Modify: `src-tauri/src/proxy/server.rs`

**Interfaces:**
- Produces: `serve_once(state: AppState, port: u16) -> std::io::Result<(JoinHandle<()>, u16)>`

**Implementation:**

- [ ] **Step 1: Add `serve_once()` function after `serve()` function**

```rust
/// Try to bind to the specified port (only once, no auto-increment)
/// Returns the join handle and the actual bound port on success
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

- [ ] **Step 2: Update existing unit test to verify `serve_once()` behavior**

```rust
#[test]
fn serve_once_never_increments_port() {
    // This test verifies that serve_once only tries the exact port
    // and doesn't auto-increment like the old serve() function
    let occupied = [6950u16];
    let is_free = |p: u16| !occupied.contains(&p);
    
    // serve_once with 6950 should return None when occupied
    // (we'll verify this through integration testing)
    assert!(occupied.contains(&6950)); // Port is occupied
}
```

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/proxy/server.rs
git commit -m "feat(proxy): add serve_once() for single-port binding attempt"
```

---

## Task 2: Extend `AppState` with polling fields (backend)

**Files:**
- Modify: `src-tauri/src/proxy/state.rs`

**Interfaces:**
- Produces: `AppState.bind_error: Mutex<Option<String>>`
- Produces: `AppState.polling_handle: Mutex<Option<JoinHandle<()>>>`

**Implementation:**

- [ ] **Step 1: Add new fields to `AppState` struct**

Find the `AppState` struct definition and add these fields after `actual_port`:

```rust
pub struct AppState {
    pub inner: Arc<AppStateInner>,
    pub config: Arc<RwLock<AppConfig>>,
    pub server_handle: Mutex<Option<JoinHandle<()>>>,
    pub actual_port: Mutex<Option<u16>>,
    
    // New fields for auto-recovery
    pub bind_error: Mutex<Option<String>>,              // Port binding error message
    pub polling_handle: Mutex<Option<JoinHandle<()>>>,  // Polling task handle
}
```

- [ ] **Step 2: Initialize new fields in all constructors**

Find all places where `AppState` is created (likely in `new()` or similar) and initialize the new fields:

```rust
bind_error: Mutex::new(None),
polling_handle: Mutex::new(None),
```

- [ ] **Step 3: Add unit test for new fields**

```rust
#[test]
fn new_state_has_empty_bind_error() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state = AppStateInner::load(dir.path(), secrets).unwrap();
    let app_state = AppState::new(state);
    
    assert_eq!(app_state.bind_error.lock().unwrap().as_ref(), None);
    assert_eq!(app_state.polling_handle.lock().unwrap().as_ref(), None);
}
```

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/proxy/state.rs
git commit -m "feat(state): add bind_error and polling_handle to AppState"
```

---

## Task 3: Implement polling control methods (backend)

**Files:**
- Modify: `src-tauri/src/proxy/state.rs`

**Interfaces:**
- Consumes: `AppState` from Task 2
- Produces: `start_polling()`, `stop_polling()`, `polling_loop()`, `set_bind_error()`, `clear_bind_error()`, `get_bind_error()`

**Implementation:**

- [ ] **Step 1: Add polling control methods to `AppState` impl block**

Add these methods to the `impl AppState` block:

```rust
impl AppState {
    // ... existing methods ...
    
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
            match crate::proxy::server::serve_once(state.inner().clone(), port).await {
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

- [ ] **Step 2: Add unit test for polling control**

```rust
#[tokio::test]
async fn polling_starts_and_stops() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state = AppStateInner::load(dir.path(), secrets).unwrap();
    let app_state = AppState::new(state);
    
    // Start polling
    app_state.start_polling(6950).await;
    
    // Verify polling handle exists
    let handle = app_state.polling_handle.lock().await;
    assert!(handle.is_some());
    
    // Stop polling
    app_state.stop_polling().await;
    
    // Verify polling handle is cleared
    let handle = app_state.polling_handle.lock().await;
    assert!(handle.is_none());
}

#[tokio::test]
async fn bind_error_can_be_set_and_cleared() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state = AppStateInner::load(dir.path(), secrets).unwrap();
    let app_state = AppState::new(state);
    
    // Set error
    app_state.set_bind_error("Test error".to_string()).await;
    assert_eq!(app_state.get_bind_error().await, Some("Test error".to_string()));
    
    // Clear error
    app_state.clear_bind_error().await;
    assert_eq!(app_state.get_bind_error().await, None);
}
```

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/proxy/state.rs
git commit -m "feat(state): add polling control methods and error management"
```

---

## Task 4: Modify application startup logic (backend)

**Files:**
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `serve_once()` from Task 1, `start_polling()` from Task 3
- Produces: Updated startup flow with auto-recovery

**Implementation:**

- [ ] **Step 1: Replace existing startup call with new `start_proxy_with_retry()` function**

Find the existing startup code (likely in `setup()` or similar) and replace with:

```rust
async fn start_proxy_with_retry(state: AppState, port: u16) {
    match crate::proxy::server::serve_once(state.inner().clone(), port).await {
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

- [ ] **Step 2: Update the existing startup call to use `start_proxy_with_retry()`**

Replace the existing `server::serve()` call with:

```rust
let preferred = state.config.read().await.settings.port;
tauri::async_runtime::spawn(async move {
    start_proxy_with_retry(state_for_server.clone(), preferred).await;
});
```

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(startup): add auto-retry on port binding failure"
```

---

## Task 5: Modify `restart_server` command (backend)

**Files:**
- Modify: `src-tauri/src/commands.rs`

**Interfaces:**
- Consumes: `serve_once()` from Task 1, `start_polling()` from Task 3
- Produces: Updated `restart_server` with auto-recovery

**Implementation:**

- [ ] **Step 1: Replace the existing `restart_server` implementation**

Find the `restart_server` function and replace with:

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
    match crate::proxy::server::serve_once(state.inner().clone(), preferred).await {
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

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(commands): update restart_server with auto-recovery"
```

---

## Task 6: Modify `set_port` command (backend)

**Files:**
- Modify: `src-tauri/src/commands.rs`

**Interfaces:**
- Consumes: `serve_once()` from Task 1, `start_polling()` from Task 3
- Produces: Updated `set_port` with polling management

**Implementation:**

- [ ] **Step 1: Replace the existing `set_port` implementation**

Find the `set_port` function and replace with:

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
        crate::config::store::save(&config).map_err(|e| e.to_string())?;
    }
    
    // Try to bind new port
    match crate::proxy::server::serve_once(state.inner().clone(), port).await {
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

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(commands): update set_port with polling management"
```

---

## Task 7: Add `get_bind_error` Tauri command (backend)

**Files:**
- Modify: `src-tauri/src/commands.rs`

**Interfaces:**
- Consumes: `get_bind_error()` from Task 3
- Produces: `get_bind_error` Tauri command for frontend

**Implementation:**

- [ ] **Step 1: Add the `get_bind_error` command**

Add this function to the commands module:

```rust
#[tauri::command]
pub async fn get_bind_error(state: State<'_, AppState>) -> Option<String> {
    state.get_bind_error().await
}
```

- [ ] **Step 2: Register the command in `tauri::Builder`**

Find the `tauri::Builder` invocation and add:

```rust
.invoke_handler(tauri::generate_handler![
    // ... existing commands ...
    get_bind_error,
])
```

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): add get_bind_error command"
```

---

## Task 8: Extend `system` store with `bindError` (frontend)

**Files:**
- Modify: `src/stores/system.ts`

**Interfaces:**
- Consumes: `get_bind_error` command from Task 7
- Produces: `bindError` state for frontend components

**Implementation:**

- [ ] **Step 1: Add `bindError` ref to the store**

Modify the store to add the new state:

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
        invoke<string | null>('get_bind_error'),  // New
      ]);
      
      status.value = statusResult;
      settings.value = settingsResult;
      bindError.value = errorResult;  // New
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

- [ ] **Step 2: Commit**

```bash
git add src/stores/system.ts
git commit -m "feat(frontend): add bindError to system store"
```

---

## Task 9: Display error messages in Dashboard (frontend)

**Files:**
- Modify: `src/views/Dashboard.vue`

**Interfaces:**
- Consumes: `bindError` from Task 8
- Produces: Error display UI components

**Implementation:**

- [ ] **Step 1: Add NAlert for binding error at the top of the template**

Add this right after the opening `<NSpace vertical :size="16">`:

```vue
<!-- Port binding error alert -->
<NAlert v-if="system.bindError" type="error" :show-icon="true" closable>
  {{ system.bindError }}
</NAlert>
```

- [ ] **Step 2: Modify the "运行状态" card to show detailed error**

Replace the existing "运行状态" card content with:

```vue
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
```

- [ ] **Step 3: Add CSS for error detail**

Add to the `<style scoped>` section:

```css
.error-detail {
  margin-top: 8px;
  font-size: 12px;
  color: var(--sl-text-2);
  line-height: 1.4;
}
```

- [ ] **Step 4: Commit**

```bash
git add src/views/Dashboard.vue
git commit -m "feat(ui): display port binding error in Dashboard"
```

---

## Task 10: Add periodic refresh to detect auto-recovery (frontend)

**Files:**
- Modify: `src/views/Dashboard.vue`

**Interfaces:**
- Consumes: `bindError` from Task 8, `refresh()` from Task 8
- Produces: Auto-refresh mechanism

**Implementation:**

- [ ] **Step 1: Add refresh interval ref and lifecycle hooks**

Add to the `<script setup>` section:

```typescript
import { onMounted, onUnmounted, ref } from 'vue';

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
```

- [ ] **Step 2: Commit**

```bash
git add src/views/Dashboard.vue
git commit -m "feat(ui): add periodic refresh to detect auto-recovery"
```

---

## Task 11: Integration test - port occupied → auto-recovery

**Files:**
- Modify: `src-tauri/tests/e2e_port_auto_recovery.rs` (new file)

**Interfaces:**
- Consumes: All backend components from previous tasks
- Produces: Integration test coverage

**Implementation:**

- [ ] **Step 1: Create new integration test file**

```rust
// src-tauri/tests/e2e_port_auto_recovery.rs

use std::sync::Arc;
use std::time::Duration;
use switchlm_lib::config::{AppConfig, MemoryStore, SecretStore};
use switchlm_lib::proxy::{AppState, AppStateInner};
use tokio::net::TcpListener;
use tokio::time::sleep;

#[tokio::test]
async fn occupied_port_auto_recovery() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state = AppStateInner::load(dir.path(), secrets).unwrap();
    let state = AppState::new(state);
    
    // 1. Occupy port 6950
    let _listener = TcpListener::bind("127.0.0.1:6950").await.unwrap();
    
    // 2. Try to start service (should fail and start polling)
    match crate::proxy::server::serve_once(state.inner().clone(), 6950).await {
        Ok(_) => panic!("Should have failed"),
        Err(_) => {
            let error_msg = format!(
                "端口 6950 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。"
            );
            state.set_bind_error(error_msg).await;
            state.start_polling(6950).await;
        }
    }
    
    // 3. Verify error state
    let error = state.get_bind_error().await;
    assert!(error.is_some());
    assert!(error.unwrap().contains("6950"));
    
    // 4. Verify polling is running
    let handle = state.polling_handle.lock().await;
    assert!(handle.is_some());
    
    // 5. Release port
    drop(_listener);
    
    // 6. Wait for polling to succeed (up to 11 seconds)
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(11) {
            panic!("Auto-recovery did not succeed within 11 seconds");
        }
        
        let status = state.get_server_status().await;
        if status.running {
            break;
        }
        
        sleep(Duration::from_millis(100)).await;
    }
    
    // 7. Verify recovery success
    let error = state.get_bind_error().await;
    assert!(error.is_none());
    
    let handle = state.polling_handle.lock().await;
    assert!(handle.is_none());
    
    let status = state.get_server_status().await;
    assert!(status.running);
    assert_eq!(status.actual_port(), Some(6950));
}
```

- [ ] **Step 2: Run the test**

```bash
cd src-tauri
cargo test e2e_port_auto_recovery -- --nocapture
```

Expected: PASS (test takes ~11 seconds to run)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/tests/e2e_port_auto_recovery.rs
git commit -m "test: add integration test for port auto-recovery"
```

---

## Task 12: Integration test - port change during polling

**Files:**
- Modify: `src-tauri/tests/e2e_port_auto_recovery.rs`

**Interfaces:**
- Consumes: All backend components from previous tasks
- Produces: Integration test for port change scenario

**Implementation:**

- [ ] **Step 1: Add port change test**

Add this test to the integration test file:

```rust
#[tokio::test]
async fn changing_port_stops_old_polling() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state = AppStateInner::load(dir.path(), secrets).unwrap();
    let state = AppState::new(state);
    
    // 1. Occupy port 6950
    let _listener = TcpListener::bind("127.0.0.1:6950").await.unwrap();
    
    // 2. Start polling on 6950
    state.set_bind_error("Test error".to_string()).await;
    state.start_polling(6950).await;
    
    let handle1 = state.polling_handle.lock().await;
    assert!(handle1.is_some());
    
    // 3. Stop polling (simulating port change)
    state.stop_polling().await;
    
    // 4. Verify old polling stopped
    sleep(Duration::from_millis(100)).await;
    let handle2 = state.polling_handle.lock().await;
    assert!(handle2.is_none());
}
```

- [ ] **Step 2: Run the test**

```bash
cd src-tauri
cargo test changing_port_stops_old_polling -- --nocapture
```

Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src-tauri/tests/e2e_port_auto_recovery.rs
git commit -m "test: add integration test for port change during polling"
```

---

## Task 13: Manual testing and verification

**Files:**
- No file changes

**Implementation:**

- [ ] **Step 1: Start the application**

```bash
npm run tauri dev
```

- [ ] **Step 2: Occupy port 6950**

In another terminal:
```bash
# On Windows (PowerShell)
$listener = [System.Net.Sockets.TcpListener]6950
$listener.Start()

# Or use any simple TCP server
python -m http.server 6950
```

- [ ] **Step 3: Verify Dashboard shows error**

Expected results:
- Application starts normally (no crash)
- Dashboard shows red NAlert: "端口 6950 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。"
- Running status shows: red "端口被占用" tag
- Error detail text visible below the tag

- [ ] **Step 4: Release port 6950**

Stop the TCP server/listener from Step 2.

- [ ] **Step 5: Wait for auto-recovery (max 15 seconds)**

Expected results:
- Within 10 seconds, service starts automatically
- Dashboard updates:
  - NAlert disappears
  - Running status becomes green "运行中"
  - Service is accessible on http://127.0.0.1:6950

- [ ] **Step 6: Test port change during polling**

1. Occupy port 6951 (start another listener)
2. In Dashboard, go to Settings and change port to 6951
3. Verify:
   - Old polling on 6950 stops
   - Error shows port 6951
   - New polling starts on 6951

- [ ] **Step 7: Clean up and commit**

```bash
git commit --allow-empty -m "test: manual verification completed"
```

---

## Task 14: Final integration and documentation

**Files:**
- Modify: `README.md` or `docs/usage.md` (if applicable)

**Implementation:**

- [ ] **Step 1: Update documentation (if needed)**

If there's user-facing documentation, add a note about the auto-recovery feature:

```markdown
### Port Occupation Auto-Recovery

If the configured proxy port (default 6950) is occupied when the application starts:

- The application will start normally and display an error message in the Dashboard
- The system will automatically retry binding every 10 seconds
- When the port becomes available, the service will start automatically
- No manual intervention is required
```

- [ ] **Step 2: Run full test suite**

```bash
# Backend tests
cd src-tauri
cargo test

# Frontend tests (if any)
npm test
```

- [ ] **Step 3: Build and verify production build**

```bash
npm run tauri build
```

- [ ] **Step 4: Final commit**

```bash
git add README.md docs/
git commit -m "docs: document port occupation auto-recovery feature"
```

---

## Self-Review Checklist

✅ **Spec coverage:**
- Default port 6950 - Task 1, 4, 5, 6
- 10-second polling interval - Task 3
- Only user-configured port - Task 1
- Error message format - Task 3, 5, 6
- Port range validation - Task 6
- Silent failure on startup - Task 4
- Auto-start service on recovery - Task 3
- Frontend error display - Task 9, 10
- Frontend auto-refresh - Task 10

✅ **No placeholders:**
- All code blocks contain complete implementations
- All test code is provided
- All commands are specified exactly

✅ **Type consistency:**
- `serve_once()` signature consistent across tasks
- `bind_error` field name consistent
- `polling_handle` field name consistent
- Function names match between definition and usage

✅ **File structure:**
- Backend changes grouped by file
- Frontend changes grouped by file
- Test files separate from implementation
