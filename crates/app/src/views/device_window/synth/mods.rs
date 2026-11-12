//! Modulation: the macros, the envelope editors (drag the corners for the
//! times and sustain, the dots on the segments for their curves), the LFO
//! editors (click to add a breakpoint, drag to move it, drag a segment's dot
//! to bend it, double- or right-click a point to delete it; Shift snaps),
//! the routings of the selected source and the full matrix.

use super::geom::{self, EnvShape, LfoHit};
use super::knob::{clear_routing, mknob, source_handle};
use super::{ModTab, SynthDrag, SynthPop, commit_curve, panel_with, pop_chip, source_color, view};
use crate::icons::Icon;
use crate::theme::*;
use crate::views::device_ui::kit::{self, Handle, PlotCfg, Rect, ax};
use crate::views::device_ui::Dev;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_stock::instrument::wavetable::modulation::{LfoCurve, ModSlot, source_name};
use aitrack_stock::instrument::wavetable::{p, src};
use gpui::{
    Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled,
    TextAlign, div, prelude::FluentBuilder, px,
};
use std::rc::Rc;

/// The four macro knobs, each with its drag handle (top bar).
pub fn macros(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, cx: &mut Context<Workspace>) -> Div {
    let mut row = div().flex().items_center().gap(px(SP2));
    for m in 0..4u32 {
        let source = src::MACRO1 + m as u8;
        row = row.child(div().flex().items_center().gap(px(SP1)).child(mknob(ws, dev, slots, p::macro_knob(m), 32., cx)).child(source_handle(dev, source)));
    }
    row
}

fn env_shape(dev: &Dev, n: u32) -> EnvShape {
    let id = |f| p::env(n, f);
    let curve = |f| dev.plain(id(f)) / 100.;
    EnvShape {
        attack: dev.norm(id(p::E_ATTACK)),
        hold: dev.norm(id(p::E_HOLD)),
        decay: dev.norm(id(p::E_DECAY)),
        sustain: dev.plain(id(p::E_SUSTAIN)) / 100.,
        release: dev.norm(id(p::E_RELEASE)),
        curves: [curve(p::E_A_CURVE), curve(p::E_D_CURVE), curve(p::E_R_CURVE)],
    }
}

/// Envelope `n`'s editor plot.
fn env_plot(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> gpui::Stateful<Div> {
    let id = |f| p::env(n, f);
    let e = env_shape(dev, n);
    let c = e.corners();
    let def = |x: u32| (x, dev.info(x).map_or(0.5, |i| i.default_normalized()));
    let base = 100 + n as usize * 10;
    let band = geom::ENV_BOTTOM - geom::ENV_TOP;
    let mut handles = vec![
        Handle { fx: c[1].0, fy: c[1].1, x: ax(id(p::E_ATTACK), 1. / geom::A_W), alt: ax(id(p::E_A_CURVE), 0.8), index: base, reset: vec![def(id(p::E_ATTACK))], ..Default::default() },
        Handle { fx: c[2].0, fy: c[2].1, x: ax(id(p::E_HOLD), 1. / geom::H_W), index: base + 1, reset: vec![def(id(p::E_HOLD))], ..Default::default() },
        Handle {
            fx: c[3].0,
            fy: c[3].1,
            x: ax(id(p::E_DECAY), 1. / geom::D_W),
            y: ax(id(p::E_SUSTAIN), -1. / band),
            alt: ax(id(p::E_D_CURVE), 0.8),
            index: base + 2,
            reset: vec![def(id(p::E_DECAY)), def(id(p::E_SUSTAIN))],
        },
        Handle { fx: c[5].0, fy: c[5].1 - 0.04, x: ax(id(p::E_RELEASE), 1. / geom::R_W), alt: ax(id(p::E_R_CURVE), 0.8), index: base + 3, reset: vec![def(id(p::E_RELEASE))], ..Default::default() },
    ];
    for (seg, f) in [p::E_A_CURVE, p::E_D_CURVE, p::E_R_CURVE].into_iter().enumerate() {
        let (bx, by) = e.bend_handle(seg);
        handles.push(Handle { fx: bx, fy: by, y: ax(id(f), geom::bend_per_frac(seg) / 2.), index: base + 4 + seg, reset: vec![def(id(f))], ..Default::default() });
    }
    let marks: Vec<(f32, f32, bool)> = handles.iter().enumerate().map(|(i, h)| (h.fx, h.fy, i >= 4)).collect();
    let color = source_color(src::ENV1 + n as u8);
    let line = e.polyline(24);
    let live = (dev.m(0) >= 1.).then(|| dev.m(10 + n as usize));
    let times = [p::E_ATTACK, p::E_HOLD, p::E_DECAY, p::E_SUSTAIN, p::E_RELEASE].map(|f| dev.text(id(f)));
    kit::plot(ws, dev, PlotCfg { slot: 60 + n as u8, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(geom::ENV_BOTTOM), pal.grid_bar);
        kit::hline(window, r, r.py(geom::ENV_TOP), pal.grid_beat);
        for (x, _) in &c[1..] {
            kit::vline(window, r, r.px(*x), pal.grid_sub);
        }
        let pts: Vec<_> = line.iter().map(|(x, y)| r.pt(*x, *y)).collect();
        kit::fill_to(window, &pts, r.py(geom::ENV_BOTTOM), with_alpha(color, 0.16));
        kit::line(window, &pts, 2., color);
        if let Some(v) = live {
            let y = r.py(geom::ENV_BOTTOM - (geom::ENV_BOTTOM - geom::ENV_TOP) * v.clamp(0., 1.));
            kit::hline(window, r, y, with_alpha(color, 0.5));
        }
        for (x, y, bend) in &marks {
            if *bend {
                kit::dot(window, r.pt(*x, *y), 3.5, pal.well, Some(color));
            } else {
                kit::dot(window, r.pt(*x, *y), 5.5, color, Some(pal.text));
            }
        }
        let names = ["A", "H", "D", "S", "R"];
        let text = names.iter().zip(&times).map(|(n, t)| format!("{n} {t}")).collect::<Vec<_>>().join("   ");
        kit::txt(window, cx, &text, r.x + 8., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
    })
}

fn env_editor(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f| p::env(n, f);
    let k = |ws: &mut Workspace, f: u32, side: f32, cx: &mut Context<Workspace>| mknob(ws, dev, slots, id(f), side, cx);
    let note = ["Env 1 shapes every note's volume.", "Env 2 drives the filters' Env knobs; route it anywhere too.", "Env 3 is free: route it in the matrix."][n as usize];
    div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(div().h(px(150.)).child(env_plot(ws, dev, n, cx)))
        .child(
            div()
                .flex()
                .items_end()
                .gap(px(SP1))
                .child(k(ws, p::E_ATTACK, 36., cx))
                .child(k(ws, p::E_HOLD, 36., cx))
                .child(k(ws, p::E_DECAY, 36., cx))
                .child(k(ws, p::E_SUSTAIN, 36., cx))
                .child(k(ws, p::E_RELEASE, 36., cx))
                .child(div().w(px(SP2)))
                .child(k(ws, p::E_A_CURVE, 30., cx))
                .child(k(ws, p::E_D_CURVE, 30., cx))
                .child(k(ws, p::E_R_CURVE, 30., cx)),
        )
        .child(label(note, TEXT_XS, p().faint))
}

/// LFO `n`'s breakpoint editor.
fn lfo_plot(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> gpui::Stateful<Div> {
    let id = |f| p::lfo(n, f);
    let device = dev.id;
    let drawn = dev.on(id(p::LFO_DRAWN));
    let shape = aitrack_dsp::lfo::LfoShape::from_index(dev.choice(id(p::LFO_SHAPE)));
    let project = &ws.s.project;
    let curve = ws.fx.wave_previews.data(project, device).curve(n as usize);
    // While a point is dragged, draw the dragged points.
    let dragging = ws.fx.synth.get(&device).and_then(|v| match &v.drag {
        Some(SynthDrag::Lfo { lfo, points, index, .. }) if *lfo == n => Some((points.clone(), *index)),
        _ => None,
    });
    let points = dragging.as_ref().map_or_else(|| curve.points().to_vec(), |d| d.0.clone());
    let active = dragging.map(|d| d.1);
    let shown = LfoCurve::new(&points);
    let color = source_color(src::LFO1 + n as u8);
    let live = (dev.m(0) >= 1. || dev.m(6 + n as usize).abs() > 1e-4).then(|| dev.m(6 + n as usize));
    let region = ws.fx.region(device, 70 + n as u8);
    let store = region.clone();
    let pts_paint = points.clone();
    let surface = gpui::canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, cx| {
            let pal = p();
            let r = Rect::of(b);
            for i in 1..16 {
                kit::vline(window, r, r.px(i as f32 / 16.), if i % 4 == 0 { pal.grid_bar } else { pal.grid_sub });
            }
            kit::hline(window, r, r.py(0.5), pal.grid_beat);
            let to = |x: f32, y: f32| {
                let (fx, fy) = geom::lfo_to_plot(x, y);
                r.pt(fx, fy)
            };
            let line: Vec<_> = if drawn {
                (0..=240).map(|i| {
                    let x = i as f32 / 240.;
                    to(x, shown.eval(x))
                }).collect()
            } else {
                (0..=240).map(|i| {
                    let x = i as f32 / 240.;
                    to(x, crate::views::device_ui::chorus::lfo_value(shape, x) * 0.5 + 0.5)
                }).collect()
            };
            let mid = to(0., 0.5).y;
            kit::fill_to(window, &line, f32::from(mid), with_alpha(color, 0.14));
            kit::line(window, &line, 2., if drawn { color } else { with_alpha(color, 0.6) });
            if drawn {
                for i in 0..pts_paint.len() {
                    let (mx, my) = geom::segment_mid(&pts_paint, i);
                    kit::dot(window, to(mx, my), 3., pal.well, Some(with_alpha(color, 0.8)));
                }
                for (i, q) in pts_paint.iter().enumerate() {
                    kit::dot(window, to(q.x, q.y), if active == Some(i) { 6.5 } else { 5. }, color, Some(pal.text));
                }
            }
            if let Some(v) = live {
                let y = to(0., v * 0.5 + 0.5).y;
                kit::hline(window, r, f32::from(y), with_alpha(pal.text, 0.35));
            }
            let hint = if drawn { "click: add · drag: move · drag ◦: bend · double-click: delete · Shift: snap" } else { "click to draw your own shape" };
            kit::txt(window, cx, hint, r.right() - 8., r.y + 4., TEXT_XS, pal.faint, TextAlign::Right);
        },
    )
    .size_full();
    let r2 = region.clone();
    let down_points = points.clone();
    let press = cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        cx.stop_propagation();
        let r = Rect::of(r2.get());
        let (fx, fy) = ((f32::from(e.position.x) - r.x) / r.w.max(1.), (f32::from(e.position.y) - r.y) / r.h.max(1.));
        let snap = e.modifiers.shift;
        let mut pts = down_points.clone();
        let hit = if drawn { geom::lfo_hit(&pts, fx, fy, r.w, r.h, 9.) } else { LfoHit::Empty };
        let (index, bend) = match hit {
            LfoHit::Point(i) if e.click_count >= 2 => {
                commit_curve(this, device, n, LfoCurve::new(&geom::lfo_delete(&pts, i)), false);
                cx.notify();
                return;
            }
            LfoHit::Point(i) => (i, false),
            LfoHit::Bend(i) => (i, true),
            LfoHit::Empty => {
                let (x, y) = geom::plot_to_lfo(fx, fy);
                let Some((v, at)) = geom::lfo_insert(&pts, geom::snap(x, 16., snap), geom::snap(y, 8., snap)) else { return };
                pts = v;
                this.s.seal();
                commit_curve(this, device, n, LfoCurve::new(&pts), true);
                (at, false)
            }
        };
        if !matches!(hit, LfoHit::Empty) {
            this.s.seal();
        }
        view(this, device).drag = Some(SynthDrag::Lfo { lfo: n, index, bend, points: pts, region: r2.clone(), last_fy: fy, snap: false });
        cx.notify();
    });
    let r3 = region.clone();
    let right_points = points;
    let delete = cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        let r = Rect::of(r3.get());
        let (fx, fy) = ((f32::from(e.position.x) - r.x) / r.w.max(1.), (f32::from(e.position.y) - r.y) / r.h.max(1.));
        if let LfoHit::Point(i) = geom::lfo_hit(&right_points, fx, fy, r.w, r.h, 9.) {
            cx.stop_propagation();
            commit_curve(this, device, n, LfoCurve::new(&geom::lfo_delete(&right_points, i)), false);
            cx.notify();
        }
    });
    div()
        .id(("lfo-plot", dev.key(0xD00 | n as usize)))
        .size_full()
        .rounded(px(r_md()))
        .bg(p().well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .cursor_crosshair()
        .child(surface)
        .on_mouse_down(MouseButton::Left, press)
        .on_mouse_down(MouseButton::Right, delete)
}

fn lfo_editor(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f| p::lfo(n, f);
    let device = dev.id;
    let drawn = dev.on(id(p::LFO_DRAWN));
    let rate = if dev.on(id(p::LFO_SYNC)) { id(p::LFO_DIVISION) } else { id(p::LFO_RATE) };
    let rate_ctl = if dev.on(id(p::LFO_SYNC)) { kit::pick(dev, rate, cx) } else { mknob(ws, dev, slots, rate, 36., cx).into_any_element() };
    div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(div().h(px(150.)).child(lfo_plot(ws, dev, n, cx)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, id(p::LFO_DRAWN), cx))
                .when(!drawn, |d| d.child(kit::pick(dev, id(p::LFO_SHAPE), cx)))
                .child(pop_chip(("lfo-presets", dev.key(n as usize)), "Presets".to_owned(), Some(84.), None, SynthPop::LfoPresets { device, lfo: n }, cx))
                .child(kit::pick(dev, id(p::LFO_SYNC), cx))
                .child(kit::pick(dev, id(p::LFO_RETRIGGER), cx))
                .child(kit::pick(dev, id(p::LFO_ONESHOT), cx)),
        )
        .child(
            div()
                .flex()
                .items_end()
                .gap(px(SP1))
                .child(rate_ctl)
                .child(mknob(ws, dev, slots, id(p::LFO_PHASE), 36., cx))
                .child(mknob(ws, dev, slots, id(p::LFO_FADE), 36., cx)),
        )
}

/// The routings that start at `source`, with depth and delete.
fn source_routings(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, source: u8, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let mut list = div().w(px(236.)).flex().flex_col().gap(px(SP1)).child(label(format!("{} →", source_name(source)), TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD));
    let mine: Vec<(usize, ModSlot)> = slots.iter().copied().enumerate().filter(|(_, m)| m.is_active() && m.source == source).collect();
    if mine.is_empty() {
        list = list.child(label("Drag the handle onto a knob to route this source.", TEXT_XS, pal.faint));
    }
    for (s, m) in mine {
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(pop_chip(("rt-dst", dev.key(s)), super::dest_label(m.dest).to_owned(), Some(124.), None, SynthPop::Dest { device, slot: s }, cx))
                .child(kit::num_w(ws, dev, p::slot_x(s as u32, p::M_AMT), 56., cx))
                .child(
                    widgets::sized_icon_button(("rt-del", dev.key(s)), Icon::X, false, 20., 10.)
                        .tooltip(widgets::tip("Remove this routing", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            clear_routing(this, device, s);
                            cx.notify();
                        })),
                ),
        );
    }
    list
}

/// The envelope / LFO section of the Synth tab.
pub fn section(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let current = view(ws, device).mods;
    let tabs: Vec<(ModTab, &str, u8)> = vec![
        (ModTab::Env(0), "ENV 1", src::ENV1),
        (ModTab::Env(1), "ENV 2", src::ENV2),
        (ModTab::Env(2), "ENV 3", src::ENV3),
        (ModTab::Lfo(0), "LFO 1", src::LFO1),
        (ModTab::Lfo(1), "LFO 2", src::LFO2),
        (ModTab::Lfo(2), "LFO 3", src::LFO3),
        (ModTab::Lfo(3), "LFO 4", src::LFO4),
    ];
    let mut bar = div().flex().items_center().gap(px(SP1));
    let mut source = src::ENV1;
    for (k, (t, name, s)) in tabs.into_iter().enumerate() {
        let on = t == current;
        if on {
            source = s;
        }
        let color = source_color(s);
        let used = slots.iter().any(|m| m.is_active() && m.source == s);
        bar = bar.child(
            div()
                .id(("mod-tab", dev.key(k)))
                .h(px(28.))
                .pl(px(SP1))
                .pr(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP1))
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if on { with_alpha(color, 0.16) } else { content(0.04) })
                .hover(|st| st.bg(with_alpha(color, 0.22)))
                .child(source_handle(dev, s))
                .child(label(name, TEXT_SM, if on { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
                .when(used, |d| d.child(div().size(px(4.)).rounded_full().bg(color)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    view(this, device).mods = t;
                    cx.notify();
                })),
        );
    }
    let editor = match current {
        ModTab::Env(n) => env_editor(ws, dev, slots, n, cx),
        ModTab::Lfo(n) => lfo_editor(ws, dev, slots, n, cx),
    };
    div()
        .flex_1()
        .min_w(px(560.))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .p(px(SP3))
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(border())
        .child(bar)
        .child(div().flex().gap(px(SP3)).child(div().flex_1().min_w(px(0.)).child(editor)).child(source_routings(ws, dev, slots, source, cx)))
}

/// The modulation matrix. `rows` = 32 lists every slot; fewer lists the
/// active ones and one empty slot to fill.
pub fn matrix(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, rows: usize, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let head = |t: &'static str, w: f32| label(t, TEXT_XS, pal.faint).w(px(w)).font_weight(FontWeight::SEMIBOLD);
    let mut table = div()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(div().flex().gap(px(SP2)).pl(px(28.)).child(head("SOURCE", 156.)).child(head("DESTINATION", 196.)).child(head("AMOUNT", 64.)).child(head("POLARITY", 88.)).child(head("VIA", 156.)));
    let first_free = slots.iter().position(|m| !m.is_active());
    for (s, m) in slots.iter().enumerate() {
        let show = rows >= 32 || m.is_active() || Some(s) == first_free;
        if !show {
            continue;
        }
        let active = m.is_active();
        let color = source_color(m.source);
        let src_text = if m.source == 0 { "Choose a source".to_owned() } else { source_name(m.source).to_owned() };
        let dst_text = if m.dest == 0 { "Choose a destination".to_owned() } else { super::dest_label(m.dest).to_owned() };
        let via_text = if m.via == 0 { "—".to_owned() } else { source_name(m.via).to_owned() };
        table = table.child(
            div()
                .h(px(28.))
                .px(px(SP1))
                .flex()
                .items_center()
                .gap(px(SP2))
                .rounded(px(r_sm()))
                .bg(if active { with_alpha(color, 0.06) } else { gpui::transparent_black() })
                .when(!active, |d| d.opacity(0.75))
                .child(mono(format!("{:>2}", s + 1), TEXT_XS, pal.faint).w(px(20.)))
                .child(pop_chip(("mx-src", dev.key(s)), src_text, Some(156.), (m.source != 0).then_some(color), SynthPop::Source { device, slot: s, via: false }, cx))
                .child(pop_chip(("mx-dst", dev.key(s)), dst_text, Some(196.), None, SynthPop::Dest { device, slot: s }, cx))
                .child(kit::num_w(ws, dev, p::slot_x(s as u32, p::M_AMT), 64., cx))
                .child(div().w(px(88.)).child(kit::pick(dev, p::slot_x(s as u32, p::M_MODE), cx)))
                .child(pop_chip(("mx-via", dev.key(s)), via_text, Some(156.), None, SynthPop::Source { device, slot: s, via: true }, cx))
                .when(active || m.source != 0 || m.dest != 0, |d| {
                    d.child(
                        widgets::sized_icon_button(("mx-del", dev.key(s)), Icon::Trash, false, 20., 12.).tooltip(widgets::tip("Clear this slot", None)).on_click(cx.listener(move |this, _, _, cx| {
                            clear_routing(this, device, s);
                            cx.notify();
                        })),
                    )
                }),
        );
    }
    let count = slots.iter().filter(|m| m.is_active()).count();
    panel_with("MODULATION MATRIX", Some(pal.accent), label(format!("{count} of 32 slots · drag any source handle onto a knob to add one"), TEXT_XS, pal.faint)).child(table)
}
