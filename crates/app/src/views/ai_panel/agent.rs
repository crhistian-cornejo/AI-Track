//! The assistant as an agent. Claude (API key or Claude subscription) or
//! Codex (ChatGPT subscription) reads and edits the project with
//! AI-Track's tools (`aitrack_ai::llm::tools`) until the request is done.
//! Replies stream in as steps: thinking, one line per thing it does, and
//! Markdown text. Every edit lands in a draft; when the turn ends the
//! changes are offered as one proposal, or accepted at once in auto mode.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use aitrack_ai::generate as gen_core;
use aitrack_ai::generate::spec::Kind;
use aitrack_ai::llm::agent::{self as api, AgentEvent, AgentRequest, AgentStream, Effort};
use aitrack_ai::llm::cli::{self, CliEvent, CliTurn, ModelInfo, Provider, TurnSpec, Vendor};
use aitrack_ai::llm::tools::{self, Outcome, Session};
use aitrack_ai::llm::{self, context::Focus};
use aitrack_model::Origin;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, FontWeight, Global, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, Task, div,
    prelude::FluentBuilder, px, svg,
};
use serde_json::{Value, json};

use super::state::{self, Attachment, Backend, Item, KeyState, Placed, ReplyState};
use super::{markdown, midi_gen};
use crate::bridge::AgentKind;
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::widgets::{self, label};
use crate::workspace::{ChatEntry, ProposalStatus, Workspace};

/// Tool rounds one request may take on the API before the agent stops.
const MAX_ROUNDS: u32 = 40;

#[derive(Clone, Debug, PartialEq)]
pub enum ToolState {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Thinking { text: String, done: bool },
    Text(String),
    Tool { name: String, state: ToolState },
}

/// An agent reply in the chat.
#[derive(Clone, Debug)]
pub struct Reply {
    pub model: String,
    pub vendor: Vendor,
    pub steps: Vec<Step>,
    pub state: ReplyState,
    pub started: Instant,
    pub elapsed: Option<Duration>,
    /// How many changes went into the proposal.
    pub changes: usize,
}

impl Reply {
    /// The text the user reads (every text step), for history and copying.
    pub fn text(&self) -> String {
        let parts: Vec<&str> = self.steps.iter().filter_map(|s| if let Step::Text(t) = s { Some(t.trim()) } else { None }).filter(|t| !t.is_empty()).collect();
        parts.join("\n\n")
    }
}

/// The model the next request goes to.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub provider: Provider,
    pub model: String,
    pub name: String,
}

impl Choice {
    fn of(m: &ModelInfo) -> Self {
        Self { provider: m.provider, model: m.id.clone(), name: m.name.clone() }
    }

    /// The preference string: `provider:model`.
    fn pref(&self) -> String {
        format!("{}:{}", self.provider.id(), self.model)
    }
}

/// Read a saved choice; older saves (an API model id, `agent:…`) still work.
fn parse_pref(s: &str) -> Option<Choice> {
    let find = |p: Provider, id: &str| cli::fallback(p).into_iter().find(|m| m.id == id).map(|m| Choice::of(&m));
    if let Some((p, model)) = s.split_once(':')
        && let Some(p) = Provider::parse(p)
    {
        return find(p, model).or_else(|| Some(Choice { provider: p, model: model.to_owned(), name: model.to_owned() }));
    }
    match s {
        "agent:claude-code" => cli::fallback(Provider::ClaudeCode).first().map(Choice::of),
        "agent:codex" => cli::fallback(Provider::Codex).first().map(Choice::of),
        id => find(Provider::ClaudeApi, id),
    }
}

enum Engine {
    Api { system: Vec<String>, messages: Vec<Value>, model: String, stream: Option<AgentStream>, rounds: u32 },
    Cli(CliTurn),
}

struct Turn {
    item: usize,
    model_name: String,
    session: Session,
    engine: Engine,
    _poll: Task<()>,
}

/// The agent's state, kept as a global next to the assistant's.
#[derive(Default)]
pub struct Agent {
    turn: Option<Turn>,
    pub effort: Effort,
    pub auto_apply: bool,
    /// Thinking steps the user opened: (item, step).
    pub expanded: HashSet<(usize, usize)>,
    pub choice: Option<Choice>,
    /// Live model lists by provider (best first, at most four).
    pub catalogs: HashMap<Provider, Vec<ModelInfo>>,
    pub installed: HashMap<Provider, bool>,
    pub menu_open: bool,
    fetching: bool,
}

impl Global for Agent {}

fn agent<'a>(cx: &'a mut Context<Workspace>) -> &'a mut Agent {
    if !cx.has_global::<Agent>() {
        cx.set_global(Agent::default());
    }
    cx.global_mut::<Agent>()
}

/// Set up the choice and fetch the model lists once (in the background).
pub fn init(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if agent(cx).choice.is_some() {
        return;
    }
    let installed = detect(ws);
    let saved = parse_pref(&ws.look.app.assistant_model);
    let has_key = state::get(cx).read(cx).key != KeyState::Missing;
    let choice = saved.filter(|c| installed.get(&c.provider).copied().unwrap_or(false)).unwrap_or_else(|| {
        // No saved choice that can run: a subscription if there is one, else the API.
        let p = [Provider::ClaudeCode, Provider::Codex].into_iter().find(|p| installed[p]).filter(|_| !has_key).unwrap_or(Provider::ClaudeApi);
        Choice::of(&cli::fallback(p)[0])
    });
    let a = agent(cx);
    a.installed = installed;
    a.choice = Some(choice);
    refresh_catalogs(cx);
}

/// Which providers can run: the CLI on disk, or as the bridge last found it.
fn detect(ws: &Workspace) -> HashMap<Provider, bool> {
    let reported = ws.b.ai.agents();
    let found = |kind: AgentKind| reported.iter().any(|s| s.kind == kind && s.installed);
    Provider::ALL
        .into_iter()
        .map(|p| {
            let by_bridge = match p {
                Provider::ClaudeCode => found(AgentKind::ClaudeCode),
                Provider::Codex => found(AgentKind::Codex),
                Provider::ClaudeApi => true,
            };
            (p, cli::available(p) || by_bridge)
        })
        .collect()
}

/// Ask the installed CLIs for their current models.
pub fn refresh_catalogs(cx: &mut Context<Workspace>) {
    let a = agent(cx);
    if a.fetching {
        return;
    }
    a.fetching = true;
    let wanted: Vec<Provider> = [Provider::ClaudeCode, Provider::Codex].into_iter().filter(|p| a.installed.get(p).copied().unwrap_or(false)).collect();
    let task = cx.background_executor().spawn(async move { wanted.into_iter().map(|p| (p, cli::catalog(p))).collect::<Vec<_>>() });
    cx.spawn(async move |ws, cx| {
        let lists = task.await;
        let _ = ws.update(cx, |_, cx| {
            let a = agent(cx);
            a.fetching = false;
            for (p, r) in lists {
                if let Ok(models) = r {
                    a.catalogs.insert(p, models);
                }
            }
            // A choice the provider no longer offers keeps its id but takes the live name.
            if let Some(c) = a.choice.as_mut()
                && let Some(m) = a.catalogs.get(&c.provider).and_then(|l| l.iter().find(|m| m.id == c.model))
            {
                c.name = m.name.clone();
            }
            cx.notify();
        });
    })
    .detach();
}

/// The models offered for `p`: the live list, else the built-in one.
fn models(a: &Agent, p: Provider) -> Vec<ModelInfo> {
    a.catalogs.get(&p).cloned().unwrap_or_else(|| cli::fallback(p))
}

pub fn choice(cx: &mut Context<Workspace>) -> Choice {
    agent(cx).choice.clone().unwrap_or_else(|| Choice::of(&cli::fallback(Provider::ClaudeApi)[0]))
}

pub fn choose(ws: &mut Workspace, m: ModelInfo, cx: &mut Context<Workspace>) {
    // Keep the older flows (/mix, parts) on a matching backend.
    let backend = match m.provider {
        Provider::ClaudeApi => Backend::Claude(llm::MODELS.iter().position(|x| x.id == m.id).unwrap_or(0)),
        Provider::ClaudeCode => Backend::Agent(AgentKind::ClaudeCode),
        Provider::Codex => Backend::Agent(AgentKind::Codex),
    };
    state::set_backend(ws, backend, cx);
    let c = Choice::of(&m);
    ws.look.app.assistant_model = c.pref();
    ws.look.app.save();
    let a = agent(cx);
    a.choice = Some(c);
    a.menu_open = false;
    cx.notify();
}

/// Whether the chosen model can answer right now.
pub fn ready(cx: &mut Context<Workspace>) -> bool {
    let c = choice(cx);
    match c.provider {
        Provider::ClaudeApi => state::get(cx).read(cx).key != KeyState::Missing,
        p => agent(cx).installed.get(&p).copied().unwrap_or(false),
    }
}

/// Whether a turn is running.
pub fn running(cx: &mut Context<Workspace>) -> bool {
    agent(cx).turn.is_some()
}

pub fn cycle_effort(cx: &mut Context<Workspace>) {
    let a = agent(cx);
    a.effort = a.effort.next();
    cx.notify();
}

pub fn toggle_auto(cx: &mut Context<Workspace>) {
    let a = agent(cx);
    a.auto_apply = !a.auto_apply;
    cx.notify();
}

pub fn toggle_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = agent(cx);
    a.menu_open = !a.menu_open;
    let open = a.menu_open;
    if open {
        ws.b.ai.refresh_agents();
        agent(cx).installed = detect(ws);
        refresh_catalogs(cx);
        state::refresh_key(cx);
    }
    cx.notify();
}

pub fn menu_open(cx: &mut Context<Workspace>) -> bool {
    agent(cx).menu_open
}

fn toggle_expanded(key: (usize, usize), cx: &mut Context<Workspace>) {
    let a = agent(cx);
    if !a.expanded.remove(&key) {
        a.expanded.insert(key);
    }
    cx.notify();
}

/// The conversation so far as (is the user, text) turns, oldest first.
fn history(items: &[Placed]) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for p in items {
        let (user, text) = match &p.item {
            Item::User { text, .. } => (true, text.clone()),
            Item::Agent(r) if !r.text().is_empty() => {
                let mut t = r.text();
                if r.changes > 0 {
                    t.push_str(&format!("\n\n[{} change(s) proposed to the user]", r.changes));
                }
                (false, t)
            }
            Item::Reply { text, state: ReplyState::Done | ReplyState::Stopped, .. } if !text.is_empty() => (false, text.clone()),
            _ => continue,
        };
        match out.last_mut() {
            Some((u, t)) if *u == user => {
                t.push_str("\n\n");
                t.push_str(&text);
            }
            _ => out.push((user, text)),
        }
    }
    while out.first().is_some_and(|(u, _)| !u) {
        out.remove(0);
    }
    let skip = out.len().saturating_sub(24);
    let mut out: Vec<_> = out.into_iter().skip(skip).collect();
    while out.first().is_some_and(|(u, _)| !u) {
        out.remove(0);
    }
    out
}

/// The conversation as API messages.
fn api_messages(turns: &[(bool, String)]) -> Vec<Value> {
    turns.iter().map(|(u, t)| json!({ "role": if *u { "user" } else { "assistant" }, "content": [{ "type": "text", "text": t }] })).collect()
}

/// The conversation as one prompt for a CLI (each turn starts fresh).
fn cli_prompt(turns: &[(bool, String)]) -> String {
    let Some(((_, last), earlier)) = turns.split_last() else { return String::new() };
    if earlier.is_empty() {
        return last.clone();
    }
    let mut s = String::from("The conversation so far:\n\n");
    for (u, t) in earlier {
        s.push_str(if *u { "User: " } else { "You: " });
        s.push_str(t);
        s.push_str("\n\n");
    }
    s.push_str("The user's new message:\n\n");
    s.push_str(last);
    s
}

/// Send `text` with the attachments to the chosen model as an agent turn.
pub fn submit(ws: &mut Workspace, text: String, cx: &mut Context<Workspace>) {
    state::stop(ws, cx);
    let a = state::get(cx);
    let attachments = a.read(cx).attachments.clone();
    if attachments.iter().any(|x| matches!(x, Attachment::Image { data: None, .. })) {
        return Workspace::toast(ws, "Still reading an attached image; send again in a moment", cx);
    }
    let chips = attachments.iter().map(Attachment::chip).collect();
    a.update(cx, |a, _| {
        state::push(ws, a, Item::User { text: text.clone(), chips });
        a.attachments.clear();
    });

    let c = choice(cx);
    let mut focus = Focus { selected_track: ws.s.selected_track, selected_clip: ws.s.selected_clip, ..Default::default() };
    let mut images = Vec::new();
    for x in &attachments {
        match x {
            Attachment::Track(id, _) => focus.tracks.push(*id),
            Attachment::Clip(id, _) => focus.clips.push(*id),
            Attachment::Sample { line, .. } => focus.samples.push(line.clone()),
            Attachment::Image { media_type, data: Some(d), .. } => images.push(((*media_type).to_owned(), d.clone())),
            Attachment::Image { .. } => {}
        }
    }
    focus.images = images.len();
    let selected = focus.tracks.first().copied().or(focus.selected_track);
    let snapshot = llm::context::producer_context(&ws.s.project, &focus);
    let snapshot = snapshot.strip_prefix(llm::context::PERSONA).unwrap_or(&snapshot).trim_start().to_owned();
    let project = format!("# The open project (at the start of this request)\n{snapshot}");
    let turns = a.read_with(cx, |a, _| history(&a.items));
    let effort = agent(cx).effort;

    let engine = match c.provider {
        Provider::ClaudeApi => {
            let mut messages = api_messages(&turns);
            // The newest user turn gets the screenshots before its text.
            if let Some(last) = messages.last_mut().filter(|m| m["role"] == "user")
                && let Some(content) = last["content"].as_array_mut()
            {
                let mut all: Vec<Value> = images
                    .iter()
                    .map(|(mime, d)| json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": base64(d) } }))
                    .collect();
                all.append(content);
                *content = all;
            }
            let system = vec![tools::AGENT_PERSONA.to_owned(), project];
            let stream = api::spawn(AgentRequest { model: c.model.clone(), system: system.clone(), messages: messages.clone(), tools: tools::definitions(), max_tokens: 64_000, effort });
            Engine::Api { system, messages, model: c.model.clone(), stream: Some(stream), rounds: 0 }
        }
        p => Engine::Cli(cli::start(TurnSpec {
            provider: p,
            model: c.model.clone(),
            effort,
            system: format!("{}\n\n{project}", tools::AGENT_PERSONA),
            prompt: cli_prompt(&turns),
            images,
            tools: tools::definitions(),
        })),
    };
    let item = a.update(cx, |a, _| {
        state::push(
            ws,
            a,
            Item::Agent(Reply { model: c.name.clone(), vendor: c.provider.vendor(), steps: Vec::new(), state: ReplyState::Waiting, started: Instant::now(), elapsed: None, changes: 0 }),
        )
    });
    let poll = cx.spawn(async move |ws, cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            if !ws.update(cx, poll).unwrap_or(false) {
                break;
            }
        }
    });
    agent(cx).turn = Some(Turn { item, model_name: c.name, session: Session::new(&ws.s.project, selected), engine, _poll: poll });
    cx.notify();
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Edit the reply of item `item`.
fn with_reply<R>(item: usize, cx: &mut Context<Workspace>, f: impl FnOnce(&mut Reply) -> R) -> Option<R> {
    let a = state::get(cx);
    a.update(cx, |a, _| match a.items.get_mut(item) {
        Some(Placed { item: Item::Agent(r), .. }) => Some(f(r)),
        _ => None,
    })
}

fn close_thinking(r: &mut Reply) {
    for s in &mut r.steps {
        if let Step::Thinking { done, .. } = s {
            *done = true;
        }
    }
}

fn push_text(r: &mut Reply, t: &str) {
    r.state = ReplyState::Streaming;
    match r.steps.last_mut() {
        Some(Step::Text(s)) => s.push_str(t),
        _ => {
            close_thinking(r);
            r.steps.push(Step::Text(t.to_owned()));
        }
    }
}

fn push_thinking(r: &mut Reply, t: &str) {
    r.state = ReplyState::Streaming;
    match r.steps.last_mut() {
        Some(Step::Thinking { text, done: false }) => text.push_str(t),
        _ => r.steps.push(Step::Thinking { text: t.to_owned(), done: false }),
    }
}

/// Run one tool on the draft: its result text and whether it worked.
fn run_tool(s: &mut Session, name: &str, input: &Value) -> (String, bool) {
    match tools::execute(s, name, input) {
        Outcome::Ok(t) => (t, true),
        Outcome::Err(e) => (e, false),
        Outcome::Delegate => match generate_part(s, input) {
            Ok(t) => (t, true),
            Err(e) => (e, false),
        },
    }
}

/// Settle a tool's line: the first running one with its name, else a new one.
fn settle_tool(item: usize, name: &str, text: &str, ok: bool, cx: &mut Context<Workspace>) {
    let shown = if ok && tools::is_read_only(name) { String::new() } else { text.to_owned() };
    with_reply(item, cx, |r| {
        let st = if ok { ToolState::Done(shown) } else { ToolState::Failed(text.to_owned()) };
        match r.steps.iter_mut().find(|s| matches!(s, Step::Tool { name: n, state: ToolState::Running } if n == name)) {
            Some(Step::Tool { state, .. }) => *state = st,
            _ => {
                close_thinking(r);
                r.steps.push(Step::Tool { name: name.to_owned(), state: st });
            }
        }
    });
}

/// Drain the running turn; `false` once it is over.
fn poll(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let Some(turn) = agent(cx).turn.as_ref() else { return false };
    let keep = match &turn.engine {
        Engine::Api { .. } => poll_api(ws, cx),
        Engine::Cli(_) => poll_cli(ws, cx),
    };
    if keep {
        state::get(cx).update(cx, |a, _| a.scroll.scroll_to_bottom());
    }
    cx.notify();
    keep
}

fn poll_cli(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let Some(Turn { item, engine: Engine::Cli(t), .. }) = agent(cx).turn.as_ref() else { return false };
    let item = *item;
    let mut events = Vec::new();
    let mut gone = false;
    loop {
        match t.events.try_recv() {
            Ok(e) => events.push(e),
            Err(crossbeam_channel::TryRecvError::Empty) => break,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                gone = true;
                break;
            }
        }
    }
    for e in events {
        match e {
            CliEvent::Text(t) => {
                with_reply(item, cx, |r| push_text(r, &t));
            }
            CliEvent::Thinking(t) => {
                with_reply(item, cx, |r| push_thinking(r, &t));
            }
            CliEvent::Tool { name, input, reply } => {
                with_reply(item, cx, |r| {
                    r.state = ReplyState::Streaming;
                    close_thinking(r);
                    r.steps.push(Step::Tool { name: name.clone(), state: ToolState::Running });
                });
                let Some(turn) = agent(cx).turn.as_mut() else { return false };
                let (text, ok) = run_tool(&mut turn.session, &name, &input);
                settle_tool(item, &name, &text, ok, cx);
                let _ = reply.send((text, !ok));
            }
            CliEvent::Done(r) => {
                finish(ws, r.map_or_else(ReplyState::Failed, |()| ReplyState::Done), cx);
                return false;
            }
        }
    }
    if gone {
        finish(ws, ReplyState::Failed("The request ended without an answer.".into()), cx);
        return false;
    }
    true
}

fn poll_api(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let Some(Turn { item, engine: Engine::Api { stream: Some(stream), .. }, .. }) = agent(cx).turn.as_ref() else { return true };
    let item = *item;
    let mut events = Vec::new();
    let mut gone = false;
    loop {
        match stream.events.try_recv() {
            Ok(e) => events.push(e),
            Err(crossbeam_channel::TryRecvError::Empty) => break,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                gone = true;
                break;
            }
        }
    }
    for e in events {
        match e {
            AgentEvent::Text(t) => {
                with_reply(item, cx, |r| push_text(r, &t));
            }
            AgentEvent::Thinking(t) => {
                with_reply(item, cx, |r| push_thinking(r, &t));
            }
            AgentEvent::ToolStart { name, .. } => {
                with_reply(item, cx, |r| {
                    r.state = ReplyState::Streaming;
                    close_thinking(r);
                    r.steps.push(Step::Tool { name, state: ToolState::Running });
                });
            }
            AgentEvent::Done { stop_reason, content, calls } => {
                step_done(ws, item, stop_reason, content, calls, cx);
                return agent(cx).turn.is_some();
            }
            AgentEvent::Error(m) => {
                finish(ws, ReplyState::Failed(m), cx);
                return false;
            }
        }
    }
    if gone {
        finish(ws, ReplyState::Failed("The request ended without an answer.".into()), cx);
        return false;
    }
    true
}

/// One API step ended: run its tools and ask again, or end the turn.
fn step_done(ws: &mut Workspace, item: usize, stop_reason: Option<String>, content: Vec<Value>, calls: Vec<api::ToolCall>, cx: &mut Context<Workspace>) {
    with_reply(item, cx, close_thinking);
    let end = match stop_reason.as_deref() {
        Some("tool_use") if !calls.is_empty() => None,
        Some("refusal") => Some(ReplyState::Failed("The model declined to continue with that request.".into())),
        Some("max_tokens") => Some(ReplyState::Failed("The reply ran out of room before it finished.".into())),
        _ => Some(ReplyState::Done),
    };
    let Some(mut turn) = agent(cx).turn.take() else { return };
    let Engine::Api { messages, stream, .. } = &mut turn.engine else { return };
    *stream = None;
    if !content.is_empty() {
        messages.push(json!({ "role": "assistant", "content": content }));
    }
    if let Some(end) = end {
        agent(cx).turn = Some(turn);
        return finish(ws, end, cx);
    }
    let mut results = Vec::new();
    for call in &calls {
        let (text, ok) = match &call.input {
            Err(raw) => (api::invalid_json(raw), false),
            Ok(input) => run_tool(&mut turn.session, &call.name, input),
        };
        settle_tool(item, &call.name, &text, ok, cx);
        results.push((call.id.clone(), text, !ok));
    }
    let Engine::Api { system, messages, model, stream, rounds } = &mut turn.engine else { return };
    messages.push(api::tool_results(&results));
    *rounds += 1;
    if *rounds >= MAX_ROUNDS {
        agent(cx).turn = Some(turn);
        return finish(ws, ReplyState::Failed(format!("Stopped after {MAX_ROUNDS} tool rounds; ask again to continue.")), cx);
    }
    let effort = agent(cx).effort;
    *stream = Some(api::spawn(AgentRequest { model: model.clone(), system: system.clone(), messages: messages.clone(), tools: tools::definitions(), max_tokens: 64_000, effort }));
    agent(cx).turn = Some(turn);
}

/// `generate_part`: the pattern engine writes the part into the draft.
fn generate_part(s: &mut Session, input: &Value) -> Result<String, String> {
    let kind_s = input.get("kind").and_then(Value::as_str).unwrap_or("bass").to_lowercase();
    let kind = match kind_s.as_str() {
        "beat" | "drums" => Kind::Drums,
        other => Kind::ALL.into_iter().find(|k| k.label().to_lowercase() == other).unwrap_or_default(),
    };
    let text = input.get("description").and_then(Value::as_str).unwrap_or_default();
    let track = match input.get("track").filter(|v| !v.is_null()) {
        Some(v) => Some(tools::refs::track(&s.draft, v, s.selected)?),
        None => s.selected,
    };
    let prepared = midi_gen::prepare(&s.draft, track, None, Some(kind), text)?;
    let seed = prepared.req.seed.unwrap_or_else(seed);
    let g = gen_core::generate(&prepared.req, &prepared.ctx, seed);
    if g.draft.notes.is_empty() {
        return Err("the pattern engine wrote no notes for that; try other words or write_clip".into());
    }
    let (cmds, lines) = midi_gen::commands(&mut s.draft, &g, prepared.dest, prepared.start);
    let label = format!("{} part, {} bars", prepared.req.kind.label(), prepared.req.bars);
    s.apply(&label, cmds)?;
    Ok(format!("{label}. {}", lines.join("; ")))
}

fn seed() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    gen_core::rng::splitmix64(now)
}

/// End the turn: settle the reply and offer what changed.
fn finish(ws: &mut Workspace, end: ReplyState, cx: &mut Context<Workspace>) {
    let Some(turn) = agent(cx).turn.take() else { return };
    match &turn.engine {
        Engine::Api { stream: Some(s), .. } => s.cancel.cancel(),
        Engine::Cli(t) => t.cancel.cancel(),
        _ => {}
    }
    let changes = turn.session.changes.len();
    with_reply(turn.item, cx, |r| {
        close_thinking(r);
        for s in &mut r.steps {
            if let Step::Tool { state: st @ ToolState::Running, .. } = s {
                *st = ToolState::Failed("not run".into());
            }
        }
        r.state = end;
        r.elapsed = Some(r.started.elapsed());
        r.changes = changes;
    });
    if changes > 0 {
        offer(ws, turn.session, &turn.model_name, cx);
    }
    state::get(cx).update(cx, |a, _| a.scroll.scroll_to_bottom());
    cx.notify();
}

/// The draft's changes as one proposal card (accepted at once in auto mode).
fn offer(ws: &mut Workspace, session: Session, model: &str, cx: &mut Context<Workspace>) {
    // Reserve the draft's ids so the user's own edits meanwhile cannot take them.
    ws.s.project.ids.observe(session.draft.ids.last());
    let n = session.changes.len();
    let label = if n == 1 { session.changes[0].clone() } else { format!("{n} changes by the assistant") };
    let destructive = session.is_destructive();
    let Some(id) = ws.s.propose(&label, Origin::Agent(model.to_owned()), session.commands) else {
        let why = ws.s.last_error.clone().unwrap_or_default();
        return state::note(ws, format!("The project changed while the assistant worked, so its edits no longer fit ({why}). Ask again."), cx);
    };
    ws.chat_log.push(ChatEntry::Proposal { id, label, origin: model.to_owned(), summary: session.changes, destructive, status: ProposalStatus::Pending });
    if agent(cx).auto_apply && !destructive {
        ws.resolve_proposal(id, true, cx);
    }
}

/// Stop the running turn, keeping what arrived and what it already changed.
pub fn stop(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if running(cx) {
        finish(ws, ReplyState::Stopped, cx);
    }
}

// ── View ──

/// A vendor's logo on its brand tile.
pub fn logo(vendor: Vendor, side: f32) -> Div {
    let glyph = match vendor {
        Vendor::Anthropic => Icon::LogoClaude,
        Vendor::OpenAi => Icon::LogoOpenAi,
    };
    div()
        .size(px(side))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px((side * 0.3).round()))
        .bg(brand(vendor))
        .child(svg().path(glyph.path()).size(px(side * 0.86)).text_color(gpui::white()))
}

/// What a finished tool line says: the result, without the ids the model needs.
fn plain(result: &str) -> String {
    let mut s = result.lines().next().unwrap_or_default().to_owned();
    // Drop "(device #41)", "(clip #44, placement #45)" and ": track #40" style tails.
    while let Some(open) = s.find(" (")
        && let Some(close) = s[open..].find(')').map(|c| open + c)
        && s[open..close].contains('#')
    {
        s.replace_range(open..=close, "");
    }
    for tail in [": track #", ": group #", " as device #"] {
        if let Some(i) = s.find(tail) {
            s.truncate(i);
        }
    }
    s
}

fn shimmer(id: (usize, usize), el: impl IntoElement + 'static) -> AnyElement {
    div()
        .child(el)
        .with_animation(("agent-shimmer", id.0 * 1024 + id.1), Animation::new(motion::NORMAL * 5).repeat(), |d, t| d.opacity(0.4 + 0.6 * (1. - (t * 2. - 1.).abs())))
        .into_any_element()
}

fn thinking_line(i: usize, j: usize, text: &str, done: bool, open: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let has_text = !text.trim().is_empty();
    let head = div()
        .id(("think", i * 1024 + j))
        .h(px(20.))
        .flex()
        .items_center()
        .gap(px(SP1))
        .when(has_text, |d| d.cursor_pointer())
        .child(label(if done { "Thought" } else { "Thinking…" }, TEXT_SM, pal.faint))
        .when(has_text, |d| d.child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 10., pal.faint)))
        .on_click(cx.listener(move |_, _, _, cx| toggle_expanded((i, j), cx)));
    let head: AnyElement = if done { head.into_any_element() } else { shimmer((i, j), head) };
    div()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(head)
        .when(open && has_text, |d| {
            d.child(div().pl(px(SP2)).border_l_1().border_color(border()).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.faint).child(text.trim().to_owned()))
        })
        .into_any_element()
}

fn tool_line(i: usize, j: usize, name: &str, st: &ToolState) -> AnyElement {
    let pal = p();
    let row = |ic: AnyElement, text: String, color| div().min_h(px(20.)).flex().items_center().gap(px(SP2)).child(ic).child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(color).child(text));
    match st {
        ToolState::Running => shimmer((i, j), row(icon(Icon::Loader, 12., pal.accent).into_any_element(), format!("{}…", tools::title(name)), pal.text)),
        ToolState::Done(t) => {
            let text = if t.is_empty() { tools::title(name).to_owned() } else { plain(t) };
            row(icon(Icon::Check, 12., pal.faint).into_any_element(), text, pal.muted).into_any_element()
        }
        ToolState::Failed(e) => div()
            .flex()
            .flex_col()
            .child(row(icon(Icon::X, 12., pal.record).into_any_element(), tools::title(name).to_owned(), pal.muted))
            .child(div().pl(px(20.)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(with_alpha(pal.record, 0.8)).child(e.lines().next().unwrap_or_default().to_owned()))
            .into_any_element(),
    }
}

/// An agent reply: no avatar, its steps in order, a quiet footer.
pub fn view(i: usize, r: &Reply, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let streaming = matches!(r.state, ReplyState::Waiting | ReplyState::Streaming);
    let expanded = agent(cx).expanded.clone();
    let mut col = div().flex().flex_col().gap(px(SP2));
    let last = r.steps.len().saturating_sub(1);
    let mut j = 0;
    while j < r.steps.len() {
        match &r.steps[j] {
            Step::Thinking { text, done } => col = col.child(thinking_line(i, j, text, *done, expanded.contains(&(i, j)), cx)),
            Step::Text(t) => col = col.child(markdown::view(i * 64 + j, t, streaming && j == last, cx)),
            Step::Tool { name, state: st } => {
                // A run of finished lookups reads as one quiet line.
                let quiet = |s: &Step| matches!(s, Step::Tool { name, state: ToolState::Done(_) } if tools::is_read_only(name));
                let run = r.steps[j..].iter().take_while(|s| quiet(s)).count();
                if run > 1 {
                    col = col.child(div().h(px(20.)).flex().items_center().gap(px(SP2)).child(icon(Icon::Search, 12., pal.faint)).child(label(format!("Looked through the project ({run})"), TEXT_SM, pal.faint)));
                    j += run;
                    continue;
                }
                col = col.child(tool_line(i, j, name, st));
            }
        }
        j += 1;
    }
    // While nothing is visibly moving, say that it is working.
    let idle = r.steps.last().is_none_or(|s| matches!(s, Step::Tool { state: ToolState::Done(_) | ToolState::Failed(_), .. } | Step::Thinking { done: true, .. }));
    if streaming && idle {
        col = col.child(shimmer((i, 999), label("Working…", TEXT_SM, pal.faint)));
    }
    let footer: Option<AnyElement> = match &r.state {
        ReplyState::Waiting | ReplyState::Streaming => None,
        ReplyState::Failed(m) => Some(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.record).child(if m.is_empty() { "No answer".to_owned() } else { m.clone() }).into_any_element()),
        end => {
            let text = r.text();
            let mut parts = vec![r.model.clone()];
            if let Some(d) = r.elapsed {
                parts.push(format!("{} {:.0} s", if *end == ReplyState::Stopped { "stopped after" } else { "worked" }, d.as_secs_f32()));
            }
            if r.changes > 0 {
                parts.push(format!("{} change{}", r.changes, if r.changes == 1 { "" } else { "s" }));
            }
            let mut row = div().h(px(20.)).flex().items_center().gap(px(SP2)).child(logo(r.vendor, 14.)).child(label(parts.join(" · "), TEXT_XS, pal.faint)).child(div().flex_1());
            if !text.is_empty() {
                row = row.child(widgets::sized_icon_button(("copy-reply", i), Icon::Copy, false, 20., 12.).tooltip(widgets::tip("Copy", None)).on_click(cx.listener(move |ws, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                    ws.toast("Copied", cx);
                })));
            }
            Some(row.into_any_element())
        }
    };
    col.children(footer).font_weight(FontWeight::NORMAL).into_any_element()
}

/// The composer's model button: logo and name.
pub fn model_pill(cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let c = choice(cx);
    let open = menu_open(cx);
    div()
        .id("agent-model")
        .h(px(24.))
        .pl(px(SP1))
        .pr(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if open { content(0.1) } else { gpui::transparent_black() })
        .hover(|s| s.bg(content(0.08)))
        .child(logo(c.provider.vendor(), 16.))
        .child(label(crate::paint::fit(c.name.trim_start_matches("Claude "), 80., TEXT_XS), TEXT_XS, pal.text))
        .child(icon(Icon::ChevronDown, 10., pal.faint))
        .tooltip(widgets::tip("Model", None))
        .on_click(cx.listener(|ws, _, _, cx| toggle_menu(ws, cx)))
        .into_any_element()
}

/// Effort and review / auto-apply, as two quiet toggles.
pub fn switches(cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (effort, auto) = (agent(cx).effort, agent(cx).auto_apply);
    let pill = |id: &'static str, ic: Icon, text: Option<&'static str>, on: bool| {
        div()
            .id(id)
            .h(px(24.))
            .px(px(SP1))
            .gap(px(SP1))
            .flex()
            .items_center()
            .rounded(px(r_md()))
            .cursor_pointer()
            .hover(|s| s.bg(content(0.08)))
            .child(icon(ic, 12., if on { pal.accent } else { pal.faint }))
            .children(text.map(|t| label(t, TEXT_XS, if on { pal.text } else { pal.muted })))
    };
    div()
        .flex()
        .items_center()
        .child(pill("agent-effort", Icon::Gauge, Some(effort.label()), effort != Effort::Low).tooltip(widgets::tip("Thinking: Quick, Balanced, Deep, Max", None)).on_click(cx.listener(|_, _, _, cx| cycle_effort(cx))))
        .child(
            pill("agent-auto", if auto { Icon::Check } else { Icon::Eye }, None, auto)
                .tooltip(widgets::tip(if auto { "Auto-apply: edits land at once (one undo step); deletions still wait" } else { "Review: edits wait for you on a card" }, None))
                .on_click(cx.listener(|_, _, _, cx| toggle_auto(cx))),
        )
        .into_any_element()
}

/// The model picker: each provider's four newest models, with its logo.
pub fn model_menu(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !menu_open(cx) {
        return None;
    }
    let pal = p();
    let current = choice(cx);
    let key = state::get(cx).read(cx).key;
    let _ = ws;
    // Scrolls when the lists are taller than the room above the composer.
    let mut rows = div().id("agent-model-list").max_h(px(360.)).overflow_y_scroll().flex().flex_col();
    for (k, provider) in Provider::ALL.into_iter().enumerate() {
        let a = agent(cx);
        let installed = a.installed.get(&provider).copied().unwrap_or(false);
        let list = models(a, provider);
        let live = a.catalogs.contains_key(&provider);
        let fetching = a.fetching;
        let (title, status) = match provider {
            Provider::ClaudeCode => ("Claude", if installed { "Your subscription" } else { "Claude Code not installed" }),
            Provider::Codex => ("ChatGPT", if installed { "Your subscription" } else { "Codex not installed" }),
            Provider::ClaudeApi => ("Anthropic API", if key == KeyState::Missing { "No API key" } else { "Your API key" }),
        };
        let usable = match provider {
            Provider::ClaudeApi => key != KeyState::Missing,
            _ => installed,
        };
        rows = rows.child(
            div()
                .when(k > 0, |d| d.mt(px(SP1)).pt(px(SP2)).border_t_1().border_color(border()))
                .px(px(SP2))
                .pb(px(SP1))
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(logo(provider.vendor(), 16.))
                .child(label(title, TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(label(if usable && installed && !live && fetching && provider != Provider::ClaudeApi { "Loading…" } else { status }, TEXT_XS, pal.faint)),
        );
        for (n, m) in list.into_iter().enumerate() {
            let on = current.provider == provider && current.model == m.id;
            let item = div()
                .id(("agent-model-row", k * 16 + n))
                .h(px(28.))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .when(!usable, |d| d.opacity(0.45))
                .child(label(m.name.clone(), TEXT_SM, pal.text).flex_1().min_w(px(0.)))
                .when(on, |d| d.child(icon(Icon::Check, 12., pal.accent)));
            rows = rows.child(if usable {
                item.on_click(cx.listener(move |ws, _, _, cx| choose(ws, m.clone(), cx)))
            } else {
                item.on_click(cx.listener(move |ws, _, _, cx| {
                    let how = match provider {
                        Provider::ClaudeCode => format!("{} Then open the model menu again.", AgentKind::ClaudeCode.setup()),
                        Provider::Codex => format!("{} Then open the model menu again.", AgentKind::Codex.setup()),
                        Provider::ClaudeApi => "Copy an Anthropic API key and choose \"Use API key from clipboard\" in the model menu.".to_owned(),
                    };
                    state::note(ws, how, cx);
                    toggle_menu(ws, cx);
                }))
            });
        }
        if provider == Provider::ClaudeApi {
            rows = rows.child(widgets::menu_item("agent-key-paste", Some(Icon::Paste), "Use API key from clipboard", None, false).on_click(cx.listener(|ws, _, _, cx| {
                state::key_from_clipboard(ws, cx);
                toggle_menu(ws, cx);
            })));
            if key == KeyState::Keychain {
                rows = rows.child(widgets::menu_item("agent-key-forget", Some(Icon::Trash), "Forget the stored key", None, false).on_click(cx.listener(|ws, _, _, cx| {
                    state::forget_key(cx);
                    toggle_menu(ws, cx);
                })));
            }
        }
    }
    Some(
        div()
            .id("agent-model-menu-wrap")
            .absolute()
            .bottom(px(112.))
            .left(px(SP3))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(motion::rise_in("agent-model-menu-in", widgets::menu_card("agent-model-menu", 240.).child(rows), 4.))
            .into_any_element(),
    )
}

/// What an empty chat offers: a few requests that show what the agent does.
const SUGGESTIONS: [(Icon, &str, &str); 4] = [
    (Icon::Piano, "Write a bassline that locks with the kick", "Create a bass track and write a rolling 4-bar bassline in the project key that locks with the kick, then loop it for 16 bars."),
    (Icon::Mixer, "Mix the drums", "Balance and process the drum tracks: gain staging, EQ, compression, a drum bus and a shared reverb."),
    (Icon::Timeline, "Turn this into an arrangement", "Turn what is here into an arrangement: 16-bar intro, build, drop, break and outro, with filter automation into the drop."),
    (Icon::Gauge, "Master the song", "Master the song on the Main track for club playback: EQ, glue compression, saturation and a limiter at -1 dBTP."),
];

/// The empty chat.
pub fn welcome(ready: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let c = choice(cx);
    let mut list = div().flex().flex_col().gap(px(SP1));
    for (k, (ic, title, prompt)) in SUGGESTIONS.into_iter().enumerate() {
        list = list.child(
            div()
                .id(("suggest", k))
                .h(px(32.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .child(icon(ic, 14., pal.faint))
                .child(label(title, TEXT_SM, pal.muted))
                .on_click(cx.listener(move |ws, _, _, cx| state::submit_text(ws, prompt.to_owned(), cx))),
        );
    }
    div()
        .pt(px(SP6))
        .flex()
        .flex_col()
        .gap(px(SP4))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(SP2))
                .child(logo(c.provider.vendor(), 28.))
                .child(label("What are we making?", TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.faint).child(if ready {
                    "Tracks, parts, arrangement, sound design, mixing and mastering. Every change waits for your review."
                } else {
                    "Pick a model below: your Claude or ChatGPT subscription, or an Anthropic API key."
                })),
        )
        .child(list)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn history_keeps_user_and_answers_in_turns() {
        let mk = |item| Placed { anchor: 0, item };
        let reply = Reply {
            model: "m".into(),
            vendor: Vendor::Anthropic,
            steps: vec![Step::Thinking { text: "x".into(), done: true }, Step::Text("Done.".into())],
            state: ReplyState::Done,
            started: Instant::now(),
            elapsed: None,
            changes: 2,
        };
        let items = vec![mk(Item::Note("hi".into())), mk(Item::User { text: "make a bass".into(), chips: vec![] }), mk(Item::Agent(reply)), mk(Item::User { text: "louder".into(), chips: vec![] })];
        let h = history(&items);
        assert_eq!(h.len(), 3);
        let m = api_messages(&h);
        assert_eq!(m[0]["role"], "user");
        assert!(m[1]["content"][0]["text"].as_str().unwrap().contains("2 change(s)"));
        let p = cli_prompt(&h);
        assert!(p.starts_with("The conversation so far") && p.ends_with("louder"), "{p}");
        assert_eq!(cli_prompt(&h[..1]), "make a bass");
    }

    #[test]
    fn saved_choices_parse_old_and_new_forms() {
        assert_eq!(parse_pref("claude-code:fable").unwrap().name, "Fable 5.2");
        assert_eq!(parse_pref("codex:gpt-6-luna").unwrap().provider, Provider::Codex);
        assert_eq!(parse_pref("claude-opus-5-5").unwrap().provider, Provider::ClaudeApi);
        assert_eq!(parse_pref("agent:codex").unwrap().provider, Provider::Codex);
        assert!(parse_pref("").is_none());
        let c = parse_pref("codex:gpt-7").unwrap();
        assert_eq!((c.model.as_str(), c.pref().as_str()), ("gpt-7", "codex:gpt-7"));
    }

    #[test]
    fn tool_lines_hide_ids() {
        assert_eq!(plain("New MIDI track \"Bass\" with Acid 303 (device #41): track #40"), "New MIDI track \"Bass\" with Acid 303");
        assert_eq!(plain("Bass: \"Rolling\", 28 notes, bars 1–5 (clip #44, placement #45)"), "Bass: \"Rolling\", 28 notes, bars 1–5");
        assert_eq!(plain("Bass: add Compressor as device #46 at position 1"), "Bass: add Compressor");
        assert_eq!(plain("Kick: fader -6.0 dB (gentle)"), "Kick: fader -6.0 dB (gentle)");
    }
}
