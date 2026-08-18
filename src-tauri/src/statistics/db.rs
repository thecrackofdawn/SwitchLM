use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use super::{ProviderStats, WindowStats, TokenUsage};
use crate::usage::UsageSnapshot;

/// Open (creating if needed) `switchlm.db` (the app-wide general-purpose
/// database; usage statistics is currently its only tenant) in `dir`.
/// WAL + busy_timeout per spec §Performance. Bundled SQLite → no system dependency.
pub(crate) fn open(dir: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(dir.join("switchlm.db"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    create_schema(&conn)?;
    Ok(conn)
}

/// No FK to `providers` (config lives in app_config.json, not this file);
/// provider deletion is handled via StatEvent::DeleteProvider (spec §Database Schema).
pub(crate) fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{UsageSnapshot, UsageTier};
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
        let found = all.drain(..).find(|s| s.provider_id == provider_id);
        found.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)
    }

    // ---- Task 2: reset-from-usage mechanism ----

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
        // Same cycle → nothing zeroed. 1, not 2: the first record ran before the
        // window was armed (Task 1 semantics — unarmed windows guard all columns).
        assert_eq!(s.last_5h.requests, 1);
        assert_eq!(s.last_5h.input_tokens, 3);
        assert_eq!(s.last_5h.output_tokens, 4);
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
}
