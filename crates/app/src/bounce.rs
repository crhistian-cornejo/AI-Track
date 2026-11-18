//! Bounce (Live 12.2 / 12.3): render tracks to audio and put the audio where
//! it is wanted.
//!
//! * **Bounce to New Track** renders the time selection (or the whole of each
//!   selected track) through the track's devices into a new audio track
//!   below it, and mutes the source, as Live does. A Group is rendered as
//!   the sum of its members.
//! * **Bounce Track in Place** is Freeze plus Flatten in one step: the
//!   track's clips and devices are replaced by the render; undo brings the
//!   original back.
//! * **Paste Bounced Audio** renders what was copied (or the time selection)
//!   and pastes the audio at the insert marker.
//!
//! The render is the offline export renderer on a worker thread with
//! progress (the track header's bar, as in Freeze); nothing in the project
//! changes until the file is ready, and then the result is ONE undo step.
//! The faders and sends of the source are not baked in: a new track takes
//! them over from the source, so it sounds as the source did.

use crate::editing::Plan;
use crate::freeze::{FreezeJob, same_inputs};
use crate::state::AppState;
use crate::workspace::Workspace;
use aitrack_model::*;
use gpui::Context;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// Where the audio of a bounce goes.
#[derive(Clone, Debug, PartialEq)]
pub enum BounceMode {
    /// A new audio track below the source; the source is muted.
    NewTrack,
    /// The source track itself becomes audio.
    InPlace,
    /// Pasted at `at`: onto `onto` when it is an audio track, else a new track.
    Paste { at: Tick, onto: Option<TrackId> },
}

/// One render: `track` over `range` (`None`: the whole track).
#[derive(Clone, Debug, PartialEq)]
pub struct BounceJob {
    pub track: TrackId,
    pub range: Option<(Tick, Tick)>,
    pub mode: BounceMode,
}

/// What a finished bounce made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounced {
    /// The track that now holds the audio.
    pub track: TrackId,
    pub placement: PlacementId,
    pub length: Tick,
}

/// Where bounced audio goes: `Samples/Bounced` next to the saved project,
/// else the app's cache folder (the same places as frozen audio).
pub fn bounce_folder(project_path: Option<&Path>) -> PathBuf {
    match project_path.and_then(Path::parent) {
        Some(dir) => dir.join(aitrack_model::paths::SAMPLES_DIR).join("Bounced"),
        None => crate::appearance::config_dir().join("cache").join("bounced"),
    }
}

/// A `.wav` path in `dir` named after the track that no file (nor `taken`) has yet.
pub fn fresh_bounce(dir: &Path, name: &str, taken: &[&Path]) -> PathBuf {
    let base = crate::project_io::safe_name(name);
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{base} Bounce.wav") } else { format!("{base} Bounce {n}.wav") }))
        .find(|p| !p.exists() && !taken.contains(&p.as_path()))
        .expect("a free name")
}

/// Does the render of `track` read the same song as when it started? A
/// group also reads its members' faders (they are part of the sum).
fn bounce_same(a: &Project, b: &Project, track: TrackId) -> bool {
    let (ma, mb) = (aitrack_model::groups::descendants(a, track), aitrack_model::groups::descendants(b, track));
    ma == mb
        && same_inputs(a, b, track)
        && ma.iter().all(|&id| {
            same_inputs(a, b, id)
                && matches!((a.track(id), b.track(id)), (Some(x), Some(y)) if (x.volume_db, x.pan, x.mute) == (y.volume_db, y.pan, y.mute))
        })
}

impl AppState {
    /// Whether Bounce can render `id`: an instrument, audio or group track
    /// with something on it.
    fn bounceable(&self, id: TrackId) -> bool {
        let p = &self.project;
        let holds = |t: &Track| t.arrangement.iter().any(|pl| pl.lane == 0);
        match p.track(id) {
            Some(t) if t.kind.has_clips() => holds(t),
            Some(t) if t.kind == TrackKind::Group => aitrack_model::groups::descendants(p, id).iter().filter_map(|d| p.track(*d)).any(|d| d.kind.has_clips() && holds(d)),
            _ => false,
        }
    }

    /// Bounce to New Track: the tracks of the time selection over its range,
    /// else each selected track whole.
    pub fn bounce_new_track_jobs(&self) -> Vec<BounceJob> {
        if let Some(sel) = self.time_selection() {
            let range = Some((sel.start, sel.end));
            return sel.tracks.iter().copied().filter(|t| self.bounceable(*t)).map(|track| BounceJob { track, range, mode: BounceMode::NewTrack }).collect();
        }
        self.selected_tracks().into_iter().filter(|t| self.bounceable(*t)).map(|track| BounceJob { track, range: None, mode: BounceMode::NewTrack }).collect()
    }

    /// Bounce Track in Place: the selected tracks that are not frozen yet
    /// (a frozen one only needs flattening: see [`AppState::flatten_tracks`]).
    pub fn bounce_in_place_jobs(&self) -> Vec<BounceJob> {
        self.freezable(&self.selected_tracks()).into_iter().filter(|t| self.bounceable(*t)).map(|track| BounceJob { track, range: None, mode: BounceMode::InPlace }).collect()
    }

    /// Paste Bounced Audio: what was copied last (its tracks, over its
    /// range), else the time selection, pasted at the insert marker.
    pub fn paste_bounced_jobs(&self) -> Result<Vec<BounceJob>, &'static str> {
        let copied = self.ed.copied.as_ref().filter(|_| matches!(self.ed.clipboard, Some(crate::editing::Clipboard::Clips { .. })));
        let (start, end, tracks) = match (copied, self.time_selection()) {
            (Some(c), _) => (c.start, c.end, c.tracks.clone()),
            (None, Some(sel)) => (sel.start, sel.end, sel.tracks.clone()),
            (None, None) => return Err("Copy clips or a stretch of time first: Paste Bounced Audio pastes them as rendered audio"),
        };
        let tracks: Vec<TrackId> = tracks.into_iter().filter(|t| self.project.track(*t).is_some_and(|t| t.kind.has_clips()) && self.bounceable(*t)).collect();
        if tracks.is_empty() || end <= start {
            return Err("There is nothing to render in what was copied");
        }
        let at = self.ed.marker.max(0);
        // One source can land on the selected audio track; several each get a track.
        let onto = (tracks.len() == 1)
            .then_some(self.selected_track)
            .flatten()
            .filter(|t| self.project.track(*t).is_some_and(|t| t.kind == TrackKind::Audio && !t.is_frozen()));
        Ok(tracks.into_iter().map(|track| BounceJob { track, range: Some((start, end)), mode: BounceMode::Paste { at, onto } }).collect())
    }

    /// Where a new track under `source` goes, and the group it joins.
    fn track_below(&self, source: TrackId) -> (usize, Option<TrackId>) {
        let p = &self.project;
        let at = p.track_index(source).map_or(p.tracks.len(), |i| i + 1);
        let last = aitrack_model::groups::descendants(p, source).iter().filter_map(|d| p.track_index(*d)).max().map_or(at, |i| i + 1);
        (last, p.track(source).and_then(|t| t.parent))
    }

    /// A new audio track below `source`, sounding as the source did: its
    /// fader, pan and sends were not baked into the render.
    fn bounce_track(&mut self, source: TrackId, name: String, take_mix: bool) -> Option<(Track, usize)> {
        let (index, parent) = self.track_below(source);
        let src = self.project.track(source)?.clone();
        let mut t = Track::new(self.project.alloc_track_id(), TrackKind::Audio, name, src.color);
        if take_mix {
            t.volume_db = src.volume_db;
            t.pan = src.pan;
            t.sends = src.sends.clone();
        }
        if let Some(g) = parent.filter(|g| self.project.track(*g).is_some()) {
            t.parent = Some(g);
            t.output = Output::Track(g);
        }
        Some((t, index))
    }

    /// Put a finished render where its job says (one undo step). `started`
    /// is the project the render was made from: if what it reads changed
    /// meanwhile, the file is stale and nothing applies.
    pub fn finish_bounce(&mut self, job: &BounceJob, file: AudioSource, started: &Project) -> Result<Bounced, &'static str> {
        const STALE: &str = "The track changed while it was being bounced: bounce it again";
        if !bounce_same(started, &self.project, job.track) {
            return Err(STALE);
        }
        let Some(src) = self.project.track(job.track).cloned() else { return Err(STALE) };
        let bpm = self.project.tempo.initial_bpm();
        let length = file.length_ticks(bpm).max(1);
        let start = job.range.map_or(0, |r| r.0).max(0);
        let (source, clip, placement) = (self.project.alloc_source_id(), self.project.alloc_clip_id(), self.project.alloc_placement_id());
        let name = format!("{} Bounce", src.name);
        match job.mode {
            BounceMode::InPlace => {
                if !src.can_freeze() || src.is_frozen() {
                    return Err("Only a MIDI or audio track that is not frozen can be bounced in place");
                }
                self.seal();
                let ok = self.edit("Bounce track in place", vec![Command::FreezeTrack { track: job.track, file }, Command::FlattenTrack { track: job.track, source, clip, placement }]);
                self.seal();
                if !ok {
                    return Err("Could not bounce the track");
                }
                let length = self.project.clips.get(&clip).map_or(length, |c| c.length);
                self.selected_track = Some(job.track);
                Ok(Bounced { track: job.track, placement, length })
            }
            BounceMode::NewTrack => {
                let Some((track, index)) = self.bounce_track(job.track, name.clone(), true) else { return Err(STALE) };
                let track_id = track.id;
                let color = self.clip_color_for(track.color);
                let mut cmds = vec![
                    Command::AddTrack { track: Box::new(track), index },
                    Command::AddSource { id: source, source: Box::new(Source::Audio(file)) },
                    Command::AddClip { id: clip, clip: Box::new(Clip::new(name, color, source, length)) },
                    Command::PlaceClip { track: track_id, placement: Placement { id: placement, clip, start, lane: 0 } },
                ];
                // The source is kept, silenced, so the bounce does not play twice.
                if !src.mute {
                    cmds.push(Command::SetTrack { id: job.track, change: TrackProp::Mute(true) });
                }
                self.seal();
                let ok = self.edit("Bounce to new track", cmds);
                self.seal();
                if !ok {
                    return Err("Could not bounce the track");
                }
                self.selected_track = Some(track_id);
                self.select_clips([placement], Some(placement));
                Ok(Bounced { track: track_id, placement, length })
            }
            BounceMode::Paste { at, onto } => {
                let mut plan = Plan::new(self);
                let target = match onto.filter(|t| self.project.track(*t).is_some_and(|t| t.kind == TrackKind::Audio && !t.is_frozen())) {
                    Some(t) => t,
                    None => {
                        let Some((track, index)) = self.bounce_track(job.track, format!("{} Bounce", src.name), false) else { return Err(STALE) };
                        let id = track.id;
                        plan.p.ids = self.project.ids.clone();
                        plan.run(Command::AddTrack { track: Box::new(track), index });
                        id
                    }
                };
                plan.clear(target, at, at + length);
                let color = self.clip_color_for(src.color);
                plan.run(Command::AddSource { id: source, source: Box::new(Source::Audio(file)) });
                plan.run(Command::AddClip { id: clip, clip: Box::new(Clip::new(name, color, source, length)) });
                plan.run(Command::PlaceClip { track: target, placement: Placement { id: placement, clip, start: at, lane: 0 } });
                if !self.commit("Paste bounced audio", plan) {
                    return Err("Could not paste the bounced audio");
                }
                self.selected_track = Some(target);
                self.select_clips([placement], Some(placement));
                self.ed.marker = at + length;
                Ok(Bounced { track: target, placement, length })
            }
        }
    }
}

impl Workspace {
    /// Cmd+B: bounce the time selection, or the selected tracks, to a new audio track.
    pub fn bounce_to_new_track(&mut self, cx: &mut Context<Self>) {
        let jobs = self.s.bounce_new_track_jobs();
        if jobs.is_empty() {
            self.toast("Select a track with clips, or drag over some time, to bounce", cx);
            return;
        }
        for job in jobs {
            self.start_bounce(job, cx);
        }
    }

    /// Bounce Track in Place: the selected tracks become audio.
    pub fn bounce_in_place(&mut self, cx: &mut Context<Self>) {
        let flattened = self.s.flatten_tracks(&self.s.selected_tracks());
        let jobs = self.s.bounce_in_place_jobs();
        if jobs.is_empty() && flattened == 0 {
            self.toast("Select a MIDI or audio track with clips to bounce in place", cx);
            return;
        }
        if flattened > 0 {
            self.toast("The frozen track is audio now", cx);
        }
        for job in jobs {
            self.start_bounce(job, cx);
        }
    }

    /// Paste Bounced Audio.
    pub fn paste_bounced(&mut self, cx: &mut Context<Self>) {
        match self.s.paste_bounced_jobs() {
            Ok(jobs) => {
                for job in jobs {
                    self.start_bounce(job, cx);
                }
            }
            Err(why) => self.toast(why, cx),
        }
    }

    /// Render `job` on a worker thread and apply it when the file is ready.
    pub(crate) fn start_bounce(&mut self, job: BounceJob, cx: &mut Context<Self>) {
        let track = job.track;
        if self.freeze.is_running(track) {
            self.toast("That track is still rendering", cx);
            return;
        }
        // What was tweaked in the plugin windows and not saved yet is in the render.
        let project = self.s.export_snapshot(&mut *self.b.engine);
        let Some(name) = project.track(track).map(|t| t.name.clone()) else { return };
        let mut taken: Vec<PathBuf> = project.tracks.iter().filter_map(|t| t.freeze.as_ref().map(|f| f.file.path.clone())).collect();
        taken.extend(self.freeze.running_paths());
        let taken: Vec<&Path> = taken.iter().map(PathBuf::as_path).collect();
        let path = fresh_bounce(&bounce_folder(self.project_path.as_deref()), &name, &taken);
        let rate = project.sample_rate_hint.max(8_000);
        let factory = self.b.engine.export_factory();
        let (running, progress, cancel) = FreezeJob::new(track, path.clone(), "bouncing");
        self.freeze.jobs.push(running);
        let started = project.clone();
        let (render_range, in_place) = (job.range, job.mode == BounceMode::InPlace);
        let job_cancel = cancel.clone();
        let task = cx.background_executor().spawn(async move {
            let mut report = |f: f32| {
                progress.store(f.to_bits(), Ordering::Relaxed);
                !job_cancel.load(Ordering::Relaxed)
            };
            if in_place {
                aitrack_engine::freeze_track_to_wav(&project, track, rate, &path, None, &*factory, Some(&mut report))
            } else {
                aitrack_engine::bounce_track_to_wav(&project, track, render_range, rate, &path, None, &*factory, Some(&mut report))
            }
            .map_err(|e| e.to_string())
        });
        self.repaint_while_rendering(track, cx);
        let toast_name = name.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.freeze.jobs.retain(|j| j.track != track);
                match result {
                    Ok(file) => {
                        let path = file.path.clone();
                        match ws.s.finish_bounce(&job, file, &started) {
                            Ok(_) => {
                                ws.toast(
                                    match job.mode {
                                        BounceMode::NewTrack => format!("Bounced {toast_name} to a new track; the source is muted"),
                                        BounceMode::InPlace => format!("Bounced {toast_name} in place: undo brings the MIDI and devices back"),
                                        BounceMode::Paste { .. } => format!("Pasted {toast_name} as audio"),
                                    },
                                    cx,
                                );
                            }
                            Err(why) => {
                                let _ = std::fs::remove_file(&path);
                                ws.toast(why, cx);
                            }
                        }
                    }
                    Err(e) if cancel.load(Ordering::Relaxed) || e.contains("cancelled") => ws.toast("Bounce cancelled", cx),
                    Err(e) => ws.toast(format!("Bounce failed: {e}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        self.toast(format!("Bouncing {name}…"), cx);
        cx.notify();
    }
}

impl Workspace {
    /// Dev view `bounced`: the first track with clips bounced to a new track
    /// over its first eight bars, without rendering (a synthetic file that is
    /// written to the cache so the waveform shows).
    pub fn fake_bounce(&mut self) {
        let Some(track) = self.s.project.tracks.iter().find(|t| t.can_freeze() && !t.arrangement.is_empty()).map(|t| t.id) else { return };
        let bpm = self.s.project.tempo.initial_bpm();
        let range = (0, 8 * 4 * PPQ);
        let seconds = (range.1 - range.0) as f64 / PPQ as f64 * 60.0 / bpm;
        let rate = 48_000u32;
        let frames = (seconds * rate as f64) as usize;
        // A pulsing chord-ish tone: a decaying burst on every beat, so the clip reads as audio.
        let mut buf = aitrack_dsp::AudioBuffer::new(2, frames);
        let beat = (60.0 / bpm * rate as f64) as usize;
        for c in 0..2 {
            for (i, s) in buf.channel_mut(c).iter_mut().enumerate() {
                let t = (i % beat.max(1)) as f32 / rate as f32;
                let env = (-t * 9.0).exp();
                let k = i as f32 / rate as f32;
                *s = 0.6 * env * ((k * 220.0 * std::f32::consts::TAU).sin() + 0.5 * (k * 330.0 * std::f32::consts::TAU).sin()) / 1.5;
            }
        }
        let path = bounce_folder(None).join("Dev Bounce.wav");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if aitrack_engine::write_wav(&path, &buf, rate, aitrack_engine::WavFormat::Float32).is_err() {
            return;
        }
        let file = AudioSource { path, hash: None, sample_rate: rate, channels: 2, frames: frames as u64, warp: Warp { enabled: true, original_bpm: Some(bpm), ..Warp::default() }, key: None };
        let started = self.s.project.clone();
        let _ = self.s.finish_bounce(&BounceJob { track, range: Some(range), mode: BounceMode::NewTrack }, file, &started);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editing::TimeSel;

    fn file(frames: u64) -> AudioSource {
        AudioSource { path: "/tmp/Bounce.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames, warp: Warp { enabled: true, original_bpm: Some(120.0), ..Warp::default() }, key: None }
    }

    /// The demo song, with its first clip-holding track selected.
    fn song() -> (AppState, TrackId) {
        let mut s = AppState::new(crate::demo::project());
        let track = s.project.tracks.iter().find(|t| t.can_freeze() && !t.arrangement.is_empty()).map(|t| t.id).expect("a track with clips");
        s.selected_track = Some(track);
        (s, track)
    }

    fn job(track: TrackId, range: Option<(Tick, Tick)>, mode: BounceMode) -> BounceJob {
        BounceJob { track, range, mode }
    }

    #[test]
    fn bounce_to_new_track_adds_a_track_below_mutes_the_source_and_is_one_undo_step() {
        let (mut s, track) = song();
        let before = s.project.clone();
        let started = s.project.clone();
        let source_index = s.project.track_index(track).unwrap();
        // Two seconds of audio at 120 bpm is four beats.
        let done = s.finish_bounce(&job(track, Some((8 * PPQ, 12 * PPQ)), BounceMode::NewTrack), file(96_000), &started).unwrap();
        assert_eq!(s.project.track_index(done.track), Some(source_index + 1), "right below the source");
        let t = s.project.track(done.track).unwrap();
        assert_eq!((t.kind, t.arrangement.len(), t.devices.len()), (TrackKind::Audio, 1, 0));
        assert_eq!(t.arrangement[0].start, 8 * PPQ, "the audio starts where the range does");
        assert_eq!(done.length, 4 * PPQ);
        assert_eq!(s.project.clips[&t.arrangement[0].clip].length, 4 * PPQ);
        let src = s.project.track(track).unwrap();
        assert!(src.mute, "the source is silenced");
        assert_eq!(src.arrangement, before.track(track).unwrap().arrangement, "and untouched");
        assert_eq!((t.volume_db, t.pan), (src.volume_db, src.pan));
        // One undo brings it all back.
        s.history.undo(&mut s.project).unwrap();
        assert_eq!((&s.project.tracks, &s.project.clips, &s.project.sources), (&before.tracks, &before.clips, &before.sources));
    }

    #[test]
    fn a_bounce_of_a_member_stays_in_its_group_and_a_group_bounces_below_its_members() {
        let (mut s, track) = song();
        let group = s.project.alloc_track_id();
        let mut g = Track::new(group, TrackKind::Group, "Bus", Color::palette(2));
        g.output = Output::Main;
        let idx = s.project.track_index(track).unwrap();
        s.project.tracks.insert(idx, g);
        s.project.track_mut(track).unwrap().parent = Some(group);
        s.project.track_mut(track).unwrap().output = Output::Track(group);
        let started = s.project.clone();
        // A member's bounce joins the same group.
        let done = s.finish_bounce(&job(track, None, BounceMode::NewTrack), file(48_000), &started).unwrap();
        assert_eq!(s.project.track(done.track).unwrap().parent, Some(group));
        // The group itself bounces from its members' sum, below the last of them.
        assert!(s.bounceable(group));
        let started = s.project.clone();
        let done = s.finish_bounce(&job(group, None, BounceMode::NewTrack), file(48_000), &started).unwrap();
        let (gi, bi) = (s.project.track_index(group).unwrap(), s.project.track_index(done.track).unwrap());
        let members = aitrack_model::groups::descendants(&s.project, group).iter().filter_map(|d| s.project.track_index(*d)).max().unwrap();
        assert!(bi > members && members > gi, "below every member of the group");
        assert_eq!(s.project.track(done.track).unwrap().parent, None);
    }

    #[test]
    fn bounce_in_place_replaces_the_track_in_one_undo_step_and_undo_restores_it() {
        let (mut s, track) = song();
        let before = s.project.clone();
        let started = s.project.clone();
        let done = s.finish_bounce(&job(track, None, BounceMode::InPlace), file(96_000), &started).unwrap();
        assert_eq!(done.track, track);
        let t = s.project.track(track).unwrap();
        assert_eq!((t.kind, t.is_frozen(), t.devices.len(), t.arrangement.len()), (TrackKind::Audio, false, 0, 1));
        assert!(!t.mute, "the track itself keeps playing");
        s.history.undo(&mut s.project).unwrap();
        assert_eq!((&s.project.tracks, &s.project.clips, &s.project.sources), (&before.tracks, &before.clips, &before.sources));
        // Redo gives the bounce again.
        s.history.redo(&mut s.project).unwrap();
        assert_eq!(s.project.track(track).unwrap().kind, TrackKind::Audio);
    }

    #[test]
    fn paste_bounced_lands_at_the_marker_on_the_selected_audio_track_or_a_new_one() {
        let (mut s, track) = song();
        let audio = s.add_track(TrackKind::Audio).unwrap();
        let started = s.project.clone();
        // Onto the selected audio track.
        let paste = |at, onto| job(track, Some((0, 4 * PPQ)), BounceMode::Paste { at, onto });
        let done = s.finish_bounce(&paste(16 * PPQ, Some(audio)), file(96_000), &started).unwrap();
        assert_eq!(done.track, audio);
        assert_eq!(s.project.track(audio).unwrap().arrangement[0].start, 16 * PPQ);
        assert_eq!(s.ed.marker, 20 * PPQ, "the marker moves to the end, so pasting again continues");
        assert!(!s.project.track(track).unwrap().mute, "paste does not touch the source");
        // Another paste over it replaces what is under it.
        let started = s.project.clone();
        s.finish_bounce(&paste(18 * PPQ, Some(audio)), file(96_000), &started).unwrap();
        let a = s.project.track(audio).unwrap();
        assert!(a.arrangement.iter().all(|pl| pl.start + s.project.clips[&pl.clip].length <= 18 * PPQ || pl.start >= 18 * PPQ));
        // No audio track selected: a new one below the source.
        let n = s.project.tracks.len();
        let started = s.project.clone();
        let done = s.finish_bounce(&paste(0, None), file(48_000), &started).unwrap();
        assert_eq!(s.project.tracks.len(), n + 1);
        assert_ne!(done.track, audio);
        // The paste is one undo step.
        s.history.undo(&mut s.project).unwrap();
        assert_eq!(s.project.tracks.len(), n);
    }

    #[test]
    fn a_track_that_changed_while_rendering_is_not_bounced() {
        let (mut s, track) = song();
        let started = s.project.clone();
        let id = s.project.track(track).unwrap().arrangement[0].id;
        assert!(s.edit("delete", vec![Command::RemovePlacement { track, id }]));
        let before = s.project.clone();
        assert!(s.finish_bounce(&job(track, None, BounceMode::NewTrack), file(48_000), &started).is_err());
        assert_eq!(s.project, before, "nothing applied");
        // The fader moving meanwhile does not matter for a track, but does for a group's members.
        let started = s.project.clone();
        assert!(s.edit("mix", vec![Command::SetTrack { id: track, change: TrackProp::VolumeDb(-9.0) }]));
        assert!(s.finish_bounce(&job(track, None, BounceMode::NewTrack), file(48_000), &started).is_ok());
    }

    #[test]
    fn jobs_follow_the_time_selection_else_the_selected_tracks() {
        let (mut s, track) = song();
        let main = s.project.main_track().id;
        assert_eq!(s.bounce_new_track_jobs(), vec![job(track, None, BounceMode::NewTrack)]);
        s.selected_track = Some(main);
        assert!(s.bounce_new_track_jobs().is_empty(), "Main is not bounced");
        s.ed.time = Some(TimeSel { start: 4 * PPQ, end: 12 * PPQ, tracks: vec![track, main], anchor: (4 * PPQ, track) });
        assert_eq!(s.bounce_new_track_jobs(), vec![job(track, Some((4 * PPQ, 12 * PPQ)), BounceMode::NewTrack)]);
        // Paste Bounced needs a copy or a selection; the marker is where it lands.
        s.ed.marker = 32 * PPQ;
        s.selected_track = Some(track);
        assert_eq!(s.paste_bounced_jobs().unwrap(), vec![job(track, Some((4 * PPQ, 12 * PPQ)), BounceMode::Paste { at: 32 * PPQ, onto: None })]);
        s.ed.time = None;
        assert!(s.paste_bounced_jobs().is_err());
        // In place skips what is frozen already.
        assert_eq!(s.bounce_in_place_jobs().len(), 1);
        let started = s.project.clone();
        s.finish_bounce(&job(track, None, BounceMode::InPlace), file(48_000), &started).unwrap();
        assert!(s.bounce_in_place_jobs().iter().all(|j| j.track != track || s.project.track(track).unwrap().kind == TrackKind::Audio));
    }

    #[test]
    fn bounced_files_go_next_to_the_project_or_in_the_cache_and_never_share_a_name() {
        let dir = std::env::temp_dir().join(format!("aitrack-bounce-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(bounce_folder(Some(&dir.join("Song.aitrack"))), dir.join("Samples").join("Bounced"));
        assert!(bounce_folder(None).ends_with("bounced"));
        let first = fresh_bounce(&dir, "Lead: 1/2", &[]);
        assert_eq!(first.file_name().unwrap().to_string_lossy(), "Lead- 1-2 Bounce.wav");
        assert_eq!(fresh_bounce(&dir, "Lead: 1/2", &[first.as_path()]).file_name().unwrap().to_string_lossy(), "Lead- 1-2 Bounce 2.wav");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
