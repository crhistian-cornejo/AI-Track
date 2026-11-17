//! What the window's meters and status bar read from a device.
//!
//! A stock plugin publishes its display data through `Processor::meter()` in
//! a layout of its own. The plugins that publish their input and output
//! peaks, their gain reduction, their latency or a "working too hard" flag
//! are listed in [`map`]; everything else shows the level of the track it
//! sits on. The numbers here are plain `f32` so the table can be tested
//! against the real plugins.

use std::time::{Duration, Instant};

/// How a published level is expressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// dBFS.
    Db,
    /// Linear peak, 1 = 0 dBFS.
    Linear,
}

impl Unit {
    pub fn to_db(self, v: f32) -> f32 {
        match self {
            Unit::Db => v,
            Unit::Linear => linear_to_db(v),
        }
    }
}

/// Which way a gain reduction slot counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    /// Reduction is negative dB (0 = none).
    Negative,
    /// Reduction is positive dB.
    Positive,
}

/// Where in a plugin's meter its in/out peaks and friends sit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Io {
    pub input: Option<(usize, Unit)>,
    pub output: Option<(usize, Unit)>,
    /// First slot, how many (the largest reduction counts) and their sign.
    pub reduction: Option<(usize, usize, Sign)>,
    /// Latency in samples.
    pub latency: Option<usize>,
    /// A slot that is above 0.001 while the device is working too hard, and
    /// what to tell the user then.
    pub warn: Option<(usize, &'static str)>,
}

/// The meter layout of `plugin`, when it publishes anything the frame uses.
pub fn map(plugin: &str) -> Option<Io> {
    use Sign::{Negative, Positive};
    use Unit::{Db, Linear};
    let dyn_io = |warn| Io {
        input: Some((1, Db)),
        output: Some((2, Db)),
        reduction: Some((0, 1, Negative)),
        latency: Some(67),
        warn,
    };
    Some(match plugin {
        "fx.compressor" => dyn_io(None),
        "fx.buscomp" => dyn_io(Some((68, "Soft clip working hard"))),
        "fx.clipper" => Io { input: Some((1, Db)), output: Some((2, Db)), reduction: Some((0, 1, Negative)), ..Io::default() },
        "fx.limiter" => Io { input: Some((2, Db)), output: Some((1, Db)), reduction: Some((0, 1, Negative)), ..Io::default() },
        "fx.mbdyn" => Io {
            input: Some((9, Db)),
            output: Some((10, Db)),
            reduction: Some((3, 3, Negative)),
            latency: Some(11),
            warn: Some((14, "Output bound engaged")),
        },
        "fx.drumbus" => Io { input: Some((1, Db)), output: Some((2, Db)), reduction: Some((0, 1, Negative)), latency: Some(3), warn: None },
        "fx.gate" => Io { reduction: Some((0, 1, Negative)), ..Io::default() },
        "fx.saturator" | "fx.tape" | "fx.amp" => Io { input: Some((0, Linear)), output: Some((1, Linear)), ..Io::default() },
        "fx.console" => Io { input: Some((0, Linear)), output: Some((1, Linear)), reduction: Some((2, 1, Positive)), ..Io::default() },
        "fx.eq" => Io { latency: Some(265), ..Io::default() },
        _ => return None,
    })
}

/// The largest slot `io` reads (the meter must be longer than this).
#[cfg(test)]
fn deepest(io: &Io) -> usize {
    [
        io.input.map(|v| v.0),
        io.output.map(|v| v.0),
        io.reduction.map(|r| r.0 + r.1 - 1),
        io.latency,
        io.warn.map(|w| w.0),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0)
}

/// What a device publishes right now, in dB where it is a level.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    pub input_db: Option<f32>,
    pub output_db: Option<f32>,
    /// Gain reduction as a positive number of dB.
    pub reduction_db: Option<f32>,
    pub latency: Option<u32>,
    pub warn: Option<&'static str>,
}

/// Read `io` out of a meter the plugin has filled (`meter` is empty until
/// the engine publishes it; slots past its end read as nothing).
pub fn read(io: &Io, meter: &[f32]) -> Reading {
    let at = |i: usize| meter.get(i).copied().filter(|v| v.is_finite());
    let level = |slot: Option<(usize, Unit)>| slot.and_then(|(i, u)| at(i).map(|v| u.to_db(v)));
    let reduction_db = io.reduction.and_then(|(first, n, sign)| {
        (first..first + n).filter_map(at).map(|v| if sign == Sign::Negative { -v } else { v }).reduce(f32::max).map(|v| v.max(0.))
    });
    Reading {
        input_db: level(io.input),
        output_db: level(io.output),
        reduction_db,
        latency: io.latency.and_then(at).filter(|v| *v > 0.5).map(|v| v.round() as u32),
        warn: io.warn.and_then(|(i, text)| at(i).filter(|v| *v > 0.001).map(|_| text)),
    }
}

pub fn linear_to_db(v: f32) -> f32 {
    if v <= 1e-6 { -120. } else { 20. * v.log10() }
}

// ───────────────────────────── Meter scale ─────────────────────────────

/// The bottom and top of a level meter, dBFS.
pub const FLOOR_DB: f32 = -60.;
pub const CEIL_DB: f32 = 6.;
/// The deepest gain reduction the reduction meter shows, dB.
pub const REDUCTION_SPAN_DB: f32 = 24.;

/// Where a level sits on the meter, 0 (floor) to 1 (top).
pub fn level_frac(db: f32) -> f32 {
    ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0., 1.)
}

/// How far down a reduction goes on its meter, 0 to 1.
pub fn reduction_frac(db: f32) -> f32 {
    (db / REDUCTION_SPAN_DB).clamp(0., 1.)
}

/// Samples as a time: `2.7 ms`.
pub fn latency_text(samples: u32, sample_rate: f64) -> String {
    let ms = f64::from(samples) * 1000. / sample_rate.max(1.);
    format!("{samples} smp · {ms:.1} ms")
}

// ───────────────────────────── Peak hold ─────────────────────────────

/// A peak tick that waits a moment and then falls.
#[derive(Clone, Copy, Debug)]
pub struct Hold {
    pub db: f32,
    until: Instant,
    last: Instant,
}

/// How long a peak stays put, and how fast it falls afterwards (dB/s).
const HOLD_FOR: Duration = Duration::from_millis(900);
const FALL_DB_PER_S: f32 = 20.;

impl Hold {
    pub fn new(now: Instant) -> Self {
        Self { db: -120., until: now, last: now }
    }

    /// Feed the level now; returns the held peak.
    pub fn update(&mut self, level_db: f32, now: Instant) -> f32 {
        let dt = now.saturating_duration_since(self.last).as_secs_f32().min(0.25);
        self.last = now;
        if level_db >= self.db {
            self.db = level_db;
            self.until = now + HOLD_FOR;
        } else if now > self.until {
            self.db = (self.db - FALL_DB_PER_S * dt).max(level_db);
        }
        self.db
    }

    /// Whether the tick is still moving or waiting (the window keeps asking for frames).
    pub fn active(&self) -> bool {
        self.db > FLOOR_DB
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::{create, registry};

    #[test]
    fn levels_are_read_in_db_whatever_the_plugin_publishes() {
        let io = map("fx.compressor").unwrap();
        let mut meter = vec![0.; 70];
        meter[0] = -7.;
        meter[1] = -20.;
        meter[2] = -9.;
        meter[67] = 96.;
        let r = read(&io, &meter);
        assert_eq!((r.input_db, r.output_db, r.reduction_db, r.latency), (Some(-20.), Some(-9.), Some(7.), Some(96)));
        // Linear peaks (the saturators) become dB.
        let sat = map("fx.saturator").unwrap();
        let r = read(&sat, &[0.5, 0.1, 0., 0., 0.]);
        assert!((r.input_db.unwrap() + 6.02).abs() < 0.01 && (r.output_db.unwrap() + 20.).abs() < 0.01);
        assert_eq!(read(&sat, &[0., 0.]).input_db, Some(-120.));
        // A meter the engine has not published yet reads as nothing.
        assert_eq!(read(&io, &[]), Reading::default());
        // The multiband dynamics reduces by its deepest band.
        let mb = read(&map("fx.mbdyn").unwrap(), &[0., 0., 0., -2., -9., 4., 0., 0., 0., -12., -10., 0., 0., 0., 0.5]);
        assert_eq!(mb.reduction_db, Some(9.));
        assert_eq!(mb.warn, Some("Output bound engaged"));
        assert_eq!(read(&map("fx.limiter").unwrap(), &[-3., -1., -5.]).input_db, Some(-5.));
        // Lifting is not reduction.
        let lift = read(&map("fx.mbdyn").unwrap(), &[0., 0., 0., 3., 2., 1., 0., 0., 0., 0., 0.]);
        assert_eq!(lift.reduction_db, Some(0.));
    }

    #[test]
    fn the_table_matches_what_the_real_plugins_publish() {
        for entry in registry() {
            let Some(io) = map(entry.id) else { continue };
            let plugin = create(entry.id).unwrap();
            let mut meter = vec![0.0f32; 320];
            let n = plugin.meter(&mut meter);
            assert!(n > deepest(&io), "{}: meter has {n} values, the frame reads slot {}", entry.id, deepest(&io));
        }
    }

    #[test]
    fn a_published_latency_is_what_the_plugin_reports() {
        // Linear-phase multiband dynamics is the one that always has some.
        let mut plugin = create("fx.mbdyn").unwrap();
        plugin.prepare(48_000., 512);
        let latency = plugin.latency();
        let mut meter = vec![0.0f32; 320];
        let n = plugin.meter(&mut meter);
        let r = read(&map("fx.mbdyn").unwrap(), &meter[..n]);
        assert_eq!(r.latency.unwrap_or(0), latency);
    }

    #[test]
    fn the_scales_put_zero_near_the_top_and_clamp() {
        assert_eq!(level_frac(-200.), 0.);
        assert_eq!(level_frac(FLOOR_DB), 0.);
        assert_eq!(level_frac(CEIL_DB), 1.);
        assert_eq!(level_frac(60.), 1.);
        let zero = level_frac(0.);
        assert!(zero > 0.85 && zero < 0.95, "{zero}");
        assert_eq!(reduction_frac(-3.), 0.);
        assert_eq!(reduction_frac(12.), 0.5);
        assert_eq!(reduction_frac(100.), 1.);
        assert_eq!(latency_text(96, 48_000.), "96 smp · 2.0 ms");
    }

    #[test]
    fn a_peak_holds_then_falls_and_a_new_peak_takes_over() {
        let t0 = Instant::now();
        let mut h = Hold::new(t0);
        assert!(!h.active());
        assert_eq!(h.update(-6., t0), -6.);
        // Held for a moment.
        assert_eq!(h.update(-30., t0 + Duration::from_millis(500)), -6.);
        // Then it falls, never below the live level.
        let later = h.update(-30., t0 + Duration::from_millis(1400));
        assert!((-30. ..-6.).contains(&later), "{later}");
        let much_later = h.update(-30., t0 + Duration::from_secs(20));
        assert!(much_later >= -30.);
        assert_eq!(h.update(-2., t0 + Duration::from_secs(21)), -2.);
    }
}
