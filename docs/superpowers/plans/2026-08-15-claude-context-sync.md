# Claude Code Context-Window Auto-Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An opt-in background task (default off) that keeps Claude Code's assumed context window in sync with the primary route model's real window, by adjusting `[1m]` suffixes on model names and `env.CLAUDE_CODE_MAX_CONTEXT_TOKENS` in `~/.claude/settings.json`.

**Architecture:** New Rust module `src-tauri/src/agent_sync.rs` with a pure, testable core (`strip_1m`, `collect_entries`, `compute_plan`, `apply_plan`) plus a thin async shell (`sync_once`, `spawn_sync_task`) spawned at app setup. A `Settings.sync_claude_context` toggle (default false) gates each 30s round. The proxy, translation, breaker, tray, and catalog structures are untouched. Spec: `docs/superpowers/specs/2026-08-15-claude-context-sync-design.md`.

**Tech Stack:** Rust (tauri 2, tokio, serde_json, chrono), Vue 3 `<script setup>` + Pinia + Naive UI (Chinese UI).

## Global Constraints

- **Spec of record:** `docs/superpowers/specs/2026-08-15-claude-context-sync-design.md`. Read §5 (sync algorithm) and §7 (error handling) before coding.
- **Sync target:** primary route model only (`profile_start_model` time-strategy winner, else backing). Never fallback hops.
- **Toggle default is `false`.** When off or turned off: never modify or delete anything in Claude Code's settings.json.
- **Never write on malformed input:** missing file / invalid JSON / `env` present-but-not-an-object → skip the round entirely.
- **Env managed-delete rule:** `env.CLAUDE_CODE_MAX_CONTEXT_TOKENS` may only be deleted when the on-disk value equals what this feature last wrote (in-memory bookkeeping, not persisted).
- **Env value = minimum** context size among entries left without `[1m]` after rewrite; written as a JSON **string** (e.g. `"200000"`).
- **`[1m]` handling is case-insensitive** (`pro[1M]` counts); `[2m]` is NOT stripped by us and `strip_1m` leaves it alone.
- **Logs identify vendor + upstream model name** (never the opaque `m_xxx` id) — repo convention from CLAUDE.md.
- **Serde field names are snake_case, no rename_all.** Frontend `types.ts` is a hand-maintained mirror — keep in sync.
- **Frontend package manager is pnpm** (`pnpm exec vue-tsc --noEmit`), never npm.
- **Cross-platform:** home dir via `HOME` (Unix) / `USERPROFILE` (Windows) env vars; `PathBuf`, no hardcoded separators. No new crate dependencies.
- **Tests:** co-located `#[cfg(test)] mod tests`; backend run via `cargo test --manifest-path src-tauri/Cargo.toml agent_sync` (or by test name).
- **Commits: dev on `main` directly** (repo convention); `git add` explicit paths only, never `-A`.

## File Structure

| File | Responsibility |
|---|---|
| Create `src-tauri/src/agent_sync.rs` | Whole feature backend: path resolution, `[1m]` strip, entry collection, plan computation, in-place apply, sync task + status bookkeeping |
| Modify `src-tauri/src/lib.rs` | `mod agent_sync;` + register 2 commands + spawn the task in setup |
| Modify `src-tauri/src/config/types.rs` | `Settings.sync_claude_context: bool` |
| Modify `src-tauri/src/commands.rs` | `SettingsView` field, `set_sync_claude_context`, `get_claude_sync_status` |
| Modify `src/lib/types.ts` | `SettingsView.sync_claude_context` + `ClaudeSyncStatus` mirror |
| Modify `src/lib/commands.ts` | `setSyncClaudeContext`, `getClaudeSyncStatus` wrappers |
| Modify `src/stores/system.ts` | `claudeSyncStatus` state + actions |
| Modify `src/views/Settings.vue` | Toggle card + status line |

Task order: 1 (config field) → 2 (pure core) → 3 (apply) → 4 (task + status + commands) → 5 (lib.rs wiring) → 6 (frontend). Each produces independently testable output.

---

### Task 1: `Settings.sync_claude_context` config field

**Files:**
- Modify: `src-tauri/src/config/types.rs` (Settings struct at ~line 44, `Default` impl at ~line 65)
- Test: same file, `#[cfg(test)]` (existing tests module starts at line 300)

**Interfaces:**
- Produces: `Settings.sync_claude_context: bool` (serde `#[serde(default)]`, `Default` = `false`). Later tasks read it via `state.config.read().await.settings.sync_claude_context`.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `mod tests` in `src-tauri/src/config/types.rs` (it already has `use super::*;` and an AppConfig-default test around line 331 — follow its style):

```rust
    #[test]
    fn sync_claude_context_defaults_off() {
        assert!(!Settings::default().sync_claude_context);
        assert!(!AppConfig::default().settings.sync_claude_context);
    }

    #[test]
    fn sync_claude_context_missing_in_old_config_loads_false() {
        // A pre-feature settings JSON without the field must deserialize to false.
        let json = r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.sync_claude_context);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml sync_claude_context`
Expected: FAIL — `no field 'sync_claude_context'` (compile error).

- [ ] **Step 3: Implement**

In `src-tauri/src/config/types.rs`, add to `Settings` (after `background_destroy`, with the doc-comment style of neighbors):

```rust
    /// 是否自动同步 Claude Code 的模型上下文声明（改写 ~/.claude/settings.json 中模型名的
    /// [1m] 后缀与 CLAUDE_CODE_MAX_CONTEXT_TOKENS，使其跟随主路由模型的真实窗口）。
    /// 默认关。见 docs/superpowers/specs/2026-08-15-claude-context-sync-design.md。
    #[serde(default)]
    pub sync_claude_context: bool,
```

In the manual `impl Default for Settings` add `sync_claude_context: false,`. **Also fix the two struct-literal test fixtures** that name every `Settings` field — `src-tauri/src/config/store.rs:113` and the AppConfig-default test in `types.rs` (~line 331) — by adding `sync_claude_context: false` to each, otherwise the crate won't compile.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml sync_claude_context && cargo test --manifest-path src-tauri/Cargo.toml config::`
Expected: PASS (both new tests + the whole config module suite).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/config/store.rs
git commit -m "feat(sync): Settings.sync_claude_context 配置字段(默认关)"
```

---

### Task 2: `agent_sync` pure core — path, `[1m]` strip, entry collection, plan

**Files:**
- Create: `src-tauri/src/agent_sync.rs`
- Modify: `src-tauri/src/lib.rs` (module list, ~line 1-11: add `pub mod agent_sync;`)

**Interfaces:**
- Consumes: `crate::config::{AppConfig, Profile}`, `crate::config::catalog::ProviderCatalog`, `crate::proxy::health::LocalNow`, `crate::proxy::strategies::profile_start_model`.
- Produces (exact signatures later tasks rely on):

```rust
pub const ENV_KEY: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";
pub const ONE_M: u32 = 1_000_000;

pub fn claude_settings_path(home: &std::path::Path) -> std::path::PathBuf;
pub fn strip_1m(name: &str) -> &str;

#[derive(Debug, Clone, PartialEq)]
pub enum EntryLocation { Model, Env(&'static str) }

/// All model-name entries found in Claude Code's settings, deduped by exact raw string,
/// in source order. Each is a (location, raw_name) pair.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeEntries { pub entries: Vec<(EntryLocation, String)> }
pub fn collect_entries(json: &serde_json::Value) -> ClaudeEntries;

/// Per-entry outcome of resolution + rewrite decision.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryPlan {
    /// Keep the name exactly as-is (already correct, or [1m] matches).
    Keep,
    /// Rewrite the name to this new string.
    Rewrite(String),
    /// Not resolvable / no catalog size — leave untouched, excluded from env calc.
    Skip(String), // reason text for logs
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncPlan {
    /// (location, raw name as found on disk, new name) for entries needing a rewrite.
    pub rewrites: Vec<(EntryLocation, String, String)>,
    /// Some(min size among [1m]-less entries after rewrite) → write env;
    /// None → no [1m]-less entries (managed-delete may apply, see apply_plan).
    pub env_value: Option<u32>,
    /// Human-readable summary for logs/status (vendor + upstream per entry).
    pub notes: Vec<String>,
}
pub fn compute_plan(
    cfg: &AppConfig,
    catalog: &ProviderCatalog,
    entries: &ClaudeEntries,
    now: &LocalNow,
) -> SyncPlan;
```

Note: dedup means one raw name shared by several locations gets ONE plan; `apply_plan` (Task 3) must still rewrite **every location** holding that raw name.

- [ ] **Step 1: Create the module with the failing tests**

Create `src-tauri/src/agent_sync.rs` containing ONLY the test module and minimal stubs that `todo!()` — no, better: write the real signatures with empty/`Default` bodies so it compiles, then the tests fail on assertions. First the stubs:

```rust
//! Claude Code 上下文声明自动同步。
//! 见 docs/superpowers/specs/2026-08-15-claude-context-sync-design.md。
//!
//! 纯函数核心（可测）+ 异步壳（sync_once / spawn_sync_task，Task 4 加入）。
//! 唯一写入目标：{home}/.claude/settings.json 中模型名的 [1m] 后缀与
//! env.CLAUDE_CODE_MAX_CONTEXT_TOKENS。凡读不出/解析不了/查不到尺寸，一律跳过不写。

use crate::config::catalog::ProviderCatalog;
use crate::config::AppConfig;
use crate::proxy::health::LocalNow;
use crate::proxy::strategies::profile_start_model;

pub const ENV_KEY: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";
pub const ONE_M: u32 = 1_000_000;

/// `~/.claude/settings.json` path under the given home dir (same layout on Windows/Linux).
pub fn claude_settings_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".claude").join("settings.json")
}

/// Strip a trailing `[1m]` (case-insensitive) declaration. `[2m]` and non-suffixed
/// names are returned unchanged (identity on the same slice).
pub fn strip_1m(name: &str) -> &str {
    if name.len() >= 4 && name[..name.len() - 4].ends_with('[') {
        // placeholder — real impl in Step 3
    }
    name
}
```

(Do not actually write placeholder bodies — Step 1 writes the tests, Step 3 writes the full implementations. Create the file with just the header `use` lines + empty test module so `cargo test` fails to find the functions; simplest compiling scaffold: put `unimplemented!()` bodies behind `#[allow(unused)]`? No — **follow TDD strictly: write tests + full signatures with `todo!()` bodies, tests will FAIL (panic), then replace with real code.**)

**Concrete Step-1 file content** (tests only, functions `todo!()`):

```rust
//! (header comment as above)

use crate::config::catalog::ProviderCatalog;
use crate::config::AppConfig;
use crate::proxy::health::LocalNow;
use crate::proxy::strategies::profile_start_model;
use serde_json::json;

pub const ENV_KEY: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";
pub const ONE_M: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq)]
pub enum EntryLocation { Model, Env(&'static str) }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeEntries { pub entries: Vec<(EntryLocation, String)> }

#[derive(Debug, Clone, PartialEq)]
pub enum EntryPlan { Keep, Rewrite(String), Skip(String) }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncPlan {
    pub rewrites: Vec<(EntryLocation, String, String)>,
    pub env_value: Option<u32>,
    pub notes: Vec<String>,
}

pub fn claude_settings_path(home: &std::path::Path) -> std::path::PathBuf { todo!() }
pub fn strip_1m(name: &str) -> &str { todo!() }
pub fn collect_entries(json: &serde_json::Value) -> ClaudeEntries { todo!() }
pub fn compute_plan(
    cfg: &AppConfig, catalog: &ProviderCatalog, entries: &ClaudeEntries, now: &LocalNow,
) -> SyncPlan { todo!() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    // ---- strip_1m ----

    #[test]
    fn strip_1m_variants() {
        assert_eq!(strip_1m("pro[1m]"), "pro");
        assert_eq!(strip_1m("pro[1M]"), "pro");
        assert_eq!(strip_1m("pro"), "pro");
        assert_eq!(strip_1m("pro[2m]"), "pro[2m]"); // 2m NOT stripped
        assert_eq!(strip_1m("[1m]"), "");           // degenerate but well-defined
        assert_eq!(strip_1m(""), "");
    }

    // ---- claude_settings_path ----

    #[test]
    fn settings_path_joins_dot_claude() {
        use std::path::Path;
        let p = claude_settings_path(Path::new("/home/u"));
        assert!(p.ends_with(".claude/settings.json") || p.to_string_lossy().contains(".claude"));
    }

    // ---- collect_entries ----

    #[test]
    fn collect_all_five_sources_deduped() {
        let j = json!({
            "model": "opus",
            "env": {
                "ANTHROPIC_MODEL": "pro[1m]",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "pro[1m]",   // dup raw string → one entry
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "flash",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "",         // empty → ignored
                "ANTHROPIC_BASE_URL": "http://x",            // not a model key → ignored
                "ANTHROPIC_AUTH_TOKEN": 42,                  // non-string → ignored
            },
            "other": "noise"
        });
        let c = collect_entries(&j);
        let names: Vec<&str> = c.entries.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["opus", "pro[1m]", "flash"]); // source order: model first
        assert_eq!(c.entries[0].0, EntryLocation::Model);
        assert!(matches!(c.entries[1].0, EntryLocation::Env("ANTHROPIC_MODEL")));
    }

    #[test]
    fn collect_tolerates_missing_keys_and_env() {
        let c = collect_entries(&json!({}));
        assert!(c.entries.is_empty());
        let c2 = collect_entries(&json!({"model": "opus"})); // no env object at all
        assert_eq!(c2.entries.len(), 1);
        let c3 = collect_entries(&json!({"model": "a", "env": "not-an-object"}));
        assert_eq!(c3.entries.len(), 1, "env present but not an object: env keys skipped, model kept");
    }

    // ---- compute_plan ----
    // Shared fixture: two profiles across context tiers.
    //   Profile "pro"  (alias "opus")  → m_small (zhipu/glm-4.6, 200k)
    //   Profile "flash"               → m_big (volcengine-coding/glm-5.2, 1M)

    fn plan_cfg() -> AppConfig {
        AppConfig {
            providers: vec![
                Provider { id: "p1".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
                           openai_base_url: None, anthropic_base_url: None, usage_creds: None },
                Provider { id: "p2".into(), vendor: "volcengine-coding".into(), display_name: "火山".into(),
                           openai_base_url: None, anthropic_base_url: None, usage_creds: None },
            ],
            models: vec![
                Model { id: "m_small".into(), provider_id: "p1".into(),
                        upstream_model_id: "glm-4.6".into(), ..Default::default() },
                Model { id: "m_big".into(), provider_id: "p2".into(),
                        upstream_model_id: "glm-5.2".into(), ..Default::default() },
            ],
            profiles: vec![
                Profile { id: "pf1".into(), name: "pro".into(), aliases: vec!["opus".into()],
                          backing_model_id: "m_small".into(), ..Default::default() },
                Profile { id: "pf2".into(), name: "flash".into(), aliases: vec![],
                          backing_model_id: "m_big".into(), ..Default::default() },
            ],
            ..Default::default()
        }
    }

    fn plan_catalog() -> ProviderCatalog {
        serde_json::from_str(r#"{"version":1,"providers":[
            {"provider_id":"zhipu","models":[{"upstream_model_id":"glm-4.6","context_size":200000}]},
            {"provider_id":"volcengine-coding","models":[{"upstream_model_id":"glm-5.2","context_size":1000000}]}
        ]}"#).unwrap()
    }

    const NOW: LocalNow = LocalNow { weekday: 1, minute: 0 };

    #[test]
    fn plan_four_rewrite_branches() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "opus".into()),          // alias → 200k, no [1m] → keep, env candidate 200k
            (EntryLocation::Env("ANTHROPIC_DEFAULT_SONNET_MODEL"), "pro[1m]".into()), // 200k → strip
            (EntryLocation::Env("ANTHROPIC_DEFAULT_HAIKU_MODEL"), "flash".into()),    // 1M → append
            (EntryLocation::Env("ANTHROPIC_DEFAULT_OPUS_MODEL"), "flash[1m]".into()), // 1M → keep
        ]};
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        // rewrite set: "pro[1m]"→"pro", "flash"→"flash[1m]"; kept: "opus", "flash[1m]"
        let mut rw: Vec<_> = plan.rewrites.iter().map(|(loc, from, to)| (from.clone(), to.clone())).collect();
        rw.sort();
        assert_eq!(rw, vec![("flash".to_string(), "flash[1m]".to_string()),
                            ("pro[1m]".to_string(), "pro".to_string())]);
        // [1m]-less after rewrite: "opus" (200k), "pro" (200k) → min = 200k
        assert_eq!(plan.env_value, Some(200_000));
    }

    #[test]
    fn plan_all_1m_entries_env_none() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "flash[1m]".into()),
        ]};
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        assert!(plan.rewrites.is_empty());
        assert_eq!(plan.env_value, None, "no [1m]-less entry → no env write");
    }

    #[test]
    fn plan_unresolvable_and_unknown_size_skip() {
        let cfg = plan_cfg();
        let cat = plan_catalog(); // lacks glm-9.9 size
        // add a model with no catalog entry, backing a profile
        let mut cfg2 = cfg.clone();
        cfg2.models.push(Model { id: "m_unknown".into(), provider_id: "p1".into(),
                                 upstream_model_id: "glm-9.9".into(), ..Default::default() });
        cfg2.profiles.push(Profile { id: "pf3".into(), name: "weird".into(), aliases: vec![],
                                     backing_model_id: "m_unknown".into(), ..Default::default() });
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "no-such-profile".into()), // Skip: unresolvable
            (EntryLocation::Env("ANTHROPIC_MODEL"), "weird".into()), // Skip: no catalog size
            (EntryLocation::Env("ANTHROPIC_DEFAULT_SONNET_MODEL"), "opus".into()), // normal
        ]};
        let plan = compute_plan(&cfg2, &cat, &entries, &NOW);
        assert!(plan.rewrites.is_empty(), "skipped entries never rewritten");
        assert_eq!(plan.env_value, Some(200_000), "skips excluded from env calc");
        assert_eq!(plan.notes.len(), 2, "one note per skip, with reason");
    }

    #[test]
    fn plan_env_takes_min_across_mixed() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "opus".into()),   // 200k, no suffix
            (EntryLocation::Env("ANTHROPIC_MODEL"), "flash".into()), // → flash[1m], excluded
        ]};
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        assert_eq!(plan.env_value, Some(200_000));
    }

    #[test]
    fn plan_follows_time_strategy_flip() {
        // "pro" has a time strategy routing to m_big on Tuesday; strategy wins over backing.
        let mut cfg = plan_cfg();
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s1".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_big".into(),
            }),
        }];
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![(EntryLocation::Model, "pro".into())] };

        // Monday: backing (200k) → no suffix, env 200k
        let p1 = compute_plan(&cfg, &cat, &entries, &LocalNow { weekday: 1, minute: 0 });
        assert!(p1.rewrites.is_empty());
        assert_eq!(p1.env_value, Some(200_000));
        // Tuesday: strategy → m_big (1M) → append [1m], env None
        let p2 = compute_plan(&cfg, &cat, &entries, &LocalNow { weekday: 2, minute: 0 });
        assert_eq!(p2.rewrites, vec![(EntryLocation::Model, "pro".into(), "pro[1m]".into())]);
        assert_eq!(p2.env_value, None);
    }

    #[test]
    fn plan_exact_1m_boundary_keeps_suffix() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![(EntryLocation::Model, "flash".into())] };
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        // 1_000_000 >= ONE_M → append
        assert_eq!(plan.rewrites, vec![(EntryLocation::Model, "flash".into(), "flash[1m]".into())]);
    }
}
```

Also add `pub mod agent_sync;` to `src-tauri/src/lib.rs` (after `pub mod commands;`, alphabetical-ish — follow existing order).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync`
Expected: FAIL — panics `not yet implemented` (`todo!()`) in every test.

- [ ] **Step 3: Implement the four functions**

Replace the `todo!()` bodies (keep signatures exactly):

```rust
/// `~/.claude/settings.json` path under the given home dir (same layout on Windows/Linux).
pub fn claude_settings_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".claude").join("settings.json")
}

/// Strip a trailing `[1m]` (case-insensitive). `[2m]` and non-suffixed names pass through.
/// Mirrors Claude Code's own suffix semantics (`[1m]` = hard 1M declaration).
pub fn strip_1m(name: &str) -> &str {
    for suffix in ["[1m]", "[1M]"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

/// Claude Code env keys that carry model names, in collection order.
const ENV_MODEL_KEYS: [&str; 4] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// Collect the model-name entries Claude Code is configured with. Source order:
/// top-level `model`, then the four env keys in ENV_MODEL_KEYS order. Deduplicated by
/// exact raw string (first occurrence wins for `EntryLocation`); non-string and empty
/// values ignored; a non-object `env` means env keys are skipped but `model` is kept.
pub fn collect_entries(json: &serde_json::Value) -> ClaudeEntries {
    let mut out: Vec<(EntryLocation, String)> = vec![];
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut push = |loc: EntryLocation, v: Option<&serde_json::Value>| {
        if let Some(serde_json::Value::String(s)) = v {
            if !s.is_empty() && seen.insert(s.as_str()) {
                out.push((loc, s.clone()));
            }
        }
    };
    push(EntryLocation::Model, json.get("model"));
    if let Some(serde_json::Value::Object(env)) = json.get("env") {
        for key in ENV_MODEL_KEYS {
            push(EntryLocation::Env(key), env.get(key));
        }
    }
    ClaudeEntries { entries: out }
}

/// Resolve one base model name (already `[1m]`-stripped) to its primary route model's
/// real context size. None = unresolvable profile OR no catalog size (caller skips).
fn resolve_size(
    cfg: &AppConfig,
    catalog: &ProviderCatalog,
    base: &str,
    now: &LocalNow,
) -> Option<u32> {
    let prof = cfg.profiles.iter().find(|p| {
        p.name == base || p.aliases.iter().any(|a| a == base)
    })?;
    let (model_id, _via) = profile_start_model(prof, cfg, now);
    let model = cfg.models.iter().find(|m| m.id == model_id)?;
    let vendor = cfg
        .providers
        .iter()
        .find(|p| p.id == model.provider_id)
        .map(|p| p.vendor.clone())
        .unwrap_or_default();
    catalog.context_size(&vendor, &model.upstream_model_id)
}

/// Compute the full round plan. Spec §5 steps 3-5.
pub fn compute_plan(
    cfg: &AppConfig,
    catalog: &ProviderCatalog,
    entries: &ClaudeEntries,
    now: &LocalNow,
) -> SyncPlan {
    let mut plan = SyncPlan::default();
    let mut env_candidates: Vec<u32> = vec![];

    for (loc, raw) in &entries.entries {
        let base = strip_1m(raw).to_string();
        let size = match resolve_size(cfg, catalog, &base, now) {
            Some(s) => s,
            None => {
                // vendor+upstream 不可得时也只报 base 名——该入口本就未解析成功。
                plan.notes.push(format!("skip '{raw}'（未匹配路由或上下文大小未知）"));
                continue;
            }
        };
        let has_1m = base.len() < raw.len();
        match (has_1m, size >= ONE_M) {
            (true, true) | (false, false) => {} // declaration already correct
            (true, false) => {
                plan.rewrites.push((loc.clone(), raw.clone(), base.clone()));
                env_candidates.push(size);
            }
            (false, true) => {
                plan.rewrites.push((loc.clone(), raw.clone(), format!("{base}[1m]")));
            }
        }
        if !has_1m {
            env_candidates.push(size);
        }
    }

    plan.env_value = env_candidates.iter().copied().min();
    plan
}
```

Remove the now-unused `serde_json::json` import if the tests import it themselves (tests keep their own `use serde_json::json;`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_sync.rs src-tauri/src/lib.rs
git commit -m "feat(sync): agent_sync 纯函数核心——[1m] 剥离/入口收集/同步计划"
```

---

### Task 3: `apply_plan` — in-place JSON write-back with managed-delete

**Files:**
- Modify: `src-tauri/src/agent_sync.rs` (add `apply_plan` + tests)

**Interfaces:**
- Consumes: `SyncPlan`, `EntryLocation`, `ENV_KEY` from Task 2.
- Produces:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("settings.json 解析失败，跳过本轮")]
    Malformed,
    #[error("写入失败: {0}")]
    Io(#[from] std::io::Error),
}

/// Apply `plan` to a parsed settings.json `Value` **in place**.
/// Returns Ok(true) if the value changed (caller persists), Ok(false) if identical.
/// `last_written_env`: what THIS FEATURE last wrote to ENV_KEY (None = never wrote).
/// Managed-delete: when `plan.env_value` is None and the on-disk value equals
/// `last_written_env`, the key is removed; a user-authored value is never touched.
pub fn apply_plan(
    json: &mut serde_json::Value,
    plan: &SyncPlan,
    last_written_env: Option<&str>,
) -> Result<bool, SyncError>;
```

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `agent_sync.rs`:

```rust
    // ---- apply_plan ----

    fn settings_doc() -> serde_json::Value {
        serde_json::json!({
            "model": "opus",
            "includeCoAuthoredBy": false,
            "env": {
                "DISABLE_AUTOUPDATER": "1",
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:6950",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "pro[1m]",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "flash",
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "999999"
            },
            "permissions": { "allow": ["Bash(git *)"] }
        })
    }

    fn simple_plan(rewrites: Vec<(EntryLocation, String, String)>, env: Option<u32>) -> SyncPlan {
        SyncPlan { rewrites, env_value: env, notes: vec![] }
    }

    #[test]
    fn apply_rewrites_every_location_holding_the_raw_name() {
        // Two locations hold "pro[1m]": model + one env key (dedup gave one plan entry,
        // but apply must fix both).
        let mut j = serde_json::json!({
            "model": "pro[1m]",
            "env": { "ANTHROPIC_DEFAULT_SONNET_MODEL": "pro[1m]" }
        });
        let plan = simple_plan(
            vec![(EntryLocation::Model, "pro[1m]".into(), "pro".into())],
            Some(200_000),
        );
        let changed = apply_plan(&mut j, &plan, None).unwrap();
        assert!(changed);
        assert_eq!(j["model"], "pro");
        assert_eq!(j["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "pro");
        assert_eq!(j["env"][ENV_KEY], "200000"); // env written as string
    }

    #[test]
    fn apply_preserves_unrelated_keys_and_env_entries() {
        let mut j = settings_doc();
        let plan = simple_plan(
            vec![(EntryLocation::Env("ANTHROPIC_DEFAULT_SONNET_MODEL"),
                  "pro[1m]".into(), "pro".into())],
            Some(200_000),
        );
        apply_plan(&mut j, &plan, None).unwrap();
        assert_eq!(j["includeCoAuthoredBy"], false);
        assert_eq!(j["permissions"]["allow"][0], "Bash(git *)");
        assert_eq!(j["env"]["DISABLE_AUTOUPDATER"], "1");
        assert_eq!(j["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:6950");
        assert_eq!(j["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "200000"); // overwritten by plan
    }

    #[test]
    fn apply_creates_env_object_when_absent() {
        let mut j = serde_json::json!({ "model": "opus" });
        let plan = simple_plan(vec![], Some(200_000));
        let changed = apply_plan(&mut j, &plan, None).unwrap();
        assert!(changed);
        assert_eq!(j["env"][ENV_KEY], "200000");
    }

    #[test]
    fn apply_no_change_returns_false() {
        let mut j = serde_json::json!({
            "model": "opus",
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000" }
        });
        // Plan with no rewrites and env already equal → no change.
        let plan = simple_plan(vec![], Some(200_000));
        assert!(!apply_plan(&mut j, &plan, None).unwrap());
    }

    #[test]
    fn apply_managed_delete_only_our_last_value() {
        // env_value None + on-disk == last_written → delete
        let mut j = serde_json::json!({
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000", "KEEP": "1" }
        });
        let plan = simple_plan(vec![], None);
        assert!(apply_plan(&mut j, &plan, Some("200000")).unwrap());
        assert!(j["env"].get(ENV_KEY).is_none());
        assert_eq!(j["env"]["KEEP"], "1");
        // env_value None + on-disk == user-authored → untouched
        let mut j2 = serde_json::json!({
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "123456" }
        });
        assert!(!apply_plan(&mut j2, &plan, Some("200000")).unwrap());
        assert_eq!(j2["env"][ENV_KEY], "123456");
        // env_value None + we never wrote → untouched
        let mut j3 = serde_json::json!({
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "123456" }
        });
        assert!(!apply_plan(&mut j3, &plan, None).unwrap());
        assert_eq!(j3["env"][ENV_KEY], "123456");
    }

    #[test]
    fn apply_env_write_also_counts_as_change() {
        // No rewrites, but env key absent and plan wants it → changed.
        let mut j = serde_json::json!({ "model": "opus" });
        let plan = simple_plan(vec![], Some(200_000));
        assert!(apply_plan(&mut j, &plan, None).unwrap());
    }

    #[test]
    fn apply_malformed_shapes_do_not_panic() {
        // "model" not a string, env scalar, root not an object: apply must not panic;
        // rewrites targeting these silently miss, env handling guards.
        let mut j = serde_json::json!({ "model": 42, "env": "oops" });
        let plan = simple_plan(
            vec![(EntryLocation::Model, "42".into(), "x".into())],
            None,
        );
        // No panic; nothing matched → unchanged.
        assert!(!apply_plan(&mut j, &plan, None).unwrap());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml apply_`
Expected: FAIL — `apply_plan` doesn't exist (compile error). Add the signature with a `todo!()` body first if you prefer the test-first cycle to fail at runtime instead of compile time; either failure is acceptable evidence.

- [ ] **Step 3: Implement**

Add to `agent_sync.rs` (and add `thiserror` is already a crate dep — verify with the existing `use thiserror::Error` pattern in `proxy/resolve.rs`; no Cargo.toml change needed):

```rust
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("settings.json 解析失败，跳过本轮")]
    Malformed,
    #[error("写入失败: {0}")]
    Io(#[from] std::io::Error),
}

/// True when `json[loc]` currently equals `raw`.
fn location_holds(json: &serde_json::Value, loc: &EntryLocation, raw: &str) -> bool {
    match loc {
        EntryLocation::Model => json.get("model").and_then(|v| v.as_str()) == Some(raw),
        EntryLocation::Env(key) => json
            .get("env")
            .and_then(|e| e.get(*key))
            .and_then(|v| v.as_str())
            == Some(raw),
    }
}

fn set_at_location(json: &mut serde_json::Value, loc: &EntryLocation, new_name: &str) {
    match loc {
        EntryLocation::Model => {
            if json.get("model").map(|v| v.is_string()).unwrap_or(false) {
                json["model"] = serde_json::Value::String(new_name.to_string());
            }
        }
        EntryLocation::Env(key) => {
            if let Some(serde_json::Value::Object(env)) = json.get_mut("env") {
                if env.get(*key).map(|v| v.is_string()).unwrap_or(false) {
                    env.insert(
                        key.to_string(),
                        serde_json::Value::String(new_name.to_string()),
                    );
                }
            }
        }
    }
}

/// Spec §5 step 6: read-modify-write on the parsed value. Only touches the model-name
/// locations named by `plan.rewrites` (matching the raw name read this round) and the
/// single `ENV_KEY` entry. Returns whether anything changed.
pub fn apply_plan(
    json: &mut serde_json::Value,
    plan: &SyncPlan,
    last_written_env: Option<&str>,
) -> Result<bool, SyncError> {
    let mut changed = false;

    for (loc, raw, new_name) in &plan.rewrites {
        if location_holds(json, loc, raw) {
            set_at_location(json, loc, new_name);
            changed = true;
        }
    }

    let want = plan.env_value.map(|v| v.to_string());
    match want {
        Some(want) => {
            let current = json
                .get("env")
                .and_then(|e| e.get(ENV_KEY))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if current.as_deref() != Some(want.as_str()) {
                let env = match json.get_mut("env") {
                    Some(serde_json::Value::Object(env)) => env,
                    _ => {
                        // absent or non-object: only create when absent
                        if json.get("env").is_some() {
                            return Ok(changed); // non-object env: never touch
                        }
                        json.as_object_mut()
                            .ok_or(SyncError::Malformed)?
                            .insert("env".into(), serde_json::Value::Object(Default::default()));
                        json.get_mut("env").unwrap().as_object_mut().unwrap()
                    }
                };
                env.insert(ENV_KEY.into(), serde_json::Value::String(want));
                changed = true;
            }
        }
        None => {
            // Managed-delete: only a value this feature wrote may be removed.
            if let Some(last) = last_written_env {
                let current = json
                    .get("env")
                    .and_then(|e| e.get(ENV_KEY))
                    .and_then(|v| v.as_str());
                if current == Some(last) {
                    if let Some(serde_json::Value::Object(env)) = json.get_mut("env") {
                        env.remove(ENV_KEY);
                        changed = true;
                    }
                }
            }
        }
    }

    Ok(changed)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync`
Expected: PASS (all Task 2 + Task 3 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_sync.rs
git commit -m "feat(sync): apply_plan 原地改写 settings.json（受管删除仅限自己写入的值）"
```

---

### Task 4: Async shell — `sync_once`, task spawn, status bookkeeping

**Files:**
- Modify: `src-tauri/src/agent_sync.rs` (add shell + tests)

**Interfaces:**
- Consumes: `crate::proxy::{AppState, AppStateInner}` (types in `src-tauri/src/proxy/state.rs`; `AppState = Arc<AppStateInner>` — verify the exact alias before coding), `state.config.read().await`, `state.catalog.read().await`, `state.clock.now_local()`, Tasks 2-3 pure core.
- Produces:

```rust
/// One full sync round. Reads the toggle; off → returns early. Every error is logged,
/// never returned upward (the loop must not die). Updates `status`.
pub async fn sync_once(state: &AppState, status: &SyncBookkeeping);

/// Shared mutable bookkeeping: last env value written by this feature + last round
/// outcome (for get_claude_sync_status). Managed in Task 5 via tauri `app.manage`.
#[derive(Default)]
pub struct SyncBookkeeping {
    inner: std::sync::Mutex<SyncBookkeepingInner>,
}
pub struct SyncBookkeepingInner {
    pub last_written_env: Option<String>,
    pub last_round: Option<LastRound>,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct LastRound {
    pub at: String,              // RFC3339 local time
    pub action: String,          // "written" | "no_change" | "skipped"
    pub detail: String,          // human summary (notes / write set / skip reason)
}
impl SyncBookkeeping {
    pub fn snapshot(&self) -> Option<LastRound>;
    pub fn last_written_env(&self) -> Option<String>;
}

/// Spawn the background loop: run `sync_once` immediately, then every 30s.
/// Checks the toggle inside each round (a flip is honored within one tick).
pub fn spawn_sync_task(state: AppState);
```

- [ ] **Step 1: Write the failing tests**

The shell reads the real user home — that must be injectable. Tests use `tempfile::tempdir` as a fake home and construct the shell around a path parameter, so the testable unit is `sync_round(state, status, settings_path) -> ()`. Append to `mod tests`:

```rust
    // ---- sync round (async shell core, real files in tempdir) ----

    use crate::proxy::state::{AppState, AppStateInner};
    use std::sync::Arc;

    async fn test_state(cfg: AppConfig) -> AppState {
        Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: tokio::sync::RwLock::new(plan_catalog()),
            secrets: crate::config::SecretStoreHandle::default(), // adjust to real constructor
            health: Default::default(),
            clock: Arc::new(crate::proxy::SystemClock),
            usage_cache: Default::default(),
            bound_port: Default::default(),
            server_handle: Default::default(),
            bind_error: Default::default(),
            polling_handle: Default::default(),
            last_served_provider: Default::default(),
            recorder: Default::default(),
            statistics: None,
        })
    }

    fn round_cfg() -> AppConfig {
        let mut cfg = plan_cfg();
        cfg.settings.sync_claude_context = true;
        cfg
    }

    #[tokio::test]
    async fn round_writes_then_settles() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "model": "pro[1m]",
            "env": { "ANTHROPIC_DEFAULT_SONNET_MODEL": "flash" }
        }).to_string()).unwrap();

        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;

        let j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["model"], "pro", "[1m] stripped (200k)");
        assert_eq!(j["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "flash[1m]");
        assert_eq!(j["env"][ENV_KEY], "200000");
        assert_eq!(status.last_written_env().as_deref(), Some("200000"));

        // Second round: nothing changes → file content byte-identical.
        let before = std::fs::read_to_string(&path).unwrap();
        sync_round(&state, &status, &path).await;
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "no-change round must not rewrite");
    }

    #[tokio::test]
    async fn round_toggle_off_never_touches_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::json!({"model": "pro[1m]"}).to_string();
        std::fs::write(&path, &original).unwrap();

        let mut cfg = round_cfg();
        cfg.settings.sync_claude_context = false;
        let state = test_state(cfg).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn round_corrupt_json_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{ not json".to_string();
        std::fs::write(&path, &original).unwrap();

        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(status.snapshot().is_some(), "skipped round still recorded");
        assert_eq!(status.snapshot().unwrap().action, "skipped");
    }

    #[tokio::test]
    async fn round_missing_file_is_quiet_skip() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert!(!path.exists(), "missing file → no file created");
    }

    #[tokio::test]
    async fn round_managed_delete_after_all_1m() {
        // Round 1 writes env=200000 (entry "pro", 200k). Round 2: a Tuesday strategy
        // flips "pro" to a 1M model → entry becomes pro[1m], env_value None → our
        // value deleted; user keys kept.
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "model": "pro",
            "env": { "KEEP": "1" }
        }).to_string()).unwrap();

        let mut cfg = round_cfg();
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s1".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_big".into(),
            }),
        }];
        let state = test_state(cfg).await;
        let status = SyncBookkeeping::default();

        // Monday (backing, 200k): writes env.
        // NOTE: SystemClock gives the real wall-clock weekday; to force Monday we
        // swap the clock for a FakeClock in test_state — see Step 3's clock parameter.
        // (test_state's clock field: Arc::new(crate::proxy::SystemClock) — replace with
        //  Arc::new(fake) where fake: FakeClock with set_local(1, 0).)
        let fake = crate::proxy::health::FakeClock::new(0);
        fake.set_local(1, 0);
        // rebuild state with the fake clock:
        let state = { /* same as test_state but clock: Arc::new(fake) */ test_state(round_cfg()).await };
        sync_round(&state, &status, &path).await;
        let j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["env"][ENV_KEY], "200000");

        // Tuesday (strategy → 1M): env deleted, KEEP preserved.
        // (FakeClock is behind a plain Arc — construct a second state with set_local(2,0).)
        // For plan simplicity this test may instead call compute_plan+apply_plan directly
        // with two LocalNow values and one SyncBookkeeping across both applies.
    }
```

**Important simplification allowed:** the last test (`round_managed_delete_after_all_1m`) may drop the `AppState` async shell and exercise `compute_plan` + `apply_plan` + `SyncBookkeeping` directly with two pinned `LocalNow` values — the managed-delete path is what it verifies. Choose whichever keeps `test_state` simple; if the clock-injection plumbing gets heavy, use the direct variant:

```rust
    #[test]
    fn managed_delete_after_all_1m_direct() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "model": "pro", "env": { "KEEP": "1" }
        }).to_string()).unwrap();

        let mut cfg = round_cfg();
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s1".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_big".into(),
            }),
        }];
        let cat = plan_catalog();
        let status = SyncBookkeeping::default();

        // Monday: write env.
        let mut j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entries = collect_entries(&j);
        let plan = compute_plan(&cfg, &cat, &entries, &LocalNow { weekday: 1, minute: 0 });
        let changed = apply_plan(&mut j, &plan, status.last_written_env().as_deref()).unwrap();
        assert!(changed);
        std::fs::write(&path, serde_json::to_string_pretty(&j).unwrap()).unwrap();
        status.record_written_env(plan.env_value.map(|v| v.to_string()));

        // Tuesday: strategy flips to 1M → env None → managed delete.
        let mut j2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entries2 = collect_entries(&j2);
        let plan2 = compute_plan(&cfg, &cat, &entries2, &LocalNow { weekday: 2, minute: 0 });
        assert!(apply_plan(&mut j2, &plan2, status.last_written_env().as_deref()).unwrap());
        assert!(j2["env"].get(ENV_KEY).is_none());
        assert_eq!(j2["env"]["KEEP"], "1");
        assert_eq!(j2["model"], "pro[1m]");
    }
```

(This direct variant is the **recommended** form — keep it, drop the async one.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml round_`
Expected: FAIL — `sync_round` / `SyncBookkeeping` don't exist (compile error).

- [ ] **Step 3: Implement**

Add to `agent_sync.rs`:

```rust
// ---- bookkeeping (in-memory only; forgets on restart by design) ----

#[derive(Debug, Clone, serde::Serialize)]
pub struct LastRound {
    pub at: String,
    pub action: String, // "written" | "no_change" | "skipped"
    pub detail: String,
}

#[derive(Default)]
pub struct SyncBookkeeping {
    inner: std::sync::Mutex<(Option<String>, Option<LastRound>)>, // (last_written_env, last_round)
}

impl SyncBookkeeping {
    pub fn record_written_env(&self, v: Option<String>) {
        self.inner.lock().unwrap().0 = v;
    }
    pub fn record_round(&self, r: LastRound) {
        self.inner.lock().unwrap().1 = Some(r);
    }
    pub fn last_written_env(&self) -> Option<String> {
        self.inner.lock().unwrap().0.clone()
    }
    pub fn snapshot(&self) -> Option<LastRound> {
        self.inner.lock().unwrap().1.clone()
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Resolve the user's home dir (cross-platform, no new dep). None if neither env var set.
fn home_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
}

/// One round, parameterized by the settings path (tests pass a tempdir path).
/// Every failure path records a skipped round and returns quietly.
pub async fn sync_round(
    state: &crate::proxy::AppState,
    status: &SyncBookkeeping,
    path: &std::path::Path,
) {
    let (cfg, catalog, now) = {
        let cfg = state.config.read().await;
        if !cfg.settings.sync_claude_context {
            return; // off — do nothing, touch nothing
        }
        let catalog = state.catalog.read().await;
        (cfg.clone(), catalog.clone(), state.clock.now_local())
    };

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            // 未安装 Claude Code / 文件不存在：正常情况，静默跳过（debug 级日志）。
            tracing::debug!(target: "switchlm::sync", "settings.json 不存在，跳过同步");
            return;
        }
    };
    let mut json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "settings.json 解析失败，跳过本轮（文件未改动）: {e}");
            status.record_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "settings.json 解析失败".into(),
            });
            return;
        }
    };

    let entries = collect_entries(&json);
    let plan = compute_plan(&cfg, &catalog, &entries, &now);
    for note in &plan.notes {
        tracing::warn!(target: "switchlm::sync", "{note}");
    }
    let last_env = status.last_written_env();
    match apply_plan(&mut json, &plan, last_env.as_deref()) {
        Ok(false) => {
            status.record_round(LastRound {
                at: now_rfc3339(), action: "no_change".into(), detail: "无变化".into(),
            });
        }
        Ok(true) => {
            let body = serde_json::to_string_pretty(&json).unwrap_or_default();
            match std::fs::write(path, body) {
                Ok(()) => {
                    tracing::info!(target: "switchlm::sync", path = %path.display(), "已同步 Claude Code 上下文声明");
                    status.record_written_env(plan.env_value.map(|v| v.to_string()));
                    status.record_round(LastRound {
                        at: now_rfc3339(), action: "written".into(),
                        detail: format!(
                            "重写 {} 项, env={:?}",
                            plan.rewrites.len(),
                            plan.env_value.map(|v| v.to_string())
                        ),
                    });
                }
                Err(e) => {
                    tracing::warn!(target: "switchlm::sync", "写入失败（下轮重试）: {e}");
                    status.record_round(LastRound {
                        at: now_rfc3339(), action: "skipped".into(),
                        detail: format!("写入失败: {e}"),
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "应用同步计划失败: {e}");
        }
    }
}

/// Run one round against the real user path.
pub async fn sync_once(state: &crate::proxy::AppState, status: &SyncBookkeeping) {
    if let Some(home) = home_dir() {
        sync_round(state, status, &claude_settings_path(&home)).await;
    }
}

/// Background loop: immediate run + every 30s. Toggle checked inside each round.
pub fn spawn_sync_task(state: crate::proxy::AppState, status: std::sync::Arc<SyncBookkeeping>) {
    tauri::async_runtime::spawn(async move {
        loop {
            sync_once(&state, &status).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
```

Note for the implementer: check `src-tauri/src/proxy/state.rs` for the exact `AppStateInner` field set when writing `test_state` — the struct in the test must list every field (copy the literal from `lib.rs:155-175` setup). If `secrets: SecretStoreHandle` has no `Default`, construct via the same call used in tests elsewhere in the repo — grep `SecretStoreHandle` in `src-tauri/src` for the existing test constructor and reuse it. Also confirm `AppState`'s type alias name (`pub type AppState = Arc<AppStateInner>;` or similar) and adjust `use` lines.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_sync.rs
git commit -m "feat(sync): sync_round 异步壳 + SyncBookkeeping 状态记录"
```

---

### Task 5: Commands, registration, and app wiring

**Files:**
- Modify: `src-tauri/src/commands.rs` (`SettingsView` ~line 905, `get_settings` ~line 914; new commands near `set_background_destroy` ~line 1121)
- Modify: `src-tauri/src/lib.rs` (invoke_handler list ~line 222; setup ~line 200)

**Interfaces:**
- Consumes: `agent_sync::{SyncBookkeeping, LastRound}` from Task 4; `Settings.sync_claude_context` from Task 1; `persist` helper (`commands.rs:1350`).
- Produces: Tauri commands `set_sync_claude_context(enabled: bool) -> ()` and `get_claude_sync_status() -> ClaudeSyncStatus`, registered and wired; `SyncBookkeeping` managed as Tauri state.

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeSyncStatus {
    pub enabled: bool,
    pub path: String,
    pub last_round: Option<agent_sync::LastRound>,
}
```

- [ ] **Step 1: Write the failing tests**

Add to `commands.rs` `mod tests` (it has existing settings-command tests — mirror one, e.g. the `set_log_level`-style test if present; if settings commands are only tested through `get_settings` fixtures, add config-level assertions):

```rust
    #[test]
    fn settings_view_exposes_sync_claude_context() {
        let mut cfg = AppConfig::default();
        cfg.settings.sync_claude_context = true;
        // get_settings is async + State-bound; assert the view mapping via a direct
        // construction mirroring get_settings's body:
        let v = SettingsView {
            port: cfg.settings.port,
            autostart: cfg.settings.autostart,
            usage_refresh_interval_secs: clamp_usage_refresh_secs(cfg.settings.usage_refresh_interval_secs),
            log_level: normalize_log_level(&cfg.settings.log_level),
            request_recording: cfg.settings.request_recording,
            background_destroy: cfg.settings.background_destroy,
            sync_claude_context: cfg.settings.sync_claude_context,
        };
        assert!(v.sync_claude_context);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml settings_view_exposes`
Expected: FAIL — `SettingsView` has no `sync_claude_context` field (compile error).

- [ ] **Step 3: Implement**

1. `SettingsView` (`commands.rs` ~905): add `pub sync_claude_context: bool,` and set it in `get_settings` from `cfg.settings.sync_claude_context`. Update the doc comment above the struct.

2. New commands after `set_background_destroy`:

```rust
/// 开关 Claude Code 上下文自动同步：持久化设置（默认关）。关闭时不动已写入的任何内容。
/// 见 docs/superpowers/specs/2026-08-15-claude-context-sync-design.md。
#[tauri::command]
pub async fn set_sync_claude_context(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        config.settings.sync_claude_context = enabled;
        persist(&app, &config)?;
    }
    Ok(())
}

/// Claude Code 同步状态（设置页展示）：开关、目标文件路径、最近一轮结果。
#[tauri::command]
pub async fn get_claude_sync_status(
    state: State<'_, AppState>,
    bk: tauri::State<'_, std::sync::Arc<crate::agent_sync::SyncBookkeeping>>,
) -> Result<ClaudeSyncStatus, String> {
    let cfg = state.config.read().await;
    let path = crate::agent_sync::claude_settings_path(&std::path::PathBuf::from(
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .unwrap_or_default(),
    ));
    Ok(ClaudeSyncStatus {
        enabled: cfg.settings.sync_claude_context,
        path: path.display().to_string(),
        last_round: bk.snapshot(),
    })
}
```

(If `home_dir()` from Task 4 is `pub`, call `crate::agent_sync::home_dir()` instead of re-deriving — prefer that; make `home_dir` pub in Task 4 or here.)

3. `lib.rs`:
   - invoke_handler: add `commands::set_sync_claude_context,` and `commands::get_claude_sync_status,` after `commands::set_background_destroy,`.
   - setup, after `app.manage(state);` (~line 182) and before the tray block:

```rust
            // Claude Code 上下文声明自动同步（默认关）：后台 30s 轮询，开关在每轮内检查。
            let sync_status = std::sync::Arc::new(crate::agent_sync::SyncBookkeeping::default());
            crate::agent_sync::spawn_sync_task(state.clone(), sync_status.clone());
            app.manage(sync_status);
```

   - Module declaration was added in Task 2; verify `pub mod agent_sync;` exists.

- [ ] **Step 4: Run tests + full check**

Run: `cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -5`
Expected: PASS (whole suite — confirms no regressions and the wiring compiles; `cargo build` warnings about unused `sync_once` would mean the spawn wiring missed, check for that).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(sync): set_sync_claude_context / get_claude_sync_status 命令与后台任务接线"
```

---

### Task 6: Frontend — types, commands, store, Settings UI

**Files:**
- Modify: `src/lib/types.ts` (`SettingsView` ~line 174)
- Modify: `src/lib/commands.ts` (~line 141, next to `setBackgroundDestroy`)
- Modify: `src/stores/system.ts`
- Modify: `src/views/Settings.vue`

**Interfaces:**
- Consumes: backend commands from Task 5 (`sync_claude_context` field in SettingsView; `ClaudeSyncStatus` shape).
- Produces: `system.claudeSyncStatus` ref + `saveSyncClaudeContext()` + `loadClaudeSyncStatus()`; Settings card「同步模型上下文到 Claude Code」.

- [ ] **Step 1: Types + commands (no test harness — type-check is the gate)**

`src/lib/types.ts` — extend `SettingsView` and add the status mirror:

```ts
export interface SettingsView {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
  log_level: string;
  request_recording: boolean;
  background_destroy: boolean;
  sync_claude_context: boolean;
}

// Mirrors src-tauri `LastRound` (agent_sync.rs).
export interface ClaudeSyncLastRound {
  at: string;
  action: "written" | "no_change" | "skipped" | string;
  detail: string;
}

// Mirrors src-tauri `ClaudeSyncStatus` (commands.rs).
export interface ClaudeSyncStatus {
  enabled: boolean;
  path: string;
  last_round: ClaudeSyncLastRound | null;
}
```

`src/lib/commands.ts`:

```ts
export const setSyncClaudeContext = (enabled: boolean) =>
  invoke<void>("set_sync_claude_context", { enabled });
export const getClaudeSyncStatus = () => invoke<ClaudeSyncStatus>("get_claude_sync_status");
```

(Import `ClaudeSyncStatus` in the type import list at the top of the file.)

- [ ] **Step 2: Store**

`src/stores/system.ts` — add state + actions, and export them:

```ts
import type { ClaudeSyncStatus, EnvSnippet, SecretStatusView, ServerStatus, SettingsView } from "../lib/types";
// ...
  const claudeSyncStatus = ref<ClaudeSyncStatus | null>(null);
// ...
  async function loadClaudeSyncStatus() {
    claudeSyncStatus.value = await api.getClaudeSyncStatus();
  }

  async function saveSyncClaudeContext(enabled: boolean) {
    await api.setSyncClaudeContext(enabled);
    await loadSettings();
    await loadClaudeSyncStatus();
  }
// add to the return object:
//   claudeSyncStatus, loadClaudeSyncStatus, saveSyncClaudeContext,
```

- [ ] **Step 3: Settings.vue card**

Insert a new card after「后台内存优化」(`Settings.vue:217-225`), and add the toggle handler + 60s status polling:

```vue
<!-- handler -->
async function toggleSyncClaudeContext(on: boolean) {
  try {
    await system.saveSyncClaudeContext(on);
    msg.success(on ? "已开启 Claude Code 上下文同步" : "已关闭 Claude Code 上下文同步（已写入内容保持不变）");
  } catch (e) {
    msg.error(`设置失败：${String(e)}`);
  }
}

const SYNC_ACTION_TEXT: Record<string, string> = {
  written: "已同步",
  no_change: "无变化",
  skipped: "已跳过",
};
```

```html
    <NCard title="Claude Code 上下文同步" size="small">
      <NSpace vertical :size="10">
        <NSpace align="center" :size="12">
          <NSwitch
            :value="system.settings?.sync_claude_context ?? false"
            @update:value="(v: boolean) => toggleSyncClaudeContext(v)"
          />
          <span class="muted">按当前路由模型的真实上下文，自动调整 Claude Code 模型名的 [1m] 声明与 CLAUDE_CODE_MAX_CONTEXT_TOKENS</span>
        </NSpace>
        <NSpace align="center" :size="12">
          <span class="muted">
            目标文件 {{ system.claudeSyncStatus?.path ?? "…" }} · 对新会话生效 · 未匹配路由的模型名不做改动 · 默认关闭
          </span>
        </NSpace>
        <span v-if="system.claudeSyncStatus?.last_round" class="muted">
          最近同步：{{ SYNC_ACTION_TEXT[system.claudeSyncStatus.last_round.action] ?? system.claudeSyncStatus.last_round.action }}
          （{{ system.claudeSyncStatus.last_round.detail }}）
        </span>
      </NSpace>
    </NCard>
```

Polling: in `Settings.vue` add (mirror the `usePolling` usage in `Usage.vue:220`):

```ts
import { usePolling } from "../lib/usePolling";
// ...
usePolling(() => system.loadClaudeSyncStatus(), () => 60_000);
```

Also load once on mount — extend the `onMounted` call: `await Promise.all([system.refresh(), system.loadSettings(), system.loadClaudeSyncStatus()]);`

- [ ] **Step 4: Verify**

Run: `pnpm exec vue-tsc --noEmit`
Expected: no errors.

Run: `pnpm build`
Expected: type-check + vite build both succeed.

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts src/views/Settings.vue
git commit -m "feat(sync): 设置页 Claude Code 上下文同步开关与状态展示"
```

---

### Task 7: End-to-end smoke (manual, optional automation)

**Files:** none (verification only)

- [ ] **Step 1: Manual run**

Run: `pnpm tauri dev`
1. Settings → toggle「Claude Code 上下文同步」on. Watch `~/.claude/settings.json` (keep it open in an editor / `Get-Content -Wait`).
2. Expect within ≤30s: model entries whose routed model is 200k lose `[1m]`; 1M entries gain it; `CLAUDE_CODE_MAX_CONTEXT_TOKENS` appears with the min size.
3. In 路由/Profiles page, flip a time strategy window so the primary route model crosses tiers; within ≤30s the file updates again.
4. Toggle off → flip strategies → file must NOT change.
5. Check log tail (`<app_data>/logs/switchlm.log`, target `switchlm::sync`) for the vendor+model identified write lines.

- [ ] **Step 2: Full suites one more time**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && pnpm exec vue-tsc --noEmit`
Expected: all PASS.

- [ ] **Step 3: Commit (if any fixups)**

```bash
git add <explicit fixup paths>
git commit -m "fix(sync): 冒烟测试修正"
```

---

## Self-Review (done at plan time)

- **Spec coverage:** §5 steps 1-6 → Tasks 2-4; §6.1 module → Tasks 2-4; §6.2 config/commands → Tasks 1, 5; §6.3 lifecycle → Task 5; §6.4 frontend → Task 6; §7 error table → Task 4 (`sync_round` paths + malformed-file tests); §8 testing → Tasks 2-5 test steps. No gap found.
- **Type consistency:** `SyncPlan { rewrites: Vec<(EntryLocation, String, String)>, env_value, notes }` used identically in Tasks 2/3/4; `SyncBookkeeping` methods (`record_written_env`, `record_round`, `last_written_env`, `snapshot`) match across Tasks 4/5; `ClaudeSyncStatus { enabled, path, last_round }` matches Task 5 Rust and Task 6 TS.
- **One deviation from spec noted inline:** Task 4 recommends the *direct* (non-async) form of the managed-delete test to avoid FakeClock plumbing; behavior covered is identical.
