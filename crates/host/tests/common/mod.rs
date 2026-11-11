//! Shared test helpers: builds the `test-gain` fixture plugin once per test
//! binary and lays it out as a `.clap` file and a `.vst3` bundle.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub struct Fixture {
    pub dir: PathBuf,
    pub clap: PathBuf,
    pub vst3: PathBuf,
}

fn target_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("..").join("..").join("target").join("host-fixtures")
}

fn lib_name() -> &'static str {
    if cfg!(windows) {
        "aitrack_test_gain.dll"
    } else if cfg!(target_os = "macos") {
        "libaitrack_test_gain.dylib"
    } else {
        "libaitrack_test_gain.so"
    }
}

fn copy_if_changed(src: &Path, dst: &Path) {
    let same = match (std::fs::metadata(src), std::fs::metadata(dst)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified().ok() <= b.modified().ok(),
        _ => false,
    };
    if !same {
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(src, dst).unwrap_or_else(|e| panic!("copy {} → {}: {e}", src.display(), dst.display()));
    }
}

pub fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("test-gain").join("Cargo.toml");
        let target = target_dir();
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target)
            .status()
            .expect("failed to run cargo for the test fixture");
        assert!(status.success(), "building the test-gain fixture failed");
        let lib = target.join("debug").join(lib_name());
        let dir = target.join("plugins");
        let clap = dir.join("AITrackTestGain.clap");
        let vst3 = dir.join("AITrackTestGain.vst3");
        if cfg!(target_os = "macos") {
            for (bundle, exe) in [(&clap, "AITrackTestGain"), (&vst3, "AITrackTestGain")] {
                copy_if_changed(&lib, &bundle.join("Contents").join("MacOS").join(exe));
                let plist = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                     <key>CFBundleExecutable</key><string>{exe}</string>\
                     <key>CFBundleIdentifier</key><string>org.aitrack.test-gain</string>\
                     <key>CFBundlePackageType</key><string>BNDL</string></dict></plist>"
                );
                std::fs::write(bundle.join("Contents").join("Info.plist"), plist).unwrap();
            }
        } else {
            copy_if_changed(&lib, &clap);
            let arch = if cfg!(target_arch = "aarch64") { "arm64-win" } else { "x86_64-win" };
            copy_if_changed(&lib, &vst3.join("Contents").join(arch).join("AITrackTestGain.vst3"));
        }
        Fixture { dir, clap, vst3 }
    })
}

pub fn constant_buffer(channels: usize, frames: usize, value: f32) -> aitrack_dsp::AudioBuffer {
    let mut b = aitrack_dsp::AudioBuffer::new(channels, frames);
    for ch in 0..channels {
        b.channel_mut(ch).fill(value);
    }
    b
}
