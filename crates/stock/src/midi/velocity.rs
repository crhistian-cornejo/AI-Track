//! `midi.velocity` — reshapes the velocity of every note-on.
//!
//! Order of the stages, on the velocity as 0…1 (the same in the display, 1…127):
//!
//! 1. *Curve* (−100…+100 %): a power law. Positive lifts the soft notes
//!    (`x^(2^(−1.5·c))`), negative pushes them down; 0 leaves it straight.
//! 2. *Compand* around *Pivot*: negative compresses (every velocity moves
//!    toward the pivot, −100 % puts them all on it), positive expands (up to
//!    three times the distance from the pivot at +100 %).
//! 3. *Gain* (±64): added in velocity steps.
//! 4. *Random* (0…100 %): adds a draw in ±(Random / 2) of the full range. The
//!    draw is a hash of *Seed* and the song position of the note (the shared
//!    [`PositionHash`]), so a note at a given position always gets the same
//!    draw: live, exported from the middle of the song, after a stop, a jump or
//!    a loop wrap, and for any block size. Notes at the very same position are
//!    told apart by their order of arrival.
//! 5. *Mode*. **Map** squeezes the result linearly into *Min*…*Max* (with the
//!    defaults 1…127 it changes nothing); **Clip** keeps it and clamps it to
//!    *Min*…*Max*; **Fixed** ignores the played velocity (and stages 1–2) and
//!    uses *Fixed*, still shaken by Gain and Random, clamped to Min…Max.
//!
//! The result is rounded to a whole velocity 1…127. Note-offs, ids and every
//! other event pass untouched.

use crate::common::ParamStore;
use crate::midi::kit::{Draw, PositionHash};
use aitrack_dsp::{Category, EventKind, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.velocity";

pub mod p {
    pub const MODE: u32 = 0;
    pub const FIXED: u32 = 1;
    pub const CURVE: u32 = 2;
    pub const COMPAND: u32 = 3;
    pub const PIVOT: u32 = 4;
    pub const GAIN: u32 = 5;
    pub const MIN: u32 = 6;
    pub const MAX: u32 = 7;
    pub const RANDOM: u32 = 8;
    pub const SEED: u32 = 9;
}

const MODES: &[&str] = &["Map", "Clip", "Fixed"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 0.0).choices(MODES),
        ParamInfo::new(p::FIXED, "Fixed", 1.0, 127.0, 100.0),
        ParamInfo::new(p::CURVE, "Curve", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::COMPAND, "Compand", -100.0, 100.0, 0.0).unit("%").short("Comp"),
        ParamInfo::new(p::PIVOT, "Pivot", 1.0, 127.0, 64.0),
        ParamInfo::new(p::GAIN, "Gain", -64.0, 64.0, 0.0),
        ParamInfo::new(p::MIN, "Min", 1.0, 127.0, 1.0),
        ParamInfo::new(p::MAX, "Max", 1.0, 127.0, 127.0),
        ParamInfo::new(p::RANDOM, "Random", 0.0, 100.0, 0.0).unit("%").short("Rand"),
        ParamInfo::new(p::SEED, "Seed", 0.0, 999.0, 0.0),
    ]
}

/// The parameters in plain units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub mode: usize,
    pub fixed: f32,
    /// Exponent of the curve: `x^gamma`.
    pub gamma: f32,
    /// Distance multiplier of the compander.
    pub factor: f32,
    pub pivot: f32,
    pub gain: f32,
    pub min: f32,
    pub max: f32,
    pub random: f32,
    pub seed: u64,
}

impl Shape {
    /// Velocity 1…127 (whole) for a played `velocity` (0…1) and a random draw in −1…1.
    pub fn apply(&self, velocity: f32, draw: f32) -> f32 {
        let mut x = if self.mode == 2 {
            self.fixed / 127.0
        } else {
            let x = velocity.clamp(0.0, 1.0).powf(self.gamma);
            let pivot = self.pivot / 127.0;
            pivot + (x - pivot) * self.factor
        };
        x += self.gain / 127.0 + draw * self.random * 0.5;
        let x = x.clamp(0.0, 1.0);
        let v127 = x * 127.0;
        let out = match self.mode {
            0 => self.min + ((v127 - 1.0) / 126.0).clamp(0.0, 1.0) * (self.max - self.min),
            _ => v127.clamp(self.min, self.max),
        };
        out.round().clamp(1.0, 127.0)
    }
}

pub struct Velocity {
    info: PluginInfo,
    params: ParamStore,
    shape: Shape,
    hash: PositionHash,
    sr: f64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Velocity::new())
}

impl Default for Velocity {
    fn default() -> Self {
        Self::new()
    }
}

impl Velocity {
    pub fn new() -> Self {
        let ps = params();
        let mut v = Self {
            info: PluginInfo {
                id: ID,
                name: "Velocity",
                vendor: "AI-Track",
                category: Category::MidiEffect,
                tags: &["midi", "velocity", "dynamics", "humanize"],
                params: ps.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&ps),
            shape: Shape { mode: 0, fixed: 100.0, gamma: 1.0, factor: 1.0, pivot: 64.0, gain: 0.0, min: 1.0, max: 127.0, random: 0.0, seed: 0 },
            hash: PositionHash::new(0x7665_6C6F),
            sr: 48_000.0,
        };
        v.refresh();
        v
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    fn refresh(&mut self) {
        let ps = &self.params;
        let curve = ps.plain(p::CURVE) / 100.0;
        let compand = ps.plain(p::COMPAND) / 100.0;
        let (a, b) = (ps.plain(p::MIN), ps.plain(p::MAX));
        self.shape = Shape {
            mode: ps.choice(p::MODE).min(2),
            fixed: ps.plain(p::FIXED),
            gamma: (-1.5 * curve).exp2(),
            factor: if compand >= 0.0 { 1.0 + 2.0 * compand } else { 1.0 + compand },
            pivot: ps.plain(p::PIVOT),
            gain: ps.plain(p::GAIN),
            min: a.min(b),
            max: a.max(b),
            random: ps.plain(p::RANDOM) / 100.0,
            seed: ps.plain(p::SEED) as u64,
        };
    }
}

impl Processor for Velocity {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = if sample_rate > 0.0 { sample_rate } else { 48_000.0 };
        self.reset();
        self.params.mark_dirty();
    }

    fn reset(&mut self) {
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

    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        if self.params.take_dirty() {
            self.refresh();
        }
        for e in ctx.events {
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } if velocity > 0.0 => {
                    let sr = if ctx.sample_rate > 0.0 { ctx.sample_rate } else { self.sr };
                    let d = if self.shape.random > 0.0 { Draw::new(self.hash.next(self.shape.seed as u32, ctx.transport, e.offset, sr, 0.0)).signed() } else { 0.0 };
                    let v = self.shape.apply(velocity, d) / 127.0;
                    out.push(e.offset, EventKind::NoteOn { channel, key, velocity: v, note_id });
                }
                EventKind::NoteExpression { .. } => {
                    // Keys and ids are unchanged: it still belongs to its note.
                    out.push(e.offset, e.kind);
                }
                other => {
                    out.push(e.offset, other);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
