//! Projects on disk: New (from a template), Open, Save and Save As over
//! `.aitrack` files (the project as JSON), with the unsaved-changes question
//! before anything that would throw work away.

use crate::workspace::Workspace;
use aitrack_model::Project;
use gpui::{Context, PathPromptOptions, PromptLevel, Window};
use std::path::{Path, PathBuf};

pub const EXTENSION: &str = "aitrack";
const MAX_RECENT_PROJECTS: usize = 12;

fn recent_projects_file() -> PathBuf {
    crate::appearance::config_dir().join("recent_projects.json")
}

/// Load persisted recent project paths (most recent first).
pub fn load_recent_projects() -> Vec<PathBuf> {
    let Ok(bytes) = std::fs::read(recent_projects_file()) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = serde_json::from_slice(&bytes).unwrap_or_default();
    paths.truncate(MAX_RECENT_PROJECTS);
    paths
}

/// Persist `paths` under the app config directory.
pub fn save_recent_projects(paths: &[PathBuf]) {
    let p = recent_projects_file();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(paths) {
        let _ = std::fs::write(p, json);
    }
}

/// Insert `path` at the front of `list`, deduplicating earlier occurrences and
/// bounding the list to `MAX_RECENT_PROJECTS`.
pub fn push_recent_project(list: &mut Vec<PathBuf>, path: &Path) {
    let canonical = std::fs::canonicalize(path).ok();
    list.retain(|existing| {
        if existing == path {
            return false;
        }
        if let Some(ref c) = canonical
            && std::fs::canonicalize(existing).ok().as_ref() == Some(c)
        {
            return false;
        }
        true
    });
    list.insert(0, path.to_path_buf());
    list.truncate(MAX_RECENT_PROJECTS);
}

/// Format `(project_name, compact_parent_dir)` for a recent project menu row.
pub fn recent_project_display(path: &Path) -> (String, String) {
    let name = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    let parent = path.parent().map(compact_dir).unwrap_or_default();
    (name, parent)
}

fn compact_dir(dir: &Path) -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    if let Some(home) = home
        && let Ok(rel) = dir.strip_prefix(&home)
    {
        return if rel.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rel.display())
        };
    }
    dir.display().to_string()
}

/// `name.aitrack` → `name.aitrack.bak`: the file as it was before the last save.
pub fn backup_path(path: &Path) -> PathBuf {
    sibling(path, ".bak")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Write `bytes` to `path` so that a crash or a power cut leaves either the
/// old file or the new one, never half of one: write a temporary file next to
/// it, flush it to the disk (`sync_all`), keep the previous file as
/// `<name>.bak` when `keep_backup`, rename the temporary file over it and flush
/// the directory entry too.
pub fn write_atomic(path: &Path, bytes: &[u8], keep_backup: bool) -> Result<(), String> {
    use std::io::Write;
    let tmp = sibling(path, ".tmp");
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not write {}: {e}", tmp.display())
    })?;
    if keep_backup && path.is_file() {
        // A failed backup must not stop the save: the new file is what matters.
        let _ = std::fs::copy(path, backup_path(path));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not write {}: {e}", path.display())
    })?;
    // Make the rename itself durable (not possible on Windows: opening a folder fails).
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// The folder a project file lives in (absolute), which its relative audio paths are read from.
pub(crate) fn project_folder(path: &Path) -> Option<PathBuf> {
    std::path::absolute(path).ok()?.parent().map(Path::to_path_buf)
}

/// The subfolders next to a project file that belong to the project.
const OWN_FOLDERS: [&str; 2] = ["Samples", "Recordings"];

/// A copy of `project` as it is written in the folder `base`: the audio files in
/// the project's own subfolders (`Samples`, `Recordings`) by relative path, so
/// the folder can be moved or shared, and every other file by its absolute
/// path. The folder of the file itself is not the project's: saving into
/// `~/Music` must not make everything under it "inside the project".
fn relativize_owned(project: &Project, base: &Path) -> Project {
    let mut out = project.clone();
    for source in out.sources.values_mut() {
        if let aitrack_model::Source::Audio(a) = source
            && let Some(rel) = aitrack_model::paths::relative_path(&a.path, base)
            && rel.components().next().is_some_and(|c| OWN_FOLDERS.iter().any(|f| c.as_os_str() == *f))
        {
            a.path = rel;
        }
    }
    out
}

/// Write `project` to `path` (atomically, keeping the previous file as `.bak`).
/// The audio files inside the project's folder go in by relative path, so the
/// folder can be moved or shared; files elsewhere keep their absolute path.
pub fn save(path: &Path, project: &Project) -> Result<(), String> {
    let on_disk = match project_folder(path) {
        Some(base) => relativize_owned(project, &base),
        None => project.clone(),
    };
    let json = on_disk.to_json().map_err(|e| format!("Could not encode the project: {e}"))?;
    write_atomic(path, json.as_bytes(), true)
}

pub fn load(path: &Path) -> Result<Project, String> {
    load_in(path, project_folder(path))
}

/// [`load`] reading relative audio paths from `folder` instead of the file's
/// own (an autosaved copy lives elsewhere than the project it is a copy of).
pub fn load_in(path: &Path, folder: Option<PathBuf>) -> Result<Project, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut project = Project::open_json(&json).map_err(|e| match e {
        // A file that is not a project at all, or one from a newer AI-Track that
        // this version must not overwrite (it would drop what it does not know).
        aitrack_model::LoadError::Malformed(why) => format!("{} is not an AI-Track project: {why}", path.display()),
        aitrack_model::LoadError::MissingVersion | aitrack_model::LoadError::InvalidVersion(_) => format!("{} is not an AI-Track project: {e}", path.display()),
        other => format!("Could not open {}: {other}", path.display()),
    })?;
    if let Some(base) = folder {
        aitrack_model::paths::resolve(&mut project, &base);
    }
    settle_notes(&mut project);
    Ok(project)
}

/// Older projects can have notes clips with an offset (trimmed from the
/// left before trims settled on offset 0). Settle them as they open, so the
/// piano roll shows each clip's own notes; it plays the same.
fn settle_notes(project: &mut Project) {
    let cmds = aitrack_model::ops::rebase_all_notes(project);
    if !cmds.is_empty() {
        // Before any history exists: this is how the project opens, not an edit to undo.
        let _ = aitrack_model::Command::Batch { commands: cmds }.apply(project);
    }
}

/// Copy the audio files outside the project folder `base` into `base/Samples`
/// ("Collect All and Save"), following the model's plan. Returns the commands
/// that point the sources of every file copied at its copy, and what failed.
/// Does file I/O: call it off the UI thread.
pub fn collect_into(project: &Project, base: &Path) -> (Vec<aitrack_model::Command>, Vec<String>) {
    let plan = aitrack_model::paths::collect_plan(project, base, |p| p.exists());
    let mut done = std::collections::BTreeSet::new();
    let mut errors = Vec::new();
    for job in &plan.copies {
        let to = base.join(&job.to);
        let copied = to.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(&job.from, &to).map(|_| ()));
        match copied {
            Ok(()) => done.extend(job.sources.iter().copied()),
            Err(e) => errors.push(format!("{}: {e}", job.from.display())),
        }
    }
    let commands = plan
        .commands
        .into_iter()
        .filter(|c| matches!(c, aitrack_model::Command::SetSourceFile { source, .. } if done.contains(source)))
        .collect();
    (commands, errors)
}

/// Delete the audio takes a project that is never saved leaves behind: the
/// files of `takes` that lie in `unsaved_dir` (where takes go until the project
/// has a file) and that no autosaved copy in `autosave_dir` still plays.
/// Returns how many were deleted. Undoing a take never calls this: its file
/// stays, the redo needs it.
pub fn remove_orphan_takes(takes: &std::collections::BTreeSet<PathBuf>, unsaved_dir: &Path, autosave_dir: &Path) -> usize {
    takes
        .iter()
        .filter(|f| f.starts_with(unsaved_dir) && !crate::autosave::references(autosave_dir, f))
        .filter(|f| std::fs::remove_file(f).is_ok())
        .count()
}

/// Point the sources at the copies [`collect_into`] made, as one undo step.
/// The project may have changed while the files were copied: only the sources
/// that still exist are repointed, and a copy nothing ended up using is
/// deleted. If the edit itself fails, every copy is deleted and the error
/// returned (nothing was collected). Returns how many sources were repointed.
pub fn apply_collected(s: &mut crate::state::AppState, cmds: Vec<aitrack_model::Command>) -> Result<usize, String> {
    use aitrack_model::{Command, Source};
    let dest = |c: &Command| match c {
        Command::SetSourceFile { path, .. } => Some(path.clone()),
        _ => None,
    };
    let alive = |c: &Command| matches!(c, Command::SetSourceFile { source, .. } if matches!(s.project.sources.get(source), Some(Source::Audio(_))));
    let (kept, dropped): (Vec<Command>, Vec<Command>) = cmds.into_iter().partition(|c| alive(c));
    let kept_files: std::collections::BTreeSet<PathBuf> = kept.iter().filter_map(dest).collect();
    // A copy no source will use (and none uses already) is a stray file.
    let used_now = |p: &Path| s.project.sources.values().any(|x| matches!(x, Source::Audio(a) if a.path == p));
    let remove = |files: &mut dyn Iterator<Item = PathBuf>| {
        for f in files {
            let _ = std::fs::remove_file(f);
        }
    };
    let stray: Vec<PathBuf> = dropped.iter().filter_map(dest).filter(|f| !kept_files.contains(f) && !used_now(f)).collect();
    remove(&mut stray.into_iter());
    if kept.is_empty() {
        return Ok(0);
    }
    let n = kept.len();
    s.seal();
    let ok = s.edit("Collect all and save", kept);
    s.seal();
    if ok {
        return Ok(n);
    }
    remove(&mut kept_files.into_iter());
    Err(s.last_error.take().unwrap_or_else(|| "the project changed".to_owned()))
}

/// The audio files under `root` (at most `limit`), for finding lost ones.
pub fn audio_files_under(root: &Path, limit: usize) -> Vec<PathBuf> {
    const AUDIO: [&str; 8] = ["wav", "wave", "aif", "aiff", "flac", "mp3", "ogg", "oga"];
    let (mut out, mut stack) = (Vec::new(), vec![root.to_path_buf()]);
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO.iter().any(|a| e.eq_ignore_ascii_case(a))) {
                out.push(path);
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// The commands that relink `missing` sources to the files in `found` (same
/// file name), and how many stay lost: no candidate, or a source whose hash
/// the file cannot be checked against.
pub fn relink_commands(project: &Project, missing: &[aitrack_model::paths::MissingSource], found: &[PathBuf]) -> (Vec<aitrack_model::Command>, usize) {
    let mut commands = Vec::new();
    for m in missing {
        let relinked = aitrack_model::paths::candidates_for(m, found)
            .into_iter()
            .find_map(|candidate| aitrack_model::paths::relink_source(project, m.source, &candidate, None).ok());
        commands.extend(relinked);
    }
    let lost = missing.len() - commands.len();
    (commands, lost)
}

/// `name.aitrack`, adding the extension when the dialog left it out.
pub fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION)) { path } else { path.with_extension(EXTENSION) }
}

/// What a new project starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Empty,
    Electronic,
    Demo,
}

impl Template {
    pub const ALL: [Template; 3] = [Self::Empty, Self::Electronic, Self::Demo];

    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "Empty project",
            Self::Electronic => "Electronic starter",
            Self::Demo => "Demo song: Midnight Drive",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Empty => "Only the Main track",
            Self::Electronic => "Drums, Bass, Synths, Vocals and FX groups, two returns, limiter on Main",
            Self::Demo => "A finished arrangement to explore",
        }
    }

    pub fn build(self) -> Project {
        match self {
            Self::Empty => crate::demo::empty(),
            Self::Electronic => crate::demo::electronic_starter(),
            Self::Demo => crate::demo::project(),
        }
    }
}

/// What to do once unsaved changes are saved or given up.
#[derive(Clone, Debug, PartialEq)]
enum Then {
    New(Template),
    Open,
    OpenPath(PathBuf),
    /// Save As first (the project had no file), then collect its audio.
    Collect,
}

impl Workspace {
    /// File → New.
    pub fn file_new(&mut self, template: Template, window: &mut Window, cx: &mut Context<Self>) {
        self.guard_unsaved(Then::New(template), window, cx);
    }

    /// File → Open…
    pub fn file_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.guard_unsaved(Then::Open, window, cx);
    }

    /// File → Open Recent → <project>.
    pub fn file_open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.guard_unsaved(Then::OpenPath(path), window, cx);
    }

    /// Record `path` at the top of the recent projects list and persist it.
    pub fn note_recent_project(&mut self, path: &Path) {
        push_recent_project(&mut self.recent_projects, path);
        save_recent_projects(&self.recent_projects);
    }

    /// Remove all entries from the recent projects list.
    pub fn clear_recent_projects(&mut self, cx: &mut Context<Self>) {
        self.recent_projects.clear();
        save_recent_projects(&self.recent_projects);
        cx.notify();
    }

    /// File → Save: to the project's file, or ask for one the first time.
    pub fn file_save(&mut self, cx: &mut Context<Self>) {
        match self.project_path.clone() {
            Some(path) => {
                self.write_project(&path, cx);
            }
            None => self.file_save_as(None, cx),
        }
    }

    /// File → Save As… (`then` continues a New/Open that asked to save first).
    pub fn file_save_as(&mut self, then: Option<Template>, cx: &mut Context<Self>) {
        self.save_as_then(then.map(Then::New), cx);
    }

    fn save_as_then(&mut self, then: Option<Then>, cx: &mut Context<Self>) {
        let dir = self.project_path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(default_folder);
        let name = format!("{}.{EXTENSION}", safe_name(&self.s.project.name));
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update(cx, |ws, cx| {
                    let path = with_extension(path);
                    if ws.write_project(&path, cx)
                        && let Some(then) = then
                    {
                        ws.proceed(then, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Save to `path` and make it the project's file. Returns whether it worked.
    fn write_project(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        // The project takes the file's name, as in every DAW.
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && self.s.project.name != stem
        {
            self.s.seal();
            self.s.edit("Rename project", vec![aitrack_model::Command::RenameProject { name: stem.to_owned() }]);
            self.s.seal();
        }
        // What was changed inside the plugins' own windows lives only in the
        // plugins until now: store it in the project first (no undo step:
        // Cmd+Z after saving still undoes the user's last edit).
        let states = self.b.engine.plugin_states();
        self.s.capture_plugin_states(states);
        match save(path, &self.s.project) {
            Ok(()) => {
                self.s.history.mark_saved();
                // Saved: the project may play the takes recorded so far, wherever they are.
                self.s.session_takes.clear();
                // The file has everything now: no copy of the old state either
                // under the old name (maybe untitled) or the new one.
                self.discard_autosave();
                self.autosaver.discard(&crate::autosave::dir(), &crate::autosave::key(path));
                self.project_path = Some(path.to_path_buf());
                self.note_recent_project(path);
                self.toast(format!("Saved {}", path.file_name().and_then(|n| n.to_str()).unwrap_or("project")), cx);
                true
            }
            Err(e) => {
                self.toast(e, cx);
                false
            }
        }
    }

    /// Housekeeping tick: copy the project in the background when it is due.
    pub(crate) fn autosave_tick(&mut self) {
        let dirty = self.s.history.is_dirty();
        if !self.autosaver.due(std::time::Instant::now(), self.s.revision(), dirty) {
            return;
        }
        // What was changed inside the plugins' own windows lives only in the
        // plugins: take it into the project (no undo step) before the copy is
        // handed to the background thread.
        let states = self.b.engine.plugin_states();
        self.s.capture_plugin_states(states);
        let meta = crate::autosave::Meta { original: self.project_path.clone(), name: self.s.project.name.clone() };
        let key = self.autosaver.key(self.project_path.as_deref());
        let _ = self.autosaver.write_in_background(crate::autosave::dir(), key, meta, self.s.project.clone());
    }

    /// Remove the open project's autosaved copy.
    pub(crate) fn discard_autosave(&mut self) {
        let key = self.autosaver.key(self.project_path.as_deref());
        self.autosaver.discard(&crate::autosave::dir(), &key);
    }

    /// First frame after start-up: if a copy newer than its project was left
    /// behind (the app died or was closed with unsaved work), offer it back.
    pub(crate) fn offer_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut found = std::mem::take(&mut self.recovery);
        if found.is_empty() {
            return;
        }
        let newest = found.remove(0);
        let message = format!("Recover “{}”?", newest.meta.name);
        let detail = format!(
            "AI-Track closed before this project was saved. An automatic copy from {} is available.{}",
            crate::autosave::ago(newest.saved_at, std::time::SystemTime::now()),
            if found.is_empty() { String::new() } else { format!(" {} more project{} will be offered the next time AI-Track starts.", found.len(), if found.len() == 1 { "" } else { "s" }) }
        );
        let rx = window.prompt(PromptLevel::Info, &message, Some(&detail), &["Recover", "Discard"], cx);
        cx.spawn(async move |this, cx| {
            let Ok(answer) = rx.await else { return };
            let _ = this.update(cx, |ws, cx| {
                let dir = crate::autosave::dir();
                if answer == 0 {
                    match load_in(&newest.file, newest.meta.original.as_deref().and_then(project_folder)) {
                        Ok(project) => {
                            ws.recover_project(project, newest.meta.original.clone(), cx);
                            if newest.meta.original.is_none() {
                                // Its copy keeps its name, so the next copies replace it.
                                ws.autosaver.adopt_untitled(newest.key.clone());
                            }
                        }
                        Err(e) => ws.toast(e, cx),
                    }
                } else {
                    ws.autosaver.discard(&dir, &newest.key);
                }
                // The next copy waits for its turn on the next launch.
                ws.recovery.clear();
            });
        })
        .detach();
    }

    /// Open a recovered project: unsaved by definition (one no-op step marks it
    /// so, and the next autosave copies it at once).
    pub(crate) fn recover_project(&mut self, project: Project, original: Option<PathBuf>, cx: &mut Context<Self>) {
        let name = project.name.clone();
        self.replace_project(project, original, cx);
        self.s.edit("Recover autosave", vec![aitrack_model::Command::RenameProject { name }]);
        self.s.seal();
        self.toast("Recovered the autosaved project. Save it to keep it", cx);
    }

    /// Ask about unsaved changes, then carry on with `then`.
    fn guard_unsaved(&mut self, then: Then, window: &mut Window, cx: &mut Context<Self>) {
        if !self.s.history.is_dirty() {
            self.proceed(then, cx);
            return;
        }
        let message = format!("Save the changes to “{}”?", self.s.project.name);
        let rx = window.prompt(PromptLevel::Warning, &message, Some("They are lost if you do not save them."), &["Save", "Don't save", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            let Ok(answer) = rx.await else { return };
            let _ = this.update(cx, |ws, cx| match answer {
                0 => match ws.project_path.clone() {
                    Some(path) => {
                        if ws.write_project(&path, cx) {
                            ws.proceed(then, cx);
                        }
                    }
                    None => ws.save_as_then(Some(then), cx),
                },
                1 => {
                    // Given up on purpose: its autosaved copy goes too, and the
                    // audio it recorded if it never had a file to keep it in.
                    ws.discard_autosave();
                    if ws.project_path.is_none() {
                        let takes = std::mem::take(&mut ws.s.session_takes);
                        remove_orphan_takes(&takes, &crate::state::recordings_dir(None), &crate::autosave::dir());
                    }
                    ws.proceed(then, cx);
                }
                _ => {}
            });
        })
        .detach();
    }

    fn proceed(&mut self, then: Then, cx: &mut Context<Self>) {
        match then {
            Then::New(template) => {
                self.replace_project(template.build(), None, cx);
                self.toast(format!("New: {}", template.label()), cx);
            }
            Then::Open => {
                let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Open project".into()) });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = rx.await
                        && let Some(path) = paths.into_iter().next()
                    {
                        let _ = this.update(cx, |ws, cx| ws.open_loaded_project(path, cx));
                    }
                })
                .detach();
            }
            Then::OpenPath(path) => {
                self.open_loaded_project(path, cx);
            }
            Then::Collect => self.collect_and_save(cx),
        }
    }

    fn open_loaded_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match load(&path) {
            Ok(project) => {
                self.note_recent_project(&path);
                self.replace_project(project, Some(path), cx);
                self.check_missing_files();
            }
            Err(e) => self.toast(e, cx),
        }
    }

    /// Look for audio files the open project points at that are not there; the
    /// first frame after asks what to do ([`offer_relink`](Self::offer_relink)).
    pub(crate) fn check_missing_files(&mut self) {
        self.pending_missing = match self.project_path.as_deref().and_then(project_folder) {
            Some(base) => aitrack_model::paths::missing_sources(&self.s.project, &base, |p| p.exists()),
            None => Vec::new(),
        };
    }

    /// "Some audio files are missing": offer to look for them in a folder.
    pub(crate) fn offer_relink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let missing = std::mem::take(&mut self.pending_missing);
        if missing.is_empty() {
            return;
        }
        let names: Vec<String> = missing.iter().take(5).filter_map(|m| m.path.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        let more = missing.len().saturating_sub(names.len());
        let detail = format!("{}{}", names.join(", "), if more > 0 { format!(" and {more} more") } else { String::new() });
        let message = if missing.len() == 1 { "An audio file is missing".to_owned() } else { format!("{} audio files are missing", missing.len()) };
        let rx = window.prompt(PromptLevel::Warning, &message, Some(&format!("{detail}. Pick the folder they were moved to and AI-Track looks for them by name.")), &["Locate…", "Skip"], cx);
        cx.spawn(async move |this, cx| {
            if rx.await.ok() == Some(0) {
                let _ = this.update(cx, |ws, cx| ws.pick_search_folder(missing, cx));
            }
        })
        .detach();
    }

    fn pick_search_folder(&mut self, missing: Vec<aitrack_model::paths::MissingSource>, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Search in".into()) });
        let epoch = self.project_epoch;
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(root) = paths.into_iter().next() else { return };
            let found = cx.background_executor().spawn(async move { audio_files_under(&root, 200_000) }).await;
            let _ = this.update(cx, |ws, cx| {
                // Another project was opened while the folder was read.
                if epoch != ws.project_epoch {
                    return;
                }
                let (cmds, lost) = relink_commands(&ws.s.project, &missing, &found);
                let n = cmds.len();
                if n > 0 {
                    ws.s.edit("Relink audio files", cmds);
                }
                ws.toast(
                    match (n, lost) {
                        (0, _) => "No file with those names was found in that folder".to_owned(),
                        (_, 0) => format!("Found {n} audio file{}", if n == 1 { "" } else { "s" }),
                        _ => format!("Found {n}; {lost} still missing (File → Find Missing Files to look elsewhere)"),
                    },
                    cx,
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// File → Find Missing Files: the same question as when a project opens.
    pub fn find_missing_files(&mut self, cx: &mut Context<Self>) {
        self.check_missing_files();
        if self.pending_missing.is_empty() {
            self.toast(if self.project_path.is_some() { "No audio file is missing" } else { "Save the project first" }, cx);
        }
        cx.notify();
    }

    /// File → Collect All and Save: copy every audio file the project uses from
    /// outside its folder into `Samples`, point the sources at the copies and
    /// save. A project never saved asks where first.
    pub fn collect_and_save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.project_path.clone() else {
            self.save_as_then(Some(Then::Collect), cx);
            return;
        };
        let Some(base) = project_folder(&path) else { return };
        self.toast("Collecting audio files…", cx);
        let project = self.s.project.clone();
        let epoch = self.project_epoch;
        let task = cx.background_executor().spawn(async move { collect_into(&project, &base) });
        cx.spawn(async move |this, cx| {
            let (cmds, errors) = task.await;
            let _ = this.update(cx, |ws, cx| {
                if epoch != ws.project_epoch {
                    return;
                }
                // The project may have changed while the files were copied.
                let n = match apply_collected(&mut ws.s, cmds) {
                    Ok(n) => n,
                    Err(e) => {
                        ws.toast(format!("Could not collect the audio files ({e}); the copies were removed and the project was not saved"), cx);
                        return;
                    }
                };
                if ws.write_project(&path, cx) {
                    ws.toast(
                        match (n, errors.len()) {
                            (0, 0) => "Nothing to collect: every audio file is already in the project folder".to_owned(),
                            (_, 0) => format!("Collected {n} audio file{} into Samples and saved", if n == 1 { "" } else { "s" }),
                            _ => format!("Collected {n}; {} could not be copied ({})", errors.len(), errors[0]),
                        },
                        cx,
                    );
                }
            });
        })
        .detach();
    }
}

/// Where the first Save As opens: the user's music or documents folder.
pub(crate) fn default_folder() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    [home.join("Music"), home.join("Documents"), home.clone()].into_iter().find(|p| p.is_dir()).unwrap_or(home)
}

/// Where Consolidate writes the audio it renders: `Samples/Consolidated`
/// next to the saved project, else `AI-Track/Consolidated` in the user's
/// music folder (the files stay where they are when the project is saved
/// later).
pub fn consolidate_folder(project_path: Option<&Path>) -> PathBuf {
    match project_path.and_then(Path::parent) {
        Some(dir) => dir.join("Samples").join("Consolidated"),
        None => default_folder().join("AI-Track").join("Consolidated"),
    }
}

/// A `.wav` path in `dir` named after `name` that no file (nor `taken`) has yet.
pub fn fresh_wav(dir: &Path, name: &str, taken: &[&Path]) -> PathBuf {
    let base = safe_name(name);
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{base} Consolidated.wav") } else { format!("{base} Consolidated {n}.wav") }))
        .find(|p| !p.exists() && !taken.contains(&p.as_path()))
        .expect("a free name")
}

/// A project name that is a valid file name on Windows and macOS.
pub(crate) fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '-' } else { c }).collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() { "Untitled".to_owned() } else { trimmed.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("aitrack-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = with_extension(dir.join("Night Song"));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some(EXTENSION));
        let project = crate::demo::project();
        save(&path, &project).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.tracks, project.tracks);
        assert_eq!(back.clips, project.clips);
        assert!(!path.with_extension(format!("{EXTENSION}.tmp")).exists(), "no temporary file is left behind");
        assert!(!backup_path(&path).exists(), "the first save has nothing to back up");
        std::fs::write(&path, "not a project").unwrap();
        assert!(load(&path).unwrap_err().contains("is not an AI-Track project"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_takes_of_a_project_given_up_unsaved_are_removed_unless_a_copy_still_plays_them() {
        let root = std::env::temp_dir().join(format!("aitrack-io-takes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (rec, copies, other) = (root.join("Recordings"), root.join("Autosave"), root.join("elsewhere"));
        for d in [&rec, &copies, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (gone, kept, outside) = (rec.join("a.wav"), rec.join("b.wav"), other.join("c.wav"));
        for f in [&gone, &kept, &outside] {
            std::fs::write(f, b"x").unwrap();
        }
        // A copy of another project still plays b.wav.
        let other_project = with_sample(&kept);
        crate::autosave::write(&copies, "someone-else", &crate::autosave::Meta { original: None, name: "x".into() }, &other_project.to_json().unwrap()).unwrap();
        let takes = [gone.clone(), kept.clone(), outside.clone()].into_iter().collect();
        assert_eq!(remove_orphan_takes(&takes, &rec, &copies), 1);
        assert!(!gone.exists() && kept.exists() && outside.exists(), "only the unreferenced take in the unsaved folder goes");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn undoing_a_take_keeps_its_file_for_the_redo() {
        use crate::bridge::AudioTake;
        let dir = std::env::temp_dir().join(format!("aitrack-io-undo-take-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("take.wav");
        std::fs::write(&wav, b"x").unwrap();
        let mut s = crate::state::AppState::new(Project::new("t"));
        let t = s.add_track(aitrack_model::TrackKind::Audio).unwrap();
        s.set_track(t, aitrack_model::TrackProp::Input(aitrack_model::InputSource::Hardware { first: 0, stereo: false }));
        let take = AudioTake { track: t, path: wav.clone(), start_tick: 0, frames: 48_000, sample_rate: 48_000, channels: 1, error: None };
        assert_eq!(s.commit_audio_takes(&[take], None).0, 1);
        assert!(s.session_takes.contains(&wav));
        s.undo();
        assert!(wav.is_file(), "the redo needs it");
        s.redo();
        assert_eq!(s.project.track(t).unwrap().arrangement.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_again_keeps_the_previous_file_as_a_backup() {
        let dir = std::env::temp_dir().join(format!("aitrack-io-bak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = with_extension(dir.join("Song"));
        let mut project = crate::demo::project();
        project.name = "First".into();
        save(&path, &project).unwrap();
        project.name = "Second".into();
        save(&path, &project).unwrap();
        assert_eq!(backup_path(&path).file_name().and_then(|n| n.to_str()), Some("Song.aitrack.bak"));
        assert_eq!(load(&backup_path(&path)).unwrap().name, "First", "the .bak is the file before the last save");
        assert_eq!(load(&path).unwrap().name, "Second");
        assert!(!sibling(&path, ".tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_from_a_newer_version_is_refused_and_left_alone() {
        let dir = std::env::temp_dir().join(format!("aitrack-io-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = with_extension(dir.join("Future"));
        let mut v: serde_json::Value = serde_json::from_str(&crate::demo::project().to_json().unwrap()).unwrap();
        v["format_version"] = 99.into();
        let text = serde_json::to_string(&v).unwrap();
        std::fs::write(&path, &text).unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.contains("newer version of AI-Track") && err.contains("99"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "opening never rewrites the file");
        // A file with no version is not one of ours either.
        v.as_object_mut().unwrap().remove("format_version");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(load(&path).unwrap_err().contains("is not an AI-Track project"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn with_sample(path: &Path) -> Project {
        let mut p = Project::new("t");
        let id = p.alloc_source_id();
        p.sources.insert(id, aitrack_model::Source::Audio(aitrack_model::AudioSource {
            path: path.to_path_buf(),
            hash: None,
            sample_rate: 48_000,
            channels: 2,
            frames: 48_000,
            warp: aitrack_model::Warp::default(),
            key: None,
        }));
        p
    }

    fn audio_paths(p: &Project) -> Vec<PathBuf> {
        p.sources.values().filter_map(|s| if let aitrack_model::Source::Audio(a) = s { Some(a.path.clone()) } else { None }).collect()
    }

    #[test]
    fn a_project_folder_can_be_moved_and_every_file_inside_it_is_still_found() {
        let root = std::env::temp_dir().join(format!("aitrack-io-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, elsewhere) = (root.join("song"), root.join("library"));
        std::fs::create_dir_all(home.join("Samples")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (inside, outside) = (home.join("Samples").join("kick.wav"), elsewhere.join("pad.wav"));
        let project = with_sample(&inside);
        let mut project = with_sample_again(project, &outside);
        project.name = "Song".into();
        let path = home.join("Song.aitrack");
        save(&path, &project).unwrap();
        // On disk the file inside the folder is relative, the other one is not.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"Samples/kick.wav\"") && !text.contains(&inside.display().to_string()));
        assert!(text.contains(&json_path(&outside)));
        // Move the folder and open it from the new place.
        let moved = root.join("moved");
        std::fs::rename(&home, &moved).unwrap();
        let back = load(&moved.join("Song.aitrack")).unwrap();
        let paths = audio_paths(&back);
        assert!(paths.contains(&moved.join("Samples").join("kick.wav")), "{paths:?}");
        assert!(paths.contains(&outside), "a file elsewhere keeps its path");
        // The project in memory is never touched by saving.
        assert!(audio_paths(&project).contains(&inside));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A path as it appears inside the saved JSON (Windows backslashes are escaped there).
    fn json_path(p: &Path) -> String {
        let quoted = serde_json::to_string(&p.display().to_string()).unwrap();
        quoted[1..quoted.len() - 1].to_owned()
    }

    fn with_sample_again(mut p: Project, path: &Path) -> Project {
        let id = p.alloc_source_id();
        p.sources.insert(id, aitrack_model::Source::Audio(aitrack_model::AudioSource {
            path: path.to_path_buf(),
            hash: None,
            sample_rate: 48_000,
            channels: 2,
            frames: 48_000,
            warp: aitrack_model::Warp::default(),
            key: None,
        }));
        p
    }

    #[test]
    fn only_files_in_the_projects_own_subfolders_are_saved_relative() {
        let root = std::env::temp_dir().join(format!("aitrack-io-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let music = root.join("Music");
        std::fs::create_dir_all(&music).unwrap();
        let paths = [music.join("Samples").join("kick.wav"), music.join("Recordings").join("take.wav"), music.join("loose.wav"), music.join("Other Song").join("pad.wav")];
        let mut project = Project::new("t");
        for p in &paths {
            project = with_sample_again(project, p);
        }
        let file = music.join("Song.aitrack");
        save(&file, &project).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"Samples/kick.wav\"") && text.contains("\"Recordings/take.wav\""), "own folders: relative");
        assert!(text.contains(&json_path(&music.join("loose.wav"))), "next to the file: absolute");
        assert!(text.contains(&json_path(&music.join("Other Song").join("pad.wav"))), "another project's folder: absolute");
        // Only the project file moves: what it did not own still plays.
        let elsewhere = root.join("Desktop");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::rename(&file, elsewhere.join("Song.aitrack")).unwrap();
        let back = audio_paths(&load(&elsewhere.join("Song.aitrack")).unwrap());
        assert!(back.contains(&music.join("loose.wav")) && back.contains(&music.join("Other Song").join("pad.wav")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_all_copies_outside_files_into_samples_and_keeps_the_project_working() {
        let root = std::env::temp_dir().join(format!("aitrack-io-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, elsewhere) = (root.join("song"), root.join("library"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (a, b) = (elsewhere.join("loop.wav"), elsewhere.join("gone.wav"));
        std::fs::write(&a, b"RIFFdata").unwrap();
        let project = with_sample_again(with_sample(&a), &b); // b does not exist
        let (cmds, errors) = collect_into(&project, &home);
        assert_eq!(cmds.len(), 1, "only the file that could be copied is repointed");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(std::fs::read(home.join("Samples").join("loop.wav")).unwrap(), b"RIFFdata");
        let mut s = crate::state::AppState::new(project);
        assert!(s.edit("Collect all and save", cmds));
        let path = home.join("Song.aitrack");
        save(&path, &s.project).unwrap();
        // Moved away from the library entirely, the collected file is still there.
        let moved = root.join("elsewhere-again");
        std::fs::rename(&home, &moved).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
        let back = load(&moved.join("Song.aitrack")).unwrap();
        assert!(audio_paths(&back).iter().any(|p| p == &moved.join("Samples").join("loop.wav") && p.is_file()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collecting_after_the_project_changed_repoints_what_is_left_and_removes_stray_copies() {
        let root = std::env::temp_dir().join(format!("aitrack-io-collect2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, elsewhere) = (root.join("song"), root.join("library"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (a, b) = (elsewhere.join("a.wav"), elsewhere.join("b.wav"));
        std::fs::write(&a, b"A").unwrap();
        std::fs::write(&b, b"B").unwrap();
        let project = with_sample_again(with_sample(&a), &b);
        let ids: Vec<_> = project.sources.keys().copied().collect();
        let (cmds, errors) = collect_into(&project, &home);
        assert!(errors.is_empty() && cmds.len() == 2);
        // Meanwhile the user deleted one source: its copy is a stray file.
        let mut s = crate::state::AppState::new(project);
        assert!(s.edit("delete", vec![aitrack_model::Command::RemoveSource { id: ids[1] }]));
        s.seal();
        assert_eq!(apply_collected(&mut s, cmds.clone()), Ok(1), "only the source that is still there");
        assert!(home.join("Samples").join("a.wav").is_file());
        assert!(!home.join("Samples").join("b.wav").exists(), "the copy nothing uses is gone");
        assert_eq!(audio_paths(&s.project), [home.join("Samples").join("a.wav")]);
        // Every source gone: nothing is applied, nothing is left behind, no undo step.
        let mut gone = crate::state::AppState::new(Project::new("t"));
        let steps = gone.history.steps().count();
        let again = collect_into(&with_sample(&a), &home).0;
        assert!(home.join("Samples").join("a 2.wav").is_file(), "a second copy under a new name");
        assert_eq!(apply_collected(&mut gone, again), Ok(0));
        assert!(!home.join("Samples").join("a 2.wav").exists());
        assert_eq!(gone.history.steps().count(), steps);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lost_files_are_found_by_name_in_a_folder_and_relinked_in_one_step() {
        let root = std::env::temp_dir().join(format!("aitrack-io-relink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let deep = root.join("backup").join("2023");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("Kick.WAV"), b"x").unwrap();
        std::fs::write(deep.join("notes.txt"), b"x").unwrap();
        let base = root.join("song");
        let lost = base.join("Samples").join("kick.wav");
        let project = with_sample_again(with_sample(&lost), &base.join("Samples").join("nowhere.wav"));
        let missing = aitrack_model::paths::missing_sources(&project, &base, |p| p.exists());
        assert_eq!(missing.len(), 2);
        let found = audio_files_under(&root, 1000);
        assert_eq!(found.len(), 1, "only audio files: {found:?}");
        let (cmds, lost_still) = relink_commands(&project, &missing, &found);
        assert_eq!((cmds.len(), lost_still), (1, 1), "same name in any case; the other has no candidate");
        let mut s = crate::state::AppState::new(project);
        assert!(s.edit("Relink audio files", cmds));
        assert!(audio_paths(&s.project).contains(&deep.join("Kick.WAV")));
        assert_eq!(aitrack_model::paths::missing_sources(&s.project, &base, |p| p.exists()).len(), 1);
        s.undo();
        assert_eq!(aitrack_model::paths::missing_sources(&s.project, &base, |p| p.exists()).len(), 2, "undo loses it again");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shaped_clips_round_trip_and_files_saved_before_fades_still_load() {
        use aitrack_model::{AutomationLane, AutomationPoint, AutomationTarget, Fade, LoopRegion};
        let dir = std::env::temp_dir().join(format!("aitrack-io-fades-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = crate::demo::project();
        // A project as it was saved before clips had fades: the same file without the two keys.
        fn strip(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(map) => {
                    map.remove("fade_in");
                    map.remove("fade_out");
                    map.values_mut().for_each(strip);
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
                _ => {}
            }
        }
        let mut old: serde_json::Value = serde_json::from_str(&plain.to_json().unwrap()).unwrap();
        strip(&mut old);
        let text = serde_json::to_string(&old).unwrap();
        assert!(!text.contains("fade_in") && !plain.clips.is_empty());
        let path = with_extension(dir.join("Old"));
        std::fs::write(&path, text).unwrap();
        let back = load(&path).unwrap();
        // A relative path in the file is read from the project's folder.
        let mut expected = plain.clone();
        aitrack_model::paths::resolve(&mut expected, &dir);
        assert_eq!((back.clips, back.tracks, back.sources), (plain.clips.clone(), plain.tracks.clone(), expected.sources));
        // Saved now, a clip with fades, a volume envelope and a loop comes back equal.
        let mut shaped = plain;
        let clip = shaped.clips.values_mut().next().unwrap();
        clip.fade_in = Fade { length: 120, curve: 0.5 };
        clip.fade_out = Fade { length: 240, curve: -0.75 };
        clip.looping = Some(LoopRegion { start: 0, end: clip.length / 2 });
        clip.envelopes.push(AutomationLane { points: vec![AutomationPoint { tick: 0, value: 0.25, curve: 0.3 }, AutomationPoint { tick: 480, value: 1., curve: 0. }], ..AutomationLane::new(AutomationTarget::Volume) });
        let path = with_extension(dir.join("New"));
        save(&path, &shaped).unwrap();
        let back = load(&path).unwrap();
        let mut expected = shaped.clone();
        aitrack_model::paths::resolve(&mut expected, &dir);
        assert_eq!((back.clips, back.tracks, back.sources), (shaped.clips, shaped.tracks, expected.sources));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn templates_build_and_are_organised() {
        for t in Template::ALL {
            let p = t.build();
            assert_eq!(p.tracks.iter().filter(|t| t.kind == aitrack_model::TrackKind::Main).count(), 1, "{}", t.label());
        }
        let p = Template::Electronic.build();
        let groups: Vec<&str> = p.tracks.iter().filter(|t| t.kind == aitrack_model::TrackKind::Group).map(|t| t.name.as_str()).collect();
        assert_eq!(groups, ["Drums", "Bass", "Synths", "Vocals", "FX"]);
        // Every track inside a group has the group's colour and routes into it.
        for t in p.tracks.iter().filter(|t| t.parent.is_some()) {
            let g = p.track(t.parent.unwrap()).unwrap();
            assert_eq!(t.color, g.color, "{} matches {}", t.name, g.name);
            assert_eq!(t.output, aitrack_model::Output::Track(g.id));
            assert_eq!(t.sends.len(), 2, "{} has both sends", t.name);
        }
        assert!(p.clips.is_empty(), "a template has no clips");
        assert!(p.main_track().devices.iter().any(|d| d.name == "Limiter"));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_name("Drop 2: final?"), "Drop 2- final-");
        assert_eq!(safe_name("  "), "Untitled");
    }

    #[test]
    fn recent_projects_dedupe_promote_and_cap() {
        let mut recent = Vec::new();
        for i in 0..15 {
            push_recent_project(&mut recent, Path::new(&format!("/tmp/Project {i}.aitrack")));
        }
        assert_eq!(recent.len(), MAX_RECENT_PROJECTS);
        assert_eq!(recent[0], PathBuf::from("/tmp/Project 14.aitrack"));
        // Re-opening an older entry promotes it without duplication.
        push_recent_project(&mut recent, Path::new("/tmp/Project 10.aitrack"));
        assert_eq!(recent.len(), MAX_RECENT_PROJECTS);
        assert_eq!(recent[0], PathBuf::from("/tmp/Project 10.aitrack"));
        assert_eq!(recent.iter().filter(|p| p.as_path() == Path::new("/tmp/Project 10.aitrack")).count(), 1);
    }

    #[test]
    fn recent_project_display_extracts_name_and_parent() {
        let (name, parent) = recent_project_display(Path::new("/tmp/Songs/Midnight Drive.aitrack"));
        assert_eq!(name, "Midnight Drive");
        assert_eq!(parent, "/tmp/Songs");
    }
}
