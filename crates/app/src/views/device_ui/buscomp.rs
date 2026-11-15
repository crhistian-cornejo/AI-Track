//! `fx.buscomp`: the bus compressor. Ratio, attack and release are
//! click-stops, so they are rows of steps (the release row ends in Auto and
//! shows the time it is using); next to them the transfer curve with a
//! draggable threshold and makeup, the level and gain-reduction history, and
//! a tall gain-reduction meter. The strip holds the continuous controls, the
//! sidechain (source, high-pass, listen) and the soft clip with its
//! "working hard" lamp.

use super::dynamics::trace;
use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption, set_now};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::envelope::gain_computer;
use aitrack_stock::fx::buscomp::{CEILING_DB, KNEE_DB, RELEASE_AUTO, SC_HPF_HZ, p};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, TextAlign, div,
    prelude::FluentBuilder, px,
};

/// Meter values the plugin publishes (see `fx::buscomp`).
const METER_LEN: usize = 70;
const M_LATENCY: usize = 67;
const M_CLIP: usize = 68;
const M_RELEASE_MS: usize = 69;

/// Bottom of both axes of the transfer plot, and the top of the output axis
/// (headroom for makeup), dB.
const LO: f32 = -60.;
const OUT_HI: f32 = 12.;
/// Gain reduction at the bottom of the meter, dB.
const GR_SPAN: f32 = 20.;

/// The static transfer of the device: output level for an input level, dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transfer {
    pub threshold: f32,
    pub ratio: f32,
    pub range: f32,
    pub makeup: f32,
}

impl Transfer {
    pub fn of(dev: &Dev) -> Self {
        let ratio = aitrack_stock::fx::buscomp::RATIOS[dev.choice(p::RATIO).min(2)];
        Self { threshold: dev.plain(p::THRESHOLD), ratio, range: dev.plain(p::RANGE), makeup: dev.plain(p::MAKEUP) }
    }

    /// Output level for a steady input level, dB (makeup included, as the
    /// plugin's own curve).
    pub fn out(&self, level: f32) -> f32 {
        level - gain_computer(level, self.threshold, self.ratio, KNEE_DB).min(self.range) + self.makeup
    }
}

/// Horizontal fraction of an input level.
fn fx_of(db: f32) -> f32 {
    ((db - LO) / -LO).clamp(0., 1.)
}

/// Vertical fraction (0 = top) of an output level.
fn fy_of(db: f32) -> f32 {
    1. - ((db - LO) / (OUT_HI - LO)).clamp(0., 1.)
}

/// Slot of the collapsible sidechain panel's state in the device's view options.
const SC_PANEL: u8 = 7;
/// Size of the sidechain filter's inset in the history.
const SC_W: f32 = 152.;
const SC_H: f32 = 40.;

/// Magnitude of the sidechain high-pass (2nd-order Butterworth) at `f`, dB;
/// `cutoff` 0 is Off.
pub fn hpf_db(cutoff: f32, f: f32) -> f32 {
    if cutoff <= 0. {
        return 0.;
    }
    let r4 = (f / cutoff).powi(4);
    10. * (r4 / (1. + r4)).max(1e-12).log10()
}

/// The SIDECHAIN fold: a header that shows or hides the response of the key's
/// high-pass, which is drawn over the history; Listen colours it.
fn sidechain_panel(ws: &Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let open = ws.fx.opt(dev.id, SC_PANEL, 1.) > 0.5;
    let device = dev.id;
    let hz = SC_HPF_HZ[dev.choice(p::SC_HPF).min(SC_HPF_HZ.len() - 1)];
    let head = div()
        .id(("bc-sc-panel", dev.key(0)))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .cursor_pointer()
        .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 10., pal.faint))
        .child(caption("SIDECHAIN"))
        .child(div().flex_1())
        .child(mono(if hz > 0. { format!("HPF {hz:.0} Hz") } else { "HPF off".to_owned() }, TEXT_XS, pal.faint))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            let was = this.fx.opt(device, SC_PANEL, 1.) > 0.5;
            this.fx.opts.insert((device, SC_PANEL), if was { 0. } else { 1. });
            cx.notify();
        }));
    head.into_any_element()
}

/// The key's high-pass response drawn into `r` (an inset of the history):
/// +3 dB at the top, -24 dB at the bottom, 20 Hz to 20 kHz across.
fn paint_hpf(r: Rect, hz: f32, listen: bool, window: &mut gpui::Window) {
    let pal = p();
    let color = if listen { pal.record } else { pal.accent };
    kit::bar(window, r.x, r.y, r.w, r.h, 4., with_alpha(pal.base, 0.85));
    for f in [100., 1_000., 10_000.] {
        kit::vline(window, r, r.px(kit::freq_frac(f)), pal.grid_beat);
    }
    let y = |db: f32| 1. - (db + 24.) / 27.;
    kit::hline(window, r, r.py(y(0.)), pal.grid_bar);
    let pts = kit::sample_curve(r, 64, |fx| y(hpf_db(hz, kit::frac_freq(fx))));
    kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.14));
    kit::line(window, &pts, 1.5, color);
    if hz > 0. {
        kit::vline(window, r, r.px(kit::freq_frac(hz)), with_alpha(pal.accent2, 0.7));
    }
}

/// A choice label without its unit ("0.3 ms" -> "0.3"): the row's caption
/// carries the unit.
pub fn step_label(choice: &str) -> &str {
    choice.split_whitespace().next().unwrap_or(choice)
}

/// A row of click-stops for the stepped parameter `id`.
fn steps(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let now = dev.choice(id);
    let device = dev.id;
    let mut row = div().flex_none().h(px(24.)).flex().items_center().gap(px(SP1)).p(px(SP1)).rounded(px(r_sm())).bg(pal.well);
    for (i, name) in info.choices.iter().enumerate() {
        let on = i == now;
        let norm = info.to_normalized(info.min + i as f32);
        row = row.child(
            div()
                .id(("bc-step", dev.key(id as usize * 16 + i)))
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.28) } else { gpui::transparent_black() })
                .hover(|d| d.bg(if on { accent_alpha(0.34) } else { content(0.1) }))
                .child(label(step_label(name).to_owned(), TEXT_XS, if on { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    set_now(this, device, id, norm);
                    cx.notify();
                })),
        );
    }
    row.into_any_element()
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let t = Transfer::of(dev);
    let live = dev.meter.len() >= METER_LEN;
    let gr = dev.m(0);
    let (peak_in, peak_out) = if live { (dev.m(1), dev.m(2)) } else { (-120., -120.) };
    let soft_clip = dev.on(p::SOFT_CLIP);
    let clip_hot = soft_clip && live && dev.m(M_CLIP) > 0.5;
    let enabled = dev.enabled;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let span = |id: u32| dev.info(id).map_or(1., |i| i.max - i.min);

    // Left: the transfer curve. The threshold slides along it; the makeup
    // handle (at -3 dB in) lifts the whole curve.
    let handles = vec![
        Handle { fx: fx_of(t.threshold), fy: fy_of(t.out(t.threshold)), x: ax(p::THRESHOLD, -LO / span(p::THRESHOLD)), index: 0, reset: vec![default(p::THRESHOLD)], ..Default::default() },
        Handle { fx: fx_of(-3.), fy: fy_of(t.out(-3.)), y: ax(p::MAKEUP, -(OUT_HI - LO) / span(p::MAKEUP)), index: 1, reset: vec![default(p::MAKEUP)], ..Default::default() },
    ];
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let curve = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::db_grid(window, cx, r, LO, OUT_HI, 12., false);
        let mut db = LO + 12.;
        while db < -0.1 {
            kit::vline(window, r, r.px(fx_of(db)), pal.grid_beat);
            db += 12.;
        }
        kit::line(window, &[r.pt(0., fy_of(LO)), r.pt(1., fy_of(0.))], 1., content(0.14));
        if soft_clip {
            kit::hline(window, r, r.py(fy_of(CEILING_DB)), with_alpha(pal.record, 0.5));
            kit::txt(window, cx, "clip", r.right() - 4., r.py(fy_of(CEILING_DB)) + 2., TEXT_XS, with_alpha(pal.record, 0.8), TextAlign::Right);
        }
        let pts = kit::sample_curve(r, 96, |x| fy_of(t.out(LO - LO * x)));
        kit::fill_to(window, &pts, r.bottom(), accent_alpha(0.1));
        kit::line(window, &pts, 2., if enabled { pal.accent } else { pal.muted });
        let tx = r.px(fx_of(t.threshold));
        kit::vline(window, r, tx, with_alpha(pal.accent2, 0.55));
        let label = format!("{:.1} dB", t.threshold);
        if tx > r.px(0.6) {
            kit::txt(window, cx, &label, tx - 4., r.y + 3., TEXT_XS, pal.muted, TextAlign::Right);
        } else {
            kit::txt(window, cx, &label, tx + 4., r.y + 3., TEXT_XS, pal.muted, TextAlign::Left);
        }
        // Where the signal sits on the curve right now.
        if peak_in > LO + 0.5 {
            kit::dot(window, r.pt(fx_of(peak_in), fy_of(t.out(peak_in))), 3.5, pal.text, None);
        }
        for (i, (fx, fy)) in marks.iter().enumerate() {
            kit::dot(window, r.pt(*fx, *fy), 7., series(i), Some(pal.text));
        }
    });

    // Middle-left: the three click-stop rows.
    let release_now = dev.choice(p::RELEASE);
    let auto = release_now == RELEASE_AUTO;
    let release_note = if auto && live && dev.m(M_RELEASE_MS) > 0. {
        Some(format!("auto, now {:.0} ms", dev.m(M_RELEASE_MS)))
    } else if auto {
        Some("auto".to_owned())
    } else {
        None
    };
    let selectors = div()
        .flex_none()
        .w(px(if dev.big { 232. } else { 200. }))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(caption("RATIO"))
        .child(steps(dev, p::RATIO, cx))
        .child(caption("ATTACK ms"))
        .child(steps(dev, p::ATTACK, cx))
        .child(div().flex().items_center().gap(px(SP2)).child(caption("RELEASE s")).child(div().flex_1()).when_some(release_note, |d, n| d.child(mono(n, TEXT_XS, pal.faint))))
        .child(steps(dev, p::RELEASE, cx))
        .child(div().flex_1())
        .child(sidechain_panel(ws, dev, cx));

    // Middle: input and output level with the gain reduction hanging from the top.
    let hist = dev.hist.clone();
    let region = ws.fx.region(dev.id, 1);
    let threshold = t.threshold;
    let latency = if soft_clip && live { dev.m(M_LATENCY) } else { 0. };
    let sc_open = ws.fx.opt(dev.id, SC_PANEL, 1.) > 0.5;
    let sc_hz = SC_HPF_HZ[dev.choice(p::SC_HPF).min(SC_HPF_HZ.len() - 1)];
    let sc_listen = dev.on(p::SC_LISTEN);
    let history = div().flex_1().min_w(px(0.)).h_full().rounded(px(r_md())).bg(pal.well).border_1().border_color(border()).overflow_hidden().child(
        gpui::canvas(
            move |b, _, _| region.set(b),
            move |b, _, window, cx| {
                let pal = p();
                let r = Rect::of(b);
                let h = hist.borrow();
                let lv = |db: f32| 1. - fx_of(db);
                kit::db_grid(window, cx, r, LO, 0., 12., true);
                let input = trace(r, &h.traces[1], lv);
                kit::fill_to(window, &input, r.bottom(), content(0.1));
                kit::line(window, &input, 1., content(0.3));
                kit::line(window, &trace(r, &h.traces[2], lv), 1.5, pal.accent2);
                kit::hline(window, r, r.py(lv(threshold)), with_alpha(pal.accent2, 0.5));
                let reduction = trace(r, &h.traces[0], |db| (-db / (2. * GR_SPAN)).min(0.5));
                kit::fill_to(window, &reduction, r.y, with_alpha(pal.record, 0.35));
                kit::line(window, &reduction, 1.5, pal.record);
                if sc_open {
                    paint_hpf(Rect { x: r.right() - SC_W - 6., y: r.bottom() - SC_H - 6., w: SC_W, h: SC_H }, sc_hz, sc_listen, window);
                }
                // The soft clip's state: off, on, or working hard.
                if soft_clip {
                    kit::dot(window, gpui::point(gpui::px(r.x + 10.), gpui::px(r.bottom() - 10.)), 3., if clip_hot { pal.record } else { with_alpha(pal.record, 0.3) }, None);
                    let text = if latency > 0. { format!("soft clip · {latency:.0} smp") } else { "soft clip".to_owned() };
                    kit::txt(window, cx, &text, r.x + 18., r.bottom() - 16., TEXT_XS, if clip_hot { pal.record } else { pal.faint }, TextAlign::Left);
                }
            },
        )
        .size_full(),
    );

    // Right: the gain reduction now, as a tall bar with a scale.
    let meter = div()
        .flex_none()
        .w(px(56.))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SP1))
        .child(mono(format!("{gr:.1}"), TEXT_LG, if gr < -0.05 { pal.text } else { pal.faint }))
        .child(
            div().flex_1().w_full().rounded(px(3.)).bg(pal.well).border_1().border_color(border()).overflow_hidden().child(
                gpui::canvas(
                    |_, _, _| {},
                    move |b, _, window, cx| {
                        let pal = p();
                        let r = Rect::of(b);
                        for db in [3., 6., 10., 15.] {
                            let y = r.py(db / GR_SPAN);
                            kit::hline(window, Rect { w: 8., ..r }, y, pal.grid_bar);
                            kit::txt(window, cx, &format!("{db:.0}"), r.x + 12., y - 6., TEXT_XS, pal.faint, TextAlign::Left);
                        }
                        let x = r.right() - 20.;
                        kit::bar(window, x, r.y, 16., (-gr / GR_SPAN).clamp(0., 1.) * r.h, 0., pal.record);
                    },
                )
                .size_full(),
            ),
        )
        .child(label("GR dB", TEXT_XS, pal.faint))
        .child(mono(if live { format!("{:>5} in", kit::fmt_db(peak_in)) } else { "—".to_owned() }, TEXT_XS, pal.faint))
        .child(mono(if live { format!("{:>5} out", kit::fmt_db(peak_out)) } else { "—".to_owned() }, TEXT_XS, if live && peak_out > CEILING_DB { pal.record } else { pal.faint }));

    let sidechain = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(super::sidechain_chip(ws, dev, p::SIDECHAIN, cx))
        .child(caption("HPF"))
        .child(kit::pick(dev, p::SC_HPF, cx))
        .child(kit::pick(dev, p::SC_LISTEN, cx))
        .child(kit::pick(dev, p::SOFT_CLIP, cx));

    kit::panel(if dev.big { 860. } else { 700. })
        .child(div().flex_1().min_h(px(0.)).flex().gap(px(SP2)).child(div().flex_none().w(px(if dev.big { 232. } else { 192. })).h_full().child(curve)).child(selectors).child(history).child(meter))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(kit::strip(ws, dev, &[p::THRESHOLD, p::RANGE, p::MAKEUP, p::MIX, p::OUTPUT], &[], cx))
                .child(div().flex_1())
                .child(sidechain),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use aitrack_stock::test_util::{process_stereo, set_plain};

    #[test]
    fn the_curve_is_the_plugins_static_curve() {
        for plain in [
            vec![],
            vec![(p::THRESHOLD, -24.), (p::RATIO, 2.), (p::MAKEUP, 6.)],
            vec![(p::THRESHOLD, -30.), (p::RATIO, 1.), (p::RANGE, 8.), (p::MAKEUP, 3.)],
        ] {
            let mut plugin = aitrack_stock::create("fx.buscomp").unwrap();
            for (id, v) in &plain {
                set_plain(plugin.as_mut(), *id, *v);
            }
            process_stereo(plugin.as_mut(), |_| (0., 0.), 2048);
            let mut m = [0.0f32; 80];
            assert_eq!(plugin.meter(&mut m), METER_LEN);
            let t = Transfer::of(&dev("fx.buscomp", &plain));
            for (i, plugin_db) in m[3..67].iter().enumerate() {
                let level = -60. + 60. * i as f32 / 63.;
                assert!((t.out(level) - plugin_db).abs() < 0.05, "{plain:?} at {level:.1} dB: drawn {:.2}, plugin {plugin_db:.2}", t.out(level));
            }
        }
    }

    #[test]
    fn the_steps_name_the_plugins_choices() {
        let d = dev("fx.buscomp", &[]);
        let names = |id: u32| d.info(id).unwrap().choices.iter().map(|c| step_label(c)).collect::<Vec<_>>();
        assert_eq!(names(p::RATIO), ["2:1", "4:1", "10:1"]);
        assert_eq!(names(p::ATTACK), ["0.01", "0.1", "0.3", "1", "3", "10", "30"]);
        assert_eq!(names(p::RELEASE), ["0.1", "0.2", "0.4", "0.6", "0.8", "1.2", "Auto"]);
        // A step writes the normalised value of its own index.
        let info = d.info(p::RELEASE).unwrap();
        assert_eq!(info.to_plain(info.to_normalized(info.min + RELEASE_AUTO as f32)).round() as usize, RELEASE_AUTO);
    }

    #[test]
    fn the_sidechain_filter_is_a_second_order_high_pass() {
        // -3 dB at the corner, 12 dB per octave below it, nothing when Off.
        assert!((hpf_db(90., 90.) + 3.01).abs() < 0.01);
        assert!((hpf_db(90., 45.) - hpf_db(90., 22.5) - 12.).abs() < 1.5);
        assert!(hpf_db(90., 5_000.).abs() < 0.01);
        assert_eq!(hpf_db(0., 20.), 0.);
        assert_eq!(SC_HPF_HZ[0], 0.);
    }

    #[test]
    fn the_plot_axes_map_levels_to_fractions() {
        assert_eq!(fx_of(-60.), 0.);
        assert_eq!(fx_of(0.), 1.);
        assert_eq!(fy_of(-60.), 1.);
        assert_eq!(fy_of(OUT_HI), 0.);
        assert!(fy_of(0.) > 0. && fy_of(0.) < fy_of(-12.));
    }
}

