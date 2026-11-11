//! Parameter tables shared by the audio and main threads.
//!
//! Hosted plugins use arbitrary 32-bit parameter ids (VST3 ids are often
//! hashes), so values are stored by *index* in a dense atomic array and
//! looked up through a sorted id table (binary search, no allocation).
//!
//! All values here are normalised 0..=1, which is what
//! `aitrack_dsp::Processor` speaks. VST3 is natively normalised; CLAP values
//! are plain and converted with the parameter's `ParamInfo` (linear or
//! stepped between `min` and `max`).

use std::sync::atomic::{AtomicU32, Ordering};

use aitrack_dsp::ParamInfo;

/// An edit made in the plugin's own GUI (or reported by the plugin), sent to
/// the DAW so it can record automation and undo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamEdit {
    pub id: u32,
    /// Normalised 0..=1 (meaningful for `Value`; last value for Begin/End).
    pub value: f32,
    pub kind: ParamEditKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamEditKind {
    /// The user grabbed a control (VST3 `beginEdit`, CLAP gesture begin).
    Begin,
    Value,
    /// The user released the control: one undo step.
    End,
}

/// Capacity of the GUI-edit queue towards the DAW.
pub const PARAM_EDIT_QUEUE: usize = 4096;

pub struct ParamTable {
    infos: Vec<ParamInfo>,
    /// `(id, index)` sorted by id.
    by_id: Vec<(u32, u32)>,
    values: Box<[AtomicU32]>,
}

impl ParamTable {
    pub fn new(infos: Vec<ParamInfo>) -> Self {
        let mut by_id: Vec<(u32, u32)> = infos.iter().enumerate().map(|(i, p)| (p.id, i as u32)).collect();
        by_id.sort_unstable();
        by_id.dedup_by_key(|e| e.0);
        let values: Box<[AtomicU32]> = infos.iter().map(|p| AtomicU32::new(p.default_normalized().to_bits())).collect();
        Self { infos, by_id, values }
    }

    pub fn infos(&self) -> &[ParamInfo] {
        &self.infos
    }

    pub fn len(&self) -> usize {
        self.infos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }

    #[inline]
    pub fn index_of(&self, id: u32) -> Option<usize> {
        self.by_id.binary_search_by_key(&id, |e| e.0).ok().map(|i| self.by_id[i].1 as usize)
    }

    #[inline]
    pub fn info(&self, id: u32) -> Option<&ParamInfo> {
        self.index_of(id).map(|i| &self.infos[i])
    }

    #[inline]
    pub fn get(&self, id: u32) -> f32 {
        self.index_of(id).map_or(0.0, |i| f32::from_bits(self.values[i].load(Ordering::Relaxed)))
    }

    #[inline]
    pub fn set(&self, id: u32, normalized: f32) {
        if let Some(i) = self.index_of(id) {
            self.values[i].store(normalized.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn get_index(&self, index: usize) -> f32 {
        self.values.get(index).map_or(0.0, |a| f32::from_bits(a.load(Ordering::Relaxed)))
    }

    /// Normalised → the plugin's native value (CLAP plain value).
    #[inline]
    pub fn to_plain(&self, id: u32, normalized: f32) -> f64 {
        match self.info(id) {
            Some(p) => {
                let n = normalized.clamp(0.0, 1.0) as f64;
                let (min, max) = (p.min as f64, p.max as f64);
                let v = min + (max - min) * n;
                if p.scale == aitrack_dsp::ParamScale::Stepped { v.round() } else { v }
            }
            None => normalized as f64,
        }
    }

    /// Native (CLAP plain) → normalised.
    #[inline]
    pub fn to_normalized(&self, id: u32, plain: f64) -> f32 {
        match self.info(id) {
            Some(p) => {
                let range = (p.max - p.min) as f64;
                if range.abs() < f64::EPSILON { 0.0 } else { (((plain - p.min as f64) / range) as f32).clamp(0.0, 1.0) }
            }
            None => plain as f32,
        }
    }
}

/// Parameter changes requested through `Processor::set_param` between two
/// blocks, delivered at offset 0 of the next block. Fixed capacity (one slot
/// per parameter), so pushing never allocates.
pub(crate) struct PendingParams {
    /// Index → pending value, NaN = none.
    slots: Box<[f32]>,
    dirty: Vec<u32>,
}

impl PendingParams {
    pub fn new(len: usize) -> Self {
        Self { slots: vec![f32::NAN; len].into_boxed_slice(), dirty: Vec::with_capacity(len) }
    }

    #[inline]
    pub fn push(&mut self, index: usize, value: f32) {
        if let Some(slot) = self.slots.get_mut(index) {
            if slot.is_nan() {
                self.dirty.push(index as u32);
            }
            *slot = value;
        }
    }

    /// Visit and clear all pending `(index, value)`.
    #[inline]
    pub fn drain(&mut self, mut f: impl FnMut(usize, f32)) {
        for &i in &self.dirty {
            let v = std::mem::replace(&mut self.slots[i as usize], f32::NAN);
            f(i as usize, v);
        }
        self.dirty.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_ids() {
        let infos = vec![ParamInfo::new(0xDEADBEEF, "A", 0.0, 1.0, 0.5), ParamInfo::new(7, "B", -10.0, 10.0, 0.0)];
        let t = ParamTable::new(infos);
        assert_eq!(t.get(0xDEADBEEF), 0.5);
        t.set(7, 0.75);
        assert_eq!(t.get(7), 0.75);
        assert!((t.to_plain(7, 0.75) - 5.0).abs() < 1e-6);
        assert!((t.to_normalized(7, 5.0) - 0.75).abs() < 1e-6);
        assert_eq!(t.get(1234), 0.0);
    }

    #[test]
    fn pending_dedupes() {
        let mut p = PendingParams::new(4);
        p.push(1, 0.2);
        p.push(1, 0.3);
        p.push(3, 1.0);
        let mut seen = Vec::new();
        p.drain(|i, v| seen.push((i, v)));
        assert_eq!(seen, vec![(1, 0.3), (3, 1.0)]);
        let mut again = 0;
        p.drain(|_, _| again += 1);
        assert_eq!(again, 0);
    }
}
