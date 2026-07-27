# Bailian (千问 Token Plan) Usage Adapter — Design

**Date:** 2026-08-02
**Status:** Approved (pending implementation plan)
**Related:** `2026-08-02-bailian-usage-cookie-capture-design.md` (this consumes its captured cookie and **corrects** its login URL; see *Capture-spec amendments*). This is the capture spec's deferred *Follow-up #1* (the `usage/bailian.rs` adapter), now unblocked by the user-supplied Qianwen Token Plan usage API doc.

## Overview

Turn the captured qianwenai console cookie into real quota data: a new `BailianUsageProvider` implements `UsageProvider`, queries the 千问 Token Plan personal-edition usage via the qianwenai console gateway (cookie + secToken), maps the response into the existing 5-hour / weekly tier model, and flips the `bailian-token` usage chip from "不支持" to live percentages. This also **corrects the cookie-capture login URL/domain** (it targeted `bailian.console.aliyun.com`, but the usage API needs `platform.qianwenai.com` login-state cookies).

The API is a console-internal gateway (not DOM scraping, not Bearer/AKSK). Source: user-provided "千问 Token Plan 用量接口说明" (a sibling project's notes). Endpoints:

| Step | Method | URL |
|---|---|---|
| secToken | `GET` | `https://platform-home.qianwenai.com/tool/user/info.json` |
| usage | `POST` | `https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage` |
| subscription *(best-effort)* | `POST` | `https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription` |

All carry the captured qianwenai cookie as a `Cookie:` header. The subscription step enriches the plan type + validity (§Functional 2.3) but is optional: if it fails, the usage tiers still display with the default plan label.

## Requirements

### Functional

1. **Cookie reaches the adapter.** The captured cookie (stored under the parallel keyring channel `{provider_id}::usage_cookie`) is loaded into `UsageCreds` and read by `BailianUsageProvider::query`. Missing cookie → `UsageError::NotConfigured` ("未登录，请先在服务商设置中登录百炼").
2. **Three-step query** (one `reqwest` client; every request carries the cookie):
   1. **secToken**: `GET https://platform-home.qianwenai.com/tool/user/info.json` → `data.secToken`. Missing/empty → `AuthFailed` ("百炼登录态失效，请重新登录").
   2. **usage** (required): `POST` the gateway URL with `Content-Type: application/x-www-form-urlencoded`, form fields `product=sfm_bailian`, `action=BroadScopeAspnGateway`, `sec_token=<secToken>`, `region=cn-beijing`, `params` = the JSON string below. Parse `data.DataV2.data.data`.
   3. **subscription** (best-effort): `POST` the subscription gateway URL — same form fields, but the `params.Data` also carries `commodityCode:"sfm_tokenplansolo_public_cn"`. Parse `data.DataV2.data.data`. **Any failure (network / non-2xx / parse / missing data) is swallowed** — the usage tiers still display with `plan = None` (no tier known) and `plan_info = None`; only the usage query is required.
      - Mapping: `specCode` (e.g. `"lite"`) → `plan = "{Titlecase(specCode)}"` — **the tier only, no product prefix** (e.g. "Lite"; the provider is already labeled 百炼 elsewhere, so prepending a product name is redundant — and the API returns no product name anyway, only `specCode`); `startTime`/`endTime` (ms) → `plan_info.start_time`/`end_time` (ISO8601 via `tiers::millis_to_iso8601`); `autoRenewFlag` (bool) → `plan_info.auto_renew`. `plan_info` is `Some` only when at least one of start/end/auto_renew is present.
      - Worked example (real response): `{"specCode":"lite","startTime":1785653265000,"endTime":1788364800000,"autoRenewFlag":false}` → plan "Lite" (tier only), `plan_info.auto_renew = Some(false)`, start/end present.
3. **Response → `UsageSnapshot`** (CONFIRMED against a real E2E response — supersedes the API doc's percentage semantics, which are wrong):
   - `billing_model = "plan"`, `unit = "%"`, `plan = None` (the titlecased tier is filled by the subscription step; `None` until/unless it succeeds).
   - Both percentage fields are **USED** fractions in **0–1** (NOT 0–100): multiply by 100 → used %, clamped 0–100.
   - tier **`five_hour`** (`TIER_FIVE_HOUR`): `used_pct = per5HourPercentage × 100` (used fraction); `reset_at = per5HourResetTime / 1000` (ms → epoch secs).
   - tier **`weekly_limit`** (`TIER_WEEKLY_LIMIT`): `per1WeekPercentage` is **used** (the API doc's "per1WeekPercentage is remaining" claim and its worked example are BOTH wrong per real responses) → `used_pct = per1WeekPercentage × 100` (do NOT invert); `reset_at = per1WeekResetTime / 1000` (ms → epoch secs).
   - Top-level `used`/`total`/`remaining` = `None` (percentage-based; the tiers carry the data). Top-level `reset_at` = the weekly reset (feeds the breaker's `recover_at`). `raw_summary` = the raw `data.data` JSON (debugging). `plan_info = None`.
   - Worked example (real response): `per5HourPercentage=0.3392, per5HourResetTime=1785698280000` → five_hour used 33.92%, reset 1785698280; `per1WeekPercentage=0.0950, per1WeekResetTime=1786285080000` → weekly used 9.50%, reset 1786285080 ✓.
4. **Register the adapter**: `usage_provider_for("bailian-token")` → `Some(Box::new(bailian::BailianUsageProvider))`.
5. **UI flip**: add `"bailian-token"` to `USAGE_ADAPTER_VENDORS` (`src/lib/selectLabel.ts`). `hasUsageAdapter("bailian-token")` becomes true, so the dashboard chip renders real data (or "查询失败" + the error on failure) instead of the "不支持" placeholder.
6. **Correct the capture login URL/domain** (see *Capture-spec amendments*): the cookie-capture window must load `platform.qianwenai.com`, not `bailian.console.aliyun.com`.

### Non-functional

- No change to dispatch / breaker / translate / catalog. The breaker's `compute_recover_at` consumes the tiers/`reset_at` as for other plan vendors.
- `UsageProvider::query` signature and the other three adapters (zhipu/deepseek/volcengine) are **unchanged** (cookie flows through `UsageCreds`, not a new trait param).
- Backward compatible: `bailian-token` previously had no adapter (chip = "不支持"); existing vendors are untouched.

## Design

### Cookie plumbing — extend `UsageCreds` (amends the capture spec)

The capture spec kept `UsageCreds` AK/SK-only. The adapter needs the cookie, and the lowest-ripple home is `UsageCreds` itself ("credentials needed to query usage" — AK/SK for Volcengine, cookie for Bailian):

- `src-tauri/src/config/types.rs` — add to `UsageCreds`:
  ```rust
  /// Aliyun Bailian (qianwenai console) usage-session cookie — a serialized `Cookie:` header
  /// captured by the in-app login window. `#[serde(skip)]`: never persisted to app_config.json;
  /// loaded from the keyring (`{provider_id}::usage_cookie`) in `AppStateInner::usage_creds`.
  #[serde(skip)]
  pub cookie: Option<String>,
  ```
  (`UsageCreds` already derives `Default`, so `unwrap_or_default()` covers the new field.)
- `src-tauri/src/proxy/state.rs` — **rework `usage_creds`** so it populates all three credential fields and always returns `Some` for a found provider. The current code early-returns `None` via `Some(sk?)` when there is no SK, which would **discard the cookie** for a Bailian provider (no AK/SK). New shape:
  ```rust
  pub async fn usage_creds(&self, provider_id: &str) -> Option<UsageCreds> {
      let provider = {
          self.config.read().await
              .providers.iter().find(|p| p.id == provider_id)?
              .clone()
      };
      let mut creds = provider.usage_creds.unwrap_or_default();
      creds.secret_access_key = self.secrets.get_usage_sk(provider_id).ok().flatten();
      creds.cookie = self.secrets.get_usage_cookie(provider_id).ok().flatten();
      Some(creds) // may be all-None; each adapter checks what it needs
  }
  ```
  **Verify:** the Volcengine adapter still behaves correctly when `secret_access_key` is now `None`-passthrough instead of the old early-`None` — it must return `NotConfigured`/`AuthFailed`, not panic. (It derives its signature from AK+SK; a missing SK should surface as a config error, as before.)
- `src/lib/types.ts` — `UsageCreds` is a TS mirror; the `cookie` field is backend-only (`#[serde(skip)]`, never serialized to the frontend), so **no TS change**. (Confirm the frontend never reads `usage_creds.cookie` — it doesn't; presence is tracked via the separate `cookieSet` map / `provider_has_usage_cookie`.)

### The adapter — `src-tauri/src/usage/bailian.rs`

`pub struct BailianUsageProvider;` implementing `UsageProvider` (mirror the existing adapters' `reqwest` + error-mapping style). `query`:

1. `let cookie = usage_creds.and_then(|c| c.cookie.as_deref());` — `None` → `NotConfigured`.
2. Build one `reqwest::Client` (reuse the project's client construction — check how `volcengine.rs`/`zhipu.rs` build theirs; follow the same pattern incl. timeout).
3. **secToken**: `client.get("https://platform-home.qianwenai.com/tool/user/info.json").header(COOKIE, cookie).send().await` → network error → `Network`; non-2xx or missing `data.secToken` → `AuthFailed`.
4. **usage**: `client.post(<gateway URL>).header(COOKIE, cookie).form(&[("product","sfm_bailian"),("action","BroadScopeAspnGateway"),("sec_token",sec_token),("region","cn-beijing"),("params", params_json)]).send().await`. The `params` JSON:
   ```json
   {"Api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage","Data":{"cornerstoneParam":{"domain":"platform.qianwenai.com","consoleSite":"QIANWENAI","console":"ONE_CONSOLE","xsp_lang":"zh-CN","protocol":"V2","productCode":"p_efm"}},"V":"1.0"}
   ```
   network error → `Network`; non-2xx → `AuthFailed` (expired session) or `Parse`; missing `data.DataV2.data.data` → `Parse`.
5. **subscription** (best-effort): `client.post(<subscription gateway URL>).header(COOKIE, cookie).form(&[same fields, params = subscription params JSON]).send().await`. The subscription `params` JSON adds `commodityCode`:
   ```json
   {"Api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription","Data":{"commodityCode":"sfm_tokenplansolo_public_cn","cornerstoneParam":{"domain":"platform.qianwenai.com","consoleSite":"QIANWENAI","console":"ONE_CONSOLE","xsp_lang":"zh-CN","protocol":"V2","productCode":"p_efm"}},"V":"1.0"}
   ```
   On success, map per §Functional 2.3; any failure is swallowed (usage tiers + default plan label preserved). Both gateway POSTs go through one helper, `post_gateway(client, url, form, cookie, label)` — POST + cookie + form, non-2xx → `AuthFailed("{label} HTTP {status}")`, body parse → `Parse`.
6. **Map** the parsed object to a `UsageSnapshot` per §Functional 3 (pure helper `fn map_usage(data: &Value) -> Result<UsageSnapshot, UsageError>` — unit-testable without HTTP). `map_usage` leaves `plan = None` + `plan_info = None`; the subscription step (§5) sets `plan` to the titlecased tier (and `plan_info`) when it succeeds.

The URLs/constants (`platform-home.qianwenai.com`, `cs-data.qianwenai.com`, product/action/api, the `params` JSON, region) are module `const`s in `bailian.rs`, copied verbatim from the doc.

### Register + UI flip

- `src-tauri/src/usage/mod.rs` — `pub mod bailian;` and add to `usage_provider_for`: `"bailian-token" => Some(Box::new(bailian::BailianUsageProvider)),`.
- `src/lib/selectLabel.ts` — add `"bailian-token"` to `USAGE_ADAPTER_VENDORS` (keep the "in sync with `usage_provider_for`" comment true).

### Capture login URL/domain correction — `src-tauri/src/bailian_login.rs`

- `LOGIN_URL` → `https://platform.qianwenai.com/home/billing/subscription/token-plan-individual` (the doc's 页面入口; logged-out it redirects to qianwenai SSO, then returns authenticated).
- `CONSOLE_URL` (the URL passed to `cookies_for_url`) → `https://platform.qianwenai.com/` — the domain whose login-state cookies the secToken/usage calls need.
- `SSO_COOKIE_NAMES` — **CONFIRMED** against a real captured jar during manual E2E testing: the qianwenai login cookie is `login_qianwenai_ticket` (the earlier Aliyun guesses `login_aliyunid_ticket`/`login_aliyunid` never fired, which is why poll auto-detect missed). The poll now drives auto-detect off this confirmed name.
- `PASSPORT_HOSTS` remains an additive secondary (host-transition) signal. The qianwenai login may still pass through Alibaba passport hosts; left as-is since the cookie-name poll is now the reliable auto-detect driver. The manual `完成登录` button remains the fallback for an already-authenticated WebView session.

### Error handling → UI

The `UsageEntry` carries the `UsageError` string; for an adapted vendor a failed query renders "查询失败" (the honest-placeholder logic from the capture spec already distinguishes this from "不支持"). So: missing cookie / expired session → "查询失败" + "未登录…" / "登录态失效…", prompting re-login.

## Testing

- **Unit (`cargo test`):**
  - `map_usage` pure mapping: a real response (`per5HourPercentage=0.3392…, per5HourResetTime=1785698280000`, `per1WeekPercentage=0.0950…, per1WeekResetTime=1786285080000` → five_hour used ~33.92% + reset 1785698280, weekly used ~9.50% NOT inverted + reset 1786285080); missing `DataV2.data.data` → `Parse`; missing percentage/reset fields → degrade gracefully (tier `used_pct`/`reset_at` = `None`).
  - secToken parse from `data.secToken` (present / missing).
  - URL + form-body construction (the `params` JSON, query params) — assert the built request matches the doc.
  - `usage_creds` populates `cookie` from the keyring (and the bailian no-AK/SK case returns `Some` with only the cookie) — `MemoryStore`/`SecretStoreHandle` test.
- **HTTP (wiremock, like the existing adapters):** the two-step flow — secToken endpoint returns `data.secToken`, usage endpoint returns the sample payload → assert the `UsageSnapshot` tiers; secToken 401 / missing → `AuthFailed`; usage non-2xx → error; cookie header present on both requests.
- **Frontend:** `npx vue-tsc --noEmit` (the `USAGE_ADAPTER_VENDORS` change).
- **Manual E2E (Windows, real account):** log into qianwenai in the capture window → cookie captured → dashboard chip shows live 5-hour/weekly % (not "不支持"); expire the cookie (or clear it) → "查询失败" + re-login prompt.

## Out of scope / follow-ups

1. **Supplementary APIs** (`BssOpenAPI-V3/GetSeatSubscriptionSummary`, `GetSubscriptionDetail`, the `quota-config` endpoint). The personal `subscription` endpoint **is now integrated** (best-effort plan type + validity — §Functional 2.3); the remaining supplementary APIs are still deferred. The doc states the main display does not depend on them; 加油包 (addon-pack) detail is deferred.
2. **Cookie-name allowlist trimming** (the stored jar currently includes all `platform.qianwenai.com` cookies; the doc only needs the login-state cookies).
3. **qianwenai SSO hosts** for the capture host-transition signal — the auth-cookie name is confirmed (`login_qianwenai_ticket`, now drives auto-detect); `PASSPORT_HOSTS` remains a best-effort additive signal.
4. **Multi-Aliyun-account** webview isolation (the capture session is shared across providers) — deferred per the capture spec.

## Capture-spec amendments

This spec amends `2026-08-02-bailian-usage-cookie-capture-design.md` in two places (the capture spec's text is now stale there):
- **Login URL/domain**: the capture spec's `LOGIN_URL`/`CONSOLE_URL`/`PASSPORT_HOSTS`/`SSO_COOKIE_NAMES` (Aliyun console) are corrected to the qianwenai platform here (§Capture login URL/domain correction). The capture *mechanism* (`WebviewWindow::cookies_for_url`, three-signal detection, keyring storage) is unchanged.
- **`UsageCreds`**: the capture spec's "UsageCreds is not extended — the cookie is a parallel keyring channel, keeping UsageCreds AK/SK-specific" is superseded — the cookie is now a `#[serde(skip)]` field on `UsageCreds`, loaded from that same keyring channel (§Cookie plumbing). The parallel keyring channel (`{provider_id}::usage_cookie`) itself is unchanged.
