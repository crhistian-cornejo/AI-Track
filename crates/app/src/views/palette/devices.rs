//! Device taxonomy for the Cmd+K palette: Instruments vs Audio Effects,
//! fine-grained subgroups, VST3/CLAP metadata + curated plugin classification,
//! user category overrides, favorites and recent devices.

use crate::bridge::ExternalPlugin;
use crate::plugin_prefs::PluginPrefs;
use aitrack_library::text::fold;
use aitrack_model::PluginFormat;

/// High-level filter on the Devices tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceKindTab {
    #[default]
    All,
    Instruments,
    AudioEffects,
    Favorites,
}

impl DeviceKindTab {
    pub const ALL: [Self; 4] = [Self::All, Self::Instruments, Self::AudioEffects, Self::Favorites];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Instruments => "Instruments",
            Self::AudioEffects => "Audio Effects",
            Self::Favorites => "Favorites",
        }
    }
}

/// Fine-grained device subgroups (`Instruments` vs `Audio Effects`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceGroup {
    // Instruments
    InstAnalog,
    InstWavetable,
    InstFm,
    InstSampler,
    InstDrums,
    InstMulti,
    // Audio Effects
    FxEq,
    FxDynamics,
    FxLimiter,
    FxSaturation,
    FxReverb,
    FxDelay,
    FxModulation,
    FxFilter,
    FxPitchTime,
    FxStereoUtility,
    FxMastering,
    FxCreative,
    FxAnalysis,
    FxModulators,
    // MIDI Effects (they run before the instrument)
    MidiEffects,
}

impl DeviceGroup {
    pub const INSTRUMENTS: [Self; 6] = [
        Self::InstAnalog,
        Self::InstWavetable,
        Self::InstFm,
        Self::InstSampler,
        Self::InstDrums,
        Self::InstMulti,
    ];

    pub const EFFECTS: [Self; 15] = [
        Self::FxEq,
        Self::FxDynamics,
        Self::FxLimiter,
        Self::FxSaturation,
        Self::FxFilter,
        Self::FxReverb,
        Self::FxDelay,
        Self::FxModulation,
        Self::FxPitchTime,
        Self::FxMastering,
        Self::FxCreative,
        Self::FxStereoUtility,
        Self::FxAnalysis,
        Self::FxModulators,
        Self::MidiEffects,
    ];

    pub const ALL: [Self; 21] = [
        Self::InstAnalog,
        Self::InstWavetable,
        Self::InstFm,
        Self::InstSampler,
        Self::InstDrums,
        Self::InstMulti,
        Self::FxEq,
        Self::FxDynamics,
        Self::FxLimiter,
        Self::FxSaturation,
        Self::FxFilter,
        Self::FxReverb,
        Self::FxDelay,
        Self::FxModulation,
        Self::FxPitchTime,
        Self::FxMastering,
        Self::FxCreative,
        Self::FxStereoUtility,
        Self::FxAnalysis,
        Self::FxModulators,
        Self::MidiEffects,
    ];

    pub fn is_instrument(self) -> bool {
        matches!(
            self,
            Self::InstAnalog
                | Self::InstWavetable
                | Self::InstFm
                | Self::InstSampler
                | Self::InstDrums
                | Self::InstMulti
        )
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::InstAnalog => "inst.analog",
            Self::InstWavetable => "inst.wavetable",
            Self::InstFm => "inst.fm",
            Self::InstSampler => "inst.sampler",
            Self::InstDrums => "inst.drums",
            Self::InstMulti => "inst.multi",
            Self::FxEq => "fx.eq",
            Self::FxDynamics => "fx.dynamics",
            Self::FxLimiter => "fx.limiter",
            Self::FxSaturation => "fx.saturation",
            Self::FxReverb => "fx.reverb",
            Self::FxDelay => "fx.delay",
            Self::FxModulation => "fx.modulation",
            Self::FxFilter => "fx.filter",
            Self::FxPitchTime => "fx.pitch_time",
            Self::FxStereoUtility => "fx.stereo_utility",
            Self::FxMastering => "fx.mastering",
            Self::FxCreative => "fx.creative",
            Self::FxAnalysis => "fx.analysis",
            Self::FxModulators => "fx.modulators",
            Self::MidiEffects => "midi.effects",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.key() == key)
    }

    /// Short chip label.
    pub fn label(self) -> &'static str {
        match self {
            Self::InstAnalog => "Analog / VA",
            Self::InstWavetable => "Wavetable / Digital",
            Self::InstFm => "FM",
            Self::InstSampler => "Sampler / Pianos",
            Self::InstDrums => "Drums",
            Self::InstMulti => "Multi-Engine",
            Self::FxEq => "EQ",
            Self::FxDynamics => "Dynamics",
            Self::FxLimiter => "Limiters",
            Self::FxSaturation => "Distortion & Saturation",
            Self::FxReverb => "Reverb",
            Self::FxDelay => "Delay",
            Self::FxModulation => "Modulation",
            Self::FxFilter => "Filter",
            Self::FxPitchTime => "Pitch & Time",
            Self::FxStereoUtility => "Stereo & Utility",
            Self::FxMastering => "Multiband & Mastering",
            Self::FxCreative => "Creative",
            Self::FxAnalysis => "Analysis",
            Self::FxModulators => "Modulators",
            Self::MidiEffects => "MIDI Effects",
        }
    }

    /// Section header label with top-level kind.
    pub fn header(self) -> &'static str {
        match self {
            Self::InstAnalog => "Instruments · Analog / VA",
            Self::InstWavetable => "Instruments · Wavetable / Digital",
            Self::InstFm => "Instruments · FM",
            Self::InstSampler => "Instruments · Sampler / Pianos",
            Self::InstDrums => "Instruments · Drums",
            Self::InstMulti => "Instruments · Multi-Engine",
            Self::FxEq => "Audio Effects · EQ",
            Self::FxDynamics => "Audio Effects · Dynamics",
            Self::FxLimiter => "Audio Effects · Limiters",
            Self::FxSaturation => "Audio Effects · Distortion & Saturation",
            Self::FxReverb => "Audio Effects · Reverb",
            Self::FxDelay => "Audio Effects · Delay",
            Self::FxModulation => "Audio Effects · Modulation",
            Self::FxFilter => "Audio Effects · Filter",
            Self::FxPitchTime => "Audio Effects · Pitch & Time",
            Self::FxStereoUtility => "Audio Effects · Stereo & Utility",
            Self::FxMastering => "Audio Effects · Multiband & Mastering",
            Self::FxCreative => "Audio Effects · Creative",
            Self::FxAnalysis => "Audio Effects · Analysis",
            Self::FxModulators => "Audio Effects · Modulators",
            Self::MidiEffects => "MIDI Effects",
        }
    }
}

/// Stable persistence key for a stock device or scanned VST3/CLAP plugin.
pub fn stock_key(id: &str) -> String {
    format!("stock:{id}")
}

pub fn external_key(format: PluginFormat, uid: &str) -> String {
    let f = match format {
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Clap => "clap",
    };
    format!("ext:{f}:{uid}")
}

/// Curated known-plugin overrides (`lowercase substring -> DeviceGroup`).
const KNOWN_OVERRIDES: &[(&str, DeviceGroup)] = &[
    // Instruments
    ("serum", DeviceGroup::InstWavetable),
    ("vital", DeviceGroup::InstWavetable),
    ("surge xt", DeviceGroup::InstWavetable),
    ("massive", DeviceGroup::InstWavetable),
    ("pigments", DeviceGroup::InstMulti),
    ("phase plant", DeviceGroup::InstMulti),
    ("omnisphere", DeviceGroup::InstMulti),
    ("ob-xf", DeviceGroup::InstAnalog),
    ("ob-xd", DeviceGroup::InstAnalog),
    ("diva", DeviceGroup::InstAnalog),
    ("repro", DeviceGroup::InstAnalog),
    ("tal-u-no", DeviceGroup::InstAnalog),
    ("juno", DeviceGroup::InstAnalog),
    ("monark", DeviceGroup::InstAnalog),
    ("dexed", DeviceGroup::InstFm),
    ("fm8", DeviceGroup::InstFm),
    ("opsix", DeviceGroup::InstFm),
    ("sfizz", DeviceGroup::InstSampler),
    ("decent sampler", DeviceGroup::InstSampler),
    ("decentsampler", DeviceGroup::InstSampler),
    ("sforzando", DeviceGroup::InstSampler),
    ("kontakt", DeviceGroup::InstSampler),
    ("pianoteq", DeviceGroup::InstSampler),
    ("keyscape", DeviceGroup::InstSampler),
    ("labs", DeviceGroup::InstSampler),
    ("kick 2", DeviceGroup::InstDrums),
    ("kick 3", DeviceGroup::InstDrums),
    ("microtonic", DeviceGroup::InstDrums),
    ("addictive drums", DeviceGroup::InstDrums),
    ("battery", DeviceGroup::InstDrums),
    ("xo", DeviceGroup::InstDrums),
    // Audio Effects
    ("zl equalizer", DeviceGroup::FxEq),
    ("pro-q", DeviceGroup::FxEq),
    ("tdr nova", DeviceGroup::FxEq),
    ("eq", DeviceGroup::FxEq),
    ("pro-l", DeviceGroup::FxLimiter),
    ("limiter", DeviceGroup::FxLimiter),
    ("maximizer", DeviceGroup::FxLimiter),
    ("barricade", DeviceGroup::FxLimiter),
    ("ott", DeviceGroup::FxDynamics),
    ("pro-c", DeviceGroup::FxDynamics),
    ("kotelnikov", DeviceGroup::FxDynamics),
    ("roughrider", DeviceGroup::FxDynamics),
    ("compressor", DeviceGroup::FxDynamics),
    ("chow tape", DeviceGroup::FxSaturation),
    ("chowtape", DeviceGroup::FxSaturation),
    ("saturn", DeviceGroup::FxSaturation),
    ("decapitator", DeviceGroup::FxSaturation),
    ("thermal", DeviceGroup::FxSaturation),
    ("clipper", DeviceGroup::FxSaturation),
    ("saturat", DeviceGroup::FxSaturation),
    ("distort", DeviceGroup::FxSaturation),
    ("overdrive", DeviceGroup::FxSaturation),
    ("fuzz", DeviceGroup::FxSaturation),
    ("cloudreverb", DeviceGroup::FxReverb),
    ("supermassive", DeviceGroup::FxReverb),
    ("valhalla", DeviceGroup::FxReverb),
    ("dragonfly", DeviceGroup::FxReverb),
    ("raumm", DeviceGroup::FxReverb),
    ("orilriver", DeviceGroup::FxReverb),
    ("reverb", DeviceGroup::FxReverb),
    ("echoboy", DeviceGroup::FxDelay),
    ("timeless", DeviceGroup::FxDelay),
    ("delay", DeviceGroup::FxDelay),
    ("tal-chorus", DeviceGroup::FxModulation),
    ("chorus", DeviceGroup::FxModulation),
    ("microshift", DeviceGroup::FxModulation),
    ("pancake", DeviceGroup::FxModulation),
    ("volcano", DeviceGroup::FxFilter),
    ("auto-filter", DeviceGroup::FxFilter),
    ("soundtoys filterfreak", DeviceGroup::FxFilter),
    ("little alterboy", DeviceGroup::FxPitchTime),
    ("autotune", DeviceGroup::FxPitchTime),
    ("melodyne", DeviceGroup::FxPitchTime),
    ("pitch", DeviceGroup::FxPitchTime),
    ("ozone", DeviceGroup::FxMastering),
    ("gate12", DeviceGroup::FxCreative),
    ("shaperbox", DeviceGroup::FxCreative),
    ("infiltrator", DeviceGroup::FxCreative),
    ("portal", DeviceGroup::FxCreative),
    ("effectrix", DeviceGroup::FxCreative),
    ("span", DeviceGroup::FxAnalysis),
    ("youlean", DeviceGroup::FxAnalysis),
    ("insight", DeviceGroup::FxAnalysis),
];

pub fn classify_stock(plugin_id: &str, prefs: &PluginPrefs) -> DeviceGroup {
    let key = stock_key(plugin_id);
    if let Some(group) = prefs.category_overrides.get(&key).and_then(|k| DeviceGroup::from_key(k)) {
        return group;
    }
    stock_group(plugin_id).unwrap_or(DeviceGroup::FxStereoUtility)
}

/// The built-in group of a stock device or rack, or `None` for an id this
/// table does not know (a test requires every registry id to be listed).
pub fn stock_group(plugin_id: &str) -> Option<DeviceGroup> {
    Some(match plugin_id {
        "instrument.synth" | "instrument.acid303" => DeviceGroup::InstAnalog,
        "instrument.wavetable" => DeviceGroup::InstWavetable,
        "instrument.sampler" | "instrument.granular" => DeviceGroup::InstSampler,
        "instrument.drum" | "instrument.kick808" => DeviceGroup::InstDrums,
        "instrument.vault" | "rack.instrument" => DeviceGroup::InstMulti,
        "rack.drum" => DeviceGroup::InstDrums,
        "fx.eq" => DeviceGroup::FxEq,
        "fx.compressor" | "fx.buscomp" | "fx.gate" | "fx.transient" | "fx.drumbus" => DeviceGroup::FxDynamics,
        "fx.limiter" => DeviceGroup::FxLimiter,
        "fx.saturator" | "fx.tape" | "fx.amp" | "fx.console" | "fx.clipper" | "fx.crusher" => DeviceGroup::FxSaturation,
        "fx.reverb" => DeviceGroup::FxReverb,
        "fx.delay" => DeviceGroup::FxDelay,
        "fx.chorus" | "fx.autopan" => DeviceGroup::FxModulation,
        "fx.shaper" | "fx.stutter" => DeviceGroup::FxCreative,
        "fx.pitchshift" | "fx.freqshift" | "fx.vocoder" => DeviceGroup::FxPitchTime,
        "fx.filter" => DeviceGroup::FxFilter,
        "fx.multiband" | "fx.mbdyn" => DeviceGroup::FxMastering,
        "fx.analyzer" | "fx.drum_extract" => DeviceGroup::FxAnalysis,
        id if id.starts_with("mod.") => DeviceGroup::FxModulators,
        "fx.midside" | "fx.utility" | "rack.audio" => DeviceGroup::FxStereoUtility,
        id if id.starts_with("midi.") || id == "rack.midi" => DeviceGroup::MidiEffects,
        _ => return None,
    })
}

pub fn classify_external(
    format: PluginFormat,
    uid: &str,
    name: &str,
    vendor: &str,
    metadata_category: &str,
    is_instrument: bool,
    prefs: &PluginPrefs,
) -> DeviceGroup {
    let key = external_key(format, uid);
    if let Some(group) = prefs.category_overrides.get(&key).and_then(|k| DeviceGroup::from_key(k)) {
        return group;
    }
    let meta = metadata_category.to_lowercase();
    let lower_name = name.to_lowercase();
    let lower_vendor = vendor.to_lowercase();

    // 1. Metadata tokens from VST3 subcategories / CLAP features.
    if !meta.is_empty() {
        for tok in meta.split(['|', ';', ',', '/']).map(str::trim).filter(|t| !t.is_empty()) {
            if let Some(g) = match_metadata_token(tok, is_instrument) {
                return g;
            }
        }
    }

    // 2. Curated plugin table (specific well-known synths/effects + keyword hints).
    for &(needle, group) in KNOWN_OVERRIDES {
        if (lower_name.contains(needle) || lower_vendor.contains(needle)) && group.is_instrument() == is_instrument {
            return group;
        }
    }

    // 3. Sensible fallback by top-level instrument vs effect flag.
    if is_instrument { DeviceGroup::InstMulti } else { DeviceGroup::FxStereoUtility }
}

fn match_metadata_token(tok: &str, is_instrument: bool) -> Option<DeviceGroup> {
    if is_instrument {
        if tok.contains("drum") || tok.contains("percuss") {
            return Some(DeviceGroup::InstDrums);
        }
        if tok.contains("sampler") || tok.contains("piano") || tok.contains("organ") || tok.contains("keys") {
            return Some(DeviceGroup::InstSampler);
        }
        if tok.contains("wavetable") {
            return Some(DeviceGroup::InstWavetable);
        }
        if tok.contains("fm") {
            return Some(DeviceGroup::InstFm);
        }
        if tok.contains("analog") || tok.contains("virtual-analog") {
            return Some(DeviceGroup::InstAnalog);
        }
        return None;
    }

    if tok == "eq" || tok.contains("equaliz") {
        Some(DeviceGroup::FxEq)
    } else if tok.contains("limit") || tok.contains("maximiz") {
        Some(DeviceGroup::FxLimiter)
    } else if tok.contains("dynamic") || tok.contains("compress") || tok == "gate" || tok.contains("transient") {
        Some(DeviceGroup::FxDynamics)
    } else if tok.contains("distort") || tok.contains("saturat") || tok.contains("overdrive") || tok.contains("amp") || tok.contains("tape") || tok.contains("clip") {
        Some(DeviceGroup::FxSaturation)
    } else if tok.contains("reverb") {
        Some(DeviceGroup::FxReverb)
    } else if tok.contains("delay") || tok.contains("echo") {
        Some(DeviceGroup::FxDelay)
    } else if tok.contains("modulat") || tok.contains("chorus") || tok.contains("flanger") || tok.contains("phaser") || tok.contains("tremolo") {
        Some(DeviceGroup::FxModulation)
    } else if tok.contains("filter") {
        Some(DeviceGroup::FxFilter)
    } else if tok.contains("pitch") || tok.contains("time") || tok.contains("stretch") {
        Some(DeviceGroup::FxPitchTime)
    } else if tok.contains("master") || tok.contains("multiband") {
        Some(DeviceGroup::FxMastering)
    } else if tok.contains("analy") || tok.contains("meter") || tok.contains("scope") {
        Some(DeviceGroup::FxAnalysis)
    } else if tok.contains("spatial") || tok.contains("stereo") || tok.contains("utility") || tok.contains("tools") || tok.contains("gain") {
        Some(DeviceGroup::FxStereoUtility)
    } else if tok.contains("glitch") || tok.contains("granular") || tok.contains("creative") {
        Some(DeviceGroup::FxCreative)
    } else {
        None
    }
}

// ───────────────────────────── One row per plugin ─────────────────────────────

/// Words a maker adds to a file name that are not part of the plugin's name.
const NAME_NOISE: &[&str] = &["vst3", "vst", "clap", "x64", "x86", "64bit", "64-bit", "(vst3)", "(clap)", "(x64)", "(64-bit)", "[vst3]", "[clap]"];

/// A plugin's name as two formats of it would agree on: folded, without
/// "(VST3)" or "x64" tags, single-spaced.
pub fn plugin_name_key(name: &str) -> String {
    fold(name).split_whitespace().filter(|w| !NAME_NOISE.contains(w)).collect::<Vec<_>>().join(" ")
}

/// Two makers' names that are the same one: equal when folded, or one is
/// the start of the other ("Xfer" and "Xfer Records"), or one is not known.
fn same_vendor(a: &str, b: &str) -> bool {
    let (a, b) = (fold(a), fold(b));
    let (a, b) = (a.trim(), b.trim());
    a.is_empty() || b.is_empty() || a == b || a.starts_with(b) || b.starts_with(a)
}

/// The VST3 and CLAP builds of one plugin, which the palette shows as a single row.
#[derive(Clone, Debug)]
pub struct PluginGroup<'a> {
    /// Preferred format first (VST3, then CLAP).
    members: Vec<&'a ExternalPlugin>,
}

impl<'a> PluginGroup<'a> {
    pub fn preferred(&self) -> &'a ExternalPlugin {
        self.members[0]
    }

    pub fn members(&self) -> &[&'a ExternalPlugin] {
        &self.members
    }

    pub fn name(&self) -> &'a str {
        &self.preferred().name
    }

    /// The maker, from the first build that names one.
    pub fn vendor(&self) -> &'a str {
        self.members.iter().map(|m| m.vendor.as_str()).find(|v| !v.trim().is_empty()).unwrap_or("")
    }

    pub fn formats(&self) -> Vec<PluginFormat> {
        self.members.iter().map(|m| m.format).collect()
    }
}

fn format_rank(f: PluginFormat) -> u8 {
    match f {
        PluginFormat::Vst3 => 0,
        PluginFormat::Clap => 1,
    }
}

/// Group plugins that are the same one in other formats. Two builds of one
/// format are never merged (they are different plugins that share a name).
/// The order of first appearance is kept.
pub fn group_plugins<'a>(plugins: impl IntoIterator<Item = &'a ExternalPlugin>) -> Vec<PluginGroup<'a>> {
    let mut groups: Vec<(String, PluginGroup<'a>)> = Vec::new();
    for p in plugins {
        let key = plugin_name_key(&p.name);
        let hit = groups.iter_mut().find(|(k, g)| *k == key && g.members.iter().all(|m| m.format != p.format) && g.members.iter().any(|m| same_vendor(&m.vendor, &p.vendor)));
        match hit {
            Some((_, g)) => {
                g.members.push(p);
                g.members.sort_by_key(|m| format_rank(m.format));
            }
            None => groups.push((key, PluginGroup { members: vec![p] })),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

/// Which builds the Devices tab shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormatFilter {
    #[default]
    All,
    Stock,
    Vst3,
    Clap,
}

impl FormatFilter {
    pub const ALL: [Self; 4] = [Self::All, Self::Stock, Self::Vst3, Self::Clap];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Any format",
            Self::Stock => "Stock",
            Self::Vst3 => "VST3",
            Self::Clap => "CLAP",
        }
    }

    /// A row with these format badges passes (`"Stock"`, `"VST3"`, `"CLAP"`).
    pub fn admits(self, formats: &[&str]) -> bool {
        match self {
            Self::All => true,
            Self::Stock => formats.contains(&"Stock"),
            Self::Vst3 => formats.contains(&"VST3"),
            Self::Clap => formats.contains(&"CLAP"),
        }
    }
}

/// The makers in `vendors` with how many rows each has, most first (case
/// and accents folded together; the first spelling is the one shown).
pub fn vendor_counts<'a>(vendors: impl IntoIterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, String, usize)> = Vec::new();
    for v in vendors {
        let key = fold(v);
        if key.trim().is_empty() {
            continue;
        }
        match counts.iter_mut().find(|(k, _, _)| *k == key) {
            Some(c) => c.2 += 1,
            None => counts.push((key, v.trim().to_owned(), 1)),
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().map(|(_, v, n)| (v, n)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| fold(&a.0).cmp(&fold(&b.0))));
    out
}

/// `true` when the row is a favourite under any of its builds' keys.
pub fn is_favorite(prefs: &PluginPrefs, keys: &[String]) -> bool {
    keys.iter().any(|k| prefs.is_favorite(k))
}

/// Star or unstar a row under all its builds' keys, so a plugin starred as
/// CLAP is still starred when it shows as one row with its VST3.
pub fn toggle_favorite(prefs: &mut PluginPrefs, keys: &[String]) -> bool {
    if is_favorite(prefs, keys) {
        let starred: Vec<&String> = keys.iter().filter(|k| prefs.is_favorite(k)).collect();
        for k in starred {
            prefs.toggle_favorite(k);
        }
        false
    } else {
        if let Some(k) = keys.first() {
            prefs.toggle_favorite(k);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_plugins_classify_into_expected_groups() {
        let prefs = PluginPrefs::default();
        assert_eq!(classify_external(PluginFormat::Vst3, "u1", "Serum", "Xfer", "", true, &prefs), DeviceGroup::InstWavetable);
        assert_eq!(classify_external(PluginFormat::Vst3, "u2", "Dexed", "Digital Suburban", "", true, &prefs), DeviceGroup::InstFm);
        assert_eq!(classify_external(PluginFormat::Clap, "u3", "OB-Xf", "Surge Synth Team", "instrument|synthesizer", true, &prefs), DeviceGroup::InstAnalog);
        assert_eq!(classify_external(PluginFormat::Vst3, "u4", "sfizz", "SFZ Tools", "", true, &prefs), DeviceGroup::InstSampler);
        assert_eq!(classify_external(PluginFormat::Vst3, "u5", "CloudReverb", "Cloud", "", false, &prefs), DeviceGroup::FxReverb);
        assert_eq!(classify_external(PluginFormat::Vst3, "u6", "ZL Equalizer", "ZL", "", false, &prefs), DeviceGroup::FxEq);
        assert_eq!(classify_external(PluginFormat::Vst3, "u7", "gate12", "Gate", "", false, &prefs), DeviceGroup::FxCreative);
        assert_eq!(classify_external(PluginFormat::Vst3, "u8", "FabFilter Pro-L 2", "FabFilter", "", false, &prefs), DeviceGroup::FxLimiter);
        assert_eq!(classify_stock("fx.limiter", &prefs), DeviceGroup::FxLimiter);
        assert_eq!(classify_stock("fx.analyzer", &prefs), DeviceGroup::FxAnalysis);
    }

    #[test]
    fn user_override_beats_metadata_and_curated_rules() {
        let mut prefs = PluginPrefs::default();
        let key = external_key(PluginFormat::Vst3, "u-eq");
        prefs.category_overrides.insert(key, DeviceGroup::FxCreative.key().into());
        assert_eq!(
            classify_external(PluginFormat::Vst3, "u-eq", "ZL Equalizer", "ZL", "Fx|EQ", false, &prefs),
            DeviceGroup::FxCreative
        );
    }

    #[test]
    fn midi_effects_have_their_own_group_among_the_effects() {
        let prefs = PluginPrefs::default();
        for id in ["midi.arp", "midi.chord", "midi.scale", "midi.velocity"] {
            assert_eq!(classify_stock(id, &prefs), DeviceGroup::MidiEffects, "{id}");
        }
        assert!(!DeviceGroup::MidiEffects.is_instrument());
        assert!(DeviceGroup::EFFECTS.contains(&DeviceGroup::MidiEffects) && DeviceGroup::ALL.contains(&DeviceGroup::MidiEffects));
        assert_eq!(DeviceGroup::from_key(DeviceGroup::MidiEffects.key()), Some(DeviceGroup::MidiEffects));
    }

    #[test]
    fn every_stock_device_has_an_explicit_group_of_its_own_kind() {
        let prefs = PluginPrefs::default();
        for p in aitrack_stock::registry() {
            let group = stock_group(p.id).unwrap_or_else(|| panic!("{} is not listed in stock_group", p.id));
            assert_eq!(classify_stock(p.id, &prefs), group);
            let kind = p.category;
            match kind {
                aitrack_dsp::Category::Instrument => assert!(group.is_instrument(), "{} is an instrument but sits in {group:?}", p.id),
                aitrack_dsp::Category::MidiEffect => assert_eq!(group, DeviceGroup::MidiEffects, "{}", p.id),
                _ => assert!(!group.is_instrument() && group != DeviceGroup::MidiEffects, "{} is an audio effect but sits in {group:?}", p.id),
            }
        }
        assert_eq!(classify_stock("fx.buscomp", &prefs), DeviceGroup::FxDynamics);
        assert_eq!(classify_stock("fx.mbdyn", &prefs), DeviceGroup::FxMastering);
        assert_eq!(stock_group("fx.unknown"), None);
    }

    fn ext(name: &str, vendor: &str, format: PluginFormat) -> ExternalPlugin {
        ExternalPlugin {
            uid: format!("{name}-{format:?}"),
            name: name.into(),
            vendor: vendor.into(),
            format,
            category: String::new(),
            path: std::path::PathBuf::from(format!("/{name}")),
            is_instrument: false,
        }
    }

    #[test]
    fn the_same_plugin_in_two_formats_is_one_row_with_both_badges() {
        let list = [
            ext("Surge XT", "Surge Synth Team", PluginFormat::Clap),
            ext("Dexed", "Digital Suburban", PluginFormat::Vst3),
            ext("Surge XT", "Surge Synth Team", PluginFormat::Vst3),
            ext("Dexed", "Digital Suburban", PluginFormat::Clap),
            ext("OTT", "Xfer Records", PluginFormat::Vst3),
        ];
        let g = group_plugins(list.iter());
        assert_eq!(g.len(), 3, "five files, three plugins");
        let surge = &g[0];
        assert_eq!(surge.formats(), vec![PluginFormat::Vst3, PluginFormat::Clap], "VST3 first, it is the one that opens");
        assert_eq!(surge.preferred().format, PluginFormat::Vst3);
        assert_eq!(surge.members().len(), 2);
        assert_eq!(g[2].formats(), vec![PluginFormat::Vst3]);
    }

    #[test]
    fn tags_in_the_name_and_a_shortened_maker_still_match() {
        let list = [ext("Vital (VST3)", "Vital Audio", PluginFormat::Vst3), ext("Vital x64", "", PluginFormat::Clap), ext("Pro Thing", "Xfer", PluginFormat::Vst3), ext("pro  thing", "Xfer Records", PluginFormat::Clap)];
        let g = group_plugins(list.iter());
        assert_eq!(g.len(), 2, "{g:?}");
        assert_eq!(g[0].vendor(), "Vital Audio", "the maker from the build that has one");
    }

    #[test]
    fn two_builds_of_one_format_or_two_makers_stay_apart() {
        let list = [
            ext("Delay", "Maker A", PluginFormat::Vst3),
            ext("Delay", "Maker B", PluginFormat::Vst3),
            ext("Delay", "Maker B", PluginFormat::Clap),
            ext("Reverb", "Maker A", PluginFormat::Clap),
            ext("Reverb", "Other Co", PluginFormat::Vst3),
        ];
        let g = group_plugins(list.iter());
        assert_eq!(g.len(), 4, "{g:?}");
        // The CLAP Delay joins the VST3 of its own maker, not the first one with the name.
        let delay_b = g.iter().find(|x| x.vendor() == "Maker B").unwrap();
        assert_eq!(delay_b.formats().len(), 2);
    }

    #[test]
    fn format_filter_and_maker_counts() {
        assert!(FormatFilter::Clap.admits(&["VST3", "CLAP"]));
        assert!(!FormatFilter::Vst3.admits(&["CLAP"]));
        assert!(FormatFilter::Stock.admits(&["Stock"]) && !FormatFilter::Stock.admits(&["VST3"]));
        assert!(FormatFilter::All.admits(&[]));
        let v = vendor_counts(["Xfer Records", "xfer records", "Valhalla DSP", "", "Xfer Records", "Zynaptiq"]);
        assert_eq!(v[0], ("Xfer Records".to_owned(), 3));
        assert_eq!(v.len(), 3, "no empty maker");
    }

    #[test]
    fn a_favourite_in_one_format_is_a_favourite_of_the_row() {
        let mut prefs = PluginPrefs::default();
        let keys = vec!["ext:vst3:x".to_owned(), "ext:clap:x".to_owned()];
        assert!(!is_favorite(&prefs, &keys));
        // (Not toggled here: that saves the user's file.)
        prefs.favorites.push("ext:clap:x".into());
        assert!(is_favorite(&prefs, &keys));
    }
}
