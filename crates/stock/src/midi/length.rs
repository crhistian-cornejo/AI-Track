//! `midi.length` — sets how long each note lasts.
//!
//! * **Fixed**: `Time` (ms) × `Gate`. **Sync**: a tempo division × `Gate`.
//!   The played release is replaced by the new length.
//! * **Stretch**: the played length times `Stretch` (1x and up: a note-off
//!   cannot be moved earlier than it arrives; to shorten, use `Max`).
//! * **Legato**: each note runs on until the next note starts, but never past
//!   the cap (`Max`, or eight beats when it is off) and never shorter than
//!   it was played.
//! * **Min / Max** (0 = off) bound the result in every mode.
//!
//! Notes live in a small table of slots (64); a note that finds it full is
//! passed through unchanged, release included, and counted as dropped.

use super::kit::{DIVISION_NAMES, Ledger, division_beats, samples_per_beat};
use crate::common::{ParamStore, load_state, save_state};
use aitrack_dsp::{Category, EventKind, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.length";

pub mod p {
    pub const MODE: u32 = 0;
    pub const TIME: u32 = 1;
    pub const DIVISION: u32 = 2;
    pub const GATE: u32 = 3;
    pub const STRETCH: u32 = 4;
    pub const MIN: u32 = 5;
    pub const MAX: u32 = 6;
}

/// `Mode` choices.
pub mod mode {
    pub const FIXED: f32 = 0.0;
    pub const SYNC: f32 = 1.0;
    pub const STRETCH: f32 = 2.0;
    pub const LEGATO: f32 = 3.0;
}

const MODES: &[&str] = &["Fixed", "Sync", "Stretch", "Legato"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 3.0, mode::SYNC).choices(MODES),
        ParamInfo::new(p::TIME, "Time", 1.0, 4_000.0, 125.0).unit("ms").log(),
        ParamInfo::new(p::DIVISION, "Division", 0.0, (DIVISION_NAMES.len() - 1) as f32, 3.0).choices(DIVISION_NAMES).short("Div"),
        ParamInfo::new(p::GATE, "Gate", 5.0, 400.0, 100.0).unit("%").log(),
        ParamInfo::new(p::STRETCH, "Stretch", 1.0, 8.0, 2.0).unit("x"),
        ParamInfo::new(p::MIN, "Min", 0.0, 2_000.0, 0.0).unit("ms").power(2.0),
        ParamInfo::new(p::MAX, "Max", 0.0, 16_000.0, 0.0).unit("ms").power(3.0),
    ]
}

const SLOTS: usize = 64;
const NEVER: i64 = i64::MAX;
/// How far a legato note may run on when `Max` is off.
const LEGATO_BEATS: f64 = 8.0;

/// A note this device started and has not ended.
#[derive(Clone, Copy)]
struct Slot {
    used: bool,
    channel: u8,
    key: u8,
    id: i32,
    /// The mode the note began in (changing the knob mid-note cannot strand it).
    mode: u8,
    on_abs: i64,
    /// When the note ends (`NEVER` until its release is known).
    off_at: i64,
    /// The latest it may end.
    cap_at: i64,
    /// The earliest it may end.
    min_at: i64,
    /// The played release has arrived (legato waits for a successor).
    released: bool,
}

const FREE: Slot = Slot { used: false, channel: 0, key: 0, id: 0, mode: 0, on_abs: 0, off_at: NEVER, cap_at: NEVER, min_at: 0, released: false };

pub struct NoteLength {
    info: PluginInfo,
    params: ParamStore,
    ledger: Ledger,
    slots: [Slot; SLOTS],
    /// Samples processed: the time axis of the slots.
    clock: i64,
    /// Start of the latest note seen.
    last_on: i64,
    lost: u64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(NoteLength::new())
}

impl Default for NoteLength {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteLength {
    pub fn new() -> Self {
        let params = params();
        Self {
            info: PluginInfo { id: ID, name: "Note Length", vendor: "AI-Track", category: Category::MidiEffect, tags: &["midi", "length", "gate", "legato"], params: params.clone(), inputs: 2, outputs: 2, has_sidechain: false },
            params: ParamStore::new(&params),
            ledger: Ledger::new(),
            slots: [FREE; SLOTS],
            clock: 0,
            last_on: i64::MIN,
            lost: 0,
        }
    }

    fn find(&self, channel: u8, key: u8) -> Option<usize> {
        self.slots.iter().position(|s| s.used && s.channel == channel && s.key == key)
    }

    /// End, in time order, every note whose end is at or before `upto`.
    fn flush_due(&mut self, out: &mut EventSink<'_>, frames: usize, upto: i64) {
        loop {
            let mut best: Option<(i64, usize)> = None;
            for (i, s) in self.slots.iter().enumerate() {
                if s.used && s.off_at <= upto && best.is_none_or(|(t, _)| s.off_at < t) {
                    best = Some((s.off_at, i));
                }
            }
            let Some((t, i)) = best else { return };
            let s = self.slots[i];
            self.slots[i].used = false;
            let offset = (t - self.clock).clamp(0, frames as i64 - 1) as u32;
            self.ledger.off_own(out, offset, self.clock + offset as i64, s.channel, s.key, 0.0, s.id);
        }
    }
}

impl Processor for NoteLength {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
    }

    fn reset(&mut self) {
        self.ledger.forget();
        self.slots = [FREE; SLOTS];
        self.last_on = i64::MIN;
    }

    /// A MIDI effect is not given audio; whatever arrives stays as it is.
    fn process(&mut self, _ctx: &mut ProcessContext<'_>) {}

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        let sr = ctx.sample_rate;
        let pr = &self.params;
        let mode = pr.choice(p::MODE) as u8;
        let gate = pr.plain(p::GATE) as f64 * 0.01;
        let stretch = pr.plain(p::STRETCH) as f64;
        let min = (pr.plain(p::MIN) as f64 * 0.001 * sr).round() as i64;
        let max = (pr.plain(p::MAX) as f64 * 0.001 * sr).round() as i64;
        let spb = samples_per_beat(ctx.transport.bpm, sr);
        let base = match mode {
            0 => pr.plain(p::TIME) as f64 * 0.001 * sr,
            _ => division_beats(pr.choice(p::DIVISION), ctx.transport.time_sig) * spb,
        };
        let frames = ctx.frames;
        let clock = self.clock;
        self.ledger.begin_block(out);
        for e in ctx.events {
            let abs = clock + e.offset as i64;
            self.flush_due(out, frames, abs);
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } => {
                    // A key pressed again before its release ends the old note.
                    if let Some(i) = self.find(channel, key) {
                        self.slots[i].off_at = abs;
                    }
                    // Legato notes waiting for a successor end here.
                    for s in &mut self.slots {
                        if s.used && s.channel == channel && s.mode == 3 && s.released && s.on_abs < abs {
                            s.off_at = abs.max(s.min_at).min(s.cap_at);
                        }
                    }
                    self.flush_due(out, frames, abs);
                    self.last_on = abs;
                    let Some(free) = self.slots.iter().position(|s| !s.used) else {
                        // No room to follow it: the note goes through as played.
                        self.lost += 1;
                        self.ledger.on(out, e.offset, abs, channel, key, velocity, note_id);
                        continue;
                    };
                    if !self.ledger.on(out, e.offset, abs, channel, key, velocity, note_id) {
                        continue;
                    }
                    let (off_at, cap_at) = match mode {
                        0 | 1 => {
                            let mut len = (base * gate).round() as i64;
                            len = len.max(min);
                            if max > 0 {
                                len = len.min(max);
                            }
                            let t = abs + len.max(1);
                            (t, t)
                        }
                        2 => (NEVER, if max > 0 { abs + max.max(1) } else { NEVER }),
                        _ => (NEVER, abs + if max > 0 { max.max(1) } else { (LEGATO_BEATS * spb).round() as i64 }),
                    };
                    // Stretch has no end until the release; the cap is its limit.
                    let off_at = if mode >= 2 { cap_at } else { off_at };
                    self.slots[free] = Slot { used: true, channel, key, id: note_id, mode, on_abs: abs, off_at: off_at.max(abs + 1), cap_at, min_at: abs + min.max(1), released: false };
                }
                EventKind::NoteOff { channel, key, velocity, note_id } => match self.find(channel, key) {
                    None => {
                        self.ledger.off(out, e.offset, abs, channel, key, velocity, note_id);
                    }
                    Some(i) => {
                        let s = &mut self.slots[i];
                        s.released = true;
                        match s.mode {
                            2 => {
                                let played = (abs - s.on_abs).max(1);
                                let target = s.on_abs + ((played as f64 * stretch).round() as i64).max(min).max(1);
                                s.off_at = target.min(s.cap_at).max(abs);
                            }
                            3 if self.last_on > s.on_abs => s.off_at = abs.max(s.min_at).min(s.cap_at),
                            _ => {}
                        }
                    }
                },
                EventKind::AllNotesOff => {
                    out.push(e.offset, e.kind);
                    self.ledger.forget();
                    self.slots = [FREE; SLOTS];
                }
                kind => {
                    out.push(e.offset, kind);
                }
            }
        }
        self.flush_due(out, frames, clock + frames as i64 - 1);
        self.clock += frames as i64;
    }

    fn dropped_events(&self) -> u64 {
        self.lost
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fxtest::{Rig, off, on};
    use super::*;

    const BEAT: i64 = 24_000; // 120 bpm
    const SIXTEENTH: i64 = BEAT / 4;

    fn rig() -> Rig {
        Rig::new(create())
    }

    fn set(r: &mut Rig, id: u32, plain: f32) {
        let n = r.p.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
        r.p.set_param(id, n);
    }

    /// `(on, off)` times of the notes of `key`, in order.
    fn spans(r: &Rig, key: u8) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        let mut open = None;
        for n in r.notes().into_iter().filter(|n| n.2 == key) {
            if n.1 {
                open = Some(n.0);
            } else if let Some(t) = open.take() {
                out.push((t, n.0));
            }
        }
        out
    }

    #[test]
    fn sync_mode_replaces_the_played_length_by_the_division_times_gate() {
        let mut r = rig();
        r.run(&[(1_000, on(60, 0.8)), (1_000 + 4 * BEAT, off(60))], 8 * BEAT, 256);
        assert_eq!(spans(&r, 60), vec![(1_000, 1_000 + SIXTEENTH)]);
        r.assert_balanced();
        let mut r = rig();
        set(&mut r, p::GATE, 50.0);
        set(&mut r, p::DIVISION, 6.0); // 1/8
        r.run(&[(0, on(60, 0.8)), (200, off(60))], 2 * BEAT, 256);
        assert_eq!(spans(&r, 60), vec![(0, BEAT / 4)], "50% of an eighth, even for a note played shorter");
    }

    #[test]
    fn fixed_mode_uses_milliseconds_and_the_tempo_does_not_matter() {
        for bpm in [90.0, 174.0] {
            let mut r = rig();
            r.bpm = bpm;
            set(&mut r, p::MODE, mode::FIXED);
            set(&mut r, p::TIME, 100.0);
            r.run(&[(500, on(60, 0.8)), (900, off(60))], 2 * BEAT, 128);
            assert_eq!(spans(&r, 60), vec![(500, 500 + 4_800)]);
        }
    }

    #[test]
    fn sync_follows_the_tempo() {
        let mut r = rig();
        r.bpm = 140.0;
        set(&mut r, p::DIVISION, 9.0); // 1/4
        r.run(&[(0, on(60, 0.8)), (50, off(60))], 2 * BEAT, 128);
        assert_eq!(spans(&r, 60), vec![(0, (60.0f64 / 140.0 * 48_000.0).round() as i64)]);
    }

    #[test]
    fn min_and_max_bound_the_result() {
        let mut r = rig();
        set(&mut r, p::MODE, mode::FIXED);
        set(&mut r, p::TIME, 10.0);
        set(&mut r, p::MIN, 50.0);
        r.run(&[(0, on(60, 0.8)), (10, off(60))], BEAT, 128);
        assert_eq!(spans(&r, 60), vec![(0, 2_400)]);
        let mut r = rig();
        set(&mut r, p::MODE, mode::FIXED);
        set(&mut r, p::TIME, 1_000.0);
        set(&mut r, p::MAX, 100.0);
        r.run(&[(0, on(60, 0.8)), (10, off(60))], 2 * BEAT, 128);
        assert_eq!(spans(&r, 60), vec![(0, 4_800)]);
    }

    #[test]
    fn stretch_multiplies_the_played_length_and_max_caps_it() {
        let mut r = rig();
        set(&mut r, p::MODE, mode::STRETCH);
        set(&mut r, p::STRETCH, 2.0);
        r.run(&[(1_000, on(60, 0.8)), (1_000 + 6_000, off(60))], 2 * BEAT, 128);
        assert_eq!(spans(&r, 60), vec![(1_000, 1_000 + 12_000)]);
        let mut r = rig();
        set(&mut r, p::MODE, mode::STRETCH);
        set(&mut r, p::STRETCH, 4.0);
        set(&mut r, p::MAX, 100.0);
        r.run(&[(0, on(60, 0.8)), (6_000, off(60))], 2 * BEAT, 128);
        assert_eq!(spans(&r, 60), vec![(0, 4_800)], "capped at 100 ms, and the played release does not cut a second time");
        r.assert_balanced();
        let mut r = rig();
        set(&mut r, p::MODE, mode::STRETCH);
        set(&mut r, p::STRETCH, 1.0);
        set(&mut r, p::MIN, 200.0);
        r.run(&[(0, on(60, 0.8)), (1_000, off(60))], 2 * BEAT, 128);
        assert_eq!(spans(&r, 60), vec![(0, 9_600)]);
    }

    #[test]
    fn legato_runs_each_note_on_to_the_next() {
        let mut r = rig();
        set(&mut r, p::MODE, mode::LEGATO);
        r.run(&[(0, on(60, 0.8)), (3_000, off(60)), (12_000, on(64, 0.8)), (15_000, off(64)), (30_000, on(67, 0.8)), (31_000, off(67))], 40 * BEAT, 256);
        assert_eq!(spans(&r, 60), vec![(0, 12_000)]);
        assert_eq!(spans(&r, 64), vec![(12_000, 30_000)]);
        // The last one has no successor: eight beats at most.
        assert_eq!(spans(&r, 67), vec![(30_000, 30_000 + 8 * BEAT)]);
        r.assert_balanced();
    }

    #[test]
    fn legato_never_cuts_a_note_that_is_still_held() {
        let mut r = rig();
        set(&mut r, p::MODE, mode::LEGATO);
        r.run(&[(0, on(60, 0.8)), (20_000, off(60)), (12_000, on(64, 0.8)), (13_000, off(64)), (30_000, on(67, 0.8)), (31_000, off(67))], 40 * BEAT, 256);
        assert_eq!(spans(&r, 60), vec![(0, 20_000)], "released after the next note began: its own release stands");
        assert_eq!(spans(&r, 64), vec![(12_000, 30_000)]);
        r.assert_balanced();
    }

    #[test]
    fn legato_with_max_uses_it_as_the_cap() {
        let mut r = rig();
        set(&mut r, p::MODE, mode::LEGATO);
        set(&mut r, p::MAX, 500.0);
        r.run(&[(0, on(60, 0.8)), (1_000, off(60))], 4 * BEAT, 256);
        assert_eq!(spans(&r, 60), vec![(0, 24_000)]);
    }

    #[test]
    fn a_mode_change_during_a_note_cannot_hang_it() {
        for (from, to) in [(mode::STRETCH, mode::FIXED), (mode::LEGATO, mode::SYNC), (mode::SYNC, mode::STRETCH), (mode::FIXED, mode::LEGATO)] {
            let mut r = rig();
            set(&mut r, p::MODE, from);
            r.run(&[(0, on(60, 0.8))], 1_000, 256);
            set(&mut r, p::MODE, to);
            r.run(&[(0, off(60))], 40 * BEAT, 256);
            r.assert_balanced();
        }
    }

    #[test]
    fn a_zero_length_note_and_a_full_table_stay_paired() {
        let mut r = rig();
        r.run(&[(100, on(60, 0.8)), (100, off(60))], 2 * BEAT, 256);
        r.assert_balanced();
        let mut r = rig();
        let mut input = Vec::new();
        for i in 0..70u8 {
            input.push((i as i64, on(20 + i, 0.8)));
            input.push((20_000 + i as i64, off(20 + i)));
        }
        r.run(&input, 10 * BEAT, 256);
        r.assert_balanced();
        assert!(r.p.dropped_events() > 0);
    }

    #[test]
    fn all_notes_off_forgets_every_note() {
        let mut r = rig();
        set(&mut r, p::MODE, mode::LEGATO);
        r.run(&[(0, on(60, 0.8)), (1_000, EventKind::AllNotesOff)], 20 * BEAT, 256);
        r.assert_balanced();
    }

    #[test]
    fn the_same_result_for_any_block_size_and_always_paired() {
        let mut seed = 0xBEEF_u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20 {
            let settings: Vec<(u32, f32)> = (0..7).map(|id| (id, (rnd() % 1001) as f32 / 1000.0)).collect();
            let mut input = Vec::new();
            let mut free_at = [0i64; 128];
            let mut t = 0;
            for _ in 0..40 {
                t += (rnd() % 5_000) as i64;
                let key = 40 + (rnd() % 10) as u8;
                if t <= free_at[key as usize] {
                    continue;
                }
                let len = 2 + (rnd() % 30_000) as i64;
                free_at[key as usize] = t + len;
                input.push((t, on(key, 0.1 + (rnd() % 90) as f32 / 100.0)));
                input.push((t + len, off(key)));
            }
            input.sort_by_key(|e| e.0);
            let mut want = None;
            for block in [3usize, 29, 512, 4096] {
                let mut r = rig();
                for (id, v) in &settings {
                    r.p.set_param(*id, *v);
                }
                r.run(&input, t + 40 * BEAT, block);
                r.assert_balanced();
                let notes = r.notes().iter().map(|n| (n.0, n.1, n.2, n.3.to_bits())).collect::<Vec<_>>();
                match &want {
                    None => want = Some(notes),
                    Some(w) => assert_eq!(&notes, w, "block {block}"),
                }
            }
        }
    }
}
