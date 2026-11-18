//! The project colour palette: a grid of hue columns by lightness rows that
//! every colour picker (tracks, clips, rack chains) offers, plus the pure
//! helpers behind "Assign Track Color to Clips" and "Randomize Colors".
//! The commands they return are ordinary `UpdateClip` / `SetTrack` ones.

use crate::command::{Command, TrackProp};
use crate::ids::{ClipId, TrackId};
use crate::project::{Clip, Color, Project};
use std::collections::BTreeSet;

/// Hue columns of the grid (the last column is the greys).
pub const GRID_COLS: usize = 14;
/// Lightness rows of the grid, light to dark.
pub const GRID_ROWS: usize = 5;
/// How many swatches the grid has.
pub const GRID_LEN: usize = GRID_COLS * GRID_ROWS;

/// Lightness of each row, light to dark.
const ROW_LIGHTNESS: [f32; GRID_ROWS] = [0.80, 0.69, 0.60, 0.50, 0.38];
/// Saturation of each row (the lightest rows are a touch softer).
const ROW_SATURATION: [f32; GRID_ROWS] = [0.70, 0.74, 0.72, 0.66, 0.58];

impl Color {
    /// A colour from hue (degrees), saturation and lightness (0..=1).
    pub fn from_hsl(hue: f32, s: f32, l: f32) -> Color {
        let h = hue.rem_euclid(360.) / 60.;
        let c = (1. - (2. * l - 1.).abs()) * s;
        let x = c * (1. - (h % 2. - 1.).abs());
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.),
            1 => (x, c, 0.),
            2 => (0., c, x),
            3 => (0., x, c),
            4 => (x, 0., c),
            _ => (c, 0., x),
        };
        let m = l - c / 2.;
        let q = |v: f32| ((v + m).clamp(0., 1.) * 255.).round() as u32;
        Color(q(r) << 16 | q(g) << 8 | q(b))
    }

    /// The swatch at `row` (0 = lightest) and `col` of the palette grid;
    /// out-of-range indices wrap.
    pub fn grid(row: usize, col: usize) -> Color {
        let (row, col) = (row % GRID_ROWS, col % GRID_COLS);
        if col == GRID_COLS - 1 {
            // Greys, lightest to darkest.
            return Color::from_hsl(220., 0.05, [0.90, 0.72, 0.56, 0.40, 0.26][row]);
        }
        // Thirteen hues, starting at a warm red and walking round the wheel.
        let hue = 4. + col as f32 * (360. / (GRID_COLS - 1) as f32);
        Color::from_hsl(hue, ROW_SATURATION[row], ROW_LIGHTNESS[row])
    }

    /// Swatch `i` of the grid in row-major order.
    pub fn grid_index(i: usize) -> Color {
        Color::grid(i / GRID_COLS, i % GRID_COLS)
    }
}

/// Small deterministic generator (SplitMix64) so randomised colours are
/// reproducible from a seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn hue_pool() -> Vec<usize> {
    (0..GRID_LEN).filter(|i| i % GRID_COLS != GRID_COLS - 1).collect()
}

/// `n` colours from the grid's hue columns (no greys) in a random order,
/// all different while `n` fits the grid, from `seed`.
pub fn random_colors(seed: u64, n: usize) -> Vec<Color> {
    let mut rng = Rng(seed);
    let mut pool = hue_pool();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        if pool.is_empty() {
            pool = hue_pool();
        }
        // The middle rows read best; the lightest and darkest get a second draw.
        let mut k = rng.below(pool.len());
        if matches!(pool[k] / GRID_COLS, 0 | 4) {
            k = rng.below(pool.len());
        }
        out.push(Color::grid_index(pool.swap_remove(k)));
    }
    out
}

/// "Assign Track Color to Clips": every clip placed on one of `tracks`
/// takes the colour of the track it is on (a clip on several tracks follows
/// the first). Empty when nothing would change.
pub fn assign_track_color_to_clips(p: &Project, tracks: &[TrackId]) -> Vec<Command> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for t in tracks.iter().filter_map(|id| p.track(*id)) {
        let clips = t.arrangement.iter().map(|pl| pl.clip).chain(t.session_slots.values().copied());
        for id in clips {
            if let Some(c) = p.clips.get(&id).filter(|c| c.color != t.color)
                && seen.insert(id)
            {
                out.push(Command::UpdateClip { id, clip: Box::new(Clip { color: t.color, ..c.clone() }) });
            }
        }
    }
    out
}

/// "Randomize Colors" for clips: each gets its own random swatch.
pub fn randomize_clips(p: &Project, clips: &[ClipId], seed: u64) -> Vec<Command> {
    let ids: Vec<ClipId> = clips.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let colors = random_colors(seed, ids.len());
    ids.into_iter()
        .zip(colors)
        .filter_map(|(id, color)| p.clips.get(&id).filter(|c| c.color != color).map(|c| Command::UpdateClip { id, clip: Box::new(Clip { color, ..c.clone() }) }))
        .collect()
}

/// "Randomize Colors" for tracks: the new colour per track.
pub fn randomize_tracks(p: &Project, tracks: &[TrackId], seed: u64) -> Vec<(TrackId, Color)> {
    let ids: Vec<TrackId> = tracks.iter().copied().filter(|t| p.track(*t).is_some()).collect();
    ids.iter().copied().zip(random_colors(seed, ids.len())).collect()
}

/// `SetTrack` colour commands for the `(track, colour)` pairs that change something.
pub fn track_color_commands(p: &Project, colors: &[(TrackId, Color)]) -> Vec<Command> {
    colors
        .iter()
        .filter_map(|(id, color)| p.track(*id).filter(|t| t.color != *color).map(|_| Command::SetTrack { id: *id, change: TrackProp::Color(*color) }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{NoteSource, Placement, Source, Track, TrackKind};

    #[test]
    fn grid_has_seventy_distinct_swatches() {
        let all: BTreeSet<u32> = (0..GRID_LEN).map(|i| Color::grid_index(i).0).collect();
        assert_eq!(GRID_LEN, 70);
        assert_eq!(all.len(), GRID_LEN);
        // Lightness falls down each column.
        let lum = |c: Color| ((c.0 >> 16 & 0xFF) * 3 + (c.0 >> 8 & 0xFF) * 6 + (c.0 & 0xFF)) as i32;
        for col in 0..GRID_COLS {
            for row in 1..GRID_ROWS {
                assert!(lum(Color::grid(row, col)) < lum(Color::grid(row - 1, col)), "col {col} row {row}");
            }
        }
    }

    #[test]
    fn hsl_primaries() {
        assert_eq!(Color::from_hsl(0., 1., 0.5), Color(0xFF0000));
        assert_eq!(Color::from_hsl(120., 1., 0.5), Color(0x00FF00));
        assert_eq!(Color::from_hsl(240., 1., 0.5), Color(0x0000FF));
        assert_eq!(Color::from_hsl(480., 1., 0.5), Color(0x00FF00));
        assert_eq!(Color::from_hsl(77., 0., 1.), Color(0xFFFFFF));
    }

    #[test]
    fn random_colors_are_reproducible_and_distinct() {
        let a = random_colors(42, 20);
        assert_eq!(a, random_colors(42, 20));
        assert_ne!(a, random_colors(43, 20));
        assert_eq!(a.iter().map(|c| c.0).collect::<BTreeSet<_>>().len(), 20);
        assert!(a.iter().all(|c| (0..GRID_LEN).any(|i| Color::grid_index(i) == *c)));
        assert_eq!(random_colors(1, 200).len(), 200);
    }

    #[test]
    fn clips_take_their_tracks_colour() {
        let mut p = Project::new("t");
        let id = TrackId(p.ids.next_raw());
        let mut t = Track::new(id, TrackKind::Instrument, "Keys", Color(0x112233));
        let src = crate::SourceId(p.ids.next_raw());
        p.sources.insert(src, Source::Notes(NoteSource::default()));
        let (c1, c2) = (ClipId(p.ids.next_raw()), ClipId(p.ids.next_raw()));
        p.clips.insert(c1, Clip::new("a", Color(0x112233), src, 960));
        p.clips.insert(c2, Clip::new("b", Color(0x445566), src, 960));
        t.arrangement.push(Placement { id: crate::PlacementId(p.ids.next_raw()), clip: c1, start: 0, lane: 0 });
        t.arrangement.push(Placement { id: crate::PlacementId(p.ids.next_raw()), clip: c2, start: 960, lane: 0 });
        p.tracks.insert(0, t);
        let cmds = assign_track_color_to_clips(&p, &[id]);
        assert_eq!(cmds.len(), 1);
        for c in &cmds {
            c.clone().apply(&mut p).unwrap();
        }
        assert_eq!(p.clips[&c2].color, Color(0x112233));
        assert!(assign_track_color_to_clips(&p, &[id]).is_empty());
        let rnd = randomize_clips(&p, &[c1, c2, c1], 7);
        assert_eq!(rnd.len(), 2);
        let tr = randomize_tracks(&p, &[id], 7);
        assert_eq!(track_color_commands(&p, &tr).len(), 1);
    }
}
