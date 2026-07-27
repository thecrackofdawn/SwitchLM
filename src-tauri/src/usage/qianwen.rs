//! 千问 (Qianwen) Token Plan usage adapter. Queries the qianwenai console gateway with the
//! captured login-state cookie (see `qianwen_login`): GET secToken, POST the usage gateway, then
//! (best-effort) POST the subscription gateway for plan type + validity.
//! See spec `docs/superpowers/specs/2026-08-02-bailian-usage-adapter-design.md`.

use crate::config::UsageCreds;
use crate::usage::tiers::millis_to_iso8601;
use crate::usage::{PlanInfo, UsageError, UsageProvider, UsageSnapshot, UsageTier, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT};
use async_trait::async_trait;
use reqwest::header::COOKIE;

const SEC_TOKEN_URL: &str = "https://platform-home.qianwenai.com/tool/user/info.json";
const USAGE_URL: &str = "https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";
const PARAMS_JSON: &str = r#"{"Api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage","Data":{"cornerstoneParam":{"domain":"platform.qianwenai.com","consoleSite":"QIANWENAI","console":"ONE_CONSOLE","xsp_lang":"zh-CN","protocol":"V2","productCode":"p_efm"}},"V":"1.0"}"#;
const SUBSCRIPTION_URL: &str = "https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription";
const SUBSCRIPTION_PARAMS_JSON: &str = r#"{"Api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription","Data":{"commodityCode":"sfm_tokenplansolo_public_cn","cornerstoneParam":{"domain":"platform.qianwenai.com","consoleSite":"QIANWENAI","console":"ONE_CONSOLE","xsp_lang":"zh-CN","protocol":"V2","productCode":"p_efm"}},"V":"1.0"}"#;

/// 千问 Token Plan usage adapter (cookie + secToken auth).
pub struct QianwenUsageProvider;

/// Extract `data.secToken` from the user-info response JSON.
fn extract_sec_token(json: &serde_json::Value) -> Option<&str> {
    json.get("data")?.get("secToken")?.as_str()
}

/// Build the `application/x-www-form-urlencoded` form fields for the usage POST.
fn usage_form<'a>(sec_token: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("product", "sfm_bailian"),
        ("action", "BroadScopeAspnGateway"),
        ("sec_token", sec_token),
        ("region", "cn-beijing"),
        ("params", PARAMS_JSON),
    ]
}

/// Build the form fields for the subscription POST (same gateway/auth, different `api` +
/// a `commodityCode` in params).
fn subscription_form<'a>(sec_token: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("product", "sfm_bailian"),
        ("action", "BroadScopeAspnGateway"),
        ("sec_token", sec_token),
        ("region", "cn-beijing"),
        ("params", SUBSCRIPTION_PARAMS_JSON),
    ]
}

/// Uppercase the first char, keep the rest ("lite" → "Lite"; empty → empty).
fn titlecase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Map the parsed usage payload to a `UsageSnapshot`.
/// Both percentage fields are **USED** fractions in 0–1 (NOT 0–100): multiply by 100 → used %.
/// `per5HourPercentage` is the five-hour used fraction; `per1WeekPercentage` is the weekly **used**
/// fraction (the API doc's "per1WeekPercentage is remaining" claim and its worked example are BOTH
/// wrong per real responses). `per5HourResetTime` / `per1WeekResetTime` are **ms**.
fn map_usage(payload: &serde_json::Value) -> Result<UsageSnapshot, UsageError> {
    let inner = payload
        .get("data").and_then(|v| v.get("DataV2")).and_then(|v| v.get("data")).and_then(|v| v.get("data"))
        .ok_or_else(|| UsageError::Parse("missing data.DataV2.data.data".into()))?;
    // Both percentage fields are USED fractions in 0–1 (NOT 0–100, and per1WeekPercentage is USED,
    // not remaining — the API doc's "remaining" claim and its worked example are both wrong per real
    // responses). Multiply by 100 → used %.
    let per5_used = inner.get("per5HourPercentage").and_then(|v| v.as_f64());
    let per1w_used = inner.get("per1WeekPercentage").and_then(|v| v.as_f64());
    let per5_reset = inner.get("per5HourResetTime").and_then(|v| v.as_i64()).map(|ms| ms / 1000);
    let per1w_reset = inner.get("per1WeekResetTime").and_then(|v| v.as_i64()).map(|ms| ms / 1000);
    let pct = |f: f64| (f * 100.0).clamp(0.0, 100.0);
    let tiers = vec![
        UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: per5_used.map(pct), reset_at: per5_reset },
        UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: per1w_used.map(pct), reset_at: per1w_reset },
    ];
    Ok(UsageSnapshot {
        used: None,
        total: None,
        remaining: None,
        reset_at: per1w_reset,
        unit: "%".to_string(),
        // Data is fully structured into `tiers` above — `raw_summary` is the last-resort
        // fallback for responses that *can't* be structured (see `UsageSnapshot::raw_summary`
        // doc). map_usage either builds tiers or returns a Parse error, so it stays None here.
        raw_summary: None,
        plan: None, // tier (specCode) is filled by the subscription step; None until then
        tiers,
        billing_model: "plan".to_string(),
        plan_info: None,
    })
}

/// Map the subscription payload's `data.DataV2.data.data` to a plan label + `PlanInfo`.
/// `specCode` → the titlecased tier only (e.g. "Lite", no product prefix); `startTime`/`endTime` (ms) → ISO8601;
/// `autoRenewFlag` → auto_renew. Absent inner data → `(None, None)`; `plan_info` is `Some`
/// only when at least one of start/end/auto_renew is present.
fn extract_plan(payload: &serde_json::Value) -> (Option<String>, Option<PlanInfo>) {
    let inner = match payload
        .get("data").and_then(|v| v.get("DataV2")).and_then(|v| v.get("data")).and_then(|v| v.get("data"))
    {
        Some(v) => v,
        None => return (None, None),
    };
    let plan = inner
        .get("specCode").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty()) // empty specCode → no tier to show (plan stays None)
        .map(|spec| titlecase_first(spec));
    let start_time = inner.get("startTime").and_then(|v| v.as_i64()).and_then(millis_to_iso8601);
    let end_time = inner.get("endTime").and_then(|v| v.as_i64()).and_then(millis_to_iso8601);
    let auto_renew = inner.get("autoRenewFlag").and_then(|v| v.as_bool());
    let plan_info = if start_time.is_some() || end_time.is_some() || auto_renew.is_some() {
        Some(PlanInfo { start_time, end_time, auto_renew })
    } else {
        None
    };
    (plan, plan_info)
}

#[async_trait]
impl UsageProvider for QianwenUsageProvider {
    async fn query(
        &self,
        _api_key: Option<&str>,
        usage_creds: Option<&UsageCreds>,
        _base_url: &str,
    ) -> Result<UsageSnapshot, UsageError> {
        let cookie = usage_creds
            .and_then(|c| c.cookie.as_deref())
            .ok_or(UsageError::NotConfigured)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| UsageError::Network(e.to_string()))?;
        query_at(&client, SEC_TOKEN_URL, USAGE_URL, SUBSCRIPTION_URL, cookie).await
    }
}

/// Log a qianwen per-request failure (target `switchlm::usage`) and return the error unchanged so
/// the caller can `?`-propagate it. `label` identifies which of the three requests failed
/// ("secToken" / "usage" / "subscription") — including the best-effort subscription, whose error
/// is otherwise swallowed by `query_at`.
fn log_err(e: UsageError, label: &str) -> UsageError {
    tracing::warn!(target: "switchlm::usage", "千问 {} 请求失败: {}", label, e);
    e
}

/// POST a form to a qianwenai gateway URL with the login cookie and parse the JSON body.
/// Non-2xx → `AuthFailed("{label} HTTP {status}")`; body parse failure → `Parse`. Every failure is
/// logged via `log_err` so both the required usage and the best-effort subscription POSTs surface.
async fn post_gateway(
    client: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
    cookie: &str,
    label: &str,
) -> Result<serde_json::Value, UsageError> {
    let resp = client
        .post(url)
        .header(COOKIE, cookie)
        .form(form)
        .send()
        .await
        .map_err(|e| log_err(UsageError::Network(e.to_string()), label))?;
    if !resp.status().is_success() {
        return Err(log_err(
            UsageError::AuthFailed(format!("{label} HTTP {}", resp.status())),
            label,
        ));
    }
    resp.json()
        .await
        .map_err(|e| log_err(UsageError::Parse(e.to_string()), label))
}

/// Query against configurable URLs (the test seam). All requests carry the cookie: GET secToken,
/// POST the (required) usage gateway, then POST the (best-effort) subscription gateway for the
/// plan type + validity.
async fn query_at(
    client: &reqwest::Client,
    sec_token_url: &str,
    usage_url: &str,
    subscription_url: &str,
    cookie: &str,
) -> Result<UsageSnapshot, UsageError> {
    let resp = client
        .get(sec_token_url)
        .header(COOKIE, cookie)
        .send()
        .await
        .map_err(|e| log_err(UsageError::Network(e.to_string()), "secToken"))?;
    if !resp.status().is_success() {
        return Err(log_err(
            UsageError::AuthFailed(format!("secToken HTTP {}", resp.status())),
            "secToken",
        ));
    }
    let info: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| log_err(UsageError::Parse(e.to_string()), "secToken"))?;
    let sec_token = extract_sec_token(&info)
        .ok_or_else(|| log_err(UsageError::AuthFailed("secToken missing in user/info".into()), "secToken"))?;

    let mut snap =
        map_usage(&post_gateway(client, usage_url, &usage_form(sec_token), cookie, "usage").await?)?;

    // Best-effort: enrich plan type + validity from the subscription endpoint. Any failure keeps
    // the usage tiers with plan = None (no tier known) + plan_info = None. The failure is still
    // logged (inside `post_gateway`) so the silent degradation is visible — only the query result
    // is unaffected.
    if let Ok(sub) =
        post_gateway(client, subscription_url, &subscription_form(sec_token), cookie, "subscription").await
    {
        let (plan, plan_info) = extract_plan(&sub);
        // `plan` is set only when the subscription returns a specCode — it stays None (no tier
        // known) otherwise. `plan_info` is simply assigned (map_usage always leaves it None, so
        // this either sets it or clears it to the subscription's value).
        if plan.is_some() {
            snap.plan = plan;
        }
        snap.plan_info = plan_info;
    }

    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, OnceLock};
    use tracing_subscriber::fmt::MakeWriter;

    // --- test-only global capturing subscriber ---------------------------------------
    // `tracing-test` filters out events with a custom `target:`, so qianwen's
    // `target: "switchlm::usage"` warns are invisible to it. Instead install a process-wide fmt
    // subscriber (modelled on logging::FileMaker) that captures EVERY target at TRACE into a shared
    // buffer, making the per-request failure logs observable. try_init is idempotent across tests.
    static CAPTURE_BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

    fn capture_buf() -> Arc<Mutex<Vec<u8>>> {
        CAPTURE_BUF.get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
            let _ = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(BufMaker(buf.clone()))
                .try_init();
            buf
        })
        .clone()
    }

    struct BufMaker(Arc<Mutex<Vec<u8>>>);
    impl<'a> MakeWriter<'a> for BufMaker {
        type Writer = BufGuard<'a>;
        fn make_writer(&'a self) -> Self::Writer {
            BufGuard(self.0.lock().unwrap_or_else(|e| e.into_inner()))
        }
    }
    struct BufGuard<'a>(std::sync::MutexGuard<'a, Vec<u8>>);
    impl<'a> std::io::Write for BufGuard<'a> {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    fn reset_logs() { capture_buf().lock().unwrap().clear(); }
    fn logs_contain(needle: &str) -> bool {
        let buf = capture_buf();
        let guard = buf.lock().unwrap();
        std::str::from_utf8(guard.as_slice()).unwrap_or("").contains(needle)
    }

    #[test]
    fn extract_sec_token_present_and_missing() {
        let with = serde_json::json!({ "data": { "secToken": "abc" } });
        assert_eq!(extract_sec_token(&with), Some("abc"));
        let without = serde_json::json!({ "data": {} });
        assert_eq!(extract_sec_token(&without), None);
    }

    #[test]
    fn usage_form_has_required_fields_and_params() {
        let form = usage_form("tok");
        assert!(form.contains(&("sec_token", "tok")));
        assert!(form.contains(&("product", "sfm_bailian")));
        assert!(form.contains(&("action", "BroadScopeAspnGateway")));
        assert!(form.contains(&("region", "cn-beijing")));
        assert!(form.iter().any(|(k, v)| *k == "params" && v.contains("cornerstoneParam")));
    }

    #[test]
    fn map_usage_real_response() {
        // Real API response (user E2E test): both fields are USED fractions in 0–1.
        // per5HourPercentage=0.3392… → 5h used ~33.92%; per1WeekPercentage=0.0950… → weekly used ~9.50% (NOT inverted).
        let payload = serde_json::json!({
            "data": { "DataV2": { "data": { "data": {
                "per1WeekPercentage": 0.0949859864,
                "per1WeekResetTime": 1786285080000i64,
                "per5HourPercentage": 0.3392356657142857,
                "per5HourResetTime": 1785698280000i64
            } } } }
        });
        let snap = map_usage(&payload).unwrap();
        assert_eq!(snap.billing_model, "plan");
        assert_eq!(snap.unit, "%");
        assert!(snap.raw_summary.is_none()); // structured into tiers — no raw fallback
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].window, TIER_FIVE_HOUR);
        assert!((snap.tiers[0].used_pct.unwrap() - 33.92356657142857).abs() < 1e-6);
        assert_eq!(snap.tiers[0].reset_at, Some(1785698280)); // per5HourResetTime ms → secs
        assert_eq!(snap.tiers[1].window, TIER_WEEKLY_LIMIT);
        assert!((snap.tiers[1].used_pct.unwrap() - 9.49859864).abs() < 1e-6); // used, NOT 100 − value
        assert_eq!(snap.tiers[1].reset_at, Some(1786285080)); // per1WeekResetTime ms → secs
        assert_eq!(snap.reset_at, Some(1786285080));
    }

    #[test]
    fn map_usage_missing_datav2_is_parse_error() {
        let payload = serde_json::json!({ "data": {} });
        assert!(matches!(map_usage(&payload), Err(UsageError::Parse(_))));
    }

    #[test]
    fn map_usage_missing_percentages_degrade_to_none() {
        let payload = serde_json::json!({ "data": { "DataV2": { "data": { "data": {} } } } });
        let snap = map_usage(&payload).unwrap();
        assert_eq!(snap.tiers[0].used_pct, None);
        assert_eq!(snap.tiers[0].reset_at, None);
        assert_eq!(snap.tiers[1].used_pct, None);
        assert_eq!(snap.tiers[1].reset_at, None);
        assert_eq!(snap.reset_at, None);
        assert!(snap.raw_summary.is_none()); // still structured (into empty tiers), no raw fallback
    }

    use crate::usage::UsageProvider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const USAGE_API_Q: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";
    const SUBSCRIPTION_API_Q: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription";

    #[test]
    fn extract_plan_maps_spec_and_validity() {
        let payload = serde_json::json!({
            "data": { "DataV2": { "data": { "data": {
                "instanceCode": "inst", "specCode": "lite", "remainingDays": 31,
                "startTime": 1785653265000i64, "endTime": 1788364800000i64,
                "autoRenewFlag": false, "status": "VALID"
            } } } }
        });
        let (plan, plan_info) = extract_plan(&payload);
        assert_eq!(plan.as_deref(), Some("Lite")); // tier only — no product prefix
        let info = plan_info.unwrap();
        assert_eq!(info.auto_renew, Some(false));
        // Pinned ms→ISO conversion (RFC3339 UTC from `to_rfc3339`).
        assert_eq!(info.start_time.as_deref(), Some("2026-08-02T06:47:45+00:00")); // 1785653265000
        assert_eq!(info.end_time.as_deref(), Some("2026-09-02T16:00:00+00:00")); // 1788364800000
    }

    #[test]
    fn extract_plan_empty_spec_code_degrades_to_default() {
        let payload = serde_json::json!({
            "data": { "DataV2": { "data": { "data": { "specCode": "" } } } }
        });
        let (plan, plan_info) = extract_plan(&payload);
        assert_eq!(plan, None); // empty specCode → no override; default label preserved
        assert_eq!(plan_info, None);
    }

    #[test]
    fn extract_plan_missing_returns_none() {
        let payload = serde_json::json!({ "data": {} });
        assert_eq!(extract_plan(&payload), (None, None));
    }

    #[tokio::test]
    async fn query_no_cookie_is_not_configured() {
        // No cookie -> NotConfigured (does not touch the network).
        let r = QianwenUsageProvider.query(None, None, "").await;
        assert!(matches!(r, Err(UsageError::NotConfigured)));
    }

    #[tokio::test]
    async fn query_at_success_maps_tiers_plan_and_sends_cookie_to_all() {
        let server = MockServer::start().await;
        let sec_url = format!("{}/tool/user/info.json", server.uri());
        let usage_url = format!("{}/data/api.json?api={USAGE_API_Q}", server.uri());
        let sub_url = format!("{}/data/api.json?api={SUBSCRIPTION_API_Q}", server.uri());
        // All mocks require the Cookie header — `.expect(1)` proves it was sent.
        Mock::given(method("GET"))
            .and(path("/tool/user/info.json"))
            .and(header("cookie", "c=1"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "data": { "secToken": "s" } })))
            .expect(1)
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/data/api.json"))
            .and(query_param("api", USAGE_API_Q))
            .and(header("cookie", "c=1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "DataV2": { "data": { "data": {
                    "per5HourPercentage": 0.5, "per5HourResetTime": 1785698280000i64,
                    "per1WeekPercentage": 0.2, "per1WeekResetTime": 1786285080000i64
                } } } }
            })))
            .expect(1)
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/data/api.json"))
            .and(query_param("api", SUBSCRIPTION_API_Q))
            .and(header("cookie", "c=1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "DataV2": { "data": { "data": {
                    "specCode": "lite", "startTime": 1785653265000i64,
                    "endTime": 1788364800000i64, "autoRenewFlag": false
                } } } }
            })))
            .expect(1)
            .mount(&server).await;
        let snap = query_at(&reqwest::Client::new(), &sec_url, &usage_url, &sub_url, "c=1").await.unwrap();
        assert_eq!(snap.tiers[0].used_pct, Some(50.0)); // five_hour: 0.5 fraction × 100
        assert_eq!(snap.tiers[0].reset_at, Some(1785698280));
        assert_eq!(snap.tiers[1].used_pct, Some(20.0)); // weekly: 0.2 used fraction × 100 (NOT inverted)
        assert_eq!(snap.tiers[1].reset_at, Some(1786285080));
        // Subscription enrichment: plan label + validity.
        assert_eq!(snap.plan.as_deref(), Some("Lite")); // tier only
        let info = snap.plan_info.as_ref().unwrap();
        assert_eq!(info.auto_renew, Some(false));
        assert!(info.start_time.is_some());
        assert!(info.end_time.is_some());
    }

    #[tokio::test]
    async fn query_at_subscription_failure_still_returns_usage() {
        // The subscription step is best-effort: a 500 there must NOT fail the whole query —
        // the usage tiers survive with plan = None (no tier known) and plan_info = None.
        let server = MockServer::start().await;
        let sec_url = format!("{}/tool/user/info.json", server.uri());
        let usage_url = format!("{}/data/api.json?api={USAGE_API_Q}", server.uri());
        let sub_url = format!("{}/data/api.json?api={SUBSCRIPTION_API_Q}", server.uri());
        Mock::given(method("GET"))
            .and(path("/tool/user/info.json"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "data": { "secToken": "s" } })))
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/data/api.json"))
            .and(query_param("api", USAGE_API_Q))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "DataV2": { "data": { "data": {
                    "per5HourPercentage": 0.5, "per5HourResetTime": 1785698280000i64,
                    "per1WeekPercentage": 0.2, "per1WeekResetTime": 1786285080000i64
                } } } }
            })))
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/data/api.json"))
            .and(query_param("api", SUBSCRIPTION_API_Q))
            .respond_with(ResponseTemplate::new(500)) // subscription outage
            .mount(&server).await;
        let snap = query_at(&reqwest::Client::new(), &sec_url, &usage_url, &sub_url, "c=1").await.unwrap();
        assert_eq!(snap.tiers[0].used_pct, Some(50.0)); // five_hour intact
        assert_eq!(snap.tiers[1].used_pct, Some(20.0)); // weekly intact
        assert_eq!(snap.plan, None); // subscription failed → no tier known, plan stays None
        assert_eq!(snap.plan_info, None);
    }

    #[tokio::test]
    async fn query_at_sec_token_missing_is_auth_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/tool/user/info.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": {} })))
            .mount(&server).await;
        let r = query_at(
            &reqwest::Client::new(),
            &format!("{}/tool/user/info.json", server.uri()),
            "http://unused.invalid/data/api.json", // never reached: secToken step fails first
            "http://unused.invalid/sub",
            "c=1",
        ).await;
        assert!(matches!(r, Err(UsageError::AuthFailed(_))));
    }

    #[tokio::test]
    async fn query_at_usage_non_2xx_is_auth_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/tool/user/info.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "secToken": "s" } })))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/data/api.json"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server).await;
        let r = query_at(
            &reqwest::Client::new(),
            &format!("{}/tool/user/info.json", server.uri()),
            &format!("{}/data/api.json?api={USAGE_API_Q}", server.uri()),
            "http://unused.invalid/sub", // never reached: usage step fails first
            "c=1",
        ).await;
        assert!(matches!(r, Err(UsageError::AuthFailed(_))));
    }

    /// Every request in the qianwen flow logs its failure — incl. the best-effort subscription,
    /// whose error used to be silently swallowed. Scenarios run sequentially in one test so the
    /// shared capture buffer has no parallel races.
    #[tokio::test]
    async fn query_at_logs_every_request_failure() {
        let client = reqwest::Client::new();

        // 1) secToken GET → non-2xx.
        reset_logs();
        {
            let server = MockServer::start().await;
            Mock::given(method("GET")).and(path("/tool/user/info.json"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server).await;
            let r = query_at(
                &client,
                &format!("{}/tool/user/info.json", server.uri()),
                "http://unused.invalid/data/api.json",
                "http://unused.invalid/sub",
                "c=1",
            ).await;
            assert!(matches!(r, Err(UsageError::AuthFailed(_))));
            assert!(logs_contain("secToken HTTP 401"), "secToken failure must be logged");
        }

        // 2) usage POST → non-2xx.
        reset_logs();
        {
            let server = MockServer::start().await;
            Mock::given(method("GET")).and(path("/tool/user/info.json"))
                .respond_with(ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "data": { "secToken": "s" } })))
                .mount(&server).await;
            Mock::given(method("POST")).and(path("/data/api.json"))
                .and(query_param("api", USAGE_API_Q))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server).await;
            let r = query_at(
                &client,
                &format!("{}/tool/user/info.json", server.uri()),
                &format!("{}/data/api.json?api={USAGE_API_Q}", server.uri()),
                &format!("{}/data/api.json?api={SUBSCRIPTION_API_Q}", server.uri()),
                "c=1",
            ).await;
            assert!(matches!(r, Err(UsageError::AuthFailed(_))));
            assert!(logs_contain("usage HTTP 500"), "usage failure must be logged");
        }

        // 3) subscription POST → non-2xx (best-effort: query still succeeds, but it IS logged).
        reset_logs();
        {
            let server = MockServer::start().await;
            let sec_url = format!("{}/tool/user/info.json", server.uri());
            let usage_url = format!("{}/data/api.json?api={USAGE_API_Q}", server.uri());
            let sub_url = format!("{}/data/api.json?api={SUBSCRIPTION_API_Q}", server.uri());
            Mock::given(method("GET")).and(path("/tool/user/info.json"))
                .respond_with(ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "data": { "secToken": "s" } })))
                .mount(&server).await;
            Mock::given(method("POST")).and(path("/data/api.json"))
                .and(query_param("api", USAGE_API_Q))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": { "DataV2": { "data": { "data": {
                        "per5HourPercentage": 0.5, "per5HourResetTime": 1785698280000i64,
                        "per1WeekPercentage": 0.2, "per1WeekResetTime": 1786285080000i64
                    } } } }
                })))
                .mount(&server).await;
            Mock::given(method("POST")).and(path("/data/api.json"))
                .and(query_param("api", SUBSCRIPTION_API_Q))
                .respond_with(ResponseTemplate::new(500)) // subscription outage
                .mount(&server).await;
            let snap = query_at(&client, &sec_url, &usage_url, &sub_url, "c=1").await;
            assert!(snap.is_ok(), "subscription failure is best-effort; query must still succeed");
            assert!(logs_contain("subscription HTTP 500"), "best-effort subscription failure must be logged");
        }
    }
}
