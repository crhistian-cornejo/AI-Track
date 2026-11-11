//! Heuristic sample categorisation from file-name keywords (several
//! languages) and audio features. A learned classifier (CLAP embeddings) can
//! replace the feature half later; the keyword half stays useful either way.

use serde::{Deserialize, Serialize};

use crate::features::Features;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleCategory {
    Kick,
    Snare,
    Clap,
    HiHat,
    Perc,
    Cymbal,
    Tom,
    Bass808,
    Bass,
    Synth,
    Pad,
    Lead,
    Pluck,
    Keys,
    Vocal,
    Fx,
    Riser,
    Impact,
    DrumLoop,
    PercLoop,
    BassLoop,
    MelodicLoop,
    VocalLoop,
    FxLoop,
    Unknown,
}

impl SampleCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::Clap => "Clap",
            Self::HiHat => "Hi-Hat",
            Self::Perc => "Percussion",
            Self::Cymbal => "Cymbal",
            Self::Tom => "Tom",
            Self::Bass808 => "808",
            Self::Bass => "Bass",
            Self::Synth => "Synth",
            Self::Pad => "Pad",
            Self::Lead => "Lead",
            Self::Pluck => "Pluck",
            Self::Keys => "Keys",
            Self::Vocal => "Vocal",
            Self::Fx => "FX",
            Self::Riser => "Riser",
            Self::Impact => "Impact",
            Self::DrumLoop => "Drum Loop",
            Self::PercLoop => "Percussion Loop",
            Self::BassLoop => "Bass Loop",
            Self::MelodicLoop => "Melodic Loop",
            Self::VocalLoop => "Vocal Loop",
            Self::FxLoop => "FX Loop",
            Self::Unknown => "Unknown",
        }
    }

    pub fn is_drum(self) -> bool {
        matches!(
            self,
            Self::Kick
                | Self::Snare
                | Self::Clap
                | Self::HiHat
                | Self::Perc
                | Self::Cymbal
                | Self::Tom
        )
    }

    pub fn is_loop(self) -> bool {
        matches!(
            self,
            Self::DrumLoop
                | Self::PercLoop
                | Self::BassLoop
                | Self::MelodicLoop
                | Self::VocalLoop
                | Self::FxLoop
        )
    }

    /// Tonal material where a detected key is meaningful.
    pub fn is_tonal(self) -> bool {
        matches!(
            self,
            Self::Bass808
                | Self::Bass
                | Self::Synth
                | Self::Pad
                | Self::Lead
                | Self::Pluck
                | Self::Keys
                | Self::Vocal
                | Self::BassLoop
                | Self::MelodicLoop
                | Self::VocalLoop
        )
    }

    /// The loop counterpart of a one-shot category.
    pub fn as_loop(self) -> Self {
        match self {
            Self::Kick
            | Self::Snare
            | Self::Clap
            | Self::HiHat
            | Self::Cymbal
            | Self::Tom
            | Self::DrumLoop => Self::DrumLoop,
            Self::Perc | Self::PercLoop => Self::PercLoop,
            Self::Bass808 | Self::Bass | Self::BassLoop => Self::BassLoop,
            Self::Vocal | Self::VocalLoop => Self::VocalLoop,
            Self::Fx | Self::Riser | Self::Impact | Self::FxLoop => Self::FxLoop,
            Self::Synth | Self::Pad | Self::Lead | Self::Pluck | Self::Keys | Self::MelodicLoop => {
                Self::MelodicLoop
            }
            Self::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySource {
    Filename,
    Features,
    Both,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub category: SampleCategory,
    pub subcategory: Option<String>,
    pub confidence: f32,
    pub source: CategorySource,
}

use SampleCategory as C;

/// (keyword, category, subcategory). Matched against lower-cased tokens of the
/// file name and parent folders. Entries of three letters or fewer must match a
/// whole token; longer ones may also match inside a token ("deepkick").
const KEYWORDS: &[(&str, SampleCategory, Option<&str>)] = &[
    // Loops first so "drum loop" wins over "drum".
    ("drumloop", C::DrumLoop, None),
    ("drum loop", C::DrumLoop, None),
    ("beatloop", C::DrumLoop, None),
    ("toploop", C::DrumLoop, Some("top")),
    ("top loop", C::DrumLoop, Some("top")),
    ("breakbeat", C::DrumLoop, Some("break")),
    ("percloop", C::PercLoop, None),
    ("perc loop", C::PercLoop, None),
    ("bassloop", C::BassLoop, None),
    ("bass loop", C::BassLoop, None),
    ("vocal loop", C::VocalLoop, None),
    ("vox loop", C::VocalLoop, None),
    ("vocalloop", C::VocalLoop, None),
    // Kicks (en, es, pt, fr, it, de).
    ("kick", C::Kick, None),
    ("kik", C::Kick, None),
    ("kck", C::Kick, None),
    ("bd", C::Kick, None),
    ("bassdrum", C::Kick, None),
    ("bombo", C::Kick, None),
    ("bumbo", C::Kick, None),
    ("grosse caisse", C::Kick, None),
    ("cassa", C::Kick, None),
    ("grancassa", C::Kick, None),
    ("trommel", C::Kick, None),
    // Snares / claps / rims.
    ("snare", C::Snare, None),
    ("snr", C::Snare, None),
    ("sd", C::Snare, None),
    ("caja", C::Snare, None),
    ("redoblante", C::Snare, None),
    ("tarola", C::Snare, None),
    ("caixa", C::Snare, None),
    ("caisse claire", C::Snare, None),
    ("rullante", C::Snare, None),
    ("rimshot", C::Perc, Some("rim")),
    ("rim", C::Perc, Some("rim")),
    ("sidestick", C::Perc, Some("rim")),
    ("clap", C::Clap, None),
    ("clp", C::Clap, None),
    ("handclap", C::Clap, None),
    ("palmas", C::Clap, None),
    ("palma", C::Clap, None),
    ("aplauso", C::Clap, None),
    ("klatsch", C::Clap, None),
    ("snap", C::Clap, Some("snap")),
    ("chasquido", C::Clap, Some("snap")),
    // Hats and cymbals.
    ("open hat", C::HiHat, Some("open")),
    ("openhat", C::HiHat, Some("open")),
    ("ohh", C::HiHat, Some("open")),
    ("oh", C::HiHat, Some("open")),
    ("closed hat", C::HiHat, Some("closed")),
    ("closedhat", C::HiHat, Some("closed")),
    ("chh", C::HiHat, Some("closed")),
    ("ch", C::HiHat, Some("closed")),
    ("hihat", C::HiHat, None),
    ("hi-hat", C::HiHat, None),
    ("hi hat", C::HiHat, None),
    ("hh", C::HiHat, None),
    ("hat", C::HiHat, None),
    ("hats", C::HiHat, None),
    ("charles", C::HiHat, None),
    ("charleston", C::HiHat, None),
    ("chimbal", C::HiHat, None),
    ("crash", C::Cymbal, Some("crash")),
    ("ride", C::Cymbal, Some("ride")),
    ("splash", C::Cymbal, Some("splash")),
    ("china", C::Cymbal, Some("china")),
    ("cymbal", C::Cymbal, None),
    ("cym", C::Cymbal, None),
    ("platillo", C::Cymbal, None),
    ("plato", C::Cymbal, None),
    ("becken", C::Cymbal, None),
    ("piatto", C::Cymbal, None),
    ("prato", C::Cymbal, None),
    ("cymbale", C::Cymbal, None),
    // Toms and percussion.
    ("floor tom", C::Tom, Some("floor")),
    ("tom", C::Tom, None),
    ("toms", C::Tom, None),
    ("surdo", C::Tom, None),
    ("conga", C::Perc, Some("conga")),
    ("bongo", C::Perc, Some("bongo")),
    ("shaker", C::Perc, Some("shaker")),
    ("maraca", C::Perc, Some("shaker")),
    ("tambourine", C::Perc, Some("tambourine")),
    ("pandereta", C::Perc, Some("tambourine")),
    ("pandeiro", C::Perc, Some("tambourine")),
    ("tamb", C::Perc, Some("tambourine")),
    ("guiro", C::Perc, Some("guiro")),
    ("güiro", C::Perc, Some("guiro")),
    ("cowbell", C::Perc, Some("cowbell")),
    ("cencerro", C::Perc, Some("cowbell")),
    ("clave", C::Perc, Some("clave")),
    ("timbal", C::Perc, Some("timbale")),
    ("timbales", C::Perc, Some("timbale")),
    ("woodblock", C::Perc, Some("block")),
    ("block", C::Perc, Some("block")),
    ("triangle", C::Perc, Some("triangle")),
    ("triangulo", C::Perc, Some("triangle")),
    ("perc", C::Perc, None),
    ("percussion", C::Perc, None),
    ("percusion", C::Perc, None),
    ("percusión", C::Perc, None),
    // Bass.
    ("808", C::Bass808, None),
    ("sub bass", C::Bass, Some("sub")),
    ("subbass", C::Bass, Some("sub")),
    ("reese", C::Bass, Some("reese")),
    ("bass", C::Bass, None),
    ("bajo", C::Bass, None),
    ("basse", C::Bass, None),
    ("baixo", C::Bass, None),
    ("basso", C::Bass, None),
    ("sub", C::Bass, Some("sub")),
    // Melodic.
    ("pad", C::Pad, None),
    ("pads", C::Pad, None),
    ("atmos", C::Pad, Some("atmosphere")),
    ("atmosphere", C::Pad, Some("atmosphere")),
    ("ambient", C::Pad, Some("ambient")),
    ("drone", C::Pad, Some("drone")),
    ("texture", C::Pad, Some("texture")),
    ("textura", C::Pad, Some("texture")),
    ("pluck", C::Pluck, None),
    ("plk", C::Pluck, None),
    ("pizz", C::Pluck, Some("pizzicato")),
    ("bell", C::Pluck, Some("bell")),
    ("mallet", C::Pluck, Some("mallet")),
    ("marimba", C::Pluck, Some("mallet")),
    ("kalimba", C::Pluck, Some("kalimba")),
    ("campana", C::Pluck, Some("bell")),
    ("lead", C::Lead, None),
    ("melody", C::Lead, Some("melody")),
    ("melodia", C::Lead, Some("melody")),
    ("melodía", C::Lead, Some("melody")),
    ("hook", C::Lead, Some("hook")),
    ("arp", C::Synth, Some("arp")),
    ("stab", C::Synth, Some("stab")),
    ("chord", C::Synth, Some("chord")),
    ("chords", C::Synth, Some("chord")),
    ("acorde", C::Synth, Some("chord")),
    ("acordes", C::Synth, Some("chord")),
    ("synth", C::Synth, None),
    ("sintetizador", C::Synth, None),
    ("sinte", C::Synth, None),
    ("piano", C::Keys, Some("piano")),
    ("keys", C::Keys, None),
    ("rhodes", C::Keys, Some("electric piano")),
    ("organ", C::Keys, Some("organ")),
    ("organo", C::Keys, Some("organ")),
    ("órgano", C::Keys, Some("organ")),
    ("teclado", C::Keys, None),
    ("guitar", C::MelodicLoop, Some("guitar")),
    ("guitarra", C::MelodicLoop, Some("guitar")),
    ("strings", C::Pad, Some("strings")),
    ("cuerdas", C::Pad, Some("strings")),
    // Vocals.
    ("vocal", C::Vocal, None),
    ("vocals", C::Vocal, None),
    ("vox", C::Vocal, None),
    ("voz", C::Vocal, None),
    ("voces", C::Vocal, None),
    ("voix", C::Vocal, None),
    ("stimme", C::Vocal, None),
    ("voce", C::Vocal, None),
    ("acapella", C::Vocal, Some("acapella")),
    ("acappella", C::Vocal, Some("acapella")),
    ("chant", C::Vocal, Some("chant")),
    ("adlib", C::Vocal, Some("adlib")),
    ("shout", C::Vocal, Some("shout")),
    ("grito", C::Vocal, Some("shout")),
    ("phrase", C::Vocal, Some("phrase")),
    ("frase", C::Vocal, Some("phrase")),
    ("choir", C::Vocal, Some("choir")),
    ("coro", C::Vocal, Some("choir")),
    // FX.
    ("riser", C::Riser, None),
    ("uplifter", C::Riser, None),
    ("uplift", C::Riser, None),
    ("buildup", C::Riser, None),
    ("build up", C::Riser, None),
    ("upsweep", C::Riser, None),
    ("subida", C::Riser, None),
    ("rise", C::Riser, None),
    ("impact", C::Impact, None),
    ("impacto", C::Impact, None),
    ("boom", C::Impact, None),
    ("hit", C::Impact, None),
    ("slam", C::Impact, None),
    ("downlifter", C::Impact, Some("downlifter")),
    ("golpe", C::Impact, None),
    ("explosion", C::Impact, None),
    ("sweep", C::Fx, Some("sweep")),
    ("whoosh", C::Fx, Some("whoosh")),
    ("noise", C::Fx, Some("noise")),
    ("ruido", C::Fx, Some("noise")),
    ("glitch", C::Fx, Some("glitch")),
    ("foley", C::Fx, Some("foley")),
    ("transition", C::Fx, Some("transition")),
    ("transicion", C::Fx, Some("transition")),
    ("fx", C::Fx, None),
    ("sfx", C::Fx, None),
    ("efecto", C::Fx, None),
    ("efectos", C::Fx, None),
    ("effect", C::Fx, None),
];

/// Words that mark a file as a loop.
const LOOP_WORDS: &[&str] = &[
    "loop",
    "loops",
    "bucle",
    "boucle",
    "groove",
    "full drum",
    "fill",
];

fn tokens(text: &str) -> (Vec<String>, String) {
    let lower = text.to_lowercase();
    let mut spaced = String::with_capacity(lower.len());
    for ch in lower.chars() {
        spaced.push(if ch.is_alphanumeric() { ch } else { ' ' });
    }
    // Split camel-ish digit boundaries too: "kick01" -> "kick 01".
    let mut out = String::with_capacity(spaced.len());
    let mut prev: Option<char> = None;
    for ch in spaced.chars() {
        if let Some(p) = prev
            && ((p.is_alphabetic() && ch.is_ascii_digit())
                || (p.is_ascii_digit() && ch.is_alphabetic()))
        {
            out.push(' ');
        }
        out.push(ch);
        prev = Some(ch);
    }
    let toks = out.split_whitespace().map(str::to_string).collect();
    let joined = format!(" {} ", out.split_whitespace().collect::<Vec<_>>().join(" "));
    (toks, joined)
}

/// Look up a category in a name; returns (category, subcategory, matched-loop-word).
pub fn category_from_name(name: &str) -> (Option<(SampleCategory, Option<String>)>, bool) {
    let (toks, joined) = tokens(name);
    let is_loop = LOOP_WORDS.iter().any(|w| {
        joined.contains(&format!(" {w} ")) || toks.iter().any(|t| t.len() > 4 && t.contains(w))
    });
    for (kw, cat, sub) in KEYWORDS {
        let hit = if kw.contains(' ') {
            joined.contains(&format!(" {kw} "))
        } else if kw.chars().count() <= 3 || kw.chars().all(|c| c.is_ascii_digit()) {
            toks.iter().any(|t| t == kw)
        } else {
            toks.iter().any(|t| t.contains(kw))
        };
        if hit {
            return (Some((*cat, sub.map(str::to_string))), is_loop);
        }
    }
    (None, is_loop)
}

/// Inputs the feature classifier needs besides [`Features`].
pub struct ClassifyInput<'a> {
    pub features: &'a Features,
    pub duration: f32,
    /// Median f0 (Hz) and how periodic the sound is (0 = noise, 1 = pure tone).
    pub pitch: Option<(f32, f32)>,
    pub is_loop: bool,
    /// Short transient bursts inside the first 60 ms (claps have several).
    pub early_bursts: u32,
}

/// Category guess from audio features alone.
pub fn category_from_features(i: &ClassifyInput) -> (SampleCategory, Option<String>, f32) {
    let f = i.features;
    let tonal = i.pitch.map_or(0.0, |p| p.1);
    let f0 = i.pitch.map_or(0.0, |p| p.0);
    if i.is_loop {
        let gated = f.sustain < 0.75;
        let cat = if f.transients >= 4 && gated && f.band_low < 0.1 {
            C::PercLoop
        } else if f.transients >= 4 && gated {
            C::DrumLoop
        } else if f.band_low > 0.6 && (tonal > 0.4 || f.flatness < 0.05) {
            C::BassLoop
        } else if tonal > 0.4 || f.flatness < 0.05 {
            C::MelodicLoop
        } else if f.transients >= 4 {
            C::DrumLoop
        } else {
            C::FxLoop
        };
        return (cat, None, 0.45);
    }
    // Risers and impacts are long, evolving, noisy.
    if i.duration > 1.5 && f.centroid_slope > 0.35 && f.level_slope_db > 3.0 {
        return (C::Riser, None, 0.6);
    }
    if i.duration > 1.0
        && f.attack_s < 0.03
        && f.band_low > 0.35
        && f.flatness > 0.15
        && f.decay_s > 0.8
    {
        return (C::Impact, None, 0.45);
    }
    let percussive = f.attack_s < 0.03 && f.decay_s < 1.2;
    // Low, tonal, long: 808 / bass.
    if f.band_low > 0.55
        && tonal > 0.6
        && f0 > 0.0
        && f0 < 160.0
        && i.duration > 0.6
        && f.decay_s > 0.6
    {
        return (
            if f.attack_s < 0.02 {
                C::Bass808
            } else {
                C::Bass
            },
            None,
            0.55,
        );
    }
    if percussive && f.band_low > 0.45 && f.centroid_hz < 1500.0 {
        return (C::Kick, None, 0.65);
    }
    // Hats and cymbals: almost all energy above 2 kHz and noisy (flat, or a very high zero-crossing rate).
    if f.band_high > 0.6
        && f.band_low < 0.05
        && (f.flatness > 0.02 || f.zero_crossing_rate > 4000.0)
    {
        if i.duration > 1.2 && f.decay_s > 0.9 {
            return (C::Cymbal, None, 0.55);
        }
        let sub = if f.decay_s > 0.25 { "open" } else { "closed" };
        return (C::HiHat, Some(sub.into()), 0.65);
    }
    if percussive && f.flatness > 0.08 && f.band_mid + f.band_high > 0.6 {
        if i.early_bursts >= 3 {
            return (C::Clap, None, 0.5);
        }
        if f.centroid_hz > 1200.0 {
            return (C::Snare, None, 0.5);
        }
    }
    if percussive && tonal > 0.5 && f0 > 60.0 && f0 < 400.0 && f.decay_s < 0.8 {
        return (C::Tom, None, 0.4);
    }
    if percussive && i.duration < 0.6 && tonal < 0.5 {
        return (C::Perc, None, 0.4);
    }
    if tonal > 0.4 || f.flatness < 0.05 {
        if f0 > 0.0 && f0 < 130.0 && f.band_low > 0.4 {
            return (C::Bass, None, 0.5);
        }
        if f.attack_s > 0.12 && i.duration > 1.5 {
            return (C::Pad, None, 0.5);
        }
        if f.attack_s < 0.03 && f.decay_s < 0.9 {
            return (C::Pluck, None, 0.45);
        }
        if i.duration > 0.5 {
            return (C::Lead, None, 0.35);
        }
        return (C::Synth, None, 0.3);
    }
    if f.flatness > 0.3 {
        return (C::Fx, None, 0.3);
    }
    (C::Unknown, None, 0.0)
}

/// Combine the name lookup and the feature guess.
pub fn classify(name: &str, input: &ClassifyInput) -> Classification {
    let (named, _) = category_from_name(name);
    let (fcat, fsub, fconf) = category_from_features(input);
    match named {
        Some((cat, sub)) => {
            let cat = if input.is_loop && !cat.is_loop() {
                cat.as_loop()
            } else {
                cat
            };
            let agrees = cat == fcat
                || (cat.is_drum() && fcat.is_drum() && cat == C::HiHat && fcat == C::Cymbal);
            Classification {
                category: cat,
                subcategory: sub.or(if agrees { fsub } else { None }),
                confidence: if agrees { 0.95 } else { 0.8 },
                source: if agrees {
                    CategorySource::Both
                } else {
                    CategorySource::Filename
                },
            }
        }
        None => Classification {
            category: fcat,
            subcategory: fsub,
            confidence: fconf,
            source: CategorySource::Features,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_several_languages() {
        let cat = |n: &str| category_from_name(n).0.map(|c| c.0);
        assert_eq!(cat("Bombo_Reggaeton_01.wav"), Some(C::Kick));
        assert_eq!(cat("caja seca.wav"), Some(C::Snare));
        assert_eq!(cat("Palmas 3.wav"), Some(C::Clap));
        assert_eq!(cat("OH_Bright.wav"), Some(C::HiHat));
        assert_eq!(cat("DeepKick02.wav"), Some(C::Kick));
        assert_eq!(cat("808_Glide_F#.wav"), Some(C::Bass808));
        assert_eq!(cat("Riser_8bars.wav"), Some(C::Riser));
        assert_eq!(cat("Voz Chant Hey.wav"), Some(C::Vocal));
        assert_eq!(cat("Drum Loop 128.wav"), Some(C::DrumLoop));
        assert_eq!(cat("Pad Ambient C.wav"), Some(C::Pad));
        assert_eq!(cat("Untitled 4.wav"), None);
        assert!(category_from_name("Top Loop 128.wav").1);
        assert!(!category_from_name("Kick 01.wav").1);
    }

    #[test]
    fn subcategories() {
        let (c, _) = category_from_name("Open Hat 04.wav");
        assert_eq!(c, Some((C::HiHat, Some("open".into()))));
    }
}
