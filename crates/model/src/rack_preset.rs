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
use crate::tracks::{copy_devices, relink_devices};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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

    /// The rack `rack` of `p`, ready to save, named `name`. What ties it to
    /// the project it sits in is cut: sidechains, which name tracks there, are
    /// cleared, and so are the macro mappings and modulator targets that point
    /// at a device the rack does not hold. `requires` lists every hosted plugin
    /// (by format and uid, once) and every file a device state names
    /// ([`state_paths`], once, with its file name as the `hint`). `author` and
    /// `tags` start empty, and the sample `hash` and `size` stay unset: the
    /// caller fills in what it knows. Plugin states are as the project holds
    /// them (the app captures the live ones before saving).
    pub fn from_device(p: &Project, rack: DeviceId, name: &str, app_version: &str) -> Result<Self, RackPresetError> {
        let source = p.device(rack).ok_or(RackPresetError::NotARack(rack))?;
        let DeviceKind::Rack(r) = &source.kind else { return Err(RackPresetError::NotARack(rack)) };
        let mut device = source.clone();
        sever_outside_links(&mut device);
        let requires = requires_of(&device);
        Ok(Self {
            format: RACK_FORMAT.into(),
            format_version: RACK_FORMAT_VERSION,
            app_version: app_version.into(),
            name: name.into(),
            author: String::new(),
            tags: Vec::new(),
            kind: r.kind,
            payload: Payload::Device(device),
            requires,
        })
    }

    /// A copy of the rack with fresh ids from `p` (racks all the way down),
    /// its macro and modulator links pointing at the copies, named after the
    /// preset. Only allocates ids: the caller adds it with one
    /// `Command::AddDevice`, in the project it came from or in any other,
    /// and as many times as it likes. A link the file makes to anything but
    /// its own devices (a hand-edited file) is dropped, as is a sidechain, so
    /// nothing in it can reach a device or a track that happens to hold an id.
    pub fn instantiate(&self, p: &mut Project) -> Device {
        let mut map = HashMap::new();
        let mut copies = copy_devices(p, std::slice::from_ref(self.device()), &mut map);
        relink_devices(&mut copies, &map, &|t| t);
        let mut device = copies.swap_remove(0);
        sever_outside_links(&mut device);
        device.name = self.name.clone();
        device.preset = Some(self.name.clone());
        device
    }

    /// What this rack needs that is not here, in `requires` order: plugins
    /// first, then samples. `has_plugin` says whether a plugin is installed
    /// and `exists` whether a file is on disk.
    pub fn missing(&self, has_plugin: impl Fn(&PluginNeed) -> bool, exists: impl Fn(&Path) -> bool) -> Vec<Missing> {
        let plugins = self.requires.plugins.iter().filter(|n| !has_plugin(n)).cloned().map(Missing::Plugin);
        let samples = self.requires.samples.iter().filter(|s| !exists(&s.path)).cloned().map(Missing::Sample);
        plugins.chain(samples).collect()
    }
}

/// Something a rack needs and the machine does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    Plugin(PluginNeed),
    Sample(SampleNeed),
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plugin(n) => {
                let format = match n.format {
                    PluginFormat::Vst3 => "VST3",
                    PluginFormat::Clap => "CLAP",
                };
                write!(f, "plugin {} ({format}) is not installed", n.name)
            }
            Self::Sample(s) => write!(f, "sample {} is missing", s.hint),
        }
    }
}

/// The files a stock device state names: `extra.path` and `extra.sfz_path`
/// (the Sampler's, see [`crate::slice::sampler_state`]; the Granular's
/// `extra.path`), when they are non-empty strings. Anything that is not a
/// JSON state (a plugin's chunk) names none.
pub fn state_paths(state: &[u8]) -> Vec<PathBuf> {
    let Ok(state) = serde_json::from_slice::<Value>(state) else { return Vec::new() };
    ["path", "sfz_path"]
        .into_iter()
        .filter_map(|key| state.get("extra")?.get(key)?.as_str())
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Cut what ties the rack `root` to where it sits: every sidechain, every
/// macro mapping onto a device its rack does not hold (at any depth, in a
/// chain or a return, the rule of `routing::dangling_references`), and every
/// modulator target that is a device outside the tree. Tracks' own fader and
/// pan targets stay: they mean the track the rack ends up on.
fn sever_outside_links(root: &mut Device) {
    fn cut(d: &mut Device, tree: &HashSet<DeviceId>) {
        d.sidechain = None;
        d.mods.retain(|m| match m.target {
            ModTarget::Param { device, .. } => tree.contains(&device),
            ModTarget::Volume | ModTarget::Pan => true,
        });
        if let DeviceKind::Rack(rack) = &mut d.kind {
            let mut inside = HashSet::new();
            for c in rack.chains.iter().chain(&rack.returns) {
                visit_devices(&c.devices, &mut |x| {
                    inside.insert(x.id);
                });
            }
            for m in &mut rack.macros {
                m.mappings.retain(|x| inside.contains(&x.device));
            }
            for c in rack.chains.iter_mut().chain(rack.returns.iter_mut()) {
                for x in &mut c.devices {
                    cut(x, tree);
                }
            }
        }
    }
    let mut tree = HashSet::new();
    visit_devices(std::slice::from_ref(&*root), &mut |d| {
        tree.insert(d.id);
    });
    cut(root, &tree);
}

/// What the rack `root` holds that has to be on the machine: the hosted
/// plugins and the files the stock states name.
fn requires_of(root: &Device) -> Requires {
    let mut requires = Requires::default();
    visit_devices(std::slice::from_ref(root), &mut |d| match &d.kind {
        DeviceKind::Plugin { format, uid, vendor, .. } => {
            if !requires.plugins.iter().any(|n| n.format == *format && n.uid == *uid) {
                requires.plugins.push(PluginNeed { format: *format, uid: uid.clone(), name: d.name.clone(), vendor: vendor.clone() });
            }
        }
        DeviceKind::Stock { .. } => {
            for path in d.state.as_deref().map(state_paths).unwrap_or_default() {
                if !requires.samples.iter().any(|s| s.path == path) {
                    let hint = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
                    requires.samples.push(SampleNeed { path, hint, hash: None, size: None });
                }
            }
        }
        DeviceKind::Rack(_) => {}
    });
    requires
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
    use crate::command::{Command, DeviceSlot};
    use crate::ids::TrackId;

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

    fn add(p: &mut Project, slot: DeviceSlot, device: Device) {
        Command::AddDevice { slot, device: Box::new(device) }.apply(p).unwrap();
    }

    fn rack_device(id: DeviceId, plugin_id: &str, name: &str, kind: RackKind) -> Device {
        Device { kind: DeviceKind::Rack(Rack::new(kind)), ..Device::stock(id, plugin_id, name) }
    }

    fn add_chain(p: &mut Project, rack: DeviceId, index: usize, name: &str) {
        Command::AddChain { rack, index, chain: Box::new(Chain::new(name, Color::palette(index))) }.apply(p).unwrap();
    }

    /// An audio track "Bus" with an EQ and an Audio Rack of two chains. "A" has
    /// a Comp, sidechained to the track "Kick", and an Instrument Rack with a
    /// Sampler on `/s/kick.wav`. "B" has a hosted VST3 (with a state) and an LFO
    /// that drives the Comp and the EQ outside the rack. Macro 1 of the rack
    /// moves the Comp, macro 1 of the inner rack the Sampler, and the rack has
    /// one stored variation. Returns the project, the track, the rack and the Comp.
    fn rig() -> (Project, TrackId, DeviceId, DeviceId) {
        let mut p = Project::new("t");
        let (t, kick) = (p.alloc_track_id(), p.alloc_track_id());
        for (i, (id, name)) in [(t, "Bus"), (kick, "Kick")].into_iter().enumerate() {
            Command::AddTrack { track: Box::new(Track::new(id, TrackKind::Audio, name, Color::palette(i))), index: 1 + i }.apply(&mut p).unwrap();
        }
        let [out_eq, rack, comp, inner, sampler, plugin, lfo] = std::array::from_fn(|_| p.alloc_device_id());
        add(&mut p, DeviceSlot::Track { track: t, index: 0 }, Device::stock(out_eq, "fx.eq", "EQ"));
        add(&mut p, DeviceSlot::Track { track: t, index: 1 }, rack_device(rack, "rack.audio", "Audio Effect Rack", RackKind::Audio));
        add_chain(&mut p, rack, 0, "A");
        add_chain(&mut p, rack, 1, "B");
        add(&mut p, DeviceSlot::Rack { rack, chain: 0, index: 0 }, Device::stock(comp, "fx.compressor", "Comp"));
        Command::SetSidechain { device: comp, source: Some(kick) }.apply(&mut p).unwrap();
        add(&mut p, DeviceSlot::Rack { rack, chain: 0, index: 1 }, rack_device(inner, "rack.instrument", "Instrument Rack", RackKind::Instrument));
        add_chain(&mut p, inner, 0, "Sampler");
        let sampler_device = Device { state: Some(crate::slice::sampler_state(Path::new("/s/kick.wav"))), ..Device::stock(sampler, "instrument.sampler", "Sampler") };
        add(&mut p, DeviceSlot::Rack { rack: inner, chain: 0, index: 0 }, sampler_device);
        let synth = DeviceKind::Plugin { format: PluginFormat::Vst3, uid: "ABCD".into(), path: "/x.vst3".into(), vendor: "Acme".into(), is_instrument: true };
        add(&mut p, DeviceSlot::Rack { rack, chain: 1, index: 0 }, Device { kind: synth, state: Some(vec![1, 2, 3]), ..Device::stock(plugin, "", "Acme Synth") });
        add(&mut p, DeviceSlot::Rack { rack, chain: 1, index: 1 }, Device::stock(lfo, "mod.lfo", "LFO"));
        for (index, device, param) in [(0, comp, 0), (1, out_eq, 1)] {
            Command::AddModMapping { device: lfo, index, mapping: ModMapping::bipolar(ModTarget::Param { device, param }, 0.5) }.apply(&mut p).unwrap();
        }
        for (rack, device, param) in [(rack, comp, 0), (inner, sampler, 3)] {
            Command::AddMapping { rack, macro_index: 0, index: 0, mapping: MacroMapping { device, param, min: 0.0, max: 1.0 } }.apply(&mut p).unwrap();
        }
        Command::SetVariations { rack, variations: vec![Variation { name: "Soft".into(), values: vec![0.25; MACRO_COUNT] }] }.apply(&mut p).unwrap();
        (p, t, rack, comp)
    }

    #[test]
    fn a_saved_rack_instantiates_with_fresh_ids_and_its_own_links() {
        let (p, t, rack, comp) = rig();
        let saved = RackPreset::from_device(&p, rack, "My Bus", "0.0.8").unwrap();
        let back = RackPreset::from_json(&saved.to_json().unwrap()).unwrap();
        assert_eq!(back, saved);
        let mut q = p.clone();
        let d = back.instantiate(&mut q);
        assert_eq!((d.name.as_str(), d.preset.as_deref()), ("My Bus", Some("My Bus")));
        let ids = |d: &Device| {
            let mut v = Vec::new();
            visit_devices(std::slice::from_ref(d), &mut |x| v.push(x.id));
            v
        };
        let (old, new) = (ids(p.device(rack).unwrap()), ids(&d));
        assert_eq!(new.len(), old.len());
        assert!(new.iter().all(|id| !old.contains(id) && p.device(*id).is_none()), "every id is fresh");
        let DeviceKind::Rack(r) = &d.kind else { panic!() };
        let new_comp = r.chains[0].devices[0].id;
        assert_ne!(new_comp, comp);
        assert_eq!(r.macros[0].mappings[0].device, new_comp);
        assert_eq!(r.variations[0].name, "Soft");
        let DeviceKind::Rack(n) = &r.chains[0].devices[1].kind else { panic!() };
        assert_eq!(n.macros[0].mappings[0].device, n.chains[0].devices[0].id, "the nested rack's macro follows its copy");
        assert_eq!(r.chains[1].devices[0].state.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(
            r.chains[1].devices[1].mods,
            vec![ModMapping::bipolar(ModTarget::Param { device: new_comp, param: 0 }, 0.5)],
            "the target outside the rack is not saved"
        );
        visit_devices(std::slice::from_ref(&d), &mut |x| assert_eq!(x.sidechain, None));
        assert_eq!(back.requires.plugins, vec![PluginNeed { format: PluginFormat::Vst3, uid: "ABCD".into(), name: "Acme Synth".into(), vendor: "Acme".into() }]);
        assert_eq!(back.requires.samples.iter().map(|s| (s.path.clone(), s.hint.as_str())).collect::<Vec<_>>(), vec![(PathBuf::from("/s/kick.wav"), "kick.wav")]);
        // One AddDevice in the project it came from; undo is exact.
        let before = q.clone();
        let undo = Command::AddDevice { slot: DeviceSlot::Track { track: t, index: 0 }, device: Box::new(d) }.apply(&mut q).unwrap();
        undo.apply(&mut q).unwrap();
        assert_eq!(q.tracks, before.tracks);
    }

    #[test]
    fn instantiating_twice_gives_two_independent_racks() {
        let (mut p, t, rack, _) = rig();
        let saved = RackPreset::from_device(&p, rack, "Bus", "0.0.8").unwrap();
        for i in 0..2 {
            let d = saved.instantiate(&mut p);
            Command::AddDevice { slot: DeviceSlot::Track { track: t, index: i }, device: Box::new(d) }.apply(&mut p).unwrap();
        }
        let racks: Vec<&Rack> = p
            .track(t)
            .unwrap()
            .devices
            .iter()
            .take(2)
            .map(|d| match &d.kind {
                DeviceKind::Rack(r) => r,
                _ => panic!(),
            })
            .collect();
        assert_ne!(racks[0].macros[0].mappings[0].device, racks[1].macros[0].mappings[0].device);
        assert!(crate::routing::dangling_references(&p).is_empty());
    }

    #[test]
    fn what_a_rack_needs_is_listed_and_what_is_missing_is_named() {
        let (p, _, rack, _) = rig();
        let saved = RackPreset::from_device(&p, rack, "Bus", "0.0.8").unwrap();
        let gone: Vec<String> = saved.missing(|_| false, |_| false).iter().map(ToString::to_string).collect();
        assert_eq!(gone, ["plugin Acme Synth (VST3) is not installed", "sample kick.wav is missing"]);
        assert!(saved.missing(|n| n.uid == "ABCD", |f| f == Path::new("/s/kick.wav")).is_empty());
        let eq = p.tracks[1].devices[0].id;
        assert_eq!(RackPreset::from_device(&p, eq, "x", "v"), Err(RackPresetError::NotARack(eq)), "the EQ outside is not a rack");
        assert_eq!(RackPreset::from_device(&p, DeviceId(9999), "x", "v"), Err(RackPresetError::NotARack(DeviceId(9999))));
    }

    #[test]
    fn state_paths_reads_the_sampler_file_and_ignores_the_rest() {
        assert_eq!(state_paths(&crate::slice::sampler_state(Path::new("/a/b.wav"))), vec![PathBuf::from("/a/b.wav")]);
        assert_eq!(state_paths(br#"{"version":1,"extra":{"path":null,"sfz_path":"/k.sfz"}}"#), vec![PathBuf::from("/k.sfz")]);
        assert!(state_paths(b"\x00\x01 not json").is_empty() && state_paths(br#"{"extra":{"path":""}}"#).is_empty());
    }

    #[test]
    fn saving_drops_a_macro_that_drives_a_device_outside_its_rack() {
        let (mut p, _, rack, _) = rig();
        let eq = p.tracks[1].devices[0].id;
        // Not a state the commands can make, but a project can carry it (see `dangling_references`).
        let DeviceKind::Rack(r) = &mut p.device_mut(rack).unwrap().kind else { panic!() };
        r.macros[1].mappings.push(MacroMapping { device: eq, param: 0, min: 0.0, max: 1.0 });
        let saved = RackPreset::from_device(&p, rack, "Bus", "0.0.8").unwrap();
        let DeviceKind::Rack(r) = &saved.device().kind else { panic!() };
        assert!(r.macros[1].mappings.is_empty(), "the EQ is not in the rack");
        assert_eq!(r.macros[0].mappings.len(), 1, "the Comp still is");
    }

    #[test]
    fn a_file_with_links_to_ids_it_does_not_hold_loads_without_them() {
        let (p, t, rack, _) = rig();
        let mut saved = RackPreset::from_device(&p, rack, "Bus", "0.0.8").unwrap();
        // As if hand-edited: a sidechain, a macro on a device that is not there, an LFO target that is not there.
        let Payload::Device(device) = &mut saved.payload;
        let DeviceKind::Rack(r) = &mut device.kind else { panic!() };
        r.chains[0].devices[0].sidechain = Some(TrackId(2));
        r.macros[1].mappings.push(MacroMapping { device: DeviceId(9999), param: 0, min: 0.0, max: 1.0 });
        r.chains[1].devices[1].mods.push(ModMapping::bipolar(ModTarget::Param { device: DeviceId(9999), param: 0 }, 0.5));
        let mut q = p.clone();
        let d = saved.instantiate(&mut q);
        let DeviceKind::Rack(r) = &d.kind else { panic!() };
        assert_eq!((r.chains[0].devices[0].sidechain, r.macros[1].mappings.len(), r.chains[1].devices[1].mods.len()), (None, 0, 1));
        Command::AddDevice { slot: DeviceSlot::Track { track: t, index: 0 }, device: Box::new(d) }.apply(&mut q).unwrap();
        assert!(crate::routing::dangling_references(&q).is_empty());
    }
}
