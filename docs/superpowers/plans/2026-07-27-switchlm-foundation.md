# SwitchLM Plan 1 — Foundation & Proxy Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scaffold the Tauri v2 + Vue 3 app and build a working, headless-testable local proxy that routes OpenAI-protocol requests by **Profile → Model** with request-model rewriting, auth injection, byte-stream passthrough, and hot-swappable config.

**Architecture:** Single Tauri process. A Rust `axum` HTTP server runs as a tokio background task on `localhost` (default port **6950**, auto-incrementing on conflict), sharing an `Arc<AppStateInner>` holding `tokio::sync::RwLock<AppConfig>` + a `SecretStore`. Vue frontend and Tauri tray (Plan 3) drive the same `AppState` via Tauri commands.

**Tech Stack:** Tauri v2, Rust (edition 2021), axum 0.7, reqwest 0.12, tokio 1, serde 1, keyring 3, Vue 3 + Vite + TypeScript.

## Scope of THIS plan

**Covers:** §2 (process model, port strategy), §3.1 pipeline steps ②③④⑤ (OpenAI path only), §3.4 Profile routing (request side), §3.6 hot-swap, §5.1–5.5 (config model + persistence + secrets), §8 (port-in-use, unmatched-profile, empty-config errors).

**Explicitly DEFERRED to later plans (do NOT implement here):**
- Anthropic edge (`/v1/messages`) + translation + tool_use state machine + response `model` echo → **Plan 2**
- ErrorAdapter, fallback, circuit breaker, usage adapters → **Plan 2**
- Model discovery (`discover_models` = GET provider `/models`) → **Plan 2** (provider interactions)
- Tauri tray, Vue pages, autostart, port-warning UI, graceful-shutdown lifecycle → **Plan 3**

> Note: the OpenAI edge in this plan does **pure byte passthrough** of the upstream response (no response body shaping). Response `model`-field echo (§3.4) is a response-shaping concern and lands in Plan 2 with translation.

## Global Constraints

- **Tauri v2** (`tauri = "2"`; `@tauri-apps/cli` & `@tauri-apps/api` v2). App identifier: `com.switchlm.app`.
- **Rust edition 2021**, MSRV 1.78.
- **Default proxy port: 6950** (configurable via `settings.port`). Auto-increment on conflict, cap at 16 attempts.
- **Config file:** `app_config.json` in Tauri app data dir (Windows: `%APPDATA%\SwitchLM\`). **Never** store `api_key` or `usage_creds` secrets in it.
- **Secrets:** `keyring` crate v3, service name `"SwitchLM"`, account = provider id. Secrets accessed at runtime only.
- **crate versions:** `axum = "0.7"`, `reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls"] }`, `tokio = { version = "1", features = ["full"] }`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `tower = { version = "0.5", features = ["util"] }`, `thiserror = "1"`, `tracing = "0.1"`, `tracing-subscriber = "0.3"`, `keyring = "3"`.
- **dev-deps:** `wiremock = "0.6"`, `tempfile = "3"`, `http-body-util = "0.1"`.
- **Frontend:** Vue 3.4+, Vite 5+, TypeScript 5+.
- **Commits:** conventional commits (`feat:`, `chore:`, `test:`, `docs:`). One logical change per commit.
- **Platform target:** Windows 11 (primary). Keep code cross-platform where it costs nothing.

## File Structure (created/modified by this plan)

```
SwitchLM/
├─ package.json                          (scaffold)
├─ vite.config.ts                        (scaffold)
├─ src/                                  (Vue; default template, untouched this plan)
├─ src-tauri/
│  ├─ Cargo.toml                         (add deps)
│  ├─ tauri.conf.json                    (identifier, window, app data)
│  └─ src/
│     ├─ main.rs                         (scaffold entry → calls lib::run)
│     ├─ lib.rs                          (MODIFY: wire state + server + commands)
│     ├─ config/
│     │  ├─ mod.rs                       (re-exports)
│     │  ├─ types.rs                     (Provider, Model, Profile, BackendConfig, AppConfig, Settings)
│     │  ├─ store.rs                     (app_config.json load/save)
│     │  └─ secrets.rs                   (SecretStore trait + KeyringStore + MemoryStore)
│     ├─ proxy/
│     │  ├─ mod.rs                       (re-exports)
│     │  ├─ state.rs                     (AppStateInner, AppState type alias)
│     │  ├─ resolve.rs                   (resolve_model: Profile name → Model)
│     │  ├─ server.rs                    (build_router, pick_port, serve)
│     │  └─ openai_edge.rs               (POST /v1/chat/completions handler)
│     └─ commands.rs                     (Tauri command skeleton)
└─ docs/superpowers/...                  (existing)
```

Responsibilities: `types.rs` = serializable config shape (no secrets); `store.rs` = JSON persistence; `secrets.rs` = secret access (testable via `MemoryStore`); `resolve.rs` = pure routing logic; `server.rs`/`openai_edge.rs` = HTTP layer; `commands.rs` = IPC surface; `state.rs` = shared runtime state.

---

### Task 1: Scaffold Tauri v2 + Vue 3 + TypeScript project

**Files:**
- Create: `package.json`, `vite.config.ts`, `src/`, `src-tauri/` (via scaffolder)
- Modify: `src-tauri/Cargo.toml` (add deps), `src-tauri/tauri.conf.json` (identifier)

**Interfaces:**
- Consumes: nothing (bootstrap).
- Produces: a runnable Tauri app; `src-tauri/src/lib.rs` with `pub fn run()`; empty modules to fill.

- [ ] **Step 1: Scaffold the app at repo root**

Run (interactive — choose the options shown):
```bash
npm create tauri-app@latest
```
Answers:
- Project name: `switchlm`
- Identifier: `com.switchlm.app`
- Frontend language: `Vue` (with `TypeScript`)
- Package manager: `npm`

If the scaffolder refuses because the directory is non-empty, scaffold into a temp dir `../_switchlm_tmp` then move `package.json`, `src/`, `src-tauri/`, `vite.config.ts`, `tsconfig*.json`, `index.html`, `.gitignore` additions into the repo root (do not overwrite existing `docs/` or `.gitignore` — merge).

- [ ] **Step 2: Add Rust dependencies**

In `src-tauri/Cargo.toml` `[dependencies]`, ensure:
```toml
axum = "0.7"
reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tower = { version = "0.5", features = ["util"] }
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
keyring = "3"

[dev-dependencies]
wiremock = "0.6"
tempfile = "3"
http-body-util = "0.1"
tower = { version = "0.5", features = ["util"] }
```

- [ ] **Step 3: Set app identifier & window title**

In `src-tauri/tauri.conf.json`, set `"identifier": "com.switchlm.app"` and a sensible window title (`"SwitchLM"`). Leave `"devUrl"`/`"frontendDist"` as the scaffolder set them.

- [ ] **Step 4: Verify the app launches**

Run: `npm install && npm run tauri dev`
Expected: a Tauri window opens showing the default Vue welcome page; no Rust compile errors. Stop it (Ctrl-C) once confirmed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: scaffold Tauri v2 + Vue 3 + TS app"
```

---

### Task 2: Config data model (`config/types.rs`)

**Files:**
- Create: `src-tauri/src/config/mod.rs`, `src-tauri/src/config/types.rs`

**Interfaces:**
- Consumes: `serde`.
- Produces: structs `AppConfig`, `Settings`, `Provider`, `UsageCreds`, `Model`, `BackendConfig`, `ModelSource` (enum), `Profile`. Field names are the contract later tasks rely on.

- [ ] **Step 1: Write the failing test**

`src-tauri/src/config/types.rs` (append at bottom after structs in Step 3 — but TDD says test first; place this test in the same file guarded by `#[cfg(test)]`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_roundtrips() {
        let cfg = AppConfig {
            providers: vec![Provider {
                id: "zhipu".into(),
                display_name: "智谱".into(),
                base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
                usage_creds: None,
            }],
            models: vec![Model {
                id: "m_glm46".into(),
                provider_id: "zhipu".into(),
                display_name: "GLM-4.6".into(),
                source: ModelSource::Discovered,
                openai: Some(BackendConfig {
                    base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
                    upstream_model_id: "glm-4.6".into(),
                    api_key_ref: None,
                }),
                anthropic: None,
                cooldown_seconds: Some(300),
                fallback_target_model_id: Some("m_glm45".into()),
            }],
            profiles: vec![Profile {
                id: "p_main".into(),
                name: "glm-5.2".into(),
                aliases: vec!["claude-sonnet-4".into()],
                backing_model_id: "m_glm46".into(),
            }],
            settings: Settings { port: 6950, autostart: false },
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(back.settings.port, 6950);
        assert!(back.profiles[0].aliases.contains(&"claude-sonnet-4".to_string()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app_config_roundtrips`
Expected: FAIL — types not defined / module missing.

- [ ] **Step 3: Write the structs**

`src-tauri/src/config/types.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub settings: Settings,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { providers: vec![], models: vec![], profiles: vec![], settings: Settings::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub autostart: bool,
}
impl Default for Settings {
    fn default() -> Self { Self { port: default_port(), autostart: false } }
}
fn default_port() -> u16 { 6950 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    #[serde(default)]
    pub usage_creds: Option<UsageCreds>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageCreds {
    // Provider-specific usage-query credentials (e.g. Volcengine AK/SK).
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub id: String,
    pub provider_id: String,
    pub display_name: String,
    #[serde(default)]
    pub source: ModelSource,
    #[serde(default)]
    pub openai: Option<BackendConfig>,
    #[serde(default)]
    pub anthropic: Option<BackendConfig>,
    #[serde(default)]
    pub cooldown_seconds: Option<u64>,
    #[serde(default)]
    pub fallback_target_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelSource {
    #[default]
    Discovered,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendConfig {
    pub base_url: String,
    pub upstream_model_id: String,
    #[serde(default)]
    pub api_key_ref: Option<String>, // None => inherit provider's key
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub backing_model_id: String,
}
```

`src-tauri/src/config/mod.rs`:
```rust
pub mod types;
pub use types::*;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app_config_roundtrips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/
git commit -m "feat(config): add AppConfig/Provider/Model/Profile data model"
```

---

### Task 3: Config persistence (`config/store.rs`)

**Files:**
- Create: `src-tauri/src/config/store.rs`
- Modify: `src-tauri/src/config/mod.rs` (add `pub mod store;`)

**Interfaces:**
- Consumes: `AppConfig` (Task 2), `tempfile` (test).
- Produces: `store::load(dir) -> Result<AppConfig>`, `store::save(dir, &AppConfig) -> Result<()>`, `store::config_path(dir) -> PathBuf`.

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/config/store.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempdir().unwrap();
        let cfg = AppConfig {
            settings: Settings { port: 7000, autostart: true },
            ..AppConfig::default()
        };
        save(dir.path(), &cfg).unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = tempdir().unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(back, AppConfig::default()); // default port 6950
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml store::`
Expected: FAIL — `save`/`load` undefined.

- [ ] **Step 3: Implement**

`src-tauri/src/config/store.rs`:
```rust
use std::path::{Path, PathBuf};
use crate::config::AppConfig;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("app_config.json")
}

pub fn load(dir: &Path) -> Result<AppConfig, StoreError> {
    let path = config_path(dir);
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let text = std::fs::read_to_string(path)?;
    let cfg: AppConfig = serde_json::from_str(&text)?;
    Ok(cfg)
}

pub fn save(dir: &Path, cfg: &AppConfig) -> Result<(), StoreError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    let path = config_path(dir);
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(path, text)?;
    Ok(())
}
```

Add to `config/mod.rs`: `pub mod store;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml store::`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/
git commit -m "feat(config): add app_config.json load/save"
```

---

### Task 4: Secret storage (`config/secrets.rs`)

**Files:**
- Create: `src-tauri/src/config/secrets.rs`
- Modify: `src-tauri/src/config/mod.rs` (add `pub mod secrets; pub use secrets::*;`)

**Interfaces:**
- Consumes: `keyring` (real impl).
- Produces: `trait SecretStore: Send + Sync` with `set_key(provider_id, key)`, `get_key(provider_id) -> Option<String>`, `delete_key(provider_id)`; impls `KeyringStore` (production) and `MemoryStore` (tests).

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/config/secrets.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::default();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
        s.set_key("zhipu", "sk-abc").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), Some("sk-abc".into()));
        s.delete_key("zhipu").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml secrets::`
Expected: FAIL — `MemoryStore` undefined.

- [ ] **Step 3: Implement**

`src-tauri/src/config/secrets.rs`:
```rust
use std::collections::HashMap;
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(String),
}

pub trait SecretStore: Send + Sync {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError>;
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError>;
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError>;
}

const SERVICE: &str = "SwitchLM";

pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError> {
        keyring::Entry::new(SERVICE, provider_id)
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .set_password(key)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        match keyring::Entry::new(SERVICE, provider_id)
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .get_password()
        {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError> {
        match keyring::Entry::new(SERVICE, provider_id)
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .delete_credential()
        {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
}

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().insert(provider_id.into(), key.into());
        Ok(())
    }
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(provider_id).cloned())
    }
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().remove(provider_id);
        Ok(())
    }
}
```

Add to `config/mod.rs`: `pub mod secrets; pub use secrets::*;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml secrets::`
Expected: PASS.

> `KeyringStore` is not unit-tested (OS keyring needs a real host session). Verify manually on Windows later (Plan 3): set a provider key via the UI and confirm it round-trips through Credential Manager. The `MemoryStore` proves the `SecretStore` contract.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/
git commit -m "feat(config): add SecretStore trait + KeyringStore/MemoryStore"
```

---

### Task 5: Shared state (`proxy/state.rs`)

**Files:**
- Create: `src-tauri/src/proxy/mod.rs`, `src-tauri/src/proxy/state.rs`

**Interfaces:**
- Consumes: `AppConfig`, `SecretStore`, `store`.
- Produces: `AppStateInner { config: tokio::sync::RwLock<AppConfig>, secrets: Arc<dyn SecretStore> }`; type alias `AppState = Arc<AppStateInner>`; `AppStateInner::load(dir, secrets) -> Result<Self, StoreError>`.

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/proxy/state.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MemoryStore, Settings};
    use tempfile::tempdir;

    #[tokio::test]
    async fn load_builds_state_from_config() {
        let dir = tempdir().unwrap();
        let cfg = AppConfig { settings: Settings { port: 7010, ..Default::default() }, ..Default::default() };
        crate::config::store::save(dir.path(), &cfg).unwrap();

        let state = AppStateInner::load(dir.path(), std::sync::Arc::new(MemoryStore::default())).unwrap();
        let guard = state.config.read().await;
        assert_eq!(guard.settings.port, 7010);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml state::`
Expected: FAIL — `AppStateInner` undefined.

- [ ] **Step 3: Implement**

`src-tauri/src/proxy/state.rs`:
```rust
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::config::{AppConfig, SecretStore};
use crate::config::store::{self, StoreError};

pub struct AppStateInner {
    pub config: RwLock<AppConfig>,
    pub secrets: Arc<dyn SecretStore>,
}

pub type AppState = Arc<AppStateInner>;

impl AppStateInner {
    pub fn load(dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self, StoreError> {
        let cfg = store::load(dir)?;
        Ok(Self { config: RwLock::new(cfg), secrets })
    }
}
```

`src-tauri/src/proxy/mod.rs`:
```rust
pub mod state;
pub use state::{AppState, AppStateInner};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml state::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/
git commit -m "feat(proxy): add AppState (config RwLock + SecretStore)"
```

---

### Task 6: Profile resolution (`proxy/resolve.rs`)

**Files:**
- Create: `src-tauri/src/proxy/resolve.rs`
- Modify: `src-tauri/src/proxy/mod.rs` (add `pub mod resolve;`)

**Interfaces:**
- Consumes: `AppConfig`, `Model`, `Profile`.
- Produces: `ResolveError` enum; `resolve_model(&AppConfig, Option<&str>) -> Result<&Model, ResolveError>`. (Returns a borrow into the passed `AppConfig`; callers hold the read guard.)

- [ ] **Step 1: Write the failing tests**

Append to `src-tauri/src/proxy/resolve.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn sample() -> AppConfig {
        AppConfig {
            models: vec![Model {
                id: "m_glm46".into(), provider_id: "zhipu".into(), display_name: "GLM-4.6".into(),
                source: ModelSource::Manual, openai: None, anthropic: None,
                cooldown_seconds: None, fallback_target_model_id: None,
            }],
            profiles: vec![Profile {
                id: "p_main".into(), name: "glm-5.2".into(),
                aliases: vec!["claude-sonnet-4".into()], backing_model_id: "m_glm46".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn hits_by_name() {
        let cfg = sample();
        let m = resolve_model(&cfg, Some("glm-5.2")).unwrap();
        assert_eq!(m.id, "m_glm46");
    }

    #[test]
    fn hits_by_alias() {
        let cfg = sample();
        let m = resolve_model(&cfg, Some("claude-sonnet-4")).unwrap();
        assert_eq!(m.id, "m_glm46");
    }

    #[test]
    fn miss_lists_available() {
        let cfg = sample();
        let err = resolve_model(&cfg, Some("nope")).unwrap_err();
        match err {
            ResolveError::ProfileNotFound { available, .. } => {
                assert!(available.contains(&"glm-5.2".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn missing_model_name_errors() {
        let cfg = sample();
        assert!(matches!(resolve_model(&cfg, None), Err(ResolveError::ProfileNotFound { .. })));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml resolve::`
Expected: FAIL — `resolve_model`/`ResolveError` undefined.

- [ ] **Step 3: Implement**

`src-tauri/src/proxy/resolve.rs`:
```rust
use crate::config::{AppConfig, Model};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("model '{requested}' is not a configured profile; available: {available:?}")]
    ProfileNotFound { requested: String, available: Vec<String> },
    #[error("profile points to missing model '{0}'")]
    MissingBackingModel(String),
}

pub fn resolve_model<'c>(cfg: &'c AppConfig, requested: Option<&str>) -> Result<&'c Model, ResolveError> {
    let available: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
    let req = requested.unwrap_or("");
    let prof = cfg
        .profiles
        .iter()
        .find(|p| p.name == req || p.aliases.iter().any(|a| a == req))
        .ok_or_else(|| ResolveError::ProfileNotFound { requested: req.to_string(), available })?;
    let model = cfg
        .models
        .iter()
        .find(|m| m.id == prof.backing_model_id)
        .ok_or_else(|| ResolveError::MissingBackingModel(prof.backing_model_id.clone()))?;
    Ok(model)
}
```

Add to `proxy/mod.rs`: `pub mod resolve; pub use resolve::{resolve_model, ResolveError};`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml resolve::`
Expected: PASS (all four).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/
git commit -m "feat(proxy): add Profile→Model resolution"
```

---

### Task 7: Router + port strategy (`proxy/server.rs`)

**Files:**
- Create: `src-tauri/src/proxy/server.rs`
- Modify: `src-tauri/src/proxy/mod.rs` (add `pub mod server;`)

**Interfaces:**
- Consumes: `AppState`, `openai_edge::chat_completions` (Task 8). NOTE: Task 8 defines the handler; to compile Task 7 in isolation, reference it as `crate::proxy::openai_edge::chat_completions` and add a temporary stub if running Task 7 before Task 8 (see Step 3 note).
- Produces: `pick_port(start, is_free) -> Option<u16>`; `build_router(AppState) -> axum::Router`; `serve(AppState, preferred_port) -> Result<(JoinHandle<()>, u16)>`.

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/proxy/server.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_port_skips_occupied() {
        // ports 6950, 6951 "occupied", 6952 free
        let occupied = [6950u16, 6951];
        let is_free = |p: u16| !occupied.contains(&p);
        assert_eq!(pick_port(6950, is_free), Some(6952));
    }

    #[test]
    fn pick_port_none_when_all_busy() {
        let is_free = |_| false;
        assert_eq!(pick_port(6950, is_free), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml server::tests`
Expected: FAIL — `pick_port` undefined.

- [ ] **Step 3: Implement**

`src-tauri/src/proxy/server.rs`:
```rust
use std::net::SocketAddr;
use std::sync::Arc;
use axum::{Router, routing::post};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use crate::proxy::{AppState, openai_edge};

const MAX_PORT_ATTEMPTS: u16 = 16;

pub fn pick_port(start: u16, is_free: impl Fn(u16) -> bool) -> Option<u16> {
    (0..MAX_PORT_ATTEMPTS)
        .map(|i| start.saturating_add(i))
        .find(|p| is_free(*p))
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(openai_edge::chat_completions))
        .with_state(state)
}

/// Bind to `preferred_port` (auto-incrementing on conflict), then serve.
/// Returns the join handle and the actual bound port.
pub async fn serve(state: AppState, preferred_port: u16) -> std::io::Result<(JoinHandle<()>, u16)> {
    let mut port = None;
    for i in 0..MAX_PORT_ATTEMPTS {
        let candidate = preferred_port.saturating_add(i);
        match TcpListener::bind(("127.0.0.1", candidate)).await {
            Ok(listener) => { port = Some((listener, candidate)); break; }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e),
        }
    }
    let (listener, actual) = port.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrInUse, "no free port in range")
    })?;
    let app = build_router(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    Ok((handle, actual))
}
```

Add to `proxy/mod.rs`: `pub mod server;`

> If Task 7 runs before Task 8, the `openai_edge` reference won't compile. Either do Task 8 first, or temporarily add `pub mod openai_edge;` with a stub `pub async fn chat_completions() {}` and remove the stub in Task 8. Recommended: implement Task 8 next.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml server::tests`
Expected: PASS (both). Full crate build may still need Task 8's handler — proceed to Task 8 before the integration build in Task 9.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/
git commit -m "feat(proxy): add router build + port auto-increment strategy"
```

---

### Task 8: OpenAI-edge passthrough handler (`proxy/openai_edge.rs`)

**Files:**
- Create: `src-tauri/src/proxy/openai_edge.rs`
- Modify: `src-tauri/src/proxy/mod.rs` (add `pub mod openai_edge;`)

**Interfaces:**
- Consumes: `AppState`, `resolve_model`, `BackendConfig`, `SecretStore`.
- Produces: `async fn chat_completions(State<AppState>, Bytes) -> Result<Response<Body>, ProxyError>`; `ProxyError` enum implementing `IntoResponse`.

- [ ] **Step 1: Write the failing integration test**

Append to `src-tauri/src/proxy/openai_edge.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::proxy::server::build_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state(upstream_base: &str) -> AppState {
        let mut cfg = AppConfig::default();
        cfg.models.push(Model {
            id: "m_glm46".into(), provider_id: "zhipu".into(), display_name: "GLM-4.6".into(),
            source: ModelSource::Manual,
            openai: Some(BackendConfig { base_url: upstream_base.into(), upstream_model_id: "glm-4.6".into(), api_key_ref: None }),
            anthropic: None, cooldown_seconds: None, fallback_target_model_id: None,
        });
        cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_glm46".into() });
        let secrets = Arc::new(MemoryStore::default());
        secrets.set_key("zhipu", "sk-test").unwrap();
        let state = AppStateInner { config: tokio::sync::RwLock::new(cfg), secrets };
        Arc::new(state)
    }

    #[tokio::test]
    async fn rewrites_model_forwards_and_passes_body() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"hi"}}]}),
            ))
            .mount(&mock).await;

        let app = build_router(test_state(&mock.uri()).await);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"model":"glm-5.2","messages":[{"role":"user","content":"hi"}]}).to_string(),
                )).unwrap()
        ).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("hi"));

        // forwarded request had model rewritten to upstream id and auth header
        let received = &mock.received_requests().await.unwrap()[0];
        let raw = String::from_utf8_lossy(received);
        let body_start = raw.find("\r\n\r\n").map(|i| &raw[i + 4..]).unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(body_start).unwrap();
        assert_eq!(v["model"], "glm-4.6");
        assert!(raw.to_lowercase().contains("authorization: bearer sk-test"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml openai_edge::`
Expected: FAIL — handler undefined.

- [ ] **Step 3: Implement**

`src-tauri/src/proxy/openai_edge.rs`:
```rust
use std::sync::Arc;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use thiserror::Error;
use crate::config::BackendConfig;
use crate::proxy::resolve::resolve_model;
use crate::proxy::{AppState, ResolveError};

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("no profiles/models configured; set up SwitchLM first")] NotConfigured,
    #[error("{0}")] Resolve(#[from] ResolveError),
    #[error("model has no openai backend")] NoOpenAiBackend,
    #[error("missing api key for provider '{0}'")] NoApiKey(String),
    #[error("upstream request failed: {0}")] Upstream(String),
}
impl IntoResponse for ProxyError {
    fn into_response(self) -> Response<Body> {
        let (code, msg) = match &self {
            ProxyError::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            ProxyError::Resolve(ResolveError::ProfileNotFound { .. }) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::Resolve(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            ProxyError::NoOpenAiBackend | ProxyError::NoApiKey(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            ProxyError::Upstream(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
        };
        Response::builder().status(code).body(Body::from(msg)).unwrap()
    }
}

pub async fn chat_completions(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    let mut req: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ProxyError::Upstream(format!("invalid json: {e}")))?;

    // resolve model by incoming profile name, then take a snapshot of what we need
    let (backend, provider_id, upstream_id, requested_name) = {
        let cfg = state.config.read().await;
        if cfg.profiles.is_empty() || cfg.models.is_empty() {
            return Err(ProxyError::NotConfigured);
        }
        let requested = req.get("model").and_then(|v| v.as_str());
        let model = resolve_model(&cfg, requested)?;
        let backend = model.openai.clone().ok_or(ProxyError::NoOpenAiBackend)?;
        (backend, model.provider_id.clone(), backend.upstream_model_id.clone(), requested.map(str::to_string))
    };

    let key = state.secrets.get_key(&provider_id)
        .map_err(|e| ProxyError::Upstream(e.to_string()))?
        .ok_or_else(|| ProxyError::NoApiKey(provider_id.clone()))?;

    // rewrite model field to upstream id
    req["model"] = serde_json::Value::String(upstream_id);
    let is_stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    let url = join_url(&backend);
    let client = reqwest::Client::new();
    let upstream = client.post(&url)
        .bearer_auth(&key)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&req).unwrap_or_default());
    let resp = upstream.send().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;

    let mut out = Response::builder().status(resp.status());
    if let Some(ct) = resp.headers().get("content-type").cloned() {
        out = out.header("content-type", ct);
    }
    if is_stream {
        out.body(Body::from_stream(resp.bytes_stream())).unwrap()
    } else {
        let bytes = resp.bytes().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
        out.body(Body::from(bytes)).unwrap()
    }
    .map_err(|e| ProxyError::Upstream(e.to_string()))
    // keep requested_name referenced for Plan 2 (response model echo); unused for now
    .map(|r| { let _ = &requested_name; r })
}

fn join_url(b: &BackendConfig) -> String {
    let base = b.base_url.trim_end_matches('/');
    format!("{base}/chat/completions")
}
```

Add to `proxy/mod.rs`: `pub mod openai_edge;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml openai_edge::`
Expected: PASS. Also build the whole crate: `cargo build --manifest-path src-tauri/Cargo.toml` → OK.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/
git commit -m "feat(proxy): add OpenAI-edge passthrough (model rewrite + auth + stream)"
```

---

### Task 9: Wire server + command skeleton into Tauri (`lib.rs`, `commands.rs`)

**Files:**
- Create: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`, `src-tauri/src/proxy/mod.rs` (re-export needed types)

**Interfaces:**
- Consumes: `AppState`, `store`, `secrets`, `server::serve`, `tauri`.
- Produces: Tauri commands `get_profiles`, `get_models`, `get_providers`, `set_profile_backing(profile_id, model_id)`; app startup spawns the proxy server and manages `AppState`.

- [ ] **Step 1: Write the command skeleton + wiring**

`src-tauri/src/commands.rs`:
```rust
use std::sync::Arc;
use tauri::{Manager, State};
use crate::proxy::{AppState, AppStateInner};
use crate::config::{AppConfig, Model, Profile, Provider};

#[tauri::command]
pub async fn get_providers(state: State<'_, AppState>) -> Result<Vec<Provider>, String> {
    Ok(state.config.read().await.providers.clone())
}
#[tauri::command]
pub async fn get_models(state: State<'_, AppState>) -> Result<Vec<Model>, String> {
    Ok(state.config.read().await.models.clone())
}
#[tauri::command]
pub async fn get_profiles(state: State<'_, AppState>) -> Result<Vec<Profile>, String> {
    Ok(state.config.read().await.profiles.clone())
}

/// Hot-swap: change a Profile's backing model. Does not affect in-flight requests.
#[tauri::command]
pub async fn set_profile_backing(
    state: State<'_, AppState>,
    profile_id: String,
    model_id: String,
    app_data_dir: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        let prof = cfg.profiles.iter_mut().find(|p| p.id == profile_id)
            .ok_or_else(|| format!("profile {profile_id} not found"))?;
        prof.backing_model_id = model_id.clone();
        persist(&app_data_dir, &cfg)?;
    }
    Ok(())
}

fn persist(app: &tauri::AppHandle, cfg: &AppConfig) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    crate::config::store::save(&dir, cfg).map_err(|e| e.to_string())
}
```

`src-tauri/src/lib.rs` (replace scaffold body; keep the `run` entry the scaffold's `main.rs` calls):
```rust
pub mod commands;
pub mod config;
pub mod proxy;

use std::sync::Arc;
use tauri::Manager;
use proxy::{AppStateInner, server};
use config::KeyringStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init()) // scaffold default; harmless
        .setup(|app| {
            let dir = app.path().app_data_dir()?.clone();
            // Use MemoryStore until Plan 3 wires real key entry; KeyringStore ready to swap in.
            let secrets: Arc<dyn config::SecretStore> = Arc::new(KeyringStore);
            let inner = AppStateInner::load(&dir, secrets)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
            let preferred = inner.config.blocking_read().settings.port;
            let state: proxy::AppState = Arc::new(inner);

            let state_for_server = state.clone();
            tauri::async_runtime::spawn(async move {
                match server::serve(state_for_server, preferred).await {
                    Ok((_handle, port)) => tracing::info!("proxy listening on {port}"),
                    Err(e) => tracing::error!("proxy bind failed: {e}"),
                }
            });
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_providers,
            commands::get_models,
            commands::get_profiles,
            commands::set_profile_backing,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SwitchLM");
}
```

> If the scaffold's `main.rs` calls a different entry, keep that call but ensure it reaches `run()`. If `tauri_plugin_opener` isn't a scaffold dep, remove that `.plugin(...)` line.

- [ ] **Step 2: Smoke test manually**

Run: `npm run tauri dev`
Once the window opens, in another terminal exercise the proxy against a real provider key you've placed in the OS keyring (or temporarily swap `KeyringStore` → a `MemoryStore` preloaded with your key for this smoke test):
```bash
curl -s http://localhost:6950/v1/chat/completions \
  -H "content-type: application/json" \
  -d '{"model":"<your-profile-name>","messages":[{"role":"user","content":"ping"}]}'
```
Expected: a valid OpenAI-style response from the upstream provider (model routed via Profile → Model). If the profile name is unknown, expect a `400` listing available profiles. Note the actual port from logs if 6950 was taken.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/commands.rs src-tauri/src/proxy/mod.rs
git commit -m "feat(app): wire proxy server + command skeleton into Tauri"
```

---

## Definition of Done (Plan 1)

- [ ] `npm run tauri dev` launches; proxy binds (6950 or next free).
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` — all unit + integration tests pass.
- [ ] OpenAI-edge passthrough verified end-to-end against a real provider (or wiremock): request `model` rewritten to `upstream_model_id`, `Authorization: Bearer …` injected, response body streamed through.
- [ ] Unknown profile name → `400` with available names.
- [ ] `set_profile_backing` changes routing for **new** requests without touching in-flight ones (manual check).
- [ ] Secrets never written to `app_config.json` (grep the file: no `api_key`/`access_key_id` values).

## Hand-off to Plan 2

Plan 2 builds on `AppState`, `resolve_model`, `build_router`, and `chat_completions`. Next additions:
- `/v1/messages` Anthropic edge + translation + tool_use state machine → mount in `build_router`.
- Response `model`-field echo (both edges) — wrap the response-building tail of `chat_completions`.
- `ErrorAdapter` + per-Model fallback + circuit breaker (`ModelHealth` map in `AppStateInner`).
- Usage adapters (`UsageProvider` trait + Zhipu/Volcengine impls).
