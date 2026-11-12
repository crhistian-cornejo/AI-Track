//! The top bar doubles as the window's title bar: identity on the left, the
//! transport cluster centred, view switch, CPU and the assistant on the right.

use crate::icons::{Icon, icon};
use crate::state::{BAR, MainView};
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::{Drag, MenuKind, Workspace};
use aitrack_model::PPQ;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, ScrollDelta,
    StatefulInteractiveElement, Styled, Window, WindowControlArea, div, point, prelude::FluentBuilder, px,
};
use std::rc::Rc;

#[cfg(target_os = "macos")]
const LEFT_INSET: f32 = 80.;
#[cfg(not(target_os = "macos"))]
const LEFT_INSET: f32 = SP3;

#[cfg(target_os = "windows")]
const CAPTION_W: f32 = 46.;

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

fn field(id: &'static str, width: f32) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(width))
        .h(px(32.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_lg()))
        .bg(p().panel)
        .border_1()
        .border_color(border())
}

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let t = ws.b.engine.transport();
    let project = &ws.s.project;
    let (bar, beat, tick) = project.tempo.bars_beats(t.position);
    let sixteenth = tick / (PPQ / 4) + 1;
    let bpm = project.tempo.initial_bpm();
    let sig = project.tempo.signature_at(t.position);
    let scale = project.scale.map_or("No key".to_owned(), |s| s.display());
    let camelot = project.scale.and_then(|s| s.camelot());
    let dirty = ws.s.history.is_dirty();
    let cpu = ws.b.engine.cpu_load();

    // The Session ⇄ Arrangement switch sits with the identity, balancing the bar.
    let switch: widgets::OnSelect = {
        let weak = cx.entity().downgrade();
        Rc::new(move |i, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_view(if i == 0 { MainView::Session } else { MainView::Arrangement }, cx));
        })
    };
    let view_index = if ws.s.view == MainView::Session { 0 } else { 1 };
    // ── Left: identity ──
    let left = drag_area()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .pl(px(LEFT_INSET))
        .gap(px(SP2))
        .flex()
        .items_center()
        .child(icon(Icon::Logo, 20., pal.accent))
        .child(label("AI-Track", TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
        .when(dirty, |d| d.child(div().size(px(6.)).rounded_full().bg(pal.accent)))
        .child(
            widgets::icon_button("toggle-browser", Icon::PanelLeft, ws.browser_open)
                .tooltip(widgets::tip("Browser", Some("Ctrl+Alt+B")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.browser_open = !this.browser_open;
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("toggle-bottom", Icon::PanelBottom, ws.bottom_open)
                .tooltip(widgets::tip("Bottom panel", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.bottom_open = !this.bottom_open;
                    cx.notify();
                })),
        )
        .child(div().w(px(SP1)))
        .child(widgets::segmented(
            "main-view",
            &[(Some(Icon::Grid), "Session"), (Some(Icon::Timeline), "Arrange")],
            view_index,
            80.,
            switch,
        ));

    // ── Centre: transport ──
    let transport = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(
            widgets::transport_button("play", Icon::Play, t.playing, pal.play)
                .tooltip(widgets::tip("Play / Stop", Some("Space")))
                .on_click(cx.listener(|this, _, window, cx| this.play_stop(&crate::actions::PlayStop, window, cx))),
        )
        .child(
            widgets::transport_button("stop", Icon::Stop, false, pal.text)
                .tooltip(widgets::tip("Stop (twice: return to start)", None))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
        )
        .child(
            widgets::transport_button("record", Icon::Record, t.recording, pal.record)
                .tooltip(widgets::tip("Record", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.b.engine.toggle_record();
                    cx.notify();
                })),
        )
        .child(div().w(px(SP1)))
        .child(
            widgets::icon_button("loop", Icon::Loop, t.looping)
                .tooltip(widgets::tip("Loop", Some("Ctrl+L")))
                .on_click(cx.listener(|this, _, _, cx| {
                    let on = !this.b.engine.transport().looping;
                    this.b.engine.set_looping(on);
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("metronome", Icon::Metronome, t.metronome)
                .tooltip(widgets::tip("Metronome", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    let on = !this.b.engine.transport().metronome;
                    this.b.engine.set_metronome(on);
                    cx.notify();
                })),
        )
        .child(div().w(px(SP2)))
        // BPM: drag vertically, scroll, shift for fine steps.
        .child(
            field("bpm", 96.)
                .justify_between()
                .cursor_ns_resize()
                .tooltip(widgets::tip("Tempo — drag or scroll, Shift for fine", None))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                    if e.click_count == 2 {
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
                .child(mono(format!("{bpm:.2}"), TEXT_LG, pal.text))
                .child(label("BPM", TEXT_XS, pal.faint).font_weight(FontWeight::MEDIUM)),
        )
        .child(
            field("timesig", 56.)
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Time signature", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::TimeSignature, point(at.x - px(28.), px(TOP_BAR_H - 4.)), cx);
                }))
                .child(mono(format!("{}/{}", sig.numerator, sig.denominator), TEXT_LG, pal.text)),
        )
        .child(
            field("position", 88.)
                .justify_center()
                .gap(px(SP1))
                .tooltip(widgets::tip("Bars . Beats . Sixteenths", None))
                .child(mono(format!("{bar}.{beat}.{sixteenth}"), TEXT_LG, if t.playing { pal.accent } else { pal.text })),
        )
        .child(
            field("scale", 136.)
                .gap(px(SP2))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Project key — drives piano roll, tools, samples and AI", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::Scale, point(at.x - px(74.), px(TOP_BAR_H - 4.)), cx);
                }))
                .child(icon(Icon::MusicNote, 14., pal.accent))
                .child(label(scale, TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
                .when_some(camelot, |d, c| d.child(widgets::badge(c, pal.muted)))
                .child(icon(Icon::ChevronDown, 12., pal.faint)),
        );

    let right = div()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .h_full()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(drag_area().flex_1())
        .child(
            div()
                .id("cpu")
                .flex_none()
                .h(px(32.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_lg()))
                .tooltip(widgets::tip("DSP load", None))
                .child(widgets::hbar(cpu, 28., 4., if cpu > 0.8 { pal.record } else { pal.accent }))
                .child(mono(format!("{:>2.0}%", cpu * 100.), TEXT_SM, pal.muted).w(px(28.))),
        )
        .child(
            widgets::icon_button("meters", Icon::Gauge, ws.meters.open)
                .tooltip(widgets::tip("Meters: loudness, spectrum, stereo field", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.meters.toggle();
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("export", Icon::Send, false)
                .tooltip(widgets::tip("Export audio", Some("Ctrl+Shift+E")))
                .on_click(cx.listener(|this, _, _, cx| this.open_export(cx))),
        )
        .child(
            widgets::icon_button("preferences", Icon::Sliders, false)
                .tooltip(widgets::tip("Preferences", Some("Ctrl+,")))
                .on_click(cx.listener(|this, _, _, cx| this.open_modal(crate::views::prefs::Modal::Preferences(crate::views::prefs::PrefsPage::Audio), cx))),
        )
        .child(
            widgets::icon_button("theme", if is_dark() { Icon::Sun } else { Icon::Moon }, false)
                .tooltip(widgets::tip("Light / dark theme", None))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_theme(window, cx))),
        )
        .child(
            div()
                .id("assistant")
                .flex_none()
                .h(px(32.))
                .px(px(SP3))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_lg()))
                .cursor_pointer()
                .bg(if ws.ai_open { accent_alpha(0.16) } else { pal.panel })
                .border_1()
                .border_color(if ws.ai_open { accent_alpha(0.5) } else { border() })
                .hover(|s| s.border_color(accent_alpha(0.6)))
                .tooltip(widgets::tip("AI assistant", Some("Ctrl+I")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ai_open = !this.ai_open;
                    cx.notify();
                }))
                .child(icon(Icon::Sparkles, 16., pal.accent))
                .child(label("Assistant", TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM)),
        )
        .child(div().flex_none().w(px(SP1)));

    let _ = BAR;
    div()
        .relative()
        .flex_none()
        .h(px(TOP_BAR_H))
        .w_full()
        .flex()
        .items_center()
        .gap(px(SP2))
        .bg(pane_bg(crate::appearance::Pane::TopBar))
        .child(left)
        .child(transport)
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
