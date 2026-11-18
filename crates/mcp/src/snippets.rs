//! The config text each client needs, for the "Copy config" buttons.
//! Claude Desktop uses the stdio shim, which reads the port and token itself,
//! so its snippet holds no secret. The others talk HTTP and carry the token.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::settings;

/// Where the `aitrack-mcp` shim sits: next to the running app.
pub fn shim_path() -> PathBuf {
    let name = if cfg!(windows) { "aitrack-mcp.exe" } else { "aitrack-mcp" };
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name))).unwrap_or_else(|| PathBuf::from(name))
}

/// `claude_desktop_config.json`: a stdio server running the shim.
pub fn claude_desktop(shim: &Path) -> String {
    serde_json::to_string_pretty(&json!({ "mcpServers": { "aitrack": { "command": shim.to_string_lossy(), "args": [] } } })).unwrap_or_default()
}

/// Claude Code: add the server over HTTP.
pub fn claude_code(port: u16, token: &str) -> String {
    format!("claude mcp add --transport http aitrack {} --header \"Authorization: Bearer {token}\"", settings::url(port))
}

/// Claude Code over the stdio shim (no token in the command).
pub fn claude_code_stdio(shim: &Path) -> String {
    let p = shim.to_string_lossy();
    if p.contains(' ') { format!("claude mcp add aitrack -- \"{p}\"") } else { format!("claude mcp add aitrack -- {p}") }
}

/// Cursor's `mcp.json`: a URL with the token header.
pub fn cursor(port: u16, token: &str) -> String {
    serde_json::to_string_pretty(&json!({ "mcpServers": { "aitrack": { "url": settings::url(port), "headers": { "Authorization": format!("Bearer {token}") } } } })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn desktop_config_is_valid_json_with_the_shim_and_no_token() {
        let s = claude_desktop(Path::new("C:\\Program Files\\AI-Track\\aitrack-mcp.exe"));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["mcpServers"]["aitrack"]["command"], "C:\\Program Files\\AI-Track\\aitrack-mcp.exe");
        assert!(!s.contains("aitrack_"), "no secret in it");
    }

    #[test]
    fn http_configs_carry_the_url_and_token() {
        let c = claude_code(47615, "aitrack_abc");
        assert!(c.starts_with("claude mcp add --transport http aitrack http://127.0.0.1:47615/mcp"));
        assert!(c.contains("Bearer aitrack_abc"));
        let v: Value = serde_json::from_str(&cursor(1234, "aitrack_abc")).unwrap();
        assert_eq!(v["mcpServers"]["aitrack"]["url"], "http://127.0.0.1:1234/mcp");
        assert_eq!(v["mcpServers"]["aitrack"]["headers"]["Authorization"], "Bearer aitrack_abc");
        assert_eq!(claude_code_stdio(Path::new("/Applications/AI Track/aitrack-mcp")), "claude mcp add aitrack -- \"/Applications/AI Track/aitrack-mcp\"");
    }
}
