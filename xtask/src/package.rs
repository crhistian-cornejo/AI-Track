//! `cargo xtask package [--target mac|win] [--universal] [--sign] [--skip-build]`
//! `cargo xtask package manifest --version X --base-url URL [--dir D] [--notes-file F]`
//!
//! Builds the four release binaries and assembles the installable artifacts in
//! `target/dist/` (see `docs/release-0.0.1.md`; the update contract is `docs/updater.md`).
//! Signing and notarization steps run only when their credentials are in the environment;
//! without them the output is still complete (ad-hoc signed on macOS, unsigned on Windows).

use crate::updates::{self, Fragment, Signer};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bundle identifier; also the window's `app_id`.
const BUNDLE_ID: &str = "dev.aitrack.daw";
/// The executables that ship together. The helpers must sit next to `aitrack`.
const BINARIES: [&str; 4] = ["aitrack", "aitrack-plugin-host", "aitrack-plugin-scanner", "aitrack-ml-worker"];
const MAC_TRIPLES: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Mac,
    Win,
}

struct Opts {
    target: Target,
    universal: bool,
    sign: bool,
    skip_build: bool,
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let dist = target_dir(root).join("dist");
    if args.first().map(String::as_str) == Some("manifest") {
        return updates::manifest_command(&args[1..], &dist);
    }
    if args.first().map(String::as_str) == Some("checksums") {
        updates::write_checksums(&dist)?;
        return Ok(());
    }
    let opts = parse_opts(args)?;
    std::fs::create_dir_all(&dist)?;
    let signer = Signer::from_env()?;
    match opts.target {
        Target::Mac => package_mac(root, &dist, &opts, &signer)?,
        Target::Win => package_win(root, &dist, &opts, &signer)?,
    }
    updates::write_checksums(&dist)?;
    Ok(())
}

fn parse_opts(args: &[String]) -> Result<Opts> {
    let mut target = if cfg!(windows) { Target::Win } else { Target::Mac };
    let (mut universal, mut sign, mut skip_build) = (false, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => {
                target = match it.next().map(String::as_str) {
                    Some("mac" | "macos") => Target::Mac,
                    Some("win" | "windows") => Target::Win,
                    other => bail!("--target takes mac or win, not {other:?}"),
                }
            }
            "--universal" => universal = true,
            "--sign" => sign = true,
            "--skip-build" => skip_build = true,
            other => bail!("unknown option {other}\nusage: cargo xtask package [--target mac|win] [--universal] [--sign] [--skip-build]\n       cargo xtask package manifest --version X --base-url URL [--dir D] [--notes-file F]"),
        }
    }
    if target == Target::Mac && !cfg!(target_os = "macos") {
        bail!("the macOS package must be built on a Mac (codesign, lipo and hdiutil are needed)");
    }
    if target == Target::Win && !cfg!(windows) {
        bail!("the Windows package must be built on Windows (MSVC toolchain and NSIS)");
    }
    if universal && target == Target::Win {
        bail!("--universal only applies to the macOS package");
    }
    Ok(Opts { target, universal, sign, skip_build })
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"))
}

fn run_cmd(cmd: &mut Command) -> Result<()> {
    let shown = format!("{:?}", cmd).replace('"', "");
    let status = cmd.status().with_context(|| format!("could not start: {shown}"))?;
    if !status.success() {
        bail!("command failed ({status}): {shown}");
    }
    Ok(())
}

fn capture(cmd: &mut Command) -> Result<String> {
    let shown = format!("{:?}", cmd).replace('"', "");
    let out = cmd.output().with_context(|| format!("could not start: {shown}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("command failed ({}): {shown}\n{text}", out.status);
    }
    Ok(text)
}

fn env_set(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// `cargo build --release` of the four binaries; `triple` None = the host.
fn build(root: &Path, triple: Option<&str>) -> Result<()> {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(root).args(["build", "--release", "-p", "aitrack-app", "-p", "aitrack-host", "-p", "aitrack-ai"]);
    for bin in BINARIES {
        cmd.args(["--bin", bin]);
    }
    if let Some(t) = triple {
        cmd.args(["--target", t]);
    }
    if cfg!(target_os = "macos") {
        cmd.env("MACOSX_DEPLOYMENT_TARGET", MAC_MIN_OS);
    }
    println!("==> cargo build --release ({})", triple.unwrap_or("host"));
    run_cmd(&mut cmd)
}

fn release_dir(root: &Path, triple: Option<&str>) -> PathBuf {
    match triple {
        Some(t) => target_dir(root).join(t).join("release"),
        None => target_dir(root).join("release"),
    }
}

fn human(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

fn artifact_name(platform: &str, ext: &str) -> String {
    format!("AI-Track-{VERSION}-{platform}.{ext}")
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else if path.exists() || path.is_symlink() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// The data the installers carry next to the executables: both manifests, the factory
/// index and the sound bank's licence notes. Never the packs themselves.
fn copy_content(root: &Path, dest: &Path) -> Result<()> {
    let files = [
        "plugins/manifest.toml",
        "soundbank/manifest.toml",
        "soundbank/factory.db",
        "soundbank/ATTRIBUTION.md",
        "soundbank/README.md",
    ];
    for rel in files {
        let from = root.join("content").join(rel);
        if !from.is_file() {
            eprintln!("warning: content/{rel} not found; the package will not include it");
            continue;
        }
        let to = dest.join(rel);
        std::fs::create_dir_all(to.parent().unwrap())?;
        std::fs::copy(&from, &to)?;
    }
    Ok(())
}

fn write_fragment(dist: &Path, key: &str, file: &Path, signature: Option<String>) -> Result<()> {
    let (sha256, size) = updates::sha256_file(file)?;
    let frag = Fragment { key: key.to_string(), version: VERSION.to_string(), file: file.file_name().unwrap().to_string_lossy().into_owned(), signature, sha256, size };
    std::fs::write(dist.join(format!("platform-{key}.json")), serde_json::to_string_pretty(&frag.to_json())? + "\n")?;
    Ok(())
}

// ---------------------------------------------------------------- macOS

const MAC_MIN_OS: &str = "13.0";

fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>aitrack</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>AI-Track</string>
	<key>CFBundleDisplayName</key>
	<string>AI-Track</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.music</string>
	<key>LSMinimumSystemVersion</key>
	<string>{MAC_MIN_OS}</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>MIT License</string>
	<key>NSMicrophoneUsageDescription</key>
	<string>AI-Track needs the microphone and audio inputs to record audio into your project.</string>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
</dict>
</plist>
"#
    )
}

struct MacSigner {
    /// `-` = ad-hoc.
    identity: String,
    real: bool,
}

impl MacSigner {
    fn new(sign: bool) -> MacSigner {
        match (sign, env_set("APPLE_SIGNING_IDENTITY")) {
            (true, Some(identity)) => MacSigner { identity, real: true },
            (true, None) => {
                eprintln!("warning: --sign without APPLE_SIGNING_IDENTITY; signing ad hoc (Gatekeeper will warn)");
                MacSigner { identity: "-".into(), real: false }
            }
            (false, _) => MacSigner { identity: "-".into(), real: false },
        }
    }

    fn codesign(&self, path: &Path, entitlements: Option<&Path>, identifier: Option<&str>) -> Result<()> {
        let mut cmd = Command::new("codesign");
        cmd.args(["--force", "--sign", &self.identity, "--options", "runtime"]);
        cmd.arg(if self.real { "--timestamp" } else { "--timestamp=none" });
        if let Some(e) = entitlements {
            cmd.arg("--entitlements").arg(e);
        }
        if let Some(i) = identifier {
            cmd.args(["--identifier", i]);
        }
        cmd.arg(path);
        run_cmd(&mut cmd)
    }

    fn can_notarize(&self) -> bool {
        self.real && env_set("APPLE_ID").is_some() && env_set("APPLE_TEAM_ID").is_some() && env_set("APPLE_APP_PASSWORD").is_some()
    }
}

fn notarize(path: &Path) -> Result<()> {
    println!("==> notarizing {} (this waits for Apple)", path.file_name().unwrap().to_string_lossy());
    let out = capture(
        Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(path)
            .args(["--apple-id", &env_set("APPLE_ID").unwrap(), "--team-id", &env_set("APPLE_TEAM_ID").unwrap(), "--password", &env_set("APPLE_APP_PASSWORD").unwrap(), "--wait", "--output-format", "json"]),
    )?;
    let json: serde_json::Value = out.lines().rev().find_map(|l| serde_json::from_str(l).ok()).or_else(|| serde_json::from_str(&out).ok()).unwrap_or_default();
    let status = json["status"].as_str().unwrap_or("unknown");
    if status != "Accepted" {
        if let Some(id) = json["id"].as_str() {
            let log = capture(Command::new("xcrun").args(["notarytool", "log", id, "--apple-id", &env_set("APPLE_ID").unwrap(), "--team-id", &env_set("APPLE_TEAM_ID").unwrap(), "--password", &env_set("APPLE_APP_PASSWORD").unwrap()]));
            eprintln!("{}", log.unwrap_or_default());
        }
        bail!("notarization finished with status {status}");
    }
    Ok(())
}

fn staple(path: &Path) -> Result<()> {
    run_cmd(Command::new("xcrun").args(["stapler", "staple"]).arg(path))
}

fn package_mac(root: &Path, dist: &Path, opts: &Opts, signer: &Signer) -> Result<()> {
    let triples: Vec<Option<&str>> = if opts.universal { MAC_TRIPLES.iter().map(|t| Some(*t)).collect() } else { vec![None] };
    if !opts.skip_build {
        if opts.universal {
            let installed = capture(Command::new("rustup").args(["target", "list", "--installed"])).unwrap_or_default();
            for t in MAC_TRIPLES {
                if !installed.lines().any(|l| l.trim() == t) {
                    bail!("the Rust target {t} is missing: rustup target add {t}");
                }
            }
        }
        for t in &triples {
            build(root, *t)?;
        }
    }
    let arch = if opts.universal {
        "universal".to_string()
    } else {
        match std::env::consts::ARCH {
            "aarch64" => "aarch64".to_string(),
            other => other.to_string(),
        }
    };
    let platform = format!("macos-{arch}");

    let stage = dist.join("stage-mac");
    remove_if_exists(&stage)?;
    let app = stage.join("AI-Track.app");
    let macos_dir = app.join("Contents/MacOS");
    let res_dir = app.join("Contents/Resources");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(&res_dir)?;

    println!("==> assembling AI-Track.app");
    for bin in BINARIES {
        let dest = macos_dir.join(bin);
        if opts.universal {
            let mut cmd = Command::new("lipo");
            cmd.arg("-create");
            for t in &triples {
                cmd.arg(release_dir(root, *t).join(bin));
            }
            run_cmd(cmd.arg("-output").arg(&dest))?;
        } else {
            let from = release_dir(root, None).join(bin);
            std::fs::copy(&from, &dest).with_context(|| format!("{} (build first, or drop --skip-build)", from.display()))?;
        }
        // Local symbols are not needed at run time; the dSYM archive below has the rest.
        let _ = Command::new("strip").arg("-x").arg(&dest).status();
    }
    std::fs::write(app.join("Contents/Info.plist"), info_plist(VERSION))?;
    std::fs::write(app.join("Contents/PkgInfo"), "APPL????")?;
    std::fs::copy(root.join("release/icons/AppIcon.icns"), res_dir.join("AppIcon.icns")).context("release/icons/AppIcon.icns (cargo xtask icons)")?;
    copy_content(root, &res_dir.join("content"))?;
    for name in ["LICENSE", "NOTICE"] {
        std::fs::copy(root.join(name), res_dir.join(name))?;
    }

    // Symbols for crash reports: one archive with the dSYM of every architecture.
    let symbols = dist.join(artifact_name(&format!("{platform}-symbols"), "tar.gz"));
    archive_dsyms(root, &triples, &symbols)?;

    // Signing: helpers first (each with its own entitlements), then the bundle.
    let mac = MacSigner::new(opts.sign);
    let ent_app = root.join("release/macos/app.entitlements");
    let ent_host = root.join("release/macos/plugin-host.entitlements");
    println!("==> codesign ({})", if mac.real { mac.identity.as_str() } else { "ad hoc" });
    mac.codesign(&macos_dir.join("aitrack-plugin-host"), Some(&ent_host), Some(&format!("{BUNDLE_ID}.plugin-host")))?;
    mac.codesign(&macos_dir.join("aitrack-plugin-scanner"), Some(&ent_host), Some(&format!("{BUNDLE_ID}.plugin-scanner")))?;
    mac.codesign(&macos_dir.join("aitrack-ml-worker"), Some(&ent_app), Some(&format!("{BUNDLE_ID}.ml-worker")))?;
    mac.codesign(&app, Some(&ent_app), Some(BUNDLE_ID))?;
    run_cmd(Command::new("codesign").args(["--verify", "--deep", "--strict", "--verbose=2"]).arg(&app))?;

    if mac.can_notarize() {
        let zip = stage.join("notarize.zip");
        run_cmd(Command::new("ditto").args(["-c", "-k", "--keepParent"]).arg(&app).arg(&zip))?;
        notarize(&zip)?;
        std::fs::remove_file(&zip)?;
        // The ticket is stapled to the .app itself, not to the zip.
        staple(&app)?;
    } else if opts.sign {
        eprintln!("warning: APPLE_ID / APPLE_TEAM_ID / APPLE_APP_PASSWORD or a Developer ID identity is missing; the app is not notarized");
    }

    // The updater artifact: a tar.gz whose only root entry is AI-Track.app/.
    let tarball = dist.join(artifact_name(&platform, "app.tar.gz"));
    remove_if_exists(&tarball)?;
    println!("==> {}", tarball.file_name().unwrap().to_string_lossy());
    {
        let enc = flate2::write::GzEncoder::new(std::fs::File::create(&tarball)?, flate2::Compression::best());
        let mut tar = tar::Builder::new(enc);
        tar.follow_symlinks(false);
        tar.append_dir_all("AI-Track.app", &app)?;
        tar.into_inner()?.finish()?;
    }

    // The disk image for manual downloads.
    let dmg = dist.join(artifact_name(&platform, "dmg"));
    remove_if_exists(&dmg)?;
    let dmg_stage = dist.join("stage-dmg");
    remove_if_exists(&dmg_stage)?;
    std::fs::create_dir_all(&dmg_stage)?;
    run_cmd(Command::new("ditto").arg(&app).arg(dmg_stage.join("AI-Track.app")))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Applications", dmg_stage.join("Applications"))?;
    println!("==> {}", dmg.file_name().unwrap().to_string_lossy());
    run_cmd(Command::new("hdiutil").args(["create", "-volname", "AI-Track", "-fs", "HFS+", "-format", "UDZO", "-ov", "-srcfolder"]).arg(&dmg_stage).arg(&dmg))?;
    if mac.real {
        run_cmd(Command::new("codesign").args(["--force", "--sign", &mac.identity, "--timestamp"]).arg(&dmg))?;
        if mac.can_notarize() {
            notarize(&dmg)?;
            staple(&dmg)?;
        }
    }
    remove_if_exists(&dmg_stage)?;

    let signature = signer.sign_file(&tarball)?;
    write_fragment(dist, &platform, &tarball, signature)?;

    println!("\nmacOS package ({platform}):");
    for bin in BINARIES {
        println!("  {bin:<24} {}", human(std::fs::metadata(macos_dir.join(bin))?.len()));
    }
    for f in [&tarball, &dmg, &symbols] {
        println!("  {:<62} {}", f.file_name().unwrap().to_string_lossy(), human(std::fs::metadata(f)?.len()));
    }
    println!("  bundle left in {}", app.display());
    Ok(())
}

/// Puts every `<bin>.dSYM` produced by the build into one tar.gz (`<triple or host>/<bin>.dSYM`).
fn archive_dsyms(root: &Path, triples: &[Option<&str>], out: &Path) -> Result<()> {
    let enc = flate2::write::GzEncoder::new(std::fs::File::create(out)?, flate2::Compression::best());
    let mut tar = tar::Builder::new(enc);
    let mut found = 0;
    for t in triples {
        let dir = release_dir(root, *t);
        for bin in BINARIES {
            let dsym = dir.join(format!("{bin}.dSYM"));
            if dsym.is_dir() {
                tar.append_dir_all(Path::new(t.unwrap_or("host")).join(format!("{bin}.dSYM")), &dsym)?;
                found += 1;
            }
        }
    }
    tar.into_inner()?.finish()?;
    if found == 0 {
        eprintln!("warning: no .dSYM found next to the binaries; the symbols archive is empty");
    }
    Ok(())
}

// -------------------------------------------------------------- Windows

fn nsis_version(version: &str) -> String {
    let mut nums: Vec<String> = version.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).take(4).map(str::to_string).collect();
    while nums.len() < 4 {
        nums.push("0".into());
    }
    nums.join(".")
}

fn find_makensis() -> Result<PathBuf> {
    let candidates = [
        PathBuf::from("makensis"),
        PathBuf::from(r"C:\Program Files (x86)\NSIS\makensis.exe"),
        PathBuf::from(r"C:\Program Files\NSIS\makensis.exe"),
    ];
    for c in candidates {
        if Command::new(&c).arg("/VERSION").output().is_ok_and(|o| o.status.success()) {
            return Ok(c);
        }
    }
    bail!("makensis (NSIS) was not found. Install it (choco install nsis, or https://nsis.sourceforge.io) and make sure it is on PATH; GitHub's windows runners already have it")
}

/// Authenticode, when credentials exist: a PFX (`WINDOWS_PFX_FILE` + `WINDOWS_PFX_PASSWORD`) via
/// signtool, or any command in `WINDOWS_SIGN_COMMAND` where `{file}` is the path (Azure Trusted
/// Signing through azuresigntool, for example). Returns whether it signed.
fn authenticode(files: &[&Path]) -> Result<bool> {
    let tr = env_set("WINDOWS_TIMESTAMP_URL").unwrap_or_else(|| "http://timestamp.digicert.com".into());
    if let Some(template) = env_set("WINDOWS_SIGN_COMMAND") {
        for f in files {
            let line = template.replace("{file}", &format!("\"{}\"", f.display()));
            run_cmd(Command::new("cmd").args(["/C", &line]))?;
        }
        return Ok(true);
    }
    if let (Some(pfx), pass) = (env_set("WINDOWS_PFX_FILE"), env_set("WINDOWS_PFX_PASSWORD")) {
        for f in files {
            let mut cmd = Command::new(env_set("SIGNTOOL").unwrap_or_else(|| "signtool".into()));
            cmd.args(["sign", "/fd", "sha256", "/tr", &tr, "/td", "sha256", "/f", &pfx]);
            if let Some(p) = &pass {
                cmd.args(["/p", p]);
            }
            run_cmd(cmd.arg(f))?;
        }
        return Ok(true);
    }
    Ok(false)
}

fn package_win(root: &Path, dist: &Path, opts: &Opts, signer: &Signer) -> Result<()> {
    if !opts.skip_build {
        build(root, None)?;
    }
    let rel = release_dir(root, None);
    let platform = "windows-x86_64";

    let stage = dist.join("stage-win");
    remove_if_exists(&stage)?;
    std::fs::create_dir_all(&stage)?;
    println!("==> staging the Windows files");
    for bin in BINARIES {
        let from = rel.join(format!("{bin}.exe"));
        std::fs::copy(&from, stage.join(format!("{bin}.exe"))).with_context(|| format!("{} (build first, or drop --skip-build)", from.display()))?;
    }
    copy_content(root, &stage.join("content"))?;
    std::fs::copy(root.join("LICENSE"), stage.join("LICENSE.txt"))?;
    std::fs::copy(root.join("NOTICE"), stage.join("NOTICE.txt"))?;

    let exes: Vec<PathBuf> = BINARIES.iter().map(|b| stage.join(format!("{b}.exe"))).collect();
    let exe_refs: Vec<&Path> = exes.iter().map(PathBuf::as_path).collect();
    let signed = if opts.sign { authenticode(&exe_refs)? } else { false };
    if opts.sign && !signed {
        eprintln!("warning: --sign without WINDOWS_PFX_FILE or WINDOWS_SIGN_COMMAND; the executables are not Authenticode-signed (SmartScreen will warn)");
    }

    // Symbols for crash reports.
    let symbols = dist.join(artifact_name(&format!("{platform}-symbols"), "zip"));
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&symbols)?);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for bin in BINARIES {
            let pdb = rel.join(format!("{}.pdb", bin.replace('-', "_")));
            if pdb.is_file() {
                zip.start_file(pdb.file_name().unwrap().to_string_lossy(), opt)?;
                std::io::copy(&mut std::fs::File::open(&pdb)?, &mut zip)?;
            }
        }
        zip.finish()?;
    }

    // Installer.
    let setup = dist.join(artifact_name(platform, "setup.exe"));
    remove_if_exists(&setup)?;
    let makensis = find_makensis()?;
    println!("==> {}", setup.file_name().unwrap().to_string_lossy());
    run_cmd(
        Command::new(makensis)
            .arg("/V2")
            .arg(format!("/DVERSION={VERSION}"))
            .arg(format!("/DNUMVERSION={}", nsis_version(VERSION)))
            .arg(format!("/DSTAGE={}", stage.display()))
            .arg(format!("/DOUTFILE={}", setup.display()))
            .arg(format!("/DICON={}", root.join("release/icons/aitrack.ico").display()))
            .arg(root.join("release/windows/installer.nsi")),
    )?;
    if opts.sign && signed {
        authenticode(&[setup.as_path()])?;
    }

    // Portable zip: the same files, with a launcher that keeps the settings beside them.
    let portable = dist.join(artifact_name(platform, "portable.zip"));
    remove_if_exists(&portable)?;
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&portable)?);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for entry in walkdir::WalkDir::new(&stage).sort_by_file_name() {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel_path = entry.path().strip_prefix(&stage)?.to_string_lossy().replace('\\', "/");
            zip.start_file(format!("AI-Track/{rel_path}"), opt)?;
            std::io::copy(&mut std::fs::File::open(entry.path())?, &mut zip)?;
        }
        zip.start_file("AI-Track/AI-Track (portable).cmd", opt)?;
        std::io::Write::write_all(&mut zip, b"@echo off\r\nset \"AITRACK_CONFIG_DIR=%~dp0data\"\r\nstart \"\" \"%~dp0aitrack.exe\"\r\n")?;
        zip.finish()?;
    }

    let signature = signer.sign_file(&setup)?;
    write_fragment(dist, platform, &setup, signature)?;

    println!("\nWindows package:");
    for bin in BINARIES {
        println!("  {bin:<24} {}", human(std::fs::metadata(stage.join(format!("{bin}.exe")))?.len()));
    }
    for f in [&setup, &portable, &symbols] {
        println!("  {:<62} {}", f.file_name().unwrap().to_string_lossy(), human(std::fs::metadata(f)?.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_carries_the_required_keys() {
        let plist = info_plist("0.0.1");
        for needle in [
            "<string>dev.aitrack.daw</string>",
            "<key>CFBundleShortVersionString</key>\n\t<string>0.0.1</string>",
            "<key>CFBundleVersion</key>\n\t<string>0.0.1</string>",
            "<key>LSMinimumSystemVersion</key>\n\t<string>13.0</string>",
            "<key>NSMicrophoneUsageDescription</key>",
            "<key>NSHighResolutionCapable</key>\n\t<true/>",
            "<key>CFBundleIconFile</key>\n\t<string>AppIcon</string>",
            "<string>public.app-category.music</string>",
            "<key>CFBundleExecutable</key>\n\t<string>aitrack</string>",
        ] {
            assert!(plist.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn nsis_wants_four_numbers() {
        assert_eq!(nsis_version("0.0.1"), "0.0.1.0");
        assert_eq!(nsis_version("1.2.3-rc.4"), "1.2.3.4");
    }

    #[test]
    fn the_app_version_is_the_workspace_version() {
        assert_eq!(VERSION, "0.0.1");
    }

    #[test]
    fn entitlements_match_the_plan() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let app = std::fs::read_to_string(root.join("release/macos/app.entitlements")).unwrap();
        let host = std::fs::read_to_string(root.join("release/macos/plugin-host.entitlements")).unwrap();
        for key in ["com.apple.security.device.audio-input", "com.apple.security.cs.disable-library-validation"] {
            assert!(app.contains(key) && host.contains(key), "{key}");
        }
        for key in ["com.apple.security.cs.allow-unsigned-executable-memory", "com.apple.security.cs.allow-jit"] {
            assert!(host.contains(key), "{key}");
            assert!(!app.contains(key), "{key} belongs to the plugin host only");
        }
    }
}
