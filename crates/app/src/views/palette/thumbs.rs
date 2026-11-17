//! Plugin thumbnails for device results: the picture of a third-party plugin's
//! editor the host took the first time the user opened it
//! (`<config>/thumbnails/plugins/<id>.<stamp>.png`, see [`aitrack_host::thumbs`]),
//! looked up on a background thread; our own devices draw their glyph
//! ([`super::glyphs`]); a plugin never opened shows a generated card (vendor
//! colour, category icon, vendor initials).
//!
//! [`Covers`] does the same for sample rows: the sample pack's artwork
//! ([`aitrack_library::cover`]), resolved once per folder on a background
//! thread; rows without one keep the pack-initials tile.

use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    crate::appearance::config_dir().join("thumbnails").join("plugins")
}

/// File-name-safe key for a plugin id (stock id or `vst3:`/`clap:` id).
pub fn key(id: &str) -> String {
    aitrack_host::thumbs::key(id)
}

/// Up to two initials of a vendor or plugin name ("Fab Filter" → "FF").
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, b, ..] => format!("{}{}", a.chars().next().unwrap_or('?'), b.chars().next().unwrap_or('?')).to_uppercase(),
    }
}

/// Stable colour slot for a name.
pub fn hue_slot(name: &str) -> usize {
    name.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % crate::theme::SERIES
}

/// What a thumbnail lookup is for: a plugin build (found by its version's stamp) or any other id.
type Job = (String, Option<(aitrack_model::PluginFormat, PathBuf)>);

/// Thumbnails of plugin editors, looked up on a thread of their own and kept
/// as paths only (the picture is decoded when its row is drawn). A plugin's
/// picture is the one of its current version: see [`aitrack_host::thumbs`].
pub struct Thumbs {
    found: HashMap<String, Option<PathBuf>>,
    tx: Sender<Job>,
    rx: Receiver<(String, Option<PathBuf>)>,
}

impl Default for Thumbs {
    fn default() -> Self {
        let (tx, jobs) = unbounded::<Job>();
        let (done, rx) = unbounded();
        let run = move || {
            let dir = dir();
            while let Ok((id, plugin)) = jobs.recv() {
                let hit = match plugin {
                    // The picture the host took the first time the plugin was opened.
                    Some((format, path)) => aitrack_host::thumbs::stamp(format, &path).and_then(|stamp| aitrack_host::thumbs::find(&dir, &id, &stamp)),
                    None => {
                        let k = key(&id);
                        ["png", "jpg"].iter().map(|e| dir.join(format!("{k}.{e}"))).find(|p| p.is_file())
                    }
                };
                if done.send((id, hit)).is_err() {
                    break;
                }
            }
        };
        let _ = std::thread::Builder::new().name("palette-thumbs".into()).spawn(run);
        Self { found: HashMap::new(), tx, rx }
    }
}

impl Thumbs {
    /// The picture of `id`, once looked up; the first ask queues the lookup and answers `None`.
    /// For a plugin pass its format and file, so a new version is not shown the old picture.
    pub fn get(&mut self, id: &str, plugin: Option<(aitrack_model::PluginFormat, &Path)>) -> Option<PathBuf> {
        match self.found.get(id) {
            Some(p) => p.clone(),
            None => {
                self.found.insert(id.to_owned(), None);
                let _ = self.tx.send((id.to_owned(), plugin.map(|(f, p)| (f, p.to_path_buf()))));
                None
            }
        }
    }

    /// Look again for what was not there: a plugin opened since may have a picture now.
    /// Cheap (one lookup each, on the thread); called when the palette opens.
    pub fn recheck(&mut self, plugins: impl Iterator<Item = (String, aitrack_model::PluginFormat, PathBuf)>) {
        for (id, format, path) in plugins {
            if self.found.get(&id).is_some_and(Option::is_none) {
                let _ = self.tx.send((id, Some((format, path))));
            }
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((id, hit)) = self.rx.try_recv() {
            any |= hit.is_some();
            self.found.insert(id, hit);
        }
        any
    }
}

/// Sample pack covers, cached per sample folder (`None` = looked up, none found).
pub struct Covers {
    found: HashMap<PathBuf, Option<PathBuf>>,
    tx: Sender<(PathBuf, PathBuf, Option<String>)>,
    rx: Receiver<(PathBuf, Option<PathBuf>)>,
}

impl Default for Covers {
    fn default() -> Self {
        let (tx, jobs) = unbounded::<(PathBuf, PathBuf, Option<String>)>();
        let (done, rx) = unbounded();
        let run = move || {
            while let Ok((dir, sample, pack)) = jobs.recv() {
                let hit = aitrack_library::cover::sample_cover(&sample, pack.as_deref());
                if done.send((dir, hit)).is_err() {
                    break;
                }
            }
        };
        let _ = std::thread::Builder::new().name("palette-covers".into()).spawn(run);
        Self { found: HashMap::new(), tx, rx }
    }
}

impl Covers {
    /// The cover for `sample` once its folder has been looked up; the first
    /// ask queues the lookup and answers `None`.
    pub fn get(&mut self, sample: &Path, pack: Option<&str>) -> Option<PathBuf> {
        let dir = sample.parent()?;
        if let Some(hit) = self.found.get(dir) {
            return hit.clone();
        }
        self.found.insert(dir.to_owned(), None);
        let _ = self.tx.send((dir.to_owned(), sample.to_owned(), pack.map(str::to_owned)));
        None
    }

    /// Take finished lookups; `true` when a cover turned up.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((dir, hit)) = self.rx.try_recv() {
            any |= hit.is_some();
            self.found.insert(dir, hit);
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_keys_and_colours() {
        assert_eq!(initials("Fab Filter"), "FF");
        assert_eq!(initials("u-he"), "UH");
        assert_eq!(initials("Surge"), "SU");
        assert_eq!(initials(""), "?");
        assert_eq!(key("com.x/Y z"), "com.x_Y_z");
        assert_eq!(hue_slot("Arturia"), hue_slot("Arturia"));
    }

    #[test]
    fn a_plugin_picture_is_found_for_its_version_only_and_a_later_one_shows_after_recheck() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let tmp = std::env::temp_dir().join(format!("aitrack-thumbs-lookup-{}-{nanos}", std::process::id()));
        let plugin = tmp.join("P.clap");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&plugin, b"v1").unwrap();
        // Points the lookup at a folder of ours.
        let thumbs_dir = dir();
        let id = format!("clap:test-{nanos}");
        let fmt = aitrack_model::PluginFormat::Clap;
        let mut t = Thumbs::default();
        let wait = |t: &mut Thumbs| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !t.poll() && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        assert_eq!(t.get(&id, Some((fmt, &plugin))), None, "queued");
        std::thread::sleep(std::time::Duration::from_millis(100));
        t.poll();
        // The host takes the picture after the first opening.
        let stamp = aitrack_host::thumbs::stamp(fmt, &plugin).unwrap();
        let px: Vec<u8> = (0..64 * 64).flat_map(|i| [(i % 251) as u8, (i % 13 * 19) as u8, 90, 255]).collect();
        let stored = aitrack_host::thumbs::store(&thumbs_dir, &id, &stamp, &px, 64, 64).unwrap().unwrap();
        t.recheck(std::iter::once((id.clone(), fmt, plugin.clone())));
        wait(&mut t);
        assert_eq!(t.get(&id, Some((fmt, &plugin))), Some(stored.clone()));
        // The plugin is updated: its old picture is not shown for the new file.
        std::fs::write(&plugin, b"version two").unwrap();
        let mut again = Thumbs::default();
        assert_eq!(again.get(&id, Some((fmt, &plugin))), None);
        std::thread::sleep(std::time::Duration::from_millis(150));
        again.poll();
        assert_eq!(again.get(&id, Some((fmt, &plugin))), None);
        let _ = std::fs::remove_file(stored);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn covers_resolve_once_per_folder_in_the_background() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let tmp = std::env::temp_dir().join(format!("aitrack-covers-{}-{nanos}", std::process::id()));
        let pack = tmp.join("Deep Pack");
        std::fs::create_dir_all(pack.join("Kicks")).unwrap();
        std::fs::write(pack.join("cover.jpg"), b"x").unwrap();
        let (a, b) = (pack.join("Kicks/a.wav"), pack.join("Kicks/b.wav"));
        let mut c = Covers::default();
        assert_eq!(c.get(&a, Some("Deep Pack")), None, "queued");
        assert_eq!(c.get(&b, Some("Deep Pack")), None, "same folder, not queued twice");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !c.poll() {
            assert!(std::time::Instant::now() < deadline, "lookup never finished");
            std::thread::yield_now();
        }
        assert_eq!(c.get(&b, Some("Deep Pack")), Some(pack.join("cover.jpg")));
        assert_eq!(c.found.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
