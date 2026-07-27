# Startup Window Auto-Fit to Route Pipeline Design

**Date:** 2026-08-02
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

## Overview

When SwitchLM launches, the window opens on the **Dashboard** at its fixed default
width (960px). The 概览「路由」card renders one `RouteLine` pipeline per profile:

```
[路由] glm-5.2  ▶  [主用] GLM-4.6 ❄ 80%  ▶  [降级] ↳ someModel（火山） +2
```

Each pipeline is `display: flex; flex-wrap: wrap` (`RouteLine.vue:122-128`), so when
the content area (~720px after the 132px sider and padding) is narrower than the
pipeline's natural width, the line **wraps onto two rows** — the visible degradation
this work removes.

At startup — once, after the initial data load — measure the widest pipeline's
natural (unwrapped) width and, if it exceeds the current content width, **grow the
window width** to fit it. Growth is **capped at `screen.availWidth`** and is
**grow-only** (never shrink). The default starting width (960) is unchanged. Manual
resizing is untouched because the auto-fit runs exactly once per launch.

## Background

### Why the window doesn't already fit

The default window (`src-tauri/tauri.conf.json:13-23`) is `width: 960, minWidth: 940`.
There is **no `tauri-plugin-window-state`** dependency (`Cargo.toml`, `package.json`),
so the window does **not** persist its size across launches — every launch starts at
960×680. The content available to a pipeline is roughly
`960 − 132 (sider) − 24×2 (content padding) − card padding ≈ 720px`, which is too tight
for a profile with a long name plus a 200px primary dropdown plus a fallback node.

### Why `scrollWidth` can't measure the natural width

Because `.pipeline` uses `flex-wrap: wrap`, an over-wide pipeline **wraps** rather
than overflowing. Once wrapped, `element.scrollWidth` reports the width of the widest
*single wrapped row*, not the full unwrapped width — so a naive
`scrollWidth - clientWidth` overflow check reads ~0 and never grows the window. The
natural width must instead be reconstructed by **summing the rendered children**.

### Resizing requires a new permission

The Tauri window API (`@tauri-apps/api` v2 is already a dependency) exposes
`getCurrentWindow().setSize(...)`, but `src-tauri/capabilities/default.json` only
grants `core:window:allow-{hide,show,close}`. Resizing from the frontend requires
adding `core:window:allow-set-size`.

## Requirements

### Functional

1. **Grow on startup.** Exactly once per app launch, after the Dashboard's initial
   data has loaded and the DOM has settled, measure every rendered `.pipeline` and
   grow the window wide enough that the widest one no longer wraps.
2. **Grow-only.** The window is only ever made wider, never narrower. If the natural
   width fits the current window, nothing happens.
3. **Cap at screen width.** The target width is clamped to `screen.availWidth`. If the
   natural width still exceeds the screen at that cap, the excess wraps as it does
   today (accepted, per the chosen policy).
4. **Default starting width unchanged.** The 960×680 default and `minWidth: 940` in
   `tauri.conf.json` stay as-is; auto-fit is purely additive on top of them.
5. **Height untouched.** Only width changes; the current inner height is preserved.
6. **Fire-once per launch.** Navigating away from and back to the Dashboard (which
   remounts the view — no keep-alive, `Dashboard.vue:31`) must **not** re-trigger a
   resize. A module-level guard makes the fit a one-shot per session.
7. **No-op when empty.** If there is no route content at startup (no
   profiles/models → `hasContent` false → no `.pipeline` elements), do nothing and
   consume the one-shot.
8. **Manual resize unaffected.** Because the fit runs only once at startup, any later
   user drag is final and never overridden.

### Non-functional

- **Frontend-only.** No backend or config-type changes; no new crate or npm
  dependency.
- **Isolated, testable policy.** The width arithmetic lives in a pure function in
  `src/lib/`, separate from the DOM/Tauri glue (mirrors the `quotaUtils.ts` pure-core
  pattern).
- **No new test infrastructure.** The repo has no frontend test runner
  (`package.json` lists no Vitest); one is not introduced for this. The pure function
  is trivially unit-testable if a runner is ever added.

## Design

### New module — `src/lib/fitRouteWindow.ts`

Two pieces: a pure policy function (the arithmetic) and an impure glue function (DOM
read + Tauri call) guarded by a module-level one-shot flag.

```ts
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

/** px between adjacent pipeline children — matches RouteLine.vue `.pipeline { gap: 10px }`. */
const PIPELINE_GAP = 10;
/** Extra px so later badge-text changes (usage poll) don't re-introduce a wrap. */
const SAFETY_MARGIN = 24;

/**
 * Pure: given the widest pipeline's natural (unwrapped) width and its current rendered
 * (client) width, compute the window width to set — or null when no grow is needed.
 * Grow-only: returns null unless target > currentWindowWidth. Capped at availWidth.
 */
export function computeTargetWidth(
  naturalWidth: number,
  clientWidth: number,
  currentWindowWidth: number,
  availWidth: number,
  margin: number = SAFETY_MARGIN,
): number | null {
  const overflow = Math.max(0, naturalWidth - clientWidth); // how much too narrow
  const needed = currentWindowWidth + overflow + margin;
  const target = Math.min(needed, availWidth);
  return target > currentWindowWidth ? target : null;
}

/** Natural (unwrapped) width of a `.pipeline` = sum of its direct children + gaps. */
function naturalWidthOf(el: HTMLElement): number {
  const kids = Array.from(el.children) as HTMLElement[];
  const sum = kids.reduce((acc, c) => acc + c.getBoundingClientRect().width, 0);
  return sum + PIPELINE_GAP * Math.max(0, kids.length - 1);
}

let done = false; // one-shot per session (module survives Dashboard remounts)

/**
 * Impure glue. Measures the widest pipeline, and if the window is too narrow, grows it
 * (grow-only, capped at screen.availWidth). No-op after the first call in a session,
 * or when no pipelines are present. Errors are swallowed (best-effort fit; never break
 * the UI over a resize failure).
 */
export async function fitWindowToRoutes(pipelineEls: HTMLElement[]): Promise<void> {
  if (done) return;
  done = true; // consume the one-shot whether or not pipelines exist (req. 7)
  if (pipelineEls.length === 0) return;
  try {
    const widest = pipelineEls
      .map((el) => ({ natural: naturalWidthOf(el), client: el.clientWidth }))
      .reduce((a, b) => (b.natural > a.natural ? b : a));
    // All values are CSS px → unit-consistent, no physical/logical conversion:
    //   - naturalWidth / clientWidth: getBoundingClientRect / clientWidth
    //   - current width / height: documentElement.clientWidth/Height (webview inner)
    //   - cap: window.screen.availWidth (taskbar-aware)
    const target = computeTargetWidth(
      widest.natural,
      widest.client,
      document.documentElement.clientWidth,
      window.screen.availWidth,
    );
    if (target != null) {
      await getCurrentWindow().setSize(
        new LogicalSize(Math.round(target), Math.round(document.documentElement.clientHeight)),
      );
    }
  } catch {
    // Resize is best-effort — never let it surface as a user-visible error.
  }
}
```

`window.screen.availWidth` is the **primary monitor's** usable width (CSS px,
taskbar-aware). The cap therefore reflects the primary display; launching on a smaller
secondary monitor is a known, accepted limitation (see Edge cases). Every value in the
function is CSS px, so the comparison and the `LogicalSize` argument are
unit-consistent with no physical/logical conversion.

### `Dashboard.vue`

Add the fit to the existing `onMounted` (`Dashboard.vue:136-167`), after the data it
awaits has resolved and the DOM has flushed:

```ts
import { nextTick } from "vue";
import { fitWindowToRoutes } from "../lib/fitRouteWindow";

// …inside onMounted, after the existing Promise.all([...]) of loads:
await nextTick(); // pipelines + quota badges are now painted
const pipelines = Array.from(
  document.querySelectorAll<HTMLElement>(".route-card .pipeline"),
);
// Guard on hasContent so an empty config does not run a pointless measurement.
if (hasContent.value) await fitWindowToRoutes(pipelines);
```

- **Selector scoping:** `.pipeline` is rendered only by `RouteLine`, which only the
  Dashboard mounts, so `document.querySelectorAll(".pipeline")` is already scoped in
  practice. The `.route-card .pipeline` qualifier is belt-and-suspenders. Scoped
  styles add a `data-v-*` attribute but do **not** remove the `pipeline` class, so the
  selector matches.
- **Why both `config.loadAll()` and `runtime.refresh()` first:** the primary node's
  quota badge (width-contributing) is derived from usage data; measuring before usage
  loads would under-measure and still wrap once the badge appears. Both are already
  awaited in the existing `onMounted` block.

### `src-tauri/capabilities/default.json`

Add one permission:

```jsonc
"permissions": [
  "core:default",
  "opener:default",
  "autostart:default",
  "core:tray:default",
  "core:window:allow-hide",
  "core:window:allow-show",
  "core:window:allow-close",
  "core:window:allow-set-size"   // ← new
]
```

No other backend change. No change to `tauri.conf.json` (default size/minWidth stay).

## Edge cases

| Case | Behavior |
|---|---|
| Empty config at startup (no profiles/models) | no `.pipeline` elements → `hasContent` false → fit skipped, one-shot consumed |
| Natural width already fits current window | `computeTargetWidth` returns null → no resize |
| Natural width exceeds `screen.availWidth` | grow to `availWidth`; remaining excess wraps (accepted) |
| Launched on a secondary monitor smaller than primary | cap still uses primary `screen.availWidth`; window may grow wider than the secondary display (known limitation — manual resize afterward) |
| `availWidth` < current window width (window wider than screen, rare) | `target ≤ availWidth < current` → null → no shrink (grow-only holds) |
| Usage poll later widens a badge slightly | the `SAFETY_MARGIN` (24px) absorbs small post-startup text growth |
| `setSize` / monitor query throws | caught and swallowed — fit is best-effort, never breaks the UI |
| User drags narrower during the brief startup window | negligible; the one-shot fires once and manual drag after is final |
| Route navigation remounts Dashboard | module-level `done` guard prevents a second fit |

## Out of scope

- **Persisting window size across launches** (`tauri-plugin-window-state`) — every
  launch still starts at 960; auto-fit re-runs each launch. Persistence is a separate
  decision.
- **Adjusting height** — only width is touched.
- **Re-fitting when routes/models change mid-session** — the fit is startup-only; a
  model added later that would wrap does not re-grow (user can resize manually).
- **Hiding the window pre-paint to avoid the one-time grow flicker** — would delay
  first paint and complicate the keyring-consent modal flow; not worth it for a
  single startup resize.
- **A frontend test runner (Vitest)** — not introduced; `computeTargetWidth` is the
  natural unit target if one ever is.

## Testing

- **Type safety:** `npm run build` (`vue-tsc --noEmit`) covers the new module +
  Dashboard wiring + the permission string (capabilities are JSON, not type-checked,
  but the build regenerates schemas).
- **Permission present:** after `npm run tauri dev`, confirm `setSize` does not throw
  a "not allowed" error (it would pre-fix). Equivalently, grep
  `core:window:allow-set-size` in `capabilities/default.json`.
- **Manual scenarios** (with ≥1 provider/model/profile configured):
  - **Long route name** (e.g. a profile named `claude-sonnet-4-5` + a long fallback
    model): on launch the window grows so the widest pipeline stays on one row; verify
    via DevTools that `.pipeline` no longer wraps.
  - **Short content** that already fits at 960: window does **not** change size.
  - **Excessive content** (very long names / deep chain): window grows up to
    `screen.availWidth` and no wider; remaining content wraps.
  - After launch, **drag the window narrower** → it stays where put; revisiting the
    Dashboard does not snap it back.
  - Launch on a **secondary smaller monitor** → known limitation: cap uses primary
    `screen.availWidth`, so the window may exceed the secondary display; verify it does
    not exceed the **primary** screen width.
- **Backend:** no change; `cargo test` unaffected.
