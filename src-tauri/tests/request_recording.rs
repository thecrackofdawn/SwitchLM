use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use switchlm_lib::config::*;
use switchlm_lib::proxy::server::build_router;
use switchlm_lib::proxy::{AppState, AppStateInner, SystemClock};
use switchlm_lib::recording::RequestRecorder;

/// State whose recorder writes into `recdir`. Returns the recorder handle so the test can
/// `drop(rec)` to close the channel (→ writer task exits) before reading the file.
async fn state_with_recording(upstream: &str, recdir: &std::path::Path) -> (AppState, Arc<RequestRecorder>) {
    let mut cfg = AppConfig::default();
    cfg.providers.push(Provider {
        id: "zhipu".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
        openai_base_url: Some(upstream.into()), anthropic_base_url: None, usage_creds: None,
    });
    cfg.models.push(Model {
        id: "m".into(), provider_id: "zhipu".into(), source: ModelSource::Manual,
        upstream_model_id: "glm-4.6".into(), ..Default::default()
    });
    cfg.profiles.push(Profile {
        id: "p".into(), name: "glm-4.6".into(), aliases: vec![],
        backing_model_id: "m".into(), ..Default::default()
    });
    let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
    secrets.set_key("zhipu", "sk-test").unwrap();
    let (rec, rx) = RequestRecorder::channel();
    let dir = recdir.to_path_buf();
    tokio::spawn(switchlm_lib::recording::run_writer(rx, dir));
    let state = Arc::new(AppStateInner {
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
        recorder: std::sync::RwLock::new(Some(rec.clone())),
        statistics: None,
    });
    (state, rec)
}

fn oai_post() -> Request<Body> {
    Request::builder().method("POST").uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({
            "model":"glm-4.6","messages":[{"role":"user","content":"hi"}]
        }).to_string())).unwrap()
}

#[tokio::test]
async fn dispatch_records_a_line_when_recording_on() {
    let mock = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id":"x","choices":[{"message":{"role":"assistant","content":"ok"}}]
        })))
        .mount(&mock).await;

    let dir = tempfile::tempdir().unwrap();
    let (state, rec) = state_with_recording(&mock.uri(), dir.path()).await;
    let app = build_router(state);
    let resp = app.oneshot(oai_post()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = resp.into_body().collect().await;

    // drain the writer, then close the channel so the file is flushed
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    drop(rec);

    let log = std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(v["outcome"], "ok");
    assert_eq!(v["status"], 200);
    assert_eq!(v["protocol"], "openai");
    assert!(v["hash_full"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(v["body"]["messages"][0]["content"], "hi"); // full inbound body stored
}

#[tokio::test]
async fn dispatch_outcome_error_on_passthrough_non_2xx() {
    let mock = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"no"})))
        .mount(&mock).await;
    let dir = tempfile::tempdir().unwrap();
    let (state, rec) = state_with_recording(&mock.uri(), dir.path()).await;
    let app = build_router(state);
    let resp = app.oneshot(oai_post()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let _ = resp.into_body().collect().await;
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    drop(rec);
    let v: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap().trim()).unwrap();
    assert_eq!(v["outcome"], "error");
    assert_eq!(v["status"], 401);
}

#[tokio::test]
async fn dispatch_records_stream_flag_for_streaming_request() {
    let mock = MockServer::start().await;
    let sse_ok = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                  data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n\
                  data: [DONE]\n\n";
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_bytes(sse_ok.as_bytes().to_vec()))
        .mount(&mock).await;

    let dir = tempfile::tempdir().unwrap();
    let (state, rec) = state_with_recording(&mock.uri(), dir.path()).await;
    let app = build_router(state);
    // Streaming OpenAI request:
    let req = Request::builder().method("POST").uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({
            "model":"glm-4.6","stream":true,"messages":[{"role":"user","content":"hi"}]
        }).to_string())).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = resp.into_body().collect().await;

    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    drop(rec);

    let log = std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(v["outcome"], "ok");
    assert_eq!(v["stream"], true);   // <- the point of this test
    assert_eq!(v["protocol"], "openai");
}
