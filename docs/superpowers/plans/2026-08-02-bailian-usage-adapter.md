# Bailian (千问 Token Plan) Usage Adapter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the captured qianwenai console cookie into live Bailian/千问 Token Plan quota data — a new `BailianUsageProvider` queries the qianwenai console gateway (cookie + secToken), maps the response to the existing 5-hour/weekly tier model, flips the `bailian-token` chip from "不支持" to real percentages, and corrects the cookie-capture login URL to `platform.qianwenai.com`.

**Architecture:** The cookie flows through a new `#[serde(skip)] cookie` field on `UsageCreds` (populated from the existing `{provider_id}::usage_cookie` keyring channel in a reworked `usage_creds`). `BailianUsageProvider::query` does two cookie-authenticated HTTP steps (secToken → usage gateway) and maps the response. The `UsageProvider::query` signature and the other three adapters are unchanged. The UI flips by adding `bailian-token` to `USAGE_ADAPTER_VENDORS`.

**Tech Stack:** Rust (reqwest, serde_json, async_trait, wiremock for HTTP tests; co-located `#[cfg(test)]` tests), Vue 3. Backend gate: `cargo test --manifest-path src-tauri/Cargo.toml`. Frontend gate: `npx vue-tsc --noEmit`.

**Spec:** `docs/superpowers/specs/2026-08-02-bailian-usage-adapter-design.md`

> **Amended post-E2E:** percentages are 0–1 used fractions (×100); `per1WeekPercentage` is used (not remaining); `per5HourResetTime` added; SSO cookie confirmed = `login_qianwenai_ticket`.
>
> **Amended (subscription query):** a third, **best-effort** `subscription` gateway call was added after the usage call to fetch plan type + validity: `specCode` → plan label "千问 Token Plan · {spec}", `startTime`/`endTime` (ms) → `plan_info.start_time`/`end_time` (ISO8601), `autoRenewFlag` → `plan_info.auto_renew`. It degrades gracefully (on any failure the usage tiers still show with the default plan label + `plan_info = None`). See spec §Functional 2.3.

## Global Constraints

- **Vendor slug:** `bailian-token`. **Cookie keyring entry:** `{provider_id}::usage_cookie` (already exists from the capture feature).
- **secToken endpoint:** `GET https://platform-home.qianwenai.com/tool/user/info.json` → response `data.secToken`. Auth: `Cookie:` header = captured cookie.
- **Usage endpoint:** `POST https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage`. `Content-Type: application/x-www-form-urlencoded`. Auth: `Cookie:` header + form `sec_token`.
- **Form fields:** `product=sfm_bailian`, `action=BroadScopeAspnGateway`, `sec_token=<secToken>`, `region=cn-beijing`, `params=` the JSON:
  `{"Api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage","Data":{"cornerstoneParam":{"domain":"platform.qianwenai.com","consoleSite":"QIANWENAI","console":"ONE_CONSOLE","xsp_lang":"zh-CN","protocol":"V2","productCode":"p_efm"}},"V":"1.0"}`
- **Response path:** `data.DataV2.data.data`. Fields: `per5HourPercentage` (number, **used fraction 0–1**, ×100 → used %), `per1WeekPercentage` (number, **used fraction 0–1**, ×100 → used % — the doc's "remaining" claim is wrong per real responses), `per5HourResetTime` + `per1WeekResetTime` (number, **ms** epoch).
- **Tier windows** (`usage/coding_plan.rs`): `TIER_FIVE_HOUR = "five_hour"`, `TIER_WEEKLY_LIMIT = "weekly_limit"`.
- **`UsageSnapshot`:** `billing_model = "plan"`, `unit = "%"`, `plan = "千问 Token Plan"`, `used/total/remaining = None`, top-level `reset_at` = weekly reset (secs). `UsageTier { window, used_pct: Option<f64>, reset_at: Option<i64> }`.
- **`UsageError` variants used:** `NotConfigured` (no cookie), `Network`, `AuthFailed` (secToken missing / non-2xx), `Parse` (missing `DataV2.data.data`).
- **`UsageProvider::query` signature is unchanged** — the cookie arrives via the `usage_creds: Option<&UsageCreds>` param (`.cookie` field). `query_usage` (`commands.rs:839-863`) already passes `usage_creds.as_ref()`; **no change there**.
- **Capture login URL correction:** `LOGIN_URL` → `https://platform.qianwenai.com/home/billing/subscription/token-plan-individual`; `CONSOLE_URL` (for `cookies_for_url`) → `https://platform.qianwenai.com/`.
- Conventional-commit messages; work on `main`; one commit per task.

---

### Task 1: Cookie plumbing — `UsageCreds.cookie` + rework `usage_creds`

Add the cookie field to `UsageCreds` and rework `usage_creds` so it populates all three credential fields and always returns `Some` for a found provider (the current `Some(sk?)` early-return would discard the cookie for a Bailian provider that has no AK/SK). TDD. Also verify the Volcengine adapter still handles a missing SK gracefully.

**Files:**
- Modify: `src-tauri/src/config/types.rs` (`UsageCreds` struct).
- Modify: `src-tauri/src/proxy/state.rs` (`usage_creds` method, ~lines 89-106; + tests).
- Verify/possibly modify: `src-tauri/src/usage/volcengine.rs` (the SK-presence check — see Step 5).

**Interfaces:**
- Produces: `UsageCreds.cookie: Option<String>` (`#[serde(skip)]`); `AppStateInner::usage_creds` now sets `creds.cookie` from `secrets.get_usage_cookie`. Consumed by Task 3's `BailianUsageProvider::query` via `usage_creds.and_then(|c| c.cookie.as_deref())`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/proxy/state.rs` `#[cfg(test)]` (follow the existing `AppStateInner` test construction pattern there — it builds a `SecretStoreHandle` over `MemoryStore`), add a test that a Bailian provider (no `usage_creds` AK/SK in config) still yields `Some(UsageCreds { cookie: Some(...), .. })`:

```rust
    #[tokio::test]
    async fn usage_creds_populates_cookie_for_bailian_without_aksk() {
        let state = /* build AppStateInner with a config containing a provider
                       { id: "bailian1", vendor: "bailian-token", usage_creds: None }
                       and a MemoryStore-backed SecretStoreHandle, mirroring the
                       existing state.rs test helpers */;
        state.secrets.set_usage_cookie("bailian1", "cna=x; ticket=y").unwrap();
        let creds = state.usage_creds("bailian1").await.expect("found provider -> Some");
        assert_eq!(creds.cookie.as_deref(), Some("cna=x; ticket=y"));
        assert_eq!(creds.access_key_id, None);
        assert_eq!(creds.secret_access_key, None);
    }
```

(If the existing test helpers don't make a one-provider config trivial, extend them minimally — do not restructure.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib proxy::state`
Expected: FAIL — `no field \`cookie\`` on `UsageCreds` (field doesn't exist yet).

- [ ] **Step 3: Add `cookie` to `UsageCreds`**

In `src-tauri/src/config/types.rs`, add the field to `UsageCreds` (after `secret_access_key`):

```rust
    /// Aliyun Bailian (qianwenai console) usage-session cookie — a serialized `Cookie:` header
    /// captured by the in-app login window. `#[serde(skip)]`: never persisted to app_config.json;
    /// loaded from the keyring (`{provider_id}::usage_cookie`) in `AppStateInner::usage_creds`.
    #[serde(skip)]
    pub cookie: Option<String>,
```

(`UsageCreds` derives `Default` — the new field defaults to `None`.)

- [ ] **Step 4: Rework `usage_creds` to populate the cookie and always return `Some`**

In `src-tauri/src/proxy/state.rs`, replace the `usage_creds` body (~lines 89-106) with:

```rust
    pub async fn usage_creds(&self, provider_id: &str) -> Option<UsageCreds> {
        let provider = {
            let cfg = self.config.read().await;
            cfg.providers.iter().find(|p| p.id == provider_id)?.clone()
        };
        let mut creds = provider.usage_creds.unwrap_or_default();
        creds.secret_access_key = self.secrets.get_usage_sk(provider_id).ok().flatten();
        creds.cookie = self.secrets.get_usage_cookie(provider_id).ok().flatten();
        Some(creds) // may be all-None; each adapter checks the credential it needs
    }
```

- [ ] **Step 5: Verify the Volcengine adapter handles a missing SK gracefully (no panic)**

The old `usage_creds` returned `None` when there was no SK (`Some(sk?)`); now it returns `Some(creds)` with `secret_access_key: None`. Read `src-tauri/src/usage/volcengine.rs`'s `query`: confirm it treats a missing SK as `NotConfigured`/`AuthFailed` (e.g. `usage_creds.and_then(|c| c.secret_access_key).ok_or(UsageError::NotConfigured)?`), NOT an `unwrap()`. If it `unwrap()`s or assumes `Some`, fix it to degrade gracefully, and add/adjust a test (`cargo test`) asserting a Volcengine provider with no SK → `NotConfigured` (not a panic). If it already degrades, no change — note that in the report.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — the new test + the full suite green (esp. any existing volcengine usage tests).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/proxy/state.rs src-tauri/src/usage/volcengine.rs
git commit -m "feat(usage): plumb captured cookie through UsageCreds.usage_creds"
```

(Only `git add volcengine.rs` if Step 5 changed it.)

---

### Task 2: `usage/bailian.rs` pure helpers (constants, secToken extraction, request + response mapping)

Create the adapter module's testable pure logic first: the endpoint constants, `extract_sec_token`, `usage_form`, and `map_usage` (response → `UsageSnapshot`). No HTTP, no `query`, no registration yet. TDD.

**Files:**
- Create: `src-tauri/src/usage/bailian.rs`

**Interfaces:**
- Produces: `pub const`-style module constants + `fn extract_sec_token(&serde_json::Value) -> Option<&str>`, `fn usage_form(sec_token) -> Vec<(&'static str, &str)>`, `fn map_usage(&serde_json::Value) -> Result<UsageSnapshot, UsageError>`, and `pub struct BailianUsageProvider;`. Consumed by Task 3's `query`.

- [ ] **Step 1: Create the module with constants + the struct + failing tests**

Create `src-tauri/src/usage/bailian.rs`:

```rust
//! 千问 Token Plan (Bailian) usage adapter. Queries the qianwenai console gateway with the
//! captured login-state cookie (see `bailian_login`): GET secToken, then POST the usage gateway.
//! See spec `docs/superpowers/specs/2026-08-02-bailian-usage-adapter-design.md`.

use crate::usage::{UsageError, UsageSnapshot, UsageTier, TIER_FIVE_HOUR, TIER_WEEKLY_LIMIT};

const SEC_TOKEN_URL: &str = "https://platform-home.qianwenai.com/tool/user/info.json";
const USAGE_URL: &str = "https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";
const PARAMS_JSON: &str = r#"{"Api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage","Data":{"cornerstoneParam":{"domain":"platform.qianwenai.com","consoleSite":"QIANWENAI","console":"ONE_CONSOLE","xsp_lang":"zh-CN","protocol":"V2","productCode":"p_efm"}},"V":"1.0"}"#;

/// 千问 Token Plan usage adapter (cookie + secToken auth).
pub struct BailianUsageProvider;

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
        raw_summary: Some(inner.to_string()),
        plan: Some("千问 Token Plan".to_string()),
        tiers,
        billing_model: "plan".to_string(),
        plan_info: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
```

- [ ] **Step 2: Declare the module + run tests to verify they fail**

In `src-tauri/src/usage/mod.rs`, add `pub mod bailian;` (next to `pub mod volcengine;` etc.).

Run: `cargo test --manifest-path src-tauri/Cargo.toml bailian`
Expected: PASS (the helpers are implemented inline above, so this is GREEN-on-first-run for the pure logic; confirm all 5 tests pass). If any fails, fix the helper, not the test.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/usage/bailian.rs src-tauri/src/usage/mod.rs
git commit -m "feat(usage): bailian adapter pure helpers (secToken, form, response mapping)"
```

---

### Task 3: `BailianUsageProvider::query` (wiremock two-step) + register

Implement the HTTP `query` (secToken → usage, both cookie-authenticated) using Task 2's helpers, register `bailian-token` in `usage_provider_for`. TDD with `wiremock`.

**Files:**
- Modify: `src-tauri/src/usage/bailian.rs` (add `impl UsageProvider for BailianUsageProvider`).
- Modify: `src-tauri/src/usage/mod.rs` (`usage_provider_for` match arm).

**Interfaces:**
- Consumes: Task 1's `usage_creds.cookie`; Task 2's helpers + constants.
- Produces: a registered `BailianUsageProvider` so `get_usage`/`get_all_usage` for `bailian-token` returns real data (Task 5 flips the UI).

- [ ] **Step 1: Write the failing wiremock tests**

Append to `bailian.rs` `#[cfg(test)] mod tests` (wiremock is already a dev-dep — mirror the HTTP-test style in `volcengine.rs`/`zhipu.rs`):

```rust
    use crate::usage::UsageProvider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const USAGE_API_Q: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";

    #[tokio::test]
    async fn query_no_cookie_is_not_configured() {
        // No cookie -> NotConfigured (does not touch the network).
        let r = BailianUsageProvider.query(None, None, "").await;
        assert!(matches!(r, Err(UsageError::NotConfigured)));
    }

    #[tokio::test]
    async fn query_at_success_maps_tiers_and_sends_cookie_to_both() {
        let server = MockServer::start().await;
        let sec_url = format!("{}/tool/user/info.json", server.uri());
        let usage_url = format!("{}/data/api.json?api={USAGE_API_Q}", server.uri());
        // Both mocks require the Cookie header — `.expect(1)` proves it was sent.
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
        let snap = query_at(&reqwest::Client::new(), &sec_url, &usage_url, "c=1").await.unwrap();
        assert_eq!(snap.tiers[0].used_pct, Some(50.0)); // five_hour: 0.5 fraction × 100
        assert_eq!(snap.tiers[0].reset_at, Some(1785698280));
        assert_eq!(snap.tiers[1].used_pct, Some(20.0)); // weekly: 0.2 used fraction × 100 (NOT inverted)
        assert_eq!(snap.tiers[1].reset_at, Some(1786285080));
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
            "c=1",
        ).await;
        assert!(matches!(r, Err(UsageError::AuthFailed(_))));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml bailian::tests::query`
Expected: FAIL — `query`/`query_at` not defined.

- [ ] **Step 3: Implement `query` (with a testable `query_at` seam)**

In `src-tauri/src/usage/bailian.rs`, add the trait impl plus a URL-injectable `query_at` helper (the test seam — `query` calls it with the real `const`s, the wiremock tests call it with the mock-server URLs):

```rust
use crate::config::UsageCreds;
use crate::usage::UsageProvider;
use async_trait::async_trait;
use reqwest::header::COOKIE;

#[async_trait]
impl UsageProvider for BailianUsageProvider {
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
        query_at(&client, SEC_TOKEN_URL, USAGE_URL, cookie).await
    }
}

/// Two-step query against configurable URLs (the test seam). Both requests carry the cookie.
async fn query_at(
    client: &reqwest::Client,
    sec_token_url: &str,
    usage_url: &str,
    cookie: &str,
) -> Result<UsageSnapshot, UsageError> {
    let resp = client
        .get(sec_token_url)
        .header(COOKIE, cookie)
        .send()
        .await
        .map_err(|e| UsageError::Network(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(UsageError::AuthFailed(format!("secToken HTTP {}", resp.status())));
    }
    let info: serde_json::Value = resp.json().await.map_err(|e| UsageError::Parse(e.to_string()))?;
    let sec_token = extract_sec_token(&info)
        .ok_or_else(|| UsageError::AuthFailed("secToken missing in user/info".into()))?;

    let resp = client
        .post(usage_url)
        .header(COOKIE, cookie)
        .form(&usage_form(sec_token))
        .send()
        .await
        .map_err(|e| UsageError::Network(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(UsageError::AuthFailed(format!("usage HTTP {}", resp.status())));
    }
    let payload: serde_json::Value = resp.json().await.map_err(|e| UsageError::Parse(e.to_string()))?;
    map_usage(&payload)
}
```

The Step 1 wiremock tests already reference this `query_at` (RED until this step adds it); after implementing, they should pass. Note `query` extracts the cookie from `usage_creds`, builds a 15s-timeout client, and delegates to `query_at` with the `const` URLs — the only network logic lives in `query_at`.

- [ ] **Step 4: Register the adapter**

In `src-tauri/src/usage/mod.rs` `usage_provider_for`, add the arm (before `_ => None`):

```rust
        "bailian-token" => Some(Box::new(bailian::BailianUsageProvider)),
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml bailian` then the full `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all bailian tests (pure + wiremock) + the full suite green.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/usage/bailian.rs src-tauri/src/usage/mod.rs
git commit -m "feat(usage): bailian-token usage adapter (cookie+secToken two-step query)"
```

---

### Task 4: Correct the cookie-capture login URL/domain → qianwenai

The capture feature shipped targeting `bailian.console.aliyun.com`, but the usage API needs `platform.qianwenai.com` login-state cookies. Correct the two certain constants; the SSO detection constants are best-guess pending manual confirmation (the manual `完成登录` button is the reliable capture path regardless).

**Files:**
- Modify: `src-tauri/src/bailian_login.rs` (constants `LOGIN_URL`, `CONSOLE_URL` ~lines 46-54; comments on `PASSPORT_HOSTS`/`SSO_COOKIE_NAMES`).

**Interfaces:** None (constant values + comments only).

- [ ] **Step 1: Update the certain constants + comments**

In `src-tauri/src/bailian_login.rs`, replace the constants block (~lines 45-54):

```rust
const LOGIN_LABEL: &str = "bailian-login";
/// The qianwenai page whose login-state cookie the usage adapter needs. Logged-out it redirects
/// to qianwenai SSO, then returns here authenticated.
const LOGIN_URL: &str =
    "https://platform.qianwenai.com/home/billing/subscription/token-plan-individual";
/// Captured cookies are those that would be sent here (qianwenai login state) — used as the
/// `Cookie:` header by the usage adapter's secToken + gateway calls.
const CONSOLE_URL: &str = "https://platform.qianwenai.com/";
/// Cookie name that indicates a successful qianwenai login — CONFIRMED against a real captured
/// jar during manual E2E testing. Capture fires when it appears.
const SSO_COOKIE_NAMES: &[&str] = &["login_qianwenai_ticket"];
/// Hosts the login flow passes through before returning to the console (host-transition signal).
/// Additive secondary signal; the cookie-name poll now drives auto-detect.
const PASSPORT_HOSTS: &[&str] =
    &["passport.aliyun.com", "signin.aliyun.com", "login.aliyun.com", "login.taobao.com", "login.qianwenai.com"];
```

- [ ] **Step 2: Build + run the full suite**

Run: `cargo build --manifest-path src-tauri/Cargo.toml` then `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: build OK; tests PASS (constant change, no logic change).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/bailian_login.rs
git commit -m "fix(bailian): switch cookie-capture login URL to platform.qianwenai.com"
```

---

### Task 5: Flip the UI — add `bailian-token` to `USAGE_ADAPTER_VENDORS`

With the adapter registered (Task 3), `hasUsageAdapter("bailian-token")` must be true so the dashboard chip renders real data instead of the "不支持" placeholder.

**Files:**
- Modify: `src/lib/selectLabel.ts` (`USAGE_ADAPTER_VENDORS`, ~line 60).

**Interfaces:** None (frontend predicate change).

- [ ] **Step 1: Add the vendor to the adapter set**

In `src/lib/selectLabel.ts`, change `USAGE_ADAPTER_VENDORS`:

```ts
const USAGE_ADAPTER_VENDORS = ["zhipu", "deepseek", "volcengine-agent", "volcengine-coding", "bailian-token"];
```

- [ ] **Step 2: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: PASS (no type errors).

- [ ] **Step 3: Manual verification (combined with the adapter E2E)**

Run `npm run tauri dev`. With a `bailian-token` provider logged in (cookie captured via the qianwenai login window): the dashboard quota chip now shows **real** 5-hour/weekly percentages (not "不支持"). If the cookie is expired/cleared, it shows "查询失败" + the error (e.g. "未登录…" / "登录态失效…"), prompting re-login. Existing vendors' chips are unchanged.

- [ ] **Step 4: Commit**

```bash
git add src/lib/selectLabel.ts
git commit -m "feat(usage): flip bailian-token chip to real data (usage adapter)"
```

---

## Self-Review (spec coverage)

| Spec section / requirement | Task |
|---|---|
| Cookie reaches adapter via `UsageCreds.cookie` + reworked `usage_creds` (always `Some`) | Task 1 |
| Volcengine adapter still handles missing SK gracefully | Task 1 Step 5 |
| Two-step query: secToken GET → usage POST, both cookie-authenticated | Task 3 |
| Form fields + `params` JSON + query params (product/action/api) | Task 2 (`usage_form`, constants) |
| Response mapping (asymmetric %, ms reset) | Task 2 (`map_usage`) |
| `UsageError`: NotConfigured / AuthFailed / Network / Parse | Task 3 |
| Register `bailian-token` in `usage_provider_for` | Task 3 Step 4 |
| UI flip (`USAGE_ADAPTER_VENDORS`) | Task 5 |
| Capture login URL/domain → qianwenai | Task 4 |
| `UsageProvider::query` signature unchanged; `query_usage` unchanged | Tasks 1/3 (no change to either) |
| Tests: pure mapping + secToken + request build (unit); two-step + cookie + errors (wiremock) | Tasks 2 + 3 |

Placeholder scan: none — every code step shows complete code, including the Task 3 wiremock tests (real `Mock::given(...)` assertions against the `query_at` seam; RED = `query_at` undefined, GREEN once Step 3 adds it). Type consistency: `extract_sec_token` / `usage_form` / `map_usage` / `query_at` / `BailianUsageProvider` / `UsageCreds.cookie` match across tasks. Task 5's `USAGE_ADAPTER_VENDORS` edit matches the `hasUsageAdapter` predicate (capture spec) that the chip already reads.
