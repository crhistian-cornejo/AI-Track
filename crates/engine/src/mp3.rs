//! MP3 export through a dynamically loaded LAME.
//!
//! **Licensing.** LAME is LGPL-2.0+. AI-Track is MIT and its dependency
//! policy forbids linking copyleft code, so LAME is never linked at build
//! time: `libmp3lame` is opened at runtime with `libloading` (ISC), like the
//! ONNX runtime. That is the LGPL's "use a shared library" case: the user
//! can replace the library, we ship no LAME code in our binaries, and when
//! the app downloads or bundles the library it must carry LAME's licence
//! text and a link to its source (see NOTICE). Pure-Rust alternatives were
//! considered: `shine-rs` is an LGPL port (same problem, lower quality) and
//! `rusty_mp3` (Apache-2.0) is too new to trust at 320 kbps.
//!
//! Output is always 320 kbps CBR, joint stereo, highest quality algorithm
//! (`quality = 2`), with an ID3v2.3 tag (title, artist, BPM, key).

use crate::EngineError;
use aitrack_dsp::AudioBuffer;
use std::ffi::c_void;
use std::os::raw::{c_float, c_int, c_uchar};
use std::path::{Path, PathBuf};

/// Metadata written to the ID3v2 tag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mp3Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub bpm: Option<f64>,
    /// Musical key, e.g. "Am" or "F#".
    pub key: Option<String>,
}

type LameT = *mut c_void;

/// Function table of a loaded `libmp3lame`.
pub struct Lame {
    _lib: libloading::Library,
    init: unsafe extern "C" fn() -> LameT,
    set_in_samplerate: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_out_samplerate: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_num_channels: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_brate: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_mode: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_vbr: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_quality: unsafe extern "C" fn(LameT, c_int) -> c_int,
    set_write_vbr_tag: unsafe extern "C" fn(LameT, c_int) -> c_int,
    init_params: unsafe extern "C" fn(LameT) -> c_int,
    encode_float: unsafe extern "C" fn(LameT, *const c_float, *const c_float, c_int, *mut c_uchar, c_int) -> c_int,
    flush: unsafe extern "C" fn(LameT, *mut c_uchar, c_int) -> c_int,
    close: unsafe extern "C" fn(LameT) -> c_int,
}

/// `MPEG_mode::JOINT_STEREO` and `vbr_mode::vbr_off` from `lame.h`.
const JOINT_STEREO: c_int = 1;
const VBR_OFF: c_int = 0;

/// File names to try for the system or downloaded library.
pub fn library_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["libmp3lame.dll", "lame_enc.dll", "mp3lame.dll"]
    } else if cfg!(target_os = "macos") {
        &["libmp3lame.dylib", "libmp3lame.0.dylib"]
    } else {
        &["libmp3lame.so.0", "libmp3lame.so"]
    }
}

impl Lame {
    /// Load LAME from an explicit file, or (with `None`) from the given
    /// directories and then the system search path.
    pub fn load(file: Option<&Path>, search_dirs: &[PathBuf]) -> Result<Self, EngineError> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(f) = file {
            candidates.push(f.to_path_buf());
        } else {
            for d in search_dirs {
                for n in library_names() {
                    candidates.push(d.join(n));
                }
            }
            for n in library_names() {
                candidates.push(PathBuf::from(n));
            }
        }
        let mut last = String::from("no candidates");
        for c in candidates {
            // SAFETY: loading a library runs its initialisers; LAME has none
            // with side effects beyond its own state.
            match unsafe { libloading::Library::new(&c) } {
                Ok(lib) => return Self::from_library(lib),
                Err(e) => last = format!("{}: {e}", c.display()),
            }
        }
        Err(EngineError::Export(format!(
            "LAME (libmp3lame) is not installed; MP3 export needs it at runtime ({last})"
        )))
    }

    fn from_library(lib: libloading::Library) -> Result<Self, EngineError> {
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the signatures match lame.h (3.99.5+).
                *unsafe { lib.get($name) }.map_err(|e| {
                    EngineError::Export(format!("libmp3lame lacks {}: {e}", String::from_utf8_lossy($name)))
                })?
            };
        }
        Ok(Self {
            init: sym!(b"lame_init"),
            set_in_samplerate: sym!(b"lame_set_in_samplerate"),
            set_out_samplerate: sym!(b"lame_set_out_samplerate"),
            set_num_channels: sym!(b"lame_set_num_channels"),
            set_brate: sym!(b"lame_set_brate"),
            set_mode: sym!(b"lame_set_mode"),
            set_vbr: sym!(b"lame_set_VBR"),
            set_quality: sym!(b"lame_set_quality"),
            set_write_vbr_tag: sym!(b"lame_set_bWriteVbrTag"),
            init_params: sym!(b"lame_init_params"),
            encode_float: sym!(b"lame_encode_buffer_ieee_float"),
            flush: sym!(b"lame_encode_flush"),
            close: sym!(b"lame_close"),
            _lib: lib,
        })
    }

    /// Encode stereo audio (`[-1, 1]`) to a complete MP3 file in memory.
    pub fn encode_320_cbr(&self, audio: &AudioBuffer, sample_rate: u32, tags: &Mp3Tags) -> Result<Vec<u8>, EngineError> {
        let err = |what: &str| EngineError::Export(format!("LAME: {what} failed"));
        // SAFETY: calls follow LAME's documented lifecycle; buffers are sized
        // per lame.h (1.25 × samples + 7200).
        unsafe {
            let gf = (self.init)();
            if gf.is_null() {
                return Err(err("lame_init"));
            }
            let ok = (self.set_in_samplerate)(gf, sample_rate as c_int) >= 0
                && (self.set_out_samplerate)(gf, sample_rate as c_int) >= 0
                && (self.set_num_channels)(gf, 2) >= 0
                && (self.set_vbr)(gf, VBR_OFF) >= 0
                && (self.set_brate)(gf, 320) >= 0
                && (self.set_mode)(gf, JOINT_STEREO) >= 0
                && (self.set_quality)(gf, 2) >= 0
                && (self.set_write_vbr_tag)(gf, 0) >= 0
                && (self.init_params)(gf) >= 0;
            if !ok {
                (self.close)(gf);
                return Err(err("configuration"));
            }
            let mut out = id3v2_tag(tags);
            let l = audio.channel(0);
            let r = audio.channel(1.min(audio.channels() - 1));
            let chunk = 8192usize;
            let mut mp3 = vec![0u8; chunk * 5 / 4 + 7200];
            for start in (0..l.len()).step_by(chunk) {
                let n = chunk.min(l.len() - start);
                let written =
                    (self.encode_float)(gf, l[start..].as_ptr(), r[start..].as_ptr(), n as c_int, mp3.as_mut_ptr(), mp3.len() as c_int);
                if written < 0 {
                    (self.close)(gf);
                    return Err(err("encoding"));
                }
                out.extend_from_slice(&mp3[..written as usize]);
            }
            let written = (self.flush)(gf, mp3.as_mut_ptr(), mp3.len() as c_int);
            if written > 0 {
                out.extend_from_slice(&mp3[..written as usize]);
            }
            (self.close)(gf);
            Ok(out)
        }
    }
}

fn syncsafe(n: u32) -> [u8; 4] {
    [((n >> 21) & 0x7F) as u8, ((n >> 14) & 0x7F) as u8, ((n >> 7) & 0x7F) as u8, (n & 0x7F) as u8]
}

/// ID3v2.3 text frame, UTF-16 with BOM.
fn text_frame(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut body = vec![1u8, 0xFF, 0xFE];
    for u in text.encode_utf16() {
        body.extend_from_slice(&u.to_le_bytes());
    }
    body.extend_from_slice(&[0, 0]);
    let mut f = Vec::with_capacity(10 + body.len());
    f.extend_from_slice(id);
    f.extend_from_slice(&(body.len() as u32).to_be_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&body);
    f
}

/// Build an ID3v2.3 tag (empty `Vec` if there is nothing to write except
/// the encoder name).
pub fn id3v2_tag(tags: &Mp3Tags) -> Vec<u8> {
    let mut frames = Vec::new();
    if let Some(t) = &tags.title {
        frames.extend(text_frame(b"TIT2", t));
    }
    if let Some(a) = &tags.artist {
        frames.extend(text_frame(b"TPE1", a));
    }
    if let Some(b) = tags.bpm {
        frames.extend(text_frame(b"TBPM", &format!("{}", b.round() as i64)));
    }
    if let Some(k) = &tags.key {
        frames.extend(text_frame(b"TKEY", k));
    }
    frames.extend(text_frame(b"TSSE", "AI-Track"));
    let mut tag = Vec::with_capacity(10 + frames.len());
    tag.extend_from_slice(b"ID3");
    tag.extend_from_slice(&[3, 0, 0]);
    tag.extend_from_slice(&syncsafe(frames.len() as u32));
    tag.extend_from_slice(&frames);
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id3_tag_layout() {
        let tag = id3v2_tag(&Mp3Tags { title: Some("Song".into()), artist: Some("Me".into()), bpm: Some(127.6), key: Some("Am".into()) });
        assert_eq!(&tag[..5], b"ID3\x03\x00");
        let size = (tag[6] as usize) << 21 | (tag[7] as usize) << 14 | (tag[8] as usize) << 7 | tag[9] as usize;
        assert_eq!(size, tag.len() - 10);
        assert_eq!(&tag[10..14], b"TIT2");
        let find = |id: &[u8]| tag.windows(4).position(|w| w == id).is_some();
        assert!(find(b"TPE1") && find(b"TBPM") && find(b"TKEY"));
        // BPM text "128" as UTF-16LE.
        let bpm: Vec<u8> = "128".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert!(tag.windows(bpm.len()).any(|w| w == bpm.as_slice()));
    }

    #[test]
    fn missing_library_is_a_clear_error() {
        let e = Lame::load(Some(Path::new("definitely-not-here/libmp3lame.bin")), &[]).err().unwrap();
        assert!(e.to_string().contains("LAME"));
    }
}
