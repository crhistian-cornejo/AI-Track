//! The level meters in the window's frame: input on the left, gain
//! reduction and output on the right, with a peak tick that waits and falls.
//!
//! A device that publishes its input and output peaks (see [`super::io`])
//! gets those. Every other device shows the level of its track, stereo, on
//! the right, labelled as the track's: that is the level of what the chain
//! sends on, not of the device alone.

use super::io::{self, CEIL_DB, FLOOR_DB, Hold, Reading, level_frac, reduction_frac};
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::views::device_ui::kit;
use crate::views::device_window::placement::RAIL_W;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, DeviceKind};
use gpui::{AnyElement, IntoElement, ParentElement, Styled, TextAlign, canvas, div, px};
use std::time::Instant;

/// Which peak tick is which.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tick {
    Input,
    Output,
    Reduction,
    TrackL,
    TrackR,
}

/// The levels the frame shows this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Levels {
    pub reading: Reading,
    /// The track's left and right level in dB, for devices that publish none.
    pub track: Option<(f32, f32)>,
    /// The device's own output peaks, left and right, in dB (the utility publishes both).
    pub pair: Option<(f32, f32)>,
}

impl Levels {
    /// Whether the device published an input or an output level of its own.
    pub fn has_own_levels(&self) -> bool {
        self.reading.input_db.is_some() || self.reading.output_db.is_some() || self.pair.is_some()
    }
}

/// The plugin id of a stock device.
pub fn plugin_of(ws: &Workspace, device: DeviceId) -> Option<String> {
    match &ws.s.project.device(device)?.kind {
        DeviceKind::Stock { plugin_id } => Some(plugin_id.clone()),
        _ => None,
    }
}

/// Collect what the meters show now: the device's own when its plugin
/// publishes them, the track's otherwise.
pub fn gather(ws: &Workspace, dev: &Dev, plugin: &str) -> Levels {
    let reading = io::map(plugin).map(|m| io::read(&m, &dev.meter)).unwrap_or_default();
    // The utility publishes its left and right output peaks (linear) ahead of the correlation.
    let pair = (plugin == "fx.utility" && dev.meter.len() >= 3).then(|| (io::linear_to_db(dev.meter[0]), io::linear_to_db(dev.meter[1])));
    let mut levels = Levels { reading, track: None, pair };
    if !levels.has_own_levels() {
        let track = ws.s.project.device_track(dev.id);
        let m = track.map(|t| ws.b.engine.meter(t)).unwrap_or_default();
        levels.track = Some((io::linear_to_db(m.left), io::linear_to_db(m.right)));
    }
    levels
}

/// A tick's held value after feeding it `db` (adds it on first use).
pub fn hold(state: &mut super::chrome::ChromeState, tick: Tick, db: f32, now: Instant) -> f32 {
    state.holds.entry(tick).or_insert_with(|| Hold::new(now)).update(db, now)
}

/// One bar of a rail: its level and the held peak above it (dB).
#[derive(Clone, Copy)]
struct Bar {
    db: f32,
    held: f32,
}

fn readout_color(held: f32) -> gpui::Hsla {
    let pal = p();
    if held > -0.1 {
        pal.record
    } else if held > -6. {
        pal.meter_mid
    } else {
        pal.muted
    }
}

fn db_text(db: f32) -> String {
    if db <= -99. { "-inf".to_owned() } else { format!("{db:.1}") }
}

/// A level rail: a title, the bars with a dB scale beside them, the held peak below.
fn level_rail(title: &str, bars: &[Bar]) -> gpui::Div {
    let pal = p();
    let bars: Vec<Bar> = bars.to_vec();
    let top = bars.iter().map(|b| b.held).fold(-120., f32::max);
    let meter_style = style().meter;
    let scale = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let r = kit::Rect::of(b);
            let (label_w, gap) = (16., 2.);
            let bars_x = r.x + label_w;
            let n = bars.len().max(1) as f32;
            let bw = ((r.w - label_w - gap * (n - 1.)) / n).max(2.);
            let (y_of, h_of) = (|db: f32| r.y + r.h * (1. - level_frac(db)), |db: f32| r.h * level_frac(db));
            let zones = [(FLOOR_DB, -18., pal.meter_low), (-18., -6., pal.meter_mid), (-6., CEIL_DB, pal.meter_high)];
            for (i, bar) in bars.iter().enumerate() {
                let x = bars_x + i as f32 * (bw + gap);
                kit::bar(window, x, r.y, bw, r.h, 2., content(0.07));
                if bar.db > FLOOR_DB {
                    for &(lo, hi, color) in &zones {
                        if bar.db <= lo {
                            break;
                        }
                        let top_db = bar.db.min(hi);
                        let color = if meter_style == MeterStyle::Solid { pal.accent } else { color };
                        kit::bar(window, x, y_of(top_db), bw, h_of(top_db) - h_of(lo), 0., color);
                    }
                }
                if bar.held > FLOOR_DB {
                    let y = y_of(bar.held);
                    kit::bar(window, x, y - 1., bw, 2., 0., readout_color(bar.held));
                }
            }
            for db in [0., -6., -12., -24., -48.] {
                let y = y_of(db);
                kit::bar(window, bars_x, y, r.w - label_w, 1., 0., with_alpha(pal.well, 0.55));
                if db == 0. || db == -12. || db == -24. || db == -48. {
                    kit::txt(window, cx, &format!("{db:.0}"), r.x + label_w - 3., y - 6., TEXT_XS - 1., pal.faint, TextAlign::Right);
                }
            }
        },
    )
    .flex_1()
    .min_h(px(0.))
    .w_full();
    div()
        .flex_none()
        .w(px(RAIL_W))
        .h_full()
        .py(px(SP2))
        .px(px(SP1))
        .gap(px(SP1))
        .flex()
        .flex_col()
        .items_center()
        .child(label(title.to_owned(), TEXT_XS, pal.faint))
        .child(scale)
        .child(mono(db_text(top), TEXT_XS, readout_color(top)))
}

/// The gain-reduction rail: it hangs from the top and shows how much is being taken off.
fn reduction_rail(db: f32, held: f32) -> gpui::Div {
    let pal = p();
    let scale = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let r = kit::Rect::of(b);
            let label_w = 16.;
            let x = r.x + label_w;
            let w = (r.w - label_w).max(2.);
            kit::bar(window, x, r.y, w, r.h, 2., content(0.07));
            if db > 0.05 {
                kit::bar(window, x, r.y, w, r.h * reduction_frac(db), 0., pal.record);
            }
            if held > 0.05 {
                let y = r.y + r.h * reduction_frac(held);
                kit::bar(window, x, (y - 2.).max(r.y), w, 2., 0., pal.text);
            }
            for mark in [3., 6., 12., 18.] {
                let y = r.y + r.h * reduction_frac(mark);
                kit::bar(window, x, y, w, 1., 0., with_alpha(pal.well, 0.55));
                if mark != 3. {
                    kit::txt(window, cx, &format!("{mark:.0}"), r.x + label_w - 3., y - 6., TEXT_XS - 1., pal.faint, TextAlign::Right);
                }
            }
        },
    )
    .flex_1()
    .min_h(px(0.))
    .w_full();
    div()
        .flex_none()
        .w(px(RAIL_W))
        .h_full()
        .py(px(SP2))
        .px(px(SP1))
        .gap(px(SP1))
        .flex()
        .flex_col()
        .items_center()
        .child(label("GR", TEXT_XS, pal.faint))
        .child(scale)
        .child(mono(if held > 0.05 { format!("-{held:.1}") } else { "0.0".to_owned() }, TEXT_XS, if db > 0.05 { pal.record } else { pal.muted }))
}

/// The rails left of the editor (the device's input).
pub fn left(ws: &mut Workspace, device: DeviceId, levels: &Levels, now: Instant) -> Option<AnyElement> {
    let input = levels.reading.input_db?;
    let state = ws.fx.chrome.entry(device).or_default();
    let held = hold(state, Tick::Input, input, now);
    Some(level_rail("IN", &[Bar { db: input, held }]).border_r_1().border_color(border()).into_any_element())
}

/// The rails right of the editor: gain reduction, then the output (or the track's level).
pub fn right(ws: &mut Workspace, device: DeviceId, levels: &Levels, now: Instant) -> Option<AnyElement> {
    let state = ws.fx.chrome.entry(device).or_default();
    let mut rails = div().flex_none().h_full().flex().border_l_1().border_color(border());
    let mut any = false;
    if let Some(gr) = levels.reading.reduction_db {
        let held = hold(state, Tick::Reduction, gr, now);
        rails = rails.child(reduction_rail(gr, held));
        any = true;
    }
    if let Some(out) = levels.reading.output_db {
        let held = hold(state, Tick::Output, out, now);
        rails = rails.child(level_rail("OUT", &[Bar { db: out, held }]));
        any = true;
    } else if let Some((l, r)) = levels.pair {
        let (hl, hr) = (hold(state, Tick::TrackL, l, now), hold(state, Tick::TrackR, r, now));
        rails = rails.child(level_rail("OUT", &[Bar { db: l, held: hl }, Bar { db: r, held: hr }]));
        any = true;
    } else if let Some((l, r)) = levels.track {
        let (hl, hr) = (hold(state, Tick::TrackL, l, now), hold(state, Tick::TrackR, r, now));
        rails = rails.child(level_rail("TRACK", &[Bar { db: l, held: hl }, Bar { db: r, held: hr }]));
        any = true;
    }
    any.then(|| rails.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_that_publishes_nothing_falls_back_to_the_track() {
        let levels = Levels { reading: Reading::default(), track: Some((-12., -14.)), pair: None };
        assert!(!levels.has_own_levels());
        let own = Levels { reading: Reading { input_db: Some(-9.), ..Reading::default() }, track: None, pair: None };
        assert!(own.has_own_levels());
        // The utility's left/right peaks count as its own levels.
        assert!(Levels { pair: Some((-6., -7.)), ..Levels::default() }.has_own_levels());
        assert_eq!(db_text(-120.), "-inf");
        assert_eq!(db_text(-6.04), "-6.0");
    }
}
