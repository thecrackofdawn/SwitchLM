use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::store::StoreError;

/// Bundled default provider/model description catalog (compiled into the binary). Organized by
/// provider so future provider-level attributes (e.g. per-plan concurrency caps) can be added
/// without reshaping the leaf model entries.
pub const EMBEDDED_CATALOG: &str = include_str!("../../assets/provider_desc.json");

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderCatalog {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub providers: Vec<ProviderDesc>,
}

/// One provider's catalog group. `provider_id` is the vendor slug (it matches `Provider.vendor`,
/// NOT the opaque account `id`). No `deny_unknown_fields`: unknown keys (future provider-level
/// attrs) are ignored gracefully by older builds and can be added without a schema break.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderDesc {
    pub provider_id: String,
    #[serde(default)]
    pub models: Vec<ModelDesc>,
    /// 个人套餐用量页面链接（用于套餐用量界面跳转）
    #[serde(default)]
    pub individual_usage_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelDesc {
    pub upstream_model_id: String,
    pub context_size: u32,
    /// 模型输出上限（如 OpenCode limit.output）。bundled catalog 全量携带；用户自定义条目
    /// 可缺省（None = 未自定义，overlay 时不覆盖 baseline 值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_size: Option<u32>,
}

impl ProviderCatalog {
    fn find_model(&self, vendor: &str, upstream_model_id: &str) -> Option<&ModelDesc> {
        self.providers
            .iter()
            .find(|p| p.provider_id == vendor)
            .and_then(|p| p.models.iter().find(|m| m.upstream_model_id == upstream_model_id))
    }

    /// Catalog-only lookup (ignores any per-Model manual override). None if not bundled.
    /// `vendor` is the provider's vendor slug (the catalog keys groups by `provider_id == vendor`).
    pub fn context_size(&self, vendor: &str, upstream_model_id: &str) -> Option<u32> {
        self.find_model(vendor, upstream_model_id).map(|m| m.context_size)
    }

    /// Output-size lookup (same keying as `context_size`). None when unknown/not bundled.
    pub fn output_size(&self, vendor: &str, upstream_model_id: &str) -> Option<u32> {
        self.find_model(vendor, upstream_model_id).and_then(|m| m.output_size)
    }

    /// Overlay user customizations onto the embedded baseline. For each (provider, model) in
    /// `custom`: override the context_size if the entry already exists in the baseline, otherwise
    /// add it. `output_size` is only overridden when the custom entry carries one (a sparse
    /// custom entry without it keeps the baseline value). A provider present only in `custom`
    /// (a user-supplemented vendor) is added wholesale. Baseline entries not mentioned in
    /// `custom` are left untouched. `version` is passthrough.
    pub fn overlay_custom(&mut self, custom: &ProviderCatalog) {
        for cp in &custom.providers {
            match self.providers.iter().position(|p| p.provider_id == cp.provider_id) {
                Some(i) => {
                    let group = &mut self.providers[i];
                    for cm in &cp.models {
                        if let Some(existing) = group
                            .models
                            .iter_mut()
                            .find(|m| m.upstream_model_id == cm.upstream_model_id)
                        {
                            existing.context_size = cm.context_size;
                            if cm.output_size.is_some() {
                                existing.output_size = cm.output_size;
                            }
                        } else {
                            group.models.push(cm.clone());
                        }
                    }
                }
                None => self.providers.push(cp.clone()),
            }
        }
    }

    /// Set, override, or remove a single (vendor, upstream_model_id) entry in this catalog. Used
    /// to mutate the **sparse** custom file: `Some(n)` sets/overrides the entry (creating the
    /// provider group if absent), `None` removes it (and drops the provider group if it becomes
    /// empty). Removing a non-existent entry is a no-op.
    pub fn set_context_size(&mut self, vendor: &str, upstream_model_id: &str, size: Option<u32>) {
        let group = self.providers.iter_mut().find(|p| p.provider_id == vendor);
        match size {
            Some(n) => match group {
                Some(g) => {
                    if let Some(existing) = g
                        .models
                        .iter_mut()
                        .find(|m| m.upstream_model_id == upstream_model_id)
                    {
                        existing.context_size = n;
                    } else {
                        g.models.push(ModelDesc {
                            upstream_model_id: upstream_model_id.into(),
                            context_size: n,
                            output_size: None,
                        });
                    }
                }
                None => self.providers.push(ProviderDesc {
                    provider_id: vendor.into(),
                    individual_usage_url: None,
                    models: vec![ModelDesc {
                        upstream_model_id: upstream_model_id.into(),
                        context_size: n,
                        output_size: None,
                    }],
                }),
            },
            None => {
                if let Some(g) = group {
                    g.models.retain(|m| m.upstream_model_id != upstream_model_id);
                    if g.models.is_empty() {
                        self.providers.retain(|p| p.provider_id != vendor);
                    }
                }
            }
        }
    }
}

/// Path of the user-editable custom catalog in the AppData dir. This is the ONLY persisted
/// catalog file; the embedded baseline is never written to disk.
pub fn custom_catalog_path(dir: &Path) -> PathBuf {
    dir.join("custom_provider_desc.json")
}

fn parse_embedded() -> ProviderCatalog {
    serde_json::from_str(EMBEDDED_CATALOG).unwrap_or_else(|e| {
        tracing::error!("内嵌 catalog 解析失败，降级为空：{e}");
        ProviderCatalog::default()
    })
}

/// Write the custom catalog file (used by tests; a future write command would call this too).
pub fn save_custom_catalog(dir: &Path, cat: &ProviderCatalog) -> Result<(), StoreError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(cat)?;
    std::fs::write(custom_catalog_path(dir), text)?;
    Ok(())
}

/// Read the user's custom catalog from disk. Returns an empty catalog if the file is missing or
/// malformed (malformed -> warning; file left untouched for manual repair).
pub fn load_custom(dir: &Path) -> ProviderCatalog {
    let path = custom_catalog_path(dir);
    if !path.exists() {
        return ProviderCatalog::default();
    }
    match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<ProviderCatalog>(&t).ok())
    {
        Some(custom) => custom,
        None => {
            tracing::warn!(
                "custom_provider_desc.json 解析失败，已忽略用户自定义（文件未改动）：{}",
                path.display()
            );
            ProviderCatalog::default()
        }
    }
}

/// Rebuild the effective in-memory catalog: embedded baseline overlaid with `custom`. Used at
/// startup (`ensure_catalog`) and after a write (`set_custom_context_size`) so memory matches
/// disk + embedded exactly.
pub fn effective_catalog(custom: &ProviderCatalog) -> ProviderCatalog {
    let mut cat = parse_embedded();
    cat.overlay_custom(custom);
    cat
}

/// Load the embedded provider catalog as the baseline, then overlay user customizations from
/// `custom_provider_desc.json` if it exists. The baseline is re-read from the binary on every
/// launch and never persisted, so a changed `context_size` in `provider_desc.json` takes effect
/// immediately without wiping any disk file. A malformed custom file is ignored with a warning
/// (the user's file is left untouched). Never fails: on any I/O error it logs and returns the in-memory baseline.
pub fn ensure_catalog(dir: &Path) -> ProviderCatalog {
    effective_catalog(&load_custom(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_custom(dir: &Path, body: &str) {
        std::fs::write(custom_catalog_path(dir), body).unwrap();
    }

    #[test]
    fn embedded_catalog_parses_and_has_sizes() {
        let cat: ProviderCatalog = serde_json::from_str(EMBEDDED_CATALOG).unwrap();
        assert!(cat.version >= 1);
        assert!(!cat.providers.is_empty());
        for p in &cat.providers {
            assert!(!p.models.is_empty(), "provider {} has no models", p.provider_id);
            for m in &p.models {
                assert!(m.context_size > 0, "{:?}/{:?} has zero size", p.provider_id, m.upstream_model_id);
                // 每个 bundled 模型都必须声明 output_size（OpenCode limit.output 依赖它）。
                assert!(m.output_size.unwrap_or(0) > 0, "{:?}/{:?} missing output_size", p.provider_id, m.upstream_model_id);
            }
        }
        // Verify that known providers have individual usage URLs
        let zhipu = cat.providers.iter().find(|p| p.provider_id == "zhipu").unwrap();
        assert_eq!(zhipu.individual_usage_url, Some("https://bigmodel.cn/coding-plan/personal/usage".to_string()));
        let deepseek = cat.providers.iter().find(|p| p.provider_id == "deepseek").unwrap();
        assert_eq!(deepseek.individual_usage_url, Some("https://platform.deepseek.com/usage".to_string()));
        let volcagent = cat.providers.iter().find(|p| p.provider_id == "volcengine-agent").unwrap();
        assert_eq!(volcagent.individual_usage_url, Some("https://console.volcengine.com/ark/region:cn-beijing/subscription/agent-plan".to_string()));
        let volccoding = cat.providers.iter().find(|p| p.provider_id == "volcengine-coding").unwrap();
        assert_eq!(volccoding.individual_usage_url, Some("https://console.volcengine.com/ark/region:cn-beijing/subscription/coding-plan".to_string()));
        let qianwen = cat.providers.iter().find(|p| p.provider_id == "qianwen-token").unwrap();
        assert_eq!(qianwen.individual_usage_url, Some("https://platform.qianwenai.com/home/billing/subscription/token-plan-individual".to_string()));
    }

    #[test]
    fn lookup_finds_bundled_models() {
        let cat: ProviderCatalog = serde_json::from_str(EMBEDDED_CATALOG).unwrap();
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(200000));
        assert_eq!(cat.context_size("deepseek", "deepseek-v4-flash"), Some(1000000));
        assert_eq!(cat.context_size("deepseek", "deepseek-v4-pro"), Some(1000000));
        assert_eq!(cat.context_size("volcengine-agent", "doubao-seed-evolving"), Some(1000000));
        assert_eq!(cat.context_size("volcengine-coding", "glm-5.2"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "qwen3.8-max-preview"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "qwen3.7-max"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "qwen3.7-plus"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "qwen3.6-flash"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "deepseek-v4-pro"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "deepseek-v4-flash-0731"), Some(1000000));
        assert_eq!(cat.context_size("qianwen-token", "glm-5.2"), Some(1000000));
        // output_size lookup on a few bundled entries
        assert_eq!(cat.output_size("zhipu", "glm-4.6"), Some(128000));
        assert_eq!(cat.output_size("deepseek", "deepseek-chat"), Some(384000));
        assert_eq!(cat.output_size("qianwen-token", "glm-5"), Some(16384));
    }

    #[test]
    fn lookup_misses_unknown() {
        let cat: ProviderCatalog = serde_json::from_str(EMBEDDED_CATALOG).unwrap();
        assert_eq!(cat.context_size("zhipu", "nope"), None);
        assert_eq!(cat.context_size("unknown-vendor", "glm-4.6"), None);
        assert_eq!(cat.output_size("zhipu", "nope"), None);
        assert_eq!(cat.output_size("unknown-vendor", "glm-4.6"), None);
    }

    #[test]
    fn overlay_overrides_and_supplements() {
        let mut base = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![
                    ModelDesc { upstream_model_id: "glm-4.6".into(), context_size: 200000, output_size: Some(128000) },
                    ModelDesc { upstream_model_id: "glm-5.2".into(), context_size: 1000000, output_size: Some(131072) },
                ],
            }],
        };
        let custom = ProviderCatalog {
            version: 1,
            providers: vec![
                // override existing model + supplement a new model under an existing provider
                ProviderDesc {
                    provider_id: "zhipu".into(),
                    individual_usage_url: None,
                    models: vec![
                        // sparse entry: no output_size -> baseline output kept
                        ModelDesc { upstream_model_id: "glm-4.6".into(), context_size: 999, output_size: None },
                        ModelDesc { upstream_model_id: "custom-user".into(), context_size: 5000, output_size: Some(4096) },
                    ],
                },
                // supplement an entirely new provider
                ProviderDesc {
                    provider_id: "mycustom".into(),
                    individual_usage_url: None,
                    models: vec![ModelDesc { upstream_model_id: "weird".into(), context_size: 7000, output_size: None }],
                },
            ],
        };
        base.overlay_custom(&custom);
        assert_eq!(base.context_size("zhipu", "glm-4.6"), Some(999), "custom overrides baseline");
        assert_eq!(base.output_size("zhipu", "glm-4.6"), Some(128000), "sparse custom keeps baseline output_size");
        assert_eq!(base.context_size("zhipu", "glm-5.2"), Some(1000000), "untouched baseline kept");
        assert_eq!(base.context_size("zhipu", "custom-user"), Some(5000), "new model under existing provider");
        assert_eq!(base.output_size("zhipu", "custom-user"), Some(4096));
        assert_eq!(base.context_size("mycustom", "weird"), Some(7000), "new provider added wholesale");
        assert_eq!(base.output_size("mycustom", "weird"), None);
    }

    #[test]
    fn set_context_size_adds_updates_removes() {
        let mut c = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc { upstream_model_id: "glm-4.6".into(), context_size: 200000, output_size: None }],
            }],
        };
        // override existing
        c.set_context_size("zhipu", "glm-4.6", Some(999));
        assert_eq!(c.context_size("zhipu", "glm-4.6"), Some(999));
        // add new model under existing provider
        c.set_context_size("zhipu", "glm-new", Some(5000));
        assert_eq!(c.context_size("zhipu", "glm-new"), Some(5000));
        // add model under a brand-new provider
        c.set_context_size("mycustom", "weird", Some(7000));
        assert_eq!(c.context_size("mycustom", "weird"), Some(7000));
        // remove a model (provider group stays, has other models)
        c.set_context_size("zhipu", "glm-new", None);
        assert_eq!(c.context_size("zhipu", "glm-new"), None);
        assert_eq!(c.context_size("zhipu", "glm-4.6"), Some(999), "sibling entry kept");
        // remove the last model under a provider -> group dropped
        c.set_context_size("mycustom", "weird", None);
        assert!(c.providers.iter().all(|p| p.provider_id != "mycustom"), "empty group dropped");
        // remove a non-existent entry -> no-op
        c.set_context_size("zhipu", "never-existed", None);
        assert_eq!(c.context_size("zhipu", "glm-4.6"), Some(999));
    }

    #[test]
    fn ensure_returns_baseline_when_no_custom() {
        let dir = tempdir().unwrap();
        assert!(!custom_catalog_path(dir.path()).exists());
        let cat = ensure_catalog(dir.path());
        // baseline is in-memory only: no file is seeded to disk
        assert!(!custom_catalog_path(dir.path()).exists(), "no custom file created");
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(200000));
    }

    #[test]
    fn ensure_overlays_custom_when_present() {
        let dir = tempdir().unwrap();
        // custom: override glm-4.6 to 999 + add a user-only provider/model
        write_custom(
            dir.path(),
            r#"{"version":1,"providers":[
                {"provider_id":"zhipu","models":[{"upstream_model_id":"glm-4.6","context_size":999}]},
                {"provider_id":"mycustom","models":[{"upstream_model_id":"weird","context_size":7000}]}
            ]}"#,
        );
        let cat = ensure_catalog(dir.path());
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(999), "custom overrides baseline");
        assert_eq!(cat.context_size("zhipu", "glm-5.2"), Some(1000000), "baseline untouched entry kept");
        assert_eq!(cat.context_size("mycustom", "weird"), Some(7000), "user-supplemented provider added");
        // custom file left untouched on disk
        let on_disk = std::fs::read_to_string(custom_catalog_path(dir.path())).unwrap();
        assert!(on_disk.contains("glm-4.6"), "custom file not modified by ensure_catalog");
    }

    #[test]
    fn ensure_ignores_malformed_custom() {
        let dir = tempdir().unwrap();
        write_custom(dir.path(), "{ not valid json ");
        let cat = ensure_catalog(dir.path());
        // malformed custom ignored -> pure baseline
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(200000));
        // custom file NOT deleted (user's file preserved for manual repair)
        assert!(custom_catalog_path(dir.path()).exists());
    }

    #[test]
    fn save_custom_catalog_round_trips_through_ensure() {
        let dir = tempdir().unwrap();
        let custom = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-4.6".into(),
                    context_size: 333333,
                    output_size: None,
                }],
            }],
        };
        save_custom_catalog(dir.path(), &custom).unwrap();
        let cat = ensure_catalog(dir.path());
        assert_eq!(cat.context_size("zhipu", "glm-4.6"), Some(333333), "saved custom overlays baseline");
    }
}
