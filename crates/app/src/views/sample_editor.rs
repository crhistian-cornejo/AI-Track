//! The sample editor: the Clip tab of the bottom panel for an audio clip
//! (filled in by the sample-editor worker).

use crate::theme::*;
use crate::widgets::label;
use crate::workspace::Workspace;
use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, px};

#[derive(Default)]
pub struct UiState {}

/// The clip editor for an audio clip (bottom panel, "Clip" tab).
pub fn render(ws: &mut Workspace, _: &mut Window, _: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let name = ws.s.selected_clip.and_then(|c| ws.s.project.clips.get(&c)).map(|c| c.name.clone()).unwrap_or_default();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SP1))
        .child(label(name, TEXT_LG, pal.text).font_weight(FontWeight::MEDIUM))
        .child(label("Sample editor", TEXT_MD, pal.faint))
        .into_any_element()
}

/// Window-wide mouse capture while one of the editor's own drags is active.
pub fn capture(_ws: &Workspace, _cx: &mut Context<Workspace>) -> Option<AnyElement> {
    None
}

/// Whether the editor needs continuous frames (playhead, animations).
pub fn wants_frames(_ws: &Workspace) -> bool {
    false
}
