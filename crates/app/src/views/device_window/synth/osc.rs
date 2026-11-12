//! Oscillator panels: the table as a stack of frames with the playing
//! position lit (drag across it = position; drop a WAV on it to import),
//! the wavetable menu, and the oscillator's knobs. Plus sub and noise.

use super::geom::{stack_at, stack_frame, stack_indices};
use super::knob::{chip_cell, mknob};
use super::{SynthPop, panel_with, pop_chip, view};
use crate::icons::Icon;
use crate::theme::*;
use crate::views::device_ui::kit::{self, Handle, PlotCfg, Rect, ax};
use crate::views::device_ui::wavetable::{self as wt, Preview, TableKey};
use crate::views::device_ui::Dev;
use crate::widgets;
use crate::workspace::Workspace;
use aitrack_stock::instrument::wavetable::WARPS;
use aitrack_stock::instrument::wavetable::modulation::ModSlot;
use aitrack_stock::instrument::wavetable::p;
use gpui::{Context, Div, ExternalPaths, Hsla, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, TextAlign, Window, div, prelude::FluentBuilder, px};
use std::rc::Rc;

pub const OSC_W: f32 = 300.;
const KNOB: f32 = 40.;

/// Paint a table as stacked frames with the frame at `pos` drawn bright.
pub fn paint_stack(window: &mut Window, r: Rect, pv: &Preview, pos: f32, color: Hsla, level: f32, max: usize) {
    let pal = p();
    if pv.frames.len() == 1 {
        // A single cycle: no stack, the whole plot.
        let inner = r.inset(8.);
        crate::views::device_ui::wavetable::paint_table(window, inner, Some(pv), 0., color, level);
        return;
    }
    let idx = stack_indices(pv.frames.len(), max);
    let n = idx.len();
    let scale = 1. / pv.peak();
    let pts = 96;
    let line_of = |x0: f32, base: f32, w: f32, amp: f32, f: &dyn Fn(f32) -> f32| -> Vec<gpui::Point<gpui::Pixels>> {
        (0..=pts).map(|j| {
            let t = j as f32 / pts as f32;
            gpui::point(px(r.x + x0 + w * t), px(r.y + base - amp * f(t) * scale))
        }).collect()
    };
    // Back to front.
    for k in (0..n).rev() {
        let (x0, base, w, amp) = stack_frame(r.w, r.h, k, n);
        let depth = if n > 1 { k as f32 / (n - 1) as f32 } else { 0. };
        let near = (1. - (depth - pos).abs() * 3.).max(0.);
        let frame = &pv.frames[idx[k]];
        let f = |t: f32| frame[((t * crate::views::device_ui::wavetable::PREVIEW_LEN as f32) as usize).min(frame.len() - 1)];
        kit::line(window, &line_of(x0, base, w, amp, &f), 1., with_alpha(color, 0.12 + 0.3 * near));
    }
    // The playing position, between frames.
    let (x0, base, w, amp) = stack_at(r.w, r.h, pos);
    let now = line_of(x0, base, w, amp, &|t| pv.sample(pos, t.min(0.9999)));
    kit::fill_to(window, &now, r.y + base, with_alpha(color, 0.1 + 0.25 * level));
    kit::line(window, &now, 2., color);
    kit::hline(window, Rect { x: r.x + x0, w, ..r }, r.y + base, with_alpha(pal.grid_bar, 0.6));
}

fn display(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> gpui::Stateful<Div> {
    let id = |f: u32| p::osc(n, f);
    let key = TableKey::of(dev, n);
    let pv = wt::preview(ws, key, cx);
    let (pos, level) = (wt::live_pos(dev, n), dev.plain(id(p::LEVEL)) / 100.);
    let knob_pos = dev.plain(id(p::POS)) / 100.;
    let warp = dev.choice(id(p::WARP));
    let warp_text = (warp > 0).then(|| format!("{} {:.0}%", WARPS.get(warp).copied().unwrap_or("Warp"), dev.plain(id(p::WARP_AMT))));
    let default = dev.info(id(p::POS)).map_or(0., |i| i.default_normalized());
    let handles = vec![Handle { fx: knob_pos.clamp(0.03, 0.97), fy: 0.93, x: ax(id(p::POS), 1.), index: n as usize, reset: vec![(id(p::POS), default)], ..Default::default() }];
    let color = if dev.enabled && level > 0. { series(n as usize) } else { p().muted };
    let device = dev.id;
    let frames_total = pv.as_ref().map_or(0, |v| v.total);
    kit::plot(ws, dev, PlotCfg { slot: 20 + n as u8, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let inner = Rect { h: r.h - 18., ..r };
        match &pv {
            Some(pv) => paint_stack(window, inner, pv, pos, color, level, 22),
            None => kit::txt(window, cx, "Building table…", r.x + r.w / 2., r.y + r.h / 2. - 6., TEXT_SM, pal.faint, TextAlign::Center),
        }
        // Position bar.
        kit::bar(window, r.x + 8., r.py(0.93) - 1., r.w - 16., 2., 1., content(0.14));
        kit::bar(window, r.x + 8., r.py(0.93) - 1., (r.w - 16.) * knob_pos, 2., 1., with_alpha(color, 0.7));
        kit::dot(window, r.pt(knob_pos.clamp(0.03, 0.97), 0.93), 5., color, Some(pal.text));
        let frame = if frames_total > 1 { format!("frame {} / {frames_total}", (pos * (frames_total - 1) as f32).round() as usize + 1) } else { "single cycle".to_owned() };
        kit::txt(window, cx, &frame, r.right() - 8., r.y + 6., TEXT_XS, pal.faint, TextAlign::Right);
        if let Some(w) = &warp_text {
            kit::txt(window, cx, w, r.x + 8., r.y + 6., TEXT_XS, with_alpha(color, 0.9), TextAlign::Left);
        }
    })
    .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(accent_alpha(0.8)).bg(accent_alpha(0.08)))
    .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
        if let Some(path) = paths.paths().first() {
            super::import_wav(this, device, n, path.clone(), cx);
        }
        cx.stop_propagation();
    }))
}

/// Oscillator `n`'s panel.
pub fn osc(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f: u32| p::osc(n, f);
    let device = dev.id;
    let name = wt::table_name(ws, dev, n);
    let busy = ws.fx.synth.get(&device).and_then(|v| v.busy.clone()).filter(|_| view(ws, device).edit_osc == n);
    let right = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(pop_chip(("osc-wave", dev.key(n as usize)), name, Some(148.), None, SynthPop::Wave { device, osc: n }, cx))
        .child(
            widgets::sized_icon_button(("osc-import", dev.key(n as usize)), Icon::FolderOpen, false, 20., 12.)
                .tooltip(widgets::tip("Import a WAV wavetable (or drop one on the display)", None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    view(this, device).edit_osc = n;
                    super::pick_wav(this, device, n, cx);
                })),
        )
        .child(
            widgets::sized_icon_button(("osc-edit", dev.key(n as usize)), Icon::Pencil, false, 20., 12.)
                .tooltip(widgets::tip("Edit this table in the wavetable editor", None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let v = view(this, device);
                    v.tab = super::Tab::Table;
                    v.edit_osc = n;
                    cx.notify();
                })),
        );
    let k = |ws: &mut Workspace, f: u32, cx: &mut Context<Workspace>| mknob(ws, dev, slots, id(f), KNOB, cx);
    panel_with(["OSC A", "OSC B", "OSC C"][n as usize], Some(series(n as usize)), right)
        .w(px(OSC_W))
        .child(div().h(px(150.)).child(display(ws, dev, n, cx)))
        .when_some(busy, |d, b| d.child(crate::widgets::label(b, TEXT_XS, p().faint)))
        .child(div().flex().justify_between().child(k(ws, p::POS, cx)).child(k(ws, p::WARP_AMT, cx)).child(k(ws, p::LEVEL, cx)).child(k(ws, p::PAN, cx)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, id(p::WARP), cx))
                .child(kit::pick(dev, id(p::ROUTE), cx))
                .child(div().flex_1())
                .child(kit::num_w(ws, dev, id(p::VOICES), 52., cx)),
        )
        .child(div().flex().justify_between().child(k(ws, p::DETUNE, cx)).child(k(ws, p::SPREAD, cx)).child(k(ws, p::BLEND, cx)).child(k(ws, p::RAND_PHASE, cx)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::num_w(ws, dev, id(p::OCTAVE), 52., cx))
                .child(kit::num_w(ws, dev, id(p::SEMI), 52., cx))
                .child(kit::num_w(ws, dev, id(p::FINE), 56., cx))
                .child(kit::num_w(ws, dev, id(p::PHASE), 56., cx)),
        )
}

/// Sub oscillator and noise.
pub fn sub_noise(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, cx: &mut Context<Workspace>) -> Div {
    let k = |ws: &mut Workspace, id: u32, cx: &mut Context<Workspace>| mknob(ws, dev, slots, id, 36., cx);
    let sub = super::panel("SUB", Some(p().muted))
        .w(px(208.))
        .child(div().flex().gap(px(SP2)).child(k(ws, p::SUB_LEVEL, cx)).child(k(ws, p::SUB_PAN, cx)))
        .child(div().flex().flex_wrap().gap(px(SP1)).child(chip_cell(dev, p::SUB_SHAPE, 60., cx)).child(chip_cell(dev, p::SUB_OCTAVE, 60., cx)).child(chip_cell(dev, p::SUB_ROUTE, 64., cx)));
    let noise = super::panel("NOISE", Some(p().muted))
        .w(px(208.))
        .child(div().flex().gap(px(SP2)).child(k(ws, p::NOISE, cx)).child(k(ws, p::NOISE_PAN, cx)))
        .child(div().flex().flex_wrap().gap(px(SP1)).child(chip_cell(dev, p::NOISE_COLOR, 64., cx)).child(chip_cell(dev, p::NOISE_ROUTE, 64., cx)));
    div().flex_none().flex().flex_col().gap(px(SP3)).child(sub).child(noise)
}
