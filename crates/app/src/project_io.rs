//! Projects on disk: New (from a template), Open, Save and Save As over
//! `.aitrack` files (the project as JSON), with the unsaved-changes question
//! before anything that would throw work away.

use crate::workspace::Workspace;
use aitrack_model::Project;
use gpui::{Context, PathPromptOptions, PromptLevel, Window};
use std::path::{Path, PathBuf};

pub const EXTENSION: &str = "aitrack";

/// Write `project` to `path` through a temporary file, so a failed write
/// never leaves half a project behind.
pub fn save(path: &Path, project: &Project) -> Result<(), String> {
    let json = project.to_json().map_err(|e| format!("Could not encode the project: {e}"))?;
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    std::fs::write(&tmp, json).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not write {}: {e}", path.display())
    })
}

pub fn load(path: &Path) -> Result<Project, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut project = Project::from_json(&json).map_err(|e| format!("{} is not an AI-Track project: {e}", path.display()))?;
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
        // plugins until now: store it in the project first.
        let states = self.b.engine.plugin_states();
        self.s.store_plugin_states(states);
        match save(path, &self.s.project) {
            Ok(()) => {
                self.s.history.mark_saved();
                self.project_path = Some(path.to_path_buf());
                self.toast(format!("Saved {}", path.file_name().and_then(|n| n.to_str()).unwrap_or("project")), cx);
                true
            }
            Err(e) => {
                self.toast(e, cx);
                false
            }
        }
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
                1 => ws.proceed(then, cx),
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
                        let _ = this.update(cx, |ws, cx| match load(&path) {
                            Ok(project) => {
                                ws.replace_project(project, Some(path), cx);
                            }
                            Err(e) => ws.toast(e, cx),
                        });
                    }
                })
                .detach();
            }
        }
    }
}

/// Where the first Save As opens: the user's music or documents folder.
fn default_folder() -> PathBuf {
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
fn safe_name(name: &str) -> String {
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
        std::fs::write(&path, "not a project").unwrap();
        assert!(load(&path).unwrap_err().contains("is not an AI-Track project"));
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!((back.clips, back.tracks, back.sources), (plain.clips.clone(), plain.tracks.clone(), plain.sources.clone()));
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
        assert_eq!((back.clips, back.tracks, back.sources), (shaped.clips, shaped.tracks, shaped.sources));
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
}
