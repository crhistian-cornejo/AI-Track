//! The assistant's state and what it does: one chat, the `/` actions, the
//! attachments, the model choice and the streaming request.
//!
//! The state lives in an entity kept as a GPUI global, so the panel needs no
//! new field on the workspace. Chat items are anchored to positions in
//! `Workspace::chat_log` (job, proposal, licence and sample cards land there)
//! so both read as one timeline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aitrack_ai::generate::spec::{Kind, Role};
use aitrack_ai::generate::{self as gen_core, Source as SpecSource};
use aitrack_ai::llm::{self, context::Focus, mix::MixPlan};
use aitrack_model::{ClipId, Origin, Source, TrackId};
use gpui::{AppContext, ClipboardEntry, Context, Entity, Global, ImageFormat, ScrollHandle, Subscription, Task};

use super::history::{self, Archive, Chat};
use super::midi_gen::{self, Prepared};
use super::part::{self, Session};
use super::{mix, slash};
use crate::bridge::{AgentKind, LibraryItem};
use crate::icons::Icon;
use crate::views::browser::short_key;
use crate::widgets::{InputEvent, TextInput};
use crate::workspace::{ChatEntry, PartCard, ProposalStatus, Workspace};

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
    Reply { text: String, model: &'static str, state: ReplyState, mix: Option<TrackId>, is_part: bool },
    /// A `/` action that ran: its card, followed by its job/result cards.
    Action { icon: Icon, title: &'static str, detail: String },
    /// A short line from the assistant itself (no model involved).
    Note(String),
    /// Claude working as an agent: thinking, text and tool calls.
    Agent(super::agent::Reply),
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
    /// Generated MIDI parts by card id.
    pub parts: HashMap<u64, Session>,
    next_part: u64,
    /// The part a model is writing right now.
    gen_job: Option<GenJob>,
    /// The project's other chats (the open one is `items`).
    pub chats: Vec<Chat>,
    pub chat_id: u64,
    /// Unix seconds of the open chat's last message.
    pub chat_updated: u64,
    /// The list of the project's chats is open.
    pub history_open: bool,
    /// When a click outside closed the list (so the button's own click does not reopen it).
    history_closed_at: Option<std::time::Instant>,
    _subs: Vec<Subscription>,
}

/// A part a model is writing: what to fall back to and where the result goes.
struct GenJob {
    prepared: Prepared,
    seed: u64,
    model: &'static str,
    mode: GenMode,
}

enum GenMode {
    New,
    /// A new take for the card with this id.
    Again(u64),
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
    let input = cx.new(|cx| TextInput::new(cx, "Ask for anything, or / for actions", None).bare());
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
        parts: HashMap::new(),
        next_part: 0,
        gen_job: None,
        chats: Vec::new(),
        chat_id: 1,
        chat_updated: 0,
        history_open: false,
        history_closed_at: None,
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

pub(super) fn push(ws: &Workspace, a: &mut Assistant, item: Item) -> usize {
    // The user's own message always brings the end into view; the rest
    // follows only a reader who is already at the end.
    if matches!(item, Item::User { .. }) || at_bottom(&a.scroll) {
        a.scroll.scroll_to_bottom();
    }
    a.items.push(Placed { anchor: anchor(ws), item });
    a.chat_updated = history::now();
    a.items.len() - 1
}

/// Whether the log shows its end (or has nothing to scroll).
pub fn at_bottom(h: &ScrollHandle) -> bool {
    let max = h.max_offset().y;
    max <= gpui::px(1.) || h.offset().y <= -max + gpui::px(24.)
}

/// Keep the newest text in view while the reader is at the end; one who
/// scrolled up to read is left where they are.
pub fn follow(cx: &mut Context<Workspace>) {
    let a = get(cx);
    a.update(cx, |a, _| {
        if at_bottom(&a.scroll) {
            a.scroll.scroll_to_bottom();
        }
    });
}

/// Scroll to the newest message (the "jump to latest" button).
pub fn jump_to_latest(cx: &mut Context<Workspace>) {
    get(cx).update(cx, |a, _| a.scroll.scroll_to_bottom());
    cx.notify();
}

/// The open chat got a message: it is the newest.
pub fn chat_changed(cx: &mut Context<Workspace>) {
    get(cx).update(cx, |a, _| a.chat_updated = history::now());
}

// ── The project's chats ──

fn current_chat(a: &Assistant) -> Option<Chat> {
    let items: Vec<history::Saved> = a.items.iter().filter_map(|p| history::save(&p.item)).collect();
    (!items.is_empty()).then(|| Chat { id: a.chat_id, title: history::title(&a.items), updated: a.chat_updated, items })
}

/// Every chat of the project, newest first, the open one included.
pub fn all_chats(cx: &mut Context<Workspace>) -> Vec<Chat> {
    let a = get(cx);
    let a = a.read(cx);
    let mut all: Vec<Chat> = current_chat(a).into_iter().chain(a.chats.iter().cloned()).collect();
    all.sort_by_key(|c| std::cmp::Reverse(c.updated));
    all
}

/// What the project file keeps under `"assistant"`; `None` with no chat at all.
pub fn archive(cx: &mut Context<Workspace>) -> Option<serde_json::Value> {
    let chats = all_chats(cx);
    if chats.is_empty() {
        return None;
    }
    let current = get(cx).read(cx).chat_id;
    serde_json::to_value(Archive { current, chats }).ok()
}

/// Leave the open chat (kept in the list) for an empty one or `next`.
fn leave_chat(ws: &mut Workspace, next: Option<Chat>, cx: &mut Context<Workspace>) {
    stop(ws, cx);
    let a = get(cx);
    a.update(cx, |a, _| {
        if let Some(c) = current_chat(a) {
            a.chats.retain(|x| x.id != c.id);
            a.chats.push(c);
        }
        a.items.clear();
        a.attachments.clear();
        a.history_open = false;
        match next {
            Some(c) => {
                a.chats.retain(|x| x.id != c.id);
                a.chat_id = c.id;
                a.chat_updated = c.updated;
                a.items = c.items.iter().map(|s| Placed { anchor: 0, item: history::restore(s) }).collect();
            }
            None => {
                a.chat_id = a.chats.iter().map(|c| c.id).max().unwrap_or(0) + 1;
                a.chat_updated = history::now();
            }
        }
        a.scroll.scroll_to_bottom();
    });
    // Keep cards still waiting for an answer (proposals, licences).
    ws.chat_log.retain(|e| matches!(e, ChatEntry::Proposal { status: ProposalStatus::Pending, .. } | ChatEntry::License { status: ProposalStatus::Pending, .. }) || matches!(e, ChatEntry::Part(c) if matches!(c.status, part::Status::Pending | part::Status::Trying)));
    super::agent::agent(cx).expanded.clear();
    cx.notify();
}

pub fn new_chat(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    leave_chat(ws, None, cx);
}

pub fn open_chat(ws: &mut Workspace, id: u64, cx: &mut Context<Workspace>) {
    if get(cx).read(cx).chat_id == id {
        get(cx).update(cx, |a, _| a.history_open = false);
        return cx.notify();
    }
    let next = get(cx).read(cx).chats.iter().find(|c| c.id == id).cloned();
    if next.is_some() {
        leave_chat(ws, next, cx);
    }
}

pub fn delete_chat(ws: &mut Workspace, id: u64, cx: &mut Context<Workspace>) {
    if get(cx).read(cx).chat_id == id {
        stop(ws, cx);
        get(cx).update(cx, |a, _| {
            a.items.clear();
            a.chat_id = a.chats.iter().map(|c| c.id).max().unwrap_or(a.chat_id) + 1;
        });
    }
    get(cx).update(cx, |a, _| a.chats.retain(|c| c.id != id));
    ws.save_chats_quietly(cx);
    cx.notify();
}

pub fn toggle_history(cx: &mut Context<Workspace>) {
    get(cx).update(cx, |a, _| {
        let just_closed = a.history_closed_at.take().is_some_and(|t| t.elapsed() < Duration::from_millis(250));
        a.history_open = !a.history_open && !just_closed;
    });
    cx.notify();
}

/// A click outside the list closes it.
pub fn close_history(cx: &mut Context<Workspace>) {
    get(cx).update(cx, |a, _| {
        if a.history_open {
            a.history_open = false;
            a.history_closed_at = Some(std::time::Instant::now());
        }
    });
    cx.notify();
}

/// A project was opened (or made): its own chats, the newest open.
pub fn load_chats(ws: &mut Workspace, archive: Option<Archive>, cx: &mut Context<Workspace>) {
    stop(ws, cx);
    let mut archive = archive.unwrap_or_default();
    let open = archive.chats.iter().position(|c| c.id == archive.current).or_else(|| (0..archive.chats.len()).max_by_key(|&i| archive.chats[i].updated)).map(|i| archive.chats.remove(i));
    let a = get(cx);
    a.update(cx, |a, _| {
        a.items.clear();
        a.attachments.clear();
        a.history_open = false;
        a.chats = archive.chats;
        match open {
            Some(c) => {
                a.chat_id = c.id;
                a.chat_updated = c.updated;
                a.items = c.items.iter().map(|s| Placed { anchor: 0, item: history::restore(s) }).collect();
            }
            None => {
                a.chat_id = a.chats.iter().map(|c| c.id).max().unwrap_or(0) + 1;
                a.chat_updated = 0;
            }
        }
        a.scroll.scroll_to_bottom();
    });
    super::agent::agent(cx).expanded.clear();
    cx.notify();
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
        InputEvent::Changed(text) => {
            a.update(cx, |a, _| a.slash_sel = 0);
            // The user is writing: have the model's CLI ready when they send.
            if !text.trim().is_empty() && slash::query(text).is_none() {
                super::agent::prewarm(cx);
            }
            cx.notify();
        }
        InputEvent::Submit(text) => {
            let text = text.trim().to_owned();
            if super::agent::asking(cx) {
                // A question is open: what was typed is the answer.
                if !text.is_empty() {
                    clear_input(&a, cx);
                    let picked = super::agent::agent(cx).pending.as_ref().map(|p| p.picked.clone()).unwrap_or_default();
                    super::agent::answer(&picked, &text, cx);
                }
            } else if let Some(q) = slash::query(&text) {
                let (sel, tab) = a.read_with(cx, |a, _| (a.slash_sel, a.slash_tab));
                let hits = slash::filter(q, tab);
                if let Some(entry) = hits.get(sel.min(hits.len().saturating_sub(1))) {
                    choose(ws, entry.action, cx);
                }
            } else if !text.is_empty() {
                clear_input(&a, cx);
                submit_text(ws, text, cx);
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
    // `/bass rolling in A minor`: the words after the command refine the action.
    let text = a.read(cx).input_text(cx);
    let args = slash::query(&text).map(|q| slash::split(q).1.to_owned()).unwrap_or_default();
    clear_input(&a, cx);
    run_with(ws, action, &args, cx);
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

fn run_with(ws: &mut Workspace, action: slash::Action, args: &str, cx: &mut Context<Workspace>) {
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
        A::ExtractDrums => {
            let Some((name, _)) = audio_target(ws) else { return note(ws, "Select an audio clip first, then run /drums again.", cx) };
            card(ws, cx, name);
            ws.extract_drums_from_selection(cx);
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
        A::Generate(kind) => generate(ws, Some(kind), args, cx),
        A::AttachSelection => attach_selection(ws, cx),
        A::AttachFile => pick_files(cx),
        A::Overview => submit_text(ws, "Give me a quick overview of this project: what is there, what stands out, and three concrete things to try next.".into(), cx),
        A::ClearChat => new_chat(ws, cx),
    }
    cx.notify();
}

/// The model that should write a part, if the assistant is set to one that
/// can answer: the same choice the chat uses, with no setting of its own.
fn model_choice(a: &Entity<Assistant>, cx: &mut Context<Workspace>) -> Option<usize> {
    match a.read_with(cx, |a, _| (a.backend, a.key)) {
        (Backend::Claude(i), key) if key != KeyState::Missing => Some(i),
        _ => None,
    }
}

/// Write a part (`/bass`, `/chords`, `/beat`, `/arp`, `/melody`, `/perc`, or
/// free text naming one) and offer it as one card. With a model connected it
/// writes the spec (structured output) and the rules play it; without one,
/// or when its answer is no use, the rules write the spec as well.
pub fn generate(ws: &mut Workspace, kind: Option<Kind>, text: &str, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let selected = target_track(ws, a.read(cx));
    let prepared = match midi_gen::prepare(&ws.s.project, selected, ws.s.selected_clip, kind, text) {
        Ok(p) => p,
        Err(e) => return note(ws, e, cx),
    };
    let seed = prepared.req.seed.unwrap_or_else(fresh_seed);
    let entry = kind.and_then(|k| slash::ENTRIES.iter().find(|e| e.action == slash::Action::Generate(k)).copied());
    let (icon, title) = entry.map_or((Icon::Sparkles, "Generate a part"), |e| (e.icon, e.title));
    let model = model_choice(&a, cx);
    let what = format!("{} · {} bar{}", prepared.req.kind.label(), prepared.req.bars, if prepared.req.bars == 1 { "" } else { "s" });
    let how = model.map_or("written by rules".to_owned(), |i| format!("asking {}", llm::MODELS[i].name));
    a.update(cx, |a, _| push(ws, a, Item::Action { icon, title, detail: format!("{what} · {how}") }));
    match model {
        Some(i) => start_model(ws, prepared, seed, GenMode::New, None, i, cx),
        None => {
            let g = gen_core::generate(&prepared.req, &prepared.ctx, seed);
            present(ws, prepared, g, "Rules".to_owned(), cx);
        }
    }
}

/// Ask the model for the spec. The reply is JSON only (the schema is
/// enforced; the provider retries once as plain text if the API refuses it).
fn start_model(ws: &mut Workspace, prepared: Prepared, seed: u64, mode: GenMode, previous: Option<&gen_core::MidiSpec>, model: usize, cx: &mut Context<Workspace>) {
    let a = get(cx);
    if a.read(cx).streaming() {
        stop(ws, cx);
    }
    let choice = llm::MODELS[model];
    let focus = Focus { selected_track: ws.s.selected_track, selected_clip: ws.s.selected_clip, ..Default::default() };
    let mut system = llm::context::producer_context(&ws.s.project, &focus);
    system.push_str("\n\n");
    system.push_str(llm::midigen::INSTRUCTIONS);
    system.push_str("\n\n");
    system.push_str(&prepared.ctx.to_prompt(prepared.req.bars));
    let user = llm::midigen::user_prompt(&prepared.req, previous);
    let request = llm::Request { model: choice.id.to_owned(), system, messages: vec![llm::Message::text(llm::Role::User, user)], max_tokens: 4_000, schema: Some(llm::midigen::schema()) };
    let stream = llm::spawn(request, None);
    a.update(cx, |a, _| {
        let item = push(ws, a, Item::Reply { text: String::new(), model: choice.name, state: ReplyState::Waiting, mix: None, is_part: true });
        a.live = Some(Live { item, stream });
        a.gen_job = Some(GenJob { prepared, seed, model: choice.name, mode });
    });
    ensure_poll(&a, cx);
    cx.notify();
}

/// The model finished (or failed): turn its spec into a part, or let the
/// rules write it and say so on the card.
fn finish_model(ws: &mut Workspace, result: Result<String, String>, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let Some(job) = a.update(cx, |a, _| a.gen_job.take()) else { return };
    let (spec, why) = match &result {
        Ok(text) => match llm::midigen::parse_reply(text) {
            Some(spec) => (Some(spec), None),
            None => (None, Some("The model's reply was not a usable spec".to_owned())),
        },
        Err(m) if m.is_empty() => (None, Some("The model did not answer".to_owned())),
        Err(m) => (None, Some(format!("The model could not answer: {m}"))),
    };
    match job.mode {
        GenMode::New => {
            let mut g = match spec {
                Some(spec) => gen_core::generate_from_spec(&job.prepared.req, &job.prepared.ctx, spec, job.seed),
                None => gen_core::generate(&job.prepared.req, &job.prepared.ctx, job.seed),
            };
            if g.fallback.is_none() {
                g.fallback = why;
            }
            let origin = if g.source == SpecSource::Model { job.model.to_owned() } else { "Rules".to_owned() };
            present(ws, job.prepared, g, origin, cx);
        }
        GenMode::Again(sid) => {
            let Some(mut s) = a.update(cx, |a, _| a.parts.remove(&sid)) else { return };
            let mut g = aitrack_ai::generate::lock::regenerate(&s.generated, &s.req, &s.ctx, &s.locks, job.seed, spec);
            if g.source == SpecSource::Template && g.fallback.is_none() {
                g.fallback = why;
            }
            s.origin = if g.source == SpecSource::Model { job.model.to_owned() } else { "Rules".to_owned() };
            if let Err(e) = s.revise(g, &mut ws.s.project, &mut ws.s.history) {
                ws.toast(e, cx);
            }
            put_back(ws, sid, s, false, cx);
        }
    }
}

/// A seed for a new part: the clock mixed with a counter, so two requests in
/// the same instant still differ. It is shown on the card.
fn fresh_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    aitrack_ai::generate::rng::splitmix64(now ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9))
}

fn card_of(sid: u64, s: &Session, busy: bool) -> PartCard {
    let bars = if s.generated.bars > 1 { (0..s.generated.bars).map(|b| s.locks.bars.contains(&b)).collect() } else { Vec::new() };
    PartCard {
        sid,
        label: s.label(),
        origin: s.origin.clone(),
        summary: s.summary.iter().take(6).cloned().collect(),
        seed: s.seed() % 100_000,
        layers: s.layers(),
        bars,
        status: s.status,
        destructive: s.destructive,
        notice: s.generated.fallback.clone(),
        busy,
    }
}

/// Make a card for a new part and offer it.
fn present(ws: &mut Workspace, prepared: Prepared, g: gen_core::Generated, origin: String, cx: &mut Context<Workspace>) {
    let a = get(cx);
    if g.draft.notes.is_empty() {
        return note(ws, "Nothing came out of that request; try other words.", cx);
    }
    let sid = a.update(cx, |a, _| {
        a.next_part += 1;
        a.next_part
    });
    let mut s = Session::new(prepared, g, origin);
    if let Err(e) = s.offer(&mut ws.s.project, &mut ws.s.history) {
        return note(ws, format!("That part did not fit the project: {e}"), cx);
    }
    ws.chat_log.push(ChatEntry::Part(card_of(sid, &s, false)));
    a.update(cx, |a, _| {
        a.parts.insert(sid, s);
        a.scroll.scroll_to_bottom();
    });
    cx.notify();
}

/// Store `s` again and redraw its card.
fn put_back(ws: &mut Workspace, sid: u64, s: Session, busy: bool, cx: &mut Context<Workspace>) {
    let card = card_of(sid, &s, busy);
    for e in &mut ws.chat_log {
        if let ChatEntry::Part(c) = e
            && c.sid == sid
        {
            *c = card.clone();
        }
    }
    get(cx).update(cx, |a, _| {
        a.parts.insert(sid, s);
    });
    cx.notify();
}

/// What the buttons of a part card do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartAction {
    /// Apply it as a normal step with the loop on it, and play.
    Listen,
    Accept,
    Keep,
    Discard,
    /// Take the listened part off again.
    Undo,
    Vary,
    Regenerate,
    HoldLayer(Role),
    HoldBar(u32),
}

pub fn part_action(ws: &mut Workspace, sid: u64, action: PartAction, cx: &mut Context<Workspace>) {
    let a = get(cx);
    if ws.chat_log.iter().any(|e| matches!(e, ChatEntry::Part(c) if c.sid == sid && c.busy)) {
        return;
    }
    let Some(mut s) = a.update(cx, |a, _| a.parts.remove(&sid)) else { return };
    let mut busy = false;
    let mut said: Option<String> = None;
    match action {
        PartAction::Listen => match s.listen(&mut ws.s.project, &mut ws.s.history) {
            Ok((start, _)) => {
                let engine = ws.b.engine.as_mut();
                engine.set_looping(true);
                engine.locate(start);
                engine.play();
            }
            Err(e) => said = Some(e),
        },
        PartAction::Accept | PartAction::Keep => match s.accept(&mut ws.s.project, &mut ws.s.history) {
            Ok(()) => said = Some("Part applied — one undo step".to_owned()),
            Err(e) => said = Some(e),
        },
        PartAction::Discard => {
            s.discard(&mut ws.s.project, &mut ws.s.history);
            if s.status == part::Status::Accepted {
                said = Some("You edited the project since, so the part stays; undo it with Cmd+Z".to_owned());
            }
        }
        PartAction::Undo => match s.undo_trial(&mut ws.s.project, &mut ws.s.history) {
            Ok(()) => {
                ws.b.engine.stop();
                if let Err(e) = s.offer(&mut ws.s.project, &mut ws.s.history) {
                    said = Some(e);
                }
            }
            Err(e) => said = Some(e),
        },
        PartAction::Vary => {
            if let Err(e) = s.vary(fresh_seed(), &mut ws.s.project, &mut ws.s.history) {
                said = Some(e);
            }
        }
        PartAction::Regenerate => match model_choice(&a, cx) {
            Some(i) if s.can_revise() => {
                let prepared = Prepared { req: s.req.clone(), ctx: s.ctx.clone(), dest: s.dest, start: s.start };
                start_model(ws, prepared, fresh_seed(), GenMode::Again(sid), Some(&s.generated.spec), i, cx);
                busy = true;
            }
            _ => {
                if let Err(e) = s.regenerate(fresh_seed(), None, &mut ws.s.project, &mut ws.s.history) {
                    said = Some(e);
                }
            }
        },
        PartAction::HoldLayer(role) => s.locks.toggle_role(role),
        PartAction::HoldBar(bar) => s.locks.toggle_bar(bar),
    }
    put_back(ws, sid, s, busy, cx);
    if let Some(t) = said {
        ws.toast(t, cx);
    }
}

/// Notice Cmd+Z (or edits) made while a part is being listened to. Called
/// when the panel is drawn.
pub fn sync_parts(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = get(cx);
    let listening = a.read_with(cx, |a, _| a.parts.values().any(|s| s.status == part::Status::Trying));
    if !listening {
        return;
    }
    a.update(cx, |a, _| {
        for s in a.parts.values_mut() {
            s.sync(&ws.s.history);
        }
        for e in &mut ws.chat_log {
            if let ChatEntry::Part(c) = e
                && let Some(s) = a.parts.get(&c.sid)
            {
                c.status = s.status;
            }
        }
    });
}

/// Typed text without a slash. With no model connected, a plain request for
/// a part ("un bajo rodante en La menor de 2 compases") is written by rules;
/// anything else, and everything when a model is connected, goes to the chat.
pub fn submit_text(ws: &mut Workspace, text: String, cx: &mut Context<Workspace>) {
    super::agent::init(ws, cx);
    if super::agent::ready(cx) {
        return super::agent::submit(ws, text, cx);
    }
    // No subscription CLI here: a plain request for a part is still written
    // by the rules; anything else says how to connect a model.
    let a = get(cx);
    a.update(cx, |a, _| push(ws, a, Item::User { text: text.clone(), chips: Vec::new() }));
    if is_request_for_a_part(&text) {
        return generate(ws, None, &text, cx);
    }
    let c = super::agent::choice(cx);
    let kind = if c.provider == aitrack_ai::llm::cli::Provider::Codex { AgentKind::Codex } else { AgentKind::ClaudeCode };
    note(ws, format!("{} isn't installed on this computer, so nobody can answer yet. {}", kind.name(), kind.setup()), cx);
}

/// Whether `text` asks for a part rather than asking a question about one.
fn is_request_for_a_part(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    let question = lower.ends_with('?') || ["how ", "what ", "why ", "which ", "can you explain", "como ", "cómo ", "qué ", "que ", "por qué", "cuál ", "cual "].iter().any(|w| lower.starts_with(w));
    !question && aitrack_ai::generate::request::parse(text, &aitrack_ai::generate::GenContext::default()).is_some()
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
    let request = llm::Request { model: model.id.to_owned(), system, messages, max_tokens: 16_000, schema: None };
    let stream = llm::spawn(request, None);
    a.update(cx, |a, _| {
        let item = push(ws, a, Item::Reply { text: String::new(), model: model.name, state: ReplyState::Waiting, mix, is_part: false });
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
    let mut finished: Option<(Option<TrackId>, bool, Result<String, String>)> = None;
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
        let Some(Placed { item: Item::Reply { text, state, mix, is_part, .. }, .. }) = a.items.get_mut(index) else {
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
                    finished = Some((*mix, *is_part, Ok(text.clone())));
                }
                llm::Event::Error(m) => {
                    *state = ReplyState::Failed(m.clone());
                    finished = Some((*mix, *is_part, Err(m)));
                }
            }
            if finished.is_some() {
                break;
            }
        }
        if finished.is_none() && gone {
            *state = ReplyState::Failed("The request ended without an answer.".into());
            finished = Some((*mix, *is_part, Err(String::new())));
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
    if let Some((_, true, result)) = finished {
        finish_model(ws, result, cx);
    } else if let Some((Some(track), _, result)) = finished {
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
pub fn stop(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    super::agent::stop(ws, cx);
    let a = get(cx);
    a.update(cx, |a, _| {
        a.gen_job = None;
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
    fn plain_requests_for_a_part_are_told_from_questions() {
        assert!(is_request_for_a_part("un bajo rodante en La menor de 2 compases"));
        assert!(is_request_for_a_part("acordes para el drop"));
        assert!(is_request_for_a_part("make me a deep house bass in F# minor"));
        assert!(!is_request_for_a_part("how do I EQ the bass?"));
        assert!(!is_request_for_a_part("por qué suena mal el bajo?"));
        assert!(!is_request_for_a_part("hello there"));
    }

    #[test]
    fn mime_types_by_extension() {
        assert_eq!(image_mime("PNG"), Some("image/png"));
        assert_eq!(image_mime("jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime("tiff"), None);
    }
}
