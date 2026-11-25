//! `instrument.drumsynth` — a synthetic percussion machine of nine types
//! (Kick, Snare, Rimshot, Hihat, Clap, Tom, Perc, Cymbal, FX), each with
//! two or three engines (26 in all) and its own Tune, Decay, Tone, Snap,
//! Level and Pan.
//!
//! Two ways to play it. In **Kit** mode every note of a General-MIDI-like
//! layout (C1 = 36 kick, 38 snare, 39 clap, 42 closed hat, 46 open hat, the
//! toms, the cymbals…) strikes its type, so a drum track drives it as it is;
//! the closed hat cuts the open one. In **Mono** mode one selected type is
//! played chromatically on any key, one voice at a time.
//!
//! Voices are preallocated (13 in all: one per type, two for the types that
//! ring over each other), nothing allocates in `process`.
//!
//! Meter layout: `[level of each type ×9, peak, active voices, last type
//! struck (−1 for none), selected type]`.

mod voice;

pub use voice::{Hit, KINDS, Kind, Settings};

use crate::common::{ParamStore, Segmented, drive, load_state, pan_gains, sanitize, save_state, write_meter};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use voice::Voice;

pub const ID: &str = "instrument.drumsynth";

/// The parameters. A type's own parameters sit at [`p::param`]`(kind, …)`.
pub mod p {
    pub const MODE: u32 = 0;
    pub const SELECT: u32 = 1;
    pub const VOLUME: u32 = 2;
    pub const VEL_SENS: u32 = 3;

    /// Offsets inside a type's block of eight ids.
    pub const MODEL: u32 = 0;
    pub const TUNE: u32 = 1;
    pub const DECAY: u32 = 2;
    pub const TONE: u32 = 3;
    pub const SNAP: u32 = 4;
    pub const LEVEL: u32 = 5;
    pub const PAN: u32 = 6;

    /// The id of parameter `field` of type `kind` (0..9).
    pub const fn param(kind: usize, field: u32) -> u32 {
        16 + kind as u32 * 8 + field
    }
}

pub const MODES: &[&str] = &["Kit", "Mono"];
pub const TYPE_NAMES: &[&str] = &["Kick", "Snare", "Rimshot", "Hihat", "Clap", "Tom", "Perc", "Cymbal", "FX"];

/// The decay ranges (ms) of each type: min, max, default.
const DECAYS: [(f32, f32, f32); KINDS] = [
    (80.0, 2500.0, 450.0),
    (60.0, 1000.0, 220.0),
    (20.0, 500.0, 90.0),
    (40.0, 1500.0, 320.0),
    (80.0, 1200.0, 300.0),
    (80.0, 2000.0, 420.0),
    (40.0, 1500.0, 220.0),
    (300.0, 6000.0, 1800.0),
    (100.0, 4000.0, 700.0),
];

/// Where each type sits in the stereo field by default, in percent.
const PANS: [f32; KINDS] = [0.0, 0.0, -10.0, 20.0, 0.0, 0.0, -20.0, 30.0, 0.0];

/// What the Tone and Snap knobs mean for each type: (name, short) × 2.
const LABELS: [[(&str, &str); 2]; KINDS] = [
    [("Kick Sweep", "Sweep"), ("Kick Click", "Click")],
    [("Snare Tone", "Tone"), ("Snare Snares", "Snares")],
    [("Rimshot Tone", "Tone"), ("Rimshot Click", "Click")],
    [("Hihat Tone", "Tone"), ("Hihat Snap", "Snap")],
    [("Clap Tone", "Tone"), ("Clap Spread", "Spread")],
    [("Tom Drop", "Drop"), ("Tom Click", "Click")],
    [("Perc Color", "Color"), ("Perc Snap", "Snap")],
    [("Cymbal Tone", "Tone"), ("Cymbal Noise", "Noise")],
    [("FX Range", "Range"), ("FX Bite", "Bite")],
];

macro_rules! names {
    ($k:literal) => {
        [concat!($k, " Model"), concat!($k, " Tune"), concat!($k, " Decay"), concat!($k, " Level"), concat!($k, " Pan")]
    };
}

/// `[model, tune, decay, level, pan]` names of each type.
const NAMES: [[&str; 5]; KINDS] =
    [names!("Kick"), names!("Snare"), names!("Rimshot"), names!("Hihat"), names!("Clap"), names!("Tom"), names!("Perc"), names!("Cymbal"), names!("FX")];

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, 0.0).choices(MODES).short("Mode"),
        ParamInfo::new(p::SELECT, "Type", 0.0, (KINDS - 1) as f32, 0.0).choices(TYPE_NAMES).short("Type"),
        ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, 0.0).unit("dB").short("Vol"),
        ParamInfo::new(p::VEL_SENS, "Velocity", 0.0, 100.0, 80.0).unit("%").short("Vel"),
    ];
    for kind in Kind::ALL {
        let k = kind.index();
        let id = |f: u32| p::param(k, f);
        let (dmin, dmax, ddef) = DECAYS[k];
        let models = kind.models();
        v.push(ParamInfo::new(id(p::MODEL), NAMES[k][0], 0.0, (models.len() - 1) as f32, 0.0).choices(models).short("Model"));
        v.push(ParamInfo::new(id(p::TUNE), NAMES[k][1], -12.0, 12.0, 0.0).unit("st").short("Tune"));
        v.push(ParamInfo::new(id(p::DECAY), NAMES[k][2], dmin, dmax, ddef).unit("ms").log().short("Decay"));
        v.push(ParamInfo::new(id(p::TONE), LABELS[k][0].0, 0.0, 100.0, 50.0).unit("%").short(LABELS[k][0].1));
        v.push(ParamInfo::new(id(p::SNAP), LABELS[k][1].0, 0.0, 100.0, 50.0).unit("%").short(LABELS[k][1].1));
        v.push(ParamInfo::new(id(p::LEVEL), NAMES[k][3], -60.0, 6.0, -3.0).unit("dB").short("Level"));
        v.push(ParamInfo::new(id(p::PAN), NAMES[k][4], -100.0, 100.0, PANS[k]).unit("%").short("Pan"));
    }
    v
}

/// What a key strikes in Kit mode: the type, and what the key adds.
pub fn map_key(key: u8) -> Option<(Kind, Hit)> {
    let h = |tune_st: f32, decay_mul: f32| Hit { tune_st, decay_mul };
    Some(match key {
        35 => (Kind::Kick, h(-2.0, 0.9)),
        36 => (Kind::Kick, h(0.0, 1.0)),
        37 => (Kind::Rimshot, h(0.0, 1.0)),
        38 => (Kind::Snare, h(0.0, 1.0)),
        40 => (Kind::Snare, h(1.5, 0.8)),
        39 => (Kind::Clap, h(0.0, 1.0)),
        41 => (Kind::Tom, h(-10.0, 1.0)),
        43 => (Kind::Tom, h(-7.0, 1.0)),
        45 => (Kind::Tom, h(-4.0, 0.95)),
        47 => (Kind::Tom, h(-1.0, 0.9)),
        48 => (Kind::Tom, h(2.0, 0.85)),
        50 => (Kind::Tom, h(5.0, 0.8)),
        42 => (Kind::Hihat, h(0.0, 0.2)),
        44 => (Kind::Hihat, h(-1.0, 0.12)),
        46 => (Kind::Hihat, h(0.0, 1.0)),
        49 => (Kind::Cymbal, h(0.0, 1.0)),
        57 => (Kind::Cymbal, h(2.0, 0.85)),
        51 => (Kind::Cymbal, h(0.0, 0.55)),
        59 => (Kind::Cymbal, h(1.0, 0.5)),
        52 => (Kind::Cymbal, h(4.0, 0.7)),
        53 => (Kind::Cymbal, h(6.0, 0.35)),
        55 => (Kind::Cymbal, h(8.0, 0.4)),
        54 => (Kind::Perc, h(-4.0, 1.0)),
        56 => (Kind::Perc, h(0.0, 1.0)),
        58 => (Kind::Perc, h(4.0, 1.0)),
        60..=83 => (Kind::Perc, h(key as f32 - 72.0, 1.0)),
        84..=107 => (Kind::Fx, h(key as f32 - 96.0, 1.0)),
        _ => return None,
    })
}

/// The keys a type answers to in Kit mode, for display.
pub fn keys_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Kick => "C1 · B0",
        Kind::Snare => "D1 · E1",
        Kind::Rimshot => "C#1",
        Kind::Hihat => "F#1 · G#1 · A#1",
        Kind::Clap => "D#1",
        Kind::Tom => "F1 G1 A1 B1 C2 D2",
        Kind::Perc => "F#2 · G#2 · A#2 · C3–B4",
        Kind::Cymbal => "C#2 D#2 E2 F2 G2 A2 B2",
        Kind::Fx => "C5–B7",
    }
}

/// Voices of each type: the first index and how many.
const SLOTS: [(usize, usize); KINDS] = [(0, 1), (1, 1), (2, 1), (3, 1), (4, 1), (5, 2), (7, 2), (9, 2), (11, 2)];
const VOICES: usize = 13;

/// What the knobs of a type say, ready for the audio thread.
#[derive(Clone, Copy, Debug)]
struct Cached {
    settings: Settings,
    gain: f32,
    pan_l: f32,
    pan_r: f32,
}

pub struct DrumSynth {
    info: PluginInfo,
    params: ParamStore,
    sr: f32,
    voices: Vec<Voice>,
    cached: [Cached; KINDS],
    mono: bool,
    selected: Kind,
    vel_sens: f32,
    gain: f32,
    peak: f32,
    last_kind: i32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(DrumSynth::new())
}

impl Default for DrumSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumSynth {
    pub fn new() -> Self {
        let params = params();
        let blank = Cached { settings: Settings { model: 0, tune: 0.0, decay_ms: 200.0, tone: 0.5, snap: 0.5 }, gain: 1.0, pan_l: 1.0, pan_r: 1.0 };
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Drum Synth",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["drums", "synth", "kit", "kick", "snare", "hat", "clap", "percussion", "808", "909"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            voices: (0..VOICES as u32).map(|i| Voice::new(i + 1)).collect(),
            cached: [blank; KINDS],
            mono: false,
            selected: Kind::Kick,
            vel_sens: 0.8,
            gain: 1.0,
            peak: 0.0,
            last_kind: -1,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.mono = pr.choice(p::MODE) == 1;
        self.selected = Kind::from_index(pr.choice(p::SELECT));
        self.gain = db_to_gain(pr.plain(p::VOLUME));
        self.vel_sens = pr.plain(p::VEL_SENS) / 100.0;
        for kind in Kind::ALL {
            let k = kind.index();
            let id = |f: u32| p::param(k, f);
            let (l, r) = pan_gains(pr.plain(id(p::PAN)) / 100.0);
            self.cached[k] = Cached {
                settings: Settings {
                    model: pr.choice(id(p::MODEL)),
                    tune: pr.plain(id(p::TUNE)),
                    decay_ms: pr.plain(id(p::DECAY)),
                    tone: pr.plain(id(p::TONE)) / 100.0,
                    snap: pr.plain(id(p::SNAP)) / 100.0,
                },
                gain: db_to_gain(pr.plain(id(p::LEVEL))),
                pan_l: l * std::f32::consts::SQRT_2,
                pan_r: r * std::f32::consts::SQRT_2,
            };
        }
    }

    /// The voice of `kind` a new hit takes: an idle one, else the oldest.
    fn pick(&self, kind: Kind) -> usize {
        let (first, n) = SLOTS[kind.index()];
        let mut best = first;
        for i in first..first + n {
            if !self.voices[i].active() {
                return i;
            }
            if self.voices[i].age() > self.voices[best].age() {
                best = i;
            }
        }
        best
    }

    fn strike(&mut self, kind: Kind, hit: Hit, key: u8, vel: f32) {
        let gain = 1.0 - self.vel_sens + self.vel_sens * vel.clamp(0.0, 1.0);
        let i = if self.mono { SLOTS[kind.index()].0 } else { self.pick(kind) };
        if self.mono {
            // One voice at a time: whatever else rings gives way.
            for (j, v) in self.voices.iter_mut().enumerate() {
                if j != i {
                    v.fade_out();
                }
            }
        }
        let s = self.cached[kind.index()].settings;
        self.voices[i].trigger(kind, &s, hit, gain, key as u32);
        self.last_kind = kind.index() as i32;
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        if self.mono {
            let hit = Hit { tune_st: key as f32 - 60.0, decay_mul: 1.0 };
            self.strike(self.selected, hit, key, vel);
        } else if let Some((kind, hit)) = map_key(key) {
            self.strike(kind, hit, key, vel);
        }
    }

    fn silence(&mut self) {
        for v in &mut self.voices {
            v.kill();
        }
    }
}

impl Segmented for DrumSynth {
    fn event(&mut self, kind: &EventKind) {
        if self.params.take_dirty() {
            self.update();
        }
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, .. } if velocity > 0.0 => self.note_on(key, velocity),
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => self.silence(),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let buf = &mut *ctx.buffer;
        let stereo = buf.channels() >= 2;
        for ch in 0..buf.channels() {
            buf.channel_mut(ch)[start..end].fill(0.0);
        }
        let mut peak = self.peak * (-((end - start) as f32) / (0.15 * self.sr)).exp();
        for i in start..end {
            let (mut l, mut r) = (0.0, 0.0);
            for v in &mut self.voices {
                if v.active() {
                    let s = v.tick();
                    let c = &self.cached[v.kind().index()];
                    let g = s * c.gain;
                    l += g * c.pan_l;
                    r += g * c.pan_r;
                }
            }
            let (l, r) = (sanitize(l * self.gain), sanitize(r * self.gain));
            peak = peak.max(l.abs()).max(r.abs());
            if stereo {
                buf.channel_mut(0)[i] = l;
                buf.channel_mut(1)[i] = r;
            } else {
                buf.channel_mut(0)[i] = 0.5 * (l + r);
            }
        }
        self.peak = peak;
    }
}

impl Processor for DrumSynth {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate as f32;
        for v in &mut self.voices {
            v.set_sample_rate(self.sr);
        }
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.silence();
        self.peak = 0.0;
        self.last_kind = -1;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn tail(&self) -> u32 {
        let longest = Kind::ALL.iter().map(|k| self.params.plain(p::param(k.index(), p::DECAY))).fold(0.0f32, f32::max);
        (f64::from(longest) * 0.0012 * f64::from(self.sr)) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let mut levels = [0.0f32; KINDS + 4];
        let mut voices = 0.0;
        for v in &self.voices {
            if v.active() {
                voices += 1.0;
                let k = v.kind().index();
                levels[k] = levels[k].max(v.level().min(1.0));
            }
        }
        levels[KINDS] = self.peak;
        levels[KINDS + 1] = voices;
        levels[KINDS + 2] = self.last_kind as f32;
        levels[KINDS + 3] = self.params.choice(p::SELECT) as f32;
        write_meter(out, 0, &levels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, get_plain, peak, render_notes, rms, set_plain};

    fn note(frame: usize, key: u8, vel: f32) -> (usize, u8, f32, bool) {
        (frame, key, vel, true)
    }

    fn key_param(kind: Kind, field: u32) -> u32 {
        p::param(kind.index(), field)
    }

    #[test]
    fn param_blocks_are_consistent() {
        let ps = params();
        assert_eq!(ps.len(), 4 + KINDS * 7);
        let mut ids: Vec<u32> = ps.iter().map(|i| i.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ps.len());
        for kind in Kind::ALL {
            let info = ps.iter().find(|i| i.id == key_param(kind, p::MODEL)).unwrap();
            assert_eq!(info.choices, kind.models());
            assert_eq!(info.max as usize, kind.models().len() - 1);
        }
        assert!(Kind::ALL.iter().map(|k| k.models().len()).sum::<usize>() >= 20, "over 20 engines");
    }

    #[test]
    fn kit_notes_strike_their_own_type() {
        for (key, kind) in [(36u8, Kind::Kick), (38, Kind::Snare), (37, Kind::Rimshot), (39, Kind::Clap), (42, Kind::Hihat), (47, Kind::Tom), (56, Kind::Perc), (49, Kind::Cymbal), (90, Kind::Fx)] {
            assert_eq!(map_key(key).unwrap().0, kind, "key {key}");
            let mut d = DrumSynth::new();
            let (l, _) = render_notes(&mut d, &[note(0, key, 1.0)], 24_000);
            assert!(peak(&l) > 0.05, "key {key} is silent");
        }
        assert!(map_key(20).is_none() && map_key(120).is_none());
    }

    #[test]
    fn a_type_that_is_not_struck_stays_silent() {
        let mut d = DrumSynth::new();
        let (l, r) = render_notes(&mut d, &[note(0, 36, 1.0)], 4_800);
        assert!(peak(&l) > 0.1 && peak(&r) > 0.1);
        let mut m = [0.0f32; 16];
        d.meter(&mut m);
        assert!(m[Kind::Kick.index()] > 0.1);
        assert_eq!(m[Kind::Snare.index()], 0.0);
        assert_eq!(m[KINDS + 2], 0.0, "the last type struck is the kick");
    }

    #[test]
    fn rendering_is_deterministic() {
        let notes: Vec<_> = [36u8, 42, 38, 39, 46, 47, 49, 56, 84].iter().enumerate().map(|(i, &k)| note(i * 3_000, k, 0.8)).collect();
        let a = render_notes(&mut DrumSynth::new(), &notes, 60_000);
        let b = render_notes(&mut DrumSynth::new(), &notes, 60_000);
        assert_eq!(a, b);
    }

    #[test]
    fn velocity_scales_the_level() {
        let loud = render_notes(&mut DrumSynth::new(), &[note(0, 38, 1.0)], 12_000).0;
        let soft = render_notes(&mut DrumSynth::new(), &[note(0, 38, 0.3)], 12_000).0;
        assert!(rms(&loud) > rms(&soft) * 1.5, "{} vs {}", rms(&loud), rms(&soft));
        let mut flat = DrumSynth::new();
        set_plain(&mut flat, p::VEL_SENS, 0.0);
        let a = render_notes(&mut flat, &[note(0, 36, 1.0)], 12_000).0;
        let b = render_notes(&mut flat, &[note(0, 36, 0.2)], 12_000).0;
        assert!((rms(&a) - rms(&b)).abs() < 1e-4, "no velocity sensitivity gives equal hits");
    }

    #[test]
    fn the_closed_hat_chokes_the_open_hat() {
        let long = |d: &mut DrumSynth| set_plain(d, p::param(Kind::Hihat.index(), p::DECAY), 1_500.0);
        let mut open_only = DrumSynth::new();
        long(&mut open_only);
        let (a, _) = render_notes(&mut open_only, &[note(0, 46, 1.0)], 30_000);
        let mut choked = DrumSynth::new();
        long(&mut choked);
        let (b, _) = render_notes(&mut choked, &[note(0, 46, 1.0), note(4_800, 42, 1.0)], 30_000);
        // The closed hat (300 ms) is long over by 0.4 s; the open one still rings.
        assert!(rms(&a[20_000..26_000]) > 1e-3, "the open hat rings");
        assert!(rms(&b[20_000..26_000]) < rms(&a[20_000..26_000]) * 0.1, "{} vs {}", rms(&b[20_000..26_000]), rms(&a[20_000..26_000]));
        assert!(b.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn retriggering_does_not_click() {
        let mut d = DrumSynth::new();
        set_plain(&mut d, p::param(Kind::Kick.index(), p::DECAY), 2_000.0);
        let (l, _) = render_notes(&mut d, &[note(0, 36, 1.0), note(10_000, 36, 1.0)], 14_000);
        let step = l[9_990..10_100].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let normal = l[2_000..2_110].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(step < normal.max(0.05) * 3.0, "retrigger step {step}");
    }

    #[test]
    fn mono_mode_plays_the_selected_type_chromatically() {
        let mut d = DrumSynth::new();
        set_plain(&mut d, p::MODE, 1.0);
        set_plain(&mut d, p::SELECT, Kind::Tom.index() as f32);
        // Any key plays the tom here.
        let (low, _) = render_notes(&mut d, &[note(0, 48, 1.0)], 24_000);
        assert!(peak(&low) > 0.1);
        let mut d2 = DrumSynth::new();
        set_plain(&mut d2, p::MODE, 1.0);
        set_plain(&mut d2, p::SELECT, Kind::Tom.index() as f32);
        let (high, _) = render_notes(&mut d2, &[note(0, 72, 1.0)], 24_000);
        let zc = |x: &[f32]| (1..x.len()).filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0).count();
        assert!(zc(&high[6_000..18_000]) > zc(&low[6_000..18_000]) * 3 / 2, "two octaves up is higher");
        // A new note silences the previous one's voice: one at a time.
        let mut m = DrumSynth::new();
        set_plain(&mut m, p::MODE, 1.0);
        set_plain(&mut m, p::SELECT, Kind::Kick.index() as f32);
        let _ = render_notes(&mut m, &[note(0, 60, 1.0), note(2_000, 64, 1.0)], 8_000);
        let mut meter = [0.0f32; 16];
        m.meter(&mut meter);
        assert!(meter[KINDS + 1] <= 1.0, "one voice");
    }

    #[test]
    fn every_model_of_every_type_plays_in_kit_and_mono() {
        for kind in Kind::ALL {
            for model in 0..kind.models().len() {
                let mut d = DrumSynth::new();
                set_plain(&mut d, key_param(kind, p::MODEL), model as f32);
                let key = [36u8, 38, 37, 42, 39, 45, 56, 49, 90][kind.index()];
                let (l, r) = render_notes(&mut d, &[note(0, key, 0.9)], 24_000);
                assert!(peak(&l) > 0.05 && l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 4.0), "{} {}", kind.label(), kind.models()[model]);
            }
        }
    }

    #[test]
    fn extreme_parameters_stay_finite_and_bounded() {
        for pass in 0..3 {
            let mut d = DrumSynth::new();
            let ids: Vec<u32> = d.info().params.iter().map(|i| i.id).collect();
            for (j, id) in ids.iter().enumerate() {
                let v = match pass {
                    0 => 0.0,
                    1 => 1.0,
                    _ => ((j * 37 + 11) % 17) as f32 / 16.0,
                };
                d.set_param(*id, v);
            }
            let notes: Vec<_> = (35u8..=100).enumerate().map(|(i, k)| note(i * 700, k, 1.0)).collect();
            let (l, r) = render_notes(&mut d, &notes, 70_000);
            assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 40.0), "pass {pass}: peak {}", peak(&l));
            // The quiet after: no denormals, no hum.
            let (tl, _) = render_notes(&mut d, &[], 4_800);
            assert!(tl.iter().all(|v| v.abs() < 40.0));
        }
    }

    #[test]
    fn level_and_pan_shape_the_mix() {
        let mut d = DrumSynth::new();
        set_plain(&mut d, key_param(Kind::Snare, p::PAN), -100.0);
        let (l, r) = render_notes(&mut d, &[note(0, 38, 1.0)], 12_000);
        assert!(rms(&l) > rms(&r) * 20.0, "hard left");
        let mut q = DrumSynth::new();
        set_plain(&mut q, key_param(Kind::Snare, p::LEVEL), -60.0);
        let (ql, _) = render_notes(&mut q, &[note(0, 38, 1.0)], 12_000);
        assert!(peak(&ql) < 0.01);
    }

    #[test]
    fn decay_and_tune_do_what_they_say() {
        let short = render_notes(&mut DrumSynth::new(), &[note(0, 36, 1.0)], 48_000).0;
        let mut long = DrumSynth::new();
        set_plain(&mut long, key_param(Kind::Kick, p::DECAY), 2_000.0);
        let long = render_notes(&mut long, &[note(0, 36, 1.0)], 48_000).0;
        assert!(rms(&long[24_000..36_000]) > rms(&short[24_000..36_000]) * 5.0);
        assert!((get_plain(&DrumSynth::new(), key_param(Kind::Kick, p::DECAY)) - 450.0).abs() < 1.0);
        let zc = |x: &[f32]| (1..x.len()).filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0).count();
        let mut up = DrumSynth::new();
        set_plain(&mut up, key_param(Kind::Kick, p::TUNE), 12.0);
        set_plain(&mut up, key_param(Kind::Kick, p::SNAP), 0.0);
        let hi = render_notes(&mut up, &[note(0, 36, 1.0)], 24_000).0;
        let mut base = DrumSynth::new();
        set_plain(&mut base, key_param(Kind::Kick, p::SNAP), 0.0);
        let lo = render_notes(&mut base, &[note(0, 36, 1.0)], 24_000).0;
        assert!(zc(&hi[4_000..12_000]) as f32 > zc(&lo[4_000..12_000]) as f32 * 1.6, "an octave up");
    }

    #[test]
    fn all_notes_off_silences_and_state_round_trips() {
        let mut d = DrumSynth::new();
        set_plain(&mut d, key_param(Kind::Hihat, p::MODEL), 2.0);
        set_plain(&mut d, p::VOLUME, -6.0);
        let bytes = d.save_state();
        let mut e = DrumSynth::new();
        e.load_state(&bytes);
        let _ = render_notes(&mut e, &[], 64);
        assert!((get_plain(&e, key_param(Kind::Hihat, p::MODEL)) - 2.0).abs() < 1e-3);
        assert!((get_plain(&e, p::VOLUME) + 6.0).abs() < 0.01);
        assert!(SR > 0.0 && e.tail() > 0);
    }
}
