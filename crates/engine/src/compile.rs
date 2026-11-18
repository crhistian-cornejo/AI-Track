//! Graph compiler: `Project` snapshot → [`CompiledGraph`]. Runs on the
//! control thread (or the offline renderer), never on the audio thread.
//!
//! Routing rules:
//! * `Output::Main` goes to the nearest enclosing **Group** (walking up
//!   through Folders), else to Main. Folders and VCAs carry no audio.
//! * `Output::Track(id)` must name an audio-carrying track other than itself;
//!   anything else is sent to Main instead, with a warning.
//! * `Output::Hardware(_)` goes straight to the device output bus (aligned
//!   with Main by PDC); Main itself always goes there.
//! * Sends go to any audio-carrying track (normally Returns).
//! * A routing cycle (outputs + sends) is cut open at the connection that
//!   closes it (found by a depth-first walk in project order), which is
//!   silenced, with a warning. The compile only fails for a project without
//!   a Main track.
//!
//! Solo: with any track soloed (a soloed VCA solos its members), a track is
//! audible if it is soloed, feeds a soloed track through output routing
//! (children of a soloed group), or is downstream of one (its groups, the
//! returns it sends to, Main). Everything else is silenced, sends included.
//! Mute silences a track's output and its sends.
//!
//! PDC: each track's chain latency comes from `Processor::latency` (racks
//! align their chains to the slowest one). Latency accumulates along the
//! routing; every edge into a summing point gets a delay equal to the
//! difference to the slowest arrival, so all paths line up at the output.

use crate::automation::{fold_tempo_lane, pan_gains, CONTROL_BLOCK};
use crate::clip::{ActiveNotes, AudioClipData, CompiledClip, EventList, Rng};
use crate::delay::DelayLine;
use crate::factory::ProcessorFactory;
use crate::graph::*;
use crate::sample_cache::SampleCache;
use crate::shared::{ProcessorPool, SharedProcessor};
use crate::status::{MeterBank, SessionStatus};
use aitrack_dsp::{db_to_gain, AudioBuffer};
use aitrack_model::{
    AutomationLane, AutomationTarget, Chain, ClipId, Device, DeviceId, DeviceKind, Macro, MacroMapping, ModTarget, Output, Project, Rack, Source, Track, TrackId, TrackKind,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("routing cycle through tracks {0:?}")]
    Cycle(Vec<TrackId>),
    #[error("track {track} has an invalid output: {reason}")]
    InvalidRoute { track: TrackId, reason: String },
    #[error("the project has no Main track")]
    NoMain,
}

/// Everything the compiler needs besides the project.
pub struct CompileContext<'a> {
    pub factory: &'a dyn ProcessorFactory,
    pub pool: &'a mut ProcessorPool,
    pub samples: &'a SampleCache,
    pub sample_rate: f64,
    pub max_frames: usize,
    /// Reused when the track set is unchanged, so meters do not flicker.
    pub prev_meters: Option<Arc<MeterBank>>,
    /// Tracks whose post-fader output is captured for stem export.
    pub taps: Vec<TrackId>,
    /// The live engine's analysis ring (`None` offline): lets the UI tap one
    /// track instead of the Main output.
    pub analysis: Option<Arc<crate::tap::AnalysisTap>>,
    /// The live engine's disk streaming: clips of streamed files get its
    /// readers (filled ahead by its loader, reused across compiles). `None`
    /// offline: each clip gets a reader of its own that loads pages on the
    /// spot (blocking), so a render never misses a page.
    pub streams: Option<&'a crate::stream::StreamHub>,
}

struct Handle {
    proc: Option<Arc<SharedProcessor>>,
    enabled: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dest {
    Track(usize),
    Out,
    None,
}

fn group_ancestor(tracks: &[Track], index: &HashMap<TrackId, usize>, t: &Track) -> Option<usize> {
    let mut p = t.parent;
    for _ in 0..tracks.len() {
        let pi = *index.get(&p?)?;
        match tracks[pi].kind {
            TrackKind::Group => return Some(pi),
            TrackKind::Folder => p = tracks[pi].parent,
            _ => return None,
        }
    }
    None
}

fn resolve_output(tracks: &[Track], index: &HashMap<TrackId, usize>, i: usize, main: usize) -> Result<Dest, CompileError> {
    let t = &tracks[i];
    if t.kind == TrackKind::Main {
        return Ok(if t.output == Output::None { Dest::None } else { Dest::Out });
    }
    if !t.kind.carries_audio() {
        return Ok(Dest::None);
    }
    Ok(match t.output {
        Output::Main => Dest::Track(group_ancestor(tracks, index, t).filter(|&g| g != i).unwrap_or(main)),
        Output::Track(id) => {
            let Some(&j) = index.get(&id) else {
                return Err(CompileError::InvalidRoute { track: t.id, reason: format!("unknown track {id}") });
            };
            if j == i {
                return Err(CompileError::InvalidRoute { track: t.id, reason: "routes to itself".into() });
            }
            if !tracks[j].kind.carries_audio() {
                return Err(CompileError::InvalidRoute { track: t.id, reason: format!("{id} carries no audio") });
            }
            Dest::Track(j)
        }
        Output::Hardware(_) => Dest::Out,
        Output::None => Dest::None,
    })
}

/// A connection of the audio routing, from a track to another.
#[derive(Clone, Copy)]
enum Edge {
    /// The track's output.
    Output,
    /// Entry `n` of the track's send list.
    Send(usize),
}

/// The first connection, walking depth-first from the tracks in project
/// order, that points back at a track still being walked: `(from, to, edge,
/// the tracks of the loop in order)`.
fn find_back_edge(dest: &[Dest], sends: &[Vec<(usize, usize)>]) -> Option<(usize, usize, Edge, Vec<usize>)> {
    let n = dest.len();
    let adj: Vec<Vec<(usize, Edge)>> = (0..n)
        .map(|i| {
            let out = match dest[i] {
                Dest::Track(j) => Some((j, Edge::Output)),
                _ => None,
            };
            out.into_iter().chain(sends[i].iter().enumerate().map(|(k, &(_, j))| (j, Edge::Send(k)))).collect()
        })
        .collect();
    // 0 = not seen, 1 = on the walk, 2 = done.
    let mut color = vec![0u8; n];
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for root in 0..n {
        if color[root] != 0 {
            continue;
        }
        color[root] = 1;
        stack.push((root, 0));
        while let Some(&(node, idx)) = stack.last() {
            let Some(&(to, edge)) = adj[node].get(idx) else {
                color[node] = 2;
                stack.pop();
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            match color[to] {
                0 => {
                    color[to] = 1;
                    stack.push((to, 0));
                }
                1 => {
                    let start = stack.iter().position(|(v, _)| *v == to).unwrap_or(0);
                    return Some((node, to, edge, stack[start..].iter().map(|(v, _)| *v).collect()));
                }
                _ => {}
            }
        }
    }
    None
}

/// Make the routing acyclic: silence, one at a time, the connection that
/// closes each loop (an output goes nowhere, a send is dropped) and say so.
fn break_cycles(tracks: &[Track], dest: &mut [Dest], sends: &mut [Vec<(usize, usize)>], warnings: &mut Vec<String>) {
    while let Some((from, to, edge, path)) = find_back_edge(dest, sends) {
        let names: Vec<String> = path.iter().chain(std::iter::once(&path[0])).map(|&i| format!("'{}'", tracks[i].name)).collect();
        let kind = match edge {
            Edge::Output => {
                dest[from] = Dest::None;
                "output"
            }
            Edge::Send(k) => {
                sends[from].remove(k);
                "send"
            }
        };
        warnings.push(format!(
            "routing cycle {}: the {kind} from '{}' to '{}' closes it and is silenced",
            names.join(" -> "),
            tracks[from].name,
            tracks[to].name
        ));
    }
}

/// A chain's volume × pan, zero when muted or silenced by another's solo.
fn chain_gain(c: &aitrack_model::Chain, any_solo: bool) -> (f32, f32) {
    let silent = c.mute || (any_solo && !c.solo);
    let (l, r) = pan_gains(c.pan);
    let g = if silent { 0.0 } else { db_to_gain(c.volume_db) };
    (l * g, r * g)
}

/// A send level in dB as a gain; at or below the model's "off" level, none.
fn send_gain(db: f32) -> f32 {
    if db <= aitrack_model::SEND_OFF_DB { 0.0 } else { db_to_gain(db) }
}

/// Every device of `devs`, racks' chains and returns included.
fn visit_all(devs: &[Device], f: &mut impl FnMut(&Device)) {
    for d in devs {
        f(d);
        if let DeviceKind::Rack(r) = &d.kind {
            for c in r.chains.iter().chain(&r.returns) {
                visit_all(&c.devices, f);
            }
        }
    }
}

/// Normalised params of `d` with every macro mapping that targets it applied.
fn effective_params(d: &Device, overrides: &HashMap<DeviceId, BTreeMap<u32, f32>>) -> BTreeMap<u32, f32> {
    let mut p = d.params.clone();
    if let Some(o) = overrides.get(&d.id) {
        for (k, v) in o {
            p.insert(*k, *v);
        }
    }
    p
}

fn collect_macro_overrides(chain: &[Device], out: &mut HashMap<DeviceId, BTreeMap<u32, f32>>) {
    for d in chain {
        if let DeviceKind::Rack(rack) = &d.kind {
            for m in &rack.macros {
                for map in &m.mappings {
                    out.entry(map.device).or_default().insert(map.param, map.min + (map.max - map.min) * m.value.clamp(0.0, 1.0));
                }
            }
            for c in rack.chains.iter().chain(&rack.returns) {
                collect_macro_overrides(&c.devices, out);
            }
        }
    }
}

struct DevCx<'a, 'b> {
    cx: &'a mut CompileContext<'b>,
    overrides: HashMap<DeviceId, BTreeMap<u32, f32>>,
    handles: HashMap<DeviceId, Handle>,
    warnings: Vec<String>,
    /// Sidechain buffer of each source track, and the track being compiled.
    side_slots: &'a HashMap<TrackId, usize>,
    track: TrackId,
}

/// The modulators of a frozen track that drive its fader or pan, with only
/// those mappings: the rest of the chain is what the frozen file replaced.
fn frozen_fader_mods(devs: &[Device]) -> Vec<Device> {
    let mut out = Vec::new();
    for d in devs {
        if d.is_modulator() {
            let mods: Vec<_> = d.mods.iter().filter(|m| !matches!(m.target, ModTarget::Param { .. })).cloned().collect();
            if !mods.is_empty() {
                out.push(Device { mods, ..d.clone() });
            }
        } else if let DeviceKind::Rack(r) = &d.kind {
            for c in r.chains.iter().chain(r.returns.iter()) {
                out.extend(frozen_fader_mods(&c.devices));
            }
        }
    }
    out
}

fn compile_devices(devs: &[Device], dc: &mut DevCx<'_, '_>) -> (Vec<DeviceNode>, u32) {
    let mut nodes = Vec::with_capacity(devs.len());
    let mut latency = 0u32;
    let (sr, max) = (dc.cx.sample_rate, dc.cx.max_frames);
    for d in devs {
        let enabled = Arc::new(AtomicBool::new(d.enabled));
        match &d.kind {
            DeviceKind::Rack(rack) => {
                let compiled: Vec<(Vec<DeviceNode>, u32)> = rack.chains.iter().map(|c| compile_devices(&c.devices, dc)).collect();
                let compiled_returns: Vec<(Vec<DeviceNode>, u32)> = rack.returns.iter().map(|c| compile_devices(&c.devices, dc)).collect();
                let max_lat = compiled.iter().map(|c| c.1).max().unwrap_or(0);
                let max_ret = compiled_returns.iter().map(|c| c.1).max().unwrap_or(0);
                let n_returns = rack.returns.len();
                let node = |c: &aitrack_model::Chain, devices: Vec<DeviceNode>, delay: u32, gain: (f32, f32)| ChainNode {
                    devices,
                    gain,
                    note: c.note,
                    play_note: c.play_note.unwrap_or(60),
                    key_range: c.key_range,
                    velocity_range: c.velocity_range,
                    selector_range: c.selector_range,
                    key_fade: c.key_fade,
                    vel_fade: c.vel_fade,
                    sel_fade: c.sel_fade,
                    choke: c.choke,
                    sends: (0..n_returns).map(|j| c.sends.get(j).copied().map_or(0.0, send_gain)).collect(),
                    to_return: match c.output {
                        aitrack_model::ChainOut::Return(j) if (j as usize) < n_returns => Some(j as usize),
                        _ => None,
                    },
                    buf: AudioBuffer::new(2, max),
                    events: EventList::with_capacity(EVENT_CAPACITY),
                    delay: DelayLine::new(2, delay as usize),
                    cur_gain: [0.0; 2],
                    primed: false,
                    held: [0; 2],
                    played: [0; 128],
                    choke_gain: 1.0,
                    choking: false,
                    choke_at: u32::MAX,
                    all_off: false,
                    asleep: false,
                    target: gain,
                };
                let any_solo = rack.chains.iter().any(|c| c.solo);
                let chains = rack
                    .chains
                    .iter()
                    .zip(compiled)
                    .map(|(c, (devices, lat))| node(c, devices, max_lat - lat, chain_gain(c, any_solo)))
                    .collect();
                let any_ret_solo = rack.returns.iter().any(|c| c.solo);
                let returns = rack
                    .returns
                    .iter()
                    .zip(compiled_returns)
                    .map(|(c, (devices, lat))| node(c, devices, max_ret - lat, chain_gain(c, any_ret_solo)))
                    .collect();
                if d.enabled {
                    latency += max_lat + max_ret;
                }
                dc.handles.insert(d.id, Handle { proc: None, enabled: enabled.clone() });
                nodes.push(DeviceNode::Rack(RackNode {
                    id: d.id,
                    kind: rack.kind,
                    enabled,
                    chains,
                    returns,
                    selector: rack.chain_selector,
                    dry_delay: DelayLine::new(2, max_ret as usize),
                    input: AudioBuffer::new(2, if rack.kind == aitrack_model::RackKind::Midi { 0 } else { max }),
                    out: EventList::with_capacity(if rack.kind == aitrack_model::RackKind::Midi { EVENT_CAPACITY } else { 0 }),
                }));
            }
            _ => {
                let params = effective_params(d, &dc.overrides);
                match dc.cx.pool.resolve(d, &params, dc.cx.factory, sr, max) {
                    Some(r) => {
                        if d.enabled {
                            latency += r.proc.reported_latency();
                        }
                        dc.handles.insert(d.id, Handle { proc: Some(r.proc.clone()), enabled: enabled.clone() });
                        nodes.push(DeviceNode::Proc(ProcNode {
                            id: d.id,
                            proc: r.proc,
                            enabled,
                            init_params: r.init_params,
                            is_instrument: r.is_instrument,
                            meter: r.meter,
                            sidechain: d.sidechain.filter(|s| *s != dc.track).and_then(|s| dc.side_slots.get(&s).copied()),
                            auto_events: EventList::with_capacity(0),
                            merged: EventList::with_capacity(0),
                            silent_samples: 0,
                            sleeping: false,
                            chunk: None,
                            is_midi: r.is_midi,
                            midi: r.is_midi.then(|| Box::new(MidiState::new())),
                        }));
                    }
                    None => {
                        dc.warnings.push(format!("no processor for device '{}' ({})", d.name, d.id));
                        nodes.push(DeviceNode::Missing(d.id));
                    }
                }
            }
        }
    }
    // MIDI effects run before everything else, in the order they were put in:
    // each hands its events to the next, and the instrument gets the result.
    nodes.sort_by_key(|n| !matches!(n, DeviceNode::Proc(p) if p.is_midi) && !matches!(n, DeviceNode::Rack(r) if r.kind == aitrack_model::RackKind::Midi));
    (nodes, latency)
}

pub(crate) fn find_rack(chain: &[Device], id: DeviceId) -> Option<&aitrack_model::Rack> {
    for d in chain {
        if let DeviceKind::Rack(r) = &d.kind {
            if d.id == id {
                return Some(r);
            }
            for c in r.chains.iter().chain(&r.returns) {
                if let Some(found) = find_rack(&c.devices, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn usable(lane: &AutomationLane) -> bool {
    lane.enabled && !lane.points.is_empty()
}

/// Compile a project snapshot.
/// What a frozen track plays: its render as one audio clip from tick 0.
struct FrozenPlay {
    clip: aitrack_model::Clip,
    data: AudioClipData,
}

/// The frozen playback of `t`, or `None` when it plays live: not frozen, or
/// its file is gone or unreadable (the track falls back to its devices, with
/// a warning). A file that is still loading plays silence until it lands and
/// the cache asks for a recompile.
fn frozen_play(project: &Project, t: &Track, cx: &CompileContext<'_>, warnings: &mut Vec<String>) -> Option<FrozenPlay> {
    let file = &t.freeze.as_ref().filter(|_| t.kind.has_clips())?.file;
    let audio = cx.samples.get(&file.path, cx.sample_rate as u32);
    if audio.is_none() {
        let failed = cx.samples.error(&file.path, cx.sample_rate as u32);
        let virtual_path = file.path.to_str().is_some_and(|s| s.contains("://"));
        if failed.is_some() || (!virtual_path && !file.path.exists()) {
            warnings.push(format!("track '{}': the frozen audio '{}' is missing; playing live", t.name, file.path.display()));
            return None;
        }
    }
    let bpm = project.tempo.initial_bpm();
    let clip = aitrack_model::Clip::new("Frozen", aitrack_model::Color(0), aitrack_model::SourceId(0), file.length_ticks(bpm).max(1));
    Some(FrozenPlay { clip, data: AudioClipData::of_bpm_in(&file.warp, audio, bpm, cx.samples) })
}

pub fn compile(project: &Project, cx: &mut CompileContext<'_>) -> Result<CompiledGraph, CompileError> {
    let tracks = &project.tracks;
    let index: HashMap<TrackId, usize> = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let main = tracks.iter().position(|t| t.kind == TrackKind::Main).ok_or(CompileError::NoMain)?;
    let sr = cx.sample_rate;
    let max = cx.max_frames;
    let mut warnings = Vec::new();

    // Frozen tracks play their render and run no devices.
    let mut frozen: Vec<Option<FrozenPlay>> = tracks.iter().map(|t| frozen_play(project, t, cx, &mut warnings)).collect();

    // Routing.
    let mut dest = Vec::with_capacity(tracks.len());
    for i in 0..tracks.len() {
        match resolve_output(tracks, &index, i, main) {
            Ok(d) => dest.push(d),
            Err(CompileError::InvalidRoute { reason, .. }) => {
                warnings.push(format!("track '{}': invalid output ({reason}); sent to Main instead", tracks[i].name));
                dest.push(Dest::Track(main));
            }
            Err(e) => return Err(e),
        }
    }
    let mut sends: Vec<Vec<(usize, usize)>> = vec![Vec::new(); tracks.len()]; // (send index, target)
    for (i, t) in tracks.iter().enumerate() {
        if !t.kind.carries_audio() {
            continue;
        }
        for (k, s) in t.sends.iter().enumerate() {
            match index.get(&s.target) {
                Some(&j) if j != i && tracks[j].kind.carries_audio() => sends[i].push((k, j)),
                _ => warnings.push(format!("track '{}': send {k} has an invalid target", t.name)),
            }
        }
    }
    break_cycles(tracks, &mut dest, &mut sends, &mut warnings);
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); tracks.len()];
    let mut out_pred: Vec<Vec<usize>> = vec![Vec::new(); tracks.len()];
    for i in 0..tracks.len() {
        if let Dest::Track(j) = dest[i] {
            succ[i].push(j);
            out_pred[j].push(i);
        }
        for &(_, j) in &sends[i] {
            succ[i].push(j);
        }
    }

    // Sidechain: a device that listens to another track needs that track
    // processed first. These edges only shape the order; they add nothing
    // to a track's input and nothing to delay compensation.
    let mut side_src: Vec<Vec<usize>> = vec![Vec::new(); tracks.len()];
    for (i, t) in tracks.iter().enumerate() {
        if !t.kind.carries_audio() || frozen[i].is_some() {
            continue;
        }
        let mut sources = Vec::new();
        visit_all(&t.devices, &mut |d| {
            if let Some(&j) = d.sidechain.and_then(|s| index.get(&s))
                && j != i
                && tracks[j].kind.carries_audio()
                && !sources.contains(&j)
            {
                sources.push(j);
            }
        });
        side_src[i] = sources;
    }
    let mut side_slots: HashMap<TrackId, usize> = HashMap::new();
    for &j in side_src.iter().flatten() {
        let next = side_slots.len();
        side_slots.entry(tracks[j].id).or_insert(next);
    }

    // Topological order (Kahn), stable in project order.
    let topo = |succ: &[Vec<usize>]| -> (Vec<usize>, Vec<usize>) {
        let mut indeg = vec![0usize; tracks.len()];
        for s in succ {
            for &j in s {
                indeg[j] += 1;
            }
        }
        let mut queue: VecDeque<usize> = (0..tracks.len()).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::with_capacity(tracks.len());
        while let Some(i) = queue.pop_front() {
            order.push(i);
            for &j in &succ[i] {
                indeg[j] -= 1;
                if indeg[j] == 0 {
                    queue.push_back(j);
                }
            }
        }
        (order, indeg)
    };
    let mut with_sidechains = succ.clone();
    for (i, sources) in side_src.iter().enumerate() {
        for &j in sources {
            with_sidechains[j].push(i);
        }
    }
    let (mut order, _) = topo(&with_sidechains);
    if order.len() != tracks.len() {
        // A sidechain that closes a loop with the routing (a track listening
        // to something it feeds) cannot be ordered: it hears the previous block.
        let (routed, indeg) = topo(&succ);
        if routed.len() != tracks.len() {
            // (`break_cycles` made the routing acyclic: not reachable.)
            let cyc: Vec<TrackId> = (0..tracks.len()).filter(|&i| indeg[i] > 0).map(|i| tracks[i].id).collect();
            return Err(CompileError::Cycle(cyc));
        }
        warnings.push("a sidechain loops back through the routing; it reacts one block late".to_owned());
        order = routed;
    }
    // Non-audio tracks (VCA/Folder) first so VCAs are evaluated before members.
    order.sort_by_key(|&i| tracks[i].kind.carries_audio());
    let mut gpos = vec![0usize; tracks.len()];
    for (g, &i) in order.iter().enumerate() {
        gpos[i] = g;
    }

    // Solo.
    let mut soloed = vec![false; tracks.len()];
    for (i, t) in tracks.iter().enumerate() {
        if t.solo {
            soloed[i] = true;
            if t.kind == TrackKind::Vca {
                for (j, m) in tracks.iter().enumerate() {
                    if m.vca == Some(t.id) {
                        soloed[j] = true;
                    }
                }
            }
        }
    }
    let any_solo = soloed.iter().enumerate().any(|(i, s)| *s && tracks[i].kind.carries_audio());
    let mut audible = vec![true; tracks.len()];
    if any_solo {
        let mut active = soloed.clone();
        let mut stack: Vec<usize> = (0..tracks.len()).filter(|&i| soloed[i]).collect();
        while let Some(i) = stack.pop() {
            for &p in &out_pred[i] {
                if !active[p] {
                    active[p] = true;
                    stack.push(p);
                }
            }
        }
        let mut down = soloed.clone();
        let mut stack: Vec<usize> = (0..tracks.len()).filter(|&i| soloed[i]).collect();
        while let Some(i) = stack.pop() {
            for &j in &succ[i] {
                if !down[j] {
                    down[j] = true;
                    stack.push(j);
                }
            }
        }
        for i in 0..tracks.len() {
            audible[i] = active[i] || down[i];
        }
    }

    // Tempo (Main's tempo lane folds into the map).
    let mut tempo = project.tempo.clone();
    if let Some(lane) = tracks[main].automation.iter().find(|l| l.target == AutomationTarget::Tempo && usable(l)) {
        tempo = fold_tempo_lane(&project.tempo, lane);
    }

    // Devices.
    cx.pool.begin();
    let mut chains = Vec::with_capacity(tracks.len());
    let mut handles_per_track = Vec::with_capacity(tracks.len());
    let mut chain_lat = vec![0u32; tracks.len()];
    {
        for (i, t) in tracks.iter().enumerate() {
            let mut overrides = HashMap::new();
            collect_macro_overrides(&t.devices, &mut overrides);
            let mut dc = DevCx { cx: &mut *cx, overrides, handles: HashMap::new(), warnings: Vec::new(), side_slots: &side_slots, track: t.id };
            let (nodes, lat) = if !t.kind.carries_audio() {
                (Vec::new(), 0)
            } else if frozen[i].is_none() {
                compile_devices(&t.devices, &mut dc)
            } else {
                // A frozen track runs none of its devices, except the modulators that drive its
                // fader or pan: they keep modulating the mixer, reading the frozen audio.
                compile_devices(&frozen_fader_mods(&t.devices), &mut dc)
            };
            warnings.append(&mut dc.warnings);
            chain_lat[i] = lat;
            chains.push(nodes);
            handles_per_track.push(dc.handles);
        }
    }
    cx.pool.end();

    // PDC.
    let mut in_lat = vec![0u32; tracks.len()];
    let mut out_lat = vec![0u32; tracks.len()];
    let mut has_input = vec![false; tracks.len()];
    for &i in &order {
        out_lat[i] = in_lat[i] + chain_lat[i];
        for &j in &succ[i] {
            in_lat[j] = in_lat[j].max(out_lat[i]);
            has_input[j] = true;
        }
    }
    let mut out_in = 0u32;
    for i in 0..tracks.len() {
        if dest[i] == Dest::Out {
            out_in = out_in.max(out_lat[i]);
        }
    }

    // Scenes.
    let scenes: Vec<_> = project.scenes.iter().map(|s| s.id).collect();
    let mut scene_lookup: Vec<_> = scenes.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    scene_lookup.sort();

    // Meter bank and session status.
    let ids: Vec<TrackId> = tracks.iter().map(|t| t.id).collect();
    let meters = match cx.prev_meters.take() {
        Some(m) if {
            let mut sorted = ids.clone();
            sorted.sort();
            m.ids == sorted
        } =>
        {
            m
        }
        _ => Arc::new(MeterBank::new(ids)),
    };

    // Tracks, in graph order.
    let mut nodes: Vec<Option<TrackNode>> = (0..tracks.len()).map(|_| None).collect();
    let mut chains: Vec<Option<Vec<DeviceNode>>> = chains.into_iter().map(Some).collect();
    let mut session_tracks = Vec::new();
    let mut hosted_devices: std::collections::HashSet<DeviceId> = std::collections::HashSet::new();
    for t in tracks {
        visit_all(&t.devices, &mut |d| {
            if matches!(d.kind, DeviceKind::Plugin { .. }) {
                hosted_devices.insert(d.id);
            }
        });
    }
    // Parameters some lane drives in this graph (see `ProcessorPool::sync_automation`).
    let mut automated: BTreeSet<(DeviceId, u32)> = BTreeSet::new();
    for &i in &order {
        let t = &tracks[i];
        let handles = &handles_per_track[i];

        // Clips.
        let frozen_here = frozen[i].take();
        let is_frozen = frozen_here.is_some();
        let mut clips: Vec<CompiledClip> = Vec::new();
        let mut clip_index: HashMap<ClipId, usize> = HashMap::new();
        let mut get_clip = |id: ClipId, clips: &mut Vec<CompiledClip>, warnings: &mut Vec<String>| -> Option<usize> {
            if let Some(&k) = clip_index.get(&id) {
                return Some(k);
            }
            let clip = project.clips.get(&id)?;
            let compiled = match project.sources.get(&clip.source)? {
                Source::Notes(n) if t.kind == TrackKind::Instrument => {
                    CompiledClip::midi_grooved(id, clip, n, project.clip_groove(id).map(|e| (e, project.groove_amount)))
                }
                Source::Audio(a) if t.kind == TrackKind::Audio => {
                    let audio = cx.samples.get(&a.path, sr as u32);
                    if audio.is_none()
                        && let Some(e) = cx.samples.error(&a.path, sr as u32) {
                            warnings.push(format!("audio '{}': {e}", a.path.display()));
                        }
                    // Each clip reads the warp of its own source: a clip transposed
                    // or stretched alone has a copy of the file's source.
                    let mut c = CompiledClip::audio(id, clip, AudioClipData::of_bpm_in(&a.warp, audio, project.tempo.initial_bpm(), cx.samples));
                    c.attach_stream(cx.streams, (t.id.0, id.0), project.tempo.initial_bpm(), sr);
                    c
                }
                _ => return None,
            };
            clips.push(compiled);
            clip_index.insert(id, clips.len() - 1);
            Some(clips.len() - 1)
        };
        let mut arrangement = Vec::new();
        let mut slots = vec![None; scenes.len()];
        if let Some(f) = frozen_here {
            let mut c = CompiledClip::audio(ClipId(0), &f.clip, f.data);
            // (Keyed apart from the clips: the track's frozen file.)
            c.attach_stream(cx.streams, (t.id.0, u64::MAX), project.tempo.initial_bpm(), sr);
            clips.push(c);
            arrangement.push(ArrPlacement { clip: 0, start: 0 });
        } else if t.kind.has_clips() {
            for p in t.arrangement.iter().filter(|p| p.lane == 0) {
                if let Some(k) = get_clip(p.clip, &mut clips, &mut warnings) {
                    arrangement.push(ArrPlacement { clip: k, start: p.start });
                }
            }
            arrangement.sort_by_key(|p| p.start);
            for (sid, cid) in &t.session_slots {
                if let Some(si) = scenes.iter().position(|s| s == sid) {
                    slots[si] = get_clip(*cid, &mut clips, &mut warnings);
                }
            }
        }
        let session_index = if t.kind.has_clips() {
            session_tracks.push((t.id, (0..scenes.len()).filter(|&s| slots[s].is_some()).collect::<Vec<_>>()));
            Some(session_tracks.len() - 1)
        } else {
            None
        };

        // Automation.
        let mut volume_lane = None;
        let mut pan_lane = None;
        let mut mute_lane = None;
        let mut send_lanes: HashMap<u16, AutomationLane> = HashMap::new();
        let mut param_autos = Vec::new();
        for lane in t.automation.iter().filter(|l| usable(l)) {
            match lane.target {
                AutomationTarget::Volume => volume_lane = Some(lane.clone()),
                AutomationTarget::Pan => pan_lane = Some(lane.clone()),
                AutomationTarget::Mute => mute_lane = Some(lane.clone()),
                AutomationTarget::Send { index } => {
                    send_lanes.insert(index, lane.clone());
                }
                AutomationTarget::DeviceParam { .. } | AutomationTarget::DeviceEnabled { .. } | AutomationTarget::Macro { .. } if is_frozen => {}
                AutomationTarget::DeviceParam { device, param } => match handles.get(&device).and_then(|h| h.proc.as_ref()) {
                    // Hosted and stock alike: timed events in the device's node.
                    Some(_) => {
                        automated.insert((device, param));
                        param_autos.push(ParamAuto { lane: lane.clone(), target: ParamTarget::Timed(device, param), last: f32::NAN });
                    }
                    None => warnings.push(format!("track '{}': automation targets unknown device {device}", t.name)),
                },
                AutomationTarget::DeviceEnabled { device } => {
                    if let Some(h) = handles.get(&device) {
                        param_autos.push(ParamAuto { lane: lane.clone(), target: ParamTarget::Enabled(h.enabled.clone()), last: f32::NAN });
                    }
                }
                AutomationTarget::Macro { device, index } => {
                    let mappings = find_rack(&t.devices, device).and_then(|r| r.macros.get(index as usize)).map(|m| m.mappings.as_slice()).unwrap_or_default();
                    let maps = mappings
                        .iter()
                        .filter(|mm| handles.get(&mm.device).is_some_and(|h| h.proc.is_some()))
                        .map(|mm| (mm.device, mm.param, mm.min, mm.max))
                        .collect::<Vec<_>>();
                    if !maps.is_empty() {
                        automated.extend(maps.iter().map(|m| (m.0, m.1)));
                        param_autos.push(ParamAuto { lane: lane.clone(), target: ParamTarget::TimedMacro(maps), last: f32::NAN });
                    }
                }
                // Tempo is read from the map; transpose only exists in clip envelopes.
                AutomationTarget::Tempo | AutomationTarget::Transpose => {}
            }
        }

        // Modulators: the targets each drives, their base values and the room
        // for their events (a target device is driven like an automated one).
        let mut mods: Vec<ModNode> = Vec::new();
        let mut has_fader_mod = false;
        let mut sources: Vec<(DeviceId, String, Vec<aitrack_model::ModMapping>)> = Vec::new();
        visit_all(&t.devices, &mut |d| {
            if d.is_modulator() && !d.mods.is_empty() {
                let mut mappings = d.mods.clone();
                if is_frozen {
                    mappings.retain(|m| !matches!(m.target, ModTarget::Param { .. }));
                }
                if !mappings.is_empty() {
                    sources.push((d.id, d.name.clone(), mappings));
                }
            }
        });
        for (src_id, src_name, src_mods) in sources {
            if handles.get(&src_id).and_then(|h| h.proc.as_ref()).is_none() {
                continue;
            }
            let mut targets: Vec<ModTargetNode> = Vec::new();
            for (k, mm) in src_mods.iter().enumerate() {
                let dest = match mm.target {
                    ModTarget::Param { device, param } => {
                        if device == src_id || handles.get(&device).and_then(|h| h.proc.as_ref()).is_none() {
                            warnings.push(format!("track '{}': modulator '{}' targets an unknown device", t.name, src_name));
                            continue;
                        }
                        ModDest::Param(device, param)
                    }
                    ModTarget::Volume => ModDest::Volume,
                    ModTarget::Pan => ModDest::Pan,
                };
                // A parameter has one modulator: the first mapped wins.
                if targets.iter().any(|x| x.dest == dest) || mods.iter().any(|m| m.targets.iter().any(|x| x.dest == dest)) {
                    continue;
                }
                let base = match dest {
                    ModDest::Param(device, param) => {
                        automated.insert((device, param));
                        project.device(device).and_then(|dv| dv.params.get(&param).copied()).or_else(|| cx.pool.default_param(device, param)).unwrap_or(0.0)
                    }
                    ModDest::Volume | ModDest::Pan => {
                        has_fader_mod = true;
                        0.0
                    }
                };
                targets.push(ModTargetNode { dest, index: k as u16, min: mm.min, max: mm.max, base, last: f32::NAN });
            }
            if !targets.is_empty() {
                mods.push(ModNode { source: src_id, targets, steps: Vec::with_capacity(max / CONTROL_BLOCK + 2) });
            }
        }

        // Room for the timed automation events of each device a lane reaches:
        // one per control block and per breakpoint in a block. A hosted plugin
        // also gets the buffer its events are merged with the block's; a stock
        // device, the scratch it is processed in pieces with.
        if let Some(devices) = chains[i].as_mut() {
            let per_lane = max / CONTROL_BLOCK + 2;
            for pa in &param_autos {
                let breaks = pa.lane.points.len().min(max);
                let mut reach = |device: DeviceId| {
                    if let Some(node) = crate::graph::find_proc_mut(devices, device) {
                        let cap = node.auto_events.events.capacity() + per_lane + breaks;
                        node.auto_events = EventList::with_capacity(cap);
                        if hosted_devices.contains(&device) {
                            node.merged = EventList::with_capacity(cap + EVENT_CAPACITY);
                        } else if node.chunk.is_none() {
                            node.chunk = Some(Box::new(ChunkScratch { buf: AudioBuffer::new(2, max), events: EventList::with_capacity(EVENT_CAPACITY) }));
                        }
                    }
                };
                match &pa.target {
                    ParamTarget::Timed(device, _) => reach(*device),
                    ParamTarget::TimedMacro(maps) => maps.iter().for_each(|m| reach(m.0)),
                    ParamTarget::Enabled(_) => {}
                }
            }
            for tg in mods.iter().flat_map(|m| &m.targets) {
                if let ModDest::Param(device, _) = tg.dest
                    && let Some(node) = crate::graph::find_proc_mut(devices, device)
                {
                    // Up to one event per control block and target, on top of any automation.
                    let cap = node.auto_events.events.capacity() + per_lane;
                    node.auto_events = EventList::with_capacity(cap);
                    if hosted_devices.contains(&device) {
                        node.merged = EventList::with_capacity(cap + EVENT_CAPACITY);
                    } else if node.chunk.is_none() {
                        node.chunk = Some(Box::new(ChunkScratch { buf: AudioBuffer::new(2, max), events: EventList::with_capacity(EVENT_CAPACITY) }));
                    }
                }
            }
        }

        // Sends.
        let send_nodes = sends[i]
            .iter()
            .map(|&(k, j)| {
                let s = &t.sends[k];
                SendNode {
                    index: k as u16,
                    target: gpos[j],
                    target_id: tracks[j].id,
                    level_db: s.level_db,
                    pre_fader: s.pre_fader,
                    lane: send_lanes.remove(&(k as u16)),
                    cur: 0.0,
                    primed: false,
                    delay: DelayLine::new(2, (in_lat[j] - out_lat[i]) as usize),
                    buf: AudioBuffer::new(2, max),
                }
            })
            .collect();

        let (output, out_delay) = match dest[i] {
            Dest::Track(j) => (OutputTarget::Track(gpos[j]), (in_lat[j] - out_lat[i]) as usize),
            Dest::Out => (OutputTarget::Out, (out_in - out_lat[i]) as usize),
            Dest::None => (OutputTarget::None, 0),
        };
        let clip_delay = if t.kind.has_clips() && has_input[i] { in_lat[i] as usize } else { 0 };

        nodes[gpos[i]] = Some(TrackNode {
            id: t.id,
            // A frozen track is a plain audio track that cannot be armed.
            kind: if is_frozen { TrackKind::Audio } else { t.kind },
            arm: t.arm && !is_frozen,
            monitor: t.monitor,
            input_sel: None,
            no_input: false,
            in_buf: AudioBuffer::new(2, if t.kind == TrackKind::Audio { max } else { 0 }),
            has_input: has_input[i],
            devices: chains[i].take().unwrap_or_default(),
            param_autos,
            mods,
            beat_steps: Vec::with_capacity(max / CONTROL_BLOCK + 2),
            has_fader_mod,
            mod_vol: Vec::with_capacity(if has_fader_mod { max / CONTROL_BLOCK + 2 } else { 0 }),
            mod_pan: Vec::with_capacity(if has_fader_mod { max / CONTROL_BLOCK + 2 } else { 0 }),
            buf: AudioBuffer::new(2, max),
            chunk_buf: AudioBuffer::new(2, CONTROL_BLOCK),
            chunk_events: EventList::with_capacity(EVENT_CAPACITY),
            clip_buf: AudioBuffer::new(2, if clip_delay > 0 { max } else { 0 }),
            clip_delay: DelayLine::new(2, clip_delay),
            clips,
            arrangement,
            slots,
            session_index,
            session: SessionRt::default(),
            events: EventList::with_capacity(EVENT_CAPACITY),
            live: EventList::with_capacity(256),
            active: ActiveNotes::new(),
            rng: Rng(0x9E37_79B9 ^ (t.id.0 as u32).wrapping_mul(2_654_435_761) | 1),
            release_all: false,
            panic_notes: false,
            volume_db: t.volume_db,
            pan: t.pan,
            mute: t.mute,
            silenced: !audible[i] && t.kind != TrackKind::Main,
            vca: t.vca.and_then(|v| index.get(&v)).filter(|&&v| tracks[v].kind == TrackKind::Vca).map(|&v| gpos[v]),
            volume_lane,
            pan_lane,
            mute_lane,
            vca_out: (0.0, 0.0, false),
            vca_in: (0.0, 0.0, false),
            cur_gain: [0.0; 2],
            gain_primed: false,
            sends: send_nodes,
            output,
            out_delay: DelayLine::new(2, out_delay),
            fader_latency: out_lat[i],
            chain_latency_in: in_lat[i],
            meter: meters.index(t.id),
            meter_acc: MeterAcc::default(),
            tap: (cx.taps.contains(&t.id) && t.kind.carries_audio()).then(|| {
                Box::new(TapNode {
                    delay: DelayLine::new(2, (out_in.saturating_sub(out_lat[i])) as usize),
                    scratch: AudioBuffer::new(2, max),
                    block: AudioBuffer::new(2, max),
                })
            }),
            analysis: cx.analysis.clone().filter(|_| t.kind.carries_audio()),
            side_out: side_slots.get(&t.id).copied(),
            side_scratch: AudioBuffer::new(2, if side_src[i].is_empty() { 0 } else { max }),
        });
    }
    let mut tracks_out: Vec<TrackNode> = nodes.into_iter().map(|n| n.expect("every track placed")).collect();
    // A parameter whose automation is gone goes back to the knob's value.
    for (device, param, value) in cx.pool.sync_automation(automated) {
        for t in &mut tracks_out {
            if let Some(node) = find_proc_mut(&mut t.devices, device) {
                node.init_params.retain(|(id, _)| *id != param);
                node.init_params.push((param, value));
                break;
            }
        }
    }
    let inputs = tracks_out.iter().map(|t| AudioBuffer::new(2, if t.has_input { max } else { 0 })).collect();
    let mut track_lookup: Vec<_> = tracks_out.iter().enumerate().map(|(g, t)| (t.id, g)).collect();
    track_lookup.sort();
    let mut warnings_out = warnings;
    warnings_out.dedup();
    let schedule = crate::multicore::Schedule::build(&tracks_out);

    Ok(CompiledGraph {
        sample_rate: sr,
        max_frames: max,
        tempo,
        tracks: tracks_out,
        inputs,
        side: (0..side_slots.len()).map(|_| AudioBuffer::new(2, max)).collect(),
        out_bus: AudioBuffer::new(2, max),
        out_acc: MeterAcc::default(),
        track_lookup,
        scenes,
        scene_lookup,
        total_latency: out_in,
        meters,
        session_status: Arc::new(SessionStatus::new(project.scenes.iter().map(|s| s.id).collect(), session_tracks)),
        warnings: warnings_out,
        track_latency: tracks.iter().enumerate().map(|(i, t)| (t.id, chain_lat[i])).collect(),
        new_crashes: 0,
        midi_dropped: 0,
        sanitized: 0,
        generation: 0,
        schedule,
    })
}

// ───────────────────────────── Parameter patches ─────────────────────────────

/// One change of a running graph that needs no compile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Patch {
    /// A device parameter (normalised).
    Param { device: DeviceId, param: u32, value: f32 },
    Volume { track: TrackId, db: f32 },
    Pan { track: TrackId, pan: f32 },
    Send { track: TrackId, index: u16, db: f32 },
    /// A macro's value, for the control thread's copy of the project only
    /// (its mapped parameters are separate `Param` patches).
    MacroValue { rack: DeviceId, index: u16, value: f32 },
    /// A rack's Chain Select value.
    Selector { rack: DeviceId, value: u8 },
    /// How strongly a pool groove acts.
    GrooveSettings { id: aitrack_model::GrooveId, settings: aitrack_model::GrooveSettings },
    /// The global groove amount.
    GrooveAmount(f32),
    /// The range of mapping `index` of a modulator.
    ModRange { device: DeviceId, index: u16, min: f32, max: f32 },
}

/// If `new` differs from `old` only in device parameters, track volume and
/// pan, send levels and fields the compiler never reads (names, colours,
/// heights, reported latency…), the changes as [`Patch`]es to apply to the
/// graph compiled from `old`; `None` when anything else changed (structure,
/// routing, clips, automation, mute / solo, states, macros…) or a change
/// cannot be expressed as a patch.
///
/// Every struct is destructured without `..`, so adding a field to the model
/// stops this from compiling until it is decided here whether the compiler
/// reads it.
pub(crate) fn param_patches(old: &Project, new: &Project) -> Option<Vec<Patch>> {
    let Project { format_version: _, name: _, tempo, scale: _, sample_rate_hint: _, tracks, scenes, clips, sources, loop_region: _, ids: _, grooves, groove_amount, clip_grooves } = old;
    let Project { format_version: _, name: _, tempo: tempo_b, scale: _, sample_rate_hint: _, tracks: tracks_b, scenes: scenes_b, clips: clips_b, sources: sources_b, loop_region: _, ids: _, grooves: grooves_b, groove_amount: groove_amount_b, clip_grooves: clip_grooves_b } = new;
    if tempo != tempo_b || scenes != scenes_b || clips != clips_b || sources != sources_b || tracks.len() != tracks_b.len() {
        return None;
    }
    let mut out = Vec::new();
    // The groove pool: a groove's settings and the global amount move live;
    // its template (cells, base, name) or its place in the pool is a compile.
    if grooves.len() != grooves_b.len() || clip_grooves != clip_grooves_b {
        return None;
    }
    for (a, b) in grooves.iter().zip(grooves_b) {
        if a.id != b.id || a.groove != b.groove {
            return None;
        }
        if a.settings != b.settings {
            out.push(Patch::GrooveSettings { id: a.id, settings: b.settings });
        }
    }
    if groove_amount != groove_amount_b {
        out.push(Patch::GrooveAmount(*groove_amount_b));
    }
    for (a, b) in tracks.iter().zip(tracks_b) {
        if a == b {
            continue;
        }
        let Track {
            id, kind, name: _, color: _, volume_db, pan, mute, solo, arm, monitor, input, output, sends, parent, vca, collapsed: _, height: _, devices, session_slots, arrangement, automation,
            reported_latency: _, scale: _, keyboard_input: _, take_lanes: _, comp: _, freeze,
        } = a;
        let Track {
            id: id_b, kind: kind_b, name: _, color: _, volume_db: volume_b, pan: pan_b, mute: mute_b, solo: solo_b, arm: arm_b, monitor: monitor_b, input: input_b, output: output_b, sends: sends_b,
            parent: parent_b, vca: vca_b, collapsed: _, height: _, devices: devices_b, session_slots: session_b, arrangement: arrangement_b, automation: automation_b,
            reported_latency: _, scale: _, keyboard_input: _, take_lanes: _, comp: _, freeze: freeze_b,
        } = b;
        if id != id_b
            || kind != kind_b
            || freeze != freeze_b
            || mute != mute_b
            || solo != solo_b
            || arm != arm_b
            || monitor != monitor_b
            || input != input_b
            || output != output_b
            || parent != parent_b
            || vca != vca_b
            || session_slots != session_b
            || arrangement != arrangement_b
            || automation != automation_b
            || sends.len() != sends_b.len()
        {
            return None;
        }
        if volume_db != volume_b {
            out.push(Patch::Volume { track: *id, db: *volume_b });
        }
        if pan != pan_b {
            out.push(Patch::Pan { track: *id, pan: *pan_b });
        }
        for (k, (sa, sb)) in sends.iter().zip(sends_b).enumerate() {
            if sa.target != sb.target || sa.pre_fader != sb.pre_fader {
                return None;
            }
            if sa.level_db != sb.level_db {
                out.push(Patch::Send { track: *id, index: k as u16, db: sb.level_db });
            }
        }
        let mut overrides = HashMap::new();
        collect_macro_overrides(devices_b, &mut overrides);
        if !diff_devices(devices, devices_b, &overrides, &mut out) {
            return None;
        }
    }
    Some(out)
}

/// Two rack chains (or returns) that differ at most by device parameters.
fn chain_diff(ca: &Chain, cb: &Chain, overrides: &HashMap<DeviceId, BTreeMap<u32, f32>>, out: &mut Vec<Patch>) -> bool {
    let Chain { name: _, color: _, devices, volume_db, pan, mute, solo, note, key_range, velocity_range, selector_range, key_fade, vel_fade, sel_fade, choke, play_note, sends, output } = ca;
    let Chain {
        name: _, color: _, devices: devices_b, volume_db: volume_b, pan: pan_b, mute: mute_b, solo: solo_b, note: note_b, key_range: key_b, velocity_range: velocity_b,
        selector_range: selector_b, key_fade: key_fade_b, vel_fade: vel_fade_b, sel_fade: sel_fade_b, choke: choke_b, play_note: play_note_b, sends: sends_b, output: output_b,
    } = cb;
    volume_db == volume_b
        && pan == pan_b
        && mute == mute_b
        && solo == solo_b
        && note == note_b
        && key_range == key_b
        && velocity_range == velocity_b
        && selector_range == selector_b
        && (key_fade, vel_fade, sel_fade, choke, play_note, sends, output) == (key_fade_b, vel_fade_b, sel_fade_b, choke_b, play_note_b, sends_b, output_b)
        && diff_devices(devices, devices_b, overrides, out)
}

/// Compare two device chains: `true` when they differ only by parameters
/// (collected into `out`).
fn diff_devices(a: &[Device], b: &[Device], overrides: &HashMap<DeviceId, BTreeMap<u32, f32>>, out: &mut Vec<Patch>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).all(|(da, db)| {
        let Device { id, name: _, kind, enabled, params, state, preset: _, collapsed: _, sidechain, mods } = da;
        let Device { id: id_b, name: _, kind: kind_b, enabled: enabled_b, params: params_b, state: state_b, preset: _, collapsed: _, sidechain: sidechain_b, mods: mods_b } = db;
        if id != id_b || enabled != enabled_b || state != state_b || sidechain != sidechain_b || mods.len() != mods_b.len() {
            return false;
        }
        // A mapping whose range moved is a patch; a different target or order is a compile.
        for (k, (ma, mb)) in mods.iter().zip(mods_b).enumerate() {
            if ma.target != mb.target {
                return false;
            }
            if (ma.min, ma.max) != (mb.min, mb.max) {
                out.push(Patch::ModRange { device: *id, index: k as u16, min: mb.min, max: mb.max });
            }
        }
        match (kind, kind_b) {
            (DeviceKind::Rack(ra), DeviceKind::Rack(rb)) => {
                let Rack { kind, chains, macros, variations: _, chain_selector, returns } = ra;
                let Rack { kind: kind_b, chains: chains_b, macros: macros_b, variations: _, chain_selector: selector_b, returns: returns_b } = rb;
                // (A chain's zones, fades, choke, notes, sends and output shape the compiled rack: any change recompiles.)
                if kind != kind_b || chains.len() != chains_b.len() || macros.len() != macros_b.len() {
                    return false;
                }
                if chain_selector != selector_b {
                    out.push(Patch::Selector { rack: *id, value: *selector_b });
                }
                // A macro that only moved: its mapped parameters follow it.
                for (ma, mb) in macros.iter().zip(macros_b) {
                    let Macro { name: _, value, mappings } = ma;
                    let Macro { name: _, value: value_b, mappings: mappings_b } = mb;
                    if mappings != mappings_b {
                        return false;
                    }
                    if value != value_b {
                        out.extend(macro_patches(mappings_b, *value_b));
                    }
                }
                let chains_ok = returns.len() == returns_b.len()
                    && chains.iter().zip(chains_b).chain(returns.iter().zip(returns_b)).all(|(ca, cb)| chain_diff(ca, cb, overrides, out));
                if !chains_ok {
                    return false;
                }
            }
            (DeviceKind::Rack(_), _) | (_, DeviceKind::Rack(_)) => return false,
            _ if kind != kind_b => return false,
            _ => {}
        }
        if params.keys().any(|k| !params_b.contains_key(k)) {
            return false;
        }
        for (&param, &value) in params_b {
            if params.get(&param) != Some(&value) {
                // A macro drives this parameter: its value is not the knob's.
                if overrides.get(id).is_some_and(|m| m.contains_key(&param)) {
                    return false;
                }
                out.push(Patch::Param { device: *id, param, value });
            }
        }
        true
    })
}

/// The parameter values a macro at `value` (0..=1) puts on its mapped
/// parameters: `min + (max - min) * value`, the range inverted when
/// `min > max`.
pub(crate) fn macro_patches(mappings: &[MacroMapping], value: f32) -> impl Iterator<Item = Patch> + '_ {
    let v = value.clamp(0.0, 1.0);
    mappings.iter().map(move |m| Patch::Param { device: m.device, param: m.param, value: m.min + (m.max - m.min) * v })
}

/// Apply a [`Patch`] to a project snapshot (the control thread keeps its copy
/// in step with the fast path).
pub(crate) fn apply_patch(project: &mut Project, patch: &Patch) {
    fn param(devices: &mut [Device], device: DeviceId, p: u32, v: f32) -> bool {
        for d in devices {
            if d.id == device {
                d.params.insert(p, v);
                return true;
            }
            if let DeviceKind::Rack(r) = &mut d.kind
                && r.chains.iter_mut().chain(r.returns.iter_mut()).any(|c| param(&mut c.devices, device, p, v))
            {
                return true;
            }
        }
        false
    }
    match *patch {
        Patch::Param { device, param: p, value } => {
            project.tracks.iter_mut().any(|t| param(&mut t.devices, device, p, value));
        }
        Patch::Volume { track, db } => {
            if let Some(t) = project.track_mut(track) {
                t.volume_db = db;
            }
        }
        Patch::Pan { track, pan } => {
            if let Some(t) = project.track_mut(track) {
                t.pan = pan;
            }
        }
        Patch::Send { track, index, db } => {
            if let Some(s) = project.track_mut(track).and_then(|t| t.sends.get_mut(index as usize)) {
                s.level_db = db;
            }
        }
        Patch::GrooveSettings { id, settings } => {
            if let Some(g) = project.grooves.iter_mut().find(|g| g.id == id) {
                g.settings = settings.normalized();
            }
        }
        Patch::GrooveAmount(a) => project.groove_amount = a.clamp(0.0, aitrack_model::MAX_GROOVE_AMOUNT),
        Patch::ModRange { device, index, min, max } => {
            if let Some(m) = project.device_mut(device).and_then(|d| d.mods.get_mut(index as usize)) {
                m.min = min;
                m.max = max;
            }
        }
        Patch::Selector { rack, value } => {
            fn set(devices: &mut [Device], rack: DeviceId, value: u8) -> bool {
                for d in devices {
                    if let DeviceKind::Rack(r) = &mut d.kind {
                        if d.id == rack {
                            r.chain_selector = value;
                            return true;
                        }
                        if r.chains.iter_mut().chain(r.returns.iter_mut()).any(|c| set(&mut c.devices, rack, value)) {
                            return true;
                        }
                    }
                }
                false
            }
            project.tracks.iter_mut().any(|t| set(&mut t.devices, rack, value));
        }
        Patch::MacroValue { rack, index, value } => {
            fn set(devices: &mut [Device], rack: DeviceId, index: usize, value: f32) -> bool {
                for d in devices {
                    if let DeviceKind::Rack(r) = &mut d.kind {
                        if d.id == rack {
                            if let Some(m) = r.macros.get_mut(index) {
                                m.value = value;
                            }
                            return true;
                        }
                        if r.chains.iter_mut().chain(r.returns.iter_mut()).any(|c| set(&mut c.devices, rack, index, value)) {
                            return true;
                        }
                    }
                }
                false
            }
            project.tracks.iter_mut().any(|t| set(&mut t.devices, rack, index as usize, value));
        }
    }
}
