# Usage Statistics Tracking Design Specification

**Date:** 2026-08-14  
**Status:** Approved (revised through 9 review rounds)  
**Version:** 1.9

## Overview

This specification defines a comprehensive usage statistics tracking system for SwitchLM that monitors and displays per-provider request counts and token consumption. The system provides real-time visibility into LLM usage across multiple time windows while gracefully handling missing data from vendor APIs.

### Requirements Summary

- Track request counts and token usage per provider
- Support both plan-based providers (智谱, 火山) and consumption-based providers (DeepSeek)
- Implement time-windowed statistics (5h, 1w, 1m) with automatic reset
- Display total tokens with hover breakdown of input/output tokens
- Graceful fallback when vendor APIs don't return usage data
- Non-blocking integration - statistics never affect request processing

## Architecture

### Core Components

```
┌─────────────────────────────────────────────────────────────┐
│                    Proxy Dispatch Layer                      │
│                   (src-tauri/src/proxy/)                     │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│              Usage Statistics Service                        │
│            (src-tauri/src/statistics/)                       │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  • Token extraction (non-stream + streaming)         │  │
│  │  • Window management (5h/1w/1m reset logic)          │  │
│  │  • Database operations (SQLite)                       │  │
│  │  • Error handling and fallback                       │  │
│  └──────────────────────────────────────────────────────┘  │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                  SQLite Database                             │
│              (switchlm.db — app-wide DB)                      │
└─────────────────────────────────────────────────────────────┘
```

### Service Integration

The statistics service integrates into the existing proxy flow at two key points:

1. **Token extraction at body-buffering points** - Pure functions on already-buffered upstream response data (never consuming the forwarded stream)

## Data Model

### Database Schema

```sql
CREATE TABLE usage_statistics (
    provider_id TEXT PRIMARY KEY,
    
    -- Plan-based providers: time-windowed counters.
    -- tokenized_requests = requests whose response carried token usage data
    -- (drives the derived DataQuality; see Core Data Structures).
    last_5h_requests INTEGER DEFAULT 0,
    last_5h_tokenized_requests INTEGER DEFAULT 0,
    last_5h_input_tokens INTEGER DEFAULT 0,
    last_5h_output_tokens INTEGER DEFAULT 0,
    last_5h_reset_at INTEGER,
    
    last_1w_requests INTEGER DEFAULT 0,
    last_1w_tokenized_requests INTEGER DEFAULT 0,
    last_1w_input_tokens INTEGER DEFAULT 0,
    last_1w_output_tokens INTEGER DEFAULT 0,
    last_1w_reset_at INTEGER,
    
    last_1m_requests INTEGER DEFAULT 0,
    last_1m_tokenized_requests INTEGER DEFAULT 0,
    last_1m_input_tokens INTEGER DEFAULT 0,
    last_1m_output_tokens INTEGER DEFAULT 0,
    last_1m_reset_at INTEGER,
    
    -- All providers: lifetime totals
    total_requests INTEGER DEFAULT 0,
    total_tokenized_requests INTEGER DEFAULT 0,
    total_input_tokens INTEGER DEFAULT 0,
    total_output_tokens INTEGER DEFAULT 0,
    
    -- Consumption-based providers
    total_tokens INTEGER DEFAULT 0,
    
    -- Metadata
    vendor TEXT NOT NULL,
    provider_type TEXT NOT NULL, -- 'plan' or 'consumption'
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_usage_vendor ON usage_statistics(vendor);
CREATE INDEX idx_usage_updated ON usage_statistics(updated_at);
```

**No foreign key to `providers`.** Provider configuration lives in `app_config.json`, not in this SQLite file — an FK would reference a table that doesn't exist in this database (and its `ON DELETE CASCADE` would be meaningless). Instead, provider deletion is handled explicitly: the Tauri command that removes a provider (`delete_provider` in `commands.rs`) also sends a `StatEvent::DeleteProvider { provider_id }` to the writer thread, which drops the statistics row. A stale row that somehow survives (e.g. crash between the two) is harmless: the UI joins stats to the provider list, so orphan rows are simply never rendered.

**No `data_quality` column.** Data quality is **derived at read time** from `tokenized_requests` vs `requests` — a stored quality flag would only reflect the last request and could drift out of sync with the counters. Derivation also exposes *partial* coverage (e.g. streaming degraded but non-streaming fine), which a single enum cannot represent:

| Condition (per scope) | Derived quality |
|---|---|
| `tokenized == 0 && requests == 0` | `Unknown` (no traffic yet) |
| `tokenized == 0 && requests > 0` | `RequestsOnly` |
| `tokenized > 0 && tokenized < requests` | `Full` **with coverage `tokenized/requests`** (partial — UI can badge it) |
| `tokenized == requests > 0` | `Full` (complete) |

### Core Data Structures

```rust
pub struct ProviderStats {
    pub provider_id: String,
    pub vendor: String,
    pub provider_type: ProviderType,
    
    // Time-windowed stats
    pub last_5h: WindowStats,
    pub last_1w: WindowStats,
    pub last_1m: WindowStats,
    
    // Lifetime totals
    pub total_requests: u64,
    pub total_tokenized_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
}

pub struct WindowStats {
    pub requests: u64,
    pub tokenized_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reset_at: Option<i64>,
    pub is_active: bool,
    
    // Derived at read time from tokenized_requests/requests:
    pub quality: DataQuality,
    /// Some(pct) when 0 < tokenized < requests (partial coverage — the UI
    /// badges it, e.g. "83% 的请求有token数据").
    pub coverage_pct: Option<f64>,
}

pub enum ProviderType {
    Plan,        // 智谱, 火山 - time-windowed quotas
    Consumption, // DeepSeek - pay-as-you-go
}

pub enum DataQuality {
    Full,         // >= 1 request carried token data
    RequestsOnly, // requests > 0, none carried token data
    Unknown,      // no traffic recorded yet (requests == 0)
}

impl WindowStats {
    /// Pure derivation — no stored quality flag to keep in sync.
    pub fn derive_quality(requests: u64, tokenized: u64) -> (DataQuality, Option<f64>) {
        match (requests, tokenized) {
            (0, _) => (DataQuality::Unknown, None),
            (_, 0) => (DataQuality::RequestsOnly, None),
            (r, t) if t == r => (DataQuality::Full, None),
            (r, t) => (DataQuality::Full, Some(t as f64 / r as f64 * 100.0)),
        }
    }
}
```

**Quality is per-window (and per-lifetime), not per-provider.** The UI renders each window row separately, and a provider can legitimately mix states — e.g. the newly-armed monthly window has traffic but no tokenized responses yet while the 5h window is fully tokenized. `WindowStats.quality` and the lifetime counters answer the display layer's question at exactly the scope it renders.

**`is_active` rule:** `reset_at.is_some()` — a window is active once a usage query has reported its cycle boundary (arming it via the reset mechanism). NULL `reset_at` means the provider's plan has no such window (or it hasn't been observed yet); its row renders as inactive/N/A and its counters are never incremented (the upsert's `CASE WHEN ... IS NOT NULL` guard).

## Token Extraction Strategy

### Non-Streaming Responses

**OpenAI Protocol:**
```json
{
  "usage": {
    "prompt_tokens": 15,      // Input tokens
    "completion_tokens": 30,  // Output tokens  
    "total_tokens": 45
  }
}
```

**Anthropic Protocol:**
```json
{
  "usage": {
    "input_tokens": 25,     // Input tokens
    "output_tokens": 50     // Output tokens
  }
}
```

### Streaming Responses

**OpenAI Streaming:** upstream returns usage **only if** the request carries `stream_options: {include_usage: true}` — the proxy injects this (see *stream_options injection* below); third-party clients (NextChat, Cherry Studio, Claude Code …) never send it themselves
```json
// Final chunk with empty choices array
{
  "choices": [],
  "usage": {
    "prompt_tokens": 15,
    "completion_tokens": 30,
    "total_tokens": 45
  }
}
```

**Anthropic Streaming:** Tokens split across events
```javascript
// message_start event (input tokens)
event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}

// message_delta event (output tokens)
event: message_delta  
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}
```

### Extraction Implementation

**⚠️ CRITICAL: Never consume the response body stream in the statistics service!**

HTTP response bodies in the proxy are **single-consumption streams**. Calling `response.bytes().await` inside a statistics function (even via `&Response<Body>`) would drain the stream and leave the proxy with an empty body to forward — the client gets HTTP 200 with no content (a silent failure). Additionally, if the proxy mutates the body before forwarding (e.g. rewrites the echoed `model` field), any post-hoc extraction on the forwarded response would see mutated data.

**Correct design: extraction happens at the existing body-buffering points, as a pure function on already-buffered bytes/JSON.**

#### Protocol note: use the *upstream* protocol, not the client protocol

Token extraction must be attached where the **upstream response** is parsed, keyed by the upstream protocol:
- Client speaks OpenAI + upstream OpenAI (passthrough) → OpenAI `usage` paths
- Client speaks Anthropic + upstream Anthropic (passthrough) → Anthropic `usage` paths
- Client speaks Anthropic + upstream OpenAI (translate) → the upstream body is OpenAI format, so extract **OpenAI paths** there (even though the client receives Anthropic)

```rust
// ✅ Pure function: no I/O, no stream consumption, no error type needed.
// Missing/unparseable fields degrade to (None, None) — the caller records
// request-count-only (fallback strategy).
fn extract_tokens_from_json(
    body: &serde_json::Value,
    upstream_protocol: ClientProtocol,
) -> (Option<u64>, Option<u64>) {
    let usage = &body["usage"];
    let (input, output) = match upstream_protocol {
        ClientProtocol::OpenAI => (
            usage["prompt_tokens"].as_u64(),
            usage["completion_tokens"].as_u64(),
        ),
        ClientProtocol::Anthropic => (
            usage["input_tokens"].as_u64(),
            usage["output_tokens"].as_u64(),
        ),
    };
    match (input, output) {
        // Normal case: both halves present.
        (i @ Some(_), o @ Some(_)) => (i, o),
        // Non-standard vendor: only total_tokens. Charge it all as input with
        // output = 0 so the *displayed* total (input + output) stays exact;
        // the hover breakdown is imprecise for such vendors — acceptable.
        (None, None) => match usage["total_tokens"].as_u64() {
            Some(total) => (Some(total), Some(0)),
            None => (None, None),
        },
        // Half present (rare): keep what we have.
        partial => partial,
    }
}
```

#### Non-streaming integration point

The existing non-stream handlers (`openai_passthrough`, `anthropic_passthrough`, `translate_via_openai`) already buffer the full body (`resp.bytes().await`) to run rate-limit checks and rewrite the echoed `model`. Extraction piggybacks on that same buffer — zero extra reads:

```rust
async fn openai_passthrough(...) -> Result<(AttemptOutcome, TokenUsage), ProxyError> {
    // ... send upstream ...
    let bytes = resp.bytes().await?;                 // already buffered today
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let tokens = extract_tokens_from_json(&v, ClientProtocol::OpenAI); // ← extract here
    // ... build Response from `bytes` as today — full body forwarded ✅ ...
    Ok((outcome, tokens))
}
```

The `TokenUsage` value flows out through the attempt/fallback walk alongside `served_model_id`, so the statistics record credits the provider that actually served the request.

#### Streaming integration point (tap, never consume)

Streaming extraction observes events **in the forwarding pipeline** without detouring them:

- **Translate path**: `StreamTranslator` already parses every SSE chunk. Feed the same parsed chunk to a `StreamingTokenCollector` alongside the translator — zero extra parsing.
- **Passthrough path**: forwarding is raw bytes. Wrap the stream so each chunk is forwarded **and** fed to the collector; when the stream ends, the collector's result is handed to the statistics service (via a captured `Arc<Mutex<Option<TokenUsage>>>` or oneshot channel set by the stream's final poll).

```rust
// Streaming token collector — fed by the forwarding pipeline, never reads the stream itself
pub struct StreamingTokenCollector {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    upstream_protocol: ClientProtocol,
}

impl StreamingTokenCollector {
    /// Feed one parsed upstream SSE chunk. Returns true when the final usage
    /// values have arrived (OpenAI: a chunk carrying usable usage numbers;
    /// Anthropic: message_delta). Cheap no-op after completion.
    pub fn ingest(&mut self, chunk: &serde_json::Value) -> bool {
        match self.upstream_protocol {
            // OpenAI spec puts usage in a final chunk with empty choices, but
            // some non-standard gateways attach usage to the LAST CONTENT chunk
            // (choices non-empty). Accept usage wherever it appears — only
            // require that at least one number is extractable. (Mid-stream
            // chunks with "usage": null pass the get() but yield None from
            // as_u64(), so they don't falsely complete the collector.)
            ClientProtocol::OpenAI => {
                if let Some(usage) = chunk.get("usage") {
                    let prompt = usage["prompt_tokens"].as_u64();
                    let completion = usage["completion_tokens"].as_u64();
                    if prompt.is_some() || completion.is_some() {
                        self.input_tokens = prompt;
                        self.output_tokens = completion;
                        return true;
                    }
                }
                false
            }
            ClientProtocol::Anthropic => {
                match chunk.get("type").and_then(|t| t.as_str()) {
                    Some("message_start") => {
                        self.input_tokens = chunk["message"]["usage"]["input_tokens"].as_u64();
                    }
                    Some("message_delta") => {
                        self.output_tokens = chunk["usage"]["output_tokens"].as_u64();
                        return self.input_tokens.is_some(); // complete when both present
                    }
                    _ => {}
                }
                false
            }
        }
    }
}
```

#### stream_options injection (OpenAI-family upstreams, streaming)

Third-party clients (NextChat, Cherry Studio, plain SDK scripts) almost never send `stream_options` — if the proxy forwarded requests verbatim, **every OpenAI-family streaming response would arrive without a usage chunk and degrade to RequestsOnly**. So the proxy must inject it itself, with merge semantics:

```rust
// In send_stream(), for StreamPath::OpenAiPassthrough and StreamPath::Translate
// (i.e. whenever the UPSTREAM speaks OpenAI) — before serialization:
// Defensive form: never indexes fwd mutably (a malformed non-object request body
// would panic on IndexMut); go through as_object_mut explicitly.
if let Some(obj) = fwd.as_object_mut() {
    let mut opts = obj
        .get("stream_options")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();         // preserve any client-set options
    opts.insert("include_usage".to_string(), serde_json::Value::Bool(true));
    obj.insert("stream_options".to_string(), serde_json::Value::Object(opts));
}
// (If fwd is not an object the request is malformed and forwarding fails
// downstream anyway — statistics simply don't inject into it.)
```

Injection rules:
- **Merge, never overwrite** — a client-supplied `stream_options` object keeps its other keys; only `include_usage` is forced to `true`. (If the client explicitly sent `false`, we still force `true`: statistics are a proxy-side feature, and the client cannot opt the proxy out of them.)
- **Only for OpenAI-family upstreams** — never injected into `StreamPath::AnthropicPassthrough` (Anthropic has no `stream_options`; unknown fields risk 400s).
- **Injected on the upstream request only** — the forwarded client response is untouched (the usage chunk with empty `choices` is consumed by the tap; the translator must silently ignore empty-`choices` chunks rather than treat them as malformed).
- **Compatibility risk**: backends that reject unknown request fields would 400. All currently supported OpenAI-compatible vendors (智谱, DeepSeek, 火山, 千问) accept `stream_options`. If an exotic backend breaks, the failure surfaces as a normal passthrough error — visible, not silent — and the vendor can be excluded via a per-vendor capability flag (`vendor_capability.rs`) if ever needed.

When no usage chunk arrives (vendor doesn't support it, stream aborted), the collector stays incomplete → record request-count-only (fallback strategy, see below).

## Window Management

### Time Window Strategy

- **Plan-based providers** (智谱, 火山): Track all three windows (5h, 1w, 1m)
- **Consumption providers** (DeepSeek): Only track lifetime totals
- **Reset driven by usage polling**: a changed `reset_at` in the usage API response signals a new window cycle and zeroes that window's counters (~60s latency, same cadence as the quota UI)
- **Inactive windows**: Providers without reset times for a window show as inactive

### Reset Logic

**⚠️ CRITICAL: `reset_at` must be the fixed plan cycle time from usage queries, never updated to current time!**

The `reset_at` values are fetched from the vendor's usage API and represent the actual plan reset times (e.g., 14:00 today for 5h windows). These should only be updated when the usage query returns a new reset time, not when counters are reset.

**Reset is fully decoupled from recording:** `record_request` is a *pure accumulation* path with no reset checks at all. Window resets happen exclusively in `update_reset_times_from_usage` (driven by the ~60s usage polling), when the API reports a **changed** `reset_at` — see *Reset At Update Mechanism* below. This eliminates both the infinite-reset-loop bug (reset_at being written from `now`) and read-then-write races between concurrent requests.

### Recording: single atomic UPSERT

A bare `UPDATE ... WHERE provider_id = ?` silently affects **0 rows** for a brand-new provider (no row exists yet), so statistics would be lost forever. And a SELECT-then-UPDATE sequence per window is non-atomic: concurrent requests can race, and a mid-sequence failure leaves partial increments.

Both are solved with **one atomic `INSERT ... ON CONFLICT DO UPDATE`** statement:

- **New provider** → the `INSERT` branch creates the row (first request counted immediately)
- **Existing provider** → the `DO UPDATE` branch increments in place
- **Inactive windows** (`reset_at IS NULL`) → a `CASE WHEN` guard skips that window's counters
- SQLite executes a single statement atomically under its write lock — no explicit transaction needed

```rust
pub fn record_request(
    &self,
    provider_id: &str,
    vendor: &str,
    provider_type: ProviderType,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    now: i64,
) -> SqliteResult<()> {
    let (input, output) = (input_tokens.unwrap_or(0), output_tokens.unwrap_or(0));
    let has_token_data = input_tokens.is_some() || output_tokens.is_some();
    // ?8 in the INSERT branch — window activity. NULL reset_at ⇒ not yet armed
    // by a usage poll ⇒ window counters stay 0 (total_* still capture everything).
    let windows_armed = false;
    // Runs on the dedicated writer thread, which owns `conn` exclusively
    // (no Mutex needed — single-threaded access by construction).
    conn.execute(
        "INSERT INTO usage_statistics (
            provider_id, vendor, provider_type,
            last_5h_requests, last_5h_tokenized_requests, last_5h_input_tokens, last_5h_output_tokens,
            last_1w_requests, last_1w_tokenized_requests, last_1w_input_tokens, last_1w_output_tokens,
            last_1m_requests, last_1m_tokenized_requests, last_1m_input_tokens, last_1m_output_tokens,
            total_requests, total_tokenized_requests, total_input_tokens, total_output_tokens, total_tokens,
            updated_at
        ) VALUES (
            ?1, ?2, ?3,
            -- window columns are ALL guarded by ?8 (window activity): without it,
            -- a first request before the usage poll arms the window would write
            -- requests=0 with input_tokens=1000 (and tokenized>requests) —
            -- contradictory data.
            CASE WHEN ?8 THEN 1 ELSE 0 END, CASE WHEN ?8 AND ?4 THEN 1 ELSE 0 END, CASE WHEN ?8 THEN ?5 ELSE 0 END, CASE WHEN ?8 THEN ?6 ELSE 0 END, -- 5h
            CASE WHEN ?8 THEN 1 ELSE 0 END, CASE WHEN ?8 AND ?4 THEN 1 ELSE 0 END, CASE WHEN ?8 THEN ?5 ELSE 0 END, CASE WHEN ?8 THEN ?6 ELSE 0 END, -- 1w
            CASE WHEN ?8 THEN 1 ELSE 0 END, CASE WHEN ?8 AND ?4 THEN 1 ELSE 0 END, CASE WHEN ?8 THEN ?5 ELSE 0 END, CASE WHEN ?8 THEN ?6 ELSE 0 END, -- 1m
            1, ?4, ?5, ?6, ?5 + ?6,
            ?7
        )
        ON CONFLICT(provider_id) DO UPDATE SET
            vendor = ?2,               -- vendor/type may have been edited in config
            provider_type = ?3,
            last_5h_requests      = last_5h_requests + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL THEN 1 ELSE 0 END,
            last_5h_tokenized_requests = last_5h_tokenized_requests + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL AND ?4 THEN 1 ELSE 0 END,
            last_5h_input_tokens  = last_5h_input_tokens + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL THEN ?5 ELSE 0 END,
            last_5h_output_tokens = last_5h_output_tokens + CASE WHEN usage_statistics.last_5h_reset_at IS NOT NULL THEN ?6 ELSE 0 END,
            last_1w_requests      = last_1w_requests + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL THEN 1 ELSE 0 END,
            last_1w_tokenized_requests = last_1w_tokenized_requests + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL AND ?4 THEN 1 ELSE 0 END,
            last_1w_input_tokens  = last_1w_input_tokens + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL THEN ?5 ELSE 0 END,
            last_1w_output_tokens = last_1w_output_tokens + CASE WHEN usage_statistics.last_1w_reset_at IS NOT NULL THEN ?6 ELSE 0 END,
            last_1m_requests      = last_1m_requests + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL THEN 1 ELSE 0 END,
            last_1m_tokenized_requests = last_1m_tokenized_requests + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL AND ?4 THEN 1 ELSE 0 END,
            last_1m_input_tokens  = last_1m_input_tokens + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL THEN ?5 ELSE 0 END,
            last_1m_output_tokens = last_1m_output_tokens + CASE WHEN usage_statistics.last_1m_reset_at IS NOT NULL THEN ?6 ELSE 0 END,
            total_requests      = total_requests + 1,
            total_tokenized_requests = total_tokenized_requests + ?4,
            total_input_tokens  = total_input_tokens + ?5,
            total_output_tokens = total_output_tokens + ?6,
            total_tokens        = total_tokens + ?5 + ?6,
            updated_at          = ?7
        ",
        rusqlite::params![
            provider_id, vendor, provider_type.as_str(),
            /* ?4 */ has_token_data,
            input, output,
            now,
            /* ?8 */ windows_armed // INSERT branch only; DO UPDATE uses the stored reset_at guards
        ],
    )?;
    Ok(())
}
```

> **Note on the INSERT branch:** a brand-new row has no `reset_at` values (NULL), so its window counters stay 0 until the first usage poll writes `reset_at` (at most ~60s later); requests in that window are still fully captured in `total_*`. Window counters begin accumulating from the poll onward — acceptable imprecision, and it keeps the recording path a single statement. If desired, the caller can pass the currently-known activity flags instead of `false`, but the default keeps it simple.

**Concurrency:** all writes are serialized through the dedicated writer thread (see *Non-Blocking Integration*), which exclusively owns the single `Connection` — no write-write contention, no `SQLITE_BUSY` between statistics writers, and no blocking of the async runtime. The recording call site is a non-blocking channel send.

### Reset At Update Mechanism

**⚠️ CRITICAL: `reset_at` values must be updated ONLY from usage API responses, never during counter resets.**

The `reset_at` fields represent fixed plan cycle boundaries (e.g., 14:00, 19:00, etc.) and should only be updated when the vendor's usage API returns new reset times. This is typically done during periodic usage polling or when quota exhaustion triggers a fresh usage query.

**A changed `reset_at` IS the new-cycle signal.** When the usage API (polled every ~60s) reports a `reset_at` different from the stored one, the plan has rolled into a new window — so the same transaction updates `reset_at` **and zeroes that window's counters**. This makes the polling loop the single authority for resets; `record_request` never inspects time at all. (The row may not exist yet — usage polling can fire for a brand-new provider before its first request — so the read below uses `.optional()` and an explicit armed-row INSERT; see the code.)

**Reset trigger source — backend choke point, NOT the frontend page.** The existing usage polling is frontend-driven (`Usage.vue`'s `usePolling`); if the window is closed, no usage query runs anywhere, and window resets would silently stop. The hook therefore lives at the **backend** choke point every usage query passes through regardless of origin: `UsageCache::set` (called by `get_usage` polling, the tray's periodic refresh, and the breaker's realtime `usage_snapshot_realtime` alike). When `set` stores a fresh snapshot, it also sends `StatEvent::ResetFromUsage { provider_id, tiers }` to the writer thread — the reset logic then applies the changed-`reset_at` rule above. Consequence: reset latency matches usage-query cadence from whichever source fires (frontend ~60s, tray refresh, or a breaker-triggered realtime query), and resets work even with the UI closed, as long as any usage query happens. (Between the vendor's window rollover and the next query, requests still accumulate — and are then zeroed together with the window at the reset application; tokens counted into the *expired* window are lost from that window's display but preserved in lifetime totals. Accepted imprecision, bounded by the query interval.)

```rust
// CORRECT: changed reset_at from the usage API → new cycle → update + zero counters
// Runs on the dedicated writer thread (owns `conn` exclusively), driven by
// `StatEvent::ResetFromUsage { provider_id, snapshot }` — the whole UsageSnapshot
// travels in the event, so this function reads it directly.
use rusqlite::OptionalExtension;

pub fn update_reset_times_from_usage(
    &self,
    provider_id: &str,
    usage_snapshot: &UsageSnapshot,
) -> SqliteResult<()> {
    // Single transaction: read old reset_at, compare, update + reset counters
    let tx = conn.unchecked_transaction()?;

    if !usage_snapshot.tiers.is_empty() {
        for tier in &usage_snapshot.tiers {
            // (column_reset, column_requests, column_tokenized, column_input, column_output) per window
            let (reset_col, req_col, tok_col, in_col, out_col) = match tier.window.as_str() {
                "five_hour" => ("last_5h_reset_at", "last_5h_requests", "last_5h_tokenized_requests", "last_5h_input_tokens", "last_5h_output_tokens"),
                "weekly_limit" => ("last_1w_reset_at", "last_1w_requests", "last_1w_tokenized_requests", "last_1w_input_tokens", "last_1w_output_tokens"),
                "monthly" => ("last_1m_reset_at", "last_1m_requests", "last_1m_tokenized_requests", "last_1m_input_tokens", "last_1m_output_tokens"),
                _ => continue,
            };
            if let Some(new_reset) = tier.reset_at {
                // ⚠️ A brand-new provider may have NO row yet (usage polling can
                // fire before the first request creates one via the upsert).
                // `query_row` would return Err(QueryReturnedNoRows) and abort the
                // whole transaction — use `.optional()` so a missing row reads
                // as None (→ arms the window on first observation below).
                let old_reset: Option<i64> = tx
                    .query_row(
                        &format!("SELECT {reset_col} FROM usage_statistics WHERE provider_id = ?1"),
                        rusqlite::params![provider_id],
                        |row| row.get(0),
                    )
                    .optional()?   // Option<Option<i64>> on a nullable column
                    .flatten();    // → Option<i64>

                if old_reset != Some(new_reset) {
                    if old_reset.is_none() && !row_exists(&tx, provider_id)? {
                        // No row at all: INSERT one carrying the reset times so the
                        // window is armed before any request arrives.
                        insert_armed_row(&tx, provider_id, new_reset)?;
                        continue;
                    }
                    // reset_at changed → new window cycle → write new reset_at and zero counters
                    // (first observation of an existing provider: old is NULL → also arms the window;
                    //  counters are already 0, so zeroing is a no-op)
                    tx.execute(
                        &format!(
                            "UPDATE usage_statistics SET {reset_col} = ?1, {req_col} = 0, {tok_col} = 0, {in_col} = 0, {out_col} = 0 WHERE provider_id = ?2"
                        ),
                        rusqlite::params![new_reset, provider_id],
                    )?;
                }
            }
        }
    }

    tx.commit()?;
    Ok(())
}
```

**Never update `reset_at` to current time during counter resets!** This would cause the infinite loop bug where every request triggers a reset. And never reset counters inside `record_request` — that reintroduces the read-then-write race the polling design eliminates.

## Fallback Strategy

### Default Fallback Behavior

**Core principle:** Always record request counts, even when token data is unavailable.

```rust
/// Fire-and-forget recording entry point, called from the dispatch hot path.
/// Only a non-blocking channel send — the dedicated writer thread applies the
/// upsert (which itself implements the default fallback: token data `None`
/// still increments request counters).
pub fn record_request_auto(
    &self,
    provider_id: &str,
    vendor: &str,
    provider_type: ProviderType,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) {
    if let Err(e) = self.tx.send(StatEvent::Record {
        provider_id: provider_id.to_string(),
        vendor: vendor.to_string(),
        provider_type,
        input_tokens,
        output_tokens,
    }) {
        // Writer thread gone (shutdown) — best-effort, drop with a warn.
        tracing::warn!("statistics writer unavailable, dropping event: {e}");
    }
    // Never blocks request processing - statistics are non-critical
}
```

### Data Quality Levels

Derived **per window scope** from `tokenized_requests` vs `requests` (never stored — see *Database Schema*):

1. **Full**: at least one request carried token data (`tokenized > 0`); `coverage_pct` exposes the exact ratio when partial
2. **RequestsOnly**: traffic recorded but no request ever carried token data (`tokenized == 0 && requests > 0`)
3. **Unknown**: no traffic recorded yet (`requests == 0`)

### Missing Data Scenarios

| Scenario | Handling | User Experience |
|----------|----------|------------------|
| Vendor doesn't return usage | Record request count only | Show "(无token数据)" badge |
| Streaming token extraction fails | Record request count only | Graceful degradation |
| Malformed response | Log warning, record request count | Partial data display |
| Database write fails | Log error, don't retry | Non-blocking failure |

## Frontend Integration

### Backend Commands

```rust
#[tauri::command]
async fn get_usage_statistics(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ProviderStats>, String> {
    // Routes through the writer thread (StatEvent::Query + oneshot) so all DB
    // access stays on the single owning connection; the async command only
    // awaits the reply channel.
    state.statistics.get_all_stats().await
        .map_err(|e| e.to_string())
}
```

### TypeScript Types

```typescript
export interface ProviderStats {
  provider_id: string;
  vendor: string;
  provider_type: 'plan' | 'consumption';
  
  last_5h: WindowStats;
  last_1w: WindowStats;
  last_1m: WindowStats;
  
  total_requests: number;
  total_tokenized_requests: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_tokens: number;
}

export interface WindowStats {
  requests: number;
  tokenized_requests: number;
  input_tokens: number;
  output_tokens: number;
  reset_at: number | null;
  is_active: boolean;
  
  // Derived (server-side) from tokenized_requests/requests:
  quality: 'Full' | 'RequestsOnly' | 'Unknown';
  /** Present when 0 < tokenized < requests — partial coverage percentage. */
  coverage_pct: number | null;
}
```

### Display Layout

**Current Usage.vue Enhancement:**
- Shorten progress bar to 40% width
- Add token count and request count after progress bar
- Show reset time at the end
- Hover tooltip shows input/output breakdown
- Data quality badges for degraded data

```vue
<div class="window-row">
  <div class="window-label">5小时</div>
  
  <!-- Progress bar (shortened to 40%) -->
  <div class="progress-container" style="width: 40%">
    <NProgress :percentage="85" :height="8" :show-indicator="false" />
  </div>
  
  <!-- Statistics display -->
  <div class="stats-display">
    <div class="token-stats">
      <NTooltip>
        <template #trigger>
          <span class="token-count">15.2K</span>
        </template>
        <div class="token-tooltip">
          <div class="tooltip-row">
            <span>Input tokens:</span>
            <span>8.1K</span>
          </div>
          <div class="tooltip-row">
            <span>Output tokens:</span>
            <span>7.1K</span>
          </div>
        </div>
      </NTooltip>
      <span class="token-label">tokens</span>
    </div>
    <div class="request-stats">
      <span class="request-count">42</span>
      <span class="request-label">requests</span>
    </div>
  </div>
  
  <!-- Reset time -->
  <div class="reset-time">重置于 14:30</div>
</div>
```

## Error Handling

### Error Categories

1. **Extraction gaps** - missing/unparseable `usage` fields in an upstream response
2. **Storage Errors** - database open/write failures (writer thread logs and drops)
3. **Writer-thread death** - channel send fails, events dropped with a warn

### Handling Strategy

Errors never propagate to request processing; every failure degrades to a logged drop:

| Failure point | Handling | Data effect |
|---|---|---|
| `usage` fields missing / malformed JSON | `extract_tokens_from_json` returns `(None, None)` (pure function — no error path at all) | Request still counted; `tokenized_requests` not incremented → quality degrades to RequestsOnly |
| SQLite write fails (batch) | Writer thread `tracing::warn!` and drops the batch | That batch's counters lost; next batch unaffected |
| Writer thread died | `tx.send` errors → warn + drop event | Statistics stop updating; proxy fully functional |
| DB file unwritable at startup | Service starts with a dead channel (every send warns) | Feature disabled for the session; app runs normally |
| DB schema migration failure | Log error, back up the corrupt file and recreate empty | Lifetime totals lost; app runs normally |

**No `ProviderNotFound` handling exists** — the UPSERT creates missing rows on first write, which is the entire point of the upsert design. Provider-type changes (user edits a provider) are absorbed by the `DO UPDATE` branch refreshing `vendor`/`provider_type` on every record.

### Non-Blocking Integration

**⚠️ Never call blocking rusqlite writes from `tokio::spawn` tasks.** rusqlite is synchronous; running it inside async worker tasks blocks tokio's runtime threads under write contention (and starves the proxy forwarding sharing those threads) — the exact opposite of "statistics never affect request processing".

The service uses a **dedicated writer thread** pattern instead:

```rust
pub struct UsageStatisticsService {
    /// Channel to the dedicated writer thread. `send` is non-blocking (unbounded
    /// mpsc); if the writer thread has died, send fails and the event is dropped
    /// with a warn log — statistics are best-effort.
    tx: std::sync::mpsc::Sender<StatEvent>,
}

pub enum StatEvent {
    Record { provider_id: String, vendor: String, provider_type: ProviderType, input_tokens: Option<u64>, output_tokens: Option<u64> },
    ResetFromUsage { provider_id: String, snapshot: UsageSnapshot }, // carried whole — the writer thread runs update_reset_times_from_usage(&snapshot) directly
    /// Read path: frontend polling. The writer thread executes the SELECT and
    /// replies on the oneshot — single connection, single owner.
    Query { reply: tokio::sync::oneshot::Sender<Vec<ProviderStats>> },
    /// Provider removed from config → drop its statistics row (replaces the
    /// impossible cross-file FK cascade; see Database Schema).
    DeleteProvider { provider_id: String },
}

// Spawned once at startup — a plain std::thread, NOT a tokio task:
fn writer_thread(mut rx: std::sync::mpsc::Receiver<StatEvent>, mut conn: Connection) {
    while let Ok(event) = rx.recv() {
        // Drain-and-batch: take everything already queued, apply in ONE transaction.
        let mut batch = vec![event];
        while let Ok(ev) = rx.try_recv() { batch.push(ev); }
        if let Err(e) = apply_batch(&mut conn, &batch) {
            tracing::warn!("statistics batch write failed: {e}"); // best-effort, drop
        }
    }
}

// In dispatch.rs — the async hot path only does a non-blocking send:
if let Err(e) = state.statistics.tx.send(StatEvent::Record { /* ... */ }) {
    tracing::warn!("statistics writer unavailable, dropping event: {e}");
}
```

Why this shape:

- **Zero blocking in the async path** — `mpsc::send` never blocks (unbounded); forwarding is never delayed by disk I/O.
- **Single writer, single connection** — all writes are serialized on one thread owning one `Connection`, so there is no write-write contention and `SQLITE_BUSY` between statistics writers cannot occur.
- **Natural batching** — draining the queue and applying in one transaction turns a burst of requests into a single fsync.
- **Read side**: `get_usage_statistics` (frontend polling) also goes through the writer thread via a request/response message (`StatEvent::Query` + oneshot `Sender<Vec<ProviderStats>>`), keeping exactly one connection in play. (Alternatively a read-only connection in WAL mode — WAL permits one writer + N readers — but the single-thread route is simpler and reads are rare, ~1/60s.)
- **Defensive `busy_timeout`** (e.g. 5s) is still set on the connection so that if a second connection ever appears (e.g. a future migration or external inspection of the DB file), SQLite waits instead of immediately erroring `SQLITE_BUSY`.

## Performance Considerations

### Database Optimizations

- **Dedicated writer thread** owning a single connection (see *Non-Blocking Integration*) — serialized writes, no async-thread blocking, batch transactions under load
- WAL mode (fast commits; allows concurrent readers if a read connection is ever split out)
- `busy_timeout = 5000ms` as defense-in-depth against any future second connection
- Prepared/cached statements for the upsert and the reset update
- Unbounded mpsc queue: burst absorption at nanosecond send cost; bounded only by the (tiny) size of one `StatEvent` per request

### Performance Targets

- Statistics send overhead on the request path: < 1ms (a channel send, no I/O)
- Database query time: < 100ms
- UI refresh: Smooth updates without blocking

## Testing Strategy

### Unit Tests

- Token extraction for all protocol combinations
- Window reset logic
- Database operations
- Error handling paths

### Integration Tests

- Full dispatch flow with statistics recording
- Streaming and non-streaming paths
- Provider initialization and fallback
- Frontend data display

### Performance Tests

- High-volume request handling
- Database concurrency
- Memory usage monitoring

## Implementation Phases

### Phase 1: Foundation (Week 1)
- Database schema and initialization
- Core `StatisticsDatabase` implementation  
- Basic service structure
- Backend command registration
- Frontend type definitions

### Phase 2: Token Extraction (Week 1-2)
- Non-streaming token extraction
- Streaming token collector
- Dispatch integration
- Token extraction unit tests

### Phase 3: Database Integration (Week 2)
- Provider initialization
- Recording logic with window management
- Error handling and auto-initialization
- Performance optimizations

### Phase 4: Frontend Integration (Week 3)
- Backend commands and API bridge
- Statistics loading and caching
- Usage.vue layout modifications
- Tooltip and formatting functions

### Phase 5: Testing and Polish (Week 4)
- Integration tests
- Edge case handling
- Performance testing
- UI/UX refinements
- Documentation updates

## Success Criteria

### Functional Requirements
- ✅ Correct token extraction and recording
- ✅ Support for OpenAI and Anthropic protocols
- ✅ Handle streaming and non-streaming responses
- ✅ Working fallback strategy

### Performance Requirements  
- ✅ Recording overhead < 1ms per request (a channel send — no I/O on the hot path)
- ✅ Database queries < 100ms
- ✅ Smooth UI refresh

### User Experience
- ✅ Accurate and consistent data display
- ✅ Clear indication of degraded data
- ✅ Complete hover tooltip information

### Code Quality
- ✅ Unit test coverage > 80%
- ✅ No memory leaks or race conditions
- ✅ Comprehensive error handling

## Migration and Compatibility

### Zero-Deployment Migration
- Database automatically created on first startup
- No breaking changes to existing config files
- Progressive enhancement - app works if statistics fail
- Graceful degradation

### Backward Compatibility
- Existing features work independently
- Optional feature with graceful failure modes
- No user configuration required

## Vendor Capability Documentation

| Vendor | Non-Stream Usage | Stream Usage | Format | Notes |
|--------|------------------|--------------|---------|-------|
| zhipu | ✅ | ✅ | Standard | Returns usage in both modes |
| deepseek | ✅ | ✅ | OpenAI Compatible | Requires `stream_options.include_usage` |
| volcengine-coding | ❓ | ❓ | Unknown | May need testing, fallback to requests only |
| qianwen-token | ❓ | ❓ | Unknown | Cookie-based auth, usage unclear |

## Future Enhancements

### Potential Features
- Token estimation when vendor doesn't provide usage
- Historical trends and charts
- Export statistics data
- Per-model statistics breakdown
- Cost estimation based on token usage

### Monitoring Improvements
- Data quality dashboards
- Vendor capability testing
- Performance metrics collection
- Error rate monitoring

---

**Document Status:** Design approved, ready for implementation  
**Next Steps:** Begin Phase 1 implementation starting with database schema