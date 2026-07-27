# Linux 平台支持 — 设计文档

- **日期**: 2026-08-01
- **目标**: 让 SwitchLM（原本只在 Windows 开发/运行）能在 Linux 上构建、运行，并产出可分发的 `.deb` / `.rpm` 安装包；同时为"未安装系统密钥环"的 Linux 环境提供**经用户授权的明文密钥回退存储**。

## 1. 背景与现状

SwitchLM 是 Tauri v2（Rust 后端 + Vue 3 前端）的桌面/托盘应用。经代码审查，**代码库本身已约 95% 跨平台**：

- Tauri v2 及所用插件（`tauri-plugin-autostart` / `single-instance` / `opener`、`tray-icon` feature）均跨平台。
- 代理 / 协议互译 / 套餐用量代码是纯 Rust，无 OS 依赖。
- 日志模块已存在 `#[cfg(unix)]` 路径；`app_data_dir()` 在 Linux 上落到 `~/.local/share/com.switchlm.app`（Tauri 用 bundle identifier，非 productName）。
- 无任何 Windows 专属 crate（`windows` / `winapi` 等）被引入。
- `main.rs` 的 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` 在 Linux 上是 no-op，无害。

**唯一的真正阻塞点**是 `src-tauri/Cargo.toml` 中 keyring 后端被硬编码为 Windows：

```toml
keyring = { version = "3", features = ["windows-native"] }
```

在 Linux 上该 feature 无法提供后端，`Entry::new()` / 读写会在运行期返回 `NoBackendAccess` 或 D-Bus 错误，导致**推理密钥（api_key）与火山用量 SK 静默丢失**（编译可通过，但密钥存不进去）。其余皆为运行期系统依赖与文档问题。

## 2. 目标 / 非目标

**目标**

1. 项目在 Linux 上可 `npm run tauri dev` 开发、`npm run tauri build` 构建。
2. 产出 `.deb` 与 `.rpm` 安装包，且**声明运行期依赖**使其安装时自动拉取。
3. README 补充 Linux 前置依赖（apt/dnf/pacman）与运行注意事项（托盘 / Wayland / 密钥环）。
4. 当系统未提供密钥环后端时，**首次启动弹出授权对话框**：用户同意则将访问密钥以明文保存到本地文件并继续运行；不同意则退出。

**非目标（明确排除）**

- **macOS 支持**：本设计的 keyring cfg 改动会让 macOS *可编译*（顺带加 `apple-native`），但托盘行为、`.app` 打包、签名均**未经测试**，不作为本任务交付，列为后续单独任务。
- **AppImage**：本期不产出（用户明确选择 deb + rpm）。
- **无桌面环境的服务器 / headless 部署**：不在范围（这是桌面托盘应用）。
- **密钥加密**：明文回退存储是用户**显式同意**的降级方案，不做客户端加密（YAGNI；需要加密就装密钥环）。

## 3. 总体方案

四处改动，按风险/价值排序：

| # | 改动 | 文件 | 性质 |
|---|---|---|---|
| 1 | keyring 后端按 OS 条件化 | `src-tauri/Cargo.toml` | 阻塞性，必做 |
| 2 | 密钥存储后端选择 + 授权流程 + `FileSecretStore` | `config/secrets.rs`、`config/store.rs`、`config/types.rs`、`lib.rs`、`commands.rs`、前端 | 新功能 |
| 3 | 声明 deb/rpm 运行期依赖 | `src-tauri/tauri.conf.json` | 配置 |
| 4 | Linux 前置依赖 + 注意事项文档 | `README.md` | 文档 |

## 4. 详细设计

### 4.1 keyring 后端条件化（`Cargo.toml`）

将单行依赖拆分为"基础版本 + 按目标 OS 叠加后端 feature"，用 Cargo 的 target 段合并语义：

```toml
[dependencies]
keyring = "3"

[target.'cfg(target_os = "windows")'.dependencies]
keyring = { version = "3", features = ["windows-native"] }

[target.'cfg(target_os = "macos")'.dependencies]
keyring = { version = "3", features = ["apple-native"] }

[target.'cfg(target_os = "linux")'.dependencies]
keyring = { version = "3", features = ["linux-native-sync-persistent"] }
```

- `linux-native-sync-persistent` 使用 `dbus-secret-service`（纯 Rust 的 Secret Service D-Bus 客户端），**仅需 `libdbus-1`**（构建与运行期），不依赖 libsecret 的 C 绑定。它是 keyring v3 官方推荐的 Linux 后端。
- macOS 行加 `apple-native` 是顺带的一行，使未来 macOS 支持零成本起步（本期不验证）。
- 现有测试 `keyring_store_roundtrip_real`（在装了密钥环的 Linux 上）将真正可跑通；该测试的提示注释已提到此处。

### 4.2 密钥环缺失时的授权回退（核心新功能）

#### 4.2.1 三个 `SecretStore` 实现

现有 trait `SecretStore`（`config/secrets.rs`）已有 `KeyringStore`（OS 钥匙串）与 `MemoryStore`（测试用）。新增两个实现：

1. **`FileSecretStore`** — 明文文件后端，持久化到 `<app_data>/secrets.json`，与 `app_config.json` 同级。
   - 文件 schema：JSON 对象，键与 keyring 条目完全一致 —— 推理密钥用 `provider_id`，火山用量 SK 用 `provider_id::usage_sk`（复用现有 `usage_sk_entry` 命名），保证与 `KeyringStore` 语义 1:1。
     ```json
     { "glm-account": "sk-...", "glm-account::usage_sk": "ABCdef..." }
     ```
   - **原子写**：写临时文件 + `rename`，避免崩溃导致半写文件。
   - **权限**：Unix 下以 `0600` 创建（仅属主可读写）；Windows 下不特别处理（依赖 NTFS 用户隔离）。
   - **构造与可变性**：`SecretStore` trait 方法取 `&self`（非 `&mut self`），故内部用 `Mutex<HashMap<String,String>>` 做 read-modify-write；构造时传入 `<app_data>` 目录路径（`FileSecretStore::new(dir)`），路径在启动期确定、运行期不变。
   - 全部 `Result` 返回，文件 I/O 错误经 `SecretError::Io` 暴露。

2. **`PendingStore`** — "等待授权"占位后端。**写操作**（`set_*` / `delete_*`）返回 `Err(SecretError::PendingConsent)`；**读操作**（`get_*`）返回 `Ok(None)`（见 §4.2.6：授权门禁只针对写入）。这样前端在授权窗口期内能正常渲染空状态（`provider_has_key` 返回 false，不会冒硬错误），而保存密钥会得到清晰错误、引导先完成授权。

#### 4.2.2 启动期后端选择（`lib.rs` setup）

引入一个**可在运行期切换**的句柄，以便用户授权后从 `PendingStore` 切换到 `FileSecretStore`（无需重启）：

- 新增 `SecretStoreHandle`，内部持 `Mutex<Arc<dyn SecretStore>>`，自身实现 `SecretStore`（委托给当前值），并提供 `swap(new)`。这样 `AppStateInner.secrets` 的调用点（`state.secrets.set_key(...)` 等）无需改动签名语义。
- 启动期探测 + 选择逻辑（纯函数 `select_backend`，可单测）：

  ```
  probe_ok = keyring_probe()        # set→get→delete 一个 switchlm_probe 临时条目
  granted  = cfg.settings.secret_store_fallback   # Option<bool>

  if probe_ok:                       backend = KeyringStore;        consent_required = false
  else if granted == Some(true):     backend = FileSecretStore;    consent_required = false
  else:                              backend = PendingStore;        consent_required = true
  ```

- `keyring_probe()`：写一个名为 `switchlm_probe` 的临时条目，读回，再删除。能区分 `NoBackendAccess`（无 feature）与 D-Bus 错误（有 feature 但无 secret-service 守护进程）；任一写入失败即判定不可用。成功时清理临时条目。
  - **锁定的 keyring**：Linux 上若 gnome-keyring 处于锁定态，探测的 set→get→delete 可能触发系统解锁弹窗或直接失败；任一情况都按"探测失败"处理、落入授权流程（安全默认），用户授权后改用文件后端即可。

- **`declined` 不持久化**：用户在对话框点"退出"只是当次 `quit_app`；下次启动仍会再次弹出授权（避免把应用"锁死"，让用户有机会改主意）。因此 `secret_store_fallback` 只有两个状态：`None`（每次询问）或 `Some(true)`（记住授权、不再问）。

#### 4.2.3 `Settings` 字段（`config/types.rs`）

`Settings` 新增：

```rust
/// Linux 等无系统密钥环的环境下，用户是否已授权将密钥以明文存入 secrets.json。
/// None=未授权(每次启动询问)；Some(true)=已授权(记住)。
#[serde(default, skip_serializing_if = "Option::is_none")]
pub secret_store_fallback: Option<bool>,
```

`#[serde(default)]` 保证旧配置（Windows 历史 config）兼容，字段缺省即 `None`。

#### 4.2.4 前端授权对话框

- 新增只读命令 `get_secret_status() -> { mode: "keyring" | "file" | "pending", consent_required: bool }`（或并入既有启动状态查询）。`mode` 与 `consent_required` 直接由 `SecretStoreHandle` 当前持有的后端类型推导（句柄知道自己持有 `KeyringStore` / `FileSecretStore` / `PendingStore`，后者即 `consent_required=true`），无需额外状态字段；swap 到 `FileSecretStore` 后，下一次轮询自然反映为 `file`。
- 新增命令 `grant_secret_consent() -> ()`：
  1. 置 `cfg.settings.secret_store_fallback = Some(true)` 并保存配置。
  2. 把句柄从 `PendingStore` **swap** 为 `FileSecretStore`。
  3. 补跑被推迟的 `migrate_usage_sk_to_keyring`（见下）。
  4. 刷新前端状态（`consent_required` 由句柄派生，第 2 步的 swap 已使其翻转为 false，无需单独清除）。
- 前端（Pinia `system` store + 一个 Naive UI `NModal`）：应用初始化时读 `get_secret_status()`；若 `consent_required`，弹出模态框：

  > **未检测到系统密钥环**（如 gnome-keyring / KWallet / seahorse-daemon）。
  > 是否同意将访问密钥以**明文**保存到本地文件 `secrets.json`？
  > （明文存储存在泄露风险，建议安装密钥环以获得更安全的存储。）
  > [ 同意并继续 ]   [ 退出 ]

  - 点"同意并继续" → 调 `grant_secret_consent()`，关闭模态框，正常使用。
  - 点"退出" → 调 `quit_app`。

- 文案中文，符合现有 UI 语言约定。

#### 4.2.5 与现有 `migrate_usage_sk_to_keyring` 的交互

启动期的明文 SK 迁移（把旧 `app_config.json` 里的 plaintext SK 搬进密钥存储）需在后端就绪后执行：

- `KeyringStore` 或 `FileSecretStore`（已授权）→ 启动期立即迁移（行为不变）。
- `PendingStore`（未授权）→ **推迟迁移**，避免在授权前向 `secrets.json` 写入；待 `grant_secret_consent()` swap 成 `FileSecretStore` 后补跑。

> 说明：选择"运行期 swap 句柄"而非"在写命令里加 consent 门禁"，正是为了让 `PendingStore` 自然拒绝一切写入（含迁移），避免在多个命令里散落 consent 检查，也避免迁移在授权前泄写。

#### 4.2.6 读语义

授权门禁**只针对写入**。`provider_has_key` / 读操作在 `PendingStore` 下返回 `None`（无密钥），不报错；这允许前端在授权前正常渲染、展示空状态。

### 4.3 打包：声明运行期依赖（`tauri.conf.json`）

`bundle.targets` 保持 `"all"`（这样 Windows 的 MSI/NSIS 不受影响）；Linux 下用 `--bundles deb,rpm` 跳过 AppImage（见文档）。新增 `bundle.linux` 声明 deb/rpm 运行期依赖，使安装时自动拉取：

```jsonc
"bundle": {
  // …existing…
  "linux": {
    "deb": {
      "depends": ["libwebkit2gtk-4.1-0", "libayatana-appindicator3-1", "libdbus-1-3"]
    },
    "rpm": {
      "depends": ["webkit2gtk4.1", "libayatana-appindicator", "dbus-libs"]
    }
  }
}
```

- **字段路径已校验**：`bundle.linux.deb.depends` 与 `bundle.linux.rpm.depends` 均存在且类型为 `Option<Vec<String>>`（RPM 用 `depends`，**非** `requires`）—— 已对照本仓库 `Cargo.lock` 锁定的 `tauri-utils v2.9.3` 源码 `LinuxConfig`/`DebConfig`/`RpmConfig` 确认。实现计划阶段仅需在实机构建时确认**各发行版的候选包名**（如 Debian `libwebkit2gtk-4.1-0` vs Fedora `webkit2gtk4.1`）。
- 这些是 Tauri 在 Linux 上的硬性运行期库：WebKitGTK（WebView）、appindicator（系统托盘）、libdbus（keyring Secret Service）。

### 4.4 文档（`README.md`）

1. **前置依赖（Linux）** 小节，给出 apt / dnf / pacman 一键安装命令，覆盖构建期 `-dev` 包：
   `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev patchelf`（构建还需 `libsoup-3`、`javascriptcore` 等随 webkit2gtk 带入；Node ≥ 20、Rust stable 同 Windows）。
2. 构建/运行命令同 Windows，额外说明 `npm run tauri build -- --bundles deb,rpm` 产出 `.deb`/`.rpm`。
3. **运行注意事项**：
   - **托盘**：GNOME（尤其 Wayland）默认无传统托盘，需安装 *AppIndicator and KStatusNotifierItem Support* 扩展；无托盘的环境下关闭窗口后将隐藏，只能再次启动应用（单实例插件会聚焦既有窗口）恢复。
   - **密钥环**：建议运行 gnome-keyring / KWallet；未安装时首次启动会弹出明文存储授权对话框（见 4.2）。
   - **不支持 headless/无桌面服务器**。

## 5. 错误处理

| 场景 | 行为 |
|---|---|
| keyring 探测成功 | 用 `KeyringStore`，无授权流程 |
| keyring 探测失败、已授权 | 用 `FileSecretStore`，无授权流程 |
| keyring 探测失败、未授权 | `PendingStore`，前端弹授权框；点退出 → `quit_app`。**代理仍会启动**：`PendingStore` 读一律返回 `None`，无任何密钥可泄露；已配置的 Profile 因取不到密钥而对请求 fail-closed 报错，用户授权并重新录入密钥后即恢复。授权针对的是**密钥存储**，不是"应用能否运行"，故无需暂停代理。点退出则连同代理任务一并 teardown |
| `FileSecretStore` 文件 I/O 失败 | 经 `SecretError::Io` 返回；写命令把错误透传给前端 |
| `secrets.json` 缺失或为空 | 视为空存储（非错误），直接返回 `Ok(None)`；首次写入时创建 |
| `secrets.json` 损坏（非法 JSON） | **先重命名隔离为 `secrets.json.bak`（保留救援机会）**，再视为空存储（不崩溃）、日志告警；后续写入重建文件。原子写（临时文件 + `rename`）保证读者永远不会读到半写文件 |
| keyring 此前可用后变为不可用 | 下次启动探测失败 → 重新弹授权；原 keyring 里的密钥读不到，用户需重新录入 |

**并发写入**：进程内 `Mutex` 保证单进程 read-modify-write 串行化；release 由 `single-instance` 插件保证全局唯一实例，故无需跨进程文件锁。唯一可能的两进程竞争发生在 dev 多实例模式（`debug_assertions`，已被设计为可接受端口竞争的 racy 模式）下，且密钥写入是低频配置操作 —— 不为此引入跨进程锁（YAGNI）。

## 6. 测试

**后端单测（`#[cfg(test)]`）**

- `FileSecretStore`：set/get/delete 往返；`usage_sk` 与 `api_key` 不互相覆盖（迁移自 `secrets.rs` 现有 keyring 测试）；原子写（中途不产生半文件）；Unix 下文件权限为 `0600`（`#[cfg(unix)]` 用 `PermissionsExt` 断言）。
- `select_backend(keyring_ok, granted)` 纯函数：三分支（keyring→KeyringStore；fail+granted→FileSecretStore；fail+pending→PendingStore）。
- 既有 `keyring_store_roundtrip_real` 保留，但**改为 `#[ignore]`**：它打真实 keyring（经 D-Bus 连 Secret Service），在无桌面 / Docker / CI 这类无 session bus 的环境里会阻塞或失败。`select_backend` 是纯函数、不触 D-Bus，照常纳入普通 `cargo test`。需要时用 `cargo test -- --ignored` 单独跑真实 keyring 用例（本机装了 gnome-keyring 时）。

> **无桌面构建环境说明**：`linux-native-sync-persistent` 依赖 `dbus-secret-service`，**编译**只需要 `libdbus-1` 头/库（apt: `libdbus-1-dev`），**不需要**运行中的 D-Bus；只有"真实 keyring"类用例在**运行**时才需要 session bus。因此 `cargo test`（排除 ignored）与 `cargo build` 在 headless 容器内可正常通过。

**集成/手测**

- `cargo test --manifest-path src-tauri/Cargo.toml`（Linux）全绿。
- `npm run tauri dev`：应用启动、托盘出现、可加 Provider+密钥（有密钥环）；或弹出授权框（无密钥环，可临时停用 gnome-keyring 验证）。
- `npm run tauri build -- --bundles deb,rpm`：在 `src-tauri/target/release/bundle/` 下产出 `.deb` 与 `.rpm`。
- 安装产出的包：依赖被自动拉取、应用可从应用菜单启动。

## 7. 安全考量

- 明文 `secrets.json` 是相对 OS 钥匙串的**安全降级**，但它是**用户显式同意**的结果，对话框中明示"明文存储存在泄露风险、建议安装密钥环"。
- 文件 `0600` 权限（Unix）限制为属主可读。
- `app_config.json` **不含新写入的密钥**（仅布尔存在性与新增的授权标记 `secret_store_fallback`，标记本身非敏感）。唯一例外：从旧版继承的遗留明文 usage SK，在授权前的 pending 窗口期仍留在 `app_config.json`；授权后由 `grant_secret_consent` 触发的推迟迁移立即搬入 `secrets.json`，窗口随之关闭。
- 不引入客户端加密：需要更高安全性即安装密钥环，避免给明文回退路径制造"看起来安全"的假象。

## 8. 后续（非本期）

- **macOS**：托盘（NSStatusItem 左右键语义、`.app` bundle）、签名与公证 —— 单独任务。
- **AppImage**：如需单文件分发，再加 target。
- **CI**：如建立 GitHub Actions，可加 Linux 矩阵构建 deb/rpm。

## 9. 变更清单（落地用）

- `src-tauri/Cargo.toml` — keyring 拆分为 OS 条件后端（+ macos 一行）。
- `src-tauri/src/config/secrets.rs` — `FileSecretStore`、`PendingStore`、`SecretStoreHandle`、`keyring_probe()`、`select_backend()`；并为 `SecretError` 增加 `Io` 与 `PendingConsent` 变体（§4.2.1/§5 所引用）。
- `src-tauri/src/config/types.rs` — `Settings.secret_store_fallback`。
- `src-tauri/src/config/store.rs` — 迁移在 pending 时推迟（`grant_secret_consent` 内补跑）。
- `src-tauri/src/lib.rs` — 启动期探测 + 后端选择；`AppStateInner.secrets` 改为可切换句柄。
- `src-tauri/src/commands.rs` — `get_secret_status`、`grant_secret_consent` 命令；注册到 `invoke_handler`。
- `src-tauri/capabilities/default.json` — 视命令需要补充权限（通常无需，命令走默认 core 权限）。
- `src/lib/commands.ts` / `src/lib/types.ts` — 新命令的 TS 包装与类型镜像。
- 前端 store + 组件 — 授权状态拉取 + `NModal` 对话框。
- `src-tauri/tauri.conf.json` — `bundle.linux.deb/rpm.depends`。
- `README.md` — Linux 前置依赖 + 构建命令 + 运行注意事项。
