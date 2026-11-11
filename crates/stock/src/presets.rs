//! Factory presets: `presets/<plugin-id>/<Category>/<Name>.json`, embedded
//! at build time.
//!
//! File format:
//! ```json
//! { "name": "Kick Punch", "plugin": "fx.eq", "author": "AI-Track",
//!   "tags": ["house", "punchy"], "params": { "0": 0.5, "1": 0.25 } }
//! ```
//! `params` maps the parameter id (as a string) to its normalised value.
//! Presets store every parameter, so loading one fully defines the sound.

use aitrack_dsp::Processor;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/factory_presets.rs"));

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub params: BTreeMap<String, f32>,
}

/// A factory preset's location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetRef {
    /// Relative path inside the factory bank, `plugin/Category/Name.json`.
    pub path: String,
    pub plugin: String,
    pub category: String,
    pub name: String,
}

#[derive(Debug)]
pub enum PresetError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    NotFound(String),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::Io(e) => write!(f, "preset I/O error: {e}"),
            PresetError::Parse(e) => write!(f, "invalid preset: {e}"),
            PresetError::NotFound(p) => write!(f, "preset not found: {p}"),
        }
    }
}

impl std::error::Error for PresetError {}

fn parse_ref(rel: &str) -> Option<PresetRef> {
    let mut parts = rel.split('/');
    let plugin = parts.next()?.to_string();
    let category = parts.next()?.to_string();
    let file = parts.next()?;
    let name = file.strip_suffix(".json").unwrap_or(file).to_string();
    Some(PresetRef { path: rel.to_string(), plugin, category, name })
}

/// Every factory preset.
pub fn all() -> Vec<PresetRef> {
    FACTORY.iter().filter_map(|(rel, _)| parse_ref(rel)).collect()
}

/// Factory presets of one plugin, sorted by category then name.
pub fn list(plugin_id: &str) -> Vec<PresetRef> {
    let mut v: Vec<PresetRef> = all().into_iter().filter(|r| r.plugin == plugin_id).collect();
    v.sort_by(|a, b| (a.category.as_str(), a.name.as_str()).cmp(&(b.category.as_str(), b.name.as_str())));
    v
}

/// Categories of one plugin's factory presets.
pub fn categories(plugin_id: &str) -> Vec<String> {
    let mut c: Vec<String> = list(plugin_id).into_iter().map(|r| r.category).collect();
    c.dedup();
    c
}

/// Load a preset: a factory path (`plugin/Category/Name.json`) or a file on
/// disk (user presets).
pub fn load(path: impl AsRef<Path>) -> Result<Preset, PresetError> {
    let p = path.as_ref();
    let key = p.to_string_lossy().replace('\\', "/");
    if let Some((_, json)) = FACTORY.iter().find(|(rel, _)| *rel == key) {
        return Preset::from_json(json);
    }
    if p.is_file() {
        let s = std::fs::read_to_string(p).map_err(PresetError::Io)?;
        return Preset::from_json(&s);
    }
    Err(PresetError::NotFound(key))
}

impl Preset {
    pub fn from_json(s: &str) -> Result<Self, PresetError> {
        serde_json::from_str(s).map_err(PresetError::Parse)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Capture a processor's current parameters.
    pub fn capture(p: &dyn Processor, name: &str, tags: &[&str]) -> Self {
        let params = p.info().params.iter().map(|i| (i.id.to_string(), p.get_param(i.id))).collect();
        Self { name: name.to_string(), plugin: p.info().id.to_string(), author: String::new(), tags: tags.iter().map(|t| t.to_string()).collect(), params }
    }

    /// Apply to a processor (control thread). Unknown ids are skipped;
    /// returns how many parameters were set.
    pub fn apply(&self, p: &mut dyn Processor) -> usize {
        let ids: Vec<u32> = p.info().params.iter().map(|i| i.id).collect();
        let mut n = 0;
        for (k, &v) in &self.params {
            if let Ok(id) = k.parse::<u32>()
                && ids.contains(&id)
            {
                p.set_param(id, v.clamp(0.0, 1.0));
                n += 1;
            }
        }
        n
    }
}

/// Source directory of the factory bank (for preset generators).
pub fn factory_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets")
}

/// Authoring helper used by the preset generators in `examples/`: starts
/// from the plugin's defaults and takes plain values (Hz, dB, ms, choice
/// index).
pub struct PresetBuilder {
    preset: Preset,
    category: String,
    infos: Vec<aitrack_dsp::ParamInfo>,
}

impl PresetBuilder {
    pub fn new(plugin_id: &str, category: &str, name: &str) -> Self {
        let proc = crate::create(plugin_id).unwrap_or_else(|| panic!("unknown plugin {plugin_id}"));
        let infos = proc.info().params.clone();
        let params = infos.iter().map(|i| (i.id.to_string(), i.default_normalized())).collect();
        Self {
            preset: Preset { name: name.to_string(), plugin: plugin_id.to_string(), author: "AI-Track".into(), tags: Vec::new(), params },
            category: category.to_string(),
            infos,
        }
    }

    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.preset.tags.extend(tags.iter().map(|t| t.to_string()));
        self
    }

    /// Set from a plain value; panics on an unknown id (generator bug).
    pub fn set(mut self, id: u32, plain: f32) -> Self {
        let info = self.infos.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{}: no param {id}", self.preset.plugin));
        self.preset.params.insert(id.to_string(), info.to_normalized(plain));
        self
    }

    pub fn set_normalized(mut self, id: u32, n: f32) -> Self {
        assert!(self.infos.iter().any(|i| i.id == id), "{}: no param {id}", self.preset.plugin);
        self.preset.params.insert(id.to_string(), n.clamp(0.0, 1.0));
        self
    }

    pub fn build(self) -> (String, Preset) {
        (self.category, self.preset)
    }

    /// Write to `<root>/<plugin>/<Category>/<Name>.json`.
    pub fn write(self, root: &Path) -> std::io::Result<PathBuf> {
        let (category, preset) = self.build();
        let dir = root.join(&preset.plugin).join(&category);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", preset.name));
        std::fs::write(&path, preset.to_json() + "\n")?;
        Ok(path)
    }
}
