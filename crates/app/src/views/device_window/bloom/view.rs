//! What the Bloom editors keep between frames: the parsed scene, the roll
//! history, the cells' motion, the transient rings, the gestures in flight
//! and the background jobs (colour analysis, matching, sample renders).
//!
//! Every edit of the scene goes through [`commit`]: one `SetDeviceState`
//! (with the Seed and Species parameters it was grown from) as one undo
//! step. Drags preview the scene in the editor and commit once, on release.

use super::field::Field;
use super::look::{self, Look};
use super::matter::Material;
use crate::views::device_ui::Dev;
use crate::workspace::Workspace;
use aitrack_ai::progress::CancelToken;
use aitrack_model::{Command, DeviceId};
use aitrack_stock::instrument::bloom::genome::{Genome, SOUNDS, Scene, Species};
use aitrack_stock::instrument::bloom::timbre::{self, Timbre};
use aitrack_stock::instrument::bloom::voice::Macros;
use aitrack_stock::instrument::bloom::patch::{Kind, Patch, PatchScene};
use aitrack_stock::instrument::bloom::tonal;
use aitrack_stock::instrument::bloom::{self as bloom, MAX_SEED, p};
use crossbeam_channel::{Receiver, Sender, unbounded};
use gpui::RenderImage;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

pub use aitrack_stock::instrument::bloom::Corners;

/// The device's state, parsed once per change of its bytes.
pub struct Parsed {
    pub stored: Option<Scene>,
    pub corners: Corners,
    /// The visual's material (Obsidian when the state names none).
    pub material: Material,
    /// The tonal patches the state stores (tonal kinds only).
    pub patches: Option<PatchScene>,
    /// Which tonal morph corners are stored.
    pub patch_corners: [bool; 4],
}

/// How the device plays its sounds: its Tune and macro knobs, rounded so a
/// knob drag asks for a bounded number of analyses.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shape {
    pub transpose: f32,
    pub macros: Macros,
}

impl Shape {
    pub fn of(dev: &Dev) -> Self {
        let q = |v: f32| (v / 100.0 * 25.0).round() / 25.0;
        Self {
            transpose: dev.plain(p::TUNE).round(),
            macros: Macros { decay: q(dev.plain(p::DECAY)), body: q(dev.plain(p::BODY)), tone: q(dev.plain(p::TONE)), grit: q(dev.plain(p::GRIT)) },
        }
    }

    fn key(&self) -> u64 {
        let m = self.macros;
        let mut h: u64 = 0xCBF2_9CE4_8422_2325;
        for v in [self.transpose, m.decay, m.body, m.tone, m.grit] {
            h = (h ^ u64::from(v.to_bits())).wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

/// How the device plays a tonal patch: its Tune and every knob that reaches
/// the patch ([`tonal::Knobs`]), rounded like [`Shape`] so a knob drag asks
/// for a bounded number of analyses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatchShape {
    pub knobs: tonal::Knobs,
    pub tune: f32,
}

impl Default for PatchShape {
    fn default() -> Self {
        Self { knobs: tonal::Knobs::default(), tune: 0.0 }
    }
}

impl PatchShape {
    pub fn of(dev: &Dev) -> Self {
        Self::quantised(&tonal_knobs(dev), dev.plain(p::TUNE))
    }

    /// `k` and `tune` in steps of 1/20 (5 % of a knob, so the defaults —
    /// Space 25 %, Velocity 80 % — sit on the grid), Glide in 10 ms, Tune in
    /// semitones.
    pub fn quantised(k: &tonal::Knobs, tune: f32) -> Self {
        // `+ 0.0` turns −0 into 0, so equal shapes have equal keys.
        let q = |v: f32| (v * 20.0).round() / 20.0 + 0.0;
        let m = k.macros;
        Self {
            knobs: tonal::Knobs {
                macros: Macros { decay: q(m.decay), body: q(m.body), tone: q(m.tone), grit: q(m.grit) },
                space: q(k.space),
                velocity: q(k.velocity),
                attack: q(k.attack),
                drop: q(k.drop),
                motion: q(k.motion),
                glide: (k.glide / 10.0).round() * 10.0 + 0.0,
            },
            tune: tune.round() + 0.0,
        }
    }

    fn key(&self) -> u64 {
        let (k, m) = (&self.knobs, self.knobs.macros);
        let mut h: u64 = 0x84222325_CBF29CE4;
        for v in [self.tune, m.decay, m.body, m.tone, m.grit, k.space, k.velocity, k.attack, k.drop, k.motion, k.glide] {
            h = (h ^ u64::from(v.to_bits())).wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

/// Salt of tonal keys in the analysis cache (they share it with the drums).
const TONAL_SALT: u64 = 0x7A3E_A11A_5EED_0001;

/// A tonal analysis waiting for its worker.
type PatchJob = (u64, u64, Box<Patch>, PatchShape);

/// Timbre analysis on worker threads, cached by sound and knobs: drum
/// genomes by [`Shape`] on one, tonal patches by [`PatchShape`] on a sibling
/// (a tonal render costs ten times a drum's, so drums never wait behind it).
pub struct Analyzer {
    tx: Sender<(u64, u64, Genome, Shape)>,
    /// The tonal worker (started with the first tonal request).
    tonal: Option<Sender<PatchJob>>,
    done: Sender<(u64, u64, Timbre)>,
    rx: Receiver<(u64, u64, Timbre)>,
    pending: HashSet<u64>,
    pending_tonal: HashSet<u64>,
    cache: HashMap<u64, Timbre>,
    /// The newest analysis of each sound under any knobs: shown while the
    /// one for the current knobs is on its way, so a drag never flickers.
    last: HashMap<u64, Timbre>,
}

/// Most analyses queued at once per worker (a drag asks for a new sound every frame).
const MAX_PENDING: usize = 48;
const MAX_PENDING_TONAL: usize = 32;

impl Analyzer {
    fn new() -> Self {
        let (tx, jobs) = unbounded::<(u64, u64, Genome, Shape)>();
        let (done, rx) = unbounded();
        let out = done.clone();
        let _ = std::thread::Builder::new().name("bloom-analysis".into()).spawn(move || {
            while let Ok((h, gh, g, sh)) = jobs.recv() {
                let t = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| timbre::analyze_shaped(&g, sh.transpose, &sh.macros))).unwrap_or_default();
                if out.send((h, gh, t)).is_err() {
                    break;
                }
            }
        });
        Self { tx, tonal: None, done, rx, pending: HashSet::new(), pending_tonal: HashSet::new(), cache: HashMap::new(), last: HashMap::new() }
    }

    /// The tonal worker: newest request first (the last frame of a drag is
    /// what the editor shows next), each a 750 ms render of the patch.
    fn tonal_worker(&mut self) -> &Sender<PatchJob> {
        let done = self.done.clone();
        self.tonal.get_or_insert_with(|| {
            let (tx, jobs) = unbounded::<PatchJob>();
            let _ = std::thread::Builder::new().name("bloom-tonal-analysis".into()).spawn(move || {
                let mut stack: Vec<PatchJob> = Vec::new();
                loop {
                    if stack.is_empty() {
                        match jobs.recv() {
                            Ok(j) => stack.push(j),
                            Err(_) => break,
                        }
                    }
                    stack.extend(jobs.try_iter());
                    let Some((h, ph, patch, sh)) = stack.pop() else { continue };
                    let t = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| patch_timbre(&patch, &sh.knobs, sh.tune))).unwrap_or_default();
                    if done.send((h, ph, t)).is_err() {
                        break;
                    }
                }
            });
            tx
        })
    }

    /// The timbre of `g` played with `shape` if known; otherwise it is asked
    /// for and the genome's newest timbre (if any) stands in.
    pub fn get(&mut self, g: &Genome, shape: Shape) -> Option<Timbre> {
        let gh = g.hash();
        let h = gh ^ shape.key().rotate_left(17);
        if let Some(t) = self.cache.get(&h) {
            return Some(*t);
        }
        // A long queue waits for the worker before taking more; the
        // estimate shows meanwhile.
        if self.pending.len() < MAX_PENDING && self.pending.insert(h) {
            let _ = self.tx.send((h, gh, *g, shape));
        }
        self.last.get(&gh).copied()
    }

    /// The timbre of tonal `patch` played with `shape`: `(timbre, exact)`
    /// when known for these knobs (`exact`) or for others (it stands in
    /// while the analysis for these is on its way, which is asked for).
    pub fn get_patch(&mut self, patch: &Patch, shape: PatchShape) -> Option<(Timbre, bool)> {
        let ph = patch.hash() ^ TONAL_SALT;
        let h = ph ^ shape.key().rotate_left(23);
        if let Some(t) = self.cache.get(&h) {
            return Some((*t, true));
        }
        if self.pending_tonal.len() < MAX_PENDING_TONAL && self.pending_tonal.insert(h) {
            let _ = self.tonal_worker().send((h, ph, Box::new(*patch), shape));
        }
        self.last.get(&ph).map(|t| (*t, false))
    }

    /// Take finished analyses; true when any arrived.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((h, gh, t)) = self.rx.try_recv() {
            self.pending.remove(&h);
            self.pending_tonal.remove(&h);
            if self.cache.len() > 4_096 {
                self.cache.clear();
                self.last.clear();
            }
            self.cache.insert(h, t);
            self.last.insert(gh, t);
            any = true;
        }
        any
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty() || !self.pending_tonal.is_empty()
    }
}

/// The timbre of a tonal patch as the device plays it: exactly
/// [`bloom::patch_timbre`] (its first 750 ms of the kind's demo note), but
/// rendering only those 750 ms (in whole 256-frame blocks, so the synth's
/// control grid is the same) instead of the whole note and tail: a pad's
/// analysis renders 0.77 s instead of 4.2 s. Off the UI thread.
pub fn patch_timbre(patch: &Patch, knobs: &tonal::Knobs, tune: f32) -> Timbre {
    let sr = f64::from(timbre::RATE);
    let mut b = bloom::Bloom::with_patch(patch);
    bloom::set_knobs(&mut b, knobs, tune);
    let (key, hold, tail) = bloom::demo_note(patch.kind);
    let on = (f64::from(hold) * sr) as usize;
    let whole = on + (f64::from(tail) * sr) as usize;
    let n = (timbre::RATE * 0.75) as usize;
    let frames = whole.min(n.div_ceil(256) * 256);
    let (l, r) = bloom::render_device(&mut b, &[(0, key, 0.8, true), (on, key, 0.0, false)], frames, sr);
    let n = n.min(l.len());
    let mono: Vec<f32> = l[..n].iter().zip(&r[..n]).map(|(a, b)| (a + b) * 0.5).collect();
    let mut t = timbre::analyze_signal(&mono, timbre::RATE);
    t.wobble = tonal::motion(&tonal::shape_knobs(patch, knobs));
    t
}

/// One cell's motion (visual units: the field's centre is 0, its half
/// extent 1 on both axes).
#[derive(Clone, Copy, Debug, Default)]
pub struct Anim {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub swell: f32,
    pub placed: bool,
}

/// A transient ring, `age` seconds old.
#[derive(Clone, Copy, Debug)]
pub struct RingAnim {
    pub cell: usize,
    pub age: f32,
}

/// A cell being dragged (to regrow it further or nearer its core).
#[derive(Clone, Copy, Debug)]
pub struct CellDrag {
    pub cell: usize,
    pub from: (f32, f32),
    pub moved: bool,
    pub distance: f32,
    /// The distance when the drag began.
    pub start: f32,
}

/// A synth parameter of a tonal patch being dragged in the patch view.
#[derive(Clone, Copy, Debug)]
pub struct PatchDrag {
    pub cell: usize,
    pub id: u32,
    pub x0: f32,
    pub y0: f32,
    pub v0: f32,
}

/// A gene being dragged in the DNA panel.
#[derive(Clone, Copy, Debug)]
pub struct GeneDrag {
    pub cell: usize,
    pub gene: usize,
    pub x0: f32,
    pub y0: f32,
    pub v0: f32,
}

pub enum MatchMsg {
    Progress(aitrack_ai::generate::bloom::MatchProgress),
    Done(Result<aitrack_ai::generate::bloom::MatchResult, String>),
}

pub struct MatchJob {
    pub cell: usize,
    pub name: String,
    pub rx: Receiver<MatchMsg>,
    pub cancel: CancelToken,
    pub fraction: f32,
    pub best: Option<Genome>,
    pub distance: f32,
    pub started: Instant,
}

pub enum RenderMsg {
    Progress(usize),
    Done(Result<PathBuf, String>),
}

pub struct RenderJob {
    pub rx: Receiver<RenderMsg>,
    pub done: usize,
}

/// What the visual paints with, kept across frames: the field's buffers and
/// the last image of each window it is painted in (an image belongs to the
/// window that uploaded it, so each window drops only its own).
#[derive(Default)]
pub struct Painter {
    pub field: Field,
    pub images: HashMap<u64, Arc<RenderImage>>,
}

/// One device's editor state.
pub struct BloomView {
    parsed: Option<(u64, Rc<Parsed>)>,
    /// The scene the parameters and state say, by (parsed state, seed, species).
    effective: Option<((u64, u64, usize), Scene)>,
    /// The tonal patches the parameters and state say, by (parsed state, seed, kind).
    effective_patches: Option<((u64, u64, usize), Rc<PatchScene>)>,
    pub history: Vec<Scene>,
    pub at: usize,
    pub anim: [Anim; SOUNDS],
    pub orbit: f32,
    pub time: f32,
    last: Option<Instant>,
    seen_hits: Option<[f32; SOUNDS]>,
    pub rings: Vec<RingAnim>,
    /// The pointer over the visual (visual units) and when it last moved.
    pub cursor: Option<(f32, f32)>,
    pub cursor_at: Option<Instant>,
    pub drag: Option<CellDrag>,
    pub gene_drag: Option<GeneDrag>,
    /// The scene a gesture shows before it commits.
    pub preview: Option<Scene>,
    /// The tonal patches a gesture shows before it commits.
    pub tonal_preview: Option<PatchScene>,
    pub patch_drag: Option<PatchDrag>,
    /// The timbre each tonal cell showed last, with the scene's seed and
    /// kind: it stands in while the cell's new patch is analysed.
    pub tonal_seen: [Option<(u64, Kind, Timbre)>; SOUNDS],
    pub matching: Option<MatchJob>,
    /// Next-best matches for a cell, offered as neighbours.
    pub alternatives: Option<(usize, Vec<Genome>)>,
    pub rendering: Option<RenderJob>,
    /// The selected sound being written for dragging, and the last one written.
    pub export: Option<ExportJob>,
    pub exported: Option<Exported>,
    pub painter: Rc<RefCell<Painter>>,
    /// Field cost per frame, ms (moving average).
    pub cost: Rc<Cell<f32>>,
    /// Synthetic activity per cell (the `bloomplay` dev view).
    pub fake: Option<[f32; SOUNDS]>,
    pub rolls: u64,
    pub dna_cell: Option<usize>,
    /// The GPU stage (the matter visual).
    pub stage: super::stage::StageState,
}

impl Default for BloomView {
    fn default() -> Self {
        Self {
            parsed: None,
            effective: None,
            effective_patches: None,
            history: Vec::new(),
            at: 0,
            anim: [Anim::default(); SOUNDS],
            orbit: 0.0,
            time: 0.0,
            last: None,
            seen_hits: None,
            rings: Vec::new(),
            cursor: None,
            cursor_at: None,
            drag: None,
            gene_drag: None,
            preview: None,
            tonal_preview: None,
            patch_drag: None,
            tonal_seen: [None; SOUNDS],
            matching: None,
            alternatives: None,
            rendering: None,
            export: None,
            exported: None,
            painter: Rc::default(),
            cost: Rc::default(),
            fake: None,
            rolls: 0,
            dna_cell: None,
            stage: Default::default(),
        }
    }
}

/// Every Bloom editor's state.
#[derive(Default)]
pub struct BloomState {
    pub views: HashMap<DeviceId, BloomView>,
    analyzer: Option<Analyzer>,
}

impl BloomState {
    pub fn view(&mut self, device: DeviceId) -> &mut BloomView {
        self.views.entry(device).or_default()
    }

    pub fn analyzer(&mut self) -> &mut Analyzer {
        self.analyzer.get_or_insert_with(Analyzer::new)
    }

    pub fn prune(&mut self, alive: &dyn Fn(DeviceId) -> bool) {
        self.views.retain(|id, v| {
            let keep = alive(*id);
            if !keep {
                if let Some(m) = &v.matching {
                    m.cancel.cancel();
                }
                // Free the stage's buffers on the render thread.
                if super::matter::service::ready() {
                    for k in v.stage.keys() {
                        super::matter::service::service().forget(k);
                    }
                }
            }
            keep
        });
    }

    /// Whether a shown stage wants its idle breathing frame now.
    pub fn wants_tick(&self) -> bool {
        self.views.values().any(|v| v.stage.wants_tick())
    }

    /// Whether any editor still moves or waits for a job.
    pub fn busy(&self) -> bool {
        self.analyzer.as_ref().is_some_and(Analyzer::busy) || self.views.values().any(|v| v.matching.is_some() || v.rendering.is_some())
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The device's state, parsed (cached until the project changes: the key is
/// the edit revision with the size and ends of the bytes, so reading it
/// every frame costs nothing).
pub fn parsed(ws: &mut Workspace, device: DeviceId) -> Rc<Parsed> {
    let revision = ws.s.revision();
    let state = ws.s.project.device(device).and_then(|d| d.state.as_deref());
    let h = state.map_or(0, |s| {
        let ends = [&s[..s.len().min(256)], &s[s.len().saturating_sub(256)..]].concat();
        fnv(&ends) ^ (s.len() as u64).rotate_left(32) ^ revision.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    });
    let v = ws.fx.bloom.view(device);
    if let Some((k, p)) = &v.parsed
        && *k == h
    {
        return p.clone();
    }
    let state = ws.s.project.device(device).and_then(|d| d.state.clone());
    let (stored, corners) = bloom::parse_state(state.as_deref());
    let material = bloom::visual_of(state.as_deref()).and_then(|k| Material::from_key(&k)).unwrap_or_default();
    let patches = bloom::parse_patches(state.as_deref());
    let patch_corners = bloom::parse_patch_corners(state.as_deref()).each_ref().map(Option::is_some);
    let p = Rc::new(Parsed { stored, corners, material, patches, patch_corners });
    v.parsed = Some((h, p.clone()));
    p
}

/// The scene the device plays now (what its parameters and state say).
pub fn scene(ws: &mut Workspace, dev: &Dev) -> Scene {
    let parsed = parsed(ws, dev.id);
    let seed = bloom::seed_of(dev.plain(p::SEED));
    let species = Species::from_index(dev.choice(p::SPECIES));
    let v = ws.fx.bloom.view(dev.id);
    let key = (v.parsed.as_ref().map_or(0, |p| p.0), seed, species.index());
    let s = match &v.effective {
        Some((k, s)) if *k == key => s.clone(),
        _ => {
            let s = bloom::effective_scene(parsed.stored.as_ref(), seed, species);
            v.effective = Some((key, s.clone()));
            s
        }
    };
    if v.history.is_empty() || v.history.get(v.at) != Some(&s) {
        // A scene that came from elsewhere (undo, a preset): it joins the history.
        if !v.history.contains(&s) {
            v.history.truncate(v.at + 1);
            v.history.push(s.clone());
            if v.history.len() > 64 {
                v.history.remove(0);
            }
        }
        v.at = v.history.iter().position(|h| *h == s).unwrap_or(0);
    }
    s
}

/// The scene the editor shows: a gesture's preview, else the device's.
pub fn shown(ws: &mut Workspace, dev: &Dev) -> Scene {
    let s = scene(ws, dev);
    ws.fx.bloom.view(dev.id).preview.clone().unwrap_or(s)
}

/// Store `scene` (and `corners`, else the current ones) on the device as
/// one undo step, with the Seed and Species it was grown from.
pub fn commit(ws: &mut Workspace, dev: &Dev, scene: &Scene, corners: Option<Corners>, label: &str) {
    let Some(d) = ws.s.project.device(dev.id) else { return };
    let corners = corners.unwrap_or_else(|| bloom::parse_state(d.state.as_deref()).1);
    let visual = bloom::visual_of(d.state.as_deref());
    let seed = dev.to_norm(p::SEED, scene.seed.min(MAX_SEED) as f32);
    let species = dev.to_norm(p::SPECIES, scene.species.index() as f32);
    let mut params: BTreeMap<u32, f32> = d.params.clone();
    params.insert(p::SEED, seed);
    params.insert(p::SPECIES, species);
    // The tonal patches (edits, locks) stay in the state.
    let patches = bloom::parse_patches(d.state.as_deref());
    let state = bloom::state_json_full(&params, scene, &corners, visual.as_deref(), patches.as_ref());
    // The tonal morph corners stay in the state.
    let state = bloom::keep_patch_corners(d.state.as_deref(), state);
    let device = dev.id;
    let cmds = vec![
        Command::SetDeviceState { device, state: Some(state), preset: d.preset.clone() },
        Command::SetParam { device, param: p::SEED, value: Some(seed) },
        Command::SetParam { device, param: p::SPECIES, value: Some(species) },
    ];
    ws.s.seal();
    ws.s.edit(label, cmds);
    ws.s.seal();
    let v = ws.fx.bloom.view(device);
    v.preview = None;
    v.history.truncate(v.at + 1);
    v.history.push(scene.clone());
    if v.history.len() > 64 {
        v.history.remove(0);
    }
    v.at = v.history.len() - 1;
}

/// The kind of sound the device grows (Drums or a tonal kind).
pub fn kind(dev: &Dev) -> Kind {
    Kind::from_index(dev.choice(p::KIND))
}

/// The tonal patches the device plays now (meaningful for tonal kinds).
pub fn patches(ws: &mut Workspace, dev: &Dev) -> PatchScene {
    let parsed = parsed(ws, dev.id);
    let seed = bloom::seed_of(dev.plain(p::SEED));
    let k = kind(dev);
    let kind = if k.is_tonal() { k } else { Kind::Bass };
    let v = ws.fx.bloom.view(dev.id);
    let key = (v.parsed.as_ref().map_or(0, |p| p.0), seed, kind.index());
    // Grown once per change (a state without patches regrows them from the seed).
    match &v.effective_patches {
        Some((k, ps)) if *k == key => (**ps).clone(),
        _ => {
            let ps = Rc::new(bloom::effective_patches(parsed.patches.as_ref(), seed, kind));
            v.effective_patches = Some((key, ps.clone()));
            (*ps).clone()
        }
    }
}

/// The sixteen names the editor shows: each drum's role, or each tonal
/// patch's grown name.
pub fn names(ws: &mut Workspace, dev: &Dev, scene: &Scene) -> [String; SOUNDS] {
    if kind(dev).is_tonal() {
        tonal::scene_names(&patches(ws, dev))
    } else {
        std::array::from_fn(|i| scene.sounds[i].role.label().to_string())
    }
}

/// Whether cell `i` is locked (in the drum scene or the tonal patches).
pub fn locked(ws: &mut Workspace, dev: &Dev, scene: &Scene, i: usize) -> bool {
    if kind(dev).is_tonal() { patches(ws, dev).locked(i) } else { scene.locked(i) }
}

/// Store tonal `patches` on the device as one undo step, with the Seed they
/// were grown from (the drum scene and corners kept as they are).
pub fn commit_patches(ws: &mut Workspace, dev: &Dev, patches: &PatchScene, label: &str) {
    let drums = scene(ws, dev);
    let Some(d) = ws.s.project.device(dev.id) else { return };
    let corners = bloom::parse_state(d.state.as_deref()).1;
    let visual = bloom::visual_of(d.state.as_deref());
    let seed = dev.to_norm(p::SEED, patches.seed.min(MAX_SEED) as f32);
    let mut params: BTreeMap<u32, f32> = d.params.clone();
    params.insert(p::SEED, seed);
    params.insert(p::SPECIES, dev.to_norm(p::SPECIES, drums.species.index() as f32));
    let state = bloom::state_json_full(&params, &drums, &corners, visual.as_deref(), Some(patches));
    // The tonal morph corners stay in the state.
    let state = bloom::keep_patch_corners(d.state.as_deref(), state);
    let device = dev.id;
    let cmds = vec![Command::SetDeviceState { device, state: Some(state), preset: d.preset.clone() }, Command::SetParam { device, param: p::SEED, value: Some(seed) }];
    ws.s.seal();
    ws.s.edit(label, cmds);
    ws.s.seal();
    ws.fx.bloom.view(device).tonal_preview = None;
}

/// The device's knobs as a tonal patch feels them (for saving or rendering).
pub fn tonal_knobs(dev: &Dev) -> tonal::Knobs {
    let pc = |id: u32| dev.plain(id) / 100.0;
    tonal::Knobs {
        macros: Macros { decay: pc(p::DECAY), body: pc(p::BODY), tone: pc(p::TONE), grit: pc(p::GRIT) },
        space: pc(p::SPACE),
        velocity: pc(p::VELOCITY),
        attack: pc(p::ATTACK),
        drop: pc(p::DROP),
        motion: pc(p::MOTION),
        glide: dev.plain(p::GLIDE),
    }
}

/// Save tonal patch `cell` (with the knobs baked in) as a user preset of
/// the Wavetable Synth; returns its name.
pub fn save_patch_preset(ws: &mut Workspace, dev: &Dev, cell: usize) -> Result<String, String> {
    let ps = patches(ws, dev);
    let patch = &ps.patches[cell.min(SOUNDS - 1)];
    let name = format!("{} {:06}", tonal::patch_name(patch), ps.seed);
    let preset = bloom::patch_preset(patch, &tonal_knobs(dev), &name);
    let params: BTreeMap<u32, f32> = preset.params.iter().filter_map(|(k, v)| Some((k.parse().ok()?, *v))).collect();
    let root = crate::views::device_window::chrome::user_root();
    crate::views::device_window::presets::save_user(&root, aitrack_stock::instrument::wavetable::ID, &name, &params).map(|(e, _)| e.name)
}

/// Switch the kind of sound the device grows (one undo step). The patches
/// of a tonal kind grow from the current seed.
pub fn set_kind(ws: &mut Workspace, dev: &Dev, kind: Kind) {
    let value = dev.to_norm(p::KIND, kind.index() as f32);
    ws.s.seal();
    ws.s.edit("Bloom kind", vec![Command::SetParam { device: dev.id, param: p::KIND, value: Some(value) }]);
    ws.s.seal();
}

/// The seed of the next roll: from the current one and the roll count, so a
/// session's rolls are repeatable (never from the clock).
pub fn next_seed(current: u64, rolls: u64) -> u64 {
    bloom::rng::splitmix64(current ^ rolls.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15)) % (MAX_SEED + 1)
}

/// Deviance as the editor reads it (0..1).
pub fn deviance(dev: &Dev) -> f32 {
    dev.plain(p::DEVIANCE) / 100.
}

/// A new scene from the next seed, `species` (else the current one).
pub fn roll(ws: &mut Workspace, dev: &Dev, species: Option<Species>) {
    let k = kind(dev);
    if k.is_tonal() && species.is_none() {
        let now = patches(ws, dev);
        let v = ws.fx.bloom.view(dev.id);
        v.rolls += 1;
        let seed = next_seed(now.seed, v.rolls);
        let new = tonal::roll(&now, seed, k, deviance(dev));
        commit_patches(ws, dev, &new, "Roll sounds");
        return;
    }
    let now = scene(ws, dev);
    let v = ws.fx.bloom.view(dev.id);
    v.rolls += 1;
    let seed = next_seed(now.seed, v.rolls);
    let species = species.unwrap_or(now.species);
    let new = bloom::grow::roll(&now, seed, species, deviance(dev));
    commit(ws, dev, &new, None, "Roll scene");
}

/// Nudge the unlocked sounds by the deviance.
pub fn mutate(ws: &mut Workspace, dev: &Dev) {
    if kind(dev).is_tonal() {
        let now = patches(ws, dev);
        let v = ws.fx.bloom.view(dev.id);
        v.rolls += 1;
        let seed = next_seed(now.seed ^ 0x5EED, v.rolls);
        let new = tonal::mutate(&now, seed, deviance(dev).max(0.05) * 0.6);
        commit_patches(ws, dev, &new, "Mutate sounds");
        return;
    }
    let now = scene(ws, dev);
    let v = ws.fx.bloom.view(dev.id);
    v.rolls += 1;
    let seed = next_seed(now.seed ^ 0x5EED, v.rolls);
    let new = bloom::grow::mutate(&now, seed, deviance(dev).max(0.05) * 0.6);
    commit(ws, dev, &new, None, "Mutate scene");
}

/// Step through the roll history (`dir` = -1 back, +1 forward).
pub fn step_history(ws: &mut Workspace, dev: &Dev, dir: i32) {
    // The history holds drum scenes; tonal kinds step with undo.
    if kind(dev).is_tonal() {
        return;
    }
    let _ = scene(ws, dev);
    let v = ws.fx.bloom.view(dev.id);
    let at = v.at as i32 + dir;
    if at < 0 || at as usize >= v.history.len() {
        return;
    }
    let target = v.history[at as usize].clone();
    let keep = v.history.clone();
    commit(ws, dev, &target, None, if dir < 0 { "Previous scene" } else { "Next scene" });
    // Walking the history does not rewrite it.
    let v = ws.fx.bloom.view(dev.id);
    v.history = keep;
    v.at = at as usize;
}

pub fn toggle_lock(ws: &mut Workspace, dev: &Dev, cell: usize) {
    if kind(dev).is_tonal() {
        let mut ps = patches(ws, dev);
        let on = !ps.locked(cell);
        ps.set_locked(cell, on);
        commit_patches(ws, dev, &ps, if on { "Lock sound" } else { "Unlock sound" });
        return;
    }
    let mut s = scene(ws, dev);
    let on = !s.locked(cell);
    s.set_locked(cell, on);
    commit(ws, dev, &s, None, if on { "Lock sound" } else { "Unlock sound" });
}

/// How far sound `cell` has grown from its core (the drum or the tonal one).
pub fn distance(ws: &mut Workspace, dev: &Dev, cell: usize) -> f32 {
    let cell = cell.min(SOUNDS - 1);
    if kind(dev).is_tonal() { patches(ws, dev).distances[cell] } else { scene(ws, dev).distances[cell] }
}

/// Show sound `cell` regrown to `distance` (a drag of the mass) until
/// [`finish_regrow`].
pub fn preview_regrow(ws: &mut Workspace, dev: &Dev, cell: usize, distance: f32) {
    if kind(dev).is_tonal() {
        let ps = patches(ws, dev);
        ws.fx.bloom.view(dev.id).tonal_preview = Some(tonal::regrow(&ps, cell, distance));
    } else {
        let s = scene(ws, dev);
        ws.fx.bloom.view(dev.id).preview = Some(bloom::grow::regrow(&s, cell, distance));
    }
}

/// Store what a drag of the mass showed (one undo step) when it `moved`,
/// else drop it.
pub fn finish_regrow(ws: &mut Workspace, dev: &Dev, moved: bool) {
    let v = ws.fx.bloom.view(dev.id);
    let (drums, tonal) = (v.preview.take(), v.tonal_preview.take());
    if !moved {
        return;
    }
    match (kind(dev).is_tonal(), drums, tonal) {
        (true, _, Some(ps)) => commit_patches(ws, dev, &ps, "Regrow sound"),
        (false, Some(s), _) => commit(ws, dev, &s, None, "Regrow sound"),
        _ => {}
    }
}

/// Set synth parameters `(id, normalised)` of tonal cell `cell`: shown
/// while a gesture goes on, or stored as one undo step (`label`). The
/// cell's core is re-based, so it keeps its place on its growth path.
pub fn set_patch_params(ws: &mut Workspace, dev: &Dev, cell: usize, values: &[(u32, f32)], label: Option<&str>) {
    let cell = cell.min(SOUNDS - 1);
    let ps = patches(ws, dev);
    let mut x = ps.patches[cell].folded();
    for &(id, v) in values {
        x.set(id, v);
    }
    let new = tonal::replace(&ps, cell, &x);
    match label {
        Some(l) => commit_patches(ws, dev, &new, l),
        None => ws.fx.bloom.view(dev.id).tonal_preview = Some(new),
    }
}

/// The visual's material (a look only: the sound does not change), as one
/// undo step through the device state.
pub fn set_material(ws: &mut Workspace, dev: &Dev, material: Material) {
    if parsed(ws, dev.id).material == material {
        return;
    }
    let s = scene(ws, dev);
    let Some(d) = ws.s.project.device(dev.id) else { return };
    let corners = bloom::parse_state(d.state.as_deref()).1;
    let mut params: BTreeMap<u32, f32> = d.params.clone();
    params.insert(p::SEED, dev.to_norm(p::SEED, s.seed.min(MAX_SEED) as f32));
    params.insert(p::SPECIES, dev.to_norm(p::SPECIES, s.species.index() as f32));
    let patches = bloom::parse_patches(d.state.as_deref());
    let state = bloom::state_json_full(&params, &s, &corners, Some(material.key()), patches.as_ref());
    // The tonal morph corners stay in the state.
    let state = bloom::keep_patch_corners(d.state.as_deref(), state);
    let cmd = Command::SetDeviceState { device: dev.id, state: Some(state), preset: d.preset.clone() };
    ws.s.seal();
    ws.s.edit(&format!("Bloom material: {}", material.name()), vec![cmd]);
    ws.s.seal();
}

/// Store the current scene's sounds in morph corner `k`, or clear it.
pub fn set_corner(ws: &mut Workspace, dev: &Dev, k: usize, clear: bool) {
    if kind(dev).is_tonal() {
        let now = patches(ws, dev);
        let Some(d) = ws.s.project.device(dev.id) else { return };
        let state = bloom::set_patch_corner(d.state.as_deref(), k, if clear { None } else { Some(&now) });
        let cmd = Command::SetDeviceState { device: dev.id, state: Some(state), preset: d.preset.clone() };
        ws.s.seal();
        ws.s.edit(if clear { "Clear morph corner" } else { "Set morph corner" }, vec![cmd]);
        ws.s.seal();
        return;
    }
    let s = scene(ws, dev);
    let mut corners = bloom::parse_state(ws.s.project.device(dev.id).and_then(|d| d.state.as_deref())).1;
    corners[k] = if clear { None } else { Some(Box::new(s.sounds)) };
    commit(ws, dev, &s, Some(corners), if clear { "Clear morph corner" } else { "Set morph corner" });
}

/// The sounds the device plays at the current Morph X/Y: the scene's own,
/// or the bilinear blend of the corner scenes (a missing corner is the scene).
pub fn played(ws: &mut Workspace, dev: &Dev, scene: &Scene) -> Scene {
    let parsed = parsed(ws, dev.id);
    if parsed.corners.iter().all(Option::is_none) {
        return scene.clone();
    }
    let (x, y) = (dev.plain(p::MORPH_X), dev.plain(p::MORPH_Y));
    let mut out = scene.clone();
    for i in 0..SOUNDS {
        let pick = |k: usize| parsed.corners[k].as_ref().map_or(&scene.sounds[i], |c| &c[i]);
        out.sounds[i] = bloom::grow::morph4([pick(0), pick(1), pick(2), pick(3)], x, y);
    }
    out
}

/// The look of every sound the editor shows (estimated until analysed):
/// `scene`'s drums, or on a tonal kind the patches.
pub fn looks(ws: &mut Workspace, dev: &Dev, scene: &Scene) -> [Look; SOUNDS] {
    let dark = crate::theme::is_dark();
    let t = timbres(ws, dev, scene);
    let k = kind(dev);
    std::array::from_fn(|i| look::with_kind(&look::look(&t[i], dark), k, dark))
}

/// The timbre of every sound the editor shows, as the device plays it:
/// `scene`'s drums under the knobs' [`Shape`] (estimated from the genes
/// until analysed), or on a tonal kind the shown patches ([`patch_timbres`]).
pub fn timbres(ws: &mut Workspace, dev: &Dev, scene: &Scene) -> [Timbre; SOUNDS] {
    if kind(dev).is_tonal() {
        return patch_timbres(ws, dev);
    }
    let shape = Shape::of(dev);
    let an = ws.fx.bloom.analyzer();
    std::array::from_fn(|i| {
        let g = scene.sounds[i];
        an.get(&g, shape).unwrap_or_else(|| look::estimate(&g))
    })
}

/// The timbre of every shown tonal patch under the knobs' [`PatchShape`].
/// While one is analysed, the same patch under other knobs stands in, else
/// what that cell showed last in this scene (an edit or a regrow of it),
/// else an estimate from the patch's parameters (a new roll or kind). A
/// note still sounding at the end of the analysed window reads as ringing
/// ([`look::ringing`]).
pub fn patch_timbres(ws: &mut Workspace, dev: &Dev) -> [Timbre; SOUNDS] {
    let ps = shown_patches(ws, dev);
    let shape = PatchShape::of(dev);
    let an = ws.fx.bloom.analyzer();
    let got: [Option<(Timbre, bool)>; SOUNDS] = std::array::from_fn(|i| an.get_patch(&ps.patches[i], shape));
    let v = ws.fx.bloom.view(dev.id);
    std::array::from_fn(|i| {
        let t = match (got[i], v.tonal_seen[i]) {
            (Some((t, _)), _) => t,
            (None, Some((seed, kind, t))) if seed == ps.seed && kind == ps.kind => t,
            (None, _) => look::estimate_patch(&ps.patches[i]),
        };
        v.tonal_seen[i] = Some((ps.seed, ps.kind, t));
        look::ringing(&t)
    })
}

/// The tonal patches the editor shows: a gesture's preview, else the device's.
pub fn shown_patches(ws: &mut Workspace, dev: &Dev) -> PatchScene {
    let ps = patches(ws, dev);
    match &ws.fx.bloom.view(dev.id).tonal_preview {
        Some(p) if p.kind == ps.kind => p.clone(),
        _ => ps,
    }
}

// ───────────────────────────── Motion ─────────────────────────────

/// Where cell `i` rests: on a ring around the core, further out the further
/// its sound has grown from its core.
pub fn rest(i: usize, distance: f32, orbit: f32) -> (f32, f32) {
    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::TAU / SOUNDS as f32 + orbit;
    let r = radius_of(distance);
    (a.cos() * r, a.sin() * r)
}

/// The ring radius of a distance (visual units) and back.
pub fn radius_of(distance: f32) -> f32 {
    0.24 + 0.6 * (distance / 1.2).clamp(0.0, 1.0)
}

pub fn distance_of(radius: f32) -> f32 {
    ((radius - 0.24) / 0.6 * 1.2).clamp(0.0, 1.5)
}

/// Advance the cells by the time since the last frame. `env` and `hits` are
/// the device meter's per-sound envelopes and hit counters. Returns whether
/// anything still moves.
pub fn step(v: &mut BloomView, distances: &[f32; SOUNDS], env: &[f32; SOUNDS], hits: &[f32; SOUNDS], reduced: bool) -> bool {
    let now = Instant::now();
    let dt = v.last.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32()).clamp(0.0, 1.0 / 20.0);
    v.last = Some(now);
    v.time += dt;
    let env = v.fake.unwrap_or(*env);
    // Synthetic activity keeps its rings coming.
    if let Some(fake) = v.fake {
        for (i, e) in fake.iter().enumerate() {
            if *e > 0.5 && !v.rings.iter().any(|r| r.cell == i) {
                v.rings.push(RingAnim { cell: i, age: 0.0 });
            }
        }
    }
    let mut moving = false;
    let hovering = v.cursor_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
    let active = env.iter().any(|e| *e > 0.01);
    if !reduced && (active || hovering || v.drag.is_some()) {
        v.orbit += dt * 0.06;
        moving = true;
    }
    // New hits emit rings.
    if let Some(seen) = v.seen_hits {
        for i in 0..SOUNDS {
            if hits[i] != seen[i] && !reduced {
                v.rings.push(RingAnim { cell: i, age: 0.0 });
            }
        }
    }
    v.seen_hits = Some(*hits);
    if v.rings.len() > 24 {
        let extra = v.rings.len() - 24;
        v.rings.drain(..extra);
    }
    for r in &mut v.rings {
        r.age += dt;
    }
    v.rings.retain(|r| r.age < RING_LIFE);
    moving |= !v.rings.is_empty();

    for i in 0..SOUNDS {
        let dragged = v.drag.filter(|d| d.cell == i && d.moved);
        let d = dragged.map_or(distances[i], |d| d.distance);
        let (tx, ty) = rest(i, d, v.orbit);
        let a = &mut v.anim[i];
        if !a.placed || reduced {
            a.x = tx;
            a.y = ty;
            a.vx = 0.0;
            a.vy = 0.0;
            a.placed = true;
        } else {
            // A spring to the resting place, plus the pointer's pull: cells lean
            // towards it and part around it, like a ferrofluid near a magnet.
            let (mut fx, mut fy) = ((tx - a.x) * 40.0 - a.vx * 11.0, (ty - a.y) * 40.0 - a.vy * 11.0);
            if let Some((cx, cy)) = v.cursor.filter(|_| hovering && dragged.is_none()) {
                let (dx, dy) = (cx - a.x, cy - a.y);
                let dist = (dx * dx + dy * dy).sqrt().max(1e-3);
                if dist < 0.55 {
                    let pull = 6.0 * (1.0 - dist / 0.55);
                    fx += dx * pull;
                    fy += dy * pull;
                }
                if dist < 0.14 {
                    let push = (0.14 - dist) * 90.0;
                    fx -= dx / dist * push;
                    fy -= dy / dist * push;
                }
            }
            a.vx += fx * dt;
            a.vy += fy * dt;
            a.x += a.vx * dt;
            a.y += a.vy * dt;
            if a.vx.abs() + a.vy.abs() > 1e-3 || (tx - a.x).abs() + (ty - a.y).abs() > 1e-3 {
                moving = true;
            }
        }
        let target = if reduced { 0.0 } else { env[i].clamp(0.0, 1.0) };
        let k = if target > a.swell { 1.0 - (-dt / 0.03).exp() } else { 1.0 - (-dt / 0.28).exp() };
        a.swell += (target - a.swell) * k;
        if a.swell > 0.005 {
            moving = true;
        }
    }
    moving || hovering
}

/// How long a transient ring lives, seconds.
pub const RING_LIFE: f32 = 0.7;

// ───────────────────────────── Jobs ─────────────────────────────

/// Start matching the sample at `path` into cell `cell`.
pub fn start_match(ws: &mut Workspace, dev: &Dev, cell: usize, path: PathBuf) {
    // Matching grows drum genomes; tonal kinds have nothing to match into.
    if kind(dev).is_tonal() {
        return;
    }
    let s = scene(ws, dev);
    let start = s.sounds[cell];
    let (tx, rx) = unbounded();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let name = path.file_name().map_or_else(|| "sample".to_owned(), |n| n.to_string_lossy().into_owned());
    let seed = s.seed ^ (cell as u64) << 20;
    let _ = std::thread::Builder::new().name("bloom-match".into()).spawn(move || {
        let audio = match aitrack_ai::audio::load(&path) {
            Ok(a) => a,
            Err(e) => {
                let _ = tx.send(MatchMsg::Done(Err(format!("Could not read the sample: {e}"))));
                return;
            }
        };
        let opts = aitrack_ai::generate::bloom::MatchOpts { seed, start: Some(start), ..Default::default() };
        let progress_tx = tx.clone();
        let r = aitrack_ai::generate::bloom::match_sample(&audio, &opts, &mut |p| {
            let _ = progress_tx.send(MatchMsg::Progress(p.clone()));
        }, &token);
        let _ = tx.send(MatchMsg::Done(r.map_err(|e| e.to_string())));
    });
    if let Some(old) = ws.fx.bloom.view(dev.id).matching.replace(MatchJob { cell, name, rx, cancel, fraction: 0.0, best: None, distance: 0.0, started: Instant::now() }) {
        old.cancel.cancel();
    }
}

pub fn cancel_match(ws: &mut Workspace, device: DeviceId) {
    if let Some(m) = ws.fx.bloom.view(device).matching.take() {
        m.cancel.cancel();
    }
}

/// Where rendered sounds go: `AI-Track/Bloom/<species> <seed>` in the user's music folder.
pub fn render_folder(scene: &Scene) -> PathBuf {
    crate::project_io::default_folder().join("AI-Track").join("Bloom").join(format!("{} {:06}", scene.species.label(), scene.seed))
}

/// Where rendered tonal sounds go: `AI-Track/Bloom/<kind> <seed>`.
pub fn tonal_folder(kind: Kind, seed: u64) -> PathBuf {
    crate::project_io::default_folder().join("AI-Track").join("Bloom").join(format!("{} {:06}", kind.label(), seed))
}

/// Render the sixteen sounds to WAV files (24-bit, 48 kHz) off the UI
/// thread: the drums, or on a tonal kind each patch's demo note.
pub fn start_render(ws: &mut Workspace, dev: &Dev) {
    let (tx, rx) = unbounded();
    if kind(dev).is_tonal() {
        let ps = patches(ws, dev);
        let names = tonal::scene_names(&ps);
        let (knobs, tune) = (tonal_knobs(dev), dev.plain(p::TUNE));
        let dir = tonal_folder(ps.kind, ps.seed);
        let _ = std::thread::Builder::new().name("bloom-render".into()).spawn(move || {
            let r = render_patches(&ps, &names, &knobs, tune, &dir, &mut |i| {
                let _ = tx.send(RenderMsg::Progress(i));
            });
            let _ = tx.send(RenderMsg::Done(r.map(|_| dir)));
        });
    } else {
        let s = scene(ws, dev);
        let dir = render_folder(&s);
        let _ = std::thread::Builder::new().name("bloom-render".into()).spawn(move || {
            let r = render_all(&s, &dir, &mut |i| {
                let _ = tx.send(RenderMsg::Progress(i));
            });
            let _ = tx.send(RenderMsg::Done(r.map(|_| dir)));
        });
    }
    ws.fx.bloom.view(dev.id).rendering = Some(RenderJob { rx, done: 0 });
}

/// Write every sound of `scene` into `dir` as `NN Role.wav`.
pub fn render_all(scene: &Scene, dir: &std::path::Path, progress: &mut dyn FnMut(usize)) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for (i, g) in scene.sounds.iter().enumerate() {
        let path = dir.join(format!("{:02} {}.wav", i + 1, g.role.label()));
        let _ = write_wav(g, &path)?;
        out.push(path);
        progress(i + 1);
    }
    Ok(out)
}

/// Write every patch of `ps` (its kind's demo note, as the device plays it
/// with `knobs` and `tune`) into `dir` as `NN Name.wav`.
pub fn render_patches(ps: &PatchScene, names: &[String; SOUNDS], knobs: &tonal::Knobs, tune: f32, dir: &std::path::Path, progress: &mut dyn FnMut(usize)) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for (i, patch) in ps.patches.iter().enumerate() {
        let path = dir.join(format!("{:02} {}.wav", i + 1, names[i]));
        let _ = write_patch_wav(patch, knobs, tune, &path)?;
        out.push(path);
        progress(i + 1);
    }
    Ok(out)
}

/// Write one drum sound as a 24-bit 48 kHz stereo WAV; returns its length in seconds.
pub fn write_wav(g: &Genome, path: &std::path::Path) -> Result<f32, String> {
    let sr = 48_000u32;
    let max = (sr * 4) as usize;
    let (l, r) = bloom::render::render_normalised(g, sr as f32, max, 1.0);
    write_stereo(&l, &r, sr, path)
}

/// Write a tonal patch's demo note ([`bloom::render_patch`]) as a 24-bit
/// 48 kHz stereo WAV; returns its length in seconds.
pub fn write_patch_wav(patch: &Patch, knobs: &tonal::Knobs, tune: f32, path: &std::path::Path) -> Result<f32, String> {
    let sr = 48_000u32;
    let (l, r) = bloom::render_patch(patch, knobs, tune, f64::from(sr));
    write_stereo(&l, &r, sr, path)
}

/// Write `l`/`r` with the silent tail trimmed (and a 5 ms fade, so the cut
/// never clicks) as a 24-bit stereo WAV; returns its length in seconds.
fn write_stereo(l: &[f32], r: &[f32], sr: u32, path: &std::path::Path) -> Result<f32, String> {
    let len = l.len().min(r.len());
    let n = bloom::render::tail_frames(l, r, sr as f32, len).max(256).min(len);
    let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
    let fade = (sr as usize / 200).min(n);
    for k in 0..n {
        let gain = if k + fade >= n { (n - k) as f32 / fade as f32 } else { 1.0 };
        for v in [l[k], r[k]] {
            let q = ((v * gain).clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            w.write_sample(q).map_err(|e| e.to_string())?;
        }
    }
    w.finalize().map_err(|e| e.to_string())?;
    Ok(n as f32 / sr as f32)
}

/// A sound exported for dragging: the sound (and knobs, for a tonal patch)
/// it was rendered from ([`export_key`]) and the file.
#[derive(Clone)]
pub struct Exported {
    pub genome: u64,
    pub item: crate::bridge::LibraryItem,
}

/// What an export of the selected sound depends on: the drum genome, or the
/// tonal patch with the knobs that shape it.
pub fn export_key(ws: &mut Workspace, dev: &Dev) -> u64 {
    let cell = dev.choice(p::SELECTED).min(SOUNDS - 1);
    if kind(dev).is_tonal() {
        patches(ws, dev).patches[cell].hash() ^ PatchShape::of(dev).key().rotate_left(7)
    } else {
        scene(ws, dev).sounds[cell].hash()
    }
}

/// Render the selected sound to a WAV off the UI thread (then it can be
/// dragged onto a track or a pad as a sample): the drum sound, or the tonal
/// patch's demo note.
pub fn start_export(ws: &mut Workspace, dev: &Dev) {
    let cell = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let key = export_key(ws, dev);
    let (tx, rx) = unbounded();
    let mkdir = |path: &std::path::Path| path.parent().map_or(Ok(()), |d| std::fs::create_dir_all(d).map_err(|e| e.to_string()));
    let role = if kind(dev).is_tonal() {
        let ps = patches(ws, dev);
        let patch = ps.patches[cell];
        let name = tonal::scene_names(&ps)[cell].clone();
        let (knobs, tune) = (tonal_knobs(dev), dev.plain(p::TUNE));
        let path = tonal_folder(ps.kind, ps.seed).join(format!("{:02} {name} {:08x}.wav", cell + 1, key as u32));
        let _ = std::thread::Builder::new().name("bloom-export".into()).spawn(move || {
            let r = mkdir(&path).and_then(|_| write_patch_wav(&patch, &knobs, tune, &path)).map(|secs| (path, secs));
            let _ = tx.send(r);
        });
        ps.kind.label()
    } else {
        let s = scene(ws, dev);
        let g = s.sounds[cell];
        let path = render_folder(&s).join(format!("{:02} {} {:08x}.wav", cell + 1, g.role.label(), g.hash() as u32));
        let _ = std::thread::Builder::new().name("bloom-export".into()).spawn(move || {
            let r = mkdir(&path).and_then(|_| write_wav(&g, &path)).map(|secs| (path, secs));
            let _ = tx.send(r);
        });
        g.role.label()
    };
    ws.fx.bloom.view(dev.id).export = Some(ExportJob { rx, genome: key, role });
}

/// The commands that put `patch` (shaped by `knobs`, at the level Bloom
/// plays it) on the track of Bloom `device` as a new Wavetable Synth right
/// after it, with that device's id. One undo step.
pub fn wavetable_commands(project: &mut aitrack_model::Project, device: DeviceId, patch: &Patch, knobs: &tonal::Knobs, name: &str) -> Option<(Vec<Command>, DeviceId)> {
    use aitrack_model::command::DeviceSlot;
    let slot = match aitrack_model::racks::device_slot(project, device)? {
        DeviceSlot::Track { track, index } => DeviceSlot::Track { track, index: index + 1 },
        DeviceSlot::Rack { rack, chain, index } => DeviceSlot::Rack { rack, chain, index: index + 1 },
        DeviceSlot::Return { rack, index, pos } => DeviceSlot::Return { rack, index, pos: pos + 1 },
    };
    let preset = bloom::patch_preset(patch, knobs, name);
    let id = project.alloc_device_id();
    let mut d = aitrack_model::Device::stock(id, aitrack_stock::instrument::wavetable::ID, name);
    d.params = preset.params.iter().filter_map(|(k, v)| Some((k.parse().ok()?, v.clamp(0.0, 1.0)))).collect();
    d.preset = Some(name.to_owned());
    Some((vec![Command::AddDevice { slot, device: Box::new(d) }], id))
}

/// Put tonal patch `cell` on the track as a Wavetable Synth after the Bloom
/// (one undo step) and open it in its window.
pub fn open_in_wavetable(ws: &mut Workspace, dev: &Dev, cell: usize, cx: &mut gpui::Context<Workspace>) {
    let ps = patches(ws, dev);
    let cell = cell.min(SOUNDS - 1);
    let name = tonal::scene_names(&ps)[cell].clone();
    let Some((cmds, id)) = wavetable_commands(&mut ws.s.project, dev.id, &ps.patches[cell], &tonal_knobs(dev), &name) else { return };
    ws.s.seal();
    let ok = ws.s.edit("Open in Wavetable Synth", cmds);
    ws.s.seal();
    if ok {
        crate::views::device_window::open(ws, id, cx);
        ws.toast(format!("Added {name} as a Wavetable Synth after Bloom"), cx);
    }
}

pub struct ExportJob {
    pub rx: Receiver<Result<(PathBuf, f32), String>>,
    pub genome: u64,
    pub role: &'static str,
}

/// Pick up job results; returns whether the editor should repaint.
pub fn poll(ws: &mut Workspace, dev: &Dev, cx: &mut gpui::Context<Workspace>) -> bool {
    let mut changed = ws.fx.bloom.analyzer().poll();
    let device = dev.id;
    // Matching.
    let mut finished = None;
    if let Some(m) = &mut ws.fx.bloom.view(device).matching {
        loop {
            match m.rx.try_recv() {
                Ok(MatchMsg::Progress(p)) => {
                    m.fraction = p.fraction();
                    m.best = Some(p.best);
                    m.distance = p.distance;
                }
                Ok(MatchMsg::Done(r)) => {
                    finished = Some((m.cell, m.name.clone(), r));
                    break;
                }
                // The worker went away without a word (it panicked).
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    finished = Some((m.cell, m.name.clone(), Err("The match stopped unexpectedly".to_owned())));
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
            }
            changed = true;
        }
        changed |= finished.is_some();
    }
    if let Some((cell, name, r)) = finished {
        ws.fx.bloom.view(device).matching = None;
        match r {
            Ok(found) if !found.found.is_empty() => {
                let s = scene(ws, dev);
                let new = bloom::grow::replace(&s, cell, &found.found[0].0);
                commit(ws, dev, &new, None, "Match sample");
                let alts: Vec<Genome> = found.found[1..].iter().map(|f| f.0).collect();
                ws.fx.bloom.view(device).alternatives = (!alts.is_empty()).then_some((cell, alts));
                ws.toast(format!("Matched {name} (distance {:.2})", found.found[0].1), cx);
            }
            Ok(_) => ws.toast("Nothing in Bloom sounds like that sample", cx),
            Err(e) => ws.toast(e, cx),
        }
    }
    // Rendering.
    let mut done = None;
    if let Some(r) = &mut ws.fx.bloom.view(device).rendering {
        loop {
            match r.rx.try_recv() {
                Ok(RenderMsg::Progress(n)) => r.done = n,
                Ok(RenderMsg::Done(res)) => {
                    done = Some(res);
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    done = Some(Err("The render stopped unexpectedly".to_owned()));
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
            }
            changed = true;
        }
        changed |= done.is_some();
    }
    if let Some(res) = done {
        ws.fx.bloom.view(device).rendering = None;
        match res {
            Ok(dir) => {
                let note = match ws.b.library.add_folder(&dir) {
                    Ok(()) => "and added them to the library".to_owned(),
                    Err(e) => format!("({e})"),
                };
                ws.toast(format!("Rendered 16 sounds to {} {note}", dir.display()), cx);
            }
            Err(e) => ws.toast(e, cx),
        }
    }
    // A sound written for dragging.
    let v = ws.fx.bloom.view(device);
    let got = v.export.as_ref().and_then(|j| match j.rx.try_recv() {
        Ok(r) => Some((j.genome, j.role, r)),
        Err(crossbeam_channel::TryRecvError::Disconnected) => Some((j.genome, j.role, Err("The export stopped unexpectedly".to_owned()))),
        Err(crossbeam_channel::TryRecvError::Empty) => None,
    });
    if let Some((genome, role, r)) = got {
        changed = true;
        v.export = None;
        match r {
            Ok((path, secs)) => {
                let name = path.file_stem().map_or_else(|| role.to_owned(), |s| s.to_string_lossy().into_owned());
                let item = crate::bridge::LibraryItem { id: 0, name, path: path.clone(), category: role.to_owned(), key: None, bpm: None, duration: secs, is_loop: false, favorite: false };
                v.exported = Some(Exported { genome, item });
                if let Some(dir) = path.parent() {
                    let _ = ws.b.library.add_folder(dir);
                }
                ws.toast("Rendered the sound: drag it onto a track or a pad", cx);
            }
            Err(e) => ws.toast(e, cx),
        }
    }
    changed
}

/// The commands that put `scene` on a new track as a Drum Rack: sixteen
/// pads, each a Bloom playing one sound (Single mode, the whole scene in its
/// state so a pad can be switched later), hats sharing a choke group. The
/// track goes right after the one holding `device`. One undo step.
pub fn drum_rack_commands(project: &mut aitrack_model::Project, device: DeviceId, scene: &Scene) -> Option<(Vec<Command>, aitrack_model::TrackId)> {
    use aitrack_model::{Chain, Color, Device, DeviceKind, Rack, RackKind, Track, TrackKind};
    let infos = bloom::params();
    let norm = |id: u32, plain: f32| infos.iter().find(|i| i.id == id).map_or(0., |i| i.to_normalized(plain));
    let plain = |params: &BTreeMap<u32, f32>, id: u32| infos.iter().find(|i| i.id == id).map_or(0., |i| i.to_plain(params.get(&id).copied().unwrap_or_else(|| i.default_normalized())));
    let source = project.device(device)?;
    let corners = bloom::parse_state(source.state.as_deref()).1;
    let visual = bloom::visual_of(source.state.as_deref());
    let base_params = source.params.clone();
    let base = plain(&base_params, p::BASE).round().clamp(0., 112.) as u8;
    let mut rack = Rack::new(RackKind::Drum);
    for (i, g) in scene.sounds.iter().enumerate() {
        let note = base + i as u8;
        let mut chain = Chain::new(g.role.label(), Color::palette(i));
        chain.note = Some(note);
        chain.key_range = (note, note);
        chain.choke = g.folded().index(aitrack_stock::instrument::bloom::genome::g::CHOKE) as u8;
        let mut params = base_params.clone();
        params.insert(p::MODE, norm(p::MODE, 2.));
        params.insert(p::SELECTED, norm(p::SELECTED, i as f32));
        params.insert(p::CHOKE, norm(p::CHOKE, 0.));
        params.insert(p::SEED, norm(p::SEED, scene.seed.min(MAX_SEED) as f32));
        params.insert(p::SPECIES, norm(p::SPECIES, scene.species.index() as f32));
        let mut d = Device::stock(project.alloc_device_id(), bloom::ID, format!("Bloom {}", g.role.label()));
        d.state = Some(bloom::state_json_with(&params, scene, &corners, visual.as_deref()));
        d.params = params;
        chain.devices.push(d);
        rack.chains.push(chain);
    }
    let mut rack_device = Device::stock(project.alloc_device_id(), "rack.drum", "Drum Rack");
    rack_device.kind = DeviceKind::Rack(rack);
    let track = project.alloc_track_id();
    let source_track = project.device_track(device);
    let color = source_track.and_then(|t| project.track(t)).map_or(Color::palette(5), |t| t.color);
    let mut t = Track::new(track, TrackKind::Instrument, format!("Bloom Kit {:06}", scene.seed), color);
    t.devices.push(rack_device);
    let index = source_track.and_then(|st| project.tracks.iter().position(|t| t.id == st)).map_or(project.tracks.len(), |i| i + 1);
    Some((vec![Command::AddTrack { track: Box::new(t), index }], track))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_are_repeatable_and_stay_in_range() {
        assert_eq!(next_seed(5, 1), next_seed(5, 1));
        assert_ne!(next_seed(5, 1), next_seed(5, 2));
        assert!((0..200).all(|k| next_seed(k * 7_919, k) <= MAX_SEED));
    }

    #[test]
    fn rings_map_distance_both_ways() {
        for d in [0.0, 0.3, 0.9, 1.2] {
            assert!((distance_of(radius_of(d)) - d).abs() < 1e-5);
        }
        let (x, y) = rest(0, 0.0, 0.0);
        assert!(x.abs() < 1e-6 && (y + 0.24).abs() < 1e-6, "cell 1 sits at the top");
    }

    #[test]
    fn cells_settle_at_rest_and_swell_with_their_envelope() {
        let mut v = BloomView::default();
        let distances = [0.5; SOUNDS];
        let mut env = [0.0; SOUNDS];
        let hits = [0.0; SOUNDS];
        step(&mut v, &distances, &env, &hits, false);
        let (x, y) = rest(3, 0.5, v.orbit);
        assert!((v.anim[3].x - x).abs() < 1e-6 && (v.anim[3].y - y).abs() < 1e-6, "first placed at rest");
        env[3] = 1.0;
        for _ in 0..30 {
            v.last = Some(Instant::now() - std::time::Duration::from_millis(16));
            step(&mut v, &distances, &env, &hits, false);
        }
        assert!(v.anim[3].swell > 0.8 && v.anim[4].swell < 0.01);
        // A new hit emits a ring; reduced motion emits none and stays still.
        let mut h2 = hits;
        h2[5] = 1.0;
        step(&mut v, &distances, &env, &h2, false);
        assert!(v.rings.iter().any(|r| r.cell == 5));
        let mut still = BloomView::default();
        assert!(!step(&mut still, &distances, &[0.0; SOUNDS], &hits, true));
        h2[6] = 1.0;
        step(&mut still, &distances, &[0.0; SOUNDS], &h2, true);
        assert!(still.rings.is_empty());
    }

    #[test]
    fn a_scene_becomes_a_drum_rack_of_single_sound_blooms() {
        use aitrack_model::{Command, DeviceKind};
        let mut s = crate::state::AppState::new(crate::demo::project());
        let track = s.project.tracks[0].id;
        let id = s.project.alloc_device_id();
        let dev = aitrack_model::Device::stock(id, bloom::ID, "Bloom");
        assert!(s.edit("Add device", vec![Command::AddDevice { slot: aitrack_model::command::DeviceSlot::Track { track, index: 0 }, device: Box::new(dev) }]));
        let scene = bloom::grow::generate(808, Default::default());
        let before = s.history.steps().count();
        let (cmds, new_track) = drum_rack_commands(&mut s.project, id, &scene).unwrap();
        assert!(s.edit("Bloom to Drum Rack", cmds));
        assert_eq!(s.history.steps().count(), before + 1, "one undo step");
        let t = s.project.track(new_track).unwrap();
        assert_eq!(s.project.tracks.iter().position(|x| x.id == new_track), Some(1), "right after the source track");
        let DeviceKind::Rack(rack) = &t.devices[0].kind else { panic!("a rack") };
        assert_eq!(rack.chains.len(), SOUNDS);
        for (i, c) in rack.chains.iter().enumerate() {
            assert_eq!(c.note, Some(36 + i as u8));
            let pad = &c.devices[0];
            let (stored, _) = bloom::parse_state(pad.state.as_deref());
            assert_eq!(stored.as_ref(), Some(&scene));
            let infos = bloom::params();
            let info = |id: u32| infos.iter().find(|x| x.id == id).unwrap().clone();
            assert_eq!(info(p::MODE).to_plain(pad.params[&p::MODE]), 2.0, "Single mode");
            assert_eq!(info(p::SELECTED).to_plain(pad.params[&p::SELECTED]), i as f32);
            assert_eq!(bloom::seed_of(info(p::SEED).to_plain(pad.params[&p::SEED])), 808);
        }
        // The hats share a choke group.
        assert_eq!(rack.chains[6].choke, 1);
        assert_eq!(rack.chains[10].choke, 1);
        assert_eq!(rack.chains[0].choke, 0);
    }

    #[test]
    fn the_short_tonal_analysis_is_the_stock_one() {
        // Rendering only the analysed 750 ms gives exactly `bloom::patch_timbre`:
        // a pluck (its note ends inside the window) and a pad (it does not).
        let knobs = tonal::Knobs { macros: Macros { tone: 0.2, ..Macros::default() }, ..tonal::Knobs::default() };
        for kind in [Kind::Pluck, Kind::Pad] {
            let patch = tonal::generate(1_337, kind).patches[3];
            let t0 = Instant::now();
            let fast = patch_timbre(&patch, &knobs, 0.0);
            let fast_ms = t0.elapsed().as_secs_f32() * 1000.0;
            let t0 = Instant::now();
            let full = bloom::patch_timbre(&patch, &knobs, 0.0);
            let full_ms = t0.elapsed().as_secs_f32() * 1000.0;
            assert_eq!(fast, full, "{kind:?}");
            assert!(fast.centroid > 0.0);
            println!("{kind:?}: {fast_ms:.1} ms (the whole note: {full_ms:.1} ms)");
        }
    }

    #[test]
    fn knob_shapes_round_so_a_drag_asks_for_few_analyses() {
        let k = |tone: f32| tonal::Knobs { macros: Macros { tone, ..Macros::default() }, ..tonal::Knobs::default() };
        let a = PatchShape::quantised(&k(0.31), 0.2);
        assert_eq!(a, PatchShape::quantised(&k(0.29), -0.3));
        assert_eq!(a.key(), PatchShape::quantised(&k(0.32), -0.3).key());
        assert_ne!(a.key(), PatchShape::quantised(&k(0.4), 0.0).key());
        assert_ne!(a.key(), PatchShape::quantised(&k(0.3), 12.0).key());
        assert_eq!(PatchShape::default(), PatchShape::quantised(&tonal::Knobs::default(), 0.0), "the defaults are on the grid");
    }

    #[test]
    fn tonal_analyses_arrive_and_stand_in_for_other_knobs() {
        let mut an = Analyzer::new();
        let patch = tonal::generate(5, Kind::Keys).patches[0];
        let shape = PatchShape::default();
        assert_eq!(an.get_patch(&patch, shape), None, "nothing known yet: asked for");
        assert!(an.busy());
        let t0 = Instant::now();
        while an.busy() && t0.elapsed().as_secs() < 60 {
            an.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (t, exact) = an.get_patch(&patch, shape).expect("analysed");
        assert!(exact && t == patch_timbre(&patch, &shape.knobs, shape.tune));
        // Other knobs: the patch's last analysis stands in while theirs is made.
        let bright = PatchShape::quantised(&tonal::Knobs { macros: Macros { tone: 1.0, ..Macros::default() }, ..tonal::Knobs::default() }, 0.0);
        assert_eq!(an.get_patch(&patch, bright), Some((t, false)));
        // Drums and patches share the cache without meeting.
        let g = bloom::grow::generate(5, Default::default()).sounds[0];
        assert_eq!(an.get(&g, Shape::default()), None);
    }

    #[test]
    fn a_patch_opens_as_a_wavetable_synth_after_its_bloom() {
        use aitrack_model::{Command, DeviceKind};
        let mut s = crate::state::AppState::new(crate::demo::project());
        let track = s.project.tracks[0].id;
        let id = s.project.alloc_device_id();
        let dev = aitrack_model::Device::stock(id, bloom::ID, "Bloom");
        assert!(s.edit("Add device", vec![Command::AddDevice { slot: aitrack_model::command::DeviceSlot::Track { track, index: 0 }, device: Box::new(dev) }]));
        let patch = tonal::generate(77, Kind::Lead).patches[2];
        let knobs = tonal::Knobs::default();
        let before = s.history.steps().count();
        let (cmds, wt) = wavetable_commands(&mut s.project, id, &patch, &knobs, "Glass Lead").unwrap();
        assert!(s.edit("Open in Wavetable Synth", cmds));
        assert_eq!(s.history.steps().count(), before + 1, "one undo step");
        let t = s.project.track(track).unwrap();
        assert_eq!(t.devices[0].id, id);
        assert_eq!(t.devices[1].id, wt, "right after the Bloom");
        let d = &t.devices[1];
        assert!(matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id == aitrack_stock::instrument::wavetable::ID));
        let preset = bloom::patch_preset(&patch, &knobs, "Glass Lead");
        assert_eq!(d.params.len(), preset.params.len());
        for (k, v) in &preset.params {
            assert!((d.params[&k.parse::<u32>().unwrap()] - v).abs() < 1e-6, "{k}");
        }
        assert_eq!(d.preset.as_deref(), Some("Glass Lead"));
    }

    #[test]
    fn a_tonal_export_writes_its_demo_note() {
        let patch = tonal::generate(9, Kind::Pluck).patches[0];
        let path = std::env::temp_dir().join(format!("aitrack-bloom-patch-{}.wav", std::process::id()));
        let secs = write_patch_wav(&patch, &tonal::Knobs::default(), 0.0, &path).unwrap();
        let r = hound::WavReader::open(&path).unwrap();
        assert_eq!((r.spec().channels, r.spec().sample_rate, r.spec().bits_per_sample), (2, 48_000, 24));
        let (_, hold, tail) = bloom::demo_note(Kind::Pluck);
        assert!(secs > 0.1 && secs <= hold + tail + 1e-3, "{secs}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rendered_samples_land_as_wavs() {
        let s = aitrack_stock::instrument::bloom::grow::generate(12, Default::default());
        let dir = std::env::temp_dir().join(format!("aitrack-bloom-render-{}", std::process::id()));
        let mut seen = 0;
        let files = render_all(&s, &dir, &mut |n| seen = n).unwrap();
        assert_eq!(files.len(), SOUNDS);
        assert_eq!(seen, SOUNDS);
        let r = hound::WavReader::open(&files[0]).unwrap();
        assert_eq!(r.spec().channels, 2);
        assert!(r.duration() > 1_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
