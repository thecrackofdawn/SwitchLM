//! 智谱额度查询适配器
//!
//! 查询智谱 BigModel 套餐额度接口（`/api/monitor/usage/quota/limit`，Bearer 鉴权），
//! 把 `data.limits[]` 里的 `TOKENS_LIMIT` 条目解析成 five_hour / weekly 两档；
//! 另 best-effort 调 `/api/biz/subscription/list` 补套餐有效期 + 自动续费（plan_info）。

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::Value;

use super::{PlanInfo, UsageError, UsageProvider, UsageSnapshot, QuotaTier, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT};
use super::tiers::extract_reset_time;

/// 智谱 TOKENS_LIMIT 条目按 `unit` 字段的显式窗口分类。
enum ZhipuWindow {
    FiveHour,
    Weekly,
}

/// 按 `unit` 字段判定 TOKENS_LIMIT 条目所属窗口。
///
/// 实测形态（bigmodel.cn 与 z.ai 共用同一后端，字段一致）：
/// - `unit: 3, number: 5` → 5 小时滚动窗口（老/新套餐均有）
/// - `unit: 6, number: 7` 与 `unit: 6, number: 1` → 每周窗口（两种取值都被
///   实测过，故只锚定 `unit`、不绑 `number`）
///
/// `unit` 缺失或值不认识时返回 None，由调用方走重置时间启发式兜底。
fn classify_zhipu_window(item: &Value) -> Option<ZhipuWindow> {
    match item.get("unit").and_then(|v| v.as_i64()) {
        Some(3) => Some(ZhipuWindow::FiveHour),
        Some(6) => Some(ZhipuWindow::Weekly),
        _ => None,
    }
}

/// 把智谱 `data` 里的 `limits[]` 解析成 tier 列表。
///
/// 分类优先级：
/// 1. 显式字段：`unit` 标识窗口类型（见 [`classify_zhipu_window`]）。不能按
///    `nextResetTime` 排序代替——周期末尾每周窗口会比 5 小时窗口更早重置
///    时间排序在该场景必然把两桶标反。
/// 2. 兜底启发式（`unit` 缺失或不识别）：无 `nextResetTime` 的条目优先归
///    five_hour（5 小时桶在 0% 等状态下可能没有 reset），其余按 reset 升序
///    依次填入仍空缺的槽位。
///
/// 老套餐（2026-02-12 前订阅）只回 1 条
/// `TOKENS_LIMIT`，自然降级为仅展示 `five_hour`；新套餐回 2 条。
fn parse_zhipu_token_tiers(data: &Value) -> Vec<QuotaTier> {
    type Entry = (Option<i64>, Option<String>, f64);
    let mut five_hour: Option<Entry> = None;
    let mut weekly: Option<Entry> = None;
    let mut unclassified: Vec<Entry> = Vec::new();

    if let Some(limits) = data.get("limits").and_then(|v| v.as_array()) {
        for limit_item in limits {
            let limit_type = limit_item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // 大小写不敏感比较：上游若把 "TOKENS_LIMIT" 改成小写或驼峰，依然能识别
            if !limit_type.eq_ignore_ascii_case("TOKENS_LIMIT") {
                continue;
            }
            let percentage = limit_item
                .get("percentage")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reset_ms = limit_item.get("nextResetTime").and_then(|v| v.as_i64());
            // extract_reset_time 兼容数字（毫秒）与 ISO8601 字符串两种 nextResetTime 形态。
            let reset_iso = limit_item.get("nextResetTime").and_then(extract_reset_time);
            let entry = (reset_ms, reset_iso, percentage);
            match classify_zhipu_window(limit_item) {
                Some(ZhipuWindow::FiveHour) if five_hour.is_none() => five_hour = Some(entry),
                Some(ZhipuWindow::Weekly) if weekly.is_none() => weekly = Some(entry),
                _ => unclassified.push(entry),
            }
        }
    }

    unclassified.sort_by_key(|(reset, _, _)| (reset.is_some(), reset.unwrap_or(i64::MIN)));
    for entry in unclassified {
        if five_hour.is_none() {
            five_hour = Some(entry);
        } else if weekly.is_none() {
            weekly = Some(entry);
        }
        // 智谱当前最多两条 TOKENS_LIMIT，多余的忽略
    }

    let mut tiers = Vec::new();
    for (name, slot) in [(TIER_FIVE_HOUR, five_hour), (TIER_WEEKLY_LIMIT, weekly)] {
        if let Some((_, resets_at, percentage)) = slot {
            tiers.push(QuotaTier {
                name: name.to_string(),
                utilization: percentage,
                resets_at,
                used_value_usd: None,
                max_value_usd: None,
            });
        }
    }
    tiers
}

/// 智谱 BigModel quota adapter. Queries the Coding-plan quota endpoint using the inference
/// `api_key` (Bearer). Reference: zai_refresh/quota_refresher.py `check_quota`.
///
/// Endpoint: `{host}/api/monitor/usage/quota/limit` where `host` is derived from the
/// provider `base_url` (e.g. `https://open.bigmodel.cn/api/paas/v4` -> `https://open.bigmodel.cn`).
pub struct ZhipuUsageProvider;

/// 增强的套餐额度查询结果
#[derive(Clone, Debug, serde::Serialize)]
pub struct ZhipuQuotaResult {
    pub tiers: Vec<QuotaTier>,
    pub level: Option<String>,  // 套餐等级
    pub success: bool,
    pub error: Option<String>,
    pub credential_status: CredentialStatus,
}

#[derive(Clone, Debug, serde::Serialize)]
pub enum CredentialStatus {
    Valid,
    Expired,
    NotFound,
}

impl ZhipuUsageProvider {
    /// 查询详细的套餐额度信息（包含多个层级）
    pub async fn query_quota_detailed(
        api_key: &str,
        base_url: &str,
    ) -> ZhipuQuotaResult {
        let url = quota_url(base_url);

        let resp = match reqwest::Client::new()
            .get(&url)
            .header("Authorization", api_key) // 智谱不加 Bearer 前缀
            .header("Content-Type", "application/json")
            .header("Accept-Language", "en-US,en")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return ZhipuQuotaResult {
                    tiers: vec![],
                    level: None,
                    success: false,
                    error: Some(format!("Network error: {e}")),
                    credential_status: CredentialStatus::Valid,
                };
            }
        };

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return ZhipuQuotaResult {
                tiers: vec![],
                level: None,
                success: false,
                error: Some(format!("Authentication failed (HTTP {status})")),
                credential_status: CredentialStatus::Expired,
            };
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return ZhipuQuotaResult {
                tiers: vec![],
                level: None,
                success: false,
                error: Some(format!("API error (HTTP {status}): {body}")),
                credential_status: CredentialStatus::Valid,
            };
        }

        // 先 bytes() 再解析：读体失败（超时/连接中断）是瞬时 → Err；拿到完整响应体
        // 后解析失败才是确定性。reqwest 的 json() 把读体错误也包成 decode，无法区分。
        let raw = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return ZhipuQuotaResult {
                    tiers: vec![],
                    level: None,
                    success: false,
                    error: Some(format!("Failed to read response: {e}")),
                    credential_status: CredentialStatus::Valid,
                };
            }
        };

        let body: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(e) => {
                return ZhipuQuotaResult {
                    tiers: vec![],
                    level: None,
                    success: false,
                    error: Some(format!("Failed to parse response: {e}")),
                    credential_status: CredentialStatus::Valid,
                };
            }
        };

        // 检查业务级别错误
        if body.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let msg = body
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            return ZhipuQuotaResult {
                tiers: vec![],
                level: None,
                success: false,
                error: Some(format!("API error: {msg}")),
                credential_status: CredentialStatus::Valid,
            };
        }

        let data = match body.get("data") {
            Some(d) => d,
            None => {
                return ZhipuQuotaResult {
                    tiers: vec![],
                    level: None,
                    success: false,
                    error: Some("Missing 'data' field in response".to_string()),
                    credential_status: CredentialStatus::Valid,
                };
            }
        };

        let tiers = parse_zhipu_token_tiers(data);

        // 套餐等级
        let level = data
            .get("level")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        ZhipuQuotaResult {
            tiers,
            level,
            success: true,
            error: None,
            credential_status: CredentialStatus::Valid,
        }
    }
}

#[async_trait]
impl UsageProvider for ZhipuUsageProvider {
    async fn query(
        &self,
        api_key: Option<&str>,
        _usage_creds: Option<&crate::config::UsageCreds>,
        base_url: &str,
    ) -> Result<UsageSnapshot, UsageError> {
        let key = api_key.ok_or(UsageError::NotConfigured)?;
        let url = quota_url(base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| UsageError::Network(e.to_string()))?;

        let resp = client
            .get(&url)
            .bearer_auth(key)
            .header("content-type", "application/json")
            .send()
            .await
            .map_err(|e| UsageError::Network(e.to_string()))?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UsageError::AuthFailed(format!("HTTP {status}")));
        }

        let v: Value = resp
            .json()
            .await
            .map_err(|e| UsageError::Network(e.to_string()))?;

        let data = v
            .get("data")
            .ok_or_else(|| UsageError::Parse("response missing data field".into()))?;

        let tiers = parse_zhipu_token_tiers(data);
        if tiers.is_empty() {
            // No TOKENS_LIMIT in window: best-effort raw fallback so the UI can show the payload.
            return Ok(UsageSnapshot {
                used: None,
                total: None,
                remaining: None,
                reset_at: None,
                unit: String::new(),
                raw_summary: Some(v.to_string()),
                plan: None,
                tiers: vec![],
                billing_model: "plan".into(),
                plan_info: None,
            });
        }
        // 套餐等级（如 "max"）作为卡片头的 plan 标签。
        let level = data.get("level").and_then(|v| v.as_str()).map(String::from);
        let mut snap = super::snapshot_from_tiers(&tiers, &level);
        // Best-effort：用订阅列表接口补充套餐有效期 + 自动续费（plan_info）。任何失败都保持
        // plan_info = None，不影响用量展示——与千问/火山的 best-effort 订阅查询一致。
        enrich_plan_info(&client, base_url, key, &mut snap).await;
        Ok(snap)
    }
}

/// Derive the quota endpoint from a provider base URL: keep scheme://host[:port], append the
/// monitor path. E.g. `https://open.bigmodel.cn/api/paas/v4` -> `https://open.bigmodel.cn/api/monitor/usage/quota/limit`.
fn quota_url(base_url: &str) -> String {
    let (scheme, rest) = base_url
        .split_once("://")
        .unwrap_or(("https", base_url));
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{scheme}://{host}/api/monitor/usage/quota/limit")
}

/// Derive the subscription-list endpoint from a provider base URL. The subscription API lives on
/// the console host (`bigmodel.cn`), not the API-gateway host (`open.bigmodel.cn`) that the
/// inference `base_url` points at, so strip a leading `open.` subdomain:
/// `https://open.bigmodel.cn/api/paas/v4` -> `https://bigmodel.cn/api/biz/subscription/list`.
fn subscription_url(base_url: &str) -> String {
    let (scheme, rest) = base_url
        .split_once("://")
        .unwrap_or(("https", base_url));
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host.strip_prefix("open.").unwrap_or(host);
    format!("{scheme}://{host}/api/biz/subscription/list")
}

/// Parse the subscription-list response into `PlanInfo` (validity range + auto-renew). `data` is
/// an array (an account may hold several subscriptions); pick the VALID one in the current period
/// (fallback: first VALID, then the first entry). `valid` is
/// `"YYYY-MM-DD HH:MM:SS-YYYY-MM-DD HH:MM:SS"` — two fixed-width 19-char timestamps joined by
/// `-` at index 19; split by position and turn the space into `T` for ISO 8601 (`start_time`/
/// `end_time` are display-only — the UI slices the first 16 chars, so no timezone is invented).
/// `autoRenew` is `0`/`1` (int); a bool is also tolerated. `None` when nothing usable is present.
fn parse_plan_info(body: &Value) -> Option<PlanInfo> {
    let arr = body.get("data")?.as_array()?;
    let pick = arr
        .iter()
        .find(|e| {
            e.get("status").and_then(|v| v.as_str()) == Some("VALID")
                && e.get("inCurrentPeriod").and_then(|v| v.as_bool()) == Some(true)
        })
        .or_else(|| arr.iter().find(|e| e.get("status").and_then(|v| v.as_str()) == Some("VALID")))
        .or_else(|| arr.first())?;
    let valid = pick.get("valid").and_then(|v| v.as_str()).unwrap_or("");
    // Each side is "YYYY-MM-DD HH:MM:SS" (19 ASCII chars); `-` joins them at byte index 19.
    let (start_time, end_time) = match (valid.get(..19), valid.as_bytes().get(19), valid.get(20..)) {
        (Some(s), Some(&b'-'), Some(e)) => {
            (Some(s.replacen(' ', "T", 1)), Some(e.replacen(' ', "T", 1)))
        }
        _ => (None, None),
    };
    let auto_renew = pick
        .get("autoRenew")
        .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)));
    if start_time.is_none() && end_time.is_none() && auto_renew.is_none() {
        return None;
    }
    Some(PlanInfo { start_time, end_time, auto_renew })
}

/// Best-effort: GET the subscription list and set `snap.plan_info` (validity + auto-renew). Any
/// failure (network / non-2xx / parse / no usable entry) leaves `plan_info` untouched (`None`) —
/// the usage tiers stay intact. Auth is the raw api token (no `Bearer` prefix), matching the
/// console API; the usage endpoint above still uses Bearer (both accepted by the same backend).
async fn enrich_plan_info(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    snap: &mut UsageSnapshot,
) {
    let resp = match client
        .get(subscription_url(base_url))
        .header("authorization", token)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };
    if !resp.status().is_success() {
        return;
    }
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(info) = parse_plan_info(&body) {
        snap.plan_info = Some(info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UsageCreds;

    #[test]
    fn quota_url_strips_path() {
        assert_eq!(
            quota_url("https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit"
        );
        assert_eq!(
            quota_url("https://open.bigmodel.cn"),
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit"
        );
    }

    #[tokio::test]
    async fn zhipu_parses_tokens_limit() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/monitor/usage/quota/limit"))
            .and(wiremock::matchers::header("authorization", "Bearer sk-test"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"limits":[{"type":"TOKENS_LIMIT","percentage":42,"nextResetTime":"2023-11-14T22:13:20Z"}]}}),
            ))
            .mount(&mock)
            .await;

        let snap = ZhipuUsageProvider
            .query(Some("sk-test"), None, &mock.uri())
            .await
            .unwrap();
        assert_eq!(snap.used, Some(42.0));
        assert_eq!(snap.total, Some(100.0));
        assert_eq!(snap.remaining, Some(58.0));
        assert_eq!(snap.reset_at, Some(1_700_000_000));
        assert_eq!(snap.unit, "%");
        assert_eq!(snap.raw_summary, None); // raw only when the response couldn't be parsed
    }

    #[tokio::test]
    async fn zhipu_auth_failed() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/monitor/usage/quota/limit"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&mock)
            .await;
        let err = ZhipuUsageProvider.query(Some("sk-bad"), None, &mock.uri()).await.unwrap_err();
        assert!(matches!(err, UsageError::AuthFailed(_)));
    }

    #[tokio::test]
    async fn zhipu_not_configured_without_key() {
        let err = ZhipuUsageProvider.query(None, None, "https://x").await.unwrap_err();
        assert!(matches!(err, UsageError::NotConfigured));
    }

    #[tokio::test]
    async fn zhipu_missing_tokens_limit_returns_raw() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/monitor/usage/quota/limit"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"limits":[{"type":"OTHER","percentage":1}]}}),
            ))
            .mount(&mock)
            .await;
        let snap = ZhipuUsageProvider.query(Some("sk"), None, &mock.uri()).await.unwrap();
        assert_eq!(snap.used, None);
        assert!(snap.raw_summary.is_some());
    }

    #[tokio::test]
    async fn zhipu_malformed_response_is_parse_error() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/monitor/usage/quota/limit"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"unexpected": true}),
            ))
            .mount(&mock)
            .await;
        let err = ZhipuUsageProvider.query(Some("sk"), Some(&UsageCreds::default()), &mock.uri()).await.unwrap_err();
        assert!(matches!(err, UsageError::Parse(_)));
    }

    #[test]
    fn subscription_url_strips_open_subdomain() {
        assert_eq!(
            subscription_url("https://open.bigmodel.cn/api/paas/v4"),
            "https://bigmodel.cn/api/biz/subscription/list",
        );
        assert_eq!(
            subscription_url("https://open.bigmodel.cn"),
            "https://bigmodel.cn/api/biz/subscription/list",
        );
        // No `open.` prefix -> host used as-is (best-effort for non-bigmodel hosts).
        assert_eq!(
            subscription_url("https://example.com/api/paas/v4"),
            "https://example.com/api/biz/subscription/list",
        );
    }

    #[test]
    fn parse_plan_info_from_real_response() {
        let body = serde_json::json!({
            "code": 200, "msg": "操作成功", "success": true,
            "data": [{
                "id": "230862", "productName": "GLM Coding Lite", "status": "VALID",
                "valid": "2027-02-01 10:00:00-2028-02-01 10:00:00",
                "autoRenew": 0, "inCurrentPeriod": true, "billingCycle": "annually"
            }]
        });
        let info = parse_plan_info(&body).unwrap();
        assert_eq!(info.start_time.as_deref(), Some("2027-02-01T10:00:00"));
        assert_eq!(info.end_time.as_deref(), Some("2028-02-01T10:00:00"));
        assert_eq!(info.auto_renew, Some(false));
    }

    #[test]
    fn parse_plan_info_autorenew_one_is_true() {
        let body = serde_json::json!({
            "data": [{ "status": "VALID", "valid": "2027-02-01 10:00:00-2028-02-01 10:00:00", "autoRenew": 1 }]
        });
        assert_eq!(parse_plan_info(&body).unwrap().auto_renew, Some(true));
    }

    #[test]
    fn parse_plan_info_picks_valid_in_current_period() {
        // First entry VALID but NOT current; second VALID + current -> pick the second.
        let body = serde_json::json!({ "data": [
            { "status": "VALID", "inCurrentPeriod": false, "valid": "2025-01-01 00:00:00-2026-01-01 00:00:00", "autoRenew": 0 },
            { "status": "VALID", "inCurrentPeriod": true,  "valid": "2027-02-01 10:00:00-2028-02-01 10:00:00", "autoRenew": 1 }
        ]});
        let info = parse_plan_info(&body).unwrap();
        assert_eq!(info.end_time.as_deref(), Some("2028-02-01T10:00:00"));
        assert_eq!(info.auto_renew, Some(true));
    }

    #[test]
    fn parse_plan_info_missing_or_empty_is_none() {
        assert!(parse_plan_info(&serde_json::json!({ "data": [] })).is_none());
        assert!(parse_plan_info(&serde_json::json!({})).is_none());
        // Entry with no valid range and no autoRenew -> nothing usable.
        assert!(parse_plan_info(&serde_json::json!({ "data": [{ "status": "VALID" }] })).is_none());
    }

    #[tokio::test]
    async fn zhipu_query_enriches_plan_info_from_subscription() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/monitor/usage/quota/limit"))
            .and(wiremock::matchers::header("authorization", "Bearer sk-test"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"level":"lite","limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42,"nextResetTime":"2023-11-14T22:13:20Z"}]}}),
            ))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/biz/subscription/list"))
            .and(wiremock::matchers::header("authorization", "sk-test")) // raw token, no Bearer
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"code":200,"success":true,"data":[{"status":"VALID","inCurrentPeriod":true,"valid":"2027-02-01 10:00:00-2028-02-01 10:00:00","autoRenew":0}]}),
            ))
            .mount(&mock)
            .await;
        let snap = ZhipuUsageProvider.query(Some("sk-test"), None, &mock.uri()).await.unwrap();
        assert_eq!(snap.plan.as_deref(), Some("lite")); // level still drives the plan label
        let info = snap.plan_info.unwrap();
        assert_eq!(info.start_time.as_deref(), Some("2027-02-01T10:00:00"));
        assert_eq!(info.end_time.as_deref(), Some("2028-02-01T10:00:00"));
        assert_eq!(info.auto_renew, Some(false));
    }

    #[tokio::test]
    async fn zhipu_query_subscription_failure_leaves_plan_info_none() {
        // The subscription step is best-effort: a 500 there must NOT fail the query or drop the
        // usage tiers — plan_info just stays None.
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/monitor/usage/quota/limit"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42,"nextResetTime":"2023-11-14T22:13:20Z"}]}}),
            ))
            .mount(&mock)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/biz/subscription/list"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&mock)
            .await;
        let snap = ZhipuUsageProvider.query(Some("sk-test"), None, &mock.uri()).await.unwrap();
        assert_eq!(snap.used, Some(42.0)); // usage tiers intact
        assert_eq!(snap.plan_info, None); // subscription failed -> no plan_info
    }
}
