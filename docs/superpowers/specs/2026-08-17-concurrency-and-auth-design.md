# Per-Plan Concurrency Limits & Client Auth (sk-xxx)

**Date:** 2026-08-17
**Status:** Design — pending implementation plan
**Branch:** dev directly on `main` (repo convention)

## 1. Problem

Two gaps stand between SwitchLM as a personal local proxy and SwitchLM as a shared service:

1. **并发没有守门。** Coding 套餐(智谱/火山)有账号级并发上限,同级厂商不同套餐等级(lite/pro/max)上限还不一样。多个 agent 或多人同时打满时,上游直接 429,触发熔断 + fallback——不是额度耗尽,只是并发瞬时超限,却让整个套餐进入冷却,殃及所有用户。
2. **代理无认证、只绑回环。** 服务只能单机使用。多人共享套餐(家庭/小团队)需要:代理可远程访问 + 客户端密钥认证,才能安全地把套餐能力提供给多个人。

## 2. Decisions (locked in brainstorming)

| Decision | Choice |
|---|---|
| 并发超限语义 | **排队等待**(不立即拒绝) |
| 排队上限 | 每套餐在途上限 = **5 × max_concurrency**(运行中 + 排队中合计);超过 → 本地 429 |
| 并发上限归属 | `Provider.max_concurrency: Option<u32>`(套餐/账号级);`None` = 不限。默认值只做 UI 预填,不进 catalog——套餐等级差异由用户手填 |
| 本地 429 行为 | **不触发熔断、不 fallback**,响应体标识 `local_concurrency_limit`,与上游 429 严格区分 |
| 认证位置 | axum 中间件层(`middleware::from_fn_with_state`),两个边缘路由共用 |
| 认证启用 | 默认关;`auth_keys` 非空即生效。本机免认证模式(现状)完全不变 |
| 请求头 | 同时接受 `Authorization: Bearer sk-xxx` 与 `x-api-key: sk-xxx`(Claude Code 原生) |
| 密钥能力 | 认证 + 名称备注。不做每密钥并发限制、不做按人统计(将来在统计记录里带 key id 即可,本次不做) |
| 密钥存储 | **元数据(id/名称/前缀/创建时间)存 `app_config.json`,本体存 OS keyring**(`auth_key::{id}` 条目,同 `usage_cookie::{provider_id}` 模式)。认证不依赖可选的统计 DB |
| 监听地址 | `Settings.bind_addr`(默认 `127.0.0.1`);可切 `0.0.0.0` |
| 安全联动 | 配置一致性硬校验:非回环监听 ⇔ 必须存在至少一个密钥;双向都拦(`set_settings` / `delete_auth_key`) |
| config 存储形态 | **维持 `app_config.json`**。讨论过迁 SQLite,结论:JSON=意图(人可读、可备份、启动必需)、DB=事实(可丢弃)、keyring=秘密,分工清晰不迁 |

## 3. Data Model

### 3.1 `Provider.max_concurrency`

```rust
/// 该套餐(账号)允许的最大并发请求数。None = 不限制。
/// 同厂商不同套餐等级(lite/pro/max)并发不同,以前端按厂商预填、用户手填为准。
#[serde(default)]
pub max_concurrency: Option<u32>,
```

- 后端语义只有 `None`(不限)/ `Some(n)`;"默认值"是 Provider.vue 新建/编辑表单里按 vendor 预填的输入框初值,不落 catalog(避免厂商级默认与套餐级实际值两处真相)。
- CLI/手改 config 无需迁移:`#[serde(default)]` 老 config 读出 `None`。

### 3.2 `AppConfig.auth_keys`

```rust
#[serde(default)]
pub auth_keys: Vec<AuthKeyMeta>,   // 空 = 免认证

pub struct AuthKeyMeta {
    pub id: String,          // 不透明主键 ak_<hex>
    pub name: String,        // 备注,如 "张三的 Claude Code"
    pub key_prefix: String,  // 列表展示 "sk-ab12…"
    pub created_at: i64,     // unix 秒
}
```

密钥本体格式 `sk-` + 32 位随机 base62,**用 `getrandom` 直接填充 32 字节再编码**(`fastrand` 是 PRNG,不适合生成凭据;`getrandom` 已在依赖树中有 3 份传递副本,直接依赖零新增)。仅在 `create_auth_key` 响应里返回完整值一次(前端弹窗展示 + 复制,此后不可再查)。

### 3.3 `Settings.bind_addr`

```rust
#[serde(default = "default_bind_addr")]
pub bind_addr: String,   // "127.0.0.1"
```

读取时解析为 `IpAddr`,解析失败回退 `127.0.0.1`(同 `normalize_log_level` 的"读时归一"惯例)。合法值限定具体 IP 与 `0.0.0.0`;不支持域名。

## 4. Auth Middleware

`build_router` 两个边缘路由前挂 `middleware::from_fn_with_state(auth_mw, state)`:

1. **空表直通**:`auth_keys` 为空 → 立即放行。本机单用户零配置升级,已配 env(无 key)的 agent 不会突然 401。
2. **取 key**:依次看 `Authorization: Bearer sk-xxx` → `x-api-key: sk-xxx`;都缺失 → 401 `missing key`。
3. **比对**:热路径查内存缓存 `AppState.auth: RwLock<HashMap<String /*full key*/, String /*key id*/>>`(std `sync::RwLock`,非 tokio 版),不碰 keyring。锁纪律:持锁区间内**无 await**、不与其它锁嵌套——一次 `HashMap::get` 为纳秒级,写方(增删密钥)为分钟级频率,此量级下无竞争问题(现有 dispatch 每跳 `config.read().await` 是更热的同构路径)。缓存重建时机:启动加载、create/delete 命令后增量更新;keyring 读失败的单条视为不存在(降级,不阻断)。
4. **401 形状按边缘协议**:Anthropic 边 `{"type":"error","error":{"type":"authentication_error",...}}`;OpenAI 边 `{"error":{"message":...,"type":"invalid_request_error","code":"invalid_api_key"}}`。中间件从路由 path 判边,不读 body。
5. **日志**:认证通过后,现有 `forward request` 日志加 `key=<名称或前缀>` 字段(不泄完整密钥),请求可按成员归属排查。

## 5. Concurrency Limiter (in dispatch, per provider)

**为什么在 dispatch 内**:进路由时不知道会打到哪个套餐(Profile→Model→Provider 是 resolve 的结果),fallback 换套餐要跟着换限额——中间件层做不到。

### 5.1 结构

`AppState.concurrency: ConcurrencyRegistry`(运行时,不持久化),按 `provider_id` 缓存:

```
ProviderLimiter {
    limit: u32,               // 当前配置的 max_concurrency
    gate: Semaphore(5×limit), // 在途上限(运行 + 排队)
    run:  Semaphore(limit),   // 同时运行上限
}
```

双层信号量语义:

- `gate.try_acquire()` 失败(在途已满 5×N)→ **本地 429 立即返回**,不打上游、不熔断、不 fallback;响应带 `Retry-After: 5`(与项目 retry_delay 默认 5s 对齐,防客户端极速盲重试);
- `gate` 拿到后 `run.acquire().await`(可无限等待)→ 排队;
- 两个 guard 都持有到请求对该 provider 的使用结束。

`None`(不限)的 provider 不建 limiter,零开销。

### 5.2 持有时长与 Drop 保证(关键)

- **非流式**:attempt 范围内持有,响应体已缓冲,attempt 结束即释放;
- **流式**:guard **移进响应 body 流**(照搬 `RecordOnce` sink 的既有模式),流真正结束才释放——否则慢 SSE 流会提前放行并发位,限制形同虚设。
- **Drop 保证**:释放语义只能来自"guard 被 body 流对象**拥有**"——用 `acquire_owned()`(`OwnedSemaphorePermit`,`Send + 'static`)把 permit 存进持有 stream 的结构体字段,靠类型系统保证任意退出路径(正常耗尽、上游中途 `Err`、客户端断开、panic unwinding)drop 即释放;**禁止**在迭代末尾显式调用释放(早退路径会漏)。客户端断开 → body 流 drop → guard 释放,无泄漏。

### 5.3 fallback / 重试交互

- 每一跳 acquire 对应 provider 的 limiter;上一跳 guard 在进入下一跳前释放(单请求同一时刻只占一个 provider 的并发位)。
- **acquire 位置**:`attempt_with_retry` **外层一次**,循环内的多次原地重试传递并复用同一 guard,不逐 attempt 重新 acquire(否则一次重试会双占并发位;且复用前上一 attempt 的上游响应/流必须已 drop)。
- 本地 429(`ProxyError::LocalConcurrencyLimit`)只发生在 acquire 前,**从不**进入熔断/fallback 路径——一个成员排队超限把整个套餐模型熔断,会殃及所有人。

### 5.4 配置变更

acquire 时发现缓存 `limit` ≠ 当前配置 → 用新限额重建该 provider 的 limiter(替换 map 里的条目;旧信号量随 guard drop 自然排空,瞬时轻微超限可接受)。

## 6. Bind Address & Safety Interlock

- `serve` / `serve_once` / `polling_loop` 绑定 `(settings.bind_addr, port)`;修改监听地址走现有 `restart_server` 路径。绑定失败沿用既有 `bind_error` 机制上报。
- **Windows 防火墙**:首次绑 `0.0.0.0` 会触发系统弹窗;若用户拒绝,监听不报错但入站被静默拦截(`bind_error` 覆盖不到)。设置页切远程模式时提示"需在 Windows 防火墙放行",不做自动改写防火墙规则。
- 环境片段(`get_env_snippet`):`bind_addr` 非回环时,`localhost` 换成本机局域网 IP(零依赖取法:UDP `connect` 一个外部地址如 `8.8.8.8:80` 后读 `local_addr`,不发实际包;**失败回退**:保留 `localhost` 并提示手动替换为本机 IP,防纯内网/断网环境),并提示远程访问需带密钥(`ANTHROPIC_AUTH_TOKEN` 示例)。

**配置一致性硬校验**(存 config 前拦,不做运行时兜底——运行中的服务不因删密钥中断,但持久配置不会出现"远程+无认证"):

1. `set_settings`:目标 `bind_addr` 非回环 且 `auth_keys` 为空 → 报错,要求先创建密钥;
2. `delete_auth_key`:删的是最后一个密钥 且 `bind_addr` 非回环 → 报错,要求先改回本机监听;
3. 设置页 UI:选中非回环地址时,密钥区高亮"远程模式需认证";**空密钥表时把"开启远程访问"做成组合流**——先弹"创建首个访问密钥"窗,创建成功后自动保存监听地址,不让后端拦截错误直接砸给用户(后端校验仍是最终防线)。

## 7. Management Commands & Frontend

### 7.1 Tauri 命令(变更后重取,遵循既有惯例)

| 命令 | 作用 |
|---|---|
| `list_auth_keys` | → `Vec<AuthKeyMeta>`(不含本体) |
| `create_auth_key(name)` | 生成 → keyring + config + 缓存 → **返回完整密钥一次** |
| `delete_auth_key(id)` | keyring + config + 缓存三处删(带 §6 联动校验) |

并发字段不新增命令:`Provider.max_concurrency` 随现有 `upsert_provider` 结构体透传(前端表单多带一个字段)。

### 7.2 前端

- **Provider.vue**:套餐表单加"最大并发"数字输入(空=不限);占位文案按厂商给建议值("智谱/火山 coding 套餐 Lite/Pro/Max 并发不同,请按套餐等级填写")。`types.ts` 同步 `max_concurrency`。
- **Settings.vue** 新增"远程访问"区块:监听地址选择(本机 `127.0.0.1` / 所有网卡 `0.0.0.0`)、密钥列表(名称/前缀/创建时间/删除)、创建按钮(弹窗输入名称 → 展示一次完整密钥 + 复制)。
- 双主题系统(`tokens.css` + `theme.ts`)同步改。

## 8. Error Handling & Edges

| 场景 | 行为 |
|---|---|
| 流式中途客户端断开 | body 流 drop → guard 释放,无泄漏 |
| fallback 跨 provider | 每跳只占当前 provider 的并发位,进下一跳前释放上一跳 |
| `max_concurrency` 改小 | 重建 limiter;旧信号量排空,瞬时超限可接受 |
| 认证缓存与 keyring 短暂不一致 | 缓存只随管理命令增删;keyring 单条读失败 → 该密钥视为不存在,401,不阻断其它 |
| keyring 无后端(Linux) | 复用既有 `secret_store_fallback` 明文授权流程,无新增平台差异 |
| 手改 config 的非法 `bind_addr` | 读时回退 `127.0.0.1` |
| 统计 DB 不可用 | 与认证无关(认证不依赖 DB),代理照跑 |

## 9. Testing

沿用 co-located `#[cfg(test)]` + wiremock + FakeClock:

- **并发(非流式)**:上游挂起响应,并发打 N+1 → 断言上游在途 ≤ N;第 5N+1 个收到本地 429(响应体含 `local_concurrency_limit`、不熔断);放行后排队者完成。
- **并发(流式)**:流未结束时上游在途数不降(guard 在流结束才释放);流结束后并发位恢复。
- **认证**:空表直通;有表时 Bearer/x-api-key 双头通过;错/缺 key 401 且按边缘协议形状;删除密钥后立即 401。
- **联动**:非回环 + 空表 → `set_settings` 报错;删最后一个密钥 + 非回环 → 报错。
- **config 兼容**:老 JSON 反序列化 → `auth_keys: []` / `max_concurrency: None` / `bind_addr: "127.0.0.1"`;round-trip(types.rs 惯例)。
- **keyring 失败降级**:坏 store 下创建报错不崩;认证缓存跳过读不到的条目。

## 10. Out of Scope

- 每密钥并发限制、按成员用量统计(DB 记录加 key id 的扩展留给将来)
- HTTPS/TLS(远程部署建议套反代或隧道,认证只防直连暴露)
- 域名监听、IPv6 之外的地址形式
- app_config.json 迁 SQLite(已讨论并否决,见 §2)
