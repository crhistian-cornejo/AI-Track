//! Processor instances shared between successive compiled graphs.
//!
//! Recompiling the graph on every edit must not rebuild plugins (that would
//! lose their state and cost milliseconds). Each processor therefore lives in
//! a [`SharedProcessor`] cell behind an `Arc`, kept by the control thread's
//! [`ProcessorPool`] keyed by [`DeviceId`]. A new graph clones the `Arc` of
//! every device whose id and kind are unchanged; the old graph keeps its own
//! clone until it is retired and dropped on the graveyard thread, so the last
//! reference (and the plugin's teardown) never goes away on the audio thread.
//!
//! # Device state
//! Each pool entry remembers a fingerprint (length + hash) of the
//! `Device::state` bytes its processor was last given. When a compile sees
//! different bytes (`Command::SetDeviceState`: a preset, an imported
//! wavetable…), the processor is rebuilt here on the control thread with the
//! new state and every parameter, and the new graph gets the new `Arc`; the
//! old processor stays alive in the graph that still holds it and is freed
//! on the graveyard thread with it. Nothing touches the running processor,
//! so the audio thread never sees a lock or an allocation for this.
//!
//! Third-party plugins keep their state inside themselves and the app writes
//! it back into the project before saving (`plugin_states()` →
//! `SetDeviceState`). That echo must not rebuild the plugin (its editor would
//! close and the sound would glitch), so whoever copies a live processor's
//! state into the project first records it with [`note_applied_state`]: when
//! the pool then sees exactly those bytes it only updates its fingerprint.
//! The same call suits any device whose editor already delivered the state
//! to the live processor some other way (e.g. a synth's lock-free remote).

use crate::factory::{create_with_fallback, ProcessorFactory};
use crate::tap::DeviceMeter;
use aitrack_dsp::Processor;
use aitrack_model::{Device, DeviceId, DeviceKind};
use std::cell::UnsafeCell;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hasher;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// A processor that several graphs may reference while only one of them
/// (the one owned by the audio thread) ever touches it.
///
/// # Safety contract
/// * While a graph containing the cell is installed on the audio thread,
///   only that thread calls [`SharedProcessor::get_mut`].
/// * The control thread calls [`SharedProcessor::get_mut`] only before the
///   cell is first sent to the audio thread, or after the audio stream has
///   been torn down (device change), when no graph can be running.
pub struct SharedProcessor {
    inner: UnsafeCell<Box<dyn Processor>>,
    /// Latency last reported by the processor, refreshed by the audio thread
    /// after each block so the control thread can detect PDC changes without
    /// touching the processor.
    latency: AtomicU32,
    /// Set by the control thread when a parameter or state edit happens so a
    /// sleeping processor wakes on the next audio block.
    wake: std::sync::atomic::AtomicBool,
}

// SAFETY: access is serialised by the contract above; `Box<dyn Processor>`
// is `Send`, so moving the processor between threads is sound.
unsafe impl Sync for SharedProcessor {}
unsafe impl Send for SharedProcessor {}

impl SharedProcessor {
    pub fn new(processor: Box<dyn Processor>) -> Self {
        let latency = processor.latency();
        Self {
            inner: UnsafeCell::new(processor),
            latency: AtomicU32::new(latency),
            wake: std::sync::atomic::AtomicBool::new(true),
        }
    }

    pub(crate) fn wake(&self) {
        self.wake.store(true, Ordering::Release);
    }

    #[inline]
    pub(crate) fn take_wake(&self) -> bool {
        self.wake.swap(false, Ordering::Acquire)
    }

    /// # Safety
    /// See the type-level contract: the caller must be the only thread
    /// using this processor right now.
    #[allow(clippy::mut_from_ref)]
    #[inline]
    pub unsafe fn get_mut(&self) -> &mut dyn Processor {
        // SAFETY: guaranteed by the caller.
        unsafe { &mut **self.inner.get() }
    }

    pub fn reported_latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn publish_latency(&self, latency: u32) {
        self.latency.store(latency, Ordering::Relaxed);
    }
}

/// Cheap identity of a state blob: equal bytes always give equal marks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StateMark {
    len: usize,
    hash: u64,
}

pub(crate) fn state_mark(state: Option<&[u8]>) -> Option<StateMark> {
    state.map(|s| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write(s);
        StateMark { len: s.len(), hash: h.finish() }
    })
}

/// States already present in live processors (see the module docs), noted
/// by the control side and consumed by the next compile that sees them in
/// the project. Only the control/main threads touch it.
#[derive(Default)]
pub struct StateEchoes {
    marks: Mutex<HashMap<DeviceId, StateMark>>,
}

impl StateEchoes {
    /// The ledger every [`ProcessorPool::new`] pool reads.
    pub fn global() -> Arc<StateEchoes> {
        static GLOBAL: OnceLock<Arc<StateEchoes>> = OnceLock::new();
        GLOBAL.get_or_init(|| Arc::new(StateEchoes::default())).clone()
    }

    /// `state` is what `device`'s live processor already holds: when the
    /// project's device state becomes exactly these bytes, keep the
    /// processor as it is.
    pub fn note(&self, device: DeviceId, state: &[u8]) {
        if let Some(m) = state_mark(Some(state)) {
            self.lock().insert(device, m);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<DeviceId, StateMark>> {
        self.marks.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Record that `device`'s live processor already holds `state`, before
/// writing those bytes into the project with `Command::SetDeviceState`, so
/// the echo does not rebuild it. Call from the control side only (never the
/// audio thread). See the module docs.
pub fn note_applied_state(device: DeviceId, state: &[u8]) {
    StateEchoes::global().note(device, state);
}

struct PoolEntry {
    kind_key: String,
    /// Fingerprint of the `Device::state` the processor was last given.
    state: Option<StateMark>,
    proc: Arc<SharedProcessor>,
    /// Where the audio thread publishes the processor's `meter()` values.
    meter: Arc<DeviceMeter>,
    /// Parameter values already applied to (or scheduled for) the processor.
    applied: BTreeMap<u32, f32>,
    used: bool,
}

/// Result of resolving one device for a new graph.
pub(crate) struct Resolved {
    pub proc: Arc<SharedProcessor>,
    /// Parameter changes the audio thread applies when it installs the graph
    /// (empty for brand-new processors, which get everything up front).
    pub init_params: Vec<(u32, f32)>,
    pub is_instrument: bool,
    pub meter: Arc<DeviceMeter>,
}

/// Control-thread cache of live processors keyed by device id.
pub struct ProcessorPool {
    entries: HashMap<DeviceId, PoolEntry>,
    echoes: Arc<StateEchoes>,
    /// Echoes taken from `echoes` for the compile in progress.
    pending_echoes: HashMap<DeviceId, StateMark>,
}

impl Default for ProcessorPool {
    fn default() -> Self {
        Self::with_echoes(StateEchoes::global())
    }
}

/// Identity of a device kind: a device keeps its processor only while this
/// stays the same.
pub(crate) fn kind_key(device: &Device) -> Option<String> {
    match &device.kind {
        DeviceKind::Stock { plugin_id } => Some(format!("stock:{plugin_id}")),
        DeviceKind::Plugin { format, uid, .. } => Some(format!("plugin:{format:?}:{uid}")),
        DeviceKind::Rack(_) => None,
    }
}

impl ProcessorPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// A pool reading its state echoes from `echoes` instead of the global
    /// ledger (tests, or an engine that hands its own ledger to the app).
    pub fn with_echoes(echoes: Arc<StateEchoes>) -> Self {
        Self { entries: HashMap::new(), echoes, pending_echoes: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn begin(&mut self) {
        for e in self.entries.values_mut() {
            e.used = false;
        }
        self.pending_echoes = std::mem::take(&mut *self.echoes.lock());
    }

    /// Drop pool references to devices the last compile did not use. Any
    /// graph still holding them keeps them alive until it is retired.
    pub(crate) fn end(&mut self) {
        self.entries.retain(|_, e| e.used);
        // Echoes this compile did not see yet (the edit carrying them may
        // still be on its way) go back, unless a newer note replaced them.
        let left = std::mem::take(&mut self.pending_echoes);
        let mut marks = self.echoes.lock();
        for (device, mark) in left {
            if self.entries.contains_key(&device) {
                marks.entry(device).or_insert(mark);
            }
        }
    }

    /// Reuse or create the processor for `device`, with `params` being the
    /// full set of normalised values it should have (model params with
    /// macro mappings applied).
    pub(crate) fn resolve(
        &mut self,
        device: &Device,
        params: &BTreeMap<u32, f32>,
        factory: &dyn ProcessorFactory,
        sample_rate: f64,
        max_frames: usize,
    ) -> Option<Resolved> {
        let key = kind_key(device)?;
        let state = state_mark(device.state.as_deref());
        let echo = self.pending_echoes.get(&device.id).copied();
        let mut meter = None;
        if let Some(entry) = self.entries.get_mut(&device.id)
            && entry.kind_key == key {
                entry.used = true;
                if echo.is_some() && echo == entry.state {
                    // The processor already has these bytes: stale note.
                    self.pending_echoes.remove(&device.id);
                }
                let echoed = state.is_some() && echo == state;
                if entry.state == state || echoed {
                    if echoed {
                        // The live processor wrote these bytes itself.
                        self.pending_echoes.remove(&device.id);
                        entry.state = state;
                    }
                    let mut init = Vec::new();
                    for (&id, &v) in params {
                        if entry.applied.get(&id).is_none_or(|old| (old - v).abs() > 1e-7) {
                            init.push((id, v));
                        }
                    }
                    entry.applied.clone_from(params);
                    return Some(Resolved { proc: entry.proc.clone(), init_params: init, is_instrument: device.is_instrument(), meter: entry.meter.clone() });
                }
                // New state: rebuild below, keeping the meter the UI reads.
                meter = Some(entry.meter.clone());
            }
        self.pending_echoes.remove(&device.id);
        let Some(mut processor) = create_with_fallback(factory, device, sample_rate, max_frames) else {
            // Keep the old processor rather than drop the device; do not
            // retry on every compile.
            let entry = self.entries.get_mut(&device.id).filter(|e| e.kind_key == key)?;
            entry.state = state;
            return Some(Resolved { proc: entry.proc.clone(), init_params: Vec::new(), is_instrument: device.is_instrument(), meter: entry.meter.clone() });
        };
        processor.prepare(sample_rate, max_frames);
        if let Some(state) = &device.state {
            processor.load_state(state);
        }
        for (&id, &v) in params {
            processor.set_param(id, v);
        }
        let is_instrument = device.is_instrument() || processor.info().category == aitrack_dsp::Category::Instrument;
        let proc = Arc::new(SharedProcessor::new(processor));
        let meter = meter.unwrap_or_default();
        // Replacing an entry drops only the pool's clone: the graph that
        // still runs the old processor frees it when it is retired.
        self.entries.insert(
            device.id,
            PoolEntry { kind_key: key, state, proc: proc.clone(), meter: meter.clone(), applied: params.clone(), used: true },
        );
        Some(Resolved { proc, init_params: Vec::new(), is_instrument, meter })
    }

    /// Re-prepare every pooled processor for a new sample rate / block size.
    ///
    /// # Safety
    /// No graph referencing these processors may be running (call after the
    /// audio stream has been dropped).
    pub(crate) unsafe fn reprepare_all(&mut self, sample_rate: f64, max_frames: usize) {
        for e in self.entries.values() {
            // SAFETY: guaranteed by the caller.
            let p = unsafe { e.proc.get_mut() };
            p.prepare(sample_rate, max_frames);
            e.proc.publish_latency(p.latency());
        }
    }

    /// Forget which parameter values were applied, so the next compile sends
    /// every value again (after a runner rebuild dropped queued graphs).
    pub(crate) fn forget_applied(&mut self) {
        for e in self.entries.values_mut() {
            e.applied.clear();
        }
    }

}
