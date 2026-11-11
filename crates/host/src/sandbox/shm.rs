//! Named shared memory and cross-process signals.
//!
//! - Windows: `CreateFileMappingW` / `MapViewOfFile` and named auto-reset
//!   events (`SetEvent` / `WaitForSingleObject`).
//! - macOS: `shm_open` + `mmap` and POSIX named semaphores. macOS has no
//!   `sem_timedwait`, so timed waits poll `sem_trywait` with spin + yield
//!   (bounded by the block budget); untimed waits block in `sem_wait`.

use std::time::Duration;

use crate::error::{HostError, Result};

fn err(what: &str, e: impl std::fmt::Display) -> HostError {
    HostError::Sandbox(format!("{what}: {e}"))
}

pub struct SharedMemory {
    ptr: *mut u8,
    len: usize,
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    name: Option<std::ffi::CString>,
}

// SAFETY: the mapping is plain memory shared with another process; access
// is coordinated by the sandbox protocol (atomics + signals).
unsafe impl Send for SharedMemory {}
unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

pub struct Signal {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    sem: *mut libc::sem_t,
    #[cfg(unix)]
    name: Option<std::ffi::CString>,
}

unsafe impl Send for Signal {}
unsafe impl Sync for Signal {}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows::Win32::System::Memory::*;
    use windows::Win32::System::Threading::*;
    use windows::core::PCWSTR;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl SharedMemory {
        pub fn create(name: &str, len: usize) -> Result<Self> {
            let w = wide(name);
            unsafe {
                let handle =
                    CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, (len as u64 >> 32) as u32, len as u32, PCWSTR(w.as_ptr()))
                        .map_err(|e| err("CreateFileMappingW", e))?;
                Self::map(handle, len)
            }
        }

        pub fn open(name: &str, len: usize) -> Result<Self> {
            let w = wide(name);
            unsafe {
                let handle = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(w.as_ptr())).map_err(|e| err("OpenFileMappingW", e))?;
                Self::map(handle, len)
            }
        }

        unsafe fn map(handle: HANDLE, len: usize) -> Result<Self> {
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) };
            if view.Value.is_null() {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(err("MapViewOfFile", windows::core::Error::from_thread()));
            }
            Ok(Self { ptr: view.Value as *mut u8, len, handle })
        }
    }

    impl Drop for SharedMemory {
        fn drop(&mut self) {
            unsafe {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut _ });
                let _ = CloseHandle(self.handle);
            }
        }
    }

    impl Signal {
        pub fn create(name: &str) -> Result<Self> {
            let w = wide(name);
            let handle = unsafe { CreateEventW(None, false, false, PCWSTR(w.as_ptr())) }.map_err(|e| err("CreateEventW", e))?;
            Ok(Self { handle })
        }

        pub fn open(name: &str) -> Result<Self> {
            let w = wide(name);
            let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(w.as_ptr())) }
                .map_err(|e| err("OpenEventW", e))?;
            Ok(Self { handle })
        }

        pub fn post(&self) {
            unsafe {
                let _ = SetEvent(self.handle);
            }
        }

        /// Wait up to `timeout`; true if signalled.
        pub fn wait(&self, timeout: Duration) -> bool {
            let ms = timeout.as_micros().div_ceil(1000).min(u32::MAX as u128) as u32;
            unsafe { WaitForSingleObject(self.handle, ms) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Signal {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::time::Instant;

    fn cname(name: &str) -> Result<CString> {
        CString::new(name).map_err(|e| err("name", e))
    }

    impl SharedMemory {
        pub fn create(name: &str, len: usize) -> Result<Self> {
            let n = cname(name)?;
            unsafe {
                libc::shm_unlink(n.as_ptr());
                let fd = libc::shm_open(n.as_ptr(), libc::O_CREAT | libc::O_RDWR | libc::O_EXCL, 0o600);
                if fd < 0 {
                    return Err(err("shm_open", std::io::Error::last_os_error()));
                }
                if libc::ftruncate(fd, len as libc::off_t) != 0 {
                    libc::close(fd);
                    return Err(err("ftruncate", std::io::Error::last_os_error()));
                }
                Self::map(fd, len, Some(n))
            }
        }

        pub fn open(name: &str, len: usize) -> Result<Self> {
            let n = cname(name)?;
            unsafe {
                let fd = libc::shm_open(n.as_ptr(), libc::O_RDWR, 0o600);
                if fd < 0 {
                    return Err(err("shm_open", std::io::Error::last_os_error()));
                }
                Self::map(fd, len, None)
            }
        }

        unsafe fn map(fd: libc::c_int, len: usize, owner: Option<CString>) -> Result<Self> {
            let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) };
            unsafe { libc::close(fd) };
            if ptr == libc::MAP_FAILED {
                return Err(err("mmap", std::io::Error::last_os_error()));
            }
            Ok(Self { ptr: ptr as *mut u8, len, name: owner })
        }
    }

    impl Drop for SharedMemory {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.len);
                if let Some(n) = &self.name {
                    libc::shm_unlink(n.as_ptr());
                }
            }
        }
    }

    impl Signal {
        pub fn create(name: &str) -> Result<Self> {
            let n = cname(name)?;
            unsafe {
                libc::sem_unlink(n.as_ptr());
                let sem = libc::sem_open(n.as_ptr(), libc::O_CREAT | libc::O_EXCL, 0o600 as libc::c_uint, 0 as libc::c_uint);
                if sem == libc::SEM_FAILED {
                    return Err(err("sem_open", std::io::Error::last_os_error()));
                }
                Ok(Self { sem, name: Some(n) })
            }
        }

        pub fn open(name: &str) -> Result<Self> {
            let n = cname(name)?;
            let sem = unsafe { libc::sem_open(n.as_ptr(), 0) };
            if sem == libc::SEM_FAILED {
                return Err(err("sem_open", std::io::Error::last_os_error()));
            }
            Ok(Self { sem, name: None })
        }

        pub fn post(&self) {
            unsafe { libc::sem_post(self.sem) };
        }

        pub fn wait(&self, timeout: Duration) -> bool {
            let start = Instant::now();
            let mut spins = 0u32;
            loop {
                if unsafe { libc::sem_trywait(self.sem) } == 0 {
                    return true;
                }
                if start.elapsed() >= timeout {
                    return false;
                }
                spins += 1;
                if spins < 64 {
                    std::hint::spin_loop();
                } else if timeout > Duration::from_millis(50) {
                    // Long (non-RT) waits: don't burn a core.
                    std::thread::sleep(Duration::from_micros(200));
                } else {
                    std::thread::yield_now();
                }
            }
        }
    }

    impl Drop for Signal {
        fn drop(&mut self) {
            unsafe {
                libc::sem_close(self.sem);
                if let Some(n) = &self.name {
                    libc::sem_unlink(n.as_ptr());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_and_signal_round_trip() {
        let id = std::process::id();
        #[cfg(windows)]
        let (m, s) = (format!("Local\\aitrack-test-shm-{id}"), format!("Local\\aitrack-test-sig-{id}"));
        #[cfg(unix)]
        let (m, s) = (format!("/aitkt{id}m"), format!("/aitkt{id}s"));
        let a = SharedMemory::create(&m, 4096).unwrap();
        let b = SharedMemory::open(&m, 4096).unwrap();
        unsafe {
            *a.as_ptr().add(100) = 42;
            assert_eq!(*b.as_ptr().add(100), 42);
        }
        let sig = Signal::create(&s).unwrap();
        let other = Signal::open(&s).unwrap();
        assert!(!sig.wait(Duration::from_millis(1)));
        other.post();
        assert!(sig.wait(Duration::from_millis(100)));
    }
}
