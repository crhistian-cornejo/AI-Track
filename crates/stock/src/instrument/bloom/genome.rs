//! The genes of one Bloom sound and the scene of sixteen of them.
//!
//! A [`Genome`] is every parameter of one sound, stored *normalised* (each
//! gene 0..1 on its own scale, log for times and frequencies), so mutation,
//! morphing and the matching search treat all genes alike and a step of 0.1
//! is a similar-sized change everywhere. [`GENES`] describes each gene with
//! a [`ParamInfo`] (range, unit, scale, choice names), which also gives the
//! DNA panel its formatting. Discrete genes come in two kinds: *ordered*
//! (voice count, burst count: they morph by rounding) and *categorical*
//! (waveforms, filter type: they switch, see [`categorical`]).
//!
//! A [`Scene`] is what the device stores in its state blob: the sixteen
//! sounds, plus how each was grown (its core, its growth path and how far
//! along it the sound sits), so a cell can be dragged closer to its core or
//! further away later (see `grow.rs`).

use aitrack_dsp::{ParamInfo, ParamScale};
use serde::{Deserialize, Serialize};

/// Sounds in a scene.
pub const SOUNDS: usize = 16;

/// Gene indices.
pub mod g {
    // Pitch
    pub const PITCH: usize = 0;
    pub const SWEEP: usize = 1;
    pub const SWEEP_T: usize = 2;
    // Oscillators
    pub const OSC1: usize = 3;
    pub const OSC2: usize = 4;
    pub const OSC2_RATIO: usize = 5;
    pub const OSC2_DETUNE: usize = 6;
    pub const OSC_MIX: usize = 7;
    pub const TONE_LEVEL: usize = 8;
    // FM (2-op: a sine modulator on oscillator 1's phase)
    pub const FM_RATIO: usize = 9;
    pub const FM_INDEX: usize = 10;
    pub const FM_DECAY: usize = 11;
    // Noise
    pub const NOISE_LEVEL: usize = 12;
    pub const NOISE_MODE: usize = 13;
    pub const NOISE_FREQ: usize = 14;
    pub const NOISE_Q: usize = 15;
    pub const NOISE_DECAY: usize = 16;
    pub const BURSTS: usize = 17;
    pub const BURST_GAP: usize = 18;
    // Shape
    pub const DRIVE: usize = 19;
    pub const FILTER: usize = 20;
    pub const CUTOFF: usize = 21;
    pub const RESO: usize = 22;
    pub const FILT_ENV: usize = 23;
    pub const FILT_DECAY: usize = 24;
    // Amp
    pub const ATTACK: usize = 25;
    pub const HOLD: usize = 26;
    pub const DECAY: usize = 27;
    pub const RELEASE: usize = 28;
    // Resonator
    pub const RES_MIX: usize = 29;
    pub const RES_DRIVE: usize = 30;
    pub const RES_MODES: usize = 31;
    pub const RES_SHAPE: usize = 32;
    pub const RES_TUNE: usize = 33;
    pub const RES_DECAY: usize = 34;
    pub const RES_DAMP: usize = 35;
    pub const RES_BRIGHT: usize = 36;
    pub const RES_POS: usize = 37;
    // Space and output
    pub const WIDTH: usize = 38;
    pub const PAN: usize = 39;
    pub const SEND: usize = 40;
    pub const LEVEL: usize = 41;
    pub const VEL_TONE: usize = 42;
    pub const CHOKE: usize = 43;
}

/// Genes in a genome.
pub const N_GENES: usize = 44;

pub const WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];
pub const NOISE_MODES: &[&str] = &["Low", "Band", "High"];
pub const FILTERS: &[&str] = &["Low", "Band", "High", "Ladder"];
pub const HITS: &[&str] = &["1", "2", "3", "4"];
pub const CHOKES: &[&str] = &["None", "A", "B"];
/// The modal shapes the resonator morphs between (gene `RES_SHAPE`, 0..4).
pub const SHAPES: &[&str] = &["String", "Membrane", "Plate", "Bar", "Inharmonic"];

const fn stepped(p: ParamInfo) -> ParamInfo {
    ParamInfo { scale: ParamScale::Stepped, ..p }
}

/// Every gene: range, unit, scale and choices. The id is the gene index.
pub const GENES: [ParamInfo; N_GENES] = [
    ParamInfo::new(g::PITCH as u32, "Pitch", 12.0, 108.0, 48.0).unit("st"),
    ParamInfo::new(g::SWEEP as u32, "Sweep", 0.0, 48.0, 0.0).unit("st"),
    ParamInfo::new(g::SWEEP_T as u32, "Sweep Time", 1.0, 400.0, 30.0).unit("ms").log(),
    ParamInfo::new(g::OSC1 as u32, "Osc 1", 0.0, 3.0, 0.0).choices(WAVES),
    ParamInfo::new(g::OSC2 as u32, "Osc 2", 0.0, 3.0, 0.0).choices(WAVES),
    ParamInfo::new(g::OSC2_RATIO as u32, "Osc 2 Ratio", 0.25, 8.0, 2.0).unit("x").log(),
    ParamInfo::new(g::OSC2_DETUNE as u32, "Osc 2 Detune", -50.0, 50.0, 0.0).unit("ct"),
    ParamInfo::new(g::OSC_MIX as u32, "Osc Mix", 0.0, 1.0, 0.0),
    ParamInfo::new(g::TONE_LEVEL as u32, "Tone", 0.0, 1.0, 1.0),
    ParamInfo::new(g::FM_RATIO as u32, "FM Ratio", 0.5, 16.0, 2.0).unit("x").log(),
    ParamInfo::new(g::FM_INDEX as u32, "FM Index", 0.0, 12.0, 0.0),
    ParamInfo::new(g::FM_DECAY as u32, "FM Decay", 2.0, 1500.0, 60.0).unit("ms").log(),
    ParamInfo::new(g::NOISE_LEVEL as u32, "Noise", 0.0, 1.0, 0.0),
    ParamInfo::new(g::NOISE_MODE as u32, "Noise Filter", 0.0, 2.0, 1.0).choices(NOISE_MODES),
    ParamInfo::new(g::NOISE_FREQ as u32, "Noise Freq", 100.0, 16_000.0, 4_000.0).unit("Hz").log(),
    ParamInfo::new(g::NOISE_Q as u32, "Noise Q", 0.5, 12.0, 1.0).log(),
    ParamInfo::new(g::NOISE_DECAY as u32, "Noise Decay", 2.0, 2000.0, 60.0).unit("ms").log(),
    ParamInfo::new(g::BURSTS as u32, "Hits", 0.0, 3.0, 0.0).choices(HITS),
    ParamInfo::new(g::BURST_GAP as u32, "Hit Gap", 3.0, 30.0, 9.0).unit("ms").log(),
    ParamInfo::new(g::DRIVE as u32, "Drive", 0.0, 1.0, 0.1),
    ParamInfo::new(g::FILTER as u32, "Filter", 0.0, 3.0, 0.0).choices(FILTERS),
    ParamInfo::new(g::CUTOFF as u32, "Cutoff", 40.0, 18_000.0, 16_000.0).unit("Hz").log(),
    ParamInfo::new(g::RESO as u32, "Resonance", 0.0, 1.0, 0.1),
    ParamInfo::new(g::FILT_ENV as u32, "Filter Env", -3.0, 7.0, 0.0).unit("oct"),
    ParamInfo::new(g::FILT_DECAY as u32, "Filter Decay", 3.0, 2000.0, 120.0).unit("ms").log(),
    ParamInfo::new(g::ATTACK as u32, "Attack", 0.1, 60.0, 0.4).unit("ms").log(),
    ParamInfo::new(g::HOLD as u32, "Hold", 0.0, 250.0, 0.0).unit("ms"),
    ParamInfo::new(g::DECAY as u32, "Decay", 8.0, 5000.0, 300.0).unit("ms").log(),
    ParamInfo::new(g::RELEASE as u32, "Release", 5.0, 3000.0, 300.0).unit("ms").log(),
    ParamInfo::new(g::RES_MIX as u32, "Resonator", 0.0, 1.0, 0.0),
    ParamInfo::new(g::RES_DRIVE as u32, "Excite", 0.0, 1.0, 0.5),
    stepped(ParamInfo::new(g::RES_MODES as u32, "Modes", 8.0, 24.0, 12.0)),
    ParamInfo::new(g::RES_SHAPE as u32, "Material", 0.0, 4.0, 1.0),
    ParamInfo::new(g::RES_TUNE as u32, "Res Tune", -24.0, 24.0, 0.0).unit("st"),
    ParamInfo::new(g::RES_DECAY as u32, "Res Decay", 20.0, 8000.0, 400.0).unit("ms").log(),
    ParamInfo::new(g::RES_DAMP as u32, "Damping", 0.0, 1.0, 0.5),
    ParamInfo::new(g::RES_BRIGHT as u32, "Brightness", 0.0, 1.0, 0.5),
    ParamInfo::new(g::RES_POS as u32, "Position", 0.0, 1.0, 0.3),
    ParamInfo::new(g::WIDTH as u32, "Width", 0.0, 1.0, 0.2),
    ParamInfo::new(g::PAN as u32, "Pan", -1.0, 1.0, 0.0),
    ParamInfo::new(g::SEND as u32, "Space", 0.0, 1.0, 0.2),
    ParamInfo::new(g::LEVEL as u32, "Level", -18.0, 6.0, 0.0).unit("dB"),
    ParamInfo::new(g::VEL_TONE as u32, "Vel > Tone", 0.0, 1.0, 0.5),
    ParamInfo::new(g::CHOKE as u32, "Choke", 0.0, 2.0, 0.0).choices(CHOKES),
];

/// The DNA panel's groups: title and gene indices.
pub const GROUPS: &[(&str, &[usize])] = &[
    ("Pitch", &[g::PITCH, g::SWEEP, g::SWEEP_T]),
    ("Oscillators", &[g::OSC1, g::OSC2, g::OSC2_RATIO, g::OSC2_DETUNE, g::OSC_MIX, g::TONE_LEVEL]),
    ("FM", &[g::FM_RATIO, g::FM_INDEX, g::FM_DECAY]),
    ("Noise", &[g::NOISE_LEVEL, g::NOISE_MODE, g::NOISE_FREQ, g::NOISE_Q, g::NOISE_DECAY, g::BURSTS, g::BURST_GAP]),
    ("Filter", &[g::DRIVE, g::FILTER, g::CUTOFF, g::RESO, g::FILT_ENV, g::FILT_DECAY]),
    ("Amp", &[g::ATTACK, g::HOLD, g::DECAY, g::RELEASE]),
    ("Resonator", &[g::RES_MIX, g::RES_DRIVE, g::RES_MODES, g::RES_SHAPE, g::RES_TUNE, g::RES_DECAY, g::RES_DAMP, g::RES_BRIGHT, g::RES_POS]),
    ("Output", &[g::WIDTH, g::PAN, g::SEND, g::LEVEL, g::VEL_TONE, g::CHOKE]),
];

/// Genes whose values are names, not amounts: they switch instead of
/// sliding when a sound grows or morphs.
pub fn categorical(gene: usize) -> bool {
    matches!(gene, g::OSC1 | g::OSC2 | g::NOISE_MODE | g::FILTER | g::CHOKE)
}

/// Number of choices of a categorical gene (0 for the others).
pub fn choice_count(gene: usize) -> usize {
    if categorical(gene) { GENES[gene].choices.len() } else { 0 }
}

/// What a sound is for in a kit. Balances a generated scene and names cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    #[default]
    Kick,
    Snare,
    Clap,
    HatClosed,
    HatOpen,
    Tom,
    Rim,
    Metal,
    Perc,
    Noise,
    Bass,
    Pluck,
    Blip,
    Zap,
    Bell,
}

/// Where a role sits in a kit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Band {
    Low,
    Mid,
    High,
    Tonal,
}

impl Role {
    pub const ALL: [Role; 15] = [
        Role::Kick,
        Role::Snare,
        Role::Clap,
        Role::HatClosed,
        Role::HatOpen,
        Role::Tom,
        Role::Rim,
        Role::Metal,
        Role::Perc,
        Role::Noise,
        Role::Bass,
        Role::Pluck,
        Role::Blip,
        Role::Zap,
        Role::Bell,
    ];

    pub fn band(self) -> Band {
        match self {
            Role::Kick | Role::Bass => Band::Low,
            Role::Snare | Role::Clap | Role::Tom | Role::Rim | Role::Perc => Band::Mid,
            Role::HatClosed | Role::HatOpen | Role::Metal | Role::Noise => Band::High,
            Role::Pluck | Role::Blip | Role::Zap | Role::Bell => Band::Tonal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Kick => "Kick",
            Role::Snare => "Snare",
            Role::Clap => "Clap",
            Role::HatClosed => "Closed Hat",
            Role::HatOpen => "Open Hat",
            Role::Tom => "Tom",
            Role::Rim => "Rim",
            Role::Metal => "Metal",
            Role::Perc => "Perc",
            Role::Noise => "Noise",
            Role::Bass => "Bass",
            Role::Pluck => "Pluck",
            Role::Blip => "Blip",
            Role::Zap => "Zap",
            Role::Bell => "Bell",
        }
    }

    /// Two letters for a small cell label.
    pub fn short(self) -> &'static str {
        match self {
            Role::Kick => "KD",
            Role::Snare => "SD",
            Role::Clap => "CP",
            Role::HatClosed => "CH",
            Role::HatOpen => "OH",
            Role::Tom => "TM",
            Role::Rim => "RM",
            Role::Metal => "MT",
            Role::Perc => "PC",
            Role::Noise => "NZ",
            Role::Bass => "BS",
            Role::Pluck => "PL",
            Role::Blip => "BP",
            Role::Zap => "ZP",
            Role::Bell => "BL",
        }
    }

    /// Whether its pitch follows the scene's key.
    pub fn tuned(self) -> bool {
        matches!(self, Role::Bass | Role::Pluck | Role::Blip | Role::Bell | Role::Tom | Role::Perc)
    }
}

/// The family a scene is grown from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Species {
    #[default]
    Kit,
    Tonal,
    Metallic,
    Glitch,
    Mixed,
}

pub const SPECIES_NAMES: &[&str] = &["Drum Kit", "Tonal Perc", "Metallic", "Glitch", "Mixed"];

impl Species {
    pub const ALL: [Species; 5] = [Species::Kit, Species::Tonal, Species::Metallic, Species::Glitch, Species::Mixed];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        SPECIES_NAMES[self.index()]
    }

    /// The role of each of the sixteen cells, laid out like a drum rack
    /// (C1 kick, D1 snare, F#1 closed hat, A#1 open hat…).
    pub fn layout(self) -> [Role; SOUNDS] {
        use Role::*;
        match self {
            Species::Kit => [Kick, Rim, Snare, Clap, Perc, Tom, HatClosed, Tom, HatClosed, Tom, HatOpen, Perc, Kick, Metal, Blip, Noise],
            Species::Tonal => [Kick, Perc, Pluck, Blip, Tom, Pluck, HatClosed, Bell, Perc, Pluck, HatOpen, Bell, Bass, Blip, Pluck, Bell],
            Species::Metallic => [Kick, Rim, Metal, Clap, Metal, Tom, HatClosed, Bell, HatClosed, Metal, HatOpen, Bell, Bass, Metal, Rim, Noise],
            Species::Glitch => [Kick, Blip, Zap, Clap, Blip, Noise, HatClosed, Zap, Blip, Rim, HatOpen, Zap, Bass, Blip, Noise, Zap],
            Species::Mixed => [Kick, Rim, Snare, Clap, Pluck, Tom, HatClosed, Bell, Zap, Perc, HatOpen, Blip, Bass, Metal, Noise, Pluck],
        }
    }
}

/// One sound: every gene, normalised 0..1, and its role.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "GenomeRepr", into = "GenomeRepr")]
pub struct Genome {
    pub role: Role,
    pub v: [f32; N_GENES],
}

#[derive(Serialize, Deserialize)]
struct GenomeRepr {
    #[serde(default)]
    role: Role,
    g: Vec<f32>,
}

impl From<GenomeRepr> for Genome {
    fn from(r: GenomeRepr) -> Self {
        let mut out = Genome::neutral(r.role);
        for (slot, v) in out.v.iter_mut().zip(r.g) {
            if v.is_finite() {
                *slot = v.clamp(-4.0, 5.0);
            }
        }
        out
    }
}

impl From<Genome> for GenomeRepr {
    fn from(g: Genome) -> Self {
        GenomeRepr { role: g.role, g: g.v.to_vec() }
    }
}

impl Default for Genome {
    fn default() -> Self {
        Self::neutral(Role::Kick)
    }
}

/// Round to a fixed grid, so the same arithmetic on two machines lands on
/// the same stored value.
#[inline]
pub fn quantize(v: f32) -> f32 {
    (v * 1_048_576.0).round() / 1_048_576.0
}

/// `log2` from arithmetic only (bit split plus an odd series), so
/// generation does not depend on the platform's libm and a seed gives the
/// same genome everywhere. Error under 1e-6.
pub fn det_log2(x: f32) -> f32 {
    let x = x.max(1e-30);
    let bits = x.to_bits();
    let mut exp = ((bits >> 23) & 0xFF) as i32 - 127;
    let mut m = f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000) as f64; // 1..2
    if m > std::f64::consts::SQRT_2 {
        m *= 0.5;
        exp += 1;
    }
    // ln(m) = 2·atanh(s), s = (m − 1)/(m + 1), |s| < 0.172.
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let ln = 2.0 * s * (1.0 + s2 * (1.0 / 3.0 + s2 * (1.0 / 5.0 + s2 * (1.0 / 7.0 + s2 * (1.0 / 9.0 + s2 / 11.0)))));
    (exp as f64 + ln * std::f64::consts::LOG2_E) as f32
}

/// `2^x` from arithmetic only, the partner of [`det_log2`]. Error under 1e-6
/// for |x| < 60.
pub fn det_exp2(x: f32) -> f32 {
    let x = f64::from(x.clamp(-60.0, 60.0));
    let i = x.floor();
    let f = (x - i) * std::f64::consts::LN_2; // 0..ln 2
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..=10 {
        term *= f / f64::from(k);
        sum += term;
    }
    (sum * f64::from_bits(((i as i64 + 1023) as u64) << 52)) as f32
}

impl Genome {
    /// Every gene at its default.
    pub fn neutral(role: Role) -> Self {
        let mut v = [0.0; N_GENES];
        for (slot, info) in v.iter_mut().zip(GENES.iter()) {
            *slot = info.default_normalized();
        }
        Self { role, v }
    }

    /// A gene's value in its unit (Hz, ms, st…); a choice gene's index.
    #[inline]
    pub fn plain(&self, gene: usize) -> f32 {
        GENES[gene].to_plain(reflect(self.v[gene]))
    }

    /// A choice or stepped gene as an index from 0.
    #[inline]
    pub fn index(&self, gene: usize) -> usize {
        (self.plain(gene) - GENES[gene].min).round().max(0.0) as usize
    }

    /// Set a gene from a plain value, without the platform's `ln` (see [`det_log2`]).
    pub fn set(&mut self, gene: usize, plain: f32) {
        let info = &GENES[gene];
        let v = plain.clamp(info.min.min(info.max), info.max.max(info.min));
        let n = match info.scale {
            ParamScale::Log => (det_log2(v) - det_log2(info.min)) / (det_log2(info.max) - det_log2(info.min)),
            ParamScale::Power(_) | ParamScale::Linear | ParamScale::Stepped => (v - info.min) / (info.max - info.min),
        };
        self.v[gene] = quantize(n.clamp(0.0, 1.0));
    }

    /// The genes as the DSP reads them: every value folded into 0..1.
    pub fn folded(&self) -> Self {
        let mut out = *self;
        for v in &mut out.v {
            *v = reflect(*v);
        }
        out
    }

    /// A stable 64-bit fingerprint (FNV-1a over the stored bits): the key of
    /// the colour and shape cache.
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: u32| {
            for k in 0..4 {
                h ^= u64::from((b >> (8 * k)) & 0xFF);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        eat(Role::ALL.iter().position(|r| *r == self.role).unwrap_or(0) as u32);
        for v in self.folded().v {
            eat(quantize(v).to_bits());
        }
        h
    }

    /// Euclidean distance between two genomes in normalised gene space
    /// (categorical genes count 1 when they differ).
    pub fn gene_distance(&self, other: &Genome) -> f32 {
        let (a, b) = (self.folded(), other.folded());
        let mut s = 0.0;
        for i in 0..N_GENES {
            let d = if categorical(i) { if a.index(i) == b.index(i) { 0.0 } else { 1.0 } } else { a.v[i] - b.v[i] };
            s += d * d;
        }
        s.sqrt()
    }
}

/// Fold any value into 0..1 by mirroring at the ends (a growth path that
/// runs past a gene's end bounces back instead of sticking to it).
#[inline]
pub fn reflect(v: f32) -> f32 {
    if (0.0..=1.0).contains(&v) {
        return v;
    }
    if !v.is_finite() {
        return 0.5;
    }
    let m = v.rem_euclid(2.0);
    if m <= 1.0 { m } else { 2.0 - m }
}

/// A full scene: what the device's state holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub seed: u64,
    pub species: Species,
    /// The sixteen sounds as they play.
    pub sounds: [Genome; SOUNDS],
    /// Where each sound's growth path starts (distance 0). May hold values
    /// outside 0..1 after an edit re-based the path (see `grow::rebase`).
    pub cores: [Genome; SOUNDS],
    /// The seed of each sound's growth path.
    pub paths: [u64; SOUNDS],
    /// How far along its path each sound is (0 = its core, 1 = far).
    pub distances: [f32; SOUNDS],
    /// Locked cells (bit `i`): rolls and mutations leave them as they are.
    #[serde(default)]
    pub locks: u16,
}

impl Scene {
    pub fn locked(&self, i: usize) -> bool {
        self.locks & (1 << i) != 0
    }

    pub fn set_locked(&mut self, i: usize, on: bool) {
        if on {
            self.locks |= 1 << i;
        } else {
            self.locks &= !(1 << i);
        }
    }

    /// The scene a fixed-size slice of the state can be parsed into.
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gene_table_is_consistent() {
        for (i, info) in GENES.iter().enumerate() {
            assert_eq!(info.id as usize, i, "{} has the wrong index", info.name);
            if !info.choices.is_empty() {
                assert_eq!(info.choices.len() as f32, info.max - info.min + 1.0, "{}", info.name);
            }
        }
        let mut seen = [false; N_GENES];
        for (_, genes) in GROUPS {
            for &k in *genes {
                assert!(!seen[k], "gene {k} in two groups");
                seen[k] = true;
            }
        }
        assert!(seen.iter().all(|s| *s), "every gene has a group");
    }

    #[test]
    fn det_exp2_is_close_to_exp2() {
        for x in [-7.5f32, -1.0, -0.3, 0.0, 0.5, 1.0, 2.7, 10.0] {
            assert!((det_exp2(x) / x.exp2() - 1.0).abs() < 1e-6, "{x}");
        }
    }

    #[test]
    fn det_log2_is_close_to_log2() {
        for x in [0.1f32, 0.5, 1.0, 1.3, 1.7, 1.99, 2.0, 3.0, 40.0, 440.0, 16_000.0] {
            assert!((det_log2(x) - x.log2()).abs() < 2e-4, "{x}: {} vs {}", det_log2(x), x.log2());
        }
    }

    #[test]
    fn set_and_plain_round_trip() {
        let mut g = Genome::neutral(Role::Snare);
        g.set(g::CUTOFF, 1_000.0);
        assert!((g.plain(g::CUTOFF) / 1_000.0 - 1.0).abs() < 2e-3);
        g.set(g::FILTER, 3.0);
        assert_eq!(g.index(g::FILTER), 3);
        g.set(g::RES_MODES, 17.0);
        assert_eq!(g.plain(g::RES_MODES), 17.0);
    }

    #[test]
    fn reflect_folds_into_the_unit_range() {
        assert_eq!(reflect(0.25), 0.25);
        assert!((reflect(1.25) - 0.75).abs() < 1e-6);
        assert!((reflect(-0.25) - 0.25).abs() < 1e-6);
        assert!((reflect(2.5) - 0.5).abs() < 1e-6);
        assert_eq!(reflect(f32::NAN), 0.5);
    }

    #[test]
    fn genomes_round_trip_through_json_and_tolerate_short_arrays() {
        let mut g = Genome::neutral(Role::Bell);
        g.v[g::FM_INDEX] = 0.7;
        let s = serde_json::to_string(&g).unwrap();
        let back: Genome = serde_json::from_str(&s).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.hash(), g.hash());
        let short: Genome = serde_json::from_str(r#"{"role":"zap","g":[0.5]}"#).unwrap();
        assert_eq!(short.role, Role::Zap);
        assert_eq!(short.v[0], 0.5);
        assert_eq!(short.v[g::DECAY], Genome::neutral(Role::Zap).v[g::DECAY]);
    }
}
