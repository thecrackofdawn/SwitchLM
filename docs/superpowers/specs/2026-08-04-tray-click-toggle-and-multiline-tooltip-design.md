# Tray Click Toggle + Multi-Line Tooltip Design

**Date:** 2026-08-04
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

> **Revision (2026-08-05) — toggle decision changed from focus-based to visibility/minimized-based.**
> The original "foreground = visible **AND** focused" rule (see *Decision rule* below) is
> **superseded**. Clicking the system-tray icon transfers focus to the shell, so `is_focused()`
> is already `false` by the time the click handler runs — the `hide()` branch was unreachable and
> the second click never hid the window (the reported bug). The decision is now driven by
> `is_visible()` + `is_minimized()` (a tray click changes neither), via the pure, unit-tested
> `toggle_action(visible, minimized)`:
>
> | Window state | `is_visible()` | `is_minimized()` | Action |
> |---|---|---|---|
> | Hidden | false | – | `show()` + `set_focus()` |
> | Minimized | true | true | `unminimize()` + `show()` + `set_focus()` |
> | Shown on screen | true | false | `hide()` |
> | Any query error | – | – | summon (never hide) |
>
> Trade-off: a window that is **visible but covered by another app** now hides on click instead of
> being raised — the focus-based "covered → raise" cannot be made reliable because the tray click
> itself perturbs focus; click again to show it. Reliable two-way toggle is prioritized over that
> case. See `src-tauri/src/tray.rs::toggle_action` (+ unit tests).

## Overview

Two tray-icon behavior changes, both confined to `src-tauri/src/tray.rs`:

1. **Left-click toggles the main window with foreground awareness.** Today a
   left-click always shows + focuses the window (`show_main_window`). New behavior:
   if the window is genuinely in the foreground (visible **and** focused) the click
   hides it; otherwise (hidden, minimized, or visible-but-covered/unfocused) the
   click summons it — restoring from minimize if needed — and focuses it.
2. **Tooltip shows one account per line.** The hover tooltip currently joins each
   account's `"name value"` with `" · "` into a single line. New behavior: join
   with `"\n"` so each account renders on its own line. Content, ordering,
   filtering, and the whole-line truncation policy are unchanged.

Right-click menu, close-to-tray, and the periodic 15s menu refresh are untouched.

## Click Toggle

### Decision rule

> ⚠️ **Superseded by the 2026-08-05 revision above.** Focus (`is_focused()`) is unreliable at
> tray-click time; the live decision now uses `is_visible()` + `is_minimized()`. The table below
> is kept for history.

On left-click `MouseButtonState::Up` (the existing trigger in
`on_tray_icon_event`):

| Window state | `is_visible()` | `is_focused()` | Action |
|---|---|---|---|
| Hidden | false | – | `show()` + `set_focus()` |
| Minimized | true* | false | `unminimize()` + `show()` + `set_focus()` |
| Visible but covered (not focused) | true | false | `show()` + `set_focus()` |
| Visible and focused (foreground) | true | true | `hide()` |

\* On Windows a minimized window still reports visible; the `is_focused()` check
is what routes it to the summon branch. `unminimize()` is called unconditionally on
the summon path — it is a no-op for a non-minimized window.

### Error handling

`is_visible()` / `is_focused()` return `Result`. If either query fails, treat the
window as **not in the foreground** and summon it. Rationale: accidentally hiding
a window the user wanted is worse than a redundant show+focus; the safe default
is to bring the window forward.

### Code shape

- Replace `show_main_window(app)` with `toggle_main_window(app)`; the handler
  stays a thin wrapper (window-state queries are Tauri-runtime calls, not
  unit-testable — keep the decision inline but trivially readable).
- Update the `build_tray` doc comment that says "Left-click opens the main
  window" to describe the toggle semantics.

### Deliberately unchanged

- **Right-click → menu.** Unaffected.
- **`lib.rs` single-instance callback** keeps its unconditional `show() +
  set_focus()`: a second launch means "open the app", and toggling there could
  hide an already-foreground window — the opposite of user intent.
- **Close button → hide to tray** (`lib.rs` `on_window_event`). Unaffected.

## Multi-Line Tooltip

`usage_tooltip(lines)` currently joins with `" · "`; change the separator to
`"\n"`. Windows tray tooltips natively render `\n` as line breaks. Everything else
in the function is preserved:

- Empty input → `"SwitchLM"`.
- Whole-line-only truncation under the 120-char cap, with a ` …(+N)` tail when
  lines are dropped (a dropped-line marker after a newline reads as its own line,
  which is acceptable).
- Single-line-too-long fallback: hard-truncate the first line and append ` …`.

Per-account content stays the primary value (`tooltip_value`): plan → 5h used %
(falling back to top-level `used`), consumption → `"unit balance"`. Ordering
(`usage_order` rank, unlisted last) and the no-value filter are unchanged.

Example tooltip, two accounts:

```
DS CNY 48.77
智谱 80%
```

## Testing

- `on_tray_icon_event` / `toggle_main_window` touch the Tauri window runtime and
  stay untested by unit tests (no headless window); verified manually via
  `pnpm tauri dev` — hidden→click shows, focused→click hides,
  minimized/covered→click restores + focuses.
- Unit tests in `tray.rs` updated for the new separator:
  - `usage_tooltip_joins_few_accounts` → `"\n"`-joined expectation.
  - `usage_tooltip_truncates_many_accounts_with_ellipsis` → parse head by
    splitting on `" …(+"` and then on `"\n"`; same whole-line + cap assertions.
  - `usage_tooltip_empty_is_app_name` → unchanged.
  - `spec_tooltip_lists_primary_values_in_order` → expect `"DS CNY 48.77\n智谱 80%"`.
- Backend suite: `cargo test --manifest-path src-tauri/Cargo.toml` green.

## Out of Scope

- No menu structure changes (套餐用量 submenu, 退出).
- No tooltip content change beyond the separator (no header line, no tier
  detail — the menu already carries the full three-window summary).
- No frontend changes.
