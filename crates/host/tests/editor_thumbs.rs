//! The first time a plugin's editor is opened, a thumbnail of it is kept.
//!
//! Skipped unless `AITRACK_TEST_PLUGINS` lists plugin files or folders (as for
//! `real_plugins`). Opens the editor of each plugin found (at most
//! `AITRACK_TEST_THUMBS_MAX`, default 6; `AITRACK_TEST_THUMBS_ONLY=name` for one) in a
//! temporary folder, waits for the picture, and says how long it took, how big
//! it is and what happens the second time. Skipped when the screen is locked.
//! `harness = false`: editor windows must live on the main thread.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aitrack_host::scan::{Scanner, find_plugin_files};
use aitrack_host::{HostedPlugin, PluginFormat, thumbs};

#[cfg(target_os = "macos")]
fn init_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.finishLaunching();
    }
}

#[cfg(not(target_os = "macos"))]
fn init_app() {}

fn main() {
    let Some(var) = std::env::var_os("AITRACK_TEST_PLUGINS") else {
        eprintln!("editor_thumbs: AITRACK_TEST_PLUGINS not set; skipped");
        return;
    };
    if aitrack_host::editor::screen_locked() {
        eprintln!("editor_thumbs: screen locked; skipped");
        return;
    }
    init_app();
    let roots: Vec<PathBuf> = std::env::split_paths(&var).collect();
    let mut found = Vec::new();
    for fmt in [PluginFormat::Vst3, PluginFormat::Clap] {
        found.extend(find_plugin_files(&roots, fmt).into_iter().map(|p| (fmt, p)));
    }
    let tmp = std::env::temp_dir().join(format!("aitrack-editor-thumbs-{}", std::process::id()));
    let dir = tmp.join("thumbs");
    std::fs::create_dir_all(&tmp).unwrap();
    thumbs::set_dir(dir.clone());
    aitrack_host::editor::placement::set_file(tmp.join("windows.json"));
    let mut scanner = Scanner::new(tmp.join("plugins.json")).with_scanner_exe(env!("CARGO_BIN_EXE_aitrack-plugin-scanner")).without_default_paths();
    scanner.scan_files(&found);
    let only = std::env::var("AITRACK_TEST_THUMBS_ONLY").ok();
    let max: usize = std::env::var("AITRACK_TEST_THUMBS_MAX").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
    let mut plugins = scanner.plugins();
    plugins.sort_by_key(|p| (p.name.clone(), format!("{:?}", p.format)));
    plugins.retain(|p| only.as_deref().is_none_or(|o| p.name.to_lowercase().contains(&o.to_lowercase())));
    plugins.truncate(max);
    let mut shot = 0;
    for info in &plugins {
        let Ok(p) = HostedPlugin::load(info) else {
            eprintln!("{} [{:?}]: does not load", info.name, info.format);
            continue;
        };
        let ctl = p.controller();
        if !ctl.has_editor() {
            eprintln!("{} [{:?}]: no editor, so no thumbnail (the row keeps its icon)", info.name, info.format);
            continue;
        }
        let stamp = thumbs::stamp(info.format, &info.path).unwrap();
        let id = info.stable_id();
        let t0 = Instant::now();
        if let Err(e) = ctl.open_editor() {
            eprintln!("{} [{:?}]: editor does not open ({e}): no thumbnail", info.name, info.format);
            continue;
        }
        let mut appeared = None;
        while t0.elapsed() < Duration::from_secs(6) {
            aitrack_host::editor::pump_messages();
            ctl.idle();
            if appeared.is_none() && thumbs::find(&dir, &id, &stamp).is_some() {
                appeared = Some(t0.elapsed());
            }
            if appeared.is_some() || thumbs::settled(&dir, &id, &stamp) {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        match thumbs::find(&dir, &id, &stamp) {
            Some(path) => {
                shot += 1;
                eprintln!("{} [{:?}]: thumbnail after {:?}, {} bytes ({})", info.name, info.format, appeared.unwrap(), std::fs::metadata(&path).map_or(0, |m| m.len()), path.display());
            }
            None => eprintln!("{} [{:?}]: editor painted nothing a snapshot can see ({} blank notes)", info.name, info.format, thumbs::settled(&dir, &id, &stamp) as u8),
        }
        ctl.close_editor();
        // Opening it again does not take another one.
        ctl.open_editor().ok();
        for _ in 0..140 {
            aitrack_host::editor::pump_messages();
            ctl.idle();
            std::thread::sleep(Duration::from_millis(15));
        }
        ctl.close_editor();
    }
    let (n, bytes) = thumbs::usage(&dir);
    eprintln!("editor_thumbs: {shot}/{} thumbnails; folder holds {n} files, {bytes} bytes", plugins.len());
    if std::env::var_os("AITRACK_TEST_THUMBS_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&tmp);
    } else {
        eprintln!("kept in {}", dir.display());
    }
}
