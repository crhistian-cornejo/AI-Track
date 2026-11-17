//! The preset side of the window chrome, as plain data: the list a device
//! offers (the factory bank, then the user's own), moving through it, whether
//! the device still matches the preset it loaded, the user's saved presets on
//! disk, and the A/B comparison slots.
//!
//! Nothing here touches the project: the chrome turns these answers into
//! `Command`s (`Workspace::load_preset` and friends), so every preset move,
//! recall and reset is one undo step like any other edit.

use aitrack_stock::presets::{Preset, PresetError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The category the user's own presets are listed under.
pub const USER: &str = "User";

/// Values within this distance of each other are the same setting.
const SAME: f32 = 1e-4;

/// One preset a device offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Factory path (`plugin/Category/Name.json`) or the file of a user preset.
    pub path: String,
    pub category: String,
    pub name: String,
    pub user: bool,
}

/// Every preset of a device in menu order: the user's first, then the
/// factory ones by category and name.
pub fn merge(factory: Vec<Entry>, user: Vec<Entry>) -> Vec<Entry> {
    let mut list = user;
    list.sort_by_key(|e| e.name.to_lowercase());
    let mut factory = factory;
    factory.sort_by(|a, b| (a.category.as_str(), a.name.as_str()).cmp(&(b.category.as_str(), b.name.as_str())));
    list.extend(factory);
    list
}

/// The entries grouped by consecutive category, in order.
pub fn by_category(list: &[Entry]) -> Vec<(&str, Vec<&Entry>)> {
    let mut out: Vec<(&str, Vec<&Entry>)> = Vec::new();
    for e in list {
        match out.last_mut() {
            Some((c, items)) if *c == e.category => items.push(e),
            _ => out.push((e.category.as_str(), vec![e])),
        }
    }
    out
}

/// Where the preset named `current` sits in `list`.
pub fn position(list: &[Entry], current: Option<&str>) -> Option<usize> {
    let name = current?;
    list.iter().position(|e| e.name == name)
}

/// The preset `dir` steps from the loaded one, wrapping at both ends. With
/// none loaded, forward starts at the first and backward at the last.
pub fn step(len: usize, current: Option<usize>, dir: i32) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(i) => (i as isize + dir.signum() as isize).rem_euclid(len as isize) as usize,
        None if dir >= 0 => 0,
        None => len - 1,
    })
}

/// Whether `values` (what the device stores, missing = default) differ from
/// what `preset` sets. `defaults` gives a parameter's default.
pub fn differs(preset: &BTreeMap<u32, f32>, values: &BTreeMap<u32, f32>, defaults: impl Fn(u32) -> Option<f32>) -> bool {
    preset.iter().any(|(&id, &v)| {
        let Some(default) = defaults(id) else { return false };
        (values.get(&id).copied().unwrap_or(default) - v).abs() > SAME
    })
}

// ───────────────────────────── User presets ─────────────────────────────

/// The folder of the user's presets of one plugin.
pub fn user_dir(root: &Path, plugin: &str) -> PathBuf {
    root.join(plugin)
}

/// A name the file system takes, or `None` when nothing usable is left.
pub fn clean_name(name: &str) -> Option<String> {
    let cleaned: String = name.chars().filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c)).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    (!cleaned.is_empty()).then(|| cleaned.chars().take(48).collect())
}

/// The user's presets of `plugin` found in `root`, by name.
pub fn list_user(root: &Path, plugin: &str) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(user_dir(root, plugin)) else { return Vec::new() };
    let mut out: Vec<Entry> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let name = p.file_stem()?.to_str()?.to_owned();
            Some(Entry { path: p.to_string_lossy().into_owned(), category: USER.to_owned(), name, user: true })
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// Write the parameters as the user's preset `name`; returns the entry and
/// whether it replaced one of the same name.
pub fn save_user(root: &Path, plugin: &str, name: &str, params: &BTreeMap<u32, f32>) -> Result<(Entry, bool), String> {
    let name = clean_name(name).ok_or_else(|| "Give the preset a name".to_owned())?;
    let dir = user_dir(root, plugin);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot save the preset: {e}"))?;
    let path = dir.join(format!("{name}.json"));
    let existed = path.exists();
    let preset = Preset {
        name: name.clone(),
        plugin: plugin.to_owned(),
        author: String::new(),
        tags: Vec::new(),
        params: params.iter().map(|(id, v)| (id.to_string(), *v)).collect(),
    };
    std::fs::write(&path, preset.to_json() + "\n").map_err(|e| format!("Cannot save the preset: {e}"))?;
    Ok((Entry { path: path.to_string_lossy().into_owned(), category: USER.to_owned(), name, user: true }, existed))
}

/// The parameters of a preset file or factory path.
pub fn read(path: &str) -> Result<(String, BTreeMap<u32, f32>), PresetError> {
    let p = aitrack_stock::presets::load(path)?;
    let params = p.params.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.clamp(0., 1.)))).collect();
    Ok((p.name, params))
}

/// A name for a new preset: the loaded one's, or the device's, with a
/// number that makes it new.
pub fn suggest_name(loaded: Option<&str>, device: &str, taken: &[Entry]) -> String {
    let base = loaded.map_or_else(|| format!("{device} custom"), |l| format!("{l} edit"));
    if !taken.iter().any(|e| e.user && e.name == base) {
        return base;
    }
    (2..).map(|n| format!("{base} {n}")).find(|n| !taken.iter().any(|e| e.user && &e.name == n)).unwrap_or(base)
}

// ───────────────────────────── A/B ─────────────────────────────

/// Two full settings of a device to flip between while listening.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ab {
    slots: [Option<BTreeMap<u32, f32>>; 2],
    /// 0 = A, 1 = B.
    pub active: usize,
}

impl Ab {
    /// Make `slot` the active one. `now` is the device's current settings: they
    /// are kept in the slot being left; an empty target starts as a copy of
    /// them. Returns the settings to apply (`None` when there is no change).
    pub fn switch(&mut self, slot: usize, now: &BTreeMap<u32, f32>) -> Option<BTreeMap<u32, f32>> {
        let slot = slot.min(1);
        if slot == self.active {
            return None;
        }
        self.slots[self.active] = Some(now.clone());
        self.active = slot;
        let target = self.slots[slot].get_or_insert_with(|| now.clone());
        Some(target.clone())
    }

    /// Copy the active slot's settings (`now`) into the other one.
    pub fn copy_to_other(&mut self, now: &BTreeMap<u32, f32>) {
        self.slots[self.active] = Some(now.clone());
        self.slots[1 - self.active] = Some(now.clone());
    }

    /// Whether the other slot holds something different from `now`.
    pub fn other_differs(&self, now: &BTreeMap<u32, f32>) -> bool {
        self.slots[1 - self.active].as_ref().is_some_and(|o| o.iter().any(|(id, v)| now.get(id).is_none_or(|n| (n - v).abs() > SAME)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(category: &str, name: &str, user: bool) -> Entry {
        Entry { path: format!("{category}/{name}"), category: category.into(), name: name.into(), user }
    }

    fn map(v: &[(u32, f32)]) -> BTreeMap<u32, f32> {
        v.iter().copied().collect()
    }

    #[test]
    fn the_menu_lists_the_users_presets_first_then_factory_by_category() {
        let list = merge(
            vec![entry("Mixing", "Glue", false), entry("Drums", "Punch", false), entry("Drums", "Air", false)],
            vec![entry(USER, "zeta", true), entry(USER, "Alpha", true)],
        );
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta", "Air", "Punch", "Glue"]);
        let groups = by_category(&list);
        assert_eq!(groups.iter().map(|g| (g.0, g.1.len())).collect::<Vec<_>>(), [(USER, 2), ("Drums", 2), ("Mixing", 1)]);
    }

    #[test]
    fn stepping_wraps_and_starts_from_the_ends() {
        assert_eq!(step(0, None, 1), None);
        assert_eq!(step(4, None, 1), Some(0));
        assert_eq!(step(4, None, -1), Some(3));
        assert_eq!(step(4, Some(3), 1), Some(0));
        assert_eq!(step(4, Some(0), -1), Some(3));
        assert_eq!(step(4, Some(1), 5), Some(2), "only the direction counts");
        let list = [entry("A", "one", false), entry("A", "two", false)];
        assert_eq!(position(&list, Some("two")), Some(1));
        assert_eq!(position(&list, Some("gone")), None);
        assert_eq!(position(&list, None), None);
    }

    #[test]
    fn a_device_matches_its_preset_until_a_parameter_moves() {
        let preset = map(&[(0, 0.25), (1, 0.5)]);
        let default = |id: u32| Some(if id == 1 { 0.5 } else { 0. });
        // Parameter 1 is at its default (not stored) and matches the preset's 0.5.
        assert!(!differs(&preset, &map(&[(0, 0.25)]), default));
        assert!(differs(&preset, &map(&[(0, 0.3)]), default));
        assert!(differs(&preset, &map(&[(0, 0.25), (1, 0.9)]), default));
        // A parameter the plugin no longer has does not count.
        assert!(!differs(&map(&[(7, 1.)]), &map(&[]), default_none));
        fn default_none(_: u32) -> Option<f32> {
            None
        }
    }

    #[test]
    fn names_are_made_safe_for_files() {
        assert_eq!(clean_name("  Warm / glue: v2 "), Some("Warm  glue v2".to_owned()));
        assert_eq!(clean_name("..hidden"), Some("hidden".to_owned()));
        assert_eq!(clean_name(" / "), None);
        assert_eq!(clean_name("").as_deref(), None);
        assert_eq!(clean_name(&"x".repeat(100)).unwrap().len(), 48);
    }

    #[test]
    fn a_saved_preset_is_listed_and_reads_back() {
        let root = std::env::temp_dir().join(format!("aitrack-dw-presets-{}", std::process::id()));
        let params = map(&[(0, 0.125), (3, 1.)]);
        let (e, existed) = save_user(&root, "fx.test", "My Bus", &params).unwrap();
        assert!(!existed && e.user && e.category == USER && e.name == "My Bus");
        assert!(save_user(&root, "fx.test", "My Bus", &params).unwrap().1, "saving again replaces it");
        assert!(save_user(&root, "fx.test", "///", &params).is_err());
        save_user(&root, "fx.test", "alpha", &params).unwrap();
        save_user(&root, "fx.other", "elsewhere", &params).unwrap();
        let listed = list_user(&root, "fx.test");
        assert_eq!(listed.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["alpha", "My Bus"]);
        assert_eq!(read(&listed[1].path).unwrap(), ("My Bus".to_owned(), params));
        assert!(list_user(&root, "fx.none").is_empty());
        assert_eq!(suggest_name(Some("My Bus"), "EQ", &listed), "My Bus edit");
        assert_eq!(suggest_name(None, "EQ", &listed), "EQ custom");
        let taken = [entry(USER, "EQ custom", true), entry(USER, "EQ custom 2", true)];
        assert_eq!(suggest_name(None, "EQ", &taken), "EQ custom 3");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ab_keeps_both_settings_and_a_fresh_b_starts_as_a_copy() {
        let a = map(&[(0, 0.1)]);
        let b = map(&[(0, 0.9)]);
        let mut ab = Ab::default();
        assert_eq!(ab.active, 0);
        assert_eq!(ab.switch(0, &a), None, "already on A");
        // First visit to B: nothing stored, so B starts as what A was.
        assert_eq!(ab.switch(1, &a), Some(a.clone()));
        assert_eq!(ab.active, 1);
        // Edit B, go back: A comes back as it was left, B keeps the edit.
        assert_eq!(ab.switch(0, &b), Some(a.clone()));
        assert!(ab.other_differs(&a), "B holds the edit now");
        assert_eq!(ab.switch(1, &a), Some(b.clone()));
        // Copy B over A.
        ab.copy_to_other(&b);
        assert!(!ab.other_differs(&b));
        assert_eq!(ab.switch(0, &b), Some(b));
    }
}
