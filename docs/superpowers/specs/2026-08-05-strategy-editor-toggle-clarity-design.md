# Strategy Editor Toggle-Clarity Design

**Date:** 2026-08-05
**Status:** Draft (pending user review)
**Author:** Claude (SwitchLM Project)

## Overview

The shared time-strategy editor (`src/components/StrategyEditor.vue`) is used by two
modals — the route editor (`src/views/Profiles.vue`, `idScope="route"`) and the failover
editor (`src/views/Fallback.vue`, `idScope="failover"`). Both modals reuse the per-strategy
**cards** from `StrategyEditor`, but the **section header + master toggle** that controls
"are time strategies active at all" is copy-pasted into each parent. That duplication, plus
two placement/wording choices, creates three usability problems this spec resolves:

1. **Master toggle reads as a free-floating status.** It sits alone under the divider
   (switch on the left, a vague subtitle "按策略时段路由" on the right) with no visual bond
   to the strategy cards it governs. Worse: turning it **off** leaves every card fully
   editable — nothing greys out — so there is no feedback that it is the module's master
   switch at all.
2. **Per-card toggle reads as "enable priority".** Each card's `NSwitch` is glued to the
   literal text "优先级", so it is easily mistaken for a toggle on the priority field. And
   a disabled card (`enabled=false`) has no visual treatment either.
3. **Every new strategy defaults to `priority: 1`.** Adding several in a row leaves them all
   at priority 1 until the user re-numbers each by hand.

All three are UI-layer only. **No backend, `types.rs`, `StrategyForm` shape, or persistence
change** — the data already carries `strategies_enabled` (Profile) / `fallback_strategies_enabled`
(Model) and per-strategy `enabled` + `priority`. This spec only changes how the editor
presents and locks those fields.

## Current state (for reference)

| Part | Location | Shared? |
|---|---|---|
| Section header `时段策略` divider + master `NSwitch` + subtitle | `Profiles.vue:223-230`, `Fallback.vue:289-296` | ❌ duplicated |
| Per-strategy card (switch / priority / 重复 / 时间 / 调用模型) | `StrategyEditor.vue:87-155` | ✅ |
| Bottom 取消 / 保存 footer | `Profiles.vue:234-238`, `Fallback.vue:300-304` | ✅ already present |

`StrategyEditor` currently takes `modelValue: StrategyForm[]`, `modelOptions`, `idScope`,
and emits `update:modelValue`. `blankStrategy()` hard-codes `priority: 1`
(`StrategyEditor.vue:35`). Nothing in the component binds `:disabled` to the master or
per-card `enabled` flags today.

## Design decisions

### Decision 1 — Lift the master toggle + section header into `StrategyEditor`

Both parents pass `form.strategies_enabled` down regardless, because the component needs it
to lock the cards (see Decision 3). Given that prop is mandatory anyway, moving the header
+ master switch *into* `StrategyEditor` costs almost nothing extra and **deletes ~7 lines of
duplicated template from each parent**. The alternative (keep the toggle in the parents,
restyle in place) still requires passing `masterEnabled` down and leaves the duplication —
strictly worse. **Lift it in.**

### Decision 2 — New section header: title left, master switch right

Replace `NDivider` + the loose `NSpace` with one flex section header:

```
时段策略                            启用时段策略 [ — ]
─────────────────────────────────────────────────
```

- Left: `<span>时段策略</span>`.
- Right: `<span>启用时段策略</span>` + a **slotless** `NSwitch` bound to `masterEnabled`.
  The switch uses no `#checked` / `#unchecked` slots — Naive UI renders those inside the
  track and a label that long looks cramped; the adjacent `<span>` is the label instead.
- The header's bottom border replaces `NDivider`.
- The header itself stays interactive when the master is off (so the user can toggle back on).

The one-line consequence subtitle renders directly **beneath the header row** (always visible,
matching current behavior) and is scoped by a new `scopeLabel` prop, because the
consequence genuinely differs between the two contexts:

- route (`Profiles.vue`): `已暂停策略，所有请求将直接走默认模型`
- failover (`Fallback.vue`): `已暂停策略，仅走默认转移目标`

### Decision 3 — Disabled = non-editable (master off and per-card off)

Per the chosen interaction model, "off" means the governed fields become **non-editable**,
not merely dimmed:

- **Master off** → the entire strategy list (all cards + the "添加策略" button) is locked.
- **Per-card off** → that card's priority select **and** body rows (重复 / 时间 / 调用模型)
  are locked; only the card's enable switch and delete stay active.

Implementation: lockable containers use the HTML `inert` attribute (bound as `:inert` —
Vue 3.5.13 treats it as a boolean attribute) plus an `opacity: .5` class for the visual dim.
`inert` is chosen over `pointer-events: none` because it also removes the subtree from the
tab order and the accessibility tree, so keyboard users cannot Tab into disabled controls
and screen readers announce them as inert. The section header (master switch) and each
card's header row (its enable switch + delete) sit **outside** their respective `inert`
scopes, so they stay operable when locked. The priority select shares the header row with
those always-active controls, so it is disabled individually via `:disabled="!s.enabled"`
rather than via the body's `inert` wrapper.

### Decision 4 — Per-card header restructure

Move the per-card `NSwitch` off the "优先级" text and to the far right of the header, next
to delete:

```
now:  [switch] 优先级（数字越小优先级越高） [select]   …   [删除]
becomes: 优先级 [select]                          …   [switch] [删除]
```

The switch is slotless with `aria-label="启用该策略"`; its position beside delete makes its
scope unambiguous. The "（数字越小优先级越高）" hint stays as-is (a future tweak could move it
to a tooltip, out of scope here). When the card is off (`enabled=false`), the priority select
is disabled too (`:disabled="!s.enabled"`, see Decision 3) — adjusting the priority of a
strategy that isn't active is meaningless — leaving only the enable switch and delete operable.

### Decision 5 — Priority auto-increments

`blankStrategy()` sets `priority` to `min(10, max(existing priorities) + 1)`, or `1` when the
list is empty, instead of the constant `1`. The dropdown still offers 1–10 and the user can
still edit any value; this only makes the default non-colliding. Priority remains a
user-controlled field — ties are still broken by the backend, unchanged.

### Decision 6 — Empty state

When the strategy list is empty (`modelValue.length === 0`) the current bare `暂无策略` line
is upgraded to a clearer call to action, e.g. `暂无时段策略，点击下方「添加策略」开始配置`,
rendered subdued above the "添加策略" button. It dims together with the rest of the list
when the master toggle is off (it lives inside the same lockable container), so an empty +
paused region reads as uniformly inactive rather than just sparse.

## Component API change (`StrategyEditor.vue`)

| Add | Signature | Notes |
|---|---|---|
| prop | `masterEnabled: boolean` | master switch state |
| emit | `update:masterEnabled: [boolean]` | parents bind `v-model:master-enabled` |
| prop | `scopeLabel?: string` (default `"路由"`) | interpolated into the off-state subtitle |

`StrategyForm` (exported from the `<script lang="ts">` block) is **unchanged**. The component
gains `:inert` bindings on the master-off list container and on each off-card's body, an
`opacity: .5` dim class, and a `.section-head` layout class — all using existing tokens
(`--sl-border`, etc.), so there is no `styles/theme.ts` change (these are behavior/opacity,
not Naive `GlobalThemeOverrides` values).

## Parent changes (`Profiles.vue`, `Fallback.vue`)

Each parent deletes its `NDivider` + `NSpace`(master switch + subtitle) block and binds the
new prop:

```vue
<StrategyEditor
  v-model="form.strategies"
  v-model:master-enabled="form.strategies_enabled"
  :model-options="..."
  :scope-label="'路由'"   <!-- Fallback.vue passes '转移' -->
  id-scope="..." />
```

`form.strategies_enabled` stays in `FormState` and is still saved to the backend exactly as
today. The list-view quick toggle (the `⏰ 策略 N 条` tag with its inline `NSwitch` on each
list card) is **unchanged**.

## Non-goals

- Backend, `types.rs`, `StrategyForm` shape, or `app_config.json`: no change.
- The 重复 day-of-week selector contrast (could be strengthened later): skipped.
- The list-view master toggle tag: untouched.
- Moving the priority hint to a tooltip: deferred.

## Verification

- `pnpm exec vue-tsc --noEmit` — type-check, focused on the `v-model:master-enabled` wiring
  and the new props/emits.
- `pnpm tauri dev` — manually open both the route editor and the failover editor and confirm:
  master off greys + locks the whole list; per-card off greys + locks that card's priority
  and body while its switch + delete still work; new strategies get incrementing priorities;
  header copy matches each context; an empty list shows the call-to-action hint.
- Keyboard check: with the master off and again with a card off, Tab through the modal and
  confirm focus never lands on a locked control (the `inert` regions are skipped).
