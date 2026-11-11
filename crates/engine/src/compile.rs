//! Graph compiler: `Project` snapshot → [`CompiledGraph`]. Runs on the
//! control thread (or the offline renderer), never on the audio thread.
//!
//! Routing rules:
//! * `Output::Main` goes to the nearest enclosing **Group** (walking up
//!   through Folders), else to Main. Folders and VCAs carry no audio.
//! * `Output::Track(id)` must name an audio-carrying track other than itself.
//! * `Output::Hardware(_)` goes straight to the device output bus (aligned
//!   with Main by PDC); Main itself always goes there.
//! * Sends go to any audio-carrying track (normally Returns).
//! * Any routing cycle (outputs + sends) is rejected with [`CompileError::Cycle`].
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
    AutomationLane, AutomationTarget, ClipId, Device, DeviceId, DeviceKind, Output, Project, Source, Track, TrackId, TrackKind,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
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
            for c in &rack.chains {
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
                let max_lat = compiled.iter().map(|c| c.1).max().unwrap_or(0);
                let any_solo = rack.chains.iter().any(|c| c.solo);
                let chains = rack
                    .chains
                    .iter()
                    .zip(compiled)
                    .map(|(c, (devices, lat))| {
                        let silent = c.mute || (any_solo && !c.solo);
                        let (l, r) = pan_gains(c.pan);
                        let g = if silent { 0.0 } else { db_to_gain(c.volume_db) };
                        ChainNode {
                            devices,
                            gain: (l * g, r * g),
                            note: c.note,
                            key_range: c.key_range,
                            velocity_range: c.velocity_range,
                            buf: AudioBuffer::new(2, max),
                            events: EventList::with_capacity(EVENT_CAPACITY),
                            delay: DelayLine::new(2, (max_lat - lat) as usize),
                        }
                    })
                    .collect();
                if d.enabled {
                    latency += max_lat;
                }
                dc.handles.insert(d.id, Handle { proc: None, enabled: enabled.clone() });
                nodes.push(DeviceNode::Rack(RackNode { id: d.id, kind: rack.kind, enabled, chains, input: AudioBuffer::new(2, max) }));
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
    (nodes, latency)
}

fn find_rack(chain: &[Device], id: DeviceId) -> Option<&aitrack_model::Rack> {
    for d in chain {
        if let DeviceKind::Rack(r) = &d.kind {
            if d.id == id {
                return Some(r);
            }
            for c in &r.chains {
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
pub fn compile(project: &Project, cx: &mut CompileContext<'_>) -> Result<CompiledGraph, CompileError> {
    let tracks = &project.tracks;
    let index: HashMap<TrackId, usize> = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let main = tracks.iter().position(|t| t.kind == TrackKind::Main).ok_or(CompileError::NoMain)?;
    let sr = cx.sample_rate;
    let max = cx.max_frames;
    let mut warnings = Vec::new();

    // Routing.
    let mut dest = Vec::with_capacity(tracks.len());
    for i in 0..tracks.len() {
        dest.push(resolve_output(tracks, &index, i, main)?);
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

    // Topological order (Kahn), stable in project order.
    let mut indeg = vec![0usize; tracks.len()];
    for s in &succ {
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
    if order.len() != tracks.len() {
        let cyc: Vec<TrackId> = (0..tracks.len()).filter(|&i| indeg[i] > 0).map(|i| tracks[i].id).collect();
        return Err(CompileError::Cycle(cyc));
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
            let mut dc = DevCx { cx: &mut *cx, overrides, handles: HashMap::new(), warnings: Vec::new() };
            let (nodes, lat) = if t.kind.carries_audio() { compile_devices(&t.devices, &mut dc) } else { (Vec::new(), 0) };
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
    for &i in &order {
        let t = &tracks[i];
        let handles = &handles_per_track[i];

        // Clips.
        let mut clips: Vec<CompiledClip> = Vec::new();
        let mut clip_index: HashMap<ClipId, usize> = HashMap::new();
        let mut get_clip = |id: ClipId, clips: &mut Vec<CompiledClip>, warnings: &mut Vec<String>| -> Option<usize> {
            if let Some(&k) = clip_index.get(&id) {
                return Some(k);
            }
            let clip = project.clips.get(&id)?;
            let compiled = match project.sources.get(&clip.source)? {
                Source::Notes(n) if t.kind == TrackKind::Instrument => CompiledClip::midi(id, clip, n),
                Source::Audio(a) if t.kind == TrackKind::Audio => {
                    let audio = cx.samples.get(&a.path, sr as u32);
                    if audio.is_none()
                        && let Some(e) = cx.samples.error(&a.path, sr as u32) {
                            warnings.push(format!("audio '{}': {e}", a.path.display()));
                        }
                    let warped = a.warp.enabled && a.warp.original_bpm.is_some_and(|b| b > 0.0);
                    CompiledClip::audio(
                        id,
                        clip,
                        AudioClipData { audio, warped, original_bpm: a.warp.original_bpm.unwrap_or(120.0), transpose: a.warp.transpose },
                    )
                }
                _ => return None,
            };
            clips.push(compiled);
            clip_index.insert(id, clips.len() - 1);
            Some(clips.len() - 1)
        };
        let mut arrangement = Vec::new();
        let mut slots = vec![None; scenes.len()];
        if t.kind.has_clips() {
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
                AutomationTarget::DeviceParam { device, param } => {
                    match handles.get(&device).and_then(|h| h.proc.clone()) {
                        Some(p) => param_autos.push(ParamAuto { lane: lane.clone(), target: ParamTarget::Param(p, param), last: f32::NAN }),
                        None => warnings.push(format!("track '{}': automation targets unknown device {device}", t.name)),
                    }
                }
                AutomationTarget::DeviceEnabled { device } => {
                    if let Some(h) = handles.get(&device) {
                        param_autos.push(ParamAuto { lane: lane.clone(), target: ParamTarget::Enabled(h.enabled.clone()), last: f32::NAN });
                    }
                }
                AutomationTarget::Macro { device, index } => {
                    let maps = find_rack(&t.devices, device)
                        .and_then(|r| r.macros.get(index as usize))
                        .map(|m| {
                            m.mappings
                                .iter()
                                .filter_map(|mm| handles.get(&mm.device).and_then(|h| h.proc.clone()).map(|p| (p, mm.param, mm.min, mm.max)))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !maps.is_empty() {
                        param_autos.push(ParamAuto { lane: lane.clone(), target: ParamTarget::Macro(maps), last: f32::NAN });
                    }
                }
                AutomationTarget::Tempo => {}
            }
        }

        // Sends.
        let send_nodes = sends[i]
            .iter()
            .map(|&(k, j)| {
                let s = &t.sends[k];
                SendNode {
                    target: gpos[j],
                    level_db: s.level_db,
                    pre_fader: s.pre_fader,
                    lane: send_lanes.remove(&(k as u16)),
                    cur: 0.0,
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
            kind: t.kind,
            arm: t.arm,
            monitor: t.monitor,
            has_input: has_input[i],
            devices: chains[i].take().unwrap_or_default(),
            param_autos,
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
        });
    }
    let tracks_out: Vec<TrackNode> = nodes.into_iter().map(|n| n.expect("every track placed")).collect();
    let inputs = tracks_out.iter().map(|t| AudioBuffer::new(2, if t.has_input { max } else { 0 })).collect();
    let mut track_lookup: Vec<_> = tracks_out.iter().enumerate().map(|(g, t)| (t.id, g)).collect();
    track_lookup.sort();
    let mut warnings_out = warnings;
    warnings_out.dedup();

    Ok(CompiledGraph {
        sample_rate: sr,
        max_frames: max,
        tempo,
        tracks: tracks_out,
        inputs,
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
    })
}
