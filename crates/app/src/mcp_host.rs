//! The app's side of the external MCP server (`aitrack-mcp`): the settings
//! and token, the server's lifetime, and the channel through which its
//! threads reach the UI thread. The server never sees app state; it sends a
//! [`UiRequest`] and waits (on its own thread) for the answer, which the
//! workspace gives from its housekeeping timer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aitrack_mcp::broker::Broker;
use aitrack_mcp::{AppGateway, CallRecord, McpSettings, RunningServer, Status, ToolReply, settings};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde_json::Value;

/// How long a call waits for the UI thread before it gives up.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

pub enum Ask {
    Tool { name: String, args: Value },
    Resource(String),
}

/// A call from the server's threads, with the way back.
pub struct UiRequest {
    pub client: String,
    pub ask: Ask,
    pub reply: Sender<ToolReply>,
}

/// What the server's threads hold: a way to ask the UI thread.
struct ChannelGateway {
    tx: Sender<UiRequest>,
}

impl ChannelGateway {
    fn ask(&self, client: &str, ask: Ask) -> ToolReply {
        let (reply, answer) = bounded(1);
        if self.tx.send(UiRequest { client: client.to_owned(), ask, reply }).is_err() {
            return ToolReply::err("AI-Track is closing.");
        }
        answer.recv_timeout(ANSWER_WITHIN).unwrap_or_else(|_| ToolReply::err("AI-Track did not answer in time. Is a dialog open? Try again."))
    }
}

impl AppGateway for ChannelGateway {
    fn call_tool(&self, client: &str, name: &str, args: Value) -> ToolReply {
        self.ask(client, Ask::Tool { name: name.to_owned(), args })
    }

    fn read_resource(&self, uri: &str) -> Result<String, String> {
        let r = self.ask("resource", Ask::Resource(uri.to_owned()));
        if r.is_error { Err(r.text) } else { Ok(r.text) }
    }
}

pub struct McpHost {
    pub settings: McpSettings,
    /// Loaded the first time it is needed.
    token: Option<String>,
    pub reveal_token: bool,
    server: Option<RunningServer>,
    /// Why the server is not running although it is on (the port is taken…).
    pub error: Option<String>,
    pub broker: Broker,
    tx: Sender<UiRequest>,
    pub rx: Receiver<UiRequest>,
    /// The last look at the server (refreshed by the timer, not every frame).
    pub status: Status,
    pub log: Vec<CallRecord>,
    refreshed: Instant,
    /// Dev view: made-up status, nothing is started or saved.
    pub demo: bool,
}

impl McpHost {
    pub fn load() -> Self {
        let settings = McpSettings::load();
        let (tx, rx) = unbounded();
        Self {
            broker: Broker::new(settings.apply_directly),
            settings,
            token: None,
            reveal_token: false,
            server: None,
            error: None,
            tx,
            rx,
            status: Status::default(),
            log: Vec::new(),
            refreshed: Instant::now(),
            demo: false,
        }
    }

    pub fn running(&self) -> bool {
        self.demo || self.server.is_some()
    }

    /// The bearer token (made on first use).
    pub fn token(&mut self) -> Option<String> {
        if self.token.is_none() {
            self.token = settings::ensure_token().map_err(|e| self.error = Some(format!("Could not save the token: {e}"))).ok();
        }
        self.token.clone()
    }

    pub fn set_token_for_demo(&mut self, t: &str) {
        self.token = Some(t.to_owned());
    }

    /// A new token; the running server uses it at once.
    pub fn regenerate_token(&mut self) -> Option<String> {
        match settings::regenerate_token() {
            Ok(t) => {
                if let Some(s) = &self.server {
                    s.set_token(t.clone());
                }
                self.token = Some(t.clone());
                Some(t)
            }
            Err(e) => {
                self.error = Some(format!("Could not save the token: {e}"));
                None
            }
        }
    }

    /// Start or stop the server to match the settings. Never blocks: binding is one system call.
    pub fn apply(&mut self) {
        if self.demo {
            return;
        }
        self.server = None;
        self.status = Status::default();
        self.error = None;
        if !self.settings.enabled {
            return;
        }
        let Some(token) = self.token() else { return };
        let gateway: Arc<dyn AppGateway> = Arc::new(ChannelGateway { tx: self.tx.clone() });
        match RunningServer::start(self.settings.port, token, gateway) {
            Ok(s) => {
                log::info!("MCP server listening on {}", s.addr());
                self.status = s.status();
                self.server = Some(s);
            }
            Err(e) => {
                log::warn!("MCP server could not start on port {}: {e}", self.settings.port);
                self.error = Some(if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!("Port {} is in use by another program. Pick another port.", self.settings.port)
                } else {
                    format!("The MCP server could not start: {e}")
                });
            }
        }
    }

    pub fn save(&self) {
        if self.demo {
            return;
        }
        if let Err(e) = self.settings.save() {
            log::warn!("could not save MCP settings: {e}");
        }
    }

    /// Refresh the cached status about twice a second; true if it changed.
    pub fn refresh(&mut self) -> bool {
        if self.demo || self.refreshed.elapsed() < Duration::from_millis(500) {
            return false;
        }
        self.refreshed = Instant::now();
        let Some(s) = &self.server else { return false };
        let (status, log) = (s.status(), s.log());
        let changed = status != self.status || log.len() != self.log.len() || log.last().map(|r| r.at) != self.log.last().map(|r| r.at);
        self.status = status;
        self.log = log;
        changed
    }

    /// "MCP: 1 client connected" for the status bar, once the server is on.
    pub fn status_text(&self) -> Option<String> {
        if !self.running() {
            return None;
        }
        Some(match self.status.clients.len() {
            0 => "MCP: waiting for a client".to_owned(),
            1 => "MCP: 1 client connected".to_owned(),
            n => format!("MCP: {n} clients connected"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_reach_the_ui_side_and_the_answer_comes_back() {
        let (tx, rx) = unbounded::<UiRequest>();
        let gw = ChannelGateway { tx };
        // The "UI thread": answers one tool call and one resource read.
        let ui = std::thread::spawn(move || {
            for _ in 0..2 {
                let req = rx.recv().unwrap();
                let text = match &req.ask {
                    Ask::Tool { name, .. } => format!("{} ran {name}", req.client),
                    Ask::Resource(u) => format!("read {u}"),
                };
                req.reply.send(ToolReply::ok(text)).unwrap();
            }
        });
        assert_eq!(gw.call_tool("Cursor", "get_project", serde_json::json!({})).text, "Cursor ran get_project");
        assert_eq!(gw.read_resource("aitrack://project").unwrap(), "read aitrack://project");
        ui.join().unwrap();
        // With the UI gone the call fails at once instead of waiting.
        assert!(gw.call_tool("Cursor", "get_project", serde_json::json!({})).is_error);
    }

    #[test]
    fn the_status_text_follows_the_clients() {
        let mut h = McpHost::load();
        h.demo = false;
        assert_eq!(h.status_text(), None);
        h.demo = true;
        assert_eq!(h.status_text().as_deref(), Some("MCP: waiting for a client"));
        h.status.clients = vec!["Claude Desktop".into()];
        assert_eq!(h.status_text().as_deref(), Some("MCP: 1 client connected"));
    }
}
