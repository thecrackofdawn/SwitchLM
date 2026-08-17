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

选定方案 A：自定义 `seo_title` front matter 字段 + 布局后写覆盖。

- 被否的备选：B（用 `page.title`，语义混乱且依赖插件行为）；C（改正文标题，效果不可控且影响访客阅读）。

## 改动点

1. **`index.md`** front matter：

   ```yaml
   ---
   layout: default
   seo_title: SwitchLM - 多编程套餐管理工具
   description: 多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠
   ---
   ```

   - `description`：`{% seo %}` 原生支持 page 级覆盖，自动成为 meta description / og:description
   - `seo_title`：自定义字段，需布局配合（见下）

2. **`_layouts/default.html`** head 中，`{% seo %}` 之后追加：

   - `{% if page.seo_title %}` 时输出 `<title>{{ page.seo_title }}</title>`（后写的 title 标签生效）
   - 同时输出 `<meta property="og:title" content="{{ page.seo_title }}">` 与 `<meta property="twitter:title" content="{{ page.seo_title }}">` 覆盖同名 meta

   采用「后写覆盖」而非移除 `{% seo %}`，保留其全部其余输出（canonical、og:url、JSON-LD、twitter:card 等）。

## 不变项

- 页面可见内容零变化：header 大标题仍为 `site.title`「SwitchLM」，副标题仍为 `site.description`
- 正文 h2「解决什么问题」保留
- 其他页面无 `seo_title`，行为不变

## 验收标准

1. `bundle exec jekyll build` 后 `_site/index.html` 的 `<title>` 为 `SwitchLM - 多编程套餐管理工具`
2. 首页 meta description 为「多厂商编程套餐管理，按时间与优先级自动切换模型分发策略，最大化利用多套餐优惠」
3. `_site/docs/install.html` 等其他页面 `<title>` 与改动前一致
4. 推送 `gh-pages` 后线上生效；在 Google Search Console 对首页请求重新编入索引以加速更新

## 已知限制

Google 不保证完全按 meta description 展示摘要（可能从正文拼凑），但正确的 `<title>` 与 description 能大幅提升命中预期展示的概率。
