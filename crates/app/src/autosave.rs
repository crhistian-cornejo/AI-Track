//! Autosave and recovery. Every minute, if the project changed since the last
//! copy (`History::revision`), a copy is written on a background thread to
//! `<config>/Autosave/`. A copy is kept per project (by its file, or one for
//! the untitled project) next to a small `.json` that says whose it is. Saving
//! the project, or giving it up on purpose, removes its copy. At start-up, a
//! copy that is newer than the project's own file is offered for recovery.

use crate::project_io::write_atomic;
use aitrack_model::Project;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How often a changed project is copied.
pub const INTERVAL: Duration = Duration::from_secs(60);

/// Where the copies live.
pub fn dir() -> PathBuf {
    crate::appearance::config_dir().join("Autosave")
}

/// Whose copy a file is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// The project's own file; `None` for a project never saved.
    pub original: Option<PathBuf>,
    pub name: String,
}

/// The name the copy of the project saved at `original` goes by: stable per
/// file. A project without a file has no stable name; see [`Autosaver::key`].
pub fn key(original: &Path) -> String {
    // FNV-1a over the path: short, stable and safe in a file name.
    let hash = original.to_string_lossy().bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{hash:016x}")
}

fn project_file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{}", crate::project_io::EXTENSION))
}

fn meta_file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

/// Write the copy of a project (already encoded as `json`) and its `meta`.
pub fn write(dir: &Path, key: &str, meta: &Meta, json: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let meta_json = serde_json::to_vec(meta).map_err(|e| e.to_string())?;
    // The project first: a copy without its meta is ignored, never half-read.
    write_atomic(&project_file(dir, key), json.as_bytes(), false)?;
    write_atomic(&meta_file(dir, key), &meta_json, false)
}

/// Remove a project's copy (it was saved, or given up).
pub fn remove(dir: &Path, key: &str) {
    let _ = std::fs::remove_file(meta_file(dir, key));
    let _ = std::fs::remove_file(project_file(dir, key));
}

/// A copy worth offering back.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub key: String,
    pub file: PathBuf,
    pub meta: Meta,
    pub saved_at: SystemTime,
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The copies that hold work the project's own file does not, newest first.
/// A copy older than its file (the project was saved afterwards) is stale and removed.
pub fn find(dir: &Path) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(key) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
        let file = project_file(dir, &key);
        let (Some(saved_at), Some(meta)) = (modified(&file), std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Meta>(&b).ok())) else {
            continue;
        };
        if let Some(original) = &meta.original
            && (modified(original).is_some_and(|m| m >= saved_at) || same_project(original, &file))
        {
            remove(dir, &key);
            continue;
        }
        found.push(Found { key, file, meta, saved_at });
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.saved_at));
    found
}

/// The copy holds exactly what the project's file holds (it was saved at the
/// same state, whatever the dates say): nothing to recover.
fn same_project(original: &Path, copy: &Path) -> bool {
    match (crate::project_io::load(original), crate::project_io::load_in(copy, crate::project_io::project_folder(original))) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether any autosaved copy in `dir` mentions the file `path` (a recording
/// that a project, possibly one waiting to be recovered, still plays).
pub fn references(dir: &Path, path: &Path) -> bool {
    // As the JSON writes it: escaped, so Windows paths match too.
    let Ok(quoted) = serde_json::to_string(path) else { return true };
    let needle = quoted.trim_matches('"');
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(crate::project_io::EXTENSION)).any(|e| {
        // A copy that cannot be read counts as referencing it: better a stray file than a lost take.
        std::fs::read_to_string(e.path()).map_or(true, |text| text.contains(needle))
    })
}

/// "3 minutes ago", for the recovery question.
pub fn ago(then: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(then).map_or(0, |d| d.as_secs());
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=59 => "less than a minute ago".to_owned(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

/// When to copy, and the background writes. One per window.
pub struct Autosaver {
    last_check: Instant,
    written_revision: Option<u64>,
    /// Bumped when a copy is removed: a write still running then must not bring it back.
    epoch: Arc<AtomicU64>,
    /// Held while a copy is written and while one is removed, so a removal
    /// never lands in the middle of a write.
    files: Arc<Mutex<()>>,
    /// The name of this session's copy of a project that has no file yet. Each
    /// session has its own, so a copy left by an earlier one is never overwritten
    /// before it was offered back.
    untitled: String,
}

impl Autosaver {
    pub fn new(now: Instant) -> Self {
        static SESSIONS: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        Self {
            last_check: now,
            written_revision: None,
            epoch: Arc::new(AtomicU64::new(0)),
            files: Arc::new(Mutex::new(())),
            untitled: format!("untitled-{stamp:x}-{:x}-{}", std::process::id(), SESSIONS.fetch_add(1, Ordering::Relaxed)),
        }
    }

    /// The copy name of the open project: its file's, or this session's untitled one.
    pub fn key(&self, original: Option<&Path>) -> String {
        match original {
            None => self.untitled.clone(),
            Some(path) => key(path),
        }
    }

    /// The recovered project had no file: its copy keeps its name, so the next
    /// copies replace it rather than sit beside it.
    pub fn adopt_untitled(&mut self, key: String) {
        self.untitled = key;
    }

    /// Whether a copy is due now: a minute since the last look, the project
    /// has unsaved changes, and it is not the revision already copied.
    pub fn due(&mut self, now: Instant, revision: u64, dirty: bool) -> bool {
        if now.saturating_duration_since(self.last_check) < INTERVAL {
            return false;
        }
        self.last_check = now;
        if !dirty || self.written_revision == Some(revision) {
            return false;
        }
        self.written_revision = Some(revision);
        true
    }

    /// Another project is open (or this one was recovered): start over, with
    /// the first copy due at the next look.
    pub fn reset(&mut self, now: Instant) {
        self.last_check = now.checked_sub(INTERVAL).unwrap_or(now);
        self.written_revision = None;
    }

    /// Copy `project` on a background thread.
    pub fn write_in_background(&self, dir: PathBuf, key: String, meta: Meta, project: Project) -> Option<std::thread::JoinHandle<()>> {
        let (epoch, files, mine) = (self.epoch.clone(), self.files.clone(), self.epoch.load(Ordering::Acquire));
        std::thread::Builder::new()
            .name("aitrack-autosave".into())
            .spawn(move || {
                let Ok(json) = project.to_json() else { return };
                // Check and write under the lock a removal takes: either the
                // removal came first (and this writes nothing) or it comes after
                // (and removes what this wrote).
                let _held = files.lock().unwrap_or_else(|p| p.into_inner());
                if epoch.load(Ordering::Acquire) != mine {
                    return;
                }
                if let Err(e) = write(&dir, &key, &meta, &json) {
                    log::warn!("autosave: {e}");
                }
            })
            .ok()
    }

    /// Remove the copy of `key` and stop any write in flight from restoring it.
    pub fn discard(&self, dir: &Path, key: &str) {
        let _held = self.files.lock().unwrap_or_else(|p| p.into_inner());
        self.epoch.fetch_add(1, Ordering::AcqRel);
        remove(dir, key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitrack-autosave-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_mtime(path: &Path, t: SystemTime) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn a_copy_is_written_found_recovered_and_dropped_when_the_project_is_saved() {
        let root = scratch("cycle");
        let (copies, songs) = (root.join("Autosave"), root.join("Songs"));
        std::fs::create_dir_all(&songs).unwrap();
        // The song as last saved, an hour ago.
        let original = songs.join("Night.aitrack");
        let mut project = crate::demo::project();
        project.name = "Night".into();
        crate::project_io::save(&original, &project).unwrap();
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        set_mtime(&original, hour_ago);
        // Edited since, then the app dies: only the copy has the edit.
        let saved_bpm = project.tempo.initial_bpm();
        assert_ne!(saved_bpm, 141.0);
        project.tempo = aitrack_model::TempoMap::constant(141.0);
        let k = key(&original);
        let meta = Meta { original: Some(original.clone()), name: project.name.clone() };
        write(&copies, &k, &meta, &project.to_json().unwrap()).unwrap();
        // Start-up: the copy is found, newer than the file, and brings the edit back.
        let found = find(&copies);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].meta, meta);
        let back = crate::project_io::load(&found[0].file).unwrap();
        assert_eq!(back.tempo.initial_bpm(), 141.0);
        assert_eq!(crate::project_io::load(&original).unwrap().tempo.initial_bpm(), saved_bpm);
        // Saved afterwards: the file is newer than the copy, which is stale and goes.
        set_mtime(&found[0].file, hour_ago - Duration::from_secs(10));
        assert!(find(&copies).is_empty());
        assert!(!project_file(&copies, &k).exists() && !meta_file(&copies, &k).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_untitled_project_has_its_own_copy_and_the_newest_comes_first() {
        let dir = scratch("untitled");
        let meta = |o: Option<&str>| Meta { original: o.map(PathBuf::from), name: "x".into() };
        let json = crate::demo::empty().to_json().unwrap();
        let (one, two) = (Autosaver::new(Instant::now()), Autosaver::new(Instant::now() + Duration::from_millis(1)));
        assert!(one.key(None).starts_with("untitled-"));
        assert_eq!(one.key(None), one.key(None), "stable within a session");
        assert_eq!(one.key(Some(Path::new("/a/b.aitrack"))), key(Path::new("/a/b.aitrack")));
        let _ = two;
        assert_ne!(key(Path::new("/a/b.aitrack")), key(Path::new("/a/c.aitrack")));
        assert_eq!(key(Path::new("/a/b.aitrack")), key(Path::new("/a/b.aitrack")));
        write(&dir, "untitled", &meta(None), &json).unwrap();
        // A project whose file is gone still gets its copy offered.
        let gone = key(Path::new("/no/such/gone.aitrack"));
        write(&dir, &gone, &meta(Some("/no/such/gone.aitrack")), &json).unwrap();
        set_mtime(&project_file(&dir, "untitled"), SystemTime::now() - Duration::from_secs(600));
        let found = find(&dir);
        assert_eq!(found.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), [gone.as_str(), "untitled"]);
        // A copy without its meta (an interrupted write) is ignored.
        std::fs::remove_file(meta_file(&dir, &gone)).unwrap();
        assert_eq!(find(&dir).len(), 1);
        remove(&dir, "untitled");
        assert!(find(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copy_is_due_a_minute_after_the_last_look_and_only_for_a_new_revision() {
        let t0 = Instant::now();
        let mut a = Autosaver::new(t0);
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(!a.due(at(30), 3, true), "too soon");
        assert!(a.due(at(61), 3, true), "a minute passed and the project changed");
        assert!(!a.due(at(90), 4, true), "the next look is a minute after the last");
        assert!(!a.due(at(125), 3, true), "revision 3 is already copied");
        assert!(a.due(at(190), 4, true));
        assert!(!a.due(at(260), 9, false), "saved: nothing to copy");
        a.reset(at(300));
        assert!(a.due(at(300), 4, true), "after a reset the first look is due at once");
    }

    #[test]
    fn a_write_in_flight_does_not_bring_back_a_discarded_copy() {
        let dir = scratch("discard");
        let a = Autosaver::new(Instant::now());
        let meta = Meta { original: None, name: "x".into() };
        a.write_in_background(dir.clone(), "untitled".into(), meta.clone(), crate::demo::empty());
        let start = Instant::now();
        while !project_file(&dir, "untitled").exists() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(meta_file(&dir, "untitled").exists());
        a.discard(&dir, "untitled");
        assert!(!project_file(&dir, "untitled").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_sessions_never_share_the_copy_of_an_untitled_project() {
        let dir = scratch("sessions");
        let (old_session, new_session) = (Autosaver::new(Instant::now()), Autosaver::new(Instant::now()));
        assert_ne!(old_session.key(None), new_session.key(None), "an earlier session's copy is not overwritten by this one's");
        let meta = Meta { original: None, name: "Old idea".into() };
        write(&dir, &old_session.key(None), &meta, &crate::demo::empty().to_json().unwrap()).unwrap();
        // The new session writes its own copy: both are found.
        new_session.write_in_background(dir.clone(), new_session.key(None), meta.clone(), crate::demo::empty()).unwrap().join().unwrap();
        assert_eq!(find(&dir).len(), 2);
        // Recovering the old one makes its name this session's: the next copies replace it.
        let mut recovered = Autosaver::new(Instant::now());
        recovered.adopt_untitled(old_session.key(None));
        assert_eq!(recovered.key(None), old_session.key(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A copy that holds what the file holds is not offered, whatever the dates say.
    #[test]
    fn a_copy_equal_to_the_saved_file_is_not_offered_even_if_it_is_newer() {
        let root = scratch("same");
        let (copies, original) = (root.join("Autosave"), root.join("Song.aitrack"));
        let project = crate::demo::project();
        crate::project_io::save(&original, &project).unwrap();
        // The copy was written after the file but of the same state (a write that lost a race with the save).
        let k = key(&original);
        write(&copies, &k, &Meta { original: Some(original.clone()), name: "Song".into() }, &project.to_json().unwrap()).unwrap();
        set_mtime(&original, SystemTime::now() - Duration::from_secs(60));
        assert!(find(&copies).is_empty(), "same content");
        // With one more edit in the copy, it is offered.
        let mut edited = project.clone();
        edited.name = "Edited".into();
        write(&copies, &k, &Meta { original: Some(original.clone()), name: "Song".into() }, &edited.to_json().unwrap()).unwrap();
        assert_eq!(find(&copies).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A write that is in flight when the project is saved must not leave a copy
    /// behind: it would be offered at the next start with older content.
    #[test]
    fn no_write_in_flight_can_leave_a_copy_after_the_discard() {
        let dir = scratch("race");
        let a = Autosaver::new(Instant::now());
        let meta = Meta { original: None, name: "x".into() };
        let project = crate::demo::project();
        for round in 0..150 {
            let writer = a.write_in_background(dir.clone(), "untitled".into(), meta.clone(), project.clone());
            if round % 3 == 0 {
                std::thread::yield_now();
            }
            a.discard(&dir, "untitled");
            if let Some(w) = writer {
                w.join().unwrap();
            }
            assert!(!project_file(&dir, "untitled").exists() && !meta_file(&dir, "untitled").exists(), "round {round}: a copy survived the discard");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recording_an_autosaved_copy_plays_is_found_in_it() {
        let dir = scratch("refs");
        let (used, free) = (std::env::temp_dir().join("takes").join("used take.wav"), std::env::temp_dir().join("takes").join("free take.wav"));
        let mut project = crate::demo::empty();
        let id = project.alloc_source_id();
        project.sources.insert(id, aitrack_model::Source::Audio(aitrack_model::AudioSource { path: used.clone(), hash: None, sample_rate: 48_000, channels: 1, frames: 1, warp: aitrack_model::Warp::default(), key: None }));
        write(&dir, "untitled-x", &Meta { original: None, name: "x".into() }, &project.to_json().unwrap()).unwrap();
        assert!(references(&dir, &used));
        assert!(!references(&dir, &free));
        assert!(!references(&dir.join("missing"), &used), "no folder, no copies");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn times_read_naturally() {
        let now = SystemTime::now();
        assert_eq!(ago(now - Duration::from_secs(20), now), "less than a minute ago");
        assert_eq!(ago(now - Duration::from_secs(60), now), "1 minute ago");
        assert_eq!(ago(now - Duration::from_secs(7300), now), "2 hours ago");
    }
}
