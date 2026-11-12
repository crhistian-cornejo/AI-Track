//! Pure layout and mapping helpers of the synth window: envelope geometry,
//! LFO breakpoint editing, modulation rings, drag-to-route, harmonic bars,
//! freehand drawing and the stacked wavetable view. No GPUI here, so all of
//! it is tested below.

use aitrack_dsp::ParamInfo;
use aitrack_stock::instrument::wavetable::modulation::{self, CurvePoint, ModSlot, shape};

// ───────────────────────────── Modulation ─────────────────────────────

/// The span a modulation ring covers on a knob, in normalised knob travel:
/// `value → value + amount` for unipolar sources, `value ± amount` for
/// bipolar ones (amount 100 % = the whole travel), clamped to the knob.
pub fn ring_span(value: f32, amount: f32, bipolar: bool) -> (f32, f32) {
    let (a, b) = if bipolar { (value - amount.abs(), value + amount.abs()) } else { (value, value + amount) };
    (a.min(b).clamp(0., 1.), a.max(b).clamp(0., 1.))
}

/// Whether slot `m` swings both ways around the knob.
pub fn slot_is_bipolar(m: &ModSlot) -> bool {
    use modulation::Polarity;
    match m.polarity {
        Polarity::Natural => modulation::source_is_bipolar(m.source),
        Polarity::Unipolar => false,
        Polarity::Bipolar => true,
    }
}

/// The depth a new routing starts at: half the knob for envelopes and
/// macros, a quarter each way for bipolar sources.
pub fn default_amount(source: u8) -> f32 {
    if modulation::source_is_bipolar(source) { 0.25 } else { 0.5 }
}

/// A modulation source dropped on the knob of `info`: the slot it takes and
/// the plain values that set it (an existing slot for the same pair keeps
/// its place and gets the new depth). `None` when the knob cannot be
/// modulated or the matrix is full.
pub fn route_drop(slots: &[ModSlot], source: u8, info: &ParamInfo) -> Option<(usize, [(u32, f32); 5])> {
    if source == 0 || !modulation::is_modulatable(info) {
        return None;
    }
    ModSlot::assign(slots, source, info.id, default_amount(source))
}

/// The plain values that clear slot `s`.
pub fn clear_slot(s: usize) -> [(u32, f32); 5] {
    ModSlot::default().param_values(s)
}

// ───────────────────────────── Envelopes ─────────────────────────────

/// An AHDSR as its editor draws it: times as the knobs' normalised values,
/// sustain 0..1, curves −1..1 (attack, decay, release).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EnvShape {
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub curves: [f32; 3],
}

pub const ENV_TOP: f32 = 0.1;
pub const ENV_BOTTOM: f32 = 0.9;
/// Plot widths of the segments at full travel.
pub const A_W: f32 = 0.24;
pub const H_W: f32 = 0.12;
pub const D_W: f32 = 0.24;
pub const S_W: f32 = 0.12;
pub const R_W: f32 = 0.24;

impl EnvShape {
    fn level_y(&self, level: f32) -> f32 {
        ENV_BOTTOM - (ENV_BOTTOM - ENV_TOP) * level.clamp(0., 1.)
    }

    /// Corners at plot fractions: start, attack peak, hold end, decay
    /// knee, sustain end (key up), end of release.
    pub fn corners(&self) -> [(f32, f32); 6] {
        let peak = 0.01 + A_W * self.attack.clamp(0., 1.);
        let hold = peak + H_W * self.hold.clamp(0., 1.);
        let knee = hold + 0.01 + D_W * self.decay.clamp(0., 1.);
        let sus = knee + S_W;
        let end = sus + 0.01 + R_W * self.release.clamp(0., 1.);
        let s = self.level_y(self.sustain);
        [(0., ENV_BOTTOM), (peak, ENV_TOP), (hold, ENV_TOP), (knee, s), (sus, s), (end.min(1.), ENV_BOTTOM)]
    }

    /// The level (0..1) of segment `seg` (0 attack, 1 decay, 2 release) at
    /// its fraction `t`, as the synth's curved segments run.
    fn seg_level(&self, seg: usize, t: f32) -> f32 {
        let s = self.sustain.clamp(0., 1.);
        let (from, to) = [(0., 1.), (1., s), (s, 0.)][seg];
        from + (to - from) * shape(t.clamp(0., 1.), self.curves[seg].clamp(-1., 1.))
    }

    /// The midpoint of a curved segment, where its bend handle sits.
    pub fn bend_handle(&self, seg: usize) -> (f32, f32) {
        let c = self.corners();
        let (a, b) = [(c[0], c[1]), (c[2], c[3]), (c[4], c[5])][seg];
        ((a.0 + b.0) / 2., self.level_y(self.seg_level(seg, 0.5)))
    }

    /// The drawn curve, `n` points per segment.
    pub fn polyline(&self, n: usize) -> Vec<(f32, f32)> {
        let c = self.corners();
        let n = n.max(2);
        let mut out = Vec::with_capacity(3 * n + 4);
        let seg = |out: &mut Vec<(f32, f32)>, k: usize, a: (f32, f32), b: (f32, f32)| {
            for i in 0..=n {
                let t = i as f32 / n as f32;
                out.push((a.0 + (b.0 - a.0) * t, self.level_y(self.seg_level(k, t))));
            }
        };
        seg(&mut out, 0, c[0], c[1]);
        out.push(c[2]);
        seg(&mut out, 1, c[2], c[3]);
        out.push(c[4]);
        seg(&mut out, 2, c[4], c[5]);
        out
    }
}

/// How a bend handle's vertical drag changes its curve (per plot height):
/// up bends a rising segment towards a fast start, down a falling one.
pub fn bend_per_frac(seg: usize) -> f32 {
    if seg == 0 { -1.6 } else { 1.6 }
}

// ───────────────────────────── LFO curves ─────────────────────────────

/// Vertical margin of an LFO plot (fraction of its height).
pub const LFO_M: f32 = 0.08;

/// A breakpoint `(x, y)` (both 0..1, y up) at plot fractions (y down).
pub fn lfo_to_plot(x: f32, y: f32) -> (f32, f32) {
    (x, LFO_M + (1. - 2. * LFO_M) * (1. - y))
}

/// The inverse of [`lfo_to_plot`], clamped to the curve's square.
pub fn plot_to_lfo(fx: f32, fy: f32) -> (f32, f32) {
    (fx.clamp(0., 1.), (1. - (fy - LFO_M) / (1. - 2. * LFO_M)).clamp(0., 1.))
}

/// The midpoint of the segment that leaves point `i` (the last one wraps
/// to the first point at phase 1), as a breakpoint.
pub fn segment_mid(points: &[CurvePoint], i: usize) -> (f32, f32) {
    let a = points[i];
    let (b, bx) = if i + 1 < points.len() { (points[i + 1], points[i + 1].x) } else { (points[0], points[0].x + 1.) };
    let x = ((a.x + bx) / 2.).rem_euclid(1.);
    (x, a.y + (b.y - a.y) * shape(0.5, a.curve))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LfoHit {
    Point(usize),
    /// The bend handle of the segment leaving this point.
    Bend(usize),
    Empty,
}

/// What the pointer at plot fractions `(fx, fy)` is over, within `reach`
/// pixels on a `w × h` plot. Points win over bend handles.
pub fn lfo_hit(points: &[CurvePoint], fx: f32, fy: f32, w: f32, h: f32, reach: f32) -> LfoHit {
    let dist = |(x, y): (f32, f32)| {
        let (px, py) = lfo_to_plot(x, y);
        ((px - fx) * w).hypot((py - fy) * h)
    };
    let near = |d: f32| d <= reach;
    let best = points.iter().enumerate().map(|(i, p)| (i, dist((p.x, p.y)))).min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((i, d)) = best
        && near(d)
    {
        return LfoHit::Point(i);
    }
    let bend = (0..points.len()).map(|i| (i, dist(segment_mid(points, i)))).min_by(|a, b| a.1.total_cmp(&b.1));
    match bend {
        Some((i, d)) if near(d) => LfoHit::Bend(i),
        _ => LfoHit::Empty,
    }
}

/// Snap to `steps` divisions when `on`.
pub fn snap(v: f32, steps: f32, on: bool) -> f32 {
    if on { (v * steps).round() / steps } else { v }
}

/// Move point `i` to `(x, y)`; it stays between its neighbours, so the
/// order (and every index) is kept.
pub fn lfo_move(points: &[CurvePoint], i: usize, x: f32, y: f32) -> Vec<CurvePoint> {
    let mut v = points.to_vec();
    if i >= v.len() {
        return v;
    }
    let lo = if i == 0 { 0. } else { v[i - 1].x };
    let hi = if i + 1 < v.len() { v[i + 1].x } else { 1. };
    v[i].x = x.clamp(lo, hi);
    v[i].y = y.clamp(0., 1.);
    v
}

/// Add a point at `(x, y)`; returns the points and the new point's index.
/// The segment it lands in keeps its curve on both halves.
pub fn lfo_insert(points: &[CurvePoint], x: f32, y: f32) -> Option<(Vec<CurvePoint>, usize)> {
    if points.len() >= modulation::MAX_POINTS {
        return None;
    }
    let mut v = points.to_vec();
    let at = v.iter().position(|p| p.x > x).unwrap_or(v.len());
    let curve = if at > 0 { v[at - 1].curve } else { v.last().map_or(0., |p| p.curve) };
    v.insert(at, CurvePoint { x: x.clamp(0., 1.), y: y.clamp(0., 1.), curve });
    Some((v, at))
}

/// Remove point `i` (a curve keeps at least two).
pub fn lfo_delete(points: &[CurvePoint], i: usize) -> Vec<CurvePoint> {
    let mut v = points.to_vec();
    if v.len() > 2 && i < v.len() {
        v.remove(i);
    }
    v
}

/// Bend the segment leaving point `i` by a vertical pointer move of `dy`
/// plot fractions (down = positive): the handle follows the pointer.
pub fn lfo_bend(points: &[CurvePoint], i: usize, dy: f32) -> Vec<CurvePoint> {
    let mut v = points.to_vec();
    if i >= v.len() {
        return v;
    }
    let next = if i + 1 < v.len() { v[i + 1].y } else { v[0].y };
    let rising = next >= v[i].y;
    let k = -dy * 2.4 * if rising { 1. } else { -1. };
    v[i].curve = (v[i].curve + k).clamp(-1., 1.);
    v
}

// ───────────────────────────── Wavetable editor ─────────────────────────────

/// Harmonic bars shown by the additive editor.
pub const HARMONICS: usize = 32;

/// The bar under plot fraction `fx`.
pub fn bar_at(fx: f32, count: usize) -> usize {
    ((fx.clamp(0., 0.9999) * count as f32) as usize).min(count.saturating_sub(1))
}

/// A bar's amplitude (0..1, linear) at plot fraction `fy` (0 = top).
pub fn bar_level(fy: f32) -> f32 {
    (1. - fy).clamp(0., 1.)
}

/// A sample value (−1..1) at plot fraction `fy` of a waveform plot.
pub fn wave_level(fy: f32) -> f32 {
    (1. - 2. * fy).clamp(-1., 1.)
}

/// Paint a freehand stroke (points `(phase 0..1, value −1..1)` in drawing
/// order) into one cycle: the samples under each stretch of the stroke take
/// the line between its ends; the rest of the cycle stays.
pub fn paint_stroke(cycle: &mut [f32], stroke: &[(f32, f32)]) {
    let n = cycle.len();
    if n == 0 || stroke.is_empty() {
        return;
    }
    let idx = |x: f32| ((x.clamp(0., 1.) * n as f32) as usize).min(n - 1);
    if stroke.len() == 1 {
        cycle[idx(stroke[0].0)] = stroke[0].1.clamp(-1., 1.);
        return;
    }
    for w in stroke.windows(2) {
        let (a, b) = if w[0].0 <= w[1].0 { (w[0], w[1]) } else { (w[1], w[0]) };
        let (i0, i1) = (idx(a.0), idx(b.0));
        for (i, s) in cycle.iter_mut().enumerate().take(i1 + 1).skip(i0) {
            let t = if i1 > i0 { (i - i0) as f32 / (i1 - i0) as f32 } else { 0. };
            *s = (a.1 + (b.1 - a.1) * t).clamp(-1., 1.);
        }
    }
}

/// Breakpoints of the editor's shape buttons (for `tablegen::draw`).
pub fn shape_points(name: &str) -> Vec<(f32, f32)> {
    match name {
        "Saw" => vec![(0., 1.), (0.999, -1.)],
        "Square" => vec![(0., 1.), (0.4999, 1.), (0.5, -1.), (0.9999, -1.)],
        "Triangle" => vec![(0., 0.), (0.25, 1.), (0.75, -1.)],
        _ => vec![(0., 0.)],
    }
}

/// Formulas the editor offers as starting points (`x` phase, `y` frame).
pub const FORMULAS: &[(&str, &str)] = &[
    ("Sine → Saw", "sin(2*pi*x)*(1-y) + saw(x)*y"),
    ("PWM", "sqr(x + 0.45*y*sin(2*pi*x))"),
    ("Fold", "sin(2*pi*x*(1+4*y))"),
    ("Sync", "sin(2*pi*fract(x*(1+7*y)))"),
    ("Harmonic", "sin(2*pi*x) + 0.5*y*sin(6*pi*x) + 0.33*y*y*sin(10*pi*x)"),
    ("Bitcrush", "floor(sin(2*pi*x)*(2+14*(1-y)))/(2+14*(1-y))"),
];

/// Frame counts the morph tools build.
pub const MORPH_COUNTS: [usize; 4] = [16, 32, 64, 256];

// ───────────────────────────── Stacked table view ─────────────────────────────

/// Where frame `i` of `n` sits in the stacked ("3D") table view of a plot
/// `w × h`: `(x offset, baseline y, width, amplitude)` in pixels. Frame 0 is
/// at the front-left bottom, the last frame at the back-right top.
pub fn stack_frame(w: f32, h: f32, i: usize, n: usize) -> (f32, f32, f32, f32) {
    stack_at(w, h, if n > 1 { i as f32 / (n - 1) as f32 } else { 0. })
}

/// [`stack_frame`] at a depth 0..1 (the playing position sits between frames).
pub fn stack_at(w: f32, h: f32, depth: f32) -> (f32, f32, f32, f32) {
    let depth = depth.clamp(0., 1.);
    let (dx, dy) = (w * 0.22, h * 0.42);
    let width = w - dx - 16.;
    let amp = h * 0.2;
    (8. + dx * depth, h - amp - 10. - dy * depth, width, amp)
}

/// Which kept frames the stacked view draws (at most `max`, evenly spread,
/// always the first and the last).
pub fn stack_indices(frames: usize, max: usize) -> Vec<usize> {
    if frames == 0 {
        return Vec::new();
    }
    let n = frames.min(max.max(2));
    if n == 1 {
        return vec![0];
    }
    let mut v: Vec<usize> = (0..n).map(|k| k * (frames - 1) / (n - 1)).collect();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::instrument::wavetable::{p, params, src};

    fn info(id: u32) -> ParamInfo {
        params().into_iter().find(|i| i.id == id).unwrap()
    }

    #[test]
    fn rings_cover_the_modulated_range() {
        assert_eq!(ring_span(0.5, 0.25, false), (0.5, 0.75));
        assert_eq!(ring_span(0.5, -0.25, false), (0.25, 0.5));
        assert_eq!(ring_span(0.5, 0.25, true), (0.25, 0.75));
        assert_eq!(ring_span(0.9, 0.5, false), (0.9, 1.), "clamped to the knob");
        assert_eq!(ring_span(0.1, -0.5, true), (0., 0.6));
    }

    #[test]
    fn dropping_a_source_on_a_knob_routes_it() {
        let slots = vec![ModSlot::default(); 32];
        // LFO 1 on the osc 1 position knob: slot 0, a quarter each way.
        let (s, edits) = route_drop(&slots, src::LFO1, &info(p::osc(0, p::POS))).unwrap();
        assert_eq!(s, 0);
        assert_eq!(edits[0], (p::slot_x(0, p::M_SRC), src::LFO1 as f32));
        assert_eq!(edits[1], (p::slot_x(0, p::M_DST), p::osc(0, p::POS) as f32));
        assert_eq!(edits[2], (p::slot_x(0, p::M_AMT), 25.));
        // An envelope goes in the next free slot at half depth.
        let mut slots = slots;
        slots[0] = ModSlot { source: src::LFO1, dest: p::osc(0, p::POS), amount: 0.25, ..Default::default() };
        let (s, edits) = route_drop(&slots, src::ENV2, &info(p::CUTOFF)).unwrap();
        assert_eq!((s, edits[2].1), (1, 50.));
        // The same pair again reuses its slot.
        assert_eq!(route_drop(&slots, src::LFO1, &info(p::osc(0, p::POS))).unwrap().0, 0);
        // Switches, choices and the matrix itself are no destinations.
        assert!(route_drop(&slots, src::ENV2, &info(p::osc(0, p::WARP))).is_none());
        assert!(route_drop(&slots, src::ENV2, &info(p::slot_x(3, p::M_AMT))).is_none());
        assert!(route_drop(&slots, 0, &info(p::CUTOFF)).is_none());
        // A full matrix takes nothing new.
        let full = vec![ModSlot { source: src::MACRO1, dest: p::VOLUME, amount: 0.1, ..Default::default() }; 32];
        assert!(route_drop(&full, src::ENV3, &info(p::CUTOFF)).is_none());
        // Clearing a slot writes the empty slot.
        assert_eq!(clear_slot(2)[0], (p::slot_x(2, p::M_SRC), 0.));
        assert!(slot_is_bipolar(&slots[0]) && !slot_is_bipolar(&ModSlot { source: src::ENV2, ..slots[0] }));
    }

    #[test]
    fn envelope_corners_follow_the_parameters() {
        let fast = EnvShape { sustain: 1., ..Default::default() };
        let c = fast.corners();
        assert_eq!(c[0], (0., ENV_BOTTOM));
        assert_eq!(c[1].1, ENV_TOP);
        assert!((c[3].1 - ENV_TOP).abs() < 1e-6, "full sustain stays at the top");
        let slow = EnvShape { attack: 1., hold: 1., decay: 1., sustain: 0.25, release: 1., curves: [0.; 3] };
        let s = slow.corners();
        for w in s.windows(2) {
            assert!(w[1].0 >= w[0].0, "time runs left to right");
        }
        assert!(s[5].0 <= 1.);
        assert!(s[1].0 > c[1].0 && s[2].0 - s[1].0 > 0.1, "hold is a plateau");
        assert!((s[3].1 - (ENV_BOTTOM - 0.8 * 0.25)).abs() < 1e-6, "sustain a quarter of the way up");
        // A linear segment's bend handle sits on the straight line; a fast
        // attack bends it up (smaller y), a fast decay bends it down.
        let lin = slow.bend_handle(0);
        assert!((lin.1 - (ENV_BOTTOM + ENV_TOP) / 2.).abs() < 1e-4);
        let bent = EnvShape { curves: [0.8, 0.8, 0.], ..slow };
        assert!(bent.bend_handle(0).1 < lin.1);
        assert!(bent.bend_handle(1).1 > slow.bend_handle(1).1);
        assert!(bend_per_frac(0) < 0. && bend_per_frac(1) > 0.);
        // The polyline passes through every corner.
        let line = slow.polyline(8);
        for corner in s {
            assert!(line.iter().any(|q| (q.0 - corner.0).abs() < 1e-5 && (q.1 - corner.1).abs() < 1e-5), "{corner:?}");
        }
    }

    #[test]
    fn lfo_points_map_both_ways() {
        for (x, y) in [(0., 0.), (0.3, 0.7), (1., 1.)] {
            let (fx, fy) = lfo_to_plot(x, y);
            let (bx, by) = plot_to_lfo(fx, fy);
            assert!((bx - x).abs() < 1e-6 && (by - y).abs() < 1e-6);
        }
        assert!(lfo_to_plot(0., 1.).1 < lfo_to_plot(0., 0.).1, "high values are drawn up");
        assert_eq!(plot_to_lfo(-1., -1.), (0., 1.));
        assert_eq!(snap(0.33, 4., true), 0.25);
        assert_eq!(snap(0.33, 4., false), 0.33);
    }

    #[test]
    fn lfo_points_are_added_moved_bent_and_deleted() {
        let pts = vec![CurvePoint { x: 0., y: 0., curve: 0. }, CurvePoint { x: 0.5, y: 1., curve: 0.2 }, CurvePoint { x: 1., y: 0., curve: 0. }];
        // Hit testing on a 400 × 100 plot.
        let (fx, fy) = lfo_to_plot(0.5, 1.);
        assert_eq!(lfo_hit(&pts, fx + 0.01, fy, 400., 100., 10.), LfoHit::Point(1));
        let mid = segment_mid(&pts, 0);
        let (mx, my) = lfo_to_plot(mid.0, mid.1);
        assert_eq!(lfo_hit(&pts, mx, my, 400., 100., 10.), LfoHit::Bend(0));
        assert_eq!(lfo_hit(&pts, 0.25, 0.95, 400., 100., 6.), LfoHit::Empty);
        // A point stays between its neighbours.
        let moved = lfo_move(&pts, 1, 2., 0.4);
        assert_eq!((moved[1].x, moved[1].y), (1., 0.4));
        let moved = lfo_move(&pts, 1, 0.2, 1.5);
        assert_eq!((moved[1].x, moved[1].y), (0.2, 1.));
        // Inserting keeps the order and the segment's curve.
        let (ins, at) = lfo_insert(&pts, 0.75, 0.5).unwrap();
        assert_eq!((at, ins.len()), (2, 4));
        assert_eq!(ins[2].curve, 0.2);
        assert!(ins.windows(2).all(|w| w[0].x <= w[1].x));
        // Deleting keeps two points at least.
        assert_eq!(lfo_delete(&ins, 2), pts);
        let two = lfo_delete(&pts, 0);
        assert_eq!(two.len(), 2);
        assert_eq!(lfo_delete(&two, 0).len(), 2);
        // Dragging a rising segment's handle up bends it to a fast start.
        let up = lfo_bend(&pts, 0, -0.1);
        assert!(up[0].curve > 0.);
        let down_on_falling = lfo_bend(&pts, 1, 0.1);
        assert!(down_on_falling[1].curve > pts[1].curve);
        assert!(lfo_bend(&pts, 0, -10.)[0].curve <= 1.);
        // A full curve takes no more points.
        let full = vec![CurvePoint::default(); modulation::MAX_POINTS];
        assert!(lfo_insert(&full, 0.5, 0.5).is_none());
    }

    #[test]
    fn harmonic_bars_and_strokes() {
        assert_eq!(bar_at(0., 32), 0);
        assert_eq!(bar_at(0.5, 32), 16);
        assert_eq!(bar_at(1., 32), 31);
        assert_eq!((bar_level(0.), bar_level(1.), bar_level(0.25)), (1., 0., 0.75));
        assert_eq!((wave_level(0.), wave_level(0.5), wave_level(1.)), (1., 0., -1.));
        // A stroke paints only the samples it crosses, either direction.
        let mut c = vec![0.; 100];
        paint_stroke(&mut c, &[(0.5, 1.), (0.2, -1.)]);
        assert_eq!(c[10], 0.);
        assert_eq!(c[20], -1.);
        assert_eq!(c[50], 1.);
        assert!((c[35] - 0.).abs() < 0.04, "a straight line between the ends");
        assert_eq!(c[80], 0.);
        paint_stroke(&mut c, &[(0.9, 2.)]);
        assert_eq!(c[90], 1., "clamped");
        // The shape buttons give valid breakpoints for tablegen::draw.
        for s in ["Saw", "Square", "Triangle", "Sine"] {
            let pts = shape_points(s);
            assert!(!pts.is_empty() && pts.iter().all(|(x, y)| (0. ..=1.).contains(x) && (-1. ..=1.).contains(y)));
        }
        let saw = aitrack_stock::instrument::wavetable::tablegen::draw(&shape_points("Saw"));
        assert!(saw[10] > 0.9 && saw[2000] < -0.9);
        // The formulas parse.
        for (_, f) in FORMULAS {
            assert!(aitrack_stock::instrument::wavetable::tablegen::Formula::parse(f).is_ok(), "{f}");
        }
    }

    #[test]
    fn the_stack_runs_front_left_to_back_right() {
        let (x0, y0, w0, a0) = stack_frame(400., 200., 0, 10);
        let (x9, y9, w9, a9) = stack_frame(400., 200., 9, 10);
        assert!(x9 > x0 && y9 < y0, "later frames sit further back");
        assert_eq!((w0, a0), (w9, a9));
        assert!(y0 + a0 <= 200. && y9 - a9 >= 0., "every frame fits");
        assert!(x9 + w9 <= 400.);
        assert_eq!(stack_indices(64, 16).len(), 16);
        let idx = stack_indices(64, 16);
        assert_eq!((idx[0], *idx.last().unwrap()), (0, 63));
        assert_eq!(stack_indices(3, 16), vec![0, 1, 2]);
        assert_eq!(stack_indices(1, 16), vec![0]);
        assert!(stack_indices(0, 16).is_empty());
    }
}
