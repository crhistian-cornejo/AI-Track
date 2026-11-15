//! `midi.chord` — one key in, up to six notes out.
//!
//! Every voice has a switch, an interval in semitones (−36…+36 from the
//! played key) and a velocity level (0…200 %, 0 mutes it). Voice 1 defaults
//! to the played key itself, so the default is a major triad. With *Fit to
//! Scale* every voice that is not the played key itself (interval 0) is moved
//! to the nearest note of the chosen scale (ties go down), which turns a
//! fixed stack of intervals into a diatonic chord whatever key is pressed.
//!
//! Root and Scale use the scale list and order of `aitrack_model::ScaleKind`
//! (see [`theory`](crate::midi::theory)). *Follow Project* is only a flag for
//! the app: when it is on the editor writes the project's scale into Root and
//! Scale; the device itself never reads it. Changing the project's scale does
//! not rewrite parameters already saved.
//!
//! Each played key remembers the keys it started, so its note-off ends exactly
//! those even if the parameters changed meanwhile. Two played keys that land
//! on the same output key share it: the second note-on retriggers it (note-off
//! then note-on) and the key ends when the last of them is released.
//!
//! Not in this device: strum, chance per voice and learn (the spec's extras).

use crate::common::ParamStore;
use crate::midi::kit::IdAlloc;
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

    pub const fn on(v: usize) -> u32 {
        ON + 3 * v as u32
    }
    pub const fn interval(v: usize) -> u32 {
        INTERVAL + 3 * v as u32
    }
    pub const fn level(v: usize) -> u32 {
        LEVEL + 3 * v as u32
    }
}

const OFF_ON: &[&str] = &["Off", "On"];
const VOICE_ON: [&str; VOICES] = ["Voice 1", "Voice 2", "Voice 3", "Voice 4", "Voice 5", "Voice 6"];
const VOICE_INTERVAL: [&str; VOICES] = ["Interval 1", "Interval 2", "Interval 3", "Interval 4", "Interval 5", "Interval 6"];
const VOICE_LEVEL: [&str; VOICES] = ["Level 1", "Level 2", "Level 3", "Level 4", "Level 5", "Level 6"];
/// Default stack: the played key, a major third and a fifth; the rest off.
const DEFAULT_INTERVALS: [f32; VOICES] = [0.0, 4.0, 7.0, 12.0, 16.0, 19.0];

const MAX_NOTES: usize = 64;

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
    v
}

/// A played key and the output keys it started.
#[derive(Clone, Copy)]
struct Played {
    channel: u8,
    key: u8,
    out: [u8; VOICES],
    n: u8,
}

#[derive(Clone, Copy)]
struct Voice {
    on: bool,
    interval: i32,
    level: f32,
}

pub struct Chord {
    info: PluginInfo,
    params: ParamStore,
    voices: [Voice; VOICES],
    fit: bool,
    mask: u16,
    played: [Played; MAX_NOTES],
    played_len: usize,
    /// How many played keys hold each output key (channel × 128 + key).
    count: [u8; 2048],
    /// Note id of the note currently sounding on each output key.
    cur_id: [i32; 2048],
    ids: IdAlloc,
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
                tags: &["midi", "chord", "harmony"],
                params: ps.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&ps),
            voices: [Voice { on: false, interval: 0, level: 1.0 }; VOICES],
            fit: false,
            mask: 0,
            played: [NO_PLAYED; MAX_NOTES],
            played_len: 0,
            count: [0; 2048],
            cur_id: [0; 2048],
            ids: IdAlloc::new(),
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
        }
        self.fit = self.params.on(p::FIT);
        self.mask = scale_mask(self.params.choice(p::SCALE).min(12), self.params.choice(p::ROOT).min(11));
    }

    #[inline]
    fn slot(channel: u8, key: u8) -> usize {
        (channel as usize & 15) * 128 + (key as usize & 127)
    }

    /// Release the output keys of `played[i]` and forget it.
    fn release(&mut self, i: usize, at: u32, out: &mut EventSink<'_>) {
        let pl = self.played[i];
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

    fn note_on(&mut self, channel: u8, key: u8, velocity: f32, at: u32, out: &mut EventSink<'_>) {
        if let Some(i) = self.played[..self.played_len].iter().position(|n| n.channel == channel && n.key == key) {
            self.release(i, at, out);
        }
        if self.played_len == MAX_NOTES {
            self.dropped += 1;
            return;
        }
        let mut pl = Played { channel, key, out: [0; VOICES], n: 0 };
        for v in self.voices {
            if !v.on || v.level <= 0.0 {
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
            if pl.out[..pl.n as usize].contains(&k) {
                continue;
            }
            let s = Self::slot(channel, k);
            if self.count[s] > 0 {
                // Already sounding for another key: retrigger it.
                let id = self.cur_id[s];
                if !out.push(at, EventKind::NoteOff { channel, key: k, velocity: 0.0, note_id: id }) {
                    self.dropped += 1;
                }
            }
            let id = self.ids.next();
            if out.push(at, EventKind::NoteOn { channel, key: k, velocity: (velocity * v.level).clamp(1.0 / 127.0, 1.0), note_id: id }) {
                self.cur_id[s] = id;
                self.count[s] += 1;
                pl.out[pl.n as usize] = k;
                pl.n += 1;
            } else {
                self.dropped += 1;
            }
        }
        self.played[self.played_len] = pl;
        self.played_len += 1;
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

    fn dropped_events(&self) -> u64 {
        self.dropped
    }

    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        if self.params.take_dirty() {
            self.refresh();
        }
        let last = (ctx.frames.max(1) - 1) as u32;
        for e in ctx.events {
            let at = e.offset.min(last);
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, .. } => self.note_on(channel, key, velocity, at, out),
                EventKind::NoteOff { channel, key, .. } => {
                    if let Some(i) = self.played[..self.played_len].iter().position(|n| n.channel == channel && n.key == key) {
                        self.release(i, at, out);
                    }
                }
                EventKind::AllNotesOff => {
                    while self.played_len > 0 {
                        self.release(self.played_len - 1, at, out);
                    }
                    out.push(at, EventKind::AllNotesOff);
                }
                EventKind::NoteExpression { .. } => {}
                other => {
                    out.push(at, other);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
