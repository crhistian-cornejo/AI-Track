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
    AutomationTarget, Clip, ClipId, Command, LaunchMode, LoopRegion, Note, Quantize, Source, Tick, TimeSignature,
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
}

/// The editor for the current tab and clip: audio clips always get the
/// sample editor; the Steps tab shows the drum rack's step clip.
pub fn clip_editor(tab: BottomTab, audio: bool) -> ClipEditor {
    match tab {
        BottomTab::Steps => ClipEditor::Steps,
        _ if audio => ClipEditor::Sample,
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
    let editor = clip_editor(tab, audio);
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
    let tabs = widgets::segmented("detail-view", &[(Some(Icon::Device), "Device"), (Some(Icon::Piano), "Clip")], usize::from(clip_view), 96., on_view);

    // What the panel is showing, next to the switch.
    let context = if clip_view {
        clip_id.and_then(|c| ws.s.project.clips.get(&c)).map(|c| (c.name.clone(), c.color))
    } else {
        ws.s.selected_track.and_then(|t| ws.s.project.track(t)).map(|t| (t.name.clone(), t.color))
    };
    let mut bar = div()
        .flex_none()
        .h(px(TAB_BAR_H + SP2))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .child(tabs)
        .when_some(context, |d, (name, color)| {
            d.child(div().w(px(SP1)))
                .child(div().size(px(8.)).rounded_full().bg(track_color(color)))
                .child(label(crate::paint::fit(&name, 144., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
        });
    // MIDI clips choose between the piano roll and the step sequencer.
    if clip_view && !audio && step_clip.is_some() {
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
    if clip_view && editor == ClipEditor::Piano && ws.bottom_open {
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
        let content = if !clip_view {
            crate::views::devices::render(ws, window, cx)
        } else {
            let editor_el = match editor {
                ClipEditor::Sample => crate::views::sample_editor::render(ws, window, cx),
                ClipEditor::Piano => crate::views::piano_roll::render(ws, window, cx),
                ClipEditor::Steps => crate::views::steps::render(ws, window, cx),
            };
            let props = clip_id.and_then(|id| clip_props(ws, id, editor, cx));
            div()
                .size_full()
                .flex()
                .children(props)
                .child(div().flex_1().min_w(px(0.)).h_full().child(editor_el))
                .into_any_element()
        };
        Some(motion::rise_in(gpui::ElementId::Name(format!("tab-{}", ws.tab_switch).into()), div().size_full().child(content), if ws.tab_switch == 0 { 0. } else { 8. }))
    } else {
        None
    };

    let resizing = matches!(ws.drag, Some(Drag::BottomResize { .. }));
    let target = if ws.bottom_open { ws.bottom_h } else { TAB_BAR_H + SP2 };
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
                    this.bottom_h = crate::workspace::bottom_h_clamped(300., this.window_h);
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
fn section(title: &'static str, ic: Icon) -> Div {
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
fn row(caption: &'static str, value: impl IntoElement) -> Div {
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
fn small_button(id: impl Into<gpui::ElementId>, text: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
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
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
            if !this.num_edit.as_ref().is_some_and(|e| e.target == target) {
                this.begin_num_edit(target, prefill.clone(), cx);
            }
        }))
        .child(body)
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
        .child(row("Signature", value_box(format!("{}/{}", sig.numerator, sig.denominator))))
        .child(row("Start", value_box(format_len(clip.offset, sig))))
        .child(row(
            "Length",
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(value_box(format_len(length, sig)))
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
            small_button(("clip-mode", n), launch_mode_name(mode), true).tooltip(widgets::tip("Launch mode: click to change", None)).on_click(cx.listener(
                move |this, _, _, cx| {
                    update_clip(this, id, "Launch mode", |c| c.launch.mode = next_launch_mode(mode));
                    cx.notify();
                },
            )),
        ))
        .child(row(
            "Quantize",
            small_button(("clip-quant", n), quantize_name(quant), true).tooltip(widgets::tip("Launch quantisation: click to change", None)).on_click(cx.listener(
                move |this, _, _, cx| {
                    update_clip(this, id, "Launch quantisation", |c| c.launch.quantize = next_quantize(quant));
                    cx.notify();
                },
            )),
        ));

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
            let (warp_on, reversed) = (warp.enabled, warp.reverse);
            let mode = format!("{:?}", warp.mode);
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
                                .tooltip(widgets::tip("Warp: follow the project tempo (Re-Pitch: speed and pitch move together)", None))
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
                        .child(widgets::badge(mode, pal.muted).id(("warp-mode", n)).tooltip(widgets::tip("Warp mode. Only Re-Pitch plays for now: pitch-preserving stretch is on its way", None))),
                ))
                .when_some(warp.original_bpm, |d, b| d.child(row("Seg. BPM", value_box(format!("{b:.2}")))))
                // A clip stretched with Cmd/Ctrl + an edge (Re-Pitch).
                .when(stretch != 1., |d| {
                    d.child(row("Stretch", value_box(format!("×{stretch:.2}")).id(("stretch", n)).tooltip(widgets::tip("Stretched in time: Cmd/Ctrl + drag a clip edge in the arrangement. Pitch follows (Re-Pitch)", None))))
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
                    toggle_button(("reverse", n), "Rev", warp.reverse).tooltip(widgets::tip("Play the sample backwards", None)).on_click(cx.listener(move |this, _, _, cx| {
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
                .child(row("Scale", widgets::badge(scale, pal.accent)))
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
        env_box = env_box.child(div().px(px(SP2)).py(px(SP1)).child(label(
            if audio { "Double-click the waveform to draw the volume envelope." } else { "No clip envelopes yet." },
            TEXT_SM,
            pal.faint,
        )));
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

fn piano_tools(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let weak = cx.entity().downgrade();
    let on_tool: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |this, cx| {
            this.s.tool = if i == 0 { Tool::Select } else { Tool::Draw };
            cx.notify();
        });
    });
    let has_scale = ws.s.clip_scale(ws.s.selected_clip).is_some();
    div()
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
            widgets::icon_button("pr-keyboard", Icon::Keyboard, ws.piano_keyboard)
                .tooltip(widgets::tip("Computer keyboard: A W S E D F T G Y H U J K O L P", None))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.piano_keyboard = !this.piano_keyboard;
                    if !this.piano_keyboard {
                        this.release_audition(cx);
                    } else {
                        this.focus.focus(window, cx);
                    }
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("pr-octave-down", Icon::Minus, false)
                .tooltip(widgets::tip("Lower keyboard octave", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.release_audition(cx);
                    this.piano_octave = this.piano_octave.saturating_sub(1);
                    cx.notify();
                })),
        )
        .child(label(format!("C{}", ws.piano_octave), TEXT_SM, pal.text))
        .child(
            widgets::icon_button("pr-octave-up", Icon::Add, false)
                .tooltip(widgets::tip("Raise keyboard octave", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.release_audition(cx);
                    this.piano_octave = (this.piano_octave + 1).min(9);
                    cx.notify();
                })),
        )
        .child(widgets::divider_v(16.))
        .child(widgets::segmented("pr-tool", &[(Some(Icon::Cursor), ""), (Some(Icon::Pencil), "")], if ws.s.tool == Tool::Select { 0 } else { 1 }, 32., on_tool))
        .child(div().w(px(SP1)))
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
                .tooltip(widgets::tip("Snap grid", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::Snap, point(at.x - px(16.), at.y - px(300.)), cx);
                }))
                .child(icon(Icon::Magnet, 14., if ws.s.snap == Snap::Off { pal.faint } else { pal.accent }))
                .child(label(ws.s.snap_label(crate::editing::GridView::Piano), TEXT_SM, pal.text)),
        )
        .child(widgets::icon_button("pr-quantize", Icon::Quantize, false).tooltip(widgets::tip("Quantize to grid", None)).on_click(cx.listener(|this, _, _, cx| this.quantize_clip(cx))))
        .child(
            widgets::icon_button("pr-scale", Icon::ScaleSnap, false)
                .when(!has_scale, |d| d.opacity(0.4))
                .tooltip(widgets::tip("Snap notes to the key", None))
                .on_click(cx.listener(|this, _, _, cx| this.scale_snap_clip(cx))),
        )
        .child(widgets::icon_button("pr-tools", Icon::Wand, false).tooltip(widgets::tip("Note tools", None)).on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
            let at = e.position();
            this.open_menu(MenuKind::PianoTools, point(at.x - px(200.), at.y - px(236.)), cx);
        })))
        .child(widgets::divider_v(16.))
        .child(widgets::icon_button("pr-zoom-out", Icon::ZoomOut, false).tooltip(widgets::tip("Zoom out (Cmd + scroll or pinch)", None)).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano.px_per_beat = (this.s.piano.px_per_beat / 1.25).max(12.);
            cx.notify();
        })))
        .child(widgets::icon_button("pr-zoom-in", Icon::ZoomIn, false).tooltip(widgets::tip("Zoom in (Cmd + scroll or pinch)", None)).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano.px_per_beat = (this.s.piano.px_per_beat * 1.25).min(320.);
            cx.notify();
        })))
        .child(widgets::divider_v(16.))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::NoteId;

    #[test]
    fn audio_clips_get_the_sample_editor_and_steps_stay_steps() {
        assert_eq!(clip_editor(BottomTab::PianoRoll, true), ClipEditor::Sample);
        assert_eq!(clip_editor(BottomTab::PianoRoll, false), ClipEditor::Piano);
        assert_eq!(clip_editor(BottomTab::Steps, false), ClipEditor::Steps);
        assert_eq!(clip_editor(BottomTab::Steps, true), ClipEditor::Steps);
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
