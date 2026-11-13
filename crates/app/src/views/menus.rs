//! Popover menus: key/scale picker, snap grid, piano roll tools, device
//! picker and time signature.

use crate::bridge::{DeviceCategory, ExternalPlugin};
use crate::icons::Icon;
use crate::state::Snap;
use crate::theme::*;
use crate::views::palette::devices::{DeviceGroup, classify_external, classify_stock, external_key, stock_key};
use crate::widgets::{self, label, menu_header, menu_item, mono};
use crate::workspace::{Menu, MenuKind, Workspace};
use aitrack_model::{Key, PluginFormat, Scale, ScaleKind, TimeSignature};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

pub fn render(ws: &mut Workspace, menu: &Menu, window: &mut Window, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    Some(match &menu.kind {
        MenuKind::Scale => scale_menu(ws, ScaleTarget::Project, cx),
        MenuKind::TrackScale(track) => scale_menu(ws, ScaleTarget::Track(*track), cx),
        MenuKind::ClipScale(clip) => scale_menu(ws, ScaleTarget::Clip(*clip), cx),
        MenuKind::Snap => snap_menu(ws, cx),
        MenuKind::PianoTools => tools_menu(ws, cx),
        MenuKind::AddDevice(track) => device_menu(ws, *track, window, cx),
        MenuKind::Presets(device) => preset_menu(ws, *device, cx)?,
        MenuKind::LibraryContext => library_menu(ws, cx),
        MenuKind::ClipContext => crate::editing::menus::clip_menu(ws, cx),
        MenuKind::LaneContext => crate::editing::menus::lane_menu(cx),
        MenuKind::TrackContext(track) => crate::editing::menus::track_menu(ws, *track, cx),
        MenuKind::DeviceContext(device) => crate::editing::menus::device_menu(ws, *device, cx),
        MenuKind::PaletteDevice { key, name, instrument } => palette_device_menu(ws, key, name, *instrument, cx),
        MenuKind::TimeSignature => sig_menu(ws, cx),
        MenuKind::File => file_menu(ws, cx),
    })
}

/// File: new from a template, open, open recent, save, export.
fn file_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    use crate::project_io::Template;
    let pal = p();
    let mac = cfg!(target_os = "macos");
    let key = |win: &'static str, mac_key: &'static str| Some(if mac { mac_key } else { win });
    let close_recent = |el: gpui::Stateful<gpui::Div>, cx: &mut Context<Workspace>| {
        el.on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if *hovered && this.file_recent_open {
                this.file_recent_open = false;
                cx.notify();
            }
        }))
    };
    let file = ws.project_path.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).map(str::to_owned);
    let mut card = widgets::menu_card("file-menu", 320.).child(menu_header("NEW"));
    for (i, t) in Template::ALL.into_iter().enumerate() {
        let item = close_recent(
            menu_item(("file-new", i), Some(if t == Template::Empty { Icon::File } else { Icon::Layers }), t.label(), if t == Template::Empty { key("Ctrl+N", "⌘N") } else { None }, false)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_menu(cx);
                    this.file_new(t, window, cx);
                })),
            cx,
        );
        card = card.child(
            div()
                .child(item)
                .child(div().pl(px(SP6 + SP2)).pr(px(SP2)).pb(px(SP1)).child(label(t.hint(), TEXT_XS, pal.faint))),
        );
    }

    let open_recent_trigger = div()
        .id("file-open-recent")
        .h(px(ROW_H))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .when(ws.file_recent_open, |d| d.bg(hover()))
        .hover(|s| s.bg(hover()))
        .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(crate::icons::icon(Icon::Clock, 14., if ws.file_recent_open { pal.accent } else { pal.muted })))
        .child(label("Open Recent", TEXT_MD, pal.text).flex_1())
        .child(crate::icons::icon(Icon::ChevronRight, 14., if ws.file_recent_open { pal.accent } else { pal.faint }))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if *hovered && !this.file_recent_open {
                this.file_recent_open = true;
                cx.notify();
            }
        }))
        .on_click(cx.listener(|this, _, _, cx| {
            this.file_recent_open = !this.file_recent_open;
            cx.notify();
        }));

    let recent_submenu = ws.file_recent_open.then(|| {
        let mut sub_list = div()
            .id("file-recent-list")
            .max_h(px(360.))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if ws.recent_projects.is_empty() {
            sub_list = sub_list.child(
                div()
                    .h(px(ROW_H))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .child(label("No recent projects", TEXT_SM, pal.faint)),
            );
        } else {
            for (i, path) in ws.recent_projects.iter().enumerate() {
                let (name, dir) = crate::project_io::recent_project_display(path);
                let full_path = path.display().to_string();
                let open_path = path.clone();
                sub_list = sub_list.child(
                    div()
                        .id(("file-recent-item", i))
                        .h(px(36.))
                        .px(px(SP2))
                        .gap(px(SP2))
                        .flex()
                        .items_center()
                        .rounded(px(r_md()))
                        .cursor_pointer()
                        .hover(|s| s.bg(hover()))
                        .tooltip(widgets::tip_owned(full_path))
                        .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(crate::icons::icon(Icon::File, 14., pal.muted)))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .justify_center()
                                .child(label(name, TEXT_MD, pal.text).truncate())
                                .when(!dir.is_empty(), |d| d.child(label(dir, TEXT_XS, pal.faint).truncate())),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let target = open_path.clone();
                            this.close_menu(cx);
                            this.file_open_path(target, window, cx);
                        })),
                );
            }
            sub_list = sub_list.child(widgets::divider_h()).child(
                menu_item("file-recent-clear", Some(Icon::Trash), "Clear Recent", None, false).on_click(cx.listener(|this, _, _, cx| {
                    this.clear_recent_projects(cx);
                })),
            );
        }
        div()
            .id("file-recent-submenu")
            .occlude()
            .absolute()
            .left_full()
            .top(px(-SP1 - 1.))
            .pl(px(4.))
            .child(widgets::menu_card("file-recent-card", 300.).child(menu_header("RECENT PROJECTS")).child(sub_list))
    });

    card.child(menu_header(file.map_or("PROJECT".to_owned(), |f| f.to_uppercase())))
        .child(close_recent(
            menu_item("file-open", Some(Icon::FolderOpen), "Open…", key("Ctrl+O", "⌘O"), false).on_click(cx.listener(|this, _, window, cx| {
                this.close_menu(cx);
                this.file_open(window, cx);
            })),
            cx,
        ))
        .child(div().relative().child(open_recent_trigger).children(recent_submenu))
        .child(close_recent(
            menu_item("file-save", Some(Icon::Check), "Save", key("Ctrl+S", "⌘S"), false).on_click(cx.listener(|this, _, _, cx| {
                this.close_menu(cx);
                this.file_save(cx);
            })),
            cx,
        ))
        .child(close_recent(
            menu_item("file-save-as", Some(Icon::File), "Save as…", key("Ctrl+Shift+S", "⇧⌘S"), false).on_click(cx.listener(|this, _, _, cx| {
                this.close_menu(cx);
                this.file_save_as(None, cx);
            })),
            cx,
        ))
        .child(close_recent(
            menu_item("file-shortcuts", Some(Icon::Keyboard), "Keyboard shortcuts…", key("Ctrl+/", "⌘/"), false).on_click(cx.listener(|this, _, window, cx| {
                this.close_menu(cx);
                crate::commands::run(this, crate::keys::Cmd::Shortcuts, window, cx);
            })),
            cx,
        ))
        .child(close_recent(
            menu_item("file-export", Some(Icon::Send), "Export audio…", key("Ctrl+Shift+E", "⇧⌘E"), false).on_click(cx.listener(|this, _, _, cx| {
                this.close_menu(cx);
                this.open_export(cx);
            })),
            cx,
        ))
        .into_any_element()
}

#[derive(Clone, Copy)]
enum ScaleTarget {
    Project,
    Track(aitrack_model::TrackId),
    Clip(aitrack_model::ClipId),
}

fn scale_menu(ws: &mut Workspace, target: ScaleTarget, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let current = match target {
        ScaleTarget::Project => ws.s.project.scale,
        ScaleTarget::Track(id) => ws.s.project.track(id).and_then(|t| t.scale),
        ScaleTarget::Clip(id) => ws.s.project.clips.get(&id).and_then(|c| c.scale),
    };
    let effective = match target {
        ScaleTarget::Project => current,
        ScaleTarget::Track(_) => current.or(ws.s.project.scale),
        ScaleTarget::Clip(id) => ws.s.clip_scale(Some(id)),
    };
    let root = effective.map_or(Key(9), |s| s.root);
    let kind = effective.map_or(ScaleKind::Minor, |s| s.kind);
    let apply_scale = move |this: &mut Workspace, next: Option<Scale>| match target {
        ScaleTarget::Project => { this.s.set_scale(next); }
        ScaleTarget::Track(id) => { this.s.set_track_scale(id, next); }
        ScaleTarget::Clip(id) => { this.s.set_clip_scale(id, next); }
    };
    let title = match target {
        ScaleTarget::Project => "PROJECT KEY · ROOT",
        ScaleTarget::Track(_) => "TRACK KEY · ROOT",
        ScaleTarget::Clip(_) => "CLIP KEY · ROOT",
    };
    let clear_label = match target {
        ScaleTarget::Project => "No key",
        ScaleTarget::Track(_) => "Inherit project key",
        ScaleTarget::Clip(_) => "Inherit track / project key",
    };
    let mut grid = div().flex().flex_wrap().gap(px(SP1)).px(px(SP1)).pb(px(SP1));
    for k in 0..12u8 {
        let on = current.is_some() && root.0 == k;
        let black = matches!(k, 1 | 3 | 6 | 8 | 10);
        grid = grid.child(
            div()
                .id(("root", k as usize))
                .w(px(36.))
                .h(px(CONTROL_H))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if on { pal.accent } else if black { content(0.04) } else { content(0.08) })
                .when(!on, |d| d.hover(|s| s.bg(content(0.14))))
                .child(label(Key(k).name(), TEXT_SM, if on { pal.on_accent } else { pal.text }).font_weight(FontWeight::MEDIUM))
                .on_click(cx.listener(move |this, _, _, cx| {
                    apply_scale(this, Some(Scale::new(Key(k), kind)));
                    cx.notify();
                })),
        );
    }
    let mut list = div().flex().flex_col();
    for (i, sk) in ScaleKind::ALL.iter().copied().enumerate() {
        let on = current.is_some_and(|s| s.kind == sk);
        list = list.child(
            menu_item(("scale-kind", i), None, sk.label(), None, on).on_click(cx.listener(move |this, _, _, cx| {
                apply_scale(this, Some(Scale::new(root, sk)));
                this.menu = None;
                cx.notify();
            })),
        );
    }
    widgets::menu_card("scale-menu", 248.)
        .child(menu_header(title))
        .child(grid)
        .child(widgets::divider_h())
        .child(menu_header("SCALE"))
        .child(list)
        .child(widgets::divider_h())
        .child(menu_item("no-key", Some(Icon::X), clear_label, None, current.is_none()).on_click(cx.listener(move |this, _, _, cx| {
            apply_scale(this, None);
            this.menu = None;
            cx.notify();
        })))
        .into_any_element()
}

fn snap_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let mut card = widgets::menu_card("snap-menu", 176.).child(menu_header("SNAP GRID"));
    for (i, s) in Snap::ALL.iter().copied().enumerate() {
        // Adaptive follows the zoom; Ctrl+1 / Ctrl+2 make it finer or coarser.
        let hint = (s == Snap::Adaptive).then_some("follows zoom");
        card = card.child(
            menu_item(("snap", i), Some(Icon::Magnet), s.label(), hint, ws.s.snap == s).on_click(cx.listener(move |this, _, _, cx| {
                this.s.snap = s;
                this.menu = None;
                cx.notify();
            })),
        );
    }
    card.into_any_element()
}

fn tools_menu(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    use crate::state::{ArpPattern, ChordKind};
    let scale_label = ws.s.clip_scale(ws.s.selected_clip).map(|s| s.display()).unwrap_or_else(|| "Chromatic".into());
    let chord_btn = |id: &'static str, title: &'static str, kind: ChordKind| {
        menu_item(id, Some(Icon::Piano), title, Some("in scale"), false).on_click(cx.listener(move |this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.build_chord(c, kind);
                this.toast(if n == 0 { "Select or add notes first".into() } else { format!("Added {n} chord notes") }, cx);
            }
            cx.notify();
        }))
    };
    widgets::menu_card("tools-menu", 244.)
        .child(menu_header(format!("SCALE TOOLS · {scale_label}")))
        .child(menu_item("t-scale-fold", Some(Icon::ScaleSnap), "Fold piano roll to key", None, ws.s.piano_scale_fold).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano_scale_fold = !this.s.piano_scale_fold;
            this.menu = None;
            cx.notify();
        })))
        .child(menu_item("t-quantize", Some(Icon::Quantize), "Quantize", Some(if cfg!(target_os = "macos") { "⌘U" } else { "Ctrl+U" }), false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.quantize_clip(cx);
        })))
        .child(menu_item("t-scale", Some(Icon::ScaleSnap), "Snap notes to scale", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.scale_snap_clip(cx);
        })))
        .child(widgets::divider_h())
        .child(menu_header("CHORDS (IN KEY)"))
        .child(chord_btn("t-chord-triad", "Build Triad (1-3-5)", ChordKind::Triad))
        .child(chord_btn("t-chord-7th", "Build 7th (1-3-5-7)", ChordKind::Seventh))
        .child(chord_btn("t-chord-9th", "Build 9th (1-3-5-7-9)", ChordKind::Ninth))
        .child(chord_btn("t-chord-sus2", "Build Sus2", ChordKind::Sus2))
        .child(chord_btn("t-chord-sus4", "Build Sus4", ChordKind::Sus4))
        .child(chord_btn("t-chord-power", "Build Power 5th", ChordKind::Power))
        .child(menu_item("t-chord-inv-up", Some(Icon::ChevronUp), "Invert chord up (+8ve)", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.invert_chords(c, 1);
                this.toast(if n == 0 { "Select a chord to invert".into() } else { format!("Inverted {n} chord(s) up") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-chord-inv-dn", Some(Icon::ChevronDown), "Invert chord down (−8ve)", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.invert_chords(c, -1);
                this.toast(if n == 0 { "Select a chord to invert".into() } else { format!("Inverted {n} chord(s) down") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-chord-prog", Some(Icon::Sparkles), "4-Bar Progression (in key)", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.generate_scale_progression(c);
                if n > 0 {
                    this.toast("Generated 4-bar chord progression", cx);
                }
            }
            cx.notify();
        })))
        .child(widgets::divider_h())
        .child(menu_header("ARPEGGIO & TRANSFORM"))
        .child(menu_item("t-arp-up", Some(Icon::Arpeggio), "Arpeggiate Up", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.arpeggiate(c, ArpPattern::Up);
                this.toast(if n == 0 { "Add notes or chords first".into() } else { format!("Arpeggiated into {n} notes") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-arp-dn", Some(Icon::Arpeggio), "Arpeggiate Down", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.arpeggiate(c, ArpPattern::Down);
                this.toast(if n == 0 { "Add notes or chords first".into() } else { format!("Arpeggiated into {n} notes") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-arp-ud", Some(Icon::Arpeggio), "Arpeggiate Up-Down", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.arpeggiate(c, ArpPattern::UpDown);
                this.toast(if n == 0 { "Add notes or chords first".into() } else { format!("Arpeggiated into {n} notes") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-strum", Some(Icon::Strum), "Strum chords", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.strum(c, false);
                this.toast(if n == 0 { "Select or add chords to strum".into() } else { format!("Strummed {n} notes") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-chop", Some(Icon::Scissors), "Chop to grid", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                let n = this.s.chop(c);
                this.toast(if n == 0 { "Select longer notes to chop".into() } else { format!("Chopped into {n} notes") }, cx);
            }
            cx.notify();
        })))
        .child(menu_item("t-rand", Some(Icon::Dice), "Randomize (in key + vel)", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            if let Some(c) = this.s.selected_clip {
                this.view_switch = this.view_switch.wrapping_add(1);
                let n = this.s.randomize_notes(c, this.view_switch);
                this.toast(if n == 0 { "No notes to randomize".into() } else { format!("Varied {n} notes in key") }, cx);
            }
            cx.notify();
        })))
        .into_any_element()
}

fn category_icon(c: DeviceCategory) -> Icon {
    match c {
        DeviceCategory::Instrument => Icon::Piano,
        DeviceCategory::Drums => Icon::Drum,
        DeviceCategory::Dynamics => Icon::Gauge,
        DeviceCategory::Eq => Icon::Sliders,
        DeviceCategory::Filter => Icon::Waveform,
        DeviceCategory::Space => Icon::Layers,
        DeviceCategory::Distortion => Icon::Wand,
        DeviceCategory::Modulation => Icon::Refresh,
        DeviceCategory::Utility => Icon::Device,
        DeviceCategory::Rack => Icon::Grid,
    }
}

pub fn device_icon(plugin_id: &str, ws: &Workspace) -> Icon {
    ws.b.plugins.device(plugin_id).map_or(Icon::Plug, |d| category_icon(d.category))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceMenuGroup {
    Favorites,
    Effect(DeviceGroup),
    LocalSynths,
    VstSynths,
}

impl DeviceMenuGroup {
    fn label(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites",
            Self::Effect(g) => g.label(),
            Self::LocalSynths => "Local Synths",
            Self::VstSynths => "VST / CLAP Synths",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Favorites => Icon::StarFilled,
            Self::Effect(g) => group_icon(g),
            Self::LocalSynths => Icon::Piano,
            Self::VstSynths => Icon::Plug,
        }
    }
}

#[derive(Clone, Debug)]
enum DeviceMenuEntry {
    Stock { id: &'static str, name: &'static str, group: DeviceGroup },
    External { plugin: ExternalPlugin, group: DeviceGroup },
}

impl DeviceMenuEntry {
    fn name(&self) -> &str {
        match self {
            Self::Stock { name, .. } => name,
            Self::External { plugin, .. } => &plugin.name,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Stock { id, .. } => stock_key(id),
            Self::External { plugin, .. } => external_key(plugin.format, &plugin.uid),
        }
    }

    fn group(&self) -> DeviceGroup {
        match self {
            Self::Stock { group, .. } | Self::External { group, .. } => *group,
        }
    }

    fn is_local(&self) -> bool {
        matches!(self, Self::Stock { .. })
    }
}

fn group_icon(g: DeviceGroup) -> Icon {
    match g {
        DeviceGroup::InstAnalog
        | DeviceGroup::InstWavetable
        | DeviceGroup::InstFm
        | DeviceGroup::InstSampler
        | DeviceGroup::InstMulti => Icon::Piano,
        DeviceGroup::InstDrums => Icon::Drum,
        DeviceGroup::FxEq => Icon::Sliders,
        DeviceGroup::FxDynamics | DeviceGroup::FxLimiter => Icon::Gauge,
        DeviceGroup::FxSaturation => Icon::Wand,
        DeviceGroup::FxFilter => Icon::Waveform,
        DeviceGroup::FxReverb => Icon::Layers,
        DeviceGroup::FxDelay => Icon::Clock,
        DeviceGroup::FxModulation => Icon::Refresh,
        DeviceGroup::FxPitchTime => Icon::MusicNote,
        DeviceGroup::FxMastering => Icon::Mixer,
        DeviceGroup::FxCreative => Icon::Sparkles,
        DeviceGroup::FxStereoUtility => Icon::Device,
        DeviceGroup::FxAnalysis => Icon::Dashboard,
    }
}

fn collect_all_entries(
    stock: &[crate::bridge::DeviceInfo],
    external: &[ExternalPlugin],
    prefs: &crate::plugin_prefs::PluginPrefs,
) -> Vec<DeviceMenuEntry> {
    let mut out = Vec::with_capacity(stock.len() + external.len());
    for d in stock {
        out.push(DeviceMenuEntry::Stock {
            id: d.id,
            name: d.name,
            group: classify_stock(d.id, prefs),
        });
    }
    for p in external {
        let group = classify_external(p.format, &p.uid, &p.name, &p.vendor, &p.category, p.is_instrument, prefs);
        out.push(DeviceMenuEntry::External { plugin: p.clone(), group });
    }
    out
}

fn entries_for_menu_group(
    all: &[DeviceMenuEntry],
    prefs: &crate::plugin_prefs::PluginPrefs,
    grp: DeviceMenuGroup,
) -> Vec<DeviceMenuEntry> {
    let favs = prefs.favorite_set();
    let mut items: Vec<DeviceMenuEntry> = match grp {
        DeviceMenuGroup::Favorites => all.iter().filter(|e| favs.contains(e.key().as_str())).cloned().collect(),
        DeviceMenuGroup::Effect(g) => all.iter().filter(|e| e.group() == g).cloned().collect(),
        DeviceMenuGroup::LocalSynths => all
            .iter()
            .filter(|e| e.is_local() && e.group().is_instrument())
            .cloned()
            .collect(),
        DeviceMenuGroup::VstSynths => all
            .iter()
            .filter(|e| !e.is_local() && e.group().is_instrument())
            .cloned()
            .collect(),
    };
    items.sort_by(|a, b| {
        b.is_local()
            .cmp(&a.is_local())
            .then_with(|| a.group().cmp(&b.group()))
            .then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase()))
    });
    items
}

fn build_main_menu_groups(
    all: &[DeviceMenuEntry],
    prefs: &crate::plugin_prefs::PluginPrefs,
) -> (Option<usize>, Vec<(DeviceMenuGroup, usize)>, usize, usize) {
    let favs = prefs.favorite_set();
    let fav_count = all.iter().filter(|e| favs.contains(e.key().as_str())).count();
    let fav_group = (fav_count > 0).then_some(fav_count);

    let fx_groups: Vec<(DeviceMenuGroup, usize)> = DeviceGroup::EFFECTS
        .iter()
        .filter_map(|&g| {
            let n = all.iter().filter(|e| e.group() == g).count();
            (n > 0).then_some((DeviceMenuGroup::Effect(g), n))
        })
        .collect();

    let local_synth_count = all.iter().filter(|e| e.is_local() && e.group().is_instrument()).count();
    let vst_synth_count = all.iter().filter(|e| !e.is_local() && e.group().is_instrument()).count();
    (fav_group, fx_groups, local_synth_count, vst_synth_count)
}

fn search_entries(
    all: &[DeviceMenuEntry],
    stock: &[crate::bridge::DeviceInfo],
    prefs: &crate::plugin_prefs::PluginPrefs,
    query: &str,
) -> Vec<DeviceMenuEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let favs = prefs.favorite_set();
    let mut matches: Vec<DeviceMenuEntry> = all
        .iter()
        .filter(|e| {
            let name_hit = e.name().to_lowercase().contains(&q);
            let group_hit = e.group().label().to_lowercase().contains(&q);
            let extra_hit = match e {
                DeviceMenuEntry::Stock { id, .. } => {
                    "local stock".contains(&q)
                        || stock
                            .iter()
                            .find(|d| d.id == *id)
                            .is_some_and(|d| d.tags.iter().any(|t| t.to_lowercase().contains(&q)))
                }
                DeviceMenuEntry::External { plugin, .. } => {
                    let fmt = match plugin.format {
                        PluginFormat::Vst3 => "vst3 vst",
                        PluginFormat::Clap => "clap",
                    };
                    fmt.contains(&q)
                        || plugin.vendor.to_lowercase().contains(&q)
                        || plugin.category.to_lowercase().contains(&q)
                }
            };
            name_hit || group_hit || extra_hit
        })
        .cloned()
        .collect();

    matches.sort_by(|a, b| {
        let a_fav = favs.contains(a.key().as_str());
        let b_fav = favs.contains(b.key().as_str());
        b_fav
            .cmp(&a_fav)
            .then_with(|| b.is_local().cmp(&a.is_local()))
            .then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase()))
    });
    matches
}

fn render_device_entry_row(
    id_prefix: &'static str,
    idx: usize,
    entry: DeviceMenuEntry,
    track: aitrack_model::TrackId,
    fav: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let key_fav = entry.key();
    let group = entry.group();
    let star_btn = div()
        .id((id_prefix, idx * 2 + 1))
        .size(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.1)))
        .tooltip(widgets::tip(if fav { "Remove favorite" } else { "Add to favorites" }, None))
        .child(crate::icons::icon(if fav { Icon::StarFilled } else { Icon::Star }, 12., if fav { pal.solo } else { pal.faint }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            let became_fav = this.palette.plugin_prefs.toggle_favorite(&key_fav);
            if !became_fav
                && this.device_menu_hover == Some(DeviceMenuGroup::Favorites)
                && this.palette.plugin_prefs.favorites.is_empty()
            {
                this.device_menu_hover = None;
            }
            cx.notify();
        }));

    match entry {
        DeviceMenuEntry::Stock { id, name, .. } => {
            let ic = if id.starts_with("rack.") { Icon::Grid } else { group_icon(group) };
            div()
                .id((id_prefix, idx * 2))
                .h(px(ROW_H))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(crate::icons::icon(ic, 14., pal.muted)))
                .child(label(name, TEXT_MD, pal.text).flex_1().min_w(px(0.)).truncate())
                .child(widgets::badge("Local", pal.faint))
                .child(star_btn)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.device_menu_hover = None;
                    if this.s.add_stock_device(track, id, name).is_some() {
                        this.palette.plugin_prefs.note_recent(&stock_key(id));
                        this.toast(format!("Added {name}"), cx);
                    }
                }))
                .into_any_element()
        }
        DeviceMenuEntry::External { plugin, .. } => {
            let fmt = match plugin.format {
                PluginFormat::Vst3 => "VST3",
                PluginFormat::Clap => "CLAP",
            };
            let ic = if group.is_instrument() { Icon::Plug } else { group_icon(group) };
            let tip_text = if plugin.vendor.is_empty() {
                format!("{fmt} · {}", plugin.name)
            } else {
                format!("{} · {fmt}", plugin.vendor)
            };
            let p_name = plugin.name.clone();
            div()
                .id((id_prefix, idx * 2))
                .h(px(ROW_H))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip_owned(tip_text))
                .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(crate::icons::icon(ic, 14., pal.muted)))
                .child(label(p_name, TEXT_MD, pal.text).flex_1().min_w(px(0.)).truncate())
                .child(widgets::badge(fmt, pal.accent))
                .child(star_btn)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.device_menu_hover = None;
                    let name = plugin.name.clone();
                    let key = external_key(plugin.format, &plugin.uid);
                    if this.s.add_plugin_device(track, &plugin).is_some() {
                        this.palette.plugin_prefs.note_recent(&key);
                        this.toast(format!("Added {name}"), cx);
                    }
                }))
                .into_any_element()
        }
    }
}

fn device_submenu_card(
    grp: DeviceMenuGroup,
    items: Vec<DeviceMenuEntry>,
    track: aitrack_model::TrackId,
    prefs: &crate::plugin_prefs::PluginPrefs,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let mut list = div()
        .id("dev-submenu-scroll")
        .max_h(px(380.))
        .overflow_y_scroll()
        .flex()
        .flex_col();

    if items.is_empty() {
        list = list
            .child(div().px(px(SP2)).py(px(SP2)).child(label("No installed VST3 / CLAP synths found.", TEXT_SM, pal.faint)))
            .child(
                menu_item("dev-rescan", Some(Icon::Refresh), "Rescan VST3 / CLAP plugins", None, false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.b.plugins.rescan();
                        this.toast("Scanning plugin folders…", cx);
                        cx.notify();
                    },
                )),
            )
            .child(
                menu_item("dev-folders", Some(Icon::FolderOpen), "Plugin folders…", None, false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.menu = None;
                        this.device_menu_hover = None;
                        this.open_modal(crate::views::prefs::Modal::Preferences(crate::views::prefs::PrefsPage::Audio), cx);
                    },
                )),
            );
    } else {
        match grp {
            DeviceMenuGroup::Effect(_) => {
                let (local, ext): (Vec<_>, Vec<_>) = items.into_iter().partition(DeviceMenuEntry::is_local);
                let both = !local.is_empty() && !ext.is_empty();
                let mut idx = 0usize;
                if !local.is_empty() {
                    if both {
                        list = list.child(menu_header("LOCAL"));
                    }
                    for entry in local {
                        let fav = prefs.is_favorite(&entry.key());
                        list = list.child(render_device_entry_row("dev-sub", idx, entry, track, fav, cx));
                        idx += 1;
                    }
                }
                if !ext.is_empty() {
                    if both {
                        list = list.child(div().pt(px(SP1)).child(widgets::divider_h())).child(menu_header("INSTALLED VST / CLAP"));
                    }
                    for entry in ext {
                        let fav = prefs.is_favorite(&entry.key());
                        list = list.child(render_device_entry_row("dev-sub", idx, entry, track, fav, cx));
                        idx += 1;
                    }
                }
            }
            DeviceMenuGroup::LocalSynths | DeviceMenuGroup::VstSynths => {
                let mut idx = 0usize;
                let mut sec = 0usize;
                for &inst_grp in &DeviceGroup::INSTRUMENTS {
                    let group_items: Vec<_> = items.iter().filter(|e| e.group() == inst_grp).cloned().collect();
                    if group_items.is_empty() {
                        continue;
                    }
                    if sec > 0 {
                        list = list.child(div().pt(px(SP1)).child(widgets::divider_h()));
                    }
                    sec += 1;
                    list = list.child(menu_header(inst_grp.label().to_uppercase()));
                    for entry in group_items {
                        let fav = prefs.is_favorite(&entry.key());
                        list = list.child(render_device_entry_row("dev-sub", idx, entry, track, fav, cx));
                        idx += 1;
                    }
                }
            }
            DeviceMenuGroup::Favorites => {
                for (idx, entry) in items.into_iter().enumerate() {
                    let fav = prefs.is_favorite(&entry.key());
                    list = list.child(render_device_entry_row("dev-sub", idx, entry, track, fav, cx));
                }
            }
        }
    }

    widgets::menu_card("dev-submenu-card", 276.)
        .child(menu_header(grp.label().to_uppercase()))
        .child(list)
        .into_any_element()
}

fn device_menu(
    ws: &mut Workspace,
    track: aitrack_model::TrackId,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let pal = p();
    let vp = window.viewport_size();
    let vw = f32::from(vp.width);
    let vh = f32::from(vp.height);
    let menu_at = ws.menu.as_ref().map(|m| m.at).unwrap_or_default();

    let main_w = 248.0_f32;
    let sub_w = 276.0_f32;
    let card_x = f32::from(menu_at.x).clamp(SP2, (vw - main_w - SP2).max(SP2));
    let space_right = vw - (card_x + main_w) - SP2;
    let space_left = card_x - SP2;
    let open_left = space_right < sub_w + 4. && space_left >= space_right;

    let stock = ws.b.plugins.stock_devices();
    let external = ws.b.plugins.external_plugins();
    let prefs = &ws.palette.plugin_prefs;
    let all = collect_all_entries(stock, external, prefs);

    let query = ws.device_menu_search.read(cx).text().trim().to_owned();
    let clear_hover = |el: gpui::Stateful<gpui::Div>, cx: &mut Context<Workspace>| {
        el.on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if *hovered && this.device_menu_hover.is_some() {
                this.device_menu_hover = None;
                cx.notify();
            }
        }))
    };

    let mut card = widgets::menu_card("device-menu", main_w)
        .child(menu_header("ADD DEVICE"))
        .child(
            clear_hover(
                div().id("dev-search-wrap").px(px(SP1)).pb(px(SP1)).child(ws.device_menu_search.clone()),
                cx,
            ),
        );

    if !query.is_empty() {
        let matches = search_entries(&all, stock, prefs, &query);
        let mut list = div()
            .id("device-menu-search-scroll")
            .max_h(px((vh - 120.).clamp(200., 400.)))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if matches.is_empty() {
            list = list.child(div().px(px(SP2)).py(px(SP2)).child(label("No matching devices", TEXT_SM, pal.faint)));
        } else {
            for (idx, entry) in matches.into_iter().enumerate() {
                let fav = prefs.is_favorite(&entry.key());
                list = list.child(render_device_entry_row("dev-q", idx, entry, track, fav, cx));
            }
        }
        return card.child(list).into_any_element();
    }

    let (fav_group, fx_groups, local_synth_count, vst_synth_count) = build_main_menu_groups(&all, prefs);
    let active_hover = ws.device_menu_hover;

    let mut row_y = 24. + CONTROL_H + SP1 + SP1;
    let mut hovered_y: Option<f32> = None;
    let mut group_idx = 0usize;

    let mut list = div()
        .id("device-menu-groups")
        .max_h(px((vh - 100.).clamp(220., 500.)))
        .overflow_y_scroll()
        .flex()
        .flex_col();

    let mut make_group_row = |grp: DeviceMenuGroup, count: usize, y: f32| {
        let active = active_hover == Some(grp);
        if active {
            hovered_y = Some(y);
        }
        let id_idx = group_idx;
        group_idx += 1;
        let chevron = if open_left { Icon::ChevronLeft } else { Icon::ChevronRight };
        let icon_color = if grp == DeviceMenuGroup::Favorites {
            pal.solo
        } else if active {
            pal.accent
        } else {
            pal.muted
        };
        div()
            .id(("dev-grp-row", id_idx))
            .h(px(ROW_H))
            .px(px(SP2))
            .gap(px(SP2))
            .flex()
            .items_center()
            .rounded(px(r_md()))
            .cursor_pointer()
            .when(active, |d| d.bg(hover()))
            .hover(|s| s.bg(hover()))
            .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(crate::icons::icon(grp.icon(), 14., icon_color)))
            .child(label(grp.label(), TEXT_MD, pal.text).flex_1().min_w(px(0.)).truncate())
            .child(mono(count.to_string(), TEXT_XS, pal.faint))
            .child(crate::icons::icon(chevron, 14., if active { pal.accent } else { pal.faint }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.device_menu_hover != Some(grp) {
                    this.device_menu_hover = Some(grp);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.device_menu_hover = if this.device_menu_hover == Some(grp) { None } else { Some(grp) };
                cx.notify();
            }))
    };

    if let Some(fav_count) = fav_group {
        list = list.child(make_group_row(DeviceMenuGroup::Favorites, fav_count, row_y));
        row_y += ROW_H;
        list = list.child(div().py(px(2.)).child(widgets::divider_h()));
        row_y += 5.;
    }

    if !fx_groups.is_empty() {
        list = list.child(menu_header("EFFECTS"));
        row_y += 24.;
        for (grp, count) in &fx_groups {
            list = list.child(make_group_row(*grp, *count, row_y));
            row_y += ROW_H;
        }
    }

    list = list.child(div().py(px(2.)).child(widgets::divider_h()));
    row_y += 5.;
    list = list.child(menu_header("SYNTHS & INSTRUMENTS"));
    row_y += 24.;
    if local_synth_count > 0 {
        list = list.child(make_group_row(DeviceMenuGroup::LocalSynths, local_synth_count, row_y));
        row_y += ROW_H;
    }
    list = list.child(make_group_row(DeviceMenuGroup::VstSynths, vst_synth_count, row_y));
    row_y += ROW_H;

    card = card.child(list);

    let submenu = active_hover.map(|grp| {
        let items = entries_for_menu_group(&all, prefs, grp);
        let sub_rows = items.len().max(3);
        let est_card_h = (row_y + SP1).min((vh - 2. * SP2).max(240.));
        let card_y = f32::from(menu_at.y).clamp(SP2, (vh - est_card_h - SP2).max(SP2));
        let est_sub_h = (24. + sub_rows as f32 * ROW_H + 32.).min(420.);
        let desired_top = hovered_y.unwrap_or(24.) - SP1;
        let min_top = SP2 - card_y;
        let max_top = (vh - SP2 - card_y - est_sub_h).max(min_top);
        let sub_top = desired_top.clamp(min_top, max_top);

        let sub = div()
            .id("dev-submenu")
            .occlude()
            .absolute()
            .top(px(sub_top))
            .child(device_submenu_card(grp, items, track, prefs, cx));
        if open_left { sub.right_full().pr(px(4.)) } else { sub.left_full().pl(px(4.)) }
    });

    div().relative().child(card).children(submenu).into_any_element()
}

/// The device's factory presets by category; picking one sets every
/// parameter (one undo step).
fn preset_menu(ws: &mut Workspace, device: aitrack_model::DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    preset_menu_card(ws, device, false, cx)
}

/// Shared preset picker menu used by both Device View cards and floating Device Windows.
pub fn preset_menu_card(ws: &mut Workspace, device: aitrack_model::DeviceId, from_pop: bool, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let d = ws.s.project.device(device)?;
    let aitrack_model::DeviceKind::Stock { plugin_id } = &d.kind else { return None };
    let current = d.preset.clone();
    let presets = ws.b.plugins.presets(plugin_id);
    if presets.is_empty() {
        return None;
    }
    let mut card = widgets::menu_card("preset-menu", 232.);
    let mut last: Option<String> = None;
    for (i, pr) in presets.into_iter().enumerate() {
        if last.as_deref() != Some(pr.category.as_str()) {
            card = card.child(menu_header(pr.category.to_uppercase()));
            last = Some(pr.category.clone());
        }
        let on = current.as_deref() == Some(pr.name.as_str());
        let path = pr.path.clone();
        card = card.child(menu_item(("preset", i), None, pr.name.clone(), None, on).on_click(cx.listener(move |this, _, _, cx| {
            if from_pop {
                this.fx.pop = None;
            } else {
                this.menu = None;
            }
            match this.b.plugins.load_preset(&path) {
                Ok((name, params)) => {
                    if this.s.load_preset(device, &name, &params) {
                        this.toast(format!("Loaded {name}"), cx);
                    }
                }
                Err(e) => this.toast(e, cx),
            }
            cx.notify();
        })));
    }
    Some(div().id("preset-menu-scroll").max_h(px(480.)).overflow_y_scroll().child(card).into_any_element())
}

fn library_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let scanning = ws.b.library.scan_status().running;
    let can_undo = ws.b.library.can_undo_organize();
    widgets::menu_card("library-menu", 232.)
        .child(menu_header("LIBRARY"))
        .child(menu_item("lib-add", Some(Icon::FolderOpen), "Add folder…", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.pick_library_folder(cx);
        })))
        .child(if scanning {
            menu_item("lib-cancel", Some(Icon::X), "Cancel scan", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.menu = None;
                this.b.library.cancel_scan();
                cx.notify();
            }))
        } else {
            menu_item("lib-rescan", Some(Icon::Refresh), "Rescan my folders", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.menu = None;
                this.b.library.rescan();
                cx.notify();
            }))
        })
        .when(!scanning, |d| {
            d.child(menu_item("lib-keys", Some(Icon::Wand), "Analyse keys of factory sounds", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.menu = None;
                match this.b.library.detect_keys() {
                    Ok(()) => this.toast("Listening for keys in the background…", cx),
                    Err(e) => this.toast(e, cx),
                }
                cx.notify();
            })))
        })
        .child(widgets::divider_h())
        .child(menu_item("lib-organize", Some(Icon::FolderTree), "Organize on disk…", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.organize = crate::workspace::OrganizeUi { items: this.library_view().iter().map(|i| i.id).collect(), ..Default::default() };
            this.open_modal(crate::views::prefs::Modal::Organize, cx);
        })))
        .when(can_undo, |d| {
            d.child(menu_item("lib-undo-org", Some(Icon::Undo), "Undo last organize", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.menu = None;
                let msg = match this.b.library.undo_organize() {
                    Ok(m) | Err(m) => m,
                };
                this.toast(msg, cx);
            })))
        })
        .child(widgets::divider_h())
        .child(menu_item("lib-hub", Some(Icon::Globe), "Content Hub…", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.open_modal(crate::views::prefs::Modal::ContentHub, cx);
        })))
        .into_any_element()
}

fn palette_device_menu(
    ws: &mut Workspace,
    key: &str,
    name: &str,
    instrument: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    use crate::views::palette::devices::DeviceGroup;
    let prefs = &ws.palette.plugin_prefs;
    let fav = prefs.is_favorite(key);
    let current_override = prefs.category_overrides.get(key).and_then(|k| DeviceGroup::from_key(k));
    let key_fav = key.to_owned();
    let key_auto = key.to_owned();
    let mut card = widgets::menu_card("palette-dev-menu", 240.)
        .child(menu_header(name.to_uppercase()))
        .child(
            menu_item("pdev-fav", Some(if fav { Icon::StarFilled } else { Icon::Star }), if fav { "Remove favorite" } else { "Add to favorites" }, None, fav)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.palette.plugin_prefs.toggle_favorite(&key_fav);
                    crate::views::palette::rebuild(this);
                    cx.notify();
                })),
        )
        .child(widgets::divider_h())
        .child(menu_header("MOVE TO GROUP"))
        .when(current_override.is_some(), |c| {
            c.child(
                menu_item("pdev-auto", Some(Icon::Refresh), "Reset to automatic", None, false).on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.palette.plugin_prefs.set_category_override(&key_auto, None);
                    crate::views::palette::rebuild(this);
                    cx.notify();
                })),
            )
        });
    let list = if instrument { &DeviceGroup::INSTRUMENTS[..] } else { &DeviceGroup::EFFECTS[..] };
    for (i, group) in list.iter().enumerate() {
        let k = key.to_owned();
        let gkey = group.key();
        let on = current_override == Some(*group);
        card = card.child(menu_item(("pdev-grp", i), None, group.label(), None, on).on_click(cx.listener(move |this, _, _, cx| {
            this.menu = None;
            this.palette.plugin_prefs.set_category_override(&k, Some(gkey));
            crate::views::palette::rebuild(this);
            cx.notify();
        })));
    }
    div().id("palette-dev-scroll").max_h(px(440.)).overflow_y_scroll().child(card).into_any_element()
}

fn sig_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let current = ws.s.project.tempo.signature_at(0);
    let mut card = widgets::menu_card("sig-menu", 140.).child(menu_header("TIME SIGNATURE"));
    for (i, (n, d)) in [(2u8, 4u8), (3, 4), (4, 4), (5, 4), (6, 8), (7, 8)].into_iter().enumerate() {
        let sig = TimeSignature { numerator: n, denominator: d };
        card = card.child(menu_item(("sig", i), None, format!("{n}/{d}"), None, sig == current).on_click(cx.listener(
            move |this, _, _, cx| {
                this.menu = None;
                this.s.set_time_signature(sig);
                cx.notify();
            },
        )));
    }
    card.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{PluginCatalog, StubCatalog};
    use crate::plugin_prefs::PluginPrefs;

    #[test]
    fn device_menu_groups_favorites_first_effects_and_synths() {
        let cat = StubCatalog::default();
        let mut prefs = PluginPrefs::default();
        let all = collect_all_entries(cat.stock_devices(), cat.external_plugins(), &prefs);

        // No favorites initially -> fav_group is None.
        let (fav_none, fx_groups, local_synths, vst_synths) = build_main_menu_groups(&all, &prefs);
        assert_eq!(fav_none, None);
        assert!(fx_groups.iter().any(|(g, _)| *g == DeviceMenuGroup::Effect(DeviceGroup::FxEq)));
        assert!(fx_groups.iter().any(|(g, _)| *g == DeviceMenuGroup::Effect(DeviceGroup::FxLimiter)));
        assert!(fx_groups.iter().any(|(g, _)| *g == DeviceMenuGroup::Effect(DeviceGroup::FxSaturation)));
        assert!(local_synths > 0);
        assert!(vst_synths > 0);

        // Adding a favorite surfaces Favorites as the first group.
        prefs.favorites.push(stock_key("fx.eq"));
        let (fav_some, _, _, _) = build_main_menu_groups(&all, &prefs);
        assert_eq!(fav_some, Some(1));
        let fav_items = entries_for_menu_group(&all, &prefs, DeviceMenuGroup::Favorites);
        assert_eq!(fav_items.len(), 1);
        assert_eq!(fav_items[0].name(), "EQ");

        // Local Synths and VST Synths remain separated.
        let locals = entries_for_menu_group(&all, &prefs, DeviceMenuGroup::LocalSynths);
        assert!(locals.iter().all(DeviceMenuEntry::is_local));
        assert!(locals.iter().any(|e| e.name() == "Acid 303"));

        let vsts = entries_for_menu_group(&all, &prefs, DeviceMenuGroup::VstSynths);
        assert!(vsts.iter().all(|e| !e.is_local()));
        assert!(vsts.iter().any(|e| e.name() == "Vital"));

        // Search finds devices across effects, local synths, and installed VSTs.
        let q_lim = search_entries(&all, cat.stock_devices(), &prefs, "limiter");
        assert!(q_lim.iter().any(|e| e.name() == "Limiter"));
        let q_vital = search_entries(&all, cat.stock_devices(), &prefs, "vital");
        assert!(q_vital.iter().any(|e| e.name() == "Vital"));
    }
}
