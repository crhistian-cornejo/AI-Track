use aitrack_library::{Analyzer, HydrateEvent, HydrateJob};
use aitrack_model::TrackId;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MOUSE: &str = "@mouse";
pub const NOTE: &str = "@note";

pub fn keyboard_note(key: &str, octave: u8) -> Option<u8> {
    let offset = match key {
        "a" => 0, "w" => 1, "s" => 2, "e" => 3, "d" => 4, "f" => 5,
        "t" => 6, "g" => 7, "y" => 8, "h" => 9, "u" => 10, "j" => 11,
        "k" => 12, "o" => 13, "l" => 14, "p" => 15, ";" => 16, "'" => 17,
        _ => return None,
    };
    let note = (octave as u16 + 1) * 12 + offset;
    (note <= 127).then_some(note as u8)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardControl {
    OctaveDown,
    OctaveUp,
    VelocityDown,
    VelocityUp,
}

pub fn keyboard_control(key: &str) -> Option<KeyboardControl> {
    match key {
        "z" => Some(KeyboardControl::OctaveDown),
        "x" => Some(KeyboardControl::OctaveUp),
        "c" => Some(KeyboardControl::VelocityDown),
        "v" => Some(KeyboardControl::VelocityUp),
        _ => None,
    }
}

#[derive(Default)]
pub struct Audition {
    held: BTreeMap<String, Vec<(TrackId, u8)>>,
    pending_off: BTreeSet<(TrackId, u8)>,
}

impl Audition {
    pub fn is_active(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn pressed(&self, track: TrackId, key: u8) -> bool {
        self.held.values().any(|notes| notes.contains(&(track, key)))
    }

    pub fn any_pressed(&self, tracks: &[TrackId], key: u8) -> bool {
        tracks.iter().any(|t| self.pressed(*t, key))
    }

    pub fn matches_track(&self, track: Option<TrackId>) -> bool {
        self.held.values().flatten().all(|(t, _)| Some(*t) == track)
    }

    pub fn press(
        &mut self,
        source: &str,
        track: TrackId,
        key: u8,
        send: impl FnMut(TrackId, u8, Option<u8>) -> bool,
    ) -> bool {
        self.press_many(source, &[track], key, 100, send)
    }

    pub fn press_many(
        &mut self,
        source: &str,
        tracks: &[TrackId],
        key: u8,
        velocity: u8,
        mut send: impl FnMut(TrackId, u8, Option<u8>) -> bool,
    ) -> bool {
        if tracks.is_empty() {
            return false;
        }
        let target: Vec<(TrackId, u8)> = tracks.iter().map(|t| (*t, key)).collect();
        if self.held.get(source) == Some(&target) {
            return true;
        }
        self.release(source, &mut send);
        let vel = velocity.clamp(1, 127);
        let mut active = Vec::with_capacity(target.len());
        for &(track, k) in &target {
            if self.pending_off.contains(&(track, k)) {
                continue;
            }
            if !self.pressed(track, k) && !send(track, k, Some(vel)) {
                continue;
            }
            active.push((track, k));
        }
        if active.is_empty() {
            return false;
        }
        self.held.insert(source.to_owned(), active);
        true
    }

    pub fn release(&mut self, source: &str, mut send: impl FnMut(TrackId, u8, Option<u8>) -> bool) -> bool {
        let Some(notes) = self.held.remove(source) else { return false };
        for (track, key) in notes {
            if !self.pressed(track, key) && !send(track, key, None) {
                self.pending_off.insert((track, key));
            }
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

    /// Play `path` again from the start (→ in the palette), even when it
    /// is the row already playing; as many times as asked.
    pub fn replay(&mut self, path: &Path) -> PathBuf {
        self.pending = None;
        self.playing = Some(path.to_owned());
        path.to_owned()
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The row playing (or waiting for its download to play).
    pub fn current(&self) -> Option<&Path> {
        self.playing.as_deref()
    }

    /// Stopped (palette closed, or a non-sample row selected).
    pub fn stop(&mut self) -> bool {
        self.pending = None;
        self.playing.take().is_some()
    }
}

/// Where an online-only sample stands.
#[derive(Clone, Debug, PartialEq)]
pub enum FetchState {
    /// Still in the cloud; nothing asked for.
    Remote,
    /// Downloading; the fraction once the size is known.
    Downloading(Option<f32>),
    /// On disk now.
    Local,
    Failed(String),
}

/// What finished since the last [`CloudFetches::poll`].
#[derive(Clone, Debug, PartialEq)]
pub enum Fetched {
    Ready(PathBuf),
    Failed(PathBuf, String),
}

/// Online-only samples being downloaded because they were auditioned or
/// dragged: one [`HydrateJob`] each (its own thread; the UI never reads a
/// placeholder), analysed into the user library once local.
#[derive(Default)]
pub struct CloudFetches {
    jobs: HashMap<PathBuf, (HydrateJob, Option<f32>)>,
    local: HashSet<PathBuf>,
    failed: HashMap<PathBuf, String>,
}

impl CloudFetches {
    /// Starts downloading `path` unless it is under way or already here.
    /// `index`: the library to analyse it into afterwards. `true` when a
    /// download was started.
    pub fn start(&mut self, path: &Path, index: Option<(PathBuf, Arc<dyn Analyzer>)>) -> bool {
        if self.local.contains(path) || self.jobs.contains_key(path) {
            return false;
        }
        self.failed.remove(path);
        self.jobs.insert(path.to_owned(), (HydrateJob::spawn(path.to_owned(), index), None));
        true
    }

    pub fn state(&self, path: &Path) -> FetchState {
        if self.local.contains(path) {
            FetchState::Local
        } else if let Some((_, f)) = self.jobs.get(path) {
            FetchState::Downloading(*f)
        } else if let Some(e) = self.failed.get(path) {
            FetchState::Failed(e.clone())
        } else {
            FetchState::Remote
        }
    }

    /// Whether a row flagged online-only still has to be downloaded.
    pub fn needs_fetch(&self, path: &Path) -> bool {
        !self.local.contains(path)
    }

    pub fn running(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Takes the jobs' news: progress, and what finished.
    pub fn poll(&mut self) -> (bool, Vec<Fetched>) {
        let mut changed = false;
        let mut out = Vec::new();
        for (path, (job, fraction)) in &mut self.jobs {
            while let Ok(ev) = job.events.try_recv() {
                changed = true;
                match ev {
                    HydrateEvent::Progress { done, total } => *fraction = (total > 0).then(|| done as f32 / total as f32),
                    HydrateEvent::Ready(_) => out.push(Fetched::Ready(path.clone())),
                    HydrateEvent::Failed(e) => out.push(Fetched::Failed(path.clone(), e)),
                }
            }
        }
        for f in &out {
            match f {
                Fetched::Ready(p) => {
                    self.jobs.remove(p);
                    self.local.insert(p.clone());
                }
                Fetched::Failed(p, e) => {
                    self.jobs.remove(p);
                    self.failed.insert(p.clone(), e.clone());
                }
            }
        }
        (changed, out)
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
    fn replay_restarts_the_same_row_every_time() {
        let mut a = SampleAudition::default();
        let t0 = Instant::now();
        assert!(a.select(Path::new("a"), true, t0).is_some());
        assert_eq!(a.replay(Path::new("a")), PathBuf::from("a"));
        assert_eq!(a.replay(Path::new("a")), PathBuf::from("a"));
        // Replaying cancels a row still settling and becomes the one playing.
        assert_eq!(a.select(Path::new("b"), false, t0), None);
        assert_eq!(a.replay(Path::new("b")), PathBuf::from("b"));
        assert!(!a.is_pending());
        assert_eq!(a.select(Path::new("b"), true, t0), None, "already playing");
    }

    #[test]
    fn cloud_fetch_downloads_once_and_reports() {
        let dir = std::env::temp_dir().join(format!("aitrack-fetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("Loop 120bpm.wav");
        std::fs::write(&f, vec![1u8; 300_000]).unwrap();
        let mut fetches = CloudFetches::default();
        assert_eq!(fetches.state(&f), FetchState::Remote);
        assert!(fetches.start(&f, None));
        assert!(!fetches.start(&f, None), "one download per file");
        assert!(matches!(fetches.state(&f), FetchState::Downloading(_)));
        let t0 = Instant::now();
        let mut done = Vec::new();
        while done.is_empty() && t0.elapsed() < Duration::from_secs(10) {
            done = fetches.poll().1;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(done, vec![Fetched::Ready(f.clone())]);
        assert_eq!(fetches.state(&f), FetchState::Local);
        assert!(!fetches.needs_fetch(&f) && !fetches.running());
        assert!(!fetches.start(&f, None), "already here");

        let missing = dir.join("gone.wav");
        fetches.start(&missing, None);
        let t0 = Instant::now();
        let mut done = Vec::new();
        while done.is_empty() && t0.elapsed() < Duration::from_secs(10) {
            done = fetches.poll().1;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(&done[..], [Fetched::Failed(p, _)] if *p == missing));
        assert!(matches!(fetches.state(&missing), FetchState::Failed(_)));
        assert!(fetches.start(&missing, None), "a failed download can be retried");
        let _ = std::fs::remove_dir_all(dir);
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

    #[test]
    fn ableton_controls_and_multi_track_keyboard_play() {
        assert_eq!(keyboard_note(";", 4), Some(76));
        assert_eq!(keyboard_control("z"), Some(KeyboardControl::OctaveDown));
        assert_eq!(keyboard_control("x"), Some(KeyboardControl::OctaveUp));
        assert_eq!(keyboard_control("c"), Some(KeyboardControl::VelocityDown));
        assert_eq!(keyboard_control("v"), Some(KeyboardControl::VelocityUp));

        let mut a = Audition::default();
        let mut events = Vec::new();
        assert!(a.press_many("a", &[TrackId(1), TrackId(2)], 60, 110, |t, k, v| { events.push((t, k, v)); true }));
        assert!(a.any_pressed(&[TrackId(2)], 60));
        a.release("a", |t, k, v| { events.push((t, k, v)); true });
        assert_eq!(
            events,
            vec![
                (TrackId(1), 60, Some(110)),
                (TrackId(2), 60, Some(110)),
                (TrackId(1), 60, None),
                (TrackId(2), 60, None),
            ]
        );
    }
}
