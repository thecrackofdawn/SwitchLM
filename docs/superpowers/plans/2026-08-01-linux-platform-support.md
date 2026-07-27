# Linux 平台支持 Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 SwitchLM 在 Linux 上可构建、运行，产出 `.deb`/`.rpm`，并为无系统密钥环的环境提供经用户授权的 `secrets.json` 明文回退存储。

**Architecture:** keyring 后端按 OS 条件化（Linux → `linux-native-sync-persistent`）。新增三个 `SecretStore` 实现（`FileSecretStore`、`PendingStore`）与一个可在运行期切换的 `SecretStoreHandle`；启动期探测 keyring 并据此选择后端，未授权时前端弹模态框，同意后 swap 到文件后端并补跑迁移。打包声明 deb/rpm 运行期依赖；README 补 Linux 前置依赖与注意事项。

**Spec:** `docs/superpowers/specs/2026-08-01-linux-platform-support-design.md`

**Tech Stack:** Rust（axum/tokio 已有）、`keyring` v3、Tauri v2、Vue 3 + Pinia + Naive UI。

**Testing realities (read first):**
- 后端用 `cargo test`（TDD：先红后绿）。真实 keyring 用例（连 D-Bus）标 `#[ignore]`，`cargo test` 默认不跑，避免无桌面环境失败。
- 前端**无单元测试框架**（仅 `vue-tsc --noEmit` 类型检查）。前端任务的"验证"= `npx vue-tsc --noEmit` 通过 + `npm run tauri dev` 手动冒烟，不是断言测试。
- 本机此前从未 `cargo build` 过（无 target/），首个 Rust 任务会触发依赖下载+编译（数分钟），属正常。

---

## File Structure

| 文件 | 职责 | 改动 |
|---|---|---|
| `src-tauri/Cargo.toml` | keyring 后端按 OS 条件化 | Modify |
| `src-tauri/src/config/secrets.rs` | `SecretError` 变体；`FileSecretStore`、`PendingStore`、`SecretStoreHandle`、`BackendKind`、`select_backend`、`make_store`、`keyring_available` | Modify(+add) |
| `src-tauri/src/config/types.rs` | `Settings.secret_store_fallback` 字段 | Modify |
| `src-tauri/src/config/mod.rs` | re-export 新增类型 | Modify |
| `src-tauri/src/config/store.rs` | 迁移签名兼容 handle（`&dyn SecretStore` 不变） | (基本不变) |
| `src-tauri/src/proxy/state.rs` | `AppStateInner.secrets` 字段类型 → `SecretStoreHandle`；`load()` 包裹 | Modify |
| `src-tauri/src/proxy/dispatch.rs` | 5 处测试构造点（2 字面量 + 3 `let` 绑定）改用 handle | Modify(test) |
| `src-tauri/src/lib.rs` | 启动期探测+选择+handle 构造+迁移推迟 | Modify |
| `src-tauri/src/commands.rs` | `get_secret_status`、`grant_secret_consent` 命令 | Modify(+add) |
| `src-tauri/tauri.conf.json` | `bundle.linux.deb/rpm.depends` | Modify |
| `src/lib/types.ts` | `SecretStatusView` 镜像 | Modify |
| `src/lib/commands.ts` | `getSecretStatus` / `grantSecretConsent` 包装 | Modify |
| `src/stores/system.ts` | `secretStatus` + `loadSecretStatus` + `grantConsent` | Modify |
| `src/App.vue` | 全局授权 `NModal` | Modify |
| `README.md` | Linux 前置依赖 + 注意事项 | Modify |

**职责边界说明：** `secrets.rs` 承担所有"密钥存储后端"逻辑（一个文件，单职责）；`SecretStoreHandle` 的"切换/派生 mode"是与存储正交的状态管理，但它必须 impl `SecretStore` 才能透明替换字段，故与后端同文件（紧耦合、一起变更）。其余文件只做"装配/调用"，不含存储逻辑。

---

## Chunk 1: 密钥存储后端（Rust 库，TDD）

纯库代码，无 Tauri 依赖，完全可单测。这是地基。

### Task 1.1: keyring 后端按 OS 条件化

**Files:**
- Modify: `src-tauri/Cargo.toml`（依赖区，当前为单行 `keyring = { version = "3", features = ["windows-native"] }`）

- [ ] **Step 1: 把 keyring 后端按目标 OS 拆分（注意放置位置！）**

keyring 当前位于 `[dependencies]` 块**中部**（`tracing-subscriber` 之后、`eventsource-stream` 之前），其后还有 8 个**通用**依赖（`eventsource-stream` … `fastrand`）。**切勿就地整段替换**：TOML 中若把三个 `[target...dependencies]` 段插在中部，会把其后直到下一节之间的通用依赖错算进最后一个 target 段（Linux），导致它们在 Windows/macOS 上被排除——这是**隐蔽回归**（`cargo metadata` 查不出、Linux 构建也照常通过）。正确做法是两步：

1. 把现有那一行 `keyring = { version = "3", features = ["windows-native"] }`（约第 30 行）改为只剩基础版本，**留在通用块内**：
   ```toml
   keyring = "3"
   ```
2. 在通用依赖块**末尾**（`fastrand = "2"` 之后、空行 + `[dev-dependencies]` 之前）追加三个 target 段：
   ```toml

   # 后端按目标 OS 叠加 feature：基础版本见上，每个 target 段补一个原生后端。
   # Linux 用 linux-native-sync-persistent（dbus-secret-service，纯 Rust，仅需 libdbus-1）。
   # macOS 行顺带启用，本期不验证（见 spec 非目标）。
   [target.'cfg(target_os = "windows")'.dependencies]
   keyring = { version = "3", features = ["windows-native"] }

   [target.'cfg(target_os = "macos")'.dependencies]
   keyring = { version = "3", features = ["apple-native"] }

   [target.'cfg(target_os = "linux")'.dependencies]
   keyring = { version = "3", features = ["linux-native-sync-persistent"] }
   ```

- [ ] **Step 2: 校验编译（仅检查解析，不跑完整 build）**

Run: `cargo metadata --manifest-path src-tauri/Cargo.toml --no-deps > /dev/null && echo OK`
Expected: `OK`（确认 manifest 解析通过、target 段语法正确）。完整编译留到 Task 1.3 一起验证。

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml
git commit -m "build(linux): keyring 后端按目标 OS 条件化"
```

### Task 1.2: `SecretError` 新增变体（TDD）

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`（顶部 `SecretError` enum，当前只有 `Keyring(String)`）

- [ ] **Step 1: 写失败测试**（加到 `secrets.rs` 末尾 `#[cfg(test)] mod tests` 内）

```rust
    #[test]
    fn secret_error_variants_format() {
        // FileSecretStore 的 I/O 错误与 PendingStore 的未授权错误各有独立变体，
        // 便于上层按错误类型给出清晰提示（而非一律 "keyring error"）。
        assert_eq!(SecretError::Io("denied".into()).to_string(), "io error: denied");
        assert_eq!(
            SecretError::PendingConsent.to_string(),
            "secret storage consent not granted"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml secret_error_variants_format`
Expected: 编译失败（`Io` / `PendingConsent` 不存在）。

- [ ] **Step 3: 扩展 enum**

把 `SecretError` 改为：

```rust
#[derive(Debug, Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("secret storage consent not granted")]
    PendingConsent,
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml secret_error_variants_format`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): SecretError 增加 Io/PendingConsent 变体"
```

### Task 1.3: `FileSecretStore`（TDD）

明文文件后端，持久化到 `<app_data>/secrets.json`。原子写 + Unix `0600` + 损坏隔离 `.bak`。

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`（在 `MemoryStore` 之后加 impl + 测试）
- Modify: `src-tauri/src/config/secrets.rs` 顶部 `use` 加 `use std::path::Path;`

- [ ] **Step 1: 写失败测试**（加到 `mod tests`）

```rust
    use std::fs;

    fn tmp_store() -> (tempfile::TempDir, FileSecretStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn file_store_roundtrip() {
        let (_dir, s) = tmp_store();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
        s.set_key("zhipu", "sk-abc").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), Some("sk-abc".into()));
        s.delete_key("zhipu").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
    }

    #[test]
    fn file_store_usage_sk_distinct_from_api_key() {
        let (_dir, s) = tmp_store();
        s.set_key("volc", "sk-inference").unwrap();
        s.set_usage_sk("volc", "secret-access-key").unwrap();
        assert_eq!(s.get_key("volc").unwrap(), Some("sk-inference".into()));
        assert_eq!(s.get_usage_sk("volc").unwrap(), Some("secret-access-key".into()));
        s.delete_key("volc").unwrap();
        assert_eq!(s.get_key("volc").unwrap(), None);
        assert_eq!(s.get_usage_sk("volc").unwrap(), Some("secret-access-key".into()));
    }

    #[test]
    fn file_store_missing_or_empty_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        // 文件不存在
        let s = FileSecretStore::new(dir.path()).unwrap();
        assert_eq!(s.get_key("x").unwrap(), None);
        // 空文件
        fs::write(dir.path().join("secrets.json"), "").unwrap();
        let s2 = FileSecretStore::new(dir.path()).unwrap();
        assert_eq!(s2.get_key("x").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_creates_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path()).unwrap();
        s.set_key("a", "b").unwrap();
        let perms = fs::metadata(dir.path().join("secrets.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(perms, 0o600, "secrets.json must be 0600 on unix");
    }

    #[test]
    fn file_store_corrupt_file_quarantined_to_bak() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secrets.json"), "{ not valid json").unwrap();
        // 加载：损坏文件被隔离为 .bak，store 视为空、不崩溃
        let s = FileSecretStore::new(dir.path()).unwrap();
        assert_eq!(s.get_key("x").unwrap(), None);
        assert!(dir.path().join("secrets.json.bak").exists(), "corrupt file quarantined");
    }

    #[test]
    fn file_store_atomic_no_half_file_on_read() {
        // 原子写（临时文件 + rename）：读者只看到完整的 secrets.json，永不会读到半写。
        let (_dir, s) = tmp_store();
        s.set_key("a", "1").unwrap();
        s.set_key("b", "2").unwrap();
        // 两次写后文件仍是合法 JSON 且两条都在
        let raw = fs::read_to_string(tmp_store_path(&_dir)).unwrap();
        assert!(raw.contains("\"a\"") && raw.contains("\"b\""));
    }

    fn tmp_store_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("secrets.json")
    }
```

> 注：`file_store_atomic_no_half_file_on_read` 是弱断言（真正验证原子性需并发/中断，超出单测范围）；这里只保证多写后文件一致。原子性由"写 `.tmp` + `rename`"实现保证。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml file_store`
Expected: 编译失败（`FileSecretStore` 不存在）。

- [ ] **Step 3: 实现 `FileSecretStore`**

在 `MemoryStore` 之后加：

```rust
/// 明文文件密钥后端（无系统密钥环时的用户授权回退）。键命名与 `KeyringStore` 1:1
/// （`provider_id` 与 `provider_id::usage_sk`），故二者语义等价、可互换。
/// 文件：<dir>/secrets.json，原子写（.tmp + rename），Unix 下 0600。
pub struct FileSecretStore {
    path: std::path::PathBuf,
    inner: Mutex<HashMap<String, String>>,
}

impl FileSecretStore {
    /// 加载 `<dir>/secrets.json`。缺失/空文件 → 空存储；损坏 → 隔离为 `.bak` 后置空。
    pub fn new(dir: &Path) -> Result<Self, SecretError> {
        let path = dir.join("secrets.json");
        let mut inner = HashMap::new();
        match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => {}
            Ok(s) => match serde_json::from_str::<HashMap<String, String>>(&s) {
                Ok(map) => inner = map,
                Err(_) => {
                    // 损坏：隔离原文为 .bak（保留救援机会），再以空存储继续。
                    let _ = std::fs::rename(&path, dir.join("secrets.json.bak"));
                    tracing::warn!("secrets.json 解析失败，已隔离为 secrets.json.bak 并重置为空");
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SecretError::Io(e.to_string())),
        }
        Ok(Self { path, inner: Mutex::new(inner) })
    }

    fn flush(&self, map: &HashMap<String, String>) -> Result<(), SecretError> {
        let dir = self.path.parent().expect("secrets.json has a parent dir");
        let tmp = dir.join("secrets.json.tmp");
        let bytes = serde_json::to_vec(map).map_err(|e| SecretError::Io(e.to_string()))?;
        std::fs::write(&tmp, bytes).map_err(|e| SecretError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| SecretError::Io(e.to_string()))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| SecretError::Io(e.to_string()))?;
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.insert(provider_id.into(), key.into());
        self.flush(&map)
    }
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(provider_id).cloned())
    }
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.remove(provider_id);
        self.flush(&map)
    }
    fn set_usage_sk(&self, provider_id: &str, sk: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.insert(usage_sk_entry(provider_id), sk.into());
        self.flush(&map)
    }
    fn get_usage_sk(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(&usage_sk_entry(provider_id)).cloned())
    }
    fn delete_usage_sk(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.remove(&usage_sk_entry(provider_id));
        self.flush(&map)
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml file_store`
Expected: 6 个用例全 PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): FileSecretStore 明文文件后端（原子写/0600/损坏隔离）"
```

### Task 1.4: `PendingStore`（TDD）

未授权占位后端：写返回 `Err(PendingConsent)`，读返回 `Ok(None)`。

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn pending_store_reads_none_writes_err() {
        let s = PendingStore;
        assert_eq!(s.get_key("a").unwrap(), None);          // 读不报错
        assert_eq!(s.get_usage_sk("a").unwrap(), None);
        assert!(matches!(s.set_key("a", "x"), Err(SecretError::PendingConsent)));  // 写拒绝
        assert!(matches!(s.set_usage_sk("a", "x"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.delete_key("a"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.delete_usage_sk("a"), Err(SecretError::PendingConsent)));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml pending_store`
Expected: 编译失败（`PendingStore` 不存在）。

- [ ] **Step 3: 实现 `PendingStore`**

```rust
/// "等待授权"占位后端：写一律返回 `PendingConsent`（引导用户先完成授权），
/// 读一律返回 `Ok(None)`（让前端在授权窗口期内正常渲染空状态，见 spec §4.2.6）。
pub struct PendingStore;

impl SecretStore for PendingStore {
    fn set_key(&self, _: &str, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn get_key(&self, _: &str) -> Result<Option<String>, SecretError> { Ok(None) }
    fn delete_key(&self, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn set_usage_sk(&self, _: &str, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn get_usage_sk(&self, _: &str) -> Result<Option<String>, SecretError> { Ok(None) }
    fn delete_usage_sk(&self, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml pending_store`
Expected: PASS。

- [ ] **Step 5: 把真实 keyring 用例标 `#[ignore]`**

在 `keyring_store_roundtrip_real` 上方加 `#[ignore]`（保持其余属性）：

```rust
    /// 真实 OS keyring 往返（非 MemoryStore）。需运行中的 Secret Service（gnome-keyring 等），
    /// 无桌面/Docker/CI 环境会失败，故 `#[ignore]`：`cargo test` 默认不跑，
    /// 显式跑用 `cargo test -- --ignored`（本机装了密钥环时）。见 spec §6。
    #[ignore]
    #[test]
    fn keyring_store_roundtrip_real() { ... } // 原函数体不变
```

- [ ] **Step 6: 跑全部 secrets 测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib secrets::`
Expected: 除 `#[ignore]` 外全 PASS（`keyring_store_roundtrip_real` 显示 ignored）。

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): PendingStore 占位后端 + 真实 keyring 用例改 #[ignore]"
```

---

## Chunk 2: 后端选择 + 可切换句柄 + 启动装配（Rust）

### Task 2.1: `BackendKind` + `select_backend` 纯函数（TDD）

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn select_backend_pure_logic() {
        use BackendKind::*;
        // keyring 可用 → 永远 Keyring（无视授权标记）
        assert_eq!(select_backend(true, None), Keyring);
        assert_eq!(select_backend(true, Some(true)), Keyring);
        // keyring 不可用 + 已授权 → File
        assert_eq!(select_backend(false, Some(true)), File);
        // keyring 不可用 + 未授权（含 None 与 Some(false)）→ Pending
        assert_eq!(select_backend(false, None), Pending);
        assert_eq!(select_backend(false, Some(false)), Pending);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml select_backend_pure_logic`
Expected: 编译失败（`BackendKind`/`select_backend` 不存在）。

- [ ] **Step 3: 实现 enum + 纯函数**

```rust
/// 当前激活的后端种类（供 `get_secret_status` 派生 mode/consent_required，无需额外状态字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Keyring,
    File,
    Pending,
}

/// 启动期后端选择（纯函数，可单测）。`keyring_ok` 来自 `keyring_available()`，
/// `granted` 来自 `Settings.secret_store_fallback`（None=每次询问；Some(true)=已授权）。
pub fn select_backend(keyring_ok: bool, granted: Option<bool>) -> BackendKind {
    if keyring_ok {
        BackendKind::Keyring
    } else if matches!(granted, Some(true)) {
        BackendKind::File
    } else {
        BackendKind::Pending
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml select_backend_pure_logic`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): BackendKind + select_backend 纯函数"
```

### Task 2.2: `keyring_available` 探测 + `make_store` 构造

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`（顶部 `use std::sync::Arc;`）

- [ ] **Step 1: 实现 `keyring_available` 与 `make_store`**

> 这两个函数打真实 keyring / 文件系统，无法纯单测；正确性靠 `select_backend` 单测 + 手动冒烟（Chunk 4）保证。无需先写失败测试。

```rust
use std::sync::Arc;

/// 探测系统密钥环是否可用：对临时条目做 set→get→delete 往返。任一步失败即视为不可用
/// （覆盖 `NoBackendAccess` 无后端、D-Bus 无守护进程、gnome-keyring 锁定态等情况）。
pub fn keyring_available() -> bool {
    let store = KeyringStore;
    const PROBE: &str = "switchlm_availability_probe";
    let ok = store.set_key(PROBE, "1").is_ok()
        && matches!(store.get_key(PROBE), Ok(Some(_)));
    let _ = store.delete_key(PROBE); // 清理
    ok
}

/// 按 `kind` 构造具体后端。`File` 需 `<dir>` 以定位 secrets.json。
pub fn make_store(kind: BackendKind, dir: &Path) -> Result<Arc<dyn SecretStore>, SecretError> {
    Ok(match kind {
        BackendKind::Keyring => Arc::new(KeyringStore),
        BackendKind::File => Arc::new(FileSecretStore::new(dir)?),
        BackendKind::Pending => Arc::new(PendingStore),
    })
}
```

- [ ] **Step 2: 编译检查**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib secrets --no-run`
Expected: 编译通过。

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): keyring_available 探测 + make_store 构造"
```

### Task 2.3: `SecretStoreHandle` 可切换句柄（TDD）

`Mutex<(Arc<dyn SecretStore>, BackendKind)>`，impl `SecretStore`（委托），提供 `swap` / `kind`。

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn handle_delegates_and_swaps_and_reports_kind() {
        let h = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Pending);
        assert_eq!(h.kind(), BackendKind::Pending);
        // 委托给当前后端（MemoryStore 读 None）
        assert_eq!(h.get_key("a").unwrap(), None);
        h.set_key("a", "v").unwrap(); // MemoryStore 写成功
        assert_eq!(h.get_key("a").unwrap(), Some("v".into()));
        // swap 到另一个后端，kind 随之更新
        h.swap(Arc::new(PendingStore), BackendKind::Pending);
        assert_eq!(h.kind(), BackendKind::Pending);
        assert_eq!(h.get_key("a").unwrap(), None); // PendingStore 读 None（不再持有旧值）
        assert!(matches!(h.set_key("a", "x"), Err(SecretError::PendingConsent)));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml handle_delegates_and_swaps_and_reports_kind`
Expected: 编译失败（`SecretStoreHandle` 不存在）。

- [ ] **Step 3: 实现 `SecretStoreHandle`**

```rust
/// 可在运行期切换后端的句柄：持有 `(当前后端, 种类)`。impl `SecretStore`（委托给当前后端），
/// 故可透明替换 `AppStateInner.secrets` 字段（所有 `state.secrets.set_key(...)` 调用点零改动）。
/// 授权后 `swap` 从 PendingStore 切到 FileSecretStore，无需重启。
pub struct SecretStoreHandle {
    inner: Mutex<(Arc<dyn SecretStore>, BackendKind)>,
}

impl SecretStoreHandle {
    pub fn new(store: Arc<dyn SecretStore>, kind: BackendKind) -> Self {
        Self { inner: Mutex::new((store, kind)) }
    }
    fn current(&self) -> Arc<dyn SecretStore> {
        self.inner.lock().unwrap().0.clone()
    }
    pub fn swap(&self, store: Arc<dyn SecretStore>, kind: BackendKind) {
        *self.inner.lock().unwrap() = (store, kind);
    }
    pub fn kind(&self) -> BackendKind {
        self.inner.lock().unwrap().1
    }
}

impl SecretStore for SecretStoreHandle {
    fn set_key(&self, id: &str, k: &str) -> Result<(), SecretError> { self.current().set_key(id, k) }
    fn get_key(&self, id: &str) -> Result<Option<String>, SecretError> { self.current().get_key(id) }
    fn delete_key(&self, id: &str) -> Result<(), SecretError> { self.current().delete_key(id) }
    fn set_usage_sk(&self, id: &str, k: &str) -> Result<(), SecretError> { self.current().set_usage_sk(id, k) }
    fn get_usage_sk(&self, id: &str) -> Result<Option<String>, SecretError> { self.current().get_usage_sk(id) }
    fn delete_usage_sk(&self, id: &str) -> Result<(), SecretError> { self.current().delete_usage_sk(id) }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml handle_delegates_and_swaps_and_reports_kind`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/secrets.rs
git commit -m "feat(secrets): SecretStoreHandle 可运行期切换后端"
```

### Task 2.4: re-export 新类型

**Files:**
- Modify: `src-tauri/src/config/mod.rs`（已有 `pub use secrets::*;`，确认覆盖即可）

- [ ] **Step 1: 确认 `mod.rs` 有 `pub use secrets::*;`**

Run: `grep -n "pub use secrets" src-tauri/src/config/mod.rs`
Expected: 命中 `pub use secrets::*;`（新类型自动 re-export，无需改动）。若无则补一行。

- [ ] **Step 2: Commit（仅当有改动）**

```bash
git add src-tauri/src/config/mod.rs && git commit -m "chore(config): re-export secrets 新类型" || echo "no change"
```

### Task 2.5: `Settings.secret_store_fallback` 字段（TDD）

**Files:**
- Modify: `src-tauri/src/config/types.rs`（`Settings` struct + `Default` impl）

- [ ] **Step 1: 写失败测试**（加到 types.rs 的 `#[cfg(test)]` 模块，或新建）

```rust
    #[test]
    fn settings_secret_store_fallback_defaults_none_and_parses() {
        // 旧配置无此字段 → None（向后兼容）
        let old: Settings = serde_json::from_str(r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info"}"#).unwrap();
        assert_eq!(old.secret_store_fallback, None);
        // 显式 true → Some(true)
        let granted: Settings = serde_json::from_str(
            r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info","secret_store_fallback":true}"#,
        ).unwrap();
        assert_eq!(granted.secret_store_fallback, Some(true));
        // Default → None
        assert_eq!(Settings::default().secret_store_fallback, None);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml settings_secret_store_fallback_defaults_none_and_parses`
Expected: 编译失败（字段不存在）。

- [ ] **Step 3: 加字段**

在 `Settings` struct 的 `log_level` 后加：

```rust
    /// 无系统密钥环时是否已授权将密钥以明文存入 secrets.json（Linux 等场景）。
    /// None=未授权（每次启动询问）；Some(true)=已授权（记住）。见 spec §4.2.3。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_store_fallback: Option<bool>,
```

在 `Default` impl 加：`secret_store_fallback: None,`

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml settings_secret_store_fallback_defaults_none_and_parses`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs
git commit -m "feat(config): Settings.secret_store_fallback 授权标记"
```

### Task 2.6: `AppStateInner.secrets` 字段类型改 handle + 构造点更新

**Files:**
- Modify: `src-tauri/src/proxy/state.rs`（字段 + `load()`）
- Modify: `src-tauri/src/proxy/dispatch.rs`（5 处测试构造点：2 字面量 + 3 `let` 绑定）

- [ ] **Step 1: 改字段类型**

`state.rs:18` `pub secrets: Arc<dyn SecretStore>,` → `pub secrets: SecretStoreHandle,`
并把 `state.rs:10` 已有的 `use crate::config::{AppConfig, SecretStore, UsageCreds};` 合并扩展为 `use crate::config::{AppConfig, SecretStore, SecretStoreHandle, BackendKind, UsageCreds};`（**合并进同一行**，勿另起新 `use`——否则 `SecretStore` 重复导入触发 E0252）。

- [ ] **Step 2: `load()` 包裹为 handle**

`state.rs` 的 `load()`（当前 `secrets,` 字段赋值）改为：

```rust
        secrets: SecretStoreHandle::new(secrets, BackendKind::Keyring),
```

> 测试默认 Keyring kind；既有测试不检查 mode，行为不变。

- [ ] **Step 3: 更新 dispatch.rs 测试中的全部 5 处构造点**

dispatch.rs 测试模块共 **5** 处构造 `secrets`（不是 2 处！）。测试模块顶部已有 `use crate::config::*;`，`SecretStoreHandle`/`BackendKind`/`MemoryStore` 均 glob 导入，**无需再加 use**；`Arc` 已在文件作用域内。分两种形态：

形态 A — struct 字面量字段（**2 处**，约 994、1020 行）：
```rust
        secrets: Arc::new(MemoryStore::default()),
```
改为：
```rust
        secrets: SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring),
```

形态 B — `let` 绑定后用字段简写（**3 处**，约 937、1135、1228 行；分别流向 971/1164/1232 的 `secrets,`）：
```rust
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
```
改为（去掉 `Arc<dyn SecretStore>` 类型标注，改产 handle）：
```rust
        let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
```
（下游 `secrets,` 字段简写**不用改**——字段类型已是 `SecretStoreHandle`，正好匹配。）

改完确认 5 处都已覆盖：
Run: `grep -n "Arc::new(MemoryStore::default())" src-tauri/src/proxy/dispatch.rs`
Expected: 5 行，每行都含 `SecretStoreHandle::new`（无残留 `Arc::new(MemoryStore)` 直接赋给 secrets 的写法）。

- [ ] **Step 4: 全量编译检查（含测试）**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --no-run`
Expected: 编译通过（所有 `state.secrets.<method>()` 调用点无需改，因 handle impl SecretStore；`load()` 的 5 个测试调用点签名不变）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/state.rs src-tauri/src/proxy/dispatch.rs
git commit -m "refactor(state): AppStateInner.secrets 改为 SecretStoreHandle"
```

### Task 2.7: 启动期探测 + 选择 + 推迟迁移（`lib.rs`）

**Files:**
- Modify: `src-tauri/src/lib.rs`（setup 内 `secrets` 选择 + migrate 顺序）

- [ ] **Step 1: 重写 setup 内的 secrets/migrate 段**

把 `lib.rs` 当前：
```rust
            let secrets: Arc<dyn config::SecretStore> = Arc::new(KeyringStore);
            // One-time: move any plaintext usage SK from app_config.json into the keyring.
            config::store::migrate_usage_sk_to_keyring(&dir, secrets.as_ref())?;
            let mut cfg = config::store::load(&dir)?;
            if config::store::normalize_legacy_vendors(&mut cfg) {
                let _ = config::store::save(&dir, &cfg);
            }
```
改为：
```rust
            // 探测系统密钥环 → 选后端（keyring 可用→KeyringStore；不可用且已授权→FileSecretStore；
            // 否则→PendingStore，前端弹授权框，授权后 swap 到 FileSecretStore）。
            let probe_ok = config::secrets::keyring_available();
            let mut cfg = config::store::load(&dir)?;
            let kind = config::secrets::select_backend(probe_ok, cfg.settings.secret_store_fallback);
            let store = config::secrets::make_store(kind, &dir)?;
            // 非 pending：立即把遗留明文 SK 从 app_config.json 迁进密钥存储，再重载干净配置。
            // pending：推迟到 grant_secret_consent（避免授权前向 secrets.json 写入）。
            if kind != config::secrets::BackendKind::Pending {
                config::store::migrate_usage_sk_to_keyring(&dir, store.as_ref())?;
                cfg = config::store::load(&dir)?;
            }
            if config::store::normalize_legacy_vendors(&mut cfg) {
                let _ = config::store::save(&dir, &cfg);
            }
            let secrets = config::SecretStoreHandle::new(store, kind);
```
（下方 `AppStateInner { ... secrets, ... }` 字段名不变；移除原 `use crate::config::KeyringStore;` 若不再直接用——按编译器提示。）

- [ ] **Step 2: 编译检查**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过。

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(startup): keyring 探测 + 后端选择 + pending 推迟迁移"
```

### Task 2.8: `get_secret_status` + `grant_secret_consent` 命令

**Files:**
- Modify: `src-tauri/src/commands.rs`（加命令 + 视需要加 `SecretStatusView`）
- Modify: `src-tauri/src/lib.rs`（`invoke_handler` 注册）

- [ ] **Step 1: 加 `SecretStatusView` + 两个命令**（`commands.rs`，参照既有 `SettingsView` 模式）

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct SecretStatusView {
    /// "keyring" | "file" | "pending"
    pub mode: String,
    /// 仅 pending 时为 true（前端据此弹授权框）
    pub consent_required: bool,
}

/// 当前密钥存储后端状态（前端启动期读，决定是否弹授权框）。
#[tauri::command]
pub async fn get_secret_status(state: State<'_, AppState>) -> Result<SecretStatusView, String> {
    let (mode, consent_required) = match state.secrets.kind() {
        config::BackendKind::Keyring => ("keyring", false),
        config::BackendKind::File => ("file", false),
        config::BackendKind::Pending => ("pending", true),
    };
    Ok(SecretStatusView { mode: mode.into(), consent_required })
}

/// 用户同意明文回退存储：记授权 → swap 到 FileSecretStore → 补跑推迟的迁移。
#[tauri::command]
pub async fn grant_secret_consent(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    {
        let mut cfg = state.config.write().await;
        cfg.settings.secret_store_fallback = Some(true);
        crate::config::store::save(&dir, &cfg).map_err(|e| e.to_string())?;
    }
    let file_store: Arc<dyn config::SecretStore> =
        Arc::new(config::FileSecretStore::new(&dir).map_err(|e| e.to_string())?);
    state.secrets.swap(file_store, config::BackendKind::File);
    // 授权前推迟的遗留 SK 迁移：现在补跑（幂等；多次调用安全）。
    crate::config::store::migrate_usage_sk_to_keyring(&dir, &state.secrets).map_err(|e| e.to_string())?;
    Ok(())
}
```
> `use std::sync::Arc;` 需在 commands.rs 顶部已存在或补上。

- [ ] **Step 2: 注册命令**（`lib.rs` 的 `invoke_handler!` 列表）

在 `quit_app` 附近加：
```rust
            commands::get_secret_status,
            commands::grant_secret_consent,
```

- [ ] **Step 3: 编译检查**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过。

- [ ] **Step 4: 全量后端测试回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全 PASS（`#[ignore]` 的真实 keyring 用例 ignored）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(cmd): get_secret_status + grant_secret_consent 命令"
```

---

## Chunk 3: 前端授权对话框（TS + Vue）

> 前端无单测框架。每个任务的验证 = `npx vue-tsc --noEmit` 通过 + 最终手动冒烟。

### Task 3.1: TS 类型 + IPC 包装

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/commands.ts`

- [ ] **Step 1: 加类型镜像**（`types.ts`，参照既有手维护镜像约定——snake_case 保留）

```ts
// Mirrors src-tauri `SecretStatusView`.
export interface SecretStatusView {
  mode: "keyring" | "file" | "pending";
  consent_required: boolean;
}
```

- [ ] **Step 2: 加 IPC 包装**（`commands.ts`）

```ts
// ---- secret store / consent (Linux fallback) ----
export const getSecretStatus = () => invoke<SecretStatusView>("get_secret_status");
export const grantSecretConsent = () => invoke<void>("grant_secret_consent");
```
（`SecretStatusView` 需加入文件顶部 `import type { ... } from "./types"` 列表。）

- [ ] **Step 3: 类型检查**

Run: `npx vue-tsc --noEmit`
Expected: 无错误。

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts
git commit -m "feat(web): SecretStatusView 类型 + getSecretStatus/grantSecretConsent 包装"
```

### Task 3.2: `system` store 加 secret 状态 + 同意动作

**Files:**
- Modify: `src/stores/system.ts`

- [ ] **Step 1: 加状态与动作**

在 store 内 `bindError` 附近加：

```ts
  const secretStatus = ref<SecretStatusView | null>(null);
```
（顶部 `import type { ... SecretStatusView } from "../lib/types";`）

加动作（并入既有动作列表 `return` 与定义）：

```ts
  async function loadSecretStatus() {
    secretStatus.value = await api.getSecretStatus();
  }

  async function grantConsent() {
    await api.grantSecretConsent();
    await loadSecretStatus(); // swap 后 mode→file, consent_required→false
  }
```

`return { ... }` 加 `secretStatus, loadSecretStatus, grantConsent`。

- [ ] **Step 2: 在启动 `refresh()` 里一并拉取**

在 `refresh()` **既有 `try { ... }` 块内**，把那段 `Promise.all`（连同紧跟的两行 `status.value = ...; bindError.value = ...;`）替换为下面这段（外层 `refreshing.value = true` 与 `finally` 原样保留，勿删）：
```ts
      const [statusResult, errorResult, secretResult] = await Promise.all([
        api.getServerStatus(),
        api.getBindError(),
        api.getSecretStatus(),
      ]);
      status.value = statusResult;
      bindError.value = errorResult;
      secretStatus.value = secretResult;
```

- [ ] **Step 3: 类型检查**

Run: `npx vue-tsc --noEmit`
Expected: 无错误。

- [ ] **Step 4: Commit**

```bash
git add src/stores/system.ts
git commit -m "feat(web): system store 加 secretStatus/loadSecretStatus/grantConsent"
```

### Task 3.3: 全局授权 `NModal`（`App.vue`）

**Files:**
- Modify: `src/App.vue`

- [ ] **Step 1: 加模态框**

`App.vue` `<script setup>` 内加：
```ts
import { NModal, NSpace, NButton, NText } from "naive-ui";
import { useSystemStore } from "./stores/system";
const system = useSystemStore();
onMounted(() => {
  system.refresh(); // 启动即拉状态（含 secretStatus）
});
const consentOpen = computed(() => system.secretStatus?.consent_required === true);
async function onConsent() { await system.grantConsent(); }
async function onQuit() { await system.quit(); }
```
（`onMounted` 与现有 `matchMedia` 监听并存；若已有 `onMounted` 则合并，勿重复注册。）

`<template>` 在 `<NLayout>` 之外（`NDialogProvider` 内）加：
```html
    <NModal :show="consentOpen" :mask-closable="false" :close-on-esc="false" preset="card" style="max-width: 480px" title="密钥存储授权">
      <NSpace vertical :size="12">
        <NText>未检测到系统密钥环（如 gnome-keyring / KWallet / seahorse-daemon）。</NText>
        <NText>是否同意将访问密钥以<strong>明文</strong>保存到本地文件 <code>secrets.json</code>？</NText>
        <NText depth="3" style="font-size: 13px">明文存储存在泄露风险，建议安装密钥环以获得更安全的存储。</NText>
        <NSpace justify="end" :size="8">
          <NButton @click="onQuit">退出</NButton>
          <NButton type="primary" @click="onConsent">同意并继续</NButton>
        </NSpace>
      </NSpace>
    </NModal>
```

- [ ] **Step 2: 类型检查**

Run: `npx vue-tsc --noEmit`
Expected: 无错误。

- [ ] **Step 3: Commit**

```bash
git add src/App.vue
git commit -m "feat(web): 无密钥环时全局授权 NModal"
```

### Task 3.4: 前端手动冒烟（验证）

- [ ] **Step 1: 正常路径（有 keyring）**

确保系统有 gnome-keyring 运行。Run: `npm run tauri dev`（首次会编译数分钟）。
Expected: 应用启动、无授权弹窗、托盘出现、可进入设置页。

- [ ] **Step 2: pending 路径（无 keyring）**

制造"无 keyring"状态以触发 pending。**首选**（最可靠、不扰动系统）：临时把 `src-tauri/src/config/secrets.rs` 里 `keyring_available()` 的返回值强制改为 `false`（仅本地调试，**勿提交**）：
```rust
pub fn keyring_available() -> bool { false } // 临时：调试 pending 流程，验证后还原
```
备选（不动代码）：停用本机 keyring 守护进程——`systemctl --user stop gnome-keyring-daemon.service`（或 `pkill -f gnome-keyring`）。

重新 `npm run tauri dev`。
Expected: 启动后弹出"密钥存储授权"模态框（不可点遮罩关闭）。点"同意并继续" → 模态框关闭；随后新增 Provider 并保存密钥成功（检查 `~/.local/share/SwitchLM/secrets.json` 出现且权限 0600、含该 key）。重启 dev → 不再弹窗（已记住授权）。点"退出" → 应用退出。

> 验证完务必还原 `keyring_available()` 改动，再进 Step 3。

- [ ] **Step 3: 还原任何临时改动，确认 `git status` 干净**

Run: `git status --short`
Expected: 无意外改动（探测强制 false 等调试改动已还原）。

---

## Chunk 4: 打包声明 + 文档 + 最终构建验证

### Task 4.1: deb/rpm 运行期依赖（`tauri.conf.json`）

**Files:**
- Modify: `src-tauri/tauri.conf.json`（`bundle` 内加 `linux`）

> 字段路径已校验（`bundle.linux.deb.depends` / `bundle.linux.rpm.depends`，均 `Option<Vec<String>>`，tauri-utils v2.9.3）。实现期仅需确认各发行版包名。

- [ ] **Step 1: 加 `bundle.linux`**

在 `bundle` 对象内（`icon` 同级）加：

```jsonc
    "linux": {
      "deb": {
        "depends": ["libwebkit2gtk-4.1-0", "libayatana-appindicator3-1", "libdbus-1-3"]
      },
      "rpm": {
        "depends": ["webkit2gtk4.1", "libayatana-appindicator", "dbus-libs"]
      }
    }
```

- [ ] **Step 2: 校验 JSON**

Run: `node -e "JSON.parse(require('fs').readFileSync('src-tauri/tauri.conf.json','utf8')); console.log('valid')"`
Expected: `valid`。

- [ ] **Step 3: Commit**

```bash
git add src-tauri/tauri.conf.json
git commit -m "build(linux): 声明 deb/rpm 运行期依赖"
```

### Task 4.2: README Linux 章节

**Files:**
- Modify: `README.md`（在"前置依赖（Windows）"小节后加"前置依赖（Linux）"，并在"构建与运行"补充 Linux 命令与注意事项）

- [ ] **Step 1: 加 Linux 前置依赖小节**

在 `### 前置依赖（Windows）` 小节之后插入：

```markdown
### 前置依赖（Linux）

Tauri v2 在 Linux 上依赖 WebKitGTK 与若干系统库。以 Debian/Ubuntu 为例：

```bash
sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libdbus-1-dev patchelf
```

Fedora：

```bash
sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libayatana-appindicator-devel \
  librsvg2-devel dbus-devel patchelf
```

另需 Node.js ≥ 20、Rust stable（同 Windows）。另见下方"Linux 运行注意事项"。
```

- [ ] **Step 2: 在"构建与运行"补充 Linux 产出与注意事项**

在该小节末尾加：

```markdown
> **Linux 打包**：`npm run tauri build -- --bundles deb,rpm` 产出 `.deb`/`.rpm`（位于 `src-tauri/target/release/bundle/`）；安装包已声明运行期依赖，安装时会自动拉取 WebKitGTK / appindicator / libdbus。

#### Linux 运行注意事项

- **系统托盘**：GNOME（尤其 Wayland）默认无传统托盘，需安装 *AppIndicator and KStatusNotifierItem Support* 扩展；无托盘的环境下关闭窗口将隐藏，再次启动应用（单实例会聚焦既有窗口）即可恢复。
- **密钥存储**：建议运行 gnome-keyring / KWallet；**未安装密钥环**时首次启动会弹出对话框询问是否将访问密钥以明文存入 `~/.local/share/SwitchLM/secrets.json`（文件权限 0600）。同意即用文件存储，退出则不保存。需要更安全的存储请安装密钥环。
- **不支持 headless / 无桌面服务器**（这是桌面托盘应用）。
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: README 补 Linux 前置依赖与运行注意事项"
```

### Task 4.3: 最终验证

- [ ] **Step 1: 后端全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全 PASS（真实 keyring 用例 ignored）。

- [ ] **Step 2: 前端类型检查 + 构建**

Run: `npm run build`
Expected: 成功（`vue-tsc --noEmit` + `vite build` 均通过）。

- [ ] **Step 3: 产出 deb/rpm 安装包**

Run: `npm run tauri build -- --bundles deb,rpm`
Expected: 在 `src-tauri/target/release/bundle/deb/` 与 `.../rpm/` 下各产出安装包，无错误。

> 若实机缺包名导致依赖声明与发行版不符，构建仍成功（声明仅影响安装期解析）；按警告微调 `tauri.conf.json` 的包名即可。

- [ ] **Step 4: 安装包冒烟（可选，建议）**

Run: `sudo dpkg -i src-tauri/target/release/bundle/deb/*.deb && sudo apt-get -f install -y`（Debian 系）
Expected: 依赖被自动拉取、应用出现在应用菜单、可启动。

- [ ] **Step 5: 最终 Commit（如有收尾调整）**

```bash
git add -A && git commit -m "chore(linux): 最终构建验证收尾" || echo "clean"
```

---

## 完成判据

- `cargo test`（排除 `#[ignore]`）全绿；`npm run build` 通过。
- 有 keyring：`tauri dev` 正常、可存取密钥。
- 无 keyring：首次启动弹授权框；同意→`secrets.json`(0600) 生效、重启不再弹；退出→应用退出。
- `tauri build -- --bundles deb,rpm` 产出两个安装包，安装时依赖自动拉取。
- README 含 Linux 前置依赖与注意事项。

## 评审备忘

- 本计划评审按"整份一次评审"进行（全文约 1150 行，但**每个 chunk 均 < 1000 行**满足 chunk 上限；单次整份评审比逐 chunk 更能发现跨块一致性问题）。
- **权限声明**：`get_secret_status` / `grant_secret_consent` 走 core 默认权限，**无需**改 `capabilities/default.json`（对应 spec §9「通常无需」）。
- 评审未覆盖：Task 4.3 Step 3/4 的实机构建（需 Linux 桌面 + 系统依赖），由实施者在执行阶段完成。
