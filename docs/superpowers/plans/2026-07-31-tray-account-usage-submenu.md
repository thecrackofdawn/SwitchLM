# Tray「账户用量」hover submenu — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the tray's "账户用量" leaf item into a hover-expanded submenu listing each account's usage rate (read-only), mirroring how "路由" already behaves.

**Architecture:** All display logic stays in the pure, unit-tested `tray_menu_spec` (add an `accounts` slice built from the existing per-provider usage map + `cfg.providers`). The native `build_menu` swaps the leaf `MenuItem` for a `Submenu`; `on_menu_event` drops the now-dead `"usage"` click→navigate arm. No new data flow, timers, or IPC.

**Tech Stack:** Rust (Tauri v2 tray/menu API), existing `UsageSnapshot` / `usage_display` helpers, co-located `#[cfg(test)] mod tests`.

## Global Constraints

- **Logs/labels identify vendor + account by display name, never the opaque internal `id`** — but the row still carries `provider_id` in its menu-id (`acct:{provider_id}`) purely for click-handler namespacing; the visible label is display-name based. (CLAUDE.md convention.)
- Pure spec logic (`tray_menu_spec`, `account_label`) must remain unit-testable without a `Tauri`/`AppHandle` — do not pull Tauri types into the pure path.
- The crate must build with **no new warnings** (watch for unused imports after removing `emit`).
- UI copy is Chinese; reuse the exact existing strings (`"账户用量"`, `"路由"`, `"退出"`).

## File Structure

- **Modify:** `src-tauri/src/tray.rs` — the only file with real logic. Adds `AccountUsageSpec` + `account_label` + `TrayMenuSpec.accounts`, extends `tray_menu_spec`, rewires `build_menu`, trims `on_menu_event`.
- **Modify (optional cleanup):** `src/App.vue` — remove the now-dead `listen("navigate")` listener (the tray was its only emitter).

---

### Task 1: Pure account-usage rows in `tray_menu_spec`

Adds the read-only per-account usage slice to the pure menu spec. Lands the helper + struct + spec field together so the private helper is consumed in the same task (no dead-code warning between tasks).

**Files:**
- Modify: `src-tauri/src/tray.rs` (spec structs ~L27-47; `usage_display` ~L63-70; `tray_menu_spec` ~L99-142; `mod tests` ~L269+)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: existing `usage_display(u: &UsageSnapshot) -> Option<String>`, the `multi: HashSet<&str>` already computed at the top of `tray_menu_spec`, and `cfg.providers` / `usage: &HashMap<String, UsageSnapshot>`.
- Produces: `pub struct AccountUsageSpec { provider_id, label }`, `TrayMenuSpec.accounts: Vec<AccountUsageSpec>`, private `fn account_label(...) -> Option<String>`.

- [ ] **Step 1: Add the failing helper tests**

Append to `mod tests` in `src-tauri/src/tray.rs`:

```rust
    #[test]
    fn account_label_plan_shows_pct() {
        assert_eq!(account_label("智谱", None, &snap(80.0)).unwrap(), "智谱 80%");
    }

    #[test]
    fn account_label_consumption_shows_balance() {
        let u = UsageSnapshot {
            used: Some(51.23),
            total: Some(100.0),
            remaining: Some(48.77),
            reset_at: None,
            unit: "CNY".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "consumption".into(),
            plan_info: None,
        };
        assert_eq!(account_label("DS工作号", None, &u).unwrap(), "DS工作号 CNY 48.77");
    }

    #[test]
    fn account_label_appends_vendor_tag() {
        assert_eq!(
            account_label("火山号A", Some("volcengine"), &snap(45.0)).unwrap(),
            "火山号A [volcengine] 45%"
        );
    }

    #[test]
    fn account_label_none_when_no_usage_value() {
        let none = UsageSnapshot {
            used: None,
            total: None,
            remaining: None,
            reset_at: None,
            unit: "%".into(),
            raw_summary: None,
            plan: None,
            tiers: vec![],
            billing_model: "plan".into(),
            plan_info: None,
        };
        assert!(account_label("自定义", None, &none).is_none());
    }
```

- [ ] **Step 2: Run the helper tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml account_label`
Expected: COMPILE ERROR — `account_label` not defined.

- [ ] **Step 3: Add the `AccountUsageSpec` struct + `account_label` helper**

In `src-tauri/src/tray.rs`, add the struct right after `ModelOptionSpec` (after its closing `}` ~L47):

```rust
#[derive(Debug, PartialEq)]
pub struct AccountUsageSpec {
    pub provider_id: String,
    /// Display label, e.g. "智谱 80%" / "DS工作号 CNY 48.77" / "火山号A [volcengine] 45%".
    pub label: String,
}
```

Add the helper immediately after `usage_display` (before `model_label`, ~L70):

```rust
/// Tray label for one account's usage: display name + (multi-account) vendor tag + usage display.
/// `None` when the account has no concrete usage value — the caller hides that account.
fn account_label(display_name: &str, vendor_tag: Option<&str>, u: &UsageSnapshot) -> Option<String> {
    let display = usage_display(u)?;
    let mut s = display_name.to_string();
    if let Some(v) = vendor_tag {
        s.push_str(&format!(" [{v}]"));
    }
    s.push_str(&display);
    Some(s)
}
```

- [ ] **Step 4: Run the helper tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml account_label`
Expected: PASS (4 tests). Note: a non-test `cargo build` may still warn `function account_label is never used` — that is resolved in Step 5.

- [ ] **Step 5: Add the failing spec tests for `tray_menu_spec` accounts**

Append to `mod tests`:

```rust
    #[test]
    fn spec_accounts_lists_usage_per_provider_in_order() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        cfg.providers.push(mk_provider("deepseek", "DS工作号"));
        let mut usage = HashMap::new();
        usage.insert("zhipu".into(), snap(80.0));
        usage.insert(
            "deepseek".into(),
            UsageSnapshot {
                used: Some(51.23),
                total: Some(100.0),
                remaining: Some(48.77),
                reset_at: None,
                unit: "CNY".into(),
                raw_summary: None,
                plan: None,
                tiers: vec![],
                billing_model: "consumption".into(),
                plan_info: None,
            },
        );

        let spec = tray_menu_spec(&cfg, &usage, &HashMap::new());
        assert_eq!(spec.accounts.len(), 2);
        assert_eq!(spec.accounts[0].provider_id, "zhipu");
        assert_eq!(spec.accounts[0].label, "智谱 80%");
        assert_eq!(spec.accounts[1].provider_id, "deepseek");
        assert_eq!(spec.accounts[1].label, "DS工作号 CNY 48.77");
    }

    #[test]
    fn spec_accounts_hides_providers_without_usage() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        cfg.providers.push(mk_provider("custom", "自定义")); // no adapter / absent from map
        let mut usage = HashMap::new();
        usage.insert("zhipu".into(), snap(80.0));
        // "custom" deliberately absent

        let spec = tray_menu_spec(&cfg, &usage, &HashMap::new());
        assert_eq!(spec.accounts.len(), 1);
        assert_eq!(spec.accounts[0].provider_id, "zhipu");
    }

    #[test]
    fn spec_accounts_vendor_tag_only_for_multi_account_vendor() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("volc1", "火山号A"));
        cfg.providers.push(mk_provider("volc2", "火山号B"));
        cfg.providers[0].vendor = "volcengine".into();
        cfg.providers[1].vendor = "volcengine".into();
        cfg.providers.push(mk_provider("zhipu", "智谱"));
        let mut usage = HashMap::new();
        usage.insert("volc1".into(), snap(40.0));
        usage.insert("volc2".into(), snap(70.0));
        usage.insert("zhipu".into(), snap(80.0));

        let spec = tray_menu_spec(&cfg, &usage, &HashMap::new());
        let by_id: HashMap<&str, &AccountUsageSpec> =
            spec.accounts.iter().map(|a| (a.provider_id.as_str(), a)).collect();
        assert_eq!(by_id["volc1"].label, "火山号A [volcengine] 40%");
        assert_eq!(by_id["volc2"].label, "火山号B [volcengine] 70%");
        assert_eq!(by_id["zhipu"].label, "智谱 80%"); // single-account vendor: no tag
    }

    #[test]
    fn spec_accounts_empty_when_no_provider_has_usage() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(mk_provider("custom", "自定义"));
        let spec = tray_menu_spec(&cfg, &HashMap::new(), &HashMap::new());
        assert!(spec.accounts.is_empty());
    }
```

- [ ] **Step 6: Run the spec tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml spec_accounts`
Expected: COMPILE ERROR — no `accounts` field on `TrayMenuSpec`.

- [ ] **Step 7: Extend `tray_menu_spec` to build `accounts`**

In `src-tauri/src/tray.rs`, first add the field to `TrayMenuSpec` (~L28-30):

```rust
#[derive(Debug, PartialEq)]
pub struct TrayMenuSpec {
    pub profiles: Vec<ProfileMenuSpec>,
    pub accounts: Vec<AccountUsageSpec>,
}
```

Then, inside `tray_menu_spec`, reuse the already-computed `multi` set to build `accounts` just before the final return. Replace the closing:

```rust
        .collect();
    TrayMenuSpec { profiles }
}
```

with:

```rust
        .collect();
    let accounts = cfg
        .providers
        .iter()
        .filter_map(|p| {
            let tag = if multi.contains(p.vendor.as_str()) { Some(p.vendor.as_str()) } else { None };
            let label = account_label(p.display_name.as_str(), tag, usage.get(&p.id)?)?;
            Some(AccountUsageSpec { provider_id: p.id.clone(), label })
        })
        .collect();
    TrayMenuSpec { profiles, accounts }
}
```

- [ ] **Step 8: Run the full tray test module to verify all pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray`
Expected: PASS — all pre-existing tests (they index `.profiles`, unaffected by the new field) plus the 8 new tests.

- [ ] **Step 9: Verify the non-test build is warning-clean**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: succeeds with no `account_label is never used` / dead-code warning (the helper is now consumed by `tray_menu_spec`).

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): per-account usage rows in tray_menu_spec"
```

---

### Task 2: Render the usage submenu + drop the dead click handler

Native-menu wiring. `build_menu`/`on_menu_event` need a live `AppHandle`, so they are not unit-testable — verified by build + the pure-spec tests + manual run.

**Files:**
- Modify: `src-tauri/src/tray.rs` — `build_menu` (~L146-170), `on_menu_event` (~L243-267), imports (~L9-18)

**Interfaces:**
- Consumes: `TrayMenuSpec.accounts` + `AccountUsageSpec { provider_id, label }` from Task 1.
- Produces: native `Submenu("账户用量")` with `MenuItem` rows (`acct:{provider_id}`) or a disabled `(无用量数据)` placeholder.

- [ ] **Step 1: Replace the leaf `账户用量` item with a `Submenu`**

In `build_menu` (`src-tauri/src/tray.rs`), replace this line (~L166):

```rust
    menu.append(&MenuItem::with_id(app, "usage", "账户用量", true, None::<&str>)?)?;
```

with:

```rust
    let usage_sub = Submenu::new(app, "账户用量", true)?;
    if spec.accounts.is_empty() {
        usage_sub.append(&MenuItem::with_id(
            app,
            "usage_empty",
            "(无用量数据)",
            false,
            None::<&str>,
        )?)?;
    } else {
        for acc in &spec.accounts {
            usage_sub.append(&MenuItem::with_id(
                app,
                format!("acct:{}", acc.provider_id),
                acc.label.clone(),
                true,
                None::<&str>,
            )?)?;
        }
    }
    menu.append(&usage_sub)?;
```

- [ ] **Step 2: Remove the dead `"usage"` click handler**

In `on_menu_event` (`src-tauri/src/tray.rs`), delete the `"usage"` arm (~L246-250) so the match begins directly with `"quit"`. The full `match` becomes:

```rust
    match id {
        "quit" => app.exit(0),
        other => {
            if let Some(rest) = other.strip_prefix(BACKING_PREFIX) {
                if let Some((profile_id, model_id)) = rest.split_once(ID_JOIN) {
                    let app = app.clone();
                    let profile_id = profile_id.to_string();
                    let model_id = model_id.to_string();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>().inner().clone();
                        let _ = commands::set_profile_backing_core(&state, &profile_id, &model_id, &app).await;
                        refresh_tray_menu(&app).await;
                    });
                }
            }
        }
    }
```

`"usage"`, `"usage_empty"`, and `acct:…` ids now fall into `other`, where `strip_prefix("backing:")` yields `None` → no-op (read-only, as designed). `show_main_window` stays — it is still used by `on_tray_icon_event`.

- [ ] **Step 3: Remove the now-unused `Emitter` import**

`emit` was the only `Emitter`-trait user in `tray.rs`. Change the import (~L13):

```rust
use tauri::{AppHandle, Emitter, Manager};
```

to:

```rust
use tauri::{AppHandle, Manager};
```

- [ ] **Step 4: Build warning-clean + run the test suite**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: succeeds, **no** `unused import: Emitter`, no other new warnings.

Run: `cargo test --manifest-path src-tauri/Cargo.toml tray`
Expected: PASS (no regressions; the menu wiring is exercised only at runtime).

- [ ] **Step 5: Manual verification (run the app)**

Run: `npm run tauri dev`
Then right-click the tray icon and verify:
1. **账户用量** now shows a ▶ and expands on hover (like 路由).
2. Each account with a usage value shows as a read-only row (`智谱  80%`, `DS工作号  CNY 48.77`, multi-account vendors get ` [vendor]`).
3. Accounts without usage data do **not** appear.
4. Clicking a usage row does nothing (no navigation).
5. Left-clicking the tray icon still opens the main window (套餐用量 reachable via the side menu).
6. If no account has usage data, the submenu shows a single disabled `(无用量数据)`.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tray.rs
git commit -m "feat(tray): account-usage hover submenu replaces leaf nav item"
```

---

### Task 3 (optional): Remove dead `navigate` listener in `App.vue`

The tray was the sole emitter of `navigate`; with Task 2 it no longer emits, so the frontend listener is dead code. Harmless — skip if you'd rather leave it.

**Files:**
- Modify: `src/App.vue` (~L18, L29-37, L40)

**Interfaces:** None (pure dead-code removal).

- [ ] **Step 1: Drop the event import**

In `src/App.vue`, remove this import line (~L18):

```ts
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
```

- [ ] **Step 2: Remove the listener declaration, setup, and teardown**

Delete the comment + declaration (~L29-30):

```ts
// Backend -> frontend navigation (e.g. tray 账户用量 opens the 套餐用量 page).
let unlistenNavigate: UnlistenFn | null = null;
```

so `onMounted` becomes:

```ts
onMounted(() => {
  mql = window.matchMedia("(prefers-color-scheme: dark)");
  mql.addEventListener("change", onScheme);
});
```

and remove the unlisten line in `onBeforeUnmount` (~L40):

```ts
  unlistenNavigate?.();
```

so it becomes:

```ts
onBeforeUnmount(() => {
  mql?.removeEventListener("change", onScheme);
});
```

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: PASS, no errors about `listen` / `UnlistenFn` / `unlistenNavigate`.

- [ ] **Step 4: Commit**

```bash
git add src/App.vue
git commit -m "chore(frontend): remove dead tray navigate listener"
```

---

## Self-Review

**Spec coverage:**
- ✅ Hover submenu mirroring 路由 → Task 2 Step 1 (`Submenu::new("账户用量")`).
- ✅ Read-only rows, no click action, no nav entry → Task 2 Step 2 (drop `"usage"` arm; `acct:`/`usage_empty` no-ops).
- ✅ One row per account in `cfg.providers` order → Task 1 Step 7 (`filter_map` over `cfg.providers`) + test `spec_accounts_lists_usage_per_provider_in_order`.
- ✅ Row appears iff `usage_display` is Some → Task 1 Step 3 (`account_label` returns `None`) + test `account_label_none_when_no_usage_value` and `spec_accounts_hides_providers_without_usage`.
- ✅ Plan `%` / consumption balance → tests `account_label_plan_shows_pct`, `account_label_consumption_shows_balance`.
- ✅ Vendor tag only for ≥2-account vendors → Task 1 Step 7 (`multi.contains`) + test `spec_accounts_vendor_tag_only_for_multi_account_vendor`.
- ✅ Empty placeholder `(无用量数据)` → Task 2 Step 1.
- ✅ Reuse existing gather/timer (no new data flow) → Task 1 consumes `usage` map already passed to `tray_menu_spec`.
- ✅ Frontend dead-listener cleanup → Task 3 (optional).

**Placeholder scan:** none — every step has concrete code/commands.

**Type consistency:** `AccountUsageSpec { provider_id: String, label: String }` defined in Task 1 Step 3, referenced identically in Task 1 Step 7 (`spec.accounts`) and Task 2 Step 1 (`acc.provider_id`, `acc.label`). `account_label(&str, Option<&str>, &UsageSnapshot) -> Option<String>` matches across Task 1 Steps 3 & 7 and every test. Menu-id prefixes `acct:` / `usage_empty` / `backing:` are disjoint.
