# 更新日志
## v0.1.1-beta
* **编码智能体上下文自动同步**（默认关，设置页开启）：按当前时段主路由模型的真实上下文，自动修正 Claude Code 与 OpenCode 配置中过期的上下文声明——Claude Code 调整 `~/.claude/settings.json` 模型名的 `[1m]` 后缀与 `CLAUDE_CODE_MAX_CONTEXT_TOKENS`；OpenCode 调整 `~/.config/opencode/opencode.jsonc` 中指向本代理 provider 的各模型 `limit.context`。关闭开关不影响已写入内容
* **用量统计功能**：本地统计每个服务商的请求次数与 token 消耗
* **后台内存优化功能**：启用后，窗口隐藏到托盘5分钟自动销毁webview进程以节省内存，代理与托盘不受影响，重新打开窗口时恢复到最后访问的页面
* **托盘优化**：托盘tooltip显示剩余配额而非已使用百分比

## v0.1.0-beta
* 多服务商配置及套餐用量查询，当前支持质谱、火山、千问（token plan）、deepseek。
* 支持配置策略在不同时段调用不同服务商模型，支持配置某个套餐限额后按照策略回退调用其他套餐的模型