use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::Response;

use crate::proxy::dispatch::{dispatch, ClientProtocol};
use crate::proxy::{AppState, ProxyError};

/// Anthropic edge (`POST /v1/messages`): thin wrapper over the shared `dispatch`.
pub async fn messages(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    dispatch(state, body, ClientProtocol::Anthropic).await
}

#[cfg(test)]
mod tests {
    use crate::config::*;
    use crate::proxy::server::build_router;
    use crate::proxy::{AppState, AppStateInner, HealthRegistry, SystemClock};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn state_with_openai_backend(upstream_base: &str) -> AppState {
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "zhipu".into(),
            vendor: "zhipu".into(),
            display_name: "智谱".into(),
            openai_base_url: Some(upstream_base.into()),
            anthropic_base_url: None,
            usage_creds: None,
        });
        cfg.models.push(Model {
            id: "m_glm46".into(),
            provider_id: "zhipu".into(),
            source: ModelSource::Manual,
            upstream_model_id: "glm-4.6".into(),
            cooldown_seconds: None,
            fallback_target_model_id: None,
            ..Default::default()
        });
        cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_glm46".into(), ..Default::default() });
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        secrets.set_key("zhipu", "sk-test").unwrap();
        Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: Default::default(),
            secrets,
            health: HealthRegistry::default(),
            clock: Arc::new(SystemClock),
            usage_cache: crate::usage::UsageCache::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
            last_served_provider: std::sync::Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn anthropic_edge_translates_non_stream() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"c1","choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}),
            ))
            .mount(&mock).await;

        let app = build_router(state_with_openai_backend(&mock.uri()).await);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({
                    "model":"glm-5.2",
                    "max_tokens":100,
                    "messages":[{"role":"user","content":"hi"}]
                }).to_string())).unwrap()
        ).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["text"], "hi");
        assert_eq!(v["model"], "glm-5.2"); // echoed
        // forwarded body was OpenAI-shaped
        let received = &mock.received_requests().await.unwrap()[0];
        let fwd: serde_json::Value = serde_json::from_slice(&received.body).unwrap();
        assert_eq!(fwd["model"], "glm-4.6");
        assert_eq!(fwd["messages"][0]["content"], "hi");
    }

    #[tokio::test]
    async fn anthropic_edge_translates_stream() {
        let mock = wiremock::MockServer::start().await;
        let sse = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_bytes(sse.as_bytes()))
            .mount(&mock).await;

        let app = build_router(state_with_openai_backend(&mock.uri()).await);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"model":"glm-5.2","stream":true,"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let out = String::from_utf8_lossy(&bytes);
        assert!(out.contains("\"type\":\"message_start\""));
        assert!(out.contains("\"text\":\"Hel\""));
        assert!(out.contains("\"text\":\"lo\""));
        assert!(out.contains("\"type\":\"message_stop\""));
        assert!(out.contains("\"model\":\"glm-5.2\""));
    }
}
