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

use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::{
    filter::LevelFilter,
    fmt::{self, format::Writer, time::FormatTime, MakeWriter},
    layer::SubscriberExt,
    reload,
    util::SubscriberInitExt,
};

/// Formats log timestamps in the system local timezone (e.g. `2026-08-01T16:32:53.722+08:00`)
/// instead of tracing-subscriber's default `SystemTime`, which renders UTC (`...Z`). Users read
/// log lines against their wall clock; the UTC default left CN (UTC+8) logs 8h behind. Uses
/// `chrono::Local` (already a dependency via the `clock` feature) so no new feature flag is needed.
struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().to_rfc3339())
    }
}

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
        GuardWriter(self.0.lock().unwrap_or_else(|e| e.into_inner()))
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

    let stdout_layer = fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .with_timer(LocalTimer);

    let file_layer = RollingFileWriter::new(log_dir).ok().map(|w| {
        fmt::layer()
            .with_writer(FileMaker(Arc::new(Mutex::new(w))))
            .with_ansi(false)
            .with_target(true)
            .with_timer(LocalTimer)
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
    fn rotate_survives_preexisting_dot1_dir() {
        let dir = tempfile::tempdir().unwrap();
        // A *directory* pre-exists at the .1 slot. rotate()'s shift loop renames .1 → .2 FIRST
        // (renaming a directory succeeds on every platform), which frees .1, so the final
        // rename(cur, .1) succeeds. This is therefore a HAPPY-PATH robustness check (writer
        // survives a pre-existing .1 entry), NOT a blocked-rename test — see
        // `rotate_handles_rename_failure` for the genuine degrade path.
        std::fs::create_dir(dir.path().join(format!("{LOG_FILE_NAME}.1"))).unwrap();
        let mut w = RollingFileWriter::new(dir.path()).unwrap();
        let big = vec![b'x'; MAX_FILE_SIZE as usize + 1]; // single write > cap ⇒ forces rotate
        w.write_all(&big).unwrap();            // rotate runs, writer stays usable
        w.write_all(b"still alive\n").unwrap(); // writer remains usable
        let on_disk = std::fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();
        assert!(on_disk.contains("still alive"));
    }

    /// Genuine degrade-path test: force rotate's `rename(cur, .1)` to FAIL and confirm the writer
    /// does not panic and stays usable (existing file reopened, no `.1` produced). On Unix a
    /// read-only log dir reliably blocks every rename/remove inside it with `EACCES`.
    ///
    /// Gated to unix because there is no portable std-only way to force a rename failure on
    /// Windows: the read-only attribute on a directory is ignored for child renames there (a
    /// read-only file is also freely renamed — both verified empirically), and std's `File` does
    /// not expose the restrictive share mode that would block rename via a held handle.
    #[cfg(unix)]
    #[test]
    fn rotate_handles_rename_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut w = RollingFileWriter::new(dir.path()).unwrap();
        // Strip write permission on the dir ⇒ rename/remove of any entry inside it fail (EACCES),
        // while reopening the still-existing `switchlm.log` for append keeps working.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let big = vec![b'x'; MAX_FILE_SIZE as usize + 1]; // forces rotate
        w.write_all(&big).unwrap();            // rename fails silently, no panic
        w.write_all(b"still alive\n").unwrap(); // file reopened by rotate ⇒ writer recovered
        // Restore perms so the tempdir can be cleaned up on drop.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();
        assert!(on_disk.contains("still alive"), "writer must stay usable after a failed rotate");
        // rename failed ⇒ no .1 backup was created (proves the failure path was actually taken)
        assert!(
            !dir.path().join(format!("{LOG_FILE_NAME}.1")).exists(),
            ".1 must not exist when rename(cur,.1) failed"
        );
    }

    #[test]
    fn level_filter_for_parses_and_defaults_to_info() {
        use tracing_subscriber::filter::LevelFilter;
        assert_eq!(level_filter_for("debug"), LevelFilter::DEBUG);
        assert_eq!(level_filter_for("INFO"), LevelFilter::INFO);
        assert_eq!(level_filter_for("warn"), LevelFilter::WARN);
        assert_eq!(level_filter_for("garbage"), LevelFilter::INFO); // fallback
        assert_eq!(level_filter_for(""), LevelFilter::INFO);
    }
}
