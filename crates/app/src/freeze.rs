//! Freeze, Unfreeze and Flatten (Live's track menu). Freezing renders a
//! track's clips and whole device chain offline (the export renderer, off
//! the UI thread, with progress) to a WAV; only when it is done does
//! `Command::FreezeTrack` apply, as one undo step. Unfreeze and Flatten are
//! commands too. While a track renders it can be cancelled; nothing in the
//! project changes until the file is ready.

use crate::state::AppState;
use crate::workspace::Workspace;
use aitrack_model::*;
use gpui::Context;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A render in flight.
pub struct FreezeJob {
    pub track: TrackId,
    /// 0..1 as `f32` bits, written by the render thread.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
}

/// The renders running now (view state of the Workspace).
#[derive(Default)]
pub struct FreezeUi {
    pub jobs: Vec<FreezeJob>,
}

impl FreezeUi {
    /// How far `track`'s render has got, while it runs.
    pub fn progress(&self, track: TrackId) -> Option<f32> {
        self.jobs.iter().find(|j| j.track == track).map(|j| f32::from_bits(j.progress.load(Ordering::Relaxed)).clamp(0., 1.))
    }

    pub fn is_running(&self, track: TrackId) -> bool {
        self.jobs.iter().any(|j| j.track == track)
    }
}

/// Where frozen audio goes: `Samples/Frozen` next to the saved project, else
/// the app's cache folder (it can be rendered again, so it is not kept in the
/// music folder).
pub fn freeze_folder(project_path: Option<&Path>) -> PathBuf {
    match project_path.and_then(Path::parent) {
        Some(dir) => dir.join(aitrack_model::paths::SAMPLES_DIR).join("Frozen"),
        None => crate::appearance::config_dir().join("cache").join("frozen"),
    }
}

/// A `.wav` path in `dir` named after the track that no file (nor `taken`) has yet.
pub fn fresh_frozen(dir: &Path, name: &str, taken: &[&Path]) -> PathBuf {
    let base = crate::project_io::safe_name(name);
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{base} Frozen.wav") } else { format!("{base} Frozen {n}.wav") }))
        .find(|p| !p.exists() && !taken.contains(&p.as_path()))
        .expect("a free name")
}

/// What a freeze reads: the same track, clips, sources and devices as when it started?
fn same_inputs(a: &Project, b: &Project, track: TrackId) -> bool {
    let (Some(ta), Some(tb)) = (a.track(track), b.track(track)) else { return false };
    let device_lane = |l: &&AutomationLane| matches!(l.target, AutomationTarget::DeviceParam { .. } | AutomationTarget::DeviceEnabled { .. } | AutomationTarget::Macro { .. });
    let clips_same = ta.arrangement.iter().all(|pl| {
        let (ca, cb) = (a.clips.get(&pl.clip), b.clips.get(&pl.clip));
        ca == cb && ca.is_some_and(|c| a.sources.get(&c.source) == b.sources.get(&c.source))
    });
    ta.kind == tb.kind
        && ta.devices == tb.devices
        && ta.arrangement == tb.arrangement
        && ta.automation.iter().filter(device_lane).eq(tb.automation.iter().filter(device_lane))
        && a.tempo == b.tempo
        && clips_same
}

impl AppState {
    /// The tracks of `ids` Freeze can act on: clip-holding, not frozen yet.
    pub fn freezable(&self, ids: &[TrackId]) -> Vec<TrackId> {
        ids.iter().copied().filter(|&id| self.project.track(id).is_some_and(|t| t.can_freeze() && !t.is_frozen())).collect()
    }

    /// The frozen tracks among `ids`.
    pub fn frozen_among(&self, ids: &[TrackId]) -> Vec<TrackId> {
        ids.iter().copied().filter(|&id| self.project.track(id).is_some_and(Track::is_frozen)).collect()
    }

    /// Put a finished render on `track` (one undo step). `started` is the
    /// project the render was made from: if what the render reads changed
    /// meanwhile, the file is stale and nothing applies.
    pub fn finish_freeze(&mut self, track: TrackId, file: AudioSource, started: &Project) -> Result<(), &'static str> {
        if !same_inputs(started, &self.project, track) {
            return Err("The track changed while it was being frozen: freeze it again");
        }
        self.seal();
        let ok = self.edit("Freeze track", vec![Command::FreezeTrack { track, file }]);
        self.seal();
        if ok { Ok(()) } else { Err("Could not freeze the track") }
    }

    /// Unfreeze the frozen tracks among `ids`: one undo step. How many.
    pub fn unfreeze_tracks(&mut self, ids: &[TrackId]) -> usize {
        let frozen = self.frozen_among(ids);
        if frozen.is_empty() {
            return 0;
        }
        let commands = frozen.iter().map(|&track| Command::UnfreezeTrack { track }).collect();
        self.seal();
        let ok = self.edit(if frozen.len() == 1 { "Unfreeze track" } else { "Unfreeze tracks" }, commands);
        self.seal();
        if ok { frozen.len() } else { 0 }
    }

    /// Flatten the frozen tracks among `ids` into audio tracks holding their
    /// render: one undo step. How many.
    pub fn flatten_tracks(&mut self, ids: &[TrackId]) -> usize {
        let frozen = self.frozen_among(ids);
        if frozen.is_empty() {
            return 0;
        }
        let commands = frozen
            .iter()
            .map(|&track| Command::FlattenTrack { track, source: self.project.alloc_source_id(), clip: self.project.alloc_clip_id(), placement: self.project.alloc_placement_id() })
            .collect();
        self.seal();
        let ok = self.edit(if frozen.len() == 1 { "Flatten track" } else { "Flatten tracks" }, commands);
        self.seal();
        if ok { frozen.len() } else { 0 }
    }
}

impl Workspace {
    /// Freeze the selected tracks: each one renders in the background and
    /// freezes when its file is ready.
    pub fn freeze_selected(&mut self, cx: &mut Context<Self>) {
        let ids = self.s.freezable(&self.s.selected_tracks());
        if ids.is_empty() {
            self.toast("Select a MIDI or audio track to freeze", cx);
            return;
        }
        for id in ids {
            self.start_freeze(id, cx);
        }
    }

    pub fn unfreeze_selected(&mut self, cx: &mut Context<Self>) {
        let n = self.s.unfreeze_tracks(&self.s.selected_tracks());
        self.toast(if n == 0 { "No frozen track selected" } else { "Unfrozen: the devices run live again" }, cx);
        cx.notify();
    }

    pub fn flatten_selected(&mut self, cx: &mut Context<Self>) {
        let n = self.s.flatten_tracks(&self.s.selected_tracks());
        self.toast(if n == 0 { "Freeze a track first: Flatten turns its render into audio" } else { "Flattened: the track is audio now" }, cx);
        cx.notify();
    }

    /// Stop the render of `track` (nothing changes in the project).
    pub fn cancel_freeze(&mut self, track: TrackId, cx: &mut Context<Self>) {
        if let Some(j) = self.freeze.jobs.iter().find(|j| j.track == track) {
            j.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    fn start_freeze(&mut self, track: TrackId, cx: &mut Context<Self>) {
        if self.freeze.is_running(track) {
            return;
        }
        // What was tweaked in the plugin windows and not saved yet is in the
        // render: the offline instances are built from the project's states.
        let project = self.s.export_snapshot(&mut *self.b.engine);
        let Some(name) = project.track(track).map(|t| t.name.clone()) else { return };
        let taken: Vec<PathBuf> = project.tracks.iter().filter_map(|t| t.freeze.as_ref().map(|f| f.file.path.clone())).collect();
        let taken: Vec<&Path> = taken.iter().map(PathBuf::as_path).collect();
        let path = fresh_frozen(&freeze_folder(self.project_path.as_deref()), &name, &taken);
        let rate = project.sample_rate_hint.max(8_000);
        let factory = self.b.engine.export_factory();
        let (progress, cancel) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        self.freeze.jobs.push(FreezeJob { track, progress: progress.clone(), cancel: cancel.clone() });
        let started = project.clone();
        let job_cancel = cancel.clone();
        let task = cx.background_executor().spawn(async move {
            let mut report = |f: f32| {
                progress.store(f.to_bits(), Ordering::Relaxed);
                !job_cancel.load(Ordering::Relaxed)
            };
            aitrack_engine::freeze_track_to_wav(&project, track, rate, &path, None, &*factory, Some(&mut report)).map_err(|e| e.to_string())
        });
        // Repaint while it renders, for the progress bar.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let alive = this
                    .update(cx, |ws, cx| {
                        let running = ws.freeze.is_running(track);
                        if running {
                            cx.notify();
                        }
                        running
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        let toast_name = name.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.freeze.jobs.retain(|j| j.track != track);
                match result {
                    Ok(file) => {
                        let path = file.path.clone();
                        match ws.s.finish_freeze(track, file, &started) {
                            Ok(()) => ws.toast(format!("Froze {toast_name}"), cx),
                            Err(why) => {
                                let _ = std::fs::remove_file(&path);
                                ws.toast(why, cx);
                            }
                        }
                    }
                    Err(e) if cancel.load(Ordering::Relaxed) || e.contains("cancelled") => ws.toast("Freeze cancelled", cx),
                    Err(e) => ws.toast(format!("Freeze failed: {e}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        self.toast(format!("Freezing {name}…"), cx);
        cx.notify();
    }

    /// Dev view `frozen`: the first instrument or audio track shown as frozen
    /// without rendering (a file that is not on disk, so a real engine plays it live).
    pub fn fake_freeze(&mut self) {
        let Some(track) = self.s.project.tracks.iter().find(|t| t.can_freeze() && !t.is_frozen() && !t.arrangement.is_empty()).map(|t| t.id) else { return };
        let end = aitrack_engine::freeze_end(&self.s.project, track);
        let bpm = self.s.project.tempo.initial_bpm();
        let frames = (end as f64 / PPQ as f64 * 60.0 / bpm * 48_000.0) as u64;
        let file = AudioSource { path: "/frozen-preview/Frozen.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames, warp: Warp::default(), key: None };
        self.s.selected_track = Some(track);
        let _ = self.s.edit("Freeze track", vec![Command::FreezeTrack { track, file }]);
        self.s.seal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (AppState, TrackId, TrackId) {
        let s = AppState::new(crate::demo::project());
        let track = s.project.tracks.iter().find(|t| t.can_freeze() && !t.arrangement.is_empty()).map(|t| t.id).expect("a track with clips");
        let empty = s.project.main_track().id;
        (s, track, empty)
    }

    fn file() -> AudioSource {
        AudioSource { path: "/tmp/Frozen.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 96_000, warp: Warp::default(), key: None }
    }

    #[test]
    fn freeze_unfreeze_and_flatten_are_one_undo_step_each() {
        let (mut s, track, main) = project();
        assert_eq!(s.freezable(&[track, main]), vec![track]);
        let started = s.project.clone();
        s.finish_freeze(track, file(), &started).unwrap();
        assert!(s.project.track(track).unwrap().is_frozen());
        assert!(s.freezable(&[track]).is_empty());
        let frozen = s.project.clone();

        // Clip and device edits are refused while frozen; the mixer is not.
        assert!(s.edit("mix", vec![Command::SetTrack { id: track, change: TrackProp::VolumeDb(-3.0) }]));
        assert!(!s.edit("clip", vec![Command::RemovePlacement { track, id: s.project.track(track).unwrap().arrangement[0].id }]));
        assert!(s.last_error.as_deref().is_some_and(|e| e.contains("frozen")), "{:?}", s.last_error);
        s.history.undo(&mut s.project).unwrap();

        assert_eq!(s.unfreeze_tracks(&[track, main]), 1);
        assert!(!s.project.track(track).unwrap().is_frozen());
        s.history.undo(&mut s.project).unwrap();
        assert_eq!(s.project, frozen);

        assert_eq!(s.flatten_tracks(&[track]), 1);
        let t = s.project.track(track).unwrap();
        assert_eq!((t.kind, t.is_frozen(), t.devices.len(), t.arrangement.len()), (TrackKind::Audio, false, 0, 1));
        s.history.undo(&mut s.project).unwrap();
        // (The id allocator moved on; everything else is back.)
        assert_eq!((&s.project.tracks, &s.project.clips, &s.project.sources), (&frozen.tracks, &frozen.clips, &frozen.sources));
        assert_eq!(s.flatten_tracks(&[main]), 0, "only frozen tracks flatten");
    }

    #[test]
    fn a_track_that_changed_while_rendering_is_not_frozen() {
        let (mut s, track, _) = project();
        let started = s.project.clone();
        let id = s.project.track(track).unwrap().arrangement[0].id;
        assert!(s.edit("delete", vec![Command::RemovePlacement { track, id }]));
        assert!(s.finish_freeze(track, file(), &started).is_err());
        assert!(!s.project.track(track).unwrap().is_frozen());
        // The fader moving meanwhile does not matter.
        let started = s.project.clone();
        assert!(s.edit("mix", vec![Command::SetTrack { id: track, change: TrackProp::VolumeDb(-9.0) }]));
        assert!(s.finish_freeze(track, file(), &started).is_ok());
    }

    #[test]
    fn frozen_files_go_next_to_the_project_or_in_the_cache() {
        let dir = std::env::temp_dir().join(format!("aitrack-frozen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(freeze_folder(Some(&dir.join("Song.aitrack"))), dir.join("Samples").join("Frozen"));
        assert!(freeze_folder(None).ends_with("frozen"));
        let first = fresh_frozen(&dir, "Lead: 1/2", &[]);
        assert_eq!(first.file_name().unwrap().to_string_lossy(), "Lead- 1-2 Frozen.wav");
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(fresh_frozen(&dir, "Lead: 1/2", &[]).file_name().unwrap().to_string_lossy(), "Lead- 1-2 Frozen 2.wav");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
