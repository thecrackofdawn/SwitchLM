---
layout: docs
title: 接入编程 Agent
prev:
  title: 安装与快速上手
  url: /docs/install/
next:
  title: 常见问题
  url: /docs/faq/
---

完成「添加服务商 → 添加模型 → 添加路由」配置后，把 Agent 的接口地址指向本地代理（默认端口 `6950`），模型名填你配置的路由名称。

## Claude Code（Anthropic 协议）

bash：

```bash
export ANTHROPIC_BASE_URL=http://localhost:6950
export ANTHROPIC_AUTH_TOKEN=<任意非空字符串>
```

Windows PowerShell：

```powershell
$env:ANTHROPIC_BASE_URL = "http://localhost:6950"
$env:ANTHROPIC_AUTH_TOKEN = "任意非空字符串"
```

## Cursor / Cline 等（OpenAI 协议）

Base URL 填 `http://localhost:6950/v1`，模型名填你配置的路由名称（或其别名）。

## 解析链路

> Agent 发送的模型名会先解析为路由（Profile），再映射到背后的真实模型与上游厂商。路由支持配置别名（如 `claude-sonnet-4`），让 Agent 无需改动模型名即可走代理。
