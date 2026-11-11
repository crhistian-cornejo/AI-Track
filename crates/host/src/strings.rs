//! String interning for the `&'static str` fields of `aitrack_dsp::PluginInfo`
//! and `ParamInfo`. Hosted plugins have dynamic names; interning keeps the
//! leak bounded to the set of distinct strings ever seen.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();

pub(crate) fn intern(s: &str) -> &'static str {
    if s.is_empty() {
        return "";
    }
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut pool = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = pool.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// UTF-16, NUL-terminated (VST3 `String128`) to `String`.
pub(crate) fn from_utf16(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// NUL-terminated C chars (VST3 `char8` arrays) to `String`.
pub(crate) fn from_c_chars(buf: &[std::ffi::c_char]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let bytes: Vec<u8> = buf[..len].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Bytes possibly NUL-terminated (CLAP name buffers) to `String`.
pub(crate) fn from_bytes(buf: &[u8]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// Copy `src` into a UTF-16 buffer, NUL-terminated, truncating if needed.
pub(crate) fn to_utf16(src: &str, dst: &mut [u16]) {
    if dst.is_empty() {
        return;
    }
    let mut n = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s;
        n += 1;
    }
    let end = n.min(dst.len() - 1);
    dst[end] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_dedupes() {
        let a = intern("Gain");
        let b = intern(&String::from("Gain"));
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn utf16_round_trip() {
        let mut buf = [0u16; 8];
        to_utf16("Cutoff Frequency", &mut buf);
        assert_eq!(from_utf16(&buf), "Cutoff ");
    }
}
