//! `midi.pitch`: the 128 keys as played (top) and as they sound (bottom),
//! joined by one line per key. The range that gets shifted (Lowest and
//! Width) is lit on the top row; a line runs from each of its keys to where
//! it lands, folded back or held at the edge when it would leave the
//! keyboard and stopped by a cross when it is blocked; keys outside the range
//! pass straight down or fade when they are blocked. With Layer the played
//! key stays and the shifted one is added beside it. The edges of the range
//! and the shift itself are handles (wheel or Alt-drag on the shift: octave).

use super::Dev;
use super::kit::{self, Handle, PlotCfg, ax};
use super::midi_kit as mk;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::midi::kit::fold_octaves;
use aitrack_stock::midi::pitch::p;
use gpui::{AnyElement, Context, Hsla, TextAlign, point, px};

/// Room left of the keys for the row names, and right of them.
const PAD_L: f32 = 24.;
const PAD_R: f32 = 8.;
const TOP: f32 = 20.;
const BOTTOM: f32 = 18.;
/// `Overflow` choices.
const BLOCK: usize = 0;
const FOLD: usize = 1;

/// The settings that decide where a key goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cfg {
    pub shift: i32,
    pub lowest: i32,
    /// Last key of the range (may lie past 127 when Width reaches over).
    pub highest: i32,
    pub block_outside: bool,
    pub overflow: usize,
    pub layer: bool,
    /// Velocity of the layered note, 0..2.
    pub level: f32,
}

impl Cfg {
    fn of(dev: &Dev) -> Self {
        let lowest = dev.plain(p::LOWEST).round() as i32;
        Self {
            shift: dev.plain(p::OCTAVE).round() as i32 * 12 + dev.plain(p::SEMITONE).round() as i32,
            lowest,
            highest: lowest + dev.plain(p::WIDTH).round() as i32 - 1,
            block_outside: dev.choice(p::OUTSIDE) == 1,
            overflow: dev.choice(p::OVERFLOW),
            layer: dev.on(p::LAYER),
            level: dev.plain(p::LAYER_LEVEL) / 100.,
        }
    }

    pub fn affects(&self, key: i32) -> bool {
        (self.lowest..=self.highest).contains(&key)
    }
}

/// What becomes of a played key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fate {
    /// Sounds where it was played.
    Same,
    /// Shifted to this key.
    Moved(u8),
    /// Shifted past the keyboard and brought back (folded or held at the edge).
    Wrapped(u8),
    /// Dropped.
    Blocked,
}

/// What the plugin does with `key`.
pub fn fate(c: &Cfg, key: i32) -> Fate {
    if !c.affects(key) {
        return if c.block_outside { Fate::Blocked } else { Fate::Same };
    }
    let k = key + c.shift;
    if (0..=127).contains(&k) {
        return if k == key { Fate::Same } else { Fate::Moved(k as u8) };
    }
    match c.overflow {
        BLOCK => Fate::Blocked,
        FOLD => Fate::Wrapped(fold_octaves(k)),
        _ => Fate::Wrapped(k.clamp(0, 127) as u8),
    }
}

/// Geometry of the two rows for a plot of this height: `(row height, top of the lower row)`.
fn rows(h: f32) -> (f32, f32) {
    let rh = ((h - TOP - BOTTOM) * 0.3).clamp(12., 28.);
    (rh, h - BOTTOM - rh)
}

fn headline(c: &Cfg) -> String {
    let range = if c.lowest == 0 && c.highest >= 127 { "all keys".to_owned() } else { format!("{}–{}", mk::note_name(c.lowest), mk::note_name(c.highest.min(127))) };
    if c.shift == 0 { format!("no shift · {range}") } else { format!("{:+} st · {range}", c.shift) }
}

fn tail(c: &Cfg) -> String {
    let overflow = ["blocked", "folded", "held"][c.overflow.min(2)];
    let mut text = format!("outside {} · beyond the keys {overflow}", if c.block_outside { "blocked" } else { "passes" });
    if c.layer {
        text += &format!(" · layer {:.0}%", c.level * 100.);
    }
    text
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let c = Cfg::of(dev);
    let h = mk::plot_h(dev);
    let (rh, y2) = rows(h);
    // The plot's width as last painted (a first guess before that).
    let seen = f32::from(ws.fx.region(dev.id, 0).get().size.width);
    let w = if seen > 100. { seen } else if dev.big { 580. } else { 500. };
    let cw = (w - PAD_L - PAD_R) / 128.;
    let key_frac = |key: f32| (PAD_L + key * cw) / w;
    let per_key = w / cw / 127.;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let last = c.highest.min(127);
    let mid = (c.lowest + last) as f32 / 2. + c.shift as f32;
    let handles = vec![
        Handle { fx: key_frac(c.lowest as f32), fy: (TOP + rh / 2.) / h, x: ax(p::LOWEST, per_key), index: 0, reset: vec![default(p::LOWEST)], ..Default::default() },
        Handle { fx: key_frac((last + 1) as f32), fy: (TOP + rh / 2.) / h, x: ax(p::WIDTH, per_key), index: 1, reset: vec![default(p::WIDTH)], ..Default::default() },
        Handle {
            fx: key_frac(mid.clamp(0., 127.) + 0.5),
            fy: (TOP + rh + (y2 - TOP - rh) / 2.) / h,
            x: ax(p::SEMITONE, w / cw / 24.),
            alt: ax(p::OCTAVE, -1.),
            index: 2,
            reset: vec![default(p::SEMITONE), default(p::OCTAVE)],
            ..Default::default()
        },
    ];
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let (left, right, enabled) = (headline(&c), tail(&c), dev.enabled);

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let lit = |c: Hsla| if enabled { c } else { pal.muted };
        let (rh, y2) = rows(r.h);
        let (x0, cw) = (r.x + PAD_L, (r.w - PAD_L - PAD_R) / 128.);
        let col = |k: i32| x0 + k as f32 * cw;
        let (top, low) = (r.y + TOP, r.y + y2);
        mk::corner(window, cx, r, &left, false, 3.);
        mk::corner(window, cx, r, &right, true, 3.);
        kit::txt(window, cx, "in", r.x + 6., top + rh / 2. - 6., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "out", r.x + 6., low + rh / 2. - 6., TEXT_XS, pal.faint, TextAlign::Left);
        // Which keys sound after the shift, and how each got there.
        let mut moved = [false; 128];
        let mut wrapped = [false; 128];
        let mut kept = [false; 128];
        for key in 0..128 {
            match fate(&c, key) {
                Fate::Same => kept[key as usize] = true,
                Fate::Moved(k) => moved[k as usize] = true,
                Fate::Wrapped(k) => wrapped[k as usize] = true,
                Fate::Blocked => {}
            }
            if c.layer && c.affects(key) {
                kept[key as usize] = true;
            }
        }
        for key in 0..128i32 {
            let black = mk::is_black(key);
            let base = content(if black { 0.05 } else { 0.11 });
            let (x, wd) = (col(key), (cw - 0.6).max(1.));
            // The row of played keys: the shifted range lit, blocked keys faded.
            let played = if c.affects(key) { with_alpha(lit(pal.accent), if black { 0.32 } else { 0.5 }) } else if c.block_outside { content(0.04) } else { base };
            kit::bar(window, x, top, wd, rh, 0., played);
            // The row of sounding keys; a layer splits it: the played note above, the added one below.
            let k = key as usize;
            let mut sounding = base;
            let mut added = None;
            if kept[k] {
                sounding = with_alpha(pal.text, 0.3);
            }
            if moved[k] {
                let colour = with_alpha(lit(pal.accent), if c.layer { 0.3 + 0.4 * c.level.min(1.) } else { 0.6 });
                if c.layer { added = Some(colour) } else { sounding = colour }
            }
            if wrapped[k] {
                let colour = with_alpha(lit(pal.accent2), 0.7);
                if c.layer { added = Some(colour) } else { sounding = colour }
            }
            match added {
                Some(colour) => {
                    kit::bar(window, x, low, wd, rh / 2., 0., sounding);
                    kit::bar(window, x, low + rh / 2., wd, rh / 2., 0., colour);
                }
                None => kit::bar(window, x, low, wd, rh, 0., sounding),
            }
        }
        // The octaves.
        for note in (0..128).step_by(12) {
            kit::txt(window, cx, &mk::note_name(note), col(note), r.bottom() - 13., TEXT_XS, pal.faint, TextAlign::Left);
        }
        // One line per key (every other key when they would crowd).
        let stride = (7. / cw).ceil().max(1.) as usize;
        let (from_y, to_y) = (top + rh, low);
        let mid_y = (from_y + to_y) / 2.;
        for key in (0..128i32).step_by(stride) {
            let x_in = col(key) + cw / 2.;
            let to = |k: u8| col(k as i32) + cw / 2.;
            let straight = |window: &mut gpui::Window, alpha: f32| kit::line(window, &[point(px(x_in), px(from_y)), point(px(x_in), px(to_y))], 1., with_alpha(pal.text, alpha));
            let layered = c.layer && c.affects(key);
            if layered {
                straight(window, 0.22);
            }
            match fate(&c, key) {
                Fate::Same if !layered => straight(window, if c.affects(key) { 0.5 } else { 0.18 }),
                Fate::Same => {}
                Fate::Moved(k) => kit::line(window, &[point(px(x_in), px(from_y)), point(px(to(k)), px(to_y))], 1., with_alpha(lit(pal.accent), 0.5)),
                Fate::Wrapped(k) => kit::line(window, &[point(px(x_in), px(from_y)), point(px(to(k)), px(to_y))], 1., with_alpha(lit(pal.accent2), 0.55)),
                Fate::Blocked if c.affects(key) => {
                    // A stub that stops short, with a cross at its end.
                    let end_x = x_in + (c.shift.signum() as f32) * (to_y - from_y) * 0.18;
                    kit::line(window, &[point(px(x_in), px(from_y)), point(px(end_x), px(mid_y))], 1., with_alpha(pal.record, 0.7));
                    kit::bar(window, end_x - 2., mid_y - 0.5, 5., 1.5, 0., pal.record);
                    kit::bar(window, end_x - 0.5, mid_y - 2.5, 1.5, 5., 0., pal.record);
                }
                Fate::Blocked => {}
            }
        }
        for (i, (fx, fy)) in marks.iter().enumerate() {
            kit::dot(window, r.pt(*fx, *fy), if i == 2 { 6. } else { 5. }, lit(if i == 2 { pal.accent2 } else { pal.accent }), Some(pal.raised));
        }
    });

    let shift = vec![kit::ctl(ws, dev, p::OCTAVE, cx), kit::ctl(ws, dev, p::SEMITONE, cx)];
    let range = vec![kit::ctl(ws, dev, p::LOWEST, cx), kit::ctl(ws, dev, p::WIDTH, cx), kit::ctl(ws, dev, p::OUTSIDE, cx)];
    let beyond = vec![kit::ctl(ws, dev, p::OVERFLOW, cx)];
    let layer = vec![kit::ctl(ws, dev, p::LAYER_LEVEL, cx)];
    let cards = vec![
        kit::section(dev, "SHIFT", None, shift, cx),
        kit::section(dev, "RANGE", None, range, cx),
        kit::section(dev, "BEYOND THE KEYS", None, beyond, cx),
        kit::section(dev, "LAYER", Some(p::LAYER), layer, cx),
    ];
    mk::body(dev, "pitch-body", 520., plot, cards)
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    fn cfg(f: impl Fn(&mut Cfg)) -> Cfg {
        let mut c = Cfg { shift: 7, lowest: 36, highest: 71, block_outside: false, overflow: FOLD, layer: false, level: 1. };
        f(&mut c);
        c
    }

    #[test]
    fn keys_in_the_range_move_and_the_rest_follow_outside() {
        let c = cfg(|_| {});
        assert_eq!(fate(&c, 40), Fate::Moved(47));
        assert_eq!(fate(&c, 35), Fate::Same);
        assert_eq!(fate(&c, 72), Fate::Same);
        assert_eq!(fate(&cfg(|c| c.block_outside = true), 35), Fate::Blocked);
        assert_eq!(fate(&cfg(|c| c.shift = 0), 40), Fate::Same);
    }

    #[test]
    fn what_leaves_the_keyboard_is_folded_held_or_blocked() {
        let up = |overflow| cfg(|c| {
            c.shift = 24;
            c.lowest = 100;
            c.highest = 127;
            c.overflow = overflow;
        });
        assert_eq!(fate(&up(FOLD), 120), Fate::Wrapped(fold_octaves(144)));
        assert_eq!(fate(&up(2), 120), Fate::Wrapped(127));
        assert_eq!(fate(&up(BLOCK), 120), Fate::Blocked);
        assert_eq!(fate(&up(BLOCK), 100), Fate::Moved(124), "still on the keyboard");
        assert_eq!(fate(&cfg(|c| {
            c.shift = -24;
            c.lowest = 0;
            c.overflow = 2;
        }), 10), Fate::Wrapped(0));
    }

    #[test]
    fn the_range_can_reach_past_the_last_key() {
        let d = dev("midi.pitch", &[(p::LOWEST, 100.), (p::WIDTH, 100.)]);
        let c = Cfg::of(&d);
        assert_eq!((c.lowest, c.highest), (100, 199));
        assert!(c.affects(127) && !c.affects(99));
        assert_eq!(headline(&c), "no shift · E7–G9");
    }

    #[test]
    fn the_settings_read_as_octaves_and_semitones() {
        let d = dev("midi.pitch", &[(p::OCTAVE, 2.), (p::SEMITONE, -3.), (p::LAYER, 1.), (p::LAYER_LEVEL, 80.)]);
        let c = Cfg::of(&d);
        assert_eq!((c.shift, c.layer, c.level), (21, true, 0.8));
        assert!(tail(&c).contains("layer 80%"));
        assert_eq!(headline(&Cfg::of(&dev("midi.pitch", &[]))), "no shift · all keys");
    }

    #[test]
    fn both_rows_fit_the_plot() {
        for h in [mk::PLOT_H - 2., mk::PLOT_H_BIG - 2.] {
            let (rh, y2) = rows(h);
            assert!(y2 > TOP + rh + 8. && y2 + rh <= h - 14., "{h}");
        }
    }
}
