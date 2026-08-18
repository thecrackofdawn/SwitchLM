# 请求记录(Request Recording)设计文档

> 本文档是 SwitchLM「给后端添模型请求 cache 能力」的第一步:**记录所有请求的完整
> 入站请求体 + 预计算哈希 + 调度元数据**,落盘成可离线分析的 JSONL。目标是既能量化
> 「相同请求占比」(全响应缓存的 go/no-go 判据),**也能为后续缓存优化方向提供素材**
> (前缀重叠 / 字段波动性 / 重试识别 / 任意口径的 retroactive 重新分桶)。缓存本身
> (存储 / 命中查找 / 失效)是后续步骤,不在本文档范围。

## 1. 目标与范围

### 目标
对代理转发的**每一个请求**,落盘一条结构化记录,使其足以回答(按"分析力"递增):

1. **精确重复率**——完整请求体与此前某次请求逻辑等价的占比。(全响应缓存命中率上界)
2. **内容重复率**——忽略可变参数(temperature / max_tokens / stream …)后,对话内容重复占比。
3. **前缀重叠(prompt-cache 价值)**——相邻同 Profile 请求之间共享多长的消息前缀?
   (编码 agent 真正的大头:每轮把上一轮回复 + 工具结果追加进 `messages`,相邻请求
   几乎不整请求相同,但前缀高度重叠。)
4. **字段波动性**——system / tools / 哪些消息位是稳定的、哪些在变?(用于设计更聪明的缓存 key)
5. **重试 vs 并发重复**——重复是几秒内的重试,还是并发相同 prompt?

只有**记录完整请求体**才能支撑 3–5;仅存哈希会把后续优化方向挡在门外(详见 §11)。

### 为什么必须先量化
编码 agent 每轮追加 `messages`,相邻请求几乎不可能逐字节相同。唯一"整请求相同"的
现实来源是**重试**与**并发相同 prompt**。先实测拿到精确重复率,决定全响应缓存是否值得;
再分析前缀重叠与字段波动性,决定缓存该按什么 key、prompt-cache 价值多大。两件事都靠
这同一份记录。

### 范围内
- 在 `dispatch()` 末端记录每个转发请求(含两条协议边)的**完整入站请求体** + 两个预计算
  哈希 + 调度元数据,以 JSONL 滚动落盘。
- 一个开关(Settings 字段 + Settings 页面开关,**带隐私警示文案**)。
- 一键清空记录目录的 Tauri command。
- 一个分析脚本,从 JSONL 算出上述 1–5。

### 范围外(推迟)
- 缓存本身(存什么、命中后如何回放流式响应、TTL/失效、与 fallback 的交互)。
- 重复率/前缀重叠的前端可视化。
- 记录的远程上报(永远不做——本地测量用途,且含完整 prompt,敏感)。

## 2. 关键设计决策

| 决策 | 选择 | 理由 |
|---|---|---|
| 记录什么 | **完整入站请求体 + 两个预计算哈希 + 元数据** | 完整请求体支撑前缀重叠 / 字段波动性 / retroactive 重新分桶等所有优化分析;两个哈希是预计算便利,让常用分组分析 O(1) 而不必对每条重算。 |
| 跨协议归一化 | 复用现有 `translate::request::anthropic_to_openai` | 两协议结构差异显著(见下),自造提取器需维护两份;翻译函数已是纯函数 + 经过测试,把 Anthropic 请求规范成 OpenAI messages 形态后,与 OpenAI 请求共用同一套 messages 哈希逻辑。 |
| 何时记录 | **dispatch 末端**(结果已知后) | 是否值得缓存取决于原请求**是否产出了可缓存响应**;只有 `outcome=="ok"`(2xx 已响应)的重复才有缓存价值。开头记录拿不到这个判据。 |
| 开关默认 | **关**,零开销 | 记录型功能且会落盘完整 prompt,不该默认开启;关时 `AppState.recorder == None`,dispatch 只做一次 `Option` 判断即返回。 |
| 写盘通道 | **有界 mpsc + `try_send`,溢出即丢弃并计数** | 完整请求体记录较大;有界 + drop-on-overflow 既不阻塞请求热路径,也不会因写盘卡顿而 OOM;丢弃会被计数+告警,分析时可知晓数据缺口。 |

### 两协议结构差异(归一化不能一把梭)

| 维度 | OpenAI `/v1/chat/completions` | Anthropic `/v1/messages` |
|---|---|---|
| system prompt | `messages` 数组里的一条 `{"role":"system"}` | 顶层 `system` 字段(字符串**或** block 数组),**不在** `messages` 内 |
| `content` | 通常是纯字符串 | 类型化 block 数组:`text`/`image`/`tool_use`/`tool_result` |
| 工具调用 | assistant 消息上的 `tool_calls` + 后续 `{"role":"tool"}` | `content` 内联的 `tool_use`/`tool_result` block |
| `max_tokens` | 可选 | **必填** |

因此"messages-only 哈希"必须先把两种形态都规范到同一形状——交给 `anthropic_to_openai`
处理 Anthropic 边,OpenAI 边直接取 `req["messages"]`。**注意:完整请求体本身存原文
(入站原始 JSON),不做归一化**——它是分析的事实来源;归一化只用于派生的
`hash_messages`。

> **归一化的有损性(已知权衡)**:`anthropic_to_openai` 为转发设计,会丢弃 `image`
> block、用 `"\n"` 拼接 text、把 tool input 序列化成字符串。对**去重哈希**没问题(纯函数
> ⇒ 确定性 ⇒ 相同输入永远相同哈希);唯一失效是**假重复**(仅在 image 等被丢字段上不同的
> 请求撞哈希),对编码 agent 流量可忽略。`hash_full` 与完整请求体始终无损。

## 3. 架构:独立的 `recording` 模块

新增 `src-tauri/src/recording/mod.rs`,职责单一、可在 dispatch 之外独立测试:

- **哈希函数**(纯函数):
  - `hash_full(req: &Value) -> String` —— 整请求体的规范哈希(见 §6)。
  - `hash_messages(req: &Value, protocol: ClientProtocol) -> String` —— Anthropic 先过
    `anthropic_to_openai` 再取 `["messages"]`;OpenAI 直接取 `req["messages"]`。
- **`RequestRecord`**:§5 schema 的小结构体(含完整 `body: Value` + 两哈希 + 元数据)。
- **`RequestRecorder`**:拥有一个 `mpsc::Sender<RequestRecord>`(有界,容量 ~256)+ 后台
  写盘任务句柄 + 一个 drop 计数器;
  - `record(&self, rec)` —— `try_send(rec)`,溢出则 `fetch_add(1)` 计数;
  - 后台 task 从 `rx` 取出 → 序列化紧凑 JSONL → 写入滚动文件(§7),并周期性把累积的
    drop 计数以一条 `tracing::warn!` 落 `switchlm.log`("dropped N request records")。

`AppState` 新增字段:`pub recorder: Option<Arc<RequestRecorder>>`。
- 关闭时为 `None`,dispatch 中 `if let Some(r) = &state.recorder { … }` 直接跳过,零开销。
- 在 `lib.rs` setup 里根据 `cfg.settings.request_recording` 与 `app_data_dir` 构造
  `Some(RequestRecorder::new(dir))` 或 `None`。

## 4. 记录管线:在 dispatch 末端落点

`proxy/dispatch.rs::dispatch()` 是两条协议边共用的唯一收口(edge handler 只是薄壳)。
在 `dispatch` **末端**、返回 `result` 之前插入记录(此时 `result`、`outcome`、`ms`、
`served` 全部已知;`req`、`body`、`protocol`、`req_id` 都在作用域内):

```text
… compute result / outcome / served / ms (现有逻辑, 见 dispatch.rs L110–L133) …

if let Some(recorder) = &state.recorder {
    let rec = RequestRecord {
        ts: now_local_rfc3339(),
        req_id, protocol, requested, served_vendor, served_model, stream,
        bytes: body.len(),
        body: req.clone(),                 // 入站原始请求体(已解析的 Value),无损
        hash_full: hash_full(&req),
        hash_messages: hash_messages(&req, protocol),
        outcome, status, ms, hops: outcome.hops,
    };
    recorder.record(rec);                  // 同步算哈希(快) + try_send;写盘在后台
}
result
```

- **哈希在 dispatch 同步算**(SHA-256 对 100KB 体量亚毫秒级);`body` 走 `req.clone()`
  (一次浅拷贝,Value 内部 Arc 共享字符串,开销可接受)。
- **写盘是慢操作,放后台 task**,不进请求关键路径。
- **`try_send` 溢出丢弃**:仅当写盘 task 严重滞后(本地磁盘几乎不会)才发生;丢弃被计数,
  分析脚本可从 `switchlm.log` 的 "dropped N" 行知晓数据缺口。
- **不记录的请求**:`body` 解析 JSON 失败(L58 提前返回)、`NotConfigured`(L68 提前返回)
  都在记录点之前,天然不落盘——它们不是真正的 LLM 请求。

> `outcome` 取值约定:仅当 `result` 为 `Ok` 且透传状态 `is_success()`(2xx)时记 `"ok"`;
> 否则记 `"error"`,`status` 在"无响应"(调度失败)时为 `null`。

## 5. 记录 Schema(JSONL,每请求一行)

```json
{"ts":"2026-08-11T14:32:01.123+08:00","req_id":"a3f2","protocol":"anthropic",
 "requested":"claude-sonnet-4","vendor":"zhipu","model":"glm-4.6","stream":true,
 "bytes":45231,
 "body":{"model":"claude-sonnet-4","system":"…","messages":[…],"tools":[…],"max_tokens":4096,"stream":true},
 "hash_full":"sha256:9f2c…","hash_messages":"sha256:b71a…",
 "outcome":"ok","status":200,"ms":1234,"hops":2}
```

| 字段 | 说明 |
|---|---|
| `ts` | 本地时区 RFC3339,与 `switchlm.log` 一致(`chrono::Local`)。 |
| `req_id` | 与 `switchlm.log` 的 `req=<id>` 关联,可在并发下重建单请求链路。 |
| `protocol` | `"anthropic"` / `"openai"`——哪条边进来的。 |
| `requested` | 客户端发送的 `model`(Profile 名/别名)。 |
| `vendor` / `model` | **实际服务**该请求的模型所属厂商 + 上游真实模型名(可能因 fallback 与 requested 不同),与现有 `served` 日志语义一致。 |
| `stream` | 是否流式。 |
| `bytes` | 原始入站请求体字节数(`body.len()`)。 |
| `body` | **完整入站请求体**(已解析的 JSON 对象,紧凑序列化,无损、未归一化)。是分析的事实来源;**含用户代码与可能的密钥,仅本地存储**(见 §8/§11)。 |
| `hash_full` | 整请求体规范哈希(§6)——未来全响应缓存的 key,预计算以便 O(1) 分组。 |
| `hash_messages` | 仅对话内容哈希(归一化后)——预计算以便 O(1) 分组。 |
| `outcome` | `"ok"`=已透传 2xx 响应(**可缓存**);`"error"`=非 2xx 透传或调度失败。 |
| `status` | 透传的 HTTP 状态;调度失败(`Err`)时为 `null`。 |
| `ms` / `hops` | dispatch 总耗时 / fallback 跳数。 |

> **为什么 `outcome` 是关键字段**:重复只有在原请求产出了**可缓存响应**时才有价值。
> 复制一次 rate-limit 的重复对缓存毫无意义。因此重复率分析只在 `outcome=="ok"` 的行上做。

## 6. 哈希与归一化

**规范 JSON(canonical JSON)**:对 `Value` 做**确定性序列化**——对象键按字典序递归排序、
无多余空白——再取 SHA-256,前缀 `sha256:`。
- 确定性保证:**无论 `serde_json` 是否启用 `preserve_order` feature**,相同逻辑请求永远
  得到相同哈希;键序差异不会制造假阴性。
- 实现是一段递归小函数(约 20 行),不引入新依赖。

**`hash_full(req)`** = 规范序列化整个 `req` → SHA-256。包含 `stream` 与全部参数,是"精确
逻辑请求"的标识(一个请求以 stream / non-stream 各发一次,算两个不同请求)。

**`hash_messages(req, protocol)`**:
- OpenAI:规范序列化 `req["messages"]` → SHA-256。
- Anthropic:`anthropic_to_openai(req)["messages"]` → 规范序列化 → SHA-256。
- 两者共用同一"内容"定义,故跨协议可比较;剔除参数/`stream`,回答"内容相同、参数不同"。

两条哈希都是纯函数 + 确定性,仅在记录开启时计算。**`body` 字段始终存原文,不参与/不依赖
这套规范哈希**——它是更深分析(前缀重叠等)的原料。

## 7. 存储与轮转

- **路径**:`<app_data>/request_log/requests.jsonl`(与 tracing 的 `logs/` 分开,便于单独
  清空 / 导出)。`app_data_dir` 已在 `lib.rs` setup 取得,直接传给 recorder 构造。
- **格式**:JSONL——一行一记录(`body` 作为嵌套 JSON 对象,紧凑序列化保证单行完整),
  任意 `jq`/python 可直接分析。
- **轮转**:复用 `logging::RollingFileWriter` 的滚动模式,用更大上限
  (50 MiB × 5 文件 = 至多 250 MiB)。每条记录都带 `ts`,故按大小滚动不影响按时间切片。
  > 完整请求体使单条记录可能达数十~上百 KB;250 MiB 上限在重度使用下约覆盖 10 天量级,
  > 足够一个测量窗口。需要更久则先把数据导出再继续。
- **清空**:Tauri command `clear_request_log` 删除整个 `request_log/` 内文件(recorder
  下次写盘自动重建当前文件)。

## 8. 开关、隐私与开销

- 新增 `Settings.request_recording: bool`(默认 `false`),持久化进 `app_config.json`。
- **关闭时**:`AppState.recorder == None`;dispatch 一次 `Option` 判断即返回——不算哈希、
  不 clone body、不落盘,对正常用户零影响。
- **开启时**:dispatch 同步算两个哈希 + clone body,`try_send` 推记录入有界 channel;
  后台 task 排空 → 写盘。写盘卡顿不会反压请求(溢出即丢 + 计数)。
- **前端**:Settings 页面加一个开关,**必须带警示文案**,例如:
  > 「请求记录:记录每个请求的完整内容(含代码与可能的密钥)到本地,用于分析缓存优化。
  > 仅本地存储、不上传,可一键清空。」
  手改 `app_config.json` 不可靠——app 在每个变更命令后会重存配置,手改可能被覆盖或损坏,
  故必须给 UI 入口。
- **隐私姿态**:记录是本地单用户、时间限定的测量工具;`body` 含完整 prompt 故敏感,但
  (a) 默认关、(b) 有轮转封顶、(c) 一键清空、(d) 永不上传。这几条共同把风险压到可接受。

## 9. 分析(回答"相同请求概率有多大" + 优化方向)

随附 `dev-reference/analyze_requests.py`,输入 `requests.jsonl`,输出:

- **精确重复率**:`hash_full` 在 `outcome=="ok"` 行中的重复占比。
- **内容重复率**:`hash_messages` 在 `outcome=="ok"` 行中的重复占比。
- **前缀重叠**:对同一 Profile 按 `ts` 排序的相邻请求,计算 `messages` 的最长公共前缀
  (按消息条数 / 按字符),分布统计——这是 prompt-cache 价值的直接度量。
- **字段波动性**:对同一 Profile 的请求序列,看 `system` / `tools` / 各消息位哪些稳定、
  哪些在变(用于设计更聪明的缓存 key)。
- **重试 vs 并发重复**:重复 key 的相邻 `ts` 间隔分布(<2s 视为重试)。
- **Top-N 最常重复的 key**、按 `bytes` 分桶的重复率差异。

示意(jq,精确重复率):
```bash
jq -r 'select(.outcome=="ok") | .hash_full' requests.jsonl \
  | sort | uniq -c | sort -rn | head
# 唯一 key 数 / 总行数 = 1 − 命中率;命中率即"相同请求概率"
```

预计算的 `hash_full` / `hash_messages` 让 1–2 与 Top-N 是 O(1) 分组;前缀重叠 / 字段波动性
需读 `body`,但有完整原文才做得出来——这正是选"完整请求体"的理由。

## 10. 测试策略

- **哈希确定性(纯单测)**:同一 `req` 多次调用 → 同一哈希;键序打乱 → 同一哈希。
- **跨协议内容一致(纯单测)**:语义等价的 Anthropic 请求与 OpenAI 请求,
  `hash_messages` 相等(验证 `anthropic_to_openai` 归一化生效)。
- **`hash_full` 区分性(纯单测)**:仅 `stream` 或 `temperature` 不同 → `hash_full` 不同、
  `hash_messages` 相同。
- **记录器(集成测,经 `build_router`)**:开启时 dispatch 一次 → `requests.jsonl` 多一行、
  字段齐全(含完整 `body`)、`body` 与入站一致;关闭时 → 文件不增长、且走的是 `None` 分支。
- **drop 计数**:用一个人为慢速的 writer(或小容量 channel + 灌入多条)验证溢出丢弃被计数。
- **轮转**:复用 `RollingFileWriter` 已有测试模式,验证 50 MiB × 5 的滚动与最旧文件丢弃。
- **`outcome` 正确性**:成功(2xx)→ `"ok"`;透传 401 → `"error"` 且 status=401;
  FallbackExhausted → `"error"` 且 status=null。
- **清空命令**:`clear_request_log` 后目录内文件被删、recorder 仍可继续写。

## 11. 风险与权衡

- **完整 prompt 落盘(首要风险)**:`body` 含用户代码与可能的密钥。缓解:默认关、轮转封顶、
  一键清空、永不上传、UI 明示警示。这是用"分析力"换来的有意权衡——仅存哈希会挡掉前缀
  重叠 / 字段波动性 / retroactive 重新分桶等优化分析(见 §1)。
- **磁盘增长**:完整请求体使记录远大于纯哈希;250 MiB 轮转封顶 + 一键清空兜底。测量完成后
  建议关闭开关并清空。
- **drop 丢记录 skew**:有界 channel 溢出会丢记录,理论上可能偏斜测量。本地磁盘几乎不会
  滞后到溢出;且丢弃被计数+告警,分析时可知晓。比"无界 channel 攒大 body 致 OOM"更稳。
- **假重复(归一化有损)**:`hash_messages` 因 `anthropic_to_openai` 丢弃 image 等撞哈希。
  对编码 agent 流量可忽略;`hash_full` 与 `body` 原文无此问题,作基线。
- **key 稳定性**:日后若改规范序列化口径,旧 `hash_*` 与新值不可比。本功能是测量用途,届时
  按"新口径"重采即可;分析脚本需注明口径。`body` 原文不受影响,可随时重算任意口径。
- **记录点遗漏**:记录只在 `dispatch` 末端,记录点之前提前返回的路径(解析失败、
  NotConfigured)不落盘——有意为之(非真实 LLM 请求),代码注释需写清边界。

## 12. 范围外 / 推迟

- 缓存本体:存哪些响应、命中后如何回放(尤其流式)、TTL/失效、与 fallback 的交互。
- 重复率 / 前缀重叠的前端图表/页面。
- 记录的远程上报(永远不做)。
- 自动密钥脱敏(正则识别 prompt 内密钥并打码):可作为后续增强,但可靠检测密钥本身是
  难题,当前以"本地存储 + 一键清空"兜底,不强行脱敏以免破坏分析原料。
