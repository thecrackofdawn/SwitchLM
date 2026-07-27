use axum::{routing::post, Router};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::proxy::{anthropic_edge, openai_edge, AppState};

const MAX_PORT_ATTEMPTS: u16 = 16;

pub fn pick_port(start: u16, is_free: impl Fn(u16) -> bool) -> Option<u16> {
    (0..MAX_PORT_ATTEMPTS)
        .map(|i| start.saturating_add(i))
        .find(|p| is_free(*p))
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(openai_edge::chat_completions))
        .route("/v1/messages", post(anthropic_edge::messages))
        .with_state(state)
}

/// Bind to `preferred_port` (auto-incrementing on conflict), then serve.
/// Returns the join handle and the actual bound port.
pub async fn serve(state: AppState, preferred_port: u16) -> std::io::Result<(JoinHandle<()>, u16)> {
    let mut bound: Option<(TcpListener, u16)> = None;
    for i in 0..MAX_PORT_ATTEMPTS {
        let candidate = preferred_port.saturating_add(i);
        match TcpListener::bind(("127.0.0.1", candidate)).await {
            Ok(listener) => {
                bound = Some((listener, candidate));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e),
        }
    }
    let (listener, actual) =
        bound.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrInUse, "no free port"))?;
    let app = build_router(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    Ok((handle, actual))
}

/// Try to bind to the specified port (only once, no auto-increment)
/// Returns the join handle and the bound port on success
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_port_skips_occupied() {
        let occupied = [6950u16, 6951];
        let is_free = |p: u16| !occupied.contains(&p);
        assert_eq!(pick_port(6950, is_free), Some(6952));
    }

    #[test]
    fn pick_port_none_when_all_busy() {
        let is_free = |_| false;
        assert_eq!(pick_port(6950, is_free), None);
    }

    // Note: `serve_once` binds exactly the requested port (no auto-increment) and returns
    // `AddrInUse` on conflict - that contract is covered end-to-end by
    // `tests/e2e_port_auto_recovery.rs::occupied_port_auto_recovery`.
}
