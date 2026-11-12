//! The four macros of each engine.
//!
//! A macro drives several parameters at once. Each [`MacroTarget`] adds an
//! offset to its parameter's *normalised* base value:
//!
//! ```text
//! effective = clamp(base + depth × lerp(from, to, curve(macro)), 0, 1)
//! ```
//!
//! `from`/`to` are normalised offsets (so `to: 0.4` opens a cutoff by 40 %
//! of its knob travel), `depth` is the preset's `MACRO_DEPTHn` (−100..100 %)
//! and the curve shapes the knob travel. With every macro at 0 the sound is
//! exactly the base patch, so a preset is written "macros down" and the
//! macro values it stores are where the knobs start.
//!
//! Macro 4 is "Space" on every engine (global reverb and delay sends).

use super::params::{self as pp, engine, fm, ladder, organ, poly, strings, wt};
use crate::instrument::wavetable::p as wp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// Slow start (x²): fine control at the bottom.
    Exp,
    /// Fast start (√x).
    Log,
    /// Smoothstep.
    S,
    /// Off below 50 %, on above (switches such as rotor speed).
    Step,
}

impl Curve {
    #[inline]
    pub fn apply(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Curve::Linear => x,
            Curve::Exp => x * x,
            Curve::Log => x.sqrt(),
            Curve::S => x * x * (3.0 - 2.0 * x),
            Curve::Step => {
                if x >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MacroTarget {
    pub param: u32,
    /// Normalised offsets at macro 0 and macro 100 %.
    pub from: f32,
    pub to: f32,
    pub curve: Curve,
}

#[derive(Clone, Copy, Debug)]
pub struct MacroDef {
    pub name: &'static str,
    pub targets: &'static [MacroTarget],
}

const fn t(param: u32, to: f32, curve: Curve) -> MacroTarget {
    MacroTarget { param, from: 0.0, to, curve }
}

const SPACE: MacroDef = MacroDef { name: "Space", targets: &[t(pp::REVERB_MIX, 0.45, Curve::Linear), t(pp::DELAY_MIX, 0.3, Curve::Exp), t(pp::REVERB_DECAY, 0.25, Curve::Linear)] };

const LADDER: [MacroDef; 4] = [
    MacroDef { name: "Cutoff", targets: &[t(ladder::CUTOFF, 0.45, Curve::Linear), t(ladder::ENV_AMOUNT, 0.1, Curve::Linear)] },
    MacroDef { name: "Resonance", targets: &[t(ladder::RESONANCE, 0.6, Curve::Linear), t(ladder::DRIVE, 0.3, Curve::Linear)] },
    MacroDef { name: "Contour", targets: &[t(ladder::F_DECAY, 0.3, Curve::Linear), t(ladder::ENV_AMOUNT, 0.25, Curve::Linear), t(ladder::A_RELEASE, 0.2, Curve::Linear)] },
    SPACE,
];

const POLY: [MacroDef; 4] = [
    MacroDef { name: "Brightness", targets: &[t(poly::CUTOFF, 0.4, Curve::Linear), t(poly::RESONANCE, 0.1, Curve::Linear)] },
    MacroDef { name: "Poly-Mod", targets: &[t(poly::PM_ENV, 0.35, Curve::Exp), t(poly::PM_OSC_B, 0.25, Curve::Exp)] },
    MacroDef { name: "Motion", targets: &[t(poly::LFO_CUTOFF, 0.35, Curve::Linear), t(poly::LFO_PW, 0.4, Curve::Linear), t(poly::DRIFT, 0.3, Curve::Linear)] },
    SPACE,
];

const FM: [MacroDef; 4] = [
    MacroDef { name: "Brightness", targets: &[t(fm::BRIGHTNESS, 0.35, Curve::Linear)] },
    MacroDef { name: "Feedback", targets: &[t(fm::FEEDBACK, 0.6, Curve::Exp)] },
    MacroDef { name: "Time", targets: &[t(fm::TIME, 0.35, Curve::Linear)] },
    SPACE,
];

const EP: [MacroDef; 4] = [
    MacroDef { name: "Tone", targets: &[t(pp::ep::TONE, 0.3, Curve::Linear), t(pp::ep::HARDNESS, 0.35, Curve::Linear), t(pp::ep::BELL, 0.3, Curve::Linear)] },
    MacroDef { name: "Bark", targets: &[t(pp::ep::BARK, 0.6, Curve::Linear), t(pp::ep::PICKUP, 0.3, Curve::Linear)] },
    MacroDef { name: "Tremolo", targets: &[t(pp::ep::TREM_DEPTH, 0.8, Curve::Linear), t(pp::ep::TREM_RATE, 0.15, Curve::Linear)] },
    SPACE,
];

const ORGAN: [MacroDef; 4] = [
    MacroDef { name: "Drawbars", targets: &[t(organ::drawbar(3), 0.6, Curve::Linear), t(organ::drawbar(5), 0.5, Curve::Linear), t(organ::drawbar(8), 0.6, Curve::Exp)] },
    MacroDef { name: "Rotor Fast", targets: &[t(organ::SPEED, 1.0, Curve::Step)] },
    MacroDef { name: "Drive", targets: &[t(organ::DRIVE, 0.6, Curve::Linear), t(organ::CLICK, 0.2, Curve::Linear)] },
    SPACE,
];

const STRINGS: [MacroDef; 4] = [
    MacroDef { name: "Tone", targets: &[t(strings::TONE, 0.35, Curve::Linear), t(strings::VIOLA, 0.3, Curve::Linear)] },
    MacroDef { name: "Ensemble", targets: &[t(strings::ENSEMBLE, 0.5, Curve::Linear), t(strings::ENS_DEPTH, 0.4, Curve::Linear)] },
    MacroDef { name: "Swell", targets: &[t(strings::ATTACK, 0.35, Curve::Linear), t(strings::SWELL, 0.6, Curve::Linear), t(strings::RELEASE, 0.2, Curve::Linear)] },
    SPACE,
];

const WAVETABLE: [MacroDef; 4] = [
    MacroDef { name: "Brightness", targets: &[t(wt(wp::CUTOFF), 0.35, Curve::Linear), t(wt(wp::RESONANCE), 0.1, Curve::Linear)] },
    // The wavetable patch routes its own macro 1 in its matrix.
    MacroDef { name: "Movement", targets: &[t(wt(wp::macro_knob(0)), 1.0, Curve::Linear)] },
    MacroDef { name: "Character", targets: &[t(wt(wp::osc(0, wp::POS)), 0.5, Curve::Linear), t(wt(wp::DRIVE), 0.4, Curve::Linear)] },
    SPACE,
];

/// The macro map of `engine` (an index into [`super::ENGINES`]).
pub fn macros(engine: usize) -> &'static [MacroDef; 4] {
    match engine {
        engine::LADDER => &LADDER,
        engine::POLY => &POLY,
        engine::FM => &FM,
        engine::EP => &EP,
        engine::ORGAN => &ORGAN,
        engine::STRINGS => &STRINGS,
        _ => &WAVETABLE,
    }
}

/// Offset macro `def` adds to its target `t` at knob value `x` (0..1) with depth `depth` (−1..1).
#[inline]
pub fn offset(target: &MacroTarget, x: f32, depth: f32) -> f32 {
    let c = target.curve.apply(x);
    depth * (target.from + (target.to - target.from) * c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_is_a_real_param_of_its_engine() {
        for e in 0..pp::ENGINE_COUNT {
            for m in macros(e) {
                assert!(!m.targets.is_empty());
                for t in m.targets {
                    assert!(pp::all().iter().any(|i| i.id == t.param), "engine {e} macro {} → {}", m.name, t.param);
                    let owner = pp::engine_of(t.param);
                    assert!(owner.is_none() || owner == Some(e), "engine {e} macro {} targets another engine", m.name);
                }
            }
        }
    }

    #[test]
    fn curves_are_monotonic_and_bounded() {
        for c in [Curve::Linear, Curve::Exp, Curve::Log, Curve::S, Curve::Step] {
            let mut last = -1.0;
            for i in 0..=100 {
                let y = c.apply(i as f32 / 100.0);
                assert!((0.0..=1.0).contains(&y) && y >= last);
                last = y;
            }
            assert_eq!(c.apply(0.0), 0.0);
            assert_eq!(c.apply(1.0), 1.0);
        }
    }
}
