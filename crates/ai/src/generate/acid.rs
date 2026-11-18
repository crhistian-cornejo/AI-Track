//! A generative acid-line (TB-303 style) phrase model.
//!
//! A phrase is a row of 16th-note steps. Each step is a rest or a note with an
//! accent, a slide (a glide into the next note) or a tie (the note is held
//! over from the step before, no new attack). The rules below are general
//! acid practice written as weights, not a copy of any recording or product:
//!
//! * The root owns the downbeats and the first step; the line leans on the
//!   minor colour tones (b3, 4, 5, b7) and only sometimes on b2 / b5 (tension).
//! * Notes repeat, and an octave jump (root to root an octave up) is the
//!   classic move; most other motion is by small intervals.
//! * Hits fall on the beat and the off-beat 8ths; the odd 16ths are ghosts.
//!   The gaps are what make the line percussive.
//! * Accents favour the off-beats and the note right after a rest, and they
//!   alternate on repeated notes.
//! * Slides glide into a different pitch (often an octave away), never from
//!   a rest, and never into a rest.
//! * The phrase is built from 8-step segments: the second answers the first
//!   (a varied copy that ends differently), later ones either restate the
//!   motif or are new, depending on the Repetition control.
//!
//! Pure functions of (seed, options): no I/O, no clock, no globals.

use super::rng::Rng;
use aitrack_model::{Key, PPQ, Scale, ScaleKind, Tick};

/// The lowest and highest note offsets the Acid 303 steps hold (semitones from its C2).
pub const LOW: i32 = -12;
pub const HIGH: i32 = 24;
/// The MIDI key of offset 0 (C2).
pub const BASE_KEY: u8 = 36;
/// One 16th note.
pub const STEP_TICKS: Tick = PPQ / 4;
/// Velocity of an accented and of a plain note in a rendered clip.
pub const ACCENT_VELOCITY: f32 = 0.9;
pub const PLAIN_VELOCITY: f32 = 0.6;
const SEG: usize = 8;
/// How far a gliding note overlaps the next one in a rendered clip.
const OVERLAP: Tick = STEP_TICKS / 4;
/// A plain note's length: a bit over half a step, like the 303's gate.
const GATE_TICKS: Tick = STEP_TICKS * 3 / 5;

/// One step of the phrase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// Semitones from the device's C2 (the key's root sits within -5..=6 of it).
    pub note: i32,
    /// Sounding (a note or a tie); false is a rest.
    pub gate: bool,
    /// Held over from the step before: same pitch, no new attack.
    pub tie: bool,
    pub accent: bool,
    /// Glides into the next step's note.
    pub slide: bool,
}

impl Step {
    fn rest(note: i32) -> Self {
        Self { note, gate: false, tie: false, accent: false, slide: false }
    }

    fn hit(note: i32) -> Self {
        Self { note, gate: true, tie: false, accent: false, slide: false }
    }
}

/// A step as the Acid 303's pattern parameters hold it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternStep {
    pub note: i32,
    pub gate: bool,
    pub accent: bool,
    pub slide: bool,
}

/// A note of the phrase as a MIDI clip holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcidNote {
    pub key: u8,
    pub start: Tick,
    pub length: Tick,
    pub velocity: f32,
}

/// The controls of `generate`; each 0..=1 unless said otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opts {
    /// 8, 16, 32 or 64 (anything else is rounded to one of those).
    pub steps: usize,
    pub scale: Scale,
    /// How many steps sound.
    pub density: f32,
    pub accent: f32,
    pub slide: f32,
    /// 0: one octave; higher: octave jumps and a wider range.
    pub octaves: f32,
    /// Chromatic colour: b2 and b5 (also when the scale lacks them).
    pub tension: f32,
    /// Motif reuse against variety.
    pub repetition: f32,
    pub root_emphasis: f32,
}

impl Opts {
    pub fn new(scale: Scale) -> Self {
        Self { steps: 16, scale, density: 0.6, accent: 0.5, slide: 0.35, octaves: 0.4, tension: 0.15, repetition: 0.55, root_emphasis: 0.7 }
    }

    /// The minor-scale default when the project has no key.
    pub fn minor(root: Key) -> Self {
        Self::new(Scale::new(root, ScaleKind::Minor))
    }

    pub fn clamped(mut self) -> Self {
        let u = |v: f32| if v.is_finite() { v.clamp(0., 1.) } else { 0.5 };
        self.steps = match self.steps {
            0..=11 => 8,
            12..=23 => 16,
            24..=47 => 32,
            _ => 64,
        };
        self.density = u(self.density);
        self.accent = u(self.accent);
        self.slide = u(self.slide);
        self.octaves = u(self.octaves);
        self.tension = u(self.tension);
        self.repetition = u(self.repetition);
        self.root_emphasis = u(self.root_emphasis);
        self
    }
}

/// A finished phrase and the scale it was written in.
#[derive(Clone, Debug, PartialEq)]
pub struct Phrase {
    pub steps: Vec<Step>,
    pub scale: Scale,
}

/// The offset of the key's root nearest C2: -5..=6 (A is -3, so it plays A1).
pub fn root_offset(scale: Scale) -> i32 {
    let r = scale.root.0 as i32 % 12;
    if r <= 6 { r } else { r - 12 }
}

fn fold(mut n: i32) -> i32 {
    while n < LOW {
        n += 12;
    }
    while n > HIGH {
        n -= 12;
    }
    n
}

fn class_of(n: i32, root_off: i32) -> i32 {
    (n - root_off).rem_euclid(12)
}

/// Move `n` by `deg` scale degrees (a note off the scale keeps its distance above the degree below it).
fn degree_shift(n: i32, scale: Scale, deg: i32) -> i32 {
    let root_off = root_offset(scale);
    let iv = scale.kind.intervals();
    let len = iv.len() as i32;
    let rel = n - root_off;
    let (oct, pc) = (rel.div_euclid(12), rel.rem_euclid(12));
    let idx = iv.iter().rposition(|&x| x as i32 <= pc).unwrap_or(0);
    let extra = pc - iv[idx] as i32;
    let t = idx as i32 + deg;
    fold(root_off + (oct + t.div_euclid(len)) * 12 + iv[t.rem_euclid(len) as usize] as i32 + extra)
}

/// How much each pitch class (semitones above the root) is used in an acid line.
const CLASS_W: [f32; 12] = [10., 1.2, 1.5, 3.4, 2.0, 2.8, 1.6, 3.2, 1.8, 1.4, 3.4, 0.8];
/// Weight of each 16th in an 8-step segment: beat, ghost, off-beat 8th, ghost, ...
const POS_W: [f32; SEG] = [1.0, 0.42, 0.72, 0.5, 0.95, 0.42, 0.7, 0.5];

fn weighted(rng: &mut Rng, w: &[f32]) -> usize {
    let total: f32 = w.iter().sum();
    if total <= 0. {
        return 0;
    }
    let mut x = rng.unit() * total;
    for (i, v) in w.iter().enumerate() {
        if x < *v {
            return i;
        }
        x -= v;
    }
    w.len() - 1
}

/// Where the walk through the phrase stands.
struct Walk {
    note: i32,
    class: i32,
    oct: i32,
    gated: bool,
}

fn pick_class(rng: &mut Rng, o: &Opts, walk: &Walk, oct: i32, after_rest: bool, down: bool) -> i32 {
    let root_off = root_offset(o.scale);
    let mask = o.scale.mask();
    let in_scale = |c: i32| mask & (1 << ((o.scale.root.0 as i32 + c).rem_euclid(12))) != 0;
    let mut w = [0f32; 12];
    for (c, slot) in w.iter_mut().enumerate() {
        let c = c as i32;
        let mut v = CLASS_W[c as usize];
        if c == 0 {
            v *= 0.35 + 1.65 * o.root_emphasis;
            if down {
                v *= 1. + 1.2 * o.root_emphasis;
            }
            if after_rest {
                v *= 1.5;
            }
        }
        if c == 1 || c == 6 {
            v = if in_scale(c) { v * (0.3 + o.tension) } else { o.tension * if c == 1 { 3.0 } else { 2.4 } };
        } else if !in_scale(c) {
            v = 0.;
        }
        // Small steps are the norm.
        let dist = (fold(root_off + c + 12 * oct) - walk.note).abs() as f32;
        *slot = v * (0.35 + 0.65 * (-dist / 7.).exp());
    }
    weighted(rng, &w) as i32
}

fn choose_pitch(rng: &mut Rng, o: &Opts, walk: &Walk, after_rest: bool, down: bool, phrase_start: bool) -> (i32, i32) {
    let re = o.root_emphasis;
    if phrase_start && (re >= 0.9 || rng.chance(0.3 + 0.7 * re)) {
        return (0, 0);
    }
    if rng.chance(0.10 + 0.30 * o.repetition) {
        return (walk.class, walk.oct);
    }
    let mut oct = walk.oct;
    if o.octaves > 0.1 {
        if rng.chance(0.03 + 0.42 * o.octaves) {
            oct = if walk.oct == 0 { if o.octaves > 0.5 && rng.chance(0.3) { -1 } else { 1 } } else { 0 };
            // The classic move: the same note an octave away.
            if rng.chance(0.6) {
                return (walk.class, oct);
            }
        } else if walk.oct != 0 && rng.chance(0.3) {
            oct = 0;
        }
    } else {
        oct = 0;
    }
    (pick_class(rng, o, walk, oct, after_rest, down), oct)
}

/// Tie, accent and slide of a freshly written segment. `prev_gated` is the step before it.
fn articulate(rng: &mut Rng, o: &Opts, steps: &mut [Step; SEG], prev_gated: bool) {
    for i in 1..SEG {
        if steps[i].gate && steps[i - 1].gate && steps[i].note == steps[i - 1].note && rng.chance(0.22 + 0.2 * (1. - o.density)) {
            steps[i].tie = true;
        }
    }
    let mut prev_acc = false;
    for i in 0..SEG {
        let s = steps[i];
        if !s.gate || s.tie {
            prev_acc = false;
            continue;
        }
        let after_rest = if i == 0 { !prev_gated } else { !steps[i - 1].gate };
        let mut m = match i % 4 {
            2 => 1.35,
            0 => 0.75,
            _ => 1.1,
        };
        if after_rest {
            m *= 1.5;
        }
        if s.note > 11 {
            m *= 1.25;
        }
        if i > 0 && steps[i - 1].gate && steps[i - 1].note == s.note {
            m *= if prev_acc { 0.3 } else { 1.4 };
        }
        let acc = rng.chance((o.accent * 0.62 * m).min(0.95));
        steps[i].accent = acc;
        prev_acc = acc;
    }
    let mut prev_slid = false;
    for i in 0..SEG - 1 {
        let (a, b) = (steps[i], steps[i + 1]);
        if !a.gate || !b.gate || b.tie || a.note == b.note {
            prev_slid = false;
            continue;
        }
        let d = (a.note - b.note).abs();
        let mut m = if d == 12 { 1.4 } else if d > 7 { 0.8 } else { 1.0 };
        if prev_slid {
            m *= 0.6;
        }
        let slide = rng.chance((o.slide * 0.8 * m).min(0.95));
        steps[i].slide = slide;
        prev_slid = slide;
    }
}

fn fresh_segment(rng: &mut Rng, o: &Opts, first: bool, walk: &mut Walk) -> [Step; SEG] {
    let d = o.density;
    let mut k = (SEG as f32 * (0.22 + 0.68 * d)).round() as i32;
    if rng.chance(0.25) {
        k -= 1;
    } else if rng.chance(0.25) {
        k += 1;
    }
    let k = k.clamp(1, SEG as i32) as usize;
    // Pick k hits by weighted draw without replacement.
    let mut keyed: Vec<(f32, usize)> = (0..SEG)
        .map(|i| {
            let w = POS_W[i] * if first && i == 0 { 1.8 } else { 1.0 } * (0.7 + 0.6 * rng.unit());
            (rng.unit().max(1e-6).powf(1. / w), i)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut gate = [false; SEG];
    for (_, i) in keyed.iter().take(k) {
        gate[*i] = true;
    }
    if first && !gate[0] && (o.root_emphasis >= 0.9 || rng.chance(0.25 + 0.9 * o.root_emphasis)) {
        gate[0] = true;
        if let Some((_, weakest)) = keyed.iter().take(k).rev().find(|(_, i)| *i != 0) {
            gate[*weakest] = false;
        }
    }
    // No long gaps: a groove that stops for half a bar is not a groove.
    for _ in 0..4 {
        let (mut best, mut i) = ((0usize, 0usize), 0usize);
        while i < SEG {
            if gate[i] {
                i += 1;
                continue;
            }
            let from = i;
            while i < SEG && !gate[i] {
                i += 1;
            }
            if i - from > best.0 {
                best = (i - from, from);
            }
        }
        if best.0 <= 4 {
            break;
        }
        if let Some(j) = (best.1..best.1 + best.0).max_by(|a, b| POS_W[*a].total_cmp(&POS_W[*b])) {
            gate[j] = true;
        }
    }

    let root_off = root_offset(o.scale);
    let mut steps = [Step::rest(root_off); SEG];
    let gated_before = walk.gated;
    let mut prev_gated = gated_before;
    for i in 0..SEG {
        if !gate[i] {
            prev_gated = false;
            continue;
        }
        let (class, oct) = choose_pitch(rng, o, walk, !prev_gated, i % 4 == 0, first && i == 0);
        let n = fold(root_off + class + 12 * oct);
        steps[i] = Step::hit(n);
        *walk = Walk { note: n, class, oct, gated: true };
        prev_gated = true;
    }
    articulate(rng, o, &mut steps, gated_before);
    walk.gated = gate[SEG - 1];
    steps
}

fn gated_notes(steps: &[Step]) -> impl Iterator<Item = usize> + '_ {
    steps.iter().enumerate().filter(|(_, s)| s.gate).map(|(i, _)| i)
}

/// Make the segment end on `class` (an open ending for the first half, a resolution for the last).
fn cadence(seg: &mut [Step], scale: Scale, class: i32) {
    let root_off = root_offset(scale);
    let Some(i) = gated_notes(seg).last() else { return };
    let prev = i.checked_sub(1).map(|p| seg[p]);
    let keep_oct = if seg[i].note - root_off >= 12 { 1 } else { 0 };
    let n = fold(root_off + class + 12 * keep_oct);
    seg[i].note = n;
    if seg[i].tie && prev.is_none_or(|p| p.note != n) {
        seg[i].tie = false;
    }
}

/// The class nearest the wanted ones that the scale holds, in order of preference.
fn pick_in_scale(scale: Scale, want: &[i32]) -> i32 {
    let mask = scale.mask();
    want.iter().copied().find(|c| mask & (1 << ((scale.root.0 as i32 + c).rem_euclid(12))) != 0).unwrap_or(0)
}

/// What a variation may touch.
#[derive(Clone, Copy)]
struct Allow {
    /// Octave moves and degree moves across the octave.
    wide: bool,
    accent: bool,
    slide: bool,
    /// Step 1 keeps its gate.
    keep_gate0: bool,
}

/// Change step `i` of `steps` in one small way; tries other moves when one does not apply.
fn vary_at(rng: &mut Rng, steps: &mut [Step], i: usize, scale: Scale, allow: Allow) {
    let n = steps.len();
    let root_off = root_offset(scale);
    let before = steps[i];
    for _ in 0..8 {
        let w = [35., if allow.accent { 25. } else { 0. }, if allow.slide { 15. } else { 0. }, if allow.wide { 10. } else { 0. }, 15.];
        match weighted(rng, &w) {
            0 if steps[i].gate && !steps[i].tie => {
                let deg = if rng.chance(0.5) { 1 } else { -1 } * if rng.chance(0.25) { 2 } else { 1 };
                let mut moved = degree_shift(steps[i].note, scale, deg);
                if !allow.wide {
                    // One octave: a step past the top or bottom of it wraps round.
                    if moved < root_off {
                        moved += 12;
                    } else if moved > root_off + 11 {
                        moved -= 12;
                    }
                }
                steps[i].note = fold(moved);
            }
            1 if steps[i].gate && !steps[i].tie => steps[i].accent = !steps[i].accent,
            2 if steps[i].gate && steps[(i + 1) % n].gate && steps[(i + 1) % n].note != steps[i].note => steps[i].slide = !steps[i].slide,
            3 if steps[i].gate && !steps[i].tie => {
                let up = steps[i].note - root_off < 12 || rng.chance(0.3);
                steps[i].note = fold(steps[i].note + if up { 12 } else { -12 });
            }
            4 if !(i == 0 && allow.keep_gate0) => {
                if steps[i].gate {
                    steps[i].gate = false;
                } else {
                    // A new hit borrows the pitch of a neighbour, or the root.
                    let near = if i > 0 && steps[i - 1].gate { steps[i - 1].note } else if steps[(i + 1) % n].gate { steps[(i + 1) % n].note } else { root_off };
                    steps[i] = Step::hit(near);
                }
            }
            _ => {}
        }
        if steps[i] != before {
            return;
        }
    }
}

/// Break up any gap longer than `max` steps (the phrase loops) with a hit that borrows a neighbour's pitch.
fn fill_gaps(steps: &mut [Step], root_off: i32, max: usize) {
    let n = steps.len();
    if n == 0 || steps.iter().all(|s| !s.gate) {
        return;
    }
    for _ in 0..n {
        let start = (0..n).find(|&i| steps[i].gate).unwrap_or(0);
        let (mut best, mut run_start, mut run) = ((0usize, 0usize), 0usize, 0usize);
        for k in 1..=n {
            let i = (start + k) % n;
            if steps[i].gate {
                if run > best.0 {
                    best = (run, run_start);
                }
                run = 0;
            } else {
                if run == 0 {
                    run_start = i;
                }
                run += 1;
            }
        }
        if best.0 <= max {
            return;
        }
        let pick = (0..best.0).map(|k| (best.1 + k) % n).max_by(|a, b| POS_W[*a % SEG].total_cmp(&POS_W[*b % SEG])).unwrap_or(best.1);
        let near = (1..=n).map(|d| steps[(pick + n - d) % n]).find(|s| s.gate).map_or(root_off, |s| s.note);
        steps[pick] = Step::hit(near);
    }
}

/// Restore what every phrase must hold: only sounding steps carry accent, slide or tie; a tie repeats the
/// pitch before it; a slide goes into a sounding, freshly attacked next step.
fn repair(steps: &mut [Step], root_off: i32) {
    let n = steps.len();
    if n == 0 {
        return;
    }
    for i in 0..n {
        let prev = steps[(i + n - 1) % n];
        let s = &mut steps[i];
        if !s.gate {
            *s = Step::rest(if s.note < LOW || s.note > HIGH { root_off } else { s.note });
            continue;
        }
        s.note = s.note.clamp(LOW, HIGH);
        if s.tie && (n < 2 || !prev.gate || prev.note != s.note) {
            s.tie = false;
        }
        if s.tie {
            s.accent = false;
        }
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let next = steps[j];
        if steps[i].slide && (!steps[i].gate || !next.gate || next.tie || n < 2) {
            steps[i].slide = false;
        } else if steps[i].slide && next.note == steps[i].note {
            // A glide into the same pitch is a tie to the synth; with an accent on it, no glide at all.
            steps[i].slide = false;
            if !next.accent && j != 0 {
                steps[j].tie = true;
            }
        }
    }
}

/// A phrase from a seed.
pub fn generate(seed: u64, opts: &Opts) -> Phrase {
    let o = opts.clamped();
    let mut rng = Rng::new(seed ^ 0x0AC1_D303);
    let root_off = root_offset(o.scale);
    let segs = o.steps / SEG;
    let mut walk = Walk { note: root_off, class: 0, oct: 0, gated: false };
    let mut out: Vec<[Step; SEG]> = Vec::with_capacity(segs);
    for k in 0..segs {
        let copy_from = if k == 0 {
            None
        } else if k % 2 == 1 {
            // The answer: a varied restatement of the segment before it.
            rng.chance(0.75 + 0.2 * o.repetition).then_some(k - 1)
        } else {
            rng.chance(0.35 + 0.6 * o.repetition).then_some(0)
        };
        let seg = match copy_from {
            Some(src) => {
                let mut seg = out[src];
                let amount = if k % 2 == 1 { 0.15 + 0.25 * (1. - o.repetition) } else { 0.08 + 0.2 * (1. - o.repetition) };
                for i in 0..SEG {
                    if rng.chance(amount) {
                        vary_at(&mut rng, &mut seg, i, o.scale, Allow { wide: o.octaves > 0.1, accent: o.accent > 0., slide: o.slide > 0., keep_gate0: false });
                    }
                }
                if let Some(l) = gated_notes(&seg).last() {
                    walk = Walk { note: seg[l].note, class: class_of(seg[l].note, root_off), oct: i32::from(seg[l].note - root_off >= 12), gated: seg[SEG - 1].gate };
                }
                seg
            }
            None => fresh_segment(&mut rng, &o, k == 0, &mut walk),
        };
        out.push(seg);
    }
    // A question and its answer: the first half ends open, the last one resolves home.
    if segs >= 2 {
        if rng.chance(0.6) {
            let c = pick_in_scale(o.scale, if rng.chance(0.5) { &[7, 10, 5, 3] } else { &[10, 7, 3, 5] });
            cadence(&mut out[segs / 2 - 1], o.scale, c);
        }
        if rng.chance(0.4 + 0.5 * o.root_emphasis) {
            cadence(&mut out[segs - 1], o.scale, 0);
        }
    }
    let mut steps: Vec<Step> = out.into_iter().flatten().collect();
    // The last step may glide back to the top of the loop.
    let n = steps.len();
    if steps[n - 1].gate && steps[0].gate && steps[n - 1].note != steps[0].note && rng.chance(o.slide * 0.3) {
        steps[n - 1].slide = true;
    }
    fill_gaps(&mut steps, root_off, 6);
    repair(&mut steps, root_off);
    Phrase { steps, scale: o.scale }
}

impl Phrase {
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Steps that sound (notes and ties).
    pub fn gated(&self) -> usize {
        self.steps.iter().filter(|s| s.gate).count()
    }

    pub fn accents(&self) -> usize {
        self.steps.iter().filter(|s| s.accent).count()
    }

    pub fn slides(&self) -> usize {
        self.steps.iter().filter(|s| s.slide).count()
    }

    /// The clip length that holds the phrase.
    pub fn ticks(&self) -> Tick {
        self.steps.len() as Tick * STEP_TICKS
    }

    /// Change a few steps and keep the motif: about `amount * 40 %` of the steps move by one small
    /// move each (a neighbouring degree, an accent, a slide, an octave, a hit added or dropped).
    /// Step 1 keeps its gate so the phrase still starts on the beat.
    pub fn mutate(&self, seed: u64, amount: f32) -> Phrase {
        let amount = if amount.is_finite() { amount.clamp(0., 1.) } else { 0.3 };
        let n = self.steps.len();
        let mut out = self.clone();
        if n == 0 || amount <= 0. {
            return out;
        }
        let mut rng = Rng::new(seed ^ 0x3D17_AC1D);
        let count = ((amount * n as f32 * 0.4).round() as usize).clamp(1, n);
        let mut idx: Vec<usize> = (0..n).collect();
        for k in 0..count {
            let j = k + rng.below(n - k);
            idx.swap(k, j);
            vary_at(&mut rng, &mut out.steps, idx[k], self.scale, Allow { wide: true, accent: true, slide: true, keep_gate0: true });
        }
        fill_gaps(&mut out.steps, root_offset(self.scale), 6);
        repair(&mut out.steps, root_offset(self.scale));
        out
    }

    /// Cycle the phrase: positive moves it later, the end wraps to the start.
    pub fn rotate(&self, by: i32) -> Phrase {
        let n = self.steps.len();
        let mut out = self.clone();
        if n > 0 {
            out.steps.rotate_right(by.rem_euclid(n as i32) as usize);
        }
        out
    }

    /// Move the phrase later by `by` steps (earlier if negative), leaving rests behind and dropping what falls off.
    pub fn shift(&self, by: i32) -> Phrase {
        let n = self.steps.len() as i32;
        let root_off = root_offset(self.scale);
        let mut out = self.clone();
        for (i, slot) in out.steps.iter_mut().enumerate() {
            let src = i as i32 - by;
            *slot = if (0..n).contains(&src) { self.steps[src as usize] } else { Step::rest(root_off) };
        }
        repair(&mut out.steps, root_off);
        out
    }

    /// Move every note by `degrees` steps of the scale (up if positive).
    pub fn transpose(&self, degrees: i32) -> Phrase {
        let mut out = self.clone();
        for s in out.steps.iter_mut() {
            s.note = degree_shift(s.note, self.scale, degrees);
        }
        repair(&mut out.steps, root_offset(self.scale));
        out
    }

    /// Play it backwards. A glide still lands on the note it came from.
    pub fn reverse(&self) -> Phrase {
        let n = self.steps.len();
        let mut out = self.clone();
        out.steps.reverse();
        for s in &mut out.steps {
            s.slide = false;
            s.tie = false;
        }
        for (i, s) in self.steps.iter().enumerate() {
            if s.tie && n > 1 {
                // old (i-1, i) was the pair; reversed it is (n-1-i, n-i), the second one tied.
                let first = n - 1 - i;
                out.steps[first].slide = false;
                out.steps[(first + 1) % n].tie = true;
            } else if s.slide && i + 1 < n {
                out.steps[n - 2 - i].slide = true;
            }
        }
        repair(&mut out.steps, root_offset(self.scale));
        out
    }

    /// The steps as the Acid 303 holds them: a tie is the step before gliding into the same note.
    pub fn pattern_steps(&self) -> Vec<PatternStep> {
        let n = self.steps.len();
        (0..n)
            .map(|i| {
                let s = self.steps[i];
                let next_tied = n > 1 && self.steps[(i + 1) % n].tie;
                PatternStep { note: s.note, gate: s.gate, accent: s.accent, slide: s.slide || next_tied }
            })
            .collect()
    }

    /// A phrase from the Acid 303's own steps (the reverse of `pattern_steps`).
    pub fn from_pattern(steps: &[PatternStep], scale: Scale) -> Phrase {
        let n = steps.len();
        let mut out: Vec<Step> = steps.iter().map(|p| Step { note: p.note, gate: p.gate, tie: false, accent: p.accent && p.gate, slide: p.slide && p.gate }).collect();
        for i in 0..n {
            let prev = steps[(i + n - 1) % n];
            let s = steps[i];
            if n > 1 && s.gate && !s.accent && prev.gate && prev.slide && prev.note == s.note {
                out[i].tie = true;
                out[(i + n - 1) % n].slide = false;
            }
        }
        repair(&mut out, root_offset(scale));
        Phrase { steps: out, scale }
    }

    /// The phrase as clip notes from tick 0: `base_key` is the key of offset 0 (36 = C2). A glide
    /// overlaps the next note (the synth slides on overlap), a tie lengthens the note before it, an
    /// accent is a hard hit.
    pub fn notes(&self, base_key: u8) -> Vec<AcidNote> {
        let n = self.steps.len();
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            let s = self.steps[i];
            if !s.gate || (s.tie && i > 0) {
                i += 1;
                continue;
            }
            let mut last = i;
            while last + 1 < n && self.steps[last + 1].tie {
                last += 1;
            }
            let end = self.steps[last];
            let glides = end.slide && last + 1 < n && self.steps[last + 1].gate;
            let length = (last - i) as Tick * STEP_TICKS + if glides { STEP_TICKS + OVERLAP } else { GATE_TICKS };
            out.push(AcidNote { key: (base_key as i32 + s.note).clamp(0, 127) as u8, start: i as Tick * STEP_TICKS, length, velocity: if s.accent { ACCENT_VELOCITY } else { PLAIN_VELOCITY } });
            i = last + 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_minor() -> Opts {
        Opts::new(Scale::new(Key(9), ScaleKind::Minor))
    }

    fn valid(p: &Phrase) {
        let n = p.steps.len();
        for (i, s) in p.steps.iter().enumerate() {
            assert!((LOW..=HIGH).contains(&s.note), "note {} out of range", s.note);
            if !s.gate {
                assert!(!s.accent && !s.slide && !s.tie, "rest {i} carries articulation");
            }
            if s.tie {
                let prev = p.steps[(i + n - 1) % n];
                assert!(prev.gate && prev.note == s.note && !s.accent, "bad tie at {i}");
            }
            if s.slide {
                let next = p.steps[(i + 1) % n];
                assert!(s.gate && next.gate && !next.tie, "slide at {i} out of or into a rest/tie");
            }
        }
    }

    fn mean(f: impl Fn(u64) -> f32, n: u64) -> f32 {
        (0..n).map(|s| f(s * 7919 + 13)).sum::<f32>() / n as f32
    }

    #[test]
    fn the_same_seed_gives_the_same_phrase_and_other_seeds_give_others() {
        let o = a_minor();
        assert_eq!(generate(42, &o), generate(42, &o));
        let distinct: std::collections::HashSet<Vec<(i32, bool, bool, bool)>> = (0..40).map(|s| generate(s, &o).steps.iter().map(|x| (x.note, x.gate, x.accent, x.slide)).collect()).collect();
        assert!(distinct.len() >= 35, "only {} distinct phrases in 40 seeds", distinct.len());
    }

    #[test]
    fn lengths_16_32_and_64_and_the_rules_hold_in_all() {
        for (asked, got) in [(16, 16), (32, 32), (64, 64), (10, 8), (100, 64)] {
            for seed in 0..30 {
                let p = generate(seed, &Opts { steps: asked, ..a_minor() });
                assert_eq!(p.len(), got);
                valid(&p);
            }
        }
    }

    #[test]
    fn without_tension_every_note_is_in_the_scale() {
        for kind in [ScaleKind::Minor, ScaleKind::Dorian, ScaleKind::Phrygian, ScaleKind::MinorPentatonic, ScaleKind::Major, ScaleKind::Blues, ScaleKind::HarmonicMinor] {
            for root in 0..12u8 {
                let scale = Scale::new(Key(root), kind);
                for seed in 0..12 {
                    let p = generate(seed, &Opts { tension: 0., octaves: 0.9, ..Opts::new(scale) });
                    for s in p.steps.iter().filter(|s| s.gate) {
                        let key = (BASE_KEY as i32 + s.note) as u8;
                        assert!(scale.contains(key), "{kind:?} root {root}: note {} off the scale", s.note);
                    }
                }
            }
        }
    }

    #[test]
    fn tension_brings_the_flat_second_and_flat_fifth() {
        let scale = Scale::new(Key(9), ScaleKind::Minor);
        let off = |tension: f32| -> usize {
            (0..60).map(|s| generate(s, &Opts { tension, ..Opts::new(scale) })).flat_map(|p| p.steps).filter(|s| s.gate && !scale.contains((BASE_KEY as i32 + s.note) as u8)).count()
        };
        assert_eq!(off(0.), 0);
        assert!(off(1.) > 20, "tension 1 should colour the line, got {}", off(1.));
        assert!(off(0.1) < off(1.));
    }

    #[test]
    fn density_sets_how_many_steps_sound() {
        let g = |d: f32| mean(|s| generate(s, &Opts { density: d, ..a_minor() }).gated() as f32, 120);
        let (lo, hi) = (g(0.2), g(0.8));
        assert!(lo + 3. < hi, "density .2 gives {lo}, .8 gives {hi}");
        assert!(lo < 8. && hi > 8., "{lo} {hi}");
        assert!(g(0.0) < g(0.5) && g(0.5) < g(1.0));
    }

    #[test]
    fn accent_and_slide_amounts_follow_their_controls_on_average() {
        let acc = |a: f32| mean(|s| generate(s, &Opts { accent: a, ..a_minor() }).accents() as f32, 120);
        assert_eq!(acc(0.), 0.);
        assert!(acc(0.2) < acc(0.5) && acc(0.5) < acc(1.0), "{} {} {}", acc(0.2), acc(0.5), acc(1.0));
        let sl = |a: f32| mean(|s| generate(s, &Opts { slide: a, ..a_minor() }).slides() as f32, 120);
        assert_eq!(sl(0.), 0.);
        assert!(sl(0.2) < sl(0.5) && sl(0.5) < sl(1.0), "{} {} {}", sl(0.2), sl(0.5), sl(1.0));
    }

    #[test]
    fn a_high_root_emphasis_starts_on_the_root() {
        for root in [0u8, 3, 7, 9, 11] {
            let scale = Scale::new(Key(root), ScaleKind::Minor);
            for seed in 0..80 {
                let p = generate(seed, &Opts { root_emphasis: 1., ..Opts::new(scale) });
                let s = p.steps[0];
                assert!(s.gate && class_of(s.note, root_offset(scale)) == 0, "seed {seed} root {root}: step 1 is {s:?}");
            }
        }
    }

    #[test]
    fn the_root_dominates_and_owns_the_downbeats() {
        let scale = Scale::new(Key(9), ScaleKind::Minor);
        let root_share = |re: f32, down: bool| -> f32 {
            let (mut hit, mut all) = (0., 0.);
            for s in 0..150 {
                let p = generate(s, &Opts { root_emphasis: re, ..Opts::new(scale) });
                for st in p.steps.iter().enumerate().filter(|(i, st)| st.gate && (i % 4 == 0) == down).map(|(_, st)| st) {
                    all += 1.;
                    if class_of(st.note, root_offset(scale)) == 0 {
                        hit += 1.;
                    }
                }
            }
            hit / all
        };
        assert!(root_share(0.9, true) > root_share(0.1, true) + 0.15);
        assert!(root_share(0.7, true) > root_share(0.7, false), "downbeats {} others {}", root_share(0.7, true), root_share(0.7, false));
        assert!(root_share(0.7, false) > 0.25, "the root is the most used note");
    }

    #[test]
    fn slides_never_leave_a_rest_and_mostly_change_pitch() {
        let (mut slides, mut changing) = (0, 0);
        for seed in 0..200 {
            let p = generate(seed, &Opts { slide: 1., ..a_minor() });
            valid(&p);
            for (i, s) in p.steps.iter().enumerate().filter(|(_, s)| s.slide) {
                slides += 1;
                if p.steps[(i + 1) % p.len()].note != s.note {
                    changing += 1;
                }
            }
        }
        assert!(slides > 200, "{slides}");
        assert!(changing as f32 / slides as f32 > 0.97, "{changing} of {slides}");
    }

    #[test]
    fn accents_prefer_the_offbeat_and_the_note_after_a_rest() {
        let (mut on, mut off, mut after_rest, mut other) = ([0f32; 2], [0f32; 2], [0f32; 2], [0f32; 2]);
        for seed in 0..400 {
            let p = generate(seed, &Opts { accent: 0.6, density: 0.6, ..a_minor() });
            for (i, s) in p.steps.iter().enumerate().filter(|(_, s)| s.gate && !s.tie) {
                let prev_rest = i > 0 && !p.steps[i - 1].gate;
                let bucket = if i % 4 == 2 { &mut off } else if i % 4 == 0 { &mut on } else { continue };
                bucket[0] += f32::from(s.accent);
                bucket[1] += 1.;
                let b2 = if prev_rest { &mut after_rest } else { &mut other };
                b2[0] += f32::from(s.accent);
                b2[1] += 1.;
            }
        }
        assert!(off[0] / off[1] > on[0] / on[1] + 0.1, "offbeat {} downbeat {}", off[0] / off[1], on[0] / on[1]);
        assert!(after_rest[0] / after_rest[1] > other[0] / other[1] + 0.05);
    }

    #[test]
    fn repetition_makes_the_second_half_answer_the_first() {
        let same = |rep: f32| -> f32 {
            mean(
                |s| {
                    let p = generate(s, &Opts { repetition: rep, ..a_minor() });
                    let (a, b) = p.steps.split_at(8);
                    a.iter().zip(b).filter(|(x, y)| x.gate == y.gate && (!x.gate || x.note == y.note)).count() as f32 / 8.
                },
                150,
            )
        };
        assert!(same(1.) > same(0.) + 0.15, "rep 1 {} rep 0 {}", same(1.), same(0.));
        assert!(same(0.55) > 0.55, "halves of a default phrase rhyme: {}", same(0.55));
    }

    #[test]
    fn octave_jumps_follow_the_octave_control() {
        let jumps = |o: f32| -> f32 {
            mean(
                |s| {
                    let p = generate(s, &Opts { octaves: o, ..a_minor() });
                    let notes: Vec<i32> = p.steps.iter().filter(|s| s.gate).map(|s| s.note).collect();
                    notes.windows(2).filter(|w| (w[0] - w[1]).abs() == 12).count() as f32
                },
                120,
            )
        };
        assert_eq!(jumps(0.), 0.);
        assert!(jumps(0.9) > jumps(0.3), "{} {}", jumps(0.9), jumps(0.3));
        // At zero the whole line sits within one octave of the root.
        for s in 0..50 {
            let p = generate(s, &Opts { octaves: 0., tension: 0., ..a_minor() });
            let (lo, hi) = p.steps.iter().filter(|s| s.gate).fold((99, -99), |(lo, hi), s| (lo.min(s.note), hi.max(s.note)));
            assert!(hi - lo <= 12, "range {lo}..{hi}");
        }
    }

    #[test]
    fn rests_make_the_groove_percussive() {
        let p = mean(|s| (generate(s, &a_minor()).len() - generate(s, &a_minor()).gated()) as f32, 80);
        assert!(p >= 3., "default phrases keep their rests, got {p}");
        for s in 0..100 {
            let ph = generate(s, &Opts { density: 0.1, ..a_minor() });
            let mut run = 0;
            let mut longest = 0;
            for st in ph.steps.iter().cycle().take(ph.len() * 2) {
                run = if st.gate { 0 } else { run + 1 };
                longest = longest.max(run);
            }
            assert!(longest <= 7, "a gap of {longest} steps");
        }
    }

    #[test]
    fn mutate_keeps_most_steps_and_is_deterministic() {
        let base = generate(5, &a_minor());
        assert_eq!(base.mutate(1, 0.), base);
        assert_eq!(base.mutate(7, 0.3), base.mutate(7, 0.3));
        let (mut changed_any, mut kept_min) = (0, 16);
        for seed in 0..100 {
            let m = base.mutate(seed, 0.3);
            valid(&m);
            let kept = base.steps.iter().zip(&m.steps).filter(|(a, b)| a == b).count();
            kept_min = kept_min.min(kept);
            changed_any += usize::from(kept < 16);
            assert!(m.steps[0].gate == base.steps[0].gate);
        }
        assert!(kept_min >= 8, "kept only {kept_min} of 16");
        assert!(changed_any > 90, "mutate changed nothing in {} runs", 100 - changed_any);
        // More amount, more change.
        let diff = |a: f32| mean(|s| base.steps.iter().zip(&base.mutate(s, a).steps).filter(|(x, y)| x != y).count() as f32, 100);
        assert!(diff(0.8) > diff(0.2));
    }

    #[test]
    fn rotate_shift_transpose_and_reverse_keep_the_rules() {
        let p = generate(9, &a_minor());
        assert_eq!(p.rotate(16), p);
        assert_eq!(p.rotate(3).rotate(-3), p);
        assert_eq!(p.rotate(-1).steps[0], p.steps[1]);
        let sh = p.shift(2);
        assert!(!sh.steps[0].gate && !sh.steps[1].gate);
        assert_eq!(sh.steps[2].note, p.steps[0].note);
        assert_eq!(p.shift(0), p);
        valid(&sh);
        valid(&p.shift(-3));
        let rev = p.reverse();
        valid(&rev);
        assert_eq!(rev.steps[0].gate, p.steps[15].gate);
        assert_eq!(rev.gated(), p.gated());
        let up = p.transpose(2);
        valid(&up);
        assert_eq!(up.gated(), p.gated());
        let clean = generate(3, &Opts { tension: 0., ..a_minor() });
        for s in clean.transpose(2).steps.iter().chain(clean.transpose(-1).steps.iter()).filter(|s| s.gate) {
            assert!(clean.scale.contains((BASE_KEY as i32 + s.note) as u8));
        }
        // Two degrees up is a third in a minor scale: the root becomes the b3 or the 3.
        let root_step = clean.steps.iter().position(|s| s.gate && class_of(s.note, root_offset(clean.scale)) == 0).unwrap();
        let c = class_of(clean.transpose(2).steps[root_step].note, root_offset(clean.scale));
        assert_eq!(c, 3);
    }

    #[test]
    fn the_pattern_steps_round_trip() {
        for seed in 0..100 {
            let p = generate(seed, &Opts { slide: 0.8, ..a_minor() });
            let steps = p.pattern_steps();
            assert_eq!(steps.len(), 16);
            assert_eq!(Phrase::from_pattern(&steps, p.scale), p, "seed {seed}");
        }
    }

    #[test]
    fn clip_notes_follow_the_rules_of_the_synth() {
        for seed in 0..80 {
            let p = generate(seed, &Opts { slide: 0.8, accent: 0.7, ..a_minor() });
            let notes = p.notes(BASE_KEY);
            let attacks = p.steps.iter().enumerate().filter(|(i, s)| s.gate && !(s.tie && *i > 0)).count();
            assert_eq!(notes.len(), attacks);
            assert!(notes.iter().all(|n| n.start >= 0 && n.start + n.length <= p.ticks() + OVERLAP && n.length > 0));
            for w in notes.windows(2) {
                let step = (w[0].start / STEP_TICKS) as usize;
                // A glide overlaps the next note; a plain note ends before it.
                let overlaps = w[0].start + w[0].length > w[1].start;
                let tail = (0..p.len()).find(|&j| j >= step && (j + 1 == p.len() || !p.steps[j + 1].tie)).unwrap();
                assert_eq!(overlaps, p.steps[tail].slide && p.steps[tail + 1].gate, "seed {seed} at step {step}");
            }
            for n in &notes {
                let i = (n.start / STEP_TICKS) as usize;
                assert_eq!(n.velocity >= 0.78, p.steps[i].accent);
                assert_eq!(n.key as i32, BASE_KEY as i32 + p.steps[i].note);
            }
        }
        // Concrete: a glide into the next note, and a tie holding the note.
        let scale = Scale::new(Key(0), ScaleKind::Minor);
        let p = Phrase { scale, steps: vec![Step { slide: true, accent: true, ..Step::hit(0) }, Step::hit(12), Step::hit(7), Step { tie: true, ..Step::hit(7) }, Step::rest(0), Step::hit(0), Step::rest(0), Step::rest(0)] };
        valid(&p);
        let n = p.notes(36);
        assert_eq!(n.len(), 4);
        assert!(n[0].start + n[0].length > n[1].start && n[0].velocity >= 0.78 && n[1].velocity < 0.78);
        assert_eq!(n[2].length, STEP_TICKS + GATE_TICKS);
        assert_eq!(p.ticks(), 8 * STEP_TICKS);
        assert!(p.pattern_steps()[2].slide);
    }

    #[test]
    fn a_default_phrase_reads_like_acid() {
        // Over many seeds: most hits are root, fifth, b7 or b3; the line uses rests, accents and slides.
        let o = a_minor();
        let (mut core, mut all) = (0, 0);
        for s in 0..100 {
            let p = generate(s, &o);
            for st in p.steps.iter().filter(|s| s.gate) {
                all += 1;
                if [0, 3, 7, 10].contains(&class_of(st.note, root_offset(p.scale))) {
                    core += 1;
                }
            }
        }
        assert!(core as f32 / all as f32 > 0.6, "{core} of {all}");
    }
}
