//! Embeds the updater's public key. `release/updater.pub` is the real minisign
//! public key; until it exists the placeholder `release/updater.pub.example` is
//! embedded instead and `updater_unconfigured` is set, which disables installing.

use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("../../release");
    // The directory, not the files: a missing file would make Cargo rerun every build.
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(updater_unconfigured)");
    let real = root.join("updater.pub");
    let (source, configured) = if real.is_file() { (real, true) } else { (root.join("updater.pub.example"), false) };
    let out = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("updater.pub");
    fs::copy(&source, &out).unwrap_or_else(|e| panic!("cannot read {}: {e}", source.display()));
    if !configured {
        println!("cargo:rustc-cfg=updater_unconfigured");
    }
}
