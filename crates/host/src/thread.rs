//! Main-thread confinement.
//!
//! VST3 controllers/views and CLAP `[main-thread]` functions must only be
//! called from the thread that created the plugin. [`MainThreadCell`] makes
//! that a runtime check instead of a convention, so the owning
//! [`HostedPlugin`](crate::HostedPlugin) can be `Send` (it moves to the
//! audio thread) while its main-thread half refuses calls from elsewhere.

use parking_lot::Mutex;
use std::mem::ManuallyDrop;
use std::thread::ThreadId;

use crate::error::{HostError, Result};

pub(crate) struct MainThreadCell<T> {
    owner: ThreadId,
    // The mutex is never touched by the audio thread; it only serialises
    // re-entrant main-thread access (e.g. a plugin calling back into us).
    inner: ManuallyDrop<Mutex<T>>,
}

// SAFETY: every access to `inner` goes through `with`, which checks that the
// caller is the owning thread, and `Drop` leaks rather than dropping on a
// foreign thread. `T` is therefore only ever touched by one thread.
unsafe impl<T> Send for MainThreadCell<T> {}
unsafe impl<T> Sync for MainThreadCell<T> {}

impl<T> MainThreadCell<T> {
    pub fn new(value: T) -> Self {
        Self { owner: std::thread::current().id(), inner: ManuallyDrop::new(Mutex::new(value)) }
    }

    pub fn is_owner(&self) -> bool {
        std::thread::current().id() == self.owner
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R> {
        if !self.is_owner() {
            return Err(HostError::WrongThread);
        }
        // Re-entrancy (plugin → host callback → us) would deadlock a plain
        // lock; treat it as a programming error and refuse.
        match self.inner.try_lock() {
            Some(mut guard) => Ok(f(&mut guard)),
            None => {
                log::error!("re-entrant main-thread access to a hosted plugin refused");
                Err(HostError::WrongThread)
            }
        }
    }
}

impl<T> Drop for MainThreadCell<T> {
    fn drop(&mut self) {
        if self.is_owner() {
            // SAFETY: dropped exactly once, here.
            unsafe { ManuallyDrop::drop(&mut self.inner) };
        } else {
            log::error!(
                "hosted plugin dropped off its main thread; leaking it instead of calling its \
                 destructor on the wrong thread"
            );
        }
    }
}
