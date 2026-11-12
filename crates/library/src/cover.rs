//! Sample pack artwork: the cover image a pack ships in its folder.
//!
//! [`pack_cover`] looks in one folder for a typically named image
//! (`cover`, `artwork`, `folder`, `front`…, `.png`/`.jpg`/`.jpeg`), or the
//! only image there. [`sample_cover`] walks from a sample's folder up to its
//! pack's root folder. Both read the disk: call them off the UI thread.

use std::path::{Path, PathBuf};

/// File stems that name a cover, best first.
const NAMES: &[&str] = &["cover", "artwork", "folder", "front", "album", "art"];
const EXTS: &[&str] = &["png", "jpg", "jpeg"];

fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn images(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| is_image(p) && p.is_file()).collect();
    v.sort();
    v
}

/// The typically named cover in `dir` (no single-image fallback).
fn named(images: &[PathBuf]) -> Option<PathBuf> {
    let stem = |p: &PathBuf| p.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase).unwrap_or_default();
    NAMES.iter().find_map(|n| images.iter().find(|p| stem(p) == *n).cloned())
}

/// The cover image in a pack's root folder: a typically named one, else
/// the only image in the folder.
pub fn pack_cover(dir: &Path) -> Option<PathBuf> {
    let imgs = images(dir);
    named(&imgs).or_else(|| (imgs.len() == 1).then(|| imgs[0].clone()))
}

/// The pack's root folder for a sample: the nearest ancestor named `pack`.
/// Pure path work, no disk access.
pub fn pack_root(sample: &Path, pack: &str) -> Option<PathBuf> {
    sample.ancestors().skip(1).find(|a| a.file_name().and_then(|n| n.to_str()) == Some(pack)).map(Path::to_path_buf)
}

/// The cover for a sample: a typically named image in its folder or any
/// folder above it up to the pack root, else the pack root's only image.
/// Without a known pack root only the sample's own folder is searched.
pub fn sample_cover(sample: &Path, pack: Option<&str>) -> Option<PathBuf> {
    let dir = sample.parent()?;
    let Some(root) = pack.and_then(|p| pack_root(sample, p)) else { return pack_cover(dir) };
    for d in dir.ancestors() {
        if d == root {
            return pack_cover(d);
        }
        if let Some(c) = named(&images(d)) {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn named_cover_beats_other_images() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        touch(&d.join("promo.jpg"));
        touch(&d.join("Artwork.PNG"));
        touch(&d.join("Cover.jpeg"));
        assert_eq!(pack_cover(d), Some(d.join("Cover.jpeg")));
    }

    #[test]
    fn single_image_is_the_cover_and_several_unnamed_are_not() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        touch(&d.join("kick.wav"));
        assert_eq!(pack_cover(d), None);
        touch(&d.join("Big Pack.jpg"));
        assert_eq!(pack_cover(d), Some(d.join("Big Pack.jpg")));
        touch(&d.join("other.png"));
        assert_eq!(pack_cover(d), None);
        assert_eq!(pack_cover(&d.join("missing")), None);
    }

    #[test]
    fn sample_walks_up_to_the_pack_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Lib").join("Techno Pack");
        let sample = root.join("Drums").join("Kicks").join("kick 01.wav");
        touch(&sample);
        // Nothing yet; images above the pack root don't count.
        touch(&t.path().join("Lib").join("cover.png"));
        assert_eq!(sample_cover(&sample, Some("Techno Pack")), None);
        // The only image at the root.
        touch(&root.join("techno.jpg"));
        assert_eq!(sample_cover(&sample, Some("Techno Pack")), Some(root.join("techno.jpg")));
        // A named one in a sub-folder on the way is nearer.
        touch(&root.join("Drums").join("folder.jpg"));
        assert_eq!(sample_cover(&sample, Some("Techno Pack")), Some(root.join("Drums").join("folder.jpg")));
        // A lone unnamed image in a sub-folder is not a cover.
        touch(&root.join("Drums").join("Kicks").join("waveform.png"));
        assert_eq!(sample_cover(&sample, Some("Techno Pack")), Some(root.join("Drums").join("folder.jpg")));
    }

    #[test]
    fn pack_root_and_unknown_pack() {
        let p = Path::new("/a/Pack/b/c.wav");
        assert_eq!(pack_root(p, "Pack"), Some(PathBuf::from("/a/Pack")));
        assert_eq!(pack_root(p, "c.wav"), None);
        assert_eq!(pack_root(p, "Nope"), None);
        let t = tempfile::tempdir().unwrap();
        let s = t.path().join("x").join("s.wav");
        touch(&s);
        touch(&t.path().join("x").join("cover.jpg"));
        assert_eq!(sample_cover(&s, None), Some(t.path().join("x").join("cover.jpg")));
        assert_eq!(sample_cover(&s, Some("Elsewhere")), Some(t.path().join("x").join("cover.jpg")));
    }
}
