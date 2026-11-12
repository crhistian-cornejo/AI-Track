//! The root view: owns [`AppState`] and the [`Bridges`], lays out the
//! window, dispatches keyboard actions and runs every drag gesture.
//!
//! Drags are one state machine ([`Drag`]): a mouse-down in any view stores
//! where it started, a window-wide listener (registered only while a drag
//! is active) feeds moves into `drag_move`, and mouse-up seals the undo step.
//! Frames are requested only while the transport plays, meters fall, or a
//! drag or animation is in flight; an idle window draws nothing.

use crate::bridge::{AiEvent, AudioAnalysis, Bridges, JobId, LibraryItem, MidiMode, ModelNotice};
use crate::icons::Icon;
use crate::state::*;
use crate::theme::{self, *};
use crate::widgets::{InputEvent, TextInput};
use crate::{actions::*, motion, views};
use aitrack_model::*;
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, KeyUpEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, Styled, Subscription, Task, Window, anchored,
    canvas, deferred, div, prelude::FluentBuilder, px,
};
use gpui::{AppContext as _, StatefulInteractiveElement};
use std::cell::Cell;
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
    BrowserResize { start_x: f32, start_w: f32 },
    MixerResize { start_y: f32, start_h: f32 },
    Notes { clip: ClipId, originals: Vec<Note>, origin: (Tick, i32), resize: bool, applied: (Tick, i32), preview: Option<(TrackId, u8)> },
    Velocity { clip: ClipId, originals: Vec<Note>, last_y: f32, lane_h: f32, delta: f32 },
    Audition { track: TrackId, key: Option<u8> },
    Marquee { clip: ClipId, from: Point<Pixels>, to: Point<Pixels>, base: BTreeSet<NoteId> },
    Clip { track: TrackId, placement: PlacementId, start: Tick, grab_tick: Tick, applied: (Tick, TrackId) },
    ClipResize { clip: ClipId, length: Tick, grab_tick: Tick },
    AutoPoint { track: TrackId, target: AutomationTarget, original: Vec<AutomationPoint>, index: usize },
    /// Alt + drag on an automation segment bends it.
    AutoCurve { track: TrackId, target: AutomationTarget, index: usize, start_y: f32, start_curve: f32, rising: bool },
    LoopBrace { anchor: Tick },
    Scrub,
    /// Colour picker: the hue strip or the saturation/value square.
    Pick { hue: bool },
}

/// Popovers and context menus.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    Scale,
    Snap,
    PianoTools,
    AddDevice(TrackId),
    /// Factory presets of a stock device.
    Presets(DeviceId),
    /// Library actions (right-click in the browser).
    LibraryContext,
    TimeSignature,
    /// File: new from a template, open, save.
    File,
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
    /// A model licence the user must accept before it is downloaded.
    License { notice: ModelNotice, audio: std::path::PathBuf, clip_name: String, status: ProposalStatus },
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
    pub audition: crate::audition::Audition,
    pub piano_keyboard: bool,
    pub piano_preview: bool,
    pub piano_octave: u8,
    pub drag: Option<Drag>,
    pub menu: Option<Menu>,
    /// View state of the visual device editors.
    pub fx: views::device_ui::UiState,
    /// View state of the sample editor (the Clip tab for an audio clip).
    #[allow(dead_code)] // Read once the sample editor is filled in.
    pub sample: views::sample_editor::UiState,
    /// Waveform peaks for the audio clips on screen.
    pub waves: crate::waveform::WaveformCache,
    /// The meter strip under the top bar.
    pub meters: views::meters::MeterStrip,
    /// CPU and memory of this process, for the status bar.
    pub usage: crate::usage::Usage,
    /// The `.aitrack` file the project was opened from or saved to.
    pub project_path: Option<std::path::PathBuf>,
    /// Which matches "Matches project" lists.
    pub match_kind: MatchKind,
    pub bottom_h: f32,
    pub bottom_open: bool,
    /// The mixer section between the arrangement and the detail panel.
    pub mixer_open: bool,
    pub mixer_h: f32,
    /// Width of the browser sidebar (drag its right edge).
    pub browser_w: f32,
    pub browser_open: bool,
    pub ai_open: bool,
    pub search: Entity<TextInput>,
    pub chat: Entity<TextInput>,
    pub query: String,
    pub library_folder: String,
    pub expanded: BTreeSet<String>,
    pub chat_log: Vec<ChatEntry>,
    pub regions: Rc<Regions>,
    pub toast: Option<(String, Instant)>,
    synced_revision: Option<u64>,
    meters_live: bool,
    ai_poll: Option<Task<()>>,
    menu_counter: u64,
    pub view_switch: u64,
    pub tab_switch: u64,
    _subs: Vec<Subscription>,
    pub look: views::prefs::Look,
    pub modal: Option<views::prefs::Modal>,
    pub export: views::export::ExportDialog,
    pub export_name: Entity<TextInput>,
    /// Audio-to-MIDI engine picked in the assistant panel.
    pub midi_mode: MidiMode,
    /// Analysis jobs → the audio source they fill in.
    analysis_jobs: std::collections::HashMap<JobId, SourceId>,
    /// Extract-MIDI and stem jobs → where their audio clip sits in the arrangement.
    midi_jobs: std::collections::HashMap<JobId, Option<Tick>>,
    /// Browser filter: samples ranked for the project key and tempo.
    pub library_matches: bool,
    pub organize: OrganizeUi,
    _housekeeping: Task<()>,
}

/// "Organize on disk": options → preview → run → undo.
#[derive(Clone, Debug, Default)]
pub struct OrganizeUi {
    pub items: Vec<u64>,
    pub dest: Option<std::path::PathBuf>,
    pub by_key: bool,
    pub move_files: bool,
    pub preview: Option<crate::bridge::OrganizePreview>,
    pub running: bool,
    pub result: Option<Result<String, String>>,
}

/// The kinds of sample "Matches project" can be narrowed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    All,
    Loops,
    OneShots,
}

/// Whether a sample in `key` sits in a song in `song`: the same key or its
/// relative major/minor (the same number on the Camelot wheel).
pub fn keys_match(key: Scale, song: Scale) -> bool {
    let number = |s: Scale| s.camelot().map(|c| c.trim_end_matches(['A', 'B']).to_owned());
    match (number(key), number(song)) {
        (Some(a), Some(b)) => a == b,
        _ => key.mask() == song.mask(),
    }
}

/// A sample dragged out of the browser.
#[derive(Clone, Debug)]
pub struct DraggedSample(pub LibraryItem);

impl Render for DraggedSample {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        div()
            .px(px(SP2))
            .h(px(ROW_H))
            .flex()
            .items_center()
            .gap(px(SP2))
            .rounded(px(r_md()))
            .bg(pal.raised)
            .border_1()
            .border_color(border_strong())
            .shadow_lg()
            .child(crate::icons::icon(Icon::Waveform, 12., pal.accent))
            .child(crate::widgets::label(self.0.name.clone(), TEXT_SM, pal.text))
    }
}

impl Workspace {
    pub fn new(project: Project, bridges: Bridges, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(cx, "Search samples, devices…", Some(Icon::Search)));
        let chat = cx.new(|cx| TextInput::new(cx, "Ask the assistant…", Some(Icon::Sparkles)));
        let export_name = cx.new(|cx| TextInput::new(cx, "File name", Some(Icon::File)));
        let look = views::prefs::Look::load(cx);
        let subs = vec![
            cx.subscribe(&search, |this, _, e: &InputEvent, cx| {
                let (InputEvent::Changed(q) | InputEvent::Submit(q)) = e;
                this.query = q.clone();
                cx.notify();
            }),
            cx.subscribe(&chat, |this, input, e: &InputEvent, cx| {
                if let InputEvent::Submit(text) = e
                    && !text.trim().is_empty() {
                        this.send_chat(text.trim().to_owned(), cx);
                        input.update(cx, |i, cx| i.set_text("", cx));
                    }
            }),
        ];
        // Main-thread plugin work, background scan results and engine
        // messages, on a short timer that runs even while nothing animates.
        let housekeeping = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                if this.update(cx, |w, cx| w.housekeeping(cx)).is_err() {
                    break;
                }
            }
        });
        let mut s = AppState::new(project);
        let mut expanded = BTreeSet::new();
        expanded.insert("categories".to_owned());
        expanded.insert("categories/drums".to_owned());
        let mut this = Self {
            s: AppState::new(Project::new("")),
            b: bridges,
            focus: cx.focus_handle(),
            audition: Default::default(),
            piano_keyboard: false,
            piano_preview: true,
            piano_octave: 4,
            drag: None,
            menu: None,
            fx: views::device_ui::UiState::default(),
            sample: Default::default(),
            waves: Default::default(),
            meters: views::meters::MeterStrip::default(),
            usage: crate::usage::Usage::default(),
            project_path: None,
            match_kind: MatchKind::All,
            bottom_h: 300.,
            bottom_open: true,
            mixer_open: false,
            mixer_h: 264.,
            browser_w: BROWSER_W,
            browser_open: true,
            // Closed at start: the arrangement gets the room; the Assistant button opens it.
            ai_open: false,
            search,
            chat,
            query: String::new(),
            library_folder: "categories/drums/kicks".into(),
            expanded,
            chat_log: Vec::new(),
            regions: Rc::new(Regions::default()),
            toast: None,
            synced_revision: None,
            meters_live: false,
            ai_poll: None,
            menu_counter: 0,
            view_switch: 0,
            tab_switch: 0,
            _subs: subs,
            look,
            modal: None,
            export: views::export::ExportDialog::default(),
            export_name,
            midi_mode: MidiMode::default(),
            analysis_jobs: Default::default(),
            midi_jobs: Default::default(),
            library_matches: false,
            organize: OrganizeUi::default(),
            _housekeeping: housekeeping,
        };
        // Open on a folder that exists in this library.
        if !folder_exists(this.b.library.folders(), &this.library_folder)
            && let Some(f) = this.b.library.folders().iter().find(|f| f.count > 0)
        {
            let first = f.children.iter().find(|c| c.count > 0).unwrap_or(f);
            this.expanded.insert(f.id.clone());
            this.library_folder = first.id.clone();
        }
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
        this.apply_dev_view();
        // The agent CLIs are looked up in the background; collect the result.
        this.ensure_ai_poll(cx);
        this
    }

    /// Development aid for visual checks without touching the keyboard:
    /// `AITRACK_DEV_VIEW=devices,meters,play,big,track=Bass` opens the app in
    /// that state. Unknown words are ignored; unset, nothing happens.
    fn apply_dev_view(&mut self) {
        let Ok(spec) = std::env::var("AITRACK_DEV_VIEW") else { return };
        for word in spec.split(',').map(str::trim) {
            match word {
                "devices" => self.s.bottom_tab = BottomTab::Devices,
                "piano" => self.s.bottom_tab = BottomTab::PianoRoll,
                "steps" => self.s.bottom_tab = BottomTab::Steps,
                "mixer" => self.mixer_open = true,
                "meters" => self.meters.open = true,
                "tall" => self.bottom_h = 460.,
                "no-ai" => self.ai_open = false,
                "ai" => self.ai_open = true,
                "no-browser" => self.browser_open = false,
                "play" => self.b.engine.play(),
                "big" => {
                    let ids: Vec<_> = self.s.project.tracks.iter().flat_map(|t| t.devices.iter().map(|d| d.id)).collect();
                    self.fx.big.extend(ids);
                }
                _ => {
                    // `device=fx.shaper` appends a stock device to the selected track.
                    if let Some(plugin) = word.strip_prefix("device=")
                        && let Some(track) = self.s.selected_track
                    {
                        let id = self.s.project.alloc_device_id();
                        let index = self.s.project.track(track).map_or(0, |t| t.devices.len());
                        let name = self.b.plugins.device(plugin).map_or(plugin.to_owned(), |d| d.name.to_owned());
                        let device = Device::stock(id, plugin, name);
                        self.s.edit("Add device", vec![Command::AddDevice { slot: aitrack_model::command::DeviceSlot::Track { track, index }, device: Box::new(device) }]);
                        self.fx.big.insert(id);
                    }
                    if let Some(name) = word.strip_prefix("track=")
                        && let Some(t) = self.s.project.tracks.iter().find(|t| t.name.eq_ignore_ascii_case(name))
                    {
                        self.s.selected_track = Some(t.id);
                    }
                }
            }
        }
    }

    // ── Engine sync & frames ──

    fn sync_engine(&mut self) {
        let rev = self.s.revision();
        if self.synced_revision != Some(rev) {
            self.b.engine.update_project(&self.s.project);
            self.synced_revision = Some(rev);
        }
    }

    /// Timer tick: plugin main-thread work, scan progress, engine messages.
    fn housekeeping(&mut self, cx: &mut Context<Self>) {
        self.audition.retry(|t, k, v| self.b.engine.audition_note(t, k, v));
        let mut changed = self.b.engine.idle();
        // The usage read-out moves about once a second even when nothing else does.
        let (cpu, mem) = (self.usage.cpu, self.usage.memory);
        self.usage.tick();
        changed |= (self.usage.cpu - cpu).abs() >= 0.005 || self.usage.memory.abs_diff(mem) >= 4 * 1024 * 1024;
        changed |= self.b.plugins.poll();
        changed |= self.b.library.poll();
        changed |= self.waves.poll();
        if let Some(r) = self.b.library.organize_result() {
            self.organize.running = false;
            self.organize.preview = None;
            match &r {
                Ok(m) | Err(m) => self.toast(m.clone(), cx),
            }
            self.organize.result = Some(r);
            changed = true;
        }
        for m in self.b.engine.take_messages() {
            self.toast(m, cx);
            changed = true;
        }
        // Keep the plugin scan progress moving while Preferences is open.
        if self.b.plugins.scan_status().running && self.modal.is_some() {
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn wants_frames(&mut self) -> bool {
        let t = self.b.engine.transport();
        let live = t.playing || self.audition.is_active();
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
        if tab != BottomTab::PianoRoll {
            self.release_audition(cx);
        }
        if self.s.bottom_tab != tab {
            self.s.bottom_tab = tab;
            self.tab_switch += 1;
        }
        // The mixer needs room for its faders.
        if tab == BottomTab::Mixer && self.bottom_h < 460. {
            self.bottom_h = 460.;
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

    pub fn audition_track(&self) -> Option<TrackId> {
        let clip = self.s.selected_clip.filter(|c| self.s.clip_notes(*c).is_some())?;
        self.s.selected_track.filter(|id| {
            self.s.project.track(*id).is_some_and(|t| {
                t.kind == TrackKind::Instrument
                    && (t.session_slots.values().any(|c| *c == clip) || t.arrangement.iter().any(|p| p.clip == clip))
            })
        }).or_else(|| self.s.clip_track(clip).filter(|id| self.s.project.track(*id).is_some_and(|t| t.kind == TrackKind::Instrument)))
    }

    pub fn press_audition(&mut self, source: &str, track: TrackId, key: u8, cx: &mut Context<Self>) -> bool {
        let ok = self.audition.press(source, track, key, |t, k, v| self.b.engine.audition_note(t, k, v));
        if !ok {
            self.toast("Cannot audition: check the audio device and instrument", cx);
        }
        cx.notify();
        ok
    }

    pub fn preview_note(&mut self, key: u8, cx: &mut Context<Self>) -> Option<(TrackId, u8)> {
        let track = self.piano_preview.then(|| self.audition_track()).flatten()?;
        self.press_audition(crate::audition::NOTE, track, key, cx).then_some((track, key))
    }

    pub fn release_audition(&mut self, cx: &mut Context<Self>) {
        self.audition.release_all(|t, k, v| self.b.engine.audition_note(t, k, v));
        if matches!(self.drag, Some(Drag::Audition { .. })) {
            self.drag = None;
        }
        cx.notify();
    }

    fn piano_key_down(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.piano_keyboard || !self.bottom_open || self.s.bottom_tab != BottomTab::PianoRoll
            || self.modal.is_some() || !self.focus.is_focused(window) || e.keystroke.modifiers.modified()
        {
            return;
        }
        let Some(key) = crate::audition::keyboard_note(&e.keystroke.key, self.piano_octave) else { return };
        let Some(track) = self.audition_track() else { return };
        if !e.is_held {
            self.press_audition(&e.keystroke.key, track, key, cx);
        }
        cx.stop_propagation();
    }

    fn piano_key_up(&mut self, e: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.audition.release(&e.keystroke.key, |t, k, v| self.b.engine.audition_note(t, k, v)) {
            cx.stop_propagation();
            cx.notify();
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
            Drag::MixerResize { start_y, start_h } => {
                self.mixer_h = (*start_h + (*start_y - f32::from(e.position.y))).clamp(160., 520.);
            }
            Drag::BrowserResize { start_x, start_w } => {
                self.browser_w = (*start_w + f32::from(e.position.x) - *start_x).clamp(200., 520.);
            }
            Drag::BottomResize { start_y, start_h } => {
                self.bottom_h = (*start_h + (*start_y - y)).clamp(180., 640.);
            }
            Drag::Notes { clip, originals, origin, resize, applied, preview } => {
                let (tick, key) = self.piano_pos(e.position);
                let raw = tick - origin.0;
                let dt = if self.s.snap == Snap::Off { raw } else { self.s.snap.round(raw) };
                let dk = if *resize { 0 } else { key - origin.1 };
                if let Some((track, previous)) = *preview {
                    let next = (origin.1 + dk).clamp(0, 127) as u8;
                    if next != previous {
                        self.audition.release(crate::audition::NOTE, |t, k, v| self.b.engine.audition_note(t, k, v));
                        *preview = self.press_audition(crate::audition::NOTE, track, next, cx).then_some((track, next));
                    }
                }
                if (dt, dk) != *applied {
                    *applied = (dt, dk);
                    if *resize {
                        self.s.resize_notes(*clip, originals, dt);
                    } else {
                        self.s.move_notes(*clip, originals, dt, dk);
                    }
                }
            }
            Drag::Velocity { clip, originals, last_y, lane_h, delta } => {
                let dy = *last_y - y;
                *last_y = y;
                if dy != 0. {
                    let next = velocity_delta(originals, *delta + dy / lane_h.max(1.) * fine);
                    if next != *delta {
                        *delta = next;
                        self.s.set_velocity(*clip, originals, *delta);
                    }
                }
            }
            Drag::Audition { track, key } => {
                let b = self.regions.piano.get();
                let in_bounds = e.position.y >= b.top() && e.position.y < b.bottom();
                let next = in_bounds.then(|| self.piano_pos(e.position).1 as u8);
                if *key != next {
                    self.audition.release(crate::audition::MOUSE, |t, k, v| self.b.engine.audition_note(t, k, v));
                    if let Some(note) = next {
                        self.press_audition(crate::audition::MOUSE, *track, note, cx);
                    }
                    *key = next;
                }
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
            Drag::AutoCurve { track, target, index, start_y, start_curve, rising } => {
                // Dragging up always lifts the middle of the segment, whichever way it runs.
                let dy = (*start_y - f32::from(e.position.y)) / 60.;
                let curve = *start_curve + if *rising { -dy } else { dy };
                self.s.set_automation_curve(*track, *target, *index, curve);
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
            Drag::Pick { hue } => {
                let hue = *hue;
                self.drag = Some(drag);
                self.pick_at(e.position, hue, cx);
                return;
            }
        }
        self.drag = Some(drag);
        cx.notify();
    }

    fn drag_end(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.drag.take() {
            match drag {
                Drag::Audition { .. } => {
                    self.audition.release(crate::audition::MOUSE, |t, k, v| self.b.engine.audition_note(t, k, v));
                }
                Drag::Notes { .. } => {
                    self.audition.release(crate::audition::NOTE, |t, k, v| self.b.engine.audition_note(t, k, v));
                }
                _ => {}
            }
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
                if let ChatEntry::Proposal { id: pid, status, .. } = e
                    && *pid == id {
                        *status = if accept { ProposalStatus::Accepted } else { ProposalStatus::Discarded };
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

    /// Arrangement start of the first placement of the clip playing `path`.
    fn audio_start(&self, path: &std::path::Path) -> Option<Tick> {
        let p = &self.s.project;
        p.tracks.iter().flat_map(|t| &t.arrangement).find_map(|pl| {
            let c = p.clips.get(&pl.clip)?;
            matches!(p.sources.get(&c.source), Some(Source::Audio(a)) if a.path == path).then_some(pl.start)
        })
    }

    pub fn extract_midi(&mut self, cx: &mut Context<Self>) {
        let Some((name, path)) = self.audio_target() else {
            self.toast("No audio clip to analyse", cx);
            return;
        };
        let bpm = self.s.project.tempo.initial_bpm();
        let id = self.b.ai.extract_midi(&path, self.midi_mode, bpm);
        self.midi_jobs.insert(id, self.audio_start(&path));
        let how = match self.midi_mode {
            MidiMode::Polyphonic => "basic-pitch",
            MidiMode::Monophonic => "monophonic",
            MidiMode::Drums => "drums",
        };
        self.chat_log.push(ChatEntry::Job { id, label: format!("Extracting MIDI from {name} ({how})"), fraction: 0., done: false });
        self.ensure_ai_poll(cx);
        cx.notify();
    }

    pub fn split_stems(&mut self, cx: &mut Context<Self>) {
        let Some((name, path)) = self.audio_target() else {
            self.toast("No audio clip to split", cx);
            return;
        };
        // The model licence comes first; nothing downloads until it is accepted.
        if let Some(notice) = self.b.ai.stem_license() {
            let already = self.chat_log.iter().any(|e| matches!(e, ChatEntry::License { status: ProposalStatus::Pending, .. }));
            if !already {
                self.chat_log.push(ChatEntry::License { notice, audio: path, clip_name: name, status: ProposalStatus::Pending });
            }
            self.ai_open = true;
            cx.notify();
            return;
        }
        self.start_stems(name, path, cx);
    }

    fn start_stems(&mut self, name: String, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let id = self.b.ai.split_stems(&path);
        self.midi_jobs.insert(id, self.audio_start(&path));
        self.chat_log.push(ChatEntry::Job { id, label: format!("Splitting {name} into stems"), fraction: 0., done: false });
        self.ensure_ai_poll(cx);
        cx.notify();
    }

    /// The user answered a model licence card.
    pub fn resolve_license(&mut self, index: usize, accept: bool, cx: &mut Context<Self>) {
        let Some(ChatEntry::License { notice, audio, clip_name, status }) = self.chat_log.get_mut(index) else { return };
        if *status != ProposalStatus::Pending {
            return;
        }
        if !accept {
            *status = ProposalStatus::Discarded;
            cx.notify();
            return;
        }
        let (model, audio, name) = (notice.model.clone(), audio.clone(), clip_name.clone());
        match self.b.ai.accept_license(&model) {
            Ok(()) => {
                if let Some(ChatEntry::License { status, .. }) = self.chat_log.get_mut(index) {
                    *status = ProposalStatus::Accepted;
                }
                self.start_stems(name, audio, cx);
            }
            Err(e) => self.toast(format!("Could not save the licence choice: {e}"), cx),
        }
    }

    /// Import an audio file as a new audio track, then analyse it (key/BPM).
    pub fn import_audio(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
        // A WAV header gives the real length right away; other formats get it from the analysis.
        let (sample_rate, channels, frames) = hound::WavReader::open(&path)
            .map(|r| {
                let s = r.spec();
                (s.sample_rate, s.channels, r.duration() as u64)
            })
            .unwrap_or((48_000, 2, 0));
        let start = Snap::Bar.floor(self.b.engine.transport().position.max(0));
        let index = self.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
        let cmds = audio_track_commands(&mut self.s, &name, Color::palette(4), &path, (sample_rate, channels, frames), start, index);
        let source = cmds.iter().find_map(|c| match c {
            Command::AddSource { id, .. } => Some(*id),
            _ => None,
        });
        let Some(source) = source.filter(|_| self.s.edit("Import audio", cmds)) else { return };
        let id = self.b.ai.analyze(&path);
        self.analysis_jobs.insert(id, source);
        self.chat_log.push(ChatEntry::Job { id, label: format!("Analysing {name} (key, BPM)"), fraction: 0., done: false });
        self.ensure_ai_poll(cx);
        self.toast(format!("Imported {name}"), cx);
    }

    pub fn pick_audio_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import audio".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next() {
                    let _ = this.update(cx, |ws, cx| ws.import_audio(p, cx));
                }
        })
        .detach();
    }

    /// Put a library sample on `track` (default: the selected audio track,
    /// else a new one) at `start` (default: the playhead's bar), as one edit.
    pub fn add_sample_clip(&mut self, item: &LibraryItem, track: Option<TrackId>, start: Option<Tick>, cx: &mut Context<Self>) {
        let start = start.unwrap_or_else(|| Snap::Bar.floor(self.b.engine.transport().position.max(0)));
        let target = track
            .or(self.s.selected_track)
            .filter(|t| self.s.project.track(*t).is_some_and(|t| t.kind == TrackKind::Audio));
        let (sample_rate, channels, frames) = hound::WavReader::open(&item.path)
            .map(|r| (r.spec().sample_rate, r.spec().channels, r.duration() as u64))
            .unwrap_or((48_000, 2, (item.duration.max(0.) as f64 * 48_000.) as u64));
        let name = item.name.clone();
        let mut cmds = match target {
            Some(_) => Vec::new(),
            None => {
                let index = self.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
                audio_track_commands(&mut self.s, &name, Color::palette(4), &item.path, (sample_rate, channels, frames), start, index)
            }
        };
        if let Some(track) = target {
            let mut one = audio_track_commands(&mut self.s, &name, Color::palette(4), &item.path, (sample_rate, channels, frames), start, 0);
            // Keep the source, clip and placement; place on the existing track.
            one.retain(|c| !matches!(c, Command::AddTrack { .. }));
            for c in &mut one {
                if let Command::PlaceClip { track: t, .. } = c {
                    *t = track;
                }
            }
            cmds = one;
        }
        // The library's key and tempo travel with the clip.
        for c in &mut cmds {
            if let Command::AddSource { source, .. } = c
                && let Source::Audio(a) = source.as_mut()
            {
                a.key = item.key;
                if item.is_loop {
                    a.warp.original_bpm = item.bpm.map(f64::from);
                    a.warp.enabled = item.bpm.is_some();
                }
            }
            if let Command::AddClip { clip, .. } = c
                && let Some(color) = target.and_then(|t| self.s.project.track(t)).map(|t| t.color)
            {
                clip.color = color;
            }
        }
        if self.s.edit("Add sample", cmds) {
            self.b.library.mark_used(item.id);
            self.toast(format!("Added {name}"), cx);
        }
    }

    pub fn pick_library_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Add to library".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next() {
                    let _ = this.update(cx, |ws, cx| {
                        match ws.b.library.add_folder(&p) {
                            Ok(()) => ws.toast(format!("Scanning {}", p.display()), cx),
                            Err(e) => ws.toast(e, cx),
                        }
                    });
                }
        })
        .detach();
    }

    /// The samples the browser currently lists (folder, search or matches).
    pub fn library_view(&self) -> Vec<LibraryItem> {
        let q = self.query.trim();
        if !q.is_empty() {
            self.b.library.search(q, None)
        } else if self.library_matches {
            let scale = self.s.project.scale;
            let mut items = self.b.library.recommend(scale, self.s.project.tempo.initial_bpm());
            // Only what is in the song's key (or its relative major/minor).
            if let Some(scale) = scale {
                items.retain(|it| it.key.is_some_and(|k| keys_match(k, scale)));
            }
            match self.match_kind {
                MatchKind::All => {}
                MatchKind::Loops => items.retain(|it| it.is_loop),
                MatchKind::OneShots => items.retain(|it| !it.is_loop),
            }
            // Loops first, then one-shots; inside each, by type and name.
            items.sort_by(|a, b| b.is_loop.cmp(&a.is_loop).then_with(|| a.category.cmp(&b.category)).then_with(|| a.name.cmp(&b.name)));
            items
        } else {
            self.b.library.items(&self.library_folder)
        }
    }

    fn apply_analysis(&mut self, source: SourceId, a: &AudioAnalysis) {
        let Some(Source::Audio(old)) = self.s.project.sources.get(&source).cloned() else { return };
        let mut new = old.clone();
        if a.key.is_some() {
            new.key = a.key;
        }
        if let Some(bpm) = a.bpm {
            new.warp.original_bpm = Some(bpm);
        }
        if a.frames > 0 {
            new.sample_rate = a.sample_rate;
            new.channels = a.channels;
            new.frames = a.frames;
        }
        if new != old {
            self.s.edit_as("Analyse audio", Origin::Feature("analysis".into()), vec![
                Command::RemoveSource { id: source },
                Command::AddSource { id: source, source: Box::new(Source::Audio(new)) },
            ]);
        }
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

    pub(crate) fn ensure_ai_poll(&mut self, cx: &mut Context<Self>) {
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
                    if notes.is_empty() {
                        self.chat_log.push(ChatEntry::Assistant(format!("No notes found in {source_name}.")));
                        continue;
                    }
                    let notes: Vec<(u8, Tick, Tick, f32)> = notes.iter().map(|n| (n.key, n.start, n.length, n.velocity)).collect();
                    let mut cmds = self.s.midi_track_commands(&source_name, Color::palette(11), &notes);
                    // Line the MIDI up under the audio it came from.
                    if let Some(Some(start)) = self.midi_jobs.remove(&job) {
                        let track = cmds.iter().find_map(|c| if let Command::AddTrack { track, .. } = c { Some(track.id) } else { None });
                        let clip = cmds.iter().find_map(|c| if let Command::AddClip { id, .. } = c { Some(*id) } else { None });
                        if let (Some(track), Some(clip)) = (track, clip) {
                            let id = self.s.project.alloc_placement_id();
                            cmds.push(Command::PlaceClip { track, placement: Placement { id, clip, start, lane: 0 } });
                        }
                    }
                    let label = format!("Add \"{source_name}\" ({} notes) as a MIDI track", notes.len());
                    if let Some(id) = self.s.propose(&label, Origin::Feature("audio-to-midi".into()), cmds) {
                        self.chat_log.extend(proposal_entry(&self.s, id));
                    }
                }
                AiEvent::StemsReady { job, stems } => {
                    self.set_job(job, 1., true);
                    let start = self.midi_jobs.remove(&job).flatten().unwrap_or(0);
                    let cmds = stem_commands(&mut self.s, &stems, start);
                    let label = format!("Add {} stems as audio tracks", stems.len());
                    if let Some(id) = self.s.propose(&label, Origin::Feature("stem-split".into()), cmds) {
                        self.chat_log.extend(proposal_entry(&self.s, id));
                    }
                }
                AiEvent::Analyzed { job, analysis, .. } => {
                    self.set_job(job, 1., true);
                    if let Some(source) = self.analysis_jobs.remove(&job) {
                        self.apply_analysis(source, &analysis);
                    }
                    let key = analysis.key.map_or("no clear key".to_owned(), |k| k.display());
                    let bpm = analysis.bpm.map_or("no clear tempo".to_owned(), |b| format!("{b:.1} BPM"));
                    self.chat_log.push(ChatEntry::Assistant(format!("Analysis: {key}, {bpm}.")));
                }
                AiEvent::Reply { text } => self.chat_log.push(ChatEntry::Assistant(text)),
                AiEvent::Proposal { label, agent, commands } => {
                    if let Some(id) = self.s.propose(&label, Origin::Agent(agent), commands) {
                        self.chat_log.extend(proposal_entry(&self.s, id));
                    }
                }
                AiEvent::Failed { job, message } => {
                    self.set_job(job, 1., true);
                    self.analysis_jobs.remove(&job);
                    self.midi_jobs.remove(&job);
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
            if let ChatEntry::Job { id, fraction, done, .. } = e
                && *id == job {
                    *fraction = f;
                    *done = finished;
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
            .text_size(px(fs(TEXT_SM)))
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
            .child(crate::widgets::label(format!("{} · {tempo:.2} BPM · {scale}", self.s.project.name), TEXT_SM, pal.muted))
            .child(crate::widgets::label(self.b.engine.status_label(), TEXT_SM, pal.faint))
            .child(self.render_usage())
            .child(self.render_zoom(cx))
            .into_any_element()
    }

    /// What AI-Track is using: audio engine load, process CPU and memory.
    fn render_usage(&mut self) -> AnyElement {
        let pal = p();
        let dsp = self.b.engine.cpu_load().clamp(0., 1.);
        let cpu = self.usage.cpu;
        let hot = |v: f32| if v > 0.8 { pal.record } else if v > 0.5 { pal.meter_mid } else { pal.muted };
        let cell = |name: &'static str, value: String, color: gpui::Hsla| {
            div().flex().items_center().gap(px(SP1)).child(crate::widgets::label(name, TEXT_XS, pal.faint)).child(crate::widgets::mono(value, TEXT_SM, color))
        };
        div()
            .id("usage")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(SP3))
            .tooltip(crate::widgets::tip("Resource use: audio engine load, this app's CPU share and memory", None))
            .child(cell("DSP", format!("{:>2.0}%", dsp * 100.), hot(dsp)))
            .child(cell("CPU", format!("{:>2.0}%", cpu * 100.), hot(cpu)))
            .child(cell("RAM", self.usage.memory_label(), pal.muted))
            .into_any_element()
    }

    /// The app zoom: − 100% +. Clicking the value resets it.
    fn render_zoom(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pal = p();
        let scale = self.look.app.ui_scale;
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                crate::widgets::sized_icon_button("zoom-out", Icon::Minus, false, 18., 10.)
                    .tooltip(crate::widgets::tip("Zoom the interface out", Some("Ctrl+-")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(-1, cx))),
            )
            .child(
                div()
                    .id("zoom-value")
                    .w(px(40.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.08)))
                    .tooltip(crate::widgets::tip("Interface zoom (click to reset)", Some("Ctrl+0")))
                    .child(crate::widgets::mono(format!("{:.0}%", scale * 100.), TEXT_SM, pal.muted))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(0, cx))),
            )
            .child(
                crate::widgets::sized_icon_button("zoom-in", Icon::Add, false, 18., 10.)
                    .tooltip(crate::widgets::tip("Zoom the interface in", Some("Ctrl++")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1, cx))),
            )
            .into_any_element()
    }

    /// Swap the open project for `project`, with a fresh undo history.
    pub fn replace_project(&mut self, project: Project, path: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        self.b.engine.stop();
        self.b.engine.locate(0);
        self.s = AppState::new(project);
        self.project_path = path;
        self.drag = None;
        self.menu = None;
        self.chat_log.retain(|e| !matches!(e, ChatEntry::Proposal { .. }));
        self.synced_revision = None;
        self.fx = views::device_ui::UiState::default();
        cx.notify();
    }

    /// The mixer as its own section: every channel strip, always the same
    /// height, independent of what the detail panel shows.
    fn render_mixer_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.mixer_open {
            return None;
        }
        let strips = views::mixer::render(self, window, cx);
        let grip = div()
            .id("mixer-grip")
            .absolute()
            .top(px(-SP1))
            .left_0()
            .right_0()
            .h(px(SP2))
            .cursor_row_resize()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if e.click_count >= 2 {
                        this.mixer_h = 264.;
                        cx.notify();
                    } else {
                        let start_h = this.mixer_h;
                        this.begin_drag(Drag::MixerResize { start_y: f32::from(e.position.y), start_h }, cx);
                    }
                }),
            );
        Some(
            motion::rise_in(
                "mixer-section",
                div()
                    .relative()
                    .flex_none()
                    .h(px(self.mixer_h))
                    .bg(pane_bg(crate::appearance::Pane::Bottom))
                    .rounded(px(r_lg()))
                    .border_1()
                    .border_color(border())
                    .child(div().size_full().overflow_hidden().rounded(px(r_lg())).child(strips))
                    .child(grip),
                8.,
            )
            .into_any_element(),
        )
    }

    /// Zoom the arrangement around the playhead (or the left edge when it is out of view).
    fn arrange_zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        let a = &mut self.s.arrange;
        let old = a.px_per_beat;
        let new = (old * factor).clamp(1., 160.);
        let width = f32::from(self.regions.arrange.get().size.width);
        let playhead = self.b.engine.transport().position as f32 / PPQ as f32;
        let anchor_px = playhead * old - a.scroll_x;
        let anchor = if (0. ..=width).contains(&anchor_px) { playhead } else { a.scroll_x / old };
        let offset = anchor * old - a.scroll_x;
        a.px_per_beat = new;
        a.scroll_x = (anchor * new - offset).max(0.);
        cx.notify();
    }

    /// An arrow-key edit of the selection (notes, or the selected clip).
    fn key_edit(&mut self, e: KeyEdit, cx: &mut Context<Self>) {
        if self.s.key_edit(e) {
            cx.notify();
        }
    }

    /// Step the interface zoom by 10 % (`0` resets to 100 %), within 80–150 %.
    pub fn zoom_by(&mut self, step: i32, cx: &mut Context<Self>) {
        self.set_look(false, cx, |a| {
            a.ui_scale = if step == 0 { 1. } else { ((a.ui_scale + step as f32 * 0.1) * 10.).round().clamp(8., 15.) / 10. };
        });
    }

    /// A dismissable strip under the top bar (no audio device and similar).
    fn render_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let text = self.b.engine.banner()?;
        let pal = p();
        Some(
            div()
                .flex_none()
                .mx(px(SP1))
                .mt(px(SP1))
                .px(px(SP3))
                .py(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .rounded(px(r_lg()))
                .bg(with_alpha(pal.record, 0.12))
                .border_1()
                .border_color(with_alpha(pal.record, 0.4))
                .child(crate::icons::icon(Icon::Volume, 14., pal.record))
                .child(div().flex_1().min_w(px(0.)).child(crate::widgets::label(text, TEXT_SM, pal.text)))
                .child(crate::widgets::text_button("banner-audio", "Audio settings", None, false).on_click(cx.listener(|this, _, _, cx| {
                    this.open_modal(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Audio), cx);
                })))
                .child(crate::widgets::icon_button("banner-close", Icon::X, false).on_click(cx.listener(|this, _, _, cx| {
                    this.b.engine.dismiss_banner();
                    cx.notify();
                })))
                .into_any_element(),
        )
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
                        .rounded(px(r_lg()))
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

fn folder_exists(folders: &[crate::bridge::LibraryFolder], id: &str) -> bool {
    folders.iter().any(|f| f.id == id || folder_exists(&f.children, id))
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
fn stem_commands(s: &mut AppState, stems: &[(String, std::path::PathBuf)], start: Tick) -> Vec<Command> {
    let mut cmds = Vec::new();
    let index = s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
    for (i, (name, path)) in stems.iter().enumerate() {
        // Stems are WAV; the header has the real length.
        let (sample_rate, channels, frames) =
            hound::WavReader::open(path).map(|r| (r.spec().sample_rate, r.spec().channels, r.duration() as u64)).unwrap_or((48_000, 2, 0));
        cmds.extend(audio_track_commands(s, &format!("{name} Stem"), Color::palette(2 + i * 3), path, (sample_rate, channels, frames), start, index + i));
    }
    cmds
}

/// Commands adding an audio track at `index` with one clip of `path` placed at `start`.
pub fn audio_track_commands(
    s: &mut AppState,
    name: &str,
    color: Color,
    path: &std::path::Path,
    (sample_rate, channels, frames): (u32, u16, u64),
    start: Tick,
    index: usize,
) -> Vec<Command> {
    let track = s.project.alloc_track_id();
    let src = s.project.alloc_source_id();
    let clip = s.project.alloc_clip_id();
    // Clip length at the project tempo (4 bars until the length is known).
    let length = if frames > 0 && sample_rate > 0 {
        let secs = frames as f64 / sample_rate as f64;
        ((secs * s.project.tempo.initial_bpm() / 60. * PPQ as f64).round() as Tick).max(PPQ)
    } else {
        4 * BAR
    };
    vec![
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, name, color)), index },
        Command::AddSource {
            id: src,
            source: Box::new(Source::Audio(AudioSource {
                path: path.to_path_buf(),
                hash: None,
                sample_rate,
                channels,
                frames,
                warp: Warp::default(),
                key: None,
            })),
        },
        Command::AddClip {
            id: clip,
            clip: Box::new(Clip::new(name, color, src, length)),
        },
        Command::PlaceClip { track, placement: Placement { id: s.project.alloc_placement_id(), clip, start, lane: 0 } },
    ]
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let audition_track = if self.bottom_open && self.s.bottom_tab == BottomTab::PianoRoll && self.modal.is_none() {
            self.audition_track()
        } else {
            None
        };
        if !self.audition.matches_track(audition_track) {
            self.release_audition(cx);
        }
        self.sync_engine();
        self.b.engine.tick();
        if let Some(animate) = self.look.pending.take() {
            self.apply_look(window, animate, cx);
        }
        self.meters.tick(self.b.engine.as_mut(), self.s.selected_track);
        let editors_visible = self.bottom_open && self.s.bottom_tab == BottomTab::Devices;
        if self.wants_frames() || self.drag.is_some() || theme::tweening() || (editors_visible && self.fx.wants_frames()) || self.meters.wants_frames() || views::sample_editor::wants_frames(self) {
            window.request_animation_frame();
        }
        self.fx.sample_rate = f64::from(self.b.engine.audio_config().sample_rate.max(8_000));
        {
            let project = &self.s.project;
            self.fx.prune(|id| project.device(id).is_some());
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
            .bg(pane_bg(crate::appearance::Pane::Center))
            .rounded(px(r_lg()))
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
        let mixer = self.render_mixer_section(window, cx);
        let menu = self.render_menu(window, cx);
        let modal = views::prefs::render_modal(self, window, cx);
        let backdrop = views::prefs::backdrop(self);
        let capture = self.drag_capture(cx);
        let fx_capture = views::device_ui::capture(self, cx);
        let sample_capture = views::sample_editor::capture(self, cx);
        let fx_overlay = views::device_ui::overlay(self, cx);
        let meter_strip = views::meters::render(self, cx);
        let meter_layers = views::meters::overlay(self, cx);
        let toast = self.render_toast();
        let status = self.render_status(cx);
        let banner = self.render_banner(cx);

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::piano_key_down))
            .on_key_up(cx.listener(Self::piano_key_up))
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
            .on_action(cx.listener(|this, _: &NewProject, window, cx| this.file_new(crate::project_io::Template::Empty, window, cx)))
            .on_action(cx.listener(|this, _: &OpenProject, window, cx| this.file_open(window, cx)))
            .on_action(cx.listener(|this, _: &SaveProject, _, cx| this.file_save(cx)))
            .on_action(cx.listener(|this, _: &SaveProjectAs, _, cx| this.file_save_as(None, cx)))
            .on_action(cx.listener(|this, _: &SplitClip, _, cx| {
                let at = this.b.engine.transport().position;
                match this.s.split_at(at) {
                    0 => this.toast("Nothing to split: put the playhead inside a clip of the selected track", cx),
                    _ => cx.notify(),
                }
            }))
            .on_action(cx.listener(|this, _: &GroupTracks, _, cx| {
                if this.s.group_selected().is_some() {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &UngroupTracks, _, cx| {
                if this.s.ungroup_selected() {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &AddAudioTrack, _, cx| {
                this.s.add_track(TrackKind::Audio);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AddMidiTrack, _, cx| {
                this.s.add_track(TrackKind::Instrument);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AddReturnTrack, _, cx| {
                this.s.add_track(TrackKind::Return);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ArrangeZoomIn, _, cx| this.arrange_zoom(1.25, cx)))
            .on_action(cx.listener(|this, _: &ArrangeZoomOut, _, cx| this.arrange_zoom(0.8, cx)))
            .on_action(cx.listener(|this, _: &ArrangeFit, _, cx| {
                // Fit the whole song to the width of the lanes.
                let width = f32::from(this.regions.arrange.get().size.width).max(200.);
                let beats = (this.s.project.arrangement_end() as f32 / PPQ as f32).max(4.);
                this.s.arrange.px_per_beat = (width / beats * 0.96).clamp(1., 160.);
                this.s.arrange.scroll_x = 0.;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_by(1, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_by(-1, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom_by(0, cx)))
            .on_action(cx.listener(|this, _: &NudgeUp, _, cx| this.key_edit(KeyEdit::Up, cx)))
            .on_action(cx.listener(|this, _: &NudgeDown, _, cx| this.key_edit(KeyEdit::Down, cx)))
            .on_action(cx.listener(|this, _: &NudgeLeft, _, cx| this.key_edit(KeyEdit::Left, cx)))
            .on_action(cx.listener(|this, _: &NudgeRight, _, cx| this.key_edit(KeyEdit::Right, cx)))
            .on_action(cx.listener(|this, _: &SemitoneUp, _, cx| this.key_edit(KeyEdit::SemitoneUp, cx)))
            .on_action(cx.listener(|this, _: &SemitoneDown, _, cx| this.key_edit(KeyEdit::SemitoneDown, cx)))
            .on_action(cx.listener(|this, _: &OctaveUp, _, cx| this.key_edit(KeyEdit::OctaveUp, cx)))
            .on_action(cx.listener(|this, _: &OctaveDown, _, cx| this.key_edit(KeyEdit::OctaveDown, cx)))
            .on_action(cx.listener(|this, _: &Longer, _, cx| this.key_edit(KeyEdit::Longer, cx)))
            .on_action(cx.listener(|this, _: &Shorter, _, cx| this.key_edit(KeyEdit::Shorter, cx)))
            .on_action(cx.listener(|this, _: &VelocityUp, _, cx| this.key_edit(KeyEdit::VelocityUp, cx)))
            .on_action(cx.listener(|this, _: &VelocityDown, _, cx| this.key_edit(KeyEdit::VelocityDown, cx)))
            .on_action(cx.listener(|this, _: &ToggleActive, _, cx| {
                if let Some(done) = this.s.toggle_active() {
                    this.toast(done, cx);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Duplicate, _, cx| {
                if this.s.duplicate_selection() {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleDetail, _, cx| {
                this.bottom_open = !this.bottom_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDetailView, _, cx| {
                // Shift+Tab in Live: swap the detail view between devices and the clip.
                let next = if this.s.bottom_tab == BottomTab::Devices { BottomTab::PianoRoll } else { BottomTab::Devices };
                this.bottom_open = true;
                this.set_tab(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowMixer, _, cx| {
                this.mixer_open = !this.mixer_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenPreferences, _, cx| this.open_modal(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Appearance), cx)))
            .on_action(cx.listener(|this, _: &ExportAudio, _, cx| this.open_export(cx)))
            .on_action(cx.listener(|this, _: &TabDevices, _, cx| this.set_tab(BottomTab::Devices, cx)))
            .on_action(cx.listener(|this, _: &TabPianoRoll, _, cx| this.set_tab(BottomTab::PianoRoll, cx)))
            .on_action(cx.listener(|this, _: &TabSteps, _, cx| this.set_tab(BottomTab::Steps, cx)))
            .on_action(cx.listener(|this, _: &TabMixer, _, cx| {
                this.mixer_open = !this.mixer_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.menu = None;
                this.modal = if this.export.running.is_some() { this.modal } else { None };
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
            .font_family(font_ui())
            .text_size(px(fs(TEXT_MD)))
            .text_color(pal.text)
            .children(backdrop)
            .child(views::topbar::render(self, window, cx))
            .children(banner)
            .children(meter_strip)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(SP1))
                    .p(px(SP1))
                    .child(middle)
                    .children(mixer)
                    .child(bottom),
            )
            .child(status)
            .children(toast)
            .children(modal)
            .children(capture)
            .children(fx_capture)
            .children(sample_capture)
            .children(fx_overlay)
            .children(meter_layers)
            .children(menu)
    }
}
