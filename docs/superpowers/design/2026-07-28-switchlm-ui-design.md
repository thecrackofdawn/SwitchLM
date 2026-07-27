# SwitchLM UI Design — "Instrument Panel"

> Produced via the `frontend-design` skill (Plan 4 Task 7). Reference for the page builds (Tasks 8–13).

## Direction

SwitchLM is a proxy developers **configure once and glance at**: "am I routed? on what model? is anything cooling? what quota's left?" Its world is identifiers, ports, percentages, and signal states. So the UI is a **precise, data-dense instrument panel** — not a marketing page. Deliberate, subject-specific choices (avoiding the AI defaults: no cream+serif, no acid-green-on-black, no broadsheet columns):

- **Typography** — IBM Plex Sans (UI) + **IBM Plex Mono for all data** (model ids, ports, urls, quotas, percentages). Plex was designed for engineering/instrument contexts; the clipped, technical character fits. CJK falls through to Microsoft YaHei UI / PingFang SC.
- **Palette** — cool "instrument" surfaces (slightly blue-grey, not warm cream). **Cobalt `#3B5BDB`** primary (live-signal, not default Tailwind blue). Cooling = amber, tripped/error = red, healthy = quiet grey. Both light + dark; dark auto-follows the OS.
- **Signature (the one justified risk)** — the **route line**: `agent ▶ Profile ▶ Model ▶ Provider`, visualizing the two-level model abstraction that *is* SwitchLM. Cooling ❄ sits on the Model chip; quota % on the Provider chip. Lives on the Dashboard (and inspires the tray label).

Tokens live in `src/styles/tokens.css` (CSS vars, light root + `@media prefers-color-scheme: dark`); Naive UI consumes the same values via `src/styles/theme.ts` (`GlobalThemeOverrides`, light + dark).

## Tokens (summary)

| token | light | dark | role |
|---|---|---|---|
| `--sl-paper` | #F5F6F8 | #0E1116 | app background |
| `--sl-panel` | #FFFFFF | #161B22 | cards / elevated |
| `--sl-panel-2` | #FAFBFC | #1C232C | recessed surface |
| `--sl-ink` | #1B1F24 | #E6EDF3 | primary text |
| `--sl-text-2` / `--sl-text-3` | #5C6370 / #8B949E | #9AA6B2 / #6B7785 | muted / faint |
| `--sl-line` | #E1E4E8 | #30363D | hairline border |
| `--sl-accent` | #3B5BDB | #5B7CFA | cobalt — routed / primary |
| `--sl-cool` | #D97706 | #F0B429 | cooling amber |
| `--sl-trip` | #DC2626 | #FF6B6B | tripped / error |
| `--sl-ok` | #1F9D63 | #3FD17F | healthy (sparingly) |
| `--sl-radius` | 8px | 8px | card radius (small 5px) |

## Shell

Fixed left **sider** (SwitchLM mark + `n-menu` nav) + **content** (slim header with page title + contextual actions; padded body). `n-config-provider` supplies the theme + dark auto-detect (listens to `prefers-color-scheme`).

---

## Page wireframes

### 1. Dashboard 概览
```
┌ 概览 ───────────────────────────────────────────────┐
│  [agent ▶ glm-5.2 ▶ GLM-4.6 ❄ ▶ 智谱 80%]   ← RouteLine │
│                                                       │
│  ┌运行状态─┐  ┌实际端口─────┐  ┌套餐额度─┐  [复制环境变量] │
│  │ ● 运行  │  │ 6950  mono  │  │ 80%    │                │
│  └─────────┘  └─────────────┘  └────────┘                │
│  ⚠ 端口与设置不符时: 红字警告 + 提示重启                   │
│                                                       │
│  快速切换   Profile [glm-5.2 ▾]  Model [GLM-4.5 ▾] [应用] │
└───────────────────────────────────────────────────────┘
```
Data: `getServerStatus`, `getPort`, `getEnvSnippet`, `get_all_usage`, `setProfileBacking`. Port-warning when actual ≠ settings.port.

### 2. Provider 管理
```
┌ Provider 管理 ──────────────────────────────┐
│  [+ 新增 provider]                            │
│  ┌─────────────────────────────────────────┐ │
│  │ 智谱   base_url …v4   [连接测试] [编辑] [删除] │ │
│  │ 火山   base_url …      [连接测试] [编辑] [删除] │ │
│  └─────────────────────────────────────────┘ │
│  编辑抽屉: id · display_name · base_url · api_key │
│            · usage_creds (AK/SK 可选)         │
└───────────────────────────────────────────────┘
```
Data: providers CRUD, `testProviderConnection`, `discoverModels`.

### 3. 真实模型管理 (Models)
```
┌ 真实模型管理 ────────────────────────────────────┐
│  [发现模型…]  provider [智谱 ▾] → 勾选 → [批量加入]    │
│  ┌────────────────────────────────────────────┐ │
│  │ GLM-4.6   zhipu · manual   openai✓ anthropic○  │ │
│  │   cooldown 300s · fallback→ m_glm45   [编辑][删除]│ │
│  └────────────────────────────────────────────┘ │
│  编辑: display_name · provider · 双后端 base/upstream_id │
│        · cooldown_seconds                        │
└──────────────────────────────────────────────────┘
```

### 4. 自定义模型 (Profile)
```
┌ 自定义模型 ─────────────────────────────────────┐
│  [+ 新增 Profile]                                 │
│  glm-5.2   接入: GLM-4.6   aliases: claude-sonnet-4  [编辑][删除] │
│  fast      接入: Doubao     aliases: —          [编辑][删除] │
│  编辑: name · aliases[] · backing_model_id [▾]   │
└──────────────────────────────────────────────────┘
```

### 5. Fallback 配置
```
┌ Fallback 配置 ──────────────────────────┐
│  GLM-4.6   →  [GLM-4.5 ▾]   (留空 = 无)   │
│  GLM-4.5   →  [无 ▾]                    │
│  Doubao    →  [GLM-4.6 ▾]               │
│  注: 设定 fallback 会重置该模型熔断状态 (§4.1) │
└──────────────────────────────────────────┘
```

### 6. 用量查看
```
┌ 用量查看 ───────────────────────────────────┐
│  智谱    ████████░░  80%   重置 2026-08-01   │
│  火山    ████░░░░░░  40%   (不支持 / 错误信息) │
│  [刷新] (60s 缓存)                            │
└──────────────────────────────────────────────┘
```

### 7. 设置
```
┌ 设置 ───────────────────────────────────┐
│  端口        [6950]   (实际: 6950)         │
│  开机自启    [ 〇 ]                         │
│  [重启服务]   [退出 SwitchLM]              │
└──────────────────────────────────────────┘
```

## Copy voice

Plain verbs, sentence case, active voice, Chinese. Controls name the outcome ("保存修改", "重启服务", "退出 SwitchLM"). Errors state what happened + how to fix, no apologies. Empty states invite action. Numbers/ids always mono.
