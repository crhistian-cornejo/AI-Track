//! The project data model.
//!
//! One coherent model unifies the paradigms in `docs/research/03`:
//! * **Sources** hold content (notes or audio) and can be shared by clips
//!   (linked clips) or owned by one clip (unique clips).
//! * **Clips** are instances of a source with their own offset, length, loop
//!   and gain. The same clip id can sit in a Session slot (Ableton) and be
//!   placed in the Arrangement.
//! * **Tracks** are typed: Instrument, Audio, Group (sums audio), Folder
//!   (organises only), Return, Vca and Main (Logic/Pro Tools mixing).
//! * **Devices** form a chain per track; a [`Rack`] holds parallel chains
//!   plus 16 macros with variations (Ableton racks, Logic Smart Controls).
//! * The step sequencer (FL Studio) is a view over a drum rack's notes.

use crate::ids::*;
use crate::scale::Scale;
use crate::time::{Tick, TempoMap, PPQ};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const PROJECT_FORMAT_VERSION: u32 = 1;
pub const MACRO_COUNT: usize = 16;

/// sRGB colour, `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Color(pub u32);

impl Color {
    /// The track/clip palette (balanced hues that read on dark and light themes).
    pub const PALETTE: [Color; 16] = [
        Color(0xFF6B6B), Color(0xFF9F43), Color(0xFECA57), Color(0xC8E05B),
        Color(0x5BD98A), Color(0x2ED3B7), Color(0x48C6EF), Color(0x5B8DEF),
        Color(0x7C6CF2), Color(0xA77BF3), Color(0xD77BE8), Color(0xF26CA7),
        Color(0xB8A88A), Color(0x8FA3B8), Color(0x9AA0A6), Color(0xE0E0E0),
    ];

    pub fn palette(i: usize) -> Color {
        Self::PALETTE[i % Self::PALETTE.len()]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    pub tempo: TempoMap,
    /// Global scale (piano roll highlighting, MIDI tools, AI, sample matching).
    pub scale: Option<Scale>,
    pub sample_rate_hint: u32,
    pub tracks: Vec<Track>,
    pub scenes: Vec<Scene>,
    pub clips: BTreeMap<ClipId, Clip>,
    pub sources: BTreeMap<SourceId, Source>,
    pub loop_region: Option<(Tick, Tick)>,
    pub ids: IdAllocator,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    /// An empty project with only the Main (master) track.
    pub fn new(name: impl Into<String>) -> Self {
        let mut p = Self {
            format_version: PROJECT_FORMAT_VERSION,
            name: name.into(),
            tempo: TempoMap::constant(120.0),
            scale: None,
            sample_rate_hint: 48_000,
            tracks: Vec::new(),
            scenes: Vec::new(),
            clips: BTreeMap::new(),
            sources: BTreeMap::new(),
            loop_region: Some((0, 16 * PPQ)),
            ids: IdAllocator::default(),
        };
        let main = Track::new(TrackId(p.ids.next_raw()), TrackKind::Main, "Main", Color(0xE0E0E0));
        p.tracks.push(main);
        for i in 0..8 {
            let id = SceneId(p.ids.next_raw());
            p.scenes.push(Scene { id, name: format!("{}", i + 1), tempo: None });
        }
        p
    }

    pub fn alloc_track_id(&mut self) -> TrackId { TrackId(self.ids.next_raw()) }
    pub fn alloc_clip_id(&mut self) -> ClipId { ClipId(self.ids.next_raw()) }
    pub fn alloc_source_id(&mut self) -> SourceId { SourceId(self.ids.next_raw()) }
    pub fn alloc_note_id(&mut self) -> NoteId { NoteId(self.ids.next_raw()) }
    pub fn alloc_device_id(&mut self) -> DeviceId { DeviceId(self.ids.next_raw()) }
    pub fn alloc_scene_id(&mut self) -> SceneId { SceneId(self.ids.next_raw()) }
    pub fn alloc_placement_id(&mut self) -> PlacementId { PlacementId(self.ids.next_raw()) }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn main_track(&self) -> &Track {
        self.tracks.iter().find(|t| t.kind == TrackKind::Main).expect("project always has a Main track")
    }

    /// Tracks shown in the arrangement/session (everything but Main).
    pub fn content_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind != TrackKind::Main)
    }

    /// Find a device anywhere (track chains and nested racks).
    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.tracks.iter().find_map(|t| find_device(&t.devices, id))
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
        self.tracks.iter_mut().find_map(|t| find_device_mut(&mut t.devices, id))
    }

    /// Track that owns a device (directly or inside a rack).
    pub fn device_track(&self, id: DeviceId) -> Option<TrackId> {
        self.tracks.iter().find(|t| find_device(&t.devices, id).is_some()).map(|t| t.id)
    }

    /// End of the last arrangement clip, at least 8 bars.
    pub fn arrangement_end(&self) -> Tick {
        self.tracks
            .iter()
            .flat_map(|t| t.arrangement.iter())
            .filter_map(|p| self.clips.get(&p.clip).map(|c| p.start + c.length))
            .max()
            .unwrap_or(0)
            .max(32 * PPQ)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let mut p: Project = serde_json::from_str(s)?;
        let max = p.max_id();
        p.ids.observe(max);
        Ok(p)
    }

    fn max_id(&self) -> u64 {
        let mut m = 0;
        for t in &self.tracks {
            m = m.max(t.id.0);
            visit_devices(&t.devices, &mut |d| m = m.max(d.id.0));
            for p in &t.arrangement { m = m.max(p.id.0); }
        }
        for s in &self.scenes { m = m.max(s.id.0); }
        for (id, c) in &self.clips { m = m.max(id.0).max(c.source.0); }
        for (id, s) in &self.sources {
            m = m.max(id.0);
            if let Source::Notes(n) = s { for note in &n.notes { m = m.max(note.id.0); } }
        }
        m
    }
}

fn find_device(chain: &[Device], id: DeviceId) -> Option<&Device> {
    for d in chain {
        if d.id == id { return Some(d); }
        if let DeviceKind::Rack(rack) = &d.kind {
            for c in &rack.chains {
                if let Some(found) = find_device(&c.devices, id) { return Some(found); }
            }
        }
    }
    None
}

fn find_device_mut(chain: &mut [Device], id: DeviceId) -> Option<&mut Device> {
    for d in chain {
        if d.id == id { return Some(d); }
        if let DeviceKind::Rack(rack) = &mut d.kind {
            for c in &mut rack.chains {
                if let Some(found) = find_device_mut(&mut c.devices, id) { return Some(found); }
            }
        }
    }
    None
}

pub fn visit_devices(chain: &[Device], f: &mut impl FnMut(&Device)) {
    for d in chain {
        f(d);
        if let DeviceKind::Rack(rack) = &d.kind {
            for c in &rack.chains { visit_devices(&c.devices, f); }
        }
    }
}

// ───────────────────────────── Tracks ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// MIDI clips → instrument device.
    Instrument,
    /// Audio clips.
    Audio,
    /// Sums its children's audio (Ableton group / Logic summing stack).
    Group,
    /// Organises children only; no audio path (Logic folder stack).
    Folder,
    /// Return/aux bus fed by sends.
    Return,
    /// Controls the level of its member tracks without carrying audio.
    Vca,
    /// Master output.
    Main,
}

impl TrackKind {
    pub fn has_clips(self) -> bool {
        matches!(self, Self::Instrument | Self::Audio)
    }
    pub fn carries_audio(self) -> bool {
        !matches!(self, Self::Folder | Self::Vca)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "id")]
pub enum Output {
    Main,
    Track(TrackId),
    /// Hardware output pair index.
    Hardware(u16),
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub target: TrackId,
    pub level_db: f32,
    pub pre_fader: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub color: Color,
    pub volume_db: f32,
    /// -1 (left) … 1 (right).
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
    pub monitor: Monitor,
    pub output: Output,
    pub sends: Vec<Send>,
    /// Group or Folder this track lives in.
    pub parent: Option<TrackId>,
    /// VCA that scales this track's fader.
    pub vca: Option<TrackId>,
    pub collapsed: bool,
    pub height: f32,
    pub devices: Vec<Device>,
    /// One slot per scene (Session View).
    pub session_slots: BTreeMap<SceneId, ClipId>,
    /// Clips placed on the timeline (Arrangement View).
    pub arrangement: Vec<Placement>,
    pub automation: Vec<AutomationLane>,
    /// Plugin delay compensation the engine reports back (samples, read-only).
    #[serde(default)]
    pub reported_latency: u32,
}

impl Track {
    pub fn new(id: TrackId, kind: TrackKind, name: impl Into<String>, color: Color) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            color,
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            arm: false,
            monitor: Monitor::Auto,
            output: if kind == TrackKind::Main { Output::Hardware(0) } else { Output::Main },
            sends: Vec::new(),
            parent: None,
            vca: None,
            collapsed: false,
            height: 56.0,
            devices: Vec::new(),
            session_slots: BTreeMap::new(),
            arrangement: Vec::new(),
            automation: Vec::new(),
            reported_latency: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Monitor {
    In,
    Auto,
    Off,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
    pub tempo: Option<f64>,
}

/// A clip placed on a track's timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub id: PlacementId,
    pub clip: ClipId,
    pub start: Tick,
    /// Take lane for comping (0 = the playing lane).
    #[serde(default)]
    pub lane: u16,
}

// ───────────────────────────── Clips & sources ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub color: Color,
    pub source: SourceId,
    /// Where in the source the clip starts.
    pub offset: Tick,
    pub length: Tick,
    pub looping: Option<LoopRegion>,
    /// Clip gain (Pro Tools style), applied before the device chain.
    pub gain_db: f32,
    /// Clip envelopes (Ableton): absolute or relative modulation.
    ///
    /// The `AutomationTarget::Volume` lane is the clip's volume envelope: its
    /// point ticks are **clip time** (ticks from the clip's start on the
    /// timeline, so the envelope does not repeat with the loop) and its value
    /// is a **linear gain multiplier 0..=1 where 1.0 is unity**.
    pub envelopes: Vec<AutomationLane>,
    pub launch: LaunchSettings,
    pub muted: bool,
    /// Per-clip scale override.
    pub scale: Option<Scale>,
    /// Fade at the clip's start, drawn on the waveform.
    #[serde(default)]
    pub fade_in: Fade,
    /// Fade at the clip's end.
    #[serde(default)]
    pub fade_out: Fade,
}

/// A fade at one end of a clip.
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Fade {
    /// Length in ticks; 0 = no fade.
    pub length: Tick,
    /// Curvature -1..=1 (0 = linear), as [`AutomationPoint::curve`].
    pub curve: f32,
}

impl Clip {
    /// A clip over `source` with everything else at its default: offset 0, no
    /// loop, 0 dB, no envelopes, default launch, not muted, no scale, no fades.
    pub fn new(name: impl Into<String>, color: Color, source: SourceId, length: Tick) -> Self {
        Self {
            name: name.into(),
            color,
            source,
            offset: 0,
            length,
            looping: None,
            gain_db: 0.0,
            envelopes: Vec::new(),
            launch: LaunchSettings::default(),
            muted: false,
            scale: None,
            fade_in: Fade::default(),
            fade_out: Fade::default(),
        }
    }

    /// The enabled volume envelope (see [`Clip::envelopes`] for its units).
    pub fn volume_envelope(&self) -> Option<&AutomationLane> {
        self.envelopes.iter().find(|l| l.enabled && l.target == AutomationTarget::Volume)
    }

    /// Linear gain at clip time `t` (0..length): fade in × fade out × volume
    /// envelope. `gain_db` is not included. No allocation, so the engine can
    /// call it per block and the views per pixel.
    pub fn gain_at(&self, t: Tick) -> f32 {
        let mut gain = 1.0;
        if self.fade_in.length > 0 && t < self.fade_in.length {
            gain *= curve_shape(t.max(0) as f32 / self.fade_in.length as f32, self.fade_in.curve);
        }
        if self.fade_out.length > 0 && t > self.length - self.fade_out.length {
            gain *= curve_shape((self.length - t).max(0) as f32 / self.fade_out.length as f32, self.fade_out.curve);
        }
        if let Some(v) = self.volume_envelope().and_then(|l| l.value_at(t)) {
            gain *= v.clamp(0.0, 1.0);
        }
        gain
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRegion {
    pub start: Tick,
    pub end: Tick,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchSettings {
    pub quantize: Quantize,
    pub mode: LaunchMode,
    pub follow: Option<FollowAction>,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self { quantize: Quantize::Bar, mode: LaunchMode::Trigger, follow: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantize {
    None,
    Sixteenth,
    Eighth,
    Beat,
    Bar,
    TwoBars,
    FourBars,
}

impl Quantize {
    pub fn ticks(self, bar: Tick) -> Tick {
        match self {
            Self::None => 1,
            Self::Sixteenth => PPQ / 4,
            Self::Eighth => PPQ / 2,
            Self::Beat => PPQ,
            Self::Bar => bar,
            Self::TwoBars => bar * 2,
            Self::FourBars => bar * 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    Trigger,
    Gate,
    Toggle,
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowAction {
    Next { after_bars: u32 },
    Previous { after_bars: u32 },
    First { after_bars: u32 },
    Random { after_bars: u32 },
    Stop { after_bars: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Source {
    Notes(NoteSource),
    Audio(AudioSource),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NoteSource {
    /// Kept sorted by `start`, then `key`.
    pub notes: Vec<Note>,
}

impl NoteSource {
    pub fn sort(&mut self) {
        self.notes.sort_by(|a, b| a.start.cmp(&b.start).then(a.key.cmp(&b.key)));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    /// MIDI key 0..=127.
    pub key: u8,
    pub start: Tick,
    pub length: Tick,
    /// 0..=1.
    pub velocity: f32,
    /// Release velocity 0..=1.
    #[serde(default = "half")]
    pub release: f32,
    /// Chance to play, 0..=1 (Ableton/FL probability).
    #[serde(default = "one")]
    pub probability: f32,
    /// Per-note pan -1..1 (FL Studio note property).
    #[serde(default)]
    pub pan: f32,
    /// Fine pitch in cents (MPE/FL fine pitch).
    #[serde(default)]
    pub fine_pitch: f32,
    /// FL-style slide: glide from the previous note.
    #[serde(default)]
    pub slide: bool,
    #[serde(default)]
    pub muted: bool,
}

fn half() -> f32 { 0.5 }
fn one() -> f32 { 1.0 }

impl Note {
    pub fn new(id: NoteId, key: u8, start: Tick, length: Tick, velocity: f32) -> Self {
        Self { id, key, start, length, velocity, release: 0.5, probability: 1.0, pan: 0.0, fine_pitch: 0.0, slide: false, muted: false }
    }
    pub fn end(&self) -> Tick { self.start + self.length }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioSource {
    pub path: PathBuf,
    /// blake3/sha256 of the file contents, so moved files can be relinked.
    pub hash: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub warp: Warp,
    /// Detected or tagged musical key of the material.
    pub key: Option<Scale>,
}

impl AudioSource {
    /// Natural length of the file on the source-tick timeline the engine
    /// plays it on: at `warp.original_bpm` when warped, else at `bpm`.
    /// 0 when the file's length is unknown.
    pub fn length_ticks(&self, bpm: f64) -> Tick {
        if self.frames == 0 || self.sample_rate == 0 {
            return 0;
        }
        let bpm = match self.warp.original_bpm {
            Some(original) if self.warp.enabled => original,
            _ => bpm,
        };
        (self.frames as f64 / self.sample_rate as f64 * bpm / 60.0 * PPQ as f64).round() as Tick
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Warp {
    pub enabled: bool,
    pub mode: WarpMode,
    /// Tempo of the material; warping maps it to the project tempo.
    pub original_bpm: Option<f64>,
    /// Transpose in semitones (with formant-preserving modes later).
    pub transpose: f32,
    /// (sample frame, tick) anchors.
    pub markers: Vec<(u64, Tick)>,
}

impl Default for Warp {
    fn default() -> Self {
        Self { enabled: false, mode: WarpMode::Beats, original_bpm: None, transpose: 0.0, markers: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarpMode {
    /// Slice at transients (drums).
    Beats,
    /// Monophonic pitch-preserving stretch.
    Tones,
    /// Polyphonic stretch (full mixes).
    Complex,
    /// Speed changes pitch (vinyl).
    Repitch,
}

// ───────────────────────────── Devices & racks ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub enabled: bool,
    /// Normalised (0..=1) parameter values by parameter id.
    #[serde(deserialize_with = "param_map::deserialize")]
    pub params: BTreeMap<u32, f32>,
    /// Opaque plugin state (VST3/CLAP chunk or stock preset), base64 in JSON.
    #[serde(default, with = "opt_bytes")]
    pub state: Option<Vec<u8>>,
    /// Name of the preset loaded last, for display.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
    /// The track this device listens to on its sidechain input (ducking a
    /// bass from the kick, keying a gate). `None` leaves it unconnected.
    #[serde(default)]
    pub sidechain: Option<TrackId>,
}

impl Device {
    pub fn stock(id: DeviceId, plugin_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            kind: DeviceKind::Stock { plugin_id: plugin_id.into() },
            enabled: true,
            params: BTreeMap::new(),
            state: None,
            preset: None,
            collapsed: false,
            sidechain: None,
        }
    }

    pub fn is_instrument(&self) -> bool {
        match &self.kind {
            DeviceKind::Stock { plugin_id } => plugin_id.starts_with("instrument."),
            DeviceKind::Rack(r) => matches!(r.kind, RackKind::Instrument | RackKind::Drum),
            DeviceKind::Plugin { is_instrument, .. } => *is_instrument,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DeviceKind {
    /// Built-in AI-Track device; `plugin_id` like `"fx.eq"` or `"instrument.wavetable"`.
    Stock { plugin_id: String },
    /// Third-party VST3 or CLAP plugin.
    Plugin { format: PluginFormat, uid: String, path: PathBuf, vendor: String, is_instrument: bool },
    Rack(Rack),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
    Vst3,
    Clap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rack {
    pub kind: RackKind,
    pub chains: Vec<Chain>,
    pub macros: Vec<Macro>,
    /// Snapshots of all macro values (macro variations).
    pub variations: Vec<Variation>,
    /// Chain selector value 0..=127 for zone-based chain switching.
    pub chain_selector: u8,
}

impl Rack {
    pub fn new(kind: RackKind) -> Self {
        Self {
            kind,
            chains: Vec::new(),
            macros: (0..MACRO_COUNT)
                .map(|i| Macro { name: format!("Macro {}", i + 1), value: 0.0, mappings: Vec::new() })
                .collect(),
            variations: Vec::new(),
            chain_selector: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RackKind {
    Instrument,
    Audio,
    /// One chain per pad; each chain has a `note` (Ableton Drum Rack / FL channel rack).
    Drum,
    Midi,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    pub name: String,
    pub color: Color,
    pub devices: Vec<Device>,
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// Drum rack: the MIDI note that triggers this pad.
    pub note: Option<u8>,
    pub key_range: (u8, u8),
    pub velocity_range: (u8, u8),
    pub selector_range: (u8, u8),
}

impl Chain {
    pub fn new(name: impl Into<String>, color: Color) -> Self {
        Self {
            name: name.into(),
            color,
            devices: Vec::new(),
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            note: None,
            key_range: (0, 127),
            velocity_range: (1, 127),
            selector_range: (0, 127),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    /// 0..=1.
    pub value: f32,
    pub mappings: Vec<MacroMapping>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MacroMapping {
    pub device: DeviceId,
    pub param: u32,
    /// Normalised range the macro sweeps (min may exceed max to invert).
    pub min: f32,
    pub max: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Variation {
    pub name: String,
    pub values: Vec<f32>,
}

// ───────────────────────────── Automation ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    pub points: Vec<AutomationPoint>,
    pub enabled: bool,
    /// Clip envelopes only: modulate around the current value instead of overriding.
    #[serde(default)]
    pub relative: bool,
}

impl AutomationLane {
    pub fn new(target: AutomationTarget) -> Self {
        Self { target, points: Vec::new(), enabled: true, relative: false }
    }

    /// Value at `tick` (normalised 0..=1) or `None` without points.
    pub fn value_at(&self, tick: Tick) -> Option<f32> {
        let pts = &self.points;
        let first = pts.first()?;
        if tick <= first.tick {
            return Some(first.value);
        }
        let i = pts.partition_point(|p| p.tick <= tick);
        let a = &pts[i - 1];
        let Some(b) = pts.get(i) else { return Some(a.value) };
        let t = (tick - a.tick) as f32 / (b.tick - a.tick).max(1) as f32;
        Some(a.value + (b.value - a.value) * curve_shape(t, a.curve))
    }

    pub fn insert(&mut self, point: AutomationPoint) {
        let i = self.points.partition_point(|p| p.tick < point.tick);
        if self.points.get(i).is_some_and(|p| p.tick == point.tick) {
            self.points[i] = point;
        } else {
            self.points.insert(i, point);
        }
    }
}

/// Progress 0..=1 bent by a curvature -1..1 (0 = linear): the shape between
/// an automation point and the next, and of clip fades.
pub fn curve_shape(t: f32, curve: f32) -> f32 {
    if curve.abs() < 1e-4 {
        t
    } else {
        let k = curve * 6.0;
        ((k * t).exp() - 1.0) / (k.exp() - 1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub tick: Tick,
    pub value: f32,
    #[serde(default)]
    pub curve: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AutomationTarget {
    Volume,
    Pan,
    Mute,
    Send { index: u16 },
    DeviceParam { device: DeviceId, param: u32 },
    DeviceEnabled { device: DeviceId },
    Macro { device: DeviceId, index: u8 },
    Tempo,
}

/// Parameter maps keyed by id. JSON object keys are strings; serde turns them
/// back into `u32` when it reads the map directly, but not when the map is
/// buffered inside an internally tagged enum (the devices of a rack chain go
/// through `DeviceKind`), so accept both forms.
mod param_map {
    use serde::{Deserialize, Deserializer};
    use std::collections::{BTreeMap, HashMap};

    #[derive(Deserialize, PartialEq, Eq, Hash)]
    #[serde(untagged)]
    enum Key {
        Num(u32),
        Text(String),
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<u32, f32>, D::Error> {
        HashMap::<Key, f32>::deserialize(d)?
            .into_iter()
            .map(|(k, v)| match k {
                Key::Num(n) => Ok((n, v)),
                Key::Text(s) => s.parse().map(|n| (n, v)).map_err(|_| serde::de::Error::custom(format!("bad parameter id {s:?}"))),
            })
            .collect()
    }
}
/// `Option<Vec<u8>>` as base64 so project JSON stays readable.
mod opt_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(bytes) => s.serialize_some(&encode(bytes)),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        s.map(|s| decode(&s).ok_or_else(|| serde::de::Error::custom("invalid base64"))).transpose()
    }

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let mut buf = 0u32;
        let mut bits = 0;
        for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
            if c == b'=' {
                break;
            }
            let v = TABLE.iter().position(|&t| t == c)? as u32;
            buf = buf << 6 | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Some(out)
    }

    #[cfg(test)]
    #[test]
    fn round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"\x00\xff\x10\x20"] {
            assert_eq!(decode(&encode(data)).unwrap(), data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rack_chain_device_params_round_trip() {
        let mut p = Project::new("Test");
        let mut inner = Device::stock(p.alloc_device_id(), "instrument.drum", "Kick");
        inner.params.insert(3, 0.25);
        let mut rack = Rack::new(RackKind::Drum);
        let mut chain = Chain::new("Kick", Color(0));
        chain.devices.push(inner);
        rack.chains.push(chain);
        let mut d = Device::stock(p.alloc_device_id(), "rack.drum", "Drum Rack");
        d.kind = DeviceKind::Rack(rack);
        let back: Device = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn new_project_has_main_and_scenes() {
        let p = Project::new("Test");
        assert_eq!(p.main_track().kind, TrackKind::Main);
        assert_eq!(p.scenes.len(), 8);
    }

    #[test]
    fn json_round_trip_keeps_ids_monotonic() {
        let mut p = Project::new("Test");
        let mut d = Device::stock(p.alloc_device_id(), "fx.eq", "EQ");
        d.state = Some(vec![1, 2, 3]);
        let id = p.alloc_track_id();
        let mut t = Track::new(id, TrackKind::Audio, "Drums", Color::palette(0));
        t.devices.push(d);
        p.tracks.push(t);
        let json = p.to_json().unwrap();
        let mut back = Project::from_json(&json).unwrap();
        assert_eq!(back.tracks, p.tracks);
        assert!(back.alloc_track_id().0 > id.0);
    }

    #[test]
    fn automation_interpolates() {
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.insert(AutomationPoint { tick: 0, value: 0.0, curve: 0.0 });
        lane.insert(AutomationPoint { tick: 100, value: 1.0, curve: 0.0 });
        assert_eq!(lane.value_at(50), Some(0.5));
        assert_eq!(lane.value_at(200), Some(1.0));
    }

    #[test]
    fn clip_without_fades_still_loads() {
        // A clip as saved before fades existed.
        let old = r#"{"name":"c","color":0,"source":7,"offset":0,"length":960,"looping":null,"gain_db":0.0,
            "envelopes":[],"launch":{"quantize":"bar","mode":"trigger","follow":null},"muted":false,"scale":null}"#;
        let clip: Clip = serde_json::from_str(old).unwrap();
        assert_eq!(clip, Clip::new("c", Color(0), SourceId(7), 960));
        let mut faded = clip.clone();
        faded.fade_out = Fade { length: 120, curve: -0.5 };
        let back: Clip = serde_json::from_str(&serde_json::to_string(&faded).unwrap()).unwrap();
        assert_eq!(back, faded);
    }

    #[test]
    fn gain_follows_fades_and_volume_envelope() {
        let mut c = Clip::new("c", Color(0), SourceId(1), 1000);
        assert_eq!(c.gain_at(0), 1.0);
        c.fade_in = Fade { length: 200, curve: 0.0 };
        c.fade_out = Fade { length: 400, curve: 0.0 };
        assert_eq!(c.gain_at(0), 0.0);
        assert_eq!(c.gain_at(100), 0.5);
        assert_eq!(c.gain_at(200), 1.0);
        assert_eq!(c.gain_at(600), 1.0);
        assert_eq!(c.gain_at(800), 0.5);
        assert_eq!(c.gain_at(1000), 0.0);
        // A curved fade still starts at 0 and ends at 1, but bends in between.
        c.fade_in.curve = 1.0;
        assert_eq!(c.gain_at(0), 0.0);
        assert!(c.gain_at(100) < 0.2);
        // The envelope multiplies; a disabled one is ignored.
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
        c.envelopes.push(lane);
        assert_eq!(c.gain_at(500), 0.5);
        assert_eq!(c.gain_at(800), 0.25);
        c.envelopes[0].enabled = false;
        assert!(c.volume_envelope().is_none());
        assert_eq!(c.gain_at(500), 1.0);
    }

    #[test]
    fn audio_length_in_ticks() {
        let mut a = AudioSource { path: "a.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 96_000, warp: Warp::default(), key: None };
        // Two seconds at 120 BPM are four beats.
        assert_eq!(a.length_ticks(120.0), 4 * PPQ);
        a.warp.original_bpm = Some(60.0);
        assert_eq!(a.length_ticks(120.0), 4 * PPQ);
        a.warp.enabled = true;
        assert_eq!(a.length_ticks(120.0), 2 * PPQ);
        a.frames = 0;
        assert_eq!(a.length_ticks(120.0), 0);
    }
}
