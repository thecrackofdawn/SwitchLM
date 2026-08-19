pub mod db;
pub mod extract;

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::proxy::health::Clock;
use crate::usage::UsageSnapshot;

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

    /// Clone of the writer-thread channel sender — lets streaming paths build
    /// a `RecordOnce` whose token values are filled in when the stream ends
    /// (Task 7: the sink outlives the async dispatch frame).
    pub fn sender(&self) -> std::sync::mpsc::Sender<StatEvent> {
        self.tx.clone()
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
            let stats = db::query_all(conn, now)?;
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
    /// Update the token halves of a held `Record` event (streaming paths learn
    /// the real usage only when the stream ends — Task 7). No-op on other
    /// event kinds or after `send`.
    pub fn set_tokens(&mut self, tokens: TokenUsage) {
        if let Some(StatEvent::Record { tokens: t, .. }) = self.evt.as_mut() {
            *t = tokens;
        }
    }
    /// Disarm without sending: the attempt did not serve a 2xx stream (rate-
    /// limited, transport error, non-2xx passthrough) so nothing may be
    /// recorded. Drop after this is a no-op (Task 7).
    pub fn cancel(&mut self) {
        self.evt = None;
    }
}

impl Drop for RecordOnce {
    fn drop(&mut self) {
        self.send();
    }
}

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
