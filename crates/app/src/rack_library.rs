//! The racks you can add from Cmd+K: your own, saved under
//! `<config>/user_racks/<kind>/<Name>.aitrack-rack`, and the factory ones
//! embedded in `aitrack-stock`. The file format is `aitrack_model::rack_preset`;
//! this module is the only place that touches the disk for racks.

use crate::project_io::write_atomic;
use crate::views::device_window::presets::clean_name;
use aitrack_model::RackKind;
use aitrack_model::rack_preset::{RackPreset, kind_slug};
use std::path::{Path, PathBuf};

pub const EXT: &str = "aitrack-rack";

/// The kind folders, in the order the library lists them.
const KINDS: [RackKind; 4] = [RackKind::Audio, RackKind::Instrument, RackKind::Midi, RackKind::Drum];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RackSource {
    /// `audio/Name.aitrack-rack`, a key of `aitrack_stock::racks::factory()`.
    Factory(&'static str),
    User(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RackEntry {
    pub source: RackSource,
    pub name: String,
    pub kind: RackKind,
    pub tags: Vec<String>,
}

impl RackEntry {
    /// Whether it is one of yours (it can be deleted).
    pub fn user(&self) -> bool {
        matches!(self.source, RackSource::User(_))
    }
}

/// `<config>/user_racks` (`AITRACK_CONFIG_DIR` applies).
pub fn root() -> PathBuf {
    crate::appearance::config_dir().join("user_racks")
}

/// The user's racks (by kind folder, then name, case-insensitive), then the
/// factory ones (in `factory()` order). A user file is listed by its stem
/// without being read; factory names and tags come from their JSON.
pub fn list(root: &Path) -> Vec<RackEntry> {
    let mut out = Vec::new();
    for kind in KINDS {
        let Ok(read) = std::fs::read_dir(root.join(kind_slug(kind))) else { continue };
        let mut mine: Vec<RackEntry> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == EXT))
            .filter_map(|p| {
                let name = p.file_stem()?.to_string_lossy().into_owned();
                Some(RackEntry { source: RackSource::User(p), name, kind, tags: Vec::new() })
            })
            .collect();
        mine.sort_by_key(|e| e.name.to_lowercase());
        out.extend(mine);
    }
    for (rel, json) in aitrack_stock::racks::factory() {
        let stem = rel.rsplit('/').next().and_then(|f| f.strip_suffix(&format!(".{EXT}"))).unwrap_or(rel);
        let kind = KINDS.into_iter().find(|k| rel.starts_with(kind_slug(*k))).unwrap_or(RackKind::Audio);
        out.push(match RackPreset::from_json(json) {
            Ok(p) => RackEntry { source: RackSource::Factory(rel), name: p.name, kind: p.kind, tags: p.tags },
            Err(_) => RackEntry { source: RackSource::Factory(rel), name: stem.to_string(), kind, tags: Vec::new() },
        });
    }
    out
}

/// Read the rack behind an entry. A broken file gives the reason in words.
pub fn read(source: &RackSource) -> Result<RackPreset, String> {
    let text = match source {
        RackSource::Factory(rel) => aitrack_stock::racks::factory()
            .iter()
            .find(|(r, _)| r == rel)
            .map(|(_, json)| json.to_string())
            .ok_or_else(|| format!("No factory rack {rel}"))?,
        RackSource::User(path) => std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?,
    };
    RackPreset::from_json(&text).map_err(|e| e.to_string())
}

/// Write `<root>/<kind>/<clean name>.aitrack-rack` with the name inside set
/// to the clean one; returns the entry and whether it replaced a file.
pub fn save(root: &Path, preset: &RackPreset) -> Result<(RackEntry, bool), String> {
    let name = clean_name(&preset.name).ok_or_else(|| "Give the rack a name".to_string())?;
    let mut preset = preset.clone();
    preset.name = name.clone();
    let json = preset.to_json().map_err(|e| format!("Cannot save the rack: {e}"))?;
    let dir = root.join(kind_slug(preset.kind));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot save the rack: {e}"))?;
    let path = dir.join(format!("{name}.{EXT}"));
    let replaced = path.is_file();
    write_atomic(&path, json.as_bytes(), false).map_err(|e| format!("Cannot save the rack: {e}"))?;
    Ok((RackEntry { source: RackSource::User(path), name, kind: preset.kind, tags: preset.tags }, replaced))
}

/// Delete one of the user's racks; a factory one is an error.
pub fn delete(entry: &RackEntry) -> Result<(), String> {
    match &entry.source {
        RackSource::User(path) => std::fs::remove_file(path).map_err(|e| format!("Cannot delete {}: {e}", entry.name)),
        RackSource::Factory(_) => Err("A factory rack cannot be deleted".into()),
    }
}

/// Size and quick hash (hex) of every sample that exists, before saving, so
/// a moved file can be told from another one of the same name.
pub fn stamp_samples(preset: &mut RackPreset) {
    for need in &mut preset.requires.samples {
        let Ok(meta) = std::fs::metadata(&need.path) else { continue };
        need.size = Some(meta.len());
        need.hash = aitrack_library::scan::quick_hash(&need.path).ok().map(|h| h.iter().map(|b| format!("{b:02x}")).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::racks::fresh_rack;
    use aitrack_model::rack_preset::SampleNeed;
    use aitrack_model::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aitrack-racks-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// A minimal audio rack (fresh chain + an EQ) as a preset.
    fn preset(name: &str) -> RackPreset {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "T", Color::palette(0))), index: 1 }.apply(&mut p).unwrap();
        let (rack, eq) = (p.alloc_device_id(), p.alloc_device_id());
        let mut device = Device::stock(rack, "rack.audio", "Rack");
        device.kind = DeviceKind::Rack(fresh_rack(RackKind::Audio));
        Command::AddDevice { slot: command::DeviceSlot::Track { track, index: 0 }, device: Box::new(device) }.apply(&mut p).unwrap();
        Command::AddDevice { slot: command::DeviceSlot::Rack { rack, chain: 0, index: 0 }, device: Box::new(Device::stock(eq, "fx.eq", "EQ")) }.apply(&mut p).unwrap();
        RackPreset::from_device(&p, rack, name, "test").unwrap()
    }

    #[test]
    fn a_saved_rack_is_listed_by_kind_reads_back_and_can_be_deleted() {
        let root = tmp("save");
        let (e, replaced) = save(&root, &preset("Warm / Bus: v2")).unwrap();
        assert!(!replaced && e.user() && e.name == "Warm  Bus v2" && e.kind == RackKind::Audio);
        assert!(matches!(&e.source, RackSource::User(p) if p.ends_with("audio/Warm  Bus v2.aitrack-rack")));
        assert!(save(&root, &preset("Warm / Bus: v2")).unwrap().1, "the same name replaces");
        std::fs::write(root.join("audio/notes.txt"), "x").unwrap();
        let mine: Vec<RackEntry> = list(&root).into_iter().filter(RackEntry::user).collect();
        assert_eq!(mine.len(), 1, "other files are not racks");
        assert_eq!(read(&mine[0].source).unwrap().name, "Warm  Bus v2");
        assert!(save(&root, &preset("///")).is_err());
        let factory: Vec<RackEntry> = list(&root).into_iter().filter(|e| !e.user()).collect();
        assert_eq!(factory.len(), aitrack_stock::racks::factory().len());
        if let Some(f) = factory.first() {
            assert!(delete(f).is_err(), "a factory rack cannot be deleted");
            assert!(read(&f.source).is_ok());
        }
        delete(&mine[0]).unwrap();
        assert!(list(&root).iter().all(|e| !e.user()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_rack_file_is_listed_but_does_not_load() {
        let root = tmp("broken");
        std::fs::create_dir_all(root.join("audio")).unwrap();
        std::fs::write(root.join("audio/Broken.aitrack-rack"), "not json").unwrap();
        let e = list(&root).into_iter().find(RackEntry::user).unwrap();
        assert_eq!(e.name, "Broken");
        assert!(read(&e.source).unwrap_err().contains("not an AI-Track rack"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stamping_fills_size_and_hash_of_the_samples_that_exist() {
        let root = tmp("stamp");
        std::fs::create_dir_all(&root).unwrap();
        let wav = root.join("kick.wav");
        std::fs::write(&wav, [1u8, 2, 3, 4]).unwrap();
        let mut p = preset("S");
        p.requires.samples = vec![
            SampleNeed { path: wav, hint: "kick.wav".into(), hash: None, size: None },
            SampleNeed { path: root.join("gone.wav"), hint: "gone.wav".into(), hash: None, size: None },
        ];
        stamp_samples(&mut p);
        assert_eq!(p.requires.samples[0].size, Some(4));
        assert_eq!(p.requires.samples[0].hash.as_ref().map(String::len), Some(64));
        assert_eq!((p.requires.samples[1].size, &p.requires.samples[1].hash), (None, &None));
        let _ = std::fs::remove_dir_all(&root);
    }
}
