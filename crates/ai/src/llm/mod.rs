//! Chat with a language model: a small provider abstraction, the Anthropic
//! Messages API with streaming (Server-Sent Events), an offline fallback that
//! says no model is configured, the producer context built from the project
//! and the mix plans a reply can carry.
//!
//! Nothing here touches the UI or the audio thread. [`spawn`] runs a request
//! on its own thread and hands the tokens back on a channel; a
//! [`CancelToken`] stops it between chunks.

pub mod anthropic;
mod base64;
pub mod context;
pub mod keychain;
pub mod mix;
pub mod sse;

use crossbeam_channel::{Receiver, Sender, unbounded};
use std::sync::Arc;

use crate::CancelToken;

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    /// An image (a screenshot): raw bytes and their MIME type
    /// (`image/png`, `image/jpeg`, `image/webp`, `image/gif`).
    Image { media_type: String, data: Arc<Vec<u8>> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self { role, parts: vec![Part::Text(text.into())] }
    }
}

/// One chat request.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

/// What a running request reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The next piece of the reply text.
    Delta(String),
    /// The reply is complete. `stop_reason` as the provider reports it
    /// (`end_turn`, `max_tokens`, `refusal`…).
    Done { stop_reason: Option<String> },
    /// The request failed; nothing more follows.
    Error(String),
}

/// A chat backend. `stream` blocks: call it from a worker thread (see
/// [`spawn`]). It must send exactly one terminal [`Event::Done`] or
/// [`Event::Error`], unless cancelled.
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn stream(&self, request: &Request, cancel: &CancelToken, tx: &Sender<Event>);
}

/// A Claude model offered in the model picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: &'static str,
    pub name: &'static str,
    pub hint: &'static str,
}

/// The models the assistant offers, newest first. The first is the default.
pub const MODELS: [ModelChoice; 3] = [
    ModelChoice { id: "claude-opus-5-5", name: "Claude Opus 5.5", hint: "Most capable, the default" },
    ModelChoice { id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", hint: "Fast and capable" },
    ModelChoice { id: "claude-haiku-5-5", name: "Claude Haiku 5.5", hint: "Fastest, lowest cost" },
];

/// Answers without a network: tells the user how to connect a model.
pub struct Offline;

/// The reply the offline provider gives.
pub const NO_MODEL: &str = "No model configured. Set the ANTHROPIC_API_KEY environment variable, or paste a key with \"Use API key from clipboard\" in the model menu (it is kept in the system keychain), then ask again. The / actions work without a model.";

impl Provider for Offline {
    fn name(&self) -> &str {
        "offline"
    }

    fn stream(&self, _: &Request, _: &CancelToken, tx: &Sender<Event>) {
        let _ = tx.send(Event::Delta(NO_MODEL.to_owned()));
        let _ = tx.send(Event::Done { stop_reason: Some("no_model".into()) });
    }
}

/// The provider to use right now: Anthropic when a key is found (environment
/// or keychain), else [`Offline`]. Reads the keychain, so call it off the UI
/// thread (the worker in [`spawn`] does).
pub fn default_provider() -> Arc<dyn Provider> {
    match keychain::api_key() {
        Some(key) => Arc::new(anthropic::Anthropic::new(key)),
        None => Arc::new(Offline),
    }
}

/// A request running on its own thread.
pub struct Stream {
    pub events: Receiver<Event>,
    pub cancel: CancelToken,
}

/// Run `request` on a worker thread. `provider` is resolved there too (`None`
/// = [`default_provider`]), so the keychain lookup never blocks the caller.
pub fn spawn(request: Request, provider: Option<Arc<dyn Provider>>) -> Stream {
    let (tx, events) = unbounded();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let spawned = std::thread::Builder::new().name("aitrack-llm".into()).spawn(move || {
        let provider = provider.unwrap_or_else(default_provider);
        provider.stream(&request, &token, &tx);
    });
    if let Err(e) = spawned {
        let (tx, events) = unbounded();
        let _ = tx.send(Event::Error(format!("could not start the request: {e}")));
        return Stream { events, cancel };
    }
    Stream { events, cancel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn offline_says_no_model() {
        let req = Request { model: MODELS[0].id.into(), system: String::new(), messages: vec![Message::text(Role::User, "hi")], max_tokens: 16 };
        let s = spawn(req, Some(Arc::new(Offline)));
        let first = s.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first, Event::Delta(NO_MODEL.to_owned()));
        assert!(matches!(s.events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Done { .. }));
    }

    #[test]
    fn default_model_is_opus() {
        assert_eq!(MODELS[0].id, "claude-opus-5-5");
    }
}
