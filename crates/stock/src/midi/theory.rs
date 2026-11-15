//! Scale tables and note snapping shared by the MIDI devices.
//!
//! The scale list and its order match `aitrack_model::ScaleKind::ALL`, so a
//! scale index means the same thing in the project and in a device
//! parameter (the stock crate does not depend on the model; the order is
//! part of the saved-parameter contract and must not change).

pub const ROOT_NAMES: &[&str] = &["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub const SCALE_NAMES: &[&str] = &[
    "Major",
    "Minor",
    "Harmonic Minor",
    "Melodic Minor",
    "Dorian",
    "Phrygian",
    "Lydian",
    "Mixolydian",
    "Locrian",
    "Major Pentatonic",
    "Minor Pentatonic",
    "Blues",
    "Chromatic",
];

/// Semitone offsets from the root, in `SCALE_NAMES` order.
pub const SCALE_INTERVALS: [&[u8]; 13] = [
    &[0, 2, 4, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 8, 10],
    &[0, 2, 3, 5, 7, 8, 11],
    &[0, 2, 3, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 9, 10],
    &[0, 1, 3, 5, 7, 8, 10],
    &[0, 2, 4, 6, 7, 9, 11],
    &[0, 2, 4, 5, 7, 9, 10],
    &[0, 1, 3, 5, 6, 8, 10],
    &[0, 2, 4, 7, 9],
    &[0, 3, 5, 7, 10],
    &[0, 3, 5, 6, 7, 10],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
];

/// 12-bit mask of the pitch classes of scale `kind` on `root` (bit 0 = C),
/// the same layout as `aitrack_model::Scale::mask`.
pub fn scale_mask(kind: usize, root: usize) -> u16 {
    SCALE_INTERVALS[kind.min(SCALE_INTERVALS.len() - 1)].iter().fold(0u16, |m, i| m | 1 << ((root + *i as usize) % 12))
}

#[inline]
pub fn in_mask(mask: u16, note: i32) -> bool {
    mask & (1 << note.rem_euclid(12)) != 0
}

/// Which way to move a note that is outside the scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Nearest,
    Up,
    Down,
}

/// First in-scale note from `from` stepping by `dir` (+1 / -1), staying in 0..=127.
fn search(mask: u16, from: i32, dir: i32) -> Option<i32> {
    let mut n = from;
    while (0..=127).contains(&n) {
        if in_mask(mask, n) {
            return Some(n);
        }
        n += dir;
    }
    None
}

/// Move `note` into the scale `mask`.
///
/// * [`Snap::Nearest`]: the closest in-scale note; on a tie the lower one.
/// * [`Snap::Up`] / [`Snap::Down`]: the first in-scale note in that
///   direction. With `strict`, a note that is already inside also moves, to
///   the *next* degree (a diatonic step).
///
/// When nothing lies that way before the end of the MIDI range the other way
/// is tried; an empty mask leaves the note alone. The result is always 0..=127.
pub fn snap_note(mask: u16, note: u8, snap: Snap, strict: bool) -> u8 {
    if mask == 0 {
        return note;
    }
    let n = note as i32;
    let found = match snap {
        Snap::Nearest => {
            let down = search(mask, n, -1);
            let up = search(mask, n, 1);
            match (down, up) {
                (Some(d), Some(u)) => Some(if n - d <= u - n { d } else { u }),
                (d, u) => d.or(u),
            }
        }
        Snap::Up => {
            let start = n + i32::from(strict);
            search(mask, start, 1).or_else(|| search(mask, n, -1))
        }
        Snap::Down => {
            let start = n - i32::from(strict);
            search(mask, start, -1).or_else(|| search(mask, n, 1))
        }
    };
    found.unwrap_or(n) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_match_the_model_layout() {
        // C major: C D E F G A B.
        assert_eq!(scale_mask(0, 0), 0b1010_1011_0101);
        // A minor has the same notes.
        assert_eq!(scale_mask(1, 9), scale_mask(0, 0));
        assert_eq!(scale_mask(12, 5), 0x0FFF);
        assert_eq!(SCALE_NAMES.len(), SCALE_INTERVALS.len());
    }

    /// The tables are a copy of the model's: same scales, same order, same
    /// masks and the same snapping.
    #[test]
    fn tables_match_the_model() {
        use aitrack_model::{Key, Scale, ScaleKind};
        assert_eq!(SCALE_NAMES.len(), ScaleKind::ALL.len());
        for (i, kind) in ScaleKind::ALL.iter().enumerate() {
            assert_eq!(SCALE_NAMES[i], kind.label(), "name of scale {i}");
            assert_eq!(SCALE_INTERVALS[i], kind.intervals(), "intervals of {}", kind.label());
            for root in 0..12u8 {
                let model = Scale::new(Key(root), *kind);
                assert_eq!(scale_mask(i, root as usize), model.mask());
                for note in 0..=127u8 {
                    // The model snaps within six semitones, preferring down.
                    let a = snap_note(model.mask(), note, Snap::Nearest, false);
                    assert_eq!(a, model.snap(note), "{} {} note {note}", Key(root).name(), kind.label());
                }
            }
        }
        for (i, name) in ROOT_NAMES.iter().enumerate() {
            assert_eq!(Key(i as u8).name(), *name);
        }
    }

    #[test]
    fn nearest_prefers_down_on_ties() {
        let c_major = scale_mask(0, 0);
        assert_eq!(snap_note(c_major, 61, Snap::Nearest, false), 60, "C# is one from C and one from D: lower");
        assert_eq!(snap_note(c_major, 63, Snap::Nearest, false), 62, "D# ties D and E: lower");
        assert_eq!(snap_note(c_major, 66, Snap::Nearest, false), 65, "F# ties F and G: lower");
        assert_eq!(snap_note(c_major, 64, Snap::Nearest, false), 64);
        // A minor pentatonic: A C D E G. B is 2 from A and 1 from C.
        let pent = scale_mask(10, 9);
        assert_eq!(snap_note(pent, 71, Snap::Nearest, false), 72);
        assert_eq!(snap_note(pent, 65, Snap::Nearest, false), 64, "F is 1 from E, 2 from G");
    }

    #[test]
    fn up_and_down_and_strict_steps() {
        let c_major = scale_mask(0, 0);
        assert_eq!(snap_note(c_major, 61, Snap::Up, false), 62);
        assert_eq!(snap_note(c_major, 61, Snap::Down, false), 60);
        assert_eq!(snap_note(c_major, 60, Snap::Up, false), 60, "inside stays");
        assert_eq!(snap_note(c_major, 60, Snap::Up, true), 62, "strict: next degree");
        assert_eq!(snap_note(c_major, 60, Snap::Down, true), 59);
        assert_eq!(snap_note(c_major, 64, Snap::Up, true), 65);
    }

    #[test]
    fn edges_of_the_midi_range_fall_back_the_other_way() {
        let only_c = 1u16;
        assert_eq!(snap_note(only_c, 127, Snap::Up, false), 120);
        assert_eq!(snap_note(only_c, 0, Snap::Down, true), 0, "C0 is the only C below: strict has nowhere to go but back");
        assert_eq!(snap_note(0, 77, Snap::Nearest, false), 77, "empty mask: untouched");
        for n in 0..=127u8 {
            for s in [Snap::Nearest, Snap::Up, Snap::Down] {
                let o = snap_note(scale_mask(11, 3), n, s, true);
                assert!(o <= 127);
            }
        }
    }
}
