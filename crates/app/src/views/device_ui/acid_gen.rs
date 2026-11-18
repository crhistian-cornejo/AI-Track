//! The Acid 303 card's phrase generator: Generate / Mutate / To MIDI clip, and the
//! five controls behind them. The musical work is `aitrack_ai::generate::acid`
//! (pure, seeded); this file keeps the per-device settings in UI state and turns
//! a phrase into one batch of `SetParam` commands (the 16 steps and the play mode)
//! or into a notes clip, each a single undo step.

use super::{Dev, set_many};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_ai::generate::acid::{self, BASE_KEY, Opts, Phrase};
use aitrack_ai::generate::rng::splitmix64;
use aitrack_dsp::ParamInfo;
use aitrack_model::{Clip, ClipId, Command, DeviceId, Key, Note, NoteSource, Placement, Project, Scale, ScaleKind, Source, Tick, TrackId, TrackKind};
use aitrack_stock::instrument::acid303::{STEPS, p};
use gpui::{AnyElement, Context, Div, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};

/// One of the five generator controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctl {
    Density,
    Accent,
    Slide,
    Octaves,
    Tension,
}

impl Ctl {
    const ALL: [Ctl; 5] = [Ctl::Density, Ctl::Accent, Ctl::Slide, Ctl::Octaves, Ctl::Tension];

    fn name(self) -> &'static str {
        match self {
            Ctl::Density => "Density",
            Ctl::Accent => "Accent",
            Ctl::Slide => "Slide",
            Ctl::Octaves => "Octaves",
            Ctl::Tension => "Tension",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Ctl::Density => "How many steps sound",
            Ctl::Accent => "How many notes are accented",
            Ctl::Slide => "How many notes glide into the next",
            Ctl::Octaves => "Octave jumps and range",
            Ctl::Tension => "Flat 2nd and flat 5th colour",
        }
    }
}

/// The generator's settings for one device. They live in the UI, not in the project.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcidGen {
    pub density: f32,
    pub accent: f32,
    pub slide: f32,
    pub octaves: f32,
    pub tension: f32,
    /// The dice: the seed of the phrase last written.
    pub seed: u64,
}

impl AcidGen {
    pub fn new(device: DeviceId) -> Self {
        Self { density: 0.6, accent: 0.5, slide: 0.3, octaves: 0.4, tension: 0.1, seed: splitmix64(0xAC1D_0303 ^ device.0) }
    }

    pub fn get(&self, c: Ctl) -> f32 {
        match c {
            Ctl::Density => self.density,
            Ctl::Accent => self.accent,
            Ctl::Slide => self.slide,
            Ctl::Octaves => self.octaves,
            Ctl::Tension => self.tension,
        }
    }

    /// Move a control one tenth up or down.
    pub fn nudge(&mut self, c: Ctl, up: bool) {
        let v = ((self.get(c) * 10.).round() + if up { 1. } else { -1. }).clamp(0., 10.) / 10.;
        match c {
            Ctl::Density => self.density = v,
            Ctl::Accent => self.accent = v,
            Ctl::Slide => self.slide = v,
            Ctl::Octaves => self.octaves = v,
            Ctl::Tension => self.tension = v,
        }
    }

    pub fn opts(&self, scale: Scale) -> Opts {
        Opts { steps: STEPS, density: self.density, accent: self.accent, slide: self.slide, octaves: self.octaves, tension: self.tension, ..Opts::new(scale) }
    }

    /// The next dice roll.
    pub fn roll(&mut self) -> u64 {
        self.seed = splitmix64(self.seed);
        self.seed
    }

    /// Four digits that tell one roll from another.
    pub fn seed_tag(&self) -> String {
        format!("#{:04}", self.seed % 10_000)
    }
}

/// The key a phrase is written in: the project's, else A minor.
pub fn scale_of(p: &Project) -> Scale {
    p.scale.unwrap_or(Scale::new(Key(9), ScaleKind::Minor))
}

/// The parameter changes (normalised) that write `phrase` into the 16 steps, and the Pattern play mode.
/// `info` finds a parameter by id; steps past the phrase are left alone.
pub fn pattern_values(phrase: &Phrase, info: &dyn Fn(u32) -> Option<ParamInfo>) -> Vec<(u32, f32)> {
    let mut out = Vec::new();
    let mut put = |id: u32, plain: f32| {
        if let Some(i) = info(id) {
            out.push((id, i.to_normalized(plain)));
        }
    };
    for (i, s) in phrase.pattern_steps().iter().enumerate().take(STEPS) {
        put(p::step(i, p::NOTE), s.note as f32);
        put(p::step(i, p::GATE), f32::from(u8::from(s.gate)));
        put(p::step(i, p::S_ACCENT), f32::from(u8::from(s.accent)));
        put(p::step(i, p::S_SLIDE), f32::from(u8::from(s.slide)));
    }
    put(p::SEQ_MODE, 1.);
    out
}

/// The phrase the device's steps hold now.
pub fn read_phrase(scale: Scale, value: &dyn Fn(u32) -> Option<f32>) -> Phrase {
    let steps: Vec<acid::PatternStep> = (0..STEPS)
        .map(|i| {
            let on = |f: u32| value(p::step(i, f)).is_some_and(|v| v >= 0.5);
            acid::PatternStep { note: value(p::step(i, p::NOTE)).map_or(0, |v| v.round() as i32), gate: on(p::GATE), accent: on(p::S_ACCENT), slide: on(p::S_SLIDE) }
        })
        .collect();
    Phrase::from_pattern(&steps, scale)
}

/// The commands that put `phrase` in a new clip: a notes source from tick 0 (a glide overlaps the next
/// note, an accent is hard), the clip as long as the phrase, and its place: the empty session `slot`
/// when given, else the arrangement from `start`.
pub fn clip_commands(p: &mut Project, track: TrackId, phrase: &Phrase, name: &str, color: aitrack_model::Color, start: Tick, slot: Option<aitrack_model::SceneId>) -> (ClipId, Vec<Command>) {
    let notes: Vec<Note> = phrase.notes(BASE_KEY).into_iter().map(|n| Note::new(p.alloc_note_id(), n.key, n.start, n.length, n.velocity)).collect();
    let (src, clip) = (p.alloc_source_id(), p.alloc_clip_id());
    let mut cmds = vec![
        Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes })) },
        Command::AddClip { id: clip, clip: Box::new(Clip::new(name, color, src, phrase.ticks())) },
    ];
    match slot {
        Some(scene) => cmds.push(Command::SetSessionSlot { track, scene, clip: Some(clip) }),
        None => {
            let placement = p.alloc_placement_id();
            cmds.push(Command::PlaceClip { track, placement: Placement { id: placement, clip, start, lane: 0 } });
        }
    }
    (clip, cmds)
}

fn info_of(ws: &Workspace, device: DeviceId, id: u32) -> Option<ParamInfo> {
    let d = ws.s.project.device(device)?;
    let key = match &d.kind {
        aitrack_model::DeviceKind::Stock { plugin_id } => plugin_id.as_str(),
        _ => return None,
    };
    ws.b.plugins.device(key)?.params.iter().find(|p| p.id == id).cloned()
}

fn current_phrase(ws: &Workspace, device: DeviceId) -> Phrase {
    let value = |id: u32| info_of(ws, device, id).map(|i| i.to_plain(super::current(ws, device, &i)));
    read_phrase(scale_of(&ws.s.project), &value)
}

/// Write a new phrase into the steps and switch to Pattern: one undo step.
pub fn generate(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    let scale = scale_of(&ws.s.project);
    let g = ws.acid_gen.entry(device).or_insert_with(|| AcidGen::new(device));
    let seed = g.roll();
    let phrase = acid::generate(seed, &g.opts(scale));
    let values = pattern_values(&phrase, &|id| info_of(ws, device, id));
    set_many(ws, "Generate acid phrase", device, &values);
    cx.notify();
}

/// Change a few steps of the phrase the device holds now: one undo step.
pub fn mutate(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    let now = current_phrase(ws, device);
    let g = ws.acid_gen.entry(device).or_insert_with(|| AcidGen::new(device));
    let seed = g.roll();
    let next = now.mutate(seed, 0.35);
    let values: Vec<(u32, f32)> = pattern_values(&next, &|id| info_of(ws, device, id))
        .into_iter()
        .filter(|&(id, v)| info_of(ws, device, id).is_none_or(|i| (super::current(ws, device, &i) - v).abs() > 1e-4) || id == p::SEQ_MODE)
        .collect();
    set_many(ws, "Mutate acid phrase", device, &values);
    cx.notify();
}

/// The phrase as a MIDI clip on the device's track: in the selected empty Session slot, else in the
/// arrangement at the playhead's bar (the first free room from there). One undo step.
pub fn to_clip(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    let Some(track) = ws.s.project.device_track(device).filter(|t| ws.s.project.track(*t).is_some_and(|t| t.kind == TrackKind::Instrument)) else {
        ws.toast("The Acid 303 has to be on an instrument track", cx);
        return;
    };
    let phrase = current_phrase(ws, device);
    if phrase.gated() == 0 {
        ws.toast("The pattern is empty", cx);
        return;
    }
    let slot = (ws.s.view == crate::state::MainView::Session && ws.s.selected_track == Some(track)).then(|| ws.s.session_scene()).flatten().filter(|s| ws.s.slot_clip(track, *s).is_none());
    let bar = ws.s.project.tempo.signature_at(0).bar_ticks().max(1);
    let playhead = crate::state::Snap::Bar.floor(ws.b.engine.transport().position.max(0));
    let start = crate::views::ai_panel::midi_gen::free_start(&ws.s.project, Some(track), playhead / bar * bar, phrase.ticks());
    let color = ws.s.project.track(track).map_or(aitrack_model::Color::palette(0), |t| ws.s.clip_color_for(t.color));
    let (_, cmds) = clip_commands(&mut ws.s.project, track, &phrase, "Acid phrase", color, start, slot);
    ws.s.seal();
    let ok = ws.s.edit("Acid phrase to MIDI clip", cmds);
    ws.s.seal();
    if ok {
        let at = if slot.is_some() { "in the selected slot".to_owned() } else { format!("at bar {}", start / bar + 1) };
        ws.toast(format!("Acid phrase added as a MIDI clip {at}"), cx);
    }
    cx.notify();
}

fn button(id: (&'static str, usize), text: &'static str, i: Icon, primary: bool, tip: &'static str) -> gpui::Stateful<Div> {
    let pal = p();
    let fg = if primary { pal.on_accent } else { pal.text };
    div()
        .id(id)
        .flex_none()
        .h(px(24.))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if primary { pal.accent } else { content(0.06) })
        .border_1()
        .border_color(if primary { pal.accent } else { border() })
        .hover(|s| s.opacity(0.9))
        .active(|s| s.opacity(0.75))
        .tooltip(crate::widgets::tip(tip, None))
        .child(icon(i, 12., fg))
        .child(label(text, TEXT_SM, fg).font_weight(gpui::FontWeight::SEMIBOLD))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A compact stepper: name, a tenth down, the value, a tenth up.
fn stepper(dev: &Dev, c: Ctl, value: f32, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let slot = Ctl::ALL.iter().position(|x| *x == c).unwrap_or(0);
    let arrow = |id: usize, text: &'static str, up: bool, cx: &mut Context<Workspace>| {
        div()
            .id(("acid-gen-nudge", dev.key(0x200 | id << 1 | usize::from(up))))
            .flex_none()
            .w(px(16.))
            .h(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(content(0.12)))
            .child(mono(text, TEXT_SM, pal.muted))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.acid_gen.entry(device).or_insert_with(|| AcidGen::new(device)).nudge(c, up);
                    cx.notify();
                }),
            )
    };
    div()
        .flex_none()
        .h(px(24.))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(content(0.05))
        .border_1()
        .border_color(border())
        .child(arrow(slot, "-", false, cx))
        .child(
            div()
                .id(("acid-gen-val", dev.key(0x300 | slot)))
                .flex()
                .items_center()
                .gap(px(SP1))
                .px(px(SP1))
                .tooltip(crate::widgets::tip(c.name(), Some(c.tip())))
                .child(label(c.name(), TEXT_XS, pal.muted))
                .child(mono(format!("{:.0}", value * 100.), TEXT_XS, if dev.enabled { pal.text } else { pal.faint })),
        )
        .child(arrow(slot, "+", true, cx))
}

/// The generator strip: the three actions, the dice, and the five controls.
pub fn strip(ws: &Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let g = ws.acid_gen.get(&device).copied().unwrap_or_else(|| AcidGen::new(device));
    let act = |f: fn(&mut Workspace, DeviceId, &mut Context<Workspace>)| {
        cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
            cx.stop_propagation();
            f(this, device, cx);
        })
    };
    let generate_btn = button(("acid-gen", dev.key(0x100)), "Generate", Icon::Dice, true, "A new acid phrase in the 16 steps, in Pattern mode (one undo step)").on_mouse_down(MouseButton::Left, act(generate));
    let mutate_btn = button(("acid-gen", dev.key(0x101)), "Mutate", Icon::Shuffle, false, "Change a few steps and keep the motif").on_mouse_down(MouseButton::Left, act(mutate));
    let clip_btn = button(("acid-gen", dev.key(0x102)), "To MIDI clip", Icon::MusicNote, false, "Write the phrase as a MIDI clip on this track, at the playhead or in the selected slot").on_mouse_down(MouseButton::Left, act(to_clip));
    let steppers = Ctl::ALL.iter().map(|&c| stepper(dev, c, g.get(c), cx).into_any_element()).collect::<Vec<AnyElement>>();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(div().flex().items_center().gap(px(SP1)).child(generate_btn).child(mutate_btn).child(clip_btn).child(div().flex_1()).child(mono(g.seed_tag(), TEXT_XS, pal.faint)))
        .child(div().flex().flex_wrap().gap(px(SP1)).children(steppers))
        .when(!dev.enabled, |d| d.opacity(0.7))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{DeviceId, History, Origin, Track};

    fn info() -> impl Fn(u32) -> Option<ParamInfo> {
        let params = aitrack_stock::instrument::acid303::params();
        move |id| params.iter().find(|p| p.id == id).cloned()
    }

    #[test]
    fn a_phrase_writes_all_sixteen_steps_and_pattern_mode() {
        let phrase = acid::generate(7, &AcidGen::new(DeviceId(1)).opts(Scale::new(Key(9), ScaleKind::Minor)));
        let vals = pattern_values(&phrase, &info());
        assert_eq!(vals.len(), STEPS * 4 + 1);
        assert!(vals.iter().all(|(_, v)| (0. ..=1.).contains(v)));
        let find = |id: u32| vals.iter().find(|(i, _)| *i == id).unwrap().1;
        let i = info();
        assert_eq!(i(p::SEQ_MODE).unwrap().to_plain(find(p::SEQ_MODE)).round(), 1.);
        let s0 = phrase.pattern_steps()[0];
        assert_eq!(i(p::step(0, p::NOTE)).unwrap().to_plain(find(p::step(0, p::NOTE))).round() as i32, s0.note);
        assert_eq!(i(p::step(0, p::GATE)).unwrap().to_plain(find(p::step(0, p::GATE))).round() == 1., s0.gate);
    }

    #[test]
    fn the_steps_read_back_into_the_phrase_that_wrote_them() {
        let phrase = acid::generate(11, &AcidGen::new(DeviceId(1)).opts(Scale::new(Key(0), ScaleKind::Minor)));
        let i = info();
        let vals = pattern_values(&phrase, &i);
        let value = |id: u32| vals.iter().find(|(v, _)| *v == id).map(|(_, n)| i(id).unwrap().to_plain(*n));
        assert_eq!(read_phrase(phrase.scale, &value), phrase);
    }

    #[test]
    fn nudging_moves_a_control_by_tenths_within_zero_and_one() {
        let mut g = AcidGen::new(DeviceId(3));
        g.nudge(Ctl::Density, true);
        assert!((g.density - 0.7).abs() < 1e-6);
        for _ in 0..20 {
            g.nudge(Ctl::Tension, true);
            g.nudge(Ctl::Slide, false);
        }
        assert_eq!((g.tension, g.slide), (1., 0.));
        let before = g.seed;
        assert_ne!(g.roll(), before);
        assert_eq!(g.seed_tag().len(), 5);
    }

    #[test]
    fn the_clip_is_one_undo_step_with_notes_from_zero_and_the_phrase_length() {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        p.tracks.insert(0, Track::new(track, TrackKind::Instrument, "Acid", aitrack_model::Color::palette(1)));
        let phrase = acid::generate(5, &Opts { slide: 1., accent: 0.8, ..AcidGen::new(DeviceId(1)).opts(Scale::new(Key(9), ScaleKind::Minor)) });
        let mut h = History::default();
        let (clip, cmds) = clip_commands(&mut p, track, &phrase, "Acid phrase", aitrack_model::Color::palette(1), 3840, None);
        h.apply(&mut p, "Acid phrase to MIDI clip", Origin::User, cmds).unwrap();
        let c = p.clips.get(&clip).unwrap();
        assert_eq!(c.length, 16 * 240);
        let Some(Source::Notes(src)) = p.sources.get(&c.source) else { panic!("no notes") };
        assert!(!src.notes.is_empty() && src.notes.iter().all(|n| n.start >= 0 && n.start + n.length <= c.length + 240));
        assert!(src.notes.iter().any(|n| n.start == 0) || phrase.steps[0].tie || !phrase.steps[0].gate);
        assert!(src.notes.iter().any(|n| n.velocity >= 0.78) && src.notes.iter().any(|n| n.velocity < 0.78));
        assert_eq!(p.track(track).unwrap().arrangement.len(), 1);
        assert_eq!(p.track(track).unwrap().arrangement[0].start, 3840);
        h.undo(&mut p).unwrap();
        assert!(!p.clips.contains_key(&clip) && p.track(track).unwrap().arrangement.is_empty(), "one undo takes the whole clip away");
    }
}
