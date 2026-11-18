//! Where each device window opens: the size every plugin starts at, and the
//! size and position the user left it at, remembered per plugin between
//! sessions (`device_windows.json` in the config folder, next to the other
//! interface settings).
//!
//! Everything here is plain numbers: the window code asks for a
//! [`Placement`] and hands in the displays, so the rules (minimum size,
//! "the window is still on a screen") are tested without a window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The smallest a device window can be made.
pub const MIN_SIZE: (f32, f32) = (720., 380.);

/// Height of the chrome's header and footer, and the width of a meter rail:
/// the frame around an editor, as the sizes below add it up.
pub const HEADER_H: f32 = 48.;
pub const FOOTER_H: f32 = 28.;
pub const RAIL_W: f32 = 44.;
/// Space around the editor card inside the frame (padding and border).
pub const BODY_PAD: f32 = 26.;

/// A window's rectangle in screen points.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Placement {
    /// The same placement with a size no smaller than `min` and no larger than
    /// `max` (a window cannot be bigger than the screen; the screen wins over
    /// the minimum, the editor scrolls then).
    pub fn clamped(self, min: (f32, f32), max: (f32, f32)) -> Self {
        let w = self.w.max(min.0).min(max.0);
        let h = self.h.max(min.1).min(max.1);
        Self { w, h, ..self }
    }

    /// Whether enough of the title bar is on `screens` to grab it: at least a
    /// 96 x 32 patch of the window's top-left area on one of them.
    pub fn is_reachable(&self, screens: &[Placement]) -> bool {
        screens.iter().any(|s| {
            let left = self.x.max(s.x);
            let right = (self.x + self.w).min(s.x + s.w);
            let top = self.y.max(s.y);
            let bottom = (self.y + 32.).min(s.y + s.h);
            right - left >= 96. && bottom - top >= 32. - 1e-3
        })
    }
}

/// How many meter rails the window of `plugin` shows: the input one when the
/// device publishes it, one for the gain reduction, and the output (or the
/// track's level).
pub fn rails(plugin: &str) -> usize {
    super::io::map(plugin).map_or(1, |io| usize::from(io.input.is_some()) + usize::from(io.reduction.is_some()) + 1)
}

/// What the frame adds around an editor: width (padding and rails) and height
/// (header, footer and padding).
pub fn frame_extra(plugin: &str) -> (f32, f32) {
    (BODY_PAD + rails(plugin) as f32 * RAIL_W, HEADER_H + FOOTER_H + BODY_PAD)
}

/// The smallest window of `plugin` that shows its whole editor without
/// scrolling: what the editor needs plus the frame. The window cannot be made
/// smaller than this (the screen's size excepted).
pub fn min_size(plugin: &str) -> (f32, f32) {
    let (w, h) = editor_extent(plugin);
    let (fw, fh) = frame_extra(plugin);
    ((w + fw).max(MIN_SIZE.0).ceil(), (h + fh).max(MIN_SIZE.1).ceil())
}

/// The size the window of `plugin` opens at the first time, in points: its
/// minimum plus some room, which the main plot takes.
pub fn default_size(plugin: &str) -> (f32, f32) {
    // Windows with a full editor of their own (instruments, the big effect windows).
    match plugin {
        "instrument.wavetable" => return (1400., 980.),
        "instrument.vault" => return (1440., 940.),
        "instrument.sampler" | "instrument.drum" => return (1120., 660.),
        "instrument.granular" => return (1320., 880.),
        "fx.drum_extract" => return (1120., 680.),
        "fx.reverb" | "fx.delay" => return (1080., 640.),
        _ => {}
    }
    let (w, h) = min_size(plugin);
    let round8 = |v: f32| (v / 8.).ceil() * 8.;
    (round8(w * 1.12).clamp(MIN_SIZE.0, 1480.), round8(h * 1.14).clamp(MIN_SIZE.1, 900.))
}

/// What an editor needs at its best: the width its big arrangement is laid
/// out for and a height that gives its plots room.
pub fn editor_extent(plugin: &str) -> (f32, f32) {
    match plugin {
        "fx.eq" => (920., 460.),
        "fx.mbdyn" => (980., 600.),
        // Measured: below these the groups clip (checked on screenshots at the minimum).
        "fx.compressor" => (700., 340.),
        "fx.gate" | "fx.transient" => (880., 540.),
        "fx.buscomp" => (960., 540.),
        "fx.limiter" => (800., 560.),
        "fx.analyzer" => (780., 400.),
        "fx.console" => (720., 480.),
        "fx.tape" | "fx.amp" | "fx.saturator" => (780., 500.),
        "fx.multiband" => (780., 400.),
        "fx.clipper" => (680., 480.),
        "fx.filter" | "fx.shaper" | "fx.autopan" | "fx.crusher" | "fx.chorus" => (680., 380.),
        "fx.stutter" => (700., 668.),
        "fx.drumbus" => (720., 520.),
        "fx.reverb" | "fx.delay" => (860., 440.),
        "fx.midside" => (560., 360.),
        "fx.utility" => (640., 480.),
        "midi.chord" | "midi.scale" => (700., 540.),
        p if p.starts_with("midi.") => (680., 500.),
        "instrument.kick808" => (720., 560.),
        "instrument.acid303" => (960., 640.),
        "instrument.bloom" => (1080., 760.),
        _ => (700., 380.),
    }
}

/// The content height a window whose top edge is `top` points below the top of
/// a `screen_h` screen can have: the screen's bottom less the title bar and a
/// margin for the dock.
pub fn room_below(screen_h: f32, top: f32) -> f32 {
    (screen_h - top.max(0.) - 32. - 48.).max(0.)
}

/// The window's content height after its drawer changed. `grown` is how much it
/// has grown for the drawer so far, `drawer` what the drawer wants now (0 when
/// closed), `cap` the tallest content the screen allows and `floor` the least
/// the window may shrink to. Returns the new height and the new `grown`: opening
/// adds the drawer's height, closing takes back what was added (so a window the
/// user made taller meanwhile keeps its extra), and a screen too short for it
/// gives what it can (the body scrolls for the rest).
pub fn follow_drawer(current: f32, grown: f32, drawer: f32, cap: f32, floor: f32) -> (f32, f32) {
    let delta = drawer - grown;
    if delta.abs() < 1. {
        return (current, grown);
    }
    let target = if delta > 0. { (current + delta).min(cap.max(current)) } else { (current + delta).max(floor.min(current)) };
    // Closed: whatever the floor kept is no longer the drawer's.
    (target, if drawer <= 0. { 0. } else { grown + (target - current) })
}

/// The remembered placements, by plugin id.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub windows: BTreeMap<String, Placement>,
}

impl Saved {
    /// `device_windows.json` in the app config folder.
    pub fn default_file() -> PathBuf {
        crate::appearance::config_dir().join("device_windows.json")
    }

    pub fn load(file: &Path) -> Self {
        std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Write the file (best effort: a window that cannot remember its size is
    /// no reason to complain).
    pub fn save(&self, file: &Path) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(file, json);
        }
    }

    /// Remember `placement` for `plugin`; false when it is what is stored.
    pub fn remember(&mut self, plugin: &str, placement: Placement) -> bool {
        if self.windows.get(plugin) == Some(&placement) {
            return false;
        }
        self.windows.insert(plugin.to_owned(), placement);
        true
    }
}

/// Where the window of `plugin` opens: what the user left (kept on screen
/// and within the screens' size), else the default size, centred on
/// `fallback_centre`. `screens` are the displays' rectangles.
pub fn resolve(saved: &Saved, plugin: &str, screens: &[Placement], fallback_centre: (f32, f32)) -> Placement {
    let biggest = screens.iter().fold((0f32, 0f32), |m, s| (m.0.max(s.w), m.1.max(s.h)));
    let max = if screens.is_empty() { (f32::MAX, f32::MAX) } else { biggest };
    if let Some(p) = saved.windows.get(plugin).copied().map(|p| p.clamped(min_size(plugin), max))
        && (screens.is_empty() || p.is_reachable(screens))
    {
        return p;
    }
    let (w, h) = default_size(plugin);
    let (w, h) = (w.min(max.0), h.min(max.1));
    // Centred across, but high up: a drawer that opens grows the window downwards.
    let (top, screen_h) = screens.first().map_or((0., fallback_centre.1 * 2.), |s| (s.y, s.h));
    let y = top + ((screen_h - h - 32.) / 2.).clamp(0., 64.);
    Placement { x: (fallback_centre.0 - w / 2.).round(), y: y.round(), w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Placement = Placement { x: 0., y: 0., w: 1920., h: 1080. };

    #[test]
    fn every_stock_plugin_opens_in_a_window_that_holds_its_editor() {
        for plugin in ["fx.eq", "fx.mbdyn", "fx.compressor", "fx.utility", "midi.arp", "instrument.kick808", "fx.unknown"] {
            let (w, h) = default_size(plugin);
            assert!(w >= MIN_SIZE.0 && h >= MIN_SIZE.1, "{plugin}: {w}x{h}");
        }
        // The multiband opens wide enough for its 980 px editor and the frame.
        let (w, _) = default_size("fx.mbdyn");
        assert!(w >= 980. + BODY_PAD + 2. * RAIL_W);
        // Instruments with a full editor get a big one.
        assert!(default_size("instrument.wavetable").0 >= 1200.);
        // The drum bus wraps its stages: it asks for a narrow window, not a 940 px one.
        assert!(editor_extent("fx.drumbus").0 <= 800.);
    }

    #[test]
    fn a_window_cannot_be_made_smaller_than_the_minimum_or_bigger_than_the_screen() {
        let tiny = Placement { x: 10., y: 10., w: 100., h: 100. }.clamped(MIN_SIZE, (1920., 1080.));
        assert_eq!((tiny.w, tiny.h), MIN_SIZE);
        let huge = Placement { x: 0., y: 0., w: 9000., h: 5000. }.clamped(MIN_SIZE, (1920., 1080.));
        assert_eq!((huge.w, huge.h), (1920., 1080.));
        // The screen wins over a minimum bigger than it.
        let small_screen = Placement { x: 0., y: 0., w: 100., h: 100. }.clamped((1200., 900.), (1100., 800.));
        assert_eq!((small_screen.w, small_screen.h), (1100., 800.));
    }

    #[test]
    fn a_drawer_grows_the_window_and_gives_the_height_back() {
        // Opens: the window grows by the drawer's height.
        let (h, grown) = follow_drawer(600., 0., 200., 900., 400.);
        assert_eq!((h, grown), (800., 200.));
        // Nothing changes while it stays open (and the estimate is refined by a measure).
        assert_eq!(follow_drawer(800., 200., 200., 900., 400.), (800., 200.));
        assert_eq!(follow_drawer(800., 200., 232., 900., 400.), (832., 232.));
        // Closes: back to where it was.
        assert_eq!(follow_drawer(832., 232., 0., 900., 400.), (600., 0.));
        // The user made it taller while open: they keep their extra.
        assert_eq!(follow_drawer(900., 200., 0., 1000., 400.), (700., 0.));
        // A short screen gives what it can, and does not ask again every frame.
        let (h, grown) = follow_drawer(600., 0., 500., 700., 400.);
        assert_eq!((h, grown), (700., 100.));
        assert_eq!(follow_drawer(700., 100., 500., 700., 400.), (700., 100.));
        // Closing never takes the window under its minimum.
        assert_eq!(follow_drawer(450., 300., 0., 900., 400.), (400., 0.));
        // A window already taller than the screen allows is not shrunk to open a drawer.
        assert_eq!(follow_drawer(950., 0., 100., 900., 400.), (950., 0.));
        assert_eq!(room_below(900., 100.), 720.);
    }

    #[test]
    fn the_minimum_holds_the_editor_and_the_default_leaves_room_over_it() {
        for plugin in ["fx.eq", "fx.mbdyn", "fx.compressor", "fx.stutter", "fx.drumbus", "fx.utility", "midi.arp", "instrument.kick808", "instrument.vault", "fx.nothing"] {
            let (min_w, min_h) = min_size(plugin);
            let (w, h) = default_size(plugin);
            let (ew, eh) = editor_extent(plugin);
            let (fw, fh) = frame_extra(plugin);
            assert!(min_w >= ew + fw && min_h >= eh + fh, "{plugin}: the minimum holds the editor and its frame");
            assert!(w >= min_w && h >= min_h, "{plugin}: opens at least at its minimum ({w}x{h} vs {min_w}x{min_h})");
            assert!(min_w >= MIN_SIZE.0 && min_h >= MIN_SIZE.1);
        }
        // A plugin that publishes input and reduction shows three rails, one that does not shows the track's.
        assert_eq!((rails("fx.compressor"), rails("fx.gate"), rails("fx.eq")), (3, 2, 1));
        assert_eq!(rails("instrument.vault"), 1);
        // Where there is room the window opens bigger than it must be.
        let (min_w, min_h) = min_size("fx.compressor");
        let (w, h) = default_size("fx.compressor");
        assert!(w > min_w && h > min_h);
    }

    #[test]
    fn a_saved_size_below_the_minimum_is_raised_and_reopening_is_stable() {
        let mut saved = Saved::default();
        saved.remember("fx.compressor", Placement { x: 100., y: 100., w: 473., h: 399. });
        let p = resolve(&saved, "fx.compressor", &[SCREEN], (960., 540.));
        assert_eq!((p.w, p.h), min_size("fx.compressor"), "a size from an older build or a bug cannot trap the editor");
        // What the window reports (frame corner, content size) is what it reopens with: no drift.
        let again = {
            let mut s = Saved::default();
            s.remember("fx.compressor", p);
            resolve(&s, "fx.compressor", &[SCREEN], (960., 540.))
        };
        assert_eq!(again, p);
    }

    #[test]
    fn a_remembered_window_comes_back_where_it_was() {
        let mut saved = Saved::default();
        let left = Placement { x: 300., y: 200., w: 1000., h: 600. };
        assert!(saved.remember("fx.eq", left));
        assert!(!saved.remember("fx.eq", left), "unchanged is not a write");
        assert_eq!(resolve(&saved, "fx.eq", &[SCREEN], (960., 540.)), left);
        // Another plugin has its own.
        let fresh = resolve(&saved, "fx.compressor", &[SCREEN], (960., 540.));
        assert_ne!(fresh, left);
        let (w, h) = default_size("fx.compressor");
        assert_eq!((fresh.w, fresh.h), (w, h));
        assert_eq!(fresh.x, (960. - w / 2.).round());
        assert!(fresh.y >= 0. && fresh.y <= 64., "opens near the top, with room below for a drawer");
    }

    #[test]
    fn a_window_left_on_a_screen_that_is_gone_opens_centred_again() {
        let mut saved = Saved::default();
        saved.remember("fx.eq", Placement { x: 3000., y: 100., w: 1000., h: 600. });
        let p = resolve(&saved, "fx.eq", &[SCREEN], (960., 540.));
        assert!(p.is_reachable(&[SCREEN]));
        assert_eq!((p.w, p.h), default_size("fx.eq"));
        // Half off the edge is still reachable (the title bar shows).
        let edge = Placement { x: 1800., y: 100., w: 1000., h: 600. };
        assert!(edge.is_reachable(&[SCREEN]));
        // Under the menu bar of a screen above is not.
        let above = Placement { x: 100., y: -700., w: 1000., h: 600. };
        assert!(!above.is_reachable(&[SCREEN]));
    }

    #[test]
    fn placements_survive_the_file() {
        let dir = std::env::temp_dir().join(format!("aitrack-dw-placement-{}", std::process::id()));
        let file = dir.join("device_windows.json");
        let mut saved = Saved::default();
        saved.remember("fx.mbdyn", Placement { x: 12., y: 34., w: 1160., h: 640. });
        saved.save(&file);
        assert_eq!(Saved::load(&file), saved);
        assert_eq!(Saved::load(&dir.join("missing.json")), Saved::default());
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(Saved::load(&file), Saved::default(), "a damaged file is forgotten, not fatal");
        let _ = std::fs::remove_dir_all(dir);
    }
}
