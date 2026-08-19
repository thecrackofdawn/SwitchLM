# 模型表单目录字段改进 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Models 页模型弹窗——上下文大小改为可输入下拉（静态档位）并新增同构的「模型最大输出」字段（含 bundled 预填、重置默认按钮、读取命令合并、文案调整）。

**Architecture:** 目录（catalog）是两字段唯一的真值源（bundled baseline + `custom_provider_desc.json` 稀疏覆盖）。custom 条目以 `context_size == 0` 作"未覆盖 baseline context"哨兵；新增 `set_output_size` 与既有 `set_context_size` 对称；读取侧合并为 `model_catalog_sizes`（一次返回 effective + default 四值）。前端 `Models.vue` 两个 `NSelect filterable tag clearable` 字段 + 重置按钮，保存时"值===bundled 默认 → 落盘 null（清除覆盖）"。

**Tech Stack:** Rust (axum/tauri v2, serde)、Vue 3 `<script setup>` + Naive UI + Pinia、pnpm。

**Spec:** `docs/superpowers/specs/2026-08-19-model-form-catalog-fields-design.md`（含所有决策的 why）

## Global Constraints

- 前端包管理器是 **pnpm**，禁止 npm/npx（会写外来 lockfile）。类型检查用 `pnpm exec vue-tsc --noEmit`。
- 后端测试 co-located 为 `#[cfg(test)] mod tests`，命令 `cargo test --manifest-path src-tauri/Cargo.toml`；单模块 `cargo test --manifest-path src-tauri/Cargo.toml config::catalog`。
- **日志必须用 vendor + upstream model 名，绝不用内部 `id`**（本计划日志面很小，遵守即可）。
- serde 结构**不使用 rename_all**：字段 snake_case 直通；前端 `src/lib/types.ts` 手工镜像保持同步。
- UI 文案（逐字）：「上下文大小（单位：B）」「模型最大输出（单位：B）」「同服务商共享」「重置默认」「模型名称（即服务商提供的模型）」「未识别」（context 占位符）「未收录」（output 占位符）。
- 不改 `agent_sync.rs`、`Model` 结构（config/types.rs）、`provider_desc.json`、Settings.vue/Fallback.vue 文案。
- 开发直接在 main 分支上（用户工作流），commit 在每个任务完成时进行。

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src-tauri/src/config/catalog.rs` | 修改 | `set_output_size`、`overlay_custom` 哨兵规则、`set_context_size` None 分支修正、`context_size()` 0→None、`parse_embedded` 提为 pub(crate)、测试 |
| `src-tauri/src/commands.rs` | 修改 | `CatalogSizes` + `model_catalog_sizes` + `set_custom_output_size`、删 `recognized_context_size`、测试 |
| `src-tauri/src/lib.rs` | 修改 | 命令注册表 |
| `src/lib/commands.ts` | 修改 | `modelCatalogSizes`、`setCustomOutputSize`、删 `recognizedContextSize` |
| `src/views/Models.vue` | 修改 | 两字段 UI、tag 解析、fetch/save、重置按钮、文案 |

---

### Task 1: catalog.rs — set_output_size + 哨兵规则 + context_size() 语义修正

**Files:**
- Modify: `src-tauri/src/config/catalog.rs:42-135`（impl 块）、`:200-400`（tests）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 现有 `ModelDesc { upstream_model_id: String, context_size: u32, output_size: Option<u32> }`、`find_model`、`set_context_size`
- Produces（Task 2/3 依赖，签名逐字）:
  - `pub fn set_output_size(&mut self, vendor: &str, upstream_model_id: &str, size: Option<u32>)`
  - `pub(crate) fn parse_embedded() -> ProviderCatalog`（从私有 `fn parse_embedded` 提升）
  - `pub fn context_size(&self, vendor, upstream_model_id) -> Option<u32>`（语义变化：条目值为 0 → None）
  - `overlay_custom` 行为变化（哨兵规则，非签名变化）

- [ ] **Step 1: 写失败测试**

在 `catalog.rs` 的 `mod tests` 末尾（`save_custom_catalog_round_trips_through_ensure` 之后、闭括号之前）追加：

```rust
    // ---- set_output_size + 哨兵规则（spec §3）----

    #[test]
    fn set_output_size_updates_existing_entry() {
        let mut c = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-4.6".into(),
                    context_size: 200000,
                    output_size: Some(128000),
                }],
            }],
        };
        c.set_output_size("zhipu", "glm-4.6", Some(96000));
        assert_eq!(c.output_size("zhipu", "glm-4.6"), Some(96000));
        assert_eq!(c.context_size("zhipu", "glm-4.6"), Some(200000), "context 不动");
    }

    #[test]
    fn set_output_size_creates_entry_with_sentinel_context() {
        let mut c = ProviderCatalog::default();
        // 不存在的条目：新建（context=0 哨兵 = 未覆盖）
        c.set_output_size("zhipu", "glm-custom", Some(4096));
        assert_eq!(c.output_size("zhipu", "glm-custom"), Some(4096));
        // custom 文件本体保留哨兵 0（struct 直读，不走 context_size() 的 0→None 映射）
        let m = c.find_model("zhipu", "glm-custom").unwrap();
        assert_eq!(m.context_size, 0);
        // 不存在的 provider 组：建组
        c.set_output_size("myvendor", "weird", Some(8192));
        assert_eq!(c.output_size("myvendor", "weird"), Some(8192));
    }

    #[test]
    fn set_output_size_none_removes_entry_when_context_is_sentinel() {
        let mut c = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-custom".into(),
                    context_size: 0,
                    output_size: Some(4096),
                }],
            }],
        };
        c.set_output_size("zhipu", "glm-custom", None);
        assert!(c.providers.iter().all(|p| p.provider_id != "zhipu"), "组空则删组");
    }

    #[test]
    fn set_output_size_none_keeps_entry_when_context_override_exists() {
        let mut c = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-4.6".into(),
                    context_size: 999,
                    output_size: Some(40000),
                }],
            }],
        };
        c.set_output_size("zhipu", "glm-4.6", None);
        assert_eq!(c.output_size("zhipu", "glm-4.6"), None, "output 覆盖已清");
        assert_eq!(c.context_size("zhipu", "glm-4.6"), Some(999), "context 覆盖保留");
    }

    #[test]
    fn set_output_size_none_on_missing_entry_is_noop() {
        let mut c = ProviderCatalog::default();
        c.set_output_size("zhipu", "nope", None);
        assert!(c.providers.is_empty());
    }

    #[test]
    fn set_context_size_none_keeps_entry_when_output_override_exists() {
        // 修正既有缺陷：清 context 覆盖不再连带丢掉同条目的 output 覆盖（spec §3 末段）
        let mut c = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-4.6".into(),
                    context_size: 999,
                    output_size: Some(40000),
                }],
            }],
        };
        c.set_context_size("zhipu", "glm-4.6", None);
        assert_eq!(c.context_size("zhipu", "glm-4.6"), None, "context 视为未覆盖");
        let m = c.find_model("zhipu", "glm-4.6").expect("条目保留（output 覆盖还在）");
        assert_eq!(m.context_size, 0, "哨兵写入");
        assert_eq!(m.output_size, Some(40000));
    }

    #[test]
    fn overlay_sentinel_context_keeps_baseline() {
        // custom 条目 context=0 → baseline context 保留；output 覆盖生效
        let mut base = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-4.6".into(),
                    context_size: 200000,
                    output_size: Some(128000),
                }],
            }],
        };
        let custom = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-4.6".into(),
                    context_size: 0,
                    output_size: Some(96000),
                }],
            }],
        };
        base.overlay_custom(&custom);
        assert_eq!(base.context_size("zhipu", "glm-4.6"), Some(200000), "哨兵不覆盖 baseline context");
        assert_eq!(base.output_size("zhipu", "glm-4.6"), Some(96000), "output 覆盖生效");
    }

    #[test]
    fn overlay_sentinel_context_without_baseline_merges_as_unknown() {
        // baseline 无该条目：按 0 合并，context_size() 把 0 视同未收录 → None
        let mut base = ProviderCatalog::default();
        let custom = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "glm-custom".into(),
                    context_size: 0,
                    output_size: Some(4096),
                }],
            }],
        };
        base.overlay_custom(&custom);
        assert_eq!(base.context_size("zhipu", "glm-custom"), None, "0 视同未收录");
        assert_eq!(base.output_size("zhipu", "glm-custom"), Some(4096));
    }

    #[test]
    fn context_size_zero_treated_as_unknown() {
        // context_size() 语义修正：0 → None（防 classify_fallback / resolve_limits 把 0 当真值）
        let c = ProviderCatalog {
            version: 1,
            providers: vec![ProviderDesc {
                provider_id: "zhipu".into(),
                individual_usage_url: None,
                models: vec![ModelDesc {
                    upstream_model_id: "weird".into(),
                    context_size: 0,
                    output_size: None,
                }],
            }],
        };
        assert_eq!(c.context_size("zhipu", "weird"), None);
    }
```

注意：`find_model` 是私有 fn，但测试在 `catalog.rs` 内的 `mod tests`（`use super::*`），可直接访问。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test --manifest-path src-tauri/Cargo.toml config::catalog
```

预期：编译失败——`set_output_size` 不存在（`no method named set_output_size`）。

- [ ] **Step 3: 实现**

在 `catalog.rs` impl 块中做四处修改：

**(a) `find_model` 已是私有、够用；`context_size()` 改语义**（约 52-54 行）：

```rust
    /// Catalog-only lookup (ignores any per-Model manual override). None if not bundled.
    /// A `context_size == 0` entry counts as unknown: 0 is the sparse-file sentinel for
    /// "this entry doesn't override the baseline context" (spec §3), so it must never leak
    /// as a real size (classify_fallback / OpenCode limit.context would treat 0 as truth).
    /// `vendor` is the provider's vendor slug (the catalog keys groups by `provider_id == vendor`).
    pub fn context_size(&self, vendor: &str, upstream_model_id: &str) -> Option<u32> {
        self.find_model(vendor, upstream_model_id)
            .map(|m| m.context_size)
            .filter(|&n| n > 0)
    }
```

**(b) `overlay_custom` 加哨兵规则**——把 `existing.context_size = cm.context_size;`（约 78 行）改为：

```rust
                            // 哨兵 0 = 该条目不覆盖 baseline 的 context（仅带 output 覆盖）
                            if cm.context_size > 0 {
                                existing.context_size = cm.context_size;
                            }
```

（else 分支 push `cm.clone()` 的行为不变：0 原样合并，由 `context_size()` 的 0→None 兜底。）

**(c) `set_context_size` None 分支修正**（约 96-134 行，整个方法替换为）：

```rust
    /// Set, override, or remove a single (vendor, upstream_model_id) entry's context override
    /// in this catalog. Used to mutate the **sparse** custom file: `Some(n)` sets/overrides the
    /// entry (creating the provider group if absent), `None` removes the context override.
    /// Clearing keeps the entry (with `context_size = 0` sentinel) when it still carries an
    /// output override, so the two overrides on one entry are independently removable; the
    /// entry (and its provider group) is deleted only when neither override remains.
    /// Removing a non-existent entry is a no-op.
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
                    if let Some(existing) = g
                        .models
                        .iter_mut()
                        .find(|m| m.upstream_model_id == upstream_model_id)
                    {
                        if existing.output_size.is_some() {
                            // output 覆盖还在：置哨兵保留条目，不删
                            existing.context_size = 0;
                            return;
                        }
                    }
                    g.models.retain(|m| m.upstream_model_id != upstream_model_id);
                    if g.models.is_empty() {
                        self.providers.retain(|p| p.provider_id != vendor);
                    }
                }
            }
        }
    }
```

**(d) 新增 `set_output_size`**（`set_context_size` 之后）：

```rust
    /// Set, override, or remove the output-size override for a (vendor, upstream_model_id)
    /// entry - the output-side twin of `set_context_size`. `Some(n)` sets/overrides (creating
    /// the entry with `context_size = 0` sentinel + the provider group if absent, so a fresh
    /// output-only override never touches the baseline context). `None` removes the override
    /// (serde `skip_serializing_if` drops the key from disk); the entry is deleted entirely
    /// only when its context is also the 0 sentinel (neither override remains). No-op on a
    /// missing entry.
    pub fn set_output_size(&mut self, vendor: &str, upstream_model_id: &str, size: Option<u32>) {
        let group = self.providers.iter_mut().find(|p| p.provider_id == vendor);
        match size {
            Some(n) => match group {
                Some(g) => {
                    if let Some(existing) = g
                        .models
                        .iter_mut()
                        .find(|m| m.upstream_model_id == upstream_model_id)
                    {
                        existing.output_size = Some(n);
                    } else {
                        g.models.push(ModelDesc {
                            upstream_model_id: upstream_model_id.into(),
                            context_size: 0,
                            output_size: Some(n),
                        });
                    }
                }
                None => self.providers.push(ProviderDesc {
                    provider_id: vendor.into(),
                    individual_usage_url: None,
                    models: vec![ModelDesc {
                        upstream_model_id: upstream_model_id.into(),
                        context_size: 0,
                        output_size: Some(n),
                    }],
                }),
            },
            None => {
                if let Some(g) = group {
                    let only_output = g
                        .models
                        .iter()
                        .find(|m| m.upstream_model_id == upstream_model_id)
                        .map(|m| m.context_size == 0)
                        .unwrap_or(false);
                    if only_output {
                        g.models.retain(|m| m.upstream_model_id != upstream_model_id);
                        if g.models.is_empty() {
                            self.providers.retain(|p| p.provider_id != vendor);
                        }
                    } else if let Some(existing) = g
                        .models
                        .iter_mut()
                        .find(|m| m.upstream_model_id == upstream_model_id)
                    {
                        existing.output_size = None;
                    }
                }
            }
        }
    }
```

**(e) `parse_embedded` 提为 `pub(crate)`**（约 143 行）：

```rust
pub(crate) fn parse_embedded() -> ProviderCatalog {
```

并在 doc comment 的开头补一句用途：`/// Parse the embedded baseline catalog (Task: also used by model_catalog_sizes to read bundled defaults, bypassing custom overrides).` —— 即把现有 doc comment 改为：

```rust
/// Parse the embedded baseline catalog. Besides the startup path, `model_catalog_sizes`
/// uses this to read bundled defaults while bypassing custom overrides.
```

- [ ] **Step 4: 跑测试确认全绿**

```bash
cargo test --manifest-path src-tauri/Cargo.toml config::catalog
```

预期：全部 PASS（含既有测试——`set_context_size_adds_updates_removes` 的条目无 output 覆盖，删整条行为不变）。

- [ ] **Step 5: 跑全量后端测试确认无回归**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

预期：PASS（`context_size()` 0→None 语义对既有调用方是收窄：现网数据里 effective 值恒 >0，不受影响）。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/config/catalog.rs
git commit -m "feat(catalog): set_output_size + context=0 哨兵规则（output 覆盖独立可清，清 context 不再丢 output 覆盖）"
```

---

### Task 2: commands.rs + lib.rs — model_catalog_sizes + set_custom_output_size，删 recognized_context_size

**Files:**
- Modify: `src-tauri/src/commands.rs:668-750`（recognized_context_size/set_custom_context_size 区域）
- Modify: `src-tauri/src/lib.rs:282-284`（注册表）
- Test: `src-tauri/src/commands.rs` `mod tests`

**Interfaces:**
- Consumes: Task 1 的 `parse_embedded()`（pub(crate)）、`catalog.context_size()/output_size()`、`catalog::load_custom/set_output_size/save_custom_catalog/effective_catalog`
- Produces（Task 3 依赖，序列化字段名逐字）:
  - Rust: `pub struct CatalogSizes { context_effective: Option<u32>, context_default: Option<u32>, output_effective: Option<u32>, output_default: Option<u32> }`
  - 命令名（Tauri 注册名）: `model_catalog_sizes`、`set_custom_output_size`

- [ ] **Step 1: 写失败测试**

在 `commands.rs` `mod tests` 末尾追加（模式参照 `grant_secret_consent_core` 测试的 state 构造；`AppStateInner::load` 会加载无 custom 文件的临时目录 → catalog 为纯 baseline）。

设计说明：**不引入 `tauri::test` feature**。两个新命令的测试都打 core fn（读侧 `catalog_sizes_core`、写侧 `set_custom_output_size_core`，见 Step 3）——与 `grant_secret_consent_core` 既有模式一致，命令壳（`State`/`AppHandle` 装箱层）不进测试。

```rust
    // ---- model_catalog_sizes / set_custom_output_size（spec §2）----

    #[tokio::test]
    async fn model_catalog_sizes_reports_effective_and_default() {
        use crate::proxy::AppStateInner;
        let dir = tempfile::tempdir().unwrap();
        let state: AppState =
            Arc::new(AppStateInner::load(dir.path(), Arc::new(MemoryStore::default())).unwrap());
        cfg_with_zhipu_provider(&state).await;

        // bundled 模型：effective == default == bundled 值
        let s = catalog_sizes_core(&state, "p1", "glm-4.6").await;
        assert_eq!(s.context_default, Some(200000));
        assert_eq!(s.context_effective, Some(200000));
        assert_eq!(s.output_default, Some(128000));
        assert_eq!(s.output_effective, Some(128000));

        // 写入 output 覆盖后：output_effective 变、output_default 不变（bundled 原值）
        set_custom_output_size_core(
            dir.path(),
            &state,
            "p1".into(),
            "glm-4.6".into(),
            Some(96000),
        )
        .await
        .unwrap();
        let s = catalog_sizes_core(&state, "p1", "glm-4.6").await;
        assert_eq!(s.output_effective, Some(96000), "覆盖生效");
        assert_eq!(s.output_default, Some(128000), "default 仍是 bundled 原值");
        assert_eq!(s.context_effective, Some(200000), "context 不受影响");
        // 覆盖已落盘 custom 文件
        let on_disk = std::fs::read_to_string(dir.path().join("custom_provider_desc.json")).unwrap();
        assert!(on_disk.contains("96000"), "写入 custom_provider_desc.json");

        // 未收录模型：四值全 None
        let s = catalog_sizes_core(&state, "p1", "nope").await;
        assert_eq!(s.context_effective, None);
        assert_eq!(s.output_effective, None);
        assert_eq!(s.context_default, None);
        assert_eq!(s.output_default, None);
    }

    /// helper：给 state 塞一个 vendor=zhipu 的 provider（id=p1）
    async fn cfg_with_zhipu_provider(state: &AppState) {
        let mut cfg = state.config.read().await.clone();
        cfg.providers.push(Provider {
            id: "p1".into(),
            vendor: "zhipu".into(),
            display_name: "智谱".into(),
            openai_base_url: Some("https://x/v1".into()),
            anthropic_base_url: None,
            usage_creds: None,
        });
        *state.config.write().await = cfg;
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test --manifest-path src-tauri/Cargo.toml commands::tests::model_catalog_sizes_reports_effective_and_default
```

预期：编译失败——`catalog_sizes_core`/`set_custom_output_size_core`/`CatalogSizes` 未定义。

- [ ] **Step 3: 实现**

**(a) `commands.rs`：替换 `recognized_context_size`（668-693 行）为 `CatalogSizes` + core + `model_catalog_sizes` 命令壳**（core 模式：读逻辑抽成无 `State` 参数的 core fn，测试直接打 core；命令壳薄封装）：

```rust
/// Effective + bundled-default catalog sizes for a (provider, upstream_model) pair, both
/// fields (context & output). "Effective" comes from the in-memory catalog (custom override
/// if set, else bundled default) - what the form pre-fills and what the proxy/OpenCode sync
/// actually use. "Default" re-reads the embedded baseline (`parse_embedded`), bypassing
/// custom overrides - the reset button's target and the save-normalization reference.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogSizes {
    pub context_effective: Option<u32>,
    pub context_default: Option<u32>,
    pub output_effective: Option<u32>,
    pub output_default: Option<u32>,
}

/// Core of `model_catalog_sizes`, testable without tauri State.
pub(crate) async fn catalog_sizes_core(
    state: &AppState,
    provider_id: &str,
    upstream_model_id: &str,
) -> CatalogSizes {
    let vendor = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.vendor.clone())
            .unwrap_or_default()
    };
    let baseline = crate::config::catalog::parse_embedded();
    let catalog = state.catalog.read().await;
    CatalogSizes {
        context_effective: catalog.context_size(&vendor, upstream_model_id),
        context_default: baseline.context_size(&vendor, upstream_model_id),
        output_effective: catalog.output_size(&vendor, upstream_model_id),
        output_default: baseline.output_size(&vendor, upstream_model_id),
    }
}

/// See `catalog_sizes_core`. Returns `Result` because Tauri requires async commands with
/// reference inputs to return a `Result`; the error variant is never produced.
#[tauri::command]
pub async fn model_catalog_sizes(
    state: State<'_, AppState>,
    provider_id: String,
    upstream_model_id: String,
) -> Result<CatalogSizes, String> {
    Ok(catalog_sizes_core(&state, &provider_id, &upstream_model_id).await)
}
```

**(b) `set_custom_output_size`（放在 `set_custom_context_size` 之后）**——core + 薄命令壳：

```rust
/// Core of `set_custom_output_size`, testable without a tauri AppHandle (the AppHandle only
/// resolves the AppData dir). Mirrors `set_custom_context_size`: write disk first, then
/// re-derive the in-memory catalog under the write lock.
async fn set_custom_output_size_core(
    dir: &std::path::Path,
    state: &AppState,
    provider_id: String,
    upstream_model_id: String,
    output_size: Option<u32>,
) -> Result<(), String> {
    let vendor = {
        let cfg = state.config.read().await;
        cfg.providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.vendor.clone())
            .ok_or_else(|| format!("provider {provider_id} not found"))?
    };
    let mut custom = crate::config::catalog::load_custom(dir);
    custom.set_output_size(&vendor, &upstream_model_id, output_size);
    crate::config::catalog::save_custom_catalog(dir, &custom).map_err(|e| e.to_string())?;
    let effective = crate::config::catalog::effective_catalog(&custom);
    *state.catalog.write().await = effective;
    Ok(())
}

/// Set (or clear, when `output_size` is None) the user's custom output-size override for a
/// (provider, upstream_model_id) pair - the output-side twin of `set_custom_context_size`.
/// Same real-time, same-vendor, sparse-file semantics; see that command's doc for the model.
#[tauri::command]
pub async fn set_custom_output_size(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    provider_id: String,
    upstream_model_id: String,
    output_size: Option<u32>,
) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    set_custom_output_size_core(&dir, &state, provider_id, upstream_model_id, output_size).await
}
```

**(c) `lib.rs` 注册表**：把 `commands::recognized_context_size,`（282 行）替换为：

```rust
            commands::model_catalog_sizes,
```

并在 `commands::set_custom_context_size,`（284 行）之后加：

```rust
            commands::set_custom_output_size,
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test --manifest-path src-tauri/Cargo.toml commands::tests::model_catalog_sizes_reports_effective_and_default
```

预期：PASS。

- [ ] **Step 5: 全量编译 + 测试（删除命令后无悬空引用）**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

预期：PASS。同时全局 grep `recognized_context_size`（src-tauri 与 src 两处目录）应只剩……不，此时**应为零处**（前端还没改，先不 grep src；src-tauri 内必须零残留——命令已删，`lib.rs` 引用已换）。若 `src-tauri` 内仍有引用则编译已报错。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): model_catalog_sizes（effective+default 四值）+ set_custom_output_size，移除 recognized_context_size"
```

---

### Task 3: commands.ts — 前端 wrapper

**Files:**
- Modify: `src/lib/commands.ts:77-87`

**Interfaces:**
- Consumes: Task 2 的命令名 `model_catalog_sizes`、`set_custom_output_size`、`CatalogSizes` 序列化形状
- Produces（Task 4 依赖，签名逐字）:
  - `export interface CatalogSizes { context_effective: number | null; context_default: number | null; output_effective: number | null; output_default: number | null }`
  - `export const modelCatalogSizes: (providerId: string, upstreamModelId: string) => Promise<CatalogSizes>`
  - `export const setCustomOutputSize: (providerId: string, upstreamModelId: string, outputSize: number | null) => Promise<void>`

- [ ] **Step 1: 替换代码**

把 `commands.ts` 77-87 行（`recognizedContextSize` 与 `setCustomContextSize` 及其注释）替换为：

```ts
/** Catalog sizes for a (provider, upstream_model) pair: effective (custom override or
 *  bundled default - what the form pre-fills) vs default (bundled value, bypassing
 *  overrides - the reset button target). null = not in the catalog. */
export interface CatalogSizes {
  context_effective: number | null;
  context_default: number | null;
  output_effective: number | null;
  output_default: number | null;
}

export const modelCatalogSizes = (providerId: string, upstreamModelId: string) =>
  invoke<CatalogSizes>("model_catalog_sizes", { providerId, upstreamModelId });

/** Set/clear a custom context-size override for a (provider, upstream_model) pair.
 *  Writes custom_provider_desc.json + updates the in-memory catalog (real-time, same-vendor).
 *  `null` clears the override -> reverts to the bundled default. */
export const setCustomContextSize = (
  providerId: string,
  upstreamModelId: string,
  contextSize: number | null,
) => invoke<void>("set_custom_context_size", { providerId, upstreamModelId, contextSize });

/** Set/clear a custom output-size override (OpenCode limit.output). Same semantics as
 *  setCustomContextSize; `null` clears -> reverts to the bundled default. */
export const setCustomOutputSize = (
  providerId: string,
  upstreamModelId: string,
  outputSize: number | null,
) => invoke<void>("set_custom_output_size", { providerId, upstreamModelId, outputSize });
```

- [ ] **Step 2: 类型检查**

```bash
pnpm exec vue-tsc --noEmit
```

预期：**报错** `src/views/Models.vue`——import 了已删除的 `recognizedContextSize`（这是预期的中间态失败；本任务只验证 wrapper 本身无误，Models.vue 的修复在 Task 4）。若要本任务全绿，可将 Task 3 与 Task 4 顺序对调的风险更大（Models.vue 会引用不存在的 wrapper）——接受此中间态，记录报错截图/输出即可。

- [ ] **Step 3: Commit（与 Task 4 一起提交亦可；此处单独提交以保持任务边界）**

```bash
git add src/lib/commands.ts
git commit -m "feat(frontend): modelCatalogSizes + setCustomOutputSize wrapper，移除 recognizedContextSize"
```

---

### Task 4: Models.vue — 两字段 UI + 重置按钮 + fetch/save + 文案

**Files:**
- Modify: `src/views/Models.vue`（script 1-221、template 269-296）

**Interfaces:**
- Consumes: Task 3 的 `modelCatalogSizes`、`setCustomOutputSize`、`setCustomContextSize`、`CatalogSizes`
- Produces: 无（叶子 UI）

- [ ] **Step 1: 更新 script——imports 与选项常量**

`Models.vue` 顶部 import 区（21 行）：

```ts
import { modelCatalogSizes, setCustomContextSize, setCustomOutputSize } from "../lib/commands";
```

（删除 `recognizedContextSize`。）`NInputNumber` 从 naive-ui import 列表移除（不再使用）。

在 `<script setup>` 中（`providerOptions` 之后约 33 行处）加静态档位与解析工具：

```ts
// ---- catalog size 下拉档位（十进制口径，spec §1；tag 允许输入任意数字）----
const CONTEXT_SIZE_OPTIONS = [
  { label: "128K (128000)", value: 128000 },
  { label: "200K (200000)", value: 200000 },
  { label: "256K (256000)", value: 256000 },
  { label: "1M (1000000)", value: 1000000 },
];
const OUTPUT_SIZE_OPTIONS = [
  { label: "8K (8192)", value: 8192 },
  { label: "32K (32000)", value: 32000 },
  { label: "64K (64000)", value: 64000 },
  { label: "128K (128000)", value: 128000 },
  { label: "256K (256000)", value: 256000 },
];

/** NSelect tag 输入是字符串：选项点击给 number 直传；tag 字符串 parseInt，NaN 丢弃。 */
function parseSizeInput(v: number | string | null): number | null {
  if (v === null) return null;
  if (typeof v === "number") return v;
  const n = Number.parseInt(v, 10);
  return Number.isFinite(n) ? n : null;
}
```

- [ ] **Step 2: 更新 script——表单状态与 fetch**

`FormState`（43 行附近）加字段；`blank()`（62 行附近）加 `output_size: null`；`openEdit`（88 行）的 `form.context_size = null;` 之后加 `form.output_size = null;`。

替换 191-213 行的 context-size 块为：

```ts
// ---- catalog sizes: effective (override or bundled default, auto-prefilled) for
// (provider, upstream), edited in-place and written real-time to
// custom_provider_desc.json via set_custom_context_size / set_custom_output_size.
// `lastRecognized*` gates the save so unchanged values aren't re-written; `default*` is
// the bundled value (reset button target + save-normalization reference: a value equal
// to the default is persisted as null = clear override, so bundled updates keep applying).
const lastRecognized = ref<number | null>(null);
const lastRecognizedOutput = ref<number | null>(null);
const defaultContext = ref<number | null>(null);
const defaultOutput = ref<number | null>(null);
async function fetchCatalogSizes() {
  const pid = form.provider_id;
  const up = form.upstream_model_id.trim();
  if (!pid || !up) {
    form.context_size = null;
    form.output_size = null;
    lastRecognized.value = null;
    lastRecognizedOutput.value = null;
    defaultContext.value = null;
    defaultOutput.value = null;
    return;
  }
  try {
    const s = await modelCatalogSizes(pid, up);
    form.context_size = s.context_effective;
    form.output_size = s.output_effective;
    lastRecognized.value = s.context_effective;
    lastRecognizedOutput.value = s.output_effective;
    defaultContext.value = s.context_default;
    defaultOutput.value = s.output_default;
  } catch {
    form.context_size = null;
    form.output_size = null;
    lastRecognized.value = null;
    lastRecognizedOutput.value = null;
    defaultContext.value = null;
    defaultOutput.value = null;
  }
}
watch(() => [form.provider_id, form.upstream_model_id], fetchCatalogSizes);
```

`openEdit` 末尾的 `void fetchContextSize();`（96 行）改为 `void fetchCatalogSizes();`。

- [ ] **Step 3: 更新 script——save 保存归一**

`save()`（129-139 行）中 context/output 写入部分替换为：

```ts
    await config.saveModel(buildModel());
    // catalog sizes are (vendor+upstream) properties, written separately and real-time
    // (memory + custom_provider_desc.json together). Save-normalization: a value equal to
    // the bundled default persists as null (clear override) so bundled updates keep applying.
    const ctx = form.context_size === defaultContext.value ? null : form.context_size;
    const out = form.output_size === defaultOutput.value ? null : form.output_size;
    if (form.context_size !== lastRecognized.value || ctx === null) {
      try {
        await setCustomContextSize(form.provider_id, form.upstream_model_id.trim(), ctx);
      } catch (e) {
        msg.warning(`上下文大小保存失败：${String(e)}`);
      }
    }
    if (form.output_size !== lastRecognizedOutput.value || out === null) {
      try {
        await setCustomOutputSize(form.provider_id, form.upstream_model_id.trim(), out);
      } catch (e) {
        msg.warning(`模型最大输出保存失败：${String(e)}`);
      }
    }
```

（注意 `|| ctx === null`：用户显式清空字段（想撤销自定义）时值可能恰等于 lastRecognized（无覆盖时 effective==输入残留场景），null 落盘分支必须放行。）

- [ ] **Step 4: 更新 template——两个字段**

替换 269-296 行（模型名称 NFormItem 的 label + 上下文大小 NFormItem）为：

```html
        <NFormItem label="模型名称（即服务商提供的模型）">
          <NSelect
            v-if="availableModels.length && !editing"
            v-model:value="form.upstream_model_id"
            :options="availableModels.map(m => ({ label: m, value: m }))"
            :render-label="ellipsisLabel"
            :loading="fetchingModels"
            placeholder="选择或输入模型名称"
            filterable
            tag
          />
          <NInput
            v-else
            v-model:value="form.upstream_model_id"
            placeholder="glm-4.6"
          />
        </NFormItem>
        <NFormItem label="上下文大小（单位：B）">
          <NSpace align="center" :size="8" style="width: 100%">
            <NSelect
              :value="form.context_size"
              :options="CONTEXT_SIZE_OPTIONS"
              :on-update:value="(v: number | string | null) => form.context_size = parseSizeInput(v)"
              filterable
              tag
              clearable
              placeholder="未识别"
              style="width: 200px"
            />
            <NButton size="tiny" quaternary :disabled="defaultContext === null" @click="form.context_size = defaultContext">重置默认</NButton>
            <span class="muted">同服务商共享</span>
          </NSpace>
        </NFormItem>
        <NFormItem label="模型最大输出（单位：B）">
          <NSpace align="center" :size="8" style="width: 100%">
            <NSelect
              :value="form.output_size"
              :options="OUTPUT_SIZE_OPTIONS"
              :on-update:value="(v: number | string | null) => form.output_size = parseSizeInput(v)"
              filterable
              tag
              clearable
              placeholder="未收录"
              style="width: 200px"
            />
            <NButton size="tiny" quaternary :disabled="defaultOutput === null" @click="form.output_size = defaultOutput">重置默认</NButton>
            <span class="muted">同服务商共享</span>
          </NSpace>
        </NFormItem>
```

模板里 `defaultContext`/`defaultOutput` 直接用 ref 名（`<script setup>` 自动解包；`:disabled` 比较与 `@click` 赋值都拿解包后的值）。

- [ ] **Step 5: 类型检查 + 构建验证**

```bash
pnpm exec vue-tsc --noEmit
```

预期：PASS（Task 3 的中间态报错在此消除）。若报 `NInputNumber` 未使用之类，删除该 import。

- [ ] **Step 6: 人工冒烟（可选但推荐；无 dev 环境时跳过并注明）**

`pnpm tauri dev`，模型页：

1. 新增模型选 bundled 模型（如 zhipu/glm-4.6）→ 两字段预填 200000 / 128000；「重置默认」可点。
2. 选档位 1M → 保存 → 重新打开编辑 → 预填 1000000（覆盖生效）；重置默认 → 字段回 200000 → 保存 → 重开 → 回 200000（覆盖已清）。
3. 手输 tag "333000" → 保存生效；清空字段保存 → 占位符「未识别」。
4. 未收录模型名 → 占位符「未识别」/「未收录」，重置默认禁用。

- [ ] **Step 7: Commit**

```bash
git add src/views/Models.vue
git commit -m "feat(models): 上下文/最大输出改为档位下拉（tag 可输入）+ bundled 预填 + 重置默认 + 文案调整"
```

---

### Task 5: 全量验证

**Files:** 无新改动（只读验证）

- [ ] **Step 1: 后端全量测试**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

预期：全部 PASS。

- [ ] **Step 2: 前端类型检查 + 构建**

```bash
pnpm build
```

（= `vue-tsc --noEmit` + `vite build`）预期：成功。

- [ ] **Step 3: 残留检查**

Grep `recognized_context_size|recognizedContextSize`（src/ 与 src-tauri/）→ 零命中。
Grep `同厂商共享|upstream_model_id，发给上游`（src/）→ 零命中。

预期：均零命中。

- [ ] **Step 4: 汇报**

无需 commit（无改动）。若一切通过，向用户汇报完成状态与人工冒烟清单。
