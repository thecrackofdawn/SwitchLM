# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What SwitchLM is

A Tauri v2 (Rust backend + Vue 3 frontend) **desktop/tray app that runs a local LLM proxy** for coding agents (Claude Code, Cursor, Cline). It lets an agent call one local URL and routes that traffic across Chinese LLM *coding plans* (智谱 Zhipu, 火山 Volcengine, DeepSeek), with bidirectional Anthropic↔OpenAI protocol translation, 429-aware model fallback, and real-time plan-quota visibility. Agents point at `http://localhost:6950` (Anthropic) or `…/v1` (OpenAI).

## Commands

```bash
pnpm install                     # frontend deps
pnpm tauri dev                   # dev mode (hot reload; boots the Rust proxy + Vue)
pnpm tauri build                 # release bundle -> src-tauri/target/release/bundle/

pnpm build                       # frontend type-check (vue-tsc --noEmit) + vite build
pnpm exec vue-tsc --noEmit       # frontend type-check only

cargo test --manifest-path src-tauri/Cargo.toml              # all backend tests
cargo test --manifest-path src-tauri/Cargo.toml config::catalog   # one module's tests
cargo test --manifest-path src-tauri/Cargo.toml fallback_on_rate_limit   # one test by name
```

**Frontend package manager is pnpm** (see `pnpm-lock.yaml`). Do not use `npm`/`npx` directly - use `pnpm`/`pnpm exec` instead (e.g. `pnpm install`, `pnpm build`, `pnpm exec vue-tsc`). `npm install` would write a foreign lockfile and desync the dependency tree.

Backend tests are co-located as `#[cfg(test)] mod tests` in each `.rs` file. HTTP behavior is tested with `wiremock` mock servers; breaker/cache logic is made deterministic by an injectable `Clock` (`FakeClock`) and an in-memory `MemoryStore` secret backend.

Windows build prerequisites: Node ≥ 20, Rust stable, MSVC C++ Build Tools, WebView2.

## Architecture

### Two independent channels

| Channel | Purpose | Form | Caller |
|---|---|---|---|
| **Proxy data channel** | real LLM traffic | local HTTP `127.0.0.1:{port}` | coding agents |
| **Management channel** | config / switch / usage / status | Tauri `#[tauri::command]` IPC | Vue frontend ↔ Rust |

The proxy (`src-tauri/src/proxy/`) is an `axum` server spawned as a tokio background task on app startup. It exposes two routes — `POST /v1/messages` (Anthropic edge) and `POST /v1/chat/completions` (OpenAI edge) — both funnelling into one `dispatch()`. Management commands live in `src-tauri/src/commands.rs` and are registered in `src-tauri/src/lib.rs`.

### The Provider → Model → Profile hierarchy (core mental model)

Three entities in `config/types.rs`, persisted to `<app_data>/app_config.json`:

- **Provider** = an *account*: a vendor slug + credentials + endpoints. Supports multiple accounts per vendor (each gets its own opaque `id`).
- **Model** = an upstream model belonging to one Provider. Carries its own `fallback_target_model_id` and circuit-breaker `cooldown_seconds`. Protocol/endpoint are *inherited from the provider*, not stored per-model.
- **Profile** = the **agent-facing route name** the client sends as `model` (e.g. `glm-4.6`), with optional aliases (e.g. `claude-sonnet-4`), backed by exactly one Model.

Request flow resolves top-down: client's `model` → matching Profile (name or alias) → backing Model → that Model's Provider (vendor + base_url + key). **Fallback is configured per-Model, not per-Profile.** `resolve_model` (proxy/resolve.rs) does the Profile→Model lookup; `dispatch` walks the chain.

**`Provider.vendor` is the single routing key** for every vendor-differentiated behavior: usage-adapter selection, model-discovery path, same-account conflict detection, rate-limit code matching, catalog context-size lookup, and base_url defaults. `Provider.id` is an opaque PK that does **not** participate in routing.

### Adding a new vendor

**A simple Bearer-key provider (e.g. DeepSeek) needs zero backend changes** — every vendor-routed path already degrades sensibly for an unknown vendor (usage → N/A, discovery → generic `GET {base}/models`, same-account conflict → match on api_key, rate-limit → HTTP 429 + keyword fallback). Only the frontend + catalog data change. A provider with a usage API, AK/SK usage credentials, non-`/models` model discovery, or a special rate-limit code needs the matching backend hook (a usage adapter, the `usage_creds` form, `volcengine_plan_action`, or the per-vendor code arm in `error_adapter.rs`).

The full per-capability decision table and checklist live in **`dev-reference/adding-a-provider.md` — read it first.** Bare minimum for any new vendor: register the slug in `src/lib/selectLabel.ts` (`vendorOptions`) + `src/views/Provider.vue` (`vendorDefaults`), add context sizes to `src-tauri/assets/provider_desc.json` (+ a guard assertion in `config/catalog.rs`), and update the known-vendor doc comment in `config/types.rs`.

### Request dispatch & resilience (proxy/dispatch.rs)

`dispatch()` resolves the Profile, then for each hop in the fallback chain:
1. **Skip cooling models** without probing (circuit breaker, `proxy/health.rs` — runtime-only, not persisted).
2. Pick the path: **passthrough** (same protocol on both sides), **translate** (Anthropic client → OpenAI backend), or none → skip to fallback. Missing API key → skip to fallback.
3. Call upstream. Rate-limit/quota errors (`is_rate_limit_error` in `proxy/error_adapter.rs`) **trip the breaker and advance to fallback**; other errors (401/400/500/network) **pass through** with no trip.
4. Cycles are caught by a visited-set; the chain is capped at 8 hops.

The **streaming path** peeks the *first SSE event*: if it's a rate-limit error before any content is forwarded, it falls back; once real content arrives it commits and forwards the rest (the "no content forwarded yet" boundary, spec §3.5). Rate-limit detection is vendor-specific (HTTP 429 always; DeepSeek 402; 智谱 codes `1113`/`1302`/`1305`/`1308`–`1321`; 火山 prefix `RateLimitExceeded*`/`QuotaExceeded*` + 429 family + 403 `AccountOverdueError`/`OperationDenied.ServiceOverdue` + 400/404 `InvalidSubscription`/`ModelNotOpen`/`UnsupportedModel`) with a conservative keyword fallback (incl. `欠费`/`overdue`/`insufficient balance`). 智谱 `1214` is a 400 param error, **not** a rate limit; 5xx passes through (spec §8).

**Breaker recovery** (`compute_recover_at`): on trip, the provider's quota is queried *in real time* (bypassing the usage cache). If a window is actually exhausted (≥100%), `recover_at` = the package reset time; otherwise it's a transient throttle and the model retries after `cooldown_seconds` (default 300s).

### Protocol translation (translate/)

Internal canonical format is **OpenAI Chat Completions** (both target backends are OpenAI-compatible, so outbound translation is near-free). Complexity is at the Anthropic edge: `anthropic_to_openai` (request), `openai_to_anthropic` (response), and `StreamTranslator` (incremental SSE). The echoed `model` field is the *requested* name (spec §3.4).

### Secrets, config, catalog

- **Secrets never touch `app_config.json`.** Inference API keys and Volcengine usage SKs live in the OS keyring (`config/secrets.rs`, `keyring` crate, service `SwitchLM`). `UsageCreds.secret_access_key` is `#[serde(skip)]`; legacy plaintext SKs are migrated to the keyring on startup (`store::migrate_usage_sk_to_keyring`). In the frontend, key fields are left blank on edit (blank = keep existing); the store tracks only boolean presence.
- **Config** (`config/store.rs`): `app_config.json` is the single source of truth, re-saved after every mutating command. `normalize_legacy_vendors` backfills `vendor` from `id` for old configs.
- **Catalog** (`config/catalog.rs` + `src-tauri/assets/provider_desc.json`): bundled provider/model context sizes, organized by provider (`{ providers: [{ provider_id, models: [{ upstream_model_id, context_size }] }] }`) so future provider-level attrs (e.g. per-plan concurrency) can be added. The embedded catalog is the in-memory baseline, re-read from the binary on every launch (never persisted), so bundled default changes take effect immediately. User customizations/supplements live in a separate **sparse** `<app_data>/custom_provider_desc.json` (only overridden entries, so bundled updates still apply to untouched models). The Models page edits context sizes in-place via `set_custom_context_size`, which writes the custom file and re-derives the in-memory catalog under a write lock in one step (real-time, same-vendor, no restart); `Model` no longer carries a `context_size` field - the catalog is the single source. Used only for the "recognized context size" value and fallback-capacity checks.

### Frontend (src/)

Vue 3 `<script setup>` + Pinia + Vue Router (hash history) + Naive UI. UI is Chinese-language.

- **Tauri IPC bridge** — `src/lib/commands.ts` is one flat module with a typed wrapper per Rust command (`export const getProviders = () => invoke<Provider[]>("get_providers")`). Tauri auto-converts camelCase JS arg keys to snake_case Rust params. `src/lib/types.ts` is a **hand-maintained TS mirror of the Rust serde structs** — snake_case field names preserved (serde uses no `rename_all`); keep it in sync with `config/types.rs` + `commands.rs` when changing shapes.
- **Three Pinia stores** (setup-style): `config` (providers/models/profiles/fallback + key-presence maps), `runtime` (usage + breaker health), `system` (server status, env snippet, settings, bind error). **Canonical pattern: after every mutating action, re-fetch the affected slice from the backend** — no optimistic local mutation.
- **`usePolling`** (auto-clears on unmount; interval can be a getter that re-arms; errors swallowed). **`useOrdered`** is a localStorage drag-reorder, cosmetic and *never sent to the backend* (Models/Fallback/Provider tabs). The **route list** and **usage display** orders are exceptions: persisted backend fields (`AppConfig.route_order` profile ids, `usage_order` provider ids) shared across their views *and the tray*, so a drag survives a tray rebuild and an app restart.
- `Provider.vue` and `Models.vue` are **not routed** — they're composed as tabs inside `Sources.vue` (6 routes, 8 view files).
- **Theming is a dual, manually-kept-in-sync system**: `styles/tokens.css` (CSS vars, dark via `prefers-color-scheme`) and `styles/theme.ts` (same values as Naive UI `GlobalThemeOverrides`). Change both together.
- `UsageSnapshot.billing_model` (`"plan"` vs `"consumption"`), not the vendor, drives whether the UI shows tier windows or a balance.

## Conventions

- **Logs must identify vendor + upstream model name, never the opaque internal `id`** (`m_xxx` is unrecognizable on another machine). `dispatch`/`log_hop` already follow this; preserve it.
- **Process & port**: closing the window hides to tray (proxy + tray keep running; quit via tray). If the preferred port (`6950`, configurable) is occupied, the proxy auto-recovers (polls every 10s) and surfaces a `bindError`. Because agents hardcode the env URL, a port-bind failure is warned prominently as a `bindError` (the proxy polls the configured port every 10s and never silently binds another); copy-paste env snippets carry the bound port.

## Spec-driven workflow

This repo develops spec-first. The **canonical design spec is `docs/superpowers/specs/2026-07-27-switchlm-llm-proxy-design.md`** — Rust code comments reference it by section (e.g. "spec §3.5", "§4.1", "§6.3"). When touching proxy/breaker behavior, read the relevant spec section there first.

- `docs/superpowers/specs/` — design docs (the "what/why")
- `docs/superpowers/plans/` — implementation plans (the "how/steps")
- `dev-reference/adding-a-provider.md` — per-vendor change checklist (see *Adding a new vendor* above)
