//! Lock-free state the audio thread publishes for the UI: meters, clip
//! launch states, transport position and CPU load.
//!
//! The audio thread only performs atomic stores into structures allocated by
//! the control thread. The `Mutex`es here guard which `Arc` is current and
//! are only ever locked by the UI and control threads, never by the audio
//! thread (which holds its own clone of the `Arc` inside the graph).

use crate::tap::{AnalysisTap, DeviceMeter, TAP_FRAMES};
use aitrack_model::{DeviceId, SceneId, TrackId};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[inline]
fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

#[inline]
fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// A meter reading in linear gain.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterReading {
    /// Highest absolute sample since the previous read.
    pub peak: [f32; 2],
    /// RMS over roughly the last 300 ms.
    pub rms: [f32; 2],
}

#[derive(Default)]
pub(crate) struct MeterSlot {
    peak: [AtomicU32; 2],
    rms: [AtomicU32; 2],
}

impl MeterSlot {
    /// Audio thread: fold a block's peak in and publish the RMS.
    #[inline]
    pub fn publish(&self, peak: [f32; 2], rms: [f32; 2]) {
        for ch in 0..2 {
            let old = load_f32(&self.peak[ch]);
            if peak[ch] > old {
                store_f32(&self.peak[ch], peak[ch]);
            }
            store_f32(&self.rms[ch], rms[ch]);
        }
    }

    /// UI thread: read and reset the peak hold.
    pub fn take(&self) -> MeterReading {
        let mut r = MeterReading::default();
        for ch in 0..2 {
            r.peak[ch] = f32::from_bits(self.peak[ch].swap(0, Ordering::Relaxed));
            r.rms[ch] = load_f32(&self.rms[ch]);
        }
        r
    }
}

/// Per-track meter slots of one graph, keyed by sorted track id.
pub struct MeterBank {
    pub(crate) ids: Vec<TrackId>,
    pub(crate) slots: Vec<MeterSlot>,
    pub(crate) master: MeterSlot,
}

impl MeterBank {
    pub(crate) fn new(mut ids: Vec<TrackId>) -> Self {
        ids.sort();
        ids.dedup();
        let slots = ids.iter().map(|_| MeterSlot::default()).collect();
        Self { ids, slots, master: MeterSlot::default() }
    }

    pub(crate) fn slot(&self, id: TrackId) -> Option<&MeterSlot> {
        self.ids.binary_search(&id).ok().map(|i| &self.slots[i])
    }

    pub(crate) fn index(&self, id: TrackId) -> Option<usize> {
        self.ids.binary_search(&id).ok()
    }
}

/// Launch state of one Session View slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Stopped,
    /// Will start at the next launch-quantize boundary.
    Queued,
    Playing,
    /// Playing, and will stop at the next boundary.
    StopQueued,
}

/// One Session View slot as reported to the UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipSlotStatus {
    pub track: TrackId,
    pub scene: SceneId,
    pub state: SlotState,
    /// Position inside the clip's loop, 0..1 (0 unless playing).
    pub progress: f32,
}

pub(crate) const NO_SCENE: i32 = -1;
pub(crate) const STOP_SCENE: i32 = -2;

#[derive(Default)]
pub(crate) struct TrackSession {
    pub playing: AtomicI32,
    pub queued: AtomicI32,
    pub progress: AtomicU32,
}

/// Session status of one graph: which scene index plays / is queued per track.
pub(crate) struct SessionStatus {
    pub scenes: Vec<SceneId>,
    /// `(track, scene indices that hold a clip)`, in graph track order.
    pub tracks: Vec<(TrackId, Vec<usize>)>,
    pub state: Vec<TrackSession>,
}

impl SessionStatus {
    pub fn new(scenes: Vec<SceneId>, tracks: Vec<(TrackId, Vec<usize>)>) -> Self {
        let state = tracks
            .iter()
            .map(|_| TrackSession { playing: AtomicI32::new(NO_SCENE), queued: AtomicI32::new(NO_SCENE), progress: AtomicU32::new(0) })
            .collect();
        Self { scenes, tracks, state }
    }

    pub fn snapshot(&self) -> Vec<ClipSlotStatus> {
        let mut out = Vec::new();
        for ((track, slots), st) in self.tracks.iter().zip(&self.state) {
            let playing = st.playing.load(Ordering::Relaxed);
            let queued = st.queued.load(Ordering::Relaxed);
            let progress = load_f32(&st.progress);
            for &s in slots {
                let si = s as i32;
                let state = match (playing == si, queued) {
                    (true, q) if q == STOP_SCENE || (q >= 0 && q != si) => SlotState::StopQueued,
                    (true, _) => SlotState::Playing,
                    (false, q) if q == si => SlotState::Queued,
                    _ => SlotState::Stopped,
                };
                out.push(ClipSlotStatus {
                    track: *track,
                    scene: self.scenes[s],
                    state,
                    progress: if playing == si { progress } else { 0.0 },
                });
            }
        }
        out
    }
}

/// Global engine status shared by the audio, control and UI threads.
pub(crate) struct SharedStatus {
    pub playing: AtomicBool,
    pub recording: AtomicBool,
    pub metronome: AtomicBool,
    /// Audible position in ticks (`f64` bits), PDC-compensated.
    pub position_ticks: AtomicU64,
    /// Engine position in samples (not compensated).
    pub position_samples: AtomicI64,
    /// Smoothed callback time / buffer time.
    pub cpu_load: AtomicU32,
    pub cpu_peak: AtomicU32,
    pub xruns: AtomicU64,
    /// Loop range the transport is using (`-1` start = off).
    pub loop_start: AtomicI64,
    pub loop_end: AtomicI64,
    /// Total latency of the current graph in samples.
    pub latency: AtomicU32,
    pub sample_rate: AtomicU32,
    pub meters: Mutex<Arc<MeterBank>>,
    pub session: Mutex<Arc<SessionStatus>>,
    pub track_latency: Mutex<Vec<(TrackId, u32)>>,
    /// Stereo ring the UI's analysers drain (written only while enabled).
    pub tap: Arc<AnalysisTap>,
    /// `meter()` slots of the current graph's devices, sorted by id.
    pub device_meters: Mutex<Vec<(DeviceId, Arc<DeviceMeter>)>>,
    /// Device side of the latency, measured by the callbacks.
    pub io: IoLatency,
    /// Where the playhead is heard (see [`PlayheadAnchor`]).
    pub anchor: AnchorSlot,
    /// Time base of every `*_nanos` value here.
    pub epoch: Instant,
}

/// Latency the device callbacks measure from cpal's stream timestamps, in
/// frames at the stream rate. Written by the audio threads (atomic stores
/// only), read by the UI.
#[derive(Default)]
pub struct IoLatency {
    /// Frames of the last output callback.
    pub buffer_frames: AtomicU32,
    /// Output callback instant → predicted DAC instant: the buffer plus
    /// whatever the driver and device add (CoreAudio: device latency and
    /// safety offset; Bluetooth: the radio link).
    pub output: AtomicU32,
    /// ADC capture → input callback instant (0 without an input stream).
    pub input: AtomicU32,
    /// Frames of the last input callback.
    pub input_buffer_frames: AtomicU32,
}

impl IoLatency {
    /// Audio thread: one output callback of `frames` frames whose data will
    /// reach the speakers `delay` frames after the callback started.
    #[inline]
    pub fn publish_output(&self, frames: u32, delay: u32) {
        self.buffer_frames.store(frames, Ordering::Relaxed);
        self.output.store(delay.max(frames), Ordering::Relaxed);
    }

    #[inline]
    pub fn publish_input(&self, frames: u32, delay: u32) {
        self.input_buffer_frames.store(frames, Ordering::Relaxed);
        self.input.store(delay, Ordering::Relaxed);
    }

    /// Forget the old device's numbers (stream rebuilt).
    pub fn reset(&self) {
        for a in [&self.buffer_frames, &self.output, &self.input, &self.input_buffer_frames] {
            a.store(0, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self, sample_rate: u32, plugin_frames: u32) -> LatencyReport {
        LatencyReport {
            sample_rate,
            buffer_frames: self.buffer_frames.load(Ordering::Relaxed),
            output_frames: self.output.load(Ordering::Relaxed),
            input_frames: self.input.load(Ordering::Relaxed),
            input_buffer_frames: self.input_buffer_frames.load(Ordering::Relaxed),
            plugin_frames,
        }
    }
}

/// Every latency figure of the running engine, in frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencyReport {
    pub sample_rate: u32,
    /// Frames per output callback.
    pub buffer_frames: u32,
    /// Callback → speakers (buffer + device), 0 before the first callback.
    pub output_frames: u32,
    /// Microphone → input callback (0 without input).
    pub input_frames: u32,
    pub input_buffer_frames: u32,
    /// Plugin delay compensation of the current graph.
    pub plugin_frames: u32,
}

impl LatencyReport {
    fn ms(&self, frames: u32) -> f32 {
        if self.sample_rate == 0 { 0. } else { frames as f32 * 1000. / self.sample_rate as f32 }
    }

    /// The part of the output latency the driver/device adds on top of the buffer.
    pub fn device_output_frames(&self) -> u32 {
        self.output_frames.saturating_sub(self.buffer_frames)
    }

    pub fn buffer_ms(&self) -> f32 {
        self.ms(self.buffer_frames)
    }

    pub fn output_ms(&self) -> f32 {
        self.ms(self.output_frames)
    }

    pub fn device_output_ms(&self) -> f32 {
        self.ms(self.device_output_frames())
    }

    /// Microphone → callback, plus one output buffer the input waits in the
    /// ring before the engine reads it.
    pub fn input_ms(&self) -> f32 {
        if self.input_frames == 0 && self.input_buffer_frames == 0 { 0. } else { self.ms(self.input_frames.max(self.input_buffer_frames)) }
    }

    pub fn plugin_ms(&self) -> f32 {
        self.ms(self.plugin_frames)
    }

    /// What a signal monitored through the engine takes, microphone to
    /// speakers: input + output (+ plugin delay compensation).
    pub fn round_trip_ms(&self) -> f32 {
        self.input_ms() + self.output_ms() + self.plugin_ms()
    }

    /// Delay between the engine's audible position and the speakers.
    pub fn output_seconds(&self) -> f64 {
        if self.sample_rate == 0 { 0. } else { self.output_frames as f64 / self.sample_rate as f64 }
    }
}

/// The audio thread's last word on where playback is: the audible tick at
/// the end of the last rendered block and the instant that block was due,
/// plus the boundaries of the current continuous run so the UI can
/// interpolate between callbacks and step back by the output latency
/// without inventing positions before a play start or across a loop wrap.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayheadAnchor {
    pub playing: bool,
    /// Audible (PDC-compensated) tick of the first sample after the block.
    pub ticks: f64,
    /// Tempo at `ticks`.
    pub ticks_per_second: f64,
    /// When the engine produced that sample, on the [`SharedStatus::epoch`]
    /// clock (callback start + its offset in the callback).
    pub at_nanos: u64,
    /// Audible tick where the current run started (play, seek, loop start).
    pub run_start: f64,
    /// Where the previous run ended when the current one began with a loop
    /// wrap (so positions before `run_start` belong to the loop's end).
    pub prev_run_end: Option<f64>,
}

impl PlayheadAnchor {
    /// The tick reaching the speakers at `now_nanos`, given `output_delay`
    /// seconds between the engine and the speakers. Never extrapolates more
    /// than `MAX_AHEAD` past the anchor (a stalled callback freezes the
    /// playhead instead of letting it run away).
    pub fn heard_at(&self, now_nanos: u64, output_delay: f64) -> f64 {
        const MAX_AHEAD: f64 = 0.1;
        if !self.playing {
            return self.ticks;
        }
        let dt = (now_nanos as f64 - self.at_nanos as f64) * 1e-9 - output_delay;
        let t = self.ticks + dt.min(MAX_AHEAD) * self.ticks_per_second;
        if t >= self.run_start {
            return t;
        }
        match self.prev_run_end {
            Some(end) => (end - (self.run_start - t)).max(0.),
            None => self.run_start,
        }
    }
}

/// Seqlock around a [`PlayheadAnchor`]: one wait-free writer (the audio
/// thread), readers retry while a write is in flight.
#[derive(Default)]
pub struct AnchorSlot {
    seq: AtomicU32,
    playing: AtomicBool,
    ticks: AtomicU64,
    rate: AtomicU64,
    at: AtomicU64,
    run_start: AtomicU64,
    /// `f64` bits; NaN = none.
    prev_end: AtomicU64,
}

impl AnchorSlot {
    #[inline]
    pub fn publish(&self, a: &PlayheadAnchor) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        self.playing.store(a.playing, Ordering::Relaxed);
        self.ticks.store(a.ticks.to_bits(), Ordering::Relaxed);
        self.rate.store(a.ticks_per_second.to_bits(), Ordering::Relaxed);
        self.at.store(a.at_nanos, Ordering::Relaxed);
        self.run_start.store(a.run_start.to_bits(), Ordering::Relaxed);
        self.prev_end.store(a.prev_run_end.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    pub fn read(&self) -> PlayheadAnchor {
        let mut last = PlayheadAnchor::default();
        for _ in 0..64 {
            let s0 = self.seq.load(Ordering::Acquire);
            last = PlayheadAnchor {
                playing: self.playing.load(Ordering::Relaxed),
                ticks: f64::from_bits(self.ticks.load(Ordering::Relaxed)),
                ticks_per_second: f64::from_bits(self.rate.load(Ordering::Relaxed)),
                at_nanos: self.at.load(Ordering::Relaxed),
                run_start: f64::from_bits(self.run_start.load(Ordering::Relaxed)),
                prev_run_end: Some(f64::from_bits(self.prev_end.load(Ordering::Relaxed))).filter(|v| !v.is_nan()),
            };
            std::sync::atomic::fence(Ordering::Acquire);
            if s0 & 1 == 0 && self.seq.load(Ordering::Relaxed) == s0 {
                break;
            }
            std::hint::spin_loop();
        }
        last
    }
}

impl SharedStatus {
    pub fn new() -> Self {
        Self {
            playing: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            metronome: AtomicBool::new(false),
            position_ticks: AtomicU64::new(0f64.to_bits()),
            position_samples: AtomicI64::new(0),
            cpu_load: AtomicU32::new(0),
            cpu_peak: AtomicU32::new(0),
            xruns: AtomicU64::new(0),
            loop_start: AtomicI64::new(-1),
            loop_end: AtomicI64::new(-1),
            latency: AtomicU32::new(0),
            sample_rate: AtomicU32::new(0),
            meters: Mutex::new(Arc::new(MeterBank::new(Vec::new()))),
            session: Mutex::new(Arc::new(SessionStatus::new(Vec::new(), Vec::new()))),
            track_latency: Mutex::new(Vec::new()),
            tap: Arc::new(AnalysisTap::new(TAP_FRAMES)),
            device_meters: Mutex::new(Vec::new()),
            io: IoLatency::default(),
            anchor: AnchorSlot::default(),
            epoch: Instant::now(),
        }
    }

    /// Nanoseconds since [`epoch`](Self::epoch). Real-time safe.
    #[inline]
    pub fn now_nanos(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    #[inline]
    pub fn set_cpu(&self, load: f32) {
        let old = load_f32(&self.cpu_load);
        store_f32(&self.cpu_load, old * 0.9 + load * 0.1);
        if load > load_f32(&self.cpu_peak) {
            store_f32(&self.cpu_peak, load);
        }
    }

    pub fn cpu(&self) -> (f32, f32) {
        (load_f32(&self.cpu_load), f32::from_bits(self.cpu_peak.swap(0, Ordering::Relaxed)))
    }

    #[inline]
    pub fn set_position(&self, ticks: f64, samples: i64) {
        self.position_ticks.store(ticks.to_bits(), Ordering::Relaxed);
        self.position_samples.store(samples, Ordering::Relaxed);
    }

    pub fn position_ticks(&self) -> f64 {
        f64::from_bits(self.position_ticks.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> PlayheadAnchor {
        // 960 ticks per second (120 bpm at 480 PPQ), anchor at tick 9600 due at t = 10 s.
        PlayheadAnchor { playing: true, ticks: 9600., ticks_per_second: 960., at_nanos: 10_000_000_000, run_start: 0., prev_run_end: None }
    }

    #[test]
    fn heard_position_steps_back_by_the_output_latency() {
        let a = anchor();
        // At the anchor instant with 50 ms output latency the speakers are 48 ticks behind.
        assert!((a.heard_at(10_000_000_000, 0.05) - 9552.).abs() < 1e-6);
        // 20 ms later it has moved on by 19.2 ticks.
        assert!((a.heard_at(10_020_000_000, 0.05) - 9571.2).abs() < 1e-6);
        // Zero latency is the engine's own audible position.
        assert!((a.heard_at(10_000_000_000, 0.) - 9600.).abs() < 1e-6);
    }

    #[test]
    fn a_stalled_callback_freezes_instead_of_running_away() {
        let a = anchor();
        let far = a.heard_at(20_000_000_000, 0.);
        assert!((far - (9600. + 0.1 * 960.)).abs() < 1e-6);
    }

    #[test]
    fn heard_position_never_goes_before_the_play_start() {
        let a = PlayheadAnchor { run_start: 9590., ..anchor() };
        assert_eq!(a.heard_at(10_000_000_000, 0.2), 9590.);
    }

    #[test]
    fn heard_position_wraps_back_into_the_loop_end() {
        // Loop 1920..9600 just wrapped: run started at 1920, previous run ended at 9600.
        let a = PlayheadAnchor { ticks: 1930., run_start: 1920., prev_run_end: Some(9600.), ..anchor() };
        // 100 ms of latency = 96 ticks: 10 after the wrap, so 86 before the loop end.
        assert!((a.heard_at(10_000_000_000, 0.1) - (9600. - 86.)).abs() < 1e-6);
    }

    #[test]
    fn stopped_anchor_is_the_position() {
        let a = PlayheadAnchor { playing: false, ..anchor() };
        assert_eq!(a.heard_at(99, 1.), 9600.);
    }

    #[test]
    fn anchor_slot_round_trips() {
        let slot = AnchorSlot::default();
        let a = PlayheadAnchor { prev_run_end: Some(5.), ..anchor() };
        slot.publish(&a);
        assert_eq!(slot.read(), a);
        let b = PlayheadAnchor { prev_run_end: None, ..a };
        slot.publish(&b);
        assert_eq!(slot.read(), b);
    }

    #[test]
    fn latency_report_math() {
        let io = IoLatency::default();
        // 256-frame buffer, the device adds 224 frames (CoreAudio safety offset + latency).
        io.publish_output(256, 480);
        io.publish_input(256, 300);
        let r = io.snapshot(48_000, 96);
        assert_eq!(r.device_output_frames(), 224);
        assert!((r.buffer_ms() - 5.333).abs() < 1e-2);
        assert!((r.output_ms() - 10.).abs() < 1e-4);
        assert!((r.input_ms() - 6.25).abs() < 1e-4);
        assert!((r.round_trip_ms() - (10. + 6.25 + 2.)).abs() < 1e-3);
        assert!((r.output_seconds() - 0.01).abs() < 1e-9);
        // A delay below the buffer (bad timestamps) still counts the buffer.
        io.publish_output(512, 0);
        assert_eq!(io.snapshot(48_000, 0).output_frames, 512);
        io.reset();
        assert_eq!(io.snapshot(48_000, 0).round_trip_ms(), 0.);
        assert_eq!(LatencyReport::default().output_ms(), 0.);
    }
}
