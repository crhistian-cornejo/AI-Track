//! The arrangement's context menus: one for clips, one for empty lane
//! space. Every item shows the shortcut the key table gives it, and runs
//! the same code as the key.

use crate::editing::Area;
use crate::icons::Icon;
use crate::keys::{self, Cmd};
use crate::theme::*;
use crate::widgets::{self, menu_item, mono};
use crate::workspace::{MenuKind, Workspace};
use aitrack_model::{Color, DeviceId, TrackId, TrackKind};
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

/// The palette grid (hue columns by lightness rows); `current` is ringed.
/// Picking one closes the menu and runs `pick`.
fn swatches(id: &'static str, current: Option<Color>, cx: &mut Context<Workspace>, pick: impl Fn(&mut Workspace, Color, &mut Context<Workspace>) + Clone + 'static) -> gpui::Div {
    use gpui::{div, px, InteractiveElement, Styled};
    let mut grid = div().px(px(SP2)).py(px(SP1)).flex().flex_col().gap(px(SP1));
    for row in 0..aitrack_model::colors::GRID_ROWS {
        let mut r = div().flex().gap(px(SP1));
        for col in 0..aitrack_model::colors::GRID_COLS {
            let color = Color::grid(row, col);
            let pick = pick.clone();
            r = r.child(
                div()
                    .id(ElementId::Name(format!("{id}-{row}-{col}").into()))
                    .w(px(12.))
                    .h(px(12.))
                    .rounded(px(r_sm()))
                    .bg(track_color(color))
                    .border_1()
                    .border_color(if current == Some(color) { p().text } else { border() })
                    .cursor_pointer()
                    .hover(|s| s.border_color(p().text))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menu = None;
                        pick(this, color, cx);
                        cx.notify();
                    })),
            );
        }
        grid = grid.child(r);
    }
    grid
}

/// A seed for "Randomize Colors" that differs on every use.
fn color_seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x5EED, |d| d.as_nanos() as u64)
}

/// Right-click on a clip. It acts on the selected clips (the clip under
/// the mouse joined the selection when the menu opened).
pub fn clip_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let clips = ws.s.clip_selection();
    let state = |f: fn(&aitrack_model::Clip) -> bool| clips.iter().filter_map(|(_, pl, _)| ws.s.project.clips.get(&pl.clip)).any(f);
    let looping = state(|c| c.looping.is_some());
    let muted = !clips.is_empty() && !state(|c| !c.muted);
    // The clip under the mouse, when it is audio and other clips play its file too.
    let clip_color = clips.first().and_then(|(_, pl, _)| ws.s.project.clips.get(&pl.clip)).map(|c| c.color).filter(|c| clips.iter().all(|(_, pl, _)| ws.s.project.clips.get(&pl.clip).is_some_and(|x| x.color == *c)));
    let sample = ws.s.selected_placement.and_then(|(_, id)| ws.s.placement(id)).map(|(_, pl, _)| pl.clip).filter(|c| ws.s.sample_targets(*c).len() > 1);
    let card = widgets::menu_card("clip-menu", 240.)
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
        .child(swatches("clip-color", clip_color, cx, |this, color, _| {
            this.s.set_clips_color(color);
        }))
        .child(action("clip-track-color", Icon::Wand, "Assign Track Color to Clips", None, false, cx, |this, _, _| {
            let tracks: Vec<TrackId> = this.s.clip_selection().iter().map(|(t, _, _)| *t).collect();
            this.s.assign_track_color_to_clips(&tracks);
        }))
        .child(action("clip-randomize", Icon::Shuffle, "Randomize Colors", None, false, cx, |this, _, _| {
            this.s.randomize_clip_colors(color_seed());
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
    let sliceable = crate::editing::slice::target(&ws.s);
    let card = match sliceable {
        Some((track, placement)) if clips.len() == 1 => card.child(widgets::divider_h()).child(
            item("clip-slice", Icon::Drum, "Slice to Drum Rack…", None, false).on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                let at = e.position();
                this.menu = None;
                this.open_menu(MenuKind::SliceToDrumRack(track, placement), gpui::point(at.x, at.y), cx);
            })),
        ),
        _ => card,
    };
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
    let mut card = widgets::menu_card("track-menu", 240.);
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
    if t.kind.has_clips() {
        let shown = ws.s.take_lanes_shown(track);
        card = card.child(widgets::divider_h()).child(
            item("track-takes", Icon::Layers, "Show Take Lanes", keys::hint(Cmd::ShowTakeLanes), shown).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.menu = None;
                this.s.toggle_take_lanes(track);
                cx.notify();
            })),
        );
        if shown {
            card = card.child(action("track-insert-take", Icon::Add, "Insert Take Lane", None, false, cx, move |this, _, _| {
                this.s.insert_take_lane(track);
            }));
        }
    }
    // Freeze (Live): render the track so its devices stop running; Flatten makes the render audio.
    let selection = ws.s.selected_tracks();
    let (can_freeze, can_unfreeze) = (!ws.s.freezable(&selection).is_empty(), !ws.s.frozen_among(&selection).is_empty());
    if can_freeze || can_unfreeze {
        card = card.child(widgets::divider_h());
    }
    if can_freeze {
        card = card.child(action("track-freeze", Icon::Snowflake, if many { "Freeze Tracks" } else { "Freeze Track" }, None, false, cx, |this, _, cx| this.freeze_selected(cx)));
    }
    if can_unfreeze {
        card = card
            .child(action("track-unfreeze", Icon::Snowflake, if many { "Unfreeze Tracks" } else { "Unfreeze Track" }, None, true, cx, |this, _, cx| this.unfreeze_selected(cx)))
            .child(action("track-flatten", Icon::Waveform, "Flatten", None, false, cx, |this, _, cx| this.flatten_selected(cx)));
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
        .child(swatches("track-color", Some(t.color), cx, move |this, color, _| {
            let ids = this.s.selected_tracks();
            this.s.set_tracks_color(&ids, color);
        }))
        .child(action("track-assign-color", Icon::Wand, "Assign Track Color to Clips", None, false, cx, |this, _, _| {
            let ids = this.s.selected_tracks();
            this.s.assign_track_color_to_clips(&ids);
        }))
        .child(action("track-randomize", Icon::Shuffle, "Randomize Colors", None, false, cx, |this, _, _| {
            let ids = this.s.selected_tracks();
            this.s.randomize_track_colors(&ids, color_seed());
        }))
        .child(action(
            "track-duplicate",
            Icon::Layers,
            if many { "Duplicate Tracks" } else { "Duplicate Track" },
            keys::action_hint("Duplicate"),
            false,
            cx,
            |this, _, _| {
                let ids = this.s.selected_tracks();
                this.s.duplicate_tracks(&ids);
            },
        ))
        .child(action(
            "track-delete",
            Icon::Trash,
            if many { "Delete Tracks" } else { "Delete Track" },
            keys::action_hint("DeleteTrack"),
            false,
            cx,
            |this, _, cx| this.delete_selected_tracks(cx),
        ))
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

/// Right-click on a take lane's header: rename, reorder, insert and delete lanes.
pub fn take_lane_menu(ws: &mut Workspace, track: TrackId, lane: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let n = ws.s.project.track(track).map_or(0, |t| t.take_lanes.len());
    let mut card = widgets::menu_card("take-lane-menu", 232.);
    card = card
        .child(item("take-rename", Icon::Pencil, "Rename Take Lane", None, false).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.menu = None;
            crate::commands::open_rename_for(this, Some(crate::commands::RenameTarget::TakeLane(track, lane)), window, cx);
        })))
        .child(action("take-insert", Icon::Add, "Insert Take Lane", None, false, cx, move |this, _, _| {
            this.s.insert_take_lane(track);
        }));
    if lane > 0 {
        card = card.child(action("take-up", Icon::ChevronUp, "Move Up", None, false, cx, move |this, _, _| {
            this.s.shift_take_lane(track, lane, -1);
        }));
    }
    if lane + 1 < n {
        card = card.child(action("take-down", Icon::ChevronDown, "Move Down", None, false, cx, move |this, _, _| {
            this.s.shift_take_lane(track, lane, 1);
        }));
    }
    card.child(widgets::divider_h())
        .child(action("take-hide", Icon::Minus, "Hide Take Lanes", keys::hint(Cmd::ShowTakeLanes), false, cx, move |this, _, _| {
            if this.s.take_lanes_shown(track) {
                this.s.toggle_take_lanes(track);
            }
        }))
        .child(action("take-delete", Icon::Trash, "Delete Take Lane", None, false, cx, move |this, _, _| {
            this.s.delete_take_lane(track, lane);
        }))
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
