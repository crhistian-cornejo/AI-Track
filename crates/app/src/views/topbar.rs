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

/// Room for the macOS window buttons at 100 % zoom, for sizing the bar; the
/// width actually left is measured each frame (`traffic_lights::inset`).
#[cfg(all(test, target_os = "macos"))]
const LEFT_INSET: f32 = 80.;
#[cfg(all(test, not(target_os = "macos")))]
const LEFT_INSET: f32 = SP2;

#[cfg(target_os = "windows")]
const CAPTION_W: f32 = 46.;

/// Where menus opened from the bar drop down.
fn menu_y() -> f32 {
    m().top_bar_h - SP1
}

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
        .h(px(m().bar_field_h))
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
        .size(px(m().bar_field_h))
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
fn text_toggle(id: &'static str, text: impl Into<gpui::SharedString>) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(m().bar_field_h))
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

/// Count-in choices in the order a click cycles them: off, 1, 2, 4 bars.
fn next_count_in(bars: u32) -> u32 {
    match bars {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 0,
    }
}

fn count_in_label(bars: u32, long: bool) -> &'static str {
    match (bars, long) {
        (0, true) => "Count-in off",
        (1, true) => "Count-in 1",
        (2, true) => "Count-in 2",
        (_, true) => "Count-in 4",
        (0, false) => "CI off",
        (1, false) => "CI 1",
        (2, false) => "CI 2",
        (_, false) => "CI 4",
    }
}

/// What pressing Record does. It records the notes played on the armed
/// instrument tracks and, with Automation Arm, the plugin controls moved
/// meanwhile. With neither there is nothing to capture: it says so instead of
/// lighting up and recording nothing. A button already lit can always be
/// switched off.
#[derive(Debug, PartialEq, Eq)]
enum RecordPress {
    Stop,
    Arm,
    Unavailable,
}

fn record_press(recording: bool, can_record: bool) -> RecordPress {
    match (recording, can_record) {
        (true, _) => RecordPress::Stop,
        (false, true) => RecordPress::Arm,
        (false, false) => RecordPress::Unavailable,
    }
}

/// Whether Record has something to capture: an armed instrument track, an
/// armed audio track that has an input, or Automation Arm for plugin moves.
fn can_record(s: &crate::state::AppState) -> bool {
    use aitrack_model::{InputSource, TrackKind};
    s.automation_arm
        || s.project.tracks.iter().any(|t| t.arm && (t.kind == TrackKind::Instrument || (t.kind == TrackKind::Audio && t.input != InputSource::None)))
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

/// What the bar shows at a window width. Controls go in this order as the
/// window narrows: the CPU read-out and the long count-in label first, then
/// the computer-keyboard and meters buttons. The centre, the view switch and
/// the panel buttons always stay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BarLayout {
    cpu: bool,
    long_labels: bool,
    extras: bool,
}

impl BarLayout {
    fn for_width(width: f32) -> Self {
        Self { cpu: width >= 1400., long_labels: width >= 1400., extras: width >= 1280. }
    }

    /// Width the right-hand controls need (px), so the left part gives way
    /// before they are clipped. Counted from the controls themselves, with a
    /// little to spare.
    fn right_width(self) -> f32 {
        let mut w = 24. * 3. + 24. + 156. + 24. + 3. * SP1 + 9. + SP1 * 6.;
        if self.cpu {
            w += 92. + 9. + 2. * SP1;
        }
        if self.extras {
            w += 28. + 24. + 2. * SP1;
        }
        w
    }
}

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let layout = BarLayout::for_width(f32::from(window.viewport_size().width));
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
    // Left and right start from the same width and grow alike, so the centre
    // stays centred; the right never shrinks below its controls, the left does.
    let side = layout.right_width();
    let left = div()
        .flex_grow(1.)
        .flex_shrink(1.)
        .flex_basis(px(side))
        .min_w(px(0.))
        .h_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(drag_area().flex_none().w(px(crate::traffic_lights::inset(window))))
        .child(
            toggle("search", Icon::Search, ws.palette.open, pal.accent)
                .tooltip(widgets::tip("Search sounds, devices and actions", Some("Ctrl+K")))
                .on_click(cx.listener(|this, _, window, cx| crate::views::palette::toggle(this, window, cx))),
        )
        .child(
            toggle("file-menu", Icon::Folder, false, pal.accent)
                .tooltip(widgets::tip("Project: new, open, save, export", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::File, point(at.x - px(12.), px(menu_y())), cx);
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
                .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                    if e.click_count() >= 2 || click_dragged(e) {
                        return;
                    }
                    if matches!(&this.num_edit, Some(edit) if edit.target == NumTarget::Tempo) {
                        return;
                    }
                    this.focus.focus(window, cx);
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
                    this.open_menu(MenuKind::TimeSignature, point(at.x - px(24.), px(menu_y())), cx);
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
                this.open_menu(MenuKind::Scale, point(at.x - px(56.), px(menu_y())), cx);
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
                .tooltip(if can_record(&ws.s) {
                    widgets::tip("Record what you play (and the input of audio tracks) on the armed tracks, and plugin moves with Automation Arm", None)
                } else {
                    widgets::tip("Arm a track (R on its header) to record on it: an instrument, or an audio track with an input", None)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    match record_press(this.b.engine.transport().recording, can_record(&this.s)) {
                        RecordPress::Stop => this.b.engine.toggle_record(),
                        RecordPress::Arm => {
                            this.b.engine.toggle_record();
                            // Record starts the song too, as in every DAW.
                            if !this.b.engine.transport().playing {
                                this.b.engine.play();
                            }
                        }
                        RecordPress::Unavailable => {
                            this.toast("Arm a track first: click R on its header (an audio track also needs an input), then Record", cx);
                        }
                    }
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

    // ── Launch: count-in, global quantisation, Back to Arrangement ──
    let count_in = ws.b.engine.count_in();
    let global_q = ws.b.engine.global_quantize();
    let following_session = ws.s.project.tracks.iter().any(|tr| ws.b.engine.slot_state(tr.id).is_some());
    let launch = group()
        .child(
            text_toggle("count-in", count_in_label(count_in, layout.long_labels))
                .tooltip(widgets::tip("Count-in: bars of metronome before playback starts (click to change: off, 1, 2, 4)", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    let next = next_count_in(this.b.engine.count_in());
                    this.b.engine.set_count_in(next);
                    cx.notify();
                })),
        )
        .child(
            text_toggle("global-quantize", format!("Q {}", crate::views::bottom::quantize_name(global_q)))
                .tooltip(widgets::tip("Global launch quantisation: scene launches into empty slots and clip stops (click to change)", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    let next = crate::views::bottom::next_quantize(this.b.engine.global_quantize());
                    this.b.engine.set_global_quantize(next);
                    cx.notify();
                })),
        )
        .child(
            toggle("back-to-arrangement", Icon::ArrangeReturn, following_session, pal.record)
                .tooltip(widgets::tip("Back to Arrangement: stop the Session clips and follow the arrangement again", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.b.engine.back_to_arrangement();
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
        .child(looping)
        .child(separator())
        .child(launch);

    // ── Right: CPU, panels, view switch, assistant ──
    let switch: widgets::OnSelect = {
        let weak = cx.entity().downgrade();
        Rc::new(move |i, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_view(if i == 0 { MainView::Session } else { MainView::Arrangement }, cx));
        })
    };
    let view_index = if ws.s.view == MainView::Session { 0 } else { 1 };
    let right = div()
        .flex_grow(1.)
        .flex_shrink_0()
        .flex_basis(px(side))
        .min_w(px(side))
        .overflow_hidden()
        .h_full()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(drag_area().flex_1())
        .when(layout.cpu, |d| d.child(
            div()
                .id("cpu")
                .flex_none()
                .h(px(m().bar_field_h))
                .px(px(SP1))
                .flex()
                .items_center()
                .gap(px(SP1))
                .tooltip(widgets::tip("Audio engine load", None))
                .child(label("CPU", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                .child(widgets::hbar(load, 24., 4., cpu_color(load)))
                .child(div().w(px(24.)).flex().justify_end().child(mono(format!("{:.0}%", load * 100.), TEXT_XS, pal.muted))),
        ).child(separator()))
        .when(layout.extras, |d| d.child(
            div()
                .id("computer-midi-kbd")
                .flex_none()
                .h(px(m().bar_field_h))
                .px(px(SP1 + 2.))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if ws.piano_keyboard { accent_alpha(0.18) } else { content(0.0) })
                .hover(|s| s.bg(if ws.piano_keyboard { accent_alpha(0.26) } else { hover() }))
                .tooltip(widgets::tip("Computer MIDI Keyboard (A–P notes, Z/X octave, C/V velocity)", Some("M")))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_computer_keyboard(cx);
                    if this.piano_keyboard {
                        this.focus.focus(window, cx);
                    }
                }))
                .child(icon(Icon::Keyboard, 14., if ws.piano_keyboard { pal.accent } else { pal.muted }))
                .when(ws.piano_keyboard, |d| d.child(mono(format!("C{}", ws.piano_octave), TEXT_XS, pal.text))),
        ))
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
        .when(layout.extras, |d| d.child(
            toggle("meters", Icon::Gauge, ws.meters.open, pal.accent)
                .tooltip(widgets::tip("Meters: loudness, spectrum, stereo field", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.meters.toggle();
                    cx.notify();
                })),
        ))
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
        .h(px(m().top_bar_h))
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
            .h(px(m().top_bar_h))
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
    fn record_never_arms_silently() {
        // Nothing armed and no Automation Arm: Record only says what to do.
        assert_eq!(record_press(false, false), RecordPress::Unavailable);
        // An armed track (or Automation Arm) gives it something to capture.
        assert_eq!(record_press(false, true), RecordPress::Arm);
        // A lit button always switches off, whatever is armed now.
        assert_eq!(record_press(true, false), RecordPress::Stop);
        assert_eq!(record_press(true, true), RecordPress::Stop);
    }

    #[test]
    fn record_has_something_to_capture_with_an_armed_instrument_track_or_automation_arm() {
        use aitrack_model::{Project, TrackKind, TrackProp};
        let mut s = crate::state::AppState::new(Project::new("t"));
        assert!(!can_record(&s));
        let audio = s.add_track(TrackKind::Audio).unwrap();
        s.set_track(audio, TrackProp::Arm(true));
        assert!(!can_record(&s), "an audio track with no input has nothing to record");
        s.set_track(audio, TrackProp::Input(aitrack_model::InputSource::Hardware { first: 0, stereo: false }));
        assert!(can_record(&s), "an armed audio track with an input records");
        s.set_track(audio, TrackProp::Arm(false));
        assert!(!can_record(&s));
        let keys = s.add_track(TrackKind::Instrument).unwrap();
        s.set_track(keys, TrackProp::Arm(true));
        assert!(can_record(&s));
        s.set_track(keys, TrackProp::Arm(false));
        s.automation_arm = true;
        assert!(can_record(&s));
    }

    #[test]
    fn count_in_cycles_and_reaches_the_engine() {
        assert_eq!([0, 1, 2, 4, 0].map(next_count_in), [1, 2, 4, 0, 1]);
        assert_eq!((count_in_label(0, true), count_in_label(4, false)), ("Count-in off", "CI 4"));
        use crate::bridge::EngineBridge;
        let mut engine = crate::bridge::StubEngine::default();
        assert_eq!((engine.count_in(), engine.global_quantize()), (0, aitrack_model::Quantize::Bar));
        engine.set_count_in(next_count_in(engine.count_in()));
        engine.set_global_quantize(crate::views::bottom::next_quantize(engine.global_quantize()));
        assert_eq!((engine.count_in(), engine.global_quantize()), (1, aitrack_model::Quantize::TwoBars));
    }

    #[test]
    fn the_bar_drops_controls_in_order_as_the_window_narrows() {
        let wide = BarLayout::for_width(1480.);
        let mid = BarLayout::for_width(1300.);
        let narrow = BarLayout::for_width(1200.);
        assert_eq!((wide.cpu, wide.long_labels, wide.extras), (true, true, true));
        assert_eq!((mid.cpu, mid.long_labels, mid.extras), (false, false, true));
        assert_eq!((narrow.cpu, narrow.extras), (false, false));
        assert!(wide.right_width() > mid.right_width() && mid.right_width() > narrow.right_width());
        // Left (the window buttons and search) + centre (about 680 px) + right fit each width.
        for (w, l) in [(1480., wide), (1280., mid), (1200., narrow)] {
            assert!(LEFT_INSET + 4. * 24. + 680. + l.right_width() + 2. * SP2 <= w, "{w}");
        }
    }

    #[test]
    fn bar_sizes_sit_on_the_grid() {
        for m in [Metrics::COMFORTABLE, Metrics::COMPACT] {
            assert!(m.bar_field_h < m.top_bar_h);
            assert_eq!((m.top_bar_h - m.bar_field_h) % 4., 0.);
        }
        assert_eq!(LEFT_INSET % 4., 0.);
    }
}
