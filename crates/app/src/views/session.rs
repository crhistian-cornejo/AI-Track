//! Session View (Ableton): track columns × scene rows of launchable clips,
//! a scene launch column, and a mini mixer under each column.

use crate::bridge::SlotState;
use crate::icons::{Icon, icon};
use crate::paint;
use crate::state::is_drum_track;
use crate::theme::*;
use crate::views::browser::kind_icon;
use crate::widgets::{self, chip, label, mono};
use crate::session_edit::DraggedSlot;
use crate::workspace::{DraggedSample, Workspace};
use aitrack_model::{ClipId, SceneId, Tick, TrackId, TrackKind, TrackProp};
use gpui::{
    AnyElement, AppContext as _, Context, Div, ExternalPaths, FontWeight, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, point, prelude::FluentBuilder, px,
};

const GAP: f32 = SP1;
const COL_PAD: f32 = SP1;

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let transport = ws.b.engine.transport();
    let tracks: Vec<TrackId> = ws
        .s
        .project
        .content_tracks()
        .filter(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Audio | TrackKind::Group | TrackKind::Return))
        .map(|t| t.id)
        .collect();
    let scenes: Vec<(SceneId, String)> = ws.s.project.scenes.iter().map(|s| (s.id, s.name.clone())).collect();

    let mut columns = div().flex().gap(px(GAP)).p(px(SP2)).pr(px(0.));
    for tid in &tracks {
        columns = columns.child(track_column(ws, *tid, &scenes, transport.position, transport.playing, cx));
    }

    // Scene column, rows aligned with the clip slots.
    let mut scene_col = div()
        .flex_none()
        .w(px(SESSION_COL_W))
        .p(px(COL_PAD))
        .flex()
        .flex_col()
        .gap(px(GAP))
        .child(
            div()
                .h(px(m().session_head_h))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .rounded(px(r_md()))
                .bg(pal.surface)
                .child(icon(Icon::Play, 12., pal.muted))
                .child(label("Scenes", TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM)),
        );
    let cursor_scene = ws.s.session_scene();
    let scene_count = scenes.len();
    for (i, (sid, name)) in scenes.iter().enumerate() {
        let sid = *sid;
        let at_cursor = cursor_scene == Some(sid);
        let group = format!("scene-row-{i}");
        scene_col = scene_col.child(
            div()
                .id(("scene", i))
                .group(group.clone())
                .relative()
                .h(px(session_row_h()))
                .px(px(SP1))
                .flex()
                .items_center()
                .gap(px(SP2))
                .rounded(px(r_md()))
                .bg(pal.surface)
                .border_1()
                .border_color(if at_cursor { accent_alpha(0.5) } else { content(0.0) })
                .cursor_pointer()
                .hover(|s| s.bg(pal.raised))
                .tooltip(widgets::tip("Click to select, double-click to rename", None))
                .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                    this.s.set_session_scene(sid);
                    if e.click_count() >= 2 {
                        crate::commands::open_rename_for(this, Some(crate::commands::RenameTarget::Scene(sid)), window, cx);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .id(("scene-launch", i))
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(r_sm()))
                        .bg(content(0.06))
                        .hover(|s| s.bg(content(0.14)))
                        .tooltip(widgets::tip("Launch scene", Some("Enter")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.set_session_scene(sid);
                            this.b.engine.launch_scene(sid);
                            cx.notify();
                        }))
                        .child(icon(Icon::Play, 10., pal.text)),
                )
                .child(label(paint::fit(name, SESSION_COL_W - 88., TEXT_SM), TEXT_SM, pal.muted).flex_1())
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(div().group_hover(group.clone(), |s| s.invisible()).child(mono(format!("{}", i + 1), TEXT_XS, pal.faint)))
                        .child(
                            div()
                                .absolute()
                                .right(px(SP1))
                                .top_0()
                                .h_full()
                                .items_center()
                                .flex()
                                .gap(px(SP1))
                                .invisible()
                                .group_hover(group, |s| s.visible())
                                .child(
                                    div()
                                        .id(("scene-dup", i))
                                        .size(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(r_sm()))
                                        .hover(|s| s.bg(content(0.14)))
                                        .tooltip(widgets::tip("Duplicate scene", None))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.s.duplicate_scene(sid);
                                            cx.notify();
                                        }))
                                        .child(icon(Icon::Copy, 12., pal.muted)),
                                )
                                .child(
                                    div()
                                        .id(("scene-del", i))
                                        .size(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(r_sm()))
                                        .hover(|s| s.bg(content(0.14)))
                                        .tooltip(widgets::tip(if scene_count > 1 { "Delete scene" } else { "The last scene stays" }, None))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.s.remove_scene(sid);
                                            cx.notify();
                                        }))
                                        .child(icon(Icon::Trash, 12., pal.muted)),
                                ),
                        ),
                ),
        );
    }
    scene_col = scene_col.child(
        div()
            .id("stop-all")
            .h(px(session_row_h()))
            .px(px(SP2))
            .flex()
            .items_center()
            .gap(px(SP2))
            .rounded(px(r_md()))
            .cursor_pointer()
            .border_1()
            .border_color(border())
            .hover(|s| s.bg(hover()))
            .child(icon(Icon::Stop, 12., pal.muted))
            .child(label("Stop all", TEXT_SM, pal.muted))
            .on_click(cx.listener(|this, _, _, cx| {
                let ids: Vec<TrackId> = this.s.project.tracks.iter().map(|t| t.id).collect();
                for t in ids {
                    this.b.engine.stop_track(t);
                }
                cx.notify();
            })),
    );
    scene_col = scene_col.child(
        div()
            .id("add-scene")
            .h(px(session_row_h()))
            .px(px(SP2))
            .flex()
            .items_center()
            .gap(px(SP2))
            .rounded(px(r_md()))
            .cursor_pointer()
            .border_1()
            .border_color(border())
            .hover(|s| s.bg(hover()))
            .tooltip(widgets::tip("Add a scene below the selected one", None))
            .child(icon(Icon::Add, 12., pal.muted))
            .child(label("Add scene", TEXT_SM, pal.muted))
            .on_click(cx.listener(|this, _, _, cx| {
                let after = this.s.session_scene();
                this.s.add_scene(after);
                cx.notify();
            })),
    );

    div()
        .size_full()
        .flex()
        .child(div().id("session-scroll").flex_1().min_w(px(0.)).h_full().overflow_x_scroll().overflow_y_scroll().child(columns))
        .child(div().w(px(1.)).h_full().bg(border()))
        .child(div().id("scene-col").h_full().overflow_y_scroll().pt(px(SP1)).child(scene_col))
        .into_any_element()
}

fn track_column(ws: &mut Workspace, tid: TrackId, scenes: &[(SceneId, String)], pos: Tick, playing: bool, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let Some(t) = ws.s.project.track(tid).cloned() else { return div() };
    let selected = ws.s.selected_track == Some(tid);
    let state = ws.b.engine.slot_state(tid);
    let color = track_color(t.color);
    let has_clips = t.kind.has_clips();
    let meter = ws.b.engine.meter(tid);

    let header = div()
        .id(("session-head", tid.0 as usize))
        .h(px(m().session_head_h))
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .bg(track_header_bg(if selected { pal.raised } else { pal.surface }, t.color))
        .border_1()
        .border_color(if selected { accent_alpha(0.5) } else { border() })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.s.selected_track = Some(tid);
            cx.notify();
        }))
        .child(div().w(px(3.)).h(px(16.)).rounded(px(2.)).bg(color))
        .child(icon(if is_drum_track(&t) { Icon::Drum } else { kind_icon(t.kind) }, 14., pal.muted))
        .child(label(paint::fit(&t.name, SESSION_COL_W - 64., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
        .child(widgets::meter(meter, 6., 16.));

    let mut col = div().flex_none().w(px(SESSION_COL_W)).p(px(COL_PAD)).flex().flex_col().gap(px(GAP)).child(header);
    for (si, (sid, _)) in scenes.iter().enumerate() {
        let clip = t.session_slots.get(sid).copied();
        col = col.child(slot(ws, tid, *sid, si, clip, has_clips, state, pos, playing, cx));
    }
    // Track stop + mini mixer.
    col = col.child(
        div()
            .id(("stop-track", tid.0 as usize))
            .h(px(session_row_h()))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(r_md()))
            .border_1()
            .border_color(border())
            .cursor_pointer()
            .hover(|s| s.bg(hover()))
            .when(has_clips, |d| d.child(icon(Icon::Stop, 12., if state.is_some() { pal.text } else { pal.faint })))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.b.engine.stop_track(tid);
                cx.notify();
            })),
    );
    let (mute, solo, arm) = (t.mute, t.solo, t.arm);
    col.child(
        div()
            .h(px(session_row_h()))
            .px(px(SP1))
            .flex()
            .items_center()
            .gap(px(SP1))
            .rounded(px(r_md()))
            .bg(pal.surface)
            .child(chip(("s-m", tid.0 as usize), "M", mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                this.s.set_track(tid, TrackProp::Mute(!mute));
                cx.notify();
            })))
            .child(chip(("s-s", tid.0 as usize), "S", solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                this.s.set_track(tid, TrackProp::Solo(!solo));
                cx.notify();
            })))
            .when(has_clips, |d| {
                d.child(chip(("s-r", tid.0 as usize), "R", arm, pal.record).on_click(cx.listener(move |this, _, _, cx| {
                    this.s.set_track(tid, TrackProp::Arm(!arm));
                    cx.notify();
                })))
            })
            .when(t.kind == aitrack_model::TrackKind::Instrument, |d| {
                let kbd = t.keyboard_input;
                let kbd_color = if ws.piano_keyboard { pal.accent } else { pal.muted };
                d.child(
                    widgets::icon_chip(("s-kbd", tid.0 as usize), Icon::Keyboard, kbd, kbd_color)
                        .tooltip(widgets::tip("Play this track with the PC/Mac keyboard (Z/X octave, C/V velocity)", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_track_keyboard(tid, cx);
                        })),
                )
            })
            .child(div().flex_1())
            .child(mono(crate::state::format_db(t.volume_db), TEXT_XS, pal.muted)),
    )
}

#[allow(clippy::too_many_arguments)]
fn slot(
    ws: &mut Workspace,
    tid: TrackId,
    sid: SceneId,
    si: usize,
    clip: Option<ClipId>,
    has_clips: bool,
    state: Option<SlotState>,
    pos: Tick,
    playing: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let key = tid.0 as usize * 1000 + si;
    let at_cursor = ws.s.selected_track == Some(tid) && ws.s.session_scene() == Some(sid);
    let instrument = ws.s.project.track(tid).is_some_and(|t| t.kind == TrackKind::Instrument);
    let Some(cid) = clip else {
        let empty = div()
            .id(("slot-empty", key))
            .h(px(session_row_h()))
            .rounded(px(r_md()))
            .bg(content(if has_clips { 0.035 } else { 0.015 }))
            .border_1()
            .border_color(if at_cursor { accent_alpha(0.6) } else { border() })
            .flex()
            .items_center()
            .justify_center()
            .gap(px(SP2))
            .when(has_clips, |d| {
                d.group(format!("slot-{key}"))
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.06)))
                    .child(
                        div()
                            .id(("slot-stop", key))
                            .invisible()
                            .group_hover(format!("slot-{key}"), |s| s.visible())
                            .tooltip(widgets::tip("Stop the track's clip", None))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.b.engine.stop_track(tid);
                                this.s.select_slot(tid, sid);
                                cx.notify();
                            }))
                            .child(icon(Icon::Stop, 10., pal.faint)),
                    )
                    .when(instrument, |d| {
                        d.child(
                            div()
                                .id(("slot-new", key))
                                .invisible()
                                .group_hover(format!("slot-{key}"), |s| s.visible())
                                .tooltip(widgets::tip("New MIDI clip (or double-click the slot)", None))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.s.create_midi_clip_in_slot(tid, sid);
                                    cx.notify();
                                }))
                                .child(icon(Icon::Add, 10., pal.faint)),
                        )
                    })
                    .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                        this.s.select_slot(tid, sid);
                        if e.click_count() >= 2 {
                            this.s.create_midi_clip_in_slot(tid, sid);
                        }
                        cx.notify();
                    }))
            });
        return accept_drops(empty, has_clips, tid, sid, cx).into_any_element();
    };
    let Some(c) = ws.s.project.clips.get(&cid).cloned() else { return div().into_any_element() };
    let selected = ws.s.selected_clip == Some(cid);
    let len = ws.s.clip_edit_len(cid);
    let (is_playing, progress, queued) = match state {
        Some(SlotState::Playing { scene, since }) if scene == sid => (true, if playing { ((pos - since).rem_euclid(len)) as f32 / len as f32 } else { 0. }, false),
        Some(SlotState::Queued { scene }) if scene == sid => (false, 0., true),
        _ => (false, 0., false),
    };
    let fg = clip_text(c.color);
    let launch = div()
        .id(("launch", key))
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(shadow(0.18))
        .hover(|s| s.bg(shadow(0.3)))
        .tooltip(widgets::tip("Launch clip", None))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.b.engine.launch_clip(tid, sid);
            cx.notify();
        }))
        .child(if is_playing {
            // Progress ring: one arc per frame, only while the transport runs.
            canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let c = b.center();
                    paint::arc(window, c, 7., 0., std::f32::consts::TAU, 2., with_alpha(fg, 0.25));
                    let a0 = -std::f32::consts::FRAC_PI_2;
                    paint::arc(window, c, 7., a0, a0 + std::f32::consts::TAU * progress.max(0.01), 2., fg);
                    paint::polygon(window, &[point(c.x - px(2.), c.y - px(3.)), point(c.x + px(3.), c.y), point(c.x - px(2.), c.y + px(3.))], fg);
                },
            )
            .size_full()
            .into_any_element()
        } else {
            icon(Icon::Play, 10., if queued { pal.accent } else { fg }).into_any_element()
        });
    let dragged = DraggedSlot { track: tid, scene: sid, name: c.name.clone(), color: c.color };
    let filled = div()
        .id(("slot", key))
        .h(px(session_row_h()))
        .pl(px(SP1))
        .pr(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .bg(clip_fill(c.color))
        .border_1()
        .border_color(if selected || at_cursor { pal.accent } else if is_playing { with_alpha(fg, 0.6) } else { shadow(0.2) })
        .cursor_pointer()
        .hover(|s| s.opacity(0.92))
        .on_drag(dragged, |d: &DraggedSlot, _, _, cx| cx.new(|_| d.clone()))
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
            this.s.select_slot(tid, sid);
            if e.click_count() >= 2 {
                this.set_tab(crate::state::BottomTab::PianoRoll, cx);
            }
            cx.notify();
        }))
        .child(launch)
        .child(label(paint::fit(&c.name, SESSION_COL_W - 48., TEXT_SM), TEXT_SM, fg).font_weight(FontWeight::MEDIUM).flex_1())
        .when(c.looping.is_some(), |d| d.child(icon(Icon::Loop, 10., with_alpha(fg, 0.7))));
    accept_drops(filled, true, tid, sid, cx).into_any_element()
}

/// What a slot takes when something is dropped on it: a library sample, audio
/// files from Finder, or another slot's clip (moved; copied with Alt/Option).
fn accept_drops(slot: Stateful<Div>, enabled: bool, tid: TrackId, sid: SceneId, cx: &mut Context<Workspace>) -> Stateful<Div> {
    if !enabled {
        return slot;
    }
    slot.drag_over::<DraggedSample>(|s, _, _, _| s.border_color(accent()).bg(accent_alpha(0.12)))
        .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(accent()).bg(accent_alpha(0.12)))
        .drag_over::<DraggedSlot>(|s, _, _, _| s.border_color(accent()).bg(accent_alpha(0.12)))
        .on_drop(cx.listener(move |this, d: &DraggedSample, _, cx| this.drop_sample_in_slot(d, tid, sid, cx)))
        .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| this.drop_files_in_slot(paths.paths().to_vec(), tid, sid, cx)))
        .on_drop(cx.listener(move |this, d: &DraggedSlot, window, cx| {
            let copy = window.modifiers().alt;
            this.drop_slot(d, (tid, sid), copy, cx);
        }))
}
