//! Device View: the selected track's chain as a row of cards. Knobs come
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
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Stateful, StatefulInteractiveElement, Styled, Window, div, point, prelude::FluentBuilder, px,
};

const CELL_W: f32 = 56.;
const KNOB: f32 = 32.;

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(track) = ws.s.selected_track.and_then(|t| ws.s.project.track(t)).cloned() else {
        return empty("Select a track to see its devices.");
    };
    let mut row = div().h_full().flex().gap(px(SP2)).p(px(SP2));
    for d in &track.devices {
        row = row.child(card(ws, track.id, d, cx));
    }
    let tid = track.id;
    row = row.child(
        div()
            .id("add-device")
            .flex_none()
            .w(px(120.))
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
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.open_menu(MenuKind::AddDevice(tid), point(at.x - px(116.), at.y - px(260.)), cx);
            }))
            .child(icon(Icon::Add, 20., pal.muted))
            .child(label("Add device", TEXT_SM, pal.muted)),
    );
    div().id("device-chain").size_full().overflow_x_scroll().child(row).into_any_element()
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

fn card(ws: &mut Workspace, track: TrackId, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
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
            this.s.toggle_device(id);
            cx.notify();
        }));
    let fold = widgets::sized_icon_button(("fold-dev", id.0 as usize), if collapsed { Icon::ChevronRight } else { Icon::ChevronLeft }, false, 20., 12.)
        .tooltip(widgets::tip(if collapsed { "Expand" } else { "Collapse" }, None))
        .on_click(cx.listener(move |this, _, _, cx| {
            // Collapsing is view state stored on the device; it goes through a command too.
            if let Some(dev) = this.s.project.device(id).cloned() {
                let mut chain_dev = dev.clone();
                chain_dev.collapsed = !dev.collapsed;
                let slot = find_slot(&this.s.project, track, id);
                if let Some(slot) = slot {
                    this.s.edit(if chain_dev.collapsed { "Collapse device" } else { "Expand device" }, vec![
                        aitrack_model::Command::RemoveDevice { id },
                        aitrack_model::Command::AddDevice { slot, device: Box::new(chain_dev) },
                    ]);
                }
            }
            cx.notify();
        }));
    if collapsed {
        return card_shell(id, enabled)
            .w(px(40.))
            .items_center()
            .py(px(SP2))
            .gap(px(SP2))
            .child(power)
            .child(fold)
            .child(icon(ic, 14., pal.muted))
            .into_any_element();
    }
    let header = div()
        .flex_none()
        .h(px(TAB_BAR_H + SP1))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(power)
        .child(icon(ic, 14., pal.muted))
        .child(label(d.name.clone(), TEXT_MD, pal.text).font_weight(FontWeight::SEMIBOLD).flex_1())
        .child(badge(kind_label, pal.muted))
        .child(fold)
        .child(
            widgets::sized_icon_button(("remove-dev", id.0 as usize), Icon::X, false, 20., 12.)
                .tooltip(widgets::tip("Remove device", None))
                .on_click(cx.listener(move |this, _, _, cx| {
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
            .child(widgets::text_button(("open-editor", id.0 as usize), "Open plugin editor", Some(Icon::Device), false).on_click(cx.listener(|this, _, _, cx| {
                this.toast("Plugin windows arrive with aitrack-host", cx);
            })))
            .into_any_element(),
        DeviceKind::Stock { plugin_id } => {
            let params = ws.b.plugins.device(plugin_id).map(|i| i.params.clone()).unwrap_or_default();
            let cols = params.len().div_ceil(2).max(3);
            let mut grid = div().w(px(cols as f32 * CELL_W + 2. * SP2)).p(px(SP2)).flex().flex_wrap().gap_y(px(SP3));
            for info in params {
                let value = d.params.get(&info.id).copied().unwrap_or_else(|| info.default_normalized());
                let default = info.default_normalized();
                let target = KnobTarget::Param { device: id, param: info.id, default };
                let bipolar = info.min < 0. && info.max > 0.;
                let k = knob_control(("knob", (id.0 as usize) << 8 | info.id as usize), value, bipolar, enabled, target, cx);
                grid = grid.child(knob_cell(k, info.short, info.format(value), CELL_W));
            }
            grid.into_any_element()
        }
    };
    card_shell(id, enabled).child(header).child(div().flex_1().child(body)).into_any_element()
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
