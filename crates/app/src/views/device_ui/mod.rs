//! Visual editors for the stock devices: the display is the control.
//!
//! `devices.rs` asks [`editor`] for a device's body by plugin id; plugins
//! without an editor of their own get the grouped knob grid in `generic`.
//! Every editor is a set of plain render functions over the [`Workspace`]:
//!
//! * [`Dev`] is a per-frame snapshot of one device (parameter values, the
//!   plugin's `meter()` data through the engine bridge, a short history of
//!   it for scrolling displays).
//! * `kit` holds what editors share: plots with draggable handles, compact
//!   number and choice controls, grids and curve painting.
//! * Handle drags are one gesture type ([`FxDrag`]): relative moves of up to
//!   three normalised parameters, applied as coalesced `SetParam` commands
//!   and sealed on mouse-up, so a drag is one undo step.
//!
//! Frames are requested only while the transport plays (the workspace does
//! that), a handle is dragged, or meter data is still changing.

pub mod analyzer;
pub mod chorus;
pub mod clipper;
pub mod delay;
pub mod drum;
pub mod dynamics;
pub mod eq;
pub mod filter;
pub mod generic;
pub mod kit;
pub mod limiter;
pub mod midside;
pub mod multiband;
pub mod reverb;
pub mod sampler;
pub mod saturator;
pub mod shaper;
pub mod utility;
pub mod vault;
pub mod wavetable;

use crate::bridge::{EngineBridge, PluginCatalog};
use crate::theme::*;
use crate::widgets::{self, label, menu_header, menu_item};
use crate::workspace::Workspace;
use aitrack_dsp::ParamInfo;
use aitrack_model::{Command, Device, DeviceId};
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, FocusHandle, InteractiveElement, IntoElement, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, StatefulInteractiveElement, Styled, WindowId, anchored, canvas, deferred, div, prelude::FluentBuilder, px,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Most values a device's `meter()` returns.
pub const METER_CAP: usize = 320;
/// Points of the scrolling histories (60 per second).
pub const HISTORY: usize = 240;

/// A plugin's parameters, addressable by id.
pub struct ParamSet {
    pub list: Vec<ParamInfo>,
    index: HashMap<u32, usize>,
}

impl ParamSet {
    pub fn new(list: Vec<ParamInfo>) -> Self {
        let index = list.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        Self { list, index }
    }

    pub fn get(&self, id: u32) -> Option<&ParamInfo> {
        self.index.get(&id).map(|i| &self.list[*i])
    }
}

/// Three traces of recent meter values (what they are depends on the plugin).
#[derive(Default)]
pub struct History {
    pub traces: [VecDeque<f32>; 3],
}

impl History {
    fn push(&mut self, v: [f32; 3]) {
        for (t, v) in self.traces.iter_mut().zip(v) {
            if t.len() >= HISTORY {
                t.pop_front();
            }
            t.push_back(v);
        }
    }
}

/// One device as its editor sees it this frame.
#[derive(Clone)]
pub struct Dev {
    pub id: DeviceId,
    pub enabled: bool,
    /// The expanded editor (more controls, a taller dock).
    pub big: bool,
    pub set: Rc<ParamSet>,
    /// Normalised values the project stores (missing = default).
    pub values: Rc<BTreeMap<u32, f32>>,
    pub meter: Rc<Vec<f32>>,
    pub hist: Rc<RefCell<History>>,
    /// Selected band / pad / handle.
    pub selected: usize,
    pub sample_rate: f64,
    pub bpm: f64,
}

impl Dev {
    pub fn info(&self, id: u32) -> Option<&ParamInfo> {
        self.set.get(id)
    }

    /// Normalised value.
    pub fn norm(&self, id: u32) -> f32 {
        self.values.get(&id).copied().unwrap_or_else(|| self.info(id).map_or(0., ParamInfo::default_normalized))
    }

    /// Value in the parameter's unit (Hz, dB, choice index…).
    pub fn plain(&self, id: u32) -> f32 {
        self.info(id).map_or(0., |i| i.to_plain(self.norm(id)))
    }

    pub fn choice(&self, id: u32) -> usize {
        self.info(id).map_or(0, |i| (self.plain(id) - i.min).round().max(0.) as usize)
    }

    pub fn on(&self, id: u32) -> bool {
        self.plain(id) >= 0.5
    }

    pub fn text(&self, id: u32) -> String {
        self.info(id).map_or_else(String::new, |i| i.format(self.norm(id)))
    }

    pub fn to_norm(&self, id: u32, plain: f32) -> f32 {
        self.info(id).map_or(0., |i| i.to_normalized(plain))
    }

    /// Meter value `i` (0 when the plugin has not published it yet).
    pub fn m(&self, i: usize) -> f32 {
        self.meter.get(i).copied().unwrap_or(0.)
    }

    /// A stable element-id component for this device's control `local`.
    pub fn key(&self, local: usize) -> usize {
        (self.id.0 as usize) << 12 | (local & 0xFFF)
    }
}

/// One parameter moved by a drag: its normalised value and how much it
/// changes per pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub param: u32,
    pub value: f32,
    pub per_px: f32,
}

/// A handle drag: horizontal moves change `x`, vertical ones `y`; with Alt
/// held the vertical move goes to `alt` (Q, knee, slope). Shift is fine.
#[derive(Clone, Debug)]
pub struct FxDrag {
    pub device: DeviceId,
    pub x: Option<Axis>,
    pub y: Option<Axis>,
    pub alt: Option<Axis>,
    pub last: (f32, f32),
}

impl FxDrag {
    /// Apply a mouse move to `pos`; returns the parameters that changed.
    pub fn advance(&mut self, pos: (f32, f32), shift: bool, alt: bool) -> Vec<(u32, f32)> {
        let fine = if shift { 0.2 } else { 1. };
        let (dx, dy) = ((pos.0 - self.last.0) * fine, (pos.1 - self.last.1) * fine);
        self.last = pos;
        let mut out = Vec::new();
        let mut step = |axis: &mut Option<Axis>, d: f32| {
            if let Some(a) = axis {
                let v = (a.value + d * a.per_px).clamp(0., 1.);
                if (v - a.value).abs() > 1e-7 {
                    a.value = v;
                    out.push((a.param, v));
                }
            }
        };
        if alt && self.alt.is_some() {
            step(&mut self.alt, dy);
        } else {
            step(&mut self.x, dx);
            step(&mut self.y, dy);
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PopKind {
    /// The choices of a stepped parameter.
    Choice { device: DeviceId, param: u32 },
    /// An EQ band's menu: type, slope, channel, dynamic, delete.
    EqBand { device: DeviceId, band: usize },
    /// The track a device listens to on its sidechain; `param` is its
    /// Internal/External switch, set along with the source.
    Sidechain { device: DeviceId, param: u32 },
    /// The synth window's menus (wavetables, matrix sources and destinations…).
    Synth(crate::views::device_window::synth::SynthPop),
}

#[derive(Clone, Debug)]
pub struct Pop {
    pub kind: PopKind,
    pub at: Point<Pixels>,
    /// The window it was opened in (`None` = the main window).
    pub window: Option<WindowId>,
    seq: u64,
}

pub type Region = Rc<Cell<Bounds<Pixels>>>;

struct Live {
    meter: Rc<Vec<f32>>,
    hist: Rc<RefCell<History>>,
    at: Instant,
    acc: f32,
}

/// View state of the device editors (lives on the workspace as `fx`).
pub struct UiState {
    pub drag: Option<FxDrag>,
    pub pop: Option<Pop>,
    pop_seq: u64,
    pub selected: HashMap<DeviceId, usize>,
    pub big: BTreeSet<DeviceId>,
    pub show_all: BTreeSet<DeviceId>,
    /// Racks whose macro knobs are shown.
    pub macros: BTreeSet<DeviceId>,
    /// Per-device display options (EQ range…), by slot.
    pub opts: HashMap<(DeviceId, u8), f32>,
    sets: HashMap<String, Rc<ParamSet>>,
    live: HashMap<DeviceId, Live>,
    regions: HashMap<(DeviceId, u8, u64), Region>,
    focus: HashMap<(DeviceId, u64), FocusHandle>,
    pub waves: kit::WaveCache,
    /// Wavetable frames for the synth displays, built off the UI thread.
    pub wave_previews: wavetable::Previews,
    /// The window whose input is being handled (set by each window's root
    /// on mouse-down, before the controls see the event; `None` = main).
    pub surface: Option<WindowId>,
    /// The window being rendered (`None` = main): plots keep their bounds
    /// and focus per window, so one device can be open in two at once.
    pub painting: Option<WindowId>,
    /// View state of the synth windows (tabs, editor tools, gestures).
    pub synth: HashMap<DeviceId, crate::views::device_window::synth::SynthView>,
    /// The Vault browsers (factory list, favourites, filters per device).
    pub vault: crate::views::device_window::vault::VaultState,
    last_change: Instant,
    pub sample_rate: f64,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            drag: None,
            pop: None,
            pop_seq: 0,
            selected: HashMap::new(),
            big: BTreeSet::new(),
            show_all: BTreeSet::new(),
            macros: BTreeSet::new(),
            opts: HashMap::new(),
            sets: HashMap::new(),
            live: HashMap::new(),
            regions: HashMap::new(),
            focus: HashMap::new(),
            waves: kit::WaveCache::default(),
            wave_previews: wavetable::Previews::default(),
            surface: None,
            painting: None,
            synth: HashMap::new(),
            vault: Default::default(),
            last_change: Instant::now() - Duration::from_secs(10),
            sample_rate: 48_000.,
        }
    }
}

/// Which meter values a plugin's scrolling history follows.
fn history_slots(plugin: &str) -> Option<[usize; 3]> {
    Some(match plugin {
        "fx.compressor" | "fx.gate" => [0, 1, 2],
        "fx.transient" => [0, 0, 0],
        "fx.limiter" => [0, 2, 1],
        "fx.saturator" => [0, 1, 1],
        _ => return None,
    })
}

impl UiState {
    /// The parameter list of a stock plugin (cached).
    pub fn params(&mut self, plugins: &dyn PluginCatalog, plugin: &str) -> Rc<ParamSet> {
        if let Some(s) = self.sets.get(plugin) {
            return s.clone();
        }
        let set = Rc::new(ParamSet::new(plugins.device(plugin).map(|i| i.params.clone()).unwrap_or_default()));
        self.sets.insert(plugin.to_owned(), set.clone());
        set
    }

    /// The description of parameter `param` of stock device `device`.
    pub fn param_info(&mut self, plugins: &dyn PluginCatalog, project: &aitrack_model::Project, device: DeviceId, param: u32) -> Option<ParamInfo> {
        let aitrack_model::DeviceKind::Stock { plugin_id } = &project.device(device)?.kind else { return None };
        self.params(plugins, plugin_id).get(param).cloned()
    }

    /// Read a device's meter data; returns whether it changed. Histories
    /// advance at 60 points per second however often this runs.
    pub fn poll(&mut self, engine: &mut dyn EngineBridge, device: DeviceId, plugin: &str) -> bool {
        let mut buf = [0.0f32; METER_CAP];
        let n = engine.device_meter(device, &mut buf);
        let now = Instant::now();
        let live = self.live.entry(device).or_insert_with(|| Live { meter: Rc::new(Vec::new()), hist: Rc::default(), at: now, acc: 0. });
        let changed = live.meter.len() != n || live.meter.iter().zip(&buf[..n]).any(|(a, b)| (a - b).abs() > 1e-4);
        if changed {
            live.meter = Rc::new(buf[..n].to_vec());
            self.last_change = now;
        }
        live.acc += now.duration_since(live.at).as_secs_f32().min(0.2) * 60.;
        live.at = now;
        if let Some(slots) = history_slots(plugin) {
            let v = slots.map(|i| buf[..n].get(i).copied().unwrap_or(0.));
            let mut hist = live.hist.borrow_mut();
            while live.acc >= 1. {
                hist.push(v);
                live.acc -= 1.;
            }
        } else {
            live.acc = 0.;
        }
        changed
    }

    /// Snapshot a stock device for its editor (and refresh its meter data).
    pub fn dev(&mut self, engine: &mut dyn EngineBridge, plugins: &dyn PluginCatalog, d: &Device, plugin: &str, bpm: f64) -> Dev {
        let set = self.params(plugins, plugin);
        self.poll(engine, d.id, plugin);
        let live = &self.live[&d.id];
        Dev {
            id: d.id,
            enabled: d.enabled,
            big: self.big.contains(&d.id),
            set,
            values: Rc::new(d.params.clone()),
            meter: live.meter.clone(),
            hist: live.hist.clone(),
            selected: self.selected.get(&d.id).copied().unwrap_or(0),
            sample_rate: self.sample_rate,
            bpm,
        }
    }

    /// Whether the editors still need frames (a drag, or meters settling).
    pub fn wants_frames(&self) -> bool {
        self.drag.is_some() || self.last_change.elapsed() < Duration::from_millis(1200)
    }

    /// The canvas bounds of a device's plot `slot`, written during prepaint.
    pub fn region(&mut self, device: DeviceId, slot: u8) -> Region {
        let w = self.painting.map_or(0, |w| w.as_u64());
        self.regions.entry((device, slot, w)).or_default().clone()
    }

    pub fn focus(&mut self, device: DeviceId, cx: &mut App) -> FocusHandle {
        let w = self.painting.map_or(0, |w| w.as_u64());
        self.focus.entry((device, w)).or_insert_with(|| cx.focus_handle()).clone()
    }

    pub fn opt(&self, device: DeviceId, slot: u8, default: f32) -> f32 {
        self.opts.get(&(device, slot)).copied().unwrap_or(default)
    }

    pub fn open_pop(&mut self, kind: PopKind, at: Point<Pixels>) {
        if self.pop.as_ref().is_some_and(|p| p.kind == kind) {
            self.pop = None;
        } else {
            self.pop_seq += 1;
            self.pop = Some(Pop { kind, at, window: self.surface, seq: self.pop_seq });
        }
    }

    /// Forget devices that left the project.
    pub fn prune(&mut self, alive: impl Fn(DeviceId) -> bool) {
        self.live.retain(|id, _| alive(*id));
        self.regions.retain(|(id, _, _), _| alive(*id));
        self.focus.retain(|(id, _), _| alive(*id));
        self.selected.retain(|id, _| alive(*id));
        self.wave_previews.prune(&alive);
        self.synth.retain(|id, _| alive(*id));
        self.vault.prune(&alive);
    }
}

// ───────────────────────────── Edits ─────────────────────────────

/// The value a parameter has in the project right now (normalised).
pub fn current(ws: &Workspace, device: DeviceId, info: &ParamInfo) -> f32 {
    ws.s.project.device(device).and_then(|d| d.params.get(&info.id).copied()).unwrap_or_else(|| info.default_normalized())
}

/// One sealed edit of a single parameter (clicks, menus, resets).
pub fn set_now(ws: &mut Workspace, device: DeviceId, param: u32, norm: f32) {
    set_many(ws, "Change parameter", device, &[(param, norm)]);
}

/// One sealed edit of several parameters of a device.
pub fn set_many(ws: &mut Workspace, label: &str, device: DeviceId, values: &[(u32, f32)]) {
    ws.s.seal();
    let cmds = values.iter().map(|&(param, v)| Command::SetParam { device, param, value: Some(v.clamp(0., 1.)) }).collect();
    ws.s.edit(label, cmds);
    ws.s.seal();
}

/// Part of a continuous gesture on one device (drag, scroll, key repeat).
pub fn set_gliding(ws: &mut Workspace, device: DeviceId, values: &[(u32, f32)]) {
    if values.is_empty() {
        return;
    }
    let cmds = values.iter().map(|&(param, v)| Command::SetParam { device, param, value: Some(v.clamp(0., 1.)) }).collect();
    ws.s.edit_coalesced(&format!("fx-{}", device.0), "Change parameter", cmds);
}

pub fn begin_drag(ws: &mut Workspace, drag: FxDrag, cx: &mut Context<Workspace>) {
    ws.s.seal();
    ws.fx.drag = Some(drag);
    cx.notify();
}

fn drag_move(ws: &mut Workspace, e: &MouseMoveEvent, cx: &mut Context<Workspace>) {
    let Some(mut drag) = ws.fx.drag.take() else { return };
    let changed = drag.advance((f32::from(e.position.x), f32::from(e.position.y)), e.modifiers.shift, e.modifiers.alt);
    set_gliding(ws, drag.device, &changed);
    ws.fx.drag = Some(drag);
    cx.notify();
}

fn drag_end(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if ws.fx.drag.take().is_some() {
        ws.s.seal();
        cx.notify();
    }
}

/// Window-wide mouse listeners, present only while a handle is dragged.
pub fn capture(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    ws.fx.drag.as_ref()?;
    let weak = cx.entity().downgrade();
    Some(
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let w = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, |this, cx| drag_move(this, e, cx));
                    }
                });
                let w = weak.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, drag_end);
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element(),
    )
}

// ───────────────────────────── Popovers ─────────────────────────────

/// The open popover (choice list or EQ band menu), anchored where it was
/// opened; each window draws only the popovers opened in it.
pub fn overlay(ws: &mut Workspace, window: Option<WindowId>, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pop = ws.fx.pop.clone().filter(|p| p.window == window)?;
    let body = match &pop.kind {
        PopKind::Choice { device, param } => choice_menu(ws, *device, *param, cx)?,
        PopKind::EqBand { device, band } => eq::band_menu(ws, *device, *band, cx)?,
        PopKind::Sidechain { device, param } => sidechain_menu(ws, *device, *param, cx)?,
        PopKind::Synth(pop) => crate::views::device_window::synth::pop_menu(ws, pop, cx)?,
    };
    let id = gpui::ElementId::Name(format!("fx-pop-{}", pop.seq).into());
    Some(
        deferred(
            anchored().position(pop.at).snap_to_window_with_margin(px(SP2)).child(
                div()
                    .id("fx-pop-layer")
                    .occlude()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.fx.pop = None;
                        cx.notify();
                    }))
                    .child(crate::motion::rise_in(id, div().child(body), 6.)),
            ),
        )
        .with_priority(2)
        .into_any_element(),
    )
}

fn choice_menu(ws: &mut Workspace, device: DeviceId, param: u32, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let plugin = match &ws.s.project.device(device)?.kind {
        aitrack_model::DeviceKind::Stock { plugin_id } => plugin_id.clone(),
        _ => return None,
    };
    let set = ws.fx.params(ws.b.plugins.as_ref(), &plugin);
    let info = set.get(param)?.clone();
    let now = (info.to_plain(current(ws, device, &info)) - info.min).round() as usize;
    let mut list = div().id("fx-choices").max_h(px(420.)).overflow_y_scroll().flex().flex_col();
    // Lists padded for future entries show only the named ones.
    for (i, name) in info.choices.iter().enumerate().filter(|(_, n)| **n != "—") {
        let norm = info.to_normalized(info.min + i as f32);
        list = list.child(menu_item(("fx-choice", i), None, *name, None, i == now).on_click(cx.listener(move |this, _, _, cx| {
            set_now(this, device, param, norm);
            this.fx.pop = None;
            cx.notify();
        })));
    }
    Some(widgets::menu_card("fx-choice-menu", 184.).child(menu_header(info.name.to_uppercase())).child(list).into_any_element())
}

/// The sidechain chip of a dynamics device: which track it listens to.
pub fn sidechain_chip(ws: &Workspace, dev: &Dev, param: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let device = dev.id;
    let source = ws.s.project.device(device).and_then(|d| d.sidechain).and_then(|t| ws.s.project.track(t)).map(|t| (t.name.clone(), t.color));
    let on = source.is_some();
    div()
        .id(("sidechain", dev.key(param as usize)))
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
        .tooltip(widgets::tip("Sidechain: the track this device listens to instead of its own input", None))
        .child(crate::icons::icon(crate::icons::Icon::Link, 10., if on { pal.accent } else { pal.faint }))
        .when_some(source.as_ref().map(|s| s.1), |d, color| d.child(div().size(px(6.)).rounded_full().bg(track_color(color))))
        .child(label(source.map_or("Sidechain".to_owned(), |s| crate::paint::fit(&s.0, 88., TEXT_XS)), TEXT_XS, if on { pal.text } else { pal.muted }))
        .child(crate::icons::icon(crate::icons::Icon::ChevronDown, 10., pal.faint))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            let at = e.position();
            this.fx.open_pop(PopKind::Sidechain { device, param }, gpui::point(at.x - px(8.), at.y + px(12.)));
            cx.notify();
        }))
        .into_any_element()
}

/// Commands that connect `device`'s sidechain to `source` and flip its
/// Internal/External switch to match, as one edit.
pub fn sidechain_commands(ws: &Workspace, device: DeviceId, param: u32, source: Option<aitrack_model::TrackId>) -> Vec<Command> {
    let plugin = match ws.s.project.device(device).map(|d| &d.kind) {
        Some(aitrack_model::DeviceKind::Stock { plugin_id }) => plugin_id.clone(),
        _ => return Vec::new(),
    };
    let mut cmds = vec![Command::SetSidechain { device, source }];
    if let Some(info) = ws.b.plugins.device(&plugin).and_then(|i| i.params.iter().find(|x| x.id == param)) {
        let external = info.to_normalized(if source.is_some() { info.max } else { info.min });
        cmds.push(Command::SetParam { device, param, value: Some(external) });
    }
    cmds
}

fn sidechain_menu(ws: &mut Workspace, device: DeviceId, param: u32, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let own = ws.s.project.device_track(device)?;
    let current = ws.s.project.device(device)?.sidechain;
    let tracks: Vec<(aitrack_model::TrackId, String)> =
        ws.s.project.tracks.iter().filter(|t| t.id != own && t.kind.carries_audio()).map(|t| (t.id, t.name.clone())).collect();
    let mut list = div().id("fx-sidechain").max_h(px(420.)).overflow_y_scroll().flex().flex_col();
    let item = |i: usize, name: String, source: Option<aitrack_model::TrackId>| {
        menu_item(("fx-sc", i), None, name, None, current == source).on_click(cx.listener(move |this, _, _, cx| {
            let cmds = sidechain_commands(this, device, param, source);
            this.s.seal();
            this.s.edit("Route sidechain", cmds);
            this.s.seal();
            this.fx.pop = None;
            cx.notify();
        }))
    };
    list = list.child(item(0, "No sidechain (its own input)".to_owned(), None));
    for (i, (id, name)) in tracks.into_iter().enumerate() {
        list = list.child(item(i + 1, name, Some(id)));
    }
    Some(widgets::menu_card("fx-sidechain-menu", 216.).child(menu_header("LISTEN TO")).child(list).into_any_element())
}

// ───────────────────────────── Dispatch ─────────────────────────────

/// Whether `plugin` has a visual editor (everything else gets the knob grid).
pub fn has_editor(plugin: &str) -> bool {
    matches!(
        plugin,
        "fx.eq"
            | "fx.tape"
            | "fx.amp"
            | "fx.console"
            | "fx.filter"
            | "fx.compressor"
            | "fx.gate"
            | "fx.transient"
            | "fx.limiter"
            | "fx.clipper"
            | "fx.multiband"
            | "fx.midside"
            | "fx.saturator"
            | "fx.shaper"
            | "fx.reverb"
            | "fx.delay"
            | "fx.chorus"
            | "fx.utility"
            | "fx.analyzer"
            | "instrument.wavetable"
            | "instrument.sampler"
            | "instrument.drum"
            | "instrument.vault"
    )
}

/// The body of a stock device's card.
pub fn editor(ws: &mut Workspace, plugin: &str, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    match plugin {
        "fx.eq" => eq::render(ws, dev, cx),
        "fx.filter" => filter::render(ws, dev, cx),
        "fx.compressor" | "fx.gate" | "fx.transient" => dynamics::render(ws, plugin, dev, cx),
        "fx.limiter" => limiter::render(ws, dev, cx),
        "fx.clipper" => clipper::render(ws, dev, cx),
        "fx.multiband" => multiband::render(ws, dev, cx),
        "fx.midside" => midside::render(ws, dev, cx),
        "fx.saturator" => saturator::render(ws, dev, cx),
        "fx.tape" => saturator::tape::render(ws, dev, cx),
        "fx.amp" => saturator::amp::render(ws, dev, cx),
        "fx.console" => saturator::console::render(ws, dev, cx),
        "fx.shaper" => shaper::render(ws, dev, cx),
        "fx.reverb" => reverb::render(ws, dev, cx),
        "fx.delay" => delay::render(ws, dev, cx),
        "fx.chorus" => chorus::render(ws, dev, cx),
        "fx.utility" => utility::render(ws, dev, cx),
        "fx.analyzer" => analyzer::render(ws, dev, cx),
        "instrument.wavetable" => wavetable::render(ws, dev, cx),
        "instrument.sampler" => sampler::render(ws, dev, d, cx),
        "instrument.drum" => drum::render(ws, dev, d, cx),
        "instrument.vault" => vault::render(ws, dev, cx),
        _ => generic::render(ws, dev, cx),
    }
}

/// A caption above a group of controls.
pub fn caption(text: impl Into<gpui::SharedString>) -> gpui::Div {
    label(text, TEXT_XS, p().faint).font_weight(gpui::FontWeight::SEMIBOLD)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::bridge::StubCatalog;

    /// A [`Dev`] of `plugin` with default parameters plus `plain` overrides.
    pub fn dev(plugin: &str, plain: &[(u32, f32)]) -> Dev {
        let catalog = StubCatalog::default();
        let set = Rc::new(ParamSet::new(catalog.device(plugin).unwrap_or_else(|| panic!("no stock device {plugin}")).params.clone()));
        let values = plain.iter().map(|&(id, v)| (id, set.get(id).unwrap_or_else(|| panic!("{plugin} has no param {id}")).to_normalized(v))).collect();
        Dev {
            id: DeviceId(1),
            enabled: true,
            big: false,
            set,
            values: Rc::new(values),
            meter: Rc::new(Vec::new()),
            hist: Rc::default(),
            selected: 0,
            sample_rate: 48_000.,
            bpm: 120.,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{Bridges, StubCatalog};

    #[test]
    fn a_drag_moves_its_axes_and_reports_changes() {
        let mut d = FxDrag {
            device: DeviceId(1),
            x: Some(Axis { param: 2, value: 0.5, per_px: 1. / 400. }),
            y: Some(Axis { param: 3, value: 0.5, per_px: -1. / 200. }),
            alt: Some(Axis { param: 4, value: 0.2, per_px: -1. / 100. }),
            last: (100., 100.),
        };
        // 40 px right, 20 px up.
        let c = d.advance((140., 80.), false, false);
        assert_eq!(c.len(), 2);
        assert!((c[0].1 - 0.6).abs() < 1e-6 && c[0].0 == 2);
        assert!((c[1].1 - 0.6).abs() < 1e-6 && c[1].0 == 3);
        // Shift moves a fifth as far.
        let c = d.advance((180., 80.), true, false);
        assert_eq!(c, vec![(2, 0.62)]);
        // Alt sends the vertical move to the third axis only.
        let c = d.advance((180., 70.), false, true);
        assert_eq!(c.len(), 1);
        assert!((c[0].1 - 0.3).abs() < 1e-6 && c[0].0 == 4);
        // Values stop at the ends and then report nothing.
        d.advance((5000., 70.), false, false);
        assert_eq!(d.x.unwrap().value, 1.);
        assert!(d.advance((6000., 70.), false, false).is_empty());
    }

    #[test]
    fn a_handle_drag_is_one_undo_step() {
        let mut s = crate::state::AppState::new(crate::demo::project());
        let eq = s.project.tracks.iter().flat_map(|t| &t.devices).find(|d| d.name == "EQ").unwrap().id;
        let before = s.history.steps().count();
        s.seal();
        for i in 1..=5 {
            let v = i as f32 * 0.1;
            s.edit_coalesced(&format!("fx-{}", eq.0), "Change parameter", vec![
                Command::SetParam { device: eq, param: 2, value: Some(v) },
                Command::SetParam { device: eq, param: 3, value: Some(1. - v) },
            ]);
        }
        s.seal();
        assert_eq!(s.history.steps().count(), before + 1);
        assert_eq!(s.project.device(eq).unwrap().params[&2], 0.5);
        s.undo();
        assert_ne!(s.project.device(eq).unwrap().params.get(&2), Some(&0.5));
    }

    #[test]
    fn every_stock_plugin_has_an_editor_and_meters_feed_histories() {
        let catalog = StubCatalog::default();
        for d in catalog.stock_devices().iter().filter(|d| !d.id.starts_with("rack.")) {
            assert!(has_editor(d.id), "{} falls back to the knob grid", d.id);
        }
        assert!(!has_editor("vendor.unknown"));
        // The stub engine's compressor meter fills the three history traces.
        let mut b = Bridges::stubs();
        let project = crate::demo::project();
        b.engine.update_project(&project);
        b.engine.play();
        let comp = project.tracks.iter().flat_map(|t| &t.devices).find(|d| matches!(&d.kind, aitrack_model::DeviceKind::Stock { plugin_id } if plugin_id == "fx.compressor")).unwrap();
        let mut ui = UiState::default();
        let dev = ui.dev(b.engine.as_mut(), b.plugins.as_ref(), comp, "fx.compressor", 128.);
        assert_eq!(dev.meter.len(), 3);
        assert!(dev.info(aitrack_stock::fx::compressor::p::THRESHOLD).is_some());
        std::thread::sleep(Duration::from_millis(40));
        ui.poll(b.engine.as_mut(), comp.id, "fx.compressor");
        assert!(!dev.hist.borrow().traces[0].is_empty());
        assert!(ui.wants_frames());
        ui.prune(|_| false);
        assert!(ui.live.is_empty());
    }
}
