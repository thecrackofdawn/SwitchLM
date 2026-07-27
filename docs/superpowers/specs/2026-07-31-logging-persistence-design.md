# Logging Persistence Design

**Date:** 2026-07-31
**Status:** Approved
**Author:** Claude (SwitchLM Project)

## Overview

The backend logs exclusively through the `tracing` ecosystem, initialized by a single line in
`src-tauri/src/lib.rs:34`:

```rust
let _ = tracing_subscriber::fmt::try_init();
```

This uses the **default** `fmt` subscriber: output goes to **stdout**, the max level is **INFO**
(verified against `tracing-subscriber-0.3.23/src/fmt/mod.rs:349` — `DEFAULT_MAX_LEVEL = INFO`),
**no `EnvFilter` is installed** (so `RUST_LOG` has no effect), and there is **no file target, no
`tauri-plugin-log`, no rolling appender**.

The practical consequence: in **release** builds, `src-tauri/src/main.rs:2` sets
`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`, so the app is a GUI subsystem
binary with **no attached console** — its stdout is detached and the logs are effectively **lost**.
Only `cargo tauri dev` shows them (the dev console window).

This design adds **persistent, size-rotated file logging** so issues can be diagnosed after the
fact, with:

- **Per-request / panic capture** — panics are written to the log file before the process dies.
- **A user-configurable log level** in Settings (default `info`), applied **live** without an app
  restart via a reloadable filter.
- **An "open log folder" button** so the user can find the file without hunting for `app_data_dir`.

## Requirements

### Functional Requirements

1. **File target** — All existing `tracing::info!/warn!/error!/debug!` calls (and future ones) are
   mirrored to a file at `<app_data_dir>/logs/switchlm.log`, in addition to stdout.
2. **Size-based rotation** — When `switchlm.log` reaches **5 MiB**, it rotates: the oldest of the
   `switchlm.log.1` … `switchlm.log.4` backups is dropped, the others shift up by one, and a fresh
   `switchlm.log` is opened. **5 files total** are kept (~25 MiB cap).
3. **Live log level** — A persisted setting `log_level` (`trace|debug|info|warn|error`, default
   `info`) controls the verbosity of **both** the file and stdout targets. Changing it in Settings
   takes effect **immediately** — no restart, no lost app/proxy state.
4. **Panic capture** — A panic hook records the panic message + location to the log file via
   `tracing::error!`, then chains to the previous hook.
5. **Backward compatible** — Old `app_config.json` without `log_level` loads at `info`
  (`#[serde(default)]`).
6. **Open log folder** — A Settings action opens `<app_data_dir>/logs/` in the OS file manager.
7. **Tamper-tolerant** — A hand-edited `log_level` that is not one of the five accepted values is
   normalized to `info` on read (mirrors the `clamp_usage_refresh_secs` projection pattern).
8. **Request-level forwarding logs (INFO)** — Every inbound forwarded request is logged at INFO at
   the `dispatch` boundary: inbound path, requested model, resolved model id, stream flag; plus the
   outcome (HTTP status + latency on success; error + latency on failure). Each fallback hop
   (cooling / rate-limited / no-key / no-backend / missing) is also logged at INFO with the model +
   reason, so the fallback chain is traceable in the log.
9. **No payload logging** — Request/response **bodies and headers are never logged**; only routing
   metadata (path, model ids, status, latency, hop reasons). API keys are never logged.

### Non-Functional Requirements

1. **Zero churn to existing log calls** — The codebase already standardized on `tracing`; the new
   target is added at the subscriber layer, so `tracing::info!` etc. are untouched.
2. **Filesystem-robust & crash-safe** — (a) On Windows an open file cannot be renamed, so rotation
   **closes the handle before renaming** then reopens. (b) `rotate()` is best-effort: if a
   rename/remove is blocked (antivirus, Windows Indexer, another process), it **keeps appending to
   the current file** and retries lazily — never panics or permanently disables logging. (c) `tracing`'s
   fmt layer swallows writer errors (logs an internal note instead of propagating), and
   `init_logging` falls back to **stdout-only** if the log file can't be opened (e.g. held by another
   instance) — so the app always starts and logging can never crash it.
3. **No event loss on exit / panic** — A **blocking** writer writes each event synchronously; there
   is no buffered channel to drain, so nothing is lost on crash or shutdown and a panic line is on
   disk before the panic hook returns (no flush wiring needed).
4. **No new dependencies** — `tracing` + `tracing-subscriber` (already present) suffice; the writer
   and size rotation are small in-repo impls, matching the project's lean-dependency style.
5. **Follows existing settings conventions** — per-field serde default, `SettingsView` projection
   with normalization, dedicated `set_*` command with validation + persist, typed invoke wrapper,
   store action, naive-ui control in `Settings.vue`.
6. **Bounded volume under per-request logging** — Each forwarded request now emits ≥1 INFO line, so
   log volume scales with traffic. The 5 MiB × 5 cap still bounds disk to ~25 MiB, but high request
   throughput rotates files faster (less time-depth). Acceptable for a troubleshooting log; revisit
   retention later if deeper history is needed.

### Non-Goals (explicitly out of scope this round)

- **Frontend log aggregation** (forwarding `console.*` from the webview into the same file).
- **In-app log viewer** (the "open folder" button covers discoverability for now).
- **Log upload / telemetry.**

## Why Not `tauri-plugin-log` (Key Decision)

`tauri-plugin-log` writes through the **`log` crate facade**, not `tracing`. The SwitchLM backend
uses `tracing::` macros exclusively (~20 call sites across `commands.rs`, `lib.rs`, `proxy/`,
`config/`, `usage/`). Installing `tauri-plugin-log` alone would route **`log::` calls** to the file
but leave every existing `tracing::` call going nowhere (there is no official tracing→log bridge —
`tracing-log` only bridges log→tracing). Making it work would mean either converting the whole
backend to the `log` facade (large, undesirable churn) or a fragile custom bridge.

Staying in the `tracing` ecosystem (file via `tracing_subscriber::fmt().with_writer(...)`, level via
`LevelFilter`) keeps every existing call site unchanged. This is the deciding factor.

## Implementation Architecture

### Data Flow

```
                    tracing::info!/warn!/error!/debug!   (existing call sites — unchanged)
                                      │
            ┌─────────────────────────┴──────────────────────────┐
            │   tracing_subscriber::Registry                       │
            │        │                                             │
            │   reload::Layer<LevelFilter>   ← shared, live level (set_log_level mutates it)
            │        │                                             │
            │   ┌────┴────┐                                        │
            │   ▼         ▼                                        │
            │ file fmt   stdout fmt                                │
            │ layer      layer (ANSI on; dev console only in       │
            │ (ANSI off) release — writes are harmless no-ops)     │
            │   │                                                  │
            │   ▼                                                  │
            │ MakeWriter → Arc<Mutex<RollingFileWriter>>            │
            │   (blocking; one global lock serializes file writes   │
            │    → correct ordering, no interleaving, nothing       │
            │    buffered to lose on crash/exit)                    │
            │   ▼                                                  │
            │ RollingFileWriter: <app_data_dir>/logs/switchlm.log  │
            │   on >= 5MiB: close → shift .1..4 → reopen            │
            │   (rename blocked? keep appending — never crash)      │
            └──────────────────────────────────────────────────────┘

panic → panic_hook → tracing::error!(msg + location) → synchronous blocking write → file
       (blocking ⇒ the panic line is on disk before the hook returns — no flush needed)
```

### Backend (Rust)

#### 1. No new dependency

`tracing` + `tracing-subscriber` (already in `Cargo.toml`) suffice. The file writer is a small
in-repo `MakeWriter` impl over `Arc<Mutex<RollingFileWriter>>` — **no `tracing-appender`, no extra
crate.** (A non-blocking writer was considered and rejected — see Decision table.)

#### 2. Size-rotating file writer

**File: `src-tauri/src/logging/mod.rs`** (NEW) — a plain `std::io::Write` impl, fully unit-testable
without a subscriber:

```rust
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5 MiB
pub const MAX_FILES: usize = 5;                 // switchlm.log + .1 .2 .3 .4
pub const LOG_FILE_NAME: &str = "switchlm.log";

/// A `Write` that appends to `<dir>/switchlm.log` and, once the current file reaches
/// `MAX_FILE_SIZE`, rotates numbered backups (keeping `MAX_FILES` total). The active handle is held
/// in an `Option` so rotation can **close it before renaming** (required on Windows). Rotation is
/// best-effort and infallible from the caller's view: a blocked `rename` degrades to "keep
/// appending" rather than leaving the writer unusable (see `rotate`).
pub struct RollingFileWriter {
    dir: PathBuf,
    file: Option<File>,
    written: u64,
}

impl RollingFileWriter {
    pub fn new(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = open_log(dir)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self { dir: dir.into(), file: Some(file), written })
    }

    /// Rotate numbered backups, keeping `MAX_FILES` total. **Best-effort:** every `fs` op whose
    /// failure is survivable is swallowed, and the active file is reopened afterward so `write()`
    /// always has somewhere to go. If the current→.1 rename is blocked (antivirus / Windows Indexer
    /// / another process holding the file), we keep appending to the existing file and retry
    /// rotation lazily (after another `MAX_FILE_SIZE` of growth) — logging never stops, never
    /// panics. (Single-writer invariant: under the blocking `MakeWriter` this is only ever called
    /// from one thread at a time, so no internal lock is needed.)
    fn rotate(&mut self) {
        self.file = None; // close BEFORE rename (Windows cannot rename open files)
        let cur = self.dir.join(LOG_FILE_NAME);
        // Drop the oldest backup (.{MAX_FILES-1}), then shift .{n} -> .{n+1} for the rest. Going
        // high→low and pre-dropping the oldest (rather than rename .{MAX_FILES-1} -> .{MAX_FILES})
        // keeps exactly MAX_FILES files — never produces a .{MAX_FILES}.
        let _ = std::fs::remove_file(self.dir.join(format!("{LOG_FILE_NAME}.{}", MAX_FILES - 1)));
        for n in (1..MAX_FILES - 1).rev() {
            let from = self.dir.join(format!("{LOG_FILE_NAME}.{n}"));
            let to = self.dir.join(format!("{LOG_FILE_NAME}.{}", n + 1));
            let _ = std::fs::rename(&from, &to); // best-effort; missing/locked files skipped
        }
        let rotated = std::fs::rename(&cur, self.dir.join(format!("{LOG_FILE_NAME}.1"))).is_ok();
        // Reopen whatever switchlm.log is now (fresh file if rotated; the old one if the rename was
        // blocked). `create(true)` ⇒ this essentially always succeeds; on the near-impossible
        // failure, the fmt layer swallows the writer error (logging halts gracefully, no crash).
        self.file = open_log(&self.dir).ok();
        // Reset the trigger: success → empty file; failure → count fresh so we retry only after
        // another MAX_FILE_SIZE of growth (avoids hammering a failing rename on every write).
        self.written = 0;
        let _ = rotated;
    }
}

impl Write for RollingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = match self.file.as_mut() {
            Some(f) => f.write(buf)?,
            None => return Err(io::Error::other("no log file")), // fmt layer swallows this
        };
        self.written += n as u64;
        if self.written >= MAX_FILE_SIZE {
            self.rotate(); // infallible (best-effort)
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map(|f| f.flush()).transpose().map(|_| ())
    }
}

fn open_log(dir: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(dir.join(LOG_FILE_NAME))
}
```

#### 3. Subscriber wiring + live level + panic hook

**File: `src-tauri/src/logging/mod.rs`** (continued). The shared, reloadable `LevelFilter` sits above
both fmt layers so one `set_level` call governs both targets:

```rust
use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::{
    filter::LevelFilter, fmt::{self, writer::MakeWriter}, layer::SubscriberExt, reload, util::SubscriberInitExt,
};

/// Live level handle. No flush guard is needed — the writer is blocking/synchronous.
pub struct LogHandle {
    level: reload::Handle<LevelFilter>,
}

/// `MakeWriter` that serializes all file writes through one `Mutex` (correct ordering, no
/// interleaving). Each event locks, writes, unlocks; rotate() runs under the same lock so it is
/// single-threaded by construction.
struct FileMaker(Arc<Mutex<RollingFileWriter>>);
impl<'a> MakeWriter<'a> for FileMaker {
    type Writer = GuardWriter<'a>;
    fn make_writer(&'a self) -> Self::Writer { GuardWriter(self.0.lock().expect("log mutex poisoned")) }
}
struct GuardWriter<'a>(std::sync::MutexGuard<'a, RollingFileWriter>);
impl<'a> std::io::Write for GuardWriter<'a> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0.write(b) }
    fn flush(&mut self) -> std::io::Result<()> { self.0.flush() }
}

pub fn init_logging(log_dir: &Path, level: LevelFilter) -> LogHandle {
    let (level_layer, level_handle) = reload::Layer::new(level);

    let stdout_layer = fmt::layer().with_writer(std::io::stdout).with_ansi(true);

    // The file layer is boxed (Box<dyn Layer>) so the stdout-only fallback arm type-matches the
    // normal arm (they have different concrete layer stacks).
    let file_layer: Option<Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>> =
        RollingFileWriter::new(log_dir).ok().map(|w| {
            Box::new(fmt::layer()
                .with_writer(FileMaker(Arc::new(Mutex::new(w))))
                .with_ansi(false)          // no color codes in the file
                .with_target(true))        // default text format, incl. timestamps
        });
    if file_layer.is_none() {
        // File unavailable (e.g. held open by another instance) — degrade to stdout-only so the
        // app still starts. init NEVER panics.
        eprintln!("switchlm: log file unavailable, falling back to stdout-only");
    }

    let reg = tracing_subscriber::registry().with(level_layer).with(stdout_layer);
    if let Some(fl) = file_layer {
        reg.with(fl).init();
    } else {
        reg.init();
    }

    install_panic_hook();
    LogHandle { level: level_handle }
}

/// Apply a new level live (no restart). No-op if the subscriber wasn't installed.
pub fn set_level(handle: &LogHandle, level: LevelFilter) {
    let _ = handle.level.modify(|current| *current = level);
}

/// Parse a level string ("info"/"debug"/…) into a `LevelFilter`, falling back to INFO on anything
/// unrecognized (so the startup path and the command share one tolerant parser).
pub fn level_filter_for(s: &str) -> LevelFilter {
    s.parse::<tracing::Level>().map(LevelFilter::from).unwrap_or(LevelFilter::INFO)
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        // Blocking writer ⇒ on disk before the hook returns — no flush race, no WorkerGuard needed.
        tracing::error!("panic: {info} at {loc}");
        prev(info);
    }));
}
```

> Exact generic types of `reload::Handle` / the layer stack are resolved during implementation; the
> structure above is the design contract.

#### 4. Startup integration

**File: `src-tauri/src/lib.rs`** — remove the `let _ = tracing_subscriber::fmt::try_init();` line
(top of `run()`), and initialize inside `setup` right after `app_data_dir` is known (before
`migrate_usage_sk_to_keyring`, so its warnings are captured). Default to `info` first, then promote
to the persisted level once the config is loaded:

```rust
let dir = app.path().app_data_dir()?;
let log_dir = dir.join("logs");
let log_handle = crate::logging::init_logging(&log_dir, LevelFilter::INFO);
// ... migrate_usage_sk_to_keyring(...) → warnings now captured
// load config, then promote to the persisted level (parse-tolerant):
let _ = crate::logging::set_level(&log_handle, crate::logging::level_filter_for(&cfg.settings.log_level));
app.manage(log_handle);
```

`mod logging;` declared at the top of `lib.rs`.

#### 5. Persisted setting + normalization

**File: `src-tauri/src/config/types.rs`** — mirrors the `usage_refresh_interval_secs` pattern:

```rust
pub const ALLOWED_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
pub const DEFAULT_LOG_LEVEL: &str = "info";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_usage_refresh_interval_secs")]
    pub usage_refresh_interval_secs: u32,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}
// Default impl and default_log_level() updated accordingly.

/// Normalize a stored/passed log level: lowercase; if not one of the five allowed, fall back to
/// `info`. Applied on read so a hand-edited config can never set an invalid level. Returns an owned
/// `String` (cloned once per call — cheap; called only on settings read/save).
pub fn normalize_log_level(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if ALLOWED_LOG_LEVELS.contains(&lower.as_str()) { lower } else { DEFAULT_LOG_LEVEL.into() }
}
```

> Returns an owned `String` rather than a borrowed `&str` so it can be stored back into `Settings`
> and serialized without lifetime gymnastics or leaking. The valid set is tiny, so the clone is
> negligible.

#### 6. Read DTO + write command + open-folder

**File: `src-tauri/src/commands.rs`**

```rust
#[derive(Serialize)]
pub struct SettingsView {
    pub port: u16,
    pub autostart: bool,
    pub usage_refresh_interval_secs: u32,
    pub log_level: String,            // already-normalized on read
}

// get_settings projects: log_level: normalize_log_level(&cfg.settings.log_level).into()

#[tauri::command]
pub async fn set_log_level(
    state: State<'_, AppState>,
    log: State<'_, LogHandle>,
    app: tauri::AppHandle,
    level: String,
) -> Result<(), String> {
    let normalized = normalize_log_level(&level);
    {
        let mut config = state.config.write().await;
        config.settings.log_level = normalized.clone(); // store the validated string
        persist(&app, &config)?;
    }
    crate::logging::set_level(&log, crate::logging::level_filter_for(&normalized));
    Ok(())
}

#[tauri::command]
pub async fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?; // ensure it exists (init may have
                                                               // fallen back to stdout-only)
    app.opener().open_location(dir, None::<&str>).map_err(|e| e.to_string())
}
```

**File: `src-tauri/src/lib.rs`** — register `commands::set_log_level`, `commands::open_log_dir`.

#### 7. Request-level forwarding logs (INFO)

**File: `src-tauri/src/proxy/dispatch.rs`** — `dispatch` is the single chokepoint for every inbound
forwarded request (both edges call it). Wrap its body to log the request at INFO on entry and the
outcome on return; add per-hop INFO lines at each fallback-advance point. **Routing metadata only**
— no request/response bodies, no headers, no keys.

```rust
use std::time::Instant;

pub async fn dispatch(state: AppState, body: Bytes, protocol: ClientProtocol)
    -> Result<Response<Body>, ProxyError>
{
    let start = Instant::now();
    // ... existing parse + resolve (start_model_id, echo_model, is_stream) ...

    tracing::info!(
        target: "switchlm::proxy",
        inbound = path_for(protocol),               // "/v1/chat/completions" | "/v1/messages"
        requested = req.get("model").and_then(|v| v.as_str()).unwrap_or("-"),
        model = %start_model_id,
        stream = is_stream,
        "forward request",
    );

    let result = if is_stream {
        dispatch_stream(&state, &req, protocol, &start_model_id, &echo_model).await
    } else {
        dispatch_non_stream(&state, &req, protocol, &start_model_id, &echo_model).await
    };

    let ms = start.elapsed().as_millis();
    match &result {
        Ok(resp) => tracing::info!(target: "switchlm::proxy", status = resp.status().as_u16(), ms, "forward ok"),
        Err(e)   => tracing::warn!(target: "switchlm::proxy", error = %e, ms, "forward failed"),
    }
    result
}

fn path_for(p: ClientProtocol) -> &'static str {
    match p { ClientProtocol::OpenAI => "/v1/chat/completions", ClientProtocol::Anthropic => "/v1/messages" }
}
```

Per-hop fallback trace — at each advance point in `dispatch_non_stream` / `dispatch_stream` (where
`last_error` is set and `next_fallback_id` yields a successor), add one INFO line so the chain is
readable end-to-end:

```rust
tracing::info!(target: "switchlm::proxy", from = %current, reason = %last_error, "fallback hop");
```

Notes:
- **Stream latency** = time to the committed response (headers / first byte), not full generation
  drain — `dispatch` returns once the stream is set up.
- **Volume** — ≥2 INFO lines per request (entry + outcome) plus hop lines; the size-rotation cap
  (Non-Functional #6) bounds the disk cost.

### Frontend (TypeScript / Vue)

#### 1. Types + options

**File: `src/lib/types.ts`**

```ts
export interface Settings { /* …existing… */ log_level: string; }
export interface SettingsView { /* …existing… */ log_level: string; }

export const LOG_LEVEL_OPTIONS = ["trace", "debug", "info", "warn", "error"] as const;
export const DEFAULT_LOG_LEVEL = "info";
```

#### 2. Invoke wrappers

**File: `src/lib/commands.ts`**

```ts
export const setLogLevel = (level: string) => invoke<void>("set_log_level", { level });
export const openLogDir = () => invoke<void>("open_log_dir");
```

#### 3. Store action

**File: `src/stores/system.ts`** — call-setter-then-reload:

```ts
async function saveLogLevel(level: string) {
  await api.setLogLevel(level);
  await loadSettings();
}
async function openLogDir() { await api.openLogDir(); }
```

#### 4. Settings UI

**File: `src/views/Settings.vue`** — a new "日志" card:

```vue
<NCard title="日志" size="small">
  <NSpace align="center" :size="12">
    <NSelect v-model:value="levelInput" :options="levelOptions" style="width: 140px" />
    <NButton type="primary" @click="saveLevel">保存</NButton>
    <NButton @click="openLogs">打开日志文件夹</NButton>
    <span class="muted">默认 info · 用于排查问题,调高可见更详细日志(立即生效)</span>
  </NSpace>
</NCard>
```

with `levelOptions = LOG_LEVEL_OPTIONS.map(v => ({ label: v, value: v }))`, a `watch` syncing
`levelInput` from `system.settings.log_level`, and `saveLevel`/`openLogs` handlers mirroring the
existing `savePort` try/catch + `msg.success/error` pattern.

## Testing Strategy

### Unit Tests (backend, no subscriber needed)

**`RollingFileWriter` rotation** — write past the cap and assert exactly `MAX_FILES` files survive:

| Action | Expected |
|--------|----------|
| Write 30 MiB in chunks to a fresh tempdir | `switchlm.log` + `.1`..`.4` exist; `.5` absent |
| After rotation, current `switchlm.log` size | `< MAX_FILE_SIZE` |
| Each rotated file size | `<= MAX_FILE_SIZE` (approx — a single oversized `write` may slightly overshoot the active file; assert `< MAX_FILE_SIZE + max_line`) |

**Rotation robustness** — after making `switchlm.log.1` non-replaceable (e.g. create a *directory* at
that path so `rename`/`remove_file` fails), writing past `MAX_FILE_SIZE` must still succeed: the
writer keeps appending to the current file, `self.file` stays `Some`, and no panic occurs. A
separate check confirms that an unwritable/locked log dir makes `init_logging` fall back to
stdout-only rather than panicking.

**`normalize_log_level`** — tamper/legacy tolerance:

| Input | Expected |
|-------|----------|
| `"INFO"`, `"Info"`, `"info"` | `"info"` |
| `"debug"` | `"debug"` |
| `""`, `"xyz"`, `"verbose"` | `"info"` |
| Config JSON without `log_level` (serde default) | `"info"` |

Plus update `app_config_roundtrips` to assert the field round-trips and defaults to `info` when
absent.

### Subscriber / level-reload (best-effort, may be manual)

A global subscriber can be installed only once per process, which conflicts with Rust's
default test harness. Options (pick in plan):
- Refactor `init_logging` to accept a `set_global: bool` and use `tracing_subscriber::set_default`
  (returns a thread-local `DefaultGuard`) in the test so multiple tests can run; assert that a
  `tracing::info!("marker")` lands in the temp file after the `LogHandle` (guard) is dropped
  (flush), and that bumping to `debug` then emitting `tracing::trace!("t")` includes it while
  dropping back to `warn` excludes a subsequent `info!`.
- Otherwise verify subscriber wiring, panic capture, and live level-change **manually** in
  `cargo tauri dev`: tail `switchlm.log`, switch level in Settings, trigger a log line, force a
  panic (debug-only path), confirm capture.

### Type Check / Manual (frontend)

- `vue-tsc --noEmit` passes.
- Manual: Settings → set level `debug` → confirm a debug-only code path (e.g.
  `proxy/state.rs:148` `tracing::debug!("自动重试绑定失败…")`, reachable by occupying the port)
  now appears in the file; set back to `info` → new debug lines stop — **without restarting**.
- Manual: "打开日志文件夹" opens the OS file manager at `…/SwitchLM/logs/`.
- Manual (request logging): send a `/v1/chat/completions` and a `/v1/messages` (stream + non-stream)
  request through the proxy; confirm `switchlm.log` shows the entry line (path/requested/resolved
  model/stream) and the outcome line (status or error + ms) for each. Force a fallback (occupy quota
  / 429 one model) and confirm a `fallback hop` line appears. Confirm **no** message bodies, headers,
  or keys are present. Existing `dispatch`/router tests stay green (logging is side-effect-free).

## Files Changed

### Backend
1. `src-tauri/src/logging/mod.rs` — NEW: `RollingFileWriter` (robust size rotation), blocking
   `FileMaker`/`MakeWriter`, `init_logging` (stdout-only fallback on open failure), `LogHandle`,
   `set_level`, `level_filter_for`, panic hook. *(No `Cargo.toml` change — no new deps.)*
2. `src-tauri/src/lib.rs` — `mod logging;`, remove `try_init`, init in setup + manage handle,
   register commands.
3. `src-tauri/src/config/types.rs` — `log_level` field + default + constants + `normalize_log_level`
   + tests.
4. `src-tauri/src/commands.rs` — `SettingsView.log_level`, `get_settings` projection,
   `set_log_level`, `open_log_dir` (with `create_dir_all`).
5. `src-tauri/src/proxy/dispatch.rs` — INFO request-entry/outcome logging at the `dispatch`
   boundary + per-fallback-hop INFO lines (additive; routing metadata only).

### Frontend
1. `src/lib/types.ts` — field in both interfaces + `LOG_LEVEL_OPTIONS` / `DEFAULT_LOG_LEVEL`.
2. `src/lib/commands.ts` — `setLogLevel`, `openLogDir`.
3. `src/stores/system.ts` — `saveLogLevel`, `openLogDir`.
4. `src/views/Settings.vue` — "日志" card (level select + open-folder button).

## Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Log facade | `tracing` (keep) | Codebase already uses it; `tauri-plugin-log` uses `log` and would orphan all `tracing::` calls. |
| Rotation unit | Size (5 MiB × 5) | User-chosen; bounds total disk to ~25 MiB regardless of request volume. |
| Rotation impl | In-repo `RollingFileWriter` (`Write` impl) | No rotation-specific crate; matches lean-dep style; fully unit-testable; Windows close-before-rename handled via `Option<File>`. |
| Async writes (rejected) | Blocking `MakeWriter` over `Arc<Mutex<RollingFileWriter>>` | Panic/exit-safe by construction (no buffer to flush, no `WorkerGuard`); zero new deps; one global lock serializes file writes (correct order, no interleaving). Negligible cost at local-proxy volume; revisit `non_blocking` only if log I/O ever contends. |
| Multi-instance | Non-goal; degrade gracefully | Two processes sharing one log dir is unsupported, but a held-file open failure makes `init_logging` fall back to stdout-only instead of crashing the second instance. |
| Live level | Shared `reload::Layer<LevelFilter>` above both fmt layers | One `set_log_level` call governs file + stdout immediately; no restart. |
| Dual output | file (ANSI off) + stdout (ANSI on) | Dev console still shows logs; release stdout writes are harmless no-ops. |
| File format | Default text (with timestamps) | Human-readable; `.with_ansi(false)`. JSON option noted but not required. |
| Default level | `info` | Matches current behavior; backward-compatible. |
| Invalid level | Normalize to `info` on read | Tamper-tolerant, mirrors `clamp_usage_refresh_secs`. |
| `normalize_log_level` return | owned `String` | Avoids borrowing/lifetime issues and any leak; tiny valid set makes the clone negligible. |
| `open_log_dir` | `tauri-plugin-opener` (already registered) | No new dependency; cross-platform file-manager open. |
| Request logging | INFO at `dispatch` boundary + per-hop | Single chokepoint captures every forwarded request; hop lines make the fallback chain traceable. |
| Request log content | Routing metadata only (no bodies/headers/keys) | Privacy + bounded volume; sufficient to diagnose routing/fallback/latency issues. |

## Success Criteria

1. ✅ Logs are written to `<app_data_dir>/logs/switchlm.log` with timestamps, in both dev and release.
2. ✅ When the file reaches 5 MiB it rotates, keeping exactly 5 files (`switchlm.log` + `.1`..`.4`);
   `.5` never appears.
3. ✅ Rotation is Windows-safe (handle closed before renaming) and a blocked rename degrades to
   keep-appending — no crash, logging continues.
4. ✅ Changing the level in Settings applies immediately (debug lines appear/disappear without a
   restart) and persists across restarts.
5. ✅ A panic is recorded to the log file before the process exits (blocking write — guaranteed,
   no flush race).
6. ✅ Old `app_config.json` without `log_level` loads at `info`; an invalid hand-edited value also
   normalizes to `info`.
7. ✅ "打开日志文件夹" opens the correct folder in the OS file manager.
8. ✅ No existing `tracing::` call site is modified (the request-logging calls in `dispatch.rs` are
   purely additive).
9. ✅ Each forwarded request produces an INFO entry line (path/requested/resolved model/stream) and
   an outcome line (status-or-error/latency); each fallback hop produces an INFO line. No request
   bodies, headers, or API keys appear in the log.
10. ✅ `init_logging` never crashes the app — if the log file is unavailable it falls back to
    stdout-only.
11. ✅ `RollingFileWriter` + `normalize_log_level` (+ rotation-robustness) unit tests pass;
    `vue-tsc --noEmit` passes.
