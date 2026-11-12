//! The top bar doubles as the window's title bar and follows Live's control
//! bar: one compact row of task groups split by thin dividers, read-outs in
//! small recessed boxes, no branding.
//!
//! Left to right: window inset (macOS traffic lights), browser and file,
//! project name (drag area) · Tap, tempo, signature, metronome · key ·
//! position and transport · loop · (drag area) · CPU, panels, the
//! Session/Arrangement switch and the assistant · Windows caption buttons.

use crate::icons::{Icon, icon};
use crate::state::MainView;
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::{Drag, MenuKind, NumTarget, Workspace, click_dragged, plain_text};
use aitrack_model::PPQ;
use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, ScrollDelta,
    StatefulInteractiveElement, Styled, Window, WindowControlArea, div, point, prelude::FluentBuilder, px,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Room for the traffic lights (three 14 px buttons from x = 14, 6 px apart).
#[cfg(target_os = "macos")]
const LEFT_INSET: f32 = 76.;
#[cfg(not(target_os = "macos"))]
const LEFT_INSET: f32 = SP2;

#[cfg(target_os = "windows")]
const CAPTION_W: f32 = 46.;

/// Where menus opened from the bar drop down.
const MENU_Y: f32 = TOP_BAR_H - SP1;

// ───────────────────────────── Tap tempo ─────────────────────────────

/// A pause this long starts a new tap sequence.
const TAP_RESET: Duration = Duration::from_secs(2);
/// Taps averaged into the tempo.
const TAP_WINDOW: usize = 8;

thread_local! {
    static TAPS: RefCell<Vec<Instant>> = const { RefCell::new(Vec::new()) };
}

/// The tempo implied by a run of taps: the mean interval of the latest
/// ones. `None` until there are two taps.
fn tap_bpm(taps: &[Instant]) -> Option<f64> {
    let (first, last) = (taps.first()?, taps.last()?);
    let n = taps.len().checked_sub(1).filter(|&n| n > 0)?;
    let mean = last.duration_since(*first).as_secs_f64() / n as f64;
    (mean > 0.).then(|| (60. / mean).clamp(20., 999.))
}

/// Records a tap at `now` and returns the new tempo, if any.
fn tap(now: Instant) -> Option<f64> {
    TAPS.with(|t| {
        let mut taps = t.borrow_mut();
        if taps.last().is_some_and(|&l| now.duration_since(l) > TAP_RESET) {
            taps.clear();
        }
        taps.push(now);
        let over = taps.len().saturating_sub(TAP_WINDOW);
        taps.drain(..over);
        tap_bpm(&taps)
    })
}

// ───────────────────────────── Pieces ─────────────────────────────

/// Empty title bar space: drags the window (and snaps/maximises natively).
fn drag_area() -> Div {
    div()
        .h_full()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |e, window, cx| {
            if cfg!(target_os = "macos") {
                cx.stop_propagation();
                if e.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            }
        })
}

/// A read-out in a small recessed box, as Live's tempo and position fields.
fn readout(id: &'static str, width: f32) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(width))
        .h(px(BAR_FIELD_H))
        .px(px(SP2))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP1))
        .rounded(px(r_sm()))
        .bg(readout_bg())
        .border_1()
        .border_color(border())
}

/// A flat square toggle: tinted with `color` while on.
fn toggle(id: &'static str, i: Icon, on: bool, color: Hsla) -> gpui::Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(BAR_FIELD_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .when(on, |d| d.bg(with_alpha(color, 0.18)))
        .when(!on, |d| d.hover(|s| s.bg(hover())))
        .active(|s| s.bg(pressed()))
        .child(icon(i, 14., if on { color } else { pal.muted }))
}

/// A short text toggle (TAP), styled as a read-out box.
fn text_toggle(id: &'static str, text: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(BAR_FIELD_H))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .border_1()
        .border_color(border())
        .cursor_pointer()
        .hover(|s| s.bg(hover()).border_color(border_strong()))
        .active(|s| s.bg(pressed()))
        .child(label(text, TEXT_XS, p().muted).font_weight(FontWeight::SEMIBOLD))
}

fn separator() -> Div {
    widgets::divider_v(20.).mx(px(SP1))
}

/// A run of controls with Live's tight spacing.
fn group() -> Div {
    div().flex_none().flex().items_center().gap(px(SP1))
}

fn cpu_color(load: f32) -> Hsla {
    let pal = p();
    if load > 0.8 {
        pal.record
    } else if load > 0.5 {
        pal.meter_mid
    } else {
        pal.meter_low
    }
}

// ───────────────────────────── Render ─────────────────────────────

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let t = ws.b.engine.transport();
    let project = &ws.s.project;
    let (bar, beat, tick) = project.tempo.bars_beats(t.position);
    let sixteenth = tick / (PPQ / 4) + 1;
    let bpm = project.tempo.initial_bpm();
    let sig = project.tempo.signature_at(t.position);
    let scale = project.scale.map_or("No key".to_owned(), |s| s.display());
    let has_key = project.scale.is_some();
    let camelot = project.scale.and_then(|s| s.camelot());
    let name = if project.name.trim().is_empty() { "Untitled".to_owned() } else { project.name.clone() };
    let dirty = ws.s.history.is_dirty();
    let load = ws.b.engine.cpu_load().clamp(0., 1.);

    // ── Left: browser, file, project name ──
    // Controls must sit outside the drag areas: on Windows the system takes
    // every click in one for moving the window, so a button inside never
    // receives it.
    let left = div()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(drag_area().flex_none().w(px(LEFT_INSET)))
        .child(
            toggle("search", Icon::Search, ws.palette.open, pal.accent)
                .tooltip(widgets::tip("Search sounds, devices and actions", Some("Ctrl+K")))
                .on_click(cx.listener(|this, _, window, cx| crate::views::palette::toggle(this, window, cx))),
        )
        .child(
            toggle("file-menu", Icon::Folder, false, pal.accent)
                .tooltip(widgets::tip("File: new, open, save, templates", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::File, point(at.x - px(12.), px(MENU_Y)), cx);
                })),
        )
        .child(
            // The project name is part of the drag area, as a window title.
            drag_area()
                .flex_1()
                .min_w(px(0.))
                .pl(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .overflow_hidden()
                .child(label(name, TEXT_MD, pal.muted).font_weight(FontWeight::MEDIUM).truncate())
                .when(dirty, |d| d.child(div().flex_none().size(px(6.)).rounded_full().bg(pal.accent))),
        );

    // ── Tempo, signature, metronome ──
    let tempo = group()
        .child(
            text_toggle("tap", "TAP")
                .tooltip(widgets::tip("Tap tempo — click in time", None))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    if let Some(bpm) = tap(Instant::now()) {
                        this.s.set_tempo(bpm);
                        this.s.seal();
                    }
                    cx.notify();
                })),
        )
        // Drag vertically or scroll, Shift for fine steps; click and type.
        .child(
            readout("bpm", 64.)
                .cursor_ns_resize()
                .hover(|s| s.border_color(border_strong()))
                .tooltip(widgets::tip(
                    "Tempo — click to type, drag or scroll, Shift for fine, double-click for 120",
                    None,
                ))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                        if e.click_count == 2 {
                            this.num_edit = None;
                        this.s.set_tempo(120.);
                        this.s.seal();
                        cx.notify();
                        return;
                    }
                    let bpm = this.s.project.tempo.initial_bpm();
                    this.begin_drag(Drag::Tempo { last_y: f32::from(e.position.y), bpm }, cx);
                }))
                .on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, _, cx| {
                    let dy = match e.delta {
                        ScrollDelta::Lines(l) => l.y,
                        ScrollDelta::Pixels(p) => f32::from(p.y) / 20.,
                    };
                    if dy.abs() > 0.01 {
                        let step = if e.modifiers.shift { 0.1 } else { 1. };
                        let bpm = this.s.project.tempo.initial_bpm() + dy.signum() as f64 * step;
                        this.s.set_tempo(bpm);
                        cx.notify();
                    }
                }))
                // A click that didn't move opens the box for typing.
                .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                    if e.click_count() >= 2 || click_dragged(e) {
                        return;
                    }
                    if matches!(&this.num_edit, Some(edit) if edit.target == NumTarget::Tempo) {
                        return;
                    }
                    let bpm = this.s.project.tempo.initial_bpm();
                    this.begin_num_edit(NumTarget::Tempo, plain_text(bpm), cx);
                }))
                .child(match &ws.num_edit {
                    Some(edit) if edit.target == NumTarget::Tempo => div()
                        .flex()
                        .items_center()
                        .child(mono(edit.text.clone(), TEXT_MD, pal.accent))
                        .child(div().w(px(1.)).h(px(12.)).ml(px(1.)).bg(pal.accent))
                        .into_any_element(),
                    _ => mono(format!("{bpm:.2}"), TEXT_MD, pal.text).into_any_element(),
                }),
        )
        .child(
            readout("timesig", 44.)
                .cursor_pointer()
                .hover(|s| s.border_color(border_strong()))
                .tooltip(widgets::tip("Time signature", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::TimeSignature, point(at.x - px(24.), px(MENU_Y)), cx);
                }))
                .child(mono(format!("{}/{}", sig.numerator, sig.denominator), TEXT_MD, pal.text)),
        )
        .child(
            toggle("metronome", Icon::Metronome, t.metronome, pal.accent)
                .tooltip(widgets::tip("Metronome", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    let on = !this.b.engine.transport().metronome;
                    this.b.engine.set_metronome(on);
                    cx.notify();
                })),
        );

    // ── Key and scale ──
    let key = group().child(
        readout("scale", 112.)
            .justify_start()
            .cursor_pointer()
            .hover(|s| s.border_color(border_strong()))
            .tooltip(widgets::tip("Project key — drives piano roll, tools, samples and AI", None))
            .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                let at = e.position();
                this.open_menu(MenuKind::Scale, point(at.x - px(56.), px(MENU_Y)), cx);
            }))
            .child(icon(Icon::ScaleSnap, 12., if has_key { pal.accent } else { pal.faint }))
            .child(
                label(scale, TEXT_SM, if has_key { pal.text } else { pal.muted })
                    .font_weight(FontWeight::MEDIUM)
                    .flex_1()
                    .min_w(px(0.))
                    .truncate(),
            )
            .when_some(camelot, |d, c| d.child(mono(c, TEXT_XS, pal.faint)))
            .child(icon(Icon::ChevronDown, 10., pal.faint)),
    );

    // ── Position and transport ──
    let transport = group()
        .child(
            readout("position", 72.)
                .tooltip(widgets::tip("Arrangement position: bars . beats . sixteenths", None))
                .child(mono(format!("{bar}. {beat}. {sixteenth}"), TEXT_MD, if t.playing { pal.accent } else { pal.text })),
        )
        .child(div().w(px(SP1)))
        .child(
            toggle("play", Icon::Play, t.playing, pal.play)
                .tooltip(widgets::tip("Play / Stop", Some("Space")))
                .on_click(cx.listener(|this, _, window, cx| this.play_stop(&crate::actions::PlayStop, window, cx))),
        )
        .child(
            toggle("stop", Icon::Stop, false, pal.text)
                .tooltip(widgets::tip("Stop (twice: return to start)", None))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
        )
        .child(
            toggle("record", Icon::Record, t.recording, pal.record)
                .tooltip(widgets::tip("Record", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.b.engine.toggle_record();
                    cx.notify();
                })),
        );

    // ── Loop ──
    let looping = group().child(
        toggle("loop", Icon::Loop, t.looping, pal.accent)
            .tooltip(widgets::tip("Arrangement loop", Some("Ctrl+L")))
            .on_click(cx.listener(|this, _, _, cx| {
                let on = !this.b.engine.transport().looping;
                this.b.engine.set_looping(on);
                cx.notify();
            })),
    );

    let centre = div()
        .flex_none()
        .h_full()
        .flex()
        .items_center()
        .child(tempo)
        .child(separator())
        .child(key)
        .child(separator())
        .child(transport)
        .child(separator())
        .child(looping);

    // ── Right: CPU, panels, view switch, assistant ──
    let switch: widgets::OnSelect = {
        let weak = cx.entity().downgrade();
        Rc::new(move |i, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_view(if i == 0 { MainView::Session } else { MainView::Arrangement }, cx));
        })
    };
    let view_index = if ws.s.view == MainView::Session { 0 } else { 1 };
    let right = div()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .h_full()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(drag_area().flex_1())
        .child(
            div()
                .id("cpu")
                .flex_none()
                .h(px(BAR_FIELD_H))
                .px(px(SP1))
                .flex()
                .items_center()
                .gap(px(SP1))
                .tooltip(widgets::tip("Audio engine load", None))
                .child(label("CPU", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                .child(widgets::hbar(load, 24., 4., cpu_color(load)))
                .child(div().w(px(24.)).flex().justify_end().child(mono(format!("{:.0}%", load * 100.), TEXT_XS, pal.muted))),
        )
        .child(separator())
        .child(
            toggle("toggle-bottom", Icon::PanelBottom, ws.bottom_open, pal.accent)
                .tooltip(widgets::tip("Detail panel (devices, clip, mixer)", Some("Ctrl+Alt+L")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.bottom_open = !this.bottom_open;
                    cx.notify();
                })),
        )
        .child(
            toggle("toggle-mixer", Icon::Mixer, ws.mixer_open, pal.accent)
                .tooltip(widgets::tip("Mixer", Some("Ctrl+Alt+M")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.mixer_open = !this.mixer_open;
                    cx.notify();
                })),
        )
        .child(
            toggle("meters", Icon::Gauge, ws.meters.open, pal.accent)
                .tooltip(widgets::tip("Meters: loudness, spectrum, stereo field", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.meters.toggle();
                    cx.notify();
                })),
        )
        .child(
            toggle("preferences", Icon::Sliders, false, pal.accent)
                .tooltip(widgets::tip("Preferences", Some("Ctrl+,")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_modal(crate::views::prefs::Modal::Preferences(crate::views::prefs::PrefsPage::Audio), cx)
                })),
        )
        .child(separator())
        .child(widgets::segmented(
            "main-view",
            &[(Some(Icon::Grid), "Session"), (Some(Icon::Timeline), "Arrange")],
            view_index,
            76.,
            switch,
        ))
        .child(
            // Our difference from Live: the only accent-coloured control here.
            toggle("assistant", Icon::Sparkles, ws.ai_open, pal.accent)
                .tooltip(widgets::tip("AI assistant", Some("Ctrl+I")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ai_open = !this.ai_open;
                    cx.notify();
                })),
        )
        .child(div().flex_none().w(px(SP1)));

    div()
        .relative()
        .flex_none()
        .h(px(TOP_BAR_H))
        .w_full()
        .flex()
        .items_center()
        .gap(px(SP2))
        .bg(pane_bg(crate::appearance::Pane::TopBar))
        .border_b_1()
        .border_color(border())
        .child(left)
        .child(centre)
        .child(right)
        .children(caption_controls(window))
        .into_any_element()
}

/// Windows draws its caption buttons as the system does (Segoe Fluent
/// glyphs, full height, red close); they hit-test as the real caption buttons
/// so Snap Layouts work. Adapted from monoBOT's `window_chrome.rs`.
#[cfg(target_os = "windows")]
fn caption_controls(window: &Window) -> Option<AnyElement> {
    let max = window.is_maximized();
    let active = window.is_window_active();
    let button = |id: &'static str, glyph: &'static str, area: WindowControlArea| {
        let close = matches!(area, WindowControlArea::Close);
        div()
            .id(id)
            .w(px(CAPTION_W))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family("Segoe Fluent Icons")
            .text_size(px(10.))
            .text_color(content(if active { 0.85 } else { 0.45 }))
            .map(|b| {
                if close {
                    b.hover(|s| s.bg(caption_close()).text_color(gpui::white()))
                } else {
                    b.hover(|s| s.bg(content(0.06)))
                }
            })
            .window_control_area(area)
            .child(glyph)
    };
    Some(
        div()
            .flex_none()
            .h(px(TOP_BAR_H))
            .flex()
            .occlude()
            .child(button("win-min", "\u{e921}", WindowControlArea::Min))
            .child(button("win-max", if max { "\u{e923}" } else { "\u{e922}" }, WindowControlArea::Max))
            .child(button("win-close", "\u{e8bb}", WindowControlArea::Close))
            .into_any_element(),
    )
}

#[cfg(not(target_os = "windows"))]
fn caption_controls(_: &Window) -> Option<AnyElement> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_tempo_averages_intervals() {
        let t0 = Instant::now();
        let taps: Vec<Instant> = (0..4).map(|i| t0 + Duration::from_millis(500 * i)).collect();
        let bpm = tap_bpm(&taps).unwrap();
        assert!((bpm - 120.).abs() < 1e-6, "{bpm}");
        assert_eq!(tap_bpm(&taps[..1]), None);
        assert_eq!(tap_bpm(&[]), None);
    }

    #[test]
    fn tap_tempo_resets_after_a_pause() {
        let t0 = Instant::now();
        assert_eq!(tap(t0), None);
        assert!(tap(t0 + Duration::from_millis(400)).is_some());
        // A long pause starts over.
        assert_eq!(tap(t0 + Duration::from_secs(10)), None);
        let bpm = tap(t0 + Duration::from_secs(10) + Duration::from_millis(1000)).unwrap();
        assert!((bpm - 60.).abs() < 1e-6, "{bpm}");
    }

    #[test]
    fn bar_sizes_sit_on_the_grid() {
        const { assert!(BAR_FIELD_H < TOP_BAR_H) };
        assert_eq!((TOP_BAR_H - BAR_FIELD_H) % 4., 0.);
        assert_eq!(LEFT_INSET % 4., 0.);
    }
}
