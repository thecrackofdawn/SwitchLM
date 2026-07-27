use serde_json::Value;

/// Classify an upstream response as a provider-specific rate-limit / quota-exhausted error.
///
/// `status` = HTTP status code if known; `body` = the raw response body (may be JSON or not).
/// Returns `true` only for rate-limit/quota signals (-> trip breaker + fallback); non-rate-limit
/// errors (401/400/500/network) return `false` (-> passthrough, no trip). See spec §3.5.
pub fn is_rate_limit_error(vendor: &str, status: Option<u16>, body: &str) -> bool {
    if status == Some(429) {
        return true;
    }
    // DeepSeek returns 402 when balance is insufficient — treat as rate-limit → fallback.
    if vendor == "deepseek" && status == Some(402) {
        return true;
    }
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return false, // non-JSON body: only the 429 status check applies
    };
    rate_limit_in_json(vendor, &v)
}

/// Same classification on a single SSE `data:` payload (an upstream error event is itself a
/// JSON object, e.g. `{"error":{"code":1302,...}}`). Used for the streaming boundary check
/// before any content has been forwarded to the client.
pub fn sse_event_is_rate_limit(vendor: &str, event_data: &str) -> bool {
    is_rate_limit_error(vendor, None, event_data)
}

fn rate_limit_in_json(vendor: &str, v: &Value) -> bool {
    let (code, message) = extract_code_message(v);

    // Provider-specific error codes (the precise signal). The generic HTTP-429 check in
    // `is_rate_limit_error` already catches every code below that arrives as 429; these arms matter
    // for non-429 signals (e.g. 火山 403 overdue, 400/404 plan errors) and as the streaming
    // first-event safety net (where the HTTP status is not yet known).
    match vendor {
        "zhipu" => {
            // 1214 is NOT here — per the official table it is "${field} 参数非法" (HTTP 400), a
            // client parameter error. The real rate-limit/quota/balance codes: 1113 arrears,
            // 1302 rate limit, 1305 model busy, and the 1308..=1321 GLM Coding Plan
            // window/plan/fairness family (all arrive as HTTP 429). 1301 (sensitive content) is
            // deliberately excluded.
            if let Some(n) = code.as_deref().and_then(|c| c.parse::<i64>().ok()) {
                if matches!(n, 1113 | 1302 | 1305) || (1308..=1321).contains(&n) {
                    return true;
                }
            }
        }
        "volcengine" | "volcengine-agent" | "volcengine-coding" => {
            if let Some(c) = code.as_deref() {
                // Namespaced codes — prefix, not exact (real codes are RateLimitExceeded.EndpointRPMExceeded).
                if c.starts_with("RateLimitExceeded") || c.starts_with("QuotaExceeded") {
                    return true;
                }
                // Explicit 429 family + non-429 fallback-worthy signals.
                if matches!(
                    c,
                    "ServerOverloaded"
                        | "RequestBurstTooFast"
                        | "ModelAccountRpmRateLimitExceeded"
                        | "ModelAccountTpmRateLimitExceeded"
                        | "ModelAccountIpmRateLimitExceeded"
                        | "APIAccountRpmRateLimitExceeded"
                        | "AccountRateLimitExceeded"
                        | "SetLimitExceeded"
                        | "InflightBatchsizeExceeded"
                        | "AccountOverdueError"            // 403: balance exhausted (≈ DeepSeek 402)
                        | "OperationDenied.ServiceOverdue" // 403: bill overdue
                        | "InvalidSubscription"            // 400: coding plan expired / not subscribed
                        | "ModelNotOpen"                   // 404: model not activated
                        | "UnsupportedModel" // 404: model not on the coding plan → switch account
                ) {
                    return true;
                }
            }
        }
        _ => {}
    }

    // Conservative keyword scan on the message (best-effort fallback for undocumented shapes).
    if let Some(msg) = message {
        let lower = msg.to_lowercase();
        const KEYWORDS: &[&str] = &[
            "rate_limit",
            "rate limit",
            "rate-limit",
            "usage limited",
            "资源耗尽",
            "quota",
            "throttl", // throttle / throttling / throttled
            "too many requests",
            "欠费", // arrears (zhipu / undocumented shapes)
            "overdue",
            "insufficient balance", // deepseek-style balance exhaustion
        ];
        if KEYWORDS.iter().any(|k| lower.contains(k)) {
            return true;
        }
    }
    false
}

/// Pull `code` (as a string, whether the JSON stored it as a string or number) and `message`
/// from `v["error"]` if present, else from `v` itself. Handles 智谱's `{"error":{"code":1302}}`
/// (numeric) and `{"error":{"code":"1302"}}` (string), and 火山's string codes.
fn extract_code_message(v: &Value) -> (Option<String>, Option<String>) {
    let src = v.get("error").unwrap_or(v);
    let code = src
        .get("code")
        .map(|c| match c {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty());
    let message = src.get("message").and_then(|m| m.as_str()).map(String::from);
    (code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zhipu_1214_is_a_param_error_not_rate_limit() {
        // Per the official code table, 1214 = "${field} 参数非法" (HTTP 400) — a client parameter
        // error, NOT a rate limit. It must not trip the breaker or trigger fallback.
        assert!(!is_rate_limit_error("zhipu", Some(400), r#"{"error":{"code":1214,"message":"参数非法"}}"#));
        assert!(!is_rate_limit_error("zhipu", Some(400), r#"{"error":{"code":"1214"}}"#));
    }

    #[test]
    fn zhipu_rate_limit_codes_are_rate_limit() {
        // The real Zhipu rate-limit/quota/balance codes (all arrive as HTTP 429 in practice; tested
        // at status 200 here to exercise the code arm, which is the streaming first-event safety net).
        // 1113=arrears, 1302=rate limit, 1305=model busy, 1309=plan expired, 1311=model not on plan,
        // 1316/1321=5h/monthly window exhausted.
        for code in [1113, 1302, 1305, 1308, 1309, 1311, 1313, 1316, 1321] {
            let body = format!(r#"{{"error":{{"code":{code}}}}}"#);
            assert!(
                is_rate_limit_error("zhipu", Some(200), &body),
                "zhipu code {code} should be a rate limit"
            );
        }
    }

    #[test]
    fn zhipu_sensitive_content_1301_is_not_rate_limit() {
        // 1301 = 敏感内容 (HTTP 400) — guards the lower bound of the 1308..=1321 range.
        assert!(!is_rate_limit_error("zhipu", Some(400), r#"{"error":{"code":1301,"message":"敏感"}}"#));
    }

    #[test]
    fn deepseek_402_is_rate_limit() {
        // DeepSeek returns 402 when balance is insufficient — must trigger fallback.
        assert!(is_rate_limit_error("deepseek", Some(402), ""));
        assert!(is_rate_limit_error(
            "deepseek",
            Some(402),
            r#"{"error":{"message":"Insufficient balance"}}"#
        ));
    }

    #[test]
    fn non_deepseek_402_is_not_rate_limit() {
        // The HTTP 402 *status* is only a rate-limit signal for DeepSeek; other vendors pass through.
        // A body that explicitly says "insufficient balance" still falls back via the keyword scan
        // (see `balance_keyword_is_rate_limit`), so the body here is intentionally neutral.
        assert!(!is_rate_limit_error("zhipu", Some(402), r#"{"error":{"code":"BadRequest"}}"#));
        assert!(!is_rate_limit_error("volcengine", Some(402), r#"{"error":{"code":"BadRequest"}}"#));
        assert!(!is_rate_limit_error("custom", Some(402), r#"{"error":{"code":"BadRequest"}}"#));
    }

    #[test]
    fn http_429_is_rate_limit_regardless_of_body() {
        assert!(is_rate_limit_error("zhipu", Some(429), ""));
        assert!(is_rate_limit_error("volcengine", Some(429), "anything"));
    }

    #[test]
    fn non_rate_limit_status_is_not_rate_limit() {
        assert!(!is_rate_limit_error("zhipu", Some(401), r#"{"error":"unauthorized"}"#));
        assert!(!is_rate_limit_error("zhipu", Some(400), r#"{"error":{"code":"bad_request"}}"#));
        assert!(!is_rate_limit_error("zhipu", Some(500), r#"{"error":{"code":"internal"}}"#));
    }

    #[test]
    fn volcengine_rate_limit_codes() {
        // Bare + namespaced: the real Ark codes are namespaced (RateLimitExceeded.EndpointRPMExceeded),
        // so an exact match would miss them — these are prefix-matched.
        assert!(is_rate_limit_error(
            "volcengine",
            Some(200),
            r#"{"error":{"code":"RateLimitExceeded","message":"x"}}"#
        ));
        assert!(is_rate_limit_error(
            "volcengine",
            Some(200),
            r#"{"error":{"code":"RateLimitExceeded.EndpointRPMExceeded"}}"#
        ));
        assert!(is_rate_limit_error("volcengine", Some(200), r#"{"error":{"code":"QuotaExceeded"}}"#));
        // Explicit 429 family members (ServerOverloaded / AccountRateLimitExceeded).
        assert!(is_rate_limit_error("volcengine", Some(429), r#"{"error":{"code":"ServerOverloaded"}}"#));
        assert!(is_rate_limit_error("volcengine", Some(429), r#"{"error":{"code":"AccountRateLimitExceeded"}}"#));
    }

    #[test]
    fn volcengine_overdue_balance_403_is_rate_limit() {
        // 403 account-overdue = balance exhausted (analogous to DeepSeek 402 / 智谱 1113). Non-429,
        // so only the code arm catches it — previously missed (passed through).
        assert!(is_rate_limit_error("volcengine", Some(403), r#"{"error":{"code":"AccountOverdueError","message":"overdue"}}"#));
        assert!(is_rate_limit_error("volcengine", Some(403), r#"{"error":{"code":"OperationDenied.ServiceOverdue"}}"#));
    }

    #[test]
    fn volcengine_plan_or_model_unavailable_is_rate_limit() {
        // Subscription expired / model not activated / model not on the coding plan → switch account
        // (no auto-reset time; rides the transient cooldown). These arrive at 400/404, not 429.
        assert!(is_rate_limit_error("volcengine", Some(400), r#"{"error":{"code":"InvalidSubscription"}}"#));
        assert!(is_rate_limit_error("volcengine", Some(404), r#"{"error":{"code":"ModelNotOpen"}}"#));
        assert!(is_rate_limit_error("volcengine", Some(404), r#"{"error":{"code":"UnsupportedModel"}}"#));
    }

    #[test]
    fn volcengine_param_and_auth_errors_are_not_rate_limit() {
        // Guards against over-matching after adding the new codes above.
        assert!(!is_rate_limit_error("volcengine", Some(400), r#"{"error":{"code":"InvalidParameter"}}"#));
        assert!(!is_rate_limit_error("volcengine", Some(401), r#"{"error":{"code":"AuthenticationError"}}"#));
        assert!(!is_rate_limit_error("volcengine", Some(404), r#"{"error":{"code":"InvalidEndpointOrModel.NotFound"}}"#));
    }

    #[test]
    fn keyword_in_message_is_rate_limit() {
        assert!(is_rate_limit_error(
            "zhipu",
            Some(200),
            r#"{"error":{"message":"API key usage limited"}}"#
        ));
        assert!(is_rate_limit_error(
            "volcengine",
            Some(200),
            r#"{"error":{"message":"throttled, too many requests"}}"#
        ));
    }

    #[test]
    fn balance_keyword_is_rate_limit() {
        // Safety net for undocumented balance-exhaustion shapes (the documented codes are caught by
        // the vendor arms above). "insufficient balance" / 欠费 → fallback.
        assert!(is_rate_limit_error("custom", Some(200), r#"{"error":{"message":"Insufficient balance"}}"#));
        assert!(is_rate_limit_error("custom", Some(200), r#"{"error":{"message":"账户已欠费，请充值"}}"#));
    }

    #[test]
    fn non_json_body_only_429_applies() {
        assert!(is_rate_limit_error("zhipu", Some(429), "<html>rate limited</html>"));
        assert!(!is_rate_limit_error("zhipu", Some(500), "<html>internal error</html>"));
    }

    #[test]
    fn unknown_provider_falls_back_to_keywords() {
        // No provider-specific code match, but keyword still catches it.
        assert!(is_rate_limit_error("custom", Some(200), r#"{"error":{"message":"quota exhausted"}}"#));
        assert!(!is_rate_limit_error("custom", Some(200), r#"{"error":{"message":"bad input"}}"#));
    }

    #[test]
    fn sse_event_rate_limit() {
        assert!(sse_event_is_rate_limit("zhipu", r#"{"error":{"code":1302}}"#));
        assert!(!sse_event_is_rate_limit("zhipu", r#"{"choices":[{"delta":{"content":"hi"}}]}"#));
    }

    #[test]
    fn benign_code_not_misclassified() {
        // A 200 with a non-rate-limit code + benign message must not trip.
        assert!(!is_rate_limit_error("zhipu", Some(200), r#"{"error":{"code":1300,"message":"ok"}}"#));
    }

    #[test]
    fn rate_limit_classifies_by_vendor_not_id() {
        // The vendor slug drives the provider-specific CODE arm. Status is 200 (not 429) so the
        // generic status short-circuit does NOT fire - this exercises the vendor match itself.
        // The body matches via volcengine's code path; its message "x" has no keyword, so an
        // opaque id that misses the code arm falls through to a false (no keyword hit). This is
        // the clean contrast: same body, vendor -> true, opaque id -> false. Guards the REAL
        // production slugs ("volcengine-coding", "volcengine-agent") that `snap.vendor` carries.
        let body = r#"{"error":{"code":"RateLimitExceeded","message":"x"}}"#;
        assert!(is_rate_limit_error("volcengine-coding", Some(200), body));
        assert!(is_rate_limit_error("volcengine-agent", Some(200), body));
        assert!(is_rate_limit_error("volcengine", Some(200), body)); // bare slug still matches
        assert!(!is_rate_limit_error("prov_abc", Some(200), body));
    }
}
