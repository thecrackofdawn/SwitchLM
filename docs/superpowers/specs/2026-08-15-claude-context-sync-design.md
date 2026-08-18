# Claude Code Context-Window Auto-Sync

**Date:** 2026-08-15
**Status:** Design — pending implementation plan
**Branch:** dev directly on `main` (repo convention)

## 1. Problem

SwitchLM routes the agent-facing model name (Profile) to different upstream models by time-of-day strategies. Those upstream models have different real context windows (e.g. glm-4.6 = 200k, glm-5.2 = 1M). Claude Code, however, decides its auto-compact threshold from the model *name* it is configured with — it never learns the real window of whatever model actually served the request.

Result: when time-based routing switches the Profile's start model across context tiers, Claude Code keeps compacting against a stale assumption — either burning the session with premature 200k compacts on a 1M model, or running past the real limit on a 200k model.

## 2. Goal

An opt-in setting (default **off**): a background task periodically rewrites the model-name `[1m]` declarations — and, where needed, `CLAUDE_CODE_MAX_CONTEXT_TOKENS` — in Claude Code's `~/.claude/settings.json` so that Claude Code's assumed context window tracks the *primary route model* (the time-strategy winner, not fallback hops) of each configured model entry.

Scope: **Claude Code only** (this spec). The proxy, translation, breaker, tray, and catalog structures are untouched.

## 3. How Claude Code resolves the context window (reverse-engineered, v2.1.226)

Verified from the local `claude.exe` binary; the resolution chain (functions `U3` / `gT` / `Kmf`) for a model name `M`:

```
1. M ends with "[1m]" (case-insensitive)   → window = 1_000_000, DONE
2. autoCompactWindow / CLAUDE_CODE_AUTO_COMPACT_WINDOW
   (settings / env, clamped 100k..1M, min'd with model cap) → that value
3. known "claude-*" model                  → built-in table
4. CLAUDE_CODE_MAX_CONTEXT_TOKENS (only when M does NOT start with "claude-")
                                            → that value
5. otherwise                               → 200_000 default,
   and "unknown-model enforcement" compacts at that default
```

Key facts this design relies on:

- **`[1m]` beats everything** — including `CLAUDE_CODE_MAX_CONTEXT_TOKENS`. A `pro[1m]` entry is a hard-coded 1M declaration.
- **`CLAUDE_CODE_MAX_CONTEXT_TOKENS` only applies to non-`claude-` names** — exactly our case (all SwitchLM Profiles are non-claude names).
- **`[1m]` is a declaration, not part of the model name sent upstream.** Claude Code strips `[1m]`/`[2m]` before sending `model` in the API request (its `dd()` normalizer). The proxy already receives the clean name, so **no proxy change is needed** for renamed entries.
- Unrecognized names default to 200k with active enforcement.

## 4. Decisions (locked in brainstorming)

| Decision | Choice |
|---|---|
| Which model is synced | The **primary route model** (`profile_start_model` — time-strategy winner, else backing). Fallback hops are NOT tracked. |
| Where the source of truth for "what Claude Code uses" lives | Read Claude Code's own settings: `model` + `env.ANTHROPIC_MODEL` + `env.ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`. Each entry is resolved through SwitchLM independently. |
| Mechanism per entry | Adjust the `[1m]` suffix on the **name** (primary lever, per-entry); write `env.CLAUDE_CODE_MAX_CONTEXT_TOKENS` only for entries left without `[1m]`. |
| Toggle | `Settings.sync_claude_context: bool`, default `false`. |
| Toggle off | Stop syncing; **never modify or delete anything already written** (the values may predate the feature / be hand-written). |
| Trigger | Background task: run once at startup, then poll every 30s. Not request-driven. |
| Unresolvable entry (name matches no Profile) | Skip that entry entirely (no rename, excluded from env calc) + `warn` log. |
| Entry resolving to a model with no catalog context size | Skip that entry + `warn` log. |
| Entries disagree on the env value | Take the **minimum** (conservative: no route exceeds its real window). |
| Malformed / missing settings.json | Skip the round entirely; never write. |

## 5. Sync algorithm

For each round (startup + every 30s, only when `sync_claude_context` is on):

1. **Read** `~/.claude/settings.json`. Missing file, invalid JSON, or `env` present-but-not-an-object → skip round (log at debug/info; a missing file is normal if Claude Code was never installed).
2. **Collect entries**: the string values of top-level `model`, `env.ANTHROPIC_MODEL`, `env.ANTHROPIC_DEFAULT_OPUS_MODEL`, `env.ANTHROPIC_DEFAULT_SONNET_MODEL`, `env.ANTHROPIC_DEFAULT_HAIKU_MODEL`. Deduplicate by exact string; preserve each key's location for write-back. Non-string / empty values are ignored.
3. **Per entry**, strip a trailing `[1m]` (case-insensitive) to get the base name → resolve via SwitchLM's Profile chain (`resolve_model` semantics: Profile name-or-alias → `profile_start_model(now)` → Model → Provider.vendor) → look up `context_size` in the effective catalog. Any failure → skip entry + `warn` (log vendor + upstream name per repo convention; for unresolved names log the requested name).
4. **Per entry, decide the rewrite** (size = real context of the primary route model):

   | Name today | Real size | Action on the name |
   |---|---|---|
   | has `[1m]` | ≥ 1_000_000 | keep |
   | has `[1m]` | < 1_000_000 | **remove `[1m]`** |
   | no `[1m]` | ≥ 1_000_000 | **append `[1m]`** |
   | no `[1m]` | < 1_000_000 | keep (env applies) |

5. **Env value**: among entries that (after rewrite) carry no `[1m]`, take the minimum size. If at least one such entry exists → `env.CLAUDE_CODE_MAX_CONTEXT_TOKENS = min` (as a JSON string, matching Claude Code's env conventions). If **no** entry remains without `[1m]`:
   - If our internal record says the previous round wrote this key (and the on-disk value still equals what we wrote) → delete the key.
   - Otherwise (value absent, or differs from what we wrote — user-authored) → leave untouched.
6. **Write-back only if something changed**: read-modify-write the JSON `Value` in place (mutate only the affected `model` / `env` keys), preserving every other key and object shape. Compare before/after; identical → no write (don't touch mtime).

### Example round

```jsonc
// before
{ "model": "opus",
  "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:6950",
           "ANTHROPIC_DEFAULT_SONNET_MODEL": "pro[1m]",
           "ANTHROPIC_DEFAULT_HAIKU_MODEL": "flash" } }
// "opus"  → Profile → primary model glm-4.6 (200k)  → no [1m] needed → env candidate 200k
// "pro"   → Profile → primary model glm-4.6 (200k)  → strip [1m]
// "flash" → Profile → primary model deepseek-v4 (1M) → append [1m]
// after
{ "model": "opus",
  "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:6950",
           "ANTHROPIC_DEFAULT_SONNET_MODEL": "pro",
           "ANTHROPIC_DEFAULT_HAIKU_MODEL": "flash[1m]",
           "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000" } }
```

## 6. Architecture

### 6.1 New Rust module `src-tauri/src/agent_sync.rs`

Pure, testable core + thin async shell:

```rust
// ---- pure core ----
pub const ENV_KEY: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";

fn claude_settings_path() -> PathBuf;              // {home}/.claude/settings.json (same on Win/Linux;
                                                   // home via std::env::os_str / HOME|USERPROFILE — no new dep)
fn strip_1m(name: &str) -> &str;                  // trailing "[1m]" case-insensitive; identity otherwise

/// The model names Claude Code is configured with, keyed by where they live.
pub struct ClaudeEntries { /* (location, raw_name) pairs: Model | Env(&'static str) */ }

fn collect_entries(json: &serde_json::Value) -> ClaudeEntries;

/// One entry's plan.
pub enum EntryPlan {
    Skip { reason: SkipReason },                  // unresolvable name / no catalog size
    Keep { name: String },
    Rewrite { from: String, to: String },         // [1m] add/remove
}

/// Whole-round decision: per-entry plans + the env value to write (or managed-delete).
pub struct SyncPlan {
    rewrites: Vec<(EntryLocation, String /*new name*/)>,
    env_value: Option<u32>,                       // Some(min) → write; None → maybe managed-delete
}

fn compute_plan(cfg: &AppConfig, catalog: &ProviderCatalog, entries: &ClaudeEntries,
                now: &LocalNow) -> SyncPlan;

/// Apply to a JSON value in place; returns true if anything changed (caller decides to persist).
fn apply_plan(json: &mut serde_json::Value, plan: &SyncPlan,
              last_written_env: Option<&str>) -> Result<bool, SyncError>;

// ---- async shell ----
pub async fn sync_once(state: &AppState);          // full round; every error logged, never panics
pub fn spawn_sync_task(state: AppState);           // startup run + 30s loop; checks the toggle each round
```

Notes:

- **In-memory bookkeeping only** (not persisted): the last env value this feature wrote, kept in a `Mutex<Option<String>>` inside the sync task / a small managed struct. Used solely for the managed-delete rule (§5 step 5). A restart simply forgets it (then a stale self-written key is left until the next round writes over it — acceptable).
- Claude Code reads settings.json at session start / on change; a rename mid-session affects the *next* session/turn boundary. The Settings page copy should set that expectation ("对已开启的会话在下次启动/新会话生效").
- The write path serializes the whole `Value` back with `to_string_pretty`-style formatting (2-space, matching Claude Code's own file style). serde_json's default map is fine; key order preservation is nice-to-have, not required.

### 6.2 Config & commands

- `Settings.sync_claude_context: bool` — `#[serde(default)]`, default `false` (`config/types.rs` + `Default` impl).
- `commands.rs`:
  - `set_sync_claude_context(state, enabled)` — persist + save config (template: `set_request_recording`).
  - `get_claude_sync_status(state) -> ClaudeSyncStatus` — for the Settings page: `{ enabled, last_round: Option<{ at: RFC3339, action: Written|NoChange|Skipped(reason), details }>, path: String }`. Backed by the same in-memory struct the task updates.
- Register in `lib.rs::invoke_handler`; `spawn_sync_task(state.clone())` next to the tray-refresh spawn (§6.3 of lib.rs setup).

### 6.3 Task lifecycle

- Spawned once at app setup (after `app.manage(state)`), runs for the app's lifetime.
- Each 30s tick: read `settings.sync_claude_context` under the config lock; off → do nothing (cheap). This also means a toggle flip is honored within one tick without any extra plumbing.
- Toggling on triggers the next tick (≤30s) — acceptable; no immediate-kick plumbing in v1. (Optional nicety: `set_sync_claude_context` spawns a one-shot `sync_once` when turning on.)

### 6.4 Frontend

- `src/lib/types.ts`: add `sync_claude_context: boolean` to `SettingsView` + new `ClaudeSyncStatus` mirror (keep snake_case, hand-maintained mirror convention).
- `src/lib/commands.ts`: `setSyncClaudeContext`, `getClaudeSyncStatus` wrappers.
- `src/stores/system.ts`: expose status + actions (canonical pattern: mutate → re-fetch).
- `src/views/Settings.vue`: new NSwitch「同步模型上下文到 Claude Code」 under a new settings group, with helper copy explaining: reads `~/.claude/settings.json` model entries, adjusts `[1m]` / `CLAUDE_CODE_MAX_CONTEXT_TOKENS` to the primary route model's real window; takes effect for new sessions; entries not mapping to a SwitchLM Profile are left untouched. Status line (polled 60s via `usePolling`): last action + time + the file path.

## 7. Error handling

| Situation | Behavior |
|---|---|
| settings.json missing | Skip round, debug log. Normal (no Claude Code installed). |
| settings.json invalid JSON / env not an object | Skip round, `warn` log. **Never write** — the file is user-owned. |
| Entry name resolves to no Profile | Skip entry, `warn` with the requested name. |
| Profile → primary model has no catalog size | Skip entry, `warn` with vendor + upstream name. |
| Write I/O failure | `warn` log; retried next tick (state is re-read every round, so the retry is self-correcting). |
| Toggle flipped off mid-run | Next tick sees it and stops acting; written values stay (§4). |
| Clock | Use the existing injectable `Clock`/`LocalNow` (deterministic in tests), consistent with dispatch. |

All logs identify vendor + upstream model name where applicable (repo convention); the target file path is logged on every actual write (info).

## 8. Testing

All in `agent_sync.rs` `#[cfg(test)] mod tests` + config/store tests; no new deps:

- `strip_1m`: exact, case-insensitive, absent, `[2m]` untouched.
- `collect_entries`: all five sources, dedup, non-string/empty ignored, missing keys tolerated.
- `compute_plan`: the four rewrite branches (§5 table); skip-on-unresolvable; skip-on-no-size; min-env across mixed entries; all-`[1m]` → env None; strategy-winner switch changes the plan (time-flip test with two `LocalNow`s).
- `apply_plan` (tempdir + real file): rewrites only what changed; unrelated keys/env entries byte-preserved; no-change round does not rewrite (content identical); managed-delete removes only the exact last-written value, leaves user-authored values; corrupted JSON returns error without writing.
- Config: `Settings::default().sync_claude_context == false`; old config without the field loads as `false` (serde default).
- Command-level: `set_sync_claude_context` persists + round-trips through `get_settings` (mirror existing settings-command tests).

No wiremock needed — no upstream HTTP is involved.

## 9. Out of scope (v1)

- Other agents (Cursor, Cline) — the module boundary (`agent_sync.rs`, `EntryLocation` abstraction) leaves room.
- Syncing fallback-hop models, per-entry `CLAUDE_CODE_MAX_CONTEXT_TOKENS` (impossible — one global env).
- Writing `autoCompactWindow` / `CLAUDE_CODE_AUTO_COMPACT_WINDOW`.
- Immediate sync on toggle-on (v1 relies on the 30s tick; optional kick noted in §6.3).
- Persisting sync bookkeeping across restarts.
