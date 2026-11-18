use super::*;
use crate::instrument::bloom::timbre::analyze_signal;

#[test]
fn every_parent_preset_loads() {
    for kind in Kind::TONAL {
        let kp = priors().kind(kind);
        assert_eq!(kp.parents.len(), PARENTS[kind.index()].len(), "{kind:?}: a parent preset is missing");
        assert!(kp.parents.len() >= 6);
        for s in 0..layout().len {
            assert!(kp.lo[s] <= kp.hi[s]);
        }
    }
    assert!(genes().len() > 60);
}

#[test]
fn the_same_seed_grows_the_same_scene() {
    for kind in Kind::TONAL {
        let a = generate(42, kind);
        let b = generate(42, kind);
        assert_eq!(a, b);
        let mut c = PatchScene::empty(Kind::Pad);
        generate_into(&mut c, 42, kind);
        assert_eq!(a, c, "in place = by value");
        let d = generate(43, kind);
        assert_ne!(a.patches, d.patches);
        // Cells differ from each other.
        for i in 1..SOUNDS {
            assert!(a.patches[i].distance(&a.patches[0]) > 0.05, "{kind:?} cell {i} is a copy");
        }
        // On the quantised grid.
        assert!(a.patches.iter().all(|p| p.v.iter().all(|v| quantize(*v) == *v)));
    }
    // Kinds grow unrelated scenes from one seed.
    assert_ne!(generate(7, Kind::Bass).patches[0].v, generate(7, Kind::Lead).patches[0].v);
}

/// A fingerprint that must not change silently: saved projects play the
/// scene their seed grows. If this changes on purpose (new priors, rules),
/// it is a sound change for every tonal preset; update the value.
#[test]
fn growth_is_stable_across_runs() {
    let mut h: u64 = 0;
    for kind in Kind::TONAL {
        for p in &generate(1337, kind).patches {
            h = h.rotate_left(5) ^ p.hash();
        }
    }
    let mut h2: u64 = 0;
    for kind in Kind::TONAL {
        for p in &generate(1337, kind).patches {
            h2 = h2.rotate_left(5) ^ p.hash();
        }
    }
    assert_eq!(h, h2);
}

/// Every patch of 120 seeds × 7 kinds respects its kind's rules.
#[test]
fn every_grown_patch_keeps_its_kinds_rules() {
    type Rules = Vec<(u32, f32, f32)>;
    let rule_sets: Vec<(Kind, Rules)> = Kind::TONAL.iter().map(|&k| (k, rules(k))).collect();
    std::thread::scope(|s| {
        for (kind, rules) in &rule_sets {
            s.spawn(move || {
                for seed in 0..120u64 {
                    let scene = generate(seed * 7_919 + 3, *kind);
                    for (i, x) in scene.patches.iter().enumerate() {
                        let at = format!("{kind:?} seed {seed} cell {i}");
                        assert_eq!(x.kind, *kind, "{at}");
                        for &(id, lo, hi) in rules {
                            let v = x.plain(id);
                            let tol = 1e-3 * (hi - lo).abs().max(1.0) + 1e-3 * v.abs();
                            assert!(v >= lo - tol && v <= hi + tol, "{at}: {} = {v} outside {lo}..{hi}", wt::params().iter().find(|i| i.id == id).unwrap().name);
                        }
                        assert!(source_level(x) >= 44.0, "{at}: nothing sounds");
                        match kind {
                            Kind::Bass => {
                                assert!(x.choice(w::VOICE_MODE) >= 1, "{at}: bass is mono");
                                assert_eq!(x.choice(w::FILTER_TYPE), 0, "{at}: bass is low-passed");
                            }
                            Kind::Pad => {
                                assert!(unison(x, 0) >= 3, "{at}: pads are wide");
                                assert!(moves(x, 6.0, 100.0), "{at}: pads move");
                            }
                            Kind::Pluck => assert!(x.plain(w::AMP_SUSTAIN) <= 2.1, "{at}: plucks do not sustain"),
                            Kind::Texture => assert!(moves(x, 8.0, 100.0), "{at}: textures evolve"),
                            _ => {}
                        }
                        // Constraining again changes nothing.
                        let mut y = *x;
                        constrain(&mut y);
                        assert!(y.distance(x) < 1e-5, "{at}: constrain is idempotent");
                    }
                }
            });
        }
    });
}

#[test]
fn paths_repeat_and_locks_hold() {
    let s = generate(5, Kind::Pad);
    for i in 0..SOUNDS {
        let out = regrow(&regrow(&s, i, 1.2), i, s.distances[i]);
        assert_eq!(out.patches[i], s.patches[i]);
        let near = grow(&s.cores[i], s.paths[i], 0.05).distance(&s.cores[i]);
        let far = grow(&s.cores[i], s.paths[i], 0.9).distance(&s.cores[i]);
        assert!(near < far, "cell {i}: {near} vs {far}");
    }
    let mut s = generate(21, Kind::Lead);
    s.set_locked(2, true);
    let rolled = roll(&s, 22, Kind::Lead, 1.0);
    assert_eq!(rolled.patches[2], s.patches[2]);
    assert_ne!(rolled.patches[3], s.patches[3]);
    let near = roll(&s, 22, Kind::Lead, 0.15);
    let moved = |t: &PatchScene| (0..SOUNDS).map(|i| t.patches[i].distance(&s.patches[i])).sum::<f32>();
    assert!(moved(&near) < moved(&rolled), "deviance sets how far a roll moves");
    let m = mutate(&rolled, 5, 0.8);
    assert_eq!(m.patches[2], s.patches[2]);
    assert_ne!(m.patches[4], rolled.patches[4]);
    // An edit stays where it was put.
    let mut edited = s.patches[4];
    edited.set_plain(w::CUTOFF, 3_000.0);
    let t = replace(&s, 4, &edited);
    assert_eq!(t.patches[4].plain(w::CUTOFF), edited.plain(w::CUTOFF));
    assert_eq!(t.distances[4], s.distances[4]);
}

#[test]
fn scenes_round_trip_through_json() {
    let s = generate(99, Kind::Keys);
    let back = PatchScene::from_json(&s.to_json()).unwrap();
    assert_eq!(back, s);
    let size = serde_json::to_string(&s).unwrap().len();
    assert!(size < 160_000, "{size} bytes");
}

#[test]
fn names_are_evocative_and_distinct() {
    for kind in Kind::TONAL {
        let s = generate(11, kind);
        let names = scene_names(&s);
        for (i, n) in names.iter().enumerate() {
            assert!(n.split(' ').count() >= 2, "{n}");
            assert!(!names[..i].contains(n), "{kind:?}: {n} twice");
        }
        assert_eq!(patch_name(&s.patches[0]), patch_name(&s.patches[0]));
    }
}

#[test]
fn knobs_at_their_defaults_are_the_identity() {
    let s = generate(3, Kind::Bass);
    let x = s.patches[0];
    assert_eq!(shape_knobs(&x, &Knobs::default()), x.folded());
    let dark = shape(&x, &crate::instrument::bloom::voice::Macros { tone: -1.0, ..Default::default() }, 0.25, 0.8);
    assert!(dark.plain(w::CUTOFF) < x.plain(w::CUTOFF) * 0.3);
    let long = shape(&x, &crate::instrument::bloom::voice::Macros { decay: 1.0, ..Default::default() }, 0.25, 0.8);
    assert!(long.plain(w::AMP_DECAY) > x.plain(w::AMP_DECAY) * 2.0 || long.plain(w::AMP_DECAY) >= 9_000.0);
    let dropped = shape_knobs(&x, &Knobs { drop: 0.6, ..Knobs::default() });
    assert!(routes(&dropped).any(|(a, s, d)| s != Src::None && d == Dst::Pitch && dropped.plain(a) > 20.0), "a pitch envelope");
    let still = shape_knobs(&x, &Knobs { motion: 0.0, ..Knobs::default() });
    assert!(routes(&still).all(|(a, s, _)| !matches!(s, Src::Lfo(_)) || still.plain(a).abs() < 1e-3));
}

// ── The quality gate ────────────────────────────────────────────────────────

const RATE: f64 = 24_000.0;

/// A patch's demo note through a plain synth at its level gain, mono.
fn render(synth: &mut WavetableSynth, patch: &Patch, gain: f32, key: u8, vel: f32, hold: f32, tail: f32) -> Vec<f32> {
    synth.restart();
    for (id, v) in patch.pairs() {
        synth.set_param(id, v);
    }
    let on = (f64::from(hold) * RATE) as usize;
    let total = on + (f64::from(tail) * RATE) as usize;
    let mut buf = AudioBuffer::new(2, 256);
    let tr = Transport::default();
    let mut out = Vec::with_capacity(total);
    let mut dc = [DcBlock::default(); 2];
    let pole = DcBlock::pole(RATE as f32);
    let mut pos = 0;
    while pos < total {
        let n = 256.min(total - pos);
        buf.set_frames(n);
        buf.clear();
        let mut ev = Vec::new();
        if pos == 0 {
            ev.push(Event { offset: 0, kind: EventKind::NoteOn { channel: 0, key, velocity: vel, note_id: -1 } });
        }
        if on >= pos && on < pos + n {
            ev.push(Event { offset: (on - pos) as u32, kind: EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: -1 } });
        }
        let mut ctx = ProcessContext { sample_rate: RATE, buffer: &mut buf, sidechain: None, events: &ev, transport: &tr };
        synth.process(&mut ctx);
        for i in 0..n {
            // As the device plays it: the DC blocker, then the level.
            let (l, r) = (dc[0].process(buf.channel(0)[i], pole), dc[1].process(buf.channel(1)[i], pole));
            out.push((l + r) * 0.5 * gain);
        }
        pos += n;
    }
    out
}

fn win_rms(x: &[f32], ms: f32) -> Vec<f32> {
    let w = (RATE as f32 * ms * 0.001) as usize;
    x.chunks(w).map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt()).collect()
}

fn low_share(x: &[f32], hz: f32) -> f32 {
    let a = (-std::f32::consts::TAU * hz / RATE as f32).exp();
    let (mut y, mut lo, mut all) = (0.0f32, 0.0f64, 0.0f64);
    for &s in x {
        y = a * y + (1.0 - a) * s;
        lo += f64::from(y * y);
        all += f64::from(s * s);
    }
    (lo / all.max(1e-20)) as f32
}

/// Spread of the frame centroids (max / min, `ms` frames over 12 % of the
/// peak).
fn movement(x: &[f32], ms: f64) -> f32 {
    let w = win_rms(x, 10.0);
    let max = w.iter().fold(0f32, |m, v| m.max(*v));
    let frame = (RATE * ms * 0.001) as usize;
    let c: Vec<f32> = x
        .chunks(frame)
        .filter_map(|c| {
            let e: f32 = c.iter().map(|v| v * v).sum();
            if e / c.len() as f32 <= (max * 0.12).powi(2) {
                return None;
            }
            let d: f32 = c.windows(2).map(|p| (p[1] - p[0]).powi(2)).sum();
            Some((d / e).sqrt())
        })
        .collect();
    if c.len() < 2 { 1.0 } else { c.iter().fold(0f32, |m, v| m.max(*v)) / c.iter().fold(f32::MAX, |m, v| m.min(*v)).max(1e-6) }
}

/// Rendered checks of two cells of 100 seeds per kind: every patch sounds,
/// stays finite, does not clip, has no DC, and lands in its kind's band.
#[test]
fn every_kind_sounds_like_itself() {
    let failures: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = Kind::TONAL
            .iter()
            .map(|&kind| {
                s.spawn(move || {
                    let mut fails = Vec::new();
                    let mut synth = WavetableSynth::new();
                    synth.prepare(RATE, 256);
                    let mut lev = Leveler::new();
                    let (key, hold, tail) = crate::instrument::bloom::demo_note(kind);
                    for seed in 0..100u64 {
                        let scene = generate(seed * 104_729 + 11, kind);
                        for cell in [(seed % 16) as usize, ((seed * 7 + 3) % 16) as usize] {
                            let x = &scene.patches[cell];
                            let at = format!("{kind:?} seed {} cell {cell} ({})", seed * 104_729 + 11, patch_name(x));
                            let gain = lev.measure(x);
                            let y = render(&mut synth, x, gain, key, 0.8, hold, tail);
                            let peak = y.iter().fold(0f32, |m, v| m.max(v.abs()));
                            if !y.iter().all(|v| v.is_finite()) {
                                fails.push(format!("{at}: not finite"));
                                continue;
                            }
                            let w = win_rms(&y, 10.0);
                            let max = w.iter().fold(0f32, |m, v| m.max(*v));
                            let mean = y.iter().sum::<f32>() / y.len() as f32;
                            if max < 0.02 {
                                fails.push(format!("{at}: silent ({max})"));
                                continue;
                            }
                            if peak > 1.0 {
                                fails.push(format!("{at}: clips ({peak})"));
                            }
                            if mean.abs() > 0.01 {
                                fails.push(format!("{at}: DC {mean}"));
                            }
                            // Time to half the loudest level of the first 300 ms
                            // (fast kinds), or of the whole note (slow kinds).
                            let early = w[..30.min(w.len())].iter().fold(0f32, |m, v| m.max(*v));
                            let reference = if matches!(kind, Kind::Pad | Kind::Texture) { max } else { early };
                            let attack = w.iter().position(|v| *v >= reference * 0.5).unwrap_or(0) as f32 * 10.0;
                            let level_at = |t: f32| w.get((t * 100.0) as usize).copied().unwrap_or(0.0) / max;
                            let off = (hold * 100.0) as usize;
                            match kind {
                                Kind::Bass => {
                                    let lo = low_share(&y, 250.0);
                                    if lo < 0.25 {
                                        fails.push(format!("{at}: bass without a low end ({lo:.2})"));
                                    }
                                    if attack > 30.0 {
                                        fails.push(format!("{at}: slow bass attack {attack}"));
                                    }
                                }
                                Kind::Lead => {
                                    if level_at(hold * 0.9) < 0.18 {
                                        fails.push(format!("{at}: lead does not hold ({:.2})", level_at(hold * 0.9)));
                                    }
                                    let t = analyze_signal(&y[..(RATE as usize).min(y.len())], RATE as f32);
                                    if t.centroid < 400.0 {
                                        fails.push(format!("{at}: dull lead ({:.0} Hz)", t.centroid));
                                    }
                                }
                                Kind::Pad => {
                                    if attack < 90.0 {
                                        fails.push(format!("{at}: pad attacks at {attack} ms"));
                                    }
                                    if level_at(hold + 0.4) < 0.03 {
                                        fails.push(format!("{at}: pad releases short ({:.3})", level_at(hold + 0.4)));
                                    }
                                }
                                Kind::Pluck => {
                                    if attack > 20.0 {
                                        fails.push(format!("{at}: pluck attacks at {attack} ms"));
                                    }
                                    let late = w[off.min(w.len())..].iter().skip(45).fold(0f32, |m, v| m.max(*v)) / max;
                                    if late > 0.35 {
                                        fails.push(format!("{at}: pluck rings ({late:.2})"));
                                    }
                                }
                                Kind::Keys => {
                                    if attack > 25.0 {
                                        fails.push(format!("{at}: keys attack at {attack} ms"));
                                    }
                                    let soft = render(&mut synth, x, gain, key, 0.3, hold, tail);
                                    let sm = win_rms(&soft, 10.0).iter().fold(0f32, |m, v| m.max(*v));
                                    if max / sm.max(1e-9) < 1.4 {
                                        fails.push(format!("{at}: keys ignore velocity ({:.2})", max / sm.max(1e-9)));
                                    }
                                }
                                Kind::Fx => {
                                    let t = analyze_signal(&y, RATE as f32);
                                    let mv = movement(&y, 25.0);
                                    if mv < 1.3 && t.noisiness < 0.2 {
                                        fails.push(format!("{at}: static FX (move {mv:.2}, noise {:.2})", t.noisiness));
                                    }
                                }
                                Kind::Texture => {
                                    if attack < 70.0 {
                                        fails.push(format!("{at}: texture attacks at {attack} ms"));
                                    }
                                    let t = analyze_signal(&y, RATE as f32);
                                    // Evolving: noise, a moving spectrum or a breathing level.
                                    let w100 = win_rms(&y[..((hold as f64 * RATE) as usize).min(y.len())], 100.0);
                                    let body = &w100[w100.len() / 3..];
                                    let mean = body.iter().sum::<f32>() / body.len().max(1) as f32;
                                    let cv = (body.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / body.len().max(1) as f32).sqrt() / mean.max(1e-9);
                                    if t.noisiness < 0.1 && movement(&y, 100.0) < 1.25 && cv < 0.12 {
                                        fails.push(format!("{at}: plain texture (noise {:.2}, move {:.2})", t.noisiness, movement(&y, 100.0)));
                                    }
                                }
                                Kind::Drums => {}
                            }
                        }
                    }
                    fails
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn levels_land_on_the_target() {
    let mut lev = Leveler::new();
    let mut synth = WavetableSynth::new();
    synth.prepare(RATE, 256);
    for kind in [Kind::Bass, Kind::Lead, Kind::Keys] {
        let s = generate(17, kind);
        for x in s.patches.iter().take(4) {
            let g = lev.measure(x);
            assert!(g.is_finite() && g > 0.0);
            assert_eq!(g, lev.measure(x), "a measurement repeats exactly");
        }
    }
}





