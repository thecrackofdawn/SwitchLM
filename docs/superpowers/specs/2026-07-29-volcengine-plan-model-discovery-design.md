# Volcengine Plan Model Discovery Design

**Date:** 2026-07-29
**Status:** Approved
**Author:** Claude (SwitchLM Project)

## Overview

Implement the ability to fetch the list of supported models for a Volcengine (火山方舟 / Ark)
provider, so the model picker in **Models.vue** is populated for `volcengine-agent` and
`volcengine-coding` providers just like for any OpenAI-compatible provider.

Volcengine exposes two control-plane OpenAPI actions for this:

| Action | Subscription |
|---|---|
| `ListArkAgentPlanModel` | Agent Plan (AFP) |
| `ListArkCodingPlanModel` | Coding Plan |

Both return a `Datas[].ModelID` list and use the **same AK/SK Sig V4 signing path** already
implemented for usage querying — they are, as the user put it, "同一套接口" as `GetAFPUsage` /
`GetCodingPlanUsage`.

## Background: why the generic path doesn't work, and which host

Today `discover_models` does a generic OpenAI `GET {base}/models` with the **inference Bearer
key**. For Volcengine this is wrong:

1. It needs **AK/SK** signing (control-plane), not the Bearer inference key.
2. The data-plane `/models` returns inference *endpoint IDs*, not the *plan model IDs* a user
   picks (e.g. `doubao-seed-1.6`).

The control-plane host is `open.volcengineapi.com` (Volcengine Sig V4, `service=ark`,
`region=cn-beijing`) — exactly the host the existing **usage adapter** signs against and has
verified working. The model-list doc examples write `ark.cn-beijing.volces.com`, but that is the
data-plane host and a doc inconsistency; per the user (and by analogy with the working usage
adapter), the correct control-plane host is `open.volcengineapi.com`. The existing
`volcengine_openapi_call` already hardcodes this host, so it is reused **unchanged**.

## Requirements

### Functional

1. **Discover by provider type, one plan each.** Agent Plan and Coding Plan are two independent
   subscriptions, configured as two separate providers. Each provider maps to exactly **one**
   endpoint and **one** call:
   - `volcengine-agent` → `ListArkAgentPlanModel`
   - `volcengine-coding` → `ListArkCodingPlanModel`
   - There is **no union, no dedupe, no "try both"** — each provider is a single plan.

2. **Reuse usage credentials.** Discovery uses the same AccessKey ID + Secret the usage adapter
   uses (`usage_creds`: AK from config, SK from keyring) — *not* the inference Bearer key.

3. **Non-Volcengine providers unchanged.** All other providers keep the existing generic
   `GET {base}/models` Bearer path. A bare `volcengine` id (not creatable via the UI) is **not**
   special-cased and falls through to the generic path.

4. **Clear failure.** If AK/SK are missing or the call fails, return a clear error; the frontend
   already surfaces discovery errors and lets the user type a custom model id.

### Non-Functional

1. **No host refactor.** `open.volcengineapi.com` is already correct; signing code is reused as-is.
2. **No new trait.** Only Volcengine needs special handling, so dispatch is inline in
   `discover_models` (mirroring the current inline generic discovery). A single-impl trait would
   be premature.
3. **No frontend changes.** The `discover_models` → `DiscoveredModel { id }` contract is
   unchanged; `Models.vue` and `Provider.vue` are already wired.

## Implementation Architecture

### 1. Dispatch in `discover_models`

**File: `src-tauri/src/commands.rs`**

Add a Volcengine branch; everything else unchanged:

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

/// List models available on a provider. Volcengine providers use the control-plane plan-model
/// OpenAPI (AK/SK); everyone else uses the generic OpenAI `GET /models` (Bearer key from keyring).
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

### 2. Credential + base_url plumbing

**File: `src-tauri/src/commands.rs`** — new helper, mirrors `query_usage`'s reads:

```rust
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

### 3. The fetcher + parser

**File: `src-tauri/src/usage/volcengine.rs`** — reuse the private `volcengine_openapi_call`
(same file, same host `open.volcengineapi.com`) and its `VolcCall` result. Expose only
`list_plan_models` + `PlanModelError` as `pub`.

```rust
/// Plan-model listing errors, mapped to a `String` by the command layer.
#[derive(Debug, thiserror::Error)]
pub enum PlanModelError {
    #[error("{0}")] // Auth detail already includes the AK/SK hint from `volcengine_openapi_call`.
    Auth(String),
    #[error("网络错误：{0}")]
    Network(String),
    #[error("获取模型列表失败：{0}")]
    Other(String),
}

/// Query a Volcengine Ark plan's supported model list via the control-plane OpenAPI
/// (`ListArkAgentPlanModel` / `ListArkCodingPlanModel`). Same Sig V4 signing + host as the usage
/// adapter. `action` selects the plan — the caller passes exactly one (no union/merge).
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

/// Parse `ListArk*PlanModel` response: `Datas[].ModelID`, conventionally under `Result`.
/// Defensive on the wrapping (`Result` may be absent; `Datas` may sit at the top level — the
/// official doc's return example is a broken render) and on per-element shape (skip entries
/// missing or empty `ModelID`).
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

> **Module visibility:** `usage/mod.rs` must declare `pub mod volcengine;` (or `pub(crate)`)
> so `crate::usage::volcengine::list_plan_models` is reachable from `commands.rs`. The signing
> helpers (`volcengine_openapi_call`, `volcengine_region`, `volcengine_sign`, …) stay private.

### Data flow

```
Models.vue  ──config.discover(id)──▶  invoke("discover_models", { provider_id })
                                              │
                          volcengine_plan_action(id)?
                          ├── volcengine-agent/coding ──▶ discover_volcengine_models
                          │       │ state.usage_creds(id)  → AK (config) + SK (keyring)
                          │       │ config … openai_base_url → region (cn-beijing)
                          │       └─▶ usage::volcengine::list_plan_models(action)
                          │              volcengine_openapi_call(Sig V4, open.volcengineapi.com)
                          │              parse Result.Datas[].ModelID
                          │              ◀── Vec<String>
                          │       └─▶ Vec<DiscoveredModel { id }>
                          └── other ──▶ provider_endpoint + fetch_discovered_models (GET /models, Bearer)
                                              ◀── Vec<DiscoveredModel { id }>
```

## Error Handling

| Condition | Behavior |
|---|---|
| Provider unknown | `provider {id} not found` |
| AK or SK empty/missing | Chinese error guiding the user to fill usage credentials |
| Auth failure (HTTP 401/403, `AccessDenied`, `Signature…`) | `PlanModelError::Auth` (already carries the AK/SK hint) |
| Network/timeout | `PlanModelError::Network` |
| Soft error (4xx business error / non-JSON / empty) | `PlanModelError::Other`; empty `Datas` → empty list (not an error) |
| Wrong plan type for subscription (e.g. `volcengine-agent` but only Coding subscribed) | Likely empty `Datas` → empty list; user can still type a custom id |

Both plans share credentials, so an `Auth` result is terminal (no second endpoint to try — and
there is no second endpoint, since each provider is one plan).

## Testing Strategy

### Unit tests (deterministic; no live HTTP — matches existing style)

**`usage/volcengine.rs`:**

1. `parse_plan_models` from a realistic `{ Result: { Datas: [{ModelID}, …] } }` envelope.
2. Defensive: `Datas` at top level (no `Result` wrapper); entries missing / empty `ModelID`
   skipped.
3. Empty result when no `Datas` present.

**`commands.rs`:**

4. `volcengine_plan_action` mapping: `-agent` → Agent action, `-coding` → Coding action, bare
   `volcengine` / `openai` → `None`.

### Not separately tested

- **Signing correctness** is already covered by the existing `sign_structure_and_determinism`
  test; `volcengine_sign` is reused unchanged.
- **Live API correctness** (real envelope shape, real host) cannot be unit-tested without creds;
  verified manually by the user against a real Volcengine account (same caveat the usage adapter
  documents: "真实正确性靠用户实测").

## Files Changed

### Backend
1. `src-tauri/src/commands.rs` — branch in `discover_models`; add `volcengine_plan_action` +
   `discover_volcengine_models`.
2. `src-tauri/src/usage/volcengine.rs` — add `list_plan_models`, `parse_plan_models`,
   `PlanModelError`; unit tests.
3. `src-tauri/src/usage/mod.rs` — ensure `volcengine` module is `pub`/`pub(crate)`.

### Frontend
None.

## Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Control-plane host | `open.volcengineapi.com` (reuse usage adapter) | User confirmed "同一套接口"; already verified working for ark Sig V4 |
| Endpoint selection | One action per provider (`-agent`/`-coding`) | Plans are independent, configured as separate providers — no union needed |
| Bare `volcengine` id | Not special-cased (generic `/models`) | Not creatable via UI; avoids logic for a non-existent case |
| Abstraction | Inline dispatch, no new trait | Only Volcengine needs it; single-impl trait is premature |
| Credentials | Reuse `usage_creds` AK/SK | Same control-plane creds as usage; `Provider.vue` already collects them |
| Signer | Reused unchanged | Host already correct; no refactor |

## Success Criteria

1. ✅ `Models.vue` shows the plan model list for a `volcengine-agent` / `volcengine-coding`
   provider after entering AK/SK.
2. ✅ Non-Volcengine providers still discover via `GET /models` exactly as before.
3. ✅ Missing AK/SK yields a clear, actionable error (and the user can still type a custom id).
4. ✅ Auth/network/soft failures map to distinct, readable errors.
5. ✅ Unit tests pass for parsing + action mapping.
6. ✅ No frontend changes required; no host/signing refactor.

## Out of Scope

- A `ModelListProvider` trait / registry (premature — only one provider needs special handling).
- Host parameterization / multi-host fallback (host is already correct).
- Changing the generic Bearer `/models` path for other providers.
- Auto-detecting which plan a credential is subscribed to (the provider type already encodes it).
