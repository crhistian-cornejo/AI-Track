//! The Vault instrument's window: a preset browser on the left (search,
//! sound types, character tags, favourites), the loaded preset with its
//! four macros on the right, and below them the expert panel of the
//! preset's engine ([`engines`]).
//!
//! Loading a preset is one `load_preset` edit (every parameter plus the
//! name, one undo step), and every knob is a `SetParam`, so the Device View
//! card ([`crate::views::device_ui::vault`]), this window and the engine
//! follow the project. Favourites are the user's, not the project's: they
//! live in a JSON file in the app config folder ([`browse::Favourites`]).

pub mod browse;
mod engines;
pub mod geom;

use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::{Dev, current, set_now};
use crate::widgets::{self, InputEvent, TextInput, label, mono};
use crate::workspace::{Drag, KnobTarget, Workspace};
use aitrack_model::{Device, DeviceId};
use aitrack_stock::instrument::vault::{self as vault, PresetInfo, TYPES, macro_knob, params};
use browse::{Favourites, Filter};
use gpui::{
    AnyElement, AppContext, Context, Div, Entity, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Stateful, StatefulInteractiveElement, Styled, Subscription, div, prelude::FluentBuilder, px,
};
use std::collections::HashMap;
use std::rc::Rc;

/// Browser state of one Vault device (its window and its card's prev / next).
#[derive(Default)]
pub struct VaultView {
    pub filter: Filter,
    search: Option<Entity<TextInput>>,
    _sub: Option<Subscription>,
}

/// What every Vault editor shares: the factory list and the favourites
/// (both read once, on first use), and each device's browser.
#[derive(Default)]
pub struct VaultState {
    presets: Option<Rc<Vec<PresetInfo>>>,
    favs: Option<Favourites>,
    pub views: HashMap<DeviceId, VaultView>,
}

impl VaultState {
    /// The factory presets with their browser metadata.
    pub fn presets(&mut self) -> Rc<Vec<PresetInfo>> {
        self.presets.get_or_insert_with(|| Rc::new(vault::preset_info())).clone()
    }

    pub fn favourites(&mut self) -> &mut Favourites {
        self.favs.get_or_insert_with(|| Favourites::load(Favourites::default_file()))
    }

    pub fn view(&mut self, device: DeviceId) -> &mut VaultView {
        self.views.entry(device).or_default()
    }

    pub fn prune(&mut self, alive: impl Fn(DeviceId) -> bool) {
        self.views.retain(|id, _| alive(*id));
    }
}

/// The engine a device plays (from its Engine parameter).
pub fn engine(dev: &Dev) -> usize {
    dev.choice(params::ENGINE).min(vault::ENGINE_COUNT - 1)
}

/// An engine's colour (badges, the macro arcs).
pub fn engine_color(engine: usize) -> Hsla {
    series(engine)
}

/// The presets the device's browser shows and the index of its loaded one.
pub fn shown(ws: &mut Workspace, device: DeviceId) -> (Rc<Vec<PresetInfo>>, Vec<usize>, Option<usize>) {
    let list = ws.fx.vault.presets();
    let filter = ws.fx.vault.view(device).filter.clone();
    let shown = browse::filtered(&list, &filter, ws.fx.vault.favourites());
    let now = browse::find(&list, ws.s.project.device(device).and_then(|d| d.preset.as_deref()));
    (list, shown, now)
}

/// Load factory preset `path` onto `device` (one undo step).
pub fn load(ws: &mut Workspace, device: DeviceId, path: &str, cx: &mut Context<Workspace>) {
    // The catalog reads the factory bank; the stub catalog has none, so
    // fall back to the bank the stock crate embeds.
    let loaded = ws.b.plugins.load_preset(path).or_else(|_| {
        let p = aitrack_stock::presets::load(path).map_err(|e| e.to_string())?;
        let params = p.params.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.clamp(0., 1.)))).collect();
        Ok::<_, String>((p.name, params))
    });
    match loaded {
        Ok((name, params)) => {
            ws.s.load_preset(device, &name, &params);
        }
        Err(e) => ws.toast(e, cx),
    }
    cx.notify();
}

/// Load the preset `dir` steps away among the browser's shown presets.
pub fn step_preset(ws: &mut Workspace, device: DeviceId, dir: i32, cx: &mut Context<Workspace>) {
    let (list, shown, now) = shown(ws, device);
    if let Some(i) = browse::step(&shown, now, dir) {
        load(ws, device, &list[i].path, cx);
    }
}

/// A big macro knob: the macro's name for this engine, the dial, the value.
pub fn macro_dial(dev: &Dev, m: u32, name: &str, side: f32, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let id = macro_knob(m);
    let device = dev.id;
    let value = dev.norm(id);
    let info = dev.info(id).cloned();
    let default = info.as_ref().map_or(0., |i| i.default_normalized());
    let color = if dev.enabled { engine_color(engine(dev)) } else { pal.faint };
    let dial = widgets::knob(("vault-macro", dev.key(0x700 | m as usize)), value, false, side, color)
        .tooltip(widgets::tip_owned(format!("Macro {}: {name}\nDrag to turn · Shift fine · double-click resets", m + 1)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if e.click_count >= 2 {
                    set_now(this, device, id, default);
                    cx.notify();
                    return;
                }
                let value = info.as_ref().map_or(0., |i| current(this, device, i));
                this.begin_drag(Drag::Knob { target: KnobTarget::Param { device, param: id, default }, last_y: f32::from(e.position.y), value }, cx);
            }),
        );
    div()
        .flex_none()
        .w(px(side + SP4))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SP1))
        .child(label(crate::paint::fit(name, side + SP4, TEXT_SM), TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM))
        .child(dial)
        .child(mono(dev.text(id), TEXT_XS, pal.muted))
}

/// The four macros of the device's engine, in a row.
pub fn macro_row(dev: &Dev, side: f32, cx: &mut Context<Workspace>) -> Div {
    let defs = vault::macros::macros(engine(dev));
    let mut row = div().flex().items_start().gap(px(SP2));
    for (m, def) in defs.iter().enumerate().take(4) {
        row = row.child(macro_dial(dev, m as u32, def.name, side, cx));
    }
    row
}

/// A small engine badge.
pub fn engine_pill(engine: usize) -> Div {
    let c = engine_color(engine);
    div()
        .flex_none()
        .h(px(16.))
        .px(px(SP1 + 2.))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(with_alpha(c, 0.16))
        .child(label(browse::engine_badge(engine), TEXT_XS, c).font_weight(FontWeight::SEMIBOLD))
}

/// A filter pill: a type or a character tag.
fn pill(id: impl Into<gpui::ElementId>, text: String, on: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .bg(if on { accent_alpha(0.22) } else { content(0.05) })
        .border_1()
        .border_color(if on { accent_alpha(0.6) } else { border() })
        .hover(|s| s.bg(if on { accent_alpha(0.3) } else { content(0.1) }))
        .child(label(text, TEXT_XS, if on { pal.accent } else { pal.muted }).font_weight(FontWeight::MEDIUM))
}

/// The star of a preset: click to (un)favourite it.
pub fn star(id: impl Into<gpui::ElementId>, path: String, on: bool, cx: &mut Context<Workspace>) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.1)))
        .tooltip(widgets::tip(if on { "Remove from favourites" } else { "Add to favourites (yours, in every project)" }, None))
        .child(icon(if on { Icon::StarFilled } else { Icon::Star }, 12., if on { pal.solo } else { pal.faint }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.fx.vault.favourites().toggle(&path);
            cx.notify();
        }))
}

/// The search field of a device's browser (made on first use).
fn search(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> Entity<TextInput> {
    if let Some(s) = ws.fx.vault.view(device).search.clone() {
        return s;
    }
    let input = cx.new(|cx| TextInput::new(cx, "Search names, tags, engines", Some(Icon::Search)));
    let query = ws.fx.vault.view(device).filter.query.clone();
    if !query.is_empty() {
        input.update(cx, |i, cx| i.set_text(&query, cx));
    }
    let sub = cx.subscribe(&input, move |ws: &mut Workspace, _, e: &InputEvent, cx| {
        let (InputEvent::Changed(t) | InputEvent::Submit(t)) = e;
        ws.fx.vault.view(device).filter.query = t.clone();
        cx.notify();
    });
    let v = ws.fx.vault.view(device);
    v.search = Some(input.clone());
    v._sub = Some(sub);
    input
}

fn browser(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let input = search(ws, device, cx);
    let (list, shown, now) = shown(ws, device);
    let filter = ws.fx.vault.view(device).filter.clone();
    let fav_count = ws.fx.vault.favourites().len();

    // Types.
    let mut types = div().flex().flex_wrap().gap(px(SP1)).child(pill(("vault-type", dev.key(0xF0)), "All".into(), filter.kind.is_none()).on_click(cx.listener(move |this, _, _, cx| {
        this.fx.vault.view(device).filter.kind = None;
        cx.notify();
    })));
    for (k, t) in TYPES.iter().enumerate() {
        let on = filter.kind == Some(k);
        types = types.child(pill(("vault-type", dev.key(k)), (*t).to_owned(), on).on_click(cx.listener(move |this, _, _, cx| {
            let f = &mut this.fx.vault.view(device).filter;
            f.kind = if f.kind == Some(k) { None } else { Some(k) };
            cx.notify();
        })));
    }
    // Character tags.
    let mut tags = div().flex().flex_wrap().gap(px(SP1));
    for (k, (t, _)) in browse::tag_counts(&list).into_iter().enumerate() {
        let on = filter.tags.contains(&t);
        let tag = t.clone();
        tags = tags.child(pill(("vault-tag", dev.key(k)), t, on).on_click(cx.listener(move |this, _, _, cx| {
            let f = &mut this.fx.vault.view(device).filter;
            if !f.tags.remove(&tag) {
                f.tags.insert(tag.clone());
            }
            cx.notify();
        })));
    }
    let favs_on = filter.favourites_only;
    let tools = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(
            div()
                .id(("vault-favs", dev.key(0)))
                .h(px(20.))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if favs_on { with_alpha(pal.solo, 0.18) } else { content(0.05) })
                .hover(|s| s.bg(content(0.1)))
                .child(icon(if favs_on { Icon::StarFilled } else { Icon::Star }, 12., if favs_on { pal.solo } else { pal.muted }))
                .child(label(format!("Favourites {fav_count}"), TEXT_XS, if favs_on { pal.text } else { pal.muted }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let f = &mut this.fx.vault.view(device).filter;
                    f.favourites_only = !f.favourites_only;
                    cx.notify();
                })),
        )
        .child(div().flex_1())
        .child(label(format!("{} of {}", shown.len(), list.len()), TEXT_XS, pal.faint))
        .when(!filter.is_empty(), |d| {
            d.child(widgets::sized_icon_button(("vault-clear", dev.key(0)), Icon::X, false, 20., 12.).tooltip(widgets::tip("Clear the filters", None)).on_click(cx.listener(move |this, _, _, cx| {
                this.fx.vault.view(device).filter = Filter::default();
                if let Some(s) = this.fx.vault.view(device).search.clone() {
                    s.update(cx, |s, cx| s.set_text("", cx));
                }
                cx.notify();
            })))
        });

    // The list.
    let mut rows = div().id(("vault-list", dev.key(0))).flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
    if shown.is_empty() {
        rows = rows.child(div().p(px(SP4)).child(label("No preset matches. Clear a filter or the search.", TEXT_SM, pal.faint)));
    }
    for &i in &shown {
        let pr = &list[i];
        let on = now == Some(i);
        let fav = ws.fx.vault.favourites().contains(&pr.path);
        let path = pr.path.clone();
        rows = rows.child(
            div()
                .id(("vault-row", dev.key(i)))
                .flex_none()
                .h(px(32.))
                .pl(px(SP1))
                .pr(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { pal.selection } else { gpui::transparent_black() })
                .hover(|s| s.bg(if on { pal.selection } else { content(0.06) }))
                .child(star(("vault-star", dev.key(i)), pr.path.clone(), fav, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(label(pr.name.clone(), TEXT_MD, pal.text).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }))
                        .child(label(crate::paint::fit(&format!("{} · {}", pr.kind, pr.tags.join(", ")), 200., TEXT_XS), TEXT_XS, pal.faint)),
                )
                .child(engine_pill(pr.engine))
                .on_click(cx.listener(move |this, _, _, cx| load(this, device, &path, cx))),
        );
    }
    div()
        .flex_none()
        .w(px(336.))
        .h_full()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .bg(pal.panel)
        .border_r_1()
        .border_color(border())
        .child(input)
        .child(types)
        .child(tags)
        .child(tools)
        .child(widgets::divider_h())
        .child(rows)
}

fn hero(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let (list, _, now) = shown(ws, device);
    let eng = engine(dev);
    let preset = now.map(|i| list[i].clone());
    let name = ws.s.project.device(device).and_then(|d| d.preset.clone()).unwrap_or_else(|| "Init".into());
    let fav = preset.as_ref().is_some_and(|p| ws.fx.vault.favourites().contains(&p.path));
    let sub = preset.as_ref().map_or_else(|| browse::engine_name(eng).to_owned(), |p| format!("{} · {}", p.kind, browse::engine_name(eng)));
    let tags = preset.as_ref().map(|p| p.tags.clone()).unwrap_or_default();
    let nav = |id: &'static str, i: Icon, dir: i32, tip: &'static str, cx: &mut Context<Workspace>| {
        widgets::sized_icon_button((id, dev.key(0)), i, false, 28., 14.).tooltip(widgets::tip(tip, None)).on_click(cx.listener(move |this, _, _, cx| step_preset(this, device, dir, cx)))
    };
    let mut tag_row = div().flex().flex_wrap().gap(px(SP1));
    for t in tags {
        tag_row = tag_row.child(div().h(px(16.)).px(px(SP1 + 2.)).flex().items_center().rounded_full().bg(content(0.06)).child(label(t, TEXT_XS, pal.muted)));
    }
    let title = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(nav("vault-prev", Icon::ChevronLeft, -1, "Previous preset (in the browser's list)", cx))
        .child(div().flex().flex_col().gap(px(2.)).child(label(name, 22., pal.text).font_weight(FontWeight::BOLD)).child(label(sub, TEXT_SM, pal.muted)))
        .child(nav("vault-next", Icon::ChevronRight, 1, "Next preset (in the browser's list)", cx))
        .when_some(preset.as_ref().map(|p| p.path.clone()), |d, path| d.child(star(("vault-hero-star", dev.key(0)), path, fav, cx)));
    div()
        .flex_none()
        .p(px(SP4))
        .gap(px(SP6))
        .flex()
        .items_center()
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(border())
        .child(div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(SP2)).child(title).child(tag_row).child(div().flex().child(engine_pill(eng))))
        .child(macro_row(dev, 64., cx))
}

/// The window body.
pub fn render(ws: &mut Workspace, dev: &Dev, _d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let browser = browser(ws, dev, cx);
    let hero = hero(ws, dev, cx);
    let edit = engines::render(ws, dev, cx);
    let globals = engines::globals(ws, dev, cx);
    div()
        .size_full()
        .flex()
        .bg(pal.base)
        .child(browser)
        .child(
            div()
                .id(("vault-main", dev.key(0)))
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .overflow_y_scroll()
                .p(px(SP3))
                .flex()
                .flex_col()
                .gap(px(SP3))
                .child(hero)
                .child(edit)
                .child(globals),
        )
        .into_any_element()
}
