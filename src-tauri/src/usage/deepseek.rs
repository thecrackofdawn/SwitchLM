//! DeepSeek 余额查询适配器
//!
//! Queries `GET https://api.deepseek.com/user/balance` with Bearer auth to retrieve
//! account balance in CNY. Maps to a `UsageSnapshot` with monetary unit so the tray
//! shows a consumption percentage and the Usage page displays the raw balance.

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::Value;

use super::{UsageError, UsageProvider, UsageSnapshot};

/// DeepSeek balance adapter. Uses the inference `api_key` (Bearer) — no separate usage creds.
///
/// Endpoint: `{host}/user/balance` where `host` is derived from the provider `base_url`
/// (e.g. `https://api.deepseek.com` stays as-is).
pub struct DeepseekUsageProvider;

#[async_trait]
impl UsageProvider for DeepseekUsageProvider {
    async fn query(
        &self,
        api_key: Option<&str>,
        _usage_creds: Option<&crate::config::UsageCreds>,
        base_url: &str,
    ) -> Result<UsageSnapshot, UsageError> {
        let key = api_key.ok_or(UsageError::NotConfigured)?;
        let url = balance_url(base_url);

        let resp = reqwest::Client::new()
            .get(&url)
            .bearer_auth(key)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| UsageError::Network(e.to_string()))?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UsageError::AuthFailed(format!("HTTP {status}")));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(UsageError::Network(format!(
                "HTTP {status}: {}",
                body.trim()
            )));
        }

        let v: Value = resp
            .json()
            .await
            .map_err(|e| UsageError::Parse(format!("invalid JSON: {e}")))?;

        // is_available must be true; otherwise the key is valid but the account is unusable.
        if v.get("is_available").and_then(|v| v.as_bool()) != Some(true) {
            return Err(UsageError::Parse(
                "account unavailable (is_available != true)".into(),
            ));
        }

        let balances = v
            .get("balance_infos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                UsageError::Parse("response missing balance_infos array".into())
            })?;

        if balances.is_empty() {
            return Err(UsageError::Parse("balance_infos array is empty".into()));
        }

        // Use the first balance entry (there is typically a single CNY entry).
        let entry = &balances[0];
        let currency = entry
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("CNY");

        let topped_up: f64 = parse_balance_field(entry, "topped_up_balance")?;
        let total_balance: f64 = parse_balance_field(entry, "total_balance")?;

        // topped_up_balance = total recharged; total_balance = remaining → used = topped_up - remaining.
        let used = (topped_up - total_balance).max(0.0);
        let remaining = total_balance;

        Ok(UsageSnapshot {
            used: Some(used),
            total: Some(topped_up),
            remaining: Some(remaining),
            reset_at: None,
            unit: currency.to_string(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "consumption".into(),
            plan_info: None,
        })
    }
}

/// Parse a numeric balance field from a JSON value. DeepSeek returns balances as strings
/// (e.g. `"48.77"`) — parse them as f64.
fn parse_balance_field(entry: &Value, field: &str) -> Result<f64, UsageError> {
    entry
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| UsageError::Parse(format!("missing or non-string field '{field}'")))?
        .parse::<f64>()
        .map_err(|_| UsageError::Parse(format!("'{field}' is not a valid number")))
}

/// Derive the balance endpoint from a provider base URL: keep scheme://host[:port],
/// append `/user/balance`.
fn balance_url(base_url: &str) -> String {
    let (scheme, rest) = base_url
        .split_once("://")
        .unwrap_or(("https", base_url));
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{scheme}://{host}/user/balance")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn balance_url_derives_from_base_url() {
        assert_eq!(
            balance_url("https://api.deepseek.com"),
            "https://api.deepseek.com/user/balance"
        );
        assert_eq!(
            balance_url("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/user/balance"
        );
    }

    fn normal_response() -> serde_json::Value {
        serde_json::json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "CNY",
                "total_balance": "48.77",
                "granted_balance": "0.00",
                "topped_up_balance": "100.00"
            }]
        })
    }

    #[tokio::test]
    async fn parses_normal_balance() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(normal_response()))
            .mount(&mock)
            .await;

        let snap = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap();

        assert_eq!(snap.used, Some(51.23)); // 100.00 - 48.77
        assert_eq!(snap.total, Some(100.00));
        assert_eq!(snap.remaining, Some(48.77));
        assert_eq!(snap.unit, "CNY");
        assert_eq!(snap.reset_at, None);
        assert!(snap.raw_summary.is_none());
    }

    #[tokio::test]
    async fn unavailable_account_is_parse_error() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"is_available": false, "balance_infos": []}),
            ))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::Parse(_)));
    }

    #[tokio::test]
    async fn missing_balance_infos_is_parse_error() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"is_available": true}),
            ))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::Parse(_)));
    }

    #[tokio::test]
    async fn empty_balance_infos_is_parse_error() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"is_available": true, "balance_infos": []}),
            ))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::Parse(_)));
    }

    #[tokio::test]
    async fn missing_key_is_not_configured() {
        let err = DeepseekUsageProvider
            .query(None, None, "https://api.deepseek.com")
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::NotConfigured));
    }

    #[tokio::test]
    async fn auth_failed_on_401() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-bad"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::AuthFailed(_)));
    }

    #[tokio::test]
    async fn auth_failed_on_403() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-bad"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::AuthFailed(_)));
    }

    #[tokio::test]
    async fn non_success_status_is_network_error() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::Network(_)));
    }

    #[tokio::test]
    async fn malformed_json_is_parse_error() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&mock)
            .await;

        let err = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::Parse(_)));
    }

    #[tokio::test]
    async fn zero_balance_still_valid() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "is_available": true,
                    "balance_infos": [{
                        "currency": "CNY",
                        "total_balance": "0.00",
                        "granted_balance": "0.00",
                        "topped_up_balance": "100.00"
                    }]
                }),
            ))
            .mount(&mock)
            .await;

        let snap = DeepseekUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap();

        assert_eq!(snap.used, Some(100.0));
        assert_eq!(snap.total, Some(100.0));
        assert_eq!(snap.remaining, Some(0.0));
        assert_eq!(snap.raw_summary, None);
    }
}
