use std::path::{Path, PathBuf};

use crate::config::{AppConfig, SecretStore};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("app_config.json")
}

pub fn load(dir: &Path) -> Result<AppConfig, StoreError> {
    let path = config_path(dir);
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let text = std::fs::read_to_string(path)?;
    let cfg: AppConfig = serde_json::from_str(&text)?;
    Ok(cfg)
}

pub fn save(dir: &Path, cfg: &AppConfig) -> Result<(), StoreError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    let path = config_path(dir);
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// One-time migration: move any plaintext `usage_creds.secret_access_key` from `app_config.json`
/// into the OS keyring, then rewrite the config without the field. Idempotent (no-op when no SK
/// is present). Runs before `load` so the `AppState` never holds a plaintext SK.
///
/// The SK is read at the raw-JSON level because `UsageCreds.secret_access_key` is `#[serde(skip)]`
/// (never deserialized into the struct) - this is the only place that consumes legacy plaintext SKs.
pub fn migrate_usage_sk_to_keyring(dir: &Path, secrets: &dyn SecretStore) -> Result<(), StoreError> {
    let path = config_path(dir);
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(()), // malformed config: let `load` surface the real error
    };
    let Some(providers) = root.get_mut("providers").and_then(|v| v.as_array_mut()) else {
        return Ok(());
    };
    let mut changed = false;
    for p in providers.iter_mut() {
        let id = p
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let Some(creds) = p.get_mut("usage_creds").and_then(|c| c.as_object_mut()) else {
            continue;
        };
        if let Some(sk_val) = creds.remove("secret_access_key") {
            // Removing the field is a config cleanup whether or not it held a real SK.
            changed = true;
            if let Some(sk) = sk_val.as_str().filter(|s| !s.is_empty()) {
                if let Err(e) = secrets.set_usage_sk(&id, sk) {
                    tracing::warn!("usage_sk migration: keyring set failed for {id}: {e}");
                }
            }
        }
    }
    if changed {
        let cleaned = serde_json::to_string_pretty(&root)?;
        std::fs::write(path, cleaned)?;
    }
    Ok(())
}

/// Fill `vendor` for legacy providers that lack it. Legacy configs used the vendor slug as the
/// provider `id`, so `vendor = id` is exactly correct and touches nothing else (no keyring, no FK).
/// Idempotent: once `vendor` is non-empty it is left alone. Returns whether any provider changed.
pub fn normalize_legacy_vendors(cfg: &mut crate::config::AppConfig) -> bool {
    let mut changed = false;
    for p in &mut cfg.providers {
        if p.vendor.is_empty() {
            p.vendor = p.id.clone();
            changed = true;
        } else if p.vendor == "bailian-token" {
            // Vendor slug rename (阿里云百炼 → 千问): remap legacy configs so an existing provider
            // keeps working. Provider ids + keyring entries are keyed by opaque id, not vendor.
            p.vendor = "qianwen-token".into();
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MemoryStore, Provider, SecretStore, Settings};
    use tempfile::tempdir;

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempdir().unwrap();
        let cfg = AppConfig {
            settings: Settings { port: 7000, autostart: true, usage_refresh_interval_secs: 60, log_level: "info".into(), secret_store_fallback: None },
            ..AppConfig::default()
        };
        save(dir.path(), &cfg).unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = tempdir().unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(back, AppConfig::default()); // default port 6950
    }

    #[test]
    fn migrate_usage_sk_moves_plaintext_sk_to_keyring() {
        let dir = tempdir().unwrap();
        // Legacy config with a plaintext SK sitting in usage_creds.
        let json = r#"{
            "providers": [
                { "id": "volcengine-coding", "display_name": "火山", "usage_creds": { "access_key_id": "AK", "secret_access_key": "plaintext-sk" } },
                { "id": "zhipu", "display_name": "智谱" }
            ]
        }"#;
        std::fs::write(dir.path().join("app_config.json"), json).unwrap();

        let secrets = MemoryStore::default();
        migrate_usage_sk_to_keyring(dir.path(), &secrets).unwrap();

        // SK now lives in the (memory) keyring, keyed by provider id.
        assert_eq!(secrets.get_usage_sk("volcengine-coding").unwrap(), Some("plaintext-sk".into()));
        assert_eq!(secrets.get_usage_sk("zhipu").unwrap(), None);

        // The plaintext SK is purged from the config file; the AK (identifier) is preserved.
        let after = std::fs::read_to_string(dir.path().join("app_config.json")).unwrap();
        assert!(!after.contains("secret_access_key"), "plaintext SK should be purged: {after}");
        assert!(after.contains("AK"), "access_key_id should remain: {after}");

        // Idempotent: re-running does not duplicate or lose the SK.
        migrate_usage_sk_to_keyring(dir.path(), &secrets).unwrap();
        assert_eq!(secrets.get_usage_sk("volcengine-coding").unwrap(), Some("plaintext-sk".into()));
    }

    #[test]
    fn normalize_legacy_vendors_fills_empty_from_id_and_is_idempotent() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "zhipu".into(), vendor: String::new(), display_name: "智谱".into(),
            openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        });
        cfg.providers.push(Provider {
            id: "volcengine-coding".into(), vendor: "volcengine-coding".into(),
            display_name: "火山".into(), openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        });
        // First pass: fills the empty one -> changed.
        assert!(normalize_legacy_vendors(&mut cfg));
        assert_eq!(cfg.providers[0].vendor, "zhipu");
        assert_eq!(cfg.providers[1].vendor, "volcengine-coding"); // already set, untouched
        // Second pass: nothing empty -> unchanged.
        assert!(!normalize_legacy_vendors(&mut cfg));
    }

    #[test]
    fn normalize_legacy_vendors_remaps_bailian_token_slug() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(Provider {
            id: "p1".into(), vendor: "bailian-token".into(), display_name: "千问".into(),
            openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        });
        cfg.providers.push(Provider {
            id: "p2".into(), vendor: "qianwen-token".into(), display_name: "千问 2".into(),
            openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        });
        assert!(normalize_legacy_vendors(&mut cfg));
        assert_eq!(cfg.providers[0].vendor, "qianwen-token"); // legacy slug remapped
        assert_eq!(cfg.providers[1].vendor, "qianwen-token"); // already new slug, untouched
        assert!(!normalize_legacy_vendors(&mut cfg)); // idempotent
    }
}
