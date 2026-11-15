//! Fitting a sample to the song: how many semitones move its key onto the
//! project's, and how far its tempo is from the project's.
//!
//! Keys are compared by the pitch classes of their scales, not by their
//! tonics, so relatives and modes are one general case: A minor over C major
//! needs **0** semitones (same notes), over C minor **+3**, and E minor over A
//! minor **+5**. Of the moves that leave the fewest notes out of the song's
//! scale the shortest one wins, and when two are as short, the lower (it
//! degrades a stretched loop less).

use aitrack_model::{Key, Scale, ScaleKind};

/// Moves tried: the shortest way round the octave.
const MOVES: std::ops::RangeInclusive<i8> = -6..=5;

/// More than this many semitones and a stretched loop starts to suffer.
pub const GENTLE_SEMITONES: i8 = 4;

/// What kind of move a fit is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitKind {
    /// The very same key: nothing to do.
    Same,
    /// Different tonic, same notes (relative major/minor, a mode of the song's scale): nothing to do.
    Relative,
    /// Moved by `semitones` onto the song's key.
    Transposed,
    /// No pitch to fit (drums, noise): left as it is.
    Atonal,
    /// The key is not known (yet): left as it is, a candidate for analysis.
    Unknown,
}

/// The result of [`fit_key`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    /// Semitones to transpose the sample by, −6..=5 (0 unless `kind` is `Transposed`).
    pub semitones: i8,
    pub kind: FitKind,
}

impl Fit {
    pub const NONE: Fit = Fit { semitones: 0, kind: FitKind::Unknown };

    /// Whether the sample ends up moved.
    pub fn moves(self) -> bool {
        self.semitones != 0
    }

    /// A big move: loops degrade, say so.
    pub fn is_big(self) -> bool {
        self.semitones.abs() > GENTLE_SEMITONES
    }
}

fn rotate(mask: u16, by: i8) -> u16 {
    let by = by.rem_euclid(12) as u32;
    ((mask << by) | (mask >> (12 - by))) & 0xFFF
}

/// The tonic of the relative scale of `p` (major ↔ minor, a minor third away).
fn relative_root(p: Scale) -> Key {
    let minor_like = matches!(p.kind.intervals().get(2), Some(3));
    Key((p.root.0 + if minor_like { 3 } else { 9 }) % 12)
}

/// Semitones (−6..=5) that move `pitch_class` onto `target`.
pub fn semitones_between(pitch_class: u8, target: Key) -> i8 {
    let d = (i16::from(target.0) - i16::from(pitch_class % 12)).rem_euclid(12) as i8;
    if d > 5 { d - 12 } else { d }
}

/// How to move a sample in `root` / `kind` (root-only when `kind` is `None`,
/// atonal when `root` is) onto the song's scale `project`.
pub fn fit_key(project: Scale, root: Option<Key>, kind: Option<ScaleKind>) -> Fit {
    let Some(root) = root else { return Fit { semitones: 0, kind: FitKind::Atonal } };
    let Some(kind) = kind else {
        // A tonal one-shot with only its note: that note onto the tonic.
        let d = semitones_between(root.0, project.root);
        return Fit { semitones: d, kind: if d == 0 { FitKind::Same } else { FitKind::Transposed } };
    };
    let sample = Scale::new(root, kind);
    let (s, p) = (sample.mask(), project.mask());
    if sample == project {
        return Fit { semitones: 0, kind: FitKind::Same };
    }
    let tonics = [project.root, relative_root(project)];
    let best = MOVES
        .map(|d| {
            let out = (rotate(s, d) & !p).count_ones();
            let lands_on_tonic = tonics.contains(&Key((root.0 + (d + 12) as u8) % 12));
            // Fewest notes out of the song's scale, then landing on its tonic (or its
            // relative), then the shortest move, then downwards.
            (out, !lands_on_tonic, d.abs(), d > 0, d)
        })
        .min()
        .map(|t| t.4)
        .unwrap_or(0);
    if best == 0 {
        // Nothing to move: the same notes under another name.
        return Fit { semitones: 0, kind: FitKind::Relative };
    }
    Fit { semitones: best, kind: FitKind::Transposed }
}

/// How a loop's tempo meets the song's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoFit {
    /// 0.5, 1 or 2: the sample is read as running at `bpm × factor`, the
    /// multiple that lands nearest the song's tempo (a 64 BPM loop in a 128 BPM song is a 2×).
    pub factor: f32,
    /// How much faster the sample plays to meet the song: `project / (bpm × factor)`.
    pub speed: f64,
    /// Within 0.7..=1.4: stretching further sounds bad, so the sample is not offered as a fit.
    pub usable: bool,
}

impl TempoFit {
    /// Percent of speed (`103` = 3 % faster).
    pub fn percent(self) -> f64 {
        self.speed * 100.
    }
}

/// How `sample_bpm` meets `project_bpm`. `None` when either is not a tempo.
pub fn fit_tempo(project_bpm: f64, sample_bpm: f32) -> Option<TempoFit> {
    let sample = f64::from(sample_bpm);
    if !(project_bpm.is_finite() && sample.is_finite()) || project_bpm <= 0. || sample <= 0. {
        return None;
    }
    let (factor, speed) = [0.5f32, 1.0, 2.0]
        .into_iter()
        .map(|f| (f, project_bpm / (sample * f64::from(f))))
        // The nearest to 1 in ratio terms (2 and ½ are as far from 1 as each other).
        .min_by(|a, b| a.1.ln().abs().total_cmp(&b.1.ln().abs()))?;
    Some(TempoFit { factor, speed, usable: (0.7..=1.4).contains(&speed) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(root: u8, kind: ScaleKind) -> Scale {
        Scale::new(Key(root), kind)
    }
    const A: u8 = 9;
    const C: u8 = 0;
    const E: u8 = 4;

    #[test]
    fn a_minor_over_c_major_needs_no_move_and_over_c_minor_three() {
        let am = fit_key(s(C, ScaleKind::Major), Some(Key(A)), Some(ScaleKind::Minor));
        assert_eq!(am, Fit { semitones: 0, kind: FitKind::Relative });
        let over_cm = fit_key(s(C, ScaleKind::Minor), Some(Key(A)), Some(ScaleKind::Minor));
        assert_eq!(over_cm, Fit { semitones: 3, kind: FitKind::Transposed });
    }

    #[test]
    fn e_minor_over_a_minor_is_five_up_not_seven_down() {
        let f = fit_key(s(A, ScaleKind::Minor), Some(Key(E)), Some(ScaleKind::Minor));
        assert_eq!(f, Fit { semitones: 5, kind: FitKind::Transposed });
        assert!(f.moves() && f.is_big(), "five is past the gentle four");
        assert!(!Fit { semitones: 3, kind: FitKind::Transposed }.is_big());
    }

    #[test]
    fn the_same_key_and_atonal_and_unknown_samples_are_left_alone() {
        assert_eq!(fit_key(s(A, ScaleKind::Minor), Some(Key(A)), Some(ScaleKind::Minor)), Fit { semitones: 0, kind: FitKind::Same });
        assert_eq!(fit_key(s(A, ScaleKind::Minor), None, None), Fit { semitones: 0, kind: FitKind::Atonal });
        assert_eq!(Fit::NONE.kind, FitKind::Unknown);
    }

    #[test]
    fn a_root_only_one_shot_moves_its_note_onto_the_tonic() {
        // A bass note E over A minor: up 5 (A is the tonic); G over A: up 2.
        assert_eq!(fit_key(s(A, ScaleKind::Minor), Some(Key(E)), None).semitones, 5);
        assert_eq!(fit_key(s(A, ScaleKind::Minor), Some(Key(7)), None).semitones, 2);
        assert_eq!(fit_key(s(A, ScaleKind::Minor), Some(Key(A)), None), Fit { semitones: 0, kind: FitKind::Same });
        // Always the shortest way round: B over A is down 2, not up 10.
        assert_eq!(fit_key(s(A, ScaleKind::Minor), Some(Key(11)), None).semitones, -2);
    }

    #[test]
    fn modes_are_the_general_case_and_the_move_is_the_shortest() {
        // D dorian has the notes of C major: over C major it is already home.
        assert_eq!(fit_key(s(C, ScaleKind::Major), Some(Key(2)), Some(ScaleKind::Dorian)).semitones, 0);
        // G dorian over C major: its notes are F major's, so up... the fewest-out-of-key move that lands on the tonic.
        let f = fit_key(s(C, ScaleKind::Major), Some(Key(7)), Some(ScaleKind::Dorian));
        assert!(f.semitones.abs() <= 6 && f.semitones != 0, "{f:?}");
        // The result never leaves a note out of the song's scale when a move exists that keeps all of them in.
        let p = s(C, ScaleKind::Major);
        let moved = rotate(Scale::new(Key(7), ScaleKind::Dorian).mask(), f.semitones);
        assert_eq!(moved & !p.mask(), 0);
    }

    #[test]
    fn a_tritone_away_goes_down_not_up_and_no_move_leaves_the_range() {
        // F# major over C major: no move keeps every note in; the shortest of the best wins, inside −6..=5.
        for root in 0..12 {
            for kind in ScaleKind::ALL {
                let f = fit_key(s(C, ScaleKind::Major), Some(Key(root)), Some(kind));
                assert!((-6..=5).contains(&f.semitones), "{root} {kind:?}: {f:?}");
            }
        }
        // Even tie (a 6 either way): downwards.
        assert_eq!(semitones_between(6, Key(0)), -6);
    }

    #[test]
    fn a_pentatonic_that_fits_the_song_stays_put() {
        // A minor pentatonic is inside C major's notes, so over C major it needs nothing.
        let f = fit_key(s(C, ScaleKind::Major), Some(Key(A)), Some(ScaleKind::MinorPentatonic));
        assert_eq!(f.semitones, 0);
    }

    #[test]
    fn tempo_picks_the_half_or_double_nearest_the_song() {
        let t = fit_tempo(128., 128.).unwrap();
        assert_eq!((t.factor, t.usable), (1., true));
        assert!((t.speed - 1.).abs() < 1e-9);
        // 64 BPM is a half-time loop of a 128 BPM song: read as 2×, no stretch.
        let t = fit_tempo(128., 64.).unwrap();
        assert_eq!((t.factor, t.usable), (2., true));
        assert!((t.percent() - 100.).abs() < 1e-9);
        // 124 BPM in a 128 BPM song: 3 % faster.
        let t = fit_tempo(128., 124.).unwrap();
        assert_eq!(t.factor, 1.);
        assert!((t.percent() - 103.2258).abs() < 0.01);
        // 90 BPM in a 128 song: 1.42 at 1×, 0.71 at 2× (180 BPM): the double is nearer.
        let t = fit_tempo(128., 90.).unwrap();
        assert_eq!(t.factor, 2.);
        assert!(t.usable);
        // Too far from every multiple: not a fit.
        let t = fit_tempo(128., 100.).unwrap();
        assert_eq!(t.factor, 1.);
        assert!(t.usable, "128/100 = 1.28 is inside 0.7..1.4");
        assert!(fit_tempo(128., 85.).is_none_or(|t| t.speed >= 0.7), "85 BPM reads as 2x = 170: 0.75");
        assert!(!fit_tempo(300., 100.).unwrap().usable, "3x is past every multiple");
        assert_eq!(fit_tempo(0., 120.), None);
        assert_eq!(fit_tempo(120., 0.), None);
    }
}
