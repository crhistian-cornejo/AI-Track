//! The external MCP server (PLAN.md F5): agents outside the app (Claude
//! Desktop, Claude Code in a terminal, Cursor, any MCP client) read and edit
//! the project that is open in a running AI-Track.
//!
//! - [`server`]: `rmcp`'s Streamable HTTP service on 127.0.0.1, behind a
//!   per-install bearer token ([`settings`]). Off unless the user turns it on.
//! - [`gateway`]: the one door into the app; the server never touches state.
//! - [`broker`]: what a tool call does to the project, on the UI thread:
//!   edits become a proposal the user accepts or discards.
//! - [`shim`]: the `aitrack-mcp` stdio binary for clients that only speak stdio.
//! - [`snippets`]: the config text for each client.
//!
//! The tools are the in-app assistant's (`aitrack_ai::llm::tools`), not a copy.

pub mod broker;
pub mod gateway;
pub mod prompts;
pub mod resources;
pub mod server;
pub mod settings;
pub mod shim;
pub mod snippets;

pub use gateway::{AppGateway, ToolReply};
pub use server::{CallRecord, RunningServer, Status};
pub use settings::McpSettings;
