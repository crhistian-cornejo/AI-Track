//! Scanner: out-of-process scanning, incremental cache, blocklist,
//! moduleinfo.json, timeout.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use aitrack_host::PluginFormat;
use aitrack_host::scan::{BlockReason, Scanner};
use common::fixture;

fn temp(name: &str) -> PathBuf {
    let d = fixture().dir.parent().unwrap().join(format!("tmp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn scanner(cache: &std::path::Path) -> Scanner {
    Scanner::new(cache).with_scanner_exe(env!("CARGO_BIN_EXE_aitrack-plugin-scanner")).without_default_paths()
}

#[test]
fn scans_out_of_process_then_uses_cache() {
    let f = fixture();
    let tmp = temp("cache");
    let cache = tmp.join("plugins.json");
    let mut s = scanner(&cache);
    s.add_search_path(PluginFormat::Clap, &f.dir);
    s.add_search_path(PluginFormat::Vst3, &f.dir);
    let r = s.scan().unwrap();
    assert_eq!(r.files, 2, "{r:?}");
    assert_eq!(r.scanned, 2, "{r:?}");
    assert!(r.failed.is_empty(), "{r:?}");
    let plugins = s.plugins();
    assert_eq!(plugins.len(), 2);
    for p in &plugins {
        assert_eq!(p.name, "AI-Track Test Gain");
        assert!(p.probed);
        assert_eq!(p.params, 1);
        assert_eq!((p.inputs, p.outputs), (2, 2));
    }

    // A new scanner instance reads the persisted cache: nothing reloaded.
    let mut again = scanner(&cache);
    again.add_search_path(PluginFormat::Clap, &f.dir);
    again.add_search_path(PluginFormat::Vst3, &f.dir);
    let r = again.scan().unwrap();
    assert_eq!((r.cached, r.scanned), (2, 0), "{r:?}");
    assert_eq!(again.plugins().len(), 2);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn broken_plugin_is_blocklisted_persistently() {
    let tmp = temp("broken");
    let plugins = tmp.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let broken = plugins.join("Broken.clap");
    std::fs::write(&broken, b"this is not a library").unwrap();
    let cache = tmp.join("plugins.json");

    let mut s = scanner(&cache);
    s.add_search_path(PluginFormat::Clap, &plugins);
    let r = s.scan().unwrap();
    assert_eq!(r.failed.len(), 1, "{r:?}");
    let (path, entry) = s.blocklist().next().expect("blocklisted");
    assert_eq!(path, &broken);
    assert_eq!(entry.reason, BlockReason::Failed);

    let mut again = scanner(&cache);
    again.add_search_path(PluginFormat::Clap, &plugins);
    let r = again.scan().unwrap();
    assert_eq!(r.skipped_blocked, 1, "persisted blocklist: {r:?}");
    assert!(again.unblock(&broken));
    let r = again.scan().unwrap();
    assert_eq!(r.failed.len(), 1, "rescanned after unblock: {r:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn timeout_kills_scanner_and_blocklists() {
    let f = fixture();
    let tmp = temp("timeout");
    let mut s = scanner(&tmp.join("plugins.json")).with_timeout(Duration::from_millis(1));
    s.add_search_path(PluginFormat::Clap, &f.dir);
    let r = s.scan().unwrap();
    // 1 ms is shorter than process start-up.
    assert_eq!(r.failed.len(), 1, "{r:?}");
    assert_eq!(s.blocklist().next().unwrap().1.reason, BlockReason::Timeout);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn moduleinfo_avoids_loading() {
    let tmp = temp("moduleinfo");
    let bundle = tmp.join("plugins").join("Fake.vst3");
    let res = bundle.join("Contents").join("Resources");
    std::fs::create_dir_all(&res).unwrap();
    std::fs::write(
        res.join("moduleinfo.json"),
        r#"{ "Name": "Fake", "Factory Info": { "Vendor": "Nobody" },
             "Classes": [ { "CID": "00112233445566778899AABBCCDDEEFF", "Category": "Audio Module Class",
                            "Name": "Fake Comp", "Version": "2.0", "Sub Categories": ["Fx", "Dynamics"], } ] }"#,
    )
    .unwrap();
    let mut s = scanner(&tmp.join("plugins.json"));
    s.add_search_path(PluginFormat::Vst3, tmp.join("plugins"));
    let r = s.scan().unwrap();
    assert_eq!(r.from_moduleinfo, 1, "{r:?}");
    assert_eq!(r.scanned, 0);
    let p = &s.plugins()[0];
    assert_eq!((p.name.as_str(), p.vendor.as_str(), p.category.as_str()), ("Fake Comp", "Nobody", "Fx|Dynamics"));
    assert!(!p.probed);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_broken_plugin_is_listed_with_its_reason_and_survives_a_restart() {
    use aitrack_host::scan::ProblemKind;
    let f = fixture();
    let tmp = temp("problems");
    let plugins = tmp.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(plugins.join("Broken.clap"), b"this is not a library").unwrap();
    // A bundle with nothing inside.
    std::fs::create_dir_all(plugins.join("Empty.vst3")).unwrap();
    let cache = tmp.join("plugins.json");

    let mut s = scanner(&cache);
    s.add_search_path(PluginFormat::Clap, &plugins);
    s.add_search_path(PluginFormat::Clap, &f.dir);
    s.add_search_path(PluginFormat::Vst3, &plugins);
    s.scan().unwrap();
    let p = s.problems();
    assert_eq!(p.len(), 2, "{p:?}");
    let broken = p.iter().find(|x| x.path.ends_with("Broken.clap")).unwrap();
    assert_eq!(broken.kind, ProblemKind::Failed);
    assert!(!broken.message.is_empty());
    let empty = p.iter().find(|x| x.path.ends_with("Empty.vst3")).unwrap();
    assert_eq!(empty.kind, ProblemKind::Failed, "it does not just vanish");
    assert!(!empty.message.is_empty());
    assert_eq!(s.plugins().len(), 1, "the good one is still listed");

    // The blocked one is remembered across runs; both are shown again.
    let mut again = scanner(&cache);
    again.add_search_path(PluginFormat::Clap, &plugins);
    assert!(again.problems().iter().any(|x| x.path.ends_with("Broken.clap")));
    // Retrying a file that is still broken says why again.
    let still = again.retry(PluginFormat::Clap, &plugins.join("Broken.clap")).expect("still broken");
    assert_eq!(still.kind, ProblemKind::Failed);
    // Once the file is fixed, a retry clears it.
    fn copy_all(from: &std::path::Path, to: &std::path::Path) {
        if from.is_dir() {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap().flatten() {
                copy_all(&e.path(), &to.join(e.file_name()));
            }
        } else {
            std::fs::copy(from, to).unwrap();
        }
    }
    std::fs::remove_file(plugins.join("Broken.clap")).unwrap();
    copy_all(&f.clap, &plugins.join("Broken.clap"));
    assert!(again.retry(PluginFormat::Clap, &plugins.join("Broken.clap")).is_none());
    assert!(again.plugins().iter().any(|x| x.path.ends_with("Broken.clap")));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn without_the_scanner_binary_files_are_reported_not_forgotten() {
    use aitrack_host::scan::ProblemKind;
    let tmp = temp("noscanner");
    let plugins = tmp.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(plugins.join("A.clap"), b"x").unwrap();
    let mut s = Scanner::new(tmp.join("plugins.json")).with_scanner_exe(tmp.join("missing-scanner")).without_default_paths();
    s.add_search_path(PluginFormat::Clap, &plugins);
    s.scan().unwrap();
    let p = s.problems();
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].kind, ProblemKind::NoScanner);
    assert!(s.blocklist().next().is_none(), "not the plugin's fault: not blocked for good");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn progress_counts_every_file_once_and_the_search_roots_are_listed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = fixture();
    let tmp = temp("progress");
    let mut s = scanner(&tmp.join("plugins.json"));
    s.add_search_path(PluginFormat::Clap, &f.dir);
    s.add_search_path(PluginFormat::Vst3, &f.dir);
    s.add_search_path(PluginFormat::Vst3, tmp.join("not-there"));
    let roots = s.search_roots();
    assert_eq!(roots.len(), 3);
    assert!(roots.iter().all(|r| r.custom));
    assert!(roots.iter().any(|r| r.exists && r.files == 1 && r.format == PluginFormat::Clap));
    let gone = roots.iter().find(|r| r.path.ends_with("not-there")).unwrap();
    assert!(!gone.exists, "a folder that is not there says so");

    let files = s.discover();
    let last = AtomicUsize::new(0);
    let calls = AtomicUsize::new(0);
    let r = s.scan_files_with(&files, &|done, total| {
        assert_eq!(total, files.len());
        last.fetch_max(done, Ordering::Relaxed);
        calls.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(last.load(Ordering::Relaxed), files.len());
    assert_eq!(calls.load(Ordering::Relaxed), files.len());
    assert!(r.elapsed > Duration::ZERO);
    let _ = std::fs::remove_dir_all(&tmp);
}
