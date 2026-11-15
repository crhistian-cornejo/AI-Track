//! `midi.pitch` — transposes notes by octaves and semitones.
//!
//! * **Octave / Semitone**: the shift (±4 octaves, ±12 semitones).
//! * **Lowest / Width**: only keys `Lowest .. Lowest + Width` are shifted.
//!   `Outside` decides what happens to the others: `Pass` leaves them as they
//!   are, `Block` drops them.
//! * **Overflow**: a result outside 0..=127 is dropped (`Block`), folded back
//!   by octaves (`Fold`) or held at the edge (`Limit`).
//! * **Layer**: keeps the played note and adds the transposed one, at
//!   `Layer Level` of the velocity.
//!
//! The note-off of a note follows the shift it was started with, whatever
//! the knobs did in between.

use super::kit::{IdAlloc, Ledger, fold_octaves};
use crate::common::{ParamStore, load_state, save_state};
use aitrack_dsp::{Category, EventKind, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "midi.pitch";

pub mod p {
    pub const OCTAVE: u32 = 0;
    pub const SEMITONE: u32 = 1;
    pub const LOWEST: u32 = 2;
    pub const WIDTH: u32 = 3;
    pub const OUTSIDE: u32 = 4;
    pub const OVERFLOW: u32 = 5;
    pub const LAYER: u32 = 6;
    pub const LAYER_LEVEL: u32 = 7;
}

/// `Outside` choices.
pub mod outside {
    pub const PASS: f32 = 0.0;
    pub const BLOCK: f32 = 1.0;
}

/// `Overflow` choices.
pub mod overflow {
    pub const BLOCK: f32 = 0.0;
    pub const FOLD: f32 = 1.0;
    pub const LIMIT: f32 = 2.0;
}

const OUTSIDES: &[&str] = &["Pass", "Block"];
const OVERFLOWS: &[&str] = &["Block", "Fold", "Limit"];
const OFF_ON: &[&str] = &["Off", "On"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::OCTAVE, "Octave", -4.0, 4.0, 0.0).choices(&[]).unit("oct").short("Oct"),
        ParamInfo::new(p::SEMITONE, "Semitone", -12.0, 12.0, 0.0).choices(&[]).unit("st").short("Semi"),
        ParamInfo::new(p::LOWEST, "Lowest", 0.0, 127.0, 0.0).choices(&[]).short("Low"),
        ParamInfo::new(p::WIDTH, "Width", 1.0, 128.0, 128.0).choices(&[]),
        ParamInfo::new(p::OUTSIDE, "Outside", 0.0, 1.0, outside::PASS).choices(OUTSIDES),
        ParamInfo::new(p::OVERFLOW, "Overflow", 0.0, 2.0, overflow::FOLD).choices(OVERFLOWS),
        ParamInfo::new(p::LAYER, "Layer", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::LAYER_LEVEL, "Layer Level", 0.0, 200.0, 100.0).unit("%").short("Level"),
    ]
}

/// What a played key became.
#[derive(Clone, Copy)]
struct Map {
    used: bool,
    /// The key the played note sounds on (`NONE` when it was dropped) and its id.
    key: u8,
    id: i32,
    /// The added note of a layer.
    copy_key: u8,
    copy_id: i32,
}

const NONE: u8 = 255;
const UNUSED: Map = Map { used: false, key: NONE, id: 0, copy_key: NONE, copy_id: 0 };

pub struct Pitch {
    info: PluginInfo,
    params: ParamStore,
    ledger: Ledger,
    maps: Vec<Map>,
    ids: IdAlloc,
    clock: i64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Pitch::new())
}

impl Default for Pitch {
    fn default() -> Self {
        Self::new()
    }
}

impl Pitch {
    pub fn new() -> Self {
        let params = params();
        Self {
            info: PluginInfo { id: ID, name: "Pitch", vendor: "AI-Track", category: Category::MidiEffect, tags: &["midi", "transpose", "pitch", "octave"], params: params.clone(), inputs: 2, outputs: 2, has_sidechain: false },
            params: ParamStore::new(&params),
            ledger: Ledger::new(),
            maps: vec![UNUSED; 2048],
            ids: IdAlloc::new(),
            clock: 0,
        }
    }
}

/// Release what a played note became.
fn end(ledger: &mut Ledger, out: &mut EventSink<'_>, offset: u32, abs: i64, channel: u8, m: Map, velocity: f32) {
    if m.key != NONE {
        ledger.off_own(out, offset, abs, channel, m.key, velocity, m.id);
    }
    if m.copy_key != NONE {
        ledger.off_own(out, offset, abs, channel, m.copy_key, velocity, m.copy_id);
    }
}

impl Processor for Pitch {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: usize) {
        self.reset();
    }

    fn reset(&mut self) {
        self.ledger.forget();
        self.maps.fill(UNUSED);
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
        let pr = &self.params;
        let shift = pr.plain(p::OCTAVE).round() as i32 * 12 + pr.plain(p::SEMITONE).round() as i32;
        let lowest = pr.plain(p::LOWEST).round() as i32;
        let highest = lowest + pr.plain(p::WIDTH).round() as i32 - 1;
        let block_outside = pr.choice(p::OUTSIDE) == 1;
        let mode = pr.choice(p::OVERFLOW);
        let layer = pr.on(p::LAYER);
        let level = pr.plain(p::LAYER_LEVEL) * 0.01;
        let clock = self.clock;
        self.ledger.begin_block(out);
        for e in ctx.events {
            let abs = clock + e.offset as i64;
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } => {
                    let slot = (channel as usize & 15) * 128 + key as usize;
                    // A key pressed again before its release: the old note ends first.
                    if self.maps[slot].used {
                        let m = self.maps[slot];
                        end(&mut self.ledger, out, e.offset, abs, channel, m, 0.0);
                    }
                    let shifted = if (lowest..=highest).contains(&(key as i32)) {
                        let k = key as i32 + shift;
                        match mode {
                            _ if (0..=127).contains(&k) => Some(k as u8),
                            0 => None,
                            1 => Some(fold_octaves(k)),
                            _ => Some(k.clamp(0, 127) as u8),
                        }
                    } else if block_outside {
                        None
                    } else {
                        Some(key)
                    };
                    let affected = (lowest..=highest).contains(&(key as i32));
                    let mut map = Map { used: true, key: NONE, id: note_id, copy_key: NONE, copy_id: 0 };
                    if layer && affected {
                        // The played note stays; the shifted one is added.
                        if self.ledger.on(out, e.offset, abs, channel, key, velocity, note_id) {
                            map.key = key;
                        }
                        if let Some(k) = shifted.filter(|k| *k != key) {
                            let id = self.ids.next();
                            if self.ledger.on(out, e.offset, abs, channel, k, (velocity * level).clamp(0.0, 1.0).max(1.0 / 127.0), id) {
                                map.copy_key = k;
                                map.copy_id = id;
                            }
                        }
                    } else if let Some(k) = shifted
                        && self.ledger.on(out, e.offset, abs, channel, k, velocity, note_id)
                    {
                        map.key = k;
                    }
                    self.maps[slot] = map;
                }
                EventKind::NoteOff { channel, key, velocity, note_id } => {
                    let slot = (channel as usize & 15) * 128 + key as usize;
                    if self.maps[slot].used {
                        let m = self.maps[slot];
                        self.maps[slot].used = false;
                        end(&mut self.ledger, out, e.offset, abs, channel, m, velocity);
                    } else {
                        // A note this device never saw begin: let its release through.
                        self.ledger.off(out, e.offset, abs, channel, key, velocity, note_id);
                    }
                }
                EventKind::NoteExpression { note_id, key, expr, value } => {
                    let mapped = self.maps[key as usize & 127].key;
                    let key = if self.maps[key as usize & 127].used && mapped != NONE && !layer { mapped } else { key };
                    out.push(e.offset, EventKind::NoteExpression { note_id, key, expr, value });
                }
                EventKind::AllNotesOff => {
                    out.push(e.offset, e.kind);
                    self.ledger.forget();
                    self.maps.fill(UNUSED);
                }
                kind => {
                    out.push(e.offset, kind);
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
}

#[cfg(test)]
mod tests {
    use super::super::fxtest::{Rig, off, on};
    use super::*;

    fn rig() -> Rig {
        Rig::new(create())
    }

    fn set(r: &mut Rig, id: u32, plain: f32) {
        let n = r.p.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
        r.p.set_param(id, n);
    }

    fn play(r: &mut Rig, key: u8) {
        r.run(&[(0, on(key, 0.8)), (100, off(key))], 200, 64);
    }

    #[test]
    fn octaves_and_semitones_add_up() {
        let mut r = rig();
        set(&mut r, p::OCTAVE, 1.0);
        set(&mut r, p::SEMITONE, 2.0);
        play(&mut r, 60);
        assert_eq!(r.keys(), vec![74]);
        r.assert_balanced();
        let mut r = rig();
        set(&mut r, p::OCTAVE, -2.0);
        set(&mut r, p::SEMITONE, -3.0);
        play(&mut r, 60);
        assert_eq!(r.keys(), vec![33]);
    }

    #[test]
    fn default_settings_pass_notes_through_unchanged() {
        let mut r = rig();
        play(&mut r, 61);
        assert_eq!(r.notes(), vec![(0, true, 61, 0.8), (100, false, 61, 0.0)]);
    }

    #[test]
    fn the_release_follows_the_shift_the_note_began_with() {
        let mut r = rig();
        set(&mut r, p::SEMITONE, 5.0);
        r.run(&[(0, on(60, 0.8))], 100, 100);
        set(&mut r, p::SEMITONE, -7.0);
        r.run(&[(0, off(60))], 100, 100);
        assert_eq!(r.notes().iter().map(|n| (n.1, n.2)).collect::<Vec<_>>(), vec![(true, 65), (false, 65)]);
    }

    #[test]
    fn only_the_keys_in_range_move() {
        let mut r = rig();
        set(&mut r, p::SEMITONE, 12.0);
        set(&mut r, p::LOWEST, 48.0);
        set(&mut r, p::WIDTH, 12.0);
        for key in [47u8, 48, 59, 60] {
            play(&mut r, key);
        }
        assert_eq!(r.keys(), vec![47, 60, 71, 60]);
        // Outside = Block drops them, and their releases.
        let mut r = rig();
        set(&mut r, p::SEMITONE, 12.0);
        set(&mut r, p::LOWEST, 48.0);
        set(&mut r, p::WIDTH, 12.0);
        set(&mut r, p::OUTSIDE, outside::BLOCK);
        for key in [47u8, 48, 59, 60] {
            play(&mut r, key);
        }
        assert_eq!(r.keys(), vec![60, 71]);
        assert_eq!(r.notes().len(), 4);
        r.assert_balanced();
    }

    #[test]
    fn overflow_modes() {
        // 124 + 12 = 136 is out of range.
        for (mode, want) in [(overflow::BLOCK, None), (overflow::FOLD, Some(124)), (overflow::LIMIT, Some(127))] {
            let mut r = rig();
            set(&mut r, p::OCTAVE, 1.0);
            set(&mut r, p::OVERFLOW, mode);
            play(&mut r, 124);
            assert_eq!(r.keys().first().copied(), want, "mode {mode}");
            assert_eq!(r.notes().len(), if want.is_some() { 2 } else { 0 });
            r.assert_balanced();
        }
        // Below zero folds up by octaves, keeping the pitch class: 3 - 12 = -9 -> 3.
        let mut r = rig();
        set(&mut r, p::OCTAVE, -1.0);
        play(&mut r, 3);
        assert_eq!(r.keys(), vec![3]);
        let mut r = rig();
        set(&mut r, p::OCTAVE, -4.0);
        set(&mut r, p::SEMITONE, -1.0);
        play(&mut r, 40);
        assert_eq!(r.keys(), vec![3], "40 - 49 = -9 folds to 3");
    }

    #[test]
    fn a_layer_keeps_the_played_note_and_adds_the_shifted_one() {
        let mut r = rig();
        set(&mut r, p::LAYER, 1.0);
        set(&mut r, p::OCTAVE, 1.0);
        set(&mut r, p::LAYER_LEVEL, 50.0);
        play(&mut r, 48);
        let ons: Vec<_> = r.notes().into_iter().filter(|n| n.1).collect();
        assert_eq!(ons.iter().map(|n| n.2).collect::<Vec<_>>(), vec![48, 60]);
        assert_eq!(ons[0].3, 0.8);
        assert!((ons[1].3 - 0.4).abs() < 1e-6);
        r.assert_balanced();
        // The two notes have different ids.
        let ids: Vec<i32> = r.log.iter().filter_map(|(_, k)| if let EventKind::NoteOn { note_id, .. } = k { Some(*note_id) } else { None }).collect();
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn a_layer_with_no_shift_does_not_double_the_note() {
        let mut r = rig();
        set(&mut r, p::LAYER, 1.0);
        play(&mut r, 48);
        assert_eq!(r.ons().len(), 1);
    }

    #[test]
    fn colliding_layers_stay_paired_and_never_stack_on_a_key() {
        let mut r = rig();
        set(&mut r, p::LAYER, 1.0);
        set(&mut r, p::SEMITONE, 7.0);
        // C and G together: C's copy lands on G.
        r.run(&[(0, on(60, 0.8)), (0, on(67, 0.8)), (3_000, off(60)), (3_500, off(67))], 6_000, 256);
        r.assert_balanced();
    }

    #[test]
    fn note_expression_follows_the_shifted_key() {
        let mut r = rig();
        set(&mut r, p::SEMITONE, 3.0);
        let expr = EventKind::NoteExpression { note_id: 60, key: 60, expr: aitrack_dsp::event::Expression::Tuning, value: 0.5 };
        r.run(&[(0, on(60, 0.8)), (10, expr), (20, off(60))], 100, 100);
        let got = r.log.iter().find_map(|(_, k)| if let EventKind::NoteExpression { key, .. } = k { Some(*key) } else { None });
        assert_eq!(got, Some(63));
    }

    #[test]
    fn all_notes_off_and_reset_forget_the_notes() {
        let mut r = rig();
        set(&mut r, p::SEMITONE, 2.0);
        r.run(&[(0, on(60, 0.8)), (10, EventKind::AllNotesOff), (30, on(60, 0.8)), (40, off(60))], 100, 100);
        assert_eq!(r.keys(), vec![62, 62]);
        r.assert_balanced();
        r.run(&[(0, on(64, 0.8))], 50, 50);
        r.p.reset();
        r.run(&[(0, off(64))], 50, 50);
        // After a reset the release is an unknown note and passes as it came.
        assert_eq!(r.notes().last().map(|n| (n.1, n.2)), Some((false, 64)));
    }

    #[test]
    fn same_notes_for_any_block_size_and_random_settings_stay_paired() {
        let mut seed = 99u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..40 {
            let settings: Vec<(u32, f32)> = (0..8).map(|id| (id, (rnd() % 1001) as f32 / 1000.0)).collect();
            let mut input = Vec::new();
            let mut free_at = [0i64; 128];
            let mut t = 0;
            for _ in 0..50 {
                t += (rnd() % 400) as i64;
                let key = (rnd() % 128) as u8;
                if t <= free_at[key as usize] {
                    continue;
                }
                let len = 2 + (rnd() % 3_000) as i64;
                free_at[key as usize] = t + len;
                input.push((t, on(key, 0.1 + (rnd() % 90) as f32 / 100.0)));
                input.push((t + len, off(key)));
            }
            input.sort_by_key(|e| e.0);
            let mut want = None;
            for block in [1usize, 17, 4096] {
                let mut r = rig();
                for (id, v) in &settings {
                    r.p.set_param(*id, *v);
                }
                r.run(&input, t + 4_000, block);
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
