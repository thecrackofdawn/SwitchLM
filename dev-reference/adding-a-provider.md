# 添加新服务商参考

以 DeepSeek 为例，说明把一个新 LLM 服务商接入 SwitchLM 需要改动的地方。每个服务商的改动量取决于它的能力组合（见下方决策表）。

## 核心概念：`vendor` 是路由键

`Provider.vendor` 是一个 slug（如 `zhipu` / `deepseek` / `volcengine-coding`），是**所有按服务商差异化行为的路由键**：

- 用量查询适配器选择（`usage_provider_for`）
- 模型发现路径选择（`volcengine_plan_action` vs 通用 `/models`）
- 同账号冲突检测方式（AK 匹配 vs api_key 匹配）
- 限流错误识别（`rate_limit_in_json` 的 per-vendor code 分支）
- catalog 上下文大小查找（catalog 按 `provider_id == vendor` 建键）
- base_url 默认值与列表行标签的来源

`Provider.id` 只是 opaque 主键（多账号时每个账号一个 id），**不参与路由**。新服务商的第一步就是定一个 vendor slug。

## 决策表：哪些改动需要做

| 能力 | 需要改的地方 | 简单 Bearer-key 服务商（如 DeepSeek/智谱） |
|---|---|---|
| 下拉选择 + 标签 | `src/lib/selectLabel.ts` | ✅ 必改 |
| base_url 默认值 | `src/views/Provider.vue` (`vendorDefaults`) | ✅ 必改 |
| 上下文大小 catalog | `src-tauri/assets/provider_desc.json` | ✅ 建议改 |
| catalog 查找测试 | `src-tauri/src/config/catalog.rs` | ✅ 建议改 |
| 已知 vendor 文档注释 | `src-tauri/src/config/types.rs` | ✅ 建议改 |
| 用量查询适配器 | `src-tauri/src/usage/mod.rs` + 新适配器模块 | ❌ 无用量 API 则跳过 |
| AKSK 用量凭据表单 | `Provider.vue` (`needsUsageCreds`) + `commands.rs` | ❌ 仅 Bearer 则跳过 |
| 特殊模型发现 | `commands.rs` (`volcengine_plan_action` + discover 路径) | ❌ 走通用 `/models` 则跳过 |
| 限流错误码识别 | `src-tauri/src/proxy/error_adapter.rs` | ❌ 429+关键字兜底够用则跳过 |
| 遗留配置迁移 | `src-tauri/src/config/store.rs` (`normalize_legacy_vendors`) | ❌ 全新服务商无需 |

**关键结论**：DeepSeek 这类纯 Bearer-key、无用量 API、走标准 `/models` 发现的服务商，**后端逻辑零改动**——所有按 vendor 路由的代码对未知 vendor 都有合理默认（用量→None、发现→通用 `/models`、冲突→按 api_key、限流→429+关键字）。只需前端注册 + catalog 数据。

## 必改项（简单 Bearer-key 服务商）

### 1. `src/lib/selectLabel.ts` — 下拉选项 + 标签

在 `vendorOptions` 加一行。`label` 是下拉显示名，`value` 是 vendor slug：

```ts
export const vendorOptions = [
  { label: "智谱", value: "zhipu" },
  { label: "DeepSeek", value: "deepseek" },   // ← 新增
  { label: "火山 · agent plan", value: "volcengine-agent" },
  { label: "火山 · coding plan", value: "volcengine-coding" },
];
```

`vendorLabel()` 和 `providerLabel()` 自动复用这个表，无需另改。

### 2. `src/views/Provider.vue` — base_url 默认值

在 `vendorDefaults` 加该服务商的两个协议端点。选中 vendor 时自动回填：

```ts
const vendorDefaults: Record<string, { openai_base_url: string; anthropic_base_url: string }> = {
  deepseek: {
    openai_base_url: "https://api.deepseek.com",
    anthropic_base_url: "https://api.deepseek.com/anthropic",
  },
  // ...
};
```

**base_url 拼接规则**（见 `src-tauri/src/proxy/dispatch.rs::join_url`）：

- OpenAI 后端 → `{openai_base_url}/chat/completions`
- Anthropic 后端 → `{anthropic_base_url}/v1/messages`

所以填的 base_url 要去掉末尾 `/`，且要让「base + 上述后缀」是该服务商的真实端点。例：DeepSeek 的 `https://api.deepseek.com` + `/chat/completions` = `https://api.deepseek.com/chat/completions`（DeepSeek 两者都接受）；`https://api.deepseek.com/anthropic` + `/v1/messages` = `https://api.deepseek.com/anthropic/v1/messages`（DeepSeek 文档的 Anthropic 端点）。

连接测试和模型发现都用 `GET {openai_base_url}/models`，故 openai_base_url 必须能响应 `/models`。

### 3. `src-tauri/assets/provider_desc.json` — 上下文大小 catalog

按 `provider_id == vendor` 建键，加该服务商的模型。`context_size` 单位是 token，换算进制看服务商（智谱/DeepSeek 用 1000 进制，1M = `1000000`；火山用 1024 进制，1M = `1024000`）：

```json
{ "provider_id": "deepseek", "models": [
  { "upstream_model_id": "deepseek-v4-flash", "context_size": 1000000 },
  { "upstream_model_id": "deepseek-v4-pro", "context_size": 1000000 }
] }
```

`upstream_model_id` 必须是该服务商 API 实际接受的模型名（转发时代码原样发给上游）。`provider_id` 等于该服务商的 vendor slug（不是 opaque 账号 id）。结构按厂商维度组织是为了后续可扩展 provider 级属性（如不同套餐的并发数）；未知字段被旧版本忽略，加字段不会破坏兼容。

catalog 作用：模型配置页的「识别上下文大小」只读提示，以及 fallback 容量检查（`classify_fallback`）。不填则这两处退化为 Unknown，不影响转发。

内置 `provider_desc.json` 是内存基线，每次启动从二进制重新加载（不落盘），所以改了内置默认值下次启动立即生效。用户自定义/补充的上下文大小写在持久化目录的 `custom_provider_desc.json`（同结构，稀疏——只存覆盖过的条目，故未覆盖条目仍随内置更新生效），启动时覆盖基线；Models 页面编辑上下文大小时经 `set_custom_context_size` 实时写盘并同步重派生内存（同 vendor 共享、无需重启）。custom 值优先；文件损坏则忽略不删。`Model` 本身不再保存 `context_size`——catalog 是唯一来源。

### 4. `src-tauri/src/config/catalog.rs` — 守护测试

在 `lookup_finds_bundled_models` 加断言，防止 catalog 数据被改坏：

```rust
assert_eq!(cat.context_size("deepseek", "deepseek-v4-flash"), Some(1000000));
assert_eq!(cat.context_size("deepseek", "deepseek-v4-pro"), Some(1000000));
```

### 5. `src-tauri/src/config/types.rs` — 文档注释

把新 slug 加进 `Provider.vendor` 字段的已知 vendor 列表（纯文档，无运行时影响）。

## 条件改动项（仅当服务商有对应能力时）

### A. 用量查询（服务商有套餐/额度 API）

1. 新建适配器模块 `src-tauri/src/usage/<vendor>.rs`，实现 `UsageProvider` trait。
2. 在 `src-tauri/src/usage/mod.rs` 的 `usage_provider_for(vendor)` match 里注册：
   ```rust
   "deepseek" => Some(Box::new(deepseek::DeepSeekUsageProvider)),
   ```
3. 适配器自行决定用推理 api_key 还是 AK/SK。

无用量 API 的服务商不注册，`usage_provider_for` 返回 `None`，用量页显示 N/A。

### B. AKSK 用量凭据（如火山）

`UsageCreds`（access_key_id 入配置、secret_access_key 入 keyring）目前只对 `volcengine*` 启用：

- `Provider.vue` 的 `needsUsageCreds = form.vendor.startsWith("volcengine")` —— 新 AKSK 服务商需扩展此判断。
- `commands.rs::same_account_conflict` 的 `vendor.starts_with("volcengine")` 分支 —— 新 AKSK 服务商需扩展，否则同账号检测会退回按 api_key 匹配。
- 表单里 AK/SK 字段、`set_provider_usage_sk` / `provider_has_usage_sk` 命令已通用，无需另写。

纯 Bearer 服务商（DeepSeek/智谱）此项全跳过——`needsUsageCreds` 为 false，AK/SK 字段不显示。

### C. 特殊模型发现（非通用 `/models`）

火山走控制面 OpenAPI（AK/SK 签名）列套餐模型。在 `commands.rs`：

- `volcengine_plan_action(vendor)` 的 match 里加 slug → action 映射。
- `discover_models` 据此分支走 `discover_volcengine_models`，否则走通用 `fetch_discovered_models`（`GET {base}/models`）。

支持标准 `/models` 的服务商（DeepSeek/智谱）走通用路径，无需改。

### D. 限流错误识别（`src-tauri/src/proxy/error_adapter.rs`）

`is_rate_limit_error` 已有通用兜底：HTTP 429 一律算限流；body 里命中 `rate_limit`/`quota`/`throttl`/`too many requests`/`资源耗尽`/`欠费`/`overdue`/`insufficient balance` 等关键字也算。`rate_limit_in_json` 的 per-vendor code 分支**只在服务商用「非 429 + 非关键字」的特殊错误码表达限流/欠费/套餐不可用时才需要加**——例如火山 `AccountOverdueError`(403 欠费)、`InvalidSubscription`(套餐过期)。智谱/火山的限流码本身都是 429(通用兜底已覆盖),只有这类"非 429 但仍该切走"的码才必须显式列。大多数服务商 429 兜底已够。各家错误码全表与 fallback/透传分类见 [`vendor-error-codes.md`](./vendor-error-codes.md)。

### E. 遗留配置迁移（`store.rs::normalize_legacy_vendors`）

仅当存在「用 vendor slug 当 provider id 的旧配置」需要迁移时相关。全新服务商无历史配置，跳过。

## 配置模型 fallback（可选 · 运行时配置，非代码改动）

前面的步骤都是「让新服务商的模型能被转发」。要让它的模型参与**限流兜底链**（主模型 429/配额耗尽时自动切到另一个模型），还需配置模型 fallback——这是写进 `app_config.json` 的运行时配置，**不需要改代码**。

**机制**：fallback 配在 **Model** 上（`Model.fallback_target_model_id`，`config/types.rs`），**不是 Profile**；值是另一个 Model 的 `id`。转发时主模型命中限流/配额错误 → 触发熔断并逐跳推进到 `fallback_target_model_id`（`proxy/dispatch.rs` 的 `walk_*`，`next_fallback_id` 解析目标）。设置命令 `set_model_fallback`（`commands.rs`，保存 + 重置该模型的熔断态）。

**在哪配**：独立的「Fallback」页（路由 `fallback`，`src/views/Fallback.vue`）。注意 Models 模态框里**不**配 fallback——那里只是保留原值（见 `Models.vue` 注释）。

**接入新服务商时需要知道的约束**：

- **目标必须先存在**：把新服务商的模型 A 设为别的模型的 fallback，或让 A 指向别的模型，A 必须先在 Models 页建好（即上面的 catalog + Models 配置已完成）。
- **跨 vendor 可用**：fallback 模型用自己 provider 的协议与 base_url。passthrough 需两侧协议一致；客户端 Anthropic、后端 OpenAI 时走 translate（`translate/`），故跨协议兜底也成立。
- **只有限流/配额错误才走 fallback**：`is_rate_limit_error`（429 + 各 vendor 特殊码 + 关键字兜底）触发熔断并推进；401/400/500/网络错误**直通不 fallback**。因此要让兜底真正生效，下面的 D 项（限流码识别）需覆盖该服务商。
- **链而非环**：dispatch 用 visited-set 检测环、`MAX_FALLBACK_HOPS = 8` 封顶，超限即 `FallbackExhausted`。建链要无环、别太长。
- **上下文容量检查**：保存前 `validate_fallback_context`（纯逻辑 `classify_fallback`）比较主模型与 fallback 的有效上下文，fallback 更小时弹非阻断警告（「仍要设置」可继续）。该检查**读第 3 步的 catalog**——把新服务商的 `context_size` 填进 catalog 能让提示准确，否则退化为 Unknown（仍可保存，但不提示）。

> 纯 Bearer-key 服务商：fallback 零代码改动，纯运行时配置；新服务商的模型建好后，直接在 Fallback 页接进兜底链即可。

## 验证

```bash
# 后端：catalog 解析 + 查找测试
cd src-tauri && cargo test --lib config::catalog

# 前端：类型检查
npx vue-tsc --noEmit
```

手动验证：新增服务商配置 → 选 vendor 后 base_url 自动回填 → 填 api_key → 连接测试通过 → 模型配置页新增模型时 upstream_model_id 命中 catalog 显示上下文提示。

## 完整清单（简单 Bearer-key 服务商）

- [ ] `src/lib/selectLabel.ts` — `vendorOptions` 加一行
- [ ] `src/views/Provider.vue` — `vendorDefaults` 加 base_url 默认值
- [ ] `src-tauri/assets/provider_desc.json` — 加模型 catalog 条目
- [ ] `src-tauri/src/config/catalog.rs` — 加 catalog 查找断言
- [ ] `src-tauri/src/config/types.rs` — 更新已知 vendor 文档注释
- [ ] `cargo test --lib config::catalog` 通过
- [ ] `npx vue-tsc --noEmit` 通过
- [ ]（条件）用量适配器 / AKSK 凭据 / 特殊发现 / 限流码 / 迁移 —— 按决策表判断
- [ ]（可选）Fallback 页：把新服务商的模型接进兜底链（目标先存在、无环、链长 ≤8、fallback 上下文 ≥ 主模型）
