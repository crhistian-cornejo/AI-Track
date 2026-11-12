//! The Vault preset browser's pure logic: filters, ranking, prev / next
//! stepping and the user's favourites (a small JSON file in the app config
//! folder: favourites belong to the user, not to a project).

use aitrack_stock::instrument::vault::{CHARACTERS, ENGINE_TAGS, ENGINES, PresetInfo, TYPES};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// What the browser shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Free text: every word must match the name, type, engine or a tag.
    pub query: String,
    /// Sound type ([`TYPES`] index).
    pub kind: Option<usize>,
    /// Character tags; a preset needs all of them.
    pub tags: BTreeSet<String>,
    pub favourites_only: bool,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty() && self.kind.is_none() && self.tags.is_empty() && !self.favourites_only
    }
}

/// Where a type sorts (unknown folders last).
fn type_rank(kind: &str) -> usize {
    TYPES.iter().position(|t| t.eq_ignore_ascii_case(kind)).unwrap_or(TYPES.len())
}

/// The engine's display name.
pub fn engine_name(engine: usize) -> &'static str {
    ENGINES.get(engine).copied().unwrap_or("—")
}

/// A short engine badge ("Ladder", "FM6"…).
pub fn engine_badge(engine: usize) -> &'static str {
    ["Ladder", "Analog", "FM6", "E.Piano", "Organ", "Strings", "Wavetable"].get(engine).copied().unwrap_or("—")
}

/// How well one query word matches a preset: 3 name prefix (or a word of
/// the name), 2 inside the name, 1 type / engine / tag. `None` = no match.
fn word_score(p: &PresetInfo, w: &str) -> Option<u32> {
    let name = p.name.to_lowercase();
    if name.starts_with(w) || name.split_whitespace().any(|x| x.starts_with(w)) {
        return Some(3);
    }
    if name.contains(w) {
        return Some(2);
    }
    let engine_tag = ENGINE_TAGS.get(p.engine).copied().unwrap_or("");
    let other = p.kind.to_lowercase().starts_with(w)
        || engine_name(p.engine).to_lowercase().contains(w)
        || engine_tag.starts_with(w)
        || p.tags.iter().any(|t| t.to_lowercase().starts_with(w));
    other.then_some(1)
}

/// The query score of a preset (sum over words), `None` when a word misses.
pub fn score(p: &PresetInfo, query: &str) -> Option<u32> {
    let q = query.trim().to_lowercase();
    q.split_whitespace().try_fold(0, |acc, w| Some(acc + word_score(p, w)?))
}

/// Whether `p` passes every filter but the text.
fn passes(p: &PresetInfo, f: &Filter, favourite: bool) -> bool {
    if f.favourites_only && !favourite {
        return false;
    }
    if let Some(k) = f.kind
        && TYPES.get(k).is_none_or(|t| !t.eq_ignore_ascii_case(&p.kind))
    {
        return false;
    }
    f.tags.iter().all(|t| p.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
}

/// The presets the filter shows, as indices into `list`: by type
/// ([`TYPES`] order) then name; with a query, the best matches first.
pub fn filtered(list: &[PresetInfo], f: &Filter, favs: &Favourites) -> Vec<usize> {
    let mut out: Vec<(u32, usize)> = list
        .iter()
        .enumerate()
        .filter(|(_, p)| passes(p, f, favs.contains(&p.path)))
        .filter_map(|(i, p)| Some((score(p, &f.query)?, i)))
        .collect();
    out.sort_by(|a, b| {
        let (pa, pb) = (&list[a.1], &list[b.1]);
        b.0.cmp(&a.0).then(type_rank(&pa.kind).cmp(&type_rank(&pb.kind))).then_with(|| pa.name.to_lowercase().cmp(&pb.name.to_lowercase()))
    });
    out.into_iter().map(|(_, i)| i).collect()
}

/// The character tags in use, in [`CHARACTERS`] order, with how many
/// presets carry each (tags outside that list follow, alphabetically).
pub fn tag_counts(list: &[PresetInfo]) -> Vec<(String, usize)> {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for p in list {
        for t in &p.tags {
            let t = t.to_lowercase();
            match seen.iter_mut().find(|(x, _)| *x == t) {
                Some((_, n)) => *n += 1,
                None => seen.push((t, 1)),
            }
        }
    }
    let rank = |t: &str| CHARACTERS.iter().position(|c| *c == t).unwrap_or(CHARACTERS.len());
    seen.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    seen
}

/// Index (into `list`) of the preset named `name`.
pub fn find(list: &[PresetInfo], name: Option<&str>) -> Option<usize> {
    let name = name?;
    list.iter().position(|p| p.name == name)
}

/// The preset `dir` steps (−1 / +1) away from `current` among `shown`
/// (indices into the list), wrapping round. With no current preset, or one
/// the filter hides, +1 gives the first shown and −1 the last.
pub fn step(shown: &[usize], current: Option<usize>, dir: i32) -> Option<usize> {
    if shown.is_empty() {
        return None;
    }
    let n = shown.len() as i32;
    let at = current.and_then(|c| shown.iter().position(|&i| i == c));
    let next = match at {
        Some(a) => (a as i32 + dir).rem_euclid(n),
        None if dir < 0 => n - 1,
        None => 0,
    };
    Some(shown[next as usize])
}

// ───────────────────────────── Favourites ─────────────────────────────

/// The user's favourite presets, by factory path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Favourites {
    paths: BTreeSet<String>,
    /// Where they are saved (`None`: kept in memory only, for tests).
    file: Option<PathBuf>,
}

impl Favourites {
    /// The favourites file in the app config folder.
    pub fn default_file() -> PathBuf {
        crate::appearance::config_dir().join("preset_favourites.json")
    }

    /// Read `file` (a missing or broken file is an empty list).
    pub fn load(file: PathBuf) -> Self {
        let paths = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()).unwrap_or_default();
        Self { paths: paths.into_iter().collect(), file: Some(file) }
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Star or unstar `path` and save; returns whether it is a favourite now.
    pub fn toggle(&mut self, path: &str) -> bool {
        let now = if self.paths.remove(path) {
            false
        } else {
            self.paths.insert(path.to_owned());
            true
        };
        self.save();
        now
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.paths.iter().collect::<Vec<_>>()) {
            let _ = std::fs::write(file, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(name: &str, kind: &str, engine: usize, tags: &[&str]) -> PresetInfo {
        PresetInfo {
            path: format!("instrument.vault/{kind}/{name}.json"),
            name: name.into(),
            kind: kind.into(),
            engine,
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            macro_names: ["Cutoff", "Resonance", "Contour", "Space"],
        }
    }

    fn sample() -> Vec<PresetInfo> {
        vec![
            pr("Velvet Pad", "Pad", 1, &["warm", "wide"]),
            pr("Acid Line", "Bass", 0, &["aggressive", "dirty"]),
            pr("Sub Floor", "Bass", 0, &["dark", "fat"]),
            pr("Glass Bells", "Keys", 2, &["glassy", "bright"]),
            pr("Tonewheel Jazz", "Organ", 4, &["vintage", "warm"]),
        ]
    }

    #[test]
    fn the_default_view_sorts_by_type_then_name() {
        let list = sample();
        let shown = filtered(&list, &Filter::default(), &Favourites::default());
        let names: Vec<&str> = shown.iter().map(|&i| list[i].name.as_str()).collect();
        assert_eq!(names, ["Acid Line", "Sub Floor", "Velvet Pad", "Glass Bells", "Tonewheel Jazz"]);
    }

    #[test]
    fn filters_combine_type_tags_text_and_favourites() {
        let list = sample();
        let mut favs = Favourites::default();
        let names = |f: &Filter, favs: &Favourites| filtered(&list, f, favs).into_iter().map(|i| list[i].name.clone()).collect::<Vec<_>>();
        let bass = Filter { kind: Some(0), ..Default::default() };
        assert_eq!(names(&bass, &favs), ["Acid Line", "Sub Floor"]);
        let warm = Filter { tags: ["warm".to_owned()].into(), ..Default::default() };
        assert_eq!(names(&warm, &favs), ["Velvet Pad", "Tonewheel Jazz"]);
        let warm_wide = Filter { tags: ["warm".to_owned(), "WIDE".to_owned()].into(), ..Default::default() };
        assert_eq!(names(&warm_wide, &favs), ["Velvet Pad"], "tags are all required, any case");
        // Text matches names, engines and tags; every word must match.
        assert_eq!(names(&Filter { query: "fm".into(), ..Default::default() }, &favs), ["Glass Bells"]);
        assert_eq!(names(&Filter { query: "bass dark".into(), ..Default::default() }, &favs), ["Sub Floor"]);
        assert!(names(&Filter { query: "zzz".into(), ..Default::default() }, &favs).is_empty());
        // Favourites only.
        favs.toggle(&list[4].path);
        let only = Filter { favourites_only: true, ..Default::default() };
        assert_eq!(names(&only, &favs), ["Tonewheel Jazz"]);
        assert!(!only.is_empty() && Filter::default().is_empty());
    }

    #[test]
    fn name_matches_rank_above_tag_matches() {
        let list = vec![pr("Warm Brass", "Brass", 1, &["bright"]), pr("Alpha", "Pad", 1, &["warm"]), pr("Lukewarm", "Lead", 0, &[])];
        let shown = filtered(&list, &Filter { query: "warm".into(), ..Default::default() }, &Favourites::default());
        let names: Vec<&str> = shown.iter().map(|&i| list[i].name.as_str()).collect();
        assert_eq!(names, ["Warm Brass", "Lukewarm", "Alpha"]);
        assert_eq!(score(&list[0], ""), Some(0), "an empty query matches everything");
    }

    #[test]
    fn stepping_wraps_and_starts_at_the_ends() {
        let shown = [4, 1, 7];
        assert_eq!(step(&shown, Some(1), 1), Some(7));
        assert_eq!(step(&shown, Some(7), 1), Some(4), "wraps forward");
        assert_eq!(step(&shown, Some(4), -1), Some(7), "wraps back");
        assert_eq!(step(&shown, None, 1), Some(4));
        assert_eq!(step(&shown, Some(99), -1), Some(7), "a hidden current preset starts at the end");
        assert_eq!(step(&[], Some(1), 1), None);
        let list = sample();
        assert_eq!(find(&list, Some("Sub Floor")), Some(2));
        assert_eq!(find(&list, None), None);
    }

    #[test]
    fn tags_are_counted_in_character_order() {
        let counts = tag_counts(&sample());
        assert_eq!(counts[0], ("warm".to_owned(), 2));
        let order: Vec<&str> = counts.iter().map(|(t, _)| t.as_str()).collect();
        let pos = |t| order.iter().position(|x| *x == t).unwrap();
        assert!(pos("bright") < pos("dark") && pos("dark") < pos("aggressive"));
    }

    #[test]
    fn favourites_persist_in_their_file() {
        let dir = std::env::temp_dir().join(format!("aitrack-favs-{}", std::process::id()));
        let file = dir.join("preset_favourites.json");
        let _ = std::fs::remove_file(&file);
        let mut f = Favourites::load(file.clone());
        assert_eq!(f.len(), 0);
        assert!(f.toggle("instrument.vault/Bass/Acid Line.json"));
        assert!(f.toggle("instrument.vault/Pad/Velvet Pad.json"));
        assert!(!f.toggle("instrument.vault/Pad/Velvet Pad.json"), "a second star removes it");
        let back = Favourites::load(file.clone());
        assert!(back.contains("instrument.vault/Bass/Acid Line.json") && back.len() == 1);
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(Favourites::load(file).len(), 0, "a broken file is an empty list");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_factory_library_has_metadata_for_the_browser() {
        let list = aitrack_stock::instrument::vault::preset_info();
        assert!(list.len() >= 40, "{} presets", list.len());
        let names: BTreeSet<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.len(), list.len(), "names are unique, so a device's preset name finds its entry");
        for (k, t) in TYPES.iter().enumerate() {
            let f = Filter { kind: Some(k), ..Default::default() };
            assert!(!filtered(&list, &f, &Favourites::default()).is_empty(), "no {t} presets");
        }
        assert!(!tag_counts(&list).is_empty());
    }
}
