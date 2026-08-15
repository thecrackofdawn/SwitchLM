---
layout: docs
title: 安装与快速上手
next:
  title: 接入编程 Agent
  url: /docs/agents/
---

## 下载安装

从 [release 页面](https://github.com/thecrackofdawn/SwitchLM/releases) 下载最新版本，当前提供 Windows 和 Linux 安装包。

## 配置指导

### 添加服务商

![添加服务商](../assets/screenshots/install_guide/add_provider.png)

### 添加模型

![添加模型](../assets/screenshots/install_guide/add_model.png)

> ⚠️ 一定要记住添加模型：只有在这里添加了模型后，才能在路由跟故障转移页面选择到相应的模型。

### 添加路由

![添加路由](../assets/screenshots/install_guide/add_router.png)

路由名称（如 `pro`）就是需要配置在智能体的模型名称。

### 配置失败回退

![配置失败回退](../assets/screenshots/install_guide/add_fallback.png)

这里配置当一个模型额度耗尽后，可以回退调用其他模型。

## Linux 运行注意事项

- **系统托盘**：GNOME（尤其 Wayland）默认无传统托盘，需安装 *AppIndicator and KStatusNotifierItem Support* 扩展；无托盘的环境下关闭窗口将隐藏，再次启动应用（单实例会聚焦既有窗口）即可恢复。
- **密钥存储**：建议运行 gnome-keyring / KWallet；**未安装密钥环**时首次启动会弹出对话框询问是否将访问密钥以明文存入 `~/.local/share/com.switchlm.app/secrets.json`（文件权限 0600）。同意即用文件存储，退出则不保存。
- **不支持 headless / 无桌面服务器**（这是桌面托盘应用）。

完成配置后，继续看[接入编程 Agent]({{ '/docs/agents/' | relative_url }})。
