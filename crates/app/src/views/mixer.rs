//! Mixer (Logic-style channel strips): input, inserts, sends, output, pan,
//! fader with meter, mute/solo/arm and the name plate, for tracks, groups,
//! returns and VCAs, with Main pinned on the right while the tracks scroll
//! sideways. Every row has a fixed height so the strips line up; when the
//! mixer is short whole rows go (see [`rows`]) and the fader and meter take
//! what is left.

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

/// Which rows a strip shows and how tall its fader is, for a mixer of
/// height `h` with `returns` return tracks. Rows go in order of how much
/// they matter as the mixer gets shorter: inserts first, then the input,
/// the sends, the output and finally pan, so the fader and the meter keep
/// the room (as in Live's mixer, which hides sections when it is short).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rows {
    pub input: bool,
    pub inserts: usize,
    pub sends: bool,
    pub output: bool,
    pub pan: bool,
    pub fader_h: f32,
}

/// Space a strip's fixed parts need: row and strip padding, border, the
/// name plate, mute/solo/arm and the dB readout, with the gaps between them.
const FIXED_H: f32 = 2. * SP2 + 2. * SP1 + 2. + NAME_H + SLOT_H + DB_H + 3. * SP1;
const NAME_H: f32 = 32.;
const DB_H: f32 = 16.;
const PAN_H: f32 = 44.;
/// The fader never gets shorter than this.
pub const FADER_MIN: f32 = 40.;
const FADER_MAX: f32 = 360.;

pub fn rows(h: f32, returns: usize) -> Rows {
    let mut left = h - FIXED_H;
    let mut r = Rows { input: false, inserts: 0, sends: false, output: false, pan: false, fader_h: 0. };
    // (row height, fader room the row must leave) in the order rows come back.
    let sends_h = returns as f32 * (SLOT_H + 2.) - 2.;
    let mut fits = |need: f32, keep: f32| {
        if need <= 0. {
            return true;
        }
        if left - (need + SP1) >= keep {
            left -= need + SP1;
            true
        } else {
            false
        }
    };
    r.pan = fits(PAN_H, 56.);
    r.output = r.pan && fits(SLOT_H, 72.);
    r.sends = r.output && returns > 0 && fits(sends_h, 80.);
    let sends_ok = r.output && (returns == 0 || r.sends);
    r.input = sends_ok && fits(SLOT_H, 96.);
    if r.input {
        while r.inserts < INSERTS && fits(SLOT_H + if r.inserts == 0 { 0. } else { 2. }, 112.) {
            r.inserts += 1;
        }
    }
    r.fader_h = left.clamp(FADER_MIN, FADER_MAX);
    r
}

pub fn render(ws: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let order = |k: TrackKind| match k {
        TrackKind::Instrument | TrackKind::Audio | TrackKind::Group | TrackKind::Folder => 0,
        TrackKind::Return => 1,
        TrackKind::Vca => 2,
        TrackKind::Main => 3,
    };
    let mut tracks: Vec<(usize, TrackId)> = ws.s.project.tracks.iter().enumerate().map(|(i, t)| (order(t.kind) * 1000 + i, t.id)).collect();
    tracks.sort();
    let returns: Vec<(TrackId, String)> = ws.s.project.tracks.iter().filter(|t| t.kind == TrackKind::Return).map(|t| (t.id, t.name.clone())).collect();
    // The section gives way to the arrangement on short windows.
    let h = ws.mixer_h_fitted() - 2.;
    let layout = rows(h, returns.len());
    // Tracks scroll sideways; returns, VCAs and Main stay pinned on the right.
    let mut scrolling = div().h_full().flex().gap(px(SP1)).p(px(SP2));
    let mut pinned = div().flex_none().h_full().flex().gap(px(SP1)).p(px(SP2)).border_l_1().border_color(border());
    let mut last_group = 1;
    for (k, id) in tracks {
        let group = k / 1000;
        if group == 0 {
            scrolling = scrolling.child(strip(ws, id, &returns, layout, cx));
            continue;
        }
        if group != last_group {
            pinned = pinned.child(div().w(px(SP1)).h_full().flex().justify_center().child(div().w(px(1.)).h_full().bg(border())));
            last_group = group;
        }
        pinned = pinned.child(strip(ws, id, &returns, layout, cx));
    }
    div()
        .size_full()
        .flex()
        .child(
            div()
                .id("mixer-scroll")
                .flex_1()
                .min_w(px(0.))
                .h_full()
                // Only the x axis scrolls, so a mouse wheel scrolls sideways too
                // and a trackpad pans with its own pixel deltas.
                .overflow_x_scroll()
                .child(scrolling),
        )
        .child(pinned)
        .into_any_element()
}

fn strip(ws: &mut Workspace, id: TrackId, returns: &[(TrackId, String)], rows: Rows, cx: &mut Context<Workspace>) -> AnyElement {
    let fader_h = rows.fader_h;
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
    for i in 0..rows.inserts {
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
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_param_menu(id, AutomationTarget::Send { index: index as u16 }, e.position, cx);
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
        }))
        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.open_param_menu(id, AutomationTarget::Volume, e.position, cx);
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
        .when(rows.input, |d| d.child(slot(crate::paint::fit(&input, STRIP_W - 28., TEXT_XS), !is_vca, Some(Icon::Plug))))
        .when(rows.inserts > 0, |d| d.child(inserts))
        .when(rows.sends, |d| d.child(sends))
        .when(rows.output, |d| d.child(slot(crate::paint::fit(&output, STRIP_W - 28., TEXT_XS), true, Some(Icon::Send))))
        .when(rows.pan, |d| d.child(
            div()
                .h(px(PAN_H))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(SP2))
                .when(!is_vca, |d| {
                    d.child(knob_control(("pan", id.0 as usize), (pan + 1.) / 2., true, true, KnobTarget::Pan { track: id }, cx).on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_param_menu(id, AutomationTarget::Pan, e.position, cx);
                        }),
                    ))
                })
                .child(mono(if is_vca { "—".into() } else { pan_text }, TEXT_XS, pal.muted).w(px(24.))),
        ))
        .child(mono(format_db(t.volume_db), TEXT_SM, pal.text).h(px(DB_H)).flex().justify_center())
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
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
                .h(px(NAME_H))
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

/// Right-click on a fader, a pan knob or a send (here or in a track
/// header): show that control's automation lane in the arrangement, or
/// delete the automation it has.
pub fn render_param_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let (track, target, at) = ws.param_menu?;
    let Some(t) = ws.s.project.track(track) else {
        ws.param_menu = None;
        return None;
    };
    let has = t.automation.iter().any(|l| l.target == target && !l.points.is_empty());
    let shown = ws.s.shown_lanes(track).contains(&target);
    let name = crate::views::arrangement::target_name(ws, target);
    let card = widgets::menu_card("param-menu", 220.)
        .child(widgets::menu_header(name.to_uppercase()))
        .child(widgets::menu_item("param-show", Some(Icon::Automation), if shown { "Go to automation" } else { "Show automation" }, None, false).on_click(cx.listener(
            move |this, _, _, cx| this.show_automation(track, target, cx),
        )))
        .when(has, |d| {
            d.child(widgets::menu_item("param-delete", Some(Icon::Trash), "Delete automation", None, false).on_click(cx.listener(move |this, _, _, cx| {
                this.param_menu = None;
                this.s.delete_automation(track, target);
                cx.notify();
            })))
        });
    Some(
        gpui::deferred(
            gpui::anchored().position(at).snap_to_window_with_margin(px(SP2)).child(
                div()
                    .id("param-menu-layer")
                    .occlude()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.param_menu = None;
                        cx.notify();
                    }))
                    .child(card),
            ),
        )
        .with_priority(2)
        .into_any_element(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tall_mixer_shows_every_row() {
        let r = rows(520., 2);
        assert!(r.input && r.sends && r.output && r.pan);
        assert_eq!(r.inserts, INSERTS);
        assert!(r.fader_h >= 112.);
    }

    #[test]
    fn rows_go_in_order_as_the_mixer_shrinks() {
        let mut last = rows(600., 2);
        for h in (120..600).rev().step_by(4) {
            let r = rows(h as f32, 2);
            // Nothing comes back while shrinking, and pan is the last to go.
            assert!(r.inserts <= last.inserts);
            assert!(!r.input || last.input);
            assert!(!r.sends || last.sends);
            assert!(!r.output || last.output);
            assert!(!r.pan || last.pan);
            assert!(!r.sends || r.output);
            assert!(r.inserts == 0 || r.input);
            last = r;
        }
        let small = rows(160., 2);
        assert!(!small.sends && small.inserts == 0);
        assert!(small.fader_h >= FADER_MIN);
    }

    #[test]
    fn the_strip_fits_its_height() {
        for h in [200., 264., 320., 400., 520.] {
            let r = rows(h, 3);
            let mut used = FIXED_H + r.fader_h + SP1;
            if r.pan {
                used += PAN_H + SP1;
            }
            if r.output {
                used += SLOT_H + SP1;
            }
            if r.input {
                used += SLOT_H + SP1;
            }
            if r.sends {
                used += 3. * (SLOT_H + 2.) - 2. + SP1;
            }
            if r.inserts > 0 {
                used += r.inserts as f32 * (SLOT_H + 2.) - 2. + SP1;
            }
            assert!(used <= h + SP1 + 0.5 || r.fader_h == FADER_MIN, "{h}: {used}");
        }
    }
}
