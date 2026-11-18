//! Offline preview of Bloom's matter visual: renders PNGs headless with
//! wgpu (no window), the same shader and mappings the app uses.
//!
//! ```text
//! cargo run -p aitrack-app --release --example bloom_matter_render -- <out_dir>
//! ```
//!
//! Writes, for review:
//! * `material-<name>-{rest,hit}-{light,dark}.png`: every material at rest and during a hit;
//! * `sound-<n>-<role>-{light,dark}.png`: six sounds' own looks on Obsidian;
//! * `strip-{light,dark}.png`: 30 frames (1/20 s apart) of a hit and its settling;
//! * `sheet-{light,dark}.png`: every material side by side;
//! * `ensemble-{window,card}-{light,dark}.png`: four sounds playing a beat,
//!   12 frames at the window's and the card's stage sizes, and
//!   `ensemble-materials-*.png`: every material mid-beat (second argument
//!   `ensemble`: only these);
//! * `timing.txt`: GPU time per frame at 512 × 512, and at the window's
//!   stage size on a Retina display (1800 × 920) down to 640 × 328.
//! * with a second argument `knobs`, only `knobs-{light,dark}.png`: one sound
//!   under each knob turned (timbre re-analysed under the knobs, then the
//!   direct mapping), at rest and a moment after a hit.

#![allow(dead_code, unused_imports)]

#[path = "../src/views/device_window/bloom/look.rs"]
mod look;
#[path = "../src/views/device_window/bloom/matter/mod.rs"]
mod matter;

use aitrack_stock::instrument::bloom::genome::{SOUNDS, Species};
use aitrack_stock::instrument::bloom::grow::{GrowOpts, generate};
use aitrack_stock::instrument::bloom::timbre;
use matter::frame::{Animator, Live, Panel};
use matter::gpu::Gpu;
use matter::mapping::{Controls, SoundLook, sound_look, with_controls};
use aitrack_stock::instrument::bloom::voice::Macros;
use matter::material::Material;
use std::path::{Path, PathBuf};

/// BGRA → RGBA in place (for PNGs).
fn bgra_to_rgba(px: &mut [u8]) {
    for c in px.as_chunks_mut::<4>().0 {
        c.swap(0, 2);
    }
}

/// The `well` colours of the Midnight and Daylight themes, linear.
fn panel_colour(light: bool) -> [f32; 3] {
    let hex: u32 = if light { 0xE4E5E9 } else { 0x101114 };
    let lin = |v: u32| {
        let c = v as f32 / 255.0;
        if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [lin(hex >> 16 & 0xFF), lin(hex >> 8 & 0xFF), lin(hex & 0xFF)]
}

const W: u32 = 720;
const H: u32 = 560;

fn panel(light: bool, w: u32, h: u32) -> Panel {
    Panel::fit(w as f32, h as f32, (0.0, 0.0, w as f32, h as f32), 24.0, panel_colour(light), light, [0.0; 3])
}

struct Shot<'a> {
    looks: &'a [SoundLook; SOUNDS],
    selected: usize,
    material: Material,
    light: bool,
    /// Seconds of rest before the shot.
    rest: f32,
    /// Seconds since a hit of the selected sound (None: no hit).
    hit: Option<f32>,
    cursor: Option<(f32, f32)>,
}

/// The animator after the shot's history, stepped at 120 Hz.
fn animate(s: &Shot) -> Animator {
    let mut a = Animator::default();
    let dt = 1.0 / 120.0;
    let steps = (s.rest / dt) as usize;
    let quiet = [0.0; SOUNDS];
    for _ in 0..steps {
        a.step(dt, &Live { env: &quiet, pulse: &quiet, looks: s.looks, selected: s.selected, hits: &NO_HITS, level: 0.0 }, s.cursor, false);
    }
    if let Some(after) = s.hit {
        let n = (after / dt) as usize;
        for k in 0..=n {
            let t = k as f32 * dt;
            let (mut env, mut pulse) = ([0.0; SOUNDS], [0.0; SOUNDS]);
            env[s.selected] = (-t / 0.25).exp();
            pulse[s.selected] = (-t / 0.06).exp();
            let mut hits = NO_HITS;
            hits[s.selected] = 1.0;
            a.step(dt, &Live { env: &env, pulse: &pulse, looks: s.looks, selected: s.selected, hits: &hits, level: 0.3 * env[s.selected] }, s.cursor, false);
        }
    }
    a
}

const NO_HITS: [f32; SOUNDS] = [0.0; SOUNDS];

/// A beat on four sounds (kick, hat, snare, perc) for `secs` seconds at
/// 120 BPM, stepped at 120 Hz; calls `shot` with the animator every `every` steps.
fn ensemble(looks: &[SoundLook; SOUNDS], secs: f32, every: usize, mut shot: impl FnMut(usize, &Animator)) {
    let mut a = Animator::default();
    let dt = 1.0 / 120.0;
    let steps = (secs / dt) as usize;
    let sounds: [(usize, f32, f32, f32); 4] = [(0, 1.0, 0.0, 4.0), (6, 0.5, 0.0, 9.0), (10, 2.0, 1.0, 5.0), (13, 1.0, 0.5, 6.0)];
    let mut hits = NO_HITS;
    for k in 0..steps {
        let beat = k as f32 * dt * 2.0;
        let (mut env, mut pulse) = ([0.0; SOUNDS], [0.0; SOUNDS]);
        for &(i, period, offset, decay) in &sounds {
            let ph = ((beat - offset) / period).rem_euclid(1.0) * period;
            env[i] = (-ph * decay).exp();
            pulse[i] = (-ph * 16.0).exp();
            hits[i] = ((beat - offset) / period).floor();
        }
        let level = (0.18 * env.iter().sum::<f32>()).min(0.9);
        a.step(dt, &Live { env: &env, pulse: &pulse, looks, selected: 0, hits: &hits, level }, None, false);
        if k % every == every - 1 {
            shot(k / every, &a);
        }
    }
}

fn render(gpu: &Gpu, target: &matter::gpu::Target, s: &Shot, w: u32, h: u32) -> Vec<u8> {
    let a = animate(s);
    let u = a.uniforms(s.material, &panel(s.light, w, h), s.selected as f32);
    let mut px = gpu.render_bgra(target, &u).expect("render");
    bgra_to_rgba(&mut px);
    px
}

fn save(dir: &Path, name: &str, px: &[u8], w: u32, h: u32) {
    let path = dir.join(name);
    image::save_buffer(&path, px, w, h, image::ExtendedColorType::Rgba8).expect("png");
    println!("{}", path.display());
}

/// Tile `images` (each w × h) into a grid `cols` wide.
fn grid(images: &[Vec<u8>], w: u32, h: u32, cols: u32) -> (Vec<u8>, u32, u32) {
    let rows = (images.len() as u32).div_ceil(cols);
    let (gw, gh) = (w * cols, h * rows);
    let mut out = vec![0u8; (gw * gh * 4) as usize];
    for (i, img) in images.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols * w, i as u32 / cols * h);
        for y in 0..h {
            let src = &img[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            let at = (((cy + y) * gw + cx) * 4) as usize;
            out[at..at + (w * 4) as usize].copy_from_slice(src);
        }
    }
    (out, gw, gh)
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "bloom-matter".into()));
    std::fs::create_dir_all(&dir).expect("out dir");
    // `MATTER_WGSL=<file>` renders with another copy of the shader (to tune it without rebuilding).
    let source = std::env::var("MATTER_WGSL").ok().and_then(|p| std::fs::read_to_string(p).ok());
    let gpu = match source.as_deref().map_or_else(Gpu::new, Gpu::with_source) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("adapter: {} ({:?})", gpu.adapter, gpu.backend);
    let scene = generate(271_828, GrowOpts { species: Species::Mixed, spread: 1.0 });
    let timbres: Vec<_> = scene.sounds.iter().map(timbre::analyze).collect();
    let looks_for = |light: bool| -> [SoundLook; SOUNDS] { std::array::from_fn(|i| sound_look(&timbres[i], light)) };
    let (looks_l, looks_d) = (looks_for(true), looks_for(false));
    // A second argument renders only the materials whose key contains it (`timing`: only the timing).
    let only = std::env::args().nth(2).unwrap_or_default();
    if only == "timing" {
        timing(&gpu, &looks_d, &dir);
        return;
    }
    let target = gpu.target(W, H);
    if only == "knobs" {
        knobs(&gpu, &target, &scene, &dir);
        return;
    }
    if only == "ensemble" || only.is_empty() {
        ensemble_shots(&gpu, &looks_l, &looks_d, &dir);
        if !only.is_empty() {
            return;
        }
    }

    // Every material at rest and during a hit.
    for light in [true, false] {
        let looks = if light { &looks_l } else { &looks_d };
        let theme = if light { "light" } else { "dark" };
        let mut sheet = Vec::new();
        for m in Material::ALL {
            if !only.is_empty() && !m.key().contains(only.as_str()) {
                continue;
            }
            let rest = render(&gpu, &target, &Shot { looks, selected: 0, material: m, light, rest: 2.0, hit: None, cursor: None }, W, H);
            let hit = render(&gpu, &target, &Shot { looks, selected: 0, material: m, light, rest: 2.0, hit: Some(0.05), cursor: None }, W, H);
            save(&dir, &format!("material-{}-rest-{theme}.png", m.key()), &rest, W, H);
            save(&dir, &format!("material-{}-hit-{theme}.png", m.key()), &hit, W, H);
            sheet.push(rest);
        }
        if only.is_empty() {
            let (g, gw, gh) = grid(&sheet, W, H, 4);
            save(&dir, &format!("sheet-{theme}.png"), &g, gw, gh);
        }
    }
    if !only.is_empty() {
        return;
    }

    // Six sounds' own looks on Obsidian, at rest and a moment after a hit.
    let picks = [0usize, 1, 3, 7, 11, 14];
    for light in [true, false] {
        let looks = if light { &looks_l } else { &looks_d };
        let theme = if light { "light" } else { "dark" };
        let mut tiles = Vec::new();
        for &i in &picks {
            let px = render(&gpu, &target, &Shot { looks, selected: i, material: Material::Obsidian, light, rest: 1.5, hit: Some(0.12), cursor: None }, W, H);
            save(&dir, &format!("sound-{:02}-{}-{theme}.png", i + 1, scene.sounds[i].role.label().to_lowercase().replace(' ', "-")), &px, W, H);
            tiles.push(px);
        }
        let (g, gw, gh) = grid(&tiles, W, H, 3);
        save(&dir, &format!("sounds-{theme}.png"), &g, gw, gh);
    }

    // A 30-frame strip: rest, a hit at frame 6, the settling.
    let (sw, sh) = (360, 280);
    let small = gpu.target(sw, sh);
    for light in [true, false] {
        let looks = if light { &looks_l } else { &looks_d };
        let theme = if light { "light" } else { "dark" };
        let mut a = Animator::default();
        let dt = 1.0 / 120.0;
        let mut frames = Vec::new();
        let quiet = [0.0; SOUNDS];
        for _ in 0..120 {
            a.step(dt, &Live { env: &quiet, pulse: &quiet, looks, selected: 0, hits: &NO_HITS, level: 0.0 }, None, false);
        }
        for f in 0..30 {
            for k in 0..6 {
                let t = (f as f32 * 6.0 + k as f32) * dt - 6.0 * 6.0 * dt;
                let (mut env, mut pulse, mut hits) = ([0.0; SOUNDS], [0.0; SOUNDS], NO_HITS);
                if t >= 0.0 {
                    env[0] = (-t / 0.25).exp();
                    pulse[0] = (-t / 0.06).exp();
                    hits[0] = 1.0;
                }
                a.step(dt, &Live { env: &env, pulse: &pulse, looks, selected: 0, hits: &hits, level: 0.0 }, Some((0.75, 0.35 - f as f32 * 0.02)), false);
            }
            let u = a.uniforms(Material::Obsidian, &panel(light, sw, sh), 0.0);
            let mut px = gpu.render_bgra(&small, &u).expect("render");
            bgra_to_rgba(&mut px);
            frames.push(px);
        }
        let (g, gw, gh) = grid(&frames, sw, sh, 6);
        save(&dir, &format!("strip-{theme}.png"), &g, gw, gh);
    }

    timing(&gpu, &looks_d, &dir);
}

/// Several sounds playing a beat: a 12-frame strip at the window's size and
/// one at the card's (a small, wide stage), every material mid-beat, light and dark.
fn ensemble_shots(gpu: &Gpu, looks_l: &[SoundLook; SOUNDS], looks_d: &[SoundLook; SOUNDS], dir: &Path) {
    for light in [true, false] {
        let looks = if light { looks_l } else { looks_d };
        let theme = if light { "light" } else { "dark" };
        for (name, w, h, zoom) in [("window", 480u32, 360u32, 1.0f32), ("card", 280, 160, 1.3)] {
            let t = gpu.target(w, h);
            let mut frames = Vec::new();
            ensemble(looks, 3.0, 30, |_, a| {
                let pl = panel(light, w, h).zoomed(zoom);
                let mut px = gpu.render_bgra(&t, &a.uniforms(Material::Obsidian, &pl, 0.0)).expect("render");
                bgra_to_rgba(&mut px);
                frames.push(px);
            });
            let (g, gw, gh) = grid(&frames, w, h, 4);
            save(dir, &format!("ensemble-{name}-{theme}.png"), &g, gw, gh);
        }
        let mut sheet = Vec::new();
        let t = gpu.target(W, H);
        ensemble(looks, 2.2, 252, |_, a| {
            for m in Material::ALL {
                let mut px = gpu.render_bgra(&t, &a.uniforms(m, &panel(light, W, H), 0.0)).expect("render");
                bgra_to_rgba(&mut px);
                sheet.push(px);
            }
        });
        let (g, gw, gh) = grid(&sheet, W, H, 4);
        save(dir, &format!("ensemble-materials-{theme}.png"), &g, gw, gh);
    }
}

/// One sound under each knob: a row at rest and a row after a hit.
fn knobs(gpu: &Gpu, target: &matter::gpu::Target, scene: &aitrack_stock::instrument::bloom::genome::Scene, dir: &Path) {
    let cases: [(&str, Controls); 8] = [
        ("neutral", Controls::default()),
        ("decay+", Controls { decay: 1.0, ..Controls::default() }),
        ("decay-", Controls { decay: -1.0, ..Controls::default() }),
        ("body+", Controls { body: 1.0, ..Controls::default() }),
        ("tone+", Controls { tone: 1.0, ..Controls::default() }),
        ("grit+", Controls { grit: 1.0, ..Controls::default() }),
        ("space+", Controls { space: 1.0, ..Controls::default() }),
        ("tune+12", Controls { tune: 12.0, ..Controls::default() }),
    ];
    let sel = 1;
    for light in [true, false] {
        let theme = if light { "light" } else { "dark" };
        let (mut rest, mut hit) = (Vec::new(), Vec::new());
        for (name, c) in cases {
            let m = Macros { decay: c.decay, body: c.body, tone: c.tone, grit: c.grit };
            let looks: [SoundLook; SOUNDS] = std::array::from_fn(|i| with_controls(&sound_look(&timbre::analyze_shaped(&scene.sounds[i], c.tune, &m), light), &c));
            rest.push(render(gpu, target, &Shot { looks: &looks, selected: sel, material: Material::Obsidian, light, rest: 2.0, hit: None, cursor: None }, W, H));
            hit.push(render(gpu, target, &Shot { looks: &looks, selected: sel, material: Material::Obsidian, light, rest: 2.0, hit: Some(0.08), cursor: None }, W, H));
            println!("{name}");
        }
        let all: Vec<Vec<u8>> = rest.into_iter().chain(hit).collect();
        let (g, gw, gh) = grid(&all, W, H, 8);
        save(dir, &format!("knobs-{theme}.png"), &g, gw, gh);
    }
}

/// GPU time per frame at 512 × 512: throughput of 120 frames, waited for at the end.
fn timing(gpu: &Gpu, looks: &[SoundLook; SOUNDS], dir: &Path) {
    let t512 = gpu.target(512, 512);
    let mut report = String::new();
    let cases = [("rest", None, None), ("hit + magnet", Some(0.03), Some((0.6, 0.3)))];
    for (name, hit, cursor) in cases {
        let mut a = animate(&Shot { looks, selected: 0, material: Material::Obsidian, light: false, rest: 1.0, hit, cursor });
        for m in Material::ALL {
            let u = a.uniforms(m, &panel(false, 512, 512), 0.0);
            let _ = gpu.render_bgra(&t512, &u);
            // Each frame submitted and waited for alone (so this is an upper
            // bound: it includes the submission round trip); median and best of 60.
            let mut times: Vec<f64> = (0..60)
                .map(|_| {
                    let t0 = std::time::Instant::now();
                    let mut enc = gpu.device.create_command_encoder(&Default::default());
                    gpu.encode(&mut enc, &t512, &u);
                    gpu.submit_and_wait(enc).expect("frame");
                    t0.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            times.sort_by(f64::total_cmp);
            // Throughput: 120 frames back to back (the GPU stays clocked up),
            // best of three runs.
            let through = (0..3)
                .map(|_| {
                    let t0 = std::time::Instant::now();
                    for _ in 0..120 {
                        let mut enc = gpu.device.create_command_encoder(&Default::default());
                        gpu.encode(&mut enc, &t512, &u);
                        gpu.queue.submit([enc.finish()]);
                    }
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                    t0.elapsed().as_secs_f64() * 1000.0 / 120.0
                })
                .fold(f64::INFINITY, f64::min);
            report.push_str(&format!(
                "{:<10} GPU {through:.3} ms/frame back to back · one frame alone (submit+wait, GPU idling between): best {:.3} ms, median {:.3} ms · 512x512 ({name})\n",
                m.name(),
                times[0],
                times[30]
            ));
            a.time += 0.01;
        }
    }
    // The window's stage on a Retina display (about 900 × 460 points) at a few
    // scales, mid-beat with four sounds: what the adaptive resolution chooses from.
    for (w, h) in [(1800u32, 920u32), (1350, 690), (900, 460), (640, 328)] {
        let t = gpu.target(w, h);
        let mut frames = Vec::new();
        ensemble(looks, 1.3, 156, |_, a| frames.push(*a));
        let a = frames.last().copied().unwrap_or_default();
        for m in [Material::Obsidian, Material::Glass] {
            let u = a.uniforms(m, &panel(false, w, h), 0.0);
            let _ = gpu.render_bgra(&t, &u);
            let through = (0..3)
                .map(|_| {
                    let t0 = std::time::Instant::now();
                    for _ in 0..60 {
                        let mut enc = gpu.device.create_command_encoder(&Default::default());
                        gpu.encode(&mut enc, &t, &u);
                        gpu.queue.submit([enc.finish()]);
                    }
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                    t0.elapsed().as_secs_f64() * 1000.0 / 60.0
                })
                .fold(f64::INFINITY, f64::min);
            report.push_str(&format!("{:<10} GPU {through:.3} ms/frame back to back · {w}x{h} ({:.2} MP, four sounds playing)\n", m.name(), (w * h) as f64 / 1e6));
        }
    }
    print!("{report}");
    std::fs::write(dir.join("timing.txt"), report).expect("timing");
}
