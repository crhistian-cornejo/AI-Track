//! Editing the Session View: the clips in the slots and the scenes.
//!
//! Every change is a `Command` (or a batch of them) and undoes in one step.
//! The views (`views/session.rs`) only call these. `impl AppState` holds the
//! project edits; `impl Workspace` the drops, which also need the toast and
//! a worker thread to read files that are not WAV.

use crate::bridge::BpmTrust;
use crate::state::{AppState, KeyEdit, MainView};
use crate::workspace::{DraggedSample, Workspace};
use aitrack_model::{
    AudioSource, Clip, ClipId, Color, Command, Note, NoteSource, Scene, SceneId, Source, SourceId, Tick, TrackId, TrackKind, Warp, PPQ,
};
use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, px};
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// One bar of 4/4: what a new MIDI clip in a slot lasts.
const NEW_CLIP_LEN: Tick = 4 * PPQ;
/// A clip's length until its file's real length is known.
const UNKNOWN_LEN: Tick = 16 * PPQ;

thread_local! {
    /// The scene row the Session View's cursor is on (UI only, not saved).
    /// The cursor's column is the selected track.
    static CURSOR_SCENE: Cell<Option<SceneId>> = const { Cell::new(None) };
}

/// What a slot drag carries: the slot it started in and the clip in it.
#[derive(Clone, Debug)]
pub struct DraggedSlot {
    pub track: TrackId,
    pub scene: SceneId,
    pub name: String,
    pub color: Color,
}

impl Render for DraggedSlot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::*;
        div()
            .px(px(SP2))
            .h(px(session_row_h()))
            .min_w(px(SESSION_COL_W - SP4))
            .flex()
            .items_center()
            .rounded(px(r_md()))
            .bg(clip_fill(self.color))
            .border_1()
            .border_color(border_strong())
            .shadow_lg()
            .child(crate::widgets::label(self.name.clone(), TEXT_SM, clip_text(self.color)))
    }
}

/// A file's `(sample rate, channels, frames)`.
pub type Facts = (u32, u16, u64);

/// What goes into a slot besides the file.
#[derive(Clone, Debug, Default)]
pub struct SlotSample {
    pub name: String,
    pub key: Option<aitrack_model::Scale>,
    /// Tempo of a loop, and whether warp starts on.
    pub bpm: Option<f64>,
    pub warp: bool,
    /// Semitones to put it in the song's key (0 = as it was recorded).
    pub transpose: f32,
    /// Warp mode to start with; `None` keeps the default one.
    pub mode: Option<aitrack_model::WarpMode>,
}

fn header(path: &Path) -> Option<Facts> {
    let r = hound::WavReader::open(path).ok()?;
    Some((r.spec().sample_rate, r.spec().channels, u64::from(r.duration())))
}

/// Whether `path` looks like audio the engine can play.
pub fn is_audio(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["wav", "wave", "aif", "aiff", "flac", "mp3", "ogg", "oga"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn probe(path: &Path) -> Option<Facts> {
    header(path).or_else(|| {
        let info = aitrack_library::analyze::read_audio_info(path).ok()?;
        (info.frames > 0 && info.sample_rate > 0).then_some((info.sample_rate, info.channels.max(1), info.frames))
    })
}

/// A clip's length in ticks for a file of `frames` at `bpm`, at least a beat.
fn length_of(bpm: f64, (rate, _, frames): Facts) -> Tick {
    if frames > 0 && rate > 0 {
        ((frames as f64 / f64::from(rate) * bpm / 60. * PPQ as f64).round() as Tick).max(PPQ)
    } else {
        UNKNOWN_LEN
    }
}

impl AppState {
    // ── Cursor ──

    /// The scene row of the Session View's cursor (the first scene until one is picked).
    pub fn session_scene(&self) -> Option<SceneId> {
        let scenes = &self.project.scenes;
        CURSOR_SCENE.get().filter(|s| scenes.iter().any(|x| x.id == *s)).or_else(|| scenes.first().map(|s| s.id))
    }

    pub fn set_session_scene(&mut self, scene: SceneId) {
        CURSOR_SCENE.set(Some(scene));
    }

    /// The columns of the Session View, left to right.
    pub fn session_tracks(&self) -> Vec<TrackId> {
        self.project
            .content_tracks()
            .filter(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Audio | TrackKind::Group | TrackKind::Return))
            .map(|t| t.id)
            .collect()
    }

    pub fn slot_clip(&self, track: TrackId, scene: SceneId) -> Option<ClipId> {
        self.project.track(track)?.session_slots.get(&scene).copied()
    }

    /// Put the cursor on a slot; a clip in it is selected for the detail panel.
    pub fn select_slot(&mut self, track: TrackId, scene: SceneId) {
        self.selected_track = Some(track);
        self.set_session_scene(scene);
        if let Some(clip) = self.slot_clip(track, scene) {
            self.select_clip(Some(clip));
        }
    }

    /// Arrow keys in the Session View move the cursor over the slots.
    /// `false` when it did not move (or the key is not an arrow).
    pub fn session_key(&mut self, e: KeyEdit) -> bool {
        if self.view != MainView::Session {
            return false;
        }
        let tracks = self.session_tracks();
        let scenes: Vec<SceneId> = self.project.scenes.iter().map(|s| s.id).collect();
        let (Some(scene), false) = (self.session_scene(), tracks.is_empty()) else { return false };
        let col = self.selected_track.and_then(|t| tracks.iter().position(|x| *x == t)).unwrap_or(0);
        let row = scenes.iter().position(|s| *s == scene).unwrap_or(0);
        let (col, row) = match e {
            KeyEdit::Left => (col.saturating_sub(1), row),
            KeyEdit::Right => ((col + 1).min(tracks.len() - 1), row),
            KeyEdit::Up => (col, row.saturating_sub(1)),
            KeyEdit::Down => (col, (row + 1).min(scenes.len() - 1)),
            _ => return false,
        };
        if self.selected_track == Some(tracks[col]) && scenes[row] == scene {
            return false;
        }
        self.select_slot(tracks[col], scenes[row]);
        true
    }

    // ── Clips in slots ──

    /// Whether a clip over `source` can sit on `track`: notes on instrument
    /// tracks, audio on audio tracks.
    fn slot_accepts(&self, track: TrackId, source: SourceId) -> bool {
        let Some(t) = self.project.track(track) else { return false };
        match self.project.sources.get(&source) {
            Some(Source::Notes(_)) => t.kind == TrackKind::Instrument,
            Some(Source::Audio(_)) => t.kind == TrackKind::Audio,
            None => false,
        }
    }

    /// An empty one-bar MIDI clip in an empty slot of an instrument track.
    pub fn create_midi_clip_in_slot(&mut self, track: TrackId, scene: SceneId) -> Option<ClipId> {
        let t = self.project.track(track).filter(|t| t.kind == TrackKind::Instrument)?;
        if t.session_slots.contains_key(&scene) || !self.project.scenes.iter().any(|s| s.id == scene) {
            return None;
        }
        let color = t.color;
        let source = self.project.alloc_source_id();
        let clip = self.project.alloc_clip_id();
        self.seal();
        let ok = self.edit(
            "Create MIDI clip",
            vec![
                Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource::default())) },
                Command::AddClip { id: clip, clip: Box::new(Clip::new("MIDI Clip", color, source, NEW_CLIP_LEN)) },
                Command::SetSessionSlot { track, scene, clip: Some(clip) },
            ],
        );
        self.seal();
        if ok {
            self.select_slot(track, scene);
        }
        ok.then_some(clip)
    }

    /// An independent copy of `clip`: audio shares its file, notes get their own.
    fn copy_clip_commands(&mut self, clip: ClipId) -> Option<(ClipId, Vec<Command>)> {
        let mut copy = self.project.clips.get(&clip)?.clone();
        let mut cmds = Vec::new();
        if let Some(Source::Notes(src)) = self.project.sources.get(&copy.source).cloned() {
            let id = self.project.alloc_source_id();
            let notes = src.notes.into_iter().map(|n| Note { id: self.project.alloc_note_id(), ..n }).collect();
            cmds.push(Command::AddSource { id, source: Box::new(Source::Notes(NoteSource { notes })) });
            copy.source = id;
        }
        let id = self.project.alloc_clip_id();
        cmds.push(Command::AddClip { id, clip: Box::new(copy) });
        Some((id, cmds))
    }

    /// Move the clip of slot `from` to slot `to` (what was there is replaced).
    /// One undo step. `false` when the clip does not fit that track.
    pub fn move_slot(&mut self, from: (TrackId, SceneId), to: (TrackId, SceneId)) -> bool {
        let Some(clip) = self.slot_clip(from.0, from.1).filter(|_| from != to) else { return false };
        let Some(source) = self.project.clips.get(&clip).map(|c| c.source) else { return false };
        if !self.slot_accepts(to.0, source) || !self.project.scenes.iter().any(|s| s.id == to.1) {
            return false;
        }
        self.seal();
        let ok = self.edit(
            "Move clip",
            vec![
                Command::SetSessionSlot { track: from.0, scene: from.1, clip: None },
                Command::SetSessionSlot { track: to.0, scene: to.1, clip: Some(clip) },
            ],
        );
        self.seal();
        if ok {
            self.select_slot(to.0, to.1);
        }
        ok
    }

    /// Copy the clip of slot `from` into slot `to`: an independent clip.
    pub fn copy_slot(&mut self, from: (TrackId, SceneId), to: (TrackId, SceneId)) -> bool {
        let Some(clip) = self.slot_clip(from.0, from.1) else { return false };
        let Some(source) = self.project.clips.get(&clip).map(|c| c.source) else { return false };
        if from == to || !self.slot_accepts(to.0, source) || !self.project.scenes.iter().any(|s| s.id == to.1) {
            return false;
        }
        let Some((id, mut cmds)) = self.copy_clip_commands(clip) else { return false };
        cmds.push(Command::SetSessionSlot { track: to.0, scene: to.1, clip: Some(id) });
        self.seal();
        let ok = self.edit("Copy clip", cmds);
        self.seal();
        if ok {
            self.select_slot(to.0, to.1);
        }
        ok
    }

    /// Empty a slot (the clip stays in the project).
    pub fn clear_slot(&mut self, track: TrackId, scene: SceneId) -> bool {
        if self.slot_clip(track, scene).is_none() {
            return false;
        }
        self.seal();
        let ok = self.edit("Clear slot", vec![Command::SetSessionSlot { track, scene, clip: None }]);
        self.seal();
        ok
    }

    /// The commands that put the audio file `path` in a slot of an audio
    /// track, as a clip as long as the file. `None` when the track is not an
    /// audio track or the scene is gone.
    pub fn audio_slot_commands(&mut self, track: TrackId, scene: SceneId, path: &Path, facts: Facts, sample: &SlotSample) -> Option<Vec<Command>> {
        let t = self.project.track(track).filter(|t| t.kind == TrackKind::Audio)?;
        if !self.project.scenes.iter().any(|s| s.id == scene) {
            return None;
        }
        let color = t.color;
        let name = if sample.name.is_empty() { path.file_stem().map_or_else(|| "Audio".into(), |n| n.to_string_lossy().into_owned()) } else { sample.name.clone() };
        let source = self.project.alloc_source_id();
        let clip = self.project.alloc_clip_id();
        let length = length_of(self.project.tempo.initial_bpm(), facts);
        let warp = Warp { enabled: sample.warp && sample.bpm.is_some(), original_bpm: sample.bpm, transpose: sample.transpose, mode: sample.mode.unwrap_or(Warp::default().mode), ..Warp::default() };
        Some(vec![
            Command::AddSource {
                id: source,
                source: Box::new(Source::Audio(AudioSource { path: path.to_path_buf(), hash: None, sample_rate: facts.0, channels: facts.1, frames: facts.2, warp, key: sample.key })),
            },
            Command::AddClip { id: clip, clip: Box::new(Clip::new(&name, color, source, length)) },
            Command::SetSessionSlot { track, scene, clip: Some(clip) },
        ])
    }

    /// Put an audio file in a slot: one undo step.
    pub fn put_audio_in_slot(&mut self, track: TrackId, scene: SceneId, path: &Path, facts: Facts, sample: &SlotSample) -> bool {
        let Some(cmds) = self.audio_slot_commands(track, scene, path, facts, sample) else { return false };
        self.seal();
        let ok = self.edit("Add sample", cmds);
        self.seal();
        if ok {
            self.select_slot(track, scene);
        }
        ok
    }

    // ── Scenes ──

    /// A new scene right below `after` (or at the end), named with the next number.
    pub fn add_scene(&mut self, after: Option<SceneId>) -> Option<SceneId> {
        let index = after.and_then(|a| self.project.scenes.iter().position(|s| s.id == a)).map_or(self.project.scenes.len(), |i| i + 1);
        let number = self.project.scenes.iter().filter_map(|s| s.name.trim().parse::<u32>().ok()).max().unwrap_or(0).max(self.project.scenes.len() as u32) + 1;
        let id = self.project.alloc_scene_id();
        self.seal();
        let ok = self.edit("Add scene", vec![Command::AddScene { scene: Scene { id, name: number.to_string(), tempo: None }, index }]);
        self.seal();
        if ok {
            self.set_session_scene(id);
        }
        ok.then_some(id)
    }

    /// Delete a scene and empty its slots. The last scene stays.
    pub fn remove_scene(&mut self, scene: SceneId) -> bool {
        if self.project.scenes.len() <= 1 || !self.project.scenes.iter().any(|s| s.id == scene) {
            return false;
        }
        let at = self.project.scenes.iter().position(|s| s.id == scene).unwrap_or(0);
        self.seal();
        let ok = self.edit("Delete scene", vec![Command::RemoveScene { id: scene }]);
        self.seal();
        if ok {
            let near = self.project.scenes.get(at).or_else(|| self.project.scenes.last()).map(|s| s.id);
            if let Some(near) = near {
                self.set_session_scene(near);
            }
        }
        ok
    }

    /// A copy of a scene right below it, with independent copies of its clips.
    pub fn duplicate_scene(&mut self, scene: SceneId) -> Option<SceneId> {
        let at = self.project.scenes.iter().position(|s| s.id == scene)?;
        let old = self.project.scenes[at].clone();
        let id = self.project.alloc_scene_id();
        let mut cmds = vec![Command::AddScene { scene: Scene { id, name: format!("{} copy", old.name), tempo: old.tempo }, index: at + 1 }];
        let slots: Vec<(TrackId, ClipId)> = self.project.tracks.iter().filter_map(|t| t.session_slots.get(&scene).map(|c| (t.id, *c))).collect();
        for (track, clip) in slots {
            let Some((copy, mut made)) = self.copy_clip_commands(clip) else { continue };
            cmds.append(&mut made);
            cmds.push(Command::SetSessionSlot { track, scene: id, clip: Some(copy) });
        }
        self.seal();
        let ok = self.edit("Duplicate scene", cmds);
        self.seal();
        if ok {
            self.set_session_scene(id);
        }
        ok.then_some(id)
    }

    pub fn rename_scene(&mut self, scene: SceneId, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.project.scenes.iter().find(|s| s.id == scene).is_none_or(|s| s.name == name) {
            return false;
        }
        self.seal();
        let ok = self.edit("Rename scene", vec![Command::RenameScene { id: scene, name: name.to_owned() }]);
        self.seal();
        ok
    }
}

impl Workspace {
    /// Enter in the Session View: launch the clip under the cursor (an empty
    /// slot stops its track, as in Live).
    pub fn launch_session_cursor(&mut self, cx: &mut Context<Self>) {
        let (Some(track), Some(scene)) = (self.s.selected_track.filter(|t| self.s.session_tracks().contains(t)), self.s.session_scene()) else {
            if let Some(scene) = self.s.session_scene() {
                self.b.engine.launch_scene(scene);
            }
            return;
        };
        if self.s.slot_clip(track, scene).is_some() {
            self.b.engine.launch_clip(track, scene);
        } else {
            self.b.engine.stop_track(track);
        }
        cx.notify();
    }

    /// A library sample dropped on a slot.
    pub fn drop_sample_in_slot(&mut self, d: &DraggedSample, track: TrackId, scene: SceneId, cx: &mut Context<Self>) {
        let item = &d.0;
        let Some(facts) = probe(&item.path) else {
            self.toast(format!("Could not read {}", item.name), cx);
            return;
        };
        let trust = if item.is_loop && item.bpm.is_some() { self.b.library.bpm_trust(item.id) } else { BpmTrust::Reliable };
        // Dragged out of the Sounds panel with "fit to song" on: it lands in
        // the song's key and tempo, like the drop into the arrangement.
        let fit = self.drop_fit(item, trust);
        let sample = SlotSample {
            name: item.name.clone(),
            key: item.key,
            bpm: match &fit {
                Some(f) => f.original_bpm,
                None => item.is_loop.then(|| item.bpm.map(f64::from)).flatten(),
            },
            warp: fit.as_ref().map_or_else(|| trust.allows_auto_warp(), |f| f.warp),
            transpose: fit.as_ref().map_or(0., |f| f.transpose),
            mode: fit.as_ref().map(|f| f.mode),
        };
        if self.s.put_audio_in_slot(track, scene, &item.path, facts, &sample) {
            self.b.library.mark_used(item.id);
        } else {
            self.toast("Samples go on audio tracks", cx);
        }
        cx.notify();
    }

    /// Audio files dropped from Finder or Explorer on a slot: the first one in
    /// it, the next ones in the slots below, one undo step for all.
    pub fn drop_files_in_slot(&mut self, paths: Vec<PathBuf>, track: TrackId, scene: SceneId, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = paths.into_iter().filter(|p| is_audio(p)).collect();
        if files.is_empty() {
            self.toast("Drop audio files: WAV, AIFF, FLAC, MP3 or OGG", cx);
            return;
        }
        if self.s.project.track(track).is_none_or(|t| t.kind != TrackKind::Audio) {
            self.toast("Audio files go on audio tracks", cx);
            return;
        }
        let task = cx.background_executor().spawn(async move { files.into_iter().map(|f| (probe(&f), f)).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let probed = task.await;
            let _ = this.update(cx, |ws, cx| {
                let n = ws.s.put_audio_files(track, scene, probed);
                if n == 0 {
                    ws.toast("Could not add those files", cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A slot dragged onto another: moved, or copied with Alt/Option held.
    pub fn drop_slot(&mut self, d: &DraggedSlot, to: (TrackId, SceneId), copy: bool, cx: &mut Context<Self>) {
        let from = (d.track, d.scene);
        let ok = if copy { self.s.copy_slot(from, to) } else { self.s.move_slot(from, to) };
        if !ok && from != to {
            self.toast("That clip does not fit this track: MIDI on instrument tracks, audio on audio tracks", cx);
        }
        cx.notify();
    }
}

impl AppState {
    /// `files` in the slots from `scene` down on `track`, as one edit; files
    /// that could not be read are skipped, and so are those past the last scene.
    /// Returns how many went in.
    pub fn put_audio_files(&mut self, track: TrackId, scene: SceneId, files: Vec<(Option<Facts>, PathBuf)>) -> usize {
        let first = self.project.scenes.iter().position(|s| s.id == scene).unwrap_or(0);
        let scenes: Vec<SceneId> = self.project.scenes.iter().skip(first).map(|s| s.id).collect();
        let mut cmds = Vec::new();
        let mut n = 0;
        for ((facts, path), scene) in files.into_iter().filter(|(f, _)| f.is_some()).zip(scenes) {
            let Some(facts) = facts else { continue };
            if let Some(mut one) = self.audio_slot_commands(track, scene, &path, facts, &SlotSample::default()) {
                cmds.append(&mut one);
                n += 1;
            }
        }
        if n == 0 {
            return 0;
        }
        self.seal();
        let ok = self.edit(if n == 1 { "Add sample" } else { "Add samples" }, cmds);
        self.seal();
        if ok {
            self.select_slot(track, scene);
        }
        if ok { n } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::Project;

    fn state() -> AppState {
        AppState::new(Project::new("Session"))
    }

    fn steps(s: &AppState) -> usize {
        s.history.steps().count()
    }

    fn wav(s: &mut AppState, track: TrackId, scene: SceneId, name: &str) -> bool {
        s.put_audio_in_slot(track, scene, Path::new(name), (48_000, 2, 96_000), &SlotSample::default())
    }

    #[test]
    fn a_4x4_session_is_built_from_scratch_one_undo_step_per_action() {
        let mut s = state();
        let midi: Vec<TrackId> = (0..2).map(|_| s.add_track(TrackKind::Instrument).unwrap()).collect();
        let audio: Vec<TrackId> = (0..2).map(|_| s.add_track(TrackKind::Audio).unwrap()).collect();
        s.seal();
        while s.project.scenes.len() > 4 {
            let last = s.project.scenes.last().unwrap().id;
            assert!(s.remove_scene(last));
        }
        let scenes: Vec<SceneId> = s.project.scenes.iter().map(|x| x.id).collect();
        let before = steps(&s);
        for (i, &scene) in scenes.iter().enumerate() {
            for &t in &midi {
                assert!(s.create_midi_clip_in_slot(t, scene).is_some());
            }
            for (k, &t) in audio.iter().enumerate() {
                assert!(wav(&mut s, t, scene, &format!("loop-{i}-{k}.wav")));
            }
        }
        assert_eq!(steps(&s), before + 16, "one undo step per slot");
        let tracks = s.session_tracks();
        assert_eq!(tracks.len(), 4);
        assert!(tracks.iter().all(|&t| scenes.iter().all(|&sc| s.slot_clip(t, sc).is_some())), "4x4 filled");
        // Audio clips are as long as their file: 2 s at 120 bpm = 4 beats.
        let clip = s.slot_clip(audio[0], scenes[0]).unwrap();
        assert_eq!(s.project.clips[&clip].length, 4 * PPQ);
        assert!(s.undo());
        assert!(s.slot_clip(audio[1], scenes[3]).is_none());
        // A filled slot, or the wrong kind of track, takes no new MIDI clip.
        assert!(s.create_midi_clip_in_slot(midi[0], scenes[0]).is_none());
        assert!(s.create_midi_clip_in_slot(audio[0], scenes[0]).is_none());
    }

    #[test]
    fn moving_and_copying_clips_between_slots() {
        let mut s = state();
        let (a, b) = (s.add_track(TrackKind::Instrument).unwrap(), s.add_track(TrackKind::Instrument).unwrap());
        let au = s.add_track(TrackKind::Audio).unwrap();
        s.seal();
        let sc: Vec<SceneId> = s.project.scenes.iter().map(|x| x.id).collect();
        let clip = s.create_midi_clip_in_slot(a, sc[0]).unwrap();
        let source = s.project.clips[&clip].source;
        let id = s.project.alloc_note_id();
        s.edit("note", vec![Command::AddNotes { source, notes: vec![Note::new(id, 60, 0, PPQ, 0.8)] }]);
        s.seal();

        let n = steps(&s);
        assert!(s.copy_slot((a, sc[0]), (b, sc[1])));
        assert_eq!(steps(&s), n + 1);
        let copy = s.slot_clip(b, sc[1]).unwrap();
        assert_ne!(copy, clip);
        assert_ne!(s.project.clips[&copy].source, source, "the copy has its own notes");
        assert!(matches!(&s.project.sources[&s.project.clips[&copy].source], Source::Notes(n) if n.notes.len() == 1));
        assert_eq!(s.slot_clip(a, sc[0]), Some(clip), "the original stays");

        let n = steps(&s);
        assert!(s.move_slot((a, sc[0]), (a, sc[2])));
        assert_eq!(steps(&s), n + 1);
        assert_eq!((s.slot_clip(a, sc[0]), s.slot_clip(a, sc[2])), (None, Some(clip)));
        assert!(s.undo());
        assert_eq!((s.slot_clip(a, sc[0]), s.slot_clip(a, sc[2])), (Some(clip), None));

        // MIDI does not go on an audio track, and onto itself is no move.
        assert!(!s.move_slot((a, sc[0]), (au, sc[0])));
        assert!(!s.copy_slot((a, sc[0]), (au, sc[0])));
        assert!(!s.move_slot((a, sc[0]), (a, sc[0])));
    }

    #[test]
    fn scenes_are_added_renamed_duplicated_and_deleted_in_single_steps() {
        let mut s = state();
        let t = s.add_track(TrackKind::Instrument).unwrap();
        s.seal();
        let first = s.project.scenes[0].id;
        let clip = s.create_midi_clip_in_slot(t, first).unwrap();
        let count = s.project.scenes.len();

        let n = steps(&s);
        let added = s.add_scene(Some(first)).unwrap();
        assert_eq!((steps(&s), s.project.scenes.len(), s.project.scenes[1].id), (n + 1, count + 1, added), "right below the one given");
        assert!(s.rename_scene(added, "Drop"));
        assert!(!s.rename_scene(added, "  "), "empty names are refused");
        assert_eq!(s.project.scenes[1].name, "Drop");

        let n = steps(&s);
        let dup = s.duplicate_scene(first).unwrap();
        assert_eq!(steps(&s), n + 1);
        assert_eq!(s.project.scenes[1].id, dup);
        let copy = s.slot_clip(t, dup).unwrap();
        assert_ne!(copy, clip);
        assert!(s.undo());
        assert!(s.project.scenes.iter().all(|x| x.id != dup));

        assert!(s.remove_scene(first));
        assert!(s.slot_clip(t, first).is_none());
        assert!(s.undo());
        assert_eq!(s.slot_clip(t, first), Some(clip), "undo brings the slot back");

        // The last scene stays.
        while s.project.scenes.len() > 1 {
            let id = s.project.scenes[0].id;
            assert!(s.remove_scene(id));
        }
        let only = s.project.scenes[0].id;
        assert!(!s.remove_scene(only));
    }

    #[test]
    fn dropped_files_fill_consecutive_scenes_in_one_step() {
        let mut s = state();
        let au = s.add_track(TrackKind::Audio).unwrap();
        s.seal();
        let sc: Vec<SceneId> = s.project.scenes.iter().map(|x| x.id).collect();
        let n = steps(&s);
        let files = vec![(Some((44_100, 2, 44_100)), PathBuf::from("a.wav")), (None, PathBuf::from("broken.wav")), (Some((44_100, 1, 88_200)), PathBuf::from("b.wav"))];
        assert_eq!(s.put_audio_files(au, sc[2], files), 2);
        assert_eq!(steps(&s), n + 1);
        assert!(s.slot_clip(au, sc[2]).is_some() && s.slot_clip(au, sc[3]).is_some());
        assert!(s.slot_clip(au, sc[4]).is_none());
        assert!(s.undo());
        assert!(s.slot_clip(au, sc[2]).is_none());
        // Past the last scene nothing is lost or invented.
        let last = *sc.last().unwrap();
        let two = vec![(Some((48_000, 2, 48_000)), PathBuf::from("x.wav")), (Some((48_000, 2, 48_000)), PathBuf::from("y.wav"))];
        assert_eq!(s.put_audio_files(au, last, two), 1);
        // A MIDI track takes no audio.
        let mid = s.add_track(TrackKind::Instrument).unwrap();
        assert!(!wav(&mut s, mid, sc[0], "z.wav"));
    }

    #[test]
    fn arrow_keys_move_the_cursor_over_the_slots() {
        let mut s = state();
        let a = s.add_track(TrackKind::Instrument).unwrap();
        let b = s.add_track(TrackKind::Audio).unwrap();
        s.seal();
        s.view = MainView::Session;
        let sc: Vec<SceneId> = s.project.scenes.iter().map(|x| x.id).collect();
        s.select_slot(a, sc[0]);
        assert!(s.session_key(KeyEdit::Right));
        assert_eq!(s.selected_track, Some(b));
        assert!(!s.session_key(KeyEdit::Right), "at the last column");
        assert!(s.session_key(KeyEdit::Down) && s.session_key(KeyEdit::Down));
        assert_eq!(s.session_scene(), Some(sc[2]));
        assert!(s.session_key(KeyEdit::Up));
        assert!(s.session_key(KeyEdit::Left));
        assert_eq!((s.selected_track, s.session_scene()), (Some(a), Some(sc[1])));
        assert!(!s.session_key(KeyEdit::Longer), "not an arrow");
        s.view = MainView::Arrangement;
        assert!(!s.session_key(KeyEdit::Down), "arrows belong to the arrangement there");
    }
}
