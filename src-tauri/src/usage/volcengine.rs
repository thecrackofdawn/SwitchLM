//! 火山方舟 (Volcengine Ark) Agent Plan / Coding Plan usage adapter.
//!
//! 与智谱/Kimi 等数据面 Bearer 余额接口不同，火山用量接口是**控制面 OpenAPI**：统一网关
//! `open.volcengineapi.com`（**不是**数据面推理域名 `ark.cn-beijing.volces.com`），形如
//! `POST https://open.volcengineapi.com/?Action=...&Version=2024-01-01&Region=cn-beijing`，
//! **强制火山引擎签名 V4（AK/SK）**--实测复用推理 Bearer Key 会被网关以
//! `400 InvalidAuthorization` 拒绝。因此用户需在 `usage_creds` 里另填火山账号的
//! AccessKey ID + Secret（与推理 Key 是两套凭据）。
//!
//! 按 vendor 选 action：`volcengine-agent`→`GetAFPUsage`（Agent Plan，回绝对额度
//! Quota/Used）、`volcengine-coding`→`GetCodingPlanUsage`（Coding Plan，回百分比）；
//! 旧版裸 `volcengine` 无法区分套餐，回落自动探测（先 Agent 后 Coding）。
//!
//! 套餐档位（Agent 的 Large / Coding 的 Pro/Lite）+ 订阅元信息一律走 `GetPersonalPlan`
//! （POST body `Plan=AgentPlan`/`CodingPlan`）取 `PlanType`/`StartTime`/`AutoRenew`（两个 plan
//! 对齐，不再读 `GetAFPUsage.PlanType`）；用量接口响应本身不含档位。plan 标签只显示档位
//! （如 "Large"/"Pro"），不带 "Coding Plan"/"Agent Plan" 前缀——厂商 + plan 类型已由 provider 标签体现。
//!
//! 签名按火山引擎签名 V4 规范实现（对照官方 volc-openapi-demos/signature），细节见
//! 下方签名块注释。

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::Value;

use super::tiers::{extract_reset_time, parse_f64, QuotaTier, TIER_FIVE_HOUR, TIER_MONTHLY, TIER_WEEKLY_LIMIT};
use super::{PlanInfo, UsageError, UsageProvider, UsageSnapshot};
use crate::config::UsageCreds;

/// 控制面 OpenAPI 统一网关（区别于数据面推理域名 ark.cn-beijing.volces.com）。
const VOLCENGINE_OPENAPI_HOST: &str = "open.volcengineapi.com";
const VOLCENGINE_API_VERSION: &str = "2024-01-01";
/// ark 控制面 OpenAPI 的默认 Region（Agent/Coding Plan 目前在 cn-beijing）。
const VOLCENGINE_DEFAULT_REGION: &str = "cn-beijing";
const VOLCENGINE_SERVICE: &str = "ark";
const VOLCENGINE_CONTENT_TYPE: &str = "application/json; charset=utf-8";
/// canonical headers 与 SignedHeaders 用**固定顺序**
/// `host;x-date;x-content-sha256;content-type`（火山特有，**不按字母序**）。
const VOLCENGINE_SIGNED_HEADERS: &str = "host;x-date;x-content-sha256;content-type";

/// 鉴权失败时的引导文案，附加在错误后。
const VOLCENGINE_AKSK_HINT: &str = "Check the AccessKey ID / Secret are correct and the \
account has Ark usage-query (OpenAPI) permission (these differ from the inference API key).";

// ── 火山引擎签名 V4（AK/SK）─────────────────────────────────
//
// 按火山引擎签名 V4 规范实现（对照官方 volc-openapi-demos/signature/java/Sign.java），
// 是 AWS SigV4 的火山变体。与标准 SigV4 有两处必须照规范的差异，照搬通用 SigV4 会签名失败：
//   1. canonical headers 与 SignedHeaders 用固定顺序（不按字母序，见上常量）；
//   2. algorithm 串 `HMAC-SHA256`（无 `AWS4` 前缀）、credential scope 结尾 `request`
//      （非 `aws4_request`）、签名密钥 `kDate=HMAC(SK, date)`（SK 不加 `AWS4` 前缀）。
// canonical query 仍按 key 字母序（与标准 SigV4 一致）；service=`ark`、POST、空 body。

fn volc_hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn volc_sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

/// RFC3986 unreserved 之外全部按 `%XX` 编码（用于 canonical query string）。
fn volc_uri_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// 构造按 key 字母序排序、逐段 URL 编码的 canonical query string。
/// 同一份字符串既用于签名也用于实际请求 URL，保证两者完全一致。
fn volcengine_canonical_query(action: &str, region: &str) -> String {
    let mut pairs = [
        ("Action", action),
        ("Region", region),
        ("Version", VOLCENGINE_API_VERSION),
    ];
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", volc_uri_encode(k), volc_uri_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 派生火山签名 V4 的签名密钥：`HMAC(SK, short_date) -> HMAC(., region) -> HMAC(., service)
/// -> HMAC(., "request")`。与 AWS SigV4 的区别：SK 不加 `AWS4` 前缀，终止串是 `request`
/// （非 `aws4_request`）。
fn derive_signing_key(secret: &str, short_date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = volc_hmac_sha256(secret.as_bytes(), short_date.as_bytes());
    let k_region = volc_hmac_sha256(&k_date, region.as_bytes());
    let k_service = volc_hmac_sha256(&k_region, service.as_bytes());
    volc_hmac_sha256(&k_service, b"request")
}

/// 生成火山引擎签名 V4 的鉴权头，返回 `(Authorization, X-Date, X-Content-Sha256)`，
/// 三者都要塞进请求头；`canonical_query` 必须与实际请求 URL 的 query 完全一致。
/// `now` 作参数传入便于写确定性单测。
fn volcengine_sign(
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    canonical_query: &str,
    body: &[u8],
    now: chrono::DateTime<chrono::Utc>,
) -> (String, String, String) {
    let x_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let x_content_sha256 = volc_sha256_hex(body);

    // 固定顺序 canonical headers（火山特有，**不排序**）。
    let canonical_headers = format!(
        "host:{VOLCENGINE_OPENAPI_HOST}\nx-date:{x_date}\nx-content-sha256:{x_content_sha256}\ncontent-type:{VOLCENGINE_CONTENT_TYPE}\n"
    );
    let canonical_request = format!(
        "POST\n/\n{canonical_query}\n{canonical_headers}\n{VOLCENGINE_SIGNED_HEADERS}\n{x_content_sha256}"
    );

    let credential_scope = format!("{short_date}/{region}/{VOLCENGINE_SERVICE}/request");
    let string_to_sign = format!(
        "HMAC-SHA256\n{x_date}\n{credential_scope}\n{}",
        volc_sha256_hex(canonical_request.as_bytes())
    );

    // 派生签名密钥（见 `derive_signing_key`：SK 不加 AWS4 前缀，终止串 `request`）。
    let k_signing = derive_signing_key(secret_access_key, &short_date, region, VOLCENGINE_SERVICE);
    let signature: String = volc_hmac_sha256(&k_signing, string_to_sign.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let authorization = format!(
        "HMAC-SHA256 Credential={access_key_id}/{credential_scope}, SignedHeaders={VOLCENGINE_SIGNED_HEADERS}, Signature={signature}"
    );
    (authorization, x_date, x_content_sha256)
}

// ── Region / 错误识别 ───────────────────────────────────────

/// 从数据面 base_url 提取控制面 OpenAPI 所需的 Region（如
/// `ark.cn-beijing.volces.com` -> `cn-beijing`）；无法识别时回落 cn-beijing。
/// 控制面 Host 是固定网关（`VOLCENGINE_OPENAPI_HOST`），不随 base_url 变化。
fn volcengine_region(base_url: &str) -> String {
    let host = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or("");
    host.split('.')
        .find(|p| p.starts_with("cn-") || p.starts_with("ap-"))
        .map(|p| p.to_string())
        .unwrap_or_else(|| VOLCENGINE_DEFAULT_REGION.to_string())
}

/// 判断 OpenAPI 错误码是否属于鉴权类（需要硬停并提示换 AK/SK）。
fn volcengine_is_auth_error_code(code: &str) -> bool {
    let c = code.to_lowercase();
    c.contains("auth")
        || c.contains("signature")
        || c.contains("accessdenied")
        || c.contains("denied")
        || c.contains("unauthorized")
        || c.contains("forbidden")
        || c.contains("credential")
        || c.contains("token")
}

/// 提取火山 OpenAPI 响应里的 `ResponseMetadata.Error`（或顶层 `Error`）。
fn volcengine_response_error(body: &Value) -> Option<(String, String)> {
    let err = body
        .get("ResponseMetadata")
        .and_then(|m| m.get("Error"))
        .or_else(|| body.get("Error"))?;
    let code = err
        .get("Code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let msg = err
        .get("Message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if code.is_empty() && msg.is_empty() {
        None
    } else {
        Some((code, msg))
    }
}

// ── OpenAPI 调用 ─────────────────────────────────────────────

/// 单次 OpenAPI 调用的归类结果。
enum VolcCall {
    /// 2xx 且 JSON 可解析、无 OpenAPI 级错误（业务 Result 仍可能为空=未订阅）。
    Body(Value),
    /// 硬鉴权失败（HTTP 401/403 或 AccessDenied/Signature 等错误码）--两个 plan
    /// 共用凭据，命中即停。
    Auth(String),
    /// 非鉴权 HTTP 错误 / 响应体非法 JSON--记录后可继续尝试另一个 plan。
    Soft(String),
    /// 瞬时传输失败（网络/超时/读体中断）--同 host 的另一个 plan 大概率同样
    /// 失败，调用方应立即以 `Err` 传播。
    Transient(String),
}

async fn volcengine_openapi_call(
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
    action: &str,
    body: &[u8],
) -> VolcCall {
    // canonical query 同时用于签名与实际 URL，确保两者逐字一致（否则签名不匹配）。
    let canonical_query = volcengine_canonical_query(action, region);
    let url = format!("https://{VOLCENGINE_OPENAPI_HOST}/?{canonical_query}");
    let (authorization, x_date, x_content_sha256) = volcengine_sign(
        access_key_id,
        secret_access_key,
        region,
        &canonical_query,
        body,
        chrono::Utc::now(),
    );

    let resp = reqwest::Client::new()
        .post(&url)
        .header("X-Date", x_date)
        .header("X-Content-Sha256", x_content_sha256)
        .header("Content-Type", VOLCENGINE_CONTENT_TYPE)
        .header("Authorization", authorization)
        .body(body.to_vec())
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return VolcCall::Transient(format!("Network error: {e}")),
    };

    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return VolcCall::Auth(format!(
            "Authentication failed (HTTP {status}). {VOLCENGINE_AKSK_HINT}"
        ));
    }
    if !status.is_success() {
        // 火山 OpenAPI 网关对签名/凭据类错误常返 4xx（多为 HTTP 400）并携带与 200
        // 路径相同的 ResponseMetadata.Error 信封，而非 401/403。这里也解析信封，让
        // Bearer 被拒时仍能给出 AK/SK 引导并标记凭据失效，而不是当成普通 API 错误。
        let raw = resp.text().await.unwrap_or_default();
        if let Ok(body) = serde_json::from_str::<Value>(&raw) {
            if let Some((code, msg)) = volcengine_response_error(&body) {
                if volcengine_is_auth_error_code(&code) {
                    return VolcCall::Auth(format!(
                        "Authentication failed (HTTP {status}, {code}): {msg}. {VOLCENGINE_AKSK_HINT}"
                    ));
                }
                return VolcCall::Soft(format!("API error (HTTP {status}, {code}): {msg}"));
            }
        }
        return VolcCall::Soft(format!("API error (HTTP {status}): {raw}"));
    }

    // 先 bytes() 再解析：读体失败是瞬时（Transient），解析失败是确定性（Soft）。
    // reqwest 的 json() 把读体错误也包成 decode，无法区分。
    let raw = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return VolcCall::Transient(format!("Failed to read response: {e}")),
    };
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return VolcCall::Soft(format!("Failed to parse response: {e}")),
    };

    // 火山 OpenAPI 业务错误常以 200 + ResponseMetadata.Error 返回。
    if let Some((code, msg)) = volcengine_response_error(&body) {
        if volcengine_is_auth_error_code(&code) {
            return VolcCall::Auth(format!(
                "Authentication failed ({code}): {msg}. {VOLCENGINE_AKSK_HINT}"
            ));
        }
        return VolcCall::Soft(format!("API error ({code}): {msg}"));
    }

    VolcCall::Body(body)
}

// ── 套餐模型列表（ListArkAgentPlanModel / ListArkCodingPlanModel）──────────
//
// 与用量查询同一套控制面 OpenAPI（同 host open.volcengineapi.com、同 AK/SK Sig V4、
// service=ark）。Agent Plan 与 Coding Plan 是两个独立套餐（添加服务商时分开配置），
// 故调用方只传一个 action，本函数不做合并/去重。

/// 套餐模型列表查询的错误，由 command 层映射成 `String`。
#[derive(Debug, thiserror::Error)]
pub enum PlanModelError {
    /// 鉴权失败——detail 已含 AK/SK 引导（来自 `volcengine_openapi_call`）。
    #[error("{0}")]
    Auth(String),
    #[error("网络错误：{0}")]
    Network(String),
    #[error("获取模型列表失败：{0}")]
    Other(String),
}

/// 查询火山方舟某个套餐支持的模型列表（`ListArkAgentPlanModel` /
/// `ListArkCodingPlanModel`）。复用用量适配器的 Sig V4 签名与 host。
/// `action` 选定套餐——调用方只传一个（Agent / Coding 独立，不合并）。
pub async fn list_plan_models(
    base_url: &str,
    access_key_id: &str,
    secret_access_key: &str,
    action: &str,
) -> Result<Vec<String>, PlanModelError> {
    let region = volcengine_region(base_url);
    match volcengine_openapi_call(&region, access_key_id, secret_access_key, action, b"").await {
        VolcCall::Auth(detail) => Err(PlanModelError::Auth(detail)),
        VolcCall::Transient(detail) => Err(PlanModelError::Network(detail)),
        VolcCall::Soft(detail) => Err(PlanModelError::Other(detail)),
        VolcCall::Body(body) => Ok(parse_plan_models(&body)),
    }
}

/// 解析 `ListArk*PlanModel` 响应：`Datas[].ModelID`，通常在 `Result` 下。
/// 对包裹层防御（`Result` 可能缺失、`Datas` 可能在顶层——官方返回示例是残缺渲染），
/// 对逐元素形状防御（跳过缺/空 `ModelID` 的条目）。
fn parse_plan_models(body: &Value) -> Vec<String> {
    let result = body.get("Result").unwrap_or(body);
    let datas = result
        .get("Datas")
        .and_then(|v| v.as_array())
        .or_else(|| body.get("Datas").and_then(|v| v.as_array()));
    let Some(datas) = datas else {
        return Vec::new();
    };
    let mut ids = Vec::with_capacity(datas.len());
    for item in datas {
        if let Some(id) = item.get("ModelID").and_then(|v| v.as_str()) {
            let id = id.trim();
            if !id.is_empty() {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

// ── 响应解析 ─────────────────────────────────────────────────

/// 解析 `GetAFPUsage` 的 `Result` 为 tier 列表。
///
/// 展示 5h / 周 / 月三个窗口（与控制台一致）；`AFPDaily` 被官方控制台隐藏
/// （其 Quota 常高于周上限，属历史默认值而非强制限额），故跳过。
/// `Quota`/`Used` 是绝对 AFP 值，已用百分比 = Used/Quota×100；`Quota<=0` 视为
/// 该窗口未订阅/未启用，跳过--也用于把"已鉴权但无 Agent Plan"识别为空结果，
/// 从而回落到 Coding Plan 探测。
fn parse_afp_tiers(result: &Value) -> Vec<QuotaTier> {
    let mut tiers = Vec::new();
    for (key, name) in [
        ("AFPFiveHour", TIER_FIVE_HOUR),
        ("AFPWeekly", TIER_WEEKLY_LIMIT),
        ("AFPMonthly", TIER_MONTHLY),
    ] {
        let Some(win) = result.get(key) else { continue };
        let quota = win.get("Quota").and_then(parse_f64).unwrap_or(0.0);
        if quota <= 0.0 {
            continue;
        }
        let used = win.get("Used").and_then(parse_f64).unwrap_or(0.0);
        // 已用百分比；不做范围裁剪，与 parse_zhipu_token_tiers 的约定一致
        //（下游渲染层负责显示策略）。
        let utilization = used / quota * 100.0;
        let resets_at = win.get("ResetTime").and_then(extract_reset_time);
        tiers.push(QuotaTier {
            name: name.to_string(),
            utilization,
            resets_at,
            used_value_usd: None,
            max_value_usd: None,
        });
    }
    tiers
}

/// 把 `GetCodingPlanUsage` 的 window 标签归一到 tier 名。
fn volcengine_coding_window(label: &str) -> Option<&'static str> {
    match label.to_lowercase().as_str() {
        "session" | "5h" | "fivehour" | "five_hour" | "rolling_5h" => Some(TIER_FIVE_HOUR),
        "weekly" | "week" | "7d" => Some(TIER_WEEKLY_LIMIT),
        "monthly" | "month" => Some(TIER_MONTHLY),
        _ => None,
    }
}

/// 解析 `GetCodingPlanUsage` 的 `Result` 为 tier 列表（防御式）。
///
/// 该接口官方文档未给出逐字段规格，依据官方 ark-cli 描述：回 session/weekly/
/// monthly 窗口、**只给百分比**（已用）、重置时间是秒级。这里宽松匹配
/// `QuotaUsage`/`Usages`/`Details` 数组及多种字段名，命中即用、未命中跳过。
fn parse_coding_plan_tiers(result: &Value) -> Vec<QuotaTier> {
    let mut tiers = Vec::new();
    let arr = result
        .get("QuotaUsage")
        .and_then(|v| v.as_array())
        .or_else(|| result.get("Usages").and_then(|v| v.as_array()))
        .or_else(|| result.get("Details").and_then(|v| v.as_array()));
    let Some(arr) = arr else { return tiers };

    for item in arr {
        // 真实字段是 `Level`（实测 2026-06-21：session/weekly/monthly）；其余作防御式 fallback。
        let label = item
            .get("Level")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("Type").and_then(|v| v.as_str()))
            .or_else(|| item.get("Period").and_then(|v| v.as_str()))
            .or_else(|| item.get("Label").and_then(|v| v.as_str()))
            .or_else(|| item.get("Window").and_then(|v| v.as_str()))
            .unwrap_or("");
        let Some(name) = volcengine_coding_window(label) else {
            continue;
        };
        let utilization = item
            .get("Percent")
            .and_then(parse_f64)
            .or_else(|| item.get("UsedPercent").and_then(parse_f64))
            .or_else(|| item.get("UsagePercent").and_then(parse_f64))
            .unwrap_or(0.0);
        // 兼容秒/毫秒/字符串（extract_reset_time 内部已区分秒与毫秒）。
        let resets_at = item
            .get("ResetTime")
            .or_else(|| item.get("ResetTimestamp"))
            .and_then(extract_reset_time);
        tiers.push(QuotaTier {
            name: name.to_string(),
            utilization,
            resets_at,
            used_value_usd: None,
            max_value_usd: None,
        });
    }
    tiers
}

// ── 套餐类型路由（vendor → 用量 action）─────────────────────
//
// 与模型发现 `volcengine_plan_action` 对齐：vendor 已经编码了套餐类型，用量查询必须
// 按 vendor 选 action，不能再无脑先探 Agent Plan——否则当账号同时持有 Agent 套餐时，
// Coding 套餐的 provider 会先命中 `GetAFPUsage` 而显示成 Agent 额度。

/// 火山方舟用量查询的套餐归属，由 `vendor` slug 推导。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolcPlanKind {
    /// `volcengine-agent` —— Agent Plan（AFP），调 `GetAFPUsage`。
    Agent,
    /// `volcengine-coding` —— Coding Plan，调 `GetCodingPlanUsage`。
    Coding,
    /// 裸 `volcengine`（旧版，未区分套餐）—— 自动探测：先 Agent 后 Coding。
    Auto,
}

impl VolcPlanKind {
    /// 把 vendor slug 映射到套餐类型；非火山 vendor 返回 `None`。
    pub fn from_vendor(vendor: &str) -> Option<Self> {
        match vendor {
            "volcengine-agent" => Some(VolcPlanKind::Agent),
            "volcengine-coding" => Some(VolcPlanKind::Coding),
            "volcengine" => Some(VolcPlanKind::Auto),
            _ => None,
        }
    }
}

/// 该套餐类型应依次尝试的 OpenAPI action 列表。Agent/Coding 各只查自己的 action，
/// 避免账号同时持有两种套餐时串台（这是被报告的 bug：Coding provider 拿到 Agent 额度）。
fn volcengine_usage_actions(plan: VolcPlanKind) -> &'static [&'static str] {
    match plan {
        VolcPlanKind::Agent => &["GetAFPUsage"],
        VolcPlanKind::Coding => &["GetCodingPlanUsage"],
        VolcPlanKind::Auto => &["GetAFPUsage", "GetCodingPlanUsage"],
    }
}

// ── 编排：按套餐类型选 action（Agent / Coding / 自动探测）────────

/// `query_volcengine` 的归类结果，映射到 `UsageError` 由 trait 层消费。
enum VolcOutcome {
    /// 成功解析出至少一个 tier。
    Tiers {
        tiers: Vec<QuotaTier>,
        plan: Option<String>,
        /// 订阅元信息（档位由 GetPersonalPlan 取，供 tag tooltip）；查询失败为 `None`。
        plan_info: Option<PlanInfo>,
    },
    /// 硬鉴权失败（两个 plan 共用凭据，命中即停）。
    Auth(String),
    /// 瞬时传输失败（网络/超时/读体中断）。
    Transient(String),
    /// 确定性非鉴权失败（4xx 业务错误 / 非法 JSON / 未订阅）。
    Soft(String),
}

// ── 套餐档位 + 订阅元信息（GetPersonalPlan -> PlanType / StartTime / AutoRenew）──
//
// Coding Plan 与 Agent Plan 的档位都不在用量接口响应里（GetCodingPlanUsage 只有
// Status + QuotaUsage[]；GetAFPUsage 虽带 PlanType，但为统一来源改为一律走个人套餐接口）。
// GetPersonalPlan（POST body Plan=CodingPlan/AgentPlan）的 Result 含 PlanType（档位：
// CodingPlan: Lite/Pro；AgentPlan: Small/Medium/Large/Max）、StartTime、AutoRenew 等，
// 档位拼进卡片头标签，StartTime/AutoRenew 作为 tag 的 tooltip。与用量查询同一套控制面
// OpenAPI（同 host、同 AK/SK Sig V4、service=ark），但 Plan 是业务参数，走 POST JSON body
// （实测放 query 会被网关回 MissingParameter.Plan），并参与 x-content-sha256 签名。

/// `GetPersonalPlan` 解析结果：套餐档位 + 订阅元信息（供 tag tooltip）。
#[derive(Clone, Debug, PartialEq)]
struct PersonalPlan {
    /// 套餐档位（如 "Pro"/"Large"），直接作为 plan 标签（只显示档位，不带产品前缀）。
    plan_type: String,
    /// 首次生效时间（ISO 8601）。
    start_time: Option<String>,
    /// 当前到期时间（ISO 8601）。
    end_time: Option<String>,
    /// 是否已开启自动续费。
    auto_renew: Option<bool>,
}

/// 解析 `GetPersonalPlan` 响应：`PlanType`（档位）+ `StartTime` + `EndTime` + `AutoRenew`。对
/// 包裹层防御（字段可能在 `Result` 下或顶层）；`PlanType` 空/缺失返回 `None`（其余字段缺失不影响）。
fn parse_personal_plan_type(body: &Value) -> Option<PersonalPlan> {
    let result = body.get("Result").unwrap_or(body);
    let plan_type = result
        .get("PlanType")
        .or_else(|| body.get("PlanType"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)?;
    let start_time = result
        .get("StartTime")
        .or_else(|| body.get("StartTime"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let end_time = result
        .get("EndTime")
        .or_else(|| body.get("EndTime"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let auto_renew = result
        .get("AutoRenew")
        .or_else(|| body.get("AutoRenew"))
        .and_then(|v| v.as_bool());
    Some(PersonalPlan { plan_type, start_time, end_time, auto_renew })
}

/// 构造 `GetPersonalPlan` 的 POST JSON body：`{"Plan":"<plan>"}`。`Plan` 是业务参数，走
/// body 而非 query（实测放 query 被网关回 `MissingParameter.Plan`）。
fn personal_plan_request_body(plan: &str) -> String {
    serde_json::json!({ "Plan": plan }).to_string()
}

/// 调 `GetPersonalPlan`（POST body `Plan={plan}`，如 "CodingPlan"/"AgentPlan"）取套餐档位
/// （`PlanType`）+ 订阅元信息（`StartTime`/`AutoRenew`）。用量接口响应不含档位，需另查
/// 此接口。best-effort：鉴权/网络/业务错误或无 `PlanType` 都返回 `None`，调用方据此省略
/// plan 标签（`None`，不带 tooltip），不影响用量展示（与用量查询同一套 AK/SK Sig V4）。
async fn volcengine_personal_plan_type(
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
    plan: &str,
) -> Option<PersonalPlan> {
    let request_body = personal_plan_request_body(plan);
    match volcengine_openapi_call(
        region,
        access_key_id,
        secret_access_key,
        "GetPersonalPlan",
        request_body.as_bytes(),
    )
    .await
    {
        VolcCall::Body(body) => match parse_personal_plan_type(&body) {
            Some(p) => Some(p),
            None => {
                // 2xx + 无 Error 信封但取不到 PlanType：截断原始响应，便于核对真实字段名/包裹层。
                let raw: String = body.to_string().chars().take(700).collect();
                tracing::warn!(
                    target: "switchlm::usage",
                    action = "GetPersonalPlan",
                    plan = %plan,
                    raw = %raw,
                    "volcengine plan tier (PlanType) not found in GetPersonalPlan response; plan label will be omitted",
                );
                None
            }
        },
        // Auth / Soft / Transient 各自的 detail 即失败原因；共用一条 warn，原因见 error 字段。
        VolcCall::Auth(detail) | VolcCall::Soft(detail) | VolcCall::Transient(detail) => {
            tracing::warn!(
                target: "switchlm::usage",
                action = "GetPersonalPlan",
                plan = %plan,
                error = %detail,
                "volcengine plan tier query failed (GetPersonalPlan); plan label will be omitted",
            );
            None
        }
    }
}

async fn query_volcengine(
    base_url: &str,
    access_key_id: &str,
    secret_access_key: &str,
    plan: VolcPlanKind,
) -> VolcOutcome {
    let region = volcengine_region(base_url);
    let mut soft_errors: Vec<String> = Vec::new();
    // 2xx + 无 Error 信封但解析不出额度时，截断原始响应用于诊断（区分"真没订阅"
    // 与"字段名/包裹层猜错"）。签名若不通会走 Auth/Soft 分支，到不了这里。
    let mut empty_responses: Vec<String> = Vec::new();
    let summarize = |action: &str, body: &Value| -> String {
        let raw: String = body.to_string().chars().take(700).collect();
        format!("{action}={raw}")
    };

    // 按套餐类型选 action（Agent→GetAFPUsage / Coding→GetCodingPlanUsage / Auto→两者）。
    // 不再无脑先探 Agent Plan：避免 Coding 套餐 provider 在账号同时持有 Agent 套餐时
    // 命中 GetAFPUsage 而显示成 Agent 额度。
    for &action in volcengine_usage_actions(plan) {
        match volcengine_openapi_call(&region, access_key_id, secret_access_key, action, b"").await {
            VolcCall::Auth(detail) => return VolcOutcome::Auth(detail),
            VolcCall::Transient(detail) => return VolcOutcome::Transient(detail),
            VolcCall::Soft(detail) => soft_errors.push(format!("{action}: {detail}")),
            VolcCall::Body(body) => {
                let result = body.get("Result").unwrap_or(&body);
                let (tiers, plan_label, plan_info) = match action {
                    "GetAFPUsage" => {
                        let tiers = parse_afp_tiers(result);
                        // 档位 + 订阅元信息一律走 GetPersonalPlan（Plan=AgentPlan），与 Coding
                        // Plan 对齐（不再读 GetAFPUsage.PlanType）。仅在已解析出用量明细时查询，
                        // 避免未订阅时浪费一次调用。plan 只显示档位（provider 已标 volcengine-agent）；
                        // 查询失败或无档位 → 无标签（与其他 plan 厂商一致），不影响用量展示。
                        let (label, plan_info) = if tiers.is_empty() {
                            (None, None)
                        } else {
                            match volcengine_personal_plan_type(
                                &region,
                                access_key_id,
                                secret_access_key,
                                "AgentPlan",
                            )
                            .await
                            {
                                Some(p) => (
                                    Some(p.plan_type),
                                    Some(PlanInfo {
                                        start_time: p.start_time,
                                        end_time: p.end_time,
                                        auto_renew: p.auto_renew,
                                    }),
                                ),
                                None => (None, None),
                            }
                        };
                        (tiers, label, plan_info)
                    }
                    "GetCodingPlanUsage" => {
                        let tiers = parse_coding_plan_tiers(result);
                        // 套餐档位不在本响应里（只有 Status + QuotaUsage[]，无 PlanType）。另调
                        // GetPersonalPlan（Plan=CodingPlan）取档位 + 订阅元信息。plan 只显示档位
                        // （provider 已标 volcengine-coding）；查询失败或无档位 → 无标签（与其他
                        // plan 厂商一致），不影响用量展示。
                        let (label, plan_info) = if tiers.is_empty() {
                            (None, None)
                        } else {
                            match volcengine_personal_plan_type(
                                &region,
                                access_key_id,
                                secret_access_key,
                                "CodingPlan",
                            )
                            .await
                            {
                                Some(p) => (
                                    Some(p.plan_type),
                                    Some(PlanInfo {
                                        start_time: p.start_time,
                                        end_time: p.end_time,
                                        auto_renew: p.auto_renew,
                                    }),
                                ),
                                None => (None, None),
                            }
                        };
                        (tiers, label, plan_info)
                    }
                    _ => (Vec::new(), None, None),
                };
                if !tiers.is_empty() {
                    return VolcOutcome::Tiers {
                        tiers,
                        plan: plan_label,
                        plan_info,
                    };
                }
                empty_responses.push(summarize(action, &body));
            }
        }
    }

    if !soft_errors.is_empty() {
        VolcOutcome::Soft(soft_errors.join("; "))
    } else if !empty_responses.is_empty() {
        // 签名已通过、请求到达业务层，但响应里没有可解析的额度。带上原始响应，
        // 便于核对真实字段名/包裹层，或确认确实未订阅。
        VolcOutcome::Soft(format!(
            "No active subscription found (signature OK). Raw: {}",
            empty_responses.join(" || ")
        ))
    } else {
        VolcOutcome::Soft(
            "No active Agent Plan or Coding Plan subscription found for this credential"
                .to_string(),
        )
    }
}

// ── UsageProvider 实现 ──────────────────────────────────────

pub struct VolcengineUsageProvider {
    /// 该 provider 的套餐归属（由 vendor 推导），决定用量查询调哪个 action。
    pub plan: VolcPlanKind,
}

impl Default for VolcengineUsageProvider {
    /// 默认 `Auto`（自动探测），供单测与未指定套餐的旧路径使用。
    fn default() -> Self {
        Self {
            plan: VolcPlanKind::Auto,
        }
    }
}

#[async_trait]
impl UsageProvider for VolcengineUsageProvider {
    async fn query(
        &self,
        _api_key: Option<&str>,
        usage_creds: Option<&UsageCreds>,
        base_url: &str,
    ) -> Result<UsageSnapshot, UsageError> {
        let creds = usage_creds.ok_or(UsageError::NotConfigured)?;
        let ak = creds.access_key_id.as_deref().map(str::trim).unwrap_or("");
        let sk = creds.secret_access_key.as_deref().map(str::trim).unwrap_or("");
        if ak.is_empty() || sk.is_empty() {
            return Err(UsageError::NotConfigured);
        }

        match query_volcengine(base_url, ak, sk, self.plan).await {
            VolcOutcome::Tiers { tiers, plan, plan_info } => {
                let mut snap = super::snapshot_from_tiers(&tiers, &plan);
                snap.plan_info = plan_info;
                Ok(snap)
            }
            VolcOutcome::Auth(detail) => Err(UsageError::AuthFailed(detail)),
            VolcOutcome::Transient(detail) => Err(UsageError::Network(detail)),
            VolcOutcome::Soft(detail) => Err(UsageError::CodingPlan(detail)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn afp_three_windows_from_official_example() {
        // 官方文档 GetAFPUsage 返回示例（逐字）：5h 25% / weekly 30% / monthly
        // 42.525%；AFPDaily 被控制台隐藏，应跳过。
        let result = json!({
            "PlanType": "Large",
            "AFPFiveHour": { "Quota": 50.0,   "Used": 12.5,  "ResetTime": 1778806800000_i64 },
            "AFPDaily":    { "Quota": 100.0,  "Used": 22.5,  "ResetTime": 1778803200000_i64 },
            "AFPWeekly":   { "Quota": 500.0,  "Used": 150.0, "ResetTime": 1779062400000_i64 },
            "AFPMonthly":  { "Quota": 2000.0, "Used": 850.5, "ResetTime": 1780531200000_i64 }
        });
        let tiers = parse_afp_tiers(&result);
        assert_eq!(tiers.len(), 3, "daily 应被跳过，只剩 5h/周/月");
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert!((tiers[0].utilization - 25.0).abs() < 1e-9);
        assert!(tiers[0].resets_at.is_some());
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert!((tiers[1].utilization - 30.0).abs() < 1e-9);
        assert_eq!(tiers[2].name, TIER_MONTHLY);
        assert!((tiers[2].utilization - 42.525).abs() < 1e-9);
    }

    #[test]
    fn afp_zero_quota_windows_treated_as_unbound() {
        // 已鉴权但无 Agent Plan：窗口 Quota=0 -> 空结果，调用方据此回落 Coding Plan。
        let result = json!({
            "AFPFiveHour": { "Quota": 0.0, "Used": 0.0 },
            "AFPWeekly":   { "Quota": 0.0, "Used": 0.0 },
            "AFPMonthly":  { "Quota": 0.0, "Used": 0.0 }
        });
        assert!(parse_afp_tiers(&result).is_empty());
    }

    #[test]
    fn coding_plan_real_response_levels() {
        // 真实 GetCodingPlanUsage 响应（用户实测 2026-06-21）：字段名是 `Level`（非 `Type`），
        // 仅百分比，秒级 ResetTimestamp；session 无活跃窗口回 -1 -> 无重置时间。
        let result = json!({
            "Status": "Running",
            "QuotaUsage": [
                { "Level": "session", "Percent": 0.0,      "ResetTimestamp": -1_i64 },
                { "Level": "weekly",  "Percent": 1.672568, "ResetTimestamp": 1782057600_i64 },
                { "Level": "monthly", "Percent": 0.836284, "ResetTimestamp": 1784303999_i64 }
            ]
        });
        let tiers = parse_coding_plan_tiers(&result);
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert!((tiers[0].utilization - 0.0).abs() < 1e-9);
        assert!(tiers[0].resets_at.is_none(), "session ResetTimestamp=-1 应无重置时间");
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert!((tiers[1].utilization - 1.672568).abs() < 1e-6);
        assert!(tiers[1].resets_at.is_some());
        assert_eq!(tiers[2].name, TIER_MONTHLY);
    }

    #[test]
    fn personal_plan_type_from_real_shape() {
        // GetPersonalPlan（Plan=CodingPlan）响应：Result.PlanType 即套餐档位
        //（CodingPlan: Lite/Pro），另含 StartTime/AutoRenew 供 tag tooltip。
        let body = json!({
            "ResponseMetadata": { "Action": "GetPersonalPlan", "Service": "ark" },
            "Result": {
                "PlanType": "Pro",
                "Status": "Running",
                "StartTime": "2026-07-30T00:00:00+08:00",
                "EndTime": "2026-09-30T15:59:59+08:00",
                "AutoRenew": false
            }
        });
        let p = parse_personal_plan_type(&body).expect("应解析出 PersonalPlan");
        assert_eq!(p.plan_type, "Pro");
        assert_eq!(p.start_time.as_deref(), Some("2026-07-30T00:00:00+08:00"));
        assert_eq!(p.end_time.as_deref(), Some("2026-09-30T15:59:59+08:00"));
        assert_eq!(p.auto_renew, Some(false));
    }

    #[test]
    fn personal_plan_type_defensive() {
        // PlanType 可能在顶层（防御式）；带 StartTime/AutoRenew 时一并解析。
        let p = parse_personal_plan_type(&json!({
            "PlanType": "Lite", "StartTime": "2026-01-01T00:00:00+08:00", "AutoRenew": true
        }))
        .expect("顶层 PlanType 也应解析");
        assert_eq!(p.plan_type, "Lite");
        assert_eq!(p.auto_renew, Some(true));
        assert!(p.start_time.is_some());
        // PlanType 缺失/空白 -> None（其余字段在不在无所谓）。
        assert!(parse_personal_plan_type(&json!({ "Result": { "PlanType": "  " } })).is_none());
        assert!(parse_personal_plan_type(&json!({ "Result": {} })).is_none());
        assert!(parse_personal_plan_type(&json!({})).is_none());
        // 有 PlanType 但缺 StartTime/AutoRenew -> 仍解析，对应字段为 None。
        let p = parse_personal_plan_type(&json!({ "PlanType": "Pro" })).expect("仅 PlanType 也应解析");
        assert_eq!(p.plan_type, "Pro");
        assert!(p.start_time.is_none());
        assert!(p.auto_renew.is_none());
    }

    #[test]
    fn personal_plan_request_body_is_compact_json() {
        // Plan 是业务参数，走 POST JSON body（非 query）；serde_json 紧凑序列化无空格。
        assert_eq!(personal_plan_request_body("CodingPlan"), r#"{"Plan":"CodingPlan"}"#);
        assert_eq!(personal_plan_request_body("AgentPlan"), r#"{"Plan":"AgentPlan"}"#);
    }

    #[test]
    fn coding_plan_unknown_window_skipped() {
        let result = json!({
            "QuotaUsage": [
                { "Level": "daily", "Percent": 9.0 },
                { "Level": "weekly", "Percent": 20.0 }
            ]
        });
        let tiers = parse_coding_plan_tiers(&result);
        assert_eq!(tiers.len(), 1, "未知 daily 窗口跳过");
        assert_eq!(tiers[0].name, TIER_WEEKLY_LIMIT);
        assert!(parse_coding_plan_tiers(&json!({})).is_empty());
    }

    #[test]
    fn region_derivation() {
        assert_eq!(
            volcengine_region("https://ark.cn-beijing.volces.com/api/coding"),
            "cn-beijing"
        );
        assert_eq!(
            volcengine_region("https://ark.cn-shanghai.volces.com/api/coding/v3"),
            "cn-shanghai"
        );
        // 无可识别 region 段时回落默认 cn-beijing。
        assert_eq!(volcengine_region("https://example.com/api/coding"), "cn-beijing");
    }

    #[test]
    fn canonical_query_is_sorted_and_encoded() {
        // 按 key 字母序：Action < Region < Version；值含 `-` 属 unreserved，不编码。
        assert_eq!(
            volcengine_canonical_query("GetAFPUsage", "cn-beijing"),
            "Action=GetAFPUsage&Region=cn-beijing&Version=2024-01-01"
        );
    }

    #[test]
    fn sign_structure_and_determinism() {
        // 没有服务端金标准向量时，锁定签名的结构契约 + 确定性（足以抓住 header 顺序、
        // scope 后缀、algorithm 前缀、空 body hash 等实现错误）。真实正确性靠用户实测。
        let now = chrono::DateTime::parse_from_rfc3339("2024-06-21T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let region = "cn-beijing";
        let query = volcengine_canonical_query("GetAFPUsage", region);
        let (auth, x_date, x_content) =
            volcengine_sign("AKLTtest", "secretkey", region, &query, b"", now);

        // 空 body 的 SHA-256（固定值），证明走的是空 body。
        assert_eq!(
            x_content,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(x_date, "20240621T000000Z");
        // Authorization 结构：算法无 AWS4 前缀、scope 结尾 ark/request、固定 SignedHeaders。
        assert!(
            auth.starts_with("HMAC-SHA256 Credential=AKLTtest/20240621/cn-beijing/ark/request,"),
            "unexpected credential/scope: {auth}"
        );
        assert!(
            auth.contains("SignedHeaders=host;x-date;x-content-sha256;content-type,"),
            "unexpected signed headers: {auth}"
        );
        // Signature 是 64 位十六进制。
        let sig = auth.rsplit("Signature=").next().unwrap();
        assert_eq!(sig.len(), 64);
        assert!(sig.bytes().all(|b| b.is_ascii_hexdigit()));

        // 确定性：同输入同输出。
        let (auth2, _, _) = volcengine_sign("AKLTtest", "secretkey", region, &query, b"", now);
        assert_eq!(auth, auth2);
    }

    #[test]
    fn auth_error_code_detection_and_extraction() {
        assert!(volcengine_is_auth_error_code("AccessDenied"));
        assert!(volcengine_is_auth_error_code("SignatureDoesNotMatch"));
        assert!(volcengine_is_auth_error_code("InvalidAuthorization"));
        assert!(!volcengine_is_auth_error_code("InvalidParameter.Action"));
        assert!(!volcengine_is_auth_error_code("InternalError"));

        let body = json!({
            "ResponseMetadata": { "RequestId": "x", "Error": { "Code": "AccessDenied", "Message": "no permission" } }
        });
        let (code, msg) = volcengine_response_error(&body).expect("应抽到 Error");
        assert_eq!(code, "AccessDenied");
        assert_eq!(msg, "no permission");

        let ok_body = json!({ "ResponseMetadata": { "RequestId": "x" }, "Result": {} });
        assert!(volcengine_response_error(&ok_body).is_none());
    }

    #[test]
    fn snapshot_picks_primary_and_lists_tiers() {
        // 多窗口：主值取 5h，tiers 结构化给出 5h/周/月 三条明细，不再走 raw_summary。
        let tiers = parse_afp_tiers(&json!({
            "AFPFiveHour": { "Quota": 50.0,  "Used": 12.5,  "ResetTime": 1_778_806_800_000_i64 },
            "AFPWeekly":   { "Quota": 500.0, "Used": 150.0, "ResetTime": 1_779_062_400_000_i64 },
            "AFPMonthly":  { "Quota": 2000.0,"Used": 850.5, "ResetTime": 1_780_531_200_000_i64 }
        }));
        let snap = crate::usage::snapshot_from_tiers(&tiers, &Some("Large".to_string()));
        assert_eq!(snap.used, Some(25.0));
        assert_eq!(snap.total, Some(100.0));
        assert_eq!(snap.remaining, Some(75.0));
        assert_eq!(snap.reset_at, Some(1_778_806_800)); // 5h ms -> secs
        assert_eq!(snap.unit, "%");
        assert_eq!(snap.plan.as_deref(), Some("Large"));
        assert!(snap.raw_summary.is_none(), "明细已结构化，不再走 raw_summary");
        assert_eq!(snap.tiers.len(), 3);
        assert_eq!(snap.tiers[0].window, TIER_FIVE_HOUR);
        assert!((snap.tiers[0].used_pct.unwrap() - 25.0).abs() < 1e-9);
        assert_eq!(snap.tiers[0].reset_at, Some(1_778_806_800));
        assert_eq!(snap.tiers[1].window, TIER_WEEKLY_LIMIT);
        assert!((snap.tiers[1].used_pct.unwrap() - 30.0).abs() < 1e-9);
        assert_eq!(snap.tiers[2].window, TIER_MONTHLY);
        assert!((snap.tiers[2].used_pct.unwrap() - 42.525).abs() < 1e-9);
    }

    #[test]
    fn snapshot_single_tier() {
        // 单窗口：主值=该窗口，tiers 只一条，无 plan，无 raw_summary。
        let tiers = vec![QuotaTier {
            name: TIER_FIVE_HOUR.to_string(),
            utilization: 42.0,
            resets_at: None,
            used_value_usd: None,
            max_value_usd: None,
        }];
        let snap = crate::usage::snapshot_from_tiers(&tiers, &None);
        assert_eq!(snap.used, Some(42.0));
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].window, TIER_FIVE_HOUR);
        assert!(snap.plan.is_none());
        assert!(snap.raw_summary.is_none());
    }

    #[test]
    fn plan_kind_from_vendor_routes_by_slug() {
        // 修复核心：vendor 已编码套餐类型，用量查询必须据此选 action，不能再无脑
        // 先探 Agent Plan（否则 Coding 套餐 provider 会显示成 Agent 额度）。
        assert_eq!(
            VolcPlanKind::from_vendor("volcengine-coding"),
            Some(VolcPlanKind::Coding)
        );
        assert_eq!(
            VolcPlanKind::from_vendor("volcengine-agent"),
            Some(VolcPlanKind::Agent)
        );
        // 旧版裸 `volcengine` 无法区分套餐 -> 自动探测。
        assert_eq!(
            VolcPlanKind::from_vendor("volcengine"),
            Some(VolcPlanKind::Auto)
        );
        // 非火山 vendor 不归用量适配器管。
        assert_eq!(VolcPlanKind::from_vendor("zhipu"), None);
        assert_eq!(VolcPlanKind::from_vendor("volcengine-coding-extra"), None);
    }

    #[test]
    fn usage_actions_never_probe_wrong_plan() {
        // Coding 套餐：只查 GetCodingPlanUsage，绝不能先打 GetAFPUsage——否则账号同时
        // 持有 Agent 套餐时会拿到 Agent 的额度。这正是被报告的 bug。
        assert_eq!(
            volcengine_usage_actions(VolcPlanKind::Coding),
            &["GetCodingPlanUsage"]
        );
        // Agent 套餐：只查 GetAFPUsage。
        assert_eq!(
            volcengine_usage_actions(VolcPlanKind::Agent),
            &["GetAFPUsage"]
        );
        // 裸 volcengine：保持旧行为，先 Agent 后 Coding。
        assert_eq!(
            volcengine_usage_actions(VolcPlanKind::Auto),
            &["GetAFPUsage", "GetCodingPlanUsage"]
        );
    }

    #[test]
    fn provider_carries_plan_kind() {
        // provider 构造时把套餐类型带进来，供 query 选用量 action。
        assert_eq!(
            VolcengineUsageProvider::default().plan,
            VolcPlanKind::Auto
        );
        assert_eq!(
            VolcengineUsageProvider { plan: VolcPlanKind::Coding }.plan,
            VolcPlanKind::Coding
        );
    }

    #[tokio::test]
    async fn missing_ak_sk_is_not_configured() {
        // 无 usage_creds -> NotConfigured（与智谱缺 api_key 一致）。
        let err = VolcengineUsageProvider::default()
            .query(None, None, "https://ark.cn-beijing.volces.com/api/coding")
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::NotConfigured));

        // usage_creds 存在但 AK/SK 为空 -> 同样 NotConfigured。
        let creds = UsageCreds::default();
        let err = VolcengineUsageProvider::default()
            .query(None, Some(&creds), "https://ark.cn-beijing.volces.com/api/coding")
            .await
            .unwrap_err();
        assert!(matches!(err, UsageError::NotConfigured));
    }

    #[test]
    fn plan_models_from_result_datas() {
        // 真实 OpenAPI 信封：模型在 Result.Datas[].ModelID。
        let body = json!({
            "ResponseMetadata": { "RequestId": "x" },
            "Result": { "Datas": [ {"ModelID": "doubao-seed-1.6"}, {"ModelID": "doubao-1.5-pro"} ] }
        });
        assert_eq!(
            parse_plan_models(&body),
            vec!["doubao-seed-1.6", "doubao-1.5-pro"]
        );
    }

    #[test]
    fn plan_models_defensive_when_no_result_wrapper() {
        // 官方返回示例是残缺渲染：Datas 可能在顶层；缺/空 ModelID 的条目跳过。
        let body = json!({
            "Datas": [ {"ModelID": "doubao-seed-1.6"}, {"Name": "x"}, {"ModelID": ""} ]
        });
        assert_eq!(parse_plan_models(&body), vec!["doubao-seed-1.6"]);
    }

    #[test]
    fn plan_models_empty_when_no_datas() {
        assert!(parse_plan_models(&json!({ "Result": {} })).is_empty());
        assert!(parse_plan_models(&json!({})).is_empty());
    }
}
