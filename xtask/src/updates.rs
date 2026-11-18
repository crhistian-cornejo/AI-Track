//! The auto-update side of a release (the contract is `docs/updater.md`): minisign
//! signatures of the updater artifacts, the key pair generator, `latest.json` and
//! `SHA256SUMS.txt`.

use anyhow::{Context, Result, bail};
use minisign::{KeyPair, SecretKey, SecretKeyBox};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Signs files with the updater's minisign secret key (`UPDATER_PRIVATE_KEY`, the contents
/// of the key file; `UPDATER_KEY_PASSWORD` unlocks it and is optional: an empty or missing
/// password means an unencrypted key). Absent key = no signatures.
pub struct Signer {
    key: Option<SecretKey>,
}

impl Signer {
    pub fn from_env() -> Result<Signer> {
        let raw = std::env::var("UPDATER_PRIVATE_KEY").unwrap_or_default();
        if raw.trim().is_empty() {
            eprintln!("warning: UPDATER_PRIVATE_KEY is not set; the updater artifacts are NOT signed and no latest.json can be published");
            return Ok(Signer { key: None });
        }
        let password = std::env::var("UPDATER_KEY_PASSWORD").unwrap_or_default();
        Ok(Signer { key: Some(Self::unlock(&raw, &password)?) })
    }

    pub fn unlock(secret_key_text: &str, password: &str) -> Result<SecretKey> {
        // Secrets pasted into CI settings often come back with CRLF or stray blanks.
        let text = secret_key_text.replace("\r\n", "\n");
        let text = format!("{}\n", text.trim());
        let sk_box = SecretKeyBox::from_string(&text).map_err(|e| anyhow::anyhow!("UPDATER_PRIVATE_KEY is not a minisign secret key: {e}"))?;
        // No password = an unencrypted key (`minisign -G -W`); never prompt on a terminal.
        if password.is_empty() {
            return sk_box.into_unencrypted_secret_key().map_err(|e| anyhow::anyhow!("UPDATER_PRIVATE_KEY is password-protected: set UPDATER_KEY_PASSWORD ({e})"));
        }
        sk_box.into_secret_key(Some(password.to_string())).map_err(|e| anyhow::anyhow!("could not unlock the updater key (wrong UPDATER_KEY_PASSWORD?): {e}"))
    }

    /// Writes `<file>.minisig` and returns its full text; `None` when there is no key.
    pub fn sign_file(&self, file: &Path) -> Result<Option<String>> {
        let Some(key) = &self.key else { return Ok(None) };
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("artifact");
        let data = std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
        let trusted = format!("timestamp:{} file:{name}", unix_now());
        let sig = minisign::sign(None, key, std::io::BufReader::new(data), Some(&trusted), Some("signature from the AI-Track updater key"))
            .map_err(|e| anyhow::anyhow!("signing {name}: {e}"))?;
        let text = sig.into_string();
        let mut sig_path = file.as_os_str().to_owned();
        sig_path.push(".minisig");
        std::fs::write(&sig_path, &text)?;
        println!("signed {name} -> {}", Path::new(&sig_path).file_name().unwrap().to_string_lossy());
        Ok(Some(text))
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `2026-11-20T12:00:00Z` for a unix timestamp.
pub fn rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let size = std::io::copy(&mut f, &mut hasher)?;
    Ok((hasher.finalize().iter().map(|b| format!("{b:02x}")).collect(), size))
}

/// What one platform's CI job leaves next to its artifacts (`platform-<key>.json`).
pub struct Fragment {
    pub key: String,
    pub version: String,
    pub file: String,
    pub signature: Option<String>,
    pub sha256: String,
    pub size: u64,
}

impl Fragment {
    pub fn to_json(&self) -> Value {
        json!({ "key": self.key, "version": self.version, "file": self.file, "signature": self.signature, "sha256": self.sha256, "size": self.size })
    }

    pub fn from_json(v: &Value) -> Result<Fragment> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).with_context(|| format!("fragment is missing \"{k}\""));
        Ok(Fragment {
            key: text("key")?,
            version: text("version")?,
            file: text("file")?,
            signature: v.get("signature").and_then(Value::as_str).map(str::to_string),
            sha256: text("sha256")?,
            size: v.get("size").and_then(Value::as_u64).context("fragment is missing \"size\"")?,
        })
    }
}

/// `latest.json` exactly as `docs/updater.md` describes it. `None` when a fragment has no
/// signature: the app would reject the release, so it is better not to publish a manifest.
pub fn build_manifest(fragments: &[Fragment], version: &str, base_url: &str, notes: &str, pub_date: &str) -> Result<Option<Value>> {
    if fragments.is_empty() {
        bail!("no platform fragments (platform-*.json) found");
    }
    let base = base_url.trim_end_matches('/');
    let mut platforms = Map::new();
    for f in fragments {
        if f.version != version {
            bail!("{} was built for {} but the release is {version}", f.file, f.version);
        }
        let Some(sig) = &f.signature else {
            eprintln!("warning: {} has no minisign signature; latest.json is not written", f.file);
            return Ok(None);
        };
        platforms.insert(f.key.clone(), json!({ "url": format!("{base}/{}", f.file), "signature": sig, "sha256": f.sha256, "size": f.size }));
    }
    Ok(Some(json!({ "version": version, "notes": notes, "pub_date": pub_date, "platforms": platforms })))
}

/// `SHA256SUMS.txt` (`<hex>  <name>` per line, sorted) for every release asset in `dir`.
pub fn write_checksums(dir: &Path) -> Result<PathBuf> {
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if !path.is_file() || name == "SHA256SUMS.txt" || (name.starts_with("platform-") && name.ends_with(".json")) || name.starts_with('.') {
            continue;
        }
        lines.push(format!("{}  {name}", sha256_file(&path)?.0));
    }
    lines.sort_by(|a, b| a[66..].cmp(&b[66..]));
    let out = dir.join("SHA256SUMS.txt");
    std::fs::write(&out, lines.join("\n") + "\n")?;
    println!("wrote {} ({} files)", out.display(), lines.len());
    Ok(out)
}

/// `cargo xtask package manifest --version X --base-url URL [--dir D] [--notes-file F]`
pub fn manifest_command(args: &[String], default_dir: &Path) -> Result<()> {
    let (mut version, mut base_url, mut dir, mut notes_file) = (None, None, default_dir.to_path_buf(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => version = it.next().cloned(),
            "--base-url" => base_url = it.next().cloned(),
            "--dir" => dir = it.next().map(PathBuf::from).context("--dir needs a value")?,
            "--notes-file" => notes_file = it.next().map(PathBuf::from),
            other => bail!("unknown option {other}"),
        }
    }
    let version = version.context("--version is required")?;
    let version = version.trim_start_matches('v').to_string();
    let base_url = base_url.context("--base-url is required (e.g. https://github.com/OWNER/REPO/releases/download/vX.Y.Z)")?;
    let notes = match notes_file {
        Some(f) => std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?,
        None => String::new(),
    };
    let mut fragments = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with("platform-") && name.ends_with(".json") {
            let v: Value = serde_json::from_slice(&std::fs::read(&path)?).with_context(|| format!("parsing {name}"))?;
            fragments.push(Fragment::from_json(&v)?);
        }
    }
    fragments.sort_by(|a, b| a.key.cmp(&b.key));
    let manifest = build_manifest(&fragments, &version, &base_url, notes.trim(), &rfc3339_utc(unix_now()))?;
    let latest = dir.join("latest.json");
    match manifest {
        Some(m) => {
            std::fs::write(&latest, serde_json::to_string_pretty(&m)? + "\n")?;
            println!("wrote {}", latest.display());
        }
        None => {
            let _ = std::fs::remove_file(&latest);
            println!("no latest.json: this release will not auto-update (unsigned updater artifacts)");
        }
    }
    write_checksums(&dir)?;
    Ok(())
}

/// `cargo xtask updater-keygen --out <dir> [--force]`
pub fn keygen_command(root: &Path, args: &[String]) -> Result<()> {
    let (mut out, mut force) = (None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--force" => force = true,
            other => bail!("unknown option {other}"),
        }
    }
    let out = out.context("--out <dir> is required: a folder OUTSIDE the repository for the secret key")?;
    std::fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    let root = root.canonicalize()?;
    if out.starts_with(&root) {
        bail!("{} is inside the repository; keep the secret key outside it", out.display());
    }
    let pub_path = root.join("release/updater.pub");
    let sec_path = out.join("aitrack-updater.key");
    if (pub_path.exists() || sec_path.exists()) && !force {
        bail!("{} or {} already exists; pass --force only if you really want to replace the updater key (installed apps would stop updating)", pub_path.display(), sec_path.display());
    }
    let (password, generated) = match std::env::var("UPDATER_KEY_PASSWORD") {
        Ok(p) if !p.is_empty() => (p, false),
        _ => (random_password()?, true),
    };
    let (pk_text, sk_text) = generate_keypair(&password)?;
    std::fs::write(&sec_path, sk_text)?;
    restrict(&sec_path);
    std::fs::write(&pub_path, pk_text)?;
    println!("secret key  {} (back it up somewhere safe; it is NOT in the repository)", sec_path.display());
    println!("public key  {} (commit this file; the app embeds it)", pub_path.display());
    if generated {
        println!("password    {password}\n            (generated; shown only now. Save it in your password manager)");
    }
    println!(
        "\nStore the secrets in GitHub:\n  gh secret set UPDATER_PRIVATE_KEY < {}\n  gh secret set UPDATER_KEY_PASSWORD   # paste the password\n\nIf this key is lost, installed copies of AI-Track can no longer update themselves.",
        sec_path.display()
    );
    Ok(())
}

/// (public key box text, secret key box text) of a fresh password-protected pair.
pub fn generate_keypair(password: &str) -> Result<(String, String)> {
    let kp = KeyPair::generate_encrypted_keypair(Some(password.to_string())).map_err(|e| anyhow::anyhow!("generating the key pair: {e}"))?;
    let pk = kp.pk.to_box().map_err(|e| anyhow::anyhow!("{e}"))?.into_string();
    let sk = kp.sk.to_box(Some("AI-Track updater secret key")).map_err(|e| anyhow::anyhow!("{e}"))?.into_string();
    Ok((pk, sk))
}

fn random_password() -> Result<String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect())
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_rfc3339() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_763_380_800), "2025-11-17T12:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    fn fragment(key: &str, file: &str, sig: Option<&str>) -> Fragment {
        Fragment { key: key.into(), version: "0.0.2".into(), file: file.into(), signature: sig.map(str::to_string), sha256: "ab".repeat(32), size: 42 }
    }

    #[test]
    fn manifest_follows_the_updater_contract() {
        let frags = [
            fragment("macos-universal", "AI-Track-0.0.2-macos-universal.app.tar.gz", Some("SIG-MAC\n")),
            fragment("windows-x86_64", "AI-Track-0.0.2-windows-x86_64-setup.exe", Some("SIG-WIN\n")),
        ];
        let m = build_manifest(&frags, "0.0.2", "https://example.test/releases/download/v0.0.2/", "notes", "2026-11-20T12:00:00Z").unwrap().unwrap();
        assert_eq!(m["version"], "0.0.2");
        assert_eq!(m["notes"], "notes");
        assert_eq!(m["pub_date"], "2026-11-20T12:00:00Z");
        let mac = &m["platforms"]["macos-universal"];
        assert_eq!(mac["url"], "https://example.test/releases/download/v0.0.2/AI-Track-0.0.2-macos-universal.app.tar.gz");
        assert_eq!(mac["signature"], "SIG-MAC\n");
        assert_eq!(mac["size"], 42);
        assert_eq!(m["platforms"]["windows-x86_64"]["sha256"], "ab".repeat(32));
    }

    #[test]
    fn an_unsigned_fragment_means_no_manifest() {
        let frags = [fragment("macos-universal", "a.tar.gz", None)];
        assert!(build_manifest(&frags, "0.0.2", "https://x", "", "now").unwrap().is_none());
    }

    #[test]
    fn a_version_mismatch_is_an_error() {
        let frags = [fragment("macos-universal", "a.tar.gz", Some("s"))];
        assert!(build_manifest(&frags, "0.0.3", "https://x", "", "now").is_err());
    }

    #[test]
    fn fragments_round_trip() {
        let f = fragment("windows-x86_64", "x.exe", None);
        let back = Fragment::from_json(&f.to_json()).unwrap();
        assert_eq!((back.key.as_str(), back.size, back.signature), ("windows-x86_64", 42, None));
    }

    #[test]
    fn a_generated_key_signs_and_the_signature_verifies() {
        let (pk_text, sk_text) = generate_keypair("hunter2").unwrap();
        assert!(Signer::unlock(&sk_text, "wrong").is_err());
        let sk = Signer::unlock(&sk_text.replace('\n', "\r\n"), "hunter2").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("payload.bin");
        std::fs::write(&file, b"payload").unwrap();
        let signer = Signer { key: Some(sk) };
        let sig_text = signer.sign_file(&file).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("payload.bin.minisig")).unwrap(), sig_text);
        let pk = minisign::PublicKeyBox::from_string(&pk_text).unwrap().into_public_key().unwrap();
        let sig = minisign::SignatureBox::from_string(&sig_text).unwrap();
        assert!(minisign::verify(&pk, &sig, std::io::Cursor::new(b"payload"), true, false, false).is_ok());
        assert!(minisign::verify(&pk, &sig, std::io::Cursor::new(b"tampered"), true, false, false).is_err());
    }

    #[test]
    fn an_unencrypted_key_needs_no_password() {
        let kp = KeyPair::generate_unencrypted_keypair().unwrap();
        let text = kp.sk.to_box(None).unwrap().into_string();
        let sk = Signer::unlock(&text, "").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, b"a").unwrap();
        assert!(Signer { key: Some(sk) }.sign_file(&file).unwrap().is_some());
        // A protected key without a password is an error, not a prompt.
        let (_, protected) = generate_keypair("pw").unwrap();
        assert!(Signer::unlock(&protected, "").is_err());
    }

    #[test]
    fn checksums_skip_fragments_and_themselves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.dmg"), b"b").unwrap();
        std::fs::write(dir.path().join("a.zip"), b"a").unwrap();
        std::fs::write(dir.path().join("platform-x.json"), b"{}").unwrap();
        let out = write_checksums(dir.path()).unwrap();
        let text = std::fs::read_to_string(out).unwrap();
        let names: Vec<&str> = text.lines().map(|l| &l[66..]).collect();
        assert_eq!(names, ["a.zip", "b.dmg"]);
        assert!(text.starts_with("ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb  a.zip"));
    }
}
