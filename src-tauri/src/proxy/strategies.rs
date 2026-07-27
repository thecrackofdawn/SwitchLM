use crate::config::{AppConfig, Model, Profile, Strategy, StrategyKind, TimeStrategy};
use crate::proxy::health::LocalNow;

/// Among enabled strategies matching `now`, pick the winner: min `priority`;
/// ties broken by lowest list index. `None` if none match. Explicit tie-break
/// (do not rely on `min_by_key`'s tie semantics).
pub fn select_strategy<'a>(strategies: &'a [Strategy], now: &LocalNow) -> Option<&'a Strategy> {
    let mut best: Option<(&Strategy, usize)> = None;
    for (i, s) in strategies.iter().enumerate() {
        if !s.enabled { continue; }
        let matches = match &s.kind {
            StrategyKind::Time(t) => strategy_matches(t, now.weekday, now.minute),
        };
        if !matches { continue; }
        match best {
            None => best = Some((s, i)),
            Some((bs, bi)) => if (s.priority, i) < (bs.priority, bi) { best = Some((s, i)); },
        }
    }
    best.map(|(s, _)| s)
}

/// Single source of truth for "which model does this profile start at, and why".
/// Master off / winner model deleted → backing_model_id (via = None).
pub fn profile_start_model<'c>(
    profile: &'c Profile,
    cfg: &'c AppConfig,
    now: &LocalNow,
) -> (&'c str, Option<&'c str>) {
    if profile.strategies_enabled {
        if let Some(s) = select_strategy(&profile.strategies, now) {
            let mid: &str = match &s.kind { StrategyKind::Time(t) => &t.model_id };
            if cfg.models.iter().any(|m| m.id == mid) {
                return (mid, Some(&s.id));
            }
        }
    }
    (&profile.backing_model_id, None)
}

/// Single source of truth for "which model does this model fail over to, and why".
/// Master off / no match / strategy model deleted → `fallback_target_model_id` (via = None);
/// no default target → (None, None). Mirror of `profile_start_model` for the failover layer.
pub fn model_fallback_target<'c>(
    model: &'c Model,
    cfg: &'c AppConfig,
    now: &LocalNow,
) -> (Option<&'c str>, Option<&'c str>) {
    if model.fallback_strategies_enabled {
        if let Some(s) = select_strategy(&model.fallback_strategies, now) {
            let mid: &str = match &s.kind { StrategyKind::Time(t) => &t.model_id };
            if cfg.models.iter().any(|m| m.id == mid) {
                return (Some(mid), Some(&s.id));
            }
        }
    }
    (model.fallback_target_model_id.as_deref(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(days: &[u8], start: u16, end: u16) -> TimeStrategy {
        TimeStrategy { days_of_week: days.to_vec(), time_start: start, time_end: end, model_id: "m".into() }
    }

    // ---- prev_weekday (周一↔周日 wrap) ----
    #[test]
    fn prev_weekday_wraps_monday_to_sunday() {
        assert_eq!(prev_weekday(1), 7); // Mon ← Sun
        assert_eq!(prev_weekday(2), 1);
        assert_eq!(prev_weekday(7), 6);
    }

    // ---- same-day window [start, end): include start, exclude end ----
    #[test]
    fn same_day_window_half_open() {
        let s = ts(&[2], 840, 1080); // Tue 14:00–18:00
        assert!(strategy_matches(&s, 2, 840));  // 14:00 include
        assert!(strategy_matches(&s, 2, 1079)); // 17:59 include
        assert!(!strategy_matches(&s, 2, 1080)); // 18:00 exclude
        assert!(!strategy_matches(&s, 2, 839));  // 13:59 before
        assert!(!strategy_matches(&s, 3, 900));  // wrong day
    }

    // ---- cross-midnight: evening looks at today, morning looks at YESTERDAY ----
    #[test]
    fn cross_midnight_evening_uses_today() {
        let s = ts(&[2], 1320, 480); // start Tue 22:00 → Wed 08:00
        assert!(strategy_matches(&s, 2, 1320));  // Tue 22:00 (today=Tue ✓)
        assert!(strategy_matches(&s, 2, 1439));  // Tue 23:59
        assert!(!strategy_matches(&s, 3, 1320)); // Wed 22:00? today=Wed not in [2]
    }

    #[test]
    fn cross_midnight_morning_uses_yesterday() {
        let s = ts(&[2], 1320, 480); // start Tue 22:00 → Wed 08:00
        assert!(strategy_matches(&s, 3, 0));    // Wed 00:00: yesterday=Tue ✓
        assert!(strategy_matches(&s, 3, 479));  // Wed 07:59 (< 480)
        assert!(!strategy_matches(&s, 3, 480)); // Wed 08:00 exclude
        assert!(!strategy_matches(&s, 4, 100)); // Thu 01:00: yesterday=Wed not in [2]
    }

    #[test]
    fn cross_midnight_morning_monday_looks_at_sunday() {
        let s = ts(&[7], 1320, 480); // start Sun 22:00 → Mon 08:00
        assert!(strategy_matches(&s, 1, 100));  // Mon 01:00: yesterday=Sun ✓
    }

    #[test]
    fn days_contains_membership() {
        assert!(days_contains(&[1, 3, 5], 3));
        assert!(!days_contains(&[1, 3, 5], 2));
        assert!(!days_contains(&[], 1));
    }

    // ---- 0:00–0:00 = 全天 (matches every minute of selected weekdays) ----
    #[test]
    fn full_day_zero_zero_matches_all_minutes() {
        let s = ts(&[6, 7], 0, 0); // 全天 周六/周日
        assert!(strategy_matches(&s, 6, 0));    // Sat 00:00
        assert!(strategy_matches(&s, 6, 500));  // Sat 08:20
        assert!(strategy_matches(&s, 6, 1439)); // Sat 23:59
        assert!(strategy_matches(&s, 7, 1439)); // Sun 23:59
        assert!(!strategy_matches(&s, 1, 500)); // Mon not in set
    }

    // ---- select_strategy ----
    use crate::config::{AppConfig, Profile, Model, ModelSource};
    use crate::config::StrategyKind;

    fn strat(id: &str, priority: u8, enabled: bool, days: &[u8], start: u16, end: u16, model: &str) -> Strategy {
        Strategy {
            id: id.into(), priority, enabled,
            kind: StrategyKind::Time(TimeStrategy { days_of_week: days.to_vec(), time_start: start, time_end: end, model_id: model.into() }),
        }
    }
    fn now(w: u8, m: u16) -> LocalNow { LocalNow { weekday: w, minute: m } }

    #[test]
    fn select_strategy_min_priority_wins() {
        let s = vec![
            strat("a", 5, true, &[2], 0, 1439, "ma"),   // matches Tue
            strat("b", 2, true, &[2], 0, 1439, "mb"),   // matches Tue, higher prio
        ];
        let win = select_strategy(&s, &now(2, 500)).unwrap();
        assert_eq!(win.id, "b");
    }

    #[test]
    fn select_strategy_tie_breaks_by_list_order() {
        let s = vec![
            strat("a", 3, true, &[2], 0, 1439, "ma"),
            strat("b", 3, true, &[2], 0, 1439, "mb"),  // same prio, later
        ];
        assert_eq!(select_strategy(&s, &now(2, 500)).unwrap().id, "a");
    }

    #[test]
    fn select_strategy_skips_disabled_and_non_matching() {
        let s = vec![
            strat("a", 1, false, &[2], 0, 1439, "ma"),  // disabled
            strat("b", 1, true, &[3], 0, 1439, "mb"),   // wrong day
        ];
        assert!(select_strategy(&s, &now(2, 500)).is_none());
    }

    fn cfg_with(profile: Profile) -> AppConfig {
        let mut c = AppConfig::default();
        c.profiles.push(profile);
        c
    }

    fn mk_model(id: &str, provider_id: &str, upstream_id: &str) -> crate::config::Model {
        crate::config::Model {
            id: id.into(),
            provider_id: provider_id.into(),
            source: ModelSource::Manual,
            upstream_model_id: upstream_id.into(),
            cooldown_seconds: None,
            fallback_target_model_id: None,
            ..Default::default()
        }
    }

    #[test]
    fn profile_start_model_uses_strategy_when_matched() {
        let mut p = Profile { id: "p".into(), name: "n".into(), aliases: vec![], backing_model_id: "mback".into(), ..Default::default() };
        p.strategies_enabled = true;
        p.strategies = vec![strat("s", 1, true, &[2], 0, 1439, "mstrat")];
        let mut cfg = cfg_with(p);
        cfg.models.push(mk_model("mstrat", "pr", "u"));
        cfg.models.push(mk_model("mback", "pr", "ub"));
        let (mid, via) = profile_start_model(&cfg.profiles[0], &cfg, &now(2, 500));
        assert_eq!(mid, "mstrat");
        assert_eq!(via, Some("s"));
    }

    #[test]
    fn profile_start_model_master_off_ignores_strategies() {
        let mut p = Profile { id: "p".into(), name: "n".into(), aliases: vec![], backing_model_id: "mback".into(), ..Default::default() };
        p.strategies_enabled = false;
        p.strategies = vec![strat("s", 1, true, &[2], 0, 1439, "mstrat")];
        let cfg = cfg_with(p);
        let (mid, via) = profile_start_model(&cfg.profiles[0], &cfg, &now(2, 500));
        assert_eq!(mid, "mback");
        assert_eq!(via, None);
    }

    #[test]
    fn profile_start_model_falls_back_when_strategy_model_deleted() {
        let mut p = Profile { id: "p".into(), name: "n".into(), aliases: vec![], backing_model_id: "mback".into(), ..Default::default() };
        p.strategies_enabled = true;
        p.strategies = vec![strat("s", 1, true, &[2], 0, 1439, "mgone")]; // no model "mgone"
        let mut cfg = cfg_with(p);
        cfg.models.push(mk_model("mback", "pr", "ub"));
        let (mid, via) = profile_start_model(&cfg.profiles[0], &cfg, &now(2, 500));
        assert_eq!(mid, "mback");
        assert_eq!(via, None);
    }

    fn fb_model(id: &str, target: Option<&str>, strats: Vec<Strategy>, enabled: bool) -> Model {
        Model {
            id: id.into(), provider_id: "pr".into(), source: ModelSource::Manual,
            upstream_model_id: id.into(), cooldown_seconds: None,
            fallback_target_model_id: target.map(String::from),
            fallback_strategies: strats, fallback_strategies_enabled: enabled,
            ..Default::default()
        }
    }
    fn fb_strat(id: &str, days: &[u8], start: u16, end: u16, model: &str) -> Strategy {
        Strategy {
            id: id.into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: days.to_vec(), time_start: start, time_end: end, model_id: model.into(),
            }),
        }
    }

    #[test]
    fn model_fallback_target_strategy_hit() {
        // Strategy matches Tue all-day -> mB; default mC.
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mB", "pr", "uB"));
        cfg.models.push(mk_model("mC", "pr", "uC"));
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[2], 0, 1439, "mB")], true);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, Some("mB"));
        assert_eq!(via, Some("s"));
    }

    #[test]
    fn model_fallback_target_master_off_uses_default() {
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mB", "pr", "uB"));
        cfg.models.push(mk_model("mC", "pr", "uC"));
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[2], 0, 1439, "mB")], false);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, Some("mC"));
        assert_eq!(via, None);
    }

    #[test]
    fn model_fallback_target_deleted_strategy_target_falls_to_default() {
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mC", "pr", "uC")); // mB does not exist
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[2], 0, 1439, "mB")], true);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, Some("mC"));
        assert_eq!(via, None);
    }

    #[test]
    fn model_fallback_target_no_default_no_hit_is_none() {
        let cfg = cfg_with(Profile::default());
        let m = fb_model("mA", None, vec![], true);
        let (t, via) = model_fallback_target(&m, &cfg, &now(2, 500));
        assert_eq!(t, None);
        assert_eq!(via, None);
    }

    #[test]
    fn model_fallback_target_boundary_22_to_06() {
        // Window 22:00(1320)–06:00(360) -> mB, all weekdays; default mC.
        let mut cfg = cfg_with(Profile::default());
        cfg.models.push(mk_model("mB", "pr", "uB"));
        cfg.models.push(mk_model("mC", "pr", "uC"));
        let m = fb_model("mA", Some("mC"), vec![fb_strat("s", &[1,2,3,4,5,6,7], 1320, 360, "mB")], true);
        // 21:59(1319) -> default mC; 22:00(1320) -> mB (evening, today in set)
        assert_eq!(model_fallback_target(&m, &cfg, &now(2, 1319)), (Some("mC"), None));
        assert_eq!(model_fallback_target(&m, &cfg, &now(2, 1320)), (Some("mB"), Some("s")));
        // 05:59(359) -> mB (morning, prev_weekday in set since all days); 06:00(360) -> default mC
        assert_eq!(model_fallback_target(&m, &cfg, &now(3, 359)), (Some("mB"), Some("s")));
        assert_eq!(model_fallback_target(&m, &cfg, &now(3, 360)), (Some("mC"), None));
    }
}

/// `weekday` (1=Mon..=7=Sun) is in the `days` set.
pub fn days_contains(days: &[u8], weekday: u8) -> bool {
    days.iter().any(|d| *d == weekday)
}

/// Previous day, wrapping Monday→Sunday. 1→7, else n-1.
pub fn prev_weekday(weekday: u8) -> u8 {
    if weekday <= 1 { 7 } else { weekday - 1 }
}

/// Does this time strategy match `(weekday, minute)`?
///  - `start == end` (仅 0:00–0:00 通过校验): 全天，命中选中星期每一分钟。
///  - `start < end` (same-day): `days_contains(weekday) && minute ∈ [start, end)`
///  - `start > end` (cross-midnight; 其它相等组合已在校验层拒绝):
///      `minute >= start` (evening) ⇒ `days_contains(weekday)`
///      `minute < end` (morning)  ⇒ `days_contains(prev_weekday(weekday))` (window started yesterday)
pub fn strategy_matches(s: &TimeStrategy, weekday: u8, minute: u16) -> bool {
    if s.time_start == s.time_end {
        // 0:00–0:00 = 全天（24h，命中选中星期的每一分钟）。
        days_contains(&s.days_of_week, weekday)
    } else if s.time_start < s.time_end {
        days_contains(&s.days_of_week, weekday) && minute >= s.time_start && minute < s.time_end
    } else if minute >= s.time_start {
        days_contains(&s.days_of_week, weekday)
    } else {
        // Morning portion of cross-midnight window: exclude the exact end minute
        minute < s.time_end && days_contains(&s.days_of_week, prev_weekday(weekday))
    }
}
