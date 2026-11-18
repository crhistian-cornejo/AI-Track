//! Helpers for the tests: scratch folders, a throwaway signing key and a tiny
//! local HTTP server that understands `Range` (no real network in tests).

use crate::Asset;
use crate::verify::hex;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// An empty folder under the system temp dir, unique to this test.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aitrack-update-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Signed {
    pub path: PathBuf,
    pub asset: Asset,
    /// The text of the `.pub` file that verifies `asset`.
    pub public_key: String,
}

/// Write `bytes` to `dir/artifact.bin`, signed with a fresh throwaway key.
pub fn signed(dir: &Path, bytes: &[u8]) -> Signed {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let signature = minisign::sign(Some(&pair.pk), &pair.sk, Cursor::new(bytes), None, None).unwrap().into_string();
    let public_key = pair.pk.to_box().unwrap().into_string();
    let path = dir.join("artifact.bin");
    std::fs::write(&path, bytes).unwrap();
    let asset = Asset { url: String::new(), signature, sha256: hex(&Sha256::digest(bytes)), size: bytes.len() as u64 };
    Signed { path, asset, public_key }
}

/// Serves one body at `/artifact`. `Range: bytes=N-` gets a 206. The first
/// request can be cut short to look like a dropped connection.
pub struct Server {
    pub url: String,
    /// The `Range` start of each request, in order (`None` = no Range header).
    pub requests: Arc<Mutex<Vec<Option<u64>>>>,
    stop: Arc<AtomicBool>,
    addr: String,
}

pub fn serve(body: Vec<u8>, cut_first_at: Option<usize>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (reqs, flag) = (requests.clone(), stop.clone());
    std::thread::spawn(move || {
        let mut first = true;
        for conn in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut conn) = conn else { continue };
            let cut = if std::mem::take(&mut first) { cut_first_at } else { None };
            handle(&mut conn, &body, cut, &reqs);
        }
    });
    Server { url: format!("http://{addr}/artifact"), requests, stop, addr }
}

fn handle(conn: &mut TcpStream, body: &[u8], cut: Option<usize>, requests: &Mutex<Vec<Option<u64>>>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if conn.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_lowercase();
    let start = head.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| r.split('-').next()).and_then(|n| n.trim().parse::<u64>().ok());
    requests.lock().unwrap().push(start);
    let len = body.len() as u64;
    let (status, from) = match start {
        Some(n) if n >= len => {
            let _ = conn.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\ncontent-range: bytes */{len}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").as_bytes());
            return;
        }
        Some(n) => ("206 Partial Content", n),
        None => ("200 OK", 0),
    };
    let rest = &body[from as usize..];
    let mut out = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n", rest.len());
    if from > 0 {
        out += &format!("content-range: bytes {from}-{}/{len}\r\n", len - 1);
    }
    out += "\r\n";
    let _ = conn.write_all(out.as_bytes());
    let send = cut.map_or(rest.len(), |c| c.min(rest.len()));
    let _ = conn.write_all(&rest[..send]);
    let _ = conn.flush();
    let _ = conn.shutdown(Shutdown::Both);
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
    }
}
