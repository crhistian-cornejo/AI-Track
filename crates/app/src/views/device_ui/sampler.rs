//! `instrument.sampler`: the sample's waveform with draggable start / end
//! markers (and loop markers in Loop mode), the playhead of the newest
//! voice, and the amp envelope with draggable points.

use super::kit::{self, Handle, PlotCfg, ax};
use super::wavetable::adsr;
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_model::Device;
use aitrack_stock::instrument::sampler::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

pub fn render(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (path, _) = kit::state_paths(d.state.as_deref());
    let wave = path.as_deref().map(|p| ws.fx.waves.get(p)).unwrap_or_default();
    let name = path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned());
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
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        if wave.is_empty() {
            kit::txt(window, cx, "No sample loaded", r.px(0.5), r.py(0.5) - 7., TEXT_SM, pal.faint, TextAlign::Center);
        } else {
            kit::paint_wave(window, r.inset(2.), &wave, if enabled { pal.waveform } else { pal.muted });
        }
        // What is outside start…end does not play.
        let (a, b) = (start.min(end), start.max(end));
        kit::bar(window, r.x, r.y, r.w * a, r.h, 0., shadow(0.45));
        kit::bar(window, r.px(b), r.y, r.w * (1. - b), r.h, 0., shadow(0.45));
        if looping {
            let (la, lb) = (loop_start.min(loop_end), loop_start.max(loop_end));
            kit::bar(window, r.px(la), r.y, r.w * (lb - la), r.h, 0., with_alpha(series(2), 0.12));
            for x in [loop_start, loop_end] {
                kit::vline(window, r, r.px(x), series(2));
                kit::dot(window, r.pt(x, 0.9), 5.5, series(2), Some(pal.text));
            }
        }
        for (x, text) in [(start, "S"), (end, "E")] {
            kit::vline(window, r, r.px(x), pal.accent);
            kit::dot(window, r.pt(x, 0.1), 6.5, pal.accent, Some(pal.text));
            kit::txt(window, cx, text, r.px(x), r.py(0.1) - 6.5, TEXT_XS, kit::ink_on(pal.accent), TextAlign::Center);
        }
        if playing {
            kit::vline(window, r, r.px((a + (b - a) * playhead).clamp(0., 1.)), pal.text);
        }
        if let Some(name) = &name {
            kit::txt(window, cx, name, r.right() - 6., r.bottom() - 15., TEXT_XS, pal.muted, TextAlign::Right);
        }
    });
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
