//! The wavetable editor: the selected oscillator's table as a stack and a
//! frame strip, the selected frame large, and the tools that change it —
//! the pen (draw over the frame), harmonics (drag the bars), a formula in
//! `x` (phase) and `y` (frame) and morphs between the frames — plus
//! normalise, DC, reverse, add / delete frame, import and export. Results
//! become the oscillator's user table, one undo step per change; FFT and
//! table generation run on the background executor.

use super::geom::{self, FORMULAS, MORPH_COUNTS};
use super::osc::paint_stack;
use super::{SynthDrag, Tool, panel, panel_with, run_job, view};
use crate::icons::Icon;
use crate::theme::*;
use crate::views::device_ui::kit::{self, Rect};
use crate::views::device_ui::wavetable as wt;
use crate::views::device_ui::Dev;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_model::Device;
use aitrack_stock::instrument::wavetable::tablegen::{self, Morph, WaveFrames};
use gpui::{
    AnyElement, AppContext, Context, Div, ExternalPaths, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, TextAlign, div, prelude::FluentBuilder, px,
};
use std::rc::Rc;

fn chip(id: impl Into<gpui::ElementId>, text: &str, on: bool) -> gpui::Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(24.))
        .px(px(SP2))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { accent_alpha(0.2) } else { content(0.06) })
        .hover(|s| s.bg(if on { accent_alpha(0.28) } else { content(0.12) }))
        .child(label(text.to_owned(), TEXT_XS, if on { pal.accent } else { pal.text }).font_weight(FontWeight::MEDIUM))
}

/// An action button of the tool column.
fn action(id: impl Into<gpui::ElementId>, text: &str, icon: Option<Icon>) -> gpui::Stateful<Div> {
    widgets::text_button(id, text.to_owned(), icon, false)
}

fn osc_picker(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let device = dev.id;
    let now = view(ws, device).edit_osc;
    let mut row = div().flex().gap(px(SP1));
    for n in 0..3u32 {
        row = row.child(chip(("ed-osc", dev.key(n as usize)), ["OSC A", "OSC B", "OSC C"][n as usize], now == n).on_click(cx.listener(move |this, _, _, cx| {
            let v = view(this, device);
            v.edit_osc = n;
            v.frame = 0;
            cx.notify();
        })));
    }
    row
}

fn tool_picker(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let device = dev.id;
    let now = view(ws, device).tool;
    let mut row = div().flex().gap(px(SP1));
    for (k, (t, name)) in [(Tool::Draw, "Draw"), (Tool::Harmonics, "Harmonics"), (Tool::Formula, "Formula"), (Tool::Morph, "Morph")].into_iter().enumerate() {
        row = row.child(chip(("ed-tool", dev.key(k)), name, now == t).on_click(cx.listener(move |this, _, _, cx| {
            view(this, device).tool = t;
            cx.notify();
        })));
    }
    row
}

/// The tool-specific controls.
fn tool_panel(ws: &mut Workspace, dev: &Dev, base: &Option<Rc<WaveFrames>>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let (tool, osc, frame) = {
        let v = view(ws, device);
        (v.tool, v.edit_osc, v.frame)
    };
    let mut col = div().flex().flex_col().gap(px(SP2));
    let Some(base) = base.clone() else { return col.child(label("Loading the table…", TEXT_SM, pal.faint)) };
    match tool {
        Tool::Draw => {
            col = col.child(label("Draw on the frame, or start it from a shape:", TEXT_XS, pal.faint));
            let mut shapes = div().flex().flex_wrap().gap(px(SP1));
            for (k, name) in ["Sine", "Triangle", "Saw", "Square"].into_iter().enumerate() {
                let b = base.clone();
                shapes = shapes.child(chip(("ed-shape", dev.key(k)), name, false).on_click(cx.listener(move |this, _, _, cx| {
                    let mut w = (*b).clone();
                    let cycle = if name == "Sine" { (0..2048).map(|i| (i as f32 / 2048. * std::f32::consts::TAU).sin()).collect() } else { tablegen::draw(&geom::shape_points(name)) };
                    w.set_frame(frame, &cycle);
                    super::commit_table(this, device, osc, w, "Draw wavetable", cx);
                })));
            }
            col = col.child(shapes);
        }
        Tool::Harmonics => {
            col = col.child(label("Drag the bars: harmonics 1 – 32 (phases kept).", TEXT_XS, pal.faint));
        }
        Tool::Formula => {
            let input = {
                let v = view(ws, device);
                if v.formula.is_none() {
                    let e = cx.new(|cx| widgets::TextInput::new(cx, "sin(2*pi*x)*(1-y) + saw(x)*y", None));
                    v.formula = Some(e);
                }
                v.formula.clone()
            };
            let frames_now = view(ws, device).formula_frames;
            let mut counts = div().flex().gap(px(SP1)).child(label("Frames", TEXT_XS, pal.faint));
            for (k, c) in [1usize, 16, 64, 256].into_iter().enumerate() {
                counts = counts.child(chip(("ed-fcount", dev.key(k)), &c.to_string(), frames_now == c).on_click(cx.listener(move |this, _, _, cx| {
                    view(this, device).formula_frames = c;
                    cx.notify();
                })));
            }
            let mut presets = div().flex().flex_wrap().gap(px(SP1));
            for (k, (name, expr)) in FORMULAS.iter().enumerate() {
                let input = input.clone();
                presets = presets.child(chip(("ed-formula", dev.key(k)), name, false).on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(i) = &input {
                        i.update(cx, |i, cx| i.set_text(expr, cx));
                    }
                })));
            }
            let gen_input = input.clone();
            col = col
                .child(label("x = phase 0..1, y = frame 0..1. Functions: sin cos tan abs sqrt exp ln pow min max floor fract clamp tanh saw tri sqr noise", TEXT_XS, pal.faint))
                .when_some(input, |d, i| d.child(i))
                .child(presets)
                .child(counts)
                .child(action(("ed-gen", dev.key(0)), "Generate table", Some(Icon::Sparkles)).on_click(cx.listener(move |this, _, _, cx| {
                    let text = gen_input.as_ref().map(|i| i.read(cx).text().to_owned()).unwrap_or_default();
                    let expr = if text.trim().is_empty() { FORMULAS[0].1.to_owned() } else { text };
                    let frames = view(this, device).formula_frames;
                    run_job(this, device, osc, "Generate from formula", move || WaveFrames::from_formula("Formula", frames, &expr).map_err(|e| e.to_string()), cx);
                })));
        }
        Tool::Morph => {
            col = col.child(label("Treat the frames as key frames and fill the table between them.", TEXT_XS, pal.faint));
            for (m, name) in [(Morph::Spectral, "Spectral"), (Morph::Crossfade, "Crossfade")] {
                let mut row = div().flex().items_center().gap(px(SP1)).child(label(name, TEXT_XS, pal.text).w(px(64.)));
                for (k, count) in MORPH_COUNTS.into_iter().enumerate() {
                    let b = base.clone();
                    row = row.child(chip(("ed-morph", dev.key(k + if m == Morph::Spectral { 0 } else { 8 })), &count.to_string(), false).on_click(cx.listener(move |this, _, _, cx| {
                        let w = (*b).clone();
                        run_job(this, device, osc, "Morph frames", move || Ok(w.morph(count, m)), cx);
                    })));
                }
                col = col.child(row);
            }
        }
    }
    col
}

/// Table-wide operations.
fn table_ops(ws: &mut Workspace, dev: &Dev, base: &Option<Rc<WaveFrames>>, cx: &mut Context<Workspace>) -> Div {
    let device = dev.id;
    let (osc, frame) = {
        let v = view(ws, device);
        (v.edit_osc, v.frame)
    };
    let mut col = div().flex().flex_wrap().gap(px(SP1));
    let Some(base) = base.clone() else { return col };
    type Op = fn(&mut WaveFrames, usize);
    let ops: [(&str, &str, Op); 6] = [
        ("Normalize", "Normalize", |w, _| w.normalize(false)),
        ("Normalize frames", "Normalize frames", |w, _| w.normalize(true)),
        ("Remove DC", "Remove DC", |w, _| w.remove_dc()),
        ("Reverse", "Reverse frames", |w, _| w.reverse_frames()),
        ("Add frame", "Add frame", |w, f| {
            let c = w.frames.get(f).cloned().unwrap_or_default();
            w.insert_frame(f + 1, &c);
        }),
        ("Delete frame", "Delete frame", |w, f| w.remove_frame(f)),
    ];
    for (k, (name, what, op)) in ops.into_iter().enumerate() {
        let b = base.clone();
        col = col.child(chip(("ed-op", dev.key(k)), name, false).on_click(cx.listener(move |this, _, _, cx| {
            let mut w = (*b).clone();
            run_job(this, device, osc, what, move || {
                op(&mut w, frame);
                Ok(w)
            }, cx);
            if what == "Add frame" {
                view(this, device).frame = frame + 1;
            }
        })));
    }
    col
}

fn export(ws: &mut Workspace, base: Rc<WaveFrames>, cx: &mut Context<Workspace>) {
    let dir = ws.project_path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(std::env::temp_dir);
    let name = format!("{}.wav", if base.name.is_empty() { "Wavetable" } else { &base.name });
    let rx = cx.prompt_for_new_path(&dir, Some(&name));
    cx.spawn(async move |this, cx| {
        let Ok(Ok(Some(path))) = rx.await else { return };
        let frames = (*base).clone();
        let job = cx.background_executor().spawn(async move { std::fs::write(&path, aitrack_stock::instrument::wavetable::wav::export(&frames)).map(|_| path) });
        let result = job.await;
        let _ = this.update(cx, |ws, cx| match result {
            Ok(p) => ws.toast(format!("Exported {}", p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())), cx),
            Err(e) => ws.toast(format!("Export failed: {e}"), cx),
        });
    })
    .detach();
}

/// The frame strip: one cell per kept frame; click to pick.
fn frame_strip(ws: &mut Workspace, dev: &Dev, pv: Option<Rc<wt::Preview>>, cx: &mut Context<Workspace>) -> gpui::Stateful<Div> {
    let device = dev.id;
    let frame = view(ws, device).frame;
    let region = ws.fx.region(device, 80);
    let store = region.clone();
    let total = pv.as_ref().map_or(1, |v| v.total);
    let pvp = pv.clone();
    let color = series(view(ws, device).edit_osc as usize);
    let surface = gpui::canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, _| {
            let pal = p();
            let r = Rect::of(b);
            let Some(pv) = &pvp else { return };
            let n = pv.frames.len().max(1);
            let w = r.w / n as f32;
            let scale = 0.4 / pv.peak();
            for (i, f) in pv.frames.iter().enumerate() {
                let real = if n > 1 { i * (pv.total - 1) / (n - 1) } else { 0 };
                let next = if i + 1 < n { (i + 1) * (pv.total - 1) / (n - 1) } else { pv.total };
                let on = (real..next.max(real + 1)).contains(&frame);
                let cell = Rect { x: r.x + w * i as f32, w, ..r };
                if on {
                    window.paint_quad(crate::paint::fill_rect(crate::paint::rect(cell.x, cell.y, cell.w, cell.h), accent_alpha(0.18)));
                }
                let pts = kit::sample_curve(cell.inset(1.), 24, |fx| 0.5 - scale * f[((fx * f.len() as f32) as usize).min(f.len() - 1)]);
                kit::line(window, &pts, 1., if on { color } else { with_alpha(pal.text, 0.4) });
            }
        },
    )
    .size_full();
    let r2 = region.clone();
    div()
        .id(("frame-strip", dev.key(0)))
        .h(px(36.))
        .rounded(px(r_md()))
        .bg(p().well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .cursor_pointer()
        .child(surface)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let r = Rect::of(r2.get());
                let fx = ((f32::from(e.position.x) - r.x) / r.w.max(1.)).clamp(0., 0.9999);
                let v = view(this, device);
                v.frame = ((fx * total as f32) as usize).min(total.saturating_sub(1));
                v.harmonics = None;
                cx.notify();
            }),
        )
}

/// The selected frame, large: the pen draws on it, or harmonic bars.
fn frame_editor(ws: &mut Workspace, dev: &Dev, base: Option<Rc<WaveFrames>>, cx: &mut Context<Workspace>) -> gpui::Stateful<Div> {
    let device = dev.id;
    let (tool, osc, frame) = {
        let v = view(ws, device);
        (v.tool, v.edit_osc, v.frame)
    };
    let color = series(osc as usize);
    let region = ws.fx.region(device, 81);
    let store = region.clone();
    let bars = tool == Tool::Harmonics;
    if bars && let Some(b) = &base {
        super::ensure_harmonics(ws, device, b, frame, cx);
    }
    let drag = ws.fx.synth.get(&device).and_then(|v| v.drag.clone());
    let cycle: Option<Vec<f32>> = match &drag {
        Some(SynthDrag::Draw { cycle, frame: f, osc: o, .. }) if *f == frame && *o == osc => Some(cycle.clone()),
        _ => base.as_ref().and_then(|b| b.frames.get(frame).cloned()),
    };
    let table = base.as_ref().map(|b| Rc::as_ptr(b) as usize);
    let levels: Option<Vec<(f32, f32)>> = match &drag {
        Some(SynthDrag::Bars { levels, .. }) => Some(levels.clone()),
        _ => ws.fx.synth.get(&device).and_then(|v| v.harmonics.as_ref()).filter(|h| Some(h.table) == table && h.frame == frame).map(|h| h.values.clone()),
    };
    let paint_levels = levels.clone();
    let surface = gpui::canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, cx| {
            let pal = p();
            let r = Rect::of(b);
            if bars {
                let Some(lv) = &paint_levels else {
                    kit::txt(window, cx, "Analysing…", r.x + r.w / 2., r.y + r.h / 2. - 6., TEXT_SM, pal.faint, TextAlign::Center);
                    return;
                };
                let peak = lv.iter().fold(0f32, |m, v| m.max(v.0)).max(1e-3).max(1.);
                let n = geom::HARMONICS;
                let w = r.w / n as f32;
                for (k, (a, _)) in lv.iter().take(n).enumerate() {
                    let h = (a / peak).clamp(0., 1.) * r.h;
                    window.paint_quad(crate::paint::rrect(crate::paint::rect(r.x + w * k as f32 + 2., r.bottom() - h, (w - 4.).max(1.), h.max(1.)), 2., with_alpha(color, 0.75)));
                    if k % 4 == 0 {
                        kit::txt(window, cx, &(k + 1).to_string(), r.x + w * k as f32 + w / 2., r.y + 4., TEXT_XS, pal.faint, TextAlign::Center);
                    }
                }
            } else {
                kit::hline(window, r, r.py(0.5), pal.grid_bar);
                for i in 1..8 {
                    kit::vline(window, r, r.px(i as f32 / 8.), pal.grid_sub);
                }
                if let Some(c) = &cycle {
                    let pts = kit::sample_curve(r, (r.w as usize).clamp(64, 1024), |fx| 0.5 - 0.5 * c[((fx * c.len() as f32) as usize).min(c.len() - 1)].clamp(-1., 1.));
                    kit::fill_to(window, &pts, r.py(0.5), with_alpha(color, 0.14));
                    kit::line(window, &pts, 2., color);
                }
            }
        },
    )
    .size_full();
    let r2 = region.clone();
    let press = cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        cx.stop_propagation();
        let Some(base) = base.clone() else { return };
        let r = Rect::of(r2.get());
        let (fx, fy) = (((f32::from(e.position.x) - r.x) / r.w.max(1.)).clamp(0., 1.), ((f32::from(e.position.y) - r.y) / r.h.max(1.)).clamp(0., 1.));
        let drag = if bars {
            let Some(mut lv) = levels.clone() else { return };
            lv.resize(geom::HARMONICS, (0., 0.));
            let peak = lv.iter().fold(0f32, |m, v| m.max(v.0)).max(1.);
            lv.iter_mut().for_each(|v| v.0 /= peak);
            let i = geom::bar_at(fx, geom::HARMONICS);
            lv[i].0 = geom::bar_level(fy);
            SynthDrag::Bars { osc, frame, base, levels: lv, region: r2.clone() }
        } else {
            let mut cycle = base.frames.get(frame).cloned().unwrap_or_else(|| vec![0.; 2048]);
            let at = (fx, geom::wave_level(fy));
            geom::paint_stroke(&mut cycle, &[at]);
            SynthDrag::Draw { osc, frame, base, cycle, last: at, region: r2.clone() }
        };
        view(this, device).drag = Some(drag);
        cx.notify();
    });
    div()
        .id(("frame-editor", dev.key(0)))
        .size_full()
        .rounded(px(r_md()))
        .bg(p().well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .cursor_crosshair()
        .child(surface)
        .on_mouse_down(MouseButton::Left, press)
}

pub fn render(ws: &mut Workspace, dev: &Dev, _d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let device = dev.id;
    let osc = view(ws, device).edit_osc;
    let (key, base) = super::table_of(ws, dev, osc, cx);
    let pv = wt::preview(ws, key, cx);
    let name = wt::table_name(ws, dev, osc);
    let total = base.as_ref().map_or(0, |b| b.frames.len());
    {
        let v = view(ws, device);
        v.frame = v.frame.min(total.saturating_sub(1));
    }
    let (frame, busy, error) = {
        let v = view(ws, device);
        (v.frame, v.busy.clone(), v.error.clone())
    };
    let color = series(osc as usize);
    let pos = if total > 1 { frame as f32 / (total - 1) as f32 } else { 0. };
    let region = ws.fx.region(device, 82);
    let stack_pv = pv.clone();
    let stack = div()
        .id(("ed-stack", dev.key(0)))
        .h(px(200.))
        .rounded(px(r_md()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(accent_alpha(0.8)))
        .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
            if let Some(p) = paths.paths().first() {
                super::import_wav(this, device, osc, p.clone(), cx);
            }
        }))
        .child(
            gpui::canvas(
                move |b, _, _| region.set(b),
                move |b, _, window, cx| {
                    let r = Rect::of(b);
                    match &stack_pv {
                        Some(pv) => paint_stack(window, r.inset(4.), pv, pos, color, 0.8, 40),
                        None => kit::txt(window, cx, "Building table…", r.x + r.w / 2., r.y + r.h / 2., TEXT_SM, p().faint, TextAlign::Center),
                    }
                },
            )
            .size_full(),
        );
    let b_export = base.clone();
    let header_right = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(action(("ed-import", dev.key(0)), "Import WAV…", Some(Icon::FolderOpen)).on_click(cx.listener(move |this, _, _, cx| super::pick_wav(this, device, osc, cx))))
        .child(action(("ed-export", dev.key(0)), "Export WAV…", Some(Icon::Download)).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(b) = b_export.clone() {
                export(this, b, cx);
            }
        })));
    let left = panel("TOOLS", Some(pal.accent))
        .w(px(320.))
        .child(osc_picker(ws, dev, cx))
        .child(tool_picker(ws, dev, cx))
        .child(tool_panel(ws, dev, &base, cx))
        .child(div().h(px(1.)).bg(border()))
        .child(label("TABLE", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(table_ops(ws, dev, &base, cx))
        .when_some(busy, |d, b| d.child(label(b, TEXT_SM, pal.accent)))
        .when_some(error, |d, e| d.child(label(e, TEXT_SM, pal.record)));
    let right = panel_with(&name, Some(color), header_right)
        .flex_1()
        .min_w(px(0.))
        .child(stack)
        .child(frame_strip(ws, dev, pv, cx))
        .child(div().flex().items_center().gap(px(SP2)).child(mono(format!("Frame {} / {}", frame + 1, total.max(1)), TEXT_SM, pal.text)).child(label("· click the strip to pick a frame · drop a WAV on the stack to import", TEXT_XS, pal.faint)))
        .child(div().h(px(340.)).child(frame_editor(ws, dev, base, cx)));
    div().flex().gap(px(SP3)).child(left).child(right).into_any_element()
}
