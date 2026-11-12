use aitrack_model::TrackId;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MOUSE: &str = "@mouse";
pub const NOTE: &str = "@note";

pub fn keyboard_note(key: &str, octave: u8) -> Option<u8> {
    let offset = match key {
        "a" => 0, "w" => 1, "s" => 2, "e" => 3, "d" => 4, "f" => 5,
        "t" => 6, "g" => 7, "y" => 8, "h" => 9, "u" => 10, "j" => 11,
        "k" => 12, "o" => 13, "l" => 14, "p" => 15,
        _ => return None,
    };
    let note = (octave as u16 + 1) * 12 + offset;
    (note <= 127).then_some(note as u8)
}

#[derive(Default)]
pub struct Audition {
    held: BTreeMap<String, (TrackId, u8)>,
    pending_off: BTreeSet<(TrackId, u8)>,
}

impl Audition {
    pub fn is_active(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn pressed(&self, track: TrackId, key: u8) -> bool {
        self.held.values().any(|note| *note == (track, key))
    }

    pub fn matches_track(&self, track: Option<TrackId>) -> bool {
        self.held.values().all(|(t, _)| Some(*t) == track)
    }

    pub fn press(
        &mut self,
        source: &str,
        track: TrackId,
        key: u8,
        mut send: impl FnMut(TrackId, u8, Option<u8>) -> bool,
    ) -> bool {
        if self.held.get(source) == Some(&(track, key)) {
            return true;
        }
        self.release(source, &mut send);
        if self.pending_off.contains(&(track, key)) {
            return false;
        }
        if !self.pressed(track, key) && !send(track, key, Some(100)) {
            return false;
        }
        self.held.insert(source.to_owned(), (track, key));
        true
    }

    pub fn release(&mut self, source: &str, mut send: impl FnMut(TrackId, u8, Option<u8>) -> bool) -> bool {
        let Some((track, key)) = self.held.remove(source) else { return false };
        if !self.pressed(track, key) && !send(track, key, None) {
            self.pending_off.insert((track, key));
        }
        true
    }

    pub fn release_all(&mut self, mut send: impl FnMut(TrackId, u8, Option<u8>) -> bool) {
        while let Some(source) = self.held.keys().next().cloned() {
            self.release(&source, &mut send);
        }
    }

    pub fn retry(&mut self, mut send: impl FnMut(TrackId, u8, Option<u8>) -> bool) {
        self.pending_off.retain(|&(track, key)| !send(track, key, None));
    }
}

/// Sample auditions from a result list (the Cmd+K palette): moving onto a
/// row plays it, the same row is not restarted, and a key held down to
/// scroll waits until it settles instead of starting a decode per row.
#[derive(Debug, Default)]
pub struct SampleAudition {
    playing: Option<PathBuf>,
    pending: Option<(PathBuf, Instant)>,
}

impl SampleAudition {
    /// How long the selection must rest before a scrolled-to row plays.
    pub const SETTLE: Duration = Duration::from_millis(90);

    /// The selection moved onto `path`; with `immediate` it plays now.
    pub fn select(&mut self, path: &Path, immediate: bool, now: Instant) -> Option<PathBuf> {
        if self.playing.as_deref() == Some(path) {
            self.pending = None;
            return None;
        }
        if immediate {
            self.pending = None;
            self.playing = Some(path.to_owned());
            return self.playing.clone();
        }
        self.pending = Some((path.to_owned(), now));
        None
    }

    /// Called on a timer: the pending row once it has settled.
    pub fn due(&mut self, now: Instant) -> Option<PathBuf> {
        let (_, at) = self.pending.as_ref()?;
        if now.duration_since(*at) < Self::SETTLE {
            return None;
        }
        let (path, _) = self.pending.take()?;
        self.playing = Some(path.clone());
        Some(path)
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Stopped (palette closed, or a non-sample row selected).
    pub fn stop(&mut self) -> bool {
        self.pending = None;
        self.playing.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_audition_settles_and_dedupes() {
        let mut a = SampleAudition::default();
        let t0 = Instant::now();
        assert_eq!(a.select(Path::new("a"), true, t0), Some(PathBuf::from("a")));
        assert_eq!(a.select(Path::new("a"), true, t0), None, "already playing");
        assert_eq!(a.select(Path::new("b"), false, t0), None);
        assert_eq!(a.select(Path::new("c"), false, t0), None);
        assert_eq!(a.due(t0 + Duration::from_millis(10)), None);
        assert_eq!(a.due(t0 + SampleAudition::SETTLE), Some(PathBuf::from("c")), "only the last row plays");
        assert!(!a.is_pending());
        assert!(a.stop());
        assert!(!a.stop());
    }

    #[test]
    fn keyboard_mapping_respects_octaves_and_midi_range() {
        assert_eq!(keyboard_note("a", 4), Some(60));
        assert_eq!(keyboard_note("w", 4), Some(61));
        assert_eq!(keyboard_note("k", 4), Some(72));
        assert_eq!(keyboard_note("p", 9), None);
        assert_eq!(keyboard_note("b", 4), None);
    }

    #[test]
    fn releases_only_after_last_source_and_keeps_original_track() {
        let mut a = Audition::default();
        let mut events = Vec::new();
        let mut send = |t, k, v| { events.push((t, k, v)); true };
        assert!(a.press("a", TrackId(1), 60, &mut send));
        assert!(a.press("a", TrackId(1), 60, &mut send));
        assert!(a.press(MOUSE, TrackId(1), 60, &mut send));
        a.release("a", &mut send);
        a.release_all(&mut send);
        assert_eq!(events, vec![(TrackId(1), 60, Some(100)), (TrackId(1), 60, None)]);
    }

    #[test]
    fn retries_failed_note_off_before_retriggering() {
        let mut a = Audition::default();
        assert!(a.press("a", TrackId(1), 60, |_, _, _| true));
        a.release("a", |_, _, _| false);
        assert!(!a.press("a", TrackId(1), 60, |_, _, _| true));
        let mut events = Vec::new();
        a.retry(|t, k, v| { events.push((t, k, v)); true });
        assert_eq!(events, vec![(TrackId(1), 60, None)]);
        assert!(a.press("a", TrackId(1), 60, |_, _, _| true));
    }
}
