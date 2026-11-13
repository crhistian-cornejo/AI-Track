//! `fx.reverb`: decay time over frequency with three draggable points
//! (mid decay, bass multiplier and its corner, damping and its corner), and
//! a sketch of the impulse: pre-delay gap, early reflections, tail, and the
//! shimmer climbing out of it. The algorithm, colour and freeze sit on the
//! control row. In a device window the large layout ([`window`]) takes
//! over and reuses both plots.

use super::kit::{self, Handle, PlotCfg, ax, frac_freq, freq_frac};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::reverb::p;
use aitrack_stock::fx::reverb::shimmer::INTERVALS;
use aitrack_stock::fx::reverb::voicing::{VOICES, Voicing, voice_index};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Stateful, Styled, TextAlign, div, px};

/// The device window's large editor (lives with the other windows).
#[path = "../device_window/reverb/mod.rs"]
pub mod window;

/// The time axis of the decay plot, seconds (log).
const T_MIN: f32 = 0.05;
const T_MAX: f32 = 40.;

pub fn time_frac(secs: f32) -> f32 {
    1. - ((secs.max(T_MIN) / T_MIN).ln() / (T_MAX / T_MIN).ln()).clamp(0., 1.)
}

/// What shapes the decay across the spectrum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decay {
    pub secs: f32,
    pub bass_mult: f32,
    pub bass_freq: f32,
    /// 0..1.
    pub damping: f32,
    pub damp_freq: f32,
}

/// The voicing the device's Algorithm / Mode select.
pub fn voicing_of(dev: &Dev) -> Voicing {
    VOICES[voice_index(dev.choice(p::ALGORITHM), dev.choice(p::MODE))]
}

impl Decay {
    pub fn of(dev: &Dev) -> Self {
        Self {
            secs: dev.plain(p::DECAY),
            bass_mult: dev.plain(p::BASS_MULT),
            bass_freq: dev.plain(p::BASS_FREQ),
            // Bright voicings damp less, warm ones more (as the plugin does).
            damping: (dev.plain(p::DAMPING) / 100. * voicing_of(dev).damping).min(1.2),
            damp_freq: dev.plain(p::DAMP_FREQ),
        }
    }

    /// Approximate decay time at `freq`: the mid decay, scaled below the bass
    /// corner and shortened above the damping corner (which slides down as
    /// damping grows, as in the plugin).
    pub fn at(&self, freq: f32) -> f32 {
        let low = 1. / (1. + (freq / self.bass_freq).powi(2));
        let corner = self.damp_freq * 2f32.powf(2. * (0.5 - self.damping));
        let high = 1. / (1. + (corner / freq).powi(2));
        let hf = 1. / (1. + 4. * self.damping);
        self.secs * (1. + (self.bass_mult - 1.) * low) * (1. + (hf - 1.) * high)
    }
}

/// Seconds between the dry sound and the end of the early cluster, and the
/// early level (0..1), for the current size and voicing.
pub fn early_shape(dev: &Dev) -> (f32, f32) {
    let v = voicing_of(dev);
    let size = dev.plain(p::SIZE) / 100.;
    let span = v.early_span.0 * (v.early_span.1 / v.early_span.0).powf(size.clamp(0., 1.)) / 1000.;
    (span, (dev.plain(p::EARLY) / 100. * v.early_gain).min(1.))
}

/// Shimmer loop amount (0..1) and its interval in semitones.
pub fn shimmer_of(dev: &Dev) -> (f32, f32) {
    let floor = voicing_of(dev).shimmer_floor;
    let amount = floor + (1. - floor) * dev.plain(p::SHIMMER) / 100.;
    (amount, INTERVALS[dev.choice(p::SHIFT).min(INTERVALS.len() - 1)])
}

/// Decay time over frequency with its three handles.
pub fn decay_plot(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Stateful<gpui::Div> {
    let d = Decay::of(dev);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let log_y = |id: u32| dev.info(id).map_or(-1., |i| -(T_MAX / T_MIN).ln() / (i.max / i.min).ln());
    let span = |id: u32| dev.info(id).map_or(1., kit::log_span);
    let point = |f: f32, probe: f32| (freq_frac(f), time_frac(d.at(probe)));
    let (mid, bass, damp) = (point(1_000., 1_000.), point(d.bass_freq, d.bass_freq * 0.5), point(d.damp_freq, d.damp_freq * 2.));
    let handles = vec![
        Handle { fx: mid.0, fy: mid.1, y: ax(p::DECAY, log_y(p::DECAY)), index: 0, reset: vec![default(p::DECAY)], ..Default::default() },
        Handle { fx: bass.0, fy: bass.1, x: ax(p::BASS_FREQ, span(p::BASS_FREQ)), y: ax(p::BASS_MULT, log_y(p::BASS_MULT)), index: 1, reset: vec![default(p::BASS_MULT), default(p::BASS_FREQ)], ..Default::default() },
        Handle { fx: damp.0, fy: damp.1, x: ax(p::DAMP_FREQ, span(p::DAMP_FREQ)), y: ax(p::DAMPING, 1.3), index: 2, reset: vec![default(p::DAMPING), default(p::DAMP_FREQ)], ..Default::default() },
    ];
    let (selected, enabled, measured, frozen) = (dev.selected, dev.enabled, dev.m(2), dev.on(p::FREEZE));
    let marks = handles.clone();
    kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::freq_grid(window, cx, r, true);
        for t in [0.1, 0.3, 1., 3., 10., 30.] {
            let y = r.py(time_frac(t));
            kit::hline(window, r, y, if t == 1. { pal.grid_bar } else { pal.grid_beat });
            kit::txt(window, cx, &format!("{t} s"), r.x + 4., y - 12., TEXT_XS, pal.faint, TextAlign::Left);
        }
        let color = if enabled { pal.accent } else { pal.muted };
        let pts = kit::sample_curve(r, 120, |fx| time_frac(d.at(frac_freq(fx))));
        kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.14));
        kit::line(window, &pts, 2., color);
        if frozen {
            kit::txt(window, cx, "FROZEN", r.right() - 6., r.y + 4., TEXT_XS, pal.accent, TextAlign::Right);
        } else if measured > 0.05 {
            // The tank's own estimate of the mid-band RT60.
            kit::txt(window, cx, &format!("RT60 {measured:.1} s"), r.right() - 6., r.y + 4., TEXT_XS, pal.muted, TextAlign::Right);
        }
        for (i, h) in marks.iter().enumerate() {
            kit::dot(window, r.pt(h.fx, h.fy), if i == selected { 8. } else { 7. }, series(i), Some(if i == selected { pal.text } else { pal.raised }));
        }
    })
}

/// The impulse over time: direct sound, gap, early reflections, tail (and
/// the shimmer's rising copies), with pre-delay and size / early handles.
pub fn impulse_plot(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Stateful<gpui::Div> {
    let d = Decay::of(dev);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let pre = dev.plain(p::PREDELAY) / 1000.;
    let (cluster, early) = early_shape(dev);
    let (shimmer, interval) = shimmer_of(dev);
    let duck = dev.plain(p::DUCK) / 100.;
    let total = (pre + d.secs * 0.9).max(0.6);
    let handles = vec![
        Handle { fx: (pre / total).clamp(0.02, 0.98), fy: 0.82, x: ax(p::PREDELAY, 1.), index: 3, reset: vec![default(p::PREDELAY)], ..Default::default() },
        Handle { fx: ((pre + cluster) / total).clamp(0.04, 0.99), fy: 0.3, x: ax(p::SIZE, 2.5), y: ax(p::EARLY, -1.5), index: 4, reset: vec![default(p::SIZE), default(p::EARLY)], ..Default::default() },
    ];
    let marks = handles.clone();
    let enabled = dev.enabled;
    let duck_now = if duck > 0. { dev.m(3) } else { 1. };
    kit::plot(ws, dev, PlotCfg { slot: 1, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let base = r.py(0.9);
        kit::hline(window, r, base, pal.grid_bar);
        let x_of = |t: f32| r.px((t / total).clamp(0., 1.));
        let color = if enabled { pal.accent } else { pal.muted };
        // The tail: −60 dB after the decay time.
        let pts = kit::sample_curve(r, 96, |fx| {
            let t = fx * total - pre;
            if t < 0. { 0.9 } else { 0.9 - 0.62 * (-6.91 * t / d.secs.max(0.05)).exp() }
        });
        kit::fill_to(window, &pts, base, with_alpha(color, 0.2));
        kit::line(window, &pts, 1.5, color);
        if shimmer > 0.01 {
            // Each trip round the shimmer comes back an interval higher and
            // a little quieter.
            for k in 1..5 {
                let t = pre + d.secs * 0.12 * k as f32;
                let h = r.h * 0.5 * shimmer.powi(k);
                if t < total && h > 1. {
                    kit::bar(window, x_of(t), base - h, 2., h, 1., with_alpha(series(5), 0.8));
                }
            }
            let sign = if interval >= 0. { "+" } else { "" };
            kit::txt(window, cx, &format!("shimmer {sign}{interval:.0}"), r.right() - 6., r.y + 4., TEXT_XS, series(5), TextAlign::Right);
        }
        kit::bar(window, r.x + 2., r.py(0.1), 2., r.h * 0.8, 1., pal.text);
        for k in 0..8 {
            let t = pre + cluster * (k as f32 + 1.) / 8. * (1. + 0.13 * ((k * 7) % 5) as f32 / 4.);
            let h = r.h * 0.6 * early * (1. - k as f32 / 11.);
            kit::bar(window, x_of(t), base - h, 1.5, h, 0., series(4));
        }
        kit::txt(window, cx, &format!("pre {:.0} ms", pre * 1000.), x_of(pre) + 6., base - 14., TEXT_XS, pal.faint, TextAlign::Left);
        if duck > 0. {
            // How far the ducker holds the tail down right now.
            let h = r.h * 0.8 * (1. - duck_now.clamp(0., 1.));
            kit::bar(window, r.right() - 5., r.py(0.1), 3., h, 1.5, pal.accent2);
        }
        for h in &marks {
            kit::dot(window, r.pt(h.fx, h.fy), 6., series(h.index), Some(pal.text));
        }
    })
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    if dev.big && ws.fx.painting.is_some() {
        return window::render(ws, dev, cx);
    }
    let decay = decay_plot(ws, dev, cx);
    let impulse = impulse_plot(ws, dev, cx);
    let classic = dev.choice(p::ALGORITHM) == 0;
    let mut row = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1)).child(kit::pick(dev, p::ALGORITHM, cx));
    if classic {
        row = row.child(kit::pick(dev, p::MODE, cx));
    }
    row = row.child(kit::pick(dev, p::COLOR, cx)).child(kit::pick(dev, p::FREEZE, cx));
    kit::panel(if dev.big { 780. } else { 540. })
        .child(div().flex_1().min_h(px(0.)).flex().gap(px(SP2)).child(div().flex_1().min_w(px(0.)).h_full().child(decay)).child(div().flex_none().w(px(if dev.big { 260. } else { 180. })).h_full().child(impulse)))
        .child(row.child(kit::strip(
            ws,
            dev,
            &[p::DECAY, p::PREDELAY, p::SIZE, p::DAMPING, p::MIX],
            &[p::SHIMMER, p::SHIFT, p::DUCK, p::BASS_MULT, p::BASS_FREQ, p::DAMP_FREQ, p::DIFFUSION, p::MOD_DEPTH, p::MOD_RATE, p::EARLY, p::WIDTH, p::LOW_CUT, p::HIGH_CUT, p::OUTPUT, p::LINES],
            cx,
        )))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_follows_the_bass_and_damping_controls() {
        let d = Decay { secs: 2., bass_mult: 1., bass_freq: 250., damping: 0., damp_freq: 5_000. };
        // Flat: the mid decay everywhere.
        for f in [30., 1_000., 15_000.] {
            assert!((d.at(f) - 2.).abs() < 1e-4);
        }
        let d = Decay { bass_mult: 2., damping: 0.5, ..d };
        assert!(d.at(40.) > 3.6, "bass rings longer: {}", d.at(40.));
        assert!((d.at(1_000.) - 2.).abs() < 0.2, "the mid band keeps the set time: {}", d.at(1_000.));
        assert!(d.at(16_000.) < 0.5 * d.at(1_000.), "highs are damped");
        let more = Decay { damping: 0.9, ..d };
        assert!(more.at(8_000.) < d.at(8_000.));
    }

    #[test]
    fn algorithms_shape_the_early_field_and_the_shimmer() {
        use super::super::testing::dev;
        let room = dev("fx.reverb", &[(p::ALGORITHM, 4.), (p::SIZE, 100.), (p::EARLY, 100.)]);
        let hall = dev("fx.reverb", &[(p::ALGORITHM, 1.), (p::SIZE, 100.), (p::EARLY, 100.)]);
        assert!(early_shape(&room).0 < early_shape(&hall).0, "a room's reflections come sooner");
        assert!(early_shape(&room).1 > early_shape(&hall).1, "and louder");
        let classic = dev("fx.reverb", &[(p::MODE, 1.)]);
        assert_eq!(voicing_of(&classic), VOICES[1], "Classic follows Mode");
        let shimmer = dev("fx.reverb", &[(p::ALGORITHM, 7.), (p::SHIMMER, 0.)]);
        assert!(shimmer_of(&shimmer).0 > 0.3, "the Shimmer algorithm always shimmers");
        assert_eq!(shimmer_of(&shimmer).1, 12., "an octave up by default");
        assert_eq!(shimmer_of(&hall).0, 0.);
    }

    #[test]
    fn the_time_axis_is_logarithmic() {
        assert_eq!(time_frac(T_MIN), 1.);
        assert!(time_frac(T_MAX).abs() < 1e-6);
        let (a, b, c) = (time_frac(0.5), time_frac(1.), time_frac(2.));
        assert!(((a - b) - (b - c)).abs() < 1e-5, "octaves are evenly spaced");
    }
}
