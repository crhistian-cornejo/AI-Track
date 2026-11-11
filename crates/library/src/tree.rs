//! Virtual folders: rule-based views over the index, never copies.

use aitrack_model::{Key, Scale, ScaleKind};
use serde::{Deserialize, Serialize};

use crate::db::{Library, Sample, decode_list, parse_scale_kind};
use crate::error::Result;
use crate::query::{Filters, SearchQuery, SortBy, camelot_scale};
use crate::taxonomy::{Category, LOOP_FAMILIES, SampleKind};

/// A virtual folder. Each one maps to a set of [`Filters`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VirtualFolder {
    All,
    Categories,
    Category { category: Category, subcategory: Option<String> },
    Loops { family: Option<String> },
    OneShots { category: Option<Category> },
    Chops,
    Keys,
    /// `scale = None` lists root-only samples (`Bass C.wav`).
    Key { root: u8, scale: Option<ScaleKind>, category: Option<Category> },
    CamelotWheel,
    Camelot(String),
    BpmRanges,
    /// Inclusive low, exclusive high.
    Bpm { lo: u16, hi: u16 },
    Genres,
    Genre(String),
    Packs,
    Pack(String),
    Favorites,
    Recent,
    RecentlyAdded,
}

impl VirtualFolder {
    /// The filters that list this folder's samples (containers list everything below them).
    pub fn filters(&self) -> Filters {
        let mut f = Filters::default();
        match self {
            Self::All | Self::Categories | Self::Keys | Self::CamelotWheel | Self::BpmRanges | Self::Genres
            | Self::Packs | Self::RecentlyAdded => {}
            Self::Category { category, subcategory } => {
                f.category = Some(*category);
                f.subcategory = subcategory.clone();
            }
            Self::Loops { family } => {
                f.kind = Some(SampleKind::Loop);
                f.loop_family = family.clone();
            }
            Self::OneShots { category } => {
                f.kind = Some(SampleKind::OneShot);
                f.category = *category;
            }
            Self::Chops => f.kind = Some(SampleKind::Chop),
            Self::Key { root, scale, category } => {
                f.key_exact = Some((Key(*root), *scale));
                f.category = *category;
            }
            Self::Camelot(c) => f.camelot = Some(c.clone()),
            Self::Bpm { lo, hi } => f.bpm_range = Some((*lo as f32, *hi as f32 - 0.001)),
            Self::Genre(g) => f.genre = Some(g.clone()),
            Self::Pack(p) => f.pack = Some(p.clone()),
            Self::Favorites => f.favorites_only = true,
            Self::Recent => f.recent_only = true,
        }
        f
    }

    fn sort(&self) -> SortBy {
        match self {
            Self::Recent => SortBy::RecentlyUsed,
            Self::RecentlyAdded => SortBy::RecentlyAdded,
            _ => SortBy::Name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TreeNode {
    pub folder: VirtualFolder,
    pub label: String,
    pub count: u64,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    fn new(folder: VirtualFolder, label: impl Into<String>, count: u64) -> Self {
        Self { folder, label: label.into(), count, children: Vec::new() }
    }

    /// Depth-first search by label path, e.g. `["Categories", "Drums", "Kicks"]`.
    pub fn find(&self, path: &[&str]) -> Option<&TreeNode> {
        match path.split_first() {
            None => Some(self),
            Some((first, rest)) => self.children.iter().find(|c| c.label == *first)?.find(rest),
        }
    }
}

/// `(group key, second key or name, count)` from a grouped COUNT query.
type GroupRow = (Option<String>, Option<String>, u64);

/// BPM buckets for the BPM view.
pub const BPM_BUCKETS: &[(u16, u16)] = &[
    (0, 70),
    (70, 80),
    (80, 90),
    (90, 100),
    (100, 110),
    (110, 120),
    (120, 125),
    (125, 130),
    (130, 140),
    (140, 150),
    (150, 160),
    (160, 170),
    (170, 180),
    (180, 400),
];

fn bpm_label(lo: u16, hi: u16) -> String {
    match (lo, hi) {
        (0, h) => format!("< {h}"),
        (l, 400) => format!("{l}+"),
        (l, h) => format!("{l}–{}", h - 1),
    }
}

impl Library {
    /// Lists the samples of a virtual folder.
    pub fn list(&self, folder: &VirtualFolder, limit: usize, offset: usize) -> Result<Vec<Sample>> {
        self.search(&SearchQuery { text: None, filters: folder.filters(), sort: folder.sort(), limit, offset })
    }

    /// Builds the whole virtual tree with counts. Grouped `COUNT(*)` queries,
    /// so it stays fast with tens of thousands of samples.
    pub fn tree(&self) -> Result<Vec<TreeNode>> {
        let q = |sql: &str| -> Result<Vec<GroupRow>> {
            let mut st = self.conn.prepare(sql)?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let ok = "status = 'ok'";
        let total = self.count()?;
        let mut out = Vec::new();

        // Categories → subcategories (canonical ones first, then any others found).
        let cats = q(&format!(
            "SELECT category, subcategory, COUNT(*) FROM samples WHERE {ok} AND category IS NOT NULL GROUP BY 1, 2"
        ))?;
        let mut categories = TreeNode::new(VirtualFolder::Categories, "Categories", 0);
        for c in Category::ALL {
            let rows: Vec<_> = cats.iter().filter(|r| r.0.as_deref() == Some(c.as_str())).collect();
            let n: u64 = rows.iter().map(|r| r.2).sum();
            let mut node = TreeNode::new(VirtualFolder::Category { category: c, subcategory: None }, c.as_str(), n);
            let mut subs: Vec<String> = c.subcategories().iter().map(|s| s.to_string()).collect();
            for r in &rows {
                if let Some(s) = &r.1
                    && !subs.contains(s)
                {
                    subs.push(s.clone());
                }
            }
            for s in subs {
                let n = rows.iter().filter(|r| r.1.as_deref() == Some(&s)).map(|r| r.2).sum();
                node.children.push(TreeNode::new(
                    VirtualFolder::Category { category: c, subcategory: Some(s.clone()) },
                    s,
                    n,
                ));
            }
            categories.count += n;
            categories.children.push(node);
        }
        out.push(categories);

        // Loops → families.
        let fams = q(&format!(
            "SELECT loop_family, NULL, COUNT(*) FROM samples WHERE {ok} AND kind = 'loop' GROUP BY 1"
        ))?;
        let mut loops = TreeNode::new(VirtualFolder::Loops { family: None }, "Loops", fams.iter().map(|r| r.2).sum());
        for fam in LOOP_FAMILIES {
            let n = fams.iter().filter(|r| r.0.as_deref() == Some(fam)).map(|r| r.2).sum();
            loops.children.push(TreeNode::new(VirtualFolder::Loops { family: Some(fam.into()) }, fam, n));
        }
        out.push(loops);

        // One-shots → categories.
        let os = q(&format!(
            "SELECT category, NULL, COUNT(*) FROM samples WHERE {ok} AND kind = 'one_shot' GROUP BY 1"
        ))?;
        let mut one_shots =
            TreeNode::new(VirtualFolder::OneShots { category: None }, "One-shots", os.iter().map(|r| r.2).sum());
        for c in Category::ALL {
            let n = os.iter().filter(|r| r.0.as_deref() == Some(c.as_str())).map(|r| r.2).sum();
            if n > 0 {
                one_shots
                    .children
                    .push(TreeNode::new(VirtualFolder::OneShots { category: Some(c) }, c.as_str(), n));
            }
        }
        out.push(one_shots);
        let chops = self.count_where(&VirtualFolder::Chops.filters())?;
        out.push(TreeNode::new(VirtualFolder::Chops, "Chops", chops));

        // Keys → "A Minor" → categories; sorted by Camelot position.
        let keys = q(&format!(
            "SELECT CAST(key_root AS TEXT), scale_kind, COUNT(*) FROM samples
             WHERE {ok} AND key_root IS NOT NULL GROUP BY 1, 2"
        ))?;
        let key_cats = {
            let mut st = self.conn.prepare(&format!(
                "SELECT key_root, scale_kind, category, COUNT(*) FROM samples
                 WHERE {ok} AND key_root IS NOT NULL AND category IS NOT NULL GROUP BY 1, 2, 3"
            ))?;
            let rows = st.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut key_entries: Vec<(u8, Option<ScaleKind>, u64)> = keys
            .iter()
            .filter_map(|(r, s, n)| {
                Some((r.as_deref()?.parse().ok()?, s.as_deref().and_then(parse_scale_kind), *n))
            })
            .collect();
        let order = |root: u8, s: Option<ScaleKind>| -> (u8, u8, u8) {
            match s.and_then(|k| Scale::new(Key(root), k).camelot()) {
                Some(c) => {
                    let (n, minor) = crate::query::parse_camelot(&c).unwrap_or((0, false));
                    (0, n, if minor { 0 } else { 1 })
                }
                None => (1, root, 0),
            }
        };
        key_entries.sort_by_key(|(r, s, _)| order(*r, *s));
        let mut keys_node = TreeNode::new(VirtualFolder::Keys, "Keys", key_entries.iter().map(|e| e.2).sum());
        for (root, scale, n) in key_entries {
            let label = match scale {
                Some(k) => {
                    let s = Scale::new(Key(root), k);
                    match s.camelot() {
                        Some(c) => format!("{} ({c})", s.display()),
                        None => s.display(),
                    }
                }
                None => format!("{} (root)", Key(root).name()),
            };
            let mut node = TreeNode::new(VirtualFolder::Key { root, scale, category: None }, label, n);
            for c in Category::ALL {
                let cn: i64 = key_cats
                    .iter()
                    .filter(|(r, s, cat, _)| {
                        *r == root as i64 && s.as_deref().and_then(parse_scale_kind) == scale && cat == c.as_str()
                    })
                    .map(|e| e.3)
                    .sum();
                if cn > 0 {
                    node.children.push(TreeNode::new(
                        VirtualFolder::Key { root, scale, category: Some(c) },
                        c.as_str(),
                        cn as u64,
                    ));
                }
            }
            keys_node.children.push(node);
        }
        out.push(keys_node);

        // Camelot wheel 1A..12B.
        let cams = q(&format!(
            "SELECT camelot, NULL, COUNT(*) FROM samples WHERE {ok} AND camelot IS NOT NULL GROUP BY 1"
        ))?;
        let mut wheel = TreeNode::new(VirtualFolder::CamelotWheel, "Camelot", cams.iter().map(|r| r.2).sum());
        for n in 1..=12 {
            for l in ['A', 'B'] {
                let code = format!("{n}{l}");
                let cnt = cams.iter().filter(|r| r.0.as_deref() == Some(&code)).map(|r| r.2).sum();
                let label = match camelot_scale(&code) {
                    Some(s) => format!("{code} · {}", s.display()),
                    None => code.clone(),
                };
                wheel.children.push(TreeNode::new(VirtualFolder::Camelot(code), label, cnt));
            }
        }
        out.push(wheel);

        // BPM ranges.
        let mut bpm = TreeNode::new(VirtualFolder::BpmRanges, "BPM", 0);
        for &(lo, hi) in BPM_BUCKETS {
            let f = VirtualFolder::Bpm { lo, hi };
            let n = self.count_where(&f.filters())?;
            if n > 0 {
                bpm.count += n;
                bpm.children.push(TreeNode::new(f, bpm_label(lo, hi), n));
            }
        }
        out.push(bpm);

        // Genres (stored as |a|b| lists).
        let g = q(&format!("SELECT genres, NULL, COUNT(*) FROM samples WHERE {ok} AND genres != '' GROUP BY 1"))?;
        let mut genre_counts: std::collections::BTreeMap<String, u64> = Default::default();
        for (list, _, n) in &g {
            for name in decode_list(list.as_deref().unwrap_or("")) {
                *genre_counts.entry(name).or_default() += n;
            }
        }
        let mut genres = TreeNode::new(VirtualFolder::Genres, "Genres", 0);
        for (name, n) in genre_counts {
            genres.count += n;
            genres.children.push(TreeNode::new(VirtualFolder::Genre(name.clone()), name, n));
        }
        out.push(genres);

        // Packs (display name from the packs table when known).
        let p = q(&format!(
            "SELECT s.pack, p.name, COUNT(*) FROM samples s LEFT JOIN packs p ON p.id = s.pack
             WHERE s.{ok} AND s.pack IS NOT NULL GROUP BY 1 ORDER BY COALESCE(p.name, s.pack) COLLATE NOCASE"
        ))?;
        let mut packs = TreeNode::new(VirtualFolder::Packs, "Packs", 0);
        for (id, name, n) in p {
            let id = id.unwrap_or_default();
            packs.count += n;
            packs.children.push(TreeNode::new(VirtualFolder::Pack(id.clone()), name.unwrap_or(id), n));
        }
        out.push(packs);

        out.push(TreeNode::new(VirtualFolder::Favorites, "Favorites", self.count_where(&VirtualFolder::Favorites.filters())?));
        out.push(TreeNode::new(VirtualFolder::Recent, "Recent", self.count_where(&VirtualFolder::Recent.filters())?));
        out.push(TreeNode::new(VirtualFolder::RecentlyAdded, "Recently Added", total));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::RootKind;
    use crate::scan::ScanOptions;
    use crate::test_util::write_wav;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn tree_counts_and_listing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        for (rel, dur) in [
            ("Techno/Kicks/Kick 1.wav", 0.3),
            ("Techno/Kicks/Kick 2.wav", 0.3),
            ("Techno/Hat Loop 128bpm.wav", 1.875),
            ("Techno/Bass Loop Am 128bpm.wav", 1.875),
            ("Reggaeton/Vocal Chop Cm.wav", 0.4),
            ("Reggaeton/Bass C.wav", 0.6),
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            write_wav(&p, 22_050, 1, dur, 0.5);
        }
        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        lib.add_root(&root, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| {}, &AtomicBool::new(false))
            .unwrap();
        let k = lib.find_by_path(&root.join("Techno/Kicks/Kick 1.wav")).unwrap().unwrap();
        lib.set_favorite(k.id, true).unwrap();
        lib.mark_used(k.id).unwrap();

        let tree = lib.tree().unwrap();
        let top = |label: &str| tree.iter().find(|n| n.label == label).unwrap();
        assert_eq!(top("Categories").find(&["Drums", "Kicks"]).unwrap().count, 2);
        assert_eq!(top("Categories").find(&["Drums", "Hats"]).unwrap().count, 1);
        assert_eq!(top("Categories").find(&["Bass"]).unwrap().count, 2);
        assert_eq!(top("Loops").count, 2);
        assert_eq!(top("Loops").find(&["Top"]).unwrap().count, 1);
        assert_eq!(top("Loops").find(&["Bass"]).unwrap().count, 1);
        assert_eq!(top("Chops").count, 1);
        assert_eq!(top("Keys").find(&["A Minor (8A)", "Bass"]).unwrap().count, 1);
        assert_eq!(top("Keys").find(&["C (root)"]).unwrap().count, 1);
        assert_eq!(top("Camelot").find(&["8A · A Minor"]).unwrap().count, 1);
        assert_eq!(top("BPM").find(&["125–129"]).unwrap().count, 2);
        assert_eq!(top("Genres").find(&["Techno"]).unwrap().count, 4);
        assert_eq!(top("Genres").find(&["Reggaeton"]).unwrap().count, 2);
        assert_eq!(top("Packs").find(&["Techno"]).unwrap().count, 4);
        assert_eq!(top("Favorites").count, 1);
        assert_eq!(top("Recent").count, 1);

        let kicks = lib
            .list(&VirtualFolder::Category { category: Category::Drums, subcategory: Some("Kicks".into()) }, 0, 0)
            .unwrap();
        assert_eq!(kicks.len(), 2);
        let node = top("Keys").find(&["A Minor (8A)"]).unwrap();
        assert_eq!(lib.list(&node.folder, 0, 0).unwrap()[0].name, "Bass Loop Am 128bpm");
        assert_eq!(lib.list(&VirtualFolder::Recent, 0, 0).unwrap()[0].id, k.id);
    }
}
