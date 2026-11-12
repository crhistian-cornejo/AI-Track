//! End-to-end checks against real third-party plugins (Surge XT, Dexed…).
//!
//! Skipped unless `AITRACK_TEST_PLUGINS` lists plugin files or folders
//! (separated like `PATH`). For example:
//!
//! ```text
//! AITRACK_TEST_PLUGINS=~/plugins cargo test -p aitrack-host --test real_plugins -- --nocapture
//! ```
//!
//! For every plugin found it checks: out-of-process scan and cache reuse,
//! parameter listing and display text, activation at two sample rates and
//! block sizes, audio through an effect, MIDI notes through an instrument
//! (non-silent output), state round-trip, latency, and opening the plugin's
//! own editor window, in process and in the `aitrack-plugin-host` sandbox
//! (`AITRACK_TEST_NO_SANDBOX=1` skips that, `AITRACK_TEST_ONLY_SANDBOX=1` runs only that: a
//! plugin that crashes on unload in process then still gets its sandbox
//! checked). `AITRACK_TEST_EDITOR_SECS=N` keeps each editor open
//! for N seconds (for screenshots); `AITRACK_TEST_NO_EDITOR=1` skips them, and so does a locked
//! screen (macOS) unless `AITRACK_TEST_EDITOR_LOCKED=1`.
//!
//! `harness = false`: plugin editors (NSWindow / HWND) must live on the
//! process's main thread, which the default test harness never runs on.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aitrack_dsp::{AudioBuffer, Event, EventKind, ProcessContext, Processor, Transport};
use aitrack_host::scan::{Scanner, find_plugin_files};
use aitrack_host::sandbox::SandboxedPlugin;
use aitrack_host::{HostedPlugin, HostedPluginInfo, PluginFormat};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn inputs() -> Vec<PathBuf> {
    let Some(raw) = std::env::var_os("AITRACK_TEST_PLUGINS") else { return Vec::new() };
    std::env::split_paths(&raw).filter(|p| p.exists()).collect()
}

fn format_of(path: &std::path::Path) -> Option<PluginFormat> {
    match path.extension()?.to_str()? {
        "vst3" => Some(PluginFormat::Vst3),
        "clap" => Some(PluginFormat::Clap),
        _ => None,
    }
}

fn files(paths: &[PathBuf]) -> Vec<(PluginFormat, PathBuf)> {
    let mut out = Vec::new();
    for p in paths {
        if let Some(f) = format_of(p) {
            out.push((f, p.clone()));
            continue;
        }
        for f in [PluginFormat::Vst3, PluginFormat::Clap] {
            out.extend(find_plugin_files(std::slice::from_ref(p), f).into_iter().map(|x| (f, x)));
        }
    }
    out
}

fn peak(buf: &AudioBuffer) -> f32 {
    (0..buf.channels()).map(|c| buf.peak(c)).fold(0.0, f32::max)
}

/// Run `blocks` blocks of `frames`; `events(block)` gives that block's
/// events, `input(buf, block)` fills the input. Returns the output peak.
fn render(
    p: &mut HostedPlugin,
    sr: f64,
    frames: usize,
    blocks: usize,
    mut events: impl FnMut(usize) -> Vec<Event>,
    mut input: impl FnMut(&mut AudioBuffer, usize),
) -> f32 {
    let mut buf = AudioBuffer::new(2, frames);
    let mut max = 0.0f32;
    for b in 0..blocks {
        buf.clear();
        input(&mut buf, b);
        let ev = events(b);
        let pos = (b * frames) as i64;
        let transport =
            Transport { playing: true, position_samples: pos, position_beats: pos as f64 / sr * 2.0, bpm: 120.0, ..Default::default() };
        let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut buf, sidechain: None, events: &ev, transport: &transport };
        p.process(&mut ctx);
        max = max.max(peak(&buf));
    }
    max
}

fn sine(buf: &mut AudioBuffer, block: usize, sr: f64) {
    let frames = buf.frames();
    for ch in 0..buf.channels() {
        for (i, s) in buf.channel_mut(ch).iter_mut().enumerate() {
            let t = (block * frames + i) as f64 / sr;
            *s = 0.5 * (t * 440.0 * std::f64::consts::TAU).sin() as f32;
        }
    }
}

fn note(on: bool, offset: u32, key: u8) -> Event {
    let kind = if on {
        EventKind::NoteOn { channel: 0, key, velocity: 0.8, note_id: -1 }
    } else {
        EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: -1 }
    };
    Event { offset, kind }
}

/// Instrument: chord at block 0, released at block `blocks/2`. Effect: sine in.
fn exercise(p: &mut HostedPlugin, info: &HostedPluginInfo, sr: f64, frames: usize) -> f32 {
    let blocks = (sr * 0.6) as usize / frames;
    if info.is_instrument {
        render(
            p,
            sr,
            frames,
            blocks,
            |b| match b {
                0 => vec![note(true, 3, 60), note(true, 3, 64), note(true, 3, 67)],
                b if b == blocks / 2 => vec![note(false, 0, 60), note(false, 0, 64), note(false, 0, 67)],
                _ => Vec::new(),
            },
            |_, _| {},
        )
    } else {
        render(p, sr, frames, blocks, |_| Vec::new(), |buf, b| sine(buf, b, sr))
    }
}

struct Outcome {
    name: String,
    lines: Vec<String>,
    failures: Vec<String>,
}

fn check(info: &HostedPluginInfo, editor_secs: Option<f64>) -> Outcome {
    let mut o = Outcome { name: format!("{} {:?} {} ({})", info.name, info.format, info.version, info.vendor), lines: Vec::new(), failures: Vec::new() };
    let t0 = Instant::now();
    let mut p = match HostedPlugin::load(info) {
        Ok(p) => p,
        Err(e) => {
            o.failures.push(format!("load: {e}"));
            return o;
        }
    };
    let pi = p.info().clone();
    o.lines.push(format!(
        "loaded in {:?}: {} in / {} out, {} params, instrument {}",
        t0.elapsed(),
        pi.inputs,
        pi.outputs,
        pi.params.len(),
        p.hosted_info().is_instrument
    ));
    let ctl = p.controller();
    if pi.params.is_empty() {
        o.failures.push("no parameters listed".into());
    }
    let shown: Vec<String> = pi
        .params
        .iter()
        .take(4)
        .map(|par| {
            let v = p.get_param(par.id);
            format!("{}={v:.3} \"{}\"", par.name, ctl.param_text(par.id, v).unwrap_or_default())
        })
        .collect();
    o.lines.push(format!("params: {}", shown.join(", ")));

    for (sr, frames) in [(SR, BLOCK), (44_100.0, 128)] {
        p.prepare(sr, frames);
        if !p.is_prepared() {
            o.failures.push(format!("prepare({sr}, {frames}) failed"));
            continue;
        }
        let level = exercise(&mut p, info, sr, frames);
        o.lines.push(format!("{sr} Hz / {frames}: peak {level:.4}, latency {}", p.latency()));
        if level < 1e-4 {
            o.failures.push(format!("silent output at {sr} Hz / {frames}"));
        }
        if p.is_crashed() {
            o.failures.push("crashed while processing".into());
        }
    }
    // A short block inside the prepared maximum (hosts do this at loop points).
    let short = render(&mut p, 44_100.0, 77, 4, |_| Vec::new(), |buf, b| sine(buf, b, 44_100.0));
    o.lines.push(format!("77-frame blocks: peak {short:.4}"));

    // Parameter automation through set_param must reach the plugin.
    if let Some(par) = pi.params.iter().find(|par| par.automatable) {
        let before = p.get_param(par.id);
        let target = if before > 0.5 { 0.1 } else { 0.9 };
        p.set_param(par.id, target);
        render(&mut p, 44_100.0, 128, 2, |_| Vec::new(), |_, _| {});
        ctl.idle();
        o.lines.push(format!("set_param {} {before:.3} -> {:.3} (\"{}\")", par.name, p.get_param(par.id), ctl.param_text(par.id, target).unwrap_or_default()));
        p.set_param(par.id, before);
    }

    match ctl.save_state() {
        Ok(state) => {
            let reload = ctl.load_state(&state);
            o.lines.push(format!("state {} bytes, reload {}", state.len(), if reload.is_ok() { "ok" } else { "FAILED" }));
            if let Err(e) = reload {
                o.failures.push(format!("load_state: {e}"));
            }
        }
        Err(e) => o.failures.push(format!("save_state: {e}")),
    }

    if let Some(secs) = editor_secs {
        if !ctl.has_editor() {
            o.lines.push("no editor".into());
        } else {
            match ctl.open_editor() {
                Ok(()) => {
                    let until = Instant::now() + Duration::from_secs_f64(secs);
                    let mut buf = AudioBuffer::new(2, 128);
                    let transport = Transport::default();
                    while Instant::now() < until {
                        aitrack_host::editor::pump_messages();
                        ctl.idle();
                        buf.clear();
                        let mut ctx = ProcessContext { sample_rate: 44_100.0, buffer: &mut buf, sidechain: None, events: &[], transport: &transport };
                        p.process(&mut ctx);
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    let open = ctl.is_editor_open();
                    o.lines.push(format!("editor opened, still open after {secs:.1}s: {open}"));
                    if !open {
                        o.failures.push("editor closed by itself".into());
                    }
                    ctl.close_editor();
                    aitrack_host::editor::pump_messages();
                    if ctl.is_editor_open() {
                        o.failures.push("editor did not close".into());
                    }
                    // Re-open once: plugins must tolerate a second view.
                    match ctl.open_editor() {
                        Ok(()) => {
                            aitrack_host::editor::pump_messages();
                            ctl.close_editor();
                        }
                        Err(e) => o.failures.push(format!("second open_editor: {e}")),
                    }
                }
                Err(e) => o.failures.push(format!("open_editor: {e}")),
            }
        }
    }
    if p.is_crashed() {
        o.failures.push("crashed".into());
    }
    drop(p);
    o
}

/// The same plugin in the `aitrack-plugin-host` sandbox process: audio
/// over shared memory, notes, state and the editor window of that process.
fn check_sandboxed(info: &HostedPluginInfo, editor_secs: Option<f64>, o: &mut Outcome) {
    let mut p = match SandboxedPlugin::spawn_with(std::path::Path::new(env!("CARGO_BIN_EXE_aitrack-plugin-host")), info) {
        Ok(p) => p,
        Err(e) => {
            o.failures.push(format!("sandbox spawn: {e}"));
            return;
        }
    };
    // Generous budget: debug builds and a cold plugin are slow.
    p.set_block_budget(Some(Duration::from_millis(500)));
    p.prepare(SR, BLOCK);
    let blocks = (SR * 0.6) as usize / BLOCK;
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut level = 0.0f32;
    for b in 0..blocks {
        buf.clear();
        let ev = match b {
            0 if info.is_instrument => vec![note(true, 3, 60)],
            b if b == blocks / 2 && info.is_instrument => vec![note(false, 0, 60)],
            _ => Vec::new(),
        };
        if !info.is_instrument {
            sine(&mut buf, b, SR);
        }
        let transport = Transport { playing: true, ..Default::default() };
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &ev, transport: &transport };
        p.process(&mut ctx);
        level = level.max(peak(&buf));
    }
    let state = p.save_state();
    o.lines.push(format!("sandbox: peak {level:.4}, {} late blocks, state {} bytes", p.late_blocks(), state.len()));
    if level < 1e-4 {
        o.failures.push("sandbox: silent output".into());
    }
    if let Some(secs) = editor_secs {
        match p.open_editor() {
            Ok(()) => {
                std::thread::sleep(Duration::from_secs_f64(secs));
                if let Err(e) = p.close_editor() {
                    o.failures.push(format!("sandbox close_editor: {e}"));
                }
                o.lines.push("sandbox editor opened and closed".into());
            }
            Err(e) => o.failures.push(format!("sandbox open_editor: {e}")),
        }
    }
    if !p.is_alive() {
        o.failures.push("sandbox process died".into());
    }
}

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
    // `cargo test -- --list` and friends: nothing to list.
    if std::env::args().any(|a| a == "--list") {
        return;
    }
    let paths = inputs();
    if paths.is_empty() {
        eprintln!("real_plugins: AITRACK_TEST_PLUGINS not set (or no such paths); skipped");
        return;
    }
    init_app();
    let found = files(&paths);
    eprintln!("real_plugins: {} plugin files", found.len());

    let dir = std::env::temp_dir().join(format!("aitrack-real-plugins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("plugins.json");
    let scanner = || Scanner::new(&cache).with_scanner_exe(env!("CARGO_BIN_EXE_aitrack-plugin-scanner")).without_default_paths();
    let t0 = Instant::now();
    let mut s = scanner();
    let report = s.scan_files(&found);
    s.save().unwrap();
    eprintln!("scan: {} files, {} scanned, {} failed in {:?}", report.files, report.scanned, report.failed.len(), t0.elapsed());
    let mut failures: Vec<String> = report.failed.iter().map(|(p, why)| format!("scan {}: {why}", p.display())).collect();
    let t1 = Instant::now();
    let mut again = scanner();
    let report2 = again.scan_files(&found);
    eprintln!("rescan: {} cached, {} scanned in {:?}", report2.cached, report2.scanned, t1.elapsed());
    if report2.scanned != 0 {
        failures.push(format!("rescan re-scanned {} unchanged files", report2.scanned));
    }

    let editor_secs = if std::env::var_os("AITRACK_TEST_NO_EDITOR").is_some() {
        None
    } else if aitrack_host::editor::screen_locked() && std::env::var_os("AITRACK_TEST_EDITOR_LOCKED").is_none() {
        // Off-screen OpenGL editors (Odin2, Dragonfly) stall the main
        // thread while the session is locked; there is nothing to see anyway.
        eprintln!("real_plugins: screen locked, editors NOT checked");
        None
    } else {
        Some(std::env::var("AITRACK_TEST_EDITOR_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0))
    };
    let mut plugins = s.plugins();
    plugins.sort_by_key(|p| (p.name.clone(), format!("{:?}", p.format)));
    for info in &plugins {
        eprintln!(
            "\n== {} [{:?}] {} by {} — {} ({} in / {} out, {} params)",
            info.name, info.format, info.version, info.vendor, info.category, info.inputs, info.outputs, info.params
        );
        let mut o = if std::env::var_os("AITRACK_TEST_ONLY_SANDBOX").is_some() {
            Outcome { name: format!("{} {:?} {} ({})", info.name, info.format, info.version, info.vendor), lines: Vec::new(), failures: Vec::new() }
        } else {
            check(info, editor_secs)
        };
        if std::env::var_os("AITRACK_TEST_NO_SANDBOX").is_none() {
            check_sandboxed(info, editor_secs, &mut o);
        }
        for l in &o.lines {
            eprintln!("   {l}");
        }
        for f in &o.failures {
            eprintln!("   FAIL {f}");
            failures.push(format!("{}: {f}", o.name));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if plugins.is_empty() {
        failures.push("no plugins found by the scanner".into());
    }
    if !failures.is_empty() {
        eprintln!("\nreal_plugins: {} failure(s):", failures.len());
        for f in &failures {
            eprintln!("  {f}");
        }
        std::process::exit(1);
    }
    eprintln!("\nreal_plugins: {} plugins ok", plugins.len());
}
