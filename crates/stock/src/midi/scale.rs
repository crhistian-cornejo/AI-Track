//! `midi.scale` — corrects every note to a scale.
//!
//! The scale is *Root* + *Scale* (the 13 scales of `aitrack_model::ScaleKind`,
//! in its order, plus *Custom*: twelve per-pitch-class switches). *Follow
//! Project* is a flag for the app: when it is on the editor writes the
//! project's scale into Root and Scale; the device never reads it, and a
//! project scale change does not rewrite saved parameters.
//!
//! * *Mode*: where a note outside the scale goes: the nearest scale note
//!   (a tie goes down), the next one up or the next one down.
//! * *Pass In-Scale*: on (default) the notes already inside are left alone.
//!   Off turns Up / Down into a diatonic step: every note, inside or not,
//!   moves to the next scale note in that direction (Nearest is unaffected).
//! * *Transpose* (±36 st) is applied before correcting, so +7 on a C minor
//!   scale gives you the fifth, corrected.
//! * *Lowest* / *Range*: only keys from Lowest up to Lowest + Range are
//!   processed; the rest go through untouched (not even transposed).
//!
//! A note-off follows its note-on, whatever was changed meanwhile. Two keys
//! corrected to the same note share it: the second retriggers it and it
//! ends with the last. Velocity and note id are kept.

use crate::common::ParamStore;
use crate::midi::theory::{ROOT_NAMES, Snap, in_mask, scale_mask, snap_note};
use aitrack_dsp::{Category, EventKind, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.scale";

pub mod p {
    pub const ROOT: u32 = 0;
    pub const SCALE: u32 = 1;
    pub const MODE: u32 = 2;
    pub const PASS: u32 = 3;
    pub const TRANSPOSE: u32 = 4;
    pub const LOWEST: u32 = 5;
    pub const RANGE: u32 = 6;
    pub const FOLLOW_PROJECT: u32 = 7;
    /// Twelve consecutive ids, C first: the pitch classes of the Custom scale.
    pub const CUSTOM0: u32 = 8;
}

/// Index of the Custom scale in the Scale choices.
pub const CUSTOM: usize = 13;

const MODES: &[&str] = &["Nearest", "Up", "Down"];
const OFF_ON: &[&str] = &["Off", "On"];
const KEY_NAMES: [&str; 12] = ["Key C", "Key C#", "Key D", "Key D#", "Key E", "Key F", "Key F#", "Key G", "Key G#", "Key A", "Key A#", "Key B"];
const KEY_SHORT: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
/// C major, the Custom scale until edited.
const CUSTOM_DEFAULT: [bool; 12] = [true, false, true, false, true, true, false, true, false, true, false, true];

const NONE: u8 = 255;

pub fn scale_choices() -> &'static [&'static str] {
    const ALL: [&str; 14] = [
        "Major",
        "Minor",
        "Harmonic Minor",
        "Melodic Minor",
        "Dorian",
        "Phrygian",
        "Lydian",
        "Mixolydian",
        "Locrian",
        "Major Pentatonic",
        "Minor Pentatonic",
        "Blues",
        "Chromatic",
        "Custom",
    ];
    &ALL
}

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::ROOT, "Root", 0.0, 11.0, 0.0).choices(ROOT_NAMES),
        ParamInfo::new(p::SCALE, "Scale", 0.0, 13.0, 1.0).choices(scale_choices()),
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 0.0).choices(MODES),
        ParamInfo::new(p::PASS, "Pass In-Scale", 0.0, 1.0, 1.0).choices(OFF_ON).short("Pass"),
        ParamInfo::new(p::TRANSPOSE, "Transpose", -36.0, 36.0, 0.0).unit("st").short("Transp"),
        ParamInfo::new(p::LOWEST, "Lowest", 0.0, 127.0, 0.0),
        ParamInfo::new(p::RANGE, "Range", 0.0, 127.0, 127.0),
        ParamInfo::new(p::FOLLOW_PROJECT, "Follow Project", 0.0, 1.0, 0.0).choices(OFF_ON).short("Follow"),
    ];
    for i in 0..12 {
        v.push(ParamInfo::new(p::CUSTOM0 + i as u32, KEY_NAMES[i], 0.0, 1.0, if CUSTOM_DEFAULT[i] { 1.0 } else { 0.0 }).choices(OFF_ON).short(KEY_SHORT[i]));
    }
    v
}

/// What the notes are corrected with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub mask: u16,
    pub snap: Snap,
    pub pass: bool,
    pub transpose: i32,
    pub lowest: i32,
    pub highest: i32,
}

impl Rule {
    /// The note a played `key` becomes.
    pub fn map(&self, key: u8) -> u8 {
        let k = key as i32;
        if k < self.lowest || k > self.highest {
            return key;
        }
        let n = (k + self.transpose).clamp(0, 127);
        if self.pass && in_mask(self.mask, n) {
            return n as u8;
        }
        snap_note(self.mask, n as u8, self.snap, !self.pass)
    }
}

pub struct ScaleFx {
    info: PluginInfo,
    params: ParamStore,
    rule: Rule,
    /// Output key started by each input key (channel × 128 + key), or `NONE`.
    map: [u8; 2048],
    /// How many input keys hold each output key.
    count: [u8; 2048],
    cur_id: [i32; 2048],
}

pub fn create() -> Box<dyn Processor> {
    Box::new(ScaleFx::new())
}

impl Default for ScaleFx {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn slot(channel: u8, key: u8) -> usize {
    (channel as usize & 15) * 128 + (key as usize & 127)
}

impl ScaleFx {
    pub fn new() -> Self {
        let ps = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Scale",
                vendor: "AI-Track",
                category: Category::MidiEffect,
                tags: &["midi", "scale", "key", "quantize", "harmony"],
                params: ps.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&ps),
            rule: Rule { mask: 0, snap: Snap::Nearest, pass: true, transpose: 0, lowest: 0, highest: 127 },
            map: [NONE; 2048],
            count: [0; 2048],
            cur_id: [0; 2048],
        };
        s.refresh();
        s
    }

    /// The rule the current parameters describe.
    pub fn rule(&self) -> Rule {
        self.rule
    }

    fn refresh(&mut self) {
        let ps = &self.params;
        let scale = ps.choice(p::SCALE).min(CUSTOM);
        let mask = if scale == CUSTOM { (0..12).fold(0u16, |m, i| if ps.on(p::CUSTOM0 + i) { m | 1 << i } else { m }) } else { scale_mask(scale, ps.choice(p::ROOT).min(11)) };
        let lowest = ps.plain(p::LOWEST).round() as i32;
        self.rule = Rule {
            mask,
            snap: match ps.choice(p::MODE) {
                1 => Snap::Up,
                2 => Snap::Down,
                _ => Snap::Nearest,
            },
            pass: ps.on(p::PASS),
            transpose: ps.plain(p::TRANSPOSE).round() as i32,
            lowest,
            highest: lowest + ps.plain(p::RANGE).round() as i32,
        };
    }

    fn release(&mut self, channel: u8, key: u8, at: u32, out: &mut EventSink<'_>) {
        let s = slot(channel, key);
        let k = self.map[s];
        if k == NONE {
            return;
        }
        self.map[s] = NONE;
        let o = slot(channel, k);
        self.count[o] = self.count[o].saturating_sub(1);
        if self.count[o] == 0 {
            out.push(at, EventKind::NoteOff { channel, key: k, velocity: 0.0, note_id: self.cur_id[o] });
        }
    }
}

impl Processor for ScaleFx {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
        self.params.mark_dirty();
    }

    fn reset(&mut self) {
        self.map.fill(NONE);
        self.count.fill(0);
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
        let last = (ctx.frames.max(1) - 1) as u32;
        for e in ctx.events {
            let at = e.offset.min(last);
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } => {
                    // A key pressed again without being released ends first.
                    self.release(channel, key, at, out);
                    let mapped = self.rule.map(key);
                    let o = slot(channel, mapped);
                    if self.count[o] > 0 {
                        out.push(at, EventKind::NoteOff { channel, key: mapped, velocity: 0.0, note_id: self.cur_id[o] });
                    }
                    if out.push(at, EventKind::NoteOn { channel, key: mapped, velocity, note_id }) {
                        self.map[slot(channel, key)] = mapped;
                        self.count[o] += 1;
                        self.cur_id[o] = note_id;
                    }
                }
                EventKind::NoteOff { channel, key, .. } => self.release(channel, key, at, out),
                EventKind::NoteExpression { note_id, key, expr, value } => {
                    // Follows the note it belongs to.
                    if let Some(k) = (0..16u8).map(|c| self.map[slot(c, key)]).find(|k| *k != NONE) {
                        out.push(at, EventKind::NoteExpression { note_id, key: k, expr, value });
                    }
                }
                EventKind::AllNotesOff => {
                    self.reset();
                    out.push(at, EventKind::AllNotesOff);
                }
                other => {
                    out.push(at, other);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
