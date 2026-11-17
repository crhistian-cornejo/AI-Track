//! `instrument.sampler`: the sample's waveform with draggable start / end
//! markers (and loop markers in Loop mode), the playhead of the newest
//! voice, and the amp envelope with draggable points.

use super::filter::{db_frac, paint_response};
use super::instr;
use super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::wavetable::adsr;
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode};
use aitrack_model::Device;
use aitrack_stock::instrument::sampler::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, point, px};

/// The sample's waveform with its start / end (and loop) markers.
fn wave_plot(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let (path, _) = kit::state_paths(d.state.as_deref());
    let sfz_path = kit::state_sfz_path(d.state.as_deref());
    let zone_count = dev.m(3).round() as usize;
    let wave = path.as_deref().map(|p| ws.fx.waves.get(p)).unwrap_or_default();
    let name = sfz_path
        .as_deref()
        .or(path.as_deref())
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let looping = dev.choice(p::MODE) == 2;
    let frac = |id: u32| dev.plain(id) / 100.;
    let (start, end, loop_start, loop_end) = (frac(p::START), frac(p::END), frac(p::LOOP_START), frac(p::LOOP_END));
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let marker = |id: u32, at: f32, fy: f32, index: usize| Handle { fx: at, fy, x: ax(id, 1.), index, reset: vec![default(id)], ..Default::default() };
    let mut handles = vec![marker(p::START, start, 0.1, 0), marker(p::END, end, 0.1, 1)];
    if looping {
        handles.push(marker(p::LOOP_START, loop_start, 0.9, 2));
        handles.push(marker(p::LOOP_END, loop_end, 0.9, 3));
    }
    let (playing, playhead, enabled) = (dev.m(0) > 0.5, dev.m(1), dev.enabled);
    kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        if wave.is_empty() {
            let empty_msg = if zone_count > 0 {
                format!("SFZ instrument · {zone_count} zones mapped")
            } else if sfz_path.is_some() {
                "SFZ multi-zone instrument loaded".to_string()
            } else {
                "No sample loaded".to_string()
            };
            kit::txt(window, cx, &empty_msg, r.px(0.5), r.py(0.5) - 7., TEXT_SM, if zone_count > 0 { pal.accent } else { pal.faint }, TextAlign::Center);
        } else {
            kit::paint_wave(window, r.inset(2.), &wave, if enabled { pal.waveform } else { pal.muted });
        }
        // What is outside start…end does not play.
        let (a, b) = (start.min(end), start.max(end));
        kit::bar(window, r.x, r.y, r.w * a, r.h, 0., shadow(0.45));
        kit::bar(window, r.px(b), r.y, r.w * (1. - b), r.h, 0., shadow(0.45));
        // The marker dots stay whole at the edges (the line is at the true place).
        let at = |x: f32| r.px(x).clamp(r.x + 9., r.right() - 9.);
        if looping {
            let (la, lb) = (loop_start.min(loop_end), loop_start.max(loop_end));
            kit::bar(window, r.px(la), r.y, r.w * (lb - la), r.h, 0., with_alpha(series(2), 0.12));
            for x in [loop_start, loop_end] {
                kit::vline(window, r, r.px(x), series(2));
                kit::dot(window, point(px(at(x)), px(r.py(0.9))), 5.5, series(2), Some(pal.text));
            }
        }
        for (x, text) in [(start, "S"), (end, "E")] {
            kit::vline(window, r, r.px(x), pal.accent);
            kit::dot(window, point(px(at(x)), px(r.py(0.1))), 6.5, pal.accent, Some(pal.text));
            kit::txt(window, cx, text, at(x), r.py(0.1) - 6.5, TEXT_XS, kit::ink_on(pal.accent), TextAlign::Center);
        }
        if playing {
            kit::vline(window, r, r.px((a + (b - a) * playhead).clamp(0., 1.)), pal.text);
        }
        if let Some(name) = &name {
            kit::txt(window, cx, name, r.right() - 6., r.bottom() - 15., TEXT_XS, pal.muted, TextAlign::Right);
        }
    })
    .into_any_element()
}

pub fn render(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    if kit::in_window() {
        return window(ws, dev, d, cx);
    }
    let plot = wave_plot(ws, dev, d, cx);
    let envelope = adsr(ws, dev, 1, [p::ATTACK, p::DECAY, p::SUSTAIN, p::RELEASE], pal.accent, cx);
    kit::panel(if dev.big { 820. } else { 560. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_1().min_w(px(0.)).h_full().child(plot))
                .child(div().flex_none().w(px(if dev.big { 240. } else { 176. })).h_full().flex().flex_col().gap(px(SP1)).child(caption("AMP ENVELOPE")).child(div().flex_1().min_h(px(0.)).child(envelope))),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, p::MODE, cx))
                .child(kit::pick(dev, p::FILTER_TYPE, cx))
                .child(kit::strip(
                    ws,
                    dev,
                    &[p::ROOT_KEY, p::TRANSPOSE, p::FINE, p::CUTOFF, p::RESONANCE, p::VOLUME],
                    &[p::FILTER_ENV, p::KEY_TRACK, p::CROSSFADE, p::VEL_SENS, p::PAN, p::GLIDE, p::POLYPHONY, p::BEND_RANGE],
                    cx,
                )),
        )
        .into_any_element()
}
/// Width of the column with the envelope and the filter.
const SIDE_W: f32 = 320.;

// ───────────────────────────── Window ─────────────────────────────

/// The sections the sampler's filter runs for a type (`FILTER_TYPES` order)
/// at a cutoff and a resonance (0..1); none when it is off.
pub fn filter_sections(kind: usize, cutoff: f32, resonance: f32, sample_rate: f64) -> Vec<SvfCoeffs> {
    let mode = match kind {
        1 => SvfMode::Lowpass,
        2 => SvfMode::Highpass,
        3 => SvfMode::Bandpass,
        _ => return Vec::new(),
    };
    let q = std::f32::consts::FRAC_1_SQRT_2 * (20f32 / std::f32::consts::FRAC_1_SQRT_2).powf(resonance.clamp(0., 1.));
    vec![SvfCoeffs::new(mode, cutoff, q, 0., sample_rate)]
}

/// The filter's response with a node: cutoff sideways, resonance up and down.
fn filter_plot(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let (kind, cutoff, reso) = (dev.choice(p::FILTER_TYPE), dev.plain(p::CUTOFF), dev.plain(p::RESONANCE) / 100.);
    let secs = filter_sections(kind, cutoff, reso, dev.sample_rate);
    let fx = freq_frac(cutoff);
    let fy = if secs.is_empty() { db_frac(0.) } else { db_frac(aitrack_dsp::response::svf_cascade_db(&secs, cutoff as f64, dev.sample_rate) as f32).clamp(0.06, 0.94) };
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![Handle {
        fx,
        fy,
        x: ax(p::CUTOFF, dev.info(p::CUTOFF).map_or(1., kit::log_span)),
        y: ax(p::RESONANCE, -1.1),
        alt: ax(p::FILTER_ENV, -1.),
        index: 0,
        reset: vec![default(p::CUTOFF), default(p::RESONANCE)],
    }];
    let (sr, enabled) = (dev.sample_rate, dev.enabled);
    let off = secs.is_empty();
    let color = if enabled && !off { p().accent } else { p().faint };
    kit::plot(ws, dev, PlotCfg { slot: 2, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        paint_response(window, cx, r, &secs, sr, color, r.w > 260.);
        if off {
            kit::txt(window, cx, "Filter off", r.px(0.5), r.py(0.5) - 24., TEXT_SM, pal.faint, TextAlign::Center);
        } else {
            kit::dot(window, r.pt(fx, fy), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
        }
    })
    .into_any_element()
}

fn window(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (path, _) = kit::state_paths(d.state.as_deref());
    let sfz = kit::state_sfz_path(d.state.as_deref());
    let zones = dev.m(3).round() as usize;
    let voices = dev.m(0);
    let name = sfz.as_deref().or(path.as_deref()).and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned());
    let (a, b) = (dev.plain(p::START), dev.plain(p::END));
    let wave = wave_plot(ws, dev, d, cx);
    let envelope = adsr(ws, dev, 1, [p::ATTACK, p::DECAY, p::SUSTAIN, p::RELEASE], pal.accent, cx);
    let filter = filter_plot(ws, dev, cx);
    let side = div()
        .flex_none()
        .w(px(SIDE_W))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP3))
        .child(div().flex_1().min_h(px(0.)).flex().flex_col().gap(px(SP1)).child(caption("AMP ENVELOPE")).child(instr::stage(72.).child(envelope)))
        .child(div().flex_1().min_h(px(0.)).flex().flex_col().gap(px(SP1)).child(caption("FILTER")).child(instr::stage(72.).child(filter)));
    let controls = instr::groups(vec![
        instr::group("PLAYBACK", pal.accent, instr::dials(dev, &[p::MODE, p::CROSSFADE], cx)),
        instr::group("TUNING", pal.accent, instr::dials(dev, &[p::ROOT_KEY, p::TRANSPOSE, p::FINE, p::KEY_TRACK], cx)),
        instr::group("AMP", pal.accent, instr::dials(dev, &[p::ATTACK, p::DECAY, p::SUSTAIN, p::RELEASE], cx)),
        instr::group("FILTER", pal.accent2, instr::dials(dev, &[p::FILTER_TYPE, p::CUTOFF, p::RESONANCE, p::FILTER_ENV], cx)),
        instr::group("VOICES", pal.muted, instr::dials(dev, &[p::VOLUME, p::PAN, p::VEL_SENS, p::GLIDE, p::POLYPHONY, p::BEND_RANGE], cx)),
    ]);
    let title = if zones > 0 { format!("SAMPLER · SFZ INSTRUMENT, {zones} ZONES") } else { "SAMPLER".to_owned() };
    instr::root(960.)
        .child(instr::title(
            title,
            vec![
                instr::tag("SAMPLE", name.unwrap_or_else(|| "none".to_owned()), pal.accent, false),
                instr::tag("PLAYS", format!("{a:.0}% – {b:.0}%"), pal.accent2, false),
                instr::tag("VOICES", format!("{voices:.0}"), pal.accent2, voices > 0.5),
            ],
        ))
        .child(div().flex_1().min_h(px(0.)).flex().gap(px(SP3)).child(instr::stage(160.).child(wave)).child(side))
        .child(controls)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn the_filter_curve_follows_the_type() {
        assert!(filter_sections(0, 1_000., 0.2, 48_000.).is_empty(), "off draws no filter");
        for kind in 1..=3 {
            assert_eq!(filter_sections(kind, 1_000., 0.2, 48_000.).len(), 1);
        }
        let d = dev("instrument.sampler", &[(p::FILTER_TYPE, 1.), (p::CUTOFF, 800.)]);
        assert_eq!(d.choice(p::FILTER_TYPE), 1);
        assert!((d.plain(p::CUTOFF) - 800.).abs() < 2.);
    }
}
