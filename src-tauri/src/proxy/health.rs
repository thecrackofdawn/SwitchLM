use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 本地时间部件（时区转换仅在 `Clock::now_local` 一处完成）。1=周一..=7=周日；minute 0..=1439。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalNow {
    pub weekday: u8,
    pub minute: u16,
}

/// Injectable wall-clock (epoch seconds) so breaker logic is deterministic in tests.
/// `HealthRegistry` methods take `now` as a parameter; `Clock` is consumed by `dispatch`
/// (Task 5) to obtain `now` at request time.
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> i64;
    fn now_local(&self) -> LocalNow;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
    fn now_local(&self) -> LocalNow {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        LocalNow {
            weekday: now.weekday().number_from_monday() as u8, // Mon=1..Sun=7
            minute: now.hour() as u16 * 60 + now.minute() as u16,
        }
    }
}

pub struct FakeClock {
    inner: Mutex<i64>,
    local: Mutex<LocalNow>,
}
impl FakeClock {
    pub fn new(secs: i64) -> Self {
        Self { inner: Mutex::new(secs), local: Mutex::new(LocalNow { weekday: 1, minute: 0 }) }
    }
    pub fn advance(&self, secs: i64) {
        *self.inner.lock().unwrap() += secs;
    }
    pub fn set_local(&self, weekday: u8, minute: u16) {
        *self.local.lock().unwrap() = LocalNow { weekday, minute };
    }
}
impl Clock for FakeClock {
    fn now_secs(&self) -> i64 {
        *self.inner.lock().unwrap()
    }
    fn now_local(&self) -> LocalNow {
        *self.local.lock().unwrap()
    }
}

/// Why a circuit breaker tripped. Logging-only: `#[serde(skip)]` on `ModelHealth` keeps it off the
/// wire (the UI does not need it). `compute_recover_at` decides the variant at trip time; the
/// cooling-skip log reads it back on a later request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TripReason {
    /// Quota still available → a transient 429; retry after `cooldown_seconds`.
    #[default]
    Transient,
    /// A quota window reached 100% → wait for the package reset time.
    Exhausted,
}

/// Per-model circuit-breaker state. Runtime-only (not persisted); restart resets all
/// models to healthy. `cooling_down` means the model recently returned a rate-limit and
/// should be bypassed (routed to its fallback) until `recover_at`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ModelHealth {
    pub cooling_down: bool,
    pub recover_at: Option<i64>,
    pub tripped_at: Option<i64>,
    /// Why the breaker tripped. Logging-only; never serialized to the UI.
    #[serde(skip)]
    pub trip_reason: TripReason,
}

/// In-memory registry of per-model breaker state. Guarded by a `Mutex` (cheap, held briefly).
#[derive(Default)]
pub struct HealthRegistry {
    inner: Mutex<HashMap<String, ModelHealth>>,
}

impl HealthRegistry {
    pub fn get(&self, model_id: &str) -> ModelHealth {
        self.inner
            .lock()
            .unwrap()
            .get(model_id)
            .cloned()
            .unwrap_or_default()
    }

    /// `true` if the model is cooling and `now` is before `recover_at`.
    /// A cooling model with no `recover_at` (should not happen - dispatch always trips with
    /// one) is treated as cooling indefinitely (safe: prefer over-cooling a 429'd model).
    pub fn is_cooling(&self, model_id: &str, now: i64) -> bool {
        let h = self.get(model_id);
        h.cooling_down && h.recover_at.map_or(true, |r| now < r)
    }

    /// If the model is cooling and `now >= recover_at`, clear its state and return `true`.
    /// A cooling model with no `recover_at` cannot auto-recover here (returns false) -
    /// dispatch guarantees `recover_at` is always set on trip.
    pub fn recover_if_due(&self, model_id: &str, now: i64) -> bool {
        let mut map = self.inner.lock().unwrap();
        if let Some(h) = map.get(model_id) {
            if h.cooling_down && h.recover_at.map_or(false, |r| r <= now) {
                let h = map.get_mut(model_id).unwrap();
                h.cooling_down = false;
                h.recover_at = None;
                h.tripped_at = None;
                h.trip_reason = TripReason::default();
                return true;
            }
        }
        false
    }

    /// Mark `model_id` as cooling until `recover_at` (epoch secs). `now` records `tripped_at`.
    /// `reason` records whether this was a real quota exhaustion or a transient throttle, so a later
    /// cooling-skip log can explain itself.
    pub fn trip(&self, model_id: &str, recover_at: Option<i64>, now: i64, reason: TripReason) {
        self.inner.lock().unwrap().insert(
            model_id.into(),
            ModelHealth { cooling_down: true, recover_at, tripped_at: Some(now), trip_reason: reason },
        );
    }

    /// Clear a model's state (e.g. on config edit). Absent -> no-op.
    pub fn reset(&self, model_id: &str) {
        self.inner.lock().unwrap().remove(model_id);
    }

    /// Snapshot all state (for the `get_model_health` management command / UI).
    pub fn snapshot(&self) -> HashMap<String, ModelHealth> {
        self.inner.lock().unwrap().clone()
    }

    /// Like `snapshot()`, but first clears every model whose `recover_at` has passed, so the
    /// cooling indicators the UI/tray read reflect the current wall-clock instead of the moment
    /// a breaker tripped. In-memory recovery is otherwise only triggered on the dispatch path,
    /// so a model that tripped and then expired without further traffic would otherwise show
    /// cooling forever (even after its quota window reset). A model with `cooling_down` but no
    /// `recover_at` stays cooling — matches `is_cooling`'s indefinite-cooling safety.
    pub fn snapshot_recovered(&self, now: i64) -> HashMap<String, ModelHealth> {
        let mut map = self.inner.lock().unwrap();
        for h in map.values_mut() {
            if h.cooling_down && h.recover_at.map_or(false, |r| r <= now) {
                h.cooling_down = false;
                h.recover_at = None;
                h.tripped_at = None;
                h.trip_reason = TripReason::default();
            }
        }
        map.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_model_is_healthy() {
        let reg = HealthRegistry::default();
        assert!(!reg.is_cooling("m1", 1000));
        assert_eq!(reg.get("m1"), ModelHealth::default());
    }

    #[test]
    fn trip_then_cooling_until_recover_at() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Transient);
        assert!(reg.is_cooling("m1", 1500)); // before recover_at
        let h = reg.get("m1");
        assert!(h.cooling_down);
        assert_eq!(h.recover_at, Some(2000));
        assert_eq!(h.tripped_at, Some(1000));
    }

    #[test]
    fn recover_if_due_clears_cooling() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Transient);
        assert!(!reg.recover_if_due("m1", 1999)); // not yet
        assert!(reg.is_cooling("m1", 1999));
        assert!(reg.recover_if_due("m1", 2000)); // at/after recover_at
        assert!(!reg.is_cooling("m1", 2000));
        assert_eq!(reg.get("m1"), ModelHealth::default());
    }

    #[test]
    fn reset_returns_to_healthy() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Transient);
        reg.reset("m1");
        assert!(!reg.is_cooling("m1", 1000));
    }

    #[test]
    fn snapshot_reflects_tripped() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Transient);
        reg.trip("m2", Some(3000), 1000, TripReason::Transient);
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap["m1"].cooling_down);
        assert_eq!(snap["m2"].recover_at, Some(3000));
    }

    #[test]
    fn snapshot_recovered_clears_expired() {
        let reg = HealthRegistry::default();
        reg.trip("expired", Some(2000), 1000, TripReason::Transient); // recover_at in the past
        reg.trip("still", Some(4000), 1000, TripReason::Transient); // recover_at in the future
        reg.trip("noref", None, 1000, TripReason::Transient); // no recover_at -> indefinite cooling

        let snap = reg.snapshot_recovered(3000);
        assert!(!snap["expired"].cooling_down); // 3000 >= 2000 -> cleared
        assert_eq!(snap["expired"].recover_at, None);
        assert!(snap["still"].cooling_down); // 3000 < 4000 -> still cooling
        assert!(snap["noref"].cooling_down); // no recover_at -> indefinite
    }

    #[test]
    fn snapshot_recovered_mutates_registry() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Transient);
        let _ = reg.snapshot_recovered(2000); // at recover_at -> recovers
        assert!(!reg.is_cooling("m1", 1000)); // state was cleared, not just masked
        assert_eq!(reg.get("m1"), ModelHealth::default());
    }

    #[test]
    fn trip_reason_defaults_to_transient() {
        assert_eq!(ModelHealth::default().trip_reason, TripReason::Transient);
    }

    #[test]
    fn trip_stores_reason_and_get_reads_it() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Exhausted);
        assert_eq!(reg.get("m1").trip_reason, TripReason::Exhausted);
        reg.trip("m2", Some(2000), 1000, TripReason::Transient);
        assert_eq!(reg.get("m2").trip_reason, TripReason::Transient);
    }

    #[test]
    fn recover_if_due_resets_reason_to_default() {
        let reg = HealthRegistry::default();
        reg.trip("m1", Some(2000), 1000, TripReason::Exhausted);
        assert!(reg.recover_if_due("m1", 2000));
        assert_eq!(reg.get("m1").trip_reason, TripReason::Transient); // cleared back to default
    }

    #[test]
    fn fake_clock_now_local_is_settable() {
        let clock = FakeClock::new(1000);
        assert_eq!(clock.now_secs(), 1000);
        clock.set_local(3, 1350); // Wednesday 22:30
        let ln = clock.now_local();
        assert_eq!(ln.weekday, 3);
        assert_eq!(ln.minute, 1350);
    }
}
