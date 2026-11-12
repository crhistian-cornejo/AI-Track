//! What each command of the shortcut table does. The keys, the context
//! menus and the shortcuts sheet all end up in [`run`], so a command behaves
//! the same wherever it is triggered from.

use crate::editing::{Area, ConsolidateJob, Consolidation, GridView};
use crate::keys::Cmd;
use crate::state::{MainView, Snap};
use crate::views::prefs::Modal;
use crate::workspace::Workspace;
use aitrack_model::*;
use crate::icons::Icon;
use crate::widgets::TextInput;
use gpui::{AppContext as _, Context, Entity, Focusable, Subscription, Window};

/// What Rename edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameTarget {
    Clip(ClipId),
    Track(TrackId),
}

/// View state of the shortcuts sheet and the Rename field.
pub struct EditUi {
    /// The search field of the shortcuts sheet.
    pub shortcuts: Entity<TextInput>,
    pub rename: Entity<TextInput>,
    pub rename_target: Option<RenameTarget>,
    _subs: Vec<Subscription>,
}

impl EditUi {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        let shortcuts = cx.new(|cx| TextInput::new(cx, "Search commands and keys…", Some(Icon::Search)));
        let rename = cx.new(|cx| TextInput::new(cx, "Name", Some(Icon::Pencil)));
        // The sheet filters as you type.
        let subs = vec![cx.observe(&shortcuts, |_, _, cx| cx.notify())];
        Self { shortcuts, rename, rename_target: None, _subs: subs }
    }
}

pub fn run(ws: &mut Workspace, cmd: Cmd, window: &mut Window, cx: &mut Context<Workspace>) {
    // While musical typing is on, its letter keys (S, O…) play notes: the
    // key goes on to the piano roll instead of running the command.
    let typing_notes = ws.piano_keyboard && ws.bottom_open && ws.s.bottom_tab == crate::state::BottomTab::PianoRoll && ws.modal.is_none();
    if typing_notes && crate::keys::is_note_key(cmd) {
        cx.propagate();
        return;
    }
    match cmd {
        Cmd::Copy => {
            let what = if ws.s.copy() { describe(ws) } else { "Nothing selected to copy".into() };
            ws.toast(what, cx);
        }
        Cmd::Cut => {
            if !ws.s.cut() {
                ws.toast("Nothing selected to cut", cx);
            }
        }
        Cmd::Paste => {
            if !ws.s.paste() {
                let why = match &ws.s.ed.clipboard {
                    None => "The clipboard is empty: copy a clip or notes first",
                    Some(crate::editing::Clipboard::Notes { .. }) => "Open a MIDI clip to paste the notes into",
                    Some(crate::editing::Clipboard::Clips { .. }) => "These clips do not fit the selected track: audio goes on audio tracks, MIDI on MIDI tracks",
                };
                ws.toast(why, cx);
            } else if ws.s.ed.area == Area::Arrange {
                reveal_marker(ws);
            }
        }
        Cmd::Consolidate => match ws.s.consolidate() {
            Ok(Consolidation::Done(n)) => consolidated(ws, n, cx),
            Ok(Consolidation::Render(job)) => render_consolidate(ws, *job, cx),
            Err(why) => ws.toast(why, cx),
        },
        Cmd::Rename => open_rename(ws, window, cx),
        Cmd::DuplicateTime => {
            if !ws.s.duplicate_time() {
                ws.toast("Drag in an empty lane to select the time to duplicate", cx);
            }
        }
        Cmd::InsertSilence => {
            if !ws.s.insert_silence() {
                ws.toast("Drag in an empty lane to select how much silence to insert", cx);
            }
        }
        Cmd::Quantize => ws.quantize_clip(cx),
        Cmd::InsertMidiClip => {
            if ws.s.insert_midi_clip().is_none() {
                ws.toast("Select a MIDI track to insert a clip", cx);
            }
        }
        Cmd::NarrowGrid | Cmd::WidenGrid => {
            ws.s.step_grid(if cmd == Cmd::NarrowGrid { -1 } else { 1 });
            ws.toast(format!("Grid: {}", grid_label(ws)), cx);
        }
        Cmd::TripletGrid => {
            let on = ws.s.toggle_triplet();
            ws.toast(format!("{} grid: {}", if on { "Triplet" } else { "Straight" }, grid_label(ws)), cx);
        }
        Cmd::ToggleSnap => {
            let on = ws.s.toggle_snap();
            ws.toast(if on { format!("Snap on: {}", grid_label(ws)) } else { "Snap off".into() }, cx);
        }
        Cmd::Shortcuts => {
            if ws.modal == Some(Modal::Shortcuts) {
                ws.close_modal(cx);
                ws.focus.focus(window, cx);
            } else {
                ws.ui.shortcuts.update(cx, |input, cx| input.set_text("", cx));
                ws.open_modal(Modal::Shortcuts, cx);
                let focus = ws.ui.shortcuts.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
        }
        Cmd::Continue => {
            if ws.b.engine.transport().playing {
                ws.b.engine.stop();
            } else {
                ws.b.engine.play();
            }
        }
        Cmd::GoToStart => {
            ws.s.set_marker(0, None);
            ws.s.arrange.scroll_x = 0.;
            ws.b.engine.locate(0);
        }
        Cmd::Metronome => {
            let on = !ws.b.engine.transport().metronome;
            ws.b.engine.set_metronome(on);
            ws.toast(if on { "Metronome on" } else { "Metronome off" }, cx);
        }
        Cmd::FocusSearch => crate::views::palette::open(ws, window, cx),
        Cmd::Fullscreen => window.toggle_fullscreen(),
        Cmd::ZoomToSelection => match ws.s.selection_span().filter(|(a, b)| b > a) {
            Some((start, end)) => {
                let width = f32::from(ws.regions.arrange.get().size.width);
                ws.s.ed.zoom_stack.push(ws.s.arrange);
                ws.s.arrange = crate::nav::zoom_to(ws.s.arrange, start, end, width);
            }
            None => ws.toast("Select clips or a time range to zoom to", cx),
        },
        Cmd::ZoomBack => {
            if let Some(view) = ws.s.ed.zoom_stack.pop() {
                ws.s.arrange = view;
            }
        }
        Cmd::Follow => {
            ws.s.ed.follow = !ws.s.ed.follow;
            ws.toast(if ws.s.ed.follow { "Follow playhead on" } else { "Follow playhead off" }, cx);
        }
        Cmd::FoldTrack => match ws.s.toggle_fold() {
            Some(_) => {}
            None => ws.toast("Only groups fold: select a group or a track inside one", cx),
        },
        Cmd::ArmTrack => {
            if ws.s.toggle_arm().is_none() {
                ws.toast("Select an audio or MIDI track to arm", cx);
            }
        }
        Cmd::Track1 | Cmd::Track2 | Cmd::Track3 | Cmd::Track4 | Cmd::Track5 | Cmd::Track6 | Cmd::Track7 | Cmd::Track8 => {
            let n = [Cmd::Track1, Cmd::Track2, Cmd::Track3, Cmd::Track4, Cmd::Track5, Cmd::Track6, Cmd::Track7, Cmd::Track8]
                .iter()
                .position(|c| *c == cmd)
                .unwrap_or(0);
            if let Some((name, on)) = ws.s.toggle_activator(n) {
                ws.toast(format!("{name} {}", if on { "on" } else { "off" }), cx);
            }
        }
    }
    cx.notify();
}

/// The grid the keys just changed: the arrangement's, or the piano roll's
/// when it has the keys.
fn grid_label(ws: &Workspace) -> String {
    let view = if ws.s.ed.area == Area::Piano { GridView::Piano } else { GridView::Arrange };
    match ws.s.snap {
        Snap::Off => format!("{} (snap off)", ws.s.grid_spec(view).label()),
        _ => ws.s.snap_label(view),
    }
}

/// What Copy just put on the clipboard.
fn describe(ws: &Workspace) -> String {
    match &ws.s.ed.clipboard {
        Some(crate::editing::Clipboard::Notes { notes, .. }) => format!("Copied {} note{}", notes.len(), if notes.len() == 1 { "" } else { "s" }),
        Some(crate::editing::Clipboard::Clips { rows, .. }) => {
            let n: usize = rows.iter().map(|r| r.seeds.len()).sum();
            let key = crate::keys::hint(Cmd::Paste).unwrap_or_default();
            format!("Copied {n} clip{} — {key} pastes at the marker", if n == 1 { "" } else { "s" })
        }
        None => "Nothing copied".into(),
    }
}

/// Scroll the arrangement so the insert marker is on screen (after a paste
/// moved it past the right edge).
fn reveal_marker(ws: &mut Workspace) {
    if ws.s.view != MainView::Arrangement {
        return;
    }
    let width = f32::from(ws.regions.arrange.get().size.width);
    if let Some(scroll) = crate::nav::follow(ws.s.arrange, ws.s.ed.marker, width) {
        ws.s.arrange.scroll_x = scroll;
    }
}

/// Ctrl+R: rename the selected clip when the arrangement has one selected,
/// else the selected track.
pub fn open_rename(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let clip = ws.s.clip_selection().first().map(|(_, pl, _)| pl.clip);
    let target = clip
        .map(RenameTarget::Clip)
        .or(ws.s.selected_track.map(RenameTarget::Track));
    open_rename_for(ws, target, window, cx);
}

/// The Rename field for `target` (a track's context menu names it directly).
pub fn open_rename_for(
    ws: &mut Workspace,
    target: Option<RenameTarget>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let name = match target {
        Some(RenameTarget::Clip(c)) => ws.s.project.clips.get(&c).map(|c| c.name.clone()),
        Some(RenameTarget::Track(t)) => ws.s.project.track(t).map(|t| t.name.clone()),
        None => None,
    };
    let (Some(target), Some(name)) = (target, name) else {
        ws.toast("Select a clip or a track to rename", cx);
        return;
    };
    ws.ui.rename_target = Some(target);
    ws.ui.rename.update(cx, |input, cx| input.set_text(&name, cx));
    ws.open_modal(Modal::Rename, cx);
    let focus = ws.ui.rename.read(cx).focus_handle(cx);
    focus.focus(window, cx);
}

/// Enter in the Rename field: apply the name and hand the keys back.
pub fn apply_rename(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let name = ws.ui.rename.read(cx).text().to_owned();
    match ws.ui.rename_target.take() {
        Some(RenameTarget::Clip(c)) => ws.s.rename_clip(c, &name),
        Some(RenameTarget::Track(t)) => ws.s.rename_track(t, &name),
        None => false,
    };
    ws.modal = None;
    ws.focus.focus(window, cx);
    cx.notify();
}

fn consolidated(ws: &mut Workspace, n: usize, cx: &mut Context<Workspace>) {
    ws.toast(if n == 1 { "Consolidated into one clip".to_owned() } else { format!("Consolidated into {n} clips") }, cx);
}

/// One audio file a Consolidate renders.
struct Render {
    group: usize,
    parts: Vec<PlacementId>,
    path: std::path::PathBuf,
    rate: u32,
}

/// Render the audio of a Consolidate off the UI thread (each track's clips
/// to a new WAV file at the highest rate among them), then apply it as one
/// undo step. The song is left alone when a render fails.
fn render_consolidate(ws: &mut Workspace, job: ConsolidateJob, cx: &mut Context<Workspace>) {
    let dir = crate::project_io::consolidate_folder(ws.project_path.as_deref());
    let mut work = Vec::new();
    for (i, g) in job.groups.iter().enumerate().filter(|(_, g)| g.audio) {
        let p = &job.project;
        let rate = g
            .parts
            .iter()
            .filter_map(|pl| match p.sources.get(&p.clips.get(&pl.clip)?.source) {
                Some(Source::Audio(a)) => Some(a.sample_rate),
                _ => None,
            })
            .max()
            .unwrap_or(p.sample_rate_hint)
            .max(8_000);
        let name = g.parts.first().and_then(|pl| p.clips.get(&pl.clip)).map_or("Audio", |c| c.name.as_str());
        // Two tracks in one go must not pick the same free name.
        let taken: Vec<&std::path::Path> = work.iter().map(|w: &Render| w.path.as_path()).collect();
        let path = crate::project_io::fresh_wav(&dir, name, &taken);
        work.push(Render { group: i, parts: g.parts.iter().map(|pl| pl.id).collect(), path, rate });
    }
    ws.toast("Rendering the audio clips…", cx);
    let project = job.project.clone();
    let tracks: Vec<TrackId> = job.groups.iter().map(|g| g.track).collect();
    let ranges: Vec<(Tick, Tick)> = job.groups.iter().map(|g| (g.start, g.end)).collect();
    let task = cx.background_executor().spawn(async move {
        let mut files = Vec::with_capacity(work.len());
        for w in work {
            let (track, range) = (tracks[w.group], ranges[w.group]);
            let file = aitrack_engine::consolidate_to_wav(&project, track, &w.parts, range, w.rate, &w.path, None).map_err(|e| e.to_string())?;
            files.push((w.group, file));
        }
        Ok::<_, String>(files)
    });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |ws, cx| {
            match result {
                Ok(files) => match ws.s.finish_consolidate(&job, &files) {
                    Ok(n) => consolidated(ws, n, cx),
                    Err(why) => ws.toast(why, cx),
                },
                Err(e) => ws.toast(format!("Consolidate failed: {e}"), cx),
            }
            cx.notify();
        });
    })
    .detach();
}
