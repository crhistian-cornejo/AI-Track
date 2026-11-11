//! On-demand model manager: a manifest of known models and ONNX Runtime
//! builds, HTTPS download with resume, SHA-256 verification, archive
//! extraction and per-model license acceptance.
//!
//! Nothing is bundled with the app. Models whose weights have an unclear or
//! restrictive license (Demucs) carry a `notice` and `requires_acceptance`:
//! [`ModelManager::ensure`] refuses to download them until the user has seen
//! the notice and [`ModelManager::accept_license`] was called.
//!
//! Layout: `<models dir>/<id>/<files>` plus `<id>/installed.json` and
//! `<models dir>/accepted.json`. The models dir is
//! `%APPDATA%\AI-Track\models` on Windows and
//! `~/Library/Application Support/AI-Track/models` on macOS, overridable with
//! `AITRACK_MODELS_DIR`.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::progress::{CancelToken, Progress, Stage};
use crate::{Error, Result};

/// Stable model ids.
pub mod ids {
    pub const BASIC_PITCH: &str = "basic-pitch";
    pub const HTDEMUCS: &str = "htdemucs";
    pub const HTDEMUCS_6S: &str = "htdemucs-6s";
    /// ONNX Runtime, CPU build (Windows) / CPU + CoreML build (macOS).
    pub const ONNXRUNTIME: &str = "onnxruntime";
    /// ONNX Runtime with the DirectML execution provider (Windows x64).
    pub const ONNXRUNTIME_DIRECTML: &str = "onnxruntime-directml";
}

/// ONNX Runtime release the `ort` 2.0.0-rc.15 crate targets (it works with any
/// runtime ≥ 1.17 because only the API-17 baseline is enabled).
pub const ORT_VERSION: &str = "1.31.1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// An ONNX model file.
    Onnx,
    /// The ONNX Runtime shared library (and its dependencies).
    Runtime,
}

/// One downloadable file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFile {
    pub url: String,
    /// Lower-case hex SHA-256 of the downloaded file.
    pub sha256: String,
    /// Exact size in bytes when known.
    pub size: Option<u64>,
    /// Name stored in the model folder.
    pub file_name: String,
    /// For archives: `(member path suffix, installed file name)`. The archive is
    /// deleted after extraction. Empty = the file itself is installed.
    #[serde(default)]
    pub extract: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ModelKind,
    pub version: String,
    pub files: Vec<ModelFile>,
    /// SPDX id (or "unclear") of the weights / binaries.
    pub license: String,
    pub license_url: String,
    /// Where the files come from (repository / release page).
    pub source: String,
    /// Text the UI must show before download when `requires_acceptance`.
    pub notice: Option<String>,
    pub requires_acceptance: bool,
    /// Separation models: output stems in tensor order.
    #[serde(default)]
    pub stems: Vec<String>,
    /// Model input sample rate (0 = n/a).
    pub sample_rate: u32,
}

impl ModelEntry {
    /// Total download size in bytes when all sizes are known.
    pub fn download_size(&self) -> Option<u64> {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Installed file names, primary first (the `.onnx`, or the runtime library).
    pub fn installed_names(&self) -> Vec<String> {
        self.files
            .iter()
            .flat_map(|f| {
                if f.extract.is_empty() {
                    vec![f.file_name.clone()]
                } else {
                    f.extract.iter().map(|e| e.1.clone()).collect()
                }
            })
            .collect()
    }
}

/// A model on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    pub dir: PathBuf,
    /// Installed files, primary first.
    pub paths: Vec<PathBuf>,
}

/// Listing row: manifest entry plus install / acceptance state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStatus {
    pub entry: ModelEntry,
    pub installed: bool,
    pub accepted: bool,
}

const DEMUCS_NOTICE: &str = "Demucs / HT-Demucs (Rouard, Massa, Défossez, Meta AI). The code is MIT-licensed. \
The ONNX export is published by StemSplit on Hugging Face under an MIT tag, but the original authors described the \
pre-trained weights as provided for research purposes (they were trained on MUSDB18-HQ and internal data), and the \
license of the weights has not been clarified. AI-Track does not distribute these weights: they are downloaded from \
the original source on your request. You are responsible for checking that your use complies with their terms.";

fn gh_release(file: &str) -> String {
    format!("https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{file}")
}

/// Every model and runtime known to this build, for the current platform.
pub fn manifest() -> Vec<ModelEntry> {
    let mut v = vec![
        ModelEntry {
            id: ids::BASIC_PITCH.into(),
            name: "basic-pitch".into(),
            description: "Spotify basic-pitch (ICASSP 2022): polyphonic, instrument-agnostic audio-to-MIDI with pitch bends.".into(),
            kind: ModelKind::Onnx,
            version: "0.4.0".into(),
            files: vec![ModelFile {
                url: "https://raw.githubusercontent.com/spotify/basic-pitch/v0.4.0/basic_pitch/saved_models/icassp_2022/nmp.onnx".into(),
                sha256: "2c3c1d144bfa61ad236e92e169c13535c880469a12a047d4e73451f2c059a0ec".into(),
                size: Some(230_444),
                file_name: "nmp.onnx".into(),
                extract: vec![],
            }],
            license: "Apache-2.0".into(),
            license_url: "https://github.com/spotify/basic-pitch/blob/v0.4.0/LICENSE".into(),
            source: "https://github.com/spotify/basic-pitch".into(),
            notice: None,
            requires_acceptance: false,
            stems: vec![],
            sample_rate: 22_050,
        },
        ModelEntry {
            id: ids::HTDEMUCS.into(),
            name: "HT-Demucs (4 stems)".into(),
            description: "Hybrid Transformer Demucs v4: drums, bass, other, vocals. 7.8 s segments at 44.1 kHz stereo; ~1.1 GB RAM.".into(),
            kind: ModelKind::Onnx,
            version: "htdemucs-onnx@d54ed9e".into(),
            files: vec![ModelFile {
                url: "https://huggingface.co/StemSplitio/htdemucs-onnx/resolve/d54ed9eb60e258ea82131c6ee14578628816456a/htdemucs.onnx".into(),
                sha256: "68d0bf16428ef66e692cdff8a9ccf28f1ef3f69440d57e58605a4cc55fcc5e74".into(),
                size: Some(316_446_953),
                file_name: "htdemucs.onnx".into(),
                extract: vec![],
            }],
            license: "unclear (code MIT; ONNX repo tagged MIT; weights described as research-only by the authors)".into(),
            license_url: "https://huggingface.co/adefossez/HTDemucs/discussions/1".into(),
            source: "https://huggingface.co/StemSplitio/htdemucs-onnx (export of facebookresearch/demucs htdemucs)".into(),
            notice: Some(DEMUCS_NOTICE.into()),
            requires_acceptance: true,
            stems: ["drums", "bass", "other", "vocals"].map(String::from).to_vec(),
            sample_rate: 44_100,
        },
        ModelEntry {
            id: ids::HTDEMUCS_6S.into(),
            name: "HT-Demucs (6 stems)".into(),
            description: "htdemucs_6s: drums, bass, other, vocals, guitar, piano. 7.8 s segments at 44.1 kHz stereo.".into(),
            kind: ModelKind::Onnx,
            version: "htdemucs-6s-onnx@49df9b6".into(),
            files: vec![ModelFile {
                url: "https://huggingface.co/StemSplitio/htdemucs-6s-onnx/resolve/49df9b6989cf2150840ea65b0bef77a2e471b678/htdemucs_6s.onnx".into(),
                sha256: "48f8e84945579f8ab340e083339e9221e03785dbe733a52c388200b6d3ca779a".into(),
                size: None,
                file_name: "htdemucs_6s.onnx".into(),
                extract: vec![],
            }],
            license: "unclear (code MIT; ONNX repo tagged MIT; weights described as research-only by the authors)".into(),
            license_url: "https://huggingface.co/adefossez/HTDemucs/discussions/1".into(),
            source: "https://huggingface.co/StemSplitio/htdemucs-6s-onnx (export of facebookresearch/demucs htdemucs_6s)".into(),
            notice: Some(DEMUCS_NOTICE.into()),
            requires_acceptance: true,
            stems: ["drums", "bass", "other", "vocals", "guitar", "piano"].map(String::from).to_vec(),
            sample_rate: 44_100,
        },
    ];
    v.extend(runtime_entries());
    v
}

fn ort_entry(
    id: &str,
    name: &str,
    description: &str,
    version: &str,
    files: Vec<ModelFile>,
) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        kind: ModelKind::Runtime,
        version: version.into(),
        files,
        license: "MIT".into(),
        license_url: "https://github.com/microsoft/onnxruntime/blob/main/LICENSE".into(),
        source: "https://github.com/microsoft/onnxruntime/releases (official Microsoft builds)"
            .into(),
        notice: None,
        requires_acceptance: false,
        stems: vec![],
        sample_rate: 0,
    }
}

#[allow(clippy::vec_init_then_push)]
fn runtime_entries() -> Vec<ModelEntry> {
    let mut v = Vec::new();
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        let zip = format!("onnxruntime-win-x64-{ORT_VERSION}.zip");
        v.push(ort_entry(
            ids::ONNXRUNTIME,
            "ONNX Runtime (CPU)",
            "Official Microsoft ONNX Runtime CPU build for Windows x64.",
            ORT_VERSION,
            vec![ModelFile {
                url: gh_release(&zip),
                sha256: "5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c".into(),
                size: Some(82_929_944),
                file_name: zip,
                extract: vec![
                    ("lib/onnxruntime.dll".into(), "onnxruntime.dll".into()),
                    (
                        "lib/onnxruntime_providers_shared.dll".into(),
                        "onnxruntime_providers_shared.dll".into(),
                    ),
                ],
            }],
        ));
        // DirectML builds stopped at 1.24.x on NuGet (DirectML is in sustained engineering).
        v.push(ort_entry(
            ids::ONNXRUNTIME_DIRECTML,
            "ONNX Runtime (DirectML GPU)",
            "Official Microsoft.ML.OnnxRuntime.DirectML 1.24.4 plus the DirectML 1.15.4 redistributable: GPU inference on any DirectX 12 GPU, CPU fallback.",
            "1.24.4",
            vec![
                ModelFile {
                    url: "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg".into(),
                    sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681".into(),
                    size: Some(12_458_649),
                    file_name: "microsoft.ml.onnxruntime.directml.1.24.4.nupkg".into(),
                    extract: vec![
                        ("runtimes/win-x64/native/onnxruntime.dll".into(), "onnxruntime.dll".into()),
                        ("runtimes/win-x64/native/onnxruntime_providers_shared.dll".into(), "onnxruntime_providers_shared.dll".into()),
                    ],
                },
                ModelFile {
                    url: "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg".into(),
                    sha256: "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9".into(),
                    size: Some(202_292_617),
                    file_name: "microsoft.ai.directml.1.15.4.nupkg".into(),
                    extract: vec![("bin/x64-win/DirectML.dll".into(), "DirectML.dll".into())],
                },
            ],
        ));
    }
    #[cfg(all(windows, target_arch = "aarch64"))]
    {
        let zip = format!("onnxruntime-win-arm64-{ORT_VERSION}.zip");
        v.push(ort_entry(
            ids::ONNXRUNTIME,
            "ONNX Runtime (CPU)",
            "Official Microsoft ONNX Runtime CPU build for Windows on Arm.",
            ORT_VERSION,
            vec![ModelFile {
                url: gh_release(&zip),
                sha256: "817f4129073f84f81cde3ff92b1a4e06d007691e7208deed08a8b6d6df143f7f".into(),
                size: Some(85_851_069),
                file_name: zip,
                extract: vec![
                    ("lib/onnxruntime.dll".into(), "onnxruntime.dll".into()),
                    (
                        "lib/onnxruntime_providers_shared.dll".into(),
                        "onnxruntime_providers_shared.dll".into(),
                    ),
                ],
            }],
        ));
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let tgz = format!("onnxruntime-osx-arm64-{ORT_VERSION}.tgz");
        v.push(ort_entry(
            ids::ONNXRUNTIME,
            "ONNX Runtime (CPU + CoreML)",
            "Official Microsoft ONNX Runtime build for Apple silicon, with the CoreML execution provider.",
            ORT_VERSION,
            vec![ModelFile {
                url: gh_release(&tgz),
                sha256: "2e1344a1926ee50eb25b38153cb6992c3567515aef0e677a1a520a1798896d6f".into(),
                size: Some(36_936_435),
                file_name: tgz,
                extract: vec![(format!("lib/libonnxruntime.{ORT_VERSION}.dylib"), "libonnxruntime.dylib".into())],
            }],
        ));
    }
    let _ = gh_release;
    let _ = ort_entry;
    v
}

#[derive(Default, Serialize, Deserialize)]
struct InstallRecord {
    id: String,
    version: String,
    sha256: Vec<String>,
}

pub struct ModelManager {
    dir: PathBuf,
    entries: Vec<ModelEntry>,
}

impl ModelManager {
    /// Manager over the default per-user models directory.
    pub fn new() -> Result<Self> {
        Ok(Self::with_dir(default_dir()?))
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            entries: manifest(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn entry(&self, id: &str) -> Result<&ModelEntry> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| Error::UnknownModel(id.into()))
    }

    pub fn list(&self) -> Vec<ModelStatus> {
        let accepted = self.accepted();
        self.entries
            .iter()
            .map(|e| ModelStatus {
                entry: e.clone(),
                installed: self.installed(&e.id).is_some(),
                accepted: !e.requires_acceptance || accepted.contains(&e.id),
            })
            .collect()
    }

    fn accepted_path(&self) -> PathBuf {
        self.dir.join("accepted.json")
    }

    fn accepted(&self) -> BTreeSet<String> {
        fs::read(self.accepted_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Record that the user accepted the model's notice (persisted).
    pub fn accept_license(&self, id: &str) -> Result<()> {
        self.entry(id)?;
        let mut set = self.accepted();
        set.insert(id.to_string());
        fs::create_dir_all(&self.dir).map_err(|e| Error::io(&self.dir, e))?;
        let p = self.accepted_path();
        fs::write(&p, serde_json::to_vec_pretty(&set).expect("set serialises"))
            .map_err(|e| Error::io(&p, e))
    }

    pub fn is_accepted(&self, id: &str) -> bool {
        self.entry(id).is_ok_and(|e| !e.requires_acceptance) || self.accepted().contains(id)
    }

    /// Installed and matching the manifest's hashes.
    pub fn installed(&self, id: &str) -> Option<InstalledModel> {
        let e = self.entry(id).ok()?;
        let dir = self.dir.join(&e.id);
        let rec: InstallRecord =
            serde_json::from_slice(&fs::read(dir.join("installed.json")).ok()?).ok()?;
        let want: Vec<String> = e.files.iter().map(|f| f.sha256.clone()).collect();
        if rec.sha256 != want {
            return None;
        }
        let paths: Vec<PathBuf> = e
            .installed_names()
            .into_iter()
            .map(|n| dir.join(n))
            .collect();
        paths.iter().all(|p| p.is_file()).then(|| InstalledModel {
            id: e.id.clone(),
            dir,
            paths,
        })
    }

    /// Return the installed model, downloading and verifying it first if needed.
    pub fn ensure(&self, id: &str, progress: &mut dyn FnMut(Progress)) -> Result<InstalledModel> {
        self.ensure_cancellable(id, progress, &CancelToken::new())
    }

    pub fn ensure_cancellable(
        &self,
        id: &str,
        progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<InstalledModel> {
        if let Some(m) = self.installed(id) {
            return Ok(m);
        }
        let e = self.entry(id)?.clone();
        if e.requires_acceptance && !self.is_accepted(id) {
            return Err(Error::LicenseNotAccepted(id.into()));
        }
        let dir = self.dir.join(&e.id);
        fs::create_dir_all(&dir).map_err(|err| Error::io(&dir, err))?;
        let total: u64 = e
            .files
            .iter()
            .map(|f| f.size.unwrap_or(1))
            .sum::<u64>()
            .max(1);
        let mut done_before = 0u64;
        for f in &e.files {
            let target = dir.join(&f.file_name);
            let weight = f.size.unwrap_or(1);
            let base = done_before;
            let mut on_bytes = |got: u64, len: Option<u64>| {
                let local = len.map_or(0.0, |l| got as f64 / l.max(1) as f64);
                let frac = (base as f64 + local * weight as f64) / total as f64;
                progress(Progress::new(
                    Stage::Downloading,
                    frac as f32 * 0.95,
                    format!("{} {:.1} MB", f.file_name, got as f64 / 1e6),
                ));
            };
            download(&f.url, &target, &f.sha256, f.size, &mut on_bytes, cancel)?;
            if !f.extract.is_empty() {
                progress(Progress::new(
                    Stage::Extracting,
                    (done_before + weight) as f32 / total as f32 * 0.95,
                    format!("Extracting {}", f.file_name),
                ));
                extract(&target, &f.extract, &dir)?;
                fs::remove_file(&target).ok();
            }
            done_before += weight;
        }
        let rec = InstallRecord {
            id: e.id.clone(),
            version: e.version.clone(),
            sha256: e.files.iter().map(|f| f.sha256.clone()).collect(),
        };
        let rec_path = dir.join("installed.json");
        fs::write(
            &rec_path,
            serde_json::to_vec_pretty(&rec).expect("record serialises"),
        )
        .map_err(|err| Error::io(&rec_path, err))?;
        progress(Progress::new(
            Stage::Done,
            1.0,
            format!("{} installed", e.name),
        ));
        self.installed(id)
            .ok_or_else(|| Error::NotInstalled(id.into()))
    }

    /// Delete an installed model.
    pub fn remove(&self, id: &str) -> Result<()> {
        let e = self.entry(id)?;
        let dir = self.dir.join(&e.id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|err| Error::io(&dir, err))?;
        }
        Ok(())
    }

    /// The ONNX Runtime variant this platform should use: DirectML on Windows
    /// x64 when `prefer_gpu` (or when it is already installed), else the
    /// default build (which includes CoreML on macOS).
    pub fn runtime_id(&self, prefer_gpu: bool) -> Result<&'static str> {
        let has = |id: &str| self.entries.iter().any(|e| e.id == id);
        if has(ids::ONNXRUNTIME_DIRECTML)
            && (prefer_gpu || self.installed(ids::ONNXRUNTIME_DIRECTML).is_some())
        {
            return Ok(ids::ONNXRUNTIME_DIRECTML);
        }
        if has(ids::ONNXRUNTIME) {
            return Ok(ids::ONNXRUNTIME);
        }
        Err(Error::UnsupportedPlatform {
            id: ids::ONNXRUNTIME.into(),
        })
    }
}

/// Persistent cache for the network tests (`AITRACK_MODELS_DIR` wins).
#[cfg(test)]
pub(crate) fn test_models_dir() -> PathBuf {
    std::env::var_os("AITRACK_MODELS_DIR").map_or_else(
        || std::env::temp_dir().join("aitrack-test-models"),
        PathBuf::from,
    )
}

pub fn default_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("AITRACK_MODELS_DIR") {
        return Ok(PathBuf::from(d));
    }
    // Windows: %APPDATA% (Roaming); macOS: ~/Library/Application Support.
    dirs::data_dir()
        .map(|d| d.join("AI-Track").join("models"))
        .ok_or_else(|| Error::InvalidInput("no per-user data directory on this system".into()))
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        // The OS trust store, so corporate TLS proxies and user-installed roots work.
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(None)
        .user_agent(concat!("AI-Track/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

fn sha256_file(path: &Path) -> Result<(Sha256, u64)> {
    let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
    Ok((h, n))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download `url` to `target`, resuming a previous `<target>.part`, verifying
/// size and SHA-256. On mismatch the partial file is deleted.
pub fn download(
    url: &str,
    target: &Path,
    sha256: &str,
    size: Option<u64>,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
    cancel: &CancelToken,
) -> Result<()> {
    if target.is_file() {
        let (h, n) = sha256_file(target)?;
        if hex(&h.finalize()).eq_ignore_ascii_case(sha256) && size.is_none_or(|s| s == n) {
            return Ok(());
        }
        fs::remove_file(target).ok();
    }
    let part = target.with_extension(format!(
        "{}.part",
        target.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));
    let dl_err = |message: String| Error::Download {
        url: url.to_string(),
        message,
    };
    let (mut hasher, mut have) = if part.is_file() {
        sha256_file(&part)?
    } else {
        (Sha256::new(), 0)
    };
    if size.is_some_and(|s| have > s) {
        fs::remove_file(&part).ok();
        (hasher, have) = (Sha256::new(), 0);
    }
    let agent = agent();
    let mut req = agent.get(url);
    if have > 0 {
        req = req.header("Range", &format!("bytes={have}-"));
    }
    let resp = req.call().map_err(|e| dl_err(e.to_string()))?;
    let status = resp.status().as_u16();
    let mut file = match status {
        206 => {
            let mut f = OpenOptions::new()
                .append(true)
                .open(&part)
                .map_err(|e| Error::io(&part, e))?;
            f.seek(SeekFrom::End(0)).map_err(|e| Error::io(&part, e))?;
            f
        }
        200 => {
            (hasher, have) = (Sha256::new(), 0);
            File::create(&part).map_err(|e| Error::io(&part, e))?
        }
        416 if size == Some(have) => OpenOptions::new()
            .append(true)
            .open(&part)
            .map_err(|e| Error::io(&part, e))?,
        s => return Err(dl_err(format!("HTTP {s}"))),
    };
    let total = size.or_else(|| {
        resp.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|l| l + have)
    });
    if status != 416 {
        let mut reader = resp.into_body().into_reader();
        let mut buf = vec![0u8; 256 * 1024];
        let mut last_report = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| dl_err(e.to_string()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| Error::io(&part, e))?;
            hasher.update(&buf[..n]);
            have += n as u64;
            if have - last_report >= 1 << 20 {
                on_bytes(have, total);
                last_report = have;
            }
        }
        file.flush().map_err(|e| Error::io(&part, e))?;
    }
    drop(file);
    on_bytes(have, total);
    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(sha256) || size.is_some_and(|s| s != have) {
        fs::remove_file(&part).ok();
        return Err(Error::Checksum {
            file: target.display().to_string(),
            expected: sha256.into(),
            actual,
        });
    }
    fs::rename(&part, target).map_err(|e| Error::io(target, e))
}

/// Extract archive members (matched by path suffix) into `dir`.
fn extract(archive: &Path, members: &[(String, String)], dir: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut found = vec![false; members.len()];
    let matches = |path: &str| -> Option<usize> {
        let p = path.replace('\\', "/");
        members
            .iter()
            .position(|(suffix, _)| p == *suffix || p.ends_with(&format!("/{suffix}")))
    };
    let write = |idx: usize, r: &mut dyn Read| -> Result<()> {
        let out = dir.join(&members[idx].1);
        let tmp = out.with_extension("tmp");
        let mut f = File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        std::io::copy(r, &mut f).map_err(|e| Error::io(&tmp, e))?;
        drop(f);
        fs::rename(&tmp, &out).map_err(|e| Error::io(&out, e))
    };
    if name.ends_with(".zip") || name.ends_with(".nupkg") {
        let f = File::open(archive).map_err(|e| Error::io(archive, e))?;
        let mut z = zip::ZipArchive::new(f)
            .map_err(|e| Error::Decode(format!("{}: {e}", archive.display())))?;
        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| Error::Decode(e.to_string()))?;
            if let Some(idx) = matches(entry.name()) {
                write(idx, &mut entry)?;
                found[idx] = true;
            }
        }
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        let f = File::open(archive).map_err(|e| Error::io(archive, e))?;
        let mut t = tar::Archive::new(flate2::read::GzDecoder::new(f));
        for entry in t.entries().map_err(|e| Error::io(archive, e))? {
            let mut entry = entry.map_err(|e| Error::io(archive, e))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry
                .path()
                .map_err(|e| Error::io(archive, e))?
                .to_string_lossy()
                .to_string();
            if let Some(idx) = matches(&path) {
                write(idx, &mut entry)?;
                found[idx] = true;
            }
        }
    } else {
        return Err(Error::InvalidInput(format!("unknown archive type {name}")));
    }
    if let Some(i) = found.iter().position(|f| !f) {
        return Err(Error::Decode(format!(
            "{} has no member {}",
            archive.display(),
            members[i].0
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitrack-models-{name}-{}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        d
    }

    #[test]
    fn manifest_is_well_formed() {
        let m = manifest();
        assert!(m.iter().any(|e| e.id == ids::BASIC_PITCH));
        for e in &m {
            assert!(!e.files.is_empty(), "{}", e.id);
            for f in &e.files {
                assert_eq!(f.sha256.len(), 64, "{}", e.id);
                assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(f.url.starts_with("https://"));
            }
            assert!(!e.requires_acceptance || e.notice.is_some());
        }
        let demucs = m.iter().find(|e| e.id == ids::HTDEMUCS).unwrap();
        assert!(demucs.requires_acceptance);
        assert_eq!(demucs.stems, vec!["drums", "bass", "other", "vocals"]);
    }

    #[test]
    fn license_gate_and_acceptance_persist() {
        let dir = temp("accept");
        let mm = ModelManager::with_dir(&dir);
        assert!(!mm.is_accepted(ids::HTDEMUCS));
        assert!(matches!(
            mm.ensure(ids::HTDEMUCS, &mut |_| {}),
            Err(Error::LicenseNotAccepted(_))
        ));
        mm.accept_license(ids::HTDEMUCS).unwrap();
        assert!(ModelManager::with_dir(&dir).is_accepted(ids::HTDEMUCS));
        assert!(mm.is_accepted(ids::BASIC_PITCH));
        assert!(matches!(
            mm.ensure("nope", &mut |_| {}),
            Err(Error::UnknownModel(_))
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zip_extraction_by_suffix() {
        let dir = temp("zip");
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&p).unwrap());
            let o: zip::write::SimpleFileOptions = Default::default();
            z.start_file("pkg-1.0/lib/thing.dll", o).unwrap();
            z.write_all(b"hello").unwrap();
            z.start_file("pkg-1.0/include/x.h", o).unwrap();
            z.write_all(b"h").unwrap();
            z.finish().unwrap();
        }
        extract(&p, &[("lib/thing.dll".into(), "thing.dll".into())], &dir).unwrap();
        assert_eq!(fs::read(dir.join("thing.dll")).unwrap(), b"hello");
        assert!(extract(&p, &[("lib/missing.dll".into(), "m.dll".into())], &dir).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// Network: downloads basic-pitch (230 KB) and checks the pinned hash.
    #[test]
    #[ignore = "network"]
    fn download_basic_pitch() {
        let dir = temp("dl");
        let mm = ModelManager::with_dir(&dir);
        let m = mm.ensure(ids::BASIC_PITCH, &mut |_| {}).unwrap();
        assert_eq!(fs::metadata(&m.paths[0]).unwrap().len(), 230_444);
        assert!(mm.installed(ids::BASIC_PITCH).is_some());
        fs::remove_dir_all(&dir).ok();
    }
}
