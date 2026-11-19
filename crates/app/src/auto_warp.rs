//! Auto-warp: find a clip's beats and put its file on the grid.
//!
//! The file is listened to on a background thread ([`aitrack_ai::beats`]):
//! its tempo, its beats and which of them start a bar. From that the clip's
//! file gets its tempo and its warp markers, Warp is switched on, and the
//! clip is moved onto the grid: it starts on the beat nearest to where it
//! started (a clip that started with the file starts on the first downbeat's
//! place in its bar) and still covers the same audio. One undo step. A file
//! other clips play too is copied for this clip first, as a transpose is.

use crate::state::AppState;
use crate::workspace::Workspace;
use aitrack_ai::beats::{AutoWarp, BeatOptions};
use aitrack_model::ops::MIN_CLIP_LEN;
use aitrack_model::{AudioSource, Clip, ClipId, Command, LoopRegion, PPQ, Project, Source, Tick, Warp};
use gpui::Context;

/// Below this share of clear beats the result is a guess, and the toast says so.
const UNSURE: f32 = 0.35;
/// How sure the "one" must be for the clip to be set on the bar; below it the clip is set on the beat.
const BAR_CONFIDENCE: f32 = 0.2;

/// Frame of `file` that source tick `tick` plays under `warp`: by its markers
/// or tempo when warped, else at the project's tempo (an unwarped file plays
/// in real time).
fn frame_of(warp: &Warp, tick: f64, rate: f64, project_bpm: f64) -> f64 {
    let real_time = tick * rate * 60.0 / (project_bpm * PPQ as f64);
    if warp.enabled { warp.frame_at(tick, rate).unwrap_or(real_time) } else { real_time }
}

/// `clip` as it sits once `file` plays by `new` instead of its own warp. Its start
/// goes to the grid position nearest to the audio it started on (`unit` is a
/// bar or a beat, `phase` where the first downbeat is in it); its end stays
/// on the audio it ended on, and never past the file's end or `room` ticks.
/// Fades and envelopes keep their place in the audio. A clip that loops keeps
/// its loop and only follows the file's new tempo.
pub fn aligned(clip: &Clip, file: &AudioSource, new: &Warp, project_bpm: f64, phase: Tick, unit: Tick, room: Tick) -> Clip {
    let old = &file.warp;
    let rate = f64::from(file.sample_rate);
    if clip.length <= 0 || rate <= 0.0 || unit <= 0 || project_bpm <= 0.0 {
        return clip.clone();
    }
    let tick_of = |frame: f64| new.tick_at(frame, rate).unwrap_or(0.0);
    let moved = |tick: Tick| tick_of(frame_of(old, tick as f64, rate, project_bpm)).round() as Tick;
    let mut c = clip.clone();
    if let Some(lp) = clip.looping {
        // The loop and the offset keep the audio they were on.
        c.offset = moved(clip.offset);
        c.looping = Some(LoopRegion { start: moved(lp.start), end: moved(lp.end).max(moved(lp.start) + MIN_CLIP_LEN) });
        return c;
    }
    let phase = phase.rem_euclid(unit);
    let start = tick_of(frame_of(old, clip.offset as f64, rate, project_bpm));
    let offset = (((start - phase as f64) / unit as f64).round() as Tick * unit + phase).max(phase);
    let end_frame = frame_of(old, clip.offset as f64 + clip.length as f64 * old.speed_ratio(), rate, project_bpm).min(file.frames as f64);
    let speed = new.speed_ratio().max(1e-6);
    let length = (((tick_of(end_frame) - offset as f64) / speed).round() as Tick).min(room).max(MIN_CLIP_LEN);
    let k = length as f64 / clip.length as f64;
    let scale = |t: Tick| (t as f64 * k).round() as Tick;
    c.offset = offset;
    c.length = length;
    c.fade_in.length = scale(clip.fade_in.length).min(length);
    c.fade_out.length = scale(clip.fade_out.length).min(length - c.fade_in.length);
    for lane in &mut c.envelopes {
        for pt in &mut lane.points {
            pt.tick = scale(pt.tick);
        }
    }
    c
}

/// The commands that auto-warp `clip` with what was found in its file. `room`
/// is how far the clip may run before the next one on its lane. Empty when
/// the clip is not audio or nothing would change.
pub fn commands(project: &mut Project, clip_id: ClipId, found: &AutoWarp, room: Tick) -> Vec<Command> {
    let Some(clip) = project.clips.get(&clip_id).cloned() else { return Vec::new() };
    let Some(Source::Audio(file)) = project.sources.get(&clip.source).cloned() else { return Vec::new() };
    let mut warp = file.warp.clone();
    warp.enabled = true;
    warp.original_bpm = Some(found.bpm);
    warp.markers = found.markers.clone();
    let bar = project.tempo.signature_at(0).bar_ticks();
    let unit = if found.downbeat_confidence >= BAR_CONFIDENCE { bar } else { PPQ };
    let mut new_clip = aligned(&clip, &file, &warp, project.tempo.initial_bpm(), found.downbeat_tick, unit, room);
    if warp == file.warp && new_clip == clip {
        return Vec::new();
    }
    let shared = project.clips.iter().any(|(id, c)| *id != clip_id && c.source == clip.source);
    let mut cmds = Vec::new();
    if shared {
        let id = project.alloc_source_id();
        cmds.push(Command::AddSource { id, source: Box::new(Source::Audio(AudioSource { warp, ..file })) });
        new_clip.source = id;
    } else {
        cmds.push(Command::SetWarp { source: clip.source, warp: Box::new(warp) });
    }
    if new_clip != clip {
        cmds.push(Command::UpdateClip { id: clip_id, clip: Box::new(new_clip) });
    }
    cmds
}

impl AppState {
    /// Auto-warp `clip` with `found`, as one undo step.
    pub fn apply_auto_warp(&mut self, clip: ClipId, found: &AutoWarp) -> bool {
        let room = crate::workspace::room_after(&self.project, clip, Tick::MAX);
        let cmds = commands(&mut self.project, clip, found, room);
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit("Auto-warp", cmds);
        self.seal();
        ok
    }
}

/// What the status toast says once a clip is auto-warped.
pub fn summary(found: &AutoWarp) -> String {
    let bpm = if (found.bpm - found.bpm.round()).abs() < 0.005 { format!("{:.0}", found.bpm) } else { format!("{:.2}", found.bpm) };
    if found.steady && found.confidence < UNSURE {
        // No clear beat to hold on to: the tempo is the best guess and nothing was bent.
        format!("Auto-warped at {bpm} BPM, but the beats are not clear in this clip: check Seg. BPM by ear")
    } else if found.steady {
        format!("Auto-warped: {bpm} BPM, steady tempo")
    } else {
        format!("Auto-warped: about {bpm} BPM, the tempo moves (up to {:.0} ms): {} warp markers follow it", found.drift_ms, found.markers.len())
    }
}

impl Workspace {
    /// Listen to the file of `clip` for its beats (in the background) and warp it onto the grid.
    pub fn auto_warp_clip(&mut self, clip: ClipId, cx: &mut Context<Self>) {
        let Some(Source::Audio(file)) = self.s.project.clips.get(&clip).and_then(|c| self.s.project.sources.get(&c.source)) else {
            self.toast("Auto-warp works on audio clips", cx);
            return;
        };
        let path = file.path.clone();
        // A tempo the file already has says which octave to count in (70 or 140).
        let opts = BeatOptions { bpm_hint: file.warp.original_bpm.filter(|b| *b > 0.0), meter: u32::from(self.s.project.tempo.signature_at(0).numerator).max(1) };
        let epoch = self.project_epoch;
        self.toast("Auto-warp: finding the beats…", cx);
        let task = cx.background_executor().spawn(async move { aitrack_ai::auto_warp_file(&path, &opts) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |ws, cx| {
                // Another project was opened meanwhile: this is not for it.
                if ws.project_epoch != epoch {
                    return;
                }
                match found {
                    Ok(Some(found)) => {
                        if ws.s.apply_auto_warp(clip, &found) {
                            ws.toast(summary(&found), cx);
                        } else {
                            ws.toast("Auto-warp: the clip is already on these beats", cx);
                        }
                    }
                    Ok(None) => ws.toast("Auto-warp: no steady beat to follow in this clip", cx),
                    Err(e) => ws.toast(format!("Auto-warp: {e}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_clip(s: &AppState) -> ClipId {
        *s.project.clips.iter().find(|(_, c)| matches!(s.project.sources.get(&c.source), Some(Source::Audio(_)))).unwrap().0
    }

    fn file_of(s: &AppState, clip: ClipId) -> AudioSource {
        match s.project.sources.get(&s.project.clips[&clip].source) {
            Some(Source::Audio(a)) => a.clone(),
            _ => unreachable!(),
        }
    }

    /// A steady file at `bpm` whose first downbeat is `lead` seconds in.
    fn steady(file: &AudioSource, bpm: f64, lead: f64) -> AutoWarp {
        let rate = f64::from(file.sample_rate);
        let downbeat_tick = (lead * bpm / 60.0 * PPQ as f64).round() as Tick;
        let beats = ((file.frames as f64 / rate - lead) * bpm / 60.0).floor() as Tick;
        let markers = vec![((lead * rate).round() as u64, downbeat_tick), (((lead + beats as f64 * 60.0 / bpm) * rate).round() as u64, downbeat_tick + beats * PPQ)];
        AutoWarp { bpm, markers, downbeat_tick, steady: true, drift_ms: 0.3, confidence: 1.0, downbeat_confidence: 0.6 }
    }

    #[test]
    fn auto_warp_sets_tempo_markers_and_the_clip_on_the_bar_in_one_undo_step() {
        let mut s = AppState::new(crate::demo::project());
        let clip = audio_clip(&s);
        // Start from an unwarped clip that covers its whole file.
        s.set_clip_warp("setup", clip, |w| {
            *w = Warp::default();
        });
        let file = file_of(&s, clip);
        let project_bpm = s.project.tempo.initial_bpm();
        let whole = (file.frames as f64 / f64::from(file.sample_rate) * project_bpm / 60.0 * PPQ as f64).round() as Tick;
        let c = Clip { offset: 0, length: whole, looping: None, ..s.project.clips[&clip].clone() };
        s.edit("setup", vec![Command::UpdateClip { id: clip, clip: Box::new(c) }]);
        s.seal();
        let before = (s.project.clips[&clip].clone(), file_of(&s, clip));
        let steps = s.history.steps().count();

        // The file is at 100 BPM and its first downbeat is half a beat in.
        let found = steady(&before.1, 100.0, 0.3);
        assert!(s.apply_auto_warp(clip, &found));
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step");
        let (now, warp) = (s.project.clips[&clip].clone(), file_of(&s, clip).warp);
        assert!(warp.enabled);
        assert_eq!((warp.original_bpm, warp.markers.len()), (Some(100.0), 2));
        assert_eq!(now.offset, PPQ / 2, "the clip starts on the first downbeat");
        // It still ends where the file does: the file's length in beats at 100 BPM, less the half beat.
        let file_ticks = (before.1.frames as f64 / f64::from(before.1.sample_rate) * 100.0 / 60.0 * PPQ as f64).round() as Tick;
        assert!((now.length - (file_ticks - PPQ / 2)).abs() <= 2, "{} vs {}", now.length, file_ticks - PPQ / 2);
        assert!(!s.apply_auto_warp(clip, &found), "again changes nothing");
        assert!(s.undo());
        assert_eq!((s.project.clips[&clip].clone(), file_of(&s, clip)), before);
    }

    #[test]
    fn an_unsure_one_sets_the_clip_on_the_beat_and_a_shared_file_is_copied() {
        let mut s = AppState::new(crate::demo::project());
        let clip = audio_clip(&s);
        let source = s.project.clips[&clip].source;
        // A second clip plays the same file.
        let twin = s.project.alloc_clip_id();
        let copy = s.project.clips[&clip].clone();
        s.edit("twin", vec![Command::AddClip { id: twin, clip: Box::new(copy) }]);
        s.seal();
        let twin_before = (s.project.clips[&twin].clone(), file_of(&s, twin));
        let file = file_of(&s, clip);
        // The first downbeat is a beat and a quarter in, and the tracker is not sure it is the "one".
        let lead = 1.25 * 60.0 / 100.0;
        let found = AutoWarp { downbeat_confidence: 0.05, ..steady(&file, 100.0, lead) };
        assert!(s.apply_auto_warp(clip, &found));
        assert_ne!(s.project.clips[&clip].source, source, "the clip got its own copy of the file");
        assert_eq!((s.project.clips[&twin].clone(), file_of(&s, twin)), twin_before, "the other clip is untouched");
        assert_eq!(s.project.clips[&clip].offset.rem_euclid(PPQ), PPQ / 4, "on the beat, wherever in the bar");
    }

    #[test]
    fn a_clip_keeps_the_audio_it_covered() {
        // A file of 8 s at 48 kHz, unwarped in a 120 BPM project; the clip is its third and fourth seconds.
        let file = AudioSource { path: "/x.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 8 * 48_000, warp: Warp::default(), key: None };
        let mut clip = Clip::new("x", aitrack_model::Color(0), aitrack_model::SourceId(1), 4 * PPQ);
        clip.offset = 4 * PPQ;
        clip.fade_in.length = PPQ;
        // Found: 150 BPM from the first frame. Two seconds are five beats there.
        let new = Warp { enabled: true, original_bpm: Some(150.0), markers: vec![(0, 0), (8 * 48_000, 20 * PPQ)], ..Warp::default() };
        let c = aligned(&clip, &file, &new, 120.0, 0, PPQ, Tick::MAX);
        assert_eq!((c.offset, c.length), (5 * PPQ, 5 * PPQ));
        assert_eq!(c.fade_in.length, PPQ * 5 / 4, "the fade covers the same audio");
        // Room on the lane holds the clip back.
        assert_eq!(aligned(&clip, &file, &new, 120.0, 0, PPQ, 3 * PPQ).length, 3 * PPQ);
        // A looping clip keeps its loop on the same audio and its length on the timeline.
        clip.looping = Some(LoopRegion { start: 4 * PPQ, end: 8 * PPQ });
        let c = aligned(&clip, &file, &new, 120.0, 0, PPQ, Tick::MAX);
        assert_eq!((c.looping, c.length), (Some(LoopRegion { start: 5 * PPQ, end: 10 * PPQ }), 4 * PPQ));
    }

    #[test]
    fn the_toast_says_what_was_found() {
        let file = AudioSource { path: "/x.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 8 * 48_000, warp: Warp::default(), key: None };
        assert_eq!(summary(&steady(&file, 128.0, 0.0)), "Auto-warped: 128 BPM, steady tempo");
        let guess = AutoWarp { confidence: 0.2, ..steady(&file, 124.0, 0.0) };
        assert_eq!(summary(&guess), "Auto-warped at 124 BPM, but the beats are not clear in this clip: check Seg. BPM by ear");
        let live = AutoWarp { bpm: 97.36, steady: false, drift_ms: 84.2, ..steady(&file, 97.36, 0.0) };
        assert_eq!(summary(&live), "Auto-warped: about 97.36 BPM, the tempo moves (up to 84 ms): 2 warp markers follow it");
    }
}
