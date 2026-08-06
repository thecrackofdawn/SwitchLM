//! E2E smoke against the real 智谱 (BigModel) endpoint. Ignored by default (real network + cost).
//!
//! Run (key read from a gitignored local file `src-tauri/.zhipu_key`, or the `ZHIPU_API_KEY`
//! env var - never passed on the command line):
//!   cargo test --manifest-path src-tauri/Cargo.toml --test e2e_zhipu -- --ignored --nocapture
//!
//! Optional: `ZHIPU_MODEL` (default `glm-4-flash`), `ZHIPU_BASE` (default paas/v4).
//! The key is never hardcoded or committed.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use switchlm_lib::config::{
    AppConfig, BackendKind, MemoryStore, Model, ModelSource, Profile, Provider,
    SecretStore, SecretStoreHandle,
};
use switchlm_lib::proxy::server::build_router;
use switchlm_lib::proxy::{AppState, AppStateInner, SystemClock};
use switchlm_lib::usage::usage_provider_for;
use tower::ServiceExt;

const DEFAULT_BASE: &str = "https://open.bigmodel.cn/api/paas/v4";
const DEFAULT_MODEL: &str = "glm-4-flash";

fn api_key() -> String {
    if let Ok(k) = std::env::var("ZHIPU_API_KEY") {
        return k;
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".zhipu_key");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("set ZHIPU_API_KEY or create {}: {e}", path.display()))
        .trim()
        .to_string()
}

fn base_url() -> String {
    std::env::var("ZHIPU_BASE").unwrap_or_else(|_| DEFAULT_BASE.into())
}

fn model_id() -> String {
    std::env::var("ZHIPU_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into())
}

async fn state() -> AppState {
    let mut cfg = AppConfig::default();
    cfg.providers.push(Provider {
        id: "zhipu".into(),
        vendor: "zhipu".into(),
        display_name: "智谱".into(),
        openai_base_url: Some(base_url()),
        anthropic_base_url: None,
        usage_creds: None,
    });
    cfg.models.push(Model {
        id: "m".into(),
        provider_id: "zhipu".into(),
        source: ModelSource::Manual,
        upstream_model_id: model_id(),
        cooldown_seconds: Some(60),
        fallback_target_model_id: None,
        ..Default::default()
    });
    cfg.profiles.push(Profile {
        id: "p".into(),
        name: "smoke".into(),
        aliases: vec![],
        backing_model_id: "m".into(),
        ..Default::default()
    });
    let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
    secrets.set_key("zhipu", &api_key()).unwrap();
    Arc::new(AppStateInner {
        config: tokio::sync::RwLock::new(cfg),
        catalog: Default::default(),
        secrets,
        health: Default::default(),
        clock: Arc::new(SystemClock),
        usage_cache: Default::default(),
        bound_port: std::sync::Mutex::new(None),
        server_handle: std::sync::Mutex::new(None),
        bind_error: std::sync::Mutex::new(None),
        polling_handle: std::sync::Mutex::new(None),
        last_served_provider: std::sync::Mutex::new(None),
    })
}

async fn body_str(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Anthropic client -> OpenAI backend (translate), non-stream. Validates the full
/// Anthropic→OpenAI→provider→Anthropic round-trip returns a well-formed message.
#[tokio::test]
#[ignore]
async fn e2e_anthropic_nonstream_translate() {
    let app = build_router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "model": "smoke",
                        "max_tokens": 50,
                        "messages": [{"role": "user", "content": "Reply with exactly: hi"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "{}", body_str(resp).await);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
    println!("non-stream translate response:\n{v}");
    assert_eq!(v["type"], "message");
    assert_eq!(v["role"], "assistant");
    assert_eq!(v["model"], "smoke"); // echoed requested name
    let text = v["content"][0]["text"].as_str().unwrap_or("");
    assert!(!text.is_empty(), "expected non-empty text content");
}

/// Anthropic client -> OpenAI backend (translate), stream. Validates the streaming state
/// machine produces a well-formed Anthropic SSE event sequence (Claude Code's hard path).
#[tokio::test]
#[ignore]
async fn e2e_anthropic_stream_translate() {
    let app = build_router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "model": "smoke",
                        "stream": true,
                        "max_tokens": 50,
                        "messages": [{"role": "user", "content": "Count: 1, 2, 3."}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let out = body_str(resp).await;
    println!("stream translate response:\n{out}");
    assert!(out.contains("\"type\":\"message_start\""), "missing message_start");
    assert!(
        out.contains("\"type\":\"content_block_delta\"") || out.contains("\"text_delta\""),
        "missing content deltas"
    );
    assert!(out.contains("\"type\":\"message_stop\""), "missing message_stop");
    assert!(out.contains("\"model\":\"smoke\""), "missing echoed model");
}

/// OpenAI client -> OpenAI backend (passthrough), non-stream. Validates the OpenAI edge +
/// response model echo against the real provider.
#[tokio::test]
#[ignore]
async fn e2e_openai_passthrough() {
    let app = build_router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "model": "smoke",
                        "max_tokens": 50,
                        "messages": [{"role": "user", "content": "Reply with exactly: hi"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "{}", body_str(resp).await);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
    println!("openai passthrough response:\n{v}");
    assert_eq!(v["model"], "smoke"); // echoed requested name
    assert!(!v["choices"][0]["message"]["content"].as_str().unwrap_or("").is_empty());
}

/// 智谱 usage query against the real quota endpoint (TOKENS_LIMIT percentage + reset_at).
#[tokio::test]
#[ignore]
async fn e2e_zhipu_usage() {
    let provider = usage_provider_for("zhipu").expect("zhipu adapter present");
    let snap = provider
        .query(Some(&api_key()), None, &base_url())
        .await
        .expect("usage query succeeded");
    println!("usage snapshot:\n{snap:?}");
    // Quota values vary; just assert it parsed (no error). unit field is set on the TOKENS_LIMIT path.
    assert_eq!(snap.unit, "%");
}
