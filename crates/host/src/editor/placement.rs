//! Where a plugin's editor window opens: the size and position the user
//! left it at, remembered per plugin between sessions.
//!
//! Plain data and files, no windows: the format back ends ask for the
//! remembered placement when they open an editor, and feed
//! [`Memory::observe`] from their idle tick while it is open. A plugin that
//! cannot be resized only has its position remembered; a size is only ever
//! applied after the plugin has accepted it (`checkSizeConstraint` /
//! `adjust_size`), so a stale file can never force a size the plugin refuses.
//!
//! The file is JSON (`plugin_windows.json` in the app's config folder, set
//! with [`set_file`]; sandbox processes inherit it from the host through the
//! `AITRACK_EDITOR_PLACEMENTS` environment variable).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Environment variable that tells a plugin host process which file to use.
pub const ENV: &str = "AITRACK_EDITOR_PLACEMENTS";

/// How long a size or position must stay unchanged before it is written
/// (a drag or a resize is one write, not hundreds).
pub const SETTLE: Duration = Duration::from_millis(600);

/// A window's remembered state. `x`, `y` are the top-left corner of the
/// frame in the platform's screen space (macOS: y grows upwards; Windows: y
/// grows downwards); `w`, `h` the content size.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    pub x: f64,
    pub y: f64,
    pub w: u32,
    pub h: u32,
}

/// A display's rectangle in the same space as [`Saved`]: its top-left
/// corner and its size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Whether enough of the title bar of a window whose top-left corner is
/// `(x, y)` and which is `w` wide is on `screens` to grab it: at least a
/// 96 × 24 patch. `y_up` says which way `y` grows.
pub fn is_reachable(x: f64, y: f64, w: f64, screens: &[Screen], y_up: bool) -> bool {
    const GRAB_W: f64 = 96.;
    const GRAB_H: f64 = 24.;
    screens.iter().any(|s| {
        let (bar_top, bar_bottom) = if y_up { (y, y - GRAB_H) } else { (y, y + GRAB_H) };
        let (s_lo, s_hi) = if y_up { (s.y - s.h, s.y) } else { (s.y, s.y + s.h) };
        let (b_lo, b_hi) = (bar_top.min(bar_bottom), bar_top.max(bar_bottom));
        let horizontal = (x + w).min(s.x + s.w) - x.max(s.x);
        let vertical = b_hi.min(s_hi) - b_lo.max(s_lo);
        horizontal >= GRAB_W.min(w) && vertical >= GRAB_H - 1e-3
    })
}

/// The remembered placements by plugin key.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Store {
    #[serde(default)]
    pub windows: BTreeMap<String, Saved>,
}

impl Store {
    pub fn load(file: &Path) -> Self {
        std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Write the file; best effort (a window that cannot remember its size
    /// is no reason to complain).
    pub fn save(&self, file: &Path) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(json) = serde_json::to_vec_pretty(self) else { return };
        // Write beside, then rename: a host that dies mid-write leaves the old file.
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }

    /// Remember `saved` under `key`; false when it is what is stored.
    pub fn remember(&mut self, key: &str, saved: Saved) -> bool {
        if self.windows.get(key) == Some(&saved) {
            return false;
        }
        self.windows.insert(key.to_owned(), saved);
        true
    }
}

static FILE: OnceLock<PathBuf> = OnceLock::new();

/// Choose the file placements are kept in (the app calls this once at start).
pub fn set_file(path: PathBuf) {
    let _ = FILE.set(path);
}

/// The file in use: the one set with [`set_file`], else the one the parent
/// process named in [`ENV`]. `None` turns remembering off.
pub fn file() -> Option<PathBuf> {
    FILE.get().cloned().or_else(|| std::env::var_os(ENV).map(PathBuf::from))
}

/// What was remembered for `key`.
pub fn recall(key: &str) -> Option<Saved> {
    let file = file()?;
    Store::load(&file).windows.get(key).copied()
}

/// Remember `saved` for `key` in the file in use.
pub fn remember(key: &str, saved: Saved) {
    let Some(file) = file() else { return };
    let mut store = Store::load(&file);
    if store.remember(key, saved) {
        store.save(&file);
    }
}

/// Watches an open window and says when its placement should be written:
/// once it differs from what is stored and has stayed put for [`SETTLE`].
#[derive(Debug, Default)]
pub struct Memory {
    /// What is on disk (or was applied on open).
    stored: Option<Saved>,
    /// What the window looked like at the last look, and since when.
    seen: Option<(Saved, Instant)>,
}

impl Memory {
    /// A window that opened with `applied` (what was recalled, or what the
    /// plugin asked for): the same placement is not written back.
    pub fn opened(applied: Option<Saved>) -> Self {
        Self { stored: applied, seen: None }
    }

    /// Look at the window now; `Some` when `current` has settled and is new.
    pub fn observe(&mut self, now: Instant, current: Saved) -> Option<Saved> {
        match self.seen {
            Some((s, _)) if s == current => {}
            _ => self.seen = Some((current, now)),
        }
        let (s, since) = self.seen?;
        if self.stored == Some(s) || now.duration_since(since) < SETTLE {
            return None;
        }
        self.stored = Some(s);
        Some(s)
    }

    /// The window closes: `current` is written unless it is stored already.
    pub fn closing(&mut self, current: Saved) -> Option<Saved> {
        if self.stored == Some(current) {
            return None;
        }
        self.stored = Some(current);
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, w: u32, h: u32) -> Saved {
        Saved { x, y, w, h }
    }

    #[test]
    fn a_title_bar_on_a_screen_can_be_grabbed() {
        let screen = [Screen { x: 0., y: 900., w: 1440., h: 900. }];
        // y up: the screen spans 0..900, the window's top edge at 500.
        assert!(is_reachable(100., 500., 800., &screen, true));
        // Entirely below the screen.
        assert!(!is_reachable(100., -50., 800., &screen, true));
        // Top edge above the screen: only the bottom of the bar would be visible.
        assert!(!is_reachable(100., 1000., 800., &screen, true));
        // Only 40 px of the width on screen.
        assert!(!is_reachable(1400., 500., 800., &screen, true));
        // Another display makes it reachable.
        let two = [screen[0], Screen { x: 1440., y: 900., w: 1000., h: 900. }];
        assert!(is_reachable(1400., 500., 800., &two, true));
    }

    #[test]
    fn y_down_spaces_work_the_same() {
        let screen = [Screen { x: 0., y: 0., w: 1920., h: 1080. }];
        assert!(is_reachable(10., 10., 640., &screen, false));
        assert!(!is_reachable(10., 1075., 640., &screen, false));
        assert!(!is_reachable(10., -200., 640., &screen, false));
        // A window narrower than the grab patch needs all of its width.
        assert!(is_reachable(10., 10., 64., &screen, false));
    }

    #[test]
    fn the_store_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("aitrack-placement-{}", std::process::id()));
        let file = dir.join("plugin_windows.json");
        let mut store = Store::default();
        assert!(store.remember("vst3:AA", at(10., 20., 800, 600)));
        assert!(!store.remember("vst3:AA", at(10., 20., 800, 600)), "the same placement is not a change");
        assert!(store.remember("vst3:AA", at(10., 20., 820, 600)));
        store.save(&file);
        assert_eq!(Store::load(&file), store);
        assert_eq!(Store::load(&dir.join("missing.json")), Store::default(), "a missing file is an empty store");
        std::fs::write(&file, b"not json").unwrap();
        assert_eq!(Store::load(&file), Store::default(), "a broken file is an empty store");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_placement_is_written_once_it_settles() {
        let t0 = Instant::now();
        let mut m = Memory::opened(Some(at(0., 0., 800, 600)));
        // Unchanged from what was applied: nothing to write.
        assert_eq!(m.observe(t0, at(0., 0., 800, 600)), None);
        assert_eq!(m.observe(t0 + SETTLE * 2, at(0., 0., 800, 600)), None);
        // A drag: the size keeps changing, nothing is written mid-gesture.
        assert_eq!(m.observe(t0 + SETTLE * 3, at(0., 0., 820, 600)), None);
        assert_eq!(m.observe(t0 + SETTLE * 3 + SETTLE / 2, at(0., 0., 840, 610)), None);
        // It stops: written after the settle time, and only once.
        let stop = t0 + SETTLE * 3 + SETTLE / 2;
        assert_eq!(m.observe(stop + SETTLE, at(0., 0., 840, 610)), Some(at(0., 0., 840, 610)));
        assert_eq!(m.observe(stop + SETTLE * 3, at(0., 0., 840, 610)), None);
    }

    #[test]
    fn closing_writes_what_is_not_stored_yet() {
        let t0 = Instant::now();
        let mut m = Memory::opened(None);
        assert_eq!(m.observe(t0, at(5., 5., 400, 300)), None, "too soon");
        assert_eq!(m.closing(at(5., 5., 400, 300)), Some(at(5., 5., 400, 300)));
        assert_eq!(m.closing(at(5., 5., 400, 300)), None);
    }
}
