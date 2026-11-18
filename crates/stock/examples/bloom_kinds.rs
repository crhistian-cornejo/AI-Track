//! Offline audition of Bloom's tonal kinds: for each kind, the sixteen
//! patches of one seed played in sequence through the device (a phrase that
//! suits the kind: bass line, lead riff, pad chord, pluck arpeggio, key
//! chords, FX hits, texture swells), written as a WAV and a spectrogram PNG,
//! plus a table of measured stats per patch.
//!
//! `cargo run -p aitrack-stock --release --example bloom_kinds -- <out_dir> [seed]`

use aitrack_dsp::Processor;
use aitrack_stock::instrument::bloom::patch::Kind;
use aitrack_stock::instrument::bloom::tonal::{self, Knobs};
use aitrack_stock::instrument::bloom::{self as bloom, Bloom, p, render_device, timbre};
use realfft::RealFftPlanner;
use std::path::PathBuf;

const SR: f64 = 48_000.0;

/// `(frame, key, velocity, on)` notes of a kind's phrase and its length.
fn phrase(kind: Kind) -> (Vec<(usize, u8, f32, bool)>, usize) {
    let s = |t: f64| (t * SR) as usize;
    let mut n = Vec::new();
    let mut note = |at: f64, len: f64, key: u8, vel: f32| {
        n.push((s(at), key, vel, true));
        n.push((s(at + len), key, 0.0, false));
    };
    let total = match kind {
        Kind::Bass => {
            for (i, k) in [36u8, 36, 48, 36, 39, 36, 43, 41].iter().enumerate() {
                note(i as f64 * 0.25, 0.2, *k, if i % 2 == 0 { 0.9 } else { 0.7 });
            }
            2.4
        }
        Kind::Lead => {
            for (i, k) in [67u8, 70, 72, 74, 72, 70, 67].iter().enumerate() {
                note(i as f64 * 0.25, 0.23, *k, 0.85);
            }
            note(1.75, 0.8, 65, 0.85);
            3.2
        }
        Kind::Pad => {
            for k in [57u8, 60, 64] {
                note(0.0, 2.5, k, 0.8);
            }
            4.2
        }
        Kind::Pluck => {
            for (i, k) in [60u8, 64, 67, 72, 67, 64, 60, 64].iter().enumerate() {
                note(i as f64 * 0.18, 0.1, *k, 0.85);
            }
            2.2
        }
        Kind::Keys => {
            for k in [60u8, 64, 67] {
                note(0.0, 0.55, k, 0.9);
            }
            for k in [57u8, 60, 64] {
                note(0.6, 0.55, k, 0.6);
            }
            for k in [53u8, 57, 60, 65] {
                note(1.2, 0.9, k, 0.8);
            }
            3.0
        }
        Kind::Fx => {
            note(0.0, 2.0, 60, 0.9);
            3.6
        }
        Kind::Texture | Kind::Drums => {
            for k in [48u8, 55, 60] {
                note(0.0, 3.0, k, 0.8);
            }
            4.8
        }
    };
    (n, s(total))
}

fn set(b: &mut Bloom, id: u32, plain: f32) {
    let n = b.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
    b.set_param(id, n);
}

/// Stats of one demo note.
struct Stats {
    centroid: f32,
    noise: f32,
    low: f32,
    attack_ms: f32,
    sustain: f32,
    tail_ms: f32,
    peak: f32,
    rms_db: f32,
    motion: f32,
}

fn window_rms(x: &[f32], win: usize) -> Vec<f32> {
    x.chunks(win).map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32).sqrt()).collect()
}

fn stats(kind: Kind, l: &[f32], r: &[f32], sr: f32) -> Stats {
    let (_, hold, _) = bloom::demo_note(kind);
    let mono: Vec<f32> = l.iter().zip(r).map(|(a, b)| (a + b) * 0.5).collect();
    let peak = l.iter().chain(r).fold(0f32, |m, v| m.max(v.abs()));
    let win = (sr * 0.01) as usize;
    let w = window_rms(&mono, win);
    let max = w.iter().fold(0f32, |m, v| m.max(*v)).max(1e-9);
    let attack_ms = w.iter().position(|v| *v >= max * 0.5).unwrap_or(0) as f32 * 10.0;
    let at = |t: f32| w.get((t * 100.0) as usize).copied().unwrap_or(0.0);
    let sustain = at(hold * 0.9) / max;
    let off = (hold * 100.0) as usize;
    let tail = w[off.min(w.len())..].iter().rposition(|v| *v > max * 0.01).unwrap_or(0) as f32 * 10.0;
    let n = mono.len().min((sr * 1.0) as usize);
    let t = timbre::analyze_signal(&mono[..n], sr);
    // Low share: one-pole low-pass at 200 Hz.
    let a = (-std::f32::consts::TAU * 200.0 / sr).exp();
    let mut y = 0.0;
    let (mut lo, mut all) = (0.0f64, 0.0f64);
    for &x in &mono {
        y = a * y + (1.0 - a) * x;
        lo += f64::from(y * y);
        all += f64::from(x * x);
    }
    // Movement: spread of the frame centroids (first-difference estimate).
    let fw = (sr * 0.1) as usize;
    let mut cents = Vec::new();
    for c in mono.chunks(fw) {
        let e: f32 = c.iter().map(|v| v * v).sum();
        if e / (c.len() as f32) > (max * 0.3) * (max * 0.3) {
            let d: f32 = c.windows(2).map(|p| (p[1] - p[0]) * (p[1] - p[0])).sum();
            cents.push(sr / std::f32::consts::TAU * (d / e).sqrt());
        }
    }
    let motion = if cents.len() > 1 { cents.iter().fold(0f32, |m, v| m.max(*v)) / cents.iter().fold(f32::MAX, |m, v| m.min(*v)).max(1.0) } else { 1.0 };
    Stats { centroid: t.centroid, noise: t.noisiness, low: (lo / all.max(1e-20)) as f32, attack_ms, sustain, tail_ms: tail, peak, rms_db: 20.0 * max.log10(), motion }
}

/// Inferno-like colour map, 0..1.
fn colour(t: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 6] = [[0.0, 0.0, 0.02], [0.17, 0.04, 0.33], [0.47, 0.11, 0.43], [0.78, 0.25, 0.27], [0.97, 0.56, 0.04], [0.99, 1.0, 0.64]];
    let t = t.clamp(0.0, 1.0) * 5.0;
    let i = (t as usize).min(4);
    let f = t - i as f32;
    let c = |k: usize| ((STOPS[i][k] + (STOPS[i + 1][k] - STOPS[i][k]) * f) * 255.0) as u8;
    [c(0), c(1), c(2)]
}

fn spectrogram(x: &[f32], sr: f32, marks: &[usize], path: &PathBuf) {
    const N: usize = 2048;
    const H: u32 = 320;
    let width = 1800u32;
    let hop = (x.len() / width as usize).max(1);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let (f_lo, f_hi) = (30.0f32, 16_000.0f32);
    let mut cols = Vec::with_capacity(width as usize);
    let mut gmax = 1e-12f32;
    for c in 0..width as usize {
        let start = c * hop;
        for (i, v) in input.iter_mut().enumerate() {
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos();
            *v = x.get(start + i).copied().unwrap_or(0.0) * w;
        }
        fft.process(&mut input, &mut spec).unwrap();
        let mut col = vec![0f32; H as usize];
        for (row, slot) in col.iter_mut().enumerate() {
            let f = f_lo * (f_hi / f_lo).powf(row as f32 / H as f32);
            let f2 = f_lo * (f_hi / f_lo).powf((row + 1) as f32 / H as f32);
            let (b0, b1) = ((f / sr * N as f32) as usize, ((f2 / sr * N as f32) as usize).max((f / sr * N as f32) as usize + 1));
            let e = spec[b0.min(N / 2)..b1.min(N / 2 + 1)].iter().map(|c| c.norm_sqr()).fold(0f32, f32::max);
            *slot = e;
            gmax = gmax.max(e);
        }
        cols.push(col);
    }
    let mut img = image::RgbImage::new(width, H);
    for (c, col) in cols.iter().enumerate() {
        for (row, e) in col.iter().enumerate() {
            let db = 10.0 * (e / gmax).max(1e-12).log10();
            img.put_pixel(c as u32, H - 1 - row as u32, image::Rgb(colour((db + 90.0) / 90.0)));
        }
    }
    for &m in marks {
        let c = (m / hop) as u32;
        if c < width {
            for y in 0..H {
                if y % 4 < 2 {
                    img.put_pixel(c, y, image::Rgb([90, 200, 255]));
                }
            }
        }
    }
    img.save(path).unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "bloom-kinds".into()));
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1337);
    std::fs::create_dir_all(&out).unwrap();
    println!("seed {seed}");
    println!("{:<8} {:>2} {:<20} {:>7} {:>5} {:>5} {:>6} {:>5} {:>6} {:>5} {:>6} {:>5} {:>5}", "kind", "#", "name", "cent", "noise", "low", "att", "sus", "tail", "peak", "rmsdB", "move", "ph.pk");
    for kind in Kind::TONAL {
        let scene = tonal::generate(seed, kind);
        let names = tonal::scene_names(&scene);
        let mut b = Bloom::new();
        set(&mut b, p::KIND, kind.index() as f32);
        set(&mut b, p::SEED, seed as f32);
        set(&mut b, p::MODE, 1.0);
        let (notes, len) = phrase(kind);
        let gap = (SR * 0.25) as usize;
        let (mut wl, mut wr) = (Vec::new(), Vec::new());
        let mut marks = Vec::new();
        let mut agg = [0f32; 6];
        for (i, name) in names.iter().enumerate() {
            set(&mut b, p::SELECTED, i as f32);
            let (l, r) = render_device(&mut b, &notes, len, SR);
            let ph_peak = l.iter().chain(&r).fold(0f32, |m, v| m.max(v.abs()));
            marks.push(wl.len());
            wl.extend_from_slice(&l);
            wr.extend_from_slice(&r);
            wl.extend(std::iter::repeat_n(0.0, gap));
            wr.extend(std::iter::repeat_n(0.0, gap));
            let (dl, dr) = bloom::render_patch(&scene.patches[i], &Knobs::default(), 0.0, SR);
            let s = stats(kind, &dl, &dr, SR as f32);
            println!(
                "{:<8} {:>2} {:<20} {:>7.0} {:>5.2} {:>5.2} {:>6.0} {:>5.2} {:>6.0} {:>5.2} {:>6.1} {:>5.2} {:>5.2}",
                kind.label(),
                i,
                name,
                s.centroid,
                s.noise,
                s.low,
                s.attack_ms,
                s.sustain,
                s.tail_ms,
                s.peak,
                s.rms_db,
                s.motion,
                ph_peak
            );
            for (a, v) in agg.iter_mut().zip([s.centroid, s.noise, s.low, s.attack_ms, s.sustain, s.tail_ms]) {
                *a += v / 16.0;
            }
        }
        println!("{:<8} mean: cent {:.0} noise {:.2} low {:.2} att {:.0} ms sus {:.2} tail {:.0} ms\n", kind.label(), agg[0], agg[1], agg[2], agg[3], agg[4], agg[5]);
        let name = kind.label().to_ascii_lowercase();
        let spec = hound::WavSpec { channels: 2, sample_rate: SR as u32, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut wav = hound::WavWriter::create(out.join(format!("{name}.wav")), spec).unwrap();
        for (l, r) in wl.iter().zip(&wr) {
            wav.write_sample((l.clamp(-1.0, 1.0) * 32_767.0) as i16).unwrap();
            wav.write_sample((r.clamp(-1.0, 1.0) * 32_767.0) as i16).unwrap();
        }
        wav.finalize().unwrap();
        let mono: Vec<f32> = wl.iter().zip(&wr).map(|(a, b)| (a + b) * 0.5).collect();
        spectrogram(&mono, SR as f32, &marks, &out.join(format!("{name}.png")));
    }
    println!("wrote {}", out.display());
}
