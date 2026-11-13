//! Device taxonomy for the Cmd+K palette: Instruments vs Audio Effects,
//! fine-grained subgroups, VST3/CLAP metadata + curated plugin classification,
//! user category overrides, favorites and recent devices.

use crate::plugin_prefs::PluginPrefs;
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

    pub const EFFECTS: [Self; 13] = [
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
    ];

    pub const ALL: [Self; 19] = [
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
    match plugin_id {
        "instrument.synth" | "instrument.acid303" => DeviceGroup::InstAnalog,
        "instrument.wavetable" => DeviceGroup::InstWavetable,
        "instrument.sampler" => DeviceGroup::InstSampler,
        "instrument.drum" | "instrument.kick808" => DeviceGroup::InstDrums,
        "instrument.vault" | "rack.instrument" => DeviceGroup::InstMulti,
        "rack.drum" => DeviceGroup::InstDrums,
        "fx.eq" => DeviceGroup::FxEq,
        "fx.compressor" | "fx.gate" | "fx.transient" => DeviceGroup::FxDynamics,
        "fx.limiter" => DeviceGroup::FxLimiter,
        "fx.saturator" | "fx.tape" | "fx.amp" | "fx.console" | "fx.clipper" => DeviceGroup::FxSaturation,
        "fx.shaper" => DeviceGroup::FxCreative,
        "fx.reverb" => DeviceGroup::FxReverb,
        "fx.delay" => DeviceGroup::FxDelay,
        "fx.chorus" => DeviceGroup::FxModulation,
        "fx.filter" => DeviceGroup::FxFilter,
        "fx.multiband" => DeviceGroup::FxMastering,
        "fx.analyzer" => DeviceGroup::FxAnalysis,
        "fx.midside" | "fx.utility" | "rack.audio" => DeviceGroup::FxStereoUtility,
        _ => DeviceGroup::FxStereoUtility,
    }
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
}
