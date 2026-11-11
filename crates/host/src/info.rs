//! Format-neutral description of a hosted plugin, as stored in the scan
//! cache and referenced by projects (`DeviceKind::Plugin`).

use std::path::PathBuf;

use aitrack_model::{DeviceKind, PluginFormat};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostedPluginInfo {
    pub format: PluginFormat,
    /// VST3: the processor class id as 32 upper-case hex digits.
    /// CLAP: the plugin id (`com.vendor.plugin`).
    pub uid: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// VST3 sub-categories (`"Fx|EQ"`) or CLAP features joined with `|`.
    pub category: String,
    pub is_instrument: bool,
    /// The bundle or file that was scanned (`.vst3` / `.clap`).
    pub path: PathBuf,
    /// Channels of the main input / output bus (0 when unknown).
    pub inputs: u16,
    pub outputs: u16,
    /// Number of parameters (0 when read from `moduleinfo.json`, which does
    /// not list them).
    pub params: u32,
    /// True when bus/param counts come from instantiating the plugin rather
    /// than from metadata only.
    #[serde(default)]
    pub probed: bool,
}

impl HostedPluginInfo {
    /// The project-model device kind referring to this plugin.
    pub fn device_kind(&self) -> DeviceKind {
        DeviceKind::Plugin {
            format: self.format,
            uid: self.uid.clone(),
            path: self.path.clone(),
            vendor: self.vendor.clone(),
            is_instrument: self.is_instrument,
        }
    }

    /// Stable id used for `aitrack_dsp::PluginInfo::id`.
    pub fn stable_id(&self) -> String {
        match self.format {
            PluginFormat::Vst3 => format!("vst3:{}", self.uid),
            PluginFormat::Clap => format!("clap:{}", self.uid),
        }
    }
}

/// The library file inside a `.vst3` bundle for this platform, or the path
/// itself for single-file (legacy Windows) VST3 modules.
pub fn vst3_binary_path(bundle: &std::path::Path) -> PathBuf {
    if bundle.is_file() {
        return bundle.to_path_buf();
    }
    let stem = bundle.file_stem().map(|s| s.to_os_string()).unwrap_or_default();
    #[cfg(windows)]
    {
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64-win"
        } else if cfg!(target_arch = "x86") {
            "x86-win"
        } else {
            "x86_64-win"
        };
        let mut file = stem;
        file.push(".vst3");
        let primary = bundle.join("Contents").join(arch).join(&file);
        if !primary.exists() && cfg!(target_arch = "aarch64") {
            let ec = bundle.join("Contents").join("arm64ec-win").join(&file);
            if ec.exists() {
                return ec;
            }
        }
        primary
    }
    #[cfg(target_os = "macos")]
    {
        bundle.join("Contents").join("MacOS").join(stem)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mut file = stem;
        file.push(".so");
        bundle.join("Contents").join("x86_64-linux").join(file)
    }
}

/// Format a 16-byte VST3 TUID as 32 upper-case hex digits (byte order as
/// stored, which is what moduleinfo.json and `PClassInfo::cid` use).
pub fn tuid_to_hex(tuid: &[u8; 16]) -> String {
    tuid.iter().map(|b| format!("{b:02X}")).collect()
}

pub fn hex_to_tuid(hex: &str) -> Option<[u8; 16]> {
    let hex: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuid_hex_round_trip() {
        let t: [u8; 16] = core::array::from_fn(|i| (i * 17) as u8);
        let h = tuid_to_hex(&t);
        assert_eq!(h.len(), 32);
        assert_eq!(hex_to_tuid(&h), Some(t));
    }

    #[test]
    fn info_serde_round_trip() {
        let info = HostedPluginInfo {
            format: PluginFormat::Clap,
            uid: "org.example.gain".into(),
            name: "Gain".into(),
            vendor: "Example".into(),
            version: "1.0".into(),
            category: "audio-effect".into(),
            is_instrument: false,
            path: "C:/x.clap".into(),
            inputs: 2,
            outputs: 2,
            params: 1,
            probed: true,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: HostedPluginInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
        assert_eq!(info.stable_id(), "clap:org.example.gain");
    }
}
