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

#[test]
fn wavetable_swaps_tables_without_rt_heap_traffic() {
    use aitrack_dsp::Processor;
    use aitrack_stock::instrument::wavetable::{self as wt, WavetableSynth, modulation::LfoCurve, tablegen};
    let mut s = WavetableSynth::new();
    s.prepare(SR, BLOCK);
    let wave = s.info().params.iter().find(|i| i.id == wt::p::osc(0, wt::p::WAVE)).unwrap().to_normalized(wt::USER_WAVE as f32);
    s.set_param(wt::p::osc(0, wt::p::WAVE), wave);
    let remote = s.remote();
    let mut d = Driver::new();
    for i in 0..3 {
        // Control side: build and post (allocates, FFTs) …
        remote.post_user_table(0, Some(tablegen::factory_table(i)));
        remote.post_lfo_curve(1, LfoCurve::duck());
        // … audio side: take it over, play it, retire the old one: no heap.
        let n = counted(|| {
            for _ in 0..8 {
                d.block(&mut s, 2);
            }
        });
        assert_eq!(n, 0, "swap {i}: {n} heap operations on the audio side");
        assert!(!remote.pending());
        // Control side frees what was retired.
        remote.collect();
    }
}

#[test]
fn vault_switches_engines_and_presets_without_rt_heap_traffic() {
    use aitrack_dsp::Processor;
    use aitrack_stock::instrument::vault::{self, Vault, params as vp};
    use aitrack_stock::presets;
    let mut s = Vault::new();
    s.prepare(SR, BLOCK);
    let engine = s.info().params.iter().find(|i| i.id == vp::ENGINE).unwrap().clone();
    // Factory presets, applied the way the engine applies parameter
    // commands: plain set_param calls between blocks.
    let loaded: Vec<presets::Preset> = presets::list(vault::ID).iter().map(|r| presets::load(&r.path).unwrap()).collect();
    let pairs: Vec<Vec<(u32, f32)>> = loaded.iter().map(|p| p.params.iter().map(|(k, v)| (k.parse().unwrap(), *v)).collect()).collect();
    let mut d = Driver::new();
    for round in 0..2 {
        for e in 0..vault::ENGINE_COUNT {
            let n = counted(|| {
                s.set_param(vp::ENGINE, engine.to_normalized(e as f32));
                for _ in 0..12 {
                    d.block(&mut s, 0);
                }
            });
            assert_eq!(n, 0, "round {round} engine {e}: {n} heap operations");
        }
        for (i, p) in pairs.iter().enumerate().step_by(5) {
            let n = counted(|| {
                for &(id, v) in p {
                    s.set_param(id, v);
                }
                for _ in 0..6 {
                    d.block(&mut s, 0);
                }
            });
            assert_eq!(n, 0, "preset {}: {n} heap operations", loaded[i].name);
        }
    }
}
