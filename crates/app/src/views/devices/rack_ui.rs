//! The panels of a rack (Live's Instrument / Audio / Drum Rack): a chain's
//! strip (volume, pan, mute, solo, and a Drum Rack pad's choke, notes and
//! sends), the Drum Rack's 4×4 pad grid (a click auditions the pad through
//! the track, the headphones play the pad's sample file as it is), its
//! return buses, the zones panel (key, velocity and chain-select ranges with
//! their fades) and the macros with their Map mode, range handles, Rand and
//! variations.
//!
//! Every change is a command with undo: a drag of a knob or a handle merges
//! into one step through `AppState`'s gliding setters (see `editing/racks.rs`).

use super::{DraggedDevice, chain_items, knob_control, knob_control_sized, move_device};
use crate::icons::{Icon, icon};
use crate::state::{db_to_fader, fader_to_db};
use crate::theme::*;
use crate::widgets::{self, knob_cell, label, mono};
use crate::workspace::{ChainKnob, Drag, KnobTarget, Workspace};
use aitrack_model::command::ChainProp;
use aitrack_model::racks::Container;
use aitrack_model::{Chain, Device, DeviceId, DeviceKind, MAX_RETURNS, Rack, RackKind, SEND_MAX_DB, SEND_OFF_DB};
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled, canvas, div, point, prelude::FluentBuilder, px,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Widths of the rack's side panels, to bound the devices of the picked chain.
const LEFT_DRUM_W: f32 = 248.;
const LEFT_LIST_W: f32 = 184.;
const ZONES_W: f32 = 440.;
const MACROS_W: f32 = 504.;
/// Window width not available to the rack (panel margins, borders, scrollbar).
const RACK_MARGIN: f32 = 96.;
const MIN_DEVICES_W: f32 = 320.;

/// Side of a pad of the Drum Rack grid.
const PAD: f32 = 52.;
/// Width of a macro cell.
const CELL: f32 = 56.;
/// Width in px of a mapping's range bar.
const MAP_W: f32 = 160.;
/// Width in px of a zone bar: two px per key or velocity step.
const ZONE_W: f32 = 256.;
/// Handles of a range bar.
const HANDLE: f32 = 8.;

/// What the rack panels remember between frames (view state; nothing here is
/// saved with the project).
#[derive(Default)]
pub struct RackUi {
    /// First note of the pad page shown, per Drum Rack.
    pub bank: HashMap<DeviceId, u8>,
    /// Map mode: the rack and the macro the next parameter touched is mapped to.
    pub map: Option<(DeviceId, usize)>,
    /// The macro whose mappings are listed.
    pub shown_macro: HashMap<DeviceId, usize>,
    /// Racks whose zones panel is open, and its tab.
    pub zones: BTreeSet<DeviceId>,
    pub zone_tab: HashMap<DeviceId, ZoneTab>,
    /// Racks showing 16 macros (8 otherwise, as Live).
    pub sixteen: BTreeSet<DeviceId>,
    /// Macros kept out of Rand and of recalling a variation.
    pub locked: BTreeSet<(DeviceId, usize)>,
    /// The Drum Rack return whose devices are shown instead of the picked pad's.
    pub ret_sel: HashMap<DeviceId, usize>,
    /// Where the next device the Add menu adds goes (a chain of a rack, a return); `None` = the track.
    pub add_into: Option<Container>,
    /// The Drum Rack pad whose sample file is playing in the preview, and
    /// which preview it is (a newer one outlives an older one's timer).
    pub previewing: Option<(DeviceId, u8, u64)>,
    previews: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZoneTab {
    #[default]
    Key,
    Vel,
    Chain,
}

impl ZoneTab {
    const ALL: [(ZoneTab, &'static str); 3] = [(ZoneTab::Key, "Key"), (ZoneTab::Vel, "Vel"), (ZoneTab::Chain, "Chain")];

    /// The zone, its fades and the property names of a chain for this tab.
    fn read(self, c: &Chain) -> ((u8, u8), (u8, u8)) {
        match self {
            ZoneTab::Key => (c.key_range, c.key_fade),
            ZoneTab::Vel => (c.velocity_range, c.vel_fade),
            ZoneTab::Chain => (c.selector_range, c.sel_fade),
        }
    }

    fn range_prop(self, r: (u8, u8)) -> ChainProp {
        match self {
            ZoneTab::Key => ChainProp::KeyRange(r),
            ZoneTab::Vel => ChainProp::VelRange(r),
            ZoneTab::Chain => ChainProp::SelRange(r),
        }
    }

    fn fade_prop(self, f: (u8, u8)) -> ChainProp {
        match self {
            ZoneTab::Key => ChainProp::KeyFade(f),
            ZoneTab::Vel => ChainProp::VelFade(f),
            ZoneTab::Chain => ChainProp::SelFade(f),
        }
    }
}

/// Which handle of a bar is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Low,
    High,
    FadeLow,
    FadeHigh,
}

/// A drag of a handle in the rack panels: the values are the ones at the press,
/// so every frame is worked out from them and the drag never drifts.
#[derive(Clone, Debug)]
pub enum RackDrag {
    /// A macro mapping's min or max (`min > max` inverts it).
    Mapping { rack: DeviceId, macro_index: usize, index: usize, handle: Handle, start_x: f32, min: f32, max: f32 },
    /// A zone of a chain (`lo, hi` and the two fades).
    Zone { rack: DeviceId, chain: usize, tab: ZoneTab, handle: Handle, start_x: f32, lo: u8, hi: u8, fade: (u8, u8) },
    /// The chain selector of a rack.
    Selector { rack: DeviceId, start_x: f32, start: u8 },
}

// ───────────────────────────── Names and values ─────────────────────────────

/// A note as Live writes it: middle C is C3, so 36 is C1.
pub fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[usize::from(n % 12)], i32::from(n / 12) - 2)
}

/// First note of the page of 16 pads a Drum Rack opens on: its lowest pad's
/// (Live opens on C1, note 36).
pub fn default_bank(rack: &Rack) -> u8 {
    rack.chains.iter().filter_map(|c| c.note).min().map_or(36, |n| n.min(112))
}

/// A send knob position (0..=1) as dB, and back.
pub fn send_db(v: f32) -> f32 {
    SEND_OFF_DB + v.clamp(0., 1.) * (SEND_MAX_DB - SEND_OFF_DB)
}

fn send_pos(db: f32) -> f32 {
    ((db - SEND_OFF_DB) / (SEND_MAX_DB - SEND_OFF_DB)).clamp(0., 1.)
}

fn db_text(db: f32) -> String {
    if db <= -59.5 { "−∞".to_owned() } else { format!("{db:+.1}") }
}

/// Whether `device` sits anywhere inside `rack` (any chain, nested racks too).
pub fn rack_contains(p: &aitrack_model::Project, rack: DeviceId, device: DeviceId) -> bool {
    fn walk(chains: &[Chain], device: DeviceId) -> bool {
        chains.iter().any(|c| c.devices.iter().any(|d| d.id == device || matches!(&d.kind, DeviceKind::Rack(r) if walk(&r.chains, device))))
    }
    match p.device(rack).map(|d| &d.kind) {
        Some(DeviceKind::Rack(r)) => walk(&r.chains, device),
        _ => false,
    }
}

// ───────────────────────────── Knobs and drags ─────────────────────────────

/// A double-click on a chain knob: its default.
pub fn reset_knob(ws: &mut Workspace, target: KnobTarget) {
    match target {
        KnobTarget::Chain { rack, index, knob } => {
            let prop = match knob {
                ChainKnob::Volume => ChainProp::VolumeDb(0.),
                ChainKnob::Pan => ChainProp::Pan(0.),
                ChainKnob::Send(i) => ChainProp::Send(i, SEND_OFF_DB),
            };
            ws.s.set_chain_prop_gliding(rack, index, prop);
        }
        KnobTarget::Return { rack, index } => {
            ws.s.set_return_prop_gliding(rack, index, ChainProp::VolumeDb(0.));
        }
        _ => {}
    }
}

/// A drag of a chain or return knob to position `v` (0..=1).
pub fn knob_drag(ws: &mut Workspace, target: KnobTarget, v: f32) {
    match target {
        KnobTarget::Chain { rack, index, knob } => {
            let prop = match knob {
                ChainKnob::Volume => ChainProp::VolumeDb(fader_to_db(v).max(-70.)),
                ChainKnob::Pan => ChainProp::Pan(v * 2. - 1.),
                ChainKnob::Send(i) => ChainProp::Send(i, send_db(v)),
            };
            ws.s.set_chain_prop_gliding(rack, index, prop);
        }
        KnobTarget::Return { rack, index } => {
            ws.s.set_return_prop_gliding(rack, index, ChainProp::VolumeDb(fader_to_db(v).max(-70.)));
        }
        _ => {}
    }
}

/// A mapping's range with its `handle` moved by `d` (a fraction of the bar).
/// A handle may cross the other: that inverts the mapping.
pub fn mapping_after_drag(handle: Handle, min: f32, max: f32, d: f32) -> (f32, f32) {
    match handle {
        Handle::Low => ((min + d).clamp(0., 1.), max),
        _ => (min, (max + d).clamp(0., 1.)),
    }
}

/// A zone `lo..=hi` with fades `fade` after `handle` moved by `d` steps. The
/// ends stay in order (velocity never below 1), and the two fades together
/// never outgrow the zone.
pub fn zone_after_drag(tab: ZoneTab, handle: Handle, lo: u8, hi: u8, fade: (u8, u8), d: i32) -> ((u8, u8), (u8, u8)) {
    let floor = if tab == ZoneTab::Vel { 1 } else { 0 };
    let clamp = |v: i32, a: i32, b: i32| v.clamp(a, b.max(a)) as u8;
    let (lo, hi) = match handle {
        Handle::Low => (clamp(i32::from(lo) + d, floor, i32::from(hi)), hi),
        Handle::High => (lo, clamp(i32::from(hi) + d, i32::from(lo), 127)),
        _ => (lo, hi),
    };
    let room = i32::from(hi) - i32::from(lo);
    let fade = match handle {
        Handle::FadeLow => (clamp(i32::from(fade.0) + d, 0, room - i32::from(fade.1)), fade.1),
        Handle::FadeHigh => (fade.0, clamp(i32::from(fade.1) - d, 0, room - i32::from(fade.0))),
        // Moving an end can leave the fades too wide: the first keeps its width, the second gives way.
        _ => {
            let first = i32::from(fade.0).min(room);
            (first as u8, i32::from(fade.1).min(room - first) as u8)
        }
    };
    ((lo, hi), fade)
}

/// The chain selector after a drag of `d` steps from `start`, kept in 0..=127.
pub fn selector_after_drag(start: u8, d: i32) -> u8 {
    (i32::from(start) + d).clamp(0, 127) as u8
}

/// A handle moved to the pointer at window x `x` (`fine`: Shift held).
pub fn drag_move(ws: &mut Workspace, drag: &RackDrag, x: f32, fine: f32) {
    match *drag {
        RackDrag::Mapping { rack, macro_index, index, handle, start_x, min, max } => {
            let (min, max) = mapping_after_drag(handle, min, max, (x - start_x) / MAP_W * fine);
            ws.s.set_mapping_range(rack, macro_index, index, min, max);
        }
        RackDrag::Selector { rack, start_x, start } => {
            let d = ((x - start_x) / (ZONE_W / 128.) * fine).round() as i32;
            ws.s.set_rack_selector(rack, selector_after_drag(start, d));
        }
        RackDrag::Zone { rack, chain, tab, handle, start_x, lo, hi, fade } => {
            let d = ((x - start_x) / (ZONE_W / 128.) * fine).round() as i32;
            let (range, fade) = zone_after_drag(tab, handle, lo, hi, fade, d);
            match handle {
                Handle::Low | Handle::High => ws.s.set_chain_prop_gliding(rack, chain, tab.range_prop(range)),
                _ => ws.s.set_chain_prop_gliding(rack, chain, tab.fade_prop(fade)),
            };
        }
    }
}

// ───────────────────────────── Small controls ─────────────────────────────

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

/// A mute / solo chip, 16 px (pads and chain rows).
fn mini_chip(id: impl Into<ElementId>, text: &'static str, on: bool, color: gpui::Hsla) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { color } else { content(0.08) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.16))))
        .child(label(text, TEXT_XS, if on { theme_on(color) } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
}

fn theme_on(color: gpui::Hsla) -> gpui::Hsla {
    crate::theme::mix(color, with_alpha(p().shadow, 1.), 0.78)
}

/// `− value +`, to step a number.
fn stepper(id: usize, name: &'static str, value: String, cx: &mut Context<Workspace>, step: impl Fn(&mut Workspace, i32) + Clone + 'static) -> Div {
    let pal = p();
    let button = |tag: &'static str, glyph: &'static str, dir: i32, cx: &mut Context<Workspace>| {
        let step = step.clone();
        div()
            .id((tag, id))
            .flex_none()
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(r_sm()))
            .cursor_pointer()
            .bg(content(0.06))
            .hover(|s| s.bg(content(0.14)))
            .on_click(cx.listener(move |this, _, _, cx| {
                step(this, dir);
                cx.notify();
            }))
            .child(label(glyph, TEXT_MD, pal.text))
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(label(name, TEXT_XS, pal.muted))
        .child(div().flex().items_center().gap(px(2.)).child(button("step-down", "−", -1, cx)).child(div().w(px(36.)).flex().justify_center().child(mono(value, TEXT_SM, pal.text))).child(button("step-up", "+", 1, cx)))
}

// ───────────────────────────── The rack ─────────────────────────────

/// The body of a rack device: chains (or pads), the picked chain's strip and
/// devices, the zones panel when open, and the macros.
pub fn rack_body(ws: &mut Workspace, id: DeviceId, rack: &Rack, enabled: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let drum = rack.kind == RackKind::Drum;
    let sel = ws.fx.selected.get(&id).copied().unwrap_or(0).min(rack.chains.len().saturating_sub(1));
    let ret = ws.rack_ui.ret_sel.get(&id).copied().filter(|i| drum && *i < rack.returns.len());
    let show_macros = drum || ws.fx.macros.contains(&id);
    let show_zones = ws.rack_ui.zones.contains(&id);
    let left = if drum { pads_column(ws, id, rack, sel, ret, cx) } else { chain_list(ws, id, rack, sel, cx) };
    // The devices of the picked chain scroll sideways inside what the side
    // panels leave, so the zones and the macros stay in view instead of
    // running off to the right of wide device cards.
    let taken = if drum { LEFT_DRUM_W } else { LEFT_LIST_W } + if show_zones { ZONES_W } else { 0. } + if show_macros { MACROS_W } else { 0. };
    let room = (ws.window_w - taken - RACK_MARGIN).max(MIN_DEVICES_W);
    div()
        .h_full()
        .flex()
        .child(left)
        .child(chain_column(ws, id, rack, sel, ret, room, cx))
        .when(show_zones, |d| d.child(zones_panel(ws, id, rack, cx)))
        .when(show_macros, |d| d.child(macros_panel(ws, id, rack, enabled, cx)))
        .into_any_element()
}

/// The picked chain: its strip on top, its devices as a chain of their own
/// below (they take drops like the track's).
fn chain_column(ws: &mut Workspace, id: DeviceId, rack: &Rack, sel: usize, ret: Option<usize>, room: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (home, devices, usable) = match ret {
        Some(i) => (Container::Return { rack: id, index: i }, rack.returns[i].devices.clone(), true),
        None => (Container::Rack { rack: id, chain: sel }, rack.chains.get(sel).map(|c| c.devices.clone()).unwrap_or_default(), !rack.chains.is_empty() || rack.kind != RackKind::Drum),
    };
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
        .child(label(if empty { "Drop devices here" } else { "Drop here" }, TEXT_SM, pal.faint));
    let strip = match ret {
        Some(i) => return_strip(ws, id, rack, i, cx),
        None => chain_strip(ws, id, rack, sel, cx),
    };
    div()
        .flex()
        .flex_col()
        .h_full()
        .min_w(px(0.))
        .max_w(px(room))
        .overflow_hidden()
        .child(strip)
        .child(
            div()
                .id(("rack-devices", id.0 as usize))
                .flex_1()
                .min_h(px(0.))
                .flex()
                .px(px(SP1))
                .py(px(SP1))
                .overflow_x_scroll()
                .when(usable, |d| d.children(chain_items(ws, home, &devices, cx)).child(drop)),
        )
        .into_any_element()
}

/// The strip of the return picked: its name and level, and a way to add a device to it.
fn return_strip(ws: &mut Workspace, id: DeviceId, rack: &Rack, ri: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let _ = ws;
    let pal = p();
    let r = &rack.returns[ri];
    let key = (id.0 as usize) << 8 | ri;
    div()
        .flex_none()
        .px(px(SP2))
        .py(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(div().size(px(8.)).rounded_full().bg(track_color(r.color)))
        .child(label(r.name.clone(), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
        .child(add_device_pill(("ret-add", key), id, Container::Return { rack: id, index: ri }, cx))
        .child(label("Effects on what the pads send here", TEXT_XS, pal.faint))
        .into_any_element()
}

/// "+ Device": opens the Add menu, whose pick goes to the end of `into`.
fn add_device_pill(tag: (&'static str, usize), rack: DeviceId, into: Container, cx: &mut Context<Workspace>) -> Stateful<Div> {
    pill(tag, "+ Device", false).on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
        let Some(track) = this.s.project.device_track(rack) else { return };
        let at = e.position();
        this.open_menu(crate::workspace::MenuKind::AddDevice(track), point(at.x - px(124.), at.y - px(260.)), cx);
        // After `open_menu`, which clears it for the menus opened any other way.
        this.rack_ui.add_into = Some(into);
    }))
}

/// The picked chain's strip: name, volume, pan, mute, solo and delete; for a
/// Drum Rack pad also choke group, the note that hits it and the note its
/// devices get, and its sends.
fn chain_strip(ws: &mut Workspace, id: DeviceId, rack: &Rack, ci: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(c) = rack.chains.get(ci) else {
        return div().h(px(SP6)).px(px(SP2)).flex().items_center().child(label("No chains yet", TEXT_SM, pal.faint)).into_any_element();
    };
    let drum = rack.kind == RackKind::Drum;
    let key = (id.0 as usize) << 8 | ci;
    let knob = |tag: &'static str, value: f32, bipolar: bool, target: KnobTarget, name: &str, text: String, cx: &mut Context<Workspace>| {
        knob_cell(knob_control_sized((tag, key), value, bipolar, !c.mute, 28., target, cx), name, text, 48.)
    };
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_end()
        .gap(px(SP2))
        // (A MIDI Effect Rack's chains carry notes, not audio: no level to set.)
        .when(rack.kind != RackKind::Midi, |r| {
            r.child(knob("cvol", db_to_fader(c.volume_db), false, KnobTarget::Chain { rack: id, index: ci, knob: ChainKnob::Volume }, "Vol", db_text(c.volume_db), cx))
                .child(knob("cpan", (c.pan + 1.) / 2., true, KnobTarget::Chain { rack: id, index: ci, knob: ChainKnob::Pan }, "Pan", pan_text(c.pan), cx))
        })
        .child(
            div()
                .flex()
                .gap(px(SP1))
                .pb(px(SP2))
                .child(widgets::chip(("cmute", key), "M", c.mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                    this.s.toggle_chain_mute(id, ci);
                    cx.notify();
                })))
                .child(widgets::chip(("csolo", key), "S", c.solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                    this.s.toggle_chain_solo(id, ci);
                    cx.notify();
                }))),
        );
    if drum {
        let (choke, note, play) = (c.choke, c.note, c.play_note);
        row = row
            .child(stepper(key, "Choke", if choke == 0 { "off".to_owned() } else { choke.to_string() }, cx, move |this, dir| {
                this.s.set_chain_prop(id, ci, ChainProp::Choke((i32::from(choke) + dir).clamp(0, 16) as u8));
            }))
            .child(stepper(key + 1_000_000, "Receive", note.map_or("none".to_owned(), note_name), cx, move |this, dir| {
                let n = (i32::from(note.unwrap_or(36)) + dir).clamp(0, 127) as u8;
                this.s.set_chain_prop(id, ci, ChainProp::ReceiveNote(Some(n)));
            }))
            .child(stepper(key + 2_000_000, "Play", note_name(play.unwrap_or(60)), cx, move |this, dir| {
                let n = (i32::from(play.unwrap_or(60)) + dir).clamp(0, 127) as u8;
                this.s.set_chain_prop(id, ci, ChainProp::PlayNote(Some(n)));
            }));
        for (ri, ret) in rack.returns.iter().enumerate() {
            let db = c.sends.get(ri).copied().unwrap_or(SEND_OFF_DB);
            let name = format!("Send {}", (b'A' + ri as u8) as char);
            let _ = ret;
            row = row.child(knob("csend", send_pos(db), false, KnobTarget::Chain { rack: id, index: ci, knob: ChainKnob::Send(ri) }, &name, db_text(db), cx).id(("csend-cell", key << 3 | ri)));
        }
    }
    let zones_on = ws.rack_ui.zones.contains(&id);
    div()
        .flex_none()
        .px(px(SP2))
        .py(px(SP2))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().size(px(8.)).rounded_full().bg(track_color(c.color)))
                .child(label(crate::paint::fit(&c.name, 200., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
                .when(drum, |d| d.child(mono(c.note.map_or(String::new(), note_name), TEXT_SM, pal.muted)))
                .child(add_device_pill(("chain-add", key), id, Container::Rack { rack: id, chain: ci }, cx))
                .child(pill(("zones-toggle", id.0 as usize), "Zones", zones_on).on_click(cx.listener(move |this, _, _, cx| {
                    if !this.rack_ui.zones.remove(&id) {
                        this.rack_ui.zones.insert(id);
                    }
                    cx.notify();
                })))
                .child(
                    widgets::sized_icon_button(("cdel", key), Icon::Trash, false, 20., 12.)
                        .tooltip(widgets::tip("Delete this chain", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.s.remove_chain(id, ci);
                            cx.notify();
                        })),
                ),
        )
        .child(row)
        .into_any_element()
}

fn pan_text(pan: f32) -> String {
    match (pan * 50.).round() as i32 {
        0 => "C".to_owned(),
        v if v < 0 => format!("{}L", -v),
        v => format!("{v}R"),
    }
}

// ───────────────────────────── Chain list (Instrument / Audio racks) ─────────────────────────────

fn chain_list(ws: &mut Workspace, id: DeviceId, rack: &Rack, sel: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let _ = ws;
    let pal = p();
    let mut list = div()
        .flex_none()
        .w(px(176.))
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
        let key = (id.0 as usize) << 8 | i;
        list = list.child(
            div()
                .id(("rack-chain", key))
                .h(px(24.))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .border_1()
                .border_color(if i == sel { accent() } else { gpui::transparent_black() })
                .bg(if i == sel { content(0.08) } else { content(0.04) })
                .opacity(if c.mute { 0.5 } else { 1. })
                .cursor_pointer()
                .hover(|s| s.bg(content(0.10)))
                .drag_over::<DraggedDevice>(|s, _, _, _| s.border_color(accent()))
                .on_drop(cx.listener(move |this, d: &DraggedDevice, _, cx| {
                    move_device(this, d.id, home, usize::MAX);
                    this.fx.selected.insert(id, i);
                    this.rack_ui.ret_sel.remove(&id);
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fx.selected.insert(id, i);
                    this.rack_ui.ret_sel.remove(&id);
                    cx.notify();
                }))
                .child(div().size(px(8.)).flex_none().rounded_full().bg(track_color(c.color)))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label(crate::paint::fit(&c.name, 76., TEXT_SM), TEXT_SM, pal.text)))
                .child(mini_chip(("lmute", key), "M", c.mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.s.toggle_chain_mute(id, i);
                    cx.notify();
                })))
                .child(mini_chip(("lsolo", key), "S", c.solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.s.toggle_chain_solo(id, i);
                    cx.notify();
                }))),
        );
    }
    list.into_any_element()
}

// ───────────────────────────── Drum Rack pads and returns ─────────────────────────────

fn pads_column(ws: &mut Workspace, id: DeviceId, rack: &Rack, sel: usize, ret: Option<usize>, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let bank = ws.rack_ui.bank.get(&id).copied().unwrap_or_else(|| default_bank(rack)).min(112);
    let track = ws.s.project.device_track(id);
    let mut grid = div().w(px(4. * PAD + 3. * SP1)).flex().flex_wrap().gap(px(SP1));
    // 4×4, the lowest note bottom-left like the hardware.
    for row in (0..4u8).rev() {
        for col in 0..4u8 {
            let note = bank + row * 4 + col;
            let ci = rack.chains.iter().position(|c| c.note == Some(note));
            let chain = ci.and_then(|i| rack.chains.get(i));
            let key = (id.0 as usize) << 8 | usize::from(note);
            let lit = track.is_some_and(|t| ws.audition.pressed(t, note));
            let sample = chain.and_then(pad_sample);
            let mut pad = div().id(("pad", key)).size(px(PAD)).rounded(px(r_md())).p(px(SP1)).flex().flex_col().justify_between().cursor_pointer();
            pad = match chain {
                Some(c) => {
                    let col = track_color(c.color);
                    let picked = ci == Some(sel) && ret.is_none();
                    pad.bg(with_alpha(col, if lit { 0.55 } else if c.mute { 0.08 } else { 0.22 }))
                        .border_1()
                        .border_color(if picked { accent() } else if c.solo { pal.solo } else { with_alpha(col, 0.5) })
                        .hover(|s| s.bg(with_alpha(col, 0.34)))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.fx.selected.insert(id, ci.unwrap_or(0));
                            this.rack_ui.ret_sel.remove(&id);
                            // A click plays the pad for as long as it is held.
                            if let Some(t) = this.s.project.device_track(id)
                                && this.press_audition(crate::audition::MOUSE, t, note, cx)
                            {
                                this.drag = Some(Drag::Pad);
                            }
                            cx.notify();
                        }))
                        // The name, and the headphones that play the pad's sample file.
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(label(crate::paint::fit(&c.name, PAD - 8. - if sample.is_some() { 16. } else { 0. }, TEXT_XS), TEXT_XS, if c.mute { pal.muted } else { pal.text }))
                                .when_some(sample.clone(), |row, path| {
                                    let on = ws.rack_ui.previewing.is_some_and(|(d, n, _)| d == id && n == note);
                                    row.child(
                                        widgets::sized_icon_button(("ppreview", key), Icon::Headphones, on, 16., 10.)
                                            .tooltip(widgets::tip(if on { "Stop the sample" } else { "Play the sample file" }, None))
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                            .on_click(cx.listener(move |this, _, _, cx| preview_pad(this, id, note, &path, cx))),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(2.))
                                        .child(mini_chip(("pmute", key), "M", c.mute, pal.mute).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(i) = ci {
                                                this.s.toggle_chain_mute(id, i);
                                            }
                                            cx.notify();
                                        })))
                                        .child(mini_chip(("psolo", key), "S", c.solo, pal.solo).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(i) = ci {
                                                this.s.toggle_chain_solo(id, i);
                                            }
                                            cx.notify();
                                        }))),
                                )
                        )
                        .tooltip(widgets::tip_owned(match &sample {
                            Some(path) => format!("{}\n{}\nClick plays the pad · the headphones play the sample", c.name, path.file_name().map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().into_owned())),
                            None => format!("{}\nClick plays the pad", c.name),
                        }))
                }
                None => pad.bg(content(0.04)).border_1().border_color(border()).child(div()).child(mono(note_name(note), TEXT_XS, pal.faint)),
            };
            grid = grid.child(pad);
        }
    }
    let page = |tag: &'static str, glyph: Icon, to: Option<u8>, cx: &mut Context<Workspace>| {
        let b = widgets::sized_icon_button((tag, id.0 as usize), glyph, false, 20., 12.);
        match to {
            Some(n) => b.on_click(cx.listener(move |this, _, _, cx| {
                this.rack_ui.bank.insert(id, n);
                cx.notify();
            })),
            None => b.opacity(0.35),
        }
    };
    let pager = div()
        .flex()
        .items_center()
        .justify_between()
        .child(page("bank-down", Icon::ChevronLeft, bank.checked_sub(16), cx))
        .child(mono(format!("{} – {}", note_name(bank), note_name(bank + 15)), TEXT_SM, pal.muted))
        .child(page("bank-up", Icon::ChevronRight, (bank < 112).then_some(bank + 16), cx));
    div()
        .flex_none()
        .h_full()
        .p(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .border_r_1()
        .border_color(border())
        .overflow_hidden()
        .child(label("PADS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(grid)
        .child(pager)
        .child(returns_section(ws, id, rack, ret, cx))
        .into_any_element()
}

/// The audio file a Drum Rack pad plays: the sample of the first sampler in
/// its chain, nested racks included.
pub fn pad_sample(chain: &Chain) -> Option<PathBuf> {
    fn find(devices: &[Device]) -> Option<String> {
        devices.iter().find_map(|d| match &d.kind {
            DeviceKind::Rack(r) => r.chains.iter().find_map(|c| find(&c.devices)),
            _ => {
                let (path, pads) = crate::views::device_ui::kit::state_paths(d.state.as_deref());
                path.or_else(|| pads.into_iter().flatten().next())
            }
        })
    }
    find(&chain.devices).filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// Play a pad's sample file in the preview (as the browser does), or stop it
/// when that pad is already playing. The button lights until the file ends.
fn preview_pad(ws: &mut Workspace, id: DeviceId, note: u8, path: &Path, cx: &mut Context<Workspace>) {
    if ws.rack_ui.previewing.take().is_some_and(|(d, n, _)| d == id && n == note) {
        ws.b.engine.preview(None);
        cx.notify();
        return;
    }
    ws.b.engine.preview(Some(path));
    ws.rack_ui.previews += 1;
    let run = ws.rack_ui.previews;
    ws.rack_ui.previewing = Some((id, note, run));
    let path = path.to_owned();
    cx.spawn(async move |this, cx| {
        let seconds = cx.background_executor().spawn(async move { wav_seconds(&path) }).await.unwrap_or(2.);
        cx.background_executor().timer(Duration::from_secs_f64(seconds.clamp(0.1, 60.) + 0.05)).await;
        let _ = this.update(cx, |this, cx| {
            if this.rack_ui.previewing.is_some_and(|(_, _, r)| r == run) {
                this.rack_ui.previewing = None;
                cx.notify();
            }
        });
    })
    .detach();
    cx.notify();
}

/// How long a WAV file plays, from its header.
fn wav_seconds(path: &Path) -> Option<f64> {
    let r = hound::WavReader::open(path).ok()?;
    let rate = r.spec().sample_rate;
    (rate > 0).then(|| f64::from(r.duration()) / f64::from(rate))
}

/// The Drum Rack's return buses: effect chains the pads send to.
fn returns_section(ws: &mut Workspace, id: DeviceId, rack: &Rack, ret: Option<usize>, cx: &mut Context<Workspace>) -> AnyElement {
    let _ = ws;
    let pal = p();
    let mut col = div().flex().flex_col().gap(px(SP1)).child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(label("RETURNS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
            .when(rack.returns.len() < MAX_RETURNS, |d| {
                d.child(pill(("add-return", id.0 as usize), "+ Return", false).on_click(cx.listener(move |this, _, _, cx| {
                    this.s.add_return(id);
                    cx.notify();
                })))
            }),
    );
    for (i, r) in rack.returns.iter().enumerate() {
        let key = (id.0 as usize) << 8 | i;
        let devices = r.devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(" › ");
        col = col.child(
            div()
                .id(("rrow", key))
                .h(px(28.))
                .px(px(SP1))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .border_1()
                .border_color(if ret == Some(i) { accent() } else { gpui::transparent_black() })
                .bg(if ret == Some(i) { content(0.08) } else { content(0.04) })
                .cursor_pointer()
                .opacity(if r.mute { 0.5 } else { 1. })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.rack_ui.ret_sel.insert(id, i);
                    cx.notify();
                }))
                .child(label((b'A' + i as u8) as char, TEXT_XS, pal.muted).w(px(12.)))
                .child(
                    div()
                        .id(("rname", key))
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .child(label(crate::paint::fit(&r.name, 68., TEXT_SM), TEXT_SM, pal.text))
                        .tooltip(widgets::tip_owned(if devices.is_empty() { format!("{}: no devices yet", r.name) } else { format!("{}: {devices}", r.name) })),
                )
                .child(knob_control_sized(("rvol", key), db_to_fader(r.volume_db), false, !r.mute, 24., KnobTarget::Return { rack: id, index: i }, cx))
                .child(mini_chip(("rmute", key), "M", r.mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let on = this.s.rack_of(id).and_then(|r| r.returns.get(i)).is_some_and(|r| r.mute);
                    this.s.set_return_prop(id, i, ChainProp::Mute(!on));
                    cx.notify();
                })))
                .child(mini_chip(("rdel", key), "×", false, pal.record).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.rack_ui.ret_sel.remove(&id);
                    this.s.remove_return(id, i);
                    cx.notify();
                }))),
        );
    }
    if rack.returns.is_empty() {
        col = col.child(label("Effects the pads send to", TEXT_XS, pal.faint));
    }
    col.into_any_element()
}

// ───────────────────────────── Zones ─────────────────────────────

/// A zone bar over 0..=127: the zone as a trapezoid (its fades are the
/// slopes), a handle at each end and a small one at the foot of each slope.
fn zone_bar(id: DeviceId, ci: usize, tab: ZoneTab, c: &Chain, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let ((lo, hi), fade) = tab.read(c);
    let key = ((id.0 as usize) << 8 | ci) << 2 | tab as usize;
    let x = |v: u8| f32::from(v) * ZONE_W / 128.;
    let color = track_color(c.color);
    let handle = |tag: &'static str, at: f32, h: f32, top: f32, on: Handle, cx: &mut Context<Workspace>| {
        div()
            .id((tag, key))
            .absolute()
            .left(px(at - HANDLE / 2.))
            .top(px(top))
            .w(px(HANDLE))
            .h(px(h))
            .rounded(px(2.))
            .bg(pal.text)
            .cursor_ew_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.begin_drag(Drag::Rack(RackDrag::Zone { rack: id, chain: ci, tab, handle: on, start_x: f32::from(e.position.x), lo, hi, fade }), cx);
                }),
            )
    };
    let (fl, fh) = (fade.0.min(hi - lo), fade.1.min(hi - lo));
    div()
        .relative()
        .flex_none()
        .w(px(ZONE_W))
        .h(px(24.))
        .rounded(px(r_sm()))
        .bg(content(0.05))
        .child(
            canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let o = b.origin;
                    let (top, bottom) = (o.y + px(4.), o.y + b.size.height - px(4.));
                    let at = |v: f32, y| point(o.x + px(v), y);
                    let pts = [at(x(lo), bottom), at(x(lo + fl), top), at(x(hi - fh), top), at(x(hi), bottom)];
                    crate::paint::polygon(window, &pts, with_alpha(color, 0.55));
                    crate::paint::polyline(window, &[pts[0], pts[1], pts[2], pts[3]], 1., color);
                },
            )
            .absolute()
            .size_full(),
        )
        .child(handle("zlo", x(lo), 20., 2., Handle::Low, cx))
        .child(handle("zhi", x(hi), 20., 2., Handle::High, cx))
        .child(handle("zfl", x(lo + fl), 8., 2., Handle::FadeLow, cx))
        .child(handle("zfh", x(hi - fh), 8., 2., Handle::FadeHigh, cx))
}

/// The chain selector: a marker on 0..=127 to drag; the chains whose zone it
/// is inside are the ones that sound.
fn selector_bar(id: DeviceId, value: u8, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let x = f32::from(value) * ZONE_W / 128.;
    div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(div().w(px(72.)).flex_none().child(label("Selector", TEXT_SM, pal.accent).font_weight(FontWeight::MEDIUM)))
        .child(
            div()
                .id(("selector-bar", id.0 as usize))
                .relative()
                .flex_none()
                .w(px(ZONE_W))
                .h(px(24.))
                .rounded(px(r_sm()))
                .bg(content(0.08))
                .cursor_ew_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        // Pressing anywhere on the bar jumps there, then drags from that spot.
                        this.s.seal();
                        this.begin_drag(Drag::Rack(RackDrag::Selector { rack: id, start_x: f32::from(e.position.x), start: value }), cx);
                    }),
                )
                .child(div().absolute().left(px(0.)).top(px(10.)).w(px(x)).h(px(4.)).rounded(px(2.)).bg(accent_alpha(0.35)))
                .child(div().absolute().left(px(x - HANDLE / 2.)).top(px(0.)).w(px(HANDLE)).h(px(24.)).rounded(px(2.)).bg(pal.accent)),
        )
        .child(mono(value.to_string(), TEXT_XS, pal.muted).w(px(64.)))
}

fn zones_panel(ws: &mut Workspace, id: DeviceId, rack: &Rack, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let tab = ws.rack_ui.zone_tab.get(&id).copied().unwrap_or_default();
    let mut tabs = div().flex().gap(px(SP1));
    for (t, name) in ZoneTab::ALL {
        tabs = tabs.child(pill(("ztab", (id.0 as usize) << 2 | t as usize), name, t == tab).on_click(cx.listener(move |this, _, _, cx| {
            this.rack_ui.zone_tab.insert(id, t);
            cx.notify();
        })));
    }
    let mut rows = div().flex().flex_col().gap(px(SP1));
    for (ci, c) in rack.chains.iter().enumerate() {
        let ((lo, hi), _) = tab.read(c);
        let range = match tab {
            ZoneTab::Key => format!("{} – {}", note_name(lo), note_name(hi)),
            _ => format!("{lo} – {hi}"),
        };
        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().w(px(72.)).flex_none().overflow_hidden().child(label(crate::paint::fit(&c.name, 72., TEXT_SM), TEXT_SM, pal.text)))
                .child(zone_bar(id, ci, tab, c, cx))
                .child(mono(range, TEXT_XS, pal.muted).w(px(64.))),
        );
    }
    let selector = rack.chain_selector;
    let selector_row = selector_bar(id, selector, cx);
    div()
        .flex_none()
        .h_full()
        .p(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .border_l_1()
        .border_color(border())
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(label("ZONES", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                .child(tabs),
        )
        .when(tab == ZoneTab::Chain, |d| d.child(selector_row))
        .child(rows)
        .child(label("Drag a handle: the ends set the zone, the small ones the fades. Shift for fine.", TEXT_XS, pal.faint))
        .into_any_element()
}

// ───────────────────────────── Macros ─────────────────────────────

/// A mapping's range bar: two handles; dragging one past the other inverts it.
fn range_bar(id: DeviceId, mi: usize, index: usize, min: f32, max: f32, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let key = ((id.0 as usize) << 8 | mi) << 6 | index;
    let (a, b) = (min.min(max) * MAP_W, min.max(max) * MAP_W);
    let handle = |tag: &'static str, v: f32, which: Handle, cx: &mut Context<Workspace>| {
        div()
            .id((tag, key))
            .absolute()
            .left(px(v * MAP_W - HANDLE / 2.))
            .top(px(2.))
            .w(px(HANDLE))
            .h(px(16.))
            .rounded(px(2.))
            .bg(pal.text)
            .cursor_ew_resize()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.begin_drag(Drag::Rack(RackDrag::Mapping { rack: id, macro_index: mi, index, handle: which, start_x: f32::from(e.position.x), min, max }), cx);
            }))
    };
    div()
        .relative()
        .flex_none()
        .w(px(MAP_W))
        .h(px(20.))
        .rounded(px(r_sm()))
        .bg(content(0.06))
        .child(div().absolute().left(px(a)).top(px(6.)).w(px((b - a).max(1.))).h(px(8.)).rounded(px(2.)).bg(if min > max { with_alpha(pal.record, 0.6) } else { accent_alpha(0.6) }))
        .child(handle("map-min", min, Handle::Low, cx))
        .child(handle("map-max", max, Handle::High, cx))
}

fn macros_panel(ws: &mut Workspace, id: DeviceId, rack: &Rack, enabled: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let sixteen = ws.rack_ui.sixteen.contains(&id);
    let shown = if sixteen { 16 } else { 8 }.min(rack.macros.len());
    let map_here = ws.rack_ui.map.filter(|(r, _)| *r == id).map(|(_, m)| m);
    let selected = map_here.or_else(|| ws.rack_ui.shown_macro.get(&id).copied()).unwrap_or(0).min(rack.macros.len().saturating_sub(1));
    let mut grid = div().w(px(8. * (CELL + 2.))).flex().flex_wrap().gap_y(px(SP3));
    for i in 0..shown {
        let m = &rack.macros[i];
        let locked = ws.rack_ui.locked.contains(&(id, i));
        let k = knob_control(("macro", (id.0 as usize) << 8 | i), m.value, false, enabled, KnobTarget::Macro { rack: id, index: i }, cx);
        grid = grid.child(
            div()
                .id(("macro-cell", (id.0 as usize) << 8 | i))
                .rounded(px(r_sm()))
                .border_1()
                .border_color(if i == selected { accent_alpha(if map_here.is_some() { 0.9 } else { 0.4 }) } else { gpui::transparent_black() })
                .when(locked, |d| d.opacity(0.7))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.rack_ui.shown_macro.insert(id, i);
                    if this.rack_ui.map.is_some_and(|(r, _)| r == id) {
                        this.rack_ui.map = Some((id, i));
                    }
                    cx.notify();
                }))
                .child(knob_cell(k, &m.name, format!("{:.0}%", m.value * 100.), CELL)),
        );
    }
    let header = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(label("MACROS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1())
        .child(pill(("macros-8", id.0 as usize), "8", !sixteen).on_click(cx.listener(move |this, _, _, cx| {
            this.rack_ui.sixteen.remove(&id);
            cx.notify();
        })))
        .child(pill(("macros-16", id.0 as usize), "16", sixteen).on_click(cx.listener(move |this, _, _, cx| {
            this.rack_ui.sixteen.insert(id);
            cx.notify();
        })))
        .child(pill(("macros-map", id.0 as usize), "Map", map_here.is_some()).on_click(cx.listener(move |this, _, _, cx| {
            this.rack_ui.map = if this.rack_ui.map.is_some_and(|(r, _)| r == id) { None } else { Some((id, this.rack_ui.shown_macro.get(&id).copied().unwrap_or(0))) };
            cx.notify();
        })))
        .child(pill(("macros-rand", id.0 as usize), "Rand", false).on_click(cx.listener(move |this, _, _, cx| {
            let locked: BTreeSet<usize> = this.rack_ui.locked.iter().filter(|(r, _)| *r == id).map(|(_, m)| *m).collect();
            let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
            this.s.randomize_macros(id, &locked, seed);
            cx.notify();
        })));
    let mut variations = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(label("VARIATIONS", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD));
    for (vi, var) in rack.variations.iter().enumerate() {
        let key = (id.0 as usize) << 8 | vi;
        variations = variations.child(
            div()
                .id(("rack-var", key))
                .h(px(20.))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .bg(content(0.06))
                .border_1()
                .border_color(border())
                .cursor_pointer()
                .hover(|s| s.bg(content(0.12)).border_color(accent()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let kept: BTreeSet<usize> = this.rack_ui.locked.iter().filter(|(r, _)| *r == id).map(|(_, m)| *m).collect();
                    this.s.recall_variation_except(id, vi, &kept);
                    cx.notify();
                }))
                .child(label(var.name.clone(), TEXT_XS, pal.text))
                .child(
                    div()
                        .id(("rack-var-del", key))
                        .px(px(2.))
                        .rounded(px(2.))
                        .hover(|s| s.bg(with_alpha(pal.record, 0.25)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.delete_rack_variation(id, vi);
                            cx.notify();
                        }))
                        .child(icon(Icon::X, 10., pal.faint)),
                ),
        );
    }
    variations = variations.child(
        pill(("rack-var-save", id.0 as usize), "+ Save", false).on_click(cx.listener(move |this, _, _, cx| {
            this.s.save_rack_variation(id, None);
            cx.notify();
        })),
    );
    let detail = mapping_list(ws, id, rack, selected, map_here.is_some(), cx);
    div()
        .flex_none()
        .h_full()
        .border_l_1()
        .border_color(border())
        .overflow_hidden()
        .child(div().p(px(SP2)).flex().flex_col().gap(px(SP2)).child(header).child(grid).child(variations).child(detail))
        .into_any_element()
}

/// The mappings of the macro picked: `Device › Parameter`, the range bar and delete.
fn mapping_list(ws: &mut Workspace, id: DeviceId, rack: &Rack, mi: usize, mapping: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(m) = rack.macros.get(mi) else { return div().into_any_element() };
    let locked = ws.rack_ui.locked.contains(&(id, mi));
    let mut col = div().flex().flex_col().gap(px(SP1)).pt(px(SP2)).border_t_1().border_color(border()).child(
        div()
            .flex()
            .items_center()
            .gap(px(SP2))
            .child(label(format!("{} · {} mapped", m.name, m.mappings.len()), TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM))
            .child(div().flex_1())
            .child(pill(("macro-lock", (id.0 as usize) << 8 | mi), if locked { "Locked" } else { "Lock" }, locked).on_click(cx.listener(move |this, _, _, cx| {
                if !this.rack_ui.locked.remove(&(id, mi)) {
                    this.rack_ui.locked.insert((id, mi));
                }
                cx.notify();
            }))),
    );
    if mapping {
        col = col.child(label(format!("Map mode: touch a parameter of a device in this rack to map it to {}.", m.name), TEXT_XS, pal.accent));
    }
    for (i, map) in m.mappings.iter().enumerate() {
        let dev = ws.s.project.device(map.device).map(|d| d.name.clone()).unwrap_or_else(|| "Device".to_owned());
        let param = ws
            .fx
            .param_info(ws.b.plugins.as_ref(), &ws.s.project, map.device, map.param)
            .map_or_else(|| format!("Param {}", map.param), |info| info.name.to_string());
        let key = ((id.0 as usize) << 8 | mi) << 6 | i;
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().w(px(152.)).flex_none().overflow_hidden().child(label(crate::paint::fit(&format!("{dev} › {param}"), 152., TEXT_SM), TEXT_SM, pal.text)))
                .child(range_bar(id, mi, i, map.min, map.max, cx))
                .child(mono(format!("{:.0}–{:.0}%", map.min * 100., map.max * 100.), TEXT_XS, pal.muted).w(px(56.)))
                .child(
                    widgets::sized_icon_button(("map-del", key), Icon::X, false, 20., 12.)
                        .tooltip(widgets::tip("Remove the mapping", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.s.unmap_macro(id, mi, i);
                            cx.notify();
                        })),
                ),
        );
    }
    if m.mappings.is_empty() && !mapping {
        col = col.child(label("Nothing mapped: press Map, then touch a parameter.", TEXT_XS, pal.faint));
    }
    col.into_any_element()
}

/// Map mode swallows a press on a parameter knob or a macro: the parameter is
/// mapped to the macro picked, or the macro is picked. `true` when it took the press.
pub fn map_pick(ws: &mut Workspace, target: KnobTarget, cx: &mut Context<Workspace>) -> bool {
    let Some((rack, mi)) = ws.rack_ui.map else { return false };
    match target {
        KnobTarget::Macro { rack: r, index } if r == rack => {
            ws.rack_ui.map = Some((rack, index));
            ws.rack_ui.shown_macro.insert(rack, index);
            cx.notify();
            true
        }
        KnobTarget::Param { device, param, default } => {
            if !rack_contains(&ws.s.project, rack, device) {
                ws.toast("That device is not inside this rack", cx);
                return true;
            }
            let current = ws.s.project.device(device).and_then(|d| d.params.get(&param).copied()).unwrap_or(default);
            let name = ws.s.rack_of(rack).and_then(|r| r.macros.get(mi)).map(|m| m.name.clone()).unwrap_or_default();
            match ws.s.map_macro_param(rack, mi, device, param, current) {
                Some(_) => ws.toast(format!("Mapped to {name}: set its range with the handles"), cx),
                None => ws.toast("That parameter is already mapped to a macro", cx),
            }
            cx.notify();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Device, Project, TrackKind};

    #[test]
    fn a_pad_plays_the_sample_of_its_first_sampler_even_inside_a_nested_rack() {
        let sampler = |id: u64, path: &str| {
            let mut d = Device::stock(DeviceId(id), "instrument.sampler", "Sampler");
            d.state = Some(format!(r#"{{"version":1,"params":{{}},"extra":{{"path":"{path}"}}}}"#).into_bytes());
            d
        };
        let mut pad = Chain::new("Kick", aitrack_model::Color(0x808080));
        pad.devices.push(Device::stock(DeviceId(1), "fx.eq", "EQ"));
        pad.devices.push(sampler(2, "/kits/kick.wav"));
        assert_eq!(pad_sample(&pad), Some(PathBuf::from("/kits/kick.wav")));

        let mut inner = Rack::new(RackKind::Instrument);
        let mut layer = Chain::new("Layer", aitrack_model::Color(0x808080));
        layer.devices.push(sampler(3, "/kits/snare.wav"));
        inner.chains.push(layer);
        let mut nested = Chain::new("Snare", aitrack_model::Color(0x808080));
        nested.devices.push(Device { kind: DeviceKind::Rack(inner), ..Device::stock(DeviceId(4), "rack.instrument", "Rack") });
        assert_eq!(pad_sample(&nested), Some(PathBuf::from("/kits/snare.wav")));

        let mut synth = Chain::new("Synth", aitrack_model::Color(0x808080));
        synth.devices.push(Device::stock(DeviceId(5), "instrument.wavetable", "Wavetable"));
        assert_eq!(pad_sample(&synth), None, "no sampler, no file to play");
    }

    #[test]
    fn notes_are_named_as_live_names_them() {
        assert_eq!((note_name(36).as_str(), note_name(60).as_str(), note_name(127).as_str(), note_name(0).as_str()), ("C1", "C3", "G8", "C-2"));
        assert_eq!(note_name(37), "C#1");
    }

    #[test]
    fn a_drum_rack_opens_on_the_page_of_its_lowest_pad() {
        let mut r = Rack::new(RackKind::Drum);
        assert_eq!(default_bank(&r), 36, "an empty rack opens on C1");
        for n in [40, 38, 51] {
            let mut c = Chain::new("p", aitrack_model::Color::palette(0));
            c.note = Some(n);
            r.chains.push(c);
        }
        assert_eq!(default_bank(&r), 38);
        r.chains[1].note = Some(125);
        r.chains.remove(0);
        assert_eq!(default_bank(&r), 51);
    }

    #[test]
    fn the_selector_follows_the_drag_and_stays_in_range() {
        assert_eq!(selector_after_drag(60, 10), 70);
        assert_eq!(selector_after_drag(60, -100), 0);
        assert_eq!(selector_after_drag(120, 50), 127);
    }

    #[test]
    fn a_send_knob_runs_from_off_to_the_top() {
        assert_eq!(send_db(0.), SEND_OFF_DB);
        assert_eq!(send_db(1.), SEND_MAX_DB);
        for db in [SEND_OFF_DB, -30., 0., SEND_MAX_DB] {
            assert!((send_db(send_pos(db)) - db).abs() < 1e-4);
        }
        assert_eq!(db_text(SEND_OFF_DB), "−∞");
    }

    #[test]
    fn mapping_handles_move_by_the_bar_and_may_cross() {
        assert_eq!(mapping_after_drag(Handle::Low, 0.2, 0.8, 0.1), (0.3, 0.8));
        assert_eq!(mapping_after_drag(Handle::High, 0.2, 0.8, -0.3), (0.2, 0.5));
        let (min, max) = mapping_after_drag(Handle::Low, 0.2, 0.8, 0.7);
        assert!(min > max, "dragged past the other handle: inverted");
        assert_eq!(mapping_after_drag(Handle::High, 0.2, 0.8, 5.), (0.2, 1.), "the bar ends at 100 %");
        assert_eq!(mapping_after_drag(Handle::Low, 0.2, 0.8, -5.), (0., 0.8));
    }

    #[test]
    fn zone_handles_keep_the_ends_in_order_and_the_fades_inside_the_zone() {
        let k = ZoneTab::Key;
        assert_eq!(zone_after_drag(k, Handle::Low, 36, 60, (0, 0), 10), ((46, 60), (0, 0)));
        assert_eq!(zone_after_drag(k, Handle::Low, 36, 60, (0, 0), 100), ((60, 60), (0, 0)), "never past the high end");
        assert_eq!(zone_after_drag(k, Handle::High, 36, 60, (0, 0), -100), ((36, 36), (0, 0)));
        assert_eq!(zone_after_drag(k, Handle::High, 36, 60, (0, 0), 100), ((36, 127), (0, 0)));
        // Fades: from each end inward, together never wider than the zone (24 keys).
        assert_eq!(zone_after_drag(k, Handle::FadeLow, 36, 60, (0, 0), 8), ((36, 60), (8, 0)));
        assert_eq!(zone_after_drag(k, Handle::FadeLow, 36, 60, (0, 20), 8), ((36, 60), (4, 20)));
        assert_eq!(zone_after_drag(k, Handle::FadeHigh, 36, 60, (6, 8), 10), ((36, 60), (6, 0)), "dragging right shrinks the high fade");
        assert_eq!(zone_after_drag(k, Handle::FadeHigh, 36, 60, (6, 0), -50), ((36, 60), (6, 18)), "dragging left widens it, to what the zone leaves");
        // Shrinking the zone squeezes the fades.
        assert_eq!(zone_after_drag(k, Handle::High, 36, 60, (10, 10), -16), ((36, 44), (8, 0)));
        // Velocity zones start at 1.
        assert_eq!(zone_after_drag(ZoneTab::Vel, Handle::Low, 1, 127, (0, 0), -10).0, (1, 127));
        assert_eq!(zone_after_drag(ZoneTab::Chain, Handle::Low, 0, 127, (0, 0), -10).0, (0, 127));
    }

    #[test]
    fn a_device_is_inside_a_rack_however_deep() {
        let mut s = crate::state::AppState::new(Project::new("t"));
        let t = s.add_track(TrackKind::Audio).unwrap();
        let outer = s.add_stock_device(t, "rack.audio", "Outer").unwrap();
        let inner = s.project.alloc_device_id();
        let eq = s.project.alloc_device_id();
        let mut rack = Rack::new(RackKind::Audio);
        let mut chain = Chain::new("c", aitrack_model::Color::palette(1));
        chain.devices.push(Device::stock(eq, "fx.eq", "EQ"));
        rack.chains.push(chain);
        let mut inner_dev = Device::stock(inner, "rack.audio", "Inner");
        inner_dev.kind = DeviceKind::Rack(rack);
        let mut outer_chain = Chain::new("o", aitrack_model::Color::palette(2));
        outer_chain.devices.push(inner_dev);
        s.edit("chain", vec![aitrack_model::Command::AddChain { rack: outer, index: 0, chain: Box::new(outer_chain) }]);
        let loose = s.add_stock_device(t, "fx.eq", "Loose").unwrap();
        assert!(rack_contains(&s.project, outer, inner) && rack_contains(&s.project, outer, eq));
        assert!(!rack_contains(&s.project, outer, loose), "a device beside the rack is not inside it");
        assert!(!rack_contains(&s.project, inner, outer));
        // Map mode maps what is inside, through the same command.
        assert_eq!(s.map_macro_param(outer, 0, eq, 2, 0.4), Some(0));
    }
}
