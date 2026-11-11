//! Tests against the plugins installed on this machine. Ignored by default
//! (they depend on the machine); run with `cargo test -p aitrack-host
//! --test system_plugins -- --ignored --nocapture`.

use aitrack_dsp::{AudioBuffer, ProcessContext, Processor, Transport};
use aitrack_host::scan::{Scanner, default_search_paths, find_plugin_files};
use aitrack_host::{HostedPlugin, PluginFormat};

#[test]
#[ignore = "depends on installed plugins"]
fn scan_system_folders() {
    for format in [PluginFormat::Vst3, PluginFormat::Clap] {
        let roots = default_search_paths(format);
        let files = find_plugin_files(&roots, format);
        eprintln!("{format:?}: {} files in {roots:?}", files.len());
    }
    let cache = std::env::temp_dir().join(format!("aitrack-system-scan-{}.json", std::process::id()));
    let mut s = Scanner::new(&cache).with_scanner_exe(env!("CARGO_BIN_EXE_aitrack-plugin-scanner"));
    let report = s.scan().unwrap();
    eprintln!("{report:#?}");
    for p in s.plugins() {
        eprintln!("  {:?} {} — {} ({}) in/out {}/{} params {}", p.format, p.name, p.vendor, p.category, p.inputs, p.outputs, p.params);
    }
    for (path, b) in s.blocklist() {
        eprintln!("  blocked {}: {:?} {}", path.display(), b.reason, b.message);
    }
    let _ = std::fs::remove_file(cache);
}

#[test]
#[ignore = "depends on installed plugins"]
fn first_installed_vst3_processes_silence() {
    let files = find_plugin_files(&default_search_paths(PluginFormat::Vst3), PluginFormat::Vst3);
    let Some(path) = files.first() else {
        eprintln!("no VST3 plugins installed; nothing to test");
        return;
    };
    let mut p = HostedPlugin::load_path(PluginFormat::Vst3, path, None).expect("load");
    p.prepare(48_000.0, 512);
    assert!(p.is_prepared());
    let mut buf = AudioBuffer::new(2, 512);
    let transport = Transport::default();
    for _ in 0..16 {
        buf.clear();
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &transport };
        p.process(&mut ctx);
    }
    assert!(!p.is_crashed());
    eprintln!("{} processed 16 blocks; latency {}", p.info().name, p.latency());
}
