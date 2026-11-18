//! A rotating log file and crash reports, both under `<config>/logs/`.
//!
//! Each run writes `aitrack-<date>-<time>.log` (the last 5 are kept). A panic
//! writes `crash-<date>-<time>.txt` with the message, place, backtrace, version
//! and system. At the next start the newest unseen report is offered to the user
//! (copy it, open an issue); nothing is ever sent by itself.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const KEEP_LOGS: usize = 5;
const KEEP_CRASHES: usize = 5;
/// A runaway log stops growing here.
const MAX_LOG_BYTES: u64 = 20 * 1024 * 1024;
pub const ISSUES_URL: &str = "https://github.com/crhistian-cornejo/AI-Track/issues/new";

pub fn logs_dir() -> PathBuf {
    crate::appearance::config_dir().join("logs")
}

/// `20261117-103000` for a Unix time, in UTC.
pub fn stamp(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Keep the newest `keep` files whose names start with `prefix` and end with `suffix`.
fn prune(dir: &Path, prefix: &str, suffix: &str, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix))).collect();
    names.sort();
    let drop = names.len().saturating_sub(keep);
    for old in names.into_iter().take(drop) {
        let _ = fs::remove_file(old);
    }
}

struct FileLogger {
    file: Mutex<File>,
    written: AtomicUsize,
    all: log::LevelFilter,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.all || (m.target().starts_with("aitrack") && m.level() <= log::Level::Info)
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) || self.written.load(Ordering::Relaxed) as u64 > MAX_LOG_BYTES {
            return;
        }
        let line = format!("{} {:5} {} {}\n", stamp(now_secs()), r.level(), r.target(), r.args());
        self.written.fetch_add(line.len(), Ordering::Relaxed);
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// Start the log file and the panic hook. Skipped for stub runs and hidden
/// visual checks, which would only fill the folder.
pub fn init() {
    if std::env::var_os("AITRACK_STUBS").is_some() || crate::dev_hidden::enabled() {
        return;
    }
    let dir = logs_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    prune(&dir, "aitrack-", ".log", KEEP_LOGS - 1);
    let path = dir.join(format!("aitrack-{}.log", stamp(now_secs())));
    if let Ok(file) = File::create(&path) {
        // `AITRACK_LOG=debug` (or trace…) opens up every crate.
        let all = std::env::var("AITRACK_LOG").ok().and_then(|v| v.parse().ok()).unwrap_or(log::LevelFilter::Warn);
        let logger = Box::new(FileLogger { file: Mutex::new(file), written: AtomicUsize::new(0), all });
        if log::set_boxed_logger(logger).is_ok() {
            log::set_max_level(all.max(log::LevelFilter::Info));
        }
    }
    install_panic_hook(dir);
    log::info!("AI-Track {} on {} {} started", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
}

fn install_panic_hook(dir: PathBuf) {
    static WRITTEN: AtomicUsize = AtomicUsize::new(0);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        // A panic in a loop must not fill the disk.
        if WRITTEN.fetch_add(1, Ordering::Relaxed) >= 3 {
            return;
        }
        let thread = std::thread::current();
        let place = info.location().map_or("unknown".to_owned(), |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let message = info.payload().downcast_ref::<&str>().map(|s| (*s).to_owned()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "(no message)".into());
        let report = report_text(&message, &place, thread.name().unwrap_or("unnamed"), &std::backtrace::Backtrace::force_capture().to_string());
        log::error!("panic at {place}: {message}");
        let path = dir.join(format!("crash-{}-{}.txt", stamp(now_secs()), WRITTEN.load(Ordering::Relaxed)));
        let _ = fs::write(path, report);
        prune(&dir, "crash-", ".txt", KEEP_CRASHES);
    }));
}

pub fn report_text(message: &str, place: &str, thread: &str, backtrace: &str) -> String {
    format!(
        "AI-Track crash report\nversion: {}\nsystem: {} {}\nwhen (UTC): {}\nthread: {thread}\nmessage: {message}\nlocation: {place}\n\nbacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        stamp(now_secs()),
    )
}

/// Looks for an unseen crash report on a background thread; the answer arrives on the channel.
pub fn spawn_crash_lookup() -> crossbeam_channel::Receiver<CrashNotice> {
    let (tx, rx) = crossbeam_channel::unbounded();
    if std::env::var_os("AITRACK_STUBS").is_none() && !crate::dev_hidden::enabled() {
        std::thread::spawn(move || {
            if let Some(n) = take_unseen_crash(&logs_dir()) {
                let _ = tx.send(n);
            }
        });
    }
    rx
}

/// A report from an earlier run, to offer to the user.
#[derive(Clone, Debug, PartialEq)]
pub struct CrashNotice {
    pub text: String,
}

/// The newest crash report not yet shown. It is marked as seen (renamed) so the
/// offer is made once; the file stays for the user.
pub fn take_unseen_crash(dir: &Path) -> Option<CrashNotice> {
    let entries = fs::read_dir(dir).ok()?;
    let mut reports: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt") && !n.contains(".seen"))).collect();
    reports.sort();
    let newest = reports.last()?.clone();
    let text = fs::read_to_string(&newest).ok()?;
    // Everything unseen counts as seen: one offer, for the newest.
    for p in &reports {
        let _ = fs::rename(p, p.with_extension("seen.txt"));
    }
    Some(CrashNotice { text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitrack-logging-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stamps_are_calendar_dates() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_700_000_000), "20231114-221320");
        assert_eq!(stamp(1_709_164_800 + 3661), "20240229-010101");
    }

    #[test]
    fn only_the_newest_files_are_kept() {
        let d = scratch("prune");
        for n in ["aitrack-1.log", "aitrack-2.log", "aitrack-3.log", "crash-1.txt", "other.log"] {
            fs::write(d.join(n), "x").unwrap();
        }
        prune(&d, "aitrack-", ".log", 2);
        let mut left: Vec<String> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["aitrack-2.log", "aitrack-3.log", "crash-1.txt", "other.log"]);
    }

    #[test]
    fn a_crash_is_offered_once_and_the_file_is_kept() {
        let d = scratch("crash");
        assert!(take_unseen_crash(&d).is_none());
        fs::write(d.join("crash-20261117-100000-1.txt"), "old").unwrap();
        fs::write(d.join("crash-20261117-110000-1.txt"), report_text("boom", "a.rs:1:1", "main", "frames")).unwrap();
        let n = take_unseen_crash(&d).unwrap();
        assert!(n.text.contains("message: boom") && n.text.contains("version:"));
        assert!(take_unseen_crash(&d).is_none(), "offered once");
        assert!(d.join("crash-20261117-110000-1.seen.txt").is_file());
    }
}
