//! Embeds the factory presets (`presets/<plugin-id>/<Category>/<Name>.json`)
//! into the binary so they ship without loose files.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("presets");
    println!("cargo:rerun-if-changed=presets");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut code = String::from("/// `(relative path, json)` of every factory preset.\npub static FACTORY: &[(&str, &str)] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let abs = f.to_string_lossy().replace('\\', "/");
        let _ = writeln!(code, "    ({rel:?}, include_str!({abs:?})),");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("factory_presets.rs");
    std::fs::write(out, code).unwrap();
}
