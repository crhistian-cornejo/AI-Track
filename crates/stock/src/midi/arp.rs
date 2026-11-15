//! `midi.arp` — arpeggiator.
//!
//! Turns the keys you hold into a rhythmic pattern. The grid is musical
//! time: steps sit on `position_beats / step_beats` of the project, so the
//! pattern is the same live and in an export, for any block size, and starts
//! aligned to the project grid wherever the transport begins. With the
//! transport stopped it runs on its own beat clock (which carries on from
//! where the transport left off), so keys played on a stopped project still
//! arpeggiate.
//!
//! Model
//! * `held`: up to 16 keys (pitch-sorted, or in pressing order for Play
//!   Order). The pool is those keys stacked over `Octaves` passes, each
//!   `Distance` semitones above the last.
//! * Step `k` (absolute grid index) of the pattern plays pool entry
//!   `pattern(style, ((k - origin) / note_repeat) mod len)`. The origin is
//!   0 (Retrigger Off: locked to the grid), the step after the latest note
//!   (Note), or the first step of the beat / bar (Beat, Bar). Random styles
//!   hash the absolute step, so they too do not depend on block size.
//! * Gate sets each note-off at a share of the step; notes live in a small
//!   fixed table with their own off times, which survive the end of the
//!   block. A key never sounds twice at once: a repeat closes the old note
//!   first, at the same sample.
//! * A jump of the transport (loop wrap, locate) closes the sounding notes
//!   and realigns to the grid; `reset` forgets everything (the engine
//!   closes what was open).
//!
//! The played notes are consumed (the instrument only hears the arp); every
//! other event passes. Per-note expressions are dropped (their note ids
//! belong to the keys you played, not to the notes generated).
//!
//! Meter layout: `[position in cycle, cycle length, held keys, sounding notes]`.

use crate::common::{ParamStore, write_meter};
use aitrack_dsp::{Category, EventKind, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.arp";

pub mod p {
    pub const STYLE: u32 = 0;
    pub const RATE: u32 = 1;
    pub const GATE: u32 = 2;
    pub const OCTAVES: u32 = 3;
    pub const DISTANCE: u32 = 4;
    pub const SWING: u32 = 5;
    pub const OFFSET: u32 = 6;
    pub const HOLD: u32 = 7;
    pub const RETRIGGER: u32 = 8;
    pub const REPEATS: u32 = 9;
    pub const NOTE_REPEAT: u32 = 10;
    pub const VEL_MODE: u32 = 11;
    pub const VEL_FIXED: u32 = 12;
    pub const VEL_LEN: u32 = 13;
    /// Eight consecutive ids: velocity of step 1..8.
    pub const VEL_STEP0: u32 = 14;
    pub const SEED: u32 = 22;
}

pub const STYLES: &[&str] = &[
    "Up",
    "Down",
    "Up-Down",
    "Down-Up",
    "Up & Down",
    "Down & Up",
    "Converge",
    "Diverge",
    "Low Pedal",
    "High Pedal",
    "Play Order",
    "Chord",
    "Random",
    "Random Other",
];
const PLAY_ORDER: usize = 10;
const CHORD: usize = 11;
const RANDOM: usize = 12;
const RANDOM_OTHER: usize = 13;

pub const RATES: &[&str] = &["1/2", "1/4", "1/4.", "1/4T", "1/8", "1/8.", "1/8T", "1/16", "1/16.", "1/16T", "1/32", "1/32.", "1/32T"];
/// Length of each of [`RATES`] in quarter notes.
const RATE_BEATS: [f64; 13] = [2.0, 1.0, 1.5, 2.0 / 3.0, 0.5, 0.75, 1.0 / 3.0, 0.25, 0.375, 1.0 / 6.0, 0.125, 0.1875, 1.0 / 12.0];

const RETRIGGERS: &[&str] = &["Off", "Note", "Beat", "Bar"];
const REPEATS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "Inf"];
const NUMBERS_1_4: &[&str] = &["1", "2", "3", "4"];
const NUMBERS_1_8: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8"];
const VEL_MODES: &[&str] = &["Input", "Fixed", "Steps"];
const OFF_ON: &[&str] = &["Off", "On"];
const STEP_NAMES: [&str; 8] = ["Step 1 Velocity", "Step 2 Velocity", "Step 3 Velocity", "Step 4 Velocity", "Step 5 Velocity", "Step 6 Velocity", "Step 7 Velocity", "Step 8 Velocity"];
const STEP_SHORT: [&str; 8] = ["Vel 1", "Vel 2", "Vel 3", "Vel 4", "Vel 5", "Vel 6", "Vel 7", "Vel 8"];

const MAX_HELD: usize = 16;
const MAX_ACTIVE: usize = 128;
/// Generated note ids live in their own range so they never clash with the
/// ids of the keys played.
const ID_BASE: i32 = 0x4100_0000;
const ID_MASK: u32 = 0x00FF_FFFF;
/// A transport position this far (quarter notes) from where the last block
/// ended counts as a jump.
const JUMP_TOL: f64 = 2e-3;

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::STYLE, "Style", 0.0, 13.0, 0.0).choices(STYLES),
        ParamInfo::new(p::RATE, "Rate", 0.0, 12.0, 7.0).choices(RATES),
        ParamInfo::new(p::GATE, "Gate", 1.0, 200.0, 80.0).unit("%"),
        ParamInfo::new(p::OCTAVES, "Octaves", 1.0, 4.0, 1.0).choices(NUMBERS_1_4).short("Oct"),
        ParamInfo::new(p::DISTANCE, "Distance", -12.0, 12.0, 12.0).unit("st").short("Dist"),
        ParamInfo::new(p::SWING, "Swing", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::OFFSET, "Offset", -50.0, 50.0, 0.0).unit("%"),
        ParamInfo::new(p::HOLD, "Hold", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::RETRIGGER, "Retrigger", 0.0, 3.0, 1.0).choices(RETRIGGERS).short("Retrig"),
        ParamInfo::new(p::REPEATS, "Repeats", 0.0, 8.0, 8.0).choices(REPEATS),
        ParamInfo::new(p::NOTE_REPEAT, "Note Repeat", 1.0, 4.0, 1.0).choices(NUMBERS_1_4).short("Note Rep"),
        ParamInfo::new(p::VEL_MODE, "Velocity", 0.0, 2.0, 0.0).choices(VEL_MODES).short("Vel"),
        ParamInfo::new(p::VEL_FIXED, "Fixed Velocity", 1.0, 127.0, 100.0).short("Vel Fixed"),
        ParamInfo::new(p::VEL_LEN, "Velocity Steps", 1.0, 8.0, 8.0).choices(NUMBERS_1_8).short("Vel Steps"),
    ];
    for i in 0..8 {
        v.push(ParamInfo::new(p::VEL_STEP0 + i as u32, STEP_NAMES[i], 1.0, 127.0, 100.0).short(STEP_SHORT[i]));
    }
    v.push(ParamInfo::new(p::SEED, "Seed", 0.0, 99.0, 0.0));
    v
}

#[derive(Clone, Copy)]
struct Held {
    key: u8,
    channel: u8,
    velocity: f32,
    down: bool,
}

const NO_HELD: Held = Held { key: 0, channel: 0, velocity: 0.0, down: false };

#[derive(Clone, Copy)]
struct Active {
    key: u8,
    channel: u8,
    note_id: i32,
    /// Device-clock sample the note-off is due at.
    off_at: i64,
}

const NO_ACTIVE: Active = Active { key: 0, channel: 0, note_id: 0, off_at: 0 };

/// The parameters in the form the audio side uses.
#[derive(Clone, Copy)]
struct Cfg {
    style: usize,
    step_beats: f64,
    gate: f64,
    octaves: usize,
    distance: i32,
    swing: f64,
    offset: f64,
    hold: bool,
    retrigger: usize,
    repeats: Option<i64>,
    note_repeat: i64,
    vel_mode: usize,
    vel_fixed: f32,
    vel_len: usize,
    vel_steps: [f32; 8],
    seed: u64,
}

pub struct Arp {
    info: PluginInfo,
    params: ParamStore,
    cfg: Cfg,
    sr: f64,
    held: [Held; MAX_HELD],
    held_len: usize,
    /// `held` ordered for the pattern: by pitch, or as pressed.
    base: [Held; MAX_HELD],
    base_len: usize,
    base_dirty: bool,
    active: [Active; MAX_ACTIVE],
    active_len: usize,
    /// Samples processed since `reset`: the time axis of the note-offs.
    clock: i64,
    /// Beat clock that carries the grid while the transport is stopped.
    free_beats: f64,
    /// Where the last block ended in beats; `None` until the first block.
    expected: Option<f64>,
    grid_sig: u64,
    next_k: i64,
    note_origin: i64,
    id_counter: u32,
    dropped: u64,
    // Per block.
    frames: i64,
    spb: f64,
    bar_beats: f64,
    // For the meter.
    m_pos: u32,
    m_len: u32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Arp::new())
}

impl Default for Arp {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Steps in one cycle of `style` over `n` pool entries (`octaves` passes for the chord style).
fn pattern_len(style: usize, n: usize, octaves: usize) -> usize {
    match style {
        2 | 3 => (2 * n).saturating_sub(2).max(1),
        4 | 5 => 2 * n,
        8 | 9 => (2 * n).saturating_sub(2).max(1),
        CHORD => octaves,
        _ => n,
    }
}

/// Converge order: lowest, highest, second lowest, second highest…
#[inline]
fn converge(pos: usize, n: usize) -> usize {
    if pos.is_multiple_of(2) { pos / 2 } else { n - 1 - pos / 2 }
}

/// Pool index of position `pos` of the cycle, for the ordered styles.
fn pattern_index(style: usize, pos: usize, n: usize) -> usize {
    let up_down = |pos: usize| if pos < n { pos } else { 2 * n - 2 - pos };
    let up_and_down = |pos: usize| if pos < n { pos } else { 2 * n - 1 - pos };
    match style {
        1 => n - 1 - pos,
        2 => up_down(pos),
        3 => n - 1 - up_down(pos),
        4 => up_and_down(pos),
        5 => n - 1 - up_and_down(pos),
        6 => converge(pos, n),
        7 => converge(n - 1 - pos, n),
        // The lowest note between every other one.
        8 => {
            if pos.is_multiple_of(2) {
                0
            } else {
                1 + pos / 2
            }
        }
        // The highest note between every other one.
        9 => {
            if pos.is_multiple_of(2) {
                pos / 2
            } else {
                n - 1
            }
        }
        _ => pos,
    }
}

/// Random pool index of absolute step `s`.
#[inline]
fn draw(s: i64, seed: u64, n: usize) -> usize {
    (mix64(s as u64 ^ (seed << 40) ^ 0x5151) % n as u64) as usize
}

/// Random pool index that never equals the one of the step before, and
/// depends only on the step number (so not on where playback began): even
/// steps are plain draws, odd steps are drawn among the entries their two
/// even neighbours did not take. With two entries it simply alternates.
fn random_other(s: i64, seed: u64, n: usize) -> usize {
    match n {
        0 | 1 => 0,
        2 => (s.rem_euclid(2) as u64 ^ (seed & 1)) as usize,
        _ => {
            if s.rem_euclid(2) == 0 {
                return draw(s, seed, n);
            }
            let (a, b) = (draw(s - 1, seed, n), draw(s + 1, seed, n));
            let left = n - if a == b { 1 } else { 2 };
            let mut r = (mix64(s as u64 ^ (seed << 40) ^ 0xA7A7) % left as u64) as usize;
            // The r-th entry that is neither a nor b.
            for i in 0..n {
                if i != a && i != b {
                    if r == 0 {
                        return i;
                    }
                    r -= 1;
                }
            }
            0
        }
    }
}

impl Arp {
    pub fn new() -> Self {
        let ps = params();
        let mut a = Self {
            info: PluginInfo {
                id: ID,
                name: "Arpeggiator",
                vendor: "AI-Track",
                category: Category::MidiEffect,
                tags: &["midi", "arpeggiator", "pattern", "sequencer"],
                params: ps.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&ps),
            cfg: Cfg {
                style: 0,
                step_beats: 0.25,
                gate: 0.8,
                octaves: 1,
                distance: 12,
                swing: 0.0,
                offset: 0.0,
                hold: false,
                retrigger: 1,
                repeats: None,
                note_repeat: 1,
                vel_mode: 0,
                vel_fixed: 100.0 / 127.0,
                vel_len: 8,
                vel_steps: [100.0 / 127.0; 8],
                seed: 0,
            },
            sr: 48_000.0,
            held: [NO_HELD; MAX_HELD],
            held_len: 0,
            base: [NO_HELD; MAX_HELD],
            base_len: 0,
            base_dirty: false,
            active: [NO_ACTIVE; MAX_ACTIVE],
            active_len: 0,
            clock: 0,
            free_beats: 0.0,
            expected: None,
            grid_sig: 0,
            next_k: 0,
            note_origin: 0,
            id_counter: 0,
            dropped: 0,
            frames: 0,
            spb: 24_000.0,
            bar_beats: 4.0,
            m_pos: 0,
            m_len: 0,
        };
        a.refresh();
        a
    }

    /// Read the parameters into `cfg`.
    fn refresh(&mut self) {
        let ps = &self.params;
        let c = &mut self.cfg;
        c.style = ps.choice(p::STYLE).min(STYLES.len() - 1);
        c.step_beats = RATE_BEATS[ps.choice(p::RATE).min(RATE_BEATS.len() - 1)];
        c.gate = ps.plain(p::GATE) as f64 / 100.0;
        c.octaves = (ps.plain(p::OCTAVES).round() as usize).clamp(1, 4);
        c.distance = ps.plain(p::DISTANCE).round() as i32;
        c.swing = (ps.plain(p::SWING) / 100.0) as f64;
        c.offset = (ps.plain(p::OFFSET) / 100.0) as f64;
        c.hold = ps.on(p::HOLD);
        c.retrigger = ps.choice(p::RETRIGGER).min(3);
        c.repeats = match ps.choice(p::REPEATS) {
            n if n >= 8 => None,
            n => Some(n as i64 + 1),
        };
        c.note_repeat = (ps.plain(p::NOTE_REPEAT).round() as i64).clamp(1, 4);
        c.vel_mode = ps.choice(p::VEL_MODE).min(2);
        c.vel_fixed = ps.plain(p::VEL_FIXED) / 127.0;
        c.vel_len = (ps.plain(p::VEL_LEN).round() as usize).clamp(1, 8);
        for i in 0..8 {
            c.vel_steps[i] = ps.plain(p::VEL_STEP0 + i as u32) / 127.0;
        }
        c.seed = ps.plain(p::SEED) as u64;
        if !c.hold {
            // Hold switched off: the keys that are no longer down are let go.
            let mut n = 0;
            for i in 0..self.held_len {
                if self.held[i].down {
                    self.held[n] = self.held[i];
                    n += 1;
                }
            }
            self.held_len = n;
        }
        self.base_dirty = true;
    }

    fn rebuild_base(&mut self) {
        let n = self.held_len;
        self.base[..n].copy_from_slice(&self.held[..n]);
        if self.cfg.style != PLAY_ORDER {
            // Stable insertion sort by pitch.
            for i in 1..n {
                let x = self.base[i];
                let mut j = i;
                while j > 0 && self.base[j - 1].key > x.key {
                    self.base[j] = self.base[j - 1];
                    j -= 1;
                }
                self.base[j] = x;
            }
        }
        self.base_len = n;
        self.base_dirty = false;
    }

    // ───────────────────────────── The grid ─────────────────────────────

    /// Start of step `k` in quarter notes.
    #[inline]
    fn step_start(&self, k: i64) -> f64 {
        let swing = if k & 1 == 1 { self.cfg.swing * 0.5 } else { 0.0 };
        (k as f64 + self.cfg.offset + swing) * self.cfg.step_beats
    }

    /// Block offset of step `k` (never negative).
    #[inline]
    fn step_offset(&self, k: i64, base: f64) -> i64 {
        (((self.step_start(k) - base) * self.spb).round() as i64).max(0)
    }

    /// The first step that has not started yet at `base`.
    fn first_step(&self, base: f64) -> i64 {
        let sb = self.cfg.step_beats;
        let mut k = (base / sb - self.cfg.offset).floor() as i64 - 2;
        let tol = 0.5 / self.spb;
        while self.step_start(k) < base - tol {
            k += 1;
        }
        k
    }

    /// Step the pattern counts from.
    fn origin(&self, k: i64) -> i64 {
        match self.cfg.retrigger {
            0 => 0,
            1 => self.note_origin,
            r => {
                let interval = if r == 2 { 1.0 } else { self.bar_beats };
                let sb = self.cfg.step_beats;
                let start = (k as f64 * sb / interval + 1e-9).floor() * interval;
                (start / sb - 1e-9).ceil() as i64
            }
        }
    }

    // ───────────────────────────── Sounding notes ─────────────────────────────

    fn next_off(&self, frames: i64) -> Option<(usize, i64)> {
        let mut best: Option<(usize, i64)> = None;
        for (i, a) in self.active[..self.active_len].iter().enumerate() {
            let rel = a.off_at - self.clock;
            if rel < frames && best.is_none_or(|(_, b)| rel < b) {
                best = Some((i, rel));
            }
        }
        best.map(|(i, r)| (i, r.max(0)))
    }

    /// Write the note-off of `active[i]` at block offset `at` and forget it.
    /// When the sink is full it stays, due again at the next block.
    fn emit_off(&mut self, i: usize, at: i64, out: &mut EventSink<'_>) {
        let a = self.active[i];
        let kind = EventKind::NoteOff { channel: a.channel, key: a.key, velocity: 0.0, note_id: a.note_id };
        if out.push(at as u32, kind) {
            self.active_len -= 1;
            self.active[i] = self.active[self.active_len];
        } else {
            self.dropped += 1;
            self.active[i].off_at = self.clock + self.frames;
        }
    }

    fn close_all(&mut self, at: i64, out: &mut EventSink<'_>) {
        for i in (0..self.active_len).rev() {
            self.emit_off(i, at, out);
        }
    }

    fn trigger(&mut self, key: u8, channel: u8, velocity: f32, at: i64, gate: i64, out: &mut EventSink<'_>) {
        if let Some(i) = self.active[..self.active_len].iter().position(|a| a.key == key && a.channel == channel) {
            self.emit_off(i, at, out);
        }
        if self.active_len == MAX_ACTIVE {
            // Out of room: the note that would end first ends now.
            let i = (0..self.active_len).min_by_key(|&i| self.active[i].off_at).unwrap_or(0);
            self.emit_off(i, at, out);
            if self.active_len == MAX_ACTIVE {
                return;
            }
        }
        self.id_counter = self.id_counter.wrapping_add(1);
        let note_id = ID_BASE + (self.id_counter & ID_MASK) as i32;
        let velocity = velocity.clamp(1.0 / 127.0, 1.0);
        if out.push(at as u32, EventKind::NoteOn { channel, key, velocity, note_id }) {
            self.active[self.active_len] = Active { key, channel, note_id, off_at: self.clock + at + gate };
            self.active_len += 1;
        } else {
            self.dropped += 1;
        }
    }

    // ───────────────────────────── Input ─────────────────────────────

    fn on_event(&mut self, kind: EventKind, at: i64, out: &mut EventSink<'_>) {
        match kind {
            EventKind::NoteOn { channel, key, velocity, .. } => {
                if self.cfg.hold && !self.held[..self.held_len].iter().any(|h| h.down) {
                    // The first key after letting go of a held chord replaces it.
                    self.held_len = 0;
                }
                if let Some(h) = self.held[..self.held_len].iter_mut().find(|h| h.key == key && h.channel == channel) {
                    h.velocity = velocity;
                    h.down = true;
                } else if self.held_len < MAX_HELD {
                    self.held[self.held_len] = Held { key, channel, velocity, down: true };
                    self.held_len += 1;
                } else {
                    self.dropped += 1;
                }
                self.base_dirty = true;
                self.note_origin = self.next_k;
            }
            EventKind::NoteOff { channel, key, .. } => {
                if let Some(i) = self.held[..self.held_len].iter().position(|h| h.key == key && h.channel == channel) {
                    if self.cfg.hold {
                        self.held[i].down = false;
                    } else {
                        self.held.copy_within(i + 1..self.held_len, i);
                        self.held_len -= 1;
                    }
                    self.base_dirty = true;
                }
            }
            EventKind::AllNotesOff => {
                self.held_len = 0;
                self.base_dirty = true;
                self.close_all(at, out);
                out.push(at as u32, EventKind::AllNotesOff);
            }
            EventKind::NoteExpression { .. } => {}
            other => {
                out.push(at as u32, other);
            }
        }
    }

    // ───────────────────────────── One step ─────────────────────────────

    fn play_step(&mut self, k: i64, at: i64, out: &mut EventSink<'_>) {
        if self.base_dirty {
            self.rebuild_base();
        }
        let nb = self.base_len;
        if nb == 0 {
            self.m_len = 0;
            return;
        }
        let cfg = self.cfg;
        let n = (k - self.origin(k)).max(0);
        let pool = nb * cfg.octaves;
        let len = pattern_len(cfg.style, pool, cfg.octaves);
        let cycle = len as i64 * cfg.note_repeat;
        if let Some(r) = cfg.repeats
            && (k - self.note_origin).max(0) / cycle >= r
        {
            self.m_len = len as u32;
            return;
        }
        let rep_step = n / cfg.note_repeat;
        let pos = (rep_step % len as i64) as usize;
        self.m_pos = pos as u32;
        self.m_len = len as u32;
        let gate = ((cfg.gate * cfg.step_beats * self.spb).round() as i64).max(1);
        let vel_i = (n as usize) % cfg.vel_len;
        let velocity = |h: &Held| match cfg.vel_mode {
            0 => h.velocity,
            1 => cfg.vel_fixed,
            _ => cfg.vel_steps[vel_i],
        };
        if cfg.style == CHORD {
            let shift = pos as i32 * cfg.distance;
            for i in 0..nb {
                let h = self.base[i];
                let key = h.key as i32 + shift;
                if (0..=127).contains(&key) {
                    self.trigger(key as u8, h.channel, velocity(&h), at, gate, out);
                }
            }
            return;
        }
        let idx = match cfg.style {
            RANDOM => draw(k.div_euclid(cfg.note_repeat), cfg.seed, pool),
            RANDOM_OTHER => random_other(k.div_euclid(cfg.note_repeat), cfg.seed, pool),
            style => pattern_index(style, pos, pool),
        };
        let h = self.base[idx % nb];
        let key = h.key as i32 + (idx / nb) as i32 * cfg.distance;
        if (0..=127).contains(&key) {
            self.trigger(key as u8, h.channel, velocity(&h), at, gate, out);
        }
    }
}

impl Processor for Arp {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = if sample_rate > 0.0 { sample_rate } else { 48_000.0 };
        self.reset();
        self.params.mark_dirty();
    }

    fn reset(&mut self) {
        self.held_len = 0;
        self.base_len = 0;
        self.base_dirty = false;
        self.active_len = 0;
        self.clock = 0;
        self.free_beats = 0.0;
        self.expected = None;
        self.next_k = 0;
        self.note_origin = 0;
        self.m_pos = 0;
        self.m_len = 0;
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

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.m_pos as f32, self.m_len as f32, self.held_len as f32, self.active_len as f32])
    }

    fn dropped_events(&self) -> u64 {
        self.dropped
    }

    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        let frames = ctx.frames as i64;
        if frames == 0 {
            return;
        }
        self.frames = frames;
        if self.params.take_dirty() {
            self.refresh();
        }
        let tr = ctx.transport;
        let bpm = if tr.bpm.is_finite() && tr.bpm > 1.0 { tr.bpm } else { 120.0 };
        let sr = if ctx.sample_rate > 0.0 { ctx.sample_rate } else { self.sr };
        self.spb = sr * 60.0 / bpm;
        let (num, den) = (tr.time_sig.0.max(1) as f64, tr.time_sig.1.max(1) as f64);
        self.bar_beats = num * 4.0 / den;
        let base = if tr.playing && tr.position_beats.is_finite() { tr.position_beats } else { self.free_beats };

        let sig = self.cfg.step_beats.to_bits();
        let jumped = self.expected.is_none_or(|e| (base - e).abs() > JUMP_TOL);
        if jumped || sig != self.grid_sig {
            if jumped && self.expected.is_some() {
                // Loop wrap or locate: what sounds is closed, the pattern starts over.
                self.close_all(0, out);
            }
            self.grid_sig = sig;
            self.next_k = self.first_step(base);
            self.note_origin = self.next_k;
        }

        let mut ei = 0usize;
        let mut last_t = 0i64;
        let mut step_off = self.step_offset(self.next_k, base);
        loop {
            let off = self.next_off(frames);
            let ev = ctx.events.get(ei).map(|e| (e.offset as i64).min(frames - 1));
            let st = (step_off < frames).then_some(step_off);
            // Earliest first; at the same sample note-offs, then input, then the step.
            let mut best: Option<(i64, u8)> = off.map(|(_, t)| (t, 0));
            if let Some(t) = ev
                && best.is_none_or(|b| (t, 1) < b)
            {
                best = Some((t, 1));
            }
            if let Some(t) = st
                && best.is_none_or(|b| (t, 2) < b)
            {
                best = Some((t, 2));
            }
            let Some((t, class)) = best else { break };
            let t = t.max(last_t);
            last_t = t;
            match class {
                0 => {
                    if let Some((i, _)) = off {
                        self.emit_off(i, t, out);
                    }
                }
                1 => {
                    self.on_event(ctx.events[ei].kind, t, out);
                    ei += 1;
                }
                _ => {
                    let k = self.next_k;
                    self.play_step(k, t, out);
                    self.next_k += 1;
                    step_off = self.step_offset(self.next_k, base);
                }
            }
        }
        self.clock += frames;
        let end = base + frames as f64 / self.spb;
        self.free_beats = end;
        self.expected = Some(end);
    }
}

#[cfg(test)]
mod tests;
