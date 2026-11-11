//! Project tasks. Run with `cargo xtask <task>`.
//!
//! `cargo xtask soundbank list`                       packs, licenses, download state
//! `cargo xtask soundbank fetch [--only id] [--max-size-mb N] [--write-manifest] [--no-index]`
//! `cargo xtask soundbank verify`                     hashes, LICENSE files, file lists
//! `cargo xtask soundbank index`                      rebuild content/soundbank/factory.db
//! `cargo xtask soundbank attribution`                regenerate ATTRIBUTION.md

mod soundbank;

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("soundbank") => soundbank::run(&args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask soundbank <list|fetch|verify|index|attribution> [options]\n\
                 \n  fetch options: --only <id> (repeatable)  --max-size-mb <N>  --write-manifest  --no-index"
            );
            bail!("unknown task")
        }
    }
}
