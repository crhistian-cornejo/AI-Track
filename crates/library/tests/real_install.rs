//! Installs a real plugin from the catalog, end to end, into the user's own
//! plugin folders. It downloads from the maker's site and leaves software on
//! the machine, so it is ignored by default and runs only when asked:
//!
//! ```text
//! cargo test -p aitrack-library --test real_install -- --ignored --nocapture
//! AITRACK_INSTALL_ID=tdr-nova cargo test … -- --ignored --nocapture
//! ```
//!
//! Set `AITRACK_INSTALL_KEEP=1` to leave the plugin installed; by default it
//! is uninstalled again, so the test does not change the machine it ran on.

use aitrack_library::catalog::{Catalog, Os};
use aitrack_library::plugins::{HttpFetcher, InstallPlan, PluginEvent, UserDirs, install_plugin, read_receipt, uninstall_plugin};
use std::sync::atomic::AtomicBool;
use crossbeam_channel::unbounded;

#[test]
#[ignore = "downloads from the maker's site and installs a plugin"]
fn a_catalogued_plugin_installs_and_uninstalls() {
    let id = std::env::var("AITRACK_INSTALL_ID").unwrap_or_else(|_| "tal-noisemaker".to_owned());
    let keep = std::env::var_os("AITRACK_INSTALL_KEEP").is_some();
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/plugins/manifest.toml")).expect("manifest");
    let catalog = Catalog::parse(&text).expect("the catalog parses");
    let entry = catalog.get(&id).unwrap_or_else(|| panic!("{id} is not in the catalog")).clone();
    let os = Os::current().expect("macOS or Windows");
    assert!(entry.installable(os), "{id} is not installable here: it needs a pinned artifact for this OS");
    let artifact = entry.artifact_for(os).expect("artifact").clone();
    let dirs = UserDirs::standard().expect("user plugin folders");
    let data = std::env::temp_dir().join(format!("aitrack-real-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);

    let plan = InstallPlan { entry, artifact, os, dirs: dirs.clone(), data: data.clone() };
    let (tx, rx) = unbounded();
    let receipt = install_plugin(&plan, &HttpFetcher::default(), &AtomicBool::new(false), &tx).expect("the install succeeds");
    drop(tx);
    let stages: Vec<_> = rx.iter().filter_map(|e| if let PluginEvent::Progress { stage, .. } = e { Some(stage) } else { None }).collect();
    println!("{id}: {} file(s) installed, stages seen: {stages:?}", receipt.files.len());

    assert!(!receipt.files.is_empty(), "the receipt names what was installed");
    for f in &receipt.files {
        assert!(f.path.exists(), "{} is on disk", f.path.display());
        assert!(f.path.starts_with(&dirs.vst3) || f.path.starts_with(&dirs.clap), "{} is in a user plugin folder", f.path.display());
        println!("  {}", f.path.display());
    }
    assert!(read_receipt(&data, &id).is_some(), "the receipt can be read back");

    if keep {
        println!("{id} left installed (AITRACK_INSTALL_KEEP)");
        return;
    }
    let removed = uninstall_plugin(&data, &dirs, &id).expect("the uninstall succeeds");
    assert_eq!(removed, receipt.files.len(), "everything the receipt named is gone");
    for f in &receipt.files {
        assert!(!f.path.exists(), "{} was removed", f.path.display());
    }
    let _ = std::fs::remove_dir_all(&data);
}
