# 设计：首页 Google 搜索标题优化（SEO snippet）

日期：2026-08-17
状态：已批准

## 背景

Google 搜索 SwitchLM 时，首页结果显示为「解决什么问题 | SwitchLM」。原因：`{% seo %}` 标签在页面无 front matter title 时，取正文第一个标题（h2「解决什么问题」）作为 `<title>`。

期望的搜索结果：

> **SwitchLM - 多编程套餐管理工具**
> `thecrackofdawn.github.io › SwitchLM`
> 多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠

## 范围

仅首页。docs / install / agents / faq 页面不动（它们的搜索标题正常）。

## 方案

选定方案 A：自定义 `seo_title` front matter 字段 + `{% seo title=false %}` 条件分支（经评审修订，弃用最初"后写覆盖"设计）。

- 被否的备选及原因：
  - **后写覆盖（原设计，已否）**：HTML5 规范要求文档只有一个 `<title>`，浏览器与爬虫在多个 title 时取第一个——后写的覆盖标签不会生效，Google 仍显示旧标题。
  - **B（原生 `page.title`）**：插件原生支持且不影响可见内容，但 `Drop#title` 会拼接 ` | site.title`，得到「SwitchLM - 多编程套餐管理工具 | SwitchLM」，品牌重复，不符合批准的标题；改 `site.title` 去后缀会连带改 header 大标题与 og:site_name。
  - **C（改正文标题）**：效果不可控且影响访客阅读。
- 附带决策：不手动输出 og:title / twitter:title 覆盖标签。`title=false` 只压制 `<title>` 元素，og/twitter 标签仍由插件生成（首页 og:title 回退为品牌名「SwitchLM」，社交卡片标准做法）；手写副本反而造成 meta 重复（插件输出 `property="twitter:title"`，与手写 `name=` 版并存）。

## 改动点

1. **`index.md`** front matter：

   ```yaml
   ---
   layout: default
   title: SwitchLM
   seo_title: SwitchLM - 多编程套餐管理工具
   description: 多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠
   ---
   ```

   - `description`：`{% seo %}` 原生支持 page 级覆盖，自动成为 meta description / og:description
   - `seo_title`：自定义字段，由布局接管 `<title>`（见下）
   - `title`：显式声明页面标题，阻止 github-pages 自带的 jekyll-titles-from-headings 插件把 `page.title` 推导为正文首个标题「解决什么问题」（该推导正是旧版 og:title / twitter:title / JSON-LD headline 显示「解决什么问题」的原因）；声明后这些 meta 均回退品牌名「SwitchLM」。`<title>` 元素不受影响——由 `seo_title` 分支接管。注意：此行是承重的，不可省略。

2. **`_layouts/default.html`** head 中，将 `{% seo %}` 替换为：

   ```html
   {% if page.seo_title %}
     <title>{{ page.seo_title | escape }}</title>
     {% seo title=false %}
   {% else %}
     {% seo %}
   {% endif %}
   ```

   - `title=false` 是 jekyll-seo-tag 2.8.0 原生支持的参数（`Drop#title?` 匹配标签参数），只压制 `<title>` 元素输出，其余全部保留（og: 系列、canonical、JSON-LD、twitter:card 等）
   - `| escape` 防止未来标题含 `&`、`"` 等字符破坏 HTML
   - og:title / twitter:title 交由插件生成，首页回退为 `site_title`「SwitchLM」，不手写覆盖

## 不变项

- 页面可见内容零变化：header 大标题仍为 `site.title`「SwitchLM」，副标题仍为 `site.description`
- 正文 h2「解决什么问题」保留
- 其他页面无 `seo_title`，行为不变

## 验收标准

1. `bundle exec jekyll build` 后 `_site/index.html` 满足：
   - `<title>` 标签**全文档仅一个**，内容为 `SwitchLM - 多编程套餐管理工具`
   - meta description 为「多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠」
   - og:title / twitter:title 由插件输出，内容为「SwitchLM」，各仅一份，无重复 meta
2. `_site/docs/install.html` 等其他页面 `<head>` 与改动前逐字节一致（`<title>安装与快速上手 | SwitchLM</title>` 等）
3. 推送 `gh-pages` 后线上生效；在 Google Search Console 对首页请求重新编入索引以加速更新

## 已知限制

Google 不保证完全按 meta description 展示摘要（可能从正文拼凑），但正确的 `<title>` 与 description 能大幅提升命中预期展示的概率。
