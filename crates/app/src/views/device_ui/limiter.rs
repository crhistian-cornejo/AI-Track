//! `fx.limiter`: the level going in as a scrolling, mirrored envelope, what
//! comes out inside it, the ceiling as a draggable line, gain reduction
//! hanging from the top, and true-peak / loudness read-outs. The strip adds
//! the character controls (style, attack, release, channel mode, auto gain,
//! dither, delta) and the plugin's own K-weighted loudness meters.

use super::dynamics::trace;
use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::limiter::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Linear amplitude at the top edge of the plot (+3 dB of headroom).
const TOP: f32 = 1.41;

/// Vertical fraction of a positive amplitude (the centre line is silence).
pub fn amp_frac(amp: f32) -> f32 {
    0.5 - 0.5 * (amp / TOP).clamp(0., 1.)
}

fn db_amp(db: f32) -> f32 {
    if db <= -100. { 0. } else { 10f32.powf(db / 20.) }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let ceiling = dev.plain(p::CEILING);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![
        Handle { fx: 0.94, fy: amp_frac(db_amp(ceiling)), y: ax(p::CEILING, -3.), index: 0, reset: vec![default(p::CEILING)], ..Default::default() },
        Handle { fx: 0.06, fy: 0.5, y: ax(p::GAIN, -1.2), index: 1, reset: vec![default(p::GAIN)], ..Default::default() },
    ];
    let hist = dev.hist.clone();
    let gain = dev.plain(p::GAIN);
    let enabled = dev.enabled;
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let h = hist.borrow();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        for db in [-12., -6., 0.] {
            let a = db_amp(db);
            kit::hline(window, r, r.py(amp_frac(a)), pal.grid_beat);
            kit::hline(window, r, r.py(1. - amp_frac(a)), pal.grid_beat);
            kit::txt(window, cx, &format!("{db:.0}"), r.x + 4., r.py(amp_frac(a)) - 12., TEXT_XS, pal.faint, TextAlign::Left);
        }
        // Input after gain: the outer envelope. Output: what is left inside.
        let upper = trace(r, &h.traces[1], |db| amp_frac(db_amp(db)));
        let lower = trace(r, &h.traces[1], |db| 1. - amp_frac(db_amp(db)));
        kit::fill_to(window, &upper, r.py(0.5), content(0.12));
        kit::fill_to(window, &lower, r.py(0.5), content(0.12));
        let color = if enabled { pal.accent } else { pal.muted };
        let out_up = trace(r, &h.traces[2], |db| amp_frac(db_amp(db)));
        let out_dn = trace(r, &h.traces[2], |db| 1. - amp_frac(db_amp(db)));
        kit::fill_to(window, &out_up, r.py(0.5), with_alpha(color, 0.35));
        kit::fill_to(window, &out_dn, r.py(0.5), with_alpha(color, 0.35));
        kit::line(window, &out_up, 1., color);
        kit::line(window, &out_dn, 1., color);
        // The ceiling, on both halves.
        let c = amp_frac(db_amp(ceiling));
        kit::hline(window, r, r.py(c), pal.record);
        kit::hline(window, r, r.py(1. - c), with_alpha(pal.record, 0.5));
        kit::txt(window, cx, &format!("ceiling {ceiling:+.1} dB"), r.right() - 34., r.py(c) - 13., TEXT_XS, pal.muted, TextAlign::Right);
        // Gain reduction from the top edge: 12 dB over a third of the height.
        let gr = trace(r, &h.traces[0], |db| (-db / 36.).min(0.34));
        kit::fill_to(window, &gr, r.y, with_alpha(pal.record, 0.4));
        kit::line(window, &gr, 1.5, pal.record);
        kit::dot(window, r.pt(0.94, c), 7., pal.record, Some(pal.text));
        kit::dot(window, r.pt(0.06, 0.5), 7., series(1), Some(pal.text));
        kit::txt(window, cx, &format!("in {gain:+.1} dB"), r.px(0.06) + 12., r.py(0.5) - 15., TEXT_XS, pal.muted, TextAlign::Left);
    });

    // The plugin publishes its own BS.1770 meters (integrated, short-term,
    // LRA). Fall back to the Main output analysis before the first block.
    ws.meters.keep_alive();
    let loud = ws.meters.loudness();
    let own = dev.meter.len() > 6;
    let (gr, tp, input) = (
        dev.m(0),
        if own { dev.m(1) } else { -120. },
        if own { dev.m(2) } else { -120. },
    );
    let lufs = |v: f32| {
        if v > -99. {
            format!("{v:.1}")
        } else {
            "—".to_owned()
        }
    };
    let (li, ls, lra) = if own {
        (lufs(dev.m(3)), lufs(dev.m(4)), lufs(dev.m(5)))
    } else {
        (
            lufs(loud.map_or(-120., |l| l.integrated)),
            lufs(loud.map_or(-120., |l| l.short_term)),
            lufs(loud.map_or(0., |l| l.range)),
        )
    };
    let readouts = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(kit::readout("GR", format!("{gr:.1} dB"), if gr < -0.05 { pal.record } else { pal.muted }))
        .child(kit::readout(
            "TP out",
            format!("{} dBTP", kit::fmt_db(tp)),
            if tp > ceiling + 0.05 {
                pal.record
            } else {
                pal.text
            },
        ))
        .child(kit::readout(
            "TP in",
            format!("{} dBTP", kit::fmt_db(input)),
            pal.text,
        ))
        .child(kit::readout("LUFS I", li, pal.text))
        .child(kit::readout("LUFS S", ls, pal.text))
        .child(kit::readout("LRA", format!("{lra} LU"), pal.text));

    kit::panel(if dev.big { 720. } else { 480. })
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(readouts)
        .child(kit::strip(
            ws,
            dev,
            &[
                p::GAIN,
                p::CEILING,
                p::STYLE,
                p::ATTACK,
                p::RELEASE,
                p::LOOKAHEAD,
                p::AUTO_RELEASE,
                p::TRUE_PEAK,
            ],
            &[
                p::LINK,
                p::OVERSAMPLING,
                p::CHANNEL,
                p::AUTO_ATTACK,
                p::AUTO_GAIN,
                p::DITHER,
                p::SHAPING,
                p::DELTA,
                p::RESET,
            ],
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amplitudes_mirror_around_the_centre() {
        assert_eq!(amp_frac(0.), 0.5);
        assert_eq!(amp_frac(TOP), 0.);
        assert!((amp_frac(1.) - (0.5 - 0.5 / TOP)).abs() < 1e-6);
        assert_eq!(db_amp(-120.), 0.);
        assert!((db_amp(-6.02) - 0.5).abs() < 1e-3);
    }
}
