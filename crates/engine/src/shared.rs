//! Processor instances shared between successive compiled graphs.
//!
//! Recompiling the graph on every edit must not rebuild plugins (that would
//! lose their state and cost milliseconds). Each processor therefore lives in
//! a [`SharedProcessor`] cell behind an `Arc`, kept by the control thread's
//! [`ProcessorPool`] keyed by [`DeviceId`]. A new graph clones the `Arc` of
//! every device whose id and kind are unchanged; the old graph keeps its own
//! clone until it is retired and dropped on the graveyard thread, so the last
//! reference (and the plugin's teardown) never goes away on the audio thread.

use crate::factory::{create_with_fallback, ProcessorFactory};
use aitrack_dsp::Processor;
use aitrack_model::{Device, DeviceId, DeviceKind};
use std::cell::UnsafeCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

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
}

// SAFETY: access is serialised by the contract above; `Box<dyn Processor>`
// is `Send`, so moving the processor between threads is sound.
unsafe impl Sync for SharedProcessor {}
unsafe impl Send for SharedProcessor {}

impl SharedProcessor {
    pub fn new(processor: Box<dyn Processor>) -> Self {
        let latency = processor.latency();
        Self { inner: UnsafeCell::new(processor), latency: AtomicU32::new(latency) }
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

struct PoolEntry {
    kind_key: String,
    proc: Arc<SharedProcessor>,
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
}

/// Control-thread cache of live processors keyed by device id.
#[derive(Default)]
pub struct ProcessorPool {
    entries: HashMap<DeviceId, PoolEntry>,
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
    }

    /// Drop pool references to devices the last compile did not use. Any
    /// graph still holding them keeps them alive until it is retired.
    pub(crate) fn end(&mut self) {
        self.entries.retain(|_, e| e.used);
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
        if let Some(entry) = self.entries.get_mut(&device.id)
            && entry.kind_key == key {
                entry.used = true;
                let mut init = Vec::new();
                for (&id, &v) in params {
                    if entry.applied.get(&id).is_none_or(|old| (old - v).abs() > 1e-7) {
                        init.push((id, v));
                    }
                }
                entry.applied.clone_from(params);
                return Some(Resolved { proc: entry.proc.clone(), init_params: init, is_instrument: device.is_instrument() });
            }
        let mut processor = create_with_fallback(factory, device, sample_rate, max_frames)?;
        processor.prepare(sample_rate, max_frames);
        if let Some(state) = &device.state {
            processor.load_state(state);
        }
        for (&id, &v) in params {
            processor.set_param(id, v);
        }
        let is_instrument = device.is_instrument() || processor.info().category == aitrack_dsp::Category::Instrument;
        let proc = Arc::new(SharedProcessor::new(processor));
        self.entries.insert(
            device.id,
            PoolEntry { kind_key: key, proc: proc.clone(), applied: params.clone(), used: true },
        );
        Some(Resolved { proc, init_params: Vec::new(), is_instrument })
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
