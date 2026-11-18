//! Installing a downloaded, verified update.
//!
//! macOS: the `.app.tar.gz` is unpacked next to the running bundle (same volume,
//! so the swap is two renames), the bundle becomes `AI-Track.app.old`, the new one
//! takes its place, and the app is relaunched with `open -n`. The `.old` bundle is
//! deleted by the next start. Windows: the NSIS installer is started with
//! `/S /RELAUNCH` and the app quits; the installer replaces the files and reopens it.
//!
//! A step that fails undoes what it did: the installed version never ends up half
//! replaced, and the temporary folders are removed.

use crate::UpdateError;
use crate::version::Version;
use flate2::read::GzDecoder;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// How the app starts a process; replaced in tests so nothing is launched.
pub trait Launcher {
    /// Open a new instance of the app bundle (macOS: `open -n <bundle>`).
    fn relaunch(&self, bundle: &Path) -> Result<(), String>;
    /// Start the installer detached and silent (Windows); with `relaunch` it reopens the app when done.
    fn run_installer(&self, setup: &Path, relaunch: bool) -> Result<(), String>;
}

pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn relaunch(&self, bundle: &Path) -> Result<(), String> {
        Command::new("open").arg("-n").arg(bundle).spawn().map(|_| ()).map_err(|e| e.to_string())
    }

    #[cfg(windows)]
    fn run_installer(&self, setup: &Path, relaunch: bool) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut cmd = Command::new(setup);
        cmd.arg("/S");
        if relaunch {
            cmd.arg("/RELAUNCH");
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn().map(|_| ()).map_err(|e| e.to_string())
    }

    #[cfg(not(windows))]
    fn run_installer(&self, _: &Path, _: bool) -> Result<(), String> {
        Err("the installer is for Windows".into())
    }
}

/// `…/AI-Track.app` for the executable `…/AI-Track.app/Contents/MacOS/aitrack`.
pub fn bundle_from_exe(exe: &Path) -> Result<PathBuf, UpdateError> {
    let not_bundled = || UpdateError::NotBundled(exe.to_path_buf());
    let macos = exe.parent().ok_or_else(not_bundled)?;
    let contents = macos.parent().ok_or_else(not_bundled)?;
    let bundle = contents.parent().ok_or_else(not_bundled)?;
    if macos.file_name() == Some(OsStr::new("MacOS")) && contents.file_name() == Some(OsStr::new("Contents")) && bundle.extension() == Some(OsStr::new("app")) {
        Ok(bundle.to_path_buf())
    } else {
        Err(not_bundled())
    }
}

fn old_path(bundle: &Path) -> PathBuf {
    let mut name: OsString = bundle.file_name().unwrap_or_default().to_owned();
    name.push(".old");
    bundle.with_file_name(name)
}

const STAGING_PREFIX: &str = ".AI-Track-update-";

fn unwritable(dir: &Path, e: &std::io::Error) -> Option<UpdateError> {
    matches!(e.kind(), ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem).then(|| UpdateError::NotWritable(dir.to_path_buf()))
}

/// Install `archive` (the verified download) over the running app. On success the
/// caller quits; with `relaunch` the new version is already starting (an update
/// installed because the user is closing the app does not reopen it).
/// `exe` is `current_exe()`.
pub fn install_downloaded(archive: &Path, exe: &Path, launcher: &dyn Launcher, relaunch: bool) -> Result<(), UpdateError> {
    if !crate::key::CONFIGURED {
        return Err(UpdateError::Unconfigured);
    }
    if cfg!(target_os = "macos") {
        install_macos(archive, exe, launcher, relaunch)
    } else if cfg!(target_os = "windows") {
        install_windows(archive, launcher, relaunch)
    } else {
        Err(UpdateError::Unsupported)
    }
}

pub fn install_windows(setup: &Path, launcher: &dyn Launcher, relaunch: bool) -> Result<(), UpdateError> {
    if !setup.is_file() {
        return Err(UpdateError::io("starting the installer", format!("{} is missing", setup.display())));
    }
    launcher.run_installer(setup, relaunch).map_err(|e| UpdateError::io("starting the installer", e))
}

pub fn install_macos(archive: &Path, exe: &Path, launcher: &dyn Launcher, relaunch: bool) -> Result<(), UpdateError> {
    let bundle = bundle_from_exe(exe)?;
    let exe_name = exe.file_name().ok_or_else(|| UpdateError::NotBundled(exe.to_path_buf()))?;
    let parent = bundle.parent().ok_or_else(|| UpdateError::NotBundled(exe.to_path_buf()))?;
    let staging = parent.join(format!("{STAGING_PREFIX}{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir(&staging).map_err(|e| unwritable(parent, &e).unwrap_or_else(|| UpdateError::io(format!("creating {}", staging.display()), &e)))?;
    let result = (|| {
        let new_app = extract_app(archive, &staging, exe_name)?;
        swap_bundle(&bundle, &new_app)
    })();
    let _ = fs::remove_dir_all(&staging);
    result?;
    if !relaunch {
        return Ok(());
    }
    launcher.relaunch(&bundle).map_err(UpdateError::Relaunch)
}

/// Unpack the archive into `staging` and return the `.app` inside. The archive must
/// hold exactly one root folder, `<name>.app`, with the executable `exe_name` in it.
pub fn extract_app(archive: &Path, staging: &Path, exe_name: &OsStr) -> Result<PathBuf, UpdateError> {
    let bad = |m: String| UpdateError::BadArchive(m);
    let file = File::open(archive).map_err(|e| UpdateError::io(format!("opening {}", archive.display()), e))?;
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    tar.set_unpack_xattrs(false);
    let mut root: Option<OsString> = None;
    for entry in tar.entries().map_err(|e| bad(e.to_string()))? {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        let path = entry.path().map_err(|e| bad(e.to_string()))?.into_owned();
        let mut parts = path.components().filter(|c| !matches!(c, Component::CurDir));
        let first = match parts.next() {
            Some(Component::Normal(n)) => n.to_owned(),
            Some(_) => return Err(bad(format!("unsafe path {}", path.display()))),
            None => continue,
        };
        // Resource-fork leftovers from bsdtar on macOS.
        if first.to_string_lossy().starts_with("._") {
            continue;
        }
        if parts.any(|c| !matches!(c, Component::Normal(_))) {
            return Err(bad(format!("unsafe path {}", path.display())));
        }
        match &root {
            None if Path::new(&first).extension() == Some(OsStr::new("app")) => root = Some(first),
            None => return Err(bad(format!("the archive starts with {}, not an .app", path.display()))),
            Some(r) if *r == first => {}
            Some(_) => return Err(bad("the archive has more than one root".into())),
        }
        if !entry.unpack_in(staging).map_err(|e| bad(format!("{}: {e}", path.display())))? {
            return Err(bad(format!("unsafe path {}", path.display())));
        }
    }
    let root = root.ok_or_else(|| bad("the archive is empty".into()))?;
    let app = staging.join(root);
    if !app.join("Contents/MacOS").join(exe_name).is_file() || !app.join("Contents/Info.plist").is_file() {
        return Err(bad(format!("the archive's app has no {}", Path::new("Contents/MacOS").join(exe_name).display())));
    }
    Ok(app)
}

/// `bundle` → `bundle.old`, `new_app` → `bundle`; the first move is undone if the second fails.
pub fn swap_bundle(bundle: &Path, new_app: &Path) -> Result<(), UpdateError> {
    let parent = bundle.parent().unwrap_or(Path::new("."));
    let old = old_path(bundle);
    if old.exists() {
        fs::remove_dir_all(&old).map_err(|e| unwritable(parent, &e).unwrap_or_else(|| UpdateError::io(format!("removing {}", old.display()), &e)))?;
    }
    fs::rename(bundle, &old).map_err(|e| unwritable(parent, &e).unwrap_or_else(|| UpdateError::io(format!("moving {} aside", bundle.display()), &e)))?;
    if let Err(e) = fs::rename(new_app, bundle) {
        // Put the running app back where it was.
        let restored = fs::rename(&old, bundle);
        let mut msg = e.to_string();
        if let Err(r) = restored {
            msg.push_str(&format!("; and the old app could not be put back ({r}), it is at {}", old.display()));
        }
        return Err(UpdateError::io(format!("moving the new app to {}", bundle.display()), msg));
    }
    Ok(())
}

/// At start-up: delete what the last update left next to the app (the `.old`
/// bundle, an abandoned staging folder). Does nothing outside a bundle.
pub fn cleanup_leftovers(exe: &Path) {
    let Ok(bundle) = bundle_from_exe(exe) else { return };
    let old = old_path(&bundle);
    if old.exists() {
        match fs::remove_dir_all(&old) {
            Ok(()) => log::info!("removed the previous version at {}", old.display()),
            Err(e) => log::warn!("could not remove {}: {e}", old.display()),
        }
    }
    let Some(parent) = bundle.parent() else { return };
    let Ok(entries) = fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(STAGING_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Delete the downloads of versions that are not newer than `current` (they were
/// installed, or are obsolete). Folders that are not versions are left alone.
pub fn prune_downloads(root: &Path, current: &str) {
    let Ok(current) = Version::parse(current) else { return };
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if Version::parse(&name.to_string_lossy()).is_ok_and(|v| v <= current) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::temp_dir;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeLauncher {
        calls: RefCell<Vec<String>>,
        fail: bool,
    }

    impl Launcher for FakeLauncher {
        fn relaunch(&self, bundle: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("open -n {}", bundle.display()));
            if self.fail { Err("no window server".into()) } else { Ok(()) }
        }

        fn run_installer(&self, setup: &Path, relaunch: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("{} /S{}", setup.display(), if relaunch { " /RELAUNCH" } else { "" }));
            Ok(())
        }
    }

    /// A fake installed bundle whose executable says `version`.
    fn fake_bundle(dir: &Path, version: &str) -> (PathBuf, PathBuf) {
        let bundle = dir.join("AI-Track.app");
        let macos = bundle.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("aitrack"), version).unwrap();
        fs::write(bundle.join("Contents/Info.plist"), "<plist/>").unwrap();
        (bundle.clone(), macos.join("aitrack"))
    }

    /// A `.app.tar.gz` of an app whose executable says `version`.
    fn archive(dir: &Path, root: &str, version: &str, with_exe: bool) -> PathBuf {
        let path = dir.join("update.app.tar.gz");
        let mut tar = tar::Builder::new(GzEncoder::new(File::create(&path).unwrap(), Compression::fast()));
        let mut add = |name: &str, data: &[u8], mode: u32| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            tar.append_data(&mut h, name, data).unwrap();
        };
        add(&format!("{root}/Contents/Info.plist"), b"<plist/>", 0o644);
        if with_exe {
            add(&format!("{root}/Contents/MacOS/aitrack"), version.as_bytes(), 0o755);
        }
        add(&format!("{root}/Contents/Resources/content/a.toml"), b"a", 0o644);
        tar.into_inner().unwrap().finish().unwrap();
        path
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn finds_the_bundle_from_the_executable() {
        assert_eq!(bundle_from_exe(Path::new("/Applications/AI-Track.app/Contents/MacOS/aitrack")).unwrap(), Path::new("/Applications/AI-Track.app"));
        for dev in ["/repo/target/debug/aitrack", "/Applications/AI-Track.app/Contents/aitrack", "/x/Contents/MacOS/aitrack", "aitrack"] {
            assert!(matches!(bundle_from_exe(Path::new(dev)), Err(UpdateError::NotBundled(_))), "{dev}");
        }
    }

    #[test]
    fn swaps_the_bundle_and_relaunches() {
        let dir = temp_dir("mac-swap");
        let (bundle, exe) = fake_bundle(&dir, "0.0.1");
        let tgz = archive(&dir, "AI-Track.app", "0.0.2", true);
        let launcher = FakeLauncher::default();
        install_macos(&tgz, &exe, &launcher, true).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.2");
        assert!(bundle.join("Contents/Resources/content/a.toml").is_file());
        assert_eq!(fs::read_to_string(dir.join("AI-Track.app.old/Contents/MacOS/aitrack")).unwrap(), "0.0.1");
        assert_eq!(*launcher.calls.borrow(), vec![format!("open -n {}", bundle.display())]);
        // No staging folder is left; only the app, its predecessor and the download.
        assert_eq!(leftovers(&dir), vec!["AI-Track.app", "AI-Track.app.old", "update.app.tar.gz"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&exe).unwrap().permissions().mode() & 0o111, 0o111, "the executable bit survives");
        }
        // The next start removes the previous version.
        cleanup_leftovers(&exe);
        assert_eq!(leftovers(&dir), vec!["AI-Track.app", "update.app.tar.gz"]);
    }

    #[test]
    fn a_second_update_replaces_a_stale_old_bundle() {
        let dir = temp_dir("mac-stale-old");
        let (_, exe) = fake_bundle(&dir, "0.0.1");
        fs::create_dir_all(dir.join("AI-Track.app.old/Contents")).unwrap();
        fs::write(dir.join("AI-Track.app.old/Contents/junk"), "x").unwrap();
        install_macos(&archive(&dir, "AI-Track.app", "0.0.2", true), &exe, &FakeLauncher::default(), true).unwrap();
        assert_eq!(fs::read_to_string(dir.join("AI-Track.app.old/Contents/MacOS/aitrack")).unwrap(), "0.0.1");
        assert!(!dir.join("AI-Track.app.old/Contents/junk").exists());
    }

    #[test]
    fn a_bad_archive_changes_nothing() {
        let dir = temp_dir("mac-bad");
        let (_, exe) = fake_bundle(&dir, "0.0.1");
        let cases = [
            ("no executable", archive(&dir, "AI-Track.app", "x", false)),
            ("not an app", archive(&dir, "Other", "x", true)),
            ("not an archive", {
                let p = dir.join("junk.tar.gz");
                fs::write(&p, b"not gzip at all").unwrap();
                p
            }),
        ];
        for (what, tgz) in cases {
            let launcher = FakeLauncher::default();
            let err = install_macos(&tgz, &exe, &launcher, true).unwrap_err();
            assert!(matches!(err, UpdateError::BadArchive(_)), "{what}: {err:?}");
            assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.1", "{what}");
            assert!(launcher.calls.borrow().is_empty(), "{what}");
            let names = leftovers(&dir);
            assert!(!names.iter().any(|n| n.starts_with(STAGING_PREFIX) || n.ends_with(".old")), "{what}: {names:?}");
        }
    }

    #[test]
    fn an_archive_cannot_write_outside_its_folder() {
        let dir = temp_dir("mac-traversal");
        let (_, exe) = fake_bundle(&dir, "0.0.1");
        let path = dir.join("evil.tar.gz");
        let mut tar = tar::Builder::new(GzEncoder::new(File::create(&path).unwrap(), Compression::fast()));
        let mut h = tar::Header::new_gnu();
        let name = b"AI-Track.app/../../escaped";
        h.as_old_mut().name[..name.len()].copy_from_slice(name);
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append(&h, &b"x"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let err = install_macos(&path, &exe, &FakeLauncher::default(), true).unwrap_err();
        assert!(matches!(err, UpdateError::BadArchive(_)), "{err:?}");
        assert!(!dir.parent().unwrap().join("escaped").exists());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.1");
    }

    #[test]
    fn dev_builds_refuse_to_install() {
        let dir = temp_dir("mac-dev");
        let exe = dir.join("target/debug/aitrack");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let tgz = archive(&dir, "AI-Track.app", "0.0.2", true);
        let err = install_macos(&tgz, &exe, &FakeLauncher::default(), true).unwrap_err();
        assert!(matches!(err, UpdateError::NotBundled(_)));
        assert!(err.to_string().contains("not running from an installed app"));
    }

    #[test]
    fn a_failed_swap_puts_the_old_app_back() {
        let dir = temp_dir("mac-rollback");
        let (bundle, exe) = fake_bundle(&dir, "0.0.1");
        // The new app is missing, so the second move fails after the first succeeded.
        let err = swap_bundle(&bundle, &dir.join("does-not-exist.app")).unwrap_err();
        assert!(matches!(err, UpdateError::Io { .. }), "{err:?}");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.1");
        assert!(!dir.join("AI-Track.app.old").exists());
    }

    #[test]
    fn a_failed_relaunch_is_reported_after_the_install() {
        let dir = temp_dir("mac-relaunch");
        let (_, exe) = fake_bundle(&dir, "0.0.1");
        let launcher = FakeLauncher { fail: true, ..Default::default() };
        let err = install_macos(&archive(&dir, "AI-Track.app", "0.0.2", true), &exe, &launcher, true).unwrap_err();
        assert_eq!(err, UpdateError::Relaunch("no window server".into()));
        assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.2", "the new version is in place");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_is_reported_as_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mac-readonly");
        let apps = dir.join("Applications");
        fs::create_dir(&apps).unwrap();
        let (_, exe) = fake_bundle(&apps, "0.0.1");
        let tgz = archive(&dir, "AI-Track.app", "0.0.2", true);
        fs::set_permissions(&apps, fs::Permissions::from_mode(0o555)).unwrap();
        let can_write_anyway = fs::create_dir(apps.join("probe")).is_ok();
        let result = install_macos(&tgz, &exe, &FakeLauncher::default(), true);
        fs::set_permissions(&apps, fs::Permissions::from_mode(0o755)).unwrap();
        if !can_write_anyway {
            assert_eq!(result, Err(UpdateError::NotWritable(apps.clone())));
            assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.1");
        }
    }

    #[test]
    fn windows_runs_the_installer_silently() {
        let dir = temp_dir("win");
        let setup = dir.join("AI-Track-0.0.2-windows-x86_64-setup.exe");
        let launcher = FakeLauncher::default();
        assert!(install_windows(&setup, &launcher, true).is_err(), "a missing installer is an error");
        fs::write(&setup, b"MZ").unwrap();
        install_windows(&setup, &launcher, true).unwrap();
        assert_eq!(*launcher.calls.borrow(), vec![format!("{} /S /RELAUNCH", setup.display())]);
        // Closing the app for good: the installer does not reopen it.
        let quiet = FakeLauncher::default();
        install_windows(&setup, &quiet, false).unwrap();
        assert_eq!(*quiet.calls.borrow(), vec![format!("{} /S", setup.display())]);
    }

    #[test]
    fn installing_on_quit_does_not_reopen_the_app() {
        let dir = temp_dir("mac-quit");
        let (_, exe) = fake_bundle(&dir, "0.0.1");
        let launcher = FakeLauncher::default();
        install_macos(&archive(&dir, "AI-Track.app", "0.0.2", true), &exe, &launcher, false).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "0.0.2");
        assert!(launcher.calls.borrow().is_empty());
    }

    #[test]
    fn obsolete_downloads_are_pruned() {
        let root = temp_dir("prune");
        for v in ["0.0.1", "0.0.2", "0.0.10", "notes"] {
            fs::create_dir_all(root.join(v)).unwrap();
        }
        prune_downloads(&root, "0.0.2");
        assert_eq!(leftovers(&root), vec!["0.0.10", "notes"]);
    }
}
