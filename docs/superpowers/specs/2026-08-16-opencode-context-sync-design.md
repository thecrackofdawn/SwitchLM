# OpenCode Context-Window Auto-Sync (agent_sync target #2)

**Date:** 2026-08-16
**Status:** Design — pending implementation plan
**Branch:** dev directly on `main` (repo convention)
**Predecessor:** `docs/superpowers/specs/2026-08-15-claude-context-sync-design.md` (implemented, commits 3c0aa73..359f8f5)

## 1. Problem

OpenCode (opencode.ai coding agent) decides when to auto-compact from `limit.context` in its config. For a custom provider like SwitchLM there is **no built-in model table** — an unset `limit.context` is `0`, which disables auto-compact entirely; a stale `1000000` makes it compact against a window the current routed model doesn't have. Same problem the Claude Code sync solved, different file and different lever.

## 2. Goal

Extend the existing `agent_sync` background feature to a second target: `~/.config/opencode/opencode.jsonc`. One shared toggle (the existing `sync_claude_context`, semantically widened to "sync agent context"), one 30s loop, per-agent rounds and per-agent status.

## 3. How OpenCode resolves the context window (verified)

From the local `opencode.exe` binary (`Is` / `Dl` functions) + the official `https://opencode.ai/config.json` schema + docs source:

```
auto-compact threshold =
    limit.input present ? max(0, limit.input - reserved)
                        : max(0, limit.context - maxOutputTokens)
reserved = cfg.compaction?.reserved ?? min(20000, maxOutputTokens)
limit.context === 0  → auto-compact disabled
auto === false       → disabled
```

- Config path (global): `~/.config/opencode/opencode.jsonc` — same layout Windows/Linux (verified present on this machine).
- Model entry shape: `provider.<id>.models.<model>.limit = { context: number, input?: number, output: number }` (schema-required: context + output; the local user file proves `output` is tolerated missing in practice).
- The file is **JSONC** (comments + trailing commas allowed) — cannot be parsed by plain `serde_json`.
- No `[1m]`-style name magic; the model key is a literal Profile name/alias.

## 4. Decisions (locked in brainstorming)

| Decision | Choice |
|---|---|
| Scope of models synced | Only providers whose `options.baseURL` points at the SwitchLM proxy. Other providers untouched. |
| Toggle | **Single shared toggle** (existing `Settings.sync_claude_context`). Off → neither agent is touched. |
| JSONC comments on write-back | **Accepted loss** — strip comments, write back pretty JSON. |
| What is written | `limit.context` per matched model entry (a JSON **number**). Never `limit.output` / `limit.input`. |
| Per-entry independence | Yes — each model key gets its own true size (no cross-entry minimum like the Claude Code env). |
| Unmatched model key (no Profile / no catalog size) | Skip entry + note; never written. |
| Fallback hops | Not tracked (primary route model only, same as Claude Code target). |
| Malformed config | Skip round, never write (same family of rules as the Claude Code target). |

## 5. Sync algorithm (OpenCode round)

Run after the Claude Code round in the same 30s tick, when the toggle is on:

1. Read `~/.config/opencode/opencode.jsonc`. Missing → silent debug skip. Read error (≠ NotFound) → `warn` + skipped round. 
2. `strip_jsonc` → parse as JSON. Either step fails → `warn` + skipped round, file untouched.
3. If `provider` is present but not an object → skip whole round (mirror of the Claude `env`-shape rule).
4. Resolve the proxy port from `state.bound_port` (actual bound port). `None` → skip this round's OpenCode part entirely (Claude round unaffected).
5. For each provider entry (object) with string `options.baseURL` **equal to** `http://127.0.0.1:{port}/v1` or **starting with** `http://127.0.0.1:{port}`: treat as a SwitchLM provider. Non-object provider values, missing or non-string `baseURL` → provider skipped silently (not an error — a foreign provider is normal).
6. For each key in that provider's `models` object (non-object `models` → provider skipped): resolve the key as a Profile name-or-alias → `profile_start_model(now)` → Model → vendor → catalog `context_size`. Failure → skip entry + note. Success → target `models.<key>.limit.context = size` (create `limit` object if absent; `output`/`input` left alone).
7. If any target value differs from on-disk → write the whole file back as pretty JSON (comments lost, accepted). Identical → no write.

### Example round

```jsonc
// before (~/.config/opencode/opencode.jsonc)
{ "$schema": "https://opencode.ai/config.json",
  "provider": {
    "switchlm": {
      "options": { "baseURL": "http://127.0.0.1:6950/v1" },
      "models": {
        "pro":   { "options": { "reasoningEffort": "high" },
                   "limit": { "context": 1000000 } },
        "flash": { "limit": { "context": 200000 } }
      }
    },
    "directio": { "options": { "baseURL": "https://api.deepseek.com/v1" },
                  "models": { "deepseek-v4-pro": {} } }
  }
}
// "pro" → Profile → primary model glm-4.6 (200k) → limit.context: 1000000 → 200000
// "flash" → Profile → primary model glm-5.2 (1M) → already 1000000? no: 200000 → 1000000
// "directio" provider: baseURL not ours → untouched
// after (pretty JSON, comments gone)
{ "$schema": "...", "provider": { "switchlm": { "options": { "baseURL": "http://127.0.0.1:6950/v1" },
  "models": { "pro": { "options": { "reasoningEffort": "high" }, "limit": { "context": 200000 } },
              "flash": { "limit": { "context": 1000000 } } } },
  "directio": { ...unchanged... } } }
```

## 6. Architecture changes

### 6.1 `agent_sync.rs` additions

```rust
/// ~/.config/opencode/opencode.jsonc (same path Windows/Linux).
pub fn opencode_config_path(home: &std::path::Path) -> std::path::PathBuf;

/// JSONC → JSON: state machine stripping // and /* */ comments (string-literal aware)
/// and trailing commas before } ]. Returns None if the input is not even structurally
/// traversable. Pure function, no deps.
fn strip_jsonc(text: &str) -> Option<String>;

/// Provider ids whose options.baseURL targets the local proxy (exact
/// `http://127.0.0.1:{port}/v1` or prefix `http://127.0.0.1:{port}`). Empty `provider`
/// section → empty vec. Non-object provider → None (caller skips the round).
fn switchlm_providers(json: &serde_json::Value, port: u16) -> Option<Vec<String>>;

/// OpenCode round; same shape and logging conventions as `sync_round` (Claude target).
pub async fn sync_opencode_round(
    state: &crate::proxy::AppState,
    status: &SyncBookkeeping,
    path: &std::path::Path,
);
```

`spawn_sync_task`'s loop body becomes: `sync_once(claude)` then `sync_opencode_once(state, status)` (both no-ops when toggle off). Both read the toggle independently (same early-return pattern).

### 6.2 Bookkeeping: per-agent rounds

`SyncBookkeeping` gains a second round slot. Concrete shape (two named fields, not a map):

```rust
pub struct SyncBookkeeping {
    inner: Mutex<BookkeepingInner>,
}
struct BookkeepingInner {
    last_written_env: Option<String>,          // Claude target only (managed-delete)
    last_notes: Option<String>,                // Claude target only
    claude_round: Option<LastRound>,
    opencode_round: Option<LastRound>,
}
// record_round gains an agent parameter, or becomes record_claude_round /
// record_opencode_round + snapshot_claude()/snapshot_opencode(). Prefer explicit
// named methods (no enum dispatch for two cases).
```

### 6.3 Status command rename

`get_claude_sync_status` → `get_agent_sync_status`; `ClaudeSyncStatus` → `AgentSyncStatus`:

```rust
#[derive(Serialize)]
pub struct AgentSyncStatus {
    pub enabled: bool,
    pub claude_path: String,
    pub opencode_path: String,
    pub claude_last_round: Option<LastRound>,
    pub opencode_last_round: Option<LastRound>,
}
```

The feature has never shipped in a release, so the rename carries no compat burden. `set_sync_claude_context` **keeps its name** (the persisted `Settings.sync_claude_context` field keeps its name too — renaming a persisted field would orphan existing configs; the UI copy changes instead).

### 6.4 Frontend

- `types.ts`: `ClaudeSyncStatus` → `AgentSyncStatus` mirror (snake_case, hand-maintained).
- `commands.ts`: `getClaudeSyncStatus` → `getAgentSyncStatus` (invoke name `get_agent_sync_status`); `setSyncClaudeContext` unchanged.
- `system.ts` store: rename status ref/actions accordingly.
- `Settings.vue`: card title「同步模型上下文到编码智能体」; description mentions both targets; two status lines (Claude Code / OpenCode), each showing action + detail; both file paths shown.

## 7. Error handling

| Situation | Behavior |
|---|---|
| File missing | Silent debug skip, no round recorded (same as Claude target). |
| Read error ≠ NotFound | `warn` + skipped round. |
| `strip_jsonc` returns None / JSON parse fails | `warn` + skipped round, file untouched. |
| `provider` present but not an object | Skip round, file untouched. |
| `bound_port` None (proxy not up) | Skip OpenCode round this tick (debug log); Claude round unaffected. |
| Provider value not an object / baseURL not a string | Provider silently skipped (normal config shape). |
| `models` not an object | Provider skipped silently. |
| Model key unresolvable / no catalog size | Entry skipped + note (into the OpenCode round's `detail`, following the notes mechanism added in 359f8f5). |
| Write I/O failure | `warn` + skipped round; retried next tick. |
| Toggle off | Both rounds return before reading either file. |

Logging conventions unchanged: vendor + upstream model names, never `m_xxx` ids; notes surface in `LastRound.detail` and warn only when the note set changes.

## 8. Testing

All co-located in `agent_sync.rs` tests:

- `strip_jsonc`: line comments, block comments, `//` inside string literals preserved, trailing commas (object + array + nested), unterminated block comment → None, unchanged JSON passthrough.
- `switchlm_providers`: exact `/v1` match, bare-prefix match, wrong port no match, foreign host no match, missing baseURL, non-object provider → None, empty section → Some(vec![]).
- `sync_opencode_round` (tempdir real files): writes `limit.context` for matched entries; creates `limit` when absent; leaves `output`/`input` and other options untouched; foreign provider byte-untouched; unmatched model skipped (entry unchanged, note recorded); no-change round does not rewrite (content byte-identical); JSONC input with comments round-trips to valid JSON with correct values; corrupt file untouched; toggle off untouched; `bound_port` None skips round.
- Refactor fallout: existing Claude-round tests updated for the bookkeeping split; full `agent_sync` suite green.
- No HTTP involved — no wiremock.

## 9. Out of scope

- Writing `limit.output` / `limit.input`.
- JSONC comment preservation on write-back.
- OpenCode project-level configs (`.opencode/`, `opencode.json` in cwd) — global file only.
- The OpenCode `compaction` config block (reserved/auto/prune) — user-owned.
- Cursor / Cline (future targets; the `agent_sync` module boundary keeps room).
