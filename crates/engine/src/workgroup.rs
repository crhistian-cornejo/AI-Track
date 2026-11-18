//! The audio workgroup of the output device (macOS 11+), which the DSP
//! workers join so the scheduler treats them and CoreAudio's IO thread as
//! one real-time group with one deadline.
//!
//! # Why
//! On Apple silicon a real-time thread that is not in the IO thread's
//! workgroup is scheduled on its own: under light load the system may keep
//! it on an efficiency core or let it sleep through the deadline, and a
//! parallel graph then drops out with the CPU meter low. Apple's guidance
//! ("Adding parallel real-time threads to audio workgroups", WWDC20 "Meet
//! Audio Workgroups") is to give every helper thread a real-time policy
//! *and* join the device's workgroup. So both are done: a worker first
//! takes the time-constraint policy ([`crate::thread_priority`]), the
//! fallback when no workgroup is available (macOS 10.x, a device that does
//! not publish one, a failed join), then joins.
//!
//! # Where the workgroup comes from
//! cpal 0.18 does not hand out the `AudioObjectID` of the device it opened,
//! but it does give its UID (`Device::id()` on CoreAudio is the
//! `kAudioDevicePropertyDeviceUID` string). [`AudioWorkgroup::for_device_uid`]
//! translates the UID back to the device with
//! `kAudioHardwarePropertyTranslateUIDToDevice` and reads
//! `kAudioDevicePropertyIOThreadOSWorkgroup` from it (a +1 `os_workgroup_t`,
//! released when the [`AudioWorkgroup`] drops). The engine does this on the
//! control thread each time it opens a device, before it builds the worker
//! pool, so a device change hands the next pool the new workgroup.
//!
//! # Joining
//! `os_workgroup_join` / `os_workgroup_leave` are real-time safe but the
//! join token belongs to the joining thread, and a thread must leave before
//! it exits. A worker joins once, when it starts (the pool lives as long as
//! the device stream, and is rebuilt when the device or the thread count
//! changes), holds the [`Joined`] guard for its whole life and leaves when
//! it returns. Nothing happens per block. Elsewhere than macOS every call
//! here is a no-op that returns `None`.

use std::sync::Arc;

/// The device IO thread's workgroup. `Send + Sync`: the os_workgroup API
/// may be called from any thread.
pub struct AudioWorkgroup {
    #[cfg(target_os = "macos")]
    wg: imp::Wg,
}

impl std::fmt::Debug for AudioWorkgroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioWorkgroup")
    }
}

/// Proof that the current thread is in a workgroup; leaves it when dropped.
/// Not `Send`: the token is only valid on the thread that joined.
pub struct Joined {
    #[cfg(target_os = "macos")]
    inner: imp::Joined,
    /// The workgroup stays alive while the thread is in it.
    #[cfg(target_os = "macos")]
    wg: Arc<AudioWorkgroup>,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl AudioWorkgroup {
    /// The IO workgroup of the CoreAudio device with this UID, if the system
    /// has one for it. Call off the audio thread (it asks the HAL).
    pub fn for_device_uid(uid: &str) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            imp::for_device_uid(uid).map(|wg| Self { wg })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = uid;
            None
        }
    }

    /// The workgroup of a device cpal opened (CoreAudio only).
    pub fn for_cpal_device(device: &cpal::Device) -> Option<Arc<Self>> {
        use cpal::traits::DeviceTrait;
        let id = device.id().ok()?;
        #[cfg(target_os = "macos")]
        if id.host() != cpal::HostId::CoreAudio {
            return None;
        }
        Self::for_device_uid(id.id()).map(Arc::new)
    }

    /// Join from the calling thread. `None` when the system refuses (the
    /// thread is in a workgroup that does not nest with this one, or the
    /// workgroup was cancelled because the device went away): the thread
    /// keeps its real-time policy and works as before. Real-time safe.
    pub fn join(self: &Arc<Self>) -> Option<Joined> {
        #[cfg(target_os = "macos")]
        {
            imp::join(&self.wg).map(|inner| Joined { inner, wg: self.clone(), _not_send: std::marker::PhantomData })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// How many threads the system suggests running in parallel in this
    /// workgroup (`None` when it does not say).
    pub fn max_parallel_threads(&self) -> Option<usize> {
        #[cfg(target_os = "macos")]
        {
            imp::max_parallel_threads(&self.wg)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

impl Drop for Joined {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        imp::leave(&self.wg.wg, &mut self.inner);
    }
}

/// A four-character code as CoreAudio spells its selectors.
#[cfg(any(test, target_os = "macos"))]
const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_core_audio::{
        kAudioDevicePropertyIOThreadOSWorkgroup, kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
        kAudioObjectSystemObject, kAudioObjectUnknown, AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    };
    use objc2_core_foundation::CFString;
    use std::ffi::{c_int, c_void};
    use std::ptr::NonNull;

    // The selectors are the documented four-character codes.
    const _: () = assert!(kAudioDevicePropertyIOThreadOSWorkgroup == super::fourcc(b"oswg"));
    const _: () = assert!(kAudioHardwarePropertyTranslateUIDToDevice == super::fourcc(b"uidd"));

    /// `os_workgroup_join_token_s`: a `uint32_t` signature and 36 opaque
    /// bytes on 64-bit (`__OS_WORKGROUP_JOIN_TOKEN_SIZE__`); the client owns
    /// the storage. Rounded up, aligned for any field it may hold.
    #[repr(C, align(16))]
    pub(super) struct Token([u8; 64]);

    unsafe extern "C" {
        fn os_workgroup_join(wg: *mut c_void, token_out: *mut Token) -> c_int;
        fn os_workgroup_leave(wg: *mut c_void, token: *mut Token);
        fn os_workgroup_max_parallel_threads(wg: *mut c_void, attr: *mut c_void) -> c_int;
        fn os_release(object: *mut c_void);
    }

    /// An owned (+1) `os_workgroup_t`.
    pub(super) struct Wg(NonNull<c_void>);

    // SAFETY: os_workgroup objects are thread-safe; every call here takes
    // the object by pointer and the reference is released once, on drop.
    unsafe impl Send for Wg {}
    unsafe impl Sync for Wg {}

    impl Drop for Wg {
        fn drop(&mut self) {
            // SAFETY: the property hands out a +1 reference ("the caller is
            // responsible for releasing the returned object").
            unsafe { os_release(self.0.as_ptr()) }
        }
    }

    pub(super) struct Joined {
        token: Box<Token>,
    }

    fn address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress { mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain }
    }

    fn device_for_uid(uid: &str) -> Option<AudioObjectID> {
        let cf_uid = CFString::from_str(uid);
        let qualifier: *const CFString = &*cf_uid;
        let addr = address(kAudioHardwarePropertyTranslateUIDToDevice);
        let mut device: AudioObjectID = kAudioObjectUnknown;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        // SAFETY: the qualifier is a CFStringRef (pointer-sized) that
        // outlives the call; the out buffer is an AudioObjectID of `size`.
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&addr),
                std::mem::size_of::<*const CFString>() as u32,
                (&qualifier as *const *const CFString).cast(),
                NonNull::from(&mut size),
                NonNull::from(&mut device).cast(),
            )
        };
        (status == 0 && device != kAudioObjectUnknown).then_some(device)
    }

    pub(super) fn for_device_uid(uid: &str) -> Option<Wg> {
        let device = device_for_uid(uid)?;
        let addr = address(kAudioDevicePropertyIOThreadOSWorkgroup);
        let mut wg: *mut c_void = std::ptr::null_mut();
        let mut size = std::mem::size_of::<*mut c_void>() as u32;
        // SAFETY: the property's data is one `os_workgroup_t` (a pointer).
        let status = unsafe { AudioObjectGetPropertyData(device, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut wg).cast()) };
        if status != 0 {
            return None;
        }
        NonNull::new(wg).map(Wg)
    }

    pub(super) fn join(wg: &Wg) -> Option<Joined> {
        // The token is allocated before joining: this runs once per worker,
        // when it starts, off the per-block path.
        let mut token = Box::new(Token([0; 64]));
        // SAFETY: `wg` is a live workgroup; the token storage is ours and
        // stays put (boxed) until the matching leave.
        let r = unsafe { os_workgroup_join(wg.0.as_ptr(), &mut *token) };
        (r == 0).then_some(Joined { token })
    }

    pub(super) fn leave(wg: &Wg, j: &mut Joined) {
        // SAFETY: same thread (the guard is not `Send`), same workgroup and
        // token as the join; the guard holds the workgroup alive.
        unsafe { os_workgroup_leave(wg.0.as_ptr(), &mut *j.token) }
    }

    pub(super) fn max_parallel_threads(wg: &Wg) -> Option<usize> {
        // SAFETY: a NULL attribute asks for the default answer.
        let n = unsafe { os_workgroup_max_parallel_threads(wg.0.as_ptr(), std::ptr::null_mut()) };
        (n > 0).then_some(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_are_the_documented_codes() {
        assert_eq!(fourcc(b"oswg"), 0x6f73_7767);
        assert_eq!(fourcc(b"uidd"), 0x7569_6464);
    }

    #[test]
    fn an_unknown_device_has_no_workgroup() {
        assert!(AudioWorkgroup::for_device_uid("aitrack-no-such-device-uid").is_none());
        assert!(AudioWorkgroup::for_device_uid("").is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_join_token_has_room_for_the_system_struct() {
        // `uint32_t sig` + `char opaque[36]` on LP64.
        assert!(std::mem::size_of::<imp::Token>() >= 4 + 36);
    }

    /// Needs an output device: `cargo test -p aitrack-engine -- --ignored real_device`.
    /// Opens the default output's workgroup and joins it from a spawned
    /// thread that first took the workers' real-time policy.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn real_device_workgroup_is_joined_from_a_worker_thread() {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        let device = host.default_output_device().expect("an output device");
        let wg = AudioWorkgroup::for_cpal_device(&device).expect("the default output publishes an IO workgroup");
        eprintln!("max parallel threads: {:?}", wg.max_parallel_threads());
        let wg2 = wg.clone();
        let joined = std::thread::spawn(move || {
            let promoted = crate::thread_priority::promote_worker_thread(std::time::Duration::from_millis(10));
            let j = wg2.join();
            let ok = j.is_some();
            drop(j);
            // Leaving and joining again works (as after a device change).
            let again = wg2.join().is_some();
            (promoted, ok, again)
        })
        .join()
        .unwrap();
        assert_eq!(joined, (true, true, true));
    }

    /// Needs an output device: every worker of a live pool built with the
    /// default output's workgroup is in it (the pool the engine builds).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn real_device_workers_of_a_live_pool_join_the_workgroup() {
        use crate::workers::{PoolKind, WorkerPool};
        use cpal::traits::HostTrait;
        let device = cpal::default_host().default_output_device().expect("an output device");
        let wg = AudioWorkgroup::for_cpal_device(&device).expect("an IO workgroup");
        let mut pool = WorkerPool::with_workgroup(4, PoolKind::Live(std::time::Duration::from_millis(10)), Some(wg));
        let n = std::sync::atomic::AtomicUsize::new(0);
        assert!(pool.run(16, &|_| {
            n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        let t0 = std::time::Instant::now();
        while pool.workgroup_members() < 3 {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5), "only {} workers joined", pool.workgroup_members());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        drop(pool);
        // An offline pool never joins.
        let wg = AudioWorkgroup::for_cpal_device(&device).expect("an IO workgroup");
        let pool = WorkerPool::with_workgroup(3, PoolKind::Offline, Some(wg));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(pool.workgroup_members(), 0);
    }
}
