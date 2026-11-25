//! `midi.chord` — one key in, up to six notes out.
//!
//! Every voice has a switch, an interval in semitones (−36…+36 from the
//! played key), a velocity level (0…200 %, 0 mutes it) and a chance (0…100 %).
//! Voice 1 defaults to the played key itself, so the default is a major triad.
//! With *Fit to Scale* every voice that is not the played key itself (interval
//! 0) is moved to the nearest note of the chosen scale (ties go down), which
//! turns a fixed stack of intervals into a diatonic chord whatever key is
//! pressed.
//!
//! Root and Scale use the scale list and order of `aitrack_model::ScaleKind`
//! (see [`theory`](crate::midi::theory)). *Follow Project* is only a flag for
//! the app: when it is on the editor writes the project's scale into Root and
//! Scale; the device itself never reads it. Changing the project's scale does
//! not rewrite parameters already saved.
//!
//! **Chance.** Each press rolls once per voice; the voice sounds when its roll
//! is below its chance. The rolls come from a hash of *Seed* and the song
//! position of the press (the same as in `midi.echo`), so playback and an
//! export agree and the block size makes no difference; *Pattern Loop* makes
//! them repeat every N bars. Every press consumes the same six rolls whatever
//! is switched on, so changing one chance never moves what another voice does.
//!
//! **Strum.** *Strum* is the time between the first and the last note of the
//! chord (0 = all at once, which is the old behaviour to the sample). The
//! notes go in order of pitch, up or down; *Down-Up* goes down on the beat and
//! up between the beats (the nearest eighth of the song position decides) and
//! *Random* shuffles them by the same hash. *Tension* bends the gaps
//! (`delay = time · t^(4^tension)` for `t` in 0…1 along the strum, so + widens
//! them towards the end and − narrows them) and *Strum Velocity* tilts the
//! velocities along the order (`1 + velocity · (t − ½)`), even with no time.
//! A note that has not started when its key is released never starts; the
//! ones that did end with the key.
//!
//! **Learn.** While it is on the played notes pass through, one note per key,
//! and the device collects every key of a gesture (from the first press until
//! all keys are up): the six lowest are published through `meter()` (see
//! [`meter`]) and the editor turns them into voices with [`learn_edits`].
//!
//! Each played key remembers the keys it started, so its note-off ends exactly
//! those even if the parameters changed meanwhile. Two played keys that land
//! on the same output key share it: the second note-on retriggers it (note-off
//! then note-on) and the key ends when the last of them is released.

use crate::common::{ParamStore, write_meter};
use crate::midi::kit::{Draw, IdAlloc, LOOP_NAMES, PositionHash, loop_beats};
use crate::midi::theory::{ROOT_NAMES, SCALE_NAMES, Snap, scale_mask, snap_note};
use aitrack_dsp::{Category, EventKind, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.chord";

pub const VOICES: usize = 6;

pub mod p {
    /// Voice `v` (0..6): on/off at `ON + 3v`, interval at `INTERVAL + 3v`, level at `LEVEL + 3v`.
    pub const ON: u32 = 0;
    pub const INTERVAL: u32 = 1;
    pub const LEVEL: u32 = 2;
    pub const FIT: u32 = 18;
    pub const ROOT: u32 = 19;
    pub const SCALE: u32 = 20;
    pub const FOLLOW_PROJECT: u32 = 21;
    /// Chance of voice `v` (0..6): `CHANCE0 + v`.
    pub const CHANCE0: u32 = 22;
    pub const STRUM: u32 = 28;
    pub const STRUM_DIR: u32 = 29;
    pub const STRUM_TENSION: u32 = 30;
    pub const STRUM_VEL: u32 = 31;
    pub const SEED: u32 = 32;
    pub const LOOP: u32 = 33;
    pub const LEARN: u32 = 34;

    pub const fn on(v: usize) -> u32 {
        ON + 3 * v as u32
    }
    pub const fn interval(v: usize) -> u32 {
        INTERVAL + 3 * v as u32
    }
    pub const fn level(v: usize) -> u32 {
        LEVEL + 3 * v as u32
    }
    pub const fn chance(v: usize) -> u32 {
        CHANCE0 + v as u32
    }
}

/// Values of `Strum Direction`.
pub mod dir {
    pub const UP: f32 = 0.0;
    pub const DOWN: f32 = 1.0;
    pub const DOWN_UP: f32 = 2.0;
    pub const RANDOM: f32 = 3.0;
}

/// Layout of `meter()`: `[serial, n, c0…c5, live]`.
pub mod meter {
    /// Counts the chords learned so far (wraps at 2^24, exact in an `f32`).
    pub const SERIAL: usize = 0;
    /// Keys of the last learned chord.
    pub const COUNT: usize = 1;
    pub const KEYS: usize = 2;
    /// Keys pressed so far in the gesture being learned now.
    pub const LIVE: usize = 8;
    pub const LEN: usize = 9;
}

const OFF_ON: &[&str] = &["Off", "On"];
const VOICE_ON: [&str; VOICES] = ["Voice 1", "Voice 2", "Voice 3", "Voice 4", "Voice 5", "Voice 6"];
const VOICE_INTERVAL: [&str; VOICES] = ["Interval 1", "Interval 2", "Interval 3", "Interval 4", "Interval 5", "Interval 6"];
const VOICE_LEVEL: [&str; VOICES] = ["Level 1", "Level 2", "Level 3", "Level 4", "Level 5", "Level 6"];
const VOICE_CHANCE: [&str; VOICES] = ["Chance 1", "Chance 2", "Chance 3", "Chance 4", "Chance 5", "Chance 6"];
pub const DIRECTIONS: &[&str] = &["Up", "Down", "Down-Up", "Random"];
/// Default stack: the played key, a major third and a fifth; the rest off.
const DEFAULT_INTERVALS: [f32; VOICES] = [0.0, 4.0, 7.0, 12.0, 16.0, 19.0];

const MAX_NOTES: usize = 64;
/// Strummed notes still to start: every held key has at most five (the first
/// note of a strum goes out at once).
const MAX_PENDING: usize = MAX_NOTES * VOICES;
const MIN_VEL: f32 = 1.0 / 127.0;
/// The longest strum, ms (the parameter's maximum).
pub const STRUM_MAX_MS: f32 = 500.0;

pub fn params() -> Vec<ParamInfo> {
    let mut v = Vec::new();
    for i in 0..VOICES {
        v.push(ParamInfo::new(p::on(i), VOICE_ON[i], 0.0, 1.0, if i < 3 { 1.0 } else { 0.0 }).choices(OFF_ON));
        v.push(ParamInfo::new(p::interval(i), VOICE_INTERVAL[i], -36.0, 36.0, DEFAULT_INTERVALS[i]).unit("st"));
        v.push(ParamInfo::new(p::level(i), VOICE_LEVEL[i], 0.0, 200.0, 100.0).unit("%"));
    }
    v.push(ParamInfo::new(p::FIT, "Fit to Scale", 0.0, 1.0, 0.0).choices(OFF_ON).short("Fit"));
    v.push(ParamInfo::new(p::ROOT, "Root", 0.0, 11.0, 0.0).choices(ROOT_NAMES));
    v.push(ParamInfo::new(p::SCALE, "Scale", 0.0, 12.0, 1.0).choices(SCALE_NAMES));
    v.push(ParamInfo::new(p::FOLLOW_PROJECT, "Follow Project", 0.0, 1.0, 0.0).choices(OFF_ON).short("Follow"));
    for i in 0..VOICES {
        v.push(ParamInfo::new(p::chance(i), VOICE_CHANCE[i], 0.0, 100.0, 100.0).unit("%"));
    }
    v.push(ParamInfo::new(p::STRUM, "Strum", 0.0, STRUM_MAX_MS, 0.0).unit("ms"));
    v.push(ParamInfo::new(p::STRUM_DIR, "Strum Direction", 0.0, 3.0, dir::UP).choices(DIRECTIONS).short("Direction"));
    v.push(ParamInfo::new(p::STRUM_TENSION, "Strum Tension", -100.0, 100.0, 0.0).unit("%").short("Tension"));
    v.push(ParamInfo::new(p::STRUM_VEL, "Strum Velocity", -100.0, 100.0, 0.0).unit("%").short("Vel"));
    v.push(ParamInfo::new(p::SEED, "Seed", 0.0, 9_999.0, 1.0).choices(&[]));
    v.push(ParamInfo::new(p::LOOP, "Pattern Loop", 0.0, (LOOP_NAMES.len() - 1) as f32, 0.0).choices(LOOP_NAMES).short("Loop"));
    v.push(ParamInfo::new(p::LEARN, "Learn", 0.0, 1.0, 0.0).choices(OFF_ON));
    v
}

/// Where note `i` of `n` starts along a strum, as a fraction (0…1) of its
/// time: `t^(4^tension)` with `t = i / (n − 1)` and `tension` in −1…1. The
/// first note is at 0 and the last at 1 whatever the tension; a lone note is
/// at 0. The editor draws the strum with this same function.
pub fn strum_position(i: usize, n: usize, tension: f64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    (i as f64 / (n - 1) as f64).powf(4f64.powf(tension))
}

/// The velocity factor of note `i` of `n` along a strum for a *Strum
/// Velocity* of `ramp` (−1…1): `1 + ramp · (t − ½)`, so the first note is at
/// `1 − ramp/2` and the last at `1 + ramp/2`; a lone note keeps its velocity.
pub fn strum_gain(i: usize, n: usize, ramp: f32) -> f32 {
    if n < 2 {
        return 1.0;
    }
    1.0 + ramp * (i as f32 / (n - 1) as f32 - 0.5)
}

/// The voices a learned chord becomes, as plain `(parameter, value)` edits:
/// the lowest key is the played key itself (interval 0), the rest follow at
/// their distance above it, all at 100 %; voices beyond the chord are
/// switched off, and Learn goes back to Off. At most [`VOICES`] keys count
/// (the lowest) and a key more than three octaves above the lowest does not
/// fit an interval and is left out. Nothing learned, nothing to edit.
pub fn learn_edits(keys: &[u8]) -> Vec<(u32, f32)> {
    let mut sorted: Vec<u8> = keys.iter().copied().filter(|k| *k < 128).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let Some(&lowest) = sorted.first() else { return Vec::new() };
    let mut edits = Vec::new();
    let mut voice = 0;
    for &k in &sorted {
        let interval = i32::from(k) - i32::from(lowest);
        if voice == VOICES || interval > 36 {
            continue;
        }
        edits.extend([(p::on(voice), 1.0), (p::interval(voice), interval as f32), (p::level(voice), 100.0)]);
        voice += 1;
    }
    edits.extend((voice..VOICES).map(|v| (p::on(v), 0.0)));
    edits.push((p::LEARN, 0.0));
    edits
}

/// A played key and the output keys it started.
#[derive(Clone, Copy)]
struct Played {
    channel: u8,
    key: u8,
    out: [u8; VOICES],
    n: u8,
}

/// A strummed note that has not started yet.
#[derive(Clone, Copy)]
struct Pending {
    /// Sample of the device's own clock at which it starts.
    due: i64,
    channel: u8,
    /// The played key whose chord it belongs to.
    played: u8,
    key: u8,
    velocity: f32,
}

const NO_PENDING: Pending = Pending { due: 0, channel: 0, played: 0, key: 0, velocity: 0.0 };

#[derive(Clone, Copy)]
struct Voice {
    on: bool,
    interval: i32,
    level: f32,
    chance: f32,
}

pub struct Chord {
    info: PluginInfo,
    params: ParamStore,
    voices: [Voice; VOICES],
    fit: bool,
    mask: u16,
    strum_ms: f64,
    strum_dir: usize,
    /// −1…1.
    tension: f64,
    /// −1…1.
    strum_vel: f32,
    seed: u32,
    loop_choice: usize,
    learn: bool,
    played: [Played; MAX_NOTES],
    played_len: usize,
    /// Strummed notes still to start, by `due` (first in, first out at a tie).
    pending: [Pending; MAX_PENDING],
    pending_len: usize,
    /// How many played keys hold each output key (channel × 128 + key).
    count: [u8; 2048],
    /// Note id of the note currently sounding on each output key.
    cur_id: [i32; 2048],
    ids: IdAlloc,
    hash: PositionHash,
    /// Samples processed: the time axis of `Pending::due`.
    clock: i64,
    /// Keys pressed in the gesture being learned (bit = key).
    gesture: u128,
    learned: [u8; VOICES],
    learned_n: usize,
    serial: u32,
    dropped: u64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Chord::new())
}

impl Default for Chord {
    fn default() -> Self {
        Self::new()
    }
}

const NO_PLAYED: Played = Played { channel: 0, key: 0, out: [0; VOICES], n: 0 };

impl Chord {
    pub fn new() -> Self {
        let ps = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Chord",
                vendor: "AI-Track",
                category: Category::MidiEffect,
                tags: &["midi", "chord", "harmony", "strum"],
                params: ps.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&ps),
            voices: [Voice { on: false, interval: 0, level: 1.0, chance: 1.0 }; VOICES],
            fit: false,
            mask: 0,
            strum_ms: 0.0,
            strum_dir: 0,
            tension: 0.0,
            strum_vel: 0.0,
            seed: 1,
            loop_choice: 0,
            learn: false,
            played: [NO_PLAYED; MAX_NOTES],
            played_len: 0,
            pending: [NO_PENDING; MAX_PENDING],
            pending_len: 0,
            count: [0; 2048],
            cur_id: [0; 2048],
            ids: IdAlloc::new(),
            hash: PositionHash::new(0x6368_6F72),
            clock: 0,
            gesture: 0,
            learned: [0; VOICES],
            learned_n: 0,
            serial: 0,
            dropped: 0,
        };
        c.refresh();
        c
    }

    fn refresh(&mut self) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            v.on = self.params.on(p::on(i));
            v.interval = self.params.plain(p::interval(i)).round() as i32;
            v.level = self.params.plain(p::level(i)) / 100.0;
            v.chance = self.params.plain(p::chance(i)) / 100.0;
        }
        self.fit = self.params.on(p::FIT);
        self.mask = scale_mask(self.params.choice(p::SCALE).min(12), self.params.choice(p::ROOT).min(11));
        self.strum_ms = f64::from(self.params.plain(p::STRUM));
        self.strum_dir = self.params.choice(p::STRUM_DIR).min(3);
        self.tension = f64::from(self.params.plain(p::STRUM_TENSION)) / 100.0;
        self.strum_vel = self.params.plain(p::STRUM_VEL) / 100.0;
        self.seed = self.params.plain(p::SEED).round().max(0.0) as u32;
        self.loop_choice = self.params.choice(p::LOOP).min(LOOP_NAMES.len() - 1);
        self.learn = self.params.on(p::LEARN);
    }

    #[inline]
    fn slot(channel: u8, key: u8) -> usize {
        (channel as usize & 15) * 128 + (key as usize & 127)
    }

    /// Forget the notes of this played key that have not started.
    fn cancel(&mut self, channel: u8, played: u8) {
        let mut kept = 0;
        for i in 0..self.pending_len {
            let n = self.pending[i];
            if !(n.channel == channel && n.played == played) {
                self.pending[kept] = n;
                kept += 1;
            }
        }
        self.pending_len = kept;
    }

    /// Release the output keys of `played[i]` and forget it (and whatever it
    /// still had to start).
    fn release(&mut self, i: usize, at: u32, out: &mut EventSink<'_>) {
        let pl = self.played[i];
        self.cancel(pl.channel, pl.key);
        for &k in &pl.out[..pl.n as usize] {
            let s = Self::slot(pl.channel, k);
            self.count[s] = self.count[s].saturating_sub(1);
            if self.count[s] == 0 && !out.push(at, EventKind::NoteOff { channel: pl.channel, key: k, velocity: 0.0, note_id: self.cur_id[s] }) {
                self.dropped += 1;
            }
        }
        self.played_len -= 1;
        self.played[i] = self.played[self.played_len];
    }

    /// Start output note `key`; a key another played key already holds is
    /// retriggered (note-off then note-on). `false` when the sink was full.
    fn start(&mut self, channel: u8, key: u8, velocity: f32, at: u32, out: &mut EventSink<'_>) -> bool {
        let s = Self::slot(channel, key);
        if self.count[s] > 0 {
            let id = self.cur_id[s];
            if !out.push(at, EventKind::NoteOff { channel, key, velocity: 0.0, note_id: id }) {
                self.dropped += 1;
            }
        }
        let id = self.ids.next();
        if out.push(at, EventKind::NoteOn { channel, key, velocity: velocity.clamp(MIN_VEL, 1.0), note_id: id }) {
            self.cur_id[s] = id;
            self.count[s] += 1;
            true
        } else {
            self.dropped += 1;
            false
        }
    }

    /// Queue a note for `due`, after the ones already due at or before it.
    fn schedule(&mut self, p: Pending) {
        if self.pending_len == MAX_PENDING {
            self.dropped += 1;
            return;
        }
        let at = self.pending[..self.pending_len].partition_point(|n| n.due <= p.due);
        self.pending.copy_within(at..self.pending_len, at + 1);
        self.pending[at] = p;
        self.pending_len += 1;
    }

    /// The first pending note starts (its time has come, at `at` of the block).
    fn start_pending(&mut self, at: u32, out: &mut EventSink<'_>) {
        let n = self.pending[0];
        self.pending.copy_within(1..self.pending_len, 0);
        self.pending_len -= 1;
        let Some(i) = self.played[..self.played_len].iter().position(|pl| pl.channel == n.channel && pl.key == n.played) else { return };
        if self.start(n.channel, n.key, n.velocity, at, out) {
            let pl = &mut self.played[i];
            pl.out[pl.n as usize] = n.key;
            pl.n += 1;
        }
    }

    fn note_on(&mut self, channel: u8, key: u8, velocity: f32, at: u32, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        if let Some(i) = self.played[..self.played_len].iter().position(|n| n.channel == channel && n.key == key) {
            self.release(i, at, out);
        }
        if self.played_len == MAX_NOTES {
            self.dropped += 1;
            return;
        }
        let mut pl = Played { channel, key, out: [0; VOICES], n: 0 };
        if self.learn {
            // Listening: the note itself, alone, and the key joins the gesture.
            self.gesture |= 1u128 << (key & 127);
            if self.start(channel, key, velocity, at, out) {
                pl.out[0] = key;
                pl.n = 1;
            }
        } else {
            self.start_chord(&mut pl, velocity, at, ctx, out);
        }
        self.played[self.played_len] = pl;
        self.played_len += 1;
    }

    /// The notes one press makes: chance, fit, order, delays and velocities.
    fn start_chord(&mut self, pl: &mut Played, velocity: f32, at: u32, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        let (channel, key) = (pl.channel, pl.key);
        let sr = ctx.sample_rate;
        let loop_len = loop_beats(self.loop_choice, ctx.transport.time_sig);
        let mut draw = Draw::new(self.hash.next(self.seed, ctx.transport, at, sr, loop_len));
        // The notes that sound: (key, velocity before the strum's tilt).
        let mut notes = [(0u8, 0.0f32); VOICES];
        let mut n = 0;
        for v in self.voices {
            // Six rolls per press, whatever is switched on.
            let roll = draw.unit();
            if !v.on || v.level <= 0.0 || roll >= v.chance {
                continue;
            }
            let mut k = key as i32 + v.interval;
            if !(0..=127).contains(&k) {
                continue;
            }
            if self.fit && v.interval != 0 {
                k = snap_note(self.mask, k as u8, Snap::Nearest, false) as i32;
            }
            let k = k as u8;
            if notes[..n].iter().any(|x| x.0 == k) {
                continue;
            }
            notes[n] = (k, velocity * v.level);
            n += 1;
        }
        // The order of the strum, by pitch.
        let notes = &mut notes[..n];
        notes.sort_by_key(|x| x.0);
        match self.strum_dir {
            1 => notes.reverse(),
            2 => {
                let beats = ctx.transport.position_beats + at as f64 * ctx.transport.bpm / 60.0 / sr;
                if ((beats * 2.0).round() as i64).rem_euclid(2) == 0 {
                    notes.reverse();
                }
            }
            3 => {
                for i in (1..n).rev() {
                    notes.swap(i, (draw.next_u64() % (i as u64 + 1)) as usize);
                }
            }
            _ => {}
        }
        let spread = self.strum_ms * 0.001 * sr;
        let abs = self.clock + at as i64;
        for (i, &(k, vel)) in notes.iter().enumerate() {
            let delay = if spread > 0.0 { (spread * strum_position(i, n, self.tension)).round() as i64 } else { 0 };
            let vel = vel * strum_gain(i, n, self.strum_vel);
            if delay <= 0 {
                if self.start(channel, k, vel, at, out) {
                    pl.out[pl.n as usize] = k;
                    pl.n += 1;
                }
            } else {
                self.schedule(Pending { due: abs + delay, channel, played: key, key: k, velocity: vel });
            }
        }
    }

    /// Every key is up: a gesture heard while learning becomes the chord.
    fn finish_gesture(&mut self) {
        if self.learn && self.gesture != 0 {
            let mut keys = self.gesture;
            let mut n = 0;
            while keys != 0 && n < VOICES {
                self.learned[n] = keys.trailing_zeros() as u8;
                keys &= keys - 1;
                n += 1;
            }
            self.learned_n = n;
            self.serial = self.serial.wrapping_add(1);
        }
        self.gesture = 0;
    }
}

impl Processor for Chord {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
        self.params.mark_dirty();
    }

    fn reset(&mut self) {
        self.played_len = 0;
        self.pending_len = 0;
        self.count.fill(0);
        self.gesture = 0;
        self.hash.reset();
    }

    /// A MIDI effect: the engine never asks for audio.
    fn process(&mut self, _ctx: &mut ProcessContext<'_>) {}

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn save_state(&self) -> Vec<u8> {
        crate::common::save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        crate::common::load_state(&mut self.params, data);
    }

    fn dropped_events(&self) -> u64 {
        self.dropped
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let mut m = [0.0f32; meter::LEN];
        m[meter::SERIAL] = (self.serial & 0x00FF_FFFF) as f32;
        m[meter::COUNT] = self.learned_n as f32;
        for (slot, key) in m[meter::KEYS..meter::LIVE].iter_mut().zip(&self.learned[..self.learned_n]) {
            *slot = f32::from(*key);
        }
        m[meter::LIVE] = self.gesture.count_ones() as f32;
        write_meter(out, 0, &m)
    }

    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        if self.params.take_dirty() {
            self.refresh();
        }
        let last = (ctx.frames.max(1) - 1) as u32;
        let end = self.clock + ctx.frames as i64;
        let mut i = 0;
        loop {
            // The next input event and the next strummed note that is due,
            // in time order; the input comes first at a tie (a key released
            // at the very moment a note is due stops it).
            let input = ctx.events.get(i).map(|e| e.offset.min(last));
            let due = (self.pending_len > 0 && self.pending[0].due < end).then(|| (self.pending[0].due - self.clock).max(0) as u32);
            match (input, due) {
                (None, None) => break,
                (Some(a), Some(b)) if b < a => self.start_pending(b, out),
                (None, Some(b)) => self.start_pending(b, out),
                (Some(at), _) => {
                    match ctx.events[i].kind {
                        EventKind::NoteOn { channel, key, velocity, .. } => self.note_on(channel, key, velocity, at, ctx, out),
                        EventKind::NoteOff { channel, key, .. } => {
                            if let Some(n) = self.played[..self.played_len].iter().position(|n| n.channel == channel && n.key == key) {
                                self.release(n, at, out);
                                if self.played_len == 0 {
                                    self.finish_gesture();
                                }
                            }
                        }
                        EventKind::AllNotesOff => {
                            while self.played_len > 0 {
                                self.release(self.played_len - 1, at, out);
                            }
                            self.pending_len = 0;
                            self.gesture = 0;
                            out.push(at, EventKind::AllNotesOff);
                        }
                        EventKind::NoteExpression { .. } => {}
                        other => {
                            out.push(at, other);
                        }
                    }
                    i += 1;
                }
            }
        }
        self.clock = end;
    }
}

#[cfg(test)]
mod tests;
