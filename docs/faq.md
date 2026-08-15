---
layout: docs
title: 常见问题
prev:
  title: 接入编程 Agent
  url: /docs/agents/
---

## 支持哪些服务商？

智谱 GLM、阿里云百炼/通义千问、火山方舟、DeepSeek 等提供 OpenAI 兼容接口的厂商均可添加。

## 默认端口是多少？能改吗？

默认 `6950`。可在应用设置中修改代理监听端口，修改后 Agent 侧的接口地址需同步更新。

## 密钥存储安全吗？

优先使用系统密钥环（gnome-keyring / KWallet 等）。未安装密钥环时，首次启动会询问是否将访问密钥以明文存入 `~/.local/share/com.switchlm.app/secrets.json`（文件权限 0600）；需要更安全的存储请安装密钥环。

## Linux 下没有系统托盘怎么办？

安装 *AppIndicator and KStatusNotifierItem Support* 扩展。无托盘环境下关闭窗口只是隐藏，再次启动应用（单实例会聚焦既有窗口）即可恢复。

## 如何本地编译？

参见[主仓库 README](https://github.com/thecrackofdawn/SwitchLM#本地编译) 的「本地编译」部分（Node.js ≥ 20 + Rust stable，`npm run tauri build` 产出安装包）。

## 后续有什么计划？

- 套餐并发配置（避免无谓的 429）
- sk-xxx 认证（团队套餐管理、服务化部署）
- 辅助自动化配置常见编程智能体
- 请求缓存（验证收益后）
- Token 消耗统计
