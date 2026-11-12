//! Wavetable generation and editing (control thread only; everything here
//! allocates).
//!
//! [`WaveFrames`] is the editable form of a wavetable: up to
//! [`MAX_FRAMES`] single cycles of [`FRAME_LEN`] samples. The UI edits it
//! with the functions below and calls [`WaveFrames::build`] to get the
//! band-limited [`MipTable`] the synth plays, then hands that to the synth
//! (`WavetableSynth::set_user_table` before it runs, or
//! `SynthRemote::post_user_table` while it runs, or by saving it into the
//! device state).
//!
//! Tools: [`draw`] (breakpoints), [`WaveFrames::from_harmonics`] /
//! [`WaveFrames::set_harmonics`] (additive), [`WaveFrames::from_formula`]
//! (expressions such as `sin(2*pi*x) * (1 - y) + saw(x) * y`),
//! [`WaveFrames::morph`] (crossfade or spectral interpolation between key
//! frames), [`WaveFrames::normalize`], [`WaveFrames::remove_dc`], and the
//! factory bank in [`factory`].

use super::table::{FRAME_LEN, MAX_FRAMES, MipTable, Normalize};
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use std::f32::consts::{PI, TAU};

/// An editable wavetable: frames of exactly [`FRAME_LEN`] samples.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveFrames {
    pub name: String,
    pub frames: Vec<Vec<f32>>,
}

/// How [`WaveFrames::morph`] fills the frames between key frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Morph {
    /// Sample-wise crossfade.
    Crossfade,
    /// Interpolate harmonic magnitudes and phases (no phase cancellation
    /// dips between frames whose harmonics are out of phase).
    Spectral,
}

impl WaveFrames {
    /// From cycles of any length (each is resampled to [`FRAME_LEN`]).
    pub fn new(name: impl Into<String>, cycles: Vec<Vec<f32>>) -> Self {
        let frames = cycles.into_iter().take(MAX_FRAMES).map(|c| if c.len() == FRAME_LEN { c } else { resample_cycle(&c, FRAME_LEN) }).collect::<Vec<_>>();
        let frames = if frames.is_empty() { vec![sine_cycle()] } else { frames };
        Self { name: name.into(), frames }
    }

    pub fn single(name: impl Into<String>, cycle: Vec<f32>) -> Self {
        Self::new(name, vec![cycle])
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Additive: `spectrum(t, k)` returns the `(sin, cos)` amplitudes of
    /// harmonic `k >= 1` for frame position `t ∈ [0, 1]`.
    pub fn from_harmonics(name: impl Into<String>, frames: usize, spectrum: impl Fn(f32, usize) -> (f32, f32)) -> Self {
        let n = frames.clamp(1, MAX_FRAMES);
        let mut planner = RealFftPlanner::<f32>::new();
        let ifft = planner.plan_fft_inverse(FRAME_LEN);
        let cycles = (0..n)
            .map(|f| {
                let t = if n > 1 { f as f32 / (n - 1) as f32 } else { 0.0 };
                let mut bins = ifft.make_input_vec();
                for (k, b) in bins.iter_mut().enumerate().take(FRAME_LEN / 2).skip(1) {
                    let (s, c) = spectrum(t, k);
                    *b = Complex::new(c * 0.5, -s * 0.5);
                }
                let mut out = ifft.make_output_vec();
                let _ = ifft.process(&mut bins, &mut out);
                out
            })
            .collect();
        Self { name: name.into(), frames: cycles }
    }

    /// From a formula in `x` (phase 0..1) and `y` (frame position 0..1); see
    /// [`Formula`] for the syntax.
    pub fn from_formula(name: impl Into<String>, frames: usize, expr: &str) -> Result<Self, FormulaError> {
        let f = Formula::parse(expr)?;
        let n = frames.clamp(1, MAX_FRAMES);
        let cycles = (0..n)
            .map(|fi| {
                let y = if n > 1 { fi as f32 / (n - 1) as f32 } else { 0.0 };
                (0..FRAME_LEN).map(|i| f.eval(i as f32 / FRAME_LEN as f32, y)).map(|v| if v.is_finite() { v.clamp(-8.0, 8.0) } else { 0.0 }).collect()
            })
            .collect();
        Ok(Self { name: name.into(), frames: cycles })
    }

    /// Harmonic `(amplitude, phase)` pairs of frame `i` up to `max` (for an
    /// additive editor). Phase in radians, sine phase convention.
    pub fn harmonics(&self, i: usize, max: usize) -> Vec<(f32, f32)> {
        let Some(frame) = self.frames.get(i) else { return Vec::new() };
        let spec = spectrum(frame);
        (1..=max.min(FRAME_LEN / 2 - 1))
            .map(|k| {
                let c = spec[k];
                // x = a sin + b cos with a = −2 Im, b = 2 Re.
                let (a, b) = (-2.0 * c.im, 2.0 * c.re);
                ((a * a + b * b).sqrt(), b.atan2(a))
            })
            .collect()
    }

    /// Replace frame `i` by an additive cycle of `(amplitude, phase)` pairs
    /// (harmonic 1 first).
    pub fn set_harmonics(&mut self, i: usize, harmonics: &[(f32, f32)]) {
        let w = Self::from_harmonics("", 1, |_, k| harmonics.get(k - 1).map_or((0.0, 0.0), |&(a, ph)| (a * ph.cos(), a * ph.sin())));
        if let Some(f) = self.frames.get_mut(i) {
            *f = w.frames.into_iter().next().unwrap_or_default();
        }
    }

    /// Replace frame `i` by a drawn cycle (see [`draw`]).
    pub fn set_frame(&mut self, i: usize, cycle: &[f32]) {
        if let Some(f) = self.frames.get_mut(i) {
            *f = if cycle.len() == FRAME_LEN { cycle.to_vec() } else { resample_cycle(cycle, FRAME_LEN) };
        }
    }

    pub fn insert_frame(&mut self, at: usize, cycle: &[f32]) {
        if self.frames.len() < MAX_FRAMES {
            let c = if cycle.len() == FRAME_LEN { cycle.to_vec() } else { resample_cycle(cycle, FRAME_LEN) };
            self.frames.insert(at.min(self.frames.len()), c);
        }
    }

    pub fn remove_frame(&mut self, at: usize) {
        if self.frames.len() > 1 && at < self.frames.len() {
            self.frames.remove(at);
        }
    }

    /// Treat the current frames as evenly spaced key frames and fill `count`
    /// frames between them.
    pub fn morph(&self, count: usize, mode: Morph) -> Self {
        let count = count.clamp(1, MAX_FRAMES);
        let keys = &self.frames;
        if keys.len() < 2 {
            return Self { name: self.name.clone(), frames: vec![keys.first().cloned().unwrap_or_else(sine_cycle); count] };
        }
        let spectra: Vec<Vec<Complex<f32>>> = if mode == Morph::Spectral { keys.iter().map(|k| spectrum(k)).collect() } else { Vec::new() };
        let mut planner = RealFftPlanner::<f32>::new();
        let ifft = planner.plan_fft_inverse(FRAME_LEN);
        let frames = (0..count)
            .map(|i| {
                let pos = if count > 1 { i as f32 / (count - 1) as f32 } else { 0.0 } * (keys.len() - 1) as f32;
                let a = (pos as usize).min(keys.len() - 2);
                let t = pos - a as f32;
                match mode {
                    Morph::Crossfade => keys[a].iter().zip(&keys[a + 1]).map(|(x, y)| x + (y - x) * t).collect(),
                    Morph::Spectral => {
                        let mut bins = ifft.make_input_vec();
                        for (k, b) in bins.iter_mut().enumerate().take(FRAME_LEN / 2).skip(1) {
                            let (p, q) = (spectra[a][k], spectra[a + 1][k]);
                            let mag = p.norm() + (q.norm() - p.norm()) * t;
                            let (pa, qa) = (p.arg(), q.arg());
                            let mut d = qa - pa;
                            if d > PI {
                                d -= TAU;
                            } else if d < -PI {
                                d += TAU;
                            }
                            *b = Complex::from_polar(mag, pa + d * t);
                        }
                        let mut out = ifft.make_output_vec();
                        let _ = ifft.process(&mut bins, &mut out);
                        out
                    }
                }
            })
            .collect();
        Self { name: self.name.clone(), frames }
    }

    /// Scale to peak 1, per frame or with one gain for the whole table.
    pub fn normalize(&mut self, per_frame: bool) {
        let peak = |f: &Vec<f32>| f.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        if per_frame {
            for f in &mut self.frames {
                let p = peak(f);
                if p > 1e-9 {
                    f.iter_mut().for_each(|v| *v /= p);
                }
            }
        } else {
            let p = self.frames.iter().map(peak).fold(0.0f32, f32::max);
            if p > 1e-9 {
                self.frames.iter_mut().flatten().for_each(|v| *v /= p);
            }
        }
    }

    /// Remove each frame's DC offset.
    pub fn remove_dc(&mut self) {
        for f in &mut self.frames {
            let m = f.iter().sum::<f32>() / f.len().max(1) as f32;
            f.iter_mut().for_each(|v| *v -= m);
        }
    }

    pub fn reverse_frames(&mut self) {
        self.frames.reverse();
    }

    /// The band-limited table the synth plays (peak-normalised as a whole).
    pub fn build(&self) -> MipTable {
        MipTable::build(&self.frames, Normalize::Table)
    }
}

/// A cycle of [`FRAME_LEN`] samples through breakpoints `(x, y)` with
/// `x ∈ [0, 1]` sorted ascending and `y ∈ [−1, 1]`; straight lines between
/// them, wrapping from the last point to the first.
pub fn draw(points: &[(f32, f32)]) -> Vec<f32> {
    if points.is_empty() {
        return vec![0.0; FRAME_LEN];
    }
    let mut pts: Vec<(f32, f32)> = points.iter().map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(-1.0, 1.0))).collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let first = pts[0];
    let last = pts[pts.len() - 1];
    (0..FRAME_LEN)
        .map(|i| {
            let x = i as f32 / FRAME_LEN as f32;
            match pts.iter().position(|p| p.0 > x) {
                Some(0) => lerp_seg((last.0 - 1.0, last.1), first, x),
                Some(j) => lerp_seg(pts[j - 1], pts[j], x),
                None => lerp_seg(last, (first.0 + 1.0, first.1), x),
            }
        })
        .collect()
}

fn lerp_seg(a: (f32, f32), b: (f32, f32), x: f32) -> f32 {
    let w = b.0 - a.0;
    if w <= 1e-9 { b.1 } else { a.1 + (b.1 - a.1) * (x - a.0) / w }
}

/// Half spectrum of a cycle, scaled so `x[n] = Σ 2 Re(X_k e^{j2πkn/N})`.
fn spectrum(cycle: &[f32]) -> Vec<Complex<f32>> {
    let n = cycle.len().max(2);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = cycle.to_vec();
    input.resize(n, 0.0);
    let mut out = fft.make_output_vec();
    let _ = fft.process(&mut input, &mut out);
    let s = 1.0 / n as f32;
    let mut v: Vec<Complex<f32>> = out.into_iter().map(|c| c * s).collect();
    v.resize(FRAME_LEN / 2 + 1, Complex::new(0.0, 0.0));
    v
}

/// Band-limited resampling of one cycle to `n` samples (FFT, keeps the
/// harmonics that fit).
pub fn resample_cycle(x: &[f32], n: usize) -> Vec<f32> {
    if x.is_empty() {
        return vec![0.0; n];
    }
    if x.len() == n {
        return x.to_vec();
    }
    let m = x.len();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(m);
    let mut input = x.to_vec();
    let mut spec = fft.make_output_vec();
    let _ = fft.process(&mut input, &mut spec);
    let ifft = planner.plan_fft_inverse(n);
    let mut bins = ifft.make_input_vec();
    let top = (m / 2).min(n / 2).saturating_sub(1);
    let s = 1.0 / m as f32;
    for k in 0..=top {
        bins[k] = spec[k] * s;
    }
    let mut out = ifft.make_output_vec();
    let _ = ifft.process(&mut bins, &mut out);
    out
}

fn sine_cycle() -> Vec<f32> {
    (0..FRAME_LEN).map(|i| (TAU * i as f32 / FRAME_LEN as f32).sin()).collect()
}

// ── Formula ─────────────────────────────────────────────────────────────────

/// Why a formula did not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormulaError {
    pub message: String,
    /// Byte offset in the source.
    pub at: usize,
}

impl std::fmt::Display for FormulaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at {})", self.message, self.at)
    }
}

impl std::error::Error for FormulaError {}

#[derive(Clone, Debug)]
enum Node {
    Num(f32),
    X,
    Y,
    Neg(Box<Node>),
    Bin(char, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

#[derive(Clone, Copy, Debug)]
enum Func {
    Sin,
    Cos,
    Tan,
    Abs,
    Sqrt,
    Exp,
    Ln,
    Pow,
    Min,
    Max,
    Floor,
    Ceil,
    Fract,
    Sign,
    Clamp,
    Tanh,
    Saw,
    Tri,
    Sqr,
    Noise,
}

/// A wavetable formula: numbers, `x` (phase 0..1), `y` (frame 0..1), `pi`,
/// `e`, `+ - * / % ^`, parentheses and the functions `sin cos tan abs sqrt
/// exp ln pow(a,b) min(a,b) max(a,b) floor ceil fract sign clamp(v,lo,hi)
/// tanh` plus the cycle helpers `saw(p) tri(p) sqr(p)` (phase in cycles,
/// −1..1 out) and `noise(p)` (deterministic hash noise).
#[derive(Clone, Debug)]
pub struct Formula {
    root: Node,
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err<T>(&self, m: &str) -> Result<T, FormulaError> {
        Err(FormulaError { message: m.to_string(), at: self.i })
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn expr(&mut self) -> Result<Node, FormulaError> {
        let mut a = self.term()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let b = self.term()?;
            a = Node::Bin(c as char, Box::new(a), Box::new(b));
        }
        Ok(a)
    }
    fn term(&mut self) -> Result<Node, FormulaError> {
        let mut a = self.unary()?;
        while let Some(c @ (b'*' | b'/' | b'%')) = self.peek() {
            self.i += 1;
            let b = self.unary()?;
            a = Node::Bin(c as char, Box::new(a), Box::new(b));
        }
        Ok(a)
    }
    fn unary(&mut self) -> Result<Node, FormulaError> {
        match self.peek() {
            Some(b'-') => {
                self.i += 1;
                Ok(Node::Neg(Box::new(self.unary()?)))
            }
            Some(b'+') => {
                self.i += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }
    fn power(&mut self) -> Result<Node, FormulaError> {
        let a = self.atom()?;
        if self.peek() == Some(b'^') {
            self.i += 1;
            let b = self.unary()?; // right associative
            return Ok(Node::Bin('^', Box::new(a), Box::new(b)));
        }
        Ok(a)
    }
    fn atom(&mut self) -> Result<Node, FormulaError> {
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                let e = self.expr()?;
                if self.peek() != Some(b')') {
                    return self.err("expected ')'");
                }
                self.i += 1;
                Ok(e)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let st = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                    self.i += 1;
                }
                if self.i < self.s.len() && (self.s[self.i] == b'e' || self.s[self.i] == b'E') {
                    let save = self.i;
                    self.i += 1;
                    if self.i < self.s.len() && (self.s[self.i] == b'-' || self.s[self.i] == b'+') {
                        self.i += 1;
                    }
                    if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                            self.i += 1;
                        }
                    } else {
                        self.i = save;
                    }
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("");
                match t.parse::<f32>() {
                    Ok(v) => Ok(Node::Num(v)),
                    Err(_) => {
                        self.i = st;
                        self.err("bad number")
                    }
                }
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let st = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                    self.i += 1;
                }
                let name = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("").to_ascii_lowercase();
                match name.as_str() {
                    "x" => return Ok(Node::X),
                    "y" => return Ok(Node::Y),
                    "pi" => return Ok(Node::Num(PI)),
                    "e" => return Ok(Node::Num(std::f32::consts::E)),
                    _ => {}
                }
                let (func, arity) = match name.as_str() {
                    "sin" => (Func::Sin, 1),
                    "cos" => (Func::Cos, 1),
                    "tan" => (Func::Tan, 1),
                    "abs" => (Func::Abs, 1),
                    "sqrt" => (Func::Sqrt, 1),
                    "exp" => (Func::Exp, 1),
                    "ln" | "log" => (Func::Ln, 1),
                    "pow" => (Func::Pow, 2),
                    "min" => (Func::Min, 2),
                    "max" => (Func::Max, 2),
                    "floor" => (Func::Floor, 1),
                    "ceil" => (Func::Ceil, 1),
                    "fract" => (Func::Fract, 1),
                    "sign" | "sgn" => (Func::Sign, 1),
                    "clamp" => (Func::Clamp, 3),
                    "tanh" => (Func::Tanh, 1),
                    "saw" => (Func::Saw, 1),
                    "tri" => (Func::Tri, 1),
                    "sqr" | "square" => (Func::Sqr, 1),
                    "noise" | "rand" => (Func::Noise, 1),
                    _ => {
                        self.i = st;
                        return self.err(&format!("unknown name '{name}'"));
                    }
                };
                if self.peek() != Some(b'(') {
                    return self.err("expected '('");
                }
                self.i += 1;
                let mut args = vec![self.expr()?];
                while self.peek() == Some(b',') {
                    self.i += 1;
                    args.push(self.expr()?);
                }
                if self.peek() != Some(b')') {
                    return self.err("expected ')'");
                }
                self.i += 1;
                if args.len() != arity {
                    return self.err(&format!("'{name}' takes {arity} argument(s)"));
                }
                Ok(Node::Call(func, args))
            }
            Some(_) => self.err("unexpected character"),
            None => self.err("unexpected end"),
        }
    }
}

fn hash_noise(p: f32) -> f32 {
    let mut h = (p * 4096.0).floor() as i64 as u64;
    h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

impl Formula {
    pub fn parse(src: &str) -> Result<Self, FormulaError> {
        let mut p = Parser { s: src.as_bytes(), i: 0 };
        let root = p.expr()?;
        if p.peek().is_some() {
            return p.err("unexpected input");
        }
        Ok(Self { root })
    }

    pub fn eval(&self, x: f32, y: f32) -> f32 {
        Self::ev(&self.root, x, y)
    }

    fn ev(n: &Node, x: f32, y: f32) -> f32 {
        match n {
            Node::Num(v) => *v,
            Node::X => x,
            Node::Y => y,
            Node::Neg(a) => -Self::ev(a, x, y),
            Node::Bin(op, a, b) => {
                let (a, b) = (Self::ev(a, x, y), Self::ev(b, x, y));
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => a / b,
                    '%' => a.rem_euclid(b),
                    _ => a.powf(b),
                }
            }
            Node::Call(f, args) => {
                let a = |i: usize| Self::ev(&args[i], x, y);
                match f {
                    Func::Sin => a(0).sin(),
                    Func::Cos => a(0).cos(),
                    Func::Tan => a(0).tan(),
                    Func::Abs => a(0).abs(),
                    Func::Sqrt => a(0).max(0.0).sqrt(),
                    Func::Exp => a(0).exp(),
                    Func::Ln => a(0).ln(),
                    Func::Pow => a(0).powf(a(1)),
                    Func::Min => a(0).min(a(1)),
                    Func::Max => a(0).max(a(1)),
                    Func::Floor => a(0).floor(),
                    Func::Ceil => a(0).ceil(),
                    Func::Fract => a(0).rem_euclid(1.0),
                    Func::Sign => a(0).signum(),
                    Func::Clamp => a(0).clamp(a(1).min(a(2)), a(2).max(a(1))),
                    Func::Tanh => a(0).tanh(),
                    Func::Saw => 2.0 * a(0).rem_euclid(1.0) - 1.0,
                    Func::Tri => 1.0 - 4.0 * (a(0).rem_euclid(1.0) - 0.5).abs(),
                    Func::Sqr => {
                        if a(0).rem_euclid(1.0) < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    Func::Noise => hash_noise(a(0)),
                }
            }
        }
    }
}

// ── Factory bank ────────────────────────────────────────────────────────────

/// Index of [`factory`] names. Index 0 of the oscillator's `Wave` parameter
/// means "the legacy Table parameter"; `Wave = i` plays `factory()[i - 1]`
/// … see `WAVES` in the synth module.
pub const FACTORY_NAMES: &[&str] = &[
    "Basic Shapes",
    "Harmonic Sweep",
    "Formant Vowels",
    "Sync Sweep",
    "FM Bell",
    "Organ Drawbars",
    "Digital Steps",
    "Sine Fold",
    "Phase Distortion",
    "Spectral Comb",
    "Growl",
    "Noise Spectra",
    "PWM Saw",
    "Bit Glass",
];

fn saw_h(k: usize) -> f32 {
    -2.0 / (PI * k as f32)
}

fn square_h(k: usize) -> f32 {
    if k % 2 == 1 { 4.0 / (PI * k as f32) } else { 0.0 }
}

fn tri_h(k: usize) -> f32 {
    if k % 2 == 1 {
        let s = if (k / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
        s * 8.0 / (PI * PI * (k * k) as f32)
    } else {
        0.0
    }
}

/// Gaussian formant bump (in harmonic-frequency Hz terms at 110 Hz).
fn formant(f: f32, centres: &[(f32, f32)]) -> f32 {
    centres.iter().map(|&(c, g)| g * (-((f - c) / (0.12 * c + 60.0)).powi(2)).exp()).sum()
}

const VOWELS: [[(f32, f32); 3]; 5] = [
    [(800.0, 1.0), (1150.0, 0.5), (2900.0, 0.25)],  // A
    [(400.0, 1.0), (1600.0, 0.35), (2700.0, 0.2)],  // E
    [(270.0, 1.0), (2300.0, 0.25), (3000.0, 0.15)], // I
    [(450.0, 1.0), (800.0, 0.6), (2830.0, 0.1)],    // O
    [(325.0, 1.0), (700.0, 0.3), (2530.0, 0.08)],   // U
];

/// One factory table by [`FACTORY_NAMES`] index.
pub fn factory_table(i: usize) -> WaveFrames {
    let name = FACTORY_NAMES.get(i).copied().unwrap_or("Sine");
    let mut w = match i {
        0 => {
            // Sine → triangle → saw → square.
            let keys = WaveFrames::new(
                name,
                vec![
                    sine_cycle(),
                    WaveFrames::from_harmonics("", 1, |_, k| (tri_h(k), 0.0)).frames.remove(0),
                    WaveFrames::from_harmonics("", 1, |_, k| (saw_h(k), 0.0)).frames.remove(0),
                    WaveFrames::from_harmonics("", 1, |_, k| (square_h(k), 0.0)).frames.remove(0),
                ],
            );
            keys.morph(64, Morph::Spectral)
        }
        1 => WaveFrames::from_harmonics(name, 64, |t, k| {
            let reach = 1.0 + 63.0 * t * t;
            let g = ((reach - (k as f32 - 1.0)).clamp(0.0, 1.0)) / k as f32;
            (g, 0.0)
        }),
        2 => WaveFrames::from_harmonics(name, 64, |t, k| {
            let pos = t * 4.0;
            let a = (pos as usize).min(3);
            let u = pos - a as f32;
            let f = 110.0 * k as f32;
            let g = formant(f, &VOWELS[a]) * (1.0 - u) + formant(f, &VOWELS[a + 1]) * u;
            (g * saw_h(k).abs(), 0.0)
        }),
        3 => {
            // Hard-synced saw, slave ratio 1 → 8 (the mip levels band-limit it).
            let frames = (0..32)
                .map(|f| {
                    let ratio = 1.0 + 7.0 * f as f32 / 31.0;
                    (0..FRAME_LEN).map(|n| 2.0 * (n as f32 / FRAME_LEN as f32 * ratio).fract() - 1.0).collect()
                })
                .collect();
            WaveFrames { name: name.into(), frames }
        }
        4 => {
            let frames = (0..32)
                .map(|f| {
                    let index = 6.0 * (f as f32 / 31.0).powf(1.5);
                    (0..FRAME_LEN)
                        .map(|n| {
                            let ph = TAU * n as f32 / FRAME_LEN as f32;
                            (ph + index * (3.0 * ph).sin()).sin()
                        })
                        .collect()
                })
                .collect();
            WaveFrames { name: name.into(), frames }
        }
        5 => {
            // Drawbar registrations (16', 8', 5⅓', 4', 2⅔', 2', 1⅗', 1⅓', 1').
            const H: [usize; 9] = [1, 2, 3, 4, 6, 8, 10, 12, 16];
            const REG: [[f32; 9]; 4] = [[8., 8., 8., 0., 0., 0., 0., 0., 0.], [8., 8., 6., 8., 0., 0., 0., 0., 4.], [8., 0., 0., 8., 0., 8., 0., 0., 8.], [8., 8., 8., 8., 8., 8., 8., 8., 8.]];
            let keys: Vec<Vec<f32>> = REG
                .iter()
                .map(|r| WaveFrames::from_harmonics("", 1, |_, k| (H.iter().position(|&h| h == k).map_or(0.0, |j| r[j] / 8.0), 0.0)).frames.remove(0))
                .collect();
            WaveFrames::new(name, keys).morph(32, Morph::Crossfade)
        }
        6 => {
            let frames = (0..32)
                .map(|f| {
                    let steps = (64.0 * (2f32 / 64.0).powf(f as f32 / 31.0)).round().max(2.0);
                    (0..FRAME_LEN).map(|n| 2.0 * ((n as f32 / FRAME_LEN as f32) * steps).floor() / (steps - 1.0) - 1.0).collect()
                })
                .collect();
            WaveFrames { name: name.into(), frames }
        }
        7 => {
            let frames = (0..32)
                .map(|f| {
                    let g = 1.0 + 7.0 * f as f32 / 31.0;
                    (0..FRAME_LEN).map(|n| fold(g * (TAU * n as f32 / FRAME_LEN as f32).sin())).collect()
                })
                .collect();
            WaveFrames { name: name.into(), frames }
        }
        8 => {
            // Resonant phase distortion: a windowed sine whose frequency rises.
            let frames = (0..32)
                .map(|f| {
                    let r = 1.0 + 15.0 * f as f32 / 31.0;
                    (0..FRAME_LEN)
                        .map(|n| {
                            let x = n as f32 / FRAME_LEN as f32;
                            (1.0 - x) * (TAU * r * x).cos() * 0.5 + 0.5 * (1.0 - x) - 0.25
                        })
                        .collect()
                })
                .collect();
            WaveFrames { name: name.into(), frames }
        }
        9 => WaveFrames::from_harmonics(name, 32, |t, k| {
            let comb = 0.5 + 0.5 * (PI * k as f32 * (0.5 + 3.5 * t)).cos();
            (saw_h(k) * comb, 0.0)
        }),
        10 => WaveFrames::from_harmonics(name, 32, |t, k| {
            let f = 55.0 * k as f32;
            let c = 300.0 + 1700.0 * t;
            let bump = (-((f - c) / (0.3 * c)).powi(2)).exp() * 3.0;
            (saw_h(k) * (0.4 + bump), 0.0)
        }),
        11 => WaveFrames::from_harmonics(name, 32, |t, k| {
            let seed = (k as f32 * 12.9898 + (t * 31.0).round() * 78.233).sin() * 43_758.547;
            let ph = seed.fract() * TAU;
            let tilt = 1.0 / (k as f32).powf(0.8 + 0.6 * (1.0 - t));
            (tilt * ph.cos(), tilt * ph.sin())
        }),
        12 => {
            // Two saws, one shifted: their difference is a pulse whose width sweeps.
            let frames = (0..32)
                .map(|f| {
                    let w = 0.5 * f as f32 / 31.0 * 0.95;
                    (0..FRAME_LEN)
                        .map(|n| {
                            let x = n as f32 / FRAME_LEN as f32;
                            0.5 * ((2.0 * x - 1.0) - (2.0 * (x + w).fract() - 1.0)) + 0.5 * (2.0 * x - 1.0)
                        })
                        .collect()
                })
                .collect();
            WaveFrames { name: name.into(), frames }
        }
        _ => WaveFrames::from_harmonics(name, 32, |t, k| {
            // Sparse, bright partials that drift: "glassy" digital.
            let on = (k * 7 + (t * 13.0) as usize).is_multiple_of(5) || k == 1;
            (if on { 1.0 / (k as f32).sqrt() } else { 0.0 }, 0.0)
        }),
    };
    w.remove_dc();
    w.normalize(true);
    w
}

fn fold(x: f32) -> f32 {
    // Triangle fold into −1..1.
    let y = (x + 1.0).rem_euclid(4.0);
    if y < 2.0 { y - 1.0 } else { 3.0 - y }
}

/// The whole factory bank, in [`FACTORY_NAMES`] order.
pub fn factory() -> Vec<WaveFrames> {
    (0..FACTORY_NAMES.len()).map(factory_table).collect()
}

/// The six legacy tables (the original `TABLES` list), as frames.
pub fn legacy_table(i: usize) -> WaveFrames {
    match i {
        1 => WaveFrames::from_harmonics("Square", 1, |_, k| (square_h(k), 0.0)),
        2 => WaveFrames::from_harmonics("Pulse", 16, |t, k| {
            let w = 0.5 - 0.45 * t;
            let kk = k as f32;
            let amp = 4.0 * (PI * kk * w).sin() / (PI * kk);
            let ph = TAU * kk * w * 0.5;
            (amp * ph.sin(), amp * ph.cos())
        }),
        3 => WaveFrames::from_harmonics("Sine-Saw", 16, |t, k| {
            let reach = 0.35 + 600.0 * t.powi(3);
            (saw_h(k) * (-((k - 1) as f32) / reach).exp(), 0.0)
        }),
        4 => WaveFrames::from_harmonics("Triangle", 1, |_, k| (tri_h(k), 0.0)),
        5 => WaveFrames::single("Sine", sine_cycle()),
        _ => WaveFrames::from_harmonics("Saw", 1, |_, k| (saw_h(k), 0.0)),
    }
}

/// Legacy tables built like the original synth did (harmonics straight
/// into the mip levels, each frame peak-normalised).
pub(crate) fn legacy_mip(i: usize) -> MipTable {
    let spectrum: Box<dyn Fn(usize, usize) -> (f32, f32)> = match i {
        1 => Box::new(|_, k| (square_h(k), 0.0)),
        2 => Box::new(|f, k| {
            let w = 0.5 - 0.45 * f as f32 / 15.0;
            let kk = k as f32;
            let amp = 4.0 * (PI * kk * w).sin() / (PI * kk);
            let ph = TAU * kk * w * 0.5;
            (amp * ph.sin(), amp * ph.cos())
        }),
        3 => Box::new(|f, k| {
            let t = f as f32 / 15.0;
            let reach = 0.35 + 600.0 * t.powi(3);
            (saw_h(k) * (-((k - 1) as f32) / reach).exp(), 0.0)
        }),
        4 => Box::new(|_, k| (tri_h(k), 0.0)),
        5 => Box::new(|_, k| if k == 1 { (1.0, 0.0) } else { (0.0, 0.0) }),
        _ => Box::new(|_, k| (saw_h(k), 0.0)),
    };
    let frames = if i == 2 || i == 3 { 16 } else { 1 };
    MipTable::from_harmonics(frames, Normalize::PerFrame, spectrum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_parses_and_evaluates() {
        let f = Formula::parse("sin(2*pi*x) * (1 - y) + saw(x) * y").unwrap();
        assert!((f.eval(0.25, 0.0) - 1.0).abs() < 1e-5);
        assert!((f.eval(0.25, 1.0) - -0.5).abs() < 1e-5);
        let f = Formula::parse("-2^2 + pow(2, 3) + min(1, 2) + clamp(5, 0, 1) + 1e1").unwrap();
        assert!((f.eval(0.0, 0.0) - (-4.0 + 8.0 + 1.0 + 1.0 + 10.0)).abs() < 1e-4);
        assert!(Formula::parse("sin(x").is_err());
        assert!(Formula::parse("foo(x)").is_err());
        assert!(Formula::parse("pow(x)").is_err());
        let e = Formula::parse("1 + $").unwrap_err();
        assert_eq!(e.at, 4);
    }

    #[test]
    fn formula_table_has_frames() {
        let w = WaveFrames::from_formula("t", 8, "sin(2*pi*x*(1+y*3))").unwrap();
        assert_eq!(w.len(), 8);
        assert_eq!(w.frames[3].len(), FRAME_LEN);
    }

    #[test]
    fn additive_round_trip() {
        let mut w = WaveFrames::single("t", vec![0.0; 64]);
        w.set_harmonics(0, &[(1.0, 0.0), (0.0, 0.0), (0.5, 0.0)]);
        let h = w.harmonics(0, 4);
        assert!((h[0].0 - 1.0).abs() < 1e-3 && h[1].0 < 1e-3 && (h[2].0 - 0.5).abs() < 1e-3, "{h:?}");
        assert!(h[0].1.abs() < 1e-3);
    }

    #[test]
    fn draw_and_resample() {
        let c = draw(&[(0.0, -1.0), (0.5, 1.0)]);
        assert_eq!(c.len(), FRAME_LEN);
        assert!((c[FRAME_LEN / 4] - 0.0).abs() < 0.01);
        assert!((c[FRAME_LEN / 2] - 1.0).abs() < 0.01);
        let r = resample_cycle(&c, 512);
        assert!((r[128] - 0.0).abs() < 0.05 && (r[256] - 1.0).abs() < 0.05);
    }

    #[test]
    fn spectral_morph_keeps_level() {
        let sin = sine_cycle();
        let neg: Vec<f32> = sin.iter().map(|v| -v).collect();
        let w = WaveFrames::new("t", vec![sin, neg]);
        let peak = |f: &Vec<f32>| f.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let x = w.morph(5, Morph::Crossfade);
        assert!(peak(&x.frames[2]) < 1e-3, "crossfade cancels in the middle");
        let s = w.morph(5, Morph::Spectral);
        assert!((peak(&s.frames[2]) - 1.0).abs() < 1e-2, "spectral keeps the level");
    }

    #[test]
    fn factory_tables_are_sane() {
        for (i, w) in factory().into_iter().enumerate() {
            assert!(!w.is_empty(), "{i}");
            for f in &w.frames {
                assert_eq!(f.len(), FRAME_LEN);
                let peak = f.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!(peak > 0.5 && peak <= 1.0001, "{} peak {peak}", w.name);
                assert!(f.iter().all(|v| v.is_finite()));
            }
        }
    }
}
