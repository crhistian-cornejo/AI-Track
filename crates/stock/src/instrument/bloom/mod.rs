//! `instrument.bloom` — a generative scene instrument: one press grows
//! sixteen percussive and tonal sounds at once, coherent with each other.
//!
//! * **Scene** ([`genome::Scene`]): sixteen sounds grown from one seed as one
//!   species (`grow.rs`). Sound `i` plays on MIDI note `Base + i` (C1..D#2 by
//!   default, a drum-rack layout); *Chromatic* plays the selected sound
//!   across the keyboard (C3 = its own pitch), *Single* plays it on any key
//!   (a pad of a Drum Rack). Velocity scales the level and, by each sound's
//!   `Vel > Tone` gene, its brightness and FM index.
//! * **Engines** (`voice.rs`): a Body (oscillators, pitch sweep, FM, noise,
//!   drive, filter, AHD-R amp) exciting a modal Resonator, plus a shared
//!   ambience (`space.rs`).
//! * **State**: the genomes live in the state blob (`extra.scene`, plus the
//!   four morph corners), never as parameters. The parameters are the
//!   performance surface: Seed and Species (what the scene was grown from),
//!   Deviance (read by the editor's Roll and Mutate), the Decay / Body /
//!   Tone / Grit / Space macros, Morph X/Y between the corner scenes, the
//!   selected sound, mode, base note, choke and volume.
//!
//! The scene the device plays is the state's scene when it was grown from
//! the Seed and Species parameters, else the scene those parameters grow
//! (a factory preset is a seed and a species). Growing on a parameter
//! change happens on the audio thread without allocating: the genomes are
//! fixed-size arrays and the level of each new sound is measured with a
//! short render, one sound per block.
//!
//! Each sound's level is normalised by such a render (`render.rs`), so a
//! random scene comes out playable. Notes allocate from [`VOICES`] voices
//! (oldest stolen with a 5 ms fade; at most two per sound in Kit mode);
//! sounds in the same choke group cut each other when Choke is on.
//!
//! Live and export are identical: every hit of a sound uses the same noise
//! seed, and nothing depends on a clock.
//!
//! # Meter (`meter()`, [`METER_LEN`] values)
//! `[0..16)` envelope per sound, `[16..32)` transient pulse per sound (1 at a
//! hit, decaying over 60 ms), `[32..48)` hit counter per sound (wraps at
//! 4096), `[48..64)` live spectral centroid per sound (0 = 50 Hz, 1 = 10 kHz,
//! log), `[64..80)` live noisiness per sound, `[80]` output RMS, `[81]`
//! output peak, `[82]` voices in use / [`VOICES`], `[83]` ambience level,
//! `[84]` share of the scene's levels measured, `[85]` the playing scene's
//! seed, `[86]` its species.

pub mod genome;
pub mod grow;
pub mod render;
pub mod rng;
pub mod space;
pub mod timbre;
pub mod voice;

use crate::common::{ParamStore, Segmented, drive, load_state, save_state};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use genome::{Genome, SOUNDS, Scene, Species, g};
use grow::{GrowOpts, generate, morph4};
use render::{MEASURE_FRAMES, measure_gain, noise_seed};
use serde_json::{Value, json};
use space::Space;
use voice::{Macros, Patch, Voice};

pub const ID: &str = "instrument.bloom";
pub const VOICES: usize = 24;
/// Voices one sound may hold in Kit mode (a third hit fades the oldest).
const PER_SOUND: usize = 2;
pub const METER_LEN: usize = 87;
/// Highest seed (the Seed parameter's range).
pub const MAX_SEED: u64 = 999_999;
/// The scene a new Bloom grows.
pub const DEFAULT_SEED: u64 = 1_337;

pub mod p {
    pub const SEED: u32 = 0;
    pub const SPECIES: u32 = 1;
    pub const DEVIANCE: u32 = 2;
    pub const DECAY: u32 = 3;
    pub const BODY: u32 = 4;
    pub const TONE: u32 = 5;
    pub const GRIT: u32 = 6;
    pub const SPACE: u32 = 7;
    pub const MORPH_X: u32 = 8;
    pub const MORPH_Y: u32 = 9;
    pub const SELECTED: u32 = 10;
    pub const MODE: u32 = 11;
    pub const BASE: u32 = 12;
    pub const CHOKE: u32 = 13;
    pub const VOLUME: u32 = 14;
    pub const VELOCITY: u32 = 15;
    pub const TUNE: u32 = 16;
}

pub const MODES: &[&str] = &["Kit", "Chromatic", "Single"];
pub const ON_OFF: &[&str] = &["Off", "On"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::SEED, "Seed", 0.0, MAX_SEED as f32, DEFAULT_SEED as f32).short("Seed"),
        ParamInfo::new(p::SPECIES, "Species", 0.0, 4.0, 0.0).choices(genome::SPECIES_NAMES),
        ParamInfo::new(p::DEVIANCE, "Deviance", 0.0, 100.0, 100.0).unit("%").short("Dev"),
        ParamInfo::new(p::DECAY, "Decay", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::BODY, "Body", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::TONE, "Tone", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::GRIT, "Grit", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::SPACE, "Space", 0.0, 100.0, 25.0).unit("%"),
        ParamInfo::new(p::MORPH_X, "Morph X", 0.0, 1.0, 0.0).short("X"),
        ParamInfo::new(p::MORPH_Y, "Morph Y", 0.0, 1.0, 0.0).short("Y"),
        ParamInfo { scale: aitrack_dsp::ParamScale::Stepped, ..ParamInfo::new(p::SELECTED, "Selected Sound", 0.0, 15.0, 0.0).short("Sound") },
        ParamInfo::new(p::MODE, "Play Mode", 0.0, 2.0, 0.0).choices(MODES).short("Mode"),
        ParamInfo { scale: aitrack_dsp::ParamScale::Stepped, ..ParamInfo::new(p::BASE, "Base Note", 12.0, 108.0, 36.0).short("Base") },
        ParamInfo::new(p::CHOKE, "Choke", 0.0, 1.0, 1.0).choices(ON_OFF),
        ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, 0.0).unit("dB").short("Vol"),
        ParamInfo::new(p::VELOCITY, "Velocity", 0.0, 100.0, 80.0).unit("%").short("Vel"),
        ParamInfo::new(p::TUNE, "Tune", -24.0, 24.0, 0.0).unit("st"),
    ]
}

/// The four morph corners a state may hold (each a full set of sounds).
pub type Corners = [Option<Box<[Genome; SOUNDS]>>; 4];

/// A seed from a stored parameter value (plain).
pub fn seed_of(plain: f32) -> u64 {
    (plain.max(0.0).round() as u64).min(MAX_SEED)
}

/// The scene a device with these parameters plays: `stored` when it was
/// grown from `seed` and `species`, else the scene they grow.
pub fn effective_scene(stored: Option<&Scene>, seed: u64, species: Species) -> Scene {
    match stored {
        Some(s) if s.seed == seed && s.species == species => s.clone(),
        _ => generate(seed, GrowOpts { species, spread: 1.0 }),
    }
}

/// The state blob for a scene and its corners (what the editor stores).
pub fn state_json(params: &std::collections::BTreeMap<u32, f32>, scene: &Scene, corners: &Corners) -> Vec<u8> {
    state_json_with(params, scene, corners, None)
}

/// [`state_json`] with the editor's visual look (`visual`: the key of the
/// visual's material; the sound never depends on it).
pub fn state_json_with(params: &std::collections::BTreeMap<u32, f32>, scene: &Scene, corners: &Corners, visual: Option<&str>) -> Vec<u8> {
    let mut p = serde_json::Map::new();
    for (k, v) in params {
        p.insert(k.to_string(), Value::from(f64::from(*v)));
    }
    let root = json!({ "version": 1, "params": Value::Object(p), "extra": extra_json(scene, corners, visual) });
    serde_json::to_vec(&root).unwrap_or_default()
}

fn extra_json(scene: &Scene, corners: &Corners, visual: Option<&str>) -> Value {
    let mut extra = json!({
        "scene": scene.to_json(),
        "corners": corners.iter().map(|c| c.as_ref().map_or(Value::Null, |s| serde_json::to_value(**s).unwrap_or(Value::Null))).collect::<Vec<_>>(),
    });
    if let (Some(v), Some(map)) = (visual, extra.as_object_mut()) {
        map.insert("visual".into(), Value::from(v));
    }
    extra
}

/// The visual's material a state blob names, if any.
pub fn visual_of(data: Option<&[u8]>) -> Option<String> {
    let root = data.and_then(|d| serde_json::from_slice::<Value>(d).ok())?;
    root.get("extra")?.get("visual")?.as_str().map(str::to_owned)
}

/// The scene and corners a state blob holds (`None` parts when absent).
pub fn parse_state(data: Option<&[u8]>) -> (Option<Scene>, Corners) {
    let mut corners: Corners = Default::default();
    let Some(root) = data.and_then(|d| serde_json::from_slice::<Value>(d).ok()) else { return (None, corners) };
    let extra = root.get("extra").unwrap_or(&Value::Null);
    let scene = extra.get("scene").and_then(Scene::from_json);
    if let Some(list) = extra.get("corners").and_then(Value::as_array) {
        for (slot, v) in corners.iter_mut().zip(list) {
            *slot = serde_json::from_value::<[Genome; SOUNDS]>(v.clone()).ok().map(Box::new);
        }
    }
    (scene, corners)
}

struct Corner {
    sounds: [Genome; SOUNDS],
    gains: [f32; SOUNDS],
}

/// What the meter publishes, computed in `process`.
struct Meters {
    env: [f32; SOUNDS],
    pulse: [f32; SOUNDS],
    hits: [f32; SOUNDS],
    centroid: [f32; SOUNDS],
    noisy: [f32; SOUNDS],
    rms: f32,
    peak: f32,
}

impl Default for Meters {
    fn default() -> Self {
        Self { env: [0.0; SOUNDS], pulse: [0.0; SOUNDS], hits: [0.0; SOUNDS], centroid: [0.0; SOUNDS], noisy: [0.0; SOUNDS], rms: 0.0, peak: 0.0 }
    }
}

pub struct Bloom {
    info: PluginInfo,
    params: ParamStore,
    sr: f32,
    scene: Box<Scene>,
    gains: [f32; SOUNDS],
    /// Bit `i` set: sound `i`'s level has been measured.
    measured: u16,
    corners: [Option<Box<Corner>>; 4],
    /// The editor's visual material, kept so a saved state round-trips it.
    visual: Option<String>,
    voices: Box<[Voice; VOICES]>,
    age: u64,
    scratch: Vec<f32>,
    space: Space,
    bus_l: Vec<f32>,
    bus_r: Vec<f32>,
    send: Vec<f32>,
    meters: Meters,
    // Cached parameters.
    macros: Macros,
    vel_amount: f32,
    mode: usize,
    base: i32,
    choke: bool,
    selected: usize,
    morph: (f32, f32),
    tune: f32,
    volume: f32,
    vol_now: f32,
    space_amt: f32,
    space_now: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Bloom::new())
}

impl Default for Bloom {
    fn default() -> Self {
        Self::new()
    }
}

/// The default scene and its levels, worked out once per process.
fn default_scene() -> &'static (Scene, [f32; SOUNDS]) {
    static S: std::sync::OnceLock<(Scene, [f32; SOUNDS])> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        crate::instrument::vault::kit::init();
        let scene = generate(DEFAULT_SEED, GrowOpts::default());
        let mut scratch = vec![0.0; MEASURE_FRAMES * 2];
        let gains = std::array::from_fn(|i| measure_gain(&scene.sounds[i], &mut scratch));
        (scene, gains)
    })
}

impl Bloom {
    pub fn new() -> Self {
        crate::instrument::vault::kit::init();
        let params = params();
        let (scene, gains) = default_scene().clone();
        let idle = Voice::blank();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Bloom",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["generative", "drums", "percussion", "random", "kit", "resonator", "fm"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            scene: Box::new(scene),
            gains,
            measured: u16::MAX,
            corners: Default::default(),
            visual: None,
            voices: Box::new([idle; VOICES]),
            age: 0,
            scratch: vec![0.0; MEASURE_FRAMES * 2],
            space: Space::new(),
            bus_l: Vec::new(),
            bus_r: Vec::new(),
            send: Vec::new(),
            meters: Meters::default(),
            macros: Macros::default(),
            vel_amount: 0.8,
            mode: 0,
            base: 36,
            choke: true,
            selected: 0,
            morph: (0.0, 0.0),
            tune: 0.0,
            volume: 1.0,
            vol_now: 1.0,
            space_amt: 0.25,
            space_now: 0.25,
        };
        s.prepare(48_000.0, 512);
        s
    }

    /// The scene being played.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Normalised levels of the playing scene's sounds.
    pub fn gains(&self) -> &[f32; SOUNDS] {
        &self.gains
    }

    fn target(&self) -> (u64, Species) {
        (seed_of(self.params.plain(p::SEED)), Species::from_index(self.params.choice(p::SPECIES)))
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.macros = Macros { decay: pr.plain(p::DECAY) / 100.0, body: pr.plain(p::BODY) / 100.0, tone: pr.plain(p::TONE) / 100.0, grit: pr.plain(p::GRIT) / 100.0 };
        self.vel_amount = pr.plain(p::VELOCITY) / 100.0;
        self.mode = pr.choice(p::MODE);
        self.base = pr.plain(p::BASE).round() as i32;
        self.choke = pr.choice(p::CHOKE) == 1;
        self.selected = pr.choice(p::SELECTED).min(SOUNDS - 1);
        self.morph = (pr.plain(p::MORPH_X), pr.plain(p::MORPH_Y));
        self.tune = pr.plain(p::TUNE);
        self.volume = db_to_gain(pr.plain(p::VOLUME));
        self.space_amt = pr.plain(p::SPACE) / 100.0;
        let (seed, species) = self.target();
        if seed != self.scene.seed || species != self.scene.species {
            // Grow in place: no allocation; levels follow one per block.
            *self.scene = generate(seed, GrowOpts { species, spread: 1.0 });
            self.measured = 0;
        }
    }

    /// Measure the level of the next sound still waiting (one per block).
    fn measure_next(&mut self) {
        if self.measured == u16::MAX {
            return;
        }
        let i = (0..SOUNDS).find(|i| self.measured & (1 << i) == 0).unwrap_or(0);
        self.gains[i] = measure_gain(&self.scene.sounds[i], &mut self.scratch);
        self.measured |= 1 << i;
    }

    fn measure_all(&mut self) {
        while self.measured != u16::MAX {
            self.measure_next();
        }
    }

    /// The genome and level of sound `i` at the current morph position.
    fn sound(&self, i: usize) -> (Genome, f32) {
        if self.corners.iter().all(Option::is_none) {
            return (self.scene.sounds[i], self.gains[i]);
        }
        let pick = |k: usize| self.corners[k].as_ref().map_or((&self.scene.sounds[i], self.gains[i]), |c| (&c.sounds[i], c.gains[i]));
        let c = [pick(0), pick(1), pick(2), pick(3)];
        let (x, y) = (self.morph.0.clamp(0.0, 1.0), self.morph.1.clamp(0.0, 1.0));
        let genome = morph4([c[0].0, c[1].0, c[2].0, c[3].0], x, y);
        let top = c[0].1 + (c[1].1 - c[0].1) * x;
        let bottom = c[2].1 + (c[3].1 - c[2].1) * x;
        (genome, top + (bottom - top) * y)
    }

    fn note_on(&mut self, key: u8, velocity: f32) {
        if velocity <= 0.0 {
            self.note_off(key);
            return;
        }
        let (sound, transpose) = match self.mode {
            1 => (self.selected, f32::from(key) - 60.0),
            2 => (self.selected, 0.0),
            _ => {
                let i = i32::from(key) - self.base;
                if !(0..SOUNDS as i32).contains(&i) {
                    return;
                }
                (i as usize, 0.0)
            }
        };
        let (genome, gain) = self.sound(sound);
        let group = genome.folded().index(g::CHOKE);
        // Choke: the other sounds of the group stop.
        if self.choke && group > 0 {
            for v in self.voices.iter_mut().filter(|v| v.active && v.sound != sound) {
                if self.scene.sounds[v.sound].folded().index(g::CHOKE) == group {
                    v.kill(8.0);
                }
            }
        }
        // At most two hits of one sound ring together (Kit and Single).
        if self.mode != 1 {
            let mut same: [(u64, usize); PER_SOUND + 1] = [(u64::MAX, 0); PER_SOUND + 1];
            let mut n = 0;
            for (k, v) in self.voices.iter().enumerate() {
                if v.active && !v.is_fading() && v.sound == sound && n < same.len() {
                    same[n] = (v.age, k);
                    n += 1;
                }
            }
            if n >= PER_SOUND {
                let oldest = same[..n].iter().min_by_key(|x| x.0).map_or(0, |x| x.1);
                self.voices[oldest].kill(5.0);
            }
        }
        let slot = match self.voices.iter().position(|v| !v.active) {
            Some(k) => k,
            None => self.voices.iter().enumerate().min_by(|a, b| (!a.1.is_fading(), a.1.age).cmp(&(!b.1.is_fading(), b.1.age))).map_or(0, |x| x.0),
        };
        let patch = Patch::new(&genome, self.sr, transpose + self.tune, velocity, self.vel_amount, &self.macros, gain);
        self.age += 1;
        let vseed = noise_seed(sound, key) ^ (velocity * 127.0) as u32;
        self.voices[slot].start(patch, sound, key, self.age, vseed);
        self.meters.pulse[sound] = 1.0;
        self.meters.hits[sound] = (self.meters.hits[sound] + 1.0) % 4096.0;
    }

    fn note_off(&mut self, key: u8) {
        if self.mode == 1 {
            for v in self.voices.iter_mut().filter(|v| v.active && v.key == key) {
                v.release();
            }
        }
    }

    fn all_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.kill(10.0);
        }
    }

    fn apply_scene(&mut self, scene: Scene) {
        *self.scene = scene;
        self.measured = 0;
        self.measure_all();
    }
}

impl Segmented for Bloom {
    fn event(&mut self, kind: &EventKind) {
        match *kind {
            EventKind::Param { id, value } => {
                self.params.set(id, value);
                if self.params.take_dirty() {
                    self.update();
                }
            }
            EventKind::NoteOn { key, velocity, .. } => {
                if self.params.take_dirty() {
                    self.update();
                }
                self.note_on(key, velocity);
            }
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => self.all_off(),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let n = end - start;
        if n == 0 || self.bus_l.len() < n {
            return;
        }
        let (bl, br, bs) = (&mut self.bus_l[..n], &mut self.bus_r[..n], &mut self.send[..n]);
        bl.fill(0.0);
        br.fill(0.0);
        bs.fill(0.0);
        let mut active = 0;
        for v in self.voices.iter_mut() {
            if v.active {
                active += 1;
                v.render(bl, br, bs);
            }
        }
        // Smooth the live levels to avoid zipper steps.
        let k = 1.0 - (-(n as f32) / (0.02 * self.sr)).exp();
        self.space_now += (self.space_amt - self.space_now) * k;
        // An idle scene with a silent tail costs nothing (a Drum Rack holds sixteen).
        if active > 0 || self.space.level > 1e-7 {
            self.space.process(bs, bl, br, self.space_now * 1.6);
        }
        let buf = &mut *ctx.buffer;
        let (mut sum, mut peak) = (0.0f32, 0.0f32);
        let vol_from = self.vol_now;
        self.vol_now += (self.volume - self.vol_now) * k;
        for i in 0..n {
            let vol = vol_from + (self.vol_now - vol_from) * (i as f32 / n as f32);
            let (l, r) = (crate::common::sanitize(bl[i] * vol).clamp(-4.0, 4.0), crate::common::sanitize(br[i] * vol).clamp(-4.0, 4.0));
            sum += l * l + r * r;
            peak = peak.max(l.abs()).max(r.abs());
            if buf.channels() >= 2 {
                buf.channel_mut(0)[start + i] = l;
                buf.channel_mut(1)[start + i] = r;
            } else if buf.channels() == 1 {
                buf.channel_mut(0)[start + i] = (l + r) * 0.5;
            }
        }
        // Meters for the visual.
        let m = &mut self.meters;
        let fall = (-(n as f32) / (0.06 * self.sr)).exp();
        let mut env = [0f32; SOUNDS];
        let mut sig = [0f32; SOUNDS];
        let mut diff = [0f32; SOUNDS];
        let mut noise = [0f32; SOUNDS];
        for v in self.voices.iter().filter(|v| v.active) {
            env[v.sound] = env[v.sound].max(v.level);
            sig[v.sound] += v.e_sig;
            diff[v.sound] += v.e_diff;
            noise[v.sound] += v.e_noise;
        }
        for i in 0..SOUNDS {
            m.env[i] = env[i].max(m.env[i] * fall);
            m.pulse[i] *= fall;
            if sig[i] > 1e-12 {
                // Centroid of a signal from the energy of its first difference.
                let c = self.sr / std::f32::consts::TAU * (diff[i] / sig[i]).sqrt() * 2.0;
                let norm = ((c.max(50.0) / 50.0).log2() / 200f32.log2()).clamp(0.0, 1.0);
                m.centroid[i] += (norm - m.centroid[i]) * 0.3;
                m.noisy[i] += ((noise[i] / sig[i]).clamp(0.0, 1.0) - m.noisy[i]) * 0.3;
            }
        }
        let rms = (sum / (2 * n) as f32).sqrt();
        m.rms = m.rms * 0.7 + rms * 0.3;
        m.peak = peak.max(m.peak * fall);
    }
}

impl Processor for Bloom {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        self.sr = sample_rate as f32;
        self.bus_l = vec![0.0; max_frames.max(1)];
        self.bus_r = vec![0.0; max_frames.max(1)];
        self.send = vec![0.0; max_frames.max(1)];
        self.space.prepare(self.sr);
        self.params.mark_dirty();
        self.update();
        self.measure_all();
        self.reset();
    }

    fn reset(&mut self) {
        for v in self.voices.iter_mut() {
            v.active = false;
            v.level = 0.0;
        }
        self.space.reset();
        self.meters = Meters::default();
        self.vol_now = self.volume;
        self.space_now = self.space_amt;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.measure_next();
        let frames = ctx.buffer.frames();
        if frames > self.bus_l.len() {
            // A block larger than prepared: render silence rather than allocate.
            ctx.buffer.clear();
            return;
        }
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn tail(&self) -> u32 {
        (self.sr * 6.0) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        let corners: Corners = std::array::from_fn(|k| self.corners[k].as_ref().map(|c| Box::new(c.sounds)));
        save_state(&self.params, extra_json(&self.scene, &corners, self.visual.as_deref()))
    }

    fn load_state(&mut self, data: &[u8]) {
        let extra = load_state(&mut self.params, data);
        let wrapped = serde_json::to_vec(&json!({ "extra": extra })).ok();
        let (stored, corners) = parse_state(wrapped.as_deref());
        self.visual = visual_of(wrapped.as_deref());
        let (seed, species) = self.target();
        let scene = effective_scene(stored.as_ref(), seed, species);
        self.apply_scene(scene);
        for (slot, c) in self.corners.iter_mut().zip(corners) {
            *slot = c.map(|sounds| {
                let gains = std::array::from_fn(|i| measure_gain(&sounds[i], &mut self.scratch));
                Box::new(Corner { sounds: *sounds, gains })
            });
        }
        self.params.mark_dirty();
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let m = &self.meters;
        let mut buf = [0f32; METER_LEN];
        buf[..16].copy_from_slice(&m.env);
        buf[16..32].copy_from_slice(&m.pulse);
        buf[32..48].copy_from_slice(&m.hits);
        buf[48..64].copy_from_slice(&m.centroid);
        buf[64..80].copy_from_slice(&m.noisy);
        buf[80] = m.rms;
        buf[81] = m.peak;
        buf[82] = self.voices.iter().filter(|v| v.active).count() as f32 / VOICES as f32;
        buf[83] = self.space.level;
        buf[84] = self.measured.count_ones() as f32 / SOUNDS as f32;
        buf[85] = self.scene.seed as f32;
        buf[86] = self.scene.species.index() as f32;
        crate::common::write_meter(out, 0, &buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, render_notes, rms, set_plain};

    #[test]
    fn each_key_plays_its_own_sound() {
        let mut b = Bloom::new();
        let (kick, _) = render_notes(&mut b, &[(0, 36, 1.0, true)], 12_000);
        b.reset();
        let (hat, _) = render_notes(&mut b, &[(0, 42, 1.0, true)], 12_000);
        b.reset();
        let (none, _) = render_notes(&mut b, &[(0, 20, 1.0, true)], 12_000);
        assert!(rms(&kick) > 1e-3 && rms(&hat) > 1e-4, "{} {}", rms(&kick), rms(&hat));
        assert!(rms(&none) < 1e-6, "notes outside the kit are silent");
        let zc = |x: &[f32]| x.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
        assert!(zc(&hat[..4_000]) > zc(&kick[..4_000]) * 4, "the hat is brighter than the kick");
    }

    #[test]
    fn live_and_offline_renders_match_whatever_the_block_size() {
        let notes = [(0, 36, 1.0, true), (3_000, 38, 0.8, true), (7_500, 42, 0.6, true), (9_000, 46, 1.0, true)];
        let mut a = Bloom::new();
        let (la, _) = render_notes(&mut a, &notes, 20_000);
        let mut b = Bloom::new();
        let (lb, _) = render_notes(&mut b, &notes, 20_000);
        assert_eq!(la, lb, "the same notes play the same samples");
        assert!(rms(&la) > 1e-3);
    }

    #[test]
    fn chromatic_mode_transposes_the_selected_sound() {
        let mut b = Bloom::new();
        let mut scene = b.scene().clone();
        let mut tone = Genome::neutral(genome::Role::Pluck);
        tone.set(g::PITCH, 60.0);
        tone.set(g::DECAY, 2_000.0);
        tone.set(g::NOISE_LEVEL, 0.0);
        tone.set(g::DRIVE, 0.0);
        scene.sounds[14] = tone;
        let params: std::collections::BTreeMap<u32, f32> = [(p::SEED, b.get_param(p::SEED))].into();
        b.load_state(&state_json(&params, &scene, &Default::default()));
        set_plain(&mut b, p::MODE, 1.0);
        set_plain(&mut b, p::SELECTED, 14.0);
        let (c3, _) = render_notes(&mut b, &[(0, 60, 1.0, true)], 9_600);
        b.reset();
        let (c4, _) = render_notes(&mut b, &[(0, 72, 1.0, true)], 9_600);
        let zc = |x: &[f32]| x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        let ratio = zc(&c4[200..9_000]) / zc(&c3[200..9_000]).max(1.0);
        assert!((ratio - 2.0).abs() < 0.05, "an octave up doubles the pitch: {ratio}");
        assert!((zc(&c3[200..9_000]) / (8_800.0 / SR as f32) - 261.6).abs() < 5.0, "C3 plays the sound at its own pitch");
    }

    #[test]
    fn hats_choke_each_other() {
        let mut b = Bloom::new();
        assert_eq!(b.scene().sounds[10].role, genome::Role::HatOpen);
        let (open, _) = render_notes(&mut b, &[(0, 46, 1.0, true)], 24_000);
        b.reset();
        let (choked, _) = render_notes(&mut b, &[(0, 46, 1.0, true), (4_800, 42, 0.01, true)], 24_000);
        assert!(rms(&choked[12_000..]) < rms(&open[12_000..]) * 0.2 + 1e-7, "{} vs {}", rms(&choked[12_000..]), rms(&open[12_000..]));
    }

    #[test]
    fn the_state_round_trips_and_a_new_seed_grows_a_new_scene() {
        let mut a = Bloom::new();
        set_plain(&mut a, p::SEED, 4242.0);
        set_plain(&mut a, p::SPECIES, 3.0);
        let _ = render_notes(&mut a, &[], 256);
        assert_eq!(a.scene().seed, 4242);
        assert_eq!(a.scene().species, Species::Glitch);
        for _ in 0..20 {
            let _ = render_notes(&mut a, &[], 128);
        }
        let mut m = [0f32; 320];
        assert_eq!(a.meter(&mut m), METER_LEN);
        assert_eq!(m[84], 1.0, "every level measured");
        assert_eq!(m[85], 4242.0);
        let bytes = a.save_state();
        let mut b = Bloom::new();
        b.load_state(&bytes);
        assert_eq!(b.scene(), a.scene());
        assert_eq!(b.gains(), a.gains());
        // A stored scene that was edited still plays when its seed matches.
        let mut edited = a.scene().clone();
        edited.sounds[3].set(g::CUTOFF, 300.0);
        let params: std::collections::BTreeMap<u32, f32> = [(p::SEED, a.get_param(p::SEED)), (p::SPECIES, a.get_param(p::SPECIES))].into();
        let blob = state_json(&params, &edited, &Default::default());
        let mut c = Bloom::new();
        c.load_state(&blob);
        assert_eq!(c.scene().sounds[3], edited.sounds[3]);
        let (parsed, corners) = parse_state(Some(&blob));
        assert_eq!(parsed.as_ref(), Some(&edited));
        assert!(corners.iter().all(Option::is_none));
        assert_eq!(visual_of(Some(&blob)), None);
    }

    #[test]
    fn the_visual_material_round_trips_and_leaves_the_sound_alone() {
        let a = Bloom::new();
        let params: std::collections::BTreeMap<u32, f32> = [(p::SEED, a.get_param(p::SEED)), (p::SPECIES, a.get_param(p::SPECIES))].into();
        let plain = state_json(&params, a.scene(), &Default::default());
        let blob = state_json_with(&params, a.scene(), &Default::default(), Some("mercury"));
        assert_eq!(visual_of(Some(&blob)).as_deref(), Some("mercury"));
        assert_eq!(parse_state(Some(&blob)).0, parse_state(Some(&plain)).0, "the same scene");
        let mut b = Bloom::new();
        b.load_state(&blob);
        assert_eq!(b.scene(), a.scene());
        // Saved by the device, the material survives.
        assert_eq!(visual_of(Some(&b.save_state())).as_deref(), Some("mercury"));
        assert_eq!(visual_of(None), None);
        assert_eq!(visual_of(Some(b"not json")), None);
    }

    /// All sixteen sounds hit every 125 ms (up to 24 voices ringing):
    /// `cargo test -p aitrack-stock --release --lib bloom_cpu -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bloom_cpu() {
        for species in Species::ALL {
            let mut b = Bloom::new();
            set_plain(&mut b, p::SPECIES, species.index() as f32);
            set_plain(&mut b, p::SEED, 4_000.0 + species.index() as f32);
            let mut notes = Vec::new();
            for step in 0..24 {
                for k in 0..16u8 {
                    notes.push((step * 6_000 + k as usize * 40, 36 + k, 0.9, true));
                }
            }
            let n = 48_000 * 3;
            let t = std::time::Instant::now();
            let _ = render_notes(&mut b, &notes, n);
            println!("{:<10} {:>6.2} % of a core", species.label(), 100.0 * t.elapsed().as_secs_f64() / 3.0);
        }
    }

    #[test]
    fn morph_corners_change_the_sound() {
        let mut b = Bloom::new();
        let other = generate(9, GrowOpts { species: Species::Metallic, spread: 1.0 });
        let corners: Corners = [None, Some(Box::new(other.sounds)), None, None];
        let params: std::collections::BTreeMap<u32, f32> = [(p::SEED, b.get_param(p::SEED))].into();
        b.load_state(&state_json(&params, &b.scene().clone(), &corners));
        let (at_a, _) = render_notes(&mut b, &[(0, 38, 1.0, true)], 9_600);
        b.reset();
        set_plain(&mut b, p::MORPH_X, 1.0);
        let (at_b, _) = render_notes(&mut b, &[(0, 38, 1.0, true)], 9_600);
        let diff: f32 = at_a.iter().zip(&at_b).map(|(x, y)| (x - y).abs()).sum();
        assert!(diff > 1.0, "{diff}");
    }
}
