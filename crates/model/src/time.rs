//! Musical time. Positions are [`Tick`]s at [`PPQ`] ticks per quarter note;
//! the [`TempoMap`] converts ticks to seconds and samples.

use serde::{Deserialize, Serialize};

/// Ticks per quarter note. 960 divides evenly by 2, 3, 4, 5, 6, 8, 10, 12,
/// 15, 16, 20, 24, 32, 40, 48, 60, 64 so triplets and quintuplets stay exact.
pub const PPQ: i64 = 960;

/// A musical position or length in ticks.
pub type Tick = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self { numerator: 4, denominator: 4 }
    }
}

impl TimeSignature {
    /// Length of one bar in ticks.
    pub fn bar_ticks(self) -> Tick {
        PPQ * 4 * self.numerator as i64 / self.denominator.max(1) as i64
    }
}

/// One tempo segment: from `start` the tempo is `bpm`, and if `ramp` is set
/// it moves linearly (in BPM over ticks) to the next segment's tempo.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoPoint {
    pub start: Tick,
    pub bpm: f64,
    #[serde(default)]
    pub ramp: bool,
}

/// Tempo and meter over time. Always has at least one point at tick 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoMap {
    points: Vec<TempoPoint>,
    pub signatures: Vec<(Tick, TimeSignature)>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

impl TempoMap {
    pub fn constant(bpm: f64) -> Self {
        Self {
            points: vec![TempoPoint { start: 0, bpm: clamp_bpm(bpm), ramp: false }],
            signatures: vec![(0, TimeSignature::default())],
        }
    }

    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// Tempo at the very start of the song.
    pub fn initial_bpm(&self) -> f64 {
        self.points[0].bpm
    }

    /// Replace the tempo at tick 0 (the common "set BPM" action).
    pub fn set_initial_bpm(&mut self, bpm: f64) {
        self.points[0].bpm = clamp_bpm(bpm);
    }

    /// Insert or replace a tempo point, keeping the list sorted.
    pub fn set_point(&mut self, point: TempoPoint) {
        let point = TempoPoint { bpm: clamp_bpm(point.bpm), ..point };
        match self.points.binary_search_by_key(&point.start, |p| p.start) {
            Ok(i) => self.points[i] = point,
            Err(i) => self.points.insert(i, point),
        }
    }

    pub fn remove_point(&mut self, start: Tick) {
        if start != 0 {
            self.points.retain(|p| p.start != start);
        }
    }

    pub fn bpm_at(&self, tick: Tick) -> f64 {
        let i = self.segment_index(tick);
        let p = self.points[i];
        match (p.ramp, self.points.get(i + 1)) {
            (true, Some(next)) => {
                let t = (tick - p.start) as f64 / (next.start - p.start).max(1) as f64;
                p.bpm + (next.bpm - p.bpm) * t.clamp(0.0, 1.0)
            }
            _ => p.bpm,
        }
    }

    pub fn signature_at(&self, tick: Tick) -> TimeSignature {
        self.signatures
            .iter()
            .rev()
            .find(|(start, _)| *start <= tick)
            .map(|(_, s)| *s)
            .unwrap_or_default()
    }

    /// Seconds from tick 0 to `tick`. Ramps are integrated exactly
    /// (tempo linear in ticks gives a logarithmic time integral).
    pub fn tick_to_seconds(&self, tick: Tick) -> f64 {
        let mut seconds = 0.0;
        for (i, p) in self.points.iter().enumerate() {
            if tick <= p.start {
                break;
            }
            let end = self.points.get(i + 1).map_or(tick, |n| n.start.min(tick));
            let next_bpm = self.points.get(i + 1).map(|n| n.bpm);
            seconds += segment_seconds(p, next_bpm, self.points.get(i + 1).map(|n| n.start), end);
        }
        if tick < 0 {
            seconds = tick as f64 / PPQ as f64 * 60.0 / self.points[0].bpm;
        }
        seconds
    }

    /// Inverse of [`tick_to_seconds`](Self::tick_to_seconds), as a fractional tick.
    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        if seconds <= 0.0 {
            return seconds * self.points[0].bpm / 60.0 * PPQ as f64;
        }
        let mut elapsed = 0.0;
        for (i, p) in self.points.iter().enumerate() {
            let next = self.points.get(i + 1);
            let seg_len = next.map(|n| segment_seconds(p, Some(n.bpm), Some(n.start), n.start));
            match seg_len {
                Some(len) if elapsed + len < seconds => elapsed += len,
                _ => {
                    // Solve inside this segment.
                    let remaining = seconds - elapsed;
                    return match (p.ramp, next) {
                        (true, Some(n)) if (n.bpm - p.bpm).abs() > 1e-9 => {
                            let span = (n.start - p.start) as f64;
                            let k = (n.bpm - p.bpm) / span; // bpm per tick
                            // t = (60/PPQ) * ln(bpm(x)/bpm0)/k  →  x = (bpm0*e^{k*t*PPQ/60} - bpm0)/k
                            let x = p.bpm * ((k * remaining * PPQ as f64 / 60.0).exp() - 1.0) / k;
                            p.start as f64 + x
                        }
                        _ => p.start as f64 + remaining * p.bpm / 60.0 * PPQ as f64,
                    };
                }
            }
        }
        unreachable!("the last segment is unbounded")
    }

    pub fn tick_to_samples(&self, tick: Tick, sample_rate: f64) -> f64 {
        self.tick_to_seconds(tick) * sample_rate
    }

    pub fn samples_to_tick(&self, samples: f64, sample_rate: f64) -> f64 {
        self.seconds_to_tick(samples / sample_rate)
    }

    /// `(bar, beat, tick-in-beat)`, all 1-based except the tick, for display.
    pub fn bars_beats(&self, tick: Tick) -> (i64, i64, i64) {
        let mut bar = 1;
        let mut cursor = 0;
        let sigs = &self.signatures;
        for (i, (start, sig)) in sigs.iter().enumerate() {
            let end = sigs.get(i + 1).map_or(Tick::MAX, |n| n.0);
            if tick < end || i + 1 == sigs.len() {
                let into = tick - start.max(&cursor);
                let bar_len = sig.bar_ticks();
                let beat_len = bar_len / sig.numerator.max(1) as i64;
                bar += into.div_euclid(bar_len);
                let in_bar = into.rem_euclid(bar_len);
                return (bar, in_bar / beat_len + 1, in_bar % beat_len);
            }
            bar += (end - start) / sig.bar_ticks();
            cursor = end;
        }
        (bar, 1, 0)
    }

    fn segment_index(&self, tick: Tick) -> usize {
        match self.points.binary_search_by_key(&tick, |p| p.start) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }
}

fn clamp_bpm(bpm: f64) -> f64 {
    if bpm.is_finite() { bpm.clamp(10.0, 999.0) } else { 120.0 }
}

/// Seconds spent in segment `p` from its start up to `end`.
fn segment_seconds(p: &TempoPoint, next_bpm: Option<f64>, next_start: Option<Tick>, end: Tick) -> f64 {
    let ticks = (end - p.start) as f64;
    let quarter = 60.0 / PPQ as f64;
    match (p.ramp, next_bpm, next_start) {
        (true, Some(nb), Some(ns)) if (nb - p.bpm).abs() > 1e-9 => {
            let k = (nb - p.bpm) / (ns - p.start) as f64;
            let bpm_end = p.bpm + k * ticks;
            quarter * (bpm_end / p.bpm).ln() / k
        }
        _ => ticks * quarter / p.bpm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_tempo_round_trips() {
        let map = TempoMap::constant(120.0);
        assert!((map.tick_to_seconds(PPQ) - 0.5).abs() < 1e-12);
        assert!((map.seconds_to_tick(2.0) - 4.0 * PPQ as f64).abs() < 1e-9);
    }

    #[test]
    fn ramp_round_trips() {
        let mut map = TempoMap::constant(100.0);
        map.set_point(TempoPoint { start: 0, bpm: 100.0, ramp: true });
        map.set_point(TempoPoint { start: 16 * PPQ, bpm: 140.0, ramp: false });
        for tick in [0, PPQ, 7 * PPQ, 16 * PPQ, 20 * PPQ] {
            let s = map.tick_to_seconds(tick);
            assert!((map.seconds_to_tick(s) - tick as f64).abs() < 1e-6, "tick {tick}");
        }
        // A ramp from 100 to 140 is faster than constant 100.
        assert!(map.tick_to_seconds(16 * PPQ) < 16.0 * 0.6);
    }

    #[test]
    fn bars_beats_display() {
        let map = TempoMap::constant(120.0);
        assert_eq!(map.bars_beats(0), (1, 1, 0));
        assert_eq!(map.bars_beats(4 * PPQ + PPQ + 10), (2, 2, 10));
    }
}
