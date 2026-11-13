//! The arrangement's context menus: one for clips, one for empty lane
//! space. Every item shows the shortcut the key table gives it, and runs
//! the same code as the key.

use crate::editing::Area;
use crate::icons::Icon;
use crate::keys::{self, Cmd};
use crate::theme::*;
use crate::widgets::{self, menu_item, mono};
use crate::workspace::{MenuKind, Workspace};
use aitrack_model::{DeviceId, TrackId, TrackKind};
use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, ParentElement, Stateful, StatefulInteractiveElement, Window};

/// A menu row with its shortcut on the right.
fn item(id: &'static str, icon: Icon, text: &'static str, hint: Option<String>, checked: bool) -> Stateful<gpui::Div> {
    let row = menu_item(ElementId::Name(id.into()), Some(icon), text, None, checked);
    match hint {
        Some(h) => row.child(mono(h, TEXT_XS, p().faint)),
        None => row,
    }
}

/// A row that closes the menu and runs a table command.
fn command(id: &'static str, icon: Icon, text: &'static str, cmd: Cmd, cx: &mut Context<Workspace>) -> Stateful<gpui::Div> {
    item(id, icon, text, keys::hint(cmd), false).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
        this.menu = None;
        crate::commands::run(this, cmd, window, cx);
    }))
}

/// A row that closes the menu and runs `f`.
fn action(
    id: &'static str,
    icon: Icon,
    text: &'static str,
    hint: Option<String>,
    checked: bool,
    cx: &mut Context<Workspace>,
    f: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<gpui::Div> {
    item(id, icon, text, hint, checked).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
        this.menu = None;
        f(this, window, cx);
        cx.notify();
    }))
}

/// Right-click on a clip. It acts on the selected clips (the clip under
/// the mouse joined the selection when the menu opened).
pub fn clip_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let clips = ws.s.clip_selection();
    let state = |f: fn(&aitrack_model::Clip) -> bool| clips.iter().filter_map(|(_, pl, _)| ws.s.project.clips.get(&pl.clip)).any(f);
    let looping = state(|c| c.looping.is_some());
    let muted = !clips.is_empty() && !state(|c| !c.muted);
    // The clip under the mouse, when it is audio and other clips play its file too.
    let sample = ws.s.selected_placement.and_then(|(_, id)| ws.s.placement(id)).map(|(_, pl, _)| pl.clip).filter(|c| ws.s.sample_targets(*c).len() > 1);
    let card = widgets::menu_card("clip-menu", 232.)
        .child(command("clip-cut", Icon::Scissors, "Cut", Cmd::Cut, cx))
        .child(command("clip-copy", Icon::Copy, "Copy", Cmd::Copy, cx))
        .child(command("clip-paste", Icon::Paste, "Paste", Cmd::Paste, cx))
        .child(action("clip-duplicate", Icon::Layers, "Duplicate", keys::action_hint("Duplicate"), false, cx, |this, _, _| {
            this.s.duplicate_selection();
        }))
        .child(action("clip-delete", Icon::Trash, "Delete", keys::action_hint("DeleteSelection"), false, cx, |this, _, _| {
            this.s.delete_selection();
        }))
        .child(widgets::divider_h())
        .child(action("clip-split", Icon::Split, "Split at Marker", keys::action_hint("SplitClip"), false, cx, |this, _, cx| this.split_clips(cx)))
        .child(action("clip-loop", Icon::Loop, "Loop", None, looping, cx, |this, _, cx| match this.s.toggle_clips_loop() {
            Some(true) => this.toast("Clip loops: drag its right edge to repeat it", cx),
            Some(false) => this.toast("Clip no longer loops", cx),
            None => {}
        }))
        .child(command("clip-consolidate", Icon::Group, "Consolidate", Cmd::Consolidate, cx))
        .child(command("clip-rename", Icon::Pencil, "Rename", Cmd::Rename, cx))
        .child(action("clip-active", Icon::Power, if muted { "Activate" } else { "Deactivate" }, keys::action_hint("ToggleActive"), false, cx, |this, _, _| {
            this.s.toggle_clips_active();
        }));
    let Some(clip) = sample else { return card.into_any_element() };
    // Transpose, detune and reverse of this clip on every clip of its sample.
    card.child(widgets::divider_h())
        .child(action("clip-sample-all", Icon::Waveform, "Apply to All Clips of This Sample", None, false, cx, move |this, _, cx| {
            let n = this.s.sample_targets(clip).len();
            if this.s.match_sample_pitch(clip) {
                this.toast(format!("Pitch applied to all {n} clips of this sample"), cx);
            } else {
                this.toast("Every clip of this sample already has this pitch", cx);
            }
        }))
        .into_any_element()
}

/// Right-click on empty lane space: the marker is already there.
pub fn lane_menu(cx: &mut Context<Workspace>) -> AnyElement {
    widgets::menu_card("lane-menu", 232.)
        .child(command("lane-paste", Icon::Paste, "Paste", Cmd::Paste, cx))
        .child(command("lane-midi-clip", Icon::MusicNote, "Insert MIDI Clip", Cmd::InsertMidiClip, cx))
        .child(widgets::divider_h())
        .child(action("lane-audio-track", Icon::Waveform, "Insert Audio Track", keys::action_hint("AddAudioTrack"), false, cx, |this, _, _| {
            this.s.add_track(TrackKind::Audio);
        }))
        .child(action("lane-midi-track", Icon::Piano, "Insert MIDI Track", keys::action_hint("AddMidiTrack"), false, cx, |this, _, _| {
            this.s.add_track(TrackKind::Instrument);
        }))
        .child(widgets::divider_h())
        .child(command("lane-shortcuts", Icon::Keyboard, "Keyboard Shortcuts…", Cmd::Shortcuts, cx))
        .into_any_element()
}

/// Right-click on a track header (Live): it acts on the selected tracks,
/// which the track under the mouse joined when the menu opened.
pub fn track_menu(ws: &mut Workspace, track: TrackId, cx: &mut Context<Workspace>) -> AnyElement {
    let p = &ws.s.project;
    let Some(t) = p.track(track) else {
        return widgets::menu_card("track-menu", 232.).into_any_element();
    };
    let is_group = t.kind == TrackKind::Group;
    let in_group = is_group
        || t.parent
            .and_then(|g| p.track(g))
            .is_some_and(|g| g.kind == TrackKind::Group);
    let folded = if is_group {
        t.collapsed
    } else {
        t.parent
            .and_then(|g| p.track(g))
            .is_some_and(|g| g.collapsed)
    };
    let groupable = t.kind.has_clips() || matches!(t.kind, TrackKind::Group | TrackKind::Folder);
    let many = ws.s.selected_tracks().len() > 1;
    let mut card = widgets::menu_card("track-menu", 232.);
    if groupable {
        if t.kind == TrackKind::Instrument {
            let kbd = t.keyboard_input;
            card = card.child(
                item("track-kbd", Icon::Keyboard, "Play with Computer Keyboard", None, kbd)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.toggle_track_keyboard(track, cx);
                    })),
            );
        }
        let key_hint = t.scale.map(|s| s.display()).unwrap_or_else(|| "Project".into());
        card = card.child(
            item("track-key", Icon::MusicNote, "Track Key / Scale…", Some(key_hint), t.scale.is_some())
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::TrackScale(track), gpui::point(at.x, at.y + gpui::px(8.)), cx);
                })),
        );
        card = card.child(action(
            "track-group",
            Icon::Group,
            if many { "Group Tracks" } else { "Group Track" },
            keys::action_hint("GroupTracks"),
            false,
            cx,
            |this, _, cx| {
                this.s.ed.area = Area::Arrange;
                crate::editing::group_selection(this, cx);
            },
        ));
    }
    if in_group {
        card = card
            .child(action(
                "track-ungroup",
                Icon::Extract,
                "Ungroup Tracks",
                keys::action_hint("UngroupTracks"),
                false,
                cx,
                |this, _, cx| {
                    this.s.ed.area = Area::Arrange;
                    crate::editing::ungroup_selection(this, cx);
                },
            ))
            .child(command(
                "track-fold",
                if folded {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                },
                if folded { "Unfold Group" } else { "Fold Group" },
                Cmd::FoldTrack,
                cx,
            ));
    }
    card.child(widgets::divider_h())
        .child(
            item(
                "track-rename",
                Icon::Pencil,
                "Rename",
                keys::hint(Cmd::Rename),
                false,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.menu = None;
                crate::commands::open_rename_for(
                    this,
                    Some(crate::commands::RenameTarget::Track(track)),
                    window,
                    cx,
                );
            })),
        )
        .child(widgets::divider_h())
        .child(action(
            "track-audio",
            Icon::Waveform,
            "Insert Audio Track",
            keys::action_hint("AddAudioTrack"),
            false,
            cx,
            |this, _, _| {
                this.s.add_track(TrackKind::Audio);
            },
        ))
        .child(action(
            "track-midi",
            Icon::Piano,
            "Insert MIDI Track",
            keys::action_hint("AddMidiTrack"),
            false,
            cx,
            |this, _, _| {
                this.s.add_track(TrackKind::Instrument);
            },
        ))
        .into_any_element()
}

/// Right-click on a device's title bar: it acts on the picked devices,
/// which the device under the mouse joined when the menu opened.
pub fn device_menu(
    ws: &mut Workspace,
    device: DeviceId,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let picked = ws.s.selected_devices();
    let ungroup = ws.s.can_ungroup_devices();
    let folded = ws.s.project.device(device).is_some_and(|d| d.collapsed);
    let mut card = widgets::menu_card("device-menu", 232.).child(action(
        "device-group",
        Icon::Grid,
        if picked.len() > 1 {
            "Group Devices"
        } else {
            "Group"
        },
        keys::action_hint("GroupTracks"),
        false,
        cx,
        |this, _, cx| {
            this.s.ed.area = Area::Devices;
            crate::editing::group_selection(this, cx);
        },
    ));
    if ungroup {
        card = card.child(action(
            "device-ungroup",
            Icon::Extract,
            "Ungroup",
            keys::action_hint("UngroupTracks"),
            false,
            cx,
            |this, _, cx| {
                this.s.ed.area = Area::Devices;
                crate::editing::ungroup_selection(this, cx);
            },
        ));
    }
    card.child(widgets::divider_h())
        .child(action(
            "device-fold",
            if folded {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            },
            if folded { "Show" } else { "Fold" },
            None,
            false,
            cx,
            move |this, _, _| {
                crate::views::devices::toggle_fold(this, device);
            },
        ))
        .child(action(
            "device-delete",
            Icon::Trash,
            if picked.len() > 1 {
                "Delete Devices"
            } else {
                "Delete"
            },
            None,
            false,
            cx,
            move |this, _, _| {
                let ids = this.s.selected_devices();
                this.s
                    .remove_devices(if ids.is_empty() { vec![device] } else { ids });
            },
        ))
        .into_any_element()
}
