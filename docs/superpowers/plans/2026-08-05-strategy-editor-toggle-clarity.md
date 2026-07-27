# Strategy Editor Toggle-Clarity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the time-strategy editor's master toggle and per-card toggles unambiguous, with proper disabled/locked semantics — purely UI-layer, shared by the route and failover modals.

**Architecture:** Lift the duplicated "时段策略" section header + master `NSwitch` from the two parent modals into the shared `StrategyEditor.vue` (new `v-model:master-enabled` + `scopeLabel` props). Restructure each strategy card so the enable switch sits at the header-right (away from "优先级"), and lock governed regions with HTML `inert` + `opacity:.5` (not `pointer-events:none`) so keyboard focus and screen readers are handled. No backend, `types.rs`, or `StrategyForm` changes.

**Tech Stack:** Vue 3.5.13 (`<script setup>` + `withDefaults`), Naive UI ^2.44 (`NSwitch`/`NSelect`/`NButton`/`NTimePicker`/`NTag`/`NSpace`), TypeScript ~5.6, existing CSS tokens. `:inert` boolean binding requires Vue ≥ 3.3 (we have 3.5.13).

## Global Constraints

Copied verbatim from the design spec (`docs/superpowers/specs/2026-08-05-strategy-editor-toggle-clarity-design.md`):

- **No backend / `types.rs` / `StrategyForm` shape / `app_config.json` changes.** The data already carries `strategies_enabled` (Profile) / `fallback_strategies_enabled` (Model) and per-strategy `enabled` + `priority`. This plan is UI-only.
- **Frontend package manager is pnpm.** Never use `npm`/`npx`; use `pnpm`/`pnpm exec`.
- **No frontend unit-test runner exists** (no vitest/@vue/test-utils). Verify each task with `pnpm exec vue-tsc --noEmit` (type-check) + `pnpm tauri dev` (manual). Do not introduce a test framework.
- **Use existing CSS tokens** (`--sl-border`, `--sl-text-2`, `--sl-text-3`) — no `styles/theme.ts` change (these are layout/opacity, not Naive `GlobalThemeOverrides` values).
- **UI copy is Chinese.** Preserve exact strings given below.
- **Commit hygiene:** the working tree has unrelated pre-existing modifications (`README.md`, the tray spec, `src-tauri/src/tray.rs`). Every commit below uses **explicit `git add <paths>`** — never `git add -A`/`git add .`, which would sweep those in.
- Dev directly on `main`; one commit per task.

## File Structure

| File | Responsibility | This plan |
|---|---|---|
| `src/components/StrategyEditor.vue` | Shared time-strategy editor: owns `StrategyForm`, add/remove/patch helpers, and now the section header + master toggle + per-card layout + locking | Modify (all 3 tasks) |
| `src/views/Profiles.vue` | Route editor modal — was rendering a duplicated header + master switch | Modify (Task 1 only) |
| `src/views/Fallback.vue` | Failover editor modal — was rendering a duplicated header + master switch | Modify (Task 1 only) |

`StrategyForm` (exported from `StrategyEditor.vue`'s `<script lang="ts">` block) is unchanged — both parents keep importing it as-is.

---

### Task 1: Lift section header + master toggle into StrategyEditor; wire both parents

This is the atomic "move the header into the component" refactor. After it, each modal has exactly one "时段策略" header (rendered by the component) whose switch drives the parent's `form.strategies_enabled`. The per-card layout is untouched in this task.

**Files:**
- Modify: `src/components/StrategyEditor.vue` (`<script setup>` props/emits ~lines 19-29; template top ~lines 86-90 and bottom ~lines 157-159; `<style>` ~lines 161-185)
- Modify: `src/views/Profiles.vue` (delete ~lines 223-230; edit ~line 232)
- Modify: `src/views/Fallback.vue` (delete ~lines 289-296; edit ~line 298)

**Interfaces:**
- Produces (consumed by parents): new prop `masterEnabled: boolean` (required) bound via `v-model:master-enabled`; new optional prop `scopeLabel?: string` (default `"路由"`) interpolated into the master hint. New emit `update:masterEnabled: [boolean]`.

- [ ] **Step 1: Extend the component's props/emits and add hint computeds**

In `src/components/StrategyEditor.vue` `<script setup>`, first add the Vue import at the top (currently only naive-ui / id / selectLabel are imported):

```ts
import { computed } from "vue";
```

Replace the existing `defineProps`/`defineEmits` block:

```ts
const props = withDefaults(
  defineProps<{
    modelValue: StrategyForm[];
    modelOptions: { label: string; value: string }[];
    idScope: string;
    masterEnabled: boolean;
    scopeLabel?: string;
  }>(),
  { scopeLabel: "路由" }
);
const emit = defineEmits<{
  "update:modelValue": [StrategyForm[]];
  "update:masterEnabled": [boolean];
}>();

// On/off hint beneath the header. On-state differs by one word (路由/转移);
// off-state differs by structure, so branch on scopeLabel (exactly two callers).
const hintOn = computed(() => `按策略时段${props.scopeLabel}`);
const hintOff = computed(() =>
  props.scopeLabel === "路由"
    ? "已暂停策略，所有请求将直接走默认模型"
    : "已暂停策略，仅走默认转移目标"
);
```

`masterEnabled` is intentionally **required** (no default) so a parent that forgets `v-model:master-enabled` fails type-check rather than silently showing a dead switch.

- [ ] **Step 2: Replace the template top — add the section header, hint, and the `.strategy-list` wrapper opening**

The template currently begins with `<div v-for="s in modelValue" ...>` directly. Insert the header + hint above it and open the lockable wrapper. Replace the template's opening (the `<template>` line through the `<div v-for ... class="strategy-card">` line) with:

```vue
<template>
  <div class="section-head">
    <span class="section-title">时段策略</span>
    <span class="master-toggle">
      <span class="muted">启用时段策略</span>
      <NSwitch
        :value="masterEnabled"
        size="small"
        @update:value="(v: boolean) => emit('update:masterEnabled', v)"
      />
    </span>
  </div>
  <div class="muted master-hint">{{ masterEnabled ? hintOn : hintOff }}</div>

  <div class="strategy-list" :class="{ 'is-locked': !masterEnabled }" :inert="!masterEnabled">
    <div v-for="s in modelValue" :key="s.id" class="strategy-card">
```

The existing card inner content (the `<NSpace>` header row + three `.row` divs) stays **unchanged** in this task — only the wrapper around it is new. `:inert="!masterEnabled"` renders the `inert` attribute when the master is off (Vue 3.5 boolean-attribute binding) and omits it when on.

- [ ] **Step 3: Close the `.strategy-list` wrapper after the add button**

The template currently ends with (a standalone empty-state `<span>` + the add `<NButton>`, as loose roots). Replace those lines:

```vue
  <span v-if="!modelValue.length" class="muted" style="display: block; margin-top: 8px">暂无策略</span>
  <NButton style="margin-top: 8px" block dashed @click="addStrategy">+ 添加策略</NButton>
</template>
```

with (same content, now **inside** the `.strategy-list` wrapper, so they dim+lock with the master toggle — note the empty-state text is deliberately unchanged here; Task 3 updates it):

```vue
    <span v-if="!modelValue.length" class="muted empty-hint">暂无策略</span>
    <NButton class="add-btn" block dashed @click="addStrategy">+ 添加策略</NButton>
  </div>
</template>
```

- [ ] **Step 4: Add the new CSS classes**

Append inside the existing `<style scoped>` block (keep all existing `.strategy-card`/`.row`/`.muted`/`.hint` rules):

```css
.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-bottom: 6px;
  margin-bottom: 4px;
  border-bottom: 1px solid var(--sl-border);
}
.section-title {
  font-size: 14px;
  font-weight: 500;
}
.master-toggle {
  display: flex;
  align-items: center;
  gap: 8px;
}
.master-hint {
  font-size: 12px;
  margin-bottom: 8px;
}
.strategy-list.is-locked {
  opacity: 0.5;
}
.empty-hint {
  display: block;
  margin-top: 8px;
}
.add-btn {
  margin-top: 8px;
}
```

- [ ] **Step 5: Wire `Profiles.vue` — delete its duplicated header block and bind the new props**

In `src/views/Profiles.vue`, delete these lines from the modal (the `NDivider` + the master-switch `NSpace`):

```vue
        <NDivider style="margin: 8px 0">时段策略</NDivider>
        <NSpace align="center" justify="space-between">
          <NSwitch v-model:value="form.strategies_enabled">
            <template #checked>已启用</template>
            <template #unchecked>已停用</template>
          </NSwitch>
          <span class="muted">{{ form.strategies_enabled ? "按策略时段路由" : "已暂停策略，所有请求将直接走默认模型" }}</span>
        </NSpace>
```

Then replace the `<StrategyEditor>` usage:

```vue
        <StrategyEditor v-model="form.strategies" :model-options="modelOptions" id-scope="route" />
```

with:

```vue
        <StrategyEditor
          v-model="form.strategies"
          v-model:master-enabled="form.strategies_enabled"
          :model-options="modelOptions"
          :scope-label="'路由'"
          id-scope="route"
        />
```

- [ ] **Step 6: Wire `Fallback.vue` — same deletion, scope `'转移'`**

In `src/views/Fallback.vue`, delete these lines from the modal:

```vue
        <NDivider style="margin: 8px 0">时段策略</NDivider>
        <NSpace align="center" justify="space-between">
          <NSwitch v-model:value="form.strategies_enabled">
            <template #checked>已启用</template>
            <template #unchecked>已停用</template>
          </NSwitch>
          <span class="muted">{{ form.strategies_enabled ? "按策略时段转移" : "已暂停策略，仅走默认转移目标" }}</span>
        </NSpace>
```

Then replace the `<StrategyEditor>` usage:

```vue
        <StrategyEditor v-model="form.strategies" :model-options="strategyModelOptions" id-scope="failover" />
```

with:

```vue
        <StrategyEditor
          v-model="form.strategies"
          v-model:master-enabled="form.strategies_enabled"
          :model-options="strategyModelOptions"
          :scope-label="'转移'"
          id-scope="failover"
        />
```

- [ ] **Step 7: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS, no errors. If it complains `masterEnabled` is missing on a `<StrategyEditor>` usage, that caller wasn't wired (fix per Steps 5-6).

- [ ] **Step 8: Manual verification**

Run: `pnpm tauri dev`. Open the **Routes** page → edit a route → in the modal confirm:
- Exactly one "时段策略" header row: title on the left, "启用时段策略" + switch on the right, a one-line hint beneath ("按策略时段路由" when on / "已暂停策略，所有请求将直接走默认模型" when off).
- Flipping the master switch off dims the whole strategy list (cards + "添加策略" button) to 50% opacity.
- With master off, click around the dimmed area and **Tab** through the modal — focus must not land on any dimmed control (`inert` skips them). The master switch itself stays focusable/operable.
- Flipping it back on restores full interactivity.

Repeat in the **故障转移** page → edit a model's failover config: same checks, but the hint reads "按策略时段转移" / "已暂停策略，仅走默认转移目标".

- [ ] **Step 9: Commit**

```bash
git add src/components/StrategyEditor.vue src/views/Profiles.vue src/views/Fallback.vue
git commit -m "refactor(strategy-editor): lift 时段策略 master toggle into component" -m "Profiles.vue + Fallback.vue drop their duplicated header/master-switch blocks; new v-model:master-enabled + scopeLabel props; master-off locks the list via :inert + opacity."
```

---

### Task 2: Per-card clarity — switch to header-right, lock priority + body when card off

Move the per-card enable switch off the "优先级" label and disable the priority select + lock the body when the card is off. Component-only.

**Files:**
- Modify: `src/components/StrategyEditor.vue` (card `v-for` body ~lines 88-154; `<style>`)

**Interfaces:**
- Consumes: `StrategyForm.enabled` and `StrategyForm.priority` (unchanged), the existing `patch(id, partial)` helper.
- Produces: no API change; same `update:modelValue` contract.

- [ ] **Step 1: Restructure the card header row**

Replace the card's current opening `<NSpace>` (the one containing the enable switch + "优先级" + priority select + delete button):

```vue
    <NSpace align="center" justify="space-between">
      <NSpace align="center" :size="8">
        <NSwitch :value="s.enabled" size="small" @update:value="(v: boolean) => patch(s.id, { enabled: v })" />
        <span class="muted">优先级<span class="hint">（数字越小优先级越高）</span></span>
        <NSelect
          :value="s.priority"
          size="small"
          style="width: 70px"
          :options="Array.from({ length: 10 }, (_, i) => ({ label: String(i + 1), value: i + 1 }))"
          @update:value="(v: number) => patch(s.id, { priority: v })"
        />
      </NSpace>
      <NButton size="tiny" type="error" ghost @click="removeStrategy(s.id)">删除</NButton>
    </NSpace>
```

with a `.card-head` that puts priority on the left and the enable switch + delete on the right (priority select gains `:disabled="!s.enabled"`):

```vue
    <div class="card-head">
      <NSpace align="center" :size="8">
        <span class="muted">优先级<span class="hint">（数字越小优先级越高）</span></span>
        <NSelect
          :value="s.priority"
          :disabled="!s.enabled"
          size="small"
          style="width: 70px"
          :options="Array.from({ length: 10 }, (_, i) => ({ label: String(i + 1), value: i + 1 }))"
          @update:value="(v: number) => patch(s.id, { priority: v })"
        />
      </NSpace>
      <NSpace align="center" :size="8">
        <NSwitch
          :value="s.enabled"
          size="small"
          aria-label="启用该策略"
          @update:value="(v: boolean) => patch(s.id, { enabled: v })"
        />
        <NButton size="tiny" type="error" ghost @click="removeStrategy(s.id)">删除</NButton>
      </NSpace>
    </div>
```

- [ ] **Step 2: Wrap the three body rows in a lockable `.card-body`**

The three `<div class="row">` blocks (重复 / 时间 / 调用模型) are currently loose children of `.strategy-card`. Wrap them in a `.card-body` that is `inert` + dimmed when the card is off. Replace those three rows with:

```vue
    <div class="card-body" :class="{ 'is-off': !s.enabled }" :inert="!s.enabled">
      <div class="row">
        <span class="muted">重复</span>
        <NSpace :size="4" align="center">
          <NButton
            v-for="d in 7"
            :key="d"
            size="tiny"
            :type="s.days_of_week.includes(d) ? 'primary' : 'default'"
            @click="toggleDay(s, d)"
          >{{ "一二三四五六日"[d - 1] }}</NButton>
          <NButton size="tiny" quaternary @click="setWeekdays(s, [1, 2, 3, 4, 5])">工作日</NButton>
          <NButton size="tiny" quaternary @click="setWeekdays(s, [1, 2, 3, 4, 5, 6, 7])">全选</NButton>
        </NSpace>
      </div>

      <div class="row">
        <span class="muted">时间</span>
        <NTimePicker
          :value="minToTs(s.time_start)"
          format="HH:mm"
          size="small"
          style="width: 110px"
          @update:value="(v: number | null) => patch(s.id, { time_start: tsToMin(v) })"
        />
        <span class="muted">到</span>
        <NTimePicker
          :value="minToTs(s.time_end)"
          format="HH:mm"
          size="small"
          style="width: 110px"
          @update:value="(v: number | null) => patch(s.id, { time_end: tsToMin(v) })"
        />
        <NTag v-if="isAllDay(s)" type="success" size="small" round>🕐 全天</NTag>
        <NTag
          v-else-if="isCrossNight(s)"
          type="warning"
          size="small"
          round
        >🌙 已跨夜 (于次日 {{ String(Math.floor(s.time_end / 60)).padStart(2, "0") }}:{{ String(s.time_end % 60).padStart(2, "0") }} 结束)</NTag>
      </div>

      <div class="row">
        <span class="muted">调用模型</span>
        <NSelect
          :value="s.model_id"
          :options="modelOptions"
          :render-label="ellipsisLabel"
          size="small"
          placeholder="选择 Model"
          @update:value="(v: string) => patch(s.id, { model_id: v })"
        />
      </div>
    </div>
```

- [ ] **Step 3: Add the card layout CSS**

Append inside `<style scoped>`:

```css
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.card-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.card-body.is-off {
  opacity: 0.5;
}
```

(`.strategy-card` already has `display:flex; flex-direction:column; gap:8px`, so `.card-head` and `.card-body` stack with the right spacing.)

- [ ] **Step 4: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS.

- [ ] **Step 5: Manual verification**

Run: `pnpm tauri dev`. In either modal, with the master toggle **on**:
- Each card's enable switch is at the top-right next to "删除"; "优先级" + its select are at the top-left (no longer adjacent to the switch).
- Flip one card's switch off: that card's priority select becomes disabled (Naive disabled style) **and** its 重复/时间/调用模型 rows dim to 50% and become non-interactive. The card's own switch and "删除" stay operable.
- Tab through an off-card: focus skips the dimmed body rows and the disabled priority select, but can still reach the card's switch + delete.
- Other cards are unaffected. Flipping the card back on restores everything.
- Then flip the master off: the whole list (all cards incl. their switches/deletes) dims + locks; Tab skips all of it; the master switch remains operable.

- [ ] **Step 6: Commit**

```bash
git add src/components/StrategyEditor.vue
git commit -m "feat(strategy-editor): per-card toggle clarity + inert locking" -m "Enable switch moves to card header-right (off the 优先级 label); priority select disabled and body :inert+dimmed when a card is off."
```

---

### Task 3: Auto-increment default priority + empty-state call-to-action

Two small polish items from the spec (Decisions 5 & 6): new strategies get a non-colliding priority, and an empty list shows a clearer hint. Component-only.

**Files:**
- Modify: `src/components/StrategyEditor.vue` (`blankStrategy` ~lines 31-42; empty-state `<span>` added in Task 1 Step 3)

**Interfaces:**
- Consumes/produces: nothing new externally. Adds one internal pure helper.

- [ ] **Step 1: Add `nextPriority()` and use it in `blankStrategy()`**

In `<script setup>`, add the helper just above `blankStrategy` and change the `priority` line. Replace:

```ts
function blankStrategy(): StrategyForm {
  return {
    // idScope namespaces the genId prefix so route vs failover ids never collide.
    id: genId(`${props.idScope}_s`, props.modelValue.map((s) => s.id)),
    priority: 1,
    enabled: true,
    days_of_week: [1, 2, 3, 4, 5],
    time_start: 22 * 60,
    time_end: 8 * 60,
    model_id: props.modelOptions[0]?.value ?? "",
  };
}
```

with:

```ts
// New strategies default to the next free priority so several in a row don't
// all collide at 1. Capped at 10 (the dropdown max); user can still edit it.
function nextPriority(): number {
  const max = props.modelValue.reduce((m, s) => Math.max(m, s.priority), 0);
  return Math.min(10, max + 1);
}
function blankStrategy(): StrategyForm {
  return {
    // idScope namespaces the genId prefix so route vs failover ids never collide.
    id: genId(`${props.idScope}_s`, props.modelValue.map((s) => s.id)),
    priority: nextPriority(),
    enabled: true,
    days_of_week: [1, 2, 3, 4, 5],
    time_start: 22 * 60,
    time_end: 8 * 60,
    model_id: props.modelOptions[0]?.value ?? "",
  };
}
```

- [ ] **Step 2: Update the empty-state copy**

Replace the empty-state span (the one placed inside `.strategy-list` in Task 1 Step 3):

```vue
    <span v-if="!modelValue.length" class="muted empty-hint">暂无策略</span>
```

with:

```vue
    <span v-if="!modelValue.length" class="muted empty-hint">暂无时段策略，点击下方「添加策略」开始配置</span>
```

- [ ] **Step 3: Type-check**

Run: `pnpm exec vue-tsc --noEmit`
Expected: PASS.

- [ ] **Step 4: Manual verification + full regression**

Run: `pnpm tauri dev`. In both the route and failover modals:
- With an empty strategy list (master on): the call-to-action "暂无时段策略，点击下方「添加策略」开始配置" shows above the add button.
- Click "添加策略" three times: the three new cards get priority **1, 2, 3** automatically (not 1, 1, 1).
- Delete the middle one and add again: the new card takes `max remaining + 1`.
- Regression pass (both modals): master off dims+locks everything (Tab skips); per-card off dims+locks that card's priority + body (Tab skips body, switch/delete work); header hint copy is correct per context; save persists `strategies_enabled` and per-strategy `enabled`/`priority` as before.

- [ ] **Step 5: Commit**

```bash
git add src/components/StrategyEditor.vue
git commit -m "feat(strategy-editor): auto-increment default priority + empty-state CTA"
```

---

## Self-Review (completed during authoring)

**Spec coverage:**
- Decision 1 (lift master toggle into component) → Task 1. ✓
- Decision 2 (section header: title left, switch right, slotless, scoped subtitle beneath) → Task 1 Steps 2 & 5-6. ✓
- Decision 3 (disabled = non-editable; `:inert` + opacity; master off locks list, per-card off locks priority+body) → Task 1 Step 2 (master `:inert`), Task 2 Steps 1-2 (priority `:disabled` + body `:inert`). ✓
- Decision 4 (per-card switch to header-right; priority disabled when off) → Task 2 Step 1. ✓
- Decision 5 (priority auto-increment, cap 10) → Task 3 Step 1. ✓
- Decision 6 (empty-state call-to-action) → Task 3 Step 2. ✓
- Verification (type-check + manual incl. keyboard Tab) → every task + Task 3 Step 4 regression. ✓

**Placeholder scan:** none — every code step contains the actual code; commit steps use explicit paths.

**Type/name consistency:** prop `masterEnabled` + emit `update:masterEnabled` + parent binding `v-model:master-enabled` match across Task 1. `scopeLabel` default `"路由"` (Profiles explicit `'路由'`, Fallback `'转移'`) matches the hint branching. `nextPriority()`, `.card-head`, `.card-body.is-off`, `.strategy-list.is-locked`, `.empty-hint`, `.add-btn` names are consistent across tasks.
