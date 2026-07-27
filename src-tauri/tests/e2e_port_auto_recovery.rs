//! Integration test: port occupation -> auto-recovery.
//!
//! Verifies the full backend flow end-to-end: when the configured port is
//! occupied, `serve_once` fails, the app records a bind error and starts a
//! polling task, and once the port is released the polling task automatically
//! binds the port, starts the service, clears the error, and stops polling.
//!
//! The polling loop sleeps 10s before each attempt, so this test takes ~10s.
//!
//! Uses an OS-assigned ephemeral port (bind to :0) instead of the default 6950
//! so the test is isolated from any concurrently running SwitchLM instance and
//! doesn't flake when the default port is already in use.

use std::sync::Arc;
use std::time::Duration;

use switchlm_lib::config::{MemoryStore, SecretStore};
use switchlm_lib::proxy::server::serve_once;
use switchlm_lib::proxy::{AppState, AppStateInner};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::time::sleep;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occupied_port_auto_recovery() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state: AppState = Arc::new(AppStateInner::load(dir.path(), secrets).unwrap());

    // 1. Occupy an ephemeral port (binds :0, then reads back the chosen port).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // 2. Try to start the service (should fail -> set bind error + start polling).
    match serve_once(state.clone(), port).await {
        Ok(_) => panic!("serve_once should have failed on an occupied port"),
        Err(_) => {
            let error_msg = format!(
                "端口 {} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。",
                port
            );
            state.set_bind_error(error_msg);
            state.clone().start_polling(port).await;
        }
    }

    // 3. Verify error state is recorded.
    let error = state.get_bind_error();
    assert!(error.is_some(), "bind error should be set after a failed bind");
    assert!(
        error.unwrap().contains(&port.to_string()),
        "bind error message should mention the occupied port"
    );

    // 4. Verify polling is running.
    {
        let handle = state.polling_handle.lock().unwrap();
        assert!(handle.is_some(), "a polling task should be running");
    }

    // 5. Release the port so the next polling attempt can succeed.
    drop(listener);

    // 6. Wait for polling to succeed (up to 11 seconds: 10s interval + margin).
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(11) {
            panic!("auto-recovery did not succeed within 11 seconds");
        }
        if state.bound_port().is_some() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }

    // 7. Verify recovery succeeded: error cleared, polling stopped, service running.
    assert_eq!(
        state.get_bind_error(),
        None,
        "bind error should be cleared after recovery"
    );
    {
        let handle = state.polling_handle.lock().unwrap();
        assert!(handle.is_none(), "polling task should be stopped after recovery");
    }
    assert_eq!(
        state.bound_port(),
        Some(port),
        "service should be running on the occupied port after recovery"
    );

    // Cleanup: abort the server task spawned by the polling loop so the port
    // is freed before the process exits (keeps parallel tests from flaking).
    if let Some(handle) = state.take_server_handle() {
        handle.abort();
        let _ = handle.await;
    }
}

// Second scenario: changing the configured port while polling is running must
// stop the old polling task. Simulates the `set_port` flow, which calls
// `stop_polling()` before rebinding on the new port so the stale poller (still
// targeting the old port) doesn't race the new bind or resurrect a server on
// the wrong port.
//
// Like the test above, this uses an OS-assigned ephemeral port (bind to :0)
// rather than the default 6950 so it is isolated from any concurrently running
// SwitchLM instance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_port_stops_old_polling() {
    let dir = tempdir().unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state: AppState = Arc::new(AppStateInner::load(dir.path(), secrets).unwrap());

    // 1. Occupy an ephemeral port (binds :0, then reads back the chosen port).
    //    The listener is held for the whole test so that, defensively, if the
    //    polling loop ever did fire it would fail to bind instead of starting a
    //    rogue server. (The loop sleeps 10s before its first attempt and we
    //    stop polling well before that, so this never happens in practice.)
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // 2. Set a bind error and start polling (as the startup flow does when the
    //    configured port is occupied).
    state.set_bind_error(format!(
        "端口 {} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。",
        port
    ));
    state.clone().start_polling(port).await;

    // 3. Verify the polling task is running.
    {
        let handle = state.polling_handle.lock().unwrap();
        assert!(handle.is_some(), "a polling task should be running");
    }

    // 4. Stop polling (simulating a port change, which stops the old polling
    //    task before starting a new one on the new port).
    state.stop_polling().await;

    // 5. Verify the old polling task is cleared. `stop_polling` takes the handle
    //    out of the mutex and aborts the task, so it is `None` once the call
    //    returns; the brief sleep is defensive.
    sleep(Duration::from_millis(100)).await;
    {
        let handle = state.polling_handle.lock().unwrap();
        assert!(
            handle.is_none(),
            "polling task should be stopped after a port change"
        );
    }
}
