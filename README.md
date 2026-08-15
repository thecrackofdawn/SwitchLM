# SwitchLM GitHub Pages

[SwitchLM](https://github.com/thecrackofdawn/SwitchLM) 产品官网源码，基于 Jekyll + [Cayman](https://github.com/pages-themes/cayman) 主题构建，由 GitHub Pages 自动部署。

线上地址：`https://thecrackofdawn.github.io/SwitchLM/`

## 站点结构

```
_config.yml                 # 站点配置（标题、baseurl、主题、exclude）
index.md                    # 首页（落地页）
docs.md                     # 文档导航页
docs/
  install.md                # 安装与快速上手
  agents.md                 # 接入编程 Agent
  faq.md                    # 常见问题
_layouts/
  default.html              # 基础布局（页头、导航、Hero）
  docs.html                 # 文档布局（窄栏 + 上一页/下一页）
_includes/
  nav.html                  # 顶部导航
  lightbox.html             # 截图灯箱（原生 <dialog>）
  docs-pager.html           # 文档分页组件
assets/
  css/style.scss            # 定制样式（@import Cayman 后覆盖）
  app-icon.svg              # 站点图标
  screenshots/              # 截图（首页画廊 + 安装指导）
```

## 本地运行环境搭建

本地预览需要 Ruby + Jekyll。无需本地环境也可以直接推送 `gh-pages` 分支，由 GitHub Pages 服务端构建。

### Windows

1. 安装 [RubyInstaller](https://rubyinstaller.org/)（带 Devkit 的 Ruby+Devkit 版本，如 Ruby 3.1+），安装时勾选 `Add Ruby executables to your PATH` 和 `MSYS2 development toolchain`
2. 打开新的终端，验证：

   ```powershell
   ruby -v
   ```

3. 安装依赖并启动：

   ```powershell
   bundle install
   bundle exec jekyll serve
   ```

4. 浏览器打开 `http://127.0.0.1:4000/SwitchLM/`（注意带 baseurl `/SwitchLM`）

### Linux / macOS

1. 安装 Ruby ≥ 2.7（建议用系统包管理器或 [rbenv](https://github.com/rbenv/rbenv)；macOS 自带的 Ruby 版本过旧，不建议直接使用）

   ```bash
   # Debian/Ubuntu 示例
   sudo apt install -y ruby-full build-essential
   ```

2. 安装依赖并启动：

   ```bash
   bundle install
   bundle exec jekyll serve
   ```

3. 浏览器打开 `http://127.0.0.1:4000/SwitchLM/`

### 常见问题

- **`bundle install` 很慢或失败**：换国内镜像源（如 [Ruby China](https://gems.ruby-china.com/)）：

  ```bash
  bundle config mirror.https://rubygems.org https://gems.ruby-china.com
  ```

- **Gemfile.lock 不存在是正常的**：`Gemfile` 只声明 `github-pages` 这一个依赖，`bundle install` 会生成 `Gemfile.lock`（已在 `.gitignore` 中忽略，不需要提交）。

- **改了 `_config.yml` 不生效**：`jekyll serve` 不监听配置变化，重启服务即可。

## 修改与发布

- 编辑 Markdown / 布局 / 样式后，`jekyll serve` 会热刷新预览
- 截图等资源来自主仓库 `assets/`，主仓库更新截图后需手动拷贝过来
- 提交并推送到 `gh-pages` 分支即自动发布：

  ```bash
  git add -A
  git commit -m "Update site content"
  git push origin gh-pages
  ```

## 注意事项

- 所有站内链接和资源路径必须使用 `{{ ... | relative_url }}`（站点部署在 `/SwitchLM` 子路径下，裸 `/` 链接会 404）
- 保持零外部依赖：不要引入 CDN 脚本、第三方 JS 库或网络字体
- `docs/superpowers/` 是内部规格/计划文档，已在 `_config.yml` 的 `exclude` 中排除，不会构建进站点
