//! Adapters that implement the [`bridge`](crate::bridge) traits over the real
//! crates: the audio engine (`aitrack-engine`), stock devices
//! (`aitrack-stock`), hosted VST3/CLAP plugins (`aitrack-host`), the ML
//! features (`aitrack-ai`) and the sample library (`aitrack-library`).

pub mod agents;
pub mod ai;
pub mod engine;
pub mod library;
pub mod main_thread;
pub mod plugins;
pub mod preview;

use crate::bridge::Bridges;
use aitrack_engine::FactoryStack;
use std::sync::Arc;

/// Persisted engine settings (`engine.json` in the config folder).
pub mod settings {
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    #[derive(Clone, Debug, Serialize, Deserialize)]
    #[serde(default)]
    pub struct Settings {
        pub audio: aitrack_engine::AudioConfig,
        /// Run third-party plugins in `aitrack-plugin-host` processes.
        pub sandbox: bool,
        /// Memory budget (in MB) for decoded audio samples before LRU eviction.
        pub sample_cache_mb: usize,
        /// Folder holding the factory sound bank (`factory.db` + `packs/`).
        pub content_dir: Option<PathBuf>,
    }

    impl Default for Settings {
        fn default() -> Self {
            Self {
                audio: aitrack_engine::AudioConfig::default(),
                sandbox: false,
                sample_cache_mb: 512,
                content_dir: None,
            }
        }
    }

    fn path() -> PathBuf {
        crate::appearance::config_dir().join("engine.json")
    }

    impl Settings {
        pub fn load() -> Self {
            std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
        }

        pub fn save(&self) {
            let p = path();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_vec_pretty(self) {
                let _ = std::fs::write(p, json);
            }
        }
    }
}

/// Stock devices first, then hosted plugins.
pub fn factory(rt: &Arc<plugins::PluginRuntime>, live: bool) -> FactoryStack {
    FactoryStack::new()
        .with(Arc::new(plugins::StockFactory))
        .with(Arc::new(plugins::HostedFactory::new(rt.clone(), live)))
}

/// Every bridge wired to the real crates. Call on the main thread.
pub fn bridges() -> Bridges {
    let settings = settings::Settings::load();
    let rt = plugins::PluginRuntime::new(main_thread::MainThread::new(), settings.sandbox);
    let live = Arc::new(factory(&rt, true));
    let export = Arc::new(factory(&rt, false));
    let catalog = plugins::RealCatalog::new(rt.clone(), crate::appearance::config_dir().join("plugins.json"));
    let library = library::RealLibrary::new(settings.content_dir.as_deref());
    let engine = engine::RealEngine::new(live, export, rt, settings);
    Bridges {
        engine: Box::new(engine),
        plugins: Box::new(catalog),
        library: Box::new(library),
        ai: Box::new(ai::RealAi::new()),
    }
}
