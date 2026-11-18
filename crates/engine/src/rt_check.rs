//! A heap check for the audio threads, for debug builds of the app: an
//! allocator that counts every allocation, reallocation and free made while
//! a thread is inside a real-time section (the device callback, a DSP
//! worker running a job). The tests prove the engine's own path clean
//! (`alloc_counter`, test builds only); this catches what they cannot
//! reach: a code path only a real session takes, or a plugin that
//! allocates in `process`.
//!
//! The app installs it in debug builds:
//! ```ignore
//! #[cfg(debug_assertions)]
//! #[global_allocator]
//! static RT_CHECK: aitrack_engine::RtAllocChecker = aitrack_engine::RtAllocChecker;
//! ```
//! and reads [`rt_heap_operations`]. Without it installed the sections cost
//! one thread-local store each. (`assert_no_alloc` does the same, but its
//! BSD-1-Clause licence is not in `deny.toml`.)

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// The thread is in a real-time section. Const-initialised, no
    /// destructor: reading it from the allocator never allocates.
    static IN_RT: Cell<bool> = const { Cell::new(false) };
}

static HEAP_OPS: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counting heap operations made inside real-time
/// sections (see the module docs).
pub struct RtAllocChecker;

#[inline]
fn note() {
    if IN_RT.try_with(Cell::get).unwrap_or(false) {
        HEAP_OPS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for RtAllocChecker {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Heap operations made inside real-time sections since the process
/// started (always 0 unless [`RtAllocChecker`] is the global allocator).
pub fn rt_heap_operations() -> u64 {
    HEAP_OPS.load(Ordering::Relaxed)
}

/// A real-time section: from [`enter`] until it drops (sections nest).
pub(crate) struct RtSection {
    was: bool,
}

/// Mark the calling thread as in a real-time section.
#[inline]
pub(crate) fn enter() -> RtSection {
    RtSection { was: IN_RT.try_with(|f| f.replace(true)).unwrap_or(false) }
}

impl Drop for RtSection {
    #[inline]
    fn drop(&mut self) {
        let _ = IN_RT.try_with(|f| f.set(self.was));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Through the checker by hand (test builds have their own global
    /// allocator): only what happens inside a section counts, nested
    /// sections end where they started.
    #[test]
    fn heap_operations_count_only_inside_real_time_sections() {
        let layout = Layout::from_size_align(64, 8).unwrap();
        let ops = move || {
            // SAFETY: a fresh block of `layout`, freed with the same layout.
            unsafe {
                let p = RtAllocChecker.alloc(layout);
                let p = RtAllocChecker.realloc(p, layout, 128);
                RtAllocChecker.dealloc(p, Layout::from_size_align(128, 8).unwrap());
            }
        };
        let before = rt_heap_operations();
        ops();
        assert_eq!(rt_heap_operations(), before, "outside: not counted");
        {
            let _rt = enter();
            ops();
            {
                let _inner = enter();
            }
            // Still inside after a nested section ended.
            // SAFETY: as above.
            unsafe { RtAllocChecker.dealloc(RtAllocChecker.alloc_zeroed(layout), layout) };
        }
        // (Only this test calls the checker by hand: the count is exact.)
        assert_eq!(rt_heap_operations(), before + 5);
        ops();
        assert_eq!(rt_heap_operations(), before + 5, "after the section: not counted");
        // Another thread is not in this thread's section.
        let other = std::thread::spawn(move || {
            let _rt = enter();
            drop(_rt);
            ops();
        });
        other.join().unwrap();
        assert_eq!(rt_heap_operations(), before + 5);
    }
}
