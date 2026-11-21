//! The server over real HTTP on a loopback port, with the app faked: the
//! broker on a project of its own behind a lock.

use super::*;
use crate::broker::Broker;
use crate::gateway::ToolReply;
use aitrack_ai::llm::tools::{Outcome, Session};
use aitrack_model::{History, Project};

struct Fake {
    state: Mutex<(Project, History, Broker)>,
    clients: Mutex<Vec<String>>,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new((Project::new("Demo"), History::new(100), Broker::default())), clients: Mutex::default() })
    }
}

impl AppGateway for Fake {
    fn call_tool(&self, client: &str, name: &str, args: Value) -> ToolReply {
        self.clients.lock().push(client.to_owned());
        let mut g = self.state.lock();
        let (p, h, b) = &mut *g;
        let r = b.call(p, h, None, client, name, &args);
        ToolReply { text: r.text, is_error: r.is_error }
    }

    fn read_resource(&self, uri: &str) -> Result<String, String> {
        let g = self.state.lock();
        match uri {
            resources::PROJECT => match aitrack_ai::llm::tools::execute(&mut Session::new(&g.0, None), "get_project", &json!({})) {
                Outcome::Ok(t) | Outcome::Err(t) => Ok(t),
            },
            resources::PENDING => Ok(g.2.describe_pending(&g.1)),
            _ => Err("no such resource".into()),
        }
    }
}

const TOKEN: &str = "aitrack_test_token";

struct Client {
    url: String,
    token: Option<String>,
    session: Option<String>,
}

impl Client {
    /// POST one JSON-RPC message: the HTTP status and the JSON messages that came back.
    fn post(&mut self, body: Value) -> (u16, Vec<Value>) {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(20))).build().into();
        let mut req = agent.post(&self.url).header("Content-Type", "application/json").header("Accept", "application/json, text/event-stream");
        if let Some(t) = &self.token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s);
        }
        let mut resp = req.send(body.to_string()).unwrap();
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            self.session = Some(s.to_owned());
        }
        let status = resp.status().as_u16();
        let sse = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.contains("event-stream"));
        let text = resp.body_mut().read_to_string().unwrap();
        let msgs = if sse {
            text.lines().filter_map(|l| l.strip_prefix("data:")).filter_map(|d| serde_json::from_str(d.trim()).ok()).collect()
        } else {
            serde_json::from_str(&text).into_iter().collect()
        };
        (status, msgs)
    }

    fn rpc(&mut self, id: u64, method: &str, params: Value) -> Value {
        let (status, msgs) = self.post(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        assert_eq!(status, 200, "{method}: {msgs:?}");
        msgs.into_iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no answer to {method}"))
    }

    fn connect(&mut self) {
        let init = self.rpc(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "Test Agent", "version": "1" } }));
        assert_eq!(init["result"]["serverInfo"]["name"], "aitrack", "{init}");
        let (status, _) = self.post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        assert_eq!(status, 202);
    }
}

fn start(g: Arc<Fake>) -> (RunningServer, Client) {
    let s = RunningServer::start(0, TOKEN.to_owned(), g).unwrap();
    let c = Client { url: crate::settings::url(s.addr().port()), token: Some(TOKEN.to_owned()), session: None };
    (s, c)
}

#[test]
fn it_only_ever_binds_loopback() {
    for port in [0, 80, 47615] {
        assert!(bind_addr(port).ip().is_loopback() && bind_addr(port).ip().is_ipv4());
    }
    let (s, _) = start(Fake::new());
    assert!(s.addr().ip().is_loopback());
    assert_ne!(s.addr().port(), 0);
}

#[test]
fn handshake_and_tool_list_are_the_assistants_tools() {
    let (s, mut c) = start(Fake::new());
    c.connect();
    let list = c.rpc(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    let expected: Vec<String> = aitrack_ai::llm::tools::definitions().iter().map(|d| d["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, expected.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(names.contains(&"get_project") && names.contains(&"create_track") && names.contains(&"generate_part"));
    let create = tools.iter().find(|t| t["name"] == "create_track").unwrap();
    assert_eq!(create["inputSchema"]["type"], "object");
    assert_eq!(create["annotations"]["readOnlyHint"], false);
    assert_eq!(tools.iter().find(|t| t["name"] == "get_project").unwrap()["annotations"]["readOnlyHint"], true);
    assert_eq!(tools.iter().find(|t| t["name"] == "delete_track").unwrap()["annotations"]["destructiveHint"], true);
    // The client shows up as connected under the name it gave.
    assert_eq!(s.status().clients, vec!["Test Agent".to_owned()]);
}

#[test]
fn no_token_or_a_wrong_one_is_refused() {
    let (s, mut c) = start(Fake::new());
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "x", "version": "1" } } });
    c.token = None;
    assert_eq!(c.post(init.clone()).0, 401);
    for wrong in ["", "aitrack_test_tokeN", "aitrack_test_token_longer", "Bearer"] {
        c.token = Some(wrong.to_owned());
        assert_eq!(c.post(init.clone()).0, 401, "{wrong:?}");
    }
    // Nothing got in, so nobody is connected and nothing was logged.
    assert!(s.status().clients.is_empty() && s.log().is_empty());
    // A new token replaces the old one without a restart.
    s.set_token("aitrack_other".into());
    c.token = Some(TOKEN.to_owned());
    assert_eq!(c.post(init.clone()).0, 401);
    c.token = Some("aitrack_other".to_owned());
    assert_eq!(c.post(init).0, 200);
}

#[test]
fn a_tool_call_makes_a_proposal_the_user_can_accept_or_discard() {
    let fake = Fake::new();
    let (s, mut c) = start(fake.clone());
    c.connect();
    let call = c.rpc(3, "tools/call", json!({ "name": "create_track", "arguments": { "name": "Bass", "kind": "midi" } }));
    assert_eq!(call["result"]["isError"], false, "{call}");
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("accept"), "{text}");
    assert_eq!(fake.clients.lock().as_slice(), ["Test Agent"]);
    {
        let mut g = fake.state.lock();
        let (p, h, b) = &mut *g;
        assert!(p.tracks.iter().all(|t| t.name != "Bass"), "the project is untouched");
        assert_eq!(h.proposals().len(), 1);
        assert_eq!(h.proposals()[0].origin, aitrack_model::Origin::Agent("Test Agent".into()));
        let id = h.proposals()[0].id;
        assert_eq!(b.pending(h), Some(id));
        // The user accepts: one undo step.
        assert!(h.accept(p, id).unwrap());
        assert!(p.tracks.iter().any(|t| t.name == "Bass"));
        assert!(h.undo(p).unwrap() && !h.can_undo());
    }
    // A second edit is a new proposal the user discards.
    let call = c.rpc(4, "tools/call", json!({ "name": "create_track", "arguments": { "name": "Lead" } }));
    assert_eq!(call["result"]["isError"], false, "{call}");
    {
        let mut g = fake.state.lock();
        let (p, h, _) = &mut *g;
        let id = h.proposals()[0].id;
        assert!(h.discard(id));
        assert!(p.tracks.iter().all(|t| t.name != "Lead"));
    }
    // Tool errors come back as results the model can read; unknown tools as protocol errors.
    let bad = c.rpc(5, "tools/call", json!({ "name": "add_device", "arguments": { "track": "Nope", "plugin": "EQ" } }));
    assert_eq!(bad["result"]["isError"], true);
    let unknown = c.rpc(6, "tools/call", json!({ "name": "format_disk", "arguments": {} }));
    assert!(unknown.get("error").is_some(), "{unknown}");
    // The log names the tools, never the arguments or the token.
    let log = s.log();
    assert_eq!(log.iter().map(|r| r.tool.as_str()).collect::<Vec<_>>(), ["create_track", "create_track", "add_device"]);
    assert!(log.iter().all(|r| !r.note.contains(TOKEN)));
    assert_eq!(s.status().calls, 3);
}

#[test]
fn resources_and_prompts() {
    let fake = Fake::new();
    let (_s, mut c) = start(fake);
    c.connect();
    let list = c.rpc(2, "resources/list", json!({}));
    let uris: Vec<&str> = list["result"]["resources"].as_array().unwrap().iter().filter_map(|r| r["uri"].as_str()).collect();
    assert_eq!(uris, [resources::PROJECT, resources::PENDING, resources::GUIDE]);
    let project = c.rpc(3, "resources/read", json!({ "uri": resources::PROJECT }));
    assert!(project["result"]["contents"][0]["text"].as_str().unwrap().contains("Demo"), "{project}");
    let guide = c.rpc(4, "resources/read", json!({ "uri": resources::GUIDE }));
    assert!(guide["result"]["contents"][0]["text"].as_str().unwrap().contains("producer"));
    assert!(c.rpc(5, "resources/read", json!({ "uri": "aitrack://nope" })).get("error").is_some());
    c.rpc(6, "tools/call", json!({ "name": "create_track", "arguments": { "name": "Bass" } }));
    let pending = c.rpc(7, "resources/read", json!({ "uri": resources::PENDING }));
    assert!(pending["result"]["contents"][0]["text"].as_str().unwrap().contains("waits for the user"));

    let prompts = c.rpc(8, "prompts/list", json!({}));
    assert_eq!(prompts["result"]["prompts"].as_array().unwrap().len(), crate::prompts::list().len());
    let got = c.rpc(9, "prompts/get", json!({ "name": "build_part", "arguments": { "what": "rolling bass" } }));
    assert!(got["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("rolling bass"), "{got}");
    assert!(c.rpc(10, "prompts/get", json!({ "name": "nope" })).get("error").is_some());
}
