# 临时限流原地重试（Transient Throttle In-Place Retry）— 设计规格

> 日期：2026-08-04
> 状态：设计完成（待评审）
> 关联：主规格 `2026-07-27-switchlm-llm-proxy-design.md`（§3.5 Fallback 边界、§4 熔断、§6.3 reset_at 喂熔断器）；姊妹篇 `2026-08-01-traceable-fallback-logging-design.md`（`TripReason::Transient/Exhausted` 的来源）

## 1. 背景与目标

现状（`proxy/dispatch.rs`）：某跳模型在**尚未向客户端转发任何内容**时返回限流错误 → `compute_recover_at` 实时查额度（绕过用量缓存）：

- 额度已耗尽（任意窗口 ≥100%）→ `TripReason::Exhausted`，`recover_at` = 套餐重置时间；
- 否则（额度可用，或查询失败 / 该厂商无用量适配器）→ `TripReason::Transient`，`recover_at` = `now + cooldown_seconds`（默认 300s）。

随后**立即**熔断 + 走 fallback 链。即便是"额度还在、只是被瞬时 RPM / 突发限流"（几秒即可恢复）的情况，也会立刻切到另一个模型，并把当前模型冷却数分钟——既浪费了一次本可自愈的请求，又让 fallback 模型承担了本不必承担的流量。

**目标**：临时限流时，先在**同一模型**上等待 + 原地重试若干次；恢复则照常返回（**不熔断**）；重试耗尽才走现行熔断 + fallback。重试次数与每次等待时长**可配（每模型）**。

**核心场景**：编码套餐常遇"按分钟 RPM / 突发"限流——请求被 429，但额度窗口远未耗尽，几秒后即恢复。与其立刻切模型，不如原地等几秒重试，保持会话连续、避免不必要的故障转移。

**成功标准**：

1. 临时限流（`Transient`）且 `retry_count > 0` → 原地 `sleep` + 重试，任一次成功则正常返回且**不熔断**；
2. 重试耗尽 → 走现行熔断（`Transient`，`recover_at` = `now + cooldown_seconds`）+ fallback，行为与今天一致；
3. 额度耗尽（`Exhausted`）→ **不重试**，直接熔断（`Exhausted`）+ fallback；
4. 流式与非流式**一致**支持（限流检测仍只在 §3.5"未转发内容"边界之前）；
5. 默认开启（`retry_count = 2`、`retry_delay_secs = 5`），`retry_count = 0` 关闭。

## 2. 设计原则与关键决策

| 决策 | 结论 | 理由 |
|---|---|---|
| 配置粒度 | **per-Model**（`Model.retry_count` / `retry_delay_secs`） | 与 `cooldown_seconds` 同层；不同套餐 / 模型限流特征差异大，按模型调最灵活。 |
| 字段类型 | `u32` / `u64`，带 serde 默认（2 / 5），**非 Option** | 本版本未发布、无需兼容；与 `fallback_strategies_enabled` 同风格（manual `Default` + serde default）。`0` = 关闭。 |
| 默认 | **默认开启**（2 次 / 5 秒） | 用户主动要该能力；无兼容包袱。 |
| 触发条件 | `compute_recover_at` 判为 `Transient` 即重试（含"查询失败 / 无适配器"） | 查询失败常由同一波限流导致；重试无害（最多多等 ~10s 再 fallback）；与现有 `Transient` 兜底分类一致。 |
| Exhausted | **不重试**，直接熔断 + fallback | 额度真耗尽，重试无意义。 |
| 额度查询次数 | **每跳 1 次**（同现状） | 首次限流查一次决定 Transient/Exhausted；重试循环内不重查；耗尽时不重查，复用首次结果。 |
| 实现结构 | **方案 A**：统一 `AttemptOutcome` + `attempt_with_retry` 包装 | 流式 / 非流式共用一套重试逻辑；顺带把流式三处限流检测点（状态码 / 非 2xx body / translate 首事件）收敛进 `stream_attempt`。 |
| 注入位置 | dispatch 每跳"调用上游"处包一层 | 链式降级 / 冷却跳过 / 防环 / 跳数上限**零改动**；重试不消耗 `MAX_FALLBACK_HOPS`。 |
| recover_at 口径 | 复用首次 `compute_recover_at` 的结果 | 瞬时熔断的 `recover_at` 从首次限流时刻起算——限流正是自该时刻开始，口径更准；且避免 trip 时二次查额度。 |

## 3. 数据结构（`config/types.rs`）

`Model` 增两字段：

```rust
pub struct Model {
    // ... 既有字段（id / provider_id / upstream_model_id / cooldown_seconds / fallback_*）...
    /// 临时限流（额度未耗尽）时的原地重试次数。0 = 关闭（立即熔断 + fallback）。默认 2。
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
    /// 每次重试前等待秒数。默认 5。
    #[serde(default = "default_retry_delay")]
    pub retry_delay_secs: u64,
}
fn default_retry_count() -> u32 { 2 }
fn default_retry_delay() -> u64 { 5 }
```

`Model` 已有 manual `impl Default`（因 `fallback_strategies_enabled` 须默认 `true`）。在其中新增两字段，**复用同一组 serde default 函数**，保证"反序列化缺失字段"与"`..Default::default()`"二者默认值 100% 一致，不会发散：

```rust
impl Default for Model {
    fn default() -> Self {
        Self {
            // ... 既有字段 ...
            retry_count: default_retry_count(),      // = 2，同 #[serde(default = "default_retry_count")]
            retry_delay_secs: default_retry_delay(), // = 5，同 #[serde(default = "default_retry_delay")]
        }
    }
}
```

`mk_model` 等测试 helper 的取值见 §7。

**持久化**：两字段随 `AppConfig` → `app_config.json` 自动落盘（同 `cooldown_seconds`），无新持久化代码。密钥仍只走 OS keyring，不在此文件。

## 4. dispatch 重试逻辑（`proxy/dispatch.rs`）

### 4.1 统一"一次尝试"的出参

把现有非流的 `CallOutcome` 提为两条路径共用：

```rust
/// 一次上游尝试的结果。`RateLimited` 只在"尚未向客户端转发任何内容"时返回（§3.5 边界）。
enum AttemptOutcome {
    /// 转发该响应（成功，或非限流上游错误原样透传）。
    Respond(Response<Body>),
    /// 流前限流（HTTP 状态码 / 非 2xx body / translate 首 SSE 事件）。
    RateLimited,
}
```

- **非流式**：`select_and_call` 返回 `Result<AttemptOutcome, ProxyError>`（= 现状 `CallOutcome` 改名，形状不变）。
- **流式**：新抽 `async fn stream_attempt(snap, req, path, key, echo_model, vendor) -> Result<AttemptOutcome, ProxyError>`，把 `dispatch_stream` 循环体里的「`send_stream` → 状态码检查 → 非 2xx body 缓冲检查 → 2xx commit（translate 首事件 / passthrough 原样）」收敛进来。运输错误仍 `Err`（透传 502，不重试、不 fallback，§8 不变）。`dispatch_stream` 循环体改为调用 `stream_attempt`，消除内联的三处限流检测点。

### 4.2 `attempt_with_retry` 包装

```rust
/// `AttemptOutcome` 加上首次限流时一次性算出的恢复信息（供调用方 trip，不二次查额度）。
enum AttemptWithRetry {
    Respond(Response<Body>),
    RateLimited(Recover),   // 现有私有 struct Recover { recover_at: Option<i64>, reason: TripReason }
}

/// 调一次上游；若返回流前限流：
///  - 首次：`compute_recover_at` 决定 Exhausted / Transient。
///    · Exhausted        → 立即返回 `RateLimited(Exhausted)`（**不重试**）。
///    · Transient 且 retry_count>0 → 进入重试循环。
///    · Transient 且 retry_count==0 → 立即返回 `RateLimited(Transient)`（等价今日行为）。
///  - 重试中仍限流：`sleep(retry_delay_secs)` 后再来，最多 `retry_count` 次。
///  - 任一次 `Respond` → 返回（**不熔断**）。
///  - 重试耗尽 → 返回 `RateLimited(Transient)`。
///
/// `call_once` 为按路径绑定的一次尝试：非流 → `select_and_call`；流 → `stream_attempt`。
async fn attempt_with_retry(
    state: &AppState,
    snap: &ModelSnapshot,
    now: i64,
    req_id: &str,
    retry_count: u32,
    retry_delay_secs: u64,
    call_once: /* 按路径的一次尝试（闭包或 enum 分发，留给实现） */,
) -> Result<AttemptWithRetry, ProxyError>;
```

要点：

- 额度查询**只发生 1 次**（首次限流的 `compute_recover_at`）；其 `Recover { recover_at, reason }` 随 `RateLimited` 回传给调用方 trip，**不二次查询**。
- `retry_count == 0` → 仅一次尝试，限流即返回 `RateLimited(Transient)`，等价今日行为。
- 等待用 `tokio::time::sleep`（测试用 `tokio::time::pause` 让其自动推进，无真实墙钟延迟）。
- 每次重试记一行日志（§6）。
- **请求体无"重复消费"问题**：`dispatch` 入参是 `Bytes`，解析为缓冲好的 `serde_json::Value`；每次 `call_once` 各自 `req.clone()` + `serde_json::to_vec` 重新生成 `Vec<u8>`。流式仅指响应（`resp.bytes_stream()`），请求体从不是流——重试直接复用 `req` 即可，不存在 "Body already consumed" / 空 body 风险。

### 4.3 dispatch 每跳接入

非流式 / 流式的"调用上游"分支由直接 `select_and_call` / 内联流式检测，改为 `attempt_with_retry(...)`：

```rust
match attempt_with_retry(...).await? {
    AttemptWithRetry::Respond(resp) => {
        outcome.served_model_id = Some(current.clone());
        return Ok(resp);
    }
    AttemptWithRetry::RateLimited(rec) => {
        state.health.trip(&current, rec.recover_at, now, rec.reason);
        last_error = "rate-limited".to_string();
        // 现行 hop_and_advance / next_fallback_id（时间感知、防环、跳数上限）零改动
        hop_and_advance!(ratelimit_reason_detail(rec.reason));
    }
}
```

冷却跳过（`is_cooling`）、missing model、无 backend、缺 key 等分支**不变**。`compute_recover_at` 不再在调用方调用（已移入包装器）。

## 5. 命令与前端

- **`commands.rs`**：`apply_model_failover` 与 `set_model_failover` 增参 `retry_count: u32, retry_delay_secs: u64`，写入 `Model`。`dispatch` 内的 `ModelSnapshot` 带这两字段。
- **`src/lib/types.ts`**：`Model` 增 `retry_count: number; retry_delay_secs: number;`——**非可选、不可空**（后端是 `u32`/`u64` 非 `Option`，且无 `skip_serializing_if`，serde 恒序列化为数字）。这与 `cooldown_seconds?: number | null`（后端 `Option<u64>`，故可空）的差异化是刻意的：required 类型杜绝 `{...model, ...form}` 展开时把 `undefined` 回写给后端、抹掉配置的隐患。
- **`src/lib/commands.ts`**：`setModelFailover` 增两参。
- **`src/views/Fallback.vue`**：模态框"熔断冷却（秒）"下方加一行——`重试次数`（`NInputNumber`，min 0，placeholder `2`）+ `重试间隔（秒）`（min 0，placeholder `5`），附说明"仅临时限流（额度未耗尽）时原地重试；耗尽后仍走熔断 fallback"。`FormState` / `openEdit` / `doSave` 同步。凡今日 round-trip `cooldown_seconds` 的其他模型编辑面（如 `Models.vue` 编辑弹窗）须同样 round-trip 这两字段，避免编辑模型时把重试配置抹掉。
- **卡片展示（可选）**：`Models.vue` 列表行在 `cooldown {n}s` 同级显示 `重试 2×5s`。

## 6. 日志（沿用 `req=<id>` 关联约定）

- 每次重试一行：`retry 1/2 after 5s (transient)`（`target: switchlm::proxy`，带 `req`，vendor/model 取自快照——遵循"记 vendor + upstream 真实名，不记内部 id"）。
- 恢复：现 `forward ok` 行已带 `ms`（会反映含重试的总耗时），无需新增。
- 耗尽：走现 `fallback hop` 行（`reason=rate-limited (transient)`），与今日一致。

## 7. 测试（`dispatch.rs` 内 `wiremock` + `FakeClock`；sleep 用 `tokio::time::pause`）

**测试 helper 约定**：现有 `mk_model`（fallback 链测试 helper）显式设 `retry_count: 0`，使既有"429 → 立即 fallback"用例（`fallback_on_rate_limit`、`all_rate_limit_exhausted`、`deepseek_402_falls_back`、`fallback_cycle_breaks`、`strategy_entry_model_rate_limits_walks_its_chain`、`failover_*` 等）**行为与断言不变**。重试专测另构 `retry_count > 0` 的模型。其余 `..Default::default()` 的 `Model` 构造（`commands.rs` / `strategies.rs` 的配置往返测试）拿到默认 2 但不走 dispatch-on-429，不受影响；凡确实把 429 驱过 dispatch 的测试（如 `e2e_zhipu`）须显式设 `retry_count: 0` 或更新断言。

新增用例：

1. **Transient + 重试恢复**：A 第 1、2 次 429、第 3 次 200 → 返回 A 内容，**未熔断**（`!is_cooling`），mock 收到 3 次请求。
2. **重试耗尽**：A 始终 429（额度查询返回可用，如 85%）→ 熔断（`Transient`，`recover_at = now + cooldown`）+ 走 fallback B。
3. **Exhausted 不重试**：额度查询返回 ≥100% → mock 仅 1 次请求，熔断（`Exhausted`，`recover_at` = 重置时间）+ fallback。（现有 `rate_limit_with_exhausted_quota_uses_reset_at` 仍过。）
4. **`retry_count = 0`**：Transient 也不重试（等价今日，mock 仅 1 次）。
5. **重试中收到非限流错误**（401）→ 透传，不重试、不熔断。
6. **流式**：首事件限流 → 重试后第 N 次返回正常流 → 正常转发，未熔断。
7. **配置默认值对齐**：`Model::default().retry_count == default_retry_count()` 且 `== 2`、`retry_delay_secs == default_retry_delay()` 且 `== 5`；serde 往返（JSON 缺这两字段反序列化也得同值）——锁死"代码默认"与"反序列化默认"一致。

## 8. 边界与注意

- **§3.5 边界不变（最关键）**：一旦向客户端转发了内容（流式 commit 之后），后续错误仍透传、**不重试**——重试只发生在"未转发内容"的限流检测点。这覆盖流式三处检测点（状态码 / 非 2xx body / translate 首事件）与全部非流式检测点。
- **流式客户端超时**：重试期间客户端在等响应头；`retry_count × retry_delay` 过大会触发 agent 侧超时——由用户配置自控（与 `cooldown_seconds` 同理，不强加后端硬上限）。默认 `2 × 5s ≈ 10s` 在多数 agent 超时之内。
- **客户端断开（不在本特性范围）**：重试 `sleep` 与既有 `send_stream().await` / `select_and_call().await` 同属 handler 内的 await 点，继承**同一套**断开语义——axum 0.7 默认 `axum::serve`（`server.rs`）在连接断开时 drop handler future，从而取消 `sleep`。代码库现状对所有 await 均**无**显式 cancel 探测（含无 `timeout` 的 `reqwest::Client::new()` 调用），故仅为 retry sleep 单独接入 `CancellationToken` 既不一致也属 YAGNI；若要感知断开，应作为 handler 级独立加固（并补 reqwest timeout），另开任务。
- **不重复查额度**：每跳 1 次；重试循环内与耗尽时均不重查。
- **只对"首次限流"重试**：模型若已在冷却（被旁路）则根本不调用 → 无重试，不与熔断状态冲突。
- **不消耗跳数**：重试在单跳内，仍受 `MAX_FALLBACK_HOPS = 8` 与 visited-set 防环约束。
- **recover_at 口径**：瞬时熔断 `recover_at` 取自首次限流时的 `compute_recover_at`（含等待耗时的 ~10s 偏移，见 §2 决策表——口径更准且省一次查询）。代码注释须写明：冷却自首次限流时刻 T₀ 起算（而非 trip 时刻），故实际冷却 ≈ `cooldown_seconds − retry 耗时`。

## 9. 不做的事（YAGNI）

- **不做指数退避 / 抖动**：固定 `retry_delay_secs`；用户可按模型手调。编码套餐的瞬时 RPM 限流通常几秒即恢复，线性等够即可。
- **不按厂商 / 套餐差异化默认**：默认 2/5 通用于所有 vendor；`vendor` 仍是路由 key，但不参与重试参数选择。
- **不加重试成功 / 失败计数到 UI**：仅在日志体现（`retry n/m`、`forward ok ms`）。
- **不改 `MAX_FALLBACK_HOPS`、不改冷却 / 防环逻辑**。
