//! `midi.random` — varies the pitch, velocity and timing of notes.
//!
//! * **Chance**: the share of notes that are altered (all three dimensions
//!   at once); the others go through untouched.
//! * **Pitch**: an altered note moves by `n × Interval` semitones, with `n`
//!   drawn from `1..=Choices` (`Sign`: up, down or both). `Choices` 0 leaves
//!   the pitch alone. Results outside 0..=127 fold back by octaves. With
//!   **Limit to Scale** the result snaps to the nearest note of
//!   `Root`/`Scale` (the editor fills them from the project scale when
//!   `Project Scale` is on; the device itself only reads `Root` and `Scale`).
//! * **Velocity**: ±`Velocity` of the full range (never below one step).
//! * **Delay**: up to `Delay` ms later, the release moving with the note so
//!   its length stays.
//! * **Seed / Pattern Loop**: every draw comes from a hash of the seed and
//!   the song position of the note — never from a running generator — so a
//!   render reproduces playback exactly and any block size gives the same
//!   notes. With `Pattern Loop` a pattern of that many bars gets the same
//!   draws on every pass.
//!
//! A held note remembers what it became, so its release follows even if the
//! knobs move. All output goes through a [`Ledger`], keeping it paired.

use super::kit::{Draw, Ledger, LOOP_NAMES, PositionHash, fold_octaves, loop_beats};
use super::theory::{ROOT_NAMES, SCALE_NAMES, Snap, scale_mask, snap_note};
use crate::common::{ParamStore, load_state, save_state};
use aitrack_dsp::{Category, Event, EventKind, EventQueue, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.random";

pub mod p {
    pub const CHANCE: u32 = 0;
    pub const INTERVAL: u32 = 1;
    pub const CHOICES: u32 = 2;
    pub const SIGN: u32 = 3;
    pub const VELOCITY: u32 = 4;
    pub const DELAY: u32 = 5;
    pub const SNAP: u32 = 6;
    pub const PROJECT: u32 = 7;
    pub const ROOT: u32 = 8;
    pub const SCALE: u32 = 9;
    pub const SEED: u32 = 10;
    pub const LOOP: u32 = 11;
}

/// `Sign` choices.
pub mod sign {
    pub const UP: f32 = 0.0;
    pub const DOWN: f32 = 1.0;
    pub const BOTH: f32 = 2.0;
}

const SIGNS: &[&str] = &["Up", "Down", "Both"];
const OFF_ON: &[&str] = &["Off", "On"];

/// Pending events (a note is an on and an off).
const QUEUE: usize = 1024;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::CHANCE, "Chance", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::INTERVAL, "Interval", 1.0, 12.0, 1.0).choices(&[]).unit("st").short("Step"),
        ParamInfo::new(p::CHOICES, "Choices", 0.0, 24.0, 12.0).choices(&[]),
        ParamInfo::new(p::SIGN, "Sign", 0.0, 2.0, sign::BOTH).choices(SIGNS),
        ParamInfo::new(p::VELOCITY, "Velocity", 0.0, 100.0, 0.0).unit("%").short("Vel"),
        ParamInfo::new(p::DELAY, "Delay", 0.0, 120.0, 0.0).unit("ms").power(2.0),
        ParamInfo::new(p::SNAP, "Limit to Scale", 0.0, 1.0, 0.0).choices(OFF_ON).short("Snap"),
        ParamInfo::new(p::PROJECT, "Project Scale", 0.0, 1.0, 0.0).choices(OFF_ON).short("Project"),
        ParamInfo::new(p::ROOT, "Root", 0.0, 11.0, 0.0).choices(ROOT_NAMES),
        ParamInfo::new(p::SCALE, "Scale", 0.0, (SCALE_NAMES.len() - 1) as f32, 1.0).choices(SCALE_NAMES),
        ParamInfo::new(p::SEED, "Seed", 0.0, 9_999.0, 1.0).choices(&[]),
        ParamInfo::new(p::LOOP, "Pattern Loop", 0.0, (LOOP_NAMES.len() - 1) as f32, 0.0).choices(LOOP_NAMES).short("Loop"),
    ]
}

/// What a played key became.
#[derive(Clone, Copy)]
struct Map {
    used: bool,
    key: u8,
    id: i32,
    /// Samples the note was delayed by.
    delay: i64,
}

const UNUSED: Map = Map { used: false, key: 0, id: 0, delay: 0 };
/// `Map::delay` of a note that was given up.
const DROPPED: i64 = -1;

pub struct Random {
    info: PluginInfo,
    params: ParamStore,
    queue: EventQueue,
    scratch: Vec<Event>,
    scratch_dropped: u32,
    ledger: Ledger,
    maps: Vec<Map>,
    /// Note-offs promised for the note-ons already queued.
    owed: usize,
    lost: u64,
    hash: PositionHash,
    clock: i64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Random::new())
}

impl Default for Random {
    fn default() -> Self {
        Self::new()
    }
}

impl Random {
    pub fn new() -> Self {
        let params = params();
        Self {
            info: PluginInfo { id: ID, name: "Random", vendor: "AI-Track", category: Category::MidiEffect, tags: &["midi", "random", "humanize", "variation"], params: params.clone(), inputs: 2, outputs: 2, has_sidechain: false },
            params: ParamStore::new(&params),
            queue: EventQueue::with_capacity(QUEUE),
            scratch: Vec::with_capacity(QUEUE),
            scratch_dropped: 0,
            ledger: Ledger::new(),
            maps: vec![UNUSED; 2048],
            owed: 0,
            lost: 0,
            hash: PositionHash::new(0x7261_6E64),
            clock: 0,
        }
    }
}

impl Random {
    /// Queue the release of the note held on `slot` (if any), moved by the
    /// delay the note was given.
    fn finish(&mut self, slot: usize, channel: u8, offset: u32, velocity: f32) {
        let m = self.maps[slot];
        if !m.used {
            return;
        }
        self.maps[slot].used = false;
        if m.delay == DROPPED {
            return;
        }
        self.owed -= 1;
        self.queue.schedule(offset, m.delay, EventKind::NoteOff { channel, key: m.key, velocity, note_id: m.id });
    }
}

impl Processor for Random {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
    }

    fn reset(&mut self) {
        self.queue.clear();
        self.ledger.forget();
        self.maps.fill(UNUSED);
        self.hash.reset();
        self.owed = 0;
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
        let chance = pr.plain(p::CHANCE);
        let step = pr.plain(p::INTERVAL).round() as i32;
        let choices = pr.plain(p::CHOICES).round() as i32;
        let direction = pr.choice(p::SIGN);
        let vel_range = pr.plain(p::VELOCITY) * 0.01;
        let delay_max = pr.plain(p::DELAY) as f64 * 0.001 * sr;
        let mask = if pr.on(p::SNAP) { scale_mask(pr.choice(p::SCALE), pr.choice(p::ROOT)) } else { 0 };
        let seed = pr.plain(p::SEED).round() as u32;
        let loop_len = loop_beats(pr.choice(p::LOOP), ctx.transport.time_sig);
        let clock = self.clock;
        self.ledger.begin_block(out);
        // First the notes are decided and queued …
        for e in ctx.events {
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } => {
                    let slot = (channel as usize & 15) * 128 + key as usize;
                    // A key pressed again before its release: the old note ends first.
                    self.finish(slot, channel, e.offset, 0.0);
                    if self.queue.len() + self.owed + 2 > self.queue.capacity() {
                        // No room to follow it: dropped, release included.
                        self.lost += 1;
                        self.maps[slot] = Map { used: true, key, id: note_id, delay: DROPPED };
                        continue;
                    }
                    // The same four draws per note whatever is switched on.
                    let mut draw = Draw::new(self.hash.next(seed, ctx.transport, e.offset, sr, loop_len));
                    let (d_chance, d_pick, d_vel, d_time) = (draw.unit(), draw.next_u64(), draw.signed(), draw.unit());
                    let (mut out_key, mut vel, mut delay) = (key, velocity, 0i64);
                    if d_chance * 100.0 < chance {
                        if choices > 0 {
                            let c = choices as u64;
                            let n = match direction {
                                0 => 1 + (d_pick % c) as i32,
                                1 => -1 - (d_pick % c) as i32,
                                _ => {
                                    let r = (d_pick % (2 * c)) as i32;
                                    if r < choices { r - choices } else { r - choices + 1 }
                                }
                            };
                            out_key = fold_octaves(key as i32 + n * step);
                            if mask != 0 {
                                out_key = snap_note(mask, out_key, Snap::Nearest, false);
                            }
                        }
                        vel = (velocity + d_vel * vel_range).clamp(1.0 / 127.0, 1.0);
                        delay = (d_time as f64 * delay_max).round() as i64;
                    }
                    self.queue.schedule(e.offset, delay, EventKind::NoteOn { channel, key: out_key, velocity: vel, note_id });
                    self.owed += 1;
                    self.maps[slot] = Map { used: true, key: out_key, id: note_id, delay };
                }
                EventKind::NoteOff { channel, key, velocity, note_id } => {
                    let slot = (channel as usize & 15) * 128 + key as usize;
                    if self.maps[slot].used {
                        self.finish(slot, channel, e.offset, velocity);
                    } else {
                        // A note this device never saw begin: its release goes straight on.
                        self.queue.schedule(e.offset, 0, EventKind::NoteOff { channel, key, velocity, note_id });
                    }
                }
                EventKind::NoteExpression { note_id, key, expr, value } => {
                    let m = self.maps[key as usize & 127];
                    if m.used && m.delay == DROPPED {
                        continue;
                    }
                    let (k, d) = if m.used { (m.key, m.delay) } else { (key, 0) };
                    self.queue.schedule(e.offset, d, EventKind::NoteExpression { note_id, key: k, expr, value });
                }
                EventKind::AllNotesOff => {
                    self.queue.clear();
                    self.maps.fill(UNUSED);
                    self.owed = 0;
                }
                _ => {}
            }
        }
        // … then what is due in this block comes out of the queue …
        self.scratch.clear();
        self.queue.drain_due(ctx.frames, &mut EventSink::new(&mut self.scratch, &mut self.scratch_dropped));
        // … and merges, in time order, with the events that are not notes.
        let Self { ledger, scratch, .. } = self;
        let mut inputs = ctx.events.iter().filter(|e| !matches!(e.kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. } | EventKind::NoteExpression { .. })).peekable();
        let mut bi = 0;
        loop {
            let from_input = match (inputs.peek(), scratch.get(bi)) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(a), Some(b)) => a.offset <= b.offset,
            };
            if from_input {
                let Some(e) = inputs.next() else { break };
                out.push(e.offset, e.kind);
                if e.kind == EventKind::AllNotesOff {
                    ledger.forget();
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
                        ledger.off(out, e.offset, abs, channel, key, velocity, note_id);
                    }
                    kind => {
                        out.push(e.offset, kind);
                    }
                }
            }
        }
        self.clock += ctx.frames as i64;
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn dropped_events(&self) -> u64 {
        self.queue.dropped() + self.lost
    }
}

#[cfg(test)]
mod tests {
    use super::super::fxtest::{Rig, SR, off, on};
    use super::*;

    const BEAT: i64 = 24_000;

    fn rig() -> Rig {
        Rig::new(create())
    }

    fn set(r: &mut Rig, id: u32, plain: f32) {
        let n = r.p.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
        r.p.set_param(id, n);
    }

    /// `n` notes of key `key`, one every `gap` samples, each 1000 long.
    fn notes(n: usize, key: u8, gap: i64) -> Vec<(i64, EventKind)> {
        let mut v = Vec::new();
        for i in 0..n as i64 {
            v.push((i * gap, on(key, 0.6)));
            v.push((i * gap + 1_000, off(key)));
        }
        v
    }

    #[test]
    fn untouched_with_chance_zero_or_nothing_to_vary() {
        let mut r = rig();
        set(&mut r, p::CHANCE, 0.0);
        r.run(&notes(20, 60, 3_000), 100_000, 256);
        assert!(r.keys().iter().all(|k| *k == 60));
        let mut r = rig();
        set(&mut r, p::CHOICES, 0.0);
        r.run(&notes(20, 60, 3_000), 100_000, 256);
        assert_eq!(r.notes().len(), 40);
        assert!(r.notes().iter().all(|n| n.2 == 60 && (!n.1 || n.3 == 0.6)));
    }

    #[test]
    fn up_down_and_both_signs_with_a_fixed_interval() {
        for (s, want) in [(sign::UP, vec![72]), (sign::DOWN, vec![48]), (sign::BOTH, vec![48, 72])] {
            let mut r = rig();
            set(&mut r, p::CHOICES, 1.0);
            set(&mut r, p::INTERVAL, 12.0);
            set(&mut r, p::SIGN, s);
            r.run(&notes(60, 60, 3_000), 200_000, 256);
            let mut keys = r.keys();
            keys.sort();
            keys.dedup();
            assert_eq!(keys, want, "sign {s}");
            r.assert_balanced();
        }
    }

    #[test]
    fn choices_bound_the_size_of_the_jump() {
        let mut r = rig();
        set(&mut r, p::CHOICES, 4.0);
        set(&mut r, p::INTERVAL, 3.0);
        set(&mut r, p::SIGN, sign::UP);
        r.run(&notes(200, 60, 2_000), 500_000, 256);
        let mut keys = r.keys();
        keys.sort();
        keys.dedup();
        assert_eq!(keys, vec![63, 66, 69, 72]);
    }

    #[test]
    fn the_chance_is_the_share_of_altered_notes() {
        let mut r = rig();
        set(&mut r, p::CHANCE, 50.0);
        set(&mut r, p::CHOICES, 1.0);
        set(&mut r, p::INTERVAL, 12.0);
        set(&mut r, p::SIGN, sign::UP);
        r.run(&notes(400, 60, 2_000), 900_000, 256);
        let altered = r.keys().iter().filter(|k| **k == 72).count();
        assert!((140..=260).contains(&altered), "{altered} of 400");
    }

    #[test]
    fn the_result_can_be_limited_to_a_scale() {
        let mut r = rig();
        set(&mut r, p::SNAP, 1.0);
        set(&mut r, p::ROOT, 9.0); // A
        set(&mut r, p::SCALE, 10.0); // minor pentatonic: A C D E G
        let mut input = Vec::new();
        for i in 0..300i64 {
            input.push((i * 2_000, on(40 + (i % 40) as u8, 0.6)));
            input.push((i * 2_000 + 500, off(40 + (i % 40) as u8)));
        }
        r.run(&input, 700_000, 256);
        let mask = scale_mask(10, 9);
        assert!(r.keys().iter().all(|k| mask & (1 << (k % 12)) != 0), "{:?}", r.keys());
        r.assert_balanced();
    }

    #[test]
    fn keys_stay_inside_the_midi_range() {
        for key in [0u8, 1, 126, 127] {
            let mut r = rig();
            set(&mut r, p::CHOICES, 24.0);
            set(&mut r, p::INTERVAL, 12.0);
            r.run(&notes(100, key, 2_000), 300_000, 256);
            assert_eq!(r.ons().len(), 100);
            r.assert_balanced();
        }
    }

    #[test]
    fn velocity_varies_inside_its_range_and_never_reaches_zero() {
        let mut r = rig();
        set(&mut r, p::CHOICES, 0.0);
        set(&mut r, p::VELOCITY, 20.0);
        r.run(&notes(200, 60, 2_000), 500_000, 256);
        let v: Vec<f32> = r.notes().iter().filter(|n| n.1).map(|n| n.3).collect();
        assert!(v.iter().all(|x| (0.39..=0.81).contains(x)), "{v:?}");
        assert!(v.iter().any(|x| *x < 0.5) && v.iter().any(|x| *x > 0.7));
        let mut r = rig();
        set(&mut r, p::CHOICES, 0.0);
        set(&mut r, p::VELOCITY, 100.0);
        r.run(&notes(200, 60, 2_000), 500_000, 256);
        assert!(r.notes().iter().filter(|n| n.1).all(|n| n.3 >= 1.0 / 127.0 - 1e-6 && n.3 <= 1.0));
    }

    #[test]
    fn a_delay_moves_note_and_release_together() {
        let mut r = rig();
        set(&mut r, p::CHOICES, 0.0);
        set(&mut r, p::DELAY, 50.0);
        let input = notes(100, 60, 8_000);
        r.run(&input, 900_000, 256);
        let n = r.notes();
        let mut late = 0;
        for i in 0..100 {
            let (a, b) = (n[2 * i], n[2 * i + 1]);
            assert!(a.1 && !b.1);
            let d = a.0 - i as i64 * 8_000;
            assert!((0..=2_400).contains(&d), "delay {d}");
            assert_eq!(b.0 - a.0, 1_000, "length kept");
            late += (d > 0) as usize;
        }
        assert!(late > 80);
        r.assert_balanced();
    }

    #[test]
    fn the_release_follows_the_key_the_note_became() {
        let mut r = rig();
        set(&mut r, p::CHOICES, 1.0);
        set(&mut r, p::INTERVAL, 5.0);
        set(&mut r, p::SIGN, sign::UP);
        r.run(&[(0, on(60, 0.6))], 100, 100);
        set(&mut r, p::INTERVAL, 12.0);
        set(&mut r, p::SIGN, sign::DOWN);
        r.run(&[(0, off(60))], 100, 100);
        assert_eq!(r.notes().iter().map(|n| (n.1, n.2)).collect::<Vec<_>>(), vec![(true, 65), (false, 65)]);
    }

    #[test]
    fn same_seed_same_notes_any_block_size_other_seed_other_notes() {
        let input = notes(60, 60, 5_000);
        let run = |seed: f32, block: usize, start_beats: f64| {
            let mut r = rig();
            set(&mut r, p::DELAY, 30.0);
            set(&mut r, p::VELOCITY, 30.0);
            set(&mut r, p::SEED, seed);
            r.start_at(start_beats);
            r.run(&input, 400_000, block);
            r.notes().into_iter().map(|n| (n.0, n.1, n.2, n.3.to_bits())).collect::<Vec<_>>()
        };
        let base = run(5.0, 256, 8.0);
        for block in [1usize, 13, 100, 4096] {
            assert_eq!(run(5.0, block, 8.0), base, "block {block}");
        }
        assert_ne!(run(6.0, 256, 8.0), base);
        // The draws belong to the song position.
        assert_ne!(run(5.0, 256, 9.0), base);
    }

    #[test]
    fn a_pattern_loop_repeats_the_draws_every_pass() {
        let run = |start_beats: f64| {
            let mut r = rig();
            set(&mut r, p::DELAY, 30.0);
            set(&mut r, p::SEED, 3.0);
            set(&mut r, p::LOOP, 1.0); // one bar
            r.start_at(start_beats);
            r.run(&notes(16, 60, BEAT / 4), 4 * BEAT, 300);
            r.notes().into_iter().map(|n| (n.0, n.1, n.2, n.3.to_bits())).collect::<Vec<_>>()
        };
        assert_eq!(run(0.0), run(4.0));
        assert_eq!(run(0.0), run(40.0));
    }

    #[test]
    fn with_the_transport_stopped_repeated_notes_still_vary() {
        let mut r = rig();
        r.playing = false;
        set(&mut r, p::CHOICES, 1.0);
        set(&mut r, p::INTERVAL, 12.0);
        r.run(&notes(60, 60, 3_000), 200_000, 256);
        let mut keys = r.keys();
        keys.sort();
        keys.dedup();
        assert_eq!(keys, vec![48, 72]);
    }

    #[test]
    fn other_events_pass_and_all_notes_off_clears() {
        let mut r = rig();
        set(&mut r, p::DELAY, 100.0);
        let bend = EventKind::PitchBend { channel: 0, value: 0.25 };
        r.run(&[(0, on(60, 0.6)), (10, bend), (20, EventKind::AllNotesOff)], 10_000, 256);
        assert!(r.log.iter().any(|(_, k)| *k == bend));
        r.assert_balanced();
        r.run(&[(0, on(61, 0.6)), (5, off(61))], 20_000, 256);
        r.assert_balanced();
    }

    #[test]
    fn a_swarm_of_notes_stays_paired_whatever_the_settings() {
        let mut seed = 0xC0FFEE_u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..40 {
            let mut r = rig();
            for info in r.p.info().params.clone() {
                r.p.set_param(info.id, (rnd() % 1001) as f32 / 1000.0);
            }
            let mut input = Vec::new();
            let mut free_at = [0i64; 128];
            let mut t = 0i64;
            for _ in 0..80 {
                t += (rnd() % 4_000) as i64;
                let key = 36 + (rnd() % 24) as u8;
                if t <= free_at[key as usize] {
                    continue;
                }
                let len = 2 + (rnd() % 20_000) as i64;
                free_at[key as usize] = t + len;
                input.push((t, on(key, 0.1 + (rnd() % 90) as f32 / 100.0)));
                input.push((t + len, off(key)));
            }
            input.sort_by_key(|e| e.0);
            r.run(&input, t + 40_000, 1 + (rnd() % 500) as usize);
            r.assert_balanced();
        }
        let _ = SR;
    }

    #[test]
    fn a_full_queue_counts_and_stays_paired() {
        let mut r = rig();
        let mut input = Vec::new();
        // 16 channels x 40 keys held together: more than the queue can follow.
        for c in 0..16u8 {
            for k in 0..40u8 {
                let id = c as i32 * 128 + k as i32;
                input.push((k as i64, EventKind::NoteOn { channel: c, key: 30 + k, velocity: 0.6, note_id: id }));
            }
        }
        for c in 0..16u8 {
            for k in 0..40u8 {
                let id = c as i32 * 128 + k as i32;
                input.push((30_000 + k as i64, EventKind::NoteOff { channel: c, key: 30 + k, velocity: 0.0, note_id: id }));
            }
        }
        input.sort_by_key(|e| e.0);
        r.run(&input, 100_000, 512);
        assert!(r.p.dropped_events() > 0);
        r.assert_balanced();
    }
}
