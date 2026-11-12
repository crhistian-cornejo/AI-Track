//! Blocking HTTP calls. Never call these on the UI or audio thread; the
//! [`crate::Service`] runs them on its own threads.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{SearchPage, SpliceSample, parse_search, waveform_bars};
use crate::query::{GRAPHQL_URL, SearchParams, request_body};
use crate::{Error, Result};

/// Previews are short MP3s; anything bigger is not one.
const MAX_PREVIEW: u64 = 16 << 20;
const MAX_IMAGE: u64 = 4 << 20;
const MAX_JSON: u64 = 8 << 20;

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent("AI-Track")
        .http_status_as_error(false)
        .build()
        .into()
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

pub struct Client {
    agent: ureq::Agent,
    token: Option<String>,
}

impl Client {
    pub fn new(token: Option<String>) -> Self {
        Self { agent: agent(), token }
    }

    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    pub fn search(&self, p: &SearchParams) -> Result<SearchPage> {
        let mut req = self.agent.post(GRAPHQL_URL).header("Content-Type", "application/json");
        if let Some(t) = &self.token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        let mut resp = req.send(request_body(p).to_string()).map_err(net)?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().with_config().limit(MAX_JSON).read_to_string().map_err(net)?;
        if status >= 400 && !body.trim_start().starts_with('{') {
            return Err(Error::Network(format!("Splice answered HTTP {status}")));
        }
        parse_search(&body)
    }

    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>> {
        if !url.starts_with("https://") {
            return Err(Error::Network("only https downloads".into()));
        }
        let mut resp = self.agent.get(url).call().map_err(net)?;
        let status = resp.status().as_u16();
        if status >= 400 {
            return Err(Error::Network(format!("HTTP {status}")));
        }
        resp.body_mut().with_config().limit(max).read_to_vec().map_err(net)
    }

    /// Fetch, descramble and cache a preview MP3 for auditioning:
    /// `<cache>/previews/<uuid>.mp3`. The cache is the app's, never the library.
    pub fn preview(&self, s: &SpliceSample, cache: &Path) -> Result<PathBuf> {
        let path = cache.join("previews").join(format!("{}.mp3", safe(&s.uuid)));
        if path.is_file() {
            return Ok(path);
        }
        let url = s.preview_url.as_deref().ok_or_else(|| Error::Network("this sample has no preview".into()))?;
        let raw = self.get(url, MAX_PREVIEW)?;
        let audio = crate::descramble::descramble(&raw).ok_or_else(|| Error::Parse("the preview is too short".into()))?;
        write_atomic(&path, &audio)?;
        Ok(path)
    }

    /// A pack cover, cached as `<cache>/covers/<hash>.<ext>`.
    pub fn cover(&self, url: &str, cache: &Path) -> Result<PathBuf> {
        let ext = if url.to_lowercase().contains(".png") { "png" } else { "jpg" };
        let path = cache.join("covers").join(format!("{:016x}.{ext}", fnv(url)));
        if path.is_file() {
            return Ok(path);
        }
        let bytes = self.get(url, MAX_IMAGE)?;
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    /// Waveform outline as `n` bars in 0..=1.
    pub fn waveform(&self, url: &str, n: usize) -> Result<Vec<f32>> {
        let bytes = self.get(url, MAX_JSON)?;
        Ok(waveform_bars(&parse_waveform(&bytes)?, n))
    }
}

/// A waveform body: a JSON number array, plain or zlib/deflate-compressed.
pub fn parse_waveform(bytes: &[u8]) -> Result<Vec<f64>> {
    if let Ok(v) = serde_json::from_slice::<Vec<f64>>(bytes) {
        return Ok(v);
    }
    let text = inflate(bytes).ok_or_else(|| Error::Parse("unreadable waveform".into()))?;
    serde_json::from_slice::<Vec<f64>>(&text).map_err(|e| Error::Parse(e.to_string()))
}

fn inflate(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(bytes).take(MAX_JSON).read_to_end(&mut out).is_ok() {
        return Some(out);
    }
    out.clear();
    flate2::read::DeflateDecoder::new(bytes).take(MAX_JSON).read_to_end(&mut out).ok().map(|_| out)
}

/// File-name-safe id.
fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let io = |e: std::io::Error| Error::Io(e.to_string());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn waveform_plain_and_compressed() {
        assert_eq!(parse_waveform(b"[1, 2.5, 3]").unwrap(), vec![1., 2.5, 3.]);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(b"[4,5]").unwrap();
        assert_eq!(parse_waveform(&z.finish().unwrap()).unwrap(), vec![4., 5.]);
        assert!(parse_waveform(b"\x00\x01garbage").is_err());
    }

    #[test]
    fn cache_names_are_safe() {
        assert_eq!(safe("../a b"), "___a_b");
        assert_ne!(fnv("a"), fnv("b"));
    }
}
