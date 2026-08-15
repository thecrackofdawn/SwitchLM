# SwitchLM GitHub Pages 内容丰富化设计

- 日期：2026-08-15
- 状态：已批准（方案 A）
- 仓库：`thecrackofdawn/SwitchLM` 的 `gh-pages` 分支 worktree

## 背景与目标

当前 gh-pages 站点（Jekyll + Cayman 主题）只有一个简略的 `index.md`。主仓库 README 内容丰富（问题背景、9 张截图、安装配置指导、Roadmap），但未利用。

**目标**：把站点丰富为「首页落地页 + 独立文档页」的产品官网，纯中文，保留 Cayman 主题底子并做定制样式，零外部依赖（无 CDN、无 JS 库），GitHub Pages 服务端构建（无需本地 Ruby）。

## 非目标

- 不做多语言（中英双语）
- 不做 CI 自动同步主仓库 README（每次发版后手动同步）
- 不 fork 整个 Cayman 主题

## 站点结构

```
_config.yml                    # 主题、标题、品牌色变量
index.md                       # 首页落地页
assets/
  app-icon.svg                 # 项目 logo（从主仓库拷贝）
  screenshots/                 # 全部 9 张截图（从主仓库拷贝）
  css/style.scss               # @import Cayman 后追加定制样式
_includes/
  lightbox.html                # 截图灯箱组件（<dialog> 原生实现）
  nav.html                     # 顶部导航（首页 · 文档 · GitHub）
_layouts/
  default.html                 # 基于 Cayman default 扩展：注入导航、logo
  docs.html                    # 文档布局：窄内容宽 + 上一页/下一页
docs/
  install.md                   # 安装与快速上手
  agents.md                    # 接入编程 Agent
  faq.md                       # FAQ
docs.md                        # 文档区导航页
```

## 首页设计（index.md）

按顺序六个区块：

1. **Hero**（Cayman 页头自带样式）：标题 SwitchLM；副标题「多编程套餐管理工具 —— 按时间与优先级自动切换模型分发策略」；按钮组：下载最新版（→ releases）+ GitHub。
2. **问题背景**：2-3 段，核心价值一句话——「拥有多个服务商编程套餐时，按『时间 + 限额』自动切换分发策略，最大化吃满每份优惠」；两个典型场景（按套餐优先级切换、按时段切换）。
3. **特性卡片网格**（2×3）：
   - 🖥️ 桌面托盘应用 — 启动即起代理，开箱即用
   - 🔄 失败自动转移 — 额度耗尽自动切换，任务不打断
   - ⏰ 按时段策略 — 夜间优先打折套餐，高峰避开加价套餐
   - 🔀 路由与别名 — Agent 无需改动模型名即可走代理
   - 📊 用量可视化 — 各套餐额度消耗一目了然
   - 🤝 多协议接入 — 兼容 Anthropic / OpenAI 协议的 Agent
4. **截图画廊**：4 张主截图（主界面、用量、失败转移、策略配置），两列网格，点击 `<dialog>` 灯箱放大。
5. **快速开始**：3 步骤编号，链接到 `docs/install`。
6. **页脚**：三列——项目信息（MIT License）/ 文档入口 / Roadmap 简表（勾选状态从主仓库手动同步）。

## 文档页设计

三个文档页共用 `docs` 布局：窄内容宽度、顶部导航、页尾上一页/下一页。

### docs/install.md — 安装与快速上手

1. 下载安装：从 releases 下载（Windows / Linux 安装包说明）
2. 配置指导（每步一节，配安装引导截图×4）：
   - 添加服务商 → 添加模型（附提示框：必须添加模型才能在路由中选择）→ 添加路由（路由名即 Agent 填的模型名）→ 配置失败回退
3. Linux 注意事项：系统托盘扩展（AppIndicator）、密钥存储（密钥环 / 明文 fallback 0600 权限）、不支持 headless
4. 页尾导航 → 接入编程 Agent

### docs/agents.md — 接入编程 Agent

- Claude Code（Anthropic 协议）：bash + PowerShell 环境变量写法（`ANTHROPIC_BASE_URL=http://localhost:6950`）
- Cursor / Cline 等（OpenAI 协议）：Base URL `http://localhost:6950/v1`，模型名填路由名
- 说明框：模型名 → 路由（Profile）→ 真实模型与上游厂商的解析链路、别名机制
- 页尾导航 → FAQ

### docs/faq.md — FAQ（初始 6 条）

1. 支持哪些服务商？（智谱 GLM、百炼/千问、火山方舟、DeepSeek 等提供 OpenAI 兼容接口的厂商）
2. 默认端口是多少？能改吗？（默认 6950）
3. 密钥存储安全吗？（密钥环优先；未装密钥环时询问是否明文存 `~/.local/share/com.switchlm.app/secrets.json`，0600）
4. Linux 无托盘怎么办？（装 AppIndicator 扩展；无托盘时关窗隐藏，再次启动恢复）
5. 如何本地编译？（链接主仓库 README 构建部分）
6. 后续有什么计划？（Roadmap 概览）

### docs.md — 文档区导航页

三个文档入口的卡片式索引页。

## 视觉定制

**品牌色**：从主仓库 `app-icon-scaled.svg` 提取主色 `#00e5ff → #2979ff`（青到蓝渐变）作为 Cayman 页头背景与按钮色。Logo 用于页头标题左侧。

**自定义样式清单**（全部在 `assets/css/style.scss`，`@import` Cayman 后追加覆盖）：

| 组件 | 样式 |
|------|------|
| 页头 logo | 页头标题左侧 36px 内联 SVG 图标 |
| 导航栏 | 页头下方文字导航：首页 · 文档 · GitHub，粘性 |
| 特性卡片 | 2/3 列响应式网格，圆角卡片、浅边框、hover 微上浮 |
| 截图画廊 | 2 列网格、圆角 + 阴影、hover 轻微放大；`<dialog>` 灯箱，Esc/点遮罩关闭 |
| 提示框 | 文档页引用块样式化（左侧色条 + 浅色背景） |
| 代码块 | 深色背景；复制按钮为低优先级可选项 |
| 页脚 | 浅灰背景、三列布局 |

**约束**：零外部依赖（无 CDN、无 JS 库、无网络字体），GitHub Pages CSP 下完全可用；移动端单列。

## 数据流与维护

- 截图与 logo 从主仓库 `assets/` 拷贝到本 worktree `assets/`（gh-pages 与主分支文件独立）
- 文档内容以主仓库 README 为源，每次发版后手动同步
- 提交推送 `gh-pages` 分支即发布

## 错误处理 / 测试

- 无运行时逻辑，唯一交互是灯箱（`<dialog>` 原生，降级为浏览器默认行为）
- 验证方式：GitHub Pages 构建成功 + 浏览器检查首页、三个文档页、灯箱、移动端响应式
