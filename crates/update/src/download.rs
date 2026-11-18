//! Downloading the artifact: resumable, and trusted only after size, sha256 and
//! signature check out. The file lives in `<root>/<version>/`: `name.part`
//! while it is coming, `name` once verified.

use crate::manifest::{agent, check_url};
use crate::verify::verify_file;
use crate::{Update, UpdateError};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// The file name the artifact is saved under: the URL's last segment, made safe.
pub fn file_name(url: &str) -> String {
    let last = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("");
    let safe: String = last.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    let safe = safe.trim_start_matches('.').to_owned();
    if safe.is_empty() { "update.bin".to_owned() } else { safe }
}

/// Where the verified artifact of `update` ends up under `root`.
pub fn downloaded_path(root: &Path, update: &Update) -> PathBuf {
    root.join(update.version.to_string()).join(file_name(&update.asset.url))
}

/// Download `update` into `root/<version>/` and verify it against `public_key`.
/// A cut download is resumed from its `.part` file on the next call; one that
/// fails verification is deleted. `progress` is called as bytes arrive.
pub fn download(update: &Update, root: &Path, public_key: &str, cancel: &AtomicBool, progress: &mut dyn FnMut(Progress)) -> Result<PathBuf, UpdateError> {
    check_url(&update.asset.url)?;
    let done = downloaded_path(root, update);
    let dir = done.parent().expect("a file under the version folder");
    fs::create_dir_all(dir).map_err(|e| UpdateError::io(format!("creating {}", dir.display()), e))?;
    // Downloaded and verified on an earlier run: nothing to fetch.
    if done.is_file() {
        if verify_file(&done, &update.asset, public_key).is_ok() {
            progress(Progress { done: update.asset.size, total: update.asset.size });
            return Ok(done);
        }
        let _ = fs::remove_file(&done);
    }
    let part = done.with_file_name(format!("{}.part", file_name(&update.asset.url)));
    match fetch_into(update, &part, cancel, progress) {
        // The partial file is not a prefix of what the server has any more: start over.
        Err(UpdateError::Http(416)) => {
            let _ = fs::remove_file(&part);
            fetch_into(update, &part, cancel, progress)?;
        }
        other => other?,
    }
    if let Err(e) = verify_file(&part, &update.asset, public_key) {
        // Corrupt or forged: never keep it to resume from.
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &done).map_err(|e| UpdateError::io(format!("moving {}", part.display()), e))?;
    Ok(done)
}

/// Append what is missing of the artifact to `part`.
fn fetch_into(update: &Update, part: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(Progress)) -> Result<(), UpdateError> {
    let total = update.asset.size;
    let mut have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if have > total {
        let _ = fs::remove_file(part);
        have = 0;
    }
    progress(Progress { done: have, total });
    if have == total {
        return Ok(());
    }
    let mut request = agent().get(&update.asset.url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let resp = request.call().map_err(|e| UpdateError::Network(e.to_string()))?;
    let io = |e: std::io::Error| UpdateError::io(format!("writing {}", part.display()), e);
    let mut out = match resp.status().as_u16() {
        200 => {
            have = 0;
            File::create(part).map_err(io)?
        }
        206 => {
            let from_here = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).is_some_and(|v| v.trim().starts_with(&format!("bytes {have}-")));
            if !from_here {
                return Err(UpdateError::Network("the server resumed from the wrong place".into()));
            }
            OpenOptions::new().append(true).open(part).map_err(io)?
        }
        other => return Err(UpdateError::Http(other)),
    };
    let mut body = resp.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let (mut got, mut reported) = (have, have);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Cancelled);
        }
        let n = body.read(&mut buf).map_err(|e| UpdateError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        got += n as u64;
        if got > total {
            drop(out);
            let _ = fs::remove_file(part);
            return Err(UpdateError::SizeMismatch { expected: total, got });
        }
        out.write_all(&buf[..n]).map_err(io)?;
        if got - reported >= 256 * 1024 {
            reported = got;
            progress(Progress { done: got, total });
        }
    }
    out.flush().map_err(io)?;
    progress(Progress { done: got, total });
    if got < total {
        // Kept: the next attempt resumes from here.
        return Err(UpdateError::Network("the connection closed before the download finished".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{Signed, serve, signed, temp_dir};
    use crate::Version;

    fn update_for(asset: crate::Asset, url: &str) -> Update {
        Update { version: Version::parse("0.0.2").unwrap(), notes: String::new(), pub_date: None, asset: crate::Asset { url: url.to_owned(), ..asset } }
    }

    fn body() -> Vec<u8> {
        (0..900_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect()
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("https://x/y/AI-Track-0.0.2-macos-universal.app.tar.gz"), "AI-Track-0.0.2-macos-universal.app.tar.gz");
        assert_eq!(file_name("https://x/y/a b.exe?token=1"), "a_b.exe");
        assert_eq!(file_name("https://x/y/.."), "update.bin");
        assert_eq!(file_name("https://x/"), "update.bin");
    }

    #[test]
    fn downloads_verifies_and_keeps_the_file() {
        let dir = temp_dir("dl-ok");
        let data = body();
        let Signed { asset, public_key, .. } = signed(&dir, &data);
        let server = serve(data.clone(), None);
        let update = update_for(asset, &server.url);
        let root = dir.join("updates");
        let mut last = Progress { done: 0, total: 0 };
        let path = download(&update, &root, &public_key, &AtomicBool::new(false), &mut |p| last = p).unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
        assert_eq!(path, root.join("0.0.2").join("artifact"));
        assert_eq!(last, Progress { done: data.len() as u64, total: data.len() as u64 });
        assert!(!path.with_extension("part").exists());
        // Asking again finds it verified and does not touch the network.
        let before = server.requests.lock().unwrap().len();
        download(&update, &root, &public_key, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(server.requests.lock().unwrap().len(), before);
    }

    #[test]
    fn a_cut_download_resumes_from_the_partial_file() {
        let dir = temp_dir("dl-resume");
        let data = body();
        let Signed { asset, public_key, .. } = signed(&dir, &data);
        let server = serve(data.clone(), Some(300_000));
        let update = update_for(asset, &server.url);
        let root = dir.join("updates");
        let first = download(&update, &root, &public_key, &AtomicBool::new(false), &mut |_| {});
        assert!(first.is_err(), "the first connection was cut");
        let part = root.join("0.0.2").join("artifact.part");
        let kept = fs::metadata(&part).unwrap().len();
        assert!(kept > 0 && kept < data.len() as u64, "{kept}");
        let path = download(&update, &root, &public_key, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
        let seen = server.requests.lock().unwrap().clone();
        assert_eq!(seen, vec![None, Some(kept)]);
        assert!(!part.exists());
    }

    #[test]
    fn a_forged_download_is_deleted_and_reported() {
        let dir = temp_dir("dl-forged");
        let data = body();
        let Signed { asset, public_key, .. } = signed(&dir, &data);
        // The server sends different bytes of the same size, with the right headers.
        let mut evil = data.clone();
        evil[1000] ^= 0xff;
        let server = serve(evil, None);
        let update = update_for(asset, &server.url);
        let root = dir.join("updates");
        let err = download(&update, &root, &public_key, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert_eq!(err, UpdateError::HashMismatch);
        let version_dir = root.join("0.0.2");
        assert!(fs::read_dir(&version_dir).unwrap().next().is_none(), "nothing is left behind");
    }

    #[test]
    fn a_wrong_size_is_rejected_and_cleaned_up() {
        let dir = temp_dir("dl-size");
        let data = body();
        let Signed { asset, public_key, .. } = signed(&dir, &data);
        let server = serve(data.clone(), None);
        let mut small = asset;
        small.size -= 10;
        let update = update_for(small, &server.url);
        let root = dir.join("updates");
        let err = download(&update, &root, &public_key, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert!(matches!(err, UpdateError::SizeMismatch { .. }), "{err:?}");
        assert!(fs::read_dir(root.join("0.0.2")).unwrap().next().is_none());
    }

    #[test]
    fn an_unusable_partial_file_starts_over() {
        let dir = temp_dir("dl-stale-part");
        let data = body();
        let Signed { asset, public_key, .. } = signed(&dir, &data);
        let server = serve(data.clone(), None);
        let update = update_for(asset, &server.url);
        let root = dir.join("updates");
        fs::create_dir_all(root.join("0.0.2")).unwrap();
        // Longer than the real file: cannot be a prefix.
        fs::write(root.join("0.0.2").join("artifact.part"), vec![7u8; data.len() + 5]).unwrap();
        let path = download(&update, &root, &public_key, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(fs::read(path).unwrap(), data);
    }

    #[test]
    fn cancelling_keeps_the_partial_file_and_says_so() {
        let dir = temp_dir("dl-cancel");
        let data = body();
        let Signed { asset, public_key, .. } = signed(&dir, &data);
        let server = serve(data, None);
        let update = update_for(asset, &server.url);
        let err = download(&update, &dir.join("updates"), &public_key, &AtomicBool::new(true), &mut |_| {}).unwrap_err();
        assert_eq!(err, UpdateError::Cancelled);
    }

    #[test]
    fn insecure_urls_are_refused_before_any_request() {
        let dir = temp_dir("dl-insecure");
        let Signed { asset, public_key, .. } = signed(&dir, b"x");
        let update = update_for(asset, "http://example.com/a");
        assert!(matches!(download(&update, &dir.join("u"), &public_key, &AtomicBool::new(false), &mut |_| {}), Err(UpdateError::InsecureUrl(_))));
    }
}
