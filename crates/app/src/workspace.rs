//! The root view: owns [`AppState`] and the [`Bridges`], lays out the
//! window, dispatches keyboard actions and runs every drag gesture.
//!
//! Drags are one state machine ([`Drag`]): a mouse-down in any view stores
//! where it started, a window-wide listener (registered only while a drag
//! is active) feeds moves into `drag_move`, and mouse-up seals the undo step.
//! Frames are requested only while the transport plays, meters fall, or a
//! drag or animation is in flight; an idle window draws nothing.

use crate::bridge::{AiEvent, AudioAnalysis, BpmTrust, Bridges, DrumExtractMode, DrumExtractRequest, JobId, LibraryItem, MidiMode, ModelNotice};
use crate::icons::Icon;
use crate::state::*;
use crate::theme::{self, *};
use crate::widgets::{InputEvent, TextInput};
use crate::{actions::*, motion, views};
use aitrack_model::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, DispatchPhase, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, Styled, Subscription, Task, Window,
    anchored, canvas, deferred, div, prelude::FluentBuilder, px,
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
    Send {
        track: TrackId,
        index: usize,
    },
    /// An audio clip's transpose (-48..=48 st) and detune (-50..=50 ct), and
    /// the other selected audio clips' with it.
    Transpose { clip: ClipId },
    Detune { clip: ClipId },
    /// A knob of a rack chain (a pad of a Drum Rack).
    Chain { rack: DeviceId, index: usize, knob: ChainKnob },
    /// The volume of one of a Drum Rack's returns.
    Return { rack: DeviceId, index: usize },
}

/// Which knob of a rack chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChainKnob {
    Volume,
    Pan,
    /// The send into return `n`.
    Send(usize),
}

/// What a click-to-type value box edits (the device number cells, the tempo box).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumTarget {
    Param { device: DeviceId, param: u32 },
    Tempo,
    /// An audio clip's transpose (st) and detune (ct), as the knobs edit them.
    Transpose { clip: ClipId },
    Detune { clip: ClipId },
    /// An audio clip's Seg. BPM (the tempo its sample was recorded at).
    SegBpm { clip: ClipId },
    /// A clip's start offset and its length, typed as bars.beats.sixteenths.
    ClipStart { clip: ClipId },
    ClipLength { clip: ClipId },
    /// A track's volume in dB (the arrangement header's value box).
    Volume { track: TrackId },
}

/// A value box being typed into. Clicking a draggable number opens it with
/// the current value selected: the first key replaces it, Enter applies,
/// Escape or a click anywhere drops it. Dragging still works — a click
/// that moved is a drag, not an edit ([`click_dragged`]).
#[derive(Clone, Debug)]
pub struct NumEdit {
    pub target: NumTarget,
    pub text: String,
    /// Just opened: the next digit replaces the pre-filled value.
    pub fresh: bool,
}

/// What a keystroke did to an open value-box edit.
pub enum NumKey {
    /// Not editing, or a key that isn't part of a number.
    Ignored,
    /// The text changed.
    Typed,
    /// Enter: apply the text.
    Commit,
    /// Escape: drop the text.
    Cancel,
}

/// Key context on the workspace and floating device windows: when an inline
/// number box is open, adding `TextInput` suppresses single-key shortcuts (`1`-`4`,
/// `0`, `-`, `+`, `backspace`, `escape`, `space`, ...) before GPUI dispatches them.
pub(crate) fn workspace_key_context(editing_num: bool) -> &'static str {
    if editing_num { "Workspace TextInput" } else { "Workspace" }
}

/// Extract a numeric character from `k`, allowing `Shift`/`fn` (needed for `+`
/// and international/numpad layouts) while ignoring shortcut modifiers.
pub(crate) fn num_edit_char(k: &gpui::Keystroke) -> Option<&str> {
    let m = &k.modifiers;
    // `platform` is Cmd on macOS and the Windows key on Windows, where
    // `secondary()` is Ctrl: a chord with either never types into the field.
    if m.secondary() || m.platform || m.control || m.alt {
        return None;
    }
    let raw = k
        .key_char
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| (k.key.len() == 1).then_some(k.key.as_str()))?;
    raw.chars().all(|c| c.is_ascii_digit() || "-+.,".contains(c)).then_some(raw)
}

/// What a value-box text parses to (`,` works as the decimal mark).
pub fn parse_plain(text: &str) -> Option<f32> {
    text.trim().replace(',', ".").parse().ok()
}

/// A plain value as editable text: no unit, trailing zeros trimmed.
pub fn plain_text(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

/// A click that moved more than a few px between press and release was a
/// drag, not a click (draggable value boxes begin the drag on mouse-down).
pub fn click_dragged(e: &ClickEvent) -> bool {
    match e {
        ClickEvent::Mouse(m) => {
            let dx = f32::from(m.up.position.x - m.down.position.x);
            let dy = f32::from(m.up.position.y - m.down.position.y);
            dx * dx + dy * dy > 16.
        }
        ClickEvent::Keyboard(_) | ClickEvent::Touch(_) => false,
    }
}

#[derive(Clone, Debug)]
pub enum Drag {
    Knob { target: KnobTarget, last_y: f32, value: f32 },
    Fader { track: TrackId, last_y: f32, pos: f32, travel: f32 },
    /// A sideways volume slider (a tall track header).
    HFader { track: TrackId, last_x: f32, pos: f32, travel: f32 },
    /// A track header's bottom edge: the track's height.
    TrackHeight { track: TrackId, start_y: f32, start_h: f32 },
    Tempo { last_y: f32, bpm: f64 },
    BottomResize { start_y: f32, start_h: f32 },
    BottomSplit { start_y: f32, start_split: f32, total_h: f32 },
    MixerResize { start_y: f32, start_h: f32 },
    Notes { clip: ClipId, originals: Vec<Note>, origin: (Tick, i32), resize: bool, applied: (Tick, i32), preview: Option<(TrackId, u8)> },
    Velocity { clip: ClipId, originals: Vec<Note>, last_y: f32, lane_h: f32, delta: f32 },
    Probability { clip: ClipId, originals: Vec<Note>, last_y: f32, lane_h: f32, delta: f32 },
    Audition { track: TrackId, key: Option<u8> },
    /// A pad of a Drum Rack held down (it sounds until the button is let go).
    Pad,
    /// The Sounds panel's edge (its width) and its volume bar.
    SoundsResize { start_x: f32, start: f32 },
    SoundsVolume { start_x: f32, start: f32 },
    /// A handle of a rack panel: a zone, a macro mapping's range.
    Rack(views::devices::rack_ui::RackDrag),
    /// An end of a modulator mapping's range bar.
    Mod(views::device_ui::modulator::ModDrag),
    Marquee { clip: ClipId, from: Point<Pixels>, to: Point<Pixels>, base: BTreeSet<NoteId> },
    Clip { track: TrackId, placement: PlacementId, start: Tick, grab_tick: Tick, applied: (Tick, TrackId) },
    ClipResize { clip: ClipId, length: Tick, grab_tick: Tick },
    /// A fade handle, a fade's slope or the left edge of a clip in the lanes.
    ClipShape(Box<views::arrange_clips::ShapeDrag>),
    /// Ctrl+drag on a clip: copies of the selection follow the pointer.
    ClipCopy(views::arrange_clips::CopyDrag),
    /// Dragging in empty lane space selects time, from where the drag began (tick, track).
    TimeSel { anchor: (Tick, TrackId) },
    AutoPoint { track: TrackId, target: AutomationTarget, original: Vec<AutomationPoint>, index: usize },
    /// Alt + drag on an automation segment bends it.
    AutoCurve { track: TrackId, target: AutomationTarget, index: usize, start_y: f32, start_curve: f32, rising: bool },
    /// Dragging off the line in an automation lane selects a time range of it, from `anchor`.
    AutoRange { track: TrackId, target: AutomationTarget, anchor: Tick },
    /// Dragging the line inside the selected automation range raises or
    /// lowers just that part (`original` is the lane when the drag began).
    AutoShift { range: crate::state::AutoRange, original: Vec<AutomationPoint>, start_y: f32 },
    /// A transform handle of the selected automation range: scale, skew or
    /// time stretch. `lo`/`hi` are the range's lowest and highest value when it began.
    AutoXform { range: crate::state::AutoRange, original: Vec<AutomationPoint>, kind: views::arrangement::XformKind, start_y: f32, lo: f32, hi: f32 },
    /// Dragging on a take lane picks a range of it; releasing promotes it into
    /// the main lane (with `hold`, from Shift, it waits for Enter).
    TakeRange { track: TrackId, lane: usize, anchor: Tick, hold: bool },
    LoopBrace { anchor: Tick },
    /// A locator on the ruler: dragged to a new place, or (if it never
    /// moved) a click that jumps there. `grab` is how far past the mark the
    /// pointer took hold of it.
    Locator { id: LocatorId, grab: Tick, moved: bool },
    Scrub,
    /// Colour picker: the hue strip or the saturation/value square.
    Pick { hue: bool },
}

/// Popovers and context menus.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    Scale,
    TrackScale(TrackId),
    ClipScale(ClipId),
    Snap,
    PianoTools,
    AddDevice(TrackId),
    /// Factory presets of a stock device.
    Presets(DeviceId),
    /// Library actions (right-click in the browser).
    LibraryContext,
    /// Right-click on an arrangement clip, and on empty lane space.
    ClipContext,
    LaneContext,
    /// Right-click on a track header (acts on the selected tracks).
    TrackContext(TrackId),
    /// Right-click on a locator of the ruler.
    LocatorContext(LocatorId),
    /// Right-click on a take lane's header (track, index among its take lanes).
    TakeLaneContext(TrackId, usize),
    /// Right-click in an automation lane: insert a shape, simplify, capture.
    AutomationContext(TrackId, AutomationTarget),
    /// Right-click on a device's title bar (acts on the picked devices).
    DeviceContext(DeviceId),
    /// Right-click on a device row in the Cmd+K palette (favorite / reassign group).
    PaletteDevice { key: String, name: String, instrument: bool },
    TimeSignature,
    /// A clip's launch mode, launch quantisation and follow action (the Clip View).
    ClipLaunch(ClipId),
    ClipQuantize(ClipId),
    ClipFollow(ClipId),
    /// Undo / redo history popover opened from the status bar.
    UndoHistory,
    /// Right-click on a Stutter step: copy, paste, clear, fill.
    StutterStep { device: DeviceId, step: usize },
    /// File: new from a template, open, save.
    File,
    /// Slice an audio clip into a Drum Rack: how to cut.
    SliceToDrumRack(TrackId, PlacementId),
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

/// The card of a generated MIDI part. The part itself (notes, spec, locks)
/// lives in the assistant's `parts`, keyed by `sid`; this is what is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct PartCard {
    pub sid: u64,
    pub label: String,
    pub origin: String,
    pub summary: Vec<String>,
    /// Shown on the card so a take can be named and found again.
    pub seed: u64,
    /// The layers that can be held, and whether each is.
    pub layers: Vec<(aitrack_ai::generate::spec::Role, bool)>,
    /// The bars that can be held (empty for one bar), and whether each is.
    pub bars: Vec<bool>,
    pub status: views::ai_panel::part::Status,
    pub destructive: bool,
    /// Why this is not what was asked for (the rules wrote it after a model failed).
    pub notice: Option<String>,
    /// A model is writing the next take.
    pub busy: bool,
}

#[derive(Clone, Debug)]
pub enum ChatEntry {
    Assistant(String),
    /// A generated MIDI part with Listen, Vary, Regenerate and holds.
    Part(PartCard),
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
    pub step_grid: Cell<Bounds<Pixels>>,
    pub step_lane: Cell<Bounds<Pixels>>,
}

pub struct Workspace {
    pub s: AppState,
    pub b: Bridges,
    pub focus: FocusHandle,
    pub audition: crate::audition::Audition,
    pub piano_keyboard: bool,
    pub piano_preview: bool,
    pub piano_octave: u8,
    pub piano_velocity: u8,
    pub drag: Option<Drag>,
    /// The value box being typed into, if any (`click_dragged` guards it
    /// against drags).
    pub num_edit: Option<NumEdit>,
    pub menu: Option<Menu>,
    /// An automation lane's target chooser, while open.
    pub auto_chooser: Option<views::arrangement::Chooser>,
    /// The chooser's search field.
    pub auto_search: Entity<TextInput>,
    /// Right-click on a fader, knob or send: "Show automation" for it.
    pub param_menu: Option<(TrackId, AutomationTarget, Point<Pixels>)>,
    /// Dev view: text to type into the chooser's search on the first frame.
    auto_search_pending: Option<String>,
    /// View state of the visual device editors.
    pub fx: views::device_ui::UiState,
    /// View state of the sample editor (the Clip tab for an audio clip).
    #[allow(dead_code)] // Read once the sample editor is filled in.
    pub sample: views::sample_editor::UiState,
    /// Waveform peaks for the audio clips on screen.
    pub waves: crate::waveform::WaveformCache,
    /// Timestamp of the last rendered frame (used by low-power throttling).
    pub last_frame: Instant,
    /// The meter strip under the top bar.
    pub meters: views::meters::MeterStrip,
    /// CPU and memory of this process, for the status bar.
    pub usage: crate::usage::Usage,
    /// The `.aitrack` file the project was opened from or saved to.
    pub project_path: Option<std::path::PathBuf>,
    /// Recently opened/saved `.aitrack` project files (most recent first).
    pub recent_projects: Vec<std::path::PathBuf>,
    /// Periodic copies of the unsaved project (see `autosave.rs`).
    pub(crate) autosaver: crate::autosave::Autosaver,
    /// Copies found at start-up, newer than their project: offered on the first frame.
    pub(crate) recovery: Vec<crate::autosave::Found>,
    /// Audio files of the project just opened that are not where it says.
    pub(crate) pending_missing: Vec<aitrack_model::paths::MissingSource>,
    /// What the engine was last told about each audio track's input and monitor, and where it records.
    track_io_sent: std::collections::BTreeMap<TrackId, (aitrack_model::InputSource, aitrack_model::Monitor)>,
    rec_dir: Option<std::path::PathBuf>,
    /// Whether the File → Open Recent cascading submenu is expanded.
    pub file_recent_open: bool,
    /// Whether the File → New from Template cascading submenu is expanded.
    pub file_templates_open: bool,
    /// Hovered group and search box in the Add Device popup menu.
    pub device_menu_hover: Option<views::menus::DeviceMenuGroup>,
    pub device_menu_search: Entity<TextInput>,
    /// Height of the detail panel (drag its top edge: [`Workspace::begin_bottom_resize`]).
    pub bottom_h: f32,
    pub bottom_open: bool,
    /// Show Clip View and Device View stacked together (Live 12 split detail panel).
    pub bottom_stacked: bool,
    /// Fraction of the stacked detail body given to Clip View (0.25..=0.75).
    pub bottom_split: f32,
    /// Show the live Info View help strip in the status bar (`?` toggles).
    pub info_view: bool,
    /// When false (default), the top-bar timer displays `MM:SS.mmm`; click to toggle to bars.
    pub show_bars_timer: bool,
    /// Window height at the last render, for clamping panel resizes.
    pub window_h: f32,
    pub window_w: f32,
    /// The Cmd+K spotlight palette.
    pub palette: views::palette::PaletteState,
    /// The mixer section between the arrangement and the detail panel.
    pub mixer_open: bool,
    pub mixer_h: f32,
    /// Set to open the palette on the first frame; only the dev view needs
    /// it, everything with a window calls `views::palette::open`.
    pub palette_pending: bool,
    pub ai_open: bool,
    /// The palette's search field.
    pub search: Entity<TextInput>,
    pub chat_log: Vec<ChatEntry>,
    pub regions: Rc<Regions>,
    pub toast: Option<(String, Instant)>,
    /// Update checks, downloads and installs (`updater.rs`).
    pub updater: crate::updater::Updater,
    /// The external MCP server: settings, token, the channel from its threads (`mcp_host.rs`).
    pub mcp: crate::mcp_host::McpHost,
    /// A crash report from the last run, offered once (`logging.rs`).
    pub crash_notice: Option<crate::logging::CrashNotice>,
    pub(crate) crash_rx: crossbeam_channel::Receiver<crate::logging::CrashNotice>,
    /// The shortcuts sheet and the Rename field.
    pub ui: crate::commands::EditUi,
    synced_revision: Option<u64>,
    /// When a drag last went through the engine's fast path: a moment after,
    /// the engine gets the project itself.
    fast_at: Option<std::time::Instant>,
    meters_live: bool,
    ai_poll: Option<Task<()>>,
    menu_counter: u64,
    pub view_switch: u64,
    pub tab_switch: u64,
    _subs: Vec<Subscription>,
    pub look: views::prefs::Look,
    pub modal: Option<views::prefs::Modal>,
    pub export: views::export::ExportDialog,
    /// Freeze renders in flight (progress on the track headers).
    pub freeze: crate::freeze::FreezeUi,
    pub export_name: Entity<TextInput>,
    /// Audio-to-MIDI engine picked in the assistant panel.
    pub midi_mode: MidiMode,
    /// Analysis jobs → the audio source they fill in.
    analysis_jobs: std::collections::HashMap<JobId, SourceId>,
    /// Counts the projects opened in this window: work started for one project
    /// (a file probe) is dropped if it finishes after another one was opened.
    pub(crate) project_epoch: u64,
    /// View state of the rack panels (pads, Map mode, zones…).
    pub rack_ui: views::devices::rack_ui::RackUi,
    /// The Sounds panel docked beside the arrangement.
    pub sounds: views::sounds::Sounds,
    /// Extract-MIDI and stem jobs → where their audio clip sits in the arrangement.
    midi_jobs: std::collections::HashMap<JobId, Option<Tick>>,
    /// Clean drum extraction jobs (from `/drums`) → arrangement placement tick.
    drum_jobs: std::collections::HashMap<JobId, Option<Tick>>,
    pub organize: OrganizeUi,
    /// Devices open in windows of their own (`views::device_window`).
    pub device_windows: std::collections::HashMap<DeviceId, gpui::WindowHandle<views::device_window::DeviceWindow>>,
    /// The Acid 303 phrase generator's settings per device (UI state, not project data).
    pub acid_gen: std::collections::HashMap<DeviceId, views::device_ui::acid_gen::AcidGen>,
    /// A device window the dev view asked for (opened on the first frame).
    window_pending: Option<DeviceId>,
    /// Device windows to open on the next frame (dev view `devwins=`).
    windows_pending: Vec<DeviceId>,
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

/// Whether a sample in `key` sits in a song in `song`: the same key or its
/// relative major/minor (the same number on the Camelot wheel).
pub fn keys_match(key: Scale, song: Scale) -> bool {
    let number = |s: Scale| s.camelot().map(|c| c.trim_end_matches(['A', 'B']).to_owned());
    match (number(key), number(song)) {
        (Some(a), Some(b)) => a == b,
        _ => key.mask() == song.mask(),
    }
}

/// A sample dragged out of the palette.
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
        let search = cx.new(|cx| TextInput::new(cx, "Search samples, devices, presets and commands", Some(Icon::Search)));
        let export_name = cx.new(|cx| TextInput::new(cx, "File name", Some(Icon::File)));
        let auto_search = cx.new(|cx| TextInput::new(cx, "Search parameters", Some(Icon::Search)));
        let device_menu_search = cx.new(|cx| TextInput::new(cx, "Search effects & synths…", Some(Icon::Search)));
        let look = views::prefs::Look::load(cx);
        let sounds = views::sounds::Sounds::new(cx);
        let subs = vec![
            cx.subscribe(&sounds.query, |this, _, e: &InputEvent, cx| {
                if let InputEvent::Changed(q) = e {
                    this.sounds.typed(q.clone());
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |this, _, e: &InputEvent, cx| {
                let (InputEvent::Changed(q) | InputEvent::Submit(q)) = e;
                views::palette::set_query(this, q);
                cx.notify();
            }),
            cx.subscribe(&auto_search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&device_menu_search, |this, _, _: &InputEvent, cx| {
                this.device_menu_hover = None;
                cx.notify();
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
        let mut this = Self {
            s: AppState::new(Project::new("")),
            b: bridges,
            focus: cx.focus_handle(),
            audition: Default::default(),
            piano_keyboard: false,
            piano_preview: true,
            piano_octave: 4,
            piano_velocity: 100,
            drag: None,
            num_edit: None,
            menu: None,
            auto_chooser: None,
            auto_search,
            param_menu: None,
            auto_search_pending: None,
            fx: views::device_ui::UiState::default(),
            sample: Default::default(),
            waves: Default::default(),
            meters: views::meters::MeterStrip::default(),
            usage: crate::usage::Usage::default(),
            last_frame: Instant::now(),
            project_path: None,
            recent_projects: crate::project_io::load_recent_projects(),
            autosaver: crate::autosave::Autosaver::new(std::time::Instant::now()),
            recovery: crate::autosave::find(&crate::autosave::dir()),
            pending_missing: Vec::new(),
            track_io_sent: Default::default(),
            rec_dir: None,
            file_recent_open: false,
            file_templates_open: false,
            device_menu_hover: None,
            device_menu_search,
            bottom_h: theme::Metrics::of(look.app.density).dock_h,
            bottom_open: true,
            bottom_stacked: false,
            bottom_split: 0.5,
            info_view: false,
            show_bars_timer: false,
            window_h: 920.,
            window_w: 1480.,
            palette: Default::default(),
            mixer_open: false,
            mixer_h: theme::Metrics::of(look.app.density).mixer_h,
            palette_pending: false,
            // Closed at start: the arrangement gets the room; the Assistant button opens it.
            ai_open: false,
            search,
            chat_log: Vec::new(),
            regions: Rc::new(Regions::default()),
            toast: None,
            mcp: crate::mcp_host::McpHost::load(),
            updater: crate::updater::Updater::new(Instant::now(), crate::updater::Config::from_env(), crate::updater::UpdatePrefs::load()),
            crash_notice: None,
            crash_rx: crate::logging::spawn_crash_lookup(),
            ui: crate::commands::EditUi::new(cx),
            synced_revision: None,
            fast_at: None,
            meters_live: false,
            ai_poll: None,
            menu_counter: 0,
            view_switch: 0,
            tab_switch: 0,
            _subs: subs,
            look,
            modal: None,
            export: views::export::ExportDialog::default(),
            freeze: Default::default(),
            export_name,
            midi_mode: MidiMode::default(),
            analysis_jobs: Default::default(),
            project_epoch: 0,
            rack_ui: Default::default(),
            sounds,
            midi_jobs: Default::default(),
            drum_jobs: Default::default(),
            organize: OrganizeUi::default(),
            device_windows: Default::default(),
            acid_gen: Default::default(),
            window_pending: None,
            windows_pending: Vec::new(),
            _housekeeping: housekeeping,
        };
        // Seed the assistant with a greeting and one reviewable proposal.
        this.chat_log.push(ChatEntry::Assistant(
            "Hi! I can extract MIDI from audio, split stems and find samples in the project key. Edits I suggest show up here as proposals you accept or discard; each one is a single undo step. Type / to see every action.".into(),
        ));
        let roots = [33u8, 29, 36, 31];
        let notes: Vec<(u8, Tick, Tick, f32)> =
            roots.iter().enumerate().map(|(i, k)| (*k, i as Tick * BAR, BAR - PPQ / 4, 0.85)).collect();
        let cmds = s.midi_track_commands("Sub Bass", Color::palette(6), &notes);
        if let Some(id) = s.propose("Add a sub-bass layer in A minor", Origin::Agent("Claude Code".into()), cmds) {
            this.chat_log.extend(proposal_entry(&s, id));
        }
        this.s = s;
        // The saved assistant may be an agent CLI, which the AI bridge drives.
        if let views::ai_panel::state::Backend::Agent(kind) = views::ai_panel::state::Backend::from_pref(&this.look.app.assistant_model) {
            this.b.ai.set_agent(Some(kind));
        }
        this.mcp_autostart();
        this.apply_dev_view(cx);
        // `slash` opens the assistant with its action menu showing.
        if std::env::var("AITRACK_DEV_VIEW").is_ok_and(|v| v.split(',').any(|w| w.trim() == "slash")) {
            this.ai_open = true;
            let input = views::ai_panel::state::get(cx).read(cx).input.clone();
            input.update(cx, |i, cx| i.set_text("/", cx));
        }
        // The agent CLIs are looked up in the background; collect the result.
        this.ensure_ai_poll(cx);
        this
    }

    /// Development aid for visual checks without touching the keyboard:
    /// `AITRACK_DEV_VIEW=devices,meters,play,big,track=Bass,group,slash` opens the app in
    /// that state. Unknown words are ignored; unset, nothing happens.
    /// The Vault of the selected track, else the first one (dev view words).
    fn dev_vault(&self) -> Option<DeviceId> {
        let p = &self.s.project;
        let is_vault = |d: &&Device| matches!(&d.kind, aitrack_model::DeviceKind::Stock { plugin_id } if plugin_id == aitrack_stock::instrument::vault::ID);
        self.s.selected_track.and_then(|t| p.track(t)).and_then(|t| t.devices.iter().find(is_vault).map(|d| d.id)).or_else(|| p.tracks.iter().flat_map(|t| &t.devices).find(is_vault).map(|d| d.id))
    }

    fn apply_dev_view(&mut self, cx: &mut Context<Self>) {
        let Ok(spec) = std::env::var("AITRACK_DEV_VIEW") else { return };
        self.apply_dev_words(&spec, cx);
    }

    /// The words of `AITRACK_DEV_VIEW`, applied (the frame-time benchmark passes its own).
    pub(crate) fn apply_dev_words(&mut self, spec: &str, cx: &mut Context<Self>) {
        for word in spec.split(',').map(str::trim) {
            match word {
                "devices" => self.s.bottom_tab = BottomTab::Devices,
                "piano" => self.s.bottom_tab = BottomTab::PianoRoll,
                // An Acid 303 on a demo track with a generated phrase (fixed seed), the Devices tab open.
                "acidgen" => {
                    let track = self.s.project.alloc_track_id();
                    let device = self.s.project.alloc_device_id();
                    let mut t = Track::new(track, TrackKind::Instrument, "Acid", Color::palette(3));
                    t.devices.push(Device::stock(device, aitrack_stock::instrument::acid303::ID, "Acid 303"));
                    let index = self.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(self.s.project.tracks.len());
                    if self.s.edit("Add track", vec![Command::AddTrack { track: Box::new(t), index }]) {
                        self.s.selected_track = Some(track);
                        let mut g = views::device_ui::acid_gen::AcidGen::new(device);
                        g.seed = 0x303;
                        self.acid_gen.insert(device, g);
                        views::device_ui::acid_gen::generate(self, device, cx);
                        self.s.bottom_tab = BottomTab::Devices;
                    }
                }
                "steps" => self.s.bottom_tab = BottomTab::Steps,
                "mixer" => self.mixer_open = true,
                "meters" => self.meters.open = true,
                "tall" => self.bottom_h = 460.,
                "taller" => self.bottom_h = 900.,
                "no-ai" => self.ai_open = false,
                "ai" => self.ai_open = true,
                // Open the palette, optionally on a tab: `palette=devices`.
                "palette" => self.palette_pending = true,
                // The palette's "Similar to <sound>" list for the first sound of the library.
                "similar" => {
                    self.palette_pending = true;
                    self.palette.tab = views::palette::Tab::Samples;
                    let items = self.b.library.search("", None);
                    if let Some(it) = items.iter().find(|i| i.id == 1).or(items.first()) {
                        self.palette.similar = Some(views::palette::SimilarTo { id: it.id, name: it.name.clone() });
                    }
                }
                // The palette's semantic search: "dark punchy kick" ranked by meaning (stub bridges fake the model).
                "semantic" => {
                    self.palette_pending = true;
                    self.palette.tab = views::palette::Tab::Samples;
                    let q = "dark punchy kick";
                    self.palette.query = q.into();
                    self.search.update(cx, |i, cx| i.set_text(q, cx));
                }
                // Preferences › Sounds with the model download half way.
                "semantic-download" => self.modal = Some(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Sounds)),
                // The updater in a state, without the network: `update=available|ready|failed`.
                "update=available" | "update=ready" | "update=failed" => views::updates::seed_dev(self, word),
                // Preferences > MCP server with the server on, a made-up client and calls (nothing is started).
                "mcp" | "prefs=mcp" => views::mcp::seed_dev(self),
                "prefs=updates" => self.modal = Some(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Updates)),
                "prefs=sounds" => {
                    self.modal = Some(views::prefs::Modal::Preferences(
                        views::prefs::PrefsPage::Sounds,
                    ))
                }
                // The Appearance page open, with the token editor expanded (`prefs=tokens`).
                "prefs=appearance" | "prefs=tokens" => {
                    self.modal = Some(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Appearance));
                    self.look.tokens_open = word == "prefs=tokens";
                }
                // The colour palette popover (the track menu) open on the selected track.
                "colors" => {
                    let track = self.s.selected_track.or_else(|| self.s.project.content_tracks().next().map(|t| t.id));
                    if let Some(t) = track {
                        self.s.selected_track = Some(t);
                        self.menu = Some(Menu { kind: MenuKind::TrackContext(t), at: gpui::point(gpui::px(120.), gpui::px(220.)), opened: 0 });
                    }
                }
                // Look settings for a screenshot, shown without saving them: `tint=40` (hue shift in degrees),
                // `saturation=-50`, `bright=20`, `contrast=30`, `gridlines=250` (percent), `clipcolor=150`,
                // `trackcolor=60`, `notes=pitch|velocity|chance|clip`.
                _ if ["tint=", "saturation=", "bright=", "contrast=", "gridlines=", "clipcolor=", "trackcolor=", "notes="].iter().any(|k| word.starts_with(k)) => {
                    if let Some((key, v)) = word.split_once('=') {
                        let n = v.parse::<f32>().unwrap_or(0.);
                        let a = &mut self.look.app;
                        match key {
                            "tint" => a.adjust.hue = n.clamp(-180., 180.),
                            "saturation" => a.adjust.saturation = (n / 100.).clamp(-1., 1.),
                            "bright" => a.adjust.brightness = (n / 100.).clamp(-1., 1.),
                            "contrast" => a.adjust.contrast = (n / 100.).clamp(-1., 1.),
                            "gridlines" => a.adjust.grid = (n / 100.).clamp(0., 3.),
                            "clipcolor" => a.clip_intensity = (n / 100.).clamp(0.2, 2.),
                            "trackcolor" => a.track_tint = (n / 100.).clamp(0., 1.),
                            _ => {
                                a.note_color = match v {
                                    "velocity" => theme::NoteColor::Velocity,
                                    "pitch" => theme::NoteColor::Pitch,
                                    "chance" => theme::NoteColor::Chance,
                                    _ => theme::NoteColor::Clip,
                                }
                            }
                        }
                        self.look.pending = Some(false);
                    }
                }
                "prefs=look" => {
                    self.modal = Some(views::prefs::Modal::Preferences(
                        views::prefs::PrefsPage::Appearance,
                    ))
                }
                "prefs=audio" => {
                    self.modal = Some(views::prefs::Modal::Preferences(
                        views::prefs::PrefsPage::Audio,
                    ))
                }
                "prefs=plugins" => {
                    self.modal = Some(views::prefs::Modal::Preferences(
                        views::prefs::PrefsPage::Plugins,
                    ))
                }
                "prefs=midi" => {
                    self.modal = Some(views::prefs::Modal::Preferences(
                        views::prefs::PrefsPage::Midi,
                    ))
                }
                // The Session View.
                "session" => self.s.view = MainView::Session,
                // A mixer strip's output / input popover for the selected track (`mixer` first).
                "route=output" | "route=input" => {
                    if let Some(t) = self.s.selected_track {
                        let sel = if word == "route=output" { crate::mixer_routing::Selector::Output } else { crate::mixer_routing::Selector::Input };
                        crate::mixer_routing::open(t, sel, gpui::point(gpui::px(420.), gpui::px(330.)));
                    }
                }
                "prefs=performance" => {
                    self.modal = Some(views::prefs::Modal::Preferences(
                        views::prefs::PrefsPage::Performance,
                    ))
                }
                // The first track with clips shown as frozen (header, clips, locked devices).
                "frozen" => self.fake_freeze(),
                // Four locators on the ruler (Intro, Build, Drop, Break), the Drop selected and
                // its menu open, with the loop brace on Build..Drop.
                "locators" => self.fake_locators(),
                // A track bounced to a new track (seeded without rendering: a synthetic file),
                // the new track selected, the source muted.
                "bounced" => self.fake_bounce(),
                "play" => self.b.engine.play(),
                // A MIDI track with three take lanes and a comp made of parts of them.
                "takes" => {
                    if let Some(track) = self.s.seed_demo_takes() {
                        self.s.selected_track = Some(track);
                        self.s.view = MainView::Arrangement;
                    }
                }
                // A demo track: a filter with an LFO and an Envelope Follower after it,
                // the LFO on the cutoff and (by hand, `lfomap`) the follower waiting in Map mode.
                "lfo" | "lfomap" => {
                    use aitrack_model::ModTarget;
                    if let Some(t) = self.s.add_track(TrackKind::Audio) {
                        self.s.selected_track = Some(t);
                        self.s.bottom_tab = BottomTab::Devices;
                        let filter = self.s.add_stock_device(t, "fx.filter", "Filter");
                        let lfo = self.s.add_stock_device(t, "mod.lfo", "LFO");
                        let env = self.s.add_stock_device(t, "mod.envfollow", "Envelope Follower");
                        if let (Some(filter), Some(lfo)) = (filter, lfo) {
                            self.s.map_modulator(lfo, ModTarget::Param { device: filter, param: aitrack_stock::fx::filter::p::CUTOFF }).ok();
                            self.s.map_modulator(lfo, ModTarget::Param { device: filter, param: aitrack_stock::fx::filter::p::RESONANCE }).ok();
                            self.s.set_mod_range(lfo, 0, -0.3, 0.3);
                            self.s.set_mod_range(lfo, 1, 0.0, 0.2);
                            self.s.seal();
                            if word == "lfomap" {
                                views::device_ui::modulator::set_map_mode(env);
                            }
                        }
                    }
                }
                // The Content Hub open on its Plugins tab (`hubplugins`), with a mix of states to look at (`hubplugins=demo`).
                "hubplugins" | "hubplugins=demo" | "hubplugins=confirm" => {
                    self.modal = Some(views::prefs::Modal::ContentHub);
                    let u = views::plugins::ui(cx);
                    u.tab = views::plugins::Tab::Plugins;
                    u.demo = word != "hubplugins";
                    if word.ends_with("confirm") {
                        views::plugins::open_confirm(cx, "tdr-nova");
                    }
                }
                // The Sounds panel open and fitting the song (`sounds`), a first result picked and played (`soundsplay`).
                "sounds" | "soundsplay" => {
                    self.sounds.prefs.open = true;
                    self.sounds.prefs.fit = true;
                    self.sounds.filters.fit = true;
                    if word == "soundsplay" {
                        self.sounds.play_first_pending = true;
                    }
                    // Seeded now, so a window that is not repainting still shows the results.
                    views::sounds::tick(self, cx);
                }
                // The selected track's devices into one rack, with a second chain (`rackgroup`).
                "rackgroup" => {
                    let ids: Vec<DeviceId> = self.s.selected_track.and_then(|t| self.s.project.track(t)).map(|t| t.devices.iter().map(|d| d.id).collect()).unwrap_or_default();
                    self.s.ed.devices = ids;
                    if let Some(rack) = self.s.group_devices() {
                        let n = self.s.rack_of(rack).map_or(0, |r| r.chains.len());
                        self.s.edit("chain", vec![Command::AddChain { rack, index: n, chain: Box::new(Chain::new("Layer 2", Color::palette(7))) }]);
                        self.s.seal();
                    }
                }
                // The first rack of the selected track: Map mode on (`rackmap`), the
                // zones panel open (`rackzones`), 16 macros (`racksixteen`), the
                // first chain's first device mapped to macro 1 over 20–80 % (`rackmapped`).
                "rackmap" | "rackzones" | "racksixteen" | "rackmapped" | "rackreturns" | "rackreturn" | "rackmacros" | "rackselector" => {
                    let rack = self.s.selected_track.and_then(|t| self.s.project.track(t)).and_then(|t| t.devices.iter().find(|d| matches!(d.kind, DeviceKind::Rack(_))).map(|d| d.id));
                    if let Some(rack) = rack {
                        match word {
                            "rackmap" => self.rack_ui.map = Some((rack, 0)),
                            "rackzones" => {
                                self.rack_ui.zones.insert(rack);
                            }
                            "racksixteen" => {
                                self.rack_ui.sixteen.insert(rack);
                            }
                            "rackreturns" => {
                                self.s.add_return(rack);
                                self.s.add_return(rack);
                            }
                            // Return A picked, with an EQ and a reverb in it.
                            "rackreturn" => {
                                self.s.add_return(rack);
                                self.s.add_return(rack);
                                if let Some(track) = self.s.project.device_track(rack) {
                                    let into = Some(aitrack_model::racks::Container::Return { rack, index: 0 });
                                    self.s.add_stock_device_in(track, into, "fx.eq", "EQ");
                                    self.s.add_stock_device_in(track, into, "fx.reverb", "Reverb");
                                }
                                self.rack_ui.ret_sel.insert(rack, 0);
                            }
                            "rackmacros" => {
                                self.fx.macros.insert(rack);
                            }
                            // The zones panel on the Chain tab, selector at 40.
                            "rackselector" => {
                                self.rack_ui.zones.insert(rack);
                                self.rack_ui.zone_tab.insert(rack, views::devices::rack_ui::ZoneTab::Chain);
                                self.s.set_rack_selector(rack, 40);
                                self.s.seal();
                            }
                            _ => {
                                let device = self.s.rack_of(rack).and_then(|r| r.chains.first()?.devices.first().map(|d| d.id));
                                if let Some(device) = device
                                    && self.s.map_macro_param(rack, 0, device, 0, 0.5).is_some()
                                {
                                    self.s.set_mapping_range(rack, 0, 0, 0.2, 0.8);
                                    self.s.seal();
                                }
                            }
                        }
                    }
                }
                // Select bars 3..5 of the first automation lane shown, with the marker there.
                "autorange" => {
                    let lane = self.s.project.tracks.iter().find(|t| self.s.automation_shown.contains(&t.id)).and_then(|t| Some((t.id, t.automation.first()?.target)));
                    if let Some((track, target)) = lane {
                        self.s.set_auto_range(track, target, 8 * PPQ, 16 * PPQ);
                        self.s.arrange.px_per_beat = 24.;
                    }
                }
                // A sine over that range, one cycle per grid step (after `autorange`).
                "autoshape" => {
                    let lane = self.s.project.tracks.iter().find(|t| self.s.automation_shown.contains(&t.id)).and_then(|t| Some((t.id, t.automation.first()?.target)));
                    if let Some((track, target)) = lane {
                        self.s.set_auto_range(track, target, 8 * PPQ, 24 * PPQ);
                        self.s.arrange.px_per_beat = 24.;
                        self.s.insert_auto_shape(aitrack_model::automation::Shape::Sine);
                        self.s.seal();
                    }
                }
                // The first lane's range selected with its transform handles, after scaling it down a little.
                "autoscale" => {
                    let lane = self.s.project.tracks.iter().find(|t| self.s.automation_shown.contains(&t.id)).and_then(|t| Some((t.id, t.automation.first()?.target)));
                    if let Some((track, target)) = lane {
                        self.s.set_auto_range(track, target, 8 * PPQ, 24 * PPQ);
                        self.s.arrange.px_per_beat = 24.;
                        if let Some(r) = self.s.auto_sel {
                            let original = self.s.project.track(r.track).and_then(|t| t.automation.iter().find(|l| l.target == r.target)).map(|l| l.points.clone()).unwrap_or_default();
                            self.s.xform_auto_range(r, &original, crate::state::AutoXform::Scale { anchor: 0., factor: 0.7 });
                            self.s.seal();
                        }
                    }
                }
                // Raise that range by a quarter (after `autorange`).
                "autoraise" => {
                    if let Some(r) = self.s.auto_sel {
                        let original = self.s.project.track(r.track).and_then(|t| t.automation.iter().find(|l| l.target == r.target)).map(|l| l.points.clone()).unwrap_or_default();
                        self.s.shift_auto_range(r, &original, 0.25);
                        self.s.seal();
                    }
                }
                // Put the selected track (`track=Name` first) in a new group.
                "group" => {
                    self.s.group_tracks();
                }
                // Open the first audio clip in the Clip view.
                "audio" => {
                    let p = &self.s.project;
                    let found = p.tracks.iter().find_map(|t| {
                        let pl = t.arrangement.iter().find(|pl| {
                            p.clips.get(&pl.clip).is_some_and(|c| {
                                matches!(p.sources.get(&c.source), Some(Source::Audio(_)))
                            })
                        })?;
                        Some((t.id, pl.id, pl.clip))
                    });
                    if let Some((track, placement, clip)) = found {
                        self.s.selected_track = Some(track);
                        self.s.selected_clip = Some(clip);
                        self.s.selected_placement = Some((track, placement));
                        self.s.bottom_tab = BottomTab::PianoRoll;
                    }
                }
                // The Clip view with envelope `clipenv=pan` / `clipenv=pitch` / `clipenv=volume` selected and
                // a few points drawn on the selected audio clip (`wav=…,audio` first).
                _ if word.starts_with("clipenv=") => {
                    let kind = match &word["clipenv=".len()..] {
                        "pan" => ClipEnv::Pan,
                        "pitch" | "transpose" => ClipEnv::Pitch,
                        _ => ClipEnv::Volume,
                    };
                    self.sample.env = kind;
                    if let Some(clip) = self.s.selected_clip
                        && let Some(length) = self.s.project.clips.get(&clip).map(|c| c.length)
                    {
                        // Fractions of the clip and where they sit in the kind's range.
                        let shape: &[(f64, f32)] = match kind {
                            ClipEnv::Volume => &[(0., 1.), (0.25, 0.4), (0.6, 0.9), (1., 0.2)],
                            ClipEnv::Pan => &[(0., 0.5), (0.3, 0.1), (0.6, 0.9), (1., 0.5)],
                            ClipEnv::Pitch => &[(0., 0.5), (0.25, 0.5), (0.5, 0.75), (0.75, 0.375), (1., 0.5)],
                        };
                        let points = shape.iter().map(|&(at, unit)| AutomationPoint { tick: (length as f64 * at).round() as Tick, value: kind.from_unit(unit), curve: 0. }).collect();
                        if let Some(cmd) = aitrack_model::ops::set_clip_envelope(&self.s.project, clip, kind, points) {
                            self.s.edit("Draw clip envelope", vec![cmd]);
                            self.s.seal();
                        }
                    }
                }
                // Reverse the selected clip's sample; `transpose=-7` / `detune=25` tune it.
                _ if word == "reverse"
                    || word.starts_with("transpose=")
                    || word.starts_with("detune=") =>
                {
                    let v: f32 = word.split('=').nth(1).and_then(|v| v.parse().ok()).unwrap_or(0.);
                    if let Some(clip) = self.s.selected_clip {
                        self.s.set_clip_warp("dev", clip, |w| match word.split('=').next() {
                            Some("transpose") => w.transpose = v,
                            Some("detune") => w.detune = v,
                            _ => w.reverse = true,
                        });
                    }
                }
                // Stretch the selected clip (`audio` first) to `stretch=2` times its length.
                _ if word == "stretch" || word.starts_with("stretch=") => {
                    let k: f64 = word.split('=').nth(1).and_then(|v| v.parse().ok()).unwrap_or(2.);
                    if let Some((_, placement)) = self.s.selected_placement
                        && let Some(st) = views::arrange_clips::edit::Stretch::take(&self.s, placement, false)
                    {
                        let length = (st.origin.clip.length as f64 * k).round() as Tick;
                        views::arrange_clips::edit::stretch(&mut self.s, &st, length);
                        self.s.seal();
                    }
                }
                // An audio track with the file `wav=/path/to.wav` at bar 2, second from the top (before `audio`).
                _ if word.starts_with("wav=") => {
                    let path = std::path::PathBuf::from(&word["wav=".len()..]);
                    let info = hound::WavReader::open(&path).map(|r| (r.spec().sample_rate, r.spec().channels, r.duration() as u64)).unwrap_or((48_000, 2, 0));
                    let cmds = audio_track_commands(&mut self.s, "Sample", Color::palette(5), &path, info, 4 * PPQ, 1);
                    self.s.edit("Add sample", cmds);
                    self.s.seal();
                }
                // Auto-warp the selected audio clip (`wav=…,audio` first): the beats are found in the background.
                "autowarp" => {
                    if let Some(clip) = self.s.selected_clip {
                        self.auto_warp_clip(clip, cx);
                    }
                }
                // Warp markers on the selected audio clip (`wav=…,audio` first): its ends pinned and its
                // middle dragged a sixteenth late, so the editor shows handles and a bent waveform.
                "markers" => {
                    if let Some(clip) = self.s.selected_clip
                        && let Some(Source::Audio(a)) = self.s.project.clips.get(&clip).and_then(|c| self.s.project.sources.get(&c.source)).cloned()
                    {
                        let mut warp = a.warp.clone();
                        warp.enabled = true;
                        warp.original_bpm = warp.original_bpm.or(Some(self.s.project.tempo.initial_bpm()));
                        warp.markers.clear();
                        let rate = f64::from(a.sample_rate.max(1));
                        let middle = warp.tick_at(a.frames as f64 / 2., rate).unwrap_or(0.).round() as Tick;
                        if let Some((pinned, index)) = views::sample_editor::warp::added(&warp, middle, rate, a.frames) {
                            let bent = views::sample_editor::warp::moved(&pinned, index, middle + PPQ / 4, rate).unwrap_or(pinned);
                            views::sample_editor::warp::apply(&mut self.s, clip, &bent);
                            self.s.seal();
                        }
                    }
                }
                // A copy of the selected clip right after it, on the same sample.
                "twin" => {
                    if let Some((track, placement)) = self.s.selected_placement
                        && let Some((_, pl, length)) = self.s.placement(placement)
                        && let Some((cmds, _)) = aitrack_model::ops::duplicate_placement(&mut self.s.project, track, placement, pl.start + length)
                    {
                        self.s.edit("Duplicate", cmds);
                        self.s.seal();
                    }
                }
                // The Slice to Drum Rack dialog on the selected audio clip (`audio` first);
                // `slice=beats` has it on the beat division, `sliced` slices by 1/8 notes.
                "slice" | "slice=beats" | "sliced" => {
                    use crate::editing::slice as sl;
                    if word == "slice=beats" {
                        sl::set_choice(sl::SliceChoice { cut: sl::Cut::Beats, ..sl::choice() });
                    }
                    if let Some((track, placement)) = sl::target(&self.s) {
                        if word == "sliced" {
                            sl::set_choice(sl::SliceChoice { cut: sl::Cut::Beats, division: aitrack_model::slice::BeatDivision::Eighth, ..sl::choice() });
                            if let Ok(done) = self.s.slice_to_drum_rack(track, placement, sl::choice().mode()) {
                                self.s.bottom_tab = BottomTab::Devices;
                                self.s.selected_track = Some(done.track);
                            }
                        } else {
                            self.menu = Some(Menu { kind: MenuKind::SliceToDrumRack(track, placement), at: gpui::point(gpui::px(420.), gpui::px(200.)), opened: 0 });
                        }
                    }
                }
                // A MIDI Effect Rack with two chains (an arpeggiator, a chord) on the selected track.
                "midirack" => {
                    if let Some(t) = self.s.selected_track
                        && let Some(rack) = self.s.add_stock_device(t, "rack.midi", "MIDI Effect Rack")
                    {
                        let into = |chain| Some(aitrack_model::racks::Container::Rack { rack, chain });
                        self.s.edit("chain", vec![Command::AddChain { rack, index: 0, chain: Box::new(Chain::new("Arp", Color::palette(2))) }, Command::AddChain { rack, index: 1, chain: Box::new(Chain::new("Chords", Color::palette(5))) }]);
                        let arp = self.s.add_stock_device_in(t, into(0), "midi.arp", "Arpeggiator");
                        self.s.add_stock_device_in(t, into(1), "midi.chord", "Chord");
                        self.s.add_stock_device_in(t, into(1), "midi.velocity", "Velocity");
                        self.s.seal();
                        if let Some(arp) = arp {
                            self.s.map_macro_param(rack, 0, arp, 0, 0.5);
                            self.s.seal();
                        }
                        self.s.bottom_tab = BottomTab::Devices;
                    }
                }
                // The right-click menu of the selected clip.
                "menu=clip" => {
                    self.menu = Some(Menu { kind: MenuKind::ClipContext, at: gpui::point(gpui::px(420.), gpui::px(200.)), opened: 0 });
                }
                // Group the selected track's whole chain into a rack.
                "rack" => {
                    let ids: Vec<_> = self
                        .s
                        .selected_track
                        .and_then(|t| self.s.project.track(t))
                        .map(|t| t.devices.iter().map(|d| d.id).collect())
                        .unwrap_or_default();
                    for id in ids {
                        self.s.click_device(id, true, false);
                    }
                    self.s
                        .ed
                        .devices
                        .retain(|d| self.s.project.device(*d).is_some());
                    self.s.group_devices();
                }
                // Open the right-click menu of the selected track / of the picked device.
                // Automation lanes on the selected track (`track=Name` first):
                // a drawn Volume lane and an empty Pan lane under it.
                "lanes" => {
                    if let Some(t) = self.s.selected_track {
                        self.s.show_automation(t, AutomationTarget::Volume);
                        for (tick, v) in [(0, 0.92), (4 * PPQ, 0.92), (8 * PPQ, 0.4), (12 * PPQ, 0.8), (16 * PPQ, 0.6)] {
                            self.s.set_automation_point(t, AutomationTarget::Volume, tick, v, false);
                        }
                        self.s.add_auto_lane(t);
                    }
                }
                // A lane for the first parameter of the selected track's first device.
                "devlane" => {
                    if let Some(t) = self.s.selected_track
                        && let Some(d) = self.s.project.track(t).and_then(|t| t.devices.first()).cloned()
                        && let Some((param, _)) = views::arrangement::device_params(self, &d).first().cloned()
                    {
                        self.s.show_automation(t, AutomationTarget::DeviceParam { device: d.id, param });
                    }
                }
                // The selected track made tall enough for the header's volume slider.
                "talltrack" => {
                    if let Some(t) = self.s.selected_track {
                        self.s.set_track(t, TrackProp::Height(88.));
                    }
                }
                // The chooser of the selected track's first lane, open (`lanes` first).
                "chooser" => {
                    if let Some(t) = self.s.selected_track {
                        self.auto_chooser = Some(views::arrangement::Chooser { track: t, index: 0, at: gpui::point(gpui::px(80.), gpui::px(240.)) });
                    }
                }
                // Search the open chooser: `autosearch=gain`.
                _ if word.starts_with("autosearch=") => {
                    self.auto_search_pending = word.split('=').nth(1).map(str::to_owned);
                }
                // The "Show automation" menu of the selected track's fader.
                "parammenu" => {
                    if let Some(t) = self.s.selected_track {
                        self.param_menu = Some((t, AutomationTarget::Volume, gpui::point(gpui::px(420.), gpui::px(240.))));
                    }
                }
                "autoarm" => self.s.automation_arm = true,
                // The Launch / Quantize dropdowns of the selected clip.
                "menu=launch" | "menu=quantize" => {
                    if let Some(c) = self.s.selected_clip {
                        let kind = if word == "menu=launch" { MenuKind::ClipLaunch(c) } else { MenuKind::ClipQuantize(c) };
                        self.menu = Some(Menu { kind, at: gpui::point(gpui::px(90.), gpui::px(560.)), opened: 0 });
                    }
                }
                // The File menu, with its templates submenu out for `menu=templates`.
                "menu=file" | "menu=templates" => {
                    self.menu = Some(Menu { kind: MenuKind::File, at: gpui::point(gpui::px(100.), gpui::px(36.)), opened: 0 });
                    self.file_templates_open = word == "menu=templates";
                }
                "menu=track" => {
                    if let Some(t) = self.s.selected_track {
                        self.menu = Some(Menu {
                            kind: MenuKind::TrackContext(t),
                            at: gpui::point(gpui::px(120.), gpui::px(220.)),
                            opened: 0,
                        });
                    }
                }
                "menu=device" => {
                    if let Some(d) = self.s.selected_devices().first() {
                        self.menu = Some(Menu {
                            kind: MenuKind::DeviceContext(*d),
                            at: gpui::point(gpui::px(320.), gpui::px(420.)),
                            opened: 0,
                        });
                    }
                }
                // Open the wavetable synth of the selected track (else the first
                // one) in its window: `synthwin`, with `synthtab=matrix|fx|table`
                // and `synthmod=lfo1` picking what it shows.
                "synthwin" => {
                    let id = {
                        let p = &self.s.project;
                        let is_synth = |d: &&Device| matches!(&d.kind, aitrack_model::DeviceKind::Stock { plugin_id } if plugin_id == aitrack_stock::instrument::wavetable::ID);
                        self.s.selected_track.and_then(|t| p.track(t)).and_then(|t| t.devices.iter().find(is_synth).map(|d| d.id)).or_else(|| p.tracks.iter().flat_map(|t| &t.devices).find(is_synth).map(|d| d.id))
                    };
                    if let Some(id) = id {
                        if let Some(t) = self.s.project.device_track(id) {
                            self.s.selected_track = Some(t);
                        }
                        self.window_pending = Some(id);
                    }
                }
                // Route LFO 1 → Osc A position and Env 2 → cutoff, and draw
                // LFO 1 (shows rings and a drawn curve in the synth window).
                "synthroute" => {
                    if let Some(id) = self.window_pending {
                        use aitrack_stock::instrument::wavetable::{modulation::LfoCurve, p, src};
                        use views::device_window::synth::{commit_curve, knob};
                        for (s, d) in [(src::LFO1, p::osc(0, p::POS)), (src::ENV2, p::CUTOFF), (src::MACRO1, p::osc(0, p::WARP_AMT))] {
                            let slots = knob::read_slots(self, id);
                            if let Some(info) = self.fx.param_info(self.b.plugins.as_ref(), &self.s.project, id, d)
                                && let Some((_, edits)) = views::device_window::synth::geom::route_drop(&slots, s, &info)
                            {
                                knob::set_plain(self, "dev", id, &edits);
                            }
                        }
                        commit_curve(self, id, 0, LfoCurve::pluck(), false);
                        // Osc A on a multi-frame factory table, osc B on another.
                        knob::set_plain(self, "dev", id, &[(p::osc(0, p::WAVE), 2.), (p::osc(0, p::POS), 35.), (p::osc(1, p::WAVE), 4.), (p::osc(1, p::POS), 60.)]);
                    }
                }
                // `set=3:2.5` sets parameter 3 of the selected track's last device (plain value).
                _ if word.starts_with("set=") => {
                    let spec = &word["set=".len()..];
                    let device = self.s.selected_track.and_then(|t| self.s.project.track(t)).and_then(|t| t.devices.last()).map(|d| d.id);
                    if let (Some(device), Some((id, v))) = (device, spec.split_once(':'))
                        && let (Ok(id), Ok(v)) = (id.parse::<u32>(), v.parse::<f32>())
                        && let Some(info) = self.fx.param_info(self.b.plugins.as_ref(), &self.s.project, device, id)
                    {
                        views::device_ui::set_now(self, device, id, info.to_normalized(v));
                    }
                }
                // Open the selected track's last stock device in its window.
                "devwin" => {
                    let id = self.s.selected_track.and_then(|t| self.s.project.track(t)).and_then(|t| t.devices.iter().rev().find(|d| views::device_window::can_open(&d.kind)).map(|d| d.id));
                    self.window_pending = id;
                }
                _ if word.starts_with("synthtab=") || word.starts_with("synthmod=") => {
                    if let Some(id) = self.window_pending {
                        use views::device_window::synth::{ModTab, Tab};
                        let v = self.fx.synth.entry(id).or_default();
                        match word {
                            "synthtab=matrix" => v.tab = Tab::Matrix,
                            "synthtab=fx" => v.tab = Tab::Fx,
                            "synthtab=table" => v.tab = Tab::Table,
                            "synthtab=harmonics" => {
                                v.tab = Tab::Table;
                                v.tool = views::device_window::synth::Tool::Harmonics;
                            }
                            "synthtab=formula" => {
                                v.tab = Tab::Table;
                                v.tool = views::device_window::synth::Tool::Formula;
                            }
                            _ => {
                                let n: u32 = word.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(1);
                                v.mods = if word.contains("lfo") { ModTab::Lfo(n.clamp(1, 4) - 1) } else { ModTab::Env(n.clamp(1, 3) - 1) };
                            }
                        }
                    }
                }
                // The Vault of the selected track (else the first one) in its
                // window; `vaultpreset=Name` loads a factory preset onto it
                // first and `vaultfilter=keys` / `vaultfilter=warm` filters
                // its browser by a type or a character tag.
                "vaultwin" => self.window_pending = self.dev_vault(),
                // A Bloom on a new track with a seeded scene; `bloomwin` opens its
                // window, `bloomplay` lights cells with synthetic activity.
                _ if word == "bloom" || word == "bloomplay" || word.starts_with("bloomkind=") => {
                    let _ = views::device_ui::bloom::dev_word(self, word);
                }
                "bloomwin" => self.window_pending = views::device_ui::bloom::dev_word(self, word),
                _ if word.starts_with("vaultpreset=") || word.starts_with("vaultfilter=") => {
                    let Some(id) = self.dev_vault() else { continue };
                    let value = word.split_once('=').map_or("", |(_, v)| v);
                    if word.starts_with("vaultpreset=") {
                        let list = self.fx.vault.presets();
                        if let Some(pr) = list.iter().find(|p| p.name.eq_ignore_ascii_case(value))
                            && let Ok(preset) = aitrack_stock::presets::load(&pr.path)
                        {
                            let params = preset.params.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.clamp(0., 1.)))).collect();
                            self.s.load_preset(id, &preset.name, &params);
                        }
                    } else {
                        let f = &mut self.fx.vault.view(id).filter;
                        match aitrack_stock::instrument::vault::TYPES.iter().position(|t| t.eq_ignore_ascii_case(value)) {
                            Some(k) => f.kind = Some(k),
                            None => {
                                f.tags.insert(value.to_lowercase());
                            }
                        }
                    }
                }
                "big" => {
                    let ids: Vec<_> = self.s.project.tracks.iter().flat_map(|t| t.devices.iter().map(|d| d.id)).collect();
                    self.fx.big.extend(ids);
                }
                // `devwins=instrument.granular+fx.stutter`: those stock devices appended to the
                // selected track, each open in its own window (frame-time runs, `AITRACK_FPS`).
                _ if word.starts_with("devwins=") => {
                    if let Some(track) = self.s.selected_track {
                        for plugin in word["devwins=".len()..].split('+').filter(|s| !s.is_empty()) {
                            let id = self.s.project.alloc_device_id();
                            let index = self.s.project.track(track).map_or(0, |t| t.devices.len());
                            let name = self.b.plugins.device(plugin).map_or(plugin.to_owned(), |d| d.name.to_owned());
                            let device = Device::stock(id, plugin, name);
                            self.s.edit("Add device", vec![Command::AddDevice { slot: aitrack_model::command::DeviceSlot::Track { track, index }, device: Box::new(device) }]);
                            self.windows_pending.push(id);
                        }
                    }
                }
                // The arrangement at `zoom=<pixels per beat>`.
                _ if word.starts_with("zoom=") => {
                    if let Ok(px) = word["zoom=".len()..].parse::<f32>() {
                        self.s.arrange.px_per_beat = px.clamp(2., 400.);
                    }
                }
                _ if word.starts_with("palette=") => {
                    self.palette_pending = true;
                    // `palette=vst3` / `palette=clap`: the Devices tab narrowed to that format.
                    if let Some(format) = match &word["palette=".len()..] {
                        "vst3" => Some(views::palette::devices::FormatFilter::Vst3),
                        "clap" => Some(views::palette::devices::FormatFilter::Clap),
                        _ => None,
                    } {
                        self.palette.device_format = format;
                    }
                    self.palette.tab = match &word["palette=".len()..] {
                        "vst3" | "clap" => views::palette::Tab::Devices,
                        "samples" => views::palette::Tab::Samples,
                        "key" => views::palette::Tab::InKey,
                        "splice" => views::palette::Tab::Splice,
                        "devices" => views::palette::Tab::Devices,
                        "presets" => views::palette::Tab::Presets,
                        "commands" => views::palette::Tab::Commands,
                        "packs" => views::palette::Tab::Packs,
                        _ => views::palette::Tab::All,
                    };
                }
                _ => {
                    // `rack=audio|instrument|midi|drum` adds a new rack to the selected track
                    // (`device=rack.audio` would make a stock device that is not a rack).
                    if let Some(kind) = word.strip_prefix("rack=")
                        && let Some(track) = self.s.selected_track
                    {
                        let (plugin, name) = match kind {
                            "instrument" => ("rack.instrument", "Instrument Rack"),
                            "midi" => ("rack.midi", "MIDI Effect Rack"),
                            "drum" => ("rack.drum", "Drum Rack"),
                            _ => ("rack.audio", "Audio Effect Rack"),
                        };
                        self.s.add_stock_device(track, plugin, name);
                    }
                    // `rackpreset=Parallel Compression` adds that saved or factory rack to the selected track.
                    if let Some(name) = word.strip_prefix("rackpreset=")
                        && let Some(track) = self.s.selected_track
                        && let Some(entry) = crate::rack_library::list(&crate::rack_library::root()).into_iter().find(|e| e.name.eq_ignore_ascii_case(name))
                    {
                        views::devices::rack_presets::add(self, &entry.source, track, cx);
                    }
                    // `racksave` opens the Save popover of the selected track's first rack.
                    if word == "racksave" {
                        let rack = self.s.selected_track.and_then(|t| self.s.project.track(t)).and_then(|t| t.devices.iter().find(|d| matches!(d.kind, DeviceKind::Rack(_))).map(|d| d.id));
                        if let Some(rack) = rack {
                            views::devices::rack_presets::open_save(self, rack, gpui::point(px(480.), px(300.)), None, cx);
                        }
                    }
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
        // Each audio track's input and monitor mode, where the project changed them.
        self.s.sync_track_io(&mut self.track_io_sent, &mut *self.b.engine);
        match self.s.sync_engine(&mut *self.b.engine, &mut self.synced_revision) {
            crate::state::Synced::Fast => self.fast_at = Some(std::time::Instant::now()),
            crate::state::Synced::Full => self.fast_at = None,
            crate::state::Synced::Nothing => {}
        }
    }

    /// Timer tick: plugin main-thread work, scan progress, engine messages.
    fn housekeeping(&mut self, cx: &mut Context<Self>) {
        self.autosave_tick();
        self.updater_tick(cx);
        self.mcp_tick(cx);
        // A take being played, or one that just ended: clips from the notes.
        let rec = self.s.poll_recording(&mut *self.b.engine);
        let clips = rec.committed + rec.audio_clips;
        if clips > 0 {
            self.toast(if clips == 1 { "Recorded a clip".to_owned() } else { format!("Recorded {clips} clips") }, cx);
        }
        for e in &rec.errors {
            self.toast(e.clone(), cx);
        }
        // Where audio takes are written follows the project's file.
        let dir = crate::state::recordings_dir(self.project_path.as_deref());
        if self.rec_dir.as_ref() != Some(&dir) {
            let _ = std::fs::create_dir_all(&dir);
            self.b.engine.set_recording_dir(&dir);
            self.rec_dir = Some(dir);
        }
        if rec.changed {
            cx.notify();
        }
        if self.fast_at.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) {
            self.fast_at = None;
            self.s.settle_engine(&mut *self.b.engine);
        }
        self.audition.retry(|t, k, v| self.b.engine.audition_note(t, k, v));
        let mut changed = self.b.engine.idle();
        // A jump to a locator waiting for the launch quantisation.
        changed |= self.poll_jump();
        // Knobs turned in a plugin's own window become undoable SetParam edits.
        let edits = self.b.engine.take_plugin_edits();
        // While recording with Automation Arm on, they are written into their lanes too.
        let t = self.b.engine.transport();
        // Capture Automation / MIDI: the last minutes of moves and played notes, while rolling without recording.
        self.s.capture.set_clock((t.playing && !t.recording).then_some(t.position));
        let now = Instant::now();
        let played = self.b.engine.take_captured_notes();
        if !played.is_empty() {
            self.s.capture.push_notes(now, played);
        }
        self.s.capture.prune(now);
        changed |= self.s.apply_plugin_edits(&edits, (t.recording && t.playing).then_some(t.position));
        // The usage read-out moves about once a second (3s in low-power mode) even when nothing else does.
        let (cpu, mem) = (self.usage.cpu, self.usage.memory);
        self.usage.tick(self.look.app.low_power);
        changed |= (self.usage.cpu - cpu).abs() >= 0.005 || self.usage.memory.abs_diff(mem) >= 4 * 1024 * 1024;
        changed |= self.b.plugins.poll();
        let lib_changed = self.b.library.poll();
        changed |= lib_changed;
        if lib_changed {
            self.sounds.dirty = true;
        }
        // Plugins the Plugins tab just installed: the scan skips what it already knows, so only they are read.
        if !self.b.plugins.scan_status().running && !self.b.library.take_installed_plugins().is_empty() {
            self.b.plugins.rescan();
        }
        changed |= views::sounds::tick(self, cx);
        changed |= self.waves.poll();
        changed |= views::palette::poll(self, cx);
        // A shown Bloom stage breathes at about 12 frames a second while idle.
        changed |= self.fx.bloom.wants_tick();
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
        self.file_recent_open = false;
        self.file_templates_open = false;
        self.device_menu_hover = None;
        if self.menu.as_ref().is_some_and(|m| m.kind == kind) {
            self.menu = None;
        } else {
            if matches!(kind, MenuKind::AddDevice(_)) {
                // Opened any other way than a rack's "+ Device": the track's chain.
                self.rack_ui.add_into = None;
                self.device_menu_search.update(cx, |i, cx| i.set_text("", cx));
            }
            self.menu_counter += 1;
            self.menu = Some(Menu { kind, at, opened: self.menu_counter });
        }
        cx.notify();
    }

    /// Right-click on a mixer control: a menu to show its automation lane.
    pub fn open_param_menu(&mut self, track: TrackId, target: AutomationTarget, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = None;
        self.auto_chooser = None;
        self.menu_counter += 1;
        self.param_menu = Some((track, target, at));
        cx.notify();
    }

    /// Right-click on any knob (`knob_control`, device UIs, mixer): resolve its
    /// [`AutomationTarget`] and open the automation context menu.
    pub fn open_knob_menu(&mut self, target: KnobTarget, at: Point<Pixels>, cx: &mut Context<Self>) {
        match target {
            KnobTarget::Param { device, param, .. } => {
                if let Some(track) = self.s.project.device_track(device) {
                    self.open_param_menu(track, AutomationTarget::DeviceParam { device, param }, at, cx);
                }
            }
            KnobTarget::Macro { rack, index } => {
                if let Some(track) = self.s.project.device_track(rack) {
                    self.open_param_menu(track, AutomationTarget::Macro { device: rack, index: index as u8 }, at, cx);
                }
            }
            KnobTarget::Pan { track } => {
                self.open_param_menu(track, AutomationTarget::Pan, at, cx);
            }
            KnobTarget::Send { track, index } => {
                self.open_param_menu(track, AutomationTarget::Send { index: index as u16 }, at, cx);
            }
            KnobTarget::Transpose { .. } | KnobTarget::Detune { .. } | KnobTarget::Chain { .. } | KnobTarget::Return { .. } => {}
        }
    }

    /// Show `track`'s automation lane for `target` in the arrangement (the
    /// mixer's and the device view's "Show automation"): switches to the
    /// Arrangement and scrolls the lane into view. The state part is
    /// [`AppState::show_automation`].
    pub fn show_automation(&mut self, track: TrackId, target: AutomationTarget, cx: &mut Context<Self>) {
        self.s.show_automation(track, target);
        self.param_menu = None;
        self.set_view(MainView::Arrangement, cx);
        let rows = views::arrangement::rows(&self.s);
        if let Some(r) = rows.iter().find(|r| r.track == track)
            && let Some(k) = r.autos.iter().position(|t| *t == target)
        {
            let view_h = f32::from(self.regions.arrange.get().size.height).max(200.);
            let (top, bottom) = (r.top, r.top + r.auto_top() + (k + 1) as f32 * views::arrangement::auto_h());
            let v = &mut self.s.arrange;
            if top < v.scroll_y {
                v.scroll_y = top;
            } else if bottom > v.scroll_y + view_h {
                v.scroll_y = (bottom - view_h).max(0.);
            }
        }
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.file_recent_open = false;
        self.file_templates_open = false;
        self.device_menu_hover = None;
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    // ── Actions ──

    /// Space. How it, Shift+Space and clicks in the lanes and the ruler
    /// work together (Live's insert marker) is set out at the top of
    /// `editing::gestures`.
    pub fn play_stop(&mut self, _: &PlayStop, window: &mut Window, cx: &mut Context<Self>) {
        // Space belongs to the Sounds panel while a preview plays or its list has the keys.
        if views::sounds::space(self, window, cx) {
            return;
        }
        crate::editing::gestures::play_stop(&self.s, self.b.engine.as_mut());
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.b.engine.stop();
        cx.notify();
    }

    /// Ctrl+E and Split in the clip menu: cut at the insert marker, or at
    /// the playhead while playing.
    pub fn split_clips(&mut self, cx: &mut Context<Self>) {
        let t = self.b.engine.transport();
        match self.s.split(t.playing.then_some(t.position)) {
            0 if t.playing => self.toast("Nothing to split at the playhead", cx),
            0 => self.toast("Nothing to split at the marker", cx),
            _ => cx.notify(),
        }
    }

    /// A (Live): show or hide the automation of the selected tracks (all
    /// shown when any is hidden). While musical typing is on, A is a note.
    fn toggle_automation_mode(&mut self, _: &ToggleAutomationMode, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_typing_notes() {
            cx.propagate();
            return;
        }
        let tracks: Vec<TrackId> = self.s.project.content_tracks().map(|t| t.id).filter(|t| self.s.is_track_selected(*t)).collect();
        let show = tracks.iter().any(|t| !self.s.automation_shown.contains(t));
        for t in tracks {
            if self.s.automation_shown.contains(&t) != show {
                self.s.toggle_automation(t);
            }
        }
        cx.notify();
    }

    fn toggle_automation_arm(&mut self, _: &ToggleAutomationArm, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_arm(cx);
    }

    /// Delete the selected tracks (the Main track stays); undo brings them back.
    pub fn delete_selected_tracks(&mut self, cx: &mut Context<Self>) {
        let ids = self.s.selected_tracks();
        let n = ids.iter().filter(|t| self.s.project.track(**t).is_some_and(|t| t.kind != TrackKind::Main)).count();
        if self.s.delete_tracks(&ids) {
            self.toast(if n == 1 { "Track deleted".to_owned() } else { format!("{n} tracks deleted") }, cx);
        } else {
            self.toast("The Main track can't be deleted", cx);
        }
        cx.notify();
    }

    /// Automation Arm on or off (the arrangement toolbar's button).
    pub fn toggle_arm(&mut self, cx: &mut Context<Self>) {
        self.s.automation_arm = !self.s.automation_arm;
        // Recording only exists to write automation: without the arm, the
        // lit Record button would record nothing.
        if recording_needs_stop(self.b.engine.transport().recording, self.s.automation_arm) {
            self.b.engine.toggle_record();
        }
        self.toast(if self.s.automation_arm { "Automation Arm on: moving a plugin control while recording writes its automation" } else { "Automation Arm off" }, cx);
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

    pub(crate) fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let label = self.s.history.undo_label().map(str::to_owned);
        if self.s.undo() {
            self.toast(format!("Undo {}", label.unwrap_or_default()), cx);
        }
    }

    pub(crate) fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let label = self.s.history.redo_label().map(str::to_owned);
        if self.s.redo() {
            self.toast(format!("Redo {}", label.unwrap_or_default()), cx);
        }
    }

    fn delete(&mut self, _: &DeleteSelection, _: &mut Window, cx: &mut Context<Self>) {
        // After a click in the Device View, Delete removes the picked devices.
        let devices = self.s.selected_devices();
        if self.s.ed.area == crate::editing::Area::Devices && !devices.is_empty() {
            if self.s.remove_devices(devices) {
                cx.notify();
            }
            return;
        }
        // In the Session View, Delete empties the slot under the cursor.
        if self.s.view == MainView::Session {
            if let (Some(track), Some(scene)) = (self.s.selected_track, self.s.session_scene())
                && self.s.clear_slot(track, scene)
            {
                cx.notify();
            }
            return;
        }
        // A locator picked on the ruler goes first (clicking in the lanes drops it).
        if let Some(id) = self.s.ed.locator
            && self.s.remove_locator(id)
        {
            cx.notify();
            return;
        }
        if self.s.delete_selection() {
            cx.notify();
        }
    }

    fn toggle_draw(&mut self, _: &ToggleDrawTool, _: &mut Window, cx: &mut Context<Self>) {
        self.s.tool = if self.s.tool == Tool::Draw { Tool::Select } else { Tool::Draw };
        self.toast(if self.s.tool == Tool::Draw { "Draw tool" } else { "Select tool" }, cx);
    }

    fn toggle_loop(&mut self, _: &ToggleLoop, _: &mut Window, cx: &mut Context<Self>) {
        // With a selection the loop brace goes around it and the loop switches on;
        // pressed again, or with nothing selected, the loop switches on or off.
        let moved = self.s.selection_span().is_some_and(|span| self.s.project.loop_region != Some(span)) && self.s.loop_selection();
        let on = moved || !self.b.engine.transport().looping;
        self.b.engine.set_looping(on);
        cx.notify();
    }

    fn toggle_sounds(&mut self, _: &ToggleSounds, _: &mut Window, cx: &mut Context<Self>) {
        views::sounds::toggle(self, cx);
    }

    fn toggle_ai(&mut self, _: &ToggleAssistant, _: &mut Window, cx: &mut Context<Self>) {
        self.ai_open = !self.ai_open;
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.s.select_all_clips() {
            cx.notify();
            return;
        }
        if let Some((_, n)) = self.s.selected_clip.and_then(|c| self.s.clip_notes(c)) {
            self.s.selected_notes = n.notes.iter().map(|n| n.id).collect();
            cx.notify();
        }
    }

    pub fn quantize_clip(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.s.selected_clip {
            let n = self.s.quantize(c, 1.);
            self.toast(if n == 0 { "Already on the grid".into() } else { format!("Quantized {n} notes to {}", self.s.snap_label(crate::editing::GridView::Piano)) }, cx);
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

    /// Whether Computer MIDI Keyboard mode is actively intercepting letter keys to play notes.
    pub fn is_typing_notes(&self) -> bool {
        self.piano_keyboard && self.modal.is_none() && self.num_edit.is_none() && !self.palette.open
    }

    /// Instrument tracks that currently receive live notes from the computer MIDI keyboard.
    pub fn keyboard_tracks(&self) -> Vec<TrackId> {
        let explicit: Vec<TrackId> = self
            .s
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Instrument && t.keyboard_input)
            .map(|t| t.id)
            .collect();
        if !explicit.is_empty() {
            return explicit;
        }
        let armed: Vec<TrackId> = self
            .s
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Instrument && t.arm)
            .map(|t| t.id)
            .collect();
        if !armed.is_empty() {
            return armed;
        }
        self.audition_track()
            .or_else(|| self.s.selected_track.filter(|id| self.s.project.track(*id).is_some_and(|t| t.kind == TrackKind::Instrument)))
            .into_iter()
            .collect()
    }

    pub fn toggle_computer_keyboard(&mut self, cx: &mut Context<Self>) {
        self.piano_keyboard = !self.piano_keyboard;
        if !self.piano_keyboard {
            self.release_audition(cx);
            self.toast("Computer MIDI Keyboard off", cx);
        } else {
            self.toast(format!("Computer MIDI Keyboard on (Octave C{})", self.piano_octave), cx);
        }
        cx.notify();
    }

    pub fn toggle_track_keyboard(&mut self, id: TrackId, cx: &mut Context<Self>) {
        let Some(t) = self.s.project.track(id) else { return };
        let next = !t.keyboard_input;
        self.release_audition(cx);
        self.s.set_track_keyboard(id, next);
        if next {
            self.piano_keyboard = true;
        }
        cx.notify();
    }

    pub fn press_audition(&mut self, source: &str, track: TrackId, key: u8, cx: &mut Context<Self>) -> bool {
        let ok = self.audition.press(source, track, key, |t, k, v| self.b.engine.audition_note(t, k, v));
        if !ok {
            self.toast("Cannot audition: check the audio device and instrument", cx);
        }
        cx.notify();
        ok
    }

    pub fn press_audition_many(&mut self, source: &str, tracks: &[TrackId], key: u8, velocity: u8, cx: &mut Context<Self>) -> bool {
        let ok = self.audition.press_many(source, tracks, key, velocity, |t, k, v| self.b.engine.audition_note(t, k, v));
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
        if !self.focus.is_focused(window) {
            return;
        }
        self.handle_keyboard_midi_down(e, cx);
    }

    pub fn handle_keyboard_midi_down(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        if !self.is_typing_notes() || e.keystroke.modifiers.modified() {
            return;
        }
        if let Some(ctrl) = crate::audition::keyboard_control(&e.keystroke.key) {
            if !e.is_held {
                use crate::audition::KeyboardControl::*;
                match ctrl {
                    OctaveDown => {
                        self.release_audition(cx);
                        self.piano_octave = self.piano_octave.saturating_sub(1);
                        self.toast(format!("Keyboard octave C{}", self.piano_octave), cx);
                    }
                    OctaveUp => {
                        self.release_audition(cx);
                        self.piano_octave = (self.piano_octave + 1).min(9);
                        self.toast(format!("Keyboard octave C{}", self.piano_octave), cx);
                    }
                    VelocityDown => {
                        self.piano_velocity = self.piano_velocity.saturating_sub(15).max(10);
                        self.toast(format!("Keyboard velocity {}", self.piano_velocity), cx);
                    }
                    VelocityUp => {
                        self.piano_velocity = (self.piano_velocity + 15).min(127);
                        self.toast(format!("Keyboard velocity {}", self.piano_velocity), cx);
                    }
                }
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        let Some(key) = crate::audition::keyboard_note(&e.keystroke.key, self.piano_octave) else { return };
        let tracks = self.keyboard_tracks();
        if tracks.is_empty() {
            return;
        }
        if !e.is_held {
            self.press_audition_many(&e.keystroke.key, &tracks, key, self.piano_velocity, cx);
        }
        cx.stop_propagation();
    }

    pub fn piano_key_up(&mut self, e: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.audition.release(&e.keystroke.key, |t, k, v| self.b.engine.audition_note(t, k, v)) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    // ── Drags ──

    /// The least height the detail panel may have in the view it shows now.
    pub fn bottom_min_h(&self) -> f32 {
        bottom_min_h(self.bottom_stacked, self.s.bottom_tab == BottomTab::Devices, theme::m().dock_bar_h)
    }

    /// The detail panel's height as drawn: the stored one, raised to what
    /// the view needs (a panel saved at 264 px still shows every knob).
    pub fn bottom_h_eff(&self) -> f32 {
        bottom_h_fitted(self.bottom_h, self.window_h, self.bottom_min_h())
    }

    /// Start dragging the top edge of the detail panel from window y `y`.
    pub fn begin_bottom_resize(&mut self, y: f32, cx: &mut Context<Self>) {
        let start_h = self.bottom_h_eff();
        self.begin_drag(Drag::BottomResize { start_y: y, start_h }, cx);
    }

    /// Start dragging the divider between stacked Clip and Device views.
    pub fn begin_bottom_split_resize(&mut self, y: f32, total_h: f32, cx: &mut Context<Self>) {
        let start_split = self.bottom_split;
        self.begin_drag(Drag::BottomSplit { start_y: y, start_split, total_h: total_h.max(120.) }, cx);
    }

    /// Toggle Live 12 stacked Clip + Devices mode in the detail panel.
    pub fn toggle_bottom_stacked(&mut self, cx: &mut Context<Self>) {
        self.bottom_stacked = !self.bottom_stacked;
        self.bottom_open = true;
        if self.bottom_stacked && self.bottom_h < 480. {
            self.bottom_h = bottom_h_fitted(480., self.window_h, self.bottom_min_h());
        }
        cx.notify();
    }

    pub fn begin_drag(&mut self, drag: Drag, cx: &mut Context<Self>) {
        // Map mode: a press on a knob maps it (or picks a macro) instead of dragging.
        if let Drag::Knob { target, .. } = &drag
            && views::devices::rack_ui::map_pick(self, *target, cx)
        {
            return;
        }
        // The same for a modulator's Map mode (a knob, the pan knob or a fader).
        if views::device_ui::modulator::pick_drag(self, &drag, cx) {
            return;
        }
        self.s.seal();
        self.drag = Some(drag);
        cx.notify();
    }

    // ── Value-box typing ──

    /// Open a value box for typing, pre-filled with the current value (the
    /// first key replaces it — see [`NumEdit::fresh`]).
    pub fn begin_num_edit(&mut self, target: NumTarget, prefill: String, cx: &mut Context<Self>) {
        self.num_edit = Some(NumEdit {
            target,
            text: prefill,
            fresh: true,
        });
        cx.notify();
    }

    /// Feed a keystroke to the open value-box edit.
    pub fn num_edit_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) -> NumKey {
        let Some(edit) = &mut self.num_edit else {
            return NumKey::Ignored;
        };
        let k = &e.keystroke;
        let out = match k.key.as_str() {
            "enter" => NumKey::Commit,
            "escape" => {
                self.num_edit = None;
                NumKey::Cancel
            }
            "backspace" | "delete" => {
                if edit.fresh {
                    edit.text.clear();
                    edit.fresh = false;
                } else {
                    edit.text.pop();
                }
                NumKey::Typed
            }
            _ => {
                let Some(ch) = num_edit_char(k) else {
                    return NumKey::Ignored;
                };
                if edit.fresh {
                    edit.text.clear();
                    edit.fresh = false;
                }
                edit.text.push_str(ch);
                NumKey::Typed
            }
        };
        cx.stop_propagation();
        cx.notify();
        out
    }

    /// The root's key hook for value-box typing; runs before the piano keys.
    pub(crate) fn num_edit_key_down(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.num_edit.is_none() || self.palette.open || self.modal.is_some() {
            return;
        }
        if matches!(self.num_edit_key(e, cx), NumKey::Commit) {
            self.commit_num_edit(cx);
        }
    }

    /// Enter in a value box: parse the text and apply it (one undo step).
    fn commit_num_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.num_edit.take() else {
            return;
        };
        match edit.target {
            NumTarget::Param { device, param } => {
                let info =
                    self.fx
                        .param_info(self.b.plugins.as_ref(), &self.s.project, device, param);
                if let (Some(info), Some(v)) = (info, parse_plain(&edit.text)) {
                    let v = views::device_ui::typed_to_plain(self, device, param, v);
                    views::device_ui::set_now(self, device, param, info.to_normalized(v));
                }
            }
            NumTarget::Transpose { clip } => {
                if let Some(v) = parse_plain(&edit.text) {
                    self.s.seal();
                    self.s.set_clip_warp("transpose-typed", clip, |w| w.transpose = v);
                    self.s.seal();
                }
            }
            NumTarget::Detune { clip } => {
                if let Some(v) = parse_plain(&edit.text) {
                    self.s.seal();
                    self.s.set_clip_warp("detune-typed", clip, |w| w.detune = v);
                    self.s.seal();
                }
            }
            NumTarget::SegBpm { clip } => {
                // Parsed as f64: a typed 123.45 must not become 123.4499969….
                if let Ok(v) = edit.text.trim().replace(',', ".").parse::<f64>() {
                    crate::views::bottom::edit_seg_bpm(&mut self.s, clip, crate::views::bottom::SegBpm::Set(v));
                }
            }
            NumTarget::ClipStart { clip } => {
                crate::views::bottom::commit_clip_field(&mut self.s, clip, crate::views::bottom::ClipField::Start, &edit.text);
            }
            NumTarget::ClipLength { clip } => {
                crate::views::bottom::commit_clip_field(&mut self.s, clip, crate::views::bottom::ClipField::Length, &edit.text);
            }
            NumTarget::Volume { track } => {
                let text = edit.text.trim().to_lowercase();
                let db = if text == "-inf" || text == "-" { Some(-70.) } else { parse_plain(&text) };
                if let Some(db) = db {
                    self.s.seal();
                    self.s.set_track(track, TrackProp::VolumeDb(if db <= -70. { -150. } else { db.min(6.) }));
                    self.s.seal();
                }
            }
            NumTarget::Tempo => {
                if let Some(v) = parse_plain(&edit.text) {
                    self.s.set_tempo((v as f64).clamp(20., 999.));
                    self.s.seal();
                }
            }
        }
        cx.notify();
    }

    /// While a value box is open, a click anywhere drops the text (the
    /// box's own click reopens it right after, in the bubble phase).
    pub(crate) fn num_edit_capture(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.num_edit.as_ref()?;
        let weak = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let w = weak.clone();
                    window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = w.update(cx, |this, cx| {
                                if this.num_edit.take().is_some() {
                                    cx.notify();
                                }
                            });
                        }
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
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
                    // Cmd/Ctrl: every clip of the sample, selected or not.
                    KnobTarget::Transpose { clip } if e.modifiers.secondary() => {
                        self.s.set_sample_warp(&format!("transpose-{}", clip.0), clip, |w| w.transpose = v * 96. - 48.);
                    }
                    KnobTarget::Detune { clip } if e.modifiers.secondary() => {
                        self.s.set_sample_warp(&format!("detune-{}", clip.0), clip, |w| w.detune = v * 100. - 50.);
                    }
                    KnobTarget::Transpose { clip } => {
                        self.s.set_clip_warp(&format!("transpose-{}", clip.0), clip, |w| w.transpose = v * 96. - 48.);
                    }
                    KnobTarget::Detune { clip } => {
                        self.s.set_clip_warp(&format!("detune-{}", clip.0), clip, |w| w.detune = v * 100. - 50.);
                    }
                    KnobTarget::Chain { .. } | KnobTarget::Return { .. } => {
                        views::devices::rack_ui::knob_drag(self, *target, v);
                    }
                }
            }
            Drag::Pad => {}
            Drag::SoundsResize { start_x, start } => {
                self.sounds.prefs.width = views::sounds::width_after_drag(*start, f32::from(e.position.x) - *start_x, self.sounds.prefs.side);
            }
            Drag::SoundsVolume { start_x, start } => {
                let v = views::sounds::volume_after_drag(*start, f32::from(e.position.x) - *start_x);
                self.sounds.prefs.volume = v;
                self.b.engine.set_preview_volume(v);
            }
            Drag::Rack(d) => views::devices::rack_ui::drag_move(self, d, f32::from(e.position.x), fine),
            Drag::Mod(d) => views::device_ui::modulator::drag_move(self, d, f32::from(e.position.x), fine),
            Drag::Fader { track, last_y, pos, travel } => {
                let dy = *last_y - y;
                *last_y = y;
                *pos = (*pos + dy / *travel * fine).clamp(0., 1.);
                let db = fader_to_db(*pos);
                let id = *track;
                self.s.set_track_coalesced(&format!("fader-{}", id.0), id, TrackProp::VolumeDb(db));
            }
            Drag::HFader { track, last_x, pos, travel } => {
                let x = f32::from(e.position.x);
                let dx = x - *last_x;
                *last_x = x;
                *pos = (*pos + dx / *travel * fine).clamp(0., 1.);
                let id = *track;
                self.s.set_track_coalesced(&format!("fader-{}", id.0), id, TrackProp::VolumeDb(fader_to_db(*pos)));
            }
            Drag::TrackHeight { track, start_y, start_h } => {
                // Whole multiples of 4 px of what is shown, as every size in the interface.
                let h = theme::m().dragged_track_h(*start_h, y - *start_y);
                let id = *track;
                self.s.set_track_coalesced(&format!("height-{}", id.0), id, TrackProp::Height(h));
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
            Drag::BottomResize { start_y, start_h } => {
                self.bottom_h = bottom_h_fitted(*start_h + (*start_y - y), self.window_h, self.bottom_min_h());
            }
            Drag::BottomSplit { start_y, start_split, total_h } => {
                self.bottom_split = split_fitted(*start_split + (y - *start_y) / total_h.max(120.), *total_h);
            }
            Drag::Notes { clip, originals, origin, resize, applied, preview } => {
                let (tick, key) = self.piano_pos(e.position);
                let raw = tick - origin.0;
                let dt = self.s.snap_tick(crate::editing::GridView::Piano, raw, e.modifiers.alt);
                let diatonic = self.s.scale_folded(Some(*clip)).is_some();
                let dk = if *resize {
                    0
                } else if diatonic {
                    let rows = self.s.piano_rows(Some(*clip));
                    let r0 = crate::state::piano_nearest_row(&rows, origin.1.clamp(0, 127) as u8) as i32;
                    let r1 = self.piano_row_clamped(e.position.y, rows.len());
                    r0 - r1
                } else {
                    key - origin.1
                };
                if let Some((track, previous)) = *preview {
                    let next = if diatonic {
                        self.s
                            .clip_scale(Some(*clip))
                            .map_or((origin.1 + dk).clamp(0, 127) as u8, |s| crate::state::step_note_in_scale(s, origin.1.clamp(0, 127) as u8, dk))
                    } else {
                        (origin.1 + dk).clamp(0, 127) as u8
                    };
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
                        self.s.move_notes_scale(*clip, originals, dt, dk, diatonic);
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
            Drag::Probability { clip, originals, last_y, lane_h, delta } => {
                let dy = *last_y - y;
                *last_y = y;
                if dy != 0. {
                    let next = crate::state::probability_delta(originals, *delta + dy / lane_h.max(1.) * fine);
                    if next != *delta {
                        *delta = next;
                        self.s.set_probability(*clip, originals, *delta);
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
            // Clips, their edges and the time selection snap to the grid of the arrangement.
            d @ (Drag::Clip { .. } | Drag::ClipResize { .. } | Drag::TimeSel { .. }) => crate::editing::gestures::drag_move(self, d, e),
            d @ (Drag::ClipShape(_) | Drag::ClipCopy(_)) => views::arrange_clips::drag_move(self, d, e),
            Drag::AutoPoint { track, target, original, index } => {
                let (tick, _) = self.arrange_pos(e.position);
                let tick = self.s.snap_tick(crate::editing::GridView::Arrange, tick, e.modifiers.alt);
                if let Some(value) = views::arrangement::automation_value_at(self, *track, *target, e.position) {
                    self.s.move_automation_point(*track, *target, original.clone(), *index, tick, value);
                }
            }
            Drag::AutoCurve { track, target, index, start_y, start_curve, rising } => {
                // Dragging up always lifts the middle of the segment, whichever way it runs.
                let dy = (*start_y - f32::from(e.position.y)) / 60.;
                let curve = *start_curve + if *rising { -dy } else { dy };
                self.s.set_automation_curve(*track, *target, *index, curve);
            }
            Drag::AutoRange { track, target, anchor } => {
                let (tick, _) = self.arrange_pos(e.position);
                let at = self.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), e.modifiers.alt);
                self.s.set_auto_range(*track, *target, *anchor, at);
            }
            Drag::TakeRange { track, lane, anchor, .. } => {
                let (tick, _) = self.arrange_pos(e.position);
                let at = self.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), e.modifiers.alt);
                // Still at the press: the whole take under it (a click promotes a clip).
                match self.s.take_clip_span(*track, *lane, *anchor).filter(|_| at == *anchor) {
                    Some((a, b)) => self.s.select_take_range(*track, *lane, a, b),
                    None => self.s.select_take_range(*track, *lane, *anchor, at),
                }
            }
            Drag::AutoXform { range, original, kind, start_y, lo, hi } => {
                use crate::state::AutoXform;
                use views::arrangement::XformKind as K;
                let dy = (*start_y - f32::from(e.position.y)) / views::arrangement::auto_value_px() * fine;
                let h = (*hi - *lo).max(0.05);
                let mid = (*lo + *hi) * 0.5;
                let (tick, _) = self.arrange_pos(e.position);
                let at = self.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), e.modifiers.alt);
                let xf = match kind {
                    // Alt scales about the middle (both edges move), else about the opposite edge.
                    K::ScaleTop if e.modifiers.alt => AutoXform::Scale { anchor: mid, factor: ((h + 2. * dy) / h).clamp(-4., 4.) },
                    K::ScaleTop => AutoXform::Scale { anchor: *lo, factor: ((h + dy) / h).clamp(-4., 4.) },
                    K::ScaleBottom if e.modifiers.alt => AutoXform::Scale { anchor: mid, factor: ((h - 2. * dy) / h).clamp(-4., 4.) },
                    K::ScaleBottom => AutoXform::Scale { anchor: *hi, factor: ((h - dy) / h).clamp(-4., 4.) },
                    K::SkewLeft => AutoXform::Skew { left: dy, right: 0. },
                    K::SkewRight => AutoXform::Skew { left: 0., right: dy },
                    K::StretchLeft => AutoXform::Stretch { start: at.min(range.end - 1), end: range.end },
                    K::StretchRight => AutoXform::Stretch { start: range.start, end: at.max(range.start + 1) },
                };
                self.s.xform_auto_range(*range, original, xf);
            }
            Drag::AutoShift { range, original, start_y } => {
                let delta = (*start_y - f32::from(e.position.y)) / views::arrangement::auto_value_px() * fine;
                self.s.shift_auto_range(*range, original, delta);
            }
            Drag::LoopBrace { anchor } => {
                let (tick, _) = self.arrange_pos(e.position);
                let t = self.s.snap_tick(crate::editing::GridView::Arrange, tick, e.modifiers.alt).max(0);
                let (a, b) = ((*anchor).min(t), (*anchor).max(t));
                if b > a {
                    self.s.set_loop(Some((a, b)));
                }
            }
            Drag::Locator { id, grab, moved } => {
                let (tick, _) = self.arrange_pos(e.position);
                let at = self.s.snap_tick(crate::editing::GridView::Arrange, (tick - *grab).max(0), e.modifiers.alt);
                // A click that wobbles is still a click: it takes a few pixels to move.
                let far = self.s.project.locator(*id).is_some_and(|l| (at - l.tick).abs() as f32 * self.s.arrange.px_per_beat / PPQ as f32 >= 3.);
                if *moved || far {
                    *moved = true;
                    self.s.move_locator(*id, at, true);
                }
            }
            Drag::Scrub => {
                let (tick, _) = self.arrange_pos(e.position);
                // The insert marker follows the scrub.
                let at = self.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), e.modifiers.alt);
                self.s.ed.marker = at;
                self.b.engine.locate(at);
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
                Drag::Audition { .. } | Drag::Pad => {
                    self.audition.release(crate::audition::MOUSE, |t, k, v| self.b.engine.audition_note(t, k, v));
                }
                Drag::SoundsResize { .. } | Drag::SoundsVolume { .. } => self.sounds.prefs.save(),
                Drag::Notes { .. } => {
                    self.audition.release(crate::audition::NOTE, |t, k, v| self.b.engine.audition_note(t, k, v));
                }
                Drag::TakeRange { hold: false, .. } => {
                    self.s.promote_take_selection();
                }
                // A locator that never moved was clicked: the playhead goes there.
                Drag::Locator { id, moved: false, .. } => {
                    if let Some(tick) = self.s.project.locator(id).map(|l| l.tick) {
                        self.jump_to(tick, cx);
                    }
                }
                Drag::ClipCopy(d) => views::arrange_clips::copy_end(self, d),
                Drag::ClipShape(d) => views::arrange_clips::shape_end(self, &d, cx),
                // A click on a clip that never moved it puts the insert
                // marker (and, when stopped, the playhead) where it was clicked.
                Drag::Clip { track, start, applied, .. } => {
                    if let Some(at) = self.s.release_clip(applied != (start, track))
                        && !self.b.engine.transport().playing
                    {
                        self.b.engine.locate(at);
                    }
                }
                _ => {}
            }
            self.s.ed.pressed = None;
            self.s.seal();
            cx.notify();
        }
    }

    fn piano_row_clamped(&self, y_win: Pixels, rows_len: usize) -> i32 {
        let b = self.regions.piano.get();
        let y = f32::from(y_win - b.origin.y) + self.s.piano.scroll_y;
        ((y / self.s.piano.row_h).floor() as i32).clamp(0, rows_len.saturating_sub(1) as i32)
    }

    /// Window position → (tick, key) in the piano roll.
    pub fn piano_pos(&self, pos: Point<Pixels>) -> (Tick, i32) {
        let b = self.regions.piano.get();
        let x = f32::from(pos.x - b.origin.x) + self.s.piano.scroll_x;
        let tick = (x / self.s.piano.px_per_beat * PPQ as f32) as Tick;
        let rows = self.s.piano_rows(self.s.selected_clip);
        let r = self.piano_row_clamped(pos.y, rows.len()) as usize;
        let key = rows.get(r).copied().unwrap_or(60) as i32;
        (tick, key)
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

    /// The track under `pos` (a window position over the headers or the lanes)
    /// and whether the drop is on its lower half: where a dragged header goes.
    pub fn track_drop_target(&self, pos: Point<Pixels>) -> Option<(TrackId, bool)> {
        let b = self.regions.arrange.get();
        let y = f32::from(pos.y - b.origin.y) + self.s.arrange.scroll_y;
        views::arrangement::rows(&self.s).into_iter().find(|r| y >= r.top && y < r.top + r.total()).map(|r| (r.track, y > r.top + r.h / 2.))
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

    pub fn extract_drums_from_selection(&mut self, cx: &mut Context<Self>) {
        let Some((name, path)) = self.audio_target() else {
            self.toast("Select an audio clip first", cx);
            return;
        };
        let id = self.b.ai.extract_drums(DrumExtractRequest {
            path: path.clone(),
            engine: DrumExtractMode::Auto,
            de_bleed: 0.75,
            de_reverb: 0.45,
            transient: 0.35,
            sensitivity: 0.60,
            loop_bars: 2,
            normalize_db: Some(-1.0),
        });
        self.drum_jobs.insert(id, self.audio_start(&path));
        self.chat_log.push(ChatEntry::Job { id, label: format!("Extracting clean drums from {name}"), fraction: 0., done: false });
        self.ensure_ai_poll(cx);
        cx.notify();
    }

    /// Put an extracted clean drum one-shot, loop, or stem onto a new audio track at the playhead bar.
    pub fn insert_extracted_audio(&mut self, path: std::path::PathBuf, name: String, bpm: Option<f32>, cx: &mut Context<Self>) {
        let start = Snap::Bar.floor(self.b.engine.transport().position.max(0));
        let (sample_rate, channels, frames) =
            hound::WavReader::open(&path).map(|r| (r.spec().sample_rate, r.spec().channels, r.duration() as u64)).unwrap_or((44_100, 2, 0));
        let index = self.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
        let mut cmds = audio_track_commands(&mut self.s, &name, Color::palette(12), &path, (sample_rate, channels, frames), start, index);
        if let Some(bpm) = bpm {
            for c in &mut cmds {
                if let Command::AddSource { source, .. } = c
                    && let Source::Audio(a) = source.as_mut()
                {
                    a.warp.original_bpm = Some(f64::from(bpm));
                    a.warp.enabled = true;
                }
            }
        }
        if self.s.edit(&format!("Add {name}"), cmds) {
            self.toast(format!("Added \"{name}\" track"), cx);
            cx.notify();
        }
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
    /// A WAV header gives the real length at once; any other format is probed
    /// (decoded) on a worker thread first, so a long FLAC does not stall the UI.
    pub fn import_audio(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let start = Snap::Bar.floor(self.b.engine.transport().position.max(0));
        if let Some(facts) = wav_header(&path) {
            self.import_audio_probed(path, Some(facts), start, cx);
            return;
        }
        let probe = path.clone();
        let epoch = self.project_epoch;
        let task = cx.background_executor().spawn(async move { probe_audio(&probe) });
        cx.spawn(async move |this, cx| {
            let facts = task.await;
            let _ = this.update(cx, |ws, cx| {
                // Another project was opened meanwhile: the import is not for it.
                if probe_is_current(epoch, ws.project_epoch) {
                    ws.import_audio_probed(path, facts, start, cx);
                }
            });
        })
        .detach();
    }

    /// The second half of [`import_audio`](Self::import_audio), with what the
    /// file is (`None` when it could not be read: the analysis tries again and
    /// fixes the clip's provisional length).
    fn import_audio_probed(&mut self, path: std::path::PathBuf, facts: Option<AudioFacts>, start: Tick, cx: &mut Context<Self>) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
        let index = self.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
        let cmds = audio_track_commands(&mut self.s, &name, Color::palette(4), &path, facts.unwrap_or((48_000, 2, 0)), start, index);
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
        cx.notify();
    }

    /// Audio files dropped from Finder or Explorer onto the arrangement: one
    /// clip per file on `track` (an audio track; otherwise one new track holds
    /// them), one after another from `start`, as one undo step.
    pub fn drop_audio_files(&mut self, paths: Vec<std::path::PathBuf>, track: Option<TrackId>, start: Tick, cx: &mut Context<Self>) {
        let files: Vec<std::path::PathBuf> = paths.into_iter().filter(|p| is_audio_file(p)).collect();
        if files.is_empty() {
            self.toast("Drop audio files: WAV, AIFF, FLAC, MP3 or OGG", cx);
            return;
        }
        let target = track.filter(|t| self.s.project.track(*t).is_some_and(|t| t.kind == TrackKind::Audio));
        // A WAV header gives its length at once; any other format is decoded to
        // count its frames, on a worker thread.
        if files.iter().all(|f| wav_header(f).is_some()) {
            let facts = files.into_iter().map(|f| (wav_header(&f), f)).collect();
            self.drop_audio_probed(facts, target, start, cx);
            return;
        }
        let epoch = self.project_epoch;
        let task = cx.background_executor().spawn(async move { files.into_iter().map(|f| (probe_audio(&f), f)).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let facts = task.await;
            let _ = this.update(cx, |ws, cx| {
                if probe_is_current(epoch, ws.project_epoch) {
                    ws.drop_audio_probed(facts, target, start, cx);
                }
            });
        })
        .detach();
    }

    fn drop_audio_probed(&mut self, files: Vec<(Option<AudioFacts>, std::path::PathBuf)>, target: Option<TrackId>, start: Tick, cx: &mut Context<Self>) {
        // A track that was deleted while the files were read is a new track after all.
        let target = target.filter(|t| self.s.project.track(*t).is_some());
        let index = self.s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
        let cmds = drop_audio_commands(&mut self.s, &files, target, start, index);
        let sources: Vec<(SourceId, std::path::PathBuf)> = cmds
            .iter()
            .filter_map(|c| match c {
                Command::AddSource { id, source } => match source.as_ref() {
                    Source::Audio(a) => Some((*id, a.path.clone())),
                    Source::Notes(_) => None,
                },
                _ => None,
            })
            .collect();
        let n = sources.len();
        if !self.s.edit("Drop audio", cmds) {
            return;
        }
        for (source, path) in sources {
            let id = self.b.ai.analyze(&path);
            self.analysis_jobs.insert(id, source);
        }
        self.ensure_ai_poll(cx);
        self.toast(if n == 1 { "Added 1 audio file".to_owned() } else { format!("Added {n} audio files") }, cx);
        cx.notify();
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
        let trust = if item.is_loop && item.bpm.is_some() { self.b.library.bpm_trust(item.id) } else { BpmTrust::Reliable };
        let fit = self.drop_fit(item, trust);
        let cmds = sample_clip_commands(&mut self.s, item, target, start, trust, fit.as_ref());
        let name = item.name.clone();
        if self.s.edit("Add sample", cmds) {
            self.b.library.mark_used(item.id);
            let note = if trust.allows_auto_warp() { "" } else { " · tempo only guessed from its length, warp off" };
            self.toast(format!("Added {name}{note}"), cx);
        }
    }

    /// How `item` is fitted to the song when it lands: only a sound that comes
    /// out of the Sounds panel with "Fit to song" on, once. The Session slots
    /// ask the same way before they build their sample.
    pub fn drop_fit(&mut self, item: &LibraryItem, trust: BpmTrust) -> Option<views::sounds::logic::DropFit> {
        let carried = self.sounds.carrying.take() == Some(item.id);
        (carried && self.sounds.fits_song()).then(|| views::sounds::logic::drop_fit(item, trust, self.s.project.scale, self.s.project.tempo.initial_bpm()))
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

    /// The samples the palette last listed (Organize on disk works on them);
    /// before any search, the library's search for everything.
    pub fn library_view(&self) -> Vec<LibraryItem> {
        let items = self.palette.sample_items();
        if items.is_empty() { self.b.library.search("", None) } else { items }
    }

    fn apply_analysis(&mut self, source: SourceId, a: &AudioAnalysis) {
        let cmds = analysis_commands(&self.s.project, source, a);
        if !cmds.is_empty() {
            self.s.edit_as("Analyse audio", Origin::Feature("analysis".into()), cmds);
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
                AiEvent::Progress { job, label, fraction } => {
                    self.set_job(job, fraction, false);
                    for st in self.fx.drum_extract.values_mut() {
                        if st.active_job == Some(job) {
                            st.progress = fraction;
                            st.progress_label = label.clone();
                        }
                    }
                }
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
                AiEvent::DrumsReady { job, result } => {
                    self.set_job(job, 1., true);
                    let mut handled_by_device = false;
                    for st in self.fx.drum_extract.values_mut() {
                        if st.active_job == Some(job) {
                            st.active_job = None;
                            st.progress = 1.0;
                            st.progress_label = format!("Done · {} one-shots, {} loops", result.oneshots.len(), result.loops.len());
                            st.error = None;
                            st.result = Some(result.clone());
                            handled_by_device = true;
                        }
                    }
                    if handled_by_device {
                        self.toast(format!("Extracted {} one-shots & {} loops", result.oneshots.len(), result.loops.len()), cx);
                    }
                    if let Some(maybe_start) = self.drum_jobs.remove(&job) {
                        let start = maybe_start.unwrap_or(0);
                        let mut tracks_to_add = vec![(format!("{} Clean Drums", result.source_name), result.clean_stem_path.clone())];
                        for lp in result.loops.iter().take(2) {
                            tracks_to_add.push((lp.name.clone(), lp.path.clone()));
                        }
                        let cmds = stem_commands(&mut self.s, &tracks_to_add, start);
                        let label = format!(
                            "Add clean drum stem + {} loop(s) ({} one-shots saved in {})",
                            result.loops.len().min(2),
                            result.oneshots.len(),
                            result.out_dir.display()
                        );
                        if let Some(id) = self.s.propose(&label, Origin::Feature("drum-extract".into()), cmds) {
                            self.chat_log.extend(proposal_entry(&self.s, id));
                        }
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
                    self.drum_jobs.remove(&job);
                    let mut device_only = false;
                    for st in self.fx.drum_extract.values_mut() {
                        if st.active_job == Some(job) {
                            st.active_job = None;
                            st.error = Some(message.clone());
                            device_only = true;
                        }
                    }
                    if !device_only {
                        self.chat_log.push(ChatEntry::Assistant(format!("That failed: {message}")));
                    }
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

    pub(crate) fn drag_capture(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
            menu_overlay(
                menu.at,
                motion::rise_in(id, div().child(body), 6.),
                cx.listener(|this, _, _, cx| this.close_menu(cx)),
            )
            .into_any_element(),
        )
    }

    /// The status bar. Left: the Sounds panel, undo and redo with what they
    /// act on, whether the project is saved, and what is selected. Right: what
    /// needs attention (proposals, faults, updates), the audio device (a
    /// click opens its preferences), resource use and the interface zoom.
    fn render_status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        use views::status::{self, SaveState};
        let pal = p();
        let mac = cfg!(target_os = "macos");
        let (undo, redo) = (self.s.history.undo_label().map(str::to_owned), self.s.history.redo_label().map(str::to_owned));
        let err = self.s.last_error.clone();
        let pending = self.s.history.proposals().len();
        let health = self.render_engine_health();
        let selection = status::selection(&self.s);
        let save = SaveState::of(self.project_path.is_some(), self.s.history.is_dirty());
        let file = self.project_path.as_ref().map(|p| p.display().to_string());
        let device = self.b.engine.status_label();
        let device_text = crate::paint::fit(status::device_short(&device), 400., TEXT_SM);
        // A text item that acts on click, lit on hover.
        let item = |id: &'static str| div().id(id).flex_none().h(px(20.)).px(px(SP1)).flex().items_center().gap(px(SP1)).rounded(px(r_sm())).cursor_pointer().hover(|s| s.bg(hover()));
        let step = |id: &'static str, ic: Icon, label: &Option<String>, verb: &'static str, keys: &'static str| {
            let tip = match label {
                Some(l) => format!("{verb}: {l}  ({keys})"),
                None => format!("Nothing to {}", verb.to_lowercase()),
            };
            crate::widgets::sized_icon_button(id, ic, false, 20., 12.).when(label.is_none(), |d| d.opacity(0.45)).tooltip(crate::widgets::tip_owned(tip))
        };
        div()
            .h(px(theme::m().status_bar_h))
            .flex_none()
            .px(px(SP2))
            .gap(px(SP2))
            .flex()
            .items_center()
            .bg(pal.panel)
            .border_t_1()
            .border_color(border())
            .text_size(px(fs(TEXT_SM)))
            .text_color(pal.muted)
            .child(
                crate::widgets::icon_chip("sounds-toggle", Icon::Waveform, self.sounds.prefs.open, pal.accent)
                    .tooltip(crate::widgets::tip("Sounds panel: search, hear sounds fitted to the song, drag them in", Some("Ctrl+Shift+K")))
                    .on_click(cx.listener(|this, _, _, cx| views::sounds::toggle(this, cx))),
            )
            .child(crate::widgets::divider_v(12.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(step("status-undo", Icon::Undo, &undo, "Undo", if mac { "⌘Z" } else { "Ctrl+Z" }).on_click(cx.listener(|this, _, window, cx| this.undo(&Undo, window, cx))))
                    .child(step("status-redo", Icon::Redo, &redo, "Redo", if mac { "⇧⌘Z" } else { "Ctrl+Shift+Z" }).on_click(cx.listener(|this, _, window, cx| this.redo(&Redo, window, cx))))
                    .child(
                        item("status-history")
                            .tooltip(crate::widgets::tip("Undo History: click to view and jump to any step", None))
                            .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                                let at = e.position();
                                this.open_menu(MenuKind::UndoHistory, gpui::point(at.x, at.y - px(260.)), cx);
                            }))
                            .child(crate::widgets::label(
                                undo.as_deref().map_or_else(|| "History".to_owned(), |l| crate::paint::fit(l, 180., TEXT_SM)),
                                TEXT_SM,
                                if undo.is_some() { pal.muted } else { pal.faint },
                            )),
                    ),
            )
            .child(crate::widgets::divider_v(12.))
            .child(
                crate::widgets::icon_chip("status-info-toggle", Icon::Help, self.info_view, pal.accent)
                    .tooltip(crate::widgets::tip("Info View: show live help for the control under the pointer", Some("?")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.info_view = !this.info_view;
                        cx.notify();
                    })),
            )
            .when_some(
                {
                    let soloed = self.s.project.tracks.iter().filter(|t| t.solo).count();
                    (soloed > 0).then_some(soloed)
                },
                |d, soloed| {
                    d.child(
                        div()
                            .id("status-unsolo-all")
                            .flex_none()
                            .h(px(18.))
                            .px(px(SP2))
                            .flex()
                            .items_center()
                            .gap(px(SP1))
                            .rounded(px(r_sm()))
                            .cursor_pointer()
                            .bg(with_alpha(pal.solo, 0.22))
                            .border_1()
                            .border_color(with_alpha(pal.solo, 0.6))
                            .hover(|s| s.bg(with_alpha(pal.solo, 0.32)))
                            .tooltip(crate::widgets::tip("Unsolo all tracks", None))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.s.unsolo_all();
                                cx.notify();
                            }))
                            .child(crate::widgets::label(format!("Unsolo ({soloed})"), TEXT_XS, pal.solo).font_weight(gpui::FontWeight::SEMIBOLD)),
                    )
                },
            )
            .when(self.info_view, |d| {
                let info = crate::widgets::last_info();
                d.child(crate::widgets::divider_v(12.)).child(
                    div()
                        .flex_shrink(1.)
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .overflow_hidden()
                        .child(crate::widgets::label(
                            info.as_ref().map_or_else(|| "Hover any control to see its description here".into(), |(t, _)| t.clone()),
                            TEXT_SM,
                            if info.is_some() { pal.text } else { pal.faint },
                        ).truncate())
                        .when_some(info.and_then(|(_, h)| h), |row, h| {
                            row.child(div().flex_none().px(px(SP1)).rounded(px(r_sm())).bg(content(0.08)).child(crate::widgets::mono(h, TEXT_XS, pal.muted)))
                        }),
                )
            })
            .child(crate::widgets::divider_v(12.))
            .child(
                item("status-save")
                    .child(match save {
                        SaveState::Saved => crate::icons::icon(Icon::Check, 12., pal.faint).into_any_element(),
                        _ => div().size(px(6.)).rounded_full().bg(pal.accent).into_any_element(),
                    })
                    .child(crate::widgets::label(save.label(), TEXT_SM, if save == SaveState::Saved { pal.faint } else { pal.text }))
                    .tooltip(crate::widgets::tip_owned(match (&file, save) {
                        (Some(f), SaveState::Saved) => format!("Saved in {f}"),
                        (Some(f), _) => format!("Click to save to {f}"),
                        (None, _) => "Not in a file yet: click to choose where to save it".to_owned(),
                    }))
                    .on_click(cx.listener(|this, _, _, cx| this.file_save(cx))),
            )
            .when_some(selection, |d, (color, text)| {
                d.child(crate::widgets::divider_v(12.)).child(
                    div()
                        .flex_shrink(1.)
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .overflow_hidden()
                        .child(div().flex_none().size(px(8.)).rounded_full().bg(theme::track_color(color)))
                        .child(crate::widgets::label(text, TEXT_SM, pal.text).truncate()),
                )
            })
            .when_some(err, |d, e| {
                d.child(
                    div()
                        .id("error")
                        .flex()
                        .gap(px(SP1))
                        .items_center()
                        .cursor_pointer()
                        .child(crate::icons::icon(Icon::Alert, 12., pal.record))
                        .child(crate::widgets::label(e, TEXT_SM, pal.record))
                        .tooltip(crate::widgets::tip("Click to dismiss", None))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.s.last_error = None;
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .when_some(health, |d, h| d.child(h))
            .children(views::updates::status_item(self, cx))
            .children(views::mcp::status_item(self, cx))
            .when(pending > 0, |d| {
                d.child(
                    div()
                        .id("status-proposals")
                        .flex_none()
                        .cursor_pointer()
                        .child(crate::widgets::badge(format!("{pending} proposal{}", if pending == 1 { "" } else { "s" }), pal.accent))
                        .tooltip(crate::widgets::tip("Edits the assistant proposed and that wait for you: click to review them", None))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_open = true;
                            cx.notify();
                        })),
                )
            })
            .child(
                item("status-audio")
                    .child(crate::icons::icon(Icon::Volume, 12., pal.faint))
                    .child(crate::widgets::label(device_text, TEXT_SM, pal.muted))
                    .tooltip(crate::widgets::tip_owned(format!("{device}\nClick for the audio preferences: device, sample rate, buffer size")))
                    .on_click(cx.listener(|this, _, _, cx| this.open_modal(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Audio), cx))),
            )
            .child(crate::widgets::divider_v(12.))
            .child(self.render_usage())
            .child(crate::widgets::divider_v(12.))
            .child(self.render_zoom(cx))
            .into_any_element()
    }

    /// The engine's report in the status bar, only when something is off: red
    /// for a fault (a crash, a silenced device, a failed compile), amber for
    /// warnings about the project and repaired samples. The tooltip lists every problem.
    fn render_engine_health(&self) -> Option<AnyElement> {
        let h = self.b.engine.health();
        if h.is_clean() {
            return None;
        }
        let pal = p();
        let color = if h.is_fault() { pal.record } else { pal.meter_mid };
        let lines = h.lines(|d| self.s.project.device(d).map(|d| d.name.clone()));
        let label = if h.is_fault() {
            "Audio fault".to_owned()
        } else if h.warning_count() == 0 {
            "Audio repaired".to_owned()
        } else {
            format!("{} engine warning{}", h.warning_count(), if h.warning_count() == 1 { "" } else { "s" })
        };
        Some(
            div()
                .id("engine-health")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(crate::icons::icon(Icon::Alert, 12., color))
                .child(crate::widgets::label(label, TEXT_SM, color))
                .tooltip(crate::widgets::tip_owned(lines.join("\n")))
                .into_any_element(),
        )
    }

    /// What AI-Track is using: audio engine load, process CPU and memory.
    fn render_usage(&mut self) -> AnyElement {
        let pal = p();
        let dsp = self.b.engine.cpu_load().clamp(0., 1.);
        let threads = self.b.engine.active_audio_threads();
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
            .tooltip(crate::widgets::tip_owned(format!(
                "Resource use: audio engine load (on {} audio thread{}), this app's CPU share and memory",
                threads,
                if threads == 1 { "" } else { "s" }
            )))
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
        // A running assistant turn ends with the project it worked on (and
        // its chats are kept in that project's file, not the next one's).
        views::ai_panel::state::stop(self, cx);
        self.b.engine.stop();
        self.b.engine.locate(0);
        // The autosaved copy of the project being left stays: it goes when that
        // project is saved or the user gives it up ("Don't save", Discard).
        self.s = AppState::new(project);
        self.autosaver.reset(std::time::Instant::now());
        self.track_io_sent.clear();
        self.project_epoch += 1;
        // An analysis of the old project's audio has nothing to apply to now.
        self.analysis_jobs.clear();
        // The assistant opens this project's own chats (kept in its file).
        let chats = path.as_deref().and_then(crate::project_io::read_assistant);
        self.project_path = path;
        self.drag = None;
        self.menu = None;
        self.chat_log.retain(|e| !matches!(e, ChatEntry::Proposal { .. } | ChatEntry::Part(_)));
        views::ai_panel::state::load_chats(self, chats, cx);
        self.synced_revision = None;
        self.fx = views::device_ui::UiState::default();
        views::device_ui::modulator::set_map_mode(None);
        cx.notify();
    }

    /// The mixer as its own section: every channel strip, always the same
    /// height, independent of what the detail panel shows.
    /// The mixer section's height in the current window.
    pub fn mixer_h_fitted(&self) -> f32 {
        let bottom = if self.bottom_open { self.bottom_h_eff() } else { theme::m().dock_bar_h };
        mixer_h_fitted(self.mixer_h, bottom, self.window_h)
    }

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
                        this.mixer_h = theme::m().mixer_h;
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
                    .h(px(self.mixer_h_fitted()))
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
        let new = (old * factor).clamp(crate::nav::MIN_PX_PER_BEAT, crate::nav::MAX_PX_PER_BEAT);
        let width = f32::from(self.regions.arrange.get().size.width);
        let playhead = self.b.engine.transport().position as f32 / PPQ as f32;
        let anchor_px = playhead * old - a.scroll_x;
        let anchor = if (0. ..=width).contains(&anchor_px) { playhead } else { a.scroll_x / old };
        let offset = anchor * old - a.scroll_x;
        a.px_per_beat = new;
        a.scroll_x = (anchor * new - offset).max(0.);
        cx.notify();
    }

    /// An arrow-key edit of the selection (notes, the selected clip, or device preset stepping).
    fn key_edit(&mut self, e: KeyEdit, cx: &mut Context<Self>) {
        // Alt+↑/↓ in the Device View steps the selected device's presets (Live 13 contextual browser).
        if self.s.ed.area == crate::editing::Area::Devices
            && matches!(e, KeyEdit::SemitoneUp | KeyEdit::SemitoneDown)
            && let Some(dev_id) = self.s.selected_devices().first().copied()
        {
            let dir = if e == KeyEdit::SemitoneUp { -1 } else { 1 };
            views::device_window::chrome::step(self, dev_id, dir, cx);
            return;
        }
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

    /// A dismissable strip when the sandbox watchdog kills or fails to start a plugin.
    fn render_watchdog_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let text = self.b.engine.watchdog_warning()?;
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
                .bg(with_alpha(pal.record, 0.14))
                .border_1()
                .border_color(with_alpha(pal.record, 0.5))
                .child(crate::icons::icon(Icon::Device, 14., pal.record))
                .child(div().flex_1().min_w(px(0.)).child(crate::widgets::label(text, TEXT_SM, pal.text)))
                .child(crate::widgets::text_button("banner-plugins", "Plugin settings", None, false).on_click(cx.listener(|this, _, _, cx| {
                    this.open_modal(views::prefs::Modal::Preferences(views::prefs::PrefsPage::Plugins), cx);
                })))
                .child(crate::widgets::icon_button("watchdog-close", Icon::X, false).on_click(cx.listener(|this, _, _, cx| {
                    this.b.engine.dismiss_watchdog_warning();
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
                .bottom(px(theme::m().status_bar_h + SP4))
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

/// Recording is on while Automation Arm goes off: it must stop with it.
fn recording_needs_stop(recording: bool, automation_arm: bool) -> bool {
    recording && !automation_arm
}

/// Whether a probe started at project `started` still belongs to the open project.
fn probe_is_current(started: u64, now: u64) -> bool {
    started == now
}

/// Whether a dropped file looks like audio the engine can play.
fn is_audio_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["wav", "wave", "aif", "aiff", "flac", "mp3", "ogg", "oga"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The commands that put dropped `files` one after another from `start`: on
/// `target` when it is an audio track, else on one new audio track at `index`.
/// Each clip starts where the one before it ends.
fn drop_audio_commands(s: &mut AppState, files: &[(Option<AudioFacts>, std::path::PathBuf)], target: Option<TrackId>, start: Tick, index: usize) -> Vec<Command> {
    let color = target.and_then(|t| s.project.track(t)).map(|t| t.color).map(|c| s.clip_color_for(c));
    let mut out = Vec::new();
    let mut track = target;
    let mut at = start;
    for (facts, path) in files {
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
        let mut one = audio_track_commands(s, &name, color.unwrap_or(Color::palette(4)), path, facts.unwrap_or((48_000, 2, 0)), at, index);
        // The first file of a drop on empty space makes the track; the rest join it.
        let made = one.iter().find_map(|c| if let Command::AddTrack { track, .. } = c { Some(track.id) } else { None });
        if track.is_some() || out.iter().any(|c| matches!(c, Command::AddTrack { .. })) {
            one.retain(|c| !matches!(c, Command::AddTrack { .. }));
        } else {
            track = made;
        }
        let mut length = PROVISIONAL_LENGTH;
        for c in &mut one {
            match c {
                Command::PlaceClip { track: t, .. } => {
                    if let Some(track) = track {
                        *t = track;
                    }
                }
                Command::AddClip { clip, .. } => length = clip.length,
                _ => {}
            }
        }
        at += length;
        out.extend(one);
    }
    out
}

/// A file's `(sample rate, channels, frames)`.
pub type AudioFacts = (u32, u16, u64);

/// What a clip is given as its length until the file's real length is known.
const PROVISIONAL_LENGTH: Tick = 4 * BAR;

/// A clip's length for a file of `frames` at the project tempo (at least a
/// quarter note); [`PROVISIONAL_LENGTH`] when the length is unknown.
fn clip_length(bpm: f64, sample_rate: u32, frames: u64) -> Tick {
    if frames > 0 && sample_rate > 0 {
        let secs = frames as f64 / sample_rate as f64;
        ((secs * bpm / 60. * PPQ as f64).round() as Tick).max(PPQ)
    } else {
        PROVISIONAL_LENGTH
    }
}

/// Rate, channels and length from a WAV header (no decoding).
fn wav_header(path: &std::path::Path) -> Option<AudioFacts> {
    let r = hound::WavReader::open(path).ok()?;
    Some((r.spec().sample_rate, r.spec().channels, r.duration() as u64))
}

/// The same for any format the engine plays: a WAV header, else a decode
/// pass (symphonia) that counts the frames. Slow for long files: not for the UI thread.
fn probe_audio(path: &std::path::Path) -> Option<AudioFacts> {
    if let Some(facts) = wav_header(path) {
        return Some(facts);
    }
    let info = aitrack_library::analyze::read_audio_info(path).ok()?;
    (info.frames > 0 && info.sample_rate > 0).then_some((info.sample_rate, info.channels.max(1), info.frames))
}

/// How far clip `id` can run from each place it is on before the next clip of
/// that track and lane starts (the least over its placements). `unbounded`
/// when nothing follows it, `fallback` when a clip already starts at or under it.
pub(crate) fn room_after(project: &Project, id: ClipId, fallback: Tick) -> Tick {
    let mut room = Tick::MAX;
    for t in &project.tracks {
        for p in t.arrangement.iter().filter(|p| p.clip == id) {
            let next = t.arrangement.iter().filter(|q| q.lane == p.lane && q.id != p.id && q.start > p.start).map(|q| q.start).min();
            if let Some(next) = next {
                room = room.min(next - p.start);
            }
        }
    }
    if room <= 0 { fallback } else { room }
}

/// The commands that fold an analysis into the project. A clip that was given
/// the provisional length (the file's length was unknown at import) gets its real one.
fn analysis_commands(project: &Project, source: SourceId, a: &AudioAnalysis) -> Vec<Command> {
    let Some(Source::Audio(old)) = project.sources.get(&source) else { return Vec::new() };
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
    if new == *old {
        return Vec::new();
    }
    let learned_length = old.frames == 0 && new.frames > 0;
    let real = clip_length(project.tempo.initial_bpm(), new.sample_rate, new.frames);
    let mut cmds = vec![Command::RemoveSource { id: source }, Command::AddSource { id: source, source: Box::new(Source::Audio(new)) }];
    if learned_length {
        for (&id, clip) in &project.clips {
            if clip.source == source && clip.offset == 0 && clip.looping.is_none() && clip.length == PROVISIONAL_LENGTH {
                // Never longer than the room it has: the clips the user placed
                // after it on the same track and lane stay untouched.
                let length = real.min(room_after(project, id, clip.length));
                if length != clip.length {
                    cmds.push(Command::UpdateClip { id, clip: Box::new(Clip { length, ..clip.clone() }) });
                }
            }
        }
    }
    cmds
}

/// The commands that put a library sample on `target` (or on a new audio
/// track) at `start`. The library's key travels with the clip. A loop with a
/// BPM gets it as `original_bpm`, and warp turns on only when that BPM is
/// [`BpmTrust::Reliable`]: a tempo guessed from the file's length would stretch
/// the loop to the wrong grid, so there the user switches warp on by choice.
fn sample_clip_commands(s: &mut AppState, item: &LibraryItem, target: Option<TrackId>, start: Tick, trust: BpmTrust, fit: Option<&views::sounds::logic::DropFit>) -> Vec<Command> {
    let facts = wav_header(&item.path).unwrap_or((48_000, 2, (item.duration.max(0.) as f64 * 48_000.) as u64));
    let name = item.name.clone();
    let mut cmds = match target {
        Some(_) => Vec::new(),
        None => {
            let index = s.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(0);
            audio_track_commands(s, &name, Color::palette(4), &item.path, facts, start, index)
        }
    };
    if let Some(track) = target {
        let mut one = audio_track_commands(s, &name, Color::palette(4), &item.path, facts, start, 0);
        // Keep the source, clip and placement; place on the existing track.
        one.retain(|c| !matches!(c, Command::AddTrack { .. }));
        for c in &mut one {
            if let Command::PlaceClip { track: t, .. } = c {
                *t = track;
            }
        }
        cmds = one;
    }
    // Fitted to the song, or as the file is: the warp the source is born with.
    let mut warp = Warp::default();
    if item.is_loop {
        warp.original_bpm = item.bpm.map(f64::from);
        warp.enabled = item.bpm.is_some() && trust.allows_auto_warp();
    }
    if let Some(f) = fit {
        warp.transpose = f.transpose;
        if f.original_bpm.is_some() {
            warp.original_bpm = f.original_bpm;
            warp.enabled = f.warp;
        }
        if warp.enabled || f.transpose != 0. {
            warp.mode = f.mode;
        }
    }
    // Warped, the loop spans its beats at the song's tempo; unwarped and transposed it plays faster or slower.
    let secs = facts.2 as f64 / f64::from(facts.0.max(1));
    let span = match (warp.enabled, warp.original_bpm) {
        (true, Some(orig)) => Some((secs * orig / 60. * PPQ as f64).round() as Tick),
        _ if warp.transpose != 0. && secs > 0. => Some((secs * s.project.tempo.initial_bpm() / 60. * PPQ as f64 / warp.speed_ratio()).round() as Tick),
        _ => None,
    };
    for c in &mut cmds {
        if let Command::AddSource { source, .. } = c
            && let Source::Audio(a) = source.as_mut()
        {
            a.key = item.key;
            a.warp = warp.clone();
        }
        if let Command::AddClip { clip, .. } = c {
            if let Some(color) = target.and_then(|t| s.project.track(t)).map(|t| t.color).map(|c| s.clip_color_for(c)) {
                clip.color = color;
            }
            if let Some(span) = span.filter(|_| facts.2 > 0) {
                clip.length = span.max(PPQ);
            }
        }
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
    let length = clip_length(s.project.tempo.initial_bpm(), sample_rate, frames);
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
        crate::fps::begin(window, || "main".to_owned());
        // The knobs' modulation rings, once a frame, for the devices panel and the device windows alike.
        views::device_ui::modulator::refresh(&self.s.project);
        if !self.recovery.is_empty() {
            self.offer_recovery(window, cx);
        } else if !self.pending_missing.is_empty() {
            self.offer_relink(window, cx);
        }
        let audition_track = if matches!(self.drag, Some(Drag::Pad)) {
            // A drum pad held down: the track of the rack whose pad it is.
            self.s.selected_track
        } else if self.bottom_open && self.s.bottom_tab == BottomTab::PianoRoll && self.modal.is_none() {
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
        self.window_h = f32::from(window.viewport_size().height);
        self.window_w = f32::from(window.viewport_size().width);
        // The dev view asks for the palette before there is a window.
        if std::mem::take(&mut self.palette_pending) && !self.palette.open {
            views::palette::open(self, window, cx);
        }
        // A dialog opened from the palette (its library menu) takes over.
        if self.modal.is_some() && self.palette.open {
            views::palette::close(self, window, cx);
        }
        let editors_visible = self.bottom_open && (self.bottom_stacked || self.s.bottom_tab == BottomTab::Devices);
        let animate = window.is_visible()
            && (self.wants_frames() || self.drag.is_some() || theme::tweening() || (editors_visible && self.fx.wants_frames()) || self.meters.wants_frames() || views::sample_editor::wants_frames(self) || views::ai_panel::wants_frames(self) || views::sounds::wants_frames(self));
        let throttle_ok = !self.look.app.low_power || self.drag.is_some() || self.last_frame.elapsed() >= Duration::from_millis(50);
        if animate && throttle_ok {
            self.last_frame = Instant::now();
            window.request_animation_frame();
        }
        self.fx.sample_rate = f64::from(self.b.engine.audio_config().sample_rate.max(8_000));
        {
            let project = &self.s.project;
            self.fx.prune(|id| project.device(id).is_some());
        }
        views::device_window::close_orphans(self, cx);
        if let Some(id) = self.window_pending.take() {
            views::device_window::open(self, id, cx);
        }
        for id in std::mem::take(&mut self.windows_pending) {
            views::device_window::open(self, id, cx);
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

        let sounds_left = views::sounds::render(self, views::sounds::logic::Side::Left, cx);
        let sounds_right = views::sounds::render(self, views::sounds::logic::Side::Right, cx);
        let middle = div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap(px(SP1))
            .children(sounds_left)
            .child(center)
            .children(sounds_right)
            .child(views::ai_panel::render(self, window, cx));

        let bottom = views::bottom::render(self, window, cx);
        let mixer = self.render_mixer_section(window, cx);
        let menu = self.render_menu(window, cx);
        if let Some(q) = self.auto_search_pending.take() {
            self.auto_search.update(cx, |i, cx| i.set_text(&q, cx));
        }
        let chooser = views::arrangement::render_chooser(self, cx);
        let param_menu = views::mixer::render_param_menu(self, cx);
        let modal = views::prefs::render_modal(self, window, cx);
        let backdrop = views::prefs::backdrop(self);
        let capture = self.drag_capture(cx);
        let num_capture = self.num_edit_capture(cx);
        let fx_capture = views::device_ui::capture(self, cx);
        let sample_capture = views::sample_editor::capture(self, cx);
        let fx_overlay = views::device_ui::overlay(self, None, cx);
        let meter_strip = views::meters::render(self, cx);
        let meter_layers = views::meters::overlay(self, cx);
        let toast = self.render_toast();
        let crash_banner = views::updates::crash_banner(self, cx);
        let status = self.render_status(cx);
        let banner = self.render_banner(cx);
        let watchdog_banner = self.render_watchdog_banner(cx);
        let palette = views::palette::render(self, window, cx);

        let editing_num = self.num_edit.is_some() && !self.palette.open && self.modal.is_none();

        crate::fps::probe(div()
            .id("workspace")
            .key_context(workspace_key_context(editing_num))
            .track_focus(&self.focus)
            // Clicks here are main-window clicks (popovers open in this window).
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.fx.surface = None))
            // The stretch modifier lights the clip edges it would stretch.
            .on_modifiers_changed(cx.listener(|_, e: &gpui::ModifiersChangedEvent, _, cx| {
                if views::arrange_clips::set_stretch_key(e.modifiers.secondary()) {
                    cx.notify();
                }
            }))
            .capture_key_down(cx.listener(Self::num_edit_key_down))
            .on_key_down(cx.listener(Self::piano_key_down))
            .on_key_up(cx.listener(Self::piano_key_up))
            // The commands of the key table (clipboard, grid, navigation…).
            .map(|d| crate::keys::register(d, cx))
            .on_action(cx.listener(Self::play_stop))
            .on_action(cx.listener(Self::toggle_view))
            .on_action(cx.listener(Self::toggle_automation_mode))
            .on_action(cx.listener(Self::toggle_automation_arm))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::toggle_draw))
            .on_action(cx.listener(Self::toggle_loop))
            .on_action(cx.listener(Self::toggle_ai))
            .on_action(cx.listener(Self::toggle_sounds))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|this, _: &NewProject, window, cx| this.file_new(crate::project_io::Template::Empty, window, cx)))
            .on_action(cx.listener(|this, _: &OpenProject, window, cx| this.file_open(window, cx)))
            .on_action(cx.listener(|this, _: &SaveProject, _, cx| this.file_save(cx)))
            .on_action(cx.listener(|this, _: &SaveProjectAs, _, cx| this.file_save_as(None, cx)))
            .on_action(cx.listener(|this, _: &SplitClip, _, cx| this.split_clips(cx)))
            .on_action(cx.listener(|this, _: &GroupTracks, _, cx| crate::editing::group_selection(this, cx)))
            .on_action(cx.listener(|this, _: &UngroupTracks, _, cx| crate::editing::ungroup_selection(this, cx)))
            .on_action(cx.listener(|this, _: &DeleteTrack, _, cx| this.delete_selected_tracks(cx)))
            .on_action(cx.listener(|this, _: &CollectAndSave, _, cx| this.collect_and_save(cx)))
            .on_action(cx.listener(|this, _: &FindMissingFiles, _, cx| this.find_missing_files(cx)))
            .on_action(cx.listener(|this, _: &CheckForUpdates, _, cx| this.check_for_updates(cx)))
            .on_action(cx.listener(|this, _: &ToggleGroupFold, _, cx| crate::editing::toggle_group_fold(this, cx)))
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| views::palette::toggle(this, window, cx)))
            .on_action(cx.listener(|this, _: &AddLibraryFolder, window, cx| views::palette::open_add_library(this, window, cx)))
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
                if this.is_typing_notes() {
                    cx.propagate();
                    return;
                }
                // Fit the whole song to the width of the lanes.
                let width = f32::from(this.regions.arrange.get().size.width).max(200.);
                let beats = (this.s.project.arrangement_end() as f32 / PPQ as f32).max(4.);
                this.s.arrange.px_per_beat = (width / beats * 0.96).clamp(crate::nav::MIN_PX_PER_BEAT, crate::nav::MAX_PX_PER_BEAT);
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
                // Shift+Tab in Live: when stacked, move focus between Clip and Devices; otherwise swap tabs.
                this.bottom_open = true;
                if this.bottom_stacked {
                    this.s.ed.area = if this.s.ed.area == crate::editing::Area::Devices {
                        crate::editing::Area::Piano
                    } else {
                        crate::editing::Area::Devices
                    };
                    cx.notify();
                } else {
                    let next = if this.s.bottom_tab == BottomTab::Devices { BottomTab::PianoRoll } else { BottomTab::Devices };
                    this.set_tab(next, cx);
                }
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
                if this.palette.open {
                    views::palette::close(this, window, cx);
                    return;
                }
                // Map mode ends with Escape.
                if views::device_ui::modulator::map_mode().is_some() {
                    views::device_ui::modulator::set_map_mode(None);
                    cx.notify();
                    return;
                }
                // A fade, trim or copy drag in the lanes is given up first.
                if views::arrange_clips::cancel_drag(this) {
                    cx.notify();
                    return;
                }
                this.menu = None;
                this.modal = if this.export.running.is_some() { this.modal } else { None };
                this.s.selected_notes.clear();
                this.s.clear_arrange_selection();
                this.s.auto_sel = None;
                this.focus.focus(window, cx);
                cx.notify();
            }))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| {
                // Clicking the workspace (outside a text field) takes the keys back.
                if !this.focus.is_focused(window) && !this.search.read(cx).focus_handle(cx).is_focused(window) && !views::ai_panel::state::input_focused(window, cx) {
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
            .children(watchdog_banner)
            .children(crash_banner)
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
            .children(num_capture)
            .children(fx_capture)
            .children(sample_capture)
            .children(fx_overlay)
            .children(meter_layers)
            .children(menu)
            .children(chooser)
            .children(param_menu)
            .children(palette), window)
    }
}

/// What the device chain needs to show every knob of the tallest card: the
/// card (title bar, body, padding) with room to spare.
pub const DEVICES_MIN_H: f32 = 280.;
/// The Clip half of the stacked detail panel never gets shorter than this.
pub const STACK_CLIP_MIN_H: f32 = 120.;
/// The divider between the stacked halves.
pub const STACK_DIVIDER_H: f32 = 6.;

/// The detail panel's height between `min` and 70 % of the window (the
/// minimum wins on a window too short for both).
pub fn bottom_h_fitted(h: f32, window_h: f32, min: f32) -> f32 {
    h.clamp(min, (window_h * 0.7).max(min))
}

/// The least height the detail panel may have for what it shows: the whole
/// device chain, alone or under the clip editor, must fit; other views keep 160 px.
pub fn bottom_min_h(stacked: bool, devices_view: bool, bar_h: f32) -> f32 {
    if stacked {
        bar_h + STACK_CLIP_MIN_H + STACK_DIVIDER_H + DEVICES_MIN_H
    } else if devices_view {
        bar_h + DEVICES_MIN_H
    } else {
        160.
    }
}

/// The stacked split (the Clip half's share of the room) kept inside the
/// range that leaves both halves their minimum height.
pub fn split_fitted(split: f32, total_h: f32) -> f32 {
    let room = (total_h - STACK_DIVIDER_H).max(1.);
    let lo = (STACK_CLIP_MIN_H / room).clamp(0.1, 0.9);
    let hi = (1. - DEVICES_MIN_H / room).clamp(lo, 0.9);
    split.clamp(lo, hi)
}

/// The arrangement keeps at least this much height when the mixer opens.
const MIN_ARRANGE_H: f32 = 200.;

/// The mixer section's height: `mixer_h`, shrunk (down to its compact
/// 160 px) so the arrangement keeps `MIN_ARRANGE_H` above the detail panel.
pub fn mixer_h_fitted(mixer_h: f32, bottom_h: f32, window_h: f32) -> f32 {
    let room = window_h - theme::m().top_bar_h - theme::m().status_bar_h - bottom_h - MIN_ARRANGE_H - 4. * SP1;
    mixer_h.min(room).max(160.)
}

/// Renders a menu anchored at `at` above a full-window click-outside backdrop.
///
/// Using an occluded backdrop instead of `on_mouse_down_out` on `menu-layer`
/// ensures cascading `.absolute()` submenus (`Open Recent`, `Add Device`)
/// extending outside the primary card's bounds are not dismissed on mouse-down
/// before their `on_click` fires.
fn menu_overlay(
    at: Point<Pixels>,
    content: impl IntoElement,
    on_dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> gpui::Deferred {
    let dismiss = Rc::new(on_dismiss);
    let backdrop = div()
        .id("menu-backdrop")
        .size_full()
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, {
            let dismiss = dismiss.clone();
            move |e, window, cx| dismiss(e, window, cx)
        })
        .on_mouse_down(gpui::MouseButton::Right, {
            let dismiss = dismiss.clone();
            move |e, window, cx| dismiss(e, window, cx)
        })
        .on_mouse_down(gpui::MouseButton::Middle, move |e, window, cx| dismiss(e, window, cx));

    deferred(
        div()
            .id("menu-overlay")
            .absolute()
            .inset_0()
            .child(backdrop)
            .child(anchored().position(at).snap_to_window_with_margin(px(SP2)).child(div().id("menu-layer").occlude().child(content))),
    )
    .with_priority(2)
}

#[cfg(test)]
mod tests {
    use super::{DEVICES_MIN_H, STACK_CLIP_MIN_H, STACK_DIVIDER_H, bottom_h_fitted, bottom_min_h, mixer_h_fitted, split_fitted};

    fn loop_item(bpm: Option<f32>, is_loop: bool) -> crate::bridge::LibraryItem {
        crate::bridge::LibraryItem {
            id: 1,
            name: "Groove".into(),
            path: std::path::PathBuf::from("/no/such/groove.flac"),
            category: "Loop".into(),
            key: None,
            bpm,
            duration: 4.0,
            is_loop,
            favorite: false,
        }
    }

    fn dropped_warp(item: &crate::bridge::LibraryItem, trust: crate::bridge::BpmTrust) -> aitrack_model::Warp {
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let cmds = super::sample_clip_commands(&mut s, item, None, 0, trust, None);
        cmds.iter()
            .find_map(|c| match c {
                aitrack_model::Command::AddSource { source, .. } => match source.as_ref() {
                    aitrack_model::Source::Audio(a) => Some(a.warp.clone()),
                    _ => None,
                },
                _ => None,
            })
            .expect("an audio source")
    }

    #[test]
    fn a_loop_with_a_guessed_bpm_is_dropped_without_warp() {
        use crate::bridge::BpmTrust;
        let item = loop_item(Some(97.0), true);
        let guessed = dropped_warp(&item, BpmTrust::Estimate);
        assert!(!guessed.enabled, "no warp to a tempo guessed from the length");
        assert_eq!(guessed.original_bpm, Some(97.0), "but the BPM is there for when the user turns warp on");
        let measured = dropped_warp(&item, BpmTrust::Reliable);
        assert!(measured.enabled);
        assert_eq!(measured.original_bpm, Some(97.0));
        // One-shots and loops without a BPM never warp.
        assert!(!dropped_warp(&loop_item(None, true), BpmTrust::Reliable).enabled);
        let one_shot = dropped_warp(&loop_item(Some(97.0), false), BpmTrust::Reliable);
        assert!(!one_shot.enabled && one_shot.original_bpm.is_none());
    }

    fn dropped_clip(item: &crate::bridge::LibraryItem, fit: Option<&crate::views::sounds::logic::DropFit>) -> (aitrack_model::Warp, aitrack_model::Tick) {
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let cmds = super::sample_clip_commands(&mut s, item, None, 0, crate::bridge::BpmTrust::Reliable, fit);
        let warp = cmds
            .iter()
            .find_map(|c| match c {
                aitrack_model::Command::AddSource { source, .. } => match source.as_ref() {
                    aitrack_model::Source::Audio(a) => Some(a.warp.clone()),
                    _ => None,
                },
                _ => None,
            })
            .expect("an audio source");
        let length = cmds.iter().find_map(|c| if let aitrack_model::Command::AddClip { clip, .. } = c { Some(clip.length) } else { None }).expect("a clip");
        (warp, length)
    }

    #[test]
    fn a_sound_dropped_with_fit_leaves_the_clip_transposed_warped_and_the_right_length() {
        use aitrack_model::{Key, PPQ, Scale, ScaleKind, WarpMode};
        let mut item = loop_item(Some(64.0), true);
        item.key = Some(Scale::new(Key(2), ScaleKind::Minor));
        item.duration = 7.5; // 8 beats at 64 BPM; read at 128 it is 16 beats of the song
        let fit = crate::views::sounds::logic::drop_fit(&item, crate::bridge::BpmTrust::Reliable, Some(Scale::new(Key(9), ScaleKind::Minor)), 128.);
        let (warp, length) = dropped_clip(&item, Some(&fit));
        assert!(warp.enabled);
        assert_eq!(warp.original_bpm, Some(128.));
        assert_eq!(warp.mode, WarpMode::Complex);
        assert_ne!(warp.transpose, 0., "D minor into A minor moves");
        // Its beats at the tempo it is read at: 7.5 s x 128 BPM = 16 beats.
        assert_eq!(length, 16 * PPQ);
        // Without fit nothing is transposed and the tempo is the file's own.
        let (plain, _) = dropped_clip(&item, None);
        assert_eq!((plain.transpose, plain.original_bpm), (0., Some(64.)));
    }

    #[test]
    fn a_transposed_one_shot_gets_a_clip_as_long_as_its_slowed_or_sped_up_sound() {
        use aitrack_model::{PPQ, Scale, Key, ScaleKind};
        let mut item = loop_item(None, false);
        item.key = Some(Scale::new(Key(9), ScaleKind::Minor));
        item.duration = 1.0;
        let flat = crate::views::sounds::logic::DropFit { transpose: -12., original_bpm: None, warp: false, mode: aitrack_model::WarpMode::Beats };
        let (warp, length) = dropped_clip(&item, Some(&flat));
        assert!(!warp.enabled && warp.transpose == -12.);
        // An octave down plays at half speed: the clip is twice as long.
        let plain = crate::state::AppState::new(aitrack_model::Project::new("t")).project.tempo.initial_bpm();
        let one_second = (plain / 60. * PPQ as f64).round() as aitrack_model::Tick;
        assert!(length >= one_second * 2 - 2 && length <= one_second * 2 + 2, "got {length}");
    }

    /// An AIFF (which `hound` cannot read, `symphonia` can) of mono 16-bit silence.
    fn write_aiff(path: &std::path::Path, rate: u32, frames: u32) {
        let mut comm = Vec::new();
        comm.extend(1i16.to_be_bytes());
        comm.extend(frames.to_be_bytes());
        comm.extend(16i16.to_be_bytes());
        let exp = 31 - rate.leading_zeros();
        comm.extend((16383u16 + exp as u16).to_be_bytes());
        comm.extend(((rate as u64) << (63 - exp)).to_be_bytes());
        let data = frames * 2;
        let mut out = Vec::new();
        out.extend(b"FORM");
        out.extend((4 + 8 + comm.len() as u32 + 8 + 8 + data).to_be_bytes());
        out.extend(b"AIFF");
        out.extend(b"COMM");
        out.extend((comm.len() as u32).to_be_bytes());
        out.extend(comm);
        out.extend(b"SSND");
        out.extend((8 + data).to_be_bytes());
        out.extend([0u8; 8]);
        out.extend(std::iter::repeat_n(0u8, data as usize));
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn a_three_minute_file_that_is_not_a_wav_enters_with_its_real_length() {
        use crate::state::BAR;
        use aitrack_model::{Command, PPQ, Source};
        let dir = std::env::temp_dir().join(format!("aitrack-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.aiff");
        write_aiff(&path, 8_000, 8_000 * 180);
        assert!(super::wav_header(&path).is_none(), "not a WAV: the header says nothing");
        let facts = super::probe_audio(&path).expect("symphonia reads it");
        assert_eq!(facts, (8_000, 1, 8_000 * 180));
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let cmds = super::audio_track_commands(&mut s, "long", aitrack_model::Color::palette(4), &path, facts, 0, 0);
        let len = cmds.iter().find_map(|c| if let Command::AddClip { clip, .. } = c { Some(clip.length) } else { None }).unwrap();
        // 180 s at 120 BPM is 360 beats, not the 4 bars of a file of unknown length.
        assert_eq!(len, 360 * PPQ);
        assert_ne!(len, 4 * BAR);
        assert!(cmds.iter().any(|c| matches!(c, Command::AddSource { source, .. } if matches!(source.as_ref(), Source::Audio(a) if a.frames == 8_000 * 180))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_corrected_length_stops_where_the_next_clip_starts() {
        use crate::bridge::AudioAnalysis;
        use aitrack_model::{Clip, Color, Command, PPQ, Placement};
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let path = std::path::Path::new("/no/such/song.mp3");
        let cmds = super::audio_track_commands(&mut s, "song", aitrack_model::Color::palette(4), path, (48_000, 2, 0), 0, 0);
        let source = cmds.iter().find_map(|c| if let Command::AddSource { id, .. } = c { Some(*id) } else { None }).unwrap();
        let clip = cmds.iter().find_map(|c| if let Command::AddClip { id, .. } = c { Some(*id) } else { None }).unwrap();
        let track = cmds.iter().find_map(|c| if let Command::AddTrack { track, .. } = c { Some(track.id) } else { None }).unwrap();
        assert!(s.edit("Import audio", cmds));
        // The user drops another clip 20 beats in (after the 4 provisional bars), while the analysis runs.
        let (c2, p2) = (s.project.alloc_clip_id(), s.project.alloc_placement_id());
        assert!(s.edit("Place", vec![
            Command::AddClip { id: c2, clip: Box::new(Clip::new("next", Color::palette(1), source, 4 * PPQ)) },
            Command::PlaceClip { track, placement: Placement { id: p2, clip: c2, start: 20 * PPQ, lane: 0 } },
        ]));
        assert_eq!(s.project.track(track).unwrap().arrangement.len(), 2);
        let analysis = AudioAnalysis { key: None, bpm: None, sample_rate: 44_100, channels: 2, frames: 44_100 * 30 };
        let fix = super::analysis_commands(&s.project, source, &analysis);
        assert!(s.edit_as("Analyse audio", aitrack_model::Origin::Feature("analysis".into()), fix));
        assert_eq!(s.project.clips[&clip].length, 20 * PPQ, "30 s would run over the next clip: it stops there");
        assert_eq!(s.project.clips[&c2].length, 4 * PPQ);
        assert_eq!(s.project.track(track).unwrap().arrangement.len(), 2, "nothing was replaced");
    }

    #[test]
    fn turning_automation_arm_off_stops_a_recording() {
        assert!(super::recording_needs_stop(true, false));
        assert!(!super::recording_needs_stop(true, true));
        assert!(!super::recording_needs_stop(false, false), "nothing recording: nothing to stop");
        // The stub engine's Record button goes dark when it is toggled off.
        use crate::bridge::EngineBridge;
        let mut e = crate::bridge::StubEngine::default();
        e.toggle_record();
        assert!(e.transport().recording);
        if super::recording_needs_stop(e.transport().recording, false) {
            e.toggle_record();
        }
        assert!(!e.transport().recording);
    }

    fn wav(dir: &std::path::Path, name: &str, seconds: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        let spec = hound::WavSpec { channels: 1, sample_rate: 8_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..8_000 * seconds {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    #[test]
    fn dropped_files_land_one_after_another_on_the_track_they_were_dropped_on() {
        use aitrack_model::{Color, Command, PPQ, Track, TrackKind};
        let dir = std::env::temp_dir().join(format!("aitrack-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<_> = [("a.wav", 1), ("b.wav", 2), ("c.wav", 1)].iter().map(|(n, s)| wav(&dir, n, *s)).collect();
        let facts: Vec<_> = files.iter().map(|f| (super::wav_header(f), f.clone())).collect();
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let id = s.project.alloc_track_id();
        assert!(s.edit("Add", vec![Command::AddTrack { track: Box::new(Track::new(id, TrackKind::Audio, "Audio", Color::palette(2))), index: 0 }]));
        let tracks = s.project.tracks.len();
        let cmds = super::drop_audio_commands(&mut s, &facts, Some(id), 4 * PPQ, 0);
        assert!(!cmds.iter().any(|c| matches!(c, Command::AddTrack { .. })), "no new track");
        assert!(s.edit("Drop audio", cmds));
        assert_eq!(s.project.tracks.len(), tracks);
        let t = s.project.track(id).unwrap();
        let mut starts: Vec<_> = t.arrangement.iter().map(|p| (p.start, s.project.clips[&p.clip].length)).collect();
        starts.sort();
        // 1 s, 2 s and 1 s at 120 BPM are 2, 4 and 2 beats, back to back from beat 4.
        assert_eq!(starts, [(4 * PPQ, 2 * PPQ), (6 * PPQ, 4 * PPQ), (10 * PPQ, 2 * PPQ)]);
        // One undo step takes them all away.
        s.undo();
        assert!(s.project.track(id).unwrap().arrangement.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_dropped_on_empty_space_make_one_new_audio_track() {
        use aitrack_model::{Command, PPQ, TrackKind};
        let dir = std::env::temp_dir().join(format!("aitrack-drop2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let facts: Vec<_> = ["x.wav", "y.wav", "z.wav"].iter().map(|n| wav(&dir, n, 1)).map(|f| (super::wav_header(&f), f)).collect();
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let before = s.project.tracks.len();
        let cmds = super::drop_audio_commands(&mut s, &facts, None, 0, 0);
        assert_eq!(cmds.iter().filter(|c| matches!(c, Command::AddTrack { .. })).count(), 1);
        assert!(s.edit("Drop audio", cmds));
        assert_eq!(s.project.tracks.len(), before + 1);
        let t = s.project.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap();
        let mut starts: Vec<_> = t.arrangement.iter().map(|p| p.start).collect();
        starts.sort();
        assert_eq!(starts, [0, 2 * PPQ, 4 * PPQ], "side by side, not stacked");
        assert!(super::is_audio_file(std::path::Path::new("/a/Kick.WAV")) && !super::is_audio_file(std::path::Path::new("/a/notes.txt")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_probe_that_finishes_in_another_project_is_dropped() {
        assert!(super::probe_is_current(3, 3));
        assert!(!super::probe_is_current(3, 4), "a project was opened while the file was probed");
    }

    #[test]
    fn the_analysis_corrects_a_provisional_clip_length() {
        use crate::bridge::AudioAnalysis;
        use crate::state::BAR;
        use aitrack_model::{Command, PPQ};
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("t"));
        // Imported without a readable header: 4 bars until the analysis knows better.
        let path = std::path::Path::new("/no/such/song.mp3");
        let cmds = super::audio_track_commands(&mut s, "song", aitrack_model::Color::palette(4), path, (48_000, 2, 0), 0, 0);
        let (source, clip) = cmds
            .iter()
            .find_map(|c| if let Command::AddClip { clip, .. } = c { Some(clip.source) } else { None })
            .map(|src| (src, cmds.iter().find_map(|c| if let Command::AddClip { id, .. } = c { Some(*id) } else { None }).unwrap()))
            .unwrap();
        assert!(s.edit("Import audio", cmds));
        assert_eq!(s.project.clips[&clip].length, 4 * BAR);
        // 30 s at 120 BPM = 60 beats.
        let analysis = AudioAnalysis { key: None, bpm: Some(100.0), sample_rate: 44_100, channels: 2, frames: 44_100 * 30 };
        let fix = super::analysis_commands(&s.project, source, &analysis);
        assert!(s.edit_as("Analyse audio", aitrack_model::Origin::Feature("analysis".into()), fix));
        assert_eq!(s.project.clips[&clip].length, 60 * PPQ, "the clip is as long as the file");
        // A clip the user already shortened keeps its length.
        let mut s2 = crate::state::AppState::new(aitrack_model::Project::new("t"));
        let cmds = super::audio_track_commands(&mut s2, "song", aitrack_model::Color::palette(4), path, (48_000, 2, 0), 0, 0);
        let clip2 = cmds.iter().find_map(|c| if let Command::AddClip { id, .. } = c { Some(*id) } else { None }).unwrap();
        let src2 = cmds.iter().find_map(|c| if let Command::AddSource { id, .. } = c { Some(*id) } else { None }).unwrap();
        assert!(s2.edit("Import audio", cmds));
        let mut shorter = s2.project.clips[&clip2].clone();
        shorter.length = BAR;
        assert!(s2.edit("Trim", vec![Command::UpdateClip { id: clip2, clip: Box::new(shorter) }]));
        let fix = super::analysis_commands(&s2.project, src2, &analysis);
        assert!(s2.edit_as("Analyse audio", aitrack_model::Origin::Feature("analysis".into()), fix));
        assert_eq!(s2.project.clips[&clip2].length, BAR);
    }

    #[test]
    fn mixer_gives_way_to_the_arrangement_on_short_windows() {
        assert_eq!(mixer_h_fitted(264., 300., 1200.), 264.);
        let short = mixer_h_fitted(264., 300., 700.);
        assert!((160. ..264.).contains(&short), "{short}");
        assert_eq!(mixer_h_fitted(264., 460., 700.), 160., "never below the compact strip");
    }

    #[test]
    fn the_device_chain_never_gets_less_than_a_card_needs() {
        let bar = 32.;
        assert_eq!(bottom_min_h(false, true, bar), bar + DEVICES_MIN_H);
        assert_eq!(bottom_min_h(true, true, bar), bar + STACK_CLIP_MIN_H + STACK_DIVIDER_H + DEVICES_MIN_H);
        assert_eq!(bottom_min_h(false, false, bar), 160., "the clip and mixer views keep their old floor");
        // A panel saved at the old 264 px is raised; a big one is left alone; a tiny window keeps the minimum.
        let min = bottom_min_h(true, true, bar);
        assert_eq!(bottom_h_fitted(264., 1000., min), min);
        assert_eq!(bottom_h_fitted(600., 1000., min), 600.);
        assert_eq!(bottom_h_fitted(600., 300., min), min);
    }

    #[test]
    fn the_stacked_split_leaves_both_halves_their_minimum() {
        for total in [406., 440., 600., 900.] {
            let room = total - STACK_DIVIDER_H;
            for want in [0., 0.25, 0.5, 0.75, 1.] {
                let s = split_fitted(want, total);
                assert!(room * s >= STACK_CLIP_MIN_H - 0.01, "clip half {} at {total}", room * s);
                assert!(room * (1. - s) >= DEVICES_MIN_H - 0.01, "devices half {} at {total}", room * (1. - s));
            }
        }
    }

    #[test]
    fn bottom_panel_stays_between_160_px_and_70_percent() {
        assert_eq!(bottom_h_fitted(50., 1000., 160.), 160.);
        assert_eq!(bottom_h_fitted(900., 1000., 160.), 700.);
        assert_eq!(bottom_h_fitted(400., 1000., 160.), 400.);
        assert_eq!(bottom_h_fitted(400., 100., 160.), 160.);
    }

    #[test]
    fn typed_values_parse_and_prefill_round_trip() {
        use super::{parse_plain, plain_text};
        assert_eq!(parse_plain("-7.6"), Some(-7.6));
        assert_eq!(parse_plain("128,5"), Some(128.5));
        assert_eq!(parse_plain(""), None);
        assert_eq!(parse_plain("-"), None);
        assert_eq!(plain_text(120.), "120");
        assert_eq!(plain_text(-7.6), "-7.6");
        assert_eq!(parse_plain(&plain_text(0.5)), Some(0.5));
    }

    #[test]
    fn num_edit_accepts_digits_shift_plus_and_key_fallback() {
        use super::num_edit_char;
        use gpui::{Keystroke, Modifiers};

        let ks = |key: &str, key_char: Option<&str>, modifiers: Modifiers| Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        };
        assert_eq!(num_edit_char(&ks("1", Some("1"), Modifiers::none())), Some("1"));
        assert_eq!(num_edit_char(&ks("0", Some("0"), Modifiers::none())), Some("0"));
        assert_eq!(num_edit_char(&ks("-", Some("-"), Modifiers::none())), Some("-"));
        assert_eq!(num_edit_char(&ks("=", Some("+"), Modifiers::shift())), Some("+"));
        assert_eq!(num_edit_char(&ks("5", None, Modifiers::none())), Some("5"));
        assert_eq!(num_edit_char(&ks("1", Some("1"), Modifiers::control())), None);
        assert_eq!(num_edit_char(&ks("1", Some("1"), Modifiers::command())), None);
        assert_eq!(num_edit_char(&ks("1", Some("1"), Modifiers::alt())), None);
        assert_eq!(num_edit_char(&ks("a", Some("a"), Modifiers::none())), None);
    }

    #[test]
    fn num_edit_key_context_suppresses_single_key_workspace_shortcuts() {
        let editing_ctx = gpui::KeyContext::parse(super::workspace_key_context(true)).unwrap();
        let normal_ctx = gpui::KeyContext::parse(super::workspace_key_context(false)).unwrap();
        let pred = gpui::KeyBindingContextPredicate::parse("Workspace && !TextInput").unwrap();
        assert!(pred.eval(&[normal_ctx]));
        assert!(!pred.eval(&[editing_ctx]));
    }

    #[test]
    fn menu_overlay_allows_clicking_cascading_submenu_and_dismisses_outside() {
        use gpui::{
            AnyWindowHandle, AppContext, Context, InputEvent, InteractiveElement, IntoElement,
            MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Render,
            StatefulInteractiveElement, Styled, TestAppContext, Window, div, point, px,
        };
        use std::cell::Cell;
        use std::rc::Rc;

        struct SubmenuHost {
            dismisses: Rc<Cell<usize>>,
            clicks: Rc<Cell<usize>>,
        }

        impl Render for SubmenuHost {
            fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                let dismisses = self.dismisses.clone();
                let clicks = self.clicks.clone();
                let submenu = div()
                    .id("test-submenu")
                    .occlude()
                    .absolute()
                    .left_full()
                    .top_0()
                    .w(px(200.))
                    .h(px(60.))
                    .child(
                        div()
                            .id("test-submenu-item")
                            .w(px(200.))
                            .h(px(40.))
                            .on_click(move |_, _, _| {
                                clicks.set(clicks.get() + 1);
                            }),
                    );
                let body = div()
                    .relative()
                    .w(px(200.))
                    .h(px(100.))
                    .child(submenu);

                div().relative().size_full().child(super::menu_overlay(
                    point(px(20.), px(20.)),
                    body,
                    move |_, _, _| {
                        dismisses.set(dismisses.get() + 1);
                    },
                ))
            }
        }

        let mut test_app = TestAppContext::single();
        let dismisses = Rc::new(Cell::new(0));
        let clicks = Rc::new(Cell::new(0));
        let window = test_app.add_window({
            let dismisses = dismisses.clone();
            let clicks = clicks.clone();
            move |_, _| SubmenuHost { dismisses, clicks }
        });
        let any_window: AnyWindowHandle = window.into();

        test_app
            .update_window(any_window, |_, window, cx| {
                window.draw(cx).clear(cx);
            })
            .unwrap();

        // Click inside the cascading submenu (x=250 is outside the 200px primary card at x=20..220).
        let in_submenu = point(px(250.), px(35.));
        test_app
            .update_window(any_window, |_, window, cx| {
                window.dispatch_event(
                    MouseDownEvent {
                        position: in_submenu,
                        button: MouseButton::Left,
                        modifiers: Default::default(),
                        click_count: 1,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseUpEvent {
                        position: in_submenu,
                        button: MouseButton::Left,
                        modifiers: Default::default(),
                        click_count: 1,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();

        assert_eq!(dismisses.get(), 0, "clicking an overflowing submenu must not dismiss the menu");
        assert_eq!(clicks.get(), 1, "submenu item on_click must fire");

        // Click outside both the primary menu and the submenu.
        test_app
            .update_window(any_window, |_, window, cx| {
                window.dispatch_event(
                    MouseDownEvent {
                        position: point(px(600.), px(500.)),
                        button: MouseButton::Left,
                        modifiers: Default::default(),
                        click_count: 1,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();

        assert_eq!(dismisses.get(), 1, "clicking outside the menu and submenu must dismiss");
    }
}
