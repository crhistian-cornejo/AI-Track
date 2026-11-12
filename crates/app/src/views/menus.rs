//! Popover menus: key/scale picker, snap grid, piano roll tools, device
//! picker and time signature.

use crate::bridge::DeviceCategory;
use crate::icons::Icon;
use crate::state::Snap;
use crate::theme::*;
use crate::widgets::{self, label, menu_header, menu_item};
use crate::workspace::{Menu, MenuKind, Workspace};
use aitrack_model::{Key, Scale, ScaleKind, TimeSignature};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder, px,
};

pub fn render(ws: &mut Workspace, menu: &Menu, _: &mut Window, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    Some(match &menu.kind {
        MenuKind::Scale => scale_menu(ws, cx),
        MenuKind::Snap => snap_menu(ws, cx),
        MenuKind::PianoTools => tools_menu(cx),
        MenuKind::AddDevice(track) => device_menu(ws, *track, cx),
        MenuKind::Presets(device) => preset_menu(ws, *device, cx)?,
        MenuKind::LibraryContext => library_menu(ws, cx),
        MenuKind::ClipContext => crate::editing::menus::clip_menu(ws, cx),
        MenuKind::LaneContext => crate::editing::menus::lane_menu(cx),
        MenuKind::TimeSignature => sig_menu(ws, cx),
        MenuKind::File => file_menu(ws, cx),
    })
}

/// File: new from a template, open, save, export.
fn file_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    use crate::project_io::Template;
    let mac = cfg!(target_os = "macos");
    let key = |win: &'static str, mac_key: &'static str| Some(if mac { mac_key } else { win });
    let file = ws.project_path.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).map(str::to_owned);
    let mut card = widgets::menu_card("file-menu", 320.).child(menu_header("NEW"));
    for (i, t) in Template::ALL.into_iter().enumerate() {
        card = card.child(
            div()
                .child(menu_item(("file-new", i), Some(if t == Template::Empty { Icon::File } else { Icon::Layers }), t.label(), if t == Template::Empty { key("Ctrl+N", "⌘N") } else { None }, false).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.menu = None;
                        this.file_new(t, window, cx);
                    },
                )))
                .child(div().pl(px(SP6 + SP2)).pr(px(SP2)).pb(px(SP1)).child(label(t.hint(), TEXT_XS, p().faint))),
        );
    }
    card.child(menu_header(file.map_or("PROJECT".to_owned(), |f| f.to_uppercase())))
        .child(menu_item("file-open", Some(Icon::FolderOpen), "Open…", key("Ctrl+O", "⌘O"), false).on_click(cx.listener(|this, _, window, cx| {
            this.menu = None;
            this.file_open(window, cx);
        })))
        .child(menu_item("file-save", Some(Icon::Check), "Save", key("Ctrl+S", "⌘S"), false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.file_save(cx);
        })))
        .child(menu_item("file-save-as", Some(Icon::File), "Save as…", key("Ctrl+Shift+S", "⇧⌘S"), false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.file_save_as(None, cx);
        })))
        .child(menu_item("file-shortcuts", Some(Icon::Keyboard), "Keyboard shortcuts…", key("Ctrl+/", "⌘/"), false).on_click(cx.listener(|this, _, window, cx| {
            this.menu = None;
            crate::commands::run(this, crate::keys::Cmd::Shortcuts, window, cx);
        })))
        .child(menu_item("file-export", Some(Icon::Send), "Export audio…", key("Ctrl+Shift+E", "⇧⌘E"), false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.open_export(cx);
        })))
        .into_any_element()
}

fn scale_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let current = ws.s.project.scale;
    let root = current.map_or(Key(9), |s| s.root);
    let kind = current.map_or(ScaleKind::Minor, |s| s.kind);
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
                    this.s.set_scale(Some(Scale::new(Key(k), kind)));
                    cx.notify();
                })),
        );
    }
    let mut list = div().flex().flex_col();
    for (i, sk) in ScaleKind::ALL.iter().copied().enumerate() {
        let on = current.is_some_and(|s| s.kind == sk);
        list = list.child(
            menu_item(("scale-kind", i), None, sk.label(), None, on).on_click(cx.listener(move |this, _, _, cx| {
                this.s.set_scale(Some(Scale::new(root, sk)));
                this.menu = None;
                cx.notify();
            })),
        );
    }
    widgets::menu_card("scale-menu", 248.)
        .child(menu_header("ROOT"))
        .child(grid)
        .child(widgets::divider_h())
        .child(menu_header("SCALE"))
        .child(list)
        .child(widgets::divider_h())
        .child(menu_item("no-key", Some(Icon::X), "No key", None, current.is_none()).on_click(cx.listener(|this, _, _, cx| {
            this.s.set_scale(None);
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

fn tools_menu(cx: &mut Context<Workspace>) -> AnyElement {
    let stub = |name: &'static str| {
        move |this: &mut Workspace, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Workspace>| {
            this.menu = None;
            this.toast(format!("{name} arrives with the MIDI tools pass"), cx);
        }
    };
    widgets::menu_card("tools-menu", 220.)
        .child(menu_header("NOTE TOOLS · selection or whole clip"))
        .child(menu_item("t-quantize", Some(Icon::Quantize), "Quantize", Some(if cfg!(target_os = "macos") { "⌘U" } else { "Ctrl+U" }), false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.quantize_clip(cx);
        })))
        .child(menu_item("t-scale", Some(Icon::ScaleSnap), "Snap to scale", None, false).on_click(cx.listener(|this, _, _, cx| {
            this.menu = None;
            this.scale_snap_clip(cx);
        })))
        .child(widgets::divider_h())
        .child(menu_item("t-chop", Some(Icon::Scissors), "Chop", None, false).on_click(cx.listener(stub("Chop"))))
        .child(menu_item("t-arp", Some(Icon::Arpeggio), "Arpeggiate", None, false).on_click(cx.listener(stub("Arpeggiate"))))
        .child(menu_item("t-strum", Some(Icon::Strum), "Strum", None, false).on_click(cx.listener(stub("Strum"))))
        .child(menu_item("t-rand", Some(Icon::Dice), "Randomize", None, false).on_click(cx.listener(stub("Randomize"))))
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

fn device_menu(ws: &mut Workspace, track: aitrack_model::TrackId, cx: &mut Context<Workspace>) -> AnyElement {
    let mut devices: Vec<(DeviceCategory, &'static str, &'static str)> =
        ws.b.plugins.stock_devices().iter().map(|d| (d.category, d.id, d.name)).collect();
    devices.sort();
    let mut card = widgets::menu_card("device-menu", 232.).child(menu_header("ADD STOCK DEVICE"));
    let mut last = None;
    for (i, (cat, id, name)) in devices.into_iter().enumerate() {
        if last != Some(cat) {
            if last.is_some() {
                card = card.child(div().h(px(SP1)));
            }
            last = Some(cat);
        }
        card = card.child(menu_item(("dev", i), Some(category_icon(cat)), name, Some(cat.label()), false).on_click(cx.listener(
            move |this, _, _, cx| {
                this.menu = None;
                if this.s.add_stock_device(track, id, name).is_some() {
                    this.toast(format!("Added {name}"), cx);
                }
            },
        )));
    }
    div().id("device-menu-scroll").max_h(px(520.)).overflow_y_scroll().child(card).into_any_element()
}

/// The device's factory presets by category; picking one sets every
/// parameter (one undo step).
fn preset_menu(ws: &mut Workspace, device: aitrack_model::DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let d = ws.s.project.device(device)?;
    let aitrack_model::DeviceKind::Stock { plugin_id } = &d.kind else { return None };
    let current = d.preset.clone();
    let presets = ws.b.plugins.presets(plugin_id);
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
            this.menu = None;
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
