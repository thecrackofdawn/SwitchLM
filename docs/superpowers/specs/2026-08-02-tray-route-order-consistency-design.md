# Tray route-order consistency — design

**Date:** 2026-08-02
**Status:** Approved (brainstormed)
**Scope:** Backend `config/types.rs`, `commands.rs`, `tray.rs`, `lib.rs`; frontend `lib/commands.ts`, `stores/config.ts`, `views/Profiles.vue`, `views/Dashboard.vue`.
**Mirrors:** `2026-07-31-usage-order-and-tray-plan-usage-design.md` — that spec unified the *account* order across the 套餐用量 tab, 概览 chips, and tray, and explicitly deferred *this* work as a non-goal ("No change to the 路由 submenu ordering … The profiles/models drag-orders stay localStorage-only"). This spec closes that gap for routes.

## Goal

Make the **route list order** identical across all three surfaces that show it — the 路由 tab, the 概览 路由 card, and the tray 路由 submenu — by giving them one shared, user-editable, persisted order. Today the two pages already agree (they share a localStorage key); only the tray diverges.

## Current state (the divergence this resolves)

| Surface | Route order today |
|---|---|
| 路由 tab (`Profiles.vue:27`) | `useOrdered("switchlm:order:profiles", …)` — **localStorage drag-order** |
| 概览 路由 card (`Dashboard.vue:34`) | `useOrdered("switchlm:order:profiles", …)` — **same** localStorage key (shared key keeps the two pages consistent) |
| Tray 路由 submenu (`tray.rs:234-257`) | raw `cfg.profiles` **config order** — diverges from the pages |

**The hard constraint (identical to `usage_order`):** the pages' order lives in **frontend localStorage** (`switchlm:order:profiles`), but the tray menu is built in the **Rust backend**, which cannot read localStorage. The tray also rebuilds from `AppState` config on a periodic timer and after every backing switch, so any order the tray honors must live in config to survive those rebuilds. A shared order therefore must be a persisted backend field.

## Design

### 1. Shared order source: `AppConfig.route_order`

Add a top-level field to `AppConfig` (`config/types.rs`), sibling to `providers`/`models`/`profiles`/`usage_order` (the sibling `usage_order` says "most-significant first" because accounts rank by importance; routes only have a display position, so the wording is "first-displayed first"):

```rust
/// User's drag-order for the route list (profile ids, first-displayed first).
/// Empty = fall back to `profiles` config order. Shared by 路由 tab, 概览 路由 card, tray.
#[serde(default)]
pub route_order: Vec<String>,
```

Update `Default::default()` to `route_order: vec![]` (and the `example()` fixture at `types.rs:215`). Empty = "no preference" → fall back to `cfg.profiles` order everywhere.

**Ordering semantics** (used by all three surfaces): rank each profile by its index in `route_order`; profiles **not listed** sink to the end, preserving their `cfg.profiles` relative order (stable sort). This mirrors `useOrdered`'s "unknown ids sink to end" rule (`useOrdered.ts:28-31`) and the `usage_order` rule, so the order is robust to routes being added/removed without rewriting the list.

**Migration (one-time, frontend):** after the config store first loads `route_order`, if it is empty **and** localStorage key `switchlm:order:profiles` exists, parse it, call `set_route_order`, then `localStorage.removeItem("switchlm:order:profiles")`. Preserves the user's existing drag-order. The guard (`route_order` empty) makes it run at most once. (Identical shape to `migrateUsageOrderIfNeeded` in `stores/config.ts:109-121`.)

**Commands** (`commands.rs`), mirroring `get_usage_order`/`set_usage_order` (`commands.rs:34-72`) exactly but keyed on profile ids:

- `get_route_order(state) -> Vec<String>` — `Ok(state.config.read().await.route_order.clone())`.
- `set_route_order(state, ids: Vec<String>, app) -> Result<(), String>` — `normalize_route_order` (filter to known profile ids, dedupe preserving first occurrence), assign `config.route_order`, `persist(&app, &config)?`, then `tray::refresh_tray_menu(&app).await` so the tray reorders immediately (not on the next timer tick).

Register both in `lib.rs` `invoke_handler`; add typed wrappers in `lib/commands.ts`. (No `lib/types.ts` change — `route_order` is a plain `string[]` fetched by `getRouteOrder()` and held as a store ref; there is no frontend `AppConfig` mirror.)

### 2. Tray honors `route_order` (`tray.rs`)

In `tray_menu_spec`, the `profiles` vec is currently built straight from `cfg.profiles.iter().map(…).collect()` (`tray.rs:234-257`). After collecting, sort it by `route_order` rank — the same pattern already used for the accounts section at `tray.rs:271-272`:

```rust
profiles.sort_by_key(|p| cfg.route_order.iter().position(|id| id == &p.profile_id).unwrap_or(usize::MAX));
```

`ProfileMenuSpec` already carries `profile_id`, so no struct change is needed. Stable sort → ties (unlisted profiles) keep config order. Empty `route_order` → every profile ranks `usize::MAX` → pure config order (no change from today).

### 3. 路由 tab (`Profiles.vue`)

Replace `useOrdered("switchlm:order:profiles", …)` (`Profiles.vue:27`) with a config-backed reorder, mirroring the 套餐用量 tab pattern from the `usage_order` spec:

- a local `ordered` ref (keeps vuedraggable's drag UX instant);
- initialize and re-sort it from the store's `profilesOrdered` computed (§5), watched;
- on drag `@end`, **debounce** the commit (~400 ms trailing) — rapid successive drags coalesce into one `set_route_order` call. The local `ordered` ref updates instantly for a responsive UI; only the persisted call is debounced. Rationale (same as `usage_order`): each `set_route_order` rebuilds the native tray menu, so debouncing avoids menu flicker / CPU spikes during fast reordering. Debounce lives in `Profiles.vue`'s commit handler (not the store action) so the one-time migration call (`setRouteOrder` from `loadAll`) is never skipped or delayed. The store re-fetches `routeOrder` per the canonical "re-fetch after mutate" pattern.

### 4. 概览 路由 card (`Dashboard.vue`)

Drop the `useOrdered("switchlm:order:profiles", …)` call (`Dashboard.vue:34`) — that key is migrated away in §1, so it must not be read anymore. Render the route card from the store's `profilesOrdered` computed (§5) instead. The card is read-only (no drag), so it just consumes the sorted list.

### 5. Shared sort: store `profilesOrdered` computed

Add a `profilesOrdered` computed to `stores/config.ts`: `config.profiles` sorted by `config.routeOrder` rank (unlisted sink to end, stable) — the single implementation of the §1 ordering semantics on the frontend. Dashboard (§4) consumes it directly; Profiles.vue (§3) seeds its draggable ref from it and re-sorts when it changes. After a drag commits and `routeOrder` is re-fetched, `profilesOrdered` recomputes to match what the user just dragged (no visible jump). `Dashboard.vue`'s old `useOrdered` import is removed.

## Backend code changes

- **`config/types.rs`**: `AppConfig.route_order` (`#[serde(default)]`) + `Default` + `example()` fixture.
- **`commands.rs`**: `normalize_route_order`, `assign_route_order` (known ids = `config.profiles` ids), `get_route_order`, `set_route_order` (validate/dedupe/persist/refresh-tray).
- **`lib.rs`**: register `get_route_order`, `set_route_order` in `invoke_handler`.
- **`tray.rs`**: sort `tray_menu_spec`'s `profiles` by `cfg.route_order` rank (one line, mirroring `tray.rs:271-272`).

## Frontend code changes

- **`lib/commands.ts`**: `getRouteOrder`, `setRouteOrder` typed wrappers.
- **`stores/config.ts`**: `routeOrder` ref; fetch in `loadAll`; `profilesOrdered` computed; `setRouteOrder(ids)` action; one-time migration `migrateRouteOrderIfNeeded` (guarded by `routeOrder.length === 0 && localStorage present`).
- **`views/Profiles.vue`**: config-backed reorder replacing `useOrdered`; debounced `setRouteOrder` on drag end.
- **`views/Dashboard.vue`**: render route card from `config.profilesOrdered`; remove the `useOrdered` import/call.

## Testing

**Backend (`cargo test --manifest-path src-tauri/Cargo.toml tray` and `commands`):**
- `tray_menu_spec.profiles` ordered by `route_order` (listed profile ids in rank order; unlisted appended in `cfg.profiles` order); empty `route_order` → config order.
- `normalize_route_order` filters unknown ids and dedupes (first wins); all-unknown → empty.
- `assign_route_order` writes the normalized result to `config.route_order`.
- `set_route_order`: unknown ids filtered, dupes collapsed, persisted, tray refreshed (persist verified via `MemoryStore`/temp config).
- `route_order` serde: `#[serde(default)]` — old config without the field deserializes to `[]`; round-trips.

**Frontend (`npm run build` / vue-tsc):**
- `profilesOrdered` orders by `routeOrder` (unlisted sink to end); empty `routeOrder` → config order.
- Profiles.vue / Dashboard.vue type-check with the new store API and no `useOrdered` usage.

## Non-goals

- **Per-route backing-model options** (the model list inside each tray route submenu) stay in config order — no page surface reorders them, so there is no inconsistency to fix.
- The Models tab's own localStorage drag-order (`switchlm:order:models`) stays localStorage-only — it has no tray surface.
- No drag interaction is added to the 概览 card (read-only).
- No change to route resolution, fallback, or any proxy behavior — this is display order only.

## Verification

```
cargo test --manifest-path src-tauri/Cargo.toml tray
cargo test --manifest-path src-tauri/Cargo.toml route_order
npm run build          # vue-tsc + vite
npm run tauri dev      # manual: drag a route in the 路由 tab → tray 路由 submenu + 概览 card follow (debounced), survive a tray rebuild / backing switch
```
