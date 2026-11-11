//! Standard plugin folders and discovery of plugin files inside them.

use std::path::{Path, PathBuf};

use aitrack_model::PluginFormat;

/// The platform's standard folders for `format`, plus `CLAP_PATH` entries
/// for CLAP (as the CLAP spec asks hosts to honour).
pub fn default_search_paths(format: PluginFormat) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let sub = match format {
        PluginFormat::Vst3 => "VST3",
        PluginFormat::Clap => "CLAP",
    };
    #[cfg(windows)]
    {
        if let Some(common) = std::env::var_os("CommonProgramFiles") {
            out.push(PathBuf::from(common).join(sub));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(PathBuf::from(local).join("Programs").join("Common").join(sub));
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from("/Library/Audio/Plug-Ins").join(sub));
        if let Some(home) = std::env::var_os("HOME") {
            out.push(PathBuf::from(home).join("Library/Audio/Plug-Ins").join(sub));
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = sub;
    }
    if format == PluginFormat::Clap
        && let Some(extra) = std::env::var_os("CLAP_PATH")
    {
        out.extend(std::env::split_paths(&extra));
    }
    out.dedup();
    out
}

pub fn extension(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Clap => "clap",
    }
}

/// Recursively find `.vst3` / `.clap` entries (files or bundle
/// directories) under `roots`. Bundles are not descended into.
pub fn find_plugin_files(roots: &[PathBuf], format: PluginFormat) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        walk(root, extension(format), 0, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &Path, ext: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_plugin = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
        if is_plugin {
            out.push(path);
        } else if path.is_dir() {
            walk(&path, ext, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_files_and_bundles_without_descending() {
        let root = std::env::temp_dir().join(format!("aitrack-scan-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Vendor/A.vst3/Contents/x86_64-win")).unwrap();
        std::fs::write(root.join("Vendor/A.vst3/Contents/x86_64-win/A.vst3"), b"x").unwrap();
        std::fs::write(root.join("B.vst3"), b"x").unwrap();
        std::fs::write(root.join("C.clap"), b"x").unwrap();
        let vst3 = find_plugin_files(std::slice::from_ref(&root), PluginFormat::Vst3);
        assert_eq!(vst3, vec![root.join("B.vst3"), root.join("Vendor").join("A.vst3")]);
        let clap = find_plugin_files(std::slice::from_ref(&root), PluginFormat::Clap);
        assert_eq!(clap, vec![root.join("C.clap")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_paths_exist_for_platform() {
        let p = default_search_paths(PluginFormat::Vst3);
        if cfg!(any(windows, target_os = "macos")) {
            assert!(!p.is_empty());
            assert!(p.iter().all(|d| d.ends_with("VST3")));
        }
    }
}
