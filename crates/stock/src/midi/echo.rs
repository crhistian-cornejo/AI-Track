//! `midi.echo` — repeats every incoming note a number of times, spaced on the
//! tempo grid (with triplets and dotted values) or in milliseconds.
//!
//! * **Repeats / Feedback / Velocity Step**: each repeat's velocity is the
//!   previous one times `Feedback`, plus `Velocity Step` (in 1/127 steps; a
//!   positive step builds up). The chain stops when it falls below one
//!   velocity step.
//! * **Pitch**: semitones added per repeat (cumulative); repeats that would
//!   leave 0..=127 are skipped.
//! * **Length**: `Original` makes every repeat as long as the played note
//!   times `Gate`; `Interval` makes it `Gate` of the echo interval.
//! * **Chance / Velocity Random / Time Random / Pitch Random / Seed /
//!   Pattern Loop**: optional variation. The draws come from a hash of the
//!   seed and the song position of the note, so a render gives exactly what
//!   playback gave and block sizes make no difference (`Pattern Loop` makes a
//!   pattern of that many bars repeat its draws).
//!
//! Repeats are scheduled in an [`EventQueue`] counted in processed samples, so
//! they sound with the transport stopped too. Every note-on written has its
//! note-off scheduled (or owed: room for it is reserved), and the output
//! goes through a [`Ledger`] so a key never carries two notes at once.

use super::kit::{DIVISION_NAMES, Draw, IdAlloc, KeySet, LOOP_NAMES, Ledger, PositionHash, division_beats, loop_beats, samples_per_beat};
use crate::common::{ParamStore, load_state, save_state};
use aitrack_dsp::{Category, Event, EventKind, EventQueue, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor, Transport};

pub const ID: &str = "midi.echo";

pub mod p {
    pub const DIVISION: u32 = 0;
    pub const SYNC: u32 = 1;
    pub const TIME: u32 = 2;
    pub const REPEATS: u32 = 3;
    pub const FEEDBACK: u32 = 4;
    pub const VEL_STEP: u32 = 5;
    pub const PITCH: u32 = 6;
    pub const LENGTH: u32 = 7;
    pub const GATE: u32 = 8;
    pub const DRY: u32 = 9;
    pub const CHANCE: u32 = 10;
    pub const VEL_RAND: u32 = 11;
    pub const TIME_RAND: u32 = 12;
    pub const PITCH_RAND: u32 = 13;
    pub const SEED: u32 = 14;
    pub const LOOP: u32 = 15;
}

/// Index of `Length` choices.
pub mod length {
    pub const ORIGINAL: f32 = 0.0;
    pub const INTERVAL: f32 = 1.0;
}

const LENGTHS: &[&str] = &["Original", "Interval"];
const SYNCS: &[&str] = &["Free", "Tempo"];
const OFF_ON: &[&str] = &["Off", "On"];

pub const MAX_REPEATS: usize = 16;
/// Notes whose repeats follow their release at the same time.
const HELD: usize = 32;
/// Pending events (a repeat is a note-on plus a note-off).
const QUEUE: usize = 1024;
const MIN_VEL: f32 = 1.0 / 127.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::DIVISION, "Time", 0.0, (DIVISION_NAMES.len() - 1) as f32, 6.0).choices(DIVISION_NAMES),
        ParamInfo::new(p::SYNC, "Sync", 0.0, 1.0, 1.0).choices(SYNCS),
        ParamInfo::new(p::TIME, "Free Time", 1.0, 2_000.0, 250.0).unit("ms").log().short("Free"),
        ParamInfo::new(p::REPEATS, "Repeats", 0.0, MAX_REPEATS as f32, 4.0).choices(&[]).short("Reps"),
        ParamInfo::new(p::FEEDBACK, "Feedback", 0.0, 100.0, 70.0).unit("%").short("Fdbk"),
        ParamInfo::new(p::VEL_STEP, "Velocity Step", -32.0, 32.0, 0.0).choices(&[]).short("Vel Step"),
        ParamInfo::new(p::PITCH, "Pitch", -24.0, 24.0, 0.0).choices(&[]).unit("st"),
        ParamInfo::new(p::LENGTH, "Length", 0.0, 1.0, length::ORIGINAL).choices(LENGTHS),
        ParamInfo::new(p::GATE, "Gate", 5.0, 400.0, 100.0).unit("%").log(),
        ParamInfo::new(p::DRY, "Dry", 0.0, 1.0, 1.0).choices(OFF_ON),
        ParamInfo::new(p::CHANCE, "Chance", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::VEL_RAND, "Velocity Random", 0.0, 100.0, 0.0).unit("%").short("Vel Rnd"),
        ParamInfo::new(p::TIME_RAND, "Time Random", 0.0, 60.0, 0.0).unit("ms").short("Time Rnd"),
        ParamInfo::new(p::PITCH_RAND, "Pitch Random", 0.0, 24.0, 0.0).choices(&[]).unit("st").short("Pitch Rnd"),
        ParamInfo::new(p::SEED, "Seed", 0.0, 9_999.0, 1.0).choices(&[]),
        ParamInfo::new(p::LOOP, "Pattern Loop", 0.0, (LOOP_NAMES.len() - 1) as f32, 0.0).choices(LOOP_NAMES).short("Loop"),
    ]
}

/// The parameters of one block, in working units.
struct Cfg {
    repeats: usize,
    /// Interval between repeats, in samples.
    step: f64,
    feedback: f32,
    /// Added to the velocity of each repeat (0..1 units).
    vel_step: f32,
    pitch: i32,
    original: bool,
    /// Fraction of the length (original mode) or of `step`.
    gate: f64,
    dry: bool,
    chance: f32,
    vel_rand: f32,
    time_rand: f64,
    pitch_rand: f32,
    seed: u32,
    loop_beats: f64,
}

/// The repeats of a note still held, whose note-offs follow its release.
#[derive(Clone, Copy)]
struct Held {
    used: bool,
    channel: u8,
    key: u8,
    on_abs: i64,
    n: usize,
    rep_on: [i64; MAX_REPEATS],
    rep_key: [u8; MAX_REPEATS],
    rep_id: [i32; MAX_REPEATS],
}

const NO_HELD: Held = Held { used: false, channel: 0, key: 0, on_abs: 0, n: 0, rep_on: [0; MAX_REPEATS], rep_key: [0; MAX_REPEATS], rep_id: [0; MAX_REPEATS] };

pub struct NoteEcho {
    info: PluginInfo,
    params: ParamStore,
    queue: EventQueue,
    /// What the queue released this block, to merge with the input.
    scratch: Vec<Event>,
    scratch_dropped: u32,
    ledger: Ledger,
    held: Vec<Held>,
    /// Note-offs promised for repeats already queued as note-ons.
    owed: usize,
    /// Repeats given up for lack of room.
    lost: u64,
    ids: IdAlloc,
    hash: PositionHash,
    /// Originals not played (Dry off), so their release is dropped too.
    muted: KeySet,
    /// Samples processed: the time axis of `Held` and the ledger.
    clock: i64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(NoteEcho::new())
}

impl Default for NoteEcho {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteEcho {
    pub fn new() -> Self {
        let params = params();
        Self {
            info: PluginInfo { id: ID, name: "Note Echo", vendor: "AI-Track", category: Category::MidiEffect, tags: &["midi", "echo", "delay", "repeat"], params: params.clone(), inputs: 2, outputs: 2, has_sidechain: false },
            params: ParamStore::new(&params),
            queue: EventQueue::with_capacity(QUEUE),
            scratch: Vec::with_capacity(QUEUE),
            scratch_dropped: 0,
            ledger: Ledger::new(),
            held: vec![NO_HELD; HELD],
            owed: 0,
            lost: 0,
            ids: IdAlloc::new(),
            hash: PositionHash::new(0x6563_686F),
            muted: KeySet::new(),
            clock: 0,
        }
    }

    fn cfg(&self, t: &Transport, sr: f64) -> Cfg {
        let pr = &self.params;
        let step = if pr.on(p::SYNC) {
            division_beats(pr.choice(p::DIVISION), t.time_sig) * samples_per_beat(t.bpm, sr)
        } else {
            pr.plain(p::TIME) as f64 * 0.001 * sr
        };
        Cfg {
            repeats: (pr.plain(p::REPEATS).round().max(0.0) as usize).min(MAX_REPEATS),
            step: step.max(1.0),
            feedback: pr.plain(p::FEEDBACK) * 0.01,
            vel_step: pr.plain(p::VEL_STEP).round() / 127.0,
            pitch: pr.plain(p::PITCH).round() as i32,
            original: pr.choice(p::LENGTH) == 0,
            gate: pr.plain(p::GATE) as f64 * 0.01,
            dry: pr.on(p::DRY),
            chance: pr.plain(p::CHANCE),
            vel_rand: pr.plain(p::VEL_RAND) * 0.01,
            time_rand: pr.plain(p::TIME_RAND) as f64 * 0.001 * sr,
            pitch_rand: pr.plain(p::PITCH_RAND).round(),
            seed: pr.plain(p::SEED).round() as u32,
            loop_beats: loop_beats(pr.choice(p::LOOP), t.time_sig),
        }
    }

    /// Queue the repeats of a note that starts at `abs`.
    #[allow(clippy::too_many_arguments)]
    fn start(&mut self, c: &Cfg, t: &Transport, sr: f64, offset: u32, abs: i64, channel: u8, key: u8, velocity: f32) {
        // The same key pressed again before its release: the repeats of the
        // earlier press end now.
        self.release(c, offset, abs, channel, key, 0.0);
        if c.repeats == 0 {
            return;
        }
        let mut draw = Draw::new(self.hash.next(c.seed, t, offset, sr, c.loop_beats));
        let slot = if c.original { self.held.iter().position(|h| !h.used) } else { None };
        let mut n = 0usize;
        let mut rep_on = [0i64; MAX_REPEATS];
        let mut rep_key = [0u8; MAX_REPEATS];
        let mut rep_id = [0i32; MAX_REPEATS];
        let interval_len = ((c.gate * c.step).round() as i64).max(1);
        let mut v = velocity;
        for k in 1..=c.repeats {
            v = v * c.feedback + c.vel_step;
            if v < MIN_VEL {
                break;
            }
            v = v.min(1.0);
            // The same four draws per repeat whatever is switched on.
            let (d_chance, d_vel, d_time, d_pitch) = (draw.unit(), draw.signed(), draw.signed(), draw.signed());
            if d_chance * 100.0 >= c.chance {
                continue;
            }
            let vk = (v * (1.0 + d_vel * c.vel_rand)).clamp(MIN_VEL, 1.0);
            let kk = key as i32 + k as i32 * c.pitch + (d_pitch * c.pitch_rand).round() as i32;
            if !(0..=127).contains(&kk) {
                continue;
            }
            let delay = ((k as f64 * c.step + d_time as f64 * c.time_rand).round() as i64).max(1);
            // Room for this note-on and its note-off, queued now or owed.
            if self.queue.len() + self.owed + 2 > self.queue.capacity() {
                self.lost += 1;
                continue;
            }
            let id = self.ids.next();
            let kind_on = EventKind::NoteOn { channel, key: kk as u8, velocity: vk, note_id: id };
            self.queue.schedule(offset, delay, kind_on);
            if slot.is_some() {
                self.owed += 1;
                rep_on[n] = abs + delay;
                rep_key[n] = kk as u8;
                rep_id[n] = id;
                n += 1;
            } else {
                self.queue.schedule(offset, delay + interval_len, EventKind::NoteOff { channel, key: kk as u8, velocity: 0.0, note_id: id });
            }
        }
        if let Some(i) = slot
            && n > 0
        {
            self.held[i] = Held { used: true, channel, key, on_abs: abs, n, rep_on, rep_key, rep_id };
        }
    }

    /// The note was released at `abs`: queue the note-offs of its repeats.
    fn release(&mut self, c: &Cfg, offset: u32, abs: i64, channel: u8, key: u8, velocity: f32) {
        let Some(i) = self.held.iter().position(|h| h.used && h.channel == channel && h.key == key) else { return };
        let h = self.held[i];
        self.held[i].used = false;
        let len = (abs - h.on_abs).max(1);
        let scaled = ((len as f64 * c.gate).round() as i64).max(1);
        for r in 0..h.n {
            // Never before the repeat's own note-on, nor in the past.
            let target = (h.rep_on[r] + scaled).max(h.rep_on[r] + 1).max(abs);
            self.queue.schedule(offset, target - abs, EventKind::NoteOff { channel, key: h.rep_key[r], velocity, note_id: h.rep_id[r] });
        }
        self.owed -= h.n;
    }
}

impl Processor for NoteEcho {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
    }

    fn reset(&mut self) {
        self.queue.clear();
        self.ledger.forget();
        self.muted.clear();
        self.hash.reset();
        self.owed = 0;
        for h in &mut self.held {
            h.used = false;
        }
    }

    /// A MIDI effect is not given audio; whatever arrives stays as it is.
    fn process(&mut self, _ctx: &mut ProcessContext<'_>) {}

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

    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        let c = self.cfg(ctx.transport, ctx.sample_rate);
        let clock = self.clock;
        self.ledger.begin_block(out);
        // First every input event does its scheduling …
        for e in ctx.events {
            let abs = clock + e.offset as i64;
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, .. } => self.start(&c, ctx.transport, ctx.sample_rate, e.offset, abs, channel, key, velocity),
                EventKind::NoteOff { channel, key, velocity, .. } => self.release(&c, e.offset, abs, channel, key, velocity),
                EventKind::AllNotesOff => {
                    self.queue.clear();
                    self.owed = 0;
                    for h in &mut self.held {
                        h.used = false;
                    }
                }
                _ => {}
            }
        }
        // … then what is due in this block comes out of the queue …
        self.scratch.clear();
        self.queue.drain_due(ctx.frames, &mut EventSink::new(&mut self.scratch, &mut self.scratch_dropped));
        // … and both are merged in time order (the input first at a tie).
        let Self { ledger, scratch, muted, .. } = self;
        let (mut ai, mut bi) = (0, 0);
        loop {
            let from_input = match (ctx.events.get(ai), scratch.get(bi)) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(a), Some(b)) => a.offset <= b.offset,
            };
            if from_input {
                let e = ctx.events[ai];
                ai += 1;
                let abs = clock + e.offset as i64;
                match e.kind {
                    EventKind::NoteOn { channel, key, velocity, note_id } => {
                        muted.set(channel, key, !c.dry);
                        if c.dry {
                            ledger.on(out, e.offset, abs, channel, key, velocity, note_id);
                        }
                    }
                    EventKind::NoteOff { channel, key, velocity, note_id } => {
                        if muted.get(channel, key) {
                            muted.set(channel, key, false);
                        } else {
                            ledger.off(out, e.offset, abs, channel, key, velocity, note_id);
                        }
                    }
                    EventKind::AllNotesOff => {
                        out.push(e.offset, e.kind);
                        ledger.forget();
                        muted.clear();
                    }
                    kind => {
                        out.push(e.offset, kind);
                    }
                }
            } else {
                let e = scratch[bi];
                bi += 1;
                let abs = clock + e.offset as i64;
                match e.kind {
                    EventKind::NoteOn { channel, key, velocity, note_id } => {
                        ledger.on(out, e.offset, abs, channel, key, velocity, note_id);
                    }
                    EventKind::NoteOff { channel, key, velocity, note_id } => {
                        ledger.off_own(out, e.offset, abs, channel, key, velocity, note_id);
                    }
                    kind => {
                        out.push(e.offset, kind);
                    }
                }
            }
        }
        self.clock += ctx.frames as i64;
    }

    fn dropped_events(&self) -> u64 {
        self.queue.dropped() + self.lost
    }
}

#[cfg(test)]
mod tests {
    use super::super::fxtest::{Rig, SR, off, on};
    use super::*;

    const BEAT: i64 = 24_000; // 120 bpm
    const EIGHTH: i64 = BEAT / 2;

    fn rig() -> Rig {
        Rig::new(create())
    }

    /// Set a parameter from its plain value.
    fn set(r: &mut Rig, id: u32, plain: f32) {
        let n = r.p.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
        r.p.set_param(id, n);
    }

    fn ons_of(r: &Rig) -> Vec<(i64, u8)> {
        r.ons()
    }

    #[test]
    fn repeats_land_on_the_tempo_grid_and_fall_with_feedback() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 3.0);
        set(&mut r, p::FEEDBACK, 50.0);
        set(&mut r, p::LENGTH, length::INTERVAL);
        set(&mut r, p::GATE, 50.0);
        r.run(&[(1_000, on(60, 0.8)), (5_000, off(60))], 4 * BEAT, 256);
        let notes = r.notes();
        let ons: Vec<_> = notes.iter().filter(|n| n.1).collect();
        assert_eq!(ons.iter().map(|n| n.0).collect::<Vec<_>>(), vec![1_000, 1_000 + EIGHTH, 1_000 + 2 * EIGHTH, 1_000 + 3 * EIGHTH]);
        let vels: Vec<f32> = ons.iter().map(|n| n.3).collect();
        for (v, want) in vels.iter().zip([0.8, 0.4, 0.2, 0.1]) {
            assert!((v - want).abs() < 1e-5, "{vels:?}");
        }
        let offs: Vec<i64> = notes.iter().filter(|n| !n.1).map(|n| n.0).collect();
        assert_eq!(offs, vec![5_000, 1_000 + EIGHTH + EIGHTH / 2, 1_000 + 2 * EIGHTH + EIGHTH / 2, 1_000 + 3 * EIGHTH + EIGHTH / 2]);
        r.assert_balanced();
    }

    #[test]
    fn triplet_dotted_and_free_times() {
        // 1/8T = a third of a beat, 1/8. = three quarters of one.
        for (division, step) in [(4.0, BEAT / 3), (8.0, BEAT * 3 / 4), (9.0, BEAT)] {
            let mut r = rig();
            set(&mut r, p::DIVISION, division);
            set(&mut r, p::REPEATS, 2.0);
            r.run(&[(0, on(60, 1.0)), (100, off(60))], 4 * BEAT, 512);
            let at: Vec<i64> = ons_of(&r).iter().map(|n| n.0).collect();
            assert_eq!(at, vec![0, step, 2 * step], "division {division}");
        }
        let mut r = rig();
        set(&mut r, p::SYNC, 0.0);
        set(&mut r, p::TIME, 125.0);
        set(&mut r, p::REPEATS, 2.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], BEAT, 512);
        assert_eq!(ons_of(&r).iter().map(|n| n.0).collect::<Vec<_>>(), vec![0, 6_000, 12_000]);
    }

    #[test]
    fn the_tempo_sets_the_spacing() {
        let mut r = rig();
        r.bpm = 140.0;
        set(&mut r, p::REPEATS, 1.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], 2 * BEAT, 512);
        // An eighth at 140 bpm: 60/140/2 s.
        assert_eq!(ons_of(&r)[1].0, (60.0 / 140.0 / 2.0 * SR).round() as i64);
    }

    #[test]
    fn original_length_follows_the_played_note_times_gate() {
        for (gate, len) in [(100.0, 6_000), (200.0, 12_000), (50.0, 3_000)] {
            let mut r = rig();
            set(&mut r, p::DIVISION, 9.0); // 1/4
            set(&mut r, p::REPEATS, 2.0);
            set(&mut r, p::GATE, gate);
            r.run(&[(0, on(60, 1.0)), (6_000, off(60))], 4 * BEAT, 256);
            let offs: Vec<i64> = r.notes().iter().filter(|n| !n.1).map(|n| n.0).collect();
            // The original ends at 6000 (a repeat of the same key follows at 24000).
            assert_eq!(offs, vec![6_000, BEAT + len, 2 * BEAT + len], "gate {gate}");
            r.assert_balanced();
        }
    }

    #[test]
    fn a_note_released_before_its_first_repeat_still_ends_every_repeat() {
        let mut r = rig();
        set(&mut r, p::DIVISION, 13.0); // a bar
        set(&mut r, p::REPEATS, 3.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], 20 * BEAT, 1024);
        assert_eq!(ons_of(&r).len(), 4);
        r.assert_balanced();
    }

    #[test]
    fn pitch_steps_pile_up_and_leave_the_range() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 4.0);
        set(&mut r, p::PITCH, 7.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], 8 * BEAT, 256);
        assert_eq!(r.keys(), vec![60, 67, 74, 81, 88]);
        let mut r = rig();
        set(&mut r, p::REPEATS, 4.0);
        set(&mut r, p::PITCH, 12.0);
        r.run(&[(0, on(110, 1.0)), (100, off(110))], 8 * BEAT, 256);
        assert_eq!(r.keys(), vec![110, 122], "repeats above 127 are skipped");
        r.assert_balanced();
    }

    #[test]
    fn a_positive_velocity_step_builds_up_and_clamps() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 6.0);
        set(&mut r, p::FEEDBACK, 100.0);
        set(&mut r, p::VEL_STEP, 16.0);
        r.run(&[(0, on(60, 0.3)), (100, off(60))], 8 * BEAT, 256);
        let v: Vec<f32> = r.notes().iter().filter(|n| n.1).map(|n| n.3).collect();
        assert_eq!(v.len(), 7);
        assert!(v.windows(2).all(|w| w[1] >= w[0]), "{v:?}");
        assert_eq!(*v.last().unwrap(), 1.0);
    }

    #[test]
    fn the_chain_stops_once_it_is_inaudible() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 16.0);
        set(&mut r, p::FEEDBACK, 30.0);
        r.run(&[(0, on(60, 0.5)), (100, off(60))], 16 * BEAT, 256);
        // 0.5, .15, .045, .0135, .004 (< 1/127): four repeats at most.
        assert_eq!(r.ons().len(), 4);
    }

    #[test]
    fn dry_off_keeps_only_the_repeats_and_never_leaves_a_stray_release() {
        let mut r = rig();
        set(&mut r, p::DRY, 0.0);
        set(&mut r, p::REPEATS, 2.0);
        r.run(&[(0, on(60, 1.0)), (6_000, off(60))], 4 * BEAT, 256);
        assert_eq!(r.ons().iter().map(|n| n.0).collect::<Vec<_>>(), vec![EIGHTH, 2 * EIGHTH]);
        assert!(r.notes().iter().all(|n| n.0 >= EIGHTH), "nothing of the original");
        r.assert_balanced();
    }

    #[test]
    fn chance_zero_plays_no_repeats() {
        let mut r = rig();
        set(&mut r, p::CHANCE, 0.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], 4 * BEAT, 256);
        assert_eq!(r.ons().len(), 1);
    }

    #[test]
    fn the_same_result_for_any_block_size() {
        let input = melody();
        for random in [false, true] {
            let mut want: Option<Vec<(i64, bool, u8, u32)>> = None;
            for block in [1usize, 7, 64, 333, 1024, 4096] {
                let mut r = rig();
                configure(&mut r, random);
                r.run(&input, 8 * BEAT, block);
                let got: Vec<_> = r.notes().into_iter().map(|n| (n.0, n.1, n.2, n.3.to_bits())).collect();
                assert!(got.len() > 20);
                match &want {
                    None => want = Some(got),
                    Some(w) => assert_eq!(&got, w, "block {block}, random {random}"),
                }
                r.assert_balanced();
            }
        }
    }

    #[test]
    fn starting_the_transport_late_gives_the_same_notes() {
        let input = melody();
        let play = |start_beats: f64| {
            let mut r = rig();
            configure(&mut r, false);
            r.start_at(start_beats);
            r.run(&input, 8 * BEAT, 300);
            r.notes()
        };
        assert_eq!(play(0.0), play(1000.0));
        assert_eq!(play(0.0), play(123.37));
    }

    #[test]
    fn a_seed_makes_the_variation_repeatable_and_a_new_one_changes_it() {
        let input = melody();
        let run = |seed: f32, start_beats: f64| {
            let mut r = rig();
            configure(&mut r, true);
            set(&mut r, p::SEED, seed);
            r.start_at(start_beats);
            r.run(&input, 8 * BEAT, 512);
            r.notes()
        };
        assert_eq!(run(7.0, 4.0), run(7.0, 4.0));
        assert_ne!(run(7.0, 4.0), run(8.0, 4.0));
        // The draws follow the song position, not the time since the device started.
        assert_ne!(run(7.0, 4.0), run(7.0, 5.0));
    }

    #[test]
    fn a_pattern_loop_gives_every_pass_the_same_draws() {
        let one_bar = |start_beats: f64| {
            let mut r = rig();
            configure(&mut r, true);
            set(&mut r, p::LOOP, 1.0);
            r.start_at(start_beats);
            r.run(&[(0, on(60, 0.9)), (3_000, off(60)), (EIGHTH, on(64, 0.9)), (EIGHTH + 3_000, off(64))], 2 * BEAT, 256);
            r.notes()
        };
        assert_eq!(one_bar(0.0), one_bar(4.0));
        assert_eq!(one_bar(0.0), one_bar(32.0));
    }

    #[test]
    fn repeats_play_with_the_transport_stopped() {
        let mut r = rig();
        r.playing = false;
        set(&mut r, p::REPEATS, 2.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], 2 * BEAT, 256);
        assert_eq!(r.ons().len(), 3);
        r.assert_balanced();
    }

    #[test]
    fn all_notes_off_cancels_what_is_pending() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 4.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60)), (EIGHTH + 50, EventKind::AllNotesOff)], 4 * BEAT, 256);
        assert_eq!(r.ons().len(), 2, "one repeat sounded before the panic");
        r.assert_balanced();
    }

    #[test]
    fn reset_forgets_the_pending_repeats() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 4.0);
        r.run(&[(0, on(60, 1.0)), (100, off(60))], 256, 256);
        r.p.reset();
        let before = r.log.len();
        r.run(&[], 4 * BEAT, 256);
        assert_eq!(r.log.len(), before);
    }

    #[test]
    fn a_full_queue_counts_and_never_leaves_a_note_hanging() {
        for original in [false, true] {
            let mut r = rig();
            set(&mut r, p::REPEATS, 16.0);
            set(&mut r, p::FEEDBACK, 100.0);
            set(&mut r, p::LENGTH, if original { length::ORIGINAL } else { length::INTERVAL });
            // 60 notes held together, then released: far more repeats than the queue holds.
            let mut input = Vec::new();
            for i in 0..60u8 {
                input.push((i as i64 * 10, on(30 + i, 0.9)));
            }
            for i in 0..60u8 {
                input.push((20_000 + i as i64 * 10, off(30 + i)));
            }
            r.run(&input, 40 * BEAT, 512);
            assert!(r.p.dropped_events() > 0, "original {original}");
            r.assert_balanced();
        }
    }

    #[test]
    fn many_notes_held_at_once_beyond_the_table_still_balance() {
        let mut r = rig();
        set(&mut r, p::REPEATS, 2.0);
        let mut input = Vec::new();
        for i in 0..40u8 {
            input.push((i as i64, on(40 + i, 0.8)));
            input.push((30_000 + i as i64, off(40 + i)));
        }
        r.run(&input, 10 * BEAT, 512);
        r.assert_balanced();
    }

    #[test]
    fn a_key_never_sounds_twice_at_once_and_everything_is_paired_whatever_the_settings() {
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for round in 0..40 {
            let mut r = rig();
            for info in r.p.info().params.clone() {
                let v = (rnd() % 1001) as f32 / 1000.0;
                r.p.set_param(info.id, v);
            }
            set(&mut r, p::REPEATS, (rnd() % 8) as f32);
            let mut input = Vec::new();
            let mut t = 0i64;
            // A key is never pressed again before its release (the input is well formed).
            let mut free_at = [0i64; 128];
            for _ in 0..60 {
                t += (rnd() % 9_000) as i64;
                let key = 36 + (rnd() % 12) as u8;
                if t <= free_at[key as usize] {
                    continue;
                }
                let len = 2 + (rnd() % 30_000) as i64;
                free_at[key as usize] = t + len;
                input.push((t, on(key, 0.2 + (rnd() % 80) as f32 / 100.0)));
                input.push((t + len, off(key)));
            }
            input.sort_by_key(|e| e.0);
            let block = 1 + (rnd() % 700) as usize;
            r.run(&input, t + 100 * BEAT, block);
            r.assert_balanced();
            let _ = round;
        }
    }

    fn melody() -> Vec<(i64, EventKind)> {
        let mut v = Vec::new();
        for (i, key) in [48u8, 51, 55, 60, 48, 53, 58, 62].iter().enumerate() {
            let t = i as i64 * EIGHTH + (i as i64 * 37) % 90;
            v.push((t, on(*key, 0.5 + 0.06 * i as f32)));
            v.push((t + 5_000, off(*key)));
        }
        // A chord on the same sample.
        v.push((4 * BEAT, on(40, 0.9)));
        v.push((4 * BEAT, on(47, 0.9)));
        v.push((4 * BEAT + 9_000, off(40)));
        v.push((4 * BEAT + 9_500, off(47)));
        v.sort_by_key(|e| e.0);
        v
    }

    fn configure(r: &mut Rig, random: bool) {
        set(r, p::DIVISION, 5.0);
        set(r, p::REPEATS, 5.0);
        set(r, p::FEEDBACK, 80.0);
        set(r, p::PITCH, 3.0);
        if random {
            set(r, p::CHANCE, 80.0);
            set(r, p::VEL_RAND, 40.0);
            set(r, p::TIME_RAND, 20.0);
            set(r, p::PITCH_RAND, 2.0);
        }
    }
}
