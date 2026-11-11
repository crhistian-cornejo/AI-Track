//! Mapping of normalised automation values (0..=1, as stored in
//! [`AutomationLane`]) to engine quantities, and tempo-lane folding.
//!
//! | target        | normalised `v` → value                                   |
//! |---------------|-----------------------------------------------------------|
//! | Volume, Send  | `v ≤ 0` → silence, else `v·76 − 70` dB (`v ≈ 0.921` = 0 dB, `1` = +6 dB) |
//! | Pan           | `2v − 1` (−1 left … 1 right)                              |
//! | Mute, DeviceEnabled | `v ≥ 0.5` → on                                      |
//! | DeviceParam, Macro | used as is (normalised)                              |
//! | Tempo         | `20 + v·280` BPM (20…300)                                 |

use aitrack_model::time::TempoPoint;
use aitrack_model::{AutomationLane, AutomationTarget, TempoMap, Tick, PPQ};

pub const VOLUME_MIN_DB: f32 = -70.0;
pub const VOLUME_MAX_DB: f32 = 6.0;
pub const TEMPO_AUTOMATION_MIN: f64 = 20.0;
pub const TEMPO_AUTOMATION_MAX: f64 = 300.0;

/// Smoothing / control-rate granularity for automation and fader moves.
pub const CONTROL_BLOCK: usize = 32;

#[inline]
pub fn volume_db_from_normalized(v: f32) -> f32 {
    if v <= 0.0 { -160.0 } else { VOLUME_MIN_DB + v.min(1.0) * (VOLUME_MAX_DB - VOLUME_MIN_DB) }
}

#[inline]
pub fn volume_normalized_from_db(db: f32) -> f32 {
    ((db - VOLUME_MIN_DB) / (VOLUME_MAX_DB - VOLUME_MIN_DB)).clamp(0.0, 1.0)
}

#[inline]
pub fn pan_from_normalized(v: f32) -> f32 {
    (v * 2.0 - 1.0).clamp(-1.0, 1.0)
}

#[inline]
pub fn tempo_from_normalized(v: f32) -> f64 {
    TEMPO_AUTOMATION_MIN + v.clamp(0.0, 1.0) as f64 * (TEMPO_AUTOMATION_MAX - TEMPO_AUTOMATION_MIN)
}

#[inline]
pub fn tempo_to_normalized(bpm: f64) -> f32 {
    ((bpm - TEMPO_AUTOMATION_MIN) / (TEMPO_AUTOMATION_MAX - TEMPO_AUTOMATION_MIN)).clamp(0.0, 1.0) as f32
}

/// Equal-power pan law normalised so the centre is unity on both channels
/// (−3 dB law compensated): hard left gives `(√2, 0)`.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (theta.cos() * std::f32::consts::SQRT_2, theta.sin() * std::f32::consts::SQRT_2)
}

/// Value of an enabled, non-empty lane at a fractional tick.
#[inline]
pub fn lane_value(lane: &AutomationLane, tick: f64) -> Option<f32> {
    if !lane.enabled {
        return None;
    }
    lane.value_at(tick.floor() as Tick)
}

/// Fold a tempo automation lane into the tempo map: every point becomes a
/// tempo point ramping linearly to the next one, so ticks↔samples stay exact.
pub fn fold_tempo_lane(base: &TempoMap, lane: &AutomationLane) -> TempoMap {
    if !lane.enabled || lane.points.is_empty() || lane.target != AutomationTarget::Tempo {
        return base.clone();
    }
    let mut map = TempoMap::constant(tempo_from_normalized(lane.points[0].value));
    map.signatures.clone_from(&base.signatures);
    let n = lane.points.len();
    for (i, p) in lane.points.iter().enumerate() {
        map.set_point(TempoPoint { start: p.tick.max(0), bpm: tempo_from_normalized(p.value), ramp: i + 1 < n });
    }
    if lane.points[0].tick > 0 {
        map.set_point(TempoPoint { start: 0, bpm: tempo_from_normalized(lane.points[0].value), ramp: false });
    }
    map
}

/// Length of one beat (in ticks) for a time-signature denominator.
#[inline]
pub fn beat_ticks(denominator: u8) -> Tick {
    PPQ * 4 / denominator.max(1) as Tick
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings() {
        assert!((volume_db_from_normalized(volume_normalized_from_db(0.0))).abs() < 1e-4);
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-5 && (r - 1.0).abs() < 1e-5);
        let (l, r) = pan_gains(-1.0);
        assert!((l - std::f32::consts::SQRT_2).abs() < 1e-5 && r.abs() < 1e-5);
        // Equal power: l² + r² constant.
        for p in [-0.7f32, -0.2, 0.3, 0.9] {
            let (l, r) = pan_gains(p);
            assert!((l * l + r * r - 2.0).abs() < 1e-4);
        }
        assert!((tempo_from_normalized(tempo_to_normalized(128.0)) - 128.0).abs() < 1e-3);
    }
}
