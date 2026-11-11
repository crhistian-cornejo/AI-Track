//! The compiled, immutable-shape audio graph and its real-time processing.
//!
//! A [`CompiledGraph`] is built by [`crate::compile`] on the control thread
//! and then owned exclusively by the audio thread. Its *shape* never changes
//! there (no allocation); only runtime state (envelopes, active notes,
//! session playback, smoothers) mutates. When a new graph arrives, the audio
//! thread moves that runtime state over ([`CompiledGraph::adopt`]) and sends
//! the old graph back to be dropped off the audio thread.
//!
//! Per track, one segment is processed as:
//! clip player → device chain (racks, sub-blocked for automation) →
//! pre-fader sends → volume/pan/mute (ramped every 32 samples) →
//! post-fader sends → meter → PDC delay → destination input.

use crate::automation::{lane_value, pan_from_normalized, pan_gains, volume_db_from_normalized, CONTROL_BLOCK};
use crate::clip::{ActiveNotes, CompiledClip, EventList, Rng, TimeCtx};
use crate::delay::DelayLine;
use crate::shared::SharedProcessor;
use crate::status::{MeterBank, SessionStatus, NO_SCENE, STOP_SCENE};
use aitrack_dsp::{db_to_gain, AudioBuffer, Event, EventKind, ProcessContext, Transport};
use aitrack_model::{AutomationLane, DeviceId, FollowAction, LaunchMode, Monitor, Quantize, RackKind, SceneId, TempoMap, Tick, TrackId, TrackKind, PPQ};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(crate) const EVENT_CAPACITY: usize = 2048;

// ───────────────────────────── Devices ─────────────────────────────

pub(crate) enum DeviceNode {
    Proc(ProcNode),
    Rack(RackNode),
    /// A device no factory could create: passes audio through.
    Missing(DeviceId),
}

pub(crate) struct ProcNode {
    pub id: DeviceId,
    pub proc: Arc<SharedProcessor>,
    pub enabled: Arc<AtomicBool>,
    pub init_params: Vec<(u32, f32)>,
    pub is_instrument: bool,
}

pub(crate) struct RackNode {
    pub id: DeviceId,
    pub kind: RackKind,
    pub enabled: Arc<AtomicBool>,
    pub chains: Vec<ChainNode>,
    pub input: AudioBuffer,
}

pub(crate) struct ChainNode {
    pub devices: Vec<DeviceNode>,
    /// Volume × pan, zero when muted or silenced by a chain solo.
    pub gain: (f32, f32),
    pub note: Option<u8>,
    pub key_range: (u8, u8),
    pub velocity_range: (u8, u8),
    pub buf: AudioBuffer,
    pub events: EventList,
    /// Aligns chains of different latency before they are summed.
    pub delay: DelayLine,
}

impl DeviceNode {
    pub fn id(&self) -> DeviceId {
        match self {
            Self::Proc(p) => p.id,
            Self::Rack(r) => r.id,
            Self::Missing(id) => *id,
        }
    }
}

fn process_devices(devs: &mut [DeviceNode], buf: &mut AudioBuffer, events: &[Event], tr: &Transport, sr: f64) {
    for d in devs {
        match d {
            DeviceNode::Proc(p) => {
                if !p.enabled.load(Ordering::Relaxed) {
                    continue;
                }
                // SAFETY: the audio thread owns the installed graph and is the
                // only user of its processors (see `SharedProcessor`).
                let proc = unsafe { p.proc.get_mut() };
                if p.is_instrument {
                    buf.clear();
                }
                let mut ctx = ProcessContext { sample_rate: sr, buffer: buf, sidechain: None, events, transport: tr };
                proc.process(&mut ctx);
                p.proc.publish_latency(proc.latency());
            }
            DeviceNode::Rack(r) => {
                if r.enabled.load(Ordering::Relaxed) {
                    r.process(buf, events, tr, sr);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
}

fn in_range(v: u8, r: (u8, u8)) -> bool {
    v >= r.0.min(r.1) && v <= r.0.max(r.1)
}

impl RackNode {
    fn accepts(kind: RackKind, chain: &ChainNode, ev: &EventKind) -> bool {
        let key_ok = |key: u8| match (kind, chain.note) {
            (RackKind::Drum, Some(n)) => key == n,
            _ => in_range(key, chain.key_range),
        };
        match *ev {
            EventKind::NoteOn { key, velocity, .. } => {
                key_ok(key) && (kind == RackKind::Drum || in_range((velocity * 127.0).round() as u8, chain.velocity_range))
            }
            EventKind::NoteOff { key, .. } | EventKind::NoteExpression { key, .. } => key_ok(key),
            _ => true,
        }
    }

    fn process(&mut self, buf: &mut AudioBuffer, events: &[Event], tr: &Transport, sr: f64) {
        let n = buf.frames();
        self.input.set_frames(n);
        self.input.copy_from(buf);
        buf.clear();
        let kind = self.kind;
        for c in &mut self.chains {
            c.events.clear();
            for e in events {
                if Self::accepts(kind, c, &e.kind) {
                    c.events.push(e.offset, e.kind);
                }
            }
            c.buf.set_frames(n);
            c.buf.copy_from(&self.input);
            process_devices(&mut c.devices, &mut c.buf, &c.events.events, tr, sr);
            c.delay.process(&mut c.buf);
            let (gl, gr) = c.gain;
            if buf.channels() >= 2 && c.buf.channels() >= 2 {
                let (ol, or) = buf.stereo_mut();
                for (o, s) in ol.iter_mut().zip(c.buf.channel(0)) {
                    *o += s * gl;
                }
                for (o, s) in or.iter_mut().zip(c.buf.channel(1)) {
                    *o += s * gr;
                }
            }
        }
    }
}

fn for_each_proc(devs: &[DeviceNode], f: &mut impl FnMut(&ProcNode)) {
    for d in devs {
        match d {
            DeviceNode::Proc(p) => f(p),
            DeviceNode::Rack(r) => {
                for c in &r.chains {
                    for_each_proc(&c.devices, f);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
}

fn adopt_devices(new: &mut [DeviceNode], old: &mut [DeviceNode]) {
    for (n, o) in new.iter_mut().zip(old.iter_mut()) {
        if n.id() != o.id() {
            continue;
        }
        if let (DeviceNode::Rack(nr), DeviceNode::Rack(or)) = (n, o) {
            for (nc, oc) in nr.chains.iter_mut().zip(or.chains.iter_mut()) {
                nc.delay.adopt(&mut oc.delay);
                adopt_devices(&mut nc.devices, &mut oc.devices);
            }
        }
    }
}

// ───────────────────────────── Automation ─────────────────────────────

pub(crate) enum ParamTarget {
    Param(Arc<SharedProcessor>, u32),
    Enabled(Arc<AtomicBool>),
    /// Macro: `(processor, param, min, max)` for every mapping.
    Macro(Vec<(Arc<SharedProcessor>, u32, f32, f32)>),
}

pub(crate) struct ParamAuto {
    pub lane: AutomationLane,
    pub target: ParamTarget,
    pub last: f32,
}

impl ParamAuto {
    fn apply(&mut self, tick: f64) {
        let Some(v) = lane_value(&self.lane, tick) else { return };
        if (v - self.last).abs() < 1e-6 {
            return;
        }
        self.last = v;
        match &self.target {
            // SAFETY: audio thread, see `SharedProcessor`.
            ParamTarget::Param(p, id) => unsafe { p.get_mut().set_param(*id, v) },
            ParamTarget::Enabled(flag) => flag.store(v >= 0.5, Ordering::Relaxed),
            ParamTarget::Macro(maps) => {
                for (p, id, min, max) in maps {
                    // SAFETY: as above.
                    unsafe { p.get_mut().set_param(*id, min + (max - min) * v) };
                }
            }
        }
    }
}

// ───────────────────────────── Tracks ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputTarget {
    Track(usize),
    /// The hardware output bus (Main, or a track routed straight to hardware).
    Out,
    None,
}

pub(crate) struct SendNode {
    pub target: usize,
    pub level_db: f32,
    pub pre_fader: bool,
    pub lane: Option<AutomationLane>,
    pub cur: f32,
    pub delay: DelayLine,
    pub buf: AudioBuffer,
}

pub(crate) struct ArrPlacement {
    pub clip: usize,
    pub start: Tick,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Queued {
    /// `None` = stop.
    pub scene: Option<usize>,
    pub at: Tick,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SessionRt {
    pub playing: Option<(usize, Tick)>,
    pub queued: Option<Queued>,
    /// A session clip took over the track; the arrangement stays silent
    /// until "back to arrangement".
    pub override_arrangement: bool,
}

#[derive(Default)]
pub(crate) struct MeterAcc {
    pub peak: [f32; 2],
    pub sumsq: [f64; 2],
    pub count: usize,
    pub rms: [f32; 2],
}

impl MeterAcc {
    fn add(&mut self, buf: &AudioBuffer) {
        for ch in 0..2.min(buf.channels()) {
            for s in buf.channel(ch) {
                let a = s.abs();
                if a > self.peak[ch] {
                    self.peak[ch] = a;
                }
                self.sumsq[ch] += (*s as f64) * (*s as f64);
            }
        }
        self.count += buf.frames();
    }

    /// Fold into the ~300 ms RMS and return `(peak, rms)`, resetting the block.
    fn finish(&mut self, sr: f64) -> ([f32; 2], [f32; 2]) {
        let peak = self.peak;
        if self.count > 0 {
            let a = (-(self.count as f64) / (0.3 * sr)).exp();
            for ch in 0..2 {
                let ms = self.sumsq[ch] / self.count as f64;
                let prev = self.rms[ch] as f64;
                self.rms[ch] = (prev * prev * a + ms * (1.0 - a)).sqrt() as f32;
            }
        }
        self.peak = [0.0; 2];
        self.sumsq = [0.0; 2];
        self.count = 0;
        (peak, self.rms)
    }
}

pub(crate) struct TrackNode {
    pub id: TrackId,
    pub kind: TrackKind,
    pub arm: bool,
    pub monitor: Monitor,
    pub has_input: bool,
    pub devices: Vec<DeviceNode>,
    pub param_autos: Vec<ParamAuto>,
    pub buf: AudioBuffer,
    pub chunk_buf: AudioBuffer,
    pub chunk_events: EventList,
    pub clip_buf: AudioBuffer,
    pub clip_delay: DelayLine,
    pub clips: Vec<CompiledClip>,
    pub arrangement: Vec<ArrPlacement>,
    /// Graph scene index → clip index.
    pub slots: Vec<Option<usize>>,
    pub session_index: Option<usize>,
    pub session: SessionRt,
    pub events: EventList,
    pub live: EventList,
    pub active: ActiveNotes,
    pub rng: Rng,
    pub release_all: bool,
    // Mixer.
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub silenced: bool,
    pub vca: Option<usize>,
    pub volume_lane: Option<AutomationLane>,
    pub pan_lane: Option<AutomationLane>,
    pub mute_lane: Option<AutomationLane>,
    /// For VCA tracks: `(dB at segment start, dB at end, muted)`.
    pub vca_out: (f32, f32, bool),
    pub vca_in: (f32, f32, bool),
    pub cur_gain: [f32; 2],
    pub sends: Vec<SendNode>,
    pub output: OutputTarget,
    pub out_delay: DelayLine,
    /// Signal at the fader lags musical time by this many samples.
    pub fader_latency: u32,
    /// Signal entering the chain lags musical time by this many samples.
    pub chain_latency_in: u32,
    pub meter: Option<usize>,
    pub meter_acc: MeterAcc,
    /// Offline stem capture (post-fader, aligned with the output bus).
    pub tap: Option<Box<TapNode>>,
}

/// Captures a track's post-fader signal for stem export, delayed so it lines
/// up with the output bus (`total latency − track output latency`).
pub(crate) struct TapNode {
    pub delay: DelayLine,
    pub scratch: AudioBuffer,
    /// The current block, written at each segment's output offset.
    pub block: AudioBuffer,
}

impl TrackNode {
    fn carries_audio(&self) -> bool {
        self.kind.carries_audio()
    }

    /// Fader gains at a tick.
    fn fader_target(&self, tick: f64, vca_db: f32, vca_mute: bool) -> (f32, f32) {
        let vol = self.volume_lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(self.volume_db, volume_db_from_normalized);
        let pan = self.pan_lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(self.pan, pan_from_normalized);
        let lane_mute = self.mute_lane.as_ref().and_then(|l| lane_value(l, tick)).is_some_and(|v| v >= 0.5);
        if self.mute || lane_mute || vca_mute || self.silenced {
            return (0.0, 0.0);
        }
        let g = db_to_gain(vol + vca_db);
        let (l, r) = pan_gains(pan);
        (l * g, r * g)
    }

    fn is_muted_at(&self, tick: f64, vca_mute: bool) -> bool {
        self.mute
            || self.silenced
            || vca_mute
            || self.mute_lane.as_ref().and_then(|l| lane_value(l, tick)).is_some_and(|v| v >= 0.5)
    }
}

/// Per-segment I/O for [`CompiledGraph::process_segment`].
pub(crate) struct SegmentIo<'a> {
    pub seg_start: i64,
    pub frames: usize,
    pub playing: bool,
    pub recording: bool,
    pub first: bool,
    /// Live input (planar, stereo) for monitoring, with the segment offset.
    pub input: Option<(&'a AudioBuffer, usize)>,
    pub out: &'a mut AudioBuffer,
    pub out_offset: usize,
    pub loop_range: Option<(Tick, Tick)>,
}

pub struct CompiledGraph {
    pub(crate) sample_rate: f64,
    pub(crate) max_frames: usize,
    pub(crate) tempo: TempoMap,
    pub(crate) tracks: Vec<TrackNode>,
    pub(crate) inputs: Vec<AudioBuffer>,
    pub(crate) out_bus: AudioBuffer,
    pub(crate) out_acc: MeterAcc,
    /// Sorted `(id, index)` for lookups on the audio thread.
    pub(crate) track_lookup: Vec<(TrackId, usize)>,
    pub(crate) scenes: Vec<SceneId>,
    pub(crate) scene_lookup: Vec<(SceneId, usize)>,
    pub(crate) total_latency: u32,
    pub(crate) meters: Arc<MeterBank>,
    pub(crate) session_status: Arc<SessionStatus>,
    pub(crate) warnings: Vec<String>,
    /// Chain latency per track (what `Track::reported_latency` should show).
    pub(crate) track_latency: Vec<(TrackId, u32)>,
}

impl CompiledGraph {
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    pub fn total_latency(&self) -> u32 {
        self.total_latency
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub(crate) fn track_index(&self, id: TrackId) -> Option<usize> {
        self.track_lookup.binary_search_by_key(&id, |e| e.0).ok().map(|i| self.track_lookup[i].1)
    }

    pub(crate) fn scene_index(&self, id: SceneId) -> Option<usize> {
        self.scene_lookup.binary_search_by_key(&id, |e| e.0).ok().map(|i| self.scene_lookup[i].1)
    }

    /// Apply parameter changes collected at compile time (audio thread,
    /// right after installation).
    pub(crate) fn apply_init_params(&mut self) {
        for t in &self.tracks {
            for_each_proc(&t.devices, &mut |p| {
                if !p.init_params.is_empty() {
                    // SAFETY: audio thread owns the installed graph.
                    let proc = unsafe { p.proc.get_mut() };
                    for &(id, v) in &p.init_params {
                        proc.set_param(id, v);
                    }
                }
            });
        }
    }

    /// Take over runtime state from the graph being replaced. Real-time safe.
    pub(crate) fn adopt(&mut self, old: &mut CompiledGraph) {
        for ti in 0..self.tracks.len() {
            let id = self.tracks[ti].id;
            let Some(oi) = old.track_index(id) else { continue };
            let o = &mut old.tracks[oi];
            let n = &mut self.tracks[ti];
            std::mem::swap(&mut n.active, &mut o.active);
            n.rng = o.rng;
            n.release_all = o.release_all;
            n.cur_gain = o.cur_gain;
            n.meter_acc.rms = o.meter_acc.rms;
            n.out_delay.adopt(&mut o.out_delay);
            n.clip_delay.adopt(&mut o.clip_delay);
            for (ns, os) in n.sends.iter_mut().zip(o.sends.iter_mut()) {
                if ns.target_id_matches(os) {
                    ns.cur = os.cur;
                    ns.delay.adopt(&mut os.delay);
                }
            }
            adopt_devices(&mut n.devices, &mut o.devices);
            let map_scene = |s: usize| old.scenes.get(s).and_then(|sid| {
                self.scene_lookup.binary_search_by_key(sid, |e| e.0).ok().map(|i| self.scene_lookup[i].1)
            });
            let os = old.tracks[oi].session;
            let n = &mut self.tracks[ti];
            n.session.override_arrangement = os.override_arrangement;
            n.session.playing = os.playing.and_then(|(s, start)| map_scene(s).map(|ns| (ns, start)));
            n.session.queued = os.queued.and_then(|q| match q.scene {
                None => Some(q),
                Some(s) => map_scene(s).map(|ns| Queued { scene: Some(ns), at: q.at }),
            });
            // A launched slot that lost its clip stops.
            if let Some((s, _)) = n.session.playing
                && n.slots.get(s).copied().flatten().is_none() {
                    n.session.playing = None;
                    n.release_all = true;
                }
        }
        self.out_acc.rms = old.out_acc.rms;
    }

    /// Flag every track to release its clip notes at the next segment.
    pub(crate) fn release_all_notes(&mut self) {
        for t in &mut self.tracks {
            t.release_all = true;
        }
    }

    pub(crate) fn stop_all_session(&mut self) {
        for t in &mut self.tracks {
            if t.session.playing.is_some() {
                t.release_all = true;
            }
            t.session.playing = None;
            t.session.queued = None;
        }
    }

    pub(crate) fn back_to_arrangement(&mut self) {
        self.stop_all_session();
        for t in &mut self.tracks {
            t.session.override_arrangement = false;
        }
    }

    fn quantize_up(&self, now: f64, q: Quantize) -> Tick {
        let now_t = now.ceil() as Tick;
        if q == Quantize::None {
            return now_t;
        }
        let sig_start = self.tempo.signatures.iter().rev().find(|(s, _)| *s as f64 <= now).map_or(0, |(s, _)| *s);
        let step = q.ticks(self.tempo.signature_at(now_t).bar_ticks()).max(1);
        let rel = now_t - sig_start;
        sig_start + (rel + step - 1).div_euclid(step) * step
    }

    /// Queue a clip launch (or a stop when the slot is empty).
    pub(crate) fn launch(&mut self, track: usize, scene: usize, now: f64, immediate: bool, global_q: Quantize) {
        let Some(slot) = self.tracks.get(track).and_then(|t| t.slots.get(scene)).copied() else { return };
        let q = slot.map_or(global_q, |c| self.tracks[track].clips[c].quantize);
        let at = if immediate { now.floor() as Tick } else { self.quantize_up(now, q) };
        let t = &mut self.tracks[track];
        match slot {
            Some(c) => {
                let toggle = t.clips[c].mode == LaunchMode::Toggle && t.session.playing.is_some_and(|(s, _)| s == scene);
                t.session.queued = Some(Queued { scene: if toggle { None } else { Some(scene) }, at });
            }
            None => {
                if t.session.playing.is_some() {
                    t.session.queued = Some(Queued { scene: None, at });
                }
            }
        }
        t.session.override_arrangement = true;
    }

    pub(crate) fn launch_scene(&mut self, scene: usize, now: f64, immediate: bool, global_q: Quantize) {
        for ti in 0..self.tracks.len() {
            if self.tracks[ti].kind.has_clips() {
                self.launch(ti, scene, now, immediate, global_q);
            }
        }
    }

    pub(crate) fn stop_track(&mut self, track: usize, now: f64, global_q: Quantize) {
        let at = self.quantize_up(now, global_q);
        if let Some(t) = self.tracks.get_mut(track) {
            if t.session.playing.is_some() {
                t.session.queued = Some(Queued { scene: None, at });
            }
            t.session.override_arrangement = true;
        }
    }

    pub(crate) fn push_live(&mut self, track: usize, kind: EventKind) {
        if let Some(t) = self.tracks.get_mut(track) {
            t.live.push(0, kind);
        }
    }

    /// Publish meters and session state (audio thread, once per block).
    pub(crate) fn publish(&mut self, ticks_now: f64) {
        let sr = self.sample_rate;
        for t in &mut self.tracks {
            if let Some(m) = t.meter {
                let (peak, rms) = t.meter_acc.finish(sr);
                self.meters.slots[m].publish(peak, rms);
            }
            if let Some(si) = t.session_index {
                let st = &self.session_status.state[si];
                let playing = t.session.playing;
                st.playing.store(playing.map_or(NO_SCENE, |(s, _)| s as i32), Ordering::Relaxed);
                st.queued.store(
                    t.session.queued.map_or(NO_SCENE, |q| q.scene.map_or(STOP_SCENE, |s| s as i32)),
                    Ordering::Relaxed,
                );
                let progress = playing
                    .and_then(|(s, start)| t.slots.get(s).copied().flatten().map(|c| (c, start)))
                    .map_or(0.0, |(c, start)| {
                        let cyc = t.clips[c].session_cycle() as f64;
                        ((ticks_now - start as f64).max(0.0) % cyc / cyc) as f32
                    });
                st.progress.store(progress.to_bits(), Ordering::Relaxed);
            }
        }
        let (peak, rms) = self.out_acc.finish(sr);
        self.meters.master.publish(peak, rms);
    }

    /// Process one segment (no transport discontinuity inside it).
    pub(crate) fn process_segment(&mut self, io: &mut SegmentIo<'_>) {
        let n = io.frames;
        let sr = self.sample_rate;
        let Self { tracks, inputs, out_bus, tempo, scenes, out_acc, .. } = self;
        let ctx = TimeCtx { tempo, sr, seg_start: io.seg_start };
        let t0 = ctx.tick_of(io.seg_start);
        let t1 = ctx.tick_of(io.seg_start + n as i64);
        let sig = tempo.signature_at(t0.floor() as Tick);
        let transport = Transport {
            playing: io.playing,
            recording: io.recording,
            bpm: tempo.bpm_at(t0.floor() as Tick),
            time_sig: (sig.numerator, sig.denominator),
            position_beats: t0 / PPQ as f64,
            position_samples: io.seg_start,
            loop_range_beats: io.loop_range.map(|(a, b)| (a as f64 / PPQ as f64, b as f64 / PPQ as f64)),
        };

        for (i, t) in tracks.iter().enumerate() {
            if t.has_input {
                inputs[i].set_frames(n);
                inputs[i].clear();
            }
        }
        out_bus.set_frames(n);
        out_bus.clear();

        // VCA pre-pass: VCAs evaluate their fader, members read it.
        for t in tracks.iter_mut().filter(|t| t.kind == TrackKind::Vca) {
            let at = |tick: f64| {
                t.volume_lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(t.volume_db, volume_db_from_normalized)
            };
            t.vca_out = (at(t0), at(t1), t.is_muted_at(t0, false));
        }
        for i in 0..tracks.len() {
            if let Some(v) = tracks[i].vca {
                let val = tracks[v].vca_out;
                tracks[i].vca_in = val;
            }
        }

        for (i, t) in tracks.iter_mut().enumerate() {
            if !t.carries_audio() {
                continue;
            }
            process_track(t, i, &ctx, &*io, &transport, inputs, out_bus, scenes.len());
        }

        out_acc.add(out_bus);
        let out = &mut *io.out;
        for ch in 0..out.channels().min(out_bus.channels()) {
            let dst = &mut out.channel_mut(ch)[io.out_offset..io.out_offset + n];
            dst.copy_from_slice(out_bus.channel(ch));
        }
    }
}

impl SendNode {
    fn target_id_matches(&self, other: &SendNode) -> bool {
        self.pre_fader == other.pre_fader && self.delay.delay() == other.delay.delay()
    }
}

#[allow(clippy::too_many_arguments)]
fn process_track(
    t: &mut TrackNode,
    index: usize,
    ctx: &TimeCtx<'_>,
    io: &SegmentIo<'_>,
    transport: &Transport,
    inputs: &mut [AudioBuffer],
    out_bus: &mut AudioBuffer,
    scene_count: usize,
) {
    let n = io.frames;
    let sr = ctx.sr;
    let lo = io.seg_start;
    let hi = lo + n as i64;

    t.buf.set_frames(n);
    if t.has_input {
        t.buf.copy_from(&inputs[index]);
    } else {
        t.buf.clear();
    }

    // Events: pending releases, live input, clip playback.
    t.events.clear();
    if t.release_all {
        t.active.release_all(0, &mut t.events);
        t.release_all = false;
    }
    if io.first {
        for e in t.live.events.iter() {
            t.events.push(e.offset, e.kind);
        }
        t.live.clear();
    }
    if t.kind.has_clips() {
        let use_clip_buf = t.clip_delay.is_active();
        if use_clip_buf {
            t.clip_buf.set_frames(n);
            t.clip_buf.clear();
        }
        if io.playing {
            play_clips(t, ctx, lo, hi, scene_count, use_clip_buf);
        }
        t.active.release_due(ctx, hi, &mut t.events);
        if use_clip_buf {
            t.clip_delay.process(&mut t.clip_buf);
            t.buf.add_from(&t.clip_buf, 1.0);
        }
        // Input monitoring for armed audio tracks.
        if t.kind == TrackKind::Audio && t.arm && t.monitor != Monitor::Off
            && let Some((input, off)) = io.input {
                for ch in 0..2.min(input.channels()) {
                    let src = &input.channel(ch.min(input.channels() - 1))[off..off + n];
                    for (d, s) in t.buf.channel_mut(ch).iter_mut().zip(src) {
                        *d += *s;
                    }
                }
            }
    }
    t.events.sort();

    // Device chain.
    if t.param_autos.is_empty() {
        process_devices(&mut t.devices, &mut t.buf, &t.events.events, transport, sr);
    } else {
        let mut a = 0;
        while a < n {
            let b = (a + CONTROL_BLOCK).min(n);
            let tick = ctx.tick_of(lo + a as i64 - t.chain_latency_in as i64);
            for pa in &mut t.param_autos {
                pa.apply(tick);
            }
            t.chunk_buf.set_frames(b - a);
            for ch in 0..t.buf.channels().min(t.chunk_buf.channels()) {
                t.chunk_buf.channel_mut(ch).copy_from_slice(&t.buf.channel(ch)[a..b]);
            }
            t.chunk_events.clear();
            for e in t.events.events.iter().filter(|e| (e.offset as usize) >= a && (e.offset as usize) < b) {
                t.chunk_events.push(e.offset - a as u32, e.kind);
            }
            let mut tr = *transport;
            tr.position_samples = lo + a as i64;
            tr.position_beats = ctx.tick_of(lo + a as i64) / PPQ as f64;
            process_devices(&mut t.devices, &mut t.chunk_buf, &t.chunk_events.events, &tr, sr);
            for ch in 0..t.buf.channels().min(t.chunk_buf.channels()) {
                t.buf.channel_mut(ch)[a..b].copy_from_slice(t.chunk_buf.channel(ch));
            }
            a = b;
        }
    }

    // Pre-fader sends, fader, post-fader sends, all at control rate.
    let fader_shift = t.fader_latency as i64;
    let (vca0, vca1, vca_mute) = t.vca_in;
    let has_vca = t.vca.is_some();
    for pass in [true, false] {
        for s in t.sends.iter_mut().filter(|s| s.pre_fader == pass) {
            s.buf.set_frames(n);
            let mut a = 0;
            while a < n {
                let b = (a + CONTROL_BLOCK).min(n);
                let tick = ctx.tick_of(lo + b as i64 - fader_shift);
                let muted = t.mute
                    || t.silenced
                    || (has_vca && vca_mute)
                    || t.mute_lane.as_ref().and_then(|l| lane_value(l, tick)).is_some_and(|v| v >= 0.5);
                let db = s.lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(s.level_db, volume_db_from_normalized);
                let target = if muted { 0.0 } else { db_to_gain(db) };
                let step = (target - s.cur) / (b - a) as f32;
                for ch in 0..2 {
                    let src = &t.buf.channel(ch)[a..b];
                    let dst = &mut s.buf.channel_mut(ch)[a..b];
                    let mut g = s.cur;
                    for (d, x) in dst.iter_mut().zip(src) {
                        g += step;
                        *d = x * g;
                    }
                }
                s.cur = target;
                a = b;
            }
            s.delay.process(&mut s.buf);
            inputs[s.target].add_from(&s.buf, 1.0);
        }
        if pass {
            // The fader itself.
            let mut a = 0;
            while a < n {
                let b = (a + CONTROL_BLOCK).min(n);
                let tick = ctx.tick_of(lo + b as i64 - fader_shift);
                let vca_db = if has_vca { vca0 + (vca1 - vca0) * (b as f32 / n as f32) } else { 0.0 };
                let (tl, tr) = t.fader_target(tick, vca_db, has_vca && vca_mute);
                let len = (b - a) as f32;
                let (sl, sr_) = ((tl - t.cur_gain[0]) / len, (tr - t.cur_gain[1]) / len);
                let (bl, br) = t.buf.stereo_mut();
                let (mut gl, mut gr) = (t.cur_gain[0], t.cur_gain[1]);
                for k in a..b {
                    gl += sl;
                    gr += sr_;
                    bl[k] *= gl;
                    br[k] *= gr;
                }
                t.cur_gain = [tl, tr];
                a = b;
            }
        }
    }

    t.meter_acc.add(&t.buf);
    if let Some(tap) = t.tap.as_deref_mut() {
        tap.scratch.set_frames(n);
        tap.scratch.copy_from(&t.buf);
        tap.delay.process(&mut tap.scratch);
        for ch in 0..2 {
            tap.block.channel_mut(ch)[io.out_offset..io.out_offset + n].copy_from_slice(tap.scratch.channel(ch));
        }
    }
    t.out_delay.process(&mut t.buf);
    match t.output {
        OutputTarget::Track(j) => inputs[j].add_from(&t.buf, 1.0),
        OutputTarget::Out => out_bus.add_from(&t.buf, 1.0),
        OutputTarget::None => {}
    }
}

/// Session and arrangement playback for one segment.
fn play_clips(t: &mut TrackNode, ctx: &TimeCtx<'_>, lo: i64, hi: i64, scene_count: usize, use_clip_buf: bool) {
    let mut cur = lo;
    // Session View: play the launched clip, applying queued launches/stops
    // (and follow actions / clip ends) at their exact sample.
    for _guard in 0..8 {
        if let Some((scene, start)) = t.session.playing
            && let Some(c) = t.slots.get(scene).copied().flatten() {
                let clip = &t.clips[c];
                let bar = ctx.tempo.signature_at(start).bar_ticks();
                let mut auto: Option<Queued> = None;
                if let Some(f) = clip.follow {
                    let (bars, target) = follow_target(f, scene, &t.slots, scene_count, &mut t.rng);
                    auto = Some(Queued { scene: target, at: start + bars as Tick * bar });
                }
                if clip.looping.is_none() {
                    let end = Queued { scene: None, at: start + clip.length };
                    if auto.is_none_or(|a| a.at > end.at) {
                        auto = Some(end);
                    }
                }
                if let Some(a) = auto
                    && t.session.queued.is_none_or(|q| q.at > a.at) {
                        t.session.queued = Some(a);
                    }
            }
        let next = t.session.queued.filter(|q| ctx.sample_of(q.at) < hi);
        let seg_hi = next.map_or(hi, |q| ctx.sample_of(q.at).clamp(cur, hi));
        if let Some((scene, start)) = t.session.playing
            && let Some(c) = t.slots.get(scene).copied().flatten() {
                let clip = &t.clips[c];
                let limit = if clip.looping.is_some() { None } else { Some(clip.length) };
                let buf = if use_clip_buf { &mut t.clip_buf } else { &mut t.buf };
                clip.schedule_midi(start, limit, cur, seg_hi, ctx, &mut t.events, &mut t.active, &mut t.rng);
                clip.render_audio(start, limit, cur, seg_hi, ctx, buf);
            }
        let Some(q) = next else { break };
        t.active.release_all((seg_hi - ctx.seg_start) as u32, &mut t.events);
        t.session.playing = q.scene.filter(|s| t.slots.get(*s).copied().flatten().is_some()).map(|s| (s, q.at));
        t.session.queued = None;
        cur = seg_hi;
    }

    if t.session.override_arrangement {
        return;
    }
    for p in &t.arrangement {
        let clip = &t.clips[p.clip];
        let s0 = ctx.sample_of(p.start);
        let s1 = ctx.sample_of(p.start + clip.length);
        let a = s0.max(lo);
        let b = s1.min(hi);
        if a >= b {
            continue;
        }
        let buf = if use_clip_buf { &mut t.clip_buf } else { &mut t.buf };
        clip.schedule_midi(p.start, Some(clip.length), a, b, ctx, &mut t.events, &mut t.active, &mut t.rng);
        clip.render_audio(p.start, Some(clip.length), a, b, ctx, buf);
    }
}

fn follow_target(f: FollowAction, scene: usize, slots: &[Option<usize>], scene_count: usize, rng: &mut Rng) -> (u32, Option<usize>) {
    let filled = |s: &usize| slots.get(*s).copied().flatten().is_some();
    let n = scene_count.min(slots.len()).max(1);
    match f {
        FollowAction::Next { after_bars } => (after_bars.max(1), (1..=n).map(|k| (scene + k) % n).find(filled)),
        FollowAction::Previous { after_bars } => (after_bars.max(1), (1..=n).map(|k| (scene + n - k) % n).find(filled)),
        FollowAction::First { after_bars } => (after_bars.max(1), (0..n).find(filled)),
        FollowAction::Random { after_bars } => {
            let start = (rng.next_f32() * n as f32) as usize;
            (after_bars.max(1), (0..n).map(|k| (start + k) % n).find(filled))
        }
        FollowAction::Stop { after_bars } => (after_bars.max(1), None),
    }
}
