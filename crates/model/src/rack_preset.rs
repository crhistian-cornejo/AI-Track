//! The `.aitrack-rack` file: one rack saved as a preset of its own, to load
//! into any project. A rack carries its devices, chains, macros, mappings
//! and variations, so what a preset holds is the rack `Device` itself.
//!
//! This module is the format and nothing else: the text in and out, its
//! version, and what a rack needs from the machine that opens it. It does no
//! I/O (the app reads and writes the files) and edits no project: loading is
//! one `Command::AddDevice` of a device with fresh ids.
//!
//! The format has a version of its own, apart from the project's, with the
//! same rules ([`crate::format`]): a file from a newer AI-Track is refused,
//! an older one goes through [`RACK_MIGRATIONS`].

use crate::format::{self, LoadError, Migration};
use crate::ids::DeviceId;
use crate::project::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// The `format` field of every rack file.
pub const RACK_FORMAT: &str = "aitrack-rack";
/// The rack format this version writes and reads.
pub const RACK_FORMAT_VERSION: u32 = 1;
/// `RACK_MIGRATIONS[n]` takes version `n + 1` to `n + 2` (the pattern of
/// [`format::MIGRATIONS`]); empty for now.
pub const RACK_MIGRATIONS: &[Migration] = &[];

/// A rack as a file: who made it, what it holds and what it needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RackPreset {
    /// Always [`RACK_FORMAT`]: what tells a rack from any other JSON.
    pub format: String,
    pub format_version: u32,
    /// The AI-Track that wrote the file.
    #[serde(default)]
    pub app_version: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The kind of the rack in the payload (it picks the folder of the file).
    pub kind: RackKind,
    pub payload: Payload,
    #[serde(default)]
    pub requires: Requires,
}

/// What the file holds. An enum so a payload can be added without breaking
/// the files already written (a lone chain, later).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    /// A device that is a rack (`DeviceKind::Rack`) of the file's `kind`.
    Device(Device),
}

/// What the rack needs that is not inside the file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub plugins: Vec<PluginNeed>,
    #[serde(default)]
    pub samples: Vec<SampleNeed>,
}

/// A hosted plugin the rack uses, by format and uid (the plugin's identity),
/// with the name and vendor to show when it is not installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginNeed {
    pub format: PluginFormat,
    pub uid: String,
    pub name: String,
    pub vendor: String,
}

/// A file a device state names (a sampler's sample). `hint` is its file name,
/// to say what is missing; `hash` and `size` tell it apart from another file
/// of that name (the app fills them in: this crate does no I/O).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleNeed {
    pub path: PathBuf,
    pub hint: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// Why a rack could not be saved or loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RackPresetError {
    /// Not JSON, not a rack file, or a rack file that does not hold together.
    #[error("not an AI-Track rack: {0}")]
    Malformed(String),
    /// Saved by a newer AI-Track. Loading it would drop what this version
    /// does not know, so it is refused.
    #[error("this rack was saved by a newer AI-Track (rack format {found}, this version reads up to {supported}); update AI-Track to load it")]
    TooNew { found: u32, supported: u32 },
    /// Only a rack can be saved as one.
    #[error("device {0} is not a rack")]
    NotARack(DeviceId),
}

impl RackPreset {
    /// The file's text, pretty-printed. Like [`Project::to_json`], it fails
    /// only on a path that is not UTF-8.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Read a rack file's text. The format name is checked first, then the
    /// version (a newer one is refused with [`RackPresetError::TooNew`], an
    /// older one is migrated), then that the payload is a rack of the file's
    /// `kind`. Missing optional fields take their default and unknown ones are
    /// ignored, so a file from a later version with only additions still reads.
    pub fn from_json(json: &str) -> Result<Self, RackPresetError> {
        let malformed = |why: String| RackPresetError::Malformed(why);
        let mut value: Value = serde_json::from_str(json).map_err(|e| malformed(e.to_string()))?;
        if value.get("format").and_then(Value::as_str) != Some(RACK_FORMAT) {
            return Err(malformed(format!("it has no \"format\": \"{RACK_FORMAT}\"")));
        }
        let version = format::peek_format_version(json).map_err(|e| malformed(reason(e)))?;
        if version > RACK_FORMAT_VERSION {
            return Err(RackPresetError::TooNew { found: version, supported: RACK_FORMAT_VERSION });
        }
        let preset: Self = if version == RACK_FORMAT_VERSION {
            serde_json::from_str(json)
        } else {
            format::migrate_with(&mut value, version, RACK_FORMAT_VERSION, RACK_MIGRATIONS).map_err(|e| malformed(reason(e)))?;
            serde_json::from_value(value)
        }
        .map_err(|e| malformed(e.to_string()))?;
        match &preset.device().kind {
            DeviceKind::Rack(r) if r.kind == preset.kind => Ok(preset),
            DeviceKind::Rack(r) => Err(malformed(format!("the file says a {} rack but holds a {} one", kind_slug(preset.kind), kind_slug(r.kind)))),
            _ => Err(malformed("what it holds is not a rack".into())),
        }
    }

    /// The rack device of the payload.
    pub fn device(&self) -> &Device {
        let Payload::Device(device) = &self.payload;
        device
    }
}

/// The folder name of a kind: `audio`, `instrument`, `midi` or `drum`.
pub fn kind_slug(kind: RackKind) -> &'static str {
    match kind {
        RackKind::Audio => "audio",
        RackKind::Instrument => "instrument",
        RackKind::Midi => "midi",
        RackKind::Drum => "drum",
    }
}

/// A project-file error, put the way a rack file would say it.
fn reason(e: LoadError) -> String {
    match e {
        LoadError::Malformed(why) => why,
        LoadError::MissingVersion => "it has no format_version".into(),
        LoadError::InvalidVersion(v) => format!("invalid format_version {v}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RackPreset {
        let mut rack = crate::racks::fresh_rack(RackKind::Audio);
        rack.chains[0].devices.push(Device::stock(DeviceId(2), "fx.compressor", "Comp"));
        let device = Device { kind: DeviceKind::Rack(rack), ..Device::stock(DeviceId(1), "rack.audio", "Glue") };
        RackPreset {
            format: RACK_FORMAT.into(),
            format_version: RACK_FORMAT_VERSION,
            app_version: "test".into(),
            name: "Glue".into(),
            author: String::new(),
            tags: vec!["bus".into()],
            kind: RackKind::Audio,
            payload: Payload::Device(device),
            requires: Requires::default(),
        }
    }

    #[test]
    fn a_rack_file_reads_back_and_a_newer_one_is_refused() {
        let p = sample();
        let json = p.to_json().unwrap();
        assert_eq!(RackPreset::from_json(&json).unwrap(), p);
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!((v["format"].as_str(), v["payload"]["device"]["kind"]["type"].as_str()), (Some("aitrack-rack"), Some("rack")));
        v["format_version"] = 99.into();
        assert_eq!(RackPreset::from_json(&v.to_string()), Err(RackPresetError::TooNew { found: 99, supported: RACK_FORMAT_VERSION }));
        assert!(RackPresetError::TooNew { found: 99, supported: 1 }.to_string().contains("newer AI-Track"));
        v["format_version"] = 1.into();
        v["format"] = "aitrack-project".into();
        assert!(matches!(RackPreset::from_json(&v.to_string()), Err(RackPresetError::Malformed(_))));
        assert!(matches!(RackPreset::from_json("not json"), Err(RackPresetError::Malformed(_))));
        // The kind must be the payload's.
        let mut w: serde_json::Value = serde_json::from_str(&json).unwrap();
        w["kind"] = "drum".into();
        assert!(matches!(RackPreset::from_json(&w.to_string()), Err(RackPresetError::Malformed(_))));
        // Missing optional fields default; unknown ones are ignored.
        let mut x: serde_json::Value = serde_json::from_str(&json).unwrap();
        x.as_object_mut().unwrap().remove("requires");
        x["later_field"] = 1.into();
        assert_eq!(RackPreset::from_json(&x.to_string()).unwrap().requires, Requires::default());
        assert_eq!((kind_slug(RackKind::Audio), kind_slug(RackKind::Instrument), kind_slug(RackKind::Midi), kind_slug(RackKind::Drum)), ("audio", "instrument", "midi", "drum"));
    }

    #[test]
    fn a_file_that_is_not_a_rack_says_why_and_never_panics() {
        let json = sample().to_json().unwrap();
        let why = |v: &serde_json::Value| RackPreset::from_json(&v.to_string()).unwrap_err().to_string();
        // A device that is not a rack.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v["payload"]["device"]["kind"] = serde_json::json!({ "type": "stock", "plugin_id": "fx.eq" });
        assert_eq!(why(&v), "not an AI-Track rack: what it holds is not a rack");
        // No version, or a name that is not ours.
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("format_version");
        assert_eq!(why(&v), "not an AI-Track rack: it has no format_version");
        for text in ["", "[1,2]", "{}", "null", r#"{"format":"aitrack-rack"}"#, r#"{"format":"aitrack-rack","format_version":1}"#] {
            assert!(matches!(RackPreset::from_json(text), Err(RackPresetError::Malformed(_))), "{text:?}");
        }
    }
}
