//! What the status bar says about the project: the selection in words, the
//! save state and the audio device in short. Pure text; `Workspace::render_status`
//! lays it out.

use crate::state::AppState;
use aitrack_model::{Color, Source, TrackKind};

fn kind_name(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Instrument => "Instrument track",
        TrackKind::Audio => "Audio track",
        TrackKind::Group => "Group",
        TrackKind::Folder => "Folder",
        TrackKind::Return => "Return",
        TrackKind::Vca => "VCA",
        #[allow(unreachable_patterns)]
        _ => "Track",
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A length in bars of the project's signature at the start: `4 bars`, `1 bar`, `2.5 bars`.
fn bars(s: &AppState, ticks: i64) -> String {
    let bar = s.project.tempo.signature_at(0).bar_ticks().max(1);
    if ticks % bar == 0 {
        count((ticks / bar) as usize, "bar", "bars")
    } else {
        format!("{:.2} bars", ticks as f64 / bar as f64).replace(".00", "")
    }
}

/// The selection in words, with the colour of what it names: the clip being
/// edited (its track, length and notes), else the selected track.
pub fn selection(s: &AppState) -> Option<(Color, String)> {
    let p = &s.project;
    if let Some((id, clip)) = s.selected_clip.and_then(|c| p.clips.get(&c).map(|clip| (c, clip))) {
        let track = s.clip_track(id).and_then(|t| p.track(t)).map(|t| format!("{} › ", t.name)).unwrap_or_default();
        let what = match p.sources.get(&clip.source) {
            Some(Source::Notes(n)) => {
                let picked = s.selected_notes.len();
                let notes = count(n.notes.len(), "note", "notes");
                if picked > 0 { format!("{notes}, {picked} selected") } else { notes }
            }
            Some(Source::Audio(_)) => "audio".to_owned(),
            None => String::new(),
        };
        let mut text = format!("{track}{} · {}", clip.name, bars(s, clip.length));
        if !what.is_empty() {
            text.push_str(" · ");
            text.push_str(&what);
        }
        return Some((clip.color, text));
    }
    let t = s.selected_track.and_then(|t| p.track(t))?;
    let mut text = format!("{} · {}", t.name, kind_name(t.kind));
    if t.kind.has_clips() {
        text.push_str(&format!(" · {}", count(t.arrangement.len() + t.session_slots.len(), "clip", "clips")));
    }
    if !t.devices.is_empty() {
        text.push_str(&format!(" · {}", count(t.devices.len(), "device", "devices")));
    }
    Some((t.color, text))
}

/// Whether the project needs saving, in the bar's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// Never saved to a file.
    Unsaved,
    /// Changed since it was last saved.
    Edited,
    Saved,
}

impl SaveState {
    pub fn of(has_file: bool, dirty: bool) -> Self {
        match (has_file, dirty) {
            (_, true) if has_file => Self::Edited,
            (false, _) => Self::Unsaved,
            _ => Self::Saved,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unsaved => "Not saved",
            Self::Edited => "Edited",
            Self::Saved => "Saved",
        }
    }
}

/// The engine's status line without its host name (`CoreAudio · Speakers · 48 kHz · 256`
/// → `Speakers · 48 kHz · 256`): the bar is short and the host is in Preferences.
pub fn device_short(status: &str) -> &str {
    match status.split_once(" · ") {
        Some((host, rest)) if rest.contains(" · ") && !host.contains(' ') => rest,
        _ => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selection_names_the_clip_then_the_track() {
        let mut s = AppState::new(crate::demo::project());
        s.selected_clip = None;
        s.selected_track = None;
        assert_eq!(selection(&s), None);
        let lead = s.project.tracks.iter().find(|t| t.name == "Lead").expect("the demo's Lead track");
        let (lead_id, lead_color, clip) = (lead.id, lead.color, lead.arrangement[0].clip);
        s.selected_track = Some(lead_id);
        let (color, text) = selection(&s).unwrap();
        assert_eq!(color, lead_color);
        assert!(text.starts_with("Lead · Instrument track · "), "{text}");
        assert!(text.contains("clips") && text.contains("device"), "{text}");
        s.selected_clip = Some(clip);
        let (_, text) = selection(&s).unwrap();
        assert!(text.starts_with("Lead › Hook · "), "{text}");
        assert!(text.contains("bar") && text.ends_with("notes"), "{text}");
        let first = match s.project.sources.get(&s.project.clips[&clip].source) {
            Some(Source::Notes(n)) => n.notes[0].id,
            _ => unreachable!(),
        };
        s.selected_notes.insert(first);
        assert!(selection(&s).unwrap().1.ends_with("notes, 1 selected"));
    }

    #[test]
    fn save_states_and_the_short_device_line() {
        assert_eq!(SaveState::of(false, false), SaveState::Unsaved);
        assert_eq!(SaveState::of(false, true), SaveState::Unsaved);
        assert_eq!(SaveState::of(true, true), SaveState::Edited);
        assert_eq!(SaveState::of(true, false), SaveState::Saved);
        assert_eq!(device_short("CoreAudio · Altavoces del MacBook Air · 48 kHz · 256 · 7.6 ms out"), "Altavoces del MacBook Air · 48 kHz · 256 · 7.6 ms out");
        assert_eq!(device_short("No audio device"), "No audio device");
        assert_eq!(device_short("Engine: stub"), "Engine: stub");
    }
}
