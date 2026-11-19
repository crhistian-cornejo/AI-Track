//! The chats of a project. They are kept inside its `.aitrack` file, under
//! an `"assistant"` key next to the project (older AI-Track versions read
//! past it), so every song opens with its own conversations. Each chat
//! keeps what was said and done: the user's messages, the replies with
//! their steps, notes and actions. Job and proposal cards are not kept:
//! they belong to the session that made them.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aitrack_ai::llm::cli::Vendor;
use serde::{Deserialize, Serialize};

use super::agent::{Reply, Step, ToolState};
use super::slash;
use super::state::{Item, Placed, ReplyState};
use crate::icons::Icon;

/// What the file holds under `"assistant"`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Archive {
    #[serde(default)]
    pub current: u64,
    #[serde(default)]
    pub chats: Vec<Chat>,
}

/// One conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    /// Unix seconds of the last message.
    #[serde(default)]
    pub updated: u64,
    #[serde(default)]
    pub items: Vec<Saved>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Saved {
    User {
        text: String,
        /// (kind, name): track, clip, sample, image.
        #[serde(default)]
        chips: Vec<(String, String)>,
    },
    Reply {
        model: String,
        #[serde(default)]
        vendor: String,
        #[serde(default)]
        steps: Vec<Step>,
        /// done, stopped or failed.
        #[serde(default)]
        state: String,
        #[serde(default)]
        error: String,
        #[serde(default)]
        seconds: Option<f32>,
        #[serde(default)]
        changes: usize,
    },
    Note {
        text: String,
    },
    Action {
        title: String,
        #[serde(default)]
        detail: String,
    },
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn chip_kind(i: Icon) -> &'static str {
    match i {
        Icon::Layers => "track",
        Icon::Waveform => "clip",
        Icon::Sample => "sample",
        _ => "image",
    }
}

fn chip_icon(kind: &str) -> Icon {
    match kind {
        "track" => Icon::Layers,
        "clip" => Icon::Waveform,
        "sample" => Icon::Sample,
        _ => Icon::File,
    }
}

fn state_words(s: &ReplyState) -> (&'static str, String) {
    match s {
        ReplyState::Failed(m) => ("failed", m.clone()),
        ReplyState::Stopped | ReplyState::Waiting | ReplyState::Streaming => ("stopped", String::new()),
        ReplyState::Done => ("done", String::new()),
    }
}

/// An item as it is kept; `None` for what is not worth keeping.
pub fn save(item: &Item) -> Option<Saved> {
    Some(match item {
        Item::User { text, chips } => Saved::User { text: text.clone(), chips: chips.iter().map(|(i, n)| (chip_kind(*i).to_owned(), n.clone())).collect() },
        Item::Agent(r) => {
            let (state, error) = state_words(&r.state);
            Saved::Reply {
                model: r.model.clone(),
                vendor: if r.vendor == Vendor::OpenAi { "openai".into() } else { "anthropic".into() },
                steps: r.steps.clone(),
                state: state.into(),
                error,
                seconds: r.elapsed.map(|d| d.as_secs_f32()),
                changes: r.changes,
            }
        }
        // A part's reply is its JSON spec; its card is not kept either.
        Item::Reply { is_part: true, .. } => return None,
        Item::Reply { text, model, state, mix, .. } => {
            let (state, error) = state_words(state);
            let shown = super::state::visible_reply(text, mix.is_some()).to_owned();
            Saved::Reply { model: (*model).to_owned(), vendor: "anthropic".into(), steps: vec![Step::Text(shown)], state: state.into(), error, seconds: None, changes: 0 }
        }
        Item::Action { title, detail, .. } => Saved::Action { title: (*title).to_owned(), detail: detail.clone() },
        Item::Note(t) => Saved::Note { text: t.clone() },
    })
}

/// A kept item back in the chat. Anything that was still running reads as stopped.
pub fn restore(s: &Saved) -> Item {
    match s {
        Saved::User { text, chips } => Item::User { text: text.clone(), chips: chips.iter().map(|(k, n)| (chip_icon(k), n.clone())).collect() },
        Saved::Reply { model, vendor, steps, state, error, seconds, changes } => {
            let mut steps = steps.clone();
            for st in &mut steps {
                match st {
                    Step::Thinking { done, .. } => *done = true,
                    Step::Tool { state: s @ ToolState::Running, .. } => *s = ToolState::Failed("not run".into()),
                    _ => {}
                }
            }
            Item::Agent(Reply {
                model: model.clone(),
                vendor: if vendor == "openai" { Vendor::OpenAi } else { Vendor::Anthropic },
                steps,
                state: match state.as_str() {
                    "failed" => ReplyState::Failed(error.clone()),
                    "stopped" => ReplyState::Stopped,
                    _ => ReplyState::Done,
                },
                started: Instant::now(),
                elapsed: seconds.map(Duration::from_secs_f32),
                changes: *changes,
            })
        }
        Saved::Note { text } => Item::Note(text.clone()),
        Saved::Action { title, detail } => match slash::ENTRIES.iter().find(|e| e.title == title) {
            Some(e) => Item::Action { icon: e.icon, title: e.title, detail: detail.clone() },
            None => Item::Note(format!("{title}: {detail}")),
        },
    }
}

/// A chat's name: the start of its first message.
pub fn title(items: &[Placed]) -> String {
    let first = items.iter().find_map(|p| if let Item::User { text, .. } = &p.item { Some(text.trim()) } else { None }).unwrap_or_default();
    let line = first.lines().next().unwrap_or_default();
    if line.is_empty() {
        return "New chat".into();
    }
    let mut t: String = line.chars().take(48).collect();
    if line.chars().count() > 48 {
        t = t.trim_end().to_owned() + "…";
    }
    t
}

/// "now", "5 min", "3 h", "2 d", "6 wk".
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "now".into(),
        60..3_600 => format!("{} min", s / 60),
        3_600..86_400 => format!("{} h", s / 3_600),
        86_400..1_209_600 => format!("{} d", s / 86_400),
        _ => format!("{} wk", s / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chats_round_trip_through_json() {
        let reply = Reply {
            model: "Opus 5.5".into(),
            vendor: Vendor::Anthropic,
            steps: vec![
                Step::Thinking { text: "hm".into(), done: false },
                Step::Tool { name: "create_track".into(), input: serde_json::json!({ "name": "Bass" }), state: ToolState::Done("New track".into()) },
                Step::Tool { name: "get_project".into(), input: serde_json::Value::Null, state: ToolState::Running },
                Step::Text("Done.".into()),
            ],
            state: ReplyState::Streaming,
            started: Instant::now(),
            elapsed: Some(Duration::from_secs(3)),
            changes: 1,
        };
        let items = [Item::User { text: "make a bass".into(), chips: vec![(Icon::Layers, "Kick".into())] }, Item::Agent(reply), Item::Note("hi".into())];
        let chat = Chat { id: 7, title: "make a bass".into(), updated: 1, items: items.iter().filter_map(save).collect() };
        let a = Archive { current: 7, chats: vec![chat] };
        let json = serde_json::to_string(&a).unwrap();
        let back: Archive = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        let restored: Vec<Item> = back.chats[0].items.iter().map(restore).collect();
        assert!(matches!(&restored[0], Item::User { chips, .. } if chips[0].0 == Icon::Layers));
        let Item::Agent(r) = &restored[1] else { panic!() };
        assert_eq!(r.state, ReplyState::Stopped, "a reply that was streaming reads as stopped");
        assert!(matches!(r.steps[0], Step::Thinking { done: true, .. }));
        assert!(matches!(&r.steps[2], Step::Tool { state: ToolState::Failed(_), .. }));
        assert_eq!(r.text(), "Done.");
    }

    #[test]
    fn titles_and_ages_read_short() {
        let mk = |t: &str| Placed { anchor: 0, item: Item::User { text: t.into(), chips: vec![] } };
        assert_eq!(title(&[]), "New chat");
        assert_eq!(title(&[mk("Write a bass\nin A minor")]), "Write a bass");
        assert!(title(&[mk(&"x".repeat(80))]).ends_with('…'));
        assert_eq!(ago(100, 130), "now");
        assert_eq!(ago(0, 7_200), "2 h");
        assert_eq!(ago(0, 3 * 86_400), "3 d");
    }
}
