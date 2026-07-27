# Bailian Usage-Cookie Capture — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an in-app embedded WebView login window so a user can authenticate to the Aliyun (阿里云) console inside SwitchLM; the backend captures the resulting HttpOnly cookie jar and stores it securely, ready for a future Bailian usage adapter.

**Architecture:** A Rust-owned child `WebviewWindow` (first multi-window code in the repo) loads the Bailian console; auto-detection (an async cookie-jar poll + host-transition fast-path) plus a manual `完成登录` button trigger capture via `WebviewWindow::cookies_for_url` (returns HttpOnly+secure cookies). The cookie is serialized to a `Cookie:` header string and stored through the existing `SecretStore` (keyring-first, file fallback) under a new `usage_cookie` channel — exactly mirroring the Volcengine `usage_sk` channel, plus an oversized-blob file fallback for the Windows Credential Manager 2560-byte cap. The frontend gets a `登录百炼控制台` button and an `已登录/未登录` presence tag.

**Tech Stack:** Rust (tauri 2.11.5, axum, serde, keyring 3.6.3, url, tokio; co-located `#[cfg(test)]` tests), Vue 3 `<script setup>` + Pinia + Naive UI. Backend gate: `cargo test --manifest-path src-tauri/Cargo.toml`. Frontend gate: `npx vue-tsc --noEmit` (frontend has no unit framework). The windowing path is manual-test-only (needs real WebView2).

**Spec:** `docs/superpowers/specs/2026-08-02-bailian-usage-cookie-capture-design.md`

## Global Constraints

- **Vendor slug:** `bailian-token`. **Window label:** `bailian-login`. **Service name (keyring):** `SwitchLM`. **Keyring entry name:** `{provider_id}::usage_cookie` (sibling to `::usage_sk`).
- **Login URL:** `https://bailian.console.aliyun.com/cn-beijing?tab=plan&commonbuy=1#/efm/subscription/coding-plan`. **Cookie-capture URL:** `https://bailian.console.aliyun.com/`. **Dedicated WebView2 data dir:** `<app_data>/bailian_webview`.
- **Windows async caveat (load-bearing):** `WebviewWindow::cookies()`/`cookies_for_url()` **deadlock on Windows inside a sync command/event handler** — call them only from `async` commands or `tauri::async_runtime::spawn` tasks, NEVER inside `on_navigation`'s sync closure. The `on_navigation` closure only inspects the URL string.
- **Windows Credential Manager cap:** `CRED_MAX_CREDENTIAL_BLOB_SIZE = 2560` bytes (keyring 3.6.3 rejects larger blobs). Cookie channel falls back to `FileSecretStore` for oversized blobs (keyring backend only).
- **SSO success-cookie candidates:** `login_aliyunid_ticket`, `login_aliyunid` (confirm against a real jar while implementing Task 5). **Passport hosts** (host-transition signal): `passport.aliyun.com`, `signin.aliyun.com`, `login.aliyun.com`, `login.taobao.com`. **Poll interval:** 2 s.
- **Event:** `bailian-login-status`, payload `{ provider_id: String, status: "captured" | "closed-empty" | "error", message: Option<String> }`.
- **UI copy (Chinese):** `登录百炼控制台`, `完成登录`, `已登录`, `未登录`.
- **No change** to dispatch/breaker/translate/catalog/existing usage adapters. `usage_provider_for("bailian-token")` still returns `None`; the usage chip stays `不支持` until the (separate) adapter spec. `selectLabel.ts` is untouched.
- **The cookie value never crosses IPC into JS** — capture writes it in Rust; commands return only `bool`/`void`/status events.
- Conventional-commit messages; work on `main`; one commit per task.

---

### Task 1: Backend — add the `usage_cookie` channel to `SecretStore`

Extend the trait + all five backends + the `SecretStoreHandle` delegator with the cookie trio, mirroring `usage_sk` exactly. No fallback yet (Task 2 adds it). TDD.

**Files:**
- Modify: `src-tauri/src/config/secrets.rs` (trait `58-67`; `usage_sk_entry` helper `74-76`; `KeyringStore` `80-133`; `MemoryStore` `140-174`; `FileSecretStore` `235-262`; `PendingStore` `267-274`; `SecretStoreHandle` impl `298-305`; tests `307+`).

**Interfaces:**
- Produces: `fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError>` (+ `get_usage_cookie` → `Result<Option<String>>`, `delete_usage_cookie` → `Result<()>`) on every `SecretStore` impl, plus private `fn usage_cookie_entry(provider_id: &str) -> String`. Task 2 wraps the Handle's versions; Tasks 4/5 consume them via `state.secrets.{set,get,delete}_usage_cookie`.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/config/secrets.rs`, inside `mod tests` (after `file_store_usage_sk_distinct_from_api_key`), add:

```rust
    #[test]
    fn memory_store_usage_cookie_distinct_from_api_key_and_sk() {
        let s = MemoryStore::default();
        s.set_key("bailian", "sk-inference").unwrap();
        s.set_usage_sk("bailian", "volc-sk").unwrap();
        s.set_usage_cookie("bailian", "cna=x; ticket=y").unwrap();
        assert_eq!(s.get_key("bailian").unwrap(), Some("sk-inference".into()));
        assert_eq!(s.get_usage_sk("bailian").unwrap(), Some("volc-sk".into()));
        assert_eq!(s.get_usage_cookie("bailian").unwrap(), Some("cna=x; ticket=y".into()));
        s.delete_key("bailian").unwrap();
        assert_eq!(s.get_key("bailian").unwrap(), None);
        assert_eq!(s.get_usage_cookie("bailian").unwrap(), Some("cna=x; ticket=y".into()));
        s.delete_usage_cookie("bailian").unwrap();
        assert_eq!(s.get_usage_cookie("bailian").unwrap(), None);
        assert_eq!(s.get_usage_sk("bailian").unwrap(), Some("volc-sk".into()));
    }

    #[test]
    fn file_store_usage_cookie_roundtrip() {
        let (_dir, s) = tmp_store();
        assert_eq!(s.get_usage_cookie("bailian").unwrap(), None);
        s.set_usage_cookie("bailian", "cna=x; ticket=y").unwrap();
        assert_eq!(s.get_usage_cookie("bailian").unwrap(), Some("cna=x; ticket=y".into()));
        s.delete_usage_cookie("bailian").unwrap();
        assert_eq!(s.get_usage_cookie("bailian").unwrap(), None);
    }

    #[test]
    fn handle_delegates_usage_cookie() {
        let h = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        assert_eq!(h.get_usage_cookie("a").unwrap(), None);
        h.set_usage_cookie("a", "c=1").unwrap();
        assert_eq!(h.get_usage_cookie("a").unwrap(), Some("c=1".into()));
        h.delete_usage_cookie("a").unwrap();
        assert_eq!(h.get_usage_cookie("a").unwrap(), None);
    }
```

Also extend `pending_store_reads_none_writes_err` — add three cookie assertions alongside the existing key/usage_sk ones:

```rust
        assert_eq!(s.get_usage_cookie("a").unwrap(), None);
        assert!(matches!(s.set_usage_cookie("a", "x"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.delete_usage_cookie("a"), Err(SecretError::PendingConsent)));
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib config::secrets::tests`
Expected: FAIL — `no method named set_usage_cookie found`.

- [ ] **Step 3: Add the trait methods + entry helper**

Add to the `SecretStore` trait (after `delete_usage_sk`, before the closing `}`):

```rust
    /// Aliyun Bailian (console) usage-session cookie. Distinct keyring entry from the inference
    /// `api_key` and the Volcengine `usage_sk`. Captured by the in-app login window
    /// (`bailian_login`); the stored value is a serialized `Cookie:` header string.
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError>;
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError>;
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError>;
```

Add a sibling helper next to `usage_sk_entry`:

```rust
fn usage_cookie_entry(provider_id: &str) -> String {
    format!("{provider_id}::usage_cookie")
}
```

- [ ] **Step 4: Implement on `KeyringStore`** (mirror `set_usage_sk`/`get_usage_sk`/`delete_usage_sk`, using `usage_cookie_entry`):

```rust
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError> {
        keyring::Entry::new(SERVICE, &usage_cookie_entry(provider_id))
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .set_password(cookie)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        match keyring::Entry::new(SERVICE, &usage_cookie_entry(provider_id))
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .get_password()
        {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError> {
        match keyring::Entry::new(SERVICE, &usage_cookie_entry(provider_id))
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .delete_credential()
        {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
```

- [ ] **Step 5: Implement on `MemoryStore`** (mirror usage_sk):

```rust
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().insert(usage_cookie_entry(provider_id), cookie.into());
        Ok(())
    }
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(&usage_cookie_entry(provider_id)).cloned())
    }
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().remove(&usage_cookie_entry(provider_id));
        Ok(())
    }
```

- [ ] **Step 6: Implement on `FileSecretStore`** (mirror usage_sk — note the `flush`):

```rust
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.insert(usage_cookie_entry(provider_id), cookie.into());
        self.flush(&map)
    }
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(&usage_cookie_entry(provider_id)).cloned())
    }
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.remove(&usage_cookie_entry(provider_id));
        self.flush(&map)
    }
```

- [ ] **Step 7: Implement on `PendingStore`** (mirror usage_sk):

```rust
    fn set_usage_cookie(&self, _: &str, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn get_usage_cookie(&self, _: &str) -> Result<Option<String>, SecretError> { Ok(None) }
    fn delete_usage_cookie(&self, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
```

- [ ] **Step 8: Delegate in `SecretStoreHandle`** (mirror usage_sk — Task 2 replaces these three):

```rust
    fn set_usage_cookie(&self, id: &str, k: &str) -> Result<(), SecretError> { self.current().set_usage_cookie(id, k) }
    fn get_usage_cookie(&self, id: &str) -> Result<Option<String>, SecretError> { self.current().get_usage_cookie(id) }
    fn delete_usage_cookie(&self, id: &str) -> Result<(), SecretError> { self.current().delete_usage_cookie(id) }
```

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib config::secrets::tests`
Expected: PASS (all four new/extended tests + existing).

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): add usage_cookie channel to SecretStore"
```

---

### Task 2: Backend — oversized-blob file fallback on `SecretStoreHandle` + wire at init

Add a `FileSecretStore` fallback to the Handle, used by the cookie methods only when the primary keyring backend rejects a blob (>2560 B). Attach it at the one production init site. TDD with a mock backend.

**Files:**
- Modify: `src-tauri/src/config/secrets.rs` (`SecretStoreHandle` struct `279-296` + impl `298-305` + tests).
- Modify: `src-tauri/src/lib.rs:85` (the single production `SecretStoreHandle::new` site, `dir` in scope).

**Interfaces:**
- Consumes: Task 1's cookie trio on the trait/backends.
- Produces: `SecretStoreHandle::with_cookie_file_fallback(self, Arc<FileSecretStore>) -> Self`; the Handle's cookie methods become fallback-aware. `new()` signature is unchanged (all test call sites and `proxy/state.rs:49` keep working with no fallback).

- [ ] **Step 1: Write the failing tests**

In `mod tests`, add a mock backend that fails only `set_usage_cookie`, plus two tests:

```rust
    /// Delegates everything to MemoryStore but always fails `set_usage_cookie` — used to exercise
    /// the handle's oversized-blob file fallback deterministically (without a real keyring).
    #[cfg(test)]
    struct FailCookie(MemoryStore);
    #[cfg(test)]
    impl SecretStore for FailCookie {
        fn set_key(&self, id: &str, k: &str) -> Result<(), SecretError> { self.0.set_key(id, k) }
        fn get_key(&self, id: &str) -> Result<Option<String>, SecretError> { self.0.get_key(id) }
        fn delete_key(&self, id: &str) -> Result<(), SecretError> { self.0.delete_key(id) }
        fn set_usage_sk(&self, id: &str, k: &str) -> Result<(), SecretError> { self.0.set_usage_sk(id, k) }
        fn get_usage_sk(&self, id: &str) -> Result<Option<String>, SecretError> { self.0.get_usage_sk(id) }
        fn delete_usage_sk(&self, id: &str) -> Result<(), SecretError> { self.0.delete_usage_sk(id) }
        fn set_usage_cookie(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Err(SecretError::Keyring("simulated oversized blob".into()))
        }
        fn get_usage_cookie(&self, id: &str) -> Result<Option<String>, SecretError> { self.0.get_usage_cookie(id) }
        fn delete_usage_cookie(&self, id: &str) -> Result<(), SecretError> { self.0.delete_usage_cookie(id) }
    }

    #[test]
    fn handle_cookie_falls_back_to_file_when_keyring_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let file = Arc::new(FileSecretStore::new(dir.path()).unwrap());
        let h = SecretStoreHandle::new(Arc::new(FailCookie(MemoryStore::default())), BackendKind::Keyring)
            .with_cookie_file_fallback(file.clone());
        // set via handle → primary errors → lands in the file fallback.
        h.set_usage_cookie("bailian", "cna=x; ticket=y").unwrap();
        assert_eq!(file.get_usage_cookie("bailian").unwrap(), Some("cna=x; ticket=y".into()));
        // handle get reads it back from the fallback (primary returned None).
        assert_eq!(h.get_usage_cookie("bailian").unwrap(), Some("cna=x; ticket=y".into()));
        h.delete_usage_cookie("bailian").unwrap();
        assert_eq!(file.get_usage_cookie("bailian").unwrap(), None);
    }

    #[test]
    fn handle_cookie_no_fallback_propagates_error() {
        // No fallback attached → primary error surfaces (no silent success).
        let h = SecretStoreHandle::new(Arc::new(FailCookie(MemoryStore::default())), BackendKind::Keyring);
        assert!(h.set_usage_cookie("bailian", "c=1").is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib config::secrets::tests::handle_cookie`
Expected: FAIL — `no method named with_cookie_file_fallback found`.

- [ ] **Step 3: Add the fallback field, builder, and a snapshot helper**

Change the struct + `new`, and add the builder + helper:

```rust
pub struct SecretStoreHandle {
    inner: Mutex<(Arc<dyn SecretStore>, BackendKind)>,
    /// File-store fallback for the cookie channel only: when the active keyring backend rejects
    /// an oversized cookie blob (> Windows Credential Manager's 2560-byte cap), the cookie is
    /// stored here instead. `None` when no fallback is attached (tests, non-keyring backends).
    cookie_file_fallback: Option<Arc<FileSecretStore>>,
}

impl SecretStoreHandle {
    pub fn new(store: Arc<dyn SecretStore>, kind: BackendKind) -> Self {
        Self { inner: Mutex::new((store, kind)), cookie_file_fallback: None }
    }
    /// Attach a file-store fallback for the cookie channel. Attach only when the primary backend
    /// is the OS keyring (the oversized-blob case). Builder-style; returns self for chaining.
    pub fn with_cookie_file_fallback(mut self, file: Arc<FileSecretStore>) -> Self {
        self.cookie_file_fallback = Some(file);
        self
    }
    fn snapshot(&self) -> (Arc<dyn SecretStore>, BackendKind) {
        let lock = self.inner.lock().unwrap();
        (lock.0.clone(), lock.1)
    }
    // ... existing current()/swap()/kind() unchanged ...
```

- [ ] **Step 4: Replace the Handle's three cookie methods with fallback-aware versions**

(In the `impl SecretStore for SecretStoreHandle` block — these supersede Task 1's plain delegations.)

```rust
    fn set_usage_cookie(&self, id: &str, k: &str) -> Result<(), SecretError> {
        let (store, kind) = self.snapshot();
        match store.set_usage_cookie(id, k) {
            Ok(()) => Ok(()),
            Err(e) => match (&self.cookie_file_fallback, kind) {
                (Some(file), BackendKind::Keyring) => file.set_usage_cookie(id, k),
                _ => Err(e),
            },
        }
    }
    fn get_usage_cookie(&self, id: &str) -> Result<Option<String>, SecretError> {
        let (store, kind) = self.snapshot();
        let primary = store.get_usage_cookie(id);
        let use_fallback = matches!(&self.cookie_file_fallback, Some(_)) && kind == BackendKind::Keyring;
        match primary {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) if use_fallback => self.cookie_file_fallback.as_ref().unwrap().get_usage_cookie(id),
            Ok(None) => Ok(None),
            Err(_) if use_fallback => self.cookie_file_fallback.as_ref().unwrap().get_usage_cookie(id),
            Err(_) => Ok(None), // best-effort read (consistent with usage_sk read handling)
        }
    }
    fn delete_usage_cookie(&self, id: &str) -> Result<(), SecretError> {
        let (store, _) = self.snapshot();
        let primary = store.delete_usage_cookie(id);
        if let Some(file) = &self.cookie_file_fallback {
            let _ = file.delete_usage_cookie(id); // idempotent cleanup of the fallback too
        }
        primary
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib config::secrets::tests`
Expected: PASS (both new tests + Task 1's tests still pass + `handle_delegates_and_swaps_and_reports_kind` still passes — `new()` signature unchanged).

- [ ] **Step 6: Wire the fallback at the production init site**

In `src-tauri/src/lib.rs`, replace line 85 (`let secrets = config::SecretStoreHandle::new(store, kind);`) with:

```rust
            let secrets = config::SecretStoreHandle::new(store, kind);
            // Cookie blobs can exceed the Windows Credential Manager 2560-byte cap; attach a
            // file-store fallback for the cookie channel when the primary is the OS keyring.
            let secrets = if matches!(kind, config::secrets::BackendKind::Keyring) {
                match config::secrets::FileSecretStore::new(&dir) {
                    Ok(file) => secrets.with_cookie_file_fallback(std::sync::Arc::new(file)),
                    Err(e) => {
                        tracing::warn!("无法构造 cookie 文件回退后端：{e}；超大 Cookie 将无法存储");
                        secrets
                    }
                }
            } else {
                secrets
            };
```

- [ ] **Step 7: Build + run the full backend test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — full suite green (no regressions; the `proxy/state.rs:49` and dispatch-test Handle constructions use `new()` with no fallback and still compile/work).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/config/secrets.rs src-tauri/src/lib.rs
git commit -m "feat(secrets): keyring-oversize file fallback for usage_cookie"
```

---

### Task 3: Backend — `bailian_login` module + pure cookie-header serializer

Create the `bailian_login` module (declared in `lib.rs`) containing the pure, fully-unit-tested `pairs_to_cookie_header` serializer. This is the only logic in the capture path that is cleanly unit-testable; the windowing (Task 5) builds on this module. TDD.

**Files:**
- Create: `src-tauri/src/bailian_login.rs`
- Modify: `src-tauri/src/lib.rs:1-7` (module declarations — add `pub mod bailian_login;`)

**Interfaces:**
- Produces: `pub fn pairs_to_cookie_header(pairs: &[(&str, &str)]) -> String` in `crate::bailian_login`. Task 5's capture routine maps `Vec<Cookie>` → `Vec<(&str,&str)>` and calls this.

- [ ] **Step 1: Declare the module**

In `src-tauri/src/lib.rs`, add to the module list (after `pub mod commands;` or alongside the others):

```rust
pub mod bailian_login;
```

- [ ] **Step 2: Write the failing tests (create the file with tests first)**

Create `src-tauri/src/bailian_login.rs`:

```rust
//! In-app Aliyun Bailian console login: open an embedded WebView window, capture the HttpOnly
//! session cookie jar after SSO, and store it for the (future) usage adapter. See spec
//! `docs/superpowers/specs/2026-08-02-bailian-usage-cookie-capture-design.md`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pairs_yield_empty_header() {
        assert_eq!(pairs_to_cookie_header(&[]), "");
    }

    #[test]
    fn joins_pairs_in_order() {
        assert_eq!(
            pairs_to_cookie_header(&[("cna", "xyz"), ("JSESSIONID", "abc")]),
            "cna=xyz; JSESSIONID=abc",
        );
    }

    #[test]
    fn preserves_same_name_different_value_duplicates() {
        // Browsers send both matching cookies (RFC 6265 §5.4); the future adapter must echo that,
        // so we deliberately do NOT dedup by name.
        assert_eq!(
            pairs_to_cookie_header(&[("token", "specific"), ("token", "general")]),
            "token=specific; token=general",
        );
    }

    #[test]
    fn drops_pairs_with_cr_or_ln_to_prevent_header_injection() {
        assert_eq!(
            pairs_to_cookie_header(&[
                ("good", "1"),
                ("evil", "x\r\nX-Evil: yes"), // CRLF in value → dropped
                ("evil2\n", "y"),             // LF in name → dropped
            ]),
            "good=1",
        );
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml bailian_login`
Expected: FAIL — `cannot find function pairs_to_cookie_header`.

- [ ] **Step 4: Implement the serializer**

Add above the `#[cfg(test)] mod tests`:

```rust
/// Serialize captured cookie `(name, value)` pairs into the value of an HTTP `Cookie:` header
/// (`name=value; name2=value2`). Order is preserved verbatim — including legitimate same-name /
/// different-domain duplicates, which browsers send (RFC 6265 §5.4) and the future quota adapter
/// must echo. The only transformation is the **header-injection guard**: any pair whose name or
/// value contains `\r` or `\n` is dropped so a malformed cookie can't smuggle extra header lines.
pub fn pairs_to_cookie_header(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .filter(|(n, v)| !n.contains('\r') && !n.contains('\n') && !v.contains('\r') && !v.contains('\n'))
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml bailian_login`
Expected: PASS (all four tests).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/bailian_login.rs src-tauri/src/lib.rs
git commit -m "feat(bailian): pure cookie-header serializer + module"
```

---

### Task 4: Backend — cookie presence + clear commands

Add the two thin Tauri commands that the frontend needs before the windowing exists: `provider_has_usage_cookie` (drives the presence tag) and `clear_bailian_cookie` (logout/re-login). Register them.

**Files:**
- Modify: `src-tauri/src/bailian_login.rs` (add the commands).
- Modify: `src-tauri/src/lib.rs:137-181` (invoke_handler — register the two commands).

**Interfaces:**
- Consumes: Task 1's `state.secrets.get_usage_cookie` / `delete_usage_cookie`. The command signature mirrors `commands::provider_has_usage_sk` (`commands.rs:414-420`) and `set_provider_usage_sk` (`commands.rs:390-411`).
- Produces: `bailian_login::provider_has_usage_cookie(state, provider_id) -> Result<bool, String>` and `bailian_login::clear_bailian_cookie(state, provider_id) -> Result<(), String>`. Task 6's frontend `providerHasUsageCookie` / `clearBailianCookie` call these.

- [ ] **Step 1: Add the two commands**

In `src-tauri/src/bailian_login.rs`, add at the top of the file:

```rust
use crate::proxy::AppState;

/// Whether a captured Bailian usage cookie is stored for the provider (does not reveal it).
#[tauri::command]
pub async fn provider_has_usage_cookie(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> Result<bool, String> {
    Ok(state.secrets.get_usage_cookie(&provider_id).map_err(|e| e.to_string())?.is_some())
}

/// Clear a stored Bailian usage cookie (logout / re-login). Idempotent.
#[tauri::command]
pub async fn clear_bailian_cookie(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> Result<(), String> {
    state.secrets.delete_usage_cookie(&provider_id).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Register the commands**

In `src-tauri/src/lib.rs`, inside `tauri::generate_handler![ ... ]` (add after `commands::provider_has_usage_sk,` at line 159):

```rust
            bailian_login::provider_has_usage_cookie,
            bailian_login::clear_bailian_cookie,
```

- [ ] **Step 3: Build + run the full test suite**

Run: `cargo build --manifest-path src-tauri/Cargo.toml` then `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: build OK; tests PASS. (The commands are thin delegations over the already-tested secrets channel. They are exercised end-to-end in Task 6's manual test.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/bailian_login.rs src-tauri/src/lib.rs
git commit -m "feat(bailian): cookie presence + clear commands"
```

---

### Task 5: Backend — login window + capture (`open_bailian_login`, `finish_bailian_login`)

The core integration task. Build the Rust-owned login window, wire the three detection signals (poll + host-transition + manual), capture via `cookies_for_url`, store via the cookie channel, emit `bailian-login-status`, and manage the single-window/single-task lifecycle. **Manual-test-only** (needs real WebView2); gate is build + existing tests stay green + manual verification.

> **API confirm-points while coding:** (a) `WebviewWindowBuilder::on_navigation(F)` and `.data_directory(PathBuf)` exist in tauri 2.11.5 (verified). (b) `tokio::time::sleep` is available (tokio is a transitive dep via axum/tauri; if not resolvable, use `tauri::async_runtime::spawn` + a tokio re-export). (c) `app.emit(...)` needs `use tauri::Emitter;`. (d) `WebviewWindow::on_window_event(F)` comes from the `Window` deref.

**Files:**
- Modify: `src-tauri/src/bailian_login.rs` (add windowing + the two commands).
- Modify: `src-tauri/src/lib.rs` invoke_handler (register the two commands).
- Create: `src-tauri/capabilities/bailian-login.json` (minimal capability for the login window).

**Interfaces:**
- Consumes: Task 1/2 `state.secrets.set_usage_cookie`; Task 3 `pairs_to_cookie_header`.
- Produces: `bailian_login::open_bailian_login(app, provider_id) -> Result<(), String>` and `bailian_login::finish_bailian_login(app, provider_id) -> Result<(), String>`; emits event `bailian-login-status`. Task 6 frontend calls these.

- [ ] **Step 1: Add the windowing constants, status struct, and lifecycle state**

In `src-tauri/src/bailian_login.rs` (after the `use crate::proxy::AppState;` from Task 4), add:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{async_runtime, AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const LOGIN_LABEL: &str = "bailian-login";
const LOGIN_URL: &str =
    "https://bailian.console.aliyun.com/cn-beijing?tab=plan&commonbuy=1#/efm/subscription/coding-plan";
const CONSOLE_URL: &str = "https://bailian.console.aliyun.com/";
/// Cookie names that indicate a successful Aliyun SSO. Capture fires when any appears.
/// (Confirm against a real jar while implementing — these are the standard Aliyun SSO tickets.)
const SSO_COOKIE_NAMES: &[&str] = &["login_aliyunid_ticket", "login_aliyunid"];
/// Hosts the login flow passes through before returning to the console (host-transition signal).
const PASSPORT_HOSTS: &[&str] =
    &["passport.aliyun.com", "signin.aliyun.com", "login.aliyun.com", "login.taobao.com"];

/// One login window at a time (fixed label): the current poll task. Aborted on re-open, on
/// capture, and on window destroy — so rapid re-clicks and close-then-reopen never leak tasks.
static POLL_TASK: tokio::sync::Mutex<Option<async_runtime::JoinHandle<()>>> =
    tokio::sync::Mutex::const_new(None);
/// Set true when a capture succeeded this session; the Destroyed handler reads it to decide
/// whether to emit "closed-empty" (user closed without logging in).
static CAPTURED: AtomicBool = AtomicBool::new(false);

#[derive(serde::Serialize, Clone)]
struct LoginStatus {
    provider_id: String,
    status: &'static str, // "captured" | "closed-empty" | "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn emit_status(app: &AppHandle, provider_id: &str, status: &'static str, message: Option<String>) {
    let _ = app.emit(
        "bailian-login-status",
        LoginStatus { provider_id: provider_id.to_string(), status, message },
    );
}

fn console_url() -> url::Url {
    url::Url::parse(CONSOLE_URL).expect("hardcoded console URL parses")
}
```

- [ ] **Step 2: Add the capture routine**

```rust
/// Read the console-domain jar, serialize, store, and close the window. `manual` controls the
/// empty-jar behavior: auto signals no-op (keep waiting); manual emits an "error" toast.
/// Must run in an async context (Windows deadlocks `cookies_for_url` in sync handlers).
async fn try_capture_and_close(app: &AppHandle, provider_id: &str, manual: bool) {
    let Some(win) = app.get_webview_window(LOGIN_LABEL) else { return };
    let cookies = match win.cookies_for_url(console_url()) {
        Ok(c) => c,
        Err(e) => {
            emit_status(app, provider_id, "error", Some(format!("读取 Cookie 失败：{e}")));
            return;
        }
    };
    let pairs: Vec<(&str, &str)> = cookies.iter().map(|c| (c.name(), c.value())).collect();
    if pairs.is_empty() {
        if manual {
            emit_status(app, provider_id, "error", Some("未检测到 Cookie，请先完成登录".into()));
        }
        return; // auto: keep waiting for login to complete
    }
    let header = pairs_to_cookie_header(&pairs);
    let state = app.state::<AppState>();
    if let Err(e) = state.secrets.set_usage_cookie(provider_id, &header) {
        emit_status(app, provider_id, "error", Some(format!("存储 Cookie 失败：{e}")));
        return;
    }
    CAPTURED.store(true, Ordering::SeqCst);
    if let Some(h) = POLL_TASK.lock().await.take() {
        h.abort();
    }
    emit_status(app, provider_id, "captured", None);
    let _ = win.close();
}
```

- [ ] **Step 3: Add the poll task**

```rust
/// Backup detection: every 2 s, if an SSO cookie is present, capture. Exits when the window is
/// gone (covers X-button close) without spamming `WindowNotFound`.
async fn poll_loop(app: AppHandle, provider_id: String) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let Some(win) = app.get_webview_window(LOGIN_LABEL) else { break };
        let has_sso = match win.cookies_for_url(console_url()) {
            Ok(cookies) => cookies.iter().any(|c| SSO_COOKIE_NAMES.contains(&c.name())),
            Err(_) => {
                if app.get_webview_window(LOGIN_LABEL).is_none() {
                    break;
                }
                continue;
            }
        };
        if has_sso {
            try_capture_and_close(&app, &provider_id, false).await;
            break; // capture closes the window + aborts; task is done
        }
    }
    let _ = POLL_TASK.lock().await.take(); // clear our own handle if we exited without capturing
}
```

- [ ] **Step 4: Add the two commands**

```rust
/// Open the embedded Bailian login window and start auto-detection. Returns immediately.
#[tauri::command]
pub async fn open_bailian_login(app: AppHandle, provider_id: String) -> Result<(), String> {
    // Already open → focus it; don't create a duplicate window or task.
    if let Some(win) = app.get_webview_window(LOGIN_LABEL) {
        let _ = win.set_focus();
        return Ok(());
    }
    // Abort any stale poll task from a prior session.
    if let Some(h) = POLL_TASK.lock().await.take() {
        h.abort();
    }
    CAPTURED.store(false, Ordering::SeqCst);

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let url = url::Url::parse(LOGIN_URL).map_err(|e| e.to_string())?;
    let saw_passport = Arc::new(std::sync::Mutex::new(false));
    let saw_passport_nav = saw_passport.clone();
    let app_for_nav = app.clone();

    let win = WebviewWindowBuilder::new(&app, LOGIN_LABEL, WebviewUrl::External(url))
        .title("登录阿里云百炼控制台")
        .inner_size(960.0, 680.0)
        .centered()
        .resizable(true)
        .data_directory(data_dir.join("bailian_webview"))
        .on_navigation(move |nav_url| {
            // SYNC handler — inspect URL only; NEVER call cookies_for_url here (Windows deadlock).
            if let Some(host) = nav_url.host_str() {
                let sp = saw_passport_nav.clone().lock().unwrap();
                let already = *sp;
                drop(sp);
                let on_passport = PASSPORT_HOSTS.iter().any(|p| host == *p || host.ends_with(p));
                if on_passport {
                    *saw_passport_nav.lock().unwrap() = true;
                } else if already && host.ends_with("console.aliyun.com") {
                    // Returned to the console after passport → likely logged in. Spawn async capture.
                    let app_c = app_for_nav.clone();
                    let pid = provider_id.clone();
                    async_runtime::spawn(async move { try_capture_and_close(&app_c, &pid, false).await; });
                }
            }
            true // allow all navigation
        })
        .build()
        .map_err(|e| e.to_string())?;

    // Cleanup on close (X-button or code close): abort the poll task, emit closed-empty if nothing
    // was captured.
    let app_for_event = app.clone();
    let pid_for_event = provider_id.clone();
    win.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let app_c = app_for_event.clone();
            let pid = pid_for_event.clone();
            async_runtime::spawn(async move {
                if let Some(h) = POLL_TASK.lock().await.take() {
                    h.abort();
                }
                if !CAPTURED.load(Ordering::SeqCst) {
                    emit_status(&app_c, &pid, "closed-empty", None);
                }
            });
        }
    });

    // Start the backup poll task and remember its handle.
    let app_for_poll = app.clone();
    let pid_for_poll = provider_id.clone();
    let handle = async_runtime::spawn(async move {
        poll_loop(app_for_poll, pid_for_poll).await;
    });
    *POLL_TASK.lock().await = Some(handle);

    Ok(())
}

/// Manual fallback: read the jar now, store, close. Use when auto-detect misses
/// (e.g. the WebView session was already authenticated, so no redirect occurred).
#[tauri::command]
pub async fn finish_bailian_login(app: AppHandle, provider_id: String) -> Result<(), String> {
    try_capture_and_close(&app, &provider_id, true).await;
    Ok(())
}
```

- [ ] **Step 5: Register the two commands**

In `src-tauri/src/lib.rs` invoke_handler, after the Task 4 entries add:

```rust
            bailian_login::open_bailian_login,
            bailian_login::finish_bailian_login,
```

- [ ] **Step 6: Add the login-window capability**

Create `src-tauri/capabilities/bailian-login.json`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "bailian-login",
  "description": "Capability for the Bailian login window (loads external HTTPS only; no app IPC)",
  "windows": ["bailian-login"],
  "permissions": ["core:default"]
}
```

- [ ] **Step 7: Build + run the full test suite**

Run: `cargo build --manifest-path src-tauri/Cargo.toml` then `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: build OK; all existing tests still PASS. Fix the API confirm-points (Step preamble) if the compiler flags any.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/bailian_login.rs src-tauri/src/lib.rs src-tauri/capabilities/bailian-login.json
git commit -m "feat(bailian): in-app login window + cookie capture"
```

---

### Task 6: Frontend — cookie-login UI + presence tag + status event

Wire the four commands, the `cookieSet` presence map, the Provider form's `登录百炼控制台`/`完成登录` buttons + `已登录/未登录` tags, the `bailian-login-status` listener, and the `core:event:allow-listen` capability.

**Files:**
- Modify: `src/lib/commands.ts:59-63` (add 4 wrappers after the SK wrappers).
- Modify: `src/lib/types.ts` (add `BailianLoginStatus`).
- Modify: `src/stores/config.ts:12-26, 163-179` (add `cookieSet`, extend `refreshKeys`, return it).
- Modify: `src/views/Provider.vue` (computed + modal section + list-row tag + listener).
- Modify: `src-tauri/capabilities/default.json` (add event-listen permissions).

**Interfaces:**
- Consumes: Tasks 4/5 commands; event `bailian-login-status`.
- Produces: `config.cookieSet: Record<string, boolean>`; the visible `登录百炼控制台`/`完成登录` buttons and `已登录/未登录` tags.

- [ ] **Step 1: Add the command wrappers**

In `src/lib/commands.ts`, after the `providerHasUsageSk` wrapper (line 63), add:

```ts
// Bailian (阿里云百炼) console-cookie login. An in-app WebView window captures the HttpOnly
// session cookie for usage queries; the cookie is stored backend-side (keyring-first). The
// frontend only observes presence + a status event (the cookie value never crosses IPC).
export const openBailianLogin = (providerId: string) =>
  invoke<void>("open_bailian_login", { providerId });
export const finishBailianLogin = (providerId: string) =>
  invoke<void>("finish_bailian_login", { providerId });
export const providerHasUsageCookie = (providerId: string) =>
  invoke<boolean>("provider_has_usage_cookie", { providerId });
export const clearBailianCookie = (providerId: string) =>
  invoke<void>("clear_bailian_cookie", { providerId });
```

- [ ] **Step 2: Add the event payload type**

In `src/lib/types.ts`, add:

```ts
/** `bailian-login-status` event payload emitted by the backend during/after the in-app login flow. */
export interface BailianLoginStatus {
  provider_id: string;
  status: "captured" | "closed-empty" | "error";
  message?: string;
}
```

- [ ] **Step 3: Add `cookieSet` to the config store**

In `src/stores/config.ts`:
- Add the ref next to `usageSkSet` (line 13):

```ts
  const cookieSet = ref<Record<string, boolean>>({});
```

- Extend `refreshKeys` (lines 18-26) to a third parallel fetch:

```ts
  async function refreshKeys() {
    const ids = providers.value.map((p) => p.id);
    const [keyResults, skResults, cookieResults] = await Promise.all([
      Promise.all(ids.map((id) => api.providerHasKey(id).catch(() => false))),
      Promise.all(ids.map((id) => api.providerHasUsageSk(id).catch(() => false))),
      Promise.all(ids.map((id) => api.providerHasUsageCookie(id).catch(() => false))),
    ]);
    keySet.value = Object.fromEntries(ids.map((id, i) => [id, keyResults[i]]));
    usageSkSet.value = Object.fromEntries(ids.map((id, i) => [id, skResults[i]]));
    cookieSet.value = Object.fromEntries(ids.map((id, i) => [id, cookieResults[i]]));
  }
```

- Return `cookieSet` from the store (add to the returned object near `usageSkSet` at line 169):

```ts
    cookieSet,
```

- [ ] **Step 4: Add the Provider form logic + listener**

In `src/views/Provider.vue`:
- Ensure `onUnmounted` is imported from `vue` alongside the existing `onMounted` (line 245 area).
- Add the computed next to `needsUsageCreds` (line 57):

```ts
// 百炼用量查询走控制台 Cookie（HttpOnly），需在应用内弹窗登录后由后端抓取。
const needsBailianLogin = computed(() => form.vendor === "bailian-token");
```

- Add login helpers (near `testConn`, before `onMounted`):

```ts
async function openBailianLogin() {
  try {
    await api.openBailianLogin(form.id);
  } catch (e) {
    msg.error(`打开登录窗口失败：${String(e)}`);
  }
}
async function finishBailianLogin() {
  try {
    await api.finishBailianLogin(form.id);
  } catch (e) {
    msg.error(`完成登录失败：${String(e)}`);
  }
}
```

- Replace the `onMounted` block (line 245) with a version that also registers the status listener:

```ts
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { BailianLoginStatus } from "../lib/types";

let unlistenStatus: UnlistenFn | undefined;
onMounted(async () => {
  await config.loadAll();
  unlistenStatus = await listen<BailianLoginStatus>("bailian-login-status", (e) => {
    const s = e.payload;
    void config.refreshKeys(); // re-fetch cookieSet (+ key/usage maps)
    const name = config.providers.find((p) => p.id === s.provider_id)?.display_name ?? s.provider_id;
    if (s.status === "captured") {
      msg.success(`「${name}」登录成功，Cookie 已获取`);
    } else if (s.status === "error") {
      msg.error(`获取 Cookie 失败：${s.message ?? "未知错误"}`);
    }
    // "closed-empty": user closed without logging in → silent
  });
});
onUnmounted(() => unlistenStatus?.());
```

(Place the two `import` lines with the other imports at the top of `<script setup>`; only the `onMounted`/`onUnmounted` calls go at the bottom.)

- [ ] **Step 5: Add the modal login section + list-row tag**

In the modal `<NForm>` (after the `推理 api_key` form item at line 333, before the `v-if="needsUsageCreds"` template at line 334), insert:

```html
        <template v-if="editing && needsBailianLogin">
          <NFormItem label="用量查询（百炼 · Cookie 登录）">
            <NSpace vertical :size="8" style="width: 100%">
              <NSpace :size="8" align="center">
                <NButton size="small" type="primary" @click="openBailianLogin">登录百炼控制台</NButton>
                <NButton size="small" @click="finishBailianLogin">完成登录</NButton>
                <NTag size="small" :type="config.cookieSet[form.id] ? 'success' : 'warning'">
                  {{ config.cookieSet[form.id] ? "已登录" : "未登录" }}
                </NTag>
              </NSpace>
              <span class="muted">点击「登录百炼控制台」在弹窗中完成阿里云登录，登录成功后自动获取用量查询所需的 Cookie；自动检测失败时点「完成登录」。</span>
            </NSpace>
          </NFormItem>
        </template>
```

In the provider list row (after the `usageSkSet` tag block at lines 276-282), add a Bailian login tag:

```html
                  <NTag
                    v-if="(p.vendor ?? p.id) === 'bailian-token'"
                    size="small"
                    :type="config.cookieSet[p.id] ? 'success' : 'warning'"
                  >
                    {{ config.cookieSet[p.id] ? "已登录" : "未登录" }}
                  </NTag>
```

- [ ] **Step 6: Add the event-listen capability**

In `src-tauri/capabilities/default.json`, add to the `permissions` array:

```json
    "core:event:allow-listen",
    "core:event:allow-unlisten",
```

- [ ] **Step 7: Type-check + build**

Run: `npx vue-tsc --noEmit`
Expected: PASS (no type errors — the new wrappers, `cookieSet`, computed, listener, and `BailianLoginStatus` all resolve).

- [ ] **Step 8: Manual end-to-end verification (Windows, real Aliyun account)**

Run `npm run tauri dev`. Then:
1. Add a `bailian-token` provider (vendor → base_urls auto-fill) → set an inference key → **save**. The login section appears only when editing an existing (saved) Bailian provider.
2. Edit it → click `登录百炼控制台` → complete real Aliyun SSO in the child window → **window auto-closes** → toast `「…」登录成功，Cookie 已获取` → the list-row + modal tags flip to `已登录` (green).
3. Re-open the window (SSO persisted in `<app_data>/bailian_webview`) → the poll captures within ~2 s with no redirect.
4. If auto-detect misses, click `完成登录` → captures manually.
5. Use `clear_bailian_cookie` path: temporarily break the cookie (e.g. re-login flow) → tag returns to `未登录`. (A clear button is not in the UI this round; verify via the store/`provider_has_usage_cookie` returning false after a backend clear, or add a small `退出登录` button if desired — optional.)
6. Existing vendors' usage chips are unchanged (real data / 查询失败; Bailian still `不支持`).

- [ ] **Step 9: Commit**

```bash
git add src/lib/commands.ts src/lib/types.ts src/stores/config.ts src/views/Provider.vue src-tauri/capabilities/default.json
git commit -m "feat(bailian): frontend cookie-login UI + status event"
```

---

## Self-Review (spec coverage)

| Spec section / requirement | Task |
|---|---|
| Embedded Rust-owned WebView window at the console plan URL | Task 5 Step 4 (`open_bailian_login`) |
| Dedicated WebView2 data dir (`<app_data>/bailian_webview`) | Task 5 Step 4 |
| Auto-detect: async jar poll (~2 s, SSO cookie) | Task 5 Steps 3–4 |
| Auto-detect: host-transition fast-path (`on_navigation`, URL-only) | Task 5 Step 4 |
| Windows async caveat — `cookies_for_url` never in sync handler | Task 5 Step 4 (capture spawned async; nav closure inspects URL only) |
| Manual fallback `完成登录` button | Task 5 Step 4 (`finish_bailian_login`) + Task 6 Step 5 |
| Cookie capture: `cookies_for_url` → header string | Task 5 Step 2 + Task 3 serializer |
| Header-injection filter (`\r`/`\n`); browser-faithful order (no dedup) | Task 3 |
| Keyring-first storage via `SecretStore` (`usage_cookie` channel) | Task 1 |
| Oversized-blob file fallback (>2560 B; keyring backend only) | Task 2 |
| Cookie never crosses IPC into JS | Tasks 4/5 (commands return `bool`/`void`/status event) |
| Commands: `open_bailian_login`, `finish_bailian_login`, `provider_has_usage_cookie`, `clear_bailian_cookie` | Tasks 4 + 5 |
| Event `bailian-login-status` emitted + frontend `listen` | Task 5 (emit) + Task 6 Step 4 (listen) |
| `已登录/未登录` presence tag (form + list row) | Task 6 Step 5 |
| `cookieSet` map in config store | Task 6 Step 3 |
| Capabilities: `core:event:allow-listen`/`unlisten` + `bailian-login.json` | Task 5 Step 6 + Task 6 Step 6 |
| Concurrency/leak guards (one window/task; abort on re-open/destroy; poll breaks on window gone) | Task 5 Steps 1–4 |
| No `selectLabel.ts` change; chip stays `不支持` | None (deliberate — capture-only) |
| No dispatch/breaker/translate/catalog/adapter change | None (deliberate) |

Placeholder scan: none — every code step shows exact code. Type consistency: `pairs_to_cookie_header`, `LoginStatus`, `bailian-login-status`, `provider_has_usage_cookie`/`clear_bailian_cookie`/`open_bailian_login`/`finish_bailian_login`, `cookieSet`, `BailianLoginStatus`, `needsBailianLogin` match across all tasks where used. The `set_usage_cookie` delegation added in Task 1 Step 8 is intentionally superseded by the fallback-aware version in Task 2 Step 4 (same method, same signature).
