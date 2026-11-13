//! Plugin discovery and scanning.
//!
//! Flow for each `.vst3` / `.clap` found under the search paths:
//!
//! 1. Same path + `mtime` + size as the cache → reuse the cached result.
//! 2. Same key in the blocklist → skip (until the file changes or the user
//!    calls [`Scanner::unblock`]).
//! 3. VST3 with a valid `moduleinfo.json` → read it, no binary loaded.
//! 4. Otherwise run `aitrack-plugin-scanner` on it with a timeout (20 s by
//!    default). A crash, hang or error blocklists the file; the DAW is
//!    never at risk.
//!
//! Several scanner processes run in parallel. The cache is saved after the
//! scan with an atomic rename.

pub mod cache;
pub mod moduleinfo;
pub mod paths;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aitrack_model::PluginFormat;
use serde::{Deserialize, Serialize};

pub use cache::{BlockEntry, BlockReason, CacheEntry, FileKey, ScanCache};
pub use paths::{default_search_paths, find_plugin_files};

use crate::error::{HostError, Result};
use crate::info::HostedPluginInfo;

/// Prefix of the result line printed by `aitrack-plugin-scanner`. Plugins
/// may print anything to stdout; only the last line with this prefix counts.
pub const RESULT_SENTINEL: &str = "@@AITRACK_SCAN_RESULT@@";
pub const SCANNER_EXE: &str = if cfg!(windows) { "aitrack-plugin-scanner.exe" } else { "aitrack-plugin-scanner" };
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPayload {
    Ok(Vec<HostedPluginInfo>),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanFailure {
    Timeout,
    Crashed(String),
    Failed(String),
    /// The scanner itself could not run (not a plugin fault; not blocked).
    Spawn(String),
}

impl ScanFailure {
    fn block_reason(&self) -> Option<BlockReason> {
        match self {
            ScanFailure::Timeout => Some(BlockReason::Timeout),
            ScanFailure::Crashed(_) => Some(BlockReason::Crashed),
            ScanFailure::Failed(_) => Some(BlockReason::Failed),
            ScanFailure::Spawn(_) => None,
        }
    }

    fn message(&self) -> String {
        match self {
            ScanFailure::Timeout => "timed out".into(),
            ScanFailure::Crashed(m) | ScanFailure::Failed(m) | ScanFailure::Spawn(m) => m.clone(),
        }
    }
}

/// Load the module in *this* process and describe its plugins (what the
/// scanner binary does). Only call this from a disposable process.
pub fn scan_in_process(format: PluginFormat, path: &Path) -> Result<Vec<HostedPluginInfo>> {
    match format {
        PluginFormat::Vst3 => crate::vst3::probe(path),
        PluginFormat::Clap => crate::clap::probe(path),
    }
}

/// Run the scanner executable on one plugin file, killing it after
/// `timeout`.
pub fn scan_out_of_process(
    exe: &Path,
    format: PluginFormat,
    path: &Path,
    timeout: Duration,
) -> std::result::Result<Vec<HostedPluginInfo>, ScanFailure> {
    let fmt = match format {
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Clap => "clap",
    };
    let mut child = Command::new(exe)
        .arg(fmt)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ScanFailure::Spawn(format!("{}: {e}", exe.display())))?;
    // Drain stdout on a thread so a chatty plugin cannot fill the pipe and
    // block the child.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stdout.read_to_end(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(ScanFailure::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(ScanFailure::Spawn(e.to_string())),
        }
    };
    let out = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&out);
    let payload = text.lines().rev().find_map(|l| l.strip_prefix(RESULT_SENTINEL));
    match payload.map(serde_json::from_str::<ScanPayload>) {
        Some(Ok(ScanPayload::Ok(infos))) => Ok(infos),
        Some(Ok(ScanPayload::Error(e))) => Err(ScanFailure::Failed(e)),
        Some(Err(e)) => Err(ScanFailure::Crashed(format!("unreadable result: {e}"))),
        None => Err(ScanFailure::Crashed(format!("scanner exited with {status} and no result"))),
    }
}

/// Entry point of the `aitrack-plugin-scanner` binary:
/// `aitrack-plugin-scanner <vst3|clap> <path>`.
pub fn scanner_main(args: &[String]) -> i32 {
    let (Some(fmt), Some(path)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: aitrack-plugin-scanner <vst3|clap> <path>");
        return 2;
    };
    let format = match fmt.as_str() {
        "vst3" => PluginFormat::Vst3,
        "clap" => PluginFormat::Clap,
        _ => {
            eprintln!("unknown format {fmt}");
            return 2;
        }
    };
    let payload = match std::panic::catch_unwind(|| scan_in_process(format, Path::new(path))) {
        Ok(Ok(infos)) if infos.is_empty() => ScanPayload::Error("no plugins in module".into()),
        Ok(Ok(infos)) => ScanPayload::Ok(infos),
        Ok(Err(e)) => ScanPayload::Error(e.to_string()),
        Err(_) => ScanPayload::Error("plugin panicked during scan".into()),
    };
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{\"error\":\"serialise\"}".into());
    // A fresh line, in case the plugin left a partial line on stdout.
    println!("\n{RESULT_SENTINEL}{json}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // Skip plugin destructors/atexit handlers, which can hang or crash.
    std::process::exit(0);
}

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    /// Plugin files found on disk.
    pub files: usize,
    pub cached: usize,
    pub from_moduleinfo: usize,
    pub scanned: usize,
    pub skipped_blocked: usize,
    /// Newly blocklisted files with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Cache entries dropped because their file disappeared.
    pub removed: usize,
}

pub struct Scanner {
    cache_path: PathBuf,
    cache: ScanCache,
    scanner_exe: Option<PathBuf>,
    timeout: Duration,
    user_paths: Vec<(PluginFormat, PathBuf)>,
    use_default_paths: bool,
    parallelism: usize,
}

impl Scanner {
    /// A scanner persisting to `cache_path` (loaded now if it exists).
    pub fn new(cache_path: impl Into<PathBuf>) -> Self {
        let cache_path = cache_path.into();
        let cache = ScanCache::load(&cache_path);
        let parallelism = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
        Self {
            cache_path,
            cache,
            scanner_exe: default_scanner_exe(),
            timeout: DEFAULT_TIMEOUT,
            user_paths: Vec::new(),
            use_default_paths: true,
            parallelism,
        }
    }

    pub fn with_scanner_exe(mut self, exe: impl Into<PathBuf>) -> Self {
        self.scanner_exe = Some(exe.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Scan only the user paths (tests, "rescan this folder").
    pub fn without_default_paths(mut self) -> Self {
        self.use_default_paths = false;
        self
    }

    pub fn add_search_path(&mut self, format: PluginFormat, path: impl Into<PathBuf>) {
        self.user_paths.push((format, path.into()));
    }

    pub fn cache(&self) -> &ScanCache {
        &self.cache
    }

    pub fn has_scanner_exe(&self) -> bool {
        self.scanner_exe.is_some()
    }

    pub fn plugins(&self) -> Vec<HostedPluginInfo> {
        self.cache.plugins().cloned().collect()
    }

    pub fn blocklist(&self) -> impl Iterator<Item = (&PathBuf, &BlockEntry)> {
        self.cache.blocklist.iter()
    }

    /// Allow a blocklisted plugin to be scanned again.
    pub fn unblock(&mut self, path: &Path) -> bool {
        self.cache.blocklist.remove(path).is_some()
    }

    pub fn save(&self) -> Result<()> {
        self.cache.save(&self.cache_path)
    }

    /// Every plugin file in the default and user search paths.
    pub fn discover(&self) -> Vec<(PluginFormat, PathBuf)> {
        let mut out = Vec::new();
        for format in [PluginFormat::Vst3, PluginFormat::Clap] {
            let mut roots: Vec<PathBuf> = if self.use_default_paths { default_search_paths(format) } else { Vec::new() };
            roots.extend(self.user_paths.iter().filter(|(f, _)| *f == format).map(|(_, p)| p.clone()));
            out.extend(find_plugin_files(&roots, format).into_iter().map(|p| (format, p)));
        }
        out
    }

    /// Incremental scan of all search paths; saves the cache.
    pub fn scan(&mut self) -> Result<ScanReport> {
        let files = self.discover();
        let mut report = self.scan_files(&files);
        // Forget plugins whose files are gone (keep blocklist entries for
        // files that still exist).
        let present: std::collections::HashSet<&PathBuf> = files.iter().map(|(_, p)| p).collect();
        let before = self.cache.entries.len();
        self.cache.entries.retain(|p, _| present.contains(p));
        self.cache.blocklist.retain(|p, _| present.contains(p));
        report.removed = before - self.cache.entries.len();
        self.save()?;
        Ok(report)
    }

    /// Scan the given files (without pruning or saving).
    pub fn scan_files(&mut self, files: &[(PluginFormat, PathBuf)]) -> ScanReport {
        let mut report = ScanReport { files: files.len(), ..Default::default() };
        let mut todo: Vec<(PluginFormat, PathBuf, FileKey)> = Vec::new();
        for (format, path) in files {
            let Some(key) = FileKey::of(*format, path) else { continue };
            if self.cache.fresh_entry(path, key).is_some() {
                report.cached += 1;
            } else if self.cache.is_blocked(path, key) {
                report.skipped_blocked += 1;
            } else if let Some(infos) = (*format == PluginFormat::Vst3).then(|| moduleinfo::read(path)).flatten() {
                report.from_moduleinfo += 1;
                self.cache.entries.insert(path.clone(), CacheEntry { format: *format, key, plugins: infos, from_moduleinfo: true });
            } else {
                todo.push((*format, path.clone(), key));
            }
        }
        if todo.is_empty() {
            return report;
        }
        let Some(exe) = self.scanner_exe.clone() else {
            for (_, path, _) in &todo {
                report.failed.push((path.clone(), format!("{SCANNER_EXE} not found")));
            }
            return report;
        };

        let queue = Mutex::new(todo);
        let results = Mutex::new(Vec::new());
        let workers = self.parallelism.min(queue.lock().unwrap().len()).max(1);
        let timeout = self.timeout;
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| {
                    loop {
                        let Some(job) = queue.lock().unwrap().pop() else { break };
                        let (format, path, key) = job;
                        let r = scan_out_of_process(&exe, format, &path, timeout);
                        results.lock().unwrap().push((format, path, key, r));
                    }
                });
            }
        });
        for (format, path, key, r) in results.into_inner().unwrap() {
            match r {
                Ok(plugins) => {
                    report.scanned += 1;
                    self.cache.blocklist.remove(&path);
                    self.cache.entries.insert(path, CacheEntry { format, key, plugins, from_moduleinfo: false });
                }
                Err(failure) => {
                    let message = failure.message();
                    log::warn!("scan of {} failed: {message}", path.display());
                    if let Some(reason) = failure.block_reason() {
                        self.cache.entries.remove(&path);
                        self.cache.blocklist.insert(path.clone(), BlockEntry { format, key, reason, message: message.clone() });
                    }
                    report.failed.push((path, message));
                }
            }
        }
        report
    }
}

/// `aitrack-plugin-scanner` next to the current executable (or one level
/// up, for test binaries in `target/<profile>/deps`).
pub fn default_scanner_exe() -> Option<PathBuf> {
    sibling_exe(SCANNER_EXE)
}

pub(crate) fn sibling_exe(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join(name), dir.parent()?.join(name)].into_iter().find(|p| p.is_file())
}

/// Helper for callers that want an error instead of a report entry.
pub fn require_scanner_exe() -> Result<PathBuf> {
    default_scanner_exe().ok_or_else(|| HostError::Scan(format!("{SCANNER_EXE} not found next to the executable")))
}
