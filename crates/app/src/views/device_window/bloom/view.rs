//! What the Bloom editors keep between frames: the parsed scene, the roll
//! history, the cells' motion, the transient rings, the gestures in flight
//! and the background jobs (colour analysis, matching, sample renders).
//!
//! Every edit of the scene goes through [`commit`]: one `SetDeviceState`
//! (with the Seed and Species parameters it was grown from) as one undo
//! step. Drags preview the scene in the editor and commit once, on release.

use super::field::Field;
use super::look::{self, Look};
use crate::views::device_ui::{Dev, Region};
use crate::workspace::Workspace;
use aitrack_ai::progress::CancelToken;
use aitrack_model::{Command, DeviceId};
use aitrack_stock::instrument::bloom::genome::{Genome, SOUNDS, Scene, Species};
use aitrack_stock::instrument::bloom::timbre::{self, Timbre};
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
}

/// Timbre analysis on a worker thread, cached by genome hash.
pub struct Analyzer {
    tx: Sender<(u64, Genome)>,
    rx: Receiver<(u64, Timbre)>,
    pending: HashSet<u64>,
    cache: HashMap<u64, Timbre>,
}

impl Analyzer {
    fn new() -> Self {
        let (tx, jobs) = unbounded::<(u64, Genome)>();
        let (done, rx) = unbounded();
        let _ = std::thread::Builder::new().name("bloom-analysis".into()).spawn(move || {
            while let Ok((h, g)) = jobs.recv() {
                if done.send((h, timbre::analyze(&g))).is_err() {
                    break;
                }
            }
        });
        Self { tx, rx, pending: HashSet::new(), cache: HashMap::new() }
    }

    /// The timbre of `g` if known; otherwise it is asked for.
    pub fn get(&mut self, g: &Genome) -> Option<Timbre> {
        let h = g.hash();
        if let Some(t) = self.cache.get(&h) {
            return Some(*t);
        }
        // A long queue (a drag asks for a new sound every frame) waits for
        // the worker before taking more; the estimate shows meanwhile.
        if self.pending.len() < 48 && self.pending.insert(h) {
            let _ = self.tx.send((h, *g));
        }
        None
    }

    /// Take finished analyses; true when any arrived.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((h, t)) = self.rx.try_recv() {
            self.pending.remove(&h);
            if self.cache.len() > 4_096 {
                self.cache.clear();
            }
            self.cache.insert(h, t);
            any = true;
        }
        any
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
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
}

/// A gene being dragged in the DNA panel.
#[derive(Clone, Copy, Debug)]
pub struct GeneDrag {
    pub cell: usize,
    pub gene: usize,
    pub x0: f32,
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
    pub matching: Option<MatchJob>,
    /// Next-best matches for a cell, offered as neighbours.
    pub alternatives: Option<(usize, Vec<Genome>)>,
    pub rendering: Option<RenderJob>,
    pub region: Region,
    pub painter: Rc<RefCell<Painter>>,
    /// Field cost per frame, ms (moving average).
    pub cost: Rc<Cell<f32>>,
    /// Synthetic activity per cell (the `bloomplay` dev view).
    pub fake: Option<[f32; SOUNDS]>,
    pub rolls: u64,
    pub dna_cell: Option<usize>,
}

impl Default for BloomView {
    fn default() -> Self {
        Self {
            parsed: None,
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
            matching: None,
            alternatives: None,
            rendering: None,
            region: Region::default(),
            painter: Rc::default(),
            cost: Rc::default(),
            fake: None,
            rolls: 0,
            dna_cell: None,
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
            if !keep && let Some(m) = &v.matching {
                m.cancel.cancel();
            }
            keep
        });
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

/// The device's state, parsed (cached by the bytes).
pub fn parsed(ws: &mut Workspace, device: DeviceId) -> Rc<Parsed> {
    let state = ws.s.project.device(device).and_then(|d| d.state.clone());
    let h = state.as_deref().map_or(0, fnv);
    let v = ws.fx.bloom.view(device);
    if let Some((k, p)) = &v.parsed
        && *k == h
    {
        return p.clone();
    }
    let (stored, corners) = bloom::parse_state(state.as_deref());
    let p = Rc::new(Parsed { stored, corners });
    v.parsed = Some((h, p.clone()));
    p
}

/// The scene the device plays now (what its parameters and state say).
pub fn scene(ws: &mut Workspace, dev: &Dev) -> Scene {
    let parsed = parsed(ws, dev.id);
    let seed = bloom::seed_of(dev.plain(p::SEED));
    let species = Species::from_index(dev.choice(p::SPECIES));
    let s = bloom::effective_scene(parsed.stored.as_ref(), seed, species);
    let v = ws.fx.bloom.view(dev.id);
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
    let seed = dev.to_norm(p::SEED, scene.seed.min(MAX_SEED) as f32);
    let species = dev.to_norm(p::SPECIES, scene.species.index() as f32);
    let mut params: BTreeMap<u32, f32> = d.params.clone();
    params.insert(p::SEED, seed);
    params.insert(p::SPECIES, species);
    let state = bloom::state_json(&params, scene, &corners);
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
    let now = scene(ws, dev);
    let v = ws.fx.bloom.view(dev.id);
    v.rolls += 1;
    let seed = next_seed(now.seed ^ 0x5EED, v.rolls);
    let new = bloom::grow::mutate(&now, seed, deviance(dev).max(0.05) * 0.6);
    commit(ws, dev, &new, None, "Mutate scene");
}

/// Step through the roll history (`dir` = -1 back, +1 forward).
pub fn step_history(ws: &mut Workspace, dev: &Dev, dir: i32) {
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
    let mut s = scene(ws, dev);
    let on = !s.locked(cell);
    s.set_locked(cell, on);
    commit(ws, dev, &s, None, if on { "Lock sound" } else { "Unlock sound" });
}

/// Store the current scene's sounds in morph corner `k`, or clear it.
pub fn set_corner(ws: &mut Workspace, dev: &Dev, k: usize, clear: bool) {
    let s = scene(ws, dev);
    let mut corners = bloom::parse_state(ws.s.project.device(dev.id).and_then(|d| d.state.as_deref())).1;
    corners[k] = if clear { None } else { Some(Box::new(s.sounds)) };
    commit(ws, dev, &s, Some(corners), if clear { "Clear morph corner" } else { "Set morph corner" });
}

/// The look of every sound of `scene` (estimated until analysed).
pub fn looks(ws: &mut Workspace, scene: &Scene) -> [Look; SOUNDS] {
    let dark = crate::theme::is_dark();
    let an = ws.fx.bloom.analyzer();
    std::array::from_fn(|i| {
        let g = scene.sounds[i];
        let t = an.get(&g).unwrap_or_else(|| look::estimate(&g));
        look::look(&t, dark)
    })
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

/// Render the sixteen sounds to WAV files (24-bit, 48 kHz) off the UI thread.
pub fn start_render(ws: &mut Workspace, dev: &Dev) {
    let s = scene(ws, dev);
    let dir = render_folder(&s);
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new().name("bloom-render".into()).spawn(move || {
        let r = render_all(&s, &dir, &mut |i| {
            let _ = tx.send(RenderMsg::Progress(i));
        });
        let _ = tx.send(RenderMsg::Done(r.map(|_| dir)));
    });
    ws.fx.bloom.view(dev.id).rendering = Some(RenderJob { rx, done: 0 });
}

/// Write every sound of `scene` into `dir` as `NN Role.wav`.
pub fn render_all(scene: &Scene, dir: &std::path::Path, progress: &mut dyn FnMut(usize)) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let sr = 48_000u32;
    let mut out = Vec::new();
    for (i, g) in scene.sounds.iter().enumerate() {
        let max = (sr * 4) as usize;
        let (l, r) = bloom::render::render_normalised(g, sr as f32, max, 1.0);
        let n = bloom::render::tail_frames(&l, &r, sr as f32, max).max(256);
        let path = dir.join(format!("{:02} {}.wav", i + 1, g.role.label()));
        let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
        let fade = (sr as usize / 200).min(n);
        for k in 0..n {
            // A short fade at the end so a trimmed tail never clicks.
            let gain = if k + fade >= n { (n - k) as f32 / fade as f32 } else { 1.0 };
            for v in [l[k], r[k]] {
                let q = ((v * gain).clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                w.write_sample(q).map_err(|e| e.to_string())?;
            }
        }
        w.finalize().map_err(|e| e.to_string())?;
        out.push(path);
        progress(i + 1);
    }
    Ok(out)
}

/// Pick up job results; returns whether the editor should repaint.
pub fn poll(ws: &mut Workspace, dev: &Dev, cx: &mut gpui::Context<Workspace>) -> bool {
    let mut changed = ws.fx.bloom.analyzer().poll();
    let device = dev.id;
    // Matching.
    let mut finished = None;
    if let Some(m) = &mut ws.fx.bloom.view(device).matching {
        while let Ok(msg) = m.rx.try_recv() {
            changed = true;
            match msg {
                MatchMsg::Progress(p) => {
                    m.fraction = p.fraction();
                    m.best = Some(p.best);
                    m.distance = p.distance;
                }
                MatchMsg::Done(r) => finished = Some((m.cell, m.name.clone(), r)),
            }
        }
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
        while let Ok(msg) = r.rx.try_recv() {
            changed = true;
            match msg {
                RenderMsg::Progress(n) => r.done = n,
                RenderMsg::Done(res) => done = Some(res),
            }
        }
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
    changed
}

/// The commands that put the scene on a new track as a Drum Rack: sixteen
/// pads, each a Bloom playing one sound (Single mode), hats sharing a
/// choke group. One undo step.
pub fn drum_rack_commands(ws: &mut Workspace, dev: &Dev) -> Option<(Vec<Command>, aitrack_model::TrackId)> {
    use aitrack_model::{Chain, Color, Device, DeviceKind, Rack, RackKind, Track, TrackKind};
    let s = scene(ws, dev);
    let source = ws.s.project.device(dev.id)?;
    let corners = bloom::parse_state(source.state.as_deref()).1;
    let base_params = source.params.clone();
    let p_ = &mut ws.s.project;
    let mut rack = Rack::new(RackKind::Drum);
    let base = dev.plain(p::BASE).round().clamp(0., 112.) as u8;
    for (i, g) in s.sounds.iter().enumerate() {
        let note = base + i as u8;
        let mut chain = Chain::new(g.role.label(), Color::palette(i));
        chain.note = Some(note);
        chain.key_range = (note, note);
        chain.choke = g.folded().index(aitrack_stock::instrument::bloom::genome::g::CHOKE) as u8;
        let mut params = base_params.clone();
        params.insert(p::MODE, dev.to_norm(p::MODE, 2.));
        params.insert(p::SELECTED, dev.to_norm(p::SELECTED, i as f32));
        params.insert(p::CHOKE, dev.to_norm(p::CHOKE, 0.));
        params.insert(p::SEED, dev.to_norm(p::SEED, s.seed as f32));
        params.insert(p::SPECIES, dev.to_norm(p::SPECIES, s.species.index() as f32));
        let mut d = Device::stock(p_.alloc_device_id(), bloom::ID, format!("Bloom {}", g.role.label()));
        d.state = Some(bloom::state_json(&params, &s, &corners));
        d.params = params;
        chain.devices.push(d);
        rack.chains.push(chain);
    }
    let mut rack_device = Device::stock(p_.alloc_device_id(), "rack.drum", "Drum Rack");
    rack_device.kind = DeviceKind::Rack(rack);
    let track = p_.alloc_track_id();
    let source_track = p_.device_track(dev.id);
    let color = source_track.and_then(|t| p_.track(t)).map_or(Color::palette(5), |t| t.color);
    let mut t = Track::new(track, TrackKind::Instrument, format!("Bloom Kit {:06}", s.seed), color);
    t.devices.push(rack_device);
    let index = source_track.and_then(|st| p_.tracks.iter().position(|t| t.id == st)).map_or(p_.tracks.len(), |i| i + 1);
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
