//! `mod.shaper` — the shaper's curve as a modulator. It passes audio through
//! untouched; the engine asks it for a control signal ([`Processor::modulate`])
//! and adds it, scaled by each mapping's range, to the parameters it drives
//! (up to eight), so a pump or a gate can sit on any knob of the track.
//!
//! The curve has six nodes like `fx.shaper`'s (the first at the start of the
//! cycle, the last at its end, four free in between) and one *Tension* that
//! bends every segment. *Trigger* decides what starts a cycle:
//!
//! * **Sync** — the cycle follows the song position (`Division` long, shifted
//!   by `Phase`); stopped, it free-runs at the tempo.
//! * **Note Loop** — every note-on restarts the cycle, which then keeps going.
//! * **Note Once** — a note-on plays the cycle one time and the output stays
//!   at the curve's last level. Before the first note, and after the song
//!   position jumps back past it, the output rests at that level too.
//!
//! The output is `0.5 + 0.5·(offset + depth·(2·level − 1))` clamped to 0..1,
//! which for a depth of 100 % and no offset is the curve's level itself.
//! Everything derives from the song position and the note-ons, so an export
//! sounds like the playback.
//!
//! Meter layout: `[output (0..1), position in the cycle (0..1)]`.

use crate::common::{ParamStore, load_state, save_state, write_meter};
use crate::fx::shaper::{curve_points_with, level_at};
use aitrack_dsp::lfo::{SYNC_DIVISIONS, division_beats};
use aitrack_dsp::{Category, EventKind, ModBlock, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "mod.shaper";

pub mod p {
    pub const DIVISION: u32 = 0;
    pub const TRIGGER: u32 = 1;
    pub const DEPTH: u32 = 2;
    pub const OFFSET: u32 = 3;
    pub const SMOOTH: u32 = 4;
    pub const TENSION: u32 = 5;
    pub const PHASE: u32 = 6;
    /// Level at the start of the cycle.
    pub const Y0: u32 = 7;
    pub const X1: u32 = 8;
    pub const Y1: u32 = 9;
    pub const X2: u32 = 10;
    pub const Y2: u32 = 11;
    pub const X3: u32 = 12;
    pub const Y3: u32 = 13;
    pub const X4: u32 = 14;
    pub const Y4: u32 = 15;
    /// Level at the end of the cycle.
    pub const Y5: u32 = 16;

    /// `(x, y)` parameter ids of the four free nodes.
    pub const NODES: [(u32, u32); 4] = [(X1, Y1), (X2, Y2), (X3, Y3), (X4, Y4)];
}

/// Values of `Trigger`.
pub mod trigger {
    pub const SYNC: usize = 0;
    pub const NOTE_LOOP: usize = 1;
    pub const NOTE_ONCE: usize = 2;
}

pub const TRIGGERS: &[&str] = &["Sync", "Note Loop", "Note Once"];
/// The division that is one beat long.
const QUARTER: usize = 7;

pub fn params() -> Vec<ParamInfo> {
    let n_div = (SYNC_DIVISIONS.len() - 1) as f32;
    let node = |id, name, short, default| ParamInfo::new(id, name, 0.0, 1.0, default).short(short);
    vec![
        ParamInfo::new(p::DIVISION, "Division", 0.0, n_div, QUARTER as f32).choices(SYNC_DIVISIONS).short("Div"),
        ParamInfo::new(p::TRIGGER, "Trigger", 0.0, 2.0, trigger::SYNC as f32).choices(TRIGGERS).short("Trig"),
        ParamInfo::new(p::DEPTH, "Depth", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OFFSET, "Offset", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::SMOOTH, "Smooth", 0.0, 200.0, 4.0).unit("ms"),
        ParamInfo::new(p::TENSION, "Tension", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::PHASE, "Phase", 0.0, 100.0, 0.0).unit("%"),
        // The default curve is the pumping duck of `fx.shaper`: down on the beat, back up by the off-beat.
        node(p::Y0, "Start Level", "Start", 0.0),
        node(p::X1, "Node 1 Time", "T1", 0.06),
        node(p::Y1, "Node 1 Level", "L1", 0.0),
        node(p::X2, "Node 2 Time", "T2", 0.3),
        node(p::Y2, "Node 2 Level", "L2", 0.7),
        node(p::X3, "Node 3 Time", "T3", 0.55),
        node(p::Y3, "Node 3 Level", "L3", 1.0),
        node(p::X4, "Node 4 Time", "T4", 0.9),
        node(p::Y4, "Node 4 Level", "L4", 1.0),
        node(p::Y5, "End Level", "End", 1.0),
    ]
}

/// The curve's six points in time order, from plain parameter values.
pub fn curve_points(plain: impl Fn(u32) -> f32) -> [(f32, f32); 6] {
    curve_points_with(plain, p::Y0, p::NODES, p::Y5)
}

/// The output (0..1) for a curve `level` (0..1), a `depth` (0..1) and an
/// `offset` (−1..1): `0.5 + 0.5·(offset + depth·(2·level − 1))` clamped. At
/// full depth and no offset it is the level itself.
pub fn output_of(level: f32, depth: f32, offset: f32) -> f32 {
    (0.5 + 0.5 * (offset + depth * (2. * level - 1.))).clamp(0., 1.)
}

pub struct ShaperMod {
    info: PluginInfo,
    params: ParamStore,
    /// Where the cycle counts from in the note modes (quarter notes, in the
    /// domain of `trig_playing`), and whether a note has started one.
    trig_beats: f64,
    triggered: bool,
    trig_playing: bool,
    /// Frames the stopped transport has free-run.
    free_samples: i64,
    smoothed: f32,
    primed: bool,
    out: f32,
    phase: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(ShaperMod::new())
}

impl Default for ShaperMod {
    fn default() -> Self {
        Self::new()
    }
}

impl ShaperMod {
    pub fn new() -> Self {
        let params = params();
        Self {
            info: PluginInfo {
                id: ID,
                name: "Shaper Mod",
                vendor: "AI-Track",
                category: Category::Utility,
                tags: &["modulation", "shaper", "pump", "sidechain", "envelope", "modulator", "mapping"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            trig_beats: 0.0,
            triggered: false,
            trig_playing: true,
            free_samples: 0,
            smoothed: 1.0,
            primed: false,
            out: 1.0,
            phase: 1.0,
        }
    }
}

impl Processor for ShaperMod {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
    }

    fn reset(&mut self) {
        self.trig_beats = 0.0;
        self.triggered = false;
        self.trig_playing = true;
        self.free_samples = 0;
        self.primed = false;
    }

    fn process(&mut self, _ctx: &mut ProcessContext<'_>) {}

    fn modulate(&mut self, m: &ModBlock<'_>, out: &mut [f32]) -> bool {
        let points = curve_points(|id| self.params.plain(id));
        let tension = self.params.plain(p::TENSION) / 100.0;
        let mode = self.params.choice(p::TRIGGER);
        let cycle = division_beats(self.params.choice(p::DIVISION));
        let depth = self.params.plain(p::DEPTH) / 100.0;
        let offset = self.params.plain(p::OFFSET) / 100.0;
        let shift = f64::from(self.params.plain(p::PHASE)) / 100.0;
        let smooth_ms = self.params.plain(p::SMOOTH);
        let sr = m.sample_rate.max(1.0);
        let coeff = if smooth_ms > 0.5 { 1.0 - (-(m.step as f32 / sr as f32) / (smooth_ms * 0.001)).exp() } else { 1.0 };
        if m.playing != self.trig_playing {
            // The clock changed domain (play / stop): a note from the other one means nothing here.
            self.trig_playing = m.playing;
            self.triggered = false;
        }
        let base = if m.playing { m.start_samples } else { self.free_samples };
        let mut next_event = 0;
        let mut pos = 1.0f64;
        for (k, o) in out.iter_mut().enumerate() {
            let off = k * m.step;
            let samples = base + off as i64;
            let beats = if m.playing { m.beats.get(k).copied().unwrap_or(0.0) } else { samples as f64 / sr * m.bpm / 60.0 };
            while let Some(e) = m.events.get(next_event).filter(|e| (e.offset as usize) < off + m.step) {
                if matches!(e.kind, EventKind::NoteOn { .. }) {
                    self.trig_beats = beats;
                    self.triggered = true;
                }
                next_event += 1;
            }
            pos = if mode == trigger::SYNC {
                (beats / cycle + shift).rem_euclid(1.0)
            } else {
                let since = (beats - self.trig_beats) / cycle;
                if !self.triggered || since < -1e-9 {
                    // At rest on the last level: before the first note, or the song went back past it.
                    1.0
                } else if mode == trigger::NOTE_LOOP {
                    (since + shift).rem_euclid(1.0)
                } else {
                    (since + shift).clamp(0.0, 1.0)
                }
            };
            let v = output_of(level_at(&points, tension, pos as f32), depth, offset);
            if !self.primed {
                self.smoothed = v;
                self.primed = true;
            }
            self.smoothed += (v - self.smoothed) * coeff;
            *o = self.smoothed;
        }
        if !m.playing {
            self.free_samples += m.frames as i64;
        }
        self.out = out.last().copied().unwrap_or(self.out);
        self.phase = pos as f32;
        true
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.out, self.phase])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, get_plain, set_plain};
    use aitrack_dsp::Event;

    const STEP: usize = 32;
    /// One beat at 120 bpm, in samples, and in control steps.
    const BEAT: i64 = 24_000;
    const BEAT_STEPS: usize = 750;

    /// `n` steps starting at song sample `start`, 120 bpm.
    fn run(p: &mut ShaperMod, start: i64, n: usize, playing: bool, events: &[Event]) -> Vec<f32> {
        let beats: Vec<f64> = (0..n).map(|k| (start + (k * STEP) as i64) as f64 / SR * 2.0).collect();
        let m = ModBlock { sample_rate: SR, playing, bpm: 120.0, step: STEP, frames: n * STEP, start_samples: start, beats: &beats, events };
        let mut out = vec![0.0; n];
        assert!(p.modulate(&m, &mut out), "a modulator says so");
        out
    }

    /// A shaper with no smoothing, so the output is the curve to the step.
    fn shaper() -> ShaperMod {
        let mut p = ShaperMod::new();
        p.prepare(SR, 128);
        set_plain(&mut p, p::SMOOTH, 0.0);
        p
    }

    fn note_on(offset: u32) -> Event {
        Event { offset, kind: EventKind::NoteOn { channel: 0, key: 60, velocity: 1.0, note_id: 1 } }
    }

    #[test]
    fn the_output_places_the_level_by_depth_and_offset() {
        assert_eq!((output_of(0., 1., 0.), output_of(1., 1., 0.), output_of(0.3, 1., 0.)), (0., 1., 0.3));
        assert_eq!(output_of(0.2, 0., 0.), 0.5, "no depth: the middle");
        assert_eq!((output_of(0., 0.5, 0.5), output_of(1., 0.5, 0.5)), (0.5, 1.), "half the depth lifted to the top");
        assert_eq!((output_of(1., 1., 1.), output_of(0., 1., -1.)), (1., 0.), "clamped");
    }

    #[test]
    fn the_default_curve_pumps_once_a_beat() {
        let mut p = shaper();
        let v = run(&mut p, 0, BEAT_STEPS, true, &[]);
        assert!(v[0] < 1e-6, "silent on the beat: {}", v[0]);
        assert!((v[BEAT_STEPS * 3 / 10] - 0.7).abs() < 1e-3, "node 2 at 30 % of the beat: {}", v[BEAT_STEPS * 3 / 10]);
        assert!(v[BEAT_STEPS * 6 / 10] > 0.99 && v[BEAT_STEPS - 1] > 0.99, "up for the second half");
        // A new cycle on the next beat.
        assert!(run(&mut p, BEAT, 4, true, &[])[0] < 1e-6);
    }

    #[test]
    fn the_division_sets_the_length_of_the_cycle() {
        let mut p = shaper();
        set_plain(&mut p, p::DIVISION, 10.0); // 1/8: half a beat
        let v = run(&mut p, 0, BEAT_STEPS, true, &[]);
        let half = BEAT_STEPS / 2;
        assert!((v[half * 3 / 10] - 0.7).abs() < 0.02, "the same shape in half the time: {}", v[half * 3 / 10]);
        assert!(v[half] < 1e-3, "and a second cycle starting: {}", v[half]);
    }

    #[test]
    fn the_output_depends_only_on_the_song_position() {
        let (mut a, mut b) = (shaper(), shaper());
        for q in [&mut a, &mut b] {
            set_plain(q, p::TENSION, 35.0);
            set_plain(q, p::DIVISION, 9.0);
            set_plain(q, p::PHASE, 20.0);
        }
        let whole = run(&mut a, 0, 64, true, &[]);
        let mut parts = run(&mut b, 0, 24, true, &[]);
        parts.extend(run(&mut b, (24 * STEP) as i64, 40, true, &[]));
        assert_eq!(whole, parts);
        // A fresh instance dropped at that position agrees (export from a cue point).
        let mut c = shaper();
        set_plain(&mut c, p::TENSION, 35.0);
        set_plain(&mut c, p::DIVISION, 9.0);
        set_plain(&mut c, p::PHASE, 20.0);
        assert_eq!(run(&mut c, (24 * STEP) as i64, 40, true, &[]), whole[24..]);
    }

    #[test]
    fn depth_and_offset_place_the_curve_like_the_lfos() {
        let mut p = shaper();
        set_plain(&mut p, p::DEPTH, 50.0);
        set_plain(&mut p, p::OFFSET, 50.0);
        let v = run(&mut p, 0, BEAT_STEPS, true, &[]);
        let (lo, hi) = v.iter().fold((1.0f32, 0.0f32), |(l, h), x| (l.min(*x), h.max(*x)));
        assert!((lo - 0.5).abs() < 1e-3 && (hi - 1.0).abs() < 1e-3, "half the depth, lifted to the top: {lo}..{hi}");
        set_plain(&mut p, p::DEPTH, 0.0);
        set_plain(&mut p, p::OFFSET, -40.0);
        assert!(run(&mut p, 0, 8, true, &[]).iter().all(|x| (x - 0.3).abs() < 1e-5), "no depth is a flat line at the offset");
        set_plain(&mut p, p::DEPTH, 100.0);
        set_plain(&mut p, p::OFFSET, 100.0);
        assert!(run(&mut p, 0, BEAT_STEPS, true, &[]).iter().all(|x| (0.0..=1.0).contains(x)), "always inside 0..1");
    }

    #[test]
    fn phase_and_tension_move_the_curve() {
        let at = |setup: &[(u32, f32)], step: usize| {
            let mut p = shaper();
            for (id, v) in setup {
                set_plain(&mut p, *id, *v);
            }
            run(&mut p, 0, BEAT_STEPS, true, &[])[step]
        };
        // Half a cycle of phase: the start of the song is half way up the curve (0.94 at 50 %).
        assert!((at(&[(p::PHASE, 50.0)], 0) - 0.94).abs() < 1e-3);
        // Positive tension holds a segment back; negative rushes it (a point between nodes 1 and 2).
        let mid = BEAT_STEPS * 18 / 100;
        assert!(at(&[(p::TENSION, 80.0)], mid) < at(&[], mid) && at(&[(p::TENSION, -80.0)], mid) > at(&[], mid));
        assert!((at(&[(p::TENSION, 80.0)], BEAT_STEPS * 3 / 10) - 0.7).abs() < 1e-3, "the nodes do not move");
    }

    #[test]
    fn a_note_once_waits_for_a_note_plays_the_curve_and_holds_the_end() {
        let mut p = shaper();
        set_plain(&mut p, p::TRIGGER, trigger::NOTE_ONCE as f32);
        // Before any note: at rest on the last level.
        assert!(run(&mut p, 0, 100, true, &[]).iter().all(|x| (x - 1.0).abs() < 1e-6));
        // A note-on at sample 10 000 (step 312 of a block that starts at 9 000, offset 1 000).
        let start = 9_000;
        let v = run(&mut p, start, BEAT_STEPS + 400, true, &[note_on(1_000)]);
        let at_note = (1_000 / STEP) + 1;
        assert!(v[at_note - 3] > 0.99, "up before the note");
        assert!(v[at_note + 1] < 0.05, "down right after it: {}", v[at_note + 1]);
        assert!((v[at_note + BEAT_STEPS * 3 / 10] - 0.7).abs() < 0.02, "node 2 a third of a beat later: {}", v[at_note + BEAT_STEPS * 3 / 10]);
        assert!(v[at_note + BEAT_STEPS + 50] > 0.99, "one cycle later it stays up, it does not start another");
    }

    #[test]
    fn a_note_loop_restarts_at_every_note_and_keeps_cycling() {
        let mut p = shaper();
        set_plain(&mut p, p::TRIGGER, trigger::NOTE_LOOP as f32);
        let v = run(&mut p, 0, 2 * BEAT_STEPS, true, &[note_on(0)]);
        assert!(v[0] < 0.05 && v[BEAT_STEPS * 6 / 10] > 0.99, "the first cycle starts with the note");
        assert!(v[BEAT_STEPS + 2] < 0.2, "and it comes round again a beat later: {}", v[BEAT_STEPS + 2]);
        // A second note 0.7 of a beat into the cycle, when the curve is up, restarts it at once.
        let mut q = shaper();
        set_plain(&mut q, p::TRIGGER, trigger::NOTE_LOOP as f32);
        let second = (BEAT * 7 / 10) as u32;
        let w = run(&mut q, 0, BEAT_STEPS, true, &[note_on(0), note_on(second)]);
        let step = second as usize / STEP;
        assert!(w[step - 5] > 0.99 && w[step + 2] < 0.2, "{} -> {}", w[step - 5], w[step + 2]);
    }

    #[test]
    fn sync_ignores_notes_and_a_jump_back_returns_the_note_modes_to_rest() {
        let (mut a, mut b) = (shaper(), shaper());
        assert_eq!(run(&mut a, 0, 200, true, &[note_on(500)]), run(&mut b, 0, 200, true, &[]), "Sync does not care about notes");
        let mut p = shaper();
        set_plain(&mut p, p::TRIGGER, trigger::NOTE_ONCE as f32);
        run(&mut p, 50_000, 10, true, &[note_on(0)]);
        // The song position goes back before the note: at rest, not a negative phase.
        assert!(run(&mut p, 0, 50, true, &[]).iter().all(|x| (x - 1.0).abs() < 1e-6));
    }

    #[test]
    fn stopped_it_free_runs_at_the_tempo() {
        let mut p = shaper();
        let a = run(&mut p, 0, BEAT_STEPS, false, &[]);
        let b = run(&mut p, 0, BEAT_STEPS, false, &[]);
        assert!(a[0] < 1e-6 && (b[0] - a[0]).abs() < 1e-3, "a new cycle every beat although the position stands still");
        assert!(a.iter().any(|x| *x > 0.99) && a.iter().any(|x| *x < 0.1));
        // A note sets the note modes off in the free-running domain too.
        set_plain(&mut p, p::TRIGGER, trigger::NOTE_ONCE as f32);
        p.reset();
        let c = run(&mut p, 0, BEAT_STEPS, false, &[note_on(0)]);
        assert!(c[1] < 0.05 && c[BEAT_STEPS * 6 / 10] > 0.99);
    }

    #[test]
    fn smoothing_rounds_the_step_between_cycles() {
        let mut sharp = shaper();
        let mut soft = shaper();
        set_plain(&mut soft, p::SMOOTH, 100.0);
        // The curve ends at 1 and the next cycle starts at 0: a step at the beat.
        let (a, b) = (run(&mut sharp, BEAT - 10 * STEP as i64, 40, true, &[]), run(&mut soft, BEAT - 10 * STEP as i64, 40, true, &[]));
        assert!(a[10] < 0.01 && a[9] > 0.99, "sharp: {} -> {}", a[9], a[10]);
        assert!(b[10] > 0.8 && b[30] < b[10], "soft: it glides down: {} then {}", b[10], b[30]);
    }

    #[test]
    fn the_curve_starts_as_the_shapers_pump() {
        use crate::fx::shaper::p as fp;
        let (mine, theirs) = (params(), crate::fx::shaper::params());
        let pairs = [
            (p::Y0, fp::Y0),
            (p::X1, fp::X1),
            (p::Y1, fp::Y1),
            (p::X2, fp::X2),
            (p::Y2, fp::Y2),
            (p::X3, fp::X3),
            (p::Y3, fp::Y3),
            (p::X4, fp::X4),
            (p::Y4, fp::Y4),
            (p::Y5, fp::Y5),
        ];
        for (a, b) in pairs {
            let (ma, tb) = (mine.iter().find(|i| i.id == a).unwrap(), theirs.iter().find(|i| i.id == b).unwrap());
            assert_eq!(ma.default, tb.default, "{} vs {}", ma.name, tb.name);
        }
    }

    #[test]
    fn registry_state_and_meter() {
        let p = crate::create(ID).unwrap();
        assert_eq!(p.info().category, Category::Utility);
        assert!(!p.info().has_sidechain);
        let mut q = shaper();
        set_plain(&mut q, p::DEPTH, 30.0);
        set_plain(&mut q, p::TRIGGER, 2.0);
        let bytes = q.save_state();
        let mut r = shaper();
        r.load_state(&bytes);
        assert!((get_plain(&r, p::DEPTH) - 30.0).abs() < 1e-3);
        assert_eq!(r.params.choice(p::TRIGGER), trigger::NOTE_ONCE);
        // Output and position are published.
        let v = run(&mut q, 0, 200, true, &[]);
        let mut m = [0.0; 4];
        assert_eq!(q.meter(&mut m), 2);
        assert_eq!(m[0], *v.last().unwrap());
        assert!((0.0..=1.0).contains(&m[1]));
    }
}
