//! `midi.scale`: one octave of keyboard with the notes of the scale lit and
//! the others dark, and under it an arrow from every key that gets moved to
//! the key it comes out as (with the mode, pass and transpose the device
//! has). Click a key to make it the root, or, with the Custom scale, to put
//! it in or out of the scale. The device has no meter, so the picture is of
//! the settings, not of notes being played.

use super::chord::{KeyBox, follow_project, key_center, piano};
use super::kit::{self, PlotCfg, Rect};
use super::midi_kit as mk;
use super::{Dev, set_now};
use crate::theme::*;
use crate::widgets::mono;
use crate::workspace::Workspace;
use aitrack_stock::midi::scale::{CUSTOM, Rule, p};
use aitrack_stock::midi::theory::{Snap, scale_mask};
use gpui::{AnyElement, Context, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, TextAlign, canvas, div, prelude::FluentBuilder, px, relative};

/// The octave drawn: C4 to B4.
const BASE: i32 = 60;
const KEYS_H: f32 = 64.;
const BLACK_H: f32 = 0.62;

/// The letter of a white key.
fn white_name(key: i32) -> &'static str {
    match key.rem_euclid(12) {
        0 => "C",
        2 => "D",
        4 => "E",
        5 => "F",
        7 => "G",
        9 => "A",
        _ => "B",
    }
}

/// The pitch classes (bit 0 = C) of the scale the device has.
pub fn mask_of(dev: &Dev) -> u16 {
    let scale = dev.choice(p::SCALE).min(CUSTOM);
    if scale == CUSTOM { (0..12).fold(0u16, |m, i| if dev.on(p::CUSTOM0 + i) { m | 1 << i } else { m }) } else { scale_mask(scale, dev.choice(p::ROOT).min(11)) }
}

/// The correction the device applies, as it builds it from its parameters.
pub fn rule_of(dev: &Dev) -> Rule {
    let lowest = dev.plain(p::LOWEST).round() as i32;
    Rule {
        mask: mask_of(dev),
        snap: match dev.choice(p::MODE) {
            1 => Snap::Up,
            2 => Snap::Down,
            _ => Snap::Nearest,
        },
        pass: dev.on(p::PASS),
        transpose: dev.plain(p::TRANSPOSE).round() as i32,
        lowest,
        highest: lowest + dev.plain(p::RANGE).round() as i32,
    }
}

/// The octave's corrections: for each pitch class that comes out as another,
/// `(from, to)` pitch classes (what key `BASE + from` becomes).
pub fn moves(rule: &Rule) -> Vec<(usize, usize)> {
    (0..12)
        .filter_map(|pc| {
            let out = i32::from(rule.map((BASE + pc as i32) as u8));
            let to = out.rem_euclid(12) as usize;
            (out != BASE + pc as i32).then_some((pc, to))
        })
        .collect()
}

/// Where each pitch class of the octave drawn comes out: `(pitch class, moved)`.
pub fn lands(rule: &Rule) -> [(usize, bool); 12] {
    std::array::from_fn(|pc| {
        let out = i32::from(rule.map((BASE + pc as i32) as u8));
        (out.rem_euclid(12) as usize, out != BASE + pc as i32)
    })
}

/// The pitch classes that can come out of the device.
pub fn sounding_mask(rule: &Rule) -> u16 {
    lands(rule).iter().fold(0, |m, (to, _)| m | 1 << to)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    follow_project(ws, dev, p::ROOT, p::SCALE, p::FOLLOW_PROJECT);
    let pal = p();
    let rule = rule_of(dev);
    let custom = dev.choice(p::SCALE).min(CUSTOM) == CUSTOM;
    let root = dev.choice(p::ROOT).min(11);
    let boxes = piano(BASE, BASE + 11);
    let win = mk::windowed(dev);
    let device = dev.id;
    let enabled = dev.enabled;

    // The keys: white ones side by side, black ones on top.
    let key = |b: &KeyBox, cx: &mut Context<Workspace>| {
        let pc = b.key.rem_euclid(12) as usize;
        let lit = rule.mask & (1 << pc) != 0;
        let is_root = !custom && pc == root;
        let colour = if enabled { pal.accent } else { pal.muted };
        let fill = if is_root { with_alpha(colour, 0.85) } else if lit { with_alpha(colour, if b.black { 0.55 } else { 0.4 }) } else if b.black { pal.well } else { content(0.1) };
        let set = dev.info(if custom { p::CUSTOM0 + pc as u32 } else { p::ROOT }).cloned();
        let on = custom && lit;
        let tip = if custom { "Click to put this note in or out of the scale" } else { "Click to make this note the root" };
        div()
            .id(("scale-key", dev.key(pc)))
            .absolute()
            .left(relative(b.x))
            .w(relative(b.w))
            .top_0()
            .h(relative(if b.black { BLACK_H } else { 1. }))
            .rounded(px(r_sm()))
            .border_1()
            .border_color(if b.black { border_strong() } else { border() })
            .bg(if b.black { pal.well } else { content(0.1) })
            .child(div().absolute().size_full().rounded(px(r_sm())).bg(fill))
            .cursor_pointer()
            .tooltip(crate::widgets::tip(tip, None))
            .when(!b.black, |d| d.flex().items_end().justify_center().pb(px(SP1)).child(mono(white_name(b.key), TEXT_SM, pal.text)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(info) = &set {
                    let id = info.id;
                    set_now(this, device, id, if custom { info.to_normalized(f32::from(!on)) } else { info.to_normalized(pc as f32) });
                }
                cx.notify();
            }))
    };
    let mut keyboard = div().relative();
    keyboard = if win { keyboard.flex_grow(1.).flex_basis(px(KEYS_H)).min_h(px(KEYS_H)) } else { keyboard.flex_none().h(px(KEYS_H)) };
    for b in boxes.iter().filter(|b| !b.black) {
        keyboard = keyboard.child(key(b, cx));
    }
    for b in boxes.iter().filter(|b| b.black) {
        keyboard = keyboard.child(key(b, cx));
    }

    let arrows_boxes = boxes.clone();
    let list = moves(&rule);
    let corrected = list.len();
    let landing = lands(&rule);
    let arrows = kit::plot(ws, dev, PlotCfg { slot: 0, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent2 } else { pal.muted };
        if win {
            // Between the two keyboards every key has its line: straight down when it stays, slanted when it moves.
            for (from, (to, moved)) in landing.iter().enumerate() {
                if *moved {
                    continue;
                }
                if let Some(a) = key_center(&arrows_boxes, BASE + from as i32) {
                    kit::vline(window, r, r.px(a), with_alpha(pal.text, 0.14));
                }
                let _ = to;
            }
        } else {
            kit::txt(window, cx, "played", r.x + 4., r.y + 2., TEXT_SM, pal.faint, TextAlign::Left);
            kit::txt(window, cx, "sounds as", r.x + 4., r.bottom() - 16., TEXT_SM, pal.faint, TextAlign::Left);
        }
        for (from, to) in &list {
            let (Some(a), Some(b)) = (key_center(&arrows_boxes, BASE + *from as i32), key_center(&arrows_boxes, BASE + *to as i32)) else { continue };
            let (ax, bx) = (r.px(a), r.px(b));
            let (y0, y1) = if win { (r.y + 2., r.bottom() - 2.) } else { (r.py(0.12), r.py(0.82)) };
            kit::line(window, &[gpui::point(px(ax), px(y0)), gpui::point(px(bx), px(y1))], 1.5, with_alpha(colour, 0.9));
            // The arrowhead, along the line.
            let (dx, dy) = (bx - ax, y1 - y0);
            let len = dx.hypot(dy).max(1.);
            let (ux, uy) = (dx / len, dy / len);
            for s in [-1., 1.] {
                let (hx, hy) = (-ux * 6. - s * uy * 3., -uy * 6. + s * ux * 3.);
                kit::line(window, &[gpui::point(px(bx + hx), px(y1 + hy)), gpui::point(px(bx), px(y1))], 1.5, with_alpha(colour, 0.9));
            }
            kit::dot(window, gpui::point(px(ax), px(y0)), 2.5, colour, None);
        }
        if list.is_empty() {
            kit::txt(window, cx, "every note passes unchanged", r.px(0.5), r.py(0.4), TEXT_SM, pal.faint, TextAlign::Center);
        }
    });

    let range = format!("corrects keys {}–{}", rule.lowest.clamp(0, 127), rule.highest.clamp(0, 127));
    let controls = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(kit::pick(dev, p::ROOT, cx))
        .child(kit::pick(dev, p::SCALE, cx))
        .child(kit::pick(dev, p::MODE, cx))
        .child(kit::pick(dev, p::PASS, cx))
        .child(kit::num(ws, dev, p::TRANSPOSE, cx))
        .child(kit::num(ws, dev, p::LOWEST, cx))
        .child(kit::num(ws, dev, p::RANGE, cx))
        .child(kit::pick(dev, p::FOLLOW_PROJECT, cx));

    // In a window, a second keyboard under the arrows shows the notes that can come out.
    let out_boxes = boxes.clone();
    let out_mask = sounding_mask(&rule);
    let out_root = (!custom).then_some(root);
    let out_keys = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let colour = if enabled { pal.accent } else { pal.muted };
            let r = Rect::of(b);
            super::chord::paint_piano(window, r, &out_boxes, |k| {
                let pc = k.rem_euclid(12) as usize;
                if out_mask & (1 << pc) == 0 {
                    return None;
                }
                Some(with_alpha(colour, if out_root == Some(pc) { 0.85 } else if mk::is_black(k) { 0.55 } else { 0.4 }))
            });
            for b in out_boxes.iter().filter(|b| !b.black) {
                kit::txt(window, cx, white_name(b.key), r.x + r.w * (b.x + b.w / 2.), r.bottom() - 16., TEXT_SM, pal.text, TextAlign::Center);
            }
        },
    )
    .size_full();
    let out_keys = div().relative().flex_grow(1.).flex_basis(px(KEYS_H)).min_h(px(KEYS_H)).child(out_keys);
    let top = div()
        .id(("scale-top", dev.key(0)))
        .size_full()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(keyboard)
        .child(div().flex_grow(if win { 1.6 } else { 1. }).flex_basis(px(if win { 72. } else { 0. })).min_h(px(if win { 72. } else { 0. })).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(arrows))
        .when(win, |d| d.child(out_keys));
    let range = if win { format!("{range} · {corrected} of 12 notes move · {} in the scale", rule.mask.count_ones()) } else { range };
    let note = div().flex_none().child(crate::widgets::label(range, TEXT_SM, p().faint));
    super::midi_kit::body_with(dev, "scale-body", 520., 116., 176., top, vec![note, controls])
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use aitrack_stock::midi::scale::ScaleFx;

    #[test]
    fn the_rule_matches_the_devices_own_default() {
        let d = dev("midi.scale", &[]);
        assert_eq!(rule_of(&d), ScaleFx::new().rule());
    }

    #[test]
    fn out_of_scale_keys_move_and_in_scale_keys_stay() {
        // C major, nearest: the black keys go to a neighbour, ties down.
        let d = dev("midi.scale", &[(p::SCALE, 0.)]);
        let m = moves(&rule_of(&d));
        assert_eq!(m, vec![(1, 0), (3, 2), (6, 5), (8, 7), (10, 9)]);
        // Up sends them the other way.
        let up = dev("midi.scale", &[(p::SCALE, 0.), (p::MODE, 1.)]);
        assert_eq!(moves(&rule_of(&up)), vec![(1, 2), (3, 4), (6, 7), (8, 9), (10, 11)]);
        // Chromatic corrects nothing.
        let all = dev("midi.scale", &[(p::SCALE, 12.)]);
        assert!(moves(&rule_of(&all)).is_empty());
    }

    #[test]
    fn every_pitch_class_has_a_landing_and_the_output_keys_are_the_scale() {
        let rule = rule_of(&dev("midi.scale", &[(p::SCALE, 0.)]));
        let l = lands(&rule);
        assert_eq!((l[0], l[1], l[2]), ((0, false), (0, true), (2, false)));
        assert_eq!(l.iter().filter(|(_, moved)| *moved).count(), moves(&rule).len());
        // C major: the seven white keys.
        assert_eq!(sounding_mask(&rule), 0b1010_1011_0101);
        // Chromatic keeps every key.
        assert_eq!(sounding_mask(&rule_of(&dev("midi.scale", &[(p::SCALE, 12.)]))), 0x0FFF);
    }

    #[test]
    fn transpose_and_pass_off_move_every_note() {
        let t = dev("midi.scale", &[(p::SCALE, 0.), (p::TRANSPOSE, 7.)]);
        // Everything moves by a fifth (the black keys then get corrected too).
        assert_eq!(moves(&rule_of(&t)).len(), 12);
        // Pass off with Up: a diatonic step for every note.
        let step = dev("midi.scale", &[(p::SCALE, 0.), (p::MODE, 1.), (p::PASS, 0.)]);
        assert!(moves(&rule_of(&step)).contains(&(0, 2)));
    }

    #[test]
    fn the_custom_scale_reads_its_twelve_switches_and_ranges_limit_the_correction() {
        let c = dev("midi.scale", &[(p::SCALE, 13.), (p::CUSTOM0, 1.), (p::CUSTOM0 + 1, 0.), (p::CUSTOM0 + 2, 0.), (p::CUSTOM0 + 4, 0.), (p::CUSTOM0 + 5, 0.), (p::CUSTOM0 + 7, 0.), (p::CUSTOM0 + 9, 0.), (p::CUSTOM0 + 11, 0.)]);
        assert_eq!(mask_of(&c), 1);
        // Outside Lowest..Lowest+Range keys pass through.
        let r = dev("midi.scale", &[(p::SCALE, 0.), (p::LOWEST, 80.), (p::RANGE, 5.)]);
        assert!(moves(&rule_of(&r)).is_empty(), "the octave drawn is below Lowest");
    }
}
