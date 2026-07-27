//! 套餐额度查询的共享类型与解析工具。
//!
//! 跨适配器复用的额度层级名常量、`QuotaTier` 结构，以及把厂商响应里的
//! 时间戳/数值字段归一化的几个无状态 helper（智谱、火山、千问等套餐适配器共用）。

use serde::Serialize;

/// 额度层级类型
pub const TIER_FIVE_HOUR: &str = "five_hour";
pub const TIER_WEEKLY_LIMIT: &str = "weekly_limit";
pub const TIER_MONTHLY: &str = "monthly";

/// 额度层级信息
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct QuotaTier {
    /// 层级名称：five_hour / weekly_limit / monthly
    pub name: String,
    /// 已用百分比（0-100）
    pub utilization: f64,
    /// 重置时间（ISO8601 字符串）
    pub resets_at: Option<String>,
    /// 已用金额（USD）- 仅某些供应商提供
    pub used_value_usd: Option<f64>,
    /// 总额度金额（USD）- 仅某些供应商提供
    pub max_value_usd: Option<f64>,
}

/// 将毫秒时间戳转换为 ISO8601（RFC3339）字符串。越界或无法表示的值返回 `None`。
pub fn millis_to_iso8601(ms: i64) -> Option<String> {
    let secs = ms / 1000;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nsecs).map(|dt| dt.to_rfc3339())
}

/// 从 JSON 字段提取重置时间，兼容三种形态：
/// - ISO8601 字符串：原样返回；
/// - 正整数：按秒（< 1e12）或毫秒（≥ 1e12）自动识别并转为 RFC3339；
/// - 0/负数（如火山 session 无活跃窗口回 -1）：视为无重置时间，返回 `None`。
pub fn extract_reset_time(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    if let Some(n) = value.as_i64() {
        if n <= 0 {
            return None;
        }
        let ms = if n < 1_000_000_000_000 { n * 1000 } else { n };
        return millis_to_iso8601(ms);
    }
    None
}

/// 解析 JSON 值为 f64，兼容数字与数字字符串（如 `100` 与 `"100"`）；其余返回 `None`。
pub fn parse_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_reset_time() {
        // 字符串格式
        assert_eq!(
            extract_reset_time(&json!("2024-01-01T00:00:00Z")),
            Some("2024-01-01T00:00:00Z".to_string())
        );

        // 秒级时间戳
        let result = extract_reset_time(&json!(1_700_000_000_i64));
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("2023-"));

        // 毫秒级时间戳
        let result = extract_reset_time(&json!(1_700_000_000_000_i64));
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("2023-"));

        // 无效时间戳
        assert!(extract_reset_time(&json!(0)).is_none());
        assert!(extract_reset_time(&json!(-1)).is_none());
    }

    #[test]
    fn test_parse_f64() {
        // 数字格式
        assert_eq!(parse_f64(&json!(42.5)), Some(42.5));
        assert_eq!(parse_f64(&json!(100)), Some(100.0));

        // 字符串格式
        assert_eq!(parse_f64(&json!("42.5")), Some(42.5));
        assert_eq!(parse_f64(&json!("100")), Some(100.0));

        // 无效格式
        assert!(parse_f64(&json!("invalid")).is_none());
        assert!(parse_f64(&json!(null)).is_none());
    }
}
