//! The server's only way into the app. The server (its own thread and
//! async runtime) never touches project state: every call goes through an
//! [`AppGateway`], which in the app sends it over a channel to the UI thread
//! and waits for the answer. Tests fake it.

use serde_json::Value;

/// What a tool call came to, as text for the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolReply {
    pub text: String,
    pub is_error: bool,
}

impl ToolReply {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }
}

/// The app, as the MCP server sees it. Calls block until the app answers, so
/// the server runs them on blocking threads, never on the UI thread.
pub trait AppGateway: Send + Sync + 'static {
    /// Run tool `name` for the client called `client`.
    fn call_tool(&self, client: &str, name: &str, args: Value) -> ToolReply;

    /// The text of the resource at `uri` (see [`crate::resources`]).
    fn read_resource(&self, uri: &str) -> Result<String, String>;
}
