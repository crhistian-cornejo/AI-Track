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
use std::time::Duration;

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

/// Whether the app can install this pack itself.
pub fn can_install(pack: &BundledPack) -> bool {
    pack.redistributable && !pack.download_url.is_empty() && pack.download_url.starts_with("https://") && pack.format != ArchiveFormat::GithubTree
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
        download(&pack.download_url, &archive, stop, &mut |done, total| {
            let _ = tx.send(InstallEvent::Progress { stage: Stage::Downloading, done, total });
        })?;
        if !pack.sha256.is_empty() {
            let _ = tx.send(InstallEvent::Progress { stage: Stage::Verifying, done: 0, total: 1 });
            verify_sha256(&archive, &pack.sha256)?;
        }
        let _ = tx.send(InstallEvent::Progress { stage: Stage::Unpacking, done: 0, total: 1 });
        let raw = work.join("raw");
        unpack(&archive, pack.format, &raw, stop)?;
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

/// Stream `url` into `dest`, reporting `(bytes, total)` (total 0 = unknown).
pub fn download(url: &str, dest: &Path, stop: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(20))).user_agent("AI-Track").build().into();
    let resp = agent.get(url).call().map_err(|e| err(format!("download failed: {e}")))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut reader = resp.into_body().into_reader();
    let mut out = BufWriter::new(File::create(dest).map_err(io)?);
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(err("cancelled"));
        }
        let n = reader.read(&mut buf).map_err(|e| err(format!("download failed: {e}")))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(io)?;
        done += n as u64;
        if done - last >= 1 << 20 {
            last = done;
            progress(done, total);
        }
    }
    out.flush().map_err(io)?;
    progress(done, total.max(done));
    Ok(())
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

/// Unpack `archive` into `out`. Entries that would land outside `out` are skipped.
pub fn unpack(archive: &Path, format: ArchiveFormat, out: &Path, stop: &AtomicBool) -> Result<()> {
    fs::create_dir_all(out).map_err(io)?;
    let open = || File::open(archive).map(BufReader::new).map_err(io);
    match format {
        ArchiveFormat::Zip => unpack_zip(archive, out, stop),
        ArchiveFormat::TarGz => unpack_tar(flate2::read::GzDecoder::new(open()?), out, stop),
        ArchiveFormat::TarXz => {
            let tar = archive.with_extension("tar");
            {
                let mut w = BufWriter::new(File::create(&tar).map_err(io)?);
                lzma_rs::xz_decompress(&mut open()?, &mut w).map_err(|e| err(format!("xz: {e:?}")))?;
                w.flush().map_err(io)?;
            }
            let r = unpack_tar(File::open(&tar).map(BufReader::new).map_err(io)?, out, stop);
            let _ = fs::remove_file(&tar);
            r
        }
        ArchiveFormat::SevenZ => {
            sevenz_rust2::decompress_file(archive, out).map_err(|e| err(format!("7z: {e}")))?;
            let root = out.canonicalize().map_err(io)?;
            for e in walkdir::WalkDir::new(out).into_iter().filter_map(|e| e.ok()) {
                if e.path().canonicalize().is_ok_and(|p| !p.starts_with(&root)) {
                    return Err(err("the archive tried to write outside its folder"));
                }
            }
            Ok(())
        }
        ArchiveFormat::GithubTree => Err(err("GitHub packs are fetched by `cargo xtask soundbank`")),
    }
}

fn unpack_zip(archive: &Path, out: &Path, stop: &AtomicBool) -> Result<()> {
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(io)?)).map_err(|e| err(format!("zip: {e}")))?;
    for i in 0..z.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(err("cancelled"));
        }
        let mut f = z.by_index(i).map_err(|e| err(format!("zip: {e}")))?;
        let Some(rel) = f.enclosed_name() else { continue };
        let target = out.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&target).map_err(io)?;
            continue;
        }
        if let Some(p) = target.parent() {
            fs::create_dir_all(p).map_err(io)?;
        }
        std::io::copy(&mut f, &mut BufWriter::new(File::create(&target).map_err(io)?)).map_err(io)?;
    }
    Ok(())
}

fn unpack_tar<R: Read>(r: R, out: &Path, stop: &AtomicBool) -> Result<()> {
    let mut a = tar::Archive::new(r);
    for entry in a.entries().map_err(io)? {
        if stop.load(Ordering::Relaxed) {
            return Err(err("cancelled"));
        }
        // `unpack_in` refuses entries that would escape `out`.
        entry.map_err(io)?.unpack_in(out).map_err(io)?;
    }
    Ok(())
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
        unpack(&archive, ArchiveFormat::Zip, &out, &AtomicBool::new(false)).unwrap();
        assert!(out.join("ok/s.wav").is_file());
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
}
