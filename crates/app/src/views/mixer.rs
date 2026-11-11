//! Mixer (Logic-style channel strips): input, inserts, sends, output, pan,
//! fader with meter, mute/solo/arm and the name plate, for tracks, groups,
//! returns and VCAs, with Main on the right. Every section has a fixed
//! height so the strips line up row by row.

use crate::icons::{Icon, icon};
use crate::state::{db_to_fader, format_db};
use crate::theme::*;
use crate::views::devices::knob_control;
use crate::widgets::{self, chip, label, mono};
use crate::workspace::{Drag, KnobTarget, Workspace};
use aitrack_model::*;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

const SLOT_H: f32 = 20.;
const INSERTS: usize = 3;

fn slot(text: impl Into<gpui::SharedString>, active: bool, i: Option<Icon>) -> Div {
    let pal = p();
    div()
        .h(px(SLOT_H))
        .px(px(SP1 + 2.))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(if active { content(0.07) } else { content(0.03) })
        .border_1()
        .border_color(border())
        .when_some(i, |d, i| d.child(icon(i, 10., pal.faint)))
        .child(label(text, TEXT_XS, if active { pal.text } else { pal.faint }))
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let order = |k: TrackKind| match k {
        TrackKind::Instrument | TrackKind::Audio | TrackKind::Group | TrackKind::Folder => 0,
        TrackKind::Return => 1,
        TrackKind::Vca => 2,
        TrackKind::Main => 3,
    };
    let mut tracks: Vec<(usize, TrackId)> = ws.s.project.tracks.iter().enumerate().map(|(i, t)| (order(t.kind) * 1000 + i, t.id)).collect();
    tracks.sort();
    let returns: Vec<(TrackId, String)> = ws.s.project.tracks.iter().filter(|t| t.kind == TrackKind::Return).map(|t| (t.id, t.name.clone())).collect();
    let fader_h = (ws.bottom_h - TAB_BAR_H - 316.).clamp(64., 360.);
    let mut row = div().h_full().flex().gap(px(SP1)).p(px(SP2));
    let mut last_group = 0;
    for (k, id) in tracks {
        let group = k / 1000;
        if group != last_group {
            row = row.child(div().w(px(SP1)).h_full().flex().justify_center().child(div().w(px(1.)).h_full().bg(border())));
            last_group = group;
        }
        row = row.child(strip(ws, id, &returns, fader_h, cx));
    }
    div().id("mixer-scroll").size_full().overflow_x_scroll().overflow_y_scroll().child(row).into_any_element()
}

fn strip(ws: &mut Workspace, id: TrackId, returns: &[(TrackId, String)], fader_h: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(t) = ws.s.project.track(id).cloned() else { return div().into_any_element() };
    let selected = ws.s.selected_track == Some(id);
    let color = track_color(t.color);
    let is_main = t.kind == TrackKind::Main;
    let is_vca = t.kind == TrackKind::Vca;
    let meter = ws.b.engine.meter(id);

    // Input.
    let input = match t.kind {
        TrackKind::Instrument => t.devices.iter().find(|d| d.is_instrument()).map_or("No instrument".into(), |d| d.name.clone()),
        TrackKind::Audio => "In 1/2".into(),
        TrackKind::Group => "Sum".into(),
        TrackKind::Return => "Sends".into(),
        TrackKind::Vca => "VCA".into(),
        TrackKind::Folder => "Folder".into(),
        TrackKind::Main => "Mix".into(),
    };
    let mut inserts = div().flex().flex_col().gap(px(2.));
    let fx: Vec<&Device> = t.devices.iter().filter(|d| !d.is_instrument()).collect();
    for i in 0..INSERTS {
        inserts = inserts.child(match fx.get(i) {
            Some(d) => slot(crate::paint::fit(&d.name, STRIP_W - 24., TEXT_XS), d.enabled, None),
            None if !is_vca => slot("", false, None),
            None => div().h(px(SLOT_H)),
        });
    }
    // Sends: one row per return, so every strip has the same rows.
    let mut sends = div().flex().flex_col().gap(px(2.));
    for (ri, (rid, rname)) in returns.iter().enumerate() {
        let send = t.sends.iter().position(|s| s.target == *rid);
        let has = send.is_some() && !is_main && !is_vca && t.kind != TrackKind::Return;
        let level = send.and_then(|i| t.sends.get(i)).map_or(-70., |s| s.level_db);
        let pos = db_to_fader(level);
        let letter = rname.chars().next().unwrap_or('?').to_string();
        sends = sends.child(
            div()
                .id(("send", (id.0 as usize) << 4 | ri))
                .h(px(SLOT_H))
                .px(px(SP1))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .bg(content(0.03))
                .when(has, |d| {
                    let index = send.unwrap_or(0);
                    d.cursor_ns_resize()
                        .tooltip(widgets::tip_owned(format!("Send to {rname}: {} dB — drag", format_db(level))))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.begin_drag(Drag::Knob { target: KnobTarget::Send { track: id, index }, last_y: f32::from(e.position.y), value: pos }, cx);
                        }))
                })
                .child(mono(letter, TEXT_XS, if has { pal.text } else { pal.faint }).w(px(10.)))
                .child(widgets::hbar(if has { pos } else { 0. }, STRIP_W - 28. - SP2, 4., accent_alpha(0.8))),
        );
    }
    let output = match t.output {
        Output::Main => "Main".to_owned(),
        Output::Track(g) => ws.s.project.track(g).map_or("—".into(), |g| g.name.clone()),
        Output::Hardware(i) => format!("Out {}/{}", i * 2 + 1, i * 2 + 2),
        Output::None => "None".into(),
    };
    let pan = t.pan;
    let pan_text = if pan.abs() < 0.01 { "C".to_owned() } else if pan < 0. { format!("{:.0}L", -pan * 50.) } else { format!("{:.0}R", pan * 50.) };
    let fpos = db_to_fader(t.volume_db);
    let fader = widgets::fader(("fader", id.0 as usize), fpos, fader_h, if is_vca { pal.solo } else { pal.accent })
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            if e.click_count >= 2 {
                this.s.set_track(id, TrackProp::VolumeDb(0.));
                cx.notify();
                return;
            }
            this.begin_drag(Drag::Fader { track: id, last_y: f32::from(e.position.y), pos: fpos, travel: fader_h - 12. }, cx);
        }));
    let (mute, solo, arm) = (t.mute, t.solo, t.arm);
    let vca_name = t.vca.and_then(|v| ws.s.project.track(v)).map(|v| v.name.clone());

    div()
        .id(("strip", id.0 as usize))
        .flex_none()
        .w(px(if is_main { STRIP_W + SP4 } else { STRIP_W }))
        .h_full()
        .p(px(SP1))
        .gap(px(SP1))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(if selected { pal.raised } else { pal.surface })
        .border_1()
        .border_color(if selected { accent_alpha(0.45) } else { border() })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.s.selected_track = Some(id);
            cx.notify();
        }))
        .child(slot(crate::paint::fit(&input, STRIP_W - 28., TEXT_XS), !is_vca, Some(Icon::Plug)))
        .child(inserts)
        .child(sends)
        .child(slot(crate::paint::fit(&output, STRIP_W - 28., TEXT_XS), true, Some(Icon::Send)))
        .child(
            div()
                .h(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(SP2))
                .when(!is_vca, |d| d.child(knob_control(("pan", id.0 as usize), (pan + 1.) / 2., true, true, KnobTarget::Pan { track: id }, cx)))
                .child(mono(if is_vca { "—".into() } else { pan_text }, TEXT_XS, pal.muted).w(px(24.))),
        )
        .child(mono(format_db(t.volume_db), TEXT_SM, pal.text).flex().justify_center())
        .child(
            div()
                .flex_1()
                .min_h(px(fader_h))
                .flex()
                .justify_center()
                .gap(px(SP1))
                .child(fader)
                .when(!is_vca, |d| d.child(div().py(px(6.)).child(widgets::meter(meter, 10., fader_h - 12.)))),
        )
        .child(
            div()
                .h(px(SLOT_H))
                .flex()
                .justify_center()
                .gap(px(SP1))
                .when(!is_main, |d| {
                    d.child(chip(("m-m", id.0 as usize), "M", mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                        this.s.set_track(id, TrackProp::Mute(!mute));
                        cx.notify();
                    })))
                    .child(chip(("m-s", id.0 as usize), "S", solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                        this.s.set_track(id, TrackProp::Solo(!solo));
                        cx.notify();
                    })))
                })
                .when(t.kind.has_clips(), |d| {
                    d.child(chip(("m-r", id.0 as usize), "R", arm, pal.record).on_click(cx.listener(move |this, _, _, cx| {
                        this.s.set_track(id, TrackProp::Arm(!arm));
                        cx.notify();
                    })))
                }),
        )
        .child(
            div()
                .h(px(32.))
                .px(px(SP1 + 2.))
                .flex()
                .flex_col()
                .justify_center()
                .rounded(px(r_sm()))
                .bg(with_alpha(color, 0.2))
                .border_t_2()
                .border_color(color)
                .child(label(crate::paint::fit(&t.name, STRIP_W - 12., TEXT_SM), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(label(vca_name.map_or_else(|| kind_name(t.kind).to_owned(), |v| format!("→ {v}")), TEXT_XS, pal.muted)),
        )
        .into_any_element()
}

fn kind_name(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Instrument => "Instrument",
        TrackKind::Audio => "Audio",
        TrackKind::Group => "Group",
        TrackKind::Folder => "Folder",
        TrackKind::Return => "Return",
        TrackKind::Vca => "VCA",
        TrackKind::Main => "Master",
    }
}
