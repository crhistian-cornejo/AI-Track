//! Persistent scan cache (JSON) and blocklist.
//!
//! Entries are keyed by plugin path and validated by the binary's `mtime`
//! and size: an unchanged plugin is never loaded again. Plugins that crash,
//! hang or fail to scan go to the blocklist with the same key, so they are
//! skipped until the file changes or the user unblocks them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use aitrack_model::PluginFormat;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::info::HostedPluginInfo;

pub const CACHE_VERSION: u32 = 1;

/// Identity of a plugin file on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileKey {
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: u128,
    pub size: u64,
}

impl FileKey {
    /// Key of the plugin binary (inside the bundle for bundles).
    pub fn of(format: PluginFormat, path: &Path) -> Option<Self> {
        let target = match format {
            PluginFormat::Vst3 => crate::info::vst3_binary_path(path),
            PluginFormat::Clap if path.is_dir() => macos_bundle_binary(path).unwrap_or_else(|| path.to_path_buf()),
            PluginFormat::Clap => path.to_path_buf(),
        };
        let meta = std::fs::metadata(&target).or_else(|_| std::fs::metadata(path)).ok()?;
        let mtime_ns = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
        Some(Self { mtime_ns, size: meta.len() })
    }
}

fn macos_bundle_binary(bundle: &Path) -> Option<PathBuf> {
    let dir = bundle.join("Contents").join("MacOS");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub format: PluginFormat,
    pub key: FileKey,
    pub plugins: Vec<HostedPluginInfo>,
    /// True when the entry came from `moduleinfo.json` (no binary loaded).
    #[serde(default)]
    pub from_moduleinfo: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// The scanner process exceeded the timeout and was killed.
    Timeout,
    /// The scanner process died (crash, abort) without a result.
    Crashed,
    /// The plugin loaded but reported an error (no factory, no classes…).
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockEntry {
    pub format: PluginFormat,
    pub key: FileKey,
    pub reason: BlockReason,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanCache {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<PathBuf, CacheEntry>,
    #[serde(default)]
    pub blocklist: BTreeMap<PathBuf, BlockEntry>,
}

impl ScanCache {
    /// Load from `path`; a missing, corrupt or old-version file gives an
    /// empty cache (the blocklist of a corrupt file is lost; plugins will be
    /// rescanned, which is safe because scanning is out of process).
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Self::empty() };
        match serde_json::from_str::<ScanCache>(&text) {
            Ok(c) if c.version == CACHE_VERSION => c,
            Ok(_) => Self::empty(),
            Err(e) => {
                log::warn!("plugin cache {} unreadable ({e}); starting over", path.display());
                Self::empty()
            }
        }
    }

    pub fn empty() -> Self {
        Self { version: CACHE_VERSION, ..Default::default() }
    }

    /// Atomic save (write to a temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn fresh_entry(&self, path: &Path, key: FileKey) -> Option<&CacheEntry> {
        self.entries.get(path).filter(|e| e.key == key)
    }

    pub fn is_blocked(&self, path: &Path, key: FileKey) -> bool {
        self.blocklist.get(path).is_some_and(|b| b.key == key)
    }

    pub fn plugins(&self) -> impl Iterator<Item = &HostedPluginInfo> {
        self.entries.values().flat_map(|e| e.plugins.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_keys() {
        let dir = std::env::temp_dir().join(format!("aitrack-cache-{}", std::process::id()));
        let file = dir.join("x.clap");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, b"abc").unwrap();
        let key = FileKey::of(PluginFormat::Clap, &file).unwrap();
        assert_eq!(key.size, 3);

        let mut c = ScanCache::empty();
        c.blocklist.insert(file.clone(), BlockEntry { format: PluginFormat::Clap, key, reason: BlockReason::Crashed, message: "boom".into() });
        let path = dir.join("cache.json");
        c.save(&path).unwrap();
        let back = ScanCache::load(&path);
        assert_eq!(back, c);
        assert!(back.is_blocked(&file, key));
        assert!(!back.is_blocked(&file, FileKey { size: 4, ..key }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_cache_is_empty() {
        let dir = std::env::temp_dir().join(format!("aitrack-cache-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(ScanCache::load(&path), ScanCache::empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
