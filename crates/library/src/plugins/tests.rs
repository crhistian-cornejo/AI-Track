//! Installer tests. No test touches the network: a throw-away HTTP server on
//! 127.0.0.1 plays the maker's server, and the archives are made on the spot.

use super::*;
use std::io::{BufRead, BufReader as BR};
use std::net::TcpListener;
use std::sync::Mutex;

enum Route {
    Body(Vec<u8>),
    Redirect(String),
    /// Endless body without a length.
    Endless,
}

struct Server {
    port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

fn serve(routes: Vec<(&'static str, Route)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let log = hits.clone();
    let routes: Arc<Vec<(&'static str, Route)>> = Arc::new(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (routes, log) = (routes.clone(), log.clone());
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut first = String::new();
                let mut r = BR::new(stream.try_clone().unwrap());
                r.read_line(&mut first).unwrap();
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(path.clone());
                match routes.iter().find(|(p, _)| *p == path) {
                    Some((_, Route::Body(b))) => {
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len());
                        let _ = stream.write_all(b);
                    }
                    Some((_, Route::Redirect(to))) => {
                        let _ = write!(stream, "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    Some((_, Route::Endless)) => {
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
                        let chunk = vec![7u8; 64 * 1024];
                        while stream.write_all(&chunk).is_ok() {}
                    }
                    None => {
                        let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            });
        }
    });
    Server { port, hits }
}

fn zip_bytes(f: impl FnOnce(&mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    f(&mut z);
    z.finish().unwrap().into_inner()
}

/// A zip with one bundle: an executable, a resource and a version link.
fn bundle_zip(name: &str, payload: &[u8]) -> Vec<u8> {
    zip_bytes(|z| {
        let exe = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        let plain = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        z.start_file(format!("Pack/{name}.vst3/Contents/MacOS/{name}"), exe).unwrap();
        z.write_all(payload).unwrap();
        z.start_file(format!("Pack/{name}.vst3/Contents/Resources/readme.txt"), plain).unwrap();
        z.write_all(b"hello").unwrap();
        z.add_symlink(format!("Pack/{name}.vst3/Contents/Resources/latest"), "readme.txt", plain).unwrap();
        z.start_file("Pack/Manual.pdf", plain).unwrap();
        z.write_all(b"%PDF").unwrap();
    })
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

const CATALOG: &str = r#"
[[plugin]]
id = "demo"
name = "Demo"
vendor = "Maker"
category = "dynamics"
version = "1.0"
homepage = "https://maker.example/demo"
license = "Proprietary-Freeware"
license_notice = "Free."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["Demo"]
allowed_hosts = ["maker.example", "127.0.0.1"]
[[plugin.artifact]]
os = "macos"
url = "https://maker.example/demo.zip"
sha256 = ""
size = 0
"#;

struct Rig {
    _tmp: tempfile::TempDir,
    inst: PluginInstaller,
    dirs: UserDirs,
    data: PathBuf,
}

/// An installer whose catalog entry points at `url` with the given pin.
fn rig(url: &str, pin: Option<(&str, u64)>, version: &str) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = UserDirs { vst3: tmp.path().join("VST3"), clap: tmp.path().join("CLAP"), system: vec![] };
    let data = tmp.path().join("data");
    let mut cat = Catalog::parse(CATALOG).unwrap();
    {
        let e = &mut cat.plugin[0];
        e.version = version.into();
        e.artifact[0].url = url.into();
        if let Some((h, s)) = pin {
            e.artifact[0].sha256 = h.into();
            e.artifact[0].size = s;
        }
    }
    let inst = PluginInstaller::with(cat, data.clone(), dirs.clone(), Some(Os::Macos), Arc::new(HttpFetcher::for_tests()));
    Rig { _tmp: tmp, inst, dirs, data }
}

fn finish(inst: &mut PluginInstaller) {
    let t0 = Instant::now();
    while inst.busy() && t0.elapsed() < Duration::from_secs(60) {
        inst.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    inst.poll();
}

fn state(inst: &PluginInstaller) -> PluginState {
    inst.cards().remove(0).state
}

#[test]
fn only_https_and_the_makers_domains_pass_the_policy() {
    let p = Policy::default();
    let hosts = vec!["maker.example".to_owned()];
    assert!(p.check("https://maker.example/a.zip", &hosts).is_ok());
    assert!(p.check("http://maker.example/a.zip", &hosts).is_err());
    assert!(p.check("https://evil.example/a.zip", &hosts).is_err());
    assert!(p.check("https://maker.example.evil.example/a.zip", &hosts).is_err());
    assert!(p.check("https://maker.example@evil.example/a.zip", &hosts).is_err());
    assert!(p.check("http://127.0.0.1/a.zip", &["127.0.0.1".to_owned()]).is_err(), "loopback http is for tests only");
    assert!(Policy::for_tests().check("http://127.0.0.1:99/a", &["127.0.0.1".to_owned()]).is_ok());
    assert!(p.check("https://maker.example/a.zip", &[]).is_err(), "no domains, no download");
}

#[test]
fn a_redirect_to_another_domain_is_not_followed() {
    let other = serve(vec![("/evil.zip", Route::Body(b"evil".to_vec()))]);
    let s = serve(vec![("/a.zip", Route::Redirect(format!("http://localhost:{}/evil.zip", other.port))), ("/ok.zip", Route::Body(b"fine".to_vec())), ("/hop.zip", Route::Redirect("/ok.zip".into()))]);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("f");
    let hosts = vec!["127.0.0.1".to_owned()];
    let f = HttpFetcher::for_tests();
    let stop = AtomicBool::new(false);
    let e = f.fetch(&s.url("/a.zip"), &hosts, 1000, &dest, &stop, &mut |_, _| {}).unwrap_err();
    assert!(e.to_string().contains("not one of the maker's domains"), "{e}");
    assert!(other.hits.lock().unwrap().is_empty(), "the other server was never asked");
    // A redirect inside the allowed domain is followed.
    f.fetch(&s.url("/hop.zip"), &hosts, 1000, &dest, &stop, &mut |_, _| {}).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"fine");
}

#[test]
fn a_download_over_the_size_the_catalog_says_is_stopped() {
    let s = serve(vec![("/big", Route::Endless), ("/sized", Route::Body(vec![1; 5000]))]);
    let tmp = tempfile::tempdir().unwrap();
    let hosts = vec!["127.0.0.1".to_owned()];
    let f = HttpFetcher::for_tests();
    let stop = AtomicBool::new(false);
    let t0 = Instant::now();
    let e = f.fetch(&s.url("/big"), &hosts, 300_000, &tmp.path().join("a"), &stop, &mut |_, _| {}).unwrap_err();
    assert!(e.to_string().contains("larger"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(10));
    let e = f.fetch(&s.url("/sized"), &hosts, 100, &tmp.path().join("b"), &stop, &mut |_, _| {}).unwrap_err();
    assert!(e.to_string().contains("larger"), "the declared length is enough to refuse: {e}");
    let stop = AtomicBool::new(true);
    assert!(f.fetch(&s.url("/sized"), &hosts, 10_000, &tmp.path().join("c"), &stop, &mut |_, _| {}).is_err(), "cancelled");
}

#[test]
fn an_unpinned_plugin_cannot_be_installed_and_asks_for_nothing() {
    let s = serve(vec![]);
    let mut r = rig(&s.url("/demo.zip"), None, "1.0");
    assert_eq!(state(&r.inst), PluginState::Guided(GuidedWhy::Unpinned));
    assert!(r.inst.start("demo").is_err());
    assert!(s.hits.lock().unwrap().is_empty(), "nothing was requested");
}

#[test]
fn a_pinned_zip_installs_with_permissions_links_and_a_receipt_then_uninstalls() {
    let zip = bundle_zip("Demo", b"\xcf\xfa\xed\xfe");
    let s = serve(vec![("/demo.zip", Route::Body(zip.clone()))]);
    let mut r = rig(&s.url("/demo.zip"), Some((&sha(&zip), zip.len() as u64)), "1.0");
    assert_eq!(state(&r.inst), PluginState::Available);
    r.inst.start("demo").unwrap();
    assert!(matches!(state(&r.inst), PluginState::Installing { .. }));
    finish(&mut r.inst);
    let bundle = r.dirs.vst3.join("Demo.vst3");
    assert!(bundle.join("Contents/Resources/readme.txt").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(bundle.join("Contents/MacOS/Demo")).unwrap().permissions().mode() & 0o111, 0o111, "executable");
        assert_eq!(fs::read_link(bundle.join("Contents/Resources/latest")).unwrap(), Path::new("readme.txt"));
    }
    assert!(!r.dirs.vst3.join("Manual.pdf").exists() && !r.dirs.vst3.join("Pack").exists(), "only the bundle is installed");
    let receipt = read_receipt(&r.data, "demo").unwrap();
    assert_eq!(receipt.version, "1.0");
    assert_eq!(receipt.sha256, sha(&zip));
    assert_eq!(receipt.files.len(), 1);
    assert_eq!(receipt.files[0].path, bundle);
    assert_eq!(receipt.files[0].tree_sha256, tree_hash(&bundle).unwrap());
    assert!(!receipt.signature.is_empty());
    assert_eq!(state(&r.inst), PluginState::Installed { version: "1.0".into() });
    assert_eq!(r.inst.take_installed(), vec![bundle.clone()], "the host is told which paths to scan");
    assert!(r.inst.take_installed().is_empty());
    assert!(!r.data.join("plugins/work/demo").exists(), "the work folder is gone");
    // Uninstall removes what the receipt lists and the receipt.
    assert_eq!(r.inst.uninstall("demo").unwrap(), 1);
    assert!(!bundle.exists() && read_receipt(&r.data, "demo").is_none());
    assert_eq!(state(&r.inst), PluginState::Available);
}

#[test]
fn a_changed_file_fails_the_hash_and_nothing_is_installed() {
    let zip = bundle_zip("Demo", b"real");
    let tampered = bundle_zip("Demo", b"evil");
    let s = serve(vec![("/demo.zip", Route::Body(tampered.clone()))]);
    // Same size as the pinned one is not enough.
    let mut r = rig(&s.url("/demo.zip"), Some((&sha(&zip), tampered.len() as u64)), "1.0");
    r.inst.start("demo").unwrap();
    finish(&mut r.inst);
    let card = r.inst.cards().remove(0);
    assert!(card.error.as_deref().is_some_and(|e| e.contains("sha256 differs")), "{:?}", card.error);
    assert!(!r.dirs.vst3.join("Demo.vst3").exists());
    assert!(read_receipt(&r.data, "demo").is_none());
    assert!(!r.data.join("plugins/work/demo").exists());
}

#[test]
fn a_bundle_the_user_already_had_is_left_alone() {
    let zip = bundle_zip("Demo", b"new");
    let s = serve(vec![("/demo.zip", Route::Body(zip.clone()))]);
    let mut r = rig(&s.url("/demo.zip"), Some((&sha(&zip), zip.len() as u64)), "1.0");
    let theirs = r.dirs.vst3.join("Demo.vst3/Contents");
    fs::create_dir_all(&theirs).unwrap();
    fs::write(theirs.join("mine.txt"), b"keep").unwrap();
    assert_eq!(state(&r.inst), PluginState::External, "found outside the app");
    r.inst.start("demo").unwrap();
    finish(&mut r.inst);
    assert_eq!(fs::read(theirs.join("mine.txt")).unwrap(), b"keep");
    assert!(r.inst.cards().remove(0).error.is_some_and(|e| e.contains("not installed by AI-Track")));
    assert!(r.inst.uninstall("demo").is_err(), "no receipt, so nothing is deleted");
    assert!(theirs.join("mine.txt").exists());
}

#[test]
fn a_new_version_replaces_the_old_one_and_the_old_bundle_does_not_linger() {
    let v1 = bundle_zip("Demo", b"v1");
    let v2 = bundle_zip("Demo", b"v2!");
    let s = serve(vec![("/demo.zip", Route::Body(v1.clone())), ("/demo2.zip", Route::Body(v2.clone()))]);
    let mut r = rig(&s.url("/demo.zip"), Some((&sha(&v1), v1.len() as u64)), "1.0");
    r.inst.start("demo").unwrap();
    finish(&mut r.inst);
    // The maker publishes 2.0: the catalog moves, the installed one is older.
    let mut cat = Catalog::parse(CATALOG).unwrap();
    cat.plugin[0].version = "2.0".into();
    cat.plugin[0].artifact[0].url = s.url("/demo2.zip");
    cat.plugin[0].artifact[0].sha256 = sha(&v2);
    cat.plugin[0].artifact[0].size = v2.len() as u64;
    let mut inst = PluginInstaller::with(cat, r.data.clone(), r.dirs.clone(), Some(Os::Macos), Arc::new(HttpFetcher::for_tests()));
    assert_eq!(state(&inst), PluginState::Update { installed: "1.0".into() });
    inst.start("demo").unwrap();
    finish(&mut inst);
    assert_eq!(state(&inst), PluginState::Installed { version: "2.0".into() });
    assert_eq!(fs::read(r.dirs.vst3.join("Demo.vst3/Contents/MacOS/Demo")).unwrap(), b"v2!");
    let leftovers: Vec<_> = fs::read_dir(&r.dirs.vst3).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(leftovers, vec!["Demo.vst3".to_owned()], "no backup left behind: {leftovers:?}");
}

#[test]
fn uninstall_refuses_a_receipt_that_points_outside_the_plugin_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = UserDirs { vst3: tmp.path().join("VST3"), clap: tmp.path().join("CLAP"), system: vec![] };
    let data = tmp.path().join("data");
    let victim = tmp.path().join("Documents");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("thesis.txt"), b"precious").unwrap();
    for path in [victim.clone(), tmp.path().join("VST3/../Documents"), dirs.vst3.join("sub/Deep.vst3"), PathBuf::from("/")] {
        let forged = Receipt { id: "demo".into(), version: "1".into(), source_url: String::new(), sha256: String::new(), installed_at: 0, files: vec![ReceiptFile { path, tree_sha256: String::new() }], signature: vec![] };
        write_receipt(&data, &forged).unwrap();
        assert!(uninstall_plugin(&data, &dirs, "demo").is_err());
    }
    assert!(victim.join("thesis.txt").exists());
}

#[test]
fn an_archive_with_only_an_installer_program_is_not_installed() {
    let zip = zip_bytes(|z| {
        z.start_file("Setup.exe", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"MZ").unwrap();
    });
    let s = serve(vec![("/demo.zip", Route::Body(zip.clone()))]);
    let mut r = rig(&s.url("/demo.zip"), Some((&sha(&zip), zip.len() as u64)), "1.0");
    r.inst.start("demo").unwrap();
    finish(&mut r.inst);
    assert!(r.inst.cards().remove(0).error.is_some_and(|e| e.contains("without running its installer")));
    assert!(!r.dirs.vst3.exists() || fs::read_dir(&r.dirs.vst3).unwrap().next().is_none());
}

#[test]
fn a_zip_that_expands_too_far_is_refused_before_anything_is_installed() {
    let bomb = zip_bytes(|z| {
        z.start_file("Demo.vst3/Contents/MacOS/Demo", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
        z.write_all(&vec![0u8; 40 << 20]).unwrap();
    });
    assert!(bomb.len() < (40 << 20) / 100, "the test archive really is a 100:1 bomb ({} bytes)", bomb.len());
    let s = serve(vec![("/demo.zip", Route::Body(bomb.clone()))]);
    let mut r = rig(&s.url("/demo.zip"), Some((&sha(&bomb), bomb.len() as u64)), "1.0");
    r.inst.start("demo").unwrap();
    finish(&mut r.inst);
    assert!(r.inst.cards().remove(0).error.is_some_and(|e| e.contains("zip bomb")));
    assert!(!r.dirs.vst3.join("Demo.vst3").exists());
}

#[test]
fn a_link_entry_and_a_missing_platform_are_guided_not_installable() {
    let cat = Catalog::bundled();
    let inst = PluginInstaller::with(cat.clone(), tempfile::tempdir().unwrap().path().to_owned(), UserDirs { vst3: "/nonexistent/v".into(), clap: "/nonexistent/c".into(), system: vec![] }, Some(Os::Windows), Arc::new(HttpFetcher::default()));
    let cards = inst.cards();
    // Whatever is pinned in the shipped catalog, only a pinned artifact for this system makes a plugin installable.
    for c in &cards {
        assert_eq!(matches!(c.state, PluginState::Available), c.entry.installable(Os::Windows), "{}", c.entry.id);
    }
    let nova = cards.iter().find(|c| c.entry.id == "tdr-nova").unwrap();
    assert!(matches!(nova.state, PluginState::Guided(GuidedWhy::Unpinned) | PluginState::Available), "{:?}", nova.state);
    let vital = cards.iter().find(|c| c.entry.id == "vital").unwrap();
    assert_eq!(vital.state, PluginState::Guided(GuidedWhy::Link));
    let noisemaker = cards.iter().find(|c| c.entry.id == "tal-noisemaker").unwrap();
    assert_eq!(noisemaker.state, PluginState::Guided(GuidedWhy::NoBuild), "only a macOS build is offered");
    let sforzando = cards.iter().find(|c| c.entry.id == "sforzando").unwrap();
    assert_eq!(sforzando.state, PluginState::Guided(GuidedWhy::Link), "it is on a shared S3 host, so a guided link");
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    fn tool_ok(program: &str, args: &[&str]) -> bool {
        Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    fn fake_bundle(root: &Path, name: &str) -> PathBuf {
        let b = root.join(format!("{name}.vst3/Contents/MacOS"));
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join(name), b"binary").unwrap();
        root.join(format!("{name}.vst3"))
    }

    /// A disk image is mounted read-only, the bundle copied out, and the image detached.
    #[test]
    fn a_dmg_gives_up_its_bundle_without_running_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fake_bundle(&src, "Demo");
        fs::write(src.join("Installer.command"), b"#!/bin/sh\ntouch /tmp/aitrack-test-must-not-exist\n").unwrap();
        let dmg = tmp.path().join("demo.dmg");
        if !tool_ok("hdiutil", &["create", "-srcfolder", src.to_str().unwrap(), "-volname", "DemoVol", "-format", "UDZO", "-quiet", dmg.to_str().unwrap()]) {
            eprintln!("hdiutil cannot make an image here: not verified");
            return;
        }
        let work = tmp.path().join("work");
        let bundles = match open_container(&dmg, Container::Dmg, &work, &work.join("raw"), &AtomicBool::new(false)) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("could not mount here ({e}): not verified");
                return;
            }
        };
        assert_eq!(bundles.len(), 1);
        assert_eq!(fs::read(bundles[0].join("Contents/MacOS/Demo")).unwrap(), b"binary");
        assert!(!Path::new("/tmp/aitrack-test-must-not-exist").exists());
        let mounted = Command::new("mount").output().unwrap();
        assert!(!String::from_utf8_lossy(&mounted.stdout).contains("DemoVol"), "the image is detached again");
    }

    /// A pkg is expanded; its postinstall script is never run.
    #[test]
    fn a_pkg_is_expanded_and_its_script_never_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root/Library/Audio/Plug-Ins/VST3");
        fs::create_dir_all(&root).unwrap();
        fake_bundle(&root, "Demo");
        let scripts = tmp.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        let marker = tmp.path().join("script-ran");
        fs::write(scripts.join("postinstall"), format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(scripts.join("postinstall"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pkg = tmp.path().join("demo.pkg");
        if !tool_ok("pkgbuild", &["--root", tmp.path().join("root").to_str().unwrap(), "--scripts", scripts.to_str().unwrap(), "--identifier", "test.demo", "--version", "1", "--install-location", "/", pkg.to_str().unwrap()]) {
            eprintln!("pkgbuild is not available: not verified");
            return;
        }
        let work = tmp.path().join("work");
        let bundles = open_container(&pkg, Container::Pkg, &work, &work.join("raw"), &AtomicBool::new(false)).unwrap();
        assert_eq!(bundles.len(), 1, "{bundles:?}");
        assert_eq!(bundles[0].file_name().unwrap(), "Demo.vst3");
        assert!(!marker.exists(), "the postinstall script must never run");
    }

    /// A zip holding the maker's pkg is opened one level deep.
    #[test]
    fn a_zip_that_holds_a_pkg_is_opened_too() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root/Library/Audio/Plug-Ins/VST3");
        fs::create_dir_all(&root).unwrap();
        fake_bundle(&root, "Inner");
        let pkg = tmp.path().join("inner.pkg");
        if !tool_ok("pkgbuild", &["--root", tmp.path().join("root").to_str().unwrap(), "--identifier", "test.inner", "--version", "1", "--install-location", "/", pkg.to_str().unwrap()]) {
            eprintln!("pkgbuild is not available: not verified");
            return;
        }
        let bytes = zip_bytes(|z| {
            z.start_file("Installer/inner.pkg", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(&fs::read(&pkg).unwrap()).unwrap();
        });
        let archive = tmp.path().join("a.zip");
        fs::write(&archive, bytes).unwrap();
        let work = tmp.path().join("work");
        let bundles = open_container(&archive, Container::Zip, &work, &work.join("raw"), &AtomicBool::new(false)).unwrap();
        assert_eq!(bundles.len(), 1, "{bundles:?}");
    }

    #[test]
    fn quarantine_is_removed_and_an_unsigned_bundle_is_signed_or_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let b = fake_bundle(tmp.path(), "Q");
        let _ = Command::new("xattr").args(["-w", "com.apple.quarantine", "0081;00000000;Test;"]).arg(&b).status();
        let note = check_signature(&b, "", &AtomicBool::new(false)).unwrap();
        assert!(note.contains("unsigned") || note.contains("signed"), "{note}");
        let left = Command::new("xattr").arg("-l").arg(&b).output().unwrap();
        assert!(!String::from_utf8_lossy(&left.stdout).contains("quarantine"));
    }
}
