//! Where the Anthropic API key comes from: the `ANTHROPIC_API_KEY`
//! environment variable first, else the OS credential store (the macOS
//! Keychain or the Windows Credential Manager, through `keyring`). The key
//! is never logged, passed on a command line, or written to the project or
//! the preferences.

/// Credential service and account the key is stored under.
pub const SERVICE: &str = "dev.aitrack.assistant";
pub const ACCOUNT: &str = "anthropic-api-key";
/// Where earlier builds put the key (macOS only, through `/usr/bin/security`).
const LEGACY_SERVICE: &str = "AI-Track";

/// The API key, if one is configured. Talks to the credential store: call it
/// off the UI thread.
pub fn api_key() -> Option<String> {
    if let Some(k) = std::env::var("ANTHROPIC_API_KEY").ok().map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()) {
        return Some(k);
    }
    read()
}

/// Whether `key` looks like an Anthropic API key (cheap sanity check before
/// storing whatever was on the clipboard).
pub fn looks_like_key(key: &str) -> bool {
    let k = key.trim();
    k.starts_with("sk-ant-") && k.len() >= 24 && !k.chars().any(char::is_whitespace)
}

fn entry(service: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(service, ACCOUNT).ok()
}

fn get(service: &str) -> Option<String> {
    let k = entry(service)?.get_password().ok()?.trim().to_owned();
    (!k.is_empty()).then_some(k)
}

fn read() -> Option<String> {
    if let Some(k) = get(SERVICE) {
        return Some(k);
    }
    // Move a key stored by an earlier build to the new service.
    let k = get(LEGACY_SERVICE)?;
    if store(&k).is_ok()
        && let Some(old) = entry(LEGACY_SERVICE)
    {
        let _ = old.delete_credential();
    }
    Some(k)
}

/// Store `key` in the credential store, replacing an earlier one.
pub fn store(key: &str) -> Result<(), String> {
    if !looks_like_key(key) {
        return Err("That does not look like an Anthropic API key (sk-ant-…).".into());
    }
    let e = entry(SERVICE).ok_or("The system keychain is not available.")?;
    e.set_password(key.trim()).map_err(|e| format!("The keychain refused the key: {e}"))
}

/// Remove the stored key.
pub fn forget() {
    for service in [SERVICE, LEGACY_SERVICE] {
        if let Some(e) = entry(service) {
            let _ = e.delete_credential();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn key_shape() {
        assert!(super::looks_like_key("sk-ant-api03-abcdefghijklmnopqrstuvwxyz"));
        assert!(!super::looks_like_key("hello world"));
        assert!(!super::looks_like_key("sk-ant-short"));
    }
}
