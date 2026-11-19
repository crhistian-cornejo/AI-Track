//! The Bloom window, laid out like a standalone instrument:
//!
//! * a header: the name, the seed (typed, or rolled with the dice), the
//!   species and the matter as dropdowns, the voices as a small live meter;
//! * the generator on the left: Roll and Mutate, the history, the selected
//!   sound (its name, lock, envelope) and the export chip;
//! * the stage in the middle, the hero, with the sixteen pads under it;
//! * every knob on the right, grouped (Sound, Shape, Space · Output, Play),
//!   from the table in `knobs.rs`; a narrow window moves them under the stage;
//! * a segmented control over the gene editor, the morph, the play settings
//!   and the tools.
//!
//! Every hint lives in a tooltip; the frame timings only show with `AITRACK_FPS`.

use super::matter::{self, Material};
use super::{TIP_BACK, TIP_EXPORT, TIP_FWD, TIP_KIND, TIP_MATERIAL, TIP_MORPH, TIP_MUTATE, TIP_ROLL, TIP_SPECIES, TIP_STAGE, TIP_STAGE_TONAL, dial, dial_w, dna, icon_action, kind_icon, knobs, morph, pads, patch_view, stage, tools, view};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::{Dev, instr, kit};
use crate::widgets::{self, label, mono};
use crate::workspace::{DraggedSample, Workspace};
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene, Species, g};
use aitrack_stock::instrument::bloom::patch::{Kind, Patch};
use aitrack_stock::instrument::bloom::{VOICES, p};
use gpui::{
    AnyElement, AppContext, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, canvas, div,
    point, prelude::FluentBuilder, px,
};

/// The side columns' width.
pub const SIDE_W: f32 = 248.;
/// The knobs' size in the window, and how many sit in a row of a group.
pub const KNOB: f32 = 28.;
pub const PER_ROW: usize = 4;
/// Below this width the knobs leave the right column for a row under the stage.
pub const NARROW_W: f32 = 1040.;
/// The tabs' body height.
pub const TAB_H: f32 = 208.;
/// The header's height.
pub const HEADER_H: f32 = 32.;
/// The least the row of the generator, the stage and the knobs keeps (the
/// generator column's content).
pub const MAIN_MIN_H: f32 = 440.;
/// The knob groups' cards when a narrow window moves them under the stage (two rows of them).
pub const BELOW_H: f32 = 2. * 112. + SP3;

/// The least height the editor needs before the window scrolls.
pub fn min_height(narrow: bool) -> f32 {
    let below = if narrow { BELOW_H + SP3 } else { 0. };
    2. * SP4 + HEADER_H + MAIN_MIN_H + below + 32. + TAB_H + 3. * SP3
}

/// A section title: small caps in the faint text colour over a hairline.
fn section(title: &'static str, tip: Option<&'static str>) -> Stateful<Div> {
    let pal = p();
    div()
        .id(SharedString::from(title))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(label(title, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1().h(px(1.)).bg(border()))
        .when_some(tip, |d, t| d.tooltip(widgets::tip(t, None)))
}

/// A panel of the window: the surface a little lifted from the base, a hairline border.
fn card() -> Div {
    div().p(px(SP3)).gap(px(SP3)).flex().flex_col().rounded(px(r_lg())).bg(content(0.03)).border_1().border_color(border())
}

/// What picking a dropdown item does.
type Pick = Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>;

/// A dropdown: `title` in small caps, the value, a chevron; `items` open under it.
#[allow(clippy::too_many_arguments)]
fn dropdown(
    id: (&'static str, usize),
    title: &'static str,
    value: &'static str,
    width: f32,
    tip: &'static str,
    open: bool,
    items: Vec<(&'static str, bool, Pick)>,
    toggle: impl Fn(&mut Workspace, bool) + Clone + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let t1 = toggle.clone();
    let mut el = div()
        .id(id)
        .relative()
        .h(px(24.))
        .w(px(width))
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if open { content(0.1) } else { content(0.05) })
        .border_1()
        .border_color(if open { accent_alpha(0.5) } else { border() })
        .hover(|s| s.bg(content(0.09)))
        .child(label(title, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(label(value, TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM))
        .child(div().flex_1())
        .child(icon(Icon::ChevronDown, 12., pal.muted))
        .tooltip(widgets::tip(tip, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            t1(this, !open);
            cx.notify();
        }));
    if open {
        let mut list = div().w(px(width.max(160.))).p(px(SP1)).flex().flex_col().gap(px(SP1)).rounded(px(r_md())).bg(pal.overlay).border_1().border_color(border()).shadow_lg();
        for (k, (name, on, pick)) in items.into_iter().enumerate() {
            let t = toggle.clone();
            list = list.child(
                div()
                    .id((id.0, id.1 * 64 + 1 + k))
                    .h(px(24.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .gap(px(SP2))
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .when(on, |d| d.bg(accent_alpha(0.16)))
                    .hover(|s| s.bg(content(0.08)))
                    .child(label(name, TEXT_SM, if on { pal.accent } else { pal.text }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        t(this, false);
                        pick(this, cx);
                        cx.notify();
                    })),
            );
        }
        let t = toggle.clone();
        el = el.child(
            gpui::deferred(
                gpui::anchored().offset(point(px(0.), px(28.))).snap_to_window_with_margin(px(SP2)).child(
                    div()
                        .id((id.0, id.1 * 64))
                        .occlude()
                        .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                            t(this, false);
                            cx.notify();
                        }))
                        .child(list),
                ),
            )
            .with_priority(2),
        );
    }
    el.into_any_element()
}

/// The material picker: a dropdown of every material. A look only; picking one is one undo step.
pub fn material_picker(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let now = view::parsed(ws, dev.id).material;
    let open = ws.fx.bloom.view(dev.id).stage.menu;
    let device = dev.id;
    let items = Material::ALL
        .into_iter()
        .map(|m| {
            let d = dev.clone();
            let pick: Pick = Box::new(move |ws, _| view::set_material(ws, &d, m));
            (m.name(), m == now, pick)
        })
        .collect();
    let (d1, d2) = (dev.clone(), dev.clone());
    div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(icon_action(("bloom-material-prev", dev.key(0x601)), Icon::ChevronLeft, true, "Previous matter", cx, move |ws, _| {
            let m = view::parsed(ws, d1.id).material.step(-1);
            view::set_material(ws, &d1, m);
        }))
        .child(dropdown(("bloom-material", dev.key(0x600)), "MATTER", now.name(), 156., TIP_MATERIAL, open, items, move |ws, o| ws.fx.bloom.view(device).stage.menu = o, cx))
        .child(icon_action(("bloom-material-next", dev.key(0x602)), Icon::ChevronRight, true, "Next matter", cx, move |ws, _| {
            let m = view::parsed(ws, d2.id).material.step(1);
            view::set_material(ws, &d2, m);
        }))
        .into_any_element()
}

/// The species as a dropdown: picking one rolls a scene of it.
fn species_picker(ws: &mut Workspace, dev: &Dev, scene: &Scene, cx: &mut Context<Workspace>) -> AnyElement {
    let open = ws.fx.bloom.view(dev.id).stage.species_menu;
    let device = dev.id;
    let items = Species::ALL
        .into_iter()
        .map(|s| {
            let d = dev.clone();
            let pick: Pick = Box::new(move |ws, _| view::roll(ws, &d, Some(s)));
            (s.label(), s == scene.species, pick)
        })
        .collect();
    dropdown(("bloom-species", dev.key(0x608)), "SPECIES", scene.species.label(), 172., TIP_SPECIES, open, items, move |ws, o| ws.fx.bloom.view(device).stage.species_menu = o, cx)
}

/// The kind of sound as a dropdown: Drums or a tonal kind.
fn kind_picker(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let open = ws.fx.bloom.view(dev.id).stage.kind_menu;
    let device = dev.id;
    let now = view::kind(dev);
    let items = Kind::ALL
        .into_iter()
        .map(|k| {
            let d = dev.clone();
            let pick: Pick = Box::new(move |ws, _| view::set_kind(ws, &d, k));
            (k.label(), k == now, pick)
        })
        .collect();
    dropdown(("bloom-kind", dev.key(0x60C)), "KIND", now.label(), 140., TIP_KIND, open, items, move |ws, o| ws.fx.bloom.view(device).stage.kind_menu = o, cx)
}

/// The voices in use as a small meter.
fn voices_meter(dev: &Dev) -> AnyElement {
    let pal = p();
    let used = dev.m(82).clamp(0., 1.);
    div()
        .id(("bloom-voices", dev.key(0x609)))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(label("VOICES", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(div().w(px(40.)).h(px(4.)).rounded(px(2.)).bg(content(0.1)).child(div().h_full().w(gpui::relative(used)).rounded(px(2.)).bg(if used > 0.9 { pal.record } else { pal.accent2 })))
        .child(mono(format!("{:>2}", (used * VOICES as f32).round()), TEXT_XS, if used > 0. { pal.text } else { pal.faint }))
        .tooltip(widgets::tip("Voices — notes sounding now, of 24", None))
        .into_any_element()
}

/// The header: name, seed, species, (room for the coming Kind), voices, matter.
fn header(ws: &mut Workspace, dev: &Dev, scene: &Scene, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let d = dev.clone();
    let dice = widgets::sized_icon_button(("bloom-dice", dev.key(0x60A)), Icon::Dice, false, 24., 14.)
        .tooltip(widgets::tip(TIP_ROLL, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            view::roll(this, &d, None);
            cx.notify();
        }));
    let seed = div()
        .id(("bloom-seed", dev.key(0x60B)))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(kit::num_shown(ws, dev, p::SEED, 80., Some(format!("{:06}", scene.seed)), cx))
        .child(dice)
        .tooltip(widgets::tip("Seed — type one, or roll the dice", None));
    div()
        .flex_none()
        .h(px(HEADER_H))
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(label("Bloom", TEXT_LG, pal.text).font_weight(FontWeight::BOLD))
        .child(seed)
        .child(kind_picker(ws, dev, cx))
        .when(!view::kind(dev).is_tonal(), |d| d.child(species_picker(ws, dev, scene, cx)))
        .child(div().flex_1())
        .child(voices_meter(dev))
        .child(material_picker(ws, dev, cx))
}

/// A wide button with an icon and a label: the primary one filled with the accent.
fn big_button(id: (&'static str, usize), text: &'static str, i: Icon, primary: bool, tip: &'static str) -> Stateful<Div> {
    let pal = p();
    let fg = if primary { pal.on_accent } else { pal.text };
    let hover = if primary { mix(pal.accent, pal.text, 0.12) } else { content(0.1) };
    div()
        .id(id)
        .flex_1()
        .h(px(36.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if primary { pal.accent } else { content(0.05) })
        .border_1()
        .border_color(if primary { accent_alpha(0.9) } else { border() })
        .hover(move |s| s.bg(hover))
        .child(icon(i, 16., fg))
        .child(label(text, TEXT_SM, fg).font_weight(FontWeight::SEMIBOLD))
        .tooltip(widgets::tip(tip, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A bordered square button for the history.
fn step_button(id: (&'static str, usize), i: Icon, enabled: bool, tip: &'static str) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_md()))
        .border_1()
        .border_color(border())
        .bg(content(0.04))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(content(0.1))))
        .when(!enabled, |d| d.opacity(0.4))
        .child(icon(i, 14., pal.text))
        .tooltip(widgets::tip(tip, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Roll and Mutate, then the history: back, where it is, forward.
fn generator(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let v = ws.fx.bloom.view(dev.id);
    let (at, len) = (v.at, v.history.len());
    let (back, fwd) = (at > 0, at + 1 < len);
    let (d1, d2, d3, d4) = (dev.clone(), dev.clone(), dev.clone(), dev.clone());
    let roll = big_button(("bloom-roll-big", dev.key(0x630)), "Roll", Icon::Dice, true, TIP_ROLL).on_click(cx.listener(move |this, _, _, cx| {
        view::roll(this, &d1, None);
        cx.notify();
    }));
    let mutate = big_button(("bloom-mutate-big", dev.key(0x631)), "Mutate", Icon::Shuffle, false, TIP_MUTATE).on_click(cx.listener(move |this, _, _, cx| {
        view::mutate(this, &d2);
        cx.notify();
    }));
    let prev = step_button(("bloom-back", dev.key(3)), Icon::ChevronLeft, back, TIP_BACK).on_click(cx.listener(move |this, _, _, cx| {
        view::step_history(this, &d3, -1);
        cx.notify();
    }));
    let next = step_button(("bloom-fwd", dev.key(4)), Icon::ChevronRight, fwd, TIP_FWD).on_click(cx.listener(move |this, _, _, cx| {
        view::step_history(this, &d4, 1);
        cx.notify();
    }));
    let place = if len == 0 { "No history yet".to_owned() } else { format!("Scene {} of {}", at + 1, len) };
    // The history holds drum scenes; a tonal kind steps back with undo.
    let history = (!view::kind(dev).is_tonal()).then(|| div().flex().items_center().gap(px(SP2)).child(prev).child(div().flex_1().flex().justify_center().child(mono(place, TEXT_XS, pal.muted))).child(next));
    div().flex().flex_col().gap(px(SP2)).child(div().flex().gap(px(SP2)).child(roll).child(mutate)).children(history)
}

/// The points of an attack–hold–decay envelope (decay = time to −40 dB),
/// `n + 1` of them over `total` ms, levels 0..1.
pub fn envelope_points(attack: f32, hold: f32, decay: f32, total: f32, n: usize) -> Vec<(f32, f32)> {
    let (a, d) = (attack.max(0.1), decay.max(1.0));
    (0..=n)
        .map(|k| {
            let t = k as f32 / n as f32 * total;
            let v = if t < a {
                t / a
            } else if t < a + hold {
                1.0
            } else {
                (-4.6 * (t - a - hold) / d).exp()
            };
            (t, v)
        })
        .collect()
}

/// The selected sound: its colour and name, its lock, its amplitude envelope
/// filled in its colour: a drum's (the resonator's ring counts when it
/// outlasts the body), or a tonal patch's amp ADSR over its demo note.
#[allow(clippy::too_many_arguments)]
fn selected_sound(dev: &Dev, scene: &Scene, patch: Option<&Patch>, cell: usize, name: String, locked: bool, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let (pts, total, tip) = match patch {
        Some(x) => {
            let x = x.folded();
            let (pts, total) = patch_view::envelope(&x, 0, 64);
            let w = |f: u32| x.plain(aitrack_stock::instrument::wavetable::p::env(0, f));
            use aitrack_stock::instrument::wavetable::p as wp;
            let tip = format!("Amp envelope — attack {:.0} ms · decay {:.0} ms · sustain {:.0}% · release {:.0} ms", w(wp::E_ATTACK), w(wp::E_DECAY), w(wp::E_SUSTAIN), w(wp::E_RELEASE));
            (pts, total, tip)
        }
        None => {
            let gn = scene.sounds[cell.min(SOUNDS - 1)].folded();
            let (a, h) = (gn.plain(g::ATTACK), gn.plain(g::HOLD).max(0.0));
            let ring = if gn.plain(g::RES_MIX) > 0.2 { gn.plain(g::RES_DECAY) * 0.8 } else { 0.0 };
            let decay = gn.plain(g::DECAY).max(ring).max(1.0);
            let total = (a + h + decay * 1.15).max(10.0);
            (envelope_points(a, h, decay, total, 64), total, format!("Envelope — attack {a:.0} ms · decay {decay:.0} ms"))
        }
    };
    let glyph = patch.map(|x| icon(kind_icon(x.kind), 12., pal.muted));
    let grid = pal.grid_beat;
    let plot = canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let (x0, y0, w, hh) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
            let y = |v: f32| y0 + hh - SP1 - v * (hh - SP3);
            for q in [0.25f32, 0.5, 0.75] {
                crate::paint::polyline(window, &[point(px(x0 + q * w), px(y0)), point(px(x0 + q * w), px(y0 + hh))], 1., grid);
            }
            let line: Vec<_> = pts.iter().map(|(t, v)| point(px(x0 + t / total * w), px(y(*v)))).collect();
            let mut area = line.clone();
            area.push(point(px(x0 + w), px(y(0.))));
            area.push(point(px(x0), px(y(0.))));
            crate::paint::polygon(window, &area, with_alpha(color, 0.18));
            crate::paint::polyline(window, &line, 1.5, color);
        },
    )
    .size_full();
    let d = dev.clone();
    let lock = widgets::icon_chip(("bloom-sel-lock", dev.key(0x641)), Icon::Lock, locked, pal.accent)
        .tooltip(widgets::tip("Lock — Roll and Mutate leave it alone", None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            view::toggle_lock(this, &d, cell);
            cx.notify();
        }));
    div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().size(px(12.)).rounded_full().bg(color))
                .child(label(crate::paint::fit(&format!("{:02} {}", cell + 1, name), SIDE_W - 96., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::SEMIBOLD))
                .children(glyph)
                .child(div().flex_1())
                .child(lock),
        )
        .child(
            div()
                .id(("bloom-envelope", dev.key(0x642)))
                .h(px(56.))
                .w_full()
                .rounded(px(r_md()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .overflow_hidden()
                .child(plot)
                .tooltip(widgets::tip_owned(tip)),
        )
}

/// The export chip: renders the selected sound to a WAV, then drags onto a
/// track or a pad as a sample.
fn export_chip(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let genome = view::export_key(ws, dev);
    let v = ws.fx.bloom.view(dev.id);
    let busy = v.export.is_some();
    let ready = v.exported.clone().filter(|e| e.genome == genome);
    let chip = div()
        .id(("bloom-export", dev.key(0x640)))
        .flex_none()
        .h(px(28.))
        .w_full()
        .px(px(SP3))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(content(0.05))
        .border_1()
        .border_color(border())
        .hover(|s| s.bg(content(0.1)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    match (busy, ready) {
        (true, _) => chip.child(mono("Rendering…", TEXT_XS, pal.muted)).into_any_element(),
        (false, Some(e)) => chip
            .border_color(accent_alpha(0.6))
            .child(icon(Icon::Waveform, 12., pal.accent))
            .child(label("Drag the sound", TEXT_SM, pal.text))
            .tooltip(widgets::tip("Drag — onto a track or a pad as a sample", None))
            .on_drag(DraggedSample(e.item), |d: &DraggedSample, _, _, cx| cx.new(|_| d.clone()))
            .into_any_element(),
        (false, None) => {
            let d = dev.clone();
            chip.child(icon(Icon::Download, 12., pal.muted))
                .child(label("Export sound", TEXT_SM, pal.text))
                .tooltip(widgets::tip(TIP_EXPORT, None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    view::start_export(this, &d);
                    cx.notify();
                }))
                .into_any_element()
        }
    }
}

/// The knobs of one group of the table, `PER_ROW` to a row.
fn knob_group(dev: &Dev, group: knobs::Group, cx: &mut Context<Workspace>) -> Option<Div> {
    let has = |id: u32| dev.info(id).is_some();
    let ids: Vec<u32> = knobs::of(group, has).map(|k| k.id).collect();
    if ids.is_empty() {
        return None;
    }
    let mut grid = div().w(px(PER_ROW as f32 * dial_w(KNOB) + (PER_ROW - 1) as f32 * SP1)).flex().flex_wrap().gap(px(SP1));
    for id in ids {
        grid = grid.child(dial(dev, id, KNOB, cx));
    }
    Some(div().flex().flex_col().gap(px(SP2)).child(section(group.title(), Some(group.tip()))).child(grid))
}

/// How a tonal kind plays, under its Play tab.
pub const TONAL_PLAY: &str = "Kit and Chromatic play the selected patch across the keyboard; Single plays it at its home note.";

/// The tabs under the stage.
pub const TABS: [(&str, &str); 3] = [("Genes", "Genes — every gene of the selected sound"), ("Play", "Play — how notes reach the sounds"), ("Tools", "Tools — match, Drum Rack, render")];

/// The tabs of a tonal kind (the same three, said for patches).
pub const TONAL_TABS: [(&str, &str); 3] = [("Genes", "Genes — the selected patch's oscillators, filter, envelopes, motion, FX"), ("Play", "Play — how notes reach the patch"), ("Tools", "Tools — render, open in Wavetable Synth")];

/// The tabs as a segmented control.
fn tab_strip(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let now = ws.fx.bloom.view(dev.id).stage.tab;
    let mut row = div().flex_none().p(px(2.)).flex().items_center().gap(px(2.)).rounded(px(r_md())).bg(content(0.06)).border_1().border_color(border());
    let tabs = if view::kind(dev).is_tonal() { &TONAL_TABS } else { &TABS };
    for (k, (name, tip)) in tabs.iter().enumerate() {
        let d = dev.clone();
        let on = k == now;
        row = row.child(
            div()
                .id(("bloom-tab", dev.key(0x650 | k)))
                .h(px(24.))
                .px(px(SP4))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .when(on, |d| d.bg(pal.surface).shadow_sm())
                .when(!on, |d| d.hover(|s| s.bg(content(0.06))))
                .child(label(*name, TEXT_SM, if on { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }))
                .tooltip(widgets::tip(tip, None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fx.bloom.view(d.id).stage.tab = k;
                    cx.notify();
                })),
        );
    }
    row
}

/// Whether the window is too narrow for the knob column (from the root's
/// width the last frame measured).
pub fn narrow(width: f32) -> bool {
    width > 0. && width < NARROW_W
}

/// What a tonal patch is like, as five small bars in its colour (in place
/// of the morph, which blends drum scenes), and how far it has grown.
fn character(dev: &Dev, x: &Patch, distance: f32, color: Hsla) -> Div {
    let pal = p();
    let mut rows = div().flex().flex_col().gap(px(SP1));
    for (k, (name, v)) in patch_view::character(x).into_iter().enumerate() {
        rows = rows.child(
            div()
                .id(("bloom-character", dev.key(0x6B0 | k)))
                .h(px(12.))
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().w(px(44.)).child(label(name, TEXT_XS, pal.muted)))
                .child(div().flex_1().h(px(4.)).rounded(px(2.)).bg(content(0.08)).child(div().h_full().w(gpui::relative(v.clamp(0.02, 1.))).rounded(px(2.)).bg(color)))
                .child(div().w(px(28.)).flex().justify_end().child(mono(format!("{:.0}", v * 100.), TEXT_XS, pal.faint)))
                .tooltip(widgets::tip_owned(format!("{name} — {:.0} %", v * 100.))),
        );
    }
    rows.child(div().pt(px(SP1)).flex().items_center().gap(px(SP2)).child(label("Grown", TEXT_XS, pal.muted)).child(div().flex_1()).child(mono(format!("{distance:.2} from its core"), TEXT_XS, pal.faint)))
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let t0 = std::time::Instant::now();
    let pal = p();
    let scene = view::shown(ws, dev);
    let cell = ws.fx.bloom.view(dev.id).dna_cell.unwrap_or_else(|| dev.choice(p::SELECTED)).min(SOUNDS - 1);
    let looks = view::looks(ws, dev, &scene);
    let cell_color = stage::colour(&looks[cell]);
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let sel_color = stage::colour(&looks[selected]);
    let names = view::names(ws, dev, &scene);
    let tonal = view::kind(dev).is_tonal();
    let shown_patches = tonal.then(|| view::shown_patches(ws, dev));
    let root_region = ws.fx.region(dev.id, 20);
    let width = f32::from(root_region.get().size.width);
    let narrow = narrow(width);

    // Left: the generator and the selected sound, then the morph (drums) or
    // the patch's character (tonal kinds, which do not morph).
    let sel_patch = shown_patches.as_ref().map(|ps| ps.patches[selected]);
    let lower: (Stateful<Div>, Div) = match &shown_patches {
        Some(ps) => (section("CHARACTER", Some("Character — what the selected patch is like")), character(dev, &ps.patches[selected], ps.distances[selected], sel_color)),
        None => (section("MORPH", Some(TIP_MORPH)), morph(ws, dev, 92., cx)),
    };
    let left = card()
        .flex_none()
        .w(px(SIDE_W))
        .child(section("GENERATE", None))
        .child(generator(ws, dev, cx))
        .child(section("SELECTED SOUND", None))
        .child(selected_sound(dev, &scene, sel_patch.as_ref(), selected, names[selected].clone(), view::locked(ws, dev, &scene, selected), sel_color, cx))
        .child(lower.0)
        .child(lower.1)
        .child(div().flex_1())
        .child(export_chip(ws, dev, cx));

    // Middle: the stage, inset in a bezel whose colour the panel's corners take, and the pads.
    let bezel = pal.surface;
    let st = stage::stage(ws, dev, &scene, stage::Opts { swatches: false, outer: bezel, slot: 0, zoom: 1.0 }, cx);
    let hero = div()
        .id(("bloom-hero", dev.key(0x660)))
        .relative()
        .flex_1()
        .min_h(px(240.))
        .p(px(SP1))
        .rounded(px(r_lg()))
        .bg(bezel)
        .border_1()
        .border_color(border())
        .child(st)
        .tooltip(widgets::tip(if tonal { TIP_STAGE_TONAL } else { TIP_STAGE }, None));
    let columns = if narrow { 1. } else { 2. };
    let stage_w = if width > 0. { width - 2. * SP4 - columns * (SIDE_W + SP3) } else { 560. };
    let cols = if pads::rows(stage_w) == 1 { SOUNDS } else { SOUNDS / 2 };
    let middle = div().flex_1().min_w(px(360.)).flex().flex_col().gap(px(SP2)).child(hero).child(pads::pads(ws, dev, &scene, &looks, cols, cx));

    // Right: every knob, grouped.
    let groups: Vec<Div> = knobs::Group::ALL.into_iter().filter_map(|gr| knob_group(dev, gr, cx)).collect();
    let mut main = div().flex_1().min_h(px(MAIN_MIN_H)).flex().gap(px(SP3)).child(left).child(middle);
    let mut below = None;
    if narrow {
        below = Some(div().flex_none().flex().flex_wrap().gap(px(SP3)).children(groups.into_iter().map(|g| card().child(g))));
    } else {
        main = main.child(card().id(("bloom-knob-column", dev.key(0x6A1))).flex_none().w(px(SIDE_W)).overflow_y_scroll().children(groups));
    }

    // Tabs: the genes of the selected sound, the morph, the play settings, the tools.
    let tab = ws.fx.bloom.view(dev.id).stage.tab.min(TABS.len() - 1);
    let body = match tab {
        0 if let Some(ps) = &shown_patches => patch_view::panel(ws, dev, ps, cell, &names[cell], cell_color, cx),
        0 => dna::panel(ws, dev, &scene, cell, cell_color, cx),
        1 => card()
            .child(section("PLAY", None))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap(px(SP4))
                    .child(div().flex().flex_col().gap(px(SP1)).child(label("Mode", TEXT_XS, pal.muted)).child(instr::chips(dev, p::MODE, cx)))
                    // Choke and Base lay out drum kits; a tonal patch plays chromatically.
                    .when(!tonal, |d| d.child(div().flex().flex_col().gap(px(SP1)).child(label("Choke", TEXT_XS, pal.muted)).child(instr::chips(dev, p::CHOKE, cx))).child(dial(dev, p::BASE, KNOB, cx)))
                    .child(dial(dev, p::SELECTED, KNOB, cx))
                    .when(tonal, |d| d.child(dial(dev, p::GLIDE, KNOB, cx)).child(dial(dev, p::VELOCITY, KNOB, cx)).child(label(TONAL_PLAY, TEXT_XS, pal.faint))),
            )
            .into_any_element(),
        _ => card().child(section("TOOLS", None)).child(tools(ws, dev, cx)).into_any_element(),
    };
    // Frame costs, only with `AITRACK_FPS` (the editor's build is last frame's).
    let timing = crate::fps::enabled().then(|| {
        let st = &ws.fx.bloom.view(dev.id).stage;
        let (stage_ms, build_ms) = (st.cost.get(), st.build_ms);
        let gpu_ms = if stage::available() == Some(true) { matter::service::service().cost_ms() } else { 0. };
        mono(format!("editor {build_ms:.2} ms · stage {stage_ms:.2} ms · GPU {gpu_ms:.2} ms"), TEXT_XS, pal.faint)
    });
    let store = root_region.clone();
    kit::need(720., min_height(narrow));
    let editor = instr::root(720.)
        .relative()
        .child(canvas(move |b, _, _| store.set(b), |_, _, _, _| {}).absolute().size_full())
        .child(header(ws, dev, &scene, cx))
        .child(main)
        .children(below)
        .child(div().flex_none().flex().items_center().gap(px(SP3)).child(tab_strip(ws, dev, cx)).child(div().flex_1()).children(timing))
        .child(div().flex_none().h(px(TAB_H)).child(body))
        .into_any_element();
    ws.fx.bloom.view(dev.id).stage.build_ms = t0.elapsed().as_secs_f32() * 1000.;
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_rise_hold_and_fall_to_minus_40_db() {
        let pts = envelope_points(10.0, 5.0, 100.0, 200.0, 200);
        assert_eq!(pts.len(), 201);
        assert_eq!(pts[0].1, 0.0);
        let at = |t: f32| pts.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())).unwrap().1;
        assert!((at(5.0) - 0.5).abs() < 0.02);
        assert_eq!(at(12.0), 1.0);
        assert!((at(115.0) - 0.01).abs() < 0.002, "−40 dB at the decay time");
        assert!(pts.windows(2).skip(20).all(|w| w[1].1 <= w[0].1 + 1e-6), "falls after the hold");
    }

    #[test]
    fn the_layout_sits_on_the_spacing_grid() {
        for v in [SIDE_W, KNOB, NARROW_W, TAB_H, HEADER_H, MAIN_MIN_H, BELOW_H, 36., 28., 24., 56., 92., 156., 172., 80., 40.] {
            assert_eq!(v % 4., 0., "{v}");
        }
        // The window's default editor size holds the wide layout without scrolling.
        let (_, h) = crate::views::device_window::placement::editor_extent(aitrack_stock::instrument::bloom::ID);
        assert!(min_height(false) <= h, "{} > {h}", min_height(false));
        assert!(min_height(true) > min_height(false));
    }

    #[test]
    fn a_group_of_knobs_fits_its_column() {
        // Three knob cells and their gaps inside a side card's padding.
        let row = PER_ROW as f32 * dial_w(KNOB) + (PER_ROW - 1) as f32 * SP1;
        assert!(row <= SIDE_W - 2. * SP3, "{row}");
        assert!(narrow(900.) && !narrow(1200.) && !narrow(0.), "unmeasured is wide");
        assert!(!super::super::KNOB_HINT.contains('\n'));
    }
}
