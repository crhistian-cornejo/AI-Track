//! How the engine obtains processors for the devices in a project.
//!
//! The engine never knows which plugins exist: the application injects one
//! or more [`ProcessorFactory`]s (stock devices from `aitrack-stock`, hosted
//! VST3/CLAP plugins from `aitrack-host`) and the graph compiler asks them in
//! order for every [`Device`] it meets.

use aitrack_dsp::Processor;
use aitrack_model::Device;
use std::sync::Arc;

/// Creates processors for devices. Called on the engine's control thread
/// (never on the audio thread), so it may allocate, load files and block.
pub trait ProcessorFactory: Send + Sync {
    /// Build a processor for `device`, or `None` if this factory does not
    /// know the device kind. The engine calls [`Processor::prepare`] itself
    /// afterwards, then loads `device.state` and `device.params`.
    fn create(&self, device: &aitrack_model::Device, sample_rate: f64, max_frames: usize) -> Option<Box<dyn Processor>>;
}

/// Tries several factories in order (e.g. stock first, then hosted plugins).
#[derive(Default, Clone)]
pub struct FactoryStack {
    factories: Vec<Arc<dyn ProcessorFactory>>,
}

impl FactoryStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, factory: Arc<dyn ProcessorFactory>) -> Self {
        self.factories.push(factory);
        self
    }

    pub fn push(&mut self, factory: Arc<dyn ProcessorFactory>) {
        self.factories.push(factory);
    }
}

impl ProcessorFactory for FactoryStack {
    fn create(&self, device: &Device, sample_rate: f64, max_frames: usize) -> Option<Box<dyn Processor>> {
        self.factories.iter().find_map(|f| f.create(device, sample_rate, max_frames))
    }
}

/// A factory that knows nothing; every device becomes a pass-through.
pub struct NullFactory;

impl ProcessorFactory for NullFactory {
    fn create(&self, _: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        None
    }
}

/// Ask `factory`, then (in tests or with the `test-nodes` feature) the
/// built-in fallback nodes.
pub(crate) fn create_with_fallback(
    factory: &dyn ProcessorFactory,
    device: &Device,
    sample_rate: f64,
    max_frames: usize,
) -> Option<Box<dyn Processor>> {
    if let Some(p) = factory.create(device, sample_rate, max_frames) {
        return Some(p);
    }
    #[cfg(any(test, feature = "test-nodes"))]
    {
        crate::fallback::FallbackFactory.create(device, sample_rate, max_frames)
    }
    #[cfg(not(any(test, feature = "test-nodes")))]
    {
        None
    }
}
