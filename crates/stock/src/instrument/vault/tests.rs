use super::params::{self as pp, ep, fm as fmp, ladder as lp, organ as op, poly as pyp, strings as sp};
use super::*;
use crate::instrument::wavetable::p as wp;
use crate::test_util::{BLOCK, SR, render_notes};
use aitrack_dsp::Transport;

fn setp(v: &mut Vault, id: u32, plain: f32) {
    let info = params::all().iter().find(|i| i.id == id).unwrap_or_else(|| panic!("no param {id}"));
    v.set_param(id, info.to_normalized(plain));
}

fn vault(engine: usize) -> Vault {
    let mut v = Vault::new();
    setp(&mut v, pp::ENGINE, engine as f32);
    v.prepare(SR, BLOCK);
    v
}

/// A "plain" patch per engine for pitch tests: one clean oscillator, no
/// drift, chorus or ensemble.
fn plain_patch(engine: usize) -> Vault {
    let mut v = vault(engine);
    match engine {
        engine::LADDER => {
            setp(&mut v, lp::CUTOFF, 20_000.0);
            setp(&mut v, lp::RESONANCE, 0.0);
            setp(&mut v, lp::ENV_AMOUNT, 0.0);
            setp(&mut v, lp::osc(0, lp::WAVE), 2.0);
        }
        engine::POLY => {
            setp(&mut v, pyp::DRIFT, 0.0);
            setp(&mut v, pyp::MIX, 0.0);
            setp(&mut v, pyp::A_WAVE, 2.0);
            setp(&mut v, pyp::CUTOFF, 20_000.0);
            setp(&mut v, pyp::ENV_AMOUNT, 0.0);
        }
        engine::FM => {
            setp(&mut v, fmp::op(1, fmp::LEVEL), 0.0);
            setp(&mut v, fmp::op(0, fmp::SUSTAIN), 100.0);
        }
        engine::ORGAN => {
            for b in 0..9 {
                setp(&mut v, op::drawbar(b), if b == 2 { 8.0 } else { 0.0 });
            }
            setp(&mut v, op::ROTARY, 0.0);
            setp(&mut v, op::CLICK, 0.0);
        }
        engine::STRINGS => {
            setp(&mut v, sp::CELLO, 0.0);
            setp(&mut v, sp::VIOLA, 0.0);
            setp(&mut v, sp::ENSEMBLE, 0.0);
        }
        engine::EP => {
            setp(&mut v, ep::DETUNE, 0.0);
            setp(&mut v, ep::BARK, 0.0);
        }
        _ => {
            setp(&mut v, wt(wp::osc(1, wp::LEVEL)), 0.0);
            setp(&mut v, wt(wp::osc(1, wp::SEMI)), 0.0);
        }
    }
    v
}

/// Fundamental by autocorrelation: the shortest lag that correlates
/// almost as well as the best one, refined by a parabola.
fn pitch(x: &[f32], expect: f32) -> f32 {
    let sr = SR as f32;
    let lo = (sr / (expect * 1.6)) as usize;
    let hi = (sr / (expect / 1.6)) as usize;
    let n = x.len() - hi - 2;
    let corr = |lag: usize| -> f32 {
        let (mut a, mut e0, mut e1) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            a += x[i] as f64 * x[i + lag] as f64;
            e0 += (x[i] as f64).powi(2);
            e1 += (x[i + lag] as f64).powi(2);
        }
        (a / (e0 * e1).sqrt().max(1e-12)) as f32
    };
    let cs: Vec<f32> = (lo..=hi + 1).map(corr).collect();
    let best = cs.iter().cloned().fold(f32::MIN, f32::max);
    let mut k = (1..cs.len() - 1).find(|&k| cs[k] >= 0.97 * best && cs[k] >= cs[k - 1] && cs[k] >= cs[k + 1]).unwrap_or(1);
    k = k.clamp(1, cs.len() - 2);
    let (a, b, c) = (cs[k - 1], cs[k], cs[k + 1]);
    let d = 0.5 * (a - c) / (a - 2.0 * b + c);
    sr / ((lo + k) as f32 + if d.is_finite() { d } else { 0.0 })
}

#[test]
fn every_engine_plays_in_tune() {
    for e in 0..ENGINE_COUNT {
        for key in [45u8, 57, 69] {
            let mut v = plain_patch(e);
            let n = (SR * 0.8) as usize;
            let (l, _) = render_notes(&mut v, &[(0, key, 0.9, true)], n);
            let f = pitch(&l[(SR * 0.3) as usize..], kit::key_hz(key as f32));
            let cents = 1200.0 * (f / kit::key_hz(key as f32)).log2();
            assert!(cents.abs() < 6.0, "{} key {key}: {f:.2} Hz ({cents:+.1} ct)", ENGINES[e]);
        }
    }
}

#[test]
fn fm_ratio_and_detune_move_the_pitch() {
    let mut v = plain_patch(engine::FM);
    setp(&mut v, fmp::op(0, fmp::COARSE), 2.0);
    let (l, _) = render_notes(&mut v, &[(0, 57, 0.9, true)], (SR * 0.6) as usize);
    let f = pitch(&l[(SR * 0.2) as usize..], 440.0);
    assert!((f - 440.0).abs() < 2.0, "ratio 2 on A3 → {f}");
}

#[test]
fn long_silent_tails_have_no_nan_or_denormals() {
    for e in 0..ENGINE_COUNT {
        let mut v = vault(e);
        // Make it ring: resonance, feedback, long-ish release.
        setp(&mut v, lp::RESONANCE, 100.0);
        setp(&mut v, pyp::RESONANCE, 100.0);
        setp(&mut v, fmp::FEEDBACK, 100.0);
        let n = (SR * 12.0) as usize;
        let (l, r) = render_notes(&mut v, &[(0, 40, 1.0, true), (0, 52, 1.0, true), (0, 64, 1.0, true), (12_000, 40, 0.0, false), (12_000, 52, 0.0, false), (12_000, 64, 0.0, false)], n);
        for (i, x) in l.iter().chain(&r).enumerate() {
            assert!(x.is_finite(), "{}: non-finite at {i}", ENGINES[e]);
            assert!(!x.is_subnormal(), "{}: denormal at {i}", ENGINES[e]);
        }
        assert_eq!(v.active_voices(), 0, "{}: voices still active after 12 s", ENGINES[e]);
        let tail = &l[n - 4800..];
        assert!(tail.iter().all(|&x| x == 0.0), "{}: tail not silent", ENGINES[e]);
    }
}

#[test]
fn voice_stealing_keeps_the_newest_notes() {
    for e in [engine::POLY, engine::FM, engine::EP, engine::ORGAN, engine::STRINGS] {
        let mut v = vault(e);
        setp(&mut v, pp::POLYPHONY, 4.0);
        let notes: Vec<(usize, u8, f32, bool)> = (0..7).map(|i| (i * 2000, 50 + i as u8 * 2, 0.8, true)).collect();
        let (l, _) = render_notes(&mut v, &notes, 20_000);
        assert!(l.iter().all(|x| x.is_finite()));
        let mut keys = [0u8; 16];
        let n = v.held_keys(&mut keys);
        let mut held: Vec<u8> = keys[..n].to_vec();
        held.sort();
        assert_eq!(held, vec![56, 58, 60, 62], "{}", ENGINES[e]);
        assert!(v.active_voices() <= 4, "{}: {} voices", ENGINES[e], v.active_voices());
    }
}

#[test]
fn full_polyphony_steals_without_clicks_or_overflow() {
    let mut v = vault(engine::POLY);
    let notes: Vec<(usize, u8, f32, bool)> = (0..40).map(|i| (i * 300, 30 + (i * 7 % 60) as u8, 1.0, true)).collect();
    let (l, r) = render_notes(&mut v, &notes, 20_000);
    assert!(v.active_voices() <= poly::VOICES);
    assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 4.0));
}

#[test]
fn mono_ladder_is_last_note_priority_with_legato() {
    let mut v = vault(engine::LADDER);
    setp(&mut v, lp::LEGATO, 1.0);
    let (_, _) = render_notes(&mut v, &[(0, 48, 0.8, true), (2000, 55, 0.8, true), (4000, 55, 0.0, false)], 6000);
    let mut keys = [0u8; 4];
    assert_eq!(v.held_keys(&mut keys), 1);
    assert_eq!(keys[0], 48, "falls back to the held key");
    assert_eq!(v.active_voices(), 1);
}

#[test]
fn macros_offset_the_mapped_params_only() {
    let mut v = vault(engine::LADDER);
    v.process_block_for_test();
    let base = v.effective(lp::CUTOFF);
    let reso = v.effective(lp::RESONANCE);
    v.set_param(macro_knob(0), 1.0);
    v.process_block_for_test();
    let m = &macros::macros(engine::LADDER)[0];
    let expect: f32 = m.targets.iter().filter(|t| t.param == lp::CUTOFF).map(|t| t.to).sum();
    assert!((v.effective(lp::CUTOFF) - (base + expect).min(1.0)).abs() < 1e-5);
    assert_eq!(v.effective(lp::RESONANCE), reso, "macro 1 must not touch resonance");
    assert_eq!(v.get_param(lp::CUTOFF), base, "base value (what the knob shows) is unchanged");
    // Negative depth inverts the travel.
    setp(&mut v, macro_depth(0), -100.0);
    v.process_block_for_test();
    assert!((v.effective(lp::CUTOFF) - (base - expect).max(0.0)).abs() < 1e-5);
    // Half travel on an exponential curve.
    setp(&mut v, macro_depth(0), 100.0);
    setp(&mut v, macro_knob(1), 50.0);
    v.process_block_for_test();
    let t = macros::macros(engine::LADDER)[1].targets[0];
    assert!((v.effective(t.param) - (v.get_param(t.param) + macros::offset(&t, 0.5, 1.0)).min(1.0)).abs() < 1e-5);
    // The same knob means something else on another engine.
    setp(&mut v, pp::ENGINE, engine::ORGAN as f32);
    v.process_block_for_test();
    assert_eq!(v.effective(lp::CUTOFF), base);
    let rotor = macros::macros(engine::ORGAN)[1].targets[0];
    assert_eq!(rotor.param, op::SPEED);
    assert_eq!(v.effective(op::SPEED), 1.0, "macro 2 at 50 % switches the rotor to fast");
}

#[test]
fn space_macro_is_shared_by_every_engine() {
    for e in 0..ENGINE_COUNT {
        assert_eq!(macros::macros(e)[3].name, "Space");
    }
}

#[test]
fn organ_rotor_ramps_with_inertia() {
    let mut v = vault(engine::ORGAN);
    setp(&mut v, op::SPEED, 1.0);
    render_notes(&mut v, &[(0, 60, 1.0, true)], (SR * 0.3) as usize);
    let (horn, drum) = v.organ.rotor_speeds();
    assert!(horn > 3.0 && horn < 6.7, "horn {horn}");
    assert!(drum < 2.5, "the heavy drum lags: {drum}");
}

#[test]
fn electric_piano_velocity_changes_the_timbre() {
    // Brightness = energy above 2 kHz relative to the total.
    let bright = |vel: f32| {
        let mut v = vault(engine::EP);
        let (l, _) = render_notes(&mut v, &[(0, 57, vel, true)], 4800);
        let mut hp = [aitrack_dsp::svf::OnePole::new(2000.0, SR); 3];
        let (mut hi, mut all) = (0.0f64, 0.0f64);
        for x in &l {
            let h = hp.iter_mut().fold(*x, |y, f| f.highpass(y));
            hi += (h as f64).powi(2);
            all += (*x as f64).powi(2);
        }
        hi / all.max(1e-12)
    };
    let soft = bright(0.25);
    let hard = bright(1.0);
    assert!(hard > soft * 1.5, "soft {soft:.4} hard {hard:.4}");
}

#[test]
fn engine_switch_crossfades_and_releases_the_old_engine() {
    let mut v = vault(engine::POLY);
    render_notes(&mut v, &[(0, 60, 1.0, true)], 4800);
    assert!(v.poly.active() > 0);
    setp(&mut v, pp::ENGINE, engine::FM as f32);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let tr = Transport::default();
    for _ in 0..20 {
        buf.clear();
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        v.process(&mut ctx);
    }
    assert_eq!(v.engine(), engine::FM);
    assert_eq!(v.poly.active(), 0, "old engine reset after the fade");
    assert!(v.prev.is_none());
}

#[test]
fn wavetable_engine_is_hosted() {
    let mut v = vault(engine::WAVETABLE);
    let (l, _) = render_notes(&mut v, &[(0, 60, 0.9, true)], 9600);
    assert!(crate::test_util::peak(&l) > 0.01);
    assert!(v.active_voices() > 0);
    // Its parameters are reachable at WT_BASE + id.
    setp(&mut v, wt(wp::VOLUME), -12.0);
    v.process_block_for_test();
    assert!(v.wt.get_param(wp::VOLUME) < 0.9);
}

#[test]
fn sustain_pedal_defers_note_offs() {
    let mut v = vault(engine::POLY);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let tr = Transport::default();
    let evs = [
        Event { offset: 0, kind: EventKind::ControlChange { channel: 0, cc: 64, value: 1.0 } },
        Event { offset: 1, kind: EventKind::NoteOn { channel: 0, key: 60, velocity: 0.8, note_id: 60 } },
        Event { offset: 10, kind: EventKind::NoteOff { channel: 0, key: 60, velocity: 0.0, note_id: 60 } },
    ];
    let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &evs, transport: &tr };
    v.process(&mut ctx);
    let mut keys = [0u8; 4];
    assert_eq!(v.held_keys(&mut keys), 1, "held by the pedal");
    let up = [Event { offset: 0, kind: EventKind::ControlChange { channel: 0, cc: 64, value: 0.0 } }];
    let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &up, transport: &tr };
    v.process(&mut ctx);
    assert_eq!(v.held_keys(&mut keys), 0);
}

#[test]
fn state_round_trip() {
    let mut v = vault(engine::STRINGS);
    setp(&mut v, sp::TONE, 1234.0);
    setp(&mut v, macro_knob(2), 40.0);
    let s = v.save_state();
    let mut w = Vault::new();
    w.load_state(&s);
    assert_eq!(w.get_param(sp::TONE), v.get_param(sp::TONE));
    assert_eq!(w.get_param(macro_knob(2)), v.get_param(macro_knob(2)));
    assert_eq!(w.get_param(pp::ENGINE), v.get_param(pp::ENGINE));
}

impl Vault {
    fn process_block_for_test(&mut self) {
        let mut buf = AudioBuffer::new(2, BLOCK);
        let tr = Transport::default();
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        self.process(&mut ctx);
    }
}

/// `cargo test -p aitrack-stock --release --lib cpu_per_engine -- --ignored --nocapture`
#[test]
#[ignore]
fn cpu_per_engine() {
    for e in 0..ENGINE_COUNT {
        let mut v = vault(e);
        let n = (SR * 3.0) as usize;
        let notes: Vec<(usize, u8, f32, bool)> = (0..16).map(|i| (i * 7, 40 + i as u8 * 3, 0.8, true)).collect();
        let t = std::time::Instant::now();
        render_notes(&mut v, &notes, n);
        println!("{:<16} 16 voices: {:6.2} % of a core", ENGINES[e], 100.0 * t.elapsed().as_secs_f64() / 3.0);
    }
}

// ── Factory presets ──

#[test]
fn factory_presets_cover_every_engine_and_type() {
    let all = preset_info();
    assert!(all.len() >= 80, "{} Vault presets", all.len());
    for e in 0..ENGINE_COUNT {
        assert!(all.iter().filter(|p| p.engine == e).count() >= 6, "{} has few presets", ENGINES[e]);
    }
    for p in &all {
        assert!(TYPES.contains(&p.kind.as_str()), "{}: type {}", p.path, p.kind);
        assert!(!p.tags.is_empty() && p.tags.iter().all(|t| CHARACTERS.contains(&t.as_str())), "{}: tags {:?}", p.path, p.tags);
        let raw = crate::presets::load(&p.path).unwrap();
        assert!(raw.tags.iter().any(|t| t == ENGINE_TAGS[p.engine]), "{}: no engine tag", p.path);
    }
    let mut names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    let n = names.len();
    names.dedup();
    assert_eq!(names.len(), n, "duplicate preset names");
}

#[test]
fn factory_presets_sit_at_a_common_loudness_without_clipping() {
    let mut failures = Vec::new();
    for info in preset_info() {
        let preset = crate::presets::load(&info.path).unwrap();
        let mut v = Vault::new();
        preset.apply(&mut v);
        let (peak, rms_db) = gain::measure(&mut v, &info.kind);
        let peak_db = 20.0 * peak.max(1e-9).log10();
        if peak_db > gain::PEAK_CEILING_DB + 0.5 {
            failures.push(format!("{}: peak {peak_db:+.1} dBFS", info.path));
        }
        // Peak-limited presets may sit lower; nothing may be much louder.
        let low_ok = peak_db > gain::PEAK_CEILING_DB - 2.0;
        if rms_db > gain::TARGET_RMS_DB + gain::TOLERANCE_DB || (rms_db < gain::TARGET_RMS_DB - gain::TOLERANCE_DB && !low_ok) {
            failures.push(format!("{}: loudness {rms_db:+.1} dBFS (peak {peak_db:+.1})", info.path));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
