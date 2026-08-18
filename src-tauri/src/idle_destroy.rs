//! 后台 webview 销毁：窗口隐藏到托盘满 IDLE_DESTROY_SECS 后销毁 webview 以释放内存。
//! 代理/托盘不受影响；重新唤起时由 summon_main_window 重建窗口。见
//! docs/superpowers/specs/2026-08-14-background-webview-destroy-design.md。

use std::sync::{Arc, Mutex};
use tauri::Manager;

/// 隐藏到托盘多少秒后销毁 webview（固定 5 分钟，按设计决策不可配置）。
pub const IDLE_DESTROY_SECS: u64 = 300;

/// 持有当前在途的销毁计时任务。arm 时替换；cancel 时置空。经 app.manage 注入。
///
/// 存储的是 `tauri::async_runtime::JoinHandle`（tauri 2.11.5 的 spawn 返回类型，
/// 非 tokio 原生 JoinHandle）。
#[derive(Clone, Default)]
pub struct IdleDestroyHandle {
    task: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
}

impl IdleDestroyHandle {
    /// 用新任务替换当前任务：先 abort 旧的，再写入新的。返回旧任务（供测试断言）。
    fn swap(&self, next: Option<tauri::async_runtime::JoinHandle<()>>) -> Option<tauri::async_runtime::JoinHandle<()>> {
        let mut guard = self.task.lock().expect("idle destroy handle poisoned");
        let prev = guard.take();
        if let Some(h) = &prev {
            h.abort();
        }
        *guard = next;
        prev
    }

    /// 取消当前任务并返回它（供测试断言），不做 abort 之外的清理。
    fn take(&self) -> Option<tauri::async_runtime::JoinHandle<()>> {
        let mut guard = self.task.lock().expect("idle destroy handle poisoned");
        let prev = guard.take();
        if let Some(h) = &prev {
            h.abort();
        }
        prev
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        self.task.lock().expect("idle destroy handle poisoned").is_none()
    }
}

/// 读取后台销毁是否开启。
pub async fn destroy_in_background_mode(app: &tauri::AppHandle) -> bool {
    use crate::proxy::AppState;
    match app.try_state::<AppState>() {
        Some(state) => state.config.read().await.settings.background_destroy,
        None => false,
    }
}

/// 当窗口隐藏到托盘时调用：若设置开启且窗口当前隐藏，启动一个 5 分钟计时任务，
/// 到点后再次核对设置与可见性，若仍满足则销毁 webview。已存在计时则先取消再重建（幂等）。
pub async fn arm_idle_destroy(app: &tauri::AppHandle) {
    if !destroy_in_background_mode(app).await {
        return;
    }
    // 仅在窗口确实隐藏时计 armed：可见窗口无内存浪费可省。
    let hidden = match app.get_webview_window("main") {
        Some(w) => !w.is_visible().unwrap_or(false),
        None => true, // 窗口已被销毁——无需再 armed（无可销毁之物）。
    };
    if !hidden {
        return;
    }
    let handle = app.state::<IdleDestroyHandle>();
    let app_cloned = app.clone();
    let next = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(IDLE_DESTROY_SECS)).await;
        // 到点再次核对：用户可能在此期间关闭了开关或重新打开了窗口。
        if !destroy_in_background_mode(&app_cloned).await {
            return;
        }
        if let Some(w) = app_cloned.get_webview_window("main") {
            if !w.is_visible().unwrap_or(false) {
                tracing::info!(
                    "主窗口进入后台 {} 秒，已销毁 webview 以节省内存",
                    IDLE_DESTROY_SECS
                );
                let _ = w.destroy();
            }
        }
    });
    handle.swap(Some(next));
}

/// 当窗口被唤起（或开关关闭）时调用：取消任何在途的销毁计时。幂等。
pub fn cancel_idle_destroy(app: &tauri::AppHandle) {
    if let Some(handle) = app.try_state::<IdleDestroyHandle>() {
        handle.take();
    }
}

/// 用 tauri.conf.json 中声明的 "main" 窗口属性重建窗口（销毁后唤起路径）。
/// 复现 title/尺寸/min 尺寸/visible/dragDrop，与静态声明保持一致。
fn recreate_main_window(app: &tauri::AppHandle) {
    use tauri::webview::WebviewWindowBuilder;
    use tauri::WebviewUrl;
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("SwitchLM")
        .inner_size(960.0, 680.0)
        .min_inner_size(940.0, 600.0)
        .visible(true)
        // 与 tauri.conf.json 的 dragDropEnabled:false 对齐：复现静态窗口走的同一条
        // webview 级路径（WindowConfig→WebviewAttributes::disable_drag_drop_handler），
        // 而非窗口级 drag_and_drop（后者 Windows-only 且关闭的是另一套原生 DnD）。
        // 该方法是跨平台的，无需 cfg 门控。HTML5 拖拽重排（vuedraggable）依赖此关闭。
        .disable_drag_drop_handler();
    if let Err(e) = builder.build() {
        // 极少情况：Destroyed 事件尚未处理完，label 仍占用。记录并放弃——
        // 用户下次点击托盘时 get_webview_window 会命中已存在的窗口走 show 分支。
        tracing::warn!("重建主窗口失败（label 可能仍被占用）：{e}");
    }
}

/// 唤起主窗口：先取消任何在途销毁计时；若窗口已被销毁则重建，否则显示并聚焦。
/// 供托盘点击、第二实例启动等所有“显示窗口”路径统一调用。
pub async fn summon_main_window(app: &tauri::AppHandle) {
    cancel_idle_destroy(app);
    match app.get_webview_window("main") {
        None => recreate_main_window(app),
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn holder_starts_idle_and_take_is_noop() {
        let h = IdleDestroyHandle::default();
        assert!(h.is_idle());
        assert!(h.take().is_none()); // 空状态下取消是 no-op，不 panic
        assert!(h.is_idle());
    }

    #[tokio::test]
    async fn swap_replaces_and_aborts_prior_task() {
        let h = IdleDestroyHandle::default();
        let forever = tauri::async_runtime::spawn(async { /* 永不自然结束 */ });
        let prev = h.swap(Some(forever));
        assert!(prev.is_none()); // 首次 swap 之前是空
        assert!(!h.is_idle());

        let forever2 = tauri::async_runtime::spawn(async {});
        let prev2 = h.swap(Some(forever2));
        assert!(prev2.is_some()); // 第二次 swap 返回并 abort 了第一个任务
        assert!(!h.is_idle());

        let prev3 = h.take();
        assert!(prev3.is_some()); // take 返回并 abort 了第二个任务
        assert!(h.is_idle());
    }
}
