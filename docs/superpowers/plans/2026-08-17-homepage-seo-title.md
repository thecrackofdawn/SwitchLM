# 首页 SEO 标题优化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Google 搜索 SwitchLM 时首页标题显示「SwitchLM - 多编程套餐管理工具」而非「解决什么问题 | SwitchLM」。

**Architecture:** Jekyll 站点（github-pages 232 / jekyll 3.10 / jekyll-seo-tag 2.8.0）。首页 `index.md` 加 `seo_title` + `description` front matter；布局 `default.html` 用条件分支——有 `seo_title` 时自己输出 `<title>` 并以 `{% seo title=false %}` 压制插件的同名输出，否则维持原 `{% seo %}`。无测试框架，验证方式为 `jekyll build` 后检查 `_site/` 生成的 HTML。

**Tech Stack:** Jekyll (Liquid)、jekyll-seo-tag 2.8.0、PowerShell、git（分支 `gh-pages`）。

**Spec:** `docs/superpowers/specs/2026-08-17-homepage-seo-title-design.md`

## Global Constraints

- 标题精确值：`SwitchLM - 多编程套餐管理工具`（连字符两侧各一个空格）
- 描述精确值：`多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠`
- 页面可见内容零变化：header 大标题仍为 `site.title`「SwitchLM」，副标题仍为 `site.description`
- 其他页面（docs/install/agents/faq）生成的 `<head>` 与改动前逐字节一致
- 不手写 og:title / twitter:title（插件生成，避免重复 meta）
- 本 worktree 只做 gh-pages 站点，不碰主代码库
- 提交信息用英文

---

### Task 1: 布局支持 seo_title 覆盖

**Files:**
- Modify: `_layouts/default.html:6`

**Interfaces:**
- Consumes: 无
- Produces: 布局约定——front matter 中 `seo_title` 非空时，`<title>` 输出 `{{ page.seo_title | escape }}` 且 `<head>` 中仅此一个 title 标签；无 `seo_title` 的页面 head 输出与旧版逐字节一致。Task 2 的 `index.md` 依赖此约定。

- [ ] **Step 1: 修改 default.html 的 head**

把 `_layouts/default.html` 第 6 行的

```html
  {% seo %}
```

替换为：

```html
  {% if page.seo_title %}
  <title>{{ page.seo_title | escape }}</title>
  {% seo title=false %}
  {% else %}
  {% seo %}
  {% endif %}
```

- [ ] **Step 2: 构建验证——无 seo_title 页面不受影响**

Run: `bundle exec jekyll build`
Expected: 构建成功（此时尚无页面声明 seo_title，全站走 else 分支）。

Run: `git diff --stat _site/docs/install.html`
Expected: 无输出（`_site` 在 .gitignore 中且内容与改动前一致；若 `_site` 未被 git 跟踪，改为在 `_site/docs/install.html` 中目视确认 `<title>安装与快速上手 | SwitchLM</title>` 仍存在）。

- [ ] **Step 3: Commit**

```bash
git add _layouts/default.html
git commit -m "Support seo_title front matter for custom page title"
```

---

### Task 2: 首页声明 seo_title 与新 description

**Files:**
- Modify: `index.md:1-3`

**Interfaces:**
- Consumes: Task 1 的布局约定（`seo_title` 字段被识别）
- Produces: 首页 SEO 元数据——`<title>SwitchLM - 多编程套餐管理工具</title>`、meta description 为新描述。发布后 Google Search Console 重新编入索引依赖此产出。

- [ ] **Step 1: 修改 index.md front matter**

把 `index.md` 头部：

```yaml
---
layout: default
---
```

替换为：

```yaml
---
layout: default
seo_title: SwitchLM - 多编程套餐管理工具
description: 多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠
---
```

- [ ] **Step 2: 构建**

Run: `bundle exec jekyll build`
Expected: 构建成功，无 Liquid 报错。

- [ ] **Step 3: 断言首页输出（spec 验收标准 1）**

检查 `_site/index.html`，以下三条全部满足：

1. `<title>` 标签全文档仅一个：PowerShell 计数
   ```powershell
   (Select-String -Path _site\index.html -Pattern '<title>').Count
   ```
   Expected: `1`
2. 该行内容为 `<title>SwitchLM - 多编程套餐管理工具</title>`
3. `<meta name="description" content="多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠" />` 存在；`og:title` 与 `twitter:title` 各出现一次且内容为 `SwitchLM`（无重复 meta）

- [ ] **Step 4: 断言其他页面不变（spec 验收标准 2）**

```powershell
Select-String -Path _site\docs\install.html,_site\docs.html,_site\docs\agents.html,_site\docs\faq.html -Pattern '<title>|description|og:title|twitter:title' | Select-Object -First 12
```

Expected: 各页 `<title>` 仍为「安装与快速上手 | SwitchLM」「文档 | SwitchLM」「接入编程 Agent | SwitchLM」「常见问题 | SwitchLM」，description 仍为旧全站描述 `多编程套餐管理工具 —— 按时间与优先级自动切换模型分发策略`。

- [ ] **Step 5: Commit**

```bash
git add index.md
git commit -m "Set homepage SEO title and description"
```

---

### Task 3: 发布与通知 Google

**Files:**
- 无文件改动（推送 + 线上验证 + Search Console 操作指引）

**Interfaces:**
- Consumes: Task 1、Task 2 已提交的 commit
- Produces: 线上生效的页面

- [ ] **Step 1: 推送**

Run: `git push origin gh-pages`
Expected: 推送成功。

- [ ] **Step 2: 等待 GitHub Pages 构建后验证线上 HTML**

Run（约 1-2 分钟后）：
```powershell
(Invoke-WebRequest -Uri "https://thecrackofdawn.github.io/SwitchLM/" -UseBasicParsing).Content -match '<title>[^<]*</title>'
```
Expected: 输出 `True`，且匹配到的 title 为 `<title>SwitchLM - 多编程套餐管理工具</title>`。

- [ ] **Step 3: 提示用户去 Search Console 请求重新编入索引**

不代做（需要用户 Google 账号登录）。告知用户：Search Console → URL 检查 → 输入 `https://thecrackofdawn.github.io/SwitchLM/` → 「请求编入索引」。Google 快照更新通常需要数天。
