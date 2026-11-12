//! A seeded demo project (128 BPM, A minor) so every view has content.
//!
//! It is built with the same [`Command`]s the UI uses, applied straight to a
//! fresh project (not through `History`, so it starts with an empty undo
//! stack).

use aitrack_model::*;
use std::path::PathBuf;

const BAR: Tick = 4 * PPQ;

struct Builder {
    p: Project,
}

impl Builder {
    fn run(&mut self, c: Command) {
        if let Err(e) = c.apply(&mut self.p) {
            panic!("demo command failed: {e}");
        }
    }

    fn track(&mut self, kind: TrackKind, name: &str, color: usize, f: impl FnOnce(&mut Track)) -> TrackId {
        let id = self.p.alloc_track_id();
        let mut t = Track::new(id, kind, name, Color::palette(color));
        f(&mut t);
        let index = self.p.tracks.len() - 1; // before Main
        self.run(Command::AddTrack { track: Box::new(t), index });
        id
    }

    /// A stock device loaded with a factory preset (`Category/Name` inside
    /// the plugin's bank), then `overrides` by parameter name (normalised).
    fn stock(&mut self, plugin: &str, name: &str, preset: Option<&str>, overrides: &[(&str, f32)]) -> Device {
        let id = self.p.alloc_device_id();
        let mut d = Device::stock(id, plugin, name);
        if let Some(preset) = preset {
            let p = aitrack_stock::presets::load(format!("{plugin}/{preset}.json")).unwrap_or_else(|e| panic!("demo preset {plugin}/{preset}: {e}"));
            d.params = p.params.iter().filter_map(|(k, v)| Some((k.parse().ok()?, *v))).collect();
            d.preset = Some(p.name);
        }
        if !overrides.is_empty() {
            let proc = aitrack_stock::create(plugin).unwrap_or_else(|| panic!("demo device {plugin}"));
            for (pname, v) in overrides {
                let info = proc.info().params.iter().find(|i| i.name == *pname).unwrap_or_else(|| panic!("{plugin} has no param {pname}"));
                d.params.insert(info.id, *v);
            }
        }
        d
    }

    fn device(&mut self, track: TrackId, plugin: &str, name: &str, preset: Option<&str>, overrides: &[(&str, f32)]) -> DeviceId {
        let d = self.stock(plugin, name, preset, overrides);
        let id = d.id;
        let index = self.p.track(track).map_or(0, |t| t.devices.len());
        self.run(Command::AddDevice { slot: command::DeviceSlot::Track { track, index }, device: Box::new(d) });
        id
    }

    /// A drum rack on `track`: one drum sampler per pad, each receiving only
    /// its own note, which triggers the matching pad of the built-in kit
    /// (36 = pad 1 …), plus eight named macros.
    fn drum_rack(&mut self, track: TrackId) -> DeviceId {
        let rack_id = self.p.alloc_device_id();
        let mut rack = Rack::new(RackKind::Drum);
        for (i, (name, note)) in DRUM_PADS.iter().enumerate() {
            let mut chain = Chain::new(*name, Color::palette(i));
            chain.note = Some(*note);
            chain.key_range = (*note, *note);
            chain.devices.push(self.stock("instrument.drum", &format!("{name} Sampler"), Some("Kits/House Kit"), &[]));
            rack.chains.push(chain);
        }
        for (i, name) in ["Punch", "Decay", "Tone", "Room", "Swing", "Drive", "Width", "Volume"].iter().enumerate() {
            rack.macros[i].name = (*name).into();
            rack.macros[i].value = [0.62, 0.4, 0.55, 0.22, 0.1, 0.35, 0.5, 0.8][i];
        }
        let mut rack_dev = Device::stock(rack_id, "rack.drum", "Drum Rack");
        rack_dev.kind = DeviceKind::Rack(rack);
        self.run(Command::AddDevice { slot: command::DeviceSlot::Track { track, index: 0 }, device: Box::new(rack_dev) });
        rack_id
    }

    fn notes(&mut self, name: &str, color: usize, notes: &[(u8, Tick, Tick, f32)], length: Tick, loop_len: Option<Tick>) -> ClipId {
        let src = self.p.alloc_source_id();
        let notes: Vec<Note> = notes
            .iter()
            .map(|&(k, s, l, v)| Note::new(self.p.alloc_note_id(), k, s, l, v))
            .collect();
        let mut source = NoteSource { notes };
        source.sort();
        self.run(Command::AddSource { id: src, source: Box::new(Source::Notes(source)) });
        self.clip(name, color, src, length, loop_len)
    }

    fn clip(&mut self, name: &str, color: usize, src: SourceId, length: Tick, loop_len: Option<Tick>) -> ClipId {
        let id = self.p.alloc_clip_id();
        let clip = Clip { looping: loop_len.map(|l| LoopRegion { start: 0, end: l }), ..Clip::new(name, Color::palette(color), src, length) };
        self.run(Command::AddClip { id, clip: Box::new(clip) });
        id
    }

    fn slot(&mut self, track: TrackId, scene: usize, clip: ClipId) {
        let scene = self.p.scenes[scene].id;
        self.run(Command::SetSessionSlot { track, scene, clip: Some(clip) });
    }

    fn place(&mut self, track: TrackId, clip: ClipId, bars: &[i64]) {
        for &b in bars {
            let id = self.p.alloc_placement_id();
            self.run(Command::PlaceClip { track, placement: Placement { id, clip, start: b * BAR, lane: 0 } });
        }
    }

    fn automate(&mut self, track: TrackId, target: AutomationTarget, points: &[(i64, f32)]) {
        let mut lane = AutomationLane::new(target);
        for &(beats, value) in points {
            lane.insert(AutomationPoint { tick: beats * PPQ, value, curve: 0. });
        }
        self.run(Command::SetAutomation { track, target, lane: Some(lane) });
    }
}

/// Notes of the demo drum rack, named after the stock drum sampler's
/// built-in kit (note 36 = pad 1).
pub const DRUM_PADS: [(&str, u8); 8] = [
    ("Kick", 36),
    ("Snare", 38),
    ("Clap", 39),
    ("Closed Hat", 42),
    ("Open Hat", 46),
    ("Rim", 37),
    ("Tom", 45),
    ("808", 51),
];

/// An empty project: only the Main track, 120 BPM, no key.
pub fn empty() -> Project {
    Project::new("Untitled")
}

/// A starting point for electronic music: tracks already grouped the way a
/// mix is usually organised (Drums, Bass, Synths, Vocals, FX), two return
/// buses fed by every track's sends, and a limiter on Main. No clips.
pub fn electronic_starter() -> Project {
    let mut b = Builder { p: Project::new("Untitled") };
    b.run(Command::SetTempo { bpm: 126. });
    b.run(Command::SetScale { scale: Some(Scale::new(Key(9), ScaleKind::Minor)) });
    b.run(Command::SetLoop { region: Some((0, 8 * BAR)) });
    let mut content = Vec::new();
    let mut inside = |b: &mut Builder, group: TrackId, kind: TrackKind, name: &str, color: usize| {
        let id = b.track(kind, name, color, |t| {
            t.parent = Some(group);
            t.output = Output::Track(group);
        });
        content.push(id);
        id
    };
    let drums = b.track(TrackKind::Group, "Drums", 0, |_| {});
    let kit = inside(&mut b, drums, TrackKind::Instrument, "Drum Rack", 0);
    b.drum_rack(kit);
    inside(&mut b, drums, TrackKind::Audio, "Percussion", 0);
    b.device(drums, "fx.compressor", "Glue Comp", Some("Bus/Drum Bus Glue"), &[]);

    let bass = b.track(TrackKind::Group, "Bass", 7, |_| {});
    let sub = inside(&mut b, bass, TrackKind::Instrument, "Sub", 7);
    b.device(sub, "instrument.wavetable", "Sub Synth", Some("Bass/House Organ Bass"), &[]);
    let mid = inside(&mut b, bass, TrackKind::Instrument, "Mid Bass", 7);
    b.device(mid, "instrument.wavetable", "Bass Synth", Some("Bass/House Organ Bass"), &[]);

    let synths = b.track(TrackKind::Group, "Synths", 9, |_| {});
    for (name, preset) in [("Lead", "Lead/Supersaw Lead"), ("Pad", "Pad/Warm Pad"), ("Pluck", "Lead/Supersaw Lead")] {
        let t = inside(&mut b, synths, TrackKind::Instrument, name, 9);
        b.device(t, "instrument.wavetable", &format!("{name} Synth"), Some(preset), &[]);
    }

    let vocals = b.track(TrackKind::Group, "Vocals", 3, |_| {});
    inside(&mut b, vocals, TrackKind::Audio, "Lead Vocal", 3);
    inside(&mut b, vocals, TrackKind::Audio, "Chops", 3);

    let fx = b.track(TrackKind::Group, "FX", 11, |_| {});
    inside(&mut b, fx, TrackKind::Audio, "Risers", 11);
    inside(&mut b, fx, TrackKind::Audio, "Impacts", 11);

    let reverb = b.track(TrackKind::Return, "A Reverb", 5, |t| t.volume_db = -6.);
    let delay = b.track(TrackKind::Return, "B Delay", 6, |t| t.volume_db = -8.);
    b.device(reverb, "fx.reverb", "Reverb", Some("Halls/Big Hall"), &[("Mix", 1.0)]);
    b.device(delay, "fx.delay", "Delay", Some("Synced/Dotted Eighth"), &[("Mix", 1.0)]);
    // Every track has its sends in place, turned all the way down.
    for t in content {
        let sends = [reverb, delay].map(|target| Send { target, level_db: -70., pre_fader: false }).to_vec();
        b.run(Command::SetTrack { id: t, change: TrackProp::Sends(sends) });
    }
    let main = b.p.main_track().id;
    b.device(main, "fx.limiter", "Limiter", Some("Mastering/Transparent"), &[]);
    b.p
}

pub fn project() -> Project {
    let mut b = Builder { p: Project::new("Midnight Drive") };
    b.run(Command::SetTempo { bpm: 128. });
    b.run(Command::SetScale { scale: Some(Scale::new(Key(9), ScaleKind::Minor)) });
    b.run(Command::SetLoop { region: Some((8 * BAR, 16 * BAR)) });
    for (i, name) in ["Intro", "Verse", "Build", "Drop", "Break", "Drop 2", "Outro", "Spare"].iter().enumerate() {
        let id = b.p.scenes[i].id;
        b.run(Command::RenameScene { id, name: (*name).into() });
    }

    // ── Tracks ──
    let drums = b.track(TrackKind::Instrument, "Drums", 0, |t| t.volume_db = -2.);
    let bass = b.track(TrackKind::Instrument, "Bass", 7, |t| t.volume_db = -4.5);
    let synths = b.track(TrackKind::Group, "Synths", 9, |t| t.volume_db = -1.5);
    let lead = b.track(TrackKind::Instrument, "Lead", 10, |t| {
        t.parent = Some(synths);
        t.output = Output::Track(synths);
        t.volume_db = -6.;
        t.pan = 0.12;
    });
    let pad = b.track(TrackKind::Instrument, "Pad", 8, |t| {
        t.parent = Some(synths);
        t.output = Output::Track(synths);
        t.volume_db = -9.;
        t.pan = -0.18;
    });
    let vocals = b.track(TrackKind::Audio, "Vocals", 3, |t| {
        t.volume_db = -3.;
        t.arm = false;
    });
    let reverb = b.track(TrackKind::Return, "A Reverb", 5, |t| t.volume_db = -6.);
    let delay = b.track(TrackKind::Return, "B Delay", 6, |t| t.volume_db = -8.);
    let vca = b.track(TrackKind::Vca, "Music VCA", 13, |_| {});
    for (t, sends) in [
        (lead, vec![(reverb, -12.), (delay, -18.)]),
        (pad, vec![(reverb, -8.), (delay, -30.)]),
        (vocals, vec![(reverb, -10.), (delay, -16.)]),
        (bass, vec![(reverb, -40.), (delay, -40.)]),
        (drums, vec![(reverb, -24.), (delay, -40.)]),
    ] {
        let sends = sends.into_iter().map(|(target, level_db)| Send { target, level_db, pre_fader: false }).collect();
        b.run(Command::SetTrack { id: t, change: TrackProp::Sends(sends) });
    }
    for t in [bass, synths] {
        b.run(Command::SetTrack { id: t, change: TrackProp::Vca(Some(vca)) });
    }

    // ── Devices ──
    b.drum_rack(drums);
    b.device(drums, "fx.compressor", "Glue Comp", Some("Bus/Drum Bus Glue"), &[]);
    b.device(drums, "fx.saturator", "Saturator", Some("Warmth/Gentle Glue"), &[]);

    b.device(bass, "instrument.wavetable", "Bass Synth", Some("Bass/House Organ Bass"), &[]);
    b.device(bass, "fx.eq", "EQ", Some("Bass/Bass Focus"), &[]);
    b.device(bass, "fx.compressor", "Compressor", Some("Bass/Bass Control"), &[]);

    b.device(lead, "instrument.wavetable", "Lead Synth", Some("Lead/Supersaw Lead"), &[]);
    b.device(lead, "fx.delay", "Delay", Some("Synced/Dotted Eighth"), &[]);
    let pad_synth = b.device(pad, "instrument.wavetable", "Pad Synth", Some("Pad/Warm Pad"), &[]);
    b.device(pad, "fx.chorus", "Chorus", Some("Chorus/Wide Ensemble"), &[]);
    b.device(pad, "fx.eq", "EQ", Some("Mixing/Pad Cleanup"), &[]);
    b.device(vocals, "fx.eq", "EQ", Some("Vocals/Vocal Air"), &[]);
    b.device(vocals, "fx.compressor", "Compressor", Some("Vocals/Vocal Leveler"), &[]);
    // Returns run fully wet.
    b.device(reverb, "fx.reverb", "Hall", Some("Halls/Big Hall"), &[("Mix", 1.0)]);
    b.device(delay, "fx.delay", "Ping Pong", Some("Synced/Eighth Ping-Pong"), &[("Mix", 1.0)]);
    let main = b.p.main_track().id;
    b.device(main, "fx.eq", "EQ", Some("Mastering/Master Polish"), &[]);
    b.device(main, "fx.limiter", "Limiter", Some("Mastering/Transparent"), &[]);

    // ── Clips ──
    let q = PPQ / 4; // a sixteenth
    let mut beat_a = Vec::new();
    for i in 0..4 {
        beat_a.push((36, i * PPQ, q, 0.95));
        beat_a.push((42, i * PPQ + 2 * q, q, 0.7));
    }
    beat_a.extend([(39, PPQ, q, 0.85), (39, 3 * PPQ, q, 0.85)]);
    let mut beat_b = beat_a.clone();
    for i in 0..16 {
        if i % 2 == 1 {
            beat_b.push((42, i * q, q, 0.45));
        }
    }
    beat_b.retain(|n| !(n.0 == 42 && n.1 % PPQ == 2 * q));
    beat_b.extend([(46, 2 * q, q, 0.6), (46, PPQ + 2 * q, q, 0.6), (46, 2 * PPQ + 2 * q, q, 0.6), (46, 3 * PPQ + 2 * q, q, 0.6)]);
    beat_b.extend([(37, 3 * q, q, 0.5), (37, 2 * PPQ + 3 * q, q, 0.5), (38, 3 * PPQ + 3 * q, q, 0.7)]);
    let beat_a = b.notes("Beat A", 0, &beat_a, 4 * BAR, Some(BAR));
    let beat_b = b.notes("Beat B", 1, &beat_b, 4 * BAR, Some(BAR));

    // Am – F – C – G, one bar each.
    let roots = [45u8, 41, 48, 43];
    let mut bass_drop = Vec::new();
    let mut bass_intro = Vec::new();
    for (bar, root) in roots.iter().enumerate() {
        let at = bar as Tick * BAR;
        bass_intro.push((*root, at, BAR - q, 0.8));
        for beat in 0..4 {
            bass_drop.push((*root, at + beat * PPQ + PPQ / 2, PPQ / 2 - q / 2, 0.85));
        }
        bass_drop.push((*root - 12, at, PPQ / 4, 0.7));
    }
    let bass_intro = b.notes("Bass Intro", 7, &bass_intro, 4 * BAR, None);
    let bass_drop = b.notes("Bass Drop", 7, &bass_drop, 4 * BAR, None);

    let hook: &[(u8, i64, i64)] = &[
        (76, 0, 2), (74, 2, 2), (72, 4, 3), (71, 7, 1), (72, 8, 2), (69, 10, 4), (67, 14, 2),
        (69, 16, 2), (72, 18, 2), (74, 20, 3), (76, 23, 1), (77, 24, 2), (76, 26, 2), (74, 28, 2), (71, 30, 2),
    ];
    let hook: Vec<_> = hook.iter().map(|&(k, s, l)| (k, s * PPQ / 2, l * PPQ / 2 - q / 2, 0.8)).collect();
    let hook = b.notes("Hook", 10, &hook, 4 * BAR, None);

    let chords: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [55, 60, 64], [55, 59, 62]];
    let mut pad_notes = Vec::new();
    for (bar, chord) in chords.iter().enumerate() {
        for k in chord {
            pad_notes.push((*k, bar as Tick * BAR, BAR, 0.6));
        }
    }
    let chords = b.notes("Chords", 8, &pad_notes, 4 * BAR, None);

    let vox_src = b.p.alloc_source_id();
    b.run(Command::AddSource {
        id: vox_src,
        source: Box::new(Source::Audio(AudioSource {
            path: PathBuf::from("Samples/Vocal Phrase Night.wav"),
            hash: None,
            sample_rate: 48_000,
            channels: 2,
            frames: 48_000 * 15,
            warp: Warp { enabled: true, original_bpm: Some(128.), ..Warp::default() },
            key: Some(Scale::new(Key(9), ScaleKind::Minor)),
        })),
    });
    let vox = b.clip("Vox Phrase", 3, vox_src, 8 * BAR, None);

    // ── Session ──
    for (scene, clip) in [(0, beat_a), (1, beat_a), (2, beat_b), (3, beat_b), (5, beat_b), (6, beat_a)] {
        b.slot(drums, scene, clip);
    }
    for (scene, clip) in [(0, bass_intro), (1, bass_intro), (3, bass_drop), (5, bass_drop)] {
        b.slot(bass, scene, clip);
    }
    for scene in [2, 3, 5] {
        b.slot(lead, scene, hook);
    }
    for scene in [0, 1, 4, 6] {
        b.slot(pad, scene, chords);
    }
    for scene in [1, 4] {
        b.slot(vocals, scene, vox);
    }

    // ── Arrangement ──
    b.place(drums, beat_a, &[8, 12]);
    b.place(drums, beat_b, &[20, 24, 28]);
    b.place(bass, bass_intro, &[0, 4]);
    b.place(bass, bass_drop, &[8, 12, 20, 24, 28]);
    b.place(lead, hook, &[8, 12, 24, 28]);
    b.place(pad, chords, &[0, 4, 16, 20]);
    b.place(vocals, vox, &[4, 16]);

    // ── Automation ──
    let cutoff = aitrack_stock::instrument::wavetable::p::CUTOFF;
    b.automate(pad, AutomationTarget::DeviceParam { device: pad_synth, param: cutoff }, &[(0, 0.45), (32, 0.62), (64, 0.8), (72, 0.5)]);
    b.automate(lead, AutomationTarget::Volume, &[(32, 0.6), (48, 0.85), (60, 0.85), (64, 0.5)]);
    b.automate(drums, AutomationTarget::Pan, &[(32, 0.5), (64, 0.5)]);

    // A group and what is inside it read as one block of colour.
    for c in crate::state::group_color_commands(&b.p, synths) {
        b.run(c);
    }

    b.p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_has_content_for_every_view() {
        let p = project();
        assert_eq!(p.tempo.initial_bpm(), 128.);
        assert!(p.content_tracks().count() >= 8);
        assert!(p.tracks.iter().any(|t| !t.session_slots.is_empty()));
        assert!(p.tracks.iter().any(|t| !t.arrangement.is_empty()));
        assert!(p.tracks.iter().any(|t| !t.automation.is_empty()));
        let drum_rack = p.tracks.iter().flat_map(|t| &t.devices).find_map(|d| match &d.kind {
            DeviceKind::Rack(r) if r.kind == RackKind::Drum => Some(r),
            _ => None,
        });
        assert_eq!(drum_rack.map(|r| r.chains.len()), Some(DRUM_PADS.len()));
        // Round-trips through JSON like a saved project.
        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back.tracks.len(), p.tracks.len());
    }

    #[test]
    fn every_demo_device_is_a_real_stock_device() {
        fn check(d: &Device) {
            match &d.kind {
                DeviceKind::Stock { plugin_id } => assert!(aitrack_stock::create(plugin_id).is_some(), "{plugin_id} is not a stock device"),
                DeviceKind::Rack(r) => r.chains.iter().flat_map(|c| &c.devices).for_each(check),
                DeviceKind::Plugin { .. } => panic!("the demo uses stock devices only"),
            }
        }
        project().tracks.iter().flat_map(|t| &t.devices).for_each(check);
    }

    /// The drop section (drums, bass, lead and the pad entering) through
    /// the real stock devices: audible and below 0 dBFS.
    #[test]
    fn demo_renders_music_below_full_scale() {
        let p = project();
        let audio = aitrack_engine::render_offline(&p, &crate::real::plugins::StockFactory, (12 * BAR, 20 * BAR), 48_000., 512).expect("render");
        let mut peak = 0f32;
        let mut sum = 0f64;
        let mut n = 0usize;
        for ch in 0..audio.channels() {
            for &s in audio.channel(ch) {
                assert!(s.is_finite());
                peak = peak.max(s.abs());
                sum += (s as f64) * (s as f64);
                n += 1;
            }
        }
        let rms = (sum / n.max(1) as f64).sqrt();
        eprintln!("demo render: peak {:.2} dBFS, rms {:.1} dBFS", 20. * peak.log10(), 20. * rms.log10());
        assert!(n > 48_000 * 2 * 10, "render too short: {n} samples");
        assert!(rms > 0.01, "demo is (nearly) silent: rms {rms}");
        assert!(peak < 1.0, "demo clips: peak {peak}");
    }
}
