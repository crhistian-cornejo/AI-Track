//! Real-time priority for audio threads: the device callback and the DSP
//! workers of [`crate::workers::WorkerPool`].
//!
//! * **Windows:** registers the calling thread with MMCSS under the
//!   "Pro Audio" task at critical priority (`AvSetMmThreadCharacteristicsW`).
//!   The callback and every worker do it.
//! * **macOS:** CoreAudio's IO thread already runs with a time-constraint
//!   policy, so the device callback needs nothing. A worker asks for the same
//!   kind of policy itself ([`promote_worker_thread`]): period = the device
//!   buffer's duration, computation = half of it, constraint = all of it,
//!   preemptible. It does not join the device's audio workgroup
//!   (`os_workgroup_join` with `kAudioDevicePropertyIOThreadOSWorkgroup`):
//!   cpal does not hand out the `AudioObjectID` of the device it opened, so
//!   the workgroup is not reachable from here. Without it the scheduler sees
//!   the workers as separate real-time threads instead of one group with
//!   the IO thread's deadline; on Apple silicon that can keep them on
//!   efficiency cores longer under light load. Joining needs the device id
//!   from the stream (a cpal change or our own CoreAudio lookup by UID).
//! * Elsewhere: no-op.
//!
//! The callback calls [`promote_audio_thread`] once, on the first callback
//! of each stream; a worker calls [`promote_worker_thread`] once when it
//! starts (off the audio thread). One syscall, no allocation.

use std::time::Duration;

/// Returns `true` if the thread was promoted.
pub fn promote_audio_thread() -> bool {
    imp::promote()
}

/// Promote a DSP worker that helps a device whose buffer lasts `period`.
/// Returns `true` if the thread was promoted.
pub fn promote_worker_thread(period: Duration) -> bool {
    imp::promote_worker(period)
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

    pub fn promote_worker(_period: std::time::Duration) -> bool {
        // MMCSS schedules "Pro Audio" threads by its own period.
        promote()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn promote() -> bool {
        false
    }

    // (`mach_timebase_info` is deprecated in `libc` in favour of `mach2`; it
    // is the same call.)
    #[allow(deprecated)]
    pub fn promote_worker(period: std::time::Duration) -> bool {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: plain syscall with a valid out-pointer.
        if unsafe { libc::mach_timebase_info(&mut tb) } != libc::KERN_SUCCESS || tb.numer == 0 || tb.denom == 0 {
            return false;
        }
        // Nanoseconds → Mach absolute time units.
        let abs = |ns: u128| (ns * tb.denom as u128 / tb.numer as u128).min(u32::MAX as u128) as u32;
        let p = period.as_nanos().clamp(500_000, 200_000_000);
        // xnu refuses a computation above its real-time quantum (50 ms) with
        // KERN_INVALID_ARGUMENT, which left workers at normal priority with
        // buffers over 100 ms: half the period, at most 50 ms.
        let mut policy = libc::thread_time_constraint_policy { period: abs(p), computation: abs((p / 2).min(50_000_000)), constraint: abs(p), preemptible: 1 };
        // SAFETY: `pthread_mach_thread_np(pthread_self())` is the calling
        // thread's port (no new port right is created); the policy struct is
        // the one this flavour expects and outlives the call.
        unsafe {
            libc::thread_policy_set(
                libc::pthread_mach_thread_np(libc::pthread_self()),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                &mut policy as *mut libc::thread_time_constraint_policy as libc::thread_policy_t,
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            ) == libc::KERN_SUCCESS
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    pub fn promote() -> bool {
        false
    }

    pub fn promote_worker(_period: std::time::Duration) -> bool {
        false
    }
}
