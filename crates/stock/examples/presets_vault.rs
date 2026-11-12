//! Generates the factory presets of instrument.vault.
//! `cargo run -p aitrack-stock --example presets_vault`
//!
//! Each preset is written "macros down" (the base patch) plus the macro
//! values it starts at. Only the global block and the preset's own engine
//! block are stored: the other engines' parameters are inert while that
//! engine plays. The volume of every preset is calibrated here by
//! rendering it ([`vault::gain::measure`]) so the whole library sits near
//! one loudness without clipping; `vault::tests` checks the result.

use aitrack_stock::instrument::vault::params::{self as vp, engine as eng, ep, fm, ladder as ld, organ as og, poly as py, strings as st};
use aitrack_stock::instrument::vault::{self as vault, gain};
use aitrack_stock::instrument::wavetable::{p as wp, src as wsrc};
use aitrack_stock::presets::{Preset, PresetBuilder, factory_dir};

type B = PresetBuilder;

struct P {
    b: B,
    engine: usize,
}

fn new(engine: usize, cat: &str, name: &str, tags: &[&str]) -> P {
    let b = B::new(vault::ID, cat, name).tags(&[vp::ENGINE_TAGS[engine]]).tags(tags).set(vp::ENGINE, engine as f32);
    let mut p = P { b, engine };
    if engine == eng::WAVETABLE {
        // The wavetable synth's oscillator 2 defaults to +7 st.
        p = p.s(vp::wt(wp::osc(1, wp::SEMI)), 0.0).s(vp::wt(wp::osc(1, wp::LEVEL)), 0.0);
    }
    p
}

impl P {
    fn s(mut self, id: u32, v: f32) -> Self {
        self.b = self.b.set(id, v);
        self
    }
    fn all(mut self, kv: &[(u32, f32)]) -> Self {
        for &(id, v) in kv {
            self.b = self.b.set(id, v);
        }
        self
    }
    /// Macro start values (%).
    fn m(self, a: f32, b: f32, c: f32, d: f32) -> Self {
        self.s(vp::macro_knob(0), a).s(vp::macro_knob(1), b).s(vp::macro_knob(2), c).s(vp::macro_knob(3), d)
    }
    /// Reverb % / decay s.
    fn verb(self, mix: f32, decay: f32) -> Self {
        self.s(vp::REVERB_MIX, mix).s(vp::REVERB_DECAY, decay)
    }
    /// Delay % / time index (DELAY_TIMES) / feedback %.
    fn delay(self, mix: f32, time: f32, fb: f32) -> Self {
        self.s(vp::DELAY_MIX, mix).s(vp::DELAY_TIME, time).s(vp::DELAY_FEEDBACK, fb)
    }
    fn wt(self, id: u32, v: f32) -> Self {
        self.s(vp::wt(id), v)
    }
}

// Delay times.
const T_8: f32 = 2.0;
const T_8D: f32 = 3.0;
const T_4: f32 = 5.0;

// ── Mono ladder ──────────────────────────────────────────────────────────

/// Oscillator `n`: wave (0 saw, 1 square, 2 tri, 3 pulse), octave (0..4, 2 = 8'), detune st, level %.
fn lo(p: P, n: u32, wave: f32, oct: f32, det: f32, lvl: f32) -> P {
    p.all(&[(ld::osc(n, ld::WAVE), wave), (ld::osc(n, ld::OCTAVE), oct), (ld::osc(n, ld::DETUNE), det), (ld::osc(n, ld::LEVEL), lvl)])
}

/// Filter: cutoff, reso %, drive %, env %, key %.
fn lf(p: P, cut: f32, reso: f32, drive: f32, env: f32, key: f32) -> P {
    p.all(&[(ld::CUTOFF, cut), (ld::RESONANCE, reso), (ld::DRIVE, drive), (ld::ENV_AMOUNT, env), (ld::KEY_TRACK, key)])
}

fn lfe(p: P, a: f32, d: f32, s: f32, r: f32) -> P {
    p.all(&[(ld::F_ATTACK, a), (ld::F_DECAY, d), (ld::F_SUSTAIN, s), (ld::F_RELEASE, r)])
}

fn lae(p: P, a: f32, d: f32, s: f32, r: f32) -> P {
    p.all(&[(ld::A_ATTACK, a), (ld::A_DECAY, d), (ld::A_SUSTAIN, s), (ld::A_RELEASE, r)])
}

fn ladder() -> Vec<P> {
    let l = eng::LADDER;
    vec![
        lae(lfe(lf(lo(lo(new(l, "Bass", "Rubber Low", &["warm", "fat", "vintage"]), 0, 0.0, 1.0, 0.0, 100.0), 1, 1.0, 0.0, 0.0, 70.0), 320.0, 25.0, 30.0, 45.0, 30.0), 1.0, 260.0, 10.0, 120.0), 1.0, 400.0, 80.0, 90.0).m(20.0, 0.0, 0.0, 0.0),
        lae(lfe(lf(lo(new(l, "Bass", "Acid Squelch", &["aggressive", "bright", "vintage"]), 0, 0.0, 1.0, 0.0, 100.0), 240.0, 82.0, 45.0, 70.0, 20.0), 1.0, 180.0, 0.0, 120.0), 1.0, 300.0, 90.0, 60.0).s(ld::GLIDE, 70.0).s(ld::LEGATO, 1.0).delay(15.0, T_8D, 35.0),
        lae(lfe(lf(lo(lo(new(l, "Bass", "Deep Sub Ladder", &["dark", "clean", "warm"]), 0, 2.0, 1.0, 0.0, 100.0), 1, 1.0, 0.0, 0.0, 35.0), 180.0, 10.0, 20.0, 15.0, 20.0), 1.0, 200.0, 30.0, 100.0), 1.0, 300.0, 100.0, 80.0),
        lae(lfe(lf(lo(lo(new(l, "Bass", "Snap Funk", &["punchy", "bright", "vintage"]), 0, 1.0, 1.0, 0.0, 100.0), 1, 0.0, 1.0, 0.07, 60.0), 260.0, 40.0, 35.0, 75.0, 40.0), 1.0, 140.0, 0.0, 80.0), 1.0, 380.0, 40.0, 70.0),
        lae(lfe(lf(lo(lo(lo(new(l, "Bass", "Growl Drive", &["aggressive", "dirty", "fat"]), 0, 0.0, 1.0, 0.0, 100.0), 1, 0.0, 1.0, 0.12, 90.0), 2, 1.0, 0.0, 0.0, 60.0), 420.0, 55.0, 85.0, 40.0, 30.0), 1.0, 450.0, 30.0, 150.0), 1.0, 500.0, 90.0, 90.0).s(vp::DRIVE, 25.0),
        lae(lfe(lf(lo(lo(new(l, "Bass", "Glide Mono Bass", &["warm", "fat", "clean"]), 0, 0.0, 1.0, 0.0, 100.0), 1, 2.0, 0.0, 0.0, 50.0), 600.0, 20.0, 25.0, 30.0, 40.0), 1.0, 350.0, 40.0, 200.0), 2.0, 500.0, 90.0, 120.0).s(ld::GLIDE, 140.0).s(ld::LEGATO, 1.0),
        lae(lfe(lf(lo(lo(new(l, "Lead", "Hollow Lead", &["warm", "vintage", "soft"]), 0, 1.0, 2.0, 0.0, 100.0), 1, 2.0, 3.0, 0.0, 40.0), 1400.0, 30.0, 25.0, 30.0, 60.0), 5.0, 500.0, 50.0, 300.0), 5.0, 400.0, 90.0, 250.0).s(ld::LFO_PITCH, 18.0).s(ld::LFO_RATE, 5.2).s(ld::GLIDE, 30.0).delay(20.0, T_8D, 40.0).verb(15.0, 2.0),
        lae(lfe(lf(lo(lo(lo(new(l, "Lead", "Screamer", &["aggressive", "bright", "dirty"]), 0, 0.0, 2.0, 0.0, 100.0), 1, 0.0, 2.0, 0.1, 100.0), 2, 0.0, 3.0, -0.1, 70.0), 2200.0, 62.0, 75.0, 35.0, 60.0), 2.0, 400.0, 60.0, 250.0), 2.0, 300.0, 100.0, 200.0).s(ld::GLIDE, 45.0).s(ld::LEGATO, 1.0).s(ld::LFO_PITCH, 12.0).delay(18.0, T_8, 30.0),
        lae(lfe(lf(lo(new(l, "Lead", "Filter Whistle", &["glassy", "clean", "soft"]), 0, 2.0, 2.0, 0.0, 15.0), 600.0, 100.0, 0.0, 0.0, 100.0), 1.0, 300.0, 100.0, 300.0), 20.0, 300.0, 100.0, 400.0).s(ld::LFO_PITCH, 15.0).s(ld::GLIDE, 60.0).verb(25.0, 3.0),
        lae(lfe(lf(lo(lo(new(l, "Lead", "Vintage Solo", &["vintage", "warm", "fat"]), 0, 0.0, 2.0, 0.0, 100.0), 1, 1.0, 2.0, 0.05, 70.0), 1800.0, 35.0, 30.0, 35.0, 50.0), 10.0, 600.0, 55.0, 300.0), 10.0, 400.0, 90.0, 300.0).s(ld::LFO_PITCH, 25.0).s(ld::LFO_RATE, 5.6).s(ld::GLIDE, 40.0).verb(18.0, 2.2),
        lae(lfe(lf(lo(lo(new(l, "Brass", "Mono Brass", &["warm", "vintage", "punchy"]), 0, 0.0, 2.0, 0.0, 100.0), 1, 0.0, 2.0, 0.08, 80.0), 500.0, 15.0, 30.0, 55.0, 50.0), 60.0, 450.0, 45.0, 250.0), 25.0, 300.0, 90.0, 250.0).s(ld::LFO_PITCH, 8.0).verb(15.0, 2.0),
        lae(lfe(lf(lo(lo(new(l, "Pluck", "Ladder Pluck", &["bright", "punchy", "clean"]), 0, 0.0, 2.0, 0.0, 100.0), 1, 1.0, 3.0, 0.0, 40.0), 700.0, 45.0, 20.0, 75.0, 60.0), 1.0, 220.0, 0.0, 200.0), 1.0, 450.0, 0.0, 300.0).delay(25.0, T_8D, 45.0).verb(15.0, 2.0),
        lae(lfe(lf(lo(new(l, "FX", "Laser Zap", &["aggressive", "bright", "metallic"]), 0, 0.0, 2.0, 0.0, 100.0), 120.0, 96.0, 40.0, 100.0, 100.0), 1.0, 220.0, 0.0, 200.0), 1.0, 450.0, 0.0, 300.0).delay(30.0, T_8, 50.0),
        lae(lfe(lf(new(l, "FX", "Noise Sweep", &["evolving", "airy", "dark"]).s(ld::osc(0, ld::LEVEL), 0.0).s(ld::NOISE, 100.0), 300.0, 70.0, 10.0, 60.0, 0.0), 2500.0, 3000.0, 30.0, 2000.0), 800.0, 2000.0, 70.0, 2000.0).s(ld::LFO_RATE, 0.15).s(ld::LFO_CUTOFF, 60.0).verb(40.0, 5.0),
        lae(lfe(lf(lo(lo(lo(new(l, "Bass", "Three Osc Bass", &["fat", "warm", "punchy"]), 0, 0.0, 1.0, 0.0, 100.0), 1, 0.0, 1.0, 0.12, 100.0), 2, 1.0, 0.0, 0.0, 80.0), 380.0, 30.0, 40.0, 50.0, 30.0), 1.0, 300.0, 25.0, 120.0), 1.0, 400.0, 85.0, 100.0),
    ]
}

// ── Poly analog ──────────────────────────────────────────────────────────

/// Osc A wave (0 saw 1 pulse 2 tri), B wave (0 saw 1 pulse 2 tri 3 sine), B semi, B fine ct, mix %.
fn pos(p: P, a: f32, b: f32, semi: f32, fine: f32, mix: f32) -> P {
    p.all(&[(py::A_WAVE, a), (py::B_WAVE, b), (py::B_SEMI, semi), (py::B_FINE, fine), (py::MIX, mix)])
}

fn pf(p: P, cut: f32, reso: f32, env: f32, key: f32) -> P {
    p.all(&[(py::CUTOFF, cut), (py::RESONANCE, reso), (py::ENV_AMOUNT, env), (py::KEY_TRACK, key)])
}

fn pfe(p: P, a: f32, d: f32, s: f32, r: f32) -> P {
    p.all(&[(py::F_ATTACK, a), (py::F_DECAY, d), (py::F_SUSTAIN, s), (py::F_RELEASE, r)])
}

fn pae(p: P, a: f32, d: f32, s: f32, r: f32) -> P {
    p.all(&[(py::A_ATTACK, a), (py::A_DECAY, d), (py::A_SUSTAIN, s), (py::A_RELEASE, r)])
}

fn poly() -> Vec<P> {
    let y = eng::POLY;
    vec![
        pae(pfe(pf(pos(new(y, "Pad", "Velvet Pad", &["warm", "soft", "vintage"]), 0.0, 0.0, 0.0, 8.0, 50.0), 1800.0, 10.0, 20.0, 40.0), 600.0, 1500.0, 60.0, 1500.0), 600.0, 800.0, 90.0, 1500.0).s(py::CHORUS, 1.0).verb(30.0, 3.5).m(0.0, 0.0, 20.0, 20.0),
        pae(pfe(pf(pos(new(y, "Strings", "Dusk Strings", &["warm", "vintage", "wide"]), 1.0, 1.0, 0.0, 6.0, 50.0), 2600.0, 5.0, 15.0, 50.0), 300.0, 1500.0, 70.0, 1200.0), 350.0, 600.0, 90.0, 1200.0).s(py::LFO_PW, 60.0).s(py::LFO_RATE, 0.7).s(py::CHORUS, 3.0).verb(25.0, 3.0),
        pae(pfe(pf(pos(new(y, "Pad", "Glass Poly", &["glassy", "bright", "airy"]), 1.0, 3.0, 12.0, 0.0, 35.0), 5000.0, 20.0, 15.0, 60.0), 5.0, 900.0, 50.0, 800.0), 80.0, 800.0, 80.0, 1400.0).s(py::A_PW, 20.0).s(py::CHORUS, 2.0).verb(35.0, 4.0).delay(15.0, T_4, 40.0),
        pae(pfe(pf(pos(new(y, "Pad", "Slow Bloom", &["evolving", "warm", "soft"]), 0.0, 0.0, 0.0, 10.0, 50.0), 400.0, 20.0, 55.0, 30.0), 2500.0, 3000.0, 70.0, 2500.0), 900.0, 1000.0, 90.0, 2500.0).s(py::CHORUS, 1.0).verb(35.0, 5.0).m(0.0, 0.0, 30.0, 25.0),
        pae(pfe(pf(pos(new(y, "Pad", "Wide Analog Pad", &["wide", "warm", "fat"]), 0.0, 0.0, -12.0, 5.0, 45.0), 2200.0, 10.0, 20.0, 40.0), 400.0, 1500.0, 70.0, 1500.0), 400.0, 1000.0, 90.0, 1800.0).s(py::SPREAD, 100.0).s(py::CHORUS, 2.0).s(py::DRIFT, 50.0).verb(30.0, 4.0),
        pae(pfe(pf(pos(new(y, "Brass", "Poly Brass", &["warm", "vintage", "punchy"]), 0.0, 0.0, 0.0, 7.0, 50.0), 700.0, 10.0, 45.0, 50.0), 80.0, 450.0, 50.0, 300.0), 30.0, 300.0, 90.0, 300.0).s(py::CHORUS, 1.0).verb(15.0, 2.0),
        pae(pfe(pf(pos(new(y, "Brass", "Stab Brass", &["punchy", "bright", "vintage"]), 0.0, 0.0, 0.0, 9.0, 50.0), 900.0, 15.0, 55.0, 50.0), 10.0, 250.0, 20.0, 200.0), 5.0, 350.0, 30.0, 200.0).verb(18.0, 2.0),
        pae(pfe(pf(pos(new(y, "Keys", "Poly Keys", &["warm", "soft", "clean"]), 2.0, 1.0, 12.0, 0.0, 30.0), 1500.0, 10.0, 35.0, 60.0), 2.0, 900.0, 20.0, 400.0), 2.0, 1200.0, 25.0, 450.0).s(py::CHORUS, 1.0).verb(20.0, 2.5),
        pae(pfe(pf(pos(new(y, "Pluck", "Analog Pluck", &["bright", "punchy", "vintage"]), 0.0, 1.0, 0.0, 8.0, 40.0), 600.0, 25.0, 60.0, 60.0), 1.0, 220.0, 0.0, 250.0), 1.0, 450.0, 0.0, 350.0).s(py::CHORUS, 1.0).delay(25.0, T_8D, 40.0).verb(15.0, 2.0),
        pae(pfe(pf(pos(new(y, "Pluck", "Poly-Mod Bell", &["metallic", "glassy", "bright"]), 2.0, 3.0, 19.0, 0.0, 15.0), 6000.0, 5.0, 10.0, 60.0), 1.0, 600.0, 20.0, 600.0), 1.0, 1400.0, 0.0, 1200.0).s(py::PM_OSC_B, 45.0).s(py::PM_FREQ, 1.0).verb(30.0, 3.0).delay(15.0, T_4, 35.0),
        pae(pfe(pf(pos(new(y, "Lead", "Sync Sweep Lead", &["aggressive", "bright", "vintage"]), 0.0, 0.0, -12.0, 0.0, 0.0), 4000.0, 15.0, 20.0, 50.0), 1.0, 800.0, 30.0, 300.0), 2.0, 300.0, 90.0, 250.0).s(py::SYNC, 1.0).s(py::PM_ENV, 55.0).s(py::PM_FREQ, 1.0).s(vp::POLYPHONY, 1.0).s(py::GLIDE, 40.0).delay(18.0, T_8D, 35.0),
        pae(pfe(pf(pos(new(y, "Bass", "Poly Bass", &["fat", "warm", "punchy"]), 0.0, 1.0, -12.0, 0.0, 45.0), 350.0, 20.0, 45.0, 30.0), 1.0, 280.0, 20.0, 150.0), 1.0, 400.0, 80.0, 120.0).s(vp::POLYPHONY, 1.0).s(py::DRIFT, 10.0),
        pae(pfe(pf(pos(new(y, "FX", "Poly-Mod Chaos", &["aggressive", "metallic", "evolving"]), 0.0, 0.0, 7.0, 0.0, 30.0), 1200.0, 50.0, 40.0, 40.0), 1500.0, 2500.0, 40.0, 1500.0), 300.0, 1500.0, 80.0, 1500.0).s(py::PM_OSC_B, 75.0).s(py::PM_FILTER, 1.0).s(py::LFO_CUTOFF, 30.0).s(py::LFO_RATE, 0.3).verb(35.0, 4.0),
        pae(pfe(pf(pos(new(y, "Pad", "Drift Choir", &["evolving", "airy", "vintage"]), 2.0, 2.0, 12.0, 4.0, 40.0), 1600.0, 25.0, 15.0, 40.0), 800.0, 2000.0, 70.0, 2000.0), 1000.0, 1200.0, 90.0, 2200.0).s(py::DRIFT, 100.0).s(py::LFO_PITCH, 6.0).s(py::CHORUS, 3.0).verb(40.0, 5.0),
        pae(pfe(pf(pos(new(y, "Strings", "Unison Strings", &["wide", "bright", "warm"]), 0.0, 0.0, 0.0, 12.0, 50.0), 3500.0, 5.0, 10.0, 50.0), 300.0, 1000.0, 80.0, 1000.0), 250.0, 600.0, 95.0, 1000.0).s(py::SPREAD, 90.0).s(py::CHORUS, 3.0).verb(25.0, 2.5),
        pae(pfe(pf(pos(new(y, "Keys", "Pulse Clavi", &["bright", "punchy", "dirty"]), 1.0, 1.0, 0.0, 3.0, 30.0), 2500.0, 30.0, 40.0, 70.0), 1.0, 300.0, 10.0, 150.0), 1.0, 600.0, 10.0, 120.0).s(py::A_PW, 12.0).s(py::B_PW, 25.0).s(py::VEL_FILTER, 60.0),
        pae(pfe(pf(pos(new(y, "Lead", "Soft Saw Lead", &["warm", "soft", "vintage"]), 0.0, 0.0, 0.0, 6.0, 50.0), 2000.0, 15.0, 20.0, 60.0), 20.0, 600.0, 60.0, 300.0), 15.0, 500.0, 90.0, 300.0).s(vp::POLYPHONY, 1.0).s(py::GLIDE, 60.0).s(py::LFO_PITCH, 15.0).s(py::LFO_RATE, 5.0).delay(20.0, T_8D, 40.0).verb(20.0, 2.5),
    ]
}

// ── FM ───────────────────────────────────────────────────────────────────

/// Operator `n`: coarse (0 = ½), fine %, level 0..99, A, D ms, S %, R ms.
#[allow(clippy::too_many_arguments)]
fn op(p: P, n: u32, coarse: f32, fine: f32, level: f32, a: f32, d: f32, s: f32, r: f32) -> P {
    p.all(&[
        (fm::op(n, fm::COARSE), coarse),
        (fm::op(n, fm::FINE), fine),
        (fm::op(n, fm::LEVEL), level),
        (fm::op(n, fm::ATTACK), a),
        (fm::op(n, fm::DECAY), d),
        (fm::op(n, fm::SUSTAIN), s),
        (fm::op(n, fm::RELEASE), r),
    ])
}

fn fmv(name: &str, cat: &str, tags: &[&str], alg: f32) -> P {
    // Start from silence on every operator but op 1.
    let mut p = new(eng::FM, cat, name, tags).s(fm::ALGORITHM, alg);
    for n in 1..6 {
        p = p.s(fm::op(n, fm::LEVEL), 0.0);
    }
    p
}

fn fm_presets() -> Vec<P> {
    vec![
        // Two stacks: 2→1 body, 4→3 tine.
        op(op(op(op(fmv("Tine FM Piano", "Keys", &["bright", "clean", "vintage"], 2.0), 0, 1.0, 0.0, 99.0, 1.0, 3500.0, 0.0, 400.0), 1, 1.0, 0.0, 74.0, 1.0, 2500.0, 0.0, 400.0), 2, 1.0, 0.0, 85.0, 1.0, 1200.0, 0.0, 300.0), 3, 14.0, 0.0, 66.0, 1.0, 180.0, 0.0, 200.0)
            .s(fm::op(1, fm::VEL), 80.0)
            .s(fm::op(3, fm::VEL), 90.0)
            .s(fm::LFO_AMP, 10.0)
            .s(fm::LFO_RATE, 4.0)
            .verb(20.0, 2.0),
        op(op(op(fmv("Glass Keys", "Keys", &["glassy", "bright", "airy"], 8.0), 0, 1.0, 0.0, 99.0, 1.0, 2500.0, 10.0, 600.0), 1, 3.0, 0.0, 72.0, 1.0, 1200.0, 10.0, 500.0), 2, 1.0, 0.0, 90.0, 1.0, 3000.0, 20.0, 800.0).s(fm::op(1, fm::VEL), 70.0).verb(30.0, 3.0).delay(15.0, T_4, 35.0),
        op(op(op(fmv("FM Clav", "Keys", &["bright", "punchy", "dirty"], 0.0), 0, 1.0, 0.0, 99.0, 1.0, 500.0, 0.0, 120.0), 1, 3.0, 0.0, 80.0, 1.0, 300.0, 10.0, 100.0), 2, 1.0, 0.0, 70.0, 1.0, 200.0, 0.0, 100.0).s(fm::FEEDBACK, 20.0),
        op(op(fmv("FM Slap Bass", "Bass", &["punchy", "bright", "aggressive"], 0.0), 0, 0.0, 0.0, 99.0, 1.0, 700.0, 40.0, 120.0), 1, 0.0, 0.0, 84.0, 1.0, 180.0, 20.0, 100.0).s(fm::FEEDBACK, 35.0).s(fm::op(1, fm::VEL), 80.0),
        op(op(fmv("Wood Bass", "Bass", &["warm", "clean", "soft"], 0.0), 0, 0.0, 0.0, 99.0, 2.0, 900.0, 30.0, 150.0), 1, 1.0, 0.0, 72.0, 1.0, 400.0, 10.0, 120.0),
        op(op(fmv("Wood Mallet", "Pluck", &["warm", "clean", "punchy"], 0.0), 0, 1.0, 0.0, 99.0, 1.0, 600.0, 0.0, 300.0), 1, 4.0, 0.0, 78.0, 1.0, 70.0, 0.0, 80.0).verb(20.0, 2.0),
        op(op(op(fmv("Kalimba Drops", "Pluck", &["glassy", "soft", "airy"], 2.0), 0, 1.0, 0.0, 99.0, 1.0, 900.0, 0.0, 600.0), 1, 6.0, 0.0, 70.0, 1.0, 120.0, 0.0, 100.0), 2, 2.0, 0.0, 70.0, 1.0, 500.0, 0.0, 400.0).delay(25.0, T_8D, 40.0).verb(25.0, 2.5),
        op(op(op(fmv("Tubular Chime", "FX", &["metallic", "bright", "glassy"], 0.0), 0, 1.0, 0.0, 99.0, 1.0, 6000.0, 0.0, 3000.0), 1, 3.0, 50.0, 80.0, 1.0, 4000.0, 0.0, 2500.0), 2, 1.0, 41.0, 60.0, 1.0, 3000.0, 0.0, 2000.0).verb(35.0, 4.0),
        op(op(op(fmv("Metal Hit", "FX", &["metallic", "aggressive", "dirty"], 0.0), 0, 1.0, 0.0, 99.0, 1.0, 1500.0, 0.0, 800.0), 1, 2.0, 41.0, 90.0, 1.0, 800.0, 0.0, 500.0), 2, 5.0, 17.0, 88.0, 1.0, 400.0, 0.0, 300.0).s(fm::FEEDBACK, 70.0).s(fm::PITCH_ENV, 7.0).s(fm::PITCH_DECAY, 40.0).verb(25.0, 2.5),
        op(op(fmv("FM Brass", "Brass", &["bright", "vintage", "punchy"], 0.0), 0, 1.0, 0.0, 99.0, 40.0, 600.0, 85.0, 250.0), 1, 1.0, 0.0, 82.0, 60.0, 500.0, 70.0, 250.0).s(fm::FEEDBACK, 30.0).s(fm::LFO_PITCH, 8.0).verb(15.0, 2.0),
        // Op 6 alone with heavy self-feedback: a bright, saw-like lead.
        op(fmv("Feedback Lead", "Lead", &["aggressive", "bright", "dirty"], 9.0), 5, 1.0, 0.0, 99.0, 2.0, 500.0, 90.0, 250.0).s(fm::op(0, fm::LEVEL), 0.0).s(fm::FEEDBACK, 75.0).s(fm::LFO_PITCH, 15.0).delay(20.0, T_8D, 35.0),
        op(op(op(op(fmv("Crystal Pad", "Pad", &["glassy", "evolving", "airy"], 9.0), 0, 1.0, 0.0, 95.0, 1200.0, 3000.0, 80.0, 2500.0), 1, 2.0, 1.0, 85.0, 1600.0, 3000.0, 70.0, 2500.0), 2, 4.0, 0.0, 78.0, 2000.0, 3000.0, 60.0, 2500.0), 3, 1.0, 0.5, 92.0, 1000.0, 3000.0, 80.0, 2500.0)
            .s(fm::op(1, fm::DETUNE), 5.0)
            .s(fm::op(3, fm::DETUNE), -6.0)
            .s(fm::LFO_PITCH, 6.0)
            .verb(40.0, 5.0)
            .m(0.0, 0.0, 0.0, 20.0),
        op(op(op(op(fmv("FM Organ", "Organ", &["clean", "bright", "vintage"], 9.0), 0, 0.0, 0.0, 95.0, 2.0, 100.0, 100.0, 60.0), 1, 1.0, 0.0, 99.0, 2.0, 100.0, 100.0, 60.0), 2, 2.0, 0.0, 90.0, 2.0, 100.0, 100.0, 60.0), 3, 3.0, 0.0, 86.0, 2.0, 100.0, 100.0, 60.0).s(fm::LFO_PITCH, 10.0).s(fm::LFO_RATE, 6.5).verb(20.0, 2.0),
        op(op(op(op(fmv("FM Strings", "Strings", &["warm", "vintage", "soft"], 1.0), 0, 1.0, 0.0, 99.0, 400.0, 2000.0, 90.0, 900.0), 1, 1.0, 0.0, 72.0, 600.0, 2000.0, 80.0, 900.0), 3, 1.0, 0.0, 95.0, 450.0, 2000.0, 90.0, 900.0), 4, 2.0, 0.0, 65.0, 700.0, 2000.0, 80.0, 900.0)
            .s(fm::op(3, fm::DETUNE), 7.0)
            .s(fm::LFO_PITCH, 12.0)
            .s(fm::LFO_RATE, 5.0)
            .verb(30.0, 3.0),
        op(op(op(fmv("Digital Bell Pad", "Pad", &["bright", "evolving", "metallic"], 3.0), 0, 1.0, 0.0, 99.0, 300.0, 4000.0, 60.0, 2500.0), 1, 7.0, 0.0, 70.0, 800.0, 3000.0, 40.0, 2000.0), 4, 1.0, 0.0, 85.0, 600.0, 3000.0, 70.0, 2500.0).s(fm::op(5, fm::LEVEL), 60.0).s(fm::op(5, fm::COARSE), 3.0).verb(40.0, 5.0).delay(15.0, T_4, 40.0),
    ]
}

// ── Electric piano ───────────────────────────────────────────────────────

/// Model (0 tine, 1 reed), hardness, bell, bark, pickup %.
fn epv(name: &str, tags: &[&str], model: f32, hard: f32, bell: f32, bark: f32, pickup: f32) -> P {
    new(eng::EP, "Keys", name, tags).all(&[(ep::MODEL, model), (ep::HARDNESS, hard), (ep::BELL, bell), (ep::BARK, bark), (ep::PICKUP, pickup)])
}

fn trem(p: P, rate: f32, depth: f32, pan: bool) -> P {
    p.all(&[(ep::TREM_RATE, rate), (ep::TREM_DEPTH, depth), (ep::AUTOPAN, if pan { 1.0 } else { 0.0 })])
}

fn epiano() -> Vec<P> {
    vec![
        trem(epv("Suitcase Tine", &["warm", "vintage", "wide"], 0.0, 45.0, 40.0, 35.0, 35.0), 4.5, 45.0, true).verb(15.0, 2.0),
        epv("Stage Tine", &["punchy", "vintage", "bright"], 0.0, 55.0, 45.0, 50.0, 40.0).verb(10.0, 1.8),
        epv("Mellow Tine", &["warm", "soft", "dark"], 0.0, 25.0, 25.0, 15.0, 25.0).s(ep::TONE, 4000.0).verb(18.0, 2.2),
        epv("Bright Bell Tine", &["bright", "glassy", "clean"], 0.0, 65.0, 85.0, 25.0, 50.0).s(ep::TONE, 14_000.0).verb(15.0, 2.0),
        epv("Barking Tine", &["aggressive", "dirty", "punchy"], 0.0, 60.0, 40.0, 85.0, 60.0).s(vp::DRIVE, 20.0),
        epv("Reed Classic", &["vintage", "warm", "punchy"], 1.0, 45.0, 30.0, 35.0, 45.0).s(ep::TONE, 7000.0),
        epv("Reed Grit", &["dirty", "aggressive", "vintage"], 1.0, 60.0, 30.0, 80.0, 60.0).s(vp::DRIVE, 30.0),
        trem(epv("Reed Soft Tremolo", &["soft", "vintage", "warm"], 1.0, 30.0, 20.0, 20.0, 40.0), 5.5, 50.0, false).s(ep::TONE, 5000.0),
        trem(epv("Dream Tine", &["airy", "wide", "soft"], 0.0, 35.0, 55.0, 15.0, 30.0), 2.0, 35.0, true).verb(40.0, 4.0).delay(20.0, T_4, 40.0).m(0.0, 0.0, 0.0, 15.0),
        epv("Lo-Fi Tine", &["dark", "dirty", "vintage"], 0.0, 40.0, 30.0, 40.0, 30.0).all(&[(ep::TONE, 2500.0), (ep::DETUNE, 14.0)]).s(vp::DRIVE, 25.0).verb(15.0, 1.8),
    ]
}

// ── Organ ────────────────────────────────────────────────────────────────

/// Drawbar registration as a 9-digit string ("888000000").
fn bars(name: &str, tags: &[&str], reg: &str) -> P {
    let mut p = new(eng::ORGAN, "Organ", name, tags);
    for (i, c) in reg.chars().enumerate() {
        p = p.s(og::drawbar(i as u32), c.to_digit(10).unwrap_or(0) as f32);
    }
    p
}

fn perc(p: P, third: bool, slow: bool, soft: bool) -> P {
    p.all(&[(og::PERC, 1.0), (og::PERC_HARM, if third { 1.0 } else { 0.0 }), (og::PERC_DECAY, if slow { 1.0 } else { 0.0 }), (og::PERC_VOL, if soft { 0.0 } else { 1.0 })])
}

fn organ() -> Vec<P> {
    vec![
        bars("Full Gospel", &["bright", "fat", "vintage"], "888888888").s(og::ROTARY, 80.0).s(og::VIBRATO, 6.0).s(og::DRIVE, 25.0).m(0.0, 0.0, 0.0, 10.0),
        perc(bars("Jazz Perc", &["clean", "vintage", "punchy"], "888000000"), true, false, false).s(og::ROTARY, 60.0).s(og::VIBRATO, 6.0),
        bars("Rock Drive", &["aggressive", "dirty", "fat"], "888800000").s(og::DRIVE, 70.0).s(og::ROTARY, 80.0).s(og::CLICK, 50.0).m(0.0, 100.0, 0.0, 0.0),
        bars("Ballad Soft", &["soft", "warm", "clean"], "006800000").s(og::CLICK, 10.0).s(og::ROTARY, 60.0).s(og::VIBRATO, 4.0).verb(20.0, 2.5),
        bars("Flute Stops", &["soft", "clean", "airy"], "008400000").s(og::CLICK, 15.0).s(og::ROTARY, 40.0).s(og::DRIVE, 0.0).verb(20.0, 2.5),
        bars("Bright Whistle", &["bright", "glassy", "clean"], "808000008").s(og::ROTARY, 70.0),
        perc(bars("Second Perc", &["punchy", "vintage", "clean"], "838000000"), false, false, true).s(og::ROTARY, 50.0),
        bars("Cathedral", &["airy", "wide", "bright"], "888888888").s(og::ROTARY, 0.0).s(og::DRIVE, 0.0).s(og::CLICK, 5.0).s(og::RELEASE, 400.0).verb(50.0, 6.0),
        perc(bars("Bubble Skank", &["punchy", "vintage", "clean"], "008000000"), true, true, false).s(og::RELEASE, 8.0).s(og::ROTARY, 30.0),
        bars("Chorus Six", &["warm", "wide", "vintage"], "688600000").s(og::VIBRATO, 6.0).s(og::ROTARY, 50.0).s(og::DRIVE, 20.0),
    ]
}

// ── String machine ───────────────────────────────────────────────────────

/// Registers: cello 16', violin 8', viola 4', horn %.
fn sm(name: &str, cat: &str, tags: &[&str], cello: f32, violin: f32, viola: f32, horn: f32) -> P {
    new(eng::STRINGS, cat, name, tags).all(&[(st::CELLO, cello), (st::VIOLIN, violin), (st::VIOLA, viola), (st::HORN, horn)])
}

fn strings() -> Vec<P> {
    vec![
        sm("Classic Ensemble", "Strings", &["vintage", "warm", "wide"], 40.0, 100.0, 50.0, 0.0).verb(25.0, 3.0),
        sm("Dark Cellos", "Strings", &["dark", "warm", "soft"], 100.0, 40.0, 0.0, 0.0).s(st::TONE, 2500.0).verb(20.0, 3.0),
        sm("Bright Violins", "Strings", &["bright", "airy", "vintage"], 0.0, 100.0, 80.0, 0.0).s(st::TONE, 9000.0).s(st::HIGH_PASS, 250.0).verb(25.0, 3.0),
        sm("Slow Swell", "Strings", &["evolving", "warm", "soft"], 50.0, 100.0, 40.0, 0.0).all(&[(st::ATTACK, 1500.0), (st::SWELL, 70.0), (st::RELEASE, 2000.0)]).verb(30.0, 4.0),
        sm("Horn Section", "Brass", &["vintage", "punchy", "warm"], 30.0, 30.0, 0.0, 100.0).all(&[(st::ATTACK, 60.0), (st::RELEASE, 300.0), (st::TONE, 3500.0), (st::ENSEMBLE, 50.0)]).verb(15.0, 2.0),
        sm("Ensemble Pad", "Pad", &["wide", "airy", "evolving"], 60.0, 100.0, 60.0, 20.0).all(&[(st::ATTACK, 800.0), (st::RELEASE, 2500.0), (st::ENS_DEPTH, 90.0)]).verb(40.0, 5.0).m(0.0, 0.0, 0.0, 20.0),
        sm("Airy Choir Strings", "Pad", &["airy", "soft", "dark"], 0.0, 100.0, 50.0, 30.0).all(&[(st::TONE, 3000.0), (st::HIGH_PASS, 400.0), (st::ENS_DEPTH, 100.0), (st::ENS_RATE, 0.4), (st::ATTACK, 600.0)]).verb(40.0, 5.0),
        sm("Lo-Fi Strings", "Strings", &["dirty", "vintage", "dark"], 40.0, 100.0, 40.0, 0.0).all(&[(st::TONE, 2000.0), (st::ENS_RATE, 1.2)]).s(vp::DRIVE, 30.0).verb(20.0, 2.5),
    ]
}

// ── Wavetable ────────────────────────────────────────────────────────────

/// Oscillator `n`: table (0 saw, 1 square, 2 pulse, 3 sine→saw, 4 tri, 5 sine), position %, octave, level %.
fn wo(p: P, n: u32, table: f32, pos: f32, oct: f32, lvl: f32) -> P {
    p.wt(wp::osc(n, wp::TABLE), table).wt(wp::osc(n, wp::POS), pos).wt(wp::osc(n, wp::OCTAVE), oct).wt(wp::osc(n, wp::LEVEL), lvl)
}

fn wu(p: P, n: u32, voices: f32, detune: f32) -> P {
    p.wt(wp::osc(n, wp::UNISON), voices).wt(wp::osc(n, wp::DETUNE), detune).wt(wp::osc(n, wp::SPREAD), 80.0)
}

fn wf(p: P, cut: f32, reso: f32, env: f32) -> P {
    p.wt(wp::CUTOFF, cut).wt(wp::RESONANCE, reso).wt(wp::FILTER_ENV, env).wt(wp::KEY_TRACK, 50.0)
}

fn wa(p: P, a: f32, d: f32, s: f32, r: f32) -> P {
    p.wt(wp::AMP_ATTACK, a).wt(wp::AMP_DECAY, d).wt(wp::AMP_SUSTAIN, s).wt(wp::AMP_RELEASE, r)
}

fn wm(p: P, a: f32, d: f32, s: f32, r: f32) -> P {
    p.wt(wp::MOD_ATTACK, a).wt(wp::MOD_DECAY, d).wt(wp::MOD_SUSTAIN, s).wt(wp::MOD_RELEASE, r)
}

/// Route the wavetable's macro 1 (Vault's "Movement") to `dest` by `amt` %.
fn movement(p: P, dest: u32, amt: f32) -> P {
    p.wt(wp::slot_x(0, wp::M_SRC), wsrc::MACRO1 as f32).wt(wp::slot_x(0, wp::M_DST), dest as f32).wt(wp::slot_x(0, wp::M_AMT), amt)
}

fn wavetable() -> Vec<P> {
    let w = eng::WAVETABLE;
    vec![
        movement(wa(wf(wo(wo(new(w, "Pad", "Table Pad", &["evolving", "warm", "wide"]), 0, 3.0, 30.0, 0.0, 70.0), 1, 4.0, 0.0, 1.0, 40.0), 2500.0, 10.0, 0.0), 900.0, 1000.0, 90.0, 2000.0), wp::osc(0, wp::POS), 60.0).verb(35.0, 4.0).m(0.0, 30.0, 0.0, 20.0),
        movement(wa(wf(wu(wo(new(w, "Lead", "Super Stack", &["bright", "wide", "aggressive"]), 0, 0.0, 0.0, 0.0, 80.0), 0, 7.0, 35.0), 6000.0, 10.0, 0.0), 2.0, 500.0, 90.0, 300.0), wp::osc(0, wp::DETUNE), 40.0).delay(18.0, T_8D, 35.0).verb(20.0, 2.5),
        movement(wm(wa(wf(wo(new(w, "Pluck", "Morph Pluck", &["bright", "punchy", "glassy"]), 0, 3.0, 60.0, 0.0, 80.0), 500.0, 20.0, 50.0), 1.0, 450.0, 0.0, 300.0), 1.0, 220.0, 0.0, 200.0), wp::osc(0, wp::POS), -50.0).delay(25.0, T_8D, 40.0).verb(15.0, 2.0),
        movement(wm(wa(wf(wo(wo(new(w, "Bass", "Growl Table", &["aggressive", "dirty", "fat"]), 0, 3.0, 70.0, -1.0, 80.0), 1, 1.0, 0.0, -1.0, 50.0), 700.0, 35.0, 30.0), 1.0, 400.0, 85.0, 100.0), 1.0, 300.0, 30.0, 150.0), wp::osc(0, wp::POS), 30.0).wt(wp::DRIVE, 40.0).wt(wp::VOICE_MODE, 1.0),
        movement(wa(wf(wo(wo(new(w, "Keys", "Shimmer Keys", &["glassy", "airy", "bright"]), 0, 5.0, 0.0, 0.0, 70.0), 1, 2.0, 30.0, 1.0, 30.0), 8000.0, 0.0, 0.0), 2.0, 1500.0, 30.0, 800.0), wp::osc(1, wp::POS), 50.0).delay(20.0, T_4, 40.0).verb(30.0, 3.0),
        movement(wa(wf(wo(wo(new(w, "FX", "Evolving Texture", &["evolving", "dark", "airy"]), 0, 3.0, 50.0, 0.0, 60.0), 1, 2.0, 50.0, -1.0, 40.0), 1500.0, 30.0, 0.0), 2000.0, 2000.0, 100.0, 3000.0), wp::CUTOFF, 30.0)
            .wt(wp::lfo(0, wp::LFO_RATE), 0.07)
            .wt(wp::slot_x(1, wp::M_SRC), wsrc::LFO1 as f32)
            .wt(wp::slot_x(1, wp::M_DST), wp::osc(0, wp::POS) as f32)
            .wt(wp::slot_x(1, wp::M_AMT), 50.0)
            .verb(45.0, 6.0),
        movement(wa(wf(wo(new(w, "Pluck", "Glass Bells", &["glassy", "metallic", "bright"]), 0, 5.0, 0.0, 1.0, 80.0), 12_000.0, 0.0, 0.0), 1.0, 1200.0, 0.0, 1200.0), wp::osc(0, wp::POS), 40.0).wt(wp::osc(0, wp::WARP), 8.0).wt(wp::osc(0, wp::WARP_AMT), 30.0).verb(30.0, 3.0).delay(15.0, T_4, 40.0),
        movement(wa(wf(wu(wo(new(w, "Lead", "Neon Lead", &["bright", "aggressive", "wide"]), 0, 1.0, 40.0, 0.0, 80.0), 0, 3.0, 20.0), 4000.0, 20.0, 0.0), 2.0, 400.0, 90.0, 250.0), wp::osc(0, wp::POS), 40.0).wt(wp::VOICE_MODE, 2.0).wt(wp::GLIDE, 50.0).delay(20.0, T_8D, 35.0),
    ]
}

/// Keep the globals and the preset's own engine block.
fn trim(preset: &mut Preset, engine: usize) {
    preset.params.retain(|k, _| {
        let id: u32 = k.parse().unwrap_or(u32::MAX);
        match vp::engine_of(id) {
            None => true,
            Some(e) => e == engine,
        }
    });
}

/// Calibrate the volume to the target loudness, under the peak ceiling.
fn calibrate(preset: &mut Preset, kind: &str) -> (f32, f32) {
    let vol_info = vault::param_infos().into_iter().find(|i| i.id == vp::VOLUME).unwrap();
    let key = vp::VOLUME.to_string();
    let mut db = 0.0f32;
    let mut last = (0.0, 0.0);
    for _ in 0..4 {
        preset.params.insert(key.clone(), vol_info.to_normalized(db));
        let mut v = vault::Vault::new();
        preset.apply(&mut v);
        let (peak, rms) = gain::measure(&mut v, kind);
        let peak_db = 20.0 * peak.max(1e-9).log10();
        last = (peak_db, rms);
        let want = (gain::TARGET_RMS_DB - rms).min(gain::PEAK_CEILING_DB - 0.3 - peak_db);
        if want.abs() < 0.2 {
            break;
        }
        db = (db + want).clamp(vol_info.min, vol_info.max);
    }
    last
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let dir = root.join(vault::ID);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    let mut n = 0;
    for p in [ladder(), poly(), fm_presets(), epiano(), organ(), strings(), wavetable()].into_iter().flatten() {
        let engine = p.engine;
        let (cat, mut preset) = p.b.build();
        trim(&mut preset, engine);
        let (peak, rms) = calibrate(&mut preset, &cat);
        let d = dir.join(&cat);
        std::fs::create_dir_all(&d)?;
        let path = d.join(format!("{}.json", preset.name));
        std::fs::write(&path, preset.to_json() + "\n")?;
        println!("{:<10} {:<22} rms {rms:+6.1}  peak {peak:+6.1}  vol {:+5.1} dB", cat, preset.name, vp::all().iter().find(|i| i.id == vp::VOLUME).unwrap().to_plain(preset.params[&vp::VOLUME.to_string()]));
        n += 1;
    }
    eprintln!("{n} presets");
    Ok(())
}
