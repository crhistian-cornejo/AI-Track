//! Arrangement navigation maths: zoom limits, the ruler's labels, zoom to
//! selection and following the playhead. Pure functions, tested here.

use crate::state::ArrangeView;
use aitrack_model::{PPQ, Tick, TimeSignature};

/// Arrangement zoom limits, in pixels per quarter note. At the maximum a
/// 1/128 note is 20 px wide, so the adaptive grid reaches its finest step.
pub const MIN_PX_PER_BEAT: f32 = 1.;
pub const MAX_PX_PER_BEAT: f32 = 640.;

/// Closest two ruler labels may sit, in pixels.
const LABEL_SPACING: f32 = 52.;

/// The distance between ruler labels at this zoom: sixteenths or beats when
/// zoomed in, then 1, 2, 4, 8… bars as the view zooms out.
pub fn label_step(px_per_beat: f32, sig: TimeSignature) -> Tick {
    let bar = sig.bar_ticks().max(1);
    let beat = (PPQ * 4 / i64::from(sig.denominator.max(1))).max(1);
    let wide = |step: Tick| step as f32 * px_per_beat / PPQ as f32 >= LABEL_SPACING;
    for step in [beat / 4, beat] {
        if step > 0 && bar % step == 0 && wide(step) {
            return step;
        }
    }
    let mut step = bar;
    while !wide(step) && step < bar * 4096 {
        step *= 2;
    }
    step
}

/// A position as the ruler writes it: `3` on a bar line, `3.2` on a beat,
/// `3.2.3` on a sixteenth of the beat (all 1-based).
pub fn position_label(tick: Tick, sig: TimeSignature) -> String {
    let bar = sig.bar_ticks().max(1);
    let beat = (PPQ * 4 / i64::from(sig.denominator.max(1))).max(1);
    let in_bar = tick.rem_euclid(bar);
    let (n, b, rest) = (tick.div_euclid(bar) + 1, in_bar / beat + 1, in_bar % beat);
    if in_bar == 0 {
        format!("{n}")
    } else if rest == 0 {
        format!("{n}.{b}")
    } else {
        format!("{n}.{b}.{}", rest * 4 / beat + 1)
    }
}

/// `view` zoomed by `factor` with the tick under `anchor_px` (pixels from
/// the left edge of the lanes) staying where it is.
pub fn zoom_at(view: ArrangeView, factor: f32, anchor_px: f32) -> ArrangeView {
    let px_per_beat = (view.px_per_beat * factor).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
    let beat = (view.scroll_x + anchor_px) / view.px_per_beat;
    ArrangeView { px_per_beat, scroll_x: (beat * px_per_beat - anchor_px).max(0.), ..view }
}

/// The view that shows `start..end` across lanes `width` px wide, with a
/// little air on both sides.
pub fn zoom_to(view: ArrangeView, start: Tick, end: Tick, width: f32) -> ArrangeView {
    let beats = ((end - start).max(1) as f32 / PPQ as f32).max(1. / 32.);
    let px_per_beat = (width.max(200.) * 0.92 / beats).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
    let pad = (width - beats * px_per_beat).max(0.) / 2.;
    ArrangeView { px_per_beat, scroll_x: (start as f32 / PPQ as f32 * px_per_beat - pad).max(0.), ..view }
}

/// Where to scroll so the playhead stays in view while following: `None`
/// while it is inside the lanes, else a page turn that puts it near the left.
pub fn follow(view: ArrangeView, playhead: Tick, width: f32) -> Option<f32> {
    let x = playhead as f32 / PPQ as f32 * view.px_per_beat - view.scroll_x;
    (x < 0. || x > width - 24.).then(|| (x + view.scroll_x - width * 0.1).max(0.))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOUR_FOUR: TimeSignature = TimeSignature { numerator: 4, denominator: 4 };
    const VIEW: ArrangeView = ArrangeView { px_per_beat: 20., scroll_x: 100., scroll_y: 7. };

    #[test]
    fn ruler_labels_thin_out_and_show_beats() {
        // Zoomed out: every 8th, then every 4th, 2nd, every bar.
        assert_eq!(label_step(2., FOUR_FOUR), 8 * 4 * PPQ);
        assert_eq!(label_step(4., FOUR_FOUR), 4 * 4 * PPQ);
        assert_eq!(label_step(7., FOUR_FOUR), 2 * 4 * PPQ);
        assert_eq!(label_step(14., FOUR_FOUR), 4 * PPQ);
        // Zoomed in: beats, then sixteenths.
        assert_eq!(label_step(60., FOUR_FOUR), PPQ);
        assert_eq!(label_step(240., FOUR_FOUR), PPQ / 4);
        let mut last = Tick::MAX;
        let mut zoom = 0.5;
        while zoom < 700. {
            let step = label_step(zoom, FOUR_FOUR);
            assert!(step <= last, "labels only get denser when zooming in");
            last = step;
            zoom *= 1.2;
        }
        assert_eq!(position_label(8 * PPQ, FOUR_FOUR), "3");
        assert_eq!(position_label(9 * PPQ, FOUR_FOUR), "3.2");
        assert_eq!(position_label(10 * PPQ, FOUR_FOUR), "3.3");
        assert_eq!(position_label(9 * PPQ + PPQ / 2, FOUR_FOUR), "3.2.3");
        // A 3/4 bar is three beats long.
        let waltz = TimeSignature { numerator: 3, denominator: 4 };
        assert_eq!(position_label(3 * PPQ, waltz), "2");
        assert_eq!(position_label(5 * PPQ, waltz), "2.3");
        assert_eq!(label_step(14., waltz), 2 * 3 * PPQ);
    }

    #[test]
    fn zoom_keeps_the_anchor_and_stays_in_range() {
        let beat_at = |v: ArrangeView, px: f32| (v.scroll_x + px) / v.px_per_beat;
        let z = zoom_at(VIEW, 2., 300.);
        assert_eq!(z.px_per_beat, 40.);
        assert!((beat_at(z, 300.) - beat_at(VIEW, 300.)).abs() < 1e-3);
        assert_eq!(z.scroll_y, VIEW.scroll_y);
        assert_eq!(zoom_at(VIEW, 1000., 0.).px_per_beat, MAX_PX_PER_BEAT);
        assert_eq!(zoom_at(VIEW, 0.0001, 0.).px_per_beat, MIN_PX_PER_BEAT);
    }

    #[test]
    fn zoom_to_selection_fills_the_width() {
        // Four beats across 1000 px, centred.
        let z = zoom_to(VIEW, 8 * PPQ, 12 * PPQ, 1000.);
        assert_eq!(z.px_per_beat, 230.);
        let left = 8. * z.px_per_beat - z.scroll_x;
        let right = 12. * z.px_per_beat - z.scroll_x;
        assert!((left - 40.).abs() < 0.01 && (right - 960.).abs() < 0.01, "{left} {right}");
        // A tiny range stops at the maximum zoom.
        assert_eq!(zoom_to(VIEW, 0, 10, 1000.).px_per_beat, MAX_PX_PER_BEAT);
    }

    #[test]
    fn follow_turns_the_page_when_the_playhead_leaves() {
        // 20 px per beat, scrolled 100 px: beats 5..55 are visible in 1000 px.
        assert_eq!(follow(VIEW, 10 * PPQ, 1000.), None);
        let next = follow(VIEW, 56 * PPQ, 1000.).expect("past the right edge");
        assert_eq!(next, 56. * 20. - 100.);
        assert_eq!(follow(VIEW, 0, 1000.), Some(0.));
    }
}
