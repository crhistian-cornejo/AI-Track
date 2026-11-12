//! The assistant's state and what it does: one chat, the `/` actions, the
//! attachments, the model choice and the streaming request.
//!
//! The state lives in an entity kept as a GPUI global, so the panel needs no
//! new field on the workspace. Chat items are anchored to positions in
//! `Workspace::chat_log` (job, proposal, licence and sample cards land there)
//! so both read as one timeline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aitrack_ai::llm::{self, context::Focus, mix::MixPlan};
use aitrack_model::{ClipId, Origin, Source, TrackId};
use gpui::{AppContext, ClipboardEntry, Context, Entity, Global, ImageFormat, ScrollHandle, Subscription, Task};

use super::{mix, slash};
use crate::bridge::{AgentKind, LibraryItem};
use crate::icons::Icon;
use crate::views::browser::short_key;
use crate::widgets::{InputEvent, TextInput};
use crate::workspace::{ChatEntry, ProposalStatus, Workspace};

/// Who answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// A Claude model through the Anthropic API (index into `llm::MODELS`).
    Claude(usize),
    /// The user's own agent CLI (Claude Code, Codex).
    Agent(AgentKind),
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Claude(i) => llm::MODELS[i].name,
            Backend::Agent(a) => a.name(),
        }
    }

    /// The id saved in the preferences (`Appearance::assistant_model`).
    pub fn pref_id(self) -> &'static str {
        match self {
            Backend::Claude(i) => llm::MODELS[i].id,
            Backend::Agent(AgentKind::ClaudeCode) => "agent:claude-code",
            Backend::Agent(AgentKind::Codex) => "agent:codex",
        }
    }

    /// The saved choice; unknown or empty ids give the default model.
    pub fn from_pref(id: &str) -> Self {
        if let Some(i) = llm::MODELS.iter().position(|m| m.id == id) {
            return Backend::Claude(i);
        }
        AgentKind::ALL.into_iter().map(Backend::Agent).find(|b| b.pref_id() == id).unwrap_or(Backend::Claude(0))
    }
}

/// Where the API key comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Checking,
    Env,
    Keychain,
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReplyState {
    /// Sent; no token yet (the thinking loader).
    Waiting,
    Streaming,
    Done,
    Stopped,
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Item {
    User { text: String, chips: Vec<(Icon, String)> },
    Reply { text: String, model: &'static str, state: ReplyState, mix: Option<TrackId> },
    /// A `/` action that ran: its card, followed by its job/result cards.
    Action { icon: Icon, title: &'static str, detail: String },
    /// A short line from the assistant itself (no model involved).
    Note(String),
}

#[derive(Clone, Debug)]
pub struct Placed {
    /// `chat_log.len()` when the item was added: it renders before that entry.
    pub anchor: usize,
    pub item: Item,
}

#[derive(Clone, Debug)]
pub enum Attachment {
    Track(TrackId, String),
    Clip(ClipId, String),
    Sample { name: String, line: String },
    /// A screenshot; `data` is `None` while the file is read.
    Image { name: String, media_type: &'static str, data: Option<Arc<Vec<u8>>> },
}

impl Attachment {
    pub fn chip(&self) -> (Icon, String) {
        match self {
            Attachment::Track(_, n) => (Icon::Layers, n.clone()),
            Attachment::Clip(_, n) => (Icon::Waveform, n.clone()),
            Attachment::Sample { name, .. } => (Icon::Sample, name.clone()),
            Attachment::Image { name, data, .. } => (Icon::File, if data.is_some() { name.clone() } else { format!("{name}…") }),
        }
    }

    fn same(&self, other: &Attachment) -> bool {
        match (self, other) {
            (Attachment::Track(a, _), Attachment::Track(b, _)) => a == b,
            (Attachment::Clip(a, _), Attachment::Clip(b, _)) => a == b,
            (Attachment::Sample { line: a, .. }, Attachment::Sample { line: b, .. }) => a == b,
            _ => false,
        }
    }
}

struct Live {
    item: usize,
    stream: llm::Stream,
}

pub struct Assistant {
    pub input: Entity<TextInput>,
    pub items: Vec<Placed>,
    pub attachments: Vec<Attachment>,
    pub backend: Backend,
    pub key: KeyState,
    pub model_menu: bool,
    pub slash_sel: usize,
    pub slash_tab: slash::Tab,
    pub scroll: ScrollHandle,
    live: Option<Live>,
    poll: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Assistant {
    /// Whether a reply is on its way.
    pub fn streaming(&self) -> bool {
        self.live.is_some()
    }

    pub fn input_text(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_owned()
    }
}

struct Handle(Entity<Assistant>);
impl Global for Handle {}

/// Whether the assistant's text field has the keys.
pub fn input_focused(window: &gpui::Window, cx: &gpui::App) -> bool {
    cx.try_global::<Handle>().is_some_and(|h| gpui::Focusable::focus_handle(h.0.read(cx).input.read(cx), cx).is_focused(window))
}

/// The assistant, created on first use.
pub fn get(cx: &mut Context<Workspace>) -> Entity<Assistant> {
    if let Some(h) = cx.try_global::<Handle>() {
        return h.0.clone();
    }
    let input = cx.new(|cx| TextInput::new(cx, "Ask, or type / for actions", Some(Icon::Sparkles)));
    let sub = cx.subscribe(&input, |ws, _, e: &InputEvent, cx| on_input(ws, e, cx));
    let entity = cx.new(|_| Assistant {
        input,
        items: Vec::new(),
        attachments: Vec::new(),
        backend: Backend::from_pref(&crate::appearance::Appearance::load().assistant_model),
        key: if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty()) { KeyState::Env } else { KeyState::Checking },
        model_menu: false,
        slash_sel: 0,
        slash_tab: slash::Tab::All,
        scroll: ScrollHandle::new(),
        live: None,
        poll: None,
        _subs: vec![sub],
    });
    cx.set_global(Handle(entity.clone()));
    refresh_key(cx);
    entity
}

/// Look for an API key off the UI thread (the keychain may block).
pub fn refresh_key(cx: &mut Context<Workspace>) {
    let Some(a) = cx.try_global::<Handle>().map(|h| h.0.clone()) else { return };
    let task = cx.background_executor().spawn(async {
        if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty()) {
            KeyState::Env
        } else if llm::keychain::api_key().is_some() {
            KeyState::Keychain
        } else {
            KeyState::Missing
        }
    });
    cx.spawn(async move |ws, cx| {
        let k = task.await;
        a.update(cx, |a, _| a.key = k);
        let _ = ws.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

fn anchor(ws: &Workspace) -> usize {
    ws.chat_log.len()
}

fn push(ws: &Workspace, a: &mut Assistant, item: Item) -> usize {
    a.items.push(Placed { anchor: anchor(ws), item });
    a.scroll.scroll_to_bottom();
    a.items.len() - 1
}

pub fn note(ws: &mut Workspace, text: impl Into<String>, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let text = text.into();
    a.update(cx, |a, _| push(ws, a, Item::Note(text)));
    cx.notify();
}

fn on_input(ws: &mut Workspace, e: &InputEvent, cx: &mut Context<Workspace>) {
    let a = get(cx);
    match e {
        InputEvent::Changed(_) => {
            a.update(cx, |a, _| a.slash_sel = 0);
            cx.notify();
        }
        InputEvent::Submit(text) => {
            let text = text.trim().to_owned();
            if let Some(q) = slash::query(&text) {
                let (sel, tab) = a.read_with(cx, |a, _| (a.slash_sel, a.slash_tab));
                let hits = slash::filter(q, tab);
                if let Some(entry) = hits.get(sel.min(hits.len().saturating_sub(1))) {
                    choose(ws, entry.action, cx);
                }
            } else if !text.is_empty() {
                clear_input(&a, cx);
                send(ws, text, None, cx);
            }
        }
    }
}

fn clear_input(a: &Entity<Assistant>, cx: &mut Context<Workspace>) {
    let input = a.read(cx).input.clone();
    input.update(cx, |i, cx| i.set_text("", cx));
}

/// Up / Down in the `/` menu.
pub fn move_selection(ws: &mut Workspace, delta: i32, cx: &mut Context<Workspace>) -> bool {
    let a = get(cx);
    let text = a.read(cx).input_text(cx);
    let Some(q) = slash::query(&text) else { return false };
    let n = slash::filter(q, a.read(cx).slash_tab).len();
    if n == 0 {
        return true;
    }
    a.update(cx, |a, _| a.slash_sel = (a.slash_sel as i32 + delta).rem_euclid(n as i32) as usize);
    let _ = ws;
    cx.notify();
    true
}

pub fn set_tab(tab: slash::Tab, cx: &mut Context<Workspace>) {
    let a = get(cx);
    a.update(cx, |a, _| {
        a.slash_tab = tab;
        a.slash_sel = 0;
    });
    cx.notify();
}

/// Run a `/` entry chosen from the menu (Enter or click).
pub fn choose(ws: &mut Workspace, action: slash::Action, cx: &mut Context<Workspace>) {
    let a = get(cx);
    clear_input(&a, cx);
    run(ws, action, cx);
}

/// The selected audio clip (else the first in the project) and its file.
fn audio_target(ws: &Workspace) -> Option<(String, PathBuf)> {
    let p = &ws.s.project;
    let audio_of = |clip: &ClipId| {
        let c = p.clips.get(clip)?;
        match p.sources.get(&c.source)? {
            Source::Audio(a) => Some((c.name.clone(), a.path.clone())),
            _ => None,
        }
    };
    ws.s.selected_clip.as_ref().and_then(audio_of).or_else(|| p.clips.keys().find_map(audio_of))
}

/// The track "this track" means: an attached one, else the selection.
fn target_track(ws: &Workspace, a: &Assistant) -> Option<TrackId> {
    a.attachments.iter().find_map(|x| if let Attachment::Track(id, _) = x { Some(*id) } else { None }).or(ws.s.selected_track).filter(|id| ws.s.project.track(*id).is_some())
}

pub fn run(ws: &mut Workspace, action: slash::Action, cx: &mut Context<Workspace>) {
    use slash::Action as A;
    let a = get(cx);
    let entry = slash::ENTRIES.iter().find(|e| e.action == action).copied();
    let card = |ws: &Workspace, cx: &mut Context<Workspace>, detail: String| {
        if let Some(e) = entry {
            a.update(cx, |a, _| push(ws, a, Item::Action { icon: e.icon, title: e.title, detail }));
        }
    };
    match action {
        A::ExtractMidi(mode) => {
            let Some((name, _)) = audio_target(ws) else { return note(ws, "Select an audio clip first, then run /midi again.", cx) };
            debug_assert!(crate::bridge::MidiMode::ALL.contains(&mode));
            card(ws, cx, format!("{name} · {}", mode.label()));
            ws.midi_mode = mode;
            ws.extract_midi(cx);
        }
        A::SplitStems => {
            let Some((name, _)) = audio_target(ws) else { return note(ws, "Select an audio clip first, then run /stems again.", cx) };
            card(ws, cx, name);
            ws.split_stems(cx);
        }
        A::ImportAudio => {
            card(ws, cx, "Pick a file; it lands on a new track at the playhead".into());
            ws.pick_audio_file(cx);
        }
        A::AnalyseKeyBpm => {
            let Some((name, path)) = audio_target(ws) else { return note(ws, "Select an audio clip first, then run /analyse again.", cx) };
            card(ws, cx, name.clone());
            let id = ws.b.ai.analyze(&path);
            ws.chat_log.push(ChatEntry::Job { id, label: format!("Analysing {name} (key, BPM)"), fraction: 0., done: false });
            ws.ensure_ai_poll(cx);
        }
        A::FindSamplesInKey => {
            card(ws, cx, ws.s.project.scale.map_or_else(|| "The project has no key yet".to_owned(), |s| s.display()));
            ws.find_samples_in_key(cx);
        }
        A::MixTrack => mix_track(ws, cx),
        A::AttachSelection => attach_selection(ws, cx),
        A::AttachFile => pick_files(cx),
        A::Overview => send(ws, "Give me a quick overview of this project: what is there, what stands out, and three concrete things to try next.".into(), None, cx),
        A::ClearChat => {
            stop(ws, cx);
            a.update(cx, |a, _| {
                a.items.clear();
                a.attachments.clear();
            });
            // Keep cards still waiting for an answer (proposals, licences).
            ws.chat_log.retain(|e| matches!(e, ChatEntry::Proposal { status: ProposalStatus::Pending, .. } | ChatEntry::License { status: ProposalStatus::Pending, .. }));
        }
    }
    cx.notify();
}

fn mix_track(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let Some(track) = target_track(ws, a.read(cx)) else { return note(ws, "Select a track (or attach one) and run /mix again.", cx) };
    let name = ws.s.project.track(track).map(|t| t.name.clone()).unwrap_or_default();
    let (backend, key) = a.read_with(cx, |a, _| (a.backend, a.key));
    match backend {
        Backend::Claude(_) if key != KeyState::Missing => send(ws, format!("Mix this track: {name}."), Some(track), cx),
        _ => {
            let entry = slash::ENTRIES.iter().find(|e| e.action == slash::Action::MixTrack).copied();
            if let Some(e) = entry {
                a.update(cx, |a, _| push(ws, a, Item::Action { icon: e.icon, title: e.title, detail: format!("{name} · by rules (no model connected)") }));
            }
            let Some(t) = ws.s.project.track(track) else { return };
            let plan = llm::mix::heuristic(t);
            propose_mix(ws, track, plan, Origin::Feature("mix".into()), "Rules", cx);
        }
    }
}

/// Offer `plan` for `track` as one proposal card.
fn propose_mix(ws: &mut Workspace, track: TrackId, plan: MixPlan, origin: Origin, by: &str, cx: &mut Context<Workspace>) {
    let name = ws.s.project.track(track).map(|t| t.name.clone()).unwrap_or_default();
    let cmds = mix::commands(&mut ws.s.project, track, &plan);
    if cmds.is_empty() {
        return note(ws, format!("{name} already sits well; nothing to change."), cx);
    }
    let label = format!("Mix {name}");
    let Some(id) = ws.s.propose(&label, origin, cmds) else { return note(ws, "That plan did not fit the project, so nothing was proposed.", cx) };
    let mut summary = mix::summary(&plan);
    summary.extend(plan.notes.iter().take(2).cloned());
    ws.chat_log.push(ChatEntry::Proposal { id, label, origin: by.to_owned(), summary, destructive: false, status: ProposalStatus::Pending });
    let a = get(cx);
    a.update(cx, |a, _| a.scroll.scroll_to_bottom());
    cx.notify();
}

// ── Attachments ──

pub fn attach(a: &mut Assistant, x: Attachment) {
    if !a.attachments.iter().any(|y| y.same(&x)) {
        a.attachments.push(x);
    }
}

pub fn detach(index: usize, cx: &mut Context<Workspace>) {
    let a = get(cx);
    a.update(cx, |a, _| {
        if index < a.attachments.len() {
            a.attachments.remove(index);
        }
    });
    cx.notify();
}

pub fn attach_selection(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let p = &ws.s.project;
    let track = ws.s.selected_track.and_then(|id| p.track(id)).map(|t| Attachment::Track(t.id, t.name.clone()));
    let clip = ws.s.selected_clip.and_then(|id| p.clips.get(&id).map(|c| Attachment::Clip(id, c.name.clone())));
    if track.is_none() && clip.is_none() {
        return crate::workspace::Workspace::toast(ws, "Select a track or a clip to attach", cx);
    }
    a.update(cx, |a, _| {
        for x in [track, clip].into_iter().flatten() {
            attach(a, x);
        }
    });
    cx.notify();
}

pub fn attach_sample(item: &LibraryItem, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let line = sample_line(item);
    let name = item.name.clone();
    a.update(cx, |a, _| attach(a, Attachment::Sample { name, line }));
    cx.notify();
}

fn sample_line(item: &LibraryItem) -> String {
    let mut s = format!("{} ({}", item.name, if item.category.is_empty() { "sample" } else { &item.category });
    if let Some(k) = item.key {
        s.push_str(&format!(", {} {}", k.display(), short_key(k)));
    }
    if let Some(b) = item.bpm {
        s.push_str(&format!(", {b:.0} BPM"));
    }
    s.push_str(&format!(", {:.1} s, {})", item.duration, if item.is_loop { "loop" } else { "one-shot" }));
    s
}

fn image_mime(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

const AUDIO_EXT: &[&str] = &["wav", "aif", "aiff", "mp3", "flac", "ogg", "m4a", "aac"];
/// Larger images are refused (the API limit is about 5 MB per image).
const MAX_IMAGE: u64 = 5 * 1024 * 1024;

/// Attach files: images as screenshots, audio files as samples.
pub fn attach_paths(paths: Vec<PathBuf>, cx: &mut Context<Workspace>) {
    let a = get(cx);
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(mime) = image_mime(&ext) {
            a.update(cx, |a, _| a.attachments.push(Attachment::Image { name: name.clone(), media_type: mime, data: None }));
            load_image(a.clone(), path, name, cx);
        } else if AUDIO_EXT.contains(&ext.to_ascii_lowercase().as_str()) {
            let line = format!("{name} (audio file the user attached)");
            a.update(cx, |a, _| attach(a, Attachment::Sample { name, line }));
        }
    }
    cx.notify();
}

/// Read an image file on the background executor.
fn load_image(a: Entity<Assistant>, path: PathBuf, name: String, cx: &mut Context<Workspace>) {
    let read = cx.background_executor().spawn(async move { read_image(&path) });
    cx.spawn(async move |ws, cx| {
        let result = read.await;
        a.update(cx, |a, _| {
            let slot = a.attachments.iter().position(|x| matches!(x, Attachment::Image { name: n, data: None, .. } if *n == name));
            match (slot, result) {
                (Some(i), Ok(bytes)) => {
                    if let Attachment::Image { data, .. } = &mut a.attachments[i] {
                        *data = Some(Arc::new(bytes));
                    }
                }
                (Some(i), Err(_)) => {
                    a.attachments.remove(i);
                }
                _ => {}
            }
        });
        let _ = ws.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

fn read_image(path: &Path) -> std::io::Result<Vec<u8>> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_IMAGE {
        return Err(std::io::Error::other("image too large"));
    }
    std::fs::read(path)
}

pub fn pick_files(cx: &mut Context<Workspace>) {
    let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
    cx.spawn(async move |ws, cx| {
        if let Ok(Ok(Some(paths))) = rx.await {
            let _ = ws.update(cx, |_, cx| attach_paths(paths, cx));
        }
    })
    .detach();
}

/// Cmd+V in the chat field: an image becomes a screenshot attachment, text
/// is typed in, files are attached.
pub fn paste(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(item) = cx.read_from_clipboard() else { return };
    let a = get(cx);
    for entry in item.entries() {
        match entry {
            ClipboardEntry::Image(img) => {
                let mime = match img.format {
                    ImageFormat::Png => "image/png",
                    ImageFormat::Jpeg => "image/jpeg",
                    ImageFormat::Webp => "image/webp",
                    ImageFormat::Gif => "image/gif",
                    _ => return Workspace::toast(ws, "That image format is not supported; paste a PNG or JPEG", cx),
                };
                if img.bytes.len() as u64 > MAX_IMAGE {
                    return Workspace::toast(ws, "That image is too large to send (5 MB at most)", cx);
                }
                let n = a.read(cx).attachments.iter().filter(|x| matches!(x, Attachment::Image { .. })).count() + 1;
                let data = Some(Arc::new(img.bytes.clone()));
                a.update(cx, |a, _| a.attachments.push(Attachment::Image { name: format!("Screenshot {n}"), media_type: mime, data }));
            }
            ClipboardEntry::String(s) => {
                let input = a.read(cx).input.clone();
                let text = format!("{}{}", input.read(cx).text(), s.text().replace(['\n', '\r'], " "));
                input.update(cx, |i, cx| i.set_text(&text, cx));
            }
            ClipboardEntry::ExternalPaths(paths) => attach_paths(paths.paths().to_vec(), cx),
        }
    }
    cx.notify();
}

/// "Use API key from clipboard": store it in the keychain, never show it.
pub fn key_from_clipboard(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(key) = cx.read_from_clipboard().and_then(|c| c.text()) else { return Workspace::toast(ws, "Copy your Anthropic API key first", cx) };
    let key = key.trim().to_owned();
    if !llm::keychain::looks_like_key(&key) {
        return Workspace::toast(ws, "The clipboard does not hold an Anthropic API key (sk-ant-…)", cx);
    }
    let task = cx.background_executor().spawn(async move { llm::keychain::store(&key) });
    cx.spawn(async move |ws, cx| {
        let result = task.await;
        let _ = ws.update(cx, |ws, cx| match result {
            Ok(()) => {
                note(ws, "API key saved in the system keychain. Claude answers from now on.", cx);
                refresh_key(cx);
            }
            Err(e) => Workspace::toast(ws, e, cx),
        });
    })
    .detach();
}

pub fn forget_key(cx: &mut Context<Workspace>) {
    let task = cx.background_executor().spawn(async { llm::keychain::forget() });
    cx.spawn(async move |ws, cx| {
        task.await;
        let _ = ws.update(cx, |_, cx| refresh_key(cx));
    })
    .detach();
}

pub fn set_backend(ws: &mut Workspace, backend: Backend, cx: &mut Context<Workspace>) {
    if ws.look.app.assistant_model != backend.pref_id() {
        ws.look.app.assistant_model = backend.pref_id().to_owned();
        ws.look.app.save();
    }
    let a = get(cx);
    a.update(cx, |a, _| {
        a.backend = backend;
        a.model_menu = false;
    });
    match backend {
        Backend::Agent(kind) => ws.b.ai.set_agent(Some(kind)),
        Backend::Claude(_) => ws.b.ai.set_agent(None),
    }
    cx.notify();
}

pub fn toggle_model_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let open = a.update(cx, |a, _| {
        a.model_menu = !a.model_menu;
        a.model_menu
    });
    if open {
        ws.b.ai.refresh_agents();
        ws.ensure_ai_poll(cx);
        refresh_key(cx);
    }
    cx.notify();
}

// ── Sending and streaming ──

/// The conversation so far as API messages (text only; images ride on the
/// message they were attached to). Consecutive turns of one role merge.
fn history(a: &Assistant) -> Vec<llm::Message> {
    let mut out: Vec<llm::Message> = Vec::new();
    for p in &a.items {
        let (role, text) = match &p.item {
            Item::User { text, .. } => (llm::Role::User, text.clone()),
            Item::Reply { text, state: ReplyState::Done | ReplyState::Stopped, .. } if !text.is_empty() => (llm::Role::Assistant, text.clone()),
            _ => continue,
        };
        match out.last_mut() {
            Some(m) if m.role == role => {
                if let Some(llm::Part::Text(t)) = m.parts.last_mut() {
                    t.push_str("\n\n");
                    t.push_str(&text);
                }
            }
            _ => out.push(llm::Message::text(role, text)),
        }
    }
    while out.first().is_some_and(|m| m.role != llm::Role::User) {
        out.remove(0);
    }
    // Keep the request small: the last 16 turns.
    let skip = out.len().saturating_sub(16);
    let mut out: Vec<_> = out.into_iter().skip(skip).collect();
    while out.first().is_some_and(|m| m.role != llm::Role::User) {
        out.remove(0);
    }
    out
}

/// Send `text` (with the attachments) to the current backend. `mix` marks a
/// "mix this track" request whose reply carries a plan.
pub fn send(ws: &mut Workspace, text: String, mix: Option<TrackId>, cx: &mut Context<Workspace>) {
    let a = get(cx);
    if a.read(cx).streaming() {
        stop(ws, cx);
    }
    let (backend, attachments) = a.read_with(cx, |a, _| (a.backend, a.attachments.clone()));
    if attachments.iter().any(|x| matches!(x, Attachment::Image { data: None, .. })) {
        return Workspace::toast(ws, "Still reading an attached image; send again in a moment", cx);
    }
    let chips = attachments.iter().map(Attachment::chip).collect();
    a.update(cx, |a, _| {
        push(ws, a, Item::User { text: text.clone(), chips });
        a.attachments.clear();
    });

    let model = match backend {
        Backend::Agent(kind) => {
            // The user's own agent CLI answers; its reply lands in the chat log.
            ws.b.ai.set_agent(Some(kind));
            let mut question = text;
            for x in &attachments {
                if let Attachment::Sample { line, .. } = x {
                    question.push_str(&format!("\n[attached sample: {line}]"));
                }
            }
            ws.b.ai.chat(&question, &ws.s.project);
            ws.ensure_ai_poll(cx);
            cx.notify();
            return;
        }
        Backend::Claude(i) => llm::MODELS[i],
    };

    let mut focus = Focus { selected_track: ws.s.selected_track, selected_clip: ws.s.selected_clip, ..Default::default() };
    let mut images = Vec::new();
    for x in &attachments {
        match x {
            Attachment::Track(id, _) => focus.tracks.push(*id),
            Attachment::Clip(id, _) => focus.clips.push(*id),
            Attachment::Sample { line, .. } => focus.samples.push(line.clone()),
            Attachment::Image { media_type, data: Some(d), .. } => images.push(llm::Part::Image { media_type: (*media_type).to_owned(), data: d.clone() }),
            Attachment::Image { .. } => {}
        }
    }
    if let Some(t) = mix {
        focus.selected_track = Some(t);
    }
    focus.images = images.len();
    let mut system = llm::context::producer_context(&ws.s.project, &focus);
    if mix.is_some() {
        system.push_str("\n\n");
        system.push_str(llm::mix::INSTRUCTIONS);
    }
    let mut messages = a.read_with(cx, |a, _| history(a));
    // The newest user turn gets the screenshots.
    if let Some(last) = messages.last_mut().filter(|m| m.role == llm::Role::User) {
        let mut parts = images;
        parts.append(&mut last.parts);
        last.parts = parts;
    }
    let request = llm::Request { model: model.id.to_owned(), system, messages, max_tokens: 16_000 };
    let stream = llm::spawn(request, None);
    a.update(cx, |a, _| {
        let item = push(ws, a, Item::Reply { text: String::new(), model: model.name, state: ReplyState::Waiting, mix });
        a.live = Some(Live { item, stream });
    });
    ensure_poll(&a, cx);
    cx.notify();
}

fn ensure_poll(a: &Entity<Assistant>, cx: &mut Context<Workspace>) {
    if a.read(cx).poll.is_some() {
        return;
    }
    let task = cx.spawn(async move |ws, cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let keep = ws.update(cx, poll).unwrap_or(false);
            if !keep {
                break;
            }
        }
    });
    a.update(cx, |a, _| a.poll = Some(task));
}

/// Drain the stream; `false` once it is over.
fn poll(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let a = get(cx);
    let mut finished: Option<(Option<TrackId>, Result<String, String>)> = None;
    let mut changed = false;
    let keep = a.update(cx, |a, _| {
        let Some(live) = &a.live else {
            a.poll = None;
            return false;
        };
        let index = live.item;
        let mut events = Vec::new();
        let mut gone = false;
        loop {
            match live.stream.events.try_recv() {
                Ok(e) => events.push(e),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        let Some(Placed { item: Item::Reply { text, state, mix, .. }, .. }) = a.items.get_mut(index) else {
            a.live = None;
            a.poll = None;
            return false;
        };
        for e in events {
            changed = true;
            match e {
                llm::Event::Delta(t) => {
                    text.push_str(&t);
                    *state = ReplyState::Streaming;
                }
                llm::Event::Done { stop_reason } => {
                    *state = if stop_reason.as_deref() == Some("refusal") && text.is_empty() {
                        ReplyState::Failed("The model declined to answer that.".into())
                    } else {
                        ReplyState::Done
                    };
                    finished = Some((*mix, Ok(text.clone())));
                }
                llm::Event::Error(m) => {
                    *state = ReplyState::Failed(m.clone());
                    finished = Some((*mix, Err(m)));
                }
            }
            if finished.is_some() {
                break;
            }
        }
        if finished.is_none() && gone {
            *state = ReplyState::Failed("The request ended without an answer.".into());
            finished = Some((*mix, Err(String::new())));
            changed = true;
        }
        if changed {
            a.scroll.scroll_to_bottom();
        }
        if finished.is_some() {
            a.live = None;
            a.poll = None;
            return false;
        }
        true
    });
    if let Some((Some(track), result)) = finished {
        let model = a.read(cx).backend.name();
        let plan = result.as_ref().ok().and_then(|t| llm::mix::parse_reply(t));
        match plan {
            Some(plan) => propose_mix(ws, track, plan, Origin::Agent(model.to_owned()), model, cx),
            None => {
                // No usable plan from the model: fall back to the rules.
                if let Some(t) = ws.s.project.track(track) {
                    let plan = llm::mix::heuristic(t);
                    propose_mix(ws, track, plan, Origin::Feature("mix".into()), "Rules", cx);
                }
            }
        }
    }
    if changed {
        cx.notify();
    }
    keep
}

/// The Stop button: cancel the request, keep what arrived.
pub fn stop(_ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = get(cx);
    a.update(cx, |a, _| {
        if let Some(live) = a.live.take() {
            live.stream.cancel.cancel();
            if let Some(Placed { item: Item::Reply { state, .. }, .. }) = a.items.get_mut(live.item) {
                *state = ReplyState::Stopped;
            }
        }
        a.poll = None;
    });
    cx.notify();
}

/// The part of a reply shown in the chat: a mix reply hides its JSON plan
/// (the proposal card shows it).
pub fn visible_reply(text: &str, mix: bool) -> &str {
    if !mix {
        return text;
    }
    match text.find("```json").or_else(|| text.rfind("```")) {
        Some(i) => text[..i].trim_end(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_replies_hide_the_plan() {
        let t = "- cut lows\n- glue\n```json\n{\"volume_db\": -6}\n```";
        assert_eq!(visible_reply(t, true), "- cut lows\n- glue");
        assert_eq!(visible_reply(t, false), t);
        assert_eq!(visible_reply("no plan yet", true), "no plan yet");
    }

    #[test]
    fn mime_types_by_extension() {
        assert_eq!(image_mime("PNG"), Some("image/png"));
        assert_eq!(image_mime("jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime("tiff"), None);
    }
}
