//! Locators: named marks on the Arrangement ruler (Live's locators). They
//! structure the song (intro, drop, break), jump the playhead, and the loop
//! brace can be set from one to the next. Every change is a `Command`.
//!
//! The pure edits are methods on `AppState` (tested headlessly); the jumps,
//! which need the transport, are on `Workspace`.

use super::ceil_to;
use crate::state::AppState;
use crate::workspace::Workspace;
use aitrack_model::*;
use gpui::Context;

/// A jump waiting for the launch quantisation: when playback reaches `at`,
/// the playhead goes to `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingJump {
    pub from: Tick,
    pub at: Tick,
    pub target: Tick,
}

/// What Set Locator did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocatorSet {
    Added(LocatorId),
    /// There was one at that spot: it is gone (Live's Set Locator toggles).
    Removed,
}

/// The name as the ruler shows it: long names are cut short.
pub fn shown_name(name: &str) -> String {
    const MAX: usize = 24;
    if name.chars().count() <= MAX { name.to_owned() } else { name.chars().take(MAX - 1).chain(std::iter::once('…')).collect() }
}

/// The width of a locator's name tag on the ruler, in px: its mark, the
/// name and some air. Shared by painting and hit testing.
pub fn tag_width(name: &str) -> f32 {
    (12. + shown_name(name).chars().count() as f32 * 6.).clamp(24., 168.)
}

/// The locator under `x` (px from the ruler's left edge; the ruler shows the
/// song from `scroll_x`), the topmost of overlapping ones: its mark or its tag.
pub fn locator_at(p: &Project, px_per_tick: f32, scroll_x: f32, x: f32) -> Option<LocatorId> {
    p.locators.iter().rev().find(|l| {
        let x0 = l.tick as f32 * px_per_tick - scroll_x;
        x >= x0 - 3. && x <= x0 + tag_width(&l.name)
    }).map(|l| l.id)
}

/// `tick` moved to the nearest multiple of `step` (`step` of 1 or less leaves it).
pub fn round_to(tick: Tick, step: Tick) -> Tick {
    if step <= 1 {
        return tick;
    }
    ((tick + step / 2).div_euclid(step) * step).max(0)
}

/// The length in ticks of a launch quantisation at `tick`.
pub fn quantum(p: &Project, q: Quantize, tick: Tick) -> Tick {
    q.ticks(p.tempo.signature_at(tick).bar_ticks()).max(1)
}

impl AppState {
    /// The first free "Locator N".
    fn next_locator_name(&self) -> String {
        let used = |n: usize| self.project.locators.iter().any(|l| l.name == format!("Locator {n}"));
        format!("Locator {}", (1..).find(|n| !used(*n)).unwrap_or(1))
    }

    /// Put a locator at `tick`, or take away the one already there. The new
    /// one is selected. One undo step.
    pub fn set_locator(&mut self, tick: Tick) -> Option<LocatorSet> {
        let tick = tick.max(0);
        self.seal();
        let out = if let Some(existing) = self.project.locators.iter().find(|l| l.tick == tick).map(|l| l.id) {
            self.edit("Delete locator", vec![Command::RemoveLocator { id: existing }]).then(|| {
                if self.ed.locator == Some(existing) {
                    self.ed.locator = None;
                }
                LocatorSet::Removed
            })
        } else {
            let id = self.project.alloc_locator_id();
            let name = self.next_locator_name();
            self.edit("Add locator", vec![Command::AddLocator { locator: Locator { id, tick, name } }]).then(|| {
                self.ed.locator = Some(id);
                LocatorSet::Added(id)
            })
        };
        self.seal();
        out
    }

    /// Move a locator; during a drag the steps coalesce into one.
    pub fn move_locator(&mut self, id: LocatorId, tick: Tick, drag: bool) -> bool {
        let tick = tick.max(0);
        if self.project.locator(id).is_none_or(|l| l.tick == tick) {
            return false;
        }
        let cmd = vec![Command::MoveLocator { id, tick }];
        if drag { self.edit_coalesced("locator-move", "Move locator", cmd) } else { self.edit("Move locator", cmd) }
    }

    pub fn rename_locator(&mut self, id: LocatorId, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.project.locator(id).is_none_or(|l| l.name == name) {
            return false;
        }
        self.seal();
        let ok = self.edit("Rename locator", vec![Command::RenameLocator { id, name: name.to_owned() }]);
        self.seal();
        ok
    }

    pub fn remove_locator(&mut self, id: LocatorId) -> bool {
        self.seal();
        let ok = self.edit("Delete locator", vec![Command::RemoveLocator { id }]);
        self.seal();
        if ok && self.ed.locator == Some(id) {
            self.ed.locator = None;
        }
        ok
    }

    /// The span Loop to Next Locator would put the loop brace on: from this
    /// locator to the next, or to the end of the song after the last one.
    pub fn locator_loop_span(&self, id: LocatorId) -> Option<(Tick, Tick)> {
        let from = self.project.locator(id)?.tick;
        let to = self.project.next_locator(from).map(|l| l.tick).unwrap_or_else(|| self.project.arrangement_end());
        (to > from).then_some((from, to))
    }

    /// Loop to Next Locator: the loop brace goes from this locator to the next.
    pub fn loop_to_next_locator(&mut self, id: LocatorId) -> bool {
        let Some(region) = self.locator_loop_span(id) else { return false };
        self.seal();
        let ok = self.edit("Set loop", vec![Command::SetLoop { region: Some(region) }]);
        self.seal();
        ok
    }

    /// The locator Loop to Next Locator acts on without being told: the
    /// selected one, else the last one at or before `at`.
    pub fn locator_for_loop(&self, at: Tick) -> Option<LocatorId> {
        self.ed.locator.filter(|id| self.project.locator(*id).is_some()).or_else(|| self.project.locators.iter().rev().find(|l| l.tick <= at).map(|l| l.id))
    }

    /// Where the previous / next locator is, seen from `from`.
    pub fn step_locator(&self, from: Tick, forward: bool) -> Option<&Locator> {
        if forward { self.project.next_locator(from) } else { self.project.prev_locator(from - 1) }
    }
}

impl Workspace {
    /// Where playback is, or the insert marker when it is stopped.
    fn here(&self) -> Tick {
        let t = self.b.engine.transport();
        if t.playing { t.position } else { self.s.ed.marker }
    }

    /// Send the playhead to `tick` (the insert marker goes too). While
    /// playing the jump waits for the global launch quantisation, as
    /// scenes do; stopped, it is at once.
    pub fn jump_to(&mut self, tick: Tick, cx: &mut Context<Self>) {
        let tick = tick.max(0);
        self.s.ed.marker = tick;
        self.s.ed.time = None;
        let t = self.b.engine.transport();
        let step = quantum(&self.s.project, self.b.engine.global_quantize(), t.position);
        let at = ceil_to(t.position, step);
        if t.playing && step > 1 && at > t.position {
            self.s.ed.jump = Some(PendingJump { from: t.position, at, target: tick });
        } else {
            self.s.ed.jump = None;
            self.b.engine.locate(tick);
        }
        cx.notify();
    }

    /// Fire the jump that was waiting for the quantisation, once playback
    /// has got there. `true` when it moved the playhead (or gave up).
    pub fn poll_jump(&mut self) -> bool {
        let Some(j) = self.s.ed.jump else { return false };
        let t = self.b.engine.transport();
        // Stopped, or the playhead was moved by something else (a loop
        // wrap, a click on the ruler): the wait is over.
        if !t.playing || t.position < j.from {
            self.s.ed.jump = None;
            return true;
        }
        if t.position >= j.at {
            self.s.ed.jump = None;
            self.b.engine.locate(j.target);
            return true;
        }
        false
    }

    /// The "Add Locator" button and key: a locator at the playhead (on the
    /// global quantisation) while playing, else at the insert marker.
    pub fn set_locator_here(&mut self, cx: &mut Context<Self>) {
        let t = self.b.engine.transport();
        let at = if t.playing {
            let step = quantum(&self.s.project, self.b.engine.global_quantize(), t.position);
            round_to(t.position, step)
        } else {
            self.here()
        };
        match self.s.set_locator(at) {
            Some(LocatorSet::Added(_)) => self.toast("Locator added: Ctrl+R renames it", cx),
            Some(LocatorSet::Removed) => self.toast("Locator removed", cx),
            None => {}
        }
        cx.notify();
    }

    /// Previous / Next Locator keys: jump to the neighbouring locator.
    pub fn step_to_locator(&mut self, forward: bool, cx: &mut Context<Self>) {
        let from = if self.b.engine.transport().playing { self.b.engine.transport().position } else { self.s.ed.marker };
        match self.s.step_locator(from, forward).map(|l| (l.id, l.tick)) {
            Some((id, tick)) => {
                self.s.ed.locator = Some(id);
                self.jump_to(tick, cx);
                self.reveal_tick(tick);
            }
            None => self.toast(if forward { "No locator after this point" } else { "No locator before this point" }, cx),
        }
    }

    /// Scroll the arrangement so `tick` is on screen.
    fn reveal_tick(&mut self, tick: Tick) {
        let width = f32::from(self.regions.arrange.get().size.width);
        if let Some(scroll) = crate::nav::follow(self.s.arrange, tick, width) {
            self.s.arrange.scroll_x = scroll;
        }
    }

    /// Loop to Next Locator, and switch the loop on.
    pub fn loop_to_next_locator(&mut self, id: Option<LocatorId>, cx: &mut Context<Self>) {
        let at = self.here();
        let Some(id) = id.or_else(|| self.s.locator_for_loop(at)) else {
            self.toast("Add a locator first: Ctrl+Shift+L puts one at the playhead", cx);
            return;
        };
        if self.s.loop_to_next_locator(id) {
            self.b.engine.set_looping(true);
            self.toast("Loop set from this locator to the next", cx);
        } else {
            self.toast("There is no room after this locator", cx);
        }
        cx.notify();
    }
}

impl Workspace {
    /// Dev view `locators`: a song structure on the ruler, the third locator
    /// selected with its menu open and the loop on the second section.
    pub fn fake_locators(&mut self) {
        let bar = 4 * PPQ;
        let spots = [(0, "Intro"), (8 * bar, "Build"), (16 * bar, "Drop"), (24 * bar, "Break")];
        // The first track's first bar is nothing special; the song is long enough to show the sections.
        self.s.project.loop_region = None;
        for (tick, name) in spots {
            let id = self.s.project.alloc_locator_id();
            self.s.edit("Add locator", vec![Command::AddLocator { locator: Locator { id, tick, name: name.into() } }]);
        }
        self.s.seal();
        let ids: Vec<LocatorId> = self.s.project.locators.iter().map(|l| l.id).collect();
        self.s.loop_to_next_locator(ids[1]);
        self.s.ed.locator = Some(ids[2]);
        self.s.arrange.px_per_beat = 6.;
        self.s.arrange.scroll_x = 0.;
        let x = gpui::px(16. * bar as f32 / PPQ as f32 * 6. + 200.);
        self.menu = Some(crate::workspace::Menu { kind: crate::workspace::MenuKind::LocatorContext(ids[2]), at: gpui::point(x, gpui::px(96.)), opened: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(Project::new("t"))
    }

    #[test]
    fn set_locator_adds_names_and_toggles_off_at_the_same_spot() {
        let mut s = state();
        let Some(LocatorSet::Added(a)) = s.set_locator(4 * PPQ) else { panic!() };
        let Some(LocatorSet::Added(b)) = s.set_locator(0) else { panic!() };
        assert_eq!(s.project.locators.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["Locator 2", "Locator 1"]);
        assert_eq!(s.ed.locator, Some(b));
        assert_eq!(s.set_locator(4 * PPQ), Some(LocatorSet::Removed));
        assert!(s.project.locator(a).is_none());
        // Each press is one undo step.
        s.history.undo(&mut s.project).unwrap();
        assert!(s.project.locator(a).is_some());
        s.history.undo(&mut s.project).unwrap();
        s.history.undo(&mut s.project).unwrap();
        assert!(s.project.locators.is_empty());
    }

    #[test]
    fn a_drag_is_one_undo_step_and_rename_and_delete_undo() {
        let mut s = state();
        let Some(LocatorSet::Added(id)) = s.set_locator(PPQ) else { panic!() };
        s.seal();
        for t in [2, 3, 5, 8] {
            assert!(s.move_locator(id, t * PPQ, true));
        }
        s.seal();
        assert_eq!(s.project.locator(id).unwrap().tick, 8 * PPQ);
        s.history.undo(&mut s.project).unwrap();
        assert_eq!(s.project.locator(id).unwrap().tick, PPQ, "the whole drag undoes at once");
        assert!(s.rename_locator(id, "  Drop "));
        assert!(!s.rename_locator(id, "Drop"), "same name: nothing to do");
        assert!(!s.rename_locator(id, "   "));
        assert_eq!(s.project.locator(id).unwrap().name, "Drop");
        assert!(s.remove_locator(id));
        assert_eq!(s.ed.locator, None);
        s.history.undo(&mut s.project).unwrap();
        assert_eq!(s.project.locator(id).unwrap().name, "Drop");
    }

    #[test]
    fn loop_to_next_locator_spans_to_the_next_one_or_the_song_end() {
        let mut s = state();
        let ids: Vec<LocatorId> = [0, 8 * PPQ, 16 * PPQ]
            .into_iter()
            .map(|t| match s.set_locator(t) {
                Some(LocatorSet::Added(id)) => id,
                _ => panic!(),
            })
            .collect();
        assert_eq!(s.locator_loop_span(ids[0]), Some((0, 8 * PPQ)));
        assert!(s.loop_to_next_locator(ids[1]));
        assert_eq!(s.project.loop_region, Some((8 * PPQ, 16 * PPQ)));
        // After the last one: to the end of the song (32 beats at least).
        assert_eq!(s.locator_loop_span(ids[2]), Some((16 * PPQ, s.project.arrangement_end())));
        s.history.undo(&mut s.project).unwrap();
        assert_ne!(s.project.loop_region, Some((8 * PPQ, 16 * PPQ)));
        // Without a selection it acts on the last locator at or before a point.
        s.ed.locator = None;
        assert_eq!(s.locator_for_loop(9 * PPQ), Some(ids[1]));
        assert_eq!(s.locator_for_loop(16 * PPQ), Some(ids[2]));
    }

    #[test]
    fn previous_and_next_skip_the_one_you_are_on() {
        let mut s = state();
        for t in [0, 8 * PPQ, 16 * PPQ] {
            s.set_locator(t);
        }
        let tick = |from, fwd| s.step_locator(from, fwd).map(|l| l.tick);
        assert_eq!(tick(8 * PPQ, true), Some(16 * PPQ));
        assert_eq!(tick(8 * PPQ, false), Some(0));
        assert_eq!(tick(9 * PPQ, false), Some(8 * PPQ));
        assert_eq!(tick(16 * PPQ, true), None);
        assert_eq!(tick(0, false), None);
    }

    #[test]
    fn quantising_a_position() {
        assert_eq!(round_to(PPQ + 10, PPQ), PPQ);
        assert_eq!(round_to(PPQ + PPQ / 2, PPQ), 2 * PPQ);
        assert_eq!(round_to(77, 1), 77);
        assert_eq!(ceil_to(PPQ + 1, PPQ), 2 * PPQ);
        assert_eq!(ceil_to(PPQ, PPQ), PPQ);
        let p = Project::new("t");
        assert_eq!(quantum(&p, Quantize::Bar, 0), 4 * PPQ);
        assert_eq!(quantum(&p, Quantize::None, 0), 1);
    }

    #[test]
    fn names_stay_inside_the_tag() {
        assert_eq!(tag_width(""), 24.);
        assert!(tag_width("Intro") > tag_width("A"));
        assert_eq!(shown_name("Drop"), "Drop");
        assert_eq!(shown_name(&"x".repeat(100)).chars().count(), 24);
        assert!(shown_name(&"x".repeat(100)).ends_with('…'));
        assert_eq!(tag_width(&"x".repeat(100)), tag_width(&"y".repeat(24)));
    }

    #[test]
    fn a_click_finds_the_locator_by_its_mark_or_its_tag() {
        let mut s = state();
        for (t, n) in [(0, "Intro"), (8 * PPQ, "Drop")] {
            let id = s.project.alloc_locator_id();
            Command::AddLocator { locator: Locator { id, tick: t, name: n.into() } }.apply(&mut s.project).unwrap();
        }
        let (intro, drop) = (s.project.locators[0].id, s.project.locators[1].id);
        let ppt = 20. / PPQ as f32; // 20 px per beat
        let x = |beat: f32| beat * 20.;
        assert_eq!(locator_at(&s.project, ppt, 0., x(0.) + 4.), Some(intro));
        assert_eq!(locator_at(&s.project, ppt, 0., x(8.) - 2.), Some(drop), "a few px before the mark counts");
        assert_eq!(locator_at(&s.project, ppt, 0., x(8.) + tag_width("Drop") - 1.), Some(drop));
        assert_eq!(locator_at(&s.project, ppt, 0., x(8.) + tag_width("Drop") + 2.), None);
        assert_eq!(locator_at(&s.project, ppt, 0., x(4.)), None, "empty ruler");
        // Scrolled: positions are in view coordinates.
        assert_eq!(locator_at(&s.project, ppt, 100., x(8.) - 100. + 3.), Some(drop));
    }
}
