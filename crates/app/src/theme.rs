//! Appearance tokens: every colour, size and radius the views paint with.
//! Views never write raw colours; they ask for a role here (`p().panel`,
//! `content(0.06)`, `accent()`), so the dark and light themes and the single
//! accent colour stay consistent everywhere.
//!
//! Spacing is a 4 px grid (`SP1`…`SP6`), radii come in three sizes, and the
//! fixed bar heights are multiples of 4 so rows line up across panels.

use aitrack_model::Color;
use gpui::{Hsla, Rgba, rgb};
use std::cell::Cell;

// ── Grid ──
pub const SP1: f32 = 4.;
pub const SP2: f32 = 8.;
pub const SP3: f32 = 12.;
pub const SP4: f32 = 16.;
pub const SP5: f32 = 20.;
pub const SP6: f32 = 24.;

// ── Radii ──
pub const R_SM: f32 = 4.;
pub const R_MD: f32 = 6.;
pub const R_LG: f32 = 8.;

// ── Fixed sizes (all multiples of 4) ──
pub const TOP_BAR_H: f32 = 48.;
pub const STATUS_BAR_H: f32 = 24.;
pub const TAB_BAR_H: f32 = 32.;
pub const CONTROL_H: f32 = 28.;
pub const ROW_H: f32 = 28.;
pub const BROWSER_W: f32 = 248.;
pub const AI_PANEL_W: f32 = 320.;
pub const TRACK_HEADER_W: f32 = 200.;
pub const RULER_H: f32 = 28.;
pub const SESSION_COL_W: f32 = 132.;
pub const SESSION_ROW_H: f32 = 28.;
pub const STRIP_W: f32 = 88.;

// ── Type scale ──
pub const TEXT_XS: f32 = 10.;
pub const TEXT_SM: f32 = 11.;
pub const TEXT_MD: f32 = 12.;
pub const TEXT_LG: f32 = 14.;

pub const FONT_UI: &str = "Geist";
pub const FONT_MONO: &str = "Geist Mono";

/// The one accent colour; everything selected or active uses it.
const ACCENT: u32 = 0xF0783C;

#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Behind everything (window background, gaps between panels).
    pub base: Hsla,
    /// Panels: browser, bottom panel, AI panel.
    pub panel: Hsla,
    /// Content surfaces inside panels: track headers, cards, strips.
    pub surface: Hsla,
    /// Controls resting on a surface: buttons, fields, slots.
    pub raised: Hsla,
    /// The deepest well: lanes, the piano roll grid, meter troughs.
    pub well: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    /// Text drawn on top of the accent.
    pub on_accent: Hsla,
    // Semantic roles that must read the same everywhere.
    pub play: Hsla,
    pub record: Hsla,
    pub solo: Hsla,
    pub mute: Hsla,
    pub meter_low: Hsla,
    pub meter_mid: Hsla,
    pub meter_high: Hsla,
    /// Piano roll rows: in scale, out of scale, black keys.
    pub row_scale: Hsla,
    pub row_out: Hsla,
    /// Ink the translucent fills are made of (white on dark, black on light).
    ink: Hsla,
}

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            base: hex(0x0C0D0F),
            panel: hex(0x141518),
            surface: hex(0x1B1C20),
            raised: hex(0x25272C),
            well: hex(0x101114),
            text: hex(0xE9EAED),
            muted: hex(0x9B9DA4),
            faint: hex(0x5F6168),
            accent: hex(ACCENT),
            on_accent: hex(0x15100C),
            play: hex(0x3ECF8E),
            record: hex(0xF0484A),
            solo: hex(0x4FA3FF),
            mute: hex(0xF5C04A),
            meter_low: hex(0x3ECF8E),
            meter_mid: hex(0xF5C04A),
            meter_high: hex(0xF0484A),
            row_scale: hex(0x17181C),
            row_out: hex(0x111215),
            ink: hex(0xFFFFFF),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            base: hex(0xDCDDE1),
            panel: hex(0xEDEEF1),
            surface: hex(0xF7F7F9),
            raised: hex(0xFFFFFF),
            well: hex(0xE4E5E9),
            text: hex(0x17181B),
            muted: hex(0x5D6068),
            faint: hex(0x9A9DA5),
            accent: hex(ACCENT),
            on_accent: hex(0xFFFFFF),
            play: hex(0x1FA86A),
            record: hex(0xD93437),
            solo: hex(0x2F7FE0),
            mute: hex(0xD99A14),
            meter_low: hex(0x1FA86A),
            meter_mid: hex(0xD99A14),
            meter_high: hex(0xD93437),
            row_scale: hex(0xF4F4F6),
            row_out: hex(0xE6E7EA),
            ink: hex(0x000000),
        }
    }
}

thread_local! {
    static PALETTE: Cell<Palette> = Cell::new(Palette::dark());
}

/// The palette views paint with.
pub fn p() -> Palette {
    PALETTE.with(Cell::get)
}

pub fn set_dark(dark: bool) {
    PALETTE.with(|c| c.set(if dark { Palette::dark() } else { Palette::light() }));
}

pub fn is_dark() -> bool {
    p().dark
}

/// Translucent content ink: hover fills, dividers, quiet glyphs.
pub fn content(alpha: f32) -> Hsla {
    let mut c = p().ink;
    c.a = alpha;
    c
}

pub fn border() -> Hsla {
    content(if is_dark() { 0.07 } else { 0.10 })
}

pub fn border_strong() -> Hsla {
    content(if is_dark() { 0.12 } else { 0.16 })
}

pub fn hover() -> Hsla {
    content(0.06)
}

pub fn pressed() -> Hsla {
    content(0.10)
}

pub fn accent() -> Hsla {
    p().accent
}

pub fn accent_alpha(alpha: f32) -> Hsla {
    with_alpha(p().accent, alpha)
}

pub fn with_alpha(mut c: Hsla, alpha: f32) -> Hsla {
    c.a = alpha;
    c
}

/// A project colour (track or clip) as a paint colour.
pub fn track_color(c: Color) -> Hsla {
    hex(c.0)
}

/// A project colour toned for a filled clip body in the current theme.
pub fn clip_fill(c: Color) -> Hsla {
    let mut h = track_color(c);
    if is_dark() {
        h.l *= 0.62;
        h.s *= 0.85;
    } else {
        h.l = (h.l * 1.12).min(0.86);
    }
    h
}

/// Text that reads on top of a clip body.
pub fn clip_text(c: Color) -> Hsla {
    if is_dark() {
        with_alpha(hex(0xFFFFFF), 0.92)
    } else {
        let mut h = track_color(c);
        h.l = 0.18;
        h
    }
}

/// Windows' own caption close-button red.
pub fn caption_close() -> Hsla {
    hex(0xC42B1C)
}

/// Meter colour for a normalised level.
pub fn meter_color(level: f32) -> Hsla {
    let pal = p();
    if level > 0.92 {
        pal.meter_high
    } else if level > 0.72 {
        pal.meter_mid
    } else {
        pal.meter_low
    }
}

/// Blend two colours in RGB.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (a.to_rgb(), b.to_rgb());
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_sit_on_the_grid() {
        for v in [
            TOP_BAR_H, STATUS_BAR_H, TAB_BAR_H, CONTROL_H, ROW_H, BROWSER_W, AI_PANEL_W, TRACK_HEADER_W, RULER_H,
            SESSION_COL_W, SESSION_ROW_H, STRIP_W, SP1, SP2, SP3, SP4, SP5, SP6, R_SM, R_LG,
        ] {
            assert_eq!(v % 4., 0., "{v} is off the 4 px grid");
        }
    }

    #[test]
    fn both_themes_share_the_accent() {
        assert_eq!(Palette::dark().accent, Palette::light().accent);
        set_dark(false);
        assert!(!is_dark());
        set_dark(true);
        assert!(is_dark());
    }
}
