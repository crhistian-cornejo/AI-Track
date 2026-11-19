//! Warp markers in the sample editor: the handles on the ruler that pin a
//! point of the file to a point in the music.
//!
//! A marker is `(file frame, source tick)`. Dragging one keeps its frame and
//! changes its tick: the audio under it moves to where it is dropped, and
//! the stretches on both sides give way. Adding one pins the audio that is
//! under the pointer now, so nothing moves until it is dragged. Removing one
//! lets the stretch between its neighbours run straight again.
//!
//! The functions here return the file's new [`Warp`]; [`apply`] sets it
//! through `AppState::set_clip_warp`, so a file other clips play too is
//! copied for this clip first and a drag is one undo step.

use crate::state::AppState;
use aitrack_model::{ClipId, PPQ, Tick, Warp};

/// Two markers are at least this far apart in the file, seconds.
const MIN_GAP: f64 = 0.02;
/// The stretch between two markers runs at most this many times faster or
/// slower than the file's tempo (the renderer goes to eight; half of that
/// still sounds like the material).
const MAX_BEND: f64 = 4.0;

/// Source ticks that `frames` of the file last at its own tempo.
fn nominal(warp: &Warp, frames: f64, rate: f64) -> Option<f64> {
    let bpm = warp.original_bpm.filter(|b| *b > 0.0)?;
    (rate > 0.0).then(|| frames / rate * bpm / 60.0 * PPQ as f64)
}

/// `warp` with marker `index` (of [`Warp::anchors`]) moved to source tick
/// `to`: its audio lands there. It stays between its neighbours, no closer
/// than [`MAX_BEND`] allows. `None` when it cannot move or is there already.
pub fn moved(warp: &Warp, index: usize, to: Tick, rate: f64) -> Option<Warp> {
    let mut anchors = warp.anchors();
    let (frame, old) = *anchors.get(index)?;
    let (mut lo, mut hi) = (f64::MIN, f64::MAX);
    if let Some(&(f, t)) = index.checked_sub(1).and_then(|i| anchors.get(i)) {
        let span = nominal(warp, (frame - f) as f64, rate)?;
        lo = lo.max(t as f64 + span / MAX_BEND);
        hi = hi.min(t as f64 + span * MAX_BEND);
    }
    if let Some(&(f, t)) = anchors.get(index + 1) {
        let span = nominal(warp, (f - frame) as f64, rate)?;
        hi = hi.min(t as f64 - span / MAX_BEND);
        lo = lo.max(t as f64 - span * MAX_BEND);
    }
    let (lo, hi) = (lo.ceil() as Tick, hi.floor() as Tick);
    if lo > hi {
        return None;
    }
    let to = to.clamp(lo, hi);
    if to == old {
        return None;
    }
    anchors[index].1 = to;
    Some(Warp { markers: anchors, ..warp.clone() })
}

/// `warp` with a marker at source tick `at`, pinning the audio that plays
/// there now, and the marker's index. A file with no markers yet gets one at
/// each of its ends too, so that dragging the new one bends only what is
/// between them. `None` when the file is not warped, `at` is outside it, or a
/// marker is already there.
pub fn added(warp: &Warp, at: Tick, rate: f64, frames: u64) -> Option<(Warp, usize)> {
    if !warp.enabled || frames == 0 {
        return None;
    }
    let frame = warp.frame_at(at as f64, rate)?.round();
    if frame < 0.0 || frame > frames as f64 {
        return None;
    }
    let frame = frame as u64;
    let mut anchors = warp.anchors();
    if anchors.is_empty() {
        anchors = vec![(0, 0), (frames, warp.tick_at(frames as f64, rate)?.round() as Tick)];
    }
    let gap = (MIN_GAP * rate) as u64;
    if anchors.iter().any(|a| a.0.abs_diff(frame) < gap.max(1) || a.1 == at) {
        return None;
    }
    let index = anchors.iter().position(|a| a.0 > frame).unwrap_or(anchors.len());
    anchors.insert(index, (frame, at));
    if !anchors.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 > w[0].1) {
        return None;
    }
    Some((Warp { markers: anchors, ..warp.clone() }, index))
}

/// `warp` without marker `index`. A warped file keeps two: with fewer there
/// is no line for the audio to follow, and it would jump back to its start.
pub fn removed(warp: &Warp, index: usize) -> Option<Warp> {
    let mut anchors = warp.anchors();
    if anchors.len() <= 2 || index >= anchors.len() {
        return None;
    }
    anchors.remove(index);
    Some(Warp { markers: anchors, ..warp.clone() })
}

/// The tempo the file is taken to have between marker `index` and the next.
pub fn tempo_after(anchors: &[(u64, Tick)], index: usize, rate: f64) -> Option<f64> {
    let (a, b) = (anchors.get(index)?, anchors.get(index + 1)?);
    let seconds = (b.0 - a.0) as f64 / rate;
    (seconds > 0.0).then(|| (b.1 - a.1) as f64 / PPQ as f64 * 60.0 / seconds)
}

/// What a dragged marker reads: the tempo of the stretch on each side of it.
pub fn readout(warp: &Warp, index: usize, rate: f64) -> Option<String> {
    let anchors = warp.anchors();
    let before = index.checked_sub(1).and_then(|i| tempo_after(&anchors, i, rate));
    let after = tempo_after(&anchors, index, rate);
    match (before, after) {
        (Some(a), Some(b)) => Some(format!("{a:.1} BPM ‹ › {b:.1} BPM")),
        (Some(a), None) => Some(format!("{a:.1} BPM ‹")),
        (None, Some(b)) => Some(format!("› {b:.1} BPM")),
        (None, None) => None,
    }
}

/// Set the file of `clip` to `warp`. Moves of one drag coalesce into one undo step.
pub fn apply(s: &mut AppState, clip: ClipId, warp: &Warp) -> bool {
    s.set_clip_warp(&format!("sample-warp-{}", clip.0), clip, |w| *w = warp.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::Source;

    const RATE: f64 = 48_000.;

    /// A file at 120 BPM (24 000 frames a beat) with a marker on beats 0, 2 and 4.
    fn warp() -> Warp {
        Warp { enabled: true, original_bpm: Some(120.0), markers: vec![(0, 0), (48_000, 2 * PPQ), (96_000, 4 * PPQ)], ..Warp::default() }
    }

    #[test]
    fn a_dragged_marker_takes_its_audio_along_and_stays_between_its_neighbours() {
        let w = moved(&warp(), 1, 2 * PPQ + 240, RATE).unwrap();
        assert_eq!(w.markers, vec![(0, 0), (48_000, 2 * PPQ + 240), (96_000, 4 * PPQ)], "the frame stays, its place in the music moves");
        // The audio that was on beat 2 is now a sixteenth later; beat 0 and beat 4 have not moved.
        assert_eq!(w.frame_at((2 * PPQ + 240) as f64, RATE), Some(48_000.0));
        assert_eq!((w.frame_at(0.0, RATE), w.frame_at(4.0 * PPQ as f64, RATE)), (Some(0.0), Some(96_000.0)));
        // Never past a neighbour: at most four times the file's tempo on either side.
        let far = moved(&warp(), 1, 100 * PPQ, RATE).unwrap();
        assert_eq!(far.markers[1].1, 4 * PPQ - 2 * PPQ / 4);
        let near = moved(&warp(), 1, -5 * PPQ, RATE).unwrap();
        assert_eq!(near.markers[1].1, 2 * PPQ / 4);
        assert_eq!(moved(&warp(), 1, 2 * PPQ, RATE), None, "dropped where it was");
        assert_eq!(moved(&warp(), 7, PPQ, RATE), None);
        // The last marker pulls the end of the file with it.
        let end = moved(&warp(), 2, 5 * PPQ, RATE).unwrap();
        assert_eq!(end.markers[2], (96_000, 5 * PPQ));
    }

    #[test]
    fn a_new_marker_pins_the_audio_under_it_without_moving_anything() {
        let (w, index) = added(&warp(), 3 * PPQ, RATE, 120_000).unwrap();
        assert_eq!((index, w.markers[2]), (2, (72_000, 3 * PPQ)));
        for tick in [0, PPQ, 2 * PPQ + 100, 3 * PPQ, 5 * PPQ] {
            assert_eq!(w.frame_at(tick as f64, RATE), warp().frame_at(tick as f64, RATE), "tick {tick} plays the same audio");
        }
        assert_eq!(added(&warp(), 2 * PPQ, RATE, 120_000), None, "there is one there");
        assert_eq!(added(&warp(), 2 * PPQ + 10, RATE, 120_000), None, "too close to one");
        assert_eq!(added(&warp(), 9 * PPQ, RATE, 120_000), None, "past the end of the file");
        assert_eq!(added(&Warp { enabled: false, ..warp() }, 3 * PPQ, RATE, 120_000), None, "warp is off");
        // A file with no markers gets its ends pinned with the new one.
        let plain = Warp { markers: Vec::new(), ..warp() };
        let (w, index) = added(&plain, PPQ, RATE, 120_000).unwrap();
        assert_eq!((index, w.markers.clone()), (1, vec![(0, 0), (24_000, PPQ), (120_000, 5 * PPQ)]));
        assert_eq!(w.frame_at(3.5 * PPQ as f64, RATE), plain.frame_at(3.5 * PPQ as f64, RATE));
    }

    #[test]
    fn a_marker_is_removed_but_never_the_last_two() {
        let w = removed(&warp(), 1).unwrap();
        assert_eq!(w.markers, vec![(0, 0), (96_000, 4 * PPQ)]);
        assert_eq!(removed(&w, 0), None);
        assert_eq!(removed(&warp(), 3), None);
    }

    #[test]
    fn the_read_out_gives_the_tempo_on_each_side() {
        // Beat 2 dragged a quarter of a beat late: 2.25 beats in one second before it, 1.75 after.
        let w = moved(&warp(), 1, 2 * PPQ + PPQ / 4, RATE).unwrap();
        assert_eq!(readout(&w, 1, RATE).as_deref(), Some("135.0 BPM ‹ › 105.0 BPM"));
        assert_eq!(readout(&w, 0, RATE).as_deref(), Some("› 135.0 BPM"));
        assert_eq!(readout(&w, 2, RATE).as_deref(), Some("105.0 BPM ‹"));
    }

    #[test]
    fn a_marker_drag_is_one_undo_step_and_copies_a_shared_file() {
        let mut s = AppState::new(crate::demo::project());
        let clip = *s.project.clips.iter().find(|(_, c)| matches!(s.project.sources.get(&c.source), Some(Source::Audio(_)))).unwrap().0;
        let file = |s: &AppState| match s.project.sources.get(&s.project.clips[&clip].source) {
            Some(Source::Audio(a)) => a.clone(),
            _ => unreachable!(),
        };
        s.set_clip_warp("setup", clip, |w| *w = warp());
        s.seal();
        let before = file(&s);
        let steps = s.history.steps().count();
        // Three moves of one drag.
        for to in [2 * PPQ + 100, 2 * PPQ + 200, 2 * PPQ + 240] {
            let w = moved(&before.warp, 1, to, RATE).unwrap();
            assert!(apply(&mut s, clip, &w));
        }
        s.seal();
        assert_eq!(file(&s).warp.markers[1], (48_000, 2 * PPQ + 240));
        assert_eq!(s.history.steps().count(), steps + 1);
        assert!(s.undo());
        assert_eq!(file(&s), before);
    }
}
