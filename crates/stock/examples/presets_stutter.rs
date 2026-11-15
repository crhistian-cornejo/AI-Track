//! Generates the factory presets of fx.stutter.
//! `cargo run -p aitrack-stock --example presets_stutter`

use aitrack_stock::fx::stutter::{self, p};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

// Choice indices.
const ON: f32 = 1.0;
const GRID_MODE: f32 = 1.0;
// Interval: 1/32, 1/16, 1/8, 1/4, 1/2, 1 bar, 2 bars, 4 bars.
const EVERY_EIGHTH: f32 = 2.0;
const EVERY_QUARTER: f32 = 3.0;
const EVERY_HALF: f32 = 4.0;
const EVERY_BAR: f32 = 5.0;
const EVERY_2_BARS: f32 = 6.0;
const EVERY_4_BARS: f32 = 7.0;
// Gate: 1/32, 1/16, 1/8, 3/16, 1/4, 3/8, 1/2, 3/4, 1 bar, 1.5 bars, 2 bars, 3 bars, 4 bars.
const GATE_32: f32 = 0.0;
const GATE_16: f32 = 1.0;
const GATE_8: f32 = 2.0;
const GATE_3_16: f32 = 3.0;
const GATE_4: f32 = 4.0;
const GATE_2: f32 = 6.0;
const GATE_BAR: f32 = 8.0;
const GATE_2_BARS: f32 = 10.0;
const GATE_4_BARS: f32 = 12.0;
// Grid: 1/32, 1/16, 1/8, 1/4, 1/2, 1 bar.
const GRID_32: f32 = 0.0;
const GRID_16: f32 = 1.0;
const GRID_8: f32 = 2.0;
const GRID_4: f32 = 3.0;
const GRID_2: f32 = 4.0;
// Offset in sixteenths.
const BEAT_3: f32 = 12.0;
const BEAT_2: f32 = 8.0;
// Reverse: Forward, Reverse, Ping-Pong, Random. Mix mode: Insert, Mix, Duck, Gate.
const REVERSE: f32 = 1.0;
const PING_PONG: f32 = 2.0;
const RANDOM: f32 = 3.0;
const MIX_ADD: f32 = 1.0;
const DUCK: f32 = 2.0;
const GATE_MIX: f32 = 3.0;
// Ramp: Off, x2, x4, x8, x16. Stop time: 1/16, 1/8, 1/4, 1/2, 1 bar…
const RAMP_X8: f32 = 3.0;
const RAMP_X16: f32 = 4.0;
const STOP_QUARTER: f32 = 2.0;
const STOP_HALF: f32 = 3.0;
// Step divisions: 1/8, 1/16, 1/16T, 1/32, 1/32T.
const DIV_8: f32 = 0.0;
const DIV_16: f32 = 1.0;
const DIV_16T: f32 = 2.0;
const DIV_32: f32 = 3.0;
const DIV_32T: f32 = 4.0;

fn b(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(stutter::ID, cat, name)
}

/// Every step on, with the given probability, division and pitch walk.
fn grid_all(builder: PresetBuilder, prob: f32, div: impl Fn(u32) -> f32, pitch: impl Fn(u32) -> f32) -> PresetBuilder {
    (0..stutter::STEPS as u32).fold(builder.set(p::MODE, GRID_MODE), |bld, i| {
        bld.set(p::step(i, p::STEP_ON), ON).set(p::step(i, p::STEP_PROB), prob).set(p::step(i, p::STEP_DIV), div(i)).set(p::step(i, p::STEP_PITCH), pitch(i))
    })
}

fn presets() -> Vec<PresetBuilder> {
    vec![
        // ---- Stutter house: repeats of the last sixteenth, eighth or thirty-second.
        b("Stutter", "Stutter House 1-16 70")
            .tags(&["stutter", "house", "sixteenth", "drums", "vocal"])
            .set(p::INTERVAL, EVERY_QUARTER)
            .set(p::CHANCE, 70.0)
            .set(p::GATE, GATE_8)
            .set(p::GRID, GRID_16)
            .set(p::DECAY, -1.5)
            .set(p::SEED, 11.0),
        b("Stutter", "Stutter 1-8 Bar End")
            .tags(&["stutter", "house", "eighth", "fill", "bar end"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_3)
            .set(p::CHANCE, 100.0)
            .set(p::GATE, GATE_4)
            .set(p::GRID, GRID_8),
        b("Stutter", "Fast Stutter 1-32")
            .tags(&["stutter", "fast", "thirty-second", "techno", "glitch"])
            .set(p::INTERVAL, EVERY_QUARTER)
            .set(p::CHANCE, 60.0)
            .set(p::GATE, GATE_3_16)
            .set(p::GRID, GRID_32)
            .set(p::SEED, 4.0),
        b("Stutter", "Triplet Stutter")
            .tags(&["stutter", "triplet", "garage", "swing"])
            .set(p::INTERVAL, EVERY_QUARTER)
            .set(p::CHANCE, 75.0)
            .set(p::GATE, GATE_8)
            .set(p::GRID, GRID_16)
            .set(p::TRIPLETS, ON)
            .set(p::VARIATION, 35.0)
            .set(p::SEED, 21.0),
        // The 3-3-2 pattern of the grid, with some steps left to chance.
        {
            let mut bld = b("Stutter", "Offbeat Grid 3-3-2")
                .tags(&["stutter", "house", "grid", "syncopated", "pattern"])
                .set(p::MODE, GRID_MODE)
                .set(p::SEED, 8.0);
            for i in 0..stutter::STEPS as u32 {
                let strong = matches!(i, 0 | 6 | 12);
                bld = bld.set(p::step(i, p::STEP_PROB), if strong { 100.0 } else { 80.0 }).set(p::step(i, p::STEP_DIV), if i % 2 == 0 { DIV_16 } else { DIV_32 });
            }
            bld
        },
        grid_all(b("Stutter", "Trap Hat Repeat 1-32T").tags(&["trap", "hats", "triplet", "roll", "grid"]).set(p::SEED, 2.0).set(p::DECAY, -1.0), 45.0, |_| DIV_32T, |_| 0.0),
        // ---- Build-ups: the repeat gets shorter, and higher, towards the drop.
        b("Build-Up", "Snare Roll 1-32 Ramp")
            .tags(&["build-up", "snare", "roll", "riser", "ramp"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_3)
            .set(p::GATE, GATE_4)
            .set(p::GRID, GRID_8)
            .set(p::RAMP, RAMP_X8)
            .set(p::PITCH_DECAY, 0.6),
        b("Build-Up", "Build-Up 2 Bars")
            .tags(&["build-up", "riser", "edm", "ramp", "drop"])
            .set(p::INTERVAL, EVERY_2_BARS)
            .set(p::GATE, GATE_2_BARS)
            .set(p::GRID, GRID_2)
            .set(p::RAMP, RAMP_X16)
            .set(p::PITCH_DECAY, 0.25)
            .set(p::MIX_MODE, DUCK),
        b("Build-Up", "Build-Up 4 Bars")
            .tags(&["build-up", "riser", "festival", "ramp", "long"])
            .set(p::INTERVAL, EVERY_4_BARS)
            .set(p::GATE, GATE_4_BARS)
            .set(p::GRID, GRID_4)
            .set(p::RAMP, RAMP_X8)
            .set(p::PITCH_DECAY, 0.15)
            .set(p::MIX_MODE, DUCK)
            .set(p::FILTER_ON, ON)
            .set(p::FILTER_FREQ, 2_500.0)
            .set(p::FILTER_WIDTH, 85.0),
        grid_all(
            b("Build-Up", "Accelerating Grid").tags(&["build-up", "grid", "accelerate", "riser", "pitch"]).set(p::MIX_MODE, DUCK),
            100.0,
            |i| match i / 4 {
                0 => DIV_8,
                1 => DIV_16,
                2 => DIV_16T,
                _ => DIV_32,
            },
            |i| (i as f32 * 0.75).round(),
        ),
        // ---- Tape-stop at the end of a bar.
        b("Tape", "Tape-Stop Bar End")
            .tags(&["tape-stop", "break", "transition", "bar end"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_3)
            .set(p::CHANCE, 60.0)
            .set(p::GATE, GATE_4)
            .set(p::TAPE_STOP, ON)
            .set(p::STOP_TIME, STOP_QUARTER)
            .set(p::SEED, 1.0),
        b("Tape", "Tape-Stop Half Bar")
            .tags(&["tape-stop", "slow down", "long", "transition"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_2)
            .set(p::CHANCE, 100.0)
            .set(p::GATE, GATE_2)
            .set(p::TAPE_STOP, ON)
            .set(p::STOP_TIME, STOP_HALF),
        // ---- Half-time.
        b("Half-Time", "Half-Time Drop")
            .tags(&["half-time", "drop", "slow", "heavy"])
            .set(p::INTERVAL, EVERY_4_BARS)
            .set(p::GATE, GATE_BAR)
            .set(p::GRID, GRID_4)
            .set(p::HALF_TIME, ON),
        b("Half-Time", "Half-Time Fill")
            .tags(&["half-time", "fill", "bar end", "pitch down"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_2)
            .set(p::CHANCE, 70.0)
            .set(p::GATE, GATE_2)
            .set(p::GRID, GRID_8)
            .set(p::HALF_TIME, ON)
            .set(p::SEED, 6.0),
        // ---- Glitch.
        b("Glitch", "Reverse Snare")
            .tags(&["reverse", "snare", "swell", "transition"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_2)
            .set(p::CHANCE, 100.0)
            .set(p::GATE, GATE_8)
            .set(p::GRID, GRID_8)
            .set(p::REVERSE, REVERSE),
        b("Glitch", "Ping-Pong Nod")
            .tags(&["ping-pong", "reverse", "groove", "bounce"])
            .set(p::INTERVAL, EVERY_HALF)
            .set(p::CHANCE, 80.0)
            .set(p::GATE, GATE_4)
            .set(p::GRID, GRID_8)
            .set(p::REVERSE, PING_PONG)
            .set(p::SEED, 13.0),
        grid_all(b("Glitch", "Random Slicer").tags(&["glitch", "random", "slicer", "grid", "idm"]).set(p::REVERSE, RANDOM).set(p::VARIATION, 0.0).set(p::SEED, 33.0), 55.0, |i| (i % 5) as f32, |i| [0.0, -5.0, 7.0, 0.0, 12.0][(i % 5) as usize]),
        // ---- Voice.
        b("Vocal", "Vocal Gate Chop")
            .tags(&["vocal", "gate", "chop", "rhythmic"])
            .set(p::INTERVAL, EVERY_EIGHTH)
            .set(p::CHANCE, 85.0)
            .set(p::GATE, GATE_16)
            .set(p::GRID, GRID_32)
            .set(p::MIX_MODE, GATE_MIX)
            .set(p::SEED, 9.0),
        b("Vocal", "Vocal Stutter Pitch-Down")
            .tags(&["vocal", "stutter", "pitch down", "dark"])
            .set(p::INTERVAL, EVERY_QUARTER)
            .set(p::CHANCE, 50.0)
            .set(p::GATE, GATE_8)
            .set(p::GRID, GRID_16)
            .set(p::PITCH_DECAY, -2.0)
            .set(p::DECAY, -2.0)
            .set(p::SEED, 17.0),
        b("Vocal", "Rhythmic Gate 1-16")
            .tags(&["gate", "trance", "rhythmic", "pad", "chop"])
            .set(p::INTERVAL, 1.0)
            .set(p::CHANCE, 70.0)
            .set(p::GATE, GATE_32)
            .set(p::GRID, GRID_32)
            .set(p::MIX_MODE, GATE_MIX)
            .set(p::SEED, 5.0),
        // ---- Round two: swing, per-step level / stop / filter, ties and From-To ramps.
        {
            // 3-3-2 with a swung 16th: the held steps make the 3-3-2 stabs long.
            let mut bld = b("Stutter", "Stutter House 3-3-2 Swing")
                .tags(&["stutter", "house", "swing", "3-3-2", "grid", "tie"])
                .set(p::MODE, GRID_MODE)
                .set(p::SWING, 33.0)
                .set(p::SEED, 12.0);
            for i in 0..stutter::STEPS as u32 {
                let head = matches!(i, 0 | 3 | 6 | 8 | 11 | 14);
                bld = bld.set(p::step(i, p::STEP_ON), if head { 1.0 } else { 0.0 }).set(p::step(i, p::STEP_PROB), if i == 0 { 100.0 } else { 85.0 }).set(p::step(i, p::STEP_DIV), DIV_16);
                bld = bld.set(p::tie(i), if matches!(i, 1 | 4 | 9 | 12) { 1.0 } else { 0.0 });
            }
            bld
        },
        b("Build-Up", "Snare Roll Level Ramp")
            .tags(&["build-up", "snare", "roll", "level", "ramp", "riser"])
            .set(p::INTERVAL, EVERY_BAR)
            .set(p::OFFSET, BEAT_3)
            .set(p::GATE, GATE_4)
            .set(p::GRID, GRID_8)
            .set(p::RAMP, RAMP_X8)
            .set(p::LEVEL, -24.0)
            .set(p::LEVEL_END, 24.0)
            .set(p::PITCH_END, 5.0),
        {
            // A volume gate on the grid (steps at -inf are cuts) with a tape-stop on the last held steps of the bar.
            let mut bld = b("Vocal", "Rhythmic Gate Bar-End Stop")
                .tags(&["gate", "rhythmic", "tape-stop", "bar end", "level", "vocal"])
                .set(p::MODE, GRID_MODE)
                .set(p::STOP_TIME, STOP_QUARTER)
                .set(p::SEED, 3.0);
            for i in 0..stutter::STEPS as u32 {
                let cut = i % 4 == 2 && i < 12;
                bld = bld.set(p::step(i, p::STEP_ON), 1.0).set(p::step(i, p::STEP_DIV), DIV_32).set(p::step(i, p::STEP_LEVEL), if cut { -60.0 } else { 0.0 });
                bld = bld.set(p::step(i, p::STEP_STOP), if i == 12 { 1.0 } else { 0.0 }).set(p::tie(i), if (13..=15).contains(&i) { 1.0 } else { 0.0 });
                bld = bld.set(p::step(i, p::STEP_PROB), if i >= 12 { 70.0 } else { 100.0 });
            }
            bld
        },
        b("Build-Up", "Pitch Rise Roll")
            .tags(&["build-up", "pitch", "riser", "roll", "ramp"])
            .set(p::INTERVAL, EVERY_2_BARS)
            .set(p::GATE, GATE_BAR)
            .set(p::GRID, GRID_8)
            .set(p::RAMP, RAMP_X8)
            .set(p::PITCH_END, 12.0)
            .set(p::LEVEL, -9.0)
            .set(p::LEVEL_END, 9.0)
            .set(p::MIX_MODE, DUCK),
        b("Glitch", "Filter Sweep Stutter")
            .tags(&["glitch", "filter", "sweep", "stutter", "ramp"])
            .set(p::INTERVAL, EVERY_HALF)
            .set(p::CHANCE, 80.0)
            .set(p::GATE, GATE_4)
            .set(p::GRID, GRID_16)
            .set(p::FILTER_ON, ON)
            .set(p::FILTER_FREQ, 500.0)
            .set(p::FILTER_WIDTH, 60.0)
            .set(p::FILTER_END, 3.0)
            .set(p::SEED, 15.0),
        {
            let mut bld = grid_all(b("Stutter", "Swung Hats 1-16").tags(&["swing", "hats", "trap", "house", "groove"]).set(p::SWING, 50.0).set(p::SEED, 19.0).set(p::DECAY, -2.0), 60.0, |_| DIV_32, |_| 0.0);
            for i in 0..stutter::STEPS as u32 {
                // Accent the downbeats, soften the rest.
                bld = bld.set(p::step(i, p::STEP_LEVEL), if i % 4 == 0 { 0.0 } else { -6.0 });
            }
            bld
        },
        {
            let mut bld = b("Glitch", "Filtered Steps")
                .tags(&["grid", "filter", "glitch", "pattern", "per-step"])
                .set(p::MODE, GRID_MODE)
                .set(p::FILTER_FREQ, 2_200.0)
                .set(p::FILTER_WIDTH, 70.0)
                .set(p::SEED, 23.0);
            for i in 0..stutter::STEPS as u32 {
                bld = bld.set(p::step(i, p::STEP_PROB), 75.0).set(p::step(i, p::STEP_DIV), if i % 3 == 0 { DIV_16 } else { DIV_32 }).set(p::step(i, p::STEP_FILTER), if i % 2 == 1 { 1.0 } else { 0.0 });
            }
            bld
        },
        b("Glitch", "Stutter Layer")
            .tags(&["stutter", "layer", "mix", "parallel", "subtle"])
            .set(p::INTERVAL, EVERY_HALF)
            .set(p::CHANCE, 65.0)
            .set(p::GATE, GATE_8)
            .set(p::GRID, GRID_16)
            .set(p::MIX_MODE, MIX_ADD)
            .set(p::DECAY, -4.0)
            .set(p::SEED, 27.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    for pr in presets() {
        println!("{}", pr.write(&root)?.display());
    }
    Ok(())
}
