# Zhipu Plan Info (validity + auto-renew) — Design

**Date:** 2026-08-03
**Status:** Implemented
**Related:** `2026-08-02-bailian-usage-adapter-design.md` (same `PlanInfo` best-effort subscription pattern, adapted to Zhipu).

## Overview

The Zhipu adapter (`usage/zhipu.rs`) queried only usage tiers + `level` from `/api/monitor/usage/quota/limit` and always left `plan_info = None` — so the智谱 card had no validity / auto-renew tooltip, unlike 火山 and 百炼. This adds a **best-effort third call** to the console subscription-list API to populate `plan_info` (有效期 + 自动续费), mirroring the Bailian subscription step.

## The endpoint

`GET https://bigmodel.cn/api/biz/subscription/list` — a console-internal API (not the inference gateway). Auth is the **raw api token** in the `authorization` header (no `Bearer` prefix), the same token used for inference + the usage endpoint. Confirmed-working curl + sample response supplied by the user.

Note the host differs from the inference base_url: the subscription API is on the **console host** `bigmodel.cn`, while `base_url` points at the API-gateway host `open.bigmodel.cn`. `subscription_url(base_url)` derives it by stripping a leading `open.` subdomain:

- `https://open.bigmodel.cn/api/paas/v4` → `https://bigmodel.cn/api/biz/subscription/list`
- (no `open.` prefix → host used as-is, best-effort for other hosts)

## Response → `PlanInfo`

`data` is an **array** of subscriptions (an account may hold several). Selection, in priority order:
1. `status == "VALID"` **and** `inCurrentPeriod == true`
2. `status == "VALID"` (first such)
3. `data[0]`

From the picked entry:
- **`valid`** — a range string `"YYYY-MM-DD HH:MM:SS-YYYY-MM-DD HH:MM:SS"` (two fixed-width 19-char timestamps joined by `-` at byte index 19). Split by position: `start_time = valid[..19]`, `end_time = valid[20..]`; convert the space to `T` for ISO 8601 (`"2027-02-01T10:00:00"`).
- **`autoRenew`** — `0`/`1` int → `bool` (`!= 0`); a JSON bool is also tolerated.

Worked example (real response): `valid:"2027-02-01 10:00:00-2028-02-01 10:00:00"`, `autoRenew:0` → `start_time "2027-02-01T10:00:00"`, `end_time "2028-02-01T10:00:00"`, `auto_renew Some(false)`.

### Why no timezone is invented

`PlanInfo.start_time`/`end_time` are **display-only**: the frontend (`Usage.vue` `fmtPlanTime`) slices the first 16 chars (`T`→space) and never parses them back to epoch (only `reset_at` round-trips through `iso_to_epoch_secs`). The Zhipu source timestamps carry no timezone, so none is added — the `T`-substituted local datetime displays identically and avoids an unverified `+08:00` claim.

### The `plan` label is unchanged

The card-header plan tag still comes from the usage endpoint's `data.level` (e.g. `"lite"`/`"max"`) — tier-only, per the all-vendor label alignment. The subscription call enriches **only** `plan_info`, not `plan`.

## Best-effort semantics

`enrich_plan_info` is called after the (required) usage query succeeds with non-empty tiers. Any failure — network error, non-2xx, JSON parse error, empty `data`, or no entry with a usable `valid`/`autoRenew` — leaves `plan_info = None`; the usage tiers and `plan` label are untouched. The degraded raw-fallback path (empty tiers → `raw_summary`) does not attempt the subscription call. This matches Bailian's subscription step and 火山's `GetPersonalPlan`.

Cost: every usage refresh now makes one extra lightweight GET (consistent with Bailian/火山, which also fetch subscription metadata each query).

## Auth note

The usage endpoint (`/api/monitor/usage/quota/limit`) uses `Authorization: Bearer <token>`; the subscription endpoint uses `Authorization: <token>` (raw), matching the confirmed-working console curl. Both are accepted by the same backend; the difference is preserved verbatim rather than normalized, to maximize first-try fidelity to the captured request.

## Testing

- `parse_plan_info` pure unit tests: real-response shape → start/end/auto_renew; `autoRenew:1` → true; multi-entry VALID+current selection; empty/missing → `None`.
- `subscription_url` derivation (strips `open.`; passthrough otherwise).
- wiremock e2e: usage + subscription both mocked → `plan_info` populated, `plan` = level.
- wiremock e2e: subscription 500 → `plan_info = None`, usage tiers intact (best-effort).
- Existing Zhipu tests unchanged (the extra subscription GET hits an unmocked path → 404 → `plan_info` stays `None`; no assertion affected).
