//! Cloud folders (iCloud Drive, Dropbox, OneDrive, Google Drive) as library
//! roots.
//!
//! - [`detect`] lists the providers' folders that exist on this machine
//!   (macOS File Provider folders in `~/Library/CloudStorage`, iCloud's
//!   `Mobile Documents`, the Windows clients' folders and drive letters), so
//!   the UI can add one as a root with one click.
//! - [`PlaceholderProbe`] tells an online-only file (a placeholder whose
//!   content is still in the cloud) from a local one **without reading it**:
//!   reading a placeholder makes the provider download it, and a scan must
//!   never download a whole cloud library. [`OsProbe`] reads the stat flags
//!   (macOS `SF_DATALESS`, Windows `RECALL_ON_*`/`OFFLINE` attributes);
//!   tests inject their own.
//! - Placeholders are indexed by name only (key, BPM and category from the
//!   file name) and flagged `cloud` in the index; [`HydrateJob`] downloads one
//!   on demand (audition, drag) on its own thread and then analyses it.

use std::collections::HashSet;
use std::fs::Metadata;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};

use crate::analyze::Analyzer;
use crate::db::{Library, SampleId};

// ───────────────────────────── Providers ─────────────────────────────

/// A cloud storage service that syncs a folder to this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    ICloudDrive,
    Dropbox,
    OneDrive,
    GoogleDrive,
}

impl Provider {
    pub const ALL: [Provider; 4] = [Provider::ICloudDrive, Provider::Dropbox, Provider::OneDrive, Provider::GoogleDrive];

    pub fn name(self) -> &'static str {
        match self {
            Self::ICloudDrive => "iCloud Drive",
            Self::Dropbox => "Dropbox",
            Self::OneDrive => "OneDrive",
            Self::GoogleDrive => "Google Drive",
        }
    }
}

/// A provider's folder found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudFolder {
    pub provider: Provider,
    /// "OneDrive (Contoso)", "Google Drive (me@example.com)", "iCloud Drive".
    pub label: String,
    /// The folder to add as a root (Google Drive: its "My Drive").
    pub path: PathBuf,
}

/// Where [`detect`] looks: the home folder, the environment and (Windows)
/// the drive roots Google Drive for desktop may mount as. Built from the
/// real machine by [`CloudEnv::current`]; tests point it at temp folders.
#[derive(Clone, Debug, Default)]
pub struct CloudEnv {
    pub home: Option<PathBuf>,
    /// Environment variables (`OneDrive`, `APPDATA`, `USERPROFILE`…).
    pub vars: Vec<(String, String)>,
    /// Drive roots (`G:\`…) to look for a "My Drive" folder in.
    pub drives: Vec<PathBuf>,
    /// Look for the Windows layout instead of the macOS one.
    pub windows: bool,
}

impl CloudEnv {
    pub fn current() -> Self {
        let windows = cfg!(windows);
        let home = std::env::var_os(if windows { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        let names = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial", "APPDATA", "LOCALAPPDATA", "USERPROFILE"];
        let vars = names.iter().filter_map(|n| std::env::var(n).ok().map(|v| (n.to_string(), v))).collect();
        // C: holds the system; Google Drive picks the first free letter from G:.
        let drives = if windows { ('D'..='Z').map(|c| PathBuf::from(format!("{c}:\\"))).collect() } else { Vec::new() };
        Self { home, vars, drives, windows }
    }

    fn var(&self, name: &str) -> Option<PathBuf> {
        self.vars.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| PathBuf::from(v)).filter(|p| !p.as_os_str().is_empty())
    }
}

/// "My Drive" in the languages Google Drive for desktop names it in.
const MY_DRIVE: &[&str] = &["My Drive", "Mi unidad", "Meine Ablage", "Mon Drive", "Il mio Drive", "Meu Drive", "Mijn Drive"];

fn my_drive(dir: &Path) -> Option<PathBuf> {
    MY_DRIVE.iter().map(|n| dir.join(n)).find(|p| p.is_dir())
}

/// "OneDrive-Contoso" → "OneDrive (Contoso)"; a bare name stays as it is.
fn suffixed(provider: Provider, rest: &str) -> String {
    let rest = rest.trim_matches(|c: char| c == '-' || c == ' ' || c == '(' || c == ')');
    if rest.is_empty() { provider.name().to_string() } else { format!("{} ({rest})", provider.name()) }
}

/// The provider of a `~/Library/CloudStorage` entry from its name.
fn cloud_storage_entry(name: &str) -> Option<(Provider, &str)> {
    for (prefix, p) in [("GoogleDrive", Provider::GoogleDrive), ("OneDrive", Provider::OneDrive), ("Dropbox", Provider::Dropbox), ("iCloud", Provider::ICloudDrive)] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return Some((p, rest));
        }
    }
    None
}

/// The cloud folders that exist under `env`, one per real folder (a legacy
/// `~/Dropbox` link to the File Provider folder is listed once).
pub fn detect(env: &CloudEnv) -> Vec<CloudFolder> {
    let mut found = Vec::new();
    let mut push = |provider: Provider, label: String, path: PathBuf| {
        if path.is_dir() {
            found.push(CloudFolder { provider, label, path });
        }
    };
    if env.windows {
        for (var, label) in [("OneDriveCommercial", "OneDrive (Business)"), ("OneDriveConsumer", "OneDrive (Personal)"), ("OneDrive", "OneDrive")] {
            if let Some(p) = env.var(var) {
                push(Provider::OneDrive, label.to_string(), p);
            }
        }
        for base in ["APPDATA", "LOCALAPPDATA"] {
            for (label, p) in env.var(base).map(|b| dropbox_info(&b.join("Dropbox").join("info.json"))).unwrap_or_default() {
                push(Provider::Dropbox, label, p);
            }
        }
        let profile = env.var("USERPROFILE").or_else(|| env.home.clone());
        if let Some(home) = &profile {
            push(Provider::Dropbox, "Dropbox".into(), home.join("Dropbox"));
            push(Provider::GoogleDrive, "Google Drive".into(), home.join("Google Drive"));
            push(Provider::ICloudDrive, "iCloud Drive".into(), home.join("iCloudDrive"));
        }
        for d in &env.drives {
            if let Some(p) = my_drive(d) {
                let letter = d.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
                push(Provider::GoogleDrive, format!("Google Drive ({letter})"), p);
            }
        }
    } else if let Some(home) = &env.home {
        push(Provider::ICloudDrive, "iCloud Drive".into(), home.join("Library/Mobile Documents/com~apple~CloudDocs"));
        let storage = home.join("Library/CloudStorage");
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&storage).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        entries.sort();
        for dir in entries {
            let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
            let Some((provider, rest)) = cloud_storage_entry(&name) else { continue };
            let path = if provider == Provider::GoogleDrive { my_drive(&dir).unwrap_or(dir) } else { dir };
            push(provider, suffixed(provider, rest), path);
        }
        // Older clients sync straight into the home folder.
        push(Provider::Dropbox, "Dropbox".into(), home.join("Dropbox"));
        push(Provider::OneDrive, "OneDrive".into(), home.join("OneDrive"));
        if let Some(p) = my_drive(&home.join("Google Drive")) {
            push(Provider::GoogleDrive, "Google Drive".into(), p);
        }
    }
    let mut seen = HashSet::new();
    found.retain(|f| seen.insert(f.path.canonicalize().unwrap_or_else(|_| f.path.clone())));
    found
}

/// The cloud folders on this machine.
pub fn detect_folders() -> Vec<CloudFolder> {
    detect(&CloudEnv::current())
}

/// Dropbox's `info.json`: `{"personal": {"path": …}, "business": {"path": …}}`.
fn dropbox_info(file: &Path) -> Vec<(String, PathBuf)> {
    let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Vec::new() };
    let mut out = Vec::new();
    for (key, label) in [("personal", "Dropbox (Personal)"), ("business", "Dropbox (Business)")] {
        if let Some(p) = v.get(key).and_then(|a| a.get("path")).and_then(|p| p.as_str()) {
            out.push((label.to_string(), PathBuf::from(p)));
        }
    }
    out
}

/// The provider a path syncs with, judged from its folders alone (no disk
/// access): `~/Library/CloudStorage/<Provider>-…`, iCloud's `CloudDocs`, or
/// a folder named like a provider's (`Dropbox`, `OneDrive - Contoso`, `My Drive`).
pub fn provider_of(path: &Path) -> Option<Provider> {
    let parts: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    for (i, part) in parts.iter().enumerate() {
        if part == "CloudStorage"
            && let Some((p, _)) = parts.get(i + 1).and_then(|n| cloud_storage_entry(n))
        {
            return Some(p);
        }
        if part == "com~apple~CloudDocs" || part == "iCloudDrive" || part == "iCloud Drive" {
            return Some(Provider::ICloudDrive);
        }
    }
    parts.iter().find_map(|part| {
        if part == "Dropbox" || part.starts_with("Dropbox (") {
            Some(Provider::Dropbox)
        } else if part == "OneDrive" || part.starts_with("OneDrive - ") || part.starts_with("OneDrive-") {
            Some(Provider::OneDrive)
        } else if part == "Google Drive" || MY_DRIVE.contains(&part.as_str()) {
            Some(Provider::GoogleDrive)
        } else {
            None
        }
    })
}

// ───────────────────────────── Placeholders ─────────────────────────────

/// Tells online-only files from local ones. Implementations must not read
/// the file's content (that downloads it).
pub trait PlaceholderProbe: Send + Sync {
    /// Whether the content of `path` (with metadata `md`) is not on disk.
    fn is_placeholder(&self, path: &Path, md: &Metadata) -> bool;
}

/// The operating system's own markers: macOS `SF_DATALESS` (File Provider
/// and iCloud evicted files) or no blocks allocated inside a cloud folder;
/// Windows `RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` or `OFFLINE`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsProbe;

/// `st_flags` bits (`<sys/stat.h>`).
pub const SF_DATALESS: u32 = 0x4000_0000;
pub const UF_COMPRESSED: u32 = 0x20;

/// `FILE_ATTRIBUTE_*` bits (`winnt.h`).
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
pub const FILE_ATTRIBUTE_PINNED: u32 = 0x8_0000;
pub const FILE_ATTRIBUTE_UNPINNED: u32 = 0x10_0000;
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// macOS: dataless, or (inside a cloud folder) a non-empty file with no
/// blocks on disk that is not transparently compressed.
pub fn mac_placeholder(flags: u32, blocks: u64, size: u64, in_cloud_folder: bool) -> bool {
    flags & SF_DATALESS != 0 || (in_cloud_folder && size > 0 && blocks == 0 && flags & UF_COMPRESSED == 0)
}

/// Windows: the content is fetched on open or on first read, or is offline.
/// `PINNED` ("always keep on this device") alone means local.
pub fn windows_placeholder(attrs: u32) -> bool {
    attrs & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_OFFLINE) != 0
}

impl PlaceholderProbe for OsProbe {
    #[allow(unused_variables)]
    fn is_placeholder(&self, path: &Path, md: &Metadata) -> bool {
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt;
            mac_placeholder(md.st_flags(), md.st_blocks(), md.len(), provider_of(path).is_some())
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            windows_placeholder(md.file_attributes())
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            false
        }
    }
}

/// The file an iCloud stub stands for: older macOS keeps an evicted
/// `Kick.wav` as a hidden `.Kick.wav.icloud` next to where it belongs.
pub fn icloud_stub_target(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let inner = name.strip_prefix('.')?.strip_suffix(".icloud")?;
    (!inner.is_empty()).then(|| path.with_file_name(inner))
}

/// The stub that stands for `path`, when there is one on disk.
pub fn icloud_stub_of(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    Some(path.with_file_name(format!(".{name}.icloud"))).filter(|s| s.is_file())
}

// ───────────────────────────── Downloads ─────────────────────────────

/// Bytes read per step while downloading.
const CHUNK: usize = 256 * 1024;
/// How long an iCloud stub may take to turn into its file.
const STUB_TIMEOUT: Duration = Duration::from_secs(300);

/// Brings the content of `path` onto this machine by reading it through
/// (the provider downloads what is read). `progress(done, total)` follows
/// the bytes; `cancel` stops between chunks. An iCloud stub is asked for
/// with `brctl download` and waited on.
pub fn hydrate(path: &Path, progress: &mut dyn FnMut(u64, u64), cancel: &AtomicBool) -> std::io::Result<()> {
    if !path.exists() && icloud_stub_of(path).is_some() {
        request_icloud_download(path);
        let start = Instant::now();
        while !path.exists() {
            if cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
            }
            if start.elapsed() > STUB_TIMEOUT {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "iCloud did not download the file"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let mut f = std::fs::File::open(path)?;
    let total = f.metadata()?.len();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        progress(done, total);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn request_icloud_download(path: &Path) {
    if let Err(e) = std::process::Command::new("/usr/bin/brctl").arg("download").arg(path).status() {
        log::warn!("iCloud download of {}: {e}", path.display());
    }
}

#[cfg(not(target_os = "macos"))]
fn request_icloud_download(_path: &Path) {}

/// Messages from a [`HydrateJob`].
#[derive(Clone, Debug, PartialEq)]
pub enum HydrateEvent {
    Progress { done: u64, total: u64 },
    /// On disk; with the re-analysed row when an index was given.
    Ready(Option<SampleId>),
    Failed(String),
}

/// One online-only file downloading on its own thread, then (optionally)
/// analysed into the library at `db` with `analyzer`.
pub struct HydrateJob {
    pub path: PathBuf,
    pub events: Receiver<HydrateEvent>,
    cancel: Arc<AtomicBool>,
}

impl HydrateJob {
    pub fn spawn(path: PathBuf, index: Option<(PathBuf, Arc<dyn Analyzer>)>) -> Self {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let (c, p) = (cancel.clone(), path.clone());
        let run = move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut on_progress = |done: u64, total: u64| {
                if done == total || last.elapsed() >= Duration::from_millis(50) {
                    last = Instant::now();
                    let _ = tx.send(HydrateEvent::Progress { done, total });
                }
            };
            if let Err(e) = hydrate(&p, &mut on_progress, &c) {
                let _ = tx.send(HydrateEvent::Failed(e.to_string()));
                return;
            }
            let id = index.and_then(|(db, analyzer)| match Library::open(&db).and_then(|lib| lib.index_file(analyzer.as_ref(), &p)) {
                Ok(id) => id,
                Err(e) => {
                    log::warn!("library: analysing downloaded {}: {e}", p.display());
                    None
                }
            });
            let _ = tx.send(HydrateEvent::Ready(id));
        };
        if let Err(e) = std::thread::Builder::new().name("aitrack-cloud-fetch".into()).spawn(run) {
            log::warn!("library: cannot start a download thread: {e}");
        }
        Self { path, events: rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdirs(root: &Path, dirs: &[&str]) {
        for d in dirs {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
    }

    #[test]
    fn detects_macos_cloud_storage_layout() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        mkdirs(&home, &[
            "Library/CloudStorage/Dropbox",
            "Library/CloudStorage/OneDrive-Personal",
            "Library/CloudStorage/OneDrive-Contoso",
            "Library/CloudStorage/GoogleDrive-me@example.com/My Drive",
            "Library/CloudStorage/GoogleDrive-me@example.com/Shared drives",
            "Library/CloudStorage/Box-Box",
            "Library/Mobile Documents/com~apple~CloudDocs",
        ]);
        // A legacy link to the File Provider folder is the same folder.
        #[cfg(unix)]
        std::os::unix::fs::symlink(home.join("Library/CloudStorage/Dropbox"), home.join("Dropbox")).unwrap();
        let found = detect(&CloudEnv { home: Some(home.clone()), ..Default::default() });
        let labels: Vec<&str> = found.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["iCloud Drive", "Dropbox", "Google Drive (me@example.com)", "OneDrive (Contoso)", "OneDrive (Personal)"]);
        let gd = found.iter().find(|f| f.provider == Provider::GoogleDrive).unwrap();
        assert!(gd.path.ends_with("My Drive"), "Google Drive roots at My Drive: {}", gd.path.display());
        for f in &found {
            assert_eq!(provider_of(&f.path), Some(f.provider), "{}", f.path.display());
        }
    }

    #[test]
    fn nothing_detected_on_a_bare_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(&CloudEnv { home: Some(dir.path().to_path_buf()), ..Default::default() }).is_empty());
        assert!(detect(&CloudEnv { home: Some(dir.path().to_path_buf()), windows: true, ..Default::default() }).is_empty());
    }

    #[test]
    fn detects_windows_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let profile = root.join("Users/me");
        mkdirs(&root, &["Users/me/OneDrive", "Users/me/OneDrive - Contoso", "Users/me/Dropbox", "Users/me/Work Dropbox", "Users/me/iCloudDrive", "AppData/Roaming/Dropbox", "G/My Drive", "H"]);
        let info = r#"{"personal": {"path": "PERSONAL", "host": 1}, "business": {"path": "BUSINESS"}}"#
            .replace("PERSONAL", &profile.join("Dropbox").to_string_lossy().replace('\\', "\\\\"))
            .replace("BUSINESS", &profile.join("Work Dropbox").to_string_lossy().replace('\\', "\\\\"));
        std::fs::write(root.join("AppData/Roaming/Dropbox/info.json"), info).unwrap();
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        let env = CloudEnv {
            home: Some(profile.clone()),
            vars: vec![
                ("OneDrive".into(), s(profile.join("OneDrive"))),
                ("OneDriveConsumer".into(), s(profile.join("OneDrive"))),
                ("OneDriveCommercial".into(), s(profile.join("OneDrive - Contoso"))),
                ("APPDATA".into(), s(root.join("AppData/Roaming"))),
                ("USERPROFILE".into(), s(profile.clone())),
            ],
            drives: vec![root.join("G"), root.join("H"), root.join("Z")],
            windows: true,
        };
        let found = detect(&env);
        let got: Vec<(Provider, &str)> = found.iter().map(|f| (f.provider, f.label.as_str())).collect();
        assert_eq!(got, vec![
            (Provider::OneDrive, "OneDrive (Business)"),
            (Provider::OneDrive, "OneDrive (Personal)"),
            (Provider::Dropbox, "Dropbox (Personal)"),
            (Provider::Dropbox, "Dropbox (Business)"),
            (Provider::ICloudDrive, "iCloud Drive"),
            (Provider::GoogleDrive, &*format!("Google Drive ({})", s(root.join("G")))),
        ]);
    }

    #[test]
    fn provider_from_path() {
        assert_eq!(provider_of(Path::new("/Users/a/Library/CloudStorage/OneDrive-Contoso/Samples")), Some(Provider::OneDrive));
        assert_eq!(provider_of(Path::new("/Users/a/Library/CloudStorage/GoogleDrive-x@y.z/My Drive")), Some(Provider::GoogleDrive));
        assert_eq!(provider_of(Path::new("/Users/a/Library/Mobile Documents/com~apple~CloudDocs/Kits")), Some(Provider::ICloudDrive));
        assert_eq!(provider_of(Path::new("C:/Users/a/OneDrive - Contoso/Loops")), Some(Provider::OneDrive));
        assert_eq!(provider_of(Path::new("G:/My Drive/Loops")), Some(Provider::GoogleDrive));
        assert_eq!(provider_of(Path::new("/Users/a/Dropbox (Personal)/x.wav")), Some(Provider::Dropbox));
        assert_eq!(provider_of(Path::new("/Users/a/Music/Samples")), None);
    }

    #[test]
    fn placeholder_markers() {
        assert!(mac_placeholder(SF_DATALESS, 8, 1000, false));
        assert!(mac_placeholder(0, 0, 1000, true), "no blocks in a cloud folder");
        assert!(!mac_placeholder(0, 0, 1000, false), "no blocks elsewhere is not enough");
        assert!(!mac_placeholder(UF_COMPRESSED, 0, 1000, true), "compressed files keep data in an attribute");
        assert!(!mac_placeholder(0, 0, 0, true), "empty files are local");
        assert!(!mac_placeholder(0, 8, 1000, true));
        assert!(windows_placeholder(FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_UNPINNED));
        assert!(windows_placeholder(FILE_ATTRIBUTE_RECALL_ON_OPEN));
        assert!(windows_placeholder(FILE_ATTRIBUTE_OFFLINE));
        assert!(!windows_placeholder(FILE_ATTRIBUTE_PINNED | 0x20));
        assert!(!windows_placeholder(0x20));
    }

    #[test]
    fn local_files_are_not_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.wav");
        std::fs::write(&f, vec![1u8; 4096]).unwrap();
        assert!(!OsProbe.is_placeholder(&f, &std::fs::metadata(&f).unwrap()));
    }

    #[test]
    fn icloud_stub_names() {
        assert_eq!(icloud_stub_target(Path::new("/k/.Kick 01.wav.icloud")), Some(PathBuf::from("/k/Kick 01.wav")));
        assert_eq!(icloud_stub_target(Path::new("/k/Kick.wav.icloud")), None);
        assert_eq!(icloud_stub_target(Path::new("/k/..icloud")), None);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".Snare.wav.icloud"), b"plist").unwrap();
        assert!(icloud_stub_of(&dir.path().join("Snare.wav")).is_some());
        assert!(icloud_stub_of(&dir.path().join("Clap.wav")).is_none());
    }

    /// Says "online-only" for the paths it holds; never touches the disk.
    struct FakeProbe(HashSet<PathBuf>);

    impl PlaceholderProbe for FakeProbe {
        fn is_placeholder(&self, path: &Path, _md: &Metadata) -> bool {
            self.0.contains(path)
        }
    }

    fn probe(paths: &[&PathBuf]) -> FakeProbe {
        FakeProbe(paths.iter().map(|p| p.to_path_buf()).collect())
    }

    #[test]
    fn scan_indexes_placeholders_by_name_without_reading_them() {
        use crate::analyze::BasicAnalyzer;
        use crate::db::RootKind;
        use crate::scan::ScanOptions;
        use crate::test_util::write_wav;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let drive = home.join("Library/CloudStorage/GoogleDrive-me@example.com/My Drive");
        let pack = drive.join("Trap Pack");
        mkdirs(&pack, &["Kicks", "Loops"]);
        let kick = pack.join("Kicks/Kick Hard.wav");
        write_wav(&kick, 22_050, 1, 0.3, 0.9);
        // Online-only: not audio at all, so reading or decoding it would show.
        let melody = pack.join("Loops/Melody Am 140bpm.wav");
        std::fs::write(&melody, vec![0u8; 5000]).unwrap();
        // An evicted iCloud-style file: only its hidden stub is on disk.
        std::fs::write(pack.join("Loops/.Pad Loop F#m 90bpm.wav.icloud"), b"bplist00").unwrap();
        let pad = pack.join("Loops/Pad Loop F#m 90bpm.wav");

        let found = detect(&CloudEnv { home: Some(home.clone()), ..Default::default() });
        assert_eq!(found.len(), 1);
        let lib = Library::open(home.join("lib.db")).unwrap();
        let root = lib.add_root(&found[0].path, RootKind::User).unwrap();
        assert_eq!(lib.root(root).unwrap().provider(), Some(Provider::GoogleDrive));

        let cancel = AtomicBool::new(false);
        let opts = ScanOptions::default();
        let s = lib.scan_with(&BasicAnalyzer, &probe(&[&melody]), &opts, &mut |_| {}, &cancel).unwrap();
        assert_eq!((s.added, s.errors, s.cloud), (3, 0, 2), "{s:?}");
        assert_eq!(lib.count_cloud().unwrap(), 2);
        let stats = lib.root_stats().unwrap();
        assert_eq!((stats.len(), stats[0].samples, stats[0].cloud), (1, 3, 2));

        let m = lib.find_by_path(&melody).unwrap().unwrap();
        assert!(m.cloud);
        assert!(m.hash.is_none(), "an online-only file is not hashed (that reads it)");
        assert_eq!((m.bpm, m.camelot.as_deref()), (Some(140.0), Some("8A")));
        assert!(m.is_loop(), "a tempo in the name of an online-only file makes it a loop");
        let p = lib.find_by_path(&pad).unwrap().unwrap();
        assert!(p.cloud);
        assert_eq!(p.path, pad, "a stub is indexed as the file it stands for");
        assert_eq!(p.bpm, Some(90.0));
        let k = lib.find_by_path(&kick).unwrap().unwrap();
        assert!(!k.cloud && k.hash.is_some() && k.duration.unwrap() > 0.2);
        // Searches list online-only rows like any other.
        let q = crate::query::SearchQuery { text: Some("melody".into()), limit: 10, ..Default::default() };
        assert_eq!(lib.search(&q).unwrap().len(), 1);

        // Nothing changed: nothing is re-read or re-indexed.
        let s = lib.scan_with(&BasicAnalyzer, &probe(&[&melody]), &opts, &mut |_| {}, &cancel).unwrap();
        assert_eq!((s.added, s.updated, s.unchanged, s.cloud), (0, 0, 3, 2), "{s:?}");

        // The melody got downloaded: it is analysed like a local file.
        write_wav(&melody, 22_050, 1, 1.0, 0.5);
        let s = lib.scan_with(&BasicAnalyzer, &probe(&[]), &opts, &mut |_| {}, &cancel).unwrap();
        assert_eq!((s.updated, s.cloud), (1, 1), "{s:?}");
        let m = lib.find_by_path(&melody).unwrap().unwrap();
        assert!(!m.cloud && m.hash.is_some() && m.duration.unwrap() > 0.9);

        // The kick got evicted (same size and date): flagged, analysis kept.
        let s = lib.scan_with(&BasicAnalyzer, &probe(&[&kick]), &opts, &mut |_| {}, &cancel).unwrap();
        assert_eq!(s.cloud, 2, "{s:?}");
        let k2 = lib.find_by_path(&kick).unwrap().unwrap();
        assert!(k2.cloud);
        assert_eq!((k2.duration, k2.hash), (k.duration, k.hash.clone()));
        // And comes back local without being analysed again.
        lib.scan_with(&BasicAnalyzer, &probe(&[]), &opts, &mut |_| {}, &cancel).unwrap();
        assert!(!lib.find_by_path(&kick).unwrap().unwrap().cloud);
    }

    #[test]
    fn index_file_reads_only_local_files() {
        use crate::analyze::BasicAnalyzer;
        use crate::db::RootKind;
        use crate::test_util::write_wav;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("Dropbox");
        std::fs::create_dir_all(&root).unwrap();
        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        lib.add_root(&root, RootKind::User).unwrap();
        let snare = root.join("Snare 128bpm Loop.wav");
        std::fs::write(&snare, vec![0u8; 3000]).unwrap();
        let id = lib.index_file_with(&BasicAnalyzer, &probe(&[&snare]), &snare).unwrap().unwrap();
        let s = lib.get(id).unwrap();
        assert!(s.cloud && s.is_loop());
        assert_eq!(s.bpm, Some(128.0));
        // An iCloud stub event indexes the file it stands for.
        let stub = root.join(".Clap 2.wav.icloud");
        std::fs::write(&stub, b"bplist00").unwrap();
        let id = lib.index_file_with(&BasicAnalyzer, &probe(&[]), &stub).unwrap().unwrap();
        assert_eq!(lib.get(id).unwrap().path, root.join("Clap 2.wav"));
        // Downloaded: the real file is analysed.
        write_wav(&snare, 22_050, 1, 0.5, 0.5);
        let id = lib.index_file_with(&BasicAnalyzer, &probe(&[]), &snare).unwrap().unwrap();
        let s = lib.get(id).unwrap();
        assert!(!s.cloud && s.duration.unwrap() > 0.4);
    }

    #[test]
    fn hydrate_job_downloads_then_analyses() {
        use crate::analyze::BasicAnalyzer;
        use crate::db::RootKind;
        use crate::test_util::write_wav;

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("OneDrive");
        std::fs::create_dir_all(&root).unwrap();
        let db = base.join("lib.db");
        let f = root.join("Bass F 808.wav");
        write_wav(&f, 22_050, 1, 0.5, 0.5);
        {
            let lib = Library::open(&db).unwrap();
            lib.add_root(&root, RootKind::User).unwrap();
            lib.index_file_with(&BasicAnalyzer, &probe(&[&f]), &f).unwrap();
            assert!(lib.find_by_path(&f).unwrap().unwrap().cloud);
        }
        let job = HydrateJob::spawn(f.clone(), Some((db.clone(), Arc::new(BasicAnalyzer))));
        let mut ready = None;
        for ev in job.events.iter() {
            match ev {
                HydrateEvent::Ready(id) => {
                    ready = Some(id);
                    break;
                }
                HydrateEvent::Failed(e) => panic!("{e}"),
                HydrateEvent::Progress { .. } => {}
            }
        }
        assert!(ready.flatten().is_some());
        let s = Library::open(&db).unwrap().find_by_path(&f).unwrap().unwrap();
        assert!(!s.cloud && s.duration.unwrap() > 0.4);
    }

    #[test]
    fn hydrate_reads_through_with_progress_and_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("big.wav");
        std::fs::write(&f, vec![7u8; CHUNK * 2 + 10]).unwrap();
        let mut seen = Vec::new();
        hydrate(&f, &mut |d, t| seen.push((d, t)), &AtomicBool::new(false)).unwrap();
        assert_eq!(seen.first(), Some(&(0, (CHUNK * 2 + 10) as u64)));
        assert_eq!(seen.last(), Some(&((CHUNK * 2 + 10) as u64, (CHUNK * 2 + 10) as u64)));
        let err = hydrate(&f, &mut |_, _| {}, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert!(hydrate(&dir.path().join("none.wav"), &mut |_, _| {}, &AtomicBool::new(false)).is_err());
    }
}
