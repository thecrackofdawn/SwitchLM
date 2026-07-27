# Bailian (阿里云百炼) Usage-Cookie Capture — Design

**Date:** 2026-08-02
**Status:** Approved (pending implementation plan)
**Author:** Claude (SwitchLM Project)
**Related:** `2026-08-02-bailian-token-plan-design.md` (this is that spec's *Out-of-scope / Follow-up #1*, scoped down to cookie **capture** only).

> **Amended by `2026-08-02-bailian-usage-adapter-design.md`** in two places: (1) the login URL/domain/SSO-hosts below are corrected from the Aliyun console to the **qianwenai platform** (`platform.qianwenai.com`) — the usage API needs qianwenai login-state cookies; (2) `UsageCreds` **is** extended with a `#[serde(skip)] cookie` field (the "AK/SK-only" statement below is superseded — the cookie is loaded from the same `{provider_id}::usage_cookie` keyring channel). The capture *mechanism* is unchanged.

## Overview

Add an in-app **login window** so a user can authenticate to the Aliyun (阿里云) console *inside* SwitchLM, after which the backend captures the resulting session cookies and stores them securely. These cookies are the credential a future `usage/bailian.rs` quota adapter will need — Bailian exposes plan usage only through an authenticated (cookie-based) console session, not a Bearer/AKSK API.

The deliverable is the **capture capability** end-to-end: open login window → detect login success → read the HttpOnly cookie jar → store securely → reflect "已登录 / 未登录" in the UI. The cookie *consumer* (the `UsageProvider` adapter that turns the cookie into quota numbers) is **out of scope** for this spec — it is blocked on the console quota XHR (endpoint + auth cookies + response shape), which is supplied separately later.

## Background & verified constraints

These facts were checked against the project's pinned dependencies, not assumed:

1. **Tauri `2.11.5` can read webview cookies, including HttpOnly/secure.** `WebviewWindow::cookies()` and `cookies_for_url(url)` (`tauri-2.11.5/src/webview/webview_window.rs:2537,2562`) return `Vec<Cookie>` covering HttpOnly + secure cookies — so the native API reaches the Aliyun SSO ticket that JS `document.cookie` cannot.
2. **Windows async caveat (load-bearing).** Per the Tauri doc comment, `cookies()`/`cookies_for_url()` **deadlock on Windows inside a synchronous command or event handler** (e.g. `on_navigation`'s sync callback; see wry#583). SwitchLM is Windows-primary, so every cookie read MUST run in an `async fn` command or a `tauri::async_runtime::spawn` task — never inline in `on_navigation`.
3. **Windows Credential Manager cap is 2560 bytes.** `windows-sys` defines `CRED_MAX_CREDENTIAL_BLOB_SIZE = 2560`, and `keyring 3.6.3`'s Windows backend (`keyring-3.6.3/src/windows.rs:214`) rejects a blob with a "too long" error above this. The full Aliyun jar is borderline ~1–2 KB+ (the `login_aliyunid_ticket` SSO ticket dominates), so for some accounts it can exceed 2560 B. The storage design handles this explicitly (see *Cookie storage*).

## Scope

**In scope (this spec):**
- Embedded WebView login window (first multi-window code in the repo), Rust-owned.
- Login-success detection (auto, with a manual fallback button).
- Cookie extraction + secure storage (keyring-first via the existing `SecretStore`, with an oversized-blob file fallback).
- Frontend affordance in the provider form + "已登录/未登录" presence tag.
- The capability/permission additions this requires.

**Out of scope (separate spec, later):**
- `src-tauri/src/usage/bailian.rs` — the `UsageProvider` adapter that *uses* the stored cookie. Blocked on the console quota XHR.
- Adding `bailian-token` to `USAGE_ADAPTER_VENDORS` (`src/lib/selectLabel.ts`) and the UI flip from "不支持" to real quota data.
- Stale-cookie detection / auto re-prompt (lands with the adapter, which is what can detect an expired session).

## Requirements

### Functional

1. **In-app login window.** From the provider form, when `vendor === "bailian-token"`, a `登录百炼控制台` button opens a dedicated child WebView window at
   `https://bailian.console.aliyun.com/cn-beijing?tab=plan&commonbuy=1#/efm/subscription/coding-plan`.
   The user completes real Aliyun SSO (scan / SMS / password) inside it. SwitchLM never sees the password — it is typed directly to Aliyun.
2. **Auto-detect login success** and capture + close the window without user action. A `完成登录` manual button is the fallback when auto-detect misses (e.g. the WebView session was already authenticated from a prior login, so no redirect occurs).
3. **Capture the HttpOnly cookie jar** for the console domain (the set a request to `bailian.console.aliyun.com` would carry), serialize it to a `Cookie:` header string, and store it securely per provider.
4. **Keyring-first storage** following the existing secrets convention: the cookie is stored through the `SecretStore` abstraction (keyring when available, file when not) — exactly like the Volcengine `usage_sk`. The cookie never passes through the frontend/IPC.
5. **Presence tag.** The provider form and provider-list row show `已登录` / `未登录` for `bailian-token` providers, mirroring the existing 密钥 / 用量SK tags. It flips to `已登录` after a successful capture.
6. **Logout / re-login.** A `clear_bailian_cookie(provider_id)` command clears the stored cookie so the tag returns to `未登录` and the user can re-login (console sessions expire).
7. **Multiple accounts.** Each Bailian provider (each gets its own opaque `id`) has its own login + cookie, like every vendor.

### Non-functional

- No change to dispatch / breaker / translate / catalog / the existing usage adapters. `usage_provider_for("bailian-token")` still returns `None`, so the usage chip still reads "不支持" until the adapter spec lands.
- No new backend HTTP signing; the (deferred) adapter will reuse `reqwest` with a `Cookie:` header.
- Backward compatible: nothing changes for existing vendors or configs.

## Design

### Window lifecycle (Rust-owned)

A new async command `open_bailian_login(provider_id)` builds the login window **in Rust** via `WebviewWindowBuilder` (the repo's first multi-window use — today only `app.get_webview_window("main")` exists, in the single-instance handler at `lib.rs:50-55`). Rust owns the window because (a) that is where `cookies()` lives and (b) the Windows async-deadlock caveat must be handled in Rust.

- Label: `"bailian-login"`. Initial URL: the console plan page above. Size ~960×680, centered, resizable.
- **Dedicated WebView2 data directory** via `WebviewWindowBuilder::data_directory(<app_data>/bailian_webview)`: the Aliyun SSO session persists *within SwitchLM* across opens (re-login only when it truly expires) and never pollutes the main window's storage.
- The existing `CloseRequested` hide-to-tray handler (`lib.rs:128-136`) is scoped to `window.label() == "main"`, so the login window's X button closes it normally — that path emits `status: "closed-empty"` if nothing was captured.

### Login-success detection (auto + manual fallback)

Three layered signals; the first to fire triggers the shared async capture routine:

1. **Primary — async jar poll (most reliable).** A `tauri::async_runtime::spawn` task, tied to the window's lifetime, calls `window.cookies_for_url("https://bailian.console.aliyun.com/")` every ~2 s (Windows-safe: it is an async task, not a sync handler). It fires capture when an SSO auth cookie appears. Candidate names: `login_aliyunid_ticket`, `login_aliyunid` (exact name confirmed at implementation by inspecting a real jar). This catches SPA/SSO completion that produces no top-level navigation, and also the "already authed on re-open" case.
2. **Fast path — host transition.** `on_navigation` (sync — it inspects only the URL string, never calls `cookies()`) records when the user navigates through a passport host (`passport.aliyun.com`, `signin.aliyun.com`, `login.aliyun.com`, `login.taobao.com`) and then lands back on `*.console.aliyun.com`. That transition spawns one async capture attempt.
3. **Fallback — `完成登录` button** in the provider form → calls `finish_bailian_login(provider_id)`, an async command that reads the jar immediately, stores it, and closes the window. Covers any auto-detect miss.

The shared capture routine: read `cookies_for_url(console_url)`; if the console-domain jar is **non-empty**, serialize + store + close window + emit `captured`; if empty, do nothing (keep waiting) for the auto signals, or emit `error` ("未检测到 Cookie，请先完成登录") for the manual button.

The poll task and the login window share a single concurrency-guarded lifecycle (the window label is fixed, so there is **one login window at a time**):

- **One window / one task.** The `bailian_login` module holds the current poll `JoinHandle` behind a `tokio::sync::Mutex<Option<JoinHandle<()>>>`. `open_bailian_login` first calls `app.get_webview_window("bailian-login")`: if it already exists, it just focuses it and returns (no duplicate window, no duplicate task — covers rapid double-clicks and close-then-reopen). Otherwise it `abort()`s any stale task handle, builds the window, spawns the new poll task, and stores its handle.
- **Cleanup on close.** The window's `on_window_event` listening for `WindowEvent::Destroyed` aborts the poll task and clears the handle — this is what covers the X-button force-close path.
- **Poll is close-aware.** The poll loop treats a `cookies_for_url` `Err` caused by the window being destroyed as a `break` (exit), so it never spams `WindowNotFound` after close. (Capture/closed-empty also tear down the task.)

### Cookie capture & storage

**Capture format.** The capture site maps each cookie to a `(name, value)` pair and calls `pairs_to_cookie_header(pairs: &[(&str, &str)]) -> String`, which joins them as `name=value` with `"; "` (the exact shape a `Cookie:` request header takes); the future adapter sends it verbatim. Taking string pairs (not the `Cookie` type) keeps the serializer pure and unit-testable without webview machinery. It applies one guard:
- **Header-injection filter:** skip any pair whose name or value contains `\r` or `\n`, so a malformed value can't inject into the eventual HTTP header.

Order is preserved verbatim from `cookies_for_url`, **including legitimate same-name / different-domain duplicates** — browsers send all matching cookies (RFC 6265 §5.4), so the future adapter must mimic that; deliberately **no cross-domain dedup** (dropping a same-name cookie could break the quota API).

Tracking-cookie trimming (dropping `_bl_uid`/`umdistinctid`/`isg`/…) is **deferred, deliberately**: we don't yet know which names the quota API needs, and guessing risks dropping an auth cookie (e.g. `cna` is sometimes required). The stored set is exactly what `cookies_for_url` resolves for the console URL — already the minimal "would-be-sent" set — and the keyring-oversize file fallback absorbs any size. Once the adapter identifies the required cookies, a name allowlist can trim the blob. This pure function is unit-tested; it lives in the new `bailian_login` module.

**Storage — keyring-first via `SecretStore` (exactly like `usage_sk`).** Extend the `SecretStore` trait (`config/secrets.rs:58-67`) with three methods mirroring the SK channel:

```rust
fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError>;
fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError>;
fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError>;
```

- Keyring entry name: `format!("{provider_id}::usage_cookie")` (sibling to `::usage_sk` at `secrets.rs:74-76`).
- Implement across **all five backends** (`KeyringStore`, `MemoryStore`, `FileSecretStore`, `PendingStore`) and **delegate in `SecretStoreHandle`** (`secrets.rs:279-305`) — any new trait method MUST be delegated there or calls silently miss the backend.
- The existing `select_backend` (`secrets.rs:28-36`) does the keyring→file choice unchanged: probe keyring → `KeyringStore`; else `FileSecretStore` if `secret_store_fallback`; else `PendingStore`. This is the user's "prefer keyring, use file when no keyring."
- `UsageCreds` (`config/types.rs:120-132`) is **not** extended — the cookie is a parallel keyring channel, keeping `UsageCreds` AK/SK-specific. The future adapter reads it directly via `state.secrets.get_usage_cookie(provider_id)`.

**Oversized-blob fallback (the 2560 B edge case).** Some Aliyun jars exceed the Windows Credential Manager's 2560-byte cap, so `KeyringStore::set_usage_cookie` would error "too long". To keep capture robust on the primary platform, `SecretStoreHandle` holds an always-constructed `FileSecretStore` instance and, **in the cookie methods only**, falls back to it when the primary (keyring) backend rejects an oversized blob:
- `set_usage_cookie`: try primary; on a size/`NoSpace`-style error *and primary is keyring*, write to the file store.
- `get_usage_cookie`: try primary; if `None` and primary is keyring, try the file store.
- `delete_usage_cookie`: delete from both (idempotent).

Net behavior: keyring preferred; file used when there is no keyring **or** the keyring rejects this oversized blob. Capture never silently dies. (If testing shows real jars reliably fit in 2560 B, this fallback is still harmless and can be kept or trimmed.)

**No command returns the cookie to the frontend.** Unlike `usage_sk` (which the user types into a form field and sends over IPC), the cookie is written inside the Rust capture routine. `open_bailian_login`/`finish_bailian_login` write it but return only a status event; `provider_has_usage_cookie` returns only a `bool`; `clear_bailian_cookie` returns nothing. The cookie value never crosses IPC into JS — a security improvement over the SK flow.

### Backend commands (new module `src-tauri/src/bailian_login.rs`, registered in `lib.rs` invoke_handler)

| Command | Kind | Purpose |
|---|---|---|
| `open_bailian_login(provider_id)` | `async` | Build + show the login window; start detection. Returns immediately. |
| `finish_bailian_login(provider_id)` | `async` | Manual fallback: read jar now, store, close window, emit status. |
| `provider_has_usage_cookie(provider_id)` | `async` | `bool` for the presence tag (mirrors `provider_has_usage_sk`, `commands.rs:391-420`). |
| `clear_bailian_cookie(provider_id)` | `async` | Delete stored cookie (logout / re-login). |

On capture, Rust emits a `bailian-login-status` event: `{ provider_id: String, status: "captured" | "closed-empty" | "error", message: Option<String> }`.

### Frontend

- **`src/views/Provider.vue`** — add a `needsBailianLogin = computed(() => form.vendor === "bailian-token")` and a `v-if="needsBailianLogin"` form section with:
  - `登录百炼控制台` → `api.openBailianLogin(provider.id)`.
  - `完成登录` → `api.finishBailianLogin(provider.id)` (the manual fallback).
  - a presence tag reading `config.cookieSet[p.id]` ? `已登录` : `未登录`.
  The provider-list row tag gains the same `已登录` / `未登录` for `bailian-token`, next to the existing 密钥 / 用量SK tags (`Provider.vue:274-283`). `vendorDefaults` (`Provider.vue:77-80`) and `needsUsageCreds` (`Provider.vue:57`) are unchanged.
- **`src/stores/config.ts`** — add a `cookieSet: Record<string, boolean>` map populated in `refreshKeys()` (`config.ts:18-26`) via `providerHasUsageCookie` (a third entry in the existing `Promise.all`, with `.catch(() => false)`), and returned alongside `keySet`/`usageSkSet` (`config.ts:168-169`).
- **`src/lib/commands.ts`** — typed wrappers `openBailianLogin`, `finishBailianLogin`, `providerHasUsageCookie`, `clearBailianCookie`, mirroring `setProviderUsageSk`/`providerHasUsageSk` (`commands.ts:60-63`). Also a `listen("bailian-login-status", …)` (first event-listener use in the app) to refresh `cookieSet` and show a toast on `captured`/`error`.
- **`src/lib/types.ts`** — no Rust struct changes (cookie is backend-only); add only the event payload type if desired.
- **`src/lib/selectLabel.ts`** — **no change** for capture-only. `USAGE_ADAPTER_VENDORS` (line 60) stays without `bailian-token`, so the usage chip correctly keeps showing "不支持" until the adapter exists. Cookie login is independent of the chip.

### Capabilities / permissions

- **`src-tauri/capabilities/default.json`** — add `core:event:allow-listen` and `core:event:allow-unlisten` so the main window can hear `bailian-login-status` (the app's first `listen()` use; today no `core:event:*` or `core:webview:*` permissions exist).
- **`src-tauri/capabilities/bailian-login.json`** (new) — a minimal capability scoped to `"windows": ["bailian-login"]` so the login window has core permissions. It loads external HTTPS and performs no app-IPC, so no further permissions are required. (Creating a `WebviewWindow` from Rust needs no capability; capabilities govern frontend↔backend IPC.)
- **Hard rule: the `bailian-login` window must never `invoke` any Tauri command.** It only loads Aliyun pages; all capture logic runs in Rust-side tasks (spawned by `open_bailian_login`/`finish_bailian_login`) or is triggered from the *main* window (the `完成登录` button lives in the provider form, not in the login window). Because the login window uses a separate WebView2 data directory, it does not share the main window's Tauri IPC channels anyway — this rule keeps that isolation airtight.
- Loading external HTTPS in the login window requires no allowlist in Tauri v2 (unlike v1's navigation allowlist); `security.csp` stays `null`.

### Security & privacy

The login window loads real Aliyun pages in an embedded WebView2. SwitchLM reads **only** the resulting cookie jar — it never sees, logs, or intercepts the password (typed directly to Aliyun) or the page content. The captured cookie is a session credential stored via the same keyring-first mechanism as the other secrets, with the file fallback limited to the size edge case; it never touches `app_config.json` or the frontend.

## Testing

- **Backend unit (`cargo test`):**
  - `pairs_to_cookie_header` serialization (multi-pair ordering, empty list, **preserves legitimate same-name/different-domain duplicates**, **`\r`/`\n` injection rejection**).
  - The `SecretStore` cookie trio round-trip:
    - directly on `MemoryStore` and `FileSecretStore` (temp dir): set → get → delete; entry naming `{id}::usage_cookie`.
    - **through `SecretStoreHandle`** wrapping each backend — guards against a missed delegation silently no-op'ing (the trait-add failure mode; per [[test-real-impl-not-just-fake]]).
  - The oversized-blob fallback path: simulate a keyring-size rejection and assert the handle writes/reads via the file store.
  - Per [[test-real-impl-not-just-fake]], also exercise the real `KeyringStore` path where the test environment has a keyring (guard for `NoBackendAccess` so it doesn't masquerade as success).
- **Frontend:** `npx vue-tsc --noEmit` (covers the new command wrappers, `cookieSet`, the computed, and the event payload type).
- **Manual (Windows, real account):**
  1. Add a `bailian-token` provider → click `登录百炼控制台` → complete real Aliyun SSO in the child window → window auto-closes → tag flips to `已登录` → cookie present in keyring (or file fallback).
  2. Re-open the window (session persisted in the dedicated data dir) → poll captures within ~2 s without a redirect.
  3. `完成登录` manual button captures when auto-detect would miss.
  4. `clear_bailian_cookie` → tag returns to `未登录`.
  5. Existing vendors' usage chips are unaffected (still real data / 查询失败; Bailian still "不支持").

## Open implementation details (to confirm while coding)

- Exact SSO cookie name(s) for the poll's success check — inspect a real captured jar.
- Whether `cookies_for_url` from `tauri::async_runtime::spawn` needs to marshal to the UI thread on WebView2; if so, read via an `async` command invoked from the spawned task (still async, still Windows-safe).
- Realistic jar size vs the 2560 B cap on the target account, to confirm whether the oversized fallback ever triggers in practice.

## Follow-ups discovered during implementation (deferred, non-blocking)

These surfaced in the per-task/final reviews; all bounded, none block this capture-only release. Triage when the usage-adapter spec lands:

- **Duplicate-capture guard.** `try_capture_and_close` has no entry guard, so the nav fast-path and the 2 s poll can both invoke it within the ~10–50 ms capture window → a double `captured` toast (store is idempotent, no data loss). Fix: a `CAPTURED.compare_exchange(false, true, …)` claim at the top of `try_capture_and_close`.
- **Cross-provider attribution.** With the fixed window label, opening login for provider B while A's window is already open just focuses A and any capture stores under A's id (needs two `bailian-token` providers + a mid-login modal switch). Consider emitting `error` ("已有其它百炼账号正在登录") on the "already open + different provider" path. Real multi-Aliyun-account also needs per-account webview isolation (the `<app_data>/bailian_webview` session is shared).
- **`退出登录` button.** `clear_bailian_cookie` is wired in Rust + has a TS wrapper but no UI button (wrapper currently unused). Add a `退出登录` button in the provider form when `vendor === "bailian-token" && cookieSet[id]`.
- **Verify SSO cookie names** (`login_aliyunid_ticket` / `login_aliyunid` are candidates) against a real jar during the first manual login.
- **Empty-header guard.** If every pair is dropped by the CRLF injection filter (pathological), an empty header would be stored and `provider_has_usage_cookie` would return `true` with an invalid cookie. Add `if header.is_empty() { treat as empty jar }`.
- **Frontend listener-leak window.** Async `onMounted` + unmount during the `await listen(...)` before `unlistenStatus` is assigned leaks the listener (narrow; Provider.vue is a long-lived tab). Fix: a `cancelled` flag set in `onUnmounted`, checked after the `await`.
- **Tighten the login-window capability.** `bailian-login.json` grants `core:default`; the hard rule says the window must never invoke commands. Custom SwitchLM commands aren't granted (no exfiltration path), but `permissions: []` is cleaner defense-in-depth.
