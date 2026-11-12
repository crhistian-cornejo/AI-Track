//! The meter strip: a bar of analysis modules that slides down from under
//! the top bar and reads the Main output through the engine's analysis tap.
//!
//! Modules (the user picks which ones and in what order): scrolling waveform
//! coloured by frequency content, spectrogram, spectrum with a dB / Hz / note
//! read-out, stereo field with correlation, level bars with a loudness
//! read-out, VU needle and oscilloscope.
//!
//! All analysis runs here on the UI side, once per painted frame, on the
//! frames the audio thread left in its ring. It stops entirely while the
//! strip is closed and nothing else asked for loudness ([`MeterStrip::keep_alive`]).

use crate::bridge::EngineBridge;
use crate::icons::Icon;
use crate::theme::*;
use crate::views::device_ui::kit::{self, Rect};
use crate::widgets::{self, menu_header, menu_item};
use crate::workspace::Workspace;
use aitrack_dsp::loudness::{LoudnessReading, LufsMeter};
use aitrack_dsp::spectrum::SpectrumAnalyzer;
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, StatefulInteractiveElement, Styled, TextAlign, Window, anchored, canvas, deferred, div, point,
    px,
};
use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

const SPEC_BANDS: usize = 160;
const GRAM_BANDS: usize = 40;
const GRAM_COLS: usize = 128;
const SCOPE_LEN: usize = 4096;
const STEREO_PTS: usize = 384;
const WAVE_COLS: usize = 420;
const FLOOR_DB: f32 = -90.;
pub const MIN_H: f32 = 64.;
pub const MAX_H: f32 = 240.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    Waveform,
    Spectrogram,
    Spectrum,
    Stereo,
    Levels,
    Vu,
    Scope,
}

impl Module {
    pub const ALL: [Module; 7] = [Self::Waveform, Self::Spectrogram, Self::Spectrum, Self::Stereo, Self::Levels, Self::Vu, Self::Scope];

    pub fn label(self) -> &'static str {
        match self {
            Self::Waveform => "Waveform",
            Self::Spectrogram => "Spectrogram",
            Self::Spectrum => "Spectrum",
            Self::Stereo => "Stereo field",
            Self::Levels => "Levels & loudness",
            Self::Vu => "VU meter",
            Self::Scope => "Oscilloscope",
        }
    }

    /// Fixed width for a strip `h` pixels high; `None` shares the free space.
    fn width(self, h: f32) -> Option<f32> {
        match self {
            Self::Waveform | Self::Spectrum => None,
            Self::Spectrogram => Some(220.),
            Self::Stereo => Some(h),
            Self::Levels => Some(132.),
            Self::Vu => Some((h * 1.5).clamp(120., 220.)),
            Self::Scope => Some(190.),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoudMode {
    Momentary,
    ShortTerm,
    Integrated,
    Rms,
}

impl LoudMode {
    const ALL: [LoudMode; 4] = [Self::Momentary, Self::ShortTerm, Self::Integrated, Self::Rms];

    fn label(self) -> &'static str {
        match self {
            Self::Momentary => "LUFS momentary (0.4 s)",
            Self::ShortTerm => "LUFS short-term (3 s)",
            Self::Integrated => "LUFS integrated",
            Self::Rms => "RMS (0.3 s)",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Momentary => "LUFS M",
            Self::ShortTerm => "LUFS S",
            Self::Integrated => "LUFS I",
            Self::Rms => "RMS",
        }
    }
}

/// One column of the scrolling waveform.
#[derive(Clone, Copy, Default)]
struct WaveCol {
    min: f32,
    max: f32,
    /// Share of the energy below ~200 Hz and above ~3 kHz.
    low: f32,
    high: f32,
}

struct Analysis {
    sr: u32,
    lufs: LufsMeter,
    mid: SpectrumAnalyzer,
    side: SpectrumAnalyzer,
    gram: SpectrumAnalyzer,
    hold: Vec<f32>,
    gram_cols: VecDeque<[u8; GRAM_BANDS]>,
    gram_acc: usize,
    scope: VecDeque<f32>,
    stereo: VecDeque<(f32, f32)>,
    stereo_skip: usize,
    wave: VecDeque<WaveCol>,
    col: (f32, f32, f32, f32, f32, usize),
    lp_low: f32,
    lp_high: f32,
    peak: [f32; 2],
    rms_sq: [f32; 2],
    corr: [f32; 3],
    heard: Instant,
}

impl Analysis {
    fn new(sr: u32) -> Self {
        let rate = f64::from(sr);
        let mut mid = SpectrumAnalyzer::new(4096, SPEC_BANDS, 20., 20_000., rate);
        let mut side = SpectrumAnalyzer::new(4096, SPEC_BANDS, 20., 20_000., rate);
        let mut gram = SpectrumAnalyzer::new(2048, GRAM_BANDS, 30., 18_000., rate);
        mid.set_fall_rate(36.);
        side.set_fall_rate(36.);
        gram.set_fall_rate(240.);
        // 4.5 dB/oct tilt: pink-ish material reads flat, as on mastering analysers.
        mid.set_tilt(4.5);
        side.set_tilt(4.5);
        gram.set_tilt(4.5);
        Self {
            sr,
            lufs: LufsMeter::new(rate),
            mid,
            side,
            gram,
            hold: vec![FLOOR_DB; SPEC_BANDS],
            gram_cols: VecDeque::with_capacity(GRAM_COLS),
            gram_acc: 0,
            scope: VecDeque::with_capacity(SCOPE_LEN),
            stereo: VecDeque::with_capacity(STEREO_PTS),
            stereo_skip: 0,
            wave: VecDeque::with_capacity(WAVE_COLS),
            col: (0., 0., 0., 0., 0., 0),
            lp_low: 0.,
            lp_high: 0.,
            peak: [0.; 2],
            rms_sq: [0.; 2],
            corr: [0.; 3],
            heard: Instant::now() - Duration::from_secs(60),
        }
    }

    fn process(&mut self, frames: &[[f32; 2]]) {
        if frames.is_empty() {
            return;
        }
        let sr = self.sr as f32;
        let one_pole = |seconds: f32| 1. - (-1. / (seconds * sr)).exp();
        let (a_rms, a_corr, peak_fall) = (one_pole(0.3), one_pole(0.12), (-1. / (0.45 * sr)).exp());
        let (a_low, a_high) = (one_pole(1. / (std::f32::consts::TAU * 200.)), one_pole(1. / (std::f32::consts::TAU * 3000.)));
        let wave_len = (self.sr as usize / 150).max(32);
        let mut loud = false;
        for &[l, r] in frames {
            self.lufs.push(l, r);
            let (m, s) = ((l + r) * 0.5, (l - r) * 0.5);
            self.mid.push_sample(m);
            self.side.push_sample(s);
            self.gram.push_sample(m);
            if self.scope.len() == SCOPE_LEN {
                self.scope.pop_front();
            }
            self.scope.push_back(m);
            self.stereo_skip += 1;
            if self.stereo_skip >= 6 {
                self.stereo_skip = 0;
                if self.stereo.len() == STEREO_PTS {
                    self.stereo.pop_front();
                }
                self.stereo.push_back((l, r));
            }
            for (c, x) in [l, r].into_iter().enumerate() {
                self.peak[c] = (self.peak[c] * peak_fall).max(x.abs());
                self.rms_sq[c] += (x * x - self.rms_sq[c]) * a_rms;
            }
            self.corr[0] += (l * r - self.corr[0]) * a_corr;
            self.corr[1] += (l * l - self.corr[1]) * a_corr;
            self.corr[2] += (r * r - self.corr[2]) * a_corr;
            // Waveform column with a rough three-way split of its energy.
            self.lp_low += (m - self.lp_low) * a_low;
            self.lp_high += (m - self.lp_high) * a_high;
            let high = m - self.lp_high;
            let c = &mut self.col;
            c.0 = c.0.min(m);
            c.1 = c.1.max(m);
            c.2 += self.lp_low * self.lp_low;
            c.3 += high * high;
            c.4 += m * m;
            c.5 += 1;
            if c.5 >= wave_len {
                let total = c.4.max(1e-12);
                if self.wave.len() == WAVE_COLS {
                    self.wave.pop_front();
                }
                self.wave.push_back(WaveCol { min: c.0, max: c.1, low: (c.2 / total).min(1.), high: (c.3 / total).min(1.) });
                *c = (0., 0., 0., 0., 0., 0);
            }
            loud |= m.abs() > 3e-5;
        }
        if loud {
            self.heard = Instant::now();
        }
        for (h, b) in self.hold.iter_mut().zip(self.mid.bands()) {
            *h = (*h - 12. * frames.len() as f32 / sr).max(*b);
        }
        let hop = (self.sr as usize / 45).max(256);
        self.gram_acc += frames.len();
        while self.gram_acc >= hop {
            self.gram_acc -= hop;
            let mut col = [0u8; GRAM_BANDS];
            for (c, db) in col.iter_mut().zip(self.gram.bands()) {
                *c = (((db - FLOOR_DB) / -FLOOR_DB).clamp(0., 1.) * 255.) as u8;
            }
            if self.gram_cols.len() == GRAM_COLS {
                self.gram_cols.pop_front();
            }
            self.gram_cols.push_back(col);
        }
    }

    fn correlation(&self) -> f32 {
        let d = (self.corr[1] * self.corr[2]).sqrt();
        if d > 1e-9 { (self.corr[0] / d).clamp(-1., 1.) } else { 0. }
    }
}

fn db(v: f32) -> f32 {
    if v > 1e-6 { 20. * v.log10() } else { -120. }
}

/// What the painters need for one frame.
struct Snap {
    mid: Vec<f32>,
    side: Vec<f32>,
    hold: Vec<f32>,
    gram: Vec<[u8; GRAM_BANDS]>,
    scope: Vec<f32>,
    stereo: Vec<(f32, f32)>,
    wave: Vec<WaveCol>,
    peak_db: [f32; 2],
    rms_db: [f32; 2],
    corr: f32,
    loud: LoudnessReading,
    mode: LoudMode,
    mid_side: bool,
    hover: Option<f32>,
}

#[derive(Clone, Copy, PartialEq)]
enum PopTarget {
    Module(usize),
    Add,
}

pub struct MeterStrip {
    pub open: bool,
    pub height: f32,
    pub modules: Vec<Module>,
    pub loud_mode: LoudMode,
    pub mid_side: bool,
    /// Analyse the selected track instead of the Main output.
    pub follow_track: bool,
    source: Option<aitrack_model::TrackId>,
    analysis: Option<Analysis>,
    buf: Vec<[f32; 2]>,
    alive: Instant,
    engine_on: bool,
    resize: Option<(f32, f32)>,
    pop: Option<(Point<Pixels>, PopTarget)>,
    hover: Option<f32>,
    spectrum_at: Rc<Cell<Bounds<Pixels>>>,
    opened: u64,
}

impl Default for MeterStrip {
    fn default() -> Self {
        Self {
            open: false,
            height: 112.,
            modules: vec![Module::Waveform, Module::Spectrogram, Module::Spectrum, Module::Stereo, Module::Levels],
            loud_mode: LoudMode::ShortTerm,
            mid_side: false,
            follow_track: false,
            source: None,
            analysis: None,
            buf: Vec::with_capacity(8192),
            alive: Instant::now() - Duration::from_secs(60),
            engine_on: false,
            resize: None,
            pop: None,
            hover: None,
            spectrum_at: Rc::default(),
            opened: 0,
        }
    }
}

impl MeterStrip {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.opened += 1;
        self.pop = None;
    }

    /// Something outside the strip (the limiter's editor) wants loudness:
    /// keep analysing for a little while even if the strip is closed.
    pub fn keep_alive(&mut self) {
        self.alive = Instant::now();
    }

    pub fn loudness(&self) -> Option<LoudnessReading> {
        self.analysis.as_ref().map(|a| a.lufs.reading())
    }

    fn needed(&self) -> bool {
        self.open || self.alive.elapsed() < Duration::from_millis(600)
    }

    /// Drain the engine's tap and update every analyser. Once per frame.
    /// `selected` is the selected track, analysed instead of the Main output
    /// when [`follow_track`](Self::follow_track) is on.
    pub fn tick(&mut self, engine: &mut dyn EngineBridge, selected: Option<aitrack_model::TrackId>) {
        let needed = self.needed();
        if needed != self.engine_on {
            engine.set_analysis(needed);
            self.engine_on = needed;
        }
        if !needed {
            return;
        }
        let source = if self.follow_track { selected } else { None };
        if source != self.source {
            engine.set_analysis_source(source);
            self.source = source;
            self.analysis = None;
        }
        self.buf.clear();
        let sr = engine.drain_analysis(&mut self.buf).max(8_000);
        if self.analysis.as_ref().is_none_or(|a| a.sr != sr) {
            self.analysis = Some(Analysis::new(sr));
        }
        if let Some(a) = &mut self.analysis {
            a.process(&self.buf);
        }
    }

    /// Frames are needed while sound is arriving (and briefly after, so the
    /// displays fall back to rest) or the strip is being resized.
    pub fn wants_frames(&self) -> bool {
        self.resize.is_some() || (self.needed() && self.analysis.as_ref().is_some_and(|a| a.heard.elapsed() < Duration::from_millis(2500)))
    }

    pub fn reset_loudness(&mut self) {
        if let Some(a) = &mut self.analysis {
            a.lufs.reset();
        }
    }

    fn snap(&self) -> Snap {
        let empty = Analysis::new(48_000);
        let a = self.analysis.as_ref().unwrap_or(&empty);
        Snap {
            mid: a.mid.bands().to_vec(),
            side: a.side.bands().to_vec(),
            hold: a.hold.clone(),
            gram: a.gram_cols.iter().copied().collect(),
            scope: a.scope.iter().copied().collect(),
            stereo: a.stereo.iter().copied().collect(),
            wave: a.wave.iter().copied().collect(),
            peak_db: [db(a.peak[0]), db(a.peak[1])],
            rms_db: [db(a.rms_sq[0].sqrt()), db(a.rms_sq[1].sqrt())],
            corr: a.correlation(),
            loud: a.lufs.reading(),
            mode: self.loud_mode,
            mid_side: self.mid_side,
            hover: self.hover,
        }
    }
}

// ───────────────────────────── Painters (shared with fx.analyzer) ─────────────────────────────

/// A log-spaced spectrum (`values` in dB, one per band) as a filled curve.
pub fn draw_spectrum(window: &mut Window, r: Rect, values: &[f32], floor_db: f32, color: Hsla) {
    if values.len() < 2 {
        return;
    }
    let n = (values.len() - 1) as f32;
    let pts: Vec<Point<Pixels>> =
        values.iter().enumerate().map(|(i, v)| r.pt(i as f32 / n, 1. - ((v - floor_db) / -floor_db).clamp(0., 1.))).collect();
    kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.26));
    kit::line(window, &pts, 1.25, color);
}

fn level_frac(db: f32) -> f32 {
    ((db + 60.) / 60.).clamp(0., 1.)
}

/// Two vertical L/R bars: RMS as the body (green / amber / red zones), peak
/// as a line, with a dB scale on the left.
pub fn draw_level_bars(window: &mut Window, cx: &mut gpui::App, r: Rect, peaks_db: [f32; 2], rms_db: [f32; 2]) {
    let pal = p();
    let scale_w = 18.;
    for mark in [0., -12., -24., -48.] {
        let y = r.py(1. - level_frac(mark));
        kit::txt(window, cx, &format!("{}", -mark as i32), r.x + scale_w - 4., y - 6., TEXT_XS, pal.faint, TextAlign::Right);
    }
    let bar_w = ((r.w - scale_w - 3.) / 2.).max(3.);
    for c in 0..2 {
        let x = r.x + scale_w + c as f32 * (bar_w + 3.);
        kit::bar(window, x, r.y, bar_w, r.h, 2., content(0.07));
        let level = level_frac(rms_db[c]);
        // Draw the zones bottom-up, each clipped to the level.
        for (from, to, color) in [(-60., -18., pal.meter_low), (-18., -6., pal.meter_mid), (-6., 0., pal.meter_high)] {
            let (a, b) = (level_frac(from), level_frac(to).min(level));
            if b > a {
                kit::bar(window, x, r.py(1. - b), bar_w, (b - a) * r.h, 0., color);
            }
        }
        let peak = level_frac(peaks_db[c]);
        if peak > 0. {
            let color = if peaks_db[c] > -0.3 { pal.meter_high } else { pal.text };
            kit::bar(window, x, r.py(1. - peak) - 1., bar_w, 2., 0., color);
        }
    }
}

/// Phase correlation from −1 (out of phase) to +1 (mono).
pub fn draw_correlation(window: &mut Window, r: Rect, corr: f32, vertical: bool) {
    let pal = p();
    kit::bar(window, r.x, r.y, r.w, r.h, 2., content(0.08));
    let color = if corr < 0. { pal.record } else { pal.play };
    if vertical {
        kit::bar(window, r.x, r.py(0.5) - 0.5, r.w, 1., 0., content(0.25));
        let y = r.py(0.5 - corr * 0.5);
        kit::bar(window, r.x, y - 2., r.w, 4., 2., color);
    } else {
        kit::bar(window, r.px(0.5) - 0.5, r.y, 1., r.h, 0., content(0.25));
        let x = r.px(0.5 + corr * 0.5);
        kit::bar(window, x - 2., r.y, 4., r.h, 2., color);
    }
}

fn paint_waveform(window: &mut Window, r: Rect, s: &Snap) {
    let pal = p();
    kit::hline(window, r, r.py(0.5), content(0.06));
    let w = r.w / WAVE_COLS as f32;
    let first = WAVE_COLS - s.wave.len();
    for (i, c) in s.wave.iter().enumerate() {
        // Bass reads warm, highs cool, the middle in the accent.
        let color = mix(mix(pal.accent, pal.meter_high, c.low), pal.accent2, c.high);
        let (top, bottom) = (r.py(0.5 - c.max.clamp(-1., 1.) * 0.48), r.py(0.5 - c.min.clamp(-1., 1.) * 0.48));
        kit::bar(window, r.x + (first + i) as f32 * w, top, w.max(1.), (bottom - top).max(1.), 0., color);
    }
}

fn paint_spectrogram(window: &mut Window, r: Rect, s: &Snap) {
    let pal = p();
    let (cw, rh) = (r.w / GRAM_COLS as f32, r.h / GRAM_BANDS as f32);
    let first = GRAM_COLS - s.gram.len();
    let ramp = |v: u8| -> Hsla {
        let t = f32::from(v) / 255.;
        // Quiet → loud: well, second accent, hot, bright.
        if t < 0.45 {
            mix(pal.well, pal.accent2, t / 0.45)
        } else if t < 0.75 {
            mix(pal.accent2, pal.meter_high, (t - 0.45) / 0.3)
        } else {
            mix(pal.meter_high, pal.meter_mid, (t - 0.75) / 0.25)
        }
    };
    for (i, col) in s.gram.iter().enumerate() {
        let x = r.x + (first + i) as f32 * cw;
        for (b, v) in col.iter().enumerate() {
            if *v > 70 {
                kit::bar(window, x, r.bottom() - (b + 1) as f32 * rh, cw + 0.5, rh + 0.5, 0., ramp(*v));
            }
        }
    }
}

/// `"A#1 +39 cents"` for a frequency.
pub fn note_name(freq: f32) -> String {
    if freq <= 0. {
        return String::new();
    }
    let midi = 69. + 12. * (freq / 440.).log2();
    let nearest = midi.round();
    let cents = ((midi - nearest) * 100.).round() as i32;
    let n = nearest as i32;
    format!("{}{} {:+} cents", aitrack_model::Key::NAMES_SHARP[n.rem_euclid(12) as usize], n.div_euclid(12) - 1, cents)
}

fn band_freq(i: usize, n: usize) -> f32 {
    20. * 1000f32.powf(i as f32 / (n.max(2) - 1) as f32)
}

fn paint_spectrum(window: &mut Window, cx: &mut gpui::App, r: Rect, s: &Snap) {
    let pal = p();
    kit::freq_grid(window, cx, r, true);
    if s.mid_side {
        draw_spectrum(window, r, &s.side, FLOOR_DB, pal.accent2);
    }
    draw_spectrum(window, r, &s.mid, FLOOR_DB, pal.accent);
    if s.hold.len() > 1 {
        let n = (s.hold.len() - 1) as f32;
        let pts: Vec<Point<Pixels>> =
            s.hold.iter().enumerate().map(|(i, v)| r.pt(i as f32 / n, 1. - ((v - FLOOR_DB) / -FLOOR_DB).clamp(0., 1.))).collect();
        kit::line(window, &pts, 1., content(0.35));
    }
    // Read-out: under the pointer, or the loudest band.
    let n = s.mid.len();
    let band = match s.hover {
        Some(x) if r.w > 0. => Some((((x - r.x) / r.w).clamp(0., 1.) * (n.max(1) - 1) as f32).round() as usize),
        _ => (0..n).max_by(|a, b| s.mid[*a].total_cmp(&s.mid[*b])).filter(|i| s.mid[*i] > FLOOR_DB + 12.),
    };
    if let Some(i) = band.filter(|i| *i < n) {
        let f = band_freq(i, n);
        let hz = if f >= 1000. { format!("{:.2} kHz", f / 1000.) } else { format!("{f:.1} Hz") };
        let text = format!("{:.1} dB  |  {hz}  |  {}", s.mid[i], note_name(f));
        if s.hover.is_some() {
            kit::vline(window, r, r.px(i as f32 / (n.max(2) - 1) as f32), content(0.4));
        }
        kit::txt(window, cx, &text, r.right() - 6., r.y + 4., TEXT_XS, if s.hover.is_some() { pal.text } else { pal.muted }, TextAlign::Right);
    }
}

fn paint_stereo(window: &mut Window, r: Rect, s: &Snap) {
    let pal = p();
    let meter_h = 5.;
    let field = Rect { h: r.h - meter_h - 6., ..r }.inset(4.);
    let radius = field.w.min(field.h) * 0.5;
    let (cx, cy) = (field.px(0.5), field.py(0.5));
    let ring: Vec<Point<Pixels>> =
        (0..=48).map(|i| (i as f32 / 48. * std::f32::consts::TAU).sin_cos()).map(|(sn, cs)| point(px(cx + cs * radius), px(cy + sn * radius))).collect();
    kit::line(window, &ring, 1., content(0.16));
    let d = radius * std::f32::consts::FRAC_1_SQRT_2;
    kit::line(window, &[point(px(cx - d), px(cy - d)), point(px(cx + d), px(cy + d))], 1., content(0.1));
    kit::line(window, &[point(px(cx - d), px(cy + d)), point(px(cx + d), px(cy - d))], 1., content(0.1));
    let dot = with_alpha(pal.accent, 0.7);
    for (l, rr) in &s.stereo {
        // Mid up, side across (a mono signal is a vertical line).
        let (x, y) = ((l - rr) * std::f32::consts::FRAC_1_SQRT_2, (l + rr) * std::f32::consts::FRAC_1_SQRT_2);
        let len = (x * x + y * y).sqrt();
        let k = if len > 1. { 1. / len } else { 1. } * radius * 0.98;
        kit::bar(window, cx + x * k - 0.75, cy - y * k - 0.75, 1.5, 1.5, 0., dot);
    }
    draw_correlation(window, Rect { x: r.x + 6., y: r.bottom() - meter_h - 3., w: r.w - 12., h: meter_h }, s.corr, false);
}

fn paint_levels(window: &mut Window, cx: &mut gpui::App, r: Rect, s: &Snap) {
    let pal = p();
    let (bars, text) = r.inset(6.).split_left(50.);
    draw_level_bars(window, cx, bars, s.peak_db, s.rms_db);
    let value = match s.mode {
        LoudMode::Momentary => s.loud.momentary,
        LoudMode::ShortTerm => s.loud.short_term,
        LoudMode::Integrated => s.loud.integrated,
        LoudMode::Rms => s.rms_db[0].max(s.rms_db[1]),
    };
    let shown = if value > -99. { format!("{value:.1}") } else { "—".to_owned() };
    let x = text.x + 8.;
    kit::bar(window, x - 4., text.y, text.w - 4., 22., r_sm(), with_alpha(pal.accent, 0.16));
    kit::txt(window, cx, &shown, x, text.y + 3., TEXT_LG, pal.text, TextAlign::Left);
    kit::txt(window, cx, s.mode.unit(), x, text.y + 26., TEXT_XS, pal.muted, TextAlign::Left);
    let peak = s.peak_db[0].max(s.peak_db[1]);
    let peak_text = if peak > -99. { format!("peak {peak:.1}") } else { "peak —".to_owned() };
    kit::txt(window, cx, &peak_text, x, text.bottom() - 14., TEXT_XS, if peak > -0.3 { pal.meter_high } else { pal.faint }, TextAlign::Left);
}

fn paint_vu(window: &mut Window, cx: &mut gpui::App, r: Rect, s: &Snap) {
    let pal = p();
    // 0 VU = −18 dBFS RMS.
    let vu = (s.rms_db[0].max(s.rms_db[1]) + 18.).clamp(-20., 3.);
    let spread = 50f32.to_radians();
    let angle = |v: f32| -spread + 2. * spread * ((v + 20.) / 23.);
    let radius = (r.h * 1.05).min(r.w * 0.62);
    let (cx0, cy0) = (r.px(0.5), r.y + radius + 10.);
    let at = |a: f32, rad: f32| point(px(cx0 + a.sin() * rad), px(cy0 - a.cos() * rad));
    let arc = |from: f32, to: f32| -> Vec<Point<Pixels>> { (0..=24).map(|i| at(angle(from + (to - from) * i as f32 / 24.), radius)).collect() };
    kit::line(window, &arc(-20., 0.), 1.5, pal.muted);
    kit::line(window, &arc(0., 3.), 2.5, pal.meter_high);
    for mark in [-20., -10., -7., -5., -3., 0., 3.] {
        let a = angle(mark);
        let color = if mark > 0. { pal.meter_high } else { pal.muted };
        kit::line(window, &[at(a, radius - 5.), at(a, radius + 1.)], 1., color);
        let label = at(a, radius - 14.);
        let text = if mark > 0. { format!("+{}", mark as i32) } else { format!("{}", mark.abs() as i32) };
        kit::txt(window, cx, &text, f32::from(label.x), f32::from(label.y) - 5., TEXT_XS, color, TextAlign::Center);
    }
    let a = angle(vu);
    kit::line(window, &[at(a, radius * 0.25), at(a, radius + 3.)], 1.5, if vu > 0. { pal.meter_high } else { pal.accent });
    kit::txt(window, cx, "VU", cx0, r.bottom() - 14., TEXT_XS, pal.faint, TextAlign::Center);
}

fn paint_scope(window: &mut Window, r: Rect, s: &Snap) {
    let pal = p();
    kit::hline(window, r, r.py(0.5), content(0.08));
    let span = ((r.w as usize) * 2).clamp(64, SCOPE_LEN / 2);
    if s.scope.len() < span * 2 {
        return;
    }
    // Start at a rising zero crossing so a steady tone stands still.
    let search = &s.scope[s.scope.len() - 2 * span..s.scope.len() - span];
    let offset = search.windows(2).position(|w| w[0] <= 0. && w[1] > 0.).unwrap_or(0);
    let start = s.scope.len() - 2 * span + offset;
    let view = &s.scope[start..start + span];
    let peak = view.iter().fold(0.05f32, |m, v| m.max(v.abs()));
    let n = (r.w as usize).max(2);
    let pts: Vec<Point<Pixels>> = (0..n)
        .map(|i| {
            let v = view[(i * (span - 1)) / (n - 1)];
            r.pt(i as f32 / (n - 1) as f32, 0.5 - v / peak * 0.44)
        })
        .collect();
    kit::line(window, &pts, 1.25, pal.accent);
}

// ───────────────────────────── View ─────────────────────────────

/// The strip itself, or `None` while it is closed.
pub fn render(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !ws.meters.open {
        return None;
    }
    let pal = p();
    let h = ws.meters.height.clamp(MIN_H, MAX_H);
    let snap = Rc::new(ws.meters.snap());
    let mut row = div().size_full().flex().gap(px(1.));
    for (i, module) in ws.meters.modules.clone().into_iter().enumerate() {
        let s = snap.clone();
        let region = (module == Module::Spectrum).then(|| ws.meters.spectrum_at.clone());
        let plot = canvas(
            move |b, _, _| {
                if let Some(region) = &region {
                    region.set(b);
                }
            },
            move |b, _, window, cx| {
                let r = Rect::of(b);
                match module {
                    Module::Waveform => paint_waveform(window, r, &s),
                    Module::Spectrogram => paint_spectrogram(window, r, &s),
                    Module::Spectrum => paint_spectrum(window, cx, r, &s),
                    Module::Stereo => paint_stereo(window, r, &s),
                    Module::Levels => paint_levels(window, cx, r, &s),
                    Module::Vu => paint_vu(window, cx, r, &s),
                    Module::Scope => paint_scope(window, r, &s),
                }
            },
        )
        .size_full();
        let mut cell = div()
            .id(("meter-module", i))
            .h_full()
            .bg(pal.well)
            .overflow_hidden()
            .tooltip(widgets::tip(module.label(), Some("Right-click for options")))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.meters.pop = Some((e.position, PopTarget::Module(i)));
                    cx.notify();
                }),
            )
            .child(plot);
        cell = match module.width(h) {
            Some(w) => cell.flex_none().w(px(w)),
            None => cell.flex_1().min_w(px(160.)),
        };
        if module == Module::Spectrum {
            cell = cell
                .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                    this.meters.hover = Some(f32::from(e.position.x));
                    cx.notify();
                }))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if !hovered {
                        this.meters.hover = None;
                        cx.notify();
                    }
                }));
        }
        if module == Module::Levels {
            cell = cell.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                let now = LoudMode::ALL.iter().position(|m| *m == this.meters.loud_mode).unwrap_or(0);
                this.meters.loud_mode = LoudMode::ALL[(now + 1) % LoudMode::ALL.len()];
                cx.notify();
            }));
        }
        row = row.child(cell);
    }
    let add = div().flex_none().w(px(28.)).h_full().flex().items_center().justify_center().bg(pal.well).child(
        widgets::sized_icon_button("meter-add", Icon::Add, false, 20., 12.)
            .tooltip(widgets::tip("Add a meter module", None))
            .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                this.meters.pop = Some((e.position(), PopTarget::Add));
                cx.notify();
            })),
    );
    let grip = div()
        .id("meter-grip")
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(5.))
        .cursor_ns_resize()
        .hover(|s| s.bg(with_alpha(p().accent, 0.5)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if e.click_count >= 2 {
                    this.meters.height = 112.;
                } else {
                    this.meters.resize = Some((f32::from(e.position.y), this.meters.height));
                }
                cx.notify();
            }),
        );
    let strip = div()
        .relative()
        .flex_none()
        .h(px(h))
        .mx(px(SP1))
        .mt(px(SP1))
        .rounded(px(r_lg()))
        .border_1()
        .border_color(border())
        .bg(border())
        .overflow_hidden()
        .child(row.child(add))
        .child(grip);
    let id = gpui::ElementId::Name(format!("meter-strip-{}", ws.meters.opened).into());
    Some(crate::motion::rise_in(id, strip, -8.).into_any_element())
}

/// Layers above the workspace: the resize capture and the module menu.
pub fn overlay(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    let mut out = Vec::new();
    if ws.meters.resize.is_some() {
        let weak = cx.entity().downgrade();
        out.push(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let w = weak.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            let _ = w.update(cx, |this, cx| {
                                if let Some((y0, h0)) = this.meters.resize {
                                    this.meters.height = (h0 + f32::from(e.position.y) - y0).clamp(MIN_H, MAX_H);
                                    cx.notify();
                                }
                            });
                        }
                    });
                    let w = weak.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            let _ = w.update(cx, |this, cx| {
                                this.meters.resize = None;
                                cx.notify();
                            });
                        }
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        );
    }
    let Some((at, target)) = ws.meters.pop else { return out };
    let close = |this: &mut Workspace, cx: &mut Context<Workspace>| {
        this.meters.pop = None;
        cx.notify();
    };
    let mut card = widgets::menu_card("meter-menu", 216.);
    match target {
        PopTarget::Add => {
            card = card.child(menu_header("SOURCE")).child(
                menu_item("meter-follow", Some(Icon::Headphones), "Analyse the selected track", None, ws.meters.follow_track).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.meters.follow_track = !this.meters.follow_track;
                        close(this, cx);
                    },
                )),
            );
            card = card.child(menu_header("MODULES"));
            for (k, m) in Module::ALL.into_iter().enumerate() {
                let present = ws.meters.modules.contains(&m);
                card = card.child(menu_item(("meter-add-item", k), None, m.label(), None, present).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(i) = this.meters.modules.iter().position(|x| *x == m) {
                        this.meters.modules.remove(i);
                    } else {
                        this.meters.modules.push(m);
                    }
                    close(this, cx);
                })));
            }
        }
        PopTarget::Module(i) => {
            let Some(module) = ws.meters.modules.get(i).copied() else {
                ws.meters.pop = None;
                return out;
            };
            card = card.child(menu_header(module.label().to_uppercase()));
            if module == Module::Spectrum {
                card = card.child(menu_item("meter-ms", None, "Show side signal (Mid / Side)", None, ws.meters.mid_side).on_click(cx.listener(move |this, _, _, cx| {
                    this.meters.mid_side = !this.meters.mid_side;
                    close(this, cx);
                })));
            }
            if module == Module::Levels {
                for (k, mode) in LoudMode::ALL.into_iter().enumerate() {
                    card = card.child(menu_item(("meter-mode", k), None, mode.label(), None, ws.meters.loud_mode == mode).on_click(cx.listener(move |this, _, _, cx| {
                        this.meters.loud_mode = mode;
                        close(this, cx);
                    })));
                }
                card = card.child(menu_item("meter-reset", Some(Icon::Refresh), "Reset integrated loudness", None, false).on_click(cx.listener(move |this, _, _, cx| {
                    this.meters.reset_loudness();
                    close(this, cx);
                })));
            }
            card = card
                .child(menu_item("meter-left", Some(Icon::ChevronLeft), "Move left", None, false).on_click(cx.listener(move |this, _, _, cx| {
                    if i > 0 && i < this.meters.modules.len() {
                        this.meters.modules.swap(i, i - 1);
                    }
                    close(this, cx);
                })))
                .child(menu_item("meter-right", Some(Icon::ChevronRight), "Move right", None, false).on_click(cx.listener(move |this, _, _, cx| {
                    if i + 1 < this.meters.modules.len() {
                        this.meters.modules.swap(i, i + 1);
                    }
                    close(this, cx);
                })))
                .child(menu_item("meter-remove", Some(Icon::Trash), "Remove module", None, false).on_click(cx.listener(move |this, _, _, cx| {
                    if i < this.meters.modules.len() {
                        this.meters.modules.remove(i);
                    }
                    close(this, cx);
                })));
        }
    }
    out.push(
        deferred(
            anchored().position(at).snap_to_window_with_margin(px(SP2)).child(
                div()
                    .id("meter-pop-layer")
                    .occlude()
                    .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(this, cx)))
                    .child(card),
            ),
        )
        .with_priority(2)
        .into_any_element(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, frames: usize, sr: f32) -> Vec<[f32; 2]> {
        (0..frames).map(|n| (std::f32::consts::TAU * freq * n as f32 / sr).sin() * amp).map(|v| [v, v]).collect()
    }

    #[test]
    fn a_sine_reads_its_level_frequency_and_phase() {
        let mut a = Analysis::new(48_000);
        // 1 kHz at −18 dBFS peak, mono.
        a.process(&sine(1000., 0.1259, 96_000, 48_000.));
        assert!((db(a.peak[0]) + 18.).abs() < 0.3, "peak {}", db(a.peak[0]));
        assert!((db(a.rms_sq[0].sqrt()) + 21.).abs() < 0.4, "rms {}", db(a.rms_sq[0].sqrt()));
        assert!(a.correlation() > 0.99);
        let bands = a.mid.bands();
        let loudest = (0..bands.len()).max_by(|x, y| bands[*x].total_cmp(&bands[*y])).unwrap();
        let f = band_freq(loudest, bands.len());
        assert!((f / 1000.).log2().abs() < 0.08, "loudest band at {f} Hz");
        // Momentary loudness of a −18 dBFS sine is about −18 LUFS − 3 dB + 3 dB (two channels).
        let m = a.lufs.reading().momentary;
        assert!((-23.0..=-17.0).contains(&m), "momentary {m}");
    }

    #[test]
    fn out_of_phase_reads_negative_correlation() {
        let mut a = Analysis::new(48_000);
        let frames: Vec<[f32; 2]> = sine(300., 0.5, 24_000, 48_000.).into_iter().map(|[l, _]| [l, -l]).collect();
        a.process(&frames);
        assert!(a.correlation() < -0.99);
    }

    #[test]
    fn note_names() {
        assert_eq!(note_name(440.), "A4 +0 cents");
        assert_eq!(note_name(55.), "A1 +0 cents");
        assert!(note_name(59.62).starts_with("A#1 +"));
    }

    #[test]
    fn strip_pauses_the_engine_tap_when_closed() {
        let mut engine = crate::bridge::StubEngine::default();
        let mut strip = MeterStrip::default();
        strip.tick(&mut engine, None);
        assert!(!strip.engine_on && strip.loudness().is_none());
        strip.toggle();
        strip.tick(&mut engine, None);
        assert!(strip.engine_on && strip.loudness().is_some());
        strip.toggle();
        strip.tick(&mut engine, None);
        assert!(!strip.engine_on);
    }
}
