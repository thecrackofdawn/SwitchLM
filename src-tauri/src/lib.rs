pub mod qianwen_login;
pub mod commands;
pub mod config;
pub mod proxy;
pub mod translate;
pub mod tray;
pub mod usage;
pub mod logging;

use std::sync::{Arc, Mutex};

use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;

use crate::proxy::{server, AppStateInner};

async fn start_proxy_with_retry(state: proxy::AppState, port: u16) {
    match server::serve_once(state.clone(), port).await {
        Ok((handle, bound_port)) => {
            state.set_server_handle(handle);
            state.set_bound_port(bound_port);
            tracing::info!("代理已在端口 {bound_port} 上启动");
        }
        Err(e) => {
            // Port binding failed
            let error_msg = crate::commands::bind_error_message(port, &e);
            state.set_bind_error(error_msg);
            state.start_polling(port).await;
            tracing::warn!("端口 {port} 绑定失败：{e}，已启动自动重试");
        }
    }
}

/// True when this process was launched by the OS autostart entry, which appends
/// `--autostart` to the command line. Used in `setup` to keep the main window hidden
/// on boot launches while showing it on every other launch (manual open, dev).
fn is_autostart_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter().any(|a| a.as_ref() == "--autostart")
}

/// Whether startup should recreate the OS autostart entry. Heals only the
/// true→missing case (user wants it on but it's confirmed off) — never the reverse,
/// so we don't fight an entry the user set up or tore down by other means.
/// `is_enabled` is `Some(b)` for `Ok(b)` from the probe, `None` for an `Err`.
fn autostart_needs_reenable(desired_on: bool, is_enabled: Option<bool>) -> bool {
    desired_on && is_enabled == Some(false)
}

/// Best-effort reconcile of the OS autostart entry with the persisted preference.
///
/// After a reinstall/uninstall or a Tauri `identifier` change, `settings.autostart`
/// can read `true` while the OS entry is already gone. Recreate it so the user's
/// declared intent ("boot at login") survives a reinstall. A failure is logged and
/// swallowed — autostart must never block startup. Only heals true→missing.
fn reconcile_autostart(app: &tauri::AppHandle, desired_on: bool) {
    let manager = app.autolaunch();
    if !autostart_needs_reenable(desired_on, manager.is_enabled().ok()) {
        return;
    }
    tracing::info!("开机自启：系统启动项缺失（重装/identifier 变更/外部清理），按持久化偏好重新写入");
    if let Err(e) = manager.enable() {
        tracing::warn!("重新写入开机自启失败（已忽略）：{e}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // Single-instance: a second launch (e.g. clicking the app icon again while it's already
    // running in the tray) must not start a new proxy/port. Instead it shows + focuses the
    // existing main window, mirroring the tray "打开主窗口" handler. Must be the first plugin so
    // it can short-circuit before anything else initialises. Desktop-only (no mobile support).
    //
    // 仅 release 启用:dev 下(`npm run tauri dev`，debug_assertions)放行多实例，便于并行
    // 跑测试。代价是 dev 多实例会抢同一端口(6950)--端口占用时不会绑到别的端口，而是显示
    // bindError 并轮询原端口;要让两个实例都跑代理，需在设置里给其中一个改用不同端口。
    //
    // `let builder = builder.plugin(...)` shadows the binding under cfg, so `builder` needs no
    // `mut`: in release the shadowed value (with the plugin) flows on; in dev the cfg statement
    // is absent and the original binding is used. (A plain `let mut` + block assignment would
    // trip `unused_mut` in dev, since the reassignment is cfg'd out.)
    #[cfg(all(desktop, not(debug_assertions)))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let log_dir = dir.join("logs");
            let log_handle = crate::logging::init_logging(
                &log_dir,
                tracing_subscriber::filter::LevelFilter::INFO,
            );
            // 开机自启启动的实例（argv 含 `--autostart`）保持主窗口隐藏，托盘/代理正常运行；
            // 其余启动（手动打开、`tauri dev`）显式 show——tauri.conf.json 已将 visible 置为
            // false 以免开机时先闪一下窗口再隐藏。放在 setup 最前（fallible 配置/密钥环之前），
            // 这样即使后续步骤失败，手动启动也已 show 过窗口，不会卡在隐藏状态。
            if is_autostart_launch(std::env::args()) {
                tracing::info!("开机自启启动，主窗口保持隐藏（托盘/代理正常运行）");
            } else if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            // 探测系统密钥环 → 选后端（keyring 可用→KeyringStore；不可用且已授权→FileSecretStore；
            // 否则→PendingStore，前端弹授权框，授权后 swap 到 FileSecretStore）。
            let probe_ok = config::secrets::keyring_available();
            let mut cfg = config::store::load(&dir)?;
            let kind = config::secrets::select_backend(probe_ok, cfg.settings.secret_store_fallback);
            let store = config::secrets::make_store(kind, &dir)?;
            // 非 pending：立即把遗留明文 SK 从 app_config.json 迁进密钥存储，再重载干净配置。
            // pending：推迟到 grant_secret_consent（避免授权前向 secrets.json 写入）。
            if kind != config::secrets::BackendKind::Pending {
                config::store::migrate_usage_sk_to_keyring(&dir, store.as_ref())?;
                cfg = config::store::load(&dir)?;
            }
            if config::store::normalize_legacy_vendors(&mut cfg) {
                let _ = config::store::save(&dir, &cfg);
            }
            // 开机自启对账：重装/卸载/identifier 变更后系统启动项可能已消失，而 settings.autostart
            // 仍为 true——按用户偏好把启动项重新写回（best-effort，失败仅告警，绝不阻断启动）。
            reconcile_autostart(app.handle(), cfg.settings.autostart);
            let secrets = config::SecretStoreHandle::new(store, kind);
            // Cookie 现分片存入 keyring（绕过单条目字节上限），文件回退已移除。若旧版本曾把超大
            // cookie 落到 secrets.json，启动时一次性迁回 keyring 并删除该明文文件。
            if matches!(kind, config::secrets::BackendKind::Keyring) {
                if let Err(e) = config::secrets::migrate_cookie_file_to_store(&dir, &secrets) {
                    tracing::warn!("迁移遗留 cookie 文件失败：{e}");
                }
            }
            let catalog = config::catalog::ensure_catalog(&dir);
            let preferred = cfg.settings.port;
            crate::logging::set_level(
                &log_handle,
                crate::logging::level_filter_for(&cfg.settings.log_level),
            );
            let cfg_for_tray = cfg.clone();
            let state: proxy::AppState = Arc::new(AppStateInner {
                config: tokio::sync::RwLock::new(cfg),
                catalog: tokio::sync::RwLock::new(catalog),
                secrets,
                health: proxy::HealthRegistry::default(),
                clock: Arc::new(proxy::SystemClock),
                usage_cache: usage::UsageCache::default(),
                bound_port: Mutex::new(None),
                server_handle: Mutex::new(None),
                bind_error: Mutex::new(None),
                polling_handle: Mutex::new(None),
                last_served_provider: Mutex::new(None),
            });

            let state_for_server = state.clone();
            tauri::async_runtime::spawn(async move {
                start_proxy_with_retry(state_for_server, preferred).await;
            });
            app.manage(state);
            app.manage(log_handle);

            // System tray: per-Profile backing switch + inline quota % + cooling markers.
            if let Err(e) = tray::build_tray(app.handle(), &cfg_for_tray) {
                tracing::error!("tray setup failed: {e}");
            }
            // Keep the tray menu current (quota %, cooling markers, backing selection). Tauri v2
            // has no "menu about-to-show" hook, so we refresh on a timer (60s usage cache hits).
            let refresh_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tray::refresh_tray_menu(&refresh_handle).await;
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close button -> hide to tray (proxy + tray keep running); quit via tray 退出.
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_providers,
            commands::get_models,
            commands::get_profiles,
            commands::get_usage_order,
            commands::set_usage_order,
            commands::get_route_order,
            commands::set_route_order,
            commands::upsert_provider,
            commands::check_provider_conflict,
            commands::delete_provider,
            commands::upsert_model,
            commands::delete_model,
            commands::upsert_profile,
            commands::delete_profile,
            commands::route_effective_models,
            commands::model_effective_fallbacks,
            commands::set_profile_strategies_enabled,
            commands::discover_models,
            commands::test_provider_connection,
            commands::test_provider_connection_with_prompt,
            commands::set_provider_key,
            commands::provider_has_key,
            commands::set_provider_usage_sk,
            commands::provider_has_usage_sk,
            qianwen_login::provider_has_usage_cookie,
            qianwen_login::clear_qianwen_cookie,
            qianwen_login::open_qianwen_login,
            qianwen_login::finish_qianwen_login,
            commands::get_usage,
            commands::get_all_usage,
            commands::get_fallback_map,
            commands::set_model_fallback,
            commands::set_model_failover,
            commands::set_model_fallback_strategies_enabled,
            commands::validate_fallback_context,
            commands::recognized_context_size,
            commands::set_custom_context_size,
            commands::get_model_health,
            commands::get_server_status,
            commands::get_port,
            commands::get_bind_error,
            commands::get_env_snippet,
            commands::restart_server,
            commands::quit_app,
            commands::get_secret_status,
            commands::grant_secret_consent,
            commands::toggle_autostart,
            commands::get_settings,
            commands::set_port,
            commands::set_usage_refresh_interval,
            commands::set_log_level,
            commands::open_log_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SwitchLM");
}

#[cfg(test)]
mod tests {
    use super::{autostart_needs_reenable, is_autostart_launch};

    #[test]
    fn autostart_flag_present() {
        assert!(is_autostart_launch(["switchlm", "--autostart"]));
    }

    #[test]
    fn autostart_flag_absent() {
        assert!(!is_autostart_launch(["switchlm"]));
        assert!(!is_autostart_launch(Vec::<String>::new()));
    }

    #[test]
    fn autostart_flag_among_other_args() {
        assert!(is_autostart_launch(["switchlm", "--foo", "--autostart", "bar"]));
    }

    #[test]
    fn no_false_positive_on_lookalike() {
        // a flag that merely contains the substring must not match
        assert!(!is_autostart_launch(["switchlm", "--no-autostart", "--autostart-x"]));
    }

    #[test]
    fn autostart_needs_reenable_only_when_on_but_missing() {
        assert!(autostart_needs_reenable(true, Some(false)));
    }

    #[test]
    fn autostart_needs_reenable_not_when_already_enabled() {
        assert!(!autostart_needs_reenable(true, Some(true)));
    }

    #[test]
    fn autostart_needs_reenable_never_when_desired_off() {
        // Never fights an entry on the false side.
        assert!(!autostart_needs_reenable(false, Some(false)));
        assert!(!autostart_needs_reenable(false, Some(true)));
    }

    #[test]
    fn autostart_needs_reenable_not_when_state_unverifiable() {
        // Probe errored → don't risk a wrong re-enable.
        assert!(!autostart_needs_reenable(true, None));
    }
}
