//! `midi.chord`: one played key (C4 here) and the notes the device adds to
//! it. Each voice is a marker over a small keyboard: its place is the
//! interval, its height the level. Drag a marker sideways for the interval
//! and up or down for the level, double-click it to reset it, double-click a
//! key to add a voice there or remove the one on it. Under the keyboard a
//! row of six cells holds the same voices as switches and numbers. With Fit
//! to Scale the keys the voices really sound are lit, and a voice the scale
//! moved shows where it came from.
//!
//! Under the six cells, cards for the **strum** (a small picture of when each
//! note of the chord starts, drawn with the device's own functions), the
//! **chance** seed and loop, and **Learn**: with Learn on the device listens
//! to the keys played and publishes the chord through its meter; this editor
//! then writes it into the voices in one undo step and switches Learn off. The
//! rest of the picture is of the settings, not of notes being played.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::midi_kit as mk;
use super::{Dev, ParamSet, set_many};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, Project};
use aitrack_stock::midi::chord::{VOICES, learn_edits, meter, p, strum_gain, strum_position};
use aitrack_stock::midi::theory::{Snap, scale_mask, snap_note};
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, TextAlign, div, prelude::FluentBuilder, px};
use std::rc::Rc;

/// The keyboard shown (C1 to C7 would be too dense: the intervals reach ±36).
pub const LO: i32 = 24;
pub const HI: i32 = 96;
/// The key the picture "plays" (C4).
pub const PLAYED: i32 = 60;
/// Share of the plot height the level markers use, and where the keyboard starts.
const LANE_END: f32 = 0.56;
const KEYS_START: f32 = 0.62;
const BLACK_H: f32 = 0.62;

/// Where the parts of the plot sit, as fractions of its height: the level
/// markers above `lane_end`, the keyboard from `keys_start` to `keys_end`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub lane_end: f32,
    pub keys_start: f32,
    pub keys_end: f32,
}

impl Geo {
    pub fn of(window: bool) -> Self {
        if window { Self { lane_end: 0.5, keys_start: 0.56, keys_end: 1. } } else { Self { lane_end: LANE_END, keys_start: KEYS_START, keys_end: 1. } }
    }
}

/// The name of the chord the pitch classes `mask` (bit 0 = C) make, with
/// `played` (a pitch class) as its root when it fits, else any note that
/// does: "Cmaj7", "Am", "Gsus4". `None` when it is not a chord this knows.
pub fn chord_name(mask: u16, played: usize) -> Option<String> {
    // Intervals from the root and the suffix they make.
    const KINDS: &[(&[u8], &str)] = &[
        (&[0, 4, 7], ""),
        (&[0, 3, 7], "m"),
        (&[0, 3, 6], "dim"),
        (&[0, 4, 8], "aug"),
        (&[0, 2, 7], "sus2"),
        (&[0, 5, 7], "sus4"),
        (&[0, 7], "5"),
        (&[0, 4, 7, 11], "maj7"),
        (&[0, 3, 7, 11], "mMaj7"),
        (&[0, 4, 7, 10], "7"),
        (&[0, 3, 7, 10], "m7"),
        (&[0, 3, 6, 10], "m7b5"),
        (&[0, 3, 6, 9], "dim7"),
        (&[0, 4, 7, 9], "6"),
        (&[0, 3, 7, 9], "m6"),
        (&[0, 5, 7, 10], "7sus4"),
        (&[0, 2, 4, 7], "add9"),
        (&[0, 2, 4, 7, 10], "9"),
        (&[0, 2, 4, 7, 11], "maj9"),
        (&[0, 2, 3, 7, 10], "m9"),
    ];
    let mask = mask & 0x0FFF;
    let rot = |root: usize| ((mask >> root) | (mask << (12 - root))) & 0x0FFF;
    let played = played % 12;
    let roots = std::iter::once(played).chain((0..12).filter(|r| *r != played));
    for root in roots.filter(|r| mask & (1 << r) != 0) {
        let m = rot(root);
        if let Some((_, suffix)) = KINDS.iter().find(|(iv, _)| iv.iter().fold(0u16, |a, i| a | 1 << i) == m) {
            return Some(format!("{}{}", ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][root], suffix));
        }
    }
    None
}

// ───────────────────────────── A keyboard ─────────────────────────────

/// One key of a drawn keyboard, as fractions of its width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyBox {
    pub key: i32,
    pub x: f32,
    pub w: f32,
    pub black: bool,
}

pub fn is_black(key: i32) -> bool {
    matches!(key.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// The keys from `lo` to `hi` inclusive laid out over a width of 1: white
/// keys side by side, black ones centred on the line between their
/// neighbours. `lo` and `hi` should be white keys.
pub fn piano(lo: i32, hi: i32) -> Vec<KeyBox> {
    let whites = (lo..=hi).filter(|k| !is_black(*k)).count().max(1);
    let ww = 1. / whites as f32;
    let mut out = Vec::new();
    let mut seen = 0usize;
    for key in lo..=hi {
        if is_black(key) {
            let bw = ww * 0.6;
            out.push(KeyBox { key, x: seen as f32 * ww - bw / 2., w: bw, black: true });
        } else {
            out.push(KeyBox { key, x: seen as f32 * ww, w: ww, black: false });
            seen += 1;
        }
    }
    out
}

/// The key under a point of the keyboard (`fx`, `fy` as fractions of it): black keys
/// sit on top, in the upper part.
pub fn key_at(boxes: &[KeyBox], fx: f32, fy: f32) -> Option<i32> {
    let hit = |black: bool| boxes.iter().find(|b| b.black == black && fx >= b.x && fx < b.x + b.w).map(|b| b.key);
    if fy <= BLACK_H { hit(true).or_else(|| hit(false)) } else { hit(false) }
}

/// Fraction of the keyboard's width at the centre of `key`.
pub fn key_center(boxes: &[KeyBox], key: i32) -> Option<f32> {
    boxes.iter().find(|b| b.key == key).map(|b| b.x + b.w / 2.)
}

/// Paint the keyboard in `r`; `tint` gives a key a fill over its own.
pub fn paint_piano(window: &mut gpui::Window, r: Rect, boxes: &[KeyBox], tint: impl Fn(i32) -> Option<gpui::Hsla>) {
    let pal = p();
    for pass in [false, true] {
        for b in boxes.iter().filter(|b| b.black == pass) {
            let (x, w) = (r.x + r.w * b.x, r.w * b.w);
            let h = if b.black { r.h * BLACK_H } else { r.h };
            let base = if b.black { pal.well } else { content(0.2) };
            kit::bar(window, x + 0.5, r.y, (w - 1.).max(1.), h, 2., base);
            if let Some(c) = tint(b.key) {
                kit::bar(window, x + 0.5, r.y, (w - 1.).max(1.), h, 2., c);
            }
        }
    }
}

// ───────────────────────────── The voices ─────────────────────────────

/// A voice as the device has it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Voice {
    pub on: bool,
    pub interval: i32,
    /// Percent.
    pub level: f32,
    /// Percent: how often the voice sounds.
    pub chance: f32,
}

/// The six voices and where they play for a key, `fit` moving them into the
/// scale (a voice at interval 0 is the played key and stays).
pub fn sounding(voices: &[Voice; VOICES], key: i32, fit: Option<u16>) -> Vec<(usize, i32)> {
    voices
        .iter()
        .enumerate()
        .filter(|(_, v)| v.on && v.level > 0.)
        .map(|(i, v)| {
            let raw = (key + v.interval).clamp(0, 127);
            let out = match fit {
                Some(mask) if v.interval != 0 => i32::from(snap_note(mask, raw as u8, Snap::Nearest, false)),
                _ => raw,
            };
            (i, out)
        })
        .collect()
}

pub fn read_voices(dev: &Dev) -> [Voice; VOICES] {
    std::array::from_fn(|v| Voice {
        on: dev.on(p::on(v)),
        interval: dev.plain(p::interval(v)).round() as i32,
        level: dev.plain(p::level(v)),
        chance: dev.plain(p::chance(v)),
    })
}

/// How solid a voice is drawn: 1 for a voice that always sounds, down to 0.4
/// for one that never does.
pub fn sureness(chance_percent: f32) -> f32 {
    0.4 + 0.6 * (chance_percent / 100.).clamp(0., 1.)
}

/// What a double-click on `key` does: remove the voice that sounds there,
/// else switch the first free voice on at that interval (level and chance
/// 100 %). `None` when every voice is taken. Returns plain edits.
pub fn toggle_voice(voices: &[Voice; VOICES], key: i32) -> Option<Vec<(u32, f32)>> {
    let interval = key - PLAYED;
    if let Some(v) = voices.iter().position(|v| v.on && v.interval == interval) {
        return Some(vec![(p::on(v), 0.)]);
    }
    let free = voices.iter().position(|v| !v.on)?;
    Some(vec![(p::on(free), 1.), (p::interval(free), interval as f32), (p::level(free), 100.), (p::chance(free), 100.)])
}

/// The mask of the device's scale.
pub fn scale_of(dev: &Dev) -> u16 {
    scale_mask(dev.choice(p::SCALE).min(12), dev.choice(p::ROOT).min(11))
}

/// Follow Project: when the switch is on and the project's scale is not what
/// Root and Scale hold, write it there (one undo step). The device itself
/// never reads the project.
pub fn follow_project(ws: &mut Workspace, dev: &Dev, root: u32, scale: u32, follow: u32) {
    if !dev.on(follow) {
        return;
    }
    let Some(edits) = follow_edits(&ws.s.project, dev, root, scale) else { return };
    set_many(ws, "Follow project scale", dev.id, &edits);
}

/// The Root and Scale edits (normalised) that make the device follow the project, if it does not yet.
pub fn follow_edits(project: &Project, dev: &Dev, root: u32, scale: u32) -> Option<Vec<(u32, f32)>> {
    let s = project.scale?;
    let kind = aitrack_model::ScaleKind::ALL.iter().position(|k| *k == s.kind)?;
    let (r, k) = (usize::from(s.root.0 % 12), kind);
    if dev.choice(root) == r && dev.choice(scale) == k {
        return None;
    }
    Some(vec![(root, dev.info(root)?.to_normalized(r as f32)), (scale, dev.info(scale)?.to_normalized(k as f32))])
}

// ───────────────────────────── The strum ─────────────────────────────

/// One note of the strum's picture: its key, where along the strum it
/// starts (0…1 of its time) and its velocity factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrumNote {
    pub key: i32,
    pub at: f32,
    pub gain: f32,
}

/// The notes of `heard` (what [`sounding`] says the played key makes) in the
/// order the strum plays them, placed and tilted by the device's own
/// [`strum_position`] and [`strum_gain`]. `direction` is the device's *Strum
/// Direction*: Down-Up is drawn by its downstroke and Random from the bottom
/// up, since the device decides those from the song position. Two voices on
/// one key are one note.
pub fn strum_notes(heard: &[(usize, i32)], direction: usize, tension: f32, ramp: f32) -> Vec<StrumNote> {
    let mut keys: Vec<i32> = heard.iter().map(|(_, k)| *k).collect();
    keys.sort_unstable();
    keys.dedup();
    if matches!(direction, 1 | 2) {
        keys.reverse();
    }
    let n = keys.len();
    keys.into_iter().enumerate().map(|(i, key)| StrumNote { key, at: strum_position(i, n, f64::from(tension)) as f32, gain: strum_gain(i, n, ramp) }).collect()
}

/// Width and height of the strum picture, and the width of one note in it.
const PICTURE: (f32, f32) = (176., 48.);
const NOTE_W: f32 = 16.;

/// The strum card: when each note of the chord starts, and what shapes it.
fn strum_card(ws: &mut Workspace, dev: &Dev, notes: Vec<StrumNote>, cx: &mut Context<Workspace>) -> gpui::Div {
    let (enabled, ms, direction) = (dev.enabled, dev.plain(p::STRUM), dev.choice(p::STRUM_DIR));
    let picture = kit::plot(ws, dev, PlotCfg { slot: 1, ..Default::default() }, cx, move |r, window, _| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        let (pad, rows) = (SP2, VOICES as f32);
        let row_h = (r.h - 2. * pad) / rows;
        // The start and the end of the strum.
        for x in [r.x + pad, r.right() - pad] {
            kit::bar(window, x, r.y + pad / 2., 1., r.h - pad, 0., pal.grid_beat);
        }
        for n in &notes {
            // The lowest note sits at the bottom, whatever order it is played in.
            let row = notes.iter().filter(|o| o.key < n.key).count();
            let x = r.x + pad + n.at * (r.w - 2. * pad - NOTE_W);
            let y = r.bottom() - pad - (row as f32 + 1.) * row_h + 1.;
            kit::bar(window, x, y, NOTE_W, (row_h - 2.).max(2.), 2., with_alpha(colour, (0.3 + 0.5 * n.gain / 1.5).clamp(0.2, 0.95)));
        }
    });
    let caption = if ms <= 0. {
        "all at once".to_owned()
    } else {
        let order = ["up", "down", "down on the beat, up between", "random order"][direction.min(3)];
        format!("{} · {order}", mk::fmt_ms(ms))
    };
    let pal = p();
    let shape = div().flex_none().flex().flex_col().gap(px(SP1)).child(div().flex_none().w(px(PICTURE.0)).h(px(PICTURE.1)).child(picture)).child(label(caption, TEXT_XS, pal.faint));
    kit::section(dev, "STRUM", None, vec![shape.into_any_element(), kit::num(ws, dev, p::STRUM, cx), kit::pick(dev, p::STRUM_DIR, cx), kit::num(ws, dev, p::STRUM_TENSION, cx), kit::num(ws, dev, p::STRUM_VEL, cx)], cx)
}

// ───────────────────────────── Learn ─────────────────────────────

/// The serial of the last chord the device learned, once it publishes one.
pub fn serial_of(dev: &Dev) -> Option<f32> {
    dev.meter.get(meter::SERIAL).copied()
}

/// The voices the chord in the device's meter becomes (normalised edits, with
/// Learn back to Off); empty when it has learned nothing.
pub fn learned_voices(dev: &Dev) -> Vec<(u32, f32)> {
    let n = (dev.m(meter::COUNT).round().max(0.) as usize).min(VOICES);
    let keys: Vec<u8> = (0..n).map(|i| dev.m(meter::KEYS + i).round().clamp(0., 127.) as u8).collect();
    learn_edits(&keys).into_iter().filter_map(|(id, v)| dev.info(id).map(|i| (id, i.to_normalized(v)))).collect()
}

/// The slot of the view options that remembers the serial last looked at.
const LEARN_SEEN: u8 = 7;

/// A chord the device heard since this editor last looked, while Learn is
/// on, goes into the voices (one undo step) and Learn is switched off. The
/// first look only takes note of the serial.
pub fn apply_learned(ws: &mut Workspace, dev: &Dev) {
    let Some(serial) = serial_of(dev) else { return };
    let seen = ws.fx.opts.insert((dev.id, LEARN_SEEN), serial);
    if dev.on(p::LEARN) {
        // Frames keep coming while the device listens, so the chord is seen the moment it is played.
        ws.fx.keep_awake();
        if seen.is_some_and(|s| s != serial) {
            let edits = learned_voices(dev);
            if !edits.is_empty() {
                set_many(ws, "Learn chord", dev.id, &edits);
            }
        }
    }
}

/// The Learn card: the switch, and what the device is waiting for.
fn learn_card(dev: &Dev, cx: &mut Context<Workspace>) -> gpui::Div {
    let pal = p();
    let learning = dev.on(p::LEARN);
    let held = dev.m(meter::LIVE).round() as usize;
    let status = match (learning, held) {
        (false, _) => "play a chord and keep what you hear".to_owned(),
        (true, 0) => "listening: play the chord on your keyboard".to_owned(),
        (true, n) => format!("listening: {n} key{} so far", if n == 1 { "" } else { "s" }),
    };
    let body = vec![kit::pick(dev, p::LEARN, cx), label(status, TEXT_XS, if learning { pal.accent } else { pal.faint }).into_any_element()];
    kit::section(dev, "LEARN", None, body, cx)
}

fn note_name(key: i32) -> String {
    format!("{}{}", ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][key.rem_euclid(12) as usize], key.div_euclid(12) - 1)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    follow_project(ws, dev, p::ROOT, p::SCALE, p::FOLLOW_PROJECT);
    apply_learned(ws, dev);
    let voices = read_voices(dev);
    let fit = dev.on(p::FIT).then(|| scale_of(dev));
    let enabled = dev.enabled;
    let boxes = piano(LO, HI);
    let out = sounding(&voices, PLAYED, fit);
    let strummed = strum_notes(&out, dev.choice(p::STRUM_DIR), dev.plain(p::STRUM_TENSION) / 100., dev.plain(p::STRUM_VEL) / 100.);
    let win = mk::windowed(dev);
    let geo = Geo::of(win);
    // What comes out, named: the notes that sound and the chord they make.
    let mut heard: Vec<i32> = out.iter().map(|(_, k)| *k).collect();
    heard.sort_unstable();
    heard.dedup();
    let mask = heard.iter().fold(0u16, |m, k| m | 1 << k.rem_euclid(12));
    let summary = match chord_name(mask, PLAYED.rem_euclid(12) as usize) {
        Some(name) => format!("chord {name} · {}", heard.iter().map(|k| note_name(*k)).collect::<Vec<_>>().join(" ")),
        None => heard.iter().map(|k| note_name(*k)).collect::<Vec<_>>().join(" "),
    };
    let names: Vec<(usize, String)> = out.iter().map(|(i, k)| (*i, note_name(*k))).collect();
    let names_of = names.clone();
    let span = (HI - LO) as f32;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));

    // A marker per voice that is on: place = interval, height = level.
    let mut handles = Vec::new();
    for (v, voice) in voices.iter().enumerate().filter(|(_, v)| v.on) {
        let raw = (PLAYED + voice.interval).clamp(LO, HI);
        let Some(fx) = key_center(&boxes, raw) else { continue };
        let fy = geo.lane_end - (geo.lane_end - 0.08) * (voice.level / 200.).clamp(0., 1.);
        handles.push(Handle {
            fx,
            fy,
            x: ax(p::interval(v), span / 72.),
            y: ax(p::level(v), -1. / (geo.lane_end - 0.08)),
            index: v,
            reset: vec![default(p::interval(v)), default(p::level(v))],
            ..Default::default()
        });
    }
    let marks: Vec<(usize, f32, f32)> = handles.iter().map(|h| (h.index, h.fx, h.fy)).collect();

    let on_empty: kit::EmptyFn = {
        let (device, set): (DeviceId, Rc<ParamSet>) = (dev.id, dev.set.clone());
        Rc::new(move |this, fx, fy, cx| {
            if fy < geo.keys_start || fy > geo.keys_end {
                return;
            }
            let rel = (fy - geo.keys_start) / (geo.keys_end - geo.keys_start);
            let Some(key) = key_at(&piano(LO, HI), fx, rel) else { return };
            if let Some(edits) = toggle_voice(&voices, key) {
                let norms: Vec<(u32, f32)> = edits.iter().filter_map(|(id, v)| set.get(*id).map(|i| (*id, i.to_normalized(*v)))).collect();
                set_many(this, "Edit chord", device, &norms);
            }
            cx.notify();
        })
    };

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, on_empty: Some(on_empty), ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        let keys = Rect { x: r.x + 4., y: r.py(geo.keys_start), w: r.w - 8., h: r.h * (geo.keys_end - geo.keys_start) - 4. };
        // The scale (when the chord is fitted) and the notes that sound.
        let tint = |key: i32| {
            if let Some((i, _)) = out.iter().find(|(_, k)| *k == key) {
                // A voice that does not always sound is drawn fainter.
                Some(with_alpha(colour, (0.35 + 0.5 * (voices[*i].level / 200.).clamp(0., 1.)) * sureness(voices[*i].chance)))
            } else if key == PLAYED {
                Some(accent_alpha(0.3))
            } else {
                fit.filter(|m| m & (1 << key.rem_euclid(12)) != 0).map(|_| content(0.12))
            }
        };
        paint_piano(window, keys, &boxes, tint);
        kit::hline(window, r, r.py(geo.lane_end), pal.grid_bar);
        for q in [0.25, 0.5, 0.75, 1.] {
            kit::hline(window, r, r.py(geo.lane_end - (geo.lane_end - 0.08) * q), pal.grid_beat);
            if win && q == 0.5 {
                kit::txt(window, cx, "100%", r.right() - 6., r.py(geo.lane_end - (geo.lane_end - 0.08) * q) - 12., TEXT_SM, pal.faint, TextAlign::Right);
            }
        }
        // The played key: a marker over its key.
        if let Some(c) = key_center(&boxes, PLAYED) {
            let x = keys.x + keys.w * c;
            kit::bar(window, x - 1., r.py(geo.lane_end), 2., keys.y - r.py(geo.lane_end), 1., accent_alpha(0.6));
            kit::txt(window, cx, &format!("played {}", note_name(PLAYED)), x - 6., r.py(geo.lane_end) + 2., TEXT_SM, pal.faint, TextAlign::Right);
        }
        // Each voice's marker, and an arrow from where the scale moved it.
        for (v, fx, fy) in &marks {
            let x = keys.x + keys.w * *fx;
            let y = r.py(*fy);
            kit::bar(window, x - 1., y, 2., r.py(geo.lane_end) - y, 1., with_alpha(colour, 0.7));
            kit::dot(window, gpui::point(px(x), px(y)), 6., with_alpha(colour, sureness(voices[*v].chance)), Some(pal.text));
            let text = if win { names.iter().find(|(i, _)| i == v).map_or_else(|| format!("{}", v + 1), |(_, n)| n.clone()) } else { format!("{}", v + 1) };
            kit::txt(window, cx, &text, x, y - 16., TEXT_SM, pal.text, TextAlign::Center);
        }
        for (i, key) in &out {
            let raw = (PLAYED + voices[*i].interval).clamp(0, 127);
            if raw != *key
                && let (Some(a), Some(b)) = (key_center(&boxes, raw), key_center(&boxes, *key))
            {
                let y = keys.y - 6.;
                kit::line(window, &[gpui::point(px(keys.x + keys.w * a), px(y)), gpui::point(px(keys.x + keys.w * b), px(y))], 1.5, with_alpha(pal.accent2, 0.9));
            }
        }
        kit::txt(window, cx, "double-click a key to add or remove a voice", r.x + 6., r.y + 4., TEXT_SM, pal.faint, TextAlign::Left);
        if win {
            kit::txt(window, cx, &format!("{} → {}", note_name(PLAYED), summary), r.right() - 6., r.y + 4., TEXT_SM, pal.muted, TextAlign::Right);
            kit::txt(window, cx, "level of each voice", r.x + 6., r.py(geo.lane_end) - 14., TEXT_SM, pal.faint, TextAlign::Left);
        }
    });

    let mut cells = div().flex_none().flex().gap(px(SP1));
    for v in 0..VOICES {
        let sounds = names_of.iter().find(|(i, _)| *i == v).map(|(_, n)| n.clone());
        cells = cells.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .p(px(SP1))
                .gap(px(SP1))
                .flex()
                .flex_col()
                .rounded(px(r_sm()))
                .bg(content(0.03))
                .when(win, |d| d.child(mono(sounds.unwrap_or_else(|| "off".to_owned()), TEXT_SM, if dev.on(p::on(v)) { p().text } else { p().faint })))
                .child(kit::pick(dev, p::on(v), cx))
                .child(kit::num_w(ws, dev, p::interval(v), 68., cx))
                .child(kit::num_w(ws, dev, p::level(v), 68., cx))
                .child(kit::num_w(ws, dev, p::chance(v), 68., cx)),
        );
    }
    let strum = strum_card(ws, dev, strummed, cx);
    let chance = kit::section(dev, "CHANCE", None, vec![kit::num(ws, dev, p::SEED, cx), kit::pick(dev, p::LOOP, cx)], cx);
    let learn = learn_card(dev, cx);
    let scale = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(kit::pick(dev, p::FIT, cx))
        .child(kit::pick(dev, p::ROOT, cx))
        .child(kit::pick(dev, p::SCALE, cx))
        .child(kit::pick(dev, p::FOLLOW_PROJECT, cx));

    let frame = div().id(("chord-plot", dev.key(0))).size_full().rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(plot);
    super::midi_kit::body_with(dev, "chord-body", 520., 108., 184., frame, vec![cells, scale, strum, chance, learn])
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn a_chord_is_named_from_its_notes_with_the_played_key_as_the_root() {
        let mask = |keys: &[i32]| keys.iter().fold(0u16, |m, k| m | 1 << k.rem_euclid(12));
        assert_eq!(chord_name(mask(&[60, 64, 67, 71]), 0).as_deref(), Some("Cmaj7"));
        assert_eq!(chord_name(mask(&[60, 63, 67]), 0).as_deref(), Some("Cm"));
        assert_eq!(chord_name(mask(&[60, 64, 67, 69]), 0).as_deref(), Some("C6"), "the played key wins over Am7");
        assert_eq!(chord_name(mask(&[60, 64, 67, 69]), 9).as_deref(), Some("Am7"));
        assert_eq!(chord_name(mask(&[60, 62, 67]), 0).as_deref(), Some("Csus2"));
        assert_eq!(chord_name(mask(&[64, 67, 72]), 4).as_deref(), Some("C"), "an inversion is still C");
        assert_eq!(chord_name(mask(&[60, 61, 62]), 0), None);
        assert_eq!(chord_name(0, 0), None);
    }

    #[test]
    fn a_window_gives_the_keyboard_the_lower_half() {
        let (card, window) = (Geo::of(false), Geo::of(true));
        assert_eq!((card.keys_start, card.keys_end), (KEYS_START, 1.));
        assert!(window.keys_end - window.keys_start > card.keys_end - card.keys_start - 0.1);
        assert!(window.lane_end < window.keys_start && card.lane_end < card.keys_start);
    }

    #[test]
    fn the_keyboard_has_white_keys_side_by_side_and_black_keys_between() {
        let k = piano(60, 72);
        assert_eq!(k.len(), 13);
        let whites: Vec<&KeyBox> = k.iter().filter(|b| !b.black).collect();
        assert_eq!(whites.len(), 8);
        assert!((whites[7].x + whites[7].w - 1.).abs() < 1e-5, "the whites fill the width");
        let cs = k.iter().find(|b| b.key == 61).unwrap();
        let (c, d) = (&whites[0], &whites[1]);
        assert!(cs.black && (cs.x + cs.w / 2. - d.x).abs() < 1e-5 && cs.x > c.x);
        assert!(k.iter().all(|b| b.key != 63 || b.black));
    }

    #[test]
    fn a_point_finds_its_key_black_keys_first_in_the_upper_part() {
        let k = piano(60, 72);
        let cs = k.iter().find(|b| b.key == 61).unwrap();
        let mid = cs.x + cs.w / 2.;
        assert_eq!(key_at(&k, mid, 0.2), Some(61));
        assert_eq!(key_at(&k, mid - 0.01, 0.9), Some(60), "below the black key it is a white one");
        assert_eq!(key_at(&k, 0.01, 0.9), Some(60));
        assert!(key_center(&k, 62).unwrap() > key_center(&k, 60).unwrap());
        assert_eq!(key_center(&k, 10), None);
    }

    #[test]
    fn default_voices_are_a_major_triad_and_fit_moves_the_third() {
        let d = dev("midi.chord", &[]);
        let v = read_voices(&d);
        assert_eq!(sounding(&v, 60, None), vec![(0, 60), (1, 64), (2, 67)]);
        // In C minor the major third becomes the minor third, the played key stays.
        let minor = scale_mask(1, 0);
        assert_eq!(sounding(&v, 60, Some(minor)), vec![(0, 60), (1, 63), (2, 67)]);
        // A muted voice does not sound.
        let mut quiet = v;
        quiet[2].level = 0.;
        assert_eq!(sounding(&quiet, 60, None).len(), 2);
    }

    #[test]
    fn a_double_click_adds_a_voice_on_a_free_slot_or_removes_the_one_there() {
        let d = dev("midi.chord", &[]);
        let v = read_voices(&d);
        // Voice 4 is the first free one.
        let add = toggle_voice(&v, 71).unwrap();
        assert_eq!(add, vec![(p::on(3), 1.), (p::interval(3), 11.), (p::level(3), 100.), (p::chance(3), 100.)]);
        // The major third is voice 2: clicking it removes it.
        assert_eq!(toggle_voice(&v, 64), Some(vec![(p::on(1), 0.)]));
        // Every voice on: nothing to add.
        let mut full = v;
        for x in &mut full {
            x.on = true;
            x.interval += 100;
        }
        assert_eq!(toggle_voice(&full, 61), None);
    }

    #[test]
    fn the_strum_picture_orders_the_notes_by_pitch_and_places_them_with_the_devices_own_math() {
        let d = dev("midi.chord", &[]);
        let heard = sounding(&read_voices(&d), 60, None);
        let up = strum_notes(&heard, 0, 0., 0.);
        assert_eq!(up.iter().map(|n| n.key).collect::<Vec<_>>(), vec![60, 64, 67]);
        assert_eq!(up.iter().map(|n| n.at).collect::<Vec<_>>(), vec![0., 0.5, 1.]);
        assert!(up.iter().all(|n| n.gain == 1.), "no tilt: every note at its own velocity");
        let down = strum_notes(&heard, 1, 0., 0.);
        assert_eq!(down.iter().map(|n| n.key).collect::<Vec<_>>(), vec![67, 64, 60]);
        // Down-Up is drawn by its downstroke, Random from the bottom up (the order is the device's to roll).
        assert_eq!(strum_notes(&heard, 2, 0., 0.), down);
        assert_eq!(strum_notes(&heard, 3, 0., 0.), up);
        // Tension and the velocity tilt come from the device's functions.
        let bent = strum_notes(&heard, 0, 1., 1.);
        assert!(bent[1].at < 0.1 && (bent[0].gain, bent[2].gain) == (0.5, 1.5), "{bent:?}");
        // Two voices that landed on one key are one note; nothing sounding is an empty picture.
        assert_eq!(strum_notes(&[(0, 60), (1, 60), (2, 67)], 0, 0., 0.).len(), 2);
        assert!(strum_notes(&[], 0, 0., 0.).is_empty());
    }

    #[test]
    fn the_strum_picture_is_what_the_device_plays() {
        use aitrack_dsp::{Event, EventSink, MidiContext, Transport};
        let sr = 48_000.0;
        let set = [(p::STRUM, 100.), (p::STRUM_TENSION, 40.), (p::STRUM_VEL, 30.), (p::STRUM_DIR, 1.), (p::on(3), 1.), (p::interval(3), 11.)];
        let mut device = aitrack_stock::create("midi.chord").unwrap();
        device.prepare(sr, 8_192);
        for (id, v) in set {
            aitrack_stock::test_util::set_plain(device.as_mut(), id, v);
        }
        let events = [Event { offset: 0, kind: aitrack_dsp::EventKind::NoteOn { channel: 0, key: 60, velocity: 0.5, note_id: 1 } }];
        let (mut out, mut dropped) = (Vec::with_capacity(64), 0);
        let ctx = MidiContext { sample_rate: sr, frames: 8_192, events: &events, transport: &Transport::default() };
        device.process_midi(&ctx, &mut EventSink::new(&mut out, &mut dropped));
        let mut played: Vec<(u32, u8, f32)> = out
            .iter()
            .filter_map(|e| match e.kind {
                aitrack_dsp::EventKind::NoteOn { key, velocity, .. } => Some((e.offset, key, velocity)),
                _ => None,
            })
            .collect();
        played.sort_by_key(|n| (n.0, n.1));
        let d = dev("midi.chord", &set);
        let picture = strum_notes(&sounding(&read_voices(&d), 60, None), d.choice(p::STRUM_DIR), d.plain(p::STRUM_TENSION) / 100., d.plain(p::STRUM_VEL) / 100.);
        assert_eq!(played.len(), 4);
        assert_eq!(picture.len(), 4);
        for (n, (offset, key, velocity)) in picture.iter().zip(&played) {
            assert_eq!(n.key, i32::from(*key));
            let at = *offset as f32 / (0.1 * sr as f32);
            assert!((at - n.at).abs() < 1. / 4_800., "{key}: drawn at {}, played at {at}", n.at);
            assert!((velocity - 0.5 * n.gain).abs() < 1e-5, "{key}: drawn {}, played {velocity}", 0.5 * n.gain);
        }
    }

    #[test]
    fn a_chord_the_device_heard_is_read_from_its_meter_and_becomes_normalised_voices() {
        let with_meter = |learn: f32, meter: Vec<f32>| Dev { meter: std::rc::Rc::new(meter), ..dev("midi.chord", &[(p::LEARN, learn)]) };
        assert_eq!(serial_of(&dev("midi.chord", &[])), None, "no meter published yet");
        let d = with_meter(1., vec![3., 3., 60., 63., 67., 0., 0., 0., 0.]);
        assert_eq!(serial_of(&d), Some(3.));
        let edits = learned_voices(&d);
        assert!(edits.iter().all(|(_, v)| (0.0..=1.0).contains(v)), "normalised: {edits:?}");
        let norm = |id: u32, plain: f32| (id, d.to_norm(id, plain));
        for want in [norm(p::on(0), 1.), norm(p::interval(0), 0.), norm(p::interval(1), 3.), norm(p::interval(2), 7.), norm(p::level(2), 100.), norm(p::on(3), 0.), norm(p::LEARN, 0.)] {
            assert!(edits.contains(&want), "{want:?} missing from {edits:?}");
        }
        // A meter that says nothing was learned writes nothing.
        assert!(learned_voices(&with_meter(1., vec![1., 0., 0., 0., 0., 0., 0., 0., 0.])).is_empty());
        // A meter cut short is read as far as it goes.
        assert_eq!(serial_of(&with_meter(1., vec![5.])), Some(5.));
        assert!(learned_voices(&with_meter(1., vec![5.])).is_empty());
    }

    #[test]
    fn following_the_project_writes_its_scale_only_when_it_differs() {
        let mut project = aitrack_model::Project::new("t");
        let d = dev("midi.chord", &[(p::FOLLOW_PROJECT, 1.)]);
        assert_eq!(follow_edits(&project, &d, p::ROOT, p::SCALE), None, "no project scale");
        project.scale = Some(aitrack_model::Scale::new(aitrack_model::Key(9), aitrack_model::ScaleKind::Dorian));
        let edits = follow_edits(&project, &d, p::ROOT, p::SCALE).unwrap();
        assert_eq!(edits.iter().map(|e| e.0).collect::<Vec<_>>(), [p::ROOT, p::SCALE]);
        let same = dev("midi.chord", &[(p::FOLLOW_PROJECT, 1.), (p::ROOT, 9.), (p::SCALE, 4.)]);
        assert_eq!(follow_edits(&project, &same, p::ROOT, p::SCALE), None);
    }
}
