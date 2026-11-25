//! A generated MIDI part as `Command`s: one source holding the notes from
//! tick 0, one clip over it and its placement on a MIDI track (an existing
//! one, or a new track with a synth when none fits). The result goes to
//! `History::propose`, never straight to the project, so accepting it is one
//! undo step. The musical work is in `aitrack_ai::generate`; this file only
//! picks the target, finds free room on the timeline and builds the commands.

use aitrack_ai::generate::spec::Kind;
use aitrack_ai::generate::{GenContext, GenRequest, Generated, TargetKind, digest, request};
use aitrack_model::{Clip, ClipId, Color, Command, Device, Key, Note, NoteSource, Placement, Project, Source, Tick, Track, TrackId, TrackKind};

use crate::state::is_drum_track;

/// Where the part goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    Existing(TrackId),
    NewTrack,
    /// Put the notes in this clip of this track, instead of making a clip.
    Replace { track: TrackId, clip: ClipId },
}

/// A request read against the project: what to write, where, and the
/// context to write it in.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub req: GenRequest,
    pub ctx: GenContext,
    pub dest: Dest,
    /// Where the clip starts on the timeline (the existing clip's start when replacing).
    pub start: Tick,
}

fn bass_like(name: &str) -> bool {
    let n = name.to_lowercase();
    ["bass", "sub", "808", "bajo"].iter().any(|w| n.contains(w))
}

/// The MIDI track a part of `kind` goes to: the selected one when it fits,
/// else a fitting one by name or kind, else a new track (melodic parts only).
pub fn pick_target(p: &Project, selected: Option<TrackId>, kind: Kind) -> Result<Dest, &'static str> {
    let midi = |t: &&Track| t.kind == TrackKind::Instrument;
    let sel = selected.and_then(|id| p.track(id)).filter(midi);
    if matches!(kind, Kind::Drums | Kind::Perc) {
        return sel.filter(|t| is_drum_track(t)).or_else(|| p.tracks.iter().filter(midi).find(|t| is_drum_track(t))).map(|t| Dest::Existing(t.id)).ok_or("Select a drum rack track (or add one with a kit) and run that again.");
    }
    if let Some(t) = sel {
        return Ok(if is_drum_track(t) { Dest::NewTrack } else { Dest::Existing(t.id) });
    }
    let named = (kind == Kind::Bass).then(|| p.tracks.iter().filter(midi).find(|t| !is_drum_track(t) && bass_like(&t.name))).flatten();
    Ok(named.map_or(Dest::NewTrack, |t| Dest::Existing(t.id)))
}

/// The bar where the loop starts, else the first bar.
pub fn region_start(p: &Project) -> Tick {
    let bar = p.tempo.signature_at(0).bar_ticks().max(1);
    p.loop_region.map_or(0, |(s, _)| s.max(0) / bar * bar)
}

/// The first bar at or after `from` where a clip of `len` ticks fits on `track`.
pub fn free_start(p: &Project, track: Option<TrackId>, from: Tick, len: Tick) -> Tick {
    let bar = p.tempo.signature_at(from).bar_ticks().max(1);
    let Some(t) = track.and_then(|id| p.track(id)) else { return from };
    let mut start = from;
    for _ in 0..256 {
        let clash = t.arrangement.iter().filter_map(|pl| p.clips.get(&pl.clip).map(|c| (pl.start, pl.start + c.length))).find(|(s, e)| *s < start + len && *e > start);
        match clash {
            Some((_, end)) => start = (end + bar - 1) / bar * bar,
            None => break,
        }
    }
    start
}

fn note_name(key: u8) -> String {
    format!("{}{}", Key(key % 12).name(), key as i32 / 12 - 1)
}

/// The clip `track` plays that `Replace` may fill: the selected one, if it
/// holds notes, is on this track, and either the words asked to replace it
/// or it is empty. Gives the clip and where it starts on the timeline.
fn replace_target(p: &Project, track: TrackId, clip: Option<ClipId>, replace: bool) -> Option<(ClipId, Tick)> {
    let id = clip?;
    let c = p.clips.get(&id)?;
    let Some(Source::Notes(src)) = p.sources.get(&c.source) else { return None };
    if !replace && !src.notes.is_empty() {
        return None;
    }
    let t = p.track(track)?;
    if let Some(pl) = t.arrangement.iter().find(|pl| pl.clip == id) {
        return Some((id, pl.start));
    }
    t.session_slots.values().any(|s| *s == id).then_some((id, 0))
}

/// Read a request against the project. `kind` is `Some` for a
/// `/bass`-style command, `None` for free text, where the words must name
/// the part. `selected_clip` is what `Replace` may fill.
pub fn prepare(p: &Project, selected: Option<TrackId>, selected_clip: Option<ClipId>, kind: Option<Kind>, text: &str) -> Result<Prepared, String> {
    let start0 = region_start(p);
    let probe = digest(p, selected, start0, 4);
    let mut req: GenRequest = match kind {
        Some(k) => request::parse_for(k, text, &probe),
        None => request::parse(text, &probe).ok_or("I could not tell which part you want. Try /bass, /chords, /beat, /arp or /melody.")?,
    };
    let mut dest = pick_target(p, selected, req.kind).map_err(str::to_owned)?;
    let mut region = start0;
    if let Dest::Existing(track) = dest
        && let Some((clip, at)) = replace_target(p, track, selected_clip, req.replace)
        && let Some(c) = p.clips.get(&clip)
    {
        let bar = p.tempo.signature_at(at).bar_ticks().max(1);
        req.bars = (((c.length + bar - 1) / bar) as u32).clamp(1, 16);
        dest = Dest::Replace { track, clip };
        region = at;
    }
    let target = match dest {
        Dest::Existing(id) | Dest::Replace { track: id, .. } => Some(id),
        Dest::NewTrack => None,
    };
    let ctx = digest(p, target, region, req.bars);
    if req.kind == Kind::Perc && ctx.target.kind != TargetKind::Drums {
        return Err("Percussion needs a drum rack track.".into());
    }
    let clip_ticks = p.tempo.signature_at(region).bar_ticks().max(1) * req.bars as Tick;
    let start = if matches!(dest, Dest::Replace { .. }) { region } else { free_start(p, target, region, clip_ticks) };
    Ok(Prepared { req, ctx, dest, start })
}

/// The commands for `g`, and the card lines.
pub fn commands(p: &mut Project, g: &Generated, dest: Dest, start: Tick) -> (Vec<Command>, Vec<String>) {
    let mut cmds = Vec::new();
    let drums = matches!(g.spec.kind, Kind::Drums | Kind::Perc);
    let bar_ticks = p.tempo.signature_at(start).bar_ticks().max(1);
    let mut lines = Vec::new();
    let new_note = |p: &mut Project, n: &aitrack_ai::generate::DraftNote| {
        let mut note = Note::new(p.alloc_note_id(), n.key, n.start, n.length, n.velocity);
        note.slide = n.slide;
        note
    };
    let (placed_line, stamped) = match dest {
        Dest::Replace { clip, .. } => {
            let Some(c) = p.clips.get(&clip).cloned() else { return (cmds, lines) };
            let old: Vec<aitrack_model::NoteId> = match p.sources.get(&c.source) {
                Some(Source::Notes(n)) => n.notes.iter().map(|n| n.id).collect(),
                _ => Vec::new(),
            };
            // The clip keeps its length: what falls past its end is cut.
            let notes: Vec<Note> = g
                .draft
                .notes
                .iter()
                .filter(|n| n.start < c.length)
                .map(|n| {
                    let mut note = new_note(p, n);
                    note.length = note.length.min(c.length - note.start);
                    note
                })
                .collect();
            if !old.is_empty() {
                cmds.push(Command::RemoveNotes { source: c.source, ids: old.clone() });
            }
            cmds.push(Command::AddNotes { source: c.source, notes });
            (format!("Replaces the {} notes of \"{}\" · bar {}", old.len(), c.name, start / bar_ticks + 1), false)
        }
        Dest::Existing(_) | Dest::NewTrack => {
            let (track, name, color, new) = match dest {
                Dest::Existing(id) => {
                    let t = p.track(id);
                    (id, t.map_or_else(String::new, |t| t.name.clone()), t.map_or(Color::palette(0), |t| t.color), false)
                }
                _ => {
                    let id = p.alloc_track_id();
                    let name = g.spec.kind.label().to_owned();
                    let color = Color::palette(match g.spec.kind {
                        Kind::Bass => 3,
                        Kind::Chords => 5,
                        Kind::Arp => 7,
                        Kind::Melody => 9,
                        Kind::Drums | Kind::Perc => 1,
                    });
                    let mut t = Track::new(id, TrackKind::Instrument, name.clone(), color);
                    let dev = p.alloc_device_id();
                    t.devices.push(Device::stock(dev, "instrument.wavetable", "Wavetable Synth"));
                    let index = p.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(p.tracks.len().saturating_sub(1));
                    cmds.push(Command::AddTrack { track: Box::new(t), index });
                    (id, name, color, true)
                }
            };
            let notes: Vec<Note> = g.draft.notes.iter().map(|n| new_note(p, n)).collect();
            let (src, clip_id, placement) = (p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
            let mut clip = Clip::new(g.interpretation.split(" · ").take(2).collect::<Vec<_>>().join(" · "), color, src, g.clip_ticks);
            // Stamp the key on the clip only when the project has none of its own.
            let stamped = p.scale.is_none() && !drums;
            if stamped {
                clip.scale = Some(g.scale());
            }
            cmds.push(Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes, ..Default::default() })) });
            cmds.push(Command::AddClip { id: clip_id, clip: Box::new(clip) });
            cmds.push(Command::PlaceClip { track, placement: Placement { id: placement, clip: clip_id, start, lane: 0 } });
            let line = if new { format!("On a new track: {name} · bar {}", start / bar_ticks + 1) } else { format!("On {name} · bar {}", start / bar_ticks + 1) };
            (line, stamped)
        }
    };

    let count = g.draft.notes.len();
    let range = g.report.stats.lowest.zip(g.report.stats.highest).map(|(lo, hi)| format!(" · {}–{}", note_name(lo), note_name(hi))).unwrap_or_default();
    let feel = if g.report.stats.swing > 0.505 { format!("swing {:.0} %", g.report.stats.swing * 100.) } else { "straight".to_owned() };
    let noun = match (drums, count) {
        (true, 1) => "hit",
        (true, _) => "hits",
        (false, 1) => "note",
        (false, _) => "notes",
    };
    lines.push(format!("{count} {noun}{range} · {feel}"));
    lines.extend(g.spec.notes.iter().take(2).cloned());
    lines.push(placed_line);
    if stamped {
        lines.push(format!("The clip is in {}", g.scale().display()));
    }
    lines.extend(g.report.warnings.iter().take(1).cloned());
    (cmds, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_ai::generate::generate;
    use aitrack_model::{Chain, History, NoteId, Origin, PPQ, Rack, RackKind, Scale, ScaleKind, SourceId};

    struct Planned {
        label: String,
        commands: Vec<Command>,
        summary: Vec<String>,
        line: String,
    }

    fn plan_with_clip(p: &mut Project, selected: Option<TrackId>, clip: Option<ClipId>, kind: Option<Kind>, text: &str, seed: u64) -> Result<Planned, String> {
        let pr = prepare(p, selected, clip, kind, text)?;
        let g = generate(&pr.req, &pr.ctx, pr.req.seed.unwrap_or(seed));
        if g.draft.notes.is_empty() {
            return Err("nothing".into());
        }
        let (commands, summary) = commands(p, &g, pr.dest, pr.start);
        Ok(Planned { label: g.interpretation.clone(), commands, summary, line: g.interpretation })
    }

    fn plan(p: &mut Project, selected: Option<TrackId>, kind: Option<Kind>, text: &str, seed: u64) -> Result<Planned, String> {
        plan_with_clip(p, selected, None, kind, text, seed)
    }

    /// A MIDI clip of `bars` bars on `track` holding `keys` as one note each.
    fn add_clip_with(p: &mut Project, track: TrackId, bars: i64, keys: &[u8]) -> ClipId {
        let src: SourceId = p.alloc_source_id();
        let clip = p.alloc_clip_id();
        let notes: Vec<Note> = keys.iter().enumerate().map(|(i, k)| Note::new(NoteId(p.ids.next_raw()), *k, i as i64 * 480, 240, 0.7)).collect();
        Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes, ..Default::default() })) }.apply(p).unwrap();
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Idea", Color(0), src, bars * 4 * PPQ)) }.apply(p).unwrap();
        let pl = p.alloc_placement_id();
        Command::PlaceClip { track, placement: Placement { id: pl, clip, start: 4 * PPQ, lane: 0 } }.apply(p).unwrap();
        clip
    }

    fn project_with_track(name: &str) -> (Project, TrackId) {
        let mut p = Project::new("t");
        let id = p.alloc_track_id();
        p.tracks.insert(0, Track::new(id, TrackKind::Instrument, name, Color::palette(2)));
        (p, id)
    }

    fn project_with_kit() -> (Project, TrackId) {
        let (mut p, id) = project_with_track("Drums");
        let mut rack = Rack::new(RackKind::Drum);
        for (name, note) in [("Kick", 36u8), ("Snare", 38), ("Clap", 39), ("Closed Hat", 42), ("Open Hat", 46), ("Rim", 37)] {
            let mut c = Chain::new(name, Color(0));
            c.note = Some(note);
            rack.chains.push(c);
        }
        let dev = p.alloc_device_id();
        let mut d = Device::stock(dev, "rack.drum", "Drum Rack");
        d.kind = aitrack_model::DeviceKind::Rack(rack);
        p.track_mut(id).unwrap().devices.push(d);
        (p, id)
    }

    #[test]
    fn a_bass_is_one_proposal_that_leaves_the_project_until_accepted() {
        let (mut p, track) = project_with_track("Bass");
        p.scale = Some(Scale::new(Key(9), ScaleKind::Minor));
        let plan = plan(&mut p, Some(track), Some(Kind::Bass), "rolling, 2 bars", 7).unwrap();
        let before = p.clone();
        let mut h = History::default();
        let id = h.propose(&p, plan.label.clone(), Origin::Feature("midi-gen".into()), plan.commands).unwrap();
        assert_eq!(p, before, "proposing changes nothing");
        assert!(h.accept(&mut p, id).unwrap());
        let t = p.track(track).unwrap();
        assert_eq!(t.arrangement.len(), 1);
        let clip = &p.clips[&t.arrangement[0].clip];
        assert_eq!(clip.length, 2 * 4 * PPQ);
        assert_eq!(clip.scale, None, "the project already has a key");
        let Some(Source::Notes(src)) = p.sources.get(&clip.source) else { panic!("no notes") };
        assert!(src.notes.len() >= 8 && src.notes.iter().all(|n| n.start >= 0 && n.start + n.length <= clip.length));
        assert!(src.notes.windows(2).all(|w| (w[0].start, w[0].key) <= (w[1].start, w[1].key)), "sorted");
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p, before, "one undo step puts everything back");
    }

    #[test]
    fn with_no_key_the_clip_carries_the_one_it_was_written_in() {
        let (mut p, track) = project_with_track("Keys");
        let plan = plan(&mut p, Some(track), Some(Kind::Chords), "in C minor", 1).unwrap();
        let mut h = History::default();
        let id = h.propose(&p, "x", Origin::Feature("midi-gen".into()), plan.commands).unwrap();
        h.accept(&mut p, id).unwrap();
        let clip = p.clips.values().next().unwrap();
        assert_eq!(clip.scale, Some(Scale::new(Key(0), ScaleKind::Minor)));
        assert_eq!(p.scale, None, "the project key is not touched");
    }

    #[test]
    fn with_no_midi_track_a_new_one_is_made() {
        let mut p = Project::new("t");
        let n = p.tracks.len();
        let plan = plan(&mut p, None, Some(Kind::Melody), "", 3).unwrap();
        assert!(matches!(plan.commands[0], Command::AddTrack { .. }));
        let mut h = History::default();
        let before = p.clone();
        let id = h.propose(&p, "x", Origin::Feature("midi-gen".into()), plan.commands).unwrap();
        h.accept(&mut p, id).unwrap();
        assert_eq!(p.tracks.len(), n + 1);
        let t = p.tracks.iter().find(|t| t.name == "Melody").unwrap();
        assert_eq!((t.arrangement.len(), t.devices.len()), (1, 1));
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn drums_need_a_drum_rack_and_use_its_pads() {
        let (mut plain, track) = project_with_track("Synth");
        assert!(plan(&mut plain, Some(track), Some(Kind::Drums), "", 1).is_err());
        let (mut p, kit) = project_with_kit();
        let plan = plan(&mut p, None, Some(Kind::Drums), "tech house", 1).unwrap();
        let notes = plan.commands.iter().find_map(|c| if let Command::AddSource { source, .. } = c { Some(source) } else { None }).unwrap();
        let Source::Notes(src) = notes.as_ref() else { panic!() };
        assert!(src.notes.iter().all(|n| [36, 38, 39, 42, 46, 37].contains(&n.key)));
        assert!(plan.commands.iter().any(|c| matches!(c, Command::PlaceClip { track, .. } if *track == kit)));
    }

    #[test]
    fn a_new_clip_goes_after_the_one_already_there() {
        let (mut p, track) = project_with_track("Bass");
        for _ in 0..2 {
            let plan = plan(&mut p, Some(track), Some(Kind::Bass), "2 bars", 4).unwrap();
            let mut h = History::default();
            let id = h.propose(&p, "x", Origin::Feature("midi-gen".into()), plan.commands).unwrap();
            h.accept(&mut p, id).unwrap();
        }
        let starts: Vec<Tick> = p.track(track).unwrap().arrangement.iter().map(|pl| pl.start).collect();
        assert_eq!(starts, vec![0, 2 * 4 * PPQ]);
    }

    #[test]
    fn the_chords_already_there_steer_the_bass() {
        let (mut p, keys) = project_with_track("Keys");
        let src = p.alloc_source_id();
        let clip = p.alloc_clip_id();
        let notes: Vec<Note> = [57u8, 60, 64, 67].iter().map(|k| Note::new(p.alloc_note_id(), *k, 0, 4 * PPQ * 4, 0.8)).collect();
        Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes, ..Default::default() })) }.apply(&mut p).unwrap();
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Am7", Color(0), src, 4 * PPQ * 4)) }.apply(&mut p).unwrap();
        let pl = p.alloc_placement_id();
        Command::PlaceClip { track: keys, placement: Placement { id: pl, clip, start: 0, lane: 0 } }.apply(&mut p).unwrap();
        let bass = p.alloc_track_id();
        p.tracks.insert(0, Track::new(bass, TrackKind::Instrument, "Bass", Color(0)));
        let plan = plan(&mut p, Some(bass), Some(Kind::Bass), "offbeat", 2).unwrap();
        let Command::AddSource { source, .. } = plan.commands.iter().find(|c| matches!(c, Command::AddSource { .. })).unwrap() else { panic!() };
        let Source::Notes(src) = source.as_ref() else { panic!() };
        assert!(src.notes.iter().all(|n| n.key % 12 == 9 || n.key % 12 == 0 || n.key % 12 == 4 || n.key % 12 == 7), "only Am7 tones: {:?}", src.notes.iter().map(|n| n.key).collect::<Vec<_>>());
        assert!(plan.summary.iter().any(|l| l.contains("Am7")), "{:?}", plan.summary);
    }

    #[test]
    fn free_text_names_the_part_and_nonsense_is_refused() {
        let (mut p, track) = project_with_track("Synth");
        let r = plan(&mut p, Some(track), None, "un bajo rodante en La menor de 2 compases", 1).unwrap();
        assert!(r.line.starts_with("Bass") && r.line.contains("A Minor") && r.line.contains("2 bars"), "{}", r.line);
        assert!(plan(&mut p, Some(track), None, "hola, cómo estás", 1).is_err());
    }

    #[test]
    fn the_summary_is_short_and_says_where_it_went() {
        let (mut p, track) = project_with_track("Bass");
        let r = plan(&mut p, Some(track), Some(Kind::Bass), "", 1).unwrap();
        assert!((3..=6).contains(&r.summary.len()), "{:?}", r.summary);
        assert!(r.summary[0].contains("notes") && r.summary.iter().any(|l| l.starts_with("On Bass · bar 1")));
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(33), "A1");
    }

    #[test]
    fn an_empty_selected_clip_is_filled_not_duplicated() {
        let (mut p, track) = project_with_track("Keys");
        let clip = add_clip_with(&mut p, track, 2, &[]);
        let plan = plan_with_clip(&mut p, Some(track), Some(clip), Some(Kind::Chords), "", 5).unwrap();
        assert!(plan.commands.iter().all(|c| matches!(c, Command::AddNotes { .. })), "{:?}", plan.commands.iter().map(Command::label).collect::<Vec<_>>());
        assert!(plan.summary.iter().any(|l| l.starts_with("Replaces the 0 notes of \"Idea\" · bar 2")), "{:?}", plan.summary);
        let before = p.clone();
        let mut h = History::default();
        let id = h.propose(&p, "x", Origin::Feature("midi-gen".into()), plan.commands).unwrap();
        h.accept(&mut p, id).unwrap();
        assert_eq!(p.clips.len(), 1, "no second clip");
        let c = &p.clips[&clip];
        assert_eq!(c.length, 2 * 4 * PPQ);
        let Some(Source::Notes(src)) = p.sources.get(&c.source) else { panic!() };
        assert!(src.notes.len() > 4 && src.notes.iter().all(|n| n.start + n.length <= c.length));
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn a_clip_with_notes_is_replaced_only_when_asked() {
        let (mut p, track) = project_with_track("Bass");
        let clip = add_clip_with(&mut p, track, 1, &[40, 43, 45]);
        let plain = plan_with_clip(&mut p, Some(track), Some(clip), Some(Kind::Bass), "", 5).unwrap();
        assert!(plain.commands.iter().any(|c| matches!(c, Command::AddClip { .. })), "a new clip next to it");
        let asked = plan_with_clip(&mut p, Some(track), Some(clip), Some(Kind::Bass), "reemplaza las notas de este clip", 5).unwrap();
        assert!(!asked.commands.iter().any(|c| matches!(c, Command::AddClip { .. })));
        assert!(matches!(asked.commands[0], Command::RemoveNotes { .. }));
        assert!(asked.summary.iter().any(|l| l.starts_with("Replaces the 3 notes")), "{:?}", asked.summary);
        // The proposal says it deletes, and one undo brings the three notes back.
        let before = p.clone();
        let mut h = History::default();
        let id = h.propose(&p, "x", Origin::Feature("midi-gen".into()), asked.commands).unwrap();
        assert!(h.proposals().iter().find(|pr| pr.id == id).unwrap().is_destructive());
        h.accept(&mut p, id).unwrap();
        let c = &p.clips[&clip];
        let Some(Source::Notes(src)) = p.sources.get(&c.source) else { panic!() };
        assert!(!src.notes.iter().any(|n| n.length == 240 && n.velocity == 0.7), "the old notes are gone");
        assert_eq!(c.length, 4 * PPQ, "the clip keeps its length");
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn a_clip_on_another_track_is_not_touched() {
        let (mut p, bass) = project_with_track("Bass");
        let (_, other_id) = {
            let id = p.alloc_track_id();
            p.tracks.insert(0, Track::new(id, TrackKind::Instrument, "Keys", Color(0)));
            ((), id)
        };
        let clip = add_clip_with(&mut p, other_id, 1, &[60]);
        let plan = plan_with_clip(&mut p, Some(bass), Some(clip), Some(Kind::Bass), "replace this clip", 5).unwrap();
        assert!(plan.commands.iter().any(|c| matches!(c, Command::AddClip { .. })), "the selected clip belongs to another track");
    }
}
