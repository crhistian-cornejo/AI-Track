//! `fx.autopan`: the gain each channel gets over two LFO cycles, left and
//! right drawn on their own with the stereo offset (or spin) between them,
//! and a head that runs along them with the plugin's phase. The thin bar at
//! the right edge is Amount: drag it. Only the time control of the chosen
//! Time Mode is shown.

use super::kit::{self, Handle, PlotCfg, ax};
use super::{Dev, current, set_gliding};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::lfo::{PhaseLfo, Waveform};
use aitrack_stock::fx::autopan::p;
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, ScrollWheelEvent, Styled, TextAlign, div, px};
use std::f32::consts::{FRAC_PI_4, SQRT_2};

/// LFO cycles of the left channel the plot shows.
const CYCLES: f64 = 2.;
/// Spin speeds the right channel up by this much at 100 %.
const SPIN_SPREAD: f64 = 0.1;

/// Where a channel's LFO starts and how fast it runs next to the left one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Channel {
    pub phase: f64,
    pub ratio: f64,
}

/// The two channels' LFO settings for a Phase offset, Stereo amount (degrees)
/// and Stereo Mode (Spin runs the right channel faster; the random shapes
/// have no spin).
pub fn channels(phase_deg: f32, stereo_deg: f32, spin: bool, wave: Waveform) -> [Channel; 2] {
    let base = f64::from(phase_deg) / 360.;
    let spin = spin && !wave.is_random();
    let right = if spin {
        Channel { phase: base, ratio: 1. + SPIN_SPREAD * f64::from(stereo_deg / 180.) }
    } else {
        Channel { phase: base + f64::from(stereo_deg) / 360., ratio: 1. }
    };
    [Channel { phase: base, ratio: 1. }, right]
}

/// `n` points of a channel's LFO (−1..1) over [`CYCLES`] cycles of the left
/// one. The random shapes use `seed`, so the two channels differ and the
/// picture stays steady.
pub fn lfo_curve(wave: Waveform, shape: f32, ch: Channel, seed: u32, n: usize) -> Vec<f32> {
    let mut lfo = PhaseLfo::new(seed);
    (0..n).map(|i| lfo.value(wave, ch.phase + CYCLES * i as f64 / (n - 1).max(1) as f64 * ch.ratio, shape)).collect()
}

/// What the plugin does with an LFO value `v` (after Invert): the gain of that channel.
pub fn gain(tremolo: bool, vintage: bool, amount: f32, v: f32) -> f32 {
    if !tremolo {
        let m = amount * v;
        return if m == 0. { 1. } else { SQRT_2 * ((m.clamp(-1., 1.) + 1.) * FRAC_PI_4).cos() };
    }
    let mut u = 0.5 * (v + 1.);
    if vintage {
        u = u.powf(1.6);
    }
    1. - amount * (1. - u)
}

/// Samples the plot's curves are traced with when looking for their peaks.
const PEAK_N: usize = 480;

/// Where a channel's gain peaks over the plot: `(x fraction, LFO value)` of
/// the first (or last) sample within a hair of the best one. The LFO values
/// are the ones after Invert; Pan's gain falls as the LFO rises, so its peak
/// is the LFO's lowest point, Tremolo's the highest. Working on the LFO
/// rather than the gain keeps the spot steady at Amount 0.
pub fn peak_of(curve: &[f32], tremolo: bool, invert: bool, last: bool) -> (f32, f32) {
    let eff = |v: f32| if invert { -v } else { v };
    let score = |v: f32| if tremolo { eff(v) } else { -eff(v) };
    let best = curve.iter().map(|v| score(*v)).fold(f32::MIN, f32::max);
    let hit = |v: &&f32| score(**v) >= best - 1e-3;
    let at = if last { curve.iter().rposition(|v| hit(&v)) } else { curve.iter().position(|v| hit(&v)) }.unwrap_or(0);
    (at as f32 / (curve.len() - 1).max(1) as f32, eff(curve[at]))
}

/// How far the right channel's last peak moves across the plot (fractions of
/// its width) when Stereo goes from 0 to its maximum, signed: what dragging
/// the right channel's handle sideways has to follow. Measured on the curves
/// themselves so Phase and Spin both come out right; `None` when it hardly
/// moves (random shapes).
pub fn stereo_reach(phase_deg: f32, stereo_deg: f32, spin: bool, wave: Waveform, shape: f32, tremolo: bool, invert: bool) -> Option<f32> {
    let at = |st: f32| {
        let ch = channels(phase_deg, st, spin, wave)[1];
        peak_of(&lfo_curve(wave, shape, ch, 23, PEAK_N * 10), tremolo, invert, true).0
    };
    let step = 9.;
    let (a, b) = if stereo_deg + step <= 180. { (stereo_deg, stereo_deg + step) } else { (stereo_deg - step, stereo_deg) };
    let reach = (at(b) - at(a)) / (step / 180.);
    (reach.abs() > 0.02 && !wave.is_random()).then_some(reach)
}

/// Where a pan position sits for the signal: the balance between what the
/// two channels let through, -1 (left) … +1 (right), 0 when both are shut.
pub fn balance(gain_l: f32, gain_r: f32) -> f32 {
    let sum = gain_l + gain_r;
    if sum < 1e-4 { 0. } else { ((gain_r - gain_l) / sum).clamp(-1., 1.) }
}

/// Height of the field band under the plot.
const FIELD_H: f32 = 24.;

/// The stereo field now: in Pan a -1…+1 scale with a marker at the balance of
/// the two channels' gains, in Tremolo a gain bar per channel.
fn field(tremolo: bool, live: bool, gains: [f32; 2], enabled: bool) -> AnyElement {
    let pal = p();
    div()
        .flex_none()
        .h(px(FIELD_H))
        .rounded(px(r_sm()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .child(
            gpui::canvas(
                |_, _, _| {},
                move |b, _, window, cx| {
                    let r = kit::Rect::of(b);
                    let (left, right) = if enabled { (series(0), series(1)) } else { (pal.muted, pal.faint) };
                    if tremolo {
                        // Two gain bars from the left edge: 0 … 1.
                        let (x0, w) = (r.x + 20., r.w - 28.);
                        for (i, (g, c)) in [(gains[0], left), (gains[1], right)].into_iter().enumerate() {
                            let y = r.y + 4. + 8. * i as f32;
                            kit::bar(window, x0, y, w, 4., 2., content(0.08));
                            if live {
                                kit::bar(window, x0, y, (w * g.clamp(0., 1.)).max(0.), 4., 2., with_alpha(c, 0.9));
                            }
                        }
                        kit::txt(window, cx, "L", r.x + 6., r.y + 1., TEXT_XS, left, TextAlign::Left);
                        kit::txt(window, cx, "R", r.x + 6., r.y + 9., TEXT_XS, right, TextAlign::Left);
                    } else {
                        let (x0, x1) = (r.x + 16., r.right() - 16.);
                        let mid = (x0 + x1) / 2.;
                        let cy = r.y + r.h / 2.;
                        kit::bar(window, x0, cy - 1., x1 - x0, 2., 1., content(0.12));
                        for q in 0..=4 {
                            let x = x0 + (x1 - x0) * q as f32 / 4.;
                            kit::bar(window, x, cy - if q == 2 { 6. } else { 3. }, 1., if q == 2 { 12. } else { 6. }, 0., pal.grid_bar);
                        }
                        kit::txt(window, cx, "L", r.x + 5., cy - 6., TEXT_XS, left, TextAlign::Left);
                        kit::txt(window, cx, "R", r.right() - 5., cy - 6., TEXT_XS, right, TextAlign::Right);
                        if live {
                            let x = mid + (x1 - x0) / 2. * balance(gains[0], gains[1]);
                            kit::bar(window, mid.min(x), cy - 3., (x - mid).abs(), 6., 3., with_alpha(pal.accent, 0.35));
                            kit::dot(window, gpui::point(gpui::px(x), gpui::px(cy)), 5.5, pal.accent, Some(pal.text));
                        }
                    }
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// The part of the plot's height the Amount bar uses, centred.
const AMOUNT_SPAN: f32 = 0.9;

/// The plot fraction (0 = top) of an Amount (0..1) on its bar.
fn amount_frac(amount: f32) -> f32 {
    (1. - AMOUNT_SPAN) / 2. + AMOUNT_SPAN * (1. - amount.clamp(0., 1.))
}

/// The plot fraction (0 = top) of a gain: unity sits a third of the way down in Pan.
fn gain_frac(tremolo: bool, g: f32) -> f32 {
    let top = if tremolo { 1.1 } else { 1.5 };
    (1. - g / top).clamp(0., 1.)
}

/// Plain degrees per typed unit of Stereo for this device, when its Spin
/// shows the parameter as a percentage of 180° (`None` for every other
/// parameter and device).
pub fn typed_scale(ws: &Workspace, device: aitrack_model::DeviceId, param: u32) -> Option<f32> {
    spin_scale(ws.b.plugins.as_ref(), &ws.s.project, device, param)
}

fn spin_scale(plugins: &dyn crate::bridge::PluginCatalog, project: &aitrack_model::Project, device: aitrack_model::DeviceId, param: u32) -> Option<f32> {
    if param != p::STEREO {
        return None;
    }
    let d = project.device(device)?;
    if !matches!(&d.kind, aitrack_model::DeviceKind::Stock { plugin_id } if plugin_id == aitrack_stock::fx::autopan::ID) {
        return None;
    }
    let stock = plugins.device(aitrack_stock::fx::autopan::ID)?;
    let value = |id: u32| -> Option<usize> {
        let info = stock.params.iter().find(|i| i.id == id)?;
        let norm = d.params.get(&id).copied().unwrap_or_else(|| info.default_normalized());
        Some((info.to_plain(norm) - info.min).round().max(0.) as usize)
    };
    let spin = value(p::STEREO_MODE)? == 1 && !Waveform::from_index(value(p::WAVE)?).is_random();
    spin.then_some(1.8)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let tremolo = dev.choice(p::MODE) == 1;
    let wave = Waveform::from_index(dev.choice(p::WAVE));
    let time_mode = dev.choice(p::TIME_MODE);
    let spin = dev.choice(p::STEREO_MODE) == 1 && !wave.is_random();
    let amount = dev.plain(p::AMOUNT) / 100.;
    let shape = dev.plain(p::SHAPE) / 100.;
    let invert = dev.on(p::INVERT);
    let vintage = tremolo && dev.on(p::VINTAGE);
    let stereo = dev.plain(p::STEREO);
    let chans = channels(dev.plain(p::PHASE), stereo, spin, wave);
    let live = dev.meter.len() >= 3;
    let (lfo_l, lfo_r, phase) = (dev.m(0), dev.m(1), dev.m(2).rem_euclid(1.));
    let enabled = dev.enabled;
    let rate = match time_mode {
        0 => dev.text(p::RATE),
        1 => dev.text(p::TIME),
        _ => dev.text(p::DIVISION),
    };
    let stereo_text = if spin { format!("spin {:.0}%", stereo / 180. * 100.) } else { format!("{stereo:.0}° apart") };
    let title = format!("{} · {rate} · {stereo_text} · phase {:.0}°", if tremolo { "tremolo" } else { "pan" }, dev.plain(p::PHASE));
    // Amount is the bar on the right edge.
    let default = dev.info(p::AMOUNT).map_or(1., |i| i.default_normalized());
    let timing = match time_mode {
        0 => p::RATE,
        1 => p::TIME,
        _ => p::DIVISION,
    };
    // The channels' peaks are handles: the left one slides the curves (Phase)
    // and bends them (Shape), Alt or the wheel sets the speed; the right one
    // slides itself away from the left (Stereo).
    let shape_now = dev.plain(p::SHAPE) / 100.;
    let curve_of = |i: usize| lfo_curve(wave, shape_now, chans[i], if i == 0 { 11 } else { 23 }, PEAK_N);
    let (peak_l, peak_r) = (peak_of(&curve_of(0), tremolo, invert, false), peak_of(&curve_of(1), tremolo, invert, true));
    let default_of = |id: u32| (id, dev.info(id).map_or(0., |i| i.default_normalized()));
    let reach = stereo_reach(dev.plain(p::PHASE), stereo, spin, wave, shape_now, tremolo, invert);
    let handles = vec![
        Handle { fx: 0.97, fy: amount_frac(amount), y: ax(p::AMOUNT, -1. / AMOUNT_SPAN), index: 0, reset: vec![(p::AMOUNT, default)], ..Default::default() },
        Handle {
            fx: peak_l.0,
            fy: gain_frac(tremolo, gain(tremolo, vintage, amount, peak_l.1)),
            x: ax(p::PHASE, -(CYCLES as f32) * 360. / dev.info(p::PHASE).map_or(360., |i| i.max - i.min)),
            y: ax(p::SHAPE, -1.),
            alt: ax(timing, -1.),
            index: 1,
            reset: vec![default_of(p::PHASE), default_of(p::SHAPE)],
        },
        Handle {
            fx: peak_r.0,
            fy: gain_frac(tremolo, gain(tremolo, vintage, amount, peak_r.1)),
            x: reach.and_then(|k| ax(p::STEREO, 1. / k)),
            index: 2,
            reset: vec![default_of(p::STEREO)],
            ..Default::default()
        },
    ];
    let marks = [(handles[1].fx, handles[1].fy), (handles[2].fx, handles[2].fy)];

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let (left, right) = if enabled { (series(0), series(1)) } else { (pal.muted, pal.faint) };
        // Cycles and the unity-gain line.
        for q in 1..4 {
            kit::vline(window, r, r.px(q as f32 / 4.), if q == 2 { pal.grid_bar } else { pal.grid_beat });
        }
        kit::hline(window, r, r.py(gain_frac(tremolo, 1.)), pal.grid_bar);
        kit::hline(window, r, r.py(1.), pal.grid_beat);
        let n = ((r.w / 2.) as usize).clamp(64, 480);
        let curves = [lfo_curve(wave, shape, chans[0], 11, n), lfo_curve(wave, shape, chans[1], 23, n)];
        let pts = |c: &[f32]| -> Vec<gpui::Point<gpui::Pixels>> {
            c.iter()
                .enumerate()
                .map(|(i, v)| {
                    let v = if invert { -*v } else { *v };
                    r.pt(i as f32 / (n - 1) as f32, gain_frac(tremolo, gain(tremolo, vintage, amount, v)))
                })
                .collect()
        };
        let (pl, pr) = (pts(&curves[0]), pts(&curves[1]));
        kit::fill_to(window, &pl, r.bottom(), with_alpha(left, 0.10));
        kit::line(window, &pr, 1.5, with_alpha(right, 0.9));
        kit::line(window, &pl, 1.5, with_alpha(left, 0.95));
        // The head runs over both cycles at the plugin's phase.
        if live {
            for cycle in 0..CYCLES as usize {
                let fx = (phase + cycle as f32) / CYCLES as f32;
                kit::vline(window, r, r.px(fx), with_alpha(pal.text, 0.3));
                kit::dot(window, r.pt(fx, gain_frac(tremolo, gain(tremolo, vintage, amount, lfo_l))), 4., left, Some(pal.text));
                kit::dot(window, r.pt(fx, gain_frac(tremolo, gain(tremolo, vintage, amount, lfo_r))), 4., right, Some(pal.text));
            }
        }
        for (i, (fx, fy)) in marks.iter().enumerate() {
            kit::dot(window, r.pt(*fx, *fy), 6., if i == 0 { left } else { right }, Some(pal.text));
        }
        // Amount: a bar on the right edge, filled to its value.
        let (bx, bw) = (r.px(0.97) - 2., 4.);
        let (top, bottom) = (r.py(amount_frac(1.)), r.py(amount_frac(0.)));
        kit::bar(window, bx, top, bw, bottom - top, 2., content(0.08));
        let now = r.py(amount_frac(amount));
        kit::bar(window, bx, now, bw, bottom - now, 2., with_alpha(pal.accent, 0.7));
        kit::dot(window, r.pt(0.97, amount_frac(amount)), 5.5, pal.accent, Some(pal.text));
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "L", r.x + 6., r.bottom() - 16., TEXT_XS, left, TextAlign::Left);
        kit::txt(window, cx, "R", r.x + 18., r.bottom() - 16., TEXT_XS, right, TextAlign::Left);
    });

    let mut flavours = div().flex_none().flex().items_center().gap(px(SP1));
    flavours = flavours.child(kit::pick(dev, p::MODE, cx)).child(kit::pick(dev, p::WAVE, cx)).child(kit::pick(dev, p::TIME_MODE, cx));
    flavours = flavours.child(kit::pick(dev, p::STEREO_MODE, cx)).child(kit::pick(dev, p::INVERT, cx));
    if tremolo {
        flavours = flavours.child(kit::pick(dev, p::HARMONIC, cx)).child(kit::pick(dev, p::VINTAGE, cx));
    }
    let stereo_ctl = kit::num_shown(ws, dev, p::STEREO, 60., spin.then(|| format!("{:.0} %", stereo / 180. * 100.)), cx);
    let mut row = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1));
    for id in [timing, p::PHASE] {
        row = row.child(kit::ctl(ws, dev, id, cx));
    }
    row = row.child(stereo_ctl);
    let rest: &[u32] = if dev.big { &[p::AMOUNT, p::SHAPE, p::ATTACK, p::DYN_FM, p::MIX, p::OUTPUT] } else { &[p::AMOUNT, p::SHAPE, p::MIX, p::OUTPUT] };
    for id in rest {
        row = row.child(kit::ctl(ws, dev, *id, cx));
    }

    // The wheel anywhere over the display sets the speed.
    let wheel_dev = dev.clone();
    let wheel = cx.listener(move |this, e: &ScrollWheelEvent, _, cx| {
        let Some(info) = wheel_dev.info(timing) else { return };
        if crate::gesture::is_zoom(e) {
            return;
        }
        let stepped = matches!(info.scale, aitrack_dsp::ParamScale::Stepped);
        let Some(change) = kit::wheel_change(e.delta, stepped, e.modifiers.shift, (info.max - info.min).max(1.)) else { return };
        cx.stop_propagation();
        let v = (current(this, wheel_dev.id, info) + change).clamp(0., 1.);
        set_gliding(this, wheel_dev.id, &[(timing, v)]);
        cx.notify();
    });
    let gains = [gain(tremolo, vintage, amount, lfo_l), gain(tremolo, vintage, amount, lfo_r)];
    kit::panel(if dev.big { 640. } else { 460. })
        .child(div().flex_1().min_h(px(0.)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().on_scroll_wheel(wheel).child(plot))
        .child(field(tremolo, live, gains, enabled))
        .child(flavours)
        .child(row)
        .into_any_element()
}


#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn the_stereo_setting_offsets_the_right_channel_or_spins_it() {
        let [l, r] = channels(90., 180., false, Waveform::Sine);
        assert_eq!((l.phase, r.phase, r.ratio), (0.25, 0.75, 1.));
        let [_, r] = channels(0., 90., true, Waveform::Sine);
        assert!((r.ratio - 1.05).abs() < 1e-9 && r.phase == 0.);
        // The random shapes have no spin: they fall back to the offset.
        assert_eq!(channels(0., 90., true, Waveform::Random)[1].ratio, 1.);
    }

    #[test]
    fn the_curves_start_on_the_phase_and_stay_in_range() {
        for wave in Waveform::ALL {
            for ch in channels(40., 180., false, wave) {
                let c = lfo_curve(wave, 0.3, ch, 5, 128);
                assert!(c.iter().all(|v| (-1.0001..=1.0001).contains(v)), "{wave:?}");
            }
        }
        // A sine starting a quarter cycle in begins at its peak and peaks again each cycle.
        let c = lfo_curve(Waveform::Sine, 0., Channel { phase: 0.25, ratio: 1. }, 1, 201);
        assert!((c[0] - 1.).abs() < 1e-5 && (c[50] + 1.).abs() < 1e-4 && (c[100] - 1.).abs() < 1e-4 && (c[200] - 1.).abs() < 1e-4);
    }

    #[test]
    fn typed_spin_percent_means_degrees_of_a_half_turn() {
        use crate::bridge::{PluginCatalog, StubCatalog};
        use aitrack_model::{Device, DeviceId, Project};
        let cat = StubCatalog::default();
        let info = |id: u32| cat.device("fx.autopan").unwrap().params.iter().find(|i| i.id == id).unwrap().clone();
        let mut project = Project::new("t");
        let mut device = Device::stock(DeviceId(900), "fx.autopan", "Auto Pan");
        project.tracks[0].devices.push(device.clone());
        // Phase mode: degrees are degrees.
        assert_eq!(spin_scale(&cat, &project, DeviceId(900), p::STEREO), None);
        // Spin: 100 % is 180°, so typing the 50 shown means 90°, and back.
        device.params.insert(p::STEREO_MODE, info(p::STEREO_MODE).to_normalized(1.));
        project.tracks[0].devices[0] = device.clone();
        let k = spin_scale(&cat, &project, DeviceId(900), p::STEREO).unwrap();
        assert_eq!((50. * k, 90. / k), (90., 50.));
        // Only Stereo is shown in percent; random shapes have no spin.
        assert_eq!(spin_scale(&cat, &project, DeviceId(900), p::PHASE), None);
        device.params.insert(p::WAVE, info(p::WAVE).to_normalized(6.));
        project.tracks[0].devices[0] = device;
        assert_eq!(spin_scale(&cat, &project, DeviceId(900), p::STEREO), None);
    }

    #[test]
    fn handles_sit_on_the_peaks_and_follow_the_parameters() {
        let curve = |phase: f32| lfo_curve(Waveform::Sine, 0., channels(phase, 0., false, Waveform::Sine)[0], 11, PEAK_N);
        // Tremolo peaks where the LFO does, Pan where it is lowest.
        let (x, v) = peak_of(&curve(0.), true, false, false);
        assert!((x - 0.125).abs() < 0.01 && v > 0.99, "{x} {v}");
        let (x, v) = peak_of(&curve(0.), false, false, false);
        assert!((x - 0.375).abs() < 0.01 && v < -0.99, "{x} {v}");
        // Invert swaps them; the last peak is in the second cycle.
        assert!((peak_of(&curve(0.), true, true, false).0 - 0.375).abs() < 0.01);
        assert!((peak_of(&curve(0.), true, false, true).0 - 0.625).abs() < 0.01);
        // More Phase slides the curve left by phase / 720 of the plot: the
        // handle's x axis (full width = 720°, so -2 of the 0..360 range).
        let moved = peak_of(&curve(45.), true, false, true).0;
        assert!((moved - (0.625 - 45. / 720.)).abs() < 0.01, "{moved}");
        // Stereo in Phase mode moves the right peak by -stereo / 720 over its 180° range.
        let k = stereo_reach(0., 0., false, Waveform::Sine, 0., true, false).unwrap();
        assert!((k + 0.25).abs() < 0.02, "{k}");
        // Spin moves it too, much less; random shapes not at all.
        let spin = stereo_reach(0., 0., true, Waveform::Sine, 0., true, false).unwrap();
        assert!(spin.abs() < k.abs() && spin.abs() > 0.02, "{spin}");
        assert_eq!(stereo_reach(0., 0., false, Waveform::Random, 0., true, false), None);
    }

    #[test]
    fn the_field_marker_follows_the_louder_channel() {
        assert_eq!(balance(1., 1.), 0.);
        assert_eq!(balance(1., 0.), -1.);
        assert_eq!(balance(0., 1.), 1.);
        assert_eq!(balance(0., 0.), 0.);
        assert!(balance(1., 0.5) < 0.);
    }

    #[test]
    fn pan_trades_level_and_tremolo_dips() {
        // Pan: centred is unity, the extremes are +3 dB and silence.
        assert_eq!(gain(false, false, 1., 0.), 1.);
        assert!((gain(false, false, 1., -1.) - SQRT_2).abs() < 1e-5 && gain(false, false, 1., 1.) < 1e-5);
        // Tremolo: full at the top of the LFO, down by Amount at the bottom.
        assert_eq!(gain(true, false, 0.6, 1.), 1.);
        assert!((gain(true, false, 0.6, -1.) - 0.4).abs() < 1e-6);
        assert!(gain(true, true, 0.6, 0.) < gain(true, false, 0.6, 0.), "vintage dips deeper");
        // The gain plot keeps both inside its frame.
        assert!(gain_frac(false, SQRT_2) > 0. && gain_frac(true, 1.) > 0. && gain_frac(true, 0.) == 1.);
    }

    #[test]
    fn the_editor_asks_for_the_params_the_plugin_has() {
        let d = dev("fx.autopan", &[(p::STEREO_MODE, 1.), (p::STEREO, 90.)]);
        assert_eq!(d.choice(p::STEREO_MODE), 1);
        assert_eq!(d.plain(p::STEREO), 90.);
        for id in [p::MODE, p::WAVE, p::TIME_MODE, p::RATE, p::TIME, p::DIVISION, p::PHASE, p::AMOUNT, p::SHAPE, p::ATTACK, p::DYN_FM, p::HARMONIC, p::VINTAGE, p::MIX, p::OUTPUT] {
            assert!(d.info(id).is_some(), "{id}");
        }
    }
}
