// TypeScript mirrors of the Rust structs (serde uses snake_case, no rename_all).
// Keep field names in sync with src-tauri/src/config/types.rs + commands.rs.

export interface UsageCreds {
  access_key_id?: string | null;
}

export interface Provider {
  id: string;
  vendor?: string;
  display_name: string;
  openai_base_url?: string | null;
  anthropic_base_url?: string | null;
  usage_creds?: UsageCreds | null;
}

export type ModelSource = "discovered" | "manual";

export interface Model {
  id: string;
  provider_id: string;
  source?: ModelSource;
  upstream_model_id: string;
  cooldown_seconds?: number | null;
  retry_count: number; // required: Rust u32, always serialized (spec §5)
  retry_delay_secs: number;
  fallback_target_model_id?: string | null;
  fallback_strategies?: Strategy[];
  fallback_strategies_enabled?: boolean;
}

export interface Profile {
  id: string;
  name: string;
  aliases?: string[];
  backing_model_id: string;
  strategies?: Strategy[];
  strategies_enabled?: boolean;
}

// Mirrors src-tauri StrategyKind (internally tagged: {"type":"time", ...}).
export interface TimeStrategy {
  days_of_week: number[]; // 1=Mon..=7=Sun
  time_start: number; // minutes 0..=1439
  time_end: number; // minutes 0..=1439
  model_id: string;
}
export interface StrategyKind extends TimeStrategy {
  type: "time";
}
export interface Strategy {
  id: string;
  priority: number; // 1..=5
  enabled: boolean;
  kind: StrategyKind;
}

// Mirrors src-tauri RouteEffective.
export interface RouteEffective {
  profile_id: string;
  effective_model_id: string;
  via_strategy_id?: string | null;
  effective_fallback_model_id?: string | null;
  via_fallback_strategy_id?: string | null;
}

/// Per-model currently-effective failover target (time-aware). Mirrors src-tauri
/// `ModelEffectiveFallback` (one entry per configured Model, drives the 故障转移 page hint).
export interface ModelEffectiveFallback {
  model_id: string;
  effective_fallback_model_id: string | null;
  via_fallback_strategy_id: string | null;
}

export interface Settings {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
  log_level: string;
}

export interface PlanInfo {
  /** 首次生效时间 (ISO 8601, e.g. "2026-07-30T00:00:00+08:00"). */
  start_time?: string | null;
  /** 当前到期时间 (ISO 8601). */
  end_time?: string | null;
  /** 是否已开启自动续费. */
  auto_renew?: boolean | null;
}

export interface UsageSnapshot {
  total?: number | null;
  remaining?: number | null;
  reset_at?: number | null;
  unit: string;
  raw_summary?: string | null;
  plan?: string | null;
  tiers?: UsageTier[];
  /** "plan" (tiered windows, default) | "consumption" (pay-as-you-go balance) */
  billing_model?: string;
  /** 套餐订阅元信息，作为套餐类型 tag 的 tooltip（火山 GetPersonalPlan）。 */
  plan_info?: PlanInfo | null;
}

export interface UsageTier {
  window: string; // "five_hour" | "weekly_limit" | "monthly"
  used_pct?: number | null;
  reset_at?: number | null;
}

export interface UsageEntry {
  provider_id: string;
  snapshot?: UsageSnapshot | null;
  error?: string | null;
}

export interface ModelHealth {
  cooling_down: boolean;
  recover_at?: number | null;
  tripped_at?: number | null;
}

export interface DiscoveredModel {
  id: string;
}

export interface ConnectionTest {
  ok: boolean;
  status: number;
  detail: string;
}

export interface ServerStatus {
  running: boolean;
  port?: number | null;
}

export interface EnvSnippet {
  anthropic_base_url: string;
  openai_base_url: string;
}

export interface SettingsView {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
  log_level: string;
}

// Mirrors src-tauri `SecretStatusView`.
export interface SecretStatusView {
  mode: "keyring" | "file" | "pending";
  consent_required: boolean;
}

// Mirror src-tauri/src/config/types.rs — keep in sync.
export const MIN_USAGE_REFRESH_SECS = 30;
export const DEFAULT_USAGE_REFRESH_SECS = 60;
export const MAX_USAGE_REFRESH_SECS = 3600;

// Mirror src-tauri/src/config/types.rs — keep in sync.
export const LOG_LEVEL_OPTIONS = ["trace", "debug", "info", "warn", "error"] as const;
export const DEFAULT_LOG_LEVEL = "info";

// Mirror src-tauri/src/config/types.rs ContextCheckStatus - serde uses
// #[serde(rename_all = "lowercase")], so the literals are lowercase.
export type ContextCheckStatus = "ok" | "smaller" | "unknown";

// Result of comparing a primary model's effective context size against its
// fallback's. serde uses snake_case (no rename_all), so fields stay snake_case.
export interface ContextCheckResult {
  status: ContextCheckStatus;
  primary_size?: number | null;
  fallback_size?: number | null;
}

/** `qianwen-login-status` event payload emitted by the backend during/after the in-app login flow. */
export interface QianwenLoginStatus {
  provider_id: string;
  status: "captured" | "closed-empty" | "error";
  message?: string;
}
