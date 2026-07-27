# 各家厂商错误码与 fallback 判定参考

SwitchLM 把上游错误分成两类:**触发 fallback**(限流/配额/欠费/套餐不可用 → 熔断 + 走下一跳)和**透传**(鉴权/参数/未找到/敏感词/5xx → 原样返回给请求方,不熔断)。本文按厂商梳理各家错误码的分类,供维护 `src-tauri/src/proxy/error_adapter.rs` 时对照。

判定实现在 `is_rate_limit_error(vendor, status, body)`,三层短路:

1. **通用 HTTP 状态**:`429` 一律算限流;`deepseek` 另加 `402`(余额不足)。
2. **厂商专属 code**(处理"非 429 但仍该切走"的信号,以及流式首事件兜底——此时 HTTP 状态未知)。
3. **关键词兜底**:`message` 命中 `rate_limit`/`quota`/`throttl`/`too many requests`/`资源耗尽`/`欠费`/`overdue`/`insufficient balance` 等。

> **5xx 一律透传**(spec §8,不 fallback、不熔断)。**欠费/套餐过期/模型未开通**这类"永久性"错误**走 fallback 切账号**(无自动重置时间 → 由 `compute_recover_at` 落到 Transient 短冷却,过期后探测再 trip)。

## 参考网址(官方错误码表)

| 厂商 | 文档 | 备注 |
|---|---|---|
| 智谱 BigModel | <https://docs.bigmodel.cn/cn/api/api-code> | 外层 HTTP 状态码 + 内层 `error.code` 业务码 |
| 火山 Volcengine Ark | <https://www.volcengine.com/docs/82379/1299023> | 控制台域名(`console.volcengine.com/.../docs/82379/1299023`)**登录态才能访问**;用 `www.volcengine.com/docs/82379/1299023` 公开页抓取。码形如 `Type`/`Code`(命名空间,如 `RateLimitExceeded.EndpointRPMExceeded`) |
| DeepSeek | <https://api-docs.deepseek.com/quick_start/error_codes> | 标准 HTTP 状态码(OpenAI 风格) |

> 火山另有"公共错误码"页(<https://www.volcengine.com/docs/6369/68677>),但推理相关限流已被 429 + 下表覆盖。

---

## 智谱 Zhipu(vendor: `zhipu`)

> **限流/配额/欠费码全部是 HTTP 429**,通用 429 兜底已覆盖;code 枚举(`1113`/`1302`/`1305`/`1308..=1321`)只作流式首事件兜底。

### → fallback(熔断 + 切下一跳)

| code | HTTP | 含义 |
|---|---|---|
| `1113` | 429 | 账户已欠费,请充值后重试 |
| `1302` | 429 | 账户已达速率限制 |
| `1305` | 429 | 该模型当前访问量过大,稍后再试 |
| `1308` | 429 | 已达到使用上限,限额将在重置时间恢复 |
| `1309` | 429 | GLM Coding Plan 套餐已到期 |
| `1310` | 429 | 已达每周/每月使用上限 |
| `1311` | 429 | 当前订阅套餐暂未开放该模型权限 |
| `1313` | 429 | 不符合公平使用策略,请求频率受限 |
| `1314` | 429 | 企业套餐已失效 |
| `1315` | 429 | 该 API Key 仅限企业编程套餐场景(更换 Key/产品类型) |
| `1316`–`1321` | 429 | 5 小时 / 7 天 / 月 各类窗口耗尽(含主账号余额不足、子账号/企业月上限等组合) |

> 代码用数值区间 `1308..=1321` 匹配(文档中 `1312` 缺省,区间内无副作用)。

### → 透传(原样返回,不熔断)

| code | HTTP | 含义 |
|---|---|---|
| `1000`/`1001`/`1003`/`1005` | 401 | 身份验证失败 / 无 Authentication / Token 过期 / 二次认证 |
| `1210`/`1213`/`1214`/`1215` | 400 | 参数有误 / 缺参数 / **参数非法(1214)** / 参数互斥 |
| `1211`/`1212`/`1221`/`1222` | 400 | 模型不存在 / 不支持该调用方式 / API 已下线 / API 不存在 |
| `1261` | 400 | Prompt 超长 |
| `1301` | 400 | 输入/生成内容含敏感信息(区间下界守护:1301 < 1308,不被误判) |
| `1220` | 403 | 无权访问该 API |
| `-`/`1200`/`1230`/`1234` | 500 | 内部错误 / API 调用失败 / 流程出错 / 网络错误 |

> ⚠️ **`1214` 是 `${field} 参数非法`(HTTP 400),不是限流。** 早期实现误把它当限流码,已修正——它必须透传。

---

## 火山 Volcengine Ark(vendor: `volcengine` / `volcengine-agent` / `volcengine-coding`)

> 限流码全部是 HTTP 429;**欠费是 403、套餐/模型不可用是 400/404**——这些"非 429 但仍该切走"的码必须显式列在 code 分支里(否则会漏,被透传)。

### → fallback(熔断 + 切下一跳)

**429 限流/配额家族**(`RateLimitExceeded*`/`QuotaExceeded*` 用**前缀**匹配;其余显式枚举):

| code | 含义 |
|---|---|
| `RateLimitExceeded.EndpointRPMExceeded` / `.EndpointTPMExceeded` | 接入点 RPM / TPM 限流 |
| `ModelAccountRpm/Tpm/IpmRateLimitExceeded` | 账户模型 RPM / TPM / IPM 限流 |
| `APIAccountRpmRateLimitExceeded` / `AccountRateLimitExceeded` | 账户接口 RPM / RPM·TPM 限流 |
| `QuotaExceeded`(免费试用额度耗尽 / 排队超限 / 5h·周·月额度) | 各类配额耗尽 |
| `ServerOverloaded` | 服务资源紧张,稍后重试(doubao-seed-1.8 及之前) |
| `RequestBurstTooFast` | 请求量激增触发系统保护(doubao-seed-2.0 及之后) |
| `SetLimitExceeded` | 达到推理限额值(安心体验模式) |
| `InflightBatchsizeExceeded` | 达到当前充值档位最大并发数 |

**非 429、需显式识别**:

| code | HTTP | 含义 |
|---|---|---|
| `AccountOverdueError` | 403 | 账号欠费(余额 < 0)——≈ DeepSeek 402 / 智谱 1113 |
| `OperationDenied.ServiceOverdue` | 403 | 账单已逾期 |
| `InvalidSubscription` | 400 | Coding Plan 套餐未订阅或已过期 |
| `ModelNotOpen` | 404 | 当前账号未开通该模型服务 |
| `UnsupportedModel` | 404 | 该模型不支持 Coding Plan |

### → 透传(原样返回,不熔断)

| 类别 | 代表 code | HTTP |
|---|---|---|
| 参数错误 | `MissingParameter`/`InvalidParameter`/`InvalidParameter.*`/`OutofContextError`(prompt 超长)/ Lean 相关 | 400 |
| 敏感内容 | `SensitiveContentDetected*`/`*RiskDetection`/`Input*Sensitive*`/`Output*Sensitive*` | 400 |
| 鉴权 | `AuthenticationError`/`InvalidAccountStatus` | 401 |
| 权限/状态 | `AccessDenied`/`OperationDenied.PermissionDenied`/`.InvalidState`/`.Unsupported*`/`FileQuotaExceeded` | 403 |
| 未找到 | `InvalidEndpointOrModel.NotFound`/`.ModelIDAccessDisabled`/`NotFound.*` | 404 |
| 服务端错误 | `InternalServiceError`(厂商说可重试,但按设计 5xx 透传) | 500 |

> ⚠️ 早期实现的 `ModelAccessDenied` 在官方文档中**不存在**,已移除;真实"模型不可用"信号是 `ModelNotOpen`/`InvalidSubscription`/`UnsupportedModel`。

---

## DeepSeek(vendor: `deepseek`)

> 标准 HTTP 状态码,最简单:`429` + `402` 走 fallback,其余透传。

| HTTP | 含义 | 判定 |
|---|---|---|
| `400` | Invalid Format(请求体格式错误) | 透传 |
| `401` | Authentication Fails(API Key 错误) | 透传 |
| `402` | Insufficient Balance(余额不足) | **→ fallback**(DeepSeek 独有;其他厂商的 402 透传) |
| `422` | Invalid Parameters(参数非法) | 透传 |
| `429` | Rate Limit Reached(官方建议切到其他厂商) | **→ fallback** |
| `500` | Server Error(厂商建议稍后重试) | 透传(5xx 不 fallback) |
| `503` | Server Overloaded(厂商建议稍后重试) | 透传(5xx 不 fallback) |

---

## 设计决策(已落地)

1. **5xx 全透传**:DeepSeek 500/503、火山 `InternalServiceError`、智谱 1200/1230/1234 虽都"可重试",但保持 spec §8 语义(不 fallback、不熔断),避免坏上游每请求烧穿整条链。
2. **欠费三统一**:DeepSeek `402` / 智谱 `1113` / 火山 `AccountOverdueError` 统一归为"余额耗尽 → fallback"。
3. **套餐/模型不可用 → fallback**:智谱 `1309`/`1311`/`1314`/`1315`、火山 `InvalidSubscription`/`ModelNotOpen`/`UnsupportedModel`。这类无自动重置时间,落到 Transient 短冷却(默认 `cooldown_seconds`=300s),过期后探测、再失败再 trip——无需改 `health.rs`。
4. **关键词兜底覆盖未知厂商**:新接入的 Bearer-key 厂商若返回 `insufficient balance`/`欠费`/`quota` 等未文档化形态,零后端改动也能正确 fallback(见 [adding-a-provider.md](./adding-a-provider.md) D 节)。

## 扩展指引

新增厂商的限流识别改动量见 [`adding-a-provider.md`](./adding-a-provider.md) 决策表 D 节:**只有当服务商用"非 429 + 非关键字"的特殊错误码表达限流/欠费/套餐不可用时**,才需在 `rate_limit_in_json` 加 per-vendor code 分支。若其限流码本身就是 429(智谱/火山/DeepSeek 都是),通用兜底已够。
