# Tray「账户用量」hover submenu — design

**Date:** 2026-07-31
**Status:** Approved (brainstormed)
**Scope:** Single-file change to `src-tauri/src/tray.rs` (+ optional dead-code cleanup in `src/App.vue`).

## Goal

The tray's **账户用量** entry currently opens the main window on click and emits `navigate → "usage"`. Make it behave like **路由** instead: hovering expands a native submenu that lists each account's usage rate. The rows are read-only (click does nothing); there is no separate "open usage page" entry. The 套餐用量 page stays reachable via left-click tray icon → main-window nav.

## Current state (for reference)

`tray.rs` builds the menu from a pure `TrayMenuSpec`:

- **路由** is a `Submenu` → per-Profile `Submenu` → `CheckMenuItem` rows (model label includes provider + quota % + ❄ cooling).
- **账户用量** is a leaf `MenuItem` (id `"usage"`); `on_menu_event` shows the window and emits `navigate` → `"usage"`.
- separator, **退出**.

`refresh_tray_menu` already calls `gather_usage() -> HashMap<provider_id, UsageSnapshot>` (60s-cached) on the existing periodic rebuild. `usage_display(&UsageSnapshot) -> Option<String>` already formats:

- plan billing (智谱/火山) → ` {pct}%` (e.g. `" 80%"`)
- consumption billing (DeepSeek) → ` {unit} {remaining}` (e.g. `" CNY 48.77"`)
- no concrete value → `None`.

## Menu structure (before → after)

```
Before                          After
路由 ▶                          路由 ▶
账户用量   (leaf; click→nav)    账户用量 ▶   (submenu)
───                                ├ 智谱  80%
退出                                ├ 火山  45%
                                    └ DS工作号  CNY 48.77
                                ───
                                退出
```

## Row visibility & format

- One row per **Provider (account)**, in `cfg.providers` order.
- A row appears **iff** it has a concrete usage value — i.e. `usage_display(&snap).is_some()`:
  - plan → ` 80%`; consumption → ` CNY 48.77`.
- Accounts with no usage data are **hidden**. "No usage data" = the provider is absent from the gathered map (custom vendor with no adapter / missing or invalid usage creds / query failed) **or** present with a snapshot that yields no `usage_display` (e.g. a raw-summary fallback with no `used`/`remaining`). Both collapse to the same "no row" outcome.
- Label format: `{display_name}[ [vendor]]{usage_display}`.
  - The `[vendor]` tag is appended **only** when the vendor has ≥2 providers — reusing the exact `multi` vendor-count logic already in `tray_menu_spec` for 路由 disambiguation. Single-account vendors stay clutter-free.
- Degenerate case: when the accounts list is empty (no provider has usage data, e.g. fresh install / all queries failing), the submenu contains a single **disabled** placeholder row `(无用量数据)` so hovering never shows an empty box.

## Data flow

No new timer, no new gather, no new IPC. `refresh_tray_menu` already produces the per-provider usage map; the accounts list is derived from it plus `cfg.providers`. It refreshes on the existing periodic rebuild (and after a backing switch).

## Code changes (`src-tauri/src/tray.rs`)

1. **Spec types.** `TrayMenuSpec` gains `pub accounts: Vec<AccountUsageSpec>`. New struct:
   ```rust
   #[derive(Debug, PartialEq)]
   pub struct AccountUsageSpec {
       pub provider_id: String,
       /// Display label, e.g. "智谱 80%" / "DS工作号 CNY 48.77" / "火山号A [volcengine] 45%".
       pub label: String,
   }
   ```
2. **Pure helper.**
   ```rust
   fn account_label(display_name: &str, vendor_tag: Option<&str>, u: &UsageSnapshot) -> Option<String> {
       let display = usage_display(u)?; // None ⇒ no row (account hidden)
       let mut s = display_name.to_string();
       if let Some(v) = vendor_tag {
           s.push_str(&format!(" [{v}]"));
       }
       s.push_str(&display);
       Some(s)
   }
   ```
3. **`tray_menu_spec`** builds `accounts` by iterating `cfg.providers` (reusing the existing `multi` `HashSet` + vendor-tag derivation), looking up `usage.get(&p.id)`, and keeping only `Some(label)` results.
4. **`build_menu`** replaces the leaf `MenuItem "usage"` with `Submenu::new(app, "账户用量", true)?`. Then:
   - if `spec.accounts` is non-empty: append each as a plain `MenuItem::with_id(app, format!("acct:{provider_id}"), label, true, None::<&str>)?`;
   - if empty: append a single disabled `MenuItem::with_id(app, "usage_empty", "(无用量数据)", false, None::<&str>)?`.
5. **`on_menu_event`** drops the `"usage" => { show_main_window; emit navigate }` arm. The `acct:` ids have no handler; the existing `other =>` branch only `strip_prefix(BACKING_PREFIX)`, so `acct:` ids fall through and are no-ops. (Left-click tray icon still opens the main window via `on_tray_icon_event`.)

## Menu-id namespaces

Existing: `backing:{profile_id}:{model_id}`. New: `acct:{provider_id}` and the literal `usage_empty`. The three are disjoint; none collide with `"quit"`. `acct:`/`usage_empty` carry no handler.

## Optional cleanup

`src/App.vue` registers `listen<string>("navigate", …)`. After this change the tray no longer emits `"navigate"`, so that listener is dead code. It is harmless; removal is optional and out of the core scope (the frontend nav menu still works independently).

## Testing

Pure-spec unit tests in `tray.rs` `#[cfg(test)]` (the existing `snap()` / `mk_provider` helpers are reused):

- plan account → row label ends ` 80%`.
- consumption account → row label ends ` CNY 48.77`.
- account with no snapshot → **not** present in `spec.accounts`.
- snapshot present but no `used`/`remaining` → not present.
- multi-account vendor → `[vendor]` tag in label; single-account vendor → no `[`.
- `cfg.providers` order preserved in `spec.accounts`.
- empty providers / no usage data → `spec.accounts.is_empty()` (build_menu renders the placeholder — covered by spec emptiness).

No existing test asserts full `TrayMenuSpec` equality, so adding the `accounts` field is backward-compatible. Tests construct `tray_menu_spec(...)` and index `.profiles`, which remain valid.

Backend verification command:
```
cargo test --manifest-path src-tauri/Cargo.toml tray
```

## Non-goals

- No tier breakdown (5h / weekly / monthly) in the tray — that lives on the 套餐用量 page; the tray shows the single primary value, consistent with 路由.
- No cooling markers here — cooling is model-level and already shown in 路由; usage is account-level.
- No tray-side "open usage page" entry (explicitly dropped per design decision).
