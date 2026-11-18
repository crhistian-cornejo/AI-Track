//! The resources the server lists: the project, the edits waiting for the
//! user, and the guide the in-app assistant works by.

use aitrack_ai::llm::tools::AGENT_PERSONA;
use serde_json::{Value, json};

pub const PROJECT: &str = "aitrack://project";
pub const PENDING: &str = "aitrack://pending";
pub const GUIDE: &str = "aitrack://guide";

pub fn list() -> Vec<Value> {
    vec![
        json!({ "uri": PROJECT, "name": "project", "title": "Open project", "description": "The song (tempo, key, signature, loop, length) and every track with ids, level, routing, devices and timeline span.", "mimeType": "text/plain" }),
        json!({ "uri": PENDING, "name": "pending", "title": "Edits waiting for the user", "description": "The draft proposal built by external agents that the user has not accepted or discarded yet.", "mimeType": "text/plain" }),
        json!({ "uri": GUIDE, "name": "guide", "title": "How to work in AI-Track", "description": "The producer guide the in-app assistant works by: units, bars and beats, drums, mixing and mastering conventions.", "mimeType": "text/markdown" }),
    ]
}

/// Resources whose text does not depend on the app.
pub fn fixed(uri: &str) -> Option<&'static str> {
    (uri == GUIDE).then_some(AGENT_PERSONA)
}

/// Resources the app answers.
pub fn is_known(uri: &str) -> bool {
    uri == PROJECT || uri == PENDING
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_resource_is_answerable() {
        for r in list() {
            let uri = r["uri"].as_str().unwrap();
            assert!(fixed(uri).is_some() || is_known(uri), "{uri}");
        }
    }
}
