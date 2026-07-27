# 开发参考 (dev-reference)

SwitchLM 开发中常用的操作手册。当前为 Windows + Tauri v2 + PowerShell 环境。

---

## 在 worktree 中运行/测试应用（复用主仓库资源）

### 背景

worktree 是仓库的干净副本，**没有自己的 `node_modules` / `target` / `dist`**。直接 `npm install` + 全量 `cargo` 编译很慢（首次 5–10 分钟）。可以通过 **junction + `CARGO_TARGET_DIR`** 复用主仓库已装好的前端依赖与已编译的 Rust 依赖，只重编 `switchlm` 这个 crate 本身。

### 关键事实（dist 何时需要）

`generate_context!` 宏里 `dev = cfg!(not(feature = "custom-protocol"))`：

- **dev 模式不需要 `dist`**：`tauri dev` / `cargo test` / `cargo build`（debug）都不启用 `custom-protocol` 特性，`dev=true`；又因配置里有 `devUrl`，codegen 走空 assets 分支，**不检查 `frontendDist`**。
- **只有 `tauri build`（release，启用 `custom-protocol`）需要 `dist`**，而它的 `beforeBuildCommand: npm run build` 会自动生成 `dist/`。

所以在 worktree 里跑 dev / 测试，**无需造假 `dist`**。

### 启动 app

```powershell
# 主仓库根目录（资源来源）
$MAIN = "C:\Users\cd\Documents\projects\SwitchLM"
# 当前 worktree 根目录
cd <worktree>   # 例: C:\Users\cd\Documents\projects\SwitchLM\.claude\worktrees\bugfix

# 1. 前端依赖：junction 借主仓库的 node_modules（瞬时完成，不复制）
New-Item -ItemType Junction -Path node_modules -Target "$MAIN\node_modules"

# 2. Rust 依赖：CARGO_TARGET_DIR 指主仓库 target（23G 已编译依赖，只重编 switchlm）
$env:CARGO_TARGET_DIR = "$MAIN\src-tauri\target"

# 3. 启动 dev（dev 模式用 devUrl，不需要 dist）
npm run tauri dev
```

> vite 会 serve **worktree 的 `src/`**（含本 worktree 改动），cargo 编译 **worktree 的 `src-tauri/`**（含本 worktree 改动），所以跑的就是 worktree 的代码。

### 用完清理（⚠️ 勿用 `rm -rf` / `Remove-Item -Recurse`，会跟随 junction 删掉主仓库的 node_modules）

```powershell
# 删 junction 本身，不递归进目标
[System.IO.Directory]::Delete((Resolve-Path node_modules).Path, $false)
Remove-Item Env:\CARGO_TARGET_DIR
```

### 注意事项

1. **端口 1420 是 `strictPort: true`**（`vite.config.ts`），被占就直接失败、不自动换端口。**别同时开主仓库的 `tauri dev`**。若必须同时开：把 worktree 的 `vite.config.ts`(`server.port`) 和 `tauri.conf.json`(`build.devUrl`) 改成 1422 之类（本地改、**不要提交**）。
2. **共享 `target` 的代价**：`switchlm` crate 的产物与主仓库共用，两边切来切去会各自重编一次 `switchlm`（第三方依赖始终命中缓存）。**别在两边同时跑 cargo**（会抢 `target` 锁）。
3. **junction 期间别在 worktree 里 `npm install`**：会通过 junction 写进主仓库的 `node_modules`（同 `package.json` 一般无害，但要知道这点）。vite 的 `.vite` 缓存也共享，同源兼容。

### 备选方案

- 若 `tauri dev` 找不到产物（极少数情况下 CLI 没认 `CARGO_TARGET_DIR`），把 target 也 junction，免去环境变量：
  ```powershell
  New-Item -ItemType Junction -Path src-tauri\target -Target "$MAIN\src-tauri\target"
  ```
- 若担心共享 `target` 有风险，就**不设 `CARGO_TARGET_DIR`**，让 worktree 自建 `target`：首次全量编译 5–10 分钟，之后增量。干净但慢。

### 只跑测试 / 类型检查（不启动 app）

```powershell
$MAIN = "C:\Users\cd\Documents\projects\SwitchLM"
cd <worktree>\src-tauri

# Rust 单测（复用主仓库 target）
$env:CARGO_TARGET_DIR = "$MAIN\src-tauri\target"
cargo test --lib tray          # 跑 tray 模块测试；去掉 tray 跑全部
cargo check --lib              # 只检查编译，不产出二进制

# 前端类型检查（需先 junction node_modules）
cd <worktree>
npx vue-tsc --noEmit
```
