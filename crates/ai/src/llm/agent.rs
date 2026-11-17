//! One step of the assistant's agent loop on the Anthropic Messages API:
//! a streamed request that carries the tools, adaptive thinking (summarised,
//! so the panel can show it) and the effort, decoded into [`AgentEvent`]s.
//!
//! The loop itself (run the tools, send their results, ask again) lives in
//! the app, because the tools edit a draft of the project on the UI thread.
//! Messages are raw JSON content blocks so the assistant's turns go back to
//! the API exactly as they came (thinking blocks keep their signatures).

use std::io::Read;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};

use super::anthropic::{API_URL, API_VERSION, agent, error_message, takes_fallbacks};
use super::sse::{Parser, SseEvent};
use crate::CancelToken;

const BETAS_FULL: &str = "server-side-fallback-2026-07-01,thinking-binding-controls-2026-08-01";

/// How hard the model thinks (`output_config.effort`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    #[default]
    High,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];

    pub fn id(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Effort::Low => "Quick",
            Effort::Medium => "Balanced",
            Effort::High => "Deep",
            Effort::Max => "Max",
        }
    }

    pub fn next(self) -> Effort {
        let i = Self::ALL.iter().position(|e| *e == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// One request of the loop.
#[derive(Clone, Debug)]
pub struct AgentRequest {
    pub model: String,
    /// Stable instructions first (cached), then the project snapshot.
    pub system: Vec<String>,
    /// `{"role": …, "content": [blocks]}` objects.
    pub messages: Vec<Value>,
    /// Tool definitions (`name`, `description`, `input_schema`).
    pub tools: Vec<Value>,
    pub max_tokens: u32,
    pub effort: Effort,
}

/// A tool the model asked for, its input parsed (or the raw text when it is
/// not valid JSON).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Result<Value, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    Text(String),
    Thinking(String),
    /// A tool call began streaming; its input follows.
    ToolStart { id: String, name: String },
    /// The step is over. `content` is the assistant turn to send back as is.
    Done { stop_reason: Option<String>, content: Vec<Value>, calls: Vec<ToolCall> },
    Error(String),
}

/// The JSON body for `r`.
pub fn body(r: &AgentRequest) -> Value {
    let mut system: Vec<Value> = r.system.iter().filter(|s| !s.is_empty()).map(|s| json!({ "type": "text", "text": s })).collect();
    // Cache the stable instructions and tools; the snapshot after them changes per turn.
    if let Some(first) = system.first_mut() {
        first["cache_control"] = json!({ "type": "ephemeral" });
    }
    let tools: Vec<Value> = r
        .tools
        .iter()
        .map(|t| {
            let mut t = t.clone();
            t["eager_input_streaming"] = json!(true);
            t
        })
        .collect();
    let full = takes_fallbacks(&r.model);
    let mut thinking = json!({ "type": "adaptive", "display": "summarized" });
    if full {
        thinking["block_binding"] = json!({ "prefix_mismatch_behavior": "drop_block" });
    }
    let mut b = json!({
        "model": r.model,
        "max_tokens": r.max_tokens,
        "stream": true,
        "system": system,
        "messages": r.messages,
        "thinking": thinking,
        "output_config": { "effort": r.effort.id() },
        "cache_control": { "type": "ephemeral" },
    });
    if !tools.is_empty() {
        b["tools"] = json!(tools);
    }
    if full {
        b["fallbacks"] = json!("default");
    }
    b
}

#[derive(Default)]
struct Block {
    value: Value,
    json: String,
}

/// Turns the SSE events of one step into [`AgentEvent`]s and assembles the
/// content blocks.
#[derive(Default)]
pub struct Decoder {
    blocks: Vec<Block>,
    stop_reason: Option<String>,
    finished: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    fn slot(&mut self, index: usize) -> &mut Block {
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, Block::default);
        }
        &mut self.blocks[index]
    }

    pub fn decode(&mut self, ev: &SseEvent) -> Option<AgentEvent> {
        let v: Value = serde_json::from_str(&ev.data).ok()?;
        let ty = v["type"].as_str().unwrap_or(ev.event.as_str());
        let index = v["index"].as_u64().unwrap_or(0) as usize;
        match ty {
            "content_block_start" => {
                let mut block = v["content_block"].clone();
                let out = if block["type"] == "tool_use" {
                    block["input"] = json!({});
                    Some(AgentEvent::ToolStart { id: block["id"].as_str().unwrap_or_default().to_owned(), name: block["name"].as_str().unwrap_or_default().to_owned() })
                } else {
                    None
                };
                self.slot(index).value = block;
                out
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let slot = self.slot(index);
                match d["type"].as_str() {
                    Some("text_delta") => {
                        let t = d["text"].as_str().unwrap_or_default();
                        append(&mut slot.value, "text", t);
                        Some(AgentEvent::Text(t.to_owned()))
                    }
                    Some("thinking_delta") => {
                        let t = d["thinking"].as_str().unwrap_or_default();
                        append(&mut slot.value, "thinking", t);
                        Some(AgentEvent::Thinking(t.to_owned()))
                    }
                    Some("signature_delta") => {
                        append(&mut slot.value, "signature", d["signature"].as_str().unwrap_or_default());
                        None
                    }
                    Some("input_json_delta") => {
                        slot.json.push_str(d["partial_json"].as_str().unwrap_or_default());
                        None
                    }
                    _ => None,
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_owned());
                }
                None
            }
            "message_stop" => {
                self.finished = true;
                Some(self.done())
            }
            "error" => {
                self.finished = true;
                Some(AgentEvent::Error(error_message(&v)))
            }
            _ => None,
        }
    }

    fn done(&mut self) -> AgentEvent {
        let mut content = Vec::new();
        let mut calls = Vec::new();
        for b in std::mem::take(&mut self.blocks) {
            let mut value = b.value;
            if value.is_null() {
                continue;
            }
            if value["type"] == "tool_use" {
                let raw = b.json.trim();
                let input = if raw.is_empty() {
                    Ok(json!({}))
                } else {
                    serde_json::from_str::<Value>(raw).ok().filter(Value::is_object).ok_or_else(|| raw.to_owned())
                };
                // The replayed block needs an object even when the input was broken.
                value["input"] = input.clone().unwrap_or_else(|_| json!({}));
                calls.push(ToolCall { id: value["id"].as_str().unwrap_or_default().to_owned(), name: value["name"].as_str().unwrap_or_default().to_owned(), input });
            }
            content.push(value);
        }
        AgentEvent::Done { stop_reason: self.stop_reason.take(), content, calls }
    }
}

fn append(block: &mut Value, field: &str, text: &str) {
    let mut s = block[field].as_str().unwrap_or_default().to_owned();
    s.push_str(text);
    block[field] = json!(s);
}

/// A step running on its own thread.
pub struct AgentStream {
    pub events: Receiver<AgentEvent>,
    pub cancel: CancelToken,
}

/// Run one step on a worker thread (the keychain is read there too).
pub fn spawn(request: AgentRequest) -> AgentStream {
    let (tx, events) = unbounded();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let spawned = std::thread::Builder::new().name("aitrack-agent".into()).spawn(move || match super::keychain::api_key() {
        Some(key) => run(&key, &request, &token, &tx),
        None => {
            let _ = tx.send(AgentEvent::Error(super::NO_MODEL.to_owned()));
        }
    });
    if let Err(e) = spawned {
        let (tx, events) = unbounded();
        let _ = tx.send(AgentEvent::Error(format!("could not start the request: {e}")));
        return AgentStream { events, cancel };
    }
    AgentStream { events, cancel }
}

fn run(key: &str, request: &AgentRequest, cancel: &CancelToken, tx: &Sender<AgentEvent>) {
    let mut req = agent()
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    if takes_fallbacks(&request.model) {
        req = req.header("anthropic-beta", BETAS_FULL);
    }
    let resp = match req.send(body(request).to_string().as_bytes()) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(AgentEvent::Error(format!("Could not reach the model: {e}")));
            return;
        }
    };
    let status = resp.status().as_u16();
    let mut reader = resp.into_body().into_reader();
    if status != 200 {
        let mut text = String::new();
        let _ = reader.take(64 * 1024).read_to_string(&mut text);
        let msg = serde_json::from_str::<Value>(&text).map(|v| error_message(&v)).unwrap_or_else(|_| text.trim().to_owned());
        let _ = tx.send(AgentEvent::Error(format!("HTTP {status}: {msg}")));
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
                let _ = tx.send(AgentEvent::Error(format!("The connection dropped: {e}")));
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
        let _ = tx.send(AgentEvent::Error("The reply ended early.".into()));
    }
}

/// A user turn of plain text.
pub fn user_text(text: &str) -> Value {
    json!({ "role": "user", "content": [{ "type": "text", "text": text }] })
}

/// The user turn that answers every tool call of the previous step.
pub fn tool_results(results: &[(String, String, bool)]) -> Value {
    let content: Vec<Value> = results
        .iter()
        .map(|(id, text, is_error)| {
            let mut b = json!({ "type": "tool_result", "tool_use_id": id, "content": text });
            if *is_error {
                b["is_error"] = json!(true);
            }
            b
        })
        .collect();
    json!({ "role": "user", "content": content })
}

/// The answer for a call whose input was not valid JSON.
pub fn invalid_json(raw: &str) -> String {
    json!({ "INVALID_JSON": raw }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"content\":[]}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Kick first.\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig==\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Adding an EQ.\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"add_device\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"track\\\": \\\"Ki\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ck\\\", \\\"plugin\\\": \\\"fx.eq\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    fn run(fixture: &str, chunk: usize) -> Vec<AgentEvent> {
        let mut p = Parser::new();
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for c in fixture.as_bytes().chunks(chunk) {
            for ev in p.feed(c) {
                out.extend(d.decode(&ev));
            }
        }
        out
    }

    #[test]
    fn assembles_thinking_text_and_tool_calls() {
        for chunk in [1, 5, 64, 8192] {
            let events = run(STREAM, chunk);
            assert_eq!(events[0], AgentEvent::Thinking("Kick first.".into()), "chunk {chunk}");
            assert_eq!(events[1], AgentEvent::Text("Adding an EQ.".into()));
            assert_eq!(events[2], AgentEvent::ToolStart { id: "toolu_1".into(), name: "add_device".into() });
            let AgentEvent::Done { stop_reason, content, calls } = &events[3] else { panic!("{events:?}") };
            assert_eq!(stop_reason.as_deref(), Some("tool_use"));
            assert_eq!(content[0]["signature"], "sig==");
            assert_eq!(content[0]["thinking"], "Kick first.");
            assert_eq!(content[2]["input"]["plugin"], "fx.eq");
            assert_eq!(calls[0].input.as_ref().unwrap()["track"], "Kick");
        }
    }

    #[test]
    fn broken_tool_input_is_reported_and_replayed_as_an_object() {
        let broken = STREAM.replace("\\\"fx.eq\\\"}", "\\\"fx.eq");
        let events = run(&broken, 64);
        let AgentEvent::Done { content, calls, .. } = events.last().unwrap() else { panic!() };
        assert!(calls[0].input.is_err());
        assert_eq!(content[2]["input"], json!({}));
    }

    #[test]
    fn body_has_tools_thinking_effort_and_cache() {
        let r = AgentRequest {
            model: "claude-opus-5-5".into(),
            system: vec!["persona".into(), "project".into()],
            messages: vec![user_text("hi")],
            tools: vec![json!({ "name": "get_project", "description": "d", "input_schema": { "type": "object", "properties": {} } })],
            max_tokens: 64_000,
            effort: Effort::High,
        };
        let b = body(&r);
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["thinking"]["display"], "summarized");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["tools"][0]["eager_input_streaming"], true);
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(b["system"][1].get("cache_control").is_none());
        assert_eq!(b["fallbacks"], "default");
        let haiku = body(&AgentRequest { model: "claude-haiku-5-5".into(), ..r });
        assert!(haiku.get("fallbacks").is_none());
        assert!(haiku["thinking"].get("block_binding").is_none());
    }

    #[test]
    fn tool_results_go_in_one_user_turn() {
        let m = tool_results(&[("a".into(), "ok".into(), false), ("b".into(), "bad".into(), true)]);
        assert_eq!(m["role"], "user");
        assert_eq!(m["content"][1]["is_error"], true);
        assert!(m["content"][0].get("is_error").is_none());
    }
}
