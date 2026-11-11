//! No NaN/inf (and no runaway levels) for silence, impulses, noise, DC and
//! very loud input under default, minimum, maximum and scrambled params.

mod harness;

use harness::{BLOCK, Driver, SR};

#[test]
fn outputs_stay_finite() {
    let mut failures = Vec::new();
    for entry in aitrack_stock::registry() {
        let ids: Vec<u32> = {
            let p = (entry.create)();
            p.info().params.iter().map(|i| i.id).collect()
        };
        for pass in 0..6 {
            let mut p = (entry.create)();
            p.prepare(SR, BLOCK);
            for (j, &id) in ids.iter().enumerate() {
                let v = match pass {
                    0 => continue,
                    1 => 0.0,
                    2 => 1.0,
                    3 => ((j * 7919 + 13) % 101) as f32 / 100.0,
                    4 => ((j * 104_729 + 7) % 97) as f32 / 96.0,
                    _ => {
                        if j % 2 == 0 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                };
                p.set_param(id, v);
            }
            let mut d = Driver::new();
            for signal in [0usize, 1, 2, 3, 4, 5] {
                // ~0.5 s per signal.
                for b in 0..190 {
                    d.block(p.as_mut(), signal);
                    // Loud input may come out loud; it must never explode.
                    let limit = if signal == 4 { 1e5 } else { 1e3 };
                    if let Err(e) = d.output_ok(limit) {
                        failures.push(format!("{} pass {pass} signal {signal} block {b}: {e}", entry.id));
                        break;
                    }
                }
            }
            // Silence after abuse must decay to silence for effects (no
            // self-oscillation runaway), and still be finite for all.
            for _ in 0..400 {
                d.block(p.as_mut(), 0);
            }
            if let Err(e) = d.output_ok(1e3) {
                failures.push(format!("{} pass {pass} after: {e}", entry.id));
            }
            // State round trip must not panic.
            let state = p.save_state();
            p.load_state(&state);
            p.load_state(b"not json");
            p.reset();
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn latency_and_tail_are_sane() {
    for entry in aitrack_stock::registry() {
        let mut p = (entry.create)();
        p.prepare(SR, BLOCK);
        assert!(p.latency() < 48_000, "{} latency {}", entry.id, p.latency());
        assert!(p.tail() <= 48_000 * 120, "{} tail {}", entry.id, p.tail());
        for info in &p.info().params {
            assert!(info.min < info.max || info.min > info.max, "{}: {} has an empty range", entry.id, info.name);
            let d = info.default;
            assert!(d >= info.min.min(info.max) && d <= info.max.max(info.min), "{}: {} default out of range", entry.id, info.name);
            if !info.choices.is_empty() {
                assert_eq!(info.choices.len() as f32, (info.max - info.min).round() + 1.0, "{}: {} choices", entry.id, info.name);
            }
            if info.scale == aitrack_dsp::ParamScale::Log {
                assert!(info.min > 0.0, "{}: {} log with min <= 0", entry.id, info.name);
            }
        }
    }
}
