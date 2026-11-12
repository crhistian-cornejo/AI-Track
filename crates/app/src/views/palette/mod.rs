//! The Cmd+K spotlight palette: one search box over samples (factory bank,
//! user library, installed Content Hub packs, the Splice folder and, opted
//! in, Splice itself), devices (stock, VST3, CLAP), their presets, the
//! songs-key matches, Content Hub packs and every command. It replaces the
//! old browser sidebar. Opened with [`crate::actions::TogglePalette`];
//! drawn above everything else by `Workspace::render`.
//!
//! Arrow keys move through the results and audition samples; Enter puts
//! the result on the selected track (samples as clips, devices and presets
//! on its chain, all through commands) or runs the command; samples drag
//! onto the arrangement; Escape closes. Searches run off the UI thread
//! ([`search`], [`aitrack_splice::Service`]); waveforms and thumbnails too.

pub mod packs;
pub mod peaks;
pub mod search;
pub mod settings;
pub mod splice;
pub mod thumbs;

use crate::audition::SampleAudition;
use crate::bridge::ExternalPlugin;
use crate::icons::{Icon, icon};
use crate::keys::{Bound, TABLE};
use crate::motion;
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::{DraggedSample, Workspace, keys_match};
use aitrack_model::{PluginFormat, Scale, ScaleKind};
use aitrack_splice::SpliceSample;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Focusable as _, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, ScrollHandle, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, img, prelude::FluentBuilder, px,
};
use packs::{InstallState, PackKind};
use search::{KindFilter, SampleQuery, SampleRow};
use std::sync::Arc;
use std::time::Instant;

const CARD_W: f32 = 880.;
const LIST_H: f32 = 432.;
const SAMPLE_ROW_H: f32 = 44.;
const ITEM_ROW_H: f32 = 40.;
const HEADER_H: f32 = 24.;
const WAVE_W: f32 = 120.;
const TIME_W: f32 = 48.;
const KEY_W: f32 = 44.;
const BPM_W: f32 = 44.;
/// Rows per section on the All tab.
const ALL_COMMANDS: usize = 4;
const ALL_DEVICES: usize = 6;
const ALL_PRESETS: usize = 4;
const ALL_SAMPLES: usize = 60;
const ALL_SPLICE: usize = 12;
const BPM_STEP: u32 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    All,
    Samples,
    InKey,
    Splice,
    Devices,
    Presets,
    Commands,
    Packs,
}

impl Tab {
    fn icon(self) -> Icon {
        match self {
            Tab::All => Icon::Search,
            Tab::Samples => Icon::Waveform,
            Tab::InKey => Icon::MusicNote,
            Tab::Splice => Icon::Globe,
            Tab::Devices => Icon::Plug,
            Tab::Presets => Icon::Sliders,
            Tab::Commands => Icon::Keyboard,
            Tab::Packs => Icon::Package,
        }
    }

    /// Tabs that list samples (and so show the sample filters).
    fn samples(self) -> bool {
        matches!(self, Tab::Samples | Tab::InKey | Tab::Splice)
    }
}

/// Column the sample list is ordered by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortCol {
    /// The search's own ranking ("Most relevant").
    #[default]
    Relevant,
    Name,
    Time,
    Key,
    Bpm,
}

#[derive(Clone, Debug)]
pub enum DeviceRef {
    Stock(&'static str),
    Plugin(Box<ExternalPlugin>),
}

#[derive(Clone, Debug)]
pub struct DeviceRow {
    pub dev: DeviceRef,
    pub name: String,
    pub vendor: String,
    pub category: String,
    pub instrument: bool,
    pub format: &'static str,
}

impl DeviceRow {
    fn thumb_id(&self) -> String {
        match &self.dev {
            DeviceRef::Stock(id) => (*id).to_owned(),
            DeviceRef::Plugin(p) => p.uid.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PresetRow {
    pub plugin_id: &'static str,
    pub plugin: &'static str,
    pub path: String,
    pub name: String,
    pub category: String,
}

#[derive(Clone, Debug)]
pub enum Item {
    Sample(Box<SampleRow>),
    Splice(Box<SpliceSample>),
    Device(DeviceRow),
    Preset(PresetRow),
    /// Index into [`crate::keys::TABLE`].
    Command(usize),
    /// Index into the pack list.
    Pack(usize),
}

#[derive(Clone, Debug)]
pub enum Row {
    Header(&'static str, usize),
    Item(Item),
}

/// Open state, the typed query, the filters and the current results.
#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    /// Index into [`PaletteState::rows`] (always an item, never a header).
    pub selected: usize,
    pub tab: Tab,
    pub kind: KindFilter,
    pub bpm: Option<(u32, u32)>,
    pub tags: Vec<String>,
    pub sort: SortCol,
    pub ascending: bool,
    pub shuffle: Option<u64>,
    pub rows: Vec<Row>,
    samples: Vec<SampleRow>,
    sample_tags: Vec<(String, usize)>,
    generation: u64,
    answered: u64,
    no_index: bool,
    worker: Option<search::Worker>,
    peaks: Option<peaks::MiniPeaks>,
    thumbs: Option<thumbs::Thumbs>,
    pub packs: Option<packs::Packs>,
    pub splice: Option<splice::SpliceUi>,
    pub settings: settings::PaletteSettings,
    loaded: bool,
    presets: Option<Vec<PresetRow>>,
    scroll: ScrollHandle,
    pub audition: SampleAudition,
}

impl PaletteState {
    /// The local sample results (Organize on disk works on them).
    pub fn sample_items(&self) -> Vec<crate::bridge::LibraryItem> {
        self.samples.iter().map(SampleRow::item).collect()
    }

    fn selected_item(&self) -> Option<&Item> {
        match self.rows.get(self.selected)? {
            Row::Item(i) => Some(i),
            Row::Header(..) => None,
        }
    }

    /// Load settings once (first open, or Preferences).
    pub fn load(&mut self) {
        if !self.loaded {
            self.loaded = true;
            self.settings = settings::PaletteSettings::load();
        }
    }

    /// Splice was switched on or off in Preferences.
    pub fn set_splice(&mut self, on: bool) {
        self.load();
        self.settings.splice = on;
        self.settings.save();
        if on {
            self.splice.get_or_insert_with(splice::SpliceUi::new);
        } else {
            self.splice = None;
            if self.tab == Tab::Splice {
                self.tab = Tab::All;
            }
        }
    }

    pub fn splice_ui(&mut self) -> Option<&mut splice::SpliceUi> {
        self.load();
        if self.settings.splice {
            Some(self.splice.get_or_insert_with(splice::SpliceUi::new))
        } else {
            None
        }
    }
}

// ───────────────────────────── Ranking ─────────────────────────────

/// Every typed word must appear in the name or the extras; a name prefix
/// counts most. `None` = no match.
pub fn text_score(q: &str, name: &str, extra: &[&str]) -> Option<f32> {
    let name = name.to_lowercase();
    let mut score = 0.;
    for w in q.split_whitespace().map(str::to_lowercase) {
        if name.starts_with(&w) {
            score += 3.;
        } else if name.split(|c: char| !c.is_alphanumeric()).any(|p| p.starts_with(&w)) {
            score += 2.5;
        } else if name.contains(&w) {
            score += 2.;
        } else if extra.iter().any(|e| e.to_lowercase().contains(&w)) {
            score += 1.;
        } else {
            return None;
        }
    }
    Some(score)
}

fn ranked<T>(items: impl Iterator<Item = (f32, T)>) -> Vec<T> {
    let mut v: Vec<(f32, T)> = items.collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.into_iter().map(|(_, t)| t).collect()
}

/// Sort keys of a sample row: name, seconds, pitch class, BPM.
type SortKey = (String, f32, Option<u8>, Option<f32>);

/// Order rows by a column (or shuffle them with `seed`).
pub fn order<T>(v: &mut [T], sort: SortCol, ascending: bool, shuffle: Option<u64>, key: impl Fn(&T) -> SortKey) {
    if let Some(seed) = shuffle {
        let h = |s: &str| s.bytes().fold(seed ^ 0x9e3779b97f4a7c15, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
        v.sort_by_cached_key(|t| h(&key(t).0));
        return;
    }
    let cmp_opt = |a: Option<f32>, b: Option<f32>| match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        // Rows without a value go last either way.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    let dir = |o: std::cmp::Ordering| if ascending { o } else { o.reverse() };
    match sort {
        SortCol::Relevant => {}
        SortCol::Name => v.sort_by(|a, b| dir(key(a).0.to_lowercase().cmp(&key(b).0.to_lowercase()))),
        SortCol::Time => v.sort_by(|a, b| dir(key(a).1.total_cmp(&key(b).1))),
        SortCol::Key => v.sort_by(|a, b| {
            let (x, y) = (key(a).2.map(f32::from), key(b).2.map(f32::from));
            if x.is_none() || y.is_none() { cmp_opt(x, y) } else { dir(cmp_opt(x, y)) }
        }),
        SortCol::Bpm => v.sort_by(|a, b| {
            let (x, y) = (key(a).3, key(b).3);
            if x.is_none() || y.is_none() { cmp_opt(x, y) } else { dir(cmp_opt(x, y)) }
        }),
    }
}

/// "Am", "C", "F#m".
pub fn short_key(s: Scale) -> String {
    let minor = matches!(s.kind, ScaleKind::Minor | ScaleKind::HarmonicMinor | ScaleKind::MelodicMinor | ScaleKind::Dorian | ScaleKind::Phrygian | ScaleKind::MinorPentatonic);
    format!("{}{}", s.root.name(), if minor { "m" } else { "" })
}

fn first_item(rows: &[Row]) -> usize {
    rows.iter().position(|r| matches!(r, Row::Item(_))).unwrap_or(0)
}

/// The next item from `from` in direction `delta`, skipping headers; stays put at the ends.
pub fn step(rows: &[Row], from: usize, delta: i32) -> usize {
    let mut i = from as i64;
    loop {
        i += i64::from(delta.signum());
        if i < 0 || i >= rows.len() as i64 {
            return from;
        }
        if matches!(rows[i as usize], Row::Item(_)) {
            return i as usize;
        }
    }
}

// ───────────────────────────── Flow ─────────────────────────────

fn ensure(ws: &mut Workspace) {
    let pal = &mut ws.palette;
    pal.load();
    if pal.worker.is_none() {
        let factory = crate::real::library::find_factory_db(crate::real::settings::Settings::load().content_dir.as_deref());
        let user = crate::appearance::config_dir().join("library.db");
        let stubs = std::env::var_os("AITRACK_STUBS").is_some();
        pal.no_index = stubs;
        pal.worker = Some(if stubs { search::Worker::spawn(None, std::path::PathBuf::new()) } else { search::Worker::spawn(factory, user) });
    }
    pal.peaks.get_or_insert_with(Default::default);
    pal.thumbs.get_or_insert_with(Default::default);
    pal.packs.get_or_insert_with(Default::default);
    if pal.presets.is_none() {
        let mut list = Vec::new();
        for d in ws.b.plugins.stock_devices() {
            for p in ws.b.plugins.presets(d.id) {
                list.push(PresetRow { plugin_id: d.id, plugin: d.name, path: p.path, name: p.name, category: p.category });
            }
        }
        ws.palette.presets = Some(list);
    }
}

pub fn open(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    ensure(ws);
    ws.palette.open = true;
    ws.palette.selected = 0;
    ws.menu = None;
    let focus = ws.search.read(cx).focus_handle(cx);
    focus.focus(window, cx);
    refresh(ws);
    cx.notify();
}

pub fn close(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    ws.palette.open = false;
    if ws.palette.audition.stop() {
        ws.b.engine.preview(None);
    }
    ws.focus.focus(window, cx);
    cx.notify();
}

pub fn toggle(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if ws.palette.open { close(ws, window, cx) } else { open(ws, window, cx) }
}

/// The search field changed.
pub fn set_query(ws: &mut Workspace, q: &str) {
    if ws.palette.query == q {
        return;
    }
    ws.palette.query = q.to_owned();
    ws.palette.selected = 0;
    if ws.palette.open {
        refresh(ws);
    }
}

fn wants_local_samples(tab: Tab) -> bool {
    matches!(tab, Tab::All | Tab::Samples | Tab::InKey)
}

/// Ask the workers for what the query and filters need, then rebuild.
pub fn refresh(ws: &mut Workspace) {
    let key = ws.s.project.scale;
    let pal = &mut ws.palette;
    let text = pal.query.trim().to_owned();
    let in_key = pal.tab == Tab::InKey;
    if wants_local_samples(pal.tab)
        && let Some(w) = &pal.worker
    {
        pal.generation += 1;
        let all = pal.tab == Tab::All;
        w.query(SampleQuery {
            generation: pal.generation,
            text: text.clone(),
            kind: if all { KindFilter::All } else { pal.kind },
            bpm: if all { None } else { pal.bpm },
            key: if in_key { key } else { None },
            tags: if all { Vec::new() } else { pal.tags.clone() },
        });
    }
    let (kind, bpm, tags, shuffle, tab) = (pal.kind, pal.bpm, pal.tags.clone(), pal.shuffle, pal.tab);
    if (tab == Tab::Splice || (tab == Tab::All && !text.is_empty()))
        && let Some(sp) = pal.splice_ui()
    {
        let splice_tags: Vec<String> = if tab == Tab::Splice { sp.tags.iter().filter(|t| tags.contains(&t.label.to_lowercase())).map(|t| t.uuid.clone()).collect() } else { Vec::new() };
        let p = if tab == Tab::All { splice::params(&text, KindFilter::All, None, None, &[], None) } else { splice::params(&text, kind, bpm, None, &splice_tags, shuffle) };
        sp.search(p);
    }
    if ws.palette.no_index && wants_local_samples(ws.palette.tab) {
        bridge_search(ws);
    }
    rebuild(ws);
}

fn device_rows(ws: &Workspace, q: &str) -> Vec<DeviceRow> {
    let stock = ws.b.plugins.stock_devices().iter().filter_map(|d| {
        let cat = d.category.label();
        let mut extra: Vec<&str> = d.tags.to_vec();
        extra.push(cat);
        extra.push("stock");
        let s = text_score(q, d.name, &extra)?;
        Some((s + 0.1, DeviceRow { dev: DeviceRef::Stock(d.id), name: d.name.to_owned(), vendor: "AI-Track".into(), category: cat.to_owned(), instrument: d.is_instrument, format: "Stock" }))
    });
    let ext = ws.b.plugins.external_plugins().iter().filter_map(|p| {
        let format = match p.format {
            PluginFormat::Vst3 => "VST3",
            PluginFormat::Clap => "CLAP",
            #[allow(unreachable_patterns)]
            _ => "Plugin",
        };
        let s = text_score(q, &p.name, &[&p.vendor, format, if p.is_instrument { "instrument" } else { "effect" }])?;
        Some((s, DeviceRow { dev: DeviceRef::Plugin(Box::new(p.clone())), name: p.name.clone(), vendor: p.vendor.clone(), category: if p.is_instrument { "Instrument" } else { "Effect" }.into(), instrument: p.is_instrument, format }))
    });
    ranked(stock.chain(ext))
}

fn preset_rows(ws: &Workspace, q: &str) -> Vec<PresetRow> {
    let Some(list) = &ws.palette.presets else { return Vec::new() };
    ranked(list.iter().filter_map(|p| Some((text_score(q, &p.name, &[p.plugin, &p.category, "preset"])?, p.clone()))))
}

fn command_rows(q: &str) -> Vec<usize> {
    ranked(TABLE.iter().enumerate().filter(|(_, e)| e.bound != Bound::Action("TogglePalette")).filter_map(|(i, e)| Some((text_score(q, e.label, &[e.help, e.category.label()])?, i))))
}

fn pack_rows(ws: &Workspace, q: &str) -> Vec<usize> {
    let Some(p) = &ws.palette.packs else { return Vec::new() };
    ranked(p.entries.iter().enumerate().filter_map(|(i, e)| {
        let tags: Vec<&str> = e.tags.iter().map(String::as_str).chain([e.author.as_str(), e.license.as_str()]).collect();
        Some((text_score(q, &e.name, &tags)?, i))
    }))
}

fn sample_key(r: &SampleRow) -> SortKey {
    (r.name.clone(), r.duration, r.key.map(|k| k.root.0), r.bpm)
}

fn splice_key(s: &SpliceSample) -> SortKey {
    (s.name.clone(), s.duration, splice::scale_of(s).map(|k| k.root.0), s.bpm)
}

/// Rebuild the row list from what has arrived.
pub fn rebuild(ws: &mut Workspace) {
    let q = ws.palette.query.trim().to_owned();
    let tab = ws.palette.tab;
    let (sort, asc, shuffle) = (ws.palette.sort, ws.palette.ascending, ws.palette.shuffle);
    let mut rows: Vec<Row> = Vec::new();
    let section = |rows: &mut Vec<Row>, title: &'static str, items: Vec<Item>, cap: usize, header: bool| {
        if items.is_empty() {
            return;
        }
        if header {
            rows.push(Row::Header(title, items.len()));
        }
        rows.extend(items.into_iter().take(cap).map(Row::Item));
    };
    let samples = || {
        let mut s = ws.palette.samples.clone();
        if tab != Tab::All {
            order(&mut s, sort, asc, shuffle, sample_key);
        }
        s.into_iter().map(|r| Item::Sample(Box::new(r))).collect::<Vec<_>>()
    };
    let splice_items = |ws: &Workspace| {
        let Some(sp) = ws.palette.splice.as_ref().filter(|_| ws.palette.settings.splice) else { return Vec::new() };
        let mut s = sp.results.clone();
        if tab == Tab::Splice {
            let tags = &ws.palette.tags;
            s.retain(|x| tags.iter().all(|t| x.tags.iter().any(|g| g.eq_ignore_ascii_case(t))));
            order(&mut s, sort, asc, None, splice_key);
        }
        s.into_iter().map(|x| Item::Splice(Box::new(x))).collect::<Vec<_>>()
    };
    match tab {
        Tab::All => {
            if !q.is_empty() {
                section(&mut rows, "Commands", command_rows(&q).into_iter().map(Item::Command).collect(), ALL_COMMANDS, true);
            }
            section(&mut rows, "Devices", device_rows(ws, &q).into_iter().map(Item::Device).collect(), ALL_DEVICES, true);
            if !q.is_empty() {
                section(&mut rows, "Presets", preset_rows(ws, &q).into_iter().map(Item::Preset).collect(), ALL_PRESETS, true);
            }
            section(&mut rows, "Samples", samples(), ALL_SAMPLES, true);
            if !q.is_empty() {
                section(&mut rows, "Splice", splice_items(ws), ALL_SPLICE, true);
                section(&mut rows, "Packs", pack_rows(ws, &q).into_iter().map(Item::Pack).collect(), 4, true);
            }
        }
        Tab::Samples | Tab::InKey => section(&mut rows, "", samples(), search::SHOWN, false),
        Tab::Splice => section(&mut rows, "", splice_items(ws), 100, false),
        Tab::Devices => section(&mut rows, "", device_rows(ws, &q).into_iter().map(Item::Device).collect(), 400, false),
        Tab::Presets => section(&mut rows, "", preset_rows(ws, &q).into_iter().map(Item::Preset).collect(), 400, false),
        Tab::Commands => section(&mut rows, "", command_rows(&q).into_iter().map(Item::Command).collect(), 400, false),
        Tab::Packs => section(&mut rows, "", pack_rows(ws, &q).into_iter().map(Item::Pack).collect(), 400, false),
    }
    let pal = &mut ws.palette;
    pal.rows = rows;
    if !matches!(pal.rows.get(pal.selected), Some(Row::Item(_))) {
        pal.selected = first_item(&pal.rows);
    }
}

/// Search the library bridge directly (stub bridges have no index).
fn bridge_search(ws: &mut Workspace) {
    ws.palette.answered = ws.palette.generation;
    let key = if ws.palette.tab == Tab::InKey { ws.s.project.scale } else { None };
    let q = ws.palette.query.trim().to_owned();
    let mut items = if key.is_some() && q.is_empty() { ws.b.library.recommend(key, ws.s.project.tempo.initial_bpm()) } else { ws.b.library.search(&q, key) };
    if let Some(k) = key {
        items.retain(|it| it.key.is_some_and(|x| keys_match(x, k)));
    }
    let all = ws.palette.tab == Tab::All;
    let (kind, bpm, tags) = (ws.palette.kind, ws.palette.bpm, ws.palette.tags.clone());
    let rows: Vec<SampleRow> = items
        .iter()
        .map(SampleRow::from_item)
        .filter(|r| all || kind == KindFilter::All || (kind == KindFilter::Loops) == r.is_loop)
        .filter(|r| all || bpm.is_none_or(|(a, b)| r.bpm.is_some_and(|x| x >= a.min(b) as f32 && x <= a.max(b) as f32)))
        .collect();
    let q = SampleQuery { text: ws.palette.query.clone(), tags: if all { Vec::new() } else { tags }, ..Default::default() };
    let (rows, tags) = search::finish(rows, &q);
    ws.palette.samples = rows;
    ws.palette.sample_tags = tags;
}

/// Housekeeping tick: results, waveforms, thumbnails, installs, Splice.
pub fn poll(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let mut changed = false;
    let mut rebuild_rows = false;
    if let Some(w) = &ws.palette.worker {
        for reply in w.poll() {
            match reply {
                search::Reply::NoIndex => ws.palette.no_index = true,
                search::Reply::Samples { generation, rows, tags } => {
                    if generation >= ws.palette.answered {
                        ws.palette.answered = generation;
                        ws.palette.samples = rows;
                        ws.palette.sample_tags = tags;
                        rebuild_rows = true;
                    }
                }
            }
        }
    }
    // Stub bridges have no index: search them on this thread (demo data, tiny).
    if ws.palette.no_index && ws.palette.open && ws.palette.answered != ws.palette.generation {
        bridge_search(ws);
        rebuild_rows = true;
    }
    if let Some(p) = &mut ws.palette.peaks {
        changed |= p.poll();
    }
    if let Some(t) = &mut ws.palette.thumbs {
        changed |= t.poll();
    }
    if let Some(p) = &mut ws.palette.packs {
        let (c, index, messages) = p.poll();
        changed |= c || p.running();
        for dir in index {
            if let Err(e) = ws.b.library.add_folder(&dir) {
                ws.toast(e, cx);
            }
        }
        for m in messages {
            ws.toast(m, cx);
        }
    }
    if let Some(sp) = &mut ws.palette.splice {
        let out = sp.poll();
        if out.changed {
            changed = true;
            rebuild_rows |= ws.palette.tab == Tab::Splice || ws.palette.tab == Tab::All;
        }
        if let Some(path) = out.play
            && ws.palette.open
        {
            ws.b.engine.preview(Some(&path));
        }
        if let Some(url) = out.open_url {
            cx.open_url(&url);
        }
        if let Some(m) = out.message {
            ws.toast(m, cx);
        }
    }
    if let Some(path) = ws.palette.audition.due(Instant::now()) {
        ws.b.engine.preview(Some(&path));
    }
    changed |= ws.palette.audition.is_pending();
    if rebuild_rows {
        rebuild(ws);
        changed = true;
    }
    changed && ws.palette.open
}

/// Audition the selected row (samples and Splice previews).
fn audition_selected(ws: &mut Workspace, immediate: bool) {
    match ws.palette.selected_item().cloned() {
        Some(Item::Sample(r)) => {
            if let Some(path) = ws.palette.audition.select(&r.path, immediate, Instant::now()) {
                ws.b.engine.preview(Some(&path));
            }
        }
        Some(Item::Splice(s)) => {
            ws.palette.audition.stop();
            if let Some(sp) = &mut ws.palette.splice
                && sp.previewing.as_deref() != Some(s.uuid.as_str())
            {
                sp.preview(&s);
            }
        }
        _ => {
            if ws.palette.audition.stop() {
                ws.b.engine.preview(None);
            }
        }
    }
}

fn move_selection(ws: &mut Workspace, delta: i32, cx: &mut Context<Workspace>) {
    let next = step(&ws.palette.rows, ws.palette.selected, delta);
    if next != ws.palette.selected {
        ws.palette.selected = next;
        ws.palette.scroll.scroll_to_item(next);
        audition_selected(ws, false);
        cx.notify();
    }
}

fn select(ws: &mut Workspace, i: usize, cx: &mut Context<Workspace>) {
    ws.palette.selected = i;
    audition_selected(ws, true);
    cx.notify();
}

fn set_tab(ws: &mut Workspace, tab: Tab, cx: &mut Context<Workspace>) {
    if ws.palette.tab != tab {
        ws.palette.tab = tab;
        ws.palette.selected = 0;
        ws.palette.tags.clear();
        ws.palette.scroll.scroll_to_item(0);
        refresh(ws);
    }
    cx.notify();
}

fn visible_tabs(ws: &Workspace) -> Vec<Tab> {
    let mut t = vec![Tab::All, Tab::Samples, Tab::InKey];
    if ws.palette.settings.splice {
        t.push(Tab::Splice);
    }
    t.extend([Tab::Devices, Tab::Presets, Tab::Commands, Tab::Packs]);
    t
}

fn tab_label(ws: &Workspace, tab: Tab) -> String {
    match tab {
        Tab::All => "All".into(),
        Tab::Samples => "Samples".into(),
        Tab::InKey => ws.s.project.scale.map_or("In key".into(), |s| format!("In {}", short_key(s))),
        Tab::Splice => "Splice".into(),
        Tab::Devices => "Devices".into(),
        Tab::Presets => "Presets".into(),
        Tab::Commands => "Commands".into(),
        Tab::Packs => "Packs".into(),
    }
}

fn cycle_tab(ws: &mut Workspace, delta: i32, cx: &mut Context<Workspace>) {
    let tabs = visible_tabs(ws);
    let i = tabs.iter().position(|t| *t == ws.palette.tab).unwrap_or(0) as i32;
    let n = tabs.len() as i32;
    set_tab(ws, tabs[((i + delta) % n + n) as usize % tabs.len()], cx);
}

/// Put a device on the selected track's chain (one command).
fn add_device(ws: &mut Workspace, d: &DeviceRow, cx: &mut Context<Workspace>) -> bool {
    let Some(track) = ws.s.selected_track else {
        ws.toast("Select a track first", cx);
        return false;
    };
    let added = match &d.dev {
        DeviceRef::Stock(id) => ws.s.add_stock_device(track, id, &d.name).is_some(),
        DeviceRef::Plugin(p) => ws.s.add_plugin_device(track, p).is_some(),
    };
    if added {
        ws.bottom_open = true;
        ws.set_tab(crate::state::BottomTab::Devices, cx);
        ws.toast(format!("Added {}", d.name), cx);
    }
    added
}

/// Enter: act on the selected row.
fn activate(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(item) = ws.palette.selected_item().cloned() else { return };
    match item {
        Item::Sample(r) => {
            close(ws, window, cx);
            ws.add_sample_clip(&r.item(), None, None, cx);
        }
        Item::Splice(s) => {
            // Licensing goes through the user's Splice account; the Splice
            // folder is indexed, so the sound shows up here once downloaded.
            cx.open_url(&aitrack_splice::page_url(&s));
            ws.toast("Opened on Splice: license it there and it lands in your Splice folder, which the library indexes", cx);
        }
        Item::Device(d) => {
            if add_device(ws, &d, cx) {
                close(ws, window, cx);
            }
        }
        Item::Preset(p) => {
            let Some(track) = ws.s.selected_track else {
                ws.toast("Select a track first", cx);
                return;
            };
            match ws.b.plugins.load_preset(&p.path) {
                Ok((name, params)) => {
                    if let Some(dev) = ws.s.add_stock_device(track, p.plugin_id, p.plugin) {
                        ws.s.load_preset(dev, &name, &params);
                        ws.bottom_open = true;
                        ws.set_tab(crate::state::BottomTab::Devices, cx);
                        ws.toast(format!("Added {} · {name}", p.plugin), cx);
                        close(ws, window, cx);
                    }
                }
                Err(e) => ws.toast(e, cx),
            }
        }
        Item::Command(i) => {
            close(ws, window, cx);
            match TABLE[i].bound {
                Bound::Cmd(c) => crate::commands::run(ws, c, window, cx),
                Bound::Action(name) => match cx.build_action(&format!("aitrack::{name}"), None) {
                    Ok(a) => window.dispatch_action(a, cx),
                    Err(e) => log::warn!("palette: {name}: {e}"),
                },
            }
        }
        Item::Pack(i) => pack_action(ws, i, window, cx),
    }
    cx.notify();
}

fn pack_action(ws: &mut Workspace, i: usize, _: &mut Window, cx: &mut Context<Workspace>) {
    let Some(packs) = &mut ws.palette.packs else { return };
    let Some(e) = packs.entries.get(i).cloned() else { return };
    match &e.kind {
        PackKind::Optional { .. } => match packs.state(&e.id) {
            InstallState::NotInstalled | InstallState::Failed(_) => {
                packs.install(&e.id);
                ws.toast(format!("Downloading {}", e.name), cx);
            }
            InstallState::Running { .. } => packs.cancel(&e.id),
            InstallState::Installed => show_pack(ws, &e.name, cx),
        },
        PackKind::Included { .. } => show_pack(ws, &e.name, cx),
        PackKind::Link { url, .. } => {
            cx.open_url(url);
            ws.toast("Opened the official page. After downloading, add the folder with “Add folder…”", cx);
        }
    }
}

/// List a pack's samples.
fn show_pack(ws: &mut Workspace, name: &str, cx: &mut Context<Workspace>) {
    ws.palette.tab = Tab::Samples;
    ws.search.update(cx, |i, cx| i.set_text(name, cx));
    set_query(ws, name);
    refresh(ws);
}

fn on_key(ws: &mut Workspace, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Workspace>) {
    let k = &e.keystroke;
    let handled = match k.key.as_str() {
        "escape" => {
            close(ws, window, cx);
            true
        }
        "up" => {
            move_selection(ws, -1, cx);
            true
        }
        "down" => {
            move_selection(ws, 1, cx);
            true
        }
        "pageup" => {
            for _ in 0..8 {
                move_selection(ws, -1, cx);
            }
            true
        }
        "pagedown" => {
            for _ in 0..8 {
                move_selection(ws, 1, cx);
            }
            true
        }
        "enter" => {
            activate(ws, window, cx);
            true
        }
        "tab" => {
            cycle_tab(ws, if k.modifiers.shift { -1 } else { 1 }, cx);
            true
        }
        _ => false,
    };
    if handled {
        cx.stop_propagation();
    }
}

// ───────────────────────────── Rendering ─────────────────────────────

fn chip(id: impl Into<gpui::ElementId>, text: impl Into<gpui::SharedString>, on: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(24.))
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP1))
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if on { accent_alpha(0.18) } else { content(0.05) })
        .hover(|s| s.bg(content(0.12)))
        .child(label(text, TEXT_SM, if on { pal.text } else { pal.muted }))
}

fn tag_chip(text: &str, on: bool) -> Div {
    let pal = p();
    div()
        .flex_none()
        .h(px(16.))
        .px(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(if on { accent_alpha(0.2) } else { content(0.06) })
        .child(label(text.to_owned(), TEXT_XS, if on { pal.text } else { pal.faint }))
}

/// Pack artwork: the cover image, else a tile in the pack's colour with its initials.
fn artwork(name: Option<&str>, cover: Option<std::path::PathBuf>, size: f32) -> AnyElement {
    let pal = p();
    if let Some(path) = cover {
        return img(path).size(px(size)).flex_none().rounded(px(r_md())).into_any_element();
    }
    let name = name.unwrap_or("");
    let color = if name.is_empty() { content(0.08) } else { with_alpha(series(thumbs::hue_slot(name)), 0.7) };
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(r_md()))
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .child(if name.is_empty() { icon(Icon::Waveform, 14., pal.faint).into_any_element() } else { label(thumbs::initials(name), TEXT_XS, pal.on_accent).font_weight(FontWeight::SEMIBOLD).into_any_element() })
        .into_any_element()
}

fn mini_wave(bars: Option<Arc<Vec<f32>>>, color: Hsla) -> AnyElement {
    let Some(bars) = bars else {
        return div().w(px(WAVE_W)).h(px(24.)).flex_none().flex().items_center().child(div().w_full().h(px(1.)).bg(content(0.1))).into_any_element();
    };
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let n = bars.len().max(1) as f32;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
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

fn row_shell(i: usize, selected: bool, h: f32) -> Stateful<Div> {
    div()
        .id(("palette-row", i))
        .h(px(h))
        .flex_none()
        .px(px(SP3))
        .gap(px(SP3))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .when(selected, |d| d.bg(accent_alpha(0.16)))
        .when(!selected, |d| d.hover(|s| s.bg(hover())))
}

fn column(w: f32, el: impl IntoElement) -> Div {
    div().w(px(w)).flex_none().flex().justify_end().child(el)
}

fn render_sample(ws: &mut Workspace, i: usize, r: &SampleRow, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let song = ws.s.project.scale;
    let in_key = r.key.zip(song).is_some_and(|(a, b)| keys_match(a, b));
    let bars = ws.palette.peaks.as_mut().and_then(|pk| pk.get(&r.path));
    let item = r.item();
    let weak = cx.entity().downgrade();
    let tags = &ws.palette.tags;
    let mut chips = div().flex().items_center().gap(px(SP1)).overflow_hidden();
    for t in r.tags.iter().take(4) {
        chips = chips.child(tag_chip(t, tags.contains(t)));
    }
    row_shell(i, selected, SAMPLE_ROW_H)
        .child(artwork(r.pack.as_deref(), None, 32.))
        .child(icon(if r.is_loop { Icon::Loop } else { Icon::Waveform }, 14., if in_key { pal.accent } else { pal.muted }))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_hidden()
                .child(label(r.name.clone(), TEXT_MD, pal.text))
                .child(chips),
        )
        .child(mini_wave(bars, if selected { pal.accent } else { content(0.45) }))
        .child(column(TIME_W, mono(format!("{:.1}s", r.duration), TEXT_SM, pal.muted)))
        .child(column(KEY_W, mono(r.key.map(short_key).unwrap_or_else(|| "–".into()), TEXT_SM, if in_key { pal.accent } else { pal.muted })))
        .child(column(BPM_W, mono(r.bpm.map_or("–".into(), |b| format!("{b:.0}")), TEXT_SM, pal.muted)))
        .child({
            let (id, fav) = (r.id, r.favorite);
            div()
                .id(("palette-fav", i))
                .flex_none()
                .child(icon(Icon::Star, 12., if fav { pal.solo } else { content(0.2) }))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.b.library.set_favorite(id, !fav);
                    for s in this.palette.samples.iter_mut().filter(|s| s.id == id) {
                        s.favorite = !fav;
                    }
                    rebuild(this);
                    cx.notify();
                }))
        })
        .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, _, cx| {
            this.open_menu(crate::workspace::MenuKind::LibraryContext, e.position, cx);
        }))
        .tooltip(widgets::tip_owned(format!("{}\n{}\nClick to audition · Enter or double-click to add · drag onto the arrangement", r.category, r.path.display())))
        .on_drag(DraggedSample(item), move |d: &DraggedSample, _, _, cx| {
            // The palette gets out of the way so the drop lands on the lanes.
            let _ = weak.update(cx, |ws, cx| {
                ws.palette.open = false;
                ws.palette.audition.stop();
                ws.b.engine.preview(None);
                cx.notify();
            });
            cx.new(|_| d.clone())
        })
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            if e.click_count >= 2 {
                this.palette.selected = i;
                activate(this, window, cx);
            } else {
                select(this, i, cx);
            }
        }))
        .into_any_element()
}

fn render_splice(ws: &mut Workspace, i: usize, s: &SpliceSample, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let song = ws.s.project.scale;
    let scale = splice::scale_of(s);
    let in_key = scale.zip(song).is_some_and(|(a, b)| keys_match(a, b));
    let tags = ws.palette.tags.clone();
    let (cover, bars, playing) = match ws.palette.splice.as_mut() {
        Some(sp) => (s.pack.as_ref().and_then(|p| p.cover_url.as_deref()).and_then(|u| sp.cover(u)), sp.wave(s), sp.previewing.as_deref() == Some(s.uuid.as_str())),
        None => (None, None, false),
    };
    let mut chips = div().flex().items_center().gap(px(SP1)).overflow_hidden();
    for t in s.tags.iter().take(4) {
        chips = chips.child(tag_chip(t, tags.iter().any(|x| x.eq_ignore_ascii_case(t))));
    }
    let page = aitrack_splice::page_url(s);
    row_shell(i, selected, SAMPLE_ROW_H)
        .child(artwork(s.pack.as_ref().map(|p| p.name.as_str()), cover, 32.))
        .child(icon(if s.is_loop { Icon::Loop } else { Icon::Waveform }, 14., if in_key { pal.accent } else { pal.muted }))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_hidden()
                .child(div().flex().items_center().gap(px(SP1)).child(label(s.name.clone(), TEXT_MD, pal.text)).when(playing, |d| d.child(icon(Icon::Volume, 12., pal.accent))))
                .child(chips),
        )
        .child(mini_wave(bars, if selected { pal.accent } else { content(0.45) }))
        .child(column(TIME_W, mono(format!("{:.1}s", s.duration), TEXT_SM, pal.muted)))
        .child(column(KEY_W, mono(s.short_key().unwrap_or_else(|| "–".into()), TEXT_SM, if in_key { pal.accent } else { pal.muted })))
        .child(column(BPM_W, mono(s.bpm.map_or("–".into(), |b| format!("{b:.0}")), TEXT_SM, pal.muted)))
        .child(
            widgets::sized_icon_button(("splice-get", i), Icon::Download, false, 24., 14.)
                .tooltip(widgets::tip("Get it on Splice (your account). It lands in your Splice folder, which the library indexes.", None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&page);
                    this.toast("Opened on Splice", cx);
                })),
        )
        .tooltip(widgets::tip_owned(format!("{}\nSplice preview · Enter opens it on splice.com", s.pack.as_ref().map_or("", |p| p.name.as_str()))))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            if e.click_count >= 2 {
                this.palette.selected = i;
                activate(this, window, cx);
            } else {
                select(this, i, cx);
            }
        }))
        .into_any_element()
}

fn device_thumb(ws: &mut Workspace, d: &DeviceRow) -> AnyElement {
    let pal = p();
    if let Some(path) = ws.palette.thumbs.as_mut().and_then(|t| t.get(&d.thumb_id())) {
        return img(path).w(px(48.)).h(px(32.)).flex_none().rounded(px(r_md())).into_any_element();
    }
    let i = match &d.dev {
        DeviceRef::Stock(id) => crate::views::menus::device_icon(id, ws),
        DeviceRef::Plugin(_) if d.instrument => Icon::Piano,
        DeviceRef::Plugin(_) => Icon::Plug,
    };
    let tint = series(thumbs::hue_slot(&d.vendor));
    div()
        .w(px(48.))
        .h(px(32.))
        .flex_none()
        .rounded(px(r_md()))
        .bg(with_alpha(tint, 0.22))
        .border_1()
        .border_color(with_alpha(tint, 0.45))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP1))
        .child(icon(i, 14., pal.text))
        .child(label(thumbs::initials(&d.vendor), TEXT_XS, pal.muted).font_weight(FontWeight::SEMIBOLD))
        .into_any_element()
}

fn render_device(ws: &mut Workspace, i: usize, d: &DeviceRow, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let thumb = device_thumb(ws, d);
    row_shell(i, selected, ITEM_ROW_H)
        .child(thumb)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(label(d.name.clone(), TEXT_MD, pal.text))
                .child(label(format!("{} · {}", d.vendor, d.category), TEXT_XS, pal.faint)),
        )
        .when(d.instrument, |r| r.child(icon(Icon::MusicNote, 12., pal.faint)))
        .child(widgets::badge(d.format, pal.muted))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            this.palette.selected = i;
            if e.click_count >= 2 {
                activate(this, window, cx);
            }
            cx.notify();
        }))
        .into_any_element()
}

fn render_preset(i: usize, pr: &PresetRow, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    row_shell(i, selected, ITEM_ROW_H)
        .child(div().w(px(48.)).flex_none().flex().justify_center().child(icon(Icon::Sliders, 14., pal.muted)))
        .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(label(pr.name.clone(), TEXT_MD, pal.text)).child(label(format!("{} · {}", pr.plugin, pr.category), TEXT_XS, pal.faint)))
        .child(widgets::badge("Preset", pal.muted))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            this.palette.selected = i;
            if e.click_count >= 2 {
                activate(this, window, cx);
            }
            cx.notify();
        }))
        .into_any_element()
}

fn keycap(text: String) -> Div {
    let pal = p();
    div().h(px(20.)).px(px(SP1 + 2.)).flex().items_center().rounded(px(r_sm())).bg(content(0.08)).border_1().border_color(border()).child(mono(text, TEXT_XS, pal.muted))
}

fn render_command(i: usize, c: usize, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let e = &TABLE[c];
    let mut keys = div().flex().items_center().gap(px(SP1));
    if let Some(k) = e.keys.first() {
        keys = keys.child(keycap(crate::keys::display(k)));
    }
    row_shell(i, selected, ITEM_ROW_H)
        .child(div().w(px(48.)).flex_none().flex().justify_center().child(icon(Icon::Keyboard, 14., pal.muted)))
        .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(label(e.label, TEXT_MD, pal.text)).child(label(format!("{} · {}", e.category.label(), e.help), TEXT_XS, pal.faint)))
        .child(keys)
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            this.palette.selected = i;
            if e.click_count >= 2 {
                activate(this, window, cx);
            }
            cx.notify();
        }))
        .into_any_element()
}

fn render_pack(ws: &mut Workspace, i: usize, idx: usize, selected: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(packs) = ws.palette.packs.as_ref() else { return div().into_any_element() };
    let Some(e) = packs.entries.get(idx).cloned() else { return div().into_any_element() };
    let state = packs.state(&e.id);
    let (status, action): (AnyElement, Option<(&'static str, Icon, bool)>) = match (&e.kind, &state) {
        (PackKind::Included { samples }, _) => (mono(format!("{samples} samples"), TEXT_XS, pal.faint).into_any_element(), Some(("Show", Icon::Search, false))),
        (PackKind::Optional { .. }, InstallState::Installed) => (label("Installed", TEXT_XS, pal.play).into_any_element(), Some(("Show", Icon::Search, false))),
        (PackKind::Optional { .. }, InstallState::Running { stage, fraction }) => {
            let what = match stage {
                aitrack_library::install::Stage::Downloading => "Downloading",
                aitrack_library::install::Stage::Verifying => "Checking",
                aitrack_library::install::Stage::Unpacking => "Unpacking",
            };
            let text = fraction.map_or(format!("{what}…"), |f| format!("{what} {:.0}%", f * 100.));
            (
                div().w(px(120.)).flex().flex_col().gap(px(2.)).child(label(text, TEXT_XS, pal.muted)).child(widgets::hbar(fraction.unwrap_or(0.), 120., 3., pal.accent)).into_any_element(),
                Some(("Cancel", Icon::X, false)),
            )
        }
        (PackKind::Optional { .. }, InstallState::Failed(err)) => (label(crate::paint::fit(err, 160., TEXT_XS), TEXT_XS, pal.record).into_any_element(), Some(("Retry", Icon::Refresh, true))),
        (PackKind::Optional { size_mb }, InstallState::NotInstalled) => (mono(format!("{size_mb:.0} MB"), TEXT_XS, pal.faint).into_any_element(), Some(("Download", Icon::Download, true))),
        (PackKind::Link { kind, requires_account, .. }, _) => (
            label(if *requires_account { format!("{kind} · free account") } else { kind.to_string() }, TEXT_XS, pal.faint).into_any_element(),
            Some(("Official page", Icon::Globe, false)),
        ),
    };
    let mut chips = div().flex().items_center().gap(px(SP1)).overflow_hidden().child(tag_chip(&e.license, false));
    for t in e.tags.iter().take(3) {
        chips = chips.child(tag_chip(t, false));
    }
    let is_link = matches!(e.kind, PackKind::Link { .. });
    row_shell(i, selected, SAMPLE_ROW_H)
        .child(artwork(Some(&e.name), None, 32.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().flex().items_center().gap(px(SP2)).child(label(e.name.clone(), TEXT_MD, pal.text)).child(label(e.author.clone(), TEXT_XS, pal.faint)))
                .child(chips),
        )
        .child(status)
        .when(is_link, |r| {
            r.child(
                widgets::text_button(("pack-add", i), "Add folder…", Some(Icon::FolderOpen), false)
                    .tooltip(widgets::tip("Index the folder you downloaded into the library", None))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.pick_library_folder(cx);
                    })),
            )
        })
        .when_some(action, |r, (text, ic, primary)| {
            r.child(widgets::text_button(("pack-act", i), text, Some(ic), primary).on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                pack_action(this, idx, window, cx);
                cx.notify();
            })))
        })
        .tooltip(widgets::tip_owned(if e.description.is_empty() { e.homepage.clone() } else { format!("{}\n{}", e.description, e.homepage) }))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
            this.palette.selected = i;
            cx.notify();
        }))
        .into_any_element()
}

/// Every Content Hub pack with its download/progress/link controls (the
/// Content Hub dialog shows the same rows as the palette's Packs tab).
pub fn pack_list(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    let n = ws.palette.packs.get_or_insert_with(Default::default).entries.len();
    // Ids far above the palette's own rows so both can be on screen.
    (0..n).map(|k| render_pack(ws, 10_000 + k, k, false, cx)).collect()
}

fn render_header(title: &'static str, count: usize) -> AnyElement {
    let pal = p();
    div()
        .h(px(HEADER_H))
        .flex_none()
        .px(px(SP3))
        .mt(px(SP1))
        .flex()
        .items_center()
        .justify_between()
        .child(label(title.to_uppercase(), TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(mono(count.to_string(), TEXT_XS, pal.faint))
        .into_any_element()
}

fn sort_header(ws: &Workspace, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let (sort, asc) = (ws.palette.sort, ws.palette.ascending);
    let head = |id: &'static str, text: &'static str, col: SortCol, w: Option<f32>, cx: &mut Context<Workspace>| {
        let on = sort == col;
        let mut d = div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(2.))
            .cursor_pointer()
            .child(label(text, TEXT_XS, if on { pal.text } else { pal.faint }).font_weight(FontWeight::SEMIBOLD))
            .when(on, |d| d.child(icon(if asc { Icon::ChevronUp } else { Icon::ChevronDown }, 10., pal.accent)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let pal = &mut this.palette;
                if pal.sort == col {
                    pal.ascending = !pal.ascending;
                } else {
                    pal.sort = col;
                    pal.ascending = matches!(col, SortCol::Name | SortCol::Key);
                }
                pal.shuffle = None;
                rebuild(this);
                cx.notify();
            }));
        if let Some(w) = w {
            d = d.w(px(w)).flex_none().justify_end();
        } else {
            d = d.flex_1();
        }
        d
    };
    let splice = ws.palette.tab == Tab::Splice;
    div()
        .h(px(HEADER_H))
        .flex_none()
        .px(px(SP3 + SP2))
        .gap(px(SP3))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(div().w(px(32. + 14. + SP3)).flex_none())
        .child(head("sort-name", "NAME", SortCol::Name, None, cx))
        .child(div().w(px(WAVE_W)).flex_none().child(label("WAVEFORM", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD)))
        .child(head("sort-time", "TIME", SortCol::Time, Some(TIME_W), cx))
        .child(head("sort-key", "KEY", SortCol::Key, Some(KEY_W), cx))
        .child(head("sort-bpm", "BPM", SortCol::Bpm, Some(BPM_W), cx))
        // The favourite star (local) or the download button (Splice).
        .child(div().w(px(if splice { 24. } else { 12. })).flex_none())
}

fn bpm_control(ws: &Workspace, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let range = ws.palette.bpm;
    let on = range.is_some();
    let step = |id: &'static str, ic: Icon, which: usize, delta: i32, cx: &mut Context<Workspace>| {
        widgets::sized_icon_button(id, ic, false, 20., 10.).on_click(cx.listener(move |this, _, _, cx| {
            if let Some((a, b)) = &mut this.palette.bpm {
                let v = if which == 0 { a } else { b };
                *v = (*v as i32 + delta).clamp(40, 220) as u32;
            }
            refresh(this);
            cx.notify();
        }))
    };
    let mut d = div().flex().items_center().gap(px(SP1)).child(
        chip("bpm-toggle", "BPM", on).on_click(cx.listener(|this, _, _, cx| {
            let bpm = this.s.project.tempo.initial_bpm().round() as u32;
            this.palette.bpm = if this.palette.bpm.is_some() { None } else { Some((bpm.saturating_sub(10).max(40), (bpm + 10).min(220))) };
            refresh(this);
            cx.notify();
        })),
    );
    if let Some((a, b)) = range {
        let d_ = BPM_STEP as i32;
        d = d
            .child(step("bpm-a-dn", Icon::Minus, 0, -d_, cx))
            .child(mono(a.to_string(), TEXT_SM, pal.text))
            .child(step("bpm-a-up", Icon::Add, 0, d_, cx))
            .child(label("–", TEXT_SM, pal.faint))
            .child(step("bpm-b-dn", Icon::Minus, 1, -d_, cx))
            .child(mono(b.to_string(), TEXT_SM, pal.text))
            .child(step("bpm-b-up", Icon::Add, 1, d_, cx));
    }
    d
}

fn filter_bar(ws: &Workspace, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let mut kinds = div().flex().items_center().gap(px(SP1));
    for (i, (k, text)) in [(KindFilter::All, "All"), (KindFilter::OneShots, "One-shots"), (KindFilter::Loops, "Loops")].into_iter().enumerate() {
        kinds = kinds.child(chip(("kind", i), text, ws.palette.kind == k).on_click(cx.listener(move |this, _, _, cx| {
            this.palette.kind = k;
            refresh(this);
            cx.notify();
        })));
    }
    let relevant = ws.palette.sort == SortCol::Relevant && ws.palette.shuffle.is_none();
    let count = ws.palette.rows.iter().filter(|r| matches!(r, Row::Item(_))).count();
    let loading = ws.palette.tab == Tab::Splice && ws.palette.splice.as_ref().is_some_and(|s| s.loading);
    div()
        .px(px(SP3))
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(kinds)
        .child(widgets::divider_v(16.))
        .child(bpm_control(ws, cx))
        .child(widgets::divider_v(16.))
        .child(
            widgets::sized_icon_button("shuffle", Icon::Shuffle, ws.palette.shuffle.is_some(), 24., 14.).tooltip(widgets::tip("Shuffle", None)).on_click(cx.listener(|this, _, _, cx| {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
                let seed = now ^ this.palette.shuffle.unwrap_or(0).wrapping_add(0x9e37_79b9);
                this.palette.shuffle = Some(seed.wrapping_mul(6364136223846793005).wrapping_add(1));
                this.palette.selected = 0;
                refresh(this);
                cx.notify();
            })),
        )
        .child(chip("relevant", "Most relevant", relevant).on_click(cx.listener(|this, _, _, cx| {
            this.palette.sort = SortCol::Relevant;
            this.palette.shuffle = None;
            refresh(this);
            cx.notify();
        })))
        .child(div().flex_1())
        .when(loading, |d| d.child(icon(Icon::Loader, 12., pal.accent)))
        .child(mono(format!("{count} results"), TEXT_XS, pal.faint))
}

fn tag_row(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<Div> {
    let tags: Vec<(String, usize)> = if ws.palette.tab == Tab::Splice {
        ws.palette.splice.as_ref()?.tags.iter().map(|t| (t.label.to_lowercase(), t.count as usize)).collect()
    } else {
        ws.palette.sample_tags.clone()
    };
    let selected = &ws.palette.tags;
    let mut shown: Vec<(String, usize)> = selected.iter().map(|t| (t.clone(), tags.iter().find(|x| &x.0 == t).map_or(0, |x| x.1))).collect();
    shown.extend(tags.into_iter().filter(|t| !selected.contains(&t.0)).take(14usize.saturating_sub(selected.len())));
    if shown.is_empty() {
        return None;
    }
    let mut row = div().id("tag-row").px(px(SP3)).flex().items_center().gap(px(SP1)).overflow_x_scroll();
    for (i, (t, n)) in shown.into_iter().enumerate() {
        let on = selected.contains(&t);
        let tag = t.clone();
        row = row.child(
            chip(("tag", i), if n > 0 { format!("{t} {n}") } else { t.clone() }, on)
                .h(px(20.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let tags = &mut this.palette.tags;
                    if let Some(p) = tags.iter().position(|x| *x == tag) {
                        tags.remove(p);
                    } else {
                        tags.push(tag.clone());
                    }
                    this.palette.selected = 0;
                    refresh(this);
                    cx.notify();
                })),
        );
    }
    Some(div().child(row))
}

fn empty_state(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let text = match ws.palette.tab {
        Tab::Splice => match ws.palette.splice.as_ref() {
            Some(s) if s.error.is_some() => format!("Splice: {}", s.error.clone().unwrap_or_default()),
            Some(s) if s.loading => "Searching Splice…".into(),
            _ => "Type to search Splice.".into(),
        },
        Tab::InKey if ws.s.project.scale.is_none() => "Set the song's key to see samples in it.".into(),
        Tab::InKey => "Nothing in this key yet. Detect keys from the library menu to find more.".into(),
        _ if ws.palette.query.trim().is_empty() => "Nothing here yet.".into(),
        _ => format!("Nothing matches “{}”.", ws.palette.query.trim()),
    };
    let detect = ws.palette.tab == Tab::InKey && ws.s.project.scale.is_some();
    div()
        .p(px(SP6))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SP3))
        .child(label(text, TEXT_MD, pal.faint))
        .when(detect, |d| {
            d.child(label("File names rarely say the key; listening to the samples finds it.", TEXT_SM, pal.faint)).child(
                widgets::text_button("detect-keys", "Detect keys by listening", Some(Icon::Wand), false).on_click(cx.listener(|this, _, _, cx| match this.b.library.detect_keys() {
                    Ok(()) => this.toast("Detecting keys in the background…", cx),
                    Err(e) => this.toast(e, cx),
                })),
            )
        })
        .into_any_element()
}

fn footer(ws: &Workspace, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let st = ws.b.library.scan_status();
    let scan = st.running.then(|| (st.message.clone(), st.fraction));
    let status = ws.b.library.status();
    let track = ws.s.selected_track.and_then(|t| ws.s.project.track(t)).map_or("a new track".to_owned(), |t| t.name.clone());
    let hint = |k: &str, t: String| div().flex().items_center().gap(px(SP1)).child(keycap(k.to_owned())).child(label(t, TEXT_XS, pal.faint));
    div()
        .flex_none()
        .h(px(32.))
        .px(px(SP3))
        .flex()
        .items_center()
        .gap(px(SP4))
        .border_t_1()
        .border_color(border())
        .child(hint("↑↓", if ws.palette.tab.samples() || ws.palette.tab == Tab::All { "Move and audition".into() } else { "Move".into() }))
        .child(hint("↵", match ws.palette.tab {
            Tab::Commands => "Run".into(),
            Tab::Packs => "Download or open".into(),
            Tab::Splice => "Open on Splice".into(),
            _ => format!("Add to {track}"),
        }))
        .child(hint("Tab", "Next section".into()))
        .child(hint("Esc", "Close".into()))
        .child(div().flex_1())
        .map(|d| match scan {
            Some((msg, f)) => d.child(label(crate::paint::fit(&msg, 200., TEXT_XS), TEXT_XS, pal.muted)).child(widgets::hbar(f, 80., 3., pal.accent)),
            None => d.child(label(crate::paint::fit(&status, 280., TEXT_XS), TEXT_XS, pal.faint)),
        })
        .child(
            widgets::sized_icon_button("palette-lib-menu", Icon::More, false, 24., 14.)
                .tooltip(widgets::tip("Library: add a folder, rescan, detect keys, organize, Content Hub", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(crate::workspace::MenuKind::LibraryContext, gpui::point(at.x - px(200.), at.y - px(180.)), cx);
                })),
        )
}

/// The palette overlay, or `None` while it is closed.
pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !ws.palette.open {
        return None;
    }
    let pal = p();
    let tab = ws.palette.tab;
    let mut tabs = div().flex().items_center().gap(px(SP1));
    for (i, t) in visible_tabs(ws).into_iter().enumerate() {
        let on = t == tab;
        tabs = tabs.child(
            div()
                .id(("palette-tab", i))
                .h(px(28.))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP1))
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.18) } else { gpui::transparent_black() })
                .hover(|s| s.bg(content(0.08)))
                .child(icon(t.icon(), 14., if on { pal.accent } else { pal.muted }))
                .child(label(tab_label(ws, t), TEXT_MD, if on { pal.text } else { pal.muted }))
                .on_click(cx.listener(move |this, _, _, cx| set_tab(this, t, cx))),
        );
    }
    let rows = ws.palette.rows.clone();
    let selected = ws.palette.selected;
    let mut list = div().id("palette-results").h(px(LIST_H)).overflow_y_scroll().track_scroll(&ws.palette.scroll).px(px(SP2)).py(px(SP1)).flex().flex_col();
    if rows.is_empty() {
        list = list.child(empty_state(ws, cx));
    }
    for (i, row) in rows.iter().enumerate() {
        let sel = i == selected;
        list = list.child(match row {
            Row::Header(t, n) => render_header(t, *n),
            Row::Item(Item::Sample(r)) => render_sample(ws, i, r, sel, cx),
            Row::Item(Item::Splice(s)) => render_splice(ws, i, s, sel, cx),
            Row::Item(Item::Device(d)) => render_device(ws, i, d, sel, cx),
            Row::Item(Item::Preset(pr)) => render_preset(i, pr, sel, cx),
            Row::Item(Item::Command(c)) => render_command(i, *c, sel, cx),
            Row::Item(Item::Pack(k)) => render_pack(ws, i, *k, sel, cx),
        });
    }
    let samples_tab = tab.samples();
    let splice_note = (tab == Tab::Splice).then(|| {
        div().px(px(SP3)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(pal.faint).child("Splice results use Splice's unofficial web API. Previews are for auditioning; get sounds through your Splice account.")
    });
    let card = div()
        .id("palette-card")
        .occlude()
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .rounded(px(r_lg() + 4.))
        .bg(pal.overlay)
        .border_1()
        .border_color(border_strong())
        .shadow_lg()
        .overflow_hidden()
        .capture_key_down(cx.listener(on_key))
        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
            cx.stop_propagation();
            // Keep the keys in the search field (arrows, Enter, Escape).
            let f = this.search.read(cx).focus_handle(cx);
            if !f.is_focused(window) {
                f.focus(window, cx);
            }
        }))
        .child(div().p(px(SP3)).pb(px(SP2)).flex().flex_col().gap(px(SP2)).child(ws.search.clone()).child(tabs))
        .when(samples_tab, |d| {
            let tags = tag_row(ws, cx);
            d.child(div().pb(px(SP2)).flex().flex_col().gap(px(SP2)).child(filter_bar(ws, cx)).children(tags).children(splice_note))
                .child(sort_header(ws, cx))
        })
        .when(!samples_tab, |d| d.child(div().border_b_1().border_color(border())))
        .child(list)
        .child(footer(ws, cx));
    Some(
        div()
            .id("palette-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(72.))
            .bg(shadow(0.3))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| close(this, window, cx)))
            .child(motion::rise_in("palette-in", card, 8.))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_score_needs_every_word() {
        assert!(text_score("eq", "EQ Eight", &[]).unwrap() > text_score("eq", "Parametric", &["eq"]).unwrap());
        assert_eq!(text_score("reverb hall", "Reverb", &["space"]), None);
        assert_eq!(text_score("", "x", &[]), Some(0.));
        assert!(text_score("eight", "EQ Eight", &[]).is_some());
    }

    #[test]
    fn selection_skips_headers_and_stops_at_ends() {
        let rows = vec![Row::Header("A", 1), Row::Item(Item::Command(0)), Row::Header("B", 1), Row::Item(Item::Command(1))];
        assert_eq!(first_item(&rows), 1);
        assert_eq!(step(&rows, 1, 1), 3);
        assert_eq!(step(&rows, 3, 1), 3);
        assert_eq!(step(&rows, 3, -1), 1);
        assert_eq!(step(&rows, 1, -1), 1);
    }

    #[test]
    fn columns_sort_and_missing_values_go_last() {
        let mut v: Vec<(String, f32, Option<u8>, Option<f32>)> = vec![("b".into(), 2., Some(9), None), ("a".into(), 1., None, Some(120.)), ("c".into(), 3., Some(0), Some(90.))];
        let k = |t: &(String, f32, Option<u8>, Option<f32>)| t.clone();
        order(&mut v, SortCol::Name, true, None, k);
        assert_eq!(v.iter().map(|t| t.0.as_str()).collect::<String>(), "abc");
        order(&mut v, SortCol::Bpm, false, None, k);
        assert_eq!(v.iter().map(|t| t.0.as_str()).collect::<String>(), "acb");
        order(&mut v, SortCol::Key, true, None, k);
        assert_eq!(v.iter().map(|t| t.0.as_str()).collect::<String>(), "cba");
        order(&mut v, SortCol::Time, false, None, k);
        assert_eq!(v.iter().map(|t| t.0.as_str()).collect::<String>(), "cba");
        let mut a = v.clone();
        let mut b = v.clone();
        order(&mut a, SortCol::Relevant, true, Some(7), k);
        order(&mut b, SortCol::Relevant, true, Some(7), k);
        assert_eq!(a, b, "a shuffle is stable for its seed");
    }

    #[test]
    fn short_keys() {
        assert_eq!(short_key(Scale::new(aitrack_model::Key(9), ScaleKind::Minor)), "Am");
        assert_eq!(short_key(Scale::new(aitrack_model::Key(6), ScaleKind::Major)), "F#");
    }
}
