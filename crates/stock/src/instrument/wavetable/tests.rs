use super::*;
use crate::test_util::{SR, render_notes, rms, set_plain};
use modulation::{CurvePoint, ModSlot};
use realfft::RealFftPlanner;

/// Estimate the fundamental from upward zero crossings.
fn pitch_hz(x: &[f32]) -> f32 {
    let mut crossings = Vec::new();
    for i in 1..x.len() {
        if x[i - 1] <= 0.0 && x[i] > 0.0 {
            let t = x[i - 1] / (x[i - 1] - x[i]);
            crossings.push(i as f32 - 1.0 + t);
        }
    }
    let n = crossings.len();
    assert!(n > 4, "no oscillation");
    (n - 1) as f32 / (crossings[n - 1] - crossings[0]) * SR as f32
}

fn sine_patch() -> WavetableSynth {
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::osc(0, p::TABLE), 5.0);
    s
}

fn hf(x: &[f32]) -> f32 {
    x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>()
}

fn slot(s: &mut WavetableSynth, i: u32, source: u8, dest: u32, amount: f32) {
    set_plain(s, p::slot_x(i, p::M_SRC), source as f32);
    set_plain(s, p::slot_x(i, p::M_DST), dest as f32);
    set_plain(s, p::slot_x(i, p::M_AMT), amount);
}

/// Worst non-harmonic peak relative to the fundamental (dB).
fn alias_db(x: &[f32], hz: f32) -> f32 {
    let n = x.len().next_power_of_two() / 2;
    let x = &x[x.len() - n..];
    let mut w: Vec<f32> = x.iter().enumerate().map(|(i, v)| v * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut out = fft.make_output_vec();
    fft.process(&mut w, &mut out).unwrap();
    let bin = SR as f32 / n as f32;
    let fund = out[(hz / bin).round() as usize].norm();
    let worst = out
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let f = *i as f32 * bin;
            (f - (f / hz).round() * hz).abs() > 60.0 && f > 40.0
        })
        .map(|(_, c)| c.norm())
        .fold(0.0f32, f32::max);
    20.0 * (worst / fund).log10()
}

#[test]
fn plays_the_right_pitch() {
    let mut s = sine_patch();
    let (l, _) = render_notes(&mut s, &[(0, 69, 1.0, true)], 24_000);
    let hz = pitch_hz(&l[4800..]);
    assert!((hz - 440.0).abs() < 1.0, "{hz}");
    assert!(rms(&l[4800..]) > 0.05);
    let mut s = sine_patch();
    set_plain(&mut s, p::osc(0, p::OCTAVE), 1.0);
    set_plain(&mut s, p::osc(0, p::SEMI), -12.0);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 220.0).abs() < 1.0);
}

#[test]
fn oscillator_three_plays() {
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::osc(0, p::LEVEL), 0.0);
    set_plain(&mut s, p::osc(2, p::TABLE), 5.0);
    set_plain(&mut s, p::osc(2, p::LEVEL), 80.0);
    set_plain(&mut s, p::osc(2, p::OCTAVE), -1.0);
    let (l, _) = render_notes(&mut s, &[(0, 69, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 220.0).abs() < 1.0);
}

#[test]
fn note_off_releases_to_silence() {
    let mut s = sine_patch();
    set_plain(&mut s, p::AMP_RELEASE, 50.0);
    let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true), (4800, 60, 0.0, false)], 24_000);
    assert!(rms(&l[2000..4800]) > 0.05);
    assert!(rms(&l[20_000..]) < 1e-4);
    assert_eq!(s.active_voices(), 0);
}

#[test]
fn voices_cap_at_polyphony_and_steal() {
    let mut s = WavetableSynth::new();
    let notes: Vec<(usize, u8, f32, bool)> = (0..24).map(|i| (i * 10, 40 + i as u8, 0.8, true)).collect();
    render_notes(&mut s, &notes, 4800);
    assert_eq!(s.active_voices(), 16);
    // The newest notes survive the stealing.
    let keys: Vec<u8> = s.voices.iter().filter(|v| v.active).map(|v| v.key).collect();
    assert!(keys.contains(&63), "{keys:?}");
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::POLYPHONY, 4.0);
    render_notes(&mut s, &notes, 4800);
    assert_eq!(s.active_voices(), 4);
}

#[test]
fn glide_moves_pitch() {
    let mut s = sine_patch();
    set_plain(&mut s, p::VOICE_MODE, 2.0);
    set_plain(&mut s, p::GLIDE, 200.0);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true), (12_000, 69, 1.0, true)], 48_000);
    let early = pitch_hz(&l[12_200..13_500]);
    let late = pitch_hz(&l[40_000..]);
    assert!(early > 225.0 && early < 400.0, "{early}");
    assert!((late - 440.0).abs() < 2.0, "{late}");
    assert_eq!(s.active_voices(), 1);
}

#[test]
fn filter_darkens_a_saw() {
    let mut bright = WavetableSynth::new();
    let (a, _) = render_notes(&mut bright, &[(0, 48, 1.0, true)], 12_000);
    let mut dark = WavetableSynth::new();
    set_plain(&mut dark, p::CUTOFF, 200.0);
    set_plain(&mut dark, p::FILTER_SLOPE, 1.0);
    let (b, _) = render_notes(&mut dark, &[(0, 48, 1.0, true)], 12_000);
    assert!(hf(&b[2000..]) < 0.05 * hf(&a[2000..]));
}

#[test]
fn unison_spreads_stereo_with_sixteen_voices() {
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::osc(0, p::VOICES), 16.0);
    set_plain(&mut s, p::osc(0, p::SPREAD), 100.0);
    let (l, r) = render_notes(&mut s, &[(0, 60, 1.0, true)], 24_000);
    assert_eq!(s.voices.iter().find(|v| v.active).unwrap().osc[0].n, 16);
    let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
    assert!(diff > 10.0);
    // Equal-power normalisation: 16 voices are about as loud as one.
    let mut one = WavetableSynth::new();
    let (l1, _) = render_notes(&mut one, &[(0, 60, 1.0, true)], 24_000);
    let ratio = rms(&l[4800..]) / rms(&l1[4800..]);
    assert!(ratio > 0.5 && ratio < 1.6, "{ratio}");
}

#[test]
fn legacy_mod_env_pitch_drop_808() {
    let mut s = sine_patch();
    set_plain(&mut s, p::MOD_DECAY, 80.0);
    set_plain(&mut s, p::MOD_SUSTAIN, 0.0);
    set_plain(&mut s, p::slot(0, p::MOD_SRC), 3.0); // Mod Env
    set_plain(&mut s, p::slot(0, p::MOD_DST), 12.0); // Pitch
    set_plain(&mut s, p::slot(0, p::MOD_AMT), 50.0);
    let (l, _) = render_notes(&mut s, &[(0, 45, 1.0, true)], 24_000);
    let start = pitch_hz(&l[0..2400]);
    let end = pitch_hz(&l[16_000..]);
    assert!(start > 125.0, "{start}");
    assert!((end - 110.0).abs() < 1.5, "{end}");
}

#[test]
fn legacy_synced_lfo_on_cutoff_wobbles() {
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::CUTOFF, 300.0);
    set_plain(&mut s, p::lfo(0, p::LFO_SYNC), 1.0);
    set_plain(&mut s, p::lfo(0, p::LFO_DIVISION), 10.0);
    set_plain(&mut s, p::slot(0, p::MOD_SRC), 1.0);
    set_plain(&mut s, p::slot(0, p::MOD_DST), 5.0);
    set_plain(&mut s, p::slot(0, p::MOD_AMT), 50.0);
    let (l, _) = render_notes(&mut s, &[(0, 40, 1.0, true)], 48_000);
    let e: Vec<f32> = l.chunks(1200).map(hf).collect();
    let max = e[5..].iter().cloned().fold(0.0, f32::max);
    let min = e[5..].iter().cloned().fold(f32::MAX, f32::min);
    assert!(max > 4.0 * min, "{max} {min}");
}

#[test]
fn matrix_routes_any_source_to_any_parameter() {
    // Macro 1 → cutoff (normalised: 100 % = the whole knob).
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::CUTOFF, 100.0);
    let (closed, _) = render_notes(&mut s, &[(0, 48, 1.0, true)], 12_000);
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::CUTOFF, 100.0);
    set_plain(&mut s, p::macro_knob(0), 100.0);
    slot(&mut s, 0, src::MACRO1, p::CUTOFF, 80.0);
    let (open, _) = render_notes(&mut s, &[(0, 48, 1.0, true)], 12_000);
    assert!(hf(&open[2000..]) > 20.0 * hf(&closed[2000..]));

    // Virtual pitch: constant × 50 % = +12 st.
    let mut s = sine_patch();
    slot(&mut s, 1, src::CONSTANT, vdst::PITCH, 50.0);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 440.0).abs() < 1.5);

    // Via: the mod wheel scales a constant pitch offset; wheel at 0 → none.
    let mut s = sine_patch();
    slot(&mut s, 2, src::CONSTANT, vdst::PITCH, 50.0);
    set_plain(&mut s, p::slot_x(2, p::M_VIA), src::MOD_WHEEL as f32);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 220.0).abs() < 1.0);

    // Bipolar velocity on oscillator level: soft notes are quieter than unity.
    let mut s = sine_patch();
    set_plain(&mut s, p::VEL_SENS, 0.0);
    slot(&mut s, 3, src::VELOCITY, p::osc(0, p::LEVEL), 80.0);
    set_plain(&mut s, p::slot_x(3, p::M_MODE), Polarity::Bipolar.index() as f32);
    let (soft, _) = render_notes(&mut s, &[(0, 60, 0.1, true)], 12_000);
    let mut s2 = sine_patch();
    set_plain(&mut s2, p::VEL_SENS, 0.0);
    let (plain, _) = render_notes(&mut s2, &[(0, 60, 0.1, true)], 12_000);
    assert!(rms(&soft[4000..]) < 0.5 * rms(&plain[4000..]));
}

#[test]
fn matrix_api_round_trips_through_params() {
    let mut s = WavetableSynth::new();
    let slots = ModSlot::read_all(|id| crate::test_util::get_plain(&s, id));
    let (i, edits) = ModSlot::assign(&slots, src::LFO2, p::filt(1, p::F_CUTOFF), -0.4).unwrap();
    for (id, v) in edits {
        set_plain(&mut s, id, v);
    }
    let back = ModSlot::read(i, |id| crate::test_util::get_plain(&s, id));
    assert_eq!(back.source, src::LFO2);
    assert_eq!(back.dest, p::filt(1, p::F_CUTOFF));
    assert!((back.amount + 0.4).abs() < 1e-4);
    assert_eq!(dest_name(back.dest), "Filter 2 Cutoff");
    assert_eq!(dest_name(vdst::AMP), "Amp");
}

#[test]
fn synth_output_is_alias_free_at_high_notes() {
    // Saw at C7 (2093 Hz) and C8 (4186 Hz), filter open, no FX.
    for key in [96u8, 108] {
        let hz = key_to_hz(key as f32);
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::VEL_SENS, 0.0);
        let (l, _) = render_notes(&mut s, &[(0, key, 1.0, true)], 40_000);
        let db = alias_db(&l[6000..], hz);
        assert!(db < -65.0, "key {key}: alias {db} dB");
    }
    // The bank tables too (formants + noise spectra are the densest).
    for wave in [3usize, 12] {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::osc(0, p::WAVE), wave as f32);
        set_plain(&mut s, p::osc(0, p::POS), 60.0);
        let (l, _) = render_notes(&mut s, &[(0, 100, 1.0, true)], 40_000);
        let db = alias_db(&l[6000..], key_to_hz(100.0));
        assert!(db < -60.0, "wave {wave}: alias {db} dB");
    }
}

#[test]
fn every_warp_mode_changes_the_tone_and_stays_finite() {
    let base = {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::osc(1, p::LEVEL), 60.0);
        render_notes(&mut s, &[(0, 48, 1.0, true)], 9600).0
    };
    for mode in 1..WARPS.len() {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::osc(1, p::LEVEL), 60.0);
        set_plain(&mut s, p::osc(0, p::WARP), mode as f32);
        set_plain(&mut s, p::osc(0, p::WARP_AMT), 70.0);
        let (l, _) = render_notes(&mut s, &[(0, 48, 1.0, true)], 9600);
        assert!(l.iter().all(|v| v.is_finite()), "{}", WARPS[mode]);
        let d: f32 = l[3000..].iter().zip(&base[3000..]).map(|(a, b)| (a - b).abs()).sum();
        assert!(d > 1.0, "{} did nothing ({d})", WARPS[mode]);
    }
}

#[test]
fn wavetable_position_morphs() {
    let mut a = WavetableSynth::new();
    set_plain(&mut a, p::osc(0, p::WAVE), 1.0); // Basic Shapes: sine … square
    let (sine, _) = render_notes(&mut a, &[(0, 48, 1.0, true)], 12_000);
    let mut b = WavetableSynth::new();
    set_plain(&mut b, p::osc(0, p::WAVE), 1.0);
    set_plain(&mut b, p::osc(0, p::POS), 100.0);
    let (square, _) = render_notes(&mut b, &[(0, 48, 1.0, true)], 12_000);
    assert!(hf(&square[2000..]) > 10.0 * hf(&sine[2000..]));
}

#[test]
fn filter_routing_serial_parallel_and_direct() {
    // Osc 1 routed Direct ignores a closed filter.
    let mut closed = WavetableSynth::new();
    set_plain(&mut closed, p::CUTOFF, 60.0);
    let (a, _) = render_notes(&mut closed, &[(0, 48, 1.0, true)], 12_000);
    let mut direct = WavetableSynth::new();
    set_plain(&mut direct, p::CUTOFF, 60.0);
    set_plain(&mut direct, p::osc(0, p::ROUTE), 3.0);
    let (b, _) = render_notes(&mut direct, &[(0, 48, 1.0, true)], 12_000);
    assert!(hf(&b[2000..]) > 50.0 * hf(&a[2000..]));
    // Serial LP → HP: band; parallel LP + HP: both ends survive.
    let setup = |parallel: bool| {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::CUTOFF, 300.0);
        set_plain(&mut s, p::filt(1, p::F_ENABLE), 1.0);
        set_plain(&mut s, p::filt(1, p::F_TYPE), 1.0);
        set_plain(&mut s, p::filt(1, p::F_CUTOFF), 3000.0);
        set_plain(&mut s, p::FILTER_ROUTING, if parallel { 1.0 } else { 0.0 });
        set_plain(&mut s, p::osc(0, p::ROUTE), 2.0);
        render_notes(&mut s, &[(0, 36, 1.0, true)], 12_000).0
    };
    let serial = setup(false);
    let parallel = setup(true);
    assert!(rms(&parallel[3000..]) > 2.0 * rms(&serial[3000..]), "{} {}", rms(&parallel[3000..]), rms(&serial[3000..]));
}

#[test]
fn second_filter_models_run() {
    for model in 0..filter::MODELS.len() {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::filt(1, p::F_ENABLE), 1.0);
        set_plain(&mut s, p::filt(1, p::F_MODEL), model as f32);
        set_plain(&mut s, p::filt(1, p::F_CUTOFF), 800.0);
        set_plain(&mut s, p::filt(1, p::F_RESO), 60.0);
        set_plain(&mut s, p::osc(0, p::ROUTE), 1.0);
        let (l, _) = render_notes(&mut s, &[(0, 48, 1.0, true)], 9600);
        assert!(l.iter().all(|v| v.is_finite() && v.abs() < 4.0), "{}", filter::MODELS[model]);
        assert!(rms(&l[3000..]) > 1e-3, "{} silent", filter::MODELS[model]);
    }
}

#[test]
fn noise_colours_tilt() {
    let render = |color: f32| {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::osc(0, p::LEVEL), 0.0);
        set_plain(&mut s, p::NOISE, 100.0);
        set_plain(&mut s, p::NOISE_COLOR, color);
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true)], 12_000);
        hf(&l[2000..]) / l[2000..].iter().map(|v| v * v).sum::<f32>()
    };
    let (white, pink, brown, violet) = (render(0.0), render(1.0), render(2.0), render(4.0));
    assert!(brown < pink && pink < white && white < violet, "{brown} {pink} {white} {violet}");
}

#[test]
fn sub_oscillator_and_drift() {
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::osc(0, p::LEVEL), 0.0);
    set_plain(&mut s, p::SUB_LEVEL, 100.0);
    let (l, _) = render_notes(&mut s, &[(0, 69, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 220.0).abs() < 1.0);
    let mut s = sine_patch();
    set_plain(&mut s, p::DRIFT, 100.0);
    let (l, _) = render_notes(&mut s, &[(0, 69, 1.0, true)], 24_000);
    let hz = pitch_hz(&l[4800..]);
    assert!((hz - 440.0).abs() < 4.0, "{hz}");
}

#[test]
fn imported_clm_table_plays() {
    // Synthetic Serum-style WAV: 8 frames of 2048 (sine → sine at 2× pitch).
    let mut samples = Vec::new();
    for f in 0..8 {
        let mult = if f < 4 { 1.0 } else { 2.0 };
        samples.extend((0..2048).map(|i| (std::f32::consts::TAU * mult * i as f32 / 2048.0).sin() * 0.9));
    }
    let bytes = wav::tests::pcm16(&samples, Some((b"clm ", b"<!>2048 10000000 wavetable (www.xferrecords.com)".to_vec())));
    let frames = wav::import(&bytes, "Test").unwrap();
    assert_eq!(frames.len(), 8);
    let mut s = WavetableSynth::new();
    s.set_user_table(0, Some(frames.clone()));
    set_plain(&mut s, p::osc(0, p::WAVE), USER_WAVE as f32);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 220.0).abs() < 1.0);
    set_plain(&mut s, p::osc(0, p::POS), 100.0);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 440.0).abs() < 1.5);
    // State round trip keeps the table.
    let state = s.save_state();
    let mut t = WavetableSynth::new();
    t.load_state(&state);
    assert_eq!(t.user_table(0).map(|w| w.len()), Some(8));
    let (l, _) = render_notes(&mut t, &[(0, 57, 1.0, true)], 24_000);
    assert!((pitch_hz(&l[4800..]) - 440.0).abs() < 1.5);
}

#[test]
fn remote_swaps_tables_and_curves_while_running() {
    let mut s = WavetableSynth::new();
    set_plain(&mut s, p::osc(0, p::WAVE), USER_WAVE as f32);
    let remote = s.remote();
    // No user table yet → the Table choice (saw) plays.
    let (saw, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 9600);
    let sine = WaveFrames::single("Sine", (0..2048).map(|i| (std::f32::consts::TAU * i as f32 / 2048.0).sin()).collect());
    remote.post_user_table(0, Some(sine));
    assert!(remote.pending());
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 9600);
    assert!(!remote.pending());
    assert!(hf(&l[3000..]) < 0.1 * hf(&saw[3000..]));
    remote.collect();
    remote.post_lfo_curve(2, LfoCurve::duck());
    render_notes(&mut s, &[], 256);
    assert_eq!(s.lfo_curve(2), LfoCurve::duck());
}

#[test]
fn drawn_and_one_shot_lfos() {
    // A drawn ramp-down LFO, one-shot at 1/4 note (0.5 s at 120 BPM), on
    // pitch: starts +12 st, ends at the key.
    let mut s = sine_patch();
    s.set_lfo_curve(2, LfoCurve::new(&[CurvePoint { x: 0.0, y: 1.0, curve: 0.0 }, CurvePoint { x: 1.0, y: 0.0, curve: 0.0 }]));
    set_plain(&mut s, p::lfo(2, p::LFO_DRAWN), 1.0);
    set_plain(&mut s, p::lfo(2, p::LFO_ONESHOT), 1.0);
    set_plain(&mut s, p::lfo(2, p::LFO_SYNC), 1.0);
    set_plain(&mut s, p::lfo(2, p::LFO_DIVISION), 7.0);
    slot(&mut s, 0, src::LFO3, vdst::PITCH, 50.0);
    set_plain(&mut s, p::slot_x(0, p::M_MODE), Polarity::Unipolar.index() as f32);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 48_000);
    let start = pitch_hz(&l[200..1400]);
    let end = pitch_hz(&l[36_000..]);
    assert!(start > 400.0, "{start}");
    assert!((end - 220.0).abs() < 1.5, "{end}");
}

#[test]
fn envelope_three_and_curves_modulate() {
    let mut s = sine_patch();
    set_plain(&mut s, p::env(2, p::E_ATTACK), 400.0);
    set_plain(&mut s, p::env(2, p::E_SUSTAIN), 100.0);
    slot(&mut s, 0, src::ENV3, vdst::PITCH, 50.0);
    let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 48_000);
    assert!(pitch_hz(&l[100..2000]) < 260.0);
    assert!((pitch_hz(&l[30_000..]) - 440.0).abs() < 1.5);
}

#[test]
fn sustain_pedal_holds_notes() {
    use aitrack_dsp::{AudioBuffer, Event, Transport};
    let mut s = sine_patch();
    s.prepare(SR, 128);
    set_plain(&mut s, p::AMP_RELEASE, 20.0);
    let mut buf = AudioBuffer::new(2, 128);
    let tr = Transport::default();
    let mut run = |s: &mut WavetableSynth, ev: &[Event]| {
        buf.clear();
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: ev, transport: &tr };
        s.process(&mut ctx);
    };
    run(&mut s, &[Event { offset: 0, kind: EventKind::ControlChange { channel: 0, cc: 64, value: 1.0 } }, Event { offset: 1, kind: EventKind::NoteOn { channel: 0, key: 60, velocity: 1.0, note_id: 1 } }]);
    run(&mut s, &[Event { offset: 0, kind: EventKind::NoteOff { channel: 0, key: 60, velocity: 0.0, note_id: 1 } }]);
    for _ in 0..40 {
        run(&mut s, &[]);
    }
    assert_eq!(s.active_voices(), 1);
    run(&mut s, &[Event { offset: 0, kind: EventKind::ControlChange { channel: 0, cc: 64, value: 0.0 } }]);
    for _ in 0..40 {
        run(&mut s, &[]);
    }
    assert_eq!(s.active_voices(), 0);
}

#[test]
fn built_in_fx_change_the_sound() {
    let base = render_notes(&mut WavetableSynth::new(), &[(0, 48, 1.0, true)], 9600).0;
    for (id, v) in [(p::DIST_MODE, 1.0), (p::DIST_MODE, 3.0), (p::DIST_MODE, 5.0), (p::EQ_HIGH, -12.0), (p::EQ_LOW, 12.0)] {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, id, v);
        set_plain(&mut s, p::DIST_DRIVE, 80.0);
        let (l, _) = render_notes(&mut s, &[(0, 48, 1.0, true)], 9600);
        assert!(l.iter().all(|x| x.is_finite()));
        let d: f32 = l[3000..].iter().zip(&base[3000..]).map(|(a, b)| (a - b).abs()).sum();
        assert!(d > 1.0, "param {id} = {v}: {d}");
    }
}

#[test]
fn defaults_keep_the_original_patch() {
    // Old projects carry only ids 0–84: the new blocks must be silent or
    // neutral by default.
    let s = WavetableSynth::new();
    let c = &s.cache;
    assert_eq!(c.plain(p::osc(2, p::LEVEL)), 0.0);
    assert!(!c.on(p::filt(1, p::F_ENABLE)) && c.on(p::filt(0, p::F_ENABLE)));
    assert_eq!(s.s.osc[0].unison, 1);
    assert_eq!(s.s.osc[0].wave, 0);
    assert_eq!(s.mx.n, 0);
    assert!(!s.fx_settings.active());
}

#[test]
fn meter_reports_voices_lfos_and_envs() {
    let mut s = WavetableSynth::new();
    render_notes(&mut s, &[(0, 60, 1.0, true)], 4800);
    let mut out = [0.0f32; 16];
    let n = s.meter(&mut out);
    assert_eq!(n, 3 + OSCS + LFOS + ENVS);
    assert_eq!(out[0], 1.0);
    assert!(out[3 + OSCS + LFOS] > 0.5, "env 1 {}", out[3 + OSCS + LFOS]);
}

#[test]
fn base64_round_trip() {
    for len in 0..10 {
        let data: Vec<u8> = (0..len as u8).map(|i| i.wrapping_mul(37)).collect();
        assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data);
    }
    assert!(b64_decode("a$").is_none());
}
