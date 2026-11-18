//! The minisign public key the app trusts, embedded at build time by `build.rs`
//! from `release/updater.pub`. Without that file the placeholder
//! `release/updater.pub.example` is embedded and the build is *unconfigured*:
//! checking works, installing is refused.

const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/updater.pub"));

/// Whether a real `release/updater.pub` was present at build time.
pub const CONFIGURED: bool = !cfg!(updater_unconfigured);

/// The trusted public key (the text of the `.pub` file), or why there is none.
pub fn public_key() -> Result<&'static str, crate::UpdateError> {
    if CONFIGURED { Ok(EMBEDDED) } else { Err(crate::UpdateError::Unconfigured) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_never_counts_as_a_key() {
        // Whatever was embedded, `public_key` agrees with `CONFIGURED`.
        assert_eq!(public_key().is_ok(), CONFIGURED);
        if CONFIGURED {
            assert!(minisign_verify::PublicKey::decode(EMBEDDED).is_ok(), "release/updater.pub is not a minisign public key");
        } else {
            assert!(EMBEDDED.contains("PLACEHOLDER"));
        }
    }
}
