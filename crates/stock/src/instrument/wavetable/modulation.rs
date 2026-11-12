//! Modulation: sources, the matrix data model, drawable LFO curves and the
//! curved envelopes.
//!
//! The matrix is plain parameters, so a slot is automatable, undoable and
//! saved with the project like any knob. Slot `s` (0..[`SLOTS`]) lives at
//! `p::slot_x(s, field)`:
//!
//! | field | range | meaning |
//! |---|---|---|
//! | `M_SRC` | 0..63 stepped | index into [`SOURCES`] (append-only list) |
//! | `M_DST` | 0..1023 stepped | the destination **parameter id** (any synth parameter) or a virtual destination (`vdst::*`, ids ≥ 900) |
//! | `M_AMT` | −100..100 % | depth; 100 % sweeps the whole knob range |
//! | `M_MODE` | Natural / Unipolar / Bipolar | how the source is mapped before scaling |
//! | `M_VIA` | 0..63 stepped | optional second source that scales the depth (0 = none) |
//!
//! Modulation is added in the destination's **normalised** space, so 100 %
//! on a log cutoff knob is the whole 20 Hz–20 kHz range, the same as dragging
//! the knob from end to end. Virtual destinations have their own units
//! (see `vdst`).
//!
//! For drag-and-drop a UI reads [`ModSlot::read_all`], shows the sources from
//! [`SOURCES`], and turns a drop into parameter edits with
//! [`ModSlot::assign`] / [`ModSlot::param_values`] (plain values, ready for
//! `SetParam` commands).

use aitrack_dsp::ParamInfo;

/// Modulation sources, by stable index (append only; never reorder).
pub const SOURCES: &[&str] = &[
    "None",
    "Env 1 (Amp)",
    "Env 2",
    "Env 3",
    "LFO 1",
    "LFO 2",
    "LFO 3",
    "LFO 4",
    "Velocity",
    "Key Track",
    "Mod Wheel",
    "Aftertouch",
    "Pitch Bend",
    "Macro 1",
    "Macro 2",
    "Macro 3",
    "Macro 4",
    "Random",
    "Constant",
    "Note Gate",
];

/// Source indices into [`SOURCES`].
pub mod src {
    pub const NONE: u8 = 0;
    pub const ENV1: u8 = 1;
    pub const ENV2: u8 = 2;
    pub const ENV3: u8 = 3;
    pub const LFO1: u8 = 4;
    pub const LFO2: u8 = 5;
    pub const LFO3: u8 = 6;
    pub const LFO4: u8 = 7;
    pub const VELOCITY: u8 = 8;
    pub const KEY_TRACK: u8 = 9;
    pub const MOD_WHEEL: u8 = 10;
    pub const AFTERTOUCH: u8 = 11;
    pub const PITCH_BEND: u8 = 12;
    pub const MACRO1: u8 = 13;
    pub const MACRO2: u8 = 14;
    pub const MACRO3: u8 = 15;
    pub const MACRO4: u8 = 16;
    /// A new random value 0..1 on every note.
    pub const RANDOM: u8 = 17;
    /// Always 1: a static offset for any parameter.
    pub const CONSTANT: u8 = 18;
    /// 1 while the key is held, 0 after note-off.
    pub const NOTE_GATE: u8 = 19;
    pub const COUNT: usize = 20;
}

/// Highest stored source index (the parameter range; keeps normalised values
/// stable while [`SOURCES`] grows).
pub const SRC_MAX: f32 = 63.0;
/// Highest stored destination id.
pub const DST_MAX: f32 = 1023.0;
/// Modulation slots in the matrix.
pub const SLOTS: usize = 32;

/// Natively bipolar sources (−1..1); the others are 0..1.
pub fn source_is_bipolar(s: u8) -> bool {
    matches!(s, src::LFO1..=src::LFO4 | src::KEY_TRACK | src::PITCH_BEND)
}

/// Sources that change per voice (the rest are global).
pub fn source_is_per_voice(s: u8) -> bool {
    matches!(s, src::ENV1..=src::ENV3 | src::VELOCITY | src::KEY_TRACK | src::RANDOM | src::NOTE_GATE) || matches!(s, src::LFO1..=src::LFO4)
}

pub fn source_name(s: u8) -> &'static str {
    SOURCES.get(s as usize).copied().unwrap_or("?")
}

/// How a slot maps its source before scaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Polarity {
    /// The source's own range (LFOs −1..1, envelopes 0..1).
    #[default]
    Natural,
    /// Always 0..1.
    Unipolar,
    /// Always −1..1.
    Bipolar,
}

pub const MODES: &[&str] = &["Natural", "Unipolar", "Bipolar"];

impl Polarity {
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => Polarity::Unipolar,
            2 => Polarity::Bipolar,
            _ => Polarity::Natural,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Map a raw source value.
    #[inline]
    pub fn map(self, source: u8, x: f32) -> f32 {
        let bi = source_is_bipolar(source);
        match (self, bi) {
            (Polarity::Natural, _) | (Polarity::Unipolar, false) | (Polarity::Bipolar, true) => x,
            (Polarity::Unipolar, true) => 0.5 * (x + 1.0),
            (Polarity::Bipolar, false) => 2.0 * x - 1.0,
        }
    }
}

/// One matrix slot as the UI sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModSlot {
    pub source: u8,
    /// Destination parameter id or virtual destination (`vdst::*`).
    pub dest: u32,
    /// −1..1 (the parameter shows −100..100 %).
    pub amount: f32,
    pub polarity: Polarity,
    /// Optional depth control source (0 = none).
    pub via: u8,
}

impl Default for ModSlot {
    fn default() -> Self {
        Self { source: 0, dest: 0, amount: 0.0, polarity: Polarity::Natural, via: 0 }
    }
}

impl ModSlot {
    pub fn is_active(&self) -> bool {
        self.source != 0 && self.dest != 0 && self.amount != 0.0
    }

    /// Read slot `s` through a plain-value getter (`|id| plain value`).
    pub fn read(s: usize, plain: impl Fn(u32) -> f32) -> Self {
        let id = |f| super::p::slot_x(s as u32, f);
        Self {
            source: plain(id(super::p::M_SRC)).round().clamp(0.0, SRC_MAX) as u8,
            dest: plain(id(super::p::M_DST)).round().clamp(0.0, DST_MAX) as u32,
            amount: plain(id(super::p::M_AMT)) / 100.0,
            polarity: Polarity::from_index(plain(id(super::p::M_MODE)).round() as usize),
            via: plain(id(super::p::M_VIA)).round().clamp(0.0, SRC_MAX) as u8,
        }
    }

    /// Every slot, in order.
    pub fn read_all(plain: impl Fn(u32) -> f32) -> Vec<ModSlot> {
        (0..SLOTS).map(|s| Self::read(s, &plain)).collect()
    }

    /// The `(param id, plain value)` edits that store `self` in slot `s`.
    pub fn param_values(&self, s: usize) -> [(u32, f32); 5] {
        let id = |f| super::p::slot_x(s as u32, f);
        [
            (id(super::p::M_SRC), self.source as f32),
            (id(super::p::M_DST), self.dest as f32),
            (id(super::p::M_AMT), (self.amount * 100.0).clamp(-100.0, 100.0)),
            (id(super::p::M_MODE), self.polarity.index() as f32),
            (id(super::p::M_VIA), self.via as f32),
        ]
    }

    /// Drag-and-drop: the slot to use for `source → dest` (the existing slot
    /// for the same pair, else the first empty one) and the edits that set
    /// it. `None` when the matrix is full.
    pub fn assign(slots: &[ModSlot], source: u8, dest: u32, amount: f32) -> Option<(usize, [(u32, f32); 5])> {
        let s = slots.iter().position(|m| m.source == source && m.dest == dest).or_else(|| slots.iter().position(|m| !m.is_active()))?;
        let slot = ModSlot { source, dest, amount, ..slots[s] };
        Some((s, slot.param_values(s)))
    }

    /// Slots that modulate `dest` (to draw modulation rings on a knob).
    pub fn targeting(slots: &[ModSlot], dest: u32) -> Vec<usize> {
        slots.iter().enumerate().filter(|(_, m)| m.is_active() && m.dest == dest).map(|(i, _)| i).collect()
    }
}

/// Whether a parameter makes sense as a modulation destination (continuous
/// knobs; the matrix itself and switches are excluded).
pub fn is_modulatable(info: &ParamInfo) -> bool {
    info.scale != aitrack_dsp::ParamScale::Stepped && !(super::p::MATRIX..super::p::MATRIX_END).contains(&info.id)
}

// ── Curves shared by envelopes and LFO segments ─────────────────────────────

/// Shape a 0..1 ramp. `curve ∈ [−1, 1]`: 0 linear, > 0 fast start
/// (exponential "analogue" decay), < 0 slow start.
#[inline]
pub fn shape(t: f32, curve: f32) -> f32 {
    let k = curve * CURVE_K;
    if k.abs() < 1e-3 {
        return t;
    }
    (1.0 - (-k * t).exp()) / (1.0 - (-k).exp())
}

/// Curvature at ±100 %.
pub const CURVE_K: f32 = 10.0;

// ── Drawable LFO curves ─────────────────────────────────────────────────────

/// Most breakpoints in an LFO curve.
pub const MAX_POINTS: usize = 32;

/// One breakpoint: `x` phase 0..1, `y` value 0..1, `curve` of the segment
/// that **leaves** this point (−1..1, see [`shape`]).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
    pub curve: f32,
}

/// A drawable LFO shape: up to [`MAX_POINTS`] breakpoints over one cycle.
/// `Copy` and fixed-size, so the audio thread reads it without allocation.
/// The segment after the last point wraps to the first point's value at
/// phase 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoCurve {
    points: [CurvePoint; MAX_POINTS],
    len: usize,
}

impl Default for LfoCurve {
    fn default() -> Self {
        Self::triangle()
    }
}

impl LfoCurve {
    /// From points (sorted by `x`, clamped; extra points are dropped). An
    /// empty list gives a flat line at 0.5.
    pub fn new(points: &[CurvePoint]) -> Self {
        let mut pts = [CurvePoint::default(); MAX_POINTS];
        let mut v: Vec<CurvePoint> = points
            .iter()
            .take(MAX_POINTS)
            .map(|p| CurvePoint { x: p.x.clamp(0.0, 1.0), y: p.y.clamp(0.0, 1.0), curve: p.curve.clamp(-1.0, 1.0) })
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .collect();
        v.sort_by(|a, b| a.x.total_cmp(&b.x));
        if v.is_empty() {
            v.push(CurvePoint { x: 0.0, y: 0.5, curve: 0.0 });
        }
        let len = v.len();
        pts[..len].copy_from_slice(&v);
        Self { points: pts, len }
    }

    pub fn points(&self) -> &[CurvePoint] {
        &self.points[..self.len]
    }

    /// Value 0..1 at phase `x ∈ [0, 1]`.
    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        let pts = &self.points[..self.len];
        let x = x.clamp(0.0, 1.0);
        let first = pts[0];
        if x < first.x {
            // Before the first point: the wrap segment from the last point.
            let last = pts[self.len - 1];
            return seg(CurvePoint { x: last.x - 1.0, ..last }, first, x);
        }
        for w in 0..self.len - 1 {
            if x < pts[w + 1].x {
                return seg(pts[w], pts[w + 1], x);
            }
        }
        let last = pts[self.len - 1];
        seg(last, CurvePoint { x: first.x + 1.0, ..first }, x)
    }

    pub fn sine() -> Self {
        Self::new(&[
            CurvePoint { x: 0.0, y: 0.5, curve: -0.25 },
            CurvePoint { x: 0.25, y: 1.0, curve: 0.25 },
            CurvePoint { x: 0.5, y: 0.5, curve: -0.25 },
            CurvePoint { x: 0.75, y: 0.0, curve: 0.25 },
        ])
    }

    pub fn triangle() -> Self {
        Self::new(&[CurvePoint { x: 0.0, y: 0.0, curve: 0.0 }, CurvePoint { x: 0.5, y: 1.0, curve: 0.0 }])
    }

    pub fn ramp_down() -> Self {
        Self::new(&[CurvePoint { x: 0.0, y: 1.0, curve: 0.0 }, CurvePoint { x: 1.0, y: 0.0, curve: 0.0 }])
    }

    /// Sidechain-style duck: drops to 0 at the beat and swells back.
    pub fn duck() -> Self {
        Self::new(&[CurvePoint { x: 0.0, y: 0.0, curve: -0.6 }, CurvePoint { x: 0.6, y: 1.0, curve: 0.0 }, CurvePoint { x: 0.99, y: 1.0, curve: 0.0 }])
    }

    /// Fast pluck: up instantly, exponential fall.
    pub fn pluck() -> Self {
        Self::new(&[CurvePoint { x: 0.0, y: 1.0, curve: 0.7 }, CurvePoint { x: 1.0, y: 0.0, curve: 0.0 }])
    }

    /// Four-step gate pattern.
    pub fn steps() -> Self {
        Self::new(&[
            CurvePoint { x: 0.0, y: 1.0, curve: 0.0 },
            CurvePoint { x: 0.2, y: 1.0, curve: 0.0 },
            CurvePoint { x: 0.2, y: 0.0, curve: 0.0 },
            CurvePoint { x: 0.5, y: 0.0, curve: 0.0 },
            CurvePoint { x: 0.5, y: 0.7, curve: 0.0 },
            CurvePoint { x: 0.7, y: 0.7, curve: 0.0 },
            CurvePoint { x: 0.7, y: 0.0, curve: 0.0 },
        ])
    }

    /// Named presets for an LFO editor.
    pub fn presets() -> Vec<(&'static str, LfoCurve)> {
        vec![("Sine", Self::sine()), ("Triangle", Self::triangle()), ("Ramp Down", Self::ramp_down()), ("Duck", Self::duck()), ("Pluck", Self::pluck()), ("Steps", Self::steps())]
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.points().iter().map(|p| serde_json::json!([p.x, p.y, p.curve])).collect())
    }

    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let arr = v.as_array()?;
        let pts: Vec<CurvePoint> = arr
            .iter()
            .filter_map(|p| {
                let a = p.as_array()?;
                Some(CurvePoint { x: a.first()?.as_f64()? as f32, y: a.get(1)?.as_f64()? as f32, curve: a.get(2).and_then(|c| c.as_f64()).unwrap_or(0.0) as f32 })
            })
            .collect();
        Some(Self::new(&pts))
    }
}

#[inline]
fn seg(a: CurvePoint, b: CurvePoint, x: f32) -> f32 {
    let w = b.x - a.x;
    if w <= 1e-6 {
        // Only the wrap segment of a curve ending at x = 1 is this narrow.
        return a.y;
    }
    let t = ((x - a.x) / w).clamp(0.0, 1.0);
    a.y + (b.y - a.y) * shape(t, a.curve)
}

// ── Curved ADSR ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
}

/// Envelope settings: times in ms, sustain 0..1, curves −1..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvSettings {
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub attack_curve: f32,
    pub decay_curve: f32,
    pub release_curve: f32,
}

impl Default for EnvSettings {
    fn default() -> Self {
        Self { attack: 2.0, hold: 0.0, decay: 300.0, sustain: 0.8, release: 200.0, attack_curve: 0.0, decay_curve: 0.7, release_curve: 0.7 }
    }
}

/// AHDSR with a shaped curve per segment. A segment runs `n` samples from
/// `from` to `to`; with curve `k ≠ 0` the value is
/// `from + (to − from)·(1 − e^{−k t})/(1 − e^{−k})`, evaluated with one
/// multiply per sample.
#[derive(Clone, Copy, Debug)]
pub struct CurveEnv {
    stage: Stage,
    value: f32,
    s: EnvSettings,
    sr: f32,
    // Current segment.
    from: f32,
    to: f32,
    left: u32,
    lin_step: f32,
    e: f32,
    mul: f32,
    denom: f32,
    curved: bool,
}

impl CurveEnv {
    pub fn new(sample_rate: f64) -> Self {
        Self { stage: Stage::Idle, value: 0.0, s: EnvSettings::default(), sr: sample_rate as f32, from: 0.0, to: 0.0, left: 0, lin_step: 0.0, e: 1.0, mul: 1.0, denom: 1.0, curved: false }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sr = sample_rate as f32;
    }

    /// New settings apply from the next segment (and to sustain at once).
    pub fn set(&mut self, s: EnvSettings) {
        self.s = s;
        if self.stage == Stage::Sustain {
            self.value = s.sustain;
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.value = 0.0;
    }

    /// Start (or restart, legato-free) from zero.
    pub fn retrigger(&mut self) {
        self.value = 0.0;
        self.gate_on();
    }

    /// Attack from the current value.
    pub fn gate_on(&mut self) {
        self.begin(Stage::Attack);
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.begin(Stage::Release);
        }
    }

    fn begin(&mut self, stage: Stage) {
        let s = self.s;
        let (to, ms, curve) = match stage {
            Stage::Attack => (1.0, s.attack, s.attack_curve),
            Stage::Hold => (1.0, s.hold, 0.0),
            Stage::Decay => (s.sustain, s.decay, s.decay_curve),
            Stage::Release => (0.0, s.release, s.release_curve),
            Stage::Sustain => {
                self.stage = Stage::Sustain;
                self.value = s.sustain;
                return;
            }
            Stage::Idle => {
                self.stage = Stage::Idle;
                self.value = 0.0;
                return;
            }
        };
        self.stage = stage;
        self.from = self.value;
        self.to = to;
        let n = (ms.max(0.0) * 0.001 * self.sr).round().max(1.0);
        self.left = n as u32;
        let k = curve.clamp(-1.0, 1.0) * CURVE_K;
        self.curved = k.abs() >= 1e-3;
        if self.curved {
            self.e = 1.0;
            self.mul = (-k / n).exp();
            self.denom = 1.0 / (1.0 - (-k).exp());
        } else {
            self.lin_step = (to - self.from) / n;
        }
    }

    fn next_stage(&mut self) {
        match self.stage {
            Stage::Attack => {
                if self.s.hold > 0.0 {
                    self.begin(Stage::Hold)
                } else {
                    self.begin(Stage::Decay)
                }
            }
            Stage::Hold => self.begin(Stage::Decay),
            Stage::Decay => self.begin(Stage::Sustain),
            Stage::Release => self.begin(Stage::Idle),
            _ => {}
        }
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Idle | Stage::Sustain => {}
            _ => {
                if self.curved {
                    self.e *= self.mul;
                    self.value = self.from + (self.to - self.from) * (1.0 - self.e) * self.denom;
                } else {
                    self.value += self.lin_step;
                }
                self.left = self.left.saturating_sub(1);
                if self.left == 0 {
                    self.value = self.to;
                    self.next_stage();
                }
            }
        }
        self.value
    }

    /// Advance `n` samples (control-rate envelopes).
    #[inline]
    pub fn advance(&mut self, n: usize) -> f32 {
        for _ in 0..n {
            self.next();
            if matches!(self.stage, Stage::Idle | Stage::Sustain) {
                break;
            }
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_hits_its_times() {
        let sr = 48_000.0;
        let mut e = CurveEnv::new(sr);
        e.set(EnvSettings { attack: 10.0, hold: 5.0, decay: 20.0, sustain: 0.5, release: 30.0, attack_curve: 0.0, decay_curve: 0.7, release_curve: -0.5 });
        e.retrigger();
        for _ in 0..479 {
            e.next();
        }
        assert!(e.value() < 1.0 && e.value() > 0.99);
        e.next();
        assert_eq!(e.stage(), Stage::Hold);
        for _ in 0..240 {
            e.next();
        }
        assert_eq!(e.stage(), Stage::Decay);
        // Fast-start decay: well past half way at a quarter of the time.
        for _ in 0..240 {
            e.next();
        }
        assert!(e.value() < 0.75, "{}", e.value());
        for _ in 0..720 {
            e.next();
        }
        assert_eq!(e.stage(), Stage::Sustain);
        assert!((e.value() - 0.5).abs() < 1e-6);
        e.gate_off();
        // Slow-start release: above the linear line at a quarter.
        for _ in 0..360 {
            e.next();
        }
        assert!(e.value() > 0.375, "{}", e.value());
        for _ in 0..1080 {
            e.next();
        }
        assert!(!e.is_active());
        assert_eq!(e.value(), 0.0);
    }

    #[test]
    fn curve_shape_endpoints() {
        for c in [-1.0, -0.3, 0.0, 0.5, 1.0] {
            assert!(shape(0.0, c).abs() < 1e-6);
            assert!((shape(1.0, c) - 1.0).abs() < 1e-5);
        }
        assert!(shape(0.25, 0.7) > 0.25 && shape(0.25, -0.7) < 0.25);
    }

    #[test]
    fn lfo_curve_evaluates_and_round_trips() {
        let c = LfoCurve::triangle();
        assert!(c.eval(0.0).abs() < 1e-6);
        assert!((c.eval(0.25) - 0.5).abs() < 1e-6);
        assert!((c.eval(0.5) - 1.0).abs() < 1e-6);
        assert!((c.eval(0.75) - 0.5).abs() < 1e-6);
        let s = LfoCurve::sine();
        assert!((s.eval(0.25) - 1.0).abs() < 1e-5 && s.eval(0.75) < 1e-5);
        // Steps: vertical jumps hold.
        let st = LfoCurve::steps();
        assert_eq!(st.eval(0.1), 1.0);
        assert_eq!(st.eval(0.3), 0.0);
        assert_eq!(st.eval(0.6), 0.7);
        let back = LfoCurve::from_json(&LfoCurve::duck().to_json()).unwrap();
        assert_eq!(back, LfoCurve::duck());
        // Points before the first one wrap from the last.
        let w = LfoCurve::new(&[CurvePoint { x: 0.5, y: 1.0, curve: 0.0 }]);
        assert_eq!(w.eval(0.1), 1.0);
    }

    #[test]
    fn polarity_maps() {
        assert_eq!(Polarity::Unipolar.map(src::LFO1, -1.0), 0.0);
        assert_eq!(Polarity::Bipolar.map(src::VELOCITY, 0.0), -1.0);
        assert_eq!(Polarity::Natural.map(src::LFO1, -1.0), -1.0);
        assert_eq!(Polarity::Natural.map(src::ENV1, 0.3), 0.3);
    }

    #[test]
    fn assign_reuses_and_fills_slots() {
        let mut slots = vec![ModSlot::default(); SLOTS];
        slots[0] = ModSlot { source: src::LFO1, dest: super::super::p::CUTOFF, amount: 0.5, ..Default::default() };
        let (s, edits) = ModSlot::assign(&slots, src::LFO1, super::super::p::CUTOFF, 0.2).unwrap();
        assert_eq!(s, 0);
        assert_eq!(edits[2].1, 20.0);
        let (s, _) = ModSlot::assign(&slots, src::ENV2, super::super::p::CUTOFF, 0.2).unwrap();
        assert_eq!(s, 1);
        assert_eq!(ModSlot::targeting(&slots, super::super::p::CUTOFF), vec![0]);
        let full = vec![ModSlot { source: 1, dest: 5, amount: 1.0, ..Default::default() }; SLOTS];
        assert!(ModSlot::assign(&full, src::ENV2, 7, 0.2).is_none());
    }
}
