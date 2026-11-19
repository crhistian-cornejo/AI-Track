//! The patch view: the Genes tab of a tonal kind. The selected patch is a
//! full Wavetable Synth parameter set; this shows the part that makes it
//! what it is, in the gene editor's language (folding groups of small knobs
//! in the sound's colour): the oscillators (wave, position, level, unison
//! voices and detune), sub and noise, the filter, the amp and mod envelopes
//! as small curves with their knobs, the motion (LFOs and routes, a
//! summary) and the effects (on/off).
//!
//! Drag a knob (right or up raises it, Shift is fine; the patch previews
//! while you drag and is stored as one undo step on release), double-click
//! it for the synth's default; a switch steps on click (back with Alt), an
//! effect toggles. Edits re-base the cell's core (`tonal::replace`), so the
//! patch keeps its place on its growth path. Everything else opens in the
//! full Wavetable Synth ("Open in Wavetable Synth").

use super::dna::{GENE_KNOB, GENE_W, dragged, step_choice};
use super::view::{self, PatchDrag};
use super::{TIP_OPEN_WT, TIP_SAVE_WT, action};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::widgets::{self, knob, label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::ParamInfo;
use aitrack_stock::instrument::bloom::genome::SOUNDS;
use aitrack_stock::instrument::bloom::patch::{Patch, PatchScene, layout};
use aitrack_stock::instrument::bloom::{demo_note, tonal};
use aitrack_stock::instrument::wavetable::modulation::source_name;
use aitrack_stock::instrument::wavetable::{LEGACY_DESTS, LEGACY_SOURCES, TABLES, dest_name, p as w, waves};
use gpui::{
    AnyElement, Context, DispatchPhase, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    StatefulInteractiveElement, Styled, canvas, div, point, prelude::FluentBuilder, px,
};

/// One control of a group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Item {
    /// A knob over a synth parameter.
    Knob(u32),
    /// A switch stepping a stepped parameter.
    Switch(u32),
    /// An envelope curve (0 amp, 1 mod).
    Curve(u32),
    /// The motion summary (LFOs and routes).
    Routes,
    /// An effect that turns on and off (its mix or mode parameter).
    Fx(u32),
}

/// A group of the patch view: its title and its controls.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub title: &'static str,
    pub items: Vec<Item>,
}

/// The parameter holding oscillator `o`'s unison voices: the 16-voice
/// field when it is set, else the original 1..7 one.
pub fn voices_id(x: &Patch, o: u32) -> u32 {
    if x.choice(w::osc(o, w::VOICES)) > 0 { w::osc(o, w::VOICES) } else { w::osc(o, w::UNISON) }
}

/// Whether oscillator `o` sounds (above 5 %).
pub fn osc_on(x: &Patch, o: u32) -> bool {
    x.plain(w::osc(o, w::LEVEL)) > 5.0
}

/// A value as a knob cell shows it: counts (unison voices) as whole numbers.
pub fn value_text(inf: &ParamInfo, norm: f32) -> String {
    if inf.scale == aitrack_dsp::ParamScale::Stepped && inf.choices.is_empty() { format!("{:.0}", inf.to_plain(norm)) } else { inf.format(norm) }
}

/// The groups of `x` (a folded patch): every sounding oscillator (A always),
/// sub and noise, the filter, both envelopes, the motion, the effects.
pub fn groups(x: &Patch) -> Vec<Group> {
    let mut out = Vec::new();
    for (o, title) in [(0u32, "Osc A"), (1, "Osc B"), (2, "Osc C")] {
        if o == 0 || osc_on(x, o) {
            out.push(Group { title, items: vec![Item::Switch(w::osc(o, w::WAVE)), Item::Knob(w::osc(o, w::POS)), Item::Knob(w::osc(o, w::LEVEL)), Item::Knob(voices_id(x, o)), Item::Knob(w::osc(o, w::DETUNE))] });
        }
    }
    out.push(Group { title: "Sub · Noise", items: vec![Item::Knob(w::SUB_LEVEL), Item::Switch(w::SUB_OCTAVE), Item::Knob(w::NOISE), Item::Switch(w::NOISE_COLOR)] });
    out.push(Group {
        title: "Filter",
        items: vec![Item::Switch(w::FILTER_TYPE), Item::Switch(w::filt(0, w::F_MODEL)), Item::Knob(w::CUTOFF), Item::Knob(w::RESONANCE), Item::Knob(w::FILTER_ENV), Item::Knob(w::DRIVE)],
    });
    for (e, title) in [(0u32, "Amp env"), (1, "Mod env")] {
        out.push(Group { title, items: vec![Item::Curve(e), Item::Knob(w::env(e, w::E_ATTACK)), Item::Knob(w::env(e, w::E_DECAY)), Item::Knob(w::env(e, w::E_SUSTAIN)), Item::Knob(w::env(e, w::E_RELEASE))] });
    }
    out.push(Group { title: "Motion", items: vec![Item::Routes] });
    out.push(Group { title: "FX", items: vec![Item::Fx(w::DIST_MODE), Item::Fx(w::CHORUS_MIX), Item::Fx(w::DELAY_MIX), Item::Fx(w::REVERB_MIX)] });
    out
}

/// A synth parameter's description.
pub fn info(id: u32) -> Option<&'static ParamInfo> {
    Patch::slot(id).map(|s| &layout().infos[s])
}

/// The name a switch shows: the wavetable's (its original table when the
/// bank field is 0), else the choice's.
pub fn choice_name(x: &Patch, id: u32) -> &'static str {
    let k = x.choice(id);
    let is_wave = (0..3u32).find(|&o| w::osc(o, w::WAVE) == id);
    match is_wave {
        Some(o) if k == 0 => TABLES.get(x.choice(w::osc(o, w::TABLE))).copied().unwrap_or("Table"),
        Some(_) => waves().get(k).copied().unwrap_or("—"),
        None => info(id).and_then(|i| i.choices.get(k).copied()).unwrap_or("—"),
    }
}

/// The next named choice of a switch from `now` (`back`: the previous one),
/// skipping the "—" padding of append-only lists.
pub fn next_choice(id: u32, now: usize, back: bool) -> usize {
    let Some(i) = info(id) else { return now };
    let n = (i.max - i.min).round().max(0.0) as usize + 1;
    let named = |k: usize| i.choices.get(k).is_none_or(|c| *c != "—") && (id != w::osc(0, w::WAVE) && id != w::osc(1, w::WAVE) && id != w::osc(2, w::WAVE) || waves().get(k).is_some_and(|c| *c != "—"));
    let mut k = now;
    for _ in 0..n {
        k = step_choice(k, n, back);
        if named(k) {
            return k;
        }
    }
    now
}

/// An effect's state: on, and its amount as text.
pub fn fx_state(x: &Patch, id: u32) -> (bool, String) {
    match id {
        w::DIST_MODE => {
            let on = x.choice(w::DIST_MODE) > 0;
            (on, if on { format!("{} {:.0}%", choice_name(x, w::DIST_MODE), x.plain(w::DIST_DRIVE)) } else { "Off".into() })
        }
        _ => {
            let v = x.plain(id);
            (v > 0.5, if v > 0.5 { format!("{v:.0}%") } else { "Off".into() })
        }
    }
}

/// The edits that toggle an effect: off sets its mix (or mode) to zero; on
/// brings it back at a moderate amount.
pub fn fx_toggle(x: &Patch, id: u32) -> Vec<(u32, f32)> {
    let (on, _) = fx_state(x, id);
    let norm = |id: u32, plain: f32| info(id).map_or(0.0, |i| i.to_normalized(plain));
    match (id, on) {
        (w::DIST_MODE, true) => vec![(w::DIST_MODE, 0.0)],
        (w::DIST_MODE, false) => vec![(w::DIST_MODE, norm(w::DIST_MODE, 1.0)), (w::DIST_DRIVE, norm(w::DIST_DRIVE, x.plain(w::DIST_DRIVE).max(30.0))), (w::DIST_MIX, norm(w::DIST_MIX, 100.0))],
        (_, true) => vec![(id, 0.0)],
        (_, false) => vec![(id, norm(id, 25.0))],
    }
}

/// One modulation route in words.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub source: String,
    pub dest: &'static str,
    /// Percent, signed.
    pub amount: f32,
}

/// An LFO in words: its shape and rate (or division when synced).
pub fn lfo_words(x: &Patch, n: u32) -> String {
    let shape = info(w::lfo(n, w::LFO_SHAPE)).and_then(|i| i.choices.get(x.choice(w::lfo(n, w::LFO_SHAPE))).copied()).unwrap_or("LFO");
    let rate = if x.choice(w::lfo(n, w::LFO_SYNC)) == 1 {
        info(w::lfo(n, w::LFO_DIVISION)).and_then(|i| i.choices.get(x.choice(w::lfo(n, w::LFO_DIVISION))).copied()).unwrap_or("sync").to_owned()
    } else {
        let hz = x.plain(w::lfo(n, w::LFO_RATE));
        if hz < 1.0 { format!("{hz:.2} Hz") } else { format!("{hz:.1} Hz") }
    };
    format!("{shape} {rate}")
}

/// Every route that moves the patch (both matrices, amounts over 0.5 %),
/// the LFOs described.
pub fn routes(x: &Patch) -> Vec<Route> {
    let mut out = Vec::new();
    let describe = |name: &str, lfo: Option<u32>| match lfo {
        Some(n) => format!("{name} · {}", lfo_words(x, n)),
        None => name.to_owned(),
    };
    for s in 0..8u32 {
        let (src, dst, amt) = (x.choice(w::slot(s, w::MOD_SRC)), x.choice(w::slot(s, w::MOD_DST)), x.plain(w::slot(s, w::MOD_AMT)));
        if src == 0 || dst == 0 || amt.abs() < 0.5 {
            continue;
        }
        let lfo = match src {
            1 => Some(0),
            2 => Some(1),
            _ => None,
        };
        out.push(Route { source: describe(LEGACY_SOURCES.get(src).copied().unwrap_or("?"), lfo), dest: LEGACY_DESTS.get(dst).copied().unwrap_or("?"), amount: amt });
    }
    for s in 0..32u32 {
        let src = x.plain(w::slot_x(s, w::M_SRC)).round() as u8;
        let dst = x.plain(w::slot_x(s, w::M_DST)).round() as u32;
        let amt = x.plain(w::slot_x(s, w::M_AMT));
        if src == 0 || dst == 0 || amt.abs() < 0.5 {
            continue;
        }
        let lfo = (4..=7).contains(&src).then(|| u32::from(src - 4));
        out.push(Route { source: describe(source_name(src), lfo), dest: dest_name(dst), amount: amt });
    }
    out
}

/// The points of an attack–decay–sustain–release envelope (ms; levels
/// 0..1): linear attack, the decay and release falling exponentially (to
/// −40 dB over their time), the key held for `gate` ms (at least the
/// attack and decay). `n + 1` points over the whole.
pub fn adsr_points(a: f32, d: f32, s: f32, r: f32, gate: f32, n: usize) -> (Vec<(f32, f32)>, f32) {
    let (a, d, r, s) = (a.max(0.1), d.max(1.0), r.max(1.0), s.clamp(0.0, 1.0));
    let gate = gate.max(a + d * 0.5);
    let total = gate + r;
    let at_gate = |t: f32| if t < a { t / a } else { s + (1.0 - s) * (-4.6 * (t - a) / d).exp() };
    let off = at_gate(gate);
    let pts = (0..=n)
        .map(|k| {
            let t = k as f32 / n as f32 * total;
            let v = if t <= gate { at_gate(t) } else { off * (-4.6 * (t - gate) / r).exp() };
            (t, v)
        })
        .collect();
    (pts, total)
}

/// The amp or mod envelope (`e` 0 / 1) of `x` as [`adsr_points`], the key
/// held as long as the kind's demo note.
pub fn envelope(x: &Patch, e: u32, n: usize) -> (Vec<(f32, f32)>, f32) {
    let hold = demo_note(x.kind).1 * 1000.0;
    adsr_points(x.plain(w::env(e, w::E_ATTACK)), x.plain(w::env(e, w::E_DECAY)), x.plain(w::env(e, w::E_SUSTAIN)) / 100.0, x.plain(w::env(e, w::E_RELEASE)), hold, n)
}

/// A small envelope plot filled in `color`.
pub fn curve(pts: Vec<(f32, f32)>, total: f32, color: Hsla) -> AnyElement {
    let grid = p().grid_beat;
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let (x0, y0, w, hh) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
            let y = |v: f32| y0 + hh - 2. - v * (hh - 6.);
            crate::paint::polyline(window, &[point(px(x0), px(y(0.))), point(px(x0 + w), px(y(0.)))], 1., grid);
            let line: Vec<_> = pts.iter().map(|(t, v)| point(px(x0 + t / total.max(1.0) * w), px(y(*v)))).collect();
            let mut area = line.clone();
            area.push(point(px(x0 + w), px(y(0.))));
            area.push(point(px(x0), px(y(0.))));
            crate::paint::polygon(window, &area, with_alpha(color, 0.2));
            crate::paint::polyline(window, &line, 1.5, color);
        },
    )
    .size_full()
    .into_any_element()
}

/// Milliseconds as the envelopes read them.
fn ms(v: f32) -> String {
    if v >= 1_000.0 { format!("{:.1} s", v / 1_000.0) } else { format!("{v:.0} ms") }
}

/// The cell under a knob or switch: its short name, the control, its value.
fn cell(name: &str, control: AnyElement, value: Option<String>) -> AnyElement {
    let pal = p();
    div()
        .w(px(GENE_W))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(label(crate::paint::fit(name, GENE_W, TEXT_XS), TEXT_XS, pal.muted))
        .child(control)
        .child(match value {
            Some(v) => mono(crate::paint::fit(&v, GENE_W, TEXT_XS), TEXT_XS, pal.text).into_any_element(),
            None => div().h(px(14.)).into_any_element(),
        })
        .into_any_element()
}

fn knob_cell(dev: &Dev, x: &Patch, cell_i: usize, id: u32, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(inf) = info(id) else { return div().into_any_element() };
    let value = x.get(id);
    let d = dev.clone();
    let default = inf.default_normalized();
    let k = knob(("bloom-patch-knob", dev.key(id as usize)), value, inf.min < 0. && inf.max > 0., GENE_KNOB, color)
        .tooltip(widgets::tip_owned(format!("{} — drag · double-click resets", inf.name)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if e.click_count >= 2 {
                    view::set_patch_params(this, &d, cell_i, &[(id, default)], Some("Edit patch"));
                } else {
                    let v0 = view::patches(this, &d).patches[cell_i].get(id);
                    this.fx.bloom.view(d.id).patch_drag = Some(PatchDrag { cell: cell_i, id, x0: f32::from(e.position.x), y0: f32::from(e.position.y), v0 });
                }
                cx.notify();
            }),
        );
    cell(inf.short, k.into_any_element(), Some(value_text(inf, value)))
}

fn switch_cell(dev: &Dev, x: &Patch, cell_i: usize, id: u32, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(inf) = info(id) else { return div().into_any_element() };
    let now = x.choice(id);
    let d = dev.clone();
    let name = choice_name(x, id);
    let switch = div()
        .id(("bloom-patch-switch", dev.key(id as usize)))
        .h(px(GENE_KNOB))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(with_alpha(color, 0.14))
        .border_1()
        .border_color(with_alpha(color, 0.4))
        .hover(|s| s.bg(content(0.1)))
        .child(label(crate::paint::fit(name, GENE_W - 8., TEXT_XS), TEXT_XS, pal.text))
        .tooltip(widgets::tip_owned(format!("{} — {name} · click: next · Alt-click: previous", inf.name)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            let k = next_choice(id, now, e.modifiers().alt);
            if let Some(i) = info(id) {
                view::set_patch_params(this, &d, cell_i, &[(id, i.to_normalized(i.min + k as f32))], Some("Edit patch"));
            }
            cx.notify();
        }));
    let short = if (0..3u32).any(|o| w::osc(o, w::WAVE) == id) { "Wave" } else { inf.short };
    cell(short, switch.into_any_element(), None)
}

fn fx_cell(dev: &Dev, x: &Patch, cell_i: usize, id: u32, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (on, text) = fx_state(x, id);
    let name = match id {
        w::DIST_MODE => "Drive",
        w::CHORUS_MIX => "Chorus",
        w::DELAY_MIX => "Delay",
        _ => "Reverb",
    };
    let edits = fx_toggle(x, id);
    let d = dev.clone();
    let chip = div()
        .id(("bloom-patch-fx", dev.key(id as usize)))
        .h(px(GENE_KNOB))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP1))
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { with_alpha(color, 0.22) } else { content(0.04) })
        .border_1()
        .border_color(if on { with_alpha(color, 0.6) } else { border() })
        .hover(|s| s.bg(content(0.1)))
        .child(div().size(px(6.)).rounded_full().bg(if on { color } else { content(0.2) }))
        .child(label(if on { "On" } else { "Off" }, TEXT_XS, if on { pal.text } else { pal.faint }))
        .tooltip(widgets::tip_owned(format!("{name} — {text} · click: turn {}", if on { "off" } else { "on" })))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            view::set_patch_params(this, &d, cell_i, &edits, Some(if on { "Effect off" } else { "Effect on" }));
            cx.notify();
        }));
    cell(name, chip.into_any_element(), Some(if on { text } else { String::new() }))
}

/// The envelope curve of a group, as wide as two cells.
fn curve_cell(dev: &Dev, x: &Patch, e: u32, color: Hsla) -> AnyElement {
    let pal = p();
    let (pts, total) = envelope(x, e, 48);
    let tip = format!(
        "{} — attack {} · decay {} · sustain {:.0}% · release {}",
        if e == 0 { "Amp envelope" } else { "Mod envelope" },
        ms(x.plain(w::env(e, w::E_ATTACK))),
        ms(x.plain(w::env(e, w::E_DECAY))),
        x.plain(w::env(e, w::E_SUSTAIN)),
        ms(x.plain(w::env(e, w::E_RELEASE)))
    );
    div()
        .id(("bloom-patch-env", dev.key(0xE00 | e as usize)))
        .w(px(2. * GENE_W))
        .h(px(56.))
        .rounded(px(r_sm()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .child(curve(pts, total, color))
        .tooltip(widgets::tip_owned(tip))
        .into_any_element()
}

/// The motion summary: up to four routes, the rest counted.
fn routes_cell(dev: &Dev, x: &Patch, color: Hsla) -> AnyElement {
    let pal = p();
    let list = routes(x);
    let mut col = div().id(("bloom-patch-routes", dev.key(0xE10))).w(px(ROUTES_W)).flex().flex_col().gap(px(2.));
    if list.is_empty() {
        col = col.child(label("Still — no LFO or envelope route", TEXT_XS, pal.faint));
    }
    for r in list.iter().take(4) {
        col = col.child(
            div()
                .h(px(12.))
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(div().w(px(4.)).h(px(4.)).rounded_full().bg(color))
                .child(label(crate::paint::fit(&r.source, 132., TEXT_XS), TEXT_XS, pal.text))
                .child(icon(Icon::ChevronRight, 8., pal.faint))
                .child(label(crate::paint::fit(r.dest, 92., TEXT_XS), TEXT_XS, pal.muted))
                .child(div().flex_1())
                .child(mono(format!("{:+.0}%", r.amount), TEXT_XS, pal.muted)),
        );
    }
    if list.len() > 4 {
        col = col.child(label(format!("+{} more in the Wavetable Synth", list.len() - 4), TEXT_XS, pal.faint));
    }
    let all = list.iter().map(|r| format!("{} → {} {:+.0}%", r.source, r.dest, r.amount)).collect::<Vec<_>>().join("\n");
    col.tooltip(widgets::tip_owned(if all.is_empty() { "Motion — nothing modulates this patch".into() } else { format!("Motion\n{all}") })).into_any_element()
}

/// The motion summary's width.
pub const ROUTES_W: f32 = 272.;

/// Window-wide listeners while a patch knob is dragged.
fn capture(dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let weak = cx.entity().downgrade();
    let d = dev.clone();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let (wk, dd) = (weak.clone(), d.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = wk.update(cx, |this, cx| {
                    let Some(g) = this.fx.bloom.view(dd.id).patch_drag else { return };
                    let value = dragged(g.v0, f32::from(e.position.x) - g.x0, f32::from(e.position.y) - g.y0, e.modifiers.shift);
                    view::set_patch_params(this, &dd, g.cell, &[(g.id, value)], None);
                    cx.notify();
                });
            });
            let (wk, dd) = (weak.clone(), d.clone());
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = wk.update(cx, |this, cx| {
                    let v = this.fx.bloom.view(dd.id);
                    if v.patch_drag.take().is_some()
                        && let Some(ps) = v.tonal_preview.take()
                    {
                        view::commit_patches(this, &dd, &ps, "Edit patch");
                    }
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// One group: a title that folds it, its controls in a row.
#[allow(clippy::too_many_arguments)]
fn group(dev: &Dev, x: &Patch, cell_i: usize, k: usize, g: &Group, folded: bool, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let d = dev.clone();
    let head = div()
        .id(("bloom-patch-group", dev.key(0x6C0 | k)))
        .flex()
        .items_center()
        .gap(px(SP1))
        .cursor_pointer()
        .child(icon(if folded { Icon::ChevronRight } else { Icon::ChevronDown }, 10., pal.faint))
        .child(label(g.title.to_uppercase(), TEXT_XS, if folded { pal.faint } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
        .tooltip(widgets::tip(if folded { "Show these" } else { "Fold these" }, None))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.fx.bloom.view(d.id).stage.patch_folded ^= 1 << k;
            cx.notify();
        }));
    let mut body = div().flex().items_start().gap(px(SP1));
    if !folded {
        for item in &g.items {
            body = body.child(match *item {
                Item::Knob(id) => knob_cell(dev, x, cell_i, id, color, cx),
                Item::Switch(id) => switch_cell(dev, x, cell_i, id, color, cx),
                Item::Fx(id) => fx_cell(dev, x, cell_i, id, color, cx),
                Item::Curve(e) => curve_cell(dev, x, e, color),
                Item::Routes => routes_cell(dev, x, color),
            });
        }
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .p(px(SP2))
        .rounded(px(r_md()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(head)
        .when(!folded, |g| g.child(body))
}

/// The patch view of tonal cell `cell` of `ps` (the shown patches, so a
/// drag previews here too), named `name`, in its colour.
pub fn panel(ws: &mut Workspace, dev: &Dev, ps: &PatchScene, cell: usize, name: &str, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let cell = cell.min(SOUNDS - 1);
    let x = ps.patches[cell].folded();
    let folded = ws.fx.bloom.view(dev.id).stage.patch_folded;
    let (d1, d2) = (dev.clone(), dev.clone());
    let open = action(("bloom-open-wt", dev.key(0x6A2)), "Open in Wavetable Synth", Icon::OpenWindow, false, TIP_OPEN_WT, cx, move |ws, cx| view::open_in_wavetable(ws, &d1, cell, cx));
    let save = action(("bloom-save-wt", dev.key(0x6A0)), "Save as Wavetable preset", Icon::Download, false, TIP_SAVE_WT, cx, move |ws, cx| match view::save_patch_preset(ws, &d2, cell) {
        Ok(n) => ws.toast(format!("Saved {n} in Wavetable Synth presets"), cx),
        Err(e) => ws.toast(e, cx),
    });
    let header = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(div().size(px(10.)).rounded_full().bg(color))
        .child(label(format!("{:02} {name}", cell + 1), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
        .child(icon(super::kind_icon(ps.kind), 12., pal.muted))
        .child(label(format!("{} patch", ps.kind.label()), TEXT_XS, pal.faint))
        .child(mono(format!("· {:.2} from its core", ps.distances[cell]), TEXT_XS, pal.muted))
        .child(div().flex_1())
        .child(save)
        .child(open);
    let mut body = div().id(("bloom-patch", dev.key(0xD7B))).flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_wrap().content_start().items_start().gap(px(SP2));
    for (k, g) in groups(&x).iter().enumerate() {
        body = body.child(group(dev, &x, cell, k, g, folded & (1 << k) != 0, color, cx));
    }
    let dragging = ws.fx.bloom.view(dev.id).patch_drag.is_some();
    div()
        .relative()
        .size_full()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(header)
        .child(body)
        .when(dragging, |d| d.child(capture(dev, cx)))
        .into_any_element()
}

/// What a patch is like, 0..1 each, for the window's character bars:
/// brightness (the cutoff), width (unison voices and detune), motion (its
/// LFO routes), grit (distortion and filter drive), air (reverb and delay).
pub fn character(x: &Patch) -> [(&'static str, f32); 5] {
    let x = x.folded();
    let bright = info(w::CUTOFF).map_or(0.5, |i| i.to_normalized(x.plain(w::CUTOFF)));
    let width = (0..3u32)
        .filter(|&o| osc_on(&x, o))
        .map(|o| {
            let n = x.choice(w::osc(o, w::VOICES));
            let v = if n > 0 { n as f32 } else { x.plain(w::osc(o, w::UNISON)) };
            ((v - 1.0) / 6.0).clamp(0.0, 1.0) * (0.4 + 0.6 * x.plain(w::osc(o, w::DETUNE)) / 100.0)
        })
        .fold(0.0, f32::max);
    let dist = if x.choice(w::DIST_MODE) > 0 { x.plain(w::DIST_DRIVE) * x.plain(w::DIST_MIX) / 100.0 } else { 0.0 };
    let grit = ((dist + 0.5 * x.plain(w::DRIVE)) / 100.0).clamp(0.0, 1.0);
    let air = ((x.plain(w::REVERB_MIX) + 0.6 * x.plain(w::DELAY_MIX)) / 60.0).clamp(0.0, 1.0);
    [("Bright", bright), ("Width", width), ("Motion", tonal::motion(&x)), ("Grit", grit), ("Air", air)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::instrument::bloom::patch::Kind;

    #[test]
    fn every_control_names_a_synth_parameter() {
        for k in Kind::TONAL {
            for patch in &tonal::generate(42, k).patches {
                let x = patch.folded();
                let gs = groups(&x);
                assert!(gs.iter().any(|g| g.title == "Osc A") && gs.iter().any(|g| g.title == "Filter"));
                for g in &gs {
                    for it in &g.items {
                        if let Item::Knob(id) | Item::Switch(id) | Item::Fx(id) = it {
                            assert!(info(*id).is_some(), "{k:?} {}: {id}", g.title);
                        }
                    }
                }
                assert!(gs.len() <= 16, "a fold bit per group");
                for id in (0..3u32).map(|o| w::osc(o, w::WAVE)).chain([w::FILTER_TYPE, w::NOISE_COLOR]) {
                    assert_ne!(choice_name(&x, id), "—", "{k:?} {id}");
                }
            }
        }
    }

    #[test]
    fn switches_skip_the_padding() {
        let wave = w::osc(0, w::WAVE);
        let n = waves().len();
        // Back from the first named entry wraps to "User" (63), over the "—" padding.
        let back = next_choice(wave, 0, true);
        assert_eq!(waves()[back], "User");
        assert!((0..n).all(|k| waves()[next_choice(wave, k, false)] != "—"));
        let model = w::filt(0, w::F_MODEL);
        let names = info(model).unwrap().choices;
        assert!((0..16).all(|k| names[next_choice(model, k, false)] != "—"));
    }

    #[test]
    fn effects_toggle_both_ways() {
        let mut x = Patch::neutral(Kind::Lead);
        x.set_plain(w::REVERB_MIX, 0.0);
        assert!(!fx_state(&x, w::REVERB_MIX).0);
        for (id, v) in fx_toggle(&x, w::REVERB_MIX) {
            x.set(id, v);
        }
        assert!(fx_state(&x, w::REVERB_MIX).0);
        for (id, v) in fx_toggle(&x, w::DIST_MODE) {
            x.set(id, v);
        }
        assert!(fx_state(&x, w::DIST_MODE).0 && x.plain(w::DIST_MIX) > 99.0);
        for (id, v) in fx_toggle(&x, w::DIST_MODE) {
            x.set(id, v);
        }
        assert!(!fx_state(&x, w::DIST_MODE).0);
    }

    #[test]
    fn pads_move_and_their_routes_read_in_words() {
        // The rules give every pad a slow movement route.
        let s = tonal::generate(1_337, Kind::Pad);
        assert!(s.patches.iter().all(|p| !routes(&p.folded()).is_empty()));
        let r = &routes(&s.patches[0].folded())[0];
        assert!(!r.source.is_empty() && r.dest != "None" && r.amount != 0.0, "{r:?}");
        assert!(lfo_words(&Patch::neutral(Kind::Pad), 0).contains("Hz"));
    }

    #[test]
    fn envelopes_rise_sustain_and_release() {
        let (pts, total) = adsr_points(100.0, 200.0, 0.5, 400.0, 1_000.0, 200);
        assert_eq!(total, 1_400.0);
        let at = |t: f32| pts.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())).unwrap().1;
        assert!((at(50.0) - 0.5).abs() < 0.03, "linear attack");
        assert!((at(900.0) - 0.5).abs() < 0.02, "sustains");
        assert!(at(1_400.0) < 0.01, "released");
        // A pluck (no sustain) falls before the key is up.
        let (p2, _) = adsr_points(1.0, 300.0, 0.0, 200.0, 250.0, 100);
        assert!(p2.windows(2).skip(2).all(|w| w[1].1 <= w[0].1 + 1e-6));
        // Every kind's envelopes draw.
        for k in Kind::TONAL {
            let x = tonal::generate(3, k).patches[0].folded();
            let (pts, total) = envelope(&x, 0, 32);
            assert!(total > 0.0 && pts.iter().all(|(t, v)| t.is_finite() && (0.0..=1.0001).contains(v)), "{k:?}");
        }
    }

    #[test]
    fn character_reads_the_kinds() {
        let mean = |k: Kind, i: usize| tonal::generate(9, k).patches.iter().map(|p| character(p)[i].1).sum::<f32>() / SOUNDS as f32;
        assert!(mean(Kind::Pad, 1) > mean(Kind::Keys, 1), "pads are wider than keys");
        assert!(mean(Kind::Pad, 4) > mean(Kind::Bass, 4), "pads sit in more air than basses");
        for k in Kind::TONAL {
            assert!(tonal::generate(9, k).patches.iter().all(|p| character(p).iter().all(|(_, v)| (0.0..=1.0).contains(v))));
        }
    }

    #[test]
    fn the_patch_view_sits_on_the_grid() {
        assert_eq!(ROUTES_W % 4., 0.);
    }
}
