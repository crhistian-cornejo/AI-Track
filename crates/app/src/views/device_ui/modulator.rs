//! The modulators' cards: `mod.lfo` draws its wave over two cycles (with a
//! head that runs along it) and `mod.envfollow` the level it has followed
//! lately; next to the display, the panel of targets. Press **Map**, then
//! touch a knob of another device on the track (or the track's pan, or its
//! fader) to bind it; **+** lists every parameter instead, which is how a
//! hosted plugin's parameters are reached. Each target has a range bar: the
//! modulator's output moves the target by `min..max` (normalised units) on
//! top of the knob's own value, so the knob stays editable. A mapped knob
//! wears a ring where the modulation sweeps it.
//!
//! The mapping state lives in the project (`Device::mods`, edited through
//! `Command`s); only the Map mode and the rings are view state, kept per
//! thread because the knobs that draw the rings are built far from here.

use super::kit::{self, Handle, PlotCfg, ax};
use super::{Dev, PopKind};
use crate::icons::Icon;
use crate::theme::*;
use crate::widgets::{self, label, menu_card, menu_header, menu_item, mono};
use crate::workspace::{Drag, KnobTarget, Workspace};
use aitrack_model::{Device, DeviceId, ModMapping, ModTarget, Project};
use aitrack_stock::fx::{envfollow, lfo};
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, TextAlign, div, prelude::FluentBuilder, px,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Cycles of the LFO the display shows.
const CYCLES: f64 = 2.;
/// Width of the targets panel and of a range bar.
const PANEL_W: f32 = 240.;
const BAR_W: f32 = 160.;
const HANDLE: f32 = 8.;
/// The part of the display's height the 0..1 output uses, centred.
const SPAN: f32 = 0.84;

// ───────────────────────────── View state ─────────────────────────────

thread_local! {
    /// Map mode: the modulator the next knob touched is mapped to.
    static MAP: Cell<Option<DeviceId>> = const { Cell::new(None) };
    /// The offset range of every mapped parameter, for the knobs' rings.
    static RINGS: RefCell<HashMap<(DeviceId, u32), (f32, f32)>> = RefCell::new(HashMap::new());
}

/// The modulator in Map mode, if any.
pub fn map_mode() -> Option<DeviceId> {
    MAP.with(Cell::get)
}

pub fn set_map_mode(device: Option<DeviceId>) {
    MAP.with(|m| m.set(device));
}

/// Collect the mapped parameters of the project (once a frame, before the
/// knobs are built). A parameter wears the ring of the first modulator that
/// drives it, as the engine gives it to that one.
pub fn refresh(project: &Project) {
    RINGS.with(|r| {
        let mut r = r.borrow_mut();
        r.clear();
        for t in &project.tracks {
            aitrack_model::visit_devices(&t.devices, &mut |d| {
                for m in &d.mods {
                    if let ModTarget::Param { device, param } = m.target {
                        r.entry((device, param)).or_insert((m.min.min(m.max), m.min.max(m.max)));
                    }
                }
            });
        }
    });
}

/// The offset range a knob's modulation sweeps, if it is mapped.
pub fn ring(device: DeviceId, param: u32) -> Option<(f32, f32)> {
    RINGS.with(|r| r.borrow().get(&(device, param)).copied())
}

// ───────────────────────────── Map mode ─────────────────────────────

/// Map mode swallows a press on a knob, the pan knob or a fader of the
/// modulator's track: the target is mapped and the mode ends. `true` when it
/// took the press.
pub fn pick_drag(ws: &mut Workspace, drag: &Drag, cx: &mut Context<Workspace>) -> bool {
    let Some(m) = map_mode() else { return false };
    let target = match drag {
        Drag::Knob { target: KnobTarget::Param { device, param, .. }, .. } => ModTarget::Param { device: *device, param: *param },
        Drag::Knob { target: KnobTarget::Pan { track }, .. } if ws.s.project.device_track(m) == Some(*track) => ModTarget::Pan,
        Drag::Fader { track, .. } | Drag::HFader { track, .. } if ws.s.project.device_track(m) == Some(*track) => ModTarget::Volume,
        Drag::Knob { target: KnobTarget::Pan { .. }, .. } | Drag::Fader { .. } | Drag::HFader { .. } => {
            ws.toast("A modulator drives the pan and volume of its own track", cx);
            return true;
        }
        _ => return false,
    };
    bind(ws, m, target, cx);
    true
}

/// Map `target` and say so (or why not).
fn bind(ws: &mut Workspace, m: DeviceId, target: ModTarget, cx: &mut Context<Workspace>) {
    match ws.s.map_modulator(m, target) {
        Ok(_) => {
            let name = target_name(ws, target);
            set_map_mode(None);
            ws.toast(format!("Mapped to {name}: set its range with the handles"), cx);
        }
        Err(why) => ws.toast(why, cx),
    }
    cx.notify();
}

// ───────────────────────────── Range drag ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeHandle {
    Low,
    High,
}

/// A drag of one end of a mapping's range bar; the values are the ones at
/// the press, so every frame is worked out from them and the drag never drifts.
#[derive(Clone, Debug)]
pub struct ModDrag {
    pub device: DeviceId,
    pub index: usize,
    pub handle: RangeHandle,
    pub start_x: f32,
    pub min: f32,
    pub max: f32,
}

/// The range after one end moved by `d` (normalised units); the ends may cross, which inverts it.
pub fn range_after_drag(handle: RangeHandle, min: f32, max: f32, d: f32) -> (f32, f32) {
    match handle {
        RangeHandle::Low => ((min + d).clamp(-1., 1.), max),
        RangeHandle::High => (min, (max + d).clamp(-1., 1.)),
    }
}

/// The bar covers -1..1 over [`BAR_W`] px; Shift (`fine` < 1) slows the drag.
pub fn drag_move(ws: &mut Workspace, drag: &ModDrag, x: f32, fine: f32) {
    let (min, max) = range_after_drag(drag.handle, drag.min, drag.max, (x - drag.start_x) / BAR_W * 2. * fine);
    ws.s.set_mod_range(drag.device, drag.index, min, max);
}

// ───────────────────────────── Names ─────────────────────────────

/// `Device › Parameter`, `Track › Volume`.
fn target_name(ws: &Workspace, target: ModTarget) -> String {
    match target {
        ModTarget::Volume => "Track › Volume".to_owned(),
        ModTarget::Pan => "Track › Pan".to_owned(),
        ModTarget::Param { device, param } => match ws.s.project.device(device) {
            Some(d) => {
                let name = crate::views::arrangement::device_params(ws, d).into_iter().find(|p| p.0 == param).map_or_else(|| format!("P{param}"), |p| p.1);
                format!("{} › {name}", d.name)
            }
            None => "Missing device".to_owned(),
        },
    }
}

/// A range as text: `−25…+25%`.
pub fn range_text(min: f32, max: f32) -> String {
    let pct = |v: f32| {
        let p = (v * 100.).round();
        if p == 0. { "0".to_owned() } else { format!("{p:+}").replace('-', "−") }
    };
    format!("{}…{}%", pct(min), pct(max))
}

// ───────────────────────────── Parts ─────────────────────────────

/// A text toggle, 20 px high.
fn pill(id: impl Into<ElementId>, text: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .border_1()
        .border_color(if on { accent_alpha(0.5) } else { gpui::transparent_black() })
        .bg(if on { accent_alpha(0.16) } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(label(text, TEXT_XS, if on { pal.accent } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
}

/// One mapping's range bar: two handles over a -1..1 scale (zero at the
/// centre); dragging one past the other inverts it.
fn range_bar(device: DeviceId, index: usize, m: &ModMapping, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let key = (device.0 as usize) << 6 | index;
    let at = |v: f32| (v.clamp(-1., 1.) + 1.) / 2. * BAR_W;
    let (a, b) = (at(m.min.min(m.max)), at(m.min.max(m.max)));
    let (min, max) = (m.min, m.max);
    let handle = |tag: &'static str, v: f32, which: RangeHandle, cx: &mut Context<Workspace>| {
        div()
            .id((tag, key))
            .absolute()
            .left(px(at(v) - HANDLE / 2.))
            .top(px(2.))
            .w(px(HANDLE))
            .h(px(16.))
            .rounded(px(2.))
            .bg(pal.text)
            .cursor_ew_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.begin_drag(Drag::Mod(ModDrag { device, index, handle: which, start_x: f32::from(e.position.x), min, max }), cx);
                }),
            )
    };
    div()
        .relative()
        .flex_none()
        .w(px(BAR_W))
        .h(px(20.))
        .rounded(px(r_sm()))
        .bg(content(0.06))
        .child(div().absolute().left(px(BAR_W / 2.)).top(px(3.)).w(px(1.)).h(px(14.)).bg(pal.grid_bar))
        .child(div().absolute().left(px(a)).top(px(6.)).w(px((b - a).max(1.))).h(px(8.)).rounded(px(2.)).bg(if m.min > m.max { with_alpha(pal.record, 0.6) } else { accent_alpha(0.6) }))
        .child(handle("mod-min", m.min, RangeHandle::Low, cx))
        .child(handle("mod-max", m.max, RangeHandle::High, cx))
}

/// The targets of a modulator: Map and + to add, a row with a range bar and
/// a delete for each.
fn targets_panel(ws: &mut Workspace, d: &Device, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let id = d.id;
    let mapping = map_mode() == Some(id);
    let full = d.mods.len() >= aitrack_model::MAX_MOD_MAPPINGS;
    let key = id.0 as usize;
    let header = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(kit_caption(format!("TARGETS · {}/{}", d.mods.len(), aitrack_model::MAX_MOD_MAPPINGS)))
        .child(div().flex_1())
        .child(
            pill(("mod-map", key), if mapping { "Mapping…" } else { "Map" }, mapping)
                .tooltip(widgets::tip("Press, then touch a knob of a device on this track", None))
                .on_click(cx.listener(move |_, _, _, cx| {
                    set_map_mode(if map_mode() == Some(id) { None } else { Some(id) });
                    cx.notify();
                })),
        )
        .child(
            pill(("mod-add", key), "+", false).tooltip(widgets::tip("Choose a parameter from a list", None)).on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.fx.open_pop(PopKind::ModTarget(id), gpui::point(at.x - px(190.), at.y + px(12.)));
                cx.notify();
            })),
        );
    let mut list = div().id(("mod-list", key)).flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col().gap(px(SP1));
    if mapping {
        list = list.child(label(if full { "Full: unmap one first" } else { "Touch a knob on this track (its pan or fader too)" }, TEXT_XS, pal.accent));
    }
    for (i, m) in d.mods.iter().enumerate() {
        let name = target_name(ws, m.target);
        let missing = match m.target {
            ModTarget::Param { device, .. } => ws.s.project.device(device).is_none_or(|t| ws.s.project.device_track(device) != ws.s.project.device_track(id) || t.id == id),
            _ => false,
        };
        list = list.child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label(crate::paint::fit(&name, PANEL_W - 52., TEXT_SM), TEXT_SM, if missing { pal.faint } else { pal.text })))
                        .child(
                            widgets::sized_icon_button(("mod-del", key << 6 | i), Icon::X, false, 20., 12.)
                                .tooltip(widgets::tip("Remove the mapping", None))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.s.unmap_modulator(id, i);
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().flex().items_center().gap(px(SP2)).child(range_bar(id, i, m, cx)).child(mono(range_text(m.min, m.max), TEXT_XS, pal.muted))),
        );
    }
    if d.mods.is_empty() && !mapping {
        list = list.child(label("Nothing mapped. Press Map, then touch a knob.", TEXT_XS, pal.faint));
    }
    div().flex_none().w(px(PANEL_W)).h_full().pl(px(SP3)).border_l_1().border_color(border()).flex().flex_col().gap(px(SP2)).child(header).child(list)
}

fn kit_caption(text: String) -> Div {
    super::caption(text)
}

/// The list of every parameter a modulator can drive (the + button): the
/// track's fader and pan, then the parameters of each device on the track.
pub fn target_menu(ws: &mut Workspace, modulator: DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let track = ws.s.project.device_track(modulator)?;
    let mut entries: Vec<(String, ModTarget)> = vec![("Track › Volume".into(), ModTarget::Volume), ("Track › Pan".into(), ModTarget::Pan)];
    let mut headers: Vec<(usize, String)> = Vec::new();
    let mut devices: Vec<Device> = Vec::new();
    aitrack_model::visit_devices(&ws.s.project.track(track)?.devices, &mut |d| devices.push(Device { state: None, ..d.clone() }));
    const CAP: usize = 400;
    for d in devices.iter().filter(|d| d.id != modulator && !matches!(d.kind, aitrack_model::DeviceKind::Rack(_))) {
        headers.push((entries.len(), d.name.clone()));
        for (param, name) in crate::views::arrangement::device_params(ws, d) {
            if entries.len() >= CAP {
                break;
            }
            entries.push((name, ModTarget::Param { device: d.id, param }));
        }
    }
    let mapped: Vec<ModTarget> = ws.s.project.device(modulator)?.mods.iter().map(|m| m.target).collect();
    let mut list = div().id("mod-targets").max_h(px(420.)).overflow_y_scroll().flex().flex_col();
    for (i, (name, target)) in entries.iter().enumerate() {
        if let Some((_, dev)) = headers.iter().find(|h| h.0 == i) {
            list = list.child(menu_header(dev.to_uppercase()));
        }
        let target = *target;
        list = list.child(menu_item(("mod-target", i), None, name.clone(), None, mapped.contains(&target)).on_click(cx.listener(move |this, _, _, cx| {
            this.fx.pop = None;
            bind(this, modulator, target, cx);
        })));
    }
    Some(menu_card("mod-target-menu", 232.).child(menu_header("MAP TO")).child(list).into_any_element())
}

// ───────────────────────────── LFO ─────────────────────────────

/// Where a 0..1 output sits on the display (0 = top).
fn out_frac(v: f32) -> f32 {
    (1. - SPAN) / 2. + SPAN * (1. - v.clamp(0., 1.))
}

/// The LFO's output (0..1) at `n` points over [`CYCLES`] cycles, as the
/// device computes it (without jitter and smoothing).
pub fn lfo_curve(wave: usize, depth: f32, offset: f32, phase: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let cycles = f64::from(phase) + CYCLES * i as f64 / (n - 1).max(1) as f64;
            (0.5 + 0.5 * (offset + depth * lfo::shape(wave, cycles))).clamp(0., 1.)
        })
        .collect()
}

/// Where the wave first peaks: the spot for the Phase handle (plot fractions).
pub fn peak_of(curve: &[f32]) -> (f32, f32) {
    let best = curve.iter().copied().fold(f32::MIN, f32::max);
    let at = curve.iter().position(|v| *v >= best - 1e-3).unwrap_or(0);
    (at as f32 / (curve.len() - 1).max(1) as f32, curve[at])
}

pub fn render_lfo(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    use lfo::p;
    let wave = dev.choice(p::WAVE);
    let sync = dev.choice(p::TIME_MODE) == 1;
    let (depth, offset) = (dev.plain(p::DEPTH) / 100., dev.plain(p::OFFSET) / 100.);
    let phase = dev.plain(p::PHASE) / 360.;
    let live = dev.meter.len() >= 2;
    let (out, head) = (dev.m(0), dev.m(1).rem_euclid(1.));
    let enabled = dev.enabled;
    let rate = if sync { dev.text(p::DIVISION) } else { dev.text(p::RATE) };
    let title = format!("{} · {rate} · depth {:.0}%", lfo::WAVES.get(wave).copied().unwrap_or("Sine").to_lowercase(), depth * 100.);
    let curve = lfo_curve(wave, depth, offset, phase, 480);
    let (px_, py_) = peak_of(&curve);
    let default_of = |id: u32| (id, dev.info(id).map_or(0., |i| i.default_normalized()));
    let handles = vec![Handle {
        fx: px_,
        fy: out_frac(py_),
        x: ax(p::PHASE, -(CYCLES as f32)),
        y: ax(p::DEPTH, -2. / SPAN),
        index: 0,
        reset: vec![default_of(p::PHASE), default_of(p::DEPTH)],
        ..Default::default()
    }];
    let mark = (px_, out_frac(py_));
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let color = if enabled { pal.accent } else { pal.muted };
        for q in 1..4 {
            kit::vline(window, r, r.px(q as f32 / 4.), if q == 2 { pal.grid_bar } else { pal.grid_beat });
        }
        kit::hline(window, r, r.py(out_frac(0.5)), pal.grid_bar);
        kit::hline(window, r, r.py(out_frac(1.)), pal.grid_beat);
        kit::hline(window, r, r.py(out_frac(0.)), pal.grid_beat);
        let n = ((r.w / 2.) as usize).clamp(64, 480);
        let pts: Vec<_> = lfo_curve(wave, depth, offset, phase, n).iter().enumerate().map(|(i, v)| r.pt(i as f32 / (n - 1) as f32, out_frac(*v))).collect();
        kit::fill_to(window, &pts, r.py(out_frac(0.)), with_alpha(color, 0.10));
        kit::line(window, &pts, 1.5, with_alpha(color, 0.95));
        // The head runs over both cycles at the device's phase.
        if live {
            let fx = (f64::from(head) - f64::from(phase)).rem_euclid(1.) as f32;
            for cycle in 0..CYCLES as usize {
                let x = (fx + cycle as f32) / CYCLES as f32;
                kit::vline(window, r, r.px(x), with_alpha(pal.text, 0.3));
                kit::dot(window, r.pt(x, out_frac(out)), 4., color, Some(pal.text));
            }
        }
        kit::dot(window, r.pt(mark.0, mark.1), 6., color, Some(pal.text));
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "1", r.x + 6., r.py(out_frac(1.)) - 12., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "0", r.x + 6., r.py(out_frac(0.)) - 12., TEXT_XS, pal.faint, TextAlign::Left);
    });
    let rate_id = if sync { p::DIVISION } else { p::RATE };
    let left = div()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(div().flex_none().flex().items_center().gap(px(SP1)).child(kit::pick(dev, p::WAVE, cx)).child(kit::pick(dev, p::TIME_MODE, cx)).child(kit::pick(dev, p::RETRIG, cx)))
        .child(kit::strip(ws, dev, &[rate_id, p::DEPTH, p::OFFSET, p::PHASE], &[p::JITTER, p::SMOOTH], cx));
    let right = targets_panel(ws, d, cx);
    kit::panel(if dev.big { 760. } else { 680. }).child(div().flex_1().min_h(px(0.)).flex().gap(px(SP3)).child(left).child(right)).into_any_element()
}

// ───────────────────────────── Envelope follower ─────────────────────────────

/// The recent outputs the device published (0..1, newest last).
pub fn history_of(meter: &[f32]) -> &[f32] {
    meter.get(1..).unwrap_or(&[])
}

pub fn render_env(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    use envfollow::p;
    let enabled = dev.enabled;
    let now = dev.m(0).clamp(0., 1.);
    let hist: Vec<f32> = history_of(&dev.meter).to_vec();
    let title = format!("{} · gain {:+.0} dB · rise {} · fall {}", dev.text(p::MODE).to_lowercase(), dev.plain(p::GAIN), dev.text(p::RISE), dev.text(p::FALL));
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let color = if enabled { pal.accent } else { pal.muted };
        for q in 1..4 {
            kit::vline(window, r, r.px(q as f32 / 4.), pal.grid_beat);
        }
        kit::hline(window, r, r.py(out_frac(0.5)), pal.grid_bar);
        kit::hline(window, r, r.py(out_frac(1.)), pal.grid_beat);
        kit::hline(window, r, r.py(out_frac(0.)), pal.grid_beat);
        if hist.len() > 1 {
            let pts: Vec<_> = hist.iter().enumerate().map(|(i, v)| r.pt(i as f32 / (hist.len() - 1) as f32, out_frac(*v))).collect();
            kit::fill_to(window, &pts, r.py(out_frac(0.)), with_alpha(color, 0.14));
            kit::line(window, &pts, 1.5, with_alpha(color, 0.95));
        }
        // The level now: a bar on the right edge.
        let (bx, bw) = (r.px(0.97) - 2., 4.);
        let (top, bottom) = (r.py(out_frac(1.)), r.py(out_frac(0.)));
        kit::bar(window, bx, top, bw, bottom - top, 2., content(0.08));
        let y = r.py(out_frac(now));
        kit::bar(window, bx, y, bw, bottom - y, 2., with_alpha(pal.accent, 0.7));
        kit::dot(window, r.pt(0.97, out_frac(now)), 5.5, color, Some(pal.text));
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
    });
    let left = div()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(div().flex_none().flex().items_center().gap(px(SP1)).child(kit::pick(dev, p::MODE, cx)).child(kit::pick(dev, p::INVERT, cx)))
        .child(kit::strip(ws, dev, &[p::GAIN, p::RISE, p::FALL, p::DEPTH], &[], cx));
    let right = targets_panel(ws, d, cx);
    kit::panel(if dev.big { 720. } else { 640. }).child(div().flex_1().min_h(px(0.)).flex().gap(px(SP3)).child(left).child(right)).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::TrackKind;

    #[test]
    fn a_range_end_moves_and_the_ends_may_cross() {
        assert_eq!(range_after_drag(RangeHandle::High, -0.25, 0.25, 0.25), (-0.25, 0.5));
        assert_eq!(range_after_drag(RangeHandle::Low, -0.25, 0.25, 0.75), (0.5, 0.25), "crossed: inverted");
        assert_eq!(range_after_drag(RangeHandle::High, 0., 0.9, 0.5), (0., 1.), "kept inside -1..1");
        assert_eq!(range_after_drag(RangeHandle::Low, -0.9, 0., -0.5), (-1., 0.));
    }

    #[test]
    fn ranges_read_as_percent() {
        assert_eq!(range_text(-0.25, 0.25), "−25…+25%");
        assert_eq!(range_text(0., 1.), "0…+100%");
        assert_eq!(range_text(0.504, -0.5), "+50…−50%");
    }

    #[test]
    fn the_curve_is_the_devices_output() {
        // A saw up with full depth runs 0 → 1 over a cycle; an offset lifts it.
        let c = lfo_curve(2, 1., 0., 0., 480);
        assert!(c[0] < 0.01 && (c[239] - 1.).abs() < 0.01 && c[240] < 0.01, "two ramps");
        assert!(lfo_curve(2, 0.5, 0.5, 0., 100).iter().all(|v| (0.25 - 1e-3..=1.0).contains(v)));
        // Sine at phase 0 starts mid-scale and peaks a quarter cycle in (1/8 of the display).
        let s = lfo_curve(0, 1., 0., 0., 481);
        assert!((s[0] - 0.5).abs() < 1e-4);
        let (fx, v) = peak_of(&s);
        assert!((fx - 0.125).abs() < 0.01 && v > 0.99, "{fx} {v}");
        // The curve matches what the processor computes for a block.
        let mut p = aitrack_stock::fx::lfo::Lfo::new();
        aitrack_dsp::Processor::prepare(&mut p, 48_000., 128);
        let beats: Vec<f64> = (0..8).map(|k| k as f64 * 32. / 24_000.).collect();
        let m = aitrack_dsp::ModBlock { sample_rate: 48_000., playing: true, bpm: 120., step: 32, frames: 256, start_samples: 0, beats: &beats, events: &[] };
        let mut out = [0.; 8];
        assert!(aitrack_dsp::Processor::modulate(&mut p, &m, &mut out));
        // Default division 1/4 = one beat per cycle: step k is k·32/24000 cycles in.
        for (k, v) in out.iter().enumerate() {
            let cycles = k as f64 * 32. / 24_000.;
            let expect = (0.5 + 0.5 * lfo::shape(0, cycles)).clamp(0., 1.);
            assert!((v - expect).abs() < 1e-5);
        }
    }

    #[test]
    fn the_history_follows_the_meter() {
        assert_eq!(history_of(&[0.5, 0.1, 0.2]), &[0.1, 0.2]);
        assert!(history_of(&[]).is_empty());
    }

    #[test]
    fn rings_follow_the_project_and_map_mode_is_per_modulator() {
        let mut s = crate::state::AppState::new(Project::new("t"));
        let t = s.add_track(TrackKind::Audio).unwrap();
        let eq = s.add_stock_device(t, "fx.eq", "EQ").unwrap();
        let m = s.add_stock_device(t, "mod.lfo", "LFO").unwrap();
        assert!(s.map_modulator(m, ModTarget::Param { device: eq, param: 2 }).is_ok());
        s.set_mod_range(m, 0, 0.5, -0.1);
        refresh(&s.project);
        assert_eq!(ring(eq, 2), Some((-0.1, 0.5)));
        assert_eq!(ring(eq, 3), None);
        assert_eq!(ring(m, 2), None);
        s.undo();
        s.undo();
        refresh(&s.project);
        assert_eq!(ring(eq, 2), None, "unmapped by undo");
        set_map_mode(Some(m));
        assert_eq!(map_mode(), Some(m));
        set_map_mode(None);
        assert_eq!(map_mode(), None);
    }
}
