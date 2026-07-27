# Volcengine Plan Model Discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Populate the model picker for `volcengine-agent` / `volcengine-coding` providers by fetching each plan's supported model list from the Volcengine control-plane OpenAPI.

**Architecture:** Add a Volcengine branch to the existing `discover_models` Tauri command. The branch reuses the **already-working** Sig V4 signing path (`volcengine_openapi_call`, host `open.volcengineapi.com`) and the **usage AK/SK** credentials. Each provider maps to exactly one OpenAPI action — `volcengine-agent` → `ListArkAgentPlanModel`, `volcengine-coding` → `ListArkCodingPlanModel` — no union/dedupe. Non-Volcengine providers keep the generic OpenAI `GET /models` path.

**Tech Stack:** Rust (Tauri backend), `reqwest`, `hmac`/`sha2` (existing signing), `thiserror`, `serde_json`. Frontend unchanged.

**Spec:** `docs/superpowers/specs/2026-07-29-volcengine-plan-model-discovery-design.md`

## Global Constraints

- **Host is fixed `open.volcengineapi.com`.** Do NOT touch `volcengine_sign` / `volcengine_openapi_call` / `VOLCENGINE_OPENAPI_HOST` — they are reused unchanged (already verified working for ark Sig V4 by the usage adapter).
- **One action per provider, no merging.** `volcengine-agent` → Agent action only; `volcengine-coding` → Coding action only; bare `volcengine` / anything else → generic `/models` (not special-cased).
- **Credentials:** reuse `state.usage_creds(provider_id)` (AK from config, SK from keyring) — the same creds as usage, *not* the inference Bearer key.
- **Error strings** that surface to the user are in Chinese (codebase convention; see existing `bind_error_message`).
- **`pub mod volcengine;` already exists** in `src-tauri/src/usage/mod.rs:2` — no mod.rs change needed; only new items must be `pub`.
- **Tests run from the repo root** via `cargo test --manifest-path src-tauri/Cargo.toml <filter>`.
- **Signing correctness is already covered** by the existing `sign_structure_and_determinism` test — do not add a new signing test. The live-HTTP path (`list_plan_models`) is verified manually (same caveat the usage adapter documents); the unit-tested surface is `parse_plan_models` + `volcengine_plan_action`.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `src-tauri/src/usage/volcengine.rs` | Fetch + parse a Volcengine plan's model list (reuses private signer) | Add `PlanModelError`, `list_plan_models`, `parse_plan_models` + unit tests |
| `src-tauri/src/commands.rs` | Dispatch Volcengine providers in `discover_models`; credential plumbing | Add `volcengine_plan_action`, `discover_volcengine_models`; branch in `discover_models`; add unit test |

No other files change. No frontend changes.

---

## Task 1: Volcengine plan-model fetcher + parser

**Files:**
- Modify: `src-tauri/src/usage/volcengine.rs` — add a "Plan model listing" section (e.g. after `volcengine_openapi_call`, which ends around line 293) and tests in the existing `#[cfg(test)] mod tests` block (starts line 511, uses `use super::*;` + `use serde_json::json;`).

**Interfaces:**
- Consumes: the private `volcengine_openapi_call(region, ak, sk, action) -> VolcCall` (same file, line 212), `volcengine_region(base_url) -> String` (line 146), `VolcCall::{Body,Auth,Transient,Soft}` (line 199), `VOLCENGINE_AKSK_HINT` (line 37), `Value` (imported line 19).
- Produces (used by Task 2): `pub async fn list_plan_models(base_url: &str, access_key_id: &str, secret_access_key: &str, action: &str) -> Result<Vec<String>, PlanModelError>` and `pub enum PlanModelError { Auth(String), Network(String), Other(String) }`.

- [ ] **Step 1: Write the failing tests**

Add these three tests to the existing `mod tests` block in `src-tauri/src/usage/volcengine.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml plan_models`
Expected: compile error — `cannot find function parse_plan_models in this scope`.

- [ ] **Step 3: Write the implementation**

Add this block to `src-tauri/src/usage/volcengine.rs` (after `volcengine_openapi_call`, before the response-parsing section):

```rust
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
/// `ListArkCodingPlanModel`）。复用量量适配器的 Sig V4 签名与 host。
/// `action` 选定套餐——调用方只传一个（Agent / Coding 独立，不合并）。
pub async fn list_plan_models(
    base_url: &str,
    access_key_id: &str,
    secret_access_key: &str,
    action: &str,
) -> Result<Vec<String>, PlanModelError> {
    let region = volcengine_region(base_url);
    match volcengine_openapi_call(&region, access_key_id, secret_access_key, action).await {
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml plan_models`
Expected: PASS (3 tests). Also confirm the whole module still compiles/passes: `cargo test --manifest-path src-tauri/Cargo.toml volcengine` → all green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/usage/volcengine.rs
git commit -m "feat(volcengine): add plan-model list fetcher + parser"
```

---

## Task 2: Wire Volcengine into `discover_models`

**Files:**
- Modify: `src-tauri/src/commands.rs` — change `discover_models` (lines 167-175) and add two helpers next to it; add a test to the existing `#[cfg(test)] mod tests` block (line 756, uses `use super::*;`).

**Interfaces:**
- Consumes (from Task 1): `crate::usage::volcengine::list_plan_models(base_url, ak, sk, action) -> Result<Vec<String>, PlanModelError>` and `PlanModelError`. Also `state.usage_creds(provider_id) -> Option<UsageCreds>` (proxy/state.rs:81), `state.config.read().await` → `cfg.providers`, `DiscoveredModel { id: String }` (commands.rs:162).
- Produces: none (terminal wiring — the `discover_models` command now returns Volcengine plan models).

- [ ] **Step 1: Write the failing test**

Add this test to the existing `mod tests` block in `src-tauri/src/commands.rs`:

```rust
    #[test]
    fn volcengine_plan_action_mapping() {
        // 每个火山 provider 对应唯一一个套餐接口；裸 volcengine / 非火山走通用 /models。
        assert_eq!(volcengine_plan_action("volcengine-agent"), Some("ListArkAgentPlanModel"));
        assert_eq!(volcengine_plan_action("volcengine-coding"), Some("ListArkCodingPlanModel"));
        assert_eq!(volcengine_plan_action("volcengine"), None);
        assert_eq!(volcengine_plan_action("openai"), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml volcengine_plan_action_mapping`
Expected: compile error — `cannot find function volcengine_plan_action`.

- [ ] **Step 3: Write the implementation**

**3a.** Replace `discover_models` (lines 167-175) with:

```rust
/// List models available on a provider. Volcengine providers use the control-plane plan-model
/// OpenAPI (AK/SK Sig V4, same as usage); everyone else uses the generic OpenAI
/// `GET {base}/models` (Bearer key from keyring).
#[tauri::command]
pub async fn discover_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<Vec<DiscoveredModel>, String> {
    if let Some(action) = volcengine_plan_action(&provider_id) {
        return discover_volcengine_models(&state, &provider_id, action).await;
    }
    let (base_url, api_key) = provider_endpoint(&state, &provider_id).await?;
    fetch_discovered_models(&base_url, api_key.as_deref()).await
}
```

**3b.** Add these two helpers to `src-tauri/src/commands.rs` (in the model-discovery section, e.g. right after `discover_models`):

```rust
/// Returns the Ark plan-model OpenAPI `Action` for a Volcengine provider, else `None`
/// (non-Volcengine / bare `volcengine` → generic OpenAI `/models` path). Agent and Coding are
/// independent subscriptions configured as separate providers, so each maps to exactly one action.
fn volcengine_plan_action(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "volcengine-agent" => Some("ListArkAgentPlanModel"),
        "volcengine-coding" => Some("ListArkCodingPlanModel"),
        _ => None,
    }
}

/// Fetch a Volcengine provider's plan model list via the control-plane OpenAPI. Uses the same
/// AK/SK as the usage adapter (AK from config, SK from keyring). `base_url` only feeds region
/// derivation (defaults to `cn-beijing` when absent).
async fn discover_volcengine_models(
    state: &AppState,
    provider_id: &str,
    action: &str,
) -> Result<Vec<DiscoveredModel>, String> {
    let usage_creds = state
        .usage_creds(provider_id)
        .await
        .ok_or_else(|| format!("provider {provider_id} not found"))?;
    let ak = usage_creds.access_key_id.as_deref().map(str::trim).unwrap_or("");
    let sk = usage_creds
        .secret_access_key
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if ak.is_empty() || sk.is_empty() {
        return Err(
            "火山模型列表需要 AccessKey ID + Secret（用量凭据，与推理 API Key 不同）\
             —— 请先在服务商设置里填写。"
                .to_string(),
        );
    }
    let base_url = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("provider {provider_id} not found"))?
            .openai_base_url
            .clone()
            .unwrap_or_default()
    };
    let ids = crate::usage::volcengine::list_plan_models(&base_url, ak, sk, action)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ids.into_iter().map(|id| DiscoveredModel { id }).collect())
}
```

- [ ] **Step 4: Run tests + build to verify**

Run: `cargo test --manifest-path src-tauri/Cargo.toml volcengine_plan_action_mapping` → PASS.
Run: `cargo build --manifest-path src-tauri/Cargo.toml` → compiles (confirms the wiring + that `state.usage_creds` / `state.config` resolve on `&AppState`, mirroring the existing `query_usage`).
Run full suite to confirm no regressions: `cargo test --manifest-path src-tauri/Cargo.toml` → all green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat(volcengine): discover plan models in discover_models"
```

---

## Manual Verification (requires a real Volcengine account — not automatable headlessly)

The signing path is reused from the usage adapter (already proven), but the **live model-list response shape** and **host** can only be confirmed against a real account — same caveat the usage adapter documents ("真实正确性靠用户实测"). After both tasks land:

1. **Build & run the app.** From repo root: `pnpm tauri dev` (or the project's run command).
2. **Configure a `volcengine-coding` (or `-agent`) provider** in Provider.vue with a valid AccessKey ID + Secret (the usage AK/SK fields).
3. **Open Models.vue for that provider** and trigger discovery (it auto-runs on form open / provider change).
4. **Expected:** the model dropdown populates with the plan's models (e.g. `doubao-seed-1.6`, …).
5. **If discovery fails:**
   - *Auth error* → the AK/SK lacks Ark OpenAPI permission, or wrong host. Re-check credentials; if the host turns out to be wrong (i.e. `open.volcengineapi.com` is rejected for these actions), revisit the host decision — the signer is the single place to change (`VOLCENGINE_OPENAPI_HOST`).
   - *Empty list but no error* → likely the account has the *other* plan; switch the provider type (`-agent` ↔ `-coding`).
6. Confirm a non-Volcengine provider still discovers normally (regression check on the generic path).
