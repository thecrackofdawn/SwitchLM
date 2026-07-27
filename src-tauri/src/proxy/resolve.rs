use crate::config::{AppConfig, Model};
use crate::proxy::health::LocalNow;
use crate::proxy::strategies::profile_start_model;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("model '{requested}' is not a configured profile; available: {available:?}")]
    ProfileNotFound { requested: String, available: Vec<String> },
    #[error("profile points to missing model '{0}'")]
    MissingBackingModel(String),
}

pub fn resolve_model<'c>(
    cfg: &'c AppConfig,
    requested: Option<&str>,
    now: &LocalNow,
) -> Result<&'c Model, ResolveError> {
    let available: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
    let req = requested.unwrap_or("");
    let prof = cfg
        .profiles
        .iter()
        .find(|p| p.name == req || p.aliases.iter().any(|a| a == req))
        .ok_or_else(|| ResolveError::ProfileNotFound { requested: req.to_string(), available })?;
    let (model_id, via_strategy) = profile_start_model(prof, cfg, now);
    if let Some(sid) = via_strategy {
        tracing::info!(
            target: "switchlm::proxy",
            profile = %prof.name,
            strategy = %sid,
            "time-strategy matched"
        );
    }
    let model = cfg
        .models
        .iter()
        .find(|m| m.id == model_id)
        .ok_or_else(|| ResolveError::MissingBackingModel(model_id.to_string()))?;
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::proxy::health::LocalNow;

    fn sample() -> AppConfig {
        AppConfig {
            models: vec![Model {
                id: "m_glm46".into(),
                provider_id: "zhipu".into(),
                source: ModelSource::Manual,
                upstream_model_id: "glm-4.6".into(),
                cooldown_seconds: None,
                fallback_target_model_id: None,
                ..Default::default()
            }],
            profiles: vec![Profile {
                id: "p_main".into(),
                name: "glm-5.2".into(),
                aliases: vec!["claude-sonnet-4".into()],
                backing_model_id: "m_glm46".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn hits_by_name() {
        let cfg = sample();
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 1, minute: 0 }).unwrap();
        assert_eq!(m.id, "m_glm46");
    }

    #[test]
    fn hits_by_alias() {
        let cfg = sample();
        let m = resolve_model(&cfg, Some("claude-sonnet-4"), &LocalNow { weekday: 1, minute: 0 }).unwrap();
        assert_eq!(m.id, "m_glm46");
    }

    #[test]
    fn miss_lists_available() {
        let cfg = sample();
        let err = resolve_model(&cfg, Some("nope"), &LocalNow { weekday: 1, minute: 0 }).unwrap_err();
        match err {
            ResolveError::ProfileNotFound { available, .. } => {
                assert!(available.contains(&"glm-5.2".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn missing_model_name_errors() {
        let cfg = sample();
        assert!(matches!(
            resolve_model(&cfg, None, &LocalNow { weekday: 1, minute: 0 }),
            Err(ResolveError::ProfileNotFound { .. })
        ));
    }

    use crate::config::{Strategy, StrategyKind, TimeStrategy};

    fn cfg_with_strategy() -> AppConfig {
        let mut cfg = sample(); // profile "glm-5.2" -> m_glm46 (backing)
        cfg.models.push(Model {
            id: "m_strat".into(), provider_id: "zhipu".into(),
            source: ModelSource::Manual, upstream_model_id: "glm-air".into(),
            cooldown_seconds: None, fallback_target_model_id: None,
            ..Default::default()
        });
        cfg.profiles[0].strategies_enabled = true;
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_strat".into(),
            }),
        }];
        cfg
    }

    #[test]
    fn resolve_uses_strategy_model_when_matched() {
        let cfg = cfg_with_strategy();
        // Tuesday within [0,1439) → strategy model
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 2, minute: 500 }).unwrap();
        assert_eq!(m.id, "m_strat");
    }

    #[test]
    fn resolve_falls_to_backing_when_no_match() {
        let cfg = cfg_with_strategy();
        // Wednesday not in [2] → backing
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 3, minute: 500 }).unwrap();
        assert_eq!(m.id, "m_glm46");
    }

    #[test]
    fn resolve_uses_backing_when_master_off() {
        let mut cfg = cfg_with_strategy();
        cfg.profiles[0].strategies_enabled = false;
        let m = resolve_model(&cfg, Some("glm-5.2"), &LocalNow { weekday: 2, minute: 500 }).unwrap();
        assert_eq!(m.id, "m_glm46");
    }
}
