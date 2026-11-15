//! The Plugins tab of the Content Hub: free plugins from their makers, each
//! with its licence in view before anything downloads, an Install that fetches
//! from the maker's own server, and a guided link for what needs an account.
//!
//! What it decides (filtering, order, wording) is plain functions with tests;
//! the installer itself is `aitrack_library::plugins`, reached through the
//! library bridge.

use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{self, InputEvent, TextInput, label, mono, text_button};
use crate::workspace::Workspace;
use aitrack_library::catalog::Lane;
use aitrack_library::plugins::{GuidedWhy, PluginCard, PluginStage, PluginState};
use gpui::{
    AnyElement, AppContext, Context, Entity, Global, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Packs,
    Plugins,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Show {
    #[default]
    All,
    /// Ones the app can install or update right now.
    Ready,
    Installed,
}

/// The tab's own state, kept as a gpui global so the workspace needs no field.
pub struct PluginsUi {
    pub tab: Tab,
    query: Entity<TextInput>,
    category: Option<&'static str>,
    show: Show,
    /// The plugin whose licence panel is open.
    confirm: Option<String>,
    /// Dev view: show a mix of states so every look can be checked without installing anything.
    pub demo: bool,
}

impl Global for PluginsUi {}

/// The tab state, made on first use.
pub fn ui<'a, 'b>(cx: &'a mut Context<'b, Workspace>) -> &'a mut PluginsUi {
    if !cx.has_global::<PluginsUi>() {
        let query = cx.new(|cx| TextInput::new(cx, "Search free plugins", Some(Icon::Search)));
        cx.subscribe(&query, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        cx.set_global(PluginsUi { tab: Tab::Packs, query, category: None, show: Show::All, confirm: None, demo: false });
    }
    cx.global_mut::<PluginsUi>()
}

// ───────────────────────────── What is shown ─────────────────────────────

fn rank(s: &PluginState) -> u8 {
    match s {
        PluginState::Installing { .. } | PluginState::Update { .. } => 0,
        PluginState::Available => 1,
        PluginState::Installed { .. } => 2,
        PluginState::External => 3,
        PluginState::Guided(GuidedWhy::Unpinned) => 4,
        PluginState::Guided(_) => 5,
    }
}

/// The cards to list, in order: what can be done now first, links last.
pub fn visible(cards: &[PluginCard], query: &str, category: Option<&str>, show: Show) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<usize> = (0..cards.len())
        .filter(|&i| {
            let c = &cards[i];
            let e = &c.entry;
            let text = q.is_empty() || [&e.name, &e.vendor, &e.category, &e.description].iter().any(|s| s.to_lowercase().contains(&q)) || e.tags.iter().any(|t| t.to_lowercase().contains(&q));
            let cat = category.is_none_or(|k| e.category == k);
            let by_show = match show {
                Show::All => true,
                Show::Ready => matches!(c.state, PluginState::Available | PluginState::Update { .. } | PluginState::Installing { .. }),
                Show::Installed => matches!(c.state, PluginState::Installed { .. } | PluginState::Update { .. } | PluginState::External),
            };
            text && cat && by_show
        })
        .collect();
    out.sort_by_key(|&i| rank(&cards[i].state));
    out
}

/// The categories the catalog has, in the catalog's own order of first appearance.
pub fn categories(cards: &[PluginCard]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for k in aitrack_library::catalog::CATEGORIES {
        if cards.iter().any(|c| c.entry.category == *k) {
            out.push(*k);
        }
    }
    out
}

pub fn category_label(k: &str) -> &'static str {
    match k {
        "dynamics" => "Dynamics",
        "eq" => "EQ",
        "saturation" => "Saturation",
        "reverb" => "Reverb",
        "delay" => "Delay",
        "modulation" => "Modulation",
        "glitch" => "Glitch",
        "vocal" => "Vocal",
        "utility" => "Utility",
        "synth" => "Synths",
        "sampler" => "Samplers",
        _ => "Multi",
    }
}

fn human_size(b: u64) -> String {
    if b == 0 {
        String::new()
    } else if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.0} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{:.0} KB", b as f64 / 1024.)
    }
}

/// Where plugins go on this system, in words.
fn where_text() -> &'static str {
    if cfg!(target_os = "macos") {
        "~/Library/Audio/Plug-Ins/VST3 and /CLAP"
    } else {
        "%LOCALAPPDATA%\\Programs\\Common\\VST3 and \\CLAP"
    }
}

/// Dev view: open the licence panel of `id`.
pub fn open_confirm(cx: &mut Context<Workspace>, id: &str) {
    ui(cx).confirm = Some(id.to_owned());
}

/// A mix of states for the dev view (`hubplugins=demo`): installing, installed, an update, outside.
fn demo_states(cards: &mut [PluginCard]) {
    let mut set = |id: &str, s: PluginState, size: u64| {
        if let Some(c) = cards.iter_mut().find(|c| c.entry.id == id) {
            c.state = s;
            if size > 0 {
                c.size = size;
                c.host = "www.tokyodawn.net".into();
            }
        }
    };
    set("tdr-kotelnikov", PluginState::Installed { version: "1.6.7".into() }, 6 << 20);
    set("tdr-nova", PluginState::Available, 14 << 20);
    set("tdr-slickeq", PluginState::Installing { stage: PluginStage::Downloading, fraction: 0.46 }, 9 << 20);
    set("surge-xt", PluginState::Update { installed: "1.3.3".into() }, 187 << 20);
    set("dexed", PluginState::External, 0);
}

// ───────────────────────────── Drawing ─────────────────────────────

fn chip(id: impl Into<gpui::ElementId>, text: impl Into<gpui::SharedString>, on: bool) -> gpui::Stateful<gpui::Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(24.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if on { accent_alpha(0.2) } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(label(text, TEXT_SM, if on { pal.accent } else { pal.muted }))
}

/// Text that wraps onto several lines.
fn para(text: impl Into<gpui::SharedString>, color: gpui::Hsla) -> gpui::Div {
    div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(color).child(text.into())
}

fn tag(text: impl Into<gpui::SharedString>) -> gpui::Div {
    div().flex_none().h(px(16.)).px(px(SP1)).flex().items_center().rounded(px(r_sm())).bg(content(0.06)).child(label(text, TEXT_XS, p().faint))
}

fn tile(name: &str) -> gpui::Div {
    let initial: String = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    let hue = name.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize)) % 8;
    div().size(px(32.)).flex_none().rounded(px(r_md())).bg(with_alpha(series(hue), 0.7)).flex().items_center().justify_center().child(label(initial, TEXT_LG, p().text).font_weight(gpui::FontWeight::SEMIBOLD))
}

/// The tab switcher at the top of the Content Hub.
pub fn tab_switch(cx: &mut Context<Workspace>) -> AnyElement {
    let tab = ui(cx).tab;
    let weak = cx.entity().downgrade();
    let on: widgets::OnSelect = std::rc::Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |ws, cx| {
            ui(cx).tab = if i == 1 { Tab::Plugins } else { Tab::Packs };
            let _ = ws;
            cx.notify();
        });
    });
    div().px(px(SP4)).py(px(SP2)).child(widgets::segmented("hub-tab", &[(Some(Icon::Package), "Sound packs"), (Some(Icon::Plug), "Plugins")], tab as usize, 128., on)).into_any_element()
}

/// The Plugins tab body.
pub fn render_tab(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let mut cards = ws.b.library.plugin_cards();
    let (query_entity, category, show, confirm, demo) = {
        let u = ui(cx);
        (u.query.clone(), u.category, u.show, u.confirm.clone(), u.demo)
    };
    if demo {
        demo_states(&mut cards);
    }
    let query = query_entity.read(cx).text().to_owned();
    let order = visible(&cards, &query, category, show);
    let total = cards.len();

    let notice = div()
        .mx(px(SP4))
        .p(px(SP3))
        .rounded(px(r_md()))
        .bg(content(0.05))
        .flex()
        .gap(px(SP2))
        .child(icon(Icon::Globe, 14., pal.accent))
        .child(
            para(
                "Free plugins from their makers. Install downloads the file from the maker's own server, a third party that AI-Track does not host or run. It is checked against a fingerprint we recorded, opened without running any installer, and put in your user plugin folder: no administrator needed. Nothing is downloaded until you press the button.",
                pal.muted,
            )
            .flex_1()
            .min_w(px(0.)),
        );

    let mut cats = div().flex().flex_wrap().items_center().gap(px(SP1)).child(chip("pl-cat-all", "All", category.is_none()).on_click(cx.listener(|_, _, _, cx| {
        ui(cx).category = None;
        cx.notify();
    })));
    for (i, k) in categories(&cards).into_iter().enumerate() {
        cats = cats.child(chip(("pl-cat", i), category_label(k), category == Some(k)).on_click(cx.listener(move |_, _, _, cx| {
            let u = ui(cx);
            u.category = if u.category == Some(k) { None } else { Some(k) };
            cx.notify();
        })));
    }
    let shows = [(Show::All, "Everything"), (Show::Ready, "Can install"), (Show::Installed, "Installed")];
    let mut show_row = div().flex().items_center().gap(px(SP1));
    for (i, (s, text)) in shows.into_iter().enumerate() {
        show_row = show_row.child(chip(("pl-show", i), text, show == s).on_click(cx.listener(move |_, _, _, cx| {
            ui(cx).show = s;
            cx.notify();
        })));
    }
    let controls = div()
        .px(px(SP4))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(div().flex().items_center().gap(px(SP3)).child(div().flex_1().child(query_entity.clone())).child(show_row))
        .child(cats);

    let mut list = div().id("plugin-list").max_h(px(400.)).overflow_y_scroll().px(px(SP4)).pb(px(SP4)).flex().flex_col().gap(px(SP1));
    if order.is_empty() {
        list = list.child(div().p(px(SP4)).child(label("No plugin matches. Clear the search or pick another category.", TEXT_SM, pal.faint)));
    }
    for i in order.iter().copied() {
        list = list.child(render_card(&cards[i], i, confirm.as_deref() == Some(&cards[i].entry.id), cx));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(notice)
        .child(controls)
        .child(div().px(px(SP4 + SP1)).child(mono(format!("{} of {total} plugins", order.len()), TEXT_XS, pal.faint)))
        .child(list)
        .into_any_element()
}

fn render_card(c: &PluginCard, index: usize, confirm_open: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let e = &c.entry;
    let id = e.id.clone();
    let page_button = |idx: usize, text: &'static str, cx: &mut Context<Workspace>| {
        let url = e.page().to_owned();
        text_button(("pl-page", idx), text, Some(Icon::Globe), false).on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.open_url(&url);
        }))
    };
    let mut right = div().flex_none().flex().items_center().gap(px(SP2));
    let mut status: Option<AnyElement> = None;
    match &c.state {
        PluginState::Available => {
            if c.size > 0 {
                right = right.child(mono(human_size(c.size), TEXT_XS, pal.faint));
            }
            let id = id.clone();
            right = right.child(text_button(("pl-install", index), "Install", Some(Icon::Download), true).on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                let u = ui(cx);
                u.confirm = if u.confirm.as_deref() == Some(&id) { None } else { Some(id.clone()) };
                cx.notify();
            })));
        }
        PluginState::Update { installed } => {
            status = Some(label(format!("Installed v{installed} · v{} available", e.version), TEXT_XS, pal.accent).into_any_element());
            let id1 = id.clone();
            let id2 = id.clone();
            right = right
                .child(text_button(("pl-update", index), "Update", Some(Icon::Refresh), true).on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    let u = ui(cx);
                    u.confirm = if u.confirm.as_deref() == Some(&id1) { None } else { Some(id1.clone()) };
                    cx.notify();
                })))
                .child(text_button(("pl-remove", index), "Uninstall", Some(Icon::Trash), false).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    uninstall(this, &id2, cx);
                })));
        }
        PluginState::Installing { stage, fraction } => {
            let text = if *stage == PluginStage::Downloading || *fraction > 0. { format!("{} {:.0}%", stage.label(), fraction * 100.) } else { format!("{}…", stage.label()) };
            right = right.child(div().w(px(120.)).flex().flex_col().gap(px(2.)).child(label(text, TEXT_XS, pal.muted)).child(widgets::hbar(*fraction, 120., 3., pal.accent)));
            let id = id.clone();
            right = right.child(text_button(("pl-cancel", index), "Cancel", Some(Icon::X), false).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.b.library.cancel_plugin_install(&id);
                cx.notify();
            })));
        }
        PluginState::Installed { version } => {
            right = right.child(label(format!("Installed · v{version}"), TEXT_SM, pal.play));
            let id = id.clone();
            right = right.child(text_button(("pl-remove", index), "Uninstall", Some(Icon::Trash), false).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                uninstall(this, &id, cx);
            })));
        }
        PluginState::External => {
            right = right.child(label("Installed outside the app", TEXT_SM, pal.muted));
        }
        PluginState::Guided(why) => {
            let note = match why {
                GuidedWhy::Link if e.requires_account => "Free account or manager needed",
                GuidedWhy::Link => "Download it from the maker's page",
                GuidedWhy::Unpinned => "Not verified yet: install from the maker's page",
                GuidedWhy::NoBuild => "No build the app can open for this system",
            };
            status = Some(label(note, TEXT_XS, pal.faint).into_any_element());
            right = right.child(page_button(index, "Open page", cx));
        }
    }
    let mut tags = div().flex().items_center().gap(px(SP1)).overflow_hidden().child(tag(e.license.clone()));
    for f in &e.formats {
        tags = tags.child(tag(f.to_uppercase()));
    }
    tags = tags.child(tag(category_label(&e.category)));
    if e.lane == Lane::Link && e.requires_account {
        tags = tags.child(widgets::badge("Account", pal.solo));
    }
    let mut body = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(2.)).child(div().flex().items_center().gap(px(SP2)).child(label(e.name.clone(), TEXT_MD, pal.text)).child(label(e.vendor.clone(), TEXT_XS, pal.faint))).child(tags);
    if let Some(s) = status {
        body = body.child(s);
    }
    if let Some(err) = &c.error {
        body = body.child(label(crate::paint::fit(err, 440., TEXT_XS), TEXT_XS, pal.record));
    }
    let row = div()
        .id(("pl-card", index))
        .px(px(SP3))
        .py(px(SP2))
        .gap(px(SP3))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .hover(|s| s.bg(hover()))
        .child(tile(&e.name))
        .child(body)
        .child(right)
        .tooltip(widgets::tip_owned(if e.description.is_empty() { e.homepage.clone() } else { format!("{}\n{}", e.description, e.homepage) }));
    let mut card = div().flex().flex_col().child(row);
    if confirm_open {
        card = card.child(confirm_panel(c, cx));
    }
    card.into_any_element()
}

/// Everything the user should read before the download starts.
fn confirm_panel(c: &PluginCard, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let e = &c.entry;
    let id = e.id.clone();
    let update = matches!(c.state, PluginState::Update { .. });
    let host = if c.host.is_empty() { "the maker's server".to_owned() } else { c.host.clone() };
    let size = human_size(c.size);
    let go = text_button(("pl-go", 0usize), if update { "Download and update" } else { "Download and install" }, Some(Icon::Download), true).on_click(cx.listener(move |this, _, _, cx| {
        cx.stop_propagation();
        ui(cx).confirm = None;
        if let Err(msg) = this.b.library.install_plugin(&id) {
            this.toast(msg, cx);
        }
        cx.notify();
    }));
    let page = e.homepage.clone();
    div()
        .mx(px(SP3))
        .mb(px(SP2))
        .p(px(SP3))
        .rounded(px(r_md()))
        .bg(content(0.05))
        .border_1()
        .border_color(border())
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(label("BEFORE YOU DOWNLOAD", TEXT_XS, pal.faint).font_weight(gpui::FontWeight::SEMIBOLD))
        .child(label(format!("Licence: {}{}", e.license, if e.license_url.is_empty() { String::new() } else { format!(" ({})", e.license_url) }), TEXT_SM, pal.text))
        .child(para(e.license_notice.clone(), pal.muted))
        .child(para(
            format!(
                "The file comes from {host}{}. That is a third party: AI-Track does not host, build or review it. It is installed only if it matches the fingerprint we recorded, for your user only in {}.",
                if size.is_empty() { String::new() } else { format!(" ({size})") },
                where_text()
            ),
            pal.muted,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(go)
                .child(text_button(("pl-no", 0usize), "Not now", None, false).on_click(cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    ui(cx).confirm = None;
                    cx.notify();
                })))
                .child(div().flex_1())
                .child(text_button(("pl-home", 0usize), "Maker's page", Some(Icon::Globe), false).on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&page);
                }))),
        )
        .into_any_element()
}

fn uninstall(ws: &mut Workspace, id: &str, cx: &mut Context<Workspace>) {
    match ws.b.library.uninstall_plugin(id) {
        Ok(msg) => {
            ws.toast(msg, cx);
            // The host forgets it on its next scan: a vanished bundle is dropped from the list.
            ws.b.plugins.rescan();
        }
        Err(e) => ws.toast(e, cx),
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_library::Catalog;

    fn cards() -> Vec<PluginCard> {
        Catalog::bundled()
            .plugin
            .into_iter()
            .map(|entry| PluginCard {
                state: if entry.lane == Lane::Link { PluginState::Guided(GuidedWhy::Link) } else { PluginState::Guided(GuidedWhy::Unpinned) },
                entry,
                error: None,
                size: 0,
                host: String::new(),
            })
            .collect()
    }

    fn names(c: &[PluginCard], idx: &[usize]) -> Vec<String> {
        idx.iter().map(|&i| c[i].entry.id.clone()).collect()
    }

    #[test]
    fn search_matches_names_makers_and_tags() {
        let c = cards();
        assert!(names(&c, &visible(&c, "kotel", None, Show::All)).contains(&"tdr-kotelnikov".to_owned()));
        let tokyo = names(&c, &visible(&c, "tokyo dawn", None, Show::All));
        assert!(tokyo.contains(&"tdr-nova".to_owned()) && !tokyo.contains(&"vital".to_owned()));
        assert!(names(&c, &visible(&c, "vocoder", None, Show::All)).contains(&"tal-vocoder".to_owned()), "tag and name");
        assert!(visible(&c, "zzzz", None, Show::All).is_empty());
    }

    #[test]
    fn category_and_status_filters_and_the_order_of_states() {
        let mut c = cards();
        let reverbs = visible(&c, "", Some("reverb"), Show::All);
        assert!(!reverbs.is_empty() && reverbs.iter().all(|&i| c[i].entry.category == "reverb"));
        let nova = c.iter().position(|x| x.entry.id == "tdr-nova").unwrap();
        let vital = c.iter().position(|x| x.entry.id == "vital").unwrap();
        let kot = c.iter().position(|x| x.entry.id == "tdr-kotelnikov").unwrap();
        c[nova].state = PluginState::Available;
        c[kot].state = PluginState::Installed { version: "1".into() };
        assert_eq!(visible(&c, "", None, Show::Ready), vec![nova], "only what can be installed now");
        assert_eq!(visible(&c, "", None, Show::Installed), vec![kot]);
        let all = visible(&c, "", None, Show::All);
        let pos = |i| all.iter().position(|&x| x == i).unwrap();
        assert!(pos(nova) < pos(kot) && pos(kot) < pos(vital), "installable, installed, then links");
    }

    #[test]
    fn categories_follow_the_catalog_and_every_one_has_a_label() {
        let c = cards();
        let cats = categories(&c);
        assert!(cats.contains(&"dynamics") && cats.contains(&"synth"));
        for k in aitrack_library::catalog::CATEGORIES {
            assert!(!category_label(k).is_empty());
        }
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(0), "");
        assert_eq!(human_size(14 << 20), "14 MB");
        assert_eq!(human_size(3 << 29), "1.5 GB");
    }
}
