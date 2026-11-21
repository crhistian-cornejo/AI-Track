//! The external MCP server: `rmcp`'s Streamable HTTP service behind a bearer
//! token, bound to 127.0.0.1 only, on its own thread and Tokio runtime.
//!
//! It knows nothing about the project. Tools run in the app through an
//! [`AppGateway`] (a channel to the UI thread there), on blocking threads
//! of this runtime, so a slow UI never stalls the network side and the
//! network never stalls the UI.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use aitrack_ai::llm::runtime::ToolKind;
use aitrack_ai::llm::tools;
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, HeaderValue, WWW_AUTHENTICATE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use parking_lot::{Mutex, RwLock};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::gateway::AppGateway;
use crate::settings::PATH;
use crate::{prompts, resources};

/// How many external calls the log keeps.
const LOG_CAP: usize = 100;
/// A client that called within this long counts as connected even without an open session.
const RECENT: Duration = Duration::from_secs(30);

/// The only address the server ever binds.
pub fn bind_addr(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// One external call, for the log in the app. Never holds the token or the arguments.
#[derive(Clone, Debug)]
pub struct CallRecord {
    pub at: SystemTime,
    pub client: String,
    pub tool: String,
    pub ok: bool,
    /// The first line of the answer.
    pub note: String,
}

/// What the app shows about the server.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub listening: Option<SocketAddr>,
    /// Names of the connected clients, one entry each.
    pub clients: Vec<String>,
    pub calls: u64,
}

/// State the server threads and the app both look at.
pub struct Shared {
    token: RwLock<String>,
    sessions: Mutex<HashMap<u64, String>>,
    recent: Mutex<HashMap<String, Instant>>,
    log: Mutex<VecDeque<CallRecord>>,
    calls: AtomicU64,
    next: AtomicU64,
}

impl Shared {
    fn new(token: String) -> Arc<Self> {
        Arc::new(Self {
            token: RwLock::new(token),
            sessions: Mutex::default(),
            recent: Mutex::default(),
            log: Mutex::default(),
            calls: AtomicU64::new(0),
            next: AtomicU64::new(1),
        })
    }

    fn authorized(&self, header: Option<&HeaderValue>) -> bool {
        let Some(h) = header.and_then(|h| h.to_str().ok()) else { return false };
        let Some((scheme, token)) = h.split_once(' ') else { return false };
        scheme.eq_ignore_ascii_case("bearer") && same(token.trim().as_bytes(), self.token.read().as_bytes())
    }

    fn record(&self, client: &str, tool: &str, ok: bool, text: &str) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.recent.lock().insert(client.to_owned(), Instant::now());
        let note: String = text.lines().next().unwrap_or_default().chars().take(120).collect();
        let mut log = self.log.lock();
        if log.len() == LOG_CAP {
            log.pop_front();
        }
        log.push_back(CallRecord { at: SystemTime::now(), client: client.to_owned(), tool: tool.to_owned(), ok, note });
    }

    fn status(&self, listening: Option<SocketAddr>) -> Status {
        let mut clients: Vec<String> = self.sessions.lock().values().cloned().collect();
        let now = Instant::now();
        for (name, at) in self.recent.lock().iter() {
            if now.duration_since(*at) < RECENT && !clients.contains(name) {
                clients.push(name.clone());
            }
        }
        clients.sort();
        Status { listening, clients, calls: self.calls.load(Ordering::Relaxed) }
    }
}

/// Equal without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// A running server. Dropping it stops it.
pub struct RunningServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    stop: CancellationToken,
}

impl RunningServer {
    /// Bind 127.0.0.1:`port` (0 = any free port) and serve in the background.
    /// Fails at once if the port is taken.
    pub fn start(port: u16, token: String, gateway: Arc<dyn AppGateway>) -> io::Result<Self> {
        let listener = std::net::TcpListener::bind(bind_addr(port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        debug_assert!(addr.ip().is_loopback());
        let shared = Shared::new(token);
        let stop = CancellationToken::new();
        let (s, st) = (shared.clone(), stop.clone());
        std::thread::Builder::new().name("aitrack-mcp".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("aitrack-mcp-rt").enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return log::error!("MCP server: no runtime: {e}"),
            };
            rt.block_on(serve(listener, s, gateway, st));
        })?;
        Ok(Self { addr, shared, stop })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn status(&self) -> Status {
        self.shared.status(Some(self.addr))
    }

    /// The most recent external calls, oldest first.
    pub fn log(&self) -> Vec<CallRecord> {
        self.shared.log.lock().iter().cloned().collect()
    }

    /// Use a new token from now on (clients with the old one are refused).
    pub fn set_token(&self, token: String) {
        *self.shared.token.write() = token;
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

type Body = BoxBody<Bytes, Infallible>;

fn reply(status: StatusCode, text: &str) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(json!({ "error": text }).to_string())).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(hyper::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

struct Ctx {
    shared: Arc<Shared>,
    mcp: StreamableHttpService<Handler, LocalSessionManager>,
}

async fn route(req: Request<Incoming>, ctx: Arc<Ctx>) -> Result<Response<Body>, Infallible> {
    if !ctx.shared.authorized(req.headers().get(AUTHORIZATION)) {
        let mut r = reply(StatusCode::UNAUTHORIZED, "A valid bearer token is required. Copy it from AI-Track > Preferences > MCP server.");
        r.headers_mut().insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"AI-Track\""));
        return Ok(r);
    }
    if req.uri().path() != PATH {
        return Ok(reply(StatusCode::NOT_FOUND, "The MCP endpoint is /mcp."));
    }
    Ok(ctx.mcp.handle(req).await)
}

async fn serve(listener: std::net::TcpListener, shared: Arc<Shared>, gateway: Arc<dyn AppGateway>, stop: CancellationToken) {
    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
    let factory = {
        let (shared, gateway) = (shared.clone(), gateway.clone());
        move || Ok(Handler::new(shared.clone(), gateway.clone()))
    };
    let config = StreamableHttpServerConfig::default().with_cancellation_token(stop.child_token());
    let mcp = StreamableHttpService::new(factory, Arc::new(LocalSessionManager::default()), config);
    let ctx = Arc::new(Ctx { shared, mcp });
    loop {
        let stream = tokio::select! {
            () = stop.cancelled() => break,
            r = listener.accept() => match r {
                Ok((stream, _)) => stream,
                Err(e) => {
                    log::warn!("MCP server: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let ctx = ctx.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let conn = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(move |req| route(req, ctx.clone())));
            tokio::pin!(conn);
            tokio::select! {
                _ = conn.as_mut() => {}
                () = stop.cancelled() => {}
            }
        });
    }
}

// ── The MCP handler ──

/// One per client session.
struct Handler {
    id: u64,
    shared: Arc<Shared>,
    gateway: Arc<dyn AppGateway>,
}

impl Handler {
    fn new(shared: Arc<Shared>, gateway: Arc<dyn AppGateway>) -> Self {
        let id = shared.next.fetch_add(1, Ordering::Relaxed);
        shared.sessions.lock().insert(id, "MCP client".to_owned());
        Self { id, shared, gateway }
    }

    fn name_client(&self, name: &str) {
        self.shared.sessions.lock().insert(self.id, name.to_owned());
    }

    /// Who is calling: the name from the handshake, else the HTTP User-Agent.
    fn client(&self, peer: &rmcp::service::Peer<RoleServer>, extensions: &rmcp::model::Extensions) -> String {
        let name = peer.peer_info().map(|i| i.client_info.name.clone()).filter(|n| !n.is_empty()).or_else(|| {
            let parts = extensions.get::<hyper::http::request::Parts>()?;
            let ua = parts.headers.get(hyper::header::USER_AGENT)?.to_str().ok()?;
            Some(ua.split(['/', ' ']).next().unwrap_or(ua).to_owned())
        });
        let name = name.unwrap_or_else(|| "MCP client".to_owned());
        self.name_client(&name);
        name
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.shared.sessions.lock().remove(&self.id);
    }
}

const INSTRUCTIONS: &str = "AI-Track is a DAW for electronic music. These tools read and edit the project that is open in the app. \
Edits are not applied: they become a draft proposal that the user accepts or discards in AI-Track (unless the user turned on direct apply), \
and reads show the draft. Start with get_project for ids; read the aitrack://guide resource for how to work.";

/// The tools, as MCP lists them: the in-app assistant's definitions, with hints.
pub fn tool_list() -> Vec<Value> {
    tools::defs()
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "title": d.title,
                "description": d.description,
                "inputSchema": d.schema,
                "annotations": { "readOnlyHint": d.kind == ToolKind::Read, "destructiveHint": d.destructive, "openWorldHint": false },
            })
        })
        .collect()
}

fn from<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, McpError> {
    serde_json::from_value(v).map_err(|e| McpError::internal_error(e.to_string(), None))
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().enable_prompts().build())
            .with_server_info(Implementation::new("aitrack", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.client(&context.peer, &context.extensions);
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        from(json!({ "tools": tool_list() }))
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if !tools::definitions().iter().any(|d| d["name"] == name.as_str()) {
            return Err(McpError::invalid_params(format!("unknown tool \"{name}\""), None));
        }
        let client = self.client(&context.peer, &context.extensions);
        let args = Value::Object(request.arguments.unwrap_or_default());
        let gateway = self.gateway.clone();
        let (c, n) = (client.clone(), name.clone());
        let reply = tokio::task::spawn_blocking(move || gateway.call_tool(&c, &n, args)).await.map_err(|e| McpError::internal_error(format!("the tool stopped: {e}"), None))?;
        self.shared.record(&client, &name, !reply.is_error, &reply.text);
        let content = vec![ContentBlock::text(reply.text)];
        Ok(CallToolResponse::Complete(if reply.is_error { CallToolResult::error(content) } else { CallToolResult::success(content) }))
    }

    async fn list_resources(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListResourcesResult, McpError> {
        from(json!({ "resources": resources::list() }))
    }

    async fn read_resource(&self, request: ReadResourceRequestParams, context: RequestContext<RoleServer>) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri.to_string();
        let text = match resources::fixed(&uri) {
            Some(t) => t.to_owned(),
            None if resources::is_known(&uri) => {
                let client = self.client(&context.peer, &context.extensions);
                let gateway = self.gateway.clone();
                let u = uri.clone();
                let r = tokio::task::spawn_blocking(move || gateway.read_resource(&u)).await.map_err(|e| McpError::internal_error(e.to_string(), None))?;
                self.shared.record(&client, &format!("read {uri}"), r.is_ok(), r.as_deref().unwrap_or_else(|e| e));
                r.map_err(|e| McpError::internal_error(e, None))?
            }
            None => return Err(McpError::resource_not_found(format!("no resource {uri}"), None)),
        };
        Ok(ReadResourceResponse::Complete(from::<ReadResourceResult>(json!({
            "contents": [serde_json::to_value(ResourceContents::text(text, uri)).map_err(|e| McpError::internal_error(e.to_string(), None))?],
        }))?))
    }

    async fn list_prompts(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListPromptsResult, McpError> {
        from(json!({ "prompts": prompts::list() }))
    }

    async fn get_prompt(&self, request: GetPromptRequestParams, _context: RequestContext<RoleServer>) -> Result<GetPromptResponse, McpError> {
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let text = prompts::text(&request.name, &args).ok_or_else(|| McpError::invalid_params(format!("no prompt \"{}\"", request.name), None))?;
        Ok(GetPromptResponse::Complete(from::<GetPromptResult>(json!({
            "description": prompts::description(&request.name),
            "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
        }))?))
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
