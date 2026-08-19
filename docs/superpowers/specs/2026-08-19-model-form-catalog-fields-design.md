# 模型表单目录字段改进：上下文大小下拉 + 新增输出上限字段

日期：2026-08-19
状态：已确认（brainstorming 产出）
范围：`src/views/Models.vue`、`src/lib/commands.ts`、`src-tauri/src/commands.rs`、`src-tauri/src/config/catalog.rs`、`src-tauri/src/lib.rs`

## 背景与动机

Models 页「新增/编辑模型」弹窗中的「上下文大小（token，目录共享值）」目前是裸 `NInputNumber`，用户要手打完整数字（如 200000），没有档位提示。同时目录里的 `output_size`（模型输出上限，OpenCode `limit.output` 同步依赖）只有 bundled 数据、没有用户编辑入口——自定义模型（不在目录中的 upstream_model_id）永远拿不到 output 上限，OpenCode 同步只能回退保守默认值（`OC_FALLBACK_OUTPUT`）。

本次改动两件事：

1. 上下文大小字段改为**可输入下拉**（静态常见档位 + tag 自由输入），标签改为「上下文大小（单位：B）」（单位维持 token 原义，仅文案）。
2. 新增「模型最大输出（单位：B）」字段，与上下文大小**完全同构**：同一目录（custom_provider_desc.json 稀疏覆盖）、同一读写命令模式、同一 UI 形式。

## 已确认的决策

- **单位**：目录值均为 token 数，标签里的"B"不改变语义，只改显示文案；下拉档位用**十进制口径**（128000 而非 131072），避免同屏两个"128K"。
- **交互形式**：`NSelect` + `filterable` + `tag` + `clearable`——与同表单「模型名称」字段一致；tag 允许输入目录外任意数字，clearable 保留「清空 = 回退目录默认」语义。
- **选项来源**：前端静态列表（不动态从目录去重、不加后端查询命令）。
- **档位**：上下文大小取目录高频档位；输出上限取 output 高频档位（见下）。

## 设计

### 1. 前端：Models.vue 表单

**上下文大小字段**（替换现有 `NInputNumber`）：

```
NFormItem label="上下文大小（单位：B）"
  NSelect
    v-model:value = form.context_size        // number | null 不变
    :options = CONTEXT_SIZE_OPTIONS
    filterable tag clearable
    placeholder="未识别"
```

静态档位（十进制口径）：

```ts
const CONTEXT_SIZE_OPTIONS = [
  { label: "128K (128000)", value: 128000 },
  { label: "200K (200000)", value: 200000 },
  { label: "256K (256000)", value: 256000 },
  { label: "1M (1000000)", value: 1000000 },
];
```

**新增输出上限字段**（同一表单、模型名称之后）：

```
NFormItem label="模型最大输出（单位：B）"
  NSelect —— 属性同上，placeholder="未收录"
```

静态档位（目录 output_size 高频值，十进制口径）：

```ts
const OUTPUT_SIZE_OPTIONS = [
  { label: "8K (8192)", value: 8192 },
  { label: "32K (32000)", value: 32000 },
  { label: "64K (64000)", value: 64000 },
  { label: "128K (128000)", value: 128000 },
  { label: "256K (256000)", value: 256000 },
];
```

**tag 输入解析**：NSelect 的 tag 输入产生字符串。用 `:on-update:value` 统一处理：选项点击给 number 直接透传；tag 字符串 `Number.parseInt`（`Number.isFinite` 校验，NaN 丢弃、不更新）。两个字段共用一个小工具函数。

**表单状态**：`FormState` 加 `output_size: number | null`；`blank()` 初始化为 null；`openEdit` 置 null（与 context_size 同样由 fetch 回填）。

**回填**：`fetchContextSize` 改名 `fetchCatalogSizes`，同一个 `[provider_id, upstream_model_id]` watch 触发，同时调用 `recognizedContextSize` 与 `recognizedOutputSize`，分别写入 `form.context_size` / `form.output_size` 与 `lastRecognized` / `lastRecognizedOutput`。

**保存**：`save()` 在现有 context 分支旁加 output 分支——`form.output_size !== lastRecognizedOutput` 时调 `setCustomOutputSize(provider_id, upstream, form.output_size)`；失败 `msg.warning` 不阻断（与 context 分支一致）。

### 2. 后端：命令

`commands.rs` 新增两个命令，与 context 版逐行同构：

```rust
#[tauri::command]
pub async fn recognized_output_size(
    state: State<'_, AppState>, provider_id: String, upstream_model_id: String,
) -> Result<Option<u32>, String>
// 实现：config 读锁解析 vendor → catalog 读锁 → catalog.output_size(&vendor, &upstream)

#[tauri::command]
pub async fn set_custom_output_size(
    state: State<'_, AppState>, app: tauri::AppHandle,
    provider_id: String, upstream_model_id: String, output_size: Option<u32>,
) -> Result<(), String>
// 实现：vendor 解析 → load_custom → custom.set_output_size → save_custom_catalog
//       → effective_catalog → *state.catalog.write() = effective
```

`lib.rs` 注册两个命令。

### 3. 后端：catalog.rs

`ModelDesc.context_size` 保持 `u32` 非 Option（不改 schema）。custom 稀疏文件中的哨兵约定：**`context_size == 0` 表示"该条目未覆盖 baseline 的 context"**（合法值恒 >0：bundled guard 断言 >0，UI 档位/手输均为正数）。

`ProviderCatalog` 新增 `set_output_size(&mut self, vendor, upstream_model_id, size: Option<u32>)`：

- `Some(n)`：条目存在则改 `output_size`；不存在则新建条目（`context_size: 0` 哨兵 + `output_size: Some(n)`）。provider 组不存在则建组。与 `set_context_size` 新建条目时 `output_size: None`（= 不覆盖 baseline output）对称。
- `None`（清除覆盖）：条目存在则 `output_size = None`（serde `skip_serializing_if` 自动从磁盘去掉该键）；**若条目 `context_size == 0`（context 也无覆盖）则删整条**（组空则删组）。不存在条目则 no-op。

**`overlay_custom` 增加哨兵规则**：custom 条目 `context_size == 0` 时保留 baseline 条目的 `context_size`（不覆盖）；若 baseline 无该条目则按 0 原样合并（此时 effective `context_size()` 返回 `Some(0)`——见下方 `context_size()` 语义修正）。

**`context_size()` 语义修正**：返回值 0 视同未收录——当条目存在但 `context_size == 0` 时返回 `None`。理由：一个只设了 output 覆盖、不在 baseline 中的自定义模型，其 context 本就未知；暴露 `Some(0)` 会被 `classify_fallback`（`fs < ps` 恒真，误判 Smaller）和 `resolve_limits`（把 0 写进 OpenCode limit.context）当真实值用。调用方（commands.rs:692/1375、agent_sync.rs:214）无需改动，自动获得修正后语义。

**同步修正 `set_context_size` 的 None 分支**：现状"清 context = 删整条"会把同条目上还存在的 output 覆盖一起丢掉。改为：清 context 时若该条目 `output_size.is_some()` 则置 `context_size = 0`（哨兵）保留条目，否则删整条。与 `set_output_size` 对称。

### 4. 不改的

- `agent_sync.rs`：读 effective catalog，自动获得新覆盖值，OpenCode limit.output 同步无需改动。
- `Model` 结构（`config/types.rs`）：output 仍只在目录，不进 app_config.json。
- `Settings.vue` / `Fallback.vue` 的"上下文大小"文案（只读展示，语义无歧义）。
- 目录 bundled 数据本身（provider_desc.json 不动）。

## 错误处理

- `setCustomOutputSize` 失败：`msg.warning("模型最大输出保存失败：...")`，不阻断保存流程（与 context 一致）。
- `recognizedOutputSize` 失败（如 provider 不存在）：字段置 null、占位符显示，不报错（与现状 `fetchContextSize` 的 catch 一致）。
- tag 输入非数字：静默忽略（不更新值）。
- custom 文件损坏：`load_custom` 现有行为（warn + 空 catalog）不变。

## 测试

**Rust（`catalog.rs` tests）**：

- `set_output_size` Some：baseline 已有条目 → 覆盖 output（context 不动）；无条目 → 新建（context=0 哨兵）。
- `set_output_size` None：条目 context=0 哨兵 → 删整条；条目 context 有覆盖 → 仅清 output 保留条目。
- `set_context_size` None：条目有 output 覆盖 → 置 context=0 保留条目；无 → 删整条（现有测试 `set_context_size_adds_updates_removes` 需相应更新：其条目无 output 覆盖，行为不变）。
- `overlay_custom`：custom 条目 context=0 → baseline context 保留；context=0 且 baseline 无该条目 → 合并且 `context_size()` 返回 None。
- `context_size()`：条目存在但值为 0 → 返回 None。
- `commands.rs`：两个新命令的 round-trip（临时目录 + 内存 state，参照 `set_custom_context_size` 的测试模式；若无现成模式则补最小 set→recognized 往返）。

**前端**：

- `pnpm exec vue-tsc --noEmit` 类型检查。
- 人工验证：新增模型（选档位/手输/清空）、编辑已有模型回填、自定义模型 output 覆盖后 OpenCode 同步生效（若开启同步）。

## 实施顺序（供 plan 参考）

1. `catalog.rs`：`set_output_size` + `overlay_custom` 哨兵规则 + `set_context_size` None 分支修正 + 测试
2. `commands.rs` + `lib.rs`：两个新命令 + 注册 + 测试
3. `commands.ts`：两个 wrapper
4. `Models.vue`：两字段 UI + 解析函数 + fetch/save 扩展
5. 全量验证：`cargo test` + `pnpm exec vue-tsc --noEmit`
