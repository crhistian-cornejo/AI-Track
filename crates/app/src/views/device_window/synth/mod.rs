//! The wavetable synth's own window: the full editor.
//!
//! Tabs: **Synth** (three oscillators with stacked wavetable views, sub and
//! noise, both filters with draggable response nodes, macros, the
//! envelope / LFO editors and the routings), **Matrix** (all 32 slots),
//! **FX** (the effect chain and the voice settings) and **Wavetable** (draw,
//! harmonics, formula and morph tools over the selected oscillator's table).
//!
//! Every change is a model command: knobs and plots send `SetParam`; user
//! tables and drawn LFO curves are device state, written with one
//! `SetDeviceState` (plus `SetParam` where a switch has to follow) per
//! gesture, so each gesture is one undo step and the Device View, this
//! window and the engine all follow the project. Table work (formulas,
//! morphs, harmonic analysis and resynthesis, WAV import) runs on the
//! background executor and lands as one edit when done.

pub mod geom;
mod editor;
pub mod knob;
mod mods;
mod osc;
mod panels;

use crate::theme::*;
use crate::views::device_ui::wavetable::{self as wt, SynthData, TableKey};
use crate::views::device_ui::{Dev, PopKind};
use crate::widgets::{self, label, menu_header, menu_item};
use crate::workspace::Workspace;
use aitrack_model::{Command, Device, DeviceId};
use aitrack_stock::instrument::wavetable::modulation::{self, CurvePoint, LfoCurve, ModSlot, SOURCES};
use aitrack_stock::instrument::wavetable::tablegen::{FACTORY_NAMES, WaveFrames};
use aitrack_stock::instrument::wavetable::{TABLES, USER_WAVE, VIRTUAL_DESTS, dest_name, p, params, src};
use gpui::{
    AnyElement, Context, DispatchPhase, Entity, FontWeight, Hsla, InteractiveElement, IntoElement, MouseMoveEvent, MouseUpEvent, ParentElement,
    StatefulInteractiveElement, Styled, canvas, div, prelude::FluentBuilder, px,
};
use std::rc::Rc;

/// The colour of a modulation source (rings, handles, matrix rows).
pub fn source_color(s: u8) -> Hsla {
    let pal = p();
    match s {
        src::ENV1 => pal.accent,
        src::ENV2 => pal.accent2,
        src::ENV3 => series(7),
        src::LFO1..=src::LFO4 => series(3 + (s - src::LFO1) as usize),
        src::MACRO1..=src::MACRO4 => pal.solo,
        _ => pal.play,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Synth,
    Matrix,
    Fx,
    Table,
}

/// Which envelope or LFO the modulation section edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModTab {
    Env(u32),
    Lfo(u32),
}

/// The wavetable editor's tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Draw,
    Harmonics,
    Formula,
    Morph,
}

/// A pointer gesture of the synth window that is not a plain parameter drag.
#[derive(Clone, Debug)]
pub enum SynthDrag {
    /// Moving a breakpoint (`bend: false`) or bending the segment after it.
    Lfo { lfo: u32, index: usize, bend: bool, points: Vec<CurvePoint>, region: crate::views::device_ui::Region, last_fy: f32, snap: bool },
    /// Painting a frame with the pen.
    Draw { osc: u32, frame: usize, base: Rc<WaveFrames>, cycle: Vec<f32>, last: (f32, f32), region: crate::views::device_ui::Region },
    /// Setting harmonic amplitudes.
    Bars { osc: u32, frame: usize, base: Rc<WaveFrames>, levels: Vec<(f32, f32)>, region: crate::views::device_ui::Region },
}

/// The harmonic analysis of one frame (computed off the UI thread).
#[derive(Clone, Debug)]
pub struct Harmonics {
    pub table: usize,
    pub frame: usize,
    pub values: Vec<(f32, f32)>,
}

/// View state of one synth window.
pub struct SynthView {
    pub tab: Tab,
    pub mods: ModTab,
    pub edit_osc: u32,
    pub frame: usize,
    pub tool: Tool,
    pub formula: Option<Entity<widgets::TextInput>>,
    pub formula_frames: usize,
    pub drag: Option<SynthDrag>,
    /// What a background job is doing (the editor shows it).
    pub busy: Option<String>,
    pub error: Option<String>,
    pub harmonics: Option<Harmonics>,
    harmonics_pending: Option<(usize, usize)>,
}

impl Default for SynthView {
    fn default() -> Self {
        Self {
            tab: Tab::Synth,
            mods: ModTab::Env(0),
            edit_osc: 0,
            frame: 0,
            tool: Tool::Draw,
            formula: None,
            formula_frames: 64,
            drag: None,
            busy: None,
            error: None,
            harmonics: None,
            harmonics_pending: None,
        }
    }
}

pub fn view(ws: &mut Workspace, device: DeviceId) -> &mut SynthView {
    ws.fx.synth.entry(device).or_default()
}

/// Popovers of the synth editors.
#[derive(Clone, Debug, PartialEq)]
pub enum SynthPop {
    /// Wavetable of oscillator `osc`.
    Wave { device: DeviceId, osc: u32 },
    /// Source (or `via`) of matrix slot `slot`.
    Source { device: DeviceId, slot: usize, via: bool },
    /// Destination of matrix slot `slot`.
    Dest { device: DeviceId, slot: usize },
    /// Shape presets of LFO `lfo`.
    LfoPresets { device: DeviceId, lfo: u32 },
}

// ───────────────────────────── Commits ─────────────────────────────

fn plain_to_norm(ws: &mut Workspace, device: DeviceId, id: u32, plain: f32) -> Option<f32> {
    ws.fx.param_info(ws.b.plugins.as_ref(), &ws.s.project, device, id).map(|i| i.to_normalized(plain))
}

/// The state edit that stores `data` on `device`.
fn state_command(ws: &Workspace, device: DeviceId, data: &SynthData) -> Option<Command> {
    let d = ws.s.project.device(device)?;
    Some(Command::SetDeviceState { device, state: Some(wt::write_state(&d.params, data)), preset: d.preset.clone() })
}

/// Give oscillator `osc` the table `frames` (and switch it to its user
/// table) as one undo step.
pub fn commit_table(ws: &mut Workspace, device: DeviceId, osc: u32, frames: WaveFrames, label: &str, cx: &mut Context<Workspace>) {
    let project = &ws.s.project;
    let mut data = (*ws.fx.wave_previews.data(project, device)).clone();
    data.tables[osc as usize] = Some(Rc::new(frames));
    let Some(state) = state_command(ws, device, &data) else { return };
    let mut cmds = vec![state];
    if let Some(v) = plain_to_norm(ws, device, p::osc(osc, p::WAVE), USER_WAVE as f32) {
        cmds.push(Command::SetParam { device, param: p::osc(osc, p::WAVE), value: Some(v) });
    }
    ws.s.seal();
    ws.s.edit(label, cmds);
    ws.s.seal();
    let frames_now = ws.fx.wave_previews.data(&ws.s.project, device).tables[osc as usize].as_ref().map_or(1, |t| t.frames.len());
    let v = view(ws, device);
    v.frame = v.frame.min(frames_now.saturating_sub(1));
    v.harmonics = None;
    cx.notify();
}

/// Store LFO `lfo`'s drawn curve (and turn its Drawn switch on). Gliding
/// edits (a drag) coalesce into one undo step until the next seal.
pub fn commit_curve(ws: &mut Workspace, device: DeviceId, lfo: u32, curve: LfoCurve, gliding: bool) {
    let project = &ws.s.project;
    let mut data = (*ws.fx.wave_previews.data(project, device)).clone();
    data.curves[lfo as usize] = Some(curve);
    let Some(state) = state_command(ws, device, &data) else { return };
    let mut cmds = vec![state];
    let drawn = p::lfo(lfo, p::LFO_DRAWN);
    let on = ws.s.project.device(device).and_then(|d| d.params.get(&drawn).copied()).unwrap_or(0.) >= 0.5;
    if !on {
        cmds.push(Command::SetParam { device, param: drawn, value: Some(1.) });
    }
    if gliding {
        ws.s.edit_coalesced(&format!("lfo-curve-{}-{lfo}", device.0), "Draw LFO", cmds);
    } else {
        ws.s.seal();
        ws.s.edit("Edit LFO shape", cmds);
        ws.s.seal();
    }
}

/// Run table work off the UI thread, then commit its result to `osc`.
pub fn run_job(
    ws: &mut Workspace,
    device: DeviceId,
    osc: u32,
    what: &str,
    job: impl FnOnce() -> Result<WaveFrames, String> + Send + 'static,
    cx: &mut Context<Workspace>,
) {
    let label = what.to_owned();
    {
        let v = view(ws, device);
        v.busy = Some(format!("{what}…"));
        v.error = None;
    }
    let task = cx.background_executor().spawn(async move { job() });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |ws, cx| {
            view(ws, device).busy = None;
            match result {
                Ok(frames) => commit_table(ws, device, osc, frames, &label, cx),
                Err(e) => view(ws, device).error = Some(e),
            }
            cx.notify();
        });
    })
    .detach();
}

/// Ask for a WAV file and load it into oscillator `osc`.
pub fn pick_wav(_ws: &mut Workspace, device: DeviceId, osc: u32, cx: &mut Context<Workspace>) {
    let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import wavetable (WAV)".into()) });
    cx.spawn(async move |this, cx| {
        if let Ok(Ok(Some(paths))) = rx.await
            && let Some(path) = paths.into_iter().next()
        {
            let _ = this.update(cx, |ws, cx| import_wav(ws, device, osc, path, cx));
        }
    })
    .detach();
}

/// Load the WAV at `path` into oscillator `osc` (decoded off the UI thread).
pub fn import_wav(ws: &mut Workspace, device: DeviceId, osc: u32, path: std::path::PathBuf, cx: &mut Context<Workspace>) {
    let is_wav = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    if !is_wav {
        ws.toast("Wavetables are imported from WAV files", cx);
        return;
    }
    let name = path.file_stem().map_or("Imported".to_owned(), |s| s.to_string_lossy().into_owned());
    run_job(ws, device, osc, &format!("Import {name}"), move || aitrack_stock::instrument::wavetable::wav::import_file(&path).map_err(|e| e.to_string()), cx);
}

// ───────────────────────────── Popovers ─────────────────────────────

pub fn pop_menu(ws: &mut Workspace, pop: &SynthPop, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    match *pop {
        SynthPop::Wave { device, osc } => Some(wave_menu(ws, device, osc, cx)),
        SynthPop::Source { device, slot, via } => Some(source_menu(ws, device, slot, via, cx)),
        SynthPop::Dest { device, slot } => Some(dest_menu(ws, device, slot, cx)),
        SynthPop::LfoPresets { device, lfo } => Some(lfo_presets_menu(device, lfo, cx)),
    }
}

fn close_pop(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    ws.fx.pop = None;
    cx.notify();
}

fn wave_menu(ws: &mut Workspace, device: DeviceId, osc: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let dev_plain = |ws: &mut Workspace, id: u32| {
        let info = ws.fx.param_info(ws.b.plugins.as_ref(), &ws.s.project, device, id);
        info.map_or(0., |i| i.to_plain(crate::views::device_ui::current(ws, device, &i)))
    };
    let wave = dev_plain(ws, p::osc(osc, p::WAVE)).round() as usize;
    let table = dev_plain(ws, p::osc(osc, p::TABLE)).round() as usize;
    let has_user = ws.fx.wave_previews.data(&ws.s.project, device).tables[osc as usize].is_some();
    let mut list = div().id("wave-list").max_h(px(460.)).overflow_y_scroll().flex().flex_col();
    list = list.child(menu_header("ORIGINAL"));
    for (i, name) in TABLES.iter().enumerate() {
        list = list.child(menu_item(("wave-legacy", i), None, *name, None, wave == 0 && table == i).on_click(wave_click(device, vec![(p::osc(osc, p::WAVE), 0.), (p::osc(osc, p::TABLE), i as f32)], cx)));
    }
    list = list.child(menu_header("FACTORY"));
    for (i, name) in FACTORY_NAMES.iter().enumerate() {
        list = list.child(menu_item(("wave-factory", i), None, *name, None, wave == i + 1).on_click(wave_click(device, vec![(p::osc(osc, p::WAVE), (i + 1) as f32)], cx)));
    }
    list = list.child(menu_header("USER"));
    if has_user {
        list = list.child(menu_item("wave-user", Some(crate::icons::Icon::Waveform), "User table", None, wave == USER_WAVE).on_click(wave_click(device, vec![(p::osc(osc, p::WAVE), USER_WAVE as f32)], cx)));
    }
    list = list
        .child(menu_item("wave-import", Some(crate::icons::Icon::FolderOpen), "Import WAV…", None, false).on_click(cx.listener(move |this, _, _, cx| {
            close_pop(this, cx);
            pick_wav(this, device, osc, cx);
        })))
        .child(menu_item("wave-edit", Some(crate::icons::Icon::Pencil), "Edit in the wavetable editor", None, false).on_click(cx.listener(move |this, _, _, cx| {
            let v = view(this, device);
            v.tab = Tab::Table;
            v.edit_osc = osc;
            close_pop(this, cx);
        })));
    widgets::menu_card("wave-menu", 220.).child(menu_header(format!("OSC {} WAVETABLE", osc + 1))).child(list).into_any_element()
}

fn wave_click(device: DeviceId, plain: Vec<(u32, f32)>, cx: &mut Context<Workspace>) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    cx.listener(move |this: &mut Workspace, _: &gpui::ClickEvent, _, cx| {
        knob::set_plain(this, "Change wavetable", device, &plain);
        close_pop(this, cx);
    })
}

fn source_menu(ws: &mut Workspace, device: DeviceId, slot: usize, via: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let slots = knob::read_slots(ws, device);
    let now = slots.get(slot).map_or(0, |m| if via { m.via } else { m.source });
    let field = if via { p::M_VIA } else { p::M_SRC };
    let mut list = div().id("src-list").max_h(px(460.)).overflow_y_scroll().flex().flex_col();
    for (i, name) in SOURCES.iter().enumerate() {
        let label_text = if i == 0 { if via { "None (full depth)" } else { "None" } } else { name };
        list = list.child(
            menu_item(("mod-src-item", i), None, label_text, None, now as usize == i).on_click(cx.listener(move |this, _, _, cx| {
                knob::set_plain(this, "Change modulation", device, &[(p::slot_x(slot as u32, field), i as f32)]);
                close_pop(this, cx);
            })),
        );
    }
    widgets::menu_card("src-menu", 196.).child(menu_header(if via { "DEPTH CONTROLLED BY" } else { "SOURCE" })).child(list).into_any_element()
}

/// Destinations a matrix slot can take: the virtual ones, then every
/// modulatable parameter.
pub fn destinations() -> Vec<(u32, &'static str)> {
    let mut v: Vec<(u32, &'static str)> = VIRTUAL_DESTS.to_vec();
    v.extend(params().into_iter().filter(modulation::is_modulatable).map(|i| (i.id, i.name)));
    v
}

fn dest_menu(ws: &mut Workspace, device: DeviceId, slot: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let slots = knob::read_slots(ws, device);
    let now = slots.get(slot).map_or(0, |m| m.dest);
    let mut list = div().id("dst-list").max_h(px(480.)).overflow_y_scroll().flex().flex_col();
    for (k, (id, name)) in destinations().into_iter().enumerate() {
        if k == VIRTUAL_DESTS.len() {
            list = list.child(menu_header("PARAMETERS"));
        }
        list = list.child(menu_item(("mod-dst-item", k), None, name, None, now == id).on_click(cx.listener(move |this, _, _, cx| {
            knob::set_plain(this, "Change modulation", device, &[(p::slot_x(slot as u32, p::M_DST), id as f32)]);
            close_pop(this, cx);
        })));
    }
    widgets::menu_card("dst-menu", 232.).child(menu_header("DESTINATION")).child(list).into_any_element()
}

fn lfo_presets_menu(device: DeviceId, lfo: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let mut list = div().flex().flex_col();
    for (i, (name, curve)) in LfoCurve::presets().into_iter().enumerate() {
        list = list.child(menu_item(("lfo-preset", i), None, name, None, false).on_click(cx.listener(move |this, _, _, cx| {
            commit_curve(this, device, lfo, curve, false);
            close_pop(this, cx);
        })));
    }
    widgets::menu_card("lfo-presets", 160.).child(menu_header("SHAPE")).child(list).into_any_element()
}

/// A chip that opens a synth popover.
pub fn pop_chip(id: impl Into<gpui::ElementId>, text: String, width: Option<f32>, color: Option<Hsla>, pop: SynthPop, cx: &mut Context<Workspace>) -> gpui::Stateful<gpui::Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .when_some(width, |d, w| d.w(px(w)))
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(content(0.06))
        .hover(|s| s.bg(content(0.12)))
        .when_some(color, |d, c| d.child(div().size(px(6.)).rounded_full().bg(c)))
        .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label(crate::paint::fit(&text, width.unwrap_or(160.) - 28., TEXT_XS), TEXT_XS, pal.text).font_weight(FontWeight::MEDIUM)))
        .child(crate::icons::icon(crate::icons::Icon::ChevronDown, 10., pal.faint))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            let at = e.position();
            this.fx.open_pop(PopKind::Synth(pop.clone()), gpui::point(at.x - px(8.), at.y + px(12.)));
            cx.notify();
        }))
}

// ───────────────────────────── Gestures ─────────────────────────────

fn gesture_move(ws: &mut Workspace, device: DeviceId, e: &MouseMoveEvent, cx: &mut Context<Workspace>) {
    let Some(mut drag) = view(ws, device).drag.take() else { return };
    let (mx, my) = (f32::from(e.position.x), f32::from(e.position.y));
    match &mut drag {
        SynthDrag::Lfo { lfo, index, bend, points, region, last_fy, snap } => {
            let r = crate::views::device_ui::kit::Rect::of(region.get());
            let (fx, fy) = ((mx - r.x) / r.w.max(1.), (my - r.y) / r.h.max(1.));
            let on = *snap != e.modifiers.shift;
            *points = if *bend {
                let v = geom::lfo_bend(points, *index, fy - *last_fy);
                *last_fy = fy;
                v
            } else {
                let (x, y) = geom::plot_to_lfo(fx, fy);
                geom::lfo_move(points, *index, geom::snap(x, 16., on), geom::snap(y, 8., on))
            };
            commit_curve(ws, device, *lfo, LfoCurve::new(points), true);
        }
        SynthDrag::Draw { cycle, last, region, .. } => {
            let r = crate::views::device_ui::kit::Rect::of(region.get());
            let now = (((mx - r.x) / r.w.max(1.)).clamp(0., 1.), geom::wave_level((my - r.y) / r.h.max(1.)));
            geom::paint_stroke(cycle, &[*last, now]);
            *last = now;
        }
        SynthDrag::Bars { levels, region, .. } => {
            let r = crate::views::device_ui::kit::Rect::of(region.get());
            let (fx, fy) = ((mx - r.x) / r.w.max(1.), (my - r.y) / r.h.max(1.));
            let i = geom::bar_at(fx, geom::HARMONICS);
            if let Some(l) = levels.get_mut(i) {
                l.0 = geom::bar_level(fy);
            }
        }
    }
    view(ws, device).drag = Some(drag);
    cx.notify();
}

fn gesture_end(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    let Some(drag) = view(ws, device).drag.take() else { return };
    match drag {
        SynthDrag::Lfo { .. } => ws.s.seal(),
        SynthDrag::Draw { osc, frame, base, cycle, .. } => {
            let mut w = (*base).clone();
            w.set_frame(frame, &cycle);
            commit_table(ws, device, osc, w, "Draw wavetable", cx);
        }
        SynthDrag::Bars { osc, frame, base, levels, .. } => {
            let mut w = (*base).clone();
            run_job(
                ws,
                device,
                osc,
                "Set harmonics",
                move || {
                    w.set_harmonics(frame, &levels);
                    Ok(w)
                },
                cx,
            );
        }
    }
    cx.notify();
}

/// Window-wide mouse listeners while a synth gesture runs.
pub fn capture(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    ws.fx.synth.get(&device)?.drag.as_ref()?;
    let weak = cx.entity().downgrade();
    Some(
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let w = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, |this, cx| gesture_move(this, device, e, cx));
                    }
                });
                let w = weak.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, |this, cx| gesture_end(this, device, cx));
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element(),
    )
}

/// Whether the window needs frames (a gesture or a job running).
pub fn wants_frames(ws: &Workspace, device: DeviceId) -> bool {
    ws.fx.synth.get(&device).is_some_and(|v| v.drag.is_some() || v.busy.is_some())
}

// ───────────────────────────── Layout ─────────────────────────────

/// A titled panel of the synth window.
pub fn panel(title: &str, accent: Option<Hsla>) -> gpui::Div {
    let pal = p();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .p(px(SP3))
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .when_some(accent, |d, c| d.child(div().w(px(3.)).h(px(12.)).rounded(px(2.)).bg(c)))
                .child(label(title.to_owned(), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD)),
        )
}

/// A panel's title row with extra controls on the right.
pub fn panel_with(title: &str, accent: Option<Hsla>, right: impl IntoElement) -> gpui::Div {
    let pal = p();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .p(px(SP3))
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .when_some(accent, |d, c| d.child(div().w(px(3.)).h(px(12.)).rounded(px(2.)).bg(c)))
                .child(label(title.to_owned(), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(right),
        )
}

fn tab_bar(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> gpui::Div {
    let device = dev.id;
    let tab = view(ws, device).tab;
    let tabs = [(Tab::Synth, "Synth"), (Tab::Matrix, "Matrix"), (Tab::Fx, "FX"), (Tab::Table, "Wavetable")];
    let mut row = div().flex().items_center().gap(px(SP1));
    for (k, (t, name)) in tabs.into_iter().enumerate() {
        let on = tab == t;
        let pal = p();
        let count = if t == Tab::Matrix { ModSlot::read_all(|id| dev.plain(id)).iter().filter(|m| m.is_active()).count() } else { 0 };
        row = row.child(
            div()
                .id(("synth-tab", k))
                .h(px(28.))
                .px(px(SP3))
                .flex()
                .items_center()
                .gap(px(SP1))
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.18) } else { gpui::transparent_black() })
                .hover(|s| s.bg(if on { accent_alpha(0.24) } else { content(0.06) }))
                .child(label(name, TEXT_MD, if on { pal.accent } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
                .when(count > 0, |d| d.child(crate::widgets::mono(format!("{count}"), TEXT_XS, pal.faint)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    view(this, device).tab = t;
                    cx.notify();
                })),
        );
    }
    row
}

/// The whole synth editor for the device window.
pub fn render(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let slots = Rc::new(ModSlot::read_all(|id| dev.plain(id)));
    let tab = view(ws, dev.id).tab;
    let top = div()
        .flex_none()
        .h(px(56.))
        .px(px(SP4))
        .flex()
        .items_center()
        .gap(px(SP4))
        .border_b_1()
        .border_color(border())
        .child(tab_bar(ws, dev, cx))
        .child(div().flex_1())
        .child(mods::macros(ws, dev, &slots, cx))
        .child(panels::master(ws, dev, &slots, cx));
    let body: AnyElement = match tab {
        Tab::Synth => div()
            .flex()
            .flex_col()
            .gap(px(SP3))
            .child(div().flex().gap(px(SP3)).child(osc::osc(ws, dev, &slots, 0, cx)).child(osc::osc(ws, dev, &slots, 1, cx)).child(osc::osc(ws, dev, &slots, 2, cx)).child(osc::sub_noise(ws, dev, &slots, cx)))
            .child(div().flex().gap(px(SP3)).child(panels::filter(ws, dev, &slots, 0, cx)).child(panels::filter(ws, dev, &slots, 1, cx)).child(mods::section(ws, dev, &slots, cx)))
            .into_any_element(),
        Tab::Matrix => mods::matrix(ws, dev, &slots, 32, cx).into_any_element(),
        Tab::Fx => panels::fx(ws, dev, &slots, cx).into_any_element(),
        Tab::Table => editor::render(ws, dev, d, cx),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(pal.base)
        .child(top)
        .child(div().id("synth-body").flex_1().min_h(px(0.)).overflow_y_scroll().p(px(SP3)).child(body))
        .into_any_element()
}

/// Make sure the analysis of the selected frame is (being) computed.
pub fn ensure_harmonics(ws: &mut Workspace, device: DeviceId, base: &Rc<WaveFrames>, frame: usize, cx: &mut Context<Workspace>) {
    let table = Rc::as_ptr(base) as usize;
    let v = view(ws, device);
    if v.harmonics.as_ref().is_some_and(|h| h.table == table && h.frame == frame) || v.harmonics_pending == Some((table, frame)) {
        return;
    }
    v.harmonics_pending = Some((table, frame));
    let cycle = base.frames.get(frame).cloned().unwrap_or_default();
    let task = cx.background_executor().spawn(async move { WaveFrames::single("", cycle).harmonics(0, geom::HARMONICS) });
    cx.spawn(async move |this, cx| {
        let values = task.await;
        let _ = this.update(cx, |ws, cx| {
            let v = view(ws, device);
            if v.harmonics_pending == Some((table, frame)) {
                v.harmonics_pending = None;
                v.harmonics = Some(Harmonics { table, frame, values });
                cx.notify();
            }
        });
    })
    .detach();
}

/// The table oscillator `osc` plays, by key and frames (for the editor).
pub fn table_of(ws: &mut Workspace, dev: &Dev, osc: u32, cx: &mut Context<Workspace>) -> (TableKey, Option<Rc<WaveFrames>>) {
    let key = TableKey::of(dev, osc);
    (key, wt::frames(ws, key, cx))
}

/// Short name of a matrix destination.
pub fn dest_label(id: u32) -> &'static str {
    dest_name(id)
}
