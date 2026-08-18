# Usage Statistics Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record per-provider request counts and input/output token consumption for every successfully-served proxied request, persisted in SQLite, displayed on the existing Usage page.

**Architecture:** A `statistics` module (pure extraction + SQLite writer thread behind an mpsc channel) records events from the proxy dispatch path without blocking it. Window resets (5h/1w/1m) are driven exclusively by usage-API polling (changed `reset_at` = new cycle), never by the recording path. The frontend polls a new `get_usage_statistics` command alongside the existing usage refresh.

**Tech Stack:** Rust (rusqlite bundled, tokio, axum), Vue 3 + Pinia + Naive UI. Spec: `docs/superpowers/specs/2026-08-14-usage-statistics-design.md` (v1.9).

**Spec sections → task map:** extraction (§Token Extraction) → Task 5, 6, 7; UPSERT (§Recording) → Task 1; reset (§Reset At Update Mechanism) → Task 2; actor/writer thread (§Non-Blocking Integration) → Task 3; wiring (§Service Integration) → Task 4, 8; UI (§Frontend Integration) → Task 9, 10.

## Global Constraints

- Statistics are **best-effort**: any failure degrades to a logged drop; the proxy path must never block or error because of statistics (spec §Non-Blocking Integration).
- `record_request` never inspects time; window resets happen only when the usage API reports a changed `reset_at` (spec §Reset Logic).
- Token extraction is a **pure function on already-buffered data**; never consume/await the forwarded response stream (spec §Extraction Implementation).
- Logs identify vendor + upstream model name, never the opaque internal `id` (CLAUDE.md).
- Frontend: pnpm only; `src/lib/types.ts` mirrors Rust serde structs with **snake_case field names** (no rename_all); after every mutating action re-fetch from backend (CLAUDE.md).
- Backend tests are co-located `#[cfg(test)] mod tests`; HTTP behavior via `wiremock`; deterministic time via `FakeClock` (CLAUDE.md).
- New dependency: `rusqlite = { version = "0.31", features = ["bundled"] }` (bundled → no system SQLite requirement on Windows/Linux).
- UI copy is Chinese.
- Run backend tests with `cargo test --manifest-path src-tauri/Cargo.toml`; frontend type-check with `pnpm exec vue-tsc --noEmit`.

## File Structure

- Create `src-tauri/src/statistics/mod.rs` — service: `StatEvent`, writer thread, `UsageStatisticsService`, `ProviderType`, `WindowStats`/`ProviderStats`, `RecordOnce` send-once guard. One responsibility: own the channel + thread, no SQL here.
- Create `src-tauri/src/statistics/db.rs` — schema, `record_request` upsert, `update_reset_times_from_usage`, `query_all`, `delete_row`. One responsibility: all SQL.
- Create `src-tauri/src/statistics/extract.rs` — `extract_tokens_from_json`, `StreamingTokenCollector`, `inject_stream_options`, `SseTap`. One responsibility: pure extraction, no SQL, no I/O.
- Modify `src-tauri/src/proxy/state.rs` — add `statistics: Option<Arc<UsageStatisticsService>>` field + `notify_usage_snapshot` helper (follows the `recorder: Option<Arc<…>>` pattern so test literals stay one-line).
- Modify `src-tauri/src/lib.rs` — module decl, service startup, command registration.
- Modify `src-tauri/src/commands.rs` — `get_usage_statistics` command, hooks in `query_usage` + `delete_provider`.
- Modify `src-tauri/src/proxy/dispatch.rs` — token extraction at the 3 non-stream handlers + stream tap + `send_stream` injection + record call sites.
- Modify `src/lib/types.ts`, `src/lib/commands.ts`, `src/stores/runtime.ts`, `src/views/Usage.vue` — types, bridge, store, display.

---

### Task 1: SQLite schema + atomic record upsert

**Files:**
- Modify: `src-tauri/Cargo.toml` (add rusqlite)
- Create: `src-tauri/src/statistics/mod.rs` (types + module decl only for now)
- Create: `src-tauri/src/statistics/db.rs`
- Register module: `src-tauri/src/lib.rs:9` (add `pub mod statistics;` after `pub mod recording;`)

**Interfaces:**
- Produces (used by Tasks 2, 3):
  - `pub type TokenUsage = (Option<u64>, Option<u64>);` — `(input, output)`
  - `db::open(dir: &Path) -> rusqlite::Result<rusqlite::Connection>`
  - `db::record_request(conn: &Connection, provider_id: &str, vendor: &str, provider_type: &str, tokens: TokenUsage, now: i64) -> rusqlite::Result<()>`

- [ ] **Step 1: Add the dependency**

In `src-tauri/Cargo.toml` `[dependencies]`, after the `fastrand` line:

```toml
rusqlite = { version = "0.31", features = ["bundled"] }
```

- [ ] **Step 2: Create the module skeleton with types**

Create `src-tauri/src/statistics/mod.rs`:

```rust
pub mod db;
pub mod extract;

use serde::Serialize;

/// (input_tokens, output_tokens) extracted from an upstream response.
/// `None` on a side = that half was not reported by the vendor.
pub type TokenUsage = (Option<u64>, Option<u64>);

/// Billing shape of a provider. Derived from the vendor slug at record time
/// (no usage query needed on the hot path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderType {
    /// 智谱/火山 — tiered plan quotas (5h/1w/1m windows).
    Plan,
    /// DeepSeek — pay-as-you-go balance; only lifetime totals are meaningful.
    Consumption,
}

impl ProviderType {
    pub fn from_vendor(vendor: &str) -> Self {
        match vendor {
            "deepseek" => ProviderType::Consumption,
            _ => ProviderType::Plan,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderType::Plan => "plan",
            ProviderType::Consumption => "consumption",
        }
    }
}

/// One time-window scope of statistics. `quality`/`coverage_pct` are derived
/// at read time from `tokenized_requests` vs `requests` (never stored).
#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct WindowStats {
    pub requests: u64,
    pub tokenized_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reset_at: Option<i64>,
    pub is_active: bool,
    pub quality: String,          // "Full" | "RequestsOnly" | "Unknown"
    pub coverage_pct: Option<f64>, // Some(pct) only for partial coverage
}

impl WindowStats {
    /// Pure derivation — no stored quality flag to keep in sync (spec §Data Model).
    pub fn derive_quality(requests: u64, tokenized: u64) -> (String, Option<f64>) {
        match (requests, tokenized) {
            (0, _) => ("Unknown".into(), None),
            (_, 0) => ("RequestsOnly".into(), None),
            (r, t) if t == r => ("Full".into(), None),
            (r, t) => ("Full".into(), Some(t as f64 / r as f64 * 100.0)),
        }
    }
}

/// All statistics for one provider, as returned to the frontend.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ProviderStats {
    pub provider_id: String,
    pub vendor: String,
    pub provider_type: String,
    pub last_5h: WindowStats,
    pub last_1w: WindowStats,
    pub last_1m: WindowStats,
    pub total_requests: u64,
    pub total_tokenized_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
}
```

Create a placeholder `src-tauri/src/statistics/extract.rs` so the module compiles (filled in Task 5):

```rust
use crate::proxy::dispatch::ClientProtocol;
use serde_json::Value;

use super::TokenUsage;

/// Extract (input, output) token counts from an already-parsed upstream
/// response body. Pure; missing fields degrade to (None, None).
/// Non-standard vendors returning only `total_tokens` get (Some(total), Some(0))
/// so the displayed total stays exact (spec §Extraction Implementation).
pub fn extract_tokens_from_json(_body: &Value, _upstream_protocol: ClientProtocol) -> TokenUsage {
    (None, None)
}
```

Add to `src-tauri/src/lib.rs` after `pub mod recording;`:

```rust
pub mod statistics;
```

- [ ] **Step 3: Write the failing tests for db**

Create `src-tauri/src/statistics/db.rs` (tests first; the implementation body is added in Step 5 — for Step 4 to compile, write the file with just the test module and empty stubs):

```rust
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use super::{ProviderStats, ProviderType, WindowStats, TokenUsage};

pub(crate) fn open(_dir: &Path) -> rusqlite::Result<Connection> {
    unimplemented!()
}

pub(crate) fn record_request(
    _conn: &Connection,
    _provider_id: &str,
    _vendor: &str,
    _provider_type: &str,
    _tokens: TokenUsage,
    _now: i64,
) -> rusqlite::Result<()> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn first_record_creates_row() {
        // Brand-new provider: the upsert INSERT branch must create the row and
        // count the request in totals (spec §Recording — a bare UPDATE would
        // silently lose it).
        let conn = mem();
        record_request(&conn, "p1", "zhipu", "plan", (Some(100), Some(200)), 1000).unwrap();
        let s = query_one(&conn, "p1").unwrap();
        assert_eq!(s.total_requests, 1);
        assert_eq!(s.total_input_tokens, 100);
        assert_eq!(s.total_output_tokens, 200);
        assert_eq!(s.total_tokens, 300);
        assert_eq!(s.total_tokenized_requests, 1);
        // Fresh row: windows not armed (reset_at NULL) → window counters stay 0.
        assert_eq!(s.last_5h.requests, 0);
        assert_eq!(s.last_5h.input_tokens, 0);
        assert_eq!(s.last_5h.tokenized_requests, 0);
    }

    #[test]
    fn untokenized_record_still_counts_requests() {
        // Fallback strategy: token data missing → request counter still moves,
        // tokenized counter does not (quality later derives to RequestsOnly).
        let conn = mem();
        record_request(&conn, "p1", "zhipu", "plan", (None, None), 1000).unwrap();
        let s = query_one(&conn, "p1").unwrap();
        assert_eq!(s.total_requests, 1);
        assert_eq!(s.total_tokenized_requests, 0);
        assert_eq!(s.total_tokens, 0);
    }

    #[test]
    fn armed_window_accumulates() {
        let conn = mem();
        record_request(&conn, "p1", "zhipu", "plan", (Some(10), Some(20)), 1000).unwrap();
        arm_windows(&conn, "p1", Some(2000), Some(3000), Some(4000)).unwrap();
        record_request(&conn, "p1", "zhipu", "plan", (Some(1), Some(2)), 1500).unwrap();
        let s = query_one(&conn, "p1").unwrap();
        // First request pre-arm is NOT in the window; the armed one is.
        assert_eq!(s.last_5h.requests, 1);
        assert_eq!(s.last_5h.input_tokens, 1);
        assert_eq!(s.last_5h.output_tokens, 2);
        assert_eq!(s.last_5h.tokenized_requests, 1);
        assert_eq!(s.last_1w.requests, 1);
        assert_eq!(s.total_requests, 2); // totals see both
    }

    #[test]
    fn unarmed_window_guards_all_columns() {
        // Symmetry guard (spec v1.8 #3): a window with NULL reset_at must skip
        // requests, tokenized AND token columns — no requests=0/tokens=1000.
        let conn = mem();
        record_request(&conn, "p1", "zhipu", "plan", (Some(10), Some(20)), 1000).unwrap();
        arm_windows(&conn, "p1", Some(2000), None, Some(4000)).unwrap(); // 1w never armed
        record_request(&conn, "p1", "zhipu", "plan", (Some(1), Some(2)), 1500).unwrap();
        let s = query_one(&conn, "p1").unwrap();
        assert_eq!(s.last_1w.requests, 0);
        assert_eq!(s.last_1w.tokenized_requests, 0);
        assert_eq!(s.last_1w.input_tokens, 0);
        assert_eq!(s.last_1w.output_tokens, 0);
    }

    #[test]
    fn vendor_and_type_refresh_on_update() {
        // Provider edited in config: DO UPDATE refreshes vendor/provider_type.
        let conn = mem();
        record_request(&conn, "p1", "zhipu", "plan", (None, None), 1000).unwrap();
        record_request(&conn, "p1", "deepseek", "consumption", (None, None), 1001).unwrap();
        let s = query_one(&conn, "p1").unwrap();
        assert_eq!(s.vendor, "deepseek");
        assert_eq!(s.provider_type, "consumption");
    }

    // ---- helpers used by these tests (and Task 2's) ----

    pub(crate) fn arm_windows(
        conn: &Connection,
        provider_id: &str,
        h5: Option<i64>,
        w1: Option<i64>,
        m1: Option<i64>,
    ) -> rusqlite::Result<()> {
        if let Some(r) = h5 {
            conn.execute("UPDATE usage_statistics SET last_5h_reset_at = ?1 WHERE provider_id = ?2", params![r, provider_id])?;
        }
        if let Some(r) = w1 {
            conn.execute("UPDATE usage_statistics SET last_1w_reset_at = ?1 WHERE provider_id = ?2", params![r, provider_id])?;
        }
        if let Some(r) = m1 {
            conn.execute("UPDATE usage_statistics SET last_1m_reset_at = ?1 WHERE provider_id = ?2", params![r, provider_id])?;
        }
        Ok(())
    }

    pub(crate) fn query_one(conn: &Connection, provider_id: &str) -> rusqlite::Result<ProviderStats> {
        let mut all = query_all(conn)?;
        all.drain(..).find(|s| s.provider_id == provider_id)
            .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)
    }
}
```

(For Step 3, `create_schema` and `query_all` referenced by tests don't exist yet — that's the failure.)

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::db`
Expected: compile error (`unimplemented!` / missing `create_schema`) — tests fail to build.

- [ ] **Step 5: Implement the schema, upsert, and query**

Replace the stub bodies in `src-tauri/src/statistics/db.rs` (above the test module) with:

```rust
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use super::{ProviderStats, WindowStats, TokenUsage};

/// Open (creating if needed) `usage_statistics.db` in `dir`. WAL + busy_timeout
/// per spec §Performance. Bundled SQLite → no system dependency.
pub(crate) fn open(dir: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(dir.join("usage_statistics.db"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    create_schema(&conn)?;
    Ok(conn)
}

pub(crate) fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    // No FK to `providers` (config lives in app_config.json, not this file);
    // provider deletion is handled via StatEvent::DeleteProvider (spec §Database Schema).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS usage_statistics (
            provider_id TEXT PRIMARY KEY,
            last_5h_requests INTEGER DEFAULT 0,
            last_5h_tokenized_requests INTEGER DEFAULT 0,
            last_5h_input_tokens INTEGER DEFAULT 0,
            last_5h_output_tokens INTEGER DEFAULT 0,
            last_5h_reset_at INTEGER,
            last_1w_requests INTEGER DEFAULT 0,
            last_1w_tokenized_requests INTEGER DEFAULT 0,
            last_1w_input_tokens INTEGER DEFAULT 0,
            last_1w_output_tokens INTEGER DEFAULT 0,
            last_1w_reset_at INTEGER,
            last_1m_requests INTEGER DEFAULT 0,
            last_1m_tokenized_requests INTEGER DEFAULT 0,
            last_1m_input_tokens INTEGER DEFAULT 0,
            last_1m_output_tokens INTEGER DEFAULT 0,
            last_1m_reset_at INTEGER,
            total_requests INTEGER DEFAULT 0,
            total_tokenized_requests INTEGER DEFAULT 0,
            total_input_tokens INTEGER DEFAULT 0,
            total_output_tokens INTEGER DEFAULT 0,
            total_tokens INTEGER DEFAULT 0,
            vendor TEXT NOT NULL,
            provider_type TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_usage_vendor ON usage_statistics(vendor)", [])?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_usage_updated ON usage_statistics(updated_at)", [])?;
    Ok(())
}

/// Pure accumulation — one atomic INSERT ... ON CONFLICT upsert (spec §Recording).
/// Never inspects time, never resets anything. The INSERT branch writes 0 for
/// every window counter (a fresh row has NULL reset_at ⇒ windows not armed);
/// the DO UPDATE branch guards each window column on its own reset_at.
pub(crate) fn record_request(
    conn: &Connection,
    provider_id: &str,
    vendor: &str,
    provider_type: &str,
    tokens: TokenUsage,
    now: i64,
) -> rusqlite::Result<()> {
    let (input, output) = (tokens.0.unwrap_or(0), tokens.1.unwrap_or(0));
    let has_token_data = tokens.0.is_some() || tokens.1.is_some();
    conn.execute(
        "INSERT INTO usage_statistics (
            provider_id, vendor, provider_type,
            last_5h_requests, last_5h_tokenized_requests, last_5h_input_tokens, last_5h_output_tokens,
            last_1w_requests, last_1w_tokenized_requests, last_1w_input_tokens, last_1w_output_tokens,
            last_1m_requests, last_1m_tokenized_requests, last_1m_input_tokens, last_1m_output_tokens,
            total_requests, total_tokenized_requests, total_input_tokens, total_output_tokens, total_tokens,
            updated_at
        ) VALUES (
            ?1, ?2, ?3,
            0, 0, 0, 0,
            0, 0, 0, 0,
            0, 0, 0, 0,
            1, ?4, ?5, ?6, ?5 + ?6,
            ?7
        )
        ON CONFLICT(provider_id) DO UPDATE SET
            vendor = ?2,
            provider_type = ?3,
            last_5h_requests      = last_5h_requests + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL THEN 1 ELSE 0 END,
            last_5h_tokenized_requests = last_5h_tokenized_requests + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL AND ?4 THEN 1 ELSE 0 END,
            last_5h_input_tokens  = last_5h_input_tokens + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL THEN ?5 ELSE 0 END,
            last_5h_output_tokens = last_5h_output_tokens + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL THEN ?6 ELSE 0 END,
            last_1w_requests      = last_1w_requests + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL THEN 1 ELSE 0 END,
            last_1w_tokenized_requests = last_1w_tokenized_requests + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL AND ?4 THEN 1 ELSE 0 END,
            last_1w_input_tokens  = last_1w_input_tokens + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL THEN ?5 ELSE 0 END,
            last_1w_output_tokens = last_1w_output_tokens + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL THEN ?6 ELSE 0 END,
            last_1m_requests      = last_1m_requests + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL THEN 1 ELSE 0 END,
            last_1m_tokenized_requests = last_1m_tokenized_requests + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL AND ?4 THEN 1 ELSE 0 END,
            last_1m_input_tokens  = last_1m_input_tokens + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL THEN ?5 ELSE 0 END,
            last_1m_output_tokens = last_1m_output_tokens + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL THEN ?6 ELSE 0 END,
            total_requests      = total_requests + 1,
            total_tokenized_requests = total_tokenized_requests + ?4,
            total_input_tokens  = total_input_tokens + ?5,
            total_output_tokens = total_output_tokens + ?6,
            total_tokens        = total_tokens + ?5 + ?6,
            updated_at          = ?7
        ",
        params![provider_id, vendor, provider_type, has_token_data, input, output, now],
    )?;
    Ok(())
}

/// Read all rows, deriving quality/coverage/is_active per window at read time.
pub(crate) fn query_all(conn: &Connection) -> rusqlite::Result<Vec<ProviderStats>> {
    let mut stmt = conn.prepare(
        "SELECT provider_id, vendor, provider_type,
            last_5h_requests, last_5h_tokenized_requests, last_5h_input_tokens, last_5h_output_tokens, last_5h_reset_at,
            last_1w_requests, last_1w_tokenized_requests, last_1w_input_tokens, last_1w_output_tokens, last_1w_reset_at,
            last_1m_requests, last_1m_tokenized_requests, last_1m_input_tokens, last_1m_output_tokens, last_1m_reset_at,
            total_requests, total_tokenized_requests, total_input_tokens, total_output_tokens, total_tokens
         FROM usage_statistics ORDER BY provider_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let w = |off: usize| -> rusqlite::Result<WindowStats> {
            let requests: u64 = row.get(off)?;
            let tokenized: u64 = row.get(off + 1)?;
            let (quality, coverage_pct) = WindowStats::derive_quality(requests, tokenized);
            Ok(WindowStats {
                requests,
                tokenized_requests: tokenized,
                input_tokens: row.get(off + 2)?,
                output_tokens: row.get(off + 3)?,
                reset_at: row.get(off + 4)?,
                is_active: row.get::<_, Option<i64>>(off + 4)?.is_some(),
                quality,
                coverage_pct,
            })
        };
        Ok(ProviderStats {
            provider_id: row.get(0)?,
            vendor: row.get(1)?,
            provider_type: row.get(2)?,
            last_5h: w(3)?,
            last_1w: w(8)?,
            last_1m: w(13)?,
            total_requests: row.get(18)?,
            total_tokenized_requests: row.get(19)?,
            total_input_tokens: row.get(20)?,
            total_output_tokens: row.get(21)?,
            total_tokens: row.get(22)?,
        })
    })?;
    rows.collect()
}

/// Drop a provider's row (StatEvent::DeleteProvider handler).
pub(crate) fn delete_row(conn: &Connection, provider_id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM usage_statistics WHERE provider_id = ?1", params![provider_id])?;
    Ok(())
}
```

Note: the test helpers `arm_windows` / `query_one` stay inside `#[cfg(test)] mod tests` (declared `pub(crate)` there so Task 2's tests can use them via `use super::tests::{arm_windows, query_one};` — if the compiler complains about unused visibility, drop the `pub(crate)`).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::db`
Expected: 5 tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/statistics/ src-tauri/src/lib.rs
git commit -m "feat(statistics): SQLite schema + atomic record upsert"
```

---

### Task 2: Reset-from-usage mechanism

**Files:**
- Modify: `src-tauri/src/statistics/db.rs` (add `update_reset_times_from_usage` + tests)

**Interfaces:**
- Consumes: `crate::usage::{UsageSnapshot, UsageTier}` (existing, `usage/mod.rs:23`, `usage/mod.rs:83`); `WindowStats` from Task 1.
- Produces: `db::update_reset_times_from_usage(conn: &Connection, provider_id: &str, snapshot: &UsageSnapshot) -> rusqlite::Result<()>` (used by Task 3's writer thread).

- [ ] **Step 1: Write the failing tests**

Append to `#[cfg(test)] mod tests` in `src-tauri/src/statistics/db.rs`:

```rust
use crate::usage::{UsageSnapshot, UsageTier};

fn snap(tiers: Vec<(&str, Option<i64>)>) -> UsageSnapshot {
    UsageSnapshot {
        total: None, remaining: None, reset_at: None, unit: "%".into(),
        raw_summary: None, plan: None, billing_model: "plan".into(), plan_info: None,
        tiers: tiers.into_iter()
            .map(|(window, reset_at)| UsageTier { window: window.into(), used_pct: Some(50.0), reset_at })
            .collect(),
    }
}

#[test]
fn changed_reset_at_zeroes_window_counters() {
    let conn = mem();
    record_request(&conn, "p1", "zhipu", "plan", (Some(10), Some(20)), 1000).unwrap();
    arm_windows(&conn, "p1", Some(2000), Some(3000), Some(4000)).unwrap();
    record_request(&conn, "p1", "zhipu", "plan", (Some(1), Some(2)), 1500).unwrap();
    // 5h rolled over: API now reports 20000 instead of 2000.
    update_reset_times_from_usage(&conn, "p1", &snap(vec![("five_hour", Some(20_000))])).unwrap();
    let s = query_one(&conn, "p1").unwrap();
    assert_eq!(s.last_5h.requests, 0);       // zeroed
    assert_eq!(s.last_5h.input_tokens, 0);
    assert_eq!(s.last_5h.reset_at, Some(20_000));
    assert_eq!(s.last_1w.requests, 1);       // untouched window keeps data
    assert_eq!(s.total_requests, 2);         // lifetime never resets
}

#[test]
fn unchanged_reset_at_is_a_noop() {
    let conn = mem();
    record_request(&conn, "p1", "zhipu", "plan", (Some(1), Some(2)), 1500).unwrap();
    arm_windows(&conn, "p1", Some(2000), None, None).unwrap();
    record_request(&conn, "p1", "zhipu", "plan", (Some(3), Some(4)), 1600).unwrap();
    update_reset_times_from_usage(&conn, "p1", &snap(vec![("five_hour", Some(2000))])).unwrap();
    let s = query_one(&conn, "p1").unwrap();
    assert_eq!(s.last_5h.requests, 2); // same cycle → nothing zeroed
}

#[test]
fn missing_row_does_not_error() {
    // New provider: usage poll fires before the first request creates a row.
    // query_row would return Err(QueryReturnedNoRows) → .optional() reads None.
    let conn = mem();
    update_reset_times_from_usage(&conn, "ghost", &snap(vec![("five_hour", Some(2000))])).unwrap();
    // Row now exists with the window armed (INSERT-or-UPDATE handles both).
    let s = query_one(&conn, "ghost").unwrap();
    assert_eq!(s.last_5h.reset_at, Some(2000));
    assert_eq!(s.last_5h.requests, 0);
    assert_eq!(s.last_5h.is_active, true);
}

#[test]
fn unknown_window_names_skipped() {
    let conn = mem();
    record_request(&conn, "p1", "zhipu", "plan", (Some(1), Some(2)), 1500).unwrap();
    update_reset_times_from_usage(&conn, "p1", &snap(vec![("quarterly", Some(9_999))])).unwrap();
    let s = query_one(&conn, "p1").unwrap();
    assert_eq!(s.last_5h.reset_at, None); // nothing armed by an unknown window
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::db`
Expected: FAIL — `update_reset_times_from_usage` not defined.

- [ ] **Step 3: Implement**

Add to `src-tauri/src/statistics/db.rs` (implementation section, above the tests):

```rust
use crate::usage::UsageSnapshot;

/// Window-reset authority (spec §Reset At Update Mechanism): a *changed*
/// `reset_at` from the usage API signals a new plan cycle → update the
/// boundary and zero that window's counters, in one transaction. A row that
/// doesn't exist yet (new provider, poll before first request) is created
/// with the reset time armed — `.optional()` avoids QueryReturnedNoRows.
pub(crate) fn update_reset_times_from_usage(
    conn: &Connection,
    provider_id: &str,
    snapshot: &UsageSnapshot,
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for tier in &snapshot.tiers {
        let (reset_col, req_col, tok_col, in_col, out_col) = match tier.window.as_str() {
            "five_hour" => ("last_5h_reset_at", "last_5h_requests", "last_5h_tokenized_requests", "last_5h_input_tokens", "last_5h_output_tokens"),
            "weekly_limit" => ("last_1w_reset_at", "last_1w_requests", "last_1w_tokenized_requests", "last_1w_input_tokens", "last_1w_output_tokens"),
            "monthly" => ("last_1m_reset_at", "last_1m_requests", "last_1m_tokenized_requests", "last_1m_input_tokens", "last_1m_output_tokens"),
            _ => continue,
        };
        let Some(new_reset) = tier.reset_at else { continue };
        let old_reset: Option<i64> = tx
            .query_row(
                &format!("SELECT {reset_col} FROM usage_statistics WHERE provider_id = ?1"),
                params![provider_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if old_reset == Some(new_reset) {
            continue; // same cycle → no reset
        }
        let row_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_statistics WHERE provider_id = ?1)",
                params![provider_id],
                |row| row.get(0),
            )?;
        if !row_exists {
            // First observation of a brand-new provider: create the armed row.
            tx.execute(
                "INSERT INTO usage_statistics (provider_id, vendor, provider_type, updated_at, \
                 last_5h_reset_at, last_1w_reset_at, last_1m_reset_at) \
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL)",
                params![provider_id, "", "plan", 0], // vendor filled by the next record_request upsert
            )?;
        }
        // New cycle (or first arming): write the boundary and zero the window.
        tx.execute(
            &format!(
                "UPDATE usage_statistics SET {reset_col} = ?1, {req_col} = 0, {tok_col} = 0, {in_col} = 0, {out_col} = 0 \
                 WHERE provider_id = ?2"
            ),
            params![new_reset, provider_id],
        )?;
    }
    tx.commit()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::db`
Expected: 9 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/statistics/db.rs
git commit -m "feat(statistics): reset windows on changed reset_at from usage API"
```

---

### Task 3: Service actor — channel, writer thread, async API

**Files:**
- Modify: `src-tauri/src/statistics/mod.rs` (add service; keep existing types)

**Interfaces:**
- Consumes: `db::*` (Tasks 1–2), `crate::proxy::health::Clock` (existing, `proxy/health.rs`), `crate::usage::UsageSnapshot` (existing).
- Produces (used by Tasks 4–8):
  - `pub struct UsageStatisticsService` with:
    - `pub fn start(dir: &Path, clock: std::sync::Arc<dyn Clock>) -> std::io::Result<std::sync::Arc<Self>>`
    - `pub fn record(&self, provider_id: &str, vendor: &str, provider_type: ProviderType, tokens: TokenUsage)` — non-blocking send
    - `pub fn notify_usage_snapshot(&self, provider_id: &str, snapshot: &UsageSnapshot)` — non-blocking send
    - `pub fn delete_provider(&self, provider_id: &str)` — non-blocking send
    - `pub async fn get_all_stats(&self) -> Result<Vec<ProviderStats>, String>` — oneshot round-trip
  - `pub struct RecordOnce` — send-once guard for streaming paths.

- [ ] **Step 1: Write the failing tests**

Append to `src-tauri/src/statistics/mod.rs`:

```rust
#[cfg(test)]
mod service_tests {
    use super::*;
    use crate::proxy::health::FakeClock;
    use crate::usage::{UsageSnapshot, UsageTier};
    use std::sync::Arc;
    use std::time::Duration;

    fn snap_5h(reset_at: i64) -> UsageSnapshot {
        UsageSnapshot {
            total: None, remaining: None, reset_at: None, unit: "%".into(),
            raw_summary: None, plan: None, billing_model: "plan".into(), plan_info: None,
            tiers: vec![UsageTier { window: "five_hour".into(), used_pct: Some(50.0), reset_at: Some(reset_at) }],
        }
    }

    /// Drain the channel-backed DB until a predicate holds (bounded wait).
    async fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        for _ in 0..200 {
            if let Some(v) = f() { return v; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met within 2s");
    }

    #[tokio::test]
    async fn record_creates_and_accumulates() {
        let dir = tempfile::tempdir().unwrap();
        let svc = UsageStatisticsService::start(dir.path(), Arc::new(FakeClock::new(1000))).unwrap();
        svc.record("p1", "zhipu", ProviderType::Plan, (Some(10), Some(20)));
        svc.record("p1", "zhipu", ProviderType::Plan, (None, None));
        // try_stats returns only the last Query result — drive reads via
        // get_all_stats (a Query round-trip) so polling actually observes writes.
        let stats = wait_for(|| poll_stats(&svc, "p1", |s| s.total_requests == 2)).await;
        assert_eq!(stats.total_tokenized_requests, 1);
        assert_eq!(stats.total_tokens, 30);
    }

    /// Sync snapshot helper for `wait_for` (block_on is fine: the send is
    /// synchronous; only the reply is async — use a small current-thread
    /// runtime per call via tokio::task::block_in_place-free polling instead:
    /// simplest correct form is a plain futures::executor::block_on).
    fn poll_stats(
        svc: &std::sync::Arc<UsageStatisticsService>,
        provider_id: &str,
        pred: impl Fn(&ProviderStats) -> bool,
    ) -> Option<ProviderStats> {
        let all = futures::executor::block_on(svc.get_all_stats()).ok()?;
        all.into_iter().find(|s| s.provider_id == provider_id && pred(s))
    }

    #[tokio::test]
    async fn usage_snapshot_arms_and_resets_window() {
        let dir = tempfile::tempdir().unwrap();
        let svc = UsageStatisticsService::start(dir.path(), Arc::new(FakeClock::new(1000))).unwrap();
        svc.notify_usage_snapshot("p1", &snap_5h(2000));
        svc.record("p1", "zhipu", ProviderType::Plan, (Some(5), Some(5)));
        let stats = wait_for(|| poll_stats(&svc, "p1", |s| s.last_5h.requests == 1)).await;
        assert_eq!(stats.last_5h.reset_at, Some(2000));
        // New cycle → zeroed.
        svc.notify_usage_snapshot("p1", &snap_5h(7000));
        let stats = wait_for(|| poll_stats(&svc, "p1", |s| s.last_5h.reset_at == Some(7000))).await;
        assert_eq!(stats.last_5h.requests, 0);
        assert_eq!(stats.total_requests, 1); // lifetime intact
    }

    #[tokio::test]
    async fn delete_provider_drops_row() {
        let dir = tempfile::tempdir().unwrap();
        let svc = UsageStatisticsService::start(dir.path(), Arc::new(FakeClock::new(1000))).unwrap();
        svc.record("p1", "zhipu", ProviderType::Plan, (Some(1), Some(1)));
        wait_for(|| poll_stats(&svc, "p1", |_| true)).await;
        svc.delete_provider("p1");
        wait_for(|| {
            let all = futures::executor::block_on(svc.get_all_stats()).ok()?;
            all.is_empty().then_some(all)
        }).await;
    }

    #[test]
    fn record_once_sends_exactly_once() {
        let (tx, rx) = std::sync::mpsc::channel::<StatEvent>();
        let mut guard = RecordOnce::new(tx, StatEvent::DeleteProvider { provider_id: "p".into() });
        guard.send();                              // explicit send
        drop(guard);                               // drop must NOT double-send
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_ok());
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::`
Expected: FAIL — `UsageStatisticsService`, `StatEvent`, `RecordOnce`, `try_stats` not defined.

- [ ] **Step 3: Implement the service**

Add to `src-tauri/src/statistics/mod.rs` (implementation section):

```rust
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::proxy::health::Clock;
use crate::usage::UsageSnapshot;

/// Messages to the dedicated writer thread. All DB access funnels through
/// this single owner — no write-write contention, no SQLITE_BUSY between
/// statistics writers (spec §Non-Blocking Integration).
pub enum StatEvent {
    Record {
        provider_id: String,
        vendor: String,
        provider_type: ProviderType,
        tokens: TokenUsage,
    },
    ResetFromUsage {
        provider_id: String,
        snapshot: UsageSnapshot,
    },
    /// Read path: the writer thread runs the SELECT and replies on the oneshot.
    Query {
        reply: tokio::sync::oneshot::Sender<Vec<ProviderStats>>,
    },
    DeleteProvider {
        provider_id: String,
    },
}

/// Handle to the statistics writer thread. Every public method is a
/// non-blocking channel send (unbounded mpsc) — the async hot path never
/// touches SQLite (spec §Non-Blocking Integration).
pub struct UsageStatisticsService {
    tx: std::sync::mpsc::Sender<StatEvent>,
    /// Last query result — lets tests (and a degraded writer) still read.
    last_query: Mutex<Option<Vec<ProviderStats>>>,
}

impl UsageStatisticsService {
    /// Open the DB and spawn the writer thread (a plain std::thread, NOT a
    /// tokio task — rusqlite is synchronous and must not block the runtime).
    pub fn start(dir: &Path, clock: Arc<dyn Clock>) -> std::io::Result<Arc<Self>> {
        let conn = db::open(dir).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let (tx, rx) = std::sync::mpsc::channel::<StatEvent>();
        let svc = Arc::new(Self { tx, last_query: Mutex::new(None) });
        let weak = Arc::downgrade(&svc);
        std::thread::Builder::new()
            .name("switchlm-statistics".into())
            .spawn(move || writer_loop(rx, conn, clock, weak))?;
        Ok(svc)
    }

    /// Fire-and-forget: record one successfully-served request. Token halves
    /// may be None (fallback: request counted, tokens not) — spec §Fallback.
    pub fn record(&self, provider_id: &str, vendor: &str, provider_type: ProviderType, tokens: TokenUsage) {
        self.send(StatEvent::Record {
            provider_id: provider_id.into(),
            vendor: vendor.into(),
            provider_type,
            tokens,
        });
    }

    /// Usage polling hook — the ONLY path that resets windows (spec §Reset).
    pub fn notify_usage_snapshot(&self, provider_id: &str, snapshot: &UsageSnapshot) {
        if snapshot.tiers.is_empty() { return; }
        self.send(StatEvent::ResetFromUsage { provider_id: provider_id.into(), snapshot: snapshot.clone() });
    }

    /// Provider removed from config → drop its statistics row.
    pub fn delete_provider(&self, provider_id: &str) {
        self.send(StatEvent::DeleteProvider { provider_id: provider_id.into() });
    }

    /// Async read used by the Tauri command: round-trip through the writer
    /// thread (single connection owner). Empty when the writer is gone.
    pub async fn get_all_stats(&self) -> Result<Vec<ProviderStats>, String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if self.tx.send(StatEvent::Query { reply: reply_tx }).is_err() {
            return Ok(Vec::new()); // writer gone → degrade, don't error the UI
        }
        reply_rx.await.map_err(|e| e.to_string())
    }

    /// Synchronous best-effort read of the last query result (tests / degraded mode).
    pub fn try_stats(&self) -> Result<Vec<ProviderStats>, String> {
        self.last_query.lock().unwrap().clone().ok_or_else(|| "no stats yet".to_string())
    }

    fn send(&self, evt: StatEvent) {
        if let Err(e) = self.tx.send(evt) {
            // Writer thread gone (shutdown/crash) — best-effort, drop with a warn.
            tracing::warn!(target: "switchlm::statistics", "writer unavailable, dropping event: {e}");
        }
    }
}

fn writer_loop(
    rx: std::sync::mpsc::Receiver<StatEvent>,
    mut conn: rusqlite::Connection,
    clock: Arc<dyn Clock>,
    svc: std::sync::Weak<UsageStatisticsService>,
) {
    while let Ok(event) = rx.recv() {
        // Drain-and-batch: take everything already queued, apply in ONE pass.
        let mut batch = vec![event];
        while let Ok(ev) = rx.try_recv() { batch.push(ev); }
        for evt in batch {
            let now = clock.now_secs();
            if let Err(e) = apply(&mut conn, evt, now, &svc) {
                tracing::warn!(target: "switchlm::statistics", "statistics write failed: {e}"); // best-effort drop
            }
        }
    }
}

fn apply(
    conn: &mut rusqlite::Connection,
    evt: StatEvent,
    now: i64,
    svc: &std::sync::Weak<UsageStatisticsService>,
) -> rusqlite::Result<()> {
    match evt {
        StatEvent::Record { provider_id, vendor, provider_type, tokens } => {
            db::record_request(conn, &provider_id, &vendor, provider_type.as_str(), tokens, now)
        }
        StatEvent::ResetFromUsage { provider_id, snapshot } => {
            db::update_reset_times_from_usage(conn, &provider_id, &snapshot)
        }
        StatEvent::Query { reply } => {
            let stats = db::query_all(conn)?;
            if let Some(svc) = svc.upgrade() {
                *svc.last_query.lock().unwrap() = Some(stats.clone());
            }
            let _ = reply.send(stats); // receiver gone → drop, not an error
            Ok(())
        }
        StatEvent::DeleteProvider { provider_id } => db::delete_row(conn, &provider_id),
    }
}

/// Send-once guard for streaming paths: the event fires exactly once — either
/// explicitly (stream completed with usage) or on drop (stream aborted →
/// still records the request, tokens None). Spec §Fallback.
pub struct RecordOnce {
    tx: std::sync::mpsc::Sender<StatEvent>,
    evt: Option<StatEvent>,
}

impl RecordOnce {
    pub fn new(tx: std::sync::mpsc::Sender<StatEvent>, evt: StatEvent) -> Self {
        Self { tx, evt: Some(evt) }
    }
    /// Consume and send the event now (no-op after the first call).
    pub fn send(&mut self) {
        if let Some(evt) = self.evt.take() {
            if let Err(e) = self.tx.send(evt) {
                tracing::warn!(target: "switchlm::statistics", "writer unavailable, dropping stream record: {e}");
            }
        }
    }
}

impl Drop for RecordOnce {
    fn drop(&mut self) {
        self.send();
    }
}
```

The `Query` variant also writes `last_query` through a `Weak` back-reference so `try_stats()` works — but tests must drive reads through `get_all_stats()` (a real Query round-trip), since `try_stats()` only returns the *latest* query result.

**`futures::executor::block_on` note:** the test helper runs inside `#[tokio::test]`; calling `block_on` on a futures executor inside a tokio runtime is fine here because the future completes via a std mpsc + oneshot without ever yielding to the tokio reactor. If the compiler/runtime complains, convert `wait_for`'s closure to an async fn and `.await` `get_all_stats()` directly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::`
Expected: 9 tests PASS (5 db + 4 service). Note: `try_stats` returns the *last* query result, so the `wait_for` loops above poll until a `Query` event has been processed — the test helper name reflects that.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/statistics/mod.rs
git commit -m "feat(statistics): writer-thread actor service with non-blocking API"
```

---

### Task 4: AppState wiring + startup

**Files:**
- Modify: `src-tauri/src/proxy/state.rs:46` (add field), `state.rs` `load()` (~line 70)
- Modify: `src-tauri/src/lib.rs:161-174` (AppStateInner literal in setup)
- Modify: every test `AppStateInner` literal (dispatch.rs ×7, plus any others found by grep)

**Interfaces:**
- Consumes: `UsageStatisticsService` (Task 3).
- Produces (used by Tasks 6–8): `AppStateInner.statistics: Option<Arc<crate::statistics::UsageStatisticsService>>` and helper `AppStateInner::notify_usage_snapshot(&self, provider_id: &str, snapshot: &UsageSnapshot)`.

- [ ] **Step 1: Add the field and helper**

In `src-tauri/src/proxy/state.rs`, add to the `AppStateInner` struct after `recorder`:

```rust
    /// 用量统计服务(`None` = 启动时 DB 打开失败或测试环境,统计禁用,零开销)。
    /// 仿 `recorder` 的 Option 模式:dispatch 各路径判 None 直接跳过。
    pub statistics: Option<std::sync::Arc<crate::statistics::UsageStatisticsService>>,
```

Add to `impl AppStateInner`:

```rust
    /// Usage 查询的统一旁路钩子(spec §Reset):任何来源(前端轮询/托盘/断路器
    /// 实时查询)拿到新快照后都调用本方法,由统计服务判定窗口重置。
    pub fn notify_usage_snapshot(&self, provider_id: &str, snapshot: &crate::usage::UsageSnapshot) {
        if let Some(s) = &self.statistics {
            s.notify_usage_snapshot(provider_id, snapshot);
        }
    }
```

In `load()` (~line 70), add before `Ok(Self {`:

```rust
        // 统计 DB 打开失败 → None(功能禁用,绝不阻断启动)。
        let statistics = crate::statistics::UsageStatisticsService::start(dir, self_clock_arc())
            .map_err(|e| {
                tracing::warn!("statistics DB unavailable, usage stats disabled: {e}");
                e
            })
            .ok();
```

with the literal gaining `statistics,` — and `load()` needs the clock; simplest is to construct with `Arc::new(SystemClock)` directly:

```rust
        let statistics = crate::statistics::UsageStatisticsService::start(
            dir,
            Arc::new(SystemClock),
        )
        .ok()
        .inspect_err(|e| tracing::warn!("statistics DB unavailable, usage stats disabled: {e}"));
```

(`inspect_err` is stable since Rust 1.76 — fine for the project's stable toolchain; if the MSRV complains, use `map_err` + `ok()` as above.)

- [ ] **Step 2: Fix the production literal in lib.rs**

In `src-tauri/src/lib.rs` (~line 161), the `AppStateInner { ... }` literal gains:

```rust
                statistics: crate::statistics::UsageStatisticsService::start(&dir, Arc::new(proxy::SystemClock))
                    .inspect_err(|e| tracing::warn!("statistics DB unavailable, stats disabled: {e}"))
                    .ok(),
```

(`dir` is already in scope in `setup`; `proxy::SystemClock` is re-exported — check `src-tauri/src/proxy/mod.rs:13`.)

- [ ] **Step 3: Fix every test literal**

Run: `grep -rn "recorder: std::sync::RwLock::new(None)" src-tauri/src`
For **each** hit (expected: `src-tauri/src/proxy/dispatch.rs` ×7 in tests, possibly others), add one line after it in the same literal:

```rust
            statistics: None,
```

- [ ] **Step 4: Build and run the full backend suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: compile succeeds; ALL pre-existing tests PASS (no behavior change — statistics is inert until dispatch integration).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/state.rs src-tauri/src/lib.rs src-tauri/src/proxy/dispatch.rs
git commit -m "feat(statistics): wire service into AppState (optional, inert until dispatch hooks)"
```

---

### Task 5: Pure extraction — JSON + streaming collector + stream_options injection

**Files:**
- Modify: `src-tauri/src/statistics/extract.rs` (replace placeholder)

**Interfaces:**
- Consumes: `crate::proxy::dispatch::ClientProtocol` (existing, `dispatch.rs:26-30`).
- Produces (used by Tasks 6–7):
  - `pub fn extract_tokens_from_json(body: &serde_json::Value, upstream_protocol: ClientProtocol) -> TokenUsage`
  - `pub struct StreamingTokenCollector` with `new(upstream_protocol: ClientProtocol) -> Self`, `ingest(&mut self, chunk: &serde_json::Value) -> bool`, `tokens(&self) -> TokenUsage`
  - `pub fn inject_stream_options(fwd: &mut serde_json::Value)`

- [ ] **Step 1: Write the failing tests**

Replace `src-tauri/src/statistics/extract.rs` entirely with the tests (implementation stubs from Task 1 can stay until Step 3):

```rust
use crate::proxy::dispatch::ClientProtocol;
use serde_json::{json, Value};

use super::TokenUsage;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_non_stream_paths() {
        let body = json!({"usage": {"prompt_tokens": 15, "completion_tokens": 30, "total_tokens": 45}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::OpenAI), (Some(15), Some(30)));
    }

    #[test]
    fn anthropic_non_stream_paths() {
        let body = json!({"usage": {"input_tokens": 25, "output_tokens": 50}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::Anthropic), (Some(25), Some(50)));
    }

    #[test]
    fn total_only_vendor_falls_back() {
        // Non-standard vendor: only total_tokens → (Some(total), Some(0)) so the
        // displayed input+output total stays exact (spec §Extraction).
        let body = json!({"usage": {"total_tokens": 100}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::OpenAI), (Some(100), Some(0)));
    }

    #[test]
    fn missing_usage_degrades_to_none() {
        assert_eq!(extract_tokens_from_json(&json!({"choices": []}), ClientProtocol::OpenAI), (None, None));
        assert_eq!(extract_tokens_from_json(&json!({"usage": null}), ClientProtocol::OpenAI), (None, None));
    }

    #[test]
    fn half_present_keeps_half() {
        let body = json!({"usage": {"prompt_tokens": 12}});
        assert_eq!(extract_tokens_from_json(&body, ClientProtocol::OpenAI), (Some(12), None));
    }

    #[test]
    fn openai_stream_usage_chunk_completes() {
        let mut c = StreamingTokenCollector::new(ClientProtocol::OpenAI);
        assert!(!c.ingest(&json!({"choices": [{"delta": {"content": "hi"}}]})));
        assert!(c.ingest(&json!({"choices": [], "usage": {"prompt_tokens": 15, "completion_tokens": 30}})));
        assert_eq!(c.tokens(), (Some(15), Some(30)));
    }

    #[test]
    fn openai_stream_null_usage_does_not_complete() {
        // OpenAI sends "usage": null on every intermediate chunk — must not fire.
        let mut c = StreamingTokenCollector::new(ClientProtocol::OpenAI);
        assert!(!c.ingest(&json!({"choices": [{"delta": {}}], "usage": null})));
        assert_eq!(c.tokens(), (None, None));
    }

    #[test]
    fn openai_stream_nonstandard_gateway_usage_on_content_chunk() {
        // Some gateways attach usage to the last CONTENT chunk (choices non-empty)
        // — accept usage wherever it appears (spec v1.9 #3).
        let mut c = StreamingTokenCollector::new(ClientProtocol::OpenAI);
        assert!(c.ingest(&json!({"choices": [{"delta": {"content": "hi"}}], "usage": {"prompt_tokens": 5, "completion_tokens": 7}})));
        assert_eq!(c.tokens(), (Some(5), Some(7)));
    }

    #[test]
    fn anthropic_stream_events() {
        let mut c = StreamingTokenCollector::new(ClientProtocol::Anthropic);
        assert!(!c.ingest(&json!({"type": "message_start", "message": {"usage": {"input_tokens": 25, "output_tokens": 1}}})));
        assert_eq!(c.tokens(), (Some(25), None));
        assert!(c.ingest(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 50}})));
        assert_eq!(c.tokens(), (Some(25), Some(50)));
    }

    #[test]
    fn anthropic_missing_message_start_does_not_complete() {
        // message_delta without a prior message_start: input unknown → not complete.
        let mut c = StreamingTokenCollector::new(ClientProtocol::Anthropic);
        assert!(!c.ingest(&json!({"type": "message_delta", "usage": {"output_tokens": 50}})));
    }

    #[test]
    fn inject_merges_into_existing_options() {
        let mut fwd = json!({"model": "x", "stream": true, "stream_options": {"other": 1}});
        inject_stream_options(&mut fwd);
        assert_eq!(fwd["stream_options"], json!({"other": 1, "include_usage": true}));
    }

    #[test]
    fn inject_overrides_client_false() {
        // Statistics are proxy-side; a client "false" cannot opt the proxy out.
        let mut fwd = json!({"stream_options": {"include_usage": false}});
        inject_stream_options(&mut fwd);
        assert_eq!(fwd["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn inject_on_missing_key_creates_it() {
        let mut fwd = json!({"model": "x", "stream": true});
        inject_stream_options(&mut fwd);
        assert_eq!(fwd["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn inject_on_non_object_body_is_a_noop() {
        // Defensive: malformed non-object body must not panic (IndexMut would).
        let mut fwd = json!([1, 2, 3]);
        inject_stream_options(&mut fwd);
        assert_eq!(fwd, json!([1, 2, 3]));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::extract`
Expected: FAIL — placeholder returns `(None, None)`; collector/inject undefined.

- [ ] **Step 3: Implement**

Replace the placeholder body of `src-tauri/src/statistics/extract.rs` (above the tests) with:

```rust
/// Extract (input, output) from an already-parsed upstream response body.
/// Pure — no I/O, no stream consumption. Missing fields degrade to (None, None);
/// total-only vendors get (Some(total), Some(0)) (spec §Extraction Implementation).
pub fn extract_tokens_from_json(body: &Value, upstream_protocol: ClientProtocol) -> TokenUsage {
    let usage = &body["usage"];
    let (input, output) = match upstream_protocol {
        ClientProtocol::OpenAI => (usage["prompt_tokens"].as_u64(), usage["completion_tokens"].as_u64()),
        ClientProtocol::Anthropic => (usage["input_tokens"].as_u64(), usage["output_tokens"].as_u64()),
    };
    match (input, output) {
        (i @ Some(_), o @ Some(_)) => (i, o),
        (None, None) => match usage["total_tokens"].as_u64() {
            Some(total) => (Some(total), Some(0)),
            None => (None, None),
        },
        partial => partial,
    }
}

/// Accumulates token usage from upstream SSE chunks. Fed by the forwarding
/// pipeline (tap) — never reads streams itself. `ingest` returns true when
/// both halves are known; further calls are cheap no-ops.
pub struct StreamingTokenCollector {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    upstream_protocol: ClientProtocol,
}

impl StreamingTokenCollector {
    pub fn new(upstream_protocol: ClientProtocol) -> Self {
        Self { input_tokens: None, output_tokens: None, upstream_protocol }
    }

    pub fn ingest(&mut self, chunk: &Value) -> bool {
        match self.upstream_protocol {
            // OpenAI spec puts usage in a final empty-choices chunk, but some
            // non-standard gateways attach it to the last content chunk —
            // accept usage wherever it appears, as long as a number comes out.
            // Intermediate chunks' "usage": null yield None → no false fire.
            ClientProtocol::OpenAI => {
                if let Some(usage) = chunk.get("usage") {
                    let prompt = usage["prompt_tokens"].as_u64();
                    let completion = usage["completion_tokens"].as_u64();
                    if prompt.is_some() || completion.is_some() {
                        self.input_tokens = prompt;
                        self.output_tokens = completion;
                        return true;
                    }
                }
                false
            }
            ClientProtocol::Anthropic => match chunk.get("type").and_then(|t| t.as_str()) {
                Some("message_start") => {
                    self.input_tokens = chunk["message"]["usage"]["input_tokens"].as_u64();
                    false
                }
                Some("message_delta") => {
                    self.output_tokens = chunk["usage"]["output_tokens"].as_u64();
                    self.input_tokens.is_some() // complete when both present
                }
                _ => false,
            },
        }
    }

    pub fn tokens(&self) -> TokenUsage {
        (self.input_tokens, self.output_tokens)
    }
}

/// Inject `stream_options.include_usage = true` into an upstream OpenAI-family
/// streaming request (clients never send it; without it no usage chunk comes
/// back and every stream degrades to RequestsOnly — spec §stream_options
/// injection). Merge semantics: keeps other client-set options; forces
/// include_usage; no-op on a malformed non-object body.
pub fn inject_stream_options(fwd: &mut Value) {
    if let Some(obj) = fwd.as_object_mut() {
        let mut opts = obj
            .get("stream_options")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        opts.insert("include_usage".to_string(), Value::Bool(true));
        obj.insert("stream_options".to_string(), Value::Object(opts));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::extract`
Expected: 14 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/statistics/extract.rs
git commit -m "feat(statistics): pure token extraction + stream collector + stream_options injection"
```

---

### Task 6: Non-streaming dispatch integration

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` — `openai_passthrough` (~686), `anthropic_passthrough` (~736), `translate_via_openai` (~783), `select_and_call` (~647), `attempt_with_retry` (~991), `dispatch_non_stream` (~214) + `AttemptOutcome`/`AttemptWithRetry` enums + tests

**Interfaces:**
- Consumes: `extract::extract_tokens_from_json` (Task 5), `AppStateInner.statistics` (Task 4), `ProviderType` (Task 1).
- Produces: `AttemptWithRetry::Respond(Response<Body>, TokenUsage)` (Task 7 builds on this).

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/proxy/dispatch.rs` tests module, add a state builder variant that attaches a real statistics service to a tempdir:

```rust
    use crate::statistics::{ProviderType, UsageStatisticsService};

    /// `chain_state` + a live statistics service over a tempdir (tests assert DB state).
    async fn chain_state_with_stats(
        clock: Arc<dyn Clock>,
        a: &str,
    ) -> (AppState, tempfile::TempDir) {
        // Build exactly like chain_state(…, a, None, None), then attach statistics.
        // (Reuse chain_state body; simplest: build via chain_state then re-build
        //  the Arc — instead, construct directly here.)
        let mut cfg = AppConfig::default();
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        cfg.providers.push(Provider {
            id: "zhipu".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
            openai_base_url: Some(a.into()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.models.push(mk_model("m_a", "zhipu", None));
        secrets.set_key("zhipu", "sk-test").unwrap();
        cfg.profiles.push(Profile {
            id: "p".into(), name: "glm-5.2".into(), aliases: vec![],
            backing_model_id: "m_a".into(), ..Default::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let stats = UsageStatisticsService::start(dir.path(), clock.clone()).unwrap();
        let state: AppState = Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: Default::default(),
            secrets,
            health: Default::default(),
            clock,
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
            last_served_provider: std::sync::Mutex::new(None),
            recorder: std::sync::RwLock::new(None),
            statistics: Some(stats),
        });
        (state, dir)
    }

    async fn stats_of(state: &AppState, provider_id: &str) -> crate::statistics::ProviderStats {
        let svc = state.statistics.as_ref().unwrap();
        for _ in 0..200 {
            let all = futures::executor::block_on(svc.get_all_stats()).unwrap_or_default();
            if let Some(s) = all.into_iter().find(|s| s.provider_id == provider_id) {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("stats for {provider_id} never appeared");
    }

    #[tokio::test]
    async fn non_stream_records_tokens_for_served_provider() {
        let mock_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"ok"}}],
                                   "usage":{"prompt_tokens":150,"completion_tokens":250,"total_tokens":400}}),
            ))
            .mount(&mock_a).await;
        let (state, _dir) = chain_state_with_stats(Arc::new(FakeClock::new(1000)), &mock_a.uri()).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let s = stats_of(&state, "zhipu").await;
        assert_eq!(s.total_requests, 1);
        assert_eq!(s.total_input_tokens, 150);
        assert_eq!(s.total_output_tokens, 250);
    }

    #[tokio::test]
    async fn non_stream_without_usage_records_request_only() {
        let mock_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"ok"}}]}),
            ))
            .mount(&mock_a).await;
        let (state, _dir) = chain_state_with_stats(Arc::new(FakeClock::new(1000)), &mock_a.uri()).await;
        let app = build_router(state.clone());
        let _ = app.oneshot(oai_post()).await.unwrap();
        let s = stats_of(&state, "zhipu").await;
        assert_eq!(s.total_requests, 1);
        assert_eq!(s.total_tokenized_requests, 0); // RequestsOnly
    }

    #[tokio::test]
    async fn upstream_error_response_not_counted() {
        // "Successful requests only": a 401 passthrough must not be recorded.
        let mock_a = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"no"})))
            .mount(&mock_a).await;
        let (state, _dir) = chain_state_with_stats(Arc::new(FakeClock::new(1000)), &mock_a.uri()).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(state.statistics.as_ref().unwrap().try_stats().map(|s| s.is_empty()).unwrap_or(true));
    }
```

**Note:** `upstream_error_response_not_counted` uses `try_stats` (last-query cache) with a fallback `unwrap_or(true)` — if no Query has run, empty is the right answer, so this form is safe; alternatively use the `stats_of`-style `block_on(get_all_stats)` loop asserting absence (mirror the `fallback_credits_serving_provider` tail).

    #[tokio::test]
    async fn fallback_credits_serving_provider() {
        // m_a 429 → m_b serves: the RECORD must land on pb (the provider that
        // actually served), not zhipu (spec §Non-streaming integration point).
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}],
                                   "usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}),
            ))
            .mount(&mock_b).await;
        // chain_state gives zhipu→pb with fallback m_a→m_b; attach stats by
        // building through chain_state then swapping in a service:
        let dir = tempfile::tempdir().unwrap();
        let stats = UsageStatisticsService::start(dir.path(), Arc::new(FakeClock::new(1000))).unwrap();
        let state = {
            let mut s = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None).await;
            // AppState is Arc — rebuild is not possible; instead chain_state
            // must be extended. See note below.
            let AppStateInnerRef = &mut s;
            let _ = AppStateInnerRef;
            s
        };
        let _ = state;
        unimplemented!("see Step 3 note — chain_state gains a `stats` parameter instead")
    }
```

**Note (fold into Step 3):** `AppState` is an `Arc` — fields can't be swapped after construction. Instead of the contortion above, change `chain_state` to take `stats: Option<Arc<UsageStatisticsService>>` (all existing call sites pass `None`; the two new tests pass `Some(...)`). Delete the `fallback_credits_serving_provider` draft body and write it as:

```rust
    #[tokio::test]
    async fn fallback_credits_serving_provider() {
        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({"error":{"code":1302}})))
            .mount(&mock_a).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"from-b"}}],
                                   "usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}),
            ))
            .mount(&mock_b).await;
        let dir = tempfile::tempdir().unwrap();
        let stats = UsageStatisticsService::start(dir.path(), Arc::new(FakeClock::new(1000))).unwrap();
        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), Some(&mock_b.uri()), None, Some(stats)).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let s = stats_of(&state, "pb").await;
        assert_eq!(s.total_requests, 1);
        assert_eq!(s.total_input_tokens, 10);
        // zhipu was rate-limited, never served → no row.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let all = futures::executor::block_on(state.statistics.as_ref().unwrap().get_all_stats()).unwrap();
        assert!(all.iter().all(|x| x.provider_id != "zhipu"));
    }
    }
```

and drop the `chain_state_with_stats` helper in favor of the parameterized `chain_state`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml dispatch::tests::non_stream`
Expected: FAIL — no recording happens (`stats never appeared` panic).

- [ ] **Step 3: Implement the integration**

In `src-tauri/src/proxy/dispatch.rs`:

1. Change `AttemptOutcome::Respond` to carry tokens (dispatch.rs:35):

```rust
enum AttemptOutcome {
    /// Forward this response to the client + the tokens extracted from its
    /// buffered body ((None, None) when the vendor reported no usage).
    Respond(Response<Body>, TokenUsage),
    /// Provider rate-limited / quota exhausted -> trip breaker + walk to fallback.
    RateLimited,
}
```

and `AttemptWithRetry` (dispatch.rs:975):

```rust
enum AttemptWithRetry {
    Respond(Response<Body>, TokenUsage),
    RateLimited(Recover),
}
```

Add the import: `use crate::statistics::{extract::extract_tokens_from_json, ProviderType, TokenUsage};`

2. In each non-stream handler, extract tokens from the already-buffered body and return them (zero extra reads — spec §Non-streaming integration point). In `openai_passthrough` (~line 719, where `v` is parsed):

```rust
    let tokens = extract_tokens_from_json(&v, ClientProtocol::OpenAI);
```

and every `Ok(AttemptOutcome::Respond(...))` in that fn becomes `Ok(AttemptOutcome::Respond(..., tokens))`. Same pattern in `anthropic_passthrough` (`ClientProtocol::Anthropic`) and `translate_via_openai` (`ClientProtocol::OpenAI` — the *upstream* protocol, per spec §Protocol note).

3. `select_and_call` and `stream_attempt` (Task 7 covers the stream body; for now make `stream_attempt` return `Respond(resp, (None, None))` where it currently constructs `Respond` — Task 7 replaces those) pass the tuple through unchanged.

4. `attempt_with_retry` (~1014): `AttemptOutcome::Respond(r) => return Ok(AttemptWithRetry::Respond(r, t))` — destructure both.

5. Record in `dispatch_non_stream`'s success arm (~line 303):

```rust
            Ok(AttemptWithRetry::Respond(resp, tokens)) => {
                outcome.served_model_id = Some(current.clone());
                // 用量统计(spec §Service Integration):仅成功(2xx)响应计数——
                // 错误透传(401/500)不算一次成功请求。非阻塞 channel send。
                if resp.status().is_success() {
                    if let Some(stats) = &state.statistics {
                        stats.record(
                            &model_snap.provider_id,
                            &model_snap.vendor,
                            ProviderType::from_vendor(&model_snap.vendor),
                            tokens,
                        );
                    }
                }
                return Ok(resp);
            }
```

(`model_snap` is in scope from the loop; it carries `provider_id` + `vendor` — the serving provider, which is what we credit.)

6. Update `dispatch_stream`'s `Ok(AttemptWithRetry::Respond(resp, _tokens))` arm to destructure and ignore tokens for now (`_tokens`) — Task 7 uses them.

7. Update the existing `chain_state` helper signature per the Step 1 note: `async fn chain_state(clock, a, b, c, stats: Option<Arc<crate::statistics::UsageStatisticsService>>) -> AppState`, literal gains `statistics: stats,` and all ~6 existing call sites append `, None`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all statistics tests + all pre-existing dispatch tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(statistics): record tokens at non-stream buffering points (upstream protocol keyed)"
```

---

### Task 7: Streaming dispatch integration — tap + injection

**Files:**
- Modify: `src-tauri/src/statistics/extract.rs` (add `SseTap`)
- Modify: `src-tauri/src/proxy/dispatch.rs` — `send_stream` (~517), `translate_stream_commit` (~558), `stream_attempt` (~434), `dispatch_stream` (~332) + tests

**Interfaces:**
- Consumes: `StreamingTokenCollector`, `inject_stream_options` (Task 5), `RecordOnce` (Task 3), `statistics` field (Task 4).
- Produces: `extract::SseTap<S>` — a forwarding byte-stream wrapper that feeds the collector.

- [ ] **Step 1: Write the failing tests**

Add to `src-tauri/src/statistics/extract.rs` tests:

```rust
    use futures::StreamExt;

    fn byte_stream(chunks: Vec<&'static str>) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
        futures::stream::iter(
            chunks.into_iter().map(|c| Ok(bytes::Bytes::from_static(c.as_bytes()))),
        )
    }

    #[tokio::test]
    async fn sse_tap_forwards_bytes_verbatim_and_collects() {
        // Chunks deliberately split mid-event: the tap must reassemble events
        // for parsing while forwarding the ORIGINAL bytes unchanged.
        let chunks = vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"He\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}], \"usage\":",
            " {\"prompt_tokens\": 15, \"completion_tokens\": 30}}\n\n",
            "data: [DONE]\n\n",
        ];
        let expected: Vec<u8> = chunks.concat().into_bytes();
        let (tap, handle) = SseTap::new(ClientProtocol::OpenAI);
        let mut out: Vec<u8> = Vec::new();
        let mut s = tap.wrap(byte_stream(chunks));
        while let Some(item) = s.next().await {
            out.extend_from_slice(&item.unwrap());
        }
        assert_eq!(out, expected); // byte-identical forwarding
        assert_eq!(handle.finish(), (Some(15), Some(30))); // resolves after drain
    }

    #[tokio::test]
    async fn sse_tap_aborted_stream_still_resolves() {
        // Stream ends without usage → handle resolves (None, None), no hang.
        let chunks = vec!["data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n"];
        let (tap, handle) = SseTap::new(ClientProtocol::OpenAI);
        let mut s = tap.wrap(byte_stream(chunks));
        while let Some(item) = s.next().await { let _ = item.unwrap(); }
        assert_eq!(handle.finish(), (None, None));
    }
```

And in `dispatch.rs` tests:

```rust
    #[tokio::test]
    async fn stream_records_tokens_via_translate_path() {
        // Anthropic client → OpenAI upstream (translate): usage chunk in the
        // OpenAI stream must be collected even though the client sees Anthropic.
        let mock_a = MockServer::start().await;
        let sse = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                   data: {\"choices\":[],\"usage\":{\"prompt_tokens\":25,\"completion_tokens\":50}}\n\n\
                   data: [DONE]\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(sse.as_bytes().to_vec()),
            )
            .mount(&mock_a).await;
        let dir = tempfile::tempdir().unwrap();
        let stats = UsageStatisticsService::start(dir.path(), Arc::new(FakeClock::new(1000))).unwrap();
        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None, Some(stats)).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(anthropic_stream_post()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = body_str(resp).await; // drain the stream so the tap completes
        let s = stats_of(&state, "zhipu").await;
        assert_eq!(s.total_requests, 1);
        assert_eq!(s.total_input_tokens, 25);
        assert_eq!(s.total_output_tokens, 50);
    }

    #[tokio::test]
    async fn stream_sends_include_usage_upstream() {
        // The proxy must inject stream_options.include_usage on OpenAI-family
        // streaming upstream requests (clients never send it themselves).
        let mock_a = MockServer::start().await;
        let sse = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
                   data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n\
                   data: [DONE]\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(sse.as_bytes().to_vec()),
            )
            .mount(&mock_a).await;
        let state = chain_state(Arc::new(FakeClock::new(1000)), &mock_a.uri(), None, None, None).await;
        let app = build_router(state.clone());
        let resp = app.oneshot(oai_stream_post()).await.unwrap();
        let _ = body_str(resp).await;
        let reqs = mock_a.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["stream_options"]["include_usage"], serde_json::json!(true));
    }

    fn oai_stream_post() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"model":"glm-5.2","stream":true,"messages":[{"role":"user","content":"hi"}]})
                    .to_string(),
            ))
            .unwrap()
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml statistics::extract::tests::sse_tap dispatch::tests::stream_records_tokens_via_translate_path dispatch::tests::stream_sends_include_usage_upstream`
Expected: FAIL — `SseTap` undefined; no recording; no injection.

- [ ] **Step 3: Implement `SseTap`**

Add to `src-tauri/src/statistics/extract.rs`:

```rust
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// Tap for a passthrough (raw-byte) SSE stream: forwards every byte
/// **unchanged** while reassembling complete SSE events (separator `\n\n`)
/// to feed a `StreamingTokenCollector`. Events split across chunks are
/// buffered until complete. The collector's result is delivered through the
/// returned oneshot-like handle when the stream ends (spec §Streaming
/// integration point — tap, never consume).
pub struct SseTap {
    collector: Arc<Mutex<StreamingTokenCollector>>,
}

impl SseTap {
    /// Returns (tap, handle). `handle` — a shared slot — resolves to the
    /// collected tokens once the wrapped stream has ended.
    pub fn new(upstream_protocol: ClientProtocol) -> (Self, SseTapHandle) {
        let collector = Arc::new(Mutex::new(StreamingTokenCollector::new(upstream_protocol)));
        (Self { collector: collector.clone() }, SseTapHandle { collector, finished: Arc::new(Mutex::new(false)) })
    }

    pub fn wrap<S>(&self, inner: S) -> TappedStream<S>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
    {
        TappedStream {
            inner,
            buf: Vec::new(),
            collector: self.collector.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SseTapHandle {
    collector: Arc<Mutex<StreamingTokenCollector>>,
    finished: Arc<Mutex<bool>>,
}

impl SseTapHandle {
    /// Mark the stream ended; returns the collected tokens. Idempotent.
    pub fn finish(&self) -> TokenUsage {
        *self.finished.lock().unwrap() = true;
        self.collector.lock().unwrap().tokens()
    }
    pub fn is_finished(&self) -> bool {
        *self.finished.lock().unwrap()
    }
}

/// The wrapping stream: yields the inner bytes untouched, feeds the collector.
pub struct TappedStream<S> {
    inner: S,
    buf: Vec<u8>,
    collector: Arc<Mutex<StreamingTokenCollector>>,
}

impl<S, E> Stream for TappedStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(item)) => {
                if let Ok(bytes) = &item {
                    self.buf.extend_from_slice(bytes);
                    // Feed every COMPLETE event now in the buffer.
                    while let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
                        let event: Vec<u8> = self.buf.drain(..=pos + 1).collect();
                        if let Some(data) = sse_event_data(&event) {
                            if let Ok(chunk) = serde_json::from_str::<Value>(&data) {
                                self.collector.lock().unwrap().ingest(&chunk);
                            }
                        }
                    }
                }
                Poll::Ready(Some(item))
            }
        }
    }
}

/// `data: <payload>` line of one SSE event (ignores comments/event: lines).
fn sse_event_data(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with("data:"))
        .map(|l| l["data:".len()..].trim().to_string())
}
```

- [ ] **Step 4: Wire the tap into dispatch**

In `src-tauri/src/proxy/dispatch.rs`:

1. **Injection** — in `send_stream` (~517), for the two OpenAI-family paths only, right before building the request body:

```rust
        StreamPath::OpenAiPassthrough => {
            let mut fwd = req.clone();
            fwd["model"] = serde_json::Value::String(upstream.clone());
            fwd["stream"] = serde_json::Value::Bool(true);
            crate::statistics::extract::inject_stream_options(&mut fwd); // 统计需要 usage chunk
            (serde_json::to_vec(&fwd).unwrap_or_default(), join_url(snap.openai_base_url.as_deref().unwrap(), BackendProtocol::OpenAI))
        }
        StreamPath::Translate => {
            let mut oai = anthropic_to_openai(req);
            oai["model"] = serde_json::Value::String(upstream.clone());
            oai["stream"] = serde_json::Value::Bool(true);
            crate::statistics::extract::inject_stream_options(&mut oai); // 同上
            (serde_json::to_vec(&oai).unwrap_or_default(), join_url(snap.openai_base_url.as_deref().unwrap(), BackendProtocol::OpenAI))
        }
        // AnthropicPassthrough: NEVER inject (Anthropic has no stream_options).
```

2. **Translate path** — `stream_attempt` (~434) gains a `sink: &mut RecordOnce` parameter (built by `dispatch_stream`); `translate_stream_commit` (~558) gains the same. Inside `translate_stream_commit`'s `async_stream::stream!` body: create a `StreamingTokenCollector` (OpenAI — upstream protocol) before the generator, feed it each parsed chunk alongside `t.ingest(Some(&chunk))`, and when the loop ends (`[DONE]`, stream end, or error) call `sink.send()` (tokens come from the collector — `RecordOnce` carries `(None, None)` tokens and is only responsible for firing; see note) .

**Design simplification for the sink:** rather than threading token values into `RecordOnce` after the fact, build the `StatEvent::Record` **inside** the stream body when it ends, with the collector's values:

```rust
// In dispatch_stream, before attempt_with_retry:
let mut sink = state.statistics.as_ref().map(|stats| {
    RecordOnce::new(stats.sender(), StatEvent::Record {
        provider_id: snap.provider_id.clone(),
        vendor: snap.vendor.clone(),
        provider_type: ProviderType::from_vendor(&snap.vendor),
        tokens: (None, None),
    })
});
```

This needs one more Task-3 method: `pub fn sender(&self) -> std::sync::mpsc::Sender<StatEvent>` on `UsageStatisticsService` (add it: `pub fn sender(&self) -> std::sync::mpsc::Sender<StatEvent> { self.tx.clone() }`). Then at each stream-end point, instead of plain `sink.send()`, construct the final event with real tokens:

```rust
// helper closure kept simple: send the record with the collector's tokens
if let Some(mut s) = sink.take() { s.send(); }
```

Since `RecordOnce`'s event was built with `(None, None)`, make `RecordOnce` hold a **token-updatable** event — change its shape to:

```rust
pub struct RecordOnce {
    tx: std::sync::mpsc::Sender<StatEvent>,
    evt: Option<StatEvent>,
}
impl RecordOnce {
    pub fn set_tokens(&mut self, tokens: TokenUsage) {
        if let Some(StatEvent::Record { tokens: t, .. }) = self.evt.as_mut() { *t = tokens; }
    }
}
```

(Add `set_tokens` to Task 3's `RecordOnce` — one small method; note it in the commit.) The translate stream body calls `sink.set_tokens(collector.tokens())` then `sink.send()` at end-of-stream; drop-aborts still send `(None, None)` via `Drop`.

3. **Passthrough path** — in `stream_attempt`'s 2xx passthrough arm (~477):

```rust
        StreamPath::OpenAiPassthrough | StreamPath::AnthropicPassthrough => {
            let status = resp.status();
            let ct = resp.headers().get("content-type").cloned();
            let upstream_proto = match path {
                StreamPath::AnthropicPassthrough => ClientProtocol::Anthropic,
                _ => ClientProtocol::OpenAI,
            };
            let (tap, handle) = crate::statistics::extract::SseTap::new(upstream_proto);
            let sink = sink.map(|s| (s, handle));
            let body_stream = tap.wrap(resp.bytes_stream());
            // Feed the handle's tokens into the sink when the stream ends:
            let mut wrapped = async_stream::stream! {
                let mut inner = Box::pin(body_stream);
                while let Some(item) = inner.next().await {
                    match item {
                        Ok(b) => yield Ok::<_, std::io::Error>(b),
                        Err(e) => { yield Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())); break; }
                    }
                }
                if let Some((mut s, handle)) = sink {
                    s.set_tokens(handle.finish());
                    s.send();
                }
            };
            let mut out = Response::builder().status(status);
            if let Some(ct) = ct { out = out.header("content-type", ct); }
            Ok(AttemptOutcome::Respond(out.body(Body::from_stream(wrapped)).unwrap(), (None, None)))
        }
```

(`futures::StreamExt` is already imported at the top of dispatch.rs; `async_stream` too.)

4. **Record in `dispatch_stream`'s success arm** — the stream record is sent by the sink inside the stream body (above), so `dispatch_stream` itself needs no extra call beyond constructing the sink and passing `&mut sink` down through `attempt_with_retry` → `stream_attempt` (add the parameter; `attempt_with_retry` passes it through only for `AttemptKind::Stream`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all statistics + dispatch tests PASS, including the two sse_tap tests, the translate-path token test, and the include_usage injection test.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/statistics/extract.rs src-tauri/src/statistics/mod.rs src-tauri/src/proxy/dispatch.rs
git commit -m "feat(statistics): streaming tap extraction + stream_options injection"
```

---

### Task 8: Usage-polling hook + provider deletion hook

**Files:**
- Modify: `src-tauri/src/commands.rs:1156` (`query_usage`), `commands.rs:129-150` (`delete_provider`)
- Modify: `src-tauri/src/proxy/dispatch.rs:1058` (`usage_snapshot_realtime`)

**Interfaces:**
- Consumes: `AppStateInner::notify_usage_snapshot` (Task 4), `statistics.delete_provider` (Task 3).

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/commands.rs` tests (there is an existing tests module at the bottom), add:

```rust
    #[tokio::test]
    async fn query_usage_notifies_statistics_reset() {
        // A usage query that returns a snapshot must forward it to the
        // statistics service (the reset choke point) — spec §Reset trigger source.
        use crate::config::{AppConfig, BackendKind, MemoryStore, Profile, Provider, SecretStoreHandle};
        use crate::proxy::state::AppStateInner;
        use crate::proxy::AppState;
        use crate::usage::{UsageSnapshot, UsageTier};

        let dir = tempfile::tempdir().unwrap();
        let stats = crate::statistics::UsageStatisticsService::start(
            dir.path(), std::sync::Arc::new(crate::proxy::health::FakeClock::new(1000)),
        ).unwrap();
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "p1".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
            openai_base_url: Some("https://x/v1".into()), anthropic_base_url: None, usage_creds: None,
        });
        cfg.profiles.push(Profile {
            id: "p".into(), name: "glm-4.6".into(), aliases: vec![],
            backing_model_id: "m".into(), ..Default::default()
        });
        let state: AppState = std::sync::Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: Default::default(),
            secrets: SecretStoreHandle::new(std::sync::Arc::new(MemoryStore::default()), BackendKind::Keyring),
            health: Default::default(),
            clock: std::sync::Arc::new(crate::proxy::health::FakeClock::new(1000)),
            usage_cache: Default::default(),
            bound_port: std::sync::Mutex::new(None),
            server_handle: std::sync::Mutex::new(None),
            bind_error: std::sync::Mutex::new(None),
            polling_handle: std::sync::Mutex::new(None),
            last_served_provider: std::sync::Mutex::new(None),
            recorder: std::sync::RwLock::new(None),
            statistics: Some(stats.clone()),
        });
        let snapshot = UsageSnapshot {
            total: None, remaining: None, reset_at: None, unit: "%".into(),
            raw_summary: None, plan: None, billing_model: "plan".into(), plan_info: None,
            tiers: vec![UsageTier { window: "five_hour".into(), used_pct: Some(10.0), reset_at: Some(5000) }],
        };
        state.notify_usage_snapshot("p1", &snapshot);
        for _ in 0..200 {
            let all = futures::executor::block_on(stats.get_all_stats()).unwrap_or_default();
            if let Some(s) = all.into_iter().find(|s| s.provider_id == "p1") {
                assert_eq!(s.last_5h.reset_at, Some(5000));
                assert!(s.last_5h.is_active);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("reset never applied");
    }
```

(`futures` is already a workspace dependency — `dispatch.rs` imports it — so no Cargo.toml change.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml query_usage_notifies`
Expected: FAIL — `last_5h.reset_at` never appears (hook not wired).

- [ ] **Step 3: Implement the two hooks**

In `commands.rs` `query_usage`, after `state.usage_cache.set(provider_id, snap.clone(), now);` (line 1156):

```rust
    state.notify_usage_snapshot(provider_id, &snap);
```

In `dispatch.rs` `usage_snapshot_realtime`, after `state.usage_cache.set(&snap.provider_id, usage.clone(), now);` (line 1058):

```rust
        state.notify_usage_snapshot(&snap.provider_id, &usage);
```

In `commands.rs` `delete_provider`, after the keyring purge block (~line 148):

```rust
    // 级联删除统计行(跨文件 FK 不可行,显式消息代替 — spec §Database Schema)。
    if let Some(stats) = &state.statistics {
        stats.delete_provider(&provider_id);
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: ALL tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/proxy/dispatch.rs
git commit -m "feat(statistics): usage-poll reset hook + provider deletion cascade"
```

---

### Task 9: Backend command + TS bridge

**Files:**
- Modify: `src-tauri/src/commands.rs` (new command), `src-tauri/src/lib.rs` (~244, invoke_handler list)
- Modify: `src/lib/types.ts`, `src/lib/commands.ts`

**Interfaces:**
- Consumes: `UsageStatisticsService::get_all_stats` (Task 3).
- Produces (used by Task 10): TS `ProviderStats`/`WindowStats` types + `getUsageStatistics(): Promise<ProviderStats[]>`.

- [ ] **Step 1: Add the command**

In `src-tauri/src/commands.rs` (near `get_usage`, ~line 478):

```rust
/// 每服务商用量统计(请求次数 + token 消耗)。统计禁用时返回空数组。
#[tauri::command]
pub async fn get_usage_statistics(
    state: State<'_, AppState>,
) -> Result<Vec<crate::statistics::ProviderStats>, String> {
    match &state.statistics {
        Some(s) => s.get_all_stats().await,
        None => Ok(Vec::new()),
    }
}
```

Register in `src-tauri/src/lib.rs` `invoke_handler` list (after `commands::get_usage,`):

```rust
            commands::get_usage_statistics,
```

- [ ] **Step 2: Add TS types**

In `src/lib/types.ts` (after the `UsageEntry` block, ~line 118; **snake_case field names** — serde uses no rename_all):

```typescript
/** 每服务商用量统计(与 Rust statistics::ProviderStats 镜像,snake_case)。 */
export interface StatWindow {
  requests: number;
  tokenized_requests: number;
  input_tokens: number;
  output_tokens: number;
  reset_at: number | null;
  is_active: boolean;
  /** 服务端从 tokenized/requests 派生:"Full" | "RequestsOnly" | "Unknown" */
  quality: "Full" | "RequestsOnly" | "Unknown";
  /** 部分覆盖时为百分比(0 < tokenized < requests),否则 null */
  coverage_pct: number | null;
}

export interface ProviderStats {
  provider_id: string;
  vendor: string;
  provider_type: "plan" | "consumption";
  last_5h: StatWindow;
  last_1w: StatWindow;
  last_1m: StatWindow;
  total_requests: number;
  total_tokenized_requests: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_tokens: number;
}
```

- [ ] **Step 3: Add the bridge call**

In `src/lib/commands.ts` (after `getUsage`, ~line 116):

```typescript
export const getUsageStatistics = () => invoke<ProviderStats[]>("get_usage_statistics");
```

(with `ProviderStats` added to the existing type import at the top of the file, following the established pattern).

- [ ] **Step 4: Verify**

Run: `cargo test --manifest-path src-tauri/Cargo.toml` and `pnpm exec vue-tsc --noEmit`
Expected: backend PASS; frontend type-check PASS (types not yet consumed — that's Task 10).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/commands.ts
git commit -m "feat(statistics): get_usage_statistics command + TS bridge"
```

---

### Task 10: Usage.vue display

**Files:**
- Modify: `src/stores/runtime.ts`, `src/views/Usage.vue`

**Interfaces:**
- Consumes: `ProviderStats`/`StatWindow` types + `getUsageStatistics` (Task 9).

- [ ] **Step 1: Extend the runtime store**

In `src/stores/runtime.ts`:

```typescript
import { defineStore } from "pinia";
import { ref } from "vue";
import type { ModelHealth, ProviderStats, UsageEntry } from "../lib/types";
import * as api from "../lib/commands";

// Runtime observability: per-provider usage + per-model breaker health + usage statistics.
export const useRuntimeStore = defineStore("runtime", () => {
  const usage = ref<UsageEntry[]>([]);
  const health = ref<Record<string, ModelHealth>>({});
  const stats = ref<ProviderStats[]>([]);
  const loading = ref(false);

  async function refresh() {
    loading.value = true;
    try {
      [usage.value, health.value, stats.value] = await Promise.all([
        api.getAllUsage(),
        api.getModelHealth(),
        api.getUsageStatistics(),
      ]);
    } finally {
      loading.value = false;
    }
  }

  return { usage, health, stats, loading, refresh };
});
```

(The page's existing `usePolling(() => runtime.refresh(), …)` now refreshes statistics too — no polling change needed.)

- [ ] **Step 2: Add display helpers to Usage.vue**

In the `<script setup>` of `src/views/Usage.vue`, after the `cards` computed (~line 52):

```typescript
import type { ProviderStats, StatWindow } from "../lib/types";

/** 窗口 key(与后端 tiers 一致)→ stats 字段。 */
function statsWindow(stats: ProviderStats | undefined, key: string): StatWindow | null {
  if (!stats) return null;
  const w = key === "five_hour" ? stats.last_5h : key === "weekly_limit" ? stats.last_1w : stats.last_1m;
  return w.is_active ? w : null;
}

function fmtNum(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "K";
  return String(n);
}

function findStats(providerId: string): ProviderStats | undefined {
  return runtime.stats.find((s) => s.provider_id === providerId);
}
```

- [ ] **Step 3: Render stats in the tier rows**

In the template's plan-billing block (~line 227-240), replace the `tier__reset` line's neighborhood to add token/request stats between the progress bar and the reset time (spec §Display Layout: 缩短进度条 → token 量 → 请求次数 → 重置时间):

```vue
            <div v-for="row in c.rows" :key="row.label" class="tier">
              <span class="tier__label">{{ row.label }}</span>
              <template v-if="row.pct != null">
                <span class="tier__pct mono">{{ fmtPct(row.pct) }}%</span>
                <NProgress
                  class="tier__bar tier__bar--short"
                  :percentage="fmtPct(row.pct)"
                  :status="status(row.pct)"
                  :show-indicator="false"
                />
                <!-- token 用量(悬浮显示输入/输出拆分;无数据降级为仅请求次数) -->
                <NTooltip v-if="statsWindow(findStats(c.provider_id), row.key)">
                  <template #trigger>
                    <span class="tier__stat mono">
                      {{ fmtNum((statsWindow(findStats(c.provider_id), row.key)!.input_tokens) +
                                (statsWindow(findStats(c.provider_id), row.key)!.output_tokens)) }}
                      <span class="tier__stat-unit">tok</span>
                    </span>
                  </template>
                  <div style="display: flex; flex-direction: column; gap: 2px">
                    <span>输入 token：{{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.input_tokens) }}</span>
                    <span>输出 token：{{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.output_tokens) }}</span>
                    <span v-if="statsWindow(findStats(c.provider_id), row.key)!.requests > 0">
                      请求次数：{{ statsWindow(findStats(c.provider_id), row.key)!.requests }}
                    </span>
                  </div>
                </NTooltip>
                <span v-if="statsWindow(findStats(c.provider_id), row.key)" class="tier__stat mono">
                  ×{{ fmtNum(statsWindow(findStats(c.provider_id), row.key)!.requests) }}
                </span>
                <span class="tier__reset mono">重置时间 {{ resetLabel(row.reset) }}</span>
              </template>
              <span v-else class="tier__na">N/A</span>
            </div>
```

Note: the `windows` const (~line 20) already carries `key` ("five_hour"/"weekly_limit"/"monthly") and `cards` already maps rows from it — add `key` to the `TierRow` interface and the row mapping (currently only `label/pct/reset` are kept):

```typescript
interface TierRow {
  key: string;   // "five_hour" | "weekly_limit" | "monthly" — joins to ProviderStats
  label: string;
  pct: number | null;
  reset: number | null;
}
// in cards computed:
rows: windows.map((w) => {
  const t = entry.snapshot?.tiers?.find((x) => x.window === w.key);
  return { key: w.key, label: w.label, pct: t?.used_pct ?? null, reset: t?.reset_at ?? null };
}),
```

- [ ] **Step 4: Consumption cards + lifetime row**

For consumption-billing providers (DeepSeek), add a total-tokens row after the balance row (~line 222):

```vue
            <div v-if="findStats(c.provider_id)" class="balance-row">
              <span class="balance-row__label">累计 token</span>
              <NTooltip>
                <template #trigger>
                  <span class="balance-row__value mono">
                    {{ fmtNum(findStats(c.provider_id)!.total_tokens) }}
                    <span class="tier__stat-unit">tok</span>
                  </span>
                </template>
                <div style="display: flex; flex-direction: column; gap: 2px">
                  <span>输入：{{ fmtNum(findStats(c.provider_id)!.total_input_tokens) }}</span>
                  <span>输出：{{ fmtNum(findStats(c.provider_id)!.total_output_tokens) }}</span>
                  <span>请求：{{ fmtNum(findStats(c.provider_id)!.total_requests) }}</span>
                </div>
              </NTooltip>
            </div>
```

And a compact lifetime line at the bottom of every card (before the closing `</NSpace>` of the card, ~line 246), Chinese copy:

```vue
        <div v-if="findStats(c.provider_id)" class="lifetime mono">
          累计：{{ fmtNum(findStats(c.provider_id)!.total_tokens) }} tok ·
          {{ fmtNum(findStats(c.provider_id)!.total_requests) }} 次请求
        </div>
```

Add styles to the `<style scoped>` block (follow the existing token-var pattern):

```css
.tier__bar--short {
  flex: 0 1 120px; /* shortened so stats + reset fit (spec §Display Layout) */
}
.tier__stat {
  flex-shrink: 0;
  font-size: 12px;
  color: var(--sl-text-2);
  white-space: nowrap;
}
.tier__stat-unit {
  font-size: 11px;
  color: var(--sl-text-3);
}
.lifetime {
  font-size: 12px;
  color: var(--sl-text-3);
}
```

- [ ] **Step 5: Type-check and manual smoke test**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS.

Run: `pnpm tauri dev` — configure a provider, send a test request via the Sources page's connection test (or any agent request), open the Usage page: token counts and request counts appear next to the windows; hover shows 输入/输出拆分; DeepSeek card shows 累计 token.

- [ ] **Step 6: Full suite + commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && pnpm exec vue-tsc --noEmit`
Expected: ALL PASS.

```bash
git add src/stores/runtime.ts src/views/Usage.vue
git commit -m "feat(statistics): display token/request stats on the Usage page"
```

---

## Verification (whole plan)

After Task 10, run the complete gate:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
pnpm exec vue-tsc --noEmit
```

Both green = plan complete. Manual: `pnpm tauri dev` → send one streaming + one non-streaming request → Usage page shows tokens/requests with hover breakdown → wait for a window rollover (or fake it by editing the vendor's reset time) → counters reset, lifetime intact.
