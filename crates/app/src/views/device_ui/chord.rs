//! `midi.chord`: one played key (C4 here) and the notes the device adds to
//! it. Each voice is a marker over a small keyboard: its place is the
//! interval, its height the level. Drag a marker sideways for the interval
//! and up or down for the level, double-click it to reset it, double-click a
//! key to add a voice there or remove the one on it. Under the keyboard a
//! row of six cells holds the same voices as switches and numbers. With Fit
//! to Scale the keys the voices really sound are lit, and a voice the scale
//! moved shows where it came from.
//!
//! The device has no meter, so the picture is of the settings, not of notes
//! being played.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, ParamSet, set_many};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, Project};
use aitrack_stock::midi::chord::{VOICES, p};
use aitrack_stock::midi::theory::{Snap, scale_mask, snap_note};
use gpui::{AnyElement, Context, InteractiveElement, ParentElement, Styled, TextAlign, div, px};
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
    std::array::from_fn(|v| Voice { on: dev.on(p::on(v)), interval: dev.plain(p::interval(v)).round() as i32, level: dev.plain(p::level(v)) })
}

/// What a double-click on `key` does: remove the voice that sounds there,
/// else switch the first free voice on at that interval (level 100 %).
/// `None` when every voice is taken. Returns plain edits.
pub fn toggle_voice(voices: &[Voice; VOICES], key: i32) -> Option<Vec<(u32, f32)>> {
    let interval = key - PLAYED;
    if let Some(v) = voices.iter().position(|v| v.on && v.interval == interval) {
        return Some(vec![(p::on(v), 0.)]);
    }
    let free = voices.iter().position(|v| !v.on)?;
    Some(vec![(p::on(free), 1.), (p::interval(free), interval as f32), (p::level(free), 100.)])
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

fn note_name(key: i32) -> String {
    format!("{}{}", ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][key.rem_euclid(12) as usize], key.div_euclid(12) - 1)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    follow_project(ws, dev, p::ROOT, p::SCALE, p::FOLLOW_PROJECT);
    let voices = read_voices(dev);
    let fit = dev.on(p::FIT).then(|| scale_of(dev));
    let enabled = dev.enabled;
    let boxes = piano(LO, HI);
    let out = sounding(&voices, PLAYED, fit);
    let span = (HI - LO) as f32;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));

    // A marker per voice that is on: place = interval, height = level.
    let mut handles = Vec::new();
    for (v, voice) in voices.iter().enumerate().filter(|(_, v)| v.on) {
        let raw = (PLAYED + voice.interval).clamp(LO, HI);
        let Some(fx) = key_center(&boxes, raw) else { continue };
        let fy = LANE_END - (LANE_END - 0.08) * (voice.level / 200.).clamp(0., 1.);
        handles.push(Handle {
            fx,
            fy,
            x: ax(p::interval(v), span / 72.),
            y: ax(p::level(v), -1. / (LANE_END - 0.08)),
            index: v,
            reset: vec![default(p::interval(v)), default(p::level(v))],
            ..Default::default()
        });
    }
    let marks: Vec<(usize, f32, f32)> = handles.iter().map(|h| (h.index, h.fx, h.fy)).collect();

    let on_empty: kit::EmptyFn = {
        let (device, set): (DeviceId, Rc<ParamSet>) = (dev.id, dev.set.clone());
        Rc::new(move |this, fx, fy, cx| {
            if fy < KEYS_START {
                return;
            }
            let rel = (fy - KEYS_START) / (1. - KEYS_START);
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
        let keys = Rect { x: r.x + 4., y: r.py(KEYS_START), w: r.w - 8., h: r.h * (1. - KEYS_START) - 4. };
        // The scale (when the chord is fitted) and the notes that sound.
        let tint = |key: i32| {
            if let Some((i, _)) = out.iter().find(|(_, k)| *k == key) {
                Some(with_alpha(colour, 0.35 + 0.5 * (voices[*i].level / 200.).clamp(0., 1.)))
            } else if key == PLAYED {
                Some(accent_alpha(0.3))
            } else {
                fit.filter(|m| m & (1 << key.rem_euclid(12)) != 0).map(|_| content(0.12))
            }
        };
        paint_piano(window, keys, &boxes, tint);
        kit::hline(window, r, r.py(LANE_END), pal.grid_bar);
        for q in [0.25, 0.5, 0.75, 1.] {
            kit::hline(window, r, r.py(LANE_END - (LANE_END - 0.08) * q), pal.grid_beat);
        }
        // The played key: a marker over its key.
        if let Some(c) = key_center(&boxes, PLAYED) {
            let x = keys.x + keys.w * c;
            kit::bar(window, x - 1., r.py(LANE_END), 2., keys.y - r.py(LANE_END), 1., accent_alpha(0.6));
            kit::txt(window, cx, &format!("played {}", note_name(PLAYED)), x - 6., r.py(LANE_END) + 2., TEXT_XS, pal.faint, TextAlign::Right);
        }
        // Each voice's marker, and an arrow from where the scale moved it.
        for (v, fx, fy) in &marks {
            let x = keys.x + keys.w * *fx;
            let y = r.py(*fy);
            kit::bar(window, x - 1., y, 2., r.py(LANE_END) - y, 1., with_alpha(colour, 0.7));
            kit::dot(window, gpui::point(px(x), px(y)), 6., colour, Some(pal.text));
            kit::txt(window, cx, &format!("{}", v + 1), x, y - 16., TEXT_XS, pal.text, TextAlign::Center);
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
        kit::txt(window, cx, "double-click a key to add or remove a voice", r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
    });

    let mut cells = div().flex_none().flex().gap(px(SP1));
    for v in 0..VOICES {
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
                .child(kit::pick(dev, p::on(v), cx))
                .child(kit::num_w(ws, dev, p::interval(v), 68., cx))
                .child(kit::num_w(ws, dev, p::level(v), 68., cx)),
        );
    }
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
    super::midi_kit::body_with(dev, "chord-body", 520., 108., 184., frame, vec![cells, scale])
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

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
        assert_eq!(add, vec![(p::on(3), 1.), (p::interval(3), 11.), (p::level(3), 100.)]);
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
