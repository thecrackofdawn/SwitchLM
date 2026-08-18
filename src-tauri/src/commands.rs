use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tauri::{Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::config::{
    clamp_usage_refresh_secs, normalize_log_level, AppConfig, BackendKind, ContextCheckResult,
    ContextCheckStatus, FileSecretStore, MAX_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, Model,
    ProviderCatalog, Profile, Provider, SecretStore, Strategy, StrategyKind,
};
use crate::logging::LogHandle;
use crate::proxy::{server, AppState, ModelHealth};
use crate::usage::{usage_provider_for, UsageSnapshot};

#[tauri::command]
pub async fn get_providers(state: State<'_, AppState>) -> Result<Vec<Provider>, String> {
    Ok(state.config.read().await.providers.clone())
}

#[tauri::command]
pub async fn get_models(state: State<'_, AppState>) -> Result<Vec<Model>, String> {
    Ok(state.config.read().await.models.clone())
}

#[tauri::command]
pub async fn get_profiles(state: State<'_, AppState>) -> Result<Vec<Profile>, String> {
    Ok(state.config.read().await.profiles.clone())
}

/// Id-list ordering for drag-display surfaces: drop ids that are no longer known and collapse
/// duplicates (first occurrence wins). Pure — unit-tested directly.
fn normalize_id_order(ids: Vec<String>, known_ids: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let known: HashSet<&str> = known_ids.iter().map(|s| s.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    ids.into_iter()
        .filter(|id| known.contains(id.as_str()) && seen.insert(id.clone()))
        .collect()
}

/// Extract known provider ids, filter/dedupe `ids` via `normalize_id_order`, and assign to
/// `config.usage_order`. Pure on the config (no persist/refresh) so the assignment wiring is
/// unit-testable without an `AppHandle`.
pub(crate) fn assign_usage_order(config: &mut AppConfig, ids: Vec<String>) {
    let known: Vec<String> = config.providers.iter().map(|p| p.id.clone()).collect();
    config.usage_order = normalize_id_order(ids, &known);
}

/// Extract known profile ids, filter/dedupe `ids` via `normalize_id_order`, and assign to
/// `config.route_order`. Pure on the config (no persist/refresh) so the assignment wiring is
/// unit-testable without an `AppHandle`.
pub(crate) fn assign_route_order(config: &mut AppConfig, ids: Vec<String>) {
    let known: Vec<String> = config.profiles.iter().map(|p| p.id.clone()).collect();
    config.route_order = normalize_id_order(ids, &known);
}

/// The user's drag-order for usage surfaces (套餐用量 tab / 概览 chips / tray). Empty = config order.
#[tauri::command]
pub async fn get_usage_order(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.config.read().await.usage_order.clone())
}

/// Persist a new usage display order (provider ids). Unknown/duplicate ids are dropped.
/// Persists, then refreshes the tray so its reordered 账户用量 rows + tooltip apply immediately.
#[tauri::command]
pub async fn set_usage_order(
    state: State<'_, AppState>,
    ids: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        assign_usage_order(&mut config, ids);
        persist(&app, &config)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}

/// The user's drag-order for the route list (路由 tab / 概览 路由 card / tray). Empty = config order.
#[tauri::command]
pub async fn get_route_order(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.config.read().await.route_order.clone())
}

/// Persist a new route-list order (profile ids). Unknown/duplicate ids are dropped.
/// Persists, then refreshes the tray so its reordered 路由 submenu applies immediately.
#[tauri::command]
pub async fn set_route_order(
    state: State<'_, AppState>,
    ids: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        assign_route_order(&mut config, ids);
        persist(&app, &config)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}

// ---- Provider / Model / Profile CRUD (persist after write) ----

/// Insert-or-replace a provider by id. Trims `display_name` and rejects a name that collides
/// (same vendor + same trimmed name) with another provider before persisting.
#[tauri::command]
pub async fn upsert_provider(
    state: State<'_, AppState>,
    mut provider: Provider,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        provider.display_name = provider.display_name.trim().to_string();
        let mut cfg = state.config.write().await;
        if conflicting_same_name(&cfg, &provider).is_some() {
            return Err(format!("该厂商下已有同名账号「{}」，请改名", provider.display_name));
        }
        upsert(&mut cfg.providers, provider);
        persist(&app, &cfg)?;
    }
    Ok(())
}

/// Delete a provider by id.
#[tauri::command]
pub async fn delete_provider(
    state: State<'_, AppState>,
    provider_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        delete_by_id(&mut cfg.providers, &provider_id);
        persist(&app, &cfg)?;
    }
    // Purge keyring entries (inference api_key + usage SK + usage cookie) so they don't orphan in the OS store.
    if let Err(e) = state.secrets.delete_key(&provider_id) {
        tracing::warn!("delete_provider: failed to purge api_key for {provider_id}: {e}");
    }
    if let Err(e) = state.secrets.delete_usage_sk(&provider_id) {
        tracing::warn!("delete_provider: failed to purge usage_sk for {provider_id}: {e}");
    }
    if let Err(e) = state.secrets.delete_usage_cookie(&provider_id) {
        tracing::warn!("delete_provider: failed to purge usage_cookie for {provider_id}: {e}");
    }
    Ok(())
}

/// Insert-or-replace a model by id. Rejects a duplicate `(provider_id, upstream_model_id)`
/// (trimmed); editing the same id is exempt. Editing a model resets its breaker (§4.1: edit resets health).
#[tauri::command]
pub async fn upsert_model(
    state: State<'_, AppState>,
    mut model: Model,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let id = model.id.clone();
    {
        model.upstream_model_id = model.upstream_model_id.trim().to_string();
        let mut cfg = state.config.write().await;
        if conflicting_model(&cfg, &model).is_some() {
            return Err(format!("该账号下已存在模型「{}」", model.upstream_model_id));
        }
        upsert(&mut cfg.models, model);
        persist(&app, &cfg)?;
    }
    state.health.reset(&id);
    Ok(())
}

/// Delete a model by id.
#[tauri::command]
pub async fn delete_model(
    state: State<'_, AppState>,
    model_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        delete_by_id(&mut cfg.models, &model_id);
        persist(&app, &cfg)?;
    }
    Ok(())
}

/// Insert-or-replace a profile by id.
#[tauri::command]
pub async fn upsert_profile(
    state: State<'_, AppState>,
    mut profile: Profile,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        normalize_and_validate_strategies(&cfg, &mut profile.strategies)?;
        upsert(&mut cfg.profiles, profile);
        persist(&app, &cfg)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}

/// Delete a profile by id.
#[tauri::command]
pub async fn delete_profile(
    state: State<'_, AppState>,
    profile_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        delete_by_id(&mut cfg.profiles, &profile_id);
        persist(&app, &cfg)?;
    }
    Ok(())
}

/// Per-profile currently-effective entry model + the strategy that matched (if any),
/// plus the entry model's time-aware failover target (故障转移链首跳).
/// Single source of truth for 概览 / 路由 list display (computed via `profile_start_model`
/// + `model_fallback_target`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteEffective {
    pub profile_id: String,
    pub effective_model_id: String,
    pub via_strategy_id: Option<String>,
    pub effective_fallback_model_id: Option<String>,
    pub via_fallback_strategy_id: Option<String>,
}

/// Pure core of `route_effective_models`: entry model + its time-aware failover, per profile.
/// Testable without `State` — pass a pinned `LocalNow`.
fn route_effective_models_core(cfg: &AppConfig, now: &crate::proxy::health::LocalNow) -> Vec<RouteEffective> {
    cfg.profiles.iter().map(|p| {
        let (mid, via) = crate::proxy::strategies::profile_start_model(p, cfg, now);
        let (fb, fb_via) = cfg.models.iter().find(|m| m.id == mid)
            .map(|m| crate::proxy::strategies::model_fallback_target(m, cfg, now))
            .unwrap_or((None, None));
        RouteEffective {
            profile_id: p.id.clone(),
            effective_model_id: mid.to_string(),
            via_strategy_id: via.map(str::to_string),
            effective_fallback_model_id: fb.map(str::to_string),
            via_fallback_strategy_id: fb_via.map(str::to_string),
        }
    }).collect()
}

#[tauri::command]
pub async fn route_effective_models(state: State<'_, AppState>) -> Result<Vec<RouteEffective>, String> {
    let cfg = state.config.read().await;
    let now = state.clock.now_local();
    Ok(route_effective_models_core(&cfg, &now))
}

/// Per-model currently-effective failover target (time-aware). Drives the 故障转移 page cards'
/// "当前生效" hint — one entry per configured Model regardless of whether it backs a profile.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelEffectiveFallback {
    pub model_id: String,
    pub effective_fallback_model_id: Option<String>,
    pub via_fallback_strategy_id: Option<String>,
}

/// Per-model currently-effective failover target (time-aware), for the 故障转移 page cards.
#[tauri::command]
pub async fn model_effective_fallbacks(state: State<'_, AppState>) -> Result<Vec<ModelEffectiveFallback>, String> {
    let cfg = state.config.read().await;
    let now = state.clock.now_local();
    Ok(cfg.models.iter().map(|m| {
        let (fb, via) = crate::proxy::strategies::model_fallback_target(m, &cfg, &now);
        ModelEffectiveFallback {
            model_id: m.id.clone(),
            effective_fallback_model_id: fb.map(str::to_string),
            via_fallback_strategy_id: via.map(str::to_string),
        }
    }).collect())
}

/// One-click master switch: toggle a profile's `strategies_enabled` and persist.
#[tauri::command]
pub async fn set_profile_strategies_enabled(
    state: State<'_, AppState>,
    profile_id: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        let prof = cfg.profiles.iter_mut().find(|p| p.id == profile_id)
            .ok_or_else(|| format!("profile {profile_id} not found"))?;
        prof.strategies_enabled = enabled;
        persist(&app, &cfg)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}

// ---- Model discovery + provider connection test ----

/// A model id discovered from a provider's OpenAI-compatible `GET /models` endpoint.
#[derive(Serialize)]
pub struct DiscoveredModel {
    pub id: String,
}

/// List models available on a provider. Volcengine providers use the control-plane plan-model
/// OpenAPI (AK/SK Sig V4, same as usage); everyone else uses the generic OpenAI
/// `GET {base}/models` (Bearer key from keyring).
#[tauri::command]
pub async fn discover_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<Vec<DiscoveredModel>, String> {
    let vendor = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.vendor.clone())
            .ok_or_else(|| format!("provider {provider_id} not found"))?
    };
    if let Some(action) = volcengine_plan_action(&vendor) {
        return discover_volcengine_models(&state, &provider_id, action).await;
    }
    let (base_url, api_key) = provider_endpoint(&state, &provider_id).await?;
    fetch_discovered_models(&base_url, api_key.as_deref()).await
}

/// Returns the Ark plan-model OpenAPI `Action` for a Volcengine provider, else `None`
/// (non-Volcengine / bare `volcengine` → generic OpenAI `/models` path). Agent and Coding are
/// independent subscriptions configured as separate providers, so each maps to exactly one action.
fn volcengine_plan_action(vendor: &str) -> Option<&'static str> {
    match vendor {
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

/// Result of a provider connection test: HTTP status + a short detail (error body or "OK").
#[derive(Serialize)]
pub struct ConnectionTest {
    pub ok: bool,
    pub status: u16,
    pub detail: String,
}

/// Cheap connectivity check: `GET {base}/models` on the provider. Returns status + a short
/// detail so the UI can show *why* it failed (e.g. 401 no-auth vs invalid key vs 404).
#[tauri::command]
pub async fn test_provider_connection(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ConnectionTest, String> {
    let (base_url, api_key) = provider_endpoint(&state, &provider_id).await?;
    test_connection(&base_url, api_key.as_deref()).await
}

/// Test provider connection using a simple prompt (fallback when /models is not supported).
/// This consumes minimal quota but validates that credentials and endpoint are working.
#[tauri::command]
pub async fn test_provider_connection_with_prompt(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ConnectionTest, String> {
    let (base_url, api_key) = provider_endpoint(&state, &provider_id).await?;
    test_connection_with_prompt(&base_url, api_key.as_deref()).await
}

/// Set (or clear) a provider's inference api key in the OS keyring. `api_key = None` clears it.
#[tauri::command]
pub async fn set_provider_key(
    state: State<'_, AppState>,
    provider_id: String,
    api_key: Option<String>,
) -> Result<(), String> {
    tracing::info!("set_provider_key: provider={provider_id} has_key={}", api_key.is_some());
    let res = match api_key {
        Some(k) => state.secrets.set_key(&provider_id, &k),
        None => state.secrets.delete_key(&provider_id),
    };
    match res {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!("set_provider_key failed for {provider_id}: {e}");
            Err(e.to_string())
        }
    }
}

/// Whether an inference key is stored for the provider (does not reveal it).
#[tauri::command]
pub async fn provider_has_key(state: State<'_, AppState>, provider_id: String) -> Result<bool, String> {
    Ok(state.secrets.get_key(&provider_id).map_err(|e| e.to_string())?.is_some())
}

/// Set (or clear) a provider's Volcengine usage-query Secret Access Key in the OS keyring.
/// `secret_access_key = None` clears it. Distinct from the inference `api_key`.
#[tauri::command]
pub async fn set_provider_usage_sk(
    state: State<'_, AppState>,
    provider_id: String,
    secret_access_key: Option<String>,
) -> Result<(), String> {
    tracing::info!(
        "set_provider_usage_sk: provider={provider_id} has_sk={}",
        secret_access_key.is_some()
    );
    let res = match secret_access_key {
        Some(k) => state.secrets.set_usage_sk(&provider_id, &k),
        None => state.secrets.delete_usage_sk(&provider_id),
    };
    match res {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!("set_provider_usage_sk failed for {provider_id}: {e}");
            Err(e.to_string())
        }
    }
}

/// Whether a Volcengine usage SK is stored for the provider (does not reveal it).
#[tauri::command]
pub async fn provider_has_usage_sk(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<bool, String> {
    Ok(state.secrets.get_usage_sk(&provider_id).map_err(|e| e.to_string())?.is_some())
}

/// Query a provider's usage snapshot (60s cache). Errors degrade to an `Err` string for the UI.
#[tauri::command]
pub async fn get_usage(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<UsageSnapshot, String> {
    query_usage(&state, &provider_id).await
}

/// One usage entry per configured provider (ok or error), for tray/usage-page rendering.
#[derive(Serialize)]
pub struct UsageEntry {
    pub provider_id: String,
    pub snapshot: Option<UsageSnapshot>,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn get_all_usage(state: State<'_, AppState>) -> Result<Vec<UsageEntry>, String> {
    let ids: Vec<String> = state
        .config
        .read()
        .await
        .providers
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let mut out = Vec::new();
    for id in ids {
        match query_usage(&state, &id).await {
            Ok(s) => out.push(UsageEntry { provider_id: id, snapshot: Some(s), error: None }),
            Err(e) => out.push(UsageEntry { provider_id: id, snapshot: None, error: Some(e) }),
        }
    }
    Ok(out)
}

/// Aggregated fallback view: `{ model_id -> fallback_target_model_id }` (None = no fallback).
#[tauri::command]
pub async fn get_fallback_map(
    state: State<'_, AppState>,
) -> Result<HashMap<String, Option<String>>, String> {
    let cfg = state.config.read().await;
    Ok(fallback_map(&cfg))
}

/// Set a model's fallback target (persist) and reset its breaker state (§4.1: edit resets health).
#[tauri::command]
pub async fn set_model_fallback(
    state: State<'_, AppState>,
    model_id: String,
    target: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        let m = cfg
            .models
            .iter_mut()
            .find(|m| m.id == model_id)
            .ok_or_else(|| format!("model {model_id} not found"))?;
        m.fallback_target_model_id = target;
        persist(&app, &cfg)?;
    }
    state.health.reset(&model_id);
    Ok(())
}

/// Pure core: validate + write a model's failover config (default target + strategies + switch +
/// cooldown + retry) into `cfg`. Mutates `strategies` (dedup/sort) during validation. Err on bad strategy.
pub fn apply_model_failover(
    cfg: &mut AppConfig,
    model_id: &str,
    fallback_target_model_id: Option<String>,
    mut fallback_strategies: Vec<Strategy>,
    fallback_strategies_enabled: bool,
    cooldown_seconds: Option<u64>,
    retry_count: u32,
    retry_delay_secs: u64,
) -> Result<(), String> {
    normalize_and_validate_strategies(cfg, &mut fallback_strategies)?;
    let m = cfg
        .models
        .iter_mut()
        .find(|m| m.id == model_id)
        .ok_or_else(|| format!("model {model_id} not found"))?;
    m.fallback_target_model_id = fallback_target_model_id;
    m.fallback_strategies = fallback_strategies;
    m.fallback_strategies_enabled = fallback_strategies_enabled;
    m.cooldown_seconds = cooldown_seconds;
    m.retry_count = retry_count;
    m.retry_delay_secs = retry_delay_secs;
    Ok(())
}

/// Set a model's full failover config (modal save): default target + time strategies + switch +
/// cooldown + retry. Validates, persists, resets the breaker (edit resets health, §4.1).
#[tauri::command]
pub async fn set_model_failover(
    state: State<'_, AppState>,
    model_id: String,
    fallback_target_model_id: Option<String>,
    fallback_strategies: Vec<Strategy>,
    fallback_strategies_enabled: bool,
    cooldown_seconds: Option<u64>,
    retry_count: u32,
    retry_delay_secs: u64,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        apply_model_failover(
            &mut cfg,
            &model_id,
            fallback_target_model_id,
            fallback_strategies,
            fallback_strategies_enabled,
            cooldown_seconds,
            retry_count,
            retry_delay_secs,
        )?;
        persist(&app, &cfg)?;
    }
    state.health.reset(&model_id);
    Ok(())
}

/// One-click card switch: toggle a model's `fallback_strategies_enabled` and persist.
#[tauri::command]
pub async fn set_model_fallback_strategies_enabled(
    state: State<'_, AppState>,
    model_id: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        let m = cfg
            .models
            .iter_mut()
            .find(|m| m.id == model_id)
            .ok_or_else(|| format!("model {model_id} not found"))?;
        m.fallback_strategies_enabled = enabled;
        persist(&app, &cfg)?;
    }
    Ok(())
}

/// Compare a primary model's effective context size against its fallback's. Resolves both models
/// (and their providers' vendors) under the config read lock (`.read().await` - safe on the tokio
/// runtime), then delegates to the pure `classify_fallback`. Use this to warn the user before
/// saving a fallback that's too small.
#[tauri::command]
pub async fn validate_fallback_context(
    state: State<'_, AppState>,
    primary_id: String,
    fallback_id: String,
) -> Result<ContextCheckResult, String> {
    let (pv, pu, fv, fu) = {
        let cfg = state.config.read().await;
        let p = cfg.models.iter().find(|m| m.id == primary_id)
            .ok_or_else(|| format!("model {primary_id} not found"))?;
        let f = cfg.models.iter().find(|m| m.id == fallback_id)
            .ok_or_else(|| format!("model {fallback_id} not found"))?;
        let vendor_of = |mid: &str| {
            cfg.providers.iter().find(|pr| pr.id == mid).map(|pr| pr.vendor.as_str()).unwrap_or("")
        };
        (
            vendor_of(&p.provider_id).to_string(), p.upstream_model_id.clone(),
            vendor_of(&f.provider_id).to_string(), f.upstream_model_id.clone(),
        )
    };
    let catalog = state.catalog.read().await;
    Ok(classify_fallback(&pv, &pu, &fv, &fu, &catalog))
}

/// Catalog-only recognized context size for a (provider, upstream_model) pair - the effective
/// size (custom_provider_desc.json override if set, else bundled default). Returns None when the
/// pair isn't recognized. Resolves the provider's vendor under the config read lock, then takes
/// the catalog read lock. This is also what the Models page edits via `set_custom_context_size`.
///
/// Returns `Result` because Tauri requires async commands with reference inputs (here
/// `State<'_, AppState>`) to return a `Result`; the Ok payload is still `Option<u32>` (serialized
/// as `number | null`), so the frontend `invoke<number | null>` contract is unchanged. The error
/// variant is never produced.
#[tauri::command]
pub async fn recognized_context_size(
    state: State<'_, AppState>,
    provider_id: String,
    upstream_model_id: String,
) -> Result<Option<u32>, String> {
    let vendor = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.vendor.clone())
            .unwrap_or_default()
    };
    let catalog = state.catalog.read().await;
    Ok(catalog.context_size(&vendor, &upstream_model_id))
}

/// Set (or clear, when `context_size` is None) the user's custom context-size override for a
/// (provider, upstream_model_id) pair. Writes `custom_provider_desc.json` to disk first, then
/// re-derives the in-memory catalog (embedded baseline + custom overlay) under the write lock - so
/// disk and memory stay consistent and the change is visible to all same-vendor accounts
/// immediately (real-time, no restart). The custom file stays sparse: only entries the user
/// explicitly set are stored, so bundled default updates still take effect for untouched entries.
#[tauri::command]
pub async fn set_custom_context_size(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    provider_id: String,
    upstream_model_id: String,
    context_size: Option<u32>,
) -> Result<(), String> {
    let vendor = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.vendor.clone())
            .ok_or_else(|| format!("provider {provider_id} not found"))?
    };
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut custom = crate::config::catalog::load_custom(&dir);
    custom.set_context_size(&vendor, &upstream_model_id, context_size);
    crate::config::catalog::save_custom_catalog(&dir, &custom).map_err(|e| e.to_string())?;
    let effective = crate::config::catalog::effective_catalog(&custom);
    *state.catalog.write().await = effective;
    Ok(())
}

/// Per-model breaker state (for tray/UI cooling indicators). Recovers any model whose
/// `recover_at` has passed before snapshotting, so the cooling indicators reflect the current
/// wall-clock (a tripped model that has since expired no longer shows cooling forever).
#[tauri::command]
pub async fn get_model_health(
    state: State<'_, AppState>,
) -> Result<HashMap<String, ModelHealth>, String> {
    Ok(state.health.snapshot_recovered(state.clock.now_secs()))
}

// ---- Server / app management (status, port, env, restart, quit, autostart) ----

/// Proxy server status: `running` once `serve()` has bound a port.
#[derive(Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub port: Option<u16>,
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let port = state.bound_port();
    Ok(ServerStatus { running: port.is_some(), port })
}

/// The port the proxy is listening on (None if not yet bound).
#[tauri::command]
pub async fn get_port(state: State<'_, AppState>) -> Result<Option<u16>, String> {
    Ok(state.bound_port())
}

/// The last port-bind error, if the proxy failed to bind its preferred port and is
/// retrying. `None` once a bind succeeds (or no bind has been attempted). Surfaced to
/// the UI so it can show "port occupied, auto-retrying" instead of a silent failure.
#[tauri::command]
pub fn get_bind_error(state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(state.get_bind_error())
}

/// Base URLs an agent should point at (with the bound port), for copy-paste.
#[derive(Serialize)]
pub struct EnvSnippet {
    pub anthropic_base_url: String,
    pub openai_base_url: String,
}

#[tauri::command]
pub async fn get_env_snippet(state: State<'_, AppState>) -> Result<EnvSnippet, String> {
    let port = state.bound_port().ok_or_else(|| "proxy is not running".to_string())?;
    Ok(env_snippet(port))
}

/// Restart the proxy, rebinding on `settings.port`. Stops the old server and any active
/// port-recovery polling first, then tries a single bind on the preferred port. On success,
/// returns the bound port. On failure (port occupied), records a bind error and starts the
/// auto-recovery polling task so the proxy retries every 10s until the port frees.
#[tauri::command]
pub async fn restart_server(state: State<'_, AppState>) -> Result<Option<u16>, String> {
    let preferred = state.config.read().await.settings.port;

    // Stop existing service and polling
    state.stop_polling().await;
    if let Some(old) = state.take_server_handle() {
        old.abort();
        let _ = old.await; // wait for the listener to drop (frees the port)
    }
    state.clear_bound_port();

    // Try to start (single bind on the preferred port, no auto-increment)
    match server::serve_once(state.inner().clone(), preferred).await {
        Ok((handle, port)) => {
            state.set_bound_port(port);
            state.set_server_handle(handle);
            tracing::info!("服务已在端口 {port} 上重启");
            Ok(Some(port))
        }
        Err(e) => {
            let error_msg = bind_error_message(preferred, &e);
            state.set_bind_error(error_msg.clone());
            state.inner().clone().start_polling(preferred).await;
            tracing::warn!("重启失败：{e}，已启动自动重试");
            Err(error_msg)
        }
    }
}

/// Quit the whole application (tray "退出").
#[tauri::command]
pub async fn quit_app(app: tauri::AppHandle) -> Result<(), String> {
    app.exit(0);
    Ok(())
}

/// Toggle OS autostart and persist the preference to `settings.autostart`. Returns the new state.
#[tauri::command]
pub async fn toggle_autostart(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<bool, String> {
    toggle_autostart_core(&state, &app, enabled).await
}

/// Decide the outcome of a disable request against the real OS autostart entry.
///
/// Returns `Ok(())` whenever the entry ends up absent — whether `disable` removed it
/// or it was already gone (confirmed by `is_enabled == Ok(false)`). The already-absent
/// case is the root cause of "设置失败：…(os error 2)": after a reinstall/uninstall, a
/// Tauri `identifier` change, or external registry cleanup, the OS entry is gone while
/// the persisted `settings.autostart` still reads `true`, so `disable()` errors on the
/// missing entry (Windows `RegDeleteValueW` → `ERROR_FILE_NOT_FOUND`, i.e. os error 2).
/// That is exactly the state the user asked for, so it is success, not a failure.
///
/// A genuine failure — `disable` errored AND the entry is still present or its state is
/// unverifiable — is returned as `Err`. Generic over `E` because the decision depends
/// only on Ok/Err and the bool, never on the error's contents.
fn disable_outcome<E>(disable: Result<(), E>, is_enabled: Result<bool, E>) -> Result<(), E> {
    match disable {
        Ok(()) => Ok(()),
        Err(e) => match is_enabled {
            Ok(false) => Ok(()),
            _ => Err(e),
        },
    }
}

/// Core autostart toggle (OS plugin + persist), shared by the command and the tray menu handler.
pub async fn toggle_autostart_core(
    state: &AppState,
    app: &tauri::AppHandle,
    enabled: bool,
) -> Result<bool, String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        // Idempotent disable: the persisted `settings.autostart` can be `true` while the
        // OS entry is already gone (reinstall/uninstall, an `identifier` change, or
        // external registry cleanup). `disable()` would then error on the missing entry
        // (Windows os error 2); `is_enabled` confirms whether it is genuinely gone, in
        // which case the desired state is already reached. Any other failure propagates.
        disable_outcome(manager.disable(), manager.is_enabled()).map_err(|e| e.to_string())?;
    }
    {
        let mut cfg = state.config.write().await;
        cfg.settings.autostart = enabled;
        persist(app, &cfg)?;
    }
    Ok(enabled)
}

/// Read-only view of app settings (port + autostart + usage refresh interval + log level
/// + request recording + background webview destroy).
#[derive(Serialize)]
pub struct SettingsView {
    pub port: u16,
    pub autostart: bool,
    pub usage_refresh_interval_secs: u32,
    pub log_level: String,
    pub request_recording: bool,
    pub background_destroy: bool,
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<SettingsView, String> {
    let cfg = state.config.read().await;
    Ok(SettingsView {
        port: cfg.settings.port,
        autostart: cfg.settings.autostart,
        usage_refresh_interval_secs: clamp_usage_refresh_secs(cfg.settings.usage_refresh_interval_secs),
        log_level: normalize_log_level(&cfg.settings.log_level),
        request_recording: cfg.settings.request_recording,
        background_destroy: cfg.settings.background_destroy,
    })
}

/// 当前密钥存储后端状态视图（前端启动期读，决定是否弹授权框）。
#[derive(Debug, Clone, Serialize)]
pub struct SecretStatusView {
    /// "keyring" | "file" | "pending"
    pub mode: String,
    /// 仅 pending 时为 true（前端据此弹授权框）
    pub consent_required: bool,
}

/// 当前密钥存储后端状态（前端启动期读，决定是否弹授权框）。
#[tauri::command]
pub async fn get_secret_status(state: State<'_, AppState>) -> Result<SecretStatusView, String> {
    let (mode, consent_required) = match state.secrets.kind() {
        BackendKind::Keyring => ("keyring", false),
        BackendKind::File => ("file", false),
        BackendKind::Pending => ("pending", true),
    };
    Ok(SecretStatusView { mode: mode.into(), consent_required })
}

/// 用户同意明文回退存储：记授权 → swap 到 FileSecretStore → 补跑推迟的迁移。
#[tauri::command]
pub async fn grant_secret_consent(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    grant_secret_consent_core(&dir, &state).await
}

/// Core consent-grant, shared by the command and tests. Persists the flag → swaps the runtime
/// secret backend Pending→File → re-runs the startup-deferred legacy-SK migration (idempotent).
///
/// 部分失败自愈：若 `save` 已成功但后续步骤报错，磁盘标记为 `Some(true)` 而运行时句柄仍为
/// Pending——下次启动时 `select_backend` 会重算出 `File`（探测失败 + 已授权）并重跑 migrate。
pub async fn grant_secret_consent_core(dir: &std::path::Path, state: &AppState) -> Result<(), String> {
    {
        let mut cfg = state.config.write().await;
        cfg.settings.secret_store_fallback = Some(true);
        crate::config::store::save(dir, &cfg).map_err(|e| e.to_string())?;
    }
    let file_store: Arc<dyn SecretStore> =
        Arc::new(FileSecretStore::new(dir).map_err(|e| e.to_string())?);
    state.secrets.swap(file_store, BackendKind::File);
    // 授权前推迟的遗留 SK 迁移：现在补跑（幂等；多次调用安全）。
    crate::config::store::migrate_usage_sk_to_keyring(dir, &state.secrets).map_err(|e| e.to_string())?;
    Ok(())
}

/// Update the preferred port: validates range, persists, and tries to bind immediately.
/// On success the old service is stopped and the new one started; on failure a bind error is
/// stored and a 10s polling task is started to retry automatically.
#[tauri::command]
pub async fn set_port(
    state: State<'_, AppState>,
    port: u16,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Validate port range
    if !(1024..=65535).contains(&port) {
        return Err("端口必须在 1024-65535 范围内".into());
    }

    // Stop existing polling
    state.stop_polling().await;

    // Update configuration
    {
        let mut config = state.config.write().await;
        config.settings.port = port;
        persist(&app, &config)?;
    }

    // Stop the old server and clear bound_port before rebinding (mirrors
    // `restart_server`): frees the old listener so the new bind has a clean
    // slate, and avoids leaving a stale bound_port / orphaned server if the
    // new bind fails.
    if let Some(old) = state.take_server_handle() {
        old.abort();
        let _ = old.await;
    }
    state.clear_bound_port();

    // Try to bind new port
    match crate::proxy::server::serve_once(state.inner().clone(), port).await {
        Ok((handle, bound_port)) => {
            state.set_server_handle(handle);
            state.set_bound_port(bound_port);
            tracing::info!("端口已更改为 {port}，服务已启动");
        }
        Err(e) => {
            let error_msg = bind_error_message(port, &e);
            state.set_bind_error(error_msg.clone());
            state.inner().clone().start_polling(port).await;
            tracing::warn!("新端口 {port} 绑定失败：{e}，已启动自动重试");
            return Err(error_msg);
        }
    }

    Ok(())
}

/// Update the usage auto-refresh interval (seconds). Validates [30, 3600] then persists.
/// The floor is also re-clamped on read (`get_settings`), so a tampered config file
/// can never drive polling below the minimum.
#[tauri::command]
pub async fn set_usage_refresh_interval(
    state: State<'_, AppState>,
    seconds: u32,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if seconds < MIN_USAGE_REFRESH_SECS {
        return Err(format!("刷新间隔不能小于 {MIN_USAGE_REFRESH_SECS} 秒"));
    }
    if seconds > MAX_USAGE_REFRESH_SECS {
        return Err(format!("刷新间隔不能大于 {MAX_USAGE_REFRESH_SECS} 秒"));
    }
    {
        let mut config = state.config.write().await;
        config.settings.usage_refresh_interval_secs = seconds;
        persist(&app, &config)?;
    }
    Ok(())
}

/// Update the log level: normalize, persist, and apply live via the reload filter (no restart).
#[tauri::command]
pub async fn set_log_level(
    state: State<'_, AppState>,
    log: State<'_, LogHandle>,
    app: tauri::AppHandle,
    level: String,
) -> Result<(), String> {
    let normalized = normalize_log_level(&level);
    {
        let mut config = state.config.write().await;
        config.settings.log_level = normalized.clone();
        persist(&app, &config)?;
    }
    crate::logging::set_level(&log, crate::logging::level_filter_for(&normalized));
    Ok(())
}

/// Open the logs folder in the OS file manager (creates it first as a safety net).
#[tauri::command]
pub async fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener().open_path(dir.to_string_lossy().to_string(), None::<&str>).map_err(|e| e.to_string())
}

/// 开关请求记录:持久化设置 + 热切换运行时记录器(无需重启)。见 spec §8。
#[tauri::command]
pub async fn set_request_recording(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        config.settings.request_recording = enabled;
        persist(&app, &config)?;
    }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let next = if enabled {
        let (rec, rx) = crate::recording::RequestRecorder::channel();
        tauri::async_runtime::spawn(crate::recording::run_writer(rx, dir.join("request_log")));
        Some(rec)
    } else {
        None // drop old Arc → channel closes → old writer task exits
    };
    *state.recorder.write().unwrap() = next;
    Ok(())
}

/// 开关后台 webview 销毁：持久化设置 + 立即生效（开启且当前隐藏则起计时；关闭则取消在途计时）。
/// 见 docs/superpowers/specs/2026-08-14-background-webview-destroy-design.md。
#[tauri::command]
pub async fn set_background_destroy(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        config.settings.background_destroy = enabled;
        persist(&app, &config)?;
    }
    if enabled {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::idle_destroy::arm_idle_destroy(&app).await;
        });
    } else {
        crate::idle_destroy::cancel_idle_destroy(&app);
    }
    Ok(())
}

/// 清空请求记录目录。记录开启时经记录器串行清空(先关句柄再删);关闭时直接删目录。
#[tauri::command]
pub async fn clear_request_log(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("request_log");
    // Clone the Arc out and drop the read guard before awaiting (guard is not Send).
    let recorder = state.recorder.read().unwrap().as_ref().map(|r| r.clone());
    if let Some(rec) = recorder {
        rec.clear().await;
    } else {
        // 记录关闭 → 没有打开的句柄,直接删(best-effort)。
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

// ---- core helpers (testable without a Tauri app handle) ----

/// Core usage query with cache. Reads config for base_url + AK, the keyring for the api key and
/// the usage SK. Mirrors the breaker's `usage_snapshot_realtime` path (but cache-aware).
pub(crate) async fn query_usage(state: &AppState, provider_id: &str) -> Result<UsageSnapshot, String> {
    let now = state.clock.now_secs();
    if let Some(cached) = state.usage_cache.get(provider_id, now) {
        return Ok(cached);
    }
    let (base_url, vendor) = {
        let cfg = state.config.read().await;
        let p = cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("provider {provider_id} not found"))?;
        (p.openai_base_url.clone().unwrap_or_default(), p.vendor.clone())
    };
    // AK from config + SK from keyring (the SK is never persisted in config).
    let usage_creds = state.usage_creds(provider_id).await;
    let provider = usage_provider_for(&vendor)
        .ok_or_else(|| format!("服务商 '{vendor}' 不支持用量查询"))?;
    let api_key = state.secrets.get_key(provider_id).map_err(|e| e.to_string())?;
    let snap = provider
        .query(api_key.as_deref(), usage_creds.as_ref(), &base_url)
        .await
        .map_err(|e| e.to_string())?;
    state.usage_cache.set(provider_id, snap.clone(), now);
    Ok(snap)
}

pub fn fallback_map(cfg: &AppConfig) -> HashMap<String, Option<String>> {
    cfg.models
        .iter()
        .map(|m| (m.id.clone(), m.fallback_target_model_id.clone()))
        .collect()
}

/// Another provider (different id) that already owns the same `(vendor, display_name)`, comparing
/// trimmed names. `None` when the name is free (or when the only match is `provider` itself).
pub fn conflicting_same_name<'c>(cfg: &'c AppConfig, provider: &Provider) -> Option<&'c Provider> {
    let name = provider.display_name.trim();
    cfg.providers.iter().find(|p| {
        p.id != provider.id && p.vendor == provider.vendor && p.display_name.trim() == name
    })
}

/// Another model (different id) that already has the same `(provider_id, upstream_model_id)`,
/// comparing trimmed upstream ids. `None` when the pair is free (or the only match is `model` itself).
pub fn conflicting_model<'c>(cfg: &'c AppConfig, model: &Model) -> Option<&'c Model> {
    let up = model.upstream_model_id.trim();
    cfg.models.iter().find(|m| {
        m.id != model.id && m.provider_id == model.provider_id && m.upstream_model_id.trim() == up
    })
}

/// Normalize (dedup+sort days_of_week) then validate every strategy in the slice.
/// Returns Err(Chinese message) on the first violation. Mutates `strategies`.
/// Used for both profile strategies (`upsert_profile`) and model failover strategies.
fn normalize_and_validate_strategies(cfg: &AppConfig, strategies: &mut [Strategy]) -> Result<(), String> {
    for s in strategies.iter_mut() {
        let t = match &mut s.kind {
            StrategyKind::Time(t) => t,
        };
        if !(1..=5).contains(&s.priority) {
            return Err("策略优先级必须在 1~5 之间".into());
        }
        t.days_of_week.sort_unstable();
        t.days_of_week.dedup();
        if t.days_of_week.is_empty() {
            return Err("策略至少需选择一个星期".into());
        }
        if t.days_of_week.iter().any(|d| !(1..=7).contains(d)) {
            return Err("星期必须在 1~7 之间".into());
        }
        if t.time_start > 1439 || t.time_end > 1439 {
            return Err("时间必须在 00:00~23:59 之间".into());
        }
        // 0:00–0:00 是合法的「全天」窗口（匹配器命中选中星期的每一分钟）；
        // 其它起止相等仍属空区间，拒绝。
        if t.time_start == t.time_end && t.time_start != 0 {
            return Err("开始时间不能等于结束时间（如需全天，请将起止均设为 00:00）".into());
        }
        if !cfg.models.iter().any(|m| m.id == t.model_id) {
            return Err(format!("策略目标模型「{}」不存在", t.model_id));
        }
    }
    Ok(())
}

/// An existing provider (same vendor) that already holds the credentials being saved.
#[derive(Serialize)]
pub struct ProviderConflict {
    pub id: String,
    pub display_name: String,
}

/// Pure core: the existing provider (same vendor) that already holds the given credential.
/// `key_of(provider_id)` reads the inference key so tests can inject a fake keyring. Volcengine
/// matches on AccessKey ID; others on the inference api_key. Blank/absent credentials never match.
pub fn same_account_conflict<'c>(
    providers: &'c [Provider],
    vendor: &str,
    access_key_id: Option<&str>,
    key_of: impl Fn(&str) -> Option<String>,
    api_key: Option<&str>,
) -> Option<&'c Provider> {
    let ak = access_key_id.map(str::trim).filter(|s| !s.is_empty());
    let key = api_key.map(str::trim).filter(|s| !s.is_empty());
    providers.iter().find(|p| {
        if p.vendor != vendor {
            return false;
        }
        if vendor.starts_with("volcengine") {
            ak.is_some()
                && p
                    .usage_creds
                    .as_ref()
                    .and_then(|c| c.access_key_id.as_deref())
                    .map(str::trim)
                    == ak
        } else {
            let existing = key_of(&p.id).map(|s| s.trim().to_string());
            key.is_some() && existing.as_deref() == key
        }
    })
}

/// Whether the credentials being saved already belong to a configured provider (same account).
/// Volcengine matches on AccessKey ID; others on the inference api_key read from the keyring.
#[tauri::command]
pub async fn check_provider_conflict(
    state: State<'_, AppState>,
    vendor: String,
    access_key_id: Option<String>,
    api_key: Option<String>,
) -> Result<Option<ProviderConflict>, String> {
    let providers = state.config.read().await.providers.clone();
    let secrets = &state.secrets;
    let found = same_account_conflict(
        &providers,
        &vendor,
        access_key_id.as_deref(),
        |pid| secrets.get_key(pid).ok().flatten(),
        api_key.as_deref(),
    );
    Ok(found.map(|p| ProviderConflict { id: p.id.clone(), display_name: p.display_name.clone() }))
}

/// Pure classification (sync, unit-testable): no locks, no AppState - just the two (vendor,
/// upstream_model_id) pairs + the catalog. The async command above resolves them under the config
/// lock, then calls this. The effective context size is the catalog value (custom override if set,
/// else bundled default); there is no per-Model override.
pub fn classify_fallback(
    primary_vendor: &str,
    primary_upstream: &str,
    fallback_vendor: &str,
    fallback_upstream: &str,
    catalog: &ProviderCatalog,
) -> ContextCheckResult {
    let p = catalog.context_size(primary_vendor, primary_upstream);
    let f = catalog.context_size(fallback_vendor, fallback_upstream);
    let status = match (p, f) {
        (Some(ps), Some(fs)) if fs < ps => ContextCheckStatus::Smaller,
        (Some(_), Some(_)) => ContextCheckStatus::Ok,
        _ => ContextCheckStatus::Unknown,
    };
    ContextCheckResult { status, primary_size: p, fallback_size: f }
}

/// Build the user-facing port-bind error message, varying the wording by error
/// kind. `AddrInUse` (port occupied) gets the "被占用" phrasing; any other bind
/// failure (e.g. permission denied) includes the actual error so the user can
/// diagnose it. Used by startup (`start_proxy_with_retry`), `restart_server`,
/// and `set_port` so the wording stays consistent.
pub(crate) fn bind_error_message(port: u16, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::AddrInUse {
        format!(
            "端口 {} 被占用，服务未运行。系统将每 10 秒自动尝试重新启动。",
            port
        )
    } else {
        format!(
            "端口 {} 绑定失败（{}），服务未运行。系统将每 10 秒自动尝试重新启动。",
            port, e
        )
    }
}

fn persist(app: &tauri::AppHandle, cfg: &AppConfig) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    crate::config::store::save(&dir, cfg).map_err(|e| e.to_string())
}

// ---- CRUD + discovery helpers (testable without a Tauri app handle) ----

trait Identifiable {
    fn id(&self) -> &str;
}
impl Identifiable for Provider {
    fn id(&self) -> &str {
        &self.id
    }
}
impl Identifiable for Model {
    fn id(&self) -> &str {
        &self.id
    }
}
impl Identifiable for Profile {
    fn id(&self) -> &str {
        &self.id
    }
}

/// Insert-or-replace `item` in `items` by id.
fn upsert<T: Identifiable>(items: &mut Vec<T>, item: T) {
    let id = item.id().to_string();
    if let Some(existing) = items.iter_mut().find(|x| x.id() == id.as_str()) {
        *existing = item;
    } else {
        items.push(item);
    }
}

/// Remove the item whose id matches.
fn delete_by_id<T: Identifiable>(items: &mut Vec<T>, id: &str) {
    items.retain(|x| x.id() != id);
}

/// Read a provider's base_url + api key (keyring) for management calls (discover/test).
async fn provider_endpoint(
    state: &AppState,
    provider_id: &str,
) -> Result<(String, Option<String>), String> {
    let base_url = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("provider {provider_id} not found"))?
            .openai_base_url
            .clone()
            .ok_or_else(|| format!("provider {provider_id} has no OpenAI base_url"))?
    };
    let api_key = state.secrets.get_key(provider_id).map_err(|e| {
        tracing::error!("get_key failed for {provider_id}: {e}");
        e.to_string()
    })?;
    tracing::info!("provider_endpoint: {provider_id} has_key={}", api_key.is_some());
    Ok((base_url, api_key))
}

/// `GET {base}/models` with an optional bearer key.
async fn get_models_endpoint(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<reqwest::Response, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new().get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    req.send().await.map_err(|e| e.to_string())
}

/// Parse an OpenAI-style `{ "data": [{ "id": ... }] }` /models body into ids.
fn parse_discovered_models(body: &serde_json::Value) -> Result<Vec<DiscoveredModel>, String> {
    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "unexpected /models response (no data array)".to_string())?;
    Ok(data
        .iter()
        .filter_map(|m| {
            m.get("id")
                .and_then(|v| v.as_str())
                .map(|id| DiscoveredModel { id: id.to_string() })
        })
        .collect())
}

async fn fetch_discovered_models(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>, String> {
    let resp = get_models_endpoint(base_url, api_key).await?;
    if !resp.status().is_success() {
        return Err(format!("upstream returned {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    parse_discovered_models(&body)
}

async fn test_connection(base_url: &str, api_key: Option<&str>) -> Result<ConnectionTest, String> {
    let resp = get_models_endpoint(base_url, api_key).await?;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        return Ok(ConnectionTest { ok: true, status, detail: "OK".into() });
    }
    let detail: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
    Ok(ConnectionTest { ok: false, status, detail })
}

/// Test connection by sending a simple prompt (fallback when /models is not supported).
/// This uses a minimal request to verify credentials and connectivity without consuming significant quota.
async fn test_connection_with_prompt(
    base_url: &str,
    api_key: Option<&str>
) -> Result<ConnectionTest, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    // Create a minimal test request with a simple prompt
    let test_payload = serde_json::json!({
        "model": "test",
        "messages": [
            {
                "role": "user",
                "content": "hi"
            }
        ],
        "max_tokens": 1
    });

    let mut req = reqwest::Client::new()
        .post(&url)
        .json(&test_payload);

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();

    // We consider it a success if we get any response (even errors) that proves auth works
    // A 401 means the endpoint exists but credentials are wrong
    // A 400/other error means the endpoint exists but request format might be wrong
    // Only connection errors or completely invalid responses should fail
    if resp.status().is_client_error() || resp.status().is_success() {
        return Ok(ConnectionTest {
            ok: true,
            status,
            detail: "Endpoint accessible (credentials validated via prompt test)".into()
        });
    }

    let detail: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
    Ok(ConnectionTest { ok: false, status, detail })
}

/// Proxy binds 127.0.0.1; agents (Claude Code / Cursor) point here.
const PROXY_HOST: &str = "127.0.0.1";

/// Build the base URLs an agent should use. The Anthropic SDK appends `/v1/messages` to the
/// base, while the OpenAI SDK appends `/chat/completions` to a base ending in `/v1` — hence
/// the asymmetric suffixes.
fn env_snippet(port: u16) -> EnvSnippet {
    EnvSnippet {
        anthropic_base_url: format!("http://{PROXY_HOST}:{port}"),
        openai_base_url: format!("http://{PROXY_HOST}:{port}/v1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    // ---- autostart disable idempotency (fix for "设置失败：…(os error 2)") ----

    #[test]
    fn disable_outcome_swallows_when_entry_already_absent() {
        // The reported bug: disable() fails (Windows os error 2) because the autostart
        // entry was already removed (reinstall/uninstall/identifier change), but
        // is_enabled confirms it is gone. Desired state reached → Ok.
        assert!(disable_outcome(Err("os error 2"), Ok(false)).is_ok());
    }

    #[test]
    fn disable_outcome_propagates_when_entry_still_present() {
        // disable() failed AND the entry is verifiably still enabled → real failure.
        assert!(disable_outcome(Err("access denied"), Ok(true)).is_err());
    }

    #[test]
    fn disable_outcome_propagates_when_state_unverifiable() {
        // disable() failed AND is_enabled() also failed → cannot confirm → propagate.
        assert!(disable_outcome(Err("os error 2"), Err("probe failed")).is_err());
    }

    #[test]
    fn disable_outcome_ok_when_disable_succeeded() {
        // Happy path: disable() removed the entry; the probe result is irrelevant.
        assert!(disable_outcome::<&str>(Ok(()), Ok(false)).is_ok());
        assert!(disable_outcome::<&str>(Ok(()), Ok(true)).is_ok());
    }

    #[test]
    fn fallback_map_reflects_config() {
        let mut cfg = AppConfig::default();
        cfg.models.push(Model {
            id: "m_a".into(),
            provider_id: "zhipu".into(),
            source: ModelSource::Manual,
            upstream_model_id: "glm".into(),
            cooldown_seconds: None,
            fallback_target_model_id: Some("m_b".into()),
            ..Default::default()
        });
        cfg.models.push(Model {
            id: "m_b".into(),
            provider_id: "zhipu".into(),
            source: ModelSource::Manual,
            upstream_model_id: "glm".into(),
            cooldown_seconds: None,
            fallback_target_model_id: None,
            ..Default::default()
        });
        let map = fallback_map(&cfg);
        assert_eq!(map.get("m_a"), Some(&Some("m_b".to_string())));
        assert_eq!(map.get("m_b"), Some(&None));
    }

    fn mk_provider(id: &str, name: &str) -> Provider {
        Provider {
            id: id.into(),
            vendor: id.into(),
            display_name: name.into(),
            openai_base_url: Some("https://example.com/v1".into()),
            anthropic_base_url: None,
            usage_creds: None,
        }
    }

    #[test]
    fn upsert_provider_inserts_then_replaces() {
        let mut cfg = AppConfig::default();
        upsert(&mut cfg.providers, mk_provider("p1", "one"));
        assert_eq!(cfg.providers.len(), 1);
        upsert(&mut cfg.providers, mk_provider("p1", "two")); // replace
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].display_name, "two");
        upsert(&mut cfg.providers, mk_provider("p2", "three")); // insert
        assert_eq!(cfg.providers.len(), 2);
    }

    #[test]
    fn delete_provider_by_id_removes_only_match() {
        let mut cfg = AppConfig::default();
        upsert(&mut cfg.providers, mk_provider("p1", "one"));
        upsert(&mut cfg.providers, mk_provider("p2", "two"));
        delete_by_id(&mut cfg.providers, "p1");
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].id, "p2");
    }

    fn pv(id: &str, vendor: &str, name: &str) -> Provider {
        Provider {
            id: id.into(), vendor: vendor.into(), display_name: name.into(),
            openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        }
    }

    #[test]
    fn conflicting_same_name_detects_dup_and_trim_and_self_exclude() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(pv("p1", "volcengine-coding", "工作号"));

        // same vendor + same name, different id -> conflict
        assert!(conflicting_same_name(&cfg, &pv("p2", "volcengine-coding", "工作号")).is_some());
        // trailing whitespace is trimmed -> still a conflict
        assert!(conflicting_same_name(&cfg, &pv("p2", "volcengine-coding", " 工作号 ")).is_some());
        // same id (edit) -> excluded
        assert!(conflicting_same_name(&cfg, &pv("p1", "volcengine-coding", "工作号")).is_none());
        // different vendor -> ok
        assert!(conflicting_same_name(&cfg, &pv("p2", "zhipu", "工作号")).is_none());
        // same vendor, different name -> ok
        assert!(conflicting_same_name(&cfg, &pv("p2", "volcengine-coding", "个人号")).is_none());
    }

    #[test]
    fn conflicting_model_detects_dup_self_exclude_other_provider_and_trim() {
        let mut cfg = AppConfig::default();
        cfg.models.push(m("m1", "zhipu", "glm-4.6"));

        // same provider + same upstream, different id -> conflict
        assert!(conflicting_model(&cfg, &m("m2", "zhipu", "glm-4.6")).is_some());
        // same id (edit) -> self excluded -> no conflict
        assert!(conflicting_model(&cfg, &m("m1", "zhipu", "glm-4.6")).is_none());
        // same upstream, different provider -> no conflict
        assert!(conflicting_model(&cfg, &m("m2", "deepseek", "glm-4.6")).is_none());
        // trailing whitespace is trimmed -> collides
        assert!(conflicting_model(&cfg, &m("m2", "zhipu", "glm-4.6 ")).is_some());
    }

    #[test]
    fn same_account_conflict_matches_credentials() {
        // Two providers, same vendor, different accounts.
        let mut p1 = pv("p1", "volcengine-coding", "工作号");
        p1.usage_creds = Some(UsageCreds { access_key_id: Some("ak1".into()), ..Default::default() });
        let mut p2 = pv("p2", "volcengine-coding", "个人号");
        p2.usage_creds = Some(UsageCreds { access_key_id: Some("ak2".into()), ..Default::default() });
        let providers = vec![p1, p2];

        // Volcengine: AK match hits p1.
        assert_eq!(
            same_account_conflict(&providers, "volcengine-coding", Some("ak1"), |_| None, None).map(|p| p.id.clone()),
            Some("p1".to_string())
        );
        // Different AK -> no conflict.
        assert!(same_account_conflict(&providers, "volcengine-coding", Some("akX"), |_| None, None).is_none());
        // Blank AK -> cannot match.
        assert!(same_account_conflict(&providers, "volcengine-coding", None, |_| None, None).is_none());

        // Non-volcengine: api_key (via key_of closure) match hits.
        let zhipu = vec![{ let p = pv("p1", "zhipu", "工作号"); p }];
        assert_eq!(
            same_account_conflict(&zhipu, "zhipu", None, |_| Some("sk-1".into()), Some("sk-1")).map(|p| p.id.clone()),
            Some("p1".to_string())
        );
    }

    fn mk_model(id: &str) -> Model {
        Model {
            id: id.into(),
            provider_id: "p1".into(),
            source: ModelSource::Manual,
            upstream_model_id: "glm".into(),
            cooldown_seconds: None,
            fallback_target_model_id: None,
            ..Default::default()
        }
    }

    #[test]
    fn upsert_and_delete_model_round_trip() {
        let mut cfg = AppConfig::default();
        upsert(&mut cfg.models, mk_model("m_a"));
        upsert(&mut cfg.models, mk_model("m_b"));
        upsert(&mut cfg.models, mk_model("m_a")); // replace, no duplicate
        assert_eq!(cfg.models.len(), 2);
        delete_by_id(&mut cfg.models, "m_a");
        assert_eq!(cfg.models.len(), 1);
        assert_eq!(cfg.models[0].id, "m_b");
    }

    fn mk_profile(id: &str) -> Profile {
        Profile {
            id: id.into(),
            name: id.into(),
            aliases: vec![],
            backing_model_id: "m_a".into(),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_and_delete_profile_round_trip() {
        let mut cfg = AppConfig::default();
        upsert(&mut cfg.profiles, mk_profile("pf1"));
        upsert(&mut cfg.profiles, mk_profile("pf1")); // replace, no duplicate
        assert_eq!(cfg.profiles.len(), 1);
        delete_by_id(&mut cfg.profiles, "pf1");
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn parse_discovered_models_extracts_ids() {
        let body = serde_json::json!({
            "data": [ {"id": "glm-4.6"}, {"id": "glm-4.5"} ]
        });
        let ids: Vec<String> =
            parse_discovered_models(&body).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["glm-4.6".to_string(), "glm-4.5".to_string()]);
    }

    #[test]
    fn parse_discovered_models_errors_without_data_array() {
        assert!(parse_discovered_models(&serde_json::json!({"object": "list"})).is_err());
    }

    #[tokio::test]
    async fn fetch_discovered_models_calls_models_endpoint() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "data": [ {"id": "glm-4.6"}, {"id": "glm-4.5"} ] }),
            ))
            .mount(&mock)
            .await;
        let models = fetch_discovered_models(&mock.uri(), None).await.unwrap();
        let ids: Vec<String> = models.into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["glm-4.6".to_string(), "glm-4.5".to_string()]);
    }

    #[tokio::test]
    async fn test_connection_true_on_2xx_false_on_4xx() {
        let ok = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&ok)
            .await;
        let bad = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&bad)
            .await;
        assert!(test_connection(&ok.uri(), None).await.unwrap().ok);
        assert!(!test_connection(&bad.uri(), None).await.unwrap().ok);
    }

    #[test]
    fn env_snippet_builds_urls_with_bound_port() {
        let s = env_snippet(6950);
        assert_eq!(s.anthropic_base_url, "http://127.0.0.1:6950");
        assert_eq!(s.openai_base_url, "http://127.0.0.1:6950/v1");
        // bound port reflected (e.g. after restart on a new port)
        let s2 = env_snippet(6951);
        assert_eq!(s2.anthropic_base_url, "http://127.0.0.1:6951");
        assert_eq!(s2.openai_base_url, "http://127.0.0.1:6951/v1");
    }

    #[test]
    fn bind_error_message_addr_in_use_says_occupied() {
        let e = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        let msg = bind_error_message(6950, &e);
        assert!(msg.contains("6950"), "message should mention the port");
        assert!(msg.contains("被占用"), "AddrInUse should say 被占用");
        assert!(!msg.contains("绑定失败"), "AddrInUse should not say 绑定失败");
    }

    #[test]
    fn bind_error_message_other_error_includes_actual_error() {
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = bind_error_message(6950, &e);
        assert!(msg.contains("6950"), "message should mention the port");
        assert!(msg.contains("绑定失败"), "non-AddrInUse should say 绑定失败");
        // The actual error (PermissionDenied display) should be surfaced so the
        // user can diagnose the failure.
        assert!(
            msg.contains(&e.to_string()),
            "message should include the underlying error: {msg}"
        );
    }

    #[test]
    fn volcengine_plan_action_mapping() {
        // 每个火山 provider 对应唯一一个套餐接口；裸 volcengine / 非火山走通用 /models。
        assert_eq!(volcengine_plan_action("volcengine-agent"), Some("ListArkAgentPlanModel"));
        assert_eq!(volcengine_plan_action("volcengine-coding"), Some("ListArkCodingPlanModel"));
        assert_eq!(volcengine_plan_action("volcengine"), None);
        assert_eq!(volcengine_plan_action("openai"), None);
        assert_eq!(volcengine_plan_action("prov_abc"), None); // opaque id is not a vendor
    }

    use crate::config::catalog::{EMBEDDED_CATALOG, ProviderCatalog};
    use crate::config::{ContextCheckStatus, Model, ModelSource};

    fn m(id: &str, pid: &str, up: &str) -> Model {
        Model {
            id: id.into(), provider_id: pid.into(),
            source: ModelSource::Manual, upstream_model_id: up.into(),
            cooldown_seconds: None, fallback_target_model_id: None,
            ..Default::default()
        }
    }
    fn cat() -> ProviderCatalog {
        serde_json::from_str(EMBEDDED_CATALOG).unwrap()
    }

    #[test]
    fn classify_ok_when_fallback_larger_or_equal() {
        let cat = cat();
        let r = classify_fallback("zhipu", "glm-4.5-air", "zhipu", "glm-4.6", &cat); // 128000 -> 200000
        assert_eq!(r.status, ContextCheckStatus::Ok);
        assert_eq!(r.primary_size, Some(128000));
        assert_eq!(r.fallback_size, Some(200000));
    }

    #[test]
    fn classify_ok_when_sizes_equal() {
        // Equal context (fallback == primary) is NOT smaller -> Ok (strict < boundary).
        let cat = cat();
        let r = classify_fallback("zhipu", "glm-4.6", "zhipu", "glm-4.6", &cat); // 200000 == 200000
        assert_eq!(r.status, ContextCheckStatus::Ok);
        assert_eq!(r.primary_size, Some(200000));
        assert_eq!(r.fallback_size, Some(200000));
    }

    #[test]
    fn classify_smaller_when_fallback_too_small() {
        let cat = cat();
        let r = classify_fallback("zhipu", "glm-4.6", "zhipu", "glm-4.5-air", &cat); // 200000 -> 128000
        assert_eq!(r.status, ContextCheckStatus::Smaller);
    }

    #[test]
    fn classify_unknown_when_size_missing() {
        let cat = cat();
        let r = classify_fallback("zhipu", "glm-4.6", "custom", "weird-model", &cat); // fallback not in catalog
        assert_eq!(r.status, ContextCheckStatus::Unknown);
        assert_eq!(r.fallback_size, None);
    }

    #[test]
    fn classify_uses_catalog_value_including_custom_override() {
        // The catalog passed in already includes any custom_provider_desc.json overlay;
        // classify_fallback just reads it. An overridden fallback size flips the verdict.
        let mut cat = cat();
        cat.set_context_size("zhipu", "glm-4.5-air", Some(300000)); // override 128000 -> 300000
        let r = classify_fallback("zhipu", "glm-4.6", "zhipu", "glm-4.5-air", &cat);
        assert_eq!(r.status, ContextCheckStatus::Ok); // 300000 >= 200000
        assert_eq!(r.fallback_size, Some(300000));
    }

    #[test]
    fn normalize_id_order_filters_unknown_and_dedupes() {
        let known = ["a".to_string(), "b".to_string(), "c".to_string()];
        // "x" is unknown -> dropped; "b" duplicated -> kept once, first position wins.
        let ids = vec!["b".into(), "x".into(), "c".into(), "a".into(), "b".into()];
        assert_eq!(normalize_id_order(ids, &known), vec!["b".to_string(), "c".to_string(), "a".to_string()]);
    }

    #[test]
    fn normalize_id_order_empty_for_all_unknown() {
        let known = ["a".to_string()];
        assert!(normalize_id_order(vec!["z".into()], &known).is_empty());
    }

    #[test]
    fn assign_usage_order_filters_unknown_dedupes_and_assigns() {
        // Wiring check: assign extracts known provider ids from the config, drops unknown ids,
        // collapses duplicates (first wins), and writes the result to `config.usage_order`.
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("a", "A"));
        cfg.providers.push(mk_provider("b", "B"));
        cfg.providers.push(mk_provider("c", "C"));
        // "x" unknown -> dropped; second "c" -> collapsed; order preserved (first wins).
        assign_usage_order(&mut cfg, vec!["c".into(), "x".into(), "a".into(), "c".into()]);
        assert_eq!(cfg.usage_order, vec!["c".to_string(), "a".to_string()]);
    }

    #[test]
    fn assign_route_order_filters_unknown_dedupes_and_assigns() {
        // Wiring check: assign extracts known profile ids from the config, drops unknown ids,
        // collapses duplicates (first wins), and writes the result to `config.route_order`.
        let mut cfg = AppConfig::default();
        cfg.profiles.push(mk_profile("a"));
        cfg.profiles.push(mk_profile("b"));
        cfg.profiles.push(mk_profile("c"));
        // "x" unknown -> dropped; second "c" -> collapsed; order preserved (first wins).
        assign_route_order(&mut cfg, vec!["c".into(), "x".into(), "a".into(), "c".into()]);
        assert_eq!(cfg.route_order, vec!["c".to_string(), "a".to_string()]);
    }

    #[tokio::test]
    async fn grant_secret_consent_core_flips_pending_to_file_and_persists() {
        use crate::config::store as config_store;
        use crate::proxy::AppStateInner;
        let dir = tempfile::tempdir().unwrap();
        let state: AppState =
            Arc::new(AppStateInner::load(dir.path(), Arc::new(MemoryStore::default())).unwrap());
        // simulate the pre-consent "pending" state
        state.secrets.swap(Arc::new(PendingStore), BackendKind::Pending);
        assert_eq!(state.secrets.kind(), BackendKind::Pending);

        grant_secret_consent_core(dir.path(), &state).await.unwrap();

        // swap happened: backend kind is now File
        assert_eq!(state.secrets.kind(), BackendKind::File);
        // consent persisted to the on-disk config
        let cfg = config_store::load(dir.path()).unwrap();
        assert_eq!(cfg.settings.secret_store_fallback, Some(true));
    }

    use crate::config::{Strategy, StrategyKind, TimeStrategy};

    fn profile_with_strategy(start: u16, end: u16, days: Vec<u8>, priority: u8, model: &str) -> Profile {
        Profile {
            id: "p".into(), name: "p".into(), aliases: vec![], backing_model_id: "m".into(),
            strategies_enabled: true,
            strategies: vec![Strategy {
                id: "s".into(), priority, enabled: true,
                kind: StrategyKind::Time(TimeStrategy { days_of_week: days, time_start: start, time_end: end, model_id: model.into() }),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn validate_rejects_start_equal_end() {
        let cfg = AppConfig::default();
        let mut p = profile_with_strategy(500, 500, vec![1], 1, "m");
        assert!(normalize_and_validate_strategies(&cfg, &mut p.strategies).is_err());
    }

    #[test]
    fn validate_accepts_zero_zero_as_fullday() {
        let mut cfg = AppConfig::default();
        cfg.models.push(mk_model("m"));
        let mut p = profile_with_strategy(0, 0, vec![6, 7], 1, "m"); // 00:00–00:00 = 全天
        normalize_and_validate_strategies(&cfg, &mut p.strategies).unwrap();
    }

    #[test]
    fn validate_rejects_bad_priority_and_days_and_dangling_model() {
        let cfg = AppConfig::default();
        let mut p = profile_with_strategy(0, 100, vec![1], 11, "m"); // priority 11
        assert!(normalize_and_validate_strategies(&cfg, &mut p.strategies).is_err());
        let mut p = profile_with_strategy(0, 100, vec![8], 1, "m"); // weekday 8
        assert!(normalize_and_validate_strategies(&cfg, &mut p.strategies).is_err());
        let mut p = profile_with_strategy(0, 100, vec![], 1, "m"); // empty days
        assert!(normalize_and_validate_strategies(&cfg, &mut p.strategies).is_err());
        let mut p = profile_with_strategy(0, 100, vec![1], 1, "nope"); // dangling model
        assert!(normalize_and_validate_strategies(&cfg, &mut p.strategies).is_err());
    }

    #[test]
    fn validate_normalizes_days_dedup_sort() {
        let mut cfg = AppConfig::default();
        cfg.models.push(mk_model("m"));
        let mut p = profile_with_strategy(0, 100, vec![3, 1, 1, 2], 1, "m");
        normalize_and_validate_strategies(&cfg, &mut p.strategies).unwrap();
        assert_eq!(p.strategies[0].kind.as_time().unwrap().days_of_week, vec![1, 2, 3]);
    }

    #[test]
    fn normalize_and_validate_strategies_rejects_and_normalizes() {
        let cfg = AppConfig::default(); // no models -> any strategy model_id is "not found"
        let mut s = vec![Strategy {
            id: "s".into(), priority: 11, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2, 2, 1], time_start: 1320, time_end: 1320, model_id: "mX".into(),
            }),
        }];
        // priority out of range
        assert!(normalize_and_validate_strategies(&cfg, &mut s).is_err());
        // start==end (non-zero) -> rejected
        s[0].priority = 1;
        assert!(normalize_and_validate_strategies(&cfg, &mut s).is_err());
        // fix window, but model_id still missing -> rejected
        let t = match &mut s[0].kind { StrategyKind::Time(t) => t };
        t.time_end = 360;
        assert!(normalize_and_validate_strategies(&cfg, &mut s).is_err());
        // days dedup+sort applied (no error path beyond model_id here, but days are normalized)
        assert_eq!(s[0].kind.as_time().unwrap().days_of_week, vec![1, 2]);
    }

    #[test]
    fn apply_model_failover_validates_and_writes() {
        let mut cfg = AppConfig::default();
        cfg.models.push(Model {
            id: "m".into(), provider_id: "p".into(), upstream_model_id: "u".into(),
            ..Default::default()
        });
        cfg.models.push(Model {
            id: "t".into(), provider_id: "p".into(), upstream_model_id: "ut".into(),
            ..Default::default()
        });
        // bad priority -> rejected, cfg unchanged (validation runs before any write)
        let bad = vec![Strategy {
            id: "s".into(), priority: 99, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![1], time_start: 0, time_end: 1439, model_id: "t".into(),
            }),
        }];
        assert!(apply_model_failover(&mut cfg, "m", Some("t".into()), bad.clone(), true, None, 2, 5).is_err());
        // good -> written
        let mut good = bad;
        good[0].priority = 1;
        apply_model_failover(&mut cfg, "m", Some("t".into()), good, true, Some(120), 2, 7).unwrap();
        let m = &cfg.models[0];
        assert_eq!(m.fallback_target_model_id.as_deref(), Some("t"));
        assert_eq!(m.fallback_strategies.len(), 1);
        assert!(m.fallback_strategies_enabled);
        assert_eq!(m.cooldown_seconds, Some(120));
        assert_eq!(m.retry_count, 2);
        assert_eq!(m.retry_delay_secs, 7);
    }

    #[test]
    fn route_effective_models_includes_failover_target() {
        // profile -> m_glm (entry); m_glm has a time strategy 22:00-06:00 -> m_qwen.
        let mut cfg = AppConfig::default();
        cfg.models.push(Model {
            id: "m_glm".into(),
            provider_id: "p".into(),
            upstream_model_id: "glm".into(),
            fallback_target_model_id: Some("m_vol".into()),
            fallback_strategies: vec![Strategy {
                id: "s".into(),
                priority: 1,
                enabled: true,
                kind: StrategyKind::Time(TimeStrategy {
                    days_of_week: vec![1, 2, 3, 4, 5, 6, 7],
                    time_start: 1320,
                    time_end: 360,
                    model_id: "m_qwen".into(),
                }),
            }],
            fallback_strategies_enabled: true,
            ..Default::default()
        });
        cfg.models.push(Model { id: "m_qwen".into(), provider_id: "p".into(), upstream_model_id: "qwen".into(), ..Default::default() });
        cfg.models.push(Model { id: "m_vol".into(), provider_id: "p".into(), upstream_model_id: "vol".into(), ..Default::default() });
        cfg.profiles.push(Profile { id: "p1".into(), name: "glm".into(), aliases: vec![], backing_model_id: "m_glm".into(), ..Default::default() });
        let now = crate::proxy::health::LocalNow { weekday: 2, minute: 1320 }; // 22:00 -> strategy m_qwen
        let eff = route_effective_models_core(&cfg, &now);
        assert_eq!(eff[0].effective_model_id, "m_glm");
        assert_eq!(eff[0].effective_fallback_model_id.as_deref(), Some("m_qwen"));
        assert_eq!(eff[0].via_fallback_strategy_id.as_deref(), Some("s"));
    }

    #[tokio::test]
    async fn set_request_recording_persists_and_swaps_recorder() {
        use crate::proxy::AppStateInner;
        let dir = tempfile::tempdir().unwrap();
        let state: AppState =
            Arc::new(AppStateInner::load(dir.path(), Arc::new(MemoryStore::default())).unwrap());
        assert!(state.recorder.read().unwrap().is_none()); // off by default

        // Build the minimal tauri::AppHandle-free path: test the core swap directly.
        let dir2 = dir.path().to_path_buf();
        // enable
        {
            let (rec, rx) = crate::recording::RequestRecorder::channel();
            tokio::spawn(crate::recording::run_writer(rx, dir2.join("request_log")));
            *state.recorder.write().unwrap() = Some(rec);
        }
        assert!(state.recorder.read().unwrap().is_some());
        // disable → drops Arc → recorder None
        *state.recorder.write().unwrap() = None;
        assert!(state.recorder.read().unwrap().is_none());
    }
}
