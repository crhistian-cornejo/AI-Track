//! CPU benchmark (ignored by default): prints the share of one core each
//! plugin needs at 48 kHz stereo, 128-frame blocks.
//!
//! `cargo test -p aitrack-stock --release --test bench -- --ignored --nocapture`

mod harness;

use harness::{BLOCK, Driver, SR};
use std::time::Instant;

#[test]
#[ignore]
fn cpu_per_plugin() {
    let seconds = 10.0;
    let blocks = (seconds * SR / BLOCK as f64) as usize;
    println!("\n{:<24} {:>8}  (48 kHz, {BLOCK} frames, {seconds} s of audio)", "plugin", "CPU %");
    for entry in aitrack_stock::registry() {
        let mut p = (entry.create)();
        p.prepare(SR, BLOCK);
        let mut d = Driver::new();
        for _ in 0..50 {
            d.block(p.as_mut(), 2);
        }
        let t = Instant::now();
        for _ in 0..blocks {
            d.block(p.as_mut(), 2);
        }
        let el = t.elapsed().as_secs_f64();
        println!("{:<24} {:>8.3}", entry.id, 100.0 * el / seconds);
    }
}
