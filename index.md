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

<p class="theme-credit">本站基于 <a href="https://github.com/pages-themes/cayman">Cayman</a> 主题构建（作者 <a href="https://github.com/jasonlong">Jason Long</a>，CC0 协议）</p>
