//! `cargo xtask soundbank`: downloads the redistributable packs listed in
//! `content/soundbank/manifest.toml`, verifies them, normalizes them into
//! `content/soundbank/packs/<id>/` (with the pack's LICENSE next to it) and
//! indexes them into `content/soundbank/factory.db`.
//!
//! Packs are fetched one at a time. Every download goes to a `.part` file and
//! is renamed only when complete, so an interrupted run never leaves a
//! truncated archive that looks finished; cached archives are re-hashed
//! before use.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use aitrack_library::hub::{ArchiveFormat, BundledPack, GithubSource, Manifest};
use aitrack_library::{BasicAnalyzer, Library, PackInfo, RootKind, ScanOptions};
use anyhow::{Context, Result, anyhow, bail};
use sha1::Digest as _;

const UA: &str = "aitrack-xtask-soundbank/0.1";
const FILES_LIST: &str = ".files.tsv";

pub fn run(args: &[String]) -> Result<()> {
    let paths = Paths::new()?;
    let cmd = args.first().map(String::as_str).unwrap_or("list");
    let mut only: Vec<String> = Vec::new();
    let mut max_mb: Option<f64> = None;
    let mut write_manifest = false;
    let mut index = true;
    let mut with_optional = false;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--only" => only.push(it.next().ok_or_else(|| anyhow!("--only needs an id"))?.clone()),
            "--max-size-mb" => {
                max_mb = Some(it.next().ok_or_else(|| anyhow!("--max-size-mb needs a number"))?.parse()?)
            }
            "--write-manifest" => write_manifest = true,
            "--no-index" => index = false,
            "--with-optional" => with_optional = true,
            other => bail!("unknown option {other}"),
        }
    }
    let manifest = Manifest::parse(&fs::read_to_string(&paths.manifest)?)?;
    match cmd {
        "list" => list(&paths, &manifest),
        "fetch" => {
            fetch(&paths, &manifest, &only, max_mb, write_manifest, with_optional)?;
            if index {
                build_index(&paths, &Manifest::parse(&fs::read_to_string(&paths.manifest)?)?)?;
            }
            write_attribution(&paths, &manifest)
        }
        "verify" => verify(&paths, &manifest),
        "index" => build_index(&paths, &manifest),
        "attribution" => write_attribution(&paths, &manifest),
        other => bail!("unknown soundbank command {other}"),
    }
}

struct Paths {
    bank: PathBuf,
    manifest: PathBuf,
    packs: PathBuf,
    cache: PathBuf,
    db: PathBuf,
}

impl Paths {
    fn new() -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| anyhow!("no workspace root"))?
            .to_path_buf();
        let bank = root.join("content").join("soundbank");
        Ok(Self {
            manifest: bank.join("manifest.toml"),
            packs: bank.join("packs"),
            cache: bank.join(".cache"),
            db: bank.join("factory.db"),
            bank,
        })
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list(paths: &Paths, m: &Manifest) -> Result<()> {
    println!("Bundled packs ({}):", m.packs.len());
    let mut total = 0.0;
    for p in &m.packs {
        let dir = paths.packs.join(p.dir_name());
        let state = match (dir.join("LICENSE").is_file(), p.optional) {
            (true, _) => "fetched",
            (false, true) => "optional",
            (false, false) => "-",
        };
        println!("  {:<30} {:>8.1} MB  {:<10} {:<8} {}", p.id, p.size_mb, p.license, state, p.name);
        if !p.optional {
            total += p.size_mb;
        }
    }
    println!("  default bank {total:.1} MB (optional packs excluded)");
    let by_kind = |k| m.hub.iter().filter(|h| h.kind == k).count();
    println!(
        "Content Hub: {} entries ({} sample packs, {} instruments, {} plugins), link-only",
        m.hub.len(),
        by_kind(aitrack_library::hub::HubKind::SamplePack),
        by_kind(aitrack_library::hub::HubKind::Instrument),
        by_kind(aitrack_library::hub::HubKind::Plugin)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn agent() -> ureq::Agent {
    // The OS verifier completes chains that miss an intermediate (as browsers
    // do); certificate checking stays fully on.
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let mut req = agent.get(url).header("User-Agent", UA);
    if url.starts_with("https://api.github.com/")
        && let Ok(tok) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN"))
    {
        req = req.header("Authorization", &format!("Bearer {tok}"));
    }
    req.call().with_context(|| format!("GET {url}"))
}

/// Streams `url` into `dest` (via `dest.part`), returning the sha256.
fn download(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<String> {
    let resp = get(agent, url)?;
    let len: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let part = dest.with_extension("part");
    if let Some(parent) = part.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut reader = resp.into_body().into_reader();
    let mut out = BufWriter::new(File::create(&part)?);
    let mut sha = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut got = 0u64;
    let mut next_report = 0u64;
    let start = Instant::now();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        got += n as u64;
        if got >= next_report {
            match len {
                Some(l) => eprint!("\r    {:.1} / {:.1} MB", got as f64 / 1e6, l as f64 / 1e6),
                None => eprint!("\r    {:.1} MB", got as f64 / 1e6),
            }
            next_report = got + 8_000_000;
        }
    }
    out.flush()?;
    drop(out);
    if let Some(l) = len
        && l != got
    {
        bail!("truncated download of {url}: {got} of {l} bytes");
    }
    eprintln!("\r    {:.1} MB in {:.0} s", got as f64 / 1e6, start.elapsed().as_secs_f64());
    fs::rename(&part, dest)?;
    Ok(hex(&sha.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = BufReader::new(File::open(path)?);
    let mut sha = sha2::Sha256::new();
    std::io::copy(&mut f, &mut sha)?;
    Ok(hex(&sha.finalize()))
}

/// Git blob id: sha1("blob <len>\0" + content).
fn git_blob_sha(path: &Path) -> Result<String> {
    let len = fs::metadata(path)?.len();
    let mut sha = sha1::Sha1::new();
    sha.update(format!("blob {len}\0").as_bytes());
    let mut f = BufReader::new(File::open(path)?);
    std::io::copy(&mut f, &mut sha)?;
    Ok(hex(&sha.finalize()))
}

fn url_encode_path(p: &str) -> String {
    let mut s = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

fn fetch(
    paths: &Paths,
    m: &Manifest,
    only: &[String],
    max_mb: Option<f64>,
    write_manifest: bool,
    with_optional: bool,
) -> Result<()> {
    for id in only {
        if m.pack(id).is_none() {
            bail!("no bundled pack with id {id}");
        }
    }
    fs::create_dir_all(&paths.packs)?;
    fs::create_dir_all(&paths.cache)?;
    let agent = agent();
    let mut budget = 0.0;
    let mut updates: Vec<(String, String, u64)> = Vec::new();
    for p in &m.packs {
        if !only.is_empty() && !only.contains(&p.id) {
            continue;
        }
        if p.optional && only.is_empty() && !with_optional {
            println!("skip {} ({:.0} MB): optional pack, use --only {} or --with-optional", p.id, p.size_mb, p.id);
            continue;
        }
        // Belt and braces: the manifest parser already enforces this.
        if !p.redistributable {
            println!("skip {}: not redistributable", p.id);
            continue;
        }
        if let Some(max) = max_mb
            && budget + p.size_mb > max
        {
            println!("skip {} ({:.0} MB): over --max-size-mb {max}", p.id, p.size_mb);
            continue;
        }
        budget += p.size_mb;
        println!("==> {} ({}, {:.1} MB)", p.name, p.license, p.size_mb);
        let sha = match p.format {
            ArchiveFormat::GithubTree => fetch_github(paths, &agent, p)?,
            _ => fetch_archive(paths, &agent, p)?,
        };
        let dir = paths.packs.join(p.dir_name());
        write_source(p, &dir, &sha)?;
        let count = count_audio(&dir);
        println!("    {} audio files, sha256 {sha}", count);
        updates.push((p.id.clone(), sha, count));
    }
    if write_manifest {
        let mut text = fs::read_to_string(&paths.manifest)?;
        for (id, sha, count) in &updates {
            text = set_pack_field(&text, id, "sha256", &format!("\"{sha}\""))?;
            text = set_pack_field(&text, id, "sample_count", &count.to_string())?;
        }
        Manifest::parse(&text)?;
        fs::write(&paths.manifest, text)?;
        println!("manifest updated ({} packs)", updates.len());
    }
    Ok(())
}

fn fetch_archive(paths: &Paths, agent: &ureq::Agent, p: &BundledPack) -> Result<String> {
    let ext = match p.format {
        ArchiveFormat::Zip => "zip",
        ArchiveFormat::TarGz => "tar.gz",
        ArchiveFormat::TarXz => "tar.xz",
        ArchiveFormat::SevenZ => "7z",
        ArchiveFormat::GithubTree => unreachable!(),
    };
    let archive = paths.cache.join(format!("{}.{ext}", p.id));
    let mut sha = None;
    if archive.is_file() {
        let have = sha256_file(&archive)?;
        if p.sha256.is_empty() || p.sha256 == have {
            println!("    cached archive ok");
            sha = Some(have);
        } else {
            println!("    cached archive hash mismatch, downloading again");
            fs::remove_file(&archive)?;
        }
    }
    let sha = match sha {
        Some(s) => s,
        None => download(agent, &p.download_url, &archive)?,
    };
    if !p.sha256.is_empty() && p.sha256 != sha {
        fs::remove_file(&archive).ok();
        bail!("{}: sha256 mismatch (manifest {}, got {sha})", p.id, p.sha256);
    }

    let raw = paths.cache.join(format!("{}.extract", p.id));
    if raw.exists() {
        fs::remove_dir_all(&raw)?;
    }
    fs::create_dir_all(&raw)?;
    println!("    extracting");
    match p.format {
        ArchiveFormat::Zip => extract_zip(&archive, &raw)?,
        ArchiveFormat::TarGz => {
            let gz = flate2::read::GzDecoder::new(BufReader::new(File::open(&archive)?));
            extract_tar(gz, &raw)?
        }
        ArchiveFormat::TarXz => {
            // Pure-Rust xz: decompress to a temporary tar, then unpack it.
            let tar_path = paths.cache.join(format!("{}.tar", p.id));
            {
                let mut input = BufReader::new(File::open(&archive)?);
                let mut out = BufWriter::new(File::create(&tar_path)?);
                lzma_rs::xz_decompress(&mut input, &mut out).map_err(|e| anyhow!("xz: {e:?}"))?;
                out.flush()?;
            }
            extract_tar(BufReader::new(File::open(&tar_path)?), &raw)?;
            fs::remove_file(&tar_path)?;
        }
        ArchiveFormat::SevenZ => {
            sevenz_rust2::decompress_file(&archive, &raw).map_err(|e| anyhow!("7z: {e}"))?;
            // Reject anything that escaped the folder (zip-slip style names).
            for e in walkdir::WalkDir::new(&raw) {
                let e = e?;
                if !e.path().starts_with(&raw) {
                    bail!("7z entry outside target: {}", e.path().display());
                }
            }
        }
        ArchiveFormat::GithubTree => unreachable!(),
    }

    let dest = paths.packs.join(p.dir_name());
    let staging = paths.packs.join(format!(".{}.staging", p.dir_name()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    normalize(&raw, &staging, p)?;
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&staging, &dest)?;
    fs::remove_dir_all(&raw)?;
    Ok(sha)
}

fn extract_zip(archive: &Path, out: &Path) -> Result<()> {
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive)?))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let Some(rel) = f.enclosed_name() else {
            println!("    skipping unsafe entry {}", f.name());
            continue;
        };
        let target = out.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut w = BufWriter::new(File::create(&target)?);
        std::io::copy(&mut f, &mut w)?;
    }
    Ok(())
}

fn extract_tar<R: Read>(r: R, out: &Path) -> Result<()> {
    let mut a = tar::Archive::new(r);
    for entry in a.entries()? {
        let mut e = entry?;
        // `unpack_in` refuses paths that would escape `out`.
        if !e.unpack_in(out)? {
            println!("    skipping unsafe entry {}", e.path()?.display());
        }
    }
    Ok(())
}

fn is_junk(rel: &str) -> bool {
    rel.split('/').any(|c| c == "__MACOSX" || c == ".DS_Store" || c.starts_with("._") || c == "Thumbs.db")
}

fn keep(rel: &str, p: &BundledPack) -> bool {
    if is_junk(rel) {
        return false;
    }
    if !p.include.is_empty() && !p.include.iter().any(|i| rel.starts_with(i.as_str())) {
        return false;
    }
    !p.exclude.iter().any(|x| rel.contains(x.as_str()))
}

/// Moves the extracted tree into `dest`, stripping the archive's top folder
/// (explicit `strip_prefix`, or automatically when there is a single one),
/// applying include/exclude, and placing the license file.
fn normalize(raw: &Path, dest: &Path, p: &BundledPack) -> Result<()> {
    let mut base = raw.to_path_buf();
    if !p.strip_prefix.is_empty() {
        base = raw.join(p.strip_prefix.trim_end_matches('/'));
    } else {
        loop {
            let entries: Vec<_> = fs::read_dir(&base)?
                .filter_map(|e| e.ok())
                .filter(|e| !is_junk(&e.file_name().to_string_lossy()))
                .collect();
            if entries.len() == 1 && entries[0].path().is_dir() {
                base = entries[0].path();
            } else {
                break;
            }
        }
    }
    fs::create_dir_all(dest)?;
    let mut license: Option<PathBuf> = None;
    for e in walkdir::WalkDir::new(&base) {
        let e = e?;
        if !e.file_type().is_file() {
            continue;
        }
        let rel = e.path().strip_prefix(&base)?.to_string_lossy().replace('\\', "/");
        let full_rel = e.path().strip_prefix(raw)?.to_string_lossy().replace('\\', "/");
        if !p.license_file.is_empty() && (full_rel == p.license_file || rel == p.license_file) {
            license = Some(e.path().to_path_buf());
        }
        if !keep(&rel, p) {
            continue;
        }
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(e.path(), &target).or_else(|_| fs::copy(e.path(), &target).map(|_| ()))?;
        if license.as_deref() == Some(e.path()) {
            license = Some(target);
        }
    }
    if license.is_none() {
        // Look for the pack's own license/readme text.
        for e in walkdir::WalkDir::new(dest).max_depth(2).into_iter().filter_map(|e| e.ok()) {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if e.file_type().is_file() && (n.starts_with("license") || n.starts_with("licence")) {
                license = Some(e.path().to_path_buf());
                break;
            }
        }
    }
    write_license(p, dest, license.as_deref())
}

/// `LICENSE` next to the pack: the pack's own license text when it ships one,
/// otherwise a notice with the SPDX id, URL and the evidence we checked.
fn write_license(p: &BundledPack, dest: &Path, original: Option<&Path>) -> Result<()> {
    let target = dest.join("LICENSE");
    let notice = format!(
        "{name}\nAuthor: {author}\nSource: {home}\nLicense: {lic} ({url})\nAttribution: {credit}\n\nLicense evidence (checked 2026-11-11): {ev}\n",
        name = p.name,
        author = p.author,
        home = p.homepage,
        lic = p.license,
        url = p.license_url,
        credit = p.credit(),
        ev = p.license_evidence,
    );
    match original {
        Some(src) if src != target => {
            let text = fs::read(src)?;
            fs::write(&target, text)?;
        }
        Some(_) => {}
        None => fs::write(&target, &notice)?,
    }
    Ok(())
}

fn write_source(p: &BundledPack, dir: &Path, sha: &str) -> Result<()> {
    let src = match &p.github {
        Some(g) => format!("https://github.com/{}/tree/{}", g.repo, g.rev),
        None => p.download_url.clone(),
    };
    fs::write(
        dir.join("SOURCE.txt"),
        format!(
            "{}\nAuthor: {}\nHomepage: {}\nDownloaded from: {}\nsha256: {}\nLicense: {} ({})\nAttribution: {}\n\n{}\n",
            p.name,
            p.author,
            p.homepage,
            src,
            sha,
            p.license,
            p.license_url,
            p.credit(),
            p.license_evidence
        ),
    )?;
    Ok(())
}

// -- GitHub trees -----------------------------------------------------------

struct TreeFile {
    path: String,
    sha: String,
    size: u64,
}

fn github_listing(agent: &ureq::Agent, g: &GithubSource, p: &BundledPack) -> Result<Vec<TreeFile>> {
    let url = format!("https://api.github.com/repos/{}/git/trees/{}?recursive=1", g.repo, g.rev);
    let mut body = String::new();
    get(agent, &url)?.into_body().into_reader().read_to_string(&mut body)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    if v["truncated"].as_bool() == Some(true) {
        bail!("{}: GitHub tree listing truncated", p.id);
    }
    let mut files: Vec<TreeFile> = v["tree"]
        .as_array()
        .ok_or_else(|| anyhow!("{}: bad tree listing", p.id))?
        .iter()
        .filter(|e| e["type"] == "blob")
        .filter_map(|e| {
            Some(TreeFile {
                path: e["path"].as_str()?.to_string(),
                sha: e["sha"].as_str()?.to_string(),
                size: e["size"].as_u64().unwrap_or(0),
            })
        })
        .filter(|f| {
            g.paths.iter().any(|sel| if sel.ends_with('/') { f.path.starts_with(sel.as_str()) } else { f.path == *sel })
        })
        .filter(|f| !is_junk(&f.path) && !p.exclude.iter().any(|x| f.path.contains(x.as_str())))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.is_empty() {
        bail!("{}: no files matched in {}@{}", p.id, g.repo, g.rev);
    }
    Ok(files)
}

fn listing_hash(files: &[TreeFile]) -> String {
    let mut sha = sha2::Sha256::new();
    for f in files {
        sha.update(format!("{}\t{}\n", f.path, f.sha).as_bytes());
    }
    hex(&sha.finalize())
}

fn github_target(p: &BundledPack, path: &str) -> String {
    path.strip_prefix(p.strip_prefix.as_str()).unwrap_or(path).to_string()
}

fn fetch_github(paths: &Paths, agent: &ureq::Agent, p: &BundledPack) -> Result<String> {
    let g = p.github.as_ref().ok_or_else(|| anyhow!("{}: no github source", p.id))?;
    let files = github_listing(agent, g, p)?;
    let sha = listing_hash(&files);
    if !p.sha256.is_empty() && p.sha256 != sha {
        bail!("{}: file list hash mismatch (manifest {}, got {sha}); the pinned revision changed?", p.id, p.sha256);
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    println!("    {} files, {:.1} MB at {}@{}", files.len(), total as f64 / 1e6, g.repo, &g.rev[..10]);
    let dest = paths.packs.join(p.dir_name());
    fs::create_dir_all(&dest)?;
    let mut done = 0u64;
    let mut fetched = 0usize;
    let mut tsv = String::new();
    for (i, f) in files.iter().enumerate() {
        let rel = github_target(p, &f.path);
        let target = dest.join(&rel);
        // Resume: keep files already present with the right content.
        let ok = target.is_file() && git_blob_sha(&target).ok().as_deref() == Some(f.sha.as_str());
        if !ok {
            let url = format!("https://raw.githubusercontent.com/{}/{}/{}", g.repo, g.rev, url_encode_path(&f.path));
            let part = target.with_extension("dl");
            if let Some(parent) = part.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut attempt = 0;
            loop {
                attempt += 1;
                let res = (|| -> Result<()> {
                    let mut r = get(agent, &url)?.into_body().into_reader();
                    let mut w = BufWriter::new(File::create(&part)?);
                    std::io::copy(&mut r, &mut w)?;
                    w.flush()?;
                    Ok(())
                })();
                match res {
                    Ok(()) => break,
                    Err(e) if attempt < 4 => {
                        eprintln!("\n    retry {attempt} for {}: {e}", f.path);
                        std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
                    }
                    Err(e) => return Err(e),
                }
            }
            let got = git_blob_sha(&part)?;
            if got != f.sha {
                fs::remove_file(&part).ok();
                bail!("{}: blob hash mismatch for {}", p.id, f.path);
            }
            fs::rename(&part, &target)?;
            fetched += 1;
        }
        done += f.size;
        tsv.push_str(&format!("{rel}\t{}\n", f.sha));
        if i % 25 == 0 || i + 1 == files.len() {
            eprint!("\r    {}/{} files, {:.1} MB", i + 1, files.len(), done as f64 / 1e6);
        }
    }
    eprintln!();
    println!("    downloaded {fetched}, already present {}", files.len() - fetched);
    fs::write(dest.join(FILES_LIST), tsv)?;
    let license = if p.license_file.is_empty() {
        None
    } else {
        Some(dest.join(github_target(p, &p.license_file))).filter(|l| l.is_file())
    };
    write_license(p, &dest, license.as_deref())?;
    Ok(sha)
}

fn count_audio(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && aitrack_library::scan::is_audio(e.path()))
        .count() as u64
}

/// Replaces (or inserts after `sha256`) `key = value` inside the `[[pack]]` block with `id`.
fn set_pack_field(text: &str, id: &str, key: &str, value: &str) -> Result<String> {
    let id_line = format!("id = \"{id}\"");
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == id_line)
        .ok_or_else(|| anyhow!("pack {id} not found in manifest"))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("[[") || l.starts_with("[pack."))
        .map_or(lines.len(), |p| start + 1 + p);
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let prefix = format!("{key} = ");
    if let Some(i) = (start..end).find(|&i| lines[i].starts_with(&prefix)) {
        out[i] = format!("{key} = {value}");
    } else {
        let after = (start..end).find(|&i| lines[i].starts_with("sha256 = ")).unwrap_or(start);
        out.insert(after + 1, format!("{key} = {value}"));
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

fn verify(paths: &Paths, m: &Manifest) -> Result<()> {
    let mut problems = 0;
    let agent = agent();
    for p in &m.packs {
        let dir = paths.packs.join(p.dir_name());
        if !dir.is_dir() {
            println!("{:<30} not fetched", p.id);
            continue;
        }
        let mut notes = Vec::new();
        if !dir.join("LICENSE").is_file() {
            notes.push("missing LICENSE".to_string());
        }
        if p.sha256.is_empty() {
            notes.push("manifest has no sha256".into());
        }
        match p.format {
            ArchiveFormat::GithubTree => {
                let list = fs::read_to_string(dir.join(FILES_LIST)).unwrap_or_default();
                let mut bad = 0;
                for line in list.lines() {
                    let Some((rel, sha)) = line.split_once('\t') else { continue };
                    if git_blob_sha(&dir.join(rel)).ok().as_deref() != Some(sha) {
                        bad += 1;
                    }
                }
                if list.is_empty() {
                    notes.push("no file list".into());
                }
                if bad > 0 {
                    notes.push(format!("{bad} files changed or missing"));
                }
                if let Some(g) = &p.github
                    && !p.sha256.is_empty()
                {
                    match github_listing(&agent, g, p) {
                        Ok(files) if listing_hash(&files) != p.sha256 => notes.push("upstream file list changed".into()),
                        Ok(_) => {}
                        Err(e) => notes.push(format!("could not list upstream: {e}")),
                    }
                }
            }
            _ => {
                let ext = match p.format {
                    ArchiveFormat::Zip => "zip",
                    ArchiveFormat::TarGz => "tar.gz",
                    ArchiveFormat::TarXz => "tar.xz",
                    _ => "7z",
                };
                let archive = paths.cache.join(format!("{}.{ext}", p.id));
                if archive.is_file() {
                    let have = sha256_file(&archive)?;
                    if !p.sha256.is_empty() && have != p.sha256 {
                        notes.push("cached archive sha256 mismatch".into());
                    }
                } else {
                    notes.push("archive not cached (sha256 not re-checked)".into());
                }
            }
        }
        let count = count_audio(&dir);
        if p.sample_count > 0 && count != p.sample_count {
            notes.push(format!("{count} audio files, manifest says {}", p.sample_count));
        }
        let fatal = notes.iter().any(|n| !n.contains("not cached"));
        if fatal {
            problems += 1;
        }
        println!("{:<30} {:>6} files  {}", p.id, count, if notes.is_empty() { "ok".into() } else { notes.join("; ") });
    }
    if problems > 0 {
        bail!("{problems} pack(s) failed verification");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

fn build_index(paths: &Paths, m: &Manifest) -> Result<()> {
    for ext in ["", "-wal", "-shm"] {
        let f = PathBuf::from(format!("{}{ext}", paths.db.display()));
        if f.exists() {
            fs::remove_file(f)?;
        }
    }
    let lib = Library::open(&paths.db)?;
    // Relative root: resolved against the database folder, so the factory
    // library can be shipped anywhere next to its packs.
    lib.add_root("packs", RootKind::Factory)?;
    for p in &m.packs {
        if paths.packs.join(p.dir_name()).is_dir() {
            lib.upsert_pack(&PackInfo::from(p))?;
        }
    }
    let start = Instant::now();
    let stats = lib.scan(
        &BasicAnalyzer,
        &ScanOptions::default(),
        &mut |pr| {
            if pr.done % 250 == 0 && pr.total > 0 {
                eprint!("\r  indexing {}/{}", pr.done, pr.total);
            }
        },
        &AtomicBool::new(false),
    )?;
    eprintln!();
    println!(
        "factory.db: {} samples ({} errors), {} presets in {:.1} s",
        stats.added, stats.errors, stats.presets, start.elapsed().as_secs_f64()
    );
    lib.connection().execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    print_stats(&lib)
}

fn print_stats(lib: &Library) -> Result<()> {
    let c = lib.connection();
    let group = |sql: &str| -> Result<Vec<(String, i64)>> {
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<aitrack_library::rusqlite::Result<_>>()?)
    };
    println!("By category:");
    for (k, n) in group(
        "SELECT COALESCE(category, '(none)') || COALESCE('/' || subcategory, ''), COUNT(*) FROM samples
         WHERE status='ok' GROUP BY 1 ORDER BY 2 DESC",
    )? {
        println!("  {k:<28} {n}");
    }
    println!("By kind:");
    for (k, n) in group("SELECT kind, COUNT(*) FROM samples WHERE status='ok' GROUP BY 1 ORDER BY 2 DESC")? {
        println!("  {k:<28} {n}");
    }
    println!("By key (Camelot / root):");
    for (k, n) in group(
        "SELECT CASE WHEN camelot IS NOT NULL THEN camelot ELSE
           CASE WHEN key_root IS NULL THEN '(atonal/unknown)' ELSE 'root ' || key_root END END,
           COUNT(*) FROM samples WHERE status='ok' GROUP BY 1 ORDER BY 2 DESC",
    )? {
        println!("  {k:<28} {n}");
    }
    println!("By pack:");
    for (k, n) in group("SELECT COALESCE(pack, '?'), COUNT(*) FROM samples WHERE status='ok' GROUP BY 1 ORDER BY 2 DESC")? {
        println!("  {k:<28} {n}");
    }
    let errors: BTreeMap<String, i64> =
        group("SELECT COALESCE(error, '?'), COUNT(*) FROM samples WHERE status='error' GROUP BY 1")?.into_iter().collect();
    for (e, n) in errors {
        println!("  error x{n}: {e}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// attribution
// ---------------------------------------------------------------------------

fn write_attribution(paths: &Paths, m: &Manifest) -> Result<()> {
    let path = paths.bank.join("ATTRIBUTION.md");
    fs::write(&path, m.attribution_markdown())?;
    println!("wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_field_update() {
        let text = "[[pack]]\nid = \"a\"\nsha256 = \"\"\nname = \"A\"\n\n[pack.github]\nrepo = \"x\"\n\n[[pack]]\nid = \"b\"\nsha256 = \"\"\n";
        let t = set_pack_field(text, "a", "sha256", "\"abc\"").unwrap();
        let t = set_pack_field(&t, "a", "sample_count", "12").unwrap();
        assert!(t.contains("id = \"a\"\nsha256 = \"abc\"\nsample_count = 12\nname"));
        assert!(t.contains("id = \"b\"\nsha256 = \"\"\n"));
        assert!(set_pack_field(text, "zzz", "sha256", "1").is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(url_encode_path("Strings/Violin Section/A#4.wav"), "Strings/Violin%20Section/A%234.wav");
        assert!(is_junk("__MACOSX/a.wav") && is_junk("x/._a.wav") && !is_junk("x/a.wav"));
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("hello.txt");
        fs::write(&f, b"hello\n").unwrap();
        // `git hash-object` of "hello\n".
        assert_eq!(git_blob_sha(&f).unwrap(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn zip_extract_and_normalize() {
        let dir = tempfile::tempdir().unwrap();
        let zpath = dir.path().join("p.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zpath).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            for name in ["Pack/Kicks/k.wav", "Pack/LICENSE.txt", "__MACOSX/Pack/._k.wav", "Pack/skip_me.wav"] {
                w.start_file(name, o).unwrap();
                w.write_all(b"data").unwrap();
            }
            w.finish().unwrap();
        }
        let raw = dir.path().join("raw");
        extract_zip(&zpath, &raw).unwrap();
        let pack = BundledPack {
            id: "p".into(),
            name: "P".into(),
            license: "CC0-1.0".into(),
            exclude: vec!["skip_me".into()],
            ..Default::default()
        };
        let dest = dir.path().join("out");
        normalize(&raw, &dest, &pack).unwrap();
        assert!(dest.join("Kicks/k.wav").is_file());
        assert!(!dest.join("skip_me.wav").exists());
        assert_eq!(fs::read(dest.join("LICENSE")).unwrap(), b"data");
    }
}
