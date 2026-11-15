//! The plugin installer behind the Plugins tab: download from the maker's own
//! domain, check the sha256, open the container (never run it), copy the
//! `.vst3` / `.clap` bundles into the user's plugin folders, check what macOS
//! wants checked, and leave a receipt that lets us update and uninstall.
//!
//! What it never does: run an installer or a script from the download, ask for
//! administrator rights, write outside the user's plugin folders, follow a
//! redirect to another domain, or install a file whose hash is not the pinned one.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::{Artifact, Catalog, Container, Lane, Os, PluginEntry};
use crate::error::{LibraryError, Result};
use crate::install::{Limits, unpack_zip_limited};

fn err(msg: impl Into<String>) -> LibraryError {
    LibraryError::Manifest(msg.into())
}

fn io(e: std::io::Error) -> LibraryError {
    err(e.to_string())
}

// ───────────────────────────── Where plugins go ─────────────────────────────

/// The per-user plugin folders: the standard ones every host looks in, which
/// need no administrator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserDirs {
    pub vst3: PathBuf,
    pub clap: PathBuf,
    /// Folders the user's other installers use; only read, to say "installed outside the app".
    pub system: Vec<PathBuf>,
}

impl UserDirs {
    pub fn standard() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            let base = home.join("Library/Audio/Plug-Ins");
            return Some(Self { vst3: base.join("VST3"), clap: base.join("CLAP"), system: vec![PathBuf::from("/Library/Audio/Plug-Ins/VST3"), PathBuf::from("/Library/Audio/Plug-Ins/CLAP")] });
        }
        #[cfg(windows)]
        {
            let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs").join("Common");
            let system = std::env::var_os("CommonProgramFiles").map(PathBuf::from).map(|c| vec![c.join("VST3"), c.join("CLAP")]).unwrap_or_default();
            return Some(Self { vst3: local.join("VST3"), clap: local.join("CLAP"), system });
        }
        #[allow(unreachable_code)]
        None
    }

    fn dir_of(&self, bundle: &Path) -> Option<&Path> {
        match bundle.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "vst3" => Some(&self.vst3),
            "clap" => Some(&self.clap),
            _ => None,
        }
    }

    /// Whether `path` is a plugin bundle directly inside one of the user folders.
    fn owns(&self, path: &Path) -> bool {
        path.parent().is_some_and(|p| p == self.vst3 || p == self.clap) && self.dir_of(path).is_some()
    }
}

// ───────────────────────────── Download ─────────────────────────────

/// What a URL (and each redirect) must satisfy to be fetched.
#[derive(Clone, Copy, Debug, Default)]
pub struct Policy {
    /// Tests only: `http://127.0.0.1` stands in for the maker's server.
    loopback_http: bool,
}

impl Policy {
    #[cfg(test)]
    fn for_tests() -> Self {
        Self { loopback_http: true }
    }

    /// `Ok(host)` when `url` is `https` (or loopback `http` in tests) and its host is one of `hosts`.
    pub fn check(&self, url: &str, hosts: &[String]) -> Result<String> {
        let host = crate::catalog::url_host(url).or_else(|| {
            let rest = url.strip_prefix("http://").filter(|_| self.loopback_http)?;
            let auth = rest.split(['/', '?', '#']).next()?;
            let h = auth.rsplit_once(':').map_or(auth, |(h, _)| h);
            (h == "127.0.0.1" || h == "localhost").then(|| h.to_owned())
        });
        let Some(host) = host else { return Err(err(format!("{url} is not an https address"))) };
        if hosts.iter().any(|h| h == &host) { Ok(host) } else { Err(err(format!("{host} is not one of the maker's domains"))) }
    }
}

/// Where bytes come from. The real one talks HTTP; tests plug in a local server.
pub trait Fetcher: Send + Sync {
    fn fetch(&self, url: &str, hosts: &[String], max: u64, dest: &Path, stop: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<()>;
}

/// HTTPS only, each redirect checked against the maker's domains, at most
/// `max` bytes, cancellable.
#[derive(Default)]
pub struct HttpFetcher {
    policy: Policy,
}

impl HttpFetcher {
    #[cfg(test)]
    fn for_tests() -> Self {
        Self { policy: Policy::for_tests() }
    }
}

const MAX_HOPS: usize = 5;

/// `location` resolved against `base` (absolute, or a path on the same origin).
fn resolve(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_owned();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let origin = rest.split('/').next().unwrap_or("");
    if location.starts_with('/') {
        format!("{scheme}://{origin}{location}")
    } else {
        let dir = base.rsplit_once('/').map_or(base, |(d, _)| d);
        format!("{dir}/{location}")
    }
}

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &str, hosts: &[String], max: u64, dest: &Path, stop: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(60)))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .user_agent("AI-Track")
            .build()
            .into();
        let mut current = url.to_owned();
        for _ in 0..=MAX_HOPS {
            self.policy.check(&current, hosts)?;
            let resp = agent.get(&current).call().map_err(|e| err(format!("download failed: {e}")))?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) else { return Err(err("the server redirected without saying where")) };
                current = resolve(&current, loc);
                continue;
            }
            if status != 200 {
                return Err(err(format!("the maker's server answered {status}")));
            }
            let total: u64 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
            if total > max {
                return Err(err("the file is larger than the catalog says; not downloading it"));
            }
            let mut reader = resp.into_body().into_reader();
            let mut out = BufWriter::new(File::create(dest).map_err(io)?);
            let mut buf = vec![0u8; 256 * 1024];
            let (mut done, mut last) = (0u64, 0u64);
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Err(err("cancelled"));
                }
                let n = reader.read(&mut buf).map_err(|e| err(format!("download failed: {e}")))?;
                if n == 0 {
                    break;
                }
                done += n as u64;
                if done > max {
                    return Err(err("the download is larger than the catalog says; stopped"));
                }
                out.write_all(&buf[..n]).map_err(io)?;
                if done - last >= 1 << 20 {
                    last = done;
                    progress(done, total.max(max.min(done)));
                }
            }
            out.flush().map_err(io)?;
            progress(done, done);
            return Ok(());
        }
        Err(err("too many redirects"))
    }
}

// ───────────────────────────── Receipts ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptFile {
    /// The installed bundle (inside a user plugin folder).
    pub path: PathBuf,
    /// sha256 over the bundle's tree (paths, contents, link targets).
    pub tree_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub version: String,
    pub source_url: String,
    /// sha256 of the file that was downloaded.
    pub sha256: String,
    pub installed_at: u64,
    pub files: Vec<ReceiptFile>,
    /// What the signature check found, per bundle, in words (`team ABC123`, `ad-hoc signed by AI-Track`, `unsigned`, `not checked`).
    pub signature: Vec<String>,
}

fn receipt_path(data: &Path, id: &str) -> PathBuf {
    data.join("plugins").join("receipts").join(format!("{id}.json"))
}

pub fn read_receipt(data: &Path, id: &str) -> Option<Receipt> {
    serde_json::from_slice(&fs::read(receipt_path(data, id)).ok()?).ok()
}

fn write_receipt(data: &Path, r: &Receipt) -> Result<()> {
    let path = receipt_path(data, &r.id);
    fs::create_dir_all(path.parent().unwrap_or(data)).map_err(io)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(r).map_err(|e| err(e.to_string()))?).map_err(io)?;
    fs::rename(&tmp, &path).map_err(io)
}

/// A hash of a whole bundle: relative paths, file contents and link targets, in order.
pub fn tree_hash(root: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut entries: Vec<_> = walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()).collect();
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    for e in entries {
        let rel = e.path().strip_prefix(root).map_err(|x| err(x.to_string()))?.to_string_lossy().replace('\\', "/");
        h.update(rel.as_bytes());
        let t = e.file_type();
        if t.is_symlink() {
            h.update(b"\0link\0");
            h.update(fs::read_link(e.path()).map_err(io)?.to_string_lossy().as_bytes());
        } else if t.is_file() {
            h.update(b"\0file\0");
            let mut f = BufReader::new(File::open(e.path()).map_err(io)?);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf).map_err(io)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
        } else {
            h.update(b"\0dir\0");
        }
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Hex sha256 of a file.
pub fn file_sha256(path: &Path) -> Result<String> {
    let mut f = BufReader::new(File::open(path).map_err(io)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ───────────────────────────── Containers ─────────────────────────────

/// Run a system tool, killing it after `secs` or when `stop` is set. Returns stderr.
fn run_tool(mut cmd: Command, stop: &AtomicBool, secs: u64) -> Result<String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| err(format!("{:?}: {e}", cmd.get_program())))?;
    let t0 = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(io)? {
            let mut text = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut text);
            }
            return if status.success() { Ok(text) } else { Err(err(format!("{:?} failed: {}", cmd.get_program(), text.trim()))) };
        }
        if stop.load(Ordering::Relaxed) || t0.elapsed() > Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err(if stop.load(Ordering::Relaxed) { "cancelled" } else { "a system tool took too long" }));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn is_bundle(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".vst3") || n.ends_with(".clap")
}

/// The plugin bundles under `root` (not looking inside a bundle for more).
pub fn find_bundles(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, want: &dyn Fn(&str) -> bool) {
        if depth > 8 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "__MACOSX" || name.starts_with("._") {
                continue;
            }
            let path = e.path();
            // A link is never a bundle, and is never followed: it could point anywhere.
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if want(&name) {
                out.push(path);
            } else if kind.is_dir() {
                walk(&path, depth + 1, out, want);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out, &is_bundle);
    out.sort();
    out
}

fn find_nested(root: &Path, ext: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, ext: &str, out: &mut Vec<PathBuf>) {
        if depth > 6 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if path.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)) {
                out.push(path);
            } else if kind.is_dir() {
                walk(&path, depth + 1, ext, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, ext, &mut out);
    out.sort();
    out
}

/// Every symlink inside `bundle` must really resolve to somewhere inside it
/// (a bundle is moved into the plugin folder, so what leaves it would then point
/// at the user's own files), and the bundle itself must not be a link.
pub fn verify_bundle_links(bundle: &Path) -> Result<()> {
    if fs::symlink_metadata(bundle).map_err(io)?.file_type().is_symlink() {
        return Err(err(format!("{} is a link, not a plugin bundle", bundle.display())));
    }
    crate::install::verify_links_inside(bundle)
}

/// Copy a tree keeping permissions and links, refusing links that leave it.
pub fn copy_tree(src: &Path, dst: &Path, limits: Limits) -> Result<()> {
    fn go(src: &Path, dst: &Path, rel: &Path, count: &mut usize, bytes: &mut u64, limits: Limits) -> Result<()> {
        let meta = fs::symlink_metadata(src).map_err(io)?;
        *count += 1;
        if *count > limits.files {
            return Err(err("too many files"));
        }
        if meta.file_type().is_symlink() {
            let target = fs::read_link(src).map_err(io)?;
            if !crate::install::link_stays_inside(rel, &target.to_string_lossy()) {
                return Err(err(format!("a link in the download leaves its folder ({})", rel.display())));
            }
            #[cfg(unix)]
            {
                let _ = fs::remove_file(dst);
                std::os::unix::fs::symlink(&target, dst).map_err(io)?;
            }
            return Ok(());
        }
        if meta.is_dir() {
            fs::create_dir_all(dst).map_err(io)?;
            let mut names: Vec<_> = fs::read_dir(src).map_err(io)?.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
            names.sort();
            for n in names {
                go(&src.join(&n), &dst.join(&n), &rel.join(&n), count, bytes, limits)?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(dst, fs::Permissions::from_mode((meta.permissions().mode() & 0o777) | 0o700));
            }
            return Ok(());
        }
        *bytes += meta.len();
        if *bytes > limits.bytes {
            return Err(err("too many bytes"));
        }
        fs::copy(src, dst).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dst, fs::Permissions::from_mode((meta.permissions().mode() & 0o777) | 0o600));
        }
        Ok(())
    }
    let (mut count, mut bytes) = (0, 0);
    go(src, dst, Path::new(""), &mut count, &mut bytes, limits)
}

/// A disk image mounted read-only. Detached on drop.
struct Mount {
    point: PathBuf,
}

impl Mount {
    fn attach(dmg: &Path, work: &Path, stop: &AtomicBool) -> Result<Self> {
        let dir = work.join("mnt");
        fs::create_dir_all(&dir).map_err(io)?;
        let mut cmd = Command::new("hdiutil");
        cmd.args(["attach", "-readonly", "-nobrowse", "-noautoopen", "-noverify", "-mountrandom"]).arg(&dir).arg(dmg);
        run_tool(cmd, stop, 120)?;
        // `-mountrandom` makes a fresh folder under `dir`: that is the volume.
        let point = fs::read_dir(&dir).map_err(io)?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.is_dir()).ok_or_else(|| err("the disk image did not mount"))?;
        Ok(Self { point })
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil").args(["detach", "-force"]).arg(&self.point).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

fn expand_pkg(pkg: &Path, out: &Path, stop: &AtomicBool) -> Result<()> {
    // Expands the payload into folders. `installer` is never called and no script is run.
    let mut cmd = Command::new("pkgutil");
    cmd.arg("--expand-full").arg(pkg).arg(out);
    run_tool(cmd, stop, 300).map(|_| ())
}

/// Open the downloaded file into `raw` and find the bundles in it, looking one
/// level into disk images and installer packages the maker put inside a zip.
fn open_container(file: &Path, container: Container, work: &Path, raw: &Path, stop: &AtomicBool) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(raw).map_err(io)?;
    let limits = Limits::PLUGIN;
    match container {
        Container::Zip => unpack_zip_limited(file, raw, stop, limits)?,
        Container::TarGz => {
            let archive_bytes = fs::metadata(file).map_err(io)?.len().max(1);
            crate::install::unpack_tar_limited(flate2::read::GzDecoder::new(BufReader::new(File::open(file).map_err(io)?)), raw, stop, limits, archive_bytes)?;
        }
        Container::Dmg => {
            if !cfg!(target_os = "macos") {
                return Err(err("disk images are opened on macOS only"));
            }
            let mount = Mount::attach(file, work, stop)?;
            // Only what we want leaves the image: bundles, and installer packages to open.
            let mut found = find_bundles(&mount.point);
            for b in &found {
                copy_tree(b, &raw.join(b.file_name().unwrap_or_default()), limits)?;
            }
            if found.is_empty() {
                for (i, pkg) in find_nested(&mount.point, "pkg").into_iter().enumerate() {
                    expand_pkg(&pkg, &raw.join(format!("pkg{i}")), stop)?;
                }
            }
            found.clear();
        }
        Container::Pkg => {
            if !cfg!(target_os = "macos") {
                return Err(err("installer packages are expanded on macOS only"));
            }
            expand_pkg(file, &raw.join("pkg"), stop)?;
        }
    }
    let mut bundles = find_bundles(raw);
    if bundles.is_empty() && cfg!(target_os = "macos") {
        // A zip that holds the maker's installer package or disk image.
        for (i, pkg) in find_nested(raw, "pkg").into_iter().enumerate() {
            expand_pkg(&pkg, &work.join(format!("inner-pkg{i}")), stop)?;
        }
        for (i, dmg) in find_nested(raw, "dmg").into_iter().enumerate() {
            let sub = work.join(format!("inner-dmg{i}"));
            let mount = Mount::attach(&dmg, &sub, stop)?;
            for b in find_bundles(&mount.point) {
                copy_tree(&b, &work.join(format!("inner-dmg{i}-out")).join(b.file_name().unwrap_or_default()), limits)?;
            }
        }
        bundles = find_bundles(work).into_iter().filter(|b| !b.starts_with(raw)).collect();
    }
    Ok(bundles)
}

// ───────────────────────────── macOS checks ─────────────────────────────

/// After the hash matched: drop the download's quarantine flag, check the code
/// signature (ad-hoc signing what is unsigned, which Apple Silicon needs), and
/// compare the Team ID with the catalog's when it names one.
fn check_signature(bundle: &Path, team_id: &str, stop: &AtomicBool) -> Result<String> {
    if !cfg!(target_os = "macos") {
        return Ok("not checked (signatures are checked on macOS)".into());
    }
    let mut x = Command::new("xattr");
    x.args(["-dr", "com.apple.quarantine"]).arg(bundle);
    let _ = run_tool(x, stop, 60);
    let mut verify = Command::new("codesign");
    verify.args(["--verify", "--deep", "--strict"]).arg(bundle);
    let signed = match run_tool(verify, stop, 120) {
        Ok(_) => true,
        Err(e) if e.to_string().contains("cancelled") => return Err(e),
        Err(_) => false,
    };
    if !signed {
        let mut sign = Command::new("codesign");
        sign.args(["--force", "--deep", "-s", "-"]).arg(bundle);
        return Ok(match run_tool(sign, stop, 120) {
            Ok(_) => "unsigned: signed ad-hoc by AI-Track so it can load".into(),
            Err(_) => "unsigned, and it could not be signed".into(),
        });
    }
    let mut info = Command::new("codesign");
    info.args(["-dv", "--verbose=4"]).arg(bundle);
    let text = run_tool(info, stop, 60).unwrap_or_default();
    let team = text.lines().find_map(|l| l.strip_prefix("TeamIdentifier=")).map(str::trim).unwrap_or("").to_owned();
    if !team_id.is_empty() && team != team_id {
        return Err(err(format!("signed by team {team:?}, not the maker's {team_id:?}; not installed")));
    }
    Ok(if team.is_empty() || team == "not set" { "signed ad-hoc".into() } else { format!("signed by team {team}") })
}

// ───────────────────────────── The install itself ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStage {
    Downloading,
    Verifying,
    Unpacking,
    Checking,
    Installing,
}

impl PluginStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Downloading => "Downloading",
            Self::Verifying => "Verifying",
            Self::Unpacking => "Unpacking",
            Self::Checking => "Checking",
            Self::Installing => "Installing",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PluginEvent {
    Progress { stage: PluginStage, done: u64, total: u64 },
    Done(Receipt),
    Failed(String),
    Cancelled,
}

/// Everything one install needs, so tests can point it at temp folders.
#[derive(Clone)]
pub struct InstallPlan {
    pub entry: PluginEntry,
    pub artifact: Artifact,
    pub os: Os,
    pub dirs: UserDirs,
    pub data: PathBuf,
}

fn say(tx: &Sender<PluginEvent>, stage: PluginStage, done: u64, total: u64) {
    let _ = tx.send(PluginEvent::Progress { stage, done, total });
}

/// Download, verify, open, check, install. Returns the receipt.
pub fn install_plugin(plan: &InstallPlan, fetcher: &dyn Fetcher, stop: &AtomicBool, tx: &Sender<PluginEvent>) -> Result<Receipt> {
    let (entry, art) = (&plan.entry, &plan.artifact);
    if !entry.installable(plan.os) || entry.artifact_for(plan.os) != Some(art) {
        return Err(err(format!("{} cannot be installed from the app: open the maker's page", entry.name)));
    }
    let work = plan.data.join("plugins").join("work").join(&entry.id);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(io)?;
    let result = install_in(plan, fetcher, stop, tx, &work);
    let _ = fs::remove_dir_all(&work);
    result
}

fn install_in(plan: &InstallPlan, fetcher: &dyn Fetcher, stop: &AtomicBool, tx: &Sender<PluginEvent>, work: &Path) -> Result<Receipt> {
    let (entry, art) = (&plan.entry, &plan.artifact);
    let file = work.join("download");
    fetcher.fetch(&art.url, &entry.allowed_hosts, art.size, &file, stop, &mut |done, total| say(tx, PluginStage::Downloading, done, total))?;
    say(tx, PluginStage::Verifying, 0, 1);
    let got = file_sha256(&file)?;
    if !got.eq_ignore_ascii_case(&art.sha256) {
        return Err(err(format!("the maker's file is not the one we checked (sha256 differs): {} may have published a new version. Open their page instead.", entry.vendor)));
    }
    say(tx, PluginStage::Unpacking, 0, 1);
    let raw = work.join("raw");
    let bundles: Vec<PathBuf> = open_container(&file, art.container, work, &raw, stop)?
        .into_iter()
        .filter(|b| b.extension().and_then(|e| e.to_str()).is_some_and(|e| entry.formats.iter().any(|f| f.eq_ignore_ascii_case(e))))
        .collect();
    if bundles.is_empty() {
        return Err(err(format!("{} has no plugin inside that can be installed without running its installer: open the maker's page", entry.name)));
    }
    // Nothing is checked, signed or moved before its links are known to stay inside it.
    for b in &bundles {
        verify_bundle_links(b)?;
    }
    // Check each staged bundle before anything reaches the plugin folders.
    say(tx, PluginStage::Checking, 0, bundles.len() as u64);
    let mut signature = Vec::new();
    for (i, b) in bundles.iter().enumerate() {
        signature.push(check_signature(b, &art.team_id, stop)?);
        say(tx, PluginStage::Checking, i as u64 + 1, bundles.len() as u64);
    }
    say(tx, PluginStage::Installing, 0, 1);
    let previous = read_receipt(&plan.data, &entry.id);
    let mut placed: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = (|| -> Result<Vec<ReceiptFile>> {
        let mut files = Vec::new();
        for b in &bundles {
            let name = b.file_name().ok_or_else(|| err("bundle without a name"))?;
            let dir = plan.dirs.dir_of(b).ok_or_else(|| err("unknown plugin format"))?;
            fs::create_dir_all(dir).map_err(io)?;
            let dest = dir.join(name);
            let ours = previous.as_ref().is_some_and(|r| r.files.iter().any(|f| f.path == dest));
            let backup = if fs::symlink_metadata(&dest).is_ok() {
                if !ours {
                    return Err(err(format!("{} is already in your plugin folder and was not installed by AI-Track; leaving it alone", name.to_string_lossy())));
                }
                let old = dest.with_extension(format!("{}.aitrack-old", dest.extension().and_then(|e| e.to_str()).unwrap_or("")));
                let _ = fs::remove_dir_all(&old);
                fs::rename(&dest, &old).map_err(io)?;
                Some(old)
            } else {
                None
            };
            placed.push((dest.clone(), backup));
            if fs::rename(b, &dest).is_err() {
                copy_tree(b, &dest, Limits::PLUGIN)?;
            }
            files.push(ReceiptFile { tree_sha256: tree_hash(&dest)?, path: dest });
        }
        Ok(files)
    })();
    let files = match result {
        Ok(f) => f,
        Err(e) => {
            // Put back what was there before.
            for (dest, backup) in placed.into_iter().rev() {
                let _ = fs::remove_dir_all(&dest);
                if let Some(old) = backup {
                    let _ = fs::rename(old, dest);
                }
            }
            return Err(e);
        }
    };
    let receipt = Receipt {
        id: entry.id.clone(),
        version: entry.version.clone(),
        source_url: art.url.clone(),
        sha256: got,
        installed_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        files,
        signature,
    };
    write_receipt(&plan.data, &receipt)?;
    for (_, backup) in placed {
        if let Some(old) = backup {
            let _ = fs::remove_dir_all(old);
        }
    }
    // A version that changed its bundle names leaves the old ones behind: those are ours too.
    if let Some(prev) = previous {
        for f in prev.files.iter().filter(|f| !receipt.files.iter().any(|n| n.path == f.path)) {
            if plan.dirs.owns(&f.path) {
                let _ = fs::remove_dir_all(&f.path);
            }
        }
    }
    Ok(receipt)
}

/// Remove exactly what the receipt lists, and nothing outside the user's plugin folders.
pub fn uninstall_plugin(data: &Path, dirs: &UserDirs, id: &str) -> Result<usize> {
    let r = read_receipt(data, id).ok_or_else(|| err("AI-Track did not install this plugin, so it does not remove it"))?;
    let mut removed = 0;
    for f in &r.files {
        if !dirs.owns(&f.path) {
            return Err(err(format!("the receipt names {}, which is not in a user plugin folder; refusing to delete it", f.path.display())));
        }
    }
    for f in &r.files {
        match fs::symlink_metadata(&f.path) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(&f.path).map_err(io)?,
            Ok(_) => fs::remove_file(&f.path).map_err(io)?,
            Err(_) => continue,
        }
        removed += 1;
    }
    fs::remove_file(receipt_path(data, id)).map_err(io)?;
    Ok(removed)
}

// ───────────────────────────── What the tab shows ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuidedWhy {
    /// An account, a form or the maker's own manager.
    Link,
    /// Downloadable, but its sha256 is not pinned yet.
    Unpinned,
    /// No build for this system the app can open.
    NoBuild,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PluginState {
    Available,
    Installing { stage: PluginStage, fraction: f32 },
    Installed { version: String },
    Update { installed: String },
    /// Found in a plugin folder, not installed by us.
    External,
    Guided(GuidedWhy),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginCard {
    pub entry: PluginEntry,
    pub state: PluginState,
    /// What went wrong the last time, when it did.
    pub error: Option<String>,
    /// Download size on this system (0 = unknown).
    pub size: u64,
    /// The maker's domain the file comes from.
    pub host: String,
}

struct Job {
    cancel: Arc<AtomicBool>,
    rx: Receiver<PluginEvent>,
}

/// The tab's engine: the catalog, receipts, and the running installs.
pub struct PluginInstaller {
    catalog: Catalog,
    data: PathBuf,
    dirs: UserDirs,
    os: Option<Os>,
    fetcher: Arc<dyn Fetcher>,
    jobs: HashMap<String, Job>,
    progress: HashMap<String, (PluginStage, f32)>,
    errors: HashMap<String, String>,
    installed_paths: Vec<PathBuf>,
}

impl PluginInstaller {
    pub fn new(data: PathBuf, dirs: UserDirs) -> Self {
        Self::with(Catalog::bundled(), data, dirs, Os::current(), Arc::new(HttpFetcher::default()))
    }

    pub fn with(catalog: Catalog, data: PathBuf, dirs: UserDirs, os: Option<Os>, fetcher: Arc<dyn Fetcher>) -> Self {
        Self { catalog, data, dirs, os, fetcher, jobs: HashMap::new(), progress: HashMap::new(), errors: HashMap::new(), installed_paths: Vec::new() }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The bundle names (lower-case, without extension) sitting in the plugin folders.
    fn names_on_disk(&self) -> std::collections::HashSet<String> {
        let dirs = [&self.dirs.vst3, &self.dirs.clap].into_iter().chain(self.dirs.system.iter());
        dirs.filter_map(|d| fs::read_dir(d).ok())
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                is_bundle(&name).then(|| name.rsplit_once('.').map(|(stem, _)| stem.to_ascii_lowercase())).flatten()
            })
            .collect()
    }

    pub fn cards(&self) -> Vec<PluginCard> {
        let on_disk = self.names_on_disk();
        self.catalog
            .plugin
            .iter()
            .map(|entry| {
                let art = self.os.and_then(|os| entry.artifact_for(os));
                let installable = self.os.is_some_and(|os| entry.installable(os));
                let receipt = read_receipt(&self.data, &entry.id).filter(|r| r.files.iter().all(|f| f.path.exists()));
                let state = if self.jobs.contains_key(&entry.id) {
                    let (stage, fraction) = self.progress.get(&entry.id).copied().unwrap_or((PluginStage::Downloading, 0.));
                    PluginState::Installing { stage, fraction }
                } else if let Some(r) = receipt {
                    if installable && r.version != entry.version { PluginState::Update { installed: r.version } } else { PluginState::Installed { version: r.version } }
                } else if entry.bundle_ids.iter().any(|b| on_disk.contains(&b.to_ascii_lowercase())) {
                    PluginState::External
                } else if installable {
                    PluginState::Available
                } else if entry.lane == Lane::Link {
                    PluginState::Guided(GuidedWhy::Link)
                } else if art.is_none() {
                    PluginState::Guided(GuidedWhy::NoBuild)
                } else {
                    PluginState::Guided(GuidedWhy::Unpinned)
                };
                PluginCard { entry: entry.clone(), state, error: self.errors.get(&entry.id).cloned(), size: art.map_or(0, |a| a.size), host: art.map(Artifact::host).unwrap_or_default() }
            })
            .collect()
    }

    /// Start installing `id`. Nothing is downloaded until this is called.
    pub fn start(&mut self, id: &str) -> Result<()> {
        let entry = self.catalog.get(id).ok_or_else(|| err("unknown plugin"))?.clone();
        let os = self.os.ok_or_else(|| err("plugins are installed on macOS and Windows only"))?;
        if self.jobs.contains_key(id) {
            return Ok(());
        }
        let artifact = entry.artifact_for(os).filter(|_| entry.installable(os)).ok_or_else(|| err(format!("{} cannot be installed from the app: open the maker's page", entry.name)))?.clone();
        let plan = InstallPlan { entry, artifact, os, dirs: self.dirs.clone(), data: self.data.clone() };
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let (stop, fetcher) = (cancel.clone(), self.fetcher.clone());
        std::thread::Builder::new()
            .name("plugin-install".into())
            .spawn(move || {
                let ev = match install_plugin(&plan, fetcher.as_ref(), &stop, &tx) {
                    Ok(r) => PluginEvent::Done(r),
                    Err(_) if stop.load(Ordering::Relaxed) => PluginEvent::Cancelled,
                    Err(e) => PluginEvent::Failed(e.to_string().trim_start_matches("manifest: ").to_owned()),
                };
                let _ = tx.send(ev);
            })
            .map_err(|e| err(e.to_string()))?;
        self.errors.remove(id);
        self.progress.insert(id.to_owned(), (PluginStage::Downloading, 0.));
        self.jobs.insert(id.to_owned(), Job { cancel, rx });
        Ok(())
    }

    pub fn cancel(&mut self, id: &str) {
        if let Some(j) = self.jobs.get(id) {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Move the jobs along. `true` when something the screen shows changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut finished = Vec::new();
        for (id, job) in &self.jobs {
            for ev in job.rx.try_iter() {
                changed = true;
                match ev {
                    PluginEvent::Progress { stage, done, total } => {
                        self.progress.insert(id.clone(), (stage, if total > 0 { (done as f32 / total as f32).clamp(0., 1.) } else { 0. }));
                    }
                    PluginEvent::Done(r) => {
                        self.installed_paths.extend(r.files.iter().map(|f| f.path.clone()));
                        finished.push(id.clone());
                    }
                    PluginEvent::Failed(e) => {
                        self.errors.insert(id.clone(), e);
                        finished.push(id.clone());
                    }
                    PluginEvent::Cancelled => finished.push(id.clone()),
                }
            }
        }
        for id in finished {
            self.jobs.remove(&id);
            self.progress.remove(&id);
        }
        changed
    }

    /// Bundles installed since the last call: the host scans these.
    pub fn take_installed(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.installed_paths)
    }

    pub fn uninstall(&mut self, id: &str) -> Result<usize> {
        let n = uninstall_plugin(&self.data, &self.dirs, id)?;
        self.errors.remove(id);
        Ok(n)
    }

    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }
}

#[cfg(test)]
mod tests;
