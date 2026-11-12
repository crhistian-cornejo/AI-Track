//! The Anthropic Messages API (`POST /v1/messages`) with `stream: true`.
//!
//! Plain HTTPS through `ureq` (rustls, the OS trust store); the reply is a
//! Server-Sent Events stream turned into [`Event`]s as it arrives.

use std::io::Read;
use std::time::Duration;

use crossbeam_channel::Sender;
use serde_json::{Value, json};

use super::sse::{Parser, SseEvent};
use super::{Event, Part, Provider, Request, Role};
use crate::CancelToken;

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2026-10-21";
/// Server-side refusal fallback (opted in on the models that take it).
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub struct Anthropic {
    key: String,
    url: String,
}

impl Anthropic {
    pub fn new(key: String) -> Self {
        Self { key, url: API_URL.to_owned() }
    }
}

/// Whether `model` takes the `fallbacks: "default"` parameter.
fn takes_fallbacks(model: &str) -> bool {
    matches!(model, "claude-opus-5-5" | "claude-sonnet-5-5")
}

/// The JSON body for `request`.
pub fn body(request: &Request) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|m| {
            let content: Vec<Value> = m
                .parts
                .iter()
                .map(|p| match p {
                    Part::Text(t) => json!({ "type": "text", "text": t }),
                    Part::Image { media_type, data } => json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": media_type, "data": super::base64::encode(data) },
                    }),
                })
                .collect();
            json!({ "role": if m.role == Role::User { "user" } else { "assistant" }, "content": content })
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !request.system.is_empty() {
        body["system"] = json!(request.system);
    }
    if takes_fallbacks(&request.model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Turns the SSE events of one reply into [`Event`]s.
#[derive(Default)]
pub struct Decoder {
    stop_reason: Option<String>,
    finished: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a terminal event (`Done` or `Error`) was produced.
    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn decode(&mut self, ev: &SseEvent) -> Option<Event> {
        let v: Value = serde_json::from_str(&ev.data).ok()?;
        let ty = v["type"].as_str().unwrap_or(ev.event.as_str());
        match ty {
            "content_block_delta" if v["delta"]["type"] == "text_delta" => v["delta"]["text"].as_str().map(|t| Event::Delta(t.to_owned())),
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_owned());
                }
                None
            }
            "message_stop" => {
                self.finished = true;
                Some(Event::Done { stop_reason: self.stop_reason.take() })
            }
            "error" => {
                self.finished = true;
                Some(Event::Error(error_message(&v)))
            }
            _ => None,
        }
    }
}

fn error_message(v: &Value) -> String {
    let e = &v["error"];
    match (e["type"].as_str(), e["message"].as_str()) {
        (Some("authentication_error"), _) => "The API key was rejected. Check ANTHROPIC_API_KEY or paste a new key.".into(),
        (Some("overloaded_error"), _) => "The model is overloaded right now. Try again in a moment.".into(),
        (Some("rate_limit_error"), _) => "Rate limited. Wait a moment and try again.".into(),
        (_, Some(m)) => m.to_owned(),
        _ => "The request failed.".into(),
    }
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_body(None)
        .user_agent(concat!("AI-Track/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

impl Provider for Anthropic {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn stream(&self, request: &Request, cancel: &CancelToken, tx: &Sender<Event>) {
        let mut req = agent()
            .post(&self.url)
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if takes_fallbacks(&request.model) {
            req = req.header("anthropic-beta", FALLBACK_BETA);
        }
        let resp = match req.send(body(request).to_string().as_bytes()) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Event::Error(format!("Could not reach the model: {e}")));
                return;
            }
        };
        let status = resp.status().as_u16();
        let mut reader = resp.into_body().into_reader();
        if status != 200 {
            let mut text = String::new();
            let _ = reader.take(64 * 1024).read_to_string(&mut text);
            let msg = serde_json::from_str::<Value>(&text).map(|v| error_message(&v)).unwrap_or_else(|_| text.trim().to_owned());
            let _ = tx.send(Event::Error(format!("HTTP {status}: {msg}")));
            return;
        }
        let mut parser = Parser::new();
        let mut decoder = Decoder::new();
        let mut buf = [0u8; 4096];
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("The connection dropped: {e}")));
                    return;
                }
            };
            for ev in parser.feed(&buf[..n]) {
                if let Some(out) = decoder.decode(&ev) {
                    let _ = tx.send(out);
                }
            }
            if decoder.finished() {
                return;
            }
        }
        if let Some(out) = parser.finish().and_then(|ev| decoder.decode(&ev)) {
            let _ = tx.send(out);
        }
        if !decoder.finished() {
            let _ = tx.send(Event::Error("The reply ended early.".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;
    use std::sync::Arc;

    fn run(fixture: &str, chunk: usize) -> Vec<Event> {
        let mut parser = Parser::new();
        let mut dec = Decoder::new();
        let mut out = Vec::new();
        for c in fixture.as_bytes().chunks(chunk) {
            for ev in parser.feed(c) {
                out.extend(dec.decode(&ev));
            }
        }
        out.extend(parser.finish().and_then(|ev| dec.decode(&ev)));
        out
    }

    #[test]
    fn streams_text_deltas_and_the_stop_reason() {
        let fixture = include_str!("fixtures/anthropic_stream.sse");
        for chunk in [1, 7, 64, 4096] {
            let events = run(fixture, chunk);
            assert_eq!(
                events,
                vec![
                    Event::Delta("Cut the bass ".into()),
                    Event::Delta("below 30 Hz".into()),
                    Event::Delta(" and duck it ♪.".into()),
                    Event::Done { stop_reason: Some("end_turn".into()) },
                ],
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn stream_errors_become_one_error_event() {
        let events = run(include_str!("fixtures/anthropic_error.sse"), 13);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Error(m) if m.contains("overloaded")));
    }

    #[test]
    fn body_carries_system_images_and_fallbacks() {
        let req = Request {
            model: "claude-opus-5-5".into(),
            system: "You are a producer.".into(),
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Image { media_type: "image/png".into(), data: Arc::new(b"foo".to_vec()) }, Part::Text("what is this?".into())],
            }],
            max_tokens: 4096,
        };
        let b = body(&req);
        assert_eq!(b["stream"], true);
        assert_eq!(b["system"], "You are a producer.");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["messages"][0]["content"][0]["source"]["data"], "Zm9v");
        assert_eq!(b["messages"][0]["content"][1]["text"], "what is this?");
        let haiku = body(&Request { model: "claude-haiku-5-5".into(), ..req });
        assert!(haiku.get("fallbacks").is_none());
    }
}
