//! What the app and the stdio shim share through the config folder: the
//! settings (`mcp.json`: on or off, port, apply directly) and the bearer
//! token (`mcp-token`). The token is made once per install from the OS
//! random source; the shim reads it, so it never has to appear in a client's
//! config file.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The port the server tries unless the user picked another.
pub const DEFAULT_PORT: u16 = 47615;
/// The URL path of the endpoint.
pub const PATH: &str = "/mcp";

/// `%APPDATA%\AI-Track` on Windows, `~/Library/Application Support/AI-Track`
/// on macOS; `AITRACK_CONFIG_DIR` overrides. The same folder the app uses
/// (`appearance::config_dir`).
pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("AITRACK_CONFIG_DIR") {
        return p.into();
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("AI-Track")
}

/// The user's choices. Off by default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
    /// Apply external edits at once (each tool call is one undo step) instead
    /// of offering them as a proposal. Destructive edits are always offered.
    pub apply_directly: bool,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self { enabled: false, port: DEFAULT_PORT, apply_directly: false }
    }
}

impl McpSettings {
    fn file(dir: &Path) -> PathBuf {
        dir.join("mcp.json")
    }

    /// Read the settings; a missing or broken file means the defaults.
    pub fn load_from(dir: &Path) -> Self {
        std::fs::read_to_string(Self::file(dir)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::load_from(&config_dir())
    }

    pub fn save_to(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(Self::file(dir), text)
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&config_dir())
    }
}

fn token_file(dir: &Path) -> PathBuf {
    dir.join("mcp-token")
}

/// A new token: 32 random bytes, hex, behind a readable prefix.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    let mut out = String::from("aitrack_");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The saved token, if there is one.
pub fn read_token_from(dir: &Path) -> Option<String> {
    let t = std::fs::read_to_string(token_file(dir)).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

pub fn read_token() -> Option<String> {
    read_token_from(&config_dir())
}

/// Save `token`, readable by the user only where the OS has such a thing.
pub fn write_token_to(dir: &Path, token: &str) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = token_file(dir);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // Written to a private file and moved over, so it is never world-readable.
        let tmp = dir.join("mcp-token.tmp");
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    }
    #[cfg(not(unix))]
    {
        // %APPDATA% is private to the user already.
        std::fs::write(&path, token)
    }
}

/// The saved token, made and saved first if this install has none.
pub fn ensure_token_in(dir: &Path) -> io::Result<String> {
    if let Some(t) = read_token_from(dir) {
        return Ok(t);
    }
    let t = new_token();
    write_token_to(dir, &t)?;
    Ok(t)
}

pub fn ensure_token() -> io::Result<String> {
    ensure_token_in(&config_dir())
}

/// Replace the token (old clients stop working).
pub fn regenerate_token_in(dir: &Path) -> io::Result<String> {
    let t = new_token();
    write_token_to(dir, &t)?;
    Ok(t)
}

pub fn regenerate_token() -> io::Result<String> {
    regenerate_token_in(&config_dir())
}

/// `http://127.0.0.1:<port>/mcp`.
pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{PATH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitrack-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn off_by_default_and_round_trips() {
        let d = dir("settings");
        assert!(!McpSettings::load_from(&d).enabled);
        let s = McpSettings { enabled: true, port: 5000, apply_directly: true };
        s.save_to(&d).unwrap();
        assert_eq!(McpSettings::load_from(&d), s);
        // Old files with fewer fields keep working.
        std::fs::write(d.join("mcp.json"), "{\"enabled\":true}").unwrap();
        assert_eq!(McpSettings::load_from(&d), McpSettings { enabled: true, ..Default::default() });
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_token_is_made_once_and_can_be_replaced() {
        let d = dir("token");
        assert_eq!(read_token_from(&d), None);
        let a = ensure_token_in(&d).unwrap();
        assert!(a.starts_with("aitrack_") && a.len() == 8 + 64);
        assert_eq!(ensure_token_in(&d).unwrap(), a);
        let b = regenerate_token_in(&d).unwrap();
        assert_ne!(a, b);
        assert_eq!(read_token_from(&d).as_deref(), Some(b.as_str()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("mcp-token")).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only the user can read the token");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
