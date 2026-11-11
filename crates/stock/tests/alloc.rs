//! Every registry plugin must process blocks without touching the heap.

mod harness;

use harness::{BLOCK, Driver, SR};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn counted(f: impl FnOnce()) -> usize {
    let before = COUNT.load(Ordering::SeqCst);
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.load(Ordering::SeqCst) - before
}

#[test]
fn process_never_allocates() {
    let mut failures = Vec::new();
    for entry in aitrack_stock::registry() {
        let mut p = (entry.create)();
        p.prepare(SR, BLOCK);
        let mut d = Driver::new();
        // Defaults, then every parameter at 0, at 1 and random-ish values;
        // parameter changes happen on the control side, the processing of
        // what they imply (coefficients, tables) on the audio side.
        let ids: Vec<u32> = p.info().params.iter().map(|i| i.id).collect();
        for pass in 0..4 {
            for (j, &id) in ids.iter().enumerate() {
                let v = match pass {
                    0 => p.info().params[j].default_normalized(),
                    1 => 0.0,
                    2 => 1.0,
                    _ => ((j * 7919 + 13) % 101) as f32 / 100.0,
                };
                p.set_param(id, v);
            }
            for signal in [0usize, 1, 2, 5] {
                let n = counted(|| {
                    for _ in 0..24 {
                        d.block(p.as_mut(), signal);
                    }
                });
                if n > 0 {
                    failures.push(format!("{} pass {pass} signal {signal}: {n} allocations", entry.id));
                }
            }
        }
        // meter() is also called from the audio side.
        let mut meter = vec![0.0f32; 4096];
        let n = counted(|| {
            p.meter(&mut meter);
        });
        if n > 0 {
            failures.push(format!("{} meter(): {n} allocations", entry.id));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn counter_detects_allocations() {
    let n = counted(|| {
        let v: Vec<u8> = Vec::with_capacity(64);
        std::hint::black_box(&v);
    });
    assert!(n >= 1);
}
