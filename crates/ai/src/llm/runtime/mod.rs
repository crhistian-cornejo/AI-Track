//! The assistant's tool runtime: what a tool is ([`ToolDef`]) and, as it
//! grows, how a turn runs it.
//!
//! Not to be confused with [`crate::runtime`], which loads ONNX Runtime for
//! the models; this one belongs to the chat and the MCP server
//! (`aitrack_ai::llm::runtime`).

pub mod def;

pub use def::{ToolDef, ToolKind};
