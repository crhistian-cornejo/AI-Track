//! `instrument.wavetable`: the compact card of the wavetable synth and the
//! data both of its editors share.
//!
//! The card shows the three oscillators' tables at their position (drag
//! across a display = position), the first filter's response with a
//! draggable cutoff / resonance node and the amp envelope; the expanded card
//! adds envelope 2, the LFOs, the routings of the modulation matrix and the
//! voice / FX knobs. Everything else (drawn LFOs, drag-to-route, the
//! wavetable editor) lives in the synth's own window
//! (`views::device_window::synth`).
//!
//! Shared data: [`Previews`] holds display copies of the tables, built on
//! the background executor (the factory tables are additive / FFT work), and
//! the user tables and drawn LFO curves read from the device state
//! ([`SynthData`]), which edits write back with [`write_state`] through a
//! `SetDeviceState` command.

use super::filter::{DB_BOTTOM, DB_TOP, db_frac};
use super::kit::{self, Handle, PlotCfg, Rect, ax, frac_freq, freq_frac};
use super::{Dev, caption};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::response::svf_cascade_db;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode};
use aitrack_model::DeviceId;
use aitrack_stock::instrument::wavetable::modulation::{self, LfoCurve, ModSlot};
use aitrack_stock::instrument::wavetable::tablegen::{FACTORY_NAMES, WaveFrames};
use aitrack_stock::instrument::wavetable::{TABLES, USER_WAVE, dest_name, factory_frames, legacy_frames, p, state_with};
use gpui::{AnyElement, Context, Div, Hsla, IntoElement, ParentElement, Styled, TextAlign, div, prelude::FluentBuilder, px};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

// ───────────────────────────── Shared helpers ─────────────────────────────

/// The sections the synth's SVF model runs (see the stock `filter` module).
pub fn filter_sections(mode: usize, slope24: bool, cutoff: f32, resonance: f32, sample_rate: f64) -> Vec<SvfCoeffs> {
    let mode = match mode {
        1 => SvfMode::Highpass,
        2 => SvfMode::Bandpass,
        _ => SvfMode::Lowpass,
    };
    let r = resonance.clamp(0., 1.);
    if slope24 {
        vec![
            SvfCoeffs::new(mode, cutoff, 0.541 * (1. + r * 0.2), 0., sample_rate),
            SvfCoeffs::new(mode, cutoff, 1.307 * (20f32 / 1.307).powf(r), 0., sample_rate),
        ]
    } else {
        let q = std::f32::consts::FRAC_1_SQRT_2 * (20f32 / std::f32::consts::FRAC_1_SQRT_2).powf(r);
        vec![SvfCoeffs::new(mode, cutoff, q, 0., sample_rate)]
    }
}

/// One of the synth's filters as a response plot sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterView {
    pub model: usize,
    pub kind: usize,
    pub slope24: bool,
    pub cutoff: f32,
    /// 0..1.
    pub resonance: f32,
}

impl FilterView {
    /// Filter `n` of a synth.
    pub fn of(dev: &Dev, n: u32) -> Self {
        let id = |f| p::filt(n, f);
        Self {
            model: dev.choice(id(p::F_MODEL)),
            kind: dev.choice(id(p::F_TYPE)),
            slope24: dev.choice(id(p::F_SLOPE)) == 1,
            cutoff: dev.plain(id(p::F_CUTOFF)),
            resonance: dev.plain(id(p::F_RESO)) / 100.,
        }
    }

    /// Magnitude in dB at `f` Hz: exact for the SVF models, a close sketch
    /// of the shape for the ladder, comb and formant models.
    pub fn db(&self, f: f32, sample_rate: f64) -> f32 {
        use aitrack_stock::instrument::wavetable::filter::model;
        let (fc, r) = (self.cutoff.max(10.), self.resonance.clamp(0., 1.));
        match self.model {
            model::LADDER => {
                // Four poles; resonance trades low end for a peak at the cutoff.
                let x = f / fc;
                let pole = -10. * (1. + x.powi(8)).log10();
                let peak = 18. * r * r * (-((x.ln()) / 0.18).powi(2)).exp();
                pole + peak - 6. * r
            }
            model::NOTCH => {
                let x = (f / fc).ln();
                let depth = if self.slope24 { 48. } else { 30. };
                -depth * (-(x / (0.25 - 0.18 * r)).powi(2)).exp()
            }
            model::COMB_POS | model::COMB_NEG => {
                // |1 / (1 − g e^{−jωT})| with T = 1 / cutoff.
                let g = (0.3 + 0.65 * r) * if self.model == model::COMB_NEG { -1. } else { 1. };
                let w = std::f32::consts::TAU * f / fc;
                let (re, im) = (1. - g * w.cos(), g * w.sin());
                -10. * (re * re + im * im).log10() - 6.
            }
            model::FORMANT => {
                // The cutoff sweeps the vowels A → U; three bumps.
                let t = ((fc / 20.).ln() / (1000f32).ln()).clamp(0., 1.) * 4.;
                let (i, frac) = ((t.floor() as usize).min(3), t - t.floor().min(3.));
                const V: [[(f32, f32); 3]; 5] = [
                    [(800., 1.), (1150., 0.63), (2900., 0.35)],
                    [(400., 1.), (1600., 0.4), (2700., 0.25)],
                    [(270., 1.), (2300., 0.3), (3000., 0.2)],
                    [(450., 1.), (800., 0.7), (2830., 0.12)],
                    [(325., 1.), (700., 0.4), (2530., 0.1)],
                ];
                let width = 0.18 - 0.1 * r;
                let lin: f32 = (0..3)
                    .map(|k| {
                        let (fa, ga) = V[i][k];
                        let (fb, gb) = V[(i + 1).min(4)][k];
                        let (fk, gk) = (fa + (fb - fa) * frac, ga + (gb - ga) * frac);
                        gk * (-((f / fk).ln() / width).powi(2)).exp()
                    })
                    .sum();
                20. * lin.max(1e-3).log10() + 6.
            }
            _ => svf_cascade_db(&filter_sections(self.kind, self.slope24, fc, r, sample_rate), f as f64, sample_rate) as f32,
        }
    }

    /// The node a plot draws for this filter, at plot fractions.
    pub fn node(&self, sample_rate: f64) -> (f32, f32) {
        (freq_frac(self.cutoff), db_frac(self.db(self.cutoff, sample_rate)).clamp(0.06, 0.94))
    }
}

/// Paint a synth filter's response into `r`.
pub fn paint_filter(window: &mut gpui::Window, cx: &mut gpui::App, r: Rect, f: FilterView, sample_rate: f64, color: Hsla, labels: bool) {
    kit::db_grid(window, cx, r, DB_BOTTOM, DB_TOP, 12., labels);
    kit::freq_grid(window, cx, r, labels);
    let n = ((r.w / 3.) as usize).clamp(32, 320);
    let pts = kit::sample_curve(r, n, |fx| db_frac(f.db(frac_freq(fx), sample_rate)));
    kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.16));
    kit::line(window, &pts, 2., color);
}

/// The corner points of an ADSR drawn in a unit square: start, attack peak,
/// decay end, release start, end. Times are the parameters' normalised
/// values, so the log scale of the knobs is the scale of the picture.
pub fn adsr_points(attack: f32, decay: f32, sustain: f32, release: f32) -> [(f32, f32); 5] {
    let (top, bottom) = (0.1, 0.9);
    let a = 0.02 + 0.24 * attack;
    let d = a + 0.02 + 0.24 * decay;
    let s = d + 0.18;
    let level = bottom - (bottom - top) * sustain.clamp(0., 1.);
    [(0., bottom), (a, top), (d, level), (s, level), ((s + 0.02 + 0.28 * release).min(1.), bottom)]
}

/// An envelope editor: drag the peak (attack), the knee (decay / sustain)
/// and the end (release). `ids` are attack, decay, sustain, release.
pub fn adsr(ws: &mut Workspace, dev: &Dev, slot: u8, ids: [u32; 4], color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let [a, d, s, r] = ids;
    let pts = adsr_points(dev.norm(a), dev.norm(d), dev.plain(s) / 100., dev.norm(r));
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let base = slot as usize * 8;
    let handles = vec![
        Handle { fx: pts[1].0, fy: pts[1].1, x: ax(a, 1. / 0.24), index: base, reset: vec![default(a)], ..Default::default() },
        Handle { fx: pts[2].0, fy: pts[2].1, x: ax(d, 1. / 0.24), y: ax(s, -1. / 0.8), index: base + 1, reset: vec![default(d), default(s)], ..Default::default() },
        Handle { fx: pts[4].0, fy: pts[4].1 - 0.06, x: ax(r, 1. / 0.28), index: base + 2, reset: vec![default(r)], ..Default::default() },
    ];
    let marks = handles.clone();
    let text = format!("A {}  D {}  S {}  R {}", dev.text(a), dev.text(d), dev.text(s), dev.text(r));
    kit::plot(ws, dev, PlotCfg { slot, handles, ..Default::default() }, cx, move |rect, window, cx| {
        let pal = p();
        kit::hline(window, rect, rect.py(0.9), pal.grid_bar);
        let line: Vec<_> = pts.iter().map(|(x, y)| rect.pt(*x, *y)).collect();
        kit::fill_to(window, &line, rect.py(0.9), with_alpha(color, 0.18));
        kit::line(window, &line, 2., color);
        if rect.w > 200. {
            kit::txt(window, cx, &text, rect.x + 6., rect.y + 3., TEXT_XS, pal.faint, TextAlign::Left);
        }
        for h in &marks {
            kit::dot(window, rect.pt(h.fx, h.fy), 5.5, color, Some(pal.text));
        }
    })
    .into_any_element()
}

// ───────────────────────────── Table data ─────────────────────────────

/// Points per frame of a display copy.
pub const PREVIEW_LEN: usize = 256;
/// Most frames a display copy keeps (spread evenly over the table).
pub const PREVIEW_FRAMES: usize = 64;

/// Which table an oscillator plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableKey {
    /// One of the original six (`TABLES`).
    Legacy(usize),
    /// Factory bank entry (`FACTORY_NAMES` index).
    Factory(usize),
    /// The oscillator's own table in the device state.
    User(DeviceId, usize),
}

impl TableKey {
    /// The table oscillator `n` of `dev` plays.
    pub fn of(dev: &Dev, n: u32) -> Self {
        let wave = dev.choice(p::osc(n, p::WAVE));
        if wave == USER_WAVE {
            TableKey::User(dev.id, n as usize)
        } else if (1..=FACTORY_NAMES.len()).contains(&wave) {
            TableKey::Factory(wave - 1)
        } else {
            TableKey::Legacy(dev.choice(p::osc(n, p::TABLE)).min(TABLES.len() - 1))
        }
    }
}

/// A display copy of a wavetable: a few frames, a few points each.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub name: String,
    pub frames: Vec<Vec<f32>>,
    /// Frames of the real table.
    pub total: usize,
}

impl Preview {
    pub fn of(w: &WaveFrames) -> Self {
        let total = w.frames.len().max(1);
        let keep = total.min(PREVIEW_FRAMES);
        let frames = (0..keep)
            .map(|i| {
                let src = if keep > 1 { i * (total - 1) / (keep - 1) } else { 0 };
                let f = &w.frames[src.min(w.frames.len().saturating_sub(1))];
                (0..PREVIEW_LEN).map(|k| f.get(k * f.len() / PREVIEW_LEN).copied().unwrap_or(0.)).collect()
            })
            .collect();
        Self { name: w.name.clone(), frames, total }
    }

    /// The value at table position `pos` (0..1) and phase `x` (0..1),
    /// between the two nearest kept frames.
    pub fn sample(&self, pos: f32, x: f32) -> f32 {
        if self.frames.is_empty() {
            return 0.;
        }
        let at = pos.clamp(0., 1.) * (self.frames.len() - 1) as f32;
        let (i, t) = (at.floor() as usize, at.fract());
        let j = (i + 1).min(self.frames.len() - 1);
        let k = ((x.rem_euclid(1.)) * PREVIEW_LEN as f32) as usize % PREVIEW_LEN;
        self.frames[i][k] * (1. - t) + self.frames[j][k] * t
    }

    /// Peak of the kept frames (for scaling a display).
    pub fn peak(&self) -> f32 {
        self.frames.iter().flatten().fold(0f32, |m, v| m.max(v.abs())).max(1e-3)
    }
}

/// The parts of a synth's state that are not parameters: the oscillators'
/// user tables and the drawn LFO curves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SynthData {
    pub tables: [Option<Rc<WaveFrames>>; 3],
    pub curves: [Option<LfoCurve>; 4],
}

impl SynthData {
    /// LFO `l`'s drawn curve (the synth's default when the state has none).
    pub fn curve(&self, l: usize) -> LfoCurve {
        self.curves.get(l).copied().flatten().unwrap_or_default()
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (padding optional); `None` on a foreign character.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut val = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        val[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = val[c as usize];
        if v == 255 {
            return None;
        }
        acc = (acc << 6 | v as u32) & 0xFF_FFFF;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn table_from_json(v: &serde_json::Value) -> Option<WaveFrames> {
    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("User");
    let len = v.get("frame_len").and_then(|n| n.as_u64()).unwrap_or(2048) as usize;
    let bytes = b64_decode(v.get("pcm16")?.as_str()?)?;
    let samples: Vec<f32> = bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32_767.).collect();
    if len < 16 || samples.len() < len {
        return None;
    }
    Some(WaveFrames::new(name, samples.chunks_exact(len).map(<[f32]>::to_vec).collect()))
}

/// The user tables and LFO curves stored in a synth's device state.
pub fn read_state(state: Option<&[u8]>) -> SynthData {
    let mut data = SynthData::default();
    let Some(root) = state.and_then(|s| serde_json::from_slice::<serde_json::Value>(s).ok()) else { return data };
    let Some(extra) = root.get("extra") else { return data };
    if let Some(arr) = extra.get("user_tables").and_then(|a| a.as_array()) {
        for (o, slot) in data.tables.iter_mut().enumerate() {
            *slot = arr.get(o).and_then(table_from_json).map(Rc::new);
        }
    }
    if let Some(arr) = extra.get("lfo_curves").and_then(|a| a.as_array()) {
        for (l, slot) in data.curves.iter_mut().enumerate() {
            *slot = arr.get(l).filter(|v| !v.is_null()).and_then(LfoCurve::from_json);
        }
    }
    data
}

/// A device state holding `data` (the parameters go in too, though the
/// engine applies the model's parameter values after loading the state).
pub fn write_state(params: &BTreeMap<u32, f32>, data: &SynthData) -> Vec<u8> {
    let pairs: Vec<(u32, f32)> = params.iter().map(|(k, v)| (*k, *v)).collect();
    let tables: Vec<Option<&WaveFrames>> = data.tables.iter().map(|t| t.as_deref()).collect();
    state_with(&pairs, &tables, &data.curves)
}

/// A cheap fingerprint of a state blob (length, a sparse sample, the tail).
fn fingerprint(bytes: Option<&[u8]>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match bytes {
        None => 0u8.hash(&mut h),
        Some(b) => {
            b.len().hash(&mut h);
            b.iter().step_by(61).for_each(|v| v.hash(&mut h));
            b[b.len().saturating_sub(256)..].hash(&mut h);
        }
    }
    h.finish()
}

struct UserEntry {
    print: u64,
    data: Rc<SynthData>,
    previews: [Option<Rc<Preview>>; 3],
}

/// Display copies of tables, built off the UI thread, and the synth data
/// of each device (parsed when its state changes).
#[derive(Default)]
pub struct Previews {
    ready: HashMap<TableKey, Rc<Preview>>,
    full: HashMap<TableKey, Rc<WaveFrames>>,
    want_full: HashSet<TableKey>,
    pending: HashSet<TableKey>,
    users: HashMap<DeviceId, UserEntry>,
}

impl Previews {
    /// The user tables and curves of `device` (cached until its state changes).
    pub fn data(&mut self, project: &aitrack_model::Project, device: DeviceId) -> Rc<SynthData> {
        self.user_entry(project, device).data.clone()
    }

    fn user_entry(&mut self, project: &aitrack_model::Project, device: DeviceId) -> &UserEntry {
        let state = project.device(device).and_then(|d| d.state.as_deref());
        let print = fingerprint(state);
        let stale = self.users.get(&device).is_none_or(|e| e.print != print);
        if stale {
            let data = read_state(state);
            let previews = std::array::from_fn(|o| data.tables[o].as_deref().map(|w| Rc::new(Preview::of(w))));
            self.users.insert(device, UserEntry { print, data: Rc::new(data), previews });
        }
        &self.users[&device]
    }

    pub fn prune(&mut self, alive: impl Fn(DeviceId) -> bool) {
        self.users.retain(|id, _| alive(*id));
    }
}

/// Raw frames of a built-in table (heavy: additive synthesis / FFTs).
fn build_builtin(key: TableKey) -> WaveFrames {
    match key {
        TableKey::Factory(i) => factory_frames(i + 1).unwrap_or_else(|| legacy_frames(0)),
        TableKey::Legacy(t) => legacy_frames(t),
        TableKey::User(..) => legacy_frames(0),
    }
}

fn request(ws: &mut Workspace, key: TableKey, cx: &mut Context<Workspace>) {
    if !ws.fx.wave_previews.pending.insert(key) {
        return;
    }
    let job = cx.background_executor().spawn(async move {
        let w = build_builtin(key);
        let preview = Preview::of(&w);
        (w, preview)
    });
    cx.spawn(async move |this, cx| {
        let (w, preview) = job.await;
        let _ = this.update(cx, |ws, cx| {
            let pv = &mut ws.fx.wave_previews;
            pv.pending.remove(&key);
            pv.ready.insert(key, Rc::new(preview));
            if pv.want_full.contains(&key) {
                pv.full.insert(key, Rc::new(w));
            }
            cx.notify();
        });
    })
    .detach();
}

/// The display copy of a table; `None` while it is being built.
pub fn preview(ws: &mut Workspace, key: TableKey, cx: &mut Context<Workspace>) -> Option<Rc<Preview>> {
    if let TableKey::User(device, o) = key {
        let project = &ws.s.project;
        return ws.fx.wave_previews.user_entry(project, device).previews[o].clone().or_else(|| {
            // A user wave with no table yet plays the oscillator's legacy table.
            let fallback = TableKey::Legacy(0);
            ws.fx.wave_previews.ready.get(&fallback).cloned()
        });
    }
    if let Some(pv) = ws.fx.wave_previews.ready.get(&key) {
        return Some(pv.clone());
    }
    request(ws, key, cx);
    None
}

/// The full frames of a table (the wavetable editor starts from them);
/// `None` while they are being built.
pub fn frames(ws: &mut Workspace, key: TableKey, cx: &mut Context<Workspace>) -> Option<Rc<WaveFrames>> {
    if let TableKey::User(device, o) = key {
        let project = &ws.s.project;
        if let Some(t) = ws.fx.wave_previews.user_entry(project, device).data.tables[o].clone() {
            return Some(t);
        }
        return frames(ws, TableKey::Legacy(0), cx);
    }
    if let Some(w) = ws.fx.wave_previews.full.get(&key) {
        return Some(w.clone());
    }
    let pv = &mut ws.fx.wave_previews;
    if pv.want_full.insert(key) {
        // Built before without its frames: build again, keeping them.
        pv.ready.remove(&key);
        pv.pending.remove(&key);
    }
    request(ws, key, cx);
    None
}

/// The name of the table oscillator `n` plays.
pub fn table_name(ws: &mut Workspace, dev: &Dev, n: u32) -> String {
    match TableKey::of(dev, n) {
        TableKey::Legacy(t) => TABLES[t].to_owned(),
        TableKey::Factory(i) => FACTORY_NAMES[i].to_owned(),
        TableKey::User(device, o) => {
            let project = &ws.s.project;
            ws.fx.wave_previews.data(project, device).tables[o].as_ref().map_or_else(|| "User (empty)".to_owned(), |t| if t.name.is_empty() { "User".to_owned() } else { t.name.clone() })
        }
    }
}

/// The table position oscillator `n` plays at right now (the meter while a
/// voice sounds, else the knob).
pub fn live_pos(dev: &Dev, n: u32) -> f32 {
    let knob = dev.plain(p::osc(n, p::POS)) / 100.;
    if dev.m(0) >= 1. { dev.m(3 + n as usize).clamp(0., 1.) } else { knob }
}

// ───────────────────────────── Compact card ─────────────────────────────

fn section(title: &str, width: f32) -> Div {
    div().flex_none().w(px(width)).h_full().flex().flex_col().gap(px(SP1)).child(caption(title.to_owned()))
}

/// Paint a table's frame at `pos` with its neighbours as ghosts.
pub fn paint_table(window: &mut gpui::Window, r: Rect, pv: Option<&Preview>, pos: f32, color: Hsla, level: f32) {
    let pal = p();
    kit::hline(window, r, r.py(0.5), pal.grid_bar);
    let Some(pv) = pv else { return };
    let scale = 0.4 / pv.peak();
    let wave = |at: f32| kit::sample_curve(r, 128, |fx| 0.5 - scale * pv.sample(at, fx.min(0.9999)));
    if pv.frames.len() > 1 {
        for ghost in [pos - 0.25, pos + 0.25] {
            if (0. ..=1.).contains(&ghost) {
                kit::line(window, &wave(ghost), 1., with_alpha(color, 0.25));
            }
        }
    }
    let now = wave(pos);
    kit::fill_to(window, &now, r.py(0.5), with_alpha(color, 0.1 + 0.2 * level));
    kit::line(window, &now, 2., color);
}

fn osc(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f: u32| p::osc(n, f);
    let key = TableKey::of(dev, n);
    let pv = preview(ws, key, cx);
    let name = table_name(ws, dev, n);
    let wave_name = name.clone();
    let (pos, level) = (live_pos(dev, n), dev.plain(id(p::LEVEL)) / 100.);
    let default = |x: u32| (x, dev.info(x).map_or(0.5, |i| i.default_normalized()));
    let knob_pos = dev.plain(id(p::POS)) / 100.;
    let handles = vec![Handle { fx: knob_pos.clamp(0.02, 0.98), fy: 0.88, x: ax(id(p::POS), 1.), index: n as usize, reset: vec![default(id(p::POS))], ..Default::default() }];
    let on = level > 0.;
    let color = if dev.enabled && on { series(n as usize) } else { p().muted };
    let display = kit::plot(ws, dev, PlotCfg { slot: n as u8, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        paint_table(window, r.inset(2.), pv.as_deref(), pos, color, level);
        kit::bar(window, r.x + 6., r.py(0.88) - 1., r.w - 12., 2., 1., content(0.14));
        kit::dot(window, r.pt(knob_pos.clamp(0.02, 0.98), 0.88), 5., color, Some(pal.text));
        kit::txt(window, cx, &crate::paint::fit(&name, r.w - 56., TEXT_XS), r.x + 6., r.y + 3., TEXT_XS, pal.muted, TextAlign::Left);
        kit::txt(window, cx, &format!("{:.0}%", knob_pos * 100.), r.right() - 6., r.y + 3., TEXT_XS, pal.faint, TextAlign::Right);
    });
    let more: &[u32] = &[id(p::WARP_AMT), id(p::DETUNE), id(p::SPREAD), id(p::PAN)];
    section(["OSC 1", "OSC 2", "OSC 3"][n as usize], 188.)
        .child(div().flex_1().min_h(px(40.)).child(display))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(crate::views::device_window::synth::pop_chip(("wt-wave", dev.key(n as usize)), wave_name, Some(104.), None, crate::views::device_window::synth::SynthPop::Wave { device: dev.id, osc: n }, cx))
                .child(kit::pick(dev, id(p::WARP), cx)),
        )
        .child(kit::strip(ws, dev, &[id(p::LEVEL), id(p::SEMI), id(p::VOICES)], more, cx))
}

fn filter(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let f = FilterView::of(dev, 0);
    let (fx, fy) = f.node(dev.sample_rate);
    let reset = [p::CUTOFF, p::RESONANCE].map(|id| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()))).to_vec();
    let handles = vec![Handle { fx, fy, x: ax(p::CUTOFF, 1.), y: ax(p::RESONANCE, -1.1), alt: ax(p::RESONANCE, -1.5), index: 3, reset }];
    let (sr, on) = (dev.sample_rate, dev.enabled && dev.on(p::filt(0, p::F_ENABLE)));
    let plot = kit::plot(ws, dev, PlotCfg { slot: 3, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        paint_filter(window, cx, r, f, sr, if on { pal.accent } else { pal.muted }, r.w > 260.);
        kit::dot(window, r.pt(fx, fy), 7., if on { pal.accent } else { pal.muted }, Some(pal.text));
    });
    section("FILTER 1", 228.)
        .child(div().flex_1().min_h(px(40.)).child(plot))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, p::filt(0, p::F_ENABLE), cx))
                .child(kit::pick(dev, p::filt(0, p::F_MODEL), cx))
                .when(f.model == 0, |d| d.child(kit::pick(dev, p::FILTER_TYPE, cx)).child(kit::pick(dev, p::FILTER_SLOPE, cx))),
        )
        .child(kit::strip(ws, dev, &[p::CUTOFF, p::RESONANCE, p::FILTER_ENV], &[p::DRIVE, p::KEY_TRACK], cx))
}

fn lfo(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f: u32| p::lfo(n, f);
    let drawn = dev.on(id(p::LFO_DRAWN));
    let shape = aitrack_dsp::lfo::LfoShape::from_index(dev.choice(id(p::LFO_SHAPE)));
    let project = &ws.s.project;
    let curve = ws.fx.wave_previews.data(project, dev.id).curve(n as usize);
    let color = series(4 + n as usize);
    let live = dev.m(6 + n as usize);
    let playing = dev.m(0) >= 1.;
    let region = ws.fx.region(dev.id, 8 + n as u8);
    let display = div().flex_1().min_h(px(32.)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(
        gpui::canvas(
            move |b, _, _| region.set(b),
            move |b, _, window, _| {
                let pal = p();
                let r = Rect::of(b);
                kit::hline(window, r, r.py(0.5), pal.grid_bar);
                kit::vline(window, r, r.px(0.5), pal.grid_beat);
                // Two cycles.
                let pts = if drawn {
                    kit::sample_curve(r, 160, |fx| 0.88 - 0.76 * curve.eval((fx * 2.).fract()))
                } else {
                    kit::sample_curve(r, 160, |fx| 0.5 - 0.38 * super::chorus::lfo_value(shape, fx * 2.))
                };
                kit::fill_to(window, &pts, r.py(0.5), with_alpha(color, 0.14));
                kit::line(window, &pts, 1.5, color);
                if playing {
                    let y = if drawn { 0.88 - 0.76 * (live * 0.5 + 0.5) } else { 0.5 - 0.38 * live };
                    kit::hline(window, r, r.py(y), with_alpha(color, 0.6));
                }
            },
        )
        .size_full(),
    );
    let rate = if dev.on(id(p::LFO_SYNC)) { id(p::LFO_DIVISION) } else { id(p::LFO_RATE) };
    section(["LFO 1", "LFO 2", "LFO 3", "LFO 4"][n as usize], 164.)
        .child(display)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .when(!drawn, |d| d.child(kit::pick(dev, id(p::LFO_SHAPE), cx)))
                .child(kit::pick(dev, id(p::LFO_DRAWN), cx))
                .child(kit::pick(dev, id(p::LFO_SYNC), cx)),
        )
        .child(kit::strip(ws, dev, &[rate], &[], cx))
}

/// The active routings of the 32-slot matrix (edit them in the window).
fn routings(dev: &Dev) -> Div {
    let pal = p();
    let slots = ModSlot::read_all(|id| dev.plain(id));
    let mut list = section("MODULATION", 236.);
    let mut any = false;
    for (s, m) in slots.iter().enumerate().filter(|(_, m)| m.is_active()).take(9) {
        any = true;
        let color = super::super::device_window::synth::source_color(m.source);
        list = list.child(
            div()
                .h(px(20.))
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(mono(format!("{}", s + 1), TEXT_XS, pal.faint).w(px(16.)))
                .child(div().size(px(6.)).rounded_full().bg(color))
                .child(label(crate::paint::fit(modulation::source_name(m.source), 72., TEXT_XS), TEXT_XS, pal.text).w(px(72.)))
                .child(label(crate::paint::fit(dest_name(m.dest), 84., TEXT_XS), TEXT_XS, pal.muted).w(px(84.)))
                .child(mono(format!("{:+.0}%", m.amount * 100.), TEXT_XS, color)),
        );
    }
    if !any {
        list = list.child(label("No routings. Open the synth in its window and drag an envelope, LFO or macro onto a knob.", TEXT_XS, pal.faint));
    }
    list
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let amp = adsr(ws, dev, 4, [p::AMP_ATTACK, p::AMP_DECAY, p::AMP_SUSTAIN, p::AMP_RELEASE], pal.accent, cx);
    let mut row = div().flex_none().h_full().p(px(SP2)).gap(px(SP3)).flex().child(osc(ws, dev, 0, cx)).child(osc(ws, dev, 1, cx));
    if dev.big || dev.plain(p::osc(2, p::LEVEL)) > 0. {
        row = row.child(osc(ws, dev, 2, cx));
    }
    row = row.child(filter(ws, dev, cx)).child(
        section("AMP ENVELOPE", 212.)
            .child(div().flex_1().min_h(px(40.)).child(amp))
            .child(kit::strip(ws, dev, &[p::VOLUME, p::SUB_LEVEL, p::NOISE], &[p::SUB_OCTAVE, p::VEL_SENS], cx)),
    );
    if dev.big {
        let env = adsr(ws, dev, 5, [p::MOD_ATTACK, p::MOD_DECAY, p::MOD_SUSTAIN, p::MOD_RELEASE], pal.accent2, cx);
        let voices = dev.m(0);
        row = row
            .child(section("ENV 2", 212.).child(div().flex_1().min_h(px(40.)).child(env)).child(kit::strip(ws, dev, &[p::FILTER_ENV], &[], cx)))
            .child(lfo(ws, dev, 0, cx))
            .child(lfo(ws, dev, 1, cx))
            .child(routings(dev))
            .child(
                section("VOICE & FX", 200.)
                    .child(div().flex().items_center().gap(px(SP1)).child(kit::pick(dev, p::VOICE_MODE, cx)).child(kit::readout("voices", format!("{voices:.0}"), pal.text)))
                    .child(kit::strip(ws, dev, &[p::GLIDE, p::POLYPHONY, p::DRIFT], &[], cx))
                    .child(kit::strip(ws, dev, &[p::CHORUS_MIX, p::DELAY_MIX, p::REVERB_MIX], &[], cx))
                    .child(kit::strip(ws, dev, &[p::DIST_DRIVE, p::DELAY_FEEDBACK, p::REVERB_DECAY], &[], cx)),
            );
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::instrument::wavetable::modulation::CurvePoint;

    #[test]
    fn envelope_points_follow_the_parameters() {
        let p = adsr_points(0., 0., 1., 0.);
        assert_eq!(p[0], (0., 0.9));
        assert!((p[1].1 - 0.1).abs() < 1e-6 && (p[2].1 - 0.1).abs() < 1e-6, "full sustain stays at the top");
        let slow = adsr_points(1., 1., 0.25, 1.);
        assert!(slow[1].0 > p[1].0 && slow[2].0 - slow[1].0 > p[2].0 - p[1].0);
        assert!((slow[2].1 - 0.7).abs() < 1e-6, "sustain 25% is a quarter of the way up");
        assert_eq!(slow[2].1, slow[3].1);
        for w in slow.windows(2) {
            assert!(w[1].0 > w[0].0, "time runs left to right");
        }
        assert!(slow[4].0 <= 1.);
    }

    #[test]
    fn the_synth_filter_matches_its_design() {
        // 12 dB/oct, no resonance: Butterworth, −3 dB at the cutoff.
        let s = filter_sections(0, false, 1_000., 0., 48_000.);
        assert!((svf_cascade_db(&s, 1_000., 48_000.) + 3.01).abs() < 0.05);
        // 24 dB/oct: two sections, resonance lifts the cutoff.
        let s = filter_sections(0, true, 1_000., 0., 48_000.);
        assert_eq!(s.len(), 2);
        let flat = svf_cascade_db(&s, 1_000., 48_000.);
        assert!((flat + 3.0).abs() < 0.2, "{flat}");
        let peaky = filter_sections(0, true, 1_000., 0.8, 48_000.);
        assert!(svf_cascade_db(&peaky, 1_000., 48_000.) > flat + 10.);
        // The UI's sections are the engine's.
        let engine = aitrack_stock::instrument::wavetable::filter::svf_sections(1, true, 700., 0.4, 48_000.);
        let ours = filter_sections(1, true, 700., 0.4, 48_000.);
        assert!((svf_cascade_db(&engine, 300., 48_000.) - svf_cascade_db(&ours, 300., 48_000.)).abs() < 1e-6);
    }

    #[test]
    fn filter_models_have_their_shapes() {
        let f = |model| FilterView { model, kind: 0, slope24: false, cutoff: 1_000., resonance: 0.5 };
        // Ladder: a low-pass with a resonant bump near the cutoff.
        let ladder = f(1);
        assert!(ladder.db(100., 48e3) > ladder.db(8_000., 48e3) + 30.);
        // Notch: a dip at the cutoff only.
        let notch = f(2);
        assert!(notch.db(1_000., 48e3) < -20. && notch.db(100., 48e3).abs() < 1.);
        // Comb +: peaks at the cutoff's multiples, dips in between.
        let comb = f(3);
        assert!(comb.db(2_000., 48e3) > comb.db(1_500., 48e3) + 6.);
        // Comb −: the opposite.
        let neg = f(4);
        assert!(neg.db(1_500., 48e3) > neg.db(2_000., 48e3) + 6.);
        // Formant: bumps, finite everywhere.
        let formant = f(5);
        assert!((20..20_000).step_by(997).all(|hz| formant.db(hz as f32, 48e3).is_finite()));
    }

    #[test]
    fn base64_round_trips_with_the_synth_state() {
        assert_eq!(b64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(b64_decode("TWE=").unwrap(), b"Ma");
        assert!(b64_decode("T$E=").is_none());
        // A state written by the stock synth comes back.
        let w = WaveFrames::from_harmonics("Mine", 3, |t, k| (if k == 1 { 1. - t } else if k == 2 { t } else { 0. }, 0.));
        let curve = LfoCurve::new(&[CurvePoint { x: 0., y: 0., curve: 0.3 }, CurvePoint { x: 0.5, y: 1., curve: 0. }]);
        let data = SynthData { tables: [None, Some(Rc::new(w.clone())), None], curves: [None, None, Some(curve), None] };
        let bytes = write_state(&BTreeMap::new(), &data);
        let back = read_state(Some(&bytes));
        assert!(back.tables[0].is_none() && back.tables[2].is_none());
        let t = back.tables[1].as_ref().unwrap();
        assert_eq!((t.name.as_str(), t.frames.len()), ("Mine", 3));
        assert!(t.frames[0].iter().zip(&w.frames[0]).all(|(a, b)| (a - b).abs() < 1e-3), "16-bit PCM is close");
        assert_eq!(back.curve(2).points().len(), 2);
        assert!((back.curve(2).points()[0].curve - 0.3).abs() < 1e-6);
        assert_eq!(back.curve(0), LfoCurve::default());
        assert_eq!(read_state(None), SynthData::default());
        assert_eq!(read_state(Some(b"not json")), SynthData::default());
    }

    #[test]
    fn previews_keep_a_few_frames_and_interpolate() {
        let w = WaveFrames::new("Ramp", (0..200).map(|f| vec![f as f32 / 199.; 2048]).collect());
        let pv = Preview::of(&w);
        assert_eq!((pv.frames.len(), pv.total, pv.frames[0].len()), (PREVIEW_FRAMES, 200, PREVIEW_LEN));
        assert_eq!(pv.sample(0., 0.3), 0.);
        assert!((pv.sample(1., 0.3) - 1.).abs() < 1e-6);
        assert!((pv.sample(0.5, 0.7) - 0.5).abs() < 0.02, "between frames");
        let one = Preview::of(&WaveFrames::single("s", vec![0.5; 64]));
        assert_eq!(one.frames.len(), 1);
        assert_eq!(one.sample(0.7, 0.2), 0.5);
    }

    #[test]
    fn oscillators_name_their_tables() {
        use super::super::testing::dev;
        let id = p::osc(0, p::WAVE);
        assert_eq!(TableKey::of(&dev("instrument.wavetable", &[]), 0), TableKey::Legacy(0));
        assert_eq!(TableKey::of(&dev("instrument.wavetable", &[(id, 3.)]), 0), TableKey::Factory(2));
        assert_eq!(TableKey::of(&dev("instrument.wavetable", &[(id, USER_WAVE as f32)]), 0), TableKey::User(DeviceId(1), 0));
        assert_eq!(TableKey::of(&dev("instrument.wavetable", &[(p::osc(1, p::TABLE), 4.)]), 1), TableKey::Legacy(4));
    }
}
