# Startup Window Auto-Fit to Route Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On app startup, grow the main window's width once so the widest Dashboard `RouteLine` pipeline no longer wraps — grow-only, capped at `screen.availWidth`, never shrinking, never overriding a later manual resize.

**Architecture:** A new pure policy function `computeTargetWidth` decides the target window width from measured widths; an impure `fitWindowToRoutes` reads the rendered `.pipeline` elements, calls the policy, and resizes via the Tauri window API. A module-level one-shot guard makes it fire exactly once per session. `Dashboard.vue` calls it in `onMounted` after data loads + `nextTick`. One new Tauri capability permission (`core:window:allow-set-size`) authorizes the resize.

**Tech Stack:** Vue 3 `<script setup>`, TypeScript, `@tauri-apps/api` v2 window API (`getCurrentWindow`, `LogicalSize`), Tauri v2 capabilities. No new dependencies.

## Global Constraints

(Copied verbatim from `docs/superpowers/specs/2026-08-02-startup-window-fit-route-design.md`. Every task implicitly includes these.)

- **Frontend-only.** No backend (`src-tauri/src/**`) changes, no new crate, no new npm dependency.
- **No frontend test runner.** The repo has no Vitest (`package.json` lists none) and the spec explicitly does not introduce one. Verification = `npx vue-tsc --noEmit` (type gate) + a zero-dependency `node` arithmetic check for the pure function + a manual run for the integrated behavior. Do NOT add Vitest.
- **Default window unchanged.** `src-tauri/tauri.conf.json` `width: 960, minWidth: 940` stays as-is; auto-fit is purely additive on top.
- **All CSS px.** Every measured/compared value is CSS px: `getBoundingClientRect().width`, `el.clientWidth`, `document.documentElement.clientWidth/clientHeight`, `window.screen.availWidth`, and `LogicalSize`. Do not mix in physical pixels / `scaleFactor`.
- **Grow-only, cap at `window.screen.availWidth`, fire once per session.**
- **Tauri v2 window API** is already a dependency: import `getCurrentWindow` and `LogicalSize` from `@tauri-apps/api/window`.
- **Spec code correction (IMPORTANT):** the spec's `computeTargetWidth` body adds `margin` unconditionally, which makes the window grow even when content already fits — contradicting spec requirement 2 ("if it fits, nothing happens"). This plan implements the corrected version: return `null` immediately when `naturalWidth <= clientWidth`. See Task 1 Step 1. This is the intended behavior; the spec code was illustrative.

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `src/lib/fitRouteWindow.ts` | Pure `computeTargetWidth` policy + private `naturalWidthOf` + impure `fitWindowToRoutes` (DOM read, Tauri resize, one-shot guard). Mirrors the `src/lib/quotaUtils.ts` "pure core + thin wrapper" pattern. | **Create** |
| `src/views/Dashboard.vue` | Call `fitWindowToRoutes` once in `onMounted` after data loads + `nextTick`. | **Modify** (3 small edits) |
| `src-tauri/capabilities/default.json` | Authorize `set_size` on the main window. | **Modify** (add 1 permission) |

No other files change. No new tests are committed (no runner — see Global Constraints); the `node` arithmetic check is run ephemerally and not saved.

---

## Task 1: Create `fitRouteWindow.ts` (pure policy + impure glue)

**Files:**
- Create: `src/lib/fitRouteWindow.ts`

**Interfaces:**
- Consumes: `@tauri-apps/api/window` → `getCurrentWindow`, `LogicalSize` (existing dep, no install). DOM APIs: `HTMLElement.children`, `HTMLElement.getBoundingClientRect()`, `HTMLElement.clientWidth`, `document.documentElement.clientWidth/clientHeight`, `window.screen.availWidth`.
- Produces (used by Task 2):
  - `computeTargetWidth(naturalWidth: number, clientWidth: number, currentWindowWidth: number, availWidth: number, margin?: number): number | null`
  - `fitWindowToRoutes(pipelineEls: HTMLElement[]): Promise<void>`

- [ ] **Step 1: Create the module**

Create `src/lib/fitRouteWindow.ts` with exactly this content:

```ts
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

/** px between adjacent pipeline children — matches RouteLine.vue `.pipeline { gap: 10px }`. */
const PIPELINE_GAP = 10;
/** Extra px so later badge-text changes (usage poll) don't re-introduce a wrap. */
const SAFETY_MARGIN = 24;

/**
 * Pure policy. Given the widest pipeline's natural (unwrapped) width and its current
 * rendered (client) width, return the window width to set — or null when no grow is
 * needed. Grow-only; capped at availWidth; null when content already fits.
 *
 * Returns null (no-op) when:
 *   - naturalWidth <= clientWidth  (content fits — spec req. 2), OR
 *   - the capped target is not larger than currentWindowWidth (no shrink / cap below current).
 */
export function computeTargetWidth(
  naturalWidth: number,
  clientWidth: number,
  currentWindowWidth: number,
  availWidth: number,
  margin: number = SAFETY_MARGIN,
): number | null {
  const overflow = naturalWidth - clientWidth;
  if (overflow <= 0) return null; // fits → no grow (spec req. 2)
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
 * Impure glue. Measures the widest pipeline and, if the window is too narrow, grows it
 * (grow-only, capped at screen.availWidth). Fires exactly once per session; no-ops on
 * later calls or when no pipelines are present. Errors are swallowed — the fit is
 * best-effort and must never break the UI.
 */
export async function fitWindowToRoutes(pipelineEls: HTMLElement[]): Promise<void> {
  if (done) return;
  done = true; // consume the one-shot whether or not pipelines exist
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

- [ ] **Step 2: Verify the pure-function arithmetic (zero-dep `node` check)**

The pure `computeTargetWidth` is the only logic worth unit-checking; since there is no
test runner, run this ephemeral JS mirror and confirm all four cases pass. Keep the JS
mirror identical to the TS body above.

Run:
```bash
node -e "const assert=require('node:assert');
function c(n,cl,cur,avail,margin=24){const o=n-cl;if(o<=0)return null;const need=cur+o+margin;const t=Math.min(need,avail);return t>cur?t:null;}
assert.strictEqual(c(700,720,960,1920),null,'fits -> no grow');
assert.strictEqual(c(1000,720,960,1920),1264,'grow by overflow+margin');
assert.strictEqual(c(9999,720,960,1280),1280,'cap at avail');
assert.strictEqual(c(9999,720,1400,1280),null,'no shrink when avail<current');
console.log('computeTargetWidth arithmetic OK');"
```
Expected output: `computeTargetWidth arithmetic OK` (no `AssertionError`). If it throws,
re-check the `overflow <= 0` early return and the `target > currentWindowWidth` guard.

- [ ] **Step 3: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors. (`fitWindowToRoutes` is exported but not yet imported anywhere —
that is fine; unused exports do not error. `@tauri-apps/api/window` resolves via the
existing `@tauri-apps/api` dependency.)

- [ ] **Step 4: Commit**

```bash
git add src/lib/fitRouteWindow.ts
git commit -m "feat(dashboard): add startup window auto-fit core (fitRouteWindow)"
```

---

## Task 2: Wire into Dashboard + grant resize permission

**Files:**
- Modify: `src/views/Dashboard.vue:2` (import), `src/views/Dashboard.vue:16` (import), `src/views/Dashboard.vue:136-145` (onMounted body)
- Modify: `src-tauri/capabilities/default.json:6-14` (permissions array)

**Interfaces:**
- Consumes: `fitWindowToRoutes` (Task 1). `nextTick` from `vue`. The Dashboard's existing `onMounted` already awaits `config.loadAll()` + `runtime.refresh()` (usage data, which drives the quota badges that contribute width) before the insertion point.
- Produces: the integrated startup auto-fit behavior.

- [ ] **Step 1: Grant the resize permission**

In `src-tauri/capabilities/default.json`, change the permissions array from:

```json
    "core:window:allow-hide",
    "core:window:allow-show",
    "core:window:allow-close"
  ]
```

to:

```json
    "core:window:allow-hide",
    "core:window:allow-show",
    "core:window:allow-close",
    "core:window:allow-set-size"
  ]
```

(Only change: add a trailing comma after `"core:window:allow-close"` and the new line. Do not touch any other permission.)

- [ ] **Step 2: Add the `vue` import for `nextTick`**

In `src/views/Dashboard.vue` line 2, change:

```ts
import { computed, onMounted, onUnmounted, ref } from "vue";
```

to:

```ts
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
```

- [ ] **Step 3: Import `fitWindowToRoutes`**

In `src/views/Dashboard.vue`, immediately after line 16 (`import RouteLine from "../components/RouteLine.vue";`), add:

```ts
import { fitWindowToRoutes } from "../lib/fitRouteWindow";
```

- [ ] **Step 4: Call it in `onMounted`, after the data loads + DOM flush**

In `src/views/Dashboard.vue`, the existing `onMounted` awaits a `Promise.all([...])` then
has a comment about binding-error refresh. Change:

```ts
    system.loadSettings(),
  ]);

  // If there's a binding error, start periodic refresh (every 5s)
```

to:

```ts
    system.loadSettings(),
  ]);

  // One-shot startup fit: grow the window so the widest route pipeline doesn't wrap.
  // Runs only once per session (module-level guard inside fitWindowToRoutes).
  await nextTick(); // pipelines + quota badges are painted
  await fitWindowToRoutes(
    Array.from(document.querySelectorAll<HTMLElement>(".route-card .pipeline")),
  );

  // If there's a binding error, start periodic refresh (every 5s)
```

Rationale for placement: by this point both `config.loadAll()` (profiles/models) and
`runtime.refresh()` (usage → quota badges, which add width) have resolved, and
`await nextTick()` guarantees Vue has flushed the `v-for` pipelines + badges to the DOM
before measurement. `.route-card .pipeline` selects exactly the Dashboard's route lines
(`RouteLine` is mounted only here; scoped styles keep the `pipeline` class).

- [ ] **Step 5: Type-check**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 6: Restart dev server to pick up the capability change**

Capability changes are not hot-reloaded. Stop the running `npm run tauri dev` (if any)
and start a fresh one:

Run: `npm run tauri dev`

(The Tauri build regenerates `src-tauri/gen/schemas/desktop-schema.json` from the
capabilities; the new `core:window:allow-set-size` permission is what lets `setSize`
succeed at runtime instead of throwing a permission error.)

- [ ] **Step 7: Manual verification (interactive)**

With at least one provider + model + profile configured (so the 概览「路由」card shows
pipelines), confirm each scenario in the running app:

- **Long content → grows, no wrap.** Rename a profile to something long (e.g.
  `claude-sonnet-4-5`) and/or give its backing model a fallback, then relaunch. On the
  Dashboard, the widest pipeline renders on a **single row** (open DevTools → inspect a
  `.pipeline`; it has one line of content, not two). The window is wider than 960.
- **Short content → untouched.** With only short names that already fit at 960, relaunch;
  the window **does not change size** (stays exactly 960 wide — no 24px jump). This is
  the spec-req-2 fix from Task 1.
- **Capped at screen.** With absurdly long content, the window grows no wider than
  `screen.availWidth`; remaining content wraps (accepted).
- **No console permission error.** DevTools console shows no `setSize` / permission error
  on launch. (If it does, the capability in Step 1 was not added or dev wasn't restarted.)
- **One-shot.** Navigate 概览 → 服务商 → 概览. The window does **not** resize again and no
  error is thrown (the module-level `done` guard holds across the remount).
- **Manual resize sticks.** After launch, drag the window narrower/wider; it stays where
  you put it.

If a subagent is executing this task, it cannot see the visual result — it should perform
Steps 1–6, confirm `npx vue-tsc --noEmit` is clean and the capability line is present
(`grep -n "core:window:allow-set-size" src-tauri/capabilities/default.json`), then **hand
Step 7 to the human** at the review checkpoint.

- [ ] **Step 8: Commit**

```bash
git add src/views/Dashboard.vue src-tauri/capabilities/default.json
git commit -m "feat(dashboard): auto-fit window width to route pipeline on startup"
```

---

## Self-Review (completed during authoring)

1. **Spec coverage:**
   - Req 1 (grow on startup, once, after data + DOM settle) → Task 2 Step 4 (placement after `Promise.all` + `nextTick`); one-shot guard → Task 1 Step 1 (`done` flag). ✓
   - Req 2 (grow-only; fits → nothing) → Task 1 `computeTargetWidth` `overflow <= 0` early return. ✓ (corrects spec code)
   - Req 3 (cap at `screen.availWidth`) → `Math.min(needed, availWidth)`. ✓
   - Req 4 (default width unchanged) → no `tauri.conf.json` edit. ✓
   - Req 5 (height untouched) → `LogicalSize(target, documentElement.clientHeight)`. ✓
   - Req 6 (fire-once across remounts) → module-level `done`. ✓
   - Req 7 (empty → no-op + consume one-shot) → Task 1 `done = true` before the empty check. ✓
   - Req 8 (manual resize unaffected) → one-shot only fires at startup. ✓
   - Non-functional (frontend-only, pure core, no Vitest) → Global Constraints + File Structure. ✓
   - Permission → Task 2 Step 1. ✓
2. **Placeholder scan:** none — every code step contains full, copy-pasteable content; the `node` check is concrete; manual scenarios are specific.
3. **Type consistency:** `computeTargetWidth` and `fitWindowToRoutes` signatures match between Task 1 (Produces) and Task 2 (import + call). `nextTick` added to the vue import. Selector `.route-card .pipeline` matches `Dashboard.vue` (`class="route-card"` on the NCard at `:252`, `.pipeline` root of `RouteLine.vue`).
4. **One correction to the spec** is called out in Global Constraints and Task 1: the fits → no-op early return, fixing the spec code's unconditional-margin growth.

No gaps found.
