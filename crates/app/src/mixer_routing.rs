//! Routing choices of the mixer strips: where a track's output goes and how
//! it is monitored.
//!
//! The strips only open a small popover; the choices and the edits are here.
//! Every change is a `SetTrack` command (one undo step). The model refuses
//! an output that would feed a track with its own signal
//! (`CommandError::RoutingCycle`), and the popover never offers one.

use crate::state::AppState;
use crate::theme::*;
use crate::widgets::{self, menu_header, menu_item};
use crate::workspace::Workspace;
use aitrack_model::{InputSource, Monitor, Output, Project, TrackId, TrackKind, TrackProp, routing};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Pixels, Point, StatefulInteractiveElement, anchored, deferred,
    div, px,
};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Which selector of a strip is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector {
    Output,
    Input,
}

thread_local! {
    /// The open popover: its strip, selector and where it opens (UI only).
    static OPEN: Cell<Option<(TrackId, Selector, Point<Pixels>)>> = const { Cell::new(None) };
    /// The popover that a click outside just closed, so the click on its own
    /// slot does not open it again at once.
    static CLOSED: Cell<Option<(TrackId, Selector, Instant)>> = const { Cell::new(None) };
}

pub fn open(track: TrackId, selector: Selector, at: Point<Pixels>) {
    let just_closed = CLOSED.get().is_some_and(|(t, s, when)| t == track && s == selector && when.elapsed() < Duration::from_millis(400));
    if just_closed || OPEN.get().is_some_and(|(t, s, _)| t == track && s == selector) {
        OPEN.set(None);
    } else {
        OPEN.set(Some((track, selector, at)));
    }
    CLOSED.set(None);
}

pub fn close() {
    if let Some((t, s, _)) = OPEN.take() {
        CLOSED.set(Some((t, s, Instant::now())));
    }
}

fn current(track: TrackId) -> Option<(Selector, Point<Pixels>)> {
    OPEN.get().filter(|(t, _, _)| *t == track).map(|(_, s, at)| (s, at))
}

/// What the output selector of `track` can be set to: Main, every track that
/// takes it without a loop, and nothing. The output it has stays listed even
/// when it is no longer a valid choice (a hardware pair, say).
pub fn output_choices(p: &Project, track: TrackId) -> Vec<(Output, String)> {
    let mut out = vec![(Output::Main, "Main".to_owned())];
    for id in routing::routable_outputs(p, track) {
        if let Some(t) = p.track(id) {
            let what = if t.kind == TrackKind::Group { "Group" } else { "Track" };
            out.push((Output::Track(id), format!("{what}: {}", t.name)));
        }
    }
    out.push((Output::None, "None".to_owned()));
    if let Some(t) = p.track(track)
        && !out.iter().any(|(o, _)| *o == t.output)
    {
        out.insert(0, (t.output, output_label(p, track)));
    }
    out
}

/// The text of a strip's output slot.
pub fn output_label(p: &Project, track: TrackId) -> String {
    let Some(t) = p.track(track) else { return "—".into() };
    match t.output {
        Output::Main => "Main".to_owned(),
        Output::Track(g) => p.track(g).map_or("—".into(), |g| g.name.clone()),
        Output::Hardware(i) => format!("Out {}/{}", i * 2 + 1, i * 2 + 2),
        Output::None => "None".into(),
    }
}

/// What the input selector of an audio track can be set to, from the input
/// channels the audio device has switched on (0-based): nothing, each channel
/// alone and each neighbouring pair.
pub fn input_choices(enabled: &[u16], current: InputSource) -> Vec<(InputSource, String)> {
    let mut channels: Vec<u16> = enabled.to_vec();
    channels.sort_unstable();
    channels.dedup();
    let mut out = vec![(InputSource::None, input_label(InputSource::None))];
    for &c in &channels {
        if channels.contains(&(c + 1)) && c % 2 == 0 {
            let pair = InputSource::Hardware { first: c, stereo: true };
            out.push((pair, input_label(pair)));
        }
    }
    for &c in &channels {
        let mono = InputSource::Hardware { first: c, stereo: false };
        out.push((mono, input_label(mono)));
    }
    if !out.iter().any(|(i, _)| *i == current) {
        out.insert(1, (current, input_label(current)));
    }
    out
}

/// The text of an audio track's input slot.
pub fn input_label(input: InputSource) -> String {
    match input {
        InputSource::None => "No input".to_owned(),
        InputSource::Hardware { first, stereo: false } => format!("In {}", first + 1),
        InputSource::Hardware { first, stereo: true } => format!("In {}/{}", first + 1, first + 2),
    }
}

impl AppState {
    /// The input an audio track records from: one `SetTrack`, one undo step.
    pub fn set_input(&mut self, track: TrackId, input: InputSource) -> bool {
        if self.project.track(track).is_none_or(|t| t.kind != TrackKind::Audio || t.input == input) {
            return false;
        }
        self.seal();
        let ok = self.set_track(track, TrackProp::Input(input));
        self.seal();
        ok
    }

    /// Route `track` to `output`: one `SetTrack`, one undo step. `false` when
    /// that would make a loop, the track cannot be routed, or nothing changes.
    pub fn set_output(&mut self, track: TrackId, output: Output) -> bool {
        let Some(t) = self.project.track(track) else { return false };
        if t.output == output || t.kind == TrackKind::Main || !t.kind.carries_audio() || routing::check_output(&self.project, track, output).is_err() {
            return false;
        }
        self.seal();
        let ok = self.set_track(track, TrackProp::Output(output));
        self.seal();
        ok
    }

    pub fn set_monitor(&mut self, track: TrackId, monitor: Monitor) -> bool {
        if self.project.track(track).is_none_or(|t| t.monitor == monitor) {
            return false;
        }
        self.seal();
        let ok = self.set_track(track, TrackProp::Monitor(monitor));
        self.seal();
        ok
    }
}

fn monitor_name(m: Monitor) -> &'static str {
    match m {
        Monitor::In => "In",
        Monitor::Auto => "Auto",
        Monitor::Off => "Off",
    }
}

/// The popover under the strip's open selector, if one is open for `track`.
pub fn popover(ws: &Workspace, track: TrackId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let (selector, at) = current(track)?;
    let t = ws.s.project.track(track)?;
    let mut card = widgets::menu_card("routing-pop", 192.);
    match selector {
        Selector::Output => {
            card = card.child(menu_header("OUTPUT"));
            for (i, (output, name)) in output_choices(&ws.s.project, track).into_iter().enumerate() {
                card = card.child(menu_item(("route-out", i), None, name, None, output == t.output).on_click(cx.listener(move |this, _, _, cx| {
                    close();
                    if !this.s.set_output(track, output) {
                        this.toast("That output would feed the track with its own signal", cx);
                    }
                    cx.notify();
                })));
            }
        }
        Selector::Input => {
            if t.kind == TrackKind::Audio {
                let enabled = ws.b.engine.audio_config().input_channels;
                card = card.child(menu_header("INPUT"));
                for (i, (input, name)) in input_choices(&enabled, t.input).into_iter().enumerate() {
                    card = card.child(menu_item(("route-in", i), None, name, None, input == t.input).on_click(cx.listener(move |this, _, _, cx| {
                        close();
                        this.s.set_input(track, input);
                        cx.notify();
                    })));
                }
            }
            card = card.child(menu_header("MONITOR"));
            for (i, m) in [Monitor::In, Monitor::Auto, Monitor::Off].into_iter().enumerate() {
                card = card.child(menu_item(("route-mon", i), None, monitor_name(m), None, m == t.monitor).on_click(cx.listener(move |this, _, _, cx| {
                    close();
                    this.s.set_monitor(track, m);
                    cx.notify();
                })));
            }
        }
    }
    Some(
        deferred(
            anchored().position(at).snap_to_window_with_margin(px(SP2)).child(
                div()
                    .id("routing-layer")
                    .occlude()
                    .on_mouse_down_out(cx.listener(|_, _, _, cx| {
                        close();
                        cx.notify();
                    }))
                    .child(card),
            ),
        )
        .with_priority(2)
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(Project::new("Routing"))
    }

    fn outputs(s: &AppState, t: TrackId) -> Vec<Output> {
        output_choices(&s.project, t).into_iter().map(|(o, _)| o).collect()
    }

    #[test]
    fn the_output_selector_never_offers_a_loop() {
        let mut s = state();
        let (a, b, c) = (s.add_track(TrackKind::Audio).unwrap(), s.add_track(TrackKind::Audio).unwrap(), s.add_track(TrackKind::Audio).unwrap());
        s.seal();
        assert!(s.set_output(a, Output::Track(b)));
        assert!(s.set_output(b, Output::Track(c)));
        // c feeds nothing yet, but b and a reach it: sending c to them is a loop.
        let offered = outputs(&s, c);
        assert!(!offered.contains(&Output::Track(a)) && !offered.contains(&Output::Track(b)), "{offered:?}");
        assert!(offered.contains(&Output::Main) && offered.contains(&Output::None));
        assert!(!s.set_output(c, Output::Track(a)), "refused even if forced");
        assert_eq!(s.project.track(c).unwrap().output, Output::Main);
        // Something that does not loop is offered, and a track is never its own output.
        assert!(outputs(&s, a).contains(&Output::Track(c)));
        assert!(!outputs(&s, a).contains(&Output::Track(a)));
        // The master takes no output change.
        let main = s.project.main_track().id;
        assert!(!s.set_output(main, Output::None));
    }

    #[test]
    fn an_output_change_is_one_undo_step_and_comes_back() {
        let mut s = state();
        let a = s.add_track(TrackKind::Audio).unwrap();
        let g = s.add_track(TrackKind::Group).unwrap();
        s.seal();
        let steps = s.history.steps().count();
        assert!(s.set_output(a, Output::Track(g)));
        assert_eq!(s.history.steps().count(), steps + 1);
        assert_eq!(output_label(&s.project, a), s.project.track(g).unwrap().name);
        assert!(!s.set_output(a, Output::Track(g)), "same output, no step");
        assert!(s.set_output(a, Output::None));
        assert_eq!(output_label(&s.project, a), "None");
        assert!(s.undo() && s.undo());
        assert_eq!(s.project.track(a).unwrap().output, Output::Main);
        // A hardware output the track already has stays in the list.
        s.project.track_mut(a).unwrap().output = Output::Hardware(1);
        assert_eq!(outputs(&s, a)[0], Output::Hardware(1));
        assert_eq!(output_label(&s.project, a), "Out 3/4");
    }

    #[test]
    fn monitor_modes_are_one_step_each() {
        let mut s = state();
        let a = s.add_track(TrackKind::Audio).unwrap();
        s.seal();
        assert_eq!(s.project.track(a).unwrap().monitor, Monitor::Auto);
        let steps = s.history.steps().count();
        assert!(s.set_monitor(a, Monitor::In));
        assert!(!s.set_monitor(a, Monitor::In));
        assert!(s.set_monitor(a, Monitor::Off));
        assert_eq!(s.history.steps().count(), steps + 2);
        assert!(s.undo());
        assert_eq!(s.project.track(a).unwrap().monitor, Monitor::In);
    }

    #[test]
    fn the_input_selector_lists_the_enabled_channels_and_audio_tracks_take_one() {
        let enabled = [0, 1, 2, 3, 5];
        let names: Vec<String> = input_choices(&enabled, InputSource::None).into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["No input", "In 1/2", "In 3/4", "In 1", "In 2", "In 3", "In 4", "In 6"]);
        // An input the device no longer has stays listed, right after "No input".
        let gone = InputSource::Hardware { first: 8, stereo: true };
        assert_eq!(input_choices(&enabled, gone)[1].0, gone);

        let mut s = state();
        let (a, m) = (s.add_track(TrackKind::Audio).unwrap(), s.add_track(TrackKind::Instrument).unwrap());
        s.seal();
        let steps = s.history.steps().count();
        let pair = InputSource::Hardware { first: 2, stereo: true };
        assert!(s.set_input(a, pair));
        assert_eq!((s.history.steps().count(), s.project.track(a).unwrap().input), (steps + 1, pair));
        assert!(!s.set_input(a, pair), "same input, no step");
        assert!(!s.set_input(m, pair), "only audio tracks record");
        assert!(s.undo());
        assert_eq!(s.project.track(a).unwrap().input, InputSource::None);
    }

    #[test]
    fn a_click_outside_does_not_reopen_the_selector_it_closed() {
        let t = TrackId(7);
        let at = Point::new(px(10.), px(10.));
        open(t, Selector::Output, at);
        assert!(current(t).is_some());
        close();
        assert!(current(t).is_none());
        open(t, Selector::Output, at);
        assert!(current(t).is_none(), "the click that follows the close is ignored");
        open(t, Selector::Input, at);
        assert_eq!(current(t).map(|c| c.0), Some(Selector::Input));
        open(t, Selector::Input, at);
        assert!(current(t).is_none(), "clicking its slot again closes it");
    }
}
