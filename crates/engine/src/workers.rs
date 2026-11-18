//! A fixed pool of DSP worker threads the audio thread hands independent
//! jobs to: no lock, no allocation, no channel on the real-time path.
//!
//! # Threads
//! [`WorkerPool::new`] spawns `threads − 1` workers (the audio thread is the
//! last one), off the audio thread: when the engine starts, when the device
//! is reopened or when the "Audio threads" setting changes. Each worker
//! promotes itself to real-time priority once
//! ([`crate::thread_priority::promote_worker_thread`]), joins the device's
//! audio workgroup when the pool was given one (macOS,
//! [`crate::workgroup`]; it leaves it when it exits), marks itself as an
//! audio thread (for the panic hook and for plugins' thread checks) and
//! then waits for work. Dropping the pool (never on the audio thread: it
//! is retired like a graph) stops and joins the workers.
//!
//! # One job
//! [`WorkerPool::run`] runs `f(0) … f(count − 1)`, each exactly once, on the
//! audio thread and the workers together, and returns when all are done.
//! The whole job is described by one atomic word, `claim`:
//! `seq (32 bits) | count (16) | next (16)`. The audio thread stores the
//! job's entry point (`call`, `ctx`) and `remaining = count`, then publishes
//! a fresh `claim` with `next = 0`. Everyone — the audio thread included —
//! takes the next index with a compare-and-swap on `claim` (`next + 1`), runs
//! it, and counts it off `remaining`. The audio thread spins until
//! `remaining` is 0: by then every write the job made is visible to it
//! (release decrements, acquire load).
//!
//! Why a worker never runs a stale job: the CAS that gives it index `k` of
//! job `seq` only succeeds while `claim` still reads `(seq, next = k)`, so
//! `k` is not done yet and the audio thread cannot have returned from that
//! `run` (it waits for every index). The `call` / `ctx` it read before the
//! CAS were written before that `claim` was published and cannot have been
//! replaced by the next job, which is only written after `run` returns. A
//! worker that read a `claim` that is already used up (or replaced) fails
//! the CAS or sees `next ≥ count` and never touches `ctx`.
//!
//! A panic inside `f` is caught where it happens (worker or audio thread),
//! still counted off, and reported by `run` once every index is done, so
//! the countdown always reaches 0 and the caller's stack frame (where `f`
//! lives) is never unwound while a worker may still use it.
//!
//! # Waking
//! A worker that finished a job spins on `claim` for a short while
//! (`spin`), then parks: it raises its `parked` flag, re-reads `claim`
//! (sequentially consistent, like the flag) and only then calls
//! `thread::park`. `run` publishes `claim` (sequentially consistent) and
//! unparks the parked workers it wants, so either the worker sees the new
//! job or the audio thread sees the flag: no lost wake-up. `Thread::unpark`
//! is an atomic swap plus, for a parked thread, one futex / ulock wake; it
//! never takes a lock nor allocates. Consecutive jobs of one block (the
//! levels of the graph) find the workers still spinning; between blocks
//! they sleep, so an idle engine costs no CPU.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use crate::workgroup::AudioWorkgroup;

/// Most indices one [`WorkerPool::run`] takes (16 bits of `claim`).
pub(crate) const MAX_JOBS: usize = 0xFFFF;
/// Most threads a pool may have (audio thread included).
pub const MAX_THREADS: usize = 64;

const IDX_MASK: u64 = 0xFFFF;
/// How long the owner spins for the last indices of a job before it starts
/// yielding its core between checks.
const YIELD_AFTER: Duration = Duration::from_micros(500);

#[inline]
fn pack(seq: u32, count: usize, next: usize) -> u64 {
    (seq as u64) << 32 | (count as u64 & IDX_MASK) << 16 | (next as u64 & IDX_MASK)
}

#[inline]
fn seq_of(c: u64) -> u32 {
    (c >> 32) as u32
}

#[inline]
fn count_of(c: u64) -> usize {
    ((c >> 16) & IDX_MASK) as usize
}

#[inline]
fn next_of(c: u64) -> usize {
    (c & IDX_MASK) as usize
}

/// Keeps the hot atomics on their own cache lines.
#[repr(align(128))]
struct Padded<T>(T);

impl<T> std::ops::Deref for Padded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// The entry point of a job: `call(ctx, index)`.
type Call = unsafe fn(*const (), usize);

struct Shared {
    claim: Padded<AtomicU64>,
    remaining: Padded<AtomicUsize>,
    /// The job's [`Call`] as an address.
    call: AtomicUsize,
    ctx: AtomicPtr<()>,
    /// Some index of the current job panicked.
    panicked: AtomicBool,
    shutdown: AtomicBool,
    /// Per worker: it is (about to be) parked and needs an unpark.
    parked: Box<[Padded<AtomicBool>]>,
    /// How long a worker spins for the next job before it parks.
    spin: Duration,
    /// The device's IO workgroup the workers join (live pools on macOS).
    workgroup: Option<Arc<AudioWorkgroup>>,
    /// Workers that are in `workgroup`.
    joined: AtomicUsize,
}

/// How the workers of a pool are scheduled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolKind {
    /// Helping a device callback whose buffer lasts this long: real-time
    /// priority, a short spin between jobs.
    Live(Duration),
    /// An offline render: normal priority, longer spins (blocks follow each
    /// other without a gap).
    Offline,
}

/// See the module docs. Owned by the audio thread once installed.
pub struct WorkerPool {
    shared: Arc<Shared>,
    /// The workers, to unpark them (a `Thread` is a handle: no allocation).
    threads: Vec<Thread>,
    handles: Vec<JoinHandle<()>>,
    /// Sequence number of the last job (the audio thread's own counter).
    seq: u32,
}

/// Threads the "Auto" setting runs on: one per physical core but one (left
/// to the UI and the system), at most 8, at least 1. The audio thread counts.
pub fn auto_threads() -> usize {
    num_cpus::get_physical().saturating_sub(1).clamp(1, 8)
}

/// Threads a setting asks for: `0` is Auto ([`auto_threads`]).
pub fn resolve_threads(setting: usize) -> usize {
    if setting == 0 { auto_threads() } else { setting.clamp(1, MAX_THREADS) }
}

impl WorkerPool {
    /// A pool with no workers: [`run`](Self::run) runs everything inline.
    pub fn serial() -> Self {
        Self::new(1, PoolKind::Offline)
    }

    /// `threads` threads in all (the caller's thread included), so
    /// `threads − 1` workers. Call off the audio thread. A worker the system
    /// refuses to start is left out (the pool is smaller, never wrong).
    pub fn new(threads: usize, kind: PoolKind) -> Self {
        Self::with_workgroup(threads, kind, None)
    }

    /// [`new`](Self::new), and a live pool's workers join `workgroup` (the
    /// output device's IO workgroup) after taking the real-time policy.
    pub fn with_workgroup(threads: usize, kind: PoolKind, workgroup: Option<Arc<AudioWorkgroup>>) -> Self {
        let workgroup = workgroup.filter(|_| matches!(kind, PoolKind::Live(_)));
        let workers = threads.clamp(1, MAX_THREADS) - 1;
        let spin = match kind {
            PoolKind::Live(_) => Duration::from_micros(50),
            PoolKind::Offline => Duration::from_micros(300),
        };
        let shared = Arc::new(Shared {
            claim: Padded(AtomicU64::new(0)),
            remaining: Padded(AtomicUsize::new(0)),
            call: AtomicUsize::new(0),
            ctx: AtomicPtr::new(std::ptr::null_mut()),
            panicked: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            parked: (0..workers).map(|_| Padded(AtomicBool::new(false))).collect(),
            spin,
            workgroup,
            joined: AtomicUsize::new(0),
        });
        let mut threads = Vec::with_capacity(workers);
        let mut handles = Vec::with_capacity(workers);
        for k in 0..workers {
            let sh = shared.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("aitrack-dsp-{}", k + 1))
                .stack_size(4 << 20)
                .spawn(move || worker_main(&sh, k, kind));
            match spawned {
                Ok(h) => {
                    threads.push(h.thread().clone());
                    handles.push(h);
                }
                Err(e) => {
                    log::warn!("could not start DSP worker {}: {e}", k + 1);
                    break;
                }
            }
        }
        Self { shared, threads, handles, seq: 0 }
    }

    /// Threads this pool runs a job on, the caller's included.
    pub fn threads(&self) -> usize {
        self.handles.len() + 1
    }

    /// Identity of the pool for the allocation counter of the tests.
    #[cfg(test)]
    pub(crate) fn id(&self) -> usize {
        Arc::as_ptr(&self.shared) as usize
    }

    /// Workers that joined the device's audio workgroup so far.
    pub fn workgroup_members(&self) -> usize {
        self.shared.joined.load(Ordering::Acquire)
    }

    /// Whether [`run`](Self::run) can use more than the calling thread.
    pub fn is_parallel(&self) -> bool {
        !self.handles.is_empty()
    }

    /// Run `f(k)` for every `k < count`, each exactly once, spread over the
    /// workers and the calling thread; returns when all are done. `false`
    /// when one of them panicked (each panic was caught where it happened;
    /// the others still ran). Real-time safe: no allocation, no lock.
    ///
    /// Only the thread that owns the pool calls this (it takes `&mut self`).
    pub fn run<F: Fn(usize) + Sync>(&mut self, count: usize, f: &F) -> bool {
        debug_assert!(count <= MAX_JOBS);
        if count == 0 {
            return true;
        }
        if self.threads.is_empty() || count == 1 || count > MAX_JOBS {
            let mut ok = true;
            for k in 0..count {
                ok &= catch(|| f(k));
            }
            return ok;
        }
        let sh = &*self.shared;
        sh.ctx.store(f as *const F as *mut (), Ordering::Relaxed);
        sh.call.store(trampoline::<F> as Call as usize, Ordering::Relaxed);
        sh.remaining.store(count, Ordering::Relaxed);
        self.seq = self.seq.wrapping_add(1);
        // Publishes `call`, `ctx`, `remaining` and everything the caller
        // wrote before (release), and orders against the parked flags.
        sh.claim.store(pack(self.seq, count, 0), Ordering::SeqCst);
        // Wake as many helpers as there are indices beyond the one this
        // thread takes first; a worker still spinning needs no call.
        for (t, parked) in self.threads.iter().zip(sh.parked.iter()).take(count - 1) {
            if parked.load(Ordering::SeqCst) {
                t.unpark();
            }
        }
        work(sh, self.seq);
        // The last indices are running elsewhere. Spin; if that takes long
        // (more threads than free cores: a worker holding an index may be
        // waiting for this very core), give the core away now and then.
        let mut spins = 0u32;
        let mut since: Option<Instant> = None;
        while sh.remaining.load(Ordering::Acquire) != 0 {
            std::hint::spin_loop();
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(256) {
                let now = Instant::now();
                if now.duration_since(*since.get_or_insert(now)) > YIELD_AFTER {
                    std::thread::yield_now();
                }
            }
        }
        !sh.panicked.swap(false, Ordering::Relaxed)
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        for t in &self.threads {
            t.unpark();
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

/// Call the job's closure. Only reached through `Shared::call`.
///
/// # Safety
/// `ctx` points to a live `F` (see [`WorkerPool::run`] and the module docs).
unsafe fn trampoline<F: Fn(usize) + Sync>(ctx: *const (), k: usize) {
    // SAFETY: `ctx` was made from `&F` by `run`, which does not return (so
    // the `F` stays alive) until this index has been counted off.
    let f = unsafe { &*(ctx as *const F) };
    f(k);
}

/// Run `f`, turning a panic into `false`. The payload is leaked, like
/// [`crate::graph::guarded`] does: freeing it would be a heap operation here.
#[inline]
fn catch(f: impl FnOnce()) -> bool {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(()) => true,
        Err(payload) => {
            std::mem::forget(payload);
            false
        }
    }
}

/// Take and run indices of job `seq` until there are none left.
fn work(sh: &Shared, seq: u32) {
    loop {
        let c = sh.claim.load(Ordering::Acquire);
        if seq_of(c) != seq || next_of(c) >= count_of(c) {
            return;
        }
        let ctx = sh.ctx.load(Ordering::Relaxed);
        let call = sh.call.load(Ordering::Relaxed);
        if sh.claim.compare_exchange_weak(c, c + 1, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            continue;
        }
        // SAFETY: `call` is the address of a `Call` stored by `run` for this
        // very job (see the module docs: the successful CAS proves the job
        // is still running, so `call` / `ctx` are its own and alive).
        let ok = catch(|| unsafe {
            let call: Call = std::mem::transmute::<usize, Call>(call);
            call(ctx, next_of(c));
        });
        if !ok {
            sh.panicked.store(true, Ordering::Relaxed);
        }
        sh.remaining.fetch_sub(1, Ordering::AcqRel);
    }
}

fn worker_main(sh: &Shared, me: usize, kind: PoolKind) {
    // Before the first job: logging here is off the real-time path.
    if let PoolKind::Live(period) = kind
        && !crate::thread_priority::promote_worker_thread(period)
        && cfg!(any(windows, target_os = "macos"))
    {
        log::warn!("DSP worker {me} runs at normal priority (the real-time promotion failed, period {period:?})");
    }
    // Join the IO thread's workgroup for the worker's whole life (the guard
    // leaves it when this function returns, before the thread exits).
    let _member = sh.workgroup.as_ref().and_then(|wg| {
        let j = wg.join();
        match &j {
            Some(_) => {
                sh.joined.fetch_add(1, Ordering::AcqRel);
            }
            None => log::warn!("DSP worker {me} could not join the audio workgroup; it keeps its real-time policy"),
        }
        j
    });
    crate::rt::mark_in_audio();
    aitrack_dsp::mark_audio_thread();
    #[cfg(test)]
    crate::alloc_counter::mark_worker(sh as *const Shared as usize);
    // Jobs are numbered from 1: a worker that starts late still joins the
    // first one if it is running (and finds nothing to take if it is done).
    let mut seen = 0u32;
    loop {
        // Wait for a job we have not taken part in.
        let mut spins = 0u32;
        let mut since: Option<Instant> = None;
        loop {
            if sh.shutdown.load(Ordering::Acquire) {
                return;
            }
            let c = sh.claim.load(Ordering::Acquire);
            if seq_of(c) != seen {
                seen = seq_of(c);
                break;
            }
            spins = spins.wrapping_add(1);
            if !spins.is_multiple_of(64) {
                std::hint::spin_loop();
                continue;
            }
            let now = Instant::now();
            if now.duration_since(*since.get_or_insert(now)) < sh.spin {
                continue;
            }
            // Park (see the module docs for why no wake-up is lost).
            sh.parked[me].store(true, Ordering::SeqCst);
            if seq_of(sh.claim.load(Ordering::SeqCst)) == seen && !sh.shutdown.load(Ordering::SeqCst) {
                std::thread::park();
            }
            sh.parked[me].store(false, Ordering::SeqCst);
            spins = 0;
            since = None;
        }
        // Plugins may have changed the FP mode; the audio thread sets it per block.
        aitrack_dsp::disable_denormals();
        work(sh, seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn every_index_runs_exactly_once() {
        for threads in [1, 2, 3, 4, 8] {
            let mut pool = WorkerPool::new(threads, PoolKind::Offline);
            assert_eq!(pool.threads(), threads);
            let hits: Vec<AtomicU32> = (0..97).map(|_| AtomicU32::new(0)).collect();
            for round in 0..200usize {
                let n = 1 + round % hits.len();
                assert!(pool.run(n, &|k| {
                    hits[k].fetch_add(1, Ordering::Relaxed);
                }));
            }
            let total: u32 = hits.iter().map(|h| h.load(Ordering::Relaxed)).sum();
            assert_eq!(total as usize, (0..200usize).map(|r| 1 + r % 97).sum::<usize>(), "{threads} threads");
        }
    }

    #[test]
    fn the_work_is_shared_and_its_writes_are_visible() {
        let mut pool = WorkerPool::new(4, PoolKind::Offline);
        // Plain (non-atomic) slots written by whoever runs the index and read
        // by the caller after `run`: the countdown publishes them.
        struct Slots(Vec<std::cell::UnsafeCell<usize>>);
        // SAFETY: each index is written by exactly one thread, read after `run`.
        unsafe impl Sync for Slots {}
        impl Slots {
            fn at(&self, k: usize) -> *mut usize {
                self.0[k].get()
            }
        }
        let slots = Slots((0..64).map(|_| std::cell::UnsafeCell::new(0)).collect());
        let names = std::sync::Mutex::new(std::collections::HashSet::new());
        for round in 1..=20usize {
            assert!(pool.run(slots.0.len(), &|k| {
                // Long enough that one thread cannot take them all.
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_micros(100) {
                    std::hint::spin_loop();
                }
                // SAFETY: index `k` is this call's alone.
                unsafe { *slots.at(k) = round * 1000 + k };
                names.lock().unwrap().insert(std::thread::current().id());
            }));
            for (k, v) in slots.0.iter().enumerate() {
                // SAFETY: `run` returned: no one writes any more.
                assert_eq!(unsafe { *v.get() }, round * 1000 + k);
            }
        }
        assert!(names.lock().unwrap().len() >= 2, "more than one thread took part");
    }

    #[test]
    fn a_panicking_job_is_reported_and_the_pool_keeps_working() {
        let mut pool = WorkerPool::new(4, PoolKind::Offline);
        let done = AtomicU32::new(0);
        let ok = pool.run(16, &|k| {
            if k == 5 || k == 11 {
                panic!("job {k} falls");
            }
            done.fetch_add(1, Ordering::Relaxed);
        });
        assert!(!ok);
        assert_eq!(done.load(Ordering::Relaxed), 14, "the others ran");
        assert!(pool.run(16, &|_| {
            done.fetch_add(1, Ordering::Relaxed);
        }));
        assert_eq!(done.load(Ordering::Relaxed), 30);
    }

    #[test]
    fn idle_workers_park_and_wake_for_the_next_job() {
        let mut pool = WorkerPool::new(3, PoolKind::Live(Duration::from_millis(5)));
        let n = AtomicU32::new(0);
        assert!(pool.run(8, &|_| {
            n.fetch_add(1, Ordering::Relaxed);
        }));
        // Long past the spin: every worker is parked.
        let t0 = Instant::now();
        while !pool.shared.parked.iter().all(|p| p.load(Ordering::SeqCst)) {
            assert!(t0.elapsed() < Duration::from_secs(5), "workers never parked");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(pool.run(8, &|_| {
            n.fetch_add(1, Ordering::Relaxed);
        }));
        assert_eq!(n.load(Ordering::Relaxed), 16);
        drop(pool);
    }

    #[test]
    fn auto_leaves_a_core_and_stays_in_range() {
        let a = auto_threads();
        assert!((1..=8).contains(&a));
        assert_eq!(resolve_threads(0), a);
        assert_eq!(resolve_threads(3), 3);
        assert_eq!(resolve_threads(10_000), MAX_THREADS);
    }

    #[test]
    fn claims_pack_and_unpack() {
        let c = pack(7, 300, 299);
        assert_eq!((seq_of(c), count_of(c), next_of(c)), (7, 300, 299));
        assert_eq!(next_of(c + 1), 300);
        assert_eq!(seq_of(pack(u32::MAX, MAX_JOBS, MAX_JOBS)), u32::MAX);
    }
}
