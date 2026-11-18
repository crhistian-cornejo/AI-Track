//! Project tasks. Run with `cargo xtask <task>`.
//!
//! `cargo xtask soundbank list`                       packs, licenses, download state
//! `cargo xtask soundbank fetch [--only id] [--max-size-mb N] [--with-optional] [--write-manifest] [--no-index]`
//! `cargo xtask soundbank verify`                     hashes, LICENSE files, file lists
//! `cargo xtask soundbank index [--no-analyse] [--threads N]`
//!                                                    rebuild content/soundbank/factory.db, then
//!                                                    detect missing key/BPM from audio (cached)
//! `cargo xtask soundbank attribution`                regenerate ATTRIBUTION.md
//! `cargo xtask plugins <list|pin|verify>`            free-plugin catalog: pin sha256 and size (downloads), check pinned ones
//! `cargo xtask icons`                                regenerate release/icons (AppIcon.icns, aitrack.ico) from the brand SVG
//! `cargo xtask package [--target mac|win] [--universal] [--sign] [--skip-build]`
//!                                                    installers and updater artifacts in target/dist (docs/release-0.0.1.md)
//! `cargo xtask package manifest --version X --base-url URL [--notes-file F]`
//!                                                    latest.json + SHA256SUMS.txt from the platform-*.json fragments
//! `cargo xtask updater-keygen --out <dir>`           new minisign key pair for the auto-updater

mod icons;
mod package;
mod plugins;
mod soundbank;
mod updates;

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("soundbank") => soundbank::run(&args[1..]),
        Some("plugins") => plugins::run(&args[1..]),
        Some("icons") => icons::run(&repo_root()),
        Some("package") => package::run(&repo_root(), &args[1..]),
        Some("updater-keygen") => updates::keygen_command(&repo_root(), &args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask soundbank <list|fetch|verify|index|attribution> [options]\n       cargo xtask plugins <list|pin [id...]|verify>\n       cargo xtask icons\n       cargo xtask package [--target mac|win] [--universal] [--sign]\n       cargo xtask updater-keygen --out <dir>\n\
                 \n  fetch options: --only <id> (repeatable)  --max-size-mb <N>  --with-optional  --write-manifest  --no-index\n\
                 \n  index options: --no-analyse (skip key/BPM from audio)  --threads <N>"
            );
            bail!("unknown task")
        }
    }
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}
