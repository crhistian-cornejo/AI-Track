//! Regression tests for the archive attacks found by the independent review
//! (7z zip-slip, symlink chains made of links to ".", setuid in tars, packs
//! installed without a pinned hash). Each builds its archive on the spot and
//! touches nothing outside its own temp folder; none uses the network.
#![cfg(unix)]

use aitrack_library::hub::{ArchiveFormat, BundledPack};
use aitrack_library::install::{Limits, can_install, unpack, unpack_zip_limited};
use aitrack_library::plugins::{copy_tree, find_bundles, verify_bundle_links};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use zip::write::SimpleFileOptions;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aitrack-atk-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn opts(mode: u32) -> SimpleFileOptions {
    SimpleFileOptions::default().unix_permissions(mode).compression_method(zip::CompressionMethod::Deflated)
}

struct Z(zip::ZipWriter<File>);

impl Z {
    fn new(p: &Path) -> Self {
        Z(zip::ZipWriter::new(File::create(p).unwrap()))
    }
    fn file(&mut self, name: &str, data: &[u8], mode: u32) {
        self.0.start_file(name, opts(mode)).unwrap();
        self.0.write_all(data).unwrap();
    }
    fn dir(&mut self, name: &str) {
        self.0.add_directory(name, opts(0o755)).unwrap();
    }
    fn link(&mut self, name: &str, target: &str) {
        self.0.add_symlink(name, target, opts(0o777)).unwrap();
    }
    fn done(self) {
        self.0.finish().unwrap();
    }
}

fn seven_z(path: &Path, entries: &[(&str, &[u8])]) {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let mut w = ArchiveWriter::create(path).unwrap();
    for (name, data) in entries {
        let mut e = ArchiveEntry::new_file(name);
        e.has_stream = true;
        w.push_archive_entry::<&[u8]>(e, Some(*data)).unwrap();
    }
    w.finish().unwrap();
}

/// Every symlink under `root` that really points outside it.
fn escaping_links(root: &Path) -> Vec<PathBuf> {
    let root = root.canonicalize().unwrap();
    walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_symlink())
        .filter(|e| e.path().canonicalize().is_ok_and(|p| !p.starts_with(&root)))
        .map(|e| e.path().to_path_buf())
        .collect()
}

// ───────────── 1. 7z zip-slip ─────────────

#[test]
fn a_7z_entry_with_dotdot_cannot_write_outside_the_folder() {
    let d = tmp("sz1");
    let victim = d.join("VICTIM");
    fs::create_dir_all(&victim).unwrap();
    let archive = d.join("evil.7z");
    seven_z(&archive, &[("../VICTIM/pwned.sh", b"#!/bin/sh\necho pwned\n"), ("innocent.txt", b"ok")]);
    let r = unpack(&archive, ArchiveFormat::SevenZ, &d.join("out"), &AtomicBool::new(false));
    assert!(!victim.join("pwned.sh").exists(), "the file escaped the extraction folder");
    assert!(r.is_err(), "an archive with a `..` entry is refused as a whole");
}

#[test]
fn a_7z_entry_with_an_absolute_path_cannot_write_there() {
    let d = tmp("sz2");
    let far = d.join("FAR");
    fs::create_dir_all(&far).unwrap();
    let target = far.join("absolute-probe");
    let archive = d.join("evil2.7z");
    seven_z(&archive, &[(target.to_str().unwrap(), b"abs")]);
    let r = unpack(&archive, ArchiveFormat::SevenZ, &d.join("out"), &AtomicBool::new(false));
    assert!(!target.exists(), "an absolute entry name was written to that absolute path");
    assert!(r.is_err());
    // And far above: five levels up.
    let archive = d.join("evil3.7z");
    seven_z(&archive, &[("../../../../../FAR/LaunchAgent.plist", b"<plist/>")]);
    assert!(unpack(&archive, ArchiveFormat::SevenZ, &d.join("deep/a/b/c/out"), &AtomicBool::new(false)).is_err());
    assert!(!far.join("LaunchAgent.plist").exists());
}

#[test]
fn a_normal_7z_still_unpacks_with_its_folders() {
    let d = tmp("sz3");
    let archive = d.join("ok.7z");
    seven_z(&archive, &[("Pack/Samples/kick.wav", b"RIFF0000"), ("Pack/LICENSE.txt", b"CC0")]);
    let out = d.join("out");
    unpack(&archive, ArchiveFormat::SevenZ, &out, &AtomicBool::new(false)).unwrap();
    assert_eq!(fs::read(out.join("Pack/Samples/kick.wav")).unwrap(), b"RIFF0000");
    assert_eq!(fs::read(out.join("Pack/LICENSE.txt")).unwrap(), b"CC0");
}

#[test]
fn a_7z_bomb_is_stopped_by_the_ratio() {
    let d = tmp("sz4");
    let archive = d.join("bomb.7z");
    let zeros = vec![0u8; 48 << 20];
    seven_z(&archive, &[("zeros.bin", &zeros)]);
    let out = d.join("out");
    assert!(unpack(&archive, ArchiveFormat::SevenZ, &out, &AtomicBool::new(false)).is_err(), "far over the ratio a sample pack may expand to");
    let left = fs::metadata(out.join("zeros.bin")).map_or(0, |m| m.len());
    assert!(left < (40 << 20), "the bomb was written out almost in full ({left} bytes)");
}

// ───────────── 2. symlinks that escape ─────────────

#[test]
fn n_links_to_dot_do_not_buy_an_escape_to_applications() {
    let d = tmp("a11");
    let zp = d.join("a.zip");
    const N: usize = 24;
    let mut z = Z::new(&zp);
    for i in 0..N {
        z.link(&format!("l{i}"), ".");
    }
    let deep: String = (0..N).map(|i| format!("l{i}/")).collect();
    let ups: String = "../".repeat(N);
    z.link(&format!("{deep}Evil.vst3"), &format!("{ups}Applications"));
    z.done();
    let out = d.join("out");
    let r = unpack_zip_limited(&zp, &out, &AtomicBool::new(false), Limits::PLUGIN);
    assert!(r.is_err(), "a link that really points at /Applications is refused");
    assert!(escaping_links(&out).is_empty(), "left a link pointing outside: {:?}", escaping_links(&out));
    assert!(find_bundles(&out).is_empty(), "a link must never be offered as a plugin bundle");
}

#[test]
fn a_link_to_dot_then_a_link_climbing_through_it_is_refused() {
    let d = tmp("a9");
    let victim = d.join("VICTIM");
    fs::create_dir_all(&victim).unwrap();
    let zp = d.join("a.zip");
    let mut z = Z::new(&zp);
    z.link("a", ".");
    z.dir("a/b/");
    z.link("a/b/l", "../../VICTIM");
    z.file("a/b/l/pwned.sh", b"#!/bin/sh\necho pwned\n", 0o755);
    z.done();
    let out = d.join("out");
    let r = unpack_zip_limited(&zp, &out, &AtomicBool::new(false), Limits::PLUGIN);
    assert!(r.is_err());
    assert!(escaping_links(&out).is_empty());
    assert!(!victim.join("pwned.sh").exists(), "written through the escaping link");
}

#[test]
fn a_link_whose_target_is_made_to_escape_later_is_caught_at_the_end() {
    // L -> d/.. is inside while `d` does not exist; then `d` becomes a link to ".", and L is really `out/..`.
    let d = tmp("late");
    let zp = d.join("a.zip");
    let mut z = Z::new(&zp);
    z.link("L", "d/..");
    z.link("d", ".");
    z.done();
    let out = d.join("out");
    let r = unpack_zip_limited(&zp, &out, &AtomicBool::new(false), Limits::PLUGIN);
    assert!(r.is_err(), "L really points at the folder above");
}

#[test]
fn find_bundles_never_returns_a_link() {
    let d = tmp("a7");
    let root = d.join("raw");
    fs::create_dir_all(root.join("real/Good.vst3/Contents")).unwrap();
    std::os::unix::fs::symlink(".", root.join("Sneaky.vst3")).unwrap();
    std::os::unix::fs::symlink("/Applications", root.join("real/Other.clap")).unwrap();
    let found: Vec<_> = find_bundles(&root).into_iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(found, vec!["Good.vst3".to_owned()]);
}

#[test]
fn a_link_inside_a_bundle_may_not_leave_the_bundle() {
    let d = tmp("a8");
    let src = d.join("Bundle.vst3");
    fs::create_dir_all(src.join("Contents/MacOS")).unwrap();
    fs::create_dir_all(src.join("Contents/Resources")).unwrap();
    fs::write(src.join("Contents/Resources/a.txt"), b"a").unwrap();
    // A link that stays in the bundle is what macOS bundles use (`Versions/Current`).
    std::os::unix::fs::symlink("../Resources", src.join("Contents/MacOS/res")).unwrap();
    verify_bundle_links(&src).unwrap();
    let ok = d.join("dest/Bundle.vst3");
    fs::create_dir_all(d.join("dest")).unwrap();
    copy_tree(&src, &ok, Limits::PLUGIN).unwrap();
    assert_eq!(fs::read(ok.join("Contents/MacOS/res/a.txt")).unwrap(), b"a");
    // Up to the folder that holds the bundle is outside it.
    std::os::unix::fs::symlink("../../../x", src.join("Contents/MacOS/l")).unwrap();
    assert!(verify_bundle_links(&src).is_err(), "../../../x leaves Bundle.vst3");
    assert!(copy_tree(&src, &d.join("dest/Second.vst3"), Limits::PLUGIN).is_err(), "and the copy refuses it too");
    fs::remove_file(src.join("Contents/MacOS/l")).unwrap();
    std::os::unix::fs::symlink("/etc", src.join("Contents/abs")).unwrap();
    assert!(verify_bundle_links(&src).is_err(), "absolute targets are never allowed");
}

// ───────────── setuid, plain zip-slip ─────────────

fn tar_gz(path: &Path, entries: &[(&str, &[u8], u32)]) {
    let gz = flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
    let mut b = tar::Builder::new(gz);
    for (name, data, mode) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_path(name).unwrap();
        h.set_size(data.len() as u64);
        h.set_mode(*mode);
        h.set_cksum();
        b.append(&h, *data).unwrap();
    }
    b.into_inner().unwrap().finish().unwrap();
}

#[test]
fn a_tar_keeps_the_executable_bit_but_never_setuid() {
    let d = tmp("a5");
    let archive = d.join("a.tar.gz");
    tar_gz(&archive, &[("Evil.vst3/Contents/MacOS/Evil", b"#!/bin/sh\n", 0o4755), ("Evil.vst3/Contents/Resources/r.txt", b"r", 0o2644)]);
    let out = d.join("out");
    unpack(&archive, ArchiveFormat::TarGz, &out, &AtomicBool::new(false)).unwrap();
    let mode = |p: &str| fs::metadata(out.join(p)).unwrap().permissions().mode();
    assert_eq!(mode("Evil.vst3/Contents/MacOS/Evil") & 0o7777, 0o755, "setuid dropped, execute kept");
    assert_eq!(mode("Evil.vst3/Contents/Resources/r.txt") & 0o7777, 0o644, "setgid dropped");
}

#[test]
fn a_tar_with_a_link_that_leaves_the_folder_is_refused() {
    let d = tmp("tarlink");
    let archive = d.join("a.tar.gz");
    let gz = flate2::write::GzEncoder::new(File::create(&archive).unwrap(), flate2::Compression::fast());
    let mut b = tar::Builder::new(gz);
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Symlink);
    h.set_size(0);
    h.set_mode(0o777);
    b.append_link(&mut h, "Evil.vst3", "../../../../../../etc").unwrap();
    b.into_inner().unwrap().finish().unwrap();
    let out = d.join("out");
    assert!(unpack(&archive, ArchiveFormat::TarGz, &out, &AtomicBool::new(false)).is_err());
    assert!(escaping_links(&out).is_empty());
}

#[test]
fn a_zip_keeps_the_executable_bit_but_never_setuid() {
    let d = tmp("a4");
    let zp = d.join("a.zip");
    let mut z = Z::new(&zp);
    z.file("Evil.vst3/Contents/MacOS/Evil", b"#!/bin/sh\nid\n", 0o4755);
    z.done();
    let out = d.join("out");
    unpack_zip_limited(&zp, &out, &AtomicBool::new(false), Limits::PLUGIN).unwrap();
    assert_eq!(fs::metadata(out.join("Evil.vst3/Contents/MacOS/Evil")).unwrap().permissions().mode() & 0o7777, 0o755);
}

#[test]
fn zip_slip_with_dotdot_and_absolute_names_writes_nothing_outside() {
    let d = tmp("a1");
    let zp = d.join("a.zip");
    let abs = d.join("abs_escaped.txt");
    let mut z = Z::new(&zp);
    z.file("../escaped.txt", b"pwned", 0o644);
    z.file(abs.to_str().unwrap(), b"pwned", 0o644);
    z.file("ok.txt", b"fine", 0o644);
    z.done();
    let _ = unpack_zip_limited(&zp, &d.join("out"), &AtomicBool::new(false), Limits::PLUGIN);
    assert!(!d.join("escaped.txt").exists() && !abs.exists());
}

// ───────────── 3. packs ─────────────

#[test]
fn the_pack_limits_include_a_ratio_and_a_zip_bomb_is_stopped() {
    assert!(std::hint::black_box(Limits::PACK.ratio) > 0, "a pack may not expand without limit");
    let d = tmp("a6");
    let zp = d.join("a.zip");
    let mut z = Z::new(&zp);
    z.file("bomb.bin", &vec![0u8; 64 << 20], 0o644);
    z.done();
    let out = d.join("out");
    assert!(unpack(&zp, ArchiveFormat::Zip, &out, &AtomicBool::new(false)).is_err());
    assert!(fs::metadata(out.join("bomb.bin")).map_or(0, |m| m.len()) < (60 << 20));
}

#[test]
fn a_pack_without_a_pinned_hash_is_not_installable() {
    let pack = |sha: &str| BundledPack {
        id: "p".into(),
        name: "P".into(),
        redistributable: true,
        download_url: "https://example.org/p.zip".into(),
        sha256: sha.into(),
        ..Default::default()
    };
    assert!(!can_install(&pack("")), "an unpinned pack would be installed unverified");
    assert!(!can_install(&pack("abc")));
    assert!(can_install(&pack(&"a".repeat(64))));
}
