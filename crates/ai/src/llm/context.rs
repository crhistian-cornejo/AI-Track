//! The producer context: what the assistant is told about the open project
//! (key and scale, tempo, time signature, every track with its type, devices,
//! routing and clips) plus the things the user attached to the message.
//!
//! Pure text from the [`Project`]; no paths outside the project, no secrets.

use std::fmt::Write;

use aitrack_model::{ClipId, Device, DeviceKind, Output, Project, Source, Track, TrackId, TrackKind, visit_devices};

/// The role and rules the assistant always gets.
pub const PERSONA: &str = "You are the producer assistant inside AI-Track, a DAW for electronic music. \
Answer like an experienced producer and mix engineer: short, concrete, in the user's language, with exact values \
(Hz, dB, ms, ratios, bars). Refer to tracks and devices by the names below. You cannot change the project yourself: \
edits you suggest are shown to the user as proposals to accept or discard.";

/// The words for a track kind.
pub fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Instrument => "MIDI",
        TrackKind::Audio => "audio",
        TrackKind::Group => "group",
        TrackKind::Folder => "folder",
        TrackKind::Return => "return",
        TrackKind::Vca => "VCA",
        TrackKind::Main => "main",
    }
}

fn track_name(p: &Project, id: TrackId) -> String {
    p.track(id).map_or_else(|| "?".to_owned(), |t| t.name.clone())
}

fn device_label(d: &Device) -> String {
    let what = match &d.kind {
        DeviceKind::Stock { plugin_id } => format!("stock {plugin_id}"),
        DeviceKind::Plugin { format, vendor, .. } => format!("{format:?} by {vendor}").to_lowercase(),
        DeviceKind::Rack(r) => format!("{:?} rack, {} chains", r.kind, r.chains.len()).to_lowercase(),
    };
    let mut s = format!("{} ({what}", d.name);
    if !d.enabled {
        s.push_str(", off");
    }
    if let Some(preset) = &d.preset {
        let _ = write!(s, ", preset {preset}");
    }
    s.push(')');
    s
}

fn bars(p: &Project, ticks: i64) -> f64 {
    let bar = p.tempo.signature_at(0).bar_ticks().max(1);
    ticks as f64 / bar as f64
}

fn routing(p: &Project, t: &Track) -> String {
    let mut parts = Vec::new();
    match t.output {
        Output::Main => {}
        Output::Track(id) => parts.push(format!("out → {}", track_name(p, id))),
        Output::Hardware(n) => parts.push(format!("out → hardware {}", n + 1)),
        Output::None => parts.push("no output".into()),
    }
    if let Some(g) = t.parent {
        parts.push(format!("in group {}", track_name(p, g)));
    }
    if let Some(v) = t.vca {
        parts.push(format!("VCA {}", track_name(p, v)));
    }
    for s in &t.sends {
        parts.push(format!("send {:+.1} dB{} → {}", s.level_db, if s.pre_fader { " pre" } else { "" }, track_name(p, s.target)));
    }
    parts.join(", ")
}

/// One line about a clip: name, length, what it plays.
pub fn describe_clip(p: &Project, id: ClipId) -> Option<String> {
    let c = p.clips.get(&id)?;
    let what = match p.sources.get(&c.source) {
        Some(Source::Notes(n)) => {
            let (lo, hi) = n.notes.iter().fold((u8::MAX, 0u8), |(lo, hi), n| (lo.min(n.key), hi.max(n.key)));
            if n.notes.is_empty() { "MIDI, empty".to_owned() } else { format!("MIDI, {} notes, keys {lo}–{hi}", n.notes.len()) }
        }
        Some(Source::Audio(a)) => {
            let file = a.path.file_name().map_or_else(|| "audio".into(), |f| f.to_string_lossy().into_owned());
            let secs = if a.sample_rate > 0 { a.frames as f64 / a.sample_rate as f64 } else { 0. };
            let key = a.key.map(|k| format!(", key {}", k.display())).unwrap_or_default();
            format!("audio {file}, {secs:.1} s, {} ch{key}, warp {:?}", a.channels, a.warp).to_lowercase()
        }
        None => "missing source".to_owned(),
    };
    Some(format!("\"{}\" ({:.1} bars{}, gain {:+.1} dB): {what}", c.name, bars(p, c.length), if c.looping.is_some() { ", looping" } else { "" }, c.gain_db))
}

/// A track in full: type, level, routing, devices and clips.
pub fn describe_track(p: &Project, t: &Track) -> String {
    let mut s = format!("{} [{}] {:+.1} dB, pan {:+.2}", t.name, kind_name(t.kind), t.volume_db, t.pan);
    for (on, word) in [(t.mute, "muted"), (t.solo, "solo"), (t.arm, "armed")] {
        if on {
            let _ = write!(s, ", {word}");
        }
    }
    let r = routing(p, t);
    if !r.is_empty() {
        let _ = write!(s, "; {r}");
    }
    let mut devices = Vec::new();
    visit_devices(&t.devices, &mut |d| devices.push(device_label(d)));
    let _ = write!(s, "\n    devices: {}", if devices.is_empty() { "none".to_owned() } else { devices.join(" → ") });
    if !t.arrangement.is_empty() {
        let start = t.arrangement.iter().map(|pl| pl.start).min().unwrap_or(0);
        let end = t.arrangement.iter().filter_map(|pl| p.clips.get(&pl.clip).map(|c| pl.start + c.length)).max().unwrap_or(start);
        let mut names: Vec<&str> = Vec::new();
        for pl in &t.arrangement {
            if let Some(c) = p.clips.get(&pl.clip)
                && !names.contains(&c.name.as_str())
            {
                names.push(&c.name);
            }
        }
        let more = names.len().saturating_sub(6);
        names.truncate(6);
        let _ = write!(
            s,
            "\n    arrangement: {} clips, bars {:.0}–{:.0}: {}{}",
            t.arrangement.len(),
            bars(p, start) + 1.,
            bars(p, end) + 1.,
            names.join(", "),
            if more > 0 { format!(" (+{more} more)") } else { String::new() }
        );
    }
    if !t.session_slots.is_empty() {
        let _ = write!(s, "\n    session: {} clip slots filled", t.session_slots.len());
    }
    s
}

/// What the user attached to the message, already described.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Focus {
    /// The selected track, if any (what "this track" means).
    pub selected_track: Option<TrackId>,
    pub selected_clip: Option<ClipId>,
    /// Tracks and clips attached as chips.
    pub tracks: Vec<TrackId>,
    pub clips: Vec<ClipId>,
    /// Samples: one line each (name, category, key, BPM, length).
    pub samples: Vec<String>,
    /// How many screenshots ride along as images.
    pub images: usize,
}

/// The full system prompt.
pub fn producer_context(p: &Project, focus: &Focus) -> String {
    let mut s = String::with_capacity(2048);
    s.push_str(PERSONA);
    s.push_str("\n\n# Project\n");
    let sig = p.tempo.signature_at(0);
    let _ = writeln!(s, "Name: {}", p.name);
    let _ = writeln!(s, "Key/scale: {}", p.scale.map_or_else(|| "not set".to_owned(), |sc| sc.display()));
    let tempo_changes = p.tempo.points().len().saturating_sub(1);
    let _ = writeln!(s, "Tempo: {:.2} BPM{}", p.tempo.initial_bpm(), if tempo_changes > 0 { format!(" ({tempo_changes} tempo changes)") } else { String::new() });
    let _ = writeln!(s, "Time signature: {}/{}", sig.numerator, sig.denominator);
    if let Some((a, b)) = p.loop_region {
        let _ = writeln!(s, "Loop: bars {:.0}–{:.0}", bars(p, a) + 1., bars(p, b) + 1.);
    }
    let _ = writeln!(s, "Scenes: {}", p.scenes.len());
    s.push_str("\n# Tracks (top to bottom)\n");
    for t in &p.tracks {
        let depth = std::iter::successors(t.parent, |g| p.track(*g).and_then(|t| t.parent)).take(8).count();
        let indent = "  ".repeat(depth);
        let _ = writeln!(s, "{indent}- {}", describe_track(p, t).replace("\n    ", &format!("\n{indent}    ")));
    }
    if let Some(t) = focus.selected_track.and_then(|id| p.track(id)) {
        let _ = writeln!(s, "\nSelected track (\"this track\"): {}", t.name);
    }
    if let Some(c) = focus.selected_clip.and_then(|id| describe_clip(p, id)) {
        let _ = writeln!(s, "Selected clip: {c}");
    }
    let attached_tracks: Vec<&Track> = focus.tracks.iter().filter_map(|id| p.track(*id)).collect();
    let attached_clips: Vec<String> = focus.clips.iter().filter_map(|id| describe_clip(p, *id)).collect();
    if !attached_tracks.is_empty() || !attached_clips.is_empty() || !focus.samples.is_empty() || focus.images > 0 {
        s.push_str("\n# Attached by the user\n");
        for t in attached_tracks {
            let _ = writeln!(s, "Track: {}", describe_track(p, t));
        }
        for c in attached_clips {
            let _ = writeln!(s, "Clip: {c}");
        }
        for smp in &focus.samples {
            let _ = writeln!(s, "Sample: {smp}");
        }
        if focus.images > 0 {
            let _ = writeln!(s, "Screenshots: {} (in the message)", focus.images);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Clip, Color, Key, NoteSource, Placement, Scale, ScaleKind, Send};

    fn project() -> (Project, TrackId, ClipId) {
        let mut p = Project::new("Night Drive");
        p.scale = Some(Scale::new(Key(9), ScaleKind::Minor));
        p.tempo.set_initial_bpm(124.);
        let group = p.alloc_track_id();
        let mut g = Track::new(group, TrackKind::Group, "Drums", Color::palette(0));
        g.volume_db = -2.;
        let kick = p.alloc_track_id();
        let mut k = Track::new(kick, TrackKind::Instrument, "Kick", Color::palette(1));
        k.parent = Some(group);
        k.output = Output::Track(group);
        let reverb = p.alloc_track_id();
        let r = Track::new(reverb, TrackKind::Return, "Verb", Color::palette(2));
        k.sends.push(Send { target: reverb, level_db: -12., pre_fader: false });
        let dev = p.alloc_device_id();
        k.devices.push(Device::stock(dev, "fx.eq", "EQ"));
        let src = p.alloc_source_id();
        let mut notes = NoteSource::default();
        for i in 0..4 {
            let id = p.alloc_note_id();
            notes.notes.push(aitrack_model::Note::new(id, 36, i * 960, 480, 0.9));
        }
        p.sources.insert(src, Source::Notes(notes));
        let clip = p.alloc_clip_id();
        p.clips.insert(clip, Clip::new("Kick Loop", Color::palette(1), src, 3840));
        let pl = p.alloc_placement_id();
        k.arrangement.push(Placement { id: pl, clip, start: 3840, lane: 0 });
        p.tracks.insert(0, g);
        p.tracks.insert(1, k);
        p.tracks.insert(2, r);
        (p, kick, clip)
    }

    #[test]
    fn context_has_key_tempo_signature_tracks_routing_and_clips() {
        let (p, kick, clip) = project();
        let s = producer_context(&p, &Focus { selected_track: Some(kick), ..Default::default() });
        assert!(s.contains("Key/scale: A minor") || s.contains("Key/scale: A"), "{s}");
        assert!(s.contains("Tempo: 124.00 BPM"), "{s}");
        assert!(s.contains("Time signature: 4/4"), "{s}");
        assert!(s.contains("Drums [group] -2.0 dB"), "{s}");
        assert!(s.contains("  - Kick [MIDI]"), "kick is indented under its group: {s}");
        assert!(s.contains("out → Drums") && s.contains("in group Drums"), "{s}");
        assert!(s.contains("send -12.0 dB → Verb"), "{s}");
        assert!(s.contains("EQ (stock fx.eq)"), "{s}");
        assert!(s.contains("arrangement: 1 clips, bars 2–3: Kick Loop"), "{s}");
        assert!(s.contains("Selected track (\"this track\"): Kick"), "{s}");
        let c = describe_clip(&p, clip).unwrap();
        assert!(c.contains("1.0 bars") && c.contains("4 notes, keys 36–36"), "{c}");
        assert!(!s.contains("# Attached"), "nothing attached");
    }

    #[test]
    fn attachments_are_listed() {
        let (p, kick, clip) = project();
        let f = Focus { tracks: vec![kick], clips: vec![clip], samples: vec!["Vox Chop (vocal, A minor, 124 BPM)".into()], images: 2, ..Default::default() };
        let s = producer_context(&p, &f);
        assert!(s.contains("# Attached by the user"));
        assert!(s.contains("Track: Kick [MIDI]"));
        assert!(s.contains("Clip: \"Kick Loop\""));
        assert!(s.contains("Sample: Vox Chop"));
        assert!(s.contains("Screenshots: 2"));
    }
}
