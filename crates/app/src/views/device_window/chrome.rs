//! The window's header: what every plugin window has at the top. The device
//! and its track, a clear bypass switch, the preset selector (previous / next,
//! a list by category, save), A/B with copy, and a reset.
//!
//! Every action is an edit of the project: loading a preset, recalling a
//! slot and resetting each go through the workspace's edit path as one undo
//! step, so Cmd+Z undoes them from either window. The A/B slots and the
//! open menus are view state (they live on [`ChromeState`], not in the
//! project).

use super::io::Hold;
use super::meters::Tick;
use super::presets::{self, Ab, Entry};
use super::status::Probe;
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::{PopKind, set_many};
use crate::widgets::{self, InputEvent, TextInput, label, menu_header, menu_item};
use crate::workspace::Workspace;
use aitrack_model::{Command, DeviceId, DeviceKind};
use gpui::{
    AnyElement, AppContext, ClickEvent, Context, Entity, FontWeight, Focusable, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, Subscription, div, point, prelude::FluentBuilder, px,
};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

/// View state of one device's window frame.
#[derive(Default)]
pub struct ChromeState {
    pub ab: Ab,
    /// The presets on offer, read once (and again after saving).
    list: Option<Rc<Vec<Entry>>>,
    /// The loaded preset's parameters, for "modified" (path, values).
    loaded: Option<(String, Rc<BTreeMap<u32, f32>>)>,
    /// The name field of the Save popover.
    name: Option<Entity<TextInput>>,
    _name_sub: Option<Subscription>,
    pub holds: HashMap<Tick, Hold>,
    /// Where the editor's open drawer was drawn last frame (its height sizes the window).
    pub drawer: crate::views::device_ui::Region,
    pub probe: Probe,
}

impl ChromeState {
    /// Whether a peak tick is still waiting or falling (the window keeps drawing).
    pub fn animating(&self) -> bool {
        self.holds.values().any(Hold::active)
    }
}

/// Where the user's presets live: `user_presets/<plugin>/<Name>.json` in the config folder.
pub fn user_root() -> PathBuf {
    crate::appearance::config_dir().join("user_presets")
}

// ───────────────────────────── Preset list and state ─────────────────────────────

/// The presets a device offers: its factory bank, then the user's.
pub fn list(ws: &mut Workspace, device: DeviceId) -> Rc<Vec<Entry>> {
    if let Some(l) = ws.fx.chrome.get(&device).and_then(|c| c.list.clone()) {
        return l;
    }
    let Some(plugin) = super::meters::plugin_of(ws, device) else { return Rc::default() };
    let mut factory: Vec<Entry> =
        ws.b.plugins.presets(&plugin).into_iter().map(|p| Entry { path: p.path, category: p.category, name: p.name, user: false }).collect();
    if factory.is_empty() {
        // The stub catalog lists none; the bank the stock crate embeds is the same one.
        factory = aitrack_stock::presets::list(&plugin).into_iter().map(|r| Entry { path: r.path, category: r.category, name: r.name, user: false }).collect();
    }
    let all = Rc::new(presets::merge(factory, presets::list_user(&user_root(), &plugin)));
    ws.fx.chrome.entry(device).or_default().list = Some(all.clone());
    all
}

/// The entry of the preset the device says it loaded.
fn current(ws: &mut Workspace, device: DeviceId) -> Option<Entry> {
    let name = ws.s.project.device(device)?.preset.clone();
    let list = list(ws, device);
    presets::position(&list, name.as_deref()).map(|i| list[i].clone())
}

/// Every parameter's normalised value (stored or default).
fn snapshot(ws: &mut Workspace, device: DeviceId) -> BTreeMap<u32, f32> {
    let Some(plugin) = super::meters::plugin_of(ws, device) else { return BTreeMap::new() };
    let set = ws.fx.params(ws.b.plugins.as_ref(), &plugin);
    let Some(d) = ws.s.project.device(device) else { return BTreeMap::new() };
    set.list.iter().map(|i| (i.id, d.params.get(&i.id).copied().unwrap_or_else(|| i.default_normalized()))).collect()
}

/// Whether a parameter moved since the preset was loaded.
pub fn is_modified(ws: &mut Workspace, device: DeviceId) -> bool {
    let Some(entry) = current(ws, device) else { return false };
    let cached = ws.fx.chrome.get(&device).and_then(|c| c.loaded.clone()).filter(|(path, _)| *path == entry.path);
    let params = match cached {
        Some((_, params)) => params,
        None => {
            let Ok((_, params)) = presets::read(&entry.path) else { return false };
            let params = Rc::new(params);
            ws.fx.chrome.entry(device).or_default().loaded = Some((entry.path.clone(), params.clone()));
            params
        }
    };
    let Some(plugin) = super::meters::plugin_of(ws, device) else { return false };
    let set = ws.fx.params(ws.b.plugins.as_ref(), &plugin);
    let Some(d) = ws.s.project.device(device) else { return false };
    presets::differs(&params, &d.params, |id| set.get(id).map(|i| i.default_normalized()))
}

// ───────────────────────────── Actions ─────────────────────────────

/// Load a preset (one undo step).
pub fn load_entry(ws: &mut Workspace, device: DeviceId, entry: &Entry, cx: &mut Context<Workspace>) {
    match presets::read(&entry.path) {
        Ok((_, params)) => {
            ws.s.load_preset(device, &entry.name, &params);
            // Recalling a preset starts the comparison afresh.
            ws.fx.chrome.entry(device).or_default().ab = Ab::default();
        }
        Err(e) => ws.toast(e.to_string(), cx),
    }
    cx.notify();
}

/// The preset `dir` steps from the loaded one.
pub fn step(ws: &mut Workspace, device: DeviceId, dir: i32, cx: &mut Context<Workspace>) {
    if super::meters::plugin_of(ws, device).is_some_and(|p| p == aitrack_stock::instrument::vault::ID) {
        super::vault::step_preset(ws, device, dir, cx);
        return;
    }
    let list = list(ws, device);
    let name = ws.s.project.device(device).and_then(|d| d.preset.clone());
    if let Some(i) = presets::step(list.len(), presets::position(&list, name.as_deref()), dir) {
        load_entry(ws, device, &list[i], cx);
    }
}

/// The commands that put every parameter back to its default and forget the
/// preset name (the device keeps its state: samples, wavetables).
pub fn reset_commands(d: &aitrack_model::Device) -> Vec<Command> {
    let device = d.id;
    let mut cmds: Vec<Command> = d.params.keys().map(|&param| Command::SetParam { device, param, value: None }).collect();
    cmds.push(Command::SetDeviceState { device, state: d.state.clone(), preset: None });
    cmds
}

/// Every parameter back to its default, as one step; the preset name goes.
pub fn reset_defaults(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    let Some(d) = ws.s.project.device(device) else { return };
    let cmds = reset_commands(d);
    ws.s.seal();
    ws.s.edit("Reset to defaults", cmds);
    ws.s.seal();
    ws.fx.chrome.entry(device).or_default().ab = Ab::default();
    cx.notify();
}

/// Back to the loaded preset, or to the defaults when there is none.
pub fn reset(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    match current(ws, device) {
        Some(entry) => load_entry(ws, device, &entry, cx),
        None => reset_defaults(ws, device, cx),
    }
}

/// Flip to A or B; the other keeps what the device had.
pub fn ab_switch(ws: &mut Workspace, device: DeviceId, slot: usize, cx: &mut Context<Workspace>) {
    let now = snapshot(ws, device);
    let apply = ws.fx.chrome.entry(device).or_default().ab.switch(slot, &now);
    if let Some(values) = apply {
        let changed: Vec<(u32, f32)> = values.into_iter().filter(|(id, v)| now.get(id).is_none_or(|n| (n - v).abs() > 1e-6)).collect();
        if !changed.is_empty() {
            set_many(ws, if slot == 0 { "Recall A" } else { "Recall B" }, device, &changed);
        }
    }
    cx.notify();
}

/// Copy what the device has now into the other slot.
pub fn ab_copy(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    let now = snapshot(ws, device);
    let state = ws.fx.chrome.entry(device).or_default();
    let (from, to) = if state.ab.active == 0 { ("A", "B") } else { ("B", "A") };
    state.ab.copy_to_other(&now);
    ws.toast(format!("Copied {from} to {to}"), cx);
    cx.notify();
}

/// Save the device's settings as the user's preset `name`.
pub fn save(ws: &mut Workspace, device: DeviceId, name: &str, cx: &mut Context<Workspace>) {
    let Some(plugin) = super::meters::plugin_of(ws, device) else { return };
    let params = snapshot(ws, device);
    match presets::save_user(&user_root(), &plugin, name, &params) {
        Ok((entry, replaced)) => {
            // The device now is that preset: show its name, in one undo step.
            let state = ws.s.project.device(device).and_then(|d| d.state.clone());
            ws.s.seal();
            ws.s.edit("Save preset", vec![Command::SetDeviceState { device, state, preset: Some(entry.name.clone()) }]);
            ws.s.seal();
            let c = ws.fx.chrome.entry(device).or_default();
            c.list = None;
            c.loaded = None;
            ws.fx.pop = None;
            ws.toast(if replaced { format!("Updated {}", entry.name) } else { format!("Saved {}", entry.name) }, cx);
        }
        Err(e) => ws.toast(e, cx),
    }
    cx.notify();
}

/// The name field of the Save popover (made on first use); Enter saves.
fn name_field(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> Entity<TextInput> {
    if let Some(n) = ws.fx.chrome.get(&device).and_then(|c| c.name.clone()) {
        return n;
    }
    let input = cx.new(|cx| TextInput::new(cx, "Preset name", Some(Icon::Pencil)));
    let sub = cx.subscribe(&input, move |ws: &mut Workspace, _, e: &InputEvent, cx| {
        if let InputEvent::Submit(t) = e {
            save(ws, device, t, cx);
        }
    });
    let c = ws.fx.chrome.entry(device).or_default();
    c.name = Some(input.clone());
    c._name_sub = Some(sub);
    input
}

/// Open the Save popover at `at`, with a name that is not taken yet.
pub(super) fn open_save(ws: &mut Workspace, device: DeviceId, at: gpui::Point<gpui::Pixels>, window: &mut gpui::Window, cx: &mut Context<Workspace>) {
    let list = list(ws, device);
    let device_name = ws.s.project.device(device).map(|d| d.name.clone()).unwrap_or_default();
    let loaded = ws.s.project.device(device).and_then(|d| d.preset.clone());
    let suggestion = presets::suggest_name(loaded.as_deref(), &device_name, &list);
    let input = name_field(ws, device, cx);
    input.update(cx, |i, cx| i.set_text(&suggestion, cx));
    input.focus_handle(cx).focus(window, cx);
    ws.fx.pop = None;
    ws.fx.open_pop(PopKind::SavePreset(device), at);
    cx.notify();
}

// ───────────────────────────── Header ─────────────────────────────

/// A 28 px square button with a glyph, for the header's tools.
fn tool(id: impl Into<gpui::ElementId>, glyph: Icon, tip: &'static str) -> Stateful<gpui::Div> {
    widgets::sized_icon_button(id, glyph, false, CONTROL_H, 14.).tooltip(widgets::tip(tip, None))
}

/// The `A` / `B` button of the comparison.
fn slot_button(id: (&'static str, usize), text: &'static str, on: bool, stored: bool) -> Stateful<gpui::Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(CONTROL_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if on { pal.accent } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(label(text, TEXT_MD, if on { pal.on_accent } else if stored { pal.text } else { pal.faint }).font_weight(FontWeight::SEMIBOLD))
}

/// The window's header.
pub fn header(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(d) = ws.s.project.device(device).cloned() else { return div().into_any_element() };
    let track = ws.s.project.device_track(device).and_then(|t| ws.s.project.track(t)).map(|t| (t.name.clone(), t.color));
    let enabled = d.enabled;
    let (ic, kind) = match &d.kind {
        DeviceKind::Stock { plugin_id } => (
            crate::views::menus::device_icon(plugin_id, ws),
            crate::views::menus::device_kind_label_of(plugin_id, &d.params, ws).to_owned(),
        ),
        _ => (Icon::Device, String::new()),
    };
    let presets = list(ws, device);
    let modified = is_modified(ws, device);
    let ab = ws.fx.chrome.entry(device).or_default().ab.clone();
    // Whether the other slot holds a setting different from the device's now.
    let other_differs = {
        let now = snapshot(ws, device);
        ab.other_differs(&now)
    };
    let preset_name = d.preset.clone();
    let has_presets = !presets.is_empty();
    let reset_tip = if preset_name.is_some() && current(ws, device).is_some() { "Back to the preset's values" } else { "Reset every parameter to its default" };
    let copy_tip = if ab.active == 0 { "Copy A to B" } else { "Copy B to A" };

    // Bypass: a switch with a word on it, amber while the device is bypassed.
    let bypass_color = if enabled { pal.accent } else { pal.mute };
    let bypass = div()
        .id(("win-bypass", device.0 as usize))
        .flex_none()
        .h(px(CONTROL_H))
        .px(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(with_alpha(bypass_color, if enabled { 0.14 } else { 0.22 }))
        .border_1()
        .border_color(with_alpha(bypass_color, if enabled { 0.35 } else { 0.6 }))
        .hover(|s| s.bg(with_alpha(bypass_color, 0.3)))
        .tooltip(widgets::tip(if enabled { "Bypass the device" } else { "Bring the device back in" }, None))
        .child(icon(Icon::Power, 14., bypass_color))
        .child(label(if enabled { "On" } else { "Bypassed" }, TEXT_SM, bypass_color).font_weight(FontWeight::SEMIBOLD))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.s.toggle_device(device);
            cx.notify();
        }));

    let title = div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(label(crate::paint::fit(&d.name, 220., DEV_TITLE), DEV_TITLE, if enabled { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .when_some(track, |r, (name, color)| {
                    r.child(div().size(px(6.)).rounded_full().bg(track_color(color))).child(label(crate::paint::fit(&name, 140., DEV_AXIS), DEV_AXIS, pal.muted))
                })
                .child(label(kind, DEV_AXIS, pal.faint)),
        );

    // The selector: [<] [name v] [>].
    let step_button = |id: &'static str, glyph: Icon, dir: i32, tip: &'static str| {
        div()
            .id((id, device.0 as usize))
            .flex_none()
            .size(px(CONTROL_H))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(content(0.12)))
            .tooltip(widgets::tip(tip, None))
            .child(icon(glyph, 12., if has_presets { pal.muted } else { pal.faint }))
            .on_click(cx.listener(move |this, _, _, cx| step(this, device, dir, cx)))
    };
    let shown = preset_name.clone().unwrap_or_else(|| "Presets".to_owned());
    let selector = div()
        .flex_none()
        .h(px(CONTROL_H))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .overflow_hidden()
        .bg(content(0.06))
        .border_1()
        .border_color(border())
        .child(step_button("win-preset-prev", Icon::ChevronLeft, -1, "Previous preset"))
        .child(
            div()
                .id(("win-presets", device.0 as usize))
                .h_full()
                .w(px(188.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .cursor_pointer()
                .border_l_1()
                .border_r_1()
                .border_color(border())
                .hover(|s| s.bg(content(0.12)))
                .tooltip(widgets::tip("Presets: factory ones by category, and yours", None))
                .when(modified, |d| d.child(div().flex_none().size(px(6.)).rounded_full().bg(pal.accent)))
                .child(
                    div().flex_1().min_w(px(0.)).overflow_hidden().child(label(
                        crate::paint::fit(&shown, 140., TEXT_MD),
                        TEXT_MD,
                        if preset_name.is_some() { pal.text } else { pal.muted },
                    )),
                )
                .child(icon(Icon::ChevronDown, 12., pal.faint))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    let at = e.position();
                    this.fx.open_pop(PopKind::Presets(device), point(at.x - px(8.), at.y + px(12.)));
                    cx.notify();
                })),
        )
        .child(step_button("win-preset-next", Icon::ChevronRight, 1, "Next preset"));

    let save_button = tool(("win-save", device.0 as usize), Icon::Download, "Save as a preset of yours").on_click(cx.listener(
        move |this, e: &ClickEvent, window, cx| {
            let at = e.position();
            open_save(this, device, point(at.x - px(200.), at.y + px(16.)), window, cx);
        },
    ));
    let reset_button = tool(("win-reset", device.0 as usize), Icon::Refresh, reset_tip).on_click(cx.listener(move |this, _, _, cx| reset(this, device, cx)));

    let ab_group = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(slot_button(("win-ab-a", device.0 as usize), "A", ab.active == 0, ab.active == 1 && other_differs).tooltip(widgets::tip("Setting A: flip between two settings while you listen", None)).on_click(cx.listener(move |this, _, _, cx| ab_switch(this, device, 0, cx))))
        .child(slot_button(("win-ab-b", device.0 as usize), "B", ab.active == 1, ab.active == 0 && other_differs).tooltip(widgets::tip("Setting B: the first visit starts as a copy of A", None)).on_click(cx.listener(move |this, _, _, cx| ab_switch(this, device, 1, cx))))
        .child(tool(("win-ab-copy", device.0 as usize), Icon::Copy, copy_tip).on_click(cx.listener(move |this, _, _, cx| ab_copy(this, device, cx))));

    div()
        .flex_none()
        .h(px(super::placement::HEADER_H))
        .px(px(SP4))
        .gap(px(SP3))
        .flex()
        .items_center()
        .bg(pal.panel)
        .border_b_1()
        .border_color(border())
        .child(bypass)
        .child(icon(ic, 16., pal.muted))
        .child(title)
        .child(selector)
        .child(div().flex().items_center().gap(px(SP1)).child(save_button).child(reset_button))
        .child(div().flex_none().w(px(1.)).h(px(20.)).bg(border()))
        .child(ab_group)
        .into_any_element()
}

// ───────────────────────────── Menus ─────────────────────────────

/// The preset list: defaults, save, then the user's presets and the factory
/// ones under their category.
pub fn preset_menu(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let entries = list(ws, device);
    let loaded = ws.s.project.device(device)?.preset.clone();
    let mut card = widgets::menu_card("win-preset-menu", 248.)
        .child(menu_item(("win-preset-init", 0usize), Some(Icon::Refresh), "Initialize", Some("defaults"), false).on_click(cx.listener(move |this, _, _, cx| {
            this.fx.pop = None;
            reset_defaults(this, device, cx);
        })))
        .child(menu_item(("win-preset-save", 0usize), Some(Icon::Download), "Save as…", None, false).on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
            let at = e.position();
            open_save(this, device, point(at.x - px(8.), at.y + px(8.)), window, cx);
        })));
    let mut index = 0usize;
    for (category, items) in presets::by_category(&entries) {
        card = card.child(menu_header(category.to_uppercase()));
        for e in items {
            let on = loaded.as_deref() == Some(e.name.as_str());
            let entry = e.clone();
            card = card.child(menu_item(("win-preset", index), None, e.name.clone(), None, on).on_click(cx.listener(move |this, _, _, cx| {
                this.fx.pop = None;
                load_entry(this, device, &entry, cx);
            })));
            index += 1;
        }
    }
    Some(div().id("win-preset-scroll").max_h(px(360.)).overflow_y_scroll().child(card).into_any_element())
}

/// The Save popover: a name and the button.
pub fn save_card(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    let input = name_field(ws, device, cx);
    let for_button = input.clone();
    let plugin_name = ws.s.project.device(device)?.name.clone();
    Some(
        widgets::menu_card("win-save-card", 280.)
            .p(px(SP3))
            .gap(px(SP2))
            .child(label("SAVE PRESET", TEXT_XS, pal.faint).font_weight(FontWeight::MEDIUM))
            .child(input)
            .child(label(format!("Kept with your presets for {plugin_name}, in every project."), TEXT_XS, pal.faint))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SP2))
                    .child(widgets::text_button("win-save-cancel", "Cancel", None, false).on_click(cx.listener(|this, _, _, cx| {
                        this.fx.pop = None;
                        cx.notify();
                    })))
                    .child(widgets::text_button("win-save-go", "Save", Some(Icon::Check), true).on_click(cx.listener(move |this, _, _, cx| {
                        let name = for_button.read(cx).text().to_owned();
                        save(this, device, &name, cx);
                    }))),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    #[test]
    fn reset_puts_every_parameter_back_in_one_undo_step() {
        let mut s = AppState::new(crate::demo::project());
        let eq = s.project.tracks.iter().flat_map(|t| &t.devices).find(|d| d.name == "EQ").unwrap().id;
        // A preset is loaded and a parameter moved on top of it.
        let mut params = BTreeMap::new();
        params.insert(2, 0.8);
        params.insert(5, 0.3);
        assert!(s.load_preset(eq, "Bass Focus", &params));
        s.edit("tweak", vec![Command::SetParam { device: eq, param: 7, value: Some(0.6) }]);
        let before = s.project.device(eq).unwrap().clone();
        assert_eq!(before.preset.as_deref(), Some("Bass Focus"));
        let steps = s.history.steps().count();

        let cmds = reset_commands(&before);
        s.seal();
        assert!(s.edit("Reset to defaults", cmds));
        s.seal();
        let after = s.project.device(eq).unwrap();
        assert!(after.params.is_empty(), "defaults are stored as nothing");
        assert_eq!(after.preset, None);
        assert_eq!(s.history.steps().count(), steps + 1);

        s.undo();
        let back = s.project.device(eq).unwrap();
        assert_eq!(back.params, before.params);
        assert_eq!(back.preset.as_deref(), Some("Bass Focus"));
    }
}
