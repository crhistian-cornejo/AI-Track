//! `midi.length`: the length of an example note before and after, in one
//! time axis. The first bar is the note as played (and, for legato, the
//! next note that cuts it), the second what the device makes of it for the
//! chosen mode, with the unclamped target as a ghost behind it and the Min
//! and Max caps as vertical lines through both. The end of the result is a
//! handle: it moves the time (fixed), the gate (sync) or the stretch.

use super::Dev;
use super::kit::{self, Handle, PlotCfg, ax};
use super::midi_kit as mk;
use crate::paint;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::midi::kit::{DIVISION_NAMES, division_beats};
use aitrack_stock::midi::length::p;
use gpui::{AnyElement, Context, FontWeight, Hsla, TextAlign, px};

const FIXED: usize = 0;
const SYNC: usize = 1;
const STRETCH: usize = 2;
const LEGATO: usize = 3;

/// The example note as played: a quarter note, and for legato the next one
/// starting an eighth after it ends.
const PLAYED_BEATS: f32 = 1.;
const NEXT_BEATS: f32 = 1.5;
/// How far a legato note may run on when Max is off (as the plugin).
const LEGATO_CAP_BEATS: f32 = 8.;

/// What the device makes of the example note, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub played: f32,
    pub out: f32,
    /// The target before Min and Max (the ghost).
    pub target: f32,
    /// The next note's start (legato only).
    pub next: Option<f32>,
    pub min: f32,
    /// 0 = off.
    pub max: f32,
}

/// The settings that decide the result, in working units.
#[derive(Clone, Copy, Debug)]
pub struct Cfg {
    pub mode: usize,
    /// The base length of a fixed or synced note, in ms.
    pub base: f32,
    pub gate: f32,
    pub stretch: f32,
    pub min: f32,
    pub max: f32,
    pub bpm: f64,
}

/// The result for a note played for `played` ms with the next one `next` ms
/// after its start, as the plugin does it.
pub fn shape(c: &Cfg, played: f32, next: f32) -> Shape {
    let capped = |v: f32| if c.max > 0. { v.min(c.max) } else { v };
    let (target, out, next) = match c.mode {
        FIXED | SYNC => {
            let target = c.base * c.gate;
            (target, capped(target.max(c.min)).max(1.), None)
        }
        STRETCH => {
            let target = played * c.stretch;
            // A release cannot be moved earlier than it came.
            (target, capped(target.max(c.min)).max(played), None)
        }
        _ => {
            let cap = if c.max > 0. { c.max } else { mk::beats_ms(LEGATO_CAP_BEATS, c.bpm) };
            (next, next.max(c.min).min(cap), Some(next))
        }
    };
    Shape { played, out, target, next, min: c.min, max: c.max }
}

/// A round tick spacing (ms) giving at most eight lines over `span`.
pub fn tick_step(span: f32) -> f32 {
    [5., 10., 20., 50., 100., 200., 500., 1_000., 2_000., 5_000., 10_000., 20_000.].into_iter().find(|s| span / s <= 8.).unwrap_or(50_000.)
}

fn cfg(dev: &Dev) -> Cfg {
    let mode = dev.choice(p::MODE).min(LEGATO);
    let base = if mode == FIXED { dev.plain(p::TIME) } else { mk::beats_ms(division_beats(dev.choice(p::DIVISION), (4, 4)) as f32, dev.bpm) };
    Cfg { mode, base, gate: dev.plain(p::GATE) / 100., stretch: dev.plain(p::STRETCH), min: dev.plain(p::MIN), max: dev.plain(p::MAX), bpm: dev.bpm }
}

/// The time axis: room for everything drawn, rounded up to the grid.
fn domain(s: &Shape) -> (f32, f32) {
    let widest = [s.played, s.out, s.target, s.next.unwrap_or(0.), s.min].into_iter().fold(1., f32::max) * 1.15;
    let step = tick_step(widest);
    ((widest / step).ceil() * step, step)
}

/// Vertical layout of the two bars: `(top of the played row, top of the result row, row height)`.
fn rows(h: f32) -> (f32, f32, f32) {
    const TOP: f32 = 20.;
    const GAP: f32 = 8.;
    let avail = h - TOP - 18.;
    let rh = ((avail - GAP) / 2.).clamp(12., 36.);
    let y1 = TOP + (avail - (2. * rh + GAP)).max(0.) / 2.;
    (y1, y1 + rh + GAP, rh)
}

fn summary(dev: &Dev, c: &Cfg) -> String {
    match c.mode {
        FIXED => format!("fixed · {} × {:.0}%", dev.text(p::TIME), c.gate * 100.),
        SYNC => format!("{} × {:.0}%", DIVISION_NAMES[dev.choice(p::DIVISION).min(DIVISION_NAMES.len() - 1)], c.gate * 100.),
        STRETCH => format!("played × {:.1}", c.stretch),
        _ => "until the next note".to_owned(),
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let c = cfg(dev);
    let s = shape(&c, mk::beats_ms(PLAYED_BEATS, dev.bpm), mk::beats_ms(NEXT_BEATS, dev.bpm));
    let (span, tick) = domain(&s);
    let h = mk::plot_h(dev);
    let (_, y2, rh) = rows(h);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    // The end of the result drags the parameter that sets it.
    let handle = match c.mode {
        FIXED => Some(Handle { fx: (s.out / span).min(1.), fy: (y2 + rh / 2.) / h, x: ax(p::TIME, span / s.out / 4_000f32.ln()), alt: ax(p::GATE, -1.), reset: vec![default(p::TIME)], ..Default::default() }),
        SYNC => Some(Handle { fx: (s.out / span).min(1.), fy: (y2 + rh / 2.) / h, x: ax(p::GATE, span / s.out / 80f32.ln()), reset: vec![default(p::GATE)], ..Default::default() }),
        STRETCH => Some(Handle { fx: (s.out / span).min(1.), fy: (y2 + rh / 2.) / h, x: ax(p::STRETCH, span / s.played / 7.), reset: vec![default(p::STRETCH)], ..Default::default() }),
        _ => None,
    };
    let marks: Vec<(f32, f32)> = handle.iter().map(|h| (h.fx, h.fy)).collect();
    let enabled = dev.enabled;
    let left = summary(dev, &c);
    let played_text = format!("example note {}", mk::fmt_ms(s.played));

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles: handle.into_iter().collect(), keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let lit = |c: Hsla| if enabled { c } else { pal.muted };
        let (y1, y2, rh) = rows(r.h);
        let x_of = |ms: f32| r.x + r.w * (ms / span).min(1.);
        // The time grid.
        let mut t = 0.;
        while t <= span + 1e-3 {
            let x = r.x + r.w * t / span;
            kit::vline(window, r, x, if t == 0. { pal.grid_bar } else { pal.grid_beat });
            kit::txt(window, cx, &format!("{t:.0}"), x + 3., r.bottom() - 13., TEXT_XS, pal.faint, TextAlign::Left);
            t += tick;
        }
        kit::txt(window, cx, "ms", r.right() - 6., r.bottom() - 13., TEXT_XS, pal.faint, TextAlign::Right);
        let head = paint::text(window, cx, &left, px(r.x + 6.), px(r.y + 3.), TEXT_XS, pal.muted, FontWeight::MEDIUM, TextAlign::Left);
        // A bar's label: after its end when there is room, else inside it.
        let name = |window: &mut gpui::Window, cx: &mut gpui::App, text: &str, end: f32, y: f32, fill: Hsla| {
            let width = text.len() as f32 * 5.6;
            if end + 6. + width < r.right() {
                kit::txt(window, cx, text, end + 6., y + rh / 2. - 6., TEXT_XS, pal.muted, TextAlign::Left);
            } else {
                kit::txt(window, cx, text, end - 5., y + rh / 2. - 6., TEXT_XS, kit::ink_on(fill), TextAlign::Right);
            }
        };
        // The note as played, and the one that follows it.
        let played = with_alpha(pal.text, 0.3);
        kit::bar(window, r.x, r.y + y1, (x_of(s.played) - r.x).max(2.), rh, 3., played);
        name(window, cx, &played_text, x_of(s.played), r.y + y1, played);
        if let Some(next) = s.next {
            kit::bar(window, x_of(next), r.y + y1, 3., rh, 1.5, lit(series(1)));
            kit::txt(window, cx, "next", x_of(next) + 7., r.y + y1 + rh / 2. - 6., TEXT_XS, pal.muted, TextAlign::Left);
        }
        // The result, over the target before the caps.
        let accent = lit(pal.accent);
        if (s.target - s.out).abs() > 0.5 {
            kit::bar(window, r.x, r.y + y2, (x_of(s.target) - r.x).max(2.), rh, 3., with_alpha(accent, 0.16));
        }
        kit::bar(window, r.x, r.y + y2, (x_of(s.out) - r.x).max(2.), rh, 3., with_alpha(accent, 0.75));
        let ink = with_alpha(accent, 0.75);
        name(window, cx, &format!("result {}", mk::fmt_ms(s.out)), x_of(s.out).max(r.x + 2.) + 8., r.y + y2, ink);
        // The caps, through both bars.
        let (top, bottom) = (r.y + y1 - 5., r.y + y2 + rh + 5.);
        let wide = |t: &str| t.len() as f32 * 5.6;
        let after_header = r.x + head + 14.;
        let min_label = (s.min > 0.).then(|| format!("min {}", mk::fmt_ms(s.min)));
        let mut min_end = after_header;
        if let Some(label) = &min_label {
            let x = x_of(s.min);
            kit::bar(window, x, top, 1.5, bottom - top, 0., lit(series(2)));
            // To the left of its line when there is room, else to the right.
            if x - 4. - wide(label) > after_header {
                kit::txt(window, cx, label, x - 4., r.y + 3., TEXT_XS, lit(series(2)), TextAlign::Right);
                min_end = x;
            } else {
                kit::txt(window, cx, label, x + 4., r.y + 3., TEXT_XS, lit(series(2)), TextAlign::Left);
                min_end = x + 4. + wide(label);
            }
        }
        if s.max > 0. {
            let beyond = s.max > span;
            let x = if beyond { r.right() - 2. } else { x_of(s.max) };
            kit::bar(window, x - 1., top, 1.5, bottom - top, 0., lit(series(3)));
            let label = format!("max {}{}", mk::fmt_ms(s.max), if beyond { " ›" } else { "" });
            if x + 4. + wide(&label) < r.right() - 4. {
                kit::txt(window, cx, &label, x + 4., r.y + 3., TEXT_XS, lit(series(3)), TextAlign::Left);
            } else if x - 5. - wide(&label) > min_end + 6. {
                kit::txt(window, cx, &label, x - 5., r.y + 3., TEXT_XS, lit(series(3)), TextAlign::Right);
            }
        }
        for (fx, fy) in &marks {
            kit::dot(window, r.pt(*fx, *fy), 6., accent, Some(pal.raised));
        }
    });

    let main = match c.mode {
        FIXED => p::TIME,
        STRETCH => p::STRETCH,
        _ => p::DIVISION,
    };
    let mut mode_card = vec![kit::ctl(ws, dev, p::MODE, cx)];
    if c.mode != LEGATO {
        mode_card.push(kit::ctl(ws, dev, main, cx));
    }
    if matches!(c.mode, FIXED | SYNC) {
        mode_card.push(kit::ctl(ws, dev, p::GATE, cx));
    }
    let limits = vec![kit::ctl(ws, dev, p::MIN, cx), kit::ctl(ws, dev, p::MAX, cx)];
    let cards = vec![kit::section(dev, "MODE", None, mode_card, cx), kit::section(dev, "LIMITS", None, limits, cx)];
    mk::body(dev, "length-body", 440., plot, cards)
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    fn at(mode: usize, f: impl Fn(&mut Cfg)) -> Shape {
        let mut c = Cfg { mode, base: 125., gate: 1., stretch: 2., min: 0., max: 0., bpm: 120. };
        f(&mut c);
        shape(&c, 500., 750.)
    }

    #[test]
    fn fixed_and_sync_replace_the_played_length_by_base_times_gate() {
        let s = at(FIXED, |c| c.gate = 0.5);
        assert_eq!((s.out, s.target, s.next), (62.5, 62.5, None));
        // Min lifts and Max caps, the target stays the ghost.
        let lifted = at(SYNC, |c| c.min = 200.);
        assert_eq!((lifted.out, lifted.target), (200., 125.));
        let capped = at(SYNC, |c| {
            c.gate = 4.;
            c.max = 300.;
        });
        assert_eq!((capped.out, capped.target), (300., 500.));
    }

    #[test]
    fn stretch_never_ends_before_the_release() {
        assert_eq!(at(STRETCH, |_| {}).out, 1_000.);
        // A cap below the played length leaves the release where it was.
        assert_eq!(at(STRETCH, |c| c.max = 100.).out, 500.);
        assert_eq!(at(STRETCH, |c| c.stretch = 1.).out, 500.);
        assert_eq!(at(STRETCH, |c| c.min = 1_200.).out, 1_200.);
    }

    #[test]
    fn legato_runs_to_the_next_note_within_its_cap() {
        let s = at(LEGATO, |_| {});
        assert_eq!((s.out, s.next), (750., Some(750.)));
        assert_eq!(at(LEGATO, |c| c.max = 600.).out, 600.);
        assert_eq!(at(LEGATO, |c| c.min = 900.).out, 900.);
        // Without Max the cap is eight beats (4 s at 120 bpm).
        let far = shape(&Cfg { mode: LEGATO, base: 0., gate: 1., stretch: 1., min: 0., max: 0., bpm: 120. }, 500., 9_000.);
        assert_eq!(far.out, 4_000.);
    }

    #[test]
    fn the_axis_holds_the_bars_and_rounds_to_its_grid() {
        let (span, step) = domain(&at(SYNC, |_| {}));
        assert!(span >= 143.75 && span % step == 0.);
        assert!(tick_step(span) == step);
        assert_eq!(tick_step(2_000.), 500.);
        for w in [3., 80., 800., 6_000., 100_000.] {
            assert!(w / tick_step(w) <= 8.01 || tick_step(w) == 50_000.);
        }
    }

    #[test]
    fn the_defaults_read_as_a_sixteenth_at_the_tempo() {
        let d = dev("midi.length", &[]);
        let c = cfg(&d);
        assert_eq!((c.mode, c.base), (SYNC, 125.));
        assert!((c.gate - 1.).abs() < 1e-5);
        assert_eq!(summary(&d, &c), "1/16 × 100%");
        let fixed = dev("midi.length", &[(p::MODE, 0.), (p::TIME, 80.)]);
        assert!((cfg(&fixed).base - 80.).abs() < 1e-2);
    }

    #[test]
    fn the_rows_fit_both_dock_sizes() {
        for h in [mk::PLOT_H - 2., mk::PLOT_H_BIG - 2.] {
            let (y1, y2, rh) = rows(h);
            assert!(y1 >= 20. && y2 > y1 + rh && y2 + rh <= h - 14., "{h}: {y1} {y2} {rh}");
        }
    }
}
