//! Slice an audio clip into a Drum Rack (Live's "Slice to New MIDI Track",
//! FL's Slicex): a new MIDI track whose Drum Rack has one pad per slice, each
//! pad a Sampler over the same source file with its Start / End set to the
//! slice, plus a MIDI clip that hits the pads in order so the track plays
//! the clip back as it sounds at the project tempo. No audio is copied.
//!
//! This is a pure command builder. Where the cuts fall comes from the mode:
//! on a beat grid or on the warp markers the model knows them; for
//! transients the caller detects them in the decoded file and passes the
//! frames in. The commands undo as one step (they are applied as a batch).
//!
//! Time model (see the engine's `source_frame`): a clip plays its source from
//! source tick `offset`; a source tick is a frame at `bpm * PPQ` per minute,
//! `bpm` being the original tempo for a warped clip and the tempo where the
//! clip starts otherwise. One clip tick is `ratio` source ticks (`ratio` is
//! `Warp::speed_ratio`, 1 unless the clip is re-pitched).

use crate::command::Command;
use crate::ids::{PlacementId, TrackId};
use crate::project::*;
use crate::time::{PPQ, Tick};
use serde_json::json;

/// Most slices (pads) one slicing makes.
pub const MAX_SLICES: usize = 64;
/// The note of the first slice's pad (C1 in Live's naming), the others follow.
pub const FIRST_NOTE: u8 = 36;
/// Plugin id of the pad's device.
pub const SAMPLER_ID: &str = "instrument.sampler";
/// Sampler parameter ids the pads set (`aitrack-stock` `sampler::p`).
const P_MODE: u32 = 0;
const P_START: u32 = 5;
const P_END: u32 = 6;
/// The Sampler's "One-Shot" mode as a normalised value (choice 1 of 0..=2):
/// a pad plays its whole slice whatever the length of the note.
const MODE_ONE_SHOT: f32 = 0.5;
/// Slices closer than this (seconds) to the one before are merged into it.
const MIN_SLICE_SECS: f64 = 0.01;

/// A musical division the clip is cut at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeatDivision {
    Quarter,
    Eighth,
    Sixteenth,
    Bar,
}

impl BeatDivision {
    pub const ALL: [Self; 4] = [Self::Quarter, Self::Eighth, Self::Sixteenth, Self::Bar];

    pub fn label(self) -> &'static str {
        match self {
            Self::Quarter => "1/4",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::Bar => "1 bar",
        }
    }

    /// Length in ticks; a bar follows the time signature.
    pub fn ticks(self, sig: crate::time::TimeSignature) -> Tick {
        match self {
            Self::Quarter => PPQ,
            Self::Eighth => PPQ / 2,
            Self::Sixteenth => PPQ / 4,
            Self::Bar => sig.bar_ticks(),
        }
    }
}

/// Where the clip is cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliceMode {
    /// At the onsets found in the audio; `sensitivity` 0..=1 (0.5 = default).
    Transients { sensitivity: f32 },
    /// At every division of the grid, from the clip's start.
    Beats(BeatDivision),
    /// At the clip's warp markers.
    WarpMarkers,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SliceError {
    #[error("only an audio clip can be sliced")]
    NotAudio,
    #[error("clip not found")]
    NoClip,
    #[error("the length of the audio file is not known")]
    NoLength,
    #[error("this clip has no warp markers")]
    NoWarpMarkers,
    #[error("nothing to slice: no slice longer than a few milliseconds")]
    NothingToSlice,
}

/// What [`slice_to_drum_rack`] builds.
#[derive(Clone, Debug)]
pub struct Sliced {
    /// Apply in this order, as one undo step.
    pub commands: Vec<Command>,
    /// The new MIDI track.
    pub track: TrackId,
    pub slices: usize,
    /// More cuts were found than [`MAX_SLICES`]; the last slice runs to the end.
    pub capped: bool,
}

/// One cut: source frames `start..end` of the file, hit at `tick` of the clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub start: u64,
    pub end: u64,
    pub tick: Tick,
}

/// The slices of the audio clip placed as `placement` on `track`.
/// `transients` are the onset frames of the file (any order), used only by
/// [`SliceMode::Transients`].
pub fn plan_slices(p: &Project, track: TrackId, placement: PlacementId, mode: SliceMode, transients: &[usize]) -> Result<(Vec<Slice>, bool), SliceError> {
    let pl = p.track(track).and_then(|t| t.arrangement.iter().find(|x| x.id == placement)).ok_or(SliceError::NoClip)?;
    let clip = p.clips.get(&pl.clip).ok_or(SliceError::NoClip)?;
    let Some(Source::Audio(a)) = p.sources.get(&clip.source) else { return Err(SliceError::NotAudio) };
    if a.frames == 0 || a.sample_rate == 0 {
        return Err(SliceError::NoLength);
    }
    let warped = a.warp.enabled && a.warp.original_bpm.is_some_and(|b| b > 0.0);
    let bpm = if warped { a.warp.original_bpm.unwrap_or(120.0) } else { p.tempo.bpm_at(pl.start) };
    let ratio = a.warp.speed_ratio().max(1e-6);
    let frames_per_tick = f64::from(a.sample_rate) * 60.0 / (bpm * PPQ as f64);
    let src_frame = |source_tick: f64| (source_tick * frames_per_tick).round().clamp(0.0, a.frames as f64) as u64;
    // Clip tick ↔ source frame.
    let frame_of = |clip_tick: Tick| src_frame(clip.offset as f64 + clip_tick as f64 * ratio);
    let tick_of = |frame: u64| ((frame as f64 / frames_per_tick - clip.offset as f64) / ratio).round() as Tick;
    let (first, last) = (frame_of(0), frame_of(clip.length.max(1)).max(frame_of(0) + 1));
    let min_gap = (f64::from(a.sample_rate) * MIN_SLICE_SECS) as u64;

    // The cut frames after the first, ascending, each with the tick it is hit at.
    let mut cuts: Vec<(u64, Tick)> = match mode {
        SliceMode::Transients { .. } => {
            let mut t: Vec<u64> = transients.iter().map(|&f| f as u64).collect();
            t.sort_unstable();
            t.into_iter().map(|f| (f, tick_of(f))).collect()
        }
        SliceMode::Beats(div) => {
            let step = div.ticks(p.tempo.signature_at(pl.start)).max(1);
            (1..).map(|k| k * step).take_while(|&t| t < clip.length).map(|t| (frame_of(t), t)).collect()
        }
        SliceMode::WarpMarkers => {
            if a.warp.markers.is_empty() {
                return Err(SliceError::NoWarpMarkers);
            }
            let mut m: Vec<(u64, Tick)> = a.warp.markers.iter().map(|&(f, t)| (f, t - clip.offset)).collect();
            m.sort_unstable();
            m
        }
    };
    cuts.retain(|&(f, t)| f > first && f < last && t > 0);
    let mut starts: Vec<(u64, Tick)> = vec![(first, 0)];
    for (f, t) in cuts {
        let prev = starts.last().map_or(first, |s| s.0);
        if f >= prev + min_gap.max(1) && t > starts.last().map_or(0, |s| s.1) {
            starts.push((f, t));
        }
    }
    let capped = starts.len() > MAX_SLICES;
    starts.truncate(MAX_SLICES);
    if last <= first + 1 {
        return Err(SliceError::NothingToSlice);
    }
    let slices = starts.iter().enumerate().map(|(i, &(start, tick))| Slice { start, end: starts.get(i + 1).map_or(last, |n| n.0), tick }).collect();
    Ok((slices, capped))
}

/// The state a Sampler restores to play `path`: its file in `extra`, the rest default.
pub fn sampler_state(path: &std::path::Path) -> Vec<u8> {
    serde_json::to_vec(&json!({ "version": 1, "extra": { "path": path.to_string_lossy(), "sfz_path": null } })).unwrap_or_default()
}

/// The commands that slice clip `placement` of `track` into a new MIDI track
/// with a Drum Rack and a clip that plays the slices back (see the module
/// docs). Allocates the ids it needs from `p`; edits nothing else.
pub fn slice_to_drum_rack(p: &mut Project, track: TrackId, placement: PlacementId, mode: SliceMode, transients: &[usize]) -> Result<Sliced, SliceError> {
    let (slices, capped) = plan_slices(p, track, placement, mode, transients)?;
    let src_track = p.track(track).ok_or(SliceError::NoClip)?;
    let (index, color) = (p.track_index(track).map_or(p.tracks.len(), |i| i + 1), src_track.color);
    let pl = src_track.arrangement.iter().find(|x| x.id == placement).cloned().ok_or(SliceError::NoClip)?;
    let clip = p.clips.get(&pl.clip).cloned().ok_or(SliceError::NoClip)?;
    let Some(Source::Audio(a)) = p.sources.get(&clip.source).cloned() else { return Err(SliceError::NotAudio) };
    let total = a.frames as f32;
    let state = sampler_state(&a.path);

    let mut rack = Rack::new(RackKind::Drum);
    let mut notes = NoteSource::default();
    for (i, s) in slices.iter().enumerate() {
        let key = FIRST_NOTE + i as u8;
        let mut chain = Chain::new(format!("Slice {}", i + 1), Color::palette(i));
        chain.note = Some(key);
        chain.key_range = (key, key);
        // Hitting a slice cuts the one before it, so a slice longer than its
        // place in a faster project does not pile up.
        chain.choke = 1;
        let id = p.alloc_device_id();
        let mut sampler = Device::stock(id, SAMPLER_ID, format!("Slice {}", i + 1));
        sampler.params.insert(P_MODE, MODE_ONE_SHOT);
        sampler.params.insert(P_START, s.start as f32 / total);
        sampler.params.insert(P_END, s.end as f32 / total);
        sampler.state = Some(state.clone());
        chain.devices.push(sampler);
        rack.chains.push(chain);
        let end = slices.get(i + 1).map_or(clip.length, |n| n.tick);
        notes.notes.push(Note::new(p.alloc_note_id(), key, s.tick, (end - s.tick).max(1), 1.0));
    }
    notes.sort();
    let rack_id = p.alloc_device_id();
    let mut rack_device = Device::stock(rack_id, "rack.drum", "Drum Rack");
    rack_device.kind = DeviceKind::Rack(rack);

    let new_track = p.alloc_track_id();
    let mut t = Track::new(new_track, TrackKind::Instrument, format!("{} Slices", clip.name), color);
    t.devices.push(rack_device);
    let (source, new_clip, new_pl) = (p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
    let midi = Clip::new(clip.name.clone(), clip.color, source, clip.length);
    let commands = vec![
        Command::AddTrack { track: Box::new(t), index },
        Command::AddSource { id: source, source: Box::new(Source::Notes(notes)) },
        Command::AddClip { id: new_clip, clip: Box::new(midi) },
        Command::PlaceClip { track: new_track, placement: Placement { id: new_pl, clip: new_clip, start: pl.start, lane: 0 } },
    ];
    Ok(Sliced { commands, track: new_track, slices: slices.len(), capped })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TempoMap;

    const SR: u32 = 48_000;

    /// A project with an audio clip of `bars` bars of a 120 BPM loop (2 s per bar).
    fn project(bars: i64, warp: Warp) -> (Project, TrackId, PlacementId) {
        let mut p = Project::new("t");
        p.tempo = TempoMap::constant(120.0);
        let (t, src, clip, pl) = (p.alloc_track_id(), p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
        Command::AddTrack { track: Box::new(Track::new(t, TrackKind::Audio, "Loop", Color::palette(2))), index: 0 }.apply(&mut p).unwrap();
        let frames = bars as u64 * 2 * SR as u64;
        let a = AudioSource { path: "/tmp/loop.wav".into(), hash: None, sample_rate: SR, channels: 2, frames, warp, key: None };
        Command::AddSource { id: src, source: Box::new(Source::Audio(a)) }.apply(&mut p).unwrap();
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Loop", Color::palette(2), src, bars * 4 * PPQ)) }.apply(&mut p).unwrap();
        Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 8 * PPQ, lane: 0 } }.apply(&mut p).unwrap();
        (p, t, pl)
    }

    #[test]
    fn beat_slices_sit_on_the_grid_and_cover_the_file() {
        let (p, t, pl) = project(1, Warp::default());
        let (s, capped) = plan_slices(&p, t, pl, SliceMode::Beats(BeatDivision::Eighth), &[]).unwrap();
        assert!(!capped);
        assert_eq!(s.len(), 8);
        assert_eq!(s[0], Slice { start: 0, end: 12_000, tick: 0 });
        assert_eq!(s[7].start, 84_000);
        assert_eq!(s[7].end, 96_000, "the last slice ends with the file");
        assert_eq!(s[3].tick, 3 * PPQ / 2);
        assert!(s.windows(2).all(|w| w[0].end == w[1].start));
        let (bars, _) = plan_slices(&p, t, pl, SliceMode::Beats(BeatDivision::Bar), &[]).unwrap();
        assert_eq!(bars.len(), 1);
    }

    #[test]
    fn a_clip_trimmed_by_offset_and_length_slices_only_what_it_plays() {
        let (mut p, t, pl) = project(2, Warp::default());
        let id = p.track(t).unwrap().arrangement[0].clip;
        let c = p.clips.get_mut(&id).unwrap();
        c.offset = 4 * PPQ; // starts at bar 2
        c.length = 2 * PPQ; // two beats
        let (s, _) = plan_slices(&p, t, pl, SliceMode::Beats(BeatDivision::Quarter), &[]).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (96_000, 120_000));
        assert_eq!((s[1].start, s[1].end), (120_000, 144_000));
        assert_eq!(s[1].tick, PPQ);
    }

    #[test]
    fn transients_cut_where_they_are_and_close_ones_merge() {
        let (p, t, pl) = project(1, Warp::default());
        // 0 is the file start (always a slice), 24_100 is 2 ms after 24_000.
        let (s, _) = plan_slices(&p, t, pl, SliceMode::Transients { sensitivity: 0.5 }, &[0, 48_000, 24_000, 24_100]).unwrap();
        assert_eq!(s.iter().map(|x| x.start).collect::<Vec<_>>(), vec![0, 24_000, 48_000]);
        assert_eq!(s[1].tick, PPQ);
        assert_eq!(s[2].end, 96_000);
    }

    #[test]
    fn a_warped_clip_is_cut_on_its_original_tempo() {
        // The file is a 100 BPM loop played at 120: source ticks are at 100 BPM.
        let warp = Warp { enabled: true, original_bpm: Some(100.0), ..Warp::default() };
        let (mut p, t, pl) = project(1, warp);
        // 1 bar at 100 BPM is 2.4 s: make the file that long.
        if let Some(Source::Audio(a)) = p.sources.values_mut().next() {
            a.frames = (2.4 * SR as f64) as u64;
        }
        let (s, _) = plan_slices(&p, t, pl, SliceMode::Beats(BeatDivision::Quarter), &[]).unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s[1].start, (0.6 * SR as f64) as u64, "a beat of the file, not of the project");
        assert_eq!(s[1].tick, PPQ, "hit on the project's beat");
    }

    #[test]
    fn warp_markers_cut_and_without_them_it_is_an_error() {
        let (p, t, pl) = project(1, Warp::default());
        assert_eq!(plan_slices(&p, t, pl, SliceMode::WarpMarkers, &[]).unwrap_err(), SliceError::NoWarpMarkers);
        let warp = Warp { markers: vec![(60_000, 2 * PPQ), (30_000, PPQ)], ..Warp::default() };
        let (p, t, pl) = project(1, warp);
        let (s, _) = plan_slices(&p, t, pl, SliceMode::WarpMarkers, &[]).unwrap();
        assert_eq!(s.iter().map(|x| (x.start, x.tick)).collect::<Vec<_>>(), vec![(0, 0), (30_000, PPQ), (60_000, 2 * PPQ)]);
    }

    #[test]
    fn at_most_sixty_four_slices_and_the_last_runs_to_the_end() {
        let (p, t, pl) = project(8, Warp::default());
        let (s, capped) = plan_slices(&p, t, pl, SliceMode::Beats(BeatDivision::Sixteenth), &[]).unwrap();
        assert!(capped);
        assert_eq!(s.len(), MAX_SLICES);
        assert_eq!(s.last().unwrap().end, 8 * 2 * SR as u64);
    }

    #[test]
    fn only_audio_clips_slice() {
        let (mut p, t, _) = project(1, Warp::default());
        let (m, src, clip, pl) = (p.alloc_track_id(), p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
        Command::AddTrack { track: Box::new(Track::new(m, TrackKind::Instrument, "M", Color::palette(0))), index: 1 }.apply(&mut p).unwrap();
        Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource::default())) }.apply(&mut p).unwrap();
        Command::AddClip { id: clip, clip: Box::new(Clip::new("n", Color::palette(0), src, PPQ)) }.apply(&mut p).unwrap();
        Command::PlaceClip { track: m, placement: Placement { id: pl, clip, start: 0, lane: 0 } }.apply(&mut p).unwrap();
        assert_eq!(plan_slices(&p, m, pl, SliceMode::Beats(BeatDivision::Quarter), &[]).unwrap_err(), SliceError::NotAudio);
        let _ = t;
    }

    #[test]
    fn the_commands_build_a_drum_rack_track_and_undo_in_one_batch() {
        let (mut p, t, pl) = project(1, Warp::default());
        let before = p.clone();
        let sliced = slice_to_drum_rack(&mut p, t, pl, SliceMode::Beats(BeatDivision::Quarter), &[]).unwrap();
        assert_eq!((sliced.slices, sliced.capped), (4, false));
        let batch = Command::Batch { commands: sliced.commands };
        let undo = batch.apply(&mut p).unwrap();
        let track = p.track(sliced.track).unwrap();
        assert_eq!(p.track_index(sliced.track), Some(1), "right under the audio track");
        assert_eq!(track.kind, TrackKind::Instrument);
        let DeviceKind::Rack(r) = &track.devices[0].kind else { panic!("a rack") };
        assert_eq!(r.kind, RackKind::Drum);
        assert_eq!(r.chains.len(), 4);
        let pad = &r.chains[1];
        assert_eq!((pad.note, pad.key_range, pad.choke), (Some(37), (37, 37), 1));
        let s = &pad.devices[0];
        assert_eq!(s.params[&P_START], 24_000.0 / 96_000.0);
        assert_eq!(s.params[&P_END], 48_000.0 / 96_000.0);
        assert!(String::from_utf8(s.state.clone().unwrap()).unwrap().contains("/tmp/loop.wav"));
        // The MIDI clip sits where the audio clip does and hits the pads in order.
        let mp = &track.arrangement[0];
        assert_eq!(mp.start, 8 * PPQ);
        let clip = &p.clips[&mp.clip];
        assert_eq!(clip.length, 4 * PPQ);
        let Some(Source::Notes(n)) = p.sources.get(&clip.source) else { panic!("notes") };
        assert_eq!(n.notes.iter().map(|x| (x.key, x.start)).collect::<Vec<_>>(), vec![(36, 0), (37, PPQ), (38, 2 * PPQ), (39, 3 * PPQ)]);
        undo.apply(&mut p).unwrap();
        assert_eq!(p.tracks, before.tracks);
        assert_eq!(p.clips, before.clips);
        assert_eq!(p.sources, before.sources);
    }
}
