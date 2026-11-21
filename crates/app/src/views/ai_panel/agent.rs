//! The assistant as an agent on the user's own subscription: Claude Code
//! (Claude) or Codex (ChatGPT) reads and edits the project with AI-Track's
//! tools (`aitrack_ai::llm::tools`) until the request is done. Replies
//! stream in as steps: thinking, one line per tool call (opened for its
//! input and result), questions to the user, and Markdown text. Every edit
//! lands in a draft; when the turn ends the changes are offered as one
//! proposal, or accepted at once in auto mode.
//!
//! A CLI takes a second or two to start, so one is warmed up while the user
//! types ([`prewarm`]) and the turn only has to send the message.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use aitrack_ai::llm::agent::Effort;
use aitrack_ai::llm::cli::{self, CliEvent, CliTurn, ModelInfo, Provider, TurnSpec, Vendor};
use aitrack_ai::llm::tools::{self, Outcome, Question, Session};
use aitrack_ai::llm::{self, context::Focus};
use aitrack_model::Origin;
use crossbeam_channel::Sender;
use gpui::{Context, Global, Task};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::state::{self, Attachment, Backend, Item, Placed, ReplyState};
use crate::bridge::AgentKind;
use crate::workspace::{ChatEntry, ProposalStatus, Workspace};

/// How much of a tool's result a reply keeps (and the project file stores).
const KEEP_RESULT: usize = 4_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolState {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Step {
    Thinking { text: String, done: bool },
    Text(String),
    /// A tool call; `input` is `Null` until the model has written it.
    Tool {
        name: String,
        #[serde(default)]
        input: Value,
        state: ToolState,
    },
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
    pub(super) fn of(m: &ModelInfo) -> Self {
        Self { provider: m.provider, model: m.id.clone(), name: m.name.clone() }
    }

    /// The preference string: `provider:model@effort`.
    fn pref(&self, effort: Effort) -> String {
        format!("{}:{}@{}", self.provider.id(), self.model, effort.id())
    }
}

/// The providers the picker offers: the user's own subscriptions.
pub const PROVIDERS: [Provider; 2] = [Provider::ClaudeCode, Provider::Codex];

fn parse_effort(s: &str) -> Option<Effort> {
    Effort::ALL.into_iter().find(|e| e.id() == s)
}

/// Read a saved choice and its effort. Older saves (`provider:model`,
/// `agent:…`) still work; an API model is no longer offered.
fn parse_pref(s: &str) -> (Option<Choice>, Option<Effort>) {
    let (s, effort) = match s.rsplit_once('@') {
        Some((head, e)) if parse_effort(e).is_some() => (head, parse_effort(e)),
        _ => (s, None),
    };
    let find = |p: Provider, id: &str| cli::fallback(p).into_iter().find(|m| m.id == id).map(|m| Choice::of(&m));
    let choice = if let Some((p, model)) = s.split_once(':')
        && let Some(p) = Provider::parse(p)
    {
        find(p, model).or_else(|| Some(Choice { provider: p, model: model.to_owned(), name: model.to_owned() }))
    } else {
        match s {
            "agent:claude-code" => cli::fallback(Provider::ClaudeCode).first().map(Choice::of),
            "agent:codex" => cli::fallback(Provider::Codex).first().map(Choice::of),
            _ => None,
        }
    };
    (choice.filter(|c| PROVIDERS.contains(&c.provider)), effort)
}

struct Turn {
    item: usize,
    model_name: String,
    session: Session,
    cli: CliTurn,
    _poll: Task<()>,
}

/// A question the model asked; the turn waits for the answer.
pub struct Pending {
    pub item: usize,
    pub step: usize,
    pub question: Question,
    /// Options picked so far (several when the question allows it).
    pub picked: Vec<usize>,
    reply: Sender<(String, bool)>,
}

/// The agent's state, kept as a global next to the assistant's.
#[derive(Default)]
pub struct Agent {
    turn: Option<Turn>,
    pub effort: Effort,
    pub auto_apply: bool,
    /// Steps the user opened (thinking, tool calls, lookup groups): (item, step).
    pub expanded: HashSet<(usize, usize)>,
    pub choice: Option<Choice>,
    /// Live model lists by provider (best first, at most four).
    pub catalogs: HashMap<Provider, Vec<ModelInfo>>,
    pub installed: HashMap<Provider, bool>,
    pub menu_open: bool,
    /// When a click outside closed the menu (so the pill's own click does not reopen it).
    pub menu_closed_at: Option<Instant>,
    /// The model under the pointer in the picker: its card shows beside the list.
    pub hover: Option<ModelInfo>,
    pub pending: Option<Pending>,
    pub(super) fetching: bool,
    /// What the last warm-up was for, so typing does not ask again.
    warmed: Option<(String, Instant)>,
}

impl Global for Agent {}

pub(super) fn agent<'a>(cx: &'a mut Context<Workspace>) -> &'a mut Agent {
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
    let (saved, effort) = parse_pref(&ws.look.app.assistant_model);
    let choice = saved.filter(|c| installed.get(&c.provider).copied().unwrap_or(false)).unwrap_or_else(|| {
        // No saved choice that can run: the first subscription that can.
        let p = PROVIDERS.into_iter().find(|p| installed[p]).unwrap_or(Provider::ClaudeCode);
        Choice::of(&cli::fallback(p)[0])
    });
    let a = agent(cx);
    a.installed = installed;
    a.choice = Some(choice);
    if let Some(e) = effort {
        a.effort = e;
    }
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
                Provider::ClaudeApi => false,
            };
            (p, p != Provider::ClaudeApi && (cli::available(p) || by_bridge))
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
    let wanted: Vec<Provider> = PROVIDERS.into_iter().filter(|p| a.installed.get(p).copied().unwrap_or(false)).collect();
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
pub(super) fn models(a: &Agent, p: Provider) -> Vec<ModelInfo> {
    a.catalogs.get(&p).cloned().unwrap_or_else(|| cli::fallback(p))
}

pub fn choice(cx: &mut Context<Workspace>) -> Choice {
    agent(cx).choice.clone().unwrap_or_else(|| Choice::of(&cli::fallback(Provider::ClaudeCode)[0]))
}

/// Pick a model (and its effort, when the card set one).
pub fn choose(ws: &mut Workspace, m: ModelInfo, effort: Option<Effort>, close: bool, cx: &mut Context<Workspace>) {
    // Keep the older flows (/mix, parts) on a matching backend.
    let backend = match m.provider {
        Provider::Codex => Backend::Agent(AgentKind::Codex),
        _ => Backend::Agent(AgentKind::ClaudeCode),
    };
    state::set_backend(ws, backend, cx);
    let c = Choice::of(&m);
    let a = agent(cx);
    if let Some(e) = effort {
        a.effort = e;
    }
    ws.look.app.assistant_model = c.pref(a.effort);
    ws.look.app.save();
    if a.choice.as_ref() != Some(&c) {
        cli::cool_down();
        a.warmed = None;
    }
    a.choice = Some(c);
    if close {
        a.menu_open = false;
        a.hover = None;
    }
    cx.notify();
}

/// Whether the chosen model can answer right now.
pub fn ready(cx: &mut Context<Workspace>) -> bool {
    let c = choice(cx);
    agent(cx).installed.get(&c.provider).copied().unwrap_or(false)
}

/// Whether a turn is running.
pub fn running(cx: &mut Context<Workspace>) -> bool {
    agent(cx).turn.is_some()
}

pub fn toggle_auto(cx: &mut Context<Workspace>) {
    let a = agent(cx);
    a.auto_apply = !a.auto_apply;
    cx.notify();
}

pub fn toggle_menu(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let a = agent(cx);
    // The press that closed it from outside landed on the pill: stay closed.
    if !a.menu_open && a.menu_closed_at.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) {
        a.menu_closed_at = None;
        return;
    }
    a.menu_open = !a.menu_open;
    a.hover = None;
    let open = a.menu_open;
    if open {
        ws.b.ai.refresh_agents();
        agent(cx).installed = detect(ws);
        refresh_catalogs(cx);
    }
    cx.notify();
}

pub fn close_menu(cx: &mut Context<Workspace>) {
    let a = agent(cx);
    if a.menu_open {
        a.menu_open = false;
        a.hover = None;
        a.menu_closed_at = Some(Instant::now());
        cx.notify();
    }
}

pub fn menu_open(cx: &mut Context<Workspace>) -> bool {
    agent(cx).menu_open
}

pub(super) fn toggle_expanded(key: (usize, usize), cx: &mut Context<Workspace>) {
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

/// What a CLI turn for the current choice starts with. The instructions
/// stay the same from turn to turn (the project goes in the message), so
/// a process warmed up before the user sends can take the turn.
fn cli_spec(c: &Choice, effort: Effort) -> TurnSpec {
    TurnSpec { provider: c.provider, model: c.model.clone(), effort, system: tools::AGENT_PERSONA.to_owned(), prompt: String::new(), images: Vec::new(), tools: tools::agent_definitions() }
}

/// Boot the CLI for the next turn in the background (the user is typing).
pub fn prewarm(cx: &mut Context<Workspace>) {
    if running(cx) || !ready(cx) {
        return;
    }
    let c = choice(cx);
    let a = agent(cx);
    let key = c.pref(a.effort);
    if a.warmed.as_ref().is_some_and(|(k, at)| *k == key && at.elapsed() < Duration::from_secs(9 * 60)) {
        return;
    }
    a.warmed = Some((key, Instant::now()));
    let spec = cli_spec(&c, a.effort);
    cx.background_executor().spawn(async move { cli::prewarm(&spec) }).detach();
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

    let mut spec = cli_spec(&c, effort);
    spec.prompt = format!("{project}\n\n---\n\n{}", cli_prompt(&turns));
    spec.images = images;
    let cli = cli::start(spec);
    agent(cx).warmed = None;
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
    agent(cx).turn = Some(Turn { item, model_name: c.name, session: Session::new(&ws.s.project, selected), cli, _poll: poll });
    state::chat_changed(cx);
    cx.notify();
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

/// A tool line the model started writing; its input comes later.
fn start_tool(r: &mut Reply, name: &str) {
    r.state = ReplyState::Streaming;
    close_thinking(r);
    r.steps.push(Step::Tool { name: name.to_owned(), input: Value::Null, state: ToolState::Running });
}

/// The line of a call with its input: the one started for it, else a new one.
fn begin_tool(r: &mut Reply, name: &str, input: &Value) -> usize {
    r.state = ReplyState::Streaming;
    close_thinking(r);
    let started = r.steps.iter().position(|s| matches!(s, Step::Tool { name: n, input: Value::Null, state: ToolState::Running } if n == name));
    match started {
        Some(j) => {
            if let Step::Tool { input: slot, .. } = &mut r.steps[j] {
                *slot = input.clone();
            }
            j
        }
        None => {
            r.steps.push(Step::Tool { name: name.to_owned(), input: input.clone(), state: ToolState::Running });
            r.steps.len() - 1
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Settle the tool line at `step`.
fn settle_tool(item: usize, step: usize, text: &str, ok: bool, cx: &mut Context<Workspace>) {
    with_reply(item, cx, |r| {
        if let Some(Step::Tool { state, .. }) = r.steps.get_mut(step) {
            *state = if ok { ToolState::Done(clip(text, KEEP_RESULT)) } else { ToolState::Failed(clip(text, KEEP_RESULT)) };
        }
    });
}

/// Drain the running turn; `false` once it is over.
fn poll(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    if agent(cx).turn.is_none() {
        return false;
    }
    let (keep, changed) = poll_cli(ws, cx);
    // Only redraw when something arrived: the loaders animate on their own.
    if changed {
        if keep {
            state::follow(cx);
        }
        cx.notify();
    }
    keep
}

/// Events waiting on `rx`, and whether the sender is gone.
fn drain<T>(rx: &crossbeam_channel::Receiver<T>) -> (Vec<T>, bool) {
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(e) => events.push(e),
            Err(crossbeam_channel::TryRecvError::Empty) => return (events, false),
            Err(crossbeam_channel::TryRecvError::Disconnected) => return (events, true),
        }
    }
}

fn poll_cli(ws: &mut Workspace, cx: &mut Context<Workspace>) -> (bool, bool) {
    let Some(Turn { item, cli: t, .. }) = agent(cx).turn.as_ref() else { return (false, false) };
    let item = *item;
    let (events, gone) = drain(&t.events);
    let changed = !events.is_empty() || gone;
    for e in events {
        match e {
            CliEvent::Text(t) => {
                with_reply(item, cx, |r| push_text(r, &t));
            }
            CliEvent::Thinking(t) => {
                with_reply(item, cx, |r| push_thinking(r, &t));
            }
            CliEvent::ToolStart(name) => {
                with_reply(item, cx, |r| start_tool(r, &name));
            }
            CliEvent::Tool { name, input, reply } => {
                let Some(step) = with_reply(item, cx, |r| begin_tool(r, &name, &input)) else {
                    let _ = reply.send(("The chat was cleared.".into(), true));
                    continue;
                };
                if name == tools::ASK_USER {
                    match Question::from_input(&input) {
                        Ok(question) => {
                            agent(cx).pending = Some(Pending { item, step, question, picked: Vec::new(), reply });
                            state::get(cx).update(cx, |a, _| a.scroll.scroll_to_bottom());
                        }
                        Err(e) => {
                            settle_tool(item, step, &e, false, cx);
                            let _ = reply.send((e, true));
                        }
                    }
                    continue;
                }
                let Some(turn) = agent(cx).turn.as_mut() else { return (false, true) };
                let (text, ok) = match tools::execute(&mut turn.session, &name, &input) {
                    Outcome::Ok(t) => (t, true),
                    Outcome::Err(e) => (e, false),
                };
                settle_tool(item, step, &text, ok, cx);
                let _ = reply.send((text, !ok));
            }
            CliEvent::Done(r) => {
                finish(ws, r.map_or_else(ReplyState::Failed, |()| ReplyState::Done), cx);
                return (false, true);
            }
        }
    }
    if gone {
        finish(ws, ReplyState::Failed("The request ended without an answer.".into()), cx);
        return (false, true);
    }
    (true, changed)
}

/// Answer the pending question: the picked options and/or the user's own
/// words (none of either skips it).
pub fn answer(picked: &[usize], own: &str, cx: &mut Context<Workspace>) {
    let Some(p) = agent(cx).pending.take() else { return };
    let text = p.question.answer(picked, own);
    let mut shown: Vec<String> = picked.iter().filter_map(|i| p.question.options.get(*i)).map(|(l, _)| l.clone()).collect();
    if !own.trim().is_empty() {
        shown.push(own.trim().to_owned());
    }
    let shown = if shown.is_empty() { "Skipped".to_owned() } else { shown.join(", ") };
    settle_tool(p.item, p.step, &shown, true, cx);
    let _ = p.reply.send((text, false));
    state::follow(cx);
    cx.notify();
}

/// Pick option `i` of the pending question: answers at once unless several may be picked.
pub fn pick(i: usize, cx: &mut Context<Workspace>) {
    let Some(p) = agent(cx).pending.as_mut() else { return };
    if i >= p.question.options.len() {
        return;
    }
    if !p.question.multi {
        return answer(&[i], "", cx);
    }
    if let Some(at) = p.picked.iter().position(|x| *x == i) {
        p.picked.remove(at);
    } else {
        p.picked.push(i);
    }
    cx.notify();
}

/// Whether a question waits for the user.
pub fn asking(cx: &mut Context<Workspace>) -> bool {
    agent(cx).pending.is_some()
}

/// End the turn: settle the reply and offer what changed.
fn finish(ws: &mut Workspace, end: ReplyState, cx: &mut Context<Workspace>) {
    let Some(turn) = agent(cx).turn.take() else { return };
    // A question still open goes with the turn (its card closes).
    agent(cx).pending = None;
    turn.cli.cancel.cancel();
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
    state::follow(cx);
    state::chat_changed(cx);
    ws.save_chats_quietly(cx);
    // The next message gets a warm process again.
    prewarm(cx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert!(!h[0].1.is_empty() && h[0].0);
        assert!(h[1].1.contains("2 change(s)"));
        let p = cli_prompt(&h);
        assert!(p.starts_with("The conversation so far") && p.ends_with("louder"), "{p}");
        assert_eq!(cli_prompt(&h[..1]), "make a bass");
    }

    #[test]
    fn saved_choices_parse_old_and_new_forms() {
        assert_eq!(parse_pref("claude-code:fable").0.unwrap().name, "Fable 5.2");
        assert_eq!(parse_pref("codex:gpt-6-luna").0.unwrap().provider, Provider::Codex);
        assert_eq!(parse_pref("agent:codex").0.unwrap().provider, Provider::Codex);
        // The API is not offered any more: an old API choice falls back.
        assert!(parse_pref("claude-opus-5-5").0.is_none());
        assert!(parse_pref("api:claude-opus-5-5").0.is_none());
        assert!(parse_pref("").0.is_none());
        let (c, e) = parse_pref("codex:gpt-7@max");
        let c = c.unwrap();
        assert_eq!((c.model.as_str(), e), ("gpt-7", Some(Effort::Max)));
        assert_eq!(c.pref(Effort::Low), "codex:gpt-7@low");
        assert_eq!(parse_pref("claude-code:opus@nonsense").0.unwrap().model, "opus@nonsense");
    }

    #[test]
    fn tool_lines_take_their_input_when_it_arrives() {
        let mut r = Reply { model: "m".into(), vendor: Vendor::Anthropic, steps: vec![], state: ReplyState::Waiting, started: Instant::now(), elapsed: None, changes: 0 };
        push_thinking(&mut r, "hm");
        start_tool(&mut r, "create_track");
        assert!(matches!(r.steps[0], Step::Thinking { done: true, .. }));
        let j = begin_tool(&mut r, "create_track", &json!({ "name": "Bass" }));
        assert_eq!(j, 1, "the started line is reused");
        let k = begin_tool(&mut r, "get_project", &json!({}));
        assert_eq!(k, 2, "a call that was not announced gets its own line");
        assert!(matches!(&r.steps[1], Step::Tool { input, .. } if input["name"] == "Bass"));
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("añb", 2), "a…");
    }
}
