//! The language model's half of the MIDI generator: the instructions, the
//! JSON schema of a [`MidiSpec`] and the prompt for one request. The model
//! writes the musical intent (chords with quality and extensions, step
//! patterns with accent classes, a motif, a feel label) and nothing else:
//! no ticks, no velocities, no MIDI numbers. What it writes goes through the
//! same `realize` and `validate` as a template's spec, which enforce the
//! limits the API schema cannot (it takes no `minimum`, `maximum` or
//! `minLength`), and a reply that does not parse or needs many repairs falls
//! back to the templates.

use serde_json::{Value, json};

use crate::generate::spec::{ChordSpec, Contour, Energy, Feel, Gate, GenRequest, Genre, Kind, MidiSpec, Pattern, Quality, Role, names};

/// Appended to the system prompt of a generation request.
pub const INSTRUCTIONS: &str = "The user asked you to write a MIDI part. Answer with the part as one JSON object that follows the given schema, \
and nothing else (if no schema is enforced, put it in one ```json block after at most two lines of text). \
You decide the musical intent; the program turns it into notes, so never write ticks, velocities, MIDI note numbers, swing percentages or octaves.\n\
\n\
Fields: kind (chords|bass|drums|arp|melody|perc), genre, bars (1-16), feel (straight|light_swing|swing|shuffle|loose), energy (low|mid|high), \
chords, layers, notes (up to 3 short reasons, in the user's language).\n\
chords: a list of {bar, beat, beats, root, alter, quality, ext}. root is a degree 0-6 of the 7-note scale of the key (0 = tonic), alter -1/0/+1 semitones \
(the leading tone of a V in minor is root 4 with quality dom7 or dom9; a phrygian bII is root 1 alter -1). quality: maj min dim aug sus2 sus4 maj7 min7 dom7 min9 maj9 dom9 add9 min6 min11 dim7 m7b5. \
ext: any of 9 11 13 b9 #11 b13 (clashing ones are dropped). Use sevenths at least in house, deep house, tech house and pop; triads or a single chord suit techno and trance. \
Bass, arp and melody parts also list the chords they follow: reuse the chords of the project when the context gives them.\n\
layers: a list of {role, pattern, steps, accents, pitches, contour, motif, gate, octave}. role: bass sub chord arp lead kick snare clap hat open_hat perc crash. \
steps and accents have 16 characters per bar for 4/4 (one per sixteenth; for other meters, the numerator times 16 over the denominator): steps uses . for a rest and x for a hit; \
accents is a digit 0-4 per step (0 ghost, 1 soft, 2 normal, 3 accent, 4 peak) and . where steps has a rest. A pattern that repeats every bar needs only one bar.\n\
pattern names: bass: rolling (every sixteenth but the first of each beat), offbeat, octaves, root_fifth, whole, sub808 (on the kick), sync332; \
chords: stabs_offbeat, sync332, charleston, pump8ths, whole; arp order: up down up_down random; use custom with explicit steps for anything else. \
pitches (bass): r 3 5 7 8 b7 a, relative to the chord (a is a semitone approach to the next chord root, use it rarely). \
motif (melody): moves in scale degrees between consecutive notes, mostly -1, 0 and 1. contour: arch rise fall wave flat. gate: short mid long. octave: -2..2.\n\
\n\
Rules that make it sound right: the bass leaves the kick steps free (steps 0, 4, 8, 12 of a four on the floor bar) unless it is an 808 locked to the kick; \
the kick and the clap or snare are never swung; hats and percussion carry the swing; vary accents (ghost notes, soft fillers, accented offbeats) instead of one level; \
the top voice of chords moves in small steps; melodies move by step, repeat a short motif and end on a chord tone; keep everything in the key; do not repeat a bar exactly four times.\n\
\n\
Minimal bass example: {\"kind\":\"bass\",\"genre\":\"techno\",\"bars\":2,\"feel\":\"straight\",\"energy\":\"mid\",\"chords\":[{\"bar\":0,\"beat\":0,\"beats\":8,\"root\":0,\"alter\":0,\"quality\":\"min7\",\"ext\":[]}],\
\"layers\":[{\"role\":\"bass\",\"pattern\":\"rolling\",\"steps\":\".xxx.xxx.xxx.xxx\",\"accents\":\".122.122.122.122\",\"pitches\":[\"r\"],\"contour\":\"arch\",\"motif\":[],\"gate\":\"mid\",\"octave\":0}],\"notes\":[\"Rolling bass that leaves the kick steps free\"]}\n\
Minimal chords example: {\"kind\":\"chords\",\"genre\":\"deep_house\",\"bars\":4,\"feel\":\"light_swing\",\"energy\":\"mid\",\"chords\":[{\"bar\":0,\"beat\":0,\"beats\":4,\"root\":0,\"alter\":0,\"quality\":\"min9\",\"ext\":[]},\
{\"bar\":1,\"beat\":0,\"beats\":4,\"root\":3,\"alter\":0,\"quality\":\"min9\",\"ext\":[]},{\"bar\":2,\"beat\":0,\"beats\":4,\"root\":4,\"alter\":0,\"quality\":\"dom9\",\"ext\":[]},{\"bar\":3,\"beat\":0,\"beats\":4,\"root\":0,\"alter\":0,\"quality\":\"min7\",\"ext\":[\"9\"]}],\
\"layers\":[{\"role\":\"chord\",\"pattern\":\"stabs_offbeat\",\"steps\":\"..x...x...x...x.\",\"accents\":\"..3...2...3...2.\",\"pitches\":[],\"contour\":\"arch\",\"motif\":[],\"gate\":\"mid\",\"octave\":0}],\"notes\":[\"i9 iv9 V9 i7 with stabs on the offbeats\"]}";

fn strings(all: Vec<String>) -> Value {
    json!({ "type": "string", "enum": all })
}

/// The schema of a [`MidiSpec`] for `output_config.format`. Only types and
/// enums: the API takes no numeric or length bounds, so those are applied by
/// `MidiSpec::clamped` and `validate`.
pub fn schema() -> Value {
    let chord = json!({
        "type": "object",
        "properties": {
            "bar": { "type": "integer" },
            "beat": { "type": "number" },
            "beats": { "type": "number" },
            "root": { "type": "integer" },
            "alter": { "type": "integer" },
            "quality": strings(names(&Quality::ALL)),
            "ext": { "type": "array", "items": { "type": "string", "enum": ["9", "11", "13", "b9", "#11", "b13"] } },
        },
        "required": ["bar", "beat", "beats", "root", "alter", "quality", "ext"],
        "additionalProperties": false,
    });
    let layer = json!({
        "type": "object",
        "properties": {
            "role": strings(names(&Role::ALL)),
            "pattern": strings(names(&Pattern::ALL)),
            "steps": { "type": "string" },
            "accents": { "type": "string" },
            "pitches": { "type": "array", "items": { "type": "string", "enum": ["r", "3", "5", "7", "8", "b7", "a"] } },
            "contour": strings(names(&Contour::ALL)),
            "motif": { "type": "array", "items": { "type": "integer" } },
            "gate": strings(names(&Gate::ALL)),
            "octave": { "type": "integer" },
        },
        "required": ["role", "pattern", "steps", "accents", "pitches", "contour", "motif", "gate", "octave"],
        "additionalProperties": false,
    });
    json!({
        "type": "object",
        "properties": {
            "kind": strings(names(&Kind::ALL)),
            "genre": strings(names(&Genre::ALL)),
            "bars": { "type": "integer" },
            "feel": strings(names(&Feel::ALL)),
            "energy": strings(names(&Energy::ALL)),
            "chords": { "type": "array", "items": chord },
            "layers": { "type": "array", "items": layer },
            "notes": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["kind", "genre", "bars", "feel", "energy", "chords", "layers", "notes"],
        "additionalProperties": false,
    })
}

/// The user message for `req`: what was asked, in the program's words. The
/// words the user typed ride along in `free_text`. `previous` is a spec to
/// avoid repeating ("Regenerate").
pub fn user_prompt(req: &GenRequest, previous: Option<&MidiSpec>) -> String {
    use std::fmt::Write;
    let mut s = format!("Write a {} part, {} bar(s).", req.kind.label().to_lowercase(), req.bars);
    if req.genre != Genre::Other {
        let _ = write!(s, " Genre: {}.", req.genre.label());
    }
    if let Some(k) = req.key {
        let _ = write!(s, " Key: {}.", k.display());
    }
    if let Some(f) = req.feel {
        let _ = write!(s, " Feel: {}.", serde_json::to_value(f).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default());
    }
    let _ = write!(s, " Density {:.1} of 1, energy {}.", req.density, serde_json::to_value(req.energy).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default());
    if let Some(p) = req.pattern {
        let _ = write!(s, " Use the pattern {}.", serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default());
    }
    if !req.only.is_empty() {
        let roles: Vec<&str> = req.only.iter().map(|r| r.label()).collect();
        let _ = write!(s, " Only these layers: {}.", roles.join(", "));
    }
    if !req.free_text.is_empty() {
        let _ = write!(s, "\nThe user's words: {}", req.free_text);
    }
    if let Some(prev) = previous
        && let Ok(json) = serde_json::to_string(prev)
    {
        let _ = write!(s, "\nA take like this one was already offered; write a clearly different one: {json}");
    }
    s
}

/// The spec in a model reply (a bare object, or in a fenced block).
pub fn parse_reply(text: &str) -> Option<MidiSpec> {
    MidiSpec::parse(text)
}

/// A one-line summary of a spec's chords for logs and the card.
pub fn chord_line(chords: &[ChordSpec]) -> String {
    chords.iter().take(6).map(|c| format!("{}{}", c.root + 1, if c.alter != 0 { "*" } else { "" })).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::spec::Layer;

    /// Every object has `additionalProperties: false`, nothing recurses and
    /// none of the keywords the API refuses appears.
    fn check(v: &Value, path: &str) {
        match v {
            Value::Object(m) => {
                // The keys of a "properties" map are field names, not keywords.
                let keywords = !path.ends_with("/properties");
                for k in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "minLength", "maxLength", "maxItems", "multipleOf", "$ref", "$defs", "definitions", "oneOf", "allOf", "pattern"] {
                    assert!(!keywords || !m.contains_key(k), "{path} uses {k}");
                }
                if let Some(n) = m.get("minItems") {
                    assert!(n.as_u64().unwrap_or(9) <= 1, "{path} minItems");
                }
                if keywords && m.get("type") == Some(&json!("object")) {
                    assert_eq!(m.get("additionalProperties"), Some(&json!(false)), "{path}");
                    let props = m["properties"].as_object().unwrap();
                    let required: Vec<&str> = m["required"].as_array().unwrap().iter().filter_map(|r| r.as_str()).collect();
                    assert!(required.iter().all(|r| props.contains_key(*r)), "{path} requires a field it does not have");
                }
                for (k, x) in m {
                    check(x, &format!("{path}/{k}"));
                }
            }
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| check(x, &format!("{path}/{i}"))),
            _ => {}
        }
    }

    #[test]
    fn the_schema_uses_only_what_the_api_accepts() {
        let s = schema();
        check(&s, "");
        assert_eq!(s["type"], "object");
        // It stays small enough for a prompt-sized request.
        assert!(s.to_string().len() < 4_000, "{}", s.to_string().len());
    }

    #[test]
    fn the_schema_lists_exactly_the_words_the_spec_understands() {
        let s = schema();
        let words = |v: &Value| -> Vec<String> { v["enum"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_owned()).collect() };
        // Every listed word parses to a variant other than the fallback unless it is the fallback's own name.
        let spec_json = |field: &str, value: &str| format!("{{\"{field}\":\"{value}\"}}");
        for kind in words(&s["properties"]["kind"]) {
            let k: MidiSpec = serde_json::from_str(&spec_json("kind", &kind)).unwrap();
            assert_eq!(serde_json::to_value(k.kind).unwrap(), json!(kind));
        }
        for g in words(&s["properties"]["genre"]) {
            let k: MidiSpec = serde_json::from_str(&spec_json("genre", &g)).unwrap();
            assert_eq!(serde_json::to_value(k.genre).unwrap(), json!(g));
        }
        for f in words(&s["properties"]["feel"]) {
            let k: MidiSpec = serde_json::from_str(&spec_json("feel", &f)).unwrap();
            assert_eq!(serde_json::to_value(k.feel).unwrap(), json!(f));
        }
        let layer = &s["properties"]["layers"]["items"]["properties"];
        for p in words(&layer["pattern"]) {
            let l: Layer = serde_json::from_str(&spec_json("pattern", &p)).unwrap();
            assert_eq!(serde_json::to_value(l.pattern).unwrap(), json!(p));
        }
        for r in words(&layer["role"]) {
            let l: Layer = serde_json::from_str(&spec_json("role", &r)).unwrap();
            assert_eq!(serde_json::to_value(l.role).unwrap(), json!(r));
        }
        for q in words(&s["properties"]["chords"]["items"]["properties"]["quality"]) {
            let c: ChordSpec = serde_json::from_str(&spec_json("quality", &q)).unwrap();
            assert_eq!(serde_json::to_value(c.quality).unwrap(), json!(q));
        }
        assert_eq!(words(&s["properties"]["kind"]).len(), 6);
        assert_eq!(words(&layer["role"]).len(), 12);
        assert_eq!(words(&layer["pattern"]).len(), 15);
        assert_eq!(words(&s["properties"]["chords"]["items"]["properties"]["quality"]).len(), 17);
    }

    #[test]
    fn the_instructions_name_every_pattern_and_the_examples_parse() {
        for p in Pattern::ALL {
            let name = serde_json::to_value(p).unwrap();
            let name = name.as_str().unwrap();
            if name != "custom" {
                assert!(INSTRUCTIONS.contains(name) || INSTRUCTIONS.contains(&name.replace('_', " ")), "{name} is not described");
            }
        }
        for example in ["Minimal bass example: ", "Minimal chords example: "] {
            let start = INSTRUCTIONS.find(example).unwrap() + example.len();
            let json = INSTRUCTIONS[start..].lines().next().unwrap();
            let spec = parse_reply(json).unwrap_or_else(|| panic!("{example} does not parse"));
            assert!(spec.has_content() && spec.notes.len() == 1);
            // And it goes through realize and validate cleanly.
            let req = GenRequest { kind: spec.kind, bars: spec.bars, ..GenRequest::new(spec.kind) };
            let g = crate::generate::generate_from_spec(&req, &crate::generate::GenContext::default(), spec, 3);
            assert_eq!(g.source, crate::generate::Source::Model, "{example} needed the fallback: {:?}", g.fallback);
            assert!(!g.draft.notes.is_empty());
        }
    }

    #[test]
    fn a_reply_is_read_bare_or_fenced_and_junk_is_refused() {
        let bare = r#"{"kind":"bass","genre":"techno","bars":2,"feel":"straight","energy":"mid","chords":[{"bar":0,"beat":0,"beats":8,"root":0,"alter":0,"quality":"min7","ext":[]}],"layers":[{"role":"bass","pattern":"rolling","steps":"","accents":"","pitches":["r"],"contour":"arch","motif":[],"gate":"mid","octave":0}],"notes":["x"]}"#;
        assert_eq!(parse_reply(bare).unwrap().genre, Genre::Techno);
        assert_eq!(parse_reply(&format!("Here you go:\n```json\n{bare}\n```")).unwrap().bars, 2);
        assert!(parse_reply("I would write a bass, but I cannot.").is_none());
        assert!(parse_reply("{\"kind\": 7}").is_none());
    }

    #[test]
    fn the_prompt_carries_the_request_and_what_to_avoid() {
        let req = GenRequest { genre: Genre::TechHouse, bars: 2, pattern: Some(Pattern::Rolling), free_text: "un bajo rodante".into(), only: vec![Role::Bass], ..GenRequest::new(Kind::Bass) };
        let p = user_prompt(&req, None);
        assert!(p.contains("bass part, 2 bar") && p.contains("tech house") && p.contains("rolling") && p.contains("un bajo rodante"), "{p}");
        let prev = MidiSpec { kind: Kind::Bass, layers: vec![Layer::new(Role::Bass, Pattern::Rolling)], ..Default::default() };
        assert!(user_prompt(&req, Some(&prev)).contains("clearly different"));
        assert_eq!(chord_line(&[ChordSpec { root: 4, alter: 1, ..Default::default() }]), "5*");
    }
}
