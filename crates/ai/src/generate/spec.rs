//! The two small descriptions the generator works from.
//!
//! * [`GenRequest`]: what the user asked for (a part, a genre, how long...).
//! * [`MidiSpec`]: the musical intent for that part: chords with root,
//!   quality and extensions, rhythm as step strings with accent classes, a
//!   motif, a feel label. It holds no ticks, no velocities, no MIDI notes and
//!   no percentages: the code decides those.
//!
//! The rule-based templates and (later) a language model both produce a
//! `MidiSpec`, and it goes through the same `realize` and `validate`, so what
//! plays offline and online has the same guarantees. Everything is
//! `serde(default)` and unknown words fall back to a default, so a loose
//! model reply is trimmed by [`MidiSpec::clamped`] instead of rejected.

use aitrack_model::Scale;
use serde::{Deserialize, Serialize};

/// Bump when a change makes the same seed give other notes.
pub const GENERATOR_VERSION: u32 = 1;

/// The serialized names of every variant of an enum, as a schema lists them.
pub fn names<T: Serialize>(all: &[T]) -> Vec<String> {
    all.iter().filter_map(|v| serde_json::to_value(v).ok()).filter_map(|v| v.as_str().map(str::to_owned)).collect()
}

/// Limits applied by [`MidiSpec::clamped`].
pub const MAX_BARS: u32 = 16;
pub const MAX_CHORDS: usize = 16;
pub const MAX_LAYERS: usize = 8;
pub const MAX_MOTIF: usize = 16;
pub const MAX_NOTES: usize = 3;
pub const MAX_NOTE_CHARS: usize = 140;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Chords,
    Drums,
    Arp,
    Melody,
    Perc,
    #[default]
    #[serde(other)]
    Bass,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Chords, Kind::Bass, Kind::Drums, Kind::Arp, Kind::Melody, Kind::Perc];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Chords => "Chords",
            Kind::Bass => "Bass",
            Kind::Drums => "Drums",
            Kind::Arp => "Arp",
            Kind::Melody => "Melody",
            Kind::Perc => "Percussion",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Genre {
    House,
    DeepHouse,
    TechHouse,
    Techno,
    Trance,
    Dnb,
    Dubstep,
    Trap,
    UkGarage,
    Reggaeton,
    Pop,
    #[default]
    #[serde(other)]
    Other,
}

impl Genre {
    pub const ALL: [Genre; 12] = [
        Genre::House,
        Genre::DeepHouse,
        Genre::TechHouse,
        Genre::Techno,
        Genre::Trance,
        Genre::Dnb,
        Genre::Dubstep,
        Genre::Trap,
        Genre::UkGarage,
        Genre::Reggaeton,
        Genre::Pop,
        Genre::Other,
    ];

    /// The genre the templates actually use: `Other` plays as house.
    pub fn style(self) -> Genre {
        if self == Genre::Other { Genre::House } else { self }
    }

    pub fn label(self) -> &'static str {
        match self {
            Genre::House => "house",
            Genre::DeepHouse => "deep house",
            Genre::TechHouse => "tech house",
            Genre::Techno => "techno",
            Genre::Trance => "trance",
            Genre::Dnb => "drum and bass",
            Genre::Dubstep => "dubstep",
            Genre::Trap => "trap",
            Genre::UkGarage => "UK garage",
            Genre::Reggaeton => "reggaeton",
            Genre::Pop => "pop",
            Genre::Other => "house",
        }
    }
}

/// How loose the groove is. The code turns each label into a Linn swing
/// percentage for the genre; the model never writes percentages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feel {

    LightSwing,
    Swing,
    Shuffle,
    Loose,
    #[default]
    #[serde(other)]
    Straight,
}

impl Feel {
    pub const ALL: [Feel; 5] = [Feel::Straight, Feel::LightSwing, Feel::Swing, Feel::Shuffle, Feel::Loose];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Energy {
    Low,
    High,
    #[default]
    #[serde(other)]
    Mid,
}

impl Energy {
    pub const ALL: [Energy; 3] = [Energy::Low, Energy::Mid, Energy::High];

    pub fn level(self) -> f32 {
        match self {
            Energy::Low => 0.2,
            Energy::Mid => 0.5,
            Energy::High => 0.85,
        }
    }

    pub fn from_level(x: f32) -> Self {
        if x < 0.35 {
            Energy::Low
        } else if x > 0.7 {
            Energy::High
        } else {
            Energy::Mid
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {

    Sub,
    Chord,
    Arp,
    Lead,
    Kick,
    Snare,
    Clap,
    Hat,
    OpenHat,
    Perc,
    Crash,
    #[default]
    #[serde(other)]
    Bass,
}

impl Role {
    pub const ALL: [Role; 12] = [Role::Bass, Role::Sub, Role::Chord, Role::Arp, Role::Lead, Role::Kick, Role::Snare, Role::Clap, Role::Hat, Role::OpenHat, Role::Perc, Role::Crash];
    pub const DRUMS: [Role; 7] = [Role::Kick, Role::Snare, Role::Clap, Role::Hat, Role::OpenHat, Role::Perc, Role::Crash];

    pub fn is_drum(self) -> bool {
        Self::DRUMS.contains(&self)
    }

    /// Roles that play one note at a time.
    pub fn is_mono(self) -> bool {
        matches!(self, Role::Bass | Role::Sub | Role::Lead | Role::Arp)
    }

    pub fn id(self) -> u64 {
        self as u64 + 1
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Bass => "bass",
            Role::Sub => "sub",
            Role::Chord => "chords",
            Role::Arp => "arp",
            Role::Lead => "lead",
            Role::Kick => "kick",
            Role::Snare => "snare",
            Role::Clap => "clap",
            Role::Hat => "hats",
            Role::OpenHat => "open hat",
            Role::Perc => "percussion",
            Role::Crash => "crash",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    // Bass.
    Rolling,
    Offbeat,
    Octaves,
    RootFifth,
    Whole,
    Sub808,
    // Chords.
    StabsOffbeat,
    Sync332,
    Charleston,
    Pump8ths,
    // Arp order.
    Up,
    Down,
    UpDown,
    Random,
    #[default]
    #[serde(other)]
    Custom,
}

impl Pattern {
    pub const ALL: [Pattern; 15] = [Pattern::Rolling, Pattern::Offbeat, Pattern::Octaves, Pattern::RootFifth, Pattern::Whole, Pattern::Sub808, Pattern::StabsOffbeat, Pattern::Sync332, Pattern::Charleston, Pattern::Pump8ths, Pattern::Up, Pattern::Down, Pattern::UpDown, Pattern::Random, Pattern::Custom];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Contour {

    Rise,
    Fall,
    Wave,
    Flat,
    #[default]
    #[serde(other)]
    Arch,
}

impl Contour {
    pub const ALL: [Contour; 5] = [Contour::Arch, Contour::Rise, Contour::Fall, Contour::Wave, Contour::Flat];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Short,
    Long,
    #[default]
    #[serde(other)]
    Mid,
}

impl Gate {
    pub const ALL: [Gate; 3] = [Gate::Short, Gate::Mid, Gate::Long];

    /// The part of the gap to the next hit the note holds.
    pub fn fraction(self) -> f32 {
        match self {
            Gate::Short => 0.35,
            Gate::Mid => 0.6,
            Gate::Long => 0.95,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Maj,
    Min,
    Dim,
    Aug,
    Sus2,
    Sus4,
    Maj7,
    Dom7,
    Min9,
    Maj9,
    Dom9,
    Add9,
    Min6,
    Min11,
    Dim7,
    M7b5,
    #[default]
    #[serde(other)]
    Min7,
}

impl Quality {
    pub const ALL: [Quality; 17] = [Quality::Maj, Quality::Min, Quality::Dim, Quality::Aug, Quality::Sus2, Quality::Sus4, Quality::Maj7, Quality::Min7, Quality::Dom7, Quality::Min9, Quality::Maj9, Quality::Dom9, Quality::Add9, Quality::Min6, Quality::Min11, Quality::Dim7, Quality::M7b5];
}

/// One chord: the root is a degree of the 7-note parent scale plus an
/// alteration in semitones, so `V9` with the leading tone in a minor key or
/// `bII` in phrygian can be written down.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChordSpec {
    /// Bar of the clip, from 0.
    pub bar: u32,
    /// Beat inside the bar, from 0.
    pub beat: f32,
    pub beats: f32,
    /// Degree 0..=6 of the parent scale.
    pub root: u32,
    /// -1, 0 or +1 semitones.
    pub alter: i32,
    pub quality: Quality,
    /// Any of "9", "11", "13", "b9", "#11", "b13".
    pub ext: Vec<String>,
}

impl Default for ChordSpec {
    fn default() -> Self {
        Self { bar: 0, beat: 0., beats: 4., root: 0, alter: 0, quality: Quality::Min7, ext: Vec::new() }
    }
}

/// One line of the part: a role with its rhythm and pitch rules.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    pub role: Role,
    pub pattern: Pattern,
    /// One char per 1/16 step and bar; `.` is silence, `x` or a digit a hit.
    pub steps: String,
    /// Accent class 0..=4 for each step (same positions as `steps`).
    pub accents: String,
    /// Chord-relative pitches for the bass: r 3 5 7 8 a(pproach).
    pub pitches: Vec<String>,
    pub contour: Contour,
    /// Melody moves in scale degrees from the previous note.
    pub motif: Vec<i32>,
    pub gate: Gate,
    /// Octave shift.
    pub octave: i32,
    pub locked: bool,
}

impl Layer {
    pub fn new(role: Role, pattern: Pattern) -> Self {
        Self { role, pattern, ..Default::default() }
    }

    pub fn with_steps(mut self, steps: &str, accents: &str) -> Self {
        self.steps = steps.to_owned();
        self.accents = accents.to_owned();
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiSpec {
    pub kind: Kind,
    pub genre: Genre,
    pub bars: u32,
    pub feel: Feel,
    pub energy: Energy,
    pub chords: Vec<ChordSpec>,
    pub layers: Vec<Layer>,
    /// Short reasons shown on the card.
    pub notes: Vec<String>,
}

impl MidiSpec {
    /// Trim everything to what the generator accepts; never an error.
    pub fn clamped(mut self) -> Self {
        self.bars = self.bars.clamp(1, MAX_BARS);
        self.chords.truncate(MAX_CHORDS);
        let bars = self.bars;
        let span = bars as f32 * 16.;
        for c in &mut self.chords {
            c.bar = c.bar.min(bars - 1);
            c.beat = if c.beat.is_finite() { c.beat.clamp(0., 15.) } else { 0. };
            c.beats = if c.beats.is_finite() { c.beats.clamp(0.25, bars as f32 * 4.) } else { 4. };
            c.root = c.root.min(6);
            c.alter = c.alter.clamp(-1, 1);
            c.ext.truncate(3);
            c.ext.retain(|e| matches!(e.as_str(), "9" | "11" | "13" | "b9" | "#11" | "b13"));
        }
        self.layers.truncate(MAX_LAYERS);
        let max_chars = (span as usize).max(16);
        for l in &mut self.layers {
            l.steps = l.steps.chars().take(max_chars).filter(|c| *c == '.' || *c == 'x' || c.is_ascii_digit()).collect();
            l.accents = l.accents.chars().take(max_chars).filter(|c| *c == '.' || c.is_ascii_digit()).collect();
            l.pitches.truncate(16);
            l.pitches.retain(|p| matches!(p.as_str(), "r" | "3" | "5" | "7" | "8" | "a" | "b7"));
            l.motif.truncate(MAX_MOTIF);
            for m in &mut l.motif {
                *m = (*m).clamp(-4, 4);
            }
            l.octave = l.octave.clamp(-2, 2);
        }
        self.notes.truncate(MAX_NOTES);
        for n in &mut self.notes {
            *n = n.chars().take(MAX_NOTE_CHARS).collect();
        }
        self
    }

    /// Whether there is anything to realize.
    pub fn has_content(&self) -> bool {
        !self.layers.is_empty() || !self.chords.is_empty()
    }

    /// The spec in a model reply: the last JSON object in the text (bare or
    /// in a fenced block), trimmed. `None` when nothing parses or the spec
    /// is empty, so the caller falls back to a template.
    pub fn parse(text: &str) -> Option<MidiSpec> {
        let mut best = None;
        for body in json_candidates(text) {
            if let Ok(spec) = serde_json::from_str::<MidiSpec>(body)
                && spec.has_content()
            {
                best = Some(spec.clamped());
            }
        }
        best
    }
}

/// Slices of `text` that may be one JSON object: fenced blocks and balanced
/// `{...}` runs. Quadratic work is avoided by scanning once.
fn json_candidates(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let (mut depth, mut start, mut in_str, mut esc) = (0usize, None, false, false);
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            match (esc, b) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' if depth > 0 => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(s) = start.take()
                {
                    out.push(&text[s..=i]);
                }
            }
            _ => {}
        }
    }
    out
}

/// The user's request, after parsing words or reading the `/` command.
#[derive(Clone, Debug, PartialEq)]
pub struct GenRequest {
    pub kind: Kind,
    pub genre: Genre,
    pub bars: u32,
    /// Overrides the project key.
    pub key: Option<Scale>,
    /// 0 (sparse) ..= 1 (busy).
    pub density: f32,
    pub energy: Energy,
    /// `None`: the genre's usual feel.
    pub feel: Option<Feel>,
    /// Micro-timing and velocity jitter on (swing is part of the style and stays).
    pub humanize: bool,
    pub seed: Option<u64>,
    /// A named pattern the words asked for ("rolling", "offbeat"...).
    pub pattern: Option<Pattern>,
    /// For drums: only these roles (hats alone...). Empty: the whole kit.
    pub only: Vec<Role>,
    /// Put the notes in the selected clip instead of making a new one.
    pub replace: bool,
    pub free_text: String,
}

impl GenRequest {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            genre: Genre::Other,
            bars: 4,
            key: None,
            density: 0.5,
            energy: Energy::Mid,
            feel: None,
            humanize: true,
            seed: None,
            pattern: None,
            only: Vec::new(),
            replace: false,
            free_text: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_words_fall_back_to_defaults() {
        let s: MidiSpec = serde_json::from_str(r#"{"kind":"tuba","genre":"polka","feel":"wobbly","energy":"lots",
            "chords":[{"quality":"zzz","root":3}],"layers":[{"role":"kazoo","pattern":"weird","gate":"x","contour":"?"}]}"#)
            .unwrap();
        assert_eq!((s.kind, s.genre, s.feel, s.energy), (Kind::Bass, Genre::Other, Feel::Straight, Energy::Mid));
        assert_eq!(s.chords[0].quality, Quality::Min7);
        assert_eq!(s.layers[0].role, Role::Bass);
        assert_eq!(s.layers[0].pattern, Pattern::Custom);
    }

    #[test]
    fn names_round_trip() {
        let s = MidiSpec {
            kind: Kind::Chords,
            genre: Genre::DeepHouse,
            bars: 4,
            chords: vec![ChordSpec { quality: Quality::M7b5, ext: vec!["9".into()], ..Default::default() }],
            layers: vec![Layer::new(Role::OpenHat, Pattern::StabsOffbeat)],
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("deep_house") && json.contains("m7b5") && json.contains("open_hat"), "{json}");
        assert_eq!(serde_json::from_str::<MidiSpec>(&json).unwrap(), s);
    }

    #[test]
    fn parse_spec_robust() {
        // Prose around fenced JSON, and a second block that wins.
        let reply = "Idea:\n```json\n{\"kind\":\"bass\",\"layers\":[{\"role\":\"bass\",\"steps\":\"x\"}],\"bars\":99}\n```\nBetter:\n```json\n{\"kind\":\"chords\",\"bars\":2,\"chords\":[{\"root\":0}]}\n```";
        let s = MidiSpec::parse(reply).unwrap();
        assert_eq!((s.kind, s.bars), (Kind::Chords, 2));
        // Bare JSON, extra fields, braces inside strings.
        let s = MidiSpec::parse(r#"{"bars": 3, "extra": {"a": 1}, "notes": ["a } b"], "layers":[{"role":"kick"}]}"#).unwrap();
        assert_eq!(s.bars, 3);
        assert_eq!(s.notes, vec!["a } b".to_owned()]);
        // Nothing usable.
        assert!(MidiSpec::parse("no spec here").is_none());
        assert!(MidiSpec::parse("{\"bars\": 4}").is_none(), "empty spec");
        assert!(MidiSpec::parse("{\"bars\": \"four\", \"layers\": 3}").is_none(), "wrong types");
        assert!(MidiSpec::parse("{{{{ [[[ \"").is_none());
        assert!(MidiSpec::parse("").is_none());
    }

    #[test]
    fn huge_specs_are_trimmed_without_panicking() {
        let chords: Vec<_> = (0..1000).map(|i| format!("{{\"root\":{},\"alter\":{},\"beat\":1e9,\"beats\":-3,\"ext\":[\"9\",\"11\",\"13\",\"b9\",\"zz\"]}}", i % 40, i - 500)).collect();
        let steps = "x".repeat(10_000);
        let json = format!("{{\"bars\":500,\"chords\":[{}],\"layers\":[{{\"steps\":\"{steps}\",\"motif\":[100,-100,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18],\"octave\":50}}],\"notes\":[\"{}\",\"b\",\"c\",\"d\"]}}", chords.join(","), "z".repeat(1000));
        let s = MidiSpec::parse(&json).unwrap();
        assert_eq!(s.bars, MAX_BARS);
        assert_eq!(s.chords.len(), MAX_CHORDS);
        assert!(s.chords.iter().all(|c| c.root <= 6 && (-1..=1).contains(&c.alter) && c.ext.len() <= 3 && c.beat <= 15. && c.beats >= 0.25 && c.bar < MAX_BARS));
        assert_eq!(s.layers[0].steps.len(), (MAX_BARS * 16) as usize);
        assert_eq!(s.layers[0].motif.len(), MAX_MOTIF);
        assert!(s.layers[0].motif.iter().all(|m| m.abs() <= 4));
        assert_eq!(s.layers[0].octave, 2);
        assert_eq!(s.notes.len(), MAX_NOTES);
        assert_eq!(s.notes[0].chars().count(), MAX_NOTE_CHARS);
    }

    #[test]
    fn steps_keep_only_step_characters() {
        let s = MidiSpec { bars: 1, layers: vec![Layer::new(Role::Kick, Pattern::Custom).with_steps("x.<script>1 \n2", "9..a3")], ..Default::default() }.clamped();
        assert_eq!(s.layers[0].steps, "x.1 2".replace(' ', ""));
        assert_eq!(s.layers[0].accents, "9..3");
    }
}
