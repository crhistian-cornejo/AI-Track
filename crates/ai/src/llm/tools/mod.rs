//! The assistant's tools: what a production, mixing and mastering engineer
//! needs to read and build a song in AI-Track. Every edit is a `Command`
//! applied to a [`Session`]'s draft of the project; the turn ends with all
//! of them offered as one proposal (one undo step), so the model never
//! touches the real project.

mod edit;
mod inspect;
pub mod refs;

use std::sync::OnceLock;

use aitrack_model::{Command, Project, TrackId};
use serde_json::{Value, json};

use super::runtime::{ToolDef, ToolKind};

/// The draft a turn edits, and what it changed.
pub struct Session {
    pub draft: Project,
    pub selected: Option<TrackId>,
    pub commands: Vec<Command>,
    /// One line per change, for the proposal card.
    pub changes: Vec<String>,
}

impl Session {
    pub fn new(project: &Project, selected: Option<TrackId>) -> Self {
        Self { draft: project.clone(), selected, commands: Vec::new(), changes: Vec::new() }
    }

    /// Apply `commands` to the draft as one change. Atomic: on error the
    /// draft is as it was.
    pub fn apply(&mut self, summary: &str, commands: Vec<Command>) -> Result<(), String> {
        Command::Batch { commands: commands.clone() }.apply(&mut self.draft).map_err(|e| e.to_string())?;
        self.commands.extend(commands);
        self.changes.push(summary.to_owned());
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn is_destructive(&self) -> bool {
        self.commands.iter().any(Command::is_destructive)
    }
}

/// What a tool call came to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Ok(String),
    Err(String),
    /// A tool the app runs itself (`generate_part`).
    Delegate,
}

/// The tools the app runs itself.
pub const DELEGATED: &[&str] = &["generate_part"];

/// Run `name` with `input` on the session.
pub fn execute(s: &mut Session, name: &str, input: &Value) -> Outcome {
    let r = match name {
        "get_project" => Ok(inspect::project(&s.draft)),
        "get_track" => refs::track_field(&s.draft, input, "track", s.selected).map(|id| inspect::track(&s.draft, id)),
        "get_device" => inspect::device(&s.draft, input),
        "get_notes" => inspect::notes(&s.draft, input),
        "list_plugins" => Ok(inspect::plugins(input)),
        "list_presets" => inspect::presets(input),
        "create_track" => edit::create_track(s, input),
        "update_track" => edit::update_track(s, input),
        "delete_track" => edit::delete_track(s, input),
        "group_tracks" => edit::group_tracks(s, input),
        "add_device" => edit::add_device(s, input),
        "set_params" => edit::set_params(s, input),
        "load_preset" => edit::load_preset(s, input),
        "remove_device" => edit::remove_device(s, input),
        "set_send" => edit::set_send(s, input),
        "set_sidechain" => edit::set_sidechain(s, input),
        "set_song" => edit::set_song(s, input),
        "write_clip" => edit::write_clip(s, input),
        "copy_clip" => edit::copy_clip(s, input),
        "move_clip" => edit::move_clip(s, input),
        "clear_bars" => edit::clear_bars(s, input),
        "set_automation" => edit::set_automation(s, input),
        n if DELEGATED.contains(&n) => return Outcome::Delegate,
        other => Err(format!("unknown tool \"{other}\"")),
    };
    match r {
        Ok(t) => Outcome::Ok(t),
        Err(e) => Outcome::Err(e),
    }
}

/// Every tool with its kind and title, built once: the project tools in a
/// stable order and [`ASK_USER`].
struct Registry {
    project: Vec<ToolDef>,
    ask: ToolDef,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry { project: project_defs(), ask: ask_def() })
}

fn lookup(name: &str) -> Option<&'static ToolDef> {
    let r = registry();
    r.project.iter().chain(std::iter::once(&r.ask)).find(|d| d.name == name)
}

/// The project tools, in the order the models see them.
pub fn defs() -> Vec<ToolDef> {
    registry().project.clone()
}

/// What `name` does; `None` when it is no tool.
pub fn kind(name: &str) -> Option<ToolKind> {
    lookup(name).map(|d| d.kind)
}

/// Whether `name` only reads. A question or an unknown name does not.
pub fn is_read(name: &str) -> bool {
    kind(name) == Some(ToolKind::Read)
}

/// What the panel shows on a tool's card while it runs and after.
pub fn title(name: &str) -> &'static str {
    lookup(name).map_or("Working", |d| d.title)
}

/// How the assistant works, for the system prompt (stable, cached).
pub const AGENT_PERSONA: &str = "You are the producer inside AI-Track, a DAW for electronic music: a senior music producer, \
arranger, sound designer, mix engineer and mastering engineer in one. You work in the open project with tools.

How you work:
- Answer in the user's language. Be concise and concrete: exact values (Hz, dB, ms, ratios, bars, BPM), short Markdown \
(headings only for long answers, bullet lists, **bold** for key values, tables when comparing).
- When the user asks for something to be done (create a track, write a bassline, build a drop, mix the drums, master the \
song, add effects), do it with the tools, then say briefly what you did and why. When they only ask a question, answer \
without editing.
- Look before you edit: get_project for ids and the overall picture; get_track and get_device before changing a track or \
a device you have not seen; list_plugins and list_presets to pick devices and sounds.
- Your edits go to a draft. When you finish, the user reviews all of them as one proposal (accept or discard, one undo \
step). Make complete, musical changes in one turn instead of asking for permission at each step.
- Notes: bars start at 1; inside a clip, note starts and lengths are in beats (quarter notes) from the clip's start; \
pitch is MIDI (C3 = 60, kick on 36); velocity 0–1. Stay in the project key unless asked. Drums use General MIDI notes on \
drum devices (36 kick, 38 snare, 39 clap, 42 closed hat, 46 open hat).
- Arrangement: build sections (intro, build, drop, break, outro) with write_clip, copy_clip, clear_bars and automation \
(filter sweeps, volume rides, risers). Electronic sections are usually 8 or 16 bars.
- Mixing: gain staging first (faders around -6 to -12 dB, headroom on Main), then subtractive EQ (low cut where there is \
no useful low end, cut mud at 200–400 Hz), compression with musical attack/release, sidechain the bass and pads to the \
kick (Compressor or Shaper with set_sidechain), reverb and delay on return tracks fed by sends rather than on every \
track, group drums and buses.
- Mastering on the Main track: corrective EQ, gentle glue (Bus Compressor ~2:1, 1–2 dB of gain reduction), optional \
saturation (Console or Tape), Multiband Dynamics if needed, Limiter last (ceiling -1 dBTP, aiming at about -9 to -7 LUFS \
for club music, -14 LUFS for streaming), and an Analyzer to check.
- Parameters are set in their own units by name (\"Threshold\": -18, \"Ratio\": 4, \"Freq\": 120) or by choice label \
(\"Type\": \"Low Cut\"). Read the real names with get_device first.
- You cannot hear the audio; reason from the project, the names and the user's description, and say so when it matters.
- If a tool fails, read the error, fix the call and continue.
- When a request is open in a way that changes the result a lot (genre, key, which track, how long), and the project \
does not answer it, ask once with ask_user: one short question and 2–4 concrete options. Do not ask about details you \
can decide yourself, and never ask for permission to edit.";

/// The tool the in-app assistant asks the user a question with. The app
/// shows a card with the options and the turn waits for the answer.
pub const ASK_USER: &str = "ask_user";

/// A question from [`ASK_USER`].
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub question: String,
    /// Label and an optional line under it.
    pub options: Vec<(String, String)>,
    /// Several options may be picked.
    pub multi: bool,
}

impl Question {
    /// Read the tool's input; the error says what is wrong with the call.
    pub fn from_input(input: &Value) -> Result<Self, String> {
        let question = input.get("question").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
        if question.is_empty() {
            return Err("ask_user needs a question".into());
        }
        let options: Vec<(String, String)> = input
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|o| match o {
                Value::String(s) => Some((s.trim().to_owned(), String::new())),
                o => Some((o.get("label")?.as_str()?.trim().to_owned(), o.get("description").and_then(Value::as_str).unwrap_or_default().trim().to_owned())),
            })
            .filter(|(l, _)| !l.is_empty())
            .take(6)
            .collect();
        let multi = input.get("multi_select").and_then(Value::as_bool).unwrap_or(false);
        Ok(Self { question, options, multi })
    }

    /// The tool result for the picked options and/or the user's own words.
    pub fn answer(&self, picked: &[usize], own: &str) -> String {
        let mut parts: Vec<String> = picked.iter().filter_map(|i| self.options.get(*i)).map(|(l, _)| format!("\"{l}\"")).collect();
        let own = own.trim();
        if !own.is_empty() {
            parts.push(format!("in their own words: \"{own}\""));
        }
        if parts.is_empty() { "The user skipped the question: decide yourself and say what you chose.".to_owned() } else { format!("The user answered {}.", parts.join(" and ")) }
    }
}

/// [`ASK_USER`]'s definition.
pub fn ask_user_def() -> ToolDef {
    registry().ask.clone()
}

fn ask_def() -> ToolDef {
    ToolDef {
        name: ASK_USER,
        title: "Asking you",
        kind: ToolKind::Ask,
        destructive: false,
        description: "Ask the user one short question with 2–4 options and wait for the answer. Only when the choice changes the result a lot and the project does not answer it; never for permission.",
        schema: obj(
            json!({
                "question": { "type": "string", "description": "One short question" },
                "options": { "type": "array", "items": { "type": "object", "properties": { "label": { "type": "string", "description": "A few words" }, "description": { "type": "string", "description": "One short line on what it means" } }, "required": ["label"] } },
                "multi_select": { "type": "boolean", "description": "Several options may be picked" },
            }),
            &["question", "options"],
        ),
    }
}

/// The tools of the in-app assistant: every project tool and [`ASK_USER`].
pub fn agent_definitions() -> Vec<Value> {
    let mut d = definitions();
    d.push(ask_user_def().to_json());
    d
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// The tool definitions sent with every request (stable order: cached).
pub fn definitions() -> Vec<Value> {
    registry().project.iter().map(ToolDef::to_json).collect()
}

/// The project tools in the order the model sees them (stable: the warm CLI process is keyed by a hash of them).
fn project_defs() -> Vec<ToolDef> {
    let track = json!({ "description": "Track id (number) or exact name; \"selected\" or \"main\" also work", "type": ["integer", "string"] });
    let device = json!({ "description": "Device id from get_track or create_track", "type": "integer" });
    let params = json!({ "type": "object", "description": "Parameter name (from get_device) → value in its own units, or a choice label for stepped parameters", "additionalProperties": { "type": ["number", "string", "boolean"] } });
    vec![
        ToolDef::read("get_project", "Reading the project", "The song (tempo, key, signature, loop, length) and every track with its id, level, routing, devices and timeline span. Call this first in a turn that edits, and whenever you need ids.", obj(json!({}), &[])),
        ToolDef::read("get_track", "Inspecting a track", "One track in full: devices with ids and the parameters that differ from default, every clip on the timeline (placement id, clip id, bar, length) and automation lanes. Call before editing a track.", obj(json!({ "track": track }), &["track"])),
        ToolDef::read("get_device", "Reading a device", "Every parameter of a device with its current value, range, units and choices. Call before set_params on a device you have not read.", obj(json!({ "device": device }), &["device"])),
        ToolDef::read("get_notes", "Reading notes", "The notes of a MIDI clip (pitch, start beat, length, velocity).", obj(json!({ "clip": { "type": "integer", "description": "Clip id from get_track" } }), &["clip"])),
        ToolDef::read("list_plugins", "Browsing devices", "The stock instruments, effects and MIDI effects you can add, with ids, tags and how many factory presets each has.", obj(json!({ "category": { "type": "string", "enum": ["instrument", "effect", "midi", "utility", "analyzer", "any"] } }), &[])),
        ToolDef::read("list_presets", "Browsing presets", "Factory presets of a stock device, filtered by a word (\"bass\", \"pad\", \"plate\"). Use to pick sounds.", obj(json!({ "plugin": { "type": "string", "description": "Plugin id or name" }, "query": { "type": "string" } }), &["plugin"])),
        ToolDef::edit("create_track", "Creating a track", "Add a track. MIDI tracks get an instrument (default the Wavetable Synth), optionally with a factory preset and parameters. Return tracks hold shared reverbs and delays.", obj(json!({
            "name": { "type": "string" },
            "kind": { "type": "string", "enum": ["midi", "audio", "group", "return"] },
            "instrument": { "type": "string", "description": "Stock instrument id or name for MIDI tracks (instrument.wavetable, instrument.vault, instrument.acid303, instrument.kick808, instrument.drum, instrument.sampler)" },
            "preset": { "type": "string", "description": "Factory preset of the instrument (list_presets)" },
            "params": params,
            "volume_db": { "type": "number" },
            "pan": { "type": "number", "description": "-1 left … 1 right" },
            "color": { "type": "integer", "description": "Palette index 0–15" },
            "after": { "description": "Put it after this track", "type": ["integer", "string"] },
        }), &["name"])),
        ToolDef::edit("update_track", "Adjusting a track", "Change a track's name, fader, pan, mute, solo, colour or output (route it into a group).", obj(json!({
            "track": track, "name": { "type": "string" }, "volume_db": { "type": "number", "description": "-70 … +6" },
            "pan": { "type": "number" }, "mute": { "type": "boolean" }, "solo": { "type": "boolean" }, "color": { "type": "integer" },
            "output": { "description": "Group track or \"main\"", "type": ["integer", "string"] },
        }), &["track"])),
        ToolDef { destructive: true, ..ToolDef::edit("delete_track", "Deleting a track", "Delete a track and its clips. Only when the user asks for it.", obj(json!({ "track": track }), &["track"])) },
        ToolDef::edit("group_tracks", "Grouping tracks", "Put tracks in a new group bus (drums, synths, vocals) to process them together.", obj(json!({ "tracks": { "type": "array", "items": { "type": ["integer", "string"] } }, "name": { "type": "string" } }), &["tracks"])),
        ToolDef::edit("add_device", "Adding a device", "Add a stock device to a track's chain (end by default): EQ, Compressor, Bus Compressor, Limiter, Saturator, Reverb, Delay, Filter, Shaper, Chorus, Utility, MIDI effects… Optionally with a preset and parameters.", obj(json!({
            "track": track, "plugin": { "type": "string", "description": "Plugin id (fx.eq) or name" },
            "preset": { "type": "string" }, "params": params, "position": { "type": "integer", "description": "0 = first in the chain" },
        }), &["track", "plugin"])),
        ToolDef::edit("set_params", "Setting parameters", "Set parameters of a device by name in their own units, and/or bypass it.", obj(json!({ "device": device, "params": params, "enabled": { "type": "boolean" } }), &["device"])),
        ToolDef::edit("load_preset", "Loading a preset", "Load a factory preset on a stock device.", obj(json!({ "device": device, "preset": { "type": "string" } }), &["device", "preset"])),
        ToolDef { destructive: true, ..ToolDef::edit("remove_device", "Removing a device", "Remove a device from its chain.", obj(json!({ "device": device }), &["device"])) },
        ToolDef::edit("set_send", "Routing a send", "Set a track's send level to a return track (creates the send; -70 dB removes it).", obj(json!({ "track": track, "to": { "description": "Return track", "type": ["integer", "string"] }, "level_db": { "type": "number" }, "pre_fader": { "type": "boolean" } }), &["track", "to", "level_db"])),
        ToolDef::edit("set_sidechain", "Routing a sidechain", "Feed a track into a device's sidechain input (duck the bass with a Compressor keyed by the kick). null disconnects.", obj(json!({ "device": device, "source": { "description": "Track that keys it, or null", "type": ["integer", "string", "null"] } }), &["device"])),
        ToolDef::edit("set_song", "Song settings", "Tempo, key and scale, loop region or the project's name.", obj(json!({
            "bpm": { "type": "number" }, "key": { "type": "string", "description": "Root: C, C#, Db, … B" },
            "scale": { "type": "string", "description": "major, minor, harmonic minor, melodic minor, dorian, phrygian, lydian, mixolydian, locrian, major pentatonic, minor pentatonic, blues, chromatic" },
            "loop_start_bar": { "type": "number" }, "loop_end_bar": { "type": "number" }, "name": { "type": "string" },
        }), &[])),
        ToolDef::edit("write_clip", "Writing notes", "Write a MIDI clip on a MIDI track's timeline with exactly these notes (you compose them). Replaces clips overlapping that range unless replace is false.", obj(json!({
            "track": track, "start_bar": { "type": "number", "description": "1 = song start" }, "length_bars": { "type": "number" },
            "name": { "type": "string" }, "replace": { "type": "boolean" },
            "notes": { "type": "array", "items": { "type": "object", "properties": {
                "pitch": { "type": "integer", "description": "MIDI note, 60 = C3" },
                "start": { "type": "number", "description": "Beats from the clip start" },
                "length": { "type": "number", "description": "Beats" },
                "velocity": { "type": "number", "description": "0–1" },
            }, "required": ["pitch", "start", "length"] } },
        }), &["track", "length_bars", "notes"])),
        ToolDef::edit("generate_part", "Generating a part", "Have AI-Track's pattern engine write a part in the project key (bass, chords, beat, arp, melody, perc) from a short description such as \"rolling bass, 4 bars\" or \"minimal house beat\". Good for quick drafts; use write_clip when you want exact notes.", obj(json!({
            "kind": { "type": "string", "enum": ["bass", "chords", "beat", "arp", "melody", "perc"] },
            "description": { "type": "string" }, "track": { "description": "Existing MIDI track, else a new one", "type": ["integer", "string"] },
        }), &["kind"])),
        ToolDef::edit("copy_clip", "Arranging clips", "Place an existing clip again (linked) at a bar, repeated `times` back to back: loop a 4-bar pattern over a 16-bar section.", obj(json!({
            "clip": { "type": "integer" }, "track": { "description": "Defaults to the clip's own track", "type": ["integer", "string"] },
            "start_bar": { "type": "number" }, "times": { "type": "integer" },
        }), &["clip", "start_bar"])),
        ToolDef::edit("move_clip", "Moving a clip", "Move a clip on the timeline (and to another track).", obj(json!({ "track": track, "placement": { "type": "integer" }, "start_bar": { "type": "number" }, "to_track": { "type": ["integer", "string"] } }), &["track", "placement", "start_bar"])),
        ToolDef { destructive: true, ..ToolDef::edit("clear_bars", "Clearing bars", "Remove the clips that start between two bars on one or more tracks (drop the drums in a break).", obj(json!({ "track": track, "tracks": { "type": "array", "items": { "type": ["integer", "string"] } }, "start_bar": { "type": "number" }, "end_bar": { "type": "number" } }), &["start_bar", "end_bar"])) },
        ToolDef::edit("set_automation", "Drawing automation", "Draw automation from points (bar, value) on a track's volume (dB), pan (-1…1) or a device parameter (its own units, e.g. a Filter cutoff sweep in Hz). curve -1…1 bends the segment after a point. Replaces the lane unless replace is false.", obj(json!({
            "track": track, "target": { "type": "string", "enum": ["volume", "pan", "param"] }, "device": device, "param": { "type": "string" },
            "replace": { "type": "boolean" },
            "points": { "type": "array", "items": { "type": "object", "properties": { "bar": { "type": "number" }, "value": { "type": "number" }, "curve": { "type": "number" } }, "required": ["bar", "value"] } },
        }), &["track", "target", "points"])),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Source, TrackKind};

    fn ok(o: Outcome) -> String {
        match o {
            Outcome::Ok(s) => s,
            other => panic!("{other:?}"),
        }
    }

    fn id_after(s: &str, word: &str) -> u64 {
        let i = s.find(word).unwrap_or_else(|| panic!("{word} in {s}")) + word.len();
        s[i..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap()
    }

    #[test]
    fn questions_read_their_options_and_answer_in_words() {
        let q = Question::from_input(&json!({ "question": " Which key? ", "options": [{ "label": "A minor", "description": "dark" }, "C major", { "label": "" }], "multi_select": true })).unwrap();
        assert_eq!(q.question, "Which key?");
        assert_eq!(q.options, vec![("A minor".to_owned(), "dark".to_owned()), ("C major".to_owned(), String::new())]);
        assert!(q.multi);
        assert_eq!(q.answer(&[1], ""), "The user answered \"C major\".");
        assert!(q.answer(&[0], "but slower").contains("own words: \"but slower\""));
        assert!(q.answer(&[], "").contains("skipped"));
        assert!(Question::from_input(&json!({ "options": [] })).is_err());
        assert!(agent_definitions().iter().any(|d| d["name"] == ASK_USER));
        assert!(!definitions().iter().any(|d| d["name"] == ASK_USER), "the MCP server does not offer it");
    }

    #[test]
    fn the_registry_keeps_names_order_kinds_and_titles() {
        let names: Vec<&str> = defs().iter().map(|d| d.name).collect();
        assert_eq!(names, ["get_project", "get_track", "get_device", "get_notes", "list_plugins", "list_presets", "create_track", "update_track", "delete_track", "group_tracks", "add_device", "set_params", "load_preset", "remove_device", "set_send", "set_sidechain", "set_song", "write_clip", "generate_part", "copy_clip", "move_clip", "clear_bars", "set_automation"]);
        for d in defs().iter().chain([ask_user_def()].iter()) {
            assert!(!d.title.is_empty() && d.title != "Working", "{}", d.name);
            let j = d.to_json();
            assert_eq!(j.as_object().unwrap().len(), 3, "{}", d.name);
            assert_eq!(j["input_schema"]["type"], "object");
        }
        assert_eq!(kind("list_plugins"), Some(ToolKind::Read));
        assert_eq!(kind("generate_part"), Some(ToolKind::Edit));
        assert_eq!(kind(ASK_USER), Some(ToolKind::Ask));
        assert!(!is_read(ASK_USER), "the panel never folds a question into the lookups");
        assert_eq!(title("get_project"), "Reading the project");
        assert_eq!(title("nope"), "Working");
        let destructive: Vec<&str> = defs().iter().filter(|d| d.destructive).map(|d| d.name).collect();
        assert_eq!(destructive, ["delete_track", "remove_device", "clear_bars"]);
    }

    #[test]
    fn every_definition_is_handled() {
        let mut s = Session::new(&Project::new("t"), None);
        for d in definitions() {
            let name = d["name"].as_str().unwrap();
            assert!(d["input_schema"]["type"] == "object");
            let out = execute(&mut s, name, &json!({}));
            assert!(!matches!(&out, Outcome::Err(e) if e.starts_with("unknown tool")), "{name}");
        }
    }

    #[test]
    fn builds_a_track_with_notes_effects_and_automation() {
        let p = Project::new("Night Drive");
        let mut s = Session::new(&p, None);
        let out = ok(execute(&mut s, "create_track", &json!({ "name": "Bass", "kind": "midi", "instrument": "Acid 303", "volume_db": -8 })));
        let track = id_after(&out, "track #");
        let dev = ok(execute(&mut s, "add_device", &json!({ "track": "Bass", "plugin": "Compressor" })));
        let comp = id_after(&dev, "device #");
        let info = refs::plugin_info(aitrack_stock::fx::compressor::ID).unwrap();
        let knob = info.params[0].name;
        ok(execute(&mut s, "set_params", &json!({ "device": comp, "params": { knob: info.params[0].min } })));
        let clip = ok(execute(&mut s, "write_clip", &json!({ "track": track, "start_bar": 2, "length_bars": 1, "notes": [
            { "pitch": 33, "start": 0, "length": 0.5, "velocity": 0.9 }, { "pitch": 45, "start": 1.5, "length": 0.5 }, { "pitch": 40, "start": 9, "length": 1 }
        ] })));
        assert!(clip.contains("2 notes"), "{clip}");
        let clip_id = id_after(&clip, "clip #");
        ok(execute(&mut s, "copy_clip", &json!({ "clip": clip_id, "start_bar": 3, "times": 3 })));
        ok(execute(&mut s, "set_automation", &json!({ "track": "bass", "target": "volume", "points": [{ "bar": 1, "value": -20 }, { "bar": 5, "value": -6 }] })));
        ok(execute(&mut s, "set_song", &json!({ "bpm": 126, "key": "A", "scale": "minor" })));

        let t = s.draft.track(TrackId(track)).unwrap();
        assert_eq!(t.kind, TrackKind::Instrument);
        assert_eq!(t.devices.len(), 2);
        assert_eq!(t.arrangement.len(), 4);
        assert_eq!(t.automation[0].points.len(), 2);
        assert!(matches!(s.draft.sources.values().next(), Some(Source::Notes(n)) if n.notes.len() == 2));
        assert_eq!(s.draft.tempo.initial_bpm(), 126.);
        assert_eq!(s.changes.len(), 7);

        // The recorded commands rebuild the same project from the original.
        let mut real = p.clone();
        Command::Batch { commands: s.commands.clone() }.apply(&mut real).unwrap();
        assert_eq!(real.tracks, s.draft.tracks);
    }

    #[test]
    fn mixing_moves_on_returns_groups_and_main() {
        let mut s = Session::new(&Project::new("t"), None);
        ok(execute(&mut s, "create_track", &json!({ "name": "Kick", "instrument": "Kick 808" })));
        ok(execute(&mut s, "create_track", &json!({ "name": "Bass" })));
        ok(execute(&mut s, "create_track", &json!({ "name": "Verb", "kind": "return" })));
        ok(execute(&mut s, "add_device", &json!({ "track": "Verb", "plugin": "Reverb" })));
        ok(execute(&mut s, "set_send", &json!({ "track": "Bass", "to": "Verb", "level_db": -14 })));
        let comp = id_after(&ok(execute(&mut s, "add_device", &json!({ "track": "Bass", "plugin": "fx.compressor" }))), "device #");
        ok(execute(&mut s, "set_sidechain", &json!({ "device": comp, "source": "Kick" })));
        let g = ok(execute(&mut s, "group_tracks", &json!({ "tracks": ["Kick", "Bass"], "name": "Low End" })));
        assert!(g.contains("group #"));
        ok(execute(&mut s, "add_device", &json!({ "track": "main", "plugin": "Limiter" })));
        let bass = s.draft.tracks.iter().find(|t| t.name == "Bass").unwrap();
        assert_eq!(bass.sends.len(), 1);
        assert!(bass.parent.is_some());
        assert_eq!(s.draft.main_track().devices.len(), 1);
        // Errors explain themselves and change nothing.
        let before = s.commands.len();
        assert!(matches!(execute(&mut s, "set_send", &json!({ "track": "Bass", "to": "Kick", "level_db": -10 })), Outcome::Err(_)));
        assert!(matches!(execute(&mut s, "add_device", &json!({ "track": "Nope", "plugin": "EQ" })), Outcome::Err(e) if e.contains("no track")));
        assert_eq!(s.commands.len(), before);
        assert_eq!(execute(&mut s, "generate_part", &json!({ "kind": "bass" })), Outcome::Delegate);
    }

    #[test]
    fn reads_describe_ids() {
        let mut s = Session::new(&Project::new("t"), None);
        ok(execute(&mut s, "create_track", &json!({ "name": "Lead" })));
        let proj = ok(execute(&mut s, "get_project", &json!({})));
        assert!(proj.contains("\"Lead\" [MIDI]"), "{proj}");
        let tr = ok(execute(&mut s, "get_track", &json!({ "track": "Lead" })));
        let dev = id_after(&tr, "Device #");
        let d = ok(execute(&mut s, "get_device", &json!({ "device": dev })));
        assert!(d.lines().count() > 3, "{d}");
        assert!(ok(execute(&mut s, "list_plugins", &json!({ "category": "instrument" }))).contains("instrument.wavetable"));
        assert!(s.changes.len() == 1, "reads change nothing");
    }
}
