//! Every keyboard shortcut in one table: its name, help text, section and
//! keys. The shortcuts sheet and the hints in context menus read it, so they
//! show what is really bound.
//!
//! The table has two kinds of rows. [`Bound::Action`] rows describe a binding
//! made in `actions.rs` (a test reads that file and fails when the two
//! disagree). [`Bound::Cmd`] rows are bound here, from the table itself, and
//! run through [`crate::commands::run`].
//!
//! Keys use GPUI's keystroke syntax, with `mod` for Cmd on macOS and Ctrl
//! elsewhere.

use crate::workspace::Workspace;
use gpui::{App, Context, Div, InteractiveElement, KeyBinding, Stateful};

/// Where a binding is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Everywhere in the app.
    App,
    /// In the workspace, also while typing in a text field.
    Workspace,
    /// In the workspace, but not while a text field has the keys.
    Keys,
}

impl Scope {
    fn context(self) -> Option<&'static str> {
        match self {
            Scope::App => None,
            Scope::Workspace => Some("Workspace"),
            Scope::Keys => Some("Workspace && !TextInput"),
        }
    }
}

/// Sections of the shortcuts sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Transport,
    Editing,
    Notes,
    Tracks,
    Grid,
    Zoom,
    Views,
    Project,
}

impl Category {
    pub const ALL: [Category; 8] =
        [Self::Transport, Self::Editing, Self::Notes, Self::Tracks, Self::Grid, Self::Zoom, Self::Views, Self::Project];

    pub fn label(self) -> &'static str {
        match self {
            Self::Transport => "Transport",
            Self::Editing => "Editing",
            Self::Notes => "Notes",
            Self::Tracks => "Tracks",
            Self::Grid => "Grid and tools",
            Self::Zoom => "Zoom and navigation",
            Self::Views => "Views",
            Self::Project => "Project",
        }
    }
}

/// What a row's keys trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// The action of this name, bound in `actions.rs`.
    Action(&'static str),
    /// A command bound from this table.
    Cmd(Cmd),
}

pub struct Entry {
    pub label: &'static str,
    /// One line for the shortcuts sheet.
    pub help: &'static str,
    pub category: Category,
    /// The first key is the one menus show.
    pub keys: &'static [&'static str],
    pub scope: Scope,
    pub bound: Bound,
}

/// Declares the commands bound from the table: the [`Cmd`] enum, one GPUI
/// action per command, and the glue that binds and dispatches them.
macro_rules! commands {
    ($($name:ident,)*) => {
        /// A command bound from the table (see [`crate::commands::run`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Cmd { $($name,)* }

        mod act {
            gpui::actions!(aitrack_edit, [$($name,)*]);
        }

        fn binding(cmd: Cmd, key: &str, context: Option<&str>) -> KeyBinding {
            match cmd { $(Cmd::$name => KeyBinding::new(key, act::$name, context),)* }
        }

        /// Route every command's action on the workspace root to [`crate::commands::run`].
        pub fn register(mut root: Stateful<Div>, cx: &mut Context<Workspace>) -> Stateful<Div> {
            $(root = root.on_action(cx.listener(|ws, _: &act::$name, window, cx| crate::commands::run(ws, Cmd::$name, window, cx)));)*
            root
        }
    };
}

commands! {
    Copy,
    Cut,
    Paste,
    Consolidate,
    Rename,
    DuplicateTime,
    InsertSilence,
    Quantize,
    InsertMidiClip,
    NarrowGrid,
    WidenGrid,
    TripletGrid,
    ToggleSnap,
    Shortcuts,
    Continue,
    GoToStart,
    Metronome,
    FocusSearch,
    Fullscreen,
    ZoomToSelection,
    ZoomBack,
    Follow,
    FoldTrack,
    ArmTrack,
    ComputerKeyboard,
    Track1,
    Track2,
    Track3,
    Track4,
    Track5,
    Track6,
    Track7,
    Track8,
}

const fn main(action: &'static str, label: &'static str, help: &'static str, category: Category, scope: Scope, keys: &'static [&'static str]) -> Entry {
    Entry { label, help, category, keys, scope, bound: Bound::Action(action) }
}

const fn cmd(cmd: Cmd, label: &'static str, help: &'static str, category: Category, keys: &'static [&'static str]) -> Entry {
    Entry { label, help, category, keys, scope: Scope::Keys, bound: Bound::Cmd(cmd) }
}

use Category as C;
use Scope::{Keys as K, Workspace as W};

pub static TABLE: &[Entry] = &[
    // Transport
    main("PlayStop", "Play / Stop", "Play from the insert marker, or stop.", C::Transport, K, &["space"]),
    cmd(Cmd::Continue, "Continue", "Play on from where playback stopped.", C::Transport, &["shift-space"]),
    cmd(Cmd::GoToStart, "Go to Start", "Move the insert marker and the playhead to the start of the song.", C::Transport, &["home"]),
    main("ToggleLoop", "Loop Selection", "Put the loop brace on the selection and switch the loop on; pressed again, or with nothing selected, switch the loop on or off.", C::Transport, W, &["mod-l"]),
    cmd(Cmd::Metronome, "Metronome", "Switch the metronome click on or off.", C::Transport, &["o"]),
    cmd(Cmd::Follow, "Follow Playhead", "Scroll the arrangement so the playhead stays in view.", C::Transport, &["mod-alt-f"]),
    // Editing
    main("Undo", "Undo", "Undo the last edit.", C::Editing, W, &["mod-z"]),
    main("Redo", "Redo", "Redo the edit that was undone.", C::Editing, W, &["mod-shift-z", "mod-y"]),
    cmd(Cmd::Cut, "Cut", "Cut the time selection, the selected clips or the selected notes.", C::Editing, &["mod-x"]),
    cmd(Cmd::Copy, "Copy", "Copy the time selection, the selected clips or the selected notes.", C::Editing, &["mod-c"]),
    cmd(Cmd::Paste, "Paste", "Paste at the insert marker; the marker moves to the end, so pasting again continues from there.", C::Editing, &["mod-v"]),
    main("Duplicate", "Duplicate", "Repeat the selection right after itself; the copy becomes the selection.", C::Editing, K, &["mod-d"]),
    main("DeleteSelection", "Delete", "Delete the time selection, the selected clips or the selected notes.", C::Editing, K, &["delete", "backspace"]),
    main("SelectAll", "Select All", "Select every clip, or every note when the piano roll has the keys.", C::Editing, K, &["mod-a"]),
    main("SplitClip", "Split", "Cut clips at the insert marker (at the playhead while playing), or at both edges of the time selection.", C::Editing, K, &["mod-e"]),
    cmd(Cmd::Consolidate, "Consolidate", "Join the selected clips into one clip per track, gaps included; audio is rendered to a new file.", C::Editing, &["mod-j"]),
    cmd(Cmd::Rename, "Rename", "Rename the selected clip, or the selected track.", C::Editing, &["mod-r"]),
    main("ToggleActive", "Deactivate", "Switch the selected notes, clip or track off or back on.", C::Editing, K, &["0"]),
    cmd(Cmd::DuplicateTime, "Duplicate Time", "Repeat the selected time on every track, pushing what follows later.", C::Editing, &["mod-shift-d"]),
    cmd(Cmd::InsertSilence, "Insert Silence", "Insert empty time the length of the time selection on every track.", C::Editing, &["mod-shift-i"]),
    cmd(Cmd::InsertMidiClip, "Insert MIDI Clip", "An empty MIDI clip in the time selection, or one bar at the insert marker.", C::Editing, &["mod-shift-m"]),
    main("NudgeLeft", "Nudge Left", "Move the selected clips or notes one grid step earlier (or the insert marker).", C::Editing, K, &["left"]),
    main("NudgeRight", "Nudge Right", "Move the selected clips or notes one grid step later (or the insert marker).", C::Editing, K, &["right"]),
    main("Escape", "Close / Deselect", "Close the open menu or dialog and clear the selection.", C::Editing, K, &["escape"]),
    // Notes
    main("NudgeUp", "Up", "Move the selected notes up one step of the scale; in the arrangement, select the track above.", C::Notes, K, &["up"]),
    main("NudgeDown", "Down", "Move the selected notes down one step of the scale; in the arrangement, select the track below.", C::Notes, K, &["down"]),
    main("SemitoneUp", "Semitone Up", "Move the selected notes up a semitone.", C::Notes, K, &["alt-up"]),
    main("SemitoneDown", "Semitone Down", "Move the selected notes down a semitone.", C::Notes, K, &["alt-down"]),
    main("OctaveUp", "Octave Up", "Move the selected notes up an octave.", C::Notes, K, &["shift-up"]),
    main("OctaveDown", "Octave Down", "Move the selected notes down an octave.", C::Notes, K, &["shift-down"]),
    main("Longer", "Longer", "Lengthen the selected notes by a grid step.", C::Notes, K, &["shift-right"]),
    main("Shorter", "Shorter", "Shorten the selected notes by a grid step.", C::Notes, K, &["shift-left"]),
    main("VelocityUp", "Velocity Up", "Raise the velocity of the selected notes.", C::Notes, K, &["mod-up"]),
    main("VelocityDown", "Velocity Down", "Lower the velocity of the selected notes.", C::Notes, K, &["mod-down"]),
    cmd(Cmd::Quantize, "Quantize", "Move note starts to the grid (the selection, or the whole clip).", C::Notes, &["mod-u"]),
    // Tracks
    main("AddAudioTrack", "Insert Audio Track", "Add an audio track below the selected track.", C::Tracks, W, &["mod-t"]),
    main("AddMidiTrack", "Insert MIDI Track", "Add a MIDI track below the selected track.", C::Tracks, W, &["mod-shift-t"]),
    main("AddReturnTrack", "Insert Return Track", "Add a return track fed by sends.", C::Tracks, W, &["mod-alt-t"]),
    main(
        "GroupTracks",
        "Group Track",
        "Put the selected tracks in a new group; in the Device View, the picked devices in a new rack.",
        C::Tracks,
        K,
        &["mod-g"],
    ),
    main(
        "UngroupTracks",
        "Ungroup Tracks",
        "Dissolve the selected group; in the Device View, the picked rack.",
        C::Tracks,
        K,
        &["mod-shift-g"],
    ),
    cmd(Cmd::FoldTrack, "Fold / Unfold Group", "Fold the selected group, or the group the selected track is in.", C::Tracks, &["s"]),
    cmd(Cmd::ArmTrack, "Arm", "Arm the selected track for recording.", C::Tracks, &["c"]),
    cmd(Cmd::ComputerKeyboard, "Computer MIDI Keyboard", "Play instruments with your PC/Mac keyboard (Z/X octave, C/V velocity).", C::Tracks, &["m"]),
    main("ToggleAutomationMode", "Show Automation", "Show or hide the automation lanes of the selected tracks.", C::Tracks, K, &["a"]),
    main("ToggleAutomationArm", "Automation Arm", "While recording, moving a control in a plugin's window writes its automation.", C::Transport, W, &[]),
    cmd(Cmd::Track1, "Track 1 Activator", "Switch the first track on or off.", C::Tracks, &["f1"]),
    cmd(Cmd::Track2, "Track 2 Activator", "Switch the second track on or off.", C::Tracks, &["f2"]),
    cmd(Cmd::Track3, "Track 3 Activator", "Switch the third track on or off.", C::Tracks, &["f3"]),
    cmd(Cmd::Track4, "Track 4 Activator", "Switch the fourth track on or off.", C::Tracks, &["f4"]),
    cmd(Cmd::Track5, "Track 5 Activator", "Switch the fifth track on or off.", C::Tracks, &["f5"]),
    cmd(Cmd::Track6, "Track 6 Activator", "Switch the sixth track on or off.", C::Tracks, &["f6"]),
    cmd(Cmd::Track7, "Track 7 Activator", "Switch the seventh track on or off.", C::Tracks, &["f7"]),
    cmd(Cmd::Track8, "Track 8 Activator", "Switch the eighth track on or off.", C::Tracks, &["f8"]),
    // Grid and tools
    main("ToggleDrawTool", "Draw Tool", "Switch between the pencil and the selection tool in the piano roll.", C::Grid, K, &["b"]),
    cmd(Cmd::NarrowGrid, "Narrow Grid", "Make the grid finer.", C::Grid, &["mod-1"]),
    cmd(Cmd::WidenGrid, "Widen Grid", "Make the grid coarser.", C::Grid, &["mod-2"]),
    cmd(Cmd::TripletGrid, "Triplet Grid", "Switch the grid between straight and triplet steps.", C::Grid, &["mod-3"]),
    cmd(Cmd::ToggleSnap, "Snap to Grid", "Switch snapping off or back on. Holding Alt while dragging also bypasses it.", C::Grid, &["mod-4"]),
    // Zoom and navigation
    main("ArrangeZoomIn", "Zoom In", "Zoom the arrangement in.", C::Zoom, K, &["=", "+"]),
    main("ArrangeZoomOut", "Zoom Out", "Zoom the arrangement out.", C::Zoom, K, &["-"]),
    main("ArrangeFit", "Fit Song to Width", "Show the whole song.", C::Zoom, K, &["w"]),
    cmd(Cmd::ZoomToSelection, "Zoom to Selection", "Fill the arrangement with the selection.", C::Zoom, &["z"]),
    cmd(Cmd::ZoomBack, "Zoom Back", "Return to the view before the last Zoom to Selection.", C::Zoom, &["x"]),
    main("ZoomIn", "Interface Zoom In", "Make the whole interface larger.", C::Zoom, W, &["mod-=", "mod-+"]),
    main("ZoomOut", "Interface Zoom Out", "Make the whole interface smaller.", C::Zoom, W, &["mod--"]),
    main("ZoomReset", "Interface Zoom 100%", "Reset the interface zoom.", C::Zoom, W, &["mod-0"]),
    // Views
    main("ToggleView", "Session / Arrangement", "Switch between the Session and the Arrangement.", C::Views, K, &["tab"]),
    main("ToggleDetailView", "Devices / Clip", "Switch the detail panel between the device chain and the clip.", C::Views, K, &["shift-tab"]),
    main("TabDevices", "Devices", "Show the device chain in the detail panel.", C::Views, K, &["1"]),
    main("TabPianoRoll", "Clip", "Show the clip (piano roll or sample) in the detail panel.", C::Views, K, &["2"]),
    main("TabSteps", "Step Sequencer", "Show the step sequencer in the detail panel.", C::Views, K, &["3"]),
    main("TabMixer", "Mixer (key 4)", "Show or hide the mixer.", C::Views, K, &["4"]),
    main("ShowMixer", "Mixer", "Show or hide the mixer.", C::Views, W, &["mod-alt-m"]),
    main("ToggleDetail", "Detail Panel", "Show or hide the bottom detail panel.", C::Views, W, &["mod-alt-l"]),
    main("TogglePalette", "Search", "Search samples, devices, presets, packs and commands; arrows audition, Enter adds to the selected track.", C::Views, W, &["mod-k"]),
    main("AddLibraryFolder", "Add Library Folder", "Add a folder on this computer or Google Drive, Dropbox, OneDrive or iCloud Drive to the sample library.", C::Views, W, &[]),
    main("ToggleAssistant", "Assistant", "Show or hide the AI assistant.", C::Views, W, &["mod-i"]),
    cmd(Cmd::FocusSearch, "Find", "Open the search palette.", C::Views, &["mod-f"]),
    cmd(Cmd::Fullscreen, "Full Screen", "Switch full screen on or off.", C::Views, &["f11"]),
    cmd(Cmd::Shortcuts, "Keyboard Shortcuts", "Search every command and its shortcut.", C::Views, &["mod-/", "?", "shift-/"]),
    // Project
    main("NewProject", "New Project", "Start an empty project.", C::Project, W, &["mod-n"]),
    main("OpenProject", "Open Project…", "Open an .aitrack project file.", C::Project, W, &["mod-o"]),
    main("SaveProject", "Save", "Save the project.", C::Project, W, &["mod-s"]),
    main("SaveProjectAs", "Save As…", "Save the project to a new file.", C::Project, W, &["mod-shift-s"]),
    main("ExportAudio", "Export Audio…", "Render the song to an audio file.", C::Project, W, &["mod-shift-e"]),
    main("OpenPreferences", "Preferences…", "Audio device, appearance and plugins.", C::Project, W, &["mod-,"]),
    main("Quit", "Quit", "Close AI-Track.", C::Project, Scope::App, &["mod-q"]),
];

fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

/// Bind the table's own commands (the `actions.rs` rows are bound there).
pub fn bind(cx: &mut App) {
    let m = if is_mac() { "cmd" } else { "ctrl" };
    let mut bindings = Vec::new();
    for e in TABLE {
        if let Bound::Cmd(c) = e.bound {
            for key in e.keys {
                bindings.push(binding(c, &key.replace("mod", m), e.scope.context()));
            }
        }
    }
    cx.bind_keys(bindings);
}

/// Split a keystroke into its modifiers and key (`mod--` is Mod + minus).
fn parts(key: &str) -> (Vec<&str>, &str) {
    match key.rfind('-') {
        Some(i) if i + 1 < key.len() => (key[..i].split('-').collect(), &key[i + 1..]),
        Some(i) => (key[..i].split('-').filter(|m| !m.is_empty()).collect(), "-"),
        None => (Vec::new(), key),
    }
}

/// A keystroke as people write it: `mod-shift-d` is "Ctrl+Shift+D" (⇧⌘D on macOS).
pub fn display(key: &str) -> String {
    display_for(key, is_mac())
}

fn display_for(key: &str, mac: bool) -> String {
    let (mods, k) = parts(key);
    let name = match k {
        "space" => "Space".to_owned(),
        "delete" => "Del".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "escape" => "Esc".to_owned(),
        "tab" => "Tab".to_owned(),
        "home" => "Home".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        other => other.to_uppercase(),
    };
    let has = |m: &str| mods.contains(&m);
    if mac {
        let mut out = String::new();
        for (m, glyph) in [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("mod", "⌘")] {
            if has(m) {
                out.push_str(glyph);
            }
        }
        out + &name
    } else {
        let mut out = Vec::new();
        for (m, text) in [("mod", "Ctrl"), ("ctrl", "Ctrl"), ("alt", "Alt"), ("shift", "Shift")] {
            if has(m) {
                out.push(text.to_owned());
            }
        }
        out.push(name);
        out.join("+")
    }
}

impl Entry {
    /// The key menus show next to the command.
    pub fn hint(&self) -> Option<String> {
        self.keys.first().map(|k| display(k))
    }

    /// Every key, for the shortcuts sheet (`?` and `Shift+/` are one key).
    pub fn caps(&self) -> Vec<String> {
        self.keys.iter().filter(|k| **k != "shift-/").map(|k| display(k)).collect()
    }
}

/// The shortcut of one of the table's own commands.
pub fn hint(cmd: Cmd) -> Option<String> {
    TABLE.iter().find(|e| e.bound == Bound::Cmd(cmd)).and_then(Entry::hint)
}

/// Whether one of the command's keys is a note key of musical typing.
pub fn is_note_key(cmd: Cmd) -> bool {
    TABLE.iter().filter(|e| e.bound == Bound::Cmd(cmd)).flat_map(|e| e.keys).any(|k| crate::audition::keyboard_note(k, 4).is_some())
}

/// Whether a command's key is used for note playing or octave/velocity control when Computer MIDI Keyboard is active.
pub fn is_musical_typing_key(cmd: Cmd) -> bool {
    is_note_key(cmd)
        || TABLE
            .iter()
            .filter(|e| e.bound == Bound::Cmd(cmd))
            .flat_map(|e| e.keys)
            .any(|k| crate::audition::keyboard_control(k).is_some())
}

/// The shortcut of an action bound in `actions.rs`.
pub fn action_hint(action: &str) -> Option<String> {
    TABLE.iter().find(|e| matches!(e.bound, Bound::Action(a) if a == action)).and_then(Entry::hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The bindings `actions.rs` makes: (key with `mod`, action, scope).
    fn bound_in_actions() -> Vec<(String, String, Scope)> {
        let mut out = Vec::new();
        for line in include_str!("actions.rs").lines() {
            let Some((_, rest)) = line.split_once("KeyBinding::new(") else { continue };
            let (key, rest) = if let Some(r) = rest.strip_prefix("&format!(\"") {
                let (k, r) = r.split_once("\")").expect("format! key");
                (k.replace("{m}", "mod"), r)
            } else {
                let (k, r) = rest.strip_prefix('"').and_then(|r| r.split_once('"')).expect("literal key");
                (k.to_owned(), r)
            };
            let mut args = rest.trim_start_matches(',').split(',').map(str::trim);
            let action = args.next().expect("action").to_owned();
            let scope = match args.next().expect("context").trim_end_matches([')', ',']) {
                "Some(CTX" => Scope::Keys,
                "Some(\"Workspace\"" => Scope::Workspace,
                "None" => Scope::App,
                other => panic!("unknown key context {other:?} in actions.rs"),
            };
            out.push((key, action, scope));
        }
        out
    }

    #[test]
    fn the_table_lists_exactly_what_actions_rs_binds() {
        let bound = bound_in_actions();
        assert!(bound.len() > 40, "the parser found the bindings");
        let listed: Vec<(String, String, Scope)> = TABLE
            .iter()
            .filter_map(|e| match e.bound {
                Bound::Action(a) => Some((e, a)),
                Bound::Cmd(_) => None,
            })
            .flat_map(|(e, a)| e.keys.iter().map(move |k| ((*k).to_owned(), a.to_owned(), e.scope)))
            .collect();
        for b in &bound {
            assert!(listed.contains(b), "actions.rs binds {b:?}; add it to keys::TABLE so the shortcuts sheet shows it");
        }
        for l in &listed {
            assert!(bound.contains(l), "keys::TABLE lists {l:?}, which actions.rs does not bind");
        }
    }

    #[test]
    fn no_key_is_bound_twice() {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for e in TABLE {
            for key in e.keys {
                if let Some(other) = seen.insert(key, e.label) {
                    panic!("{key} is bound to both {other} and {}", e.label);
                }
            }
        }
        // Every command of the table has a row, and a key, except the few
        // reached only from the palette and menus.
        const KEYLESS: &[Bound] = &[Bound::Action("AddLibraryFolder"), Bound::Action("ToggleAutomationArm")];
        for e in TABLE {
            assert!(!e.keys.is_empty() || KEYLESS.contains(&e.bound), "{} has no key", e.label);
            assert!(!e.label.is_empty() && e.help.ends_with('.'), "{}", e.label);
        }
    }

    #[test]
    fn single_keys_do_not_fire_while_typing() {
        for e in TABLE {
            for key in e.keys {
                let (mods, _) = parts(key);
                let typed = mods.iter().all(|m| *m == "shift");
                assert!(!typed || e.scope == Scope::Keys, "{key} ({}) would fire in a text field", e.label);
            }
            if matches!(e.bound, Bound::Cmd(_)) {
                assert_eq!(e.scope, Scope::Keys, "{}", e.label);
            }
        }
    }

    #[test]
    fn keys_read_like_keys() {
        assert_eq!(display_for("mod-shift-d", false), "Ctrl+Shift+D");
        assert_eq!(display_for("mod--", false), "Ctrl+-");
        assert_eq!(display_for("-", false), "-");
        assert_eq!(display_for("shift-space", false), "Shift+Space");
        assert_eq!(display_for("f11", false), "F11");
        assert_eq!(display_for("mod-alt-f", true), "⌥⌘F");
        assert_eq!(display_for("left", true), "←");
        assert_eq!(hint(Cmd::Paste).as_deref(), Some(if is_mac() { "⌘V" } else { "Ctrl+V" }));
        assert!(action_hint("SplitClip").is_some());
        // S and O are notes while musical typing is on; Ctrl+C never is.
        assert!(is_note_key(Cmd::FoldTrack) && is_note_key(Cmd::Metronome));
        assert!(!is_note_key(Cmd::Copy) && !is_note_key(Cmd::ArmTrack));
    }
}
