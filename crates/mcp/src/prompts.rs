//! Ready-made prompts clients can offer as slash commands. They only ask the
//! agent to use the tools; nothing here edits anything.

use serde_json::{Value, json};

struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// (argument, description, required)
    args: &'static [(&'static str, &'static str, bool)],
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "build_part",
        title: "Write a part",
        description: "Write a bass line, chords, beat, arpeggio or melody in the project key.",
        args: &[("what", "What to write, e.g. \"rolling bass, 8 bars\" or \"minimal house beat\"", true), ("track", "Track to put it on (a new one if empty)", false)],
    },
    Prompt {
        name: "mix_check",
        title: "Mix the project",
        description: "Balance levels, EQ, compression, sends and sidechain across the project.",
        args: &[("focus", "What to work on, e.g. \"drums\" or \"low end\" (everything if empty)", false)],
    },
    Prompt {
        name: "master_song",
        title: "Master the song",
        description: "Build a mastering chain on the Main track.",
        args: &[("target", "Loudness target, e.g. \"club, -8 LUFS\" or \"streaming, -14 LUFS\"", false)],
    },
];

pub fn list() -> Vec<Value> {
    PROMPTS
        .iter()
        .map(|p| {
            let args: Vec<Value> = p.args.iter().map(|(n, d, r)| json!({ "name": n, "description": d, "required": r })).collect();
            json!({ "name": p.name, "title": p.title, "description": p.description, "arguments": args })
        })
        .collect()
}

pub fn description(name: &str) -> &'static str {
    PROMPTS.iter().find(|p| p.name == name).map_or("", |p| p.description)
}

fn arg(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The user message for prompt `name`, or `None` if there is no such prompt.
pub fn text(name: &str, args: &Value) -> Option<String> {
    Some(match name {
        "build_part" => {
            let what = arg(args, "what").unwrap_or_else(|| "a bass line".to_owned());
            let on = arg(args, "track").map_or("on a new MIDI track".to_owned(), |t| format!("on the track \"{t}\""));
            format!(
                "Read the project with get_project, then write this in AI-Track {on}: {what}. Stay in the project key and tempo. \
                 Use generate_part for a quick draft or write_clip when you want exact notes, and say briefly what you did. \
                 Your edits are offered to the user as a proposal."
            )
        }
        "mix_check" => {
            let focus = arg(args, "focus").map_or("the whole project".to_owned(), |f| format!("the {f}"));
            format!(
                "Read the project with get_project and get_track, then mix {focus}: gain staging first, subtractive EQ, compression, sends to return tracks, \
                 and sidechain where it helps. Explain each move in one line. Your edits are offered to the user as a proposal."
            )
        }
        "master_song" => {
            let target = arg(args, "target").unwrap_or_else(|| "club, about -8 LUFS".to_owned());
            format!(
                "Read the project with get_project, then build a mastering chain on the Main track for {target}: corrective EQ, gentle glue compression, \
                 optional saturation, a limiter last with a -1 dBTP ceiling. You cannot hear the audio, so say what to check by ear. Your edits are offered as a proposal."
            )
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_prompt_has_text_and_arguments_are_used() {
        for p in list() {
            let name = p["name"].as_str().unwrap();
            assert!(text(name, &json!({})).is_some(), "{name}");
        }
        assert!(text("build_part", &json!({ "what": "rolling bass", "track": "Bass" })).unwrap().contains("rolling bass"));
        assert!(text("nope", &json!({})).is_none());
    }
}
