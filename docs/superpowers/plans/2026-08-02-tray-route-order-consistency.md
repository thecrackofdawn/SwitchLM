# Tray route-order consistency — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the route-list order identical across the 路由 tab, the 概览 路由 card, and the tray 路由 submenu by giving them one persisted backend field (`AppConfig.route_order`).

**Architecture:** A near-line-for-line mirror of the already-shipped `usage_order` feature. Add `AppConfig.route_order` (profile ids) + `get/set_route_order` commands; the tray sorts its profiles vec by it; the frontend migrates the shared `switchlm:order:profiles` localStorage key into it once, then all three surfaces read the store's `profilesOrdered` computed.

**Tech Stack:** Rust (axum/Tauri v2 backend, `#[cfg(test)] mod tests`), Vue 3 `<script setup>` + Pinia + vuedraggable + Naive UI frontend.

## Global Constraints

- **Field name:** `route_order: Vec<String>` — profile ids, **first-displayed first**. Sibling to `usage_order`.
- **Ordering semantics (all surfaces):** rank each profile by its index in `route_order`; profiles **not listed sink to the end**, preserving their `cfg.profiles` relative order (**stable** sort). Empty `route_order` → pure config order. Identical to `usage_order` / `useOrdered.ts:28-31`.
- **`#[serde(default)]`** on `route_order` — old `app_config.json` without the field must deserialize to `[]`.
- **Re-fetch after mutate** (frontend canonical pattern): every set action re-fetches the affected slice; no optimistic local mutation.
- **Drag commit is debounced ~400 ms** (each `set_route_order` rebuilds the native tray menu → coalesce rapid drags). Debounce lives in the view, not the store, so the one-time migration call from `loadAll` is never delayed.
- **Migration is one-time and guarded** by `routeOrder.length === 0 && localStorage key present`; it then `removeItem`s the key.
- **Out of scope:** per-route backing-model options inside each tray submenu stay config order; the Models tab's own `switchlm:order:models` localStorage order is untouched; no drag on the 概览 card.

**Reference spec:** `docs/superpowers/specs/2026-08-02-tray-route-order-consistency-design.md`. **Pattern source for the frontend debounced reorder:** `src/views/Usage.vue:51-89`.

---

### Task 1: `AppConfig.route_order` field + serde (TDD)

**Files:**
- Modify: `src-tauri/src/config/types.rs:10-17` (struct), `:19-29` (`Default`), `:200-217` (`example()` fixture), `:295-309` (add tests beside the `usage_order` tests).

**Interfaces:**
- Produces: `AppConfig.route_order: Vec<String>` (`#[serde(default)]`, default `vec![]`) for Tasks 2–3 to read/write.

- [ ] **Step 1: Write the failing tests**

Add these immediately after the `usage_order_round_trips` test (`types.rs:309`), mirroring `usage_order_defaults_empty_when_absent` (`:295`) and `usage_order_round_trips` (`:303`):

```rust
    #[test]
    fn route_order_defaults_empty_when_absent() {
        // An old config JSON that never had a route_order field must deserialize to [].
        let json = r#"{"providers":[],"models":[],"profiles":[],"settings":{}}"#;
        let cfg: AppConfig = serde_json::from_str(json).expect("deserialize");
        assert!(cfg.route_order.is_empty());
    }

    #[test]
    fn route_order_round_trips() {
        let mut cfg = AppConfig::default();
        cfg.route_order = vec!["p_b".into(), "p_a".into()];
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.route_order, vec!["p_b".to_string(), "p_a".to_string()]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml route_order`
Expected: FAIL to compile — `no field `route_order` on type `AppConfig``.

- [ ] **Step 3: Add the field + wire defaults**

3a. In the `AppConfig` struct (`types.rs:10-17`), add the field right after the `usage_order` block (after line 14):

```rust
    /// User's drag-order for the route list (profile ids, first-displayed first).
    /// Empty = fall back to `profiles` config order. Shared by 路由 tab, 概览 路由 card, tray.
    #[serde(default)]
    pub route_order: Vec<String>,
```

3b. In `impl Default for AppConfig` (`types.rs:21-27`), add after `usage_order: vec![],` (line 25):

```rust
            route_order: vec![],
```

3c. In the `example()` / round-trip fixture (`types.rs:215`, the line `usage_order: vec![],`), add immediately after it:

```rust
            route_order: vec![],
```

3d. Grep to confirm no other struct-literal construction of `AppConfig` was missed:
Run: `grep -rn "AppConfig {" src-tauri/src` — every hit must either be `Default::default()`/`::default()` (unaffected) or include `route_order`. Only `Default` (`:21`) and the fixture (`:200`) use full literals; both are covered above.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml route_order`
Expected: PASS (both new tests). Also run `cargo test --manifest-path src-tauri/Cargo.toml` to confirm nothing else broke from the new field.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs
git commit -m "feat(config): add AppConfig.route_order field"
```

---

### Task 2: `get_route_order` / `set_route_order` commands (TDD)

**Files:**
- Modify: `src-tauri/src/commands.rs:32-72` (helpers + commands), `:1483-1508` (tests), `src-tauri/src/lib.rs:141-142` (registration).

**Interfaces:**
- Consumes: `AppConfig.route_order` (Task 1).
- Produces: Tauri commands `get_route_order(state) -> Vec<String>` and `set_route_order(state, ids, app) -> Result<(), String>`; pure helpers `normalize_id_order`, `assign_route_order` (Task 3 tests + frontend Tasks 4–6 consume the commands).

**DRY note:** `normalize_usage_order` (`commands.rs:34-41`) is generic in everything but its name — it just filters/dedupes an id list against a known list. Rename it to `normalize_id_order` and have **both** `assign_usage_order` and the new `assign_route_order` call it, instead of duplicating the body.

- [ ] **Step 1: Write the failing test**

Add beside `assign_usage_order_filters_unknown_dedupes_and_assigns` (`commands.rs:1497`), using the existing `mk_profile` helper (`commands.rs:1293`):

```rust
    #[test]
    fn assign_route_order_filters_unknown_dedupes_and_assigns() {
        // Wiring check: assign extracts known profile ids from the config, drops unknown ids,
        // collapses duplicates (first wins), and writes the result to `config.route_order`.
        let mut cfg = AppConfig::default();
        cfg.profiles.push(mk_profile("a"));
        cfg.profiles.push(mk_profile("b"));
        cfg.profiles.push(mk_profile("c"));
        // "x" unknown -> dropped; second "c" -> collapsed; order preserved (first wins).
        assign_route_order(&mut cfg, vec!["c".into(), "x".into(), "a".into(), "c".into()]);
        assert_eq!(cfg.route_order, vec!["c".to_string(), "a".to_string()]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml assign_route_order`
Expected: FAIL to compile — `cannot find function `assign_route_order``.

- [ ] **Step 3: Rename the generic helper + add the route commands**

3a. Rename `normalize_usage_order` → `normalize_id_order` (`commands.rs:32-41`) and make its doc entity-agnostic:

```rust
/// Id-list ordering for drag-display surfaces: drop ids that are no longer known and collapse
/// duplicates (first occurrence wins). Pure — unit-tested directly.
fn normalize_id_order(ids: Vec<String>, known_ids: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let known: HashSet<&str> = known_ids.iter().map(|s| s.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    ids.into_iter()
        .filter(|id| known.contains(id.as_str()) && seen.insert(id.clone()))
        .collect()
}
```

3b. Update `assign_usage_order` (`commands.rs:46-49`) to call the renamed helper:

```rust
pub(crate) fn assign_usage_order(config: &mut AppConfig, ids: Vec<String>) {
    let known: Vec<String> = config.providers.iter().map(|p| p.id.clone()).collect();
    config.usage_order = normalize_id_order(ids, &known);
}
```

3c. Add `assign_route_order` immediately after `assign_usage_order` (`commands.rs:49`):

```rust
/// Extract known profile ids, filter/dedupe `ids` via `normalize_id_order`, and assign to
/// `config.route_order`. Pure on the config (no persist/refresh) so the assignment wiring is
/// unit-testable without an `AppHandle`.
pub(crate) fn assign_route_order(config: &mut AppConfig, ids: Vec<String>) {
    let known: Vec<String> = config.profiles.iter().map(|p| p.id.clone()).collect();
    config.route_order = normalize_id_order(ids, &known);
}
```

3d. Add the two commands immediately after `set_usage_order` (`commands.rs:72`), mirroring `get_usage_order`/`set_usage_order` (`:51-72`):

```rust
/// The user's drag-order for the route list (路由 tab / 概览 路由 card / tray). Empty = config order.
#[tauri::command]
pub async fn get_route_order(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.config.read().await.route_order.clone())
}

/// Persist a new route-list order (profile ids). Unknown/duplicate ids are dropped.
/// Persists, then refreshes the tray so its reordered 路由 submenu applies immediately.
#[tauri::command]
pub async fn set_route_order(
    state: State<'_, AppState>,
    ids: Vec<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        assign_route_order(&mut config, ids);
        persist(&app, &config)?;
    }
    crate::tray::refresh_tray_menu(&app).await;
    Ok(())
}
```

3e. Update the two existing tests that call the old name directly — rename `normalize_usage_order_filters_unknown_and_dedupes` and `normalize_usage_order_empty_for_all_unknown` (`commands.rs:1484`, `:1492`) to call `normalize_id_order` instead (only the function name in the `assert_eq!`/call changes; bodies stay). Example for the first:

```rust
    #[test]
    fn normalize_id_order_filters_unknown_and_dedupes() {
        let known = ["a".to_string(), "b".to_string(), "c".to_string()];
        // "x" is unknown -> dropped; "b" duplicated -> kept once, first position wins.
        let ids = vec!["b".into(), "x".into(), "c".into(), "a".into(), "b".into()];
        assert_eq!(normalize_id_order(ids, &known), vec!["b".to_string(), "c".to_string(), "a".to_string()]);
    }

    #[test]
    fn normalize_id_order_empty_for_all_unknown() {
        let known = ["a".to_string()];
        assert!(normalize_id_order(vec!["z".into()], &known).is_empty());
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml commands::tests`
Expected: PASS — incl. the renamed `normalize_id_order_*` tests, the existing `assign_usage_order_*` test (still passes via the renamed helper), and the new `assign_route_order_*` test.

- [ ] **Step 5: Register the commands**

In `lib.rs` `invoke_handler` (`:137-142`), add the two new commands right after `commands::set_usage_order,` (line 142):

```rust
            commands::get_route_order,
            commands::set_route_order,
```

- [ ] **Step 6: Verify build**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: compiles cleanly (confirms registration + signatures).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): add get/set_route_order; share normalize_id_order"
```

---

### Task 3: Tray honors `route_order` (TDD)

**Files:**
- Modify: `src-tauri/src/tray.rs:234-257` (`tray_menu_spec` profiles build), `:811-839` (add tests beside the accounts-order tests).

**Interfaces:**
- Consumes: `AppConfig.route_order` (Task 1); `ProfileMenuSpec.profile_id` (`tray.rs:37`).
- Produces: `tray_menu_spec` returns `profiles` sorted by `route_order` rank.

- [ ] **Step 1: Write the failing tests**

Add beside `spec_accounts_ordered_by_usage_order` (`tray.rs:811`), constructing `Profile` inline like the existing tray tests (`tray.rs:560-565`):

```rust
    #[test]
    fn spec_profiles_ordered_by_route_order() {
        let mut cfg = AppConfig::default();
        cfg.profiles.push(Profile { id: "a".into(), name: "A".into(), aliases: vec![], backing_model_id: "m".into() });
        cfg.profiles.push(Profile { id: "b".into(), name: "B".into(), aliases: vec![], backing_model_id: "m".into() });
        cfg.profiles.push(Profile { id: "c".into(), name: "C".into(), aliases: vec![], backing_model_id: "m".into() });
        cfg.route_order = vec!["c".into(), "a".into()]; // "b" unlisted -> sinks to end

        let spec = tray_menu_spec(&cfg, &HashMap::new(), &HashMap::new());
        let ids: Vec<&str> = spec.profiles.iter().map(|p| p.profile_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]); // ordered, then unlisted in config order
    }

    #[test]
    fn spec_profiles_keep_config_order_when_route_order_empty() {
        let mut cfg = AppConfig::default();
        cfg.profiles.push(Profile { id: "a".into(), name: "A".into(), aliases: vec![], backing_model_id: "m".into() });
        cfg.profiles.push(Profile { id: "b".into(), name: "B".into(), aliases: vec![], backing_model_id: "m".into() });

        let spec = tray_menu_spec(&cfg, &HashMap::new(), &HashMap::new());
        let ids: Vec<&str> = spec.profiles.iter().map(|p| p.profile_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray::tests::spec_profiles`
Expected: FAIL — `spec_profiles_ordered_by_route_order` gets `["a","b","c"]` (config order) instead of `["c","a","b"]`. The empty-order test already passes.

- [ ] **Step 3: Sort the profiles vec by `route_order` rank**

In `tray_menu_spec` (`tray.rs:234`), change `let profiles =` to `let mut profiles =`, then add the sort immediately after the `.collect();` that ends the profiles build (currently line 257), mirroring the accounts sort at `tray.rs:271-272`:

```rust
    // Order by route_order rank; unlisted sink to the end, preserving config order (stable).
    profiles.sort_by_key(|p| cfg.route_order.iter().position(|id| id == &p.profile_id).unwrap_or(usize::MAX));
```

(The downstream `TrayMenuSpec { profiles, accounts, tooltip }` move at `tray.rs:288` is unchanged.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray`
Expected: PASS — both new profile-order tests plus all existing tray tests (the rename in Task 2 didn't touch tray.rs; accounts tests still pass).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): order 路由 submenu by route_order"
```

---

### Task 4: Frontend data layer — commands wrappers + store

**Files:**
- Modify: `src/lib/commands.ts:24-25` (add wrappers), `src/stores/config.ts:2` (import `computed`), `:14` (`routeOrder` ref), `:27-42` (`loadAll`), `:101-121` (actions/migration), `:123-148` (return block).

**Interfaces:**
- Consumes: `get_route_order` / `set_route_order` commands (Task 2).
- Produces: `config.routeOrder` (ref), `config.profilesOrdered` (computed `Profile[]`), `config.setRouteOrder(ids)` — consumed by Tasks 5–6.

- [ ] **Step 1: Add the typed command wrappers**

In `commands.ts`, add immediately after `setUsageOrder` (line 25):

```ts
export const getRouteOrder = () => invoke<string[]>("get_route_order");
export const setRouteOrder = (ids: string[]) => invoke<void>("set_route_order", { ids });
```

- [ ] **Step 2: Extend the config store**

2a. `src/stores/config.ts:2` — add `computed` to the vue import:

```ts
import { computed, ref } from "vue";
```

2b. After `const usageOrder = ref<string[]>([]);` (line 14), add:

```ts
  const routeOrder = ref<string[]>([]);
```

2c. In `loadAll`, after `usageOrder.value = await api.getUsageOrder();` (line 36), add:

```ts
      routeOrder.value = await api.getRouteOrder();
```

And after `await migrateUsageOrderIfNeeded();` (line 38), add:

```ts
      await migrateRouteOrderIfNeeded();
```

2d. Add the `profilesOrdered` computed, the `setRouteOrder` action, and the migration. Place them right after `setUsageOrder` / `migrateUsageOrderIfNeeded` (after line 121):

```ts
  /** Profiles ordered by `routeOrder` rank (unlisted sink to end). Single frontend impl of the
   *  route_order semantics shared by 路由 tab, 概览 路由 card (mirrors the backend tray sort). */
  const profilesOrdered = computed(() => {
    const rank = new Map<string, number>();
    routeOrder.value.forEach((id, i) => rank.set(id, i));
    return [...profiles.value].sort((a, b) => {
      const ia = rank.get(a.id);
      const ib = rank.get(b.id);
      if (ia === undefined && ib === undefined) return 0;
      if (ia === undefined) return 1; // unlisted sink to end
      if (ib === undefined) return -1;
      return ia - ib;
    });
  });

  /** Persist a new route-list order, then re-fetch (canonical re-fetch-after-mutate). */
  async function setRouteOrder(ids: string[]) {
    await api.setRouteOrder(ids);
    routeOrder.value = await api.getRouteOrder();
  }

  /** One-time: seed `route_order` from the legacy localStorage drag-order key, then drop it.
   *  Idempotent — no-ops once `routeOrder` is non-empty. */
  async function migrateRouteOrderIfNeeded() {
    if (routeOrder.value.length > 0) return;
    try {
      const raw = localStorage.getItem("switchlm:order:profiles");
      if (!raw) return;
      const ids = JSON.parse(raw);
      if (!Array.isArray(ids) || ids.length === 0) return;
      await setRouteOrder(ids as string[]);
      localStorage.removeItem("switchlm:order:profiles");
    } catch {
      // Malformed legacy key — leave it; backend order simply stays empty (config order).
    }
  }
```

2e. Expose them in the store's return object (`config.ts:123-148`). Add `routeOrder` near `usageOrder`, and `profilesOrdered` + `setRouteOrder` near `setUsageOrder`:

```ts
    routeOrder,
    profilesOrdered,
    setRouteOrder,
```

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: PASS (no errors). `profilesOrdered` is `ComputedRef<Profile[]>`, unwrapped to `Profile[]` on the store proxy.

- [ ] **Step 4: Commit**

```bash
git add src/lib/commands.ts src/stores/config.ts
git commit -m "feat(store): add routeOrder + profilesOrdered; migrate profiles localStorage"
```

---

### Task 5: 路由 tab — config-backed debounced reorder

**Files:**
- Modify: `src/views/Profiles.vue:2` (add `watch`), `:21` (remove `useOrdered` import), `:27` (replace reorder setup), `:122` (draggable events).

**Interfaces:**
- Consumes: `config.profilesOrdered`, `config.setRouteOrder` (Task 4).

- [ ] **Step 1: Update vue import + remove useOrdered**

`src/views/Profiles.vue:2` — add `watch`:

```ts
import { computed, onMounted, reactive, ref, watch } from "vue";
```

`src/views/Profiles.vue:21` — delete the line `import { useOrdered } from "../lib/useOrdered";`.

- [ ] **Step 2: Replace the reorder setup**

Replace `src/views/Profiles.vue:27` (`const { ordered, commit } = useOrdered("switchlm:order:profiles", () => config.profiles, (p) => p.id);`) with the config-backed debounced reorder, mirroring `Usage.vue:51-89`:

```ts
// Config-backed drag order (single source shared with tray + 概览). Local `ordered` updates
// instantly for a responsive drag; persistence is debounced to coalesce rapid reorder into
// one backend save + tray rebuild.
const ordered = ref<Profile[]>([]);
const dragging = ref(false);
watch(
  () => config.profilesOrdered,
  (list) => {
    if (!dragging.value) ordered.value = [...list];
  },
  { immediate: true },
);
let persistTimer: ReturnType<typeof setTimeout> | null = null;
function commit() {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    config.setRouteOrder(ordered.value.map((p) => p.id)).catch((e) => msg.error(`顺序保存失败：${String(e)}`));
    persistTimer = null;
  }, 400);
}
function onDragEnd() {
  dragging.value = false;
  commit();
}
```

(`Profile` is already imported at `Profiles.vue:17`; `msg` is the existing `useMessage()` at `:23`.)

- [ ] **Step 3: Wire the drag events**

`src/views/Profiles.vue:122` — add `@start` and change `@end`:

```html
    <draggable v-model="ordered" item-key="id" class="drag-list" handle=".drag-handle" :animation="150" @start="dragging = true" @end="onDragEnd">
```

- [ ] **Step 4: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/views/Profiles.vue
git commit -m "feat(profiles): config-backed debounced route reorder"
```

---

### Task 6: 概览 路由 card — read `profilesOrdered`, drop localStorage

**Files:**
- Modify: `src/views/Dashboard.vue:19` (remove import), `:32-34` (remove `orderedProfiles`), `:263` (`v-for` source).

**Interfaces:**
- Consumes: `config.profilesOrdered` (Task 4).

- [ ] **Step 1: Remove the useOrdered import**

`src/views/Dashboard.vue:19` — delete `import { useOrdered } from "../lib/useOrdered";`.

- [ ] **Step 2: Remove the localStorage-backed `orderedProfiles`**

`src/views/Dashboard.vue:32-34` — delete the comment + const:

```ts
/** 路由顺序与「路由」tab 保持一致：复用同一份 localStorage 拖拽顺序。无 keep-alive，
 *  切换 tab 会重新挂载本视图并读取最新顺序。 */
const { ordered: orderedProfiles } = useOrdered("switchlm:order:profiles", () => config.profiles, (p) => p.id);
```

(The card is read-only — it consumes the shared order, no drag.)

- [ ] **Step 3: Point the v-for at the store computed**

`src/views/Dashboard.vue:263` — change `v-for="p in orderedProfiles"` to:

```html
          v-for="p in config.profilesOrdered"
```

- [ ] **Step 4: Type-check + build**

Run: `npm run build`
Expected: PASS (vue-tsc + vite build). This is the first build that exercises the whole frontend change end-to-end.

- [ ] **Step 5: Commit**

```bash
git add src/views/Dashboard.vue
git commit -m "feat(dashboard): route card reads profilesOrdered"
```

---

### Task 7: Full verification

- [ ] **Step 1: Full backend test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — incl. `route_order`, `normalize_id_order`, `assign_route_order`, and the tray `spec_profiles_*` tests; no regressions in `usage_order`/`assign_usage_order`/tray accounts tests.

- [ ] **Step 2: Frontend build**

Run: `npm run build`
Expected: PASS.

- [ ] **Step 3: Manual smoke test**

Run: `npm run tauri dev`
Verify:
1. Add/reorder ≥3 routes in the 路由 tab — the 概览 路由 card follows instantly (local), and after ~400 ms the tray 路由 submenu matches the new order.
2. Switch a backing model from the tray → tray rebuilds and the route order is **preserved** (survives the rebuild, proving it reads `route_order` not config order).
3. Quit + relaunch → order persists (proves it was written to `app_config.json`, not just localStorage).
4. Check `app_config.json` no longer relies on `switchlm:order:profiles` (the localStorage key is removed after first load with a prior order).

- [ ] **Step 4: Update CLAUDE.md conventions note**

`CLAUDE.md:85` lumps `usePolling` and `useOrdered` together and says the localStorage drag-order is "*never sent to the backend*; shared storage keys keep order consistent across views." After this change the **route list** order is no longer localStorage — it's the backend `route_order` field, shared with the tray. Replace that whole line with:

```markdown
- **`usePolling`** (auto-clears on unmount; interval can be a getter that re-arms; errors swallowed). **`useOrdered`** is a localStorage drag-reorder, cosmetic and *never sent to the backend* (Models/Fallback/Provider tabs). The **route list** and **usage display** orders are exceptions: persisted backend fields (`AppConfig.route_order` profile ids, `usage_order` provider ids) shared across their views *and the tray*, so a drag survives a tray rebuild and an app restart.
```

```bash
git add CLAUDE.md
git commit -m "docs: route order now backend-backed (route_order)"
```
