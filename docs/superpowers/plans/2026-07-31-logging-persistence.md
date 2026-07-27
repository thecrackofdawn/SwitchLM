# Logging Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist the backend's `tracing` logs to a size-rotated file (`<app_data_dir>/logs/switchlm.log`, 5 MiB × 5), add a live user-configurable log level, capture panics, log each forwarded request at INFO, and expose an "open log folder" action — with zero new dependencies.

**Architecture:** Stay in the `tracing` ecosystem. A small in-repo `RollingFileWriter` (a `std::io::Write`) is wrapped by a blocking `MakeWriter` (`Arc<Mutex<RollingFileWriter>>`) and layered under `tracing_subscriber::fmt`, alongside a stdout layer, both gated by one shared reloadable `LevelFilter`. Blocking writes make panic/exit-safe logging free (no `WorkerGuard`, no flush dance). `tracing`'s default-feature `fmt` already enables `registry`, so the layer cake compiles with **no `Cargo.toml` change**. A persisted `log_level` field + `set_log_level` command drive the reload handle live; `dispatch.rs` gets INFO request-entry/outcome logging at its single chokepoint.

**Tech Stack:** Rust + Tauri v2 + tracing/tracing-subscriber (backend); Vue 3 + Pinia + naive-ui + vue-tsc (frontend).

## Global Constraints

- **Rotation:** `MAX_FILE_SIZE = 5 * 1024 * 1024` (5 MiB), `MAX_FILES = 5` (current + `.1`/`.2`/`.3`/`.4`), file name `switchlm.log`. Verbatim.
- **Log level:** allowed `["trace","debug","info","warn","error"]`, default `"info"`. Stored lowercase as a `String`. Invalid/absent → `"info"` (normalize on read + serde default).
- **Serde:** snake_case, **no** `rename_all` (matches `config/types.rs`). New persisted field uses `#[serde(default = "fn")]` so old config files load at `"info"`.
- **No new crates.** `tracing` + `tracing-subscriber` (both already deps) suffice. Do **not** add `tracing-appender` / `tauri-plugin-log`. No `Cargo.toml` edits.
- **Writer model:** blocking `Arc<Mutex<RollingFileWriter>>` `MakeWriter` (decided; see spec Decision table). Never panic from logging: `rotate()` is best-effort; `init_logging` falls back to stdout-only if the file can't be opened.
- **Tauri arg conversion:** camelCase JS arg keys → snake_case Rust params. `set_log_level` uses param `level` (single word → same key both sides).
- **Privacy:** request logging records routing metadata only (path, model ids, status, latency, hop reasons). **Never** log request/response bodies, headers, or API keys.
- **Tests:** backend via `cd src-tauri && cargo test <name>`; frontend has no unit-test runner — verify via `npx vue-tsc --noEmit` + manual check.
- **Commit only when the task says to.** Each task ends with one conventional-commit commit.

---

## File Structure

| File | Responsibility | Action |
|------|----------------|--------|
| `src-tauri/src/logging/mod.rs` | `RollingFileWriter` (robust size rotation); blocking `FileMaker`/`MakeWriter`; `init_logging` (stdout-only fallback); `LogHandle`; `set_level`; `level_filter_for`; panic hook; unit tests | Create |
| `src-tauri/src/lib.rs` | `pub mod logging;`; remove `try_init`; init logging in `setup` + `app.manage(LogHandle)`; register commands | Modify |
| `src-tauri/src/config/types.rs` | `log_level` field + serde default + constants + `normalize_log_level` + tests | Modify |
| `src-tauri/src/commands.rs` | `SettingsView.log_level`; `get_settings` projection; `set_log_level`; `open_log_dir` (with `create_dir_all`) | Modify |
| `src-tauri/src/proxy/dispatch.rs` | INFO request-entry/outcome logging at `dispatch` boundary + per-fallback-hop INFO lines (additive) | Modify |
| `src/lib/types.ts` | `log_level` in `Settings`/`SettingsView` + `LOG_LEVEL_OPTIONS`/`DEFAULT_LOG_LEVEL` | Modify |
| `src/lib/commands.ts` | `setLogLevel`, `openLogDir` wrappers | Modify |
| `src/stores/system.ts` | `saveLogLevel`, `openLogDir` store actions | Modify |
| `src/views/Settings.vue` | "日志" card (level select + open-folder button) | Modify |

---

### Task 1: `RollingFileWriter` + robust size rotation (TDD, pure)

**Files:**
- Create: `src-tauri/src/logging/mod.rs`
- Modify: `src-tauri/src/lib.rs` (add module declaration only)

**Interfaces:**
- Produces (consumed by Task 2): `pub const MAX_FILE_SIZE: u64`, `pub const MAX_FILES: usize`, `pub const LOG_FILE_NAME: &str`, `pub struct RollingFileWriter { ... }`, `impl RollingFileWriter { pub fn new(dir: &Path) -> io::Result<Self> }`, and `impl std::io::Write for RollingFileWriter`. Reachable as `crate::logging::RollingFileWriter` once Task 1's `pub mod logging;` is added to `lib.rs`.

- [ ] **Step 1: Declare the module**

In `src-tauri/src/lib.rs`, add this line among the other `pub mod` declarations near the top (after `pub mod usage;`):

```rust
pub mod logging;
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/logging/mod.rs` with **only** the test module first (the types it references don't exist yet, so this won't compile — that's the failing test):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_keeps_five_files_and_drops_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = RollingFileWriter::new(dir.path()).unwrap();
        // 25 × 1 MiB ⇒ 5 rotations; ends with current + .1 .2 .3 .4 (5 files), no .5.
        let chunk = vec![b'x'; MAX_FILE_SIZE as usize]; // 1 × cap per write
        for _ in 0..5 {
            for _ in 0..5 {
                w.write_all(&chunk).unwrap();
            }
        }
        w.flush().unwrap();
        for n in 0..MAX_FILES {
            let name = if n == 0 { LOG_FILE_NAME.into() } else { format!("{LOG_FILE_NAME}.{n}") };
            assert!(dir.path().join(name).exists(), "expected file to exist");
        }
        assert!(!dir.path().join(format!("{LOG_FILE_NAME}.{MAX_FILES}")).exists(), ".5 must not exist");
    }

    #[test]
    fn rotate_survives_blocked_rename() {
        let dir = tempfile::tempdir().unwrap();
        // A *directory* at the .1 target makes rename(cur, .1) fail — rotate must swallow it
        // and keep the writer usable (never panic, never leave file=None).
        std::fs::create_dir(dir.path().join(format!("{LOG_FILE_NAME}.1"))).unwrap();
        let mut w = RollingFileWriter::new(dir.path()).unwrap();
        let big = vec![b'x'; MAX_FILE_SIZE as usize + 1]; // single write > cap ⇒ forces rotate
        w.write_all(&big).unwrap();            // rotate fails silently, write itself succeeds
        w.write_all(b"still alive\n").unwrap(); // writer remains usable
        let on_disk = std::fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();
        assert!(on_disk.contains("still alive"));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd src-tauri && cargo test logging`
Expected: compile error — `RollingFileWriter` / constants not defined.

- [ ] **Step 4: Write the implementation**

Prepend the implementation above the `#[cfg(test)] mod tests` block in `src-tauri/src/logging/mod.rs`:

```rust
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5 MiB
pub const MAX_FILES: usize = 5; // switchlm.log + .1 .2 .3 .4
pub const LOG_FILE_NAME: &str = "switchlm.log";

/// A `Write` that appends to `<dir>/switchlm.log` and rotates numbered backups once the current
/// file reaches `MAX_FILE_SIZE`, keeping `MAX_FILES` total. The active handle is held in an
/// `Option` so rotation can close it before renaming (required on Windows). Rotation is
/// best-effort: a blocked rename degrades to "keep appending" rather than leaving the writer dead.
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

    /// Best-effort rotation. Drop the oldest backup, shift the rest up, rename current→.1. Every
    /// `fs` op whose failure is survivable is swallowed; the current file is reopened afterward so
    /// `write()` always has a target. Single-writer invariant (the blocking `MakeWriter` in Task 2
    /// serializes writes through one lock) means no internal lock is needed here.
    fn rotate(&mut self) {
        self.file = None; // close BEFORE rename (Windows cannot rename open files)
        let cur = self.dir.join(LOG_FILE_NAME);
        let _ = std::fs::remove_file(self.dir.join(format!("{LOG_FILE_NAME}.{}", MAX_FILES - 1)));
        for n in (1..MAX_FILES - 1).rev() {
            let from = self.dir.join(format!("{LOG_FILE_NAME}.{n}"));
            let to = self.dir.join(format!("{LOG_FILE_NAME}.{}", n + 1));
            let _ = std::fs::rename(&from, &to); // best-effort; missing/locked files skipped
        }
        let _ = std::fs::rename(&cur, self.dir.join(format!("{LOG_FILE_NAME}.1")));
        self.file = open_log(&self.dir).ok(); // reopen — never leave None
        self.written = 0; // fresh file → 0; rename-blocked → retry after another cap of growth
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

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd src-tauri && cargo test logging`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/logging/mod.rs src-tauri/src/lib.rs
git commit -m "feat(logging): add size-rotating RollingFileWriter (5MiB×5, best-effort)"
```

---

### Task 2: Subscriber wiring — `init_logging`, `LogHandle`, live level, panic hook

**Files:**
- Modify: `src-tauri/src/logging/mod.rs` (append subscriber code)

**Interfaces:**
- Consumes (from Task 1): `RollingFileWriter::new(dir)`, `MAX_FILE_SIZE`, etc.
- Produces (consumed by Tasks 4 & 5): `pub struct LogHandle` (opaque, `Send + Sync + 'static`), `pub fn init_logging(log_dir: &Path, level: LevelFilter) -> LogHandle`, `pub fn set_level(handle: &LogHandle, level: LevelFilter)`, `pub fn level_filter_for(s: &str) -> LevelFilter`.

- [ ] **Step 1: Write the failing test for the pure parser**

In `src-tauri/src/logging/mod.rs` `tests` module, add:

```rust
    #[test]
    fn level_filter_for_parses_and_defaults_to_info() {
        use tracing_subscriber::filter::LevelFilter;
        assert_eq!(level_filter_for("debug"), LevelFilter::DEBUG);
        assert_eq!(level_filter_for("INFO"), LevelFilter::INFO);
        assert_eq!(level_filter_for("warn"), LevelFilter::WARN);
        assert_eq!(level_filter_for("garbage"), LevelFilter::INFO); // fallback
        assert_eq!(level_filter_for(""), LevelFilter::INFO);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test level_filter_for`
Expected: compile error — `level_filter_for` undefined.

- [ ] **Step 3: Append the subscriber implementation**

Append to `src-tauri/src/logging/mod.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::{
    filter::LevelFilter,
    fmt::{self, MakeWriter},
    layer::SubscriberExt,
    reload,
    util::SubscriberInitExt,
};

/// Opaque handle holding the live-level setter. Stored via Tauri managed state. The reload
/// `Handle`'s concrete generic is captured inside a closure so callers never name it.
pub struct LogHandle {
    set_level: Box<dyn Fn(LevelFilter) + Send + Sync>,
}

/// `MakeWriter` that serializes all file writes through one `Mutex` (correct ordering, no
/// interleaving). Each event locks, writes, unlocks; `rotate()` runs under the same lock.
struct FileMaker(Arc<Mutex<RollingFileWriter>>);
impl<'a> MakeWriter<'a> for FileMaker {
    type Writer = GuardWriter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        GuardWriter(self.0.lock().expect("log mutex poisoned"))
    }
}
struct GuardWriter<'a>(std::sync::MutexGuard<'a, RollingFileWriter>);
impl<'a> Write for GuardWriter<'a> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> { self.0.write(b) }
    fn flush(&mut self) -> io::Result<()> { self.0.flush() }
}

/// Install the global subscriber. Writes to a rolling file (ANSI off) + stdout (ANSI on), both
/// gated by one reloadable `LevelFilter`. If the file can't be opened (e.g. held by another
/// instance) it degrades to stdout-only — **never panics**. Blocking writer ⇒ panic lines are on
/// disk before the hook returns; no flush guard is needed.
pub fn init_logging(log_dir: &Path, level: LevelFilter) -> LogHandle {
    let (level_layer, level_handle) = reload::Layer::new(level);

    let stdout_layer = fmt::layer().with_writer(std::io::stdout).with_ansi(true);

    let file_layer = RollingFileWriter::new(log_dir).ok().map(|w| {
        fmt::layer()
            .with_writer(FileMaker(Arc::new(Mutex::new(w))))
            .with_ansi(false)
            .with_target(true)
    });
    if file_layer.is_none() {
        eprintln!("switchlm: log file unavailable at {log_dir:?}, falling back to stdout-only");
    }

    let base = tracing_subscriber::registry()
        .with(level_layer)
        .with(stdout_layer);
    match file_layer {
        Some(fl) => base.with(fl).init(),
        None => base.init(),
    }

    install_panic_hook();

    let set_level: Box<dyn Fn(LevelFilter) + Send + Sync> = Box::new(move |lvl| {
        let _ = level_handle.modify(|current| *current = lvl);
    });
    LogHandle { set_level }
}

/// Apply a new level live (no restart).
pub fn set_level(handle: &LogHandle, level: LevelFilter) {
    (handle.set_level)(level);
}

/// Parse a level string ("info"/"debug"/…) into a `LevelFilter`, falling back to INFO.
pub fn level_filter_for(s: &str) -> LevelFilter {
    s.parse::<Level>().map(LevelFilter::from).unwrap_or(LevelFilter::INFO)
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        tracing::error!("panic: {info} at {loc}");
        prev(info);
    }));
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test level_filter_for`
Expected: PASS. Then run `cd src-tauri && cargo build` — must compile cleanly (this verifies the layer-cake typing; `registry()` is available because `fmt` is a default feature and enables `registry`).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/logging/mod.rs
git commit -m "feat(logging): init subscriber (file+stdout, live level, panic hook)"
```

---

### Task 3: Persisted `log_level` setting + `normalize_log_level` (TDD)

**Files:**
- Modify: `src-tauri/src/config/types.rs` (`Settings` struct, `Default` impl, constants, tests)

**Interfaces:**
- Produces (consumed by Tasks 4 & 5): `pub const ALLOWED_LOG_LEVELS: &[&str]`, `pub const DEFAULT_LOG_LEVEL: &str`, field `Settings.log_level: String`, `pub fn normalize_log_level(s: &str) -> String`. Re-exported by `config/mod.rs` (`pub use types::*`) → `crate::config::normalize_log_level` etc.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/config/types.rs`, inside the existing `#[cfg(test)] mod tests` block, add (and update `app_config_roundtrips`):

Update the `app_config_roundtrips` settings literal (the `Settings { port: 6950, autostart: false, usage_refresh_interval_secs: 60 }` line) to add the field:

```rust
            settings: Settings { port: 6950, autostart: false, usage_refresh_interval_secs: 60, log_level: "info".into() },
```

And add inside `app_config_roundtrips` (after the existing `usage_refresh_interval_secs` assertion):

```rust
        assert_eq!(back.settings.log_level, "info");
```

Then append these tests to the `tests` mod:

```rust
    #[test]
    fn normalize_log_level_lowercases_and_validates() {
        assert_eq!(normalize_log_level("INFO"), "info");
        assert_eq!(normalize_log_level("Debug"), "debug");
        assert_eq!(normalize_log_level("warn"), "warn");
        assert_eq!(normalize_log_level(""), "info");      // empty -> default
        assert_eq!(normalize_log_level("verbose"), "info"); // unknown -> default
    }

    #[test]
    fn settings_log_level_defaults_when_absent() {
        let json = r#"{"port": 7000, "autostart": true}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.log_level, DEFAULT_LOG_LEVEL);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib config::types`
Expected: compile error — `log_level`/`normalize_log_level`/`DEFAULT_LOG_LEVEL` undefined.

- [ ] **Step 3: Write the implementation**

In `src-tauri/src/config/types.rs`, add constants near the existing usage-refresh constants (after `MAX_USAGE_REFRESH_SECS`):

```rust
pub const ALLOWED_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
pub const DEFAULT_LOG_LEVEL: &str = "info";
```

Add the field + default fn to `Settings`, and the normalize helper:

```rust
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
impl Default for Settings {
    fn default() -> Self {
        Self {
            port: default_port(),
            autostart: false,
            usage_refresh_interval_secs: DEFAULT_USAGE_REFRESH_SECS,
            log_level: DEFAULT_LOG_LEVEL.into(),
        }
    }
}
fn default_log_level() -> String {
    DEFAULT_LOG_LEVEL.into()
}

/// Normalize a stored/passed log level: lowercase; if not one of the five allowed, fall back to
/// `info`. Applied on read so a hand-edited config can never set an invalid level. Returns an
/// owned `String` (cloned once per call; only on settings read/save).
pub fn normalize_log_level(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if ALLOWED_LOG_LEVELS.contains(&lower.as_str()) {
        lower
    } else {
        DEFAULT_LOG_LEVEL.into()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib config`
Expected: PASS (all config tests, including the new ones + updated roundtrip).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config/types.rs
git commit -m "feat(logging): persist log_level setting with normalize (default info)"
```

---

### Task 4: Wire `init_logging` into startup + manage `LogHandle`

**Files:**
- Modify: `src-tauri/src/lib.rs` (top of `run()`; `setup` closure)

**Interfaces:**
- Consumes (from Tasks 1–3): `crate::logging::{init_logging, set_level, level_filter_for, LogHandle}`, `Settings.log_level`.
- Produces: a managed `LogHandle` (`app.manage`), reachable as `State<'_, LogHandle>` by Task 5.

- [ ] **Step 1: Remove the old stdout-only init**

In `src-tauri/src/lib.rs`, delete this line at the top of `pub fn run()`:

```rust
    let _ = tracing_subscriber::fmt::try_init();
```

- [ ] **Step 2: Initialize logging at the start of `setup`**

In the `.setup(|app| { ... })` closure, the first line is `let dir = app.path().app_data_dir()?;`. Immediately after it (and **before** `config::store::migrate_usage_sk_to_keyring(...)`, so migration warnings are captured), add:

```rust
            let log_dir = dir.join("logs");
            let log_handle = crate::logging::init_logging(
                &log_dir,
                tracing_subscriber::filter::LevelFilter::INFO,
            );
```

- [ ] **Step 3: Apply the persisted level once config is loaded**

Find `let preferred = cfg.settings.port;` (after `let mut cfg = config::store::load(&dir)?;` and the legacy-vendor normalize). Immediately after that line, add:

```rust
            crate::logging::set_level(
                &log_handle,
                crate::logging::level_filter_for(&cfg.settings.log_level),
            );
```

- [ ] **Step 4: Manage the `LogHandle`**

Find `app.manage(state);` and immediately after it add:

```rust
            app.manage(log_handle);
```

- [ ] **Step 5: Verify it builds and logs**

Run: `cd src-tauri && cargo build`
Expected: compiles. Then optionally run `cd src-tauri && cargo tauri dev`, trigger any action (e.g. open Settings), and confirm `<app_data_dir>/logs/switchlm.log` is created with timestamped lines. (Manual; the dev console also shows stdout.)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(logging): init file logging at startup + manage LogHandle"
```

---

### Task 5: `set_log_level` + `open_log_dir` commands + `SettingsView.log_level`

**Files:**
- Modify: `src-tauri/src/commands.rs` (`SettingsView`, `get_settings`, new commands, imports)
- Modify: `src-tauri/src/lib.rs` (`generate_handler!`)

**Interfaces:**
- Consumes: `crate::logging::{LogHandle, set_level, level_filter_for}`, `crate::config::normalize_log_level`, the `persist` helper (`persist(&app, &cfg) -> Result<(), String>` at `commands.rs:827`).
- Produces: Tauri commands `set_log_level { state, log, app, level } -> Result<(), String>` and `open_log_dir { app } -> Result<(), String>`; `SettingsView.log_level`.

- [ ] **Step 1: Add `log_level` to `SettingsView` and project it on read**

In `src-tauri/src/commands.rs`, update the import block (top of file) — add `normalize_log_level` to the existing `use crate::config::{ ... }` and add a `use crate::logging::LogHandle;` line:

```rust
use crate::config::{
    clamp_usage_refresh_secs, normalize_log_level, AppConfig, ContextCheckResult,
    ContextCheckStatus, MAX_USAGE_REFRESH_SECS, MIN_USAGE_REFRESH_SECS, Model, ModelCatalog,
    Profile, Provider,
};
use crate::logging::LogHandle;
```

Update `SettingsView` (currently `port`, `autostart`, `usage_refresh_interval_secs`) to add the field:

```rust
/// Read-only view of app settings (port + autostart + usage refresh interval + log level).
#[derive(Serialize)]
pub struct SettingsView {
    pub port: u16,
    pub autostart: bool,
    pub usage_refresh_interval_secs: u32,
    pub log_level: String,
}
```

Update `get_settings` to project the normalized level:

```rust
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<SettingsView, String> {
    let cfg = state.config.read().await;
    Ok(SettingsView {
        port: cfg.settings.port,
        autostart: cfg.settings.autostart,
        usage_refresh_interval_secs: clamp_usage_refresh_secs(cfg.settings.usage_refresh_interval_secs),
        log_level: normalize_log_level(&cfg.settings.log_level),
    })
}
```

- [ ] **Step 2: Add the `set_log_level` command**

Add near `set_usage_refresh_interval` (mirrors its validate-then-persist shape):

```rust
/// Update the log level: normalize, persist, and apply live via the reload filter (no restart).
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
        config.settings.log_level = normalized.clone();
        persist(&app, &config)?;
    }
    crate::logging::set_level(&log, crate::logging::level_filter_for(&normalized));
    Ok(())
}
```

- [ ] **Step 3: Add the `open_log_dir` command**

```rust
/// Open the logs folder in the OS file manager (creates it first as a safety net).
#[tauri::command]
pub async fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener().open_location(dir, None::<&str>).map_err(|e| e.to_string())
}
```

- [ ] **Step 4: Register both commands**

In `src-tauri/src/lib.rs`, inside `tauri::generate_handler![ ... ]`, add these two entries (e.g. after `commands::set_usage_refresh_interval,`):

```rust
            commands::set_log_level,
            commands::open_log_dir,
```

- [ ] **Step 5: Verify it builds and existing tests pass**

Run: `cd src-tauri && cargo build && cargo test`
Expected: compiles; all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(logging): set_log_level (live) + open_log_dir commands"
```

---

### Task 6: Per-request INFO logging at the `dispatch` boundary

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` (`dispatch`, helper, fallback advance points)

**Interfaces:**
- Consumes: nothing new — `tracing` is already in scope crate-wide. `dispatch`'s signature is unchanged: `pub async fn dispatch(state: AppState, body: Bytes, protocol: ClientProtocol) -> Result<Response<Body>, ProxyError>`.
- Produces: additive INFO logs only (no API change). Edges (`openai_edge`, `anthropic_edge`) are untouched.

- [ ] **Step 1: Add the entry + outcome logging around the dispatch body**

In `src-tauri/src/proxy/dispatch.rs`, add `use std::time::Instant;` to the imports at the top. Then replace the tail of `dispatch` (the `if is_stream { ... } else { ... }` block that currently just returns the inner result) with a wrapped version that logs entry and outcome. The full new `dispatch` body from the resolve block onward:

```rust
    let requested = req.get("model").and_then(|v| v.as_str()).unwrap_or("-").to_string();
    tracing::info!(
        target: "switchlm::proxy",
        inbound = path_for(protocol),
        requested = %requested,
        model = %start_model_id,
        stream = is_stream,
        "forward request",
    );

    let start = Instant::now();
    let result = if is_stream {
        dispatch_stream(&state, &req, protocol, &start_model_id, &echo_model).await
    } else {
        dispatch_non_stream(&state, &req, protocol, &start_model_id, &echo_model).await
    };

    let ms = start.elapsed().as_millis();
    match &result {
        Ok(resp) => tracing::info!(
            target: "switchlm::proxy",
            status = resp.status().as_u16(),
            ms,
            "forward ok"
        ),
        Err(e) => tracing::warn!(
            target: "switchlm::proxy",
            error = %e,
            ms,
            "forward failed"
        ),
    }
    result
}
```

Note: the existing block already computes `(start_model_id, echo_model, is_stream)` and `requested` from `req.get("model")`; lift `requested` to a `String` before the entry log (as shown) and reuse it. Keep the early `Err(ProxyError::NotConfigured)` / resolve-error returns above this block untouched.

- [ ] **Step 2: Add the `path_for` helper**

Add near the other small helpers (e.g. next to `has_backend`):

```rust
fn path_for(p: ClientProtocol) -> &'static str {
    match p {
        ClientProtocol::OpenAI => "/v1/chat/completions",
        ClientProtocol::Anthropic => "/v1/messages",
    }
}
```

- [ ] **Step 3: Add a fallback-hop INFO line at each advance point**

At every spot in `dispatch_non_stream` and `dispatch_stream` where `last_error` is set and the chain advances to a fallback (the `match next_fallback_id(state, &current).await { Some(next) => { current = next; continue; } ... }` branches, and the stream `advance_or_exhausted!` macro), add one INFO line right after `last_error` is assigned, before the advance:

```rust
            tracing::info!(target: "switchlm::proxy", from = %current, reason = %last_error, "fallback hop");
```

For the stream path, the cleanest single insertion is inside the `advance_or_exhausted!` macro body (it already sets `last_error = $err;`):

```rust
        macro_rules! advance_or_exhausted {
            ($err:expr) => {{
                last_error = $err;
                tracing::info!(target: "switchlm::proxy", from = %current, reason = %last_error, "fallback hop");
                match next_fallback_id(state, &current).await {
                    Some(n) => { current = n; continue; }
                    None => return Err(ProxyError::FallbackExhausted { tried, last_error }),
                }
            }};
        }
```

(For `dispatch_non_stream`, add the same `tracing::info!` line immediately after each `last_error = format!(...);` that precedes a fallback `continue`.)

- [ ] **Step 4: Verify existing dispatch tests stay green**

Run: `cd src-tauri && cargo test --lib proxy::dispatch`
Expected: PASS (all existing fallback/rate-limit tests; logging is side-effect-free).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs
git commit -m "feat(proxy): log each forwarded request at INFO (entry/outcome/fallback hops)"
```

---

### Task 7: Frontend — log-level setting + open-folder in Settings

**Files:**
- Modify: `src/lib/types.ts`, `src/lib/commands.ts`, `src/stores/system.ts`, `src/views/Settings.vue`

**Interfaces:**
- Consumes: Tauri commands `set_log_level` / `open_log_dir` (Task 5); `get_settings` now returns `log_level`.
- Produces: typed wrappers `setLogLevel`/`openLogDir`, store actions `saveLogLevel`/`openLogDir`, and a "日志" card in Settings.

- [ ] **Step 1: Mirror the field + options in `types.ts`**

In `src/lib/types.ts`, add `log_level: string;` to both `Settings` and `SettingsView`, and export the level options/constants near the existing usage-refresh constants:

```ts
export interface Settings {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
  log_level: string;
}

export interface SettingsView {
  port: number;
  autostart: boolean;
  usage_refresh_interval_secs: number;
  log_level: string;
}

// Mirror src-tauri/src/config/types.rs — keep in sync.
export const LOG_LEVEL_OPTIONS = ["trace", "debug", "info", "warn", "error"] as const;
export const DEFAULT_LOG_LEVEL = "info";
```

- [ ] **Step 2: Add invoke wrappers in `commands.ts`**

In `src/lib/commands.ts`, after `setUsageRefreshInterval`:

```ts
export const setLogLevel = (level: string) => invoke<void>("set_log_level", { level });
export const openLogDir = () => invoke<void>("open_log_dir");
```

- [ ] **Step 3: Add store actions in `system.ts`**

In `src/stores/system.ts`, add these actions (after `saveUsageRefreshInterval`) and export them:

```ts
  async function saveLogLevel(level: string) {
    await api.setLogLevel(level);
    await loadSettings();
  }
  async function openLogDir() {
    await api.openLogDir();
  }
```

And add `saveLogLevel, openLogDir,` to the returned object.

- [ ] **Step 4: Add the "日志" card in `Settings.vue`**

In `src/views/Settings.vue`, import `NSelect` from `naive-ui` and `LOG_LEVEL_OPTIONS` from `../lib/types`. Add a reactive ref + handlers:

```ts
import { NButton, NCard, NInputNumber, NSelect, NSpace, NSwitch, useDialog, useMessage } from "naive-ui";
import { LOG_LEVEL_OPTIONS } from "../lib/types";

const levelOptions = LOG_LEVEL_OPTIONS.map((v) => ({ label: v, value: v }));
const levelInput = ref<string>("info");
watch(
  () => system.settings,
  (s) => { if (s) levelInput.value = s.log_level; },
  { immediate: true },
);
async function saveLevel() {
  if (!levelInput.value) return;
  try {
    await system.saveLogLevel(levelInput.value);
    msg.success("日志级别已保存");
  } catch (e) {
    msg.error(`保存失败：${String(e)}`);
  }
}
async function openLogs() {
  try {
    await system.openLogDir();
  } catch (e) {
    msg.error(`打开失败：${String(e)}`);
  }
}
```

Add a new card (e.g. after the "用量刷新间隔" card):

```vue
    <NCard title="日志" size="small">
      <NSpace align="center" :size="12">
        <NSelect v-model:value="levelInput" :options="levelOptions" style="width: 140px" />
        <NButton type="primary" @click="saveLevel">保存</NButton>
        <NButton @click="openLogs">打开日志文件夹</NButton>
        <span class="muted">默认 info · 调高可见更详细日志(立即生效),用于排查问题</span>
      </NSpace>
    </NCard>
```

- [ ] **Step 5: Verify the frontend type-checks**

Run: `npx vue-tsc --noEmit`
Expected: no errors.

- [ ] **Step 6: Manual smoke + commit**

Run `cd src-tauri && cargo tauri dev`. In Settings → 日志: change level to `debug`, save (no restart), send a request through the proxy, and confirm debug lines appear in `switchlm.log`; set back to `info` and confirm new debug lines stop. Click "打开日志文件夹" and confirm the OS file manager opens at `…/SwitchLM/logs/`.

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts src/views/Settings.vue
git commit -m "feat(settings): log-level select + open-log-folder action"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** File target/rotation (Task 1), subscriber+stdout+reload (Task 2), live level (Tasks 3–5), panic hook (Task 2), backward-compat/normalize (Task 3), open-folder (Task 5/7), per-request INFO + hops (Task 6), frontend (Task 7). All Functional Requirements 1–9 and Non-Functional 1–6 map to tasks. ✓
- **Placeholder scan:** none — every code step contains real code; no "TODO"/"similar to". ✓
- **Type consistency:** `LogHandle`/`init_logging`/`set_level`/`level_filter_for` names match across Tasks 2/4/5; `Settings.log_level` + `normalize_log_level` match across Tasks 3/5; `path_for`/`ClientProtocol` match Task 6; frontend `log_level`/`setLogLevel`/`openLogDir` match Task 7. `persist(&app, &cfg)` signature verified at `commands.rs:827`. ✓
- **Correction logged:** the spec's `rotate()` shift was off-by-one (kept 6 files); both spec sketch and Task 1 use the corrected drop-oldest-then-shift. ✓
