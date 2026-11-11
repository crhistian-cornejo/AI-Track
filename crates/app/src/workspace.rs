//! The root view: owns [`AppState`] and the [`Bridges`], lays out the
//! window, dispatches keyboard actions and runs every drag gesture.
//!
//! Drags are one state machine ([`Drag`]): a mouse-down in any view stores
//! where it started, a window-wide listener (registered only while a drag
//! is active) feeds moves into `drag_move`, and mouse-up seals the undo step.
//! Frames are requested only while the transport plays, meters fall, or a
//! drag or animation is in flight; an idle window draws nothing.

use crate::bridge::{AiEvent, Bridges, JobId, LibraryItem};
use crate::icons::Icon;
use crate::state::*;
use crate::theme::{self, *};
use crate::widgets::{InputEvent, TextInput};
use crate::{actions::*, motion, views};
use aitrack_model::*;
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, Styled, Subscription, Task, Window, anchored,
    canvas, deferred, div, point, prelude::FluentBuilder, px,
};
use gpui::{AppContext as _, StatefulInteractiveElement};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// What a knob edits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KnobTarget {
    Param { device: DeviceId, param: u32, default: f32 },
    Macro { rack: DeviceId, index: usize },
    Pan { track: TrackId },
    Send { track: TrackId, index: usize },
}

#[derive(Clone, Debug)]
pub enum Drag {
    Knob { target: KnobTarget, last_y: f32, value: f32 },
    Fader { track: TrackId, last_y: f32, pos: f32, travel: f32 },
    Tempo { last_y: f32, bpm: f64 },
    BottomResize { start_y: f32, start_h: f32 },
    Notes { clip: ClipId, originals: Vec<Note>, origin: (Tick, i32), resize: bool, applied: (Tick, i32) },
    Velocity { clip: ClipId, originals: Vec<Note>, start_y: f32, lane_h: f32 },
    Marquee { clip: ClipId, from: Point<Pixels>, to: Point<Pixels>, base: BTreeSet<NoteId> },
    Clip { track: TrackId, placement: PlacementId, start: Tick, grab_tick: Tick, applied: (Tick, TrackId) },
    ClipResize { clip: ClipId, length: Tick, grab_tick: Tick },
    AutoPoint { track: TrackId, target: AutomationTarget, original: Vec<AutomationPoint>, index: usize },
    LoopBrace { anchor: Tick },
    Scrub,
}

/// Popovers and context menus.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    Scale,
    Snap,
    PianoTools,
    AddDevice(TrackId),
    TimeSignature,
}

#[derive(Clone, Debug)]
pub struct Menu {
    pub kind: MenuKind,
    pub at: Point<Pixels>,
    pub opened: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Discarded,
}

#[derive(Clone, Debug)]
pub enum ChatEntry {
    User(String),
    Assistant(String),
    Proposal { id: u64, label: String, origin: String, summary: Vec<String>, destructive: bool, status: ProposalStatus },
    Job { id: JobId, label: String, fraction: f32, done: bool },
    Samples { title: String, items: Vec<LibraryItem> },
}

/// Canvas bounds of the dense views, written during prepaint so mouse
/// handlers can convert window positions into ticks and keys.
#[derive(Default)]
pub struct Regions {
    pub arrange: Cell<Bounds<Pixels>>,
    pub ruler: Cell<Bounds<Pixels>>,
    pub piano: Cell<Bounds<Pixels>>,
    pub velocity: Cell<Bounds<Pixels>>,
    pub steps: Cell<Bounds<Pixels>>,
}

pub struct Workspace {
    pub s: AppState,
    pub b: Bridges,
    pub focus: FocusHandle,
    pub drag: Option<Drag>,
    pub menu: Option<Menu>,
    pub bottom_h: f32,
    pub bottom_open: bool,
    pub browser_open: bool,
    pub ai_open: bool,
    pub search: Entity<TextInput>,
    pub chat: Entity<TextInput>,
    pub query: String,
    pub library_folder: String,
    pub expanded: BTreeSet<String>,
    pub chat_log: Vec<ChatEntry>,
    pub regions: Rc<Regions>,
    pub hover_note: Option<NoteId>,
    pub toast: Option<(String, Instant)>,
    synced_revision: Option<u64>,
    meters_live: bool,
    ai_poll: Option<Task<()>>,
    menu_counter: u64,
    pub view_switch: u64,
    pub tab_switch: u64,
    _subs: Vec<Subscription>,
    pub scroll_session: Rc<RefCell<f32>>,
}

impl Workspace {
    pub fn new(project: Project, bridges: Bridges, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(cx, "Search samples, devices…", Some(Icon::Search)));
        let chat = cx.new(|cx| TextInput::new(cx, "Ask the assistant…", Some(Icon::Sparkles)));
        let subs = vec![
            cx.subscribe(&search, |this, _, e: &InputEvent, cx| {
                if let InputEvent::Changed(q) | InputEvent::Submit(q) = e {
                    this.query = q.clone();
                    cx.notify();
                }
            }),
            cx.subscribe(&chat, |this, input, e: &InputEvent, cx| {
                if let InputEvent::Submit(text) = e {
                    if !text.trim().is_empty() {
                        this.send_chat(text.trim().to_owned(), cx);
                        input.update(cx, |i, cx| i.set_text("", cx));
                    }
                }
            }),
        ];
        let mut s = AppState::new(project);
        let mut expanded = BTreeSet::new();
        expanded.insert("categories".to_owned());
        expanded.insert("categories/drums".to_owned());
        let mut this = Self {
            s: AppState::new(Project::new("")),
            b: bridges,
            focus: cx.focus_handle(),
            drag: None,
            menu: None,
            bottom_h: 300.,
            bottom_open: true,
            browser_open: true,
            ai_open: true,
            search,
            chat,
            query: String::new(),
            library_folder: "categories/drums/kicks".into(),
            expanded,
            chat_log: Vec::new(),
            regions: Rc::new(Regions::default()),
            hover_note: None,
            toast: None,
            synced_revision: None,
            meters_live: false,
            ai_poll: None,
            menu_counter: 0,
            view_switch: 0,
            tab_switch: 0,
            _subs: subs,
            scroll_session: Rc::new(RefCell::new(0.)),
        };
        // Seed the assistant with a greeting and one reviewable proposal.
        this.chat_log.push(ChatEntry::Assistant(
            "Hi! I can extract MIDI from audio, split stems and find samples in the project key. Edits I suggest show up here as proposals you accept or discard; each one is a single undo step.".into(),
        ));
        let roots = [33u8, 29, 36, 31];
        let notes: Vec<(u8, Tick, Tick, f32)> =
            roots.iter().enumerate().map(|(i, k)| (*k, i as Tick * BAR, BAR - PPQ / 4, 0.85)).collect();
        let cmds = s.midi_track_commands("Sub Bass", Color::palette(6), &notes);
        if let Some(id) = s.propose("Add a sub-bass layer in A minor", Origin::Agent("Claude Code".into()), cmds) {
            this.chat_log.extend(proposal_entry(&s, id));
        }
        this.s = s;
        this
    }

    // ── Engine sync & frames ──

    fn sync_engine(&mut self) {
        let rev = self.s.revision();
        if self.synced_revision != Some(rev) {
            self.b.engine.update_project(&self.s.project);
            self.synced_revision = Some(rev);
        }
    }

    fn wants_frames(&mut self) -> bool {
        let t = self.b.engine.transport();
        let live = t.playing;
        // One more frame after stopping so meters drop to rest.
        let was = self.meters_live;
        self.meters_live = live;
        live || was || self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_millis(2600))
    }

    pub fn toast(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some((text.into(), Instant::now()));
        let dismiss = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(2700)).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, at)| at.elapsed() >= Duration::from_millis(2600)) {
                    this.toast = None;
                    cx.notify();
                }
            });
        });
        dismiss.detach();
        cx.notify();
    }

    pub fn open_menu(&mut self, kind: MenuKind, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|m| m.kind == kind) {
            self.menu = None;
        } else {
            self.menu_counter += 1;
            self.menu = Some(Menu { kind, at, opened: self.menu_counter });
        }
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    // ── Actions ──

    pub fn play_stop(&mut self, _: &PlayStop, _: &mut Window, cx: &mut Context<Self>) {
        if self.b.engine.transport().playing {
            self.b.engine.stop();
        } else {
            self.b.engine.play();
        }
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.b.engine.stop();
        cx.notify();
    }

    fn toggle_view(&mut self, _: &ToggleView, _: &mut Window, cx: &mut Context<Self>) {
        self.set_view(if self.s.view == MainView::Session { MainView::Arrangement } else { MainView::Session }, cx);
    }

    pub fn set_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        if self.s.view != view {
            self.s.view = view;
            self.view_switch += 1;
            cx.notify();
        }
    }

    pub fn set_tab(&mut self, tab: BottomTab, cx: &mut Context<Self>) {
        if self.s.bottom_tab != tab {
            self.s.bottom_tab = tab;
            self.tab_switch += 1;
        }
        self.bottom_open = true;
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let label = self.s.history.undo_label().map(str::to_owned);
        if self.s.undo() {
            self.toast(format!("Undo {}", label.unwrap_or_default()), cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let label = self.s.history.redo_label().map(str::to_owned);
        if self.s.redo() {
            self.toast(format!("Redo {}", label.unwrap_or_default()), cx);
        }
    }

    fn delete(&mut self, _: &DeleteSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.s.delete_selection() {
            cx.notify();
        }
    }

    fn toggle_draw(&mut self, _: &ToggleDrawTool, _: &mut Window, cx: &mut Context<Self>) {
        self.s.tool = if self.s.tool == Tool::Draw { Tool::Select } else { Tool::Draw };
        self.toast(if self.s.tool == Tool::Draw { "Draw tool" } else { "Select tool" }, cx);
    }

    fn toggle_loop(&mut self, _: &ToggleLoop, _: &mut Window, cx: &mut Context<Self>) {
        let on = !self.b.engine.transport().looping;
        self.b.engine.set_looping(on);
        cx.notify();
    }

    fn toggle_ai(&mut self, _: &ToggleAssistant, _: &mut Window, cx: &mut Context<Self>) {
        self.ai_open = !self.ai_open;
        cx.notify();
    }

    fn toggle_browser(&mut self, _: &ToggleBrowser, _: &mut Window, cx: &mut Context<Self>) {
        self.browser_open = !self.browser_open;
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, n)) = self.s.selected_clip.and_then(|c| self.s.clip_notes(c)) {
            self.s.selected_notes = n.notes.iter().map(|n| n.id).collect();
            cx.notify();
        }
    }

    pub fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        theme::set_dark(!theme::is_dark());
        window.refresh();
        cx.notify();
    }

    pub fn quantize_clip(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.s.selected_clip {
            let n = self.s.quantize(c, 1.);
            self.toast(if n == 0 { "Already on the grid".into() } else { format!("Quantized {n} notes to {}", self.s.snap.label()) }, cx);
        }
    }

    pub fn scale_snap_clip(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.s.selected_clip {
            let scale = self.s.clip_scale(Some(c)).map(|s| s.display()).unwrap_or_else(|| "no scale".into());
            let n = self.s.scale_snap(c);
            self.toast(if n == 0 { format!("All notes already in {scale}") } else { format!("Moved {n} notes into {scale}") }, cx);
        }
    }

    // ── Drags ──

    pub fn begin_drag(&mut self, drag: Drag, cx: &mut Context<Self>) {
        self.s.seal();
        self.drag = Some(drag);
        cx.notify();
    }

    fn drag_move(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(mut drag) = self.drag.take() else { return };
        let y = f32::from(e.position.y);
        let fine = if e.modifiers.shift { 0.12 } else { 1. };
        match &mut drag {
            Drag::Knob { target, last_y, value } => {
                let dy = *last_y - y;
                *last_y = y;
                *value = (*value + dy / 180. * fine).clamp(0., 1.);
                let v = *value;
                match *target {
                    KnobTarget::Param { device, param, .. } => {
                        self.s.set_param(device, param, v);
                    }
                    KnobTarget::Macro { rack, index } => {
                        self.s.set_macro(rack, index, v);
                    }
                    KnobTarget::Pan { track } => {
                        self.s.set_track_coalesced(&format!("pan-{}", track.0), track, TrackProp::Pan(v * 2. - 1.));
                    }
                    KnobTarget::Send { track, index } => {
                        self.s.set_send(track, index, fader_to_db(v).max(-70.));
                    }
                }
            }
            Drag::Fader { track, last_y, pos, travel } => {
                let dy = *last_y - y;
                *last_y = y;
                *pos = (*pos + dy / *travel * fine).clamp(0., 1.);
                let db = fader_to_db(*pos);
                let id = *track;
                self.s.set_track_coalesced(&format!("fader-{}", id.0), id, TrackProp::VolumeDb(db));
            }
            Drag::Tempo { last_y, bpm } => {
                let dy = (*last_y - y) as f64;
                *last_y = y;
                *bpm = (*bpm + dy * if e.modifiers.shift { 0.05 } else { 0.5 }).clamp(20., 999.);
                self.s.set_tempo(*bpm);
            }
            Drag::BottomResize { start_y, start_h } => {
                self.bottom_h = (*start_h + (*start_y - y)).clamp(180., 640.);
            }
            Drag::Notes { clip, originals, origin, resize, applied } => {
                let (tick, key) = self.piano_pos(e.position);
                let raw = tick - origin.0;
                let dt = if self.s.snap == Snap::Off { raw } else { self.s.snap.round(raw) };
                let dk = if *resize { 0 } else { key - origin.1 };
                if (dt, dk) != *applied {
                    *applied = (dt, dk);
                    if *resize {
                        self.s.resize_notes(*clip, originals, dt);
                    } else {
                        self.s.move_notes(*clip, originals, dt, dk);
                    }
                }
            }
            Drag::Velocity { clip, originals, start_y, lane_h } => {
                let dv = (*start_y - y) / lane_h.max(1.);
                self.s.set_velocity(*clip, originals, dv);
            }
            Drag::Marquee { clip, from, to, base } => {
                *to = e.position;
                let (t0, k0) = self.piano_pos(*from);
                let (t1, k1) = self.piano_pos(*to);
                let (ta, tb) = (t0.min(t1), t0.max(t1));
                let (ka, kb) = (k0.min(k1), k0.max(k1));
                let mut sel = base.clone();
                if let Some((_, n)) = self.s.clip_notes(*clip) {
                    for n in &n.notes {
                        if n.end() > ta && n.start < tb && (n.key as i32) >= ka && (n.key as i32) <= kb {
                            sel.insert(n.id);
                        }
                    }
                }
                self.s.selected_notes = sel;
            }
            Drag::Clip { track, placement, start, grab_tick, applied } => {
                let (tick, row) = self.arrange_pos(e.position);
                let snap = if self.s.snap == Snap::Off { Snap::Off } else { Snap::Beat.max_grid(self.s.snap) };
                let new_start = snap.round(*start + tick - *grab_tick).max(0);
                let kind = self.s.project.track(*track).map(|t| t.kind);
                let to = row
                    .and_then(|r| self.s.project.track(r))
                    .filter(|t| Some(t.kind) == kind)
                    .map_or(applied.1, |t| t.id);
                if (new_start, to) != *applied {
                    let from = applied.1;
                    if self.s.move_placement(from, *placement, to, new_start) {
                        *applied = (new_start, to);
                    }
                }
            }
            Drag::ClipResize { clip, length, grab_tick } => {
                let (tick, _) = self.arrange_pos(e.position);
                let snap = if self.s.snap == Snap::Off { Snap::Off } else { Snap::Beat.max_grid(self.s.snap) };
                let new_len = (*length + snap.round(tick - *grab_tick)).max(PPQ);
                self.s.resize_clip(*clip, new_len);
            }
            Drag::AutoPoint { track, target, original, index } => {
                let (tick, _) = self.arrange_pos(e.position);
                let tick = self.s.snap.round(tick);
                if let Some(value) = views::arrangement::automation_value_at(self, *track, e.position) {
                    self.s.move_automation_point(*track, *target, original.clone(), *index, tick, value);
                }
            }
            Drag::LoopBrace { anchor } => {
                let (tick, _) = self.arrange_pos(e.position);
                let t = Snap::Bar.round(tick).max(0);
                let (a, b) = ((*anchor).min(t), (*anchor).max(t));
                if b > a {
                    self.s.set_loop(Some((a, b)));
                }
            }
            Drag::Scrub => {
                let (tick, _) = self.arrange_pos(e.position);
                self.b.engine.locate(Snap::Beat.floor(tick.max(0)));
            }
        }
        self.drag = Some(drag);
        cx.notify();
    }

    fn drag_end(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.s.seal();
            cx.notify();
        }
    }

    /// Window position → (tick, key) in the piano roll.
    pub fn piano_pos(&self, pos: Point<Pixels>) -> (Tick, i32) {
        let b = self.regions.piano.get();
        let x = f32::from(pos.x - b.origin.x) + self.s.piano.scroll_x;
        let y = f32::from(pos.y - b.origin.y) + self.s.piano.scroll_y;
        let tick = (x / self.s.piano.px_per_beat * PPQ as f32) as Tick;
        let key = 127 - (y / self.s.piano.row_h).floor() as i32;
        (tick, key.clamp(0, 127))
    }

    /// Window position → (tick, track row) in the arrangement.
    pub fn arrange_pos(&self, pos: Point<Pixels>) -> (Tick, Option<TrackId>) {
        let b = self.regions.arrange.get();
        let x = f32::from(pos.x - b.origin.x) + self.s.arrange.scroll_x;
        let y = f32::from(pos.y - b.origin.y) + self.s.arrange.scroll_y;
        let tick = (x / self.s.arrange.px_per_beat * PPQ as f32) as Tick;
        let row = views::arrangement::rows(&self.s).into_iter().find(|r| y >= r.top && y < r.top + r.total()).map(|r| r.track);
        (tick, row)
    }

    // ── AI ──

    pub fn resolve_proposal(&mut self, id: u64, accept: bool, cx: &mut Context<Self>) {
        let ok = if accept { self.s.accept(id) } else { self.s.discard(id) };
        if ok {
            for e in &mut self.chat_log {
                if let ChatEntry::Proposal { id: pid, status, .. } = e {
                    if *pid == id {
                        *status = if accept { ProposalStatus::Accepted } else { ProposalStatus::Discarded };
                    }
                }
            }
            if accept {
                self.toast("Proposal applied — one undo step", cx);
            }
        }
        cx.notify();
    }

    fn send_chat(&mut self, text: String, cx: &mut Context<Self>) {
        self.chat_log.push(ChatEntry::User(text.clone()));
        self.b.ai.chat(&text, &self.s.project);
        self.ensure_ai_poll(cx);
        cx.notify();
    }

    /// The audio the AI actions work on: the selected audio clip, else the first one.
    fn audio_target(&self) -> Option<(String, std::path::PathBuf)> {
        let audio_of = |clip: &ClipId| {
            let c = self.s.project.clips.get(clip)?;
            match self.s.project.sources.get(&c.source)? {
                Source::Audio(a) => Some((c.name.clone(), a.path.clone())),
                _ => None,
            }
        };
        self.s.selected_clip.as_ref().and_then(audio_of).or_else(|| self.s.project.clips.keys().find_map(audio_of))
    }

    pub fn extract_midi(&mut self, cx: &mut Context<Self>) {
        let Some((name, path)) = self.audio_target() else {
            self.toast("No audio clip to analyse", cx);
            return;
        };
        let id = self.b.ai.extract_midi(&path);
        self.chat_log.push(ChatEntry::Job { id, label: format!("Extracting MIDI from {name}"), fraction: 0., done: false });
        self.ensure_ai_poll(cx);
        cx.notify();
    }

    pub fn split_stems(&mut self, cx: &mut Context<Self>) {
        let Some((name, path)) = self.audio_target() else {
            self.toast("No audio clip to split", cx);
            return;
        };
        let id = self.b.ai.split_stems(&path);
        self.chat_log.push(ChatEntry::Job { id, label: format!("Splitting {name} into stems"), fraction: 0., done: false });
        self.ensure_ai_poll(cx);
        cx.notify();
    }

    pub fn find_samples_in_key(&mut self, cx: &mut Context<Self>) {
        let scale = self.s.project.scale;
        let mut items = self.b.library.search("", scale);
        items.retain(|i| i.key.is_some());
        items.truncate(6);
        let title = match scale {
            Some(s) => format!("Samples in {} ({})", s.display(), s.camelot().unwrap_or_default()),
            None => "Samples (project has no key)".into(),
        };
        self.chat_log.push(ChatEntry::Samples { title, items });
        cx.notify();
    }

    fn ensure_ai_poll(&mut self, cx: &mut Context<Self>) {
        if self.ai_poll.is_some() {
            return;
        }
        // A short timer only while jobs run, not a frame tick.
        self.ai_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(80)).await;
                let keep = this.update(cx, |w, cx| w.poll_ai(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn poll_ai(&mut self, cx: &mut Context<Self>) -> bool {
        for ev in self.b.ai.poll() {
            match ev {
                AiEvent::Progress { job, fraction, .. } => self.set_job(job, fraction, false),
                AiEvent::MidiReady { job, source_name, notes } => {
                    self.set_job(job, 1., true);
                    let notes: Vec<(u8, Tick, Tick, f32)> = notes.iter().map(|n| (n.key, n.start, n.length, n.velocity)).collect();
                    let cmds = self.s.midi_track_commands(&source_name, Color::palette(11), &notes);
                    if let Some(id) = self.s.propose(&format!("Add \"{source_name}\" as a MIDI track"), Origin::Feature("Audio to MIDI".into()), cmds) {
                        self.chat_log.extend(proposal_entry(&self.s, id));
                    }
                }
                AiEvent::StemsReady { job, stems } => {
                    self.set_job(job, 1., true);
                    let cmds = stem_commands(&mut self.s, &stems);
                    if let Some(id) = self.s.propose("Add 4 stems as audio tracks", Origin::Feature("Stem split".into()), cmds) {
                        self.chat_log.extend(proposal_entry(&self.s, id));
                    }
                }
                AiEvent::Reply { text } => self.chat_log.push(ChatEntry::Assistant(text)),
                AiEvent::Proposal { label, agent, commands } => {
                    if let Some(id) = self.s.propose(&label, Origin::Agent(agent), commands) {
                        self.chat_log.extend(proposal_entry(&self.s, id));
                    }
                }
                AiEvent::Failed { job, message } => {
                    self.set_job(job, 1., true);
                    self.chat_log.push(ChatEntry::Assistant(format!("That failed: {message}")));
                }
            }
        }
        cx.notify();
        let busy = self.b.ai.busy();
        if !busy {
            self.ai_poll = None;
        }
        busy
    }

    fn set_job(&mut self, job: JobId, f: f32, finished: bool) {
        for e in &mut self.chat_log {
            if let ChatEntry::Job { id, fraction, done, .. } = e {
                if *id == job {
                    *fraction = f;
                    *done = finished;
                }
            }
        }
    }

    // ── Layout ──

    fn drag_capture(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.drag.as_ref()?;
        let weak = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let w = weak.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            let _ = w.update(cx, |this, cx| this.drag_move(e, cx));
                        }
                    });
                    let w = weak.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            let _ = w.update(cx, |this, cx| this.drag_end(cx));
                        }
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    fn render_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.clone()?;
        let body = views::menus::render(self, &menu, window, cx)?;
        let id = gpui::ElementId::Name(format!("menu-{}", menu.opened).into());
        Some(
            deferred(
                anchored().position(menu.at).snap_to_window_with_margin(px(SP2)).child(
                    div()
                        .id("menu-layer")
                        .occlude()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                        .child(motion::rise_in(id, div().child(body), 6.)),
                ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn render_status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pal = p();
        let undo = self.s.history.undo_label().map(|l| format!("Undo: {l}")).unwrap_or_else(|| "Nothing to undo".into());
        let err = self.s.last_error.clone();
        let tempo = self.s.project.tempo.initial_bpm();
        let scale = self.s.project.scale.map_or("No key".into(), |s| s.display());
        let pending = self.s.history.proposals().len();
        div()
            .h(px(STATUS_BAR_H))
            .flex_none()
            .px(px(SP3))
            .gap(px(SP4))
            .flex()
            .items_center()
            .bg(pal.panel)
            .border_t_1()
            .border_color(border())
            .text_size(px(TEXT_SM))
            .text_color(pal.muted)
            .child(crate::icons::icon(Icon::Undo, 12., pal.faint))
            .child(crate::widgets::label(undo, TEXT_SM, pal.muted))
            .when_some(err, |d, e| {
                d.child(
                    div()
                        .id("error")
                        .flex()
                        .gap(px(SP1))
                        .items_center()
                        .cursor_pointer()
                        .child(crate::widgets::label(e, TEXT_SM, pal.record))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.s.last_error = None;
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .when(pending > 0, |d| d.child(crate::widgets::badge(format!("{pending} proposal{}", if pending == 1 { "" } else { "s" }), pal.accent)))
            .child(crate::widgets::label(format!("{} · {tempo:.2} BPM · {scale} · 48 kHz", self.s.project.name), TEXT_SM, pal.muted))
            .child(crate::widgets::label("Engine: stub", TEXT_SM, pal.faint))
            .into_any_element()
    }

    fn render_toast(&self) -> Option<AnyElement> {
        let (text, at) = self.toast.clone()?;
        if at.elapsed() > Duration::from_millis(2600) {
            return None;
        }
        let pal = p();
        let id = gpui::ElementId::Name(format!("toast-{}", at.elapsed().as_nanos() / 1_000_000_000_000).into());
        let _ = id;
        Some(
            div()
                .absolute()
                .bottom(px(STATUS_BAR_H + SP4))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(motion::rise_in(
                    gpui::ElementId::Name(format!("toast-{text}").into()),
                    div()
                        .px(px(SP3))
                        .h(px(CONTROL_H + 4.))
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .rounded(px(R_LG))
                        .bg(pal.raised)
                        .border_1()
                        .border_color(border_strong())
                        .shadow_lg()
                        .child(crate::icons::icon(Icon::Check, 14., pal.accent))
                        .child(crate::widgets::label(text, TEXT_MD, pal.text)),
                    8.,
                ))
                .into_any_element(),
        )
    }
}

impl Snap {
    /// The coarser of two grids (clips move on at least a beat).
    fn max_grid(self, other: Snap) -> Snap {
        if other.ticks() > self.ticks() { other } else { self }
    }
}

/// The chat card for a pending proposal.
pub fn proposal_entry(s: &AppState, id: u64) -> Option<ChatEntry> {
    let p = s.history.proposals().iter().find(|p| p.id == id)?;
    let origin = match &p.origin {
        Origin::Agent(a) | Origin::Feature(a) | Origin::Script(a) => a.clone(),
        Origin::User => "You".into(),
    };
    let mut summary: Vec<String> = Vec::new();
    for c in &p.commands {
        let l = c.label().to_owned();
        if !summary.contains(&l) {
            summary.push(l);
        }
    }
    Some(ChatEntry::Proposal { id, label: p.label.clone(), origin, summary, destructive: p.is_destructive(), status: ProposalStatus::Pending })
}

/// Commands adding one audio track per stem.
fn stem_commands(s: &mut AppState, stems: &[(String, std::path::PathBuf)]) -> Vec<Command> {
    let mut cmds = Vec::new();
    let index = s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
    for (i, (name, path)) in stems.iter().enumerate() {
        let track = s.project.alloc_track_id();
        let src = s.project.alloc_source_id();
        let clip = s.project.alloc_clip_id();
        let color = Color::palette(2 + i * 3);
        cmds.push(Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, format!("{name} Stem"), color)), index: index + i });
        cmds.push(Command::AddSource {
            id: src,
            source: Box::new(Source::Audio(AudioSource {
                path: path.clone(),
                hash: None,
                sample_rate: 48_000,
                channels: 2,
                frames: 48_000 * 15,
                warp: Warp::default(),
                key: s.project.scale,
            })),
        });
        cmds.push(Command::AddClip {
            id: clip,
            clip: Box::new(Clip {
                name: format!("{name} Stem"),
                color,
                source: src,
                offset: 0,
                length: 8 * BAR,
                looping: None,
                gain_db: 0.,
                envelopes: Vec::new(),
                launch: LaunchSettings::default(),
                muted: false,
                scale: None,
            }),
        });
        let pl = s.project.alloc_placement_id();
        cmds.push(Command::PlaceClip { track, placement: Placement { id: pl, clip, start: 4 * BAR, lane: 0 } });
    }
    cmds
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_engine();
        if self.wants_frames() || self.drag.is_some() {
            window.request_animation_frame();
        }
        let pal = p();
        let center = match self.s.view {
            MainView::Session => views::session::render(self, window, cx),
            MainView::Arrangement => views::arrangement::render(self, window, cx),
        };
        let switch_id = gpui::ElementId::Name(format!("view-{}", self.view_switch).into());
        let dx = if self.s.view == MainView::Session { -24. } else { 24. };
        let center = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_hidden()
            .bg(pal.panel)
            .rounded(px(R_LG))
            .border_1()
            .border_color(border())
            .child(motion::slide_in(switch_id, div().size_full().child(center), if self.view_switch == 0 { 0. } else { dx }));

        let middle = div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap(px(SP1))
            .when(self.browser_open, |d| d.child(views::browser::render(self, window, cx)))
            .child(center)
            .child(views::ai_panel::render(self, window, cx));

        let bottom = views::bottom::render(self, window, cx);
        let menu = self.render_menu(window, cx);
        let capture = self.drag_capture(cx);
        let toast = self.render_toast();
        let status = self.render_status(cx);

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::play_stop))
            .on_action(cx.listener(Self::toggle_view))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::toggle_draw))
            .on_action(cx.listener(Self::toggle_loop))
            .on_action(cx.listener(Self::toggle_ai))
            .on_action(cx.listener(Self::toggle_browser))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|this, _: &TabDevices, _, cx| this.set_tab(BottomTab::Devices, cx)))
            .on_action(cx.listener(|this, _: &TabPianoRoll, _, cx| this.set_tab(BottomTab::PianoRoll, cx)))
            .on_action(cx.listener(|this, _: &TabSteps, _, cx| this.set_tab(BottomTab::Steps, cx)))
            .on_action(cx.listener(|this, _: &TabMixer, _, cx| this.set_tab(BottomTab::Mixer, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.menu = None;
                this.s.selected_notes.clear();
                this.focus.focus(window, cx);
                cx.notify();
            }))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| {
                // Clicking the workspace (outside a text field) takes the keys back.
                if !this.focus.is_focused(window) && !this.search.read(cx).focus_handle(cx).is_focused(window) && !this.chat.read(cx).focus_handle(cx).is_focused(window) {
                    this.focus.focus(window, cx);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(pal.base)
            .font_family(FONT_UI)
            .text_size(px(TEXT_MD))
            .text_color(pal.text)
            .child(views::topbar::render(self, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(SP1))
                    .p(px(SP1))
                    .child(middle)
                    .child(bottom),
            )
            .child(status)
            .children(toast)
            .children(capture)
            .children(menu)
    }
}

/// A point helper for menus opened from a click.
pub fn below(bounds_bottom_left: Point<Pixels>) -> Point<Pixels> {
    point(bounds_bottom_left.x, bounds_bottom_left.y + px(SP1))
}
