//! Signing in to Splice without ever seeing a password.
//!
//! The app opens Splice's own sign-in page in the system browser (it
//! handles Google or e-mail), with an OAuth 2 authorization-code request
//! using PKCE and a loopback redirect (`http://127.0.0.1:<port>/callback`,
//! RFC 8252). A one-shot listener on that port takes the code, swaps it for
//! a token and the token goes to the macOS Keychain / Windows Credential
//! Manager. Splice publishes no client registration for third-party apps,
//! so the endpoints and client id come from the environment
//! ([`AuthConfig::from_env`]); without them sign-in reports that it is not
//! configured, and search and previews keep working signed out.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Keychain service name of the stored token.
pub const KEYCHAIN_SERVICE: &str = "dev.aitrack.splice";
const KEYCHAIN_USER: &str = "token";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthConfig {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scope: String,
}

impl AuthConfig {
    /// `AITRACK_SPLICE_CLIENT_ID` (required), `AITRACK_SPLICE_AUTHORIZE_URL`,
    /// `AITRACK_SPLICE_TOKEN_URL` and `AITRACK_SPLICE_SCOPE`.
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let client_id = var("AITRACK_SPLICE_CLIENT_ID")?;
        Some(Self {
            authorize_url: var("AITRACK_SPLICE_AUTHORIZE_URL").unwrap_or_else(|| "https://auth.splice.com/authorize".into()),
            token_url: var("AITRACK_SPLICE_TOKEN_URL").unwrap_or_else(|| "https://auth.splice.com/oauth/token".into()),
            client_id,
            scope: var("AITRACK_SPLICE_SCOPE").unwrap_or_else(|| "openid profile offline_access".into()),
        })
    }
}

/// Percent-encoding for query strings and form bodies (RFC 3986 unreserved kept).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `n` random bytes, base64url without padding.
pub fn random_token(n: usize) -> Result<String> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| Error::Auth(format!("no randomness: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// A PKCE verifier and its S256 challenge.
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Result<Self> {
        Ok(Self::from_verifier(random_token(32)?))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// The sign-in page to open in the browser.
pub fn authorize_url(cfg: &AuthConfig, redirect: &str, state: &str, challenge: &str) -> String {
    let sep = if cfg.authorize_url.contains('?') { '&' } else { '?' };
    format!(
        "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        cfg.authorize_url,
        encode(&cfg.client_id),
        encode(redirect),
        encode(&cfg.scope),
        encode(state),
        encode(challenge),
    )
}

/// What the browser brought back to the loopback listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    Token(String),
}

/// Read the request line of the redirect (`GET /callback?code=…&state=… HTTP/1.1`).
pub fn parse_callback(request_line: &str, state: &str) -> Result<Callback> {
    let target = request_line.split_whitespace().nth(1).ok_or_else(|| Error::Auth("malformed redirect".into()))?;
    let query = target.split_once('?').map_or("", |(_, q)| q);
    let mut code = None;
    let mut token = None;
    let mut got_state = None;
    let mut error = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v);
        match k {
            "code" => code = Some(v),
            "access_token" | "token" => token = Some(v),
            "state" => got_state = Some(v),
            "error_description" => error = Some(v),
            "error" if error.is_none() => error = Some(v),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(Error::Auth(format!("Splice refused the sign-in: {e}")));
    }
    if got_state.as_deref() != Some(state) {
        return Err(Error::Auth("the sign-in answer did not match this request".into()));
    }
    match (code, token) {
        (_, Some(t)) if !t.is_empty() => Ok(Callback::Token(t)),
        (Some(c), _) if !c.is_empty() => Ok(Callback::Code(c)),
        _ => Err(Error::Auth("the sign-in answer carried no code".into())),
    }
}

/// The form body that swaps a code for a token.
pub fn token_form(cfg: &AuthConfig, code: &str, verifier: &str, redirect: &str) -> String {
    format!(
        "grant_type=authorization_code&client_id={}&code={}&code_verifier={}&redirect_uri={}",
        encode(&cfg.client_id),
        encode(code),
        encode(verifier),
        encode(redirect)
    )
}

/// The access token of a token-endpoint response.
pub fn parse_token(body: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))?;
    if let Some(e) = v.get("error_description").or_else(|| v.get("error")).and_then(|e| e.as_str()) {
        return Err(Error::Auth(e.to_owned()));
    }
    v.get("access_token").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(str::to_owned).ok_or_else(|| Error::Auth("no access token in the answer".into()))
}

/// A one-shot HTTP listener on a free loopback port.
pub struct Loopback {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl Loopback {
    pub fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| Error::Auth(format!("cannot listen for the sign-in: {e}")))?;
        let port = listener.local_addr().map_err(|e| Error::Auth(e.to_string()))?.port();
        Ok(Self { listener, redirect_uri: format!("http://127.0.0.1:{port}/callback") })
    }

    /// Wait (up to `timeout`) for the browser's redirect and answer it with a short page.
    pub fn wait(self, state: &str, timeout: Duration) -> Result<Callback> {
        self.listener.set_nonblocking(true).map_err(|e| Error::Auth(e.to_string()))?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = answer(stream, state) {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(Error::Auth("the sign-in timed out".into()));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(Error::Auth(e.to_string())),
            }
        }
    }
}

/// Handle one connection: `None` for requests that are not the redirect (favicon…).
fn answer(stream: TcpStream, state: &str) -> Option<Result<Callback>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut stream = reader.into_inner();
    if !line.split_whitespace().nth(1).is_some_and(|t| t.starts_with("/callback")) {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return None;
    }
    let result = parse_callback(&line, state);
    let text = match &result {
        Ok(_) => "Signed in to Splice. You can close this tab and go back to AI-Track.",
        Err(_) => "Splice sign-in did not complete. Go back to AI-Track and try again.",
    };
    let body = format!("<!doctype html><meta charset=utf-8><title>AI-Track</title><p style=\"font:16px system-ui;margin:48px\">{text}</p>");
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    Some(result)
}

/// Where the token is kept.
pub trait TokenStore: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, token: &str) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// The OS credential store (Keychain, Credential Manager).
pub struct KeychainStore;

impl KeychainStore {
    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| Error::Auth(e.to_string()))
    }
}

impl TokenStore for KeychainStore {
    fn get(&self) -> Option<String> {
        Self::entry().ok()?.get_password().ok().filter(|t| !t.is_empty())
    }

    fn set(&self, token: &str) -> Result<()> {
        Self::entry()?.set_password(token).map_err(|e| Error::Auth(format!("cannot store the token: {e}")))
    }

    fn clear(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Auth(e.to_string())),
        }
    }
}

/// An in-memory store (tests, or a session that should not persist).
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<Option<String>>);

impl TokenStore for MemoryStore {
    fn get(&self) -> Option<String> {
        self.0.lock().ok()?.clone()
    }

    fn set(&self, token: &str) -> Result<()> {
        *self.0.lock().map_err(|_| Error::Auth("poisoned".into()))? = Some(token.to_owned());
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        *self.0.lock().map_err(|_| Error::Auth("poisoned".into()))? = None;
        Ok(())
    }
}

/// The whole flow, blocking (run it on a worker thread): open the sign-in
/// page with `open_browser`, wait for the redirect, get the token, store it.
pub fn sign_in(cfg: &AuthConfig, store: &dyn TokenStore, open_browser: impl FnOnce(&str), timeout: Duration) -> Result<()> {
    let pkce = Pkce::new()?;
    let state = random_token(16)?;
    let loopback = Loopback::bind()?;
    let redirect = loopback.redirect_uri.clone();
    open_browser(&authorize_url(cfg, &redirect, &state, &pkce.challenge));
    let token = match loopback.wait(&state, timeout)? {
        Callback::Token(t) => t,
        Callback::Code(code) => {
            let agent = crate::client::agent();
            let mut resp = agent
                .post(&cfg.token_url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .send(token_form(cfg, &code, &pkce.verifier, &redirect))
                .map_err(|e| Error::Network(e.to_string()))?;
            let body = resp.body_mut().read_to_string().map_err(|e| Error::Network(e.to_string()))?;
            parse_token(&body)?
        }
    };
    store.set(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AuthConfig {
        AuthConfig { authorize_url: "https://auth.example/authorize".into(), token_url: "https://auth.example/token".into(), client_id: "abc".into(), scope: "openid profile".into() }
    }

    #[test]
    fn pkce_challenge_is_rfc7636_s256() {
        // RFC 7636, appendix B.
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let a = Pkce::new().unwrap();
        assert!(a.verifier.len() >= 43 && a.verifier != Pkce::new().unwrap().verifier);
    }

    #[test]
    fn authorize_url_carries_the_pkce_request() {
        let u = authorize_url(&cfg(), "http://127.0.0.1:5000/callback", "s1", "ch");
        assert!(u.starts_with("https://auth.example/authorize?response_type=code&client_id=abc"));
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fcallback"));
        assert!(u.contains("scope=openid%20profile"));
        assert!(u.contains("state=s1") && u.contains("code_challenge=ch&code_challenge_method=S256"));
        assert!(!u.to_lowercase().contains("password"));
    }

    #[test]
    fn callback_checks_state_and_errors() {
        assert_eq!(parse_callback("GET /callback?code=x%2By&state=s HTTP/1.1", "s").unwrap(), Callback::Code("x+y".into()));
        assert_eq!(parse_callback("GET /callback?state=s&access_token=tok HTTP/1.1", "s").unwrap(), Callback::Token("tok".into()));
        assert!(parse_callback("GET /callback?code=x&state=other HTTP/1.1", "s").is_err());
        assert!(parse_callback("GET /callback?error=access_denied&state=s HTTP/1.1", "s").is_err());
        assert!(parse_callback("GET /callback?state=s HTTP/1.1", "s").is_err());
        assert!(parse_callback("garbage", "s").is_err());
    }

    #[test]
    fn token_form_and_answer() {
        let f = token_form(&cfg(), "c d", "v", "http://127.0.0.1:1/callback");
        assert_eq!(f, "grant_type=authorization_code&client_id=abc&code=c%20d&code_verifier=v&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcallback");
        assert_eq!(parse_token(r#"{"access_token":"t","token_type":"Bearer"}"#).unwrap(), "t");
        assert!(parse_token(r#"{"error":"invalid_grant"}"#).is_err());
        assert!(parse_token("nope").is_err());
    }

    #[test]
    fn loopback_takes_the_redirect() {
        let lb = Loopback::bind().unwrap();
        let addr = lb.redirect_uri.trim_start_matches("http://").trim_end_matches("/callback").to_owned();
        let browser = std::thread::spawn(move || {
            let mut s = TcpStream::connect(&addr).unwrap();
            s.write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n").unwrap();
            let mut s = TcpStream::connect(&addr).unwrap();
            s.write_all(b"GET /callback?code=abc&state=st HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut reply = String::new();
            let _ = std::io::Read::read_to_string(&mut s, &mut reply);
            reply
        });
        assert_eq!(lb.wait("st", Duration::from_secs(5)).unwrap(), Callback::Code("abc".into()));
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn memory_store_round_trips() {
        let s = MemoryStore::default();
        assert_eq!(s.get(), None);
        s.set("t").unwrap();
        assert_eq!(s.get().as_deref(), Some("t"));
        s.clear().unwrap();
        assert_eq!(s.get(), None);
    }
}
