use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::Response;

use crate::proxy::dispatch::{dispatch, ClientProtocol};
use crate::proxy::{AppState, ProxyError};

/// OpenAI edge (`POST /v1/chat/completions`): thin wrapper over the shared `dispatch`.
pub async fn chat_completions(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    dispatch(state, body, ClientProtocol::OpenAI).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::proxy::server::build_router;
    use crate::proxy::AppStateInner;
    use crate::proxy::{HealthRegistry, SystemClock};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state(upstream_base: &str) -> AppState {
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
        cfg.profiles.push(Profile {
            id: "p".into(),
            name: "glm-5.2".into(),
            aliases: vec![],
            backing_model_id: "m_glm46".into(),
            ..Default::default()
        });
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        secrets.set_key("zhipu", "sk-test").unwrap();
        let state = AppStateInner {
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
            recorder: std::sync::RwLock::new(None),
            statistics: None,
        };
        Arc::new(state)
    }

    #[tokio::test]
    async fn rewrites_model_forwards_and_passes_body() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"hi"}}]}),
            ))
            .mount(&mock)
            .await;

        let app = build_router(test_state(&mock.uri()).await);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"model":"glm-5.2","messages":[{"role":"user","content":"hi"}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("hi"));

        // Forwarded request had model rewritten to upstream id and an auth header.
        let received = &mock.received_requests().await.unwrap()[0];
        let raw_body = String::from_utf8_lossy(&received.body);
        let v: serde_json::Value = serde_json::from_str(&raw_body).unwrap();
        assert_eq!(v["model"], "glm-4.6");
        let auth = received
            .headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        assert_eq!(auth.to_lowercase(), "bearer sk-test");
    }

    #[tokio::test]
    async fn openai_edge_echoes_requested_model_nonstream() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","model":"glm-4.6","choices":[{"message":{"role":"assistant","content":"hi"}}]}),
            ))
            .mount(&mock)
            .await;

        let app = build_router(test_state(&mock.uri()).await);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"model":"glm-5.2","messages":[{"role":"user","content":"hi"}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["model"], "glm-5.2"); // echoed, not glm-4.6
    }
}
