//! The browser: the smart library (virtual folders by category, key and
//! genre), plugins (stock, VST3, CLAP) and the project's own contents.

use crate::bridge::LibraryFolder;
use crate::icons::{Icon, icon};
use crate::state::BrowserTab;
use crate::theme::*;
use crate::widgets::{self, badge, label, mono};
use crate::workspace::{DraggedSample, MenuKind, Workspace};
use aitrack_model::{PluginFormat, Scale, ScaleKind, TrackKind};
use gpui::{
    AnyElement, AppContext as _, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, uniform_list,
};
use std::rc::Rc;

/// "Am", "C", "F#m" — the compact key a badge shows.
pub fn short_key(s: Scale) -> String {
    let minor = matches!(s.kind, ScaleKind::Minor | ScaleKind::HarmonicMinor | ScaleKind::MelodicMinor | ScaleKind::Dorian | ScaleKind::Phrygian | ScaleKind::MinorPentatonic);
    format!("{}{}", s.root.name(), if minor { "m" } else { "" })
}

fn row(id: impl Into<gpui::ElementId>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(ROW_H))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .when(selected, |d| d.bg(accent_alpha(0.14)))
        .when(!selected, |d| d.hover(|s| s.bg(hover())))
}

fn section(title: &'static str, count: usize) -> Div {
    let pal = p();
    div()
        .h(px(24.))
        .px(px(SP2))
        .mt(px(SP1))
        .flex()
        .items_center()
        .justify_between()
        .child(label(title, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(mono(count.to_string(), TEXT_XS, pal.faint))
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let weak = cx.entity().downgrade();
    let tab = ws.s.browser_tab;
    // The Project tab is optional (Preferences → Appearance); without it the
    // browser is just sounds and devices.
    let with_project = ws.look.app.project_tab;
    let tab = if tab == BrowserTab::Project && !with_project { BrowserTab::Library } else { tab };
    let order: &[BrowserTab] = if with_project { &[BrowserTab::Library, BrowserTab::Plugins, BrowserTab::Project] } else { &[BrowserTab::Library, BrowserTab::Plugins] };
    let index = order.iter().position(|t| *t == tab).unwrap_or(0);
    let on_tab: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |this, cx| {
            let with_project = this.look.app.project_tab;
            this.s.browser_tab = match i {
                1 => BrowserTab::Plugins,
                2 if with_project => BrowserTab::Project,
                _ => BrowserTab::Library,
            };
            cx.notify();
        });
    });
    let width = ws.browser_w;
    let all = [(Some(Icon::Sample), "Library"), (Some(Icon::Plug), "Plugins"), (Some(Icon::Folder), "Project")];
    let tabs = widgets::segmented("browser-tabs", &all[..order.len()], index, (width - 2. * SP2 - 6.) / order.len() as f32, on_tab);
    let body = match tab {
        BrowserTab::Library => library(ws, cx),
        BrowserTab::Plugins => plugins(ws, cx),
        BrowserTab::Project => project(ws, cx),
    };
    div()
        .flex_none()
        .w(px(width))
        .relative()
        .h_full()
        .flex()
        .flex_col()
        .bg(pane_bg(crate::appearance::Pane::Browser))
        .rounded(px(r_lg()))
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .child(div().p(px(SP2)).pb(px(0.)).flex().flex_col().gap(px(SP2)).child(tabs).child(ws.search.clone()))
        .child(crate::motion::fade_in(gpui::ElementId::Name(format!("browser-{index}").into()), div().flex_1().min_h(px(0.)).flex().flex_col().child(body)))
        .child(
            // Drag the right edge to resize; double-click for the default width.
            div()
                .id("browser-grip")
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(5.))
                .cursor_ew_resize()
                .hover(|s| s.bg(accent_alpha(0.5)))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if e.click_count >= 2 {
                            this.browser_w = BROWSER_W;
                            cx.notify();
                        } else {
                            let start_w = this.browser_w;
                            this.begin_drag(crate::workspace::Drag::BrowserResize { start_x: f32::from(e.position.x), start_w }, cx);
                        }
                    }),
                ),
        )
        .into_any_element()
}

fn library(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let query = ws.query.trim().to_owned();
    let title = if !query.is_empty() {
        format!("Results for “{query}”")
    } else if ws.library_matches {
        let key = ws.s.project.scale.map_or("any key".to_owned(), |s| s.display());
        format!("Matches {key} · {:.0} BPM", ws.s.project.tempo.initial_bpm())
    } else {
        find_folder(ws.b.library.folders(), &ws.library_folder).map_or("Library".to_owned(), |f| f.name.clone())
    };
    let items = ws.library_view();
    let scan = ws.b.library.scan_status();
    let status = ws.b.library.status();
    let mut tree = div().flex().flex_col().px(px(SP1));
    if query.is_empty() && !ws.library_matches {
        let folders = ws.b.library.folders().to_vec();
        for f in &folders {
            tree = folder_rows(ws, tree, f, 0, cx);
        }
    }
    let items = Rc::new(items);
    let count = items.len();
    let list_items = items.clone();
    let project_key = ws.s.project.scale;
    let weak = cx.entity().downgrade();
    let list = uniform_list("library-items", count, move |range, _, _| {
        range
            .map(|i| {
                let it = &list_items[i];
                let pal = p();
                let in_key = it.key.zip(project_key).is_some_and(|(a, b)| a.mask() == b.mask());
                let item = it.clone();
                let fav_item = it.clone();
                let weak = weak.clone();
                let weak_fav = weak.clone();
                let weak_menu = weak.clone();
                row(("lib-item", i), false)
                    .mx(px(SP1))
                    .child(icon(if it.is_loop { Icon::Loop } else { Icon::Waveform }, 14., if in_key { pal.accent } else { pal.muted }))
                    .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label(it.name.clone(), TEXT_MD, pal.text)))
                    .when_some(it.key, |d, k| d.child(badge(short_key(k), if in_key { pal.accent } else { pal.muted })))
                    .when_some(it.bpm, |d, b| d.child(badge(format!("{b:.0}"), pal.solo)))
                    .child(mono(format!("{:.1}s", it.duration), TEXT_XS, pal.faint).w(px(32.)).flex().justify_end())
                    .child(
                        div()
                            .id(("fav", i))
                            .child(icon(Icon::Star, 12., if it.favorite { pal.solo } else { content(0.2) }))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = weak_fav.update(cx, |this, cx| {
                                    this.b.library.set_favorite(fav_item.id, !fav_item.favorite);
                                    cx.notify();
                                });
                            }),
                    )
                    .tooltip(widgets::tip_owned(format!("{} · {}\nClick to preview, double-click or drag to add", it.category, it.path.display())))
                    .on_drag(DraggedSample(it.clone()), |d: &DraggedSample, _, _, cx| cx.new(|_| d.clone()))
                    .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| {
                        let at = e.position;
                        let _ = weak_menu.update(cx, |this, cx| this.open_menu(MenuKind::LibraryContext, at, cx));
                    })
                    .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                        let double = e.click_count >= 2;
                        let item = item.clone();
                        let _ = weak.update(cx, |this, cx| {
                            if double {
                                this.b.engine.preview(None);
                                this.add_sample_clip(&item, None, None, cx);
                            } else {
                                this.b.engine.preview(Some(&item.path));
                            }
                        });
                    })
            })
            .collect::<Vec<_>>()
    })
    .flex_1()
    .min_h(px(0.));

    let matches_on = ws.library_matches;
    let toolbar = div()
        .px(px(SP2))
        .pt(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(
            div()
                .id("lib-matches")
                .h(px(22.))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP1))
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if matches_on { accent_alpha(0.18) } else { content(0.06) })
                .hover(|s| s.bg(content(0.12)))
                .child(icon(Icon::Sparkles, 12., if matches_on { pal.accent } else { pal.muted }))
                .child(label("Matches project", TEXT_XS, if matches_on { pal.text } else { pal.muted }))
                .tooltip(widgets::tip("Ranked for the project key and tempo", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.library_matches = !this.library_matches;
                    cx.notify();
                })),
        )
        .child(div().flex_1())
        .child(widgets::icon_button("lib-add-folder", Icon::Add, false).tooltip(widgets::tip("Add a folder to the library", None)).on_click(cx.listener(|this, _, _, cx| this.pick_library_folder(cx))))
        .child(widgets::icon_button("lib-menu", Icon::More, false).tooltip(widgets::tip("Library actions", None)).on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
            let at = e.position();
            this.open_menu(MenuKind::LibraryContext, gpui::point(at.x - px(180.), at.y + px(12.)), cx);
        })));
    let scan_row = scan.running.then(|| {
        div()
            .px(px(SP3))
            .pt(px(SP1))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(label(crate::paint::fit(&scan.message, ws.browser_w - 32., TEXT_XS), TEXT_XS, pal.muted))
            .child(widgets::hbar(scan.fraction, ws.browser_w - 2. * SP3, 3., pal.accent))
    });
    div()
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .child(toolbar)
        .children(scan_row)
        .when(query.is_empty() && !matches_on, |d| {
            d.child(div().id("folder-tree").max_h(px(220.)).overflow_y_scroll().pt(px(SP2)).child(tree)).child(div().mx(px(SP3)).my(px(SP1)).child(widgets::divider_h()))
        })
        .child(
            div()
                .h(px(24.))
                .px(px(SP3))
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(icon(Icon::FolderOpen, 12., pal.faint))
                .child(label(title, TEXT_XS, pal.muted).font_weight(FontWeight::SEMIBOLD).flex_1())
                .child(mono(format!("{count}"), TEXT_XS, pal.faint)),
        )
        .child(if count == 0 {
            div().p(px(SP4)).child(label("Nothing here yet.", TEXT_MD, pal.faint)).into_any_element()
        } else {
            list.into_any_element()
        })
        .child(div().flex_none().px(px(SP3)).py(px(SP1)).border_t_1().border_color(border()).child(label(crate::paint::fit(&status, ws.browser_w - 24., TEXT_XS), TEXT_XS, pal.faint)))
        .into_any_element()
}

fn find_folder<'a>(folders: &'a [LibraryFolder], id: &str) -> Option<&'a LibraryFolder> {
    folders.iter().find_map(|f| if f.id == id { Some(f) } else { find_folder(&f.children, id) })
}

fn folder_rows(ws: &Workspace, mut tree: Div, f: &LibraryFolder, depth: usize, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let open = ws.expanded.contains(&f.id);
    let selected = ws.library_folder == f.id;
    let has_children = !f.children.is_empty();
    let id = f.id.clone();
    let top_icon = match f.id.as_str() {
        "categories" => Icon::FolderTree,
        "keys" => Icon::MusicNote,
        "genres" => Icon::Globe,
        "camelot" => Icon::Refresh,
        "bpm" => Icon::Gauge,
        "packs" => Icon::Layers,
        "favorites" => Icon::Star,
        _ if f.id.starts_with("keys/") || f.id.starts_with("camelot/") => Icon::MusicNote,
        _ => if open { Icon::FolderOpen } else { Icon::Folder },
    };
    tree = tree.child(
        row(gpui::ElementId::Name(format!("folder-{}", f.id).into()), selected)
            .h(px(24.))
            .pl(px(SP2 + depth as f32 * SP3))
            .child(
                div().size(px(12.)).flex_none().when(has_children, |d| {
                    d.child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12., pal.faint))
                }),
            )
            .child(icon(top_icon, 14., if selected { pal.accent } else { pal.muted }))
            .child(label(f.name.clone(), TEXT_MD, if selected { pal.text } else { pal.muted }).when(depth == 0, |l| l.font_weight(FontWeight::MEDIUM)).flex_1())
            .when(f.count > 0, |d| d.child(mono(f.count.to_string(), TEXT_XS, pal.faint)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded.remove(&id) {
                    this.expanded.insert(id.clone());
                }
                this.library_folder = id.clone();
                cx.notify();
            })),
    );
    if open {
        for c in &f.children {
            tree = folder_rows(ws, tree, c, depth + 1, cx);
        }
    }
    tree
}

fn plugins(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let q = ws.query.trim().to_lowercase();
    let matches = |name: &str, tags: &[&str]| q.is_empty() || name.to_lowercase().contains(&q) || tags.iter().any(|t| t.contains(&q));
    let track = ws.s.selected_track;
    let mut col = div().flex().flex_col().px(px(SP1)).pb(px(SP2));
    let stock: Vec<_> = ws.b.plugins.stock_devices().iter().filter(|d| matches(d.name, d.tags)).map(|d| (d.id, d.name, d.category, d.is_instrument)).collect();
    col = col.child(section("STOCK DEVICES", stock.len()));
    for (i, (id, name, cat, instrument)) in stock.into_iter().enumerate() {
        col = col.child(
            row(("stock", i), false)
                .child(icon(crate::views::menus::device_icon(id, ws), 14., pal.accent))
                .child(label(name, TEXT_MD, pal.text).flex_1())
                .when(instrument, |d| d.child(icon(Icon::MusicNote, 12., pal.faint)))
                .child(badge(cat.label(), pal.muted))
                .tooltip(widgets::tip("Click to add to the selected track", None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(t) = track
                        && this.s.add_stock_device(t, id, name).is_some() {
                            this.set_tab(crate::state::BottomTab::Devices, cx);
                            this.toast(format!("Added {name}"), cx);
                        }
                })),
        );
    }
    for (fmt, title) in [(PluginFormat::Vst3, "VST3"), (PluginFormat::Clap, "CLAP")] {
        let list: Vec<_> = ws.b.plugins.external_plugins().iter().filter(|p| p.format == fmt && matches(&p.name, &[])).cloned().collect();
        col = col.child(section(title, list.len()));
        for (i, plug) in list.into_iter().enumerate() {
            let name = plug.name.clone();
            col = col.child(
                row(gpui::ElementId::Name(format!("ext-{title}-{i}").into()), false)
                    .child(icon(if plug.is_instrument { Icon::Piano } else { Icon::Plug }, 14., pal.muted))
                    .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(label(plug.name.clone(), TEXT_MD, pal.text)))
                    .child(label(plug.vendor.clone(), TEXT_XS, pal.faint))
                    .tooltip(widgets::tip_owned(plug.path.display().to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(t) = track
                            && this.s.add_plugin_device(t, &plug).is_some() {
                                this.set_tab(crate::state::BottomTab::Devices, cx);
                                this.toast(format!("Added {name}"), cx);
                            }
                    })),
            );
        }
    }
    div().id("plugins-scroll").flex_1().min_h(px(0.)).overflow_y_scroll().pt(px(SP1)).child(col).into_any_element()
}

pub fn kind_icon(kind: TrackKind) -> Icon {
    match kind {
        TrackKind::Instrument => Icon::Piano,
        TrackKind::Audio => Icon::Waveform,
        TrackKind::Group => Icon::Group,
        TrackKind::Folder => Icon::Folder,
        TrackKind::Return => Icon::Return,
        TrackKind::Vca => Icon::Sliders,
        TrackKind::Main => Icon::Volume,
    }
}

fn project(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let p = &ws.s.project;
    let mut col = div().flex().flex_col().px(px(SP1)).pb(px(SP2));
    col = col.child(section("TRACKS", p.tracks.len()));
    for t in &p.tracks {
        let id = t.id;
        let clips = t.session_slots.len() + t.arrangement.len();
        let selected = ws.s.selected_track == Some(id);
        col = col.child(
            row(("proj-track", t.id.0 as usize), selected)
                .when(t.parent.is_some(), |d| d.pl(px(SP2 + SP3)))
                .child(div().w(px(3.)).h(px(14.)).rounded(px(2.)).bg(track_color(t.color)))
                .child(icon(kind_icon(t.kind), 14., pal.muted))
                .child(label(t.name.clone(), TEXT_MD, pal.text).flex_1())
                .when(clips > 0, |d| d.child(mono(format!("{clips}"), TEXT_XS, pal.faint)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.s.selected_track = Some(id);
                    cx.notify();
                })),
        );
    }
    col = col.child(section("SCENES", p.scenes.len()));
    for (i, s) in p.scenes.iter().enumerate() {
        col = col.child(row(("proj-scene", i), false).child(icon(Icon::Play, 12., pal.faint)).child(label(s.name.clone(), TEXT_MD, pal.muted)));
    }
    let audio: Vec<String> = p
        .sources
        .values()
        .filter_map(|s| match s {
            aitrack_model::Source::Audio(a) => Some(a.path.display().to_string()),
            _ => None,
        })
        .collect();
    col = col.child(section("AUDIO FILES", audio.len()));
    for (i, a) in audio.into_iter().enumerate() {
        col = col.child(row(("proj-audio", i), false).child(icon(Icon::File, 14., pal.muted)).child(label(a, TEXT_SM, pal.muted)));
    }
    div().id("project-scroll").flex_1().min_h(px(0.)).overflow_y_scroll().pt(px(SP1)).child(col).into_any_element()
}
