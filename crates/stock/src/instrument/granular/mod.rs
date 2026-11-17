//! `instrument.granular` — a polyphonic granular synthesiser.
//!
//! A cloud of short, windowed grains is read from a **source** and played by
//! MIDI voices. Three sources:
//!
//! * **Sample**: a WAV (`extra.path` in the state, loaded with the same
//!   reader as the sampler);
//! * **Live**: the device's *sidechain* input is recorded continuously into a
//!   ring buffer (up to 8 s); **Freeze** stops the recording, and the grains
//!   keep playing from what was captured. Freezing can never click: grains
//!   are windowed and read from fixed ring positions, and the recording stops
//!   between two samples. A frozen capture is saved with the project;
//! * **Built-in**: six synthesised textures (so the device sounds out of the
//!   box and in every preset).
//!
//! # The grain engine
//! Every voice owns a grain scheduler. In **Free** mode grains are triggered
//! by a phase accumulator at `Density` Hz (a threshold randomised by
//! `Timing Jitter`, Poisson-ish at 100 %), so changing the density takes
//! effect on the very next grain; in **Sync** modes they are locked to the
//! transport grid (the grid of the song position while playing, of the note
//! start when stopped), `Grains per Step` per division. Each grain draws, in a
//! fixed order whatever the settings, a random size, position, pitch, pan and
//! direction, so turning a spread to zero never reshuffles the rest.
//!
//! * **Position** maps the grain's source segment over the part of the
//!   source where it fits, so position 100 % is "the last grain that still
//!   fits", not a pile-up at the end. **Scan** moves it (speed and/or LFO);
//!   **Spray** randomises it.
//! * **Pitch** = key tracking + `Pitch` + random `Pitch Spread` (+ bend),
//!   optionally snapped to a scale (`Quantize`, `Scale`, `Scale Root`; the
//!   editor can copy the project's scale into them).
//! * **Windows** (Hann, Tukey, Gauss, Triangle, Blackman, Sine) are all zero
//!   at both ends; *Window Shape* moves the peak (percussive … swell).
//! * **Anti-aliasing**: grains transposed up read from a mip chain
//!   low-passed by half-band filters (see [`source`]).
//! * **Level**: each voice is normalised by the expected grain overlap
//!   (`density × size × mean window power`) so density and size can be swept
//!   without the level running away (Gillet's Clouds and the spatial-granular
//!   literature both compensate by overlap; power sum, hence the square root).
//! * At most [`MAX_GRAINS`] grains play at once, shared fairly between
//!   voices; when the pool is full a new grain is simply not started.
//!
//! # Determinism
//! The random stream of a note is seeded from `Seed`, the note's transport
//! position (song position + sample offset) and its key, never from a
//! free-running clock, and all scheduling is in sample time, so the same
//! project renders the same cloud live and in an export, whatever the block
//! size. Pitch bend and sidechain content are inputs like any other.
//!
//! References: C. Roads, *Microsound* (MIT Press, 2001); B. Truax, "Real-time
//! granular synthesis with a digital signal processor", *Computer Music
//! Journal* 12(2), 1988; the open-source Mutable Instruments *Clouds*
//! (É. Gillet) for the freeze/overlap/density behaviour; F. Harris, "On the
//! use of windows for harmonic analysis with the DFT", *Proc. IEEE* 66(1),
//! 1978, for the windows.
//!
//! # Meter layout (`meter()`, 264 values)
//! `[0]` voices, `[1]` grains, `[2]` output peak, `[3]` position of the last
//! grain (0..1), `[4]` frozen (0/1), `[5]` source in use (0 sample, 1 live,
//! 2 built-in), `[6]` source seconds, `[7]` live fill (0..1), `[8..72)` 64
//! waveform peaks of the source (0..1), `[72..264)` 64 grains as
//! `(position 0..1, age 0..1, pan -1..1)`; unused grains are `(-1, 0, 0)`.

pub mod builtin;
mod params;
mod source;
mod window;

pub use params::*;

use crate::common::{ParamStore, Segmented, drive, load_state, pan_gains, sanitize, save_state, sidechain_frame, write_meter};
use crate::instrument::sample::{SampleData, load_wav};
use crate::midi::theory::in_mask;
use aitrack_dsp::envelope::Adsr;
use aitrack_dsp::lfo::division_beats;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{DcBlocker, SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, PluginInfo, ProcessContext, Processor, db_to_gain};
use serde_json::{Value, json};
use source::{LIVE_GUARD, MipBuf, level_pos};
use window::Windows;

pub const ID: &str = "instrument.granular";
/// Voices a player can use (`Polyphony` ≤ this).
pub const MAX_VOICES: usize = 8;
/// Voice slots: the extra ones hold voices that are fading out after being
/// stolen, so a new note starts on its own sample.
const SLOTS: usize = MAX_VOICES + 4;
/// Grains playing at once, in total.
pub const MAX_GRAINS: usize = 128;
const CHUNK: usize = 32;
const MIN_GRAIN: u32 = 32;
const LIVE_SECS_MAX: f64 = 8.0;
const LIVE_SECS_CAP: f64 = 11.0;
const CTL_PERIOD: u32 = 32;
const METER_GRAINS: usize = 64;
const METER_WAVE: usize = 64;
/// Meter values written by [`Processor::meter`].
pub const METER_LEN: usize = 8 + METER_WAVE + METER_GRAINS * 3;

#[derive(Clone, Copy)]
struct Grain {
    active: bool,
    mono: bool,
    voice: u8,
    src: u8,
    win: u8,
    delay: u32,
    rem: u32,
    /// Level-0 absolute read position and signed step per output sample.
    t0: f64,
    step: f64,
    bend0: f32,
    u: f32,
    du: f32,
    peak: f32,
    s1: f32,
    s2: f32,
    gl: f32,
    gr: f32,
    disp_pos: f32,
    pan: f32,
}

impl Grain {
    const IDLE: Grain = Grain {
        active: false,
        mono: true,
        voice: 0,
        src: 0,
        win: 0,
        delay: 0,
        rem: 0,
        t0: 0.0,
        step: 1.0,
        bend0: 1.0,
        u: 0.0,
        du: 0.0,
        peak: 0.5,
        s1: 1.0,
        s2: 1.0,
        gl: 1.0,
        gr: 1.0,
        disp_pos: 0.0,
        pan: 0.0,
    };
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    released: bool,
    fading: bool,
    key: u8,
    vel_gain: f32,
    age: u64,
    env: Adsr,
    filt: [SvfState; 2],
    coeffs: SvfCoeffs,
    fc: f32,
    ctl: u32,
    rng: u64,
    /// Samples since note-on.
    t: u64,
    gphase: f64,
    gthr: f64,
    alt: bool,
    scan: f64,
    lfo: f64,
    norm: Smoother,
    fade: f32,
}

impl Voice {
    fn new(sr: f64) -> Self {
        Self {
            active: false,
            released: false,
            fading: false,
            key: 60,
            vel_gain: 1.0,
            age: 0,
            env: Adsr::new(sr),
            filt: [SvfState::default(); 2],
            coeffs: SvfCoeffs::passthrough(),
            fc: 20_000.0,
            ctl: 0,
            rng: 1,
            t: 0,
            gphase: 1.0,
            gthr: 1.0,
            alt: false,
            scan: 0.0,
            lfo: 0.0,
            norm: Smoother::new(1.0),
            fade: 1.0,
        }
    }
}

#[inline]
fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform in `[0, 1)`.
#[inline]
fn rnd(s: &mut u64) -> f32 {
    (splitmix(s) >> 40) as f32 / 16_777_216.0
}

/// Derived settings (refreshed whenever a parameter changes).
#[derive(Clone, Copy)]
struct Cfg {
    source: usize,
    builtin: usize,
    frozen: bool,
    live_len: f64,
    position: f64,
    scan_rate: f64,
    lfo_rate: f64,
    lfo_depth: f64,
    spray: f32,
    size_ms: f32,
    size_rand: f32,
    density: f64,
    sync: Option<f64>,
    per_step: f64,
    jitter: f32,
    window: usize,
    peak: f32,
    s1: f32,
    s2: f32,
    win_power: f32,
    dir: usize,
    pitch: f32,
    spread: f32,
    quant: usize,
    mask: u16,
    root_key: f32,
    key_track: f32,
    pan_spread: f32,
    seed: u32,
    vel_sens: f32,
    filter: usize,
    cutoff: f32,
    reso: f32,
    env_amt: f32,
    f_key: f32,
    poly: usize,
    bend_range: f32,
}

pub struct Granular {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    windows: Box<Windows>,
    file: Option<Box<MipBuf>>,
    file_path: Option<String>,
    live: MipBuf,
    live_ready: bool,
    saved_live: Option<(f64, Vec<f32>)>,
    recording: bool,
    voices: [Voice; SLOTS],
    grains: [Grain; MAX_GRAINS],
    c: Cfg,
    bend: f32,
    age: u64,
    block_pos0: i64,
    local: usize,
    bpm: f64,
    playing: bool,
    vol: Smoother,
    width: Smoother,
    dc: [DcBlocker; 2],
    peak: f32,
    last_pos: f32,
    started: u64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Granular::new())
}

impl Default for Granular {
    fn default() -> Self {
        Self::new()
    }
}

fn default_cfg() -> Cfg {
    Cfg {
        source: SOURCE_BUILTIN,
        builtin: 0,
        frozen: false,
        live_len: 4.0 * 48_000.0,
        position: 0.3,
        scan_rate: 0.0,
        lfo_rate: 0.3,
        lfo_depth: 0.0,
        spray: 0.15,
        size_ms: 100.0,
        size_rand: 0.0,
        density: 20.0,
        sync: None,
        per_step: 1.0,
        jitter: 0.25,
        window: 0,
        peak: 0.5,
        s1: 1.0,
        s2: 1.0,
        win_power: 0.375,
        dir: 0,
        pitch: 0.0,
        spread: 0.0,
        quant: 0,
        mask: 0xFFF,
        root_key: 60.0,
        key_track: 1.0,
        pan_spread: 0.5,
        seed: 0,
        vel_sens: 0.5,
        filter: 0,
        cutoff: 20_000.0,
        reso: 0.0,
        env_amt: 0.0,
        f_key: 0.0,
        poly: 4,
        bend_range: 2.0,
    }
}

/// Nearest in-scale note to `note` (ties go down).
fn quantize(mask: u16, note: f32) -> f32 {
    let n = note.round() as i32;
    if in_mask(mask, n) {
        return n as f32;
    }
    for d in 1..=6 {
        if in_mask(mask, n - d) {
            return (n - d) as f32;
        }
        if in_mask(mask, n + d) {
            return (n + d) as f32;
        }
    }
    n as f32
}

fn note_seed(seed: u32, pos: i64, key: u8) -> u64 {
    let mut s = (seed as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pos as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ ((key as u64) << 48);
    let a = splitmix(&mut s);
    a ^ splitmix(&mut s).rotate_left(17)
}

/// Where grains may read right now.
#[derive(Clone, Copy)]
struct Region {
    src: u8,
    lo: f64,
    hi: f64,
    mono: bool,
    ratio: f64,
}

impl Granular {
    pub fn new() -> Self {
        let params = params();
        let sr = 48_000.0;
        // Synthesise the built-in sources now (once per process), never on the
        // audio thread.
        let _ = builtin::sources();
        let mut g = Self {
            info: PluginInfo {
                id: ID,
                name: "Granular",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["granular", "texture", "pad", "freeze", "sampler", "live"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr,
            windows: Box::new(Windows::new()),
            file: None,
            file_path: None,
            live: MipBuf::new_live(64, sr),
            live_ready: false,
            saved_live: None,
            recording: false,
            voices: [Voice::new(sr); SLOTS],
            grains: [Grain::IDLE; MAX_GRAINS],
            c: default_cfg(),
            bend: 0.0,
            age: 0,
            block_pos0: 0,
            local: 0,
            bpm: 120.0,
            playing: false,
            vol: Smoother::new(1.0),
            width: Smoother::new(1.0),
            dc: [DcBlocker::new(8.0, sr); 2],
            peak: 0.0,
            last_pos: 0.0,
            started: 0,
        };
        g.prepare(sr, 512);
        g
    }

    /// Decode a WAV and use it as the *Sample* source (control thread). Also
    /// switches `Source` to *Sample*.
    pub fn load_wav(&mut self, path: &str) -> Result<(), String> {
        let s = load_wav(path)?;
        self.set_sample_data(s);
        let n = self.info.params[p::SOURCE as usize].to_normalized(SOURCE_SAMPLE as f32);
        self.params.set(p::SOURCE, n);
        Ok(())
    }

    /// Use interleaved audio as the *Sample* source (control thread).
    pub fn set_sample(&mut self, interleaved: &[f32], channels: usize, sample_rate: f64) {
        self.set_sample_data(SampleData::from_interleaved(interleaved, channels, sample_rate));
    }

    pub fn set_sample_data(&mut self, data: SampleData) {
        self.kill_all();
        self.file_path = data.path.clone();
        if data.frames() < 2 {
            self.file = None;
        } else {
            let l: Vec<f32> = (0..data.frames()).map(|i| data.read(0, i as f64)).collect();
            let m = if data.channels() > 1 {
                let r: Vec<f32> = (0..data.frames()).map(|i| data.read(1, i as f64)).collect();
                MipBuf::from_planar(&l, Some(&r), data.sample_rate)
            } else {
                MipBuf::from_planar(&l, None, data.sample_rate)
            };
            self.file = Some(Box::new(m));
        }
        self.params.mark_dirty();
    }

    pub fn has_sample(&self) -> bool {
        self.file.is_some()
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Grains started since creation (for tests and diagnostics).
    pub fn grains_started(&self) -> u64 {
        self.started
    }

    pub fn active_grains(&self) -> usize {
        self.grains.iter().filter(|g| g.active).count()
    }

    fn kill_all(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
        for g in &mut self.grains {
            g.active = false;
        }
    }

    fn update(&mut self) {
        let pr = &self.params;
        let sr = self.sr;
        let mut c = self.c;
        c.source = pr.choice(p::SOURCE);
        c.builtin = pr.choice(p::BUILTIN).min(builtin::COUNT - 1);
        c.frozen = pr.on(p::FREEZE);
        c.live_len = (pr.plain(p::LIVE_LEN) as f64).clamp(0.25, LIVE_SECS_MAX) * sr;
        c.position = (pr.plain(p::POSITION) / 100.0) as f64;
        c.scan_rate = (pr.plain(p::SCAN_SPEED) / 100.0) as f64;
        c.lfo_rate = pr.plain(p::SCAN_RATE) as f64;
        c.lfo_depth = (pr.plain(p::SCAN_DEPTH) / 100.0) as f64;
        c.spray = pr.plain(p::SPRAY) / 100.0;
        c.size_ms = pr.plain(p::SIZE);
        c.size_rand = pr.plain(p::SIZE_RAND) / 100.0;
        c.density = pr.plain(p::DENSITY).clamp(0.5, 400.0) as f64;
        let s = pr.choice(p::SYNC);
        c.sync = if s == 0 { None } else { Some(division_beats(s - 1)) };
        c.per_step = pr.plain(p::SYNC_MULT).round().clamp(1.0, 8.0) as f64;
        c.jitter = pr.plain(p::JITTER) / 100.0;
        c.window = pr.choice(p::WINDOW).min(window::COUNT - 1);
        c.peak = (0.1 + 0.8 * (1.0 - pr.plain(p::WIN_SHAPE) / 100.0)).clamp(0.1, 0.9);
        (c.s1, c.s2) = window::warp_slopes(c.peak);
        c.win_power = self.windows.power(c.window);
        c.dir = pr.choice(p::DIRECTION).min(3);
        c.pitch = pr.plain(p::PITCH);
        c.spread = pr.plain(p::PITCH_SPREAD);
        c.quant = pr.choice(p::QUANT).min(2);
        let kind = pr.choice(p::SCALE).min(SCALES.len() - 1);
        c.mask = scale_mask_ext(kind, if c.quant == 2 { 0 } else { pr.choice(p::ROOT).min(11) });
        c.root_key = pr.plain(p::ROOT_KEY).round();
        c.key_track = pr.plain(p::KEY_TRACK) / 100.0;
        c.pan_spread = pr.plain(p::PAN_SPREAD) / 100.0;
        c.seed = pr.plain(p::SEED).round() as u32;
        c.vel_sens = pr.plain(p::VEL_SENS) / 100.0;
        c.filter = pr.choice(p::FILTER_TYPE).min(3);
        c.cutoff = pr.plain(p::CUTOFF);
        c.reso = pr.plain(p::RESONANCE) / 100.0;
        c.env_amt = pr.plain(p::FILTER_ENV) / 100.0;
        c.f_key = pr.plain(p::FILTER_KEY) / 100.0;
        c.poly = (pr.plain(p::POLYPHONY).round() as usize).clamp(1, MAX_VOICES);
        c.bend_range = pr.plain(p::BEND_RANGE).round();
        let (a, d, su, r) = (pr.plain(p::ATTACK), pr.plain(p::DECAY), pr.plain(p::SUSTAIN) / 100.0, pr.plain(p::RELEASE));
        for v in &mut self.voices {
            v.env.set(a, d, su, r);
        }
        self.vol.set_target(db_to_gain(pr.plain(p::VOLUME)));
        self.width.set_target(pr.plain(p::WIDTH) / 100.0);
        self.c = c;
    }

    /// Which source the grains read now: 0 sample, 1 live, 2+ built-in.
    fn active_src(&self) -> u8 {
        match self.c.source {
            SOURCE_LIVE => 1,
            SOURCE_SAMPLE if self.file.is_some() => 0,
            _ => 2 + self.c.builtin as u8,
        }
    }

    fn src_buf(&self, src: u8) -> Option<&MipBuf> {
        match src {
            0 => self.file.as_deref(),
            1 => Some(&self.live),
            k => builtin::sources().get(k as usize - 2),
        }
    }

    /// `behind`: frames of the live ring that were pushed after the moment
    /// being asked about (the rest of the current chunk).
    fn region(&self, behind: u64) -> Option<Region> {
        let src = self.active_src();
        let m = self.src_buf(src)?;
        let ratio = m.src_sr / self.sr;
        let (lo, hi) = if src == 1 {
            if !self.live_ready {
                return None;
            }
            let head = m.head().saturating_sub(behind) as f64;
            let valid = (m.valid.saturating_sub(behind) as f64).min(self.c.live_len);
            (head - valid + 48.0, head - LIVE_GUARD)
        } else {
            (0.0, m.frames as f64)
        };
        (hi - lo >= 64.0).then_some(Region { src, lo, hi, mono: m.mono, ratio })
    }

    // ----- notes ------------------------------------------------------------

    fn note_on(&mut self, key: u8, vel: f32) {
        if vel <= 0.0 {
            self.note_off(key);
            return;
        }
        let live_notes = self.voices.iter().filter(|v| v.active && !v.fading).count();
        if live_notes >= self.c.poly {
            // Fade the best victim out; the new note takes its own slot.
            let victim = (0..SLOTS)
                .filter(|&i| self.voices[i].active && !self.voices[i].fading)
                .min_by_key(|&i| (!self.voices[i].released as u8, self.voices[i].age));
            if let Some(i) = victim {
                self.voices[i].fading = true;
            }
        }
        let Some(slot) = (0..SLOTS).find(|&i| !self.voices[i].active) else { return };
        self.start_voice(slot, key, vel);
    }

    fn start_voice(&mut self, vi: usize, key: u8, vel: f32) {
        self.age += 1;
        let pos = self.block_pos0 + self.local as i64;
        let seed = note_seed(self.c.seed, pos, key);
        let norm = self.norm_target();
        let sens = self.c.vel_sens;
        let v = &mut self.voices[vi];
        v.active = true;
        v.released = false;
        v.fading = false;
        v.fade = 1.0;
        v.key = key;
        let vel = vel.clamp(0.0, 1.0);
        v.vel_gain = 1.0 - sens + sens * vel * vel;
        v.age = self.age;
        v.env.retrigger();
        v.filt = [SvfState::default(); 2];
        v.fc = self.c.cutoff;
        v.ctl = 0;
        v.rng = seed;
        v.t = 0;
        v.gphase = 1.0;
        v.gthr = 1.0;
        v.alt = false;
        v.scan = 0.0;
        v.lfo = 0.0;
        v.norm.snap(norm);
        for g in &mut self.grains {
            if g.voice as usize == vi {
                g.active = false;
            }
        }
    }

    fn note_off(&mut self, key: u8) {
        for v in &mut self.voices {
            if v.active && v.key == key && !v.released && !v.fading {
                v.env.gate_off();
                v.released = true;
            }
        }
    }

    // ----- grains -----------------------------------------------------------

    /// Overlap compensation target for a voice: the cloud's expected power
    /// is `density × size × window power`, kept at a constant by `1/√`.
    fn norm_target(&self) -> f32 {
        let c = &self.c;
        let voices = self.voices.iter().filter(|v| v.active && !v.fading).count().max(1);
        let cap = (MAX_GRAINS / voices).max(4) as f64;
        let dens = match c.sync {
            Some(b) => c.per_step * self.bpm.max(1.0) / (60.0 * b),
            None => c.density,
        };
        let overlap = (dens * c.size_ms as f64 * 0.001).min(cap * 0.9) * c.win_power as f64;
        (0.6 / overlap.max(1e-6)).sqrt().min(1.0) as f32
    }

    /// Start a grain for voice `vi`. `delay` is in samples from the chunk
    /// start. All random draws happen first and always.
    fn spawn(&mut self, vi: usize, delay: u32, counts: &mut [u32; SLOTS], step_len: f64, chunk_n: usize) {
        let behind = chunk_n.saturating_sub(delay as usize) as u64;
        let c = self.c;
        let sr = self.sr;
        let region = self.region(behind);
        let bend_semis = self.bend * c.bend_range;
        let active_voices = self.voices.iter().filter(|v| v.active && !v.fading).count().max(1);
        let v = &mut self.voices[vi];
        let (r_size, r_pos, r_pitch, r_pan, r_dir, r_jit) = (rnd(&mut v.rng), rnd(&mut v.rng), rnd(&mut v.rng), rnd(&mut v.rng), rnd(&mut v.rng), rnd(&mut v.rng));
        let mut delay = delay;
        if c.sync.is_some() {
            delay += (r_jit * c.jitter * 0.5 * step_len as f32) as u32;
        } else {
            v.gthr = 1.0 + (c.jitter * 0.9 * (r_jit * 2.0 - 1.0)) as f64;
        }
        let alt_flip = v.alt;
        v.alt = !alt_flip;
        let key = v.key;
        let off_s = delay as f64 / sr;
        let scan = v.scan + if c.frozen { 0.0 } else { c.scan_rate * off_s };
        let lfo = v.lfo + c.lfo_rate * off_s;
        let Some(rg) = region else { return };

        // Direction.
        let dir = match c.dir {
            0 => 1.0,
            1 => -1.0,
            2 => {
                if alt_flip {
                    -1.0
                } else {
                    1.0
                }
            }
            _ => {
                if r_dir < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        };
        // Pitch.
        let offset = c.pitch + c.spread * (r_pitch * 2.0 - 1.0);
        let mut semis = c.key_track * (key as f32 - c.root_key) + offset;
        match c.quant {
            1 => semis = quantize(c.mask, c.root_key + semis) - c.root_key,
            2 => semis = c.key_track * (key as f32 - c.root_key) + quantize(c.mask, 60.0 + offset) - 60.0,
            _ => {}
        }
        let bend_factor = 2f64.powf(bend_semis as f64 / 12.0);
        let semis = semis.clamp(-60.0, 60.0);
        let rate = rg.ratio * 2f64.powf(semis as f64 / 12.0) * bend_factor;
        // Size, shortened to what the source region can hold.
        let size_ms = (c.size_ms * 2f32.powf(c.size_rand * (r_size * 2.0 - 1.0) * 1.5)).clamp(1.0, 2000.0);
        let mut n = ((size_ms as f64 * 0.001 * sr).round() as u32).max(MIN_GRAIN);
        let l = rg.hi - rg.lo;
        let fit = (l / rate).floor();
        if fit < MIN_GRAIN as f64 {
            return;
        }
        n = n.min(fit as u32);
        // Position.
        let base = c.position + c.lfo_depth * 0.5 * (std::f64::consts::TAU * lfo).sin();
        let mut pos = if c.scan_rate != 0.0 { (base + scan).rem_euclid(1.0) } else { base.clamp(0.0, 1.0) };
        pos = (pos + (c.spray * (r_pos - 0.5)) as f64).clamp(0.0, 1.0);
        let span = n as f64 * rate;
        let half = span * 0.5;
        let center = rg.lo + half + pos * (l - span).max(0.0);
        let (t0, step) = if dir > 0.0 { (center - half, rate) } else { (center + half, -rate) };
        // Pan.
        let pan = c.pan_spread * (r_pan * 2.0 - 1.0);
        let (mut gl, mut gr) = pan_gains(pan);
        if !rg.mono {
            gl *= std::f32::consts::SQRT_2;
            gr *= std::f32::consts::SQRT_2;
        }
        // Slot.
        let cap = (MAX_GRAINS / active_voices).max(4) as u32;
        if counts[vi] >= cap {
            return;
        }
        let Some(slot) = self.grains.iter().position(|g| !g.active) else { return };
        counts[vi] += 1;
        self.started += 1;
        let disp = if l > span { ((center - half - rg.lo) / (l - span)).clamp(0.0, 1.0) as f32 } else { 0.5 };
        self.last_pos = disp;
        self.grains[slot] = Grain {
            active: true,
            mono: rg.mono,
            voice: vi as u8,
            src: rg.src,
            win: c.window as u8,
            delay,
            rem: n,
            t0,
            step,
            bend0: bend_factor as f32,
            u: 0.0,
            du: 1.0 / n as f32,
            peak: c.peak,
            s1: c.s1,
            s2: c.s2,
            gl,
            gr,
            disp_pos: disp,
            pan,
        };
    }

    /// Render `n ≤ CHUNK` frames of the whole instrument into `out`.
    fn chunk(&mut self, out: &mut [[f32; CHUNK]; 2], n: usize, abs: i64) {
        let sr = self.sr;
        let mut counts = [0u32; SLOTS];
        for g in &self.grains {
            if g.active {
                counts[g.voice as usize] += 1;
            }
        }
        let norm = self.norm_target();
        let nf = n as f64;

        // 1. Schedule grains.
        let step_len = match self.c.sync {
            Some(b) => (b * 60.0 / self.bpm.max(1.0) * sr / self.c.per_step).max(8.0),
            None => 0.0,
        };
        for vi in 0..SLOTS {
            if !self.voices[vi].active || self.voices[vi].fading {
                continue;
            }
            self.voices[vi].norm.set_target(norm);
            if self.c.sync.is_some() {
                let a0 = if self.playing { abs } else { self.voices[vi].t as i64 };
                let k0 = (a0 as f64 / step_len).ceil() as i64;
                for k in (k0..).take(64) {
                    let tb = k as f64 * step_len;
                    if tb >= (a0 + n as i64) as f64 {
                        break;
                    }
                    let off = (tb - a0 as f64).max(0.0).ceil() as u32;
                    self.spawn(vi, off, &mut counts, step_len, n);
                }
            } else {
                let rate = self.c.density / sr;
                let mut pos = 0.0f64;
                for _ in 0..64 {
                    let v = &self.voices[vi];
                    let need = ((v.gthr - v.gphase) / rate).max(0.0);
                    if pos + need < nf {
                        pos += need;
                        self.voices[vi].gphase = 0.0;
                        self.spawn(vi, pos as u32, &mut counts, 0.0, n);
                    } else {
                        self.voices[vi].gphase += (nf - pos) * rate;
                        break;
                    }
                }
            }
            let frozen = self.c.frozen;
            let (sr_rate, lfo_rate, scan_rate) = (sr, self.c.lfo_rate, self.c.scan_rate);
            let v = &mut self.voices[vi];
            if !frozen {
                v.scan += scan_rate * nf / sr_rate;
                v.scan -= v.scan.floor();
            }
            v.lfo += lfo_rate * nf / sr_rate;
            v.lfo -= v.lfo.floor();
        }

        // 2. Mix the grains into per-voice buses.
        let mut bus = [[[0.0f32; CHUNK]; 2]; SLOTS];
        let bend_now = 2f64.powf((self.bend * self.c.bend_range) as f64 / 12.0) as f32;
        {
            let Granular { grains, file, live, windows, voices, .. } = self;
            let sources = builtin::sources();
            for g in grains.iter_mut().filter(|g| g.active) {
                if !voices[g.voice as usize].active {
                    g.active = false;
                    continue;
                }
                let m: &MipBuf = match g.src {
                    0 => match file.as_deref() {
                        Some(m) => m,
                        None => {
                            g.active = false;
                            continue;
                        }
                    },
                    1 => &*live,
                    k => &sources[(k as usize - 2).min(sources.len() - 1)],
                };
                mix_grain(g, m, windows, &mut bus[g.voice as usize], n, bend_now);
            }
        }

        // 3. Voices: overlap gain, filter, envelope; sum.
        let mut mix = [[0.0f32; CHUNK]; 2];
        let c = self.c;
        let mut dead = [false; SLOTS];
        for vi in 0..SLOTS {
            let v = &mut self.voices[vi];
            if !v.active {
                continue;
            }
            for s in 0..n {
                if v.ctl == 0 {
                    v.ctl = CTL_PERIOD;
                    if c.filter > 0 {
                        let kt = c.f_key * (v.key as f32 - 60.0) / 12.0;
                        let target = c.cutoff * 2f32.powf(c.env_amt * v.env.value() * 6.0 + kt);
                        v.fc += (target.clamp(20.0, 20_000.0) - v.fc) * 0.4;
                        let fc = v.fc.clamp(20.0, (0.45 * sr as f32).min(20_000.0));
                        let q = std::f32::consts::FRAC_1_SQRT_2 * (20.0f32 / std::f32::consts::FRAC_1_SQRT_2).powf(c.reso);
                        let mode = match c.filter {
                            1 => SvfMode::Lowpass,
                            2 => SvfMode::Highpass,
                            _ => SvfMode::Bandpass,
                        };
                        v.coeffs = SvfCoeffs::new(mode, fc, q, 0.0, sr);
                    }
                }
                v.ctl -= 1;
                let ng = v.norm.next();
                let (mut a, mut b) = (bus[vi][0][s] * ng, bus[vi][1][s] * ng);
                if c.filter > 0 {
                    a = v.filt[0].process(&v.coeffs, a);
                    b = v.filt[1].process(&v.coeffs, b);
                }
                let env = v.env.next();
                if v.fading {
                    v.fade = (v.fade - 1.0 / (0.002 * sr as f32).max(8.0)).max(0.0);
                }
                let g = env * v.vel_gain * v.fade;
                mix[0][s] += a * g;
                mix[1][s] += b * g;
                v.t += 1;
                if !v.env.is_active() || v.fade <= 0.0 {
                    dead[vi] = true;
                }
            }
            if !(v.filt[0].is_finite() && v.filt[1].is_finite()) {
                v.filt = [SvfState::default(); 2];
            }
            if dead[vi] {
                v.active = false;
            }
        }
        for vi in 0..SLOTS {
            if dead[vi] {
                for g in &mut self.grains {
                    if g.voice as usize == vi {
                        g.active = false;
                    }
                }
            }
        }

        // 4. Output stage: width, volume, DC block, soft ceiling.
        let mut peak = self.peak * (-(n as f32) / (0.15 * sr as f32)).exp();
        for s in 0..n {
            let vol = self.vol.next();
            let w = self.width.next();
            let (l, r) = (mix[0][s], mix[1][s]);
            let mid = 0.5 * (l + r);
            let side = 0.5 * (l - r) * w;
            let l = self.dc[0].process((mid + side) * vol);
            let r = self.dc[1].process((mid - side) * vol);
            let (l, r) = (soft_ceiling(sanitize(l)), soft_ceiling(sanitize(r)));
            out[0][s] = l;
            out[1][s] = r;
            peak = peak.max(l.abs()).max(r.abs());
        }
        self.peak = peak;
    }
}

/// Transparent below 0.9, a smooth tanh knee above, never beyond ±1.
#[inline]
fn soft_ceiling(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.9 { x } else { (0.9 + 0.1 * ((a - 0.9) * 10.0).tanh()).copysign(x) }
}

/// Add one grain's next `n` frames (or what is left of it) to its bus.
fn mix_grain(g: &mut Grain, m: &MipBuf, windows: &Windows, bus: &mut [[f32; CHUNK]; 2], n: usize, bend_now: f32) {
    let start = if g.delay as usize >= n {
        g.delay -= n as u32;
        return;
    } else {
        std::mem::take(&mut g.delay) as usize
    };
    let count = (g.rem as usize).min(n - start);
    let step = g.step * (bend_now / g.bend0) as f64;
    // Mip levels around the pitch (a little low-biased to cover the
    // half-band transition band).
    let a = step.abs();
    let lod = if a > 0.7 { (a.log2() + 0.25).clamp(0.0, 3.0) } else { 0.0 };
    let ja = (lod.floor() as usize).min(3);
    let (jb, tf) = if ja >= 3 { (3, 0.0) } else { (ja + 1, (lod - ja as f64) as f32) };
    let two = |j: usize| (1usize << j) as f64;
    let (la, lb) = (&m.levels[ja], &m.levels[jb]);
    let mut pa = level_pos(ja, g.t0, la.cap);
    let mut pb = level_pos(jb, g.t0, lb.cap);
    let (sa, sb) = (step / two(ja), step / two(jb));
    let (ca, cb) = (la.cap as f64, lb.cap as f64);
    let blend = tf > 0.0;
    let (win, peak, s1, s2) = (g.win as usize, g.peak, g.s1, g.s2);
    let (gl, gr) = (g.gl, g.gr);
    let mut u = g.u;
    for s in start..start + count {
        let w = windows.at(win, window::warp(u, peak, s1, s2));
        if g.mono {
            let mut x = la.read(0, pa);
            if blend {
                x += (lb.read(0, pb) - x) * tf;
            }
            x *= w;
            bus[0][s] += x * gl;
            bus[1][s] += x * gr;
        } else {
            let mut xl = la.read(0, pa);
            let mut xr = la.read(1, pa);
            if blend {
                xl += (lb.read(0, pb) - xl) * tf;
                xr += (lb.read(1, pb) - xr) * tf;
            }
            bus[0][s] += xl * w * gl;
            bus[1][s] += xr * w * gr;
        }
        u += g.du;
        pa += sa;
        if pa >= ca {
            pa -= ca;
        } else if pa < 0.0 {
            pa += ca;
        }
        pb += sb;
        if pb >= cb {
            pb -= cb;
        } else if pb < 0.0 {
            pb += cb;
        }
    }
    g.u = u;
    g.t0 += step * count as f64;
    g.rem -= count as u32;
    if g.rem == 0 {
        g.active = false;
    }
}

impl Segmented for Granular {
    fn event(&mut self, kind: &EventKind) {
        if self.params.take_dirty() {
            self.update();
        }
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, .. } => self.note_on(key, velocity),
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::PitchBend { value, .. } => self.bend = if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 },
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => {
                for v in &mut self.voices {
                    if v.active && !v.released {
                        v.env.gate_off();
                        v.released = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let sc = ctx.sidechain;
        let buf = &mut *ctx.buffer;
        for ch in 0..buf.channels() {
            buf.channel_mut(ch)[start..end].fill(0.0);
        }
        let record = self.c.source == SOURCE_LIVE && !self.c.frozen && self.live_ready;
        if record && !self.recording {
            // Recording resumes after a gap: the old history no longer joins
            // the new one, so nothing may read across the seam.
            self.live.valid = 0;
        }
        self.recording = record;
        let mut i = start;
        while i < end {
            let n = CHUNK.min(end - i);
            if record {
                for k in 0..n {
                    let (a, b) = sidechain_frame(sc, i + k);
                    self.live.push(sanitize(a), sanitize(b));
                }
            }
            let mut out = [[0.0f32; CHUNK]; 2];
            self.chunk(&mut out, n, self.block_pos0 + i as i64);
            if buf.channels() >= 2 {
                let (l, r) = buf.stereo_mut();
                l[i..i + n].copy_from_slice(&out[0][..n]);
                r[i..i + n].copy_from_slice(&out[1][..n]);
            } else if buf.channels() == 1 {
                for k in 0..n {
                    buf.channel_mut(0)[i + k] = 0.5 * (out[0][k] + out[1][k]);
                }
            }
            i += n;
        }
        self.local = end;
    }
}

impl Processor for Granular {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        let sr = sample_rate.clamp(8_000.0, 384_000.0);
        let changed = sr != self.sr || !self.live_ready;
        self.sr = sr;
        for v in &mut self.voices {
            v.env.set_sample_rate(sr);
            v.norm.set_time(30.0, sr);
        }
        self.vol.set_time(8.0, sr);
        self.width.set_time(8.0, sr);
        self.dc = [DcBlocker::new(8.0, sr); 2];
        if changed {
            self.live = MipBuf::new_live((LIVE_SECS_CAP * sr) as usize, sr);
            self.live_ready = true;
            self.recording = false;
            self.apply_saved_live();
        }
        self.params.mark_dirty();
        self.update();
        self.vol.snap_to_target();
        self.width.snap_to_target();
        self.reset();
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.env.reset();
            v.filt = [SvfState::default(); 2];
        }
        for g in &mut self.grains {
            g.active = false;
        }
        self.peak = 0.0;
        self.bend = 0.0;
        self.dc = [DcBlocker::new(8.0, self.sr); 2];
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.block_pos0 = ctx.transport.position_samples;
        self.playing = ctx.transport.playing;
        self.bpm = if ctx.transport.bpm.is_finite() && ctx.transport.bpm > 1.0 { ctx.transport.bpm } else { 120.0 };
        self.local = 0;
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn tail(&self) -> u32 {
        ((self.params.plain(p::RELEASE) / 1000.0 * 1.2 + 0.05) as f64 * self.sr) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, json!({ "path": self.file_path, "live": self.snapshot_live() }))
    }

    fn load_state(&mut self, data: &[u8]) {
        let extra = load_state(&mut self.params, data);
        if let Some(path) = extra.get("path").and_then(Value::as_str) {
            // A missing file leaves the built-in source in charge.
            let _ = load_wav(path).map(|s| self.set_sample_data(s));
        }
        if let Some(live) = extra.get("live").filter(|v| !v.is_null()) {
            self.saved_live = decode_live(live);
            if self.live_ready {
                self.apply_saved_live();
            }
        }
        self.params.mark_dirty();
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let src = self.active_src();
        let region = self.region(0);
        let (secs, fill) = match (self.src_buf(src), src) {
            (Some(m), 1) => ((m.valid as f64).min(self.c.live_len) / self.sr, (m.valid as f64 / self.c.live_len.max(1.0)).min(1.0)),
            (Some(m), _) => (m.frames as f64 / m.src_sr, 1.0),
            _ => (0.0, 0.0),
        };
        let voices = self.voices.iter().filter(|v| v.active).count() as f32;
        let grains = self.grains.iter().filter(|g| g.active).count() as f32;
        let kind = match src {
            0 => 0.0,
            1 => 1.0,
            _ => 2.0,
        };
        let mut o = write_meter(out, 0, &[voices, grains, self.peak, self.last_pos, self.c.frozen as u8 as f32, kind, secs as f32, fill as f32]);
        let mut wave = [0.0f32; METER_WAVE];
        if let (Some(m), Some(rg)) = (self.src_buf(src), region) {
            let l = &m.levels[0];
            let len = rg.hi - rg.lo;
            for (k, w) in wave.iter_mut().enumerate() {
                let a = rg.lo + len * k as f64 / METER_WAVE as f64;
                let mut pk = 0.0f32;
                for j in 0..16 {
                    let t = a + len * j as f64 / (16.0 * METER_WAVE as f64);
                    let idx = (t.max(0.0) as u64 % l.cap as u64) as usize + source::GUARD;
                    pk = pk.max(l.ch[0][idx].abs());
                }
                *w = pk.min(1.0);
            }
        }
        o = write_meter(out, o, &wave);
        let mut gl = [0.0f32; METER_GRAINS * 3];
        let mut it = self.grains.iter().filter(|g| g.active);
        for k in 0..METER_GRAINS {
            match it.next() {
                Some(g) => {
                    gl[k * 3] = g.disp_pos;
                    gl[k * 3 + 1] = g.u.clamp(0.0, 1.0);
                    gl[k * 3 + 2] = g.pan;
                }
                None => gl[k * 3] = -1.0,
            }
        }
        write_meter(out, o, &gl)
    }
}

// ----- live capture persistence ---------------------------------------------

impl Granular {
    /// The frozen/recorded capture as JSON (`sr`, `frames`, 16-bit stereo PCM
    /// in base64), or `null` when there is nothing worth keeping.
    fn snapshot_live(&self) -> Value {
        if !(self.c.frozen && self.c.source == SOURCE_LIVE) || self.live.valid < 64 || !self.live_ready {
            return Value::Null;
        }
        let frames = (self.live.valid as usize).min(self.c.live_len as usize).min((LIVE_SECS_MAX * self.sr) as usize);
        let head = self.live.head();
        let l = &self.live.levels[0];
        let mut bytes = Vec::with_capacity(frames * 4);
        for k in 0..frames {
            let abs = head - frames as u64 + k as u64;
            let idx = (abs % l.cap as u64) as usize + source::GUARD;
            for ch in 0..2 {
                let v = (l.ch[ch][idx].clamp(-1.0, 1.0) * 32767.0).round() as i16;
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        json!({ "sr": self.sr, "frames": frames, "pcm": base64_encode(&bytes) })
    }

    /// Push the saved capture into the (freshly allocated) ring.
    fn apply_saved_live(&mut self) {
        let Some((sr, data)) = self.saved_live.clone() else { return };
        self.live.clear();
        let frames = data.len() / 2;
        let ratio = sr / self.sr;
        let out_frames = (frames as f64 / ratio) as usize;
        for k in 0..out_frames {
            let pos = k as f64 * ratio;
            let i = (pos as usize).min(frames.saturating_sub(1));
            let j = (i + 1).min(frames - 1);
            let t = (pos - i as f64) as f32;
            let l = data[i * 2] + (data[j * 2] - data[i * 2]) * t;
            let r = data[i * 2 + 1] + (data[j * 2 + 1] - data[i * 2 + 1]) * t;
            self.live.push(l, r);
        }
        self.recording = false;
    }
}

fn decode_live(v: &Value) -> Option<(f64, Vec<f32>)> {
    let sr = v.get("sr")?.as_f64()?;
    let bytes = base64_decode(v.get("pcm")?.as_str()?)?;
    if !(sr.is_finite() && (8_000.0..=384_000.0).contains(&sr)) || bytes.len() < 8 {
        return None;
    }
    let data: Vec<f32> = bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32767.0).collect();
    Some((sr, data[..data.len() / 2 * 2].to_vec()))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(B64[(n >> 18) as usize & 63] as char);
        s.push(B64[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    s
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
