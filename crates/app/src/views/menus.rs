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
        MenuKind::TimeSignature => sig_menu(ws, cx),
    })
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
        card = card.child(
            menu_item(("snap", i), Some(Icon::Magnet), s.label(), None, ws.s.snap == s).on_click(cx.listener(move |this, _, _, cx| {
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
        .child(menu_item("t-quantize", Some(Icon::Quantize), "Quantize", Some("Q"), false).on_click(cx.listener(|this, _, _, cx| {
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
