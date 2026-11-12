//! What the device editors share: a plot with draggable handles, compact
//! number / choice controls, and painting helpers (grids, curves, areas).

use super::{Axis, Dev, FxDrag, PopKind, begin_drag, current, set_gliding, set_many, set_now};
use crate::icons::{Icon, icon};
use crate::paint;
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::{Drag, KnobTarget, NumTarget, Workspace, click_dragged, plain_text};
use aitrack_dsp::{ParamInfo, ParamScale};
use gpui::{
    AnyElement, App, Bounds, ContentMask, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, ScrollDelta, ScrollWheelEvent, Stateful, StatefulInteractiveElement, Styled, TextAlign,
    Window, canvas, div, point, prelude::FluentBuilder, px,
};
use std::collections::HashMap;
use std::rc::Rc;

// ───────────────────────────── Geometry ─────────────────────────────

/// A plot's rectangle in window pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn of(b: Bounds<Pixels>) -> Self {
        Self { x: f32::from(b.origin.x), y: f32::from(b.origin.y), w: f32::from(b.size.width), h: f32::from(b.size.height) }
    }

    pub fn inset(self, d: f32) -> Self {
        Self { x: self.x + d, y: self.y + d, w: (self.w - 2. * d).max(1.), h: (self.h - 2. * d).max(1.) }
    }

    /// Window x of horizontal fraction `fx`.
    pub fn px(&self, fx: f32) -> f32 {
        self.x + self.w * fx
    }

    /// Window y of vertical fraction `fy` (0 = top).
    pub fn py(&self, fy: f32) -> f32 {
        self.y + self.h * fy
    }

    pub fn pt(&self, fx: f32, fy: f32) -> Point<Pixels> {
        point(px(self.px(fx)), px(self.py(fy)))
    }

    #[allow(dead_code)]
    pub fn bounds(&self) -> Bounds<Pixels> {
        paint::rect(self.x, self.y, self.w, self.h)
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Split off `w` pixels from the left; returns `(left, rest)`.
    pub fn split_left(self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w);
        (Rect { w, ..self }, Rect { x: self.x + w, w: self.w - w, ..self })
    }
}

pub const F_LO: f32 = 20.;
pub const F_HI: f32 = 20_000.;

/// Horizontal fraction of a frequency on the 20 Hz – 20 kHz log axis.
pub fn freq_frac(f: f32) -> f32 {
    (f.max(1.) / F_LO).ln() / (F_HI / F_LO).ln()
}

/// Frequency at a horizontal fraction of the log axis.
pub fn frac_freq(t: f32) -> f32 {
    F_LO * (F_HI / F_LO).powf(t)
}

/// How far a log-scaled parameter's normalised value moves when its handle
/// crosses the whole 20 Hz – 20 kHz axis.
pub fn log_span(info: &ParamInfo) -> f32 {
    match info.scale {
        ParamScale::Log if info.min > 0. && info.max > info.min => (F_HI / F_LO).ln() / (info.max / info.min).ln(),
        _ => 1.,
    }
}

// ───────────────────────────── Painting ─────────────────────────────

pub fn line(window: &mut Window, pts: &[Point<Pixels>], width: f32, color: Hsla) {
    paint::polyline(window, pts, width, color);
}

/// The area between a curve and the horizontal line at `base_y`.
pub fn fill_to(window: &mut Window, pts: &[Point<Pixels>], base_y: f32, color: Hsla) {
    if pts.len() < 2 {
        return;
    }
    let mut poly = Vec::with_capacity(pts.len() + 2);
    poly.push(point(pts[0].x, px(base_y)));
    poly.extend_from_slice(pts);
    poly.push(point(pts[pts.len() - 1].x, px(base_y)));
    paint::polygon(window, &poly, color);
}

pub fn hline(window: &mut Window, r: Rect, y: f32, color: Hsla) {
    window.paint_quad(paint::fill_rect(paint::rect(r.x, y.round(), r.w, 1.), color));
}

pub fn vline(window: &mut Window, r: Rect, x: f32, color: Hsla) {
    window.paint_quad(paint::fill_rect(paint::rect(x.round(), r.y, 1., r.h), color));
}

pub fn bar(window: &mut Window, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Hsla) {
    window.paint_quad(paint::rrect(paint::rect(x, y, w, h), radius, color));
}

/// A round handle: filled, with an optional ring (selection, hover).
pub fn dot(window: &mut Window, c: Point<Pixels>, r: f32, fill: Hsla, ring: Option<Hsla>) {
    let b = Bounds::new(point(c.x - px(r), c.y - px(r)), gpui::size(px(2. * r), px(2. * r)));
    match ring {
        Some(ring) => window.paint_quad(paint::rrect_border(b, r, fill, ring, 2.)),
        None => window.paint_quad(paint::rrect(b, r, fill)),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn txt(window: &mut Window, cx: &mut App, s: &str, x: f32, y: f32, size: f32, color: Hsla, align: TextAlign) {
    paint::text(window, cx, s, px(x), px(y), size, color, FontWeight::NORMAL, align);
}

/// `n + 1` points of `f(fx) -> fy` across a rect, clamped to it.
pub fn sample_curve(r: Rect, n: usize, f: impl Fn(f32) -> f32) -> Vec<Point<Pixels>> {
    (0..=n)
        .map(|i| {
            let fx = i as f32 / n as f32;
            r.pt(fx, f(fx).clamp(0., 1.))
        })
        .collect()
}

/// Vertical lines of a 20 Hz – 20 kHz log axis, decades stronger, with labels.
pub fn freq_grid(window: &mut Window, cx: &mut App, r: Rect, labels: bool) {
    let pal = p();
    for (f, text, major) in [
        (20., "20", true),
        (50., "50", false),
        (100., "100", true),
        (200., "200", false),
        (500., "500", false),
        (1_000., "1k", true),
        (2_000., "2k", false),
        (5_000., "5k", false),
        (10_000., "10k", true),
        (20_000., "20k", true),
    ] {
        let x = r.px(freq_frac(f));
        if f > 20. && f < 20_000. {
            vline(window, r, x, if major { pal.grid_bar } else { pal.grid_beat });
        }
        if labels && r.w > 220. {
            let align = if f <= 20. { TextAlign::Left } else if f >= 20_000. { TextAlign::Right } else { TextAlign::Center };
            let dx = if f <= 20. { 3. } else if f >= 20_000. { -3. } else { 0. };
            txt(window, cx, text, x + dx, r.bottom() - 13., TEXT_XS, pal.faint, align);
        }
    }
}

/// Horizontal lines every `step` dB between `lo` and `hi`, zero stronger.
pub fn db_grid(window: &mut Window, cx: &mut App, r: Rect, lo: f32, hi: f32, step: f32, labels: bool) {
    let pal = p();
    let mut db = (lo / step).ceil() * step;
    while db <= hi + 1e-3 {
        let y = r.py((hi - db) / (hi - lo));
        if db > lo && db < hi {
            hline(window, r, y, if db.abs() < 1e-3 { pal.grid_bar } else { pal.grid_beat });
            if labels {
                txt(window, cx, &format!("{db:+.0}"), r.x + 4., y - 12., TEXT_XS, pal.faint, TextAlign::Left);
            }
        }
        db += step;
    }
}

/// Text that reads on top of `fill`.
pub fn ink_on(fill: Hsla) -> Hsla {
    let pal = p();
    if contrast(pal.clip_text_dark, fill) > contrast(pal.clip_text_light, fill) { pal.clip_text_dark } else { pal.clip_text_light }
}

// ───────────────────────────── Plot with handles ─────────────────────────────

/// How a handle's move along one axis maps to a parameter: the change of
/// the normalised value when the handle crosses the whole plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisSpec {
    pub param: u32,
    pub per_frac: f32,
}

pub fn ax(param: u32, per_frac: f32) -> Option<AxisSpec> {
    Some(AxisSpec { param, per_frac })
}

/// A draggable point of a plot, at fractions of its rect.
#[derive(Clone, Debug, Default)]
pub struct Handle {
    pub fx: f32,
    pub fy: f32,
    pub x: Option<AxisSpec>,
    pub y: Option<AxisSpec>,
    /// Alt + vertical drag, and the scroll wheel.
    pub alt: Option<AxisSpec>,
    /// What `selected` becomes when the handle is grabbed.
    pub index: usize,
    /// Parameters (normalised) a double-click restores.
    pub reset: Vec<(u32, f32)>,
}

pub type EmptyFn = Rc<dyn Fn(&mut Workspace, f32, f32, &mut Context<Workspace>)>;
pub type ContextFn = Rc<dyn Fn(&mut Workspace, usize, Point<Pixels>, &mut Context<Workspace>)>;

#[derive(Default)]
pub struct PlotCfg {
    /// Which of the device's plots this is (its bounds are kept per slot).
    pub slot: u8,
    pub handles: Vec<Handle>,
    /// Double-click on empty space, at plot fractions.
    pub on_empty: Option<EmptyFn>,
    /// Right-click on a handle.
    pub on_context: Option<ContextFn>,
    /// Arrow keys nudge the selected handle (the plot takes focus on click).
    pub keys: bool,
}

/// Handles within this many pixels of the pointer are hit.
const GRAB: f32 = 14.;

fn nearest(handles: &[Handle], b: Bounds<Pixels>, pos: Point<Pixels>, reach: f32) -> Option<&Handle> {
    let r = Rect::of(b);
    let (mx, my) = (f32::from(pos.x), f32::from(pos.y));
    handles
        .iter()
        .map(|h| (h, (r.px(h.fx) - mx).hypot(r.py(h.fy) - my)))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| h)
}

fn axis_of(ws: &Workspace, dev: &Dev, spec: Option<AxisSpec>, extent: f32) -> Option<Axis> {
    let spec = spec?;
    let info = dev.info(spec.param)?;
    Some(Axis { param: spec.param, value: current(ws, dev.id, info), per_px: spec.per_frac / extent.max(1.) })
}

/// How much one wheel event moves a parameter (normalised). A mouse wheel
/// moves a step per notch; a trackpad moves smoothly with its pixel delta
/// (a 25 px swipe is one step). Stepped parameters move one choice per
/// notch, or per 25 px of swipe. `None` when the event moves nothing.
pub fn wheel_change(delta: ScrollDelta, stepped: bool, fine: bool, span: f32) -> Option<f32> {
    const PX_PER_STEP: f32 = 25.;
    let step = if stepped { 1. / span } else if fine { 0.008 } else { 0.04 };
    let change = match delta {
        ScrollDelta::Lines(l) if l.y.abs() >= 0.01 => l.y.signum() * step,
        ScrollDelta::Pixels(p) if !stepped && f32::from(p.y).abs() >= 0.01 => f32::from(p.y) / PX_PER_STEP * step,
        // Stepped parameters on a trackpad: one choice per long enough swipe event.
        ScrollDelta::Pixels(p) if stepped && f32::from(p.y).abs() >= PX_PER_STEP / 5. => f32::from(p.y).signum() * step,
        _ => return None,
    };
    Some(change)
}

/// A painted plot whose handles drag parameters. `paint` draws into the
/// plot's rect (clipped to it); the handles themselves are part of that
/// drawing, this only makes them interactive.
pub fn plot(
    ws: &mut Workspace,
    dev: &Dev,
    cfg: PlotCfg,
    cx: &mut Context<Workspace>,
    paint: impl FnOnce(Rect, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let region = ws.fx.region(dev.id, cfg.slot);
    let handles = Rc::new(cfg.handles);
    let focus = cfg.keys.then(|| ws.fx.focus(dev.id, cx));
    let store = region.clone();
    let surface = canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, cx| window.with_content_mask(Some(ContentMask { bounds: b }), |window| paint(Rect::of(b), window, cx)),
    )
    .size_full();

    let (d, r, h, f, on_empty) = (dev.clone(), region.clone(), handles.clone(), focus.clone(), cfg.on_empty.clone());
    let down = cx.listener(move |this, e: &MouseDownEvent, window, cx| {
        cx.stop_propagation();
        if let Some(f) = &f {
            f.focus(window, cx);
        }
        let b = r.get();
        match nearest(&h, b, e.position, GRAB) {
            Some(handle) => {
                this.fx.selected.insert(d.id, handle.index);
                if e.click_count >= 2 {
                    if !handle.reset.is_empty() {
                        set_many(this, "Reset", d.id, &handle.reset);
                    }
                } else {
                    let (w, hh) = (f32::from(b.size.width), f32::from(b.size.height));
                    let drag = FxDrag {
                        device: d.id,
                        x: axis_of(this, &d, handle.x, w),
                        y: axis_of(this, &d, handle.y, hh),
                        alt: axis_of(this, &d, handle.alt, hh),
                        last: (f32::from(e.position.x), f32::from(e.position.y)),
                    };
                    begin_drag(this, drag, cx);
                }
            }
            None => {
                if e.click_count >= 2
                    && let Some(add) = &on_empty
                {
                    let r = Rect::of(b);
                    add(this, ((f32::from(e.position.x) - r.x) / r.w).clamp(0., 1.), ((f32::from(e.position.y) - r.y) / r.h).clamp(0., 1.), cx);
                }
            }
        }
        cx.notify();
    });

    let (d, r, h, on_context) = (dev.clone(), region.clone(), handles.clone(), cfg.on_context.clone());
    let context = cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        if let (Some(handle), Some(open)) = (nearest(&h, r.get(), e.position, GRAB), &on_context) {
            cx.stop_propagation();
            this.fx.selected.insert(d.id, handle.index);
            open(this, handle.index, e.position, cx);
            cx.notify();
        }
    });

    let (d, r, h) = (dev.clone(), region.clone(), handles.clone());
    let scroll = cx.listener(move |this, e: &ScrollWheelEvent, _, cx| {
        let selected = this.fx.selected.get(&d.id).copied();
        let handle = nearest(&h, r.get(), e.position, 2. * GRAB).or_else(|| h.iter().find(|x| Some(x.index) == selected));
        let Some(spec) = handle.and_then(|x| x.alt) else { return };
        let Some(info) = d.info(spec.param) else { return };
        // Cmd/Ctrl + wheel is a zoom gesture: leave it to the view around.
        if crate::gesture::is_zoom(e) {
            return;
        }
        let stepped = matches!(info.scale, ParamScale::Stepped);
        let Some(change) = wheel_change(e.delta, stepped, e.modifiers.shift, (info.max - info.min).max(1.)) else { return };
        cx.stop_propagation();
        let v = (current(this, d.id, info) + change).clamp(0., 1.);
        set_gliding(this, d.id, &[(spec.param, v)]);
        cx.notify();
    });

    let pal = p();
    let mut root = div()
        .id(("plot", dev.key(0xF00 | cfg.slot as usize)))
        .relative()
        .size_full()
        .rounded(px(r_md()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .child(surface)
        .on_mouse_down(MouseButton::Left, down)
        .on_mouse_down(MouseButton::Right, context)
        .on_scroll_wheel(scroll);

    if let Some(focus) = focus {
        let (d, h) = (dev.clone(), handles.clone());
        root = root.track_focus(&focus).on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
            if h.is_empty() {
                return;
            }
            let selected = this.fx.selected.get(&d.id).copied().unwrap_or(h[0].index);
            let at = h.iter().position(|x| x.index == selected).unwrap_or(0);
            let handle = &h[at];
            let mods = e.keystroke.modifiers;
            let step = if mods.shift { 0.002 } else { 0.01 };
            // The sign of an axis says which way is "up" or "right" on screen.
            let nudge = |spec: Option<AxisSpec>, dir: f32| spec.map(|s| (s.param, dir * step * s.per_frac.signum()));
            let change = match e.keystroke.key.as_str() {
                "left" => nudge(handle.x, -1.),
                "right" => nudge(handle.x, 1.),
                "up" => nudge(if mods.alt { handle.alt } else { handle.y.or(handle.alt) }, -1.),
                "down" => nudge(if mods.alt { handle.alt } else { handle.y.or(handle.alt) }, 1.),
                "[" | "]" => {
                    let next = if e.keystroke.key == "]" { (at + 1) % h.len() } else { (at + h.len() - 1) % h.len() };
                    this.fx.selected.insert(d.id, h[next].index);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                _ => return,
            };
            cx.stop_propagation();
            if let Some((param, delta)) = change
                && let Some(info) = d.info(param)
            {
                let v = (current(this, d.id, info) + delta).clamp(0., 1.);
                set_gliding(this, d.id, &[(param, v)]);
                cx.notify();
            }
        }));
    }
    root
}

// ───────────────────────────── Controls ─────────────────────────────

/// A compact number: the parameter's short name over its value. Drag
/// vertically to change it (Shift = fine), click and type to enter it
/// (Enter applies, Escape cancels), double-click for the default.
pub fn num(ws: &mut Workspace, dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    num_w(ws, dev, id, 60., cx)
}

pub fn num_w(
    ws: &mut Workspace,
    dev: &Dev,
    id: u32,
    width: f32,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let value = dev.norm(id);
    let default = info.default_normalized();
    let device = dev.id;
    if let Some(edit) = ws
        .num_edit
        .as_ref()
        .filter(|e| e.target == NumTarget::Param { device, param: id })
    {
        // Being typed into: the text with a cursor (keys are routed at the
        // workspace root, so every focused view reaches them).
        let text = edit.text.clone();
        return div()
            .id(("num", dev.key(id as usize)))
            .flex_none()
            .w(px(width))
            .h(px(32.))
            .px(px(SP1 + 2.))
            .flex()
            .flex_col()
            .justify_center()
            .rounded(px(r_sm()))
            .bg(content(0.08))
            .border_1()
            .border_color(accent_alpha(0.8))
            .overflow_hidden()
            .tooltip(widgets::tip(info.name, None))
            .child(label(
                paint::fit(info.short, width - 8., TEXT_XS),
                TEXT_XS,
                pal.faint,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(mono(
                        paint::fit(&text, width - 10., TEXT_XS),
                        TEXT_XS,
                        pal.text,
                    ))
                    .child(div().w(px(1.)).h(px(10.)).ml(px(1.)).bg(pal.accent)),
            )
            .into_any_element();
    }
    let fill = if info.min < 0. && info.max > 0. {
        (value - 0.5).abs() * 2.
    } else {
        value
    };
    let info_click = info.clone();
    div()
        .id(("num", dev.key(id as usize)))
        .flex_none()
        .w(px(width))
        .h(px(32.))
        .px(px(SP1 + 2.))
        .relative()
        .flex()
        .flex_col()
        .justify_center()
        .rounded(px(r_sm()))
        .bg(content(0.04))
        .hover(|s| s.bg(content(0.08)))
        .cursor_ns_resize()
        .overflow_hidden()
        .tooltip(widgets::tip(info.name, None))
        .child(label(paint::fit(info.short, width - 8., TEXT_XS), TEXT_XS, pal.faint))
        .child(mono(paint::fit(&info.format(value), width - 6., TEXT_XS), TEXT_XS, if dev.enabled { pal.text } else { pal.muted }))
        .child(div().absolute().left_0().bottom_0().h(px(2.)).w(px((width * fill).max(0.))).bg(with_alpha(if dev.enabled { pal.accent } else { pal.faint }, 0.7)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if e.click_count >= 2 {
                    this.num_edit = None;
                    set_now(this, device, id, default);
                    cx.notify();
                    return;
                }
                let value = current(this, device, &info);
                this.begin_drag(Drag::Knob { target: KnobTarget::Param { device, param: id, default }, last_y: f32::from(e.position.y), value }, cx);
            }),
        )
        // A click that didn't move opens the box for typing (a moved click
        // was a drag; double-click resets, handled above on mouse-down).
        .on_click({
            let info = info_click;
            cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() >= 2 || click_dragged(e) {
                    return;
                }
                if matches!(&this.num_edit, Some(edit) if edit.target == NumTarget::Param { device, param: id }) {
                    return;
                }
                let plain = info.to_plain(current(this, device, &info));
                this.begin_num_edit(NumTarget::Param { device, param: id }, plain_text(plain as f64), cx);
            })
        })
        .into_any_element()
}

/// A stepped parameter: an on/off chip for switches, otherwise a chip that
/// opens the list of choices.
pub fn pick(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let device = dev.id;
    let now = dev.choice(id);
    let switch = info.choices == ["Off", "On"];
    let on = switch && now == 1;
    let text: String = if switch { info.short.to_owned() } else { info.choices.get(now).map_or_else(|| dev.text(id), |s| (*s).to_owned()) };
    div()
        .id(("pick", dev.key(id as usize)))
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { accent_alpha(0.2) } else { content(0.06) })
        .hover(|s| s.bg(if on { accent_alpha(0.28) } else { content(0.12) }))
        .tooltip(widgets::tip(info.name, None))
        .child(label(text, TEXT_XS, if on { pal.accent } else { pal.text }).font_weight(FontWeight::MEDIUM))
        .when(!switch && info.choices.len() > 2, |d| d.child(icon(Icon::ChevronDown, 10., pal.faint)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            if info.choices.len() <= 2 {
                let next = if now == 0 { 1. } else { 0. };
                set_now(this, device, id, info.to_normalized(info.min + next));
            } else {
                let at = e.position();
                this.fx.open_pop(PopKind::Choice { device, param: id }, point(at.x - px(8.), at.y + px(12.)));
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The right control for a parameter: a chip for choices, a number otherwise.
pub fn ctl(ws: &mut Workspace, dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    match dev.info(id) {
        Some(i) if matches!(i.scale, ParamScale::Stepped) && !i.choices.is_empty() => pick(dev, id, cx),
        Some(_) => num(ws, dev, id, cx),
        None => div().into_any_element(),
    }
}

/// A wrapping row of controls: `primary` always, `more` in the expanded editor.
pub fn strip(
    ws: &mut Workspace,
    dev: &Dev,
    primary: &[u32],
    more: &[u32],
    cx: &mut Context<Workspace>,
) -> Div {
    let mut row = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1));
    for id in primary.iter().chain(if dev.big { more.iter() } else { [].iter() }) {
        row = row.child(ctl(ws, dev, *id, cx));
    }
    row
}

/// The root of an editor: fixed width, the dock's height, a column.
pub fn panel(width: f32) -> Div {
    div().flex_none().w(px(width)).h_full().p(px(SP2)).gap(px(SP2)).flex().flex_col()
}

/// A small `label value` read-out in a box.
pub fn readout(name: &str, value: String, color: Hsla) -> Div {
    let pal = p();
    div()
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .child(label(name.to_owned(), TEXT_XS, pal.faint))
        .child(mono(value, TEXT_XS, color))
}

pub fn fmt_db(db: f32) -> String {
    if db <= -99. { "-inf".into() } else { format!("{db:+.1}") }
}

// ───────────────────────────── Samples ─────────────────────────────

/// Min/max overviews of audio files, decoded once.
#[derive(Default)]
pub struct WaveCache {
    map: HashMap<String, Rc<Vec<(f32, f32)>>>,
}

/// Columns of a cached overview.
pub const OVERVIEW: usize = 512;

impl WaveCache {
    /// The overview of a WAV file (empty when it cannot be read).
    pub fn get(&mut self, path: &str) -> Rc<Vec<(f32, f32)>> {
        if let Some(w) = self.map.get(path) {
            return w.clone();
        }
        let w = Rc::new(overview(path).unwrap_or_default());
        self.map.insert(path.to_owned(), w.clone());
        w
    }
}

fn overview(path: &str) -> Option<Vec<(f32, f32)>> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let frames = reader.duration() as usize;
    if frames == 0 {
        return None;
    }
    let scale = 1. / (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
    let mono: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect::<Vec<_>>().chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect(),
        hound::SampleFormat::Int => {
            reader.samples::<i32>().filter_map(Result::ok).collect::<Vec<_>>().chunks(ch).map(|c| c.iter().map(|v| *v as f32 * scale).sum::<f32>() / ch as f32).collect()
        }
    };
    Some(peaks(&mono, OVERVIEW))
}

/// `(min, max)` of `n` equal slices of a signal.
pub fn peaks(samples: &[f32], n: usize) -> Vec<(f32, f32)> {
    if samples.is_empty() || n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| {
            let a = i * samples.len() / n;
            let b = (((i + 1) * samples.len()) / n).max(a + 1).min(samples.len());
            samples[a..b].iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
        })
        .collect()
}

/// Sample paths a stock sampler keeps in its state: the single sample and
/// the drum pads.
pub fn state_paths(state: Option<&[u8]>) -> (Option<String>, Vec<Option<String>>) {
    let Some(root) = state.and_then(|s| serde_json::from_slice::<serde_json::Value>(s).ok()) else { return (None, Vec::new()) };
    let extra = root.get("extra").unwrap_or(&root);
    let path = extra.get("path").and_then(|v| v.as_str()).map(str::to_owned);
    let pads = extra.get("pads").and_then(|v| v.as_array()).map(|a| a.iter().map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
    (path, pads)
}

/// Paint a min/max overview into `r` (mirrored around its centre line).
pub fn paint_wave(window: &mut Window, r: Rect, wave: &[(f32, f32)], color: Hsla) {
    if wave.is_empty() {
        return;
    }
    let cols = (r.w / 2.).floor().max(1.) as usize;
    let mid = r.y + r.h / 2.;
    let half = r.h / 2. - 2.;
    for c in 0..cols {
        let a = c * wave.len() / cols;
        let b = ((c + 1) * wave.len() / cols).max(a + 1).min(wave.len());
        let (lo, hi) = wave[a..b].iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v.0), hi.max(v.1)));
        let (top, bottom) = (mid - hi.clamp(-1., 1.) * half, mid - lo.clamp(-1., 1.) * half);
        window.paint_quad(paint::fill_rect(paint::rect(r.x + c as f32 * 2., top, 1.5, (bottom - top).max(1.)), color));
    }
}


/// A narrow numeric control for table rows (same gestures as `num`).
pub fn slim(
    ws: &mut Workspace,
    dev: &Dev,
    id: u32,
    width: f32,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    num_w(ws, dev, id, width, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frequency_axis_round_trips() {
        for f in [20., 55., 440., 1_000., 9_500., 20_000.] {
            assert!((frac_freq(freq_frac(f)) / f - 1.).abs() < 1e-4, "{f}");
        }
        assert_eq!(freq_frac(20.), 0.);
        assert!((freq_frac(20_000.) - 1.).abs() < 1e-6);
        assert!((freq_frac(632.46) - 0.5).abs() < 1e-3, "the geometric middle of the axis");
    }

    #[test]
    fn log_parameters_track_the_axis() {
        // A full-range frequency parameter moves 1:1 with the axis…
        let full = ParamInfo::new(0, "f", 20., 20_000., 1_000.).log();
        assert!((log_span(&full) - 1.).abs() < 1e-6);
        // …a narrower one crosses its whole range in part of the plot.
        let narrow = ParamInfo::new(0, "f", 40., 2_000., 200.).log();
        let span = log_span(&narrow);
        let (a, b) = (freq_frac(100.), freq_frac(800.));
        let moved = narrow.to_normalized(100.) + (b - a) * span;
        assert!((narrow.to_plain(moved) / 800. - 1.).abs() < 1e-3);
        assert_eq!(log_span(&ParamInfo::new(0, "g", -12., 12., 0.)), 1.);
    }

    #[test]
    fn peaks_and_state_paths() {
        let s: Vec<f32> = (0..1000).map(|i| if i < 500 { 0.5 } else { -0.25 }).collect();
        let p = peaks(&s, 4);
        assert_eq!(p, vec![(0.5, 0.5), (0.5, 0.5), (-0.25, -0.25), (-0.25, -0.25)]);
        assert!(peaks(&[], 8).is_empty());
        let state = br#"{"version":1,"params":{},"extra":{"path":"a.wav","pads":["k.wav",null]}}"#;
        let (path, pads) = state_paths(Some(state));
        assert_eq!(path.as_deref(), Some("a.wav"));
        assert_eq!(pads, vec![Some("k.wav".to_owned()), None]);
        assert_eq!(state_paths(None), (None, Vec::new()));
        assert!(WaveCache::default().get("does/not/exist.wav").is_empty());
    }

    #[test]
    fn rects_map_fractions_to_pixels() {
        let r = Rect { x: 10., y: 20., w: 200., h: 100. };
        assert_eq!((r.px(0.5), r.py(0.25)), (110., 45.));
        let (l, rest) = r.split_left(50.);
        assert_eq!((l.w, rest.x, rest.w), (50., 60., 150.));
        assert_eq!(r.inset(5.), Rect { x: 15., y: 25., w: 190., h: 90. });
    }

    #[test]
    fn wheel_steps_are_per_notch_and_smooth_on_a_trackpad() {
        use gpui::{point, px};
        assert_eq!(wheel_change(ScrollDelta::Lines(point(0., 1.)), false, false, 1.), Some(0.04));
        assert_eq!(wheel_change(ScrollDelta::Lines(point(0., -3.)), false, true, 1.), Some(-0.008));
        // Five 5 px trackpad events move as much as one notch.
        let small = wheel_change(ScrollDelta::Pixels(point(px(0.), px(5.))), false, false, 1.).unwrap();
        assert!((small * 5. - 0.04).abs() < 1e-6);
        assert_eq!(wheel_change(ScrollDelta::Pixels(point(px(0.), px(0.))), false, false, 1.), None);
        // A stepped parameter moves a whole choice, and ignores tiny swipes.
        assert_eq!(wheel_change(ScrollDelta::Lines(point(0., 1.)), true, false, 4.), Some(0.25));
        assert_eq!(wheel_change(ScrollDelta::Pixels(point(px(0.), px(1.))), true, false, 4.), None);
        assert_eq!(wheel_change(ScrollDelta::Pixels(point(px(0.), px(-9.))), true, false, 4.), Some(-0.25));
    }
}
