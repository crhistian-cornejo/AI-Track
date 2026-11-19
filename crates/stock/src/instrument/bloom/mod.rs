//! `instrument.bloom` — a generative scene instrument: one press grows
//! sixteen sounds at once, coherent with each other: a drum scene, or
//! sixteen patches of one tonal kind (bass, lead, pad, pluck, keys, FX,
//! texture) played by an embedded Wavetable Synth.
//!
//! * **Kind** ([`patch::Kind`], parameter [`p::KIND`]): *Drums* (the default,
//!   the original engine, unchanged) or a tonal kind.
//! * **Drum scene** ([`genome::Scene`]): sixteen sounds grown from one seed
//!   as one species (`grow.rs`). Sound `i` plays on MIDI note `Base + i`
//!   (C1..D#2 by default, a drum-rack layout); *Chromatic* plays the
//!   selected sound across the keyboard (C3 = its own pitch), *Single*
//!   plays it on any key (a pad of a Drum Rack). Velocity scales the level
//!   and, by each sound's `Vel > Tone` gene, its brightness and FM index.
//! * **Tonal scene** ([`patch::PatchScene`], `tonal.rs`): sixteen Wavetable
//!   Synth patches of the kind, grown from the seed out of the hand-made
//!   factory presets of that kind. The **selected** patch plays,
//!   chromatically: *Kit* and *Chromatic* both play it across the keyboard
//!   at the synth's own pitch (key 60 = C3 is the patch's home), *Single*
//!   plays it at key 60 whatever the key. Base and Choke do nothing there.
//!   Morph X/Y blends the selected patch between up to four stored tonal
//!   corners (`extra.patch_corners`, [`tonal::morph4`]), gliding there over
//!   [`MORPH_GLIDE`] so automation never steps.
//! * **Engines** (`voice.rs`): a Body (oscillators, pitch sweep, FM, noise,
//!   drive, filter, AHD-R amp) exciting a modal Resonator, plus a shared
//!   ambience (`space.rs`). Tonal kinds: the embedded
//!   [`WavetableSynth`] (allocated at creation), its output into the same
//!   ambience.
//! * **State**: the genomes live in the state blob (`extra.scene`, the four
//!   morph corners, for a tonal kind `extra.patches`, and the tonal morph
//!   corners `extra.patch_corners`), never as
//!   parameters. The parameters are the performance surface: Seed, Species
//!   and Kind (what the scene was grown from), Deviance (read by the
//!   editor's Roll and Mutate), the Decay / Body / Tone / Grit / Space
//!   macros, Morph X/Y, the selected sound, mode, base note, choke, volume,
//!   and the performance knobs (Attack, Drop, Motion, Width, Drive, Filter,
//!   Vary, Glide, React).
//!
//! The scene the device plays is the state's scene when it was grown from
//! the Seed and Species (Kind) parameters, else the scene those parameters
//! grow (a factory preset is a seed, a species or kind, and knobs). Growing
//! on a parameter change happens on the audio thread without allocating:
//! genomes and patches are fixed-size arrays; the level of each new drum
//! sound is measured with a short render, one sound per block, and of each
//! new tonal patch by [`tonal::Leveler`], a chunk per block (the selected
//! patch first).
//!
//! Each sound's level is normalised by such a render (`render.rs`,
//! `tonal.rs`), so a random scene comes out playable. Drum notes allocate
//! from [`VOICES`] voices (oldest stolen with a 5 ms fade; at most two per
//! sound in Kit mode); sounds in the same choke group cut each other when
//! Choke is on.
//!
//! Live and export are identical: every hit of a drum sound uses the same
//! noise seed, Vary is seeded by the sound, the key and the song position
//! (a hit counter while the transport stops), and nothing reads a clock.
//! The embedded synth runs block-accurately (Bloom's parameter changes
//! reach it at the block start, notes at their sample), so a tonal render is
//! the same for the same block size.
//!
//! # Meter (`meter()`, [`METER_LEN`] values)
//! `[0..16)` envelope per sound, `[16..32)` transient pulse per sound (1 at a
//! hit, decaying over 60 ms), `[32..48)` hit counter per sound (wraps at
//! 4096), `[48..64)` live spectral centroid per sound (0 = 50 Hz, 1 = 10 kHz,
//! log), `[64..80)` live noisiness per sound, `[80]` output RMS, `[81]`
//! output peak, `[82]` voices in use / [`VOICES`], `[83]` ambience level,
//! `[84]` share of the scene's levels measured, `[85]` the playing scene's
//! seed, `[86]` its species, `[87]` the playing kind ([`Kind::index`]),
//! `[88]` the selected tonal patch's level gain. For a tonal kind the
//! per-sound slots of the selected sound follow the synth (its amp
//! envelope, notes, live centroid, the patch's noise share).

pub mod genome;
pub mod grow;
pub mod pack;
pub mod patch;
pub mod render;
pub mod rng;
pub mod space;
pub mod timbre;
pub mod tonal;
pub mod voice;

use crate::common::{ParamStore, Segmented, drive, load_state, save_state};
use crate::instrument::vault::kit::soft;
use crate::instrument::wavetable::WavetableSynth;
use aitrack_dsp::event::Expression;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{AudioBuffer, Category, Event, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, Transport, db_to_gain};
use genome::{Genome, SOUNDS, Scene, Species, g};
use grow::{GrowOpts, generate, morph4};
use patch::{KIND_NAMES, Kind, PATCH_LEN, Patch, PatchScene};
use render::{MEASURE_FRAMES, measure_gain, noise_seed};
use serde_json::{Value, json};
use space::Space;
use tonal::{Knobs, Leveler, PatchCorners};
use voice::{Macros, Patch as DrumPatch, Perform, Voice};

pub const ID: &str = "instrument.bloom";
pub const VOICES: usize = 24;
/// Voices one sound may hold in Kit mode (a third hit fades the oldest).
const PER_SOUND: usize = 2;
pub const METER_LEN: usize = 89;
/// Highest seed (the Seed parameter's range).
pub const MAX_SEED: u64 = 999_999;
/// The scene a new Bloom grows.
pub const DEFAULT_SEED: u64 = 1_337;
/// How much of the synth goes to the shared ambience.
const TONAL_SEND: f32 = 0.35;
/// Most events handed to the synth in one block.
const SYNTH_EVENTS: usize = 1024;
/// Seconds a tonal morph takes to follow Morph X/Y (one pole).
pub const MORPH_GLIDE: f32 = 0.03;
/// How far (in X + Y) the gliding morph moves before the synth gets the
/// next blend: patches change in small steps, at most once per block.
const MORPH_STEP: f32 = 0.004;

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
    /// Drums (the scene engine) or a tonal kind played by the embedded
    /// Wavetable Synth ([`super::patch::KIND_NAMES`]).
    pub const KIND: u32 = 17;
    /// −100..100 %: every sound's attack (drum AHD attack; tonal amp and mod
    /// env attack), ×⅛..×8 (+20 ms at the top).
    pub const ATTACK: u32 = 18;
    /// −100..100 %: pitch drop (drum pitch sweep −100 none … +100 two more
    /// octaves; tonal a pitch envelope at the note start, up to 12 st from
    /// above (+) or below (−)).
    pub const DROP: u32 = 19;
    /// 0..200 %: modulation depth (drum FM index; tonal LFO and envelope
    /// routes).
    pub const MOTION: u32 = 20;
    /// 0..200 %: stereo width of the output (mid/side).
    pub const WIDTH: u32 = 21;
    /// 0..100 %: saturation on the output, level-compensated.
    pub const DRIVE: u32 = 22;
    /// −100..100 %: a resonant DJ filter on the output (− low-pass sweeping
    /// down, + high-pass sweeping up; bypassed at 0).
    pub const FILTER: u32 = 23;
    /// 0..100 %: per-hit humanise of pitch, tone and level (seeded by sound,
    /// key and song position).
    pub const VARY: u32 = 24;
    /// 0..1000 ms: glide of tonal kinds (at least this much; drums ignore it).
    pub const GLIDE: u32 = 25;
    /// 0..200 %: how strongly the editor's visual reacts (the audio ignores it).
    pub const REACT: u32 = 26;
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
        ParamInfo::new(p::KIND, "Kind", 0.0, (KIND_NAMES.len() - 1) as f32, 0.0).choices(KIND_NAMES),
        ParamInfo::new(p::ATTACK, "Attack", -100.0, 100.0, 0.0).unit("%").short("Atk"),
        ParamInfo::new(p::DROP, "Drop", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::MOTION, "Motion", 0.0, 200.0, 100.0).unit("%").short("Mot"),
        ParamInfo::new(p::WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
        ParamInfo::new(p::DRIVE, "Drive", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::FILTER, "Filter", -100.0, 100.0, 0.0).unit("%").short("Filt"),
        ParamInfo::new(p::VARY, "Vary", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::GLIDE, "Glide", 0.0, 1000.0, 0.0).unit("ms").power(2.0),
        ParamInfo::new(p::REACT, "React", 0.0, 200.0, 100.0).unit("%"),
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

/// The tonal scene a device with these parameters plays: `stored` when it
/// was grown from `seed` and `kind`, else the scene they grow.
pub fn effective_patches(stored: Option<&PatchScene>, seed: u64, kind: Kind) -> PatchScene {
    match stored {
        Some(s) if s.seed == seed && s.kind == kind => s.clone(),
        _ => tonal::generate(seed, kind),
    }
}

/// The state blob for a scene and its corners (what the editor stores).
pub fn state_json(params: &std::collections::BTreeMap<u32, f32>, scene: &Scene, corners: &Corners) -> Vec<u8> {
    state_json_with(params, scene, corners, None)
}

/// [`state_json`] with the editor's visual look (`visual`: the key of the
/// visual's material; the sound never depends on it).
pub fn state_json_with(params: &std::collections::BTreeMap<u32, f32>, scene: &Scene, corners: &Corners, visual: Option<&str>) -> Vec<u8> {
    state_json_full(params, scene, corners, visual, None)
}

/// [`state_json_with`] with a tonal scene (`extra.patches`; stored only
/// when given, i.e. when the Kind parameter is tonal).
pub fn state_json_full(params: &std::collections::BTreeMap<u32, f32>, scene: &Scene, corners: &Corners, visual: Option<&str>, patches: Option<&PatchScene>) -> Vec<u8> {
    state_json_tonal(params, scene, corners, visual, patches, &Default::default())
}

/// [`state_json_full`] with the tonal morph corners (`extra.patch_corners`,
/// written only when one is set).
pub fn state_json_tonal(params: &std::collections::BTreeMap<u32, f32>, scene: &Scene, corners: &Corners, visual: Option<&str>, patches: Option<&PatchScene>, patch_corners: &PatchCorners) -> Vec<u8> {
    let mut p = serde_json::Map::new();
    for (k, v) in params {
        p.insert(k.to_string(), Value::from(f64::from(*v)));
    }
    let root = json!({ "version": 1, "params": Value::Object(p), "extra": extra_json(scene, corners, visual, patches, Some(patch_corners)) });
    serde_json::to_vec(&root).unwrap_or_default()
}

fn patch_corners_json(c: &PatchCorners) -> Value {
    Value::Array(c.iter().map(|c| c.as_ref().map_or(Value::Null, |s| serde_json::to_value(**s).unwrap_or(Value::Null))).collect())
}

fn extra_json(scene: &Scene, corners: &Corners, visual: Option<&str>, patches: Option<&PatchScene>, patch_corners: Option<&PatchCorners>) -> Value {
    let mut extra = json!({
        "scene": scene.to_json(),
        "corners": corners.iter().map(|c| c.as_ref().map_or(Value::Null, |s| serde_json::to_value(**s).unwrap_or(Value::Null))).collect::<Vec<_>>(),
    });
    if let Some(map) = extra.as_object_mut() {
        if let Some(v) = visual {
            map.insert("visual".into(), Value::from(v));
        }
        if let Some(t) = patches {
            map.insert("patches".into(), t.to_json());
        }
        if let Some(c) = patch_corners.filter(|c| c.iter().any(Option::is_some)) {
            map.insert("patch_corners".into(), patch_corners_json(c));
        }
    }
    extra
}

/// The tonal morph corners a state blob holds (`None` where unset).
pub fn parse_patch_corners(data: Option<&[u8]>) -> PatchCorners {
    let mut out: PatchCorners = Default::default();
    let Some(root) = data.and_then(|d| serde_json::from_slice::<Value>(d).ok()) else { return out };
    if let Some(list) = root.get("extra").and_then(|e| e.get("patch_corners")).and_then(Value::as_array) {
        for (slot, v) in out.iter_mut().zip(list) {
            *slot = serde_json::from_value::<[Patch; SOUNDS]>(v.clone()).ok().map(Box::new);
        }
    }
    out
}

/// `state` with tonal morph corner `k` (0..4 = A..D) set to `scene`'s
/// sixteen patches, or cleared (`None`); everything else in the blob is
/// kept. What the editor's corner buttons store for a tonal kind.
pub fn set_patch_corner(state: Option<&[u8]>, k: usize, scene: Option<&PatchScene>) -> Vec<u8> {
    let mut corners = parse_patch_corners(state);
    tonal::set_corner(&mut corners, k, scene);
    with_patch_corners(state, &corners)
}

/// `state` holding `corners` as its tonal morph corners (the key removed
/// when none is set); everything else is kept.
pub fn with_patch_corners(state: Option<&[u8]>, corners: &PatchCorners) -> Vec<u8> {
    let mut root = match state.and_then(|d| serde_json::from_slice::<Value>(d).ok()) {
        Some(Value::Object(m)) => m,
        _ => {
            let mut m = serde_json::Map::new();
            m.insert("version".into(), Value::from(1));
            m.insert("params".into(), json!({}));
            m
        }
    };
    let extra = root.entry("extra").or_insert_with(|| json!({}));
    if !extra.is_object() {
        *extra = json!({});
    }
    if let Some(map) = extra.as_object_mut() {
        if corners.iter().any(Option::is_some) {
            map.insert("patch_corners".into(), patch_corners_json(corners));
        } else {
            map.remove("patch_corners");
        }
    }
    serde_json::to_vec(&Value::Object(root)).unwrap_or_default()
}

/// `new` (a blob the editor rebuilt) with the tonal morph corners `old`
/// held, so rewriting the scene does not drop them.
pub fn keep_patch_corners(old: Option<&[u8]>, new: Vec<u8>) -> Vec<u8> {
    let corners = parse_patch_corners(old);
    if corners.iter().all(Option::is_none) || parse_patch_corners(Some(&new)).iter().any(Option::is_some) {
        return new;
    }
    with_patch_corners(Some(&new), &corners)
}

/// The parameters (normalised, only those it sets) and the state blob of a
/// Bloom playing pack member `m`: its Kind, Seed (the pack's for a tonal
/// member, the drum seed for drums), Species, the selected cell, Kit mode
/// for drums and Chromatic for tonal kinds, and its sounds in the state so
/// the device plays them as grown.
pub fn member_device(pack: &pack::Pack, m: &pack::Member) -> (std::collections::BTreeMap<u32, f32>, Vec<u8>) {
    let infos = params();
    let norm = |id: u32, plain: f32| infos.iter().find(|i| i.id == id).map_or(0.0, |i| i.to_normalized(plain));
    let species = pack.character.species;
    let mut params = std::collections::BTreeMap::new();
    params.insert(p::KIND, norm(p::KIND, m.kind.index() as f32));
    params.insert(p::SEED, norm(p::SEED, m.seed().min(MAX_SEED) as f32));
    params.insert(p::SPECIES, norm(p::SPECIES, species.index() as f32));
    params.insert(p::SELECTED, norm(p::SELECTED, m.selected.min(SOUNDS - 1) as f32));
    params.insert(p::MODE, norm(p::MODE, if m.kind.is_tonal() { 1.0 } else { 0.0 }));
    let state = match &m.sounds {
        pack::Sounds::Drums(scene) => state_json(&params, scene, &Default::default()),
        pack::Sounds::Tonal(patches) => {
            let drums = generate(patches.seed, GrowOpts { species, spread: 1.0 });
            state_json_full(&params, &drums, &Default::default(), None, Some(patches))
        }
    };
    (params, state)
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

/// The tonal scene a state blob holds, if any.
pub fn parse_patches(data: Option<&[u8]>) -> Option<PatchScene> {
    let root = data.and_then(|d| serde_json::from_slice::<Value>(d).ok())?;
    root.get("extra")?.get("patches").and_then(PatchScene::from_json)
}

struct Corner {
    sounds: [Genome; SOUNDS],
    gains: [f32; SOUNDS],
}

/// A tonal morph corner: sixteen patches and their measured levels.
struct TonalCorner {
    patches: [Patch; SOUNDS],
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

/// The output stage: width, drive and the DJ filter, each smoothed.
struct Output {
    width: f32,
    drive: f32,
    filter: f32,
    coeffs: SvfCoeffs,
    coeffs_at: f32,
    svf: [SvfState; 2],
}

impl Output {
    fn new() -> Self {
        Self { width: 1.0, drive: 0.0, filter: 0.0, coeffs: SvfCoeffs::passthrough(), coeffs_at: f32::NAN, svf: [SvfState::default(), SvfState::default()] }
    }

    fn reset(&mut self, width: f32, drive: f32, filter: f32) {
        self.width = width;
        self.drive = drive;
        self.filter = filter;
        self.coeffs_at = f32::NAN;
        for s in &mut self.svf {
            s.reset();
        }
    }

    /// The DJ filter's coefficients at position `f` (−1..1).
    fn configure(&mut self, f: f32, sr: f32) {
        let a = f.abs();
        let (mode, fc) = if f < 0.0 { (SvfMode::Lowpass, 20_000.0 * 2f32.powf(-a * 9.6)) } else { (SvfMode::Highpass, 20.0 * 2f32.powf(a * 9.6)) };
        let q = 0.707 + 1.6 * a;
        self.coeffs = SvfCoeffs::new(mode, fc.clamp(20.0, sr * 0.45), q, 0.0, f64::from(sr));
        self.coeffs_at = f;
    }
}

/// Saturation with its level compensated (a −10 dBFS sine keeps its level).
#[inline]
fn saturate(x: f32, gain: f32, comp: f32) -> f32 {
    soft(x * gain) * comp
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
    // The tonal engine.
    kind: Kind,
    tonal: Box<PatchScene>,
    tonal_gains: [f32; SOUNDS],
    tonal_measured: u16,
    leveler: Box<Leveler>,
    synth: Box<WavetableSynth>,
    synth_buf: AudioBuffer,
    synth_events: Vec<Event>,
    /// Send every synth note off at the next block.
    synth_kill: bool,
    /// Frames the synth still renders after the kind left the tonal ones.
    synth_tail: usize,
    /// What the synth holds (normalised by slot; NaN = not sent yet).
    applied: Box<[f32; PATCH_LEN]>,
    synth_gain: f32,
    synth_gain_now: f32,
    synth_env: f32,
    synth_prev: f32,
    synth_dc: [tonal::DcBlock; 2],
    tonal_noise: f32,
    /// Tonal morph corners (a corner of another kind is ignored).
    patch_corners: [Option<Box<TonalCorner>>; 4],
    /// Where the tonal morph is, gliding towards `morph`.
    morph_now: (f32, f32),
    /// The morph position the synth last received.
    morph_applied: (f32, f32),
    // Where the block is, for Vary's seeds.
    block_pos: i64,
    playing: bool,
    seg: usize,
    hit_count: u32,
    out: Output,
    // Cached parameters.
    macros: Macros,
    perform: Perform,
    vary: f32,
    glide: f32,
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
    width: f32,
    drive_amt: f32,
    filter_amt: f32,
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

/// A seeded value in −1..1 (Vary), from arithmetic only.
#[inline]
fn vary_draw(seed: u64, k: u64) -> f32 {
    let x = rng::splitmix64(seed ^ k.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

impl Bloom {
    pub fn new() -> Self {
        crate::instrument::vault::kit::init();
        // The tonal priors and patch layout: built here, never on the audio thread.
        let _ = tonal::priors();
        let params = params();
        let (scene, gains) = default_scene().clone();
        let idle = Voice::blank();
        let mut synth = Box::new(WavetableSynth::new());
        synth.prepare(48_000.0, 512);
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Bloom",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["generative", "drums", "percussion", "random", "kit", "resonator", "fm", "synth", "bass", "pad", "lead"],
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
            kind: Kind::Drums,
            tonal: Box::new(PatchScene::empty(Kind::Drums)),
            tonal_gains: [1.0; SOUNDS],
            tonal_measured: 0,
            leveler: Box::new(Leveler::new()),
            synth,
            synth_buf: AudioBuffer::new(2, 512),
            synth_events: Vec::with_capacity(SYNTH_EVENTS),
            synth_kill: false,
            synth_tail: 0,
            applied: Box::new([f32::NAN; PATCH_LEN]),
            synth_gain: 1.0,
            synth_gain_now: 1.0,
            synth_env: 0.0,
            synth_prev: 0.0,
            synth_dc: Default::default(),
            tonal_noise: 0.0,
            patch_corners: Default::default(),
            morph_now: (0.0, 0.0),
            morph_applied: (0.0, 0.0),
            block_pos: 0,
            playing: false,
            seg: 0,
            hit_count: 0,
            out: Output::new(),
            macros: Macros::default(),
            perform: Perform::default(),
            vary: 0.0,
            glide: 0.0,
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
            width: 1.0,
            drive_amt: 0.0,
            filter_amt: 0.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    /// A device playing `patch` (in every cell) at its measured level,
    /// Chromatic mode, every knob at its default: what a tonal sound's
    /// export, drag and audition render through. Allocates; control thread.
    pub fn with_patch(patch: &Patch) -> Self {
        let mut b = Self::new();
        let kind = if patch.kind.is_tonal() { patch.kind } else { Kind::Bass };
        let set = |b: &mut Self, id: u32, plain: f32| {
            let n = b.info.params.iter().find(|i| i.id == id).map_or(0.0, |i| i.to_normalized(plain));
            b.params.set(id, n);
        };
        set(&mut b, p::KIND, kind.index() as f32);
        set(&mut b, p::MODE, 1.0);
        let seed = seed_of(b.params.plain(p::SEED));
        let mut scene = PatchScene::empty(kind);
        scene.seed = seed;
        let mut one = patch.folded();
        one.kind = kind;
        scene.patches = [one; SOUNDS];
        scene.cores = [one; SOUNDS];
        let gain = b.leveler.measure(&one);
        *b.tonal = scene;
        b.tonal_gains = [gain; SOUNDS];
        b.tonal_measured = u16::MAX;
        b.params.mark_dirty();
        b.update();
        b.synth_gain_now = b.synth_gain;
        b
    }

    /// The drum scene (what Drums plays).
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Normalised levels of the drum scene's sounds.
    pub fn gains(&self) -> &[f32; SOUNDS] {
        &self.gains
    }

    /// The kind playing.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The tonal scene (meaningful while the kind is tonal).
    pub fn patches(&self) -> &PatchScene {
        &self.tonal
    }

    /// Level gains of the tonal scene's patches.
    pub fn patch_gains(&self) -> &[f32; SOUNDS] {
        &self.tonal_gains
    }

    /// Where the tonal morph is (it glides towards Morph X/Y).
    pub fn morph_position(&self) -> (f32, f32) {
        self.morph_now
    }

    /// The selected tonal patch and its level as the device plays them now
    /// (morphed between the stored corners; before the knobs).
    pub fn playing_patch(&self) -> (Patch, f32) {
        (self.selected_patch(), self.selected_gain())
    }

    /// Whether a tonal morph corner of the playing kind is stored.
    fn morphing(&self) -> bool {
        self.kind.is_tonal() && self.patch_corners.iter().any(|c| c.as_ref().is_some_and(|c| c.patches[0].kind == self.kind))
    }

    /// Cell `i` of corner `k`: the corner's patch and level when it holds
    /// the playing kind, else the scene's.
    fn corner(&self, k: usize, i: usize) -> (&Patch, f32) {
        match &self.patch_corners[k] {
            Some(c) if c.patches[i].kind == self.kind => (&c.patches[i], c.gains[i]),
            _ => (&self.tonal.patches[i], self.tonal_gains[i]),
        }
    }

    /// The selected tonal patch as it plays: morphed between the corners
    /// at the gliding position. No allocation.
    fn selected_patch(&self) -> Patch {
        let i = self.selected;
        if !self.morphing() {
            return self.tonal.patches[i];
        }
        let c = [self.corner(0, i).0, self.corner(1, i).0, self.corner(2, i).0, self.corner(3, i).0];
        tonal::morph4(c, self.morph_now.0, self.morph_now.1)
    }

    /// The level of [`Self::selected_patch`]: the corners' measured levels
    /// interpolated in dB.
    fn selected_gain(&self) -> f32 {
        let i = self.selected;
        if !self.morphing() {
            return self.tonal_gains[i];
        }
        let g = [self.corner(0, i).1, self.corner(1, i).1, self.corner(2, i).1, self.corner(3, i).1];
        tonal::morph_gain(g, self.morph_now.0, self.morph_now.1)
    }

    /// Glide the tonal morph one block towards Morph X/Y and send the synth
    /// the new blend once it has moved [`MORPH_STEP`] (or arrived).
    fn step_morph(&mut self, frames: usize) {
        if !self.morphing() {
            self.morph_now = self.morph;
            self.morph_applied = self.morph;
            return;
        }
        if self.morph_now == self.morph && self.morph_applied == self.morph {
            return;
        }
        let k = 1.0 - (-(frames as f32) / (MORPH_GLIDE * self.sr)).exp();
        let (tx, ty) = self.morph;
        let (mut x, mut y) = self.morph_now;
        x += (tx - x) * k;
        y += (ty - y) * k;
        if (tx - x).abs() < 1e-4 {
            x = tx;
        }
        if (ty - y).abs() < 1e-4 {
            y = ty;
        }
        self.morph_now = (x, y);
        let moved = (x - self.morph_applied.0).abs() + (y - self.morph_applied.1).abs();
        if moved >= MORPH_STEP || self.morph_now == self.morph {
            self.apply_patch();
        }
    }

    fn target(&self) -> (u64, Species) {
        (seed_of(self.params.plain(p::SEED)), Species::from_index(self.params.choice(p::SPECIES)))
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.macros = Macros { decay: pr.plain(p::DECAY) / 100.0, body: pr.plain(p::BODY) / 100.0, tone: pr.plain(p::TONE) / 100.0, grit: pr.plain(p::GRIT) / 100.0 };
        self.perform = Perform { attack: pr.plain(p::ATTACK) / 100.0, drop: pr.plain(p::DROP) / 100.0, motion: pr.plain(p::MOTION) / 100.0, tone: 0.0, level: 1.0 };
        self.vary = pr.plain(p::VARY) / 100.0;
        self.glide = pr.plain(p::GLIDE);
        self.width = pr.plain(p::WIDTH) / 100.0;
        self.drive_amt = pr.plain(p::DRIVE) / 100.0;
        self.filter_amt = pr.plain(p::FILTER) / 100.0;
        self.vel_amount = pr.plain(p::VELOCITY) / 100.0;
        let mode = pr.choice(p::MODE);
        self.base = pr.plain(p::BASE).round() as i32;
        self.choke = pr.choice(p::CHOKE) == 1;
        self.selected = pr.choice(p::SELECTED).min(SOUNDS - 1);
        self.morph = (pr.plain(p::MORPH_X), pr.plain(p::MORPH_Y));
        self.tune = pr.plain(p::TUNE);
        self.volume = db_to_gain(pr.plain(p::VOLUME));
        self.space_amt = pr.plain(p::SPACE) / 100.0;
        let kind = Kind::from_index(pr.choice(p::KIND));
        let (seed, species) = self.target();
        if seed != self.scene.seed || species != self.scene.species {
            // Grow in place: no allocation; levels follow one per block.
            *self.scene = generate(seed, GrowOpts { species, spread: 1.0 });
            self.measured = 0;
        }
        if kind != self.kind {
            if kind.is_tonal() && !self.kind.is_tonal() {
                self.all_off();
            }
            self.synth_kill = true;
            self.kind = kind;
        }
        if mode != self.mode && kind.is_tonal() {
            // Keys map differently: end what the synth holds.
            self.synth_kill = true;
        }
        self.mode = mode;
        if kind.is_tonal() {
            if self.tonal.seed != seed || self.tonal.kind != kind {
                tonal::generate_into(&mut self.tonal, seed, kind);
                self.tonal_measured = 0;
                self.leveler.cancel();
            }
            self.apply_patch();
        }
    }

    /// Send the selected patch, shaped by the knobs, to the synth: only the
    /// values that changed. No allocation.
    fn apply_patch(&mut self) {
        let knobs = Knobs { macros: self.macros, space: self.space_amt, velocity: self.vel_amount, attack: self.perform.attack, drop: self.perform.drop, motion: self.perform.motion, glide: self.glide };
        let patch = self.selected_patch();
        let shaped = tonal::shape_knobs(&patch, &knobs);
        let l = patch::layout();
        for k in 0..l.len {
            let v = shaped.v[k];
            if self.applied[k].to_bits() != v.to_bits() {
                self.synth.set_param(l.ids[k], v);
                self.applied[k] = v;
            }
        }
        self.tonal_noise = tonal::noise_share(&patch);
        self.synth_gain = self.selected_gain();
        self.morph_applied = self.morph_now;
    }

    /// Measure the next tonal patch still waiting (a chunk per block, the
    /// selected one first).
    fn measure_tonal(&mut self) {
        if !self.kind.is_tonal() || self.tonal_measured == u16::MAX {
            return;
        }
        if self.leveler.busy().is_none() {
            let sel = self.selected;
            let i = if self.tonal_measured & (1 << sel) == 0 { sel } else { (0..SOUNDS).find(|i| self.tonal_measured & (1 << i) == 0).unwrap_or(0) };
            self.leveler.start(i, &self.tonal.patches[i]);
        }
        if let Some((i, g)) = self.leveler.step() {
            self.tonal_gains[i] = g;
            self.tonal_measured |= 1 << i;
            if i == self.selected {
                self.synth_gain = self.selected_gain();
            }
        }
    }

    fn measure_tonal_all(&mut self) {
        self.leveler.cancel();
        if !self.kind.is_tonal() {
            return;
        }
        for i in 0..SOUNDS {
            if self.tonal_measured & (1 << i) == 0 {
                self.tonal_gains[i] = self.leveler.measure(&self.tonal.patches[i]);
                self.tonal_measured |= 1 << i;
            }
        }
        self.synth_gain = self.selected_gain();
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

    /// The seed of a hit's Vary: the sound, the key and where the hit is
    /// (the song position while playing, else a hit counter).
    fn vary_seed(&mut self, sound: usize, key: u8, offset: i64) -> u64 {
        self.hit_count = self.hit_count.wrapping_add(1);
        let at = if self.playing { (self.block_pos + offset) as u64 } else { u64::from(self.hit_count) ^ 0xA5A5_0000_0000 };
        rng::part(at, sound as u64, u64::from(key))
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
        let mut perform = self.perform;
        let mut pitch = 0.0;
        if self.vary > 0.0 {
            let s = self.vary_seed(sound, key, self.seg as i64);
            pitch = vary_draw(s, 1) * 0.6 * self.vary;
            perform.tone = vary_draw(s, 2) * 0.7 * self.vary;
            perform.level = 1.0 + vary_draw(s, 3) * 0.25 * self.vary;
        }
        let mut patch = DrumPatch::new(&genome, self.sr, transpose + self.tune + pitch, velocity, self.vel_amount, &self.macros, gain);
        patch.perform(&perform);
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

    /// The key the synth plays for an incoming key.
    fn synth_key(&self, key: u8) -> u8 {
        if self.mode == 2 { 60 } else { key }
    }

    /// Run the synth over the whole block into `synth_buf` (tonal notes and
    /// controllers; Bloom's own parameters reach it as patch values).
    fn render_synth(&mut self, ctx: &ProcessContext<'_>) {
        let frames = ctx.buffer.frames();
        self.synth_buf.set_frames(frames);
        self.synth_buf.clear();
        self.synth_events.clear();
        if std::mem::take(&mut self.synth_kill) {
            self.synth_events.push(Event { offset: 0, kind: EventKind::AllNotesOff });
        }
        let tonal = self.kind.is_tonal();
        let cap = self.synth_events.capacity();
        for ev in ctx.events {
            if self.synth_events.len() + 2 > cap {
                break;
            }
            match ev.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } if tonal => {
                    let k = self.synth_key(key);
                    let (mut tune, mut vel) = (self.tune, velocity);
                    if self.vary > 0.0 && velocity > 0.0 {
                        let s = self.vary_seed(self.selected, key, i64::from(ev.offset));
                        tune += vary_draw(s, 1) * 0.3 * self.vary;
                        vel = (vel * (1.0 + vary_draw(s, 3) * 0.25 * self.vary)).clamp(0.01, 1.0);
                    }
                    self.synth_events.push(Event { offset: ev.offset, kind: EventKind::NoteOn { channel, key: k, velocity: vel, note_id } });
                    if tune != 0.0 && velocity > 0.0 {
                        self.synth_events.push(Event { offset: ev.offset, kind: EventKind::NoteExpression { note_id, key: k, expr: Expression::Tuning, value: tune } });
                    }
                }
                EventKind::NoteOff { channel, key, velocity, note_id } => {
                    let k = self.synth_key(key);
                    self.synth_events.push(Event { offset: ev.offset, kind: EventKind::NoteOff { channel, key: k, velocity, note_id } });
                }
                EventKind::PitchBend { .. } | EventKind::ControlChange { .. } | EventKind::ChannelPressure { .. } | EventKind::AllNotesOff => self.synth_events.push(*ev),
                EventKind::NoteExpression { note_id, key, expr, value } if tonal => {
                    let value = if expr == Expression::Tuning { value + self.tune } else { value };
                    self.synth_events.push(Event { offset: ev.offset, kind: EventKind::NoteExpression { note_id, key: self.synth_key(key), expr, value } });
                }
                _ => {}
            }
        }
        let mut sub = ProcessContext { sample_rate: ctx.sample_rate, buffer: &mut self.synth_buf, sidechain: None, events: &self.synth_events, transport: ctx.transport };
        self.synth.process(&mut sub);
        if tonal {
            self.synth_tail = self.synth.tail() as usize + frames;
        } else {
            self.synth_tail = self.synth_tail.saturating_sub(frames);
        }
        let mut m = [0f32; 16];
        let n = self.synth.meter(&mut m);
        // The newest voice's amp envelope (meter layout of the synth).
        self.synth_env = if n > 10 && m[0] > 0.0 { m[10].clamp(0.0, 1.0) } else { 0.0 };
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
                if self.kind.is_tonal() {
                    if velocity > 0.0 {
                        let s = self.selected;
                        self.meters.pulse[s] = 1.0;
                        self.meters.hits[s] = (self.meters.hits[s] + 1.0) % 4096.0;
                    }
                } else {
                    self.note_on(key, velocity);
                }
            }
            EventKind::NoteOff { key, .. } => {
                if !self.kind.is_tonal() {
                    self.note_off(key);
                }
            }
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => self.all_off(),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        self.seg = end;
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
        // The synth's share of the block.
        let synth_on = self.synth_buf.frames() >= end && (self.kind.is_tonal() || self.synth_tail > 0);
        let (mut t_sig, mut t_diff) = (0.0f32, 0.0f32);
        if synth_on {
            let g_from = self.synth_gain_now;
            self.synth_gain_now += (self.synth_gain - self.synth_gain_now) * k;
            let (sl, sr) = (self.synth_buf.channel(0), self.synth_buf.channel(1));
            let mut prev = self.synth_prev;
            let pole = tonal::DcBlock::pole(self.sr);
            let dc = &mut self.synth_dc;
            for i in 0..n {
                let gn = g_from + (self.synth_gain_now - g_from) * (i as f32 / n as f32);
                let (l, r) = (dc[0].process(sl[start + i], pole) * gn, dc[1].process(sr[start + i], pole) * gn);
                bl[i] += l;
                br[i] += r;
                let m = (l + r) * 0.5;
                bs[i] += m * TONAL_SEND;
                t_sig += m * m;
                t_diff += (m - prev) * (m - prev);
                prev = m;
            }
            self.synth_prev = prev;
        }
        self.space_now += (self.space_amt - self.space_now) * k;
        // An idle scene with a silent tail costs nothing (a Drum Rack holds sixteen).
        if active > 0 || synth_on || self.space.level > 1e-7 {
            self.space.process(bs, bl, br, self.space_now * 1.6);
        }
        // The output stage: volume, width, drive, the DJ filter.
        let vol_from = self.vol_now;
        self.vol_now += (self.volume - self.vol_now) * k;
        let o = &mut self.out;
        let (w_from, d_from) = (o.width, o.drive);
        o.width += (self.width - o.width) * k;
        o.drive += (self.drive_amt - o.drive) * k;
        if (o.width - self.width).abs() < 1e-4 {
            o.width = self.width;
        }
        if (o.drive - self.drive_amt).abs() < 1e-5 {
            o.drive = self.drive_amt;
        }
        let f_from = o.filter;
        o.filter += (self.filter_amt - o.filter) * k;
        if (o.filter - self.filter_amt).abs() < 1e-4 {
            o.filter = self.filter_amt;
        }
        let width_on = w_from != 1.0 || o.width != 1.0;
        let drive_on = d_from > 0.0 || o.drive > 0.0;
        let filter_on = f_from != 0.0 || o.filter != 0.0;
        if !filter_on && !o.coeffs_at.is_nan() {
            // Back at 0: bypassed, and fresh when it comes back.
            o.reset(o.width, o.drive, 0.0);
        }
        let buf = &mut *ctx.buffer;
        let (mut sum, mut peak) = (0.0f32, 0.0f32);
        for i in 0..n {
            let t = i as f32 / n as f32;
            let vol = vol_from + (self.vol_now - vol_from) * t;
            let (mut l, mut r) = (bl[i] * vol, br[i] * vol);
            if width_on {
                let wd = w_from + (o.width - w_from) * t;
                let (m, s) = ((l + r) * 0.5, (l - r) * 0.5 * wd);
                l = m + s;
                r = m - s;
            }
            if drive_on {
                let d = d_from + (o.drive - d_from) * t;
                let gain = 1.0 + 9.0 * d;
                let comp = 0.3 / soft(0.3 * gain);
                let wet = (d * 10.0).min(1.0);
                l += (saturate(l, gain, comp) - l) * wet;
                r += (saturate(r, gain, comp) - r) * wet;
            }
            if filter_on {
                if i % 32 == 0 {
                    let f = f_from + (o.filter - f_from) * t;
                    if (f - o.coeffs_at).abs() > 1e-4 || o.coeffs_at.is_nan() {
                        o.configure(f, self.sr);
                    }
                }
                l = o.svf[0].process(&o.coeffs, l);
                r = o.svf[1].process(&o.coeffs, r);
            }
            let (l, r) = (crate::common::sanitize(l).clamp(-4.0, 4.0), crate::common::sanitize(r).clamp(-4.0, 4.0));
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
        if synth_on {
            let s = self.selected;
            env[s] = env[s].max(self.synth_env);
            sig[s] += t_sig;
            diff[s] += t_diff;
            noise[s] += t_sig * self.tonal_noise;
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
        self.synth.prepare(sample_rate, max_frames.max(1));
        self.synth_buf = AudioBuffer::new(2, max_frames.max(1));
        self.space.prepare(self.sr);
        self.params.mark_dirty();
        self.update();
        self.measure_all();
        self.measure_tonal_all();
        self.apply_patch_if_tonal();
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
        self.synth.restart();
        self.synth_kill = false;
        self.synth_tail = 0;
        self.synth_gain_now = self.synth_gain;
        self.synth_env = 0.0;
        self.synth_prev = 0.0;
        self.synth_dc = Default::default();
        self.hit_count = 0;
        if self.morph_now != self.morph {
            self.morph_now = self.morph;
            self.apply_patch_if_tonal();
        }
        self.out.reset(self.width, self.drive_amt, self.filter_amt);
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.measure_next();
        let frames = ctx.buffer.frames();
        if frames > self.bus_l.len() {
            // A block larger than prepared: render silence rather than allocate.
            ctx.buffer.clear();
            return;
        }
        self.block_pos = ctx.transport.position_samples;
        self.playing = ctx.transport.playing;
        self.seg = 0;
        let kind_event = ctx.events.iter().any(|e| matches!(e.kind, EventKind::Param { id: p::KIND, .. }));
        if self.kind.is_tonal() || kind_event || self.synth_tail > 0 {
            // The synth runs over the whole block: Bloom's parameter changes
            // reach it at the block start.
            for ev in ctx.events {
                if let EventKind::Param { id, value } = ev.kind {
                    self.params.set(id, value);
                }
            }
            if self.params.take_dirty() {
                self.update();
            }
            self.step_morph(frames);
            self.measure_tonal();
            self.render_synth(ctx);
        } else {
            self.synth_buf.set_frames(0);
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
        let drums = (self.sr * 6.0) as u32;
        if self.kind.is_tonal() { drums.max(self.synth.tail()) } else { drums }
    }

    fn save_state(&self) -> Vec<u8> {
        let corners: Corners = std::array::from_fn(|k| self.corners[k].as_ref().map(|c| Box::new(c.sounds)));
        let patches = self.kind.is_tonal().then_some(&*self.tonal);
        let patch_corners: PatchCorners = std::array::from_fn(|k| self.patch_corners[k].as_ref().map(|c| Box::new(c.patches)));
        save_state(&self.params, extra_json(&self.scene, &corners, self.visual.as_deref(), patches, Some(&patch_corners)))
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
        for (slot, c) in self.patch_corners.iter_mut().zip(parse_patch_corners(wrapped.as_deref())) {
            *slot = c.map(|patches| {
                let gains = std::array::from_fn(|i| self.leveler.measure(&patches[i]));
                Box::new(TonalCorner { patches: *patches, gains })
            });
        }
        let kind = Kind::from_index(self.params.choice(p::KIND));
        if kind.is_tonal() {
            let stored = parse_patches(wrapped.as_deref());
            *self.tonal = effective_patches(stored.as_ref(), seed, kind);
            self.tonal_measured = 0;
            self.kind = kind;
            self.measure_tonal_all();
            self.synth_kill = true;
        }
        self.params.mark_dirty();
        self.update();
        self.morph_now = self.morph;
        self.apply_patch_if_tonal();
        self.synth_gain_now = self.synth_gain;
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
        let measured = if self.kind.is_tonal() { self.tonal_measured } else { self.measured };
        buf[84] = measured.count_ones() as f32 / SOUNDS as f32;
        buf[85] = self.scene.seed as f32;
        buf[86] = self.scene.species.index() as f32;
        buf[87] = self.kind.index() as f32;
        buf[88] = if self.kind.is_tonal() { self.synth_gain } else { 0.0 };
        crate::common::write_meter(out, 0, &buf)
    }
}

impl Bloom {
    fn apply_patch_if_tonal(&mut self) {
        if self.kind.is_tonal() {
            self.apply_patch();
        }
    }
}

// ── Offline renders of tonal patches ────────────────────────────────────────

/// Render notes `(frame, key, velocity, on)` through `b` at `sr` in blocks
/// of 256 (prepares it). Control thread.
pub fn render_device(b: &mut Bloom, notes: &[(usize, u8, f32, bool)], frames: usize, sr: f64) -> (Vec<f32>, Vec<f32>) {
    const BLOCK: usize = 256;
    b.prepare(sr, BLOCK);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let (mut l, mut r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let tr = Transport::default();
    let mut events = Vec::new();
    let mut pos = 0;
    while pos < frames {
        let len = BLOCK.min(frames - pos);
        buf.set_frames(len);
        buf.clear();
        events.clear();
        for &(f, key, vel, on) in notes {
            if f >= pos && f < pos + len {
                let kind = if on { EventKind::NoteOn { channel: 0, key, velocity: vel, note_id: -1 } } else { EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: -1 } };
                events.push(Event { offset: (f - pos) as u32, kind });
            }
        }
        let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut buf, sidechain: None, events: &events, transport: &tr };
        b.process(&mut ctx);
        l.extend_from_slice(buf.channel(0));
        r.extend_from_slice(buf.channel(1));
        pos += len;
    }
    (l, r)
}

/// The note a kind is heard with: `(key, hold seconds, tail seconds)` — a
/// held bass note, a lead note, a long pad, a plucked note, a struck key,
/// an FX hit, a texture swell.
pub fn demo_note(kind: Kind) -> (u8, f32, f32) {
    match kind {
        Kind::Bass => (36, 0.7, 0.4),
        Kind::Lead => (67, 1.0, 0.6),
        Kind::Pad => (60, 2.2, 2.0),
        Kind::Pluck => (64, 0.25, 1.0),
        Kind::Keys => (60, 0.9, 0.8),
        Kind::Fx => (60, 2.0, 1.5),
        Kind::Texture => (60, 2.5, 2.0),
        Kind::Drums => (60, 0.5, 0.5),
    }
}

/// Render `patch` (its kind's [`demo_note`], velocity 0.8) as the device
/// plays it with `knobs` and `tune`: what an export or a drag of a tonal
/// sound writes. Control thread (allocates a device).
pub fn render_patch(patch: &Patch, knobs: &Knobs, tune: f32, sr: f64) -> (Vec<f32>, Vec<f32>) {
    let mut b = Bloom::with_patch(patch);
    set_knobs(&mut b, knobs, tune);
    let (key, hold, tail) = demo_note(patch.kind);
    let on = (hold as f64 * sr) as usize;
    let frames = on + (tail as f64 * sr) as usize;
    render_device(&mut b, &[(0, key, 0.8, true), (on, key, 0.0, false)], frames, sr)
}

/// Set a device's knobs from [`Knobs`] (plain values on its parameters).
pub fn set_knobs(b: &mut Bloom, k: &Knobs, tune: f32) {
    let vals = [
        (p::DECAY, k.macros.decay * 100.0),
        (p::BODY, k.macros.body * 100.0),
        (p::TONE, k.macros.tone * 100.0),
        (p::GRIT, k.macros.grit * 100.0),
        (p::SPACE, k.space * 100.0),
        (p::VELOCITY, k.velocity * 100.0),
        (p::ATTACK, k.attack * 100.0),
        (p::DROP, k.drop * 100.0),
        (p::MOTION, k.motion * 100.0),
        (p::GLIDE, k.glide),
        (p::TUNE, tune),
    ];
    for (id, plain) in vals {
        if let Some(info) = b.info.params.iter().find(|i| i.id == id) {
            let n = info.to_normalized(plain);
            b.params.set(id, n);
        }
    }
    if b.params.take_dirty() {
        b.update();
    }
}

/// The timbre of a tonal patch as the device plays it (its Tune and
/// knobs): the first 750 ms of its demo note at [`timbre::RATE`], measured
/// like a drum sound, with the patch's modulation as the wobble. Off the
/// audio and UI threads.
pub fn patch_timbre(patch: &Patch, knobs: &Knobs, tune: f32) -> timbre::Timbre {
    let (l, r) = render_patch(patch, knobs, tune, f64::from(timbre::RATE));
    let n = ((timbre::RATE * 0.75) as usize).min(l.len());
    let mono: Vec<f32> = l[..n].iter().zip(&r[..n]).map(|(a, b)| (a + b) * 0.5).collect();
    let mut t = timbre::analyze_signal(&mono, timbre::RATE);
    t.wobble = tonal::motion(&tonal::shape_knobs(patch, knobs));
    t
}

/// A generated patch as an `instrument.wavetable` preset named `name`,
/// shaped by `knobs` and at the level Bloom plays it.
pub fn patch_preset(patch: &Patch, knobs: &Knobs, name: &str) -> crate::presets::Preset {
    let shaped = tonal::shape_knobs(patch, knobs);
    let mut preset = shaped.to_preset(name, tonal::measure_gain(patch));
    preset.tags.push(tonal::patch_name(patch).to_ascii_lowercase());
    preset
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

    fn tonal(kind: Kind) -> Bloom {
        let mut b = Bloom::new();
        set_plain(&mut b, p::KIND, kind.index() as f32);
        b
    }

    #[test]
    fn the_performance_knobs_at_their_defaults_leave_the_drums_alone() {
        let notes = [(0, 36, 1.0, true), (2_000, 38, 0.8, true), (5_000, 42, 0.6, true)];
        let mut a = Bloom::new();
        let (la, ra) = render_notes(&mut a, &notes, 12_000);
        let mut b = Bloom::new();
        for (id, v) in [(p::ATTACK, 0.0), (p::DROP, 0.0), (p::MOTION, 100.0), (p::WIDTH, 100.0), (p::DRIVE, 0.0), (p::FILTER, 0.0), (p::VARY, 0.0), (p::GLIDE, 300.0), (p::REACT, 170.0)] {
            set_plain(&mut b, id, v);
        }
        let (lb, rb) = render_notes(&mut b, &notes, 12_000);
        assert_eq!(la, lb, "Glide and React do nothing to drums, the rest is the identity");
        assert_eq!(ra, rb);
        assert_eq!(b.kind(), Kind::Drums);
        let defaults = params();
        for id in p::KIND..=p::REACT {
            let info = defaults.iter().find(|i| i.id == id).unwrap();
            assert_eq!(a.get_param(id), info.default_normalized());
        }
    }

    #[test]
    fn the_performance_knobs_shape_drums() {
        let hit = [(0, 36, 1.0, true)];
        let base = render_notes(&mut Bloom::new(), &hit, 9_600);
        let with = |id: u32, v: f32| {
            let mut b = Bloom::new();
            set_plain(&mut b, id, v);
            render_notes(&mut b, &hit, 9_600)
        };
        let diff = |a: &(Vec<f32>, Vec<f32>), b: &(Vec<f32>, Vec<f32>)| a.0.iter().zip(&b.0).map(|(x, y)| (x - y).abs()).sum::<f32>();
        for (id, v) in [(p::ATTACK, 80.0), (p::DROP, 70.0), (p::MOTION, 200.0), (p::DRIVE, 80.0), (p::FILTER, -70.0), (p::VARY, 100.0)] {
            assert!(diff(&base, &with(id, v)) > 0.5, "param {id} does nothing");
        }
        // Width 0 is mono.
        let (l, r) = with(p::WIDTH, 0.0);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-5));
        // Soft attack: the first 2 ms are quieter.
        let soft = with(p::ATTACK, 100.0);
        assert!(rms(&soft.0[..96]) < rms(&base.0[..96]) * 0.5);
        // The DJ filter darkens a hat.
        let hat = [(0, 42, 1.0, true)];
        let open = render_notes(&mut Bloom::new(), &hat, 6_000);
        let mut f = Bloom::new();
        set_plain(&mut f, p::FILTER, -80.0);
        let dark = render_notes(&mut f, &hat, 6_000);
        assert!(rms(&dark.0) < rms(&open.0) * 0.5, "{} vs {}", rms(&dark.0), rms(&open.0));
        // Vary is seeded: two devices agree.
        let a = with(p::VARY, 100.0);
        let b = with(p::VARY, 100.0);
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn tonal_kinds_play_the_selected_patch_chromatically() {
        for kind in Kind::TONAL {
            let mut b = tonal(kind);
            let (key, _, _) = demo_note(kind);
            let (l, _) = render_notes(&mut b, &[(0, key, 0.9, true), (20_000, key, 0.0, false)], 40_000);
            assert!(l.iter().all(|v| v.is_finite()));
            assert!(rms(&l) > 1e-3, "{kind:?} is silent");
            assert_eq!(b.kind(), kind);
            assert_eq!(b.patches().kind, kind);
            assert_eq!(b.patches().seed, DEFAULT_SEED);
            let mut m = [0f32; 320];
            assert_eq!(b.meter(&mut m), METER_LEN);
            assert_eq!(m[87], kind.index() as f32);
            assert_eq!(m[84], 1.0);
            assert!(m[32] >= 1.0, "the selected cell counts the note");
        }
        // Another selection is another sound; an octave up is an octave up.
        let mut b = tonal(Kind::Lead);
        let (a, _) = render_notes(&mut b, &[(0, 60, 0.9, true)], 9_600);
        set_plain(&mut b, p::SELECTED, 5.0);
        let (c, _) = render_notes(&mut b, &[(0, 60, 0.9, true)], 9_600);
        assert!(a.iter().zip(&c).map(|(x, y)| (x - y).abs()).sum::<f32>() > 1.0);
        // Single mode: every key plays the patch at C3.
        set_plain(&mut b, p::MODE, 2.0);
        let (s1, _) = render_notes(&mut b, &[(0, 40, 0.9, true)], 9_600);
        let (s2, _) = render_notes(&mut b, &[(0, 60, 0.9, true)], 9_600);
        assert_eq!(s1, s2);
    }

    #[test]
    fn tonal_live_and_offline_renders_match() {
        let notes = [(0, 48, 0.9, true), (5_000, 48, 0.0, false), (6_000, 55, 0.7, true), (14_000, 55, 0.0, false)];
        for kind in [Kind::Bass, Kind::Pad, Kind::Pluck] {
            let (a, _) = render_notes(&mut tonal(kind), &notes, 24_000);
            let (b, _) = render_notes(&mut tonal(kind), &notes, 24_000);
            assert_eq!(a, b, "{kind:?}");
        }
        // The export path is the device.
        let b = tonal(Kind::Keys);
        let patch = b.patches().patches[0];
        let (l1, _) = render_patch(&patch, &Knobs::default(), 0.0, 48_000.0);
        let (l2, _) = render_patch(&patch, &Knobs::default(), 0.0, 48_000.0);
        assert_eq!(l1, l2);
        assert!(rms(&l1) > 1e-3);
    }

    #[test]
    fn a_seed_automation_regrows_the_tonal_scene_on_the_audio_side() {
        let mut b = tonal(Kind::Pluck);
        let _ = render_notes(&mut b, &[], 256);
        let before = b.patches().clone();
        set_plain(&mut b, p::SEED, 4242.0);
        let _ = render_notes(&mut b, &[], 128);
        assert_eq!(b.patches().seed, 4242);
        assert_ne!(b.patches().patches, before.patches);
        assert_eq!(*b.patches(), tonal::generate(4242, Kind::Pluck), "the same scene as grown off-thread");
        // render_notes prepares (measuring everything); measure in blocks instead.
        let mut buf = AudioBuffer::new(2, 128);
        let tr = Transport::default();
        let ev = [Event { offset: 0, kind: EventKind::Param { id: p::SEED, value: b.info.params[0].to_normalized(9_999.0) } }];
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &ev, transport: &tr };
        b.process(&mut ctx);
        let mut m = [0f32; 320];
        b.meter(&mut m);
        assert!(m[84] < 1.0, "levels are measured over the next blocks");
        for _ in 0..400 {
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
            b.process(&mut ctx);
        }
        b.meter(&mut m);
        assert_eq!(m[84], 1.0);
        let fresh = Bloom::with_patch(&b.patches().patches[3]);
        assert_eq!(fresh.patch_gains()[0], b.patch_gains()[3], "measured on the audio side = measured at once");
    }

    #[test]
    fn the_tonal_state_round_trips() {
        let mut a = tonal(Kind::Pad);
        set_plain(&mut a, p::SEED, 31_337.0);
        let _ = render_notes(&mut a, &[], 256);
        let bytes = a.save_state();
        let parsed = parse_patches(Some(&bytes)).expect("patches stored");
        assert_eq!(parsed, *a.patches());
        let mut b = Bloom::new();
        b.load_state(&bytes);
        assert_eq!(b.kind(), Kind::Pad);
        assert_eq!(b.patches(), a.patches());
        assert_eq!(b.patch_gains(), a.patch_gains());
        // An edited tonal scene plays when its seed and kind match.
        let mut edited = a.patches().clone();
        edited.patches[0].set_plain(crate::instrument::wavetable::p::CUTOFF, 700.0);
        let params: std::collections::BTreeMap<u32, f32> = [(p::SEED, a.get_param(p::SEED)), (p::KIND, a.get_param(p::KIND))].into();
        let blob = state_json_full(&params, a.scene(), &Default::default(), None, Some(&edited));
        let mut c = Bloom::new();
        c.load_state(&blob);
        assert_eq!(c.patches().patches[0], edited.patches[0]);
        // Drums store no patches.
        assert!(parse_patches(Some(&Bloom::new().save_state())).is_none());
    }

    /// A Pad with corner B (another seed's scene) and corner C (a Lead
    /// scene, ignored by a Pad) stored.
    fn morphing_pad() -> (Bloom, PatchScene) {
        let mut a = tonal(Kind::Pad);
        let _ = render_notes(&mut a, &[], 256);
        let b = tonal::generate(77, Kind::Pad);
        let blob = set_patch_corner(Some(&a.save_state()), 1, Some(&b));
        let blob = set_patch_corner(Some(&blob), 2, Some(&tonal::generate(77, Kind::Lead)));
        let mut d = Bloom::new();
        d.load_state(&blob);
        (d, b)
    }

    #[test]
    fn tonal_morph_corners_blend_the_selected_patch() {
        let (mut d, b) = morphing_pad();
        assert_eq!(d.kind(), Kind::Pad);
        let own = d.patches().patches[0];
        assert_eq!(d.playing_patch().0, own.folded(), "X/Y at A plays the scene");
        let note = [(0, 60, 0.8, true)];
        let (at_a, _) = render_notes(&mut d, &note, 9_600);
        set_plain(&mut d, p::MORPH_X, 1.0);
        let (at_b, _) = render_notes(&mut d, &note, 9_600);
        assert_eq!(d.morph_position(), (1.0, 0.0), "a render (prepare + reset) lands on the target");
        assert_eq!(d.playing_patch().0, b.patches[0].folded(), "X at 1 plays corner B");
        let gain_b = tonal::measure_gain(&b.patches[0]);
        assert!((d.playing_patch().1 - gain_b).abs() < 1e-6, "at its own level");
        assert!(at_a.iter().zip(&at_b).map(|(x, y)| (x - y).abs()).sum::<f32>() > 1.0);
        // The Lead corner C stands in as the Pad scene.
        set_plain(&mut d, p::MORPH_X, 0.0);
        set_plain(&mut d, p::MORPH_Y, 1.0);
        let _ = render_notes(&mut d, &[], 256);
        assert_eq!(d.playing_patch().0, own.folded());
        // Halfway, between both, and its level between theirs (in dB).
        set_plain(&mut d, p::MORPH_Y, 0.0);
        set_plain(&mut d, p::MORPH_X, 0.5);
        let _ = render_notes(&mut d, &[], 256);
        let (mid, g) = d.playing_patch();
        assert!(mid.distance(&own) > 1e-3 && mid.distance(&b.patches[0]) > 1e-3);
        let (ga, gb) = (d.patch_gains()[0], gain_b);
        assert!(g >= ga.min(gb) - 1e-6 && g <= ga.max(gb) + 1e-6);
        // The corners round-trip through the device's state; editor rewrites keep them.
        let saved = d.save_state();
        let corners = parse_patch_corners(Some(&saved));
        assert!(corners[0].is_none() && corners[3].is_none());
        assert_eq!(corners[1].as_deref(), Some(&b.patches));
        let params: std::collections::BTreeMap<u32, f32> = [(p::SEED, d.get_param(p::SEED)), (p::KIND, d.get_param(p::KIND))].into();
        let rebuilt = state_json_full(&params, d.scene(), &Default::default(), None, Some(d.patches()));
        assert!(parse_patch_corners(Some(&rebuilt)).iter().all(Option::is_none));
        let kept = keep_patch_corners(Some(&saved), rebuilt);
        assert_eq!(parse_patch_corners(Some(&kept))[1].as_deref(), Some(&b.patches));
        assert_eq!(parse_patches(Some(&kept)).as_ref(), Some(d.patches()));
        let cleared = set_patch_corner(Some(&kept), 1, None);
        let cleared = set_patch_corner(Some(&cleared), 2, None);
        assert!(!String::from_utf8_lossy(&cleared).contains("patch_corners"));
        // Drums keep their own corners and ignore the tonal ones.
        assert!(parse_state(Some(&kept)).1.iter().all(Option::is_none));
    }

    #[test]
    fn tonal_morph_automation_glides_without_steps() {
        let (mut d, _) = morphing_pad();
        d.prepare(SR, 128);
        let infos = params();
        let x_norm = |v: f32| infos.iter().find(|i| i.id == p::MORPH_X).unwrap().to_normalized(v);
        let tr = Transport::default();
        let mut buf = AudioBuffer::new(2, 128);
        let mut out = Vec::new();
        let mut run = |d: &mut Bloom, events: &[Event], out: &mut Vec<f32>| {
            buf.set_frames(128);
            buf.clear();
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events, transport: &tr };
            d.process(&mut ctx);
            out.extend_from_slice(buf.channel(0));
        };
        run(&mut d, &[Event { offset: 0, kind: EventKind::NoteOn { channel: 0, key: 60, velocity: 0.8, note_id: 1 } }], &mut out);
        for _ in 0..300 {
            run(&mut d, &[], &mut out);
        }
        // Jump X to 1 in one event: the morph glides there over ~30 ms.
        run(&mut d, &[Event { offset: 0, kind: EventKind::Param { id: p::MORPH_X, value: x_norm(1.0) } }], &mut out);
        let first = d.morph_position().0;
        assert!(first > 0.0 && first < 0.2, "one block moves a little: {first}");
        let mut last = first;
        for _ in 0..140 {
            run(&mut d, &[], &mut out);
            let x = d.morph_position().0;
            assert!(x >= last && x - last < 0.2, "monotone, small steps");
            last = x;
        }
        assert_eq!(d.morph_position().0, 1.0, "arrives");
        assert!(out.iter().all(|v| v.is_finite()));
        // No click: the sweep's largest sample step stays near the static sound's.
        let step = |x: &[f32]| x.windows(2).fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
        let before = step(&out[20_000..300 * 128]);
        let during = step(&out[300 * 128..out.len()]);
        assert!(during < before * 2.5 + 1e-3, "a morph sweep steps {during} vs {before} at rest");
    }

    #[test]
    fn tonal_timbres_and_presets() {
        let bass = tonal::generate(5, Kind::Bass).patches[0];
        let lead = tonal::generate(5, Kind::Lead).patches[0];
        let tb = patch_timbre(&bass, &Knobs::default(), 0.0);
        let tl = patch_timbre(&lead, &Knobs::default(), 0.0);
        assert!(tb.centroid < tl.centroid, "{} vs {}", tb.centroid, tl.centroid);
        let up = patch_timbre(&bass, &Knobs::default(), 12.0);
        assert!(up.centroid > tb.centroid);
        let preset = patch_preset(&lead, &Knobs::default(), "Test Lead");
        let mut synth = crate::instrument::wavetable::WavetableSynth::new();
        assert!(preset.apply(&mut synth) > 300);
        let (l, _) = render_notes(&mut synth, &[(0, 67, 0.8, true)], 9_600);
        assert!(rms(&l) > 1e-3);
    }
}
