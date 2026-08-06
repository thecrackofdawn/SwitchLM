use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::config::catalog::{ensure_catalog, ProviderCatalog};
use crate::config::store::{self, StoreError};
use crate::config::{AppConfig, SecretStore, SecretStoreHandle, BackendKind, UsageCreds};
use crate::proxy::health::{Clock, HealthRegistry, SystemClock};
use crate::usage::UsageCache;

pub struct AppStateInner {
    pub config: RwLock<AppConfig>,
    /// Bundled provider catalog overlaid with user customizations from
    /// custom_provider_desc.json. Real-time mutable via `set_custom_context_size`
    /// (write lock); readers (`recognized_context_size`, `classify_fallback`) take
    /// the read lock.
    pub catalog: RwLock<ProviderCatalog>,
    pub secrets: SecretStoreHandle,
    /// Per-model circuit-breaker state (runtime-only, not persisted).
    pub health: HealthRegistry,
    /// Injectable wall-clock; production uses `SystemClock`, tests use `FakeClock`.
    pub clock: Arc<dyn Clock>,
    /// Best-effort usage snapshot cache (TTL ~60s).
    pub usage_cache: UsageCache,
    /// Port the proxy is currently bound to (`None` until `serve()` stores it). Reported by
    /// `get_port`/`get_env_snippet`; warns when it differs from `settings.port`.
    pub bound_port: Mutex<Option<u16>>,
    /// Handle to the running axum server task, so `restart_server` can stop it
    /// (abort + await drops the listener, freeing the port before rebind).
    pub server_handle: Mutex<Option<JoinHandle<()>>>,
    /// Port binding error message (stored when bind fails, cleared on success).
    pub bind_error: Mutex<Option<String>>,
    /// Handle to the port polling task (so we can abort it when stopping/restarting).
    pub polling_handle: Mutex<Option<JoinHandle<()>>>,
    /// provider_id of the model that last produced a response - the "currently effective" plan.
    /// Drives the tray-icon tooltip (single-line balance of the active plan) instead of listing
    /// every plan (which overflows the Windows 64-char tray-tooltip limit). `None` until the
    /// first request produces a response. Runtime-only, not persisted.
    pub last_served_provider: Mutex<Option<String>>,
}

pub type AppState = Arc<AppStateInner>;

impl AppStateInner {
    pub fn load(dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self, StoreError> {
        let mut cfg = store::load(dir)?;
        if store::normalize_legacy_vendors(&mut cfg) {
            store::save(dir, &cfg)?;
        }
        let catalog = RwLock::new(ensure_catalog(dir));
        Ok(Self {
            config: RwLock::new(cfg),
            catalog,
            secrets: SecretStoreHandle::new(secrets, BackendKind::Keyring),
            health: HealthRegistry::default(),
            clock: Arc::new(SystemClock),
            usage_cache: UsageCache::default(),
            bound_port: Mutex::new(None),
            server_handle: Mutex::new(None),
            bind_error: Mutex::new(None),
            polling_handle: Mutex::new(None),
            last_served_provider: Mutex::new(None),
        })
    }

    /// Store the port the proxy bound to (called after `serve()` returns).
    pub fn set_bound_port(&self, port: u16) {
        *self.bound_port.lock().unwrap() = Some(port);
    }

    /// The bound port (`None` until `serve()` stores it).
    pub fn bound_port(&self) -> Option<u16> {
        *self.bound_port.lock().unwrap()
    }

    /// Mark the proxy as not bound (e.g. after stopping the server for a restart).
    pub fn clear_bound_port(&self) {
        *self.bound_port.lock().unwrap() = None;
    }

    /// Take ownership of the current server task handle (for restart/shutdown).
    pub fn take_server_handle(&self) -> Option<JoinHandle<()>> {
        self.server_handle.lock().unwrap().take()
    }

    /// Store the handle of the currently running server task.
    pub fn set_server_handle(&self, handle: JoinHandle<()>) {
        *self.server_handle.lock().unwrap() = Some(handle);
    }

    /// Read a provider's usage credentials: `access_key_id` from config + `secret_access_key`
    /// and `cookie` from the OS keyring (neither secret is ever persisted in `app_config.json`).
    /// Returns `None` only if the provider is unknown; otherwise returns `Some` (the creds may be
    /// all-None — each adapter checks the credential it needs, e.g. Volcengine needs AK+SK,
    /// Qianwen needs the cookie). Best-effort on the keyring reads: a keyring error is treated as
    /// "no secret" (caller degrades), matching breaker semantics.
    pub async fn usage_creds(&self, provider_id: &str) -> Option<UsageCreds> {
        let provider = {
            let cfg = self.config.read().await;
            cfg.providers.iter().find(|p| p.id == provider_id)?.clone()
        };
        let mut creds = provider.usage_creds.unwrap_or_default();
        creds.secret_access_key = self.secrets.get_usage_sk(provider_id).ok().flatten();
        creds.cookie = self.secrets.get_usage_cookie(provider_id).ok().flatten();
        Some(creds) // may be all-None; each adapter checks the credential it needs
    }

    /// Start polling task (if not already running)
    pub async fn start_polling(self: Arc<Self>, port: u16) {
        let mut handle = self.polling_handle.lock().unwrap();
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
        let mut handle = self.polling_handle.lock().unwrap();
        if let Some(task) = handle.take() {
            task.abort();
        }
    }

    /// Polling loop - retries every 10 seconds
    async fn polling_loop(state: Arc<AppStateInner>, port: u16) {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;

            // Try to bind and start
            match crate::proxy::server::serve_once(state.clone(), port).await {
                Ok((handle, bound_port)) => {
                    // Success: update state
                    state.set_server_handle(handle);
                    state.set_bound_port(bound_port);
                    state.clear_bind_error();
                    state.stop_polling().await;
                    tracing::info!("自动恢复成功：服务已在端口 {bound_port} 上启动");
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
    pub fn set_bind_error(&self, error: String) {
        *self.bind_error.lock().unwrap() = Some(error);
    }

    /// Clear binding error
    pub fn clear_bind_error(&self) {
        *self.bind_error.lock().unwrap() = None;
    }

    /// Get binding error
    pub fn get_bind_error(&self) -> Option<String> {
        self.bind_error.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MemoryStore, Provider, Settings};
    use tempfile::tempdir;

    #[tokio::test]
    async fn load_builds_state_from_config() {
        let dir = tempdir().unwrap();
        let cfg = AppConfig {
            settings: Settings { port: 7010, ..Default::default() },
            ..Default::default()
        };
        crate::config::store::save(dir.path(), &cfg).unwrap();

        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let state = AppStateInner::load(dir.path(), secrets).unwrap();
        let guard = state.config.read().await;
        assert_eq!(guard.settings.port, 7010);

        // load() exposes the in-memory embedded catalog (a custom_provider_desc.json would be
        // overlaid here if present).
        assert_eq!(state.catalog.read().await.context_size("zhipu", "glm-4.6"), Some(200000));
    }

    #[test]
    fn bound_port_stores_and_reports() {
        let dir = tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let state = AppStateInner::load(dir.path(), secrets).unwrap();
        assert_eq!(state.bound_port(), None);
        state.set_bound_port(6950);
        assert_eq!(state.bound_port(), Some(6950));
    }

    #[test]
    fn new_state_has_empty_bind_error() {
        let dir = tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let state = AppStateInner::load(dir.path(), secrets).unwrap();

        assert!(state.bind_error.lock().unwrap().is_none());
        assert!(state.polling_handle.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn polling_starts_and_stops() {
        let dir = tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let state = Arc::new(AppStateInner::load(dir.path(), secrets).unwrap());

        // Start polling
        state.clone().start_polling(6950).await;

        // Verify polling handle exists
        {
            let handle = state.polling_handle.lock().unwrap();
            assert!(handle.is_some());
        }

        // Stop polling
        state.stop_polling().await;

        // Verify polling handle is cleared
        {
            let handle = state.polling_handle.lock().unwrap();
            assert!(handle.is_none());
        }
    }

    #[tokio::test]
    async fn bind_error_can_be_set_and_cleared() {
        let dir = tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let state = AppStateInner::load(dir.path(), secrets).unwrap();

        // Set error
        state.set_bind_error("Test error".to_string());
        assert_eq!(state.get_bind_error(), Some("Test error".to_string()));

        // Clear error
        state.clear_bind_error();
        assert_eq!(state.get_bind_error(), None);
    }

    #[tokio::test]
    async fn usage_creds_populates_cookie_for_qianwen_without_aksk() {
        // A Qianwen provider has no AK/SK usage creds (cookie-based); the reworked `usage_creds`
        // must still return `Some` and populate the cookie from the keyring, rather than
        // early-returning `None` on the missing SK (which would discard the cookie).
        let dir = tempdir().unwrap();
        let cfg = AppConfig {
            providers: vec![Provider {
                id: "qianwen1".into(),
                vendor: "qianwen-token".into(),
                display_name: "千问".into(),
                openai_base_url: Some("https://dashscope.aliyuncs.com".into()),
                anthropic_base_url: None,
                usage_creds: None,
            }],
            ..Default::default()
        };
        crate::config::store::save(dir.path(), &cfg).unwrap();

        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let state = AppStateInner::load(dir.path(), secrets).unwrap();
        state.secrets.set_usage_cookie("qianwen1", "cna=x; ticket=y").unwrap();
        let creds = state.usage_creds("qianwen1").await.expect("found provider -> Some");
        assert_eq!(creds.cookie.as_deref(), Some("cna=x; ticket=y"));
        assert_eq!(creds.access_key_id, None);
        assert_eq!(creds.secret_access_key, None);
    }
}
