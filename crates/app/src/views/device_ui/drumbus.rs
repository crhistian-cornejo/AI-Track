//! `fx.drumbus`: a row of the bus's stages, each with its own switch, under
//! a view of the Boom. The view shows the last 640 ms of the input's low
//! band (the grey area the Boom listens to) with the synthesised low end
//! laid over it, and flashes on every hit the Boom locks to. The bar on its
//! right edge is the Boom amount: drag it.

use super::dynamics::win;
use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, set_now};
use crate::theme::*;
use crate::widgets::{self, label};
use crate::workspace::Workspace;
use aitrack_model::DeviceId;
use aitrack_stock::fx::drumbus::{drive_curve, p};
use gpui::{
    AnyElement, App, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, TextAlign,
    Window, div, point, prelude::FluentBuilder, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Meter offsets (see the plugin's layout note).
const GR: usize = 0;
const LATENCY: usize = 3;
const TRIGGERS: usize = 5;
const TRANS_DB: usize = 6;
const BOOM_HIST: usize = 7;
const KEY_HIST: usize = 71;
const BUCKETS: usize = 64;
/// Meter length when the plugin reports everything.
const METER_LEN: usize = 135;
/// How long a hit's flash takes to fade.
const FLASH: Duration = Duration::from_millis(280);
/// The share of the view's height a full-scale level reaches.
const REACH: f32 = 0.86;
/// The share of the view's height the Boom bar uses, centred.
const GAUGE_SPAN: f32 = 0.9;
/// Boom's frequency range (the parameter's), Hz, and the ruler along the
/// view's bottom edge that shows it as a keyboard: where it starts and ends
/// across the view, and its row.
const BOOM_LO: f32 = 30.;
const BOOM_HI: f32 = 120.;
const RULER_L: f32 = 0.03;
const RULER_R: f32 = 0.93;
const RULER_Y: f32 = 0.78;
/// Height of a stage's mini display.
const MINI_H: f32 = 32.;
/// Drive: pre-gain at 100 %, dB, and the level it keeps the signal at (as the plugin).
const DRIVE_MAX_DB: f32 = 18.;
const DRIVE_REF: f32 = 0.5;
/// Compress: threshold at 100 %, and the span of its reduction bar, dB.
const COMP_GR_SPAN: f32 = 12.;
/// Transients: span of the applied-gain bar each way, dB.
const TRANS_SPAN: f32 = 12.;

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Frequency of MIDI note `m` (A4 = 69 = 440 Hz).
pub fn note_hz(m: i32) -> f32 {
    440. * 2f32.powf((m as f32 - 69.) / 12.)
}

/// The MIDI note nearest to `hz`.
pub fn nearest_note(hz: f32) -> i32 {
    (69. + 12. * (hz.max(1.) / 440.).log2()).round() as i32
}

/// A note's name with its octave (C4 = 60): "A1" is 55 Hz.
pub fn note_name(m: i32) -> String {
    format!("{}{}", NOTE_NAMES[m.rem_euclid(12) as usize], m.div_euclid(12) - 1)
}

/// `hz` moved to its nearest note, kept inside Boom's range.
pub fn snap_to_note(hz: f32) -> f32 {
    let mut m = nearest_note(hz);
    while note_hz(m) < BOOM_LO {
        m += 1;
    }
    while note_hz(m) > BOOM_HI {
        m -= 1;
    }
    note_hz(m)
}

/// Where a Boom frequency sits along the ruler (0..1; the range is two octaves).
pub fn boom_pos(hz: f32) -> f32 {
    ((hz / BOOM_LO).max(1e-3).ln() / (BOOM_HI / BOOM_LO).ln()).clamp(0., 1.)
}

/// The drive stage's output for an input amplitude: the curve at the pre-gain
/// the Drive amount makes, level-matched like the plugin.
pub fn drive_out(choice: usize, amount: f32, x: f32) -> f32 {
    let curve = drive_curve(choice);
    let g = 10f32.powf(DRIVE_MAX_DB * amount / 20.);
    let y = curve.f(f64::from(g * DRIVE_REF), 0.) as f32;
    let makeup = if y > 1e-6 { DRIVE_REF / y } else { 1. };
    curve.f(f64::from(g * x), 0.) as f32 * makeup
}

/// Magnitude of the Damp low-pass (2nd order, Q = 1/sqrt 2) at `f`, dB.
pub fn damp_db(cutoff: f32, f: f32) -> f32 {
    let r = f / cutoff.max(1.);
    -10. * (1. + r.powi(4)).log10()
}

/// Turns the plugin's trigger counter into a flash that fades after each hit.
#[derive(Clone, Copy, Debug, Default)]
pub struct HitFlash {
    seen: Option<f32>,
    at: Option<Instant>,
}

impl HitFlash {
    /// The flash strength (0..1) at `now` given the trigger counter. The
    /// first reading only learns the counter: opening an editor is no hit.
    pub fn update(&mut self, counter: f32, now: Instant) -> f32 {
        if self.seen.is_some_and(|s| s != counter) {
            self.at = Some(now);
        }
        self.seen = Some(counter);
        self.at.map_or(0., |at| (1. - now.duration_since(at).as_secs_f32() / FLASH.as_secs_f32()).clamp(0., 1.))
    }
}

thread_local! {
    static FLASHES: RefCell<HashMap<DeviceId, HitFlash>> = RefCell::new(HashMap::new());
}

/// How tall a linear level is drawn (0..1): a square root, so quiet tails
/// still show.
fn level_frac(v: f32) -> f32 {
    (v.max(0.).sqrt() * REACH).min(1.)
}

fn gauge_frac(norm: f32) -> f32 {
    (1. - GAUGE_SPAN) / 2. + GAUGE_SPAN * (1. - norm.clamp(0., 1.))
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let live = dev.meter.len() >= METER_LEN;
    let slice = |from: usize| if live { dev.meter[from..from + BUCKETS].to_vec() } else { Vec::new() };
    let (boom, key) = (slice(BOOM_HIST), slice(KEY_HIST));
    let flash = if live { FLASHES.with(|f| f.borrow_mut().entry(dev.id).or_default().update(dev.m(TRIGGERS), Instant::now())) } else { 0. };
    let boom_on = dev.on(p::BOOM_ON);
    let enabled = dev.enabled && boom_on;
    let freq = dev.plain(p::BOOM_FREQ);
    let latency = if live && dev.m(LATENCY) > 0. { format!(" · latency {:.0} smp", dev.m(LATENCY)) } else { String::new() };
    let title = format!("boom {} ({}) · decay {}{latency}", dev.text(p::BOOM_FREQ), note_name(nearest_note(freq)), dev.text(p::BOOM_DECAY));
    let amount = dev.norm(p::BOOM_AMOUNT);
    let default = dev.info(p::BOOM_AMOUNT).map_or(0., |i| i.default_normalized());
    let handle = Handle { fx: 0.97, fy: gauge_frac(amount), y: ax(p::BOOM_AMOUNT, -1. / GAUGE_SPAN), index: 0, reset: vec![(p::BOOM_AMOUNT, default)], ..Default::default() };
    // The ruler along the bottom: slide the marker to tune Boom; double-click resets.
    let freq_default = dev.info(p::BOOM_FREQ).map_or(0.5, |i| i.default_normalized());
    let ruler = Handle {
        fx: RULER_L + (RULER_R - RULER_L) * boom_pos(freq),
        fy: RULER_Y,
        x: ax(p::BOOM_FREQ, 1. / (RULER_R - RULER_L)),
        index: 1,
        reset: vec![(p::BOOM_FREQ, freq_default)],
        ..Default::default()
    };

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles: vec![handle, ruler], keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        // 160 ms marks and quarter levels.
        for q in 1..4 {
            kit::vline(window, r, r.px(q as f32 / 4.), pal.grid_beat);
            kit::hline(window, r, r.py(q as f32 / 4.), pal.grid_beat);
        }
        let curve = |data: &[f32]| -> Vec<gpui::Point<gpui::Pixels>> {
            data.iter().enumerate().map(|(i, v)| r.pt(i as f32 / (BUCKETS - 1) as f32, 1. - level_frac(*v))).collect()
        };
        if key.len() == BUCKETS {
            let low = curve(&key);
            kit::fill_to(window, &low, r.bottom(), content(0.14));
            kit::line(window, &low, 1., content(0.4));
            let add = curve(&boom);
            kit::fill_to(window, &add, r.bottom(), with_alpha(colour, 0.22));
            kit::line(window, &add, 2., colour);
        } else {
            kit::txt(window, cx, "play the bus to see the boom", r.px(0.5), r.py(0.5), TEXT_SM, pal.faint, TextAlign::Center);
        }
        if flash > 0. {
            kit::bar(window, r.px(0.88), r.y, r.w * 0.12, r.h, 0., with_alpha(pal.accent, 0.22 * flash));
            kit::dot(window, r.pt(1., 0.5), 4. + 6. * flash, with_alpha(pal.text, 0.8 * flash), None);
        }
        // The Boom amount: a bar on the right edge.
        let (top, bottom) = (r.py(gauge_frac(1.)), r.py(gauge_frac(0.)));
        kit::bar(window, r.px(0.97) - 2., top, 4., bottom - top, 2., content(0.08));
        let now = r.py(gauge_frac(amount));
        kit::bar(window, r.px(0.97) - 2., now, 4., bottom - now, 2., with_alpha(pal.accent, 0.7));
        kit::dot(window, r.pt(0.97, gauge_frac(amount)), 5.5, pal.accent, Some(pal.text));
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_SM, pal.faint, TextAlign::Left);
        paint_ruler(r, freq, colour, window, cx);
    });

    // The mini displays take height the compact editor does not have.
    let minis = dev.big;
    let live_peak = if live { dev.m(1) } else { -120. };
    let amount_of = |id: u32| dev.plain(id) / 100.;

    // Drive.
    let (drive_type, drive_amount) = (dev.choice(p::DRIVE_TYPE), amount_of(p::DRIVE));
    let drive_mini = mini(move |r, window, _| {
        let pal = p();
        kit::line(window, &[r.pt(0., 1.), r.pt(1., 0.)], 1., content(0.14));
        let pts = kit::sample_curve(r, 48, |fx| 1. - drive_out(drive_type, drive_amount, fx) / 1.2);
        kit::fill_to(window, &pts, r.bottom(), accent_alpha(0.12));
        kit::line(window, &pts, 1.5, pal.accent);
        if live_peak > -80. {
            let x = 10f32.powf(live_peak / 20.).min(1.);
            kit::dot(window, r.pt(x, (1. - drive_out(drive_type, drive_amount, x) / 1.2).clamp(0., 1.)), 3., pal.text, None);
        }
    });
    // Crunch: how much grit is asked for.
    let crunch = amount_of(p::CRUNCH);
    let crunch_mini = mini(move |r, window, cx| {
        let pal = p();
        kit::bar(window, r.x + 4., r.py(0.62), r.w - 8., 6., 3., content(0.08));
        kit::bar(window, r.x + 4., r.py(0.62), (r.w - 8.) * crunch, 6., 3., with_alpha(pal.accent, 0.8));
        kit::txt(window, cx, &format!("{:.0}%", crunch * 100.), r.x + 4., r.y + 3., TEXT_SM, pal.muted, TextAlign::Left);
    });
    // Compress: the reduction now, from the right, and the threshold it works at.
    let gr_now = if live { dev.m(GR) } else { 0. };
    let thr = -2. - 34. * amount_of(p::COMPRESS);
    let comp_mini = mini(move |r, window, cx| {
        let pal = p();
        kit::bar(window, r.x + 4., r.py(0.62), r.w - 8., 6., 3., content(0.08));
        let w = (r.w - 8.) * (-gr_now / COMP_GR_SPAN).clamp(0., 1.);
        kit::bar(window, r.right() - 4. - w, r.py(0.62), w, 6., 3., pal.record);
        kit::txt(window, cx, &format!("{gr_now:.1} dB"), r.x + 4., r.y + 3., TEXT_SM, if gr_now < -0.05 { pal.text } else { pal.faint }, TextAlign::Left);
        // The threshold only when there is room beside the reduction.
        if r.w > 150. {
            kit::txt(window, cx, &format!("thr {thr:.0}"), r.right() - 4., r.y + 3., TEXT_SM, pal.faint, TextAlign::Right);
        }
    });
    // Transients: the gain applied now, around the middle.
    let trans_now = if live { dev.m(TRANS_DB) } else { 0. };
    let trans_mini = mini(move |r, window, cx| {
        let pal = p();
        let mid = r.x + r.w / 2.;
        kit::bar(window, r.x + 4., r.py(0.62), r.w - 8., 6., 3., content(0.08));
        let w = (trans_now / TRANS_SPAN).clamp(-1., 1.) * (r.w - 8.) / 2.;
        kit::bar(window, mid.min(mid + w), r.py(0.62), w.abs(), 6., 3., if trans_now >= 0. { pal.play } else { pal.record });
        kit::bar(window, mid, r.py(0.62) - 2., 1., 10., 0., pal.grid_bar);
        kit::txt(window, cx, &format!("{trans_now:+.1} dB"), mid, r.y + 3., TEXT_SM, if trans_now.abs() > 0.05 { pal.text } else { pal.faint }, TextAlign::Center);
    });
    // Boom: its note, and the envelope its Decay makes over 800 ms.
    let decay = dev.plain(p::BOOM_DECAY);
    let boom_mini = mini(move |r, window, cx| {
        let pal = p();
        let pts = kit::sample_curve(r, 48, |fx| 1. - 0.9 * (-(fx * 800.) / decay).exp());
        kit::fill_to(window, &pts, r.bottom(), accent_alpha(0.12));
        kit::line(window, &pts, 1.5, pal.accent);
        kit::txt(window, cx, &format!("{} · {}", note_name(nearest_note(freq)), widgets_hz(freq)), r.right() - 4., r.y + 3., TEXT_SM, pal.muted, TextAlign::Right);
    });
    // Damp: the low-pass's response, 20 Hz to 20 kHz.
    let damp_hz = dev.plain(p::DAMP);
    let damp_mini = mini(move |r, window, _| {
        let pal = p();
        let pts = kit::sample_curve(r, 48, |fx| 0.15 - damp_db(damp_hz, kit::frac_freq(fx)) / 30.);
        kit::fill_to(window, &pts, r.bottom(), accent_alpha(0.12));
        kit::line(window, &pts, 1.5, pal.accent);
        kit::vline(window, r, r.px(kit::freq_frac(damp_hz).clamp(0., 1.)), with_alpha(pal.accent2, 0.7));
    });

    let snap = {
        let device = dev.id;
        let target = dev.to_norm(p::BOOM_FREQ, snap_to_note(freq));
        let on_note = (snap_to_note(freq) - freq).abs() < 0.05;
        div()
            .id(("snap-boom", dev.key(0)))
            .flex_none()
            .h(px(16.))
            .px(px(SP1 + 2.))
            .flex()
            .items_center()
            .rounded(px(r_sm()))
            .cursor_pointer()
            .bg(if on_note { accent_alpha(0.2) } else { content(0.06) })
            .hover(|s| s.bg(content(0.12)))
            .tooltip(widgets::tip("Force To Note: tune the Boom to the nearest note", None))
            .child(label("To note", TEXT_SM, if on_note { p().accent } else { p().text }).font_weight(FontWeight::MEDIUM))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                set_now(this, device, p::BOOM_FREQ, target);
                cx.notify();
            }))
            .into_any_element()
    };

    let out = vec![ctl(ws, dev, p::TRIM, cx), ctl(ws, dev, p::MIX, cx), ctl(ws, dev, p::OUTPUT, cx)];
    let drive = vec![ctl(ws, dev, p::DRIVE, cx), kit::pick(dev, p::DRIVE_TYPE, cx)];
    let crunch_ctl = vec![ctl(ws, dev, p::CRUNCH, cx)];
    let comp_ctl = vec![ctl(ws, dev, p::COMPRESS, cx)];
    let trans_ctl = vec![ctl(ws, dev, p::TRANSIENTS, cx)];
    let boom_controls = vec![ctl(ws, dev, p::BOOM_AMOUNT, cx), ctl(ws, dev, p::BOOM_FREQ, cx), ctl(ws, dev, p::BOOM_DECAY, cx)];
    let damp_ctl = vec![ctl(ws, dev, p::DAMP, cx)];

    let stages = (if win::in_window(dev) { kit::flow() } else { div().flex_none().flex().flex_wrap().gap(px(SP1)) })
        .child(stage(dev, "DRIVE", Some(p::DRIVE_ON), None, minis.then_some(drive_mini), drive, cx))
        .child(stage(dev, "CRUNCH", Some(p::CRUNCH_ON), None, minis.then_some(crunch_mini), crunch_ctl, cx))
        .child(stage(dev, "COMP", Some(p::COMP_ON), None, minis.then_some(comp_mini), comp_ctl, cx))
        .child(stage(dev, "TRANS", Some(p::TRANS_ON), None, minis.then_some(trans_mini), trans_ctl, cx))
        .child(stage(dev, "BOOM", Some(p::BOOM_ON), Some(snap), minis.then_some(boom_mini), boom_controls, cx))
        .child(stage(dev, "DAMP", Some(p::DAMP_ON), None, minis.then_some(damp_mini), damp_ctl, cx))
        .child(stage(dev, "OUTPUT", None, None, None, out, cx));

    // The card is as wide as its stages need; the window takes whatever it has
    // and the stages wrap (the Boom view above keeps at least 160 px).
    let windowed = win::in_window(dev);
    kit::panel(if windowed { 720. } else { 1060. })
        .child((if windowed { kit::display(160.) } else { div().flex_1().min_h(px(0.)) }).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(plot))
        .child(stages)
        .into_any_element()
}

/// A stage's control: a knob in a device window, a compact number in the card.
fn ctl(ws: &mut Workspace, dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    if win::in_window(dev) { win::knob(dev, id, cx) } else { kit::num(ws, dev, id, cx) }
}

/// "55 Hz".
fn widgets_hz(hz: f32) -> String {
    format!("{hz:.0} Hz")
}

/// A stage's mini display: a strip as wide as the stage's controls.
fn mini(paint: impl Fn(Rect, &mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .flex_none()
        .h(px(MINI_H))
        .rounded(px(r_sm()))
        .bg(p().well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .child(gpui::canvas(|_, _, _| {}, move |b, _, window, cx| paint(Rect::of(b), window, cx)).size_full())
        .into_any_element()
}

/// One stage of the chain: its title and switch (and an extra control beside
/// them), its mini display, then its controls.
fn stage(dev: &Dev, title: &str, switch: Option<u32>, extra: Option<AnyElement>, display: Option<AnyElement>, body: Vec<AnyElement>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let on = switch.is_none_or(|id| dev.on(id));
    let header = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(label(title.to_owned(), TEXT_SM, if on { pal.text } else { pal.faint }).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1().min_w(px(SP1)))
        .children(extra)
        .when_some(switch, |d, id| {
            d.child(
                widgets::switch(("stage-on", dev.key(id as usize)), on)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        set_now(this, device, id, if on { 0. } else { 1. });
                        cx.notify();
                    })),
            )
        });
    // In a window the stages share each line's width (see `kit::flow`).
    let card = div().p(px(SP2)).gap(px(SP1)).flex().flex_col().rounded(px(r_md())).bg(content(0.03)).border_1().border_color(if on { border_strong() } else { border() });
    let card = if win::in_window(dev) { kit::grow(card) } else { card.flex_none() };
    card
        .child(header)
        .children(display)
        .child(div().flex().items_center().justify_around().gap(px(SP1)).when(!on, |d| d.opacity(0.45)).children(body))
}

/// The keyboard ruler along the view's bottom: a tick per semitone between
/// 30 and 120 Hz (taller for the naturals), the octaves named, the Boom's
/// frequency as a marker with its note.
fn paint_ruler(r: Rect, freq: f32, colour: gpui::Hsla, window: &mut Window, cx: &mut App) {
    let pal = p();
    let (x0, x1) = (r.px(RULER_L), r.px(RULER_R));
    let y = r.py(RULER_Y);
    kit::bar(window, x0, y, x1 - x0, 1., 0., pal.grid_bar);
    let mut m = nearest_note(BOOM_LO) - 1;
    while note_hz(m) <= BOOM_HI * 1.001 {
        let hz = note_hz(m);
        if hz >= BOOM_LO * 0.999 {
            let x = x0 + (x1 - x0) * boom_pos(hz);
            let sharp = NOTE_NAMES[m.rem_euclid(12) as usize].len() > 1;
            kit::bar(window, x, y - if sharp { 5. } else { 8. }, 1., if sharp { 5. } else { 8. }, 0., content(if sharp { 0.2 } else { 0.4 }));
            if m.rem_euclid(12) == 0 {
                kit::txt(window, cx, &note_name(m), x, y + 2., TEXT_SM, pal.faint, TextAlign::Center);
            }
        }
        m += 1;
    }
    let x = x0 + (x1 - x0) * boom_pos(freq);
    kit::dot(window, point(px(x), px(y)), 5., colour, Some(pal.text));
    kit::txt(window, cx, &note_name(nearest_note(freq)), x + 9., y + 2., TEXT_SM, pal.text, TextAlign::Left);
}


#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn a_hit_flashes_and_fades() {
        let t0 = Instant::now();
        let mut f = HitFlash::default();
        assert_eq!(f.update(3., t0), 0., "opening the editor is no hit");
        assert_eq!(f.update(3., t0 + Duration::from_millis(10)), 0.);
        assert!(f.update(4., t0 + Duration::from_millis(20)) > 0.99, "a new count is a hit");
        let half = f.update(4., t0 + Duration::from_millis(20) + FLASH / 2);
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert_eq!(f.update(4., t0 + Duration::from_millis(20) + FLASH * 2), 0., "gone");
        // The counter wraps: a drop is a hit too.
        assert!(f.update(0., t0 + Duration::from_secs(5)) > 0.99);
    }

    #[test]
    fn notes_and_the_boom_range() {
        assert_eq!((note_name(33), note_name(69), note_name(60), note_name(22)), ("A1".to_owned(), "A4".to_owned(), "C4".to_owned(), "A#0".to_owned()));
        assert!((note_hz(33) - 55.).abs() < 1e-3);
        assert_eq!(nearest_note(56.), 33);
        // Force To Note lands on a note inside the 30..120 Hz range.
        assert!((snap_to_note(56.) - 55.).abs() < 1e-3);
        assert!((snap_to_note(30.) - note_hz(23)).abs() < 1e-3, "{}", snap_to_note(30.));
        for hz in [30., 41., 63.3, 99., 120.] {
            let s = snap_to_note(hz);
            assert!((BOOM_LO..=BOOM_HI).contains(&s), "{hz} -> {s}");
            assert_eq!(snap_to_note(s), s, "idempotent");
        }
        // The ruler is the parameter's own log axis, so the marker follows the Freq value.
        let d = dev("fx.drumbus", &[]);
        let info = d.info(p::BOOM_FREQ).unwrap();
        assert_eq!((info.min, info.max), (BOOM_LO, BOOM_HI));
        for hz in [30., 55., 90., 120.] {
            assert!((info.to_normalized(hz) - boom_pos(hz)).abs() < 1e-4, "{hz}");
        }
    }

    #[test]
    fn the_mini_displays_follow_the_stages() {
        // Drive keeps the reference level where it was, whatever the amount and type.
        for choice in 0..3 {
            for amount in [0.2, 0.6, 1.] {
                assert!((drive_out(choice, amount, DRIVE_REF) - DRIVE_REF).abs() < 1e-3, "{choice} {amount}");
            }
        }
        // More drive bends the curve: the same quiet input comes out louder.
        assert!(drive_out(1, 1., 0.1) > drive_out(1, 0.2, 0.1));
        // Damp is -3 dB at the cutoff and flat well below it.
        assert!((damp_db(2_000., 2_000.) + 3.01).abs() < 0.01);
        assert!(damp_db(2_000., 100.).abs() < 0.01 && damp_db(2_000., 8_000.) < -20.);
    }

    #[test]
    fn levels_are_drawn_on_a_root_scale_inside_the_view() {
        assert_eq!(level_frac(0.), 0.);
        assert!(level_frac(0.01) > 0.05, "quiet tails show");
        assert_eq!(level_frac(4.), 1., "clamped");
        assert!(gauge_frac(0.) > gauge_frac(1.) && gauge_frac(1.) > 0. && gauge_frac(0.) < 1.);
    }

    #[test]
    fn the_editor_asks_for_the_params_the_plugin_has() {
        let d = dev("fx.drumbus", &[(p::BOOM_FREQ, 45.)]);
        assert_eq!(d.plain(p::BOOM_FREQ), 45.);
        for id in [p::TRIM, p::DRIVE, p::DRIVE_TYPE, p::CRUNCH, p::DAMP, p::TRANSIENTS, p::BOOM_AMOUNT, p::BOOM_FREQ, p::BOOM_DECAY, p::COMPRESS, p::MIX, p::OUTPUT, p::DRIVE_ON, p::CRUNCH_ON, p::COMP_ON, p::TRANS_ON, p::BOOM_ON, p::DAMP_ON] {
            assert!(d.info(id).is_some(), "{id}");
        }
        // The meter offsets are the plugin's layout: header, boom, key.
        assert_eq!((BOOM_HIST, KEY_HIST, METER_LEN), (7, 7 + BUCKETS, 7 + 2 * BUCKETS));
    }
}
