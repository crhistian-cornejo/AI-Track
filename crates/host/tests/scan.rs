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
