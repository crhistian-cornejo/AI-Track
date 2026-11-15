//! `midi.random`: the notes an example note can become. The played note sits
//! in the middle of a key strip; the possible results are bars on their keys,
//! up (one colour) and down (the other) from it, as tall as their share of
//! the chance, with the unchanged note standing for the notes that are not
//! altered. With Limit to Scale the keys of the scale are lit, a candidate
//! outside it is a faint ghost and its share moves to the note it snaps to.
//! Two handles on the bars set how far they reach (Choices) and the size of
//! one step (Interval).

use super::Dev;
use super::kit::{self, Handle, PlotCfg, ax};
use super::midi_kit as mk;
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::set_many;
use crate::widgets::{self, label};
use crate::workspace::Workspace;
use aitrack_dsp::ParamInfo;
use aitrack_model::{Key, ScaleKind};
use aitrack_stock::midi::kit::fold_octaves;
use aitrack_stock::midi::random::p;
use aitrack_stock::midi::theory::{ROOT_NAMES, SCALE_NAMES, Snap, in_mask, scale_mask, snap_note};
use gpui::{AnyElement, Context, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, TextAlign, div, prelude::FluentBuilder, px};

const PAD: f32 = 6.;
/// Room above the bars (the header), and below them (the keys and names).
const TOP: f32 = 22.;
const BOTTOM: f32 = 26.;
const KEYS_H: f32 = 10.;

/// The settings that decide the pitch of an altered note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cfg {
    /// Share of notes that are altered, 0..1.
    pub chance: f32,
    pub step: i32,
    pub choices: i32,
    /// 0 up, 1 down, 2 both.
    pub sign: usize,
    /// Scale to stay in (bit 0 = C); 0 = free.
    pub mask: u16,
}

impl Cfg {
    fn of(dev: &Dev) -> Self {
        Self {
            chance: dev.plain(p::CHANCE) / 100.,
            step: dev.plain(p::INTERVAL).round() as i32,
            choices: dev.plain(p::CHOICES).round() as i32,
            sign: dev.choice(p::SIGN),
            mask: if dev.on(p::SNAP) { scale_mask(dev.choice(p::SCALE), dev.choice(p::ROOT)) } else { 0 },
        }
    }

    /// The offsets (in steps) an altered note may take, each equally likely.
    pub fn offsets(&self) -> Vec<i32> {
        let c = self.choices.max(0);
        match self.sign {
            0 => (1..=c).collect(),
            1 => (1..=c).map(|n| -n).collect(),
            _ => (1..=c).rev().map(|n| -n).chain(1..=c).collect(),
        }
    }
}

/// One thing an altered note may become before the scale has its say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub key: u8,
    pub in_scale: bool,
    /// Where it ends up (the same key when it is in the scale or no scale applies).
    pub lands: u8,
}

/// The candidates for a played `key`, in order from the lowest offset.
pub fn candidates(c: &Cfg, key: i32) -> Vec<Candidate> {
    c.offsets()
        .into_iter()
        .map(|n| {
            let k = fold_octaves(key + n * c.step);
            let lands = if c.mask != 0 { snap_note(c.mask, k, Snap::Nearest, false) } else { k };
            Candidate { key: k, in_scale: c.mask == 0 || in_mask(c.mask, k as i32), lands }
        })
        .collect()
}

/// The chance of each key being what `key` becomes.
pub fn distribution(c: &Cfg, key: i32) -> [f32; 128] {
    let mut out = [0.; 128];
    let list = candidates(c, key);
    if list.is_empty() {
        out[key.clamp(0, 127) as usize] = 1.;
        return out;
    }
    out[key.clamp(0, 127) as usize] += 1. - c.chance;
    let each = c.chance / list.len() as f32;
    for cand in list {
        out[cand.lands as usize] += each;
    }
    out
}

/// Keys either side of the played one that the strip shows.
fn half_span(c: &Cfg) -> i32 {
    (c.choices.max(1) * c.step + 2).clamp(12, 30)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let c = Cfg::of(dev);
    let h = mk::plot_h(dev);
    let key = if c.mask != 0 { 60 + dev.choice(p::ROOT) as i32 } else { 60 };
    let half = half_span(&c);
    let cols = (2 * half + 1) as f32;
    let seen = f32::from(ws.fx.region(dev.id, 0).get().size.width);
    let w = if seen > 100. { seen } else if dev.big { 580. } else { 500. };
    let cw = (w - 2. * PAD) / cols;
    let dist = distribution(&c, key);
    let peak = dist.iter().copied().fold(0.01, f32::max);
    let area = h - TOP - BOTTOM;
    let base_rel = h - BOTTOM;
    let col_frac = |offset: i32| (PAD + (offset + half) as f32 * cw + cw / 2.) / w;
    let top_frac = |k: i32| {
        let p = dist.get(k.clamp(0, 127) as usize).copied().unwrap_or(0.);
        (base_rel - (area * p / peak).max(3.)) / h
    };
    // The far end of the reach and the first step, on the side the bars lean to.
    let side = if c.sign == 1 { -1 } else { 1 };
    let reach = c.choices.max(1);
    let mut handles = Vec::new();
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let at = |n: i32| {
        let off = n * side * c.step;
        // Past the strip the handle waits at its edge, on the floor.
        if off.abs() > half {
            return (col_frac(off.clamp(-half, half)), (base_rel - 3.) / h);
        }
        (col_frac(off), top_frac(key + off))
    };
    let (fx, fy) = at(reach);
    let cols_per_frac = w / cw;
    handles.push(Handle { fx, fy, x: ax(p::CHOICES, side as f32 * cols_per_frac / c.step.max(1) as f32 / 24.), index: 0, reset: vec![default(p::CHOICES)], ..Default::default() });
    if c.choices > 1 {
        let (fx, fy) = at(1);
        handles.push(Handle { fx, fy, x: ax(p::INTERVAL, side as f32 * cols_per_frac / 11.), index: 1, reset: vec![default(p::INTERVAL)], ..Default::default() });
    }
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let enabled = dev.enabled;
    let distinct = dist.iter().filter(|v| **v > 1e-4).count();
    let mut left = if c.choices == 0 { "pitch unchanged".to_owned() } else { format!("{distinct} notes possible") };
    if c.chance < 0.995 {
        left += &format!(" · {:.0}% altered", c.chance * 100.);
    }
    let mut right = Vec::new();
    if dev.plain(p::VELOCITY) > 0. {
        right.push(format!("velocity ±{:.0}%", dev.plain(p::VELOCITY)));
    }
    if dev.plain(p::DELAY) > 0. {
        right.push(format!("up to {:.0} ms late", dev.plain(p::DELAY)));
    }
    if c.mask != 0 {
        right.push(format!("{} {}", ROOT_NAMES[dev.choice(p::ROOT).min(11)], SCALE_NAMES[dev.choice(p::SCALE).min(SCALE_NAMES.len() - 1)]));
    }
    let right = right.join(" · ");
    let ghosts = candidates(&c, key);

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let lit = |c: Hsla| if enabled { c } else { pal.muted };
        let cw = (r.w - 2. * PAD) / cols;
        let x_of = |k: i32| r.x + PAD + (k - key + half) as f32 * cw;
        let strip = r.bottom() - BOTTOM;
        let area = (strip - r.y - TOP).max(8.);
        mk::corner(window, cx, r, &left, false, 3.);
        mk::corner(window, cx, r, &right, true, 3.);
        kit::hline(window, r, strip, pal.grid_bar);
        // The keys around the played note.
        for k in key - half..=key + half {
            if !(0..=128).contains(&(k + 1)) || k < 0 {
                continue;
            }
            let black = mk::is_black(k);
            let mut fill = content(if black { 0.05 } else { 0.12 });
            if c.mask != 0 {
                fill = if in_mask(c.mask, k) { with_alpha(if k.rem_euclid(12) == (key.rem_euclid(12)) { pal.root_highlight } else { pal.scale_highlight }, if black { 0.45 } else { 0.7 }) } else { content(0.03) };
            }
            kit::bar(window, x_of(k), strip + 1., (cw - 0.6).max(1.), KEYS_H, 0., fill);
            if k.rem_euclid(12) == 0 && k != key {
                kit::txt(window, cx, &mk::note_name(k), x_of(k), r.bottom() - 13., TEXT_XS, pal.faint, TextAlign::Left);
            }
        }
        // The candidates outside the scale, faint, before they snap.
        for g in ghosts.iter().filter(|g| !g.in_scale) {
            let k = g.key as i32;
            if (k - key).abs() <= half {
                let each = c.chance / ghosts.len() as f32;
                let gh = (area * each / peak).max(3.);
                kit::bar(window, x_of(k) + 1., strip - gh, (cw - 2.).max(2.), gh, 1.5, with_alpha(pal.faint, 0.45));
                kit::bar(window, x_of(k) + 1., strip - gh, (cw - 2.).max(2.), 1.5, 0., with_alpha(pal.muted, 0.9));
            }
        }
        // The outcomes, as tall as their share.
        for (k, share) in dist.iter().enumerate() {
            let k = k as i32;
            if *share <= 1e-4 || (k - key).abs() > half {
                continue;
            }
            let color = match k.cmp(&key) {
                std::cmp::Ordering::Equal => pal.text,
                std::cmp::Ordering::Greater => series(0),
                std::cmp::Ordering::Less => series(1),
            };
            let bh = (area * share / peak).max(3.);
            kit::bar(window, x_of(k) + 1., strip - bh, (cw - 2.).max(2.), bh, 1.5, with_alpha(lit(color), 0.85));
        }
        kit::bar(window, x_of(key) + cw / 2. - 0.5, r.y + TOP, 1., area, 0., with_alpha(pal.text, 0.25));
        kit::txt(window, cx, &mk::note_name(key), x_of(key) + cw / 2., r.bottom() - 13., TEXT_XS, pal.text, TextAlign::Center);
        for (i, (fx, fy)) in marks.iter().enumerate() {
            kit::dot(window, r.pt(*fx, *fy), if i == 0 { 6. } else { 5. }, lit(if side == 1 { series(0) } else { series(1) }), Some(pal.raised));
        }
    });

    let pitch = vec![kit::ctl(ws, dev, p::CHOICES, cx), kit::ctl(ws, dev, p::INTERVAL, cx), kit::ctl(ws, dev, p::SIGN, cx)];
    let amount = vec![kit::ctl(ws, dev, p::CHANCE, cx), kit::ctl(ws, dev, p::VELOCITY, cx), kit::ctl(ws, dev, p::DELAY, cx)];
    let mut scale = vec![project_chip(ws, dev, cx)];
    scale.extend([kit::ctl(ws, dev, p::ROOT, cx), kit::ctl(ws, dev, p::SCALE, cx)]);
    let seed = vec![kit::ctl(ws, dev, p::SEED, cx), kit::ctl(ws, dev, p::LOOP, cx)];
    let cards = vec![
        kit::section(dev, "PITCH", None, pitch, cx),
        kit::section(dev, "AMOUNT", None, amount, cx),
        kit::section(dev, "LIMIT TO SCALE", Some(p::SNAP), scale, cx),
        kit::section(dev, "SEED", None, seed, cx),
    ];
    mk::body(dev, "random-body", 560., plot, cards)
}

/// The index of the project's scale in the device's lists.
pub fn project_scale(ws: &Workspace) -> Option<(usize, usize)> {
    let s = ws.s.project.scale?;
    let Key(root) = s.root;
    Some((root as usize % 12, ScaleKind::ALL.iter().position(|k| *k == s.kind)?))
}

/// What switching Project Scale `on` sets: the switch, and with a project
/// scale to follow, its root and scale in the device's own lists.
pub fn scale_values(on: bool, project: Option<(usize, usize)>, root: Option<&ParamInfo>, scale: Option<&ParamInfo>) -> Vec<(u32, f32)> {
    let mut values = vec![(p::PROJECT, if on { 1. } else { 0. })];
    if let (true, Some((r, k)), Some(ri), Some(si)) = (on, project, root, scale) {
        values.push((p::ROOT, ri.to_normalized(ri.min + r as f32)));
        values.push((p::SCALE, si.to_normalized(si.min + k as f32)));
    }
    values
}

/// The "follow the project scale" switch. The device only reads Root and
/// Scale, so switching it on copies the project's into them in the same undo
/// step, and while it is on and they differ a chip offers to copy them again.
fn project_chip(ws: &Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let on = dev.on(p::PROJECT);
    let device = dev.id;
    let project = project_scale(ws);
    let (root, scale) = (dev.info(p::ROOT).cloned(), dev.info(p::SCALE).cloned());
    let stale = on && project.is_some_and(|(r, k)| r != dev.choice(p::ROOT) || k != dev.choice(p::SCALE));
    let copy = move |on: bool, project: Option<(usize, usize)>| scale_values(on, project, root.as_ref(), scale.as_ref());
    let toggle = copy.clone();
    let chip = div()
        .id(("rnd-project", dev.key(p::PROJECT as usize)))
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { accent_alpha(0.2) } else { content(0.06) })
        .hover(|s| s.bg(if on { accent_alpha(0.28) } else { content(0.12) }))
        .tooltip(widgets::tip("Use the project's scale (copied into Root and Scale)", None))
        .child(label("Project", TEXT_XS, if on { pal.accent } else { pal.text }).font_weight(FontWeight::MEDIUM))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            let values = toggle(!on, project_scale(this));
            set_many(this, "Project scale", device, &values);
            cx.notify();
        }));
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(chip)
        .when(stale, |d| {
            d.child(
                div()
                    .id(("rnd-project-sync", dev.key(p::ROOT as usize)))
                    .flex_none()
                    .h(px(20.))
                    .px(px(SP2))
                    .gap(px(SP1))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(content(0.06))
                    .hover(|s| s.bg(content(0.12)))
                    .tooltip(widgets::tip("The project's scale changed: copy it again", None))
                    .child(icon(Icon::Refresh, 10., pal.muted))
                    .child(label("Match", TEXT_XS, pal.text).font_weight(FontWeight::MEDIUM))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let values = copy(true, project_scale(this));
                        set_many(this, "Project scale", device, &values);
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    fn cfg(sign: usize, choices: i32, step: i32, chance: f32, mask: u16) -> Cfg {
        Cfg { chance, step, choices, sign, mask }
    }

    #[test]
    fn offsets_follow_the_sign_and_never_include_zero() {
        assert_eq!(cfg(0, 3, 1, 1., 0).offsets(), vec![1, 2, 3]);
        assert_eq!(cfg(1, 3, 1, 1., 0).offsets(), vec![-1, -2, -3]);
        assert_eq!(cfg(2, 2, 1, 1., 0).offsets(), vec![-2, -1, 1, 2]);
        assert!(cfg(2, 0, 1, 1., 0).offsets().is_empty());
    }

    #[test]
    fn the_distribution_splits_the_chance_among_the_candidates() {
        let d = distribution(&cfg(2, 2, 3, 1., 0), 60);
        for k in [54, 57, 63, 66] {
            assert!((d[k] - 0.25).abs() < 1e-6, "{k}");
        }
        assert_eq!(d[60], 0.);
        // The rest of the notes are not altered.
        let half = distribution(&cfg(0, 2, 2, 0.5, 0), 60);
        assert!((half[60] - 0.5).abs() < 1e-6 && (half[62] - 0.25).abs() < 1e-6 && (half[64] - 0.25).abs() < 1e-6);
        assert!((half.iter().sum::<f32>() - 1.).abs() < 1e-5);
        // Nothing to choose from: the note stays.
        assert_eq!(distribution(&cfg(2, 0, 1, 1., 0), 60)[60], 1.);
    }

    #[test]
    fn a_scale_moves_the_outsiders_to_the_nearest_note() {
        let c_major = scale_mask(0, 0);
        let c = cfg(2, 1, 1, 1., c_major);
        let list = candidates(&c, 60);
        assert_eq!(list.iter().map(|k| (k.key, k.in_scale, k.lands)).collect::<Vec<_>>(), vec![(59, true, 59), (61, false, 60)]);
        let d = distribution(&c, 60);
        assert!((d[59] - 0.5).abs() < 1e-6 && (d[60] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn keys_that_leave_the_keyboard_fold_back() {
        let c = cfg(0, 1, 12, 1., 0);
        assert_eq!(distribution(&c, 125)[fold_octaves(137) as usize], 1.);
    }

    #[test]
    fn the_strip_reaches_as_far_as_the_bars() {
        assert_eq!(half_span(&cfg(2, 0, 1, 1., 0)), 12);
        assert_eq!(half_span(&cfg(2, 4, 3, 1., 0)), 14);
        assert_eq!(half_span(&cfg(2, 24, 12, 1., 0)), 30);
    }

    #[test]
    fn the_settings_read_from_the_device() {
        let d = dev("midi.random", &[(p::CHANCE, 50.), (p::CHOICES, 4.), (p::INTERVAL, 2.), (p::SIGN, 0.), (p::SNAP, 1.), (p::ROOT, 2.), (p::SCALE, 1.)]);
        let c = Cfg::of(&d);
        assert_eq!((c.chance, c.choices, c.step, c.sign), (0.5, 4, 2, 0));
        assert_eq!(c.mask, scale_mask(1, 2));
        assert_eq!(Cfg::of(&dev("midi.random", &[])).mask, 0, "no scale until Limit to Scale is on");
    }

    #[test]
    fn switching_on_copies_the_project_scale_into_root_and_scale() {
        let d = dev("midi.random", &[]);
        let (root, scale) = (d.info(p::ROOT).unwrap(), d.info(p::SCALE).unwrap());
        // A minor: root 9, scale 1.
        let on = scale_values(true, Some((9, 1)), Some(root), Some(scale));
        assert_eq!(on.len(), 3);
        assert_eq!(on[0], (p::PROJECT, 1.));
        assert_eq!(root.to_plain(on[1].1).round(), 9.);
        assert_eq!(scale.to_plain(on[2].1).round(), 1.);
        // Without a project scale, or switching off, only the switch moves.
        assert_eq!(scale_values(true, None, Some(root), Some(scale)), vec![(p::PROJECT, 1.)]);
        assert_eq!(scale_values(false, Some((9, 1)), Some(root), Some(scale)), vec![(p::PROJECT, 0.)]);
    }

    #[test]
    fn the_project_scale_indexes_the_device_lists() {
        use aitrack_model::{Scale, ScaleKind};
        assert_eq!(ScaleKind::ALL.len(), SCALE_NAMES.len());
        for (i, kind) in ScaleKind::ALL.iter().enumerate() {
            assert_eq!(SCALE_NAMES[i], kind.label());
        }
        let _ = Scale::new(Key(9), ScaleKind::Minor);
    }
}
