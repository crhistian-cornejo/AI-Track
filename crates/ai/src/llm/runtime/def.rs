//! What a tool is, apart from how it runs: its name, the title its card
//! shows, whether it reads, edits or asks, and the schema the model sees.

use serde_json::{Value, json};

/// What a tool does, which decides how the panel and the MCP server treat it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    /// Only looks: it never changes the draft, and the panel folds a run of
    /// them into one line.
    Read,
    /// Changes the draft.
    Edit,
    /// Asks the user and waits for the answer (`ask_user`). The in-app
    /// assistant has it; the MCP server does not.
    Ask,
}

/// One tool, as the models, the panel and the MCP server all see it.
#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: &'static str,
    /// What the panel shows on the tool's card while it runs and after.
    pub title: &'static str,
    pub kind: ToolKind,
    /// MCP's destructiveHint: it removes something the user made
    /// (`delete_track`, `clear_bars`, `remove_device`).
    pub destructive: bool,
    pub description: &'static str,
    /// JSON Schema of the input: an object.
    pub schema: Value,
}

impl ToolDef {
    /// A tool that only reads.
    pub fn read(name: &'static str, title: &'static str, description: &'static str, schema: Value) -> Self {
        Self { name, title, kind: ToolKind::Read, destructive: false, description, schema }
    }

    /// A tool that edits the draft.
    pub fn edit(name: &'static str, title: &'static str, description: &'static str, schema: Value) -> Self {
        Self { name, title, kind: ToolKind::Edit, destructive: false, description, schema }
    }

    /// What a model is sent: `{ "name", "description", "input_schema" }`.
    pub fn to_json(&self) -> Value {
        json!({ "name": self.name, "description": self.description, "input_schema": self.schema })
    }
}
