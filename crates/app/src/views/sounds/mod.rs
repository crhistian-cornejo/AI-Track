//! The Sounds panel: a column docked beside the arrangement to find sounds,
//! hear them already adjusted to the song (stretched to its tempo, moved to its
//! key) and drag them in. The sources are the factory bank, the user's folders
//! and the folder the Splice desktop app downloads licensed sounds to (a
//! collection like any other; nothing here talks to a service).
//!
//! The decisions (the query the chips make, how a result is adjusted, the
//! synced preview) are in [`logic`]; this file is state and drawing. Rows are
//! virtualised (`uniform_list`), so ten thousand sounds scroll as smoothly as ten.

pub mod logic;
mod search;

use crate::bridge::{SoundEnv, SoundGroup, SoundKind, SoundOrigin, SoundPage, SpliceState};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::palette::peaks::MiniPeaks;
use crate::widgets::{self, TextInput, label, mono};
use crate::workspace::{DraggedSample, Drag, MenuKind, Workspace};
use gpui::{
    AppContext,
    AnyElement, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement,
    Stateful, StatefulInteractiveElement, Styled, UniformListScrollHandle, Window, canvas, div, prelude::FluentBuilder, px, uniform_list,
};
use logic::{Filters, Prefs, Side, build_query, fit_label, preview_request, row_fit, step_selection};
use std::time::{Duration, Instant};

const ROW_H: f32 = 56.;
const WAVE_W: f32 = 56.;
const GRIP_W: f32 = 4.;
/// Width in px of the volume bar.
const VOLUME_W: f32 = 96.;
/// Arrow keys wait this long on a row before it plays, so holding one to scroll does not play every row.
const SETTLE: Duration = Duration::from_millis(90);
/// How often the disk is searched for a Splice folder while there is none.
const SPLICE_PROBE: Duration = Duration::from_secs(5);
/// Folder chips shown before "More".
const FOLDER_CHIPS: usize = 3;

/// What the preview of a result was made from: if any of it changes while it
/// plays, it is rendered again.
#[derive(Clone, Debug, PartialEq)]
struct PreviewSig {
    id: u64,
    bpm: u64,
    scale: Option<aitrack_model::Scale>,
    fit: bool,
}

pub struct Sounds {
    pub prefs: Prefs,
    pub filters: Filters,
    pub query: Entity<TextInput>,
    pub focus: FocusHandle,
    pub text: String,
    pub page: SoundPage,
    pub selected: Option<usize>,
    scroll: UniformListScrollHandle,
    peaks: MiniPeaks,
    /// The results are stale: ask the library again.
    pub dirty: bool,
    last_query: Option<crate::bridge::SoundQuery>,
    /// The search thread, once the library has one to offer (stub bridges search on the UI thread: demo data, tiny).
    searcher: Option<search::Searcher>,
    searcher_tried: bool,
    settle: Option<Instant>,
    playing: Option<PreviewSig>,
    /// The Splice folder last handed to the library, whether it was there then,
    /// and when the disk was last searched for it.
    splice_folder: Option<std::path::PathBuf>,
    splice_there: bool,
    splice_probe: Option<Instant>,
    /// All the folder chips are shown, not the first few.
    folders_expanded: bool,
    analysis_asked: bool,
    loaded: bool,
    /// Dev view: pick and play the first result once there is one.
    pub play_first_pending: bool,
    /// The sound being dragged out (or added) from this panel: where it lands, it is fitted to the song.
    pub carrying: Option<u64>,
}

impl Sounds {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        let query = cx.new(|cx| TextInput::new(cx, "Search sounds", Some(Icon::Search)));
        Self {
            prefs: Prefs::load(),
            filters: Filters::default(),
            query,
            focus: cx.focus_handle(),
            text: String::new(),
            page: SoundPage::default(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            peaks: MiniPeaks::default(),
            dirty: true,
            last_query: None,
            searcher: None,
            searcher_tried: false,
            settle: None,
            playing: None,
            splice_folder: None,
            splice_there: false,
            splice_probe: None,
            folders_expanded: false,
            analysis_asked: false,
            loaded: false,
            play_first_pending: false,
            carrying: None,
        }
    }

    /// The text of the search field changed.
    pub fn typed(&mut self, text: String) {
        self.text = text;
        self.dirty = true;
        self.selected = None;
    }

    /// "Fit to song" is on: previews and drops are adjusted to the song.
    pub fn fits_song(&self) -> bool {
        self.prefs.fit && self.filters.fit
    }

    pub fn is_playing(&self) -> bool {
        self.playing.is_some()
    }
}

// ───────────────────────────── Actions ─────────────────────────────

pub fn toggle(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    ws.sounds.prefs.open = !ws.sounds.prefs.open;
    if ws.sounds.prefs.open {
        ws.sounds.dirty = true;
    } else {
        stop_preview(ws);
    }
    ws.sounds.prefs.save();
    cx.notify();
}

/// Space while the panel is the one in use: stop what plays, or play the selected sound.
/// `true` when it took the key (the song's transport then keeps out of it).
pub fn space(ws: &mut Workspace, window: &Window, cx: &mut Context<Workspace>) -> bool {
    if !ws.sounds.prefs.open {
        return false;
    }
    let focused = ws.sounds.focus.contains_focused(window, cx);
    if ws.sounds.is_playing() {
        stop_preview(ws);
        cx.notify();
        return true;
    }
    if focused && ws.sounds.selected.is_some() {
        preview_selected(ws, cx);
        return true;
    }
    false
}

pub fn stop_preview(ws: &mut Workspace) {
    if ws.sounds.playing.take().is_some() {
        ws.b.engine.preview(None);
    }
    ws.sounds.settle = None;
}

fn signature(ws: &Workspace, id: u64) -> PreviewSig {
    PreviewSig { id, bpm: ws.s.project.tempo.initial_bpm().to_bits(), scale: ws.s.project.scale, fit: ws.sounds.filters.fit && ws.sounds.prefs.fit }
}

/// Play the selected result, adjusted to the song, entering on the next bar if it runs.
pub fn preview_selected(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(hit) = ws.sounds.selected.and_then(|i| ws.sounds.page.hits.get(i)).cloned() else { return };
    let mut filters = ws.sounds.filters.clone();
    filters.fit = filters.fit && ws.sounds.prefs.fit;
    let t = ws.b.engine.transport();
    let req = preview_request(&hit, &filters, ws.s.project.scale, &ws.s.project.tempo, (t.position, t.playing));
    ws.b.engine.set_preview_volume(ws.sounds.prefs.volume);
    ws.b.engine.preview_synced(&req);
    ws.sounds.playing = Some(signature(ws, hit.item.id));
    ws.sounds.settle = None;
    cx.notify();
}

fn select(ws: &mut Workspace, i: usize, play: bool, cx: &mut Context<Workspace>) {
    ws.sounds.selected = Some(i);
    ws.sounds.scroll.scroll_to_item(i, gpui::ScrollStrategy::Nearest);
    if play {
        preview_selected(ws, cx);
    }
    cx.notify();
}

/// Add the selected sound to the project at the playhead, adjusted like a drop.
fn add_selected(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if let Some(hit) = ws.sounds.selected.and_then(|i| ws.sounds.page.hits.get(i)).cloned() {
        stop_preview(ws);
        ws.sounds.carrying = Some(hit.item.id);
        ws.add_sample_clip(&hit.item, None, None, cx);
    }
}

fn key_down(ws: &mut Workspace, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Workspace>) {
    let len = ws.sounds.page.hits.len();
    match e.keystroke.key.as_str() {
        "down" | "up" => {
            let next = step_selection(ws.sounds.selected, len, if e.keystroke.key == "down" { 1 } else { -1 });
            if let Some(i) = next {
                select(ws, i, false, cx);
                // Played once it settles: scrolling through the list is silent.
                ws.sounds.settle = Some(Instant::now());
            }
        }
        "enter" => add_selected(ws, cx),
        "escape" => {
            stop_preview(ws);
            window.focus(&ws.focus.clone(), cx);
            cx.notify();
        }
        " " => {
            space(ws, window, cx);
        }
        _ => return,
    }
    cx.stop_propagation();
}

// ───────────────────────────── Housekeeping ─────────────────────────────

/// Timer tick while the panel is open: the library's changes, the search,
/// the arrow-key settle, a preview that has to follow the song, the analysis
/// of what lacks a key or tempo. `true` when something changed.
pub fn tick(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    // The Splice folder is followed whether or not the panel is open: a sound licensed in
    // Splice is found by the palette too.
    sync_splice_folder(ws);
    if !ws.sounds.prefs.open {
        return false;
    }
    let mut changed = ws.sounds.peaks.poll();
    // A drag that ended without landing anywhere leaves nothing behind.
    if ws.sounds.carrying.is_some() && !cx.has_active_drag() {
        ws.sounds.carrying = None;
    }
    let f = &ws.sounds.filters;
    let fit_on = f.fit && ws.sounds.prefs.fit;
    let mut filters = f.clone();
    filters.fit = fit_on;
    let q = build_query(&ws.sounds.text, &filters, ws.s.project.scale, ws.s.project.tempo.initial_bpm());
    if !ws.sounds.searcher_tried {
        ws.sounds.searcher_tried = true;
        ws.sounds.searcher = ws.b.library.sound_searcher().map(search::Searcher::spawn);
    }
    // An answer from the search thread (the one asked last, never an older one).
    if let Some(page) = ws.sounds.searcher.as_mut().and_then(search::Searcher::poll) {
        show_page(ws, page);
        changed = true;
    }
    if ws.sounds.dirty || ws.sounds.last_query.as_ref() != Some(&q) {
        ws.sounds.dirty = false;
        if ws.sounds.searcher.is_some() {
            let env = SoundEnv { splice_folder: ws.sounds.splice_folder.clone(), scanning: ws.b.library.scan_status().running };
            if let Some(s) = ws.sounds.searcher.as_mut() {
                s.ask(q.clone(), env);
            }
        } else {
            let page = ws.b.library.search_sounds(&q);
            show_page(ws, page);
            changed = true;
        }
        ws.sounds.last_query = Some(q);
    }
    // What lacks a key or tempo is listened to in the background (once per session): the list never waits for it.
    if !ws.sounds.analysis_asked && ws.sounds.page.hits.iter().any(|h| h.unanalysed) && !ws.b.library.scan_status().running {
        ws.sounds.analysis_asked = true;
        let _ = ws.b.library.detect_keys();
    }
    if ws.sounds.play_first_pending && !ws.sounds.page.hits.is_empty() {
        ws.sounds.play_first_pending = false;
        select(ws, 0, true, cx);
        changed = true;
    }
    if ws.sounds.settle.is_some_and(|t| t.elapsed() >= SETTLE) {
        preview_selected(ws, cx);
        changed = true;
    }
    // The song's tempo or key moved, or "fit" was switched: the preview is made again.
    if let (Some(playing), Some(i)) = (ws.sounds.playing.clone(), ws.sounds.selected)
        && let Some(hit) = ws.sounds.page.hits.get(i)
    {
        let now = signature(ws, hit.item.id);
        if now.id == playing.id && now != playing {
            preview_selected(ws, cx);
            changed = true;
        }
    }
    changed
}

/// Keep the library's Splice collection pointed at the right folder.
fn sync_splice_folder(ws: &mut Workspace) {
    // Preferences' choice, else the one found (looked for again every few seconds while there is
    // none, so installing Splice while the app runs is noticed).
    ws.palette.load();
    let probe_due = ws.sounds.splice_probe.is_none_or(|t| t.elapsed() >= SPLICE_PROBE);
    let folder = logic::splice_folder(
        ws.palette.settings.splice_folder.clone(),
        ws.sounds.splice_folder.as_deref(),
        probe_due,
        || aitrack_library::splice::detect().map(|d| d.folder),
        aitrack_library::splice::default_folder(),
    );
    if probe_due {
        ws.sounds.splice_probe = Some(Instant::now());
    }
    let there = folder.as_deref().is_some_and(std::path::Path::is_dir);
    if !ws.sounds.loaded || folder != ws.sounds.splice_folder || there != ws.sounds.splice_there {
        ws.sounds.loaded = true;
        ws.sounds.splice_folder = folder.clone();
        ws.sounds.splice_there = there;
        ws.b.library.set_splice_folder(folder);
        ws.sounds.dirty = true;
    }
}

/// Show a page of results, keeping the selected sound selected if it is still in it.
fn show_page(ws: &mut Workspace, page: SoundPage) {
    let keep = ws.sounds.selected.and_then(|i| ws.sounds.page.hits.get(i)).map(|h| h.item.id);
    ws.sounds.page = page;
    ws.sounds.selected = keep.and_then(|id| ws.sounds.page.hits.iter().position(|h| h.item.id == id));
}

/// Is the panel animating or loading something that wants the next frame?
pub fn wants_frames(ws: &Workspace) -> bool {
    ws.sounds.prefs.open && (ws.sounds.settle.is_some() || ws.sounds.dirty || ws.sounds.searcher.as_ref().is_some_and(search::Searcher::pending))
}

// ───────────────────────────── Drawing ─────────────────────────────

/// A text chip, 20 px high.
fn chip(id: impl Into<gpui::ElementId>, text: impl Into<gpui::SharedString>, on: bool) -> Stateful<gpui::Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .border_1()
        .border_color(if on { accent_alpha(0.5) } else { gpui::transparent_black() })
        .bg(if on { accent_alpha(0.16) } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(label(text, TEXT_XS, if on { pal.accent } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
}

fn short_key(s: aitrack_model::Scale) -> String {
    use aitrack_model::ScaleKind::*;
    let suffix = match s.kind {
        Minor => "m",
        Major => "",
        k => return format!("{} {}", s.root.name(), k.label()),
    };
    format!("{}{suffix}", s.root.name())
}

/// The mini waveform of a result.
fn wave(bars: Option<std::sync::Arc<Vec<f32>>>, color: gpui::Hsla) -> AnyElement {
    let Some(bars) = bars else {
        return div().w(px(WAVE_W)).h(px(24.)).flex_none().flex().items_center().child(div().w_full().h(px(1.)).bg(content(0.1))).into_any_element();
    };
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let n = bars.len().max(1) as f32;
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            let bw = (w / n).max(1.);
            for (i, v) in bars.iter().enumerate() {
                let bh = (v.clamp(0., 1.) * h).max(1.);
                window.paint_quad(crate::paint::fill_rect(crate::paint::at(b.origin, i as f32 * bw, (h - bh) / 2., (bw - 1.).max(1.), bh), color));
            }
        },
    )
    .w(px(WAVE_W))
    .h(px(24.))
    .flex_none()
    .into_any_element()
}

/// The panel, or `None` while it is closed. `dock` says which side it is drawn on.
pub fn render(ws: &mut Workspace, side: Side, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !ws.sounds.prefs.open || ws.sounds.prefs.side != side {
        return None;
    }
    let pal = p();
    let width = ws.sounds.prefs.width;
    let song_scale = ws.s.project.scale;
    let song_bpm = ws.s.project.tempo.initial_bpm();
    let fit = ws.sounds.prefs.fit;
    let f = ws.sounds.filters.clone();

    // ── Header ──
    let header = div()
        .flex_none()
        .h(px(TAB_BAR_H + SP2))
        .px(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(icon(Icon::Waveform, 16., pal.accent))
        .child(label("Sounds", TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1())
        .child(
            widgets::icon_button("sounds-dock", if ws.sounds.prefs.side == Side::Left { Icon::PanelRight } else { Icon::PanelLeft }, false)
                .tooltip(widgets::tip("Dock on the other side", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.sounds.prefs.side = if this.sounds.prefs.side == Side::Left { Side::Right } else { Side::Left };
                    this.sounds.prefs.save();
                    cx.notify();
                })),
        )
        .child(widgets::icon_button("sounds-close", Icon::X, false).tooltip(widgets::tip("Close", Some("Ctrl+Shift+K"))).on_click(cx.listener(|this, _, _, cx| toggle(this, cx))));

    // ── Fit to song ──
    let key_text = song_scale.map(|s| format!("{} ({})", short_key(s), s.camelot().unwrap_or_default())).unwrap_or_default();
    let fit_row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP2))
        .child(widgets::switch("sounds-fit", fit).on_click(cx.listener(|this, _, _, cx| {
            this.sounds.prefs.fit = !this.sounds.prefs.fit;
            this.sounds.prefs.save();
            this.sounds.dirty = true;
            cx.notify();
        })))
        .child(label("Fit to song", TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
        .when(fit, |d| {
            d.child(chip("sounds-bpm", format!("♪ {song_bpm:.0} BPM"), f.by_bpm).on_click(cx.listener(|this, _, _, cx| {
                this.sounds.filters.by_bpm = !this.sounds.filters.by_bpm;
                cx.notify();
            })))
            .child(match song_scale {
                Some(_) => chip("sounds-key", format!("♯ {key_text}"), f.by_key)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sounds.filters.by_key = !this.sounds.filters.by_key;
                        cx.notify();
                    }))
                    .into_any_element(),
                None => chip("sounds-set-key", "Set the song's key", false)
                    .tooltip(widgets::tip("The song has no key yet: choose one to filter by it and transpose to it", None))
                    .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                        this.open_menu(MenuKind::Scale, e.position(), cx);
                    }))
                    .into_any_element(),
            })
        });
    let fit_options = div().when(fit, |d| {
        d.flex().flex_wrap().items_center().gap(px(SP1)).child(label("Key", TEXT_XS, pal.faint))
            .child(chip("sounds-key-compat", "Compatible", !f.key_exact).tooltip(widgets::tip("The song's key, its relative and its neighbours on the Camelot wheel", None)).on_click(cx.listener(|this, _, _, cx| {
                this.sounds.filters.key_exact = false;
                cx.notify();
            })))
            .child(chip("sounds-key-exact", "Exact", f.key_exact).on_click(cx.listener(|this, _, _, cx| {
                this.sounds.filters.key_exact = true;
                cx.notify();
            })))
            .child(label("BPM", TEXT_XS, pal.faint).ml(px(SP2)))
            .child(chip("sounds-tol", format!("±{:.0} %", f.bpm_tolerance), false).tooltip(widgets::tip("Click to change how far from the song's tempo (½× and 2× count)", None)).on_click(cx.listener(|this, _, _, cx| {
                this.sounds.filters.bpm_tolerance = match this.sounds.filters.bpm_tolerance as u32 {
                    0..=3 => 6.,
                    4..=6 => 12.,
                    7..=12 => 25.,
                    _ => 3.,
                };
                cx.notify();
            })))
    });

    // ── Chips: type, group, collection, tags, favourites ──
    let mut kinds = div().flex().flex_wrap().gap(px(SP1));
    for (k, name) in [(SoundKind::All, "All"), (SoundKind::Loops, "Loops"), (SoundKind::OneShots, "One-shots")] {
        kinds = kinds.child(chip(("sounds-kind", k as usize), name, f.kind == k).on_click(cx.listener(move |this, _, _, cx| {
            this.sounds.filters.kind = k;
            cx.notify();
        })));
    }
    kinds = kinds.child(
        chip("sounds-fav", "★ Favourites", f.favorites).on_click(cx.listener(|this, _, _, cx| {
            this.sounds.filters.favorites = !this.sounds.filters.favorites;
            cx.notify();
        })),
    );
    let mut groups = div().flex().flex_wrap().gap(px(SP1));
    for g in SoundGroup::ALL {
        let on = f.group == Some(g);
        groups = groups.child(chip(("sounds-group", g as usize), g.label(), on).on_click(cx.listener(move |this, _, _, cx| {
            this.sounds.filters.group = if on { None } else { Some(g) };
            cx.notify();
        })));
    }
    let mut origins = div().flex().flex_wrap().items_center().gap(px(SP1)).child(label("From", TEXT_XS, pal.faint));
    for o in SoundOrigin::ALL {
        let on = f.origin == Some(o);
        origins = origins.child(chip(("sounds-origin", o as usize), o.label(), on).on_click(cx.listener(move |this, _, _, cx| {
            this.sounds.filters.origin = if on { None } else { Some(o) };
            cx.notify();
        })));
    }
    let folders_open = ws.sounds.folders_expanded;
    let mut folders = div().flex().flex_wrap().items_center().gap(px(SP1)).child(label("In", TEXT_XS, pal.faint));
    let all_folders = ws.sounds.page.folders.clone();
    // The chosen folder always shows, even when it is not among the biggest.
    let mut shown: Vec<&crate::bridge::SoundFolder> = all_folders.iter().take(if folders_open { usize::MAX } else { FOLDER_CHIPS }).collect();
    if let Some(sel) = f.folder.as_ref()
        && !shown.iter().any(|x| x.library == sel.library && x.root == sel.root && x.under == sel.under)
    {
        shown.insert(0, sel);
    }
    for (i, fo) in shown.iter().enumerate() {
        let on = f.folder.as_ref().is_some_and(|x| x.library == fo.library && x.root == fo.root && x.under == fo.under);
        let pick = (*fo).clone();
        folders = folders.child(
            chip(("sounds-folder", i), format!("{} {}", fo.label, fo.count), on)
                .tooltip(widgets::tip_owned(fo.under.clone().unwrap_or_else(|| fo.label.clone())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sounds.filters.folder = if on { None } else { Some(pick.clone()) };
                    cx.notify();
                })),
        );
    }
    if all_folders.len() > FOLDER_CHIPS {
        let more = if folders_open { "Less".to_owned() } else { format!("More (+{})", all_folders.len() - FOLDER_CHIPS) };
        folders = folders.child(chip("sounds-folder-more", more, folders_open).on_click(cx.listener(|this, _, _, cx| {
            this.sounds.folders_expanded = !this.sounds.folders_expanded;
            cx.notify();
        })));
    }
    let mut tags = div().flex().flex_wrap().gap(px(SP1));
    for (i, (t, n)) in ws.sounds.page.tags.iter().take(8).enumerate() {
        let on = f.tag.as_deref() == Some(t.as_str());
        let tag = t.clone();
        tags = tags.child(chip(("sounds-tag", i), format!("{t} {n}"), on).on_click(cx.listener(move |this, _, _, cx| {
            this.sounds.filters.tag = if on { None } else { Some(tag.clone()) };
            cx.notify();
        })));
    }

    // ── The list ──
    let n = ws.sounds.page.hits.len();
    let list = if n == 0 {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .px(px(SP4))
            .child(label(empty_text(ws), TEXT_SM, pal.faint))
            .into_any_element()
    } else {
        uniform_list(
            "sounds-list",
            n,
            cx.processor(move |this: &mut Workspace, range: std::ops::Range<usize>, _window, cx: &mut Context<Workspace>| {
                range.map(|i| row(this, i, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&ws.sounds.scroll)
        .flex_1()
        .into_any_element()
    };

    // ── Footer: volume, counts, Splice status ──
    let volume = ws.sounds.prefs.volume;
    let status = status_line(ws);
    let footer = div()
        .flex_none()
        .px(px(SP3))
        .py(px(SP2))
        .gap(px(SP1))
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(icon(Icon::Volume, 14., pal.muted))
                .child(
                    div()
                        .id("sounds-volume")
                        .relative()
                        .w(px(VOLUME_W))
                        .h(px(16.))
                        .cursor_ew_resize()
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.begin_drag(Drag::SoundsVolume { start_x: f32::from(e.position.x), start: volume }, cx);
                        }))
                        .child(div().absolute().left(px(0.)).top(px(6.)).w(px(VOLUME_W)).h(px(4.)).rounded(px(2.)).bg(content(0.12)))
                        .child(div().absolute().left(px(0.)).top(px(6.)).w(px(VOLUME_W * volume)).h(px(4.)).rounded(px(2.)).bg(pal.accent))
                        .child(div().absolute().left(px(VOLUME_W * volume - 4.)).top(px(2.)).w(px(8.)).h(px(12.)).rounded(px(2.)).bg(pal.text))
                        .tooltip(widgets::tip("Preview volume", None)),
                )
                .child(mono(format!("{:.0}%", volume * 100.), TEXT_XS, pal.muted))
                .child(div().flex_1())
                .child(label(format!("{n}{}", if ws.sounds.page.more { "+" } else { "" }), TEXT_XS, pal.faint)),
        )
        .child(label(status.0, TEXT_XS, if status.1 { pal.solo } else { pal.faint }));

    let grip = div()
        .id("sounds-grip")
        .flex_none()
        .w(px(GRIP_W))
        .h_full()
        .cursor_ew_resize()
        .hover(|s| s.bg(accent_alpha(0.4)))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            this.begin_drag(Drag::SoundsResize { start_x: f32::from(e.position.x), start: width }, cx);
        }));
    let body = div()
        .id("sounds-panel")
        .key_context("TextInput")
        .track_focus(&ws.sounds.focus)
        .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| key_down(this, e, window, cx)))
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(
            div()
                .flex_none()
                .p(px(SP3))
                .gap(px(SP2))
                .flex()
                .flex_col()
                .child(ws.sounds.query.clone())
                .child(fit_row)
                .child(fit_options)
                .child(kinds)
                .child(groups)
                .child(origins)
                .when(!ws.sounds.page.folders.is_empty(), |d| d.child(folders))
                .when(!ws.sounds.page.tags.is_empty(), |d| d.child(tags)),
        )
        .child(div().h(px(1.)).w_full().bg(border()))
        .child(list)
        .child(footer);
    let panel = div()
        .flex_none()
        .w(px(width))
        .h_full()
        .flex()
        .bg(pane_bg(crate::appearance::Pane::Center))
        .rounded(px(r_lg()))
        .border_1()
        .border_color(border())
        .overflow_hidden();
    Some(if side == Side::Left { panel.child(body).child(grip) } else { panel.child(grip).child(body) }.into_any_element())
}

/// What to say instead of an empty list.
fn empty_text(ws: &Workspace) -> String {
    let f = &ws.sounds.filters;
    if ws.sounds.dirty {
        "Searching…".to_owned()
    } else if ws.sounds.prefs.fit && (f.by_key || f.by_bpm) && ws.sounds.text.is_empty() {
        "Nothing fits this song yet. Switch the key or BPM chip off, widen the tolerance, or add folders to the library.".to_owned()
    } else {
        "No sounds match. Add a folder to the library or switch a filter off.".to_owned()
    }
}

/// The one line about the Splice folder, and whether it is a problem. A
/// folder the user chose that is not there is a problem; one nobody chose is
/// only a hint, as most people do not use Splice at all.
fn status_line(ws: &Workspace) -> (String, bool) {
    let busy = ws.b.library.scan_status();
    let chosen = ws.palette.settings.splice_folder.is_some();
    match &ws.sounds.page.splice {
        SpliceState::Missing(path) if chosen => (format!("The Splice folder {} is not there. Choose it again in Preferences ▸ Sounds.", path.display()), true),
        SpliceState::Missing(_) => ("No Splice folder found (looked in ~/Splice and Ableton's Splice samples). Using Splice? Choose its folder in Preferences ▸ Sounds.".to_owned(), false),
        _ if busy.running => (busy.message, false),
        SpliceState::Indexed { sounds: 0, .. } => ("Splice folder found, with no sounds yet: license one in Splice and it appears here".to_owned(), false),
        SpliceState::Indexed { sounds, .. } => (format!("Splice folder: {sounds} sounds · new downloads appear on their own"), false),
        SpliceState::Scanning(_) => ("Reading the Splice folder…".to_owned(), false),
        SpliceState::Unknown => (String::new(), false),
    }
}

/// One result row.
fn row(ws: &mut Workspace, i: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(hit) = ws.sounds.page.hits.get(i).cloned() else { return div().h(px(ROW_H)).into_any_element() };
    let selected = ws.sounds.selected == Some(i);
    let song_scale = ws.s.project.scale;
    let fit_on = ws.sounds.prefs.fit && ws.sounds.filters.fit;
    let fit = row_fit(&hit, song_scale, ws.s.project.tempo.initial_bpm());
    let it = &hit.item;
    let bars = ws.sounds.peaks.get(&it.path);
    let playing = ws.sounds.playing.as_ref().is_some_and(|p| p.id == it.id);
    let in_key = it.key.zip(song_scale).is_some_and(|(a, b)| crate::workspace::keys_match(a, b));
    let sub = if fit_on { fit_label(&fit, it.is_loop) } else { String::new() };
    let (id, fav) = (it.id, it.favorite);
    let drag_item = it.clone();
    let weak = cx.entity().downgrade();
    div()
        .id(("sound", i))
        .h(px(ROW_H))
        .w_full()
        .px(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .cursor_pointer()
        .bg(if selected { accent_alpha(0.14) } else { gpui::transparent_black() })
        .border_b_1()
        .border_color(border())
        .when(!selected, |d| d.hover(|s| s.bg(content(0.05))))
        .child(icon(if it.is_loop { Icon::Loop } else { Icon::Waveform }, 14., if in_key { pal.accent } else { pal.muted }))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(label(crate::paint::fit(&it.name, 150., TEXT_MD), TEXT_MD, pal.text))
                        .when(playing, |d| d.child(icon(Icon::Volume, 12., pal.accent))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(widgets::badge(hit.origin.label(), if hit.origin == SoundOrigin::Splice { pal.accent } else { pal.faint }))
                        .child(mono(if it.duration > 0. { format!("{:.1}s", it.duration) } else { String::new() }, TEXT_XS, pal.faint))
                        .when(!sub.is_empty(), |d| d.child(mono(sub, TEXT_XS, if fit.key.is_big() { pal.solo } else { pal.accent }))),
                ),
        )
        .child(wave(bars, if selected { pal.accent } else { content(0.45) }))
        .child(
            div()
                .flex_none()
                .w(px(48.))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(2.))
                .child(mono(it.bpm.map_or(if hit.unanalysed && it.is_loop { "…".to_owned() } else { "–".to_owned() }, |b| format!("{b:.0}")), TEXT_SM, pal.muted))
                .child(mono(it.key.map_or(if hit.unanalysed { "…".to_owned() } else { "–".to_owned() }, short_key), TEXT_SM, if in_key { pal.accent } else { pal.muted })),
        )
        .child(
            div()
                .id(("sound-fav", i))
                .flex_none()
                .child(if fav { icon(Icon::StarFilled, 12., pal.solo) } else { icon(Icon::Star, 12., content(0.2)) })
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.b.library.set_favorite(id, !fav);
                    this.sounds.dirty = true;
                    cx.notify();
                })),
        )
        .tooltip(widgets::tip_owned(format!("{}\n{}\nClick to hear it · Enter or double-click to add · drag onto the arrangement or a Session slot", it.category, it.path.display())))
        .on_drag(DraggedSample(drag_item), move |d: &DraggedSample, _, _, cx| {
            let carried = d.0.id;
            let _ = weak.update(cx, |ws, cx| {
                stop_preview(ws);
                ws.sounds.carrying = Some(carried);
                cx.notify();
            });
            cx.new(|_| d.clone())
        })
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            this.sounds.focus.focus(window, cx);
            select(this, i, true, cx);
            if e.click_count >= 2 {
                add_selected(this, cx);
            }
        }))
        .into_any_element()
}

/// The volume bar dragged by `dx` px from where it started.
pub fn volume_after_drag(start: f32, dx: f32) -> f32 {
    (start + dx / VOLUME_W).clamp(0., 1.)
}

/// The panel's width after its edge was dragged by `dx` px (toward the middle is wider
/// for a left dock, narrower for a right one).
pub fn width_after_drag(start: f32, dx: f32, side: Side) -> f32 {
    let d = if side == Side::Left { dx } else { -dx };
    (start + d).clamp(logic::MIN_WIDTH, logic::MAX_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volume_and_the_width_follow_their_drags_inside_their_limits() {
        assert_eq!(volume_after_drag(0.5, VOLUME_W / 4.), 0.75);
        assert_eq!(volume_after_drag(0.9, 1000.), 1.);
        assert_eq!(volume_after_drag(0.1, -1000.), 0.);
        assert_eq!(width_after_drag(336., 40., Side::Left), 376.);
        assert_eq!(width_after_drag(336., 40., Side::Right), 296., "a right dock grows to the left");
        assert_eq!(width_after_drag(336., 4000., Side::Left), logic::MAX_WIDTH);
        assert_eq!(width_after_drag(336., -4000., Side::Left), logic::MIN_WIDTH);
    }

    #[test]
    fn keys_read_the_way_people_write_them() {
        use aitrack_model::{Key, Scale, ScaleKind};
        assert_eq!(short_key(Scale::new(Key(9), ScaleKind::Minor)), "Am");
        assert_eq!(short_key(Scale::new(Key(0), ScaleKind::Major)), "C");
        assert_eq!(short_key(Scale::new(Key(2), ScaleKind::Dorian)), "D Dorian");
    }
}
