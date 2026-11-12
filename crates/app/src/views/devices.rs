//! Device View (Live's Detail View): the selected track's chain as a row of
//! device boxes in signal order, each with a title bar (on/off, fold, name)
//! and its editor; folded boxes are a narrow strip with the name running
//! down. Drag a title bar to move a device; the gaps between boxes are the
//! insertion points. The chain scrolls sideways when it is wider than the
//! panel. Knobs come
//! from each device's `ParamInfo` list; racks show their chains, the
//! devices of the chain picked on the left (a chain like the track's, which
//! takes drops too) and, on demand, their 16 macros; drum racks a pad grid.
//! Click a title bar to pick a device, Cmd (macOS) / Ctrl (Windows) + click
//! to add one, Shift+click for a run; Cmd+G groups the pick into a rack and
//! right-click opens the device menu.

use crate::icons::{Icon, icon};
use crate::state::drum_pads;
use crate::theme::*;
use crate::views::menus::device_icon;
use crate::widgets::{self, badge, icon_chip, knob, knob_cell, label, mono};
use crate::workspace::{Drag, KnobTarget, MenuKind, Workspace};
use aitrack_model::racks::{self, Container};
use aitrack_model::{
    Chain, Command, Device, DeviceId, DeviceKind, MACRO_COUNT, PluginFormat, Rack, RackKind,
};
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
    // The scrollable element is the row itself: a wrapper would stretch its
    // child to the viewport width and report nothing to scroll.
    let mut row = div()
        .id("device-chain")
        .size_full()
        .flex()
        .px(px(SP1))
        .py(px(SP2))
        .overflow_x_scroll();
    row = row.children(chain_items(ws, Container::Track(tid), &track.devices, cx));
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
                move_device(this, d.id, Container::Track(tid), usize::MAX);
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
    row.into_any_element()
}

/// Live's chain: devices left to right in signal order, an insertion point
/// between each pair (it lights up while a device is dragged over).
fn chain_items(
    ws: &mut Workspace,
    home: Container,
    devices: &[Device],
    cx: &mut Context<Workspace>,
) -> Vec<AnyElement> {
    let ids: Vec<DeviceId> = devices.iter().map(|d| d.id).collect();
    let mut items = Vec::new();
    for (index, d) in devices.iter().enumerate() {
        items.push(insertion(home, index, &ids, cx));
        items.push(card(ws, home, index, d, cx));
    }
    items.push(insertion(home, ids.len(), &ids, cx));
    items
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
fn insertion(
    home: Container,
    slot: usize,
    chain: &[DeviceId],
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let chain = chain.to_vec();
    let key = match home {
        Container::Track(_) => slot,
        Container::Rack { rack, chain } => (rack.0 as usize) << 16 | chain << 8 | slot,
    };
    div()
        .id(("insert-device", key))
        .flex_none()
        .w(px(SP2 + SP1))
        .h_full()
        .py(px(SP1))
        .flex()
        .justify_center()
        .child(div().w(px(2.)).h_full().rounded(px(1.)))
        .drag_over::<DraggedDevice>(|s, _, _, _| s.bg(accent_alpha(0.55)).rounded(px(r_sm())))
        .on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
            move_device(this, d.id, home, insertion_index(&chain, d.id, slot));
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
fn droppable(
    shell: Stateful<Div>,
    home: Container,
    index: usize,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    shell.drag_over::<DraggedDevice>(|s, _, _, _| s.border_color(accent())).on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
            move_device(this, d.id, home, index);
        cx.notify();
    }))
}

/// Whether `home` is a chain of rack `id` or of a rack inside it: a rack
/// cannot be moved into itself.
fn is_inside(p: &aitrack_model::Project, home: Container, id: DeviceId) -> bool {
    let mut at = home;
    for _ in 0..64 {
        let Container::Rack { rack, .. } = at else {
            return false;
        };
        if rack == id {
            return true;
        }
        let Some(slot) = racks::device_slot(p, rack) else {
            return false;
        };
        at = Container::of(slot);
    }
    true
}

/// Move a device to `index` of chain `home` (one undo step), which may be
/// another chain than its own. `index` is the place it ends up in, counted
/// after it has left its old place. A rack without chains gets one.
pub fn move_device(ws: &mut Workspace, id: DeviceId, home: Container, index: usize) {
    let p = &ws.s.project;
    if p.device(id).is_none() || is_inside(p, home, id) {
        return;
    }
    let mut cmds = Vec::new();
    let chain: Vec<DeviceId> = match (racks::chain(p, home), home) {
        (Some(c), _) => c.iter().map(|d| d.id).collect(),
        (None, Container::Rack { rack, chain: 0 }) if matches!(p.device(rack).map(|d| &d.kind), Some(DeviceKind::Rack(r)) if r.chains.is_empty()) =>
        {
            let name = p.device(id).map(|d| d.name.clone()).unwrap_or_default();
            cmds.push(Command::AddChain {
                rack,
                index: 0,
                chain: Box::new(Chain::new(name, aitrack_model::Color::palette(0))),
            });
            Vec::new()
        }
        _ => return,
    };
    let from = chain.iter().position(|d| *d == id);
    let index = index.min(chain.len() - usize::from(from.is_some()));
    if from == Some(index) {
        return;
    }
    cmds.push(Command::MoveDevice {
        id,
        to: home.slot(index),
    });
    ws.s.edit("Move device", cmds);
}

pub fn empty(text: &'static str) -> AnyElement {
    div().size_full().flex().items_center().justify_center().child(label(text, TEXT_MD, p().faint)).into_any_element()
}

fn card_shell(id: DeviceId, enabled: bool, picked: bool) -> Stateful<Div> {
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
        .border_color(if picked { accent() } else { border() })
        .when(!enabled, |d| d.opacity(0.62))
        .overflow_hidden()
}

/// Left click on a title bar picks the device (Cmd/Ctrl adds, Shift runs);
/// right click opens the device menu on the pick.
fn pickable(el: Stateful<Div>, id: DeviceId, cx: &mut Context<Workspace>) -> Stateful<Div> {
    el.on_mouse_down(
        MouseButton::Left,
        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            if e.click_count == 1 {
                this.s
                    .click_device(id, e.modifiers.secondary(), e.modifiers.shift);
                cx.notify();
            }
        }),
    )
    .on_mouse_down(
        MouseButton::Right,
        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            if !this.s.is_device_selected(id) {
                this.s.click_device(id, false, false);
            }
            this.s.ed.area = crate::editing::Area::Devices;
            this.menu = None;
            this.open_menu(MenuKind::DeviceContext(id), e.position, cx);
        }),
    )
}

fn card(
    ws: &mut Workspace,
    home: Container,
    index: usize,
    d: &Device,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let id = d.id;
    let picked = ws.s.is_device_selected(id);
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
        toggle_fold(this, id);
            cx.notify();
        }));
    if collapsed {
        // Folded (Live): a narrow box with the name running down it.
        let mut name = div().flex().flex_col().items_center().overflow_hidden().flex_1().min_h(px(0.));
        for ch in d.name.chars().take(MAX_FOLDED_CHARS) {
            name = name.child(label(ch.to_string(), TEXT_SM, if enabled { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD).h(px(13.)));
        }
        return pickable(
            droppable(card_shell(id, enabled, picked), home, index, cx),
            id,
            cx,
        )
        .w(px(FOLDED_W))
        .cursor_grab()
        .on_drag(
            DraggedDevice {
                id,
                name: d.name.clone(),
            },
            |d: &DraggedDevice, _, _, cx| cx.new(|_| d.clone()),
        )
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            if e.click_count() >= 2 {
                toggle_fold(this, id);
                cx.notify();
            }
        }))
        .tooltip(widgets::tip_owned(format!(
            "{}: {kind_label}. Double-click to show it",
            d.name
        )))
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
        .map(|h| pickable(h, id, cx))
        .cursor_grab()
        .on_drag(DraggedDevice { id, name: d.name.clone() }, |d: &DraggedDevice, _, _, cx| cx.new(|_| d.clone()))
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            if e.click_count() >= 2 {
                toggle_fold(this, id);
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
        .when(matches!(&d.kind, DeviceKind::Rack(r) if r.kind != RackKind::Drum), |h| {
            let shown = ws.fx.macros.contains(&id);
            h.child(
                widgets::sized_icon_button(("macros-dev", id.0 as usize), Icon::Sliders, shown, 20., 12.)
                    .tooltip(widgets::tip(if shown { "Hide the macros" } else { "Show the macros" }, None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.fx.macros.remove(&id) {
                            this.fx.macros.insert(id);
                        }
                        cx.notify();
                    })),
            )
        })
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
        DeviceKind::Rack(rack) => rack_body(ws, id, rack, enabled, cx),
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
    droppable(card_shell(id, enabled, picked), home, index, cx)
        .child(header)
        .child(div().flex_1().min_h(px(0.)).child(body))
        .into_any_element()
}

/// Fold or unfold a device. Folding is view state stored on the device, so
/// it goes through a command too.
pub fn toggle_fold(ws: &mut Workspace, id: DeviceId) {
    let Some(mut dev) = ws.s.project.device(id).cloned() else { return };
    let Some(slot) = racks::device_slot(&ws.s.project, id) else {
        return;
    };
    dev.collapsed = !dev.collapsed;
    ws.s.edit(if dev.collapsed { "Fold device" } else { "Show device" }, vec![
            Command::RemoveDevice { id },
            Command::AddDevice {
                slot,
                device: Box::new(dev),
            },
    ]);
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
                KnobTarget::Transpose { clip } => {
                    this.s.set_clip_warp("transpose-reset", clip, |w| w.transpose = 0.);
                }
                KnobTarget::Detune { clip } => {
                    this.s.set_clip_warp("detune-reset", clip, |w| w.detune = 0.);
                }
            }
            this.s.seal();
            cx.notify();
            return;
        }
        this.begin_drag(Drag::Knob { target, last_y: f32::from(e.position.y), value }, cx);
    }))
}

fn rack_body(
    ws: &mut Workspace,
    id: DeviceId,
    rack: &Rack,
    enabled: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let mut macros = div().w(px(8. * CELL_W)).flex().flex_wrap().gap_y(px(SP3));
    for i in 0..MACRO_COUNT.min(rack.macros.len()) {
        let m = &rack.macros[i];
        let k = knob_control(("macro", (id.0 as usize) << 8 | i), m.value, false, enabled, KnobTarget::Macro { rack: id, index: i }, cx);
        macros = macros.child(knob_cell(k, &m.name, format!("{:.0}%", m.value * 100.), CELL_W));
    }
    let macros = div()
        .flex_none()
        .p(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(label("MACROS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(macros);
    if rack.kind != RackKind::Drum {
        let show_macros = ws.fx.macros.contains(&id);
        return div()
            .h_full()
            .flex()
            .child(chains(ws, id, rack, cx))
            .when(show_macros, |d| {
                d.child(div().border_l_1().border_color(border()).child(macros))
            })
            .into_any_element();
    }
    let pads =
        ws.s.project
            .device_track(id)
            .and_then(|t| ws.s.project.track(t))
            .map(drum_pads)
            .unwrap_or_default();
    let mut grid = div()
        .w(px(4. * 40. + 3. * SP1))
        .flex()
        .flex_wrap()
        .gap(px(SP1));
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
                            .child(label(
                                crate::paint::fit(name, 36., TEXT_XS),
                                TEXT_XS,
                                pal.text,
                            ))
                            .child(mono(format!("{note}"), TEXT_XS, pal.muted)),
                        None => d.bg(content(0.04)).border_1().border_color(border()),
                    }),
            );
        }
    }
    let side = div()
        .p(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .border_r_1()
        .border_color(border())
        .child(label("PADS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(grid);
    div().flex().child(side).child(macros).into_any_element()
}

/// An instrument or audio rack (Live): its chains on the left, and the
/// devices of the picked one as a chain of their own, which takes drops
/// like the track's. Dropping on a chain's name sends the device to its end.
fn chains(
    ws: &mut Workspace,
    id: DeviceId,
    rack: &Rack,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let sel = ws
        .fx
        .selected
        .get(&id)
        .copied()
        .unwrap_or(0)
        .min(rack.chains.len().saturating_sub(1));
    let mut list = div()
        .flex_none()
        .w(px(144.))
        .h_full()
        .p(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP1))
        .border_r_1()
        .border_color(border())
        .child(label("CHAINS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD));
    for (i, c) in rack.chains.iter().enumerate() {
        let home = Container::Rack { rack: id, chain: i };
        list = list.child(
            div()
                .id(("rack-chain", (id.0 as usize) << 8 | i))
                .h(px(24.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .border_1()
                .border_color(if i == sel {
                    accent()
                } else {
                    gpui::transparent_black()
                })
                .bg(if i == sel {
                    content(0.08)
                } else {
                    content(0.04)
                })
                .cursor_pointer()
                .hover(|s| s.bg(content(0.10)))
                .drag_over::<DraggedDevice>(|s, _, _, _| s.border_color(accent()))
                .on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
                    move_device(this, d.id, home, usize::MAX);
                    this.fx.selected.insert(id, i);
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fx.selected.insert(id, i);
                    cx.notify();
                }))
                .child(div().size(px(8.)).rounded_full().bg(track_color(c.color)))
                .child(label(
                    crate::paint::fit(&c.name, 100., TEXT_SM),
                    TEXT_SM,
                    pal.text,
                )),
        );
    }
    let home = Container::Rack {
        rack: id,
        chain: sel,
    };
    let devices = rack
        .chains
        .get(sel)
        .map(|c| c.devices.clone())
        .unwrap_or_default();
    let empty = devices.is_empty();
    let drop = div()
        .id(("rack-drop", id.0 as usize))
        .flex_none()
        .w(px(if empty { 176. } else { 96. }))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .px(px(SP2))
        .rounded(px(r_md()))
        .border_1()
        .border_dashed()
        .border_color(border_strong())
        .drag_over::<DraggedDevice>(|s, _, _, _| s.border_color(accent()).bg(accent_alpha(0.08)))
        .on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
            move_device(this, d.id, home, usize::MAX);
            cx.notify();
        }))
        .child(label(
            if empty {
                "Drop devices here"
            } else {
                "Drop here"
            },
            TEXT_SM,
            pal.faint,
        ));
    div()
        .h_full()
        .flex()
        .child(list)
        .child(
            div()
                .h_full()
                .flex()
                .px(px(SP1))
                .py(px(SP1))
                .children(chain_items(ws, home, &devices, cx))
                .child(drop),
        )
        .into_any_element()
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
