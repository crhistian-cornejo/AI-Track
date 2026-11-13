//! `fx.tape`: the magnetic hysteresis loop at the current input level
//! (drag its handle sideways = drive, up and down = saturation) next to the
//! playback response — head bump and HF loss over frequency, with the bump
//! (height = amount) and the loss corner (sideways = HF loss) draggable.

use super::super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::db_to_gain;
use aitrack_stock::fx::saturator::tape::{H_SCALE, Ja, SPEED_IPS, bump_freq, loop_points, loss_freq, p, playback_db};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Loop plot range: ±`H_RANGE` of field, ±1.1 of M / Ms.
const H_RANGE: f32 = 6.;

/// Response plot range (dB) around 0.
const RESP_DB: f32 = 18.;

/// `(x, y)` plot fractions of a loop point.
pub fn loop_frac(h: f32, m: f32) -> (f32, f32) {
    (((h / H_RANGE) + 1.) / 2., 0.5 - m / 2.2)
}

/// Plot fraction of a response level.
pub fn resp_frac(db: f32) -> f32 {
    (0.5 - db / (2. * RESP_DB)).clamp(0., 1.)
}

/// Field amplitude the loop is drawn at: a −6 dBFS signal through Input.
pub fn loop_amp(input_db: f32) -> f64 {
    (H_SCALE * 0.5 * db_to_gain(input_db) as f64).min(H_RANGE as f64)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pct = |id| dev.plain(id) / 100.;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let ja = Ja::from_controls(pct(p::DRIVE) as f64, pct(p::SATURATION) as f64, pct(p::BIAS) as f64);
    let amp = loop_amp(dev.plain(p::INPUT));
    let pts = loop_points(ja, amp, 160);
    let enabled = dev.enabled;
    let level = dev.m(2).clamp(0., 1.2);
    // The handle: x = drive, y = saturation (top = more).
    let handles = vec![Handle {
        fx: 0.1 + 0.8 * pct(p::DRIVE),
        fy: 0.9 - 0.8 * pct(p::SATURATION),
        x: ax(p::DRIVE, 1.25),
        y: ax(p::SATURATION, -1.25),
        index: 0,
        reset: vec![default(p::DRIVE), default(p::SATURATION)],
        ..Default::default()
    }];
    let (hx, hy) = (handles[0].fx, handles[0].fy);
    let hyst = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        kit::vline(window, r, r.px(0.5), pal.grid_bar);
        for m in [-1f32, 1.] {
            kit::hline(window, r, r.py(loop_frac(0., m).1), with_alpha(pal.grid_bar, 0.6));
        }
        let color = if enabled { pal.accent } else { pal.muted };
        let curve: Vec<_> = pts.iter().map(|(h, m)| loop_frac(*h, *m)).map(|(x, y)| r.pt(x, y)).collect();
        kit::line(window, &curve, 2., color);
        // How far into the loop the signal goes now.
        let lv = r.py(loop_frac(0., level).1);
        kit::hline(window, r, lv, with_alpha(pal.accent2, 0.5));
        kit::txt(window, cx, "H", r.right() - 6., r.py(0.5) + 2., TEXT_XS, pal.faint, TextAlign::Right);
        kit::txt(window, cx, "M", r.px(0.5) + 4., r.y + 3., TEXT_XS, pal.faint, TextAlign::Left);
        kit::dot(window, r.pt(hx, hy), 6., with_alpha(color, 0.85), Some(pal.text));
    });

    // Playback response.
    let ips = SPEED_IPS[dev.choice(p::SPEED).min(3)];
    let (loss, bump) = (pct(p::HF_LOSS), pct(p::HEAD_BUMP));
    let sr = dev.sample_rate.max(44_100.);
    let fb = bump_freq(ips);
    let fl = loss_freq(ips, loss).min(20_000.);
    let handles = vec![
        Handle { fx: freq_frac(fb), fy: resp_frac(playback_db(ips, loss, bump, fb as f64, sr)), y: ax(p::HEAD_BUMP, -2.), index: 1, reset: vec![default(p::HEAD_BUMP)], ..Default::default() },
        Handle { fx: freq_frac(fl), fy: resp_frac(-3.), x: ax(p::HF_LOSS, -1.), index: 2, reset: vec![default(p::HF_LOSS)], ..Default::default() },
    ];
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let speed = dev.text(p::SPEED);
    let resp = kit::plot(ws, dev, PlotCfg { slot: 1, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::freq_grid(window, cx, r, true);
        kit::hline(window, r, r.py(resp_frac(0.)), pal.grid_bar);
        let color = if enabled { pal.accent } else { pal.muted };
        let curve = kit::sample_curve(r, 120, |fx| resp_frac(playback_db(ips, loss, bump, kit::frac_freq(fx) as f64, sr)));
        kit::fill_to(window, &curve, r.py(resp_frac(0.)), with_alpha(color, 0.1));
        kit::line(window, &curve, 2., color);
        for (i, (x, y)) in marks.iter().enumerate() {
            kit::dot(window, r.pt(*x, *y), 6., if i == 0 { series(1) } else { series(2) }, Some(pal.text));
        }
        kit::txt(window, cx, &speed, r.right() - 6., r.y + 4., TEXT_XS, pal.muted, TextAlign::Right);
    });

    let readouts = div()
        .flex_none()
        .flex()
        .gap(px(SP1))
        .child(kit::readout("IN", kit::fmt_db(20. * dev.m(0).max(1e-6).log10()), p().text))
        .child(kit::readout("OUT", kit::fmt_db(20. * dev.m(1).max(1e-6).log10()), p().text))
        .child(kit::readout("SAT", format!("{:.0}%", level * 100.), p().accent2));
    kit::panel(if dev.big { 820. } else { 560. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_none().w(px(if dev.big { 300. } else { 188. })).h_full().child(hyst))
                .child(div().flex_1().min_w(px(0.)).h_full().child(resp)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(caption("TAPE"))
                .child(kit::strip(ws, dev, &[p::INPUT, p::DRIVE, p::SATURATION, p::BIAS, p::SPEED], &[p::HF_LOSS, p::HEAD_BUMP], cx))
                .child(div().w(px(SP2)))
                .child(caption("TRANSPORT"))
                .child(kit::strip(ws, dev, &[p::WOW, p::FLUTTER, p::WIDTH, p::OUTPUT, p::MIX], &[p::OVERSAMPLING], cx))
                .child(readouts),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_fits_its_plot() {
        for input in [-24., 0., 24.] {
            let amp = loop_amp(input);
            for (h, m) in loop_points(Ja::from_controls(1., 1., 0.), amp, 64) {
                let (x, y) = loop_frac(h, m);
                assert!((0. ..=1.).contains(&x) && (0. ..=1.).contains(&y), "input {input}: ({h}, {m})");
            }
        }
        assert_eq!(resp_frac(0.), 0.5);
        assert_eq!(resp_frac(100.), 0.);
    }
}
