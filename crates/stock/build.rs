//! Embeds the factory presets (`presets/<plugin-id>/<Category>/<Name>.json`)
//! and the factory racks (`racks/<kind>/<Name>.aitrack-rack`) into the binary
//! so they ship without loose files.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Every file under `dir` whose extension is `ext`.
fn collect(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

/// Write `OUT_DIR/<out_file>`: a `pub static <name>` of `(path relative to
/// <dir>, text)` for every `*.<ext>` under `<dir>` of the package, by path.
fn embed(manifest: &Path, dir: &str, ext: &str, doc: &str, name: &str, out_file: &str) {
    let root = manifest.join(dir);
    println!("cargo:rerun-if-changed={dir}");
    let mut files = Vec::new();
    collect(&root, ext, &mut files);
    files.sort();
    let mut code = format!("/// {doc}\npub static {name}: &[(&str, &str)] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let abs = f.to_string_lossy().replace('\\', "/");
        let _ = writeln!(code, "    ({rel:?}, include_str!({abs:?})),");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(out_file);
    std::fs::write(out, code).unwrap();
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    embed(&manifest, "presets", "json", "`(relative path, json)` of every factory preset.", "FACTORY", "factory_presets.rs");
    embed(&manifest, "racks", "aitrack-rack", "`(relative path, json)` of every factory rack.", "FACTORY_RACKS", "factory_racks.rs");
}
