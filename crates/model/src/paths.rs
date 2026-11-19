//! Audio file paths as pure functions: relative to the project folder when
//! saved, resolved when opened, which files are missing, relinking a lost
//! file, and the plan for "Collect All and Save". The model does no I/O:
//! callers pass in what exists on disk and do the copying themselves.
//!
//! In memory a source's path is absolute. Paths that are not files (a
//! `mem://` source) are never touched. A path counts as relative only when
//! the file sits inside the project folder, so a project folder that moves
//! on its own keeps working and files elsewhere keep their absolute path.

use crate::command::Command;
use crate::ids::SourceId;
use crate::project::*;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// The folder "Collect All and Save" copies into, inside the project folder.
pub const SAMPLES_DIR: &str = "Samples";

/// Sources such as `mem://ramp.wav` are not files on disk.
fn is_virtual(path: &Path) -> bool {
    path.to_str().is_some_and(|s| s.contains("://"))
}

/// `path` relative to `base`, with forward slashes so the project reads the
/// same on macOS and Windows; `None` unless it is an absolute path strictly
/// inside `base`.
pub fn relative_path(path: &Path, base: &Path) -> Option<PathBuf> {
    if path.is_relative() || is_virtual(path) {
        return None;
    }
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| PathBuf::from(parts.join("/")))
}

/// The absolute path of a source's file: a relative path is read from the
/// project folder `base`.
pub fn absolute_path(path: &Path, base: &Path) -> PathBuf {
    if path.is_relative() && !is_virtual(path) { base.join(path) } else { path.to_path_buf() }
}

/// A copy of `p` as it should be written to disk in folder `base`: the files
/// inside it by relative path. `p` itself keeps its absolute paths.
pub fn relativize(p: &Project, base: &Path) -> Project {
    let mut out = p.clone();
    for source in out.sources.values_mut() {
        if let Source::Audio(a) = source
            && let Some(rel) = relative_path(&a.path, base)
        {
            a.path = rel;
        }
    }
    for t in &mut out.tracks {
        if let Some(f) = &mut t.freeze
            && let Some(rel) = relative_path(&f.file.path, base)
        {
            f.file.path = rel;
        }
    }
    out
}

/// Make every relative source path absolute, read from the project folder
/// `base`: what opening a project does (the folder may have moved).
pub fn resolve(p: &mut Project, base: &Path) {
    for source in p.sources.values_mut() {
        if let Source::Audio(a) = source {
            a.path = absolute_path(&a.path, base);
        }
    }
    for t in &mut p.tracks {
        if let Some(f) = &mut t.freeze {
            f.file.path = absolute_path(&f.file.path, base);
        }
    }
}

/// An audio source whose file is not where the project says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingSource {
    pub source: SourceId,
    /// Where it was looked for (absolute).
    pub path: PathBuf,
    pub hash: Option<String>,
}

/// The audio sources whose file `exists` says is not there, in id order.
/// Relative paths are looked for inside `base`; virtual sources are skipped.
pub fn missing_sources(p: &Project, base: &Path, exists: impl Fn(&Path) -> bool) -> Vec<MissingSource> {
    p.sources
        .iter()
        .filter_map(|(&source, s)| match s {
            Source::Audio(a) if !is_virtual(&a.path) => {
                let path = absolute_path(&a.path, base);
                (!exists(&path)).then(|| MissingSource { source, path, hash: a.hash.clone() })
            }
            _ => None,
        })
        .collect()
}

/// The files in `files` that could be the lost file: the same file name,
/// whatever the case.
pub fn candidates_for(missing: &MissingSource, files: &[PathBuf]) -> Vec<PathBuf> {
    let Some(name) = missing.path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return Vec::new() };
    files.iter().filter(|f| f.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == name)).cloned().collect()
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RelinkError {
    #[error("source {0} not found")]
    SourceNotFound(SourceId),
    #[error("source {0} is not an audio source")]
    NotAudio(SourceId),
    #[error("that is not the same file (expected {expected}, found {found})")]
    HashMismatch { expected: String, found: String },
    #[error("the file's hash is needed to check it is the same file (expected {expected})")]
    HashRequired { expected: String },
}

/// The command that points `source` at `candidate`, a file the user picked
/// to replace a lost one. When the source has a `hash`, `candidate_hash`
/// (computed by the caller) must equal it; with no hash on the source any
/// candidate goes, and the candidate's hash is recorded when given.
pub fn relink_source(p: &Project, source: SourceId, candidate: &Path, candidate_hash: Option<&str>) -> Result<Command, RelinkError> {
    let a = match p.sources.get(&source) {
        Some(Source::Audio(a)) => a,
        Some(_) => return Err(RelinkError::NotAudio(source)),
        None => return Err(RelinkError::SourceNotFound(source)),
    };
    if let Some(expected) = &a.hash {
        match candidate_hash {
            None => return Err(RelinkError::HashRequired { expected: expected.clone() }),
            Some(found) if found != expected => {
                return Err(RelinkError::HashMismatch { expected: expected.clone(), found: found.to_owned() });
            }
            Some(_) => {}
        }
    }
    let hash = candidate_hash.map(String::from).or_else(|| a.hash.clone());
    Ok(Command::SetSourceFile { source, path: candidate.to_path_buf(), hash })
}

/// One file "Collect All and Save" has to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyJob {
    /// The file now (absolute).
    pub from: PathBuf,
    /// Where it goes, relative to the project folder (`Samples/<name>`).
    pub to: PathBuf,
    /// Every source that plays this file.
    pub sources: Vec<SourceId>,
}

/// What "Collect All and Save" does.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectPlan {
    /// Copy these files (once per file), then apply `commands`.
    pub copies: Vec<CopyJob>,
    /// Point every collected source at its copy, inside `base`: one undo step.
    pub commands: Vec<Command>,
}

/// Plan collecting every audio file outside the project folder `base` into
/// `base/Samples`. A file used by several sources is copied once. Files with
/// the same name get a number (`kick 2.wav`), also when the name is already
/// on disk (`exists` is asked with the absolute destination) or is only
/// different in case. Files already inside `base`, relative paths and
/// virtual sources are left alone.
pub fn collect_plan(p: &Project, base: &Path, exists: impl Fn(&Path) -> bool) -> CollectPlan {
    let mut copies: Vec<CopyJob> = Vec::new();
    let mut by_file: HashMap<PathBuf, usize> = HashMap::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (&source, s) in &p.sources {
        let Source::Audio(a) = s else { continue };
        if is_virtual(&a.path) || a.path.is_relative() || a.path.starts_with(base) {
            continue;
        }
        if let Some(&i) = by_file.get(&a.path) {
            copies[i].sources.push(source);
            continue;
        }
        let name = a.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "audio".into());
        let (stem, ext) = match (a.path.file_stem(), a.path.extension()) {
            (Some(stem), Some(ext)) if a.path.file_name().is_some_and(|n| n.to_string_lossy().contains('.')) => {
                (stem.to_string_lossy().into_owned(), format!(".{}", ext.to_string_lossy()))
            }
            _ => (name.clone(), String::new()),
        };
        let mut n = 1;
        let to = loop {
            let candidate = if n == 1 { name.clone() } else { format!("{stem} {n}{ext}") };
            let rel = format!("{SAMPLES_DIR}/{candidate}");
            if !used.contains(&rel.to_lowercase()) && !exists(&base.join(&rel)) {
                used.insert(rel.to_lowercase());
                break PathBuf::from(rel);
            }
            n += 1;
        };
        by_file.insert(a.path.clone(), copies.len());
        copies.push(CopyJob { from: a.path.clone(), to, sources: vec![source] });
    }
    let mut commands = Vec::new();
    for job in &copies {
        for &source in &job.sources {
            let Some(Source::Audio(a)) = p.sources.get(&source) else { continue };
            commands.push(Command::SetSourceFile { source, path: base.join(&job.to), hash: a.hash.clone() });
        }
    }
    CollectPlan { copies, commands }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{History, Origin};
    use std::collections::BTreeSet;

    /// The absolute path `p` (written the Unix way, `/music/song`) on this
    /// platform: Windows only takes a path with a drive for absolute.
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) { PathBuf::from(format!("C:{p}")) } else { PathBuf::from(p) }
    }

    fn audio(path: &str, hash: Option<&str>) -> Source {
        let path = if path.starts_with('/') { abs(path) } else { PathBuf::from(path) };
        Source::Audio(AudioSource { path, hash: hash.map(String::from), sample_rate: 48_000, channels: 2, frames: 48_000, warp: Warp::default(), key: None })
    }

    fn project(paths: &[(&str, Option<&str>)]) -> (Project, Vec<SourceId>) {
        let mut p = Project::new("t");
        let ids = paths
            .iter()
            .map(|(path, hash)| {
                let id = p.alloc_source_id();
                p.sources.insert(id, audio(path, *hash));
                id
            })
            .collect();
        (p, ids)
    }

    fn path_of(p: &Project, id: SourceId) -> PathBuf {
        let Some(Source::Audio(a)) = p.sources.get(&id) else { panic!("audio") };
        a.path.clone()
    }

    #[test]
    fn a_frozen_tracks_file_is_saved_relative_and_resolved_on_open() {
        let (mut p, _) = project(&[]);
        let id = p.alloc_track_id();
        let mut t = Track::new(id, TrackKind::Audio, "a", Color(0));
        t.freeze = Some(Freeze { file: match audio("/music/song/Samples/Frozen/a Frozen.wav", None) { Source::Audio(a) => a, _ => unreachable!() } });
        p.tracks.push(t);
        let saved = relativize(&p, &abs("/music/song"));
        assert_eq!(saved.track(id).unwrap().freeze.as_ref().unwrap().file.path, PathBuf::from("Samples/Frozen/a Frozen.wav"));
        let mut opened = saved;
        resolve(&mut opened, &abs("/elsewhere/song"));
        assert_eq!(opened.track(id).unwrap().freeze.as_ref().unwrap().file.path, abs("/elsewhere/song/Samples/Frozen/a Frozen.wav"));
    }

    #[test]
    fn only_paths_inside_the_project_folder_become_relative() {
        let base = &abs("/music/song");
        assert_eq!(relative_path(&abs("/music/song/Samples/kick.wav"), base), Some(PathBuf::from("Samples/kick.wav")));
        assert_eq!(relative_path(&abs("/music/song/a.wav"), base), Some(PathBuf::from("a.wav")));
        assert_eq!(relative_path(&abs("/music/other/kick.wav"), base), None, "outside stays absolute");
        assert_eq!(relative_path(&abs("/music/song-2/kick.wav"), base), None, "a sibling with the same prefix is outside");
        assert_eq!(relative_path(Path::new("Samples/kick.wav"), base), None, "already relative");
        assert_eq!(relative_path(base, base), None);
    }

    #[test]
    fn saving_relative_and_opening_after_moving_the_folder_resolves_every_file() {
        let (p, ids) = project(&[
            ("/music/song/Samples/kick.wav", None),
            ("/library/loops/pad.wav", None),
            ("mem://ramp.wav", None),
            ("/music/song/Samples/Deep/snare.wav", None),
        ]);
        let saved = relativize(&p, &abs("/music/song"));
        assert_eq!(path_of(&saved, ids[0]), PathBuf::from("Samples/kick.wav"));
        assert_eq!(path_of(&saved, ids[1]), abs("/library/loops/pad.wav"));
        assert_eq!(path_of(&saved, ids[2]), PathBuf::from("mem://ramp.wav"));
        assert_eq!(path_of(&saved, ids[3]), PathBuf::from("Samples/Deep/snare.wav"));
        assert_eq!(path_of(&p, ids[0]), abs("/music/song/Samples/kick.wav"), "the project in memory is untouched");
        // The folder moved: open the saved text and resolve against the new place.
        let mut reopened = Project::open_json(&saved.to_json().unwrap()).unwrap();
        resolve(&mut reopened, &abs("/elsewhere/song"));
        assert_eq!(path_of(&reopened, ids[0]), abs("/elsewhere/song/Samples/kick.wav"));
        assert_eq!(path_of(&reopened, ids[1]), abs("/library/loops/pad.wav"));
        assert_eq!(path_of(&reopened, ids[2]), PathBuf::from("mem://ramp.wav"));
        assert_eq!(path_of(&reopened, ids[3]), abs("/elsewhere/song/Samples/Deep/snare.wav"));
        // Back where it was, the round trip is the identity.
        let mut home = saved.clone();
        resolve(&mut home, &abs("/music/song"));
        assert_eq!(home.sources, p.sources);
    }

    #[test]
    fn missing_files_are_found_with_a_caller_supplied_check() {
        let (p, ids) = project(&[("/m/Samples/a.wav", None), ("Samples/b.wav", None), ("mem://x.wav", None), ("/lost/c.wav", Some("h3"))]);
        let present: BTreeSet<PathBuf> = [abs("/m/Samples/a.wav")].into();
        let missing = missing_sources(&p, &abs("/m"), |f| present.contains(f));
        // A relative path is looked for inside the project folder; memory sources are never missing.
        assert_eq!(missing.iter().map(|m| m.source).collect::<Vec<_>>(), vec![ids[1], ids[3]]);
        assert_eq!(missing[0].path, abs("/m/Samples/b.wav"));
        assert_eq!(missing[1].hash.as_deref(), Some("h3"));
        // Candidates for a lost file: same file name, any case.
        let found = candidates_for(&missing[1], &[PathBuf::from("/new/C.WAV"), PathBuf::from("/new/d.wav"), PathBuf::from("/other/c.wav")]);
        assert_eq!(found, vec![PathBuf::from("/new/C.WAV"), PathBuf::from("/other/c.wav")]);
    }

    #[test]
    fn relinking_checks_the_hash_and_is_one_undoable_command() {
        let (mut p, ids) = project(&[("/lost/kick.wav", Some("abc")), ("/lost/snare.wav", None)]);
        let before = p.clone();
        // A candidate with another hash is not the same file; without a hash we cannot tell.
        assert_eq!(
            relink_source(&p, ids[0], Path::new("/new/kick.wav"), Some("zzz")),
            Err(RelinkError::HashMismatch { expected: "abc".into(), found: "zzz".into() })
        );
        assert_eq!(relink_source(&p, ids[0], Path::new("/new/kick.wav"), None), Err(RelinkError::HashRequired { expected: "abc".into() }));
        assert_eq!(relink_source(&p, SourceId(999), Path::new("/x.wav"), None), Err(RelinkError::SourceNotFound(SourceId(999))));
        let notes = p.alloc_source_id();
        p.sources.insert(notes, Source::Notes(NoteSource::default()));
        assert_eq!(relink_source(&p, notes, Path::new("/x.wav"), None), Err(RelinkError::NotAudio(notes)));
        let before = Project { ids: p.ids.clone(), ..before };
        p.sources.remove(&notes);

        let cmd = relink_source(&p, ids[0], Path::new("/new/kick.wav"), Some("abc")).unwrap();
        let mut h = History::default();
        h.apply(&mut p, "Relink", Origin::User, vec![cmd]).unwrap();
        assert_eq!(path_of(&p, ids[0]), PathBuf::from("/new/kick.wav"));
        h.undo(&mut p).unwrap();
        assert_eq!(p.sources, before.sources);
        // With no hash to check, any candidate is accepted, and its hash is recorded.
        let cmd = relink_source(&p, ids[1], Path::new("/new/snare.wav"), Some("def")).unwrap();
        h.apply(&mut p, "Relink", Origin::User, vec![cmd]).unwrap();
        let Some(Source::Audio(a)) = p.sources.get(&ids[1]) else { panic!() };
        assert_eq!((a.path.clone(), a.hash.clone()), (PathBuf::from("/new/snare.wav"), Some("def".to_string())));
        h.undo(&mut p).unwrap();
        h.redo(&mut p).unwrap();
        assert_eq!(path_of(&p, ids[1]), PathBuf::from("/new/snare.wav"));
        // Commands are plain data for agents and scripts.
        let c = relink_source(&p, ids[0], Path::new("/n.wav"), Some("abc")).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&serde_json::to_string(&c).unwrap()).unwrap(), c);
        assert_eq!(c.label(), "Relink file");
    }

    #[test]
    fn collect_all_plans_one_copy_per_file_and_resolves_name_clashes() {
        let (p, ids) = project(&[
            ("/lib/a/kick.wav", None),
            ("/lib/b/kick.wav", None),          // same name, another file
            ("/lib/a/kick.wav", None),          // same file as the first: one copy
            ("/music/song/Samples/own.wav", None), // already in the project
            ("/lib/Hat.wav", None),
            ("/lib/other/hat.WAV", None),       // clashes with Hat.wav on a case-insensitive disk
            ("mem://ramp.wav", None),
            ("Samples/rel.wav", None),          // relative: already in the project
            ("/lib/taken.wav", None),
        ]);
        let base = &abs("/music/song");
        let taken = abs("/music/song/Samples/taken.wav");
        let plan = collect_plan(&p, base, |f| f == taken);
        // Compared as paths: a path reads with the platform's own separator.
        let jobs: Vec<(PathBuf, PathBuf)> = plan.copies.iter().map(|c| (c.from.clone(), c.to.clone())).collect();
        assert_eq!(
            jobs,
            vec![
                (abs("/lib/a/kick.wav"), PathBuf::from("Samples/kick.wav")),
                (abs("/lib/b/kick.wav"), PathBuf::from("Samples/kick 2.wav")),
                (abs("/lib/Hat.wav"), PathBuf::from("Samples/Hat.wav")),
                (abs("/lib/other/hat.WAV"), PathBuf::from("Samples/hat 2.WAV")),
                (abs("/lib/taken.wav"), PathBuf::from("Samples/taken 2.wav")),
            ]
        );
        assert_eq!(plan.copies[0].sources, vec![ids[0], ids[2]]);
        // The commands point every source at its copy, as one undo step.
        let mut q = p.clone();
        let mut h = History::default();
        h.apply(&mut q, "Collect All and Save", Origin::User, plan.commands.clone()).unwrap();
        assert_eq!(path_of(&q, ids[0]), abs("/music/song/Samples/kick.wav"));
        assert_eq!(path_of(&q, ids[1]), abs("/music/song/Samples/kick 2.wav"));
        assert_eq!(path_of(&q, ids[2]), abs("/music/song/Samples/kick.wav"));
        assert_eq!(path_of(&q, ids[3]), abs("/music/song/Samples/own.wav"));
        assert_eq!(path_of(&q, ids[6]), PathBuf::from("mem://ramp.wav"));
        assert_eq!(path_of(&q, ids[7]), PathBuf::from("Samples/rel.wav"));
        h.undo(&mut q).unwrap();
        assert_eq!(q.sources, p.sources);
        // Collected, the project saves with nothing but relative paths and a second plan has nothing to do.
        h.redo(&mut q).unwrap();
        assert!(collect_plan(&q, base, |_| false).copies.is_empty());
        let saved = relativize(&q, base);
        assert_eq!(path_of(&saved, ids[1]), PathBuf::from("Samples/kick 2.wav"));
    }

    #[test]
    fn a_name_without_an_extension_or_with_dots_gets_its_number_in_the_right_place() {
        let (p, _) = project(&[("/a/take", None), ("/b/take", None), ("/a/v1.2.final.wav", None), ("/b/v1.2.final.wav", None)]);
        let plan = collect_plan(&p, &abs("/m"), |_| false);
        let to: Vec<PathBuf> = plan.copies.iter().map(|c| c.to.clone()).collect();
        assert_eq!(to, ["Samples/take", "Samples/take 2", "Samples/v1.2.final.wav", "Samples/v1.2.final 2.wav"].map(PathBuf::from));
    }
}
