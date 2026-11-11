//! Real-time priority for audio threads.
//!
//! * **Windows:** registers the calling thread with MMCSS under the
//!   "Pro Audio" task at critical priority (`AvSetMmThreadCharacteristicsW`).
//! * **macOS:** CoreAudio's IO thread already runs with a time-constraint
//!   policy, so the device callback needs nothing. TODO: when the engine
//!   gains worker threads they must join the device's audio workgroup
//!   (`os_workgroup_join` via `kAudioDevicePropertyIOThreadOSWorkgroup`).
//! * Elsewhere: no-op.
//!
//! Called once, on the first callback of each stream (one syscall, no
//! allocation).

/// Returns `true` if the thread was promoted.
pub fn promote_audio_thread() -> bool {
    imp::promote()
}

#[cfg(windows)]
mod imp {
    use windows::core::w;
    use windows::Win32::System::Threading::{AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, AVRT_PRIORITY_CRITICAL};

    pub fn promote() -> bool {
        let mut task_index = 0u32;
        // SAFETY: plain Win32 calls with a static wide string and a valid
        // out-pointer; the handle is intentionally kept for the thread's life.
        unsafe {
            match AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) {
                Ok(handle) => AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL).is_ok(),
                Err(_) => false,
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn promote() -> bool {
        false
    }
}
