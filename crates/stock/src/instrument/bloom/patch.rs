//! Tonal sounds: a [`Patch`] is a full parameter set of the Wavetable Synth
//! (`instrument.wavetable`), and a [`PatchScene`] is sixteen of them grown
//! from one seed as one [`Kind`] (bass, lead, pad…), the tonal partner of
//! the drum [`Scene`](super::genome::Scene).
//!
//! A patch is dense and fixed-size: one normalised value per synth
//! parameter, in the order of `wavetable::params()` ([`PATCH_LEN`] slots, no
//! heap), so it can be grown, copied and compared on the audio thread. It
//! is stored sparsely by parameter id (only the values that differ from the
//! synth's defaults), so it survives new synth parameters.
//!
//! Conversions between plain and normalised values use [`det_log2`] /
//! [`det_exp2`] instead of the platform's `ln`/`powf`, so a seed grows the
//! same patches on every machine.

use super::genome::{SOUNDS, det_exp2, det_log2, quantize, reflect};
use crate::instrument::wavetable::{self as wt, PARAM_SPACE};
use aitrack_dsp::{ParamInfo, ParamScale};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

/// Slots of a patch (at least the Wavetable Synth's parameter count).
pub const PATCH_LEN: usize = 384;

/// What a Bloom scene is made of: the drum scene (the original engine) or
/// one of the tonal kinds, played by the embedded Wavetable Synth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Drums,
    Bass,
    Lead,
    Pad,
    Pluck,
    Keys,
    Fx,
    Texture,
}

/// Names of the Kind parameter: the kinds, padded with "—" so kinds can be
/// appended later without moving the stored normalised values.
pub const KIND_NAMES: &[&str] = &["Drums", "Bass", "Lead", "Pad", "Pluck", "Keys", "FX", "Texture", "—", "—", "—", "—"];

impl Kind {
    pub const ALL: [Kind; 8] = [Kind::Drums, Kind::Bass, Kind::Lead, Kind::Pad, Kind::Pluck, Kind::Keys, Kind::Fx, Kind::Texture];
    /// The kinds the Wavetable Synth plays.
    pub const TONAL: [Kind; 7] = [Kind::Bass, Kind::Lead, Kind::Pad, Kind::Pluck, Kind::Keys, Kind::Fx, Kind::Texture];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    /// The kind of a parameter index (the padding entries are Drums).
    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        KIND_NAMES[self.index()]
    }

    pub fn is_tonal(self) -> bool {
        self != Kind::Drums
    }

    /// Salt of the kind in seeds, so one seed grows unrelated scenes per kind.
    pub(crate) fn salt(self) -> u64 {
        0x7A3E_0000 + self.index() as u64 * 0x1F1F
    }
}

/// Where every synth parameter sits in a patch.
pub struct Layout {
    /// Parameter id of each slot (`len` used).
    pub ids: [u32; PATCH_LEN],
    pub len: usize,
    /// Slot of each id (`u16::MAX`: not a synth parameter).
    pub slot: [u16; PARAM_SPACE],
    pub infos: &'static [ParamInfo],
    /// The synth's defaults, normalised.
    pub defaults: [f32; PATCH_LEN],
}

/// The layout, built once (call it off the audio thread first; Bloom does
/// at creation).
pub fn layout() -> &'static Layout {
    static L: OnceLock<Box<Layout>> = OnceLock::new();
    L.get_or_init(|| {
        let infos = wt::params_static();
        assert!(infos.len() <= PATCH_LEN, "PATCH_LEN too small for {} synth parameters", infos.len());
        let mut l = Box::new(Layout { ids: [0; PATCH_LEN], len: infos.len(), slot: [u16::MAX; PARAM_SPACE], infos, defaults: [0.0; PATCH_LEN] });
        for (k, info) in infos.iter().enumerate() {
            l.ids[k] = info.id;
            l.slot[info.id as usize] = k as u16;
            l.defaults[k] = quantize(det_norm(info, info.default));
        }
        l
    })
}

/// A plain value (Hz, ms, %, choice index) of `info` at normalised `n`,
/// from arithmetic only.
pub fn det_plain(info: &ParamInfo, n: f32) -> f32 {
    let n = n.clamp(0.0, 1.0);
    match info.scale {
        ParamScale::Linear => info.min + (info.max - info.min) * n,
        ParamScale::Log => info.min * det_exp2(n * (det_log2(info.max) - det_log2(info.min))),
        ParamScale::Power(e) => info.min + (info.max - info.min) * if n <= 0.0 { 0.0 } else { det_exp2(e * det_log2(n)) },
        ParamScale::Stepped => info.min + (n * (info.max - info.min).round()).round(),
    }
}

/// The normalised value of plain `v`, from arithmetic only.
pub fn det_norm(info: &ParamInfo, v: f32) -> f32 {
    let v = v.clamp(info.min.min(info.max), info.max.max(info.min));
    let n = match info.scale {
        ParamScale::Linear | ParamScale::Stepped => (v - info.min) / (info.max - info.min),
        ParamScale::Log => (det_log2(v) - det_log2(info.min)) / (det_log2(info.max) - det_log2(info.min)),
        ParamScale::Power(e) => {
            let x = (v - info.min) / (info.max - info.min);
            if x <= 0.0 { 0.0 } else { det_exp2(det_log2(x) / e) }
        }
    };
    if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 }
}

/// One tonal sound: every Wavetable Synth parameter, normalised, by slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Patch {
    pub kind: Kind,
    pub v: [f32; PATCH_LEN],
}

impl Default for Patch {
    fn default() -> Self {
        Self::neutral(Kind::Bass)
    }
}

impl Patch {
    /// The synth's defaults.
    pub fn neutral(kind: Kind) -> Self {
        Self { kind, v: layout().defaults }
    }

    /// Slot of parameter `id`.
    #[inline]
    pub fn slot(id: u32) -> Option<usize> {
        let s = *layout().slot.get(id as usize)?;
        (s != u16::MAX).then_some(s as usize)
    }

    /// Normalised value of parameter `id` (0 when unknown).
    #[inline]
    pub fn get(&self, id: u32) -> f32 {
        Self::slot(id).map_or(0.0, |s| reflect(self.v[s]))
    }

    /// Set the normalised value of `id`.
    #[inline]
    pub fn set(&mut self, id: u32, n: f32) {
        if let Some(s) = Self::slot(id) {
            self.v[s] = quantize(n.clamp(0.0, 1.0));
        }
    }

    /// Plain value of `id`.
    #[inline]
    pub fn plain(&self, id: u32) -> f32 {
        match Self::slot(id) {
            Some(s) => det_plain(&layout().infos[s], reflect(self.v[s])),
            None => 0.0,
        }
    }

    /// Set `id` from a plain value (clamped to its range).
    #[inline]
    pub fn set_plain(&mut self, id: u32, v: f32) {
        if let Some(s) = Self::slot(id) {
            self.v[s] = quantize(det_norm(&layout().infos[s], v));
        }
    }

    /// Index of a stepped parameter.
    #[inline]
    pub fn choice(&self, id: u32) -> usize {
        match Self::slot(id) {
            Some(s) => {
                let info = &layout().infos[s];
                (det_plain(info, reflect(self.v[s])) - info.min).round().max(0.0) as usize
            }
            None => 0,
        }
    }

    /// Clamp plain `id` into `lo..=hi`.
    pub fn clamp_plain(&mut self, id: u32, lo: f32, hi: f32) {
        let v = self.plain(id);
        if v < lo || v > hi {
            self.set_plain(id, v.clamp(lo, hi));
        }
    }

    /// Every value folded into 0..1 (what the synth receives).
    pub fn folded(&self) -> Self {
        let mut out = *self;
        for v in &mut out.v {
            *v = reflect(*v);
        }
        out
    }

    /// A stable fingerprint (FNV-1a over the stored bits).
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ self.kind.index() as u64;
        for v in self.folded().v {
            let b = quantize(v).to_bits();
            for k in 0..4 {
                h ^= u64::from((b >> (8 * k)) & 0xFF);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    /// `(id, normalised)` of every synth parameter (for `set_param`).
    pub fn pairs(&self) -> impl Iterator<Item = (u32, f32)> + '_ {
        let l = layout();
        (0..l.len).map(move |k| (l.ids[k], reflect(self.v[k])))
    }

    /// From `(id, normalised)` pairs over the synth's defaults (a preset's
    /// parameters); unknown ids are skipped.
    pub fn from_pairs(kind: Kind, pairs: impl IntoIterator<Item = (u32, f32)>) -> Self {
        let mut p = Self::neutral(kind);
        for (id, v) in pairs {
            if v.is_finite() {
                p.set(id, v);
            }
        }
        p
    }

    /// The Wavetable Synth preset of this patch (`instrument.wavetable`;
    /// every parameter stored). `gain` is Bloom's level normalisation, folded
    /// into the synth's Volume so the preset plays at the level Bloom does.
    pub fn to_preset(&self, name: &str, gain: f32) -> crate::presets::Preset {
        let mut p = self.folded();
        if gain.is_finite() && gain > 0.0 {
            let db = p.plain(wt::p::VOLUME) + 20.0 * det_log2(gain) * std::f32::consts::LOG10_2;
            p.set_plain(wt::p::VOLUME, db);
        }
        crate::presets::Preset {
            name: name.to_string(),
            plugin: wt::ID.to_string(),
            author: "Bloom".into(),
            tags: vec!["bloom".into(), self.kind.label().to_ascii_lowercase()],
            params: p.pairs().map(|(id, v)| (id.to_string(), v)).collect(),
        }
    }

    /// Distance in normalised space (for tests and searches).
    pub fn distance(&self, other: &Patch) -> f32 {
        let (a, b) = (self.folded(), other.folded());
        let l = layout();
        (0..l.len).map(|k| (a.v[k] - b.v[k]) * (a.v[k] - b.v[k])).sum::<f32>().sqrt()
    }
}

#[derive(Serialize, Deserialize)]
struct PatchRepr {
    #[serde(default)]
    kind: Kind,
    /// Values that differ from the synth's defaults, by parameter id.
    p: serde_json::Map<String, Value>,
}

impl Serialize for Patch {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let l = layout();
        let mut p = serde_json::Map::new();
        for k in 0..l.len {
            if self.v[k] != l.defaults[k] {
                p.insert(l.ids[k].to_string(), Value::from(f64::from(self.v[k])));
            }
        }
        PatchRepr { kind: self.kind, p }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Patch {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = PatchRepr::deserialize(d)?;
        let mut out = Patch::neutral(r.kind);
        for (k, v) in &r.p {
            if let (Ok(id), Some(x)) = (k.parse::<u32>(), v.as_f64())
                && let Some(s) = Patch::slot(id)
                && x.is_finite()
            {
                // Cores may sit outside 0..1 after an edit re-based them.
                out.v[s] = (x as f32).clamp(-4.0, 5.0);
            }
        }
        Ok(out)
    }
}

/// Sixteen tonal sounds grown from one seed: what the state stores
/// (`extra.patches`), like [`Scene`](super::genome::Scene) for drums.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchScene {
    pub seed: u64,
    pub kind: Kind,
    /// The sixteen patches as they play.
    pub patches: [Patch; SOUNDS],
    /// Where each growth path starts (distance 0).
    pub cores: [Patch; SOUNDS],
    pub paths: [u64; SOUNDS],
    pub distances: [f32; SOUNDS],
    #[serde(default)]
    pub locks: u16,
}

impl PatchScene {
    /// Sixteen synth defaults (a placeholder before anything grows).
    pub fn empty(kind: Kind) -> Self {
        let p = Patch::neutral(kind);
        Self { seed: u64::MAX, kind, patches: [p; SOUNDS], cores: [p; SOUNDS], paths: [0; SOUNDS], distances: [0.0; SOUNDS], locks: 0 }
    }

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

    pub fn from_json(v: &Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_covers_every_synth_parameter() {
        let l = layout();
        assert_eq!(l.len, wt::params().len());
        for (k, info) in wt::params().iter().enumerate() {
            assert_eq!(Patch::slot(info.id), Some(k));
            let back = det_plain(info, l.defaults[k]);
            assert!((back - info.default).abs() <= 1e-3 * (info.max - info.min).abs().max(1.0), "{}: {back} vs {}", info.name, info.default);
        }
        assert_eq!(Patch::slot(999), None);
        assert_eq!(KIND_NAMES.len(), 12);
        assert_eq!(Kind::from_index(11), Kind::Drums, "padding plays drums");
    }

    #[test]
    fn det_conversions_match_the_platform() {
        for info in wt::params_static() {
            for n in [0.0f32, 0.13, 0.5, 0.77, 1.0] {
                let (a, b) = (det_plain(info, n), info.to_plain(n));
                assert!((a - b).abs() <= 2e-3 * (info.max - info.min).abs().max(1.0) + 1e-4 * b.abs(), "{} at {n}: {a} vs {b}", info.name);
                let back = det_norm(info, a);
                if info.scale != ParamScale::Stepped {
                    assert!((back - n).abs() < 1e-3, "{} at {n}: {back}", info.name);
                }
            }
        }
    }

    #[test]
    fn patches_round_trip_sparsely() {
        let mut p = Patch::neutral(Kind::Pad);
        p.set_plain(wt::p::CUTOFF, 1234.0);
        p.set_plain(wt::p::AMP_ATTACK, 800.0);
        p.v[3] = 1.4; // a re-based core value
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.len() < 200, "only the changes are stored: {s}");
        let back: Patch = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
        let preset = p.to_preset("Test", 2.0);
        assert_eq!(preset.plugin, wt::ID);
        assert_eq!(preset.params.len(), wt::params().len());
        let vol = wt::params_static().iter().find(|i| i.id == wt::p::VOLUME).unwrap();
        assert!((vol.to_plain(preset.params[&wt::p::VOLUME.to_string()]) - (-6.0 + 6.02)).abs() < 0.05);
    }
}
