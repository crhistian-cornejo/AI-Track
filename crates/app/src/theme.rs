//! Theming. A [`Theme`] is data (it round-trips through `.aitheme` JSON
//! files): semantic colour tokens plus style choices (radius, density,
//! knob/fader/meter/waveform styles, font). The user's [`Appearance`]
//! (`crate::appearance`) picks a theme and layers overrides on top; the
//! result resolves into a [`Palette`] of paint colours that views read
//! through `p()` and the helpers below. Views never write raw colours.
//!
//! Switching themes cross-fades every token over [`TWEEN`] on the shared
//! motion curve; frames are requested only while the fade runs.
//!
//! Spacing is a 4 px grid (`SP1`…`SP6`) and the fixed bar heights are
//! multiples of 4 so rows line up across panels. Radii scale with the theme.

use aitrack_model::Color;
use gpui::{Hsla, Rgba};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::time::{Duration, Instant};

// ── Grid ──
pub const SP1: f32 = 4.;
pub const SP2: f32 = 8.;
pub const SP3: f32 = 12.;
pub const SP4: f32 = 16.;
pub const SP6: f32 = 24.;

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
pub const STRIP_W: f32 = 88.;

// ── Type scale (multiplied by the theme's font size, see `fs`) ──
pub const TEXT_XS: f32 = 10.;
pub const TEXT_SM: f32 = 11.;
pub const TEXT_MD: f32 = 12.;
pub const TEXT_LG: f32 = 14.;

pub const FONT_MONO: &str = "Geist Mono";

/// The cross-fade between themes.
pub const TWEEN: Duration = Duration::from_millis(360);

// ───────────────────────────── Colour values ─────────────────────────────

/// An sRGB colour with alpha, `0xRRGGBBAA`; serialised as `"#rrggbb"` or `"#rrggbbaa"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba8(pub u32);

impl Rgba8 {
    pub const fn rgb(c: u32) -> Self {
        Self(c << 8 | 0xFF)
    }
    pub const fn rgba(c: u32, a: u8) -> Self {
        Self(c << 8 | a as u32)
    }
    pub fn hex(self) -> String {
        if self.0 & 0xFF == 0xFF { format!("#{:06x}", self.0 >> 8) } else { format!("#{:08x}", self.0) }
    }
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.trim().trim_start_matches('#');
        let v = u32::from_str_radix(h, 16).ok()?;
        match h.len() {
            6 => Some(Self::rgb(v)),
            8 => Some(Self(v)),
            3 => {
                let (r, g, b) = (v >> 8 & 0xF, v >> 4 & 0xF, v & 0xF);
                Some(Self::rgb((r * 17) << 16 | (g * 17) << 8 | (b * 17)))
            }
            _ => None,
        }
    }
    pub fn to_hsla(self) -> Hsla {
        Rgba {
            r: (self.0 >> 24 & 0xFF) as f32 / 255.,
            g: (self.0 >> 16 & 0xFF) as f32 / 255.,
            b: (self.0 >> 8 & 0xFF) as f32 / 255.,
            a: (self.0 & 0xFF) as f32 / 255.,
        }
        .into()
    }
    pub fn from_hsla(c: Hsla) -> Self {
        let r = c.to_rgb();
        let q = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
        Self(q(r.r) << 24 | q(r.g) << 16 | q(r.b) << 8 | q(r.a))
    }
}

impl Serialize for Rgba8 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgba8 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba8::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid colour {s:?}")))
    }
}

// ───────────────────────────── Tokens ─────────────────────────────

macro_rules! tokens {
    ($($name:ident: $doc:literal,)*) => {
        /// The semantic colour tokens a theme defines. Missing tokens in a
        /// theme file fall back to Midnight's.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct Colors { $(#[doc = $doc] pub $name: Rgba8,)* }

        /// Resolved paint colours (what views read).
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct Palette { pub dark: bool, $(pub $name: Hsla,)* }

        impl Colors {
            pub const NAMES: &'static [&'static str] = &[$(stringify!($name),)*];
            pub fn get(&self, name: &str) -> Option<Rgba8> {
                match name { $(stringify!($name) => Some(self.$name),)* _ => None }
            }
            pub fn set(&mut self, name: &str, v: Rgba8) -> bool {
                match name { $(stringify!($name) => { self.$name = v; true })* _ => false }
            }
            fn palette(&self, dark: bool) -> Palette {
                Palette { dark, $($name: self.$name.to_hsla(),)* }
            }
        }

        impl Palette {
            fn lerp(&self, to: &Palette, t: f32) -> Palette {
                Palette { dark: if t < 0.5 { self.dark } else { to.dark }, $($name: mix(self.$name, to.$name, t),)* }
            }
        }

        /// A human description per token, for the Appearance editor.
        pub fn token_doc(name: &str) -> &'static str {
            match name { $(stringify!($name) => $doc,)* _ => "" }
        }
    };
}

tokens! {
    base: "Window background behind every panel",
    panel: "Panels: browser, bottom dock, assistant",
    surface: "Content on panels: track headers, cards, strips",
    raised: "Controls on surfaces: buttons, fields, menus",
    overlay: "Popovers and dialogs",
    well: "Deepest layer: lanes, grids, meter troughs",
    text: "Primary text",
    muted: "Secondary text and icons",
    faint: "Tertiary text, hints, disabled",
    accent: "Selection, active controls, playhead",
    accent2: "Second accent: tempo/BPM badges, links",
    on_accent: "Text drawn on the accent",
    border: "Panel and control borders",
    divider: "Stronger separators and focused borders",
    selection: "Selected rows and items",
    focus: "Keyboard focus ring",
    record: "Record and arm",
    play: "Play and success",
    solo: "Solo",
    mute: "Mute",
    arm: "Record-arm on tracks",
    meter_low: "Meter, safe zone",
    meter_mid: "Meter, hot zone",
    meter_high: "Meter, clipping zone",
    grid_bar: "Grid lines on bars",
    grid_beat: "Grid lines on beats",
    grid_sub: "Grid lines on subdivisions",
    key_white: "Piano keyboard white keys",
    key_black: "Piano keyboard black keys",
    row_scale: "Piano roll rows in the scale",
    row_out: "Piano roll rows outside the scale",
    scale_highlight: "Marker for in-scale keys",
    root_highlight: "Root note rows and marker",
    waveform: "Audio waveforms (mixed with the clip colour)",
    automation: "Automation lines (mixed with the track colour)",
    clip_text_light: "Clip text on dark clip colours",
    clip_text_dark: "Clip text on light clip colours",
    shadow: "Shadows and dark overlays on clips",
    ink: "Translucent hover/press fills are made of this",
}

impl Default for Colors {
    fn default() -> Self {
        builtin_themes().remove(0).colors
    }
}

// ───────────────────────────── Style ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KnobStyle {
    /// A value arc around a body with a pointer.
    #[default]
    Arc,
    /// A thin ring with a dot.
    Minimal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FaderStyle {
    #[default]
    Classic,
    Slim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WaveformStyle {
    #[default]
    Filled,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MeterStyle {
    /// Green, yellow and red zones.
    #[default]
    Zones,
    /// LED segments.
    Segmented,
    /// One colour (the accent).
    Solid,
}

/// How the 16 project colours are toned so they read on this theme.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipStyle {
    /// Multiplies the clip colour's saturation.
    pub saturation: f32,
    /// Multiplies the clip colour's lightness (dark themes) or lifts it (light themes).
    pub brightness: f32,
}

impl Default for ClipStyle {
    fn default() -> Self {
        Self { saturation: 0.85, brightness: 0.62 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    /// Multiplies every corner radius (0 = square, 1 = default, 2 = round).
    pub radius: f32,
    pub density: Density,
    pub knob: KnobStyle,
    pub fader: FaderStyle,
    pub waveform: WaveformStyle,
    pub meter: MeterStyle,
    pub font_family: String,
    /// Base UI text size in px (12 = default).
    pub font_size: f32,
    /// Whole-interface zoom (0.8–1.5).
    pub ui_scale: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            radius: 1.,
            density: Density::Comfortable,
            knob: KnobStyle::Arc,
            fader: FaderStyle::Classic,
            waveform: WaveformStyle::Filled,
            meter: MeterStyle::Zones,
            font_family: "Geist".into(),
            font_size: 12.,
            ui_scale: 1.,
        }
    }
}

/// A theme: data that round-trips through `.aitheme` files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub colors: Colors,
    #[serde(default)]
    pub clips: ClipStyle,
    #[serde(default)]
    pub style: Style,
}

impl Theme {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

// ───────────────────────────── Built-in themes ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn make(
    name: &str,
    dark: bool,
    layers: [u32; 6],
    text: [u32; 3],
    accent: u32,
    accent2: u32,
    on_accent: u32,
    states: [u32; 5],
    rows: [u32; 4],
    clips: ClipStyle,
) -> Theme {
    let ink = if dark { 0xFFFFFF } else { 0x000000 };
    let a = |alpha: u8| Rgba8::rgba(ink, alpha);
    let [base, panel, surface, raised, overlay, well] = layers;
    let [record, play, solo, mute, arm] = states;
    let [row_scale, row_out, key_white, key_black] = rows;
    Theme {
        name: name.into(),
        dark,
        author: "AI-Track".into(),
        colors: Colors {
            base: Rgba8::rgb(base),
            panel: Rgba8::rgb(panel),
            surface: Rgba8::rgb(surface),
            raised: Rgba8::rgb(raised),
            overlay: Rgba8::rgb(overlay),
            well: Rgba8::rgb(well),
            text: Rgba8::rgb(text[0]),
            muted: Rgba8::rgb(text[1]),
            faint: Rgba8::rgb(text[2]),
            accent: Rgba8::rgb(accent),
            accent2: Rgba8::rgb(accent2),
            on_accent: Rgba8::rgb(on_accent),
            border: a(if dark { 0x14 } else { 0x1C }),
            divider: a(if dark { 0x24 } else { 0x2C }),
            selection: Rgba8::rgba(accent, 0x2E),
            focus: Rgba8::rgba(accent, 0xB3),
            record: Rgba8::rgb(record),
            play: Rgba8::rgb(play),
            solo: Rgba8::rgb(solo),
            mute: Rgba8::rgb(mute),
            arm: Rgba8::rgb(arm),
            meter_low: Rgba8::rgb(play),
            meter_mid: Rgba8::rgb(mute),
            meter_high: Rgba8::rgb(record),
            grid_bar: a(if dark { 0x1A } else { 0x26 }),
            grid_beat: a(if dark { 0x0C } else { 0x12 }),
            grid_sub: a(if dark { 0x06 } else { 0x09 }),
            key_white: Rgba8::rgb(key_white),
            key_black: Rgba8::rgb(key_black),
            row_scale: Rgba8::rgb(row_scale),
            row_out: Rgba8::rgb(row_out),
            scale_highlight: Rgba8::rgba(accent, 0x73),
            root_highlight: Rgba8::rgba(accent, 0x14),
            waveform: Rgba8::rgba(if dark { 0xFFFFFF } else { 0x000000 }, 0xB3),
            automation: Rgba8::rgb(accent),
            clip_text_light: Rgba8::rgba(0xFFFFFF, 0xEB),
            clip_text_dark: Rgba8::rgb(0x16171A),
            shadow: Rgba8::rgba(0x000000, 0x40),
            ink: Rgba8::rgb(ink),
        },
        clips,
        style: Style::default(),
    }
}

const DARK_CLIPS: ClipStyle = ClipStyle { saturation: 0.85, brightness: 0.62 };
const LIGHT_CLIPS: ClipStyle = ClipStyle { saturation: 0.9, brightness: 1.12 };

/// The built-in themes, Midnight first (the default).
pub fn builtin_themes() -> Vec<Theme> {
    let std_states = [0xF0484A, 0x3ECF8E, 0x4FA3FF, 0xF5C04A, 0xF0484A];
    let light_states = [0xD93437, 0x1A9A60, 0x2F7FE0, 0xC98A0E, 0xD93437];
    let mut v = vec![
        make("Midnight", true, [0x0C0D0F, 0x141518, 0x1B1C20, 0x25272C, 0x2A2C32, 0x101114], [0xE9EAED, 0x9B9DA4, 0x6A6C73], 0xF0783C, 0x58B4F0, 0x15100C, std_states, [0x17181C, 0x111215, 0xC9CACE, 0x16171A], DARK_CLIPS),
        make("Daylight", false, [0xDCDDE1, 0xEDEEF1, 0xF7F7F9, 0xFFFFFF, 0xFFFFFF, 0xE4E5E9], [0x17181B, 0x52555D, 0x8A8D95], 0xE0661F, 0x1F7AC4, 0xFFFFFF, light_states, [0xF4F4F6, 0xE3E4E8, 0xFFFFFF, 0x2A2B30], LIGHT_CLIPS),
        make("Graphite", true, [0x1B1B1C, 0x252526, 0x2D2D2F, 0x38383B, 0x3E3E42, 0x1F1F20], [0xEDEDED, 0xABABAE, 0x77777B], 0x5AA9E6, 0xE0B050, 0x0B1620, std_states, [0x2A2A2C, 0x232324, 0xD2D2D2, 0x1C1C1D], DARK_CLIPS),
        make("Neon", true, [0x0B0614, 0x140B24, 0x1C1231, 0x281A45, 0x2E1F50, 0x090411], [0xF6EEFF, 0xBCA9E0, 0x7A66A0], 0xFF2E97, 0x00E5FF, 0x1A0010, [0xFF3B6B, 0x2BF5B4, 0x00D8FF, 0xFFD23F, 0xFF3B6B], [0x1A1030, 0x120A22, 0xE4DAF5, 0x150C26], ClipStyle { saturation: 1.0, brightness: 0.66 }),
        make("Ember", true, [0x120C09, 0x1B130F, 0x241A14, 0x30231B, 0x36281F, 0x0F0A07], [0xF6EAE2, 0xC2A898, 0x84695A], 0xFF7A1A, 0xFFC857, 0x1C0D02, [0xF04A3A, 0x7BD389, 0x6FA8FF, 0xF2C14E, 0xF04A3A], [0x22180F, 0x18110C, 0xE6D6CC, 0x1A120D], ClipStyle { saturation: 0.8, brightness: 0.6 }),
        make("Forest", true, [0x0B110D, 0x111914, 0x17221B, 0x203026, 0x25352B, 0x090E0B], [0xE6F0E8, 0xA0B8A7, 0x637A6A], 0x6BCB77, 0xE8C468, 0x08160B, [0xEF5B4F, 0x6BCB77, 0x5FA8E8, 0xE8C468, 0xEF5B4F], [0x16201A, 0x0F1612, 0xD7E3DA, 0x101712], ClipStyle { saturation: 0.75, brightness: 0.6 }),
        make("Oceanic", true, [0x09101A, 0x0E1822, 0x14212D, 0x1C2D3C, 0x213344, 0x070E16], [0xE4EEF6, 0x9DB4C7, 0x5F7689], 0x2EC4D6, 0xF7B267, 0x041418, [0xF0606A, 0x4CD6A5, 0x6AA8FF, 0xF7C866, 0xF0606A], [0x13202B, 0x0C1620, 0xD5E2EC, 0x0D1720], ClipStyle { saturation: 0.85, brightness: 0.6 }),
        make("Oceanic Light", false, [0xD3DEE6, 0xE6EEF3, 0xF3F7FA, 0xFFFFFF, 0xFFFFFF, 0xDCE6ED], [0x0F1C26, 0x45596A, 0x7E91A0], 0x0E8FA3, 0xC66A12, 0xFFFFFF, light_states, [0xEEF4F8, 0xDEE8EF, 0xFFFFFF, 0x1E2C38], LIGHT_CLIPS),
        make("Mono", true, [0x000000, 0x0A0A0A, 0x141414, 0x1F1F1F, 0x262626, 0x000000], [0xFFFFFF, 0xD6D6D6, 0x9A9A9A], 0xFFD400, 0x00C8FF, 0x000000, [0xFF4040, 0x3CFF8A, 0x40B0FF, 0xFFD400, 0xFF4040], [0x141414, 0x050505, 0xFFFFFF, 0x000000], ClipStyle { saturation: 1.0, brightness: 0.55 }),
        make("Mono Light", false, [0xFFFFFF, 0xFFFFFF, 0xF5F5F5, 0xFFFFFF, 0xFFFFFF, 0xEDEDED], [0x000000, 0x2E2E2E, 0x5C5C5C], 0x0047C2, 0xB34700, 0xFFFFFF, [0xC40000, 0x007A3D, 0x0047C2, 0x8A6100, 0xC40000], [0xFAFAFA, 0xE6E6E6, 0xFFFFFF, 0x000000], LIGHT_CLIPS),
    ];
    // High-contrast themes draw stronger structure.
    for t in v.iter_mut().filter(|t| t.name.starts_with("Mono")) {
        let ink = if t.dark { 0xFFFFFF } else { 0x000000 };
        t.colors.border = Rgba8::rgba(ink, 0x40);
        t.colors.divider = Rgba8::rgba(ink, 0x70);
        t.colors.grid_bar = Rgba8::rgba(ink, 0x40);
        t.colors.grid_beat = Rgba8::rgba(ink, 0x20);
    }
    v
}

// ───────────────────────────── Runtime ─────────────────────────────

/// Style choices resolved from the theme and the user's appearance settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub style: Style,
    pub clips: ClipStyle,
    /// Panes painted translucent over a background image.
    pub glass: Vec<crate::appearance::Pane>,
    pub glass_alpha: f32,
}

struct Runtime {
    target: Palette,
    from: Palette,
    start: Instant,
    resolved: Resolved,
}

thread_local! {
    static RT: RefCell<Runtime> = RefCell::new({
        let t = &builtin_themes()[0];
        let pal = t.colors.palette(t.dark);
        Runtime { target: pal, from: pal, start: Instant::now() - TWEEN, resolved: Resolved { style: t.style.clone(), clips: t.clips, glass: Vec::new(), glass_alpha: 1. } }
    });
}

/// Install a resolved theme. With `animate`, colours cross-fade from what is on screen.
pub fn install(colors: &Colors, dark: bool, resolved: Resolved, animate: bool) {
    let pal = colors.palette(dark);
    RT.with(|rt| {
        let mut rt = rt.borrow_mut();
        let shown = sample(&rt, Instant::now());
        rt.from = if animate { shown } else { pal };
        rt.target = pal;
        rt.start = Instant::now();
        rt.resolved = resolved;
    });
}

fn sample(rt: &Runtime, now: Instant) -> Palette {
    let t = now.saturating_duration_since(rt.start).as_secs_f32() / TWEEN.as_secs_f32();
    if t >= 1. {
        rt.target
    } else {
        rt.from.lerp(&rt.target, crate::motion::bezier(t, crate::motion::EMPHASIZED))
    }
}

/// The palette views paint with (mid-fade while a theme switch runs).
pub fn p() -> Palette {
    RT.with(|rt| sample(&rt.borrow(), Instant::now()))
}

/// Whether a theme cross-fade is still running (the window keeps drawing).
pub fn tweening() -> bool {
    RT.with(|rt| rt.borrow().start.elapsed() < TWEEN)
}

pub fn style() -> Style {
    RT.with(|rt| rt.borrow().resolved.style.clone())
}

pub fn is_dark() -> bool {
    p().dark
}

// ── Sizes that follow the theme ──

fn radius_scale() -> f32 {
    RT.with(|rt| rt.borrow().resolved.style.radius)
}
pub fn r_sm() -> f32 {
    4. * radius_scale()
}
pub fn r_md() -> f32 {
    6. * radius_scale()
}
pub fn r_lg() -> f32 {
    8. * radius_scale()
}

/// Text size `base` scaled by the theme's font size.
pub fn fs(base: f32) -> f32 {
    (base * RT.with(|rt| rt.borrow().resolved.style.font_size) / 12.).round()
}

pub fn font_ui() -> String {
    RT.with(|rt| rt.borrow().resolved.style.font_family.clone())
}

/// Row heights shrink a little in compact density.
pub fn density(h: f32) -> f32 {
    match RT.with(|rt| rt.borrow().resolved.style.density) {
        Density::Compact => (h * 0.8 / 4.).round() * 4.,
        Density::Comfortable => h,
    }
}

pub fn session_row_h() -> f32 {
    density(28.)
}

// ── Colour helpers ──

/// Translucent content ink: hover fills, quiet glyphs.
pub fn content(alpha: f32) -> Hsla {
    with_alpha(p().ink, alpha)
}

pub fn border() -> Hsla {
    p().border
}

pub fn border_strong() -> Hsla {
    p().divider
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

pub fn shadow(alpha: f32) -> Hsla {
    with_alpha(p().shadow, alpha)
}

pub fn with_alpha(mut c: Hsla, alpha: f32) -> Hsla {
    c.a = alpha;
    c
}

/// Panel background, translucent when the pane is set to glass over a background image.
pub fn pane_bg(pane: crate::appearance::Pane) -> Hsla {
    let r = RT.with(|rt| (rt.borrow().resolved.glass.contains(&pane), rt.borrow().resolved.glass_alpha));
    let base = match pane {
        crate::appearance::Pane::TopBar => p().base,
        _ => p().panel,
    };
    if r.0 { with_alpha(base, base.a * r.1) } else { base }
}

/// A project colour (track or clip) as a paint colour.
pub fn track_color(c: Color) -> Hsla {
    Rgba8::rgb(c.0).to_hsla()
}

/// A project colour toned for a clip body on this theme.
pub fn clip_fill(c: Color) -> Hsla {
    let mut h = track_color(c);
    let cs = RT.with(|rt| rt.borrow().resolved.clips);
    h.s = (h.s * cs.saturation).clamp(0., 1.);
    if is_dark() {
        h.l = (h.l * cs.brightness).clamp(0.08, 0.9);
    } else {
        h.l = (h.l * cs.brightness).min(0.86);
    }
    h
}

/// Text that reads on top of a clip body: whichever clip text token contrasts more.
pub fn clip_text(c: Color) -> Hsla {
    let pal = p();
    let fill = clip_fill(c);
    if contrast(pal.clip_text_dark, fill) > contrast(pal.clip_text_light, fill) { pal.clip_text_dark } else { pal.clip_text_light }
}

/// Windows' own caption close-button red.
pub fn caption_close() -> Hsla {
    Rgba8::rgb(0xC42B1C).to_hsla()
}

/// Blend two colours in RGB.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    if t <= 0. {
        return a;
    }
    if t >= 1. {
        return b;
    }
    let (a, b): (Rgba, Rgba) = (a.to_rgb(), b.to_rgb());
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }.into()
}

/// WCAG relative luminance.
pub fn luminance(c: Hsla) -> f32 {
    let r = c.to_rgb();
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(r.r) + 0.7152 * lin(r.g) + 0.0722 * lin(r.b)
}

/// WCAG contrast ratio of `fg` over an opaque `bg`.
pub fn contrast(fg: Hsla, bg: Hsla) -> f32 {
    let fg = mix(bg, with_alpha(fg, 1.), fg.a);
    let (a, b) = (luminance(fg), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_sit_on_the_grid() {
        for v in [TOP_BAR_H, STATUS_BAR_H, TAB_BAR_H, CONTROL_H, ROW_H, BROWSER_W, AI_PANEL_W, TRACK_HEADER_W, RULER_H, SESSION_COL_W, STRIP_W, SP1, SP2, SP3, SP4, SP6] {
            assert_eq!(v % 4., 0., "{v} is off the 4 px grid");
        }
    }

    #[test]
    fn builtin_themes_pass_contrast_checks() {
        let themes = builtin_themes();
        assert!(themes.len() >= 8);
        for t in &themes {
            let p = t.colors.palette(t.dark);
            for (bg, name) in [(p.panel, "panel"), (p.surface, "surface"), (p.raised, "raised")] {
                let c = contrast(p.text, bg);
                assert!(c >= 4.5, "{}: text on {name} is {c:.2}:1", t.name);
                let c = contrast(p.muted, bg);
                assert!(c >= 3.0, "{}: secondary text on {name} is {c:.2}:1", t.name);
            }
            let c = contrast(p.on_accent, p.accent);
            assert!(c >= 3.0, "{}: text on accent is {c:.2}:1", t.name);
        }
    }

    #[test]
    fn theme_files_round_trip() {
        for t in builtin_themes() {
            let back = Theme::from_json(&t.to_json()).unwrap();
            assert_eq!(back, t);
        }
        // A partial file falls back to Midnight for missing tokens and style.
        let partial = Theme::from_json(r##"{"name":"Mine","dark":true,"colors":{"accent":"#ff00aa"}}"##).unwrap();
        assert_eq!(partial.colors.accent, Rgba8::rgb(0xFF00AA));
        assert_eq!(partial.colors.panel, builtin_themes()[0].colors.panel);
        assert_eq!(partial.style, Style::default());
    }

    #[test]
    fn colours_parse_and_print() {
        assert_eq!(Rgba8::parse("#abc"), Some(Rgba8::rgb(0xAABBCC)));
        assert_eq!(Rgba8::parse("#11223344").unwrap().hex(), "#11223344");
        assert_eq!(Rgba8::rgb(0x102030).hex(), "#102030");
        assert!(Rgba8::parse("nope").is_none());
        let c = Rgba8::rgb(0xF0783C);
        assert_eq!(Rgba8::from_hsla(c.to_hsla()), c);
    }

    #[test]
    fn tokens_are_addressable_by_name() {
        let mut c = Colors::default();
        assert!(Colors::NAMES.contains(&"accent"));
        assert!(c.set("accent", Rgba8::rgb(0x123456)));
        assert_eq!(c.get("accent"), Some(Rgba8::rgb(0x123456)));
        assert!(!c.set("nope", Rgba8::rgb(0)));
        assert!(!token_doc("base").is_empty());
    }
}
