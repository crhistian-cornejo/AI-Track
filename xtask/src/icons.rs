//! `cargo xtask icons`: regenerates `release/icons/AppIcon.icns` (macOS) and
//! `release/icons/aitrack.ico` (Windows) from `assets/brand/aitrack-logo.svg`.
//!
//! Both containers are written here (PNG payloads only), so the result is the same on every
//! host and does not depend on `iconutil`. The macOS art follows Apple's template (the tile
//! occupies 80 % of the canvas); the Windows art is full-bleed.

use anyhow::{Context, Result, bail};
use resvg::{tiny_skia, usvg};
use std::path::Path;

/// (icns type, pixel size). `icp4`/`icp5` are 16/32 px; `ic11..ic14` are the @2x variants.
const ICNS_ENTRIES: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

pub fn run(root: &Path) -> Result<()> {
    let svg = std::fs::read(root.join("assets/brand/aitrack-logo.svg")).context("assets/brand/aitrack-logo.svg")?;
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).context("parsing the logo SVG")?;
    let out = root.join("release/icons");
    std::fs::create_dir_all(&out)?;

    let mut icns_pngs = Vec::new();
    for (ty, px) in ICNS_ENTRIES {
        icns_pngs.push((*ty, render_png(&tree, px, 0.8)?));
    }
    std::fs::write(out.join("AppIcon.icns"), write_icns(&icns_pngs))?;

    let mut ico_pngs = Vec::new();
    for px in ICO_SIZES {
        ico_pngs.push((px, render_png(&tree, px, 1.0)?));
    }
    std::fs::write(out.join("aitrack.ico"), write_ico(&ico_pngs)?)?;
    println!("wrote {} and {}", out.join("AppIcon.icns").display(), out.join("aitrack.ico").display());
    Ok(())
}

/// The SVG tile scaled to `fill` (0..=1) of a square canvas of `px`, centred, as PNG bytes.
fn render_png(tree: &usvg::Tree, px: u32, fill: f32) -> Result<Vec<u8>> {
    let mut pixmap = tiny_skia::Pixmap::new(px, px).context("pixmap")?;
    let size = tree.size();
    let scale = px as f32 * fill / size.width().max(size.height());
    let offset_x = (px as f32 - size.width() * scale) / 2.0;
    let offset_y = (px as f32 - size.height() * scale) / 2.0;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(offset_x, offset_y);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().context("encoding PNG")
}

fn write_icns(entries: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let total: usize = 8 + entries.iter().map(|(_, d)| 8 + d.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(total as u32).to_be_bytes());
    for (ty, data) in entries {
        out.extend_from_slice(ty);
        out.extend_from_slice(&((8 + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(data);
    }
    out
}

fn write_ico(entries: &[(u32, Vec<u8>)]) -> Result<Vec<u8>> {
    if entries.is_empty() || entries.len() > u16::MAX as usize {
        bail!("bad ICO entry count");
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len() as u32;
    for (px, data) in entries {
        let dim = if *px >= 256 { 0 } else { *px as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in entries {
        out.extend_from_slice(data);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icns_header_and_lengths_add_up() {
        let icns = write_icns(&[(*b"ic07", vec![1, 2, 3]), (*b"ic08", vec![4; 10])]);
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize, icns.len());
        assert_eq!(icns.len(), 8 + (8 + 3) + (8 + 10));
        assert_eq!(&icns[8..12], b"ic07");
        assert_eq!(u32::from_be_bytes(icns[12..16].try_into().unwrap()), 11);
    }

    #[test]
    fn ico_directory_points_at_the_payloads() {
        let ico = write_ico(&[(16, vec![9; 5]), (256, vec![8; 7])]).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
        // First entry: 16 px, 5 bytes at offset 6 + 32.
        assert_eq!(ico[6], 16);
        assert_eq!(u32::from_le_bytes(ico[14..18].try_into().unwrap()), 5);
        assert_eq!(u32::from_le_bytes(ico[18..22].try_into().unwrap()), 38);
        // Second entry: 256 px is stored as 0, payload right after the first.
        assert_eq!(ico[22], 0);
        assert_eq!(u32::from_le_bytes(ico[34..38].try_into().unwrap()), 43);
        assert_eq!(ico.len(), 38 + 5 + 7);
    }

    #[test]
    fn the_brand_svg_renders_to_png() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let svg = std::fs::read(root.join("assets/brand/aitrack-logo.svg")).unwrap();
        let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).unwrap();
        let png = render_png(&tree, 64, 0.8).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
}
