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

use crate::format::LoadError;
use crate::ids::*;
use crate::groove::PoolGroove;
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
    /// The grooves the project uses (Live's Groove Pool), with their settings.
    #[serde(default)]
    pub grooves: Vec<PoolGroove>,
    /// Global groove amount, 0..=[`MAX_GROOVE_AMOUNT`]: scales the timing,
    /// random and velocity of every groove. 1 = as set.
    #[serde(default = "one")]
    pub groove_amount: f32,
    /// The groove each notes clip is played with (see [`Project::clip_groove`]).
    /// Kept beside the clips, not in them: a clip without an entry plays its
    /// notes as they are, and an entry for a clip that no longer exists is
    /// ignored (so undoing a clip's deletion brings its groove back).
    #[serde(default)]
    pub clip_grooves: BTreeMap<ClipId, GrooveId>,
    /// Named marks on the Arrangement ruler (Live's locators), kept in order
    /// of position. Projects saved before locators existed have none.
    #[serde(default)]
    pub locators: Vec<Locator>,
}

/// A named mark on the Arrangement timeline: a song section (intro, drop,
/// break) to jump to, loop or rehearse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Locator {
    pub id: LocatorId,
    pub tick: Tick,
    pub name: String,
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
            grooves: Vec::new(),
            groove_amount: 1.0,
            clip_grooves: BTreeMap::new(),
            locators: Vec::new(),
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

    pub fn groove(&self, id: GrooveId) -> Option<&PoolGroove> {
        self.grooves.iter().find(|g| g.id == id)
    }

    /// The pool groove clip `clip` is played with, if it has one that is
    /// still in the pool.
    pub fn clip_groove(&self, clip: ClipId) -> Option<&PoolGroove> {
        self.clip_grooves.get(&clip).and_then(|g| self.groove(*g))
    }

    pub fn alloc_locator_id(&mut self) -> LocatorId { LocatorId(self.ids.next_raw()) }

    pub fn locator(&self, id: LocatorId) -> Option<&Locator> {
        self.locators.iter().find(|l| l.id == id)
    }

    /// The first locator strictly after `tick`.
    pub fn next_locator(&self, tick: Tick) -> Option<&Locator> {
        self.locators.iter().find(|l| l.tick > tick)
    }

    /// The last locator strictly before `tick`.
    pub fn prev_locator(&self, tick: Tick) -> Option<&Locator> {
        self.locators.iter().rev().find(|l| l.tick < tick)
    }

    pub fn alloc_groove_id(&mut self) -> GrooveId {
        GrooveId(self.ids.next_raw())
    }

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

    /// Open a project file's text. `format_version` is read first: a file
    /// from a newer AI-Track is refused ([`LoadError::TooNew`]) instead of
    /// being read with fields silently dropped, and an older one goes through
    /// the migration chain ([`crate::format`]). The ids are made monotonic.
    pub fn open_json(s: &str) -> Result<Self, LoadError> {
        let mut p = crate::format::open(s)?;
        let max = p.max_id();
        p.ids.observe(max);
        Ok(p)
    }

    /// [`open_json`](Self::open_json) with the error as a `serde_json::Error`
    /// (same checks, same message), for callers that predate [`LoadError`].
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        Self::open_json(s).map_err(<serde_json::Error as serde::de::Error>::custom)
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

pub(crate) fn find_device(chain: &[Device], id: DeviceId) -> Option<&Device> {
    for d in chain {
        if d.id == id { return Some(d); }
        if let DeviceKind::Rack(rack) = &d.kind {
            for c in rack.chains.iter().chain(&rack.returns) {
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
            for c in rack.chains.iter_mut().chain(rack.returns.iter_mut()) {
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
            for c in rack.chains.iter().chain(&rack.returns) { visit_devices(&c.devices, f); }
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
    /// Where an Audio track records from (none until the user picks one).
    #[serde(default)]
    pub input: InputSource,
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
    /// Optional per-track key/scale override (used by the Piano Roll when the clip has no scale).
    #[serde(default)]
    pub scale: Option<Scale>,
    /// Whether this Instrument track receives notes from the computer MIDI keyboard.
    #[serde(default)]
    pub keyboard_input: bool,
    /// Take lanes (comping): rows under the track holding takes that stay
    /// silent. The placements of take lane `i` have `lane == i + 1`; the
    /// track's main lane (`lane == 0`) is the comp, the only one that plays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub take_lanes: Vec<TakeLane>,
    /// Which take lane each stretch of the main lane was promoted from (the
    /// parts of the takes the arrangement highlights). Sorted, never overlapping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comp: Vec<CompSpan>,
    /// The track's rendered audio while it is frozen: the engine plays it
    /// instead of the clips and the device chain (see `Command::FreezeTrack`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<Freeze>,
}

/// A take lane of a track (see [`Track::take_lanes`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeLane {
    pub name: String,
}

/// A stretch of the main lane that was promoted from a take lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompSpan {
    /// The placement lane of the take: 1 + the index in [`Track::take_lanes`].
    pub lane: u16,
    pub start: Tick,
    pub end: Tick,
}

/// What a frozen track plays: its clips and whole device chain rendered
/// offline from tick 0, ring-out tail included, before the fader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Freeze {
    /// The rendered file, a stereo (or mono) WAV that starts at tick 0. Its
    /// warp follows the song tempo when the tempo does not change.
    pub file: AudioSource,
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
            input: InputSource::None,
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
            scale: None,
            keyboard_input: false,
            take_lanes: Vec::new(),
            comp: Vec::new(),
            freeze: None,
        }
    }

    /// True while the track plays its frozen render instead of running live.
    pub fn is_frozen(&self) -> bool {
        self.freeze.is_some()
    }

    /// Whether the track kind can be frozen: those that hold clips and a
    /// device chain.
    pub fn can_freeze(&self) -> bool {
        self.kind.has_clips()
    }
}

/// The audio input a track records from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InputSource {
    /// Not recording from anything.
    #[default]
    None,
    /// Hardware input channel `first` (0-based), or the stereo pair
    /// `first`, `first + 1`.
    Hardware { first: u16, stereo: bool },
}

impl InputSource {
    /// `(first channel, channel count)` of a hardware input.
    pub fn channel_span(self) -> Option<(u16, u16)> {
        match self {
            Self::None => None,
            Self::Hardware { first, stereo } => Some((first, if stereo { 2 } else { 1 })),
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
    /// 0 = the main lane, the one that plays; `i + 1` = take lane `i` of
    /// the track (silent, see [`Track::take_lanes`]).
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
    /// is a **linear gain multiplier 0..=1 where 1.0 is unity**. Audio clips
    /// have two more, same time base and curves ([`ClipEnv`]): an
    /// `AutomationTarget::Pan` lane whose value is a **pan -1..=1** (0 centre)
    /// and an `AutomationTarget::Transpose` lane whose value is **semitones
    /// -24..=24** (0 = no change). A missing lane is a flat envelope at the
    /// neutral value, so files saved before they existed load unchanged.
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
        self.envelope(ClipEnv::Volume)
    }

    /// The enabled pan envelope (-1..=1).
    pub fn pan_envelope(&self) -> Option<&AutomationLane> {
        self.envelope(ClipEnv::Pan)
    }

    /// The enabled transpose envelope (semitones).
    pub fn transpose_envelope(&self) -> Option<&AutomationLane> {
        self.envelope(ClipEnv::Pitch)
    }

    /// The enabled envelope of `kind`.
    pub fn envelope(&self, kind: ClipEnv) -> Option<&AutomationLane> {
        let target = kind.target();
        self.envelopes.iter().find(|l| l.enabled && l.target == target)
    }

    /// Pan -1..=1 at clip time `t`; 0 without a pan envelope.
    pub fn pan_at(&self, t: Tick) -> f32 {
        self.pan_envelope().and_then(|l| l.value_at(t)).map_or(0.0, |v| ClipEnv::Pan.clamp(v))
    }

    /// Transpose in semitones at clip time `t`; 0 without a transpose envelope.
    pub fn transpose_at(&self, t: Tick) -> f32 {
        self.transpose_envelope().and_then(|l| l.value_at(t)).map_or(0.0, |v| ClipEnv::Pitch.clamp(v))
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

/// Which of an audio clip's envelopes (see [`Clip::envelopes`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ClipEnv {
    #[default]
    Volume,
    Pan,
    Pitch,
}

impl ClipEnv {
    pub const ALL: [ClipEnv; 3] = [Self::Volume, Self::Pan, Self::Pitch];

    /// The lane target that stores this envelope in [`Clip::envelopes`].
    pub fn target(self) -> AutomationTarget {
        match self {
            Self::Volume => AutomationTarget::Volume,
            Self::Pan => AutomationTarget::Pan,
            Self::Pitch => AutomationTarget::Transpose,
        }
    }

    pub fn of(target: AutomationTarget) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.target() == target)
    }

    /// The least and the greatest value a point can hold.
    pub fn range(self) -> (f32, f32) {
        match self {
            Self::Volume => (0.0, 1.0),
            Self::Pan => (-1.0, 1.0),
            Self::Pitch => (-24.0, 24.0),
        }
    }

    /// The value that changes nothing: unity gain, centre, no transpose.
    pub fn neutral(self) -> f32 {
        match self {
            Self::Volume => 1.0,
            Self::Pan | Self::Pitch => 0.0,
        }
    }

    /// `v` held inside [`range`](Self::range); a NaN or infinity is neutral.
    pub fn clamp(self, v: f32) -> f32 {
        let (lo, hi) = self.range();
        if v.is_finite() { v.clamp(lo, hi) } else { self.neutral() }
    }

    /// Position of `v` in the range, 0..=1 (0 = least).
    pub fn unit(self, v: f32) -> f32 {
        let (lo, hi) = self.range();
        ((self.clamp(v) - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    /// Inverse of [`unit`](Self::unit).
    pub fn from_unit(self, u: f32) -> f32 {
        let (lo, hi) = self.range();
        lo + u.clamp(0.0, 1.0) * (hi - lo)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::Pan => "Pan",
            Self::Pitch => "Transpose",
        }
    }
}

/// Channel gains of a clip pan -1..=1: a balance law. The side the sound
/// moves away from is turned down on an equal-power curve and the other
/// stays at unity, so the centre changes nothing, for stereo and mono files.
pub fn clip_pan_gains(pan: f32) -> (f32, f32) {
    let pan = if pan.is_finite() { pan.clamp(-1.0, 1.0) } else { 0.0 };
    if pan == 0.0 {
        return (1.0, 1.0);
    }
    let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    ((theta.cos() * std::f32::consts::SQRT_2).clamp(0.0, 1.0), (theta.sin() * std::f32::consts::SQRT_2).clamp(0.0, 1.0))
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
    /// plays it on: by its warp markers or at `warp.original_bpm` when
    /// warped, else at `bpm`. 0 when the file's length is unknown.
    pub fn length_ticks(&self, bpm: f64) -> Tick {
        if self.frames == 0 || self.sample_rate == 0 {
            return 0;
        }
        if self.warp.enabled && !self.warp.anchors().is_empty() {
            return self.warp.tick_at(self.frames as f64, self.sample_rate as f64).map_or(0, |t| t.round().max(0.0) as Tick);
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
    /// Transpose in semitones, -48..=48 (Live's clip Transpose). Re-Pitch:
    /// the sample plays faster or slower as well.
    pub transpose: f32,
    /// Fine tuning in cents, -50..=50 (Live's clip Detune).
    #[serde(default)]
    pub detune: f32,
    /// Play the sample backwards.
    #[serde(default)]
    pub reverse: bool,
    /// (sample frame, tick) anchors.
    pub markers: Vec<(u64, Tick)>,
}

impl Warp {
    /// Transpose plus detune, in semitones.
    pub fn pitch(&self) -> f32 {
        self.transpose + self.detune / 100.0
    }

    /// Playback speed requested by transpose and detune (1 = as recorded).
    pub fn pitch_ratio(&self) -> f64 {
        2f64.powf(f64::from(self.pitch()) / 12.0)
    }

    /// Source speed factor per clip tick from transpose and detune: `1.0` for
    /// pitch-preserving warp modes, or [`pitch_ratio`](Self::pitch_ratio) when
    /// Warp is off or set to `Re-Pitch`.
    pub fn speed_ratio(&self) -> f64 {
        if !self.enabled || self.mode == WarpMode::Repitch { self.pitch_ratio() } else { 1.0 }
    }

    /// The markers that bend time, in order: each later in the file and later
    /// in the music than the one before (any that is not is left out). Fewer
    /// than two bend nothing, and the result is empty: the file then plays at
    /// one tempo, [`original_bpm`](Self::original_bpm).
    pub fn anchors(&self) -> Vec<(u64, Tick)> {
        let mut sorted = self.markers.clone();
        sorted.sort_unstable();
        let mut out: Vec<(u64, Tick)> = Vec::with_capacity(sorted.len());
        for m in sorted {
            if out.last().is_none_or(|l| m.0 > l.0 && m.1 > l.1) {
                out.push(m);
            }
        }
        if out.len() < 2 {
            out.clear();
        }
        out
    }

    /// Frame of a file at `rate` that source tick `tick` plays: along the
    /// line between the two markers around it, and beyond the first and last
    /// marker at the pace of the stretch next to them. Without markers (see
    /// [`anchors`](Self::anchors)) the file runs at `original_bpm` from its
    /// first frame; `None` when that is not known either.
    pub fn frame_at(&self, tick: f64, rate: f64) -> Option<f64> {
        let a = self.anchors();
        if a.is_empty() {
            let bpm = self.original_bpm.filter(|b| *b > 0.0)?;
            return Some(tick * rate * 60.0 / (bpm * PPQ as f64));
        }
        let k = a.windows(2).position(|w| tick <= w[1].1 as f64).unwrap_or(a.len() - 2);
        let ((f0, t0), (f1, t1)) = (a[k], a[k + 1]);
        Some(f0 as f64 + (f1 - f0) as f64 * (tick - t0 as f64) / (t1 - t0) as f64)
    }

    /// The source tick that `frame` of a file at `rate` plays at: the inverse
    /// of [`frame_at`](Self::frame_at).
    pub fn tick_at(&self, frame: f64, rate: f64) -> Option<f64> {
        let a = self.anchors();
        if a.is_empty() {
            let bpm = self.original_bpm.filter(|b| *b > 0.0)?;
            return (rate > 0.0).then(|| frame * bpm * PPQ as f64 / (rate * 60.0));
        }
        let k = a.windows(2).position(|w| frame <= w[1].0 as f64).unwrap_or(a.len() - 2);
        let ((f0, t0), (f1, t1)) = (a[k], a[k + 1]);
        Some(t0 as f64 + (t1 - t0) as f64 * (frame - f0 as f64) / (f1 - f0) as f64)
    }

    /// Give the file a new tempo. The markers keep their frames and count
    /// their beats at the new tempo, so a file with markers is stretched as a
    /// whole (twice the tempo: the same audio is twice as many beats), as one
    /// without them is.
    pub fn set_original_bpm(&mut self, bpm: f64) {
        if !(bpm.is_finite() && bpm > 0.0) {
            return;
        }
        if let Some(old) = self.original_bpm.filter(|b| *b > 0.0) {
            let k = bpm / old;
            for m in &mut self.markers {
                m.1 = (m.1 as f64 * k).round() as Tick;
            }
        }
        self.original_bpm = Some(bpm);
    }
}

impl Default for Warp {
    fn default() -> Self {
        Self { enabled: false, mode: WarpMode::Beats, original_bpm: None, transpose: 0.0, detune: 0.0, reverse: false, markers: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarpMode {
    /// Slice at transients (drums & percussion loops).
    Beats,
    /// Monophonic pitch-period stretch (vocals, bass, solo lines).
    Tones,
    /// Granular stretch with smooth grain scatter (pads & ambiences).
    Texture,
    /// Phase-locked polyphonic stretch (full mixes & chords).
    Complex,
    /// Formant-preserving polyphonic stretch (vocals & harmonic mixes).
    ComplexPro,
    /// Speed changes pitch (vinyl).
    Repitch,
}

impl WarpMode {
    pub const ALL: [Self; 6] = [Self::Beats, Self::Tones, Self::Texture, Self::Complex, Self::ComplexPro, Self::Repitch];

    pub fn label(self) -> &'static str {
        match self {
            Self::Beats => "Beats",
            Self::Tones => "Tones",
            Self::Texture => "Texture",
            Self::Complex => "Complex",
            Self::ComplexPro => "Complex Pro",
            Self::Repitch => "Re-Pitch",
        }
    }

    pub fn preserves_pitch(self) -> bool {
        self != Self::Repitch
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&m| m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
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
    /// For a modulator (`mod.*`): the parameters it drives on its track.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<ModMapping>,
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
            mods: Vec::new(),
        }
    }

    /// Whether this is a modulator device (LFO, Envelope Follower…).
    pub fn is_modulator(&self) -> bool {
        matches!(&self.kind, DeviceKind::Stock { plugin_id } if plugin_id.starts_with("mod."))
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
    #[serde(default)]
    pub chain_selector: u8,
    /// Internal return buses (effect chains the pads send to). Only a Drum
    /// Rack has them, [`MAX_RETURNS`] at most; a pad's [`Chain::sends`] are
    /// levels into them, in this order.
    #[serde(default)]
    pub returns: Vec<Chain>,
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
            returns: Vec::new(),
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
    /// Fade widths at the low and high end of the key zone (in keys): the
    /// velocity of a note fades out over them.
    #[serde(default)]
    pub key_fade: (u8, u8),
    /// The same for the velocity zone.
    #[serde(default)]
    pub vel_fade: (u8, u8),
    /// The same for the chain-selector zone (it fades the chain's volume).
    #[serde(default)]
    pub sel_fade: (u8, u8),
    /// Drum Rack choke group, 1..=16; 0 = none. Hitting a pad chokes the
    /// other pads of its group.
    #[serde(default)]
    pub choke: u8,
    /// Drum Rack: the note the pad's devices get when it is hit (`note` is
    /// the one that hits it). `None` = the default, middle C.
    #[serde(default)]
    pub play_note: Option<u8>,
    /// Drum Rack: send level in dB into each of the rack's returns, in
    /// order; missing entries are off ([`SEND_OFF_DB`]).
    #[serde(default)]
    pub sends: Vec<f32>,
    /// Where the chain's audio goes.
    #[serde(default)]
    pub output: ChainOut,
}

/// Most internal returns a Drum Rack has.
pub const MAX_RETURNS: usize = 6;
/// Send level that means "off".
pub const SEND_OFF_DB: f32 = -60.0;
/// Highest send level.
pub const SEND_MAX_DB: f32 = 6.0;

/// Where a rack chain's output goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "index")]
pub enum ChainOut {
    /// Summed with the other chains into the rack's output.
    #[default]
    Rack,
    /// Straight into the rack's return `n` (instead of the rack output).
    Return(u8),
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
            key_fade: (0, 0),
            vel_fade: (0, 0),
            sel_fade: (0, 0),
            choke: 0,
            play_note: None,
            sends: Vec::new(),
            output: ChainOut::Rack,
        }
    }
}

/// What a modulator drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ModTarget {
    /// A parameter of a device on the modulator's track (stock or hosted).
    Param { device: DeviceId, param: u32 },
    /// The track's fader.
    Volume,
    /// The track's pan.
    Pan,
}

/// The most targets one modulator drives.
pub const MAX_MOD_MAPPINGS: usize = 8;

/// One target of a modulator. The modulator's output `m` (0..=1) moves the
/// target by `min + (max - min) * m` in normalised units on top of the
/// knob's (or automation's) value, which stays editable. `min > max`
/// inverts; `-d..d` is a bipolar depth.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModMapping {
    pub target: ModTarget,
    pub min: f32,
    pub max: f32,
}

impl ModMapping {
    /// A bipolar mapping of depth `d` around the base value.
    pub fn bipolar(target: ModTarget, d: f32) -> Self {
        Self { target, min: -d, max: d }
    }

    /// The offset (normalised units) at modulator output `m`.
    #[inline]
    pub fn offset(&self, m: f32) -> f32 {
        self.min + (self.max - self.min) * m
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

impl MacroMapping {
    /// Whether the macro sweeps the parameter downwards (`min > max`).
    pub fn is_inverted(&self) -> bool {
        self.min > self.max
    }
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
    /// Clip envelopes only: transpose in semitones (see [`ClipEnv`]).
    Transpose,
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
    fn warp_markers_map_ticks_to_frames_and_back() {
        // No markers: one tempo from the first frame. 120 BPM at 48 kHz is 24 000 frames a beat.
        let plain = Warp { original_bpm: Some(120.0), ..Warp::default() };
        assert_eq!(plain.frame_at(PPQ as f64, 48_000.0), Some(24_000.0));
        assert_eq!(plain.tick_at(48_000.0, 48_000.0), Some(2.0 * PPQ as f64));
        assert_eq!(Warp::default().frame_at(0.0, 48_000.0), None, "no tempo, no map");
        // One marker bends nothing.
        let one = Warp { markers: vec![(1_000, PPQ)], ..plain.clone() };
        assert!(one.anchors().is_empty());
        assert_eq!(one.frame_at(PPQ as f64, 48_000.0), Some(24_000.0));
        // A beat played late and the next one early: the line between the markers, and its pace beyond them.
        let bent = Warp { markers: vec![(48_000, 2 * PPQ), (24_500, PPQ), (71_000, 3 * PPQ)], ..plain };
        assert_eq!(bent.anchors(), vec![(24_500, PPQ), (48_000, 2 * PPQ), (71_000, 3 * PPQ)], "kept in order");
        assert_eq!(bent.frame_at(1.5 * PPQ as f64, 48_000.0), Some(36_250.0));
        assert_eq!(bent.frame_at(0.0, 48_000.0), Some(1_000.0), "before the first marker at the first stretch's pace");
        assert_eq!(bent.frame_at(4.0 * PPQ as f64, 48_000.0), Some(94_000.0), "after the last at the last one's");
        for frame in [0.0, 10_000.0, 24_500.0, 40_000.0, 60_000.0, 90_000.0] {
            let tick = bent.tick_at(frame, 48_000.0).unwrap();
            assert!((bent.frame_at(tick, 48_000.0).unwrap() - frame).abs() < 1e-6, "{frame}");
        }
        // A marker that goes back in the music is not an anchor.
        let odd = Warp { markers: vec![(100, 10), (200, 5), (300, 20)], ..Warp::default() };
        assert_eq!(odd.anchors(), vec![(100, 10), (300, 20)]);
    }

    #[test]
    fn a_new_tempo_takes_the_markers_with_it() {
        let mut w = Warp { original_bpm: Some(100.0), markers: vec![(0, 0), (48_000, 2 * PPQ)], ..Warp::default() };
        let frame = w.frame_at(PPQ as f64, 48_000.0);
        w.set_original_bpm(200.0);
        assert_eq!(w.markers, vec![(0, 0), (48_000, 4 * PPQ)], "the same audio is twice as many beats");
        assert_eq!(w.frame_at(2.0 * PPQ as f64, 48_000.0), frame, "and every frame keeps its place in it");
        w.set_original_bpm(f64::NAN);
        w.set_original_bpm(0.0);
        assert_eq!(w.original_bpm, Some(200.0));
        let mut fresh = Warp { markers: vec![(0, 0), (10, 10)], ..Warp::default() };
        fresh.set_original_bpm(90.0);
        assert_eq!((fresh.original_bpm, fresh.markers.len()), (Some(90.0), 2), "a first tempo scales nothing");
    }

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
    fn clip_pan_and_transpose_envelopes_round_trip_and_old_files_have_none() {
        let mut c = Clip::new("c", Color(0), SourceId(7), 960);
        c.envelopes.push(AutomationLane { points: vec![AutomationPoint { tick: 0, value: -0.5, curve: 0.3 }], ..AutomationLane::new(AutomationTarget::Pan) });
        c.envelopes.push(AutomationLane { points: vec![AutomationPoint { tick: 480, value: 7.0, curve: 0.0 }], ..AutomationLane::new(AutomationTarget::Transpose) });
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""type":"transpose""#), "{json}");
        let back: Clip = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        assert_eq!((back.pan_at(0), back.transpose_at(960)), (-0.5, 7.0));
        // A clip saved before they existed: flat, nothing to play.
        let old = Clip::new("c", Color(0), SourceId(7), 960);
        assert_eq!((old.pan_at(100), old.transpose_at(100)), (0.0, 0.0));
        assert!(old.pan_envelope().is_none() && old.transpose_envelope().is_none());
    }

    #[test]
    fn clip_env_ranges_and_pan_law() {
        for kind in ClipEnv::ALL {
            let (lo, hi) = kind.range();
            assert_eq!((kind.unit(lo), kind.unit(hi)), (0.0, 1.0));
            assert_eq!(kind.from_unit(kind.unit(kind.neutral())), kind.neutral());
            assert_eq!(kind.clamp(f32::NAN), kind.neutral());
            assert_eq!(ClipEnv::of(kind.target()), Some(kind));
        }
        assert_eq!(clip_pan_gains(0.0), (1.0, 1.0));
        assert_eq!(clip_pan_gains(1.0).0, 0.0);
        assert!((clip_pan_gains(1.0).1 - 1.0).abs() < 1e-6);
        assert!((clip_pan_gains(-1.0).0 - 1.0).abs() < 1e-6 && clip_pan_gains(-1.0).1.abs() < 1e-6);
        let (l, r) = clip_pan_gains(0.5);
        assert!(l < 1.0 && r == 1.0);
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

    #[test]
    fn track_defaults_scale_and_keyboard_input_when_missing() {
        let track = Track::new(TrackId(7), TrackKind::Instrument, "Keys", Color(2));
        let mut val = serde_json::to_value(&track).unwrap();
        val.as_object_mut().unwrap().remove("scale");
        val.as_object_mut().unwrap().remove("keyboard_input");
        let parsed: Track = serde_json::from_value(val).unwrap();
        assert_eq!(parsed.scale, None);
        assert!(!parsed.keyboard_input);
    }
}
