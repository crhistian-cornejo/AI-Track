//! "Organize on disk": an optional, undoable copy/move of samples into a
//! clean folder layout.
//!
//! 1. [`Library::plan_organize`] builds a dry-run [`OrganizePlan`]
//!    (source → destination, conflicts resolved, duplicates skipped).
//! 2. [`OrganizePlan::execute`] performs it, writing a JSON-lines journal in
//!    `<dest>/.aitrack-organize/<id>.jsonl` *before* each operation.
//! 3. [`undo_organize`] reverts a journal: copies are removed (only if the
//!    content still matches), moves are moved back. Originals are never
//!    deleted in copy mode, and nothing is ever overwritten.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::{Library, RootKind, Sample, SampleId, now, rel_string};
use crate::error::{LibraryError, Result};
use crate::query::SearchQuery;
use crate::scan::quick_hash;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrganizeLayout {
    /// `<dest>/Key/<Key Scale>/<Category>/<Sub>/file` (atonal → `Key/Atonal/…`).
    ByKey,
    /// `<dest>/<Category>/<Sub>/file`.
    ByCategory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OrganizeMode {
    /// Default: originals stay where they are.
    #[default]
    Copy,
    /// Moves files and updates the index (same-volume rename, else copy + delete).
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpAction {
    Copy,
    Move,
    /// Destination already holds identical content.
    SkipDuplicate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrganizeOp {
    pub sample: SampleId,
    pub src: PathBuf,
    pub dst: PathBuf,
    pub bytes: u64,
    pub action: OpAction,
    /// The destination name was changed to avoid overwriting another file.
    pub renamed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrganizePlan {
    pub dest: PathBuf,
    pub layout: OrganizeLayout,
    pub mode: OrganizeMode,
    pub ops: Vec<OrganizeOp>,
    /// Bytes that will be written (copies) or moved.
    pub total_bytes: u64,
}

impl OrganizePlan {
    /// Folder → file count, for the preview tree.
    pub fn preview(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for op in self.ops.iter().filter(|o| o.action != OpAction::SkipDuplicate) {
            let rel = op.dst.parent().and_then(|p| p.strip_prefix(&self.dest).ok()).map(rel_string).unwrap_or_default();
            *m.entry(rel).or_insert(0) += 1;
        }
        m
    }

    pub fn duplicates(&self) -> usize {
        self.ops.iter().filter(|o| o.action == OpAction::SkipDuplicate).count()
    }

    /// Runs the plan. Returns the journal (needed for undo).
    pub fn execute(&self, lib: &Library) -> Result<OrganizeJournal> {
        let id = format!("{}-{}", now(), std::process::id());
        let dir = self.dest.join(".aitrack-organize");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.jsonl"));
        let mut file = OpenOptions::new().create_new(true).append(true).open(&path)?;
        let header = JournalLine::Header { id: id.clone(), mode: self.mode, dest: self.dest.clone(), ts: now() };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;

        // Moved samples must stay indexed: make sure the destination is a root.
        let dest_root = if self.mode == OrganizeMode::Move {
            Some(match lib.root_for_path(&self.dest)? {
                Some(r) => r,
                None => {
                    let rid = lib.add_root(&self.dest, RootKind::User)?;
                    lib.root(rid)?
                }
            })
        } else {
            None
        };

        let mut done = 0usize;
        for (seq, op) in self.ops.iter().enumerate() {
            if op.action == OpAction::SkipDuplicate {
                continue;
            }
            if op.dst.exists() {
                return Err(LibraryError::Other(format!("refusing to overwrite {}", op.dst.display())));
            }
            let hash = quick_hash(&op.src).ok().map(hex);
            let entry = JournalLine::Op {
                seq,
                sample: op.sample,
                action: op.action,
                src: op.src.clone(),
                dst: op.dst.clone(),
                hash,
                done: false,
            };
            writeln!(file, "{}", serde_json::to_string(&entry)?)?;
            file.flush()?;
            if let Some(parent) = op.dst.parent() {
                fs::create_dir_all(parent)?;
            }
            match op.action {
                OpAction::Copy => {
                    fs::copy(&op.src, &op.dst)?;
                }
                OpAction::Move => {
                    if fs::rename(&op.src, &op.dst).is_err() {
                        fs::copy(&op.src, &op.dst)?;
                        fs::remove_file(&op.src)?;
                    }
                    if let Some(root) = &dest_root {
                        let rel = rel_string(op.dst.strip_prefix(&root.path).unwrap_or(&op.dst));
                        lib.relocate(op.sample, root.id, &rel)?;
                    }
                }
                OpAction::SkipDuplicate => {}
            }
            writeln!(file, "{}", serde_json::to_string(&JournalLine::Done { seq })?)?;
            done += 1;
        }
        file.sync_all()?;
        Ok(OrganizeJournal { id, path, operations: done })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrganizeJournal {
    pub id: String,
    pub path: PathBuf,
    pub operations: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum JournalLine {
    Header { id: String, mode: OrganizeMode, dest: PathBuf, ts: i64 },
    Op { seq: usize, sample: SampleId, action: OpAction, src: PathBuf, dst: PathBuf, hash: Option<String>, done: bool },
    Done { seq: usize },
    Undone { ts: i64 },
}

fn hex(h: [u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Makes a folder name safe on Windows and macOS.
fn safe(component: &str) -> String {
    let s: String = component
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').to_string();
    if s.is_empty() { "_".into() } else { s }
}

/// Destination folder (relative to the plan's root) for a sample.
pub fn target_dir(s: &Sample, layout: OrganizeLayout) -> PathBuf {
    let mut p = PathBuf::new();
    if layout == OrganizeLayout::ByKey {
        p.push("Key");
        p.push(safe(&s.key_label().unwrap_or_else(|| "Atonal".into())));
    }
    p.push(safe(s.category.map_or("Uncategorized", |c| c.as_str())));
    if let Some(sub) = &s.subcategory {
        p.push(safe(sub));
    }
    p
}

impl Library {
    /// Builds a dry-run plan. `samples` empty = the whole library.
    pub fn plan_organize(
        &self,
        samples: &[SampleId],
        dest: &Path,
        layout: OrganizeLayout,
        mode: OrganizeMode,
    ) -> Result<OrganizePlan> {
        let list: Vec<Sample> = if samples.is_empty() {
            let mut all = Vec::new();
            let mut offset = 0;
            loop {
                let page = self.search(&SearchQuery { limit: 5000, offset, ..Default::default() })?;
                let n = page.len();
                all.extend(page);
                if n < 5000 {
                    break;
                }
                offset += n;
            }
            all
        } else {
            samples.iter().map(|id| self.get(*id)).collect::<Result<_>>()?
        };
        let dest = std::path::absolute(dest)?;
        let mut taken: HashSet<PathBuf> = HashSet::new();
        let mut ops = Vec::new();
        let mut total = 0u64;
        for s in list {
            if !s.path.is_file() || s.path.starts_with(&dest) {
                continue;
            }
            let dir = dest.join(target_dir(&s, layout));
            let file_name = s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let stem = Path::new(&file_name).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = Path::new(&file_name).extension().map(|x| x.to_string_lossy().into_owned());
            let mut dst = dir.join(&file_name);
            let mut action = match mode {
                OrganizeMode::Copy => OpAction::Copy,
                OrganizeMode::Move => OpAction::Move,
            };
            let mut renamed = false;
            let mut n = 2;
            loop {
                if taken.contains(&dst) {
                    // Another sample in this plan already goes here.
                } else if dst.exists() {
                    if quick_hash(&dst).ok() == quick_hash(&s.path).ok() {
                        action = OpAction::SkipDuplicate;
                        break;
                    }
                } else {
                    break;
                }
                let name = match &ext {
                    Some(e) => format!("{stem}_{n}.{e}"),
                    None => format!("{stem}_{n}"),
                };
                dst = dir.join(name);
                renamed = true;
                n += 1;
            }
            taken.insert(dst.clone());
            if action != OpAction::SkipDuplicate {
                total += s.size;
            }
            ops.push(OrganizeOp { sample: s.id, src: s.path.clone(), dst, bytes: s.size, action, renamed });
        }
        Ok(OrganizePlan { dest, layout, mode, ops, total_bytes: total })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoStats {
    pub removed_copies: usize,
    pub moved_back: usize,
    /// Entries left alone because the file changed or the original came back.
    pub skipped: usize,
}

/// Reverts an organize journal. Idempotent: a journal is undone once.
pub fn undo_organize(lib: &Library, journal: &Path) -> Result<UndoStats> {
    let f = fs::File::open(journal)?;
    let mut ops: Vec<(usize, SampleId, OpAction, PathBuf, PathBuf, Option<String>)> = Vec::new();
    let mut done: HashSet<usize> = HashSet::new();
    let mut dest: Option<PathBuf> = None;
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A torn last line (crash mid-write) is ignored.
        let Ok(entry) = serde_json::from_str::<JournalLine>(&line) else { continue };
        match entry {
            JournalLine::Header { dest: d, .. } => dest = Some(d),
            JournalLine::Op { seq, sample, action, src, dst, hash, .. } => ops.push((seq, sample, action, src, dst, hash)),
            JournalLine::Done { seq } => {
                done.insert(seq);
            }
            JournalLine::Undone { .. } => return Err(LibraryError::Other("journal already undone".into())),
        }
    }
    let mut stats = UndoStats::default();
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for (seq, sample, action, src, dst, hash) in ops.into_iter().rev() {
        let matches = dst.is_file() && hash.as_deref().is_none_or(|h| quick_hash(&dst).ok().map(hex).as_deref() == Some(h));
        // An op without "done" may or may not have happened; trust the file.
        if !done.contains(&seq) && !matches {
            continue;
        }
        if !matches {
            stats.skipped += 1;
            continue;
        }
        match action {
            OpAction::Copy => {
                fs::remove_file(&dst)?;
                stats.removed_copies += 1;
            }
            OpAction::Move => {
                if src.exists() {
                    stats.skipped += 1;
                    continue;
                }
                if let Some(p) = src.parent() {
                    fs::create_dir_all(p)?;
                }
                if fs::rename(&dst, &src).is_err() {
                    fs::copy(&dst, &src)?;
                    fs::remove_file(&dst)?;
                }
                if let Some(root) = lib.root_for_path(&src)? {
                    let rel = rel_string(src.strip_prefix(&root.path).unwrap_or(&src));
                    lib.relocate(sample, root.id, &rel)?;
                }
                stats.moved_back += 1;
            }
            OpAction::SkipDuplicate => {}
        }
        if let Some(p) = dst.parent() {
            dirs.insert(p.to_path_buf());
        }
    }
    // Remove folders the organize created and that are now empty.
    if let Some(dest) = dest {
        let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            let mut cur = Some(d.as_path());
            while let Some(c) = cur {
                if c == dest || !c.starts_with(&dest) || fs::remove_dir(c).is_err() {
                    break;
                }
                cur = c.parent();
            }
        }
    }
    let mut f = OpenOptions::new().append(true).open(journal)?;
    writeln!(f, "{}", serde_json::to_string(&JournalLine::Undone { ts: now() })?)?;
    Ok(stats)
}

/// Journals found under a destination folder, newest first.
pub fn list_journals(dest: &Path) -> Result<Vec<PathBuf>> {
    let dir = dest.join(".aitrack-organize");
    let mut v: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v.reverse();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::scan::ScanOptions;
    use crate::test_util::write_wav;
    use std::sync::atomic::AtomicBool;

    fn setup() -> (tempfile::TempDir, Library, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("in");
        for (rel, dur) in [
            ("PackA/Kick 1.wav", 0.3),
            ("PackB/Kick 1.wav", 0.4), // same name, different content
            ("PackA/Bass Loop Am 120bpm.wav", 4.0),
            ("PackA/Pad C#maj.wav", 1.0),
        ] {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            write_wav(&p, 22_050, 1, dur, 0.5);
        }
        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        lib.add_root(&root, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| {}, &AtomicBool::new(false))
            .unwrap();
        (dir, lib, root)
    }

    #[test]
    fn plan_copy_and_undo() {
        let (dir, lib, root) = setup();
        let dest = dir.path().join("Organized");
        let plan = lib.plan_organize(&[], &dest, OrganizeLayout::ByKey, OrganizeMode::Copy).unwrap();
        assert_eq!(plan.ops.len(), 4);
        // Dry run touches nothing.
        assert!(!dest.exists());
        let preview = plan.preview();
        assert_eq!(preview.get("Key/Atonal/Drums/Kicks"), Some(&2));
        assert_eq!(preview.get("Key/A Minor/Bass"), Some(&1));
        assert_eq!(preview.get("Key/C# Major/Synths/Pads"), Some(&1));
        assert_eq!(plan.ops.iter().filter(|o| o.renamed).count(), 1);

        let j = plan.execute(&lib).unwrap();
        assert_eq!(j.operations, 4);
        assert!(dest.join("Key/Atonal/Drums/Kicks/Kick 1.wav").is_file());
        assert!(dest.join("Key/Atonal/Drums/Kicks/Kick 1_2.wav").is_file());
        assert!(root.join("PackA/Kick 1.wav").is_file(), "copy keeps originals");

        // Planning again finds identical files: all duplicates.
        let again = lib.plan_organize(&[], &dest, OrganizeLayout::ByKey, OrganizeMode::Copy).unwrap();
        assert_eq!(again.duplicates(), 4);

        let u = undo_organize(&lib, &j.path).unwrap();
        assert_eq!(u.removed_copies, 4);
        assert!(!dest.join("Key").exists(), "empty folders removed");
        assert!(root.join("PackA/Kick 1.wav").is_file());
        assert!(undo_organize(&lib, &j.path).is_err(), "undo only once");
        assert_eq!(list_journals(&dest).unwrap().len(), 1);
    }

    #[test]
    fn move_updates_index_and_undo_restores() {
        let (dir, lib, root) = setup();
        let dest = dir.path().join("ByCat");
        let plan = lib.plan_organize(&[], &dest, OrganizeLayout::ByCategory, OrganizeMode::Move).unwrap();
        let j = plan.execute(&lib).unwrap();
        assert!(!root.join("PackA/Pad C#maj.wav").exists());
        let moved = dest.join("Synths/Pads/Pad C#maj.wav");
        assert!(moved.is_file());
        let s = lib.find_by_path(&moved).unwrap().expect("index follows the move");
        assert_eq!(s.name, "Pad C#maj");
        assert_eq!(lib.count().unwrap(), 4);

        // A file that was modified after the move is left alone.
        let bass = dest.join("Bass/Bass Loop Am 120bpm.wav");
        write_wav(&bass, 22_050, 1, 2.0, 0.1);

        let u = undo_organize(&lib, &j.path).unwrap();
        assert_eq!((u.moved_back, u.skipped), (3, 1));
        assert!(root.join("PackA/Pad C#maj.wav").is_file());
        assert!(bass.is_file());
        let s = lib.find_by_path(&root.join("PackA/Pad C#maj.wav")).unwrap().unwrap();
        assert_eq!(s.name, "Pad C#maj");
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe("A: B?"), "A_ B_");
        assert_eq!(safe(" ."), "_");
    }
}
