//! What the MIDI-effect editors (echo, random, length, pitch) share: the
//! scrolling body (a plot over a wrap of cards), note names and key colours,
//! and time formatting. These devices have no audio, so every drawing here
//! comes from the parameters alone (their `meter()` publishes nothing).

use super::Dev;
use super::kit::{self, Rect};
use crate::theme::*;
use gpui::{AnyElement, Div, Hsla, InteractiveElement, IntoElement, ParentElement, Stateful, StatefulInteractiveElement, Styled, TextAlign, Window, div, px};

/// Height of the plot in the compact dock and in the large editor.
pub const PLOT_H: f32 = 88.;
pub const PLOT_H_BIG: f32 = 136.;

const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// `60` → `"C4"` (middle C is C4).
pub fn note_name(note: i32) -> String {
    format!("{}{}", NAMES[note.rem_euclid(12) as usize], note.div_euclid(12) - 1)
}

/// Whether the key is a black one.
pub fn is_black(note: i32) -> bool {
    matches!(note.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// `250 ms`, `1.2 s`.
pub fn fmt_ms(ms: f32) -> String {
    if ms >= 1000. { format!("{:.1} s", ms / 1000.) } else if ms >= 100. { format!("{ms:.0} ms") } else { format!("{ms:.1} ms") }
}

/// Milliseconds of `beats` quarter notes at `bpm`.
pub fn beats_ms(beats: f32, bpm: f64) -> f32 {
    beats * 60_000. / bpm.max(1.) as f32
}

/// Quarter notes in `ms` milliseconds at `bpm`.
pub fn ms_beats(ms: f32, bpm: f64) -> f32 {
    ms * bpm.max(1.) as f32 / 60_000.
}

/// The root of one of these editors: the plot at a fixed height over the
/// cards, scrolling together when the dock is too short for both.
pub fn body(dev: &Dev, id: &'static str, narrow: f32, plot: Stateful<Div>, cards: Vec<Div>) -> AnyElement {
    let h = if dev.big { PLOT_H_BIG } else { PLOT_H };
    kit::panel(if dev.big { narrow + 80. } else { narrow })
        .child(
            div()
                .id((id, dev.key(0)))
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP2))
                .overflow_y_scroll()
                .child(div().flex_none().h(px(h)).child(plot))
                .child(div().flex_none().flex().flex_wrap().gap(px(SP1)).children(cards)),
        )
        .into_any_element()
}

/// As [`body`], for an editor whose plot needs its own heights (a keyboard,
/// a curve): `compact` in the compact dock, `big` in the large editor.
pub fn body_with(dev: &Dev, id: &'static str, narrow: f32, compact: f32, big: f32, plot: Stateful<Div>, cards: Vec<Div>) -> AnyElement {
    let h = if dev.big { big } else { compact };
    kit::panel(if dev.big { narrow + 80. } else { narrow })
        .child(
            div()
                .id((id, dev.key(0)))
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP2))
                .overflow_y_scroll()
                .child(div().flex_none().h(px(h)).child(plot))
                .child(div().flex_none().flex().flex_wrap().gap(px(SP1)).children(cards)),
        )
        .into_any_element()
}

/// The plot's height for this device, for the fractions of its handles.
pub fn plot_h(dev: &Dev) -> f32 {
    (if dev.big { PLOT_H_BIG } else { PLOT_H }) - 2.
}

/// A filled rectangle between two x positions (either order).
pub fn span(window: &mut Window, x0: f32, x1: f32, y: f32, h: f32, radius: f32, color: Hsla) {
    kit::bar(window, x0.min(x1), y, (x1 - x0).abs().max(1.), h, radius, color);
}

/// A caption in the corner of a plot.
pub fn corner(window: &mut Window, cx: &mut gpui::App, r: Rect, text: &str, right: bool, y: f32) {
    let pal = p();
    if right {
        kit::txt(window, cx, text, r.right() - 6., r.y + y, TEXT_XS, pal.faint, TextAlign::Right);
    } else {
        kit::txt(window, cx, text, r.x + 6., r.y + y, TEXT_XS, pal.faint, TextAlign::Left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_follow_middle_c() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(0), "C-1");
        assert_eq!(note_name(127), "G9");
        assert_eq!(note_name(70), "A#4");
        assert!(is_black(61) && !is_black(60) && is_black(70));
    }

    #[test]
    fn time_follows_the_tempo() {
        assert_eq!(beats_ms(1., 120.), 500.);
        assert!((ms_beats(500., 120.) - 1.).abs() < 1e-6);
        assert_eq!(fmt_ms(250.), "250 ms");
        assert_eq!(fmt_ms(1_500.), "1.5 s");
        assert_eq!(fmt_ms(12.5), "12.5 ms");
    }
}
