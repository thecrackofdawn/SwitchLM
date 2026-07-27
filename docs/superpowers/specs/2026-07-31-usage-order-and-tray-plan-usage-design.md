# Unified usage order & tray plan-usage — design

**Date:** 2026-07-31
**Status:** Approved (brainstormed)
**Scope:** Backend `config/types.rs`, `commands.rs`, `tray.rs`, `lib.rs`; frontend `lib/types.ts`, `lib/commands.ts`, `stores/config.ts`, `views/Usage.vue`, `views/Dashboard.vue`, `lib/quotaUtils.ts`.
**Supersedes:** the "No tier breakdown in the tray" non-goal of `2026-07-31-tray-account-usage-submenu-design.md` (§Non-goals) — the tray now shows the 5h/周/月 tier breakdown.

## Goal

Make usage display **consistent across three surfaces** — the 概览「可用额度」chips, the 套餐用量 tab, and the tray「账户用量」submenu — by giving them **one shared, user-editable account order**, and make the tray reflect the same 套餐用量 (plan-usage) detail as the tab. Additionally, surface a 套餐用量 summary on the tray-icon hover tooltip (today a static `"SwitchLM"`).

Concretely:

1. **One order source.** The user's drag-order from the 套餐用量 tab becomes a persisted backend field (`AppConfig.usage_order`) that all three surfaces read.
2. **Tray rows show plan usage.** Each tray account row shows the three tier windows (5h / 周 / 月) of used %, matching the 套餐用量 tab, instead of a single primary %.
3. **Tray-icon tooltip shows 套餐用量.** Hovering the tray icon shows a compact per-account usage summary instead of the static app name.

## Current state (the divergences this resolves)

| Surface | Order today | Plan-usage value today |
|---|---|---|
| 概览「可用额度」chips (`Dashboard.vue` ← `quotaUtils.ts:deriveQuotaChips`) | **urgency sort**: plan chips by `remaining` ascending (red first), then consumption, then failed (`quotaUtils.ts:146`) | `remaining` % (`可用 N%`) |
| 套餐用量 tab (`Usage.vue`) | `useOrdered("switchlm:order:usage", …)` — **localStorage drag-order**, defaulting to config order | `used_pct` per tier (5h/周/月) + reset time |
| Tray「账户用量」rows (`tray.rs`) | raw `cfg.providers` order (`tray.rs:161-169`) | single primary `used` % via `quota_pct` (`tray.rs:62-67`) |
| Tray-icon tooltip (`tray.rs:228`) | — | static `"SwitchLM"`, never updated |

**The hard constraint:** the 套餐用量 tab's order lives in **frontend localStorage** (`switchlm:order:usage`), but the tray menu is built in the **Rust backend**, which cannot read localStorage. (Today the tray doesn't honor the tab order at all — like its 路由 submenu, it uses config order.) So a shared order must live in the backend.

`UsageSnapshot` already carries everything needed (`usage/mod.rs:22-49`): `billing_model` (`"plan"` | `"consumption"`), `tiers: Vec<UsageTier>` (`window` ∈ `five_hour` / `weekly_limit` / `monthly`, each `used_pct` + `reset_at`), `remaining`, `unit`. Tray and frontend share the same 60s-cached `query_usage` pipeline.

## Design

### 1. Shared order source: `AppConfig.usage_order`

Add a top-level field to `AppConfig` (`config/types.rs`), sibling to `providers`/`models`/`profiles`:

```rust
#[serde(default)]
pub usage_order: Vec<String>, // provider ids, most-significant first
```

Update `Default::default()` to `usage_order: vec![]`. Empty = "no preference" → fall back to `cfg.providers` order everywhere.

**Ordering semantics** (used by all three surfaces): rank each provider by its index in `usage_order`; providers **not listed** sink to the end, preserving their `cfg.providers` relative order. This mirrors `useOrdered`'s "unknown ids sink to end" rule (`useOrdered.ts:28-31`) and makes the order robust to providers being added/removed without rewriting the list.

**Migration (one-time, frontend):** after the config store first loads `usage_order`, if it is empty **and** localStorage key `switchlm:order:usage` exists, parse it, call `set_usage_order`, then `localStorage.removeItem("switchlm:order:usage")`. Preserves the user's existing drag-order. The guard (`usage_order` empty) makes it run at most once.

**Commands** (`commands.rs`), mirroring the existing `set_usage_refresh_interval` shape (`state.config.write().await` → mutate → `persist(&app, &config)`):

- `get_usage_order(state) -> Vec<String>` — `Ok(state.config.read().await.usage_order.clone())`.
- `set_usage_order(state, ids: Vec<String>, app) -> Result<(), String>` — filter `ids` to known provider ids, dedupe preserving first occurrence, assign `config.usage_order`, `persist`, then `tray::refresh_tray_menu(&app).await` so the tray reorders immediately (not after the 15s tick).

Register both in `lib.rs` `invoke_handler`; add typed wrappers in `lib/commands.ts` and the field to `lib/types.ts`.

### 2. Surface changes

| Surface | Display after change | Order after change |
|---|---|---|
| 概览「可用额度」chips | **unchanged** (`remaining` %, urgency color) | `usage_order` (urgency **sort dropped**; color still signals urgency) |
| 套餐用量 tab | **unchanged** | read/write `usage_order` (+ migration) |
| Tray「账户用量」rows | **three-tier used %** (§3) | `usage_order` |
| Tray-icon tooltip | **new: 套餐用量 summary** (§4) | `usage_order` |

**Intentional asymmetry (approved):** the overview chips keep showing `remaining` % (their label is 「可用额度」= *available*), while the tray shows `used` % (套餐用量口径, matching the tab). The three surfaces agree on **order**; the overview's metric stays "available" by its name.

### 3. Tray row format (three-tier plan usage)

For a **plan** account, the row shows all three tier windows in fixed order, each rounded to an integer:

```
{display_name}[ [vendor]] 5h:80% 周:40% 月:10%
```

- Window → short label: `five_hour` → `5h`, `weekly_limit` → `周`, `monthly` → `月`.
- For each window: look up `snapshot.tiers.iter().find(|t| t.window == WIN)`; if present with `used_pct.is_some()` → `{label}:{pct}%` (rounded to nearest int); else → `{label}:–`.
- Join the three with single spaces.
- Full label: `{display_name}` + optional ` [vendor]` tag (the existing `multi`-vendor rule, unchanged) + ` ` + tier string.
- Example: `火山号A [volcengine] 5h:45% 周:30% 月:10%`.

For a **consumption** account: unchanged — `{display_name}[ [vendor]] {unit} {remaining}` (e.g. `DS工作号 CNY 48.77`).

**Visibility** (unchanged rule, extended to tiers): a row appears iff it has a concrete value —
- plan: **at least one** of the three windows has a `used_pct` (a snapshot whose tiers are all empty/`None`, e.g. a raw-summary fallback, yields `5h:– 周:– 月:–` and is hidden);
- consumption: `remaining.is_some()`.

When no account has usage data, the submenu keeps its existing disabled placeholder `(无用量数据)`.

Reset times are **not** shown in the tray (they live only on the 套餐用量 tab) — the three-percent row is already long enough.

The submenu title「账户用量」is renamed to「套餐用量」for consistency with the tab (consumption accounts sit under the same title on the tab today).

### 4. Tray-icon tooltip (套餐用量 summary)

In `refresh_tray_menu`, alongside `tray.set_menu(...)`, call `tray.set_tooltip(summary)` with a pure, unit-tested summary built from the gathered usage map + `usage_order`. It replaces the static `"SwitchLM"` set in `build_tray`.

Content — one compact entry per **visible** account (same visibility rule as rows), using the **primary 5h** value:
- plan: `snapshot.tiers` `five_hour.used_pct` rounded to int → `{name} {pct}%` (falls back to top-level `used` if 5h tier absent);
- consumption: `{name} {unit} {remaining}` (e.g. `DeepSeek CNY 48.77`).

(`{name}` = `display_name` only; the `[vendor]` tag is omitted in the tooltip for brevity — names are user-set and usually distinct.) Ordered by `usage_order`. When no account has usage data, the tooltip stays `"SwitchLM"` (no empty popup).

**Length cap (mandatory, all platforms).** Windows classic tooltips cap near 128 chars, and the limit is on the buffer itself — so it binds multiline too. `usage_tooltip` greedily adds **whole** account units (never cutting mid-name) while the total stays ≤ 120 chars; if accounts remain, it appends `…(N more)` (N = remaining count). Zero accounts → `"SwitchLM"`. This cap is enforced on the final string regardless of format.

**Platform risk — verify first (implementation step 1).** Windows classic `szTip` has limited/no multiline rendering. Two formats are designed; pick based on what Tauri v2 `set_tooltip` actually renders on Windows:
- **preferred where supported (multiline):** one account per line, `\n`-joined:
  ```
  智谱 80%
  火山号A 45%
  DeepSeek CNY 48.77
  ```
- **fallback (single line):** ` · `-joined:
  ```
  智谱 80% · 火山号A 45% · DeepSeek CNY 48.77
  ```

In both cases the length cap above is enforced on the final string.

(A richer custom flyout window on hover is explicitly out of scope — native tooltip only.)

### 5. Overview chips (`Dashboard.vue` / `quotaUtils.ts`)

`deriveQuotaChips` gains an `order: string[]` argument and sorts the chips by `usage_order` rank (unlisted sink to end, stable) **instead of** the current urgency `sortKey`. Chip **display and color logic are untouched** — a chip still shows `remaining` % and goes red/amber/green by urgency; only its **position** now follows the user's order. `Dashboard.vue` passes `config.usageOrder`.

### 6. 套餐用量 tab (`Usage.vue`)

Replace `useOrdered("switchlm:order:usage", …)` with a config-backed reorder:
- a local `ordered` ref (keeps vuedraggable's drag UX instant);
- initialize and re-sort it from `config.usageOrder` (watched) + `cards`;
- on drag `@end`, **debounce** the commit (~400 ms trailing) — rapid successive drags coalesce into one `setUsageOrder` call. The local `ordered` ref updates instantly for a responsive UI; only the persisted call is debounced. Rationale: each `set_usage_order` rebuilds the native tray menu (and recomputes the tooltip), so debouncing avoids menu flicker / CPU spikes during fast reordering. Debounce lives in `Usage.vue`'s commit handler (not the store action) so the one-time migration call (`setUsageOrder` from `loadAll`) is never skipped or delayed. The store re-fetches `usageOrder` per the canonical "re-fetch after mutate" pattern.

The migration in §1 runs from the config store right after `loadAll`.

## Backend code changes

- **`config/types.rs`**: `AppConfig.usage_order` + `Default`.
- **`commands.rs`**: `get_usage_order`, `set_usage_order` (validate/dedupe/persist/refresh-tray).
- **`lib.rs`**: register the two commands in `invoke_handler`.
- **`tray.rs`** (pure helpers, TDD-style unit-tested like the existing spec tests):
  - extend `usage_display`'s plan arm (or a new `tier_summary(&UsageSnapshot) -> Option<String>`) to render `5h:% 周:% 月:%`; keep the consumption arm.
  - `usage_tooltip(accounts: &[(name, tag, snap)], order: &[String]) -> String` — the §4 summary.
  - `tray_menu_spec`: build `accounts` (still from `cfg.providers` + `multi` tag + visibility filter), then **sort by `cfg.usage_order`** rank.
  - visibility rule per §3 (plan: ≥1 tier with `used_pct`).
  - `build_menu`: submenu title `账户用量` → `套餐用量`.
  - `refresh_tray_menu`: `tray.set_tooltip(usage_tooltip(...))` after `set_menu`.

## Frontend code changes

- **`lib/commands.ts`**: `getUsageOrder`, `setUsageOrder` typed wrappers. (No `lib/types.ts` change — `usage_order` is a plain `string[]` fetched by `getUsageOrder()` and held as a store ref; there is no frontend `AppConfig` mirror.)
- **`stores/config.ts`**: `usageOrder` ref; fetch in `loadAll`; `setUsageOrder(ids)` action; one-time migration (`migrateUsageOrderIfNeeded`) guarded by `usageOrder.length === 0 && localStorage present`.
- **`views/Usage.vue`**: config-backed reorder replacing `useOrdered`.
- **`views/Dashboard.vue`** + **`lib/quotaUtils.ts`**: `deriveQuotaChips(..., order)` sorts by `usage_order`.

## Testing

**Backend (`cargo test --manifest-path src-tauri/Cargo.toml tray` and `commands`):**
- `tier_summary`: plan with all three tiers → `5h:80% 周:40% 月:10%`; plan with a missing tier → `… 月:–`; plan with no tiers → `None` (hidden); consumption → balance string; rounding to int.
- `tray_menu_spec.accounts` ordered by `usage_order` (listed ids in rank order; unlisted appended in `cfg.providers` order); empty `usage_order` → config order.
- plan snapshot with no concrete tier → row absent; consumption without `remaining` → absent.
- `usage_tooltip`: per-account primary value; ordered by `usage_order`; empty → `"SwitchLM"`; **length cap** — many accounts → result ≤ 120 chars, ends `…(N more)`, no mid-name cut; few accounts → full string, no ellipsis.
- `set_usage_order`: unknown ids filtered, dupes collapsed, persisted, tray refreshed (persist verified via `MemoryStore`/temp config).
- `usage_order` serde: `#[serde(default)]` — old config without the field deserializes to `[]`.

**Frontend (`npm run build` / vue-tsc):**
- `deriveQuotaChips` orders by `order` (urgency sort gone); unlisted sink to end.

## Non-goals

- No custom tray-hover flyout window — native tooltip only.
- No reset times in the tray (stay on the 套餐用量 tab).
- No change to the 路由 submenu ordering (it stays config order; only usage surfaces unify). The profiles/models drag-orders stay localStorage-only.
- The overview「可用额度」metric stays `remaining` (only its order changes).
- No change to usage fetching/caching (60s `UsageCache` reused).

## Verification

```
cargo test --manifest-path src-tauri/Cargo.toml tray
cargo test --manifest-path src-tauri/Cargo.toml set_usage_order
npm run build          # vue-tsc + vite
npm run tauri dev      # manual: drag a 套餐用量 card, confirm tray + overview follow; hover tray icon
```
