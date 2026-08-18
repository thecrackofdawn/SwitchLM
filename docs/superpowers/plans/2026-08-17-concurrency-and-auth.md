# 套餐并发限制与 sk-xxx 客户端认证 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 每套餐(Provider)可配最大并发(排队等待,5× 上限本地 429),并可开启 sk-xxx 客户端认证 + 非 127.0.0.1 监听,把 SwitchLM 安全地提供给多人远程使用。

**Architecture:** 认证在 axum 中间件层(空密钥表直通,内存缓存比对,不碰 keyring);并发限制在 `dispatch` 内部按 provider 双层信号量(`run`=N 运行 + `gate`=5N 在途),流式响应把 permit 移进 body 流靠 Drop 释放;密钥元数据存 `app_config.json`、本体存 OS keyring;非回环监听与"至少一个密钥"双向硬校验。Spec: `docs/superpowers/specs/2026-08-17-concurrency-and-auth-design.md`。

**Tech Stack:** Rust (axum 0.7, tokio semaphore, keyring, getrandom), Vue 3 + Naive UI + Pinia, Tauri v2 IPC。

## Global Constraints

- 前端包管理器是 **pnpm**,禁止 npm/npx(会写坏 lockfile)。类型检查:`pnpm exec vue-tsc --noEmit`。
- 后端测试:`cargo test --manifest-path src-tauri/Cargo.toml`(co-located `#[cfg(test)]`,wiremock + FakeClock 惯例)。
- **秘密绝不进 `app_config.json`**:sk-xxx 完整密钥只存 SecretStore(keyring);config 只存 id/名称/前缀/创建时间。
- 日志标识 vendor + upstream 模型名,绝不打内部 `id`;日志中的密钥只出现名称或前缀,绝不出现完整密钥。
- serde 无 `rename_all`:Rust 字段蛇形即 JSON 字段名;`src/lib/types.ts` 是手工维护的 TS 镜像,改结构必须同步。
- UI 文案中文;每个 mutation 命令后前端**重取**受影响切片(不乐观更新)。
- 平台目标 Windows + Linux;路径用 `PathBuf`,平台分支用 `#[cfg(target_os)]`。
- 直接在 `main` 开发(仓库惯例);**commit 时 `git add` 必须显式列路径,禁止 `git add -A`**。
- 本地 429(并发排队超限)**不触发熔断、不 fallback**;带 `Retry-After: 5`。
- 重试 sleep 边界上不持有任何上游 response/stream(reviewer 检查点,重构时不得破坏)。

---

### Task 1: Config 类型:`Provider.max_concurrency`、`AppConfig.auth_keys`、`Settings.bind_addr`

**Files:**
- Modify: `src-tauri/src/config/types.rs`
- Modify: 所有含 `Provider {` 字面量的测试/代码文件(编译器逐一报错,机械补字段)

**Interfaces:**
- Produces: `Provider.max_concurrency: Option<u32>`;`AppConfig.auth_keys: Vec<AuthKeyMeta>`;`AuthKeyMeta { id, name, key_prefix, created_at }`;`Settings.bind_addr: String`(默认 `"127.0.0.1"`)。后续所有任务消费这些字段名。

- [ ] **Step 1: 写失败测试**(加在 `types.rs` 的 `mod tests`)

```rust
#[test]
fn provider_max_concurrency_defaults_none_when_absent() {
    // 旧 config 无此字段 -> None(不限),向后兼容。
    let json = r#"{"id":"zhipu","vendor":"zhipu","display_name":"智谱","openai_base_url":"https://x"}"#;
    let p: Provider = serde_json::from_str(json).unwrap();
    assert_eq!(p.max_concurrency, None);
}

#[test]
fn provider_max_concurrency_round_trips() {
    let p = Provider {
        id: "p".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
        openai_base_url: None, anthropic_base_url: None, usage_creds: None,
        max_concurrency: Some(5),
    };
    let back: Provider = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back.max_concurrency, Some(5));
}

#[test]
fn auth_key_meta_round_trips() {
    let m = AuthKeyMeta {
        id: "ak_ab12".into(), name: "张三的 Claude Code".into(),
        key_prefix: "sk-Ab9x".into(), created_at: 1755400000,
    };
    let back: AuthKeyMeta = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.created_at, 1755400000);
}

#[test]
fn app_config_auth_keys_defaults_empty_when_absent() {
    let json = r#"{"providers":[],"models":[],"profiles":[],"settings":{}}"#;
    let cfg: AppConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.auth_keys.is_empty());
}

#[test]
fn settings_bind_addr_defaults_loopback_when_absent() {
    let json = r#"{"port":6950,"autostart":false,"usage_refresh_interval_secs":60,"log_level":"info"}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(s.bind_addr, "127.0.0.1");
    assert_eq!(Settings::default().bind_addr, "127.0.0.1");
}
```

同时更新既有 `app_config_roundtrips` 测试:`Settings { ... }` 字面量补 `bind_addr: "127.0.0.1".into()`,`Provider { ... }` 补 `max_concurrency: None`,`AppConfig { ... }` 补 `auth_keys: vec![]`,保持该测试编译。

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config::types`
Expected: FAIL(`max_concurrency`/`AuthKeyMeta`/`bind_addr`/`auth_keys` 不存在)

- [ ] **Step 3: 实现**(types.rs)

```rust
// Provider 增字段(放 usage_creds 之后):
    /// 该套餐(账号)允许的最大并发请求数。None = 不限制。同级厂商不同套餐等级
    /// (lite/pro/max)并发不同,以前端按厂商预填、用户手填为准(spec 2026-08-17 §3.1)。
    #[serde(default)]
    pub max_concurrency: Option<u32>,
```

```rust
// AppConfig 增字段(放 settings 之前):
    /// 客户端认证密钥元数据。空 = 免认证(本机模式)。完整密钥存 OS keyring,
    /// 绝不进本文件(spec 2026-08-17 §3.2)。
    #[serde(default)]
    pub auth_keys: Vec<AuthKeyMeta>,
```

```rust
/// sk-xxx 客户端密钥的元数据(本体在 keyring:条目名 `auth_key::{id}`)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthKeyMeta {
    /// 不透明主键 ak_<hex>。
    pub id: String,
    /// 备注,如 "张三的 Claude Code"。
    pub name: String,
    /// 列表展示前缀,如 "sk-Ab9x…"。
    pub key_prefix: String,
    /// unix 秒。
    pub created_at: i64,
}
```

```rust
// Settings 增字段 + 默认值:
    /// 代理监听地址(默认 127.0.0.1;非回环 = 远程模式,须先有认证密钥,
    /// 见 set_bind_addr/delete_auth_key 的联动校验,spec §6)。
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
// ...
fn default_bind_addr() -> String { "127.0.0.1".into() }
```

同步更新 `Default for Settings` / `Default for AppConfig`(补 `bind_addr: default_bind_addr()` / `auth_keys: vec![]`)。

- [ ] **Step 4: 修全仓 `Provider {` 字面量**

Run: `grep -rn "Provider {" --include="*.rs" src-tauri/src`(约 51 处,分布在 dispatch.rs/commands.rs/usage/*/tray.rs 等测试与代码)。每处在最后一个字段后补一行 `max_concurrency: None,`。以 `cargo check --manifest-path src-tauri/Cargo.toml` 的编译错误为清单逐一补齐,直至编译通过。**不改任何既有测试断言**。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS(全部,包括既有 round-trip)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src
git commit -m "feat(config): Provider.max_concurrency / auth_keys 元数据 / bind_addr 字段"
```

---

### Task 2: SecretStore 增加 auth-key 条目(4 后端 + handle)

**Files:**
- Modify: `src-tauri/src/config/secrets.rs`
- Modify: `src-tauri/src/config/mod.rs`(如 AuthKeyMeta 从 types 重导出,确认导出链)

**Interfaces:**
- Produces: `SecretStore::{set_auth_key, get_auth_key, delete_auth_key}(key_id: &str, ...)`,keyring 条目名 `auth_key::{key_id}`。Task 7 的命令与 Task 5 的启动加载消费。

- [ ] **Step 1: 写失败测试**(secrets.rs `mod tests`,仿 `memory_store_usage_cookie_distinct_from_api_key_and_sk`)

```rust
#[test]
fn memory_store_auth_key_distinct_from_other_secrets() {
    let s = MemoryStore::default();
    s.set_key("p", "sk-inference").unwrap();
    s.set_auth_key("ak_1", "sk-client-secret").unwrap();
    assert_eq!(s.get_auth_key("ak_1").unwrap(), Some("sk-client-secret".into()));
    assert_eq!(s.get_key("p").unwrap(), Some("sk-inference".into())); // 不互扰
    s.delete_auth_key("ak_1").unwrap();
    assert_eq!(s.get_auth_key("ak_1").unwrap(), None);
}

#[test]
fn file_store_auth_key_roundtrip() {
    let (_dir, s) = tmp_store();
    assert_eq!(s.get_auth_key("ak_1").unwrap(), None);
    s.set_auth_key("ak_1", "sk-client-secret").unwrap();
    assert_eq!(s.get_auth_key("ak_1").unwrap(), Some("sk-client-secret".into()));
    s.delete_auth_key("ak_1").unwrap();
    assert_eq!(s.get_auth_key("ak_1").unwrap(), None);
}

#[test]
fn handle_delegates_auth_key() {
    let h = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
    h.set_auth_key("ak_1", "sk-x").unwrap();
    assert_eq!(h.get_auth_key("ak_1").unwrap(), Some("sk-x".into()));
    h.delete_auth_key("ak_1").unwrap();
    assert_eq!(h.get_auth_key("ak_1").unwrap(), None);
}
```

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config::secrets`
Expected: FAIL(方法不存在)

- [ ] **Step 3: 实现**

trait `SecretStore` 增三个方法(带注释:client auth key,存 `auth_key::{key_id}` 条目,与 api_key/usage_sk/usage_cookie 命名空间隔离):

```rust
    /// 客户端认证密钥(sk-xxx)。keyring 条目名 `auth_key::{key_id}`,与其余秘密隔离。
    fn set_auth_key(&self, key_id: &str, key: &str) -> Result<(), SecretError>;
    fn get_auth_key(&self, key_id: &str) -> Result<Option<String>, SecretError>;
    fn delete_auth_key(&self, key_id: &str) -> Result<(), SecretError>;
```

辅助函数(与 `usage_sk_entry` 并列):

```rust
fn auth_key_entry(key_id: &str) -> String {
    format!("auth_key::{key_id}")
}
```

四个实现全部按 `usage_sk` 模式照写(密钥 ~67 字节,远低于 500B 分片阈值,**不分片**):
- `KeyringStore`:三个方法分别用 `Entry::new(SERVICE, &auth_key_entry(key_id))` set/get/delete,`NoEntry -> Ok(None)`/`Ok(())`,其余错误包 `SecretError::Keyring`;
- `MemoryStore`:`HashMap` 以 `auth_key_entry(key_id)` 为键;
- `FileSecretStore`:同键写 `flush`;
- `PendingStore`:set/delete 返回 `Err(SecretError::PendingConsent)`,get 返回 `Ok(None)`;
- `SecretStoreHandle`:三个纯委托方法。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml config::secrets`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/secrets.rs src-tauri/src/config/mod.rs src-tauri/src/config/types.rs
git commit -m "feat(secrets): SecretStore 客户端认证密钥条目(auth_key:: 命名空间)"
```

---

### Task 3: `ConcurrencyRegistry`(双层信号量,纯模块)

**Files:**
- Create: `src-tauri/src/proxy/concurrency.rs`
- Modify: `src-tauri/src/proxy/mod.rs`(加 `pub mod concurrency;`)

**Interfaces:**
- Produces:
  - `ConcurrencyRegistry`(impl `Default`):`async fn acquire(&self, provider_id: &str, limit: u32) -> Option<ConcurrencyPermit>`——`None` = 在途已满 5×limit,调用方立即本地 429;`Some` = 已同时持有 gate+run permit(排队完成后);
  - `ConcurrencyPermit`:两个 `OwnedSemaphorePermit`(`Send + 'static`,可移入响应 body 流;Drop 即释放);
  - `fn attach_permit(resp: Response<Body>, permit: ConcurrencyPermit) -> Response<Body>`:把 permit 转移进 body 流。
  - Task 5(state 字段)与 Task 6(dispatch 接线)消费。

- [ ] **Step 1: 写失败测试**(concurrency.rs 内 `mod tests`)

```rust
use super::*;

#[tokio::test]
async fn acquire_up_to_run_limit_concurrent() {
    let r = ConcurrencyRegistry::default();
    let p1 = r.acquire("prov", 2).await.expect("first ok");
    let p2 = r.acquire("prov", 2).await.expect("second ok");
    let _ = (p1, p2); // 两个并发位都被占
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn third_waits_until_permit_dropped() {
    let r = Arc::new(ConcurrencyRegistry::default());
    let p1 = r.acquire("prov", 1).await.expect("ok");
    // 排队者 spawn 挂起:持有期不完成,释放后被唤醒(证明的是"排队者"而非新请求)。
    let waiter = tokio::spawn({
        let r = r.clone();
        async move { r.acquire("prov", 1).await.expect("acquired after release") }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!waiter.is_finished(), "must still be queued while p1 held");
    drop(p1);
    let _p2 = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await.expect("queued acquirer proceeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_full_returns_none_at_five_x() {
    // limit=1:1 运行 + 4 排队 = 5 在途;第 6 个 gate try_acquire 失败 -> None(本地 429)。
    // 排队中的 acquire 不会返回(run 信号量等待),必须 spawn 挂起,不能同步收集。
    let r = Arc::new(ConcurrencyRegistry::default());
    let _run = r.acquire("prov", 1).await.expect("run ok");
    let mut queued = Vec::new();
    for _ in 0..4 {
        let r2 = r.clone();
        queued.push(tokio::spawn(async move {
            r2.acquire("prov", 1).await.expect("queue slot ok")
        }));
    }
    // 给排队任务一点时间确认它们卡在 run 等待(gate 已拿到)。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(r.acquire("prov", 1).await.is_none(), "6th must be local-429");
    assert!(queued.iter().all(|j| !j.is_finished()), "queued still waiting");
}

#[tokio::test]
async fn limit_change_rebuilds_limiter() {
    let r = ConcurrencyRegistry::default();
    let _p = r.acquire("prov", 1).await.expect("ok");
    r.acquire("prov", 1).await.expect("queue ok"); // gate 空位仍够
    // limit 改 2:重建,不 panic;旧排队者随后自然排空
    let _a = r.acquire("prov", 2).await.expect("new limiter ok");
    let _b = r.acquire("prov", 2).await.expect("two run slots now");
}

#[tokio::test]
async fn attach_permit_releases_only_when_body_dropped() {
    // permit 移入 body 流:流活着 -> 并发位被占;流 drop -> 释放。
    let r = ConcurrencyRegistry::default();
    let permit = r.acquire("prov", 1).await.expect("ok");
    let resp = attach_permit(Response::new(Body::from("hello")), permit);
    // 第二个 acquire 必须排队(body 还没被消费/drop)
    tokio::time::timeout(std::time::Duration::from_millis(50), async {
        r.acquire("prov", 1).await.expect("should not complete");
    }).await.expect_err("permit still owned by body");
    drop(resp); // 丢弃响应 = 客户端断开的等价物
    let _next = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        r.acquire("prov", 1).await.expect("released on drop")
    }).await.expect("permit released when body dropped");
}
```

(`third_waits_until_permit_dropped` 的排队判定同样用 spawn 挂起式:spawn 一个排队 acquire → 断言 50ms 内未完成 → drop 运行 permit → 断言排队任务 1s 内拿到。与 `gate_full_returns_none_at_five_x` 同型,不重复展开。)

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::concurrency`
Expected: FAIL(模块不存在)

- [ ] **Step 3: 实现**(concurrency.rs 全文)

```rust
//! 每套餐(Provider)并发限制:run 信号量(limit 个运行位)+ gate 信号量(5×limit 个
//! 在途位)。gate.try_acquire 失败 -> 本地 429(不熔断不 fallback);gate 拿到后在
//! run 上无限排队。permit 是 OwnedSemaphorePermit 对,可移入流式响应 body 靠 Drop
//! 释放(spec 2026-08-17 §5)。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Response;
use futures::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 在途上限 = 运行上限的倍数(运行 + 排队合计)。
pub const GATE_MULTIPLIER: u32 = 5;

pub struct ProviderLimiter {
    pub limit: u32,
    gate: Arc<Semaphore>,
    run: Arc<Semaphore>,
}

/// 同时持有的两个 permit。Drop 即释放(OwnedSemaphorePermit 语义)。
pub struct ConcurrencyPermit {
    _gate: OwnedSemaphorePermit,
    _run: OwnedSemaphorePermit,
}

#[derive(Default)]
pub struct ConcurrencyRegistry {
    inner: Mutex<HashMap<String, Arc<ProviderLimiter>>>,
}

impl ConcurrencyRegistry {
    /// 取(或按新 limit 重建)`provider_id` 的 limiter。limit 变化时整体替换:
    /// 旧信号量随持有者排空,瞬时轻微超限可接受(spec §5.4)。
    fn limiter_for(&self, provider_id: &str, limit: u32) -> Arc<ProviderLimiter> {
        let mut map = self.inner.lock().unwrap();
        let rebuild = map
            .get(provider_id)
            .map(|l| l.limit != limit)
            .unwrap_or(true);
        if rebuild {
            map.insert(
                provider_id.to_string(),
                Arc::new(ProviderLimiter {
                    limit,
                    gate: Arc::new(Semaphore::new((limit * GATE_MULTIPLIER) as usize)),
                    run: Arc::new(Semaphore::new(limit as usize)),
                }),
            );
        }
        map.get(provider_id).expect("just inserted").clone()
    }

    /// 尝试占用一个并发位。`None` = 在途已满(gate 失败),调用方立即返回本地 429;
    /// `Some` = gate+run 双 permit 已持有(run 等待期间 gate 一并持有,计入在途)。
    /// 候选 future 被 drop 时未完成的等待自动放弃(gate permit 同步释放)。
    pub async fn acquire(&self, provider_id: &str, limit: u32) -> Option<ConcurrencyPermit> {
        let limiter = self.limiter_for(provider_id, limit);
        // 先 try gate:失败 -> 本地 429,不排队(spec §5.1)。
        let gate = limiter.gate.clone().try_acquire_owned().ok()?;
        let run = limiter.run.clone().acquire_owned().await;
        Some(ConcurrencyPermit { _gate: gate, _run: run })
    }
}

/// 把 permit 的所有权转移进 `resp` 的 body 流:流被消费完、中途 Err、或被 drop
/// (客户端断开)时 permit 随之释放——释放语义由类型系统保证,禁止在任何迭代末尾
/// 显式释放(spec §5.2 Drop 保证)。
pub fn attach_permit(resp: Response<Body>, permit: ConcurrencyPermit) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let mut inner = Box::pin(body.into_data_stream());
    let stream = async_stream::stream! {
        let _permit = permit; // stream 拥有 permit;generator drop = 释放
        while let Some(item) = inner.next().await {
            yield item;
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}
```

(若 `gate_full_returns_none_at_five_x` 因排队语义需要,把 4 个排队者改为 `tokio::spawn(r.acquire(...))` 挂起并断言 `is_none()` 的第 6 个直接返回;断言强度不变。)

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::concurrency`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/concurrency.rs src-tauri/src/proxy/mod.rs
git commit -m "feat(proxy): ConcurrencyRegistry 双层信号量与流式 permit Drop 语义"
```

---

### Task 4: `ProxyError::LocalConcurrencyLimit`(429 + Retry-After)

**Files:**
- Modify: `src-tauri/src/proxy/error.rs`

**Interfaces:**
- Produces: `ProxyError::LocalConcurrencyLimit { provider: String }`,IntoResponse -> 429 + `Retry-After: 5` + 文本体含 `local_concurrency_limit`。Task 6 消费。

- [ ] **Step 1: 写失败测试**(error.rs 加 `mod tests`)

```rust
#[test]
fn local_concurrency_limit_maps_to_429_with_retry_after() {
    let resp = ProxyError::LocalConcurrencyLimit { provider: "prov_x".into() }.into_response();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers().get("retry-after").and_then(|v| v.to_str().ok()),
        Some("5")
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .now_or_never().map(|r| r.unwrap()).unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(s.contains("local_concurrency_limit"), "body tags the error kind: {s}");
}
```

(`now_or_never` 需要 `futures::FutureExt`;若不便,测试改 `#[tokio::test]` + `await`。)

- [ ] **Step 2: 确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::error`
Expected: FAIL(变体不存在)

- [ ] **Step 3: 实现**

```rust
    /// 本地并发排队超限(在途 > 5×max_concurrency)。不是上游限流:不熔断、不
    /// fallback(spec 2026-08-17 §5.3)。
    #[error("local_concurrency_limit: provider '{provider}' concurrency queue full; retry after 5s")]
    LocalConcurrencyLimit { provider: String },
```

`IntoResponse` 的 match 增分支:

```rust
            ProxyError::LocalConcurrencyLimit { .. } => {
                return Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("retry-after", "5")
                    .body(Body::from(self.to_string()))
                    .unwrap();
            }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::error`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/error.rs
git commit -m "feat(proxy): LocalConcurrencyLimit 错误变体(429 + Retry-After: 5)"
```

---

### Task 5: 认证中间件 + AppState 新字段(auth + concurrency)+ 路由接线

**Files:**
- Create: `src-tauri/src/proxy/auth.rs`
- Modify: `src-tauri/src/proxy/mod.rs`(`pub mod auth;` + 重导出)
- Modify: `src-tauri/src/proxy/state.rs`(两个字段 + `reload_auth_cache` + load 时调用)
- Modify: `src-tauri/src/proxy/server.rs`(`build_router` 挂中间件)
- Modify: `src-tauri/src/proxy/openai_edge.rs` / `anthropic_edge.rs` / `dispatch.rs`(传递认证身份用于日志)
- Modify: 全部 16 处 `AppStateInner {` 字面量(编译器清单,补两个新字段)

**Interfaces:**
- Produces:
  - `AuthCache`(`std::sync::RwLock<HashMap<String, String>>`,`new/contains/insert/remove`,锁内无 await);
  - `pub async fn auth_mw(State<AppState>, Request<Body>, Next) -> Response`(空 `cfg.auth_keys` 直通;非空则要求 Bearer/x-api-key 命中缓存,否则按 path 返回对应协议形状的 401);
  - `AppStateInner.auth: AuthCache`、`AppStateInner.concurrency: ConcurrencyRegistry`;
  - `AppStateInner::reload_auth_cache(&self)`(从 config.auth_keys + secrets 重建,单条 keyring 失败跳过);
  - `AuthedKeyId(pub String)` 请求扩展(中间件注入);edge 处理器签名增 `Extension` 参数;`dispatch(state, body, protocol, authed_key: Option<String>)` 日志加 `key=` 字段。

- [ ] **Step 1: 写失败测试**(auth.rs `mod tests`,直接走 `build_router` oneshot,复用 openai_edge.rs 的 state 搭建模式)

```rust
// 测试用最小 state:单 provider/mock 上游可省——认证失败在上游之前。
// 认证通过用例的上游用 wiremock 200 兜底(与 openai_edge::test_state 同构)。

#[tokio::test]
async fn empty_auth_keys_passthrough() {
    // auth_keys 为空 -> 无 header 也放行(本机模式,升级不 401)。
    let state = test_state_auth(&[]).await; // 参数:Vec<(id, full_key)>
    let app = build_router(state);
    let resp = app.oneshot(oai_post_no_auth()).await.unwrap();
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_key_401_openai_shape() {
    let state = test_state_auth(&[("ak_1", "sk-secret1")]).await;
    let app = build_router(state);
    let resp = app.oneshot(oai_post_no_auth()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_str(resp).await;
    assert!(body.contains("invalid_api_key"), "OpenAI 形状: {body}");
    assert!(body.contains("invalid_request_error"));
}

#[tokio::test]
async fn bad_key_401_anthropic_shape() {
    let state = test_state_auth(&[("ak_1", "sk-secret1")]).await;
    let app = build_router(state);
    let resp = app.oneshot(
        Request::builder().method("POST").uri("/v1/messages")
            .header("content-type", "application/json")
            .header("x-api-key", "sk-wrong")
            .body(Body::from(r#"{"model":"glm-5.2","messages":[]}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_str(resp).await;
    assert!(body.contains("authentication_error"), "Anthropic 形状: {body}");
}

#[tokio::test]
async fn bearer_and_x_api_key_both_accepted() {
    // OpenAI 边 Bearer、Anthropic 边 x-api-key 都命中同一缓存。
    let state = test_state_auth(&[("ak_1", "sk-secret1")]).await;
    let app = build_router(state);
    let r1 = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer sk-secret1")
            .body(Body::from(r#"{"model":"glm-5.2","messages":[]}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = app.oneshot(
        Request::builder().method("POST").uri("/v1/messages")
            .header("content-type", "application/json")
            .header("x-api-key", "sk-secret1")
            .body(Body::from(r#"{"model":"glm-5.2","messages":[]}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
}
```

`test_state_auth`:仿 `openai_edge::test_state` 搭 `AppStateInner`,差异:`cfg.auth_keys` 按 `(id, key)` 生成 `AuthKeyMeta`,`state.auth.insert(key, id)`;provider 指向 wiremock 200 mock(认证通过用例需要走到上游)。注意此 helper 也要补 Task 5 引入的两个新字段。

- [ ] **Step 2: 确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::auth`
Expected: FAIL(模块不存在)

- [ ] **Step 3: 实现**

**auth.rs**:

```rust
//! 客户端 sk-xxx 认证中间件。空 `auth_keys` = 直通(本机模式不变);非空则要求
//! `Authorization: Bearer sk-xxx` 或 `x-api-key: sk-xxx` 命中内存缓存(热路径不碰
//! keyring)。401 响应按边缘协议给对应错误形状(spec 2026-08-17 §4)。

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::proxy::AppState;

/// 完整密钥 -> key_id 的内存缓存。std RwLock:锁内只做一次 HashMap 查/改(纳秒级),
/// **持锁区间内不得 await、不得嵌套其它锁**(spec §4 锁纪律)。
#[derive(Default)]
pub struct AuthCache(std::sync::RwLock<HashMap<String, String>>);

impl AuthCache {
    pub fn contains(&self, key: &str) -> bool {
        self.0.read().unwrap().contains_key(key)
    }
    pub fn insert(&self, full_key: String, key_id: String) {
        self.0.write().unwrap().insert(full_key, key_id);
    }
    pub fn remove(&self, full_key: &str) {
        self.0.write().unwrap().remove(full_key);
    }
    pub fn clear(&self) {
        self.0.write().unwrap().clear();
    }
}

/// 中间件注入的认证身份(通过后供 dispatch 日志标注 `key=`)。
#[derive(Clone)]
pub struct AuthedKeyId(pub String);

fn extract_key(req: &Request<Body>) -> Option<&str> {
    if let Some(v) = req.headers().get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(k) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            return Some(k);
        }
    }
    req.headers().get("x-api-key").and_then(|v| v.to_str().ok())
}

fn unauthorized(path: &str) -> Response<Body> {
    let (body, status) = if path.starts_with("/v1/messages") {
        // Anthropic 边错误形状。
        (
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid or missing api key; set ANTHROPIC_AUTH_TOKEN (Bearer) or x-api-key to a SwitchLM access key"}}"#.to_string(),
            StatusCode::UNAUTHORIZED,
        )
    } else {
        // OpenAI 边错误形状。
        (
            r#"{"error":{"message":"invalid or missing api key; set Authorization: Bearer <SwitchLM access key>","type":"invalid_request_error","code":"invalid_api_key"}}"#.to_string(),
            StatusCode::UNAUTHORIZED,
        )
    };
    (
        status,
        [("content-type", "application/json")],
        body,
    ).into_response()
}

pub async fn auth_mw(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    // 空密钥表 = 免认证直通。以 config 为准(而非缓存):config 有密钥而缓存读失败
    // 时必须 fail-closed(全部 401),不得误放行。
    let keys_empty = state.config.read().await.auth_keys.is_empty();
    if keys_empty {
        return next.run(req).await;
    }
    let presented = extract_key(&req).map(str::to_string);
    match presented.filter(|k| state.auth.contains(k)) {
        Some(k) => {
            if let Some(id) = state.auth.key_id(&k) {
                req.extensions_mut().insert(AuthedKeyId(id));
            }
            next.run(req).await
        }
        None => unauthorized(req.uri().path()),
    }
}
```

`AuthCache` 增 `pub fn key_id(&self, key: &str) -> Option<String>`(读锁 get clone)。

**mod.rs**:`pub mod auth;` + `pub use auth::{AuthCache, AuthedKeyId};`。

**state.rs**:`AppStateInner` 增

```rust
    /// 客户端认证密钥内存缓存(完整密钥 -> key_id)。运行时,不持久化;启动时
    /// `reload_auth_cache` 从 keyring 加载,管理命令增量维护。
    pub auth: AuthCache,
    /// 每套餐并发限制器(运行时,不持久化)。limit 从 Provider.max_concurrency 读取。
    pub concurrency: crate::proxy::concurrency::ConcurrencyRegistry,
```

```rust
    /// 从 config.auth_keys + SecretStore 重建认证缓存。单条 keyring 读失败 -> 跳过该条
    /// (该密钥认证 401,降级不阻断;spec §4)。
    pub fn reload_auth_cache(&self) {
        let metas = {
            // 此方法为同步(&self,非 async):用 blocking_read 取快照。调用点均在启动/
            // 管理命令(锁空闲),不会与 dispatch 的长读竞争——如实现遇阻塞风险,
            // 改为 async 并同步调用点。
            match self.config.blocking_read() {
                Ok(cfg) => cfg.auth_keys.clone(),
                Err(_) => return,
            }
        };
        self.auth.clear();
        for m in &metas {
            match self.secrets.get_auth_key(&m.id) {
                Ok(Some(k)) => self.auth.insert(k, m.id.clone()),
                Ok(None) => tracing::warn!("auth key {} 在密钥存储中不存在,认证将被拒绝", m.id),
                Err(e) => tracing::warn!("auth key {} 读取失败(跳过):{e}", m.id),
            }
        }
    }
```

(若 `blocking_read` 在测试单线程 runtime 报错,则把 `reload_auth_cache` 改 `async fn` 并 `load()`/命令处 `.await`——`load` 是同步函数,需把调用挪到 lib.rs 启动流程 `AppStateInner::load` 之后。实现者二选一,保持"启动后缓存就绪"不变量并让全部测试通过。)

`load()` 尾部构造 `Self { ..., auth: AuthCache::default(), concurrency: Default::default() }` 并调用 reload。

**server.rs**:

```rust
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(openai_edge::chat_completions))
        .route("/v1/messages", post(anthropic_edge::messages))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::auth_mw))
        .with_state(state)
}
```

**edge 传身份**:`openai_edge::chat_completions` / `anthropic_edge::messages` 增 extractor `Extension(authed): Extension<Option<AuthedKeyId>>`(axum 的 `Extension<Option<T>>` 缺省即 None,不影响无该扩展的测试请求),`dispatch(state, body, protocol, authed.map(|a| a.0))`。`dispatch` 签名加 `authed_key: Option<String>`;`forward request` 日志增:

```rust
        // 认证身份(密钥 id,如 ak_ab12;None = 免认证模式)。绝不打完整密钥。
        if let Some(k) = &authed_key {
            tracing::info!(target: "switchlm::proxy", req = %req_id, key = %k, "authenticated");
        }
```

(或并入 `forward request` 行的字段——实现者取改动小者,但 `key=` 字段必须在。)

**16 处 `AppStateInner {` 字面量**:`grep -rn "AppStateInner {" --include="*.rs" src-tauri/src`,每处补 `auth: Default::default(), concurrency: Default::default(),`。

- [ ] **Step 4: 跑全部测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS(新 auth 用例 + 既有全绿;dispatch 测试因签名变化需在调用处传 `None`——机械补)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy src-tauri/src/agent_sync.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(proxy): sk-xxx 认证中间件(空表直通/双头/协议形状 401)+ AppState auth/concurrency 字段"
```

---

### Task 6: dispatch 接线并发限制(非流式 + 流式 permit 转移)

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs`

**Interfaces:**
- Consumes: `ConcurrencyRegistry::acquire`、`ConcurrencyPermit`、`attach_permit`(Task 3)、`ProxyError::LocalConcurrencyLimit`(Task 4)、`ModelSnapshot.max_concurrency`(本任务加)。
- Produces: 每一跳对当前 provider acquire;流式响应用 `attach_permit` 转移 permit。

- [ ] **Step 1: 写失败测试**(dispatch.rs `mod tests`;`chain_state` 的 provider 构造处给 `max_concurrency: Some(...)`,需要给 `chain_state` 加参数或新写一个 helper)

非流式排队 + 本地 429(用 wiremock 延迟响应 + 真实时间,300ms 量级):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_limits_inflight_and_local_429_at_5x() {
    // max_concurrency=1:1 运行 + 4 排队;第 6 个本地 429(不打上游、不熔断)。
    let mock = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({"id":"x","choices":[{"message":{"role":"assistant","content":"ok"}}]}))
            .set_delay(std::time::Duration::from_millis(300)))
        .mount(&mock).await;

    let state = single_provider_state(&mock.uri(), Some(1)).await; // provider max_concurrency=1
    let app = build_router(state.clone());
    let mut handles = Vec::new();
    for _ in 0..6 {
        let app = app.clone();
        handles.push(tokio::spawn(async move { app.oneshot(oai_post()).await.unwrap() }));
    }
    // 429 立即返回(不排队)
    let mut saw_local_429 = false;
    for h in handles {
        let resp = h.await.unwrap();
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_local_429 = true;
            assert_eq!(resp.headers().get("retry-after").and_then(|v| v.to_str().ok()), Some("5"));
        }
    }
    assert!(saw_local_429, "6th request must get local 429");
    // 排队者最终全部成功;上游未超并发不可直接断言 wiremock 并发计数,
    // 但 5 个成功 = 排队语义正确(未把第 2..5 个打回)。
    assert_eq!(mock.received_requests().await.unwrap().len(), 5);
    // 未熔断:breaker 健康如常
    assert!(!state.health.is_cooling("m_a", 1000));
}

#[tokio::test]
async fn concurrency_none_is_unlimited() {
    // max_concurrency=None:并发 8 个全直通,无 429。
    let mock = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x"})))
        .mount(&mock).await;
    let state = single_provider_state(&mock.uri(), None).await;
    let app = build_router(state);
    let h1 = tokio::spawn({ let app = app.clone(); async move { app.clone().oneshot(oai_post()).await.unwrap() }});
    let h2 = tokio::spawn({ let app = app.clone(); async move { app.oneshot(oai_post()).await.unwrap() }});
    // ... 同型共 8 个;全部断言 status == OK
}
```

流式释放语义(unit 级已在 Task 3 `attach_permit_releases_only_when_body_dropped` 覆盖;此处补一条 e2e:流式请求完成后并发位恢复):

```rust
#[tokio::test]
async fn stream_completes_and_releases_slot() {
    let mock = MockServer::start().await;
    let sse = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_bytes(sse.as_bytes().to_vec()))
        .mount(&mock).await;
    let state = single_provider_state(&mock.uri(), Some(1)).await;
    let app = build_router(state);
    let r1 = app.clone().oneshot(anthropic_stream_post()).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let _ = body_str(r1).await; // 消费完流
    let r2 = app.oneshot(anthropic_stream_post()).await.unwrap(); // 并发位已恢复
    assert_eq!(r2.status(), StatusCode::OK);
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::dispatch`
Expected: FAIL(acquire 未接线,第 6 个请求不会 429)

- [ ] **Step 3: 实现**

1. `ModelSnapshot` 增 `max_concurrency: Option<u32>`;`snapshot_model` 从 provider 取(`p.and_then(|p| p.max_concurrency)`)。
2. `dispatch_non_stream` 循环内、`key_for` 拿到 key 之后、`attempt_with_retry` 之前:

```rust
        // 套餐并发限制(spec §5):在途满 5N -> 本地 429(不熔断不 fallback);
        // 否则排队(gate+run 双 permit)。acquire 在 attempt_with_retry 外层一次,
        // 原地重试复用同一 permit;循环迭代结束(进入下一跳前)自动释放。
        let permit = match model_snap.max_concurrency {
            Some(limit) => match state.concurrency.acquire(&model_snap.provider_id, limit).await {
                Some(p) => Some(p),
                None => {
                    tracing::warn!(
                        target: "switchlm::proxy", req = %req_id,
                        vendor = %model_snap.vendor,
                        "local concurrency limit hit (queue full)"
                    );
                    return Err(ProxyError::LocalConcurrencyLimit {
                        provider: model_snap.provider_id.clone(),
                    });
                }
            },
            None => None, // 不限
        };
```

`permit` 是循环内局部变量:`return Ok(resp)` 时随缓冲响应一并 drop(非流式 body 已物化);`continue` 进下一跳前在迭代尾自然 drop(Rust 循环体词法作用域,单请求同一时刻只占一个 provider 的位)。

3. `dispatch_stream` 同位接入;`Ok(AttemptWithRetry::Respond(resp, _))` 分支返回前:

```rust
            Ok(AttemptWithRetry::Respond(resp, _tokens)) => {
                outcome.served_model_id = Some(current.clone());
                // 流式:permit 移交 body 流,流真正结束/断开才释放(spec §5.2)。
                let resp = match permit {
                    Some(p) => crate::proxy::concurrency::attach_permit(resp, p),
                    None => resp,
                };
                return Ok(resp);
            }
```

4. **不变量注释**:在 `attempt_with_retry` 的 doc 注释追加一行:
   `/// 不变量:进入本函数的 sleep(重试等待)边界时,调用方持有的并发 permit 仍在,但任何上一 attempt 的上游 response/stream 已被本函数消费完毕或随作用域 drop——重构时不得在 sleep 间隙持有未消费的上游 body。`

- [ ] **Step 4: 跑全部测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): dispatch 按套餐并发限流(排队/5N 本地 429/流式 permit 转移)"
```

---

### Task 7: 密钥管理命令 + 联动校验 + 启动接线

**Files:**
- Modify: `src-tauri/src/commands.rs`(命令 + Cargo 需要 `getrandom`)
- Modify: `src-tauri/src/Cargo.toml`(即 `src-tauri/Cargo.toml`,加 `getrandom = "0.3"`)
- Modify: `src-tauri/src/lib.rs`(注册 3 个命令;启动时 `reload_auth_cache`——若 Task 5 已在 load 内做则跳过)

**Interfaces:**
- Consumes: Task 1 `AuthKeyMeta`、Task 2 `set/get/delete_auth_key`、Task 5 `AuthCache`。
- Produces(Tauri 命令,前端 Task 9 消费,serde 字段蛇形):
  - `list_auth_keys() -> Vec<AuthKeyMeta>`
  - `create_auth_key(name: String) -> CreatedAuthKey { meta: AuthKeyMeta, key: String }`(完整密钥仅此一次返回)
  - `delete_auth_key(key_id: String) -> ()`(联动校验:最后一个密钥 + 非回环监听 -> 拒绝)

- [ ] **Step 1: 写失败测试**(commands.rs `mod tests`,用现有测试 state 构造模式)

```rust
// 用 grant_secret_consent_core 的测试同款方式搭 state(见 commands.rs 既有测试),
// secrets 用 MemoryStore。

#[tokio::test]
async fn create_then_list_then_auth_cache_updated() {
    let state = test_state().await;
    let created = create_auth_key_core(&state, &app(), "张三".into()).await.unwrap();
    assert!(created.key.starts_with("sk-"));
    assert_eq!(created.key.len(), "sk-".len() + 64);
    assert_eq!(created.meta.name, "张三");
    assert!(created.meta.key_prefix.starts_with("sk-"));
    assert!(state.auth.contains(&created.key)); // 缓存立即生效
    let listed = list_auth_keys_core(&state).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.meta.id);
}

#[tokio::test]
async fn create_auth_key_name_trimmed_and_uniqueness_not_required() {
    // 备注可重名(只作展示);仅 trim。
    let state = test_state().await;
    let a = create_auth_key_core(&state, &app(), "  同名  ".into()).await.unwrap();
    let _b = create_auth_key_core(&state, &app(), "同名".into()).await.unwrap();
    assert_eq!(a.meta.name, "同名");
}

#[tokio::test]
async fn delete_removes_everywhere() {
    let state = test_state().await;
    let c = create_auth_key_core(&state, &app(), "x".into()).await.unwrap();
    delete_auth_key_core(&state, &app(), c.meta.id.clone()).await.unwrap();
    assert!(list_auth_keys_core(&state).await.is_empty());
    assert!(!state.auth.contains(&c.key));
    assert_eq!(state.secrets.get_auth_key(&c.meta.id).unwrap(), None);
}

#[tokio::test]
async fn delete_last_key_blocked_when_remote_bind() {
    let state = test_state().await;
    let c = create_auth_key_core(&state, &app(), "x".into()).await.unwrap();
    state.config.write().await.settings.bind_addr = "0.0.0.0".into();
    let err = delete_auth_key_core(&state, &app(), c.meta.id).await.unwrap_err();
    assert!(err.contains("密钥"), "interlock message: {err}");
}

#[tokio::test]
async fn delete_last_key_allowed_when_loopback() {
    let state = test_state().await;
    let c = create_auth_key_core(&state, &app(), "x".into()).await.unwrap();
    assert!(delete_auth_key_core(&state, &app(), c.meta.id).await.is_ok());
}
```

(app() 为测试用 AppHandle 的既有替代——commands.rs 里 persist 需要 AppHandle;已有测试用 `persist_core`/目录直写模式的话照抄其方式,core 函数接 `dir: &Path` 而非 AppHandle。以仓库现有 `grant_secret_consent_core(dir, state)` 模式为准:core 函数签名统一 `async fn xxx_core(dir: &Path, state: &AppState, ...)`。)

- [ ] **Step 2: 确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml commands::`
Expected: FAIL(core 函数不存在)

- [ ] **Step 3: 实现**

Cargo.toml `[dependencies]` 加 `getrandom = "0.3"`。

commands.rs:

```rust
use crate::config::AuthKeyMeta;

/// create_auth_key 的返回:完整密钥仅此一次下发,前端弹窗展示+复制后不可再查。
#[derive(Serialize)]
pub struct CreatedAuthKey {
    pub meta: AuthKeyMeta,
    pub key: String,
}

/// 32 随机字节 -> "sk-" + 64 个 base62 字符(每字节编 2 字符,62²=3844>256,双射)。
/// 用 getrandom(CSPRNG)而非 fastrand(PRG 不适合凭据,spec §3.2)。
fn generate_client_key() -> Result<String, String> {
    const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("生成密钥失败:{e}"))?;
    let mut out = String::from("sk-");
    for b in bytes {
        out.push(ALPHABET[(b % 62) as usize] as char);
        out.push(ALPHABET[((b / 62) % 62) as usize] as char);
    }
    Ok(out)
}

pub async fn create_auth_key_core(
    dir: &std::path::Path, state: &AppState, name: String,
) -> Result<CreatedAuthKey, String> {
    let name = name.trim().to_string();
    let key = generate_client_key()?;
    let meta = AuthKeyMeta {
        id: format!("ak_{:08x}", fastrand::u32(..)),
        key_prefix: format!("{}…", &key[..8]),
        name,
        created_at: chrono::Local::now().timestamp(),
    };
    // keyring 先写:后端失败则不落 config(避免出现"元数据在、密钥丢"的死键)。
    state.secrets.set_auth_key(&meta.id, &key).map_err(|e| e.to_string())?;
    {
        let mut cfg = state.config.write().await;
        cfg.auth_keys.push(meta.clone());
        crate::config::store::save(dir, &cfg).map_err(|e| e.to_string())?;
    }
    state.auth.insert(key.clone(), meta.id.clone());
    Ok(CreatedAuthKey { meta, key })
}

pub async fn list_auth_keys_core(state: &AppState) -> Vec<AuthKeyMeta> {
    state.config.read().await.auth_keys.clone()
}

pub async fn delete_auth_key_core(
    dir: &std::path::Path, state: &AppState, key_id: String,
) -> Result<(), String> {
    let (full_key, is_last, remote) = {
        let mut cfg = state.config.write().await;
        let is_last = cfg.auth_keys.len() == 1 && cfg.auth_keys[0].id == key_id;
        let remote = !cfg.settings.bind_addr.parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(true);
        if is_last && remote {
            return Err("当前为远程监听(非 127.0.0.1),不能删除最后一个访问密钥;请先把监听地址改回本机".into());
        }
        let idx = cfg.auth_keys.iter().position(|m| m.id == key_id)
            .ok_or_else(|| "密钥不存在".to_string())?;
        cfg.auth_keys.remove(idx);
        crate::config::store::save(dir, &cfg).map_err(|e| e.to_string())?;
        // 找缓存量:删除前从 secrets 读一次拿完整密钥(用于清缓存)
        (state.secrets.get_auth_key(&key_id).ok().flatten(), is_last, remote)
    };
    let _ = (is_last, remote);
    if let Err(e) = state.secrets.delete_auth_key(&key_id) {
        tracing::warn!("delete_auth_key: keyring 清理失败 {key_id}:{e}(元数据已删,忽略)");
    }
    if let Some(k) = full_key {
        state.auth.remove(&k);
    }
    Ok(())
}

#[tauri::command]
pub async fn list_auth_keys(state: State<'_, AppState>) -> Result<Vec<AuthKeyMeta>, String> {
    Ok(list_auth_keys_core(&state).await)
}

#[tauri::command]
pub async fn create_auth_key(
    app: tauri::AppHandle, state: State<'_, AppState>, name: String,
) -> Result<CreatedAuthKey, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    create_auth_key_core(&dir, &state, name).await
}

#[tauri::command]
pub async fn delete_auth_key(
    app: tauri::AppHandle, state: State<'_, AppState>, key_id: String,
) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    delete_auth_key_core(&dir, &state, key_id).await
}
```

lib.rs `invoke_handler` 注册三个命令。启动时(若 Task 5 的 load 未覆盖)`state.reload_auth_cache()`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(auth): 密钥管理命令(create/list/delete)+ getrandom 生成 + 删最后密钥联动校验"
```

---

### Task 8: 监听地址生效 + 环境片段 + `set_bind_addr` 命令

**Files:**
- Modify: `src-tauri/src/proxy/server.rs`(serve/serve_once 读 `settings.bind_addr`)
- Modify: `src-tauri/src/proxy/state.rs`(polling_loop 随 serve_once 一致)
- Modify: `src-tauri/src/commands.rs`(`set_bind_addr` + `env_snippet` 本机 IP + `get_env_snippet`)

**Interfaces:**
- Consumes: `Settings.bind_addr`(Task 1)、auth 联动逻辑(Task 7 模式)。
- Produces: `set_bind_addr(addr: String) -> ()`(校验 IpAddr + 非回环须有密钥 + 落盘 + 重启代理);`EnvSnippet` URL 在远程模式用本机局域网 IP(取不到回退 127.0.0.1)。

- [ ] **Step 1: 写失败测试**

server.rs(纯函数化 bind 地址解析):

```rust
#[test]
fn parse_bind_addr_falls_back_to_loopback() {
    assert_eq!(parse_bind_addr("0.0.0.0"), std::net::IpAddr::from([0, 0, 0, 0]));
    assert_eq!(parse_bind_addr("192.168.1.5"), "192.168.1.5".parse::<std::net::IpAddr>().unwrap());
    assert_eq!(parse_bind_addr("not-an-ip"), std::net::IpAddr::from([127, 0, 0, 1]));
    assert_eq!(parse_bind_addr(""), std::net::IpAddr::from([127, 0, 0, 1]));
}
```

commands.rs:

```rust
#[tokio::test]
async fn set_bind_addr_rejects_remote_without_keys() {
    let state = test_state().await; // auth_keys 空
    let err = set_bind_addr_core(&dir, &state, "0.0.0.0".into()).await.unwrap_err();
    assert!(err.contains("密钥"));
    // config 未被改坏
    assert_eq!(state.config.read().await.settings.bind_addr, "127.0.0.1");
}

#[tokio::test]
async fn set_bind_addr_rejects_invalid_ip() {
    let state = test_state().await;
    assert!(set_bind_addr_core(&dir, &state, "abc".into()).await.is_err());
}

#[tokio::test]
async fn set_bind_addr_remote_ok_with_key() {
    let state = test_state().await;
    let _ = create_auth_key_core(&dir, &state, "k".into()).await.unwrap();
    // 绑定 0.0.0.0:测试环境允许绑定(端口 0 不可行则此用例仅校验落盘,
    // 见 Step 3 关于 serve 的注释)
    set_bind_addr_core(&dir, &state, "0.0.0.0".into()).await.unwrap();
    assert_eq!(state.config.read().await.settings.bind_addr, "0.0.0.0");
}

#[test]
fn env_snippet_uses_given_host() {
    let s = env_snippet(6950, "192.168.1.5");
    assert_eq!(s.anthropic_base_url, "http://192.168.1.5:6950");
    assert_eq!(s.openai_base_url, "http://192.168.1.5:6950/v1");
}

#[test]
fn local_ip_helper_returns_something_parseable_or_none() {
    // 不强断具体值(环境相关);只要求 None 或合法 IP。
    if let Some(ip) = local_ip() {
        assert!(ip.to_string().parse::<std::net::IpAddr>().is_ok());
    }
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml proxy::server commands::`
Expected: FAIL

- [ ] **Step 3: 实现**

server.rs:

```rust
/// 解析 settings.bind_addr;非法值回退 127.0.0.1(读时归一,同 normalize_log_level)。
pub fn parse_bind_addr(s: &str) -> std::net::IpAddr {
    s.trim().parse().unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]))
}
```

`serve` / `serve_once` 开头读 `let addr = { state.config.read().await.settings.bind_addr.clone() };`(`polling_loop` 的 `serve_once` 调用同源,无需改签名),`TcpListener::bind((addr, candidate))`。现有 e2e 端口测试默认 bind_addr=127.0.0.1,行为不变。

commands.rs:

```rust
/// 零依赖取本机局域网 IP:UDP connect 外部地址后读 local_addr(不发实际包)。
/// 纯内网/断网环境失败 -> None(片段回退 localhost,UI 提示手动替换;spec §6)。
fn local_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip())
}

fn env_snippet(port: u16, host: &str) -> EnvSnippet {
    EnvSnippet {
        anthropic_base_url: format!("http://{host}:{port}"),
        openai_base_url: format!("http://{host}:{port}/v1"),
    }
}

#[tauri::command]
pub async fn get_env_snippet(state: State<'_, AppState>) -> Result<EnvSnippet, String> {
    let port = state.bound_port().ok_or_else(|| "proxy is not running".to_string())?;
    let bind_addr = state.config.read().await.settings.bind_addr.clone();
    let host = match crate::proxy::server::parse_bind_addr(&bind_addr) {
        ip if ip.is_loopback() => "127.0.0.1".to_string(),
        _ => local_ip().map(|ip| ip.to_string()).unwrap_or_else(|| "127.0.0.1".into()),
    };
    Ok(env_snippet(port, &host))
}

pub async fn set_bind_addr_core(
    dir: &std::path::Path, state: &AppState, addr: String,
) -> Result<(), String> {
    let ip: std::net::IpAddr = addr.trim().parse()
        .map_err(|_| format!("无效的监听地址「{addr}」;应为 IP 地址,如 0.0.0.0"))?;
    // 联动校验:远程监听必须有至少一个密钥(spec §6)。
    let keys_empty = state.config.read().await.auth_keys.is_empty();
    if !ip.is_loopback() && keys_empty {
        return Err("远程监听前请先创建访问密钥(设置 → 远程访问 → 创建密钥)".into());
    }
    {
        let mut cfg = state.config.write().await;
        cfg.settings.bind_addr = ip.to_string();
        crate::config::store::save(dir, &cfg).map_err(|e| e.to_string())?;
    }
    // 与 set_port 同款重启流程:停旧服务/轮询 -> 单次绑定 -> 失败进 bind_error+轮询。
    restart_proxy(state).await.map_err(|e| e.to_string())
}
```

(`restart_proxy` 为从 `set_port`/`restart_server` 中提的共同停启段——若提取改动过大,直接内联复制该 30 行流程,以改动小者为准。)

`#[tauri::command] set_bind_addr(app, state, addr)` 包 core(同 Task 7 模式),lib.rs 注册。

既有测试 `env_snippet_builds_urls_with_bound_port` 改为传 `"127.0.0.1"`。

- [ ] **Step 4: 跑全部测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/server.rs src-tauri/src/proxy/state.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(server): 可配置监听地址(远程联动校验)+ 环境片段局域网 IP + set_bind_addr"
```

---

### Task 9: 前端类型与命令桥 + system store

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/commands.ts`
- Modify: `src/stores/system.ts`

**Interfaces:**
- Consumes: Task 1/7/8 的 Rust serde 形状(蛇形字段)。
- Produces: `Provider.max_concurrency?: number | null`;`AuthKeyMeta`、`CreatedAuthKey`、`SettingsView.bind_addr: string`;`listAuthKeys/createAuthKey/deleteAuthKey/setBindAddr` 包装;system store 的 `authKeys` state + `loadAuthKeys/saveBindAddr` actions。Task 10/11 消费。

- [ ] **Step 1: types.ts 增类型**

```ts
// Provider 接口内增(与 config/types.rs 对齐,蛇形):
  max_concurrency?: number | null;

// 文件任意合适位置:
// Mirrors src-tauri AuthKeyMeta (config/types.rs).
export interface AuthKeyMeta {
  id: string;
  name: string;
  key_prefix: string;
  created_at: number;
}

// Mirrors src-tauri CreatedAuthKey (commands.rs):完整密钥仅创建时返回一次。
export interface CreatedAuthKey {
  meta: AuthKeyMeta;
  key: string;
}

// SettingsView 增:
  bind_addr: string;
```

- [ ] **Step 2: commands.ts 增包装**(紧跟既有风格,每行一个)

```ts
export const listAuthKeys = () => invoke<AuthKeyMeta[]>("list_auth_keys");
export const createAuthKey = (name: string) =>
  invoke<CreatedAuthKey>("create_auth_key", { name });
export const deleteAuthKey = (keyId: string) =>
  invoke<void>("delete_auth_key", { keyId });
export const setBindAddr = (addr: string) => invoke<void>("set_bind_addr", { addr });
```

顶部 `import type { ... }` 补 `AuthKeyMeta, CreatedAuthKey`。

- [ ] **Step 3: system store 增状态与动作**(照 store 既有"变更后重取"惯例)

```ts
  const authKeys = ref<AuthKeyMeta[]>([]);

  async function loadAuthKeys() {
    authKeys.value = await api.listAuthKeys();
  }

  async function saveBindAddr(addr: string) {
    await api.setBindAddr(addr);
    await refresh(); // 重取 settings(端口/bind_addr 等权威值)
    await loadAuthKeys();
  }
```

`refresh()`(或等价的设置加载函数)确保已含新 `bind_addr` 字段(类型自动带上)。导出 `authKeys, loadAuthKeys, saveBindAddr`。

- [ ] **Step 4: 类型检查**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS(0 error)

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts
git commit -m "feat(frontend): 认证密钥/监听地址类型与命令桥、system store 扩展"
```

---

### Task 10: Provider.vue 最大并发字段

**Files:**
- Modify: `src/views/Provider.vue`

**Interfaces:**
- Consumes: `Provider.max_concurrency`(Task 9)、`upsertProvider`(既有)。

- [ ] **Step 1: 表单状态与联动**

`FormState` 增 `maxConcurrency: number | null`;`blank()` 增 `maxConcurrency: null`;`openEdit` 增 `form.maxConcurrency = p.max_concurrency ?? null;`。

`onVendorChange` 里,新建模式下按厂商给预填提示值(留空 = 不限;**预填 0 语义等同不限,故只做 placeholder 不预填数值**,避免猜错套餐等级):

```ts
// 各厂商的并发输入提示(不预填数值:同级厂商不同套餐等级并发不同,由用户按套餐页填写)。
const concurrencyPlaceholder: Record<string, string> = {
  zhipu: "智谱 Coding 套餐不同等级并发不同,请按套餐等级填写;留空 = 不限制",
  "volcengine-coding": "火山 Coding 套餐不同等级并发不同,请按套餐等级填写;留空 = 不限制",
  "volcengine-agent": "火山方舟套餐请按实际并发上限填写;留空 = 不限制",
  "qianwen-token": "千问套餐请按实际并发上限填写;留空 = 不限制",
  deepseek: "按量计费通常不限并发,可留空",
};
const concurrencyHint = computed(
  () => concurrencyPlaceholder[form.vendor] ?? "套餐最大并发请求数;留空 = 不限制",
);
```

- [ ] **Step 2: 模板输入**(放在 API 密钥输入之后、弹窗表单内)

```vue
<NFormItem label="最大并发">
  <NInputNumber
    v-model:value="form.maxConcurrency"
    :min="1"
    :max="256"
    :placeholder="concurrencyHint"
    style="width: 100%"
    clearable
  />
</NFormItem>
```

(`NInputNumber` 补进 naive-ui import;clearable 清空后值为 `null`。)

- [ ] **Step 3: save() 组装**

`const provider: Provider = { ... }` 增一行:

```ts
    max_concurrency:
      form.maxConcurrency != null && form.maxConcurrency > 0 ? form.maxConcurrency : null,
```

保存成功后的重取逻辑沿用既有(config store action)。

- [ ] **Step 4: 验证**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/views/Provider.vue
git commit -m "feat(frontend): 套餐表单最大并发输入(空=不限,按厂商提示)"
```

---

### Task 11: Settings.vue 远程访问区块(监听地址 + 密钥管理 + 组合流)

**Files:**
- Modify: `src/views/Settings.vue`

**Interfaces:**
- Consumes: Task 9 的 store 动作与类型、`useSystemStore` 既有 `envSnippet`/`refresh`。

- [ ] **Step 1: script 逻辑**

```ts
const bindAddr = ref<string>("127.0.0.1");
watch(
  () => system.settings?.bind_addr,
  (v) => { if (v) bindAddr.value = v; },
  { immediate: true },
);
const isRemote = computed(() => bindAddr.value !== "127.0.0.1");
const hasKeys = computed(() => system.authKeys.length > 0);

const addrOptions = [
  { label: "仅本机(127.0.0.1)", value: "127.0.0.1" },
  { label: "所有网卡(0.0.0.0,局域网可访问)", value: "0.0.0.0" },
];

async function onAddrChange(v: string) {
  if (v !== "127.0.0.1" && !hasKeys.value) {
    // 组合流:空密钥表切远程 -> 先弹创建密钥,成功后再保存地址(spec §6.3)。
    msg.warning("远程访问需要认证:请先创建首个访问密钥");
    showCreateKey.value = true; // 创建成功回调里再 saveBindAddr(v)
    pendingAddr.value = v;
    bindAddr.value = "127.0.0.1"; // 选择回退,等密钥建好
    return;
  }
  try {
    await system.saveBindAddr(v);
    msg.success(v === "127.0.0.1" ? "已切回本机监听" : "已开启远程监听");
  } catch (e) {
    msg.error(String(e));
    bindAddr.value = system.settings?.bind_addr ?? "127.0.0.1";
  }
}

const showCreateKey = ref(false);
const newKeyName = ref("");
const createdKey = ref<string | null>(null); // 完整密钥仅展示一次
const pendingAddr = ref<string | null>(null);

async function onCreateKey() {
  const name = newKeyName.value.trim();
  if (!name) { msg.warning("请填写备注名称,如「张三的 Claude Code」"); return; }
  try {
    const r = await api.createAuthKey(name);
    createdKey.value = r.key;
    await system.loadAuthKeys();
    if (pendingAddr.value) {
      await system.saveBindAddr(pendingAddr.value); // 组合流续接
      pendingAddr.value = null;
      msg.success(`密钥已创建,远程监听已开启`);
    }
    newKeyName.value = "";
  } catch (e) {
    msg.error(String(e));
  }
}

async function onDeleteKey(id: string, name: string) {
  dialog.warning({
    title: "删除访问密钥",
    content: `确定删除「${name}」?使用该密钥的成员将立即无法访问。`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        await api.deleteAuthKey(id);
        await system.loadAuthKeys();
        msg.success("已删除");
      } catch (e) {
        msg.error(String(e)); // 联动拒绝(最后一个密钥+远程监听)在此展示
      }
    },
  });
}

onMounted(() => { void system.loadAuthKeys(); });
```

(import 补:`computed, onMounted, ref, watch`、`NInput, NModal, NPopconfirm, NSelect, NTag, NDescriptions`(按实际使用)、`api`、`AuthKeyMeta` 类型按需。)

- [ ] **Step 2: 模板**(放在"端口"卡片之后)

```vue
<NCard title="远程访问" size="small">
  <NSpace vertical>
    <NFormItem label="监听地址" :show-feedback="false">
      <NSelect v-model:value="bindAddr" :options="addrOptions" style="width: 100%"
               @update:value="onAddrChange" />
    </NFormItem>
    <div v-if="isRemote" style="font-size: 12px; opacity: 0.7">
      局域网设备请连接 {{ system.envSnippet?.anthropic_base_url }},并携带
      <code>ANTHROPIC_AUTH_TOKEN: sk-…</code>;Windows 首次开启需在防火墙放行本应用。
    </div>

    <NDivider style="margin: 8px 0">访问密钥(多人共享时每个成员一个)</NDivider>
    <NSpace v-if="system.authKeys.length" vertical>
      <NSpace v-for="k in system.authKeys" :key="k.id" align="center" justify="space-between">
        <span>{{ k.name }} <NTag size="small">{{ k.key_prefix }}</NTag></span>
        <NButton size="tiny" quaternary type="error" @click="onDeleteKey(k.id, k.name)">删除</NButton>
      </NSpace>
    </NSpace>
    <NEmpty v-else description="尚未创建密钥(本机使用无需认证)" size="small" />
    <NButton size="small" @click="showCreateKey = true; createdKey = null">创建密钥</NButton>
  </NSpace>
</NCard>

<NModal v-model:show="showCreateKey" preset="card" title="创建访问密钥" style="max-width: 480px">
  <NSpace v-if="!createdKey" vertical>
    <NInput v-model:value="newKeyName" placeholder="备注名称,如「张三的 Claude Code」"
            @keydown.enter="onCreateKey" />
    <NButton type="primary" @click="onCreateKey">生成密钥</NButton>
  </NSpace>
  <NSpace v-else vertical>
    <div style="font-size: 12px; color: var(--error-color, #d03050)">
      请立即复制保存:完整密钥仅显示这一次,之后无法再查看。
    </div>
    <NInput :value="createdKey" readonly />
    <NButton @click="navigator.clipboard.writeText(createdKey); msg.success('已复制')">复制</NButton>
  </NSpace>
</NModal>
```

(组件按 Settings.vue 现有 import 风格补齐;`NDivider/NEmpty/NTag/NModal/NInput/NSelect` 未 import 的加上。)

- [ ] **Step 3: 验证**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS

- [ ] **Step 4: 手动冒烟(pnpm tauri dev)**

1. Provider 编辑:填/清空最大并发,保存后重开确认回显。
2. 设置 → 远程访问:创建密钥(弹窗显示一次完整密钥并复制)→ 切 0.0.0.0(不再被拦)→ 删除该密钥被拒(提示先改回本机)→ 改回 127.0.0.1 → 删除成功。
3. 带 `Authorization: Bearer sk-…` 与 `x-api-key` 各打一次 `/v1/messages`,通过;错 key 401。
4. 最大并发=1 的套餐下并发 6 个请求,观察第 6 个收到本地 429 且 breaker 未熔断(日志 `local concurrency limit hit`)。

- [ ] **Step 5: Commit**

```bash
git add src/views/Settings.vue
git commit -m "feat(frontend): 设置页远程访问区块(监听地址/密钥管理/组合流/防火墙提示)"
```

---

## Self-Review 记录

- **Spec 覆盖**:§3(类型)=Task 1;§3.2 getrandom+仅一次下发=Task 7;§4 中间件=Task 5;§5 并发=Task 3/4/6;§6 监听+联动+IP=Task 7(delete 拦截)+Task 8(set 拦截/serve/片段);§7 命令与前端=Task 7/9/10/11;§8 边界(断开/跨 provider/改小/keyring 降级/回环回退)散布 Task 3/5/6/8;§9 测试=各 Task Step 1。日志 `key=` 字段=Task 5。Windows 防火墙提示=Task 11 模板。**未覆盖项**:无。
- **类型一致性**:`ConcurrencyPermit`/`attach_permit`(Task 3 定义,Task 6 消费)、`AuthCache.contains/insert/remove/key_id`(Task 5 自洽)、`create_auth_key_core(dir, state, name)`(Task 7 定义,Task 8 测试复用)、`max_concurrency` 蛇形贯穿。已核对。
- **占位符**:Task 5 Step 1 的 `test_state_auth`、Task 6 的 `single_provider_state` 是测试 helper 的搭建说明(内容已给出差异点,基座为既有 `test_state`/`chain_state`),非实现占位。Task 8 `restart_proxy` 提取给了"提取或内联"二选一的明确判据(改动小者)。
