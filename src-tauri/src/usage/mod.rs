pub mod qianwen;
pub mod tiers;
pub mod deepseek;
pub mod volcengine;
pub mod zhipu;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use crate::config::UsageCreds;

// Re-export commonly used types from tiers
pub use tiers::{QuotaTier, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY};

/// Best-effort provider usage snapshot. Every field except `unit` is optional - adapters
/// surface whatever the provider returns. `reset_at` (epoch secs) is consumed by the circuit
/// breaker to set `recover_at` when a model trips (spec §6.3).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UsageSnapshot {
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    /// Package/quota window reset time (epoch secs). Feeds the breaker's `recover_at`.
    pub reset_at: Option<i64>,
    /// Display unit ("%" / "tokens" / "CNY" ...).
    pub unit: String,
    /// Original text fallback when the response could not be structured.
    pub raw_summary: Option<String>,
    /// Plan tier label for the card header — the tier only, no product/plan-type prefix (e.g.
    /// "Pro", "Large", "Lite"); the vendor + plan type already show via the provider label.
    /// `None` when the tier is unknown (query failed / not yet fetched).
    #[serde(default)]
    pub plan: Option<String>,
    /// Per-window breakdown (5h / weekly / monthly) for the Usage page. Empty when the provider
    /// only surfaces a single aggregate or the response could not be structured (then `raw_summary`
    /// carries the raw payload instead).
    #[serde(default)]
    pub tiers: Vec<UsageTier>,
    /// Billing model discriminator: `"plan"` (tiered windows, default) for plan-based billing
    /// (智谱/火山); `"consumption"` for pay-as-you-go balance display (DeepSeek).
    #[serde(default = "default_billing_model")]
    pub billing_model: String,
    /// Plan subscription metadata for the card-header tag tooltip. `None` for providers that
    /// don't surface it (DeepSeek) or when the query failed (tag still shows `plan`, just without
    /// a tooltip). 火山 (`GetPersonalPlan`), 千问 (`subscription` gateway) and 智谱
    /// (`/api/biz/subscription/list`) all populate it on a best-effort basis.
    #[serde(default)]
    pub plan_info: Option<PlanInfo>,
}

/// Plan subscription metadata shown as a tooltip on the plan-type tag. Populated (best-effort)
/// by the Volcengine adapter (`GetPersonalPlan`), the Qianwen adapter (`subscription` gateway)
/// and the Zhipu adapter (`/api/biz/subscription/list`); `None` for other providers / failed
/// queries.
#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct PlanInfo {
    /// 首次生效时间 (ISO 8601, e.g. "2026-07-30T00:00:00+08:00").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    /// 当前到期时间 (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    /// 是否已开启自动续费.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_renew: Option<bool>,
}

/// Serde default for `billing_model` (see the `#[serde(default = "...")]` attribute on that
/// field). A no-op while `UsageSnapshot` derives `Serialize` only - the in-memory `UsageCache`
/// never deserializes - so serde never emits the call and the helper reads as dead code. Kept as
/// the documented default should `Deserialize` be added.
#[allow(dead_code)]
fn default_billing_model() -> String {
    "plan".to_string()
}

/// One quota window in a `UsageSnapshot`. `window` matches the `TIER_*` constants
/// (`five_hour` / `weekly_limit` / `monthly`); the UI renders those three in a fixed order.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UsageTier {
    /// `five_hour` | `weekly_limit` | `monthly`
    pub window: String,
    /// Percentage used (0-100).
    pub used_pct: Option<f64>,
    /// Window reset time (epoch secs).
    pub reset_at: Option<i64>,
}

/// Parse an ISO8601 reset-time string (as produced by `tiers::extract_reset_time` /
/// `millis_to_iso8601`) back to epoch secs. `None` on parse failure (caller degrades).
pub(crate) fn iso_to_epoch_secs(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp())
}

/// Build a `UsageSnapshot` from parsed `QuotaTier`s: the first tier drives `total`/`remaining`/
/// `reset_at` (breaker + consumption math); display-layer primary selection is via `primary_used`
/// (5h -> 周 -> 月). Every tier is surfaced structurally in `tiers` for the Usage page's per-window
/// breakdown. `raw_summary` stays `None` (the breakdown is structural, not a raw fallback).
pub(crate) fn snapshot_from_tiers(tiers: &[QuotaTier], plan: &Option<String>) -> UsageSnapshot {
    let primary = tiers.first();
    let used = primary.map(|t| t.utilization);
    let total = used.map(|_| 100.0);
    let remaining = used.map(|u| (100.0 - u).max(0.0));
    let reset_at = primary
        .and_then(|t| t.resets_at.as_deref())
        .and_then(iso_to_epoch_secs);
    let snap_tiers = tiers
        .iter()
        .map(|t| UsageTier {
            window: t.name.clone(),
            used_pct: Some(t.utilization),
            reset_at: t.resets_at.as_deref().and_then(iso_to_epoch_secs),
        })
        .collect();
    UsageSnapshot {
        total,
        remaining,
        reset_at,
        unit: "%".into(),
        raw_summary: None,
        plan: plan.clone(),
        tiers: snap_tiers,
        billing_model: "plan".into(),
        plan_info: None,
    }
}

/// plan 主值窗口的选取顺序：最紧、最可操作的窗口优先。
const PRIMARY_WINDOW_ORDER: &[&str] = &[TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT, TIER_MONTHLY];

/// plan 账号的主已用百分比：按 5h → 周 → 月 取首个有具体值的窗口，连同窗口名一起返回
/// （供展示层标注窗口）。无任何窗口有值时返回 `None`（账号从单值展示中隐藏）。
pub fn primary_used(tiers: &[UsageTier]) -> Option<(f64, &'static str)> {
    for &window in PRIMARY_WINDOW_ORDER {
        if let Some(t) = tiers.iter().find(|t| t.window == window) {
            if let Some(pct) = t.used_pct {
                return Some((pct, window));
            }
        }
    }
    None
}

/// 套餐额度查询结果
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SubscriptionQuota {
    /// 工具标识（固定为 "coding_plan"）
    pub tool: String,
    /// 凭据状态
    pub credential_status: CredentialStatus,
    /// 凭据消息（套餐等级等）
    pub credential_message: Option<String>,
    /// 查询是否成功
    pub success: bool,
    /// 额度层级列表
    pub tiers: Vec<QuotaTier>,
    /// 额外用量信息
    pub extra_usage: Option<String>,
    /// 错误信息（失败时）
    pub error: Option<String>,
    /// 查询时间戳
    pub queried_at: Option<i64>,
}

/// 凭据状态
#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum CredentialStatus {
    /// 凭据有效
    Valid,
    /// 凭据过期或无效
    Expired,
    /// 凭据未找到（未配置）
    NotFound,
}

#[derive(Debug, Error)]
pub enum UsageError {
    #[error("usage credentials not configured for this provider")]
    NotConfigured,
    #[error("usage query unsupported for this provider")]
    UnsupportedByProvider,
    #[error("usage query network error: {0}")]
    Network(String),
    #[error("usage query authentication failed: {0}")]
    AuthFailed(String),
    #[error("usage response parse error: {0}")]
    Parse(String),
    #[error("coding plan quota query failed: {0}")]
    CodingPlan(String),
}

/// Per-provider usage/quota adapter. Each impl uses the credentials it needs: 智谱 uses the
/// inference `api_key` (Bearer); 火山 uses `usage_creds` (AK/SK, control-plane OpenAPI Sig V4).
/// `base_url` is the provider's configured base URL (adapters derive their management endpoint).
#[async_trait]
pub trait UsageProvider: Send + Sync {
    async fn query(
        &self,
        api_key: Option<&str>,
        usage_creds: Option<&UsageCreds>,
        base_url: &str,
    ) -> Result<UsageSnapshot, UsageError>;
}

/// Pick the usage adapter for a vendor slug. Returns `None` for unknown vendors / opaque provider
/// ids (usage N/A). Note: the argument is the **vendor** routing key (e.g. `zhipu`,
/// `volcengine-coding`), NOT the opaque per-account `provider_id`.
pub fn usage_provider_for(vendor: &str) -> Option<Box<dyn UsageProvider>> {
    match vendor {
        "zhipu" => Some(Box::new(zhipu::ZhipuUsageProvider)),
        "deepseek" => Some(Box::new(deepseek::DeepseekUsageProvider)),
        "volcengine" | "volcengine-agent" | "volcengine-coding" => {
            // vendor 已编码套餐类型（agent/coding），用量查询据此选 action——与模型发现
            // `volcengine_plan_action` 对齐。`from_vendor` 对三个 slug 都返回 `Some`。
            let plan = volcengine::VolcPlanKind::from_vendor(vendor)
                .expect("matched a volcengine slug above");
            Some(Box::new(volcengine::VolcengineUsageProvider { plan }))
        }
        "qianwen-token" => Some(Box::new(qianwen::QianwenUsageProvider)),
        _ => None,
    }
}

const DEFAULT_USAGE_TTL_SECS: i64 = 60;

/// Best-effort usage cache (spec §6.4, TTL ~60s). Epoch-secs-based so it is deterministic in
/// tests (like `HealthRegistry`). Not `Clone` (contains a `Mutex`); stored by value behind
/// the `Arc<AppStateInner>`.
pub struct UsageCache {
    inner: Mutex<HashMap<String, (i64, UsageSnapshot)>>,
    ttl_secs: i64,
}

impl UsageCache {
    pub fn new(ttl_secs: i64) -> Self {
        Self { inner: Mutex::new(HashMap::new()), ttl_secs }
    }

    pub fn get(&self, provider_id: &str, now: i64) -> Option<UsageSnapshot> {
        let map = self.inner.lock().unwrap();
        if let Some((stored, snap)) = map.get(provider_id) {
            if now - stored < self.ttl_secs {
                return Some(snap.clone());
            }
        }
        None
    }

    pub fn set(&self, provider_id: &str, snapshot: UsageSnapshot, now: i64) {
        self.inner.lock().unwrap().insert(provider_id.into(), (now, snapshot));
    }

    pub fn invalidate(&self, provider_id: &str) {
        self.inner.lock().unwrap().remove(provider_id);
    }
}

impl Default for UsageCache {
    fn default() -> Self {
        Self::new(DEFAULT_USAGE_TTL_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_within_ttl() {
        let c = UsageCache::new(60);
        assert!(c.get("zhipu", 1000).is_none());
        c.set("zhipu", snap(50.0), 1000);
        assert_eq!(c.get("zhipu", 1059).map(|s| s.remaining), Some(Some(50.0)));
    }

    #[test]
    fn cache_miss_after_ttl() {
        let c = UsageCache::new(60);
        c.set("zhipu", snap(50.0), 1000);
        assert!(c.get("zhipu", 1060).is_none()); // exactly ttl -> expired (now - stored < ttl is false)
    }

    #[test]
    fn cache_invalidate() {
        let c = UsageCache::new(60);
        c.set("zhipu", snap(50.0), 1000);
        c.invalidate("zhipu");
        assert!(c.get("zhipu", 1000).is_none());
    }

    #[test]
    fn primary_used_picks_five_hour_first() {
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: Some(80.0), reset_at: None },
            UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: Some(40.0), reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), Some((80.0, TIER_FIVE_HOUR)));
    }

    #[test]
    fn primary_used_falls_back_to_weekly_when_no_five_hour() {
        // 千问 regression：5h 缺、周档有 -> 周档为主值。
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: None, reset_at: None },
            UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: Some(9.0), reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), Some((9.0, TIER_WEEKLY_LIMIT)));
    }

    #[test]
    fn primary_used_falls_back_to_monthly() {
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: None, reset_at: None },
            UsageTier { window: TIER_WEEKLY_LIMIT.to_string(), used_pct: None, reset_at: None },
            UsageTier { window: TIER_MONTHLY.to_string(), used_pct: Some(10.0), reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), Some((10.0, TIER_MONTHLY)));
    }

    #[test]
    fn primary_used_none_when_no_window_has_value() {
        let tiers = vec![
            UsageTier { window: TIER_FIVE_HOUR.to_string(), used_pct: None, reset_at: None },
        ];
        assert_eq!(primary_used(&tiers), None);
        assert_eq!(primary_used(&[]), None);
    }

    #[test]
    fn registry_known_and_unknown() {
        assert!(usage_provider_for("zhipu").is_some());
        assert!(usage_provider_for("deepseek").is_some());
        assert!(usage_provider_for("volcengine").is_some());
        assert!(usage_provider_for("volcengine-agent").is_some());
        assert!(usage_provider_for("volcengine-coding").is_some());
        assert!(usage_provider_for("custom").is_none());
    }

    #[test]
    fn usage_provider_for_matches_on_vendor() {
        assert!(usage_provider_for("zhipu").is_some());
        assert!(usage_provider_for("deepseek").is_some());
        assert!(usage_provider_for("volcengine-coding").is_some());
        assert!(usage_provider_for("volcengine-agent").is_some());
        assert!(usage_provider_for("prov_abc").is_none()); // an opaque id is NOT a vendor
    }

    fn snap(used: f64) -> UsageSnapshot {
        UsageSnapshot {
            total: Some(100.0),
            remaining: Some(100.0 - used),
            reset_at: None,
            unit: "%".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "plan".into(),
            plan_info: None,
        }
    }
}
