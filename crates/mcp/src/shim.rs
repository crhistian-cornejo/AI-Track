//! The stdio shim: newline-delimited JSON-RPC in, the same out, forwarded to
//! the running app's HTTP endpoint with the bearer token. It keeps the MCP
//! session id and protocol version the server hands out and answers a
//! request with a JSON-RPC error when the app cannot be reached.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::settings::{self, McpSettings};

/// Where the app listens and the token it wants.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
}

impl Endpoint {
    /// Read the port and token the app saved in its config folder.
    pub fn from_config(dir: &Path) -> Result<Self, String> {
        let token = settings::read_token_from(dir).ok_or("no token yet: turn on Preferences > MCP server in AI-Track first")?;
        Ok(Self { url: settings::url(McpSettings::load_from(dir).port), token })
    }
}

fn error_line(id: &Value, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": message } }).to_string()
}

/// Forward every line of `input` and write what comes back to `out`.
pub fn run<R: BufRead, W: Write>(input: R, out: &mut W, ep: &Endpoint) -> io::Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(180))).build().into();
    let mut session: Option<String> = None;
    let mut version: Option<String> = None;
    for line in input.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
        let id = msg.get("id").cloned();
        let mut req = agent
            .post(&ep.url)
            .header("Authorization", &format!("Bearer {}", ep.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(s) = &session {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(v) = &version {
            req = req.header("MCP-Protocol-Version", v);
        }
        let mut resp = match req.send(line) {
            Ok(r) => r,
            Err(e) => {
                if let Some(id) = &id {
                    writeln!(out, "{}", error_line(id, &format!("AI-Track is not reachable ({e}). Open AI-Track and turn on Preferences > MCP server.")))?;
                    out.flush()?;
                }
                continue;
            }
        };
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            session = Some(s.to_owned());
        }
        let status = resp.status().as_u16();
        let event_stream = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.contains("text/event-stream"));
        if status == 202 || status == 204 {
            continue;
        }
        if !(200..300).contains(&status) {
            if let Some(id) = &id {
                let why = match status {
                    401 => "AI-Track refused the token. Open Preferences > MCP server and check it.".to_owned(),
                    s => format!("AI-Track's MCP server answered HTTP {s}."),
                };
                writeln!(out, "{}", error_line(id, &why))?;
                out.flush()?;
            }
            continue;
        }
        let mut emit = |payload: &str, out: &mut W| -> io::Result<()> {
            let Ok(v) = serde_json::from_str::<Value>(payload) else { return Ok(()) };
            if msg["method"] == "initialize"
                && let Some(p) = v["result"]["protocolVersion"].as_str()
            {
                version = Some(p.to_owned());
            }
            writeln!(out, "{v}")?;
            out.flush()
        };
        let reader = resp.body_mut().as_reader();
        if event_stream {
            let mut data = String::new();
            for l in io::BufReader::new(reader).lines() {
                let l = l?;
                if let Some(d) = l.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(d.strip_prefix(' ').unwrap_or(d));
                } else if l.is_empty() && !data.is_empty() {
                    emit(&std::mem::take(&mut data), out)?;
                }
            }
            if !data.is_empty() {
                emit(&data, out)?;
            }
        } else {
            let mut body = String::new();
            io::Read::read_to_string(&mut io::BufReader::new(reader), &mut body)?;
            emit(&body, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Read};
    use std::net::{TcpListener, TcpStream};

    /// What the fake server saw of one request.
    struct Seen {
        head: String,
        body: String,
    }

    fn read_request(s: &mut TcpStream) -> Seen {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            buf.push(byte[0]);
        }
        let head = String::from_utf8(buf).unwrap();
        let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        Seen { head, body: String::from_utf8(body).unwrap() }
    }

    /// Answers each request with the canned (status line, headers, body) in turn.
    fn fake(replies: Vec<(&'static str, &'static str, &'static str)>) -> (String, std::thread::JoinHandle<Vec<Seen>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, headers, body) in replies {
                let (mut s, _) = l.accept().unwrap();
                seen.push(read_request(&mut s));
                use std::io::Write;
                write!(s, "HTTP/1.1 {status}\r\n{headers}Connection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            }
            seen
        });
        (url, h)
    }

    #[test]
    fn forwards_requests_with_the_token_and_session_and_unwraps_events() {
        let init = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}"#;
        let list = "id: 0\nretry: 3000\ndata:\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        let (url, server) = fake(vec![
            ("200 OK", "Content-Type: application/json\r\nMcp-Session-Id: s-1\r\n", init),
            ("202 Accepted", "", ""),
            ("200 OK", "Content-Type: text/event-stream\r\n", list),
        ]);
        let ep = Endpoint { url, token: "aitrack_secret".into() };
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\nnot json\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let mut out = Vec::new();
        run(BufReader::new(input.as_bytes()), &mut out, &ep).unwrap();
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["result"]["tools"], json!([]));

        let seen = server.join().unwrap();
        let low = |s: &Seen| s.head.to_ascii_lowercase();
        assert!(low(&seen[0]).contains("authorization: bearer aitrack_secret"));
        assert!(low(&seen[0]).contains("accept: application/json, text/event-stream"));
        assert!(!low(&seen[0]).contains("mcp-session-id"), "none before the server hands one out");
        assert!(seen[0].body.contains("initialize"));
        assert!(low(&seen[1]).contains("mcp-session-id: s-1"));
        assert!(low(&seen[2]).contains("mcp-protocol-version: 2025-06-18"));
        assert!(seen[2].body.contains("tools/list"));
    }

    #[test]
    fn a_refused_token_or_a_missing_app_becomes_a_jsonrpc_error() {
        let (url, server) = fake(vec![("401 Unauthorized", "Content-Type: application/json\r\n", "{}")]);
        let ep = Endpoint { url, token: "wrong".into() };
        let mut out = Vec::new();
        run(BufReader::new("{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n".as_bytes()), &mut out, &ep).unwrap();
        server.join().unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["id"], 7);
        assert!(v["error"]["message"].as_str().unwrap().contains("token"));

        // Nothing listens: a request gets an error, a notification gets nothing.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let ep = Endpoint { url: settings::url(port), token: "t".into() };
        let mut out = Vec::new();
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n";
        run(BufReader::new(input.as_bytes()), &mut out, &ep).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("not reachable"));
    }

    #[test]
    fn the_endpoint_comes_from_the_config_folder() {
        let d = std::env::temp_dir().join(format!("aitrack-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(Endpoint::from_config(&d).is_err());
        McpSettings { port: 5555, ..Default::default() }.save_to(&d).unwrap();
        let t = settings::ensure_token_in(&d).unwrap();
        let ep = Endpoint::from_config(&d).unwrap();
        assert_eq!((ep.url.as_str(), ep.token.as_str()), ("http://127.0.0.1:5555/mcp", t.as_str()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
