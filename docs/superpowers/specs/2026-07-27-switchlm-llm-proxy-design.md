# SwitchLM 设计文档

- **日期**:2026-07-27
- **技术栈**:Tauri v2 + Vue 3(前端)+ Rust(后端 / 代理核心)
- **状态**:已通过 brainstorming,待实现计划

## 1. 目标与范围

SwitchLM 是一个常驻系统托盘的国内大模型代理工具,让编码智能体(Claude Code、Cursor、Cline 等)通过本地代理动态切换模型,在智谱、火山两个套餐间调度,并在资源耗尽时自动 fallback。

### 核心功能

1. 启动本地代理服务,支持动态切换编码智能体使用的模型。
2. 动态切换模型,不打断进行中的请求。
3. Fallback:模型返回资源耗尽(429)时,切换到另一个模型继续处理。
4. 查询模型使用量(厂商官方额度)。
5. 退出到系统托盘;托盘可切换模型、列出使用量。

### 目标 Provider(套餐)

- **智谱**(Zhipu):`https://open.bigmodel.cn/api/paas/v4`,OpenAI 兼容。
- **火山**(Volcengine 方舟):`https://ark.cn-beijing.volces.com/api/v3`,OpenAI 兼容。

两家数据面 API 均完全兼容 OpenAI 协议;多数国内厂商后端同时支持 Anthropic 协议。

## 2. 架构总览

### 进程模型

单个 Tauri 应用进程。Rust 后端在应用启动时拉起一个 `axum` HTTP server,作为 tokio 后台任务常驻监听本地端口(默认 **6950**)。

### 两条互不相关的通道

| 通道 | 用途 | 形态 | 使用者 |
|---|---|---|---|
| **代理数据通道** | 真实 LLM 流量 | 本地 HTTP `localhost:6950` | 编码智能体 |
| **管理通道** | 配置 / 切换 / 用量 / 状态 | Tauri command(IPC) | Vue 前端 ↔ Rust |

代理数据通道暴露两条路由:

- `POST /v1/messages` — Anthropic Messages 协议(Claude Code,设 `ANTHROPIC_BASE_URL=http://localhost:6950`)。
- `POST /v1/chat/completions` — OpenAI 协议(Cursor / Cline 等,设 `OPENAI_BASE_URL`)。

### 内部规范格式 = OpenAI Chat Completions

因两家 provider 数据面本就 OpenAI 兼容,选其作为内部表示,出向翻译近乎零成本;翻译复杂度集中在 Anthropic 边缘。

### 生命周期

- 应用启动 → axum server 就绪 → 前端就绪。
- 关闭窗口 → **不退出**,最小化到托盘,服务继续运行(不中断在途请求)。
- 托盘"退出" → 优雅停机(等待在途请求结束或超时)→ 进程退出。

### 端口策略

默认 `6950`(可配)。启动时若被占用 → 自动递增尝试若干端口,并把**实际端口**回显给前端。

> ⚠️ **端口变更风险**:Agent 端通常硬编码环境变量(如 `OPENAI_BASE_URL=http://localhost:6950`),若 SwitchLM 因冲突切到 6951,Agent 会连不上。故当**实际端口 ≠ 配置端口**时:
> - Dashboard 顶部与托盘给出**强警告**(如"⚠️ 默认端口 6950 被占用,已临时切至 6951")。
> - 提供**一键复制环境变量配置**按钮(`ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`,带实际端口),便于粘到 Agent 配置。

```
┌─────────────────────────────────────────────────────────────┐
│  Tauri 进程 (Rust + WebView)                                 │
│                                                              │
│  ┌──────────────┐    Tauri command (IPC)    ┌────────────┐  │
│  │  Vue 前端     │◄─────────────────────────►│  管理层     │  │
│  │ (配置/托盘UI) │   切换/用量/状态           │ (config/   │  │
│  └──────────────┘                            │  usage/tray)│  │
│                                               └─────┬──────┘  │
│   编码智能体 ──HTTP──► ┌──────────────────────────┐  │        │
│   (Claude Code/      │  axum 代理服务            │  │        │
│    Cursor/Cline)     │  /v1/messages (Anthropic) │◄─┘        │
│                       │  /v1/chat/completions    │           │
│                       │   ↓ 翻译/路由/fallback     │           │
│                       └────────────┬─────────────┘           │
│                                    │ reqwest (SSE 流式)       │
│                         ┌──────────▼──────────┐             │
│                         │ 智谱 / 火山 (OpenAI兼容)│            │
│                         └─────────────────────┘              │
└─────────────────────────────────────────────────────────────┘
```

## 3. 核心代理组件

### 3.1 请求处理管线

```
HTTP 请求 (/v1/messages 或 /v1/chat/completions)
  ① 归一化:翻译为内部 OpenAI ChatCompletionRequest(若来自 Anthropic 边缘)
  ② 解析模型:按入站 `model` 名查 Profile(见 §3.4)→ 取其 backing Model;查熔断状态决定是否旁路
  ③ 协议匹配:优先同协议直通,否则翻译
  ④ 构建:内部 model → provider 上游 model id + 鉴权头
  ⑤ 上游调用:reqwest 流式 SSE
  ⑥ 错误判定:流前 429/资源耗尽 → 走 fallback;流中失败 → 透传;非 429 → 透传
  ⑦ 回译:按客户端协议(Anthropic/OpenAI)输出 SSE
  ⑧ 计量:从响应统计 token / 记录调用
```

### 3.2 协议翻译层(头号技术风险)

- **AnthropicEdge**:Anthropic Messages ↔ OpenAI 双向。
  - 入:`system` 参数、`content` blocks(text / tool_use / tool_result / image)→ OpenAI messages(system role / tool_calls / tool role)。
  - 出:OpenAI delta(`choices[0].delta.content` / `.tool_calls`)→ Anthropic 事件序列(`message_start` → `content_block_start` → `content_block_delta` → `content_block_stop` → `message_delta` → `message_stop`)。
- **Tool Use 流式拼接(关键坑)**:Claude Code / Cline 极度依赖工具调用。OpenAI 把单个 `tool_call` 的 `function.arguments`(JSON 串)拆成**极碎增量**吐出,首块带 `id`/`name`,后续块只有参数片段;Anthropic 则要求先发 `content_block_start`(type=`tool_use`,带 `id`+`name`+空 `input`),再用 `input_json_delta` 增量喂参数片段,最后 `content_block_stop`。
  - **必须用显式状态机**按 `tool_calls[].index` 维护每个工具调用的装配状态,确保先发出合规的 `content_block_start` 再发 delta。
  - **`tool_use_id` 缺失时合成**(上游 OpenAI 偶尔不给 `id`),生成形如 `toolu_<n>` 的标准 id;反向(Anthropic→OpenAI)透传 id。
- **OpenAIEdge**:近透传,只做 model 替换与鉴权注入。
- **反向翻译**:OpenAI 客户端 ↔ Anthropic 后端(协议不匹配时触发)。

### 3.3 路由与协议匹配(优先同协议直通)

每个模型最多配两个上游后端(OpenAI / Anthropic)。给定客户端协议与选中模型的可用后端:

```
① 模型有"与客户端同协议"的后端 → 直通(零翻译)        ← 常见路径
② 否则,模型有另一协议的后端     → 翻译(客户端协议 ↔ 后端协议)
③ 都没有                        → 报错
```

Fallback 切换到新模型时,对新模型**重新走一遍协议匹配**。

### 3.4 Model 解析(Profile 路由)

代理以**自定义模型(Profile)**为路由单位:Agent 请求里的 `model` 名 = Profile 名。

```
Agent 请求 model: "glm-5.2"
  → 按 name / aliases 找 Profile
       命中   → 取其 backing_model_id → Model → 协议匹配(§3.3)→ 路由
       未命中 → 直接报错(400,列出可用 Profile 名,便于排查)
```

- **不再有 `force_model_override` 开关**:永远按名找 Profile,天然支持多模型 Agent(主模型/小模型各映射一个 Profile)。
- **未匹配即报错**:暴露配错(如 Profile 名与 Agent 配置不一致),不静默兜底。
- **出站 `model`**:发给 provider 的永远是 backing Model 的 `upstream_model_id`,Agent 原始名不透传。
- **响应 `model` 字段**:回显 Agent 入站的 `model` 名(保持对 Agent 透明);入站缺 `model` 时回填 backing Model 的 `display_name`。

Profile/Model 结构见 §5.2、§5.3。

### 3.5 Fallback 机制

- **触发判定(`ErrorAdapter`,每 provider 适配)**:国内厂商配额耗尽/限流/欠费/套餐不可用**未必返回标准 429**(智谱/火山的限流码实际都是 429,但火山欠费是 403、套餐/模型不可用是 400/404;SSE 首块也可能藏错误)。判定器须综合 HTTP 状态 + 响应体/SSE 错误事件:
  ```rust
  fn is_rate_limit_error(status, body_or_event) -> bool {
      status == 429                                  // 通用:所有厂商限流码实际都是 429
      || (vendor == "deepseek" && status == 402)     // DeepSeek 余额不足
      || 厂商专属 code:
           智谱  1113/1302/1305/1308..=1321          // 1214 是 400 参数错误,非限流
           火山  starts_with("RateLimitExceeded"|"QuotaExceeded")  // 命名空间码,前缀匹配
                | ServerOverloaded|*RateLimitExceeded|SetLimitExceeded|...   // 429 家族
                | AccountOverdueError|OperationDenied.ServiceOverdue          // 403 欠费
                | InvalidSubscription|ModelNotOpen|UnsupportedModel           // 400/404 套餐/模型不可用
      || 关键词命中(["rate_limit","资源耗尽","quota","throttl","too many requests","欠费","overdue","insufficient balance", ...])
      // 5xx 一律透传(不 fallback、不熔断,见 §8)
  }
  ```
- **时机边界(已细化)**:Fallback 允许的边界是"**尚未向客户端转发任何内容事件**",而非粗略的"流前"。
  - 上游在**首块即返回配额错误**(即使 HTTP 200、错误藏在 SSE 第一个 event)→ 此时还没向客户端转发任何内容 → **仍可 fallback**。
  - 一旦已向客户端**转发过内容**(content event),即"已提交";此后流中错误 → **不重试,透传给客户端**。
  - → 与"流中失败不重试"一致:承诺的是"输出已开始就不打断",而非"上游一开流就不能换"。
- **重试策略**:沿每模型配置的 fallback 目标逐个尝试,首个成功即止;耗尽则返回最后一个错误。
- **透传**:401(鉴权)、400(参数)、500 等非限流错误**原样返回客户端**,不 fallback、不熔断。

### 3.6 热切换(不打断在途请求)

- `AppState { config: Arc<RwLock<AppConfig>> }`(熔断状态 `ModelHealth` 单独存,见 §4)。
- 每个请求在**派发时**读一次快照(解析到的 Profile + backing Model + 当时的熔断状态);在途请求持有自己的快照不受影响。
- 切换 = 写入新的 `AppConfig`(如改某 Profile 的 `backing_model_id`)→ 只有新请求看到新配置。

### 3.7 流式管道

- `reqwest` 流式响应 → 异步 SSE 事件流 → 翻译 → 以 SSE 转发给客户端。
- 非流式请求:缓冲完整响应 → 翻译 → 返回 JSON。

## 4. 模型熔断(避免反复戳 429)

429 的恢复通常需较长时间(直到套餐重置)。若每次新请求都先戳已 429 的模型再触发 fallback,既浪费又持续打限流模型。故引入每模型熔断/冷却。

### 4.1 状态(每模型,内存中)

```rust
ModelHealth {
  model_id,
  cooling_down: bool,               // false = 可用, true = 冷却中(旁路→走 fallback)
  recover_at:   Option<timestamp>,  // 冷却到何时
  tripped_at:   Option<timestamp>,  // 仅展示用
}
```

状态**临时**,只存内存;重启后全部回到 `cooling_down=false`。编辑某模型配置时重置其状态。

### 4.2 熔断触发与 recover_at 计算

模型在流前返回 429/资源耗尽 → 置 `cooling_down=true` → 计算 `recover_at`:

```
若 用量适配器能返回 套餐重置时间 → recover_at = 重置时间   (官方口径,最准)
否则                              → recover_at = now + cooldown_duration
```

### 4.3 路由结合熔断

```
cooling_down == false                → 正常使用
cooling_down == true 且 now <  recover_at  → 跳过,直接走它的 fallback(不发探测请求)
cooling_down == true 且 now >= recover_at  → 自动恢复为 false,重新路由给它
```

### 4.4 恢复策略

**基于时间自动恢复**:到 `recover_at` 即置回 `cooling_down=false` 重新使用。若实际仍未恢复,下一次请求会再次 429 → 重新熔断(代价仅一次探测失败)。无需 half-open 探针。

## 5. 数据模型与配置

### 5.1 Provider

```rust
Provider {
  id:           "zhipu" | "volcengine",        // 或自定义 id
  display_name: "智谱" | "火山",
  base_url:     "https://open.bigmodel.cn/api/paas/v4",  // 预置默认,可改
  api_key:      <secret>,                      // 推理 Key
  usage_creds:  Option<UsageCreds>,            // 官方额度查询凭证,可选(如火山 AK/SK)
}
```

`usage_creds` 单独存在:用量/额度查询可能在管控面,需要不同于推理 Key 的凭证。没有则该 provider 额度不可查。

### 5.2 Model(真实模型 / 引擎)

实际服务的引擎,承载 provider、双后端、fallback、冷却。**不直接暴露给 Agent**——Agent 看到的是 Profile(§5.3),Profile 接入 Model。

```rust
Model {
  id:                        "m_glm46",          // 内部 id
  provider_id:               "zhipu",
  display_name:              "GLM-4.6",
  source:                    "discovered" | "manual",   // /models 拉取 or 手填
  openai:                    Option<BackendConfig>,     // OpenAI 后端
  anthropic:                 Option<BackendConfig>,     // Anthropic 后端
  cooldown_duration:         Option<Duration>,          // 冷却时长(官方查不到重置时间时用)
  fallback_target_model_id:  Option<String>,            // 429 时切到的目标 Model(可空=无 fallback)
}

BackendConfig {
  base_url:          String,
  upstream_model_id: String,                 // 实际发给上游的 model id
  api_key_ref:       Option<KeyRef>,         // 缺省时继承 provider.api_key
}
```

两个后端都可选;国内厂商多支持双协议,故常两者皆配。`upstream_model_id` 仅此处出现,出站请求用它(见 §3.4)。

### 5.3 Profile(自定义模型)与配置

**Profile** 是 Agent 看到并按名请求的入口;每个 Profile 接入一个真实 Model 作 backing,可热切换。

```rust
Profile {
  id:               "p_main",
  name:             "glm-5.2",            // 暴露给 Agent 的名字(用户自定义);Agent 请求时填它
  aliases:          Vec<String>,          // 可选:额外匹配名,如 ["claude-sonnet-4"]
  backing_model_id: "m_glm46",            // 当前接入的真实 Model(切换 = 改这里)
}
```

- 多个 Profile 共存 → 多模型 Agent 各角色分别映射,代理同时提供多种可服务模型。
- 多个 Profile 可接入同一个 Model(共享其 fallback/熔断状态)。
- Fallback 链由各 Model 的 `fallback_target_model_id` 串联(Profile 不参与),需**环检测 + 最大跳数上限**。

**AppConfig(热切换目标)**:整个配置存于 `Arc<RwLock<AppConfig>>`,每个请求派发时取一次快照,在途请求不受影响(满足"切换不打断")。

```rust
AppConfig {
  providers: Vec<Provider>,
  models:    Vec<Model>,      // 真实模型池
  profiles:  Vec<Profile>,    // 自定义模型入口
  settings:  { port, autostart, ... },
}
// 熔断状态(ModelHealth)是运行时态,单独存,不入 AppConfig
```

### 5.4 模型发现流程

```
添加 provider(选预置智谱/火山 或 自填 base_url + Key)
   │
   ├─ 调 provider 的 GET /models ──► 拉到上游模型 id 列表
   │      └─ 用户勾选要启用的 ──► 生成 source="discovered" 的 Model
   │
   └─ /models 不可用 或 列表里没有想要的?
          └─ 用户手填 model id ──► 生成 source="manual" 的 Model
```

### 5.5 持久化与密钥存储

- **非敏感配置** → 明文 `app_config.json`,放 Tauri app data 目录(Windows:`%APPDATA%\SwitchLM\`)。含:provider(base_url,不含 key)、models、active_config、端口、UI 偏好。
- **敏感凭证**(`api_key`、`usage_creds`)→ **OS 原生密钥库**,用 `keyring` crate(Windows = Credential Manager),按 provider 命名条目,绝不落明文。
- 启动:读配置文件 + 从 keyring 取密钥 → 组装内存 `AppState`。

## 6. 用量查询子系统

### 6.1 适配器接口(统一、best-effort)

各家厂商返回结构差异大,快照字段全可选,能拿到什么就展示什么:

```rust
trait UsageProvider {
  async fn query(&self, creds: &UsageCreds) -> Result<UsageSnapshot, UsageError>;
}

UsageSnapshot {
  used:        Option<f64>,    // 已用
  total:       Option<f64>,    // 总额度
  remaining:   Option<f64>,    // 剩余
  reset_at:    Option<timestamp>,  // ← 熔断器消费(套餐重置时间)
  unit:        String,         // "积分" / "tokens" / "CNY" 等展示用
  raw_summary: Option<String>, // 无法结构化时的原始文本兜底
}

enum UsageError {
  NotConfigured,         // 没配 usage_creds / key
  UnsupportedByProvider, // 该厂商无此接口
  NetworkError,
  AuthFailed,
}
```

### 6.2 两个实现

- **`ZhipuUsageProvider`**:查智谱计费/用量接口。
- **`VolcengineUsageProvider`**:查方舟用量/额度。⚠️ 方舟管控面计费可能需要 AK/SK → 配了 `usage_creds` 才能查,否则降级。

### 6.3 reset_at 喂给熔断器

模型流前 429 → 熔断时调该 provider 的 `usage_provider.query()`:
- 返回了 `reset_at` → `recover_at = reset_at`。
- 没返回 / 查询失败 → `recover_at = now + cooldown_duration`。

### 6.4 调用时机 + 缓存

- **按需**:打开托盘 / 用量页 → 刷新(带缓存,TTL ~60s)。
- **熔断时**:触发一次该 provider 查询(走缓存)。
- 不做后台轮询(MVP)。

### 6.5 显示粒度

官方额度通常是**套餐/账号级**(一个 provider 的额度被名下所有模型共享)。渲染时**按 provider 取快照、按模型内联显示**——同一套餐下的不同模型显示相同的额度值。不做逐模型自统计。

### 6.6 降级

适配器返回 `UsageError` → UI 显示"额度查询不可用(原因)";熔断器退回 `cooldown_duration`。

## 7. 托盘与前端

### 7.1 托盘菜单(按 Profile 切换接入)

```
SwitchLM
├─ 自定义模型 ▶
│   ├─ glm-5.2 ▶            接入: GLM-4.6  智谱 80%
│   │   ├─ ✓ GLM-4.6
│   │   ├─ ○ GLM-4.5
│   │   └─ ○ Doubao-seed
│   └─ fast ▶               接入: Doubao  火山 45%
│       ├─ ○ GLM-4.6
│       └─ ✓ Doubao-seed
├─ ─────────────
├─ 打开主窗口
├─ 开机自启
└─ 退出
```

- 每个 Profile 下可切换它接入的真实 Model(改 `backing_model_id`,热切换),在途请求不受影响。
- 模型后内联其 provider 额度;接入模型冷却中时标注。
- 托盘菜单**打开时**刷新用量(60s 缓存)。
- **悬浮 tooltip**：只显示"最近一次请求实际服务的模型"所属套餐的**剩余**余量（单行，如 `智谱 剩 20%`；回落时显示回落方套餐）。规避 Windows 托盘 tooltip 64 字符上限——列全部套餐(85 字符)会被 OS 从中间截断。`last_served_provider` 由 `dispatch` 在每次有模型产出响应时记录；右键菜单仍列全部套餐明细。

### 7.2 主窗口页面(结构 + 字段;视觉/wireframe 推迟到前端实现)

| 页面 | 结构与字段 |
|---|---|
| **Dashboard 概览** | 当前模型、端口、服务状态、套餐额度速览、快速切换入口;**端口变更强警告 + 一键复制环境变量配置** |
| **Provider 管理** | 增删改 provider:base_url、推理 api_key、`usage_creds`(AK/SK 可选)、连接测试 |
| **真实模型管理** | 增删改 Model:发现(`/models` 拉取勾选)+ 手填;每模型配 OpenAI/Anthropic 双后端 + `cooldown_duration` |
| **自定义模型 (Profile)** | 增删改 Profile:命名(+ aliases)、选接入的 backing Model——即所见即所得的多模型入口 |
| **Fallback 配置** | 每真实 Model 下拉选 `fallback_target_model_id`(可留空 = 无) |
| **用量查看** | 套餐 + 每模型额度明细(used / total / 重置) |
| **设置** | 端口(默认 6950)、开机自启、通用偏好 |

### 7.3 Vue ↔ Rust 接口面(Tauri command)

```
// 配置
get_providers / upsert_provider / delete_provider / test_provider_connection
discover_models(provider_id)            // 调 GET /models
get_models / upsert_model / delete_model          // 真实模型(Model)
get_profiles / upsert_profile / delete_profile    // 自定义模型(Profile)
set_profile_backing(profile_id, model_id)         // ← 热切换:改 Profile 接入的真实模型
// fallback 关系存于每 Model.fallback_target_model_id;以下为聚合视图
get_fallback_map                         // → { model_id → fallback_target_model_id }
set_model_fallback(model_id, target)

// 用量
get_usage(provider_id) → UsageSnapshot
get_all_usage()                          // 托盘/用量页一次性渲染

// 服务/应用
get_server_status / get_port / restart_server
get_env_snippet()                       // → { ANTHROPIC_BASE_URL, OPENAI_BASE_URL }(带实际端口,供复制)
quit_app / toggle_autostart
```

### 7.4 状态一致性

真正的状态都在 Rust 的 `AppState`(config + 各模型 health + usage 缓存)。**托盘菜单**由 Rust 直接读写 `AppState`;**主窗口**通过 command 读写同一份 `AppState`。任一处切换 → `AppState` 变 → 另一处下次读取即同步(可加 event 广播让窗口实时刷新,非必须)。

## 8. 错误处理

| 场景 | 处理 |
|---|---|
| 配额/限流错误(429 或厂商特有码,见 §3.5 `ErrorAdapter`)、且尚未转发内容 | 熔断(置 `cooling_down`)+ 走 fallback |
| 已转发内容后的流中错误 | 不重试,把错误返回客户端(已提交) |
| 非 429 上游错误(401/400/500) | 原样透传,不 fallback、不熔断 |
| 网络错误/连接失败(流前) | 透传(按"仅 429"口径,不熔断) |
| Fallback 耗尽 | 返回最后一个错误给客户端(带尝试过的模型列表) |
| 用量查询失败 | UI 显示"不可用(原因)";熔断退回 `cooldown_duration` |
| 启动端口被占 | 自动递增尝试 → 回显实际端口到 UI |
| 无任何 Profile/Model 配置 | 返回 503 + 提示"请先在 SwitchLM 配置自定义模型" |
| 入站 `model` 匹配不到 Profile | 返回 400 + 列出可用 Profile 名(便于排查) |
| 翻译失败(畸形请求) | 返回 400 + 诊断信息 |
| 配置文件缺失/损坏 | 以空配置启动,引导用户初始化 |
| 关闭/退出 | 优雅停机:等待在途请求(带超时)后退出 |

## 9. 测试策略

### 单元测试(Rust `#[test]`)

- **协议翻译**:已知 Anthropic 请求 → 预期 OpenAI;已知 OpenAI SSE → 预期 Anthropic 事件序列。覆盖纯文本、`tool_use`/`tool_result`、多轮、`system`、图像。
- **Tool Use 流式拼接**:多块 `arguments` 增量 → 正确的 `content_block_start`+`input_json_delta`+`content_block_stop` 序列;`id` 缺失时合成;并发多个 tool_call(按 `index` 区分)。
- **`ErrorAdapter`**:按厂商分类限流/配额/欠费/套餐错误码(智谱 1113/1302/1305/1308..=1321;火山 `RateLimitExceeded*`/`QuotaExceeded*` 前缀 + 429 家族 + 403 `AccountOverdueError`/`ServiceOverdue` + `InvalidSubscription`/`ModelNotOpen`/`UnsupportedModel`;智谱 1214 是参数错误,不算限流)→ 正确判为限流并触发 fallback;非限流码不误触发;5xx 一律透传;首块错误(未转发内容)可 fallback。
- **Model 解析(Profile 路由)**:入站 `model` 名 → 命中 Profile 路由其 backing;未命中/缺失 → 报错并列出可用名;响应 `model` 回显入站名。
- **反向翻译**(OpenAI 客户端 ↔ Anthropic 后端)。
- **熔断**:429 触发 → 冷却期跳过 → 到 `recover_at` 自动恢复;`reset_at`(官方)vs `cooldown_duration`(兜底);环检测/最大跳数。
- **Fallback 链**:A→B→C、耗尽返回最后错误、非 429 透传。
- **热切换**:在途请求持有旧快照不受影响。
- **Model 覆盖**:入站 `model` 字段被忽略。

### 契约/集成测试

- `wiremock` 模拟上游返回 429 → 断言 fallback 触发 + 熔断状态。
- 回放**真实 Claude Code 抓包**的请求/响应流,逐字节断言 SSE 正确性(Anthropic 边缘硬指标)。

### E2E(终极验证)

- 真实 Claude Code 指向代理打真实智谱/火山,跑编码任务冒烟测。
- Cursor/Cline(OpenAI 客户端)同样冒烟。

### 用量适配器

mock 厂商用量接口 → 断言快照解析 + `reset_at` 提取 + 降级路径。

## 10. 关键技术风险

1. **Anthropic 流式翻译正确性(最高)**:Claude Code 对 SSE 事件类型、`tool_use` 块很挑剔,尤其 tool_call 增量拼接与 `tool_use_id`。缓解:双协议后端让常见路径直通免翻译;Tool Use 用显式状态机装配(§3.2);用真实 Claude Code 抓包做逐字节契约测试。
2. **厂商官方用量接口差异/可达性**:方舟计费可能需 AK/SK。缓解:适配器 best-effort + 降级,熔断退回 `cooldown_duration`。
3. **熔断环/无限 fallback**:缓解:`fallback_target_model_id` 串联需环检测 + 最大跳数上限。
4. **厂商限流语义非标准**:智谱/火山用特有错误码表达限流/欠费/套餐不可用(火山欠费是 403、套餐过期/模型未开通是 400/404,非 429),且码可能带命名空间(`RateLimitExceeded.EndpointRPMExceeded`)。缓解:`ErrorAdapter` 按厂商分类 code(前缀匹配 + 显式列表)+ 状态 + 关键词综合判定(§3.5);边界细化为"未转发内容即可 fallback",首块错误也能救。

## 11. 范围外 / 推迟

- **UI 视觉设计 / wireframe**:推迟到前端实现阶段,用 `frontend-design` skill。
- **后台用量轮询**:MVP 不做,按需刷新。
- **主动配额预警(剩余<阈值提前熔断)**:MVP 不做,仅被动(实际 429 才熔断)。
- **多 Key 轮询 / 负载均衡**:不在本期。
- **网络错误也触发 fallback**:按"仅 429"口径不做。
