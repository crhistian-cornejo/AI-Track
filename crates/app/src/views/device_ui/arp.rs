//! `midi.arp`: the pattern the arpeggiator would play, drawn as steps. Each
//! column is a step of the grid, each bar a note (its height the pitch, its
//! length the Gate, its brightness the velocity), swing moves the odd steps
//! and the lines are the octaves. The order follows the Style, the notes
//! repeat over the Octaves at the Distance, and a head lights the step the
//! device is on. The dot on the first bar is the Gate: drag it.
//!
//! The device's meter reports where it is in the cycle and how many keys are
//! held, but not which, so the notes are an example chord (as many notes as
//! keys are held, or three) and say so. The random styles are drawn from a
//! preview of the order, not the device's own draws.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::midi_kit as mk;
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::midi::arp::{RATES, STYLES, p};
use gpui::{AnyElement, Context, InteractiveElement, ParentElement, Styled, TextAlign, div, px};

/// Meter layout: position in the cycle, cycle length, keys held, notes sounding.
const POS: usize = 0;
const LEN: usize = 1;
const HELD: usize = 2;
const METER_LEN: usize = 4;
/// Columns drawn, at most.
const MAX_COLS: usize = 32;
/// The example chord, from the bottom: C4 E4 G4 B4 D5 F5 A5 C6…
const EXAMPLE: [i32; 16] = [60, 64, 67, 71, 74, 77, 81, 84, 86, 88, 91, 93, 95, 98, 100, 103];
const PLAY_ORDER: usize = 10;
const CHORD: usize = 11;
const RANDOM: usize = 12;
const RANDOM_OTHER: usize = 13;

/// One step of the grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Col {
    /// The pitches played at this step (one, or the whole chord in the Chord style).
    pub notes: Vec<i32>,
    /// The position in the pattern's cycle (what the meter reports).
    pub pos: usize,
}

/// Steps in one cycle of `style` over `n` pool entries (the device's own rule).
pub fn cycle_len(style: usize, n: usize, octaves: usize) -> usize {
    match style {
        2 | 3 | 8 | 9 => (2 * n).saturating_sub(2).max(1),
        4 | 5 => 2 * n,
        CHORD => octaves,
        _ => n,
    }
}

fn converge(pos: usize, n: usize) -> usize {
    if pos.is_multiple_of(2) { pos / 2 } else { n - 1 - pos / 2 }
}

/// Pool index of position `pos` for the ordered styles.
pub fn pattern_index(style: usize, pos: usize, n: usize) -> usize {
    let up_down = |pos: usize| if pos < n { pos } else { 2 * n - 2 - pos };
    let up_and_down = |pos: usize| if pos < n { pos } else { 2 * n - 1 - pos };
    match style {
        1 => n - 1 - pos,
        2 => up_down(pos),
        3 => n - 1 - up_down(pos),
        4 => up_and_down(pos),
        5 => n - 1 - up_and_down(pos),
        6 => converge(pos, n),
        7 => converge(n - 1 - pos, n),
        8 => {
            if pos.is_multiple_of(2) {
                0
            } else {
                1 + pos / 2
            }
        }
        9 => {
            if pos.is_multiple_of(2) {
                pos / 2
            } else {
                n - 1
            }
        }
        _ => pos,
    }
}

/// A steady stand-in for the random styles' draws.
fn mix(seed: u64, n: u64) -> u64 {
    let mut z = seed.wrapping_mul(0xD6E8_FEB8_6659_FD93).wrapping_add(n).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The keys the picture plays: `held` of the example chord (3 when none are
/// held), pitch-sorted, or in an "as pressed" order for Play Order.
pub fn example_keys(held: usize, style: usize) -> Vec<i32> {
    let n = if held == 0 { 3 } else { held.min(EXAMPLE.len()) };
    let mut keys = EXAMPLE[..n].to_vec();
    if style == PLAY_ORDER && n > 1 {
        keys.rotate_left(1);
    }
    keys
}

/// The pattern for `keys`: the pool is the keys stacked over `octaves`
/// passes each `distance` semitones above the last, played in the order of
/// `style`, every step repeated `note_repeat` times.
pub fn preview(style: usize, keys: &[i32], octaves: usize, distance: i32, note_repeat: usize, seed: u64) -> Vec<Col> {
    let nb = keys.len().max(1);
    let pool = nb * octaves.max(1);
    let len = cycle_len(style, pool, octaves.max(1));
    let note = |idx: usize| keys[idx % nb] + (idx / nb) as i32 * distance;
    let mut last = usize::MAX;
    let steps: Vec<Vec<i32>> = (0..len)
        .map(|pos| {
            if style == CHORD {
                return keys.iter().map(|k| k + pos as i32 * distance).collect();
            }
            let idx = match style {
                RANDOM => (mix(seed, pos as u64) % pool as u64) as usize,
                RANDOM_OTHER => {
                    let mut i = (mix(seed, pos as u64) % pool as u64) as usize;
                    if i == last && pool > 1 {
                        i = (i + 1) % pool;
                    }
                    last = i;
                    i
                }
                s => pattern_index(s, pos, pool),
            };
            vec![note(idx)]
        })
        .collect();
    steps.into_iter().enumerate().flat_map(|(pos, notes)| (0..note_repeat.max(1)).map(move |_| Col { notes: notes.clone(), pos })).take(MAX_COLS).collect()
}

/// Columns a window shows, at most (the pattern repeats to fill them).
const WINDOW_COLS: usize = 32;

/// `want` columns (never fewer than one whole cycle) made of the cycle
/// `cols` over and over, each with the number of the cycle it belongs to.
pub fn tiled(cols: &[Col], want: usize) -> Vec<(usize, Col)> {
    if cols.is_empty() {
        return Vec::new();
    }
    let total = want.max(cols.len());
    (0..total).map(|i| (i / cols.len(), cols[i % cols.len()].clone())).collect()
}

/// Where the pattern sits in the plot, as fractions of it: room on the left
/// for the keys that come in, on the right, and under it for the velocities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub left: f32,
    pub right: f32,
    pub vel: f32,
}

impl Geo {
    /// The dock's: the pattern takes the whole plot.
    pub const CARD: Geo = Geo { left: 0., right: 0., vel: 0. };

    /// A window's, for a plot `w` pixels wide.
    pub fn window(w: f32) -> Geo {
        Geo { left: (64. / w.max(200.)).min(0.2), right: 8. / w.max(200.), vel: 0.2 }
    }

    pub fn width(&self) -> f32 {
        1. - self.left - self.right
    }

    /// Plot fractions of a fraction of the pattern area.
    pub fn x(&self, f: f32) -> f32 {
        self.left + f * self.width()
    }

    pub fn y(&self, f: f32) -> f32 {
        f * (1. - self.vel)
    }
}

/// The columns of the pattern position the device reports (all its repeats).
pub fn head_columns(cols: &[Col], pos: usize) -> std::ops::Range<usize> {
    let first = cols.iter().position(|c| c.pos == pos).unwrap_or(0);
    let last = cols.iter().rposition(|c| c.pos == pos).map_or(0, |i| i + 1);
    first..last.max(first)
}

/// The velocity (0..1) a column plays at.
pub fn column_velocity(mode: usize, fixed: f32, steps: &[f32; 8], len: usize, col: usize) -> f32 {
    match mode {
        1 => fixed / 127.,
        2 => steps[col % len.clamp(1, 8)] / 127.,
        _ => 100. / 127.,
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let style = dev.choice(p::STYLE);
    let octaves = dev.plain(p::OCTAVES).round() as usize;
    let distance = dev.plain(p::DISTANCE).round() as i32;
    let note_repeat = dev.plain(p::NOTE_REPEAT).round() as usize;
    let gate = dev.plain(p::GATE) / 100.;
    let swing = dev.plain(p::SWING) / 100.;
    let offset = dev.plain(p::OFFSET) / 100.;
    let vel_mode = dev.choice(p::VEL_MODE);
    let vel_fixed = dev.plain(p::VEL_FIXED);
    let vel_len = dev.plain(p::VEL_LEN).round() as usize;
    let vel_steps: [f32; 8] = std::array::from_fn(|i| dev.plain(p::VEL_STEP0 + i as u32));
    let seed = dev.plain(p::SEED) as u64;
    let live = dev.meter.len() >= METER_LEN;
    let held = if live { dev.m(HELD) as usize } else { 0 };
    let keys = example_keys(held, style);
    let win = mk::windowed(dev);
    let geo = if win { Geo::window(mk::seen_w(ws, dev, 0, 640.)) } else { Geo::CARD };
    let cycle = preview(style, &keys, octaves, distance, note_repeat, seed);
    let cycle_cols = cycle.len().max(1);
    // A window shows the pattern come round again (as many cycles as the Repeats allow).
    let cols: Vec<Col> = if win { tiled(&cycle, WINDOW_COLS).into_iter().map(|(_, c)| c).collect() } else { cycle };
    let cycles_played = if dev.choice(p::REPEATS) < 8 { dev.choice(p::REPEATS) + 1 } else { usize::MAX };
    let head = live.then(|| head_columns(&cols[..cycle_cols.min(cols.len())], dev.m(POS) as usize)).filter(|r| !r.is_empty());
    let _ = LEN;
    let (lo, hi) = (cols.iter().flat_map(|c| c.notes.iter()).copied().min().unwrap_or(60) - 2, cols.iter().flat_map(|c| c.notes.iter()).copied().max().unwrap_or(72) + 2);
    let (lo, hi) = (lo.min(keys.iter().copied().min().unwrap_or(lo) - 2), hi.max(keys.iter().copied().max().unwrap_or(hi) + 2));
    let n = cols.len().max(1) as f32;
    let enabled = dev.enabled;
    let repeats = dev.choice(p::REPEATS);
    let title = format!(
        "{} · {} · {}{}",
        STYLES[style.min(STYLES.len() - 1)],
        RATES[dev.choice(p::RATE).min(RATES.len() - 1)],
        if live && held > 0 { format!("{held} keys held (example notes)") } else { "example chord".to_owned() },
        if repeats < 8 { format!(" · {} cycle{}", repeats + 1, if repeats == 0 { "" } else { "s" }) } else { String::new() },
    );
    let y_of = move |pitch: i32| 1. - 0.12 - 0.76 * (pitch - lo) as f32 / (hi - lo).max(1) as f32;
    let cycle_cols_n = cycle_cols;
    let in_keys = keys.clone();
    let vel_name = ["as played", "fixed", "per step"][vel_mode.min(2)];
    // The Gate: the end of the first bar.
    let first = cols.first().and_then(|c| c.notes.first().copied()).unwrap_or(60);
    let cell = 1. / n;
    let handle_fx = ((offset.max(0.) + gate.min(1.9)) * cell).min(0.98);
    let handles = vec![Handle {
        fx: geo.x(handle_fx),
        fy: geo.y(y_of(first)),
        x: ax(p::GATE, 100. / (199. * cell) / geo.width()),
        index: 0,
        reset: vec![(p::GATE, dev.info(p::GATE).map_or(0.4, |i| i.default_normalized()))],
        ..Default::default()
    }];

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |full: Rect, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        // The pattern's own area; a window keeps the left for the keys that come in and the bottom for velocities.
        let r = Rect { x: full.px(geo.left), y: full.y, w: full.w * geo.width(), h: full.h * (1. - geo.vel) };
        let row_h = r.h * 0.76 / (hi - lo).max(1) as f32;
        // The keys of the keyboard behind the notes: the black ones a shade darker.
        if win {
            for pitch in lo..=hi {
                if mk::is_black(pitch) {
                    kit::bar(window, r.x, r.py(y_of(pitch)) - row_h / 2., r.w, row_h, 0., content(0.035));
                }
            }
        }
        // The octaves (every C).
        for pitch in lo..=hi {
            if pitch.rem_euclid(12) == 0 {
                kit::hline(window, r, r.py(y_of(pitch)), pal.grid_bar);
                // In a window the keys that come in are named on the left, so the octaves go on the right.
                if win {
                    kit::txt(window, cx, &format!("C{}", pitch.div_euclid(12) - 1), r.right() - 4., r.py(y_of(pitch)) - 14., TEXT_SM, pal.faint, TextAlign::Right);
                } else {
                    kit::txt(window, cx, &format!("C{}", pitch.div_euclid(12) - 1), r.x + 4., r.py(y_of(pitch)) - 14., TEXT_SM, pal.faint, TextAlign::Left);
                }
            }
        }
        // The step grid, the cycles of the pattern and the head on the step it is on.
        for i in 0..cols.len() {
            if i % 4 == 0 {
                kit::vline(window, r, r.px(i as f32 * cell), pal.grid_beat);
            }
        }
        if win {
            for k in (cycle_cols_n..cols.len()).step_by(cycle_cols_n) {
                kit::vline(window, r, r.px(k as f32 * cell), pal.grid_bar);
            }
            // The keys that come in: a name on each at its height, a faint line along it.
            kit::txt(window, cx, "in", full.x + 6., full.y + 4., TEXT_SM, pal.faint, TextAlign::Left);
            for key in &in_keys {
                let y = r.py(y_of(*key));
                kit::hline(window, r, y, content(0.08));
                kit::bar(window, full.x + 6., y - 6., 30., 12., 3., with_alpha(pal.text, 0.28));
                kit::txt(window, cx, &mk::note_name(*key), full.x + 21., y - 6., TEXT_SM, pal.text, TextAlign::Center);
            }
            kit::vline(window, full, r.x - 4., pal.grid_bar);
        }
        if let Some(range) = &head {
            kit::bar(window, r.px(range.start as f32 * cell), r.y, r.w * cell * range.len() as f32, r.h, 0., accent_alpha(0.14));
        }
        // The notes.
        let steps = vel_steps;
        let thick = if win { (row_h * 0.8).clamp(6., 18.) } else { 6. };
        for (i, c) in cols.iter().enumerate() {
            let start = (i as f32 + offset + if i % 2 == 1 { swing * 0.5 } else { 0. }) * cell;
            let v = column_velocity(vel_mode, vel_fixed, &steps, vel_len, i);
            // After the Repeats have run out the pattern is shown as a ghost.
            let silent = win && i / cycle_cols_n >= cycles_played;
            for pitch in &c.notes {
                let w = (gate.min(1.9) * cell * r.w).max(3.);
                let playing = head.as_ref().is_some_and(|h| h.contains(&i));
                let fill = with_alpha(if playing { pal.text } else { colour }, if silent { 0.1 } else { 0.3 + 0.7 * v });
                kit::bar(window, r.px(start), r.py(y_of(*pitch)) - thick / 2., w, thick, 2., fill);
            }
            if win {
                // The velocity of the step, under the grid.
                let lane = Rect { x: r.x, y: full.y + full.h * (1. - geo.vel) + 20., w: r.w, h: full.h * geo.vel - 24. };
                let bw = (cell * r.w - 3.).max(2.);
                let bh = (lane.h * v).max(2.);
                kit::bar(window, r.px(i as f32 * cell) + 1., lane.bottom() - bh, bw, bh, 1., with_alpha(if silent { pal.muted } else { colour }, if silent { 0.2 } else { 0.55 }));
            }
        }
        if win {
            let ly = full.y + full.h * (1. - geo.vel);
            kit::hline(window, Rect { x: full.x, w: full.w, ..r }, ly, pal.grid_bar);
            kit::txt(window, cx, &format!("velocity · {vel_name}"), full.x + 6., ly + 4., TEXT_SM, pal.faint, TextAlign::Left);
        }
        kit::dot(window, r.pt(handle_fx, y_of(first)), 5.5, pal.accent, Some(pal.text));
        kit::txt(window, cx, &title, r.right() - 6., r.y + 4., TEXT_SM, pal.faint, TextAlign::Right);
    });

    let mut style_card = vec![kit::ctl(ws, dev, p::STYLE, cx), kit::ctl(ws, dev, p::RATE, cx), kit::ctl(ws, dev, p::OCTAVES, cx), kit::ctl(ws, dev, p::DISTANCE, cx), kit::ctl(ws, dev, p::NOTE_REPEAT, cx)];
    if style == RANDOM || style == RANDOM_OTHER {
        style_card.push(kit::ctl(ws, dev, p::SEED, cx));
    }
    let gate_card: Vec<AnyElement> = [p::GATE, p::SWING, p::OFFSET].into_iter().map(|id| kit::ctl(ws, dev, id, cx)).collect();
    let hold_card: Vec<AnyElement> = [p::HOLD, p::RETRIGGER, p::REPEATS].into_iter().map(|id| kit::ctl(ws, dev, id, cx)).collect();
    let mut vel_card = vec![kit::ctl(ws, dev, p::VEL_MODE, cx)];
    match vel_mode {
        1 => vel_card.push(kit::ctl(ws, dev, p::VEL_FIXED, cx)),
        2 => {
            vel_card.push(kit::ctl(ws, dev, p::VEL_LEN, cx));
            for i in 0..vel_len.clamp(1, 8) {
                vel_card.push(kit::num_w(ws, dev, p::VEL_STEP0 + i as u32, 44., cx));
            }
        }
        _ => {}
    }
    let cards = vec![
        kit::section(dev, "PATTERN", None, style_card, cx),
        kit::section(dev, "GATE", None, gate_card, cx),
        kit::section(dev, "HOLD", None, hold_card, cx),
        kit::section(dev, "VELOCITY", None, vel_card, cx),
    ];
    let frame = div().id(("arp-plot", dev.key(0))).size_full().rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(plot);
    super::midi_kit::body_with(dev, "arp-body", 520., 96., 176., frame, cards)
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    fn notes(cols: &[Col]) -> Vec<i32> {
        cols.iter().map(|c| c.notes[0]).collect()
    }

    #[test]
    fn the_ordered_styles_walk_the_pool_as_the_device_does() {
        let keys = [60, 64, 67];
        assert_eq!(notes(&preview(0, &keys, 1, 12, 1, 0)), [60, 64, 67]);
        assert_eq!(notes(&preview(1, &keys, 1, 12, 1, 0)), [67, 64, 60]);
        assert_eq!(notes(&preview(2, &keys, 1, 12, 1, 0)), [60, 64, 67, 64], "Up-Down does not repeat the ends");
        assert_eq!(notes(&preview(4, &keys, 1, 12, 1, 0)), [60, 64, 67, 67, 64, 60], "Up & Down does");
        assert_eq!(notes(&preview(6, &keys, 1, 12, 1, 0)), [60, 67, 64], "Converge");
        // Octaves stack the keys a Distance higher each pass.
        assert_eq!(notes(&preview(0, &keys, 2, 12, 1, 0)), [60, 64, 67, 72, 76, 79]);
        assert_eq!(notes(&preview(0, &keys, 2, -12, 1, 0)), [60, 64, 67, 48, 52, 55]);
    }

    #[test]
    fn chord_style_plays_the_whole_chord_a_distance_higher_each_pass() {
        let cols = preview(CHORD, &[60, 64, 67], 3, 12, 1, 0);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[1].notes, vec![72, 76, 79]);
    }

    #[test]
    fn note_repeat_repeats_each_step_and_the_head_covers_all_its_repeats() {
        let cols = preview(0, &[60, 64, 67], 1, 12, 2, 0);
        assert_eq!(cols.iter().map(|c| c.pos).collect::<Vec<_>>(), [0, 0, 1, 1, 2, 2]);
        assert_eq!(head_columns(&cols, 1), 2..4);
        // Never more columns than fit.
        assert_eq!(preview(0, &[60, 64, 67, 71], 4, 12, 4, 0).len(), MAX_COLS);
    }

    #[test]
    fn the_random_styles_are_steady_for_a_seed_and_random_other_never_repeats() {
        let a = notes(&preview(RANDOM, &[60, 64, 67], 2, 12, 1, 7));
        assert_eq!(a, notes(&preview(RANDOM, &[60, 64, 67], 2, 12, 1, 7)));
        assert_ne!(a, notes(&preview(RANDOM, &[60, 64, 67], 2, 12, 1, 8)));
        let o = notes(&preview(RANDOM_OTHER, &[60, 64, 67], 2, 12, 1, 3));
        assert!(o.windows(2).all(|w| w[0] != w[1]), "{o:?}");
    }

    #[test]
    fn the_example_chord_has_as_many_notes_as_keys_held() {
        assert_eq!(example_keys(0, 0), vec![60, 64, 67]);
        assert_eq!(example_keys(5, 0), vec![60, 64, 67, 71, 74]);
        assert_eq!(example_keys(2, PLAY_ORDER), vec![64, 60], "as pressed, not by pitch");
        assert_eq!(example_keys(40, 0).len(), 16);
    }

    #[test]
    fn velocity_follows_the_mode_and_the_cycle_length_of_the_steps() {
        let steps = [10., 20., 30., 40., 50., 60., 70., 80.];
        assert_eq!(column_velocity(0, 90., &steps, 8, 3), 100. / 127.);
        assert_eq!(column_velocity(1, 90., &steps, 8, 3), 90. / 127.);
        assert_eq!(column_velocity(2, 90., &steps, 3, 4), 20. / 127., "step 4 wraps over 3");
    }

    #[test]
    fn the_editor_reads_the_real_params() {
        let d = dev("midi.arp", &[(p::STYLE, 2.), (p::OCTAVES, 3.), (p::GATE, 50.)]);
        assert_eq!((d.choice(p::STYLE), d.plain(p::OCTAVES), d.plain(p::GATE)), (2, 3., 50.));
        assert_eq!(cycle_len(2, 6, 1), 10);
        assert_eq!(cycle_len(4, 6, 1), 12);
        assert_eq!((STYLES.len(), RATES.len()), (14, 13));
    }
}
