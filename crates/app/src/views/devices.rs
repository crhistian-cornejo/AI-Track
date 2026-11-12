//! Device View (Live's Detail View): the selected track's chain as a row of
//! device boxes in signal order, each with a title bar (on/off, fold, name)
//! and its editor; folded boxes are a narrow strip with the name running
//! down. Drag a title bar to move a device; the gaps between boxes are the
//! insertion points. The chain scrolls sideways when it is wider than the
//! panel. Knobs come
//! from each device's `ParamInfo` list; racks show their 16 macros (and a
//! pad grid for drum racks).

use crate::icons::{Icon, icon};
use crate::state::drum_pads;
use crate::theme::*;
use crate::views::menus::device_icon;
use crate::widgets::{self, badge, icon_chip, knob, knob_cell, label, mono};
use crate::workspace::{Drag, KnobTarget, MenuKind, Workspace};
use aitrack_model::{Device, DeviceId, DeviceKind, MACRO_COUNT, PluginFormat, Rack, RackKind, TrackId};
use gpui::{
    AnyElement, AppContext, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Stateful, StatefulInteractiveElement, Styled, Window, div, point, prelude::FluentBuilder, px,
};

const CELL_W: f32 = 56.;
const KNOB: f32 = 32.;
/// Height of a device's title bar.
const TITLE_H: f32 = 28.;
/// Width of a folded device.
const FOLDED_W: f32 = 28.;
/// Letters of a folded device's name (the rest is cut).
const MAX_FOLDED_CHARS: usize = 16;

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(track) = ws.s.selected_track.and_then(|t| ws.s.project.track(t)).cloned() else {
        return empty("Select a track to see its devices.");
    };
    let tid = track.id;
    let ids: Vec<DeviceId> = track.devices.iter().map(|d| d.id).collect();
    // Live's chain: devices left to right in signal order, an insertion
    // point between each pair (it lights up while a device is dragged over).
    let mut row = div().h_full().flex().px(px(SP1)).py(px(SP2));
    for (index, d) in track.devices.iter().enumerate() {
        row = row.child(insertion(tid, index, &ids, cx)).child(card(ws, track.id, index, d, cx));
    }
    row = row.child(insertion(tid, ids.len(), &ids, cx));
    row = row.child(
        div()
            .id("add-device")
            .flex_none()
            .w(px(if ids.is_empty() { 240. } else { 120. }))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SP2))
            .rounded(px(r_lg()))
            .border_1()
            .border_dashed()
            .border_color(border_strong())
            .cursor_pointer()
            .hover(|s| s.bg(hover()))
            // Dropping a device here sends it to the end of the chain.
            .drag_over::<DraggedDevice>(|s, _, _, _| s.border_color(accent()))
            .on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
                move_device(this, d.id, tid, usize::MAX);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.open_menu(MenuKind::AddDevice(tid), point(at.x - px(116.), at.y - px(260.)), cx);
            }))
            .child(icon(Icon::Add, 20., pal.muted))
            .child(label(if ids.is_empty() { "Add an instrument or effect" } else { "Add device" }, TEXT_SM, pal.muted)),
    );
    // Only the x axis scrolls: a mouse wheel moves the chain sideways and a
    // trackpad pans it with its own pixel deltas.
    div().id("device-chain").size_full().overflow_x_scroll().child(row).into_any_element()
}

/// Where a device dropped on the insertion point before `slot` ends up,
/// counted after it has left its old place (what [`move_device`] takes).
pub fn insertion_index(chain: &[DeviceId], dragged: DeviceId, slot: usize) -> usize {
    match chain.iter().position(|d| *d == dragged) {
        Some(from) if from < slot => slot - 1,
        _ => slot,
    }
}

/// The gap before device `slot` of the chain: a drop target that shows
/// where the dragged device will land.
fn insertion(track: TrackId, slot: usize, chain: &[DeviceId], cx: &mut Context<Workspace>) -> AnyElement {
    let chain = chain.to_vec();
    div()
        .id(("insert-device", slot))
        .flex_none()
        .w(px(SP2 + SP1))
        .h_full()
        .py(px(SP1))
        .flex()
        .justify_center()
        .child(div().w(px(2.)).h_full().rounded(px(1.)))
        .drag_over::<DraggedDevice>(|s, _, _, _| s.bg(accent_alpha(0.55)).rounded(px(r_sm())))
        .on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
            move_device(this, d.id, track, insertion_index(&chain, d.id, slot));
            cx.notify();
        }))
        .into_any_element()
}

/// A device being dragged along its chain. The order of a chain is the
/// order the signal passes through it, so moving a device changes the sound.
#[derive(Clone, Debug)]
pub struct DraggedDevice {
    pub id: DeviceId,
    pub name: String,
}

impl gpui::Render for DraggedDevice {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        div()
            .px(px(SP2))
            .h(px(TAB_BAR_H))
            .flex()
            .items_center()
            .gap(px(SP2))
            .rounded(px(r_md()))
            .bg(pal.raised)
            .border_1()
            .border_color(accent())
            .shadow_lg()
            .child(icon(Icon::Device, 12., pal.accent))
            .child(label(self.name.clone(), TEXT_SM, pal.text))
    }
}

/// Dropping a dragged device on a card moves it to that card's place.
fn droppable(shell: Stateful<Div>, track: TrackId, index: usize, cx: &mut Context<Workspace>) -> Stateful<Div> {
    shell.drag_over::<DraggedDevice>(|s, _, _, _| s.border_color(accent())).on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
        move_device(this, d.id, track, index);
        cx.notify();
    }))
}

/// Move a device to `index` of `track`'s chain (one undo step). `index` is
/// the place it ends up in, counted after it has left its old place.
pub fn move_device(ws: &mut Workspace, id: DeviceId, track: TrackId, index: usize) {
    let Some(chain) = ws.s.project.track(track).map(|t| &t.devices) else { return };
    let last = chain.len().saturating_sub(1);
    let index = index.min(last);
    if chain.iter().position(|d| d.id == id) == Some(index) || !chain.iter().any(|d| d.id == id) {
        return;
    }
    ws.s.edit("Move device", vec![aitrack_model::Command::MoveDevice { id, to: aitrack_model::command::DeviceSlot::Track { track, index } }]);
}

pub fn empty(text: &'static str) -> AnyElement {
    div().size_full().flex().items_center().justify_center().child(label(text, TEXT_MD, p().faint)).into_any_element()
}

fn card_shell(id: DeviceId, enabled: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(("device", id.0 as usize))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(border())
        .when(!enabled, |d| d.opacity(0.62))
        .overflow_hidden()
}

fn card(ws: &mut Workspace, track: TrackId, index: usize, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let id = d.id;
    let enabled = d.enabled;
    let collapsed = d.collapsed;
    let (kind_label, ic) = match &d.kind {
        DeviceKind::Stock { plugin_id } => (
            ws.b.plugins.device(plugin_id).map_or("Device", |i| i.category.label()).to_owned(),
            device_icon(plugin_id, ws),
        ),
        DeviceKind::Plugin { format, .. } => ((if *format == PluginFormat::Vst3 { "VST3" } else { "CLAP" }).to_owned(), Icon::Plug),
        DeviceKind::Rack(r) => (
            match r.kind {
                RackKind::Drum => "Drum Rack",
                RackKind::Instrument => "Instrument Rack",
                RackKind::Audio => "Audio Rack",
                RackKind::Midi => "MIDI Rack",
            }
            .to_owned(),
            if r.kind == RackKind::Drum { Icon::Drum } else { Icon::Grid },
        ),
    };
    let power = icon_chip(("power", id.0 as usize), Icon::Power, enabled, pal.accent)
        .tooltip(widgets::tip("Device on/off", None))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.s.toggle_device(id);
            cx.notify();
        }));
    let fold = widgets::sized_icon_button(("fold-dev", id.0 as usize), if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, false, 20., 12.)
        .tooltip(widgets::tip(if collapsed { "Show the device" } else { "Fold the device" }, None))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            toggle_fold(this, track, id);
            cx.notify();
        }));
    if collapsed {
        // Folded (Live): a narrow box with the name running down it.
        let mut name = div().flex().flex_col().items_center().overflow_hidden().flex_1().min_h(px(0.));
        for ch in d.name.chars().take(MAX_FOLDED_CHARS) {
            name = name.child(label(ch.to_string(), TEXT_SM, if enabled { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD).h(px(13.)));
        }
        return droppable(card_shell(id, enabled), track, index, cx)
            .w(px(FOLDED_W))
            .cursor_grab()
            .on_drag(DraggedDevice { id, name: d.name.clone() }, |d: &DraggedDevice, _, _, cx| cx.new(|_| d.clone()))
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    toggle_fold(this, track, id);
                    cx.notify();
                }
            }))
            .tooltip(widgets::tip_owned(format!("{}: {kind_label}. Double-click to show it", d.name)))
            .items_center()
            .py(px(SP1))
            .gap(px(SP1))
            .child(power)
            .child(fold)
            .child(icon(ic, 12., pal.muted))
            .child(name)
            .into_any_element();
    }
    let has_presets = matches!(&d.kind, DeviceKind::Stock { plugin_id } if !ws.b.plugins.presets(plugin_id).is_empty());
    let preset_name = d.preset.clone().unwrap_or_else(|| "Presets".into());
    // The title bar (Live): on/off, fold, name; double-click folds, drag moves.
    let header = div()
        .id(("device-header", id.0 as usize))
        .cursor_grab()
        .on_drag(DraggedDevice { id, name: d.name.clone() }, |d: &DraggedDevice, _, _, cx| cx.new(|_| d.clone()))
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            if e.click_count() >= 2 {
                toggle_fold(this, track, id);
                cx.notify();
            }
        }))
        .flex_none()
        .h(px(TITLE_H))
        .pl(px(SP1))
        .pr(px(SP1))
        .gap(px(SP1))
        .flex()
        .items_center()
        .bg(if enabled { content(0.05) } else { content(0.02) })
        .border_b_1()
        .border_color(border())
        .child(power)
        .child(fold)
        .child(icon(ic, 12., pal.muted))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .child(label(d.name.clone(), TEXT_MD, if enabled { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD)),
        )
        .when(has_presets, |h| {
            h.child(
                div()
                    .id(("presets", id.0 as usize))
                    .flex_none()
                    .h(px(20.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .gap(px(SP1))
                    .rounded(px(r_sm()))
                    .bg(content(0.06))
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.12)))
                    .child(label(crate::paint::fit(&preset_name, 96., TEXT_XS), TEXT_XS, pal.muted))
                    .child(icon(Icon::ChevronDown, 10., pal.faint))
                    .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                        cx.stop_propagation();
                        let at = e.position();
                        this.open_menu(MenuKind::Presets(id), point(at.x - px(8.), at.y + px(12.)), cx);
                    })),
            )
        })
        .when(!matches!(&d.kind, DeviceKind::Stock { .. }), |h| h.child(badge(kind_label, pal.muted)))
        .when(matches!(&d.kind, DeviceKind::Stock { plugin_id } if crate::views::device_ui::has_editor(plugin_id)), |h| {
            let big = ws.fx.big.contains(&id);
            h.child(
                widgets::sized_icon_button(("grow-dev", id.0 as usize), if big { Icon::ZoomOut } else { Icon::ZoomIn }, big, 20., 12.)
                    .tooltip(widgets::tip(if big { "Compact editor" } else { "Large editor" }, None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.fx.big.remove(&id) {
                            this.fx.big.insert(id);
                        }
                        cx.notify();
                    })),
            )
        })
        .child(
            widgets::sized_icon_button(("remove-dev", id.0 as usize), Icon::X, false, 20., 12.)
                .tooltip(widgets::tip("Remove device", None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.s.remove_device(id);
                    cx.notify();
                })),
        );

    let body: AnyElement = match &d.kind {
        DeviceKind::Rack(rack) => rack_body(ws, track, id, rack, enabled, cx),
        DeviceKind::Plugin { vendor, path, .. } => div()
            .w(px(232.))
            .p(px(SP3))
            .flex()
            .flex_col()
            .gap(px(SP2))
            .child(label(vendor.clone(), TEXT_SM, pal.muted))
            .child(label(crate::paint::fit(&path.display().to_string(), 208., TEXT_XS), TEXT_XS, pal.faint))
            .child(div().h(px(SP2)))
            .child(widgets::text_button(("open-editor", id.0 as usize), "Open plugin editor", Some(Icon::Device), false).on_click(cx.listener(move |this, _, _, cx| {
                if let Err(e) = this.b.engine.open_editor(id) {
                    this.toast(e, cx);
                }
            })))
            .into_any_element(),
        DeviceKind::Stock { plugin_id } => {
            // Every stock device draws its own editor: the display is the control.
            let bpm = ws.s.project.tempo.initial_bpm();
            let dev = ws.fx.dev(ws.b.engine.as_mut(), ws.b.plugins.as_ref(), d, plugin_id, bpm);
            crate::views::device_ui::editor(ws, plugin_id, &dev, d, cx)
        }
    };
    droppable(card_shell(id, enabled), track, index, cx).child(header).child(div().flex_1().min_h(px(0.)).child(body)).into_any_element()
}

/// Fold or unfold a device. Folding is view state stored on the device, so
/// it goes through a command too.
fn toggle_fold(ws: &mut Workspace, track: TrackId, id: DeviceId) {
    let Some(mut dev) = ws.s.project.device(id).cloned() else { return };
    let Some(slot) = find_slot(&ws.s.project, track, id) else { return };
    dev.collapsed = !dev.collapsed;
    ws.s.edit(if dev.collapsed { "Fold device" } else { "Show device" }, vec![
        aitrack_model::Command::RemoveDevice { id },
        aitrack_model::Command::AddDevice { slot, device: Box::new(dev) },
    ]);
}

/// The slot a device sits in on `track`'s top-level chain.
fn find_slot(p: &aitrack_model::Project, track: TrackId, id: DeviceId) -> Option<aitrack_model::command::DeviceSlot> {
    let index = p.track(track)?.devices.iter().position(|d| d.id == id)?;
    Some(aitrack_model::command::DeviceSlot::Track { track, index })
}

/// A knob wired to a drag; double-click resets to the default, Shift drags fine.
pub fn knob_control(
    id: impl Into<gpui::ElementId>,
    value: f32,
    bipolar: bool,
    enabled: bool,
    target: KnobTarget,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let color = if enabled { accent() } else { p().faint };
    knob(id, value, bipolar, KNOB, color).on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        cx.stop_propagation();
        if e.click_count >= 2 {
            match target {
                KnobTarget::Param { device, param, default } => {
                    this.s.set_param(device, param, default);
                }
                KnobTarget::Macro { rack, index } => {
                    this.s.set_macro(rack, index, 0.);
                }
                KnobTarget::Pan { track } => {
                    this.s.set_track(track, aitrack_model::TrackProp::Pan(0.));
                }
                KnobTarget::Send { track, index } => {
                    this.s.set_send(track, index, -70.);
                }
            }
            this.s.seal();
            cx.notify();
            return;
        }
        this.begin_drag(Drag::Knob { target, last_y: f32::from(e.position.y), value }, cx);
    }))
}

fn rack_body(ws: &mut Workspace, track: TrackId, id: DeviceId, rack: &Rack, enabled: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let mut macros = div().w(px(8. * CELL_W)).flex().flex_wrap().gap_y(px(SP3));
    for i in 0..MACRO_COUNT.min(rack.macros.len()) {
        let m = &rack.macros[i];
        let k = knob_control(("macro", (id.0 as usize) << 8 | i), m.value, false, enabled, KnobTarget::Macro { rack: id, index: i }, cx);
        macros = macros.child(knob_cell(k, &m.name, format!("{:.0}%", m.value * 100.), CELL_W));
    }
    let macros = div()
        .p(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(label("MACROS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(macros);
    let side: AnyElement = if rack.kind == RackKind::Drum {
        let pads = ws.s.project.track(track).map(drum_pads).unwrap_or_default();
        let mut grid = div().w(px(4. * 40. + 3. * SP1)).flex().flex_wrap().gap(px(SP1));
        // 4×4 pads, bottom-left first like hardware.
        for row in (0..4).rev() {
            for col in 0..4 {
                let i = row * 4 + col;
                let pad = pads.get(i).cloned();
                grid = grid.child(
                    div()
                        .id(("pad", (id.0 as usize) << 8 | i))
                        .size(px(40.))
                        .rounded(px(r_md()))
                        .p(px(SP1))
                        .flex()
                        .flex_col()
                        .justify_between()
                        .cursor_pointer()
                        .map(|d| match &pad {
                            Some((name, color, note)) => d
                                .bg(with_alpha(track_color(*color), 0.22))
                                .border_1()
                                .border_color(with_alpha(track_color(*color), 0.5))
                                .hover(|s| s.bg(with_alpha(track_color(*color), 0.32)))
                                .child(label(crate::paint::fit(name, 36., TEXT_XS), TEXT_XS, pal.text))
                                .child(mono(format!("{note}"), TEXT_XS, pal.muted)),
                            None => d.bg(content(0.04)).border_1().border_color(border()),
                        }),
                );
            }
        }
        div()
            .p(px(SP2))
            .flex()
            .flex_col()
            .gap(px(SP2))
            .border_r_1()
            .border_color(border())
            .child(label("PADS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
            .child(grid)
            .into_any_element()
    } else {
        let mut list = div().w(px(144.)).p(px(SP2)).flex().flex_col().gap(px(SP1)).border_r_1().border_color(border()).child(label("CHAINS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD));
        for c in &rack.chains {
            list = list.child(
                div().h(px(24.)).px(px(SP2)).gap(px(SP2)).flex().items_center().rounded(px(r_sm())).bg(content(0.04))
                    .child(div().size(px(8.)).rounded_full().bg(track_color(c.color)))
                    .child(label(c.name.clone(), TEXT_SM, pal.text)),
            );
        }
        if rack.chains.is_empty() {
            list = list.child(label("Drop devices here", TEXT_SM, pal.faint));
        }
        list.into_any_element()
    };
    div().flex().child(side).child(macros).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_points_count_after_the_device_leaves() {
        let chain = [DeviceId(1), DeviceId(2), DeviceId(3)];
        // Dragging the first device to the gap after the second: it ends up second.
        assert_eq!(insertion_index(&chain, DeviceId(1), 2), 1);
        // Dragging the last device to the front.
        assert_eq!(insertion_index(&chain, DeviceId(3), 0), 0);
        // To the end of the chain.
        assert_eq!(insertion_index(&chain, DeviceId(1), 3), 2);
        // A device from elsewhere goes where the gap is.
        assert_eq!(insertion_index(&chain, DeviceId(9), 1), 1);
    }
}
