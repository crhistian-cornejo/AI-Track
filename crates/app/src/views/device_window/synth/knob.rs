//! Knobs that show and take modulation: the arc of the value, one ring per
//! routing that targets the knob (in its source's colour, over the range it
//! sweeps), and a drop target for the drag handles of the envelopes, LFOs
//! and macros. Alt + drag changes the depth of the knob's newest routing;
//! right-click removes it.

use super::geom::{clear_slot, ring_span, route_drop, slot_is_bipolar};
use super::source_color;
use crate::paint;
use crate::theme::*;
use crate::views::device_ui::{Dev, current, set_many, set_now};
use crate::widgets::{self, label, mono};
use crate::workspace::{Drag, KnobTarget, Workspace};
use aitrack_stock::instrument::wavetable::modulation::{ModSlot, source_name};
use gpui::{
    Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, point, px, AppContext,
};
use std::rc::Rc;

const A0: f32 = std::f32::consts::PI * 0.75;
const SPAN: f32 = std::f32::consts::PI * 1.5;

/// A modulation source being dragged onto a knob.
#[derive(Clone, Debug)]
pub struct ModSource {
    pub src: u8,
}

impl Render for ModSource {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        let color = source_color(self.src);
        div()
            .h(px(24.))
            .px(px(SP2))
            .gap(px(SP1))
            .flex()
            .items_center()
            .rounded(px(r_md()))
            .bg(pal.raised)
            .border_1()
            .border_color(with_alpha(color, 0.8))
            .shadow_md()
            .child(div().size(px(8.)).rounded_full().bg(color))
            .child(label(source_name(self.src), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
            .child(label("→ drop on a knob", TEXT_XS, pal.faint))
    }
}

/// The drag handle of a modulation source (a small target glyph).
pub fn source_handle(dev: &Dev, src: u8) -> Stateful<Div> {
    let color = source_color(src);
    div()
        .id(("mod-src", dev.key(0xE00 | src as usize)))
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(with_alpha(color, 0.18))
        .border_1()
        .border_color(with_alpha(color, 0.6))
        .cursor_grab()
        .hover(|s| s.bg(with_alpha(color, 0.32)))
        .tooltip(widgets::tip("Drag onto any knob to modulate it", None))
        .child(div().size(px(6.)).rounded_full().bg(color))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_drag(ModSource { src }, |s: &ModSource, _, _, cx| cx.new(|_| s.clone()))
}

/// The routings of `slots` that target `id`, newest last.
pub fn rings_of(slots: &[ModSlot], id: u32) -> Vec<(usize, ModSlot)> {
    ModSlot::targeting(slots, id).into_iter().map(|s| (s, slots[s])).collect()
}

fn paint_knob(window: &mut Window, b: gpui::Bounds<gpui::Pixels>, value: f32, bipolar: bool, color: gpui::Hsla, rings: &[(f32, f32, gpui::Hsla)]) {
    let pal = p();
    let c = b.center();
    let side = f32::from(b.size.width.min(b.size.height));
    let r = side / 2. - 3. - 4. * rings.len().min(3) as f32;
    let r = r.max(6.);
    let v = value.clamp(0., 1.);
    let ang = A0 + SPAN * v;
    let (a, z) = if bipolar { let mid = A0 + SPAN / 2.; (mid.min(ang), mid.max(ang)) } else { (A0, ang) };
    paint::arc(window, c, r, A0, A0 + SPAN, 3., content(0.12));
    if z - a > 1e-3 {
        paint::arc(window, c, r, a, z, 3., color);
    }
    // Modulation rings outside the value arc.
    for (k, (lo, hi, col)) in rings.iter().take(3).enumerate() {
        let rr = r + 4. + 4. * k as f32;
        paint::arc(window, c, rr, A0, A0 + SPAN, 2., with_alpha(*col, 0.12));
        if hi - lo > 1e-3 {
            paint::arc(window, c, rr, A0 + SPAN * lo, A0 + SPAN * hi, 2., *col);
        }
    }
    let body = r - 4.;
    window.paint_quad(paint::rrect(gpui::Bounds::new(point(c.x - px(body), c.y - px(body)), gpui::size(px(body * 2.), px(body * 2.))), body, pal.raised));
    let p0 = point(c.x + px((body * 0.25) * ang.cos()), c.y + px((body * 0.25) * ang.sin()));
    let p1 = point(c.x + px((body - 1.) * ang.cos()), c.y + px((body - 1.) * ang.sin()));
    paint::polyline(window, &[p0, p1], 2., pal.text);
}

/// A labelled, modulatable knob of parameter `id`. `side` is the knob's
/// diameter (rings included); the cell is `side + 16` wide.
pub fn mknob(_ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, id: u32, side: f32, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let Some(info) = dev.info(id).cloned() else { return div() };
    let device = dev.id;
    let value = dev.norm(id);
    let default = info.default_normalized();
    let bipolar = info.min < 0. && info.max > 0.;
    let color = if dev.enabled { pal.accent } else { pal.faint };
    let rings = rings_of(slots, id);
    let spans: Vec<(f32, f32, gpui::Hsla)> = rings
        .iter()
        .map(|(_, m)| {
            let (lo, hi) = ring_span(value, m.amount, slot_is_bipolar(m));
            (lo, hi, source_color(m.source))
        })
        .collect();
    let newest = rings.last().map(|(s, _)| *s);
    let width = side + 16.;
    let tip = if rings.is_empty() {
        format!("{}\nDrag an envelope, LFO or macro handle here to modulate it", info.name)
    } else {
        let list: Vec<String> = rings.iter().map(|(_, m)| format!("{} {:+.0}%", source_name(m.source), m.amount * 100.)).collect();
        format!("{}\nModulated by {}\nAlt+drag: depth of the newest · right-click: remove it", info.name, list.join(", "))
    };
    let dial = div()
        .id(("mknob", dev.key(id as usize)))
        .flex_none()
        .size(px(side))
        .rounded_full()
        .cursor_ns_resize()
        .drag_over::<super::knob::ModSource>(|s, _, _, _| s.bg(accent_alpha(0.22)))
        .child(canvas(|_, _, _| {}, move |b, _, window, _| paint_knob(window, b, value, bipolar, color, &spans)).size_full())
        .tooltip(widgets::tip_owned(tip))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if e.click_count >= 2 {
                    set_now(this, device, id, default);
                    cx.notify();
                    return;
                }
                // Alt + drag: the depth of the newest routing on this knob.
                if e.modifiers.alt
                    && let Some(s) = newest
                {
                    let amt = aitrack_stock::instrument::wavetable::p::slot_x(s as u32, aitrack_stock::instrument::wavetable::p::M_AMT);
                    if let Some(i) = this.fx.param_info(this.b.plugins.as_ref(), &this.s.project, device, amt) {
                        let v = current(this, device, &i);
                        this.begin_drag(Drag::Knob { target: KnobTarget::Param { device, param: amt, default: 0.5 }, last_y: f32::from(e.position.y), value: v }, cx);
                    }
                    return;
                }
                let v = this.fx.param_info(this.b.plugins.as_ref(), &this.s.project, device, id).map_or(value, |i| current(this, device, &i));
                this.begin_drag(Drag::Knob { target: KnobTarget::Param { device, param: id, default }, last_y: f32::from(e.position.y), value: v }, cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                let Some(s) = newest else { return };
                cx.stop_propagation();
                clear_routing(this, device, s);
                cx.notify();
            }),
        )
        .on_drop(cx.listener(move |this, source: &ModSource, _, cx| {
            add_routing(this, device, source.src, id, cx);
            cx.notify();
        }));
    div()
        .flex_none()
        .w(px(width))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(label(paint::fit(info.short, width, TEXT_XS), TEXT_XS, pal.muted))
        .child(dial)
        .child(mono(paint::fit(&dev.text(id), width, TEXT_XS), TEXT_XS, if rings.is_empty() { pal.text } else { source_color(rings[rings.len() - 1].1.source) }))
}

/// Plain values → one sealed edit of a device's parameters.
pub fn set_plain(ws: &mut Workspace, label: &str, device: aitrack_model::DeviceId, plain: &[(u32, f32)]) {
    let norms: Vec<(u32, f32)> =
        plain.iter().filter_map(|&(id, v)| ws.fx.param_info(ws.b.plugins.as_ref(), &ws.s.project, device, id).map(|i| (id, i.to_normalized(v)))).collect();
    set_many(ws, label, device, &norms);
}

/// The slots of a device's matrix as they are in the project.
pub fn read_slots(ws: &mut Workspace, device: aitrack_model::DeviceId) -> Vec<ModSlot> {
    let plugins = ws.b.plugins.as_ref();
    let project = &ws.s.project;
    let set = match project.device(device).map(|d| &d.kind) {
        Some(aitrack_model::DeviceKind::Stock { plugin_id }) => ws.fx.params(plugins, plugin_id),
        _ => return Vec::new(),
    };
    let d = project.device(device);
    ModSlot::read_all(|id| {
        let Some(i) = set.get(id) else { return 0. };
        let norm = d.and_then(|d| d.params.get(&id).copied()).unwrap_or_else(|| i.default_normalized());
        i.to_plain(norm)
    })
}

/// Route `src` to parameter `dest` (drag-and-drop; one undo step).
pub fn add_routing(ws: &mut Workspace, device: aitrack_model::DeviceId, src: u8, dest: u32, cx: &mut Context<Workspace>) {
    let Some(info) = ws.fx.param_info(ws.b.plugins.as_ref(), &ws.s.project, device, dest) else { return };
    let slots = read_slots(ws, device);
    match route_drop(&slots, src, &info) {
        Some((_, edits)) => set_plain(ws, "Add modulation", device, &edits),
        None if !aitrack_stock::instrument::wavetable::modulation::is_modulatable(&info) => {}
        None => ws.toast("The modulation matrix is full (32 routings)", cx),
    }
}

pub fn clear_routing(ws: &mut Workspace, device: aitrack_model::DeviceId, slot: usize) {
    set_plain(ws, "Remove modulation", device, &clear_slot(slot));
}

/// The cell a knob row expects for parameters without a knob (choices).
pub fn chip_cell(dev: &Dev, id: u32, width: f32, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let name = dev.info(id).map_or("", |i| i.short);
    div()
        .flex_none()
        .w(px(width))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SP1))
        .child(label(paint::fit(name, width, TEXT_XS), TEXT_XS, pal.muted))
        .child(crate::views::device_ui::kit::pick(dev, id, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::instrument::wavetable::{p, src};

    #[test]
    fn rings_list_the_routings_of_a_knob() {
        let mut slots = vec![ModSlot::default(); 32];
        slots[3] = ModSlot { source: src::LFO2, dest: p::CUTOFF, amount: 0.3, ..Default::default() };
        slots[7] = ModSlot { source: src::MACRO1, dest: p::CUTOFF, amount: -0.2, ..Default::default() };
        slots[9] = ModSlot { source: src::ENV2, dest: p::VOLUME, amount: 0.5, ..Default::default() };
        let r = rings_of(&slots, p::CUTOFF);
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![3, 7]);
        assert!(rings_of(&slots, p::RESONANCE).is_empty());
    }
}
