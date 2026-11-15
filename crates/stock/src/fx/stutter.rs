//! `fx.stutter` — a rhythmic repeater for stutter house, glitch and
//! tape-stop. Zero latency, never allocates in `process`.
//!
//! **Capture.** The input is always recorded into a stereo ring of at least
//! 16 s, allocated in `prepare`. An *event* freezes the stretch of audio
//! that ended at the instant of the trigger and plays it back in a loop;
//! the output returns to the dry signal when the event ends.
//!
//! **Two ways to trigger** (`Mode`):
//!
//! * *Classic*: every `Interval` (from the `Offset`), and with probability
//!   `Chance`, an event of length `Gate` repeats a chunk of `Grid` (made
//!   triplet by `Triplets`, randomly altered by `Variation`).
//! * *Grid*: a pattern of up to 16 steps. Each step has `On`, a probability,
//!   a chunk division, a pitch and a reverse switch; the clock walks the
//!   steps at `Step Rate`, so a step is an event as long as the step.
//!
//! **Determinism.** Every random decision (does this slot fire, which way
//! does this repeat read, what does `Variation` do) is a pure function of
//! `(seed, slot index)` or `(event id, repeat number)`, where a slot is
//! `floor(song position / clock unit)`. Nothing depends on the block size,
//! on when playback began or on a random state that advances, so a bounce
//! sounds like the live pass and undo/redo never reshuffles the pattern.
//! With the transport stopped a free-running clock at the current tempo
//! stands in for the song position.
//!
//! **Reading.** One repeat lasts exactly `grid` samples of output. It plays
//! the last `grid · r` samples before the trigger at speed
//! `r = 2^((pitch + decay · k) / 12)` (`k` = repeat number; `Half-Time`
//! halves it) with 4-point Hermite interpolation, forward, backward or
//! alternating. Between repeats a 1.5 ms crossfade joins the end of one pass
//! (continued past the chunk) to the start of the next; entering and leaving
//! an event is a 2 ms fade, and an event that follows another straight away
//! simply re-anchors instead of dipping to the dry signal. `Ramp` halves the
//! grid in equal slices of the event for build-ups.
//!
//! **Tape-stop** replaces the loop by a pointer that leaves the live edge of
//! the buffer and slows down exponentially to a halt over `Stop Time` at the
//! end of the event (the level follows the speed down); `Half-Time` also
//! makes it start at half speed. **Mix modes**: *Insert* replaces the dry
//! signal, *Mix* adds to it, *Duck* closes it progressively over the event
//! and *Gate* also keeps it muted for the rest of the interval after the
//! repeat ends.
//!
//! **Transport jumps** (loop, locate, start, stop): the running event fades
//! out in 2 ms; a new one starts only on a clock boundary, at once if the jump
//! lands on one, and never from a chunk that would reach back before the
//! jump (the capture is not cleared, so a loop wrap on a boundary repeats the
//! end of the loop). `Hold` repeats from the moment it is switched on until
//! it is switched off, whatever the clock says.
//!
//! With nothing firing the audio path is the identity, bit for bit.
//!
//! Meter layout (134 values): `[0]` read position inside the frozen chunk
//! (0..1), `[1]` state (0 idle, 1 repeating, 2 tape-stop, 3 gated silence),
//! `[2]` active step (-1 in Classic), `[3]` current speed `r`, `[4]` event
//! progress (0..1), `[5]` wet weight, then 128 peaks of the chunk (the frozen
//! one, or the one that would be frozen now).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::hermite;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.stutter";

pub mod p {
    pub const MODE: u32 = 0;
    pub const INTERVAL: u32 = 1;
    pub const OFFSET: u32 = 2;
    pub const CHANCE: u32 = 3;
    pub const GATE: u32 = 4;
    pub const GRID: u32 = 5;
    pub const TRIPLETS: u32 = 6;
    pub const VARIATION: u32 = 7;
    pub const PITCH: u32 = 8;
    pub const PITCH_DECAY: u32 = 9;
    pub const DECAY: u32 = 10;
    pub const REVERSE: u32 = 11;
    pub const FILTER_ON: u32 = 12;
    pub const FILTER_FREQ: u32 = 13;
    pub const FILTER_WIDTH: u32 = 14;
    pub const MIX_MODE: u32 = 15;
    pub const TAPE_STOP: u32 = 16;
    pub const STOP_TIME: u32 = 17;
    pub const HALF_TIME: u32 = 18;
    pub const RAMP: u32 = 19;
    pub const HOLD: u32 = 20;
    pub const SEED: u32 = 21;
    pub const MIX: u32 = 22;
    pub const OUTPUT: u32 = 23;
    pub const STEP_RATE: u32 = 24;
    pub const LENGTH: u32 = 25;
    /// Delay of the odd steps, as a share of a step.
    pub const SWING: u32 = 26;
    /// Change of pitch (semitones) reached at the end of the event.
    pub const PITCH_END: u32 = 27;
    /// Change of the filter centre (octaves) reached at the end of the event.
    pub const FILTER_END: u32 = 28;
    /// Change of level (dB) reached at the end of the event.
    pub const LEVEL_END: u32 = 29;
    /// Level of the repeat at the start of the event.
    pub const LEVEL: u32 = 30;

    /// First id of the step parameters; step `i` field `f` is `STEP_BASE + i · STEP_STRIDE + f`.
    pub const STEP_BASE: u32 = 100;
    pub const STEP_STRIDE: u32 = 8;
    pub const STEP_ON: u32 = 0;
    pub const STEP_PROB: u32 = 1;
    pub const STEP_DIV: u32 = 2;
    pub const STEP_PITCH: u32 = 3;
    pub const STEP_REV: u32 = 4;
    pub const STEP_LEVEL: u32 = 5;
    pub const STEP_STOP: u32 = 6;
    pub const STEP_FILTER: u32 = 7;

    /// Tie flags: step `i` holds the event of the step before it.
    pub const TIE_BASE: u32 = 300;
    pub const fn tie(i: u32) -> u32 {
        TIE_BASE + i
    }

    pub const fn step(i: u32, field: u32) -> u32 {
        STEP_BASE + i * STEP_STRIDE + field
    }
}

pub const STEPS: usize = 16;

const MODES: &[&str] = &["Classic", "Grid"];
/// `(length, in bars)`: bar lengths follow the time signature.
const INTERVALS: &[(f64, bool)] = &[(0.125, false), (0.25, false), (0.5, false), (1.0, false), (2.0, false), (1.0, true), (2.0, true), (4.0, true)];
const INTERVAL_NAMES: &[&str] = &["1/32", "1/16", "1/8", "1/4", "1/2", "1 bar", "2 bars", "4 bars"];
const OFFSET_NAMES: &[&str] = &["0", "1/16", "1/8", "3/16", "1/4", "5/16", "3/8", "7/16", "1/2", "9/16", "5/8", "11/16", "3/4", "13/16", "7/8", "15/16"];
const GATES: &[(f64, bool)] = &[(0.125, false), (0.25, false), (0.5, false), (0.75, false), (1.0, false), (1.5, false), (2.0, false), (3.0, false), (1.0, true), (1.5, true), (2.0, true), (3.0, true), (4.0, true)];
const GATE_NAMES: &[&str] = &["1/32", "1/16", "1/8", "3/16", "1/4", "3/8", "1/2", "3/4", "1 bar", "1.5 bars", "2 bars", "3 bars", "4 bars"];
const GRIDS: &[(f64, bool)] = &[(0.125, false), (0.25, false), (0.5, false), (1.0, false), (2.0, false), (1.0, true)];
const GRID_NAMES: &[&str] = &["1/32", "1/16", "1/8", "1/4", "1/2", "1 bar"];
const STOPS: &[(f64, bool)] = &[(0.25, false), (0.5, false), (1.0, false), (2.0, false), (1.0, true), (2.0, true), (4.0, true)];
const STOP_NAMES: &[&str] = &["1/16", "1/8", "1/4", "1/2", "1 bar", "2 bars", "4 bars"];
const STEP_RATES: &[f64] = &[1.0, 0.5, 0.25, 0.125];
const STEP_RATE_NAMES: &[&str] = &["1/4", "1/8", "1/16", "1/32"];
/// Chunk divisions of a step, in beats.
const DIVS: &[f64] = &[0.5, 0.25, 1.0 / 6.0, 0.125, 1.0 / 12.0];
const DIV_NAMES: &[&str] = &["1/8", "1/16", "1/16T", "1/32", "1/32T"];
const REVERSES: &[&str] = &["Forward", "Reverse", "Ping-Pong", "Random"];
const MIX_MODES: &[&str] = &["Insert", "Mix", "Duck", "Gate"];
const RAMPS: &[&str] = &["Off", "x2", "x4", "x8", "x16"];
const LENGTH_NAMES: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16"];
const OFF_ON: &[&str] = &["Off", "On"];
const STEP_REVS: &[&str] = &["Fwd", "Rev"];

/// A default pattern with the 3-3-2 push of a house stab.
const fn default_on(i: u32) -> bool {
    matches!(i, 0 | 3 | 6 | 8 | 11 | 14)
}

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, 0.0).choices(MODES),
        ParamInfo::new(p::INTERVAL, "Interval", 0.0, 7.0, 5.0).choices(INTERVAL_NAMES).short("Intvl"),
        ParamInfo::new(p::OFFSET, "Offset", 0.0, 15.0, 0.0).choices(OFFSET_NAMES).short("Offs"),
        ParamInfo::new(p::CHANCE, "Chance", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::GATE, "Gate", 0.0, 12.0, 4.0).choices(GATE_NAMES),
        ParamInfo::new(p::GRID, "Grid", 0.0, 5.0, 1.0).choices(GRID_NAMES),
        ParamInfo::new(p::TRIPLETS, "Triplets", 0.0, 1.0, 0.0).choices(OFF_ON).short("Trip"),
        ParamInfo::new(p::VARIATION, "Variation", 0.0, 100.0, 0.0).unit("%").short("Var"),
        ParamInfo::new(p::PITCH, "Pitch", -24.0, 24.0, 0.0).unit("st"),
        ParamInfo::new(p::PITCH_DECAY, "Pitch Decay", -12.0, 12.0, 0.0).unit("st").short("P Decay"),
        ParamInfo::new(p::DECAY, "Decay", -24.0, 0.0, 0.0).unit("dB"),
        ParamInfo::new(p::REVERSE, "Reverse", 0.0, 3.0, 0.0).choices(REVERSES).short("Rev"),
        ParamInfo::new(p::FILTER_ON, "Filter", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::FILTER_FREQ, "Filter Freq", 80.0, 12_000.0, 1_200.0).unit("Hz").log().short("Freq"),
        ParamInfo::new(p::FILTER_WIDTH, "Filter Width", 0.0, 100.0, 40.0).unit("%").short("Width"),
        ParamInfo::new(p::MIX_MODE, "Mix Mode", 0.0, 3.0, 0.0).choices(MIX_MODES).short("Mode"),
        ParamInfo::new(p::TAPE_STOP, "Tape-Stop", 0.0, 1.0, 0.0).choices(OFF_ON).short("Stop"),
        ParamInfo::new(p::STOP_TIME, "Stop Time", 0.0, 6.0, 2.0).choices(STOP_NAMES).short("Stop T"),
        ParamInfo::new(p::HALF_TIME, "Half-Time", 0.0, 1.0, 0.0).choices(OFF_ON).short("Half"),
        ParamInfo::new(p::RAMP, "Ramp", 0.0, 4.0, 0.0).choices(RAMPS),
        ParamInfo::new(p::HOLD, "Hold", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::SEED, "Seed", 0.0, 999.0, 0.0).choices(&[]),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::STEP_RATE, "Step Rate", 0.0, 3.0, 2.0).choices(STEP_RATE_NAMES).short("Rate"),
        ParamInfo::new(p::LENGTH, "Steps", 0.0, 15.0, 15.0).choices(LENGTH_NAMES).short("Len"),
        ParamInfo::new(p::SWING, "Swing", 0.0, 75.0, 0.0).unit("%"),
        ParamInfo::new(p::PITCH_END, "Pitch End", -24.0, 24.0, 0.0).unit("st").short("P End"),
        ParamInfo::new(p::FILTER_END, "Filter End", -4.0, 4.0, 0.0).unit("oct").short("F End"),
        ParamInfo::new(p::LEVEL_END, "Level End", -60.0, 60.0, 0.0).unit("dB").short("L End"),
        ParamInfo::new(p::LEVEL, "Level", -60.0, 12.0, 0.0).unit("dB").short("Level"),
    ];
    // The Length list is 1-based for the reader, 0-based as a value.
    for i in 0..STEPS as u32 {
        v.push(ParamInfo::new(p::step(i, p::STEP_ON), "Step On", 0.0, 1.0, f32::from(default_on(i))).choices(OFF_ON).short("On"));
        v.push(ParamInfo::new(p::step(i, p::STEP_PROB), "Step Probability", 0.0, 100.0, 100.0).unit("%").short("Prob"));
        v.push(ParamInfo::new(p::step(i, p::STEP_DIV), "Step Division", 0.0, 4.0, 3.0).choices(DIV_NAMES).short("Div"));
        v.push(ParamInfo::new(p::step(i, p::STEP_PITCH), "Step Pitch", -12.0, 12.0, 0.0).unit("st").choices(&[]).short("Pitch"));
        v.push(ParamInfo::new(p::step(i, p::STEP_REV), "Step Reverse", 0.0, 1.0, 0.0).choices(STEP_REVS).short("Rev"));
        v.push(ParamInfo::new(p::step(i, p::STEP_LEVEL), "Step Level", -60.0, 0.0, 0.0).unit("dB").short("Level"));
        v.push(ParamInfo::new(p::step(i, p::STEP_STOP), "Step Stop", 0.0, 1.0, 0.0).choices(OFF_ON).short("Stop"));
        v.push(ParamInfo::new(p::step(i, p::STEP_FILTER), "Step Filter", 0.0, 1.0, 0.0).choices(OFF_ON).short("Filter"));
    }
    for i in 0..STEPS as u32 {
        v.push(ParamInfo::new(p::tie(i), "Step Tie", 0.0, 1.0, 0.0).choices(OFF_ON).short("Tie"));
    }
    v
}

// ---------------------------------------------------------------------------
// Constants of the engine
// ---------------------------------------------------------------------------

const BUFFER_SECONDS: f64 = 16.0;
/// Fade in and out of an event.
const FADE_SECONDS: f64 = 0.002;
/// Crossfade at the joint of two repeats.
const SEAM_SECONDS: f64 = 0.0015;
/// Added to the clock phase so a boundary that lands a hair short still counts.
const CLOCK_EPS: f64 = 1e-6;
/// A change of song position bigger than this (in beats) is a jump.
const JUMP_TOL: f64 = 1.0 / 32.0;
const MIN_GRID: f64 = 64.0;
/// Shortest chunk a repeat can read, in samples.
const MIN_CHUNK: f64 = 8.0;
/// How sharply a tape-stop decelerates (exponent over the stop time).
const STOP_CURVE: f64 = 3.5;
const HUGE: i64 = i64::MAX / 4;
const PEAKS: usize = 128;

// ---------------------------------------------------------------------------
// Deterministic decisions
// ---------------------------------------------------------------------------

#[inline]
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const SALT_CHANCE: u64 = 1;
const SALT_VARIATION: u64 = 2;
const SALT_VARIATION_PICK: u64 = 3;
const SALT_STEP: u64 = 4;
const SALT_ID: u64 = 5;

/// A number in `[0, 1)` that depends only on the seed, the slot and the purpose.
#[inline]
fn unit(seed: u32, slot: i64, salt: u64) -> f64 {
    let h = mix64(mix64(u64::from(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt.wrapping_mul(0xD6E8_FEB8_6659_FD93)) ^ slot as u64);
    (h >> 32) as f64 / 4_294_967_296.0
}

/// A number in `[0, 1)` that depends only on an event id and a repeat number.
#[inline]
fn unit_of(id: u64, rep: u32) -> f64 {
    (mix64(id ^ u64::from(rep).wrapping_mul(0xA24B_AED4_963E_E407)) >> 32) as f64 / 4_294_967_296.0
}

// ---------------------------------------------------------------------------
// Capture ring
// ---------------------------------------------------------------------------

/// Stereo ring addressed by absolute sample index (`wr` = samples written
/// since the last reset). Nothing before the reset is ever read back: indices
/// below zero read silence and indices past the newest sample read the newest.
struct Capture {
    l: Vec<f32>,
    r: Vec<f32>,
    mask: i64,
    wr: i64,
    /// Reads that asked for audio the ring no longer holds (tests only).
    #[cfg(test)]
    stale: std::cell::Cell<u64>,
}

impl Capture {
    fn new(min_frames: usize) -> Self {
        let n = min_frames.max(256).next_power_of_two();
        Self {
            l: vec![0.0; n],
            r: vec![0.0; n],
            mask: n as i64 - 1,
            wr: 0,
            #[cfg(test)]
            stale: std::cell::Cell::new(0),
        }
    }

    fn len(&self) -> usize {
        self.l.len()
    }

    /// Furthest back (in samples) a read may reach.
    fn max_lag(&self) -> f64 {
        (self.l.len() - 64) as f64
    }

    #[inline]
    fn push(&mut self, l: f32, r: f32) {
        let i = (self.wr & self.mask) as usize;
        self.l[i] = l;
        self.r[i] = r;
        self.wr += 1;
    }

    #[inline]
    fn at(&self, j: i64) -> (f32, f32) {
        if j < 0 || self.wr == 0 {
            return (0.0, 0.0);
        }
        let lo = (self.wr - self.l.len() as i64 + 4).max(0);
        #[cfg(test)]
        if j < lo {
            self.stale.set(self.stale.get() + 1);
        }
        let i = (j.clamp(lo, self.wr - 1) & self.mask) as usize;
        (self.l[i], self.r[i])
    }

    /// Hermite interpolation at `base + rel`. The integer and fractional
    /// parts are split before the sum, so the result is the same whatever
    /// `base` is.
    #[inline]
    fn read_rel(&self, base: i64, rel: f64) -> (f32, f32) {
        let fl = rel.floor();
        let i = base + fl as i64;
        let t = (rel - fl) as f32;
        let x0 = self.at(i);
        if t == 0.0 {
            return x0;
        }
        let xm = self.at(i - 1);
        let x1 = self.at(i + 1);
        let x2 = self.at(i + 2);
        (hermite(t, xm.0, x0.0, x1.0, x2.0), hermite(t, xm.1, x0.1, x1.1, x2.1))
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Loop a frozen chunk.
    Loop,
    /// Leave the live edge and slow to a halt.
    Run,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RevMode {
    Fwd,
    Rev,
    /// Alternate; the flag starts backwards.
    PingPong(bool),
    Random,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MixMode {
    Insert,
    Mix,
    Duck,
    Gate,
}

/// Everything that defines one event, decided at its trigger.
#[derive(Clone, Copy)]
struct Spec {
    kind: Kind,
    id: u64,
    /// Samples per repeat (before `Ramp`).
    grid: f64,
    /// Samples of audible repeat.
    play: u64,
    /// `play` plus any silent tail that keeps the dry signal muted.
    total: u64,
    pitch: f32,
    pitch_step: f32,
    gain_step_db: f32,
    rev: RevMode,
    half: bool,
    ramp: u32,
    /// Samples over which a tape-stop slows to a halt (ending at `play`).
    stop: u64,
    /// Another event is due the instant this one ends.
    chain: bool,
    /// Linear level of the repeat at the start (global Level × step level).
    level: f32,
    /// Change of level in dB reached at the end of the event.
    level_end_db: f32,
    /// Change of pitch in semitones reached at the end of the event.
    pitch_end: f32,
    filter: bool,
    /// Change of the filter centre in octaves reached at the end of the event.
    filter_end: f32,
}

impl Spec {
    /// Samples of history the first repeat reads.
    fn needed(&self) -> f64 {
        match self.kind {
            Kind::Run => 0.0,
            Kind::Loop => self.grid * 2f64.powf(f64::from(self.pitch.max(self.pitch + self.pitch_end)) / 12.0) * if self.half { 0.5 } else { 1.0 },
        }
    }
}

/// Time constants, fixed by the sample rate.
#[derive(Clone, Copy)]
struct Timing {
    fade_len: u64,
    /// Per-sample slope of the fade envelopes.
    fade_step: f32,
    seam: u32,
    gain_coef: f32,
    max_lag: f64,
}

impl Timing {
    fn new(sr: f64, max_lag: f64) -> Self {
        let fade_len = (FADE_SECONDS * sr).round().max(8.0);
        Self {
            fade_len: fade_len as u64,
            fade_step: (1.0 / fade_len) as f32,
            seam: (SEAM_SECONDS * sr).round().max(4.0) as u32,
            gain_coef: (1.0 - (-1.0 / (0.002 * sr)).exp()) as f32,
            max_lag,
        }
    }
}

struct Tick {
    l: f32,
    r: f32,
    /// Weight of the event against the dry signal.
    w: f32,
    /// Audibility of the repeat inside the event (0 in a gated tail).
    x: f32,
    progress: f32,
    /// Sample of the event this is (counted from the last re-anchor).
    t: u64,
}

#[inline]
fn slew(v: f32, target: f32, step: f32) -> f32 {
    let d = target - v;
    if d.abs() <= step * 1.0001 { target } else { v + step.copysign(d) }
}

#[inline]
fn ease(w: f32) -> f32 {
    w * w * (3.0 - 2.0 * w)
}

/// The tail of a previous pass, fading out across a joint. Positions are
/// relative to `base` so what is read never depends on the absolute sample
/// count (a bounce started elsewhere gives the same bits).
#[derive(Clone, Copy)]
struct Ghost {
    base: i64,
    q: f64,
    step: f64,
    w: f32,
    dw: f32,
}

impl Ghost {
    const OFF: Ghost = Ghost { base: 0, q: 0.0, step: 0.0, w: 0.0, dw: 0.0 };
}

struct Voice {
    on: bool,
    spec: Spec,
    /// Samples since the event (or its last re-anchor) began.
    t: u64,
    /// Absolute index just past the frozen chunk.
    wr0: i64,
    /// Samples per repeat in the current ramp segment.
    grid: f64,
    seg: u64,
    seg_len: u64,
    /// Samples into the current repeat.
    u: f64,
    rep: u32,
    rate: f64,
    /// Samples of source one repeat consumes.
    src_len: f64,
    has_q: bool,
    /// Where `q` is measured from, the read position for the current sample
    /// and the per-sample advance.
    base: i64,
    q: f64,
    step: f64,
    ghosts: [Ghost; 2],
    gain: f32,
    gain_target: f32,
    w: f32,
    x: f32,
    release_at: u64,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            on: false,
            spec: Spec { kind: Kind::Loop, id: 0, grid: MIN_GRID, play: 0, total: 0, pitch: 0.0, pitch_step: 0.0, gain_step_db: 0.0, rev: RevMode::Fwd, half: false, ramp: 0, stop: 0, chain: false, level: 1.0, level_end_db: 0.0, pitch_end: 0.0, filter: false, filter_end: 0.0 },
            t: 0,
            wr0: 0,
            grid: MIN_GRID,
            seg: 0,
            seg_len: 0,
            u: 0.0,
            rep: 0,
            rate: 1.0,
            src_len: 0.0,
            has_q: false,
            base: 0,
            q: 0.0,
            step: 1.0,
            ghosts: [Ghost::OFF; 2],
            gain: 1.0,
            gain_target: 1.0,
            w: 0.0,
            x: 1.0,
            release_at: u64::MAX,
        }
    }

    /// Begin an event whose chunk ends at `wr0`. If one is already playing
    /// the new one takes over from where it is, joined by the seam crossfade
    /// and without leaving the dry signal in between.
    fn start(&mut self, spec: Spec, wr0: i64, tm: &Timing) {
        let cont = self.on;
        if !cont {
            self.w = 0.0;
            self.x = 1.0;
            self.ghosts = [Ghost::OFF; 2];
        }
        self.on = true;
        self.spec = spec;
        self.t = 0;
        self.wr0 = wr0;
        self.grid = spec.grid;
        self.seg = 0;
        self.seg_len = if spec.ramp > 0 { spec.play / (u64::from(spec.ramp) + 1) } else { 0 };
        self.rep = 0;
        self.release_at = if spec.chain { spec.total } else { spec.total.saturating_sub(tm.fade_len) };
        self.has_q = cont;
        match spec.kind {
            Kind::Loop => self.begin_rep(0.0, tm),
            Kind::Run => self.begin_run(tm),
        }
        self.has_q = true;
    }

    /// Start fading out now.
    fn release_now(&mut self) {
        self.release_at = self.release_at.min(self.t);
    }

    fn direction(&self) -> f64 {
        match self.spec.rev {
            RevMode::Fwd => 1.0,
            RevMode::Rev => -1.0,
            RevMode::PingPong(start_back) => {
                if self.rep.is_multiple_of(2) != start_back {
                    1.0
                } else {
                    -1.0
                }
            }
            RevMode::Random => {
                if unit_of(self.spec.id, self.rep) < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }

    /// Begin a repeat `carry` samples (< 1) after its nominal start.
    fn begin_rep(&mut self, carry: f64, tm: &Timing) {
        let spec = self.spec;
        let ramp = if spec.play == u64::MAX { 0.0 } else { (self.t as f64 / spec.play.max(1) as f64).min(1.0) as f32 };
        let semitones = f64::from((spec.pitch + spec.pitch_end * ramp + spec.pitch_step * self.rep as f32).clamp(-48.0, 48.0));
        let mut rate = 2f64.powf(semitones / 12.0);
        if spec.half {
            rate *= 0.5;
        }
        let grid = self.grid;
        let len = (grid * rate).clamp(MIN_CHUNK, tm.max_lag - 8.0);
        rate = len / grid;
        let dir = self.direction();
        // Forward passes read the last `len` samples before the trigger,
        // backward ones the same samples from the end.
        let q_new = if dir > 0.0 { -len + rate * carry } else { -1.0 - rate * carry };
        self.join(self.wr0, q_new, grid, tm);
        self.base = self.wr0;
        self.q = q_new;
        self.step = dir * rate;
        self.u = carry;
        self.rate = rate;
        self.src_len = len;
        self.gain_target = spec.level * db_to_gain(spec.gain_step_db * self.rep as f32);
        if !self.has_q {
            self.gain = self.gain_target;
        }
    }

    fn begin_run(&mut self, tm: &Timing) {
        // Three samples behind the live edge: the reach of the interpolator.
        let q_new = -3.0;
        self.join(self.wr0, q_new, f64::MAX, tm);
        self.base = self.wr0;
        self.q = q_new;
        self.step = 1.0;
        self.u = 0.0;
        self.rate = 1.0;
        self.src_len = 0.0;
        self.gain_target = self.spec.level;
        if !self.has_q {
            self.gain = self.gain_target;
        }
    }

    /// If the pass about to start (`q_new` from `base`) is not where the
    /// current one is heading, let the current one fade out underneath it.
    fn join(&mut self, base: i64, q_new: f64, room: f64, tm: &Timing) {
        if !self.has_q || ((base - self.base) as f64 + q_new - self.q).abs() <= 2.0 {
            return;
        }
        let idx = if self.ghosts[0].w <= 0.0 {
            0
        } else if self.ghosts[1].w <= 0.0 {
            1
        } else if self.ghosts[0].w < self.ghosts[1].w {
            0
        } else {
            1
        };
        // What is audible now moves to the ghost; whatever other ghost is
        // fading keeps fading.
        let held = self.ghosts[0].w + self.ghosts[1].w;
        let len = tm.seam.min((room * 0.25).min(f64::from(tm.seam)) as u32).max(2);
        let w = (1.0 - held + self.ghosts[idx].w).clamp(0.0, 1.0);
        self.ghosts[idx] = Ghost { base: self.base, q: self.q, step: self.step, w, dw: w / len as f32 };
    }

    /// Speed and level of a tape-stop pointer at the current event time.
    fn run_state(&self) -> (f64, f32) {
        let s = &self.spec;
        let base = if s.half { 0.5 } else { 1.0 };
        let from = s.play.saturating_sub(s.stop);
        let curve = if s.stop > 0 && self.t >= from {
            let u = ((self.t - from) as f64 / s.stop as f64).min(1.0);
            let end = (-STOP_CURVE).exp();
            (((-STOP_CURVE * u).exp() - end) / (1.0 - end)).max(0.0)
        } else {
            1.0
        };
        let level = (curve / 0.35).min(1.0) as f32;
        (base * curve, ease(level))
    }

    /// One output sample of the event; `None` once it is over.
    fn tick(&mut self, cap: &Capture, tm: &Timing) -> Option<Tick> {
        let releasing = self.t >= self.release_at;
        self.w = slew(self.w, if releasing { 0.0 } else { 1.0 }, tm.fade_step);
        if releasing && self.w <= 0.0 {
            self.on = false;
            return None;
        }
        let tail = self.spec.play < self.spec.total && self.t >= self.spec.play;
        self.x = slew(self.x, if tail { 0.0 } else { 1.0 }, tm.fade_step);

        // Repeat boundaries are handled when the next sample is due, so a new
        // event that arrives exactly on one re-anchors from the pass that is
        // still running instead of stacking two joints on the same sample.
        let run = self.spec.kind == Kind::Run;
        if !run {
            let seg = self.t.checked_div(self.seg_len).map_or(0, |s| s.min(u64::from(self.spec.ramp)));
            if seg != self.seg {
                self.seg = seg;
                self.grid = (self.spec.grid / (1u64 << seg) as f64).max(MIN_GRID);
                self.rep += 1;
                self.begin_rep(0.0, tm);
            } else if self.u >= self.grid {
                let carry = self.u - self.grid;
                self.rep += 1;
                self.begin_rep(carry, tm);
            }
        }

        let mut main_w = 1.0f32;
        let (mut gl, mut gr) = (0.0f32, 0.0f32);
        for g in &mut self.ghosts {
            if g.w > 0.0 {
                let (l, r) = cap.read_rel(g.base, g.q);
                gl += l * g.w;
                gr += r * g.w;
                main_w -= g.w;
                g.q += g.step;
                g.w = (g.w - g.dw).max(0.0);
            }
        }
        let (mut l, mut r) = cap.read_rel(self.base, self.q);
        if main_w != 1.0 {
            l = l * main_w.max(0.0) + gl;
            r = r * main_w.max(0.0) + gr;
        }
        self.gain += (self.gain_target - self.gain) * tm.gain_coef;
        let mut g = self.gain;

        let run_state = run.then(|| self.run_state());
        if let Some((_, level)) = run_state {
            g *= level;
        }
        let progress = if self.spec.play == u64::MAX { 0.0 } else { (self.t as f64 / self.spec.play.max(1) as f64).min(1.0) as f32 };
        if self.spec.level_end_db != 0.0 {
            g *= (self.spec.level_end_db * progress * (std::f32::consts::LN_10 / 20.0)).exp();
        }
        let out = Tick { l: l * g, r: r * g, w: ease(self.w), x: ease(self.x), progress, t: self.t };

        self.t += 1;
        match run_state {
            Some((speed, _)) => {
                self.step = speed;
                // Never reach back further than the ring holds.
                self.q = (self.q + speed).max((cap.wr - self.base) as f64 - tm.max_lag);
            }
            None => {
                self.q += self.step;
                self.u += 1.0;
            }
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// The processor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct StepCfg {
    on: bool,
    prob: f64,
    div: usize,
    pitch: f32,
    rev: bool,
    /// Linear; 0 below -59.5 dB.
    level: f32,
    stop: bool,
    filter: bool,
}

/// Parameters in the form the audio path reads them.
#[derive(Clone, Copy)]
struct Cfg {
    grid_mode: bool,
    interval: usize,
    offset: usize,
    chance: f64,
    gate: usize,
    grid: usize,
    triplets: bool,
    variation: f64,
    pitch: f32,
    pitch_decay: f32,
    decay_db: f32,
    rev: RevMode,
    filter_on: bool,
    mix_mode: MixMode,
    tape_stop: bool,
    stop: usize,
    half: bool,
    ramp: u32,
    hold: bool,
    seed: u32,
    mix: f32,
    out: f32,
    step_rate: usize,
    length: usize,
    steps: [StepCfg; STEPS],
    ties: [bool; STEPS],
    swing: f64,
    pitch_end: f32,
    filter_end: f32,
    level_db: f32,
    level_end_db: f32,
}

fn beats_of(entry: (f64, bool), bar: f64) -> f64 {
    if entry.1 { entry.0 * bar } else { entry.0 }
}

pub struct Stutter {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    tm: Timing,
    cap: Capture,
    cfg: Cfg,
    voice: Voice,

    // Block context.
    pos: f64,
    dbeat: f64,
    spb: f64,
    bar: f64,
    expect: f64,
    free: f64,
    was_playing: bool,
    synced: bool,
    pending_jump: bool,

    // Trigger clock: slot = floor((beats − off) · rate).
    rate: f64,
    off: f64,
    clock_key: u64,
    synced_key: u64,
    last_slot: i64,
    /// Samples recorded since the last discontinuity of the song position.
    since_jump: i64,

    holding: bool,
    hold_prev: bool,

    filt: [SvfState; 2],
    fcoef: SvfCoeffs,
    /// Filter coefficients at the start of an event, and what they come from.
    fbase: SvfCoeffs,
    ffreq: f32,
    fq: f32,
    /// Swing in use by the clock (Grid mode only).
    swing: f64,
    mix_s: f32,
    out_s: f32,
    smooth: f32,

    triggers: u64,
    step_active: i32,
}

/// Lengths in beats of the Classic mode's `Interval` and `Gate` choices (for
/// editors), given a bar of `bar_beats` beats.
pub fn classic_lengths(interval: usize, gate: usize, bar_beats: f64) -> (f64, f64) {
    (beats_of(INTERVALS[interval.min(INTERVALS.len() - 1)], bar_beats), beats_of(GATES[gate.min(GATES.len() - 1)], bar_beats))
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Stutter::new())
}

impl Default for Stutter {
    fn default() -> Self {
        Self::new()
    }
}

impl Stutter {
    pub fn new() -> Self {
        let params = params();
        let step = StepCfg { on: false, prob: 100.0, div: 3, pitch: 0.0, rev: false, level: 1.0, stop: false, filter: false };
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Stutter",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["glitch", "stutter", "beat repeat", "tape stop", "half-time", "rhythm"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            tm: Timing::new(48_000.0, 0.0),
            cap: Capture::new(0),
            cfg: Cfg {
                grid_mode: false,
                interval: 5,
                offset: 0,
                chance: 100.0,
                gate: 4,
                grid: 1,
                triplets: false,
                variation: 0.0,
                pitch: 0.0,
                pitch_decay: 0.0,
                decay_db: 0.0,
                rev: RevMode::Fwd,
                filter_on: false,
                mix_mode: MixMode::Insert,
                tape_stop: false,
                stop: 2,
                half: false,
                ramp: 0,
                hold: false,
                seed: 0,
                mix: 1.0,
                out: 1.0,
                step_rate: 2,
                length: STEPS,
                steps: [step; STEPS],
                ties: [false; STEPS],
                swing: 0.0,
                pitch_end: 0.0,
                filter_end: 0.0,
                level_db: 0.0,
                level_end_db: 0.0,
            },
            voice: Voice::idle(),
            pos: 0.0,
            dbeat: 120.0 / 60.0 / 48_000.0,
            spb: 24_000.0,
            bar: 4.0,
            expect: 0.0,
            free: 0.0,
            was_playing: false,
            synced: false,
            pending_jump: false,
            rate: 0.25,
            off: 0.0,
            clock_key: 0,
            synced_key: u64::MAX,
            last_slot: 0,
            since_jump: HUGE,
            holding: false,
            hold_prev: false,
            filt: [SvfState::default(); 2],
            fcoef: SvfCoeffs::passthrough(),
            fbase: SvfCoeffs::passthrough(),
            ffreq: 1_200.0,
            fq: 1.0,
            swing: 0.0,
            mix_s: 1.0,
            out_s: 1.0,
            smooth: 0.01,
            triggers: 0,
            step_active: -1,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let ps = &self.params;
        let c = &mut self.cfg;
        c.grid_mode = ps.choice(p::MODE) == 1;
        c.interval = ps.choice(p::INTERVAL).min(INTERVALS.len() - 1);
        c.offset = ps.choice(p::OFFSET).min(OFFSET_NAMES.len() - 1);
        c.chance = f64::from(ps.plain(p::CHANCE));
        c.gate = ps.choice(p::GATE).min(GATES.len() - 1);
        c.grid = ps.choice(p::GRID).min(GRIDS.len() - 1);
        c.triplets = ps.on(p::TRIPLETS);
        c.variation = f64::from(ps.plain(p::VARIATION));
        c.pitch = ps.plain(p::PITCH);
        c.pitch_decay = ps.plain(p::PITCH_DECAY);
        c.decay_db = ps.plain(p::DECAY);
        c.rev = match ps.choice(p::REVERSE) {
            1 => RevMode::Rev,
            2 => RevMode::PingPong(false),
            3 => RevMode::Random,
            _ => RevMode::Fwd,
        };
        c.filter_on = ps.on(p::FILTER_ON);
        c.mix_mode = match ps.choice(p::MIX_MODE) {
            1 => MixMode::Mix,
            2 => MixMode::Duck,
            3 => MixMode::Gate,
            _ => MixMode::Insert,
        };
        c.tape_stop = ps.on(p::TAPE_STOP);
        c.stop = ps.choice(p::STOP_TIME).min(STOPS.len() - 1);
        c.half = ps.on(p::HALF_TIME);
        c.ramp = ps.choice(p::RAMP).min(4) as u32;
        c.hold = ps.on(p::HOLD);
        c.seed = ps.plain(p::SEED).round().max(0.0) as u32;
        c.mix = (ps.plain(p::MIX) / 100.0).clamp(0.0, 1.0);
        c.out = db_to_gain(ps.plain(p::OUTPUT));
        c.step_rate = ps.choice(p::STEP_RATE).min(STEP_RATES.len() - 1);
        c.length = (ps.choice(p::LENGTH) + 1).min(STEPS);
        for (i, st) in c.steps.iter_mut().enumerate() {
            let i = i as u32;
            *st = StepCfg {
                on: ps.on(p::step(i, p::STEP_ON)),
                prob: f64::from(ps.plain(p::step(i, p::STEP_PROB))),
                div: ps.choice(p::step(i, p::STEP_DIV)).min(DIVS.len() - 1),
                pitch: ps.plain(p::step(i, p::STEP_PITCH)).round(),
                rev: ps.on(p::step(i, p::STEP_REV)),
                level: {
                    let db = ps.plain(p::step(i, p::STEP_LEVEL));
                    if db <= -59.5 { 0.0 } else { db_to_gain(db) }
                },
                stop: ps.on(p::step(i, p::STEP_STOP)),
                filter: ps.on(p::step(i, p::STEP_FILTER)),
            };
            c.ties[i as usize] = ps.on(p::tie(i));
        }
        c.swing = f64::from(ps.plain(p::SWING)).round().clamp(0.0, 75.0) / 100.0;
        c.pitch_end = ps.plain(p::PITCH_END);
        c.filter_end = ps.plain(p::FILTER_END);
        c.level_db = ps.plain(p::LEVEL);
        c.level_end_db = ps.plain(p::LEVEL_END);
        let width = (ps.plain(p::FILTER_WIDTH) / 100.0).clamp(0.0, 1.0);
        let q = 0.5 * 24f32.powf(1.0 - width);
        self.ffreq = ps.plain(p::FILTER_FREQ);
        self.fq = q;
        self.fbase = SvfCoeffs::new(SvfMode::Bandpass, self.ffreq.min(0.45 * self.sr as f32), q, 0.0, self.sr);
        self.fcoef = self.fbase;
        self.refresh_clock();
    }

    fn refresh_clock(&mut self) {
        let c = &self.cfg;
        if c.grid_mode {
            self.rate = 1.0 / STEP_RATES[c.step_rate];
            self.off = 0.0;
            self.swing = c.swing;
        } else {
            self.swing = 0.0;
            self.rate = 1.0 / beats_of(INTERVALS[c.interval], self.bar);
            self.off = c.offset as f64 * 0.25;
        }
        let key = ((self.swing * 100.0).round() as u64) << 48 | (u64::from(c.grid_mode) << 40) | ((c.interval as u64) << 32) | ((c.offset as u64) << 24) | ((c.step_rate as u64) << 16);
        self.clock_key = key ^ mix64(self.bar.to_bits());
    }

    #[inline]
    fn clock_phase(&self, beats: f64) -> f64 {
        (beats - self.off) * self.rate + CLOCK_EPS
    }

    /// Slot containing clock phase `u` (in steps). Swing delays the start of
    /// every odd slot by that share of a slot, shortening it and lengthening
    /// the even slot before it; pairs still add up to two slots.
    #[inline]
    fn slot_of(&self, u: f64) -> i64 {
        if self.swing == 0.0 {
            return u.floor() as i64;
        }
        let k = (u * 0.5).floor();
        let r = u - 2.0 * k;
        k as i64 * 2 + i64::from(r >= 1.0 + self.swing)
    }

    /// Clock phase (in steps) at which `slot` begins.
    fn slot_start(&self, slot: i64) -> f64 {
        if self.swing == 0.0 {
            return slot as f64;
        }
        let k = slot.div_euclid(2);
        (k * 2) as f64 + if slot.rem_euclid(2) == 1 { 1.0 + self.swing } else { 0.0 }
    }

    /// Does this slot start an event? A pure function of the slot and the seed.
    fn passes(&self, slot: i64) -> bool {
        let c = &self.cfg;
        if c.grid_mode {
            let idx = slot.rem_euclid(c.length as i64) as usize;
            let st = &c.steps[idx];
            // A tied step only extends the event before it.
            !c.ties[idx] && st.on && unit(c.seed, slot, SALT_STEP) * 100.0 < st.prob
        } else {
            unit(c.seed, slot, SALT_CHANCE) * 100.0 < c.chance
        }
    }

    /// The event that starts at `slot` (which has passed).
    fn build_spec(&self, slot: i64) -> Spec {
        let c = &self.cfg;
        let (spb, bar, tm) = (self.spb, self.bar, &self.tm);
        let min_play = 4 * tm.fade_len;
        let id = mix64(u64::from(c.seed) << 32 ^ (slot as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ SALT_ID);
        let grid_step = c.grid_mode.then(|| &c.steps[slot.rem_euclid(c.length as i64) as usize]);
        let kind = if c.tape_stop || grid_step.is_some_and(|st| st.stop) { Kind::Run } else { Kind::Loop };
        let (grid_beats, play_beats, total_beats, end_step, pitch, rev) = if c.grid_mode {
            let st = &c.steps[slot.rem_euclid(c.length as i64) as usize];
            let step_beats = STEP_RATES[c.step_rate];
            // Tied steps after this one are part of the same event.
            let mut held = 1i64;
            while (held as usize) < c.length && c.ties[(slot + held).rem_euclid(c.length as i64) as usize] {
                held += 1;
            }
            let span = (self.slot_start(slot + held) - self.slot_start(slot)) * step_beats;
            let rev = if st.rev {
                match c.rev {
                    RevMode::Fwd | RevMode::Random => RevMode::Rev,
                    RevMode::Rev => RevMode::Fwd,
                    RevMode::PingPong(back) => RevMode::PingPong(!back),
                }
            } else {
                c.rev
            };
            (DIVS[st.div], span, span, span / held as f64, c.pitch + st.pitch, rev)
        } else {
            let mut grid = beats_of(GRIDS[c.grid], bar);
            if c.triplets {
                grid *= 2.0 / 3.0;
            }
            if c.variation > 0.0 && unit(c.seed, slot, SALT_VARIATION) * 100.0 < c.variation {
                grid *= match (unit(c.seed, slot, SALT_VARIATION_PICK) * 3.0) as u32 {
                    0 => 0.5,
                    1 => 2.0 / 3.0,
                    _ => 2.0,
                };
            }
            let interval = beats_of(INTERVALS[c.interval], bar);
            let play = beats_of(GATES[c.gate], bar);
            let total = if c.mix_mode == MixMode::Gate { play.max(interval) } else { play };
            (grid, play, total, interval, c.pitch, c.rev)
        };
        let play = ((play_beats * spb).round() as u64).max(min_play);
        let mut total = ((total_beats * spb).round() as u64).max(play);
        if total - play < 2 * tm.fade_len {
            total = play;
        }
        // The next event follows without a gap if the end of this one is a
        // clock boundary that fires.
        let m = total_beats / end_step;
        let chain = m >= 1.0 && (m - m.round()).abs() < 1e-6 && self.passes(slot + m.round() as i64);
        Spec {
            kind,
            id,
            grid: (grid_beats * spb).max(MIN_GRID),
            play,
            total,
            pitch,
            pitch_step: c.pitch_decay,
            gain_step_db: c.decay_db,
            rev,
            half: c.half,
            ramp: c.ramp,
            stop: ((beats_of(STOPS[c.stop], bar) * spb).round() as u64).min(play),
            chain,
            level: db_to_gain(c.level_db) * grid_step.map_or(1.0, |st| st.level),
            level_end_db: c.level_end_db.min(12.0 - c.level_db),
            pitch_end: c.pitch_end,
            filter: c.filter_on || grid_step.is_some_and(|st| st.filter),
            filter_end: c.filter_end,
        }
    }

    fn trigger(&mut self, slot: i64) {
        if !self.passes(slot) {
            return;
        }
        let spec = self.build_spec(slot);
        if (self.since_jump as f64) < spec.needed() {
            return;
        }
        self.triggers += 1;
        self.step_active = if self.cfg.grid_mode { slot.rem_euclid(self.cfg.length as i64) as i32 } else { -1 };
        self.begin(spec);
    }

    fn begin(&mut self, spec: Spec) {
        if !self.voice.on {
            self.filt = [SvfState::default(); 2];
        }
        self.fcoef = self.fbase;
        self.voice.start(spec, self.cap.wr, &self.tm);
    }

    fn start_hold(&mut self, beats: f64) {
        let c = &self.cfg;
        let grid_beats = beats_of(GRIDS[c.grid], self.bar) * if c.triplets { 2.0 / 3.0 } else { 1.0 };
        let slot = (beats * 8.0).floor() as i64;
        let spec = Spec {
            kind: Kind::Loop,
            id: mix64(u64::from(c.seed) << 32 ^ slot as u64 ^ SALT_ID),
            grid: (grid_beats * self.spb).max(MIN_GRID),
            play: u64::MAX,
            total: u64::MAX,
            pitch: c.pitch,
            pitch_step: c.pitch_decay,
            gain_step_db: c.decay_db,
            rev: c.rev,
            half: c.half,
            ramp: 0,
            stop: 0,
            chain: false,
            level: db_to_gain(c.level_db),
            level_end_db: 0.0,
            pitch_end: 0.0,
            filter: c.filter_on,
            filter_end: 0.0,
        };
        self.holding = true;
        self.step_active = -1;
        self.begin(spec);
    }

    /// The song position jumped (or playback began): cancel what is running
    /// and align the clock so only a boundary at, or after, `beats` fires.
    fn on_jump(&mut self, beats: f64) {
        if !self.holding && self.voice.on {
            self.voice.release_now();
        }
        let x = self.clock_phase(beats);
        let slot = self.slot_of(x);
        let landed = x - self.slot_start(slot) <= 2.0 * CLOCK_EPS + self.dbeat * self.rate * 1.0001;
        self.last_slot = if landed { slot - 1 } else { slot };
        self.since_jump = if landed { HUGE } else { 0 };
        self.synced_key = self.clock_key;
    }

    #[inline]
    fn frame(&mut self, i: usize, l: f32, r: f32) -> (f32, f32) {
        let beats = self.pos + i as f64 * self.dbeat;
        let slot = self.slot_of(self.clock_phase(beats));
        if slot != self.last_slot {
            self.last_slot = slot;
            if !self.holding {
                self.trigger(slot);
            }
        }
        self.cap.push(l, r);
        self.since_jump = self.since_jump.saturating_add(1);

        let (mut ol, mut or) = (l, r);
        if self.voice.on
            && let Some(tk) = self.voice.tick(&self.cap, &self.tm)
        {
            let (mut wl, mut wr) = (tk.l, tk.r);
            if self.voice.spec.filter {
                // A ramped centre is re-tuned every 16 samples of the event.
                if self.voice.spec.filter_end != 0.0 && tk.t & 15 == 0 {
                    let hz = self.ffreq * 2f32.powf(self.voice.spec.filter_end * tk.progress);
                    self.fcoef = SvfCoeffs::new(SvfMode::Bandpass, hz.clamp(20.0, 0.45 * self.sr as f32), self.fq, 0.0, self.sr);
                }
                wl = self.filt[0].process(&self.fcoef, wl);
                wr = self.filt[1].process(&self.fcoef, wr);
            }
            let ww = tk.w * tk.x;
            let dry_gain = match self.cfg.mix_mode {
                MixMode::Insert | MixMode::Gate => 1.0 - tk.w,
                MixMode::Mix => 1.0,
                MixMode::Duck => 1.0 - tk.w * tk.progress,
            };
            ol = l * dry_gain + wl * ww;
            or = r * dry_gain + wr * ww;
            if self.mix_s != 1.0 {
                ol = l + (ol - l) * self.mix_s;
                or = r + (or - r) * self.mix_s;
            }
        }
        if self.mix_s != self.cfg.mix {
            self.mix_s = slew(self.mix_s, self.cfg.mix, self.smooth);
        }
        if self.out_s != self.cfg.out {
            self.out_s += (self.cfg.out - self.out_s) * self.smooth * 4.0;
            if (self.out_s - self.cfg.out).abs() < 1e-5 {
                self.out_s = self.cfg.out;
            }
        }
        if self.out_s != 1.0 {
            ol *= self.out_s;
            or *= self.out_s;
        }
        (sanitize(ol), sanitize(or))
    }
}

impl Segmented for Stutter {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let b0 = self.pos + start as f64 * self.dbeat;
        if self.pending_jump {
            self.pending_jump = false;
            self.on_jump(b0);
        } else if self.clock_key != self.synced_key {
            // The grid itself changed: wait for its next boundary.
            self.synced_key = self.clock_key;
            self.last_slot = self.slot_of(self.clock_phase(b0));
        }
        if self.cfg.hold != self.hold_prev {
            self.hold_prev = self.cfg.hold;
            if self.cfg.hold {
                self.start_hold(b0);
            } else if self.holding {
                self.holding = false;
                self.voice.release_now();
            }
        }
        for_frames(ctx.buffer, start, end, |i, l, r| self.frame(i, l, r));
    }
}

impl Processor for Stutter {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate.max(8_000.0);
        let frames = (BUFFER_SECONDS * self.sr).ceil() as usize + 128;
        if self.cap.len() < frames || self.cap.len() > 4 * frames {
            self.cap = Capture::new(frames);
        }
        self.tm = Timing::new(self.sr, self.cap.max_lag());
        self.smooth = (1.0 - (-1.0 / (0.005 * self.sr)).exp()) as f32;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.cap.wr = 0;
        self.voice = Voice::idle();
        self.synced = false;
        self.pending_jump = false;
        self.free = 0.0;
        self.expect = 0.0;
        self.was_playing = false;
        self.last_slot = 0;
        self.synced_key = u64::MAX;
        self.since_jump = HUGE;
        self.holding = false;
        self.hold_prev = false;
        self.filt = [SvfState::default(); 2];
        self.mix_s = self.cfg.mix;
        self.out_s = self.cfg.out;
        self.triggers = 0;
        self.step_active = -1;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.buffer.frames();
        if frames == 0 {
            return;
        }
        let tr = *ctx.transport;
        let bpm = if tr.bpm.is_finite() { tr.bpm.clamp(1.0, 999.0) } else { 120.0 };
        self.dbeat = bpm / 60.0 / self.sr;
        self.spb = 60.0 * self.sr / bpm;
        let (n, d) = tr.time_sig;
        let bar = if n > 0 && d > 0 { f64::from(n) * 4.0 / f64::from(d) } else { 4.0 };
        if bar != self.bar {
            self.bar = bar;
            self.refresh_clock();
        }
        let pos = if !tr.playing {
            self.free
        } else if tr.position_beats.is_finite() {
            tr.position_beats
        } else {
            0.0
        };
        if !self.synced || tr.playing != self.was_playing || (pos - self.expect).abs() > JUMP_TOL {
            self.pending_jump = true;
        }
        self.synced = true;
        self.was_playing = tr.playing;
        self.pos = pos;
        self.expect = pos + frames as f64 * self.dbeat;
        self.free = self.expect;
        drive(self, ctx);
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
        let v = &self.voice;
        let state = match (v.on, v.spec.kind) {
            (false, _) => 0.0,
            (true, Kind::Run) => 2.0,
            (true, Kind::Loop) if v.x < 0.5 => 3.0,
            (true, Kind::Loop) => 1.0,
        };
        let pos = if v.on && v.spec.kind == Kind::Loop && v.src_len > 0.0 { ((v.q + v.src_len) / v.src_len).clamp(0.0, 1.0) } else { 0.0 };
        let progress = if v.on && v.spec.play != u64::MAX { (v.t as f64 / v.spec.play.max(1) as f64).min(1.0) } else { 0.0 };
        let rate = if v.on { v.step.abs() } else { 1.0 };
        let mut n = write_meter(out, 0, &[pos as f32, state, self.step_active as f32, rate as f32, progress as f32, v.w]);
        // The chunk that is frozen, or the one a trigger now would freeze.
        let (start, len) = if v.on && v.spec.kind == Kind::Loop && v.src_len > 0.0 {
            (v.wr0 - v.src_len as i64, v.src_len as i64)
        } else {
            let grid = beats_of(GRIDS[self.cfg.grid], self.bar) * self.spb;
            let len = (grid as i64).clamp(PEAKS as i64, self.cap.max_lag() as i64);
            (self.cap.wr - len, len)
        };
        for b in 0..PEAKS {
            if n >= out.len() {
                break;
            }
            let lo = start + len * b as i64 / PEAKS as i64;
            let hi = start + len * (b as i64 + 1) / PEAKS as i64;
            let stride = ((hi - lo) / 8).max(1);
            let mut peak = 0.0f32;
            let mut j = lo;
            while j < hi.max(lo + 1) {
                let (l, r) = self.cap.at(j);
                peak = peak.max(l.abs()).max(r.abs());
                j += stride;
            }
            out[n] = peak;
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests;
