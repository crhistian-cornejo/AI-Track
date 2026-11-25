//! Racks as presets in the Device View: the Save popover of a rack (from its
//! title bar or its menu) and adding one from the palette (Enter, or a drag
//! into the chain). The file work is `rack_library`; the project-side
//! steps are `AppState::rack_preset_for_save` (see `editing/rack_presets.rs`).

use crate::icons::{Icon, icon};
use crate::rack_library::{self, RackSource};
use crate::theme::*;
use crate::views::device_ui::PopKind;
use crate::views::device_window::chrome::name_field;
use crate::widgets::{self, label};
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, TrackId};
use gpui::{AnyElement, Context, FontWeight, Focusable, IntoElement, ParentElement, Pixels, Point, Render, StatefulInteractiveElement, Styled, Window, div, px};

/// Open the Save popover of `rack` at `at`, with the rack's name in the
/// field (focused when there is a window to focus it in).
pub fn open_save(ws: &mut Workspace, rack: DeviceId, at: Point<Pixels>, window: Option<&mut Window>, cx: &mut Context<Workspace>) {
    let name = ws.s.project.device(rack).map(|d| d.name.clone()).unwrap_or_default();
    let input = name_field(ws, rack, "Rack name", save, cx);
    input.update(cx, |i, cx| i.set_text(&name, cx));
    if let Some(window) = window {
        input.focus_handle(cx).focus(window, cx);
    }
    ws.fx.pop = None;
    ws.fx.open_pop(PopKind::SaveRack(rack), at);
    cx.notify();
}

/// The Save popover: a name and the button.
pub fn save_card(ws: &mut Workspace, rack: DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    ws.s.project.device(rack)?;
    let input = name_field(ws, rack, "Rack name", save, cx);
    let for_button = input.clone();
    Some(
        widgets::menu_card("rack-save-card", 280.)
            .p(px(SP3))
            .gap(px(SP2))
            .child(label("SAVE RACK", TEXT_XS, pal.faint).font_weight(FontWeight::MEDIUM))
            .child(input)
            .child(label("Kept with your racks, in every project.", TEXT_XS, pal.faint))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SP2))
                    .child(widgets::text_button("rack-save-cancel", "Cancel", None, false).on_click(cx.listener(|this, _, _, cx| {
                        this.fx.pop = None;
                        cx.notify();
                    })))
                    .child(widgets::text_button("rack-save-go", "Save", Some(Icon::Check), true).on_click(cx.listener(move |this, _, _, cx| {
                        let name = for_button.read(cx).text().to_owned();
                        save(this, rack, &name, cx);
                    }))),
            )
            .into_any_element(),
    )
}

/// Save `rack` as your rack `name`: the live plugin states are stored first,
/// the samples it names are stamped, then it is written under your racks.
pub fn save(ws: &mut Workspace, rack: DeviceId, name: &str, cx: &mut Context<Workspace>) {
    let states = ws.b.engine.plugin_states();
    let result = ws
        .s
        .rack_preset_for_save(rack, name, states)
        .and_then(|(mut preset, uncaptured)| {
            rack_library::stamp_samples(&mut preset);
            rack_library::save(&rack_library::root(), &preset).map(|(entry, replaced)| (entry, replaced, uncaptured))
        });
    match result {
        Ok((entry, replaced, uncaptured)) => {
            ws.fx.pop = None;
            let mut text = format!("{} {}", if replaced { "Updated" } else { "Saved" }, entry.name);
            if !uncaptured.is_empty() {
                // One toast says both: a second one would replace the first at once.
                let this = if uncaptured.len() == 1 { "this plugin" } else { "these plugins" };
                text += &format!(
                    " · {}: the state of {this} could not be read (sandbox?), the rack keeps the last one saved",
                    uncaptured.join(", ")
                );
            }
            ws.toast(text, cx);
            ws.palette.invalidate_racks();
        }
        Err(e) => ws.toast(e, cx),
    }
    cx.notify();
}

/// A rack being dragged from the palette into a chain.
#[derive(Clone, Debug)]
pub struct DraggedRack {
    pub source: RackSource,
    pub name: String,
}

impl Render for DraggedRack {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        div()
            .px(px(SP2))
            .h(px(TAB_BAR_H))
            .flex()
            .items_center()
            .gap(px(SP2))
            .rounded(px(r_md()))
            .bg(pal.raised)
            .border_1()
            .border_color(accent())
            .shadow_lg()
            .child(icon(Icon::Grid, 12., pal.accent))
            .child(label(self.name.clone(), TEXT_SM, pal.text))
    }
}

/// Add the rack behind `source` at the end of `track`'s chain, as one undo
/// step, and say what it is missing. Opens the Devices tab, where it shows.
pub fn add(ws: &mut Workspace, source: &RackSource, track: TrackId, cx: &mut Context<Workspace>) -> bool {
    let preset = match rack_library::read(source) {
        Ok(p) => p,
        Err(e) => {
            ws.toast(e, cx);
            return false;
        }
    };
    let installed = ws.b.plugins.external_plugins().to_vec();
    let Some(load) = ws.s.add_rack_preset(track, &preset, &installed, &|f| f.exists()) else {
        ws.toast("Could not add the rack", cx);
        return false;
    };
    ws.bottom_open = true;
    ws.set_tab(crate::state::BottomTab::Devices, cx);
    ws.s.ed.devices = vec![load.device];
    ws.toast(
        if load.missing.is_empty() { format!("Added {}", preset.name) } else { format!("Added {} · missing: {}", preset.name, load.missing.join(", ")) },
        cx,
    );
    cx.notify();
    true
}
