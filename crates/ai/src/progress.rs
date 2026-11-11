//! Progress reporting and cooperative cancellation for long jobs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Which phase a long job is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Downloading,
    Extracting,
    Verifying,
    Decoding,
    LoadingModel,
    Inference,
    PostProcessing,
    Writing,
    Done,
}

/// One progress event. `fraction` is the overall job progress in 0..=1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: Stage,
    pub fraction: f32,
    pub message: String,
}

impl Progress {
    pub fn new(stage: Stage, fraction: f32, message: impl Into<String>) -> Self {
        Self {
            stage,
            fraction: fraction.clamp(0.0, 1.0),
            message: message.into(),
        }
    }
}

/// Shared flag a UI sets to stop a job. Jobs poll it between chunks.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(Error::Cancelled)` once cancelled, for use with `?`.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Progress callback type used across the crate.
pub type ProgressFn<'a> = &'a mut dyn FnMut(Progress);

/// A progress sink that ignores everything.
pub fn no_progress() -> impl FnMut(Progress) {
    |_| {}
}
