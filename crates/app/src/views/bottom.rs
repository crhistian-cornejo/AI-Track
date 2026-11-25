//! The Detail View (Live): the resizable, collapsible panel under the main
//! view. It shows one of two things, switched with the tabs or Shift+Tab:
//!
//! * **Device View**: the selected track's device chain (`devices.rs`).
//! * **Clip View**: the selected clip, with its properties on the left in
//!   Live's boxes (Clip, Sample for audio, Notes for MIDI, Envelopes) and
//!   its editor on the right: the sample editor for audio, the piano roll
//!   or the step sequencer for notes. Double-clicking a clip opens it here.
//!
//! The panel's top edge is the resize grip (`Workspace::begin_bottom_resize`).
//! Every edit made here is a model `Command`.

use crate::icons::{Icon, icon};
use crate::motion;
use crate::state::{BottomTab, Snap, Tool};
use crate::theme::*;
use crate::views::sample_editor::geom;
use crate::widgets::{self, label, mono};
use crate::workspace::{Drag, KnobTarget, MenuKind, NumTarget, Workspace, plain_text};
use aitrack_model::{
    AutomationTarget, Clip, ClipId, Command, FollowAction, LaunchMode, LoopRegion, Note, Quantize, Source, Tick, TimeSignature,
};
use gpui::{
    AnimationExt, AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Window, div, point, prelude::FluentBuilder, px,
};
use std::rc::Rc;

/// Width of the Clip View's properties column.
/// What Cmd/Ctrl does on the Transpose and Detune knobs.
const ALL_CLIPS_HINT: &str = if cfg!(target_os = "macos") { "⌘-drag: every clip of this sample" } else { "Ctrl-drag: every clip of this sample" };

const PROPS_W: f32 = 224.;
/// Height of a property row.
const ROW_H: f32 = 24.;

/// What the Clip View edits and with which editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipEditor {
    Sample,
    Piano,
    Steps,
    /// A Step Sequencer clip: rows of steps with their own length, rate,
    /// direction and swing (`step_clip.rs`).
    StepClip,
}

/// The editor for the current tab and clip: audio clips always get the
/// sample editor; the Steps tab shows the drum rack's step grid; a clip that
/// holds a step pattern (`pattern`) gets the step sequencer clip editor
/// instead of the piano roll.
pub fn clip_editor(tab: BottomTab, audio: bool, pattern: bool) -> ClipEditor {
    match tab {
        BottomTab::Steps => ClipEditor::Steps,
        _ if audio => ClipEditor::Sample,
        _ if pattern => ClipEditor::StepClip,
        _ => ClipEditor::Piano,
    }
}

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    // The mixer is its own section (as in Live); the Detail View holds
    // what belongs to the selected track or clip.
    if ws.s.bottom_tab == BottomTab::Mixer {
        ws.s.bottom_tab = BottomTab::Devices;
    }
    let tab = ws.s.bottom_tab;
    let clip_view = tab != BottomTab::Devices;
    let audio = ws.s.selected_clip.and_then(|c| ws.s.project.clips.get(&c)).is_some_and(|c| matches!(ws.s.project.sources.get(&c.source), Some(Source::Audio(_))));
    let pattern = ws.s.selected_clip.is_some_and(|c| ws.s.clip_pattern(c).is_some());
    let editor = clip_editor(tab, audio, pattern);
    let step_clip = ws.s.step_clip();
    // The clip the Clip View shows.
    let clip_id = match editor {
        ClipEditor::Steps => step_clip.map(|(_, c)| c),
        _ => ws.s.selected_clip,
    }
    .filter(|c| ws.s.project.clips.contains_key(c));

    let weak = cx.entity().downgrade();
    let on_view: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |this, cx| {
            let to_clip = i == 1;
            let now_clip = this.s.bottom_tab != BottomTab::Devices;
            if to_clip == now_clip && this.bottom_open {
                // Clicking the open view again folds the panel (Live's triangle).
                this.bottom_open = false;
                this.release_audition(cx);
                cx.notify();
            } else if to_clip {
                let tab = if this.s.bottom_tab == BottomTab::Steps { BottomTab::Steps } else { BottomTab::PianoRoll };
                this.set_tab(tab, cx);
            } else {
                this.set_tab(BottomTab::Devices, cx);
            }
        });
    });
    let stacked = ws.bottom_stacked;
    let tabs = widgets::segmented("detail-view", &[(Some(Icon::Device), "Device"), (Some(Icon::Piano), "Clip")], usize::from(clip_view), 96., on_view);

    // What the panel is showing, next to the switch.
    let context = if clip_view || stacked {
        clip_id.and_then(|c| ws.s.project.clips.get(&c)).map(|c| (c.name.clone(), c.color))
    } else {
        ws.s.selected_track.and_then(|t| ws.s.project.track(t)).map(|t| (t.name.clone(), t.color))
    };
    let mut bar = div()
        .flex_none()
        .h(px(m().dock_bar_h))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .child(tabs)
        .child(
            widgets::icon_chip("detail-stack", Icon::Layers, stacked, pal.accent)
                .tooltip(widgets::tip(
                    if stacked { "Unstack Clip & Devices (show one view at a time)" } else { "Stack Clip & Devices together (Live 12 split view)" },
                    Some("Shift+Tab switches focus"),
                ))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_bottom_stacked(cx))),
        )
        .when_some(context, |d, (name, color)| {
            d.child(div().w(px(SP1)))
                .child(div().size(px(8.)).rounded_full().bg(track_color(color)))
                .child(label(crate::paint::fit(&name, 144., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
        });
    // MIDI clips choose between the piano roll and the step sequencer.
    if (clip_view || stacked) && !audio && step_clip.is_some() && editor != ClipEditor::StepClip {
        let weak = cx.entity().downgrade();
        let on_editor: widgets::OnSelect = Rc::new(move |i, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_tab(if i == 0 { BottomTab::PianoRoll } else { BottomTab::Steps }, cx));
        });
        bar = bar.child(widgets::divider_v(16.)).child(widgets::segmented(
            "clip-editor",
            &[(Some(Icon::Piano), "Notes"), (Some(Icon::Steps), "Steps")],
            usize::from(editor == ClipEditor::Steps),
            72.,
            on_editor,
        ));
    }
    bar = bar.child(div().flex_1());
    // The note tools act on a clip: without one the bar stays clear.
    if (clip_view || stacked) && editor == ClipEditor::Piano && ws.bottom_open && clip_id.is_some() {
        bar = bar.child(piano_tools(ws, cx));
    }
    bar = bar.child(
        widgets::icon_button("collapse-bottom", if ws.bottom_open { Icon::ChevronDown } else { Icon::ChevronUp }, false)
            .tooltip(widgets::tip(if ws.bottom_open { "Hide the Detail View" } else { "Show the Detail View" }, Some("Ctrl+Alt+L")))
            .on_click(cx.listener(|this, _, _, cx| {
                this.bottom_open = !this.bottom_open;
                if !this.bottom_open {
                    this.release_audition(cx);
                }
                cx.notify();
            })),
    );

    let body = if ws.bottom_open {
        let render_clip = |ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
            let editor_el = match editor {
                ClipEditor::Sample => crate::views::sample_editor::render(ws, window, cx),
                ClipEditor::Piano => crate::views::piano_roll::render(ws, window, cx),
                ClipEditor::Steps => crate::views::steps::render(ws, window, cx),
                ClipEditor::StepClip => crate::views::step_clip::render(ws, window, cx),
            };
            let props = clip_id.and_then(|id| clip_props(ws, id, editor, cx));
            div()
                .size_full()
                .flex()
                .children(props)
                .child(div().flex_1().min_w(px(0.)).h_full().child(editor_el))
                .into_any_element()
        };
        let content = if stacked {
            let clip_el = render_clip(ws, window, cx);
            let dev_el = crate::views::devices::render(ws, window, cx);
            let total_h = (ws.bottom_h_eff() - m().dock_bar_h).max(120.);
            let split = crate::workspace::split_fitted(ws.bottom_split, total_h);
            let dev_focused = ws.s.ed.area == crate::editing::Area::Devices;
            let divider = div()
                .id("bottom-split-resize")
                .flex_none()
                .h(px(crate::workspace::STACK_DIVIDER_H))
                .w_full()
                .cursor_row_resize()
                .flex()
                .items_center()
                .justify_center()
                .bg(content(0.03))
                .hover(|s| s.bg(accent_alpha(0.18)))
                .tooltip(widgets::tip("Drag to resize Clip / Devices split · Double-click to reset", None))
                .child(div().w(px(36.)).h(px(2.)).rounded_full().bg(if dev_focused { pal.accent } else { border_strong() }))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if e.click_count >= 2 {
                        this.bottom_split = 0.5;
                        cx.notify();
                    } else {
                        this.begin_bottom_split_resize(f32::from(e.position.y), total_h, cx);
                    }
                }));
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_grow(split)
                        .flex_basis(px(0.))
                        .min_h(px(crate::workspace::STACK_CLIP_MIN_H))
                        .w_full()
                        .overflow_hidden()
                        .border_l_2()
                        .border_color(if !dev_focused { accent_alpha(0.6) } else { gpui::transparent_black() })
                        .child(clip_el),
                )
                .child(divider)
                .child(
                    div()
                        .flex_grow(1. - split)
                        .flex_basis(px(0.))
                        .min_h(px(crate::workspace::DEVICES_MIN_H))
                        .w_full()
                        .overflow_hidden()
                        .border_l_2()
                        .border_color(if dev_focused { accent_alpha(0.6) } else { gpui::transparent_black() })
                        .child(dev_el),
                )
                .into_any_element()
        } else if !clip_view {
            crate::views::devices::render(ws, window, cx)
        } else {
            render_clip(ws, window, cx)
        };
        Some(motion::rise_in(gpui::ElementId::Name(format!("tab-{}", ws.tab_switch).into()), div().size_full().child(content), if ws.tab_switch == 0 { 0. } else { 8. }))
    } else {
        None
    };

    let resizing = matches!(ws.drag, Some(Drag::BottomResize { .. } | Drag::BottomSplit { .. }));
    let target = if ws.bottom_open { ws.bottom_h_eff() } else { m().dock_bar_h };
    let handle = div()
        .id("bottom-resize")
        .absolute()
        .top(px(-SP1))
        .left_0()
        .right_0()
        .h(px(SP2))
        .cursor_row_resize()
        .when(ws.bottom_open, |d| {
            d.on_mouse_down(MouseButton::Left, cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if e.click_count >= 2 {
                    // Double-click the edge: back to the default height.
                    this.bottom_h = crate::workspace::bottom_h_fitted(m().dock_h, this.window_h, this.bottom_min_h());
                    cx.notify();
                } else {
                    this.begin_bottom_resize(f32::from(e.position.y), cx);
                }
            }))
        });
    let panel = div()
        .relative()
        .flex_none()
        .w_full()
        .flex()
        .flex_col()
        .bg(pane_bg(crate::appearance::Pane::Bottom))
        .rounded(px(r_lg()))
        .border_1()
        .border_color(border())
        .child(bar)
        .child(div().flex_1().min_h(px(0.)).overflow_hidden().border_t_1().border_color(border()).children(body))
        .child(handle);
    if resizing {
        panel.h(px(target)).into_any_element()
    } else {
        panel.with_spring("bottom-height", motion::spring(motion::PANEL_SPRING, target), |d, h| d.h(px(h))).into_any_element()
    }
}

// ───────────────────────────── Clip properties ─────────────────────────────

/// One of Live's property boxes: a caption bar and its rows.
pub(super) fn section(title: &'static str, ic: Icon) -> Div {
    let pal = p();
    div().flex_none().flex().flex_col().border_b_1().border_color(border()).pb(px(SP2)).child(
        div()
            .h(px(ROW_H))
            .px(px(SP2))
            .gap(px(SP2))
            .flex()
            .items_center()
            .child(icon(ic, 12., pal.faint))
            .child(label(title, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD)),
    )
}

/// A caption and its value on one row.
pub(super) fn row(caption: &'static str, value: impl IntoElement) -> Div {
    div()
        .h(px(ROW_H))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .child(div().w(px(64.)).flex_none().child(label(caption, TEXT_SM, p().muted)))
        .child(div().flex_1().min_w(px(0.)).flex().items_center().gap(px(SP1)).child(value))
}

/// A small text button of the properties column.
pub(super) fn small_button(id: impl Into<gpui::ElementId>, text: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(20.))
        .min_w(px(28.))
        .px(px(SP1 + 2.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .bg(content(0.06))
        .border_1()
        .border_color(border())
        .cursor_pointer()
        .hover(|s| s.bg(content(0.12)))
        .active(|s| s.bg(pressed()))
        .when(!enabled, |d| d.opacity(0.4))
        .child(mono(text, TEXT_XS, pal.text))
}

/// A value that opens a menu: its text and a chevron, in a well.
fn dropdown(id: impl Into<gpui::ElementId>, text: &'static str, tip: &'static str) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .h(px(20.))
        .w(px(88.))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(r_sm()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .cursor_pointer()
        .hover(|s| s.border_color(border_strong()))
        .tooltip(widgets::tip(tip, None))
        .child(mono(text, TEXT_SM, pal.text))
        .child(icon(Icon::ChevronDown, 10., pal.faint))
}

/// Every launch mode and quantisation, in the order the panel lists them.
pub(crate) const LAUNCH_MODES: [LaunchMode; 4] = [LaunchMode::Trigger, LaunchMode::Gate, LaunchMode::Toggle, LaunchMode::Repeat];
pub(crate) const QUANTIZES: [Quantize; 7] = [Quantize::None, Quantize::Sixteenth, Quantize::Eighth, Quantize::Beat, Quantize::Bar, Quantize::TwoBars, Quantize::FourBars];

/// A small on/off button of the properties column, lit while on.
fn toggle_button(id: impl Into<gpui::ElementId>, text: &'static str, on: bool) -> Stateful<Div> {
    let b = small_button(id, text, true);
    if on {
        b.bg(accent_alpha(0.22)).border_color(accent())
    } else {
        b
    }
}

/// A value box that opens for typing on a click: Enter applies the number,
/// Escape or a click elsewhere drops it.
fn num_box(ws: &Workspace, id: impl Into<gpui::ElementId>, target: NumTarget, shown: String, prefill: String, cx: &mut Context<Workspace>) -> Stateful<Div> {
    let pal = p();
    let editing = ws.num_edit.as_ref().filter(|e| e.target == target).map(|e| e.text.clone());
    let body = match editing {
        Some(text) => div().flex().items_center().child(mono(text, TEXT_SM, pal.accent)).child(div().w(px(1.)).h(px(12.)).ml(px(1.)).bg(pal.accent)),
        None => div().child(mono(shown, TEXT_SM, pal.text)),
    };
    div()
        .id(id)
        .h(px(20.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(pal.well)
        .border_1()
        .border_color(if ws.num_edit.as_ref().is_some_and(|e| e.target == target) { accent() } else { border() })
        .cursor_text()
        .tooltip(widgets::tip("Click to type a value", None))
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
            if !this.num_edit.as_ref().is_some_and(|e| e.target == target) {
                this.focus.focus(window, cx);
                this.begin_num_edit(target, prefill.clone(), cx);
            }
        }))
        .child(body)
}

/// A change to a sample's Seg. BPM (the tempo it was recorded at).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SegBpm {
    Half,
    Double,
    Step(f64),
    /// A typed value (`commit_num_edit` in `workspace.rs`).
    Set(f64),
}

impl SegBpm {
    /// The new Seg. BPM, kept inside the tempo range.
    fn apply(self, bpm: f64) -> f64 {
        let next = match self {
            SegBpm::Half => bpm / 2.,
            SegBpm::Double => bpm * 2.,
            SegBpm::Step(d) => bpm + d,
            SegBpm::Set(v) => v,
        };
        if next.is_finite() { next.clamp(20., 999.) } else { bpm }
    }
}

/// Change the Seg. BPM of the sample `clip` plays: a `SetWarp` in one undo
/// step, and the clip stretches to the project tempo from the new value.
/// Acts on the selected clips as every warp edit does. `false` when nothing
/// changed (no Seg. BPM yet, or the value is already there).
pub(crate) fn edit_seg_bpm(s: &mut crate::state::AppState, clip: ClipId, edit: SegBpm) -> bool {
    s.seal();
    let changed = s.set_clip_warp("seg-bpm", clip, |w| {
        if let Some(b) = w.original_bpm {
            // The markers count their beats at the new tempo, so the whole file follows it.
            w.set_original_bpm(edit.apply(b));
        }
    });
    s.seal();
    changed
}

/// Launch mode of a clip: one `UpdateClip`, one undo step.
pub(crate) fn set_launch_mode(s: &mut crate::state::AppState, clip: ClipId, mode: LaunchMode) -> bool {
    edit_clip(s, clip, "Launch mode", |c| c.launch.mode = mode)
}

/// Launch quantisation of a clip: one `UpdateClip`, one undo step.
pub(crate) fn set_launch_quantize(s: &mut crate::state::AppState, clip: ClipId, quantize: Quantize) -> bool {
    edit_clip(s, clip, "Launch quantisation", |c| c.launch.quantize = quantize)
}

fn edit_clip(s: &mut crate::state::AppState, id: ClipId, label: &str, f: impl FnOnce(&mut Clip)) -> bool {
    let Some(mut clip) = s.project.clips.get(&id).cloned() else { return false };
    let before = clip.clone();
    f(&mut clip);
    if clip == before {
        return false;
    }
    s.seal();
    let ok = s.edit(label, vec![Command::UpdateClip { id, clip: Box::new(clip) }]);
    s.seal();
    ok
}

/// A typed position or length: `bars.beats.sixteenths` as the panel writes
/// it ("2.1.0"), where missing parts are 0 (so "4" is four bars). Commas
/// work as separators too. `None` for anything else.
pub(crate) fn parse_position(text: &str, sig: TimeSignature) -> Option<Tick> {
    let mut parts = text.trim().split(['.', ',']);
    let mut next = |max: Tick| -> Option<Tick> {
        match parts.next() {
            None => Some(0),
            Some(t) => t.trim().parse::<Tick>().ok().filter(|v| (0..=max).contains(v)),
        }
    };
    let bars = next(100_000)?;
    let beats = next(63)?;
    let sixteenths = next(63)?;
    if parts.next().is_some() {
        return None;
    }
    let beat = (aitrack_model::PPQ * 4 / Tick::from(sig.denominator.max(1))).max(1);
    Some(bars * sig.bar_ticks().max(1) + beats * beat + sixteenths * (aitrack_model::PPQ / 4))
}

/// The clip fields a value box types into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipField {
    Start,
    Length,
}

/// Apply typed `text` to a clip field (Enter in its value box): `SetClip`
/// style `UpdateClip`, one undo step. `false` when the text is not a
/// position or nothing changes.
pub(crate) fn commit_clip_field(s: &mut crate::state::AppState, clip: ClipId, field: ClipField, text: &str) -> bool {
    let Some(ticks) = parse_position(text, s.project.tempo.signature_at(0)) else { return false };
    match field {
        ClipField::Start => set_clip_start(s, clip, ticks),
        ClipField::Length => {
            s.seal();
            let ok = s.resize_clip(clip, ticks.max(aitrack_model::ops::MIN_CLIP_LEN));
            s.seal();
            ok
        }
    }
}

/// Move where an audio clip starts inside its file (Live's Start marker),
/// keeping the clip's place on the timeline. It stays inside the file and,
/// unless the clip loops, shrinks if the tail would run past the end.
/// Notes clips keep their notes from tick 0, so they have no Start to move.
pub(crate) fn set_clip_start(s: &mut crate::state::AppState, clip: ClipId, start: Tick) -> bool {
    let Some(file) = aitrack_model::ops::source_len(&s.project, clip) else { return false };
    let min = aitrack_model::ops::MIN_CLIP_LEN;
    let start = start.clamp(0, (file - min).max(0));
    let Some(old) = s.project.clips.get(&clip) else { return false };
    let looping = old.looping.is_some();
    let rate = aitrack_model::ops::clip_rate(&s.project, clip);
    edit_clip(s, clip, "Clip start", |c| {
        c.offset = start;
        if !looping {
            let room = (((file - start) as f64) / rate).floor().max(min as f64) as Tick;
            c.length = c.length.min(room);
        }
    })
}

/// A value shown in a well (what Live draws as a number box).
fn value_box(text: impl Into<SharedString>) -> Div {
    let pal = p();
    div().h(px(20.)).px(px(SP2)).flex().items_center().rounded(px(r_sm())).bg(pal.well).border_1().border_color(border()).child(mono(text, TEXT_SM, pal.text))
}

pub fn launch_mode_name(m: LaunchMode) -> &'static str {
    match m {
        LaunchMode::Trigger => "Trigger",
        LaunchMode::Gate => "Gate",
        LaunchMode::Toggle => "Toggle",
        LaunchMode::Repeat => "Repeat",
    }
}

#[cfg(test)]
pub fn next_launch_mode(m: LaunchMode) -> LaunchMode {
    match m {
        LaunchMode::Trigger => LaunchMode::Gate,
        LaunchMode::Gate => LaunchMode::Toggle,
        LaunchMode::Toggle => LaunchMode::Repeat,
        LaunchMode::Repeat => LaunchMode::Trigger,
    }
}

pub fn quantize_name(q: Quantize) -> &'static str {
    match q {
        Quantize::None => "None",
        Quantize::Sixteenth => "1/16",
        Quantize::Eighth => "1/8",
        Quantize::Beat => "1/4",
        Quantize::Bar => "1 Bar",
        Quantize::TwoBars => "2 Bars",
        Quantize::FourBars => "4 Bars",
    }
}

pub fn next_quantize(q: Quantize) -> Quantize {
    match q {
        Quantize::None => Quantize::Sixteenth,
        Quantize::Sixteenth => Quantize::Eighth,
        Quantize::Eighth => Quantize::Beat,
        Quantize::Beat => Quantize::Bar,
        Quantize::Bar => Quantize::TwoBars,
        Quantize::TwoBars => Quantize::FourBars,
        Quantize::FourBars => Quantize::None,
    }
}

fn target_name(t: &AutomationTarget) -> String {
    match t {
        AutomationTarget::Volume => "Volume".into(),
        AutomationTarget::Pan => "Pan".into(),
        AutomationTarget::Mute => "Mute".into(),
        AutomationTarget::Send { index } => format!("Send {}", (b'A' + (*index as u8).min(25)) as char),
        AutomationTarget::DeviceParam { param, .. } => format!("Parameter {param}"),
        AutomationTarget::DeviceEnabled { .. } => "Device On".into(),
        AutomationTarget::Macro { index, .. } => format!("Macro {}", index + 1),
        AutomationTarget::Tempo => "Tempo".into(),
        AutomationTarget::Transpose => "Transpose".into(),
    }
}

/// The note name of a MIDI key (C4 = 60).
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[key as usize % 12], key as i32 / 12 - 1)
}

/// `notes` moved by `semitones`, kept inside the MIDI range; `None` when no
/// note would move (all already at the edge).
pub fn transposed(notes: &[Note], semitones: i32) -> Option<Vec<Note>> {
    let moved: Vec<Note> = notes
        .iter()
        .filter_map(|n| {
            let key = (n.key as i32 + semitones).clamp(0, 127) as u8;
            (key != n.key).then(|| Note { key, ..n.clone() })
        })
        .collect();
    (!moved.is_empty()).then_some(moved)
}

/// Human label for a clip's Follow Action.
pub fn follow_action_name(action: Option<FollowAction>) -> &'static str {
    match action {
        None => "None",
        Some(FollowAction::Next { .. }) => "Play Next",
        Some(FollowAction::Previous { .. }) => "Play Prev",
        Some(FollowAction::First { .. }) => "Play First",
        Some(FollowAction::Random { .. }) => "Play Any",
        Some(FollowAction::Stop { .. }) => "Stop",
    }
}

pub fn follow_action_bars(action: Option<FollowAction>) -> u32 {
    match action {
        None => 1,
        Some(
            FollowAction::Next { after_bars }
            | FollowAction::Previous { after_bars }
            | FollowAction::First { after_bars }
            | FollowAction::Random { after_bars }
            | FollowAction::Stop { after_bars },
        ) => after_bars.max(1),
    }
}

pub fn set_follow_action(s: &mut crate::state::AppState, id: ClipId, action: Option<FollowAction>) {
    let Some(mut clip) = s.project.clips.get(&id).cloned() else { return };
    if clip.launch.follow == action {
        return;
    }
    clip.launch.follow = action;
    s.seal();
    s.edit("Follow action", vec![Command::UpdateClip { id, clip: Box::new(clip) }]);
    s.seal();
}

pub fn with_follow_bars(action: FollowAction, after_bars: u32) -> FollowAction {
    let after_bars = after_bars.clamp(1, 64);
    match action {
        FollowAction::Next { .. } => FollowAction::Next { after_bars },
        FollowAction::Previous { .. } => FollowAction::Previous { after_bars },
        FollowAction::First { .. } => FollowAction::First { after_bars },
        FollowAction::Random { .. } => FollowAction::Random { after_bars },
        FollowAction::Stop { .. } => FollowAction::Stop { after_bars },
    }
}

/// Replace a clip with an edited copy (one undo step).
fn update_clip(ws: &mut Workspace, id: ClipId, label: &str, f: impl FnOnce(&mut Clip)) {
    let Some(mut clip) = ws.s.project.clips.get(&id).cloned() else { return };
    f(&mut clip);
    ws.s.seal();
    ws.s.edit(label, vec![Command::UpdateClip { id, clip: Box::new(clip) }]);
    ws.s.seal();
}

/// The notes a tool works on: the selected ones, or the whole clip.
fn note_targets(ws: &Workspace, id: ClipId) -> Option<(aitrack_model::SourceId, Vec<Note>)> {
    let (source, n) = ws.s.clip_notes(id)?;
    let sel: Vec<Note> = n.notes.iter().filter(|n| ws.s.selected_notes.contains(&n.id)).cloned().collect();
    Some((source, if sel.is_empty() { n.notes.clone() } else { sel }))
}

fn transpose(ws: &mut Workspace, id: ClipId, semitones: i32) {
    let Some((source, notes)) = note_targets(ws, id) else { return };
    if let Some(notes) = transposed(&notes, semitones) {
        ws.s.seal();
        ws.s.edit("Transpose", vec![Command::UpdateNotes { source, notes }]);
        ws.s.seal();
    }
}

fn nudge_velocity(ws: &mut Workspace, id: ClipId, delta: f32) {
    let Some((source, notes)) = note_targets(ws, id) else { return };
    if notes.is_empty() {
        return;
    }
    let notes = notes.into_iter().map(|n| Note { velocity: (n.velocity + delta).clamp(0.01, 1.), ..n }).collect();
    ws.s.seal();
    ws.s.edit("Change velocity", vec![Command::UpdateNotes { source, notes }]);
    ws.s.seal();
}

fn resize(ws: &mut Workspace, id: ClipId, length: Tick) {
    ws.s.seal();
    ws.s.resize_clip(id, length.max(aitrack_model::ops::MIN_CLIP_LEN));
    ws.s.seal();
}

fn format_len(ticks: Tick, sig: TimeSignature) -> String {
    geom::format_length(ticks, sig)
}

/// The properties column of the Clip View.
fn clip_props(ws: &mut Workspace, id: ClipId, editor: ClipEditor, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    let clip = ws.s.project.clips.get(&id)?.clone();
    let sig = ws.s.project.tempo.signature_at(0);
    let bpm = ws.s.project.tempo.initial_bpm();
    let n = id.0 as usize;

    // ── Clip ──
    let muted = clip.muted;
    let looping = clip.looping.is_some();
    let length = clip.length;
    let mode = clip.launch.mode;
    let quant = clip.launch.quantize;
    let audio = editor == ClipEditor::Sample;
    let clip_box = section("CLIP", Icon::Timeline)
        .child(
            div()
                .h(px(ROW_H + SP1))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .child(
                    widgets::icon_chip(("clip-on", n), Icon::Power, !muted, pal.accent)
                        .tooltip(widgets::tip("Clip on/off (a deactivated clip does not play)", Some("0")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            update_clip(this, id, if muted { "Activate clip" } else { "Deactivate clip" }, |c| c.muted = !muted);
                            cx.notify();
                        })),
                )
                .child(div().w(px(3.)).h(px(16.)).rounded(px(2.)).bg(track_color(clip.color)))
                .child(div().flex_1().min_w(px(0.)).child(label(crate::paint::fit(&clip.name, PROPS_W - 64., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::SEMIBOLD))),
        )
        .child(row(
            "Signature",
            value_box(format!("{}/{}", sig.numerator, sig.denominator))
                .id(("clip-signature", n))
                .cursor_pointer()
                .hover(|s| s.border_color(border_strong()))
                .tooltip(widgets::tip("Project time signature: click to change", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::TimeSignature, point(at.x - px(12.), at.y - px(160.)), cx);
                })),
        ))
        .child(row("Start", num_box(ws, ("clip-start", n), NumTarget::ClipStart { clip: id }, format_len(clip.offset, sig), format_len(clip.offset, sig), cx)))
        .child(row(
            "Length",
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(num_box(ws, ("clip-length", n), NumTarget::ClipLength { clip: id }, format_len(length, sig), format_len(length, sig), cx))
                .child(small_button(("clip-half", n), ":2", length >= 2 * aitrack_model::ops::MIN_CLIP_LEN).tooltip(widgets::tip("Half as long", None)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        resize(this, id, length / 2);
                        cx.notify();
                    },
                )))
                .child(small_button(("clip-double", n), "×2", true).tooltip(widgets::tip("Twice as long", None)).on_click(cx.listener(move |this, _, _, cx| {
                    resize(this, id, length * 2);
                    cx.notify();
                }))),
        ))
        .child(row(
            "Loop",
            widgets::switch(("clip-loop", n), looping).tooltip(widgets::tip("Loop the clip: then dragging its right edge out repeats the loop. Off, it stops at the end of the sample", None)).on_click(cx.listener(move |this, _, _, cx| {
                if audio {
                    crate::views::sample_editor::edit::toggle_loop(&mut this.s, id);
                } else {
                    update_clip(this, id, if looping { "Loop off" } else { "Loop on" }, |c| {
                        c.looping = if looping { None } else { Some(LoopRegion { start: c.offset, end: c.offset + c.length }) };
                    });
                }
                cx.notify();
            })),
        ))
        .when_some(clip.looping, |d, lp| d.child(row("Loop len", value_box(format_len(lp.end - lp.start, sig)))))
        .child(row(
            "Launch",
            dropdown(("clip-mode", n), launch_mode_name(mode), "Launch mode: click to choose").on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.open_menu(MenuKind::ClipLaunch(id), point(at.x - px(12.), at.y - px(160.)), cx);
            })),
        ))
        .child(row(
            "Quantize",
            dropdown(("clip-quant", n), quantize_name(quant), "Launch quantisation: when the clip starts after you launch it").on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.open_menu(MenuKind::ClipQuantize(id), point(at.x - px(12.), at.y - px(220.)), cx);
            })),
        ))
        .child(row(
            "Follow",
            dropdown(("clip-follow", n), follow_action_name(clip.launch.follow), "Follow Action: what Session View plays after this clip runs").on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.open_menu(MenuKind::ClipFollow(id), point(at.x - px(12.), at.y - px(200.)), cx);
            })),
        ))
        .when_some(clip.launch.follow, |d, follow| {
            let bars = follow_action_bars(Some(follow));
            d.child(row(
                "After",
                div()
                    .flex()
                    .items_center()
                    .gap(px(SP1))
                    .child(small_button(("follow-bars-down", n), "−", bars > 1).on_click(cx.listener(move |this, _, _, cx| {
                        set_follow_action(&mut this.s, id, Some(with_follow_bars(follow, bars.saturating_sub(1))));
                        cx.notify();
                    })))
                    .child(value_box(if bars == 1 { "1 bar".into() } else { format!("{bars} bars") }))
                    .child(small_button(("follow-bars-up", n), "+", bars < 64).on_click(cx.listener(move |this, _, _, cx| {
                        set_follow_action(&mut this.s, id, Some(with_follow_bars(follow, bars + 1)));
                        cx.notify();
                    }))),
            ))
        });

    let mut column = div().flex().flex_col().child(clip_box);

    // ── Sample (audio) / Notes (MIDI) ──
    match ws.s.project.sources.get(&clip.source).cloned() {
        Some(Source::Audio(src)) => {
            let file = src.path.file_name().map_or_else(|| src.path.display().to_string(), |f| f.to_string_lossy().into_owned());
            let seconds = if src.sample_rate > 0 { src.frames as f64 / f64::from(src.sample_rate) } else { 0. };
            let channels = match src.channels {
                1 => "Mono",
                2 => "Stereo",
                _ => "Multi",
            };
            let gain = clip.gain_db;
            let warp = &src.warp;
            let (warp_on, warp_mode, reversed) = (warp.enabled, warp.mode, warp.reverse);
            let markers = warp.anchors().len();
            let mode = warp_mode.label();
            let stretch = (aitrack_model::ops::stretch_factor(warp, ws.s.project.tempo.initial_bpm()) * 100.).round() / 100.;
            let sample_box = section("SAMPLE", Icon::Sample)
                .child(div().px(px(SP2)).h(px(ROW_H)).flex().items_center().child(label(crate::paint::fit(&file, PROPS_W - 16., TEXT_SM), TEXT_SM, pal.text)))
                .child(row("Format", mono(format!("{:.1} kHz {channels}", f64::from(src.sample_rate) / 1000.), TEXT_SM, pal.muted)))
                .child(row("Duration", mono(geom::format_seconds(seconds), TEXT_SM, pal.muted)))
                .child(row(
                    "Warp",
                    div()
                        .flex()
                        .gap(px(SP1))
                        .child(
                            toggle_button(("warp-on", n), if warp.enabled { "On" } else { "Off" }, warp.enabled)
                                .tooltip(widgets::tip("Warp: follow project tempo and unlock pitch-preserving modes (Beats, Tones, Texture, Complex, Complex Pro)", None))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let bpm = this.s.project.tempo.initial_bpm();
                                    this.s.seal();
                                    let on = !warp_on;
                                    this.s.set_clip_warp("warp-toggle", id, |w| {
                                        w.enabled = on;
                                        // Without a known tempo the sample is taken to be at the project's, so it sounds the same.
                                        if w.enabled && w.original_bpm.is_none() {
                                            w.original_bpm = Some(bpm);
                                        }
                                    });
                                    this.s.seal();
                                    cx.notify();
                                })),
                        )
                        .child(
                            small_button(("warp-mode", n), mode, true)
                                .tooltip(widgets::tip("Warp algorithm (click to cycle): Beats · Tones · Texture · Complex · Complex Pro · Re-Pitch", None))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let bpm = this.s.project.tempo.initial_bpm();
                                    let next = warp_mode.next();
                                    this.s.seal();
                                    this.s.set_clip_warp("warp-mode", id, |w| {
                                        w.enabled = true;
                                        if w.original_bpm.is_none() {
                                            w.original_bpm = Some(bpm);
                                        }
                                        w.mode = next;
                                    });
                                    this.s.seal();
                                    cx.notify();
                                })),
                        ),
                ))
                .child(row(
                    "Beats",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .child(
                            small_button(("auto-warp", n), "Auto-warp", true)
                                .tooltip(widgets::tip("Listen for the beats: sets the tempo of the sample, puts warp markers where its tempo moves, and starts the clip on the bar", None))
                                .on_click(cx.listener(move |this, _, _, cx| this.auto_warp_clip(id, cx))),
                        )
                        .when(markers > 0, |d| d.child(label(if markers == 1 { "1 marker".to_owned() } else { format!("{markers} markers") }, TEXT_XS, pal.faint))),
                ))
                .when_some(warp.original_bpm, |d, b| {
                    d.child(row(
                        "Seg. BPM",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SP1))
                            .child(small_button(("seg-bpm-down", n), "−", true).tooltip(widgets::tip("Seg. BPM −1: the tempo the sample was recorded at", None)).on_click(cx.listener(move |this, _, _, cx| {
                                edit_seg_bpm(&mut this.s, id, SegBpm::Step(-1.));
                                cx.notify();
                            })))
                            .child(num_box(ws, ("seg-bpm", n), NumTarget::SegBpm { clip: id }, format!("{b:.2}"), plain_text(b), cx))
                            .child(small_button(("seg-bpm-up", n), "+", true).tooltip(widgets::tip("Seg. BPM +1", None)).on_click(cx.listener(move |this, _, _, cx| {
                                edit_seg_bpm(&mut this.s, id, SegBpm::Step(1.));
                                cx.notify();
                            }))),
                    ))
                    .child(row(
                        "",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SP1))
                            .child(small_button(("seg-bpm-half", n), "÷2", true).tooltip(widgets::tip("Halve Seg. BPM: the clip plays twice as fast", None)).on_click(cx.listener(move |this, _, _, cx| {
                                edit_seg_bpm(&mut this.s, id, SegBpm::Half);
                                cx.notify();
                            })))
                            .child(small_button(("seg-bpm-double", n), "×2", true).tooltip(widgets::tip("Double Seg. BPM: the clip plays at half speed", None)).on_click(cx.listener(move |this, _, _, cx| {
                                edit_seg_bpm(&mut this.s, id, SegBpm::Double);
                                cx.notify();
                            }))),
                    ))
                })
                // A clip stretched with Cmd/Ctrl + an edge.
                .when(stretch != 1., |d| {
                    d.child(row("Stretch", value_box(format!("×{stretch:.2}")).id(("stretch", n)).tooltip(widgets::tip("Stretched in time: Cmd/Ctrl + drag a clip edge in the arrangement", None))))
                })
                .child(row(
                    "Transpose",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(crate::views::devices::knob_control(("transpose", n), (warp.transpose + 48.) / 96., true, true, KnobTarget::Transpose { clip: id }, cx).tooltip(widgets::tip("Transpose in semitones: drag, Shift for fine, double-click for 0. Acts on every selected audio clip", Some(ALL_CLIPS_HINT))))
                        .child(num_box(ws, ("transpose-value", n), NumTarget::Transpose { clip: id }, format!("{:+.0} st", warp.transpose), plain_text(f64::from(warp.transpose)), cx))
                ))
                .child(row(
                    "Detune",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(crate::views::devices::knob_control(("detune", n), (warp.detune + 50.) / 100., true, true, KnobTarget::Detune { clip: id }, cx).tooltip(widgets::tip("Detune in cents: drag, Shift for fine, double-click for 0. Acts on every selected audio clip", Some(ALL_CLIPS_HINT))))
                        .child(num_box(ws, ("detune-value", n), NumTarget::Detune { clip: id }, format!("{:+.0} ct", warp.detune), plain_text(f64::from(warp.detune)), cx)),
                ))
                .child(row(
                    "Reverse",
                    toggle_button(("reverse", n), "Rev", warp.reverse).tooltip(widgets::tip("Play the sample backwards", Some("R"))).on_click(cx.listener(move |this, _, _, cx| {
                        this.s.seal();
                        this.s.set_clip_warp("reverse", id, |w| w.reverse = !reversed);
                        this.s.seal();
                        cx.notify();
                    })),
                ))
                .when_some(src.key, |d, k| d.child(row("Key", widgets::badge(k.display(), pal.accent))))
                .child(row(
                    "Gain",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(small_button(("gain-down", n), "−", true).on_click(cx.listener(move |this, _, _, cx| {
                            crate::views::sample_editor::edit::apply_now(&mut this.s, id, crate::views::sample_editor::edit::Gesture::Gain(gain - 1.));
                            cx.notify();
                        })))
                        .child(
                            value_box(format!("{gain:+.1} dB")).id(("gain-value", n)).tooltip(widgets::tip("Clip gain; double-click for 0 dB", None)).on_click(cx.listener(
                                move |this, e: &gpui::ClickEvent, _, cx| {
                                    if e.click_count() >= 2 {
                                        crate::views::sample_editor::edit::apply_now(&mut this.s, id, crate::views::sample_editor::edit::Gesture::Gain(0.));
                                        cx.notify();
                                    }
                                },
                            )),
                        )
                        .child(small_button(("gain-up", n), "+", true).on_click(cx.listener(move |this, _, _, cx| {
                            crate::views::sample_editor::edit::apply_now(&mut this.s, id, crate::views::sample_editor::edit::Gesture::Gain(gain + 1.));
                            cx.notify();
                        }))),
                ));
            column = column.child(sample_box);
        }
        Some(Source::Notes(src)) if src.pattern.is_some() => {
            let rows = src.pattern.as_ref().map_or(0, |p| p.rows.len());
            let hits = src.notes.len();
            let steps_box = section("STEPS", Icon::Steps)
                .child(row("Rows", mono(format!("{rows}"), TEXT_SM, pal.text)))
                .child(row("Hits", mono(format!("{hits}"), TEXT_SM, pal.text)))
                .child(
                    div().px(px(SP2)).pt(px(SP1)).flex().gap(px(SP1)).child(
                        small_button(("steps-to-notes", n), "Convert to Notes", true)
                            .tooltip(widgets::tip("Turn the steps into plain MIDI notes", None))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.s.convert_to_notes(id);
                                cx.notify();
                            })),
                    ),
                );
            column = column.child(steps_box);
        }
        Some(Source::Notes(_)) => {
            let notes: Vec<Note> = ws.s.clip_notes(id).map(|(_, n)| n.notes.clone()).unwrap_or_default();
            let selected = notes.iter().filter(|n| ws.s.selected_notes.contains(&n.id)).count();
            let range = match (notes.iter().map(|n| n.key).min(), notes.iter().map(|n| n.key).max()) {
                (Some(lo), Some(hi)) if lo == hi => key_name(lo),
                (Some(lo), Some(hi)) => format!("{}–{}", key_name(lo), key_name(hi)),
                _ => "—".into(),
            };
            let count = if selected > 0 { format!("{selected} of {}", notes.len()) } else { format!("{}", notes.len()) };
            let scale = ws.s.clip_scale(Some(id)).map_or_else(|| "None".to_owned(), |s| s.display());
            let has_notes = !notes.is_empty();
            let mut transpose_row = div().flex().gap(px(SP1));
            for (k, (text, st)) in [("−12", -12), ("−1", -1), ("+1", 1), ("+12", 12)].into_iter().enumerate() {
                transpose_row = transpose_row.child(small_button(("transpose", n << 3 | k), text, has_notes).on_click(cx.listener(move |this, _, _, cx| {
                    transpose(this, id, st);
                    cx.notify();
                })));
            }
            let notes_box = section("NOTES", Icon::Piano)
                .child(row("Notes", mono(count, TEXT_SM, pal.text)))
                .child(row("Range", mono(range, TEXT_SM, pal.muted)))
                .child(row(
                    "Scale",
                    div()
                        .id(("clip-scale-pick", n))
                        .cursor_pointer()
                        .tooltip(widgets::tip("Set clip key / scale (overrides track & project)", None))
                        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                            let at = e.position();
                            this.open_menu(MenuKind::ClipScale(id), point(at.x - px(12.), at.y - px(280.)), cx);
                        }))
                        .child(widgets::badge(scale, pal.accent)),
                ))
                .child(row("Transpose", transpose_row))
                .child(row(
                    "Velocity",
                    div()
                        .flex()
                        .gap(px(SP1))
                        .child(small_button(("vel-down", n), "−10%", has_notes).on_click(cx.listener(move |this, _, _, cx| {
                            nudge_velocity(this, id, -0.1);
                            cx.notify();
                        })))
                        .child(small_button(("vel-up", n), "+10%", has_notes).on_click(cx.listener(move |this, _, _, cx| {
                            nudge_velocity(this, id, 0.1);
                            cx.notify();
                        }))),
                ))
                .child(row(
                    "Chance",
                    div()
                        .flex()
                        .gap(px(SP1))
                        .child(small_button(("chance-down", n), "−10%", has_notes).tooltip(widgets::tip("Lower note playback probability by 10%", None)).on_click(cx.listener(move |this, _, _, cx| {
                            this.s.nudge_probability(id, -0.1);
                            cx.notify();
                        })))
                        .child(small_button(("chance-up", n), "+10%", has_notes).tooltip(widgets::tip("Raise note playback probability by 10%", None)).on_click(cx.listener(move |this, _, _, cx| {
                            this.s.nudge_probability(id, 0.1);
                            cx.notify();
                        }))),
                ))
                .child(
                    div()
                        .px(px(SP2))
                        .pt(px(SP1))
                        .flex()
                        .gap(px(SP1))
                        .child(small_button(("notes-quantize", n), "Quantize", has_notes).tooltip(widgets::tip("Quantize to the grid", Some("Ctrl+U"))).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.s.selected_clip = Some(id);
                                this.quantize_clip(cx);
                            },
                        )))
                        .child(small_button(("notes-steps", n), "To Steps", has_notes).tooltip(widgets::tip("Turn the notes into a step sequencer clip (snapped to sixteenth steps)", None)).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.s.convert_to_steps(id);
                                cx.notify();
                            },
                        )))
                        .child(small_button(("notes-scale", n), "Fit to Scale", has_notes).tooltip(widgets::tip("Move the notes into the key", None)).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.s.selected_clip = Some(id);
                                this.scale_snap_clip(cx);
                            },
                        ))),
                );
            column = column.child(notes_box);
        }
        None => {}
    }

    // ── Envelopes ──
    let mut env_box = section("ENVELOPES", Icon::Automation);
    if audio {
        for (fade_in, f) in [(true, clip.fade_in), (false, clip.fade_out)] {
            if f.length > 0 {
                let secs = f.length as f64 * 60. / (bpm * aitrack_model::PPQ as f64);
                env_box = env_box.child(row(if fade_in { "Fade in" } else { "Fade out" }, mono(geom::format_seconds(secs), TEXT_SM, pal.muted)));
            }
        }
    }
    if clip.envelopes.is_empty() {
        env_box = env_box.child(div().px(px(SP2)).py(px(SP1)).min_w(px(0.)).child(label(
            if audio { "Double-click the waveform to draw the volume envelope." } else { "No clip envelopes yet." },
            TEXT_SM,
            pal.faint,
        ).whitespace_normal()));
    }
    for (i, lane) in clip.envelopes.iter().enumerate() {
        let points = lane.points.len();
        env_box = env_box.child(
            div()
                .h(px(ROW_H))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .child(div().size(px(6.)).rounded_full().bg(if lane.enabled { pal.accent } else { pal.faint }))
                .child(label(target_name(&lane.target), TEXT_SM, pal.text).flex_1())
                .child(mono(format!("{points} pt"), TEXT_XS, pal.faint))
                .child(
                    widgets::sized_icon_button(("env-clear", n << 4 | i.min(15)), Icon::X, false, 20., 10.)
                        .tooltip(widgets::tip("Clear this envelope", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            update_clip(this, id, "Clear envelope", |c| {
                                if i < c.envelopes.len() {
                                    c.envelopes.remove(i);
                                }
                            });
                            cx.notify();
                        })),
                ),
        );
    }
    column = column.child(env_box);

    Some(
        div()
            .id("clip-props")
            .flex_none()
            .w(px(PROPS_W))
            .h_full()
            .border_r_1()
            .border_color(border())
            .bg(pal.surface)
            .overflow_y_scroll()
            .child(column)
            .into_any_element(),
    )
}

// ───────────────────────────── Piano tools ─────────────────────────────

/// A named control of the note editor's tool bar: icon, name (when there is
/// room for it) and a chevron when it opens a menu. Lit while `on`.
fn tool_button(id: &'static str, ic: Icon, text: Option<SharedString>, on: bool, menu: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_H))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if on { accent_alpha(0.18) } else { content(0.0) })
        .hover(move |s| s.bg(if on { accent_alpha(0.26) } else { hover() }))
        .child(icon(ic, 14., if on { pal.accent } else { pal.muted }))
        .when_some(text, |d, t| d.child(label(t, TEXT_SM, if on { pal.text } else { pal.muted })))
        .when(menu, |d| d.child(icon(Icon::ChevronDown, 10., pal.faint)))
}

/// Window width from which the tool bar spells its controls out.
const TOOL_NAMES_W: f32 = 1280.;

/// Whether the tool bar has room for names next to its icons.
fn tool_names(window_w: f32) -> bool {
    window_w >= TOOL_NAMES_W
}

/// The note editor's tools, in the order of the work: hear (audition, the
/// computer keyboard) · pointer (select or draw) · grid (snap, quantize) ·
/// key (fold) · everything else in the Tools menu · zoom.
fn piano_tools(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let names = tool_names(ws.window_w);
    let name = |t: &'static str| names.then(|| SharedString::from(t));
    let weak = cx.entity().downgrade();
    let on_tool: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |this, cx| {
            this.s.tool = if i == 0 { Tool::Select } else { Tool::Draw };
            cx.notify();
        });
    });
    let scale = ws.s.clip_scale(ws.s.selected_clip);
    let has_scale = scale.is_some();
    let scale_fold_on = ws.s.scale_folded(ws.s.selected_clip).is_some();
    let fold_name: SharedString = scale.map_or_else(|| "Fold to key".into(), |s| format!("Fold · {}", s.display()).into());
    let keyboard = ws.piano_keyboard;
    let hear = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(
            widgets::icon_button("pr-preview", Icon::Headphones, ws.piano_preview)
                .tooltip(widgets::tip("Audition notes when drawing or moving them", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.piano_preview = !this.piano_preview;
                    if !this.piano_preview {
                        this.release_audition(cx);
                    }
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("pr-keyboard", Icon::Keyboard, keyboard)
                .tooltip(widgets::tip("Computer MIDI Keyboard: A–P notes, Z/X octave, C/V velocity", Some("M")))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_computer_keyboard(cx);
                    if this.piano_keyboard {
                        this.focus.focus(window, cx);
                    }
                })),
        )
        // The octave only matters while the computer keyboard plays notes.
        .when(keyboard, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .rounded(px(r_md()))
                    .bg(content(0.05))
                    .child(
                        widgets::sized_icon_button("pr-octave-down", Icon::Minus, false, 20., 10.)
                            .tooltip(widgets::tip("Lower keyboard octave", Some("Z")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.release_audition(cx);
                                this.piano_octave = this.piano_octave.saturating_sub(1);
                                cx.notify();
                            })),
                    )
                    .child(div().w(px(24.)).flex().justify_center().child(mono(format!("C{}", ws.piano_octave), TEXT_SM, pal.text)))
                    .child(
                        widgets::sized_icon_button("pr-octave-up", Icon::Add, false, 20., 10.)
                            .tooltip(widgets::tip("Raise keyboard octave", Some("X")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.release_audition(cx);
                                this.piano_octave = (this.piano_octave + 1).min(9);
                                cx.notify();
                            })),
                    ),
            )
        });
    let pointer = if names {
        widgets::segmented("pr-tool", &[(Some(Icon::Cursor), "Select"), (Some(Icon::Pencil), "Draw")], if ws.s.tool == Tool::Select { 0 } else { 1 }, 68., on_tool)
    } else {
        widgets::segmented("pr-tool", &[(Some(Icon::Cursor), ""), (Some(Icon::Pencil), "")], if ws.s.tool == Tool::Select { 0 } else { 1 }, 32., on_tool)
    };
    let grid = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(
            div()
                .id("pr-snap")
                .h(px(CONTROL_H))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Snap grid: what notes and edits snap to", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::Snap, point(at.x - px(16.), at.y - px(300.)), cx);
                }))
                .child(icon(Icon::Magnet, 14., if ws.s.snap == Snap::Off { pal.faint } else { pal.accent }))
                .child(label(ws.s.snap_label(crate::editing::GridView::Piano), TEXT_SM, pal.text))
                .child(icon(Icon::ChevronDown, 10., pal.faint)),
        )
        .child(
            tool_button("pr-quantize", Icon::Quantize, name("Quantize"), false, false)
                .tooltip(widgets::tip("Quantize the selected notes (or the clip) to the grid", Some(if cfg!(target_os = "macos") { "⌘U" } else { "Ctrl+U" })))
                .on_click(cx.listener(|this, _, _, cx| this.quantize_clip(cx))),
        );
    let key = tool_button("pr-scale-fold", Icon::Fold, names.then_some(fold_name), scale_fold_on, false)
        .when(!has_scale, |d| d.opacity(0.55))
        .tooltip(widgets::tip("Fold: show only the keys of the clip's scale, and transpose inside it. With no key set, it starts from C minor.", None))
        .on_click(cx.listener(|this, _, _, cx| {
            if this.s.clip_scale(this.s.selected_clip).is_none() {
                this.s.set_scale(Some(aitrack_model::Scale::new(aitrack_model::Key(0), aitrack_model::ScaleKind::Minor)));
                this.s.piano_scale_fold = true;
            } else {
                this.s.piano_scale_fold = !this.s.piano_scale_fold;
            }
            this.s.piano.scroll_y = 0.;
            cx.notify();
        }));
    let tools = tool_button("pr-tools", Icon::Wand, name("Tools"), false, true)
        .tooltip(widgets::tip("Note tools: snap to key, chords, inversions, arpeggios, strum, chop, randomize", None))
        .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
            let at = e.position();
            this.open_menu(MenuKind::PianoTools, point(at.x - px(200.), at.y - px(380.)), cx);
        }));
    let zoom = div()
        .flex()
        .items_center()
        .child(widgets::icon_button("pr-zoom-out", Icon::ZoomOut, false).tooltip(widgets::tip("Zoom out (Cmd + scroll or pinch)", None)).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano.px_per_beat = (this.s.piano.px_per_beat / 1.25).max(12.);
            cx.notify();
        })))
        .child(widgets::icon_button("pr-zoom-in", Icon::ZoomIn, false).tooltip(widgets::tip("Zoom in (Cmd + scroll or pinch)", None)).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano.px_per_beat = (this.s.piano.px_per_beat * 1.25).min(320.);
            cx.notify();
        })));
    let rule = || widgets::divider_v(16.).mx(px(SP1));
    div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(hear)
        .child(rule())
        .child(pointer)
        .child(rule())
        .child(grid)
        .child(rule())
        .child(key)
        .child(tools)
        .child(rule())
        .child(zoom)
        .child(rule())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::NoteId;

    #[test]
    fn seg_bpm_halves_and_doubles_in_one_undo_step() {
        let mut s = crate::state::AppState::new(crate::demo::project());
        let clip = *s.project.clips.iter().find(|(_, c)| matches!(s.project.sources.get(&c.source), Some(Source::Audio(_)))).unwrap().0;
        let warp = |s: &crate::state::AppState| match s.project.sources.get(&s.project.clips[&clip].source) {
            Some(Source::Audio(a)) => a.warp.clone(),
            _ => unreachable!(),
        };
        let stretch = |s: &crate::state::AppState| aitrack_model::ops::stretch_factor(&warp(s), s.project.tempo.initial_bpm());
        let project_bpm = s.project.tempo.initial_bpm();
        s.seal();
        s.set_clip_warp("setup", clip, |w| {
            w.enabled = true;
            w.original_bpm = Some(project_bpm);
        });
        s.seal();
        assert!((stretch(&s) - 1.).abs() < 1e-9);
        let steps = s.history.steps().count();
        let before = warp(&s);
        assert!(edit_seg_bpm(&mut s, clip, SegBpm::Double));
        assert_eq!(warp(&s).original_bpm, Some(project_bpm * 2.));
        assert!((stretch(&s) - 2.).abs() < 1e-9, "the clip re-stretches");
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step");
        assert!(edit_seg_bpm(&mut s, clip, SegBpm::Half));
        assert_eq!(warp(&s), before, "÷2 undoes ×2");
        assert!(edit_seg_bpm(&mut s, clip, SegBpm::Step(1.)));
        assert_eq!(warp(&s).original_bpm, Some(project_bpm + 1.));
        assert!(s.undo());
        assert_eq!(warp(&s), before);
        assert!(!edit_seg_bpm(&mut s, clip, SegBpm::Set(project_bpm)), "same value, no step");
    }

    fn audio_clip(s: &crate::state::AppState) -> ClipId {
        *s.project.clips.iter().find(|(_, c)| matches!(s.project.sources.get(&c.source), Some(Source::Audio(_)))).unwrap().0
    }

    #[test]
    fn launch_mode_and_quantize_pick_a_value_in_one_undo_step() {
        let mut s = crate::state::AppState::new(crate::demo::project());
        let clip = audio_clip(&s);
        let before = s.project.clips[&clip].clone();
        let steps = s.history.steps().count();
        let (mode, quant) = (LaunchMode::Repeat, Quantize::TwoBars);
        let (mode, quant) = if before.launch.mode == mode { (LaunchMode::Gate, quant) } else { (mode, quant) };
        let quant = if before.launch.quantize == quant { Quantize::Bar } else { quant };
        assert!(set_launch_mode(&mut s, clip, mode));
        assert!(!set_launch_mode(&mut s, clip, mode), "same value, no step");
        assert!(set_launch_quantize(&mut s, clip, quant));
        assert_eq!((s.project.clips[&clip].launch.mode, s.project.clips[&clip].launch.quantize), (mode, quant));
        assert_eq!(s.history.steps().count(), steps + 2);
        assert!(s.undo() && s.undo());
        assert_eq!(s.project.clips[&clip], before);
    }

    #[test]
    fn typed_positions_follow_the_panel_format() {
        let four = TimeSignature { numerator: 4, denominator: 4 };
        let p = aitrack_model::PPQ;
        assert_eq!(parse_position("2.0.0", four), Some(8 * p));
        assert_eq!(parse_position("1.2.2", four), Some(4 * p + 2 * p + p / 2));
        assert_eq!(parse_position("4", four), Some(16 * p));
        assert_eq!(parse_position(" 0,1 ", four), Some(p), "comma separates parts too");
        assert_eq!(parse_position("a", four), None);
        assert_eq!(parse_position("1.2.3.4", four), None);
        assert_eq!(parse_position("-1", four), None);
        // What the panel prints parses back to the same tick.
        let six8 = TimeSignature { numerator: 6, denominator: 8 };
        let t = 3 * six8.bar_ticks() + 2 * (p / 2) + p / 4;
        assert_eq!(parse_position(&format_len(t, six8), six8), Some(t));
    }

    #[test]
    fn typed_start_and_length_are_one_undo_step_each() {
        let mut s = crate::state::AppState::new(crate::demo::project());
        let clip = audio_clip(&s);
        let p = aitrack_model::PPQ;
        let before = s.project.clips[&clip].clone();
        let steps = s.history.steps().count();
        assert!(commit_clip_field(&mut s, clip, ClipField::Length, "0.2.0"));
        assert_eq!(s.project.clips[&clip].length, 2 * p);
        assert!(commit_clip_field(&mut s, clip, ClipField::Start, "0.1.0"));
        assert_eq!(s.project.clips[&clip].offset, p);
        assert_eq!(s.history.steps().count(), steps + 2);
        assert!(!commit_clip_field(&mut s, clip, ClipField::Start, "nope"));
        assert!(s.undo() && s.undo());
        assert_eq!(s.project.clips[&clip], before);
        // A start past the file's end is held inside it.
        assert!(set_clip_start(&mut s, clip, 1_000_000_000));
        let file = aitrack_model::ops::source_len(&s.project, clip).unwrap();
        assert!(s.project.clips[&clip].offset < file);
    }

    #[test]
    fn the_tool_bar_names_its_controls_from_1280_px() {
        assert!(!tool_names(1024.) && !tool_names(1279.));
        assert!(tool_names(1280.) && tool_names(1480.));
    }

    #[test]
    fn seg_bpm_stays_in_the_tempo_range() {
        assert_eq!(SegBpm::Half.apply(30.), 20.);
        assert_eq!(SegBpm::Double.apply(700.), 999.);
        assert_eq!(SegBpm::Set(f64::NAN).apply(120.), 120.);
        assert_eq!(SegBpm::Step(-1.).apply(120.5), 119.5);
    }

    #[test]
    fn audio_clips_get_the_sample_editor_and_steps_stay_steps() {
        assert_eq!(clip_editor(BottomTab::PianoRoll, true, false), ClipEditor::Sample);
        assert_eq!(clip_editor(BottomTab::PianoRoll, false, false), ClipEditor::Piano);
        assert_eq!(clip_editor(BottomTab::PianoRoll, false, true), ClipEditor::StepClip);
        assert_eq!(clip_editor(BottomTab::Steps, false, false), ClipEditor::Steps);
        assert_eq!(clip_editor(BottomTab::Steps, true, false), ClipEditor::Steps);
    }

    #[test]
    fn launch_settings_cycle_through_every_value() {
        let mut m = LaunchMode::Trigger;
        let mut seen = vec![];
        for _ in 0..4 {
            seen.push(launch_mode_name(m));
            m = next_launch_mode(m);
        }
        assert_eq!(m, LaunchMode::Trigger);
        assert_eq!(seen, ["Trigger", "Gate", "Toggle", "Repeat"]);
        let mut q = Quantize::None;
        for _ in 0..7 {
            q = next_quantize(q);
        }
        assert_eq!(q, Quantize::None);
    }

    #[test]
    fn transpose_stays_in_range_and_skips_stuck_notes() {
        let note = |id, key| Note::new(NoteId(id), key, 0, 96, 0.8);
        let notes = [note(1, 60), note(2, 127)];
        let up = transposed(&notes, 1).unwrap();
        assert_eq!(up.len(), 1, "the top note cannot go higher");
        assert_eq!(up[0].key, 61);
        assert!(transposed(&[note(3, 0)], -12).is_none());
        assert_eq!(transposed(&notes, -12).unwrap().iter().map(|n| n.key).collect::<Vec<_>>(), [48, 115]);
        assert_eq!(key_name(60), "C4");
        assert_eq!(key_name(61), "C#4");
    }
}
