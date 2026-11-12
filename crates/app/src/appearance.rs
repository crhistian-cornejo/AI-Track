//! The user's appearance settings: which theme (or follow the system),
//! accent and per-token overrides, background image with glass panes, UI
//! scale, density, radius, font size and styles. Persisted as JSON in the
//! app config dir; custom themes live there as `.aitheme` files and are
//! hot-reloaded when the folder changes.

use crate::theme::{self, Colors, Density, KnobStyle, MeterStyle, Resolved, Rgba8, Theme, WaveformStyle};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pane {
    TopBar,
    Browser,
    Center,
    Bottom,
    Assistant,
}

impl Pane {
    pub const ALL: [Pane; 5] = [Self::TopBar, Self::Browser, Self::Center, Self::Bottom, Self::Assistant];
    pub fn label(self) -> &'static str {
        match self {
            Self::TopBar => "Top bar",
            Self::Browser => "Browser",
            Self::Center => "Session / Arrangement",
            Self::Bottom => "Bottom panel",
            Self::Assistant => "Assistant",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BgFit {
    #[default]
    Cover,
    Contain,
    Tile,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Background {
    pub path: PathBuf,
    pub fit: BgFit,
    /// Blur radius in px (0–40), applied once to a cached copy.
    pub blur: f32,
    /// Darken (or lighten on light themes) over the image, 0–1.
    pub dim: f32,
    /// Panes painted translucent over the image.
    pub glass: Vec<Pane>,
    /// Opacity of glass panes, 0.3–1.
    pub glass_opacity: f32,
}

impl Default for Background {
    fn default() -> Self {
        Self { path: PathBuf::new(), fit: BgFit::Cover, blur: 12., dim: 0.35, glass: vec![Pane::Browser, Pane::Assistant, Pane::TopBar], glass_opacity: 0.78 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub follow_system: bool,
    pub dark_theme: String,
    pub light_theme: String,
    pub accent: Option<Rgba8>,
    /// Token name → colour, applied over the active theme.
    pub overrides: BTreeMap<String, Rgba8>,
    pub background: Option<Background>,
    pub ui_scale: f32,
    pub density: Density,
    pub radius: f32,
    pub font_size: f32,
    pub waveform: WaveformStyle,
    pub meter: MeterStyle,
    pub knob: KnobStyle,
    pub reduced_motion: bool,
    /// The assistant's brain: a Claude model id, or `agent:<cli>`; empty
    /// for the default.
    pub assistant_model: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "Midnight".into(),
            follow_system: false,
            dark_theme: "Midnight".into(),
            light_theme: "Daylight".into(),
            accent: None,
            overrides: BTreeMap::new(),
            background: None,
            ui_scale: 1.,
            density: Density::Comfortable,
            radius: 1.,
            font_size: 12.,
            waveform: WaveformStyle::Filled,
            meter: MeterStyle::Zones,
            knob: KnobStyle::Arc,
            reduced_motion: false,
            assistant_model: String::new(),
        }
    }
}

/// `%APPDATA%\AI-Track` on Windows, `~/Library/Application Support/AI-Track`
/// on macOS (`AITRACK_CONFIG_DIR` overrides, for tests and portable installs).
pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("AITRACK_CONFIG_DIR") {
        return p.into();
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("AI-Track")
}

pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

fn settings_path() -> PathBuf {
    config_dir().join("appearance.json")
}

impl Appearance {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// The theme name in effect for the system's light/dark mode.
    pub fn active_name(&self, system_dark: bool) -> &str {
        if self.follow_system {
            if system_dark { &self.dark_theme } else { &self.light_theme }
        } else {
            &self.theme
        }
    }
}

/// Built-in themes plus the `.aitheme` files in the themes folder.
#[derive(Clone, Debug, Default)]
pub struct ThemeLibrary {
    pub themes: Vec<(Theme, Option<PathBuf>)>,
}

impl ThemeLibrary {
    pub fn load() -> Self {
        let mut themes: Vec<(Theme, Option<PathBuf>)> = theme::builtin_themes().into_iter().map(|t| (t, None)).collect();
        for (t, path) in load_dir(&themes_dir()) {
            // A file may replace a built-in of the same name.
            themes.retain(|(x, _)| x.name != t.name || x.name.is_empty());
            themes.push((t, Some(path)));
        }
        Self { themes }
    }

    pub fn find(&self, name: &str) -> Option<&Theme> {
        self.themes.iter().map(|(t, _)| t).find(|t| t.name == name)
    }
}

/// Parse every `.aitheme` in `dir`, skipping files that do not parse.
pub fn load_dir(dir: &Path) -> Vec<(Theme, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(Theme, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "aitheme"))
        .filter_map(|p| Some((Theme::from_json(&std::fs::read_to_string(&p).ok()?).ok()?, p)))
        .collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
}

/// The theme with the user's accent and token overrides applied.
pub fn effective_theme(app: &Appearance, base: &Theme) -> Theme {
    let mut t = base.clone();
    apply_overrides(&mut t.colors, app);
    t.style.ui_scale = app.ui_scale;
    t.style.density = app.density;
    t.style.radius = app.radius;
    t.style.font_size = app.font_size;
    t.style.waveform = app.waveform;
    t.style.meter = app.meter;
    t.style.knob = app.knob;
    t
}

pub fn apply_overrides(c: &mut Colors, app: &Appearance) {
    if let Some(a) = app.accent {
        // The accent carries its derived tokens with it.
        c.accent = a;
        let rgb = a.0 >> 8;
        c.selection = Rgba8::rgba(rgb, 0x2E);
        c.focus = Rgba8::rgba(rgb, 0xB3);
        c.scale_highlight = Rgba8::rgba(rgb, 0x73);
        c.root_highlight = Rgba8::rgba(rgb, 0x14);
        c.automation = a;
    }
    for (name, v) in &app.overrides {
        c.set(name, *v);
    }
}

/// Resolve and install the active theme. Returns the effective theme.
pub fn apply(app: &Appearance, lib: &ThemeLibrary, system_dark: bool, animate: bool) -> Theme {
    let base = lib.find(app.active_name(system_dark)).or_else(|| lib.find("Midnight")).cloned().unwrap_or_else(|| theme::builtin_themes().remove(0));
    let t = effective_theme(app, &base);
    let (glass, glass_alpha) = match &app.background {
        Some(bg) if !bg.path.as_os_str().is_empty() => (bg.glass.clone(), bg.glass_opacity.clamp(0.3, 1.)),
        _ => (Vec::new(), 1.),
    };
    theme::install(&t.colors, t.dark, Resolved { style: t.style.clone(), clips: t.clips, glass, glass_alpha }, animate && !app.reduced_motion);
    t
}

/// Write `theme` as a new `.aitheme` in the themes folder (unique file name).
pub fn save_theme(theme: &Theme) -> std::io::Result<PathBuf> {
    let dir = themes_dir();
    std::fs::create_dir_all(&dir)?;
    let stem: String = theme.name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let mut path = dir.join(format!("{stem}.aitheme"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.aitheme"));
        n += 1;
    }
    std::fs::write(&path, theme.to_json())?;
    Ok(path)
}

/// Copy a shared theme file into the themes folder.
pub fn import_theme(path: &Path) -> Result<Theme, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let t = Theme::from_json(&text).map_err(|e| format!("Not a theme file: {e}"))?;
    save_theme(&t).map_err(|e| e.to_string())?;
    Ok(t)
}

/// A unique "Name copy" for duplicates.
pub fn copy_name(lib: &ThemeLibrary, name: &str) -> String {
    let mut n = format!("{name} Copy");
    let mut i = 2;
    while lib.find(&n).is_some() {
        n = format!("{name} Copy {i}");
        i += 1;
    }
    n
}

/// Decode, downscale and blur the background once; returns the cached file to show.
pub fn prepare_background(bg: &Background) -> Result<PathBuf, String> {
    if bg.blur < 0.5 {
        return Ok(bg.path.clone());
    }
    let meta = std::fs::metadata(&bg.path).map_err(|e| e.to_string())?;
    let stamp = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(bg.path.to_string_lossy().to_string(), stamp, (bg.blur * 10.) as u32), &mut h);
    let out = config_dir().join("cache").join(format!("bg-{:016x}.png", std::hash::Hasher::finish(&h)));
    if out.exists() {
        return Ok(out);
    }
    let img = image::open(&bg.path).map_err(|e| e.to_string())?;
    let img = img.thumbnail(1600, 1600);
    let blurred = img.fast_blur(bg.blur.clamp(0., 40.));
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    blurred.save(&out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Watch the themes folder; `on_change` runs (on the watcher's thread) after edits.
pub fn watch_themes(on_change: impl Fn() + Send + 'static) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let dir = themes_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(e) = res
            && e.paths.iter().any(|p| p.extension().is_some_and(|x| x == "aitheme")) {
                on_change();
            }
    })
    .ok()?;
    w.watch(&dir, notify::RecursiveMode::NonRecursive).ok()?;
    Some(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_apply_over_the_theme() {
        let base = theme::builtin_themes().remove(0);
        let mut app = Appearance { accent: Some(Rgba8::rgb(0x00AAFF)), ..Appearance::default() };
        app.overrides.insert("panel".into(), Rgba8::rgb(0x223344));
        app.overrides.insert("not_a_token".into(), Rgba8::rgb(0x000000));
        app.radius = 0.;
        let t = effective_theme(&app, &base);
        assert_eq!(t.colors.accent, Rgba8::rgb(0x00AAFF));
        assert_eq!(t.colors.focus, Rgba8::rgba(0x00AAFF, 0xB3));
        assert_eq!(t.colors.panel, Rgba8::rgb(0x223344));
        assert_eq!(t.colors.text, base.colors.text);
        assert_eq!(t.style.radius, 0.);
    }

    #[test]
    fn follow_system_picks_by_mode() {
        let app = Appearance { follow_system: true, ..Appearance::default() };
        assert_eq!(app.active_name(true), "Midnight");
        assert_eq!(app.active_name(false), "Daylight");
    }

    #[test]
    fn settings_and_theme_files_round_trip() {
        let mut app = Appearance { background: Some(Background { path: "bg.jpg".into(), ..Background::default() }), ..Appearance::default() };
        app.overrides.insert("accent2".into(), Rgba8::rgba(0x112233, 0x80));
        let back: Appearance = serde_json::from_str(&serde_json::to_string(&app).unwrap()).unwrap();
        assert_eq!(back, app);

        let dir = std::env::temp_dir().join(format!("aitrack-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut t = theme::builtin_themes()[3].clone();
        t.name = "Round Trip".into();
        std::fs::write(dir.join("rt.aitheme"), t.to_json()).unwrap();
        std::fs::write(dir.join("broken.aitheme"), "{ not json").unwrap();
        let loaded = load_dir(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
