//! `cargo xtask plugins <list|pin|verify>`: the maintainer's side of the free-plugin catalog
//! (`content/plugins/manifest.toml`).
//!
//! `pin` downloads each artifact that has no sha256 yet, from the maker's own
//! domain through the same checks the app uses (https, allowed hosts, size
//! limit), writes its sha256 and size into the manifest and keeps the rest of
//! the file as it was. Review the diff and the maker's licence before
//! committing: a pinned hash only says "this is the file we looked at".
//!
//! `verify` downloads every pinned artifact again and fails when a hash or size
//! changed or a URL stopped answering: the maker replaced the file, so the
//! catalog entry has to be reviewed again.
//!
//! Both need the network; neither runs in `cargo test`.

use aitrack_library::catalog::{Catalog, Lane, MAX_DOWNLOAD_BYTES, Os, pin_text};
use aitrack_library::plugins::{Fetcher, HttpFetcher, file_sha256};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../content/plugins/manifest.toml")
}

pub fn run(args: &[String]) -> Result<()> {
    let path = manifest_path();
    let text = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
    let catalog = Catalog::parse(&text)?;
    match args.first().map(String::as_str) {
        Some("list") => {
            for p in &catalog.plugin {
                let state = match (p.lane, p.artifact.iter().filter(|a| a.pinned()).count(), p.artifact.len()) {
                    (Lane::Link, ..) => "link".to_owned(),
                    (Lane::Direct, pinned, all) => format!("direct, {pinned}/{all} pinned"),
                };
                println!("{:28} {:20} {state}", p.id, p.vendor);
            }
            Ok(())
        }
        Some("pin") => pin(&catalog, text, &path, &args[1..]),
        Some("verify") => verify(&catalog),
        _ => bail!("usage: cargo xtask plugins <list|pin [id...]|verify>"),
    }
}

fn pin(catalog: &Catalog, mut text: String, path: &PathBuf, only: &[String]) -> Result<()> {
    let fetcher = HttpFetcher::default();
    let stop = AtomicBool::new(false);
    let tmp = std::env::temp_dir().join("aitrack-plugin-pin");
    std::fs::create_dir_all(&tmp)?;
    let mut pinned = 0;
    for p in catalog.plugin.iter().filter(|p| p.lane == Lane::Direct && (only.is_empty() || only.contains(&p.id))) {
        for a in p.artifact.iter().filter(|a| !a.pinned()) {
            let os = a.os.context("artifact without os")?;
            let file = tmp.join("download");
            eprintln!("{} ({:?}): downloading {}", p.id, os, a.url);
            if let Err(e) = fetcher.fetch(&a.url, &p.allowed_hosts, MAX_DOWNLOAD_BYTES, &file, &stop, &mut |_, _| {}) {
                eprintln!("  skipped: {e}");
                continue;
            }
            let (sha, size) = (file_sha256(&file)?, std::fs::metadata(&file)?.len());
            let _ = std::fs::remove_file(&file);
            let Some(next) = pin_text(&text, &p.id, os, &a.arch, &sha, size) else { bail!("{}: artifact not found in the manifest text", p.id) };
            text = next;
            // Written after each one: a long run that stops half way keeps what it pinned.
            Catalog::parse(&text)?;
            std::fs::write(path, &text)?;
            eprintln!("  pinned sha256 {sha} ({size} bytes)");
            pinned += 1;
        }
    }
    eprintln!("{pinned} artifact(s) pinned. Review the diff and each maker's terms before committing.");
    Ok(())
}

fn verify(catalog: &Catalog) -> Result<()> {
    let fetcher = HttpFetcher::default();
    let stop = AtomicBool::new(false);
    let tmp = std::env::temp_dir().join("aitrack-plugin-verify");
    std::fs::create_dir_all(&tmp)?;
    let mut bad = 0;
    for p in catalog.plugin.iter().filter(|p| p.lane == Lane::Direct) {
        for a in p.artifact.iter().filter(|a| a.pinned()) {
            let file = tmp.join("download");
            let os = a.os.map_or("?", |o| if o == Os::Macos { "macos" } else { "windows" });
            match fetcher.fetch(&a.url, &p.allowed_hosts, a.size, &file, &stop, &mut |_, _| {}) {
                Ok(()) if file_sha256(&file)?.eq_ignore_ascii_case(&a.sha256) => eprintln!("ok      {} ({os})", p.id),
                Ok(()) => {
                    bad += 1;
                    eprintln!("CHANGED {} ({os}): the maker replaced the file", p.id);
                }
                Err(e) => {
                    bad += 1;
                    eprintln!("FAILED  {} ({os}): {e}", p.id);
                }
            }
            let _ = std::fs::remove_file(&file);
        }
    }
    if bad > 0 {
        bail!("{bad} artifact(s) need review");
    }
    Ok(())
}
