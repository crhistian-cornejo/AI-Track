//! `instrument.kick808`: 808 amplitude & pitch-sweep contour visualizer with
//! draggable Decay / Sweep handles + chromatic & drive strips.

use super::instr;
use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::ParamInfo;
use aitrack_stock::instrument::kick808::p;
use gpui::{AnyElement, Context, Div, IntoElement, ParentElement, Styled, TextAlign, div, prelude::FluentBuilder, px};

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    if kit::in_window() {
        return window(ws, dev, cx);
    }
    let pal = p();
    let w = if dev.big { 420. } else { 312. };
    let tune = dev.plain(p::TUNE);
    let sweep = dev.plain(p::SWEEP);
    let sweep_ms = dev.plain(p::SWEEP_DECAY);
    let decay_ms = dev.plain(p::DECAY);
    let drive = dev.plain(p::DRIVE) / 100.;
    let active = dev.m(0) > 0.5;
    let live_env = dev.m(3).clamp(0., 1.);
    let enabled = dev.enabled;
    let color = if enabled { pal.accent } else { pal.faint };

    // Time window matches up to 2.5s or current decay * 1.4.
    let window_ms = (decay_ms * 1.35).clamp(250., 3500.);
    let fx = (decay_ms / window_ms).clamp(0.08, 0.92);
    let fy = (1.0 - sweep / 36.0).clamp(0.08, 0.92);
    let reset = [p::DECAY, p::SWEEP]
        .map(|id| (id, dev.info(id).map_or(0.5, |i| i.default_normalized())))
        .to_vec();
    let handles = vec![Handle { fx, fy, x: ax(p::DECAY, 1.), y: ax(p::SWEEP, -1.), alt: ax(p::DRIVE, -1.), index: 0, reset }];

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r: Rect, window, cx| {
        let pal = p();
        // Horizontal reference lines.
        kit::hline(window, r, r.py(0.5), border());
        kit::hline(window, r, r.py(0.85), with_alpha(pal.faint, 0.25));

        let n = ((r.w / 3.) as usize).clamp(48, 220);
        // Amplitude envelope + warm filled body.
        let amp_pts = kit::sample_curve(r, n, |t| {
            let ms = t * window_ms;
            let e = (-ms / decay_ms.max(10.)).exp();
            1.0 - e * 0.88
        });
        kit::fill_to(window, &amp_pts, r.bottom(), with_alpha(color, 0.14 + drive * 0.12 + if active { live_env * 0.1 } else { 0. }));
        kit::line(window, &amp_pts, 2., color);

        // Pitch sweep contour overlay.
        let pitch_pts = kit::sample_curve(r, n, |t| {
            let ms = t * window_ms;
            let p_env = (-ms / sweep_ms.max(2.)).exp();
            let norm = (sweep / 36.0) * p_env;
            (0.88 - norm * 0.78).clamp(0.06, 0.92)
        });
        kit::line(window, &pitch_pts, 1.5, pal.accent2);

        kit::dot(window, r.pt(fx, fy), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
        kit::txt(window, cx, &format!("{decay_ms:.0}ms"), r.px(fx) + 8., r.py(fy) - 10., TEXT_XS, color, TextAlign::Left);
    });

    let primary = [p::TUNE, p::SWEEP, p::SWEEP_DECAY, p::DECAY, p::CLICK, p::DRIVE, p::TONE, p::KEY_TRACK];
    let more = [p::GLIDE, p::HOLD, p::VOLUME];

    kit::panel(w)
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .child(caption("808 BODY & PITCH SWEEP"))
                .child(div().flex().gap(px(SP1)).child(kit::readout("ROOT", format!("{tune:.0} Hz"), color)).when(active, |d| {
                    d.child(kit::readout("ENV", format!("{:.0}%", live_env * 100.), pal.accent2))
                })),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(74.))
                .rounded(px(r_sm()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .overflow_hidden()
                .child(plot),
        )
        .child(kit::strip(ws, dev, &primary, &more, cx))
        .into_any_element()
}

// ───────────────────────────── Window ─────────────────────────────

/// The time axis both lanes share, logarithmic: 2 ms … 4 s. A log axis puts
/// the sweep (4 – 180 ms) and the decay (60 ms – 4 s) on one plot, and a
/// handle's sideways drag maps to its parameter exactly (see [`log_per_frac`]).
const T_LO: f32 = 2.;
const T_HI: f32 = 4_000.;
/// The pitch axis, logarithmic too: 20 Hz … 1 kHz, high at the top.
const F_LO: f32 = 20.;
const F_HI: f32 = 1_000.;
/// Where the tuning handle sits on the pitch lane (the sweep is long over).
const TUNE_AT: f32 = 0.94;

/// Horizontal fraction of `ms` on the time axis.
pub fn time_frac(ms: f32) -> f32 {
    ((ms.max(T_LO) / T_LO).ln() / (T_HI / T_LO).ln()).clamp(0., 1.)
}

/// Milliseconds at a horizontal fraction of the time axis.
pub fn frac_time(f: f32) -> f32 {
    T_LO * (T_HI / T_LO).powf(f)
}

/// Vertical fraction of `hz` on the pitch lane (0 = top).
pub fn pitch_frac(hz: f32) -> f32 {
    (1. - (hz.max(F_LO) / F_LO).ln() / (F_HI / F_LO).ln()).clamp(0., 1.)
}

/// Vertical fraction of an amplitude (0..1) on the level lane.
pub fn level_frac(level: f32) -> f32 {
    1. - (0.06 + 0.88 * level.clamp(0., 1.))
}

/// The pitch `ms` after the hit: the sweep falls exponentially onto the tuning.
pub fn pitch_at(tune: f32, sweep_st: f32, sweep_ms: f32, ms: f32) -> f32 {
    tune * 2f32.powf(sweep_st * (-ms / sweep_ms.max(1.)).exp() / 12.)
}

/// How far a log-scaled parameter's normalised value moves when its handle
/// crosses the whole of an axis spanning `lo..hi`.
pub fn log_per_frac(info: &ParamInfo, lo: f32, hi: f32) -> f32 {
    (hi / lo).ln() / (info.max / info.min).ln()
}

/// The sweep handle's vertical axis: it sits where the sweep is `1/e` of its
/// depth, so a move of `dy` of the lane changes the depth by `dy` scaled by that.
pub fn sweep_per_frac(info: &ParamInfo) -> f32 {
    -12. * (F_HI / F_LO).ln() * std::f32::consts::E / (std::f32::consts::LN_2 * (info.max - info.min))
}

/// The time grid: (ms, label, major).
const TICKS: [(f32, &str, bool); 9] =
    [(5., "", false), (10., "10 ms", true), (20., "", false), (50., "", false), (100., "100 ms", true), (200., "", false), (500., "", false), (1_000., "1 s", true), (2_000., "", false)];

fn window(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (tune, sweep, sweep_ms, decay_ms) = (dev.plain(p::TUNE), dev.plain(p::SWEEP), dev.plain(p::SWEEP_DECAY), dev.plain(p::DECAY));
    let drive = dev.plain(p::DRIVE) / 100.;
    let (active, live_env, enabled) = (dev.m(0) > 0.5, dev.m(3).clamp(0., 1.), dev.enabled);
    let color = if enabled { pal.accent } else { pal.faint };
    let pitch_color = if enabled { pal.accent2 } else { pal.faint };
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let per = |id: u32| dev.info(id).map_or(1., |i| log_per_frac(i, T_LO, T_HI));
    let start = pitch_at(tune, sweep, sweep_ms, 0.);
    let e = std::f32::consts::E;

    // Pitch lane: the sweep's time and depth, and the tuning.
    let sweep_handle = Handle {
        fx: time_frac(sweep_ms),
        fy: pitch_frac(pitch_at(tune, sweep, sweep_ms, sweep_ms)),
        x: ax(p::SWEEP_DECAY, per(p::SWEEP_DECAY)),
        y: ax(p::SWEEP, dev.info(p::SWEEP).map_or(-1., sweep_per_frac)),
        index: 0,
        reset: vec![default(p::SWEEP_DECAY), default(p::SWEEP)],
        ..Default::default()
    };
    let tune_handle = Handle {
        fx: TUNE_AT,
        fy: pitch_frac(tune),
        y: ax(p::TUNE, dev.info(p::TUNE).map_or(-1., |i| -(F_HI / F_LO).ln() / (i.max / i.min).ln())),
        index: 1,
        reset: vec![default(p::TUNE)],
        ..Default::default()
    };
    let pitch_dots = [(sweep_handle.fx, sweep_handle.fy), (tune_handle.fx, tune_handle.fy)];
    let pitch_lane = kit::plot(ws, dev, PlotCfg { slot: 0, handles: vec![sweep_handle, tune_handle], keys: true, ..Default::default() }, cx, move |r: Rect, window, cx| {
        let pal = p();
        for (ms, _, major) in TICKS {
            kit::vline(window, r, r.px(time_frac(ms)), if major { pal.grid_bar } else { pal.grid_beat });
        }
        for (hz, text) in [(25., "25"), (50., "50"), (100., "100 Hz"), (200., "200"), (400., "400"), (800., "800")] {
            let y = r.py(pitch_frac(hz));
            kit::hline(window, r, y, if hz == 100. { pal.grid_bar } else { pal.grid_beat });
            kit::txt(window, cx, text, r.right() - 8., y + 2., TEXT_XS, pal.faint, TextAlign::Right);
        }
        let n = ((r.w / 2.) as usize).clamp(64, 480);
        let pts = kit::sample_curve(r, n, |t| pitch_frac(pitch_at(tune, sweep, sweep_ms, frac_time(t))));
        kit::fill_to(window, &pts, r.bottom(), with_alpha(pitch_color, 0.1));
        kit::line(window, &pts, 2., pitch_color);
        kit::txt(window, cx, "PITCH", r.x + 8., r.bottom() - 15., TEXT_XS, pal.faint, TextAlign::Left);
        let ys = r.py(pitch_frac(start));
        kit::txt(window, cx, &format!("{start:.0} Hz"), r.x + 6., (ys - 26.).max(r.y + 4.), TEXT_XS, pitch_color, TextAlign::Left);
        let [(bx, by), (tx, ty)] = pitch_dots;
        kit::dot(window, r.pt(bx, by), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
        kit::txt(window, cx, &format!("{sweep_ms:.0} ms · {sweep:.0} st"), r.px(bx) + 10., r.py(by) - 8., TEXT_XS, pitch_color, TextAlign::Left);
        kit::dot(window, r.pt(tx, ty), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
        kit::txt(window, cx, &format!("{tune:.0} Hz"), r.px(tx), r.py(ty) - 22., TEXT_XS, pitch_color, TextAlign::Center);
    });

    // Level lane: the decay, on the same time axis.
    let decay_handle = Handle {
        fx: time_frac(decay_ms),
        fy: level_frac(1. / e),
        x: ax(p::DECAY, per(p::DECAY)),
        index: 2,
        reset: vec![default(p::DECAY)],
        ..Default::default()
    };
    let (dx, dy) = (decay_handle.fx, decay_handle.fy);
    let level_lane = kit::plot(ws, dev, PlotCfg { slot: 1, handles: vec![decay_handle], keys: true, ..Default::default() }, cx, move |r: Rect, window, cx| {
        let pal = p();
        for (ms, text, major) in TICKS {
            let x = r.px(time_frac(ms));
            kit::vline(window, r, x, if major { pal.grid_bar } else { pal.grid_beat });
            if !text.is_empty() {
                kit::txt(window, cx, text, x + 4., r.bottom() - 14., TEXT_XS, pal.faint, TextAlign::Left);
            }
        }
        kit::hline(window, r, r.py(level_frac(0.5)), pal.grid_beat);
        let n = ((r.w / 2.) as usize).clamp(64, 480);
        let pts = kit::sample_curve(r, n, |t| level_frac((-frac_time(t) / decay_ms.max(10.)).exp()));
        kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.14 + drive * 0.12 + if active { live_env * 0.1 } else { 0. }));
        kit::line(window, &pts, 2., color);
        if active {
            kit::hline(window, r, r.py(level_frac(live_env)), with_alpha(pal.accent2, 0.6));
        }
        kit::txt(window, cx, "LEVEL", r.right() - 8., r.y + 6., TEXT_XS, pal.faint, TextAlign::Right);
        kit::dot(window, r.pt(dx, dy), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
        kit::txt(window, cx, &format!("{decay_ms:.0} ms"), r.px(dx) + 10., r.py(dy) - 8., TEXT_XS, color, TextAlign::Left);
    });

    let grow = |mut lane: Div, factor: f32| {
        lane.style().flex_grow = Some(factor);
        lane
    };
    let lanes = div()
        .flex_1()
        .min_h(px(240.))
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(grow(instr::stage(96.), 3.).child(pitch_lane))
        .child(grow(instr::stage(64.), 2.).child(level_lane));

    let body = [
        instr::group("PITCH", pitch_color, instr::dials(dev, &[p::TUNE, p::SWEEP, p::SWEEP_DECAY], cx)),
        instr::group("BODY", color, instr::dials(dev, &[p::DECAY, p::HOLD], cx)),
        instr::group("PUNCH", color, instr::dials(dev, &[p::CLICK, p::DRIVE, p::TONE], cx)),
        instr::group("PLAY", pal.muted, instr::dials(dev, &[p::KEY_TRACK, p::GLIDE, p::VOLUME], cx)),
    ];
    instr::root(720.)
        .child(instr::title(
            "808 KICK · PITCH SWEEP AND BODY",
            vec![
                instr::tag("START", format!("{start:.0} Hz"), pitch_color, false),
                instr::tag("ROOT", format!("{tune:.0} Hz"), color, false),
                instr::tag("DECAY", format!("{decay_ms:.0} ms"), color, false),
                instr::tag("ENV", format!("{:.0}%", live_env * 100.), pal.accent2, active),
            ],
        ))
        .child(lanes)
        .child(instr::groups(body.into()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn kick808_editor_reads_fundamental_and_sweep() {
        let d = dev("instrument.kick808", &[(p::TUNE, 55.), (p::SWEEP, 24.)]);
        assert!((d.plain(p::TUNE) - 55.).abs() < 1.0);
        assert!((d.plain(p::SWEEP) - 24.).abs() < 0.1);
    }
    #[test]
    fn the_axes_round_trip() {
        for ms in [2., 10., 26., 680., 4_000.] {
            assert!((frac_time(time_frac(ms)) - ms).abs() / ms < 1e-3, "{ms}");
        }
        assert!(pitch_frac(1_000.) < 1e-3 && pitch_frac(20.) > 0.999, "high pitches are at the top");
        assert!(level_frac(1.) < level_frac(0.5) && level_frac(0.5) < level_frac(0.));
    }

    #[test]
    fn the_pitch_falls_from_the_sweep_onto_the_tuning() {
        let (tune, sweep) = (50., 12.);
        assert!((pitch_at(tune, sweep, 30., 0.) - 100.).abs() < 0.01, "a 12 st sweep starts an octave up");
        assert!((pitch_at(tune, sweep, 30., 3_000.) - tune).abs() < 0.01);
        assert!((pitch_at(tune, 0., 30., 0.) - tune).abs() < 1e-3);
    }

    /// A drag of a handle by some distance must move the handle by that
    /// distance: the axis specs are the inverse of the drawing.
    #[test]
    fn handle_axes_match_the_drawing() {
        let d = dev("instrument.kick808", &[]);
        let at = |id: u32, plain: f32| d.info(id).unwrap().to_normalized(plain);
        let per = |id: u32| log_per_frac(d.info(id).unwrap(), T_LO, T_HI);
        // Decay: x of the handle against the parameter.
        let (a, b) = (at(p::DECAY, 300.), at(p::DECAY, 360.));
        let dfx = time_frac(360.) - time_frac(300.);
        assert!(((b - a) / dfx - per(p::DECAY)).abs() / per(p::DECAY) < 0.02);
        // Sweep time likewise.
        let (a, b) = (at(p::SWEEP_DECAY, 20.), at(p::SWEEP_DECAY, 24.));
        let dfx = time_frac(24.) - time_frac(20.);
        assert!(((b - a) / dfx - per(p::SWEEP_DECAY)).abs() / per(p::SWEEP_DECAY) < 0.02);
        // Tuning: y on the pitch lane.
        let (a, b) = (at(p::TUNE, 45.), at(p::TUNE, 50.));
        let dfy = pitch_frac(50.) - pitch_frac(45.);
        let want = -(F_HI / F_LO).ln() / (110f32 / 24.).ln();
        assert!(((b - a) / dfy - want).abs() / want.abs() < 0.02);
        // Sweep depth: y of the handle that sits 1/e down the sweep.
        let (tune, ms) = (48., 26.);
        let y = |sweep: f32| pitch_frac(pitch_at(tune, sweep, ms, ms));
        let (a, b) = (at(p::SWEEP, 16.), at(p::SWEEP, 17.));
        let want = sweep_per_frac(d.info(p::SWEEP).unwrap());
        assert!(((b - a) / (y(17.) - y(16.)) - want).abs() / want.abs() < 0.03);
    }
}
