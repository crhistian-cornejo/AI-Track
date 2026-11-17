//! `midi.echo`: the repeats of one example note on a beat grid. Every repeat
//! is a block starting where the echo lands, as tall as its velocity (so the
//! fade-out is the outline of the row) and as long as the repeat will sound;
//! a label above it carries the pitch piled up so far. Whiskers mark the
//! random timing and velocity, repeats that leave the keyboard are drawn
//! hollow, and the slots the fade never reaches are small ticks on the
//! floor. The first repeat is a handle: across sets the time, up and down the
//! feedback.

use super::Dev;
use super::kit::{self, Handle, PlotCfg, ax};
use super::midi_kit as mk;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::midi::echo::{MAX_REPEATS, p};
use aitrack_stock::midi::kit::{DIVISION_NAMES, division_beats};
use gpui::{AnyElement, Context, Hsla, TextAlign};

/// The example note: middle C played at this velocity for a sixteenth.
const KEY: i32 = 60;
const VELOCITY: f32 = 0.8;
const NOTE_BEATS: f32 = 0.25;
/// What the plugin stops at: one velocity step.
const MIN_VEL: f32 = 1. / 127.;
/// Room above the bars (header and pitch labels) and below them (the beats).
const TOP: f32 = 24.;
const BOTTOM: f32 = 16.;

/// One repeat of the example note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rep {
    pub k: usize,
    pub vel: f32,
    /// Semitones piled up by this repeat.
    pub pitch: i32,
    /// Whether the key stays on the keyboard (a repeat that leaves it is skipped).
    pub heard: bool,
}

/// The repeats the plugin queues for a note of `key` at `velocity`: each
/// one is the last times `feedback` plus `vel_step` (0..1 units), the chain
/// stops once it is inaudible, and repeats off the keyboard are skipped (but
/// the chain goes on).
pub fn chain(repeats: usize, feedback: f32, vel_step: f32, pitch: i32, key: i32, velocity: f32) -> Vec<Rep> {
    let mut v = velocity;
    let mut out = Vec::new();
    for k in 1..=repeats.min(MAX_REPEATS) {
        v = v * feedback + vel_step;
        if v < MIN_VEL {
            break;
        }
        v = v.min(1.);
        let shift = k as i32 * pitch;
        out.push(Rep { k, vel: v, pitch: shift, heard: (0..=127).contains(&(key + shift)) });
    }
    out
}

/// The shortest span of beats (2, 4, 8 … 64) that holds `repeats` echoes of
/// `step` beats and the last one's body.
pub fn span_beats(step: f32, repeats: usize) -> f32 {
    let need = step * repeats as f32 + step.max(0.5);
    [2., 4., 8., 16., 32.].into_iter().find(|s| *s >= need).unwrap_or(64.)
}

/// Everything the plot draws, from the parameters.
#[derive(Clone, Debug)]
struct Echo {
    step: f32,
    span: f32,
    configured: usize,
    reps: Vec<Rep>,
    /// Length of every repeat in beats.
    len: f32,
    dry: bool,
    /// Share of repeats that sound, 0..1.
    chance: f32,
    vel_rand: f32,
    /// Timing spread in beats either way.
    time_rand: f32,
    pitch_step: i32,
    pitch_rand: i32,
}

impl Echo {
    fn of(dev: &Dev) -> Self {
        let step = if dev.on(p::SYNC) { division_beats(dev.choice(p::DIVISION), (4, 4)) as f32 } else { mk::ms_beats(dev.plain(p::TIME), dev.bpm) }.max(0.01);
        let configured = (dev.plain(p::REPEATS).round().max(0.) as usize).min(MAX_REPEATS);
        let gate = dev.plain(p::GATE) / 100.;
        let pitch_step = dev.plain(p::PITCH).round() as i32;
        Self {
            step,
            // A window shows at least two bars.
            span: if mk::windowed(dev) { span_beats(step, configured).max(8.) } else { span_beats(step, configured) },
            configured,
            reps: chain(configured, dev.plain(p::FEEDBACK) / 100., dev.plain(p::VEL_STEP).round() / 127., pitch_step, KEY, VELOCITY),
            len: if dev.choice(p::LENGTH) == 0 { NOTE_BEATS * gate } else { step * gate },
            dry: dev.on(p::DRY),
            chance: dev.plain(p::CHANCE) / 100.,
            vel_rand: dev.plain(p::VEL_RAND) / 100.,
            time_rand: mk::ms_beats(dev.plain(p::TIME_RAND), dev.bpm),
            pitch_step,
            pitch_rand: dev.plain(p::PITCH_RAND).round() as i32,
        }
    }

    fn heard(&self) -> usize {
        self.reps.iter().filter(|r| r.heard).count()
    }

    /// The left caption: how many of the repeats ring out.
    fn headline(&self) -> String {
        if self.configured == 0 {
            return "no repeats".to_owned();
        }
        let mut text = format!("{} of {} repeats sound", self.heard(), self.configured);
        if self.reps.len() < self.configured {
            text += &format!(" · fades out after {}", self.reps.len());
        }
        if self.chance < 0.995 {
            text += &format!(" · {:.0}% each", self.chance * 100.);
        }
        text
    }
}

/// The plot fraction (0 = top) of a velocity.
fn vel_frac(vel: f32, h: f32) -> f32 {
    (TOP + (h - TOP - BOTTOM) * (1. - vel.clamp(0., 1.))) / h
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let e = Echo::of(dev);
    let win = mk::windowed(dev);
    let h = if win { mk::seen_h(ws, dev, 0, mk::plot_h(dev)) } else { mk::plot_h(dev) };
    let sync = dev.on(p::SYNC);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    // The first repeat that sounds: its start is the time, its height the feedback.
    let mut handles = Vec::new();
    if let Some(first) = e.reps.first().filter(|r| r.heard) {
        let usable = (h - TOP - BOTTOM) / h;
        handles.push(Handle {
            fx: (e.step / e.span).min(1.),
            fy: vel_frac(first.vel, h),
            x: if sync { ax(p::DIVISION, 1.) } else { ax(p::TIME, e.span / e.step / 2000f32.ln()) },
            y: ax(p::FEEDBACK, -1. / (usable * VELOCITY)),
            reset: vec![default(if sync { p::DIVISION } else { p::TIME }), default(p::FEEDBACK)],
            ..Default::default()
        });
    }
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let enabled = dev.enabled;
    let right = if sync {
        format!("every {} · {}", DIVISION_NAMES[dev.choice(p::DIVISION).min(DIVISION_NAMES.len() - 1)], mk::fmt_ms(mk::beats_ms(e.step, dev.bpm)))
    } else {
        format!("every {} · {:.2} beats", mk::fmt_ms(dev.plain(p::TIME)), e.step)
    };
    let left = e.headline();
    let note = e.clone();

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let lit = |c: Hsla| if enabled { c } else { pal.muted };
        let e = note;
        let base = r.bottom() - BOTTOM;
        let usable = (base - r.y - TOP).max(8.);
        // The beat grid, bars stronger.
        let every = if e.span <= 16. { 1. } else { 4. };
        let mut b = 0.;
        while b <= e.span + 1e-3 {
            let x = r.px(b / e.span);
            let bar = (b % 4.).abs() < 1e-3;
            kit::vline(window, r, x, if bar { pal.grid_bar } else { pal.grid_beat });
            if bar || e.span <= 8. {
                kit::txt(window, cx, &format!("{}", b as i32), x + 3., r.bottom() - 13., TEXT_SM, pal.faint, TextAlign::Left);
            }
            b += every;
        }
        kit::hline(window, r, base, pal.grid_bar);
        if win {
            // The velocity scale: full, and half.
            for (q, text) in [(1., "127"), (0.5, "64")] {
                kit::hline(window, r, base - usable * q, if q == 1. { pal.grid_bar } else { pal.grid_beat });
                kit::txt(window, cx, text, r.x + 4., base - usable * q + 2., TEXT_SM, pal.faint, TextAlign::Left);
            }
        }
        kit::txt(window, cx, "beats", r.right() - 6., r.bottom() - 13., TEXT_SM, pal.faint, TextAlign::Right);
        mk::corner(window, cx, r, &left, false, 3.);
        mk::corner(window, cx, r, &right, true, 3.);
        // The note played; hollow when the dry signal is off.
        let h0 = usable * VELOCITY;
        kit::bar(window, r.x + 1., base - h0, (NOTE_BEATS / e.span * r.w).max(3.), h0, 2., if e.dry { with_alpha(lit(pal.text), 0.6) } else { with_alpha(pal.text, 0.18) });
        if win {
            kit::txt(window, cx, if e.dry { "in" } else { "in (dry off)" }, r.x + 4., base - h0 - 14., TEXT_SM, pal.muted, TextAlign::Left);
        }
        let room = e.step / e.span * r.w >= 26.;
        for rep in &e.reps {
            let t = rep.k as f32 * e.step;
            let x = r.px(t / e.span);
            let w = (e.len / e.span * r.w).max(3.);
            let bh = (usable * rep.vel).max(2.);
            let fill = if rep.heard { with_alpha(lit(series(0)), (0.3 + 0.7 * rep.vel) * (0.35 + 0.65 * e.chance)) } else { with_alpha(pal.muted, 0.2) };
            kit::bar(window, x, base - bh, w, bh, 2., fill);
            if win && rep.heard && w >= 16. && bh >= 16. {
                kit::txt(window, cx, &format!("{:.0}", rep.vel * 127.), x + w / 2., base - bh + 3., TEXT_SM, kit::ink_on(fill), TextAlign::Center);
            }
            if e.time_rand > 0. {
                mk::span(window, r.px((t - e.time_rand) / e.span), r.px((t + e.time_rand) / e.span), base - 3., 3., 1.5, with_alpha(pal.text, 0.5));
            }
            if e.vel_rand > 0. {
                let hi = (rep.vel * (1. + e.vel_rand)).min(1.);
                let lo = rep.vel * (1. - e.vel_rand);
                kit::bar(window, x + w / 2. - 0.5, base - usable * hi, 1., usable * (hi - lo), 0., with_alpha(pal.text, 0.6));
            }
            if room && rep.heard && (e.pitch_step != 0 || e.pitch_rand != 0) {
                let mut label = format!("{:+}", rep.pitch);
                if e.pitch_rand > 0 {
                    label += &format!("±{}", e.pitch_rand);
                }
                // Above the velocity whisker, when there is one.
                let high = if e.vel_rand > 0. { (rep.vel * (1. + e.vel_rand)).min(1.) } else { rep.vel };
                kit::txt(window, cx, &label, x + w / 2., (base - usable * high - 14.).max(r.y + 14.), TEXT_SM, pal.muted, TextAlign::Center);
            }
        }
        // Where the fade never reaches.
        for k in e.reps.len() + 1..=e.configured {
            let t = k as f32 * e.step;
            if t < e.span {
                kit::bar(window, r.px(t / e.span), base - 2., 3., 2., 1., with_alpha(pal.faint, 0.6));
            }
        }
        for (fx, fy) in &marks {
            kit::dot(window, r.pt(*fx, *fy), 6., lit(series(0)), Some(pal.raised));
        }
    });

    let sync_or_free = if sync { p::DIVISION } else { p::TIME };
    let time = vec![kit::ctl(ws, dev, p::SYNC, cx), kit::ctl(ws, dev, sync_or_free, cx), kit::ctl(ws, dev, p::LOOP, cx)];
    let reps: Vec<AnyElement> = [p::REPEATS, p::FEEDBACK, p::VEL_STEP, p::PITCH].into_iter().map(|id| kit::ctl(ws, dev, id, cx)).collect();
    let length: Vec<AnyElement> = [p::LENGTH, p::GATE, p::DRY].into_iter().map(|id| kit::ctl(ws, dev, id, cx)).collect();
    let random: Vec<AnyElement> = [p::CHANCE, p::VEL_RAND, p::TIME_RAND, p::PITCH_RAND, p::SEED].into_iter().map(|id| kit::ctl(ws, dev, id, cx)).collect();
    let cards = vec![
        kit::section(dev, "TIME", None, time, cx),
        kit::section(dev, "REPEATS", None, reps, cx),
        kit::section(dev, "LENGTH", None, length, cx),
        kit::section(dev, "RANDOM", None, random, cx),
    ];
    mk::body(dev, "echo-body", 560., plot, cards)
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn the_chain_falls_with_the_feedback_and_stops_when_inaudible() {
        let c = chain(3, 0.5, 0., 0, 60, 0.8);
        assert_eq!(c.iter().map(|r| r.vel).collect::<Vec<_>>(), vec![0.4, 0.2, 0.1]);
        // .15, .045, .0135, then .004 is under one step: three repeats at most.
        assert_eq!(chain(16, 0.3, 0., 0, 60, 0.5).len(), 3);
        assert!(chain(0, 1., 0., 0, 60, 1.).is_empty());
    }

    #[test]
    fn a_positive_step_builds_up_and_clamps() {
        let c = chain(6, 1., 16. / 127., 0, 60, 0.3);
        assert_eq!(c.len(), 6);
        assert!(c.windows(2).all(|w| w[1].vel >= w[0].vel));
        assert_eq!(c.last().unwrap().vel, 1.);
    }

    #[test]
    fn pitch_piles_up_and_repeats_off_the_keyboard_are_skipped() {
        let c = chain(4, 1., 0., 12, 110, 0.8);
        assert_eq!(c.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![12, 24, 36, 48]);
        assert_eq!(c.iter().map(|r| r.heard).collect::<Vec<_>>(), vec![true, false, false, false]);
        let down = chain(3, 1., 0., -7, 10, 0.8);
        assert_eq!(down.iter().map(|r| r.heard).collect::<Vec<_>>(), vec![true, false, false]);
    }

    #[test]
    fn the_span_holds_every_repeat() {
        assert_eq!(span_beats(0.5, 4), 4.);
        assert_eq!(span_beats(1., 4), 8.);
        assert_eq!(span_beats(4., 16), 64.);
        assert_eq!(span_beats(0.01, 0), 2.);
    }

    #[test]
    fn the_plot_follows_the_parameters() {
        // The default: an eighth every half beat at 120 bpm, four repeats.
        let e = Echo::of(&dev("midi.echo", &[]));
        assert_eq!((e.step, e.configured, e.reps.len()), (0.5, 4, 4));
        assert!(e.headline().starts_with("4 of 4"));
        // Free time in milliseconds follows the tempo.
        let free = Echo::of(&dev("midi.echo", &[(p::SYNC, 0.), (p::TIME, 500.)]));
        assert!((free.step - 1.).abs() < 1e-6);
        // Interval length is the gate of the spacing, original length of the note.
        let interval = Echo::of(&dev("midi.echo", &[(p::LENGTH, 1.), (p::GATE, 50.)]));
        assert!((interval.len - 0.25).abs() < 1e-6);
        let original = Echo::of(&dev("midi.echo", &[(p::GATE, 200.)]));
        assert!((original.len - 0.5).abs() < 1e-6);
        let fading = Echo::of(&dev("midi.echo", &[(p::REPEATS, 16.), (p::FEEDBACK, 30.)]));
        assert!(fading.headline().contains("fades out after 3"), "{}", fading.headline());
    }
}
