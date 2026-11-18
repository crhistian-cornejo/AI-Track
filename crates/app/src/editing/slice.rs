//! Slice to Drum Rack: the audio clip menu entry, the small dialog that picks
//! how to cut (transients with a sensitivity, a beat division, the warp
//! markers) and the action that finds the cuts and hands them to
//! `aitrack_model::slice`, which builds the track as one undoable step.

use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::*;
use crate::widgets::{self, menu_header, menu_item};
use crate::workspace::{MenuKind, Workspace};
use aitrack_model::slice::{self, BeatDivision, SliceMode};
use aitrack_model::{PlacementId, Source, TrackId};
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, ClickEvent, Context, ElementId, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, px};
use std::cell::Cell;

/// How the dialog cuts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cut {
    Transients,
    Beats,
    Markers,
}

/// What the dialog has picked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliceChoice {
    pub cut: Cut,
    /// Transient sensitivity 0..=1.
    pub sensitivity: f32,
    pub division: BeatDivision,
}

impl SliceChoice {
    pub const DEFAULT: Self = Self { cut: Cut::Transients, sensitivity: 0.5, division: BeatDivision::Sixteenth };

    pub fn mode(self) -> SliceMode {
        match self.cut {
            Cut::Transients => SliceMode::Transients { sensitivity: self.sensitivity },
            Cut::Beats => SliceMode::Beats(self.division),
            Cut::Markers => SliceMode::WarpMarkers,
        }
    }
}

thread_local! {
    /// The dialog's choice, kept between openings (UI thread only).
    static CHOICE: Cell<SliceChoice> = const { Cell::new(SliceChoice::DEFAULT) };
}

pub fn choice() -> SliceChoice {
    CHOICE.with(Cell::get)
}

pub fn set_choice(c: SliceChoice) {
    CHOICE.with(|x| x.set(c));
}

/// What a slicing made, for the toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sliced {
    pub track: TrackId,
    pub slices: usize,
    pub capped: bool,
}

impl AppState {
    /// The audio clip `placement` of `track` can be sliced (it is an audio
    /// clip over a file of known length).
    pub fn can_slice(&self, track: TrackId, placement: PlacementId) -> bool {
        self.audio_of(track, placement).is_some_and(|a| a.frames > 0)
    }

    fn audio_of(&self, track: TrackId, placement: PlacementId) -> Option<&aitrack_model::AudioSource> {
        let pl = self.project.track(track)?.arrangement.iter().find(|p| p.id == placement)?;
        match self.project.sources.get(&self.project.clips.get(&pl.clip)?.source)? {
            Source::Audio(a) => Some(a),
            _ => None,
        }
    }

    /// Whether the clip has warp markers to cut at.
    pub fn has_warp_markers(&self, track: TrackId, placement: PlacementId) -> bool {
        self.audio_of(track, placement).is_some_and(|a| !a.warp.markers.is_empty())
    }

    /// Slice the audio clip into a new MIDI track holding a Drum Rack, one
    /// undo step; the new track is selected.
    pub fn slice_to_drum_rack(&mut self, track: TrackId, placement: PlacementId, mode: SliceMode) -> Result<Sliced, String> {
        let a = self.audio_of(track, placement).ok_or("Select an audio clip to slice")?;
        let mut transients = Vec::new();
        if let SliceMode::Transients { sensitivity } = mode {
            let (channels, rate) = aitrack_engine::sample_cache::decode_file(&a.path).map_err(|e| format!("Could not read the audio: {e}"))?;
            let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
            transients = aitrack_dsp::warp::detect_transients_with(&refs, rate, sensitivity);
        }
        let built = slice::slice_to_drum_rack(&mut self.project, track, placement, mode, &transients).map_err(|e| e.to_string())?;
        self.seal();
        if !self.edit("Slice to Drum Rack", built.commands) {
            return Err("The slices could not be added".into());
        }
        self.seal();
        self.selected_track = Some(built.track);
        Ok(Sliced { track: built.track, slices: built.slices, capped: built.capped })
    }
}

/// The toast for a finished slicing.
pub fn done_text(s: Sliced) -> String {
    if s.capped {
        format!("Sliced into {} pads (the most one Drum Rack takes; the last runs to the end)", s.slices)
    } else {
        format!("Sliced into {} pads on a new MIDI track", s.slices)
    }
}

/// The audio clip the dialog and the command act on: the selected one.
pub fn target(s: &AppState) -> Option<(TrackId, PlacementId)> {
    s.selected_placement.filter(|&(t, p)| s.can_slice(t, p))
}

/// The Cmd+K command and anything else that has no menu position: open the
/// dialog on the selected audio clip.
pub fn open_for_selection(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    match target(&ws.s) {
        Some((track, placement)) => ws.open_menu(MenuKind::SliceToDrumRack(track, placement), gpui::point(px(420.), px(200.)), cx),
        None => ws.toast("Select an audio clip to slice it into a Drum Rack", cx),
    }
}

/// The dialog: how to cut, then Slice.
pub fn slice_menu(ws: &mut Workspace, track: TrackId, placement: PlacementId, cx: &mut Context<Workspace>) -> AnyElement {
    let now = choice();
    let markers = ws.s.has_warp_markers(track, placement);
    let mut card = widgets::menu_card("slice-menu", 232.).child(menu_header("SLICE TO DRUM RACK")).child(menu_header("CUT AT"));
    let rows = [(Cut::Transients, "Transients", Icon::Waveform), (Cut::Beats, "Beat division", Icon::Grid), (Cut::Markers, "Warp markers", Icon::Split)];
    for (i, (cut, text, icon)) in rows.into_iter().enumerate() {
        if cut == Cut::Markers && !markers {
            continue;
        }
        card = card.child(menu_item(ElementId::Name(format!("slice-cut-{i}").into()), Some(icon), text, None, now.cut == cut).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
            set_choice(SliceChoice { cut, ..choice() });
            cx.notify();
        })));
    }
    card = card.child(widgets::divider_h());
    match now.cut {
        Cut::Transients => {
            card = card.child(menu_header("SENSITIVITY"));
            for (i, (text, value)) in [("Low", 0.25), ("Medium", 0.5), ("High", 0.8)].into_iter().enumerate() {
                card = card.child(menu_item(ElementId::Name(format!("slice-sens-{i}").into()), None, text, None, (now.sensitivity - value).abs() < 0.05).on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| {
                        set_choice(SliceChoice { sensitivity: value, ..choice() });
                        cx.notify();
                    },
                )));
            }
        }
        Cut::Beats => {
            card = card.child(menu_header("DIVISION"));
            let mut row = div().px(px(SP2)).pb(px(SP1)).flex().gap(px(SP1));
            for (i, division) in BeatDivision::ALL.into_iter().enumerate() {
                let on = now.division == division;
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("slice-div-{i}").into()))
                        .h(px(24.))
                        .px(px(SP2))
                        .flex()
                        .items_center()
                        .rounded(px(r_sm()))
                        .cursor_pointer()
                        .bg(if on { p().accent } else { content(0.06) })
                        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
                        .child(widgets::mono(division.label(), TEXT_XS, if on { p().on_accent } else { p().text }))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            set_choice(SliceChoice { division, ..choice() });
                            cx.notify();
                        })),
                );
            }
            card = card.child(row);
        }
        Cut::Markers => {}
    }
    card.child(widgets::divider_h())
        .child(menu_item("slice-go", Some(Icon::Drum), "Slice to New MIDI Track", None, false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menu = None;
            run(this, track, placement, choice().mode(), cx);
        })))
        .into_any_element()
}

/// Slice and say what happened.
pub fn run(ws: &mut Workspace, track: TrackId, placement: PlacementId, mode: SliceMode, cx: &mut Context<Workspace>) {
    match ws.s.slice_to_drum_rack(track, placement, mode) {
        Ok(done) => {
            ws.set_tab(crate::state::BottomTab::Devices, cx);
            ws.toast(done_text(done), cx);
        }
        Err(why) => ws.toast(why, cx),
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::*;

    /// A one-bar loop file (four clicks, one per beat at 120 BPM) and a song with it on an audio track.
    fn song(name: &str) -> (AppState, TrackId, PlacementId, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("aitrack-app-slice-{}-{name}.wav", std::process::id()));
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..96_000usize {
            let t = (i % 24_000) as f32 / 48_000.0;
            w.write_sample((12_000.0 * (-t * 60.0).exp() * (t * 3_000.0).sin()) as i16).unwrap();
        }
        w.finalize().unwrap();
        let mut p = Project::new("t");
        let (track, source, clip, pl) = (p.alloc_track_id(), p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "Loop", Color::palette(1))), index: 0 }.apply(&mut p).unwrap();
        let a = AudioSource { path: path.clone(), hash: None, sample_rate: 48_000, channels: 1, frames: 96_000, warp: Warp::default(), key: None };
        Command::AddSource { id: source, source: Box::new(Source::Audio(a)) }.apply(&mut p).unwrap();
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Loop", Color::palette(1), source, 4 * PPQ)) }.apply(&mut p).unwrap();
        Command::PlaceClip { track, placement: Placement { id: pl, clip, start: 0, lane: 0 } }.apply(&mut p).unwrap();
        (AppState::new(p), track, pl, path)
    }

    #[test]
    fn slicing_by_transients_finds_the_clicks_and_undoes_in_one_step() {
        let (mut s, track, pl, path) = song("transients");
        let before = s.project.clone();
        let done = s.slice_to_drum_rack(track, pl, SliceChoice::DEFAULT.mode()).unwrap();
        assert_eq!(done.slices, 4, "one slice per click");
        assert!(!done.capped);
        assert_eq!(s.selected_track, Some(done.track));
        let t = s.project.track(done.track).unwrap();
        assert_eq!(t.kind, TrackKind::Instrument);
        let Some(Source::Notes(n)) = s.project.sources.get(&s.project.clips[&t.arrangement[0].clip].source) else { panic!("notes") };
        let eighth = PPQ / 8;
        let ticks: Vec<_> = n.notes.iter().map(|x| (x.start + eighth / 2) / eighth).collect();
        assert_eq!(ticks, vec![0, 8, 16, 24], "hit on the beats (the detector snaps a couple of ms early)");
        assert!(s.undo());
        assert_eq!(s.project.tracks, before.tracks, "one undo removes the track, the clip and the notes");
        assert_eq!(s.project.clips, before.clips);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn slicing_needs_an_audio_clip_and_a_readable_file() {
        let (mut s, track, pl, path) = song("errors");
        let _ = std::fs::remove_file(&path);
        assert!(s.slice_to_drum_rack(track, pl, SliceMode::Transients { sensitivity: 0.5 }).is_err(), "the file is gone");
        assert!(s.slice_to_drum_rack(track, pl, SliceMode::WarpMarkers).is_err(), "no markers");
        assert!(s.slice_to_drum_rack(track, pl, SliceMode::Beats(BeatDivision::Quarter)).is_ok(), "a beat grid needs no file read");
    }

    #[test]
    fn the_dialog_remembers_what_it_was_given() {
        set_choice(SliceChoice { cut: Cut::Beats, division: BeatDivision::Eighth, ..SliceChoice::DEFAULT });
        assert_eq!(choice().mode(), SliceMode::Beats(BeatDivision::Eighth));
        set_choice(SliceChoice::DEFAULT);
        assert_eq!(choice().mode(), SliceMode::Transients { sensitivity: 0.5 });
    }

    #[test]
    fn a_midi_rack_is_added_among_the_midi_effects() {
        let (mut s, _, _, path) = song("midirack");
        let inst = s.add_track(TrackKind::Instrument).unwrap();
        let synth = s.add_stock_device(inst, "instrument.synth", "Synth").unwrap();
        let arp = s.add_stock_device(inst, "midi.arp", "Arp").unwrap();
        let rack = s.add_stock_device(inst, "rack.midi", "MIDI Effect Rack").unwrap();
        let order: Vec<_> = s.project.track(inst).unwrap().devices.iter().map(|d| d.id).collect();
        assert_eq!(order, vec![arp, rack, synth], "after the last MIDI effect, in front of the instrument");
        assert!(matches!(&s.project.device(rack).unwrap().kind, DeviceKind::Rack(r) if r.kind == RackKind::Midi));
        let _ = std::fs::remove_file(path);
    }
}
