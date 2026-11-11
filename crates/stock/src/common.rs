//! Shared plumbing for the stock plugins: parameter storage, sample-accurate
//! event splitting, stereo frame iteration and JSON state.

use aitrack_dsp::{AudioBuffer, EventKind, ParamInfo, ProcessContext};
use serde_json::{Map, Value};

/// Normalised parameter values indexed by id, plus a dirty flag the plugin
/// polls to recompute coefficients off the per-sample path.
#[derive(Clone, Debug)]
pub struct ParamStore {
    infos: Vec<Option<ParamInfo>>,
    norm: Vec<f32>,
    dirty: bool,
}

impl ParamStore {
    pub fn new(params: &[ParamInfo]) -> Self {
        let len = params.iter().map(|p| p.id as usize + 1).max().unwrap_or(0);
        let mut infos = vec![None; len];
        let mut norm = vec![0.0; len];
        for p in params {
            norm[p.id as usize] = p.default_normalized();
            infos[p.id as usize] = Some(p.clone());
        }
        Self { infos, norm, dirty: true }
    }

    #[inline]
    pub fn set(&mut self, id: u32, normalized: f32) {
        if let Some(v) = self.norm.get_mut(id as usize) {
            let n = if normalized.is_finite() { normalized.clamp(0.0, 1.0) } else { return };
            if *v != n {
                *v = n;
                self.dirty = true;
            }
        }
    }

    #[inline]
    pub fn get(&self, id: u32) -> f32 {
        self.norm.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Plain value (Hz, dB, ms…) of a parameter.
    #[inline]
    pub fn plain(&self, id: u32) -> f32 {
        match self.infos.get(id as usize) {
            Some(Some(info)) => info.to_plain(self.norm[id as usize]),
            _ => 0.0,
        }
    }

    /// Index of a stepped parameter.
    #[inline]
    pub fn choice(&self, id: u32) -> usize {
        match self.infos.get(id as usize) {
            Some(Some(info)) => (info.to_plain(self.norm[id as usize]) - info.min).round().max(0.0) as usize,
            _ => 0,
        }
    }

    #[inline]
    pub fn on(&self, id: u32) -> bool {
        self.plain(id) >= 0.5
    }

    /// Set from a plain value.
    pub fn set_plain(&mut self, id: u32, plain: f32) {
        if let Some(Some(info)) = self.infos.get(id as usize) {
            let n = info.to_normalized(plain);
            self.set(id, n);
        }
    }

    /// True once after any change.
    #[inline]
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn infos(&self) -> impl Iterator<Item = &ParamInfo> {
        self.infos.iter().flatten()
    }

    /// `{"<id>": normalized, …}`.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        for p in self.infos() {
            m.insert(p.id.to_string(), Value::from(self.norm[p.id as usize] as f64));
        }
        Value::Object(m)
    }

    pub fn load_json(&mut self, v: &Value) {
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let (Ok(id), Some(n)) = (k.parse::<u32>(), val.as_f64()) {
                    self.set(id, n as f32);
                }
            }
        }
    }
}

/// Serialise parameters plus plugin-specific extras as versioned JSON.
pub fn save_state(params: &ParamStore, extra: Value) -> Vec<u8> {
    let mut root = Map::new();
    root.insert("version".into(), Value::from(1));
    root.insert("params".into(), params.to_json());
    if !extra.is_null() {
        root.insert("extra".into(), extra);
    }
    serde_json::to_vec(&Value::Object(root)).unwrap_or_default()
}

/// Restore parameters; returns the `extra` object (or `Null`).
pub fn load_state(params: &mut ParamStore, data: &[u8]) -> Value {
    match serde_json::from_slice::<Value>(data) {
        Ok(Value::Object(mut root)) => {
            if let Some(p) = root.get("params") {
                params.load_json(p);
            }
            root.remove("extra").unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

/// A processor body that renders sub-ranges of the block between events.
pub trait Segmented {
    /// Handle one event at its sample position (parameter changes, notes…).
    fn event(&mut self, kind: &EventKind);
    /// Render frames `start..end` of `ctx.buffer` in place.
    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize);
}

/// Split the block at event offsets so parameter and note events land
/// sample-accurately.
pub fn drive<P: Segmented>(p: &mut P, ctx: &mut ProcessContext<'_>) {
    let frames = ctx.buffer.frames();
    let events = ctx.events;
    let mut start = 0;
    for ev in events {
        let off = (ev.offset as usize).min(frames);
        if off > start {
            p.render(ctx, start, off);
            start = off;
        }
        p.event(&ev.kind);
    }
    if start < frames {
        p.render(ctx, start, frames);
    }
}

/// Visit frames `start..end` as stereo pairs and write the results back. A
/// mono buffer is presented as `(x, x)` and receives the left result.
#[inline]
pub fn for_frames(buf: &mut AudioBuffer, start: usize, end: usize, mut f: impl FnMut(usize, f32, f32) -> (f32, f32)) {
    if buf.channels() >= 2 {
        let (l, r) = buf.stereo_mut();
        for i in start..end {
            let (a, b) = f(i, l[i], r[i]);
            l[i] = a;
            r[i] = b;
        }
    } else if buf.channels() == 1 {
        let l = buf.channel_mut(0);
        for i in start..end {
            let (a, _) = f(i, l[i], l[i]);
            l[i] = a;
        }
    }
}

/// Read a sidechain frame as a stereo pair (mono fans out, missing → 0).
#[inline]
pub fn sidechain_frame(sc: Option<&AudioBuffer>, i: usize) -> (f32, f32) {
    match sc {
        Some(b) if b.channels() >= 2 && i < b.frames() => (b.channel(0)[i], b.channel(1)[i]),
        Some(b) if b.channels() == 1 && i < b.frames() => {
            let v = b.channel(0)[i];
            (v, v)
        }
        _ => (0.0, 0.0),
    }
}

/// Replace NaN/inf with silence (last-resort guard on outputs).
#[inline]
pub fn sanitize(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// Write `values` into `out` from `offset`; returns the new offset.
#[inline]
pub fn write_meter(out: &mut [f32], offset: usize, values: &[f32]) -> usize {
    if offset >= out.len() {
        return offset;
    }
    let n = values.len().min(out.len() - offset);
    out[offset..offset + n].copy_from_slice(&values[..n]);
    offset + n
}

/// Equal-power pan law (−3 dB centre): returns `(left, right)` gains for
/// `pan ∈ [−1, 1]`.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (a.cos(), a.sin())
}

/// MIDI key → Hz (A4 = 440).
#[inline]
pub fn key_to_hz(key: f32) -> f32 {
    440.0 * 2f32.powf((key - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_round_trip() {
        let params = [ParamInfo::new(0, "Gain", -24.0, 24.0, 0.0).unit("dB"), ParamInfo::new(3, "Freq", 20.0, 20_000.0, 1000.0).log()];
        let mut s = ParamStore::new(&params);
        assert!((s.plain(3) - 1000.0).abs() < 0.1);
        s.set_plain(0, 6.0);
        let bytes = save_state(&s, Value::Null);
        let mut t = ParamStore::new(&params);
        load_state(&mut t, &bytes);
        assert!((t.plain(0) - 6.0).abs() < 1e-4);
    }
}
