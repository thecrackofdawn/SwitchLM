use axum::body::Body;
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use thiserror::Error;

use crate::proxy::ResolveError;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("no profiles/models configured; set up SwitchLM first")]
    NotConfigured,
    #[error("{0}")]
    Resolve(#[from] ResolveError),
    #[error("model has no usable backend ({0})")]
    NoBackend(String),
    #[error("missing api key for provider '{0}'")]
    NoApiKey(String),
    #[error("translation error: {0}")]
    Translation(String),
    #[error("upstream request failed: {0}")]
    Upstream(String),
    #[error("all models rate-limited/unavailable (tried {tried:?}); last: {last_error}")]
    FallbackExhausted { tried: Vec<String>, last_error: String },
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response<Body> {
        let (code, msg) = match &self {
            ProxyError::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            ProxyError::Resolve(ResolveError::ProfileNotFound { .. }) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            ProxyError::Resolve(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            ProxyError::NoBackend(_) | ProxyError::NoApiKey(_) => {
                (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
            }
            ProxyError::Translation(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::Upstream(_) | ProxyError::FallbackExhausted { .. } => {
                (StatusCode::BAD_GATEWAY, self.to_string())
            }
        };
        Response::builder()
            .status(code)
            .body(Body::from(msg))
            .unwrap()
    }
}
