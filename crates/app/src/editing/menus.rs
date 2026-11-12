//! The arrangement's context menus: one for clips, one for empty lane
//! space. Every item shows the shortcut the key table gives it, and runs
//! the same code as the key.

use crate::icons::Icon;
use crate::keys::{self, Cmd};
use crate::theme::*;
use crate::widgets::{self, menu_item, mono};
use crate::workspace::Workspace;
use aitrack_model::TrackKind;
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
    widgets::menu_card("clip-menu", 232.)
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
        .child(command("clip-rename", Icon::Pencil, "Rename", Cmd::Rename, cx))
        .child(action("clip-active", Icon::Power, if muted { "Activate" } else { "Deactivate" }, keys::action_hint("ToggleActive"), false, cx, |this, _, _| {
            this.s.toggle_clips_active();
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
