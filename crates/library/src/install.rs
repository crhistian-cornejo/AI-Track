//! One-click install of the optional sound bank packs (`[[pack]]` with
//! `optional = true`): download on a background thread with progress and
//! cancel, check the sha256, unpack safely (no entry may escape the target),
//! apply the manifest's strip/include/exclude rules, and leave the pack in
//! `<root>/<dir_name>` with its license and attribution next to it. The
//! caller then indexes that folder into the user library.
//!
//! Link-only Content Hub entries (`[[hub]]`) are never fetched here: their
//! terms forbid redistribution, so the user downloads them from the
//! official page.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};
use sha2::{Digest, Sha256};

use crate::error::{LibraryError, Result};
use crate::hub::{ArchiveFormat, BundledPack};

/// Written into an installed pack's folder; its presence means "installed".
pub const MARKER: &str = ".aitrack-pack";

fn err(msg: impl Into<String>) -> LibraryError {
    LibraryError::Manifest(msg.into())
}

fn io(e: std::io::Error) -> LibraryError {
    err(e.to_string())
}

/// Where `pack` lives once installed under `root`.
pub fn pack_dir(root: &Path, pack: &BundledPack) -> PathBuf {
    root.join(pack.dir_name())
}

pub fn is_installed(root: &Path, pack: &BundledPack) -> bool {
    pack_dir(root, pack).join(MARKER).is_file()
}

/// Whether the app can install this pack itself: redistributable, an https
/// download, and a pinned sha256. A pack without a hash is never installed
/// unverified.
pub fn can_install(pack: &BundledPack) -> bool {
    pack.redistributable
        && crate::catalog::url_host(&pack.download_url).is_some()
        && pack.format != ArchiveFormat::GithubTree
        && pack.sha256.len() == 64
        && pack.sha256.chars().all(|c| c.is_ascii_hexdigit())
}

/// The domains a pack may be fetched from, redirects included: the host of its
/// download URL, and for a GitHub release the hosts GitHub serves assets from.
pub fn pack_hosts(url: &str) -> Vec<String> {
    let Some(host) = crate::catalog::url_host(url) else { return Vec::new() };
    let mut hosts = vec![host.clone()];
    if host == "github.com" {
        hosts.extend(["objects.githubusercontent.com", "release-assets.githubusercontent.com"].map(String::from));
    }
    hosts
}

/// The most a pack may download: its size in the manifest plus a little, or 4 GiB when it does not say.
fn pack_size_cap(pack: &BundledPack) -> u64 {
    if pack.size_mb > 0. { ((pack.size_mb * 1.05) as u64 + 1) << 20 } else { 4 << 30 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Downloading,
    Verifying,
    Unpacking,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstallEvent {
    Progress { stage: Stage, done: u64, total: u64 },
    /// Installed in this folder.
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// A running install. Drop it or call [`InstallJob::cancel`] to stop.
pub struct InstallJob {
    pub pack_id: String,
    rx: Receiver<InstallEvent>,
    cancel: Arc<AtomicBool>,
}

impl InstallJob {
    /// Start installing `pack` under `root` on a background thread.
    pub fn spawn(pack: BundledPack, root: PathBuf) -> Self {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let pack_id = pack.id.clone();
        let run = move || {
            let ev = match install(&pack, &root, &stop, &tx) {
                Ok(dir) => InstallEvent::Done(dir),
                Err(_) if stop.load(Ordering::Relaxed) => InstallEvent::Cancelled,
                Err(e) => InstallEvent::Failed(e.to_string()),
            };
            let _ = tx.send(ev);
        };
        if let Err(e) = std::thread::Builder::new().name("pack-install".into()).spawn(run) {
            log::warn!("pack install: {e}");
        }
        Self { pack_id, rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Events since the last call.
    pub fn poll(&self) -> Vec<InstallEvent> {
        self.rx.try_iter().collect()
    }
}

impl Drop for InstallJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn install(pack: &BundledPack, root: &Path, stop: &AtomicBool, tx: &Sender<InstallEvent>) -> Result<PathBuf> {
    if !can_install(pack) {
        return Err(err(format!("{} cannot be installed from the app", pack.name)));
    }
    let work = root.join(format!(".{}.work", pack.dir_name()));
    if work.exists() {
        fs::remove_dir_all(&work).map_err(io)?;
    }
    fs::create_dir_all(&work).map_err(io)?;
    let result = (|| {
        let archive = work.join("archive");
        // The same download rules as plugins: https, the maker's domains on every redirect, a size cap.
        use crate::plugins::Fetcher;
        crate::plugins::HttpFetcher::default().fetch(&pack.download_url, &pack_hosts(&pack.download_url), pack_size_cap(pack), &archive, stop, &mut |done, total| {
            let _ = tx.send(InstallEvent::Progress { stage: Stage::Downloading, done, total });
        })?;
        let _ = tx.send(InstallEvent::Progress { stage: Stage::Verifying, done: 0, total: 1 });
        verify_sha256(&archive, &pack.sha256)?;
        let _ = tx.send(InstallEvent::Progress { stage: Stage::Unpacking, done: 0, total: 1 });
        let raw = work.join("raw");
        unpack_with(&archive, pack.format, &raw, stop, Limits::PACK)?;
        let staging = work.join("pack");
        normalize(&raw, &staging, &Rules::of(pack))?;
        fs::write(staging.join("ATTRIBUTION.txt"), format!("{}\n\nLicense: {} ({})\nSource: {}\n", pack.credit(), pack.license, pack.license_url, pack.homepage)).map_err(io)?;
        fs::write(staging.join(MARKER), &pack.id).map_err(io)?;
        let dest = pack_dir(root, pack);
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(io)?;
        }
        fs::rename(&staging, &dest).map_err(io)?;
        Ok(dest)
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
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
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got.eq_ignore_ascii_case(expected.trim()) { Ok(()) } else { Err(err("the download is damaged (checksum mismatch)")) }
}

/// Unpack `archive` into `out` with the limits of a sample pack.
pub fn unpack(archive: &Path, format: ArchiveFormat, out: &Path, stop: &AtomicBool) -> Result<()> {
    unpack_with(archive, format, out, stop, Limits::PACK)
}

/// Unpack `archive` into `out`. Entry names are cleaned by hand (no absolute
/// path, no `..`), links must resolve inside `out`, and the entry count, the
/// bytes written and the expansion ratio are limited. Anything suspicious
/// aborts the whole unpacking.
pub fn unpack_with(archive: &Path, format: ArchiveFormat, out: &Path, stop: &AtomicBool, limits: Limits) -> Result<()> {
    fs::create_dir_all(out).map_err(io)?;
    let archive_bytes = fs::metadata(archive).map_err(io)?.len().max(1);
    let open = || File::open(archive).map(BufReader::new).map_err(io);
    match format {
        ArchiveFormat::Zip => unpack_zip_limited(archive, out, stop, limits),
        ArchiveFormat::TarGz => unpack_tar_limited(flate2::read::GzDecoder::new(open()?), out, stop, limits, archive_bytes),
        ArchiveFormat::TarXz => {
            let tar = archive.with_extension("tar");
            {
                let mut w = BufWriter::new(File::create(&tar).map_err(io)?);
                lzma_rs::xz_decompress(&mut open()?, &mut w).map_err(|e| err(format!("xz: {e:?}")))?;
                w.flush().map_err(io)?;
            }
            let r = unpack_tar_limited(File::open(&tar).map(BufReader::new).map_err(io)?, out, stop, limits, archive_bytes);
            let _ = fs::remove_file(&tar);
            r
        }
        ArchiveFormat::SevenZ => unpack_7z(archive, out, stop, limits),
        ArchiveFormat::GithubTree => Err(err("GitHub packs are fetched by `cargo xtask soundbank`")),
    }
}

/// What an archive may expand to before it is refused as a bomb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Entries (files, folders and links).
    pub files: usize,
    /// Bytes once expanded.
    pub bytes: u64,
    /// Expanded bytes per archive byte (audio compresses a few times, never hundreds); 0 = not checked.
    pub ratio: u64,
}

impl Limits {
    /// Sample packs: many large files.
    pub const PACK: Self = Self { files: 200_000, bytes: 8 << 30, ratio: 100 };
    /// Plugins: a few bundles, and nothing that expands 100 times over.
    pub const PLUGIN: Self = Self { files: 20_000, bytes: 2 << 30, ratio: 100 };

    /// The bytes an archive of `archive_bytes` may expand to.
    fn budget(self, archive_bytes: u64) -> u64 {
        if self.ratio > 0 { self.bytes.min(archive_bytes.saturating_mul(self.ratio)) } else { self.bytes }
    }
}

/// An archive entry name as a relative path made of plain components only.
/// `None` for absolute names, `..`, drive prefixes, NULs or nothing at all.
fn clean_rel(name: &str) -> Option<PathBuf> {
    use std::path::Component;
    if name.contains('\0') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in Path::new(name).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// `path` with `.` and `..` resolved by name only. The caller passes a path
/// whose existing parts are already real (canonical), so this is the real place.
fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether a link made in the (real) folder `real_parent`, pointing at `target`, ends inside `root`.
fn link_target_ok(real_parent: &Path, target: &str, root: &Path) -> bool {
    let t = Path::new(target);
    !t.has_root() && !target.contains('\0') && normalize_lexical(&real_parent.join(t)).starts_with(root)
}

/// After an archive is unpacked: every symbolic link under `root` must really
/// resolve inside it. This catches what a name-by-name check cannot, such as a
/// link to `d/..` made before `d` became a link to `.`.
pub fn verify_links_inside(root: &Path) -> Result<()> {
    let root = root.canonicalize().map_err(io)?;
    for e in walkdir::WalkDir::new(&root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !e.file_type().is_symlink() {
            continue;
        }
        let target = fs::read_link(e.path()).map_err(io)?;
        if target.has_root() {
            return Err(err(format!("a link points at an absolute path ({})", e.path().display())));
        }
        let inside = match e.path().canonicalize() {
            Ok(real) => real.starts_with(&root),
            // Dangling: judged by where its name leads from its real folder.
            Err(_) => e.path().parent().and_then(|p| p.canonicalize().ok()).is_some_and(|p| normalize_lexical(&p.join(&target)).starts_with(&root)),
        };
        if !inside {
            return Err(err(format!("a link leaves its folder ({})", e.path().display())));
        }
    }
    Ok(())
}

/// A lexical check used when copying a tree that was already verified: does a
/// link at `link` (relative to the tree's root) pointing at `target` stay inside?
pub(crate) fn link_stays_inside(link: &Path, target: &str) -> bool {
    use std::path::Component;
    let target = Path::new(target);
    if target.has_root() || target.components().any(|c| matches!(c, Component::Prefix(_))) {
        return false;
    }
    let mut depth: i64 = link.parent().map_or(0, |p| p.components().filter(|c| matches!(c, Component::Normal(_))).count() as i64);
    for c in target.components() {
        match c {
            Component::ParentDir => depth -= 1,
            Component::Normal(_) => depth += 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// Make `dir` (and its parents) and check that, with links resolved, it is inside `root`.
fn ensure_dir_inside(dir: &Path, root: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(io)?;
    if dir.canonicalize().is_ok_and(|c| c.starts_with(root)) { Ok(()) } else { Err(err("the archive tried to write outside its folder")) }
}

/// Unpack a zip keeping what bundles need: Unix permission bits (without
/// setuid/setgid/sticky) and symbolic links that stay inside `out`. A link
/// that points outside, too many entries, or more bytes than `limits` allow
/// (counted as they are written, not as the headers claim) abort the whole
/// unpacking.
pub fn unpack_zip_limited(archive: &Path, out: &Path, stop: &AtomicBool, limits: Limits) -> Result<()> {
    let archive_bytes = fs::metadata(archive).map_err(io)?.len().max(1);
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(io)?)).map_err(|e| err(format!("zip: {e}")))?;
    if z.len() > limits.files {
        return Err(err(format!("the archive has too many entries ({} > {})", z.len(), limits.files)));
    }
    fs::create_dir_all(out).map_err(io)?;
    let root = out.canonicalize().map_err(io)?;
    let mut written = 0u64;
    let budget = limits.budget(archive_bytes);
    for i in 0..z.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(err("cancelled"));
        }
        let mut f = z.by_index(i).map_err(|e| err(format!("zip: {e}")))?;
        let Some(rel) = clean_rel(f.name()) else {
            // `enclosed_name` used to skip these quietly; an archive that tries them is hostile.
            return Err(err(format!("the archive has an entry with an unsafe name ({:?})", f.name())));
        };
        let target = root.join(&rel);
        let mode = f.unix_mode().map(|m| m & 0o777);
        if f.is_dir() {
            ensure_dir_inside(&target, &root)?;
            set_mode(&target, mode.map(|m| m | 0o700));
            continue;
        }
        let parent = target.parent().unwrap_or(&root).to_path_buf();
        // Nothing is written through a link that leads out of the folder.
        ensure_dir_inside(&parent, &root)?;
        let real_parent = parent.canonicalize().map_err(io)?;
        if f.is_symlink() {
            let mut link = String::new();
            f.by_ref().take(4096).read_to_string(&mut link).map_err(io)?;
            if !link_target_ok(&real_parent, &link, &root) {
                return Err(err(format!("the archive has a link that leaves its folder ({})", rel.display())));
            }
            make_symlink(&link, &real_parent.join(target.file_name().unwrap_or_default()))?;
            continue;
        }
        let dest_path = real_parent.join(target.file_name().unwrap_or_default());
        // Never create a file where a link is already waiting.
        if fs::symlink_metadata(&dest_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(err("the archive writes a file over one of its own links"));
        }
        let mut dest = BufWriter::new(File::create(&dest_path).map_err(io)?);
        let n = std::io::copy(&mut f.by_ref().take(budget.saturating_sub(written) + 1), &mut dest).map_err(io)?;
        written += n;
        if written > budget {
            return Err(err("the archive expands to more than it is allowed to (zip bomb?)"));
        }
        dest.flush().map_err(io)?;
        drop(dest);
        set_mode(&dest_path, mode.map(|m| m | 0o600));
    }
    verify_links_inside(&root)
}

/// 7z: entries are written one by one to names cleaned here, never to the
/// path the library would build, so `../x` and `/x` cannot leave `out`.
fn unpack_7z(archive: &Path, out: &Path, stop: &AtomicBool, limits: Limits) -> Result<()> {
    let archive_bytes = fs::metadata(archive).map_err(io)?.len().max(1);
    fs::create_dir_all(out).map_err(io)?;
    let root = out.canonicalize().map_err(io)?;
    let budget = limits.budget(archive_bytes);
    let (mut files, mut written) = (0usize, 0u64);
    let mut failure: Option<LibraryError> = None;
    let mut one = |entry: &sevenz_rust2::ArchiveEntry, reader: &mut dyn Read| -> Result<()> {
        if stop.load(Ordering::Relaxed) {
            return Err(err("cancelled"));
        }
        files += 1;
        if files > limits.files {
            return Err(err("the archive has too many entries"));
        }
        if entry.is_anti_item() {
            return Ok(());
        }
        let Some(rel) = clean_rel(entry.name()) else { return Err(err(format!("the archive has an entry with an unsafe name ({:?})", entry.name()))) };
        let target = root.join(&rel);
        if entry.is_directory() {
            return ensure_dir_inside(&target, &root);
        }
        let parent = target.parent().unwrap_or(&root).to_path_buf();
        ensure_dir_inside(&parent, &root)?;
        let dest_path = parent.canonicalize().map_err(io)?.join(target.file_name().unwrap_or_default());
        if fs::symlink_metadata(&dest_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(err("the archive writes a file over a link"));
        }
        let mut dest = BufWriter::new(File::create(&dest_path).map_err(io)?);
        let n = std::io::copy(&mut reader.take(budget.saturating_sub(written) + 1), &mut dest).map_err(io)?;
        written += n;
        if written > budget {
            return Err(err("the archive expands to more than it is allowed to (bomb?)"));
        }
        dest.flush().map_err(io)
    };
    let r = sevenz_rust2::decompress_file_with_extract_fn(archive, &root, |entry, reader, _library_path| match one(entry, reader) {
        Ok(()) => Ok(true),
        Err(e) => {
            failure = Some(e);
            // Stop reading: nothing more is written.
            Ok(false)
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    r.map_err(|e| err(format!("7z: {e}")))?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(m) = mode {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(m));
    }
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: Option<u32>) {}

#[cfg(unix)]
fn make_symlink(target: &str, at: &Path) -> Result<()> {
    let _ = fs::remove_file(at);
    std::os::unix::fs::symlink(target, at).map_err(io)
}

/// Windows cannot make links without a privilege; a plugin that needs them is macOS-only anyway.
#[cfg(not(unix))]
fn make_symlink(_: &str, _: &Path) -> Result<()> {
    Ok(())
}

/// Unpack a tar with the same rules as the zip: clean names, links that stay
/// inside, no hard links, permission bits without setuid/setgid/sticky, limits.
pub(crate) fn unpack_tar_limited<R: Read>(r: R, out: &Path, stop: &AtomicBool, limits: Limits, archive_bytes: u64) -> Result<()> {
    fs::create_dir_all(out).map_err(io)?;
    let root = out.canonicalize().map_err(io)?;
    let budget = limits.budget(archive_bytes);
    let mut a = tar::Archive::new(r);
    // The mode is set by hand below, so no setuid bit can come through.
    a.set_preserve_permissions(false);
    a.set_unpack_xattrs(false);
    let (mut files, mut bytes) = (0usize, 0u64);
    for entry in a.entries().map_err(io)? {
        if stop.load(Ordering::Relaxed) {
            return Err(err("cancelled"));
        }
        let mut entry = entry.map_err(io)?;
        files += 1;
        bytes += entry.size();
        if files > limits.files || bytes > budget {
            return Err(err("the archive expands to more than it is allowed to"));
        }
        let kind = entry.header().entry_type();
        let name = entry.path().map_err(io)?.to_string_lossy().into_owned();
        let Some(rel) = clean_rel(&name) else { return Err(err(format!("the archive has an entry with an unsafe name ({name:?})"))) };
        let target = root.join(&rel);
        ensure_dir_inside(target.parent().unwrap_or(&root), &root)?;
        let mode = entry.header().mode().ok().map(|m| m & 0o777);
        if kind.is_hard_link() {
            return Err(err("the archive has a hard link"));
        }
        if kind.is_symlink() {
            let link = entry.link_name().map_err(io)?.map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
            let real_parent = target.parent().unwrap_or(&root).canonicalize().map_err(io)?;
            if !link_target_ok(&real_parent, &link, &root) {
                return Err(err(format!("the archive has a link that leaves its folder ({})", rel.display())));
            }
        }
        if !entry.unpack_in(&root).map_err(io)? {
            return Err(err("the archive tried to write outside its folder"));
        }
        if kind.is_file() {
            set_mode(&target, mode.map(|m| m | 0o600));
        } else if kind.is_dir() {
            set_mode(&target, mode.map(|m| m | 0o700));
        }
    }
    verify_links_inside(&root)
}

/// The manifest's file selection for one pack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    pub strip_prefix: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub license_file: String,
}

impl Rules {
    pub fn of(p: &BundledPack) -> Self {
        Self { strip_prefix: p.strip_prefix.clone(), include: p.include.clone(), exclude: p.exclude.clone(), license_file: p.license_file.clone() }
    }

    /// Whether a file at `rel` (relative to the stripped base, `/`-separated) is kept.
    pub fn keeps(&self, rel: &str) -> bool {
        let junk = rel.split('/').any(|c| c == "__MACOSX" || c == ".DS_Store" || c.starts_with("._") || c == "Thumbs.db");
        !junk && (self.include.is_empty() || self.include.iter().any(|i| rel.starts_with(i.as_str()))) && !self.exclude.iter().any(|x| rel.contains(x.as_str()))
    }
}

/// The folder the pack's files are relative to: the explicit prefix, or the
/// archive's single top folder (repeated while there is only one).
fn base_dir(raw: &Path, rules: &Rules) -> Result<PathBuf> {
    if !rules.strip_prefix.is_empty() {
        return Ok(raw.join(rules.strip_prefix.trim_end_matches('/')));
    }
    let mut base = raw.to_path_buf();
    loop {
        let entries: Vec<_> = fs::read_dir(&base).map_err(io)?.filter_map(|e| e.ok()).filter(|e| Rules::default().keeps(&e.file_name().to_string_lossy())).collect();
        if entries.len() == 1 && entries[0].path().is_dir() {
            base = entries[0].path();
        } else {
            return Ok(base);
        }
    }
}

/// Move the kept files of the unpacked tree `raw` into `dest`, with the
/// license file as `LICENSE`.
pub fn normalize(raw: &Path, dest: &Path, rules: &Rules) -> Result<()> {
    let base = base_dir(raw, rules)?;
    fs::create_dir_all(dest).map_err(io)?;
    for e in walkdir::WalkDir::new(&base).into_iter().filter_map(|e| e.ok()) {
        if !e.file_type().is_file() {
            continue;
        }
        let rel = e.path().strip_prefix(&base).map_err(|x| err(x.to_string()))?.to_string_lossy().replace('\\', "/");
        let full = e.path().strip_prefix(raw).map_err(|x| err(x.to_string()))?.to_string_lossy().replace('\\', "/");
        let is_license = !rules.license_file.is_empty() && (rel == rules.license_file || full == rules.license_file);
        if is_license {
            fs::copy(e.path(), dest.join("LICENSE")).map_err(io)?;
        }
        if !rules.keeps(&rel) {
            continue;
        }
        let target = dest.join(&rel);
        if let Some(p) = target.parent() {
            fs::create_dir_all(p).map_err(io)?;
        }
        fs::rename(e.path(), &target).or_else(|_| fs::copy(e.path(), &target).map(|_| ())).map_err(io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (path, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, path, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn rules_keep_and_drop() {
        let r = Rules { include: vec!["Samples/".into()], exclude: vec!["amen".into()], ..Default::default() };
        assert!(r.keeps("Samples/kick.wav"));
        assert!(!r.keeps("Docs/readme.txt"));
        assert!(!r.keeps("Samples/loop_amen.wav"));
        assert!(!r.keeps("Samples/__MACOSX/x.wav"));
        assert!(!r.keeps("Samples/._kick.wav"));
    }

    #[test]
    fn unpacks_strips_the_top_folder_and_places_the_license() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        fs::write(&archive, tar_gz(&[("Pack-1.0/Kicks/k1.wav", b"RIFF"), ("Pack-1.0/LICENSE.txt", b"CC0"), ("Pack-1.0/.DS_Store", b"x")])).unwrap();
        let raw = dir.path().join("raw");
        unpack(&archive, ArchiveFormat::TarGz, &raw, &AtomicBool::new(false)).unwrap();
        let dest = dir.path().join("pack");
        let rules = Rules { license_file: "LICENSE.txt".into(), ..Default::default() };
        normalize(&raw, &dest, &rules).unwrap();
        assert_eq!(fs::read(dest.join("Kicks/k1.wav")).unwrap(), b"RIFF");
        assert_eq!(fs::read(dest.join("LICENSE")).unwrap(), b"CC0");
        assert!(!dest.join(".DS_Store").exists());
    }

    #[test]
    fn zip_entries_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&archive).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("../evil.wav", o).unwrap();
            z.write_all(b"x").unwrap();
            z.start_file("ok/s.wav", o).unwrap();
            z.write_all(b"y").unwrap();
            z.finish().unwrap();
        }
        let out = dir.path().join("out");
        // An archive that tries to leave its folder is refused whole, and nothing lands outside.
        assert!(unpack(&archive, ArchiveFormat::Zip, &out, &AtomicBool::new(false)).is_err());
        assert!(!dir.path().join("evil.wav").exists());
    }

    #[test]
    fn checksum_and_install_rules() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        fs::write(&f, b"abc").unwrap();
        verify_sha256(&f, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").unwrap();
        assert!(verify_sha256(&f, "00").is_err());
        let m = crate::Manifest::bundled();
        let optional: Vec<_> = m.packs.iter().filter(|p| p.optional).collect();
        assert!(!optional.is_empty() && optional.iter().all(|p| can_install(p)));
        let gh = m.packs.iter().find(|p| p.format == ArchiveFormat::GithubTree).unwrap();
        assert!(!can_install(gh));
        assert!(!is_installed(dir.path(), optional[0]));
    }

    #[test]
    fn a_pack_may_only_come_from_its_own_host() {
        assert_eq!(pack_hosts("https://freepats.zenvoid.org/Piano/x.tar.xz"), vec!["freepats.zenvoid.org".to_owned()]);
        let gh = pack_hosts("https://github.com/o/r/releases/download/v1/a.zip");
        assert!(gh.contains(&"github.com".to_owned()) && gh.contains(&"objects.githubusercontent.com".to_owned()));
        assert!(pack_hosts("http://example.org/a.zip").is_empty(), "no https, no host, no download");
        let p = BundledPack { size_mb: 10., ..Default::default() };
        assert_eq!(pack_size_cap(&p), 11 << 20);
        assert_eq!(pack_size_cap(&BundledPack::default()), 4 << 30);
    }

    #[test]
    fn a_pack_without_download_fails_without_network() {
        let job = InstallJob::spawn(BundledPack { id: "x".into(), name: "X".into(), ..Default::default() }, std::env::temp_dir());
        let t0 = std::time::Instant::now();
        let mut ev = Vec::new();
        while ev.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            ev = job.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(ev.last(), Some(InstallEvent::Failed(_))), "{ev:?}");
    }

    fn zip_with(path: &Path, f: impl FnOnce(&mut zip::ZipWriter<File>)) {
        let mut z = zip::ZipWriter::new(File::create(path).unwrap());
        f(&mut z);
        z.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_bundle_keeps_its_permissions_and_its_links() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("b.zip");
        zip_with(&archive, |z| {
            let exe = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
            let plain = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
            z.start_file("X.vst3/Contents/MacOS/X", exe).unwrap();
            z.write_all(b"\xcf\xfa").unwrap();
            z.start_file("X.vst3/Contents/Resources/a.txt", plain).unwrap();
            z.write_all(b"a").unwrap();
            z.add_symlink("X.vst3/Contents/Resources/latest", "a.txt", plain).unwrap();
            z.add_symlink("X.vst3/Contents/Current", "../Contents/MacOS", plain).unwrap();
        });
        let out = dir.path().join("out");
        unpack(&archive, ArchiveFormat::Zip, &out, &AtomicBool::new(false)).unwrap();
        let exe = out.join("X.vst3/Contents/MacOS/X");
        assert_eq!(fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755, "the executable bit survives");
        assert_eq!(fs::metadata(out.join("X.vst3/Contents/Resources/a.txt")).unwrap().permissions().mode() & 0o777, 0o644);
        assert_eq!(fs::read_link(out.join("X.vst3/Contents/Resources/latest")).unwrap(), Path::new("a.txt"));
        assert_eq!(fs::read(out.join("X.vst3/Contents/Resources/latest")).unwrap(), b"a", "and the link works");
        assert!(fs::symlink_metadata(out.join("X.vst3/Contents/Current")).unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn setuid_bits_are_dropped_and_links_that_leave_the_folder_abort() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("s.zip");
        zip_with(&archive, |z| {
            z.start_file("a/tool", zip::write::SimpleFileOptions::default().unix_permissions(0o4755)).unwrap();
            z.write_all(b"x").unwrap();
        });
        let out = dir.path().join("out");
        unpack(&archive, ArchiveFormat::Zip, &out, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::metadata(out.join("a/tool")).unwrap().permissions().mode() & 0o7777, 0o755);
        for target in ["../../etc/passwd", "/etc/passwd", "a/../../../x"] {
            let bad = dir.path().join("bad.zip");
            zip_with(&bad, |z| {
                z.add_symlink("a/link", target, zip::write::SimpleFileOptions::default()).unwrap();
            });
            let o2 = dir.path().join(format!("o-{}", target.len()));
            assert!(unpack(&bad, ArchiveFormat::Zip, &o2, &AtomicBool::new(false)).is_err(), "{target}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("t.zip");
        // `d` points at `.` (inside), then `d/../..` style paths are refused by enclosed_name; a link to a
        // sibling folder is fine, but a file cannot be placed through a link that left the root.
        zip_with(&archive, |z| {
            let o = zip::write::SimpleFileOptions::default();
            z.add_symlink("d", ".", o).unwrap();
            z.start_file("d/f.txt", o).unwrap();
            z.write_all(b"ok").unwrap();
        });
        let out = dir.path().join("out");
        unpack(&archive, ArchiveFormat::Zip, &out, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(out.join("f.txt")).unwrap(), b"ok", "a link to the root is inside the root");
    }

    #[test]
    fn a_zip_bomb_is_stopped_by_entries_bytes_and_ratio() {
        let dir = tempfile::tempdir().unwrap();
        // Many entries.
        let many = dir.path().join("many.zip");
        zip_with(&many, |z| {
            for i in 0..30 {
                z.start_file(format!("f{i}"), zip::write::SimpleFileOptions::default()).unwrap();
            }
        });
        let small = Limits { files: 10, bytes: 1 << 20, ratio: 0 };
        assert!(unpack_zip_limited(&many, &dir.path().join("o1"), &AtomicBool::new(false), small).is_err());
        // Zeros compress hugely: 8 MB of nothing in a few KB.
        let bomb = dir.path().join("bomb.zip");
        zip_with(&bomb, |z| {
            z.start_file("zeros", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
            z.write_all(&vec![0u8; 8 << 20]).unwrap();
        });
        let bytes = Limits { files: 10, bytes: 1 << 20, ratio: 0 };
        assert!(unpack_zip_limited(&bomb, &dir.path().join("o2"), &AtomicBool::new(false), bytes).is_err(), "over the byte limit");
        let ratio = Limits { files: 10, bytes: 1 << 30, ratio: 100 };
        assert!(unpack_zip_limited(&bomb, &dir.path().join("o3"), &AtomicBool::new(false), ratio).is_err(), "over the ratio");
        let fine = Limits { files: 10, bytes: 1 << 30, ratio: 0 };
        assert!(unpack_zip_limited(&bomb, &dir.path().join("o4"), &AtomicBool::new(false), fine).is_ok());
    }
}
