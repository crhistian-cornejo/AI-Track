//! What the Sounds panel decides, apart from how it looks: the query the
//! filters make, how each result is adjusted to the song, the synced preview
//! asked for, and the remembered layout.

use crate::bridge::{BpmTrust, LibraryItem, SoundFolder, SoundGroup, SoundHit, SoundKind, SoundOrigin, SoundQuery, SyncedPreview};
use aitrack_library::{Fit, FitKind, TempoFit, fit_key, fit_tempo};
use aitrack_model::{PPQ, Scale, TempoMap, Tick};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Which side of the arrangement the panel docks to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// The panel's chips and switches.
#[derive(Clone, Debug, PartialEq)]
pub struct Filters {
    /// "Fit to song": filter by the project's key and tempo, show how far each
    /// sound moves, and preview and drop them adjusted.
    pub fit: bool,
    /// With `fit` on: only keys that suit the song's (the rest of its chips narrow it further).
    pub by_key: bool,
    /// Only the song's key itself, not its relatives and neighbours.
    pub key_exact: bool,
    pub by_bpm: bool,
    /// Tempo tolerance in percent.
    pub bpm_tolerance: f32,
    pub kind: SoundKind,
    pub group: Option<SoundGroup>,
    pub tag: Option<String>,
    pub origin: Option<SoundOrigin>,
    pub favorites: bool,
    /// Only inside this folder.
    pub folder: Option<SoundFolder>,
}

impl Default for Filters {
    fn default() -> Self {
        Self { fit: false, by_key: true, key_exact: false, by_bpm: true, bpm_tolerance: 6., kind: SoundKind::All, group: None, tag: None, origin: None, favorites: false, folder: None }
    }
}

/// The library query the text and filters make for this song.
pub fn build_query(text: &str, f: &Filters, song_scale: Option<Scale>, song_bpm: f64) -> SoundQuery {
    SoundQuery {
        text: text.trim().to_owned(),
        kind: f.kind,
        group: f.group,
        key: song_scale.filter(|_| f.fit && f.by_key).map(|s| (s, f.key_exact)),
        bpm: (f.fit && f.by_bpm && song_bpm > 0.).then_some((song_bpm as f32, f.bpm_tolerance)),
        tag: f.tag.clone(),
        origin: f.origin,
        favorites: f.favorites,
        folder: f.folder.clone(),
        limit: 0,
    }
}

/// Which folder is the Splice one: the user's choice, else the one already in
/// use while it is there, else (when `probe_due`) whatever `detect` finds,
/// else the place Splice would use, so the panel can say where it looked.
/// Looking is a walk of the disk, hence `probe_due`.
pub fn splice_folder(
    explicit: Option<std::path::PathBuf>,
    current: Option<&std::path::Path>,
    probe_due: bool,
    detect: impl FnOnce() -> Option<std::path::PathBuf>,
    fallback: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    if let Some(c) = current.filter(|c| c.is_dir()) {
        return Some(c.to_path_buf());
    }
    if probe_due && let Some(found) = detect() {
        return Some(found);
    }
    current.map(std::path::Path::to_path_buf).or(fallback)
}

/// How a result is adjusted to the song.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowFit {
    pub key: Fit,
    /// Loops with a tempo only.
    pub tempo: Option<TempoFit>,
}

/// Drums and effects have no pitch to fit.
fn atonal_by_name(item: &LibraryItem) -> bool {
    let c = item.category.to_lowercase();
    ["kick", "snare", "clap", "hat", "perc", "riser", "impact", "atmos", "noise", "fx", "tom", "cymbal"].iter().any(|w| c.contains(w))
}

/// How `hit` meets the song: the semitones to its key and the stretch to its tempo.
pub fn row_fit(hit: &SoundHit, song_scale: Option<Scale>, song_bpm: f64) -> RowFit {
    let key = match (hit.item.key, song_scale) {
        (Some(k), Some(song)) => fit_key(song, Some(k.root), Some(k.kind)),
        (None, _) if atonal_by_name(&hit.item) => Fit { semitones: 0, kind: FitKind::Atonal },
        _ => Fit::NONE,
    };
    let tempo = hit.item.bpm.filter(|_| hit.item.is_loop).and_then(|b| fit_tempo(song_bpm, b));
    RowFit { key, tempo }
}

/// The line under a result: `+3 st · 103 %`.
pub fn fit_label(fit: &RowFit, is_loop: bool) -> String {
    let key = match fit.key.kind {
        FitKind::Same | FitKind::Relative => "0 st".to_owned(),
        FitKind::Transposed => format!("{:+} st", fit.key.semitones),
        FitKind::Atonal => String::new(),
        FitKind::Unknown => "key ?".to_owned(),
    };
    let tempo = match (fit.tempo, is_loop) {
        (Some(t), _) => format!("{:.0} %", t.percent()),
        (None, true) => "tempo ?".to_owned(),
        (None, false) => String::new(),
    };
    [key, tempo].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// How a sound dropped into the song is left: the semitones to the song's
/// key, the tempo the loop is read at (warp maps it to the song's), and the
/// warp mode that suits what it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropFit {
    pub transpose: f32,
    /// The loop's tempo times the 1/2/1/2 multiple that lands nearest the song's.
    pub original_bpm: Option<f64>,
    /// Warp starts on: the tempo is a measured one.
    pub warp: bool,
    pub mode: aitrack_model::WarpMode,
}

/// What dropping `item` leaves, with "fit to song" on. A tonal sound moves the
/// shortest way to the song's key (minor over its relative major is 0); a loop
/// whose tempo was measured (not guessed from its length) is warped to the song,
/// unless it is too far from every multiple of the song's (the preview leaves it as it is too).
pub fn drop_fit(item: &LibraryItem, trust: BpmTrust, song_scale: Option<Scale>, song_bpm: f64) -> DropFit {
    let key = match (item.key, song_scale) {
        (Some(k), Some(song)) => fit_key(song, Some(k.root), Some(k.kind)),
        _ => Fit::NONE,
    };
    let transpose = if key.kind == FitKind::Transposed { key.semitones as f32 } else { 0. };
    let tempo = item.bpm.filter(|_| item.is_loop).and_then(|b| fit_tempo(song_bpm, b).filter(|t| t.usable).map(|t| (b, t)));
    let original_bpm = tempo.map(|(b, t)| f64::from(b * t.factor));
    let tonal = item.key.is_some() && !atonal_by_name(item);
    DropFit {
        transpose,
        original_bpm,
        warp: original_bpm.is_some() && trust.allows_auto_warp(),
        // Chords and melodies keep their harmony when stretched; drums slice at their hits.
        mode: if tonal { aitrack_model::WarpMode::Complex } else { aitrack_model::WarpMode::Beats },
    }
}

/// Time from `position` to the next bar line of the song, and the length of a bar there.
pub fn next_bar(tempo: &TempoMap, position: Tick) -> (Duration, Duration) {
    let sig = tempo.signature_at(position);
    let bar = sig.bar_ticks().max(PPQ);
    let line = position.div_euclid(bar) * bar;
    let next = line + bar;
    let secs = |a: Tick, b: Tick| Duration::from_secs_f64((tempo.tick_to_seconds(b) - tempo.tick_to_seconds(a)).max(0.));
    // Exactly on a bar line: the sound enters now.
    let start = if position == line { Duration::ZERO } else { secs(position, next) };
    (start, secs(line, next))
}

/// The synced preview asked for `hit`. With "fit" on it is stretched to the
/// song (when the sound's tempo is trustworthy, as when it is dropped) and
/// transposed to its key; off, it plays as the file is. `transport` is
/// `(position, playing)`: with the song running a loop enters on the next bar.
pub fn preview_request(hit: &SoundHit, f: &Filters, song_scale: Option<Scale>, tempo: &TempoMap, transport: (Tick, bool)) -> SyncedPreview {
    let song_bpm = tempo.initial_bpm();
    let (bar_start, bar) = next_bar(tempo, transport.0);
    let fit = row_fit(hit, song_scale, song_bpm);
    let stretch = f.fit && hit.item.is_loop && hit.bpm_trust == BpmTrust::Reliable;
    let original_bpm = fit.tempo.filter(|t| stretch && t.usable).and_then(|t| hit.item.bpm.map(|b| f64::from(b * t.factor)));
    let semitones = if f.fit && fit.key.kind == FitKind::Transposed { fit.key.semitones } else { 0 };
    SyncedPreview {
        path: hit.item.path.clone(),
        original_bpm,
        project_bpm: song_bpm,
        semitones,
        tonal: hit.item.key.is_some() && !atonal_by_name(&hit.item),
        looped: hit.item.is_loop,
        // A loop of the running song enters on its bar; a one-shot plays when asked.
        start_in: (transport.1 && hit.item.is_loop).then_some(bar_start),
        bar,
    }
}

/// The result after moving `delta` rows from `selected` (`None` = none yet),
/// kept inside the list. `None` for an empty list.
pub fn step_selection(selected: Option<usize>, len: usize, delta: i32) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match selected {
        None => if delta < 0 { len - 1 } else { 0 },
        Some(i) => (i as i64 + i64::from(delta)).clamp(0, len as i64 - 1) as usize,
    })
}

/// What is remembered between runs (`sounds.json` in the settings folder).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub open: bool,
    pub width: f32,
    pub side: Side,
    pub fit: bool,
    pub volume: f32,
}

pub const MIN_WIDTH: f32 = 280.;
pub const MAX_WIDTH: f32 = 560.;
pub const DEFAULT_WIDTH: f32 = 336.;

impl Default for Prefs {
    fn default() -> Self {
        Self { open: false, width: DEFAULT_WIDTH, side: Side::Left, fit: true, volume: 0.8 }
    }
}

impl Prefs {
    fn path() -> std::path::PathBuf {
        crate::appearance::config_dir().join("sounds.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let mut p: Self = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        p.width = p.width.clamp(MIN_WIDTH, MAX_WIDTH);
        p.volume = p.volume.clamp(0., 1.);
        p
    }

    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    pub fn save_to(&self, path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Key, ScaleKind};
    use std::path::PathBuf;

    fn am() -> Scale {
        Scale::new(Key(9), ScaleKind::Minor)
    }

    fn hit(name: &str, key: Option<Scale>, bpm: Option<f32>, is_loop: bool, category: &str) -> SoundHit {
        SoundHit {
            item: LibraryItem { id: 1, name: name.into(), path: PathBuf::from(format!("/s/{name}.wav")), category: category.into(), key, bpm, duration: 4., is_loop, favorite: false },
            origin: SoundOrigin::Mine,
            tags: vec![],
            bpm_trust: BpmTrust::Reliable,
            unanalysed: false,
        }
    }

    #[test]
    fn the_splice_folder_is_the_choice_then_the_one_in_use_then_what_is_found() {
        use std::path::Path;
        let dir = std::env::temp_dir().join(format!("aitrack-splice-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fallback = Some(PathBuf::from("/nowhere/Splice"));
        let found = || Some(PathBuf::from("/found/Splice"));
        // A choice wins even when it is missing: the panel says so.
        assert_eq!(splice_folder(Some("/mine".into()), Some(&dir), true, found, fallback.clone()), Some("/mine".into()));
        // The folder in use is kept without looking at the disk again.
        assert_eq!(splice_folder(None, Some(&dir), true, || panic!("must not look"), fallback.clone()), Some(dir.clone()));
        // Missing: looked for when due, else the old guess stays (and nothing is walked).
        assert_eq!(splice_folder(None, Some(Path::new("/gone")), true, found, fallback.clone()), Some("/found/Splice".into()));
        assert_eq!(splice_folder(None, Some(Path::new("/gone")), false, || panic!("not due"), fallback.clone()), Some("/gone".into()));
        assert_eq!(splice_folder(None, None, true, || None, fallback.clone()), fallback, "nothing found: say where it was looked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fit_off_asks_for_everything_and_fit_on_for_the_songs_key_and_tempo() {
        let mut f = Filters::default();
        let q = build_query("  pad ", &f, Some(am()), 128.);
        assert_eq!((q.text.as_str(), q.key, q.bpm), ("pad", None, None), "nothing is filtered by the song until fit is on");
        f.fit = true;
        let q = build_query("", &f, Some(am()), 128.);
        assert_eq!(q.key, Some((am(), false)), "compatible keys by default");
        assert_eq!(q.bpm, Some((128., 6.)));
        f.key_exact = true;
        f.by_bpm = false;
        f.group = Some(SoundGroup::Bass);
        f.origin = Some(SoundOrigin::Splice);
        let q = build_query("", &f, Some(am()), 128.);
        assert_eq!((q.key, q.bpm, q.group, q.origin), (Some((am(), true)), None, Some(SoundGroup::Bass), Some(SoundOrigin::Splice)));
        // A song with no key cannot filter by it (the panel asks for one).
        assert_eq!(build_query("", &f, None, 128.).key, None);
    }

    #[test]
    fn each_row_says_how_far_it_moves_and_how_much_it_stretches() {
        let song = Scale::new(Key(0), ScaleKind::Minor); // C minor
        let bass = hit("Bass Loop Am", Some(am()), Some(124.), true, "Bass Loop");
        let fit = row_fit(&bass, Some(song), 128.);
        assert_eq!(fit.key.semitones, 3, "A minor to C minor: three up");
        assert_eq!(fit_label(&fit, true), "+3 st · 103 %");
        let relative = row_fit(&hit("Pad", Some(am()), None, false, "Pad"), Some(Scale::new(Key(0), ScaleKind::Major)), 128.);
        assert_eq!(fit_label(&relative, false), "0 st", "A minor in C major: the same notes");
        let kick = row_fit(&hit("Kick", None, None, false, "Kick"), Some(song), 128.);
        assert_eq!(fit_label(&kick, false), "", "drums have nothing to fit");
        let unknown = row_fit(&hit("Mystery", None, Some(120.), true, "Loop"), Some(song), 128.);
        assert_eq!(fit_label(&unknown, true), "key ? · 107 %");
        let half = row_fit(&hit("Slow Loop", Some(am()), Some(64.), true, "Loop"), Some(Scale::new(Key(9), ScaleKind::Minor)), 128.);
        assert_eq!(fit_label(&half, true), "0 st · 100 %", "half time reads as double");
    }

    #[test]
    fn the_synced_preview_stretches_to_the_song_and_moves_to_its_key_only_when_fit_is_on() {
        let song = Scale::new(Key(0), ScaleKind::Minor);
        let tempo = TempoMap::constant(128.);
        let loop_am = hit("Bass Loop Am", Some(am()), Some(124.), true, "Bass Loop");
        let mut f = Filters { fit: true, ..Filters::default() };
        let r = preview_request(&loop_am, &f, Some(song), &tempo, (0, false));
        assert_eq!((r.original_bpm, r.semitones, r.project_bpm), (Some(124.), 3, 128.));
        assert!(r.tonal && r.looped);
        assert_eq!(r.start_in, None, "the song is stopped: it starts at once");
        // Fit off: the file as it is.
        f.fit = false;
        let plain = preview_request(&loop_am, &f, Some(song), &tempo, (0, false));
        assert_eq!((plain.original_bpm, plain.semitones), (None, 0));
        // A tempo only guessed from the length is not stretched, as when it is dropped.
        f.fit = true;
        let mut guessed = loop_am.clone();
        guessed.bpm_trust = BpmTrust::Estimate;
        assert_eq!(preview_request(&guessed, &f, Some(song), &tempo, (0, false)).original_bpm, None);
        // A one-shot is moved in key, not stretched, and plays once.
        let shot = hit("Bass Hit Am", Some(am()), None, false, "Bass");
        let r = preview_request(&shot, &f, Some(song), &tempo, (0, true));
        assert_eq!((r.original_bpm, r.semitones, r.looped, r.start_in), (None, 3, false, None));
        // A kick is left alone.
        let r = preview_request(&hit("Kick", None, None, false, "Kick"), &f, Some(song), &tempo, (0, false));
        assert_eq!((r.semitones, r.tonal), (0, false));
    }

    #[test]
    fn a_loop_enters_on_the_next_bar_of_the_running_song() {
        let tempo = TempoMap::constant(120.); // a 4/4 bar is 2 s
        let loop_am = hit("Loop", Some(am()), Some(120.), true, "Loop");
        let f = Filters { fit: true, ..Filters::default() };
        // Half a beat into the second bar (tick 4*PPQ + PPQ/2): 3.5 beats left = 1.75 s.
        let r = preview_request(&loop_am, &f, Some(am()), &tempo, (4 * PPQ + PPQ / 2, true));
        assert_eq!(r.bar, Duration::from_secs(2));
        let wait = r.start_in.unwrap().as_secs_f64();
        assert!((wait - 1.75).abs() < 1e-6, "{wait}");
        // Exactly on a bar line: now.
        assert_eq!(preview_request(&loop_am, &f, Some(am()), &tempo, (8 * PPQ, true)).start_in, Some(Duration::ZERO));
        // Three beats in 3/4 (a 1.5 s bar): half a second.
        let mut waltz = TempoMap::constant(120.);
        waltz.signatures = vec![(0, aitrack_model::TimeSignature { numerator: 3, denominator: 4 })];
        let (to_bar, bar) = next_bar(&waltz, 2 * PPQ + PPQ / 2);
        assert!((bar.as_secs_f64() - 1.5).abs() < 1e-6 && (to_bar.as_secs_f64() - 0.25).abs() < 1e-6, "{to_bar:?} {bar:?}");
    }

    #[test]
    fn the_arrows_move_inside_the_list() {
        assert_eq!(step_selection(None, 5, 1), Some(0));
        assert_eq!(step_selection(None, 5, -1), Some(4));
        assert_eq!(step_selection(Some(2), 5, 1), Some(3));
        assert_eq!(step_selection(Some(4), 5, 1), Some(4), "stops at the end");
        assert_eq!(step_selection(Some(0), 5, -1), Some(0));
        assert_eq!(step_selection(Some(1), 5, 10), Some(4));
        assert_eq!(step_selection(Some(0), 0, 1), None);
    }

    #[test]
    fn the_layout_is_remembered_and_old_or_wild_files_are_tamed() {
        let path = std::env::temp_dir().join(format!("aitrack-sounds-prefs-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(Prefs::load_from(&path), Prefs::default());
        let p = Prefs { open: true, width: 420., side: Side::Right, fit: false, volume: 0.3 };
        p.save_to(&path);
        assert_eq!(Prefs::load_from(&path), p);
        std::fs::write(&path, r#"{"open": true, "width": 5000, "volume": 9}"#).unwrap();
        let wild = Prefs::load_from(&path);
        assert!(wild.open && wild.width == MAX_WIDTH && wild.volume == 1. && wild.side == Side::Left);
        let _ = std::fs::remove_file(&path);
    }

    fn item(key: Option<Scale>, bpm: Option<f32>, is_loop: bool, category: &str) -> LibraryItem {
        hit("x", key, bpm, is_loop, category).item
    }

    #[test]
    fn dropping_a_loop_fits_its_key_and_reads_its_tempo_at_the_nearest_multiple() {
        let c_major = Scale::new(Key(0), ScaleKind::Major);
        // Am loop over C major: its relative, nothing to move.
        let f = drop_fit(&item(Some(am()), Some(120.), true, "Bass loop"), BpmTrust::Reliable, Some(c_major), 128.);
        assert_eq!(f.transpose, 0.);
        assert!(f.warp && f.original_bpm == Some(120.));
        // The same loop over C minor: +3, the shortest way to the song's scale.
        let c_minor = Scale::new(Key(0), ScaleKind::Minor);
        assert_eq!(drop_fit(&item(Some(am()), Some(120.), true, "Bass loop"), BpmTrust::Reliable, Some(c_minor), 128.).transpose, 3.);
        // A 64 BPM loop in a 128 BPM song is read at 128.
        let half = drop_fit(&item(Some(am()), Some(64.), true, "Pad loop"), BpmTrust::Reliable, Some(am()), 128.);
        assert_eq!(half.original_bpm, Some(128.));
        assert_eq!(half.mode, aitrack_model::WarpMode::Complex);
    }

    #[test]
    fn dropping_a_loop_never_stretches_past_what_the_preview_would() {
        // 100 BPM in a 300 BPM song is 3x at best: the preview plays it as it is, so a drop leaves it unwarped too.
        let wild = item(Some(am()), Some(100.), true, "Bass loop");
        let f = drop_fit(&wild, BpmTrust::Reliable, Some(am()), 300.);
        assert!(f.original_bpm.is_none() && !f.warp, "{f:?}");
        let hit = hit("x", Some(am()), Some(100.), true, "Bass loop");
        let filters = Filters { fit: true, ..Filters::default() };
        let tempo = TempoMap::constant(300.);
        let req = preview_request(&hit, &filters, Some(am()), &tempo, (0, false));
        assert!(req.original_bpm.is_none(), "preview and drop agree");
        // Within range both stretch.
        let ok = drop_fit(&item(Some(am()), Some(120.), true, "Bass loop"), BpmTrust::Reliable, Some(am()), 128.);
        assert_eq!(ok.original_bpm, Some(120.));
    }

    #[test]
    fn dropping_a_guessed_tempo_or_a_one_shot_does_not_warp() {
        let guessed = drop_fit(&item(None, Some(97.), true, "Drum loop"), BpmTrust::Estimate, Some(am()), 128.);
        assert!(!guessed.warp, "a tempo guessed from the length never stretches by itself");
        assert_eq!(guessed.mode, aitrack_model::WarpMode::Beats);
        let shot = drop_fit(&item(Some(Scale::new(Key(2), ScaleKind::Minor)), None, false, "Synth one-shot"), BpmTrust::Reliable, Some(am()), 128.);
        assert!(!shot.warp && shot.original_bpm.is_none());
        assert_ne!(shot.transpose, 0., "a one-shot in D minor over A minor is moved");
        let kick = drop_fit(&item(None, None, false, "Kick"), BpmTrust::Reliable, Some(am()), 128.);
        assert_eq!(kick.transpose, 0., "no pitch to fit");
    }
}
