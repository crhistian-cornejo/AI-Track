//! `fx.mbdyn`: three-band dynamics, downward and upward. Each band is a
//! column with a vertical level meter whose axis is the dial: the downward
//! threshold, the upward threshold and the Floor are lines you drag up and
//! down, and they cut the meter into zones (above the downward threshold the
//! band is pushed down, between the two thresholds it is left alone, between
//! the upward threshold and the Floor it is lifted, under the Floor nothing is
//! lifted). The curve drawn over the meter is the gain the band applies at
//! each level (left of the centre line it cuts, right of it it lifts), and the
//! strip beside it shows the gain being applied right now, up or down, with
//! the *Max Up* cap as a handle. The crossovers are two handles on the
//! frequency bar under the columns. The ratios, times and levels of the
//! selected band sit under the display; the controls that act on all bands
//! are on the right.

use super::kit::{self, Handle, PlotCfg, Rect, ax, freq_frac};
use super::multiband::edges;
use super::{Dev, caption};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_stock::fx::mbdyn::{METER_BAND_PEAK, METER_GAIN, METER_IN_PEAK, METER_LATENCY, METER_LEN, METER_LINEAR, METER_OUT_PEAK, p};
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, TextAlign, div, point, prelude::FluentBuilder, px};

/// Bottom of the level axis, dB.
const LV_LO: f32 = -80.;
/// Gain at the top and bottom edge of the gain strip, dB.
const GS: f32 = 24.;
/// The meter's vertical extent as fractions of the plot, and the frequency
/// bar's row.
const MT: f32 = 0.03;
const MB: f32 = 0.80;
const XF_Y: f32 = 0.92;
/// Where things sit across one column (fractions of its width): the gain
/// strip's centre, the meter's edges and the handles' column.
const GAIN_X: f32 = 0.085;
const GAIN_W: f32 = 0.09;
const METER_L: f32 = 0.17;
const METER_R: f32 = 0.74;
const HANDLE_X: f32 = 0.89;
/// Handle indices: `band * 4 + kind`, then the two crossovers.
const K_DOWN: usize = 0;
const K_UP: usize = 1;
const K_FLOOR: usize = 2;
const K_MAXUP: usize = 3;
const XOVER: usize = 12;

/// Normalised ratio change when Alt-dragging a threshold across the whole
/// plot height (up raises the ratio).
const RATIO_PER_FRAC: f32 = -1.5;

const NAMES: [&str; 3] = ["LOW", "MID", "HIGH"];

/// The band whose controls are shown, from the selected handle index.
pub fn band_of(selected: usize) -> usize {
    (if selected < XOVER { selected / 4 } else { selected - XOVER }).min(2)
}

/// Vertical plot fraction of a detector level.
pub fn level_fy(db: f32) -> f32 {
    MT + (MB - MT) * (1. - ((db - LV_LO) / -LV_LO).clamp(0., 1.))
}

/// Vertical plot fraction of an applied gain (0 dB in the middle of the strip).
pub fn gain_fy(g: f32) -> f32 {
    (MT + MB) / 2. - (g / GS).clamp(-1., 1.) * (MB - MT) / 2.
}

/// How far a level parameter's normalised value moves when its handle
/// crosses the whole plot height (up raises it), for a parameter spanning
/// `span` dB.
pub fn level_per_frac(span: f32) -> f32 {
    -(-LV_LO / (MB - MT)) / span
}

/// The same for the Max Up handle on the gain strip.
pub fn gain_per_frac(span: f32) -> f32 {
    -(2. * GS / (MB - MT)) / span
}

/// A ratio as text: "3:1", "2.5:1", "off" at 1.
pub fn ratio_text(ratio: f32) -> String {
    if ratio < 1.05 {
        "off".to_owned()
    } else if ratio >= 10. || (ratio - ratio.round()).abs() < 0.05 {
        format!("{ratio:.0}:1")
    } else {
        format!("{ratio:.1}:1")
    }
}

/// Excess of `over` above 0 dB with a quadratic soft knee of width `knee`.
fn knee_excess(over: f32, knee: f32) -> f32 {
    if knee > 0. && 2. * over.abs() <= knee {
        let t = over + knee * 0.5;
        t * t / (2. * knee)
    } else if over > 0. {
        over
    } else {
        0.
    }
}

/// The static gain of one band, as the plugin computes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandCurve {
    pub down: f32,
    pub up: f32,
    pub floor: f32,
    pub max_up: f32,
    /// The ratios as set (1 = off), for display.
    pub ratio_down: f32,
    pub ratio_up: f32,
    slope_down: f32,
    slope_up: f32,
    knee: f32,
}

impl BandCurve {
    pub fn of(dev: &Dev, b: u32) -> Self {
        let amount = dev.plain(p::AMOUNT) * 0.01;
        let up = dev.plain(p::UPWARD) * 0.01 * amount;
        let down = dev.plain(p::DOWNWARD) * 0.01 * amount;
        let ta = dev.plain(p::band(b, p::TH_DOWN));
        let rd = dev.plain(p::band(b, p::RATIO_DOWN)).max(1.);
        let ru = dev.plain(p::band(b, p::RATIO_UP)).max(1.);
        Self {
            down: ta,
            up: dev.plain(p::band(b, p::TH_UP)).min(ta),
            floor: dev.plain(p::band(b, p::FLOOR)),
            max_up: dev.plain(p::band(b, p::MAX_UP)),
            ratio_down: rd,
            ratio_up: ru,
            slope_down: (1. - 1. / rd) * down,
            slope_up: (1. - 1. / ru) * up,
            knee: dev.plain(p::KNEE),
        }
    }

    /// Gain in dB (negative cuts, positive lifts) for a steady detector level.
    pub fn gain(&self, level: f32) -> f32 {
        let down = knee_excess(level - self.down, self.knee) * self.slope_down;
        let fade = ((level - self.floor) / 6.).clamp(0., 1.);
        let up = (knee_excess(self.up - level, self.knee) * self.slope_up).min(self.max_up) * fade;
        (up - down).clamp(-100., 60.)
    }
}

/// Everything the plot needs about one band.
#[derive(Clone, Copy)]
struct BandView {
    curve: BandCurve,
    /// Detector level now, dB, and the gain being applied, dB.
    level: f32,
    gain: f32,
    /// The band's output peak, dBFS.
    peak: f32,
    mute: bool,
    solo: bool,
}

/// What the plot draws.
#[derive(Clone, Copy)]
struct View {
    bands: [BandView; 3],
    live: bool,
    /// Band edges, Hz.
    edges: [f32; 4],
    selected: usize,
    enabled: bool,
}

fn paint(r: Rect, view: View, window: &mut gpui::Window, cx: &mut gpui::App) {
    let View { bands, live, edges: e, selected, enabled } = view;
    let pal = p();
    let cw = r.w / 3.;
    let (top, bottom) = (r.py(MT), r.py(MB));
    for (b, v) in bands.iter().enumerate() {
        let color = if enabled { series(b) } else { pal.muted };
        let x0 = r.px(b as f32 / 3.);
        let at = |f: f32| x0 + f * cw;
        let here = b == selected;
        kit::bar(window, x0, r.y, cw, bottom - r.y + 0.04 * r.h, 0., with_alpha(color, if here { 0.1 } else { 0.035 }));
        if b > 0 {
            kit::vline(window, r, x0, pal.grid_bar);
        }
        let (mx0, mx1) = (at(METER_L), at(METER_R));
        let mw = mx1 - mx0;
        let c = &v.curve;
        let (yd, yu, yf) = (r.py(level_fy(c.down)), r.py(level_fy(c.up)), r.py(level_fy(c.floor)));
        // Zones, top to bottom: cut, left alone, lift, nothing lifted.
        kit::bar(window, mx0, top, mw, yd - top, 0., with_alpha(pal.record, 0.13));
        kit::bar(window, mx0, yd, mw, (yu - yd).max(0.), 0., content(0.03));
        kit::bar(window, mx0, yu, mw, (yf - yu).max(0.), 0., with_alpha(pal.play, 0.13));
        kit::bar(window, mx0, yf, mw, (bottom - yf).max(0.), 0., with_alpha(pal.base, 0.4));
        for db in [-60., -40., -20.] {
            let y = r.py(level_fy(db));
            kit::bar(window, mx0, y, mw, 1., 0., content(0.07));
            kit::txt(window, cx, &format!("{db:.0}"), mx0 + 3., y + 1., TEXT_XS, pal.faint, TextAlign::Left);
        }
        // The level now.
        if live && v.level > LV_LO {
            let y = r.py(level_fy(v.level));
            kit::bar(window, mx0, y, 6., bottom - y, 0., with_alpha(color, 0.9));
            kit::bar(window, mx0, y - 1., mw, 2., 0., with_alpha(color, 0.8));
        }
        // The band's output peak, on the same axis: a short mark on the right edge.
        if live && v.peak > LV_LO {
            let y = r.py(level_fy(v.peak));
            kit::bar(window, mx1 - 10., y - 1., 10., 2., 0., with_alpha(pal.text, 0.9));
        }
        // What the band does at every level: left of the centre line it cuts.
        let mcx = (mx0 + mx1) / 2.;
        kit::bar(window, mcx, top, 1., bottom - top, 0., content(0.14));
        let pts: Vec<_> = (0..=48)
            .map(|i| {
                let level = LV_LO * (i as f32 / 48.);
                point(px(mcx + (c.gain(level) / GS).clamp(-1., 1.) * mw / 2.), px(r.py(level_fy(level))))
            })
            .collect();
        kit::line(window, &pts, 1.5, with_alpha(pal.text, 0.85));
        // Threshold and floor lines run out to their handles.
        let hx = at(HANDLE_X);
        for (y, line, name, dy) in [
            (yd, pal.record, format!("{:.0} · {}", c.down, ratio_text(c.ratio_down)), -12.),
            (yu, pal.play, format!("{:.0} · {}", c.up, ratio_text(c.ratio_up)), 1.),
            (yf, pal.muted, format!("{:.0}", c.floor), 1.),
        ] {
            kit::bar(window, mx0, y - 0.5, hx - mx0, 1.5, 0., with_alpha(line, 0.9));
            kit::txt(window, cx, &name, hx - 9., y + dy, TEXT_XS, pal.muted, TextAlign::Right);
        }
        // The gain strip: zero in the middle, lifts above, cuts below.
        let (gx, gw) = (at(GAIN_X - GAIN_W / 2.), GAIN_W * cw);
        let y0 = r.py(gain_fy(0.));
        kit::bar(window, gx, top, gw, bottom - top, 3., content(0.05));
        if live {
            let y = r.py(gain_fy(v.gain));
            if v.gain >= 0. {
                kit::bar(window, gx + 2., y, gw - 4., y0 - y, 1., pal.play);
            } else {
                kit::bar(window, gx + 2., y0, gw - 4., y - y0, 1., pal.record);
            }
        }
        kit::bar(window, gx - 2., y0, gw + 4., 1., 0., content(0.35));
        let ym = r.py(gain_fy(c.max_up));
        kit::bar(window, gx - 1., ym - 1., gw + 2., 2., 0., with_alpha(pal.play, 0.9));
        // Handles.
        let ring = here.then_some(pal.text);
        kit::dot(window, point(px(hx), px(yd)), 6., if enabled { pal.record } else { pal.muted }, ring);
        kit::dot(window, point(px(hx), px(yu)), 6., if enabled { pal.play } else { pal.muted }, ring);
        kit::dot(window, point(px(hx), px(yf)), 5., pal.raised, Some(if here { pal.text } else { pal.muted }));
        kit::dot(window, point(px(at(GAIN_X)), px(ym)), 5., if enabled { pal.play } else { pal.muted }, ring);
        if v.mute || v.solo {
            kit::txt(window, cx, if v.mute { "MUTE" } else { "SOLO" }, mcx, top + 3., TEXT_XS, if v.mute { pal.mute } else { pal.solo }, TextAlign::Center);
        }
    }
    // The frequency bar: the bands as segments, the crossovers as handles.
    let y = r.py(XF_Y);
    for b in 0..3 {
        let (a, z) = (r.px(freq_frac(e[b])), r.px(freq_frac(e[b + 1])));
        kit::bar(window, a + 1., y - 2., (z - a - 2.).max(1.), 4., 2., with_alpha(if enabled { series(b) } else { pal.muted }, 0.6));
    }
    for f in [e[1], e[2]] {
        let x = r.px(freq_frac(f));
        kit::dot(window, point(px(x), px(y)), 6., pal.raised, Some(pal.text));
        let text = if f >= 1000. { format!("{:.1} kHz", f / 1000.) } else { format!("{f:.0} Hz") };
        kit::txt(window, cx, &text, x + 9., y - 13., TEXT_XS, pal.muted, TextAlign::Left);
    }
}

/// One band's header cell: name, the gain it is applying, mute and solo.
fn header(dev: &Dev, b: usize, gain: Option<f32>, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let device = dev.id;
    div()
        .flex_1()
        .min_w(px(0.))
        .h(px(24.))
        .px(px(SP1))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded_t(px(r_sm()))
        .bg(with_alpha(series(b), if selected { 0.14 } else { 0.05 }))
        .child(div().flex_none().size(px(8.)).rounded_full().bg(series(b)))
        .child(label(NAMES[b], TEXT_XS, if selected { pal.text } else { pal.muted }).font_weight(gpui::FontWeight::SEMIBOLD))
        .child(div().flex_1())
        .when_some(gain, |d, g| d.child(mono(format!("{g:+.1}"), TEXT_XS, if g.abs() < 0.05 { pal.faint } else if g > 0. { pal.play } else { pal.record })))
        .child(kit::pick(dev, p::band(b as u32, p::MUTE), cx))
        .child(kit::pick(dev, p::band(b as u32, p::SOLO), cx))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.fx.selected.insert(device, b * 4);
                cx.notify();
            }),
        )
        .into_any_element()
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let live = dev.meter.len() >= METER_LEN;
    let sel = band_of(dev.selected);
    let enabled = dev.enabled;
    let e = edges(dev.plain(p::XOVER_LOW), dev.plain(p::XOVER_HIGH));
    let bands: [BandView; 3] = std::array::from_fn(|b| BandView {
        curve: BandCurve::of(dev, b as u32),
        level: dev.m(b),
        gain: dev.m(METER_GAIN + b),
        peak: dev.m(METER_BAND_PEAK + b),
        mute: dev.on(p::band(b as u32, p::MUTE)),
        solo: dev.on(p::band(b as u32, p::SOLO)),
    });

    // Handles: per band the two thresholds, the Floor and Max Up; then the crossovers.
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let span = |id: u32| dev.info(id).map_or(1., |i| i.max - i.min);
    let level_axis = |id: u32| ax(id, level_per_frac(span(id)));
    let mut handles = Vec::new();
    for (b, v) in bands.iter().enumerate() {
        let bu = b as u32;
        let x0 = b as f32 / 3.;
        let hx = x0 + HANDLE_X / 3.;
        let (down, up, floor, max_up) = (p::band(bu, p::TH_DOWN), p::band(bu, p::TH_UP), p::band(bu, p::FLOOR), p::band(bu, p::MAX_UP));
        // Alt-drag (or the wheel) on a threshold sets that side's ratio.
        let (ratio_down, ratio_up) = (p::band(bu, p::RATIO_DOWN), p::band(bu, p::RATIO_UP));
        handles.push(Handle { fx: hx, fy: level_fy(v.curve.down), y: level_axis(down), alt: ax(ratio_down, RATIO_PER_FRAC), index: b * 4 + K_DOWN, reset: vec![default(down), default(ratio_down)], ..Default::default() });
        handles.push(Handle { fx: hx, fy: level_fy(v.curve.up), y: level_axis(up), alt: ax(ratio_up, RATIO_PER_FRAC), index: b * 4 + K_UP, reset: vec![default(up), default(ratio_up)], ..Default::default() });
        handles.push(Handle { fx: hx, fy: level_fy(v.curve.floor), y: level_axis(floor), index: b * 4 + K_FLOOR, reset: vec![default(floor)], ..Default::default() });
        handles.push(Handle {
            fx: x0 + GAIN_X / 3.,
            fy: gain_fy(v.curve.max_up),
            y: ax(max_up, gain_per_frac(span(max_up))),
            index: b * 4 + K_MAXUP,
            reset: vec![default(max_up)],
            ..Default::default()
        });
    }
    for (i, id) in [p::XOVER_LOW, p::XOVER_HIGH].into_iter().enumerate() {
        let log = dev.info(id).map_or(1., kit::log_span);
        handles.push(Handle { fx: freq_frac(e[i + 1]), fy: XF_Y, x: ax(id, log), index: XOVER + i, reset: vec![default(id)], ..Default::default() });
    }
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| paint(r, View { bands, live, edges: e, selected: sel, enabled }, window, cx));

    let mut heads = div().flex_none().flex();
    for (b, v) in bands.iter().enumerate() {
        heads = heads.child(header(dev, b, live.then_some(v.gain), b == sel, cx));
    }

    // The selected band's ratios, times and levels.
    let sb = sel as u32;
    let mut row = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(div().flex_none().size(px(8.)).rounded_full().bg(series(sel)))
        .child(label(NAMES[sel], TEXT_XS, pal.text).font_weight(gpui::FontWeight::SEMIBOLD));
    for off in [p::RATIO_DOWN, p::RATIO_UP, p::MAX_UP, p::ATTACK, p::RELEASE, p::BAND_OUT, p::BAND_DEPTH] {
        row = row.child(kit::num_w(ws, dev, p::band(sb, off), 64., cx));
    }

    // Everything that acts on all bands.
    let global: &[u32] = if dev.big { &[p::AMOUNT, p::TIME, p::DEPTH, p::OUTPUT, p::UPWARD, p::DOWNWARD, p::KNEE, p::INPUT] } else { &[p::AMOUNT, p::TIME, p::DEPTH, p::OUTPUT] };
    let mut nums = div().flex_none().flex().flex_wrap().gap(px(SP1));
    for id in global {
        nums = nums.child(kit::num_w(ws, dev, *id, 60., cx));
    }
    let linear = live && dev.m(METER_LINEAR) > 0.5;
    let side = div()
        .flex_none()
        .w(px(if dev.big { 196. } else { 128. }))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(caption("ALL BANDS"))
        .child(nums)
        .child(div().flex().flex_wrap().gap(px(SP1)).child(kit::pick(dev, p::MODE, cx)).child(kit::pick(dev, p::DETECT, cx)))
        .child(super::sidechain_chip(ws, dev, p::SIDECHAIN, cx))
        .child(div().flex_1())
        .when(live, |d| {
            d.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(mono(format!("in {:>6} out {:>6}", kit::fmt_db(dev.m(METER_IN_PEAK)), kit::fmt_db(dev.m(METER_OUT_PEAK))), TEXT_XS, pal.faint))
                    .when(linear, |d| d.child(mono(format!("latency {:.0} smp", dev.m(METER_LATENCY)), TEXT_XS, pal.faint))),
            )
        });

    kit::panel(if dev.big { 980. } else { 800. })
        .flex_row()
        .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().child(heads).child(div().flex_1().min_h(px(0.)).child(plot)).child(div().h(px(SP2))).child(row))
        .child(side)
        .into_any_element()
}



#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use aitrack_stock::fx::mbdyn::{METER_CURVES, METER_CURVE_MIN_DB, METER_CURVE_POINTS};
    use aitrack_stock::test_util::{process_stereo, set_plain, sine};

    #[test]
    fn the_curve_is_the_plugins_static_curve() {
        let mut plain = vec![(p::KNEE, 9.), (p::UPWARD, 70.), (p::AMOUNT, 80.)];
        for b in 0..3u32 {
            plain.extend([
                (p::band(b, p::TH_DOWN), -20. - 4. * b as f32),
                (p::band(b, p::RATIO_DOWN), 2. + b as f32),
                (p::band(b, p::TH_UP), -50. + 3. * b as f32),
                (p::band(b, p::RATIO_UP), 3.),
                (p::band(b, p::MAX_UP), 15. - 4. * b as f32),
                (p::band(b, p::FLOOR), -70. + 5. * b as f32),
            ]);
        }
        let mut plugin = aitrack_stock::create("fx.mbdyn").unwrap();
        for (id, v) in &plain {
            set_plain(plugin.as_mut(), *id, *v);
        }
        process_stereo(plugin.as_mut(), sine(700., 0.1), 4096);
        let mut m = [0.0f32; 320];
        assert_eq!(plugin.meter(&mut m), METER_LEN);
        let d = dev("fx.mbdyn", &plain);
        for b in 0..3 {
            let c = BandCurve::of(&d, b as u32);
            for i in 0..METER_CURVE_POINTS {
                let level = METER_CURVE_MIN_DB - METER_CURVE_MIN_DB * i as f32 / (METER_CURVE_POINTS - 1) as f32;
                let plugin_out = m[METER_CURVES + b * METER_CURVE_POINTS + i];
                assert!((level + c.gain(level) - plugin_out).abs() < 0.05, "band {b} at {level:.1} dB: drawn {:.2}, plugin {plugin_out:.2}", level + c.gain(level));
            }
        }
    }

    #[test]
    fn zones_and_gain_have_the_expected_shape() {
        let d = dev("fx.mbdyn", &[(p::band(0, p::TH_DOWN), -20.), (p::band(0, p::RATIO_DOWN), 4.), (p::band(0, p::TH_UP), -50.), (p::band(0, p::FLOOR), -70.), (p::KNEE, 0.)]);
        let c = BandCurve::of(&d, 0);
        assert_eq!(c.gain(-30.), 0.);
        assert!((c.gain(-8.) + 9.).abs() < 1e-4, "{}", c.gain(-8.));
        assert!(c.gain(-60.) > 0.);
        assert_eq!(c.gain(-79.), 0.);
        // Louder levels are higher on the plot; gains mirror around the middle.
        assert!(level_fy(-10.) < level_fy(-50.));
        assert!((level_fy(0.) - MT).abs() < 1e-6 && (level_fy(-80.) - MB).abs() < 1e-6);
        assert!(gain_fy(6.) < gain_fy(0.) && gain_fy(0.) < gain_fy(-6.));
        assert!((gain_fy(0.) - (MT + MB) / 2.).abs() < 1e-6);
    }

    #[test]
    fn dragging_a_handle_keeps_it_under_the_pointer() {
        // A 1 dB-per-unit parameter dragged 10 % of the plot height down
        // lands where the plot draws that level.
        let (span, dy) = (60., 0.1);
        let d = dev("fx.mbdyn", &[]);
        let info = d.info(p::band(0, p::TH_DOWN)).unwrap();
        assert_eq!(info.max - info.min, span);
        let before = -26.;
        let after = before + dy * level_per_frac(span) * span;
        assert!((level_fy(after) - (level_fy(before) + dy)).abs() < 1e-4, "{after}");
        // Max Up: 36 dB wide, drawn on the ±24 dB strip.
        let (span, dy) = (36., 0.05);
        let after = 12. + dy * gain_per_frac(span) * span;
        assert!((gain_fy(after) - (gain_fy(12.) + dy)).abs() < 1e-4, "{after}");
    }

    #[test]
    fn ratios_read_as_text() {
        assert_eq!([1., 1.02, 2., 2.5, 3., 12.].map(ratio_text), ["off", "off", "2:1", "2.5:1", "3:1", "12:1"]);
        let c = BandCurve::of(&dev("fx.mbdyn", &[(p::band(1, p::RATIO_DOWN), 4.)]), 1);
        assert!((c.ratio_down - 4.).abs() < 1e-3);
    }

    #[test]
    fn handle_indices_select_their_band() {
        assert_eq!([0, 3, 4, 7, 8, 11].map(band_of), [0, 0, 1, 1, 2, 2]);
        assert_eq!([XOVER, XOVER + 1].map(band_of), [0, 1]);
        assert_eq!(band_of(99), 2);
    }
}
