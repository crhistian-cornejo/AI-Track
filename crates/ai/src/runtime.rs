//! ONNX Runtime, loaded at run time (`ort` with `load-dynamic`).
//!
//! The shared library is not shipped with the app: [`ensure_loaded`] asks the
//! [`ModelManager`] for the platform's official Microsoft build (downloading it
//! once), loads it into the process, and keeps it for the process lifetime.
//! A process can load only one ONNX Runtime; the first variant wins.
//!
//! Execution providers: CoreML on macOS, DirectML on Windows (with the
//! DirectML runtime variant), always followed by the CPU provider, so a
//! missing or failing GPU provider degrades to CPU.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::environment::Environment;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

use crate::models::ModelManager;
use crate::progress::Progress;
use crate::{Error, Result};

/// Which hardware to ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accel {
    /// CPU only (small models: basic-pitch).
    Cpu,
    /// GPU / Neural Engine when available, CPU fallback.
    #[default]
    Auto,
}

struct Loaded {
    env: Environment,
    library: PathBuf,
    runtime_id: &'static str,
}

static LOADED: OnceLock<Loaded> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

/// Path of the loaded ONNX Runtime library and the manifest id it came from.
pub fn loaded() -> Option<(&'static Path, &'static str)> {
    LOADED.get().map(|l| (l.library.as_path(), l.runtime_id))
}

/// Make sure an ONNX Runtime is loaded, installing it if needed.
/// `AITRACK_ORT_DYLIB` points at a library to use instead (development).
pub fn ensure_loaded(
    manager: &ModelManager,
    accel: Accel,
    progress: &mut dyn FnMut(Progress),
) -> Result<&'static Environment> {
    if let Some(l) = LOADED.get() {
        return Ok(&l.env);
    }
    let _guard = INIT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(l) = LOADED.get() {
        return Ok(&l.env);
    }
    let (library, runtime_id) = match std::env::var_os("AITRACK_ORT_DYLIB") {
        Some(p) => (PathBuf::from(p), "env"),
        None => {
            let id = manager.runtime_id(accel == Accel::Auto)?;
            let installed = manager.ensure(id, progress)?;
            preload_dependencies(&installed.dir);
            (installed.paths[0].clone(), id)
        }
    };
    let env = ort::init_from(&library)
        .map_err(|e| Error::Runtime(format!("loading {}: {e}", library.display())))?
        .with_name("aitrack")
        .build()?;
    let _ = LOADED.set(Loaded {
        env,
        library,
        runtime_id,
    });
    Ok(&LOADED.get().expect("just set").env)
}

/// Windows resolves a DLL's dependencies from the application directory, not
/// from the DLL's own folder, so DirectML.dll next to onnxruntime.dll would be
/// missed (and an older System32 copy used). Loading it first by absolute path
/// makes the later by-name lookup hit the already loaded module.
fn preload_dependencies(dir: &Path) {
    #[cfg(windows)]
    for name in ["DirectML.dll", "onnxruntime_providers_shared.dll"] {
        let p = dir.join(name);
        if p.is_file() {
            // SAFETY: loading a Microsoft-signed system library we verified by SHA-256; its
            // initialisers have no preconditions. It stays loaded for the process lifetime.
            match unsafe { libloading::Library::new(&p) } {
                Ok(lib) => std::mem::forget(lib),
                Err(e) => log::warn!("could not preload {}: {e}", p.display()),
            }
        }
    }
    #[cfg(not(windows))]
    let _ = dir;
}

fn execution_providers(accel: Accel) -> Vec<ort::ep::ExecutionProviderDispatch> {
    let mut eps = Vec::new();
    if accel == Accel::Auto {
        #[cfg(target_os = "macos")]
        eps.push(
            ort::ep::CoreML::default()
                .with_compute_units(ort::ep::coreml::ComputeUnits::All)
                .build(),
        );
        #[cfg(windows)]
        eps.push(ort::ep::DirectML::default().build());
    }
    eps.push(ort::ep::CPU::default().build());
    eps
}

/// Intra-op threads per session: `AITRACK_ORT_THREADS`, else all cores but
/// one (leaving room for the UI and audio), capped at 4 in tests.
pub fn intra_threads() -> usize {
    if let Some(n) = std::env::var("AITRACK_ORT_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return n.max(1);
    }
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let n = cores.saturating_sub(1).max(1);
    if cfg!(test) { n.min(4) } else { n }
}

/// Build a session for `model`, loading the runtime first if needed.
pub fn session(
    manager: &ModelManager,
    model: &Path,
    accel: Accel,
    progress: &mut dyn FnMut(Progress),
) -> Result<Session> {
    let env = ensure_loaded(manager, accel, progress)?;
    let mut builder = Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(intra_threads())?
        .with_execution_providers(execution_providers(accel))?;
    builder
        .commit_from_file(model)
        .map_err(|e| Error::Runtime(format!("loading {}: {e}", model.display())))
}

/// Static input shape of the first input, if the model declares one (dynamic dims are -1).
pub fn input_shape(session: &Session) -> Option<Vec<i64>> {
    session
        .inputs()
        .first()
        .and_then(|i| i.dtype().tensor_shape())
        .map(|s| s.to_vec())
}

pub fn output_shape(session: &Session) -> Option<Vec<i64>> {
    session
        .outputs()
        .first()
        .and_then(|o| o.dtype().tensor_shape())
        .map(|s| s.to_vec())
}
