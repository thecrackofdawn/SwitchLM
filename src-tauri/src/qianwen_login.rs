//! In-app 千问 (Qianwen) console login: open an embedded WebView window, capture the HttpOnly
//! session cookie jar after SSO, and store it for the `usage/qianwen.rs` adapter. See spec
//! `docs/superpowers/specs/2026-08-02-bailian-usage-cookie-capture-design.md`.

use crate::config::secrets::SecretStore;
use crate::proxy::AppState;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{async_runtime, AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder};

/// Whether a captured Qianwen usage cookie is stored for the provider (does not reveal it).
#[tauri::command]
pub async fn provider_has_usage_cookie(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> Result<bool, String> {
    Ok(state.secrets.get_usage_cookie(&provider_id).map_err(|e| e.to_string())?.is_some())
}

/// Clear a stored Qianwen usage cookie (logout / re-login). Idempotent.
#[tauri::command]
pub async fn clear_qianwen_cookie(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> Result<(), String> {
    state.secrets.delete_usage_cookie(&provider_id).map_err(|e| e.to_string())
}

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

/// Monotonic counter so two login-window opens in the same nanosecond still get distinct dirs.
static LOGIN_WEBVIEW_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh, unique temp directory for the login webview's isolated WebView2 profile. Ephemeral:
/// a new dir per login-window open (created by WebView2, removed on window destroy). Nothing is
/// persisted under app_data — the captured cookie lives in the keyring, not here.
fn fresh_login_webview_dir() -> PathBuf {
    let seq = LOGIN_WEBVIEW_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("switchlm-qianwen-login-{nanos}-{seq}"))
}

/// Best-effort removal of the ephemeral login-webview profile. WebView2 may briefly hold file
/// locks after the window closes (especially on Windows), so this retries a few times; a lingering
/// dir lives under the OS temp (reaped eventually), never under app_data.
fn cleanup_login_webview_dir(dir: PathBuf) {
    async_runtime::spawn(async move {
        for _ in 0..6u32 {
            if !dir.exists() {
                return;
            }
            if std::fs::remove_dir_all(&dir).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        tracing::warn!("无法清理临时登录 webview 目录（WebView2 仍占用？）：{}", dir.display());
    });
}

const LOGIN_LABEL: &str = "qianwen-login";
/// The qianwenai page whose login-state cookie the usage adapter needs. Logged-out it redirects
/// to qianwenai SSO, then returns here authenticated.
const LOGIN_URL: &str =
    "https://platform.qianwenai.com/home/billing/subscription/token-plan-individual";
/// Captured cookies are those that would be sent here (qianwenai login state) — used as the
/// `Cookie:` header by the usage adapter's secToken + gateway calls.
const CONSOLE_URL: &str = "https://platform.qianwenai.com/";
/// Cookie name that indicates a successful qianwenai login — CONFIRMED against a real captured
/// jar during manual E2E testing. Capture fires when it appears.
const SSO_COOKIE_NAMES: &[&str] = &["login_qianwenai_ticket"];
/// Hosts the login flow passes through before returning to the console (host-transition fast-path).
/// Additive secondary signal — the SSO-cookie-name poll (above) is the primary auto-detect driver.
/// Kept as Aliyun-SSO-oriented guesses (qianwenai may reuse Alibaba SSO); harmless if they don't match.
const PASSPORT_HOSTS: &[&str] =
    &["passport.aliyun.com", "signin.aliyun.com", "login.aliyun.com", "login.taobao.com", "login.qianwenai.com"];

/// One login window at a time (fixed label): the current poll task. Aborted on re-open, on
/// capture, and on window destroy - so rapid re-clicks and close-then-reopen never leak tasks.
static POLL_TASK: tokio::sync::Mutex<Option<async_runtime::JoinHandle<()>>> =
    tokio::sync::Mutex::const_new(None);
/// Set true when a capture succeeded this session; the Destroyed handler reads it to decide
/// whether to emit "closed-empty" (user closed without logging in).
static CAPTURED: AtomicBool = AtomicBool::new(false);
/// Guards the open path: while true, a concurrent `open_qianwen_login` returns Ok(()) (clean
/// no-op). Reset on every exit via `OpenGuard`, so a failed build never permanently blocks opening.
static OPENING: AtomicBool = AtomicBool::new(false);

/// Drop guard that resets `OPENING` to false. Ensures every exit path (including `?` early
/// returns) releases the open-in-flight flag - a failed build can't permanently block opening.
struct OpenGuard;
impl Drop for OpenGuard {
    fn drop(&mut self) {
        OPENING.store(false, Ordering::SeqCst);
    }
}

#[derive(serde::Serialize, Clone)]
struct LoginStatus {
    provider_id: String,
    status: &'static str, // "captured" | "closed-empty" | "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn emit_status(app: &AppHandle, provider_id: &str, status: &'static str, message: Option<String>) {
    let _ = app.emit(
        "qianwen-login-status",
        LoginStatus { provider_id: provider_id.to_string(), status, message },
    );
}

fn console_url() -> Url {
    Url::parse(CONSOLE_URL).expect("hardcoded console URL parses")
}

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

/// Open the embedded Qianwen login window and start auto-detection. Returns immediately.
#[tauri::command]
pub async fn open_qianwen_login(app: AppHandle, provider_id: String) -> Result<(), String> {
    // Serialize: if another open is in flight (build-in-progress), clean no-op.
    if OPENING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }
    let _guard = OpenGuard; // resets OPENING=false on every exit (incl. ? early returns)

    // Already open -> focus it; don't create a duplicate window or task.
    if let Some(win) = app.get_webview_window(LOGIN_LABEL) {
        let _ = win.set_focus();
        return Ok(());
    }
    // Abort any stale poll task from a prior session.
    if let Some(h) = POLL_TASK.lock().await.take() {
        h.abort();
    }
    CAPTURED.store(false, Ordering::SeqCst);

    let webview_dir = fresh_login_webview_dir();
    let url = Url::parse(LOGIN_URL).map_err(|e| e.to_string())?;
    let saw_passport = Arc::new(std::sync::Mutex::new(false));
    let saw_passport_nav = saw_passport.clone();
    let app_for_nav = app.clone();
    // Clone before the move closure so `provider_id` stays available for the event/poll clones below.
    let pid_for_nav = provider_id.clone();

    let win = WebviewWindowBuilder::new(&app, LOGIN_LABEL, WebviewUrl::External(url))
        .title("登录千问控制台")
        .inner_size(960.0, 680.0)
        .center()
        .resizable(true)
        .data_directory(webview_dir.clone())
        .on_navigation(move |nav_url| {
            // SYNC handler - inspect URL only; NEVER call cookies_for_url here (Windows deadlock).
            if let Some(host) = nav_url.host_str() {
                let already = *saw_passport_nav.lock().unwrap();
                let on_passport = PASSPORT_HOSTS.iter().any(|p| host == *p || host.ends_with(p));
                if on_passport {
                    *saw_passport_nav.lock().unwrap() = true;
                } else if already && host.ends_with("console.aliyun.com") {
                    // Returned to the console after passport -> likely logged in. Spawn async capture.
                    let app_c = app_for_nav.clone();
                    let pid = pid_for_nav.clone();
                    async_runtime::spawn(async move { try_capture_and_close(&app_c, &pid, false).await; });
                }
            }
            true // allow all navigation
        })
        .build()
        .map_err(|e| e.to_string())?;

    // Cleanup on close (X-button or code close): remove the ephemeral webview profile, abort the
    // poll task, and emit closed-empty if nothing was captured.
    let app_for_event = app.clone();
    let pid_for_event = provider_id.clone();
    let cleanup_dir = webview_dir.clone();
    win.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            cleanup_login_webview_dir(cleanup_dir.clone()); // reap the temp WebView2 profile
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
pub async fn finish_qianwen_login(app: AppHandle, provider_id: String) -> Result<(), String> {
    if app.get_webview_window(LOGIN_LABEL).is_none() {
        return Err("登录窗口未打开".into());
    }
    try_capture_and_close(&app, &provider_id, true).await;
    Ok(())
}

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

    #[test]
    fn fresh_login_webview_dir_is_under_temp_and_unique() {
        let a = fresh_login_webview_dir();
        let b = fresh_login_webview_dir();
        assert!(a.starts_with(std::env::temp_dir()), "must live under the OS temp dir, not app_data");
        assert!(b.starts_with(std::env::temp_dir()));
        assert_ne!(a, b, "each login-window open must get a distinct temp dir");
    }
}
