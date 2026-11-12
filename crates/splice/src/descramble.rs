//! Splice's preview MP3s arrive lightly obfuscated: a 28-byte header holds
//! a length `n` (little-endian, bytes 2..10) and an 18-byte key (bytes
//! 10..28). After the header, bytes `0..n` and `2n..3n` of the audio are
//! XORed with the repeating key. The web player undoes this to play the
//! preview; so do we, in memory, only to audition it. Previews are never
//! added to the library.

const HEADER: usize = 28;
const KEY: std::ops::Range<usize> = 10..28;

fn xor_range(data: &mut [u8], from: usize, len: usize, key: &[u8]) {
    let end = from.saturating_add(len).min(data.len());
    if from >= end {
        return;
    }
    for (b, k) in data[from..end].iter_mut().zip(key.iter().cycle()) {
        *b ^= k;
    }
}

/// The playable MP3 inside a preview download, or `None` when it is too
/// short to carry the header.
pub fn descramble(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < HEADER {
        return None;
    }
    let mut n = [0u8; 8];
    n.copy_from_slice(&data[2..10]);
    let n = usize::try_from(u64::from_le_bytes(n)).unwrap_or(usize::MAX);
    let key = &data[KEY];
    let mut audio = data[HEADER..].to_vec();
    xor_range(&mut audio, 0, n, key);
    xor_range(&mut audio, n.saturating_mul(2), n, key);
    Some(audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inverse of [`descramble`], to build test inputs.
    fn scramble(audio: &[u8], n: u64, key: &[u8; 18]) -> Vec<u8> {
        let mut body = audio.to_vec();
        xor_range(&mut body, 0, n as usize, key);
        xor_range(&mut body, 2 * n as usize, n as usize, key);
        let mut out = vec![0u8, 0];
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(key);
        out.extend(body);
        out
    }

    #[test]
    fn round_trips_both_ranges() {
        let audio: Vec<u8> = (0..100u8).collect();
        let key = *b"0123456789abcdefgh";
        let s = scramble(&audio, 20, &key);
        assert_ne!(&s[HEADER..HEADER + 20], &audio[..20]);
        assert_eq!(&s[HEADER + 20..HEADER + 40], &audio[20..40], "the middle range is clear");
        assert_eq!(descramble(&s).unwrap(), audio);
    }

    #[test]
    fn length_past_the_end_is_clamped() {
        let audio = vec![7u8; 10];
        let key = [1u8; 18];
        let s = scramble(&audio, 1 << 40, &key);
        assert_eq!(descramble(&s).unwrap(), audio);
        assert!(descramble(&[0; 5]).is_none());
    }
}
