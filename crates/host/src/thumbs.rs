//! Thumbnails of plugin editors, taken the first time a user opens a plugin.
//!
//! Never during the scan: opening editors is what hangs some plugins, and the
//! scan has to stay fast and safe. The host takes the picture a moment after
//! an editor it opened for the user has painted ([`crate::plugin`]), shrinks
//! it to [`W`]×[`H`] and keeps it in one folder, next to the scan cache:
//!
//! ```text
//! <key>.<stamp>.png    the picture, `stamp` = modification time and size of the plugin file
//! <key>.<stamp>.none   an editor that painted nothing we could see (how many times we tried)
//! ```
//!
//! A new version of the plugin changes its file, hence the stamp, hence the old
//! picture is not found (and is deleted when the new one is written). The
//! folder is kept under [`MAX_FILES`] files and [`MAX_BYTES`] bytes, oldest first.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aitrack_model::PluginFormat;

/// Pictures are this size: 3:2, twice the 48×32 pt slot of a palette row.
pub const W: u32 = 144;
pub const H: u32 = 96;
pub const MAX_FILES: usize = 400;
pub const MAX_BYTES: u64 = 24 << 20;
/// A blank editor is tried again on later opens, up to this many times.
pub const MAX_BLANK_TRIES: u32 = 3;
/// The folder, for the sandbox process (the app sets it with [`set_dir`]).
pub const ENV: &str = "AITRACK_PLUGIN_THUMBS";
/// Behind a picture that is not 3:2 (the editor is shown whole, not stretched).
const BACKGROUND: [u8; 3] = [28, 28, 30];

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Choose the folder thumbnails are kept in (the app calls this once at start).
pub fn set_dir(path: PathBuf) {
    let _ = DIR.set(path);
}

/// The folder in use: the one set with [`set_dir`], else the one the parent
/// process named in [`ENV`]. `None` turns thumbnails off.
pub fn dir() -> Option<PathBuf> {
    DIR.get().cloned().or_else(|| std::env::var_os(ENV).map(PathBuf::from))
}

/// File-name-safe form of a plugin id (`vst3:ABCD…`, `clap:com.x.y`).
pub fn key(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

/// What changes when the plugin is updated: the modification time (seconds) and size of its file.
pub fn stamp(format: PluginFormat, path: &Path) -> Option<String> {
    let k = crate::scan::FileKey::of(format, path)?;
    Some(format!("{:x}-{:x}", k.mtime_ns / 1_000_000_000, k.size))
}

fn image_path(dir: &Path, id: &str, stamp: &str) -> PathBuf {
    dir.join(format!("{}.{stamp}.png", key(id)))
}

fn blank_path(dir: &Path, id: &str, stamp: &str) -> PathBuf {
    dir.join(format!("{}.{stamp}.none", key(id)))
}

/// The picture of the current version of a plugin, if there is one.
pub fn find(dir: &Path, id: &str, stamp: &str) -> Option<PathBuf> {
    Some(image_path(dir, id, stamp)).filter(|p| p.is_file())
}

/// Is there nothing more to try for this version: a picture, or too many blank ones?
pub fn settled(dir: &Path, id: &str, stamp: &str) -> bool {
    find(dir, id, stamp).is_some() || blank_tries(dir, id, stamp) >= MAX_BLANK_TRIES
}

fn blank_tries(dir: &Path, id: &str, stamp: &str) -> u32 {
    std::fs::read_to_string(blank_path(dir, id, stamp)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// Most of the picture is one colour (an editor drawn with OpenGL/Metal that a
/// view snapshot cannot see comes out empty or flat).
pub fn is_blank(rgba: &[u8]) -> bool {
    let n = rgba.len() / 4;
    if n == 0 {
        return true;
    }
    let mut seen = std::collections::HashMap::<u16, usize>::new();
    for px in rgba.as_chunks::<4>().0.iter().step_by(7) {
        let q = if px[3] < 8 { 0xFFFF } else { u16::from(px[0] >> 4) << 8 | u16::from(px[1] >> 4) << 4 | u16::from(px[2] >> 4) };
        *seen.entry(q).or_default() += 1;
    }
    let total: usize = seen.values().sum();
    seen.values().max().is_none_or(|m| *m * 100 >= total * 97)
}

/// Shrink `src` (`w`×`h`, RGBA, alpha premultiplied or not: it is laid over
/// the background) to fit [`W`]×[`H`], centred, as RGB.
pub fn shrink(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (W * H * 3) as usize];
    for px in out.as_chunks_mut::<3>().0 {
        *px = BACKGROUND;
    }
    if w == 0 || h == 0 || src.len() < (w * h * 4) as usize {
        return out;
    }
    let scale = (f64::from(W) / f64::from(w)).min(f64::from(H) / f64::from(h)).min(1.0);
    let (tw, th) = (((f64::from(w) * scale).round() as u32).clamp(1, W), ((f64::from(h) * scale).round() as u32).clamp(1, H));
    let (ox, oy) = ((W - tw) / 2, (H - th) / 2);
    for ty in 0..th {
        let (y0, y1) = ((u64::from(ty) * u64::from(h) / u64::from(th)) as u32, (((u64::from(ty) + 1) * u64::from(h)).div_ceil(u64::from(th))) as u32);
        for tx in 0..tw {
            let (x0, x1) = ((u64::from(tx) * u64::from(w) / u64::from(tw)) as u32, (((u64::from(tx) + 1) * u64::from(w)).div_ceil(u64::from(tw))) as u32);
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    r += u32::from(src[i]);
                    g += u32::from(src[i + 1]);
                    b += u32::from(src[i + 2]);
                    a += u32::from(src[i + 3]);
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            // Average colour and coverage; what is not covered shows the background.
            let cov = (a / n).min(255);
            let mix = |c: u32, bg: u8| -> u8 { ((c / n) * cov / 255 + u32::from(bg) * (255 - cov) / 255).min(255) as u8 };
            // A premultiplied source already carries its coverage: its colour never exceeds it.
            let premultiplied = r / n <= cov && g / n <= cov && b / n <= cov && cov < 255;
            let o = (((oy + ty) * W + ox + tx) * 3) as usize;
            if premultiplied {
                let over = |c: u32, bg: u8| -> u8 { (c / n + u32::from(bg) * (255 - cov) / 255).min(255) as u8 };
                out[o] = over(r, BACKGROUND[0]);
                out[o + 1] = over(g, BACKGROUND[1]);
                out[o + 2] = over(b, BACKGROUND[2]);
            } else {
                out[o] = mix(r, BACKGROUND[0]);
                out[o + 1] = mix(g, BACKGROUND[1]);
                out[o + 2] = mix(b, BACKGROUND[2]);
            }
        }
    }
    out
}

/// Keep a picture of the editor `rgba` (`w`×`h`) for this version of the plugin.
/// A blank snapshot is not kept as a picture; it is counted so the next opens
/// try again a few times. Returns the picture's path when one was written.
pub fn store(dir: &Path, id: &str, stamp: &str, rgba: &[u8], w: u32, h: u32) -> std::io::Result<Option<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    // Another version of this plugin: its picture and notes go.
    let prefix = format!("{}.", key(id));
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && !name[prefix.len()..].starts_with(&format!("{stamp}.")) && (name.ends_with(".png") || name.ends_with(".none")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    if is_blank(rgba) {
        let tries = blank_tries(dir, id, stamp) + 1;
        std::fs::write(blank_path(dir, id, stamp), tries.to_string())?;
        return Ok(None);
    }
    let rgb = shrink(rgba, w, h);
    let target = image_path(dir, id, stamp);
    let tmp = target.with_extension("png.tmp");
    {
        use image::ImageEncoder;
        let file = std::fs::File::create(&tmp)?;
        image::codecs::png::PngEncoder::new_with_quality(std::io::BufWriter::new(file), image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive)
            .write_image(&rgb, W, H, image::ExtendedColorType::Rgb8)
            .map_err(std::io::Error::other)?;
    }
    std::fs::rename(&tmp, &target)?;
    let _ = std::fs::remove_file(blank_path(dir, id, stamp));
    prune(dir, MAX_FILES, MAX_BYTES);
    Ok(Some(target))
}

/// Delete the oldest files until at most `max_files` and `max_bytes` are left.
pub fn prune(dir: &Path, max_files: usize, max_bytes: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    files.sort_by_key(|f| f.0);
    let mut bytes: u64 = files.iter().map(|f| f.1).sum();
    let mut count = files.len();
    for (_, len, path) in files {
        if count <= max_files && bytes <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            count -= 1;
            bytes = bytes.saturating_sub(len);
        }
    }
}

/// Files and bytes in the folder (for the app's status and tests).
pub fn usage(dir: &Path) -> (usize, u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return (0, 0) };
    rd.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).fold((0, 0), |(n, b), m| (n + 1, b + m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitrack-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A 600×400 "editor": a dark panel with a bright knob grid, not one flat colour.
    fn editor(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let knob = (x / 50 + y / 50) % 2 == 0 && x % 50 > 10 && y % 50 > 10;
                let c = if knob { [220, 120, 40, 255] } else { [40 + (x % 7) as u8, 44, 50, 255] };
                v.extend_from_slice(&c);
            }
        }
        v
    }

    #[test]
    fn a_picture_is_kept_small_found_by_version_and_replaced_by_a_new_one() {
        let dir = tmp("store");
        let px = editor(600, 400);
        let p = store(&dir, "vst3:ABCD", "1-10", &px, 600, 400).unwrap().expect("written");
        assert_eq!(find(&dir, "vst3:ABCD", "1-10"), Some(p.clone()));
        assert!(settled(&dir, "vst3:ABCD", "1-10"));
        let size = std::fs::metadata(&p).unwrap().len();
        assert!(size < 40_000, "a thumbnail is a few KB, not {size}");
        let img = image::open(&p).unwrap();
        assert_eq!((img.width(), img.height()), (W, H));
        // The plugin was updated: its file has another stamp, so the old picture is not its picture ...
        assert_eq!(find(&dir, "vst3:ABCD", "2-10"), None);
        assert!(!settled(&dir, "vst3:ABCD", "2-10"));
        // ... and writing the new one removes the old.
        store(&dir, "vst3:ABCD", "2-10", &px, 600, 400).unwrap().unwrap();
        assert_eq!(find(&dir, "vst3:ABCD", "1-10"), None);
        assert_eq!(usage(&dir).0, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_blank_editor_is_not_kept_and_is_tried_a_few_times_only() {
        let dir = tmp("blank");
        let flat = vec![0u8; 300 * 200 * 4];
        for n in 1..=MAX_BLANK_TRIES {
            assert!(!settled(&dir, "clap:x", "s"), "try {n} still due");
            assert_eq!(store(&dir, "clap:x", "s", &flat, 300, 200).unwrap(), None);
        }
        assert!(settled(&dir, "clap:x", "s"), "enough tries: the row keeps its icon");
        assert_eq!(find(&dir, "clap:x", "s"), None);
        // An editor that paints later wins over the notes.
        assert!(store(&dir, "clap:x", "s", &editor(300, 200), 300, 200).unwrap().is_some());
        assert!(!blank_path(&dir, "clap:x", "s").exists());
        assert!(is_blank(&vec![30; 400]) && !is_blank(&editor(100, 100)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shrinking_keeps_the_shape_and_letterboxes_a_tall_editor() {
        // A 100×400 tall editor of solid orange: shown whole, centred, bars of background at the sides.
        let tall: Vec<u8> = (0..100 * 400).flat_map(|_| [255u8, 128, 0, 255]).collect();
        let out = shrink(&tall, 100, 400);
        let at = |x: u32, y: u32| &out[((y * W + x) * 3) as usize..][..3];
        assert_eq!(at(W / 2, H / 2), &[255, 128, 0]);
        assert_eq!(at(2, H / 2), &BACKGROUND, "side bar");
        // Premultiplied transparent corner lets the background through.
        let half: Vec<u8> = (0..16).flat_map(|_| [0u8, 0, 0, 0]).collect();
        assert_eq!(&shrink(&half, 4, 4)[..3], &BACKGROUND);
    }

    #[test]
    fn the_folder_is_capped_oldest_first() {
        let dir = tmp("cap");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..10 {
            std::fs::write(dir.join(format!("p{i}.s.png")), vec![0u8; 1000]).unwrap();
            // Distinct modification times.
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        prune(&dir, 6, 100_000);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["p4.s.png", "p5.s.png", "p6.s.png", "p7.s.png", "p8.s.png", "p9.s.png"]);
        prune(&dir, 100, 2500);
        assert_eq!(usage(&dir), (2, 2000), "by size too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_file_safe_and_the_stamp_follows_the_file() {
        assert_eq!(key("clap:com.x/Y z"), "clap_com.x_Y_z");
        let d = tmp("stamp");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("P.clap");
        std::fs::write(&f, b"abc").unwrap();
        let a = stamp(PluginFormat::Clap, &f).unwrap();
        std::fs::write(&f, b"abcdef").unwrap();
        assert_ne!(Some(a), stamp(PluginFormat::Clap, &f));
        let _ = std::fs::remove_dir_all(&d);
    }
}
