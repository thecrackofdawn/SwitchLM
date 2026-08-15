# SwitchLM GitHub Pages 内容丰富化 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 gh-pages 站点从单页简略首页升级为「首页落地页 + 独立文档页」的产品官网。

**Architecture:** 纯 Jekyll 静态站点，GitHub Pages 服务端构建。保留 `jekyll-theme-cayman` 远程主题样式，通过自建 `default`/`docs` 布局和 `assets/css/style.scss`（`@import` 主题后追加覆盖）实现定制。零外部依赖（无 CDN、无 JS 库、无网络字体）。

**Tech Stack:** Jekyll（GitHub Pages 自动构建，无需本地 Ruby）、Cayman 主题、原生 `<dialog>` 灯箱、Liquid 模板。

## Global Constraints

- 语言：站点内容纯中文；`lang: zh-CN`
- 站点 URL：`https://thecrackofdawn.github.io/SwitchLM/`，因此 `baseurl: "/SwitchLM"`，**所有站内链接和资源路径必须用 `{{ ... | relative_url }}`**（Jekyll 会自动拼上 baseurl）
- 品牌色：青到蓝渐变 `#00b0ff → #2979ff`（取自 app-icon），页头背景与按钮使用
- 零外部依赖：无 CDN、无 JS 库、无网络字体
- 不 fork 主题、不引入 CI 同步；文档内容以主仓库 README 为源手动维护
- 主仓库路径：`C:\Users\cd\Documents\projects\SwitchLM`；本 worktree 路径：`C:\Users\cd\Documents\projects\switchlm-gh-pages`
- 主分支为 `gh-pages`（本 worktree 已在其上），直接提交推送即可发布
- 无本地 Ruby/Jekyll 环境时，「测试」步骤为结构性验证（文件存在、front matter 完整、引用路径与实际文件匹配），最终以 GitHub Pages 线上构建验证

---

### Task 1: 站点配置与资源拷贝

**Files:**
- Modify: `_config.yml`（整体重写）
- Create: `assets/app-icon.svg`（从主仓库拷贝）
- Create: `assets/screenshots/`（9 张 png，从主仓库拷贝）

**Interfaces:**
- Produces: 后续任务依赖的站点配置键 —— `site.title`、`site.description`、`site.github_repo`、`site.releases_url`、`site.baseurl`；资源路径 `/assets/app-icon.svg`、`/assets/screenshots/*.png`

- [ ] **Step 1: 重写 `_config.yml`**

```yaml
title: SwitchLM
description: 多编程套餐管理工具 —— 按时间与优先级自动切换模型分发策略
theme: jekyll-theme-cayman
lang: zh-CN
url: https://thecrackofdawn.github.io
baseurl: "/SwitchLM"
show_downloads: false
github_repo: https://github.com/thecrackofdawn/SwitchLM
releases_url: https://github.com/thecrackofdawn/SwitchLM/releases
markdown: kramdown
```

- [ ] **Step 2: 拷贝资源**

```powershell
New-Item -ItemType Directory -Force assets\screenshots | Out-Null
Copy-Item C:\Users\cd\Documents\projects\SwitchLM\assets\app-icon-scaled.svg assets\app-icon.svg
Copy-Item C:\Users\cd\Documents\projects\SwitchLM\assets\screenshots\*.png assets\screenshots\
Copy-Item -Recurse C:\Users\cd\Documents\projects\SwitchLM\assets\screenshots\install_guide assets\screenshots\
```

- [ ] **Step 3: 验证资源就位**

Run: `Get-ChildItem -Recurse assets | Select-Object -ExpandProperty FullName`
Expected: `app-icon.svg` + 5 张 `screenshots\*.png`（switchlm_dashboard / switchlm_quota / switchlm_fallback / switchlm_timestrategy + install_guide 下 4 张）

- [ ] **Step 4: 提交**

```powershell
git add _config.yml assets
git commit -m "Add site config and copy brand assets from main repo"
```

---

### Task 2: 布局与公共组件

**Files:**
- Create: `_includes/nav.html`
- Create: `_includes/lightbox.html`
- Create: `_includes/docs-pager.html`
- Create: `_layouts/default.html`
- Create: `_layouts/docs.html`

**Interfaces:**
- Consumes: Task 1 的 `site.title`/`site.description`/`site.releases_url`/`site.github_repo`、资源路径
- Produces: 布局 `default`、`docs`（供 `index.md` 与 docs 页面 front matter 使用）；灯箱约定——页面里任何 `<a class="lightbox-link" href="完整图路径">` 包裹 `<img>` 即获得点击放大；分页组件约定——页面 front matter 写 `prev: {title, url}` / `next: {title, url}` 后 `{% include docs-pager.html prev=page.prev next=page.next %}` 生效

- [ ] **Step 1: 写 `_includes/nav.html`**

```html
<nav class="site-nav">
  <a href="{{ '/' | relative_url }}">首页</a>
  <a href="{{ '/docs/' | relative_url }}">文档</a>
  <a href="{{ site.releases_url }}">下载</a>
  <a href="{{ site.github_repo }}">GitHub</a>
</nav>
```

- [ ] **Step 2: 写 `_includes/lightbox.html`**

```html
<dialog id="lightbox" class="lightbox">
  <button class="lightbox-close" aria-label="关闭">&#10005;</button>
  <img src="" alt="截图放大预览">
</dialog>
<script>
  (function () {
    var dialog = document.getElementById('lightbox');
    if (!dialog || !dialog.showModal) return;
    dialog.addEventListener('click', function (e) {
      if (e.target === dialog || e.target.classList.contains('lightbox-close')) dialog.close();
    });
    document.querySelectorAll('a.lightbox-link').forEach(function (a) {
      a.addEventListener('click', function (e) {
        e.preventDefault();
        dialog.querySelector('img').src = a.getAttribute('href');
        dialog.showModal();
      });
    });
  })();
</script>
```

- [ ] **Step 3: 写 `_includes/docs-pager.html`**

```html
{% if include.prev or include.next %}
<nav class="docs-pager">
  {% if include.prev %}
  <a class="pager-prev" href="{{ include.prev.url | relative_url }}">&#8592; {{ include.prev.title }}</a>
  {% endif %}
  {% if include.next %}
  <a class="pager-next" href="{{ include.next.url | relative_url }}">{{ include.next.title }} &#8594;</a>
  {% endif %}
</nav>
{% endif %}
```

- [ ] **Step 4: 写 `_layouts/default.html`**

结构参考 Cayman 官方布局（`page-header` + `main-content` 类名保留，确保主题样式生效），注入导航、logo、Hero 按钮组：

```html
<!DOCTYPE html>
<html lang="{{ site.lang | default: 'zh-CN' }}">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  {% seo %}
  <link rel="icon" type="image/svg+xml" href="{{ '/assets/app-icon.svg' | relative_url }}">
  <link rel="stylesheet" href="{{ '/assets/css/style.css' | relative_url }}">
</head>
<body>
  <header class="page-header" role="banner">
    {% include nav.html %}
    <h1 class="project-name">
      <img class="header-logo" src="{{ '/assets/app-icon.svg' | relative_url }}" alt="SwitchLM 图标">
      {{ site.title }}
    </h1>
    <h2 class="project-tagline">{{ site.description }}</h2>
    {% if page.hero_buttons != false %}
    <p class="hero-buttons">
      <a href="{{ site.releases_url }}" class="btn">&#11015; 下载最新版</a>
      <a href="{{ site.github_repo }}" class="btn">&#9733; GitHub</a>
    </p>
    {% endif %}
  </header>
  <main id="content" class="main-content" role="main">
    {{ content }}
  </main>
</body>
</html>
```

- [ ] **Step 5: 写 `_layouts/docs.html`**

```html
---
layout: default
hero_buttons: false
---
<article class="docs-article">
  {{ content }}
  {% include docs-pager.html prev=page.prev next=page.next %}
</article>
```

注意：docs 布局通过 front matter `hero_buttons: false` 传给 default 布局关闭大按钮组；`layout: default` 链式嵌套时，内层布局的 front matter 会作为 `layout` 变量而非 `page` 变量——为使 `{% if page.hero_buttons %}` 生效，default.html 中的判断改为同时检查两者：

```liquid
{% if page.hero_buttons != false and layout.hero_buttons != false %}
```

（Step 4 中的条件行以此为准。）

- [ ] **Step 6: 验证**

Run: `Get-ChildItem _includes, _layouts`
Expected: nav.html / lightbox.html / docs-pager.html / default.html / docs.html 五个文件齐全，且每个 `.html` 中所有 `relative_url` 前的路径都以 `/assets/`、`/docs/` 或 `/` 开头（无裸相对路径）。

- [ ] **Step 7: 提交**

```powershell
git add _includes _layouts
git commit -m "Add site layouts, nav, lightbox and pager components"
```

---

### Task 3: 定制样式

**Files:**
- Create: `assets/css/style.scss`

**Interfaces:**
- Consumes: Task 2 布局中的类名（`site-nav`、`header-logo`、`hero-buttons`、`docs-article`、`docs-pager`、`pager-prev/next`、`lightbox`、`lightbox-close`）
- Produces: 首页与文档页使用的组件类 —— `feature-grid`/`feature-card`、`gallery`/`lightbox-link`、`callout`、`site-footer`/`footer-grid`、`steps`

- [ ] **Step 1: 写 `assets/css/style.scss`**

```scss
---
---
@import "jekyll-theme-cayman";

/* ===== 品牌化页头 ===== */
.page-header {
  background: linear-gradient(120deg, #0091ea 0%, #2979ff 100%);
  padding: 1.5rem 1rem 3rem;
}
.header-logo {
  width: 36px;
  height: 36px;
  vertical-align: -6px;
  margin-right: 10px;
  border-radius: 8px;
}
.site-nav {
  font-size: 0.95rem;
  margin-bottom: 1.5rem;
}
.site-nav a {
  color: rgba(255, 255, 255, 0.9);
  margin: 0 10px;
  text-decoration: none;
  border-bottom: 1px solid transparent;
}
.site-nav a:hover { border-bottom-color: rgba(255, 255, 255, 0.7); }
.hero-buttons .btn {
  margin: 0 6px 10px;
  background: rgba(255, 255, 255, 0.14);
}

/* ===== 特性卡片 ===== */
.feature-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 16px;
  margin: 1.5rem 0;
}
.feature-card {
  border: 1px solid #e1e4e8;
  border-radius: 10px;
  padding: 18px 16px;
  transition: transform 0.15s ease, box-shadow 0.15s ease;
}
.feature-card:hover {
  transform: translateY(-3px);
  box-shadow: 0 6px 16px rgba(0, 0, 0, 0.08);
}
.feature-card .feature-icon { font-size: 1.6rem; }
.feature-card h3 { margin: 8px 0 6px; font-size: 1.05rem; }
.feature-card p { margin: 0; color: #57606a; font-size: 0.92rem; }
@media (max-width: 768px) { .feature-grid { grid-template-columns: 1fr; } }

/* ===== 截图画廊与灯箱 ===== */
.gallery {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 16px;
  margin: 1.5rem 0;
}
@media (max-width: 768px) { .gallery { grid-template-columns: 1fr; } }
.gallery a.lightbox-link {
  display: block;
  text-decoration: none;
  color: inherit;
  border: 1px solid #e1e4e8;
  border-radius: 10px;
  overflow: hidden;
  transition: transform 0.15s ease, box-shadow 0.15s ease;
}
.gallery a.lightbox-link:hover {
  transform: translateY(-3px);
  box-shadow: 0 6px 16px rgba(0, 0, 0, 0.1);
}
.gallery img { display: block; width: 100%; margin: 0; border-radius: 10px 10px 0 0; }
.gallery figcaption,
.gallery .caption {
  display: block;
  text-align: center;
  padding: 8px;
  font-size: 0.9rem;
  color: #57606a;
}
.lightbox {
  border: none;
  border-radius: 8px;
  padding: 0;
  max-width: 92vw;
  max-height: 92vh;
  background: transparent;
}
.lightbox::backdrop { background: rgba(0, 0, 0, 0.75); }
.lightbox img { display: block; max-width: 92vw; max-height: 92vh; border-radius: 8px; }
.lightbox-close {
  position: absolute;
  top: 10px;
  right: 12px;
  border: none;
  background: rgba(0, 0, 0, 0.55);
  color: #fff;
  font-size: 16px;
  line-height: 1;
  padding: 6px 9px;
  border-radius: 50%;
  cursor: pointer;
}

/* ===== 快速开始步骤 ===== */
.steps { counter-reset: step; margin: 1.5rem 0; padding: 0; list-style: none; }
.steps li {
  counter-increment: step;
  position: relative;
  padding: 0 0 18px 52px;
}
.steps li::before {
  content: counter(step);
  position: absolute;
  left: 0;
  top: -4px;
  width: 34px;
  height: 34px;
  border-radius: 50%;
  background: linear-gradient(120deg, #0091ea, #2979ff);
  color: #fff;
  font-weight: 600;
  display: flex;
  align-items: center;
  justify-content: center;
}

/* ===== 文档页 ===== */
.docs-article { max-width: 780px; }
.docs-article img { max-width: 100%; border: 1px solid #e1e4e8; border-radius: 8px; }
.docs-pager {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  margin-top: 3rem;
  padding-top: 1.5rem;
  border-top: 1px solid #e1e4e8;
}
.docs-pager a {
  text-decoration: none;
  color: #0366d6;
  padding: 8px 14px;
  border: 1px solid #e1e4e8;
  border-radius: 8px;
}
.docs-pager a:hover { background: #f6f8fa; }

/* ===== 提示框（引用块） ===== */
.main-content blockquote {
  border-left: 4px solid #2979ff;
  background: #f0f7ff;
  padding: 10px 16px;
  border-radius: 0 8px 8px 0;
  color: #24292f;
}

/* ===== 页脚 ===== */
.site-footer {
  margin-top: 3rem;
  padding-top: 2rem;
  border-top: 1px solid #e1e4e8;
}
.footer-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 24px;
}
@media (max-width: 768px) { .footer-grid { grid-template-columns: 1fr; } }
.footer-grid h4 { margin: 0 0 8px; font-size: 0.95rem; }
.footer-grid ul { margin: 0; padding-left: 18px; font-size: 0.9rem; color: #57606a; }
```

- [ ] **Step 2: 验证**

Run: `Select-String -Path assets\css\style.scss -Pattern 'jekyll-theme-cayman'`
Expected: 命中 `@import` 行（首行为空 front matter `---`，第二行 `@import "jekyll-theme-cayman";`）；文件包含 Task 2 全部组件类名的样式定义。

- [ ] **Step 3: 提交**

```powershell
git add assets/css/style.scss
git commit -m "Add custom brand styles on top of Cayman theme"
```

---

### Task 4: 首页

**Files:**
- Modify: `index.md`（整体重写）

**Interfaces:**
- Consumes: `layout: default`（Task 2）、`feature-card`/`gallery`/`steps`/`site-footer`/`footer-grid` 类（Task 3）、`lightbox.html`（Task 2）、截图路径（Task 1）
- Produces: 线上首页 `/SwitchLM/`；文档入口链接 `/docs/`、`/docs/install/` 等（Task 5-7 生成）

- [ ] **Step 1: 重写 `index.md`**

```markdown
---
layout: default
---

## 解决什么问题

国产大模型厂商（智谱 GLM、阿里云百炼/通义千问、火山方舟、DeepSeek 等）普遍推出**编程套餐**：包月/包周/5 小时窗口的额度，计费窗口、闲时优惠各不相同——有的夜间打折、有的忙时加价。同时持有多个套餐时，如何把每份优惠都吃满就成了问题。

SwitchLM 的核心能力：**拥有多个服务商编程套餐时，按「时间 + 限额」自动切换分发策略，在不同服务商模型之间无感切换，最大化利用不同套餐的优惠。**

典型场景：

- **按套餐优先级切换**：优先使用智谱套餐，额度耗光后自动切到其他套餐继续跑，不打断任务。
- **按时段切换**：配置夜间优先使用千问套餐（夜间打折）、下午不用智谱套餐（高峰加价），其余时间走默认模型。

## 特性

<div class="feature-grid">
  <div class="feature-card"><div class="feature-icon">🖥️</div><h3>桌面托盘应用</h3><p>启动即起代理，专注分发场景，开箱即用</p></div>
  <div class="feature-card"><div class="feature-icon">🔄</div><h3>失败自动转移</h3><p>额度耗尽自动切换到其他套餐，任务不打断</p></div>
  <div class="feature-card"><div class="feature-icon">⏰</div><h3>按时段策略</h3><p>夜间优先打折套餐，高峰避开加价套餐</p></div>
  <div class="feature-card"><div class="feature-icon">🔀</div><h3>路由与别名</h3><p>Agent 无需改动模型名即可走代理</p></div>
  <div class="feature-card"><div class="feature-icon">📊</div><h3>用量可视化</h3><p>各套餐额度消耗一目了然</p></div>
  <div class="feature-card"><div class="feature-icon">🤝</div><h3>多协议接入</h3><p>兼容 Anthropic / OpenAI 协议的编程 Agent</p></div>
</div>

## 页面演示

<div class="gallery">
  <a class="lightbox-link" href="{{ '/assets/screenshots/switchlm_dashboard.png' | relative_url }}"><img src="{{ '/assets/screenshots/switchlm_dashboard.png' | relative_url }}" alt="主界面"><span class="caption">主界面</span></a>
  <a class="lightbox-link" href="{{ '/assets/screenshots/switchlm_quota.png' | relative_url }}"><img src="{{ '/assets/screenshots/switchlm_quota.png' | relative_url }}" alt="套餐用量"><span class="caption">用量显示</span></a>
  <a class="lightbox-link" href="{{ '/assets/screenshots/switchlm_fallback.png' | relative_url }}"><img src="{{ '/assets/screenshots/switchlm_fallback.png' | relative_url }}" alt="失败转移"><span class="caption">失败转移</span></a>
  <a class="lightbox-link" href="{{ '/assets/screenshots/switchlm_timestrategy.png' | relative_url }}"><img src="{{ '/assets/screenshots/switchlm_timestrategy.png' | relative_url }}" alt="策略配置"><span class="caption">策略配置</span></a>
</div>

{% include lightbox.html %}

## 快速开始

<ol class="steps">
  <li><strong>下载安装</strong>：从 <a href="{{ site.releases_url }}">release 页面</a>下载最新版本（Windows / Linux）。</li>
  <li><strong>完成配置</strong>：添加服务商 → 添加模型 → 添加路由 → 配置失败回退，详见<a href="{{ '/docs/install/' | relative_url }}">安装与快速上手</a>。</li>
  <li><strong>接入 Agent</strong>：把 Claude Code / Cursor 等的接口地址指向本地代理，详见<a href="{{ '/docs/agents/' | relative_url }}">接入编程 Agent</a>。</li>
</ol>

<footer class="site-footer">
  <div class="footer-grid">
    <div>
      <h4>项目</h4>
      <ul>
        <li>MIT License</li>
        <li><a href="{{ site.github_repo }}">GitHub 仓库</a></li>
        <li><a href="{{ site.releases_url }}">下载发布版</a></li>
      </ul>
    </div>
    <div>
      <h4>文档</h4>
      <ul>
        <li><a href="{{ '/docs/install/' | relative_url }}">安装与快速上手</a></li>
        <li><a href="{{ '/docs/agents/' | relative_url }}">接入编程 Agent</a></li>
        <li><a href="{{ '/docs/faq/' | relative_url }}">常见问题</a></li>
      </ul>
    </div>
    <div>
      <h4>Roadmap</h4>
      <ul>
        <li>套餐并发配置</li>
        <li>sk-xxx 认证（团队部署）</li>
        <li>Agent 自动化配置</li>
        <li>Token 消耗统计</li>
      </ul>
    </div>
  </div>
</footer>
```

- [ ] **Step 2: 验证**

Run: `Select-String -Path index.md -Pattern 'relative_url' | Measure-Object | Select-Object -ExpandProperty Count`
Expected: 计数 ≥ 12；且 `assets/screenshots/` 中引用的 4 个文件名与 Task 1 拷贝的文件一致（人工核对一次）。

- [ ] **Step 3: 提交**

```powershell
git add index.md
git commit -m "Rewrite homepage with features, gallery, quick start and footer"
```

---

### Task 5: 文档页 — 安装与快速上手

**Files:**
- Create: `docs/install.md`

**Interfaces:**
- Consumes: `layout: docs`（Task 2）、`docs/install_guide/*.png` 截图（Task 1）
- Produces: 页面 URL `/SwitchLM/docs/install/`；front matter `next: {title: 接入编程 Agent, url: /docs/agents/}` 供分页组件使用（与 Task 6 的 `prev` 对应）

- [ ] **Step 1: 写 `docs/install.md`**

```markdown
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

完成配置后，继续看[接入编程 Agent](/docs/agents/)。
```

注意图片路径：`docs/install.md` 里的相对路径 `../assets/...` 在 Jekyll 构建后指向站点根 `assets/`，正确。

- [ ] **Step 2: 验证**

Run: `Select-String -Path docs\install.md -Pattern '!\['`
Expected: 4 处图片引用，文件名与 `assets\screenshots\install_guide\` 下的 4 个文件一致。

- [ ] **Step 3: 提交**

```powershell
git add docs/install.md
git commit -m "Add install and quick-start guide doc"
```

---

### Task 6: 文档页 — 接入编程 Agent

**Files:**
- Create: `docs/agents.md`

**Interfaces:**
- Consumes: `layout: docs`（Task 2）
- Produces: 页面 URL `/SwitchLM/docs/agents/`；front matter `prev`（对应 Task 5 的 `next`）与 `next`（对应 Task 7 FAQ 的 `prev`）

- [ ] **Step 1: 写 `docs/agents.md`**

````markdown
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
````

- [ ] **Step 2: 验证**

Run: `Get-Content docs\agents.md -TotalCount 8`
Expected: front matter 含 `layout: docs`、`prev.url: /docs/install/`、`next.url: /docs/faq/`，与 Task 5/Task 7 的对应字段一致。

- [ ] **Step 3: 提交**

```powershell
git add docs/agents.md
git commit -m "Add agent integration doc"
```

---

### Task 7: FAQ 与文档导航页

**Files:**
- Create: `docs/faq.md`
- Create: `docs.md`

**Interfaces:**
- Consumes: `layout: docs`（Task 2）、`layout: default`（Task 2）
- Produces: 页面 URL `/SwitchLM/docs/faq/` 与 `/SwitchLM/docs/`（首页导航「文档」指向此处）

- [ ] **Step 1: 写 `docs/faq.md`**

```markdown
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
```

- [ ] **Step 2: 写 `docs.md`（文档导航页）**

```markdown
---
layout: default
title: 文档
---

## 文档

<div class="feature-grid">
  <a class="feature-card" href="{{ '/docs/install/' | relative_url }}"><div class="feature-icon">📦</div><h3>安装与快速上手</h3><p>下载安装、添加服务商/模型/路由、失败回退与 Linux 注意事项</p></a>
  <a class="feature-card" href="{{ '/docs/agents/' | relative_url }}"><div class="feature-icon">🤖</div><h3>接入编程 Agent</h3><p>Claude Code、Cursor / Cline 等接入本地代理的方法</p></a>
  <a class="feature-card" href="{{ '/docs/faq/' | relative_url }}"><div class="feature-icon">❓</div><h3>常见问题</h3><p>支持的服务商、端口、密钥存储、Roadmap 等</p></a>
</div>
```

- [ ] **Step 3: 验证**

Run: `Select-String -Path docs\faq.md,docs.md -Pattern 'docs/'`
Expected: `docs/faq.md` 的 `prev.url` 与 `docs/agents.md` 的 `next.url` 均为 `/docs/faq/`；`docs.md` 三个链接 `/docs/install/`、`/docs/agents/`、`/docs/faq/` 与实际文件名一一对应。

- [ ] **Step 4: 提交**

```powershell
git add docs/faq.md docs.md
git commit -m "Add FAQ and docs index page"
```

---

### Task 8: 推送与线上验证

**Files:**
- 无新文件；推送全部提交并线上验证

**Interfaces:**
- Consumes: Task 1-7 的全部产出
- Produces: 线上站点 `https://thecrackofdawn.github.io/SwitchLM/`

- [ ] **Step 1: 确认仓库 Settings → Pages**

在浏览器打开 `https://github.com/thecrackofdawn/SwitchLM/settings/pages`，确认 Source 为 `gh-pages` 分支 `/ (root)`。若已是，跳过。

- [ ] **Step 2: 推送**

```powershell
git push origin gh-pages
```

- [ ] **Step 3: 等待构建并验证线上页面**

推送后等待 1-2 分钟（GitHub Pages 构建），然后逐项检查：

Run: 用浏览器或 `Invoke-WebRequest https://thecrackofdawn.github.io/SwitchLM/ -UseBasicParsing | Select-Object -ExpandProperty StatusCode`
Expected: `200`

检查清单：
- 首页加载正常：Hero（青蓝渐变页头 + logo + 双按钮）、特性卡片 6 张、截图画廊 4 张（图片均 200）、快速开始 3 步、页脚三列
- 点击任一截图弹出灯箱，Esc / 点遮罩可关闭
- `/SwitchLM/docs/` 三张文档卡片可点
- `/SwitchLM/docs/install/` 4 张配置截图显示、blockquote 提示框有样式、页尾「接入编程 Agent →」链接
- `/SwitchLM/docs/agents/` 上一页/下一页双向正确、代码块样式正常
- 浏览器窗口缩窄到手机宽度：卡片、画廊变单列

若构建失败：到 `https://github.com/thecrackofdawn/SwitchLM/actions` 查看 Pages build 日志定位（常见原因：Sass 语法错误、Liquid 标签不闭合）。

- [ ] **Step 4: 收尾提交（如有线上修复）**

```powershell
git add -A
git commit -m "Fix issues found in production verification" --allow-empty
git push origin gh-pages
```

（无修复则跳过此步。）
