//! Is this file the one the release published? Size, sha256 and the minisign
//! signature, in one streaming pass (the app archive is large).

use crate::{Asset, UpdateError};
use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// `path` is exactly `asset`: right size, right sha256, signed by `public_key`
/// (the text of a minisign `.pub` file). Signatures must be pre-hashed, which is
/// what `minisign -S` does by default.
pub fn verify_file(path: &Path, asset: &Asset, public_key: &str) -> Result<(), UpdateError> {
    let key = PublicKey::decode(public_key).map_err(|e| UpdateError::BadPublicKey(e.to_string()))?;
    let signature = Signature::decode(&asset.signature).map_err(|e| UpdateError::BadSignature(e.to_string()))?;
    let mut stream = key.verify_stream(&signature).map_err(|e| UpdateError::BadSignature(e.to_string()))?;
    let io = |e: std::io::Error| UpdateError::io(format!("reading {}", path.display()), e);
    let mut file = File::open(path).map_err(io)?;
    let got = file.metadata().map_err(io)?.len();
    if got != asset.size {
        return Err(UpdateError::SizeMismatch { expected: asset.size, got });
    }
    let mut sha = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        stream.update(&buf[..n]);
    }
    if !hex(&sha.finalize()).eq_ignore_ascii_case(asset.sha256.trim()) {
        return Err(UpdateError::HashMismatch);
    }
    stream.finalize().map_err(|_| UpdateError::SignatureInvalid)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{Signed, signed, temp_dir};

    #[test]
    fn a_good_file_verifies() {
        let dir = temp_dir("verify-ok");
        let Signed { path, asset, public_key } = signed(&dir, b"the new app");
        assert_eq!(verify_file(&path, &asset, &public_key), Ok(()));
        // Uppercase hex from a manifest is the same checksum.
        let upper = Asset { sha256: asset.sha256.to_uppercase(), ..asset };
        assert_eq!(verify_file(&path, &upper, &public_key), Ok(()));
    }

    #[test]
    fn a_wrong_size_is_rejected_first() {
        let dir = temp_dir("verify-size");
        let Signed { path, asset, public_key } = signed(&dir, b"the new app");
        let bad = Asset { size: asset.size + 1, ..asset };
        assert_eq!(verify_file(&path, &bad, &public_key), Err(UpdateError::SizeMismatch { expected: 12, got: 11 }));
    }

    #[test]
    fn a_wrong_checksum_is_rejected() {
        let dir = temp_dir("verify-sha");
        let Signed { path, asset, public_key } = signed(&dir, b"the new app");
        let bad = Asset { sha256: "00".repeat(32), ..asset };
        assert_eq!(verify_file(&path, &bad, &public_key), Err(UpdateError::HashMismatch));
    }

    #[test]
    fn a_tampered_file_fails_the_signature_even_with_a_matching_checksum() {
        let dir = temp_dir("verify-tamper");
        let Signed { path, asset, public_key } = signed(&dir, b"the new app");
        // An attacker who controls the manifest can fix size and sha256 but cannot sign.
        let evil = dir.join("evil");
        std::fs::write(&evil, b"the new apX").unwrap();
        let forged = Asset { sha256: hex(&Sha256::digest(b"the new apX")), ..asset };
        assert_eq!(verify_file(&evil, &forged, &public_key), Err(UpdateError::SignatureInvalid));
        assert!(path.exists());
    }

    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let dir = temp_dir("verify-key");
        let Signed { path, asset, .. } = signed(&dir, b"the new app");
        let other = signed(&temp_dir("verify-key-2"), b"x");
        let err = verify_file(&path, &asset, &other.public_key).unwrap_err();
        assert!(matches!(err, UpdateError::BadSignature(_)), "{err:?}");
    }

    #[test]
    fn garbage_keys_and_signatures_are_errors_not_panics() {
        let dir = temp_dir("verify-garbage");
        let Signed { path, asset, public_key } = signed(&dir, b"the new app");
        assert!(matches!(verify_file(&path, &asset, "nonsense"), Err(UpdateError::BadPublicKey(_))));
        let bad = Asset { signature: "nonsense".into(), ..asset };
        assert!(matches!(verify_file(&path, &bad, &public_key), Err(UpdateError::BadSignature(_))));
        assert!(matches!(verify_file(&dir.join("missing"), &bad, &public_key), Err(UpdateError::BadSignature(_))));
    }
}
