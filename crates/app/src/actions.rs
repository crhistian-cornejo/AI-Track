//! Keyboard actions and their default bindings.

use gpui::{App, KeyBinding, actions};

actions!(
    aitrack,
    [
        PlayStop,
        ToggleView,
        Undo,
        Redo,
        DeleteSelection,
        ToggleDrawTool,
        ToggleLoop,
        ToggleAssistant,
        ToggleBrowser,
        SelectAll,
        TabDevices,
        TabPianoRoll,
        TabSteps,
        TabMixer,
        Escape,
        Quit,
    ]
);

/// Single-key shortcuts must not fire while a text field has the keys.
const CTX: &str = "Workspace && !TextInput";

pub fn bind(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    cx.bind_keys([
        KeyBinding::new("space", PlayStop, Some(CTX)),
        KeyBinding::new("tab", ToggleView, Some(CTX)),
        KeyBinding::new(&format!("{m}-z"), Undo, Some("Workspace")),
        KeyBinding::new(&format!("{m}-shift-z"), Redo, Some("Workspace")),
        KeyBinding::new(&format!("{m}-y"), Redo, Some("Workspace")),
        KeyBinding::new("delete", DeleteSelection, Some(CTX)),
        KeyBinding::new("backspace", DeleteSelection, Some(CTX)),
        KeyBinding::new("b", ToggleDrawTool, Some(CTX)),
        KeyBinding::new(&format!("{m}-l"), ToggleLoop, Some("Workspace")),
        KeyBinding::new(&format!("{m}-i"), ToggleAssistant, Some("Workspace")),
        KeyBinding::new(&format!("{m}-alt-b"), ToggleBrowser, Some("Workspace")),
        KeyBinding::new(&format!("{m}-a"), SelectAll, Some(CTX)),
        KeyBinding::new("1", TabDevices, Some(CTX)),
        KeyBinding::new("2", TabPianoRoll, Some(CTX)),
        KeyBinding::new("3", TabSteps, Some(CTX)),
        KeyBinding::new("4", TabMixer, Some(CTX)),
        KeyBinding::new("escape", Escape, Some(CTX)),
        KeyBinding::new(&format!("{m}-q"), Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}
