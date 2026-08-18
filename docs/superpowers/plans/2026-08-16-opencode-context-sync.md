# OpenCode Context-Window Auto-Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing agent_sync background feature with a second target — `~/.config/opencode/opencode.jsonc` — writing per-model `limit.context` from the SwitchLM primary-route model's real window, behind the same shared toggle.

**Architecture:** New pure pieces in `src-tauri/src/agent_sync.rs` (`strip_jsonc` state machine, `switchlm_providers` matcher, `sync_opencode_round` async shell) plus a bookkeeping split into per-agent round slots and a status-command rename to `get_agent_sync_status` (frontend mirrors). Spec: `docs/superpowers/specs/2026-08-16-opencode-context-sync-design.md`.

**Tech Stack:** Rust (tauri 2, tokio, serde_json, chrono), Vue 3 `<script setup>` + Pinia + Naive UI (Chinese UI).

## Global Constraints

- **Spec of record:** `docs/superpowers/specs/2026-08-16-opencode-context-sync-design.md`. Read §5 (algorithm) and §7 (error table) before coding.
- **Single shared toggle**: existing `Settings.sync_claude_context` (persisted name unchanged; UI copy changes). Off → neither agent's files are touched.
- **Scope:** only providers whose `options.baseURL` equals `http://127.0.0.1:{bound_port}/v1` or starts with `http://127.0.0.1:{bound_port}` (port = `state.bound_port()`, the ACTUAL bound port). Other providers untouched, byte-for-byte.
- **Write only `limit.context`** (JSON number). Never `limit.output` / `limit.input` / anything else.
- **JSONC**: strip comments + trailing commas via state machine; write back pretty JSON (comment loss accepted). Never write on parse failure.
- **Never write on malformed input**: missing file (silent debug), read error ≠ NotFound (warn + skipped round), strip/parse failure (warn + skipped round), `provider` present-but-not-object (skip round).
- **`bound_port` None → skip OpenCode round this tick** (Claude round unaffected).
- Unmatched model key (no Profile / no catalog size) → skip entry + note; never written.
- **Logs identify vendor + upstream model name** (never `m_xxx` ids); notes surface in `LastRound.detail`, warn only when the note set changes (the 359f8f5 mechanism).
- **Serde field names snake_case, no rename_all**; `types.ts` is a hand-maintained mirror — keep in sync.
- **Frontend package manager is pnpm** (`pnpm exec vue-tsc --noEmit`), never npm.
- **No new crate dependencies.**
- **Tests co-located** `#[cfg(test)]`; run via `cargo test --manifest-path src-tauri/Cargo.toml agent_sync`.
- **Commits: dev on `main` directly; `git add` explicit paths only, never `-A`.**

## File Structure

| File | Responsibility |
|---|---|
| Modify `src-tauri/src/agent_sync.rs` | `opencode_config_path`, `strip_jsonc`, `switchlm_providers`, `sync_opencode_round`, bookkeeping split, loop wiring |
| Modify `src-tauri/src/commands.rs` | `ClaudeSyncStatus` → `AgentSyncStatus` (two paths + two rounds), command rename `get_agent_sync_status` |
| Modify `src-tauri/src/lib.rs` | invoke_handler entry rename |
| Modify `src/lib/types.ts` | `ClaudeSyncStatus` → `AgentSyncStatus` mirror |
| Modify `src/lib/commands.ts` | `getClaudeSyncStatus` → `getAgentSyncStatus` |
| Modify `src/stores/system.ts` | status ref/actions rename |
| Modify `src/views/Settings.vue` | card copy + two status lines |

Task order: 1 (strip_jsonc) → 2 (provider matching + round) → 3 (bookkeeping split + loop + status rename) → 4 (frontend). The pre-existing flaky test `proxy::dispatch::tests::transient_retry_exhausted_trips_and_falls_back` occasionally fails (~1/3 runs) at baseline — **not** caused by this work; if it fails, re-run once and move on if green.

---

### Task 1: `strip_jsonc` + `opencode_config_path`

**Files:**
- Modify: `src-tauri/src/agent_sync.rs` (add near the other path helpers, ~line 32)

**Interfaces:**
- Produces (Task 2 consumes):
  - `pub fn opencode_config_path(home: &std::path::Path) -> std::path::PathBuf`
  - `fn strip_jsonc(text: &str) -> Option<String>` — None only on structurally untraversable input (unterminated block comment / unterminated string literal); trailing commas and comments always removed.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `agent_sync.rs`:

```rust
    // ---- strip_jsonc / opencode_config_path ----

    #[test]
    fn opencode_path_joins_config_dir() {
        use std::path::Path;
        let p = opencode_config_path(Path::new("/home/u"));
        assert!(p.ends_with(".config/opencode/opencode.jsonc")
            || p.to_string_lossy().contains("opencode.jsonc"));
    }

    #[test]
    fn strip_jsonc_passthrough_plain_json() {
        let s = r#"{"a":1,"b":[1,2],"c":{"d":"x"}}"#;
        assert_eq!(strip_jsonc(s).unwrap(), s);
    }

    #[test]
    fn strip_jsonc_line_and_block_comments() {
        let s = "{\n// line\n\"a\": 1, /* block */ \"b\": 2\n}";
        assert_eq!(strip_jsonc(s).unwrap(), "{\n\n\"a\": 1,  \"b\": 2\n}");
    }

    #[test]
    fn strip_jsonc_preserves_slashes_inside_strings() {
        let s = r#"{"url":"http://x//y","re":"a/*b*/c"}"#;
        assert_eq!(strip_jsonc(s).unwrap(), s);
    }

    #[test]
    fn strip_jsonc_trailing_commas() {
        assert_eq!(strip_jsonc(r#"{"a":1,}"#).unwrap(), r#"{"a":1}"#);
        assert_eq!(strip_jsonc(r#"{"a":[1,2,3,],}"#).unwrap(), r#"{"a":[1,2,3]}"#);
        // comma before a comment that precedes } still trailing
        assert_eq!(strip_jsonc("{\"a\":1, // c\n}").unwrap(), "{\"a\":1  \n}");
    }

    #[test]
    fn strip_jsonc_unterminated_is_none() {
        assert!(strip_jsonc("{\"a\": /* never closed").is_none());
        assert!(strip_jsonc("{\"a\": \"never closed").is_none());
    }

    #[test]
    fn strip_jsonc_comment_char_inside_string_not_comment() {
        // "//" inside a string must not start a comment; string content kept verbatim.
        let s = "{\"k\": \"va//lue\"}";
        assert_eq!(strip_jsonc(s).unwrap(), s);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml strip_jsonc opencode_path` → compile error (functions don't exist). Add `todo!()` stubs first if you prefer runtime failure; either evidence is fine.

- [ ] **Step 3: Implement**

```rust
/// ~/.config/opencode/opencode.jsonc — global OpenCode config (same path on
/// Windows/Linux; verified against the local installation).
pub fn opencode_config_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".config").join("opencode").join("opencode.jsonc")
}

/// JSONC → JSON text: removes // line comments, /* */ block comments (string-literal
/// aware) and trailing commas before `}` / `]`. Returns None on unterminated block
/// comment or string literal. Write-back is plain JSON — user comments are lost
/// (accepted in spec §4).
fn strip_jsonc(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    #[derive(PartialEq)]
    enum St { Code, Str, Line, Block }
    let mut st = St::Code;
    // Position of the last pending comma in `out` (candidate trailing comma),
    // invalidated whenever any non-whitespace code content follows it.
    let mut pending_comma: Option<usize> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match st {
            St::Code => match c {
                b'"' => { pending_comma = None; st = St::Str; out.push('"'); i += 1; }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                    pending_comma = None; st = St::Line; i += 2;
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                    pending_comma = None; st = St::Block; i += 2;
                }
                b',' => { pending_comma = Some(out.len()); out.push(','); i += 1; }
                b' ' | b'\t' | b'\r' | b'\n' => { out.push(c as char); i += 1; }
                _ => { pending_comma = None; out.push(c as char); i += 1; }
            },
            St::Str => match c {
                b'\\' if i + 1 < bytes.len() => {
                    // copy escape pair verbatim
                    out.push(bytes[i] as char);
                    out.push(bytes[i + 1] as char);
                    i += 2;
                }
                b'"' => { st = St::Code; out.push('"'); i += 1; }
                _ => { out.push(c as char); i += 1; }
            },
            St::Line => {
                if c == b'\n' { st = St::Code; out.push('\n'); }
                i += 1;
            }
            St::Block => {
                if c == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    st = St::Code; i += 2;
                } else {
                    if c == b'\n' { out.push('\n'); } // keep line structure
                    i += 1;
                }
            }
        }
    }
    if st == St::Block || st == St::Str {
        return None; // unterminated
    }
    // Resolve trailing commas: for each pending comma, if only whitespace until the
    // next structural close, remove it.
    if let Some(pos) = pending_comma {
        let rest = &out[pos + 1..];
        if rest.trim_start().starts_with('}') || rest.trim_start().starts_with(']') {
            out.remove(pos);
        }
    }
    Some(out)
}
```

Note on the trailing-comma test expectation `"{\n\n\"a\": 1,  \"b\": 2\n}"`: the block comment ` /* block */ ` is replaced by nothing but the surrounding spaces remain (`1, ` + ` ` before `"b"`). If your implementation collapses differently, adjust the TEST to the implementation's actual output **only** after verifying by hand that the result still parses as `{"a":1,"b":2}` with serde_json — semantic equivalence is the requirement, byte equality is convenience. Prefer asserting `serde_json::from_str::<serde_json::Value>(&strip_jsonc(s).unwrap()).unwrap()` equals the expected Value where byte-exactness is brittle:

```rust
    #[test]
    fn strip_jsonc_semantics_via_parse() {
        for (input, expect) in [
            ("{\n// line\n\"a\": 1, /* block */ \"b\": 2\n}", json!({"a":1,"b":2})),
            ("{\"a\":1, // c\n}", json!({"a":1})),
            ("{\"a\":[1,2,3,],}", json!({"a":[1,2,3]})),
        ] {
            let stripped = strip_jsonc(input).unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&stripped).unwrap(), expect);
        }
    }
```

Keep BOTH styles: byte-exact where stable (`passthrough_plain_json`, `preserves_slashes_inside_strings`), parse-based where whitespace is incidental.

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml strip_jsonc opencode_path`
Expected: PASS (all new tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_sync.rs
git commit -m "feat(sync): strip_jsonc 状态机与 opencode 配置路径"
```

---

### Task 2: `switchlm_providers` + `sync_opencode_round`

**Files:**
- Modify: `src-tauri/src/agent_sync.rs`

**Interfaces:**
- Consumes: `opencode_config_path`, `strip_jsonc` (Task 1); `strip_1m`, `ENV_KEY`-style consts and `resolve_size`-equivalent resolution (existing); `SyncBookkeeping::{record_notes, last_notes, ...}` (existing — Task 3 splits rounds, this task uses `record_round` for the Claude slot temporarily? **No** — see note); `AppStateInner::bound_port()` (`proxy/state.rs:91`, `pub fn bound_port(&self) -> Option<u16>`).
- Produces (Task 3 consumes):
  - `fn switchlm_providers(json: &serde_json::Value, port: u16) -> Option<Vec<String>>` — None = `provider` present but not an object (caller skips round); Some(ids) = providers whose `options.baseURL` is a string equal to `http://127.0.0.1:{port}/v1` or starting with `http://127.0.0.1:{port}`.
  - `pub async fn sync_opencode_round(state: &crate::proxy::AppState, status: &SyncBookkeeping, path: &std::path::Path)`
  - `pub async fn sync_opencode_once(state: &AppState, status: &SyncBookkeeping)` — resolves home, calls round. **Order dependency:** Task 3 renames the bookkeeping round methods; to avoid double-refactor, this task writes `sync_opencode_round` to take an explicit `record: &dyn Fn(LastRound)`? No — overengineering. This task lands AFTER Task 3's bookkeeping split in the same plan execution order? **Decision: swap order — implement Task 3's bookkeeping split FIRST is cleaner.** Final task order in this plan: Task 1 = strip_jsonc (above), Task 2 = bookkeeping split + loop + status rename, Task 3 = switchlm_providers + opencode round, Task 4 = frontend. The tasks below are renumbered accordingly — follow the RENUMBERED order.

> **Executed plan order: Task 1 (done above) → Task 2 (bookkeeping split) → Task 3 (opencode round) → Task 4 (frontend).**

### Task 2: Bookkeeping split + loop wiring + status rename

**Files:**
- Modify: `src-tauri/src/agent_sync.rs` (~line 286-312 `SyncBookkeeping`; ~line 457-472 `sync_once`/`spawn_sync_task`)
- Modify: `src-tauri/src/commands.rs` (~1146-1186)
- Modify: `src-tauri/src/lib.rs` (~286)

**Interfaces:**
- Produces (Task 3 consumes):
  - `SyncBookkeeping` with named methods:
    ```rust
    pub fn record_claude_round(&self, r: LastRound);
    pub fn record_opencode_round(&self, r: LastRound);
    pub fn snapshot_claude(&self) -> Option<LastRound>;
    pub fn snapshot_opencode(&self) -> Option<LastRound>;
    ```
    (`record_round`/`snapshot` removed; `record_written_env`/`last_written_env`/`record_notes`/`last_notes` unchanged — Claude-target-only fields.)
  - `spawn_sync_task` loop calls `sync_once` (Claude) then `sync_opencode_once` — but `sync_opencode_once` doesn't exist until Task 3. **Wire it in Task 3, not here.** This task only does the rename/split.
  - Command `get_agent_sync_status(state, bk) -> AgentSyncStatus` with fields:
    ```rust
    #[derive(Debug, Clone, Serialize)]
    pub struct AgentSyncStatus {
        pub enabled: bool,
        pub claude_path: String,
        pub opencode_path: String,
        pub claude_last_round: Option<agent_sync::LastRound>,
        pub opencode_last_round: Option<agent_sync::LastRound>,
    }
    ```
    (opencode_path via `agent_sync::opencode_config_path` from Task 1.)

- [ ] **Step 1: Update tests to the new API (they are the failing tests)**

In `agent_sync.rs` tests, replace every `status.record_round(` with `status.record_claude_round(` and `status.snapshot()` with `status.snapshot_claude()` (grep shows they are used in `round_corrupt_json_left_untouched`, `round_missing_file_is_quiet_skip`, `round_writes_then_settles`). Add one new test:

```rust
    #[test]
    fn bookkeeping_rounds_are_independent() {
        let bk = SyncBookkeeping::default();
        assert!(bk.snapshot_claude().is_none());
        assert!(bk.snapshot_opencode().is_none());
        bk.record_claude_round(LastRound { at: "t1".into(), action: "written".into(), detail: "c".into() });
        bk.record_opencode_round(LastRound { at: "t2".into(), action: "skipped".into(), detail: "o".into() });
        assert_eq!(bk.snapshot_claude().unwrap().action, "written");
        assert_eq!(bk.snapshot_opencode().unwrap().action, "skipped");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync` → compile errors (`record_claude_round` not found). 

- [ ] **Step 3: Implement**

Replace the `SyncBookkeeping` struct + impl (agent_sync.rs ~286-312):

```rust
#[derive(Default)]
pub struct SyncBookkeeping {
    inner: std::sync::Mutex<BookkeepingInner>,
}

#[derive(Default)]
struct BookkeepingInner {
    /// Claude 目标专用：本功能最后一次写入的 ENV_KEY 值（受管删除的判定依据）。
    last_written_env: Option<String>,
    /// Claude 目标专用：上一轮跳过原因集合（拼接串），"只在变化时 warn"去重。
    last_notes: Option<String>,
    claude_round: Option<LastRound>,
    opencode_round: Option<LastRound>,
}

impl SyncBookkeeping {
    pub fn record_written_env(&self, v: Option<String>) {
        self.inner.lock().unwrap().last_written_env = v;
    }
    pub fn record_notes(&self, v: Option<String>) {
        self.inner.lock().unwrap().last_notes = v;
    }
    pub fn last_written_env(&self) -> Option<String> {
        self.inner.lock().unwrap().last_written_env.clone()
    }
    pub fn last_notes(&self) -> Option<String> {
        self.inner.lock().unwrap().last_notes.clone()
    }
    pub fn record_claude_round(&self, r: LastRound) {
        self.inner.lock().unwrap().claude_round = Some(r);
    }
    pub fn record_opencode_round(&self, r: LastRound) {
        self.inner.lock().unwrap().opencode_round = Some(r);
    }
    pub fn snapshot_claude(&self) -> Option<LastRound> {
        self.inner.lock().unwrap().claude_round.clone()
    }
    pub fn snapshot_opencode(&self) -> Option<LastRound> {
        self.inner.lock().unwrap().opencode_round.clone()
    }
}
```

Update `sync_round` body's `status.record_round(` → `status.record_claude_round(` (all call sites inside it).

In `commands.rs` (~1146-1186): rename `ClaudeSyncStatus` → `AgentSyncStatus` with the fields above; rename command `get_claude_sync_status` → `get_agent_sync_status`; build both paths from `agent_sync::home_dir()`; use `bk.snapshot_claude()` / `bk.snapshot_opencode()`; update the doc comments (「Claude Code / OpenCode 同步状态视图」). `set_sync_claude_context` keeps its name (persisted field name unchanged) but update its doc comment to mention both targets.

In `lib.rs` invoke_handler (~286): `commands::get_claude_sync_status,` → `commands::get_agent_sync_status,`.

- [ ] **Step 4: Run tests + full check**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync && cargo check --manifest-path src-tauri/Cargo.toml --all-targets 2>&1 | grep -c warning` 
Expected: all agent_sync tests PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_sync.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "refactor(sync): SyncBookkeeping 按目标拆分轮次，状态命令改名 get_agent_sync_status"
```

---

### Task 3: `switchlm_providers` + `sync_opencode_round` + loop wiring

**Files:**
- Modify: `src-tauri/src/agent_sync.rs`

**Interfaces:**
- Consumes: `opencode_config_path`, `strip_jsonc` (Task 1); `record_opencode_round`/`record_notes`/`last_notes` (Task 2); `state.bound_port()`; `resolve_size` — note it's currently `fn resolve_size(...) -> Result<Option<u32>, (String, String)>` (private). Reuse as-is.
- Produces:
  - `fn switchlm_providers(json: &serde_json::Value, port: u16) -> Option<Vec<String>>`
  - `pub async fn sync_opencode_round(state: &AppState, status: &SyncBookkeeping, path: &std::path::Path)`
  - `pub async fn sync_opencode_once(state: &AppState, status: &SyncBookkeeping)`
  - `spawn_sync_task` loop runs both `sync_once` and `sync_opencode_once`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests`:

```rust
    // ---- switchlm_providers ----

    fn oc_doc(base: Option<&str>) -> serde_json::Value {
        let mut prov = serde_json::Map::new();
        if let Some(b) = base {
            prov.insert("options".into(), json!({ "baseURL": b }));
        }
        json!({ "provider": { "switchlm": prov } })
    }

    #[test]
    fn switchlm_providers_matches_exact_and_prefix() {
        let p = 6950u16;
        assert_eq!(
            switchlm_providers(&oc_doc(Some("http://127.0.0.1:6950/v1")), p),
            Some(vec!["switchlm".to_string()])
        );
        assert_eq!(
            switchlm_providers(&oc_doc(Some("http://127.0.0.1:6950")), p),
            Some(vec!["switchlm".to_string()])
        );
    }

    #[test]
    fn switchlm_providers_rejects_foreign() {
        let p = 6950u16;
        assert_eq!(switchlm_providers(&oc_doc(Some("http://127.0.0.1:6951/v1")), p), Some(vec![]));
        assert_eq!(switchlm_providers(&oc_doc(Some("https://api.deepseek.com/v1")), p), Some(vec![]));
        assert_eq!(switchlm_providers(&oc_doc(Some("http://localhost:6950/v1")), p), Some(vec![]));
        assert_eq!(switchlm_providers(&oc_doc(None), p), Some(vec![])); // no baseURL
    }

    #[test]
    fn switchlm_providers_shape_errors() {
        // provider present but not an object → None (caller skips round)
        assert_eq!(switchlm_providers(&json!({ "provider": "oops" }), 6950), None);
        // absent provider → empty vec, NOT an error
        assert_eq!(switchlm_providers(&json!({}), 6950), Some(vec![]));
    }

    // ---- sync_opencode_round ----

    fn oc_state(port: Option<u16>) -> crate::proxy::AppState {
        // Build the same AppStateInner as test_state() but with bound_port set.
        // test_state() is the existing fixture helper in this module — extend it with
        // a port parameter OR clone the pattern here:
        // (see existing test_state; minimal version below)
        let state = await_blocked(); // placeholder — replaced below
        state
    }
```

The async-state fixture already exists as `test_state(cfg)` in this module — extend the approach: add a small helper that sets `bound_port` after building:

```rust
    fn with_port(state: &crate::proxy::AppState, port: Option<u16>) {
        *state.bound_port.lock().unwrap() = port;
    }
```

(`bound_port` is `pub Mutex<Option<u16>>` on `AppStateInner` — direct assignment is fine in tests; `set_bound_port(&self, port: u16)` and `clear_bound_port(&self)` also exist, `proxy/state.rs:86-98`.)

Round tests (mirror the Claude `round_*` tests):

```rust
    #[tokio::test]
    async fn oc_round_writes_limit_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "provider": {
                "switchlm": {
                    "options": { "baseURL": "http://127.0.0.1:6950/v1" },
                    "models": {
                        "pro":   { "options": { "reasoningEffort": "high" },
                                   "limit": { "context": 1000000 } },
                        "flash": { "limit": { "context": 200000 } },
                        "weird": {}
                    }
                },
                "direct": {
                    "options": { "baseURL": "https://api.deepseek.com/v1" },
                    "models": { "deepseek-v4-pro": {} }
                }
            }
        }).to_string()).unwrap();

        let mut cfg = round_cfg();          // existing helper: toggle on + pro/flash profiles
        // "pro" → m_small (200k), "flash" → m_big (1M), "weird" → no profile
        let state = test_state(cfg).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();

        sync_opencode_round(&state, &status, &path).await;

        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["options"]["reasoningEffort"], "high", "sibling options kept");
        assert_eq!(j["provider"]["switchlm"]["models"]["flash"]["limit"]["context"], 1_000_000);
        assert!(j["provider"]["switchlm"]["models"]["weird"].as_object().unwrap().is_empty(), "unmatched entry untouched (no limit created)");
        assert_eq!(j["provider"]["direct"]["models"]["deepseek-v4-pro"].as_object().unwrap().len(), 0, "foreign provider untouched");
        let r = status.snapshot_opencode().unwrap();
        assert_eq!(r.action, "written");
        assert!(r.detail.contains("weird"), "skip note surfaced in detail");

        // Settle: second round, no change → byte-identical file.
        let before = std::fs::read_to_string(&path).unwrap();
        sync_opencode_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(status.snapshot_opencode().unwrap().action, "no_change");
    }

    #[tokio::test]
    async fn oc_round_creates_limit_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json!({
            "provider": { "switchlm": {
                "options": { "baseURL": "http://127.0.0.1:6950/v1" },
                "models": { "pro": { "options": { "textVerbosity": "low" } } }
            }}
        }).to_string()).unwrap();
        let state = test_state(round_cfg()).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["options"]["textVerbosity"], "low");
        // output/input NOT invented
        assert!(j["provider"]["switchlm"]["models"]["pro"]["limit"].get("output").is_none());
    }

    #[tokio::test]
    async fn oc_round_jsonc_comments_survive_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n  // my note\n  \"provider\": { \"switchlm\": {\n    \"options\": { \"baseURL\": \"http://127.0.0.1:6950/v1\", },\n    \"models\": { \"pro\": {}, }\n  } }\n}\n").unwrap();
        let state = test_state(round_cfg()).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        // write-back is plain JSON: no comment markers remain
        assert!(!std::fs::read_to_string(&path).unwrap().contains("//"));
    }

    #[tokio::test]
    async fn oc_round_no_port_skips() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = json!({ "provider": { "switchlm": {
            "options": { "baseURL": "http://127.0.0.1:6950/v1" },
            "models": { "pro": {} }
        }}}).to_string();
        std::fs::write(&path, &original).unwrap();
        let state = test_state(round_cfg()).await;
        with_port(&state, None); // proxy not bound
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original, "untouched");
    }

    #[tokio::test]
    async fn oc_round_toggle_off_and_corrupt_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // toggle off
        let original = json!({ "provider": { "switchlm": {
            "options": { "baseURL": "http://127.0.0.1:6950/v1" }, "models": { "pro": {} }
        }}}).to_string();
        std::fs::write(&path, &original).unwrap();
        let mut cfg = round_cfg();
        cfg.settings.sync_claude_context = false;
        let state = test_state(cfg).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        // corrupt
        std::fs::write(&path, "{ not json").unwrap();
        let state2 = test_state(round_cfg()).await;
        with_port(&state2, Some(6950));
        sync_opencode_round(&state2, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        assert_eq!(status.snapshot_opencode().unwrap().action, "skipped");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml switchlm_providers oc_round` → compile errors.

- [ ] **Step 3: Implement**

Add to `agent_sync.rs`:

```rust
/// Provider ids in an OpenCode config whose `options.baseURL` targets the local
/// proxy. Matching: baseURL == `http://127.0.0.1:{port}/v1` OR starts with
/// `http://127.0.0.1:{port}` (covers a bare root or deeper path). `None` = the
/// `provider` section exists but isn't an object (caller skips the round);
/// `Some(vec![])` = no provider matches (normal — nothing to do).
fn switchlm_providers(json: &serde_json::Value, port: u16) -> Option<Vec<String>> {
    let providers = match json.get("provider") {
        None => return Some(vec![]),
        Some(serde_json::Value::Object(p)) => p,
        Some(_) => return None,
    };
    let exact = format!("http://127.0.0.1:{port}/v1");
    let prefix = format!("http://127.0.0.1:{port}");
    let mut out = vec![];
    for (id, v) in providers {
        let base = v
            .get("options")
            .and_then(|o| o.get("baseURL"))
            .and_then(|b| b.as_str());
        if let Some(b) = base {
            if b == exact || b.starts_with(&prefix) {
                out.push(id.clone());
            }
        }
    }
    Some(out)
}

/// One OpenCode round. Same contract family as `sync_round` (Claude target): every
/// failure path records a skipped round and returns quietly.
pub async fn sync_opencode_round(
    state: &crate::proxy::AppState,
    status: &SyncBookkeeping,
    path: &std::path::Path,
) {
    let (cfg, catalog, now, port) = {
        let cfg = state.config.read().await;
        if !cfg.settings.sync_claude_context {
            return; // 单一总开关：关 → 两个目标都不碰
        }
        let port = state.bound_port();
        if port.is_none() {
            // 代理未绑定端口：无法识别 SwitchLM provider，本轮跳过（Claude 轮不受影响）。
            tracing::debug!(target: "switchlm::sync", "代理端口未就绪，跳过 OpenCode 同步");
            return;
        }
        let catalog = state.catalog.read().await;
        (cfg.clone(), catalog.clone(), state.clock.now_local(), port.unwrap())
    };

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(target: "switchlm::sync", "opencode.jsonc 不存在，跳过同步");
            return;
        }
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "opencode.jsonc 读取失败（文件未改动）: {e}");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("opencode.jsonc 读取失败: {e}"),
            });
            return;
        }
    };

    let stripped = match strip_jsonc(&text).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        Some(v) => v,
        None => {
            tracing::warn!(target: "switchlm::sync", "opencode.jsonc 解析失败，跳过本轮（文件未改动）");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "opencode.jsonc 解析失败".into(),
            });
            return;
        }
    };
    let mut json = stripped;

    let targets = match switchlm_providers(&json, port) {
        Some(t) => t,
        None => {
            tracing::warn!(target: "switchlm::sync", "opencode 配置的 provider 字段不是对象，跳过本轮（文件未改动）");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "provider 字段不是对象".into(),
            });
            return;
        }
    };

    // 计划：对每个 SwitchLM provider 的每个模型条目求目标 limit.context。
    struct EntryEdit { provider: String, model: String, size: u32 }
    let mut edits: Vec<EntryEdit> = vec![];
    let mut notes: Vec<String> = vec![];
    for pid in &targets {
        let models = json
            .get("provider").and_then(|p| p.get(pid))
            .and_then(|p| p.get("models"))
            .and_then(|m| m.as_object());
        let Some(models) = models else {
            continue; // models 非对象/缺失 → 该 provider 静默跳过（正常形态）
        };
        for key in models.keys() {
            match resolve_size(&cfg, &catalog, key, &now) {
                Ok(Some(size)) => edits.push(EntryEdit { provider: pid.clone(), model: key.clone(), size }),
                Ok(None) => notes.push(format!("skip '{key}'（未匹配任何路由）")),
                Err((vendor, upstream)) => notes.push(format!(
                    "skip '{key}'（{vendor}/{upstream} 上下文大小未知）"
                )),
            }
        }
    }

    // 应用（只改 limit.context，缺失则创建 limit 对象；不动 output/input）。
    let mut changed = false;
    for e in &edits {
        let limit = json
            .get_mut("provider").and_then(|p| p.get_mut(&e.provider))
            .and_then(|p| p.get_mut("models"))
            .and_then(|m| m.get_mut(&e.model))
            .and_then(|m| m.get_mut("limit"));
        match limit {
            Some(serde_json::Value::Object(l)) => {
                if l.get("context").and_then(|c| c.as_u64()) != Some(e.size as u64) {
                    l.insert("context".into(), serde_json::Value::from(e.size));
                    changed = true;
                }
            }
            _ => {
                // limit 缺失（或非对象——非对象不动，视为用户手写异形）：
                if let Some(m) = json
                    .get_mut("provider").and_then(|p| p.get_mut(&e.provider))
                    .and_then(|p| p.get_mut("models"))
                    .and_then(|m| m.get_mut(&e.model))
                    .and_then(|m| m.as_object_mut())
                {
                    if !m.contains_key("limit") {
                        m.insert("limit".into(), serde_json::json!({ "context": e.size }));
                        changed = true;
                    }
                }
            }
        }
    }

    // notes 去重 warn + 并入 detail（与 Claude 轮相同机制；OpenCode 轮单独记）。
    {
        let note_set = notes.join(" | ");
        let changed_notes = status.oc_notes().as_deref() != Some(note_set.as_str());
        if changed_notes && !notes.is_empty() {
            for n in &notes {
                tracing::warn!(target: "switchlm::sync", "{n}");
            }
        }
        status.record_oc_notes(if notes.is_empty() { None } else { Some(note_set) });
    }
    let notes_detail = |base: &str| -> String {
        match status.oc_notes() {
            Some(n) if !n.is_empty() => format!("{base}（{n}）"),
            _ => base.to_string(),
        }
    };

    if !changed {
        status.record_opencode_round(LastRound {
            at: now_rfc3339(), action: "no_change".into(), detail: notes_detail("无变化"),
        });
        return;
    }

    // 写回：纯 JSON pretty（注释丢失，spec §4 已接受）。序列化不可失败；万一失败
    // 也绝不写空文件。
    let body = match serde_json::to_string_pretty(&json) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "opencode.jsonc 序列化失败（文件未改动）: {e}");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("opencode.jsonc 序列化失败: {e}"),
            });
            return;
        }
    };
    match std::fs::write(path, body) {
        Ok(()) => {
            tracing::info!(target: "switchlm::sync", path = %path.display(), "已同步 OpenCode 上下文声明");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "written".into(),
                detail: notes_detail(&format!("写入 {} 项 limit.context", edits.len())),
            });
        }
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "写入失败（下轮重试）: {e}");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("写入失败: {e}"),
            });
        }
    }
}

/// Run one OpenCode round against the real user path.
pub async fn sync_opencode_once(state: &crate::proxy::AppState, status: &SyncBookkeeping) {
    if let Some(home) = home_dir() {
        sync_opencode_round(state, status, &opencode_config_path(&home)).await;
    }
}
```

**Design decision embedded above:** the OpenCode round needs its OWN notes slot (the Claude round's `last_notes` must not mix with OpenCode's). Extend `BookkeepingInner` with `oc_notes: Option<String>` and add `record_oc_notes` / `oc_notes` methods (mirror `record_notes`/`last_notes`). Fold this into this task (not Task 2) since it's consumed here:

```rust
// BookkeepingInner gains:
    oc_notes: Option<String>,
// SyncBookkeeping gains:
    pub fn record_oc_notes(&self, v: Option<String>) {
        self.inner.lock().unwrap().oc_notes = v;
    }
    pub fn oc_notes(&self) -> Option<String> {
        self.inner.lock().unwrap().oc_notes.clone()
    }
```

Wire the loop (`spawn_sync_task`):

```rust
pub fn spawn_sync_task(state: crate::proxy::AppState, status: std::sync::Arc<SyncBookkeeping>) {
    tauri::async_runtime::spawn(async move {
        loop {
            sync_once(&state, &status).await;
            sync_opencode_once(&state, &status).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync && cargo check --manifest-path src-tauri/Cargo.toml --all-targets 2>&1 | grep -c warning`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_sync.rs
git commit -m "feat(sync): OpenCode 轮——provider 匹配/limit.context 写入/循环接线"
```

---

### Task 4: Frontend rename + two-line status

**Files:**
- Modify: `src/lib/types.ts` (~170-190)
- Modify: `src/lib/commands.ts` (~140-146)
- Modify: `src/stores/system.ts` (~4, 25, 88-96, 127-139)
- Modify: `src/views/Settings.vue` (~115-128, 245-264)

**Interfaces:**
- Consumes: backend `AgentSyncStatus` shape from Task 2.
- Produces: `system.agentSyncStatus` + `loadAgentSyncStatus()` (+ `saveSyncClaudeContext` unchanged name).

- [ ] **Step 1: types.ts**

Replace the `ClaudeSyncStatus`/`ClaudeSyncLastRound` block:

```ts
// Mirrors src-tauri `LastRound` (agent_sync.rs).
export interface AgentSyncLastRound {
  at: string;
  action: "written" | "no_change" | "skipped" | string;
  detail: string;
}

// Mirrors src-tauri `AgentSyncStatus` (commands.rs).
export interface AgentSyncStatus {
  enabled: boolean;
  claude_path: string;
  opencode_path: string;
  claude_last_round: AgentSyncLastRound | null;
  opencode_last_round: AgentSyncLastRound | null;
}
```

- [ ] **Step 2: commands.ts**

```ts
export const getAgentSyncStatus = () => invoke<AgentSyncStatus>("get_agent_sync_status");
```

(Replace `getClaudeSyncStatus`; update the type import list.)

- [ ] **Step 3: system.ts**

- import type `AgentSyncStatus` (replacing `ClaudeSyncStatus`)
- `const agentSyncStatus = ref<AgentSyncStatus | null>(null);` (replacing `claudeSyncStatus`)
- `async function loadAgentSyncStatus() { agentSyncStatus.value = await api.getAgentSyncStatus(); }` (replacing `loadClaudeSyncStatus`; `saveSyncClaudeContext` now calls it)
- update the return-object exports accordingly.

- [ ] **Step 4: Settings.vue**

Handler message update (toggle function keeps its name):

```ts
async function toggleSyncClaudeContext(on: boolean) {
  try {
    await system.saveSyncClaudeContext(on);
    msg.success(on ? "已开启编码智能体上下文同步" : "已关闭编码智能体上下文同步（已写入内容保持不变）");
  } catch (e) {
    msg.error(`设置失败：${String(e)}`);
  }
}
```

Card replacement (title + copy + two status lines):

```html
    <NCard title="编码智能体上下文同步" size="small">
      <NSpace vertical :size="10">
        <NSpace align="center" :size="12">
          <NSwitch
            :value="system.settings?.sync_claude_context ?? false"
            @update:value="(v: boolean) => toggleSyncClaudeContext(v)"
          />
          <span class="muted">按当前路由模型的真实上下文，自动调整 Claude Code（[1m] 声明 + CLAUDE_CODE_MAX_CONTEXT_TOKENS）与 OpenCode（limit.context）</span>
        </NSpace>
        <span class="muted">
          Claude Code：{{ system.agentSyncStatus?.claude_path ?? "…" }} · 对新会话生效
        </span>
        <span class="muted">
          OpenCode：{{ system.agentSyncStatus?.opencode_path ?? "…" }} · 写回会丢失注释 · 仅同步指向本代理的 provider
        </span>
        <span v-if="system.agentSyncStatus?.claude_last_round" class="muted">
          Claude Code 最近：{{ SYNC_ACTION_TEXT[system.agentSyncStatus.claude_last_round.action] ?? system.agentSyncStatus.claude_last_round.action }}
          （{{ system.agentSyncStatus.claude_last_round.detail }}）
        </span>
        <span v-if="system.agentSyncStatus?.opencode_last_round" class="muted">
          OpenCode 最近：{{ SYNC_ACTION_TEXT[system.agentSyncStatus.opencode_last_round.action] ?? system.agentSyncStatus.opencode_last_round.action }}
          （{{ system.agentSyncStatus.opencode_last_round.detail }}）
        </span>
      </NSpace>
    </NCard>
```

`onMounted` + `usePolling`: replace `system.loadClaudeSyncStatus()` with `system.loadAgentSyncStatus()` (both the mount line ~164 and the poll ~168).

- [ ] **Step 5: Verify + commit**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: clean (pre-existing chunk-size warning only).

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts src/views/Settings.vue
git commit -m "feat(sync): 设置页双目标状态展示与文案（Claude Code / OpenCode）"
```

---

### Task 5: End-to-end smoke (manual; automated portion required)

- [ ] **Step 1: Automated**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent_sync && cargo test --manifest-path src-tauri/Cargo.toml --lib 2>&1 | tail -2 && pnpm exec vue-tsc --noEmit`
Expected: all PASS (re-run once if ONLY `transient_retry_exhausted_trips_and_falls_back` failed — known pre-existing flake).

- [ ] **Step 2: Manual (user)**

`pnpm tauri dev` → 设置页开启开关 → within ≤60s verify `~/.config/opencode/opencode.jsonc`: `pro` limit.context follows the primary route model's real window; `~/.claude/settings.json` continues to sync as before; toggle off freezes both files.

- [ ] **Step 3: Commit fixups if any** (explicit paths only).

---

## Self-Review (done at plan time)

- **Spec coverage:** §3 mechanism (no task — informational) · §5 steps 1-7 → Task 3 (`sync_opencode_round`) + Task 1 (strip) · §6.1 module additions → Tasks 1+3 · §6.2 bookkeeping split → Task 2 (+ oc_notes in Task 3 where consumed) · §6.3 command rename → Task 2 · §6.4 frontend → Task 4 · §7 error table → Task 3 branches + tests (`oc_round_no_port_skips`, `oc_round_toggle_off_and_corrupt_untouched`, missing-file debug path) · §8 testing → each task's test steps · §9 out-of-scope respected (only `limit.context` written — asserted in `oc_round_creates_limit_when_absent`).
- **Placeholder scan:** the Task 3 Step-1 test snippet contains a `fn oc_state` stub with a `await_blocked()` placeholder that is immediately superseded by the `with_port` helper — implementers must DELETE the stub and use `test_state(cfg)` + `with_port(&state, Some(6950))` as the real tests below it do. This instruction is stated here rather than left ambiguous.
- **Type consistency:** `AgentSyncStatus { enabled, claude_path, opencode_path, claude_last_round, opencode_last_round }` consistent across Task 2 (Rust) and Task 4 (TS). `record_oc_notes`/`oc_notes` defined in Task 3 (extension of Task 2's struct). `sync_opencode_once`/`sync_opencode_round` names consistent. `loadAgentSyncStatus`/`agentSyncStatus` consistent between store and view.
