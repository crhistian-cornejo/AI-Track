//! Test-only global allocator that counts heap operations made by the
//! current thread while a closure runs, so tests can prove the real-time
//! path neither allocates nor frees. The DSP workers of the pools a test
//! names are counted too ([`count_allocs_in_pools`]).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

struct Counting;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
    /// On a DSP worker: the id of its pool (`0` elsewhere).
    static POOL: Cell<usize> = const { Cell::new(0) };
}

/// The pools whose workers are being counted (`0` = free slot).
static MEASURED: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
static WORKER_COUNT: AtomicU64 = AtomicU64::new(0);

#[inline]
fn bump() {
    let _ = ACTIVE.try_with(|a| {
        if a.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
    let _ = POOL.try_with(|p| {
        let id = p.get();
        if id != 0 && MEASURED.iter().any(|m| m.load(Ordering::Relaxed) == id) {
            WORKER_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        bump();
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Run `f`, returning its result and the number of heap events on this thread.
pub(crate) fn count_allocs<R>(f: impl FnOnce() -> R) -> (R, u64) {
    COUNT.with(|c| c.set(0));
    ACTIVE.with(|a| a.set(true));
    let r = f();
    ACTIVE.with(|a| a.set(false));
    (r, COUNT.with(Cell::get))
}

/// A DSP worker of pool `id` calls this when it starts.
pub(crate) fn mark_worker(id: usize) {
    POOL.with(|p| p.set(id));
}

/// Like [`count_allocs`], plus the heap events of every worker of the pools
/// `ids` ([`crate::workers::WorkerPool::id`]) while `f` runs. Tests that use
/// it must not run at the same time as each other (they share one counter):
/// take [`WORKER_COUNT_LOCK`].
pub(crate) fn count_allocs_in_pools<R>(ids: &[usize], f: impl FnOnce() -> R) -> (R, u64) {
    assert!(ids.len() <= MEASURED.len());
    WORKER_COUNT.store(0, Ordering::SeqCst);
    for (m, id) in MEASURED.iter().zip(ids) {
        m.store(*id, Ordering::SeqCst);
    }
    let (r, local) = count_allocs(f);
    for m in &MEASURED {
        m.store(0, Ordering::SeqCst);
    }
    (r, local + WORKER_COUNT.load(Ordering::SeqCst))
}

/// Serialises the tests that use [`count_allocs_in_pools`].
pub(crate) static WORKER_COUNT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
