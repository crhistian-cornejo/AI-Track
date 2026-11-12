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
        }
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
