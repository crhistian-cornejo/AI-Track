//! The `/` menu: every assistant action, grouped in tabs and filtered by
//! what follows the slash.

use crate::bridge::MidiMode;
use crate::icons::Icon;

/// Tabs of the `/` menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    All,
    Audio,
    Midi,
    Mix,
    Library,
    Chat,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::All, Tab::Audio, Tab::Midi, Tab::Mix, Tab::Library, Tab::Chat];

    pub fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Audio => "Audio",
            Tab::Midi => "MIDI",
            Tab::Mix => "Mix",
            Tab::Library => "Library",
            Tab::Chat => "Chat",
        }
    }

    /// The next tab (Tab key), wrapping.
    pub fn next(self) -> Tab {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// What an action does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ExtractMidi(MidiMode),
    SplitStems,
    ImportAudio,
    AnalyseKeyBpm,
    FindSamplesInKey,
    MixTrack,
    AttachSelection,
    AttachFile,
    Overview,
    ClearChat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub action: Action,
    /// What you type after the slash.
    pub command: &'static str,
    pub title: &'static str,
    pub hint: &'static str,
    pub icon: Icon,
    pub tab: Tab,
    /// Extra words that find it.
    pub keywords: &'static [&'static str],
}

pub static ENTRIES: &[Entry] = &[
    Entry { action: Action::ExtractMidi(MidiMode::Polyphonic), command: "midi", title: "Extract MIDI", hint: "Selected audio clip, polyphonic (basic-pitch)", icon: Icon::Extract, tab: Tab::Midi, keywords: &["audio to midi", "transcribe", "notes", "poly", "chords"] },
    Entry { action: Action::ExtractMidi(MidiMode::Monophonic), command: "midi-mono", title: "Extract MIDI · Mono", hint: "One note at a time: bass, lead, vocal", icon: Icon::Extract, tab: Tab::Midi, keywords: &["melody", "monophonic", "bassline", "transcribe"] },
    Entry { action: Action::ExtractMidi(MidiMode::Drums), command: "midi-drums", title: "Extract MIDI · Drums", hint: "Kick, snare and hats as drum notes", icon: Icon::Drum, tab: Tab::Midi, keywords: &["beat", "groove", "drums to midi", "transcribe"] },
    Entry { action: Action::SplitStems, command: "stems", title: "Split stems", hint: "Vocals · drums · bass · other", icon: Icon::Split, tab: Tab::Audio, keywords: &["separate", "demucs", "isolate", "acapella", "vocals"] },
    Entry { action: Action::ImportAudio, command: "import", title: "Import audio", hint: "New track, detects key and BPM", icon: Icon::FolderOpen, tab: Tab::Audio, keywords: &["file", "add", "open", "load"] },
    Entry { action: Action::AnalyseKeyBpm, command: "analyse", title: "Analyse key / BPM", hint: "Of the selected audio clip", icon: Icon::Gauge, tab: Tab::Audio, keywords: &["analyze", "tempo", "detect", "key", "bpm", "scale"] },
    Entry { action: Action::FindSamplesInKey, command: "samples", title: "Find samples in key", hint: "From the library, in the project key", icon: Icon::Search, tab: Tab::Library, keywords: &["browse", "loops", "one-shots", "library", "match"] },
    Entry { action: Action::MixTrack, command: "mix", title: "Mix this track", hint: "Fader, pan, EQ and compression as a proposal", icon: Icon::Sliders, tab: Tab::Mix, keywords: &["eq", "compress", "balance", "level", "mixdown", "engineer"] },
    Entry { action: Action::AttachSelection, command: "attach", title: "Attach selection", hint: "The selected track and clip as context", icon: Icon::Link, tab: Tab::Chat, keywords: &["context", "track", "clip", "selected"] },
    Entry { action: Action::AttachFile, command: "file", title: "Attach a file", hint: "A sample or a screenshot", icon: Icon::File, tab: Tab::Chat, keywords: &["image", "screenshot", "picture", "sample", "upload"] },
    Entry { action: Action::Overview, command: "overview", title: "Project overview", hint: "What the project has and what to try next", icon: Icon::Dashboard, tab: Tab::Chat, keywords: &["summary", "ideas", "feedback", "review", "arrangement"] },
    Entry { action: Action::ClearChat, command: "clear", title: "Clear chat", hint: "Start a new conversation", icon: Icon::Trash, tab: Tab::Chat, keywords: &["reset", "new", "empty"] },
];

/// The query of an input that opens the menu: the text after a leading `/`.
pub fn query(input: &str) -> Option<&str> {
    input.strip_prefix('/')
}

/// How well `entry` matches `q` (higher is better); `None` hides it.
fn score(entry: &Entry, q: &str) -> Option<u32> {
    if q.is_empty() {
        return Some(1);
    }
    let title = entry.title.to_lowercase();
    if entry.command == q {
        return Some(100);
    }
    if entry.command.starts_with(q) {
        return Some(90 - entry.command.len().min(40) as u32);
    }
    if title.starts_with(q) {
        return Some(70);
    }
    if title.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(q)) {
        return Some(60);
    }
    if entry.keywords.iter().any(|k| k.starts_with(q) || k.split(' ').any(|w| w.starts_with(q))) {
        return Some(40);
    }
    if title.contains(q) || entry.hint.to_lowercase().contains(q) {
        return Some(25);
    }
    // Letters in order (`xm` → "eXtract Midi").
    let mut chars = title.chars();
    q.chars().filter(|c| !c.is_whitespace()).all(|c| chars.any(|t| t == c)).then_some(10)
}

/// The entries for `query` in `tab`, best first (stable for ties).
pub fn filter(query: &str, tab: Tab) -> Vec<&'static Entry> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(u32, usize, &'static Entry)> =
        ENTRIES.iter().enumerate().filter(|(_, e)| tab == Tab::All || e.tab == tab).filter_map(|(i, e)| score(e, &q).map(|s| (s, i, e))).collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, _, e)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(q: &str, tab: Tab) -> Vec<Action> {
        filter(q, tab).iter().map(|e| e.action).collect()
    }

    #[test]
    fn only_a_leading_slash_opens_the_menu() {
        assert_eq!(query("/mi"), Some("mi"));
        assert_eq!(query("/"), Some(""));
        assert_eq!(query("mix /this"), None);
    }

    #[test]
    fn empty_query_lists_everything_in_order() {
        assert_eq!(filter("", Tab::All).len(), ENTRIES.len());
        assert_eq!(filter("", Tab::All)[0].action, ENTRIES[0].action);
    }

    #[test]
    fn exact_command_wins() {
        assert_eq!(actions("mix", Tab::All)[0], Action::MixTrack);
        assert_eq!(actions("midi", Tab::All)[0], Action::ExtractMidi(MidiMode::Polyphonic));
        assert_eq!(actions("STEMS", Tab::All)[0], Action::SplitStems);
    }

    #[test]
    fn prefixes_words_and_keywords_match() {
        let midi = actions("mid", Tab::All);
        assert_eq!(&midi[..3], &[Action::ExtractMidi(MidiMode::Polyphonic), Action::ExtractMidi(MidiMode::Monophonic), Action::ExtractMidi(MidiMode::Drums)]);
        assert_eq!(actions("bpm", Tab::All)[0], Action::AnalyseKeyBpm);
        assert!(actions("vocals", Tab::All).contains(&Action::SplitStems));
        assert!(actions("screenshot", Tab::All).contains(&Action::AttachFile));
        assert_eq!(actions("xm", Tab::All)[0], Action::ExtractMidi(MidiMode::Polyphonic), "letters in order");
        assert!(actions("zzzz", Tab::All).is_empty());
    }

    #[test]
    fn tabs_narrow_the_list() {
        assert!(filter("", Tab::Midi).iter().all(|e| e.tab == Tab::Midi));
        assert_eq!(filter("", Tab::Mix).len(), 1);
        assert!(actions("mix", Tab::Audio).is_empty());
        assert_eq!(Tab::Chat.next(), Tab::All);
    }

    #[test]
    fn commands_are_unique() {
        let mut c: Vec<_> = ENTRIES.iter().map(|e| e.command).collect();
        c.sort();
        c.dedup();
        assert_eq!(c.len(), ENTRIES.len());
    }
}
