use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// User's drag-order for usage-display surfaces (provider ids, most-significant first).
    /// Empty = fall back to `providers` config order. Shared by 套餐用量 tab, 概览 chips, tray.
    #[serde(default)]
    pub usage_order: Vec<String>,
    /// User's drag-order for the route list (profile ids, first-displayed first).
    /// Empty = fall back to `profiles` config order. Shared by 路由 tab, 概览 路由 card, tray.
    #[serde(default)]
    pub route_order: Vec<String>,
    #[serde(default)]
    pub settings: Settings,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: vec![],
            models: vec![],
            profiles: vec![],
            usage_order: vec![],
            route_order: vec![],
            settings: Settings::default(),
        }
    }
}

pub const MIN_USAGE_REFRESH_SECS: u32 = 30;
pub const DEFAULT_USAGE_REFRESH_SECS: u32 = 60;
pub const MAX_USAGE_REFRESH_SECS: u32 = 3600;

pub const ALLOWED_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
pub const DEFAULT_LOG_LEVEL: &str = "info";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_usage_refresh_interval_secs")]
    pub usage_refresh_interval_secs: u32,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// 无系统密钥环时是否已授权将密钥以明文存入 secrets.json（Linux 等场景）。
    /// None=未授权（每次启动询问）；Some(true)=已授权（记住）。见 spec §4.2.3。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_store_fallback: Option<bool>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            port: default_port(),
            autostart: false,
            usage_refresh_interval_secs: DEFAULT_USAGE_REFRESH_SECS,
            log_level: DEFAULT_LOG_LEVEL.into(),
            secret_store_fallback: None,
        }
    }
}
fn default_port() -> u16 {
    6950
}
fn default_usage_refresh_interval_secs() -> u32 {
    DEFAULT_USAGE_REFRESH_SECS
}
fn default_log_level() -> String {
    DEFAULT_LOG_LEVEL.into()
}

/// Clamp a usage-refresh interval (seconds) into the allowed [30, 3600] range.
/// Applied on read (`get_settings`) so a hand-edited config file can never drive
/// polling below the floor.
pub fn clamp_usage_refresh_secs(secs: u32) -> u32 {
    secs.clamp(MIN_USAGE_REFRESH_SECS, MAX_USAGE_REFRESH_SECS)
}

/// Normalize a stored/passed log level: lowercase; if not one of the five allowed, fall back to
/// `info`. Applied on read so a hand-edited config can never set an invalid level. Returns an
/// owned `String` (cloned once per call; only on settings read/save).
pub fn normalize_log_level(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if ALLOWED_LOG_LEVELS.contains(&lower.as_str()) {
        lower
    } else {
        DEFAULT_LOG_LEVEL.into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    /// Vendor kind slug — the routing key (usage adapter, Volcengine plan discovery, catalog
    /// lookup, rate-limit classification) and source of base_url defaults + the list-row tag.
    /// Known: `zhipu`, `deepseek`, `volcengine-agent`, `volcengine-coding`, `qianwen-token`;
    /// else custom. `id` is the opaque PK. `deepseek`/`zhipu`/`qianwen-token` are Bearer-key only
    /// for inference (no AKSK usage_creds); `qianwen-token` usage goes through the cookie-based
    /// `usage/qianwen.rs` adapter (qianwenai console cookie captured by the in-app login window).
    #[serde(default)]
    pub vendor: String,
    pub display_name: String,
    /// OpenAI-compatible endpoint root, e.g. https://open.bigmodel.cn/api/paas/v4. Used for
    /// /chat/completions forwarding, /models discovery, connection test, and usage queries.
    #[serde(default)]
    pub openai_base_url: Option<String>,
    /// Anthropic-protocol endpoint root (optional). Used for /v1/messages forwarding.
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    #[serde(default)]
    pub usage_creds: Option<UsageCreds>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageCreds {
    /// Volcengine AccessKey ID - an account identifier (not secret). Persisted in config.
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// Volcengine Secret Access Key - the actual secret. Stored in the OS keyring
    /// (`SecretStore::set_usage_sk`), NOT in this config file. `#[serde(skip)]` guarantees it is
    /// never written to `app_config.json`; it is merged in from the keyring at query time
    /// (`AppStateInner::usage_creds`). Existing plaintext SKs are migrated to the keyring on
    /// startup (`store::migrate_usage_sk_to_keyring`).
    #[serde(skip)]
    pub secret_access_key: Option<String>,
    /// 千问 (Qianwen) (qianwenai console) usage-session cookie — a serialized `Cookie:` header
    /// captured by the in-app login window. `#[serde(skip)]`: never persisted to app_config.json;
    /// loaded from the keyring (`{provider_id}::usage_cookie`) in `AppStateInner::usage_creds`.
    #[serde(skip)]
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub id: String,
    pub provider_id: String,
    #[serde(default)]
    pub source: ModelSource,
    /// Upstream model id sent to the provider (e.g. glm-4.6). Protocol + base_url are inherited
    /// from the provider (openai_base_url / anthropic_base_url).
    #[serde(default)]
    pub upstream_model_id: String,
    #[serde(default)]
    pub cooldown_seconds: Option<u64>,
    #[serde(default)]
    pub fallback_target_model_id: Option<String>,
    /// 时段故障转移策略（复用 Strategy，与入口层 Profile.strategies 同构）。
    #[serde(default)]
    pub fallback_strategies: Vec<Strategy>,
    /// 总开关：false 跳过时段策略，直接走 fallback_target_model_id。默认 true。
    #[serde(default = "default_fb_strategies_enabled")]
    pub fallback_strategies_enabled: bool,
    /// 临时限流（额度未耗尽）时的原地重试次数。0 = 关闭（立即熔断 + fallback）。默认 2。
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
    /// 每次重试前等待秒数。默认 5。
    #[serde(default = "default_retry_delay")]
    pub retry_delay_secs: u64,
}
fn default_fb_strategies_enabled() -> bool { true }
fn default_retry_count() -> u32 { 2 }
fn default_retry_delay() -> u64 { 5 }

/// Manual (not derived) so `fallback_strategies_enabled` defaults to `true`, matching the serde
/// default (`default_fb_strategies_enabled`) and the on-disk semantics of an old config that omits
/// the field. A `#[derive(Default)]` would yield `false` for the bool, diverging from serde and
/// silently disabling time-strategies on any `Model { ..Default::default() }` construction.
/// Mirrors `Profile`'s manual `impl Default` (the sibling entry-layer struct).
impl Default for Model {
    fn default() -> Self {
        Self {
            id: String::new(),
            provider_id: String::new(),
            source: ModelSource::default(),
            upstream_model_id: String::new(),
            cooldown_seconds: None,
            fallback_target_model_id: None,
            fallback_strategies: vec![],
            fallback_strategies_enabled: true,
            retry_count: default_retry_count(),
            retry_delay_secs: default_retry_delay(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelSource {
    #[default]
    Discovered,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 兜底模型：无策略命中/失效/总开关关时的入口模型。
    pub backing_model_id: String,
    #[serde(default)]
    pub strategies: Vec<Strategy>,
    /// 总开关：false 跳过全部策略，直接走兜底。默认 true（空集即无操作）。
    #[serde(default = "default_strategies_enabled")]
    pub strategies_enabled: bool,
}
fn default_strategies_enabled() -> bool { true }

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            aliases: vec![],
            backing_model_id: String::new(),
            strategies: vec![],
            strategies_enabled: true,
        }
    }
}

/// 一条转发策略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Strategy {
    pub id: String,
    /// 1..=5，数字越小优先级越高。多条命中取最小；并列按列表顺序（靠前胜）。
    pub priority: u8,
    /// 单策略启停（默认 true）。总开关 `Profile.strategies_enabled` 才是"一键回退"。
    #[serde(default = "default_strategy_enabled")]
    pub enabled: bool,
    pub kind: StrategyKind,
}
fn default_strategy_enabled() -> bool { true }

/// 策略类型（tag 表示，序列化为 {"type":"time", ...}），预留扩展。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StrategyKind {
    Time(TimeStrategy),
}

impl StrategyKind {
    pub fn as_time(&self) -> Option<&TimeStrategy> {
        match self { StrategyKind::Time(t) => Some(t) }
    }
}

/// 按天重复的时间窗口（支持跨夜）。minute_of_day 0..=1439。
/// start 含、end 不含（半开区间）；start > end 表示跨夜（自当天 start 起，至次日 end 止）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeStrategy {
    /// 命中此窗口的"起始日"星期集合（窗口下半夜归属该日的次日）。1=周一..=7=周日。
    pub days_of_week: Vec<u8>,
    pub time_start: u16,
    pub time_end: u16,
    pub model_id: String,
}

/// Verdict for whether a fallback model's context window can hold the primary's traffic.
/// Serialized lowercase (`ok`/`smaller`/`unknown`) - the TS mirror (Task 6) expects those strings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ContextCheckStatus {
    Ok,
    Smaller,
    Unknown,
}

/// Result of comparing a primary model's effective context size against its fallback's.
/// `primary_size`/`fallback_size` are the resolved effective sizes (manual override > catalog > None).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextCheckResult {
    pub status: ContextCheckStatus,
    pub primary_size: Option<u32>,
    pub fallback_size: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_roundtrips() {
        let cfg = AppConfig {
            providers: vec![Provider {
                id: "zhipu".into(),
                vendor: "zhipu".into(),
                display_name: "智谱".into(),
                openai_base_url: Some("https://open.bigmodel.cn/api/paas/v4".into()),
                anthropic_base_url: None,
                usage_creds: None,
            }],
            models: vec![Model {
                id: "m_glm46".into(),
                provider_id: "zhipu".into(),
                source: ModelSource::Discovered,
                upstream_model_id: "glm-4.6".into(),
                cooldown_seconds: Some(300),
                fallback_target_model_id: Some("m_glm45".into()),
                ..Default::default()
            }],
            profiles: vec![Profile {
                id: "p_main".into(),
                name: "glm-5.2".into(),
                aliases: vec!["claude-sonnet-4".into()],
                backing_model_id: "m_glm46".into(),
                ..Default::default()
            }],
            usage_order: vec![],
            route_order: vec![],
            settings: Settings { port: 6950, autostart: false, usage_refresh_interval_secs: 60, log_level: "info".into(), secret_store_fallback: None },
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(back.settings.port, 6950);
        assert_eq!(back.settings.usage_refresh_interval_secs, 60);
        assert_eq!(back.settings.log_level, "info");
        assert_eq!(back.providers[0].openai_base_url.as_deref(), Some("https://open.bigmodel.cn/api/paas/v4"));
        assert_eq!(back.providers[0].vendor, "zhipu");
        assert_eq!(back.models[0].upstream_model_id, "glm-4.6");
        assert!(back.profiles[0].aliases.contains(&"claude-sonnet-4".to_string()));
    }

    #[test]
    fn clamp_usage_refresh_secs_enforces_range() {
        assert_eq!(clamp_usage_refresh_secs(0), 30);
        assert_eq!(clamp_usage_refresh_secs(5), 30);
        assert_eq!(clamp_usage_refresh_secs(29), 30);
        assert_eq!(clamp_usage_refresh_secs(30), 30);
        assert_eq!(clamp_usage_refresh_secs(60), 60);
        assert_eq!(clamp_usage_refresh_secs(120), 120);
        assert_eq!(clamp_usage_refresh_secs(3600), 3600);
        assert_eq!(clamp_usage_refresh_secs(99999), 3600);
    }

    #[test]
    fn settings_default_when_field_absent() {
        // Old config files (pre-feature) omit usage_refresh_interval_secs;
        // serde default must fill 60 so existing installs keep working.
        let json = r#"{"port": 7000, "autostart": true}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.usage_refresh_interval_secs, DEFAULT_USAGE_REFRESH_SECS);
    }

    #[test]
    fn model_loads_without_display_name() {
        // display_name is removed; a model JSON that omits it (the new shape) must deserialize.
        let json = r#"{"id":"m","provider_id":"zhipu","upstream_model_id":"glm-4.6"}"#;
        let model: Model = serde_json::from_str(json).unwrap();
        assert_eq!(model.upstream_model_id, "glm-4.6");
    }

    #[test]
    fn legacy_provider_without_vendor_defaults_empty() {
        // Legacy app_config.json omits `vendor`; serde must fill "" so the Task 2
        // migration can derive it from the id.
        let json = r#"{"id":"zhipu","display_name":"智谱","openai_base_url":"https://x"}"#;
        let p: Provider = serde_json::from_str(json).unwrap();
        assert_eq!(p.vendor, "");
    }

    #[test]
    fn normalize_log_level_lowercases_and_validates() {
        assert_eq!(normalize_log_level("INFO"), "info");
        assert_eq!(normalize_log_level("Debug"), "debug");
        assert_eq!(normalize_log_level("warn"), "warn");
        assert_eq!(normalize_log_level(""), "info");      // empty -> default
        assert_eq!(normalize_log_level("verbose"), "info"); // unknown -> default
    }

    #[test]
    fn settings_log_level_defaults_when_absent() {
        let json = r#"{"port": 7000, "autostart": true}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.log_level, DEFAULT_LOG_LEVEL);
    }

    #[test]
    fn usage_order_defaults_empty_when_absent() {
        // An old config JSON that never had a usage_order field must deserialize to [].
        let json = r#"{"providers":[],"models":[],"profiles":[],"settings":{}}"#;
        let cfg: AppConfig = serde_json::from_str(json).expect("deserialize");
        assert!(cfg.usage_order.is_empty());
    }

    #[test]
    fn usage_order_round_trips() {
        let mut cfg = AppConfig::default();
        cfg.usage_order = vec!["p_b".into(), "p_a".into()];
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.usage_order, vec!["p_b".to_string(), "p_a".to_string()]);
    }

    #[test]
    fn route_order_defaults_empty_when_absent() {
        // An old config JSON that never had a route_order field must deserialize to [].
        let json = r#"{"providers":[],"models":[],"profiles":[],"settings":{}}"#;
        let cfg: AppConfig = serde_json::from_str(json).expect("deserialize");
        assert!(cfg.route_order.is_empty());
    }

    #[test]
    fn route_order_round_trips() {
        let mut cfg = AppConfig::default();
        cfg.route_order = vec!["p_b".into(), "p_a".into()];
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.route_order, vec!["p_b".to_string(), "p_a".to_string()]);
    }

    #[test]
    fn settings_secret_store_fallback_defaults_none_and_parses() {
        // 旧配置无此字段 → None（向后兼容）
        let old: Settings = serde_json::from_str(r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info"}"#).unwrap();
        assert_eq!(old.secret_store_fallback, None);
        // 显式 true → Some(true)
        let granted: Settings = serde_json::from_str(
            r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info","secret_store_fallback":true}"#,
        ).unwrap();
        assert_eq!(granted.secret_store_fallback, Some(true));
        // Default → None
        assert_eq!(Settings::default().secret_store_fallback, None);
    }

    #[test]
    fn profile_strategies_default_when_absent() {
        // Old config JSON (pre-feature) omits strategies + strategies_enabled.
        let json = r#"{"id":"p","name":"glm","backing_model_id":"m"}"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert!(p.strategies.is_empty());
        assert!(p.strategies_enabled); // default true → empty set is a no-op
    }

    #[test]
    fn profile_with_strategy_roundtrips() {
        let p = Profile {
            id: "p".into(), name: "glm".into(), aliases: vec![],
            backing_model_id: "m".into(),
            strategies: vec![Strategy {
                id: "s1".into(), priority: 2, enabled: true,
                kind: StrategyKind::Time(TimeStrategy {
                    days_of_week: vec![1, 2, 3, 4, 5],
                    time_start: 1320, time_end: 480, model_id: "m2".into(),
                }),
            }],
            strategies_enabled: true,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        // Internally-tagged kind serializes as {"type":"time",...}
        assert!(json.contains(r#""type":"time""#));
        assert!(json.contains(r#""days_of_week":[1,2,3,4,5]"#));
    }

    #[test]
    fn model_fallback_strategies_default_when_absent() {
        // Old config JSON (pre-feature) omits both fields; serde default fills [] / true.
        let json = r#"{"id":"m","provider_id":"zhipu","upstream_model_id":"glm-4.6"}"#;
        let model: Model = serde_json::from_str(json).unwrap();
        assert!(model.fallback_strategies.is_empty());
        assert!(model.fallback_strategies_enabled);
    }

    #[test]
    fn model_with_fallback_strategies_roundtrips() {
        let m = Model {
            id: "m".into(), provider_id: "zhipu".into(), upstream_model_id: "glm".into(),
            fallback_target_model_id: Some("m2".into()),
            fallback_strategies: vec![Strategy {
                id: "s".into(), priority: 1, enabled: true,
                kind: StrategyKind::Time(TimeStrategy {
                    days_of_week: vec![1], time_start: 1320, time_end: 360, model_id: "m2".into(),
                }),
            }],
            fallback_strategies_enabled: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn model_default_enables_fallback_strategies() {
        // `Model::default()` (manual impl) must agree with the serde default: strategies enabled,
        // empty strategy list. Guards against a `#[derive(Default)]` regression that would silently
        // flip the bool to `false` (the latent bug fixed post-review).
        let m = Model::default();
        assert!(m.fallback_strategies.is_empty());
        assert!(m.fallback_strategies_enabled);
    }

    #[test]
    fn model_retry_defaults_align_between_default_and_serde() {
        // Code default and serde default must agree (spec §3 / §7).
        let m = Model::default();
        assert_eq!(m.retry_count, default_retry_count());
        assert_eq!(m.retry_count, 2);
        assert_eq!(m.retry_delay_secs, default_retry_delay());
        assert_eq!(m.retry_delay_secs, 5);

        // A Model JSON that OMITS both fields deserializes to the same defaults.
        let json = r#"{
            "id":"m","provider_id":"p","upstream_model_id":"glm",
            "source":"manual","cooldown_seconds":null,"fallback_target_model_id":null,
            "fallback_strategies":[],"fallback_strategies_enabled":true
        }"#;
        let m: Model = serde_json::from_str(json).unwrap();
        assert_eq!(m.retry_count, 2);
        assert_eq!(m.retry_delay_secs, 5);

        // Round-trips through serialize→deserialize.
        let json = serde_json::to_string(&Model::default()).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(back.retry_count, 2);
        assert_eq!(back.retry_delay_secs, 5);
    }
}
