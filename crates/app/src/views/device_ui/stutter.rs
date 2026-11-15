//! `fx.stutter`: the pattern as stacked lanes over the same sixteen columns.
//!
//! * **GATE** is the step on/off and its probability: click to switch, drag
//!   sideways to paint the state of the first step touched over the rest,
//!   drag up or down for the probability (the fill is its height).
//! * **DIV** is the size of the repeated chunk (click or wheel to change,
//!   drag sideways to copy it), **PITCH** is drawn directly (bars of ±12
//!   semitones), **LEVEL** is a bar per step (0 dB at the top, −60 cuts it),
//!   **TIE** links a step to the one before it, and REV / STOP / FILTER are
//!   dots (reverse, tape-stop and filter for that step).
//! * The handle after the last step sets the pattern length; the column
//!   that is playing is lit with a progress bar for the event.
//! * Copy, Paste, Rotate, ×2 and Clear work on the whole pattern, Random and
//!   Mutate rewrite it from the seed shown (one undo step each), and a
//!   right-click on a step opens its own menu.
//!
//! In Classic mode the lanes give way to the event drawn inside its
//! interval. Under either, the chunk being repeated with its loop, the read
//! position and the speed the plugin plays it at.
//!
//! Room for the per-step LEVEL and STOP lanes and the ramp curve: a lane is
//! a [`Kind`] with its fields in [`Step`]; add the field, the lane and its
//! drawing, and the pattern functions carry it.

use super::kit::{self, Rect};
use super::{Dev, ParamSet, Region, current, set_gliding, set_many, set_now};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::{Drag, KnobTarget, MenuKind, Workspace};
use aitrack_model::DeviceId;
use aitrack_stock::fx::stutter::{ID, STEPS, classic_lengths, p};
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, ScrollWheelEvent, StatefulInteractiveElement, Styled, TextAlign, canvas, div, prelude::FluentBuilder, px, relative,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// Meter layout (see the plugin): read position, state, active step, speed,
/// event progress, wet weight, then the chunk's peaks.
const READ: usize = 0;
const STATE: usize = 1;
const ACTIVE: usize = 2;
const SPEED: usize = 3;
const PROGRESS: usize = 4;
const PEAKS_AT: usize = 6;
const PEAKS: usize = 128;
const METER_LEN: usize = PEAKS_AT + PEAKS;

/// Width of the lane labels and the gap between columns.
const GUTTER: f32 = 40.;
const GAP: f32 = SP1;
const GATE_H: f32 = 40.;
const DIV_H: f32 = 24.;
const PITCH_H: f32 = 36.;
const LEVEL_H: f32 = 28.;
const SWITCH_H: f32 = 16.;
const VIEW_H: f32 = 96.;
const VIEW_LABELS: f32 = 16.;
/// Divisions a step can have, by index (the plugin's `DIV_NAMES`).
const DIV_COUNT: u32 = 5;
const SEEDS: u32 = 1000;
/// Pixels of vertical drag per DIV step.
const DIV_DRAG_PX: f32 = 10.;
/// A press moves this far (px) before it is a drag.
const DRAG_SLOP: f32 = 4.;

// ───────────────────────────── Meter helpers ─────────────────────────────

/// What the plugin is doing, from its state value.
pub fn state_name(state: f32) -> &'static str {
    match state.round() as i32 {
        1 => "repeating",
        2 => "tape-stop",
        3 => "gated",
        _ => "idle",
    }
}

/// The step the plugin is on (`None` in Classic mode, where it reports -1).
pub fn active_step(meter: f32) -> Option<usize> {
    (meter >= 0. && meter < STEPS as f32).then_some(meter as usize)
}

/// How much of its interval a Classic event fills (0..1) and whether it
/// runs on into the next one.
pub fn event_share(interval: f64, gate: f64) -> (f32, bool) {
    if interval <= 0. {
        return (0., false);
    }
    (((gate / interval) as f32).clamp(0., 1.), gate > interval + 1e-9)
}

/// The speed the chunk is read at over the course of the event, for the
/// curve drawn over the waveform. A new event (progress jumping back)
/// starts a new curve.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpeedTrace {
    pub points: Vec<(f32, f32)>,
    last: f32,
}

impl SpeedTrace {
    pub fn push(&mut self, state: f32, progress: f32, speed: f32) {
        if state.round() == 0. {
            return;
        }
        if progress + 0.2 < self.last {
            self.points.clear();
        }
        self.last = progress;
        if self.points.last().is_none_or(|(p, _)| progress > *p + 0.003) {
            self.points.push((progress.clamp(0., 1.), speed));
            if self.points.len() > 400 {
                self.points.remove(0);
            }
        }
    }
}

/// Plot fraction (0 = top) of a read speed: one octave is a quarter of the height.
fn speed_frac(speed: f32) -> f32 {
    (0.5 - 0.25 * speed.max(1e-3).log2()).clamp(0.02, 0.98)
}

// ───────────────────────────── The pattern ─────────────────────────────

/// One step's fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub on: bool,
    /// Percent.
    pub prob: f32,
    /// Index of the chunk division.
    pub div: u32,
    pub pitch: i32,
    pub rev: bool,
    /// dB, −60 (cut) to 0.
    pub level: f32,
    /// Tape-stop of this step.
    pub stop: bool,
    /// The step goes through the filter.
    pub filter: bool,
    /// The step holds the event of the one before it.
    pub tie: bool,
}

pub type Pattern = [Step; STEPS];

/// The plugin's default step: the 3-3-2 push of a house stab.
#[cfg(test)]
pub fn default_step(i: usize) -> Step {
    Step { on: matches!(i, 0 | 3 | 6 | 8 | 11 | 14), ..OFF }
}

const OFF: Step = Step { on: false, prob: 100., div: 3, pitch: 0, rev: false, level: 0., stop: false, filter: false, tie: false };

fn fields(i: usize, s: &Step) -> [(u32, f32); 9] {
    let id = |f| p::step(i as u32, f);
    [
        (id(p::STEP_ON), f32::from(s.on)),
        (id(p::STEP_PROB), s.prob),
        (id(p::STEP_DIV), s.div as f32),
        (id(p::STEP_PITCH), s.pitch as f32),
        (id(p::STEP_REV), f32::from(s.rev)),
        (id(p::STEP_LEVEL), s.level),
        (id(p::STEP_STOP), f32::from(s.stop)),
        (id(p::STEP_FILTER), f32::from(s.filter)),
        (p::tie(i as u32), f32::from(s.tie)),
    ]
}

/// The pattern as the project stores it.
pub fn read_pattern(set: &ParamSet, values: &BTreeMap<u32, f32>) -> Pattern {
    let plain = |id: u32| {
        let info = set.get(id);
        info.map_or(0., |i| i.to_plain(values.get(&id).copied().unwrap_or_else(|| i.default_normalized())))
    };
    std::array::from_fn(|i| {
        let id = |f| p::step(i as u32, f);
        Step {
            on: plain(id(p::STEP_ON)) >= 0.5,
            prob: plain(id(p::STEP_PROB)).clamp(0., 100.),
            div: plain(id(p::STEP_DIV)).round().clamp(0., (DIV_COUNT - 1) as f32) as u32,
            pitch: plain(id(p::STEP_PITCH)).round().clamp(-12., 12.) as i32,
            rev: plain(id(p::STEP_REV)) >= 0.5,
            level: plain(id(p::STEP_LEVEL)).clamp(-60., 0.),
            stop: plain(id(p::STEP_STOP)) >= 0.5,
            filter: plain(id(p::STEP_FILTER)) >= 0.5,
            tie: plain(p::tie(i as u32)) >= 0.5,
        }
    })
}

/// The parameter edits (normalised) that turn `from` into `to`; steps that
/// stay as they are cost nothing. `extra` plain values ride along.
pub fn diff(set: &ParamSet, from: &Pattern, to: &Pattern, extra: &[(u32, f32)]) -> Vec<(u32, f32)> {
    let mut out = Vec::new();
    for i in 0..STEPS {
        if from[i] == to[i] {
            continue;
        }
        let (a, b) = (fields(i, &from[i]), fields(i, &to[i]));
        for (x, y) in a.iter().zip(b.iter()) {
            if x.1 != y.1 {
                out.push(*y);
            }
        }
    }
    out.extend_from_slice(extra);
    out.into_iter().filter_map(|(id, v)| set.get(id).map(|i| (id, i.to_normalized(v)))).collect()
}

/// The first `length` steps moved by `by` (wrapping inside them).
pub fn rotated(p: &Pattern, length: usize, by: i32) -> Pattern {
    let mut out = *p;
    let n = length.clamp(1, STEPS) as i32;
    for i in 0..n {
        out[((i + by).rem_euclid(n)) as usize] = p[i as usize];
    }
    out
}

/// The pattern doubled: its steps repeated right after themselves, and the
/// length to match. A pattern longer than half the grid has its first half
/// copied over the second.
pub fn doubled(p: &Pattern, length: usize) -> (Pattern, usize) {
    let length = length.clamp(1, STEPS);
    let half = if length * 2 <= STEPS { length } else { STEPS / 2 };
    let mut out = *p;
    out[half..half * 2].copy_from_slice(&p[..half]);
    (out, half * 2)
}

/// Every step off and the fields back at their defaults.
pub fn cleared() -> Pattern {
    [OFF; STEPS]
}

/// `step` copied to every `every`-th step from it (and from it backwards)
/// inside `length`; the other steps inside it are switched off.
pub fn filled(p: &Pattern, length: usize, step: usize, every: usize) -> Pattern {
    let mut out = *p;
    let every = every.max(1);
    for i in 0..length.clamp(1, STEPS) {
        out[i] = if (i + every * STEPS - step).is_multiple_of(every) { Step { on: true, ..p[step.min(STEPS - 1)] } } else { Step { on: false, ..p[i] } };
    }
    out
}

/// A small deterministic generator for the dice (not for audio).
struct Dice(u64);

impl Dice {
    fn new(seed: u32) -> Self {
        Self(u64::from(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / 16_777_216.0
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[(self.next() * from.len() as f32) as usize % from.len()]
    }
}

/// The next seed in the chain a Mutate walks.
pub fn next_seed(seed: u32) -> u32 {
    (Dice::new(seed).next() * SEEDS as f32) as u32 % SEEDS
}

/// A whole new pattern from `seed`: mostly full-chance steps of a sixteenth
/// or a thirty-second, some pitched, a few backwards; the first step is
/// always on so the pattern has a pulse. The same seed gives the same pattern.
pub fn randomized(seed: u32) -> Pattern {
    let mut dice = Dice::new(seed);
    std::array::from_fn(|i| Step {
        on: i == 0 || dice.next() < 0.42,
        prob: if dice.next() < 0.7 { 100. } else { dice.pick(&[50., 60., 75., 85.]) },
        div: dice.pick(&[1, 1, 1, 3, 3, 0, 2, 4]),
        pitch: if dice.next() < 0.75 { 0 } else { dice.pick(&[-12, -7, -5, 5, 7, 12]) },
        rev: dice.next() < 0.12,
        level: if dice.next() < 0.8 { 0. } else { dice.pick(&[-3., -6., -9., -12.]) },
        stop: dice.next() < 0.04,
        filter: dice.next() < 0.1,
        tie: i > 0 && dice.next() < 0.1,
    })
}

/// A small variation: two or three steps inside `length` change one thing
/// each. Always differs from `p`.
pub fn mutated(p: &Pattern, length: usize, seed: u32) -> Pattern {
    let mut dice = Dice::new(seed);
    let length = length.clamp(1, STEPS);
    let mut out = *p;
    let count = if length < 3 { length } else { 2 + usize::from(dice.next() < 0.5) };
    let mut done = Vec::new();
    while done.len() < count {
        let i = (dice.next() * length as f32) as usize % length;
        if done.contains(&i) {
            continue;
        }
        done.push(i);
        let s = &mut out[i];
        match dice.next() {
            r if r < 0.3 => s.on = !s.on,
            r if r < 0.45 => s.prob = dice.pick(&[50., 60., 75., 85., 100.]),
            r if r < 0.57 => s.div = dice.pick(&[0, 1, 2, 3, 4]),
            r if r < 0.69 => s.pitch = if s.pitch == 0 { dice.pick(&[-12, -7, -5, 5, 7, 12]) } else { 0 },
            r if r < 0.77 => s.rev = !s.rev,
            r if r < 0.85 => s.level = if s.level == 0. { dice.pick(&[-3., -6., -9., -12.]) } else { 0. },
            r if r < 0.9 => s.filter = !s.filter,
            r if r < 0.94 => s.stop = !s.stop,
            _ => s.tie = i > 0 && !s.tie,
        }
    }
    if out == *p {
        out[done[0]].on = !out[done[0]].on;
    }
    out
}

thread_local! {
    static CLIP_PATTERN: RefCell<Option<Pattern>> = const { RefCell::new(None) };
    static CLIP_STEP: Cell<Option<Step>> = const { Cell::new(None) };
    static TRACES: RefCell<HashMap<DeviceId, SpeedTrace>> = RefCell::new(HashMap::new());
    static GESTURE: RefCell<Option<Gesture>> = const { RefCell::new(None) };
    static HOLD_DOWN: Cell<Option<DeviceId>> = const { Cell::new(None) };
}

/// A fresh seed for the dice (an interactive choice, kept in the project as the Seed).
fn fresh_seed() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| (d.subsec_nanos() ^ d.as_secs() as u32) % SEEDS)
}

fn pattern_of(ws: &Workspace, device: DeviceId) -> Option<(Rc<ParamSet>, Pattern, usize)> {
    let d = ws.s.project.device(device)?;
    let stock = ws.b.plugins.device(ID)?;
    let set = Rc::new(ParamSet::new(stock.params.clone()));
    let length = {
        let info = set.get(p::LENGTH)?;
        info.to_plain(d.params.get(&p::LENGTH).copied().unwrap_or_else(|| info.default_normalized())).round().max(0.) as usize + 1
    };
    let pattern = read_pattern(&set, &d.params);
    Some((set, pattern, length))
}

fn apply(ws: &mut Workspace, set: &ParamSet, device: DeviceId, label: &str, from: &Pattern, to: &Pattern, extra: &[(u32, f32)]) {
    let edits = diff(set, from, to, extra);
    if !edits.is_empty() {
        set_many(ws, label, device, &edits);
    }
}

fn length_plain(length: usize) -> (u32, f32) {
    (p::LENGTH, (length.clamp(1, STEPS) - 1) as f32)
}

// ───────────────────────────── Gestures ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Gate,
    Div,
    Pitch,
    Level,
    Switch(Sw),
    Length,
}

/// The on/off lanes under the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sw {
    Tie,
    Rev,
    Stop,
    Filter,
}

impl Sw {
    fn param(self, step: usize) -> u32 {
        match self {
            Sw::Tie => p::tie(step as u32),
            Sw::Rev => p::step(step as u32, p::STEP_REV),
            Sw::Stop => p::step(step as u32, p::STEP_STOP),
            Sw::Filter => p::step(step as u32, p::STEP_FILTER),
        }
    }
}

/// The parameter a lane edits on `step` (the gate's own is the on/off; its
/// probability is edited by the vertical drag).
fn lane_param(kind: Kind, step: usize) -> Option<u32> {
    let id = |f| p::step(step as u32, f);
    match kind {
        Kind::Gate => Some(id(p::STEP_ON)),
        Kind::Div => Some(id(p::STEP_DIV)),
        Kind::Pitch => Some(id(p::STEP_PITCH)),
        Kind::Level => Some(id(p::STEP_LEVEL)),
        Kind::Switch(sw) => Some(sw.param(step)),
        Kind::Length => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// Pressed, not yet moved enough to be a drag.
    Pending,
    /// Sideways: the value of the first step goes onto every step swept over.
    Paint,
    /// Up and down on a DIV chip.
    Vertical,
    /// The value follows the pointer (pitch, length).
    Draw,
}

#[derive(Clone)]
struct Gesture {
    device: DeviceId,
    kind: Kind,
    mode: Mode,
    set: Rc<ParamSet>,
    region: Region,
    start: (f32, f32),
    step: usize,
    last: usize,
    shift: bool,
    /// The first step's plain value (a DIV index, or 0/1 for a switch).
    v0: f32,
    /// What painting sets.
    paint: f32,
}

/// The column under `x` (window pixels) in a lane whose cells fill `b`.
fn step_at(b: Bounds<Pixels>, x: f32) -> usize {
    let pitch = (f32::from(b.size.width) + GAP) / STEPS as f32;
    (((x - f32::from(b.origin.x)) / pitch.max(1.)).floor().max(0.) as usize).min(STEPS - 1)
}

/// Semitones for a pointer height in a pitch lane (top is +12).
fn pitch_at(b: Bounds<Pixels>, y: f32) -> i32 {
    let f = ((y - f32::from(b.origin.y)) / f32::from(b.size.height).max(1.)).clamp(0., 1.);
    (12. - 24. * f).round().clamp(-12., 12.) as i32
}

/// Level (dB) for a pointer height in a level lane: the top is 0 dB, the
/// bottom −60 (cut), in whole decibels.
fn level_at(b: Bounds<Pixels>, y: f32) -> f32 {
    let f = ((y - f32::from(b.origin.y)) / f32::from(b.size.height).max(1.)).clamp(0., 1.);
    (-60. * f).round()
}

/// What a drawn lane puts under the pointer at height `y`.
fn drawn_value(kind: Kind, b: Bounds<Pixels>, y: f32) -> f32 {
    if kind == Kind::Level { level_at(b, y) } else { pitch_at(b, y) as f32 }
}

/// The pattern length for a pointer position in a lane (the boundary nearest).
fn length_at(b: Bounds<Pixels>, x: f32) -> usize {
    let pitch = (f32::from(b.size.width) + GAP) / STEPS as f32;
    (((x - f32::from(b.origin.x)) / pitch.max(1.)).round().max(1.) as usize).min(STEPS)
}

fn plain_of(ws: &Workspace, set: &ParamSet, device: DeviceId, id: u32) -> f32 {
    set.get(id).map_or(0., |i| i.to_plain(current(ws, device, i)))
}

fn glide(ws: &mut Workspace, g: &Gesture, edits: &[(u32, f32)]) {
    let norms: Vec<(u32, f32)> = edits.iter().filter_map(|(id, v)| g.set.get(*id).map(|i| (*id, i.to_normalized(*v)))).collect();
    set_gliding(ws, g.device, &norms);
}

/// Put `value` on `step` for a gesture's kind.
fn put(ws: &mut Workspace, g: &Gesture, step: usize, value: f32) {
    if let Some(id) = lane_param(g.kind, step) {
        glide(ws, g, &[(id, value)]);
    }
}

fn start_gesture(ws: &mut Workspace, g: Gesture, cx: &mut Context<Workspace>) {
    ws.s.seal();
    match g.kind {
        Kind::Pitch | Kind::Level | Kind::Length => {
            let mut g = g;
            g.mode = Mode::Draw;
            let b = g.region.get();
            if g.kind != Kind::Length {
                put(ws, &g, g.step, drawn_value(g.kind, b, g.start.1));
            } else {
                let (id, v) = length_plain(length_at(b, g.start.0));
                glide(ws, &g, &[(id, v)]);
            }
            GESTURE.set(Some(g));
        }
        _ => GESTURE.set(Some(g)),
    }
    cx.notify();
}

fn gesture_move(ws: &mut Workspace, e: &MouseMoveEvent, cx: &mut Context<Workspace>) {
    let Some(mut g) = GESTURE.take() else { return };
    let (x, y) = (f32::from(e.position.x), f32::from(e.position.y));
    let b = g.region.get();
    let here = step_at(b, x);
    if g.mode == Mode::Pending {
        let (dx, dy) = (x - g.start.0, y - g.start.1);
        if dx.hypot(dy) >= DRAG_SLOP {
            if dx.abs() >= dy.abs() {
                g.mode = Mode::Paint;
                let first = g.step;
                put(ws, &g, first, g.paint);
            } else if g.kind == Kind::Gate {
                // Vertical on a gate cell is the probability: the shared knob drag does it.
                let id = p::step(g.step as u32, p::STEP_PROB);
                if let Some(info) = g.set.get(id) {
                    let default = info.default_normalized();
                    let value = current(ws, g.device, info);
                    ws.begin_drag(Drag::Knob { target: KnobTarget::Param { device: g.device, param: id, default }, last_y: g.start.1, value }, cx);
                }
                cx.notify();
                return;
            } else {
                g.mode = Mode::Vertical;
            }
        }
    }
    match g.mode {
        Mode::Paint => {
            let (lo, hi) = (g.last.min(here), g.last.max(here));
            for s in lo..=hi {
                put(ws, &g, s, g.paint);
            }
            g.last = here;
        }
        Mode::Vertical => {
            let steps = ((g.start.1 - y) / DIV_DRAG_PX).round();
            let v = (g.v0 + steps).clamp(0., (DIV_COUNT - 1) as f32);
            put(ws, &g, g.step, v);
        }
        Mode::Draw => {
            if g.kind != Kind::Length {
                put(ws, &g, here, drawn_value(g.kind, b, y));
            } else {
                let (id, v) = length_plain(length_at(b, x));
                glide(ws, &g, &[(id, v)]);
            }
        }
        Mode::Pending => {}
    }
    GESTURE.set(Some(g));
    cx.notify();
}

fn gesture_up(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if let Some(device) = HOLD_DOWN.take() {
        release_hold(ws, device);
    }
    let Some(g) = GESTURE.take() else { return };
    if g.mode == Mode::Pending {
        // A click: switch, or step through the divisions.
        match g.kind {
            Kind::Gate | Kind::Switch(_) => put_now(ws, &g, g.paint),
            Kind::Div => {
                let next = (g.v0 + if g.shift { DIV_COUNT as f32 - 1. } else { 1. }) % DIV_COUNT as f32;
                put_now(ws, &g, next);
            }
            _ => {}
        }
    }
    ws.s.seal();
    cx.notify();
}

fn put_now(ws: &mut Workspace, g: &Gesture, value: f32) {
    if matches!(g.kind, Kind::Pitch | Kind::Level | Kind::Length) {
        return;
    }
    if let Some(id) = lane_param(g.kind, g.step)
        && let Some(info) = g.set.get(id)
    {
        set_now(ws, g.device, id, info.to_normalized(value));
    }
}

/// Window-wide listeners, present only while a lane gesture or the HOLD pad is held.
fn capture(device: DeviceId, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let active = GESTURE.with(|g| g.borrow().as_ref().is_some_and(|g| g.device == device)) || HOLD_DOWN.get() == Some(device);
    if !active {
        return None;
    }
    let weak = cx.entity().downgrade();
    Some(
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let w = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, |this, cx| gesture_move(this, e, cx));
                    }
                });
                let w = weak.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, gesture_up);
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element(),
    )
}

// ───────────────────────────── HOLD ─────────────────────────────

fn hold_edit(ws: &mut Workspace, device: DeviceId, on: bool) {
    let Some(info) = ws.b.plugins.device(ID).and_then(|d| d.params.iter().find(|i| i.id == p::HOLD).cloned()) else { return };
    set_gliding(ws, device, &[(p::HOLD, info.to_normalized(f32::from(on)))]);
}

/// The pad pressed: held while the button is down, or latched with Shift.
fn press_hold(ws: &mut Workspace, device: DeviceId, was_on: bool, latch: bool, cx: &mut Context<Workspace>) {
    ws.s.seal();
    if latch {
        hold_edit(ws, device, !was_on);
        ws.s.seal();
    } else {
        hold_edit(ws, device, true);
        HOLD_DOWN.set(Some(device));
    }
    cx.notify();
}

fn release_hold(ws: &mut Workspace, device: DeviceId) {
    hold_edit(ws, device, false);
    ws.s.seal();
}

/// The shortcut: switch Hold on or off on the selected track's Stutter.
pub fn toggle_hold(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let found = ws.s.selected_track.and_then(|t| ws.s.project.track(t)).and_then(|t| {
        t.devices.iter().find(|d| matches!(&d.kind, aitrack_model::DeviceKind::Stock { plugin_id } if plugin_id == ID)).map(|d| (d.id, d.params.get(&p::HOLD).copied().unwrap_or(0.) >= 0.5))
    });
    match found {
        Some((device, on)) => {
            hold_edit(ws, device, !on);
            ws.s.seal();
            ws.toast(if on { "Stutter hold off" } else { "Stutter hold on" }, cx);
        }
        None => ws.toast("Select a track with a Stutter to hold", cx),
    }
}

// ───────────────────────────── The step menu ─────────────────────────────

pub fn step_menu(_ws: &mut Workspace, device: DeviceId, step: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let mut card = widgets::menu_card("stutter-step-menu", 176.).child(widgets::menu_header(format!("STEP {}", step + 1)));
    let items: [(&'static str, Option<Icon>); 5] =
        [("Copy step", Some(Icon::Copy)), ("Paste step", Some(Icon::Paste)), ("Clear step", Some(Icon::Trash)), ("Fill every 2", None), ("Fill every 4", None)];
    for (n, (text, ic)) in items.into_iter().enumerate() {
        card = card.child(widgets::menu_item(("stutter-step", n), ic, text, None, false).on_click(cx.listener(move |this, _, _, cx| {
            this.menu = None;
            let Some((set, pattern, length)) = pattern_of(this, device) else { return };
            let mut new = pattern;
            match n {
                0 => CLIP_STEP.set(Some(pattern[step])),
                1 => {
                    if let Some(s) = CLIP_STEP.get() {
                        new[step] = s;
                    }
                }
                2 => new[step] = OFF,
                3 => new = filled(&pattern, length, step, 2),
                _ => new = filled(&pattern, length, step, 4),
            }
            apply(this, &set, device, "Edit pattern", &pattern, &new, &[]);
            cx.notify();
        })));
    }
    card.into_any_element()
}

// ───────────────────────────── Drawing ─────────────────────────────

/// Plot fraction (0 = top) of a ramp's value in the chunk viewer: pitch in
/// semitones (±48), the filter in octaves (±4) and the level in dB (−60..+12).
pub fn ramp_frac(ramp: Ramp, value: f32) -> f32 {
    let f = match ramp {
        Ramp::Pitch => 0.5 - value / 96.,
        Ramp::Filter => 0.5 - value / 8.,
        Ramp::Level => 1. - (value + 60.) / 72.,
    };
    f.clamp(0.02, 0.98)
}

/// The three ramps a Stutter event can run: each goes from its own value
/// to that value plus its End (the change reached at the end of the event).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ramp {
    Pitch,
    Filter,
    Level,
}

/// `(ramp, from, to)` for every ramp that moves (an End of 0 is no ramp).
pub fn active_ramps(pitch: f32, pitch_end: f32, filter_end: f32, level: f32, level_end: f32) -> Vec<(Ramp, f32, f32)> {
    let mut out = Vec::new();
    if pitch_end != 0. {
        out.push((Ramp::Pitch, pitch, pitch + pitch_end));
    }
    if filter_end != 0. {
        out.push((Ramp::Filter, 0., filter_end));
    }
    if level_end != 0. {
        out.push((Ramp::Level, level, level + level_end));
    }
    out
}

/// A pair of controls for a ramp: where it starts, an arrow, and the change it reaches.
fn ramp_pair(ws: &mut Workspace, dev: &Dev, from: u32, to: u32, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(kit::num(ws, dev, from, cx))
        .child(icon(Icon::ChevronRight, 12., p().faint))
        .child(kit::num(ws, dev, to, cx))
        .into_any_element()
}

fn small_button(id: impl Into<gpui::ElementId>, ic: Icon, text: &'static str, tip: &'static str) -> gpui::Stateful<gpui::Div> {
    let pal = p();
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
        .bg(content(0.06))
        .hover(|s| s.bg(content(0.12)))
        .active(|s| s.bg(pressed()))
        .tooltip(widgets::tip(tip, None))
        .child(icon(ic, 12., pal.muted))
        .when(!text.is_empty(), |d| d.child(label(text, TEXT_XS, pal.text).font_weight(FontWeight::MEDIUM)))
}

/// What a lane draws for one column.
struct Col {
    step: Step,
    inside: bool,
    playing: bool,
}

fn column_tint(i: usize, c: &Col) -> gpui::Hsla {
    if c.playing { accent_alpha(0.16) } else { content(if (i / 4).is_multiple_of(2) { 0.05 } else { 0.03 }) }
}

fn div_name(i: u32) -> &'static str {
    ["1/8", "1/16", "1/16T", "1/32", "1/32T"][(i as usize).min(4)]
}

#[allow(clippy::too_many_arguments)]
fn lane(
    ws: &mut Workspace,
    dev: &Dev,
    kind: Kind,
    slot: u8,
    height: f32,
    name: &'static str,
    cells: &[Col],
    progress: Option<f32>,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let pal = p();
    let region = ws.fx.region(dev.id, slot);
    let (device, set) = (dev.id, dev.set.clone());
    let store = region.clone();
    let measure = canvas(move |b, _, _| store.set(b), |_, _, _, _| {}).absolute().size_full();
    let mut row = div().relative().flex_1().min_w(px(0.)).h_full().flex().gap(px(GAP));
    for (i, c) in cells.iter().enumerate() {
        let s = c.step;
        let live = c.inside;
        let mut cell = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .relative()
            .rounded(px(r_sm()))
            .bg(column_tint(i, c))
            .when(c.playing, |d| d.border_1().border_color(with_alpha(pal.text, 0.55)))
            .when(!live, |d| d.opacity(0.5));
        cell = match kind {
            Kind::Gate => {
                let fill = if !live { content(0.1) } else if s.on { with_alpha(pal.accent, if c.playing { 1. } else { 0.75 }) } else { content(0.16) };
                let h = if s.on { s.prob / 100. } else { s.prob / 100. * 0.35 };
                // A tied step holds the event of the one before: a bar across the gap links them.
                let tied = s.tie && i > 0;
                cell.child(div().absolute().left_0().right_0().bottom_0().h(relative(h.max(0.06))).bg(if tied { with_alpha(pal.accent, 0.35) } else { fill }))
                    .when(tied, |d| d.child(div().absolute().left(px(-GAP)).w(px(GAP)).top(px(SP1)).h(px(SP1)).bg(pal.accent)).child(div().absolute().left_0().right_0().top(px(SP1)).h(px(SP1)).bg(pal.accent)))
            }
            Kind::Div => cell.flex().items_center().justify_center().child(mono(div_name(s.div), TEXT_XS, if s.on { pal.text } else { pal.faint })),
            Kind::Pitch => {
                let bar = s.pitch.abs() as f32 / 12. * 0.5;
                let colour = if s.on { pal.accent } else { pal.faint };
                cell.child(div().absolute().left_0().right_0().top(relative(0.5)).h(px(1.)).bg(content(0.2)))
                    .when(s.pitch > 0, |d| d.child(div().absolute().left(px(SP1)).right(px(SP1)).bottom(relative(0.5)).h(relative(bar)).bg(colour)))
                    .when(s.pitch < 0, |d| d.child(div().absolute().left(px(SP1)).right(px(SP1)).top(relative(0.5)).h(relative(bar)).bg(colour)))
            }
            Kind::Level => {
                let frac = (s.level + 60.) / 60.;
                let colour = if !live { content(0.1) } else if s.on { with_alpha(pal.accent2, 0.85) } else { content(0.2) };
                cell.child(div().absolute().left(px(SP1)).right(px(SP1)).bottom_0().h(relative(frac.clamp(0.04, 1.))).bg(colour))
            }
            Kind::Switch(sw) => {
                let set = match sw {
                    Sw::Tie => s.tie,
                    Sw::Rev => s.rev,
                    Sw::Stop => s.stop,
                    Sw::Filter => s.filter,
                };
                // A tie is a bar; the others are dots.
                let mark = if sw == Sw::Tie {
                    div().w(relative(1.)).h(px(4.)).bg(if set { pal.accent } else { content(0.1) })
                } else {
                    div().size(px(8.)).rounded_full().bg(if set { pal.accent } else { content(0.14) })
                };
                cell.flex()
                    .items_center()
                    .justify_center()
                    .child(mark)
                    .when_some(progress.filter(|_| c.playing && sw == Sw::Filter), |d, pr| d.child(div().absolute().left_0().bottom_0().h(px(2.)).w(relative(pr)).bg(pal.accent)))
            }
            Kind::Length => cell,
        };
        row = row.child(cell);
    }
    row = row.child(measure);
    let wheel_set = set.clone();
    let down_region = region.clone();
    let wheel_region = region.clone();
    row = row
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let b = down_region.get();
                let (x, y) = (f32::from(e.position.x), f32::from(e.position.y));
                let step = step_at(b, x);
                if e.click_count >= 2 {
                    let reset = match kind {
                        Kind::Gate => Some((p::step(step as u32, p::STEP_PROB), 100.)),
                        Kind::Div => Some((p::step(step as u32, p::STEP_DIV), 3.)),
                        Kind::Pitch => Some((p::step(step as u32, p::STEP_PITCH), 0.)),
                        Kind::Level => Some((p::step(step as u32, p::STEP_LEVEL), 0.)),
                        Kind::Switch(sw) => Some((sw.param(step), 0.)),
                        Kind::Length => None,
                    };
                    if let Some((id, v)) = reset
                        && let Some(info) = set.get(id)
                    {
                        set_now(this, device, id, info.to_normalized(v));
                    }
                    return;
                }
                let Some(field) = lane_param(kind, step) else { return };
                let v0 = plain_of(this, &set, device, field);
                let paint = if matches!(kind, Kind::Gate | Kind::Switch(_)) { f32::from(v0 < 0.5) } else { v0 };
                let g = Gesture { device, kind, mode: Mode::Pending, set: set.clone(), region: down_region.clone(), start: (x, y), step, last: step, shift: e.modifiers.shift, v0, paint };
                start_gesture(this, g, cx);
            }),
        )
        .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| gesture_up(this, cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let step = step_at(region.get(), f32::from(e.position.x));
                this.open_menu(MenuKind::StutterStep { device, step }, e.position, cx);
            }),
        );
    if kind == Kind::Div {
        row = row.on_scroll_wheel(cx.listener(move |this, e: &ScrollWheelEvent, _, cx| {
            let step = step_at(wheel_region.get(), f32::from(e.position.x));
            let id = p::step(step as u32, p::STEP_DIV);
            let dy = match e.delta {
                gpui::ScrollDelta::Lines(l) => l.y,
                gpui::ScrollDelta::Pixels(px) => f32::from(px.y) / 25.,
            };
            if dy.abs() < 0.05 {
                return;
            }
            cx.stop_propagation();
            let v = (plain_of(this, &wheel_set, device, id) + dy.signum()).clamp(0., (DIV_COUNT - 1) as f32);
            if let Some(info) = wheel_set.get(id) {
                set_now(this, device, id, info.to_normalized(v));
            }
            cx.notify();
        }));
    }
    div()
        .flex_none()
        .h(px(height))
        .flex()
        .gap(px(GAP))
        .child(div().flex_none().w(px(GUTTER)).flex().items_center().child(label(name, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD)))
        .child(row)
}

fn lanes(ws: &mut Workspace, dev: &Dev, pattern: &Pattern, length: usize, active: Option<usize>, progress: Option<f32>, cx: &mut Context<Workspace>) -> gpui::Div {
    let pal = p();
    let cells: Vec<Col> = (0..STEPS).map(|i| Col { step: pattern[i], inside: i < length, playing: active == Some(i) }).collect();
    let numbers = div()
        .flex_none()
        .h(px(16.))
        .flex()
        .gap(px(GAP))
        .child(div().flex_none().w(px(GUTTER)))
        .child(div().flex_1().min_w(px(0.)).flex().gap(px(GAP)).children((0..STEPS).map(|i| {
            div().flex_1().min_w(px(0.)).flex().justify_center().child(mono(format!("{}", i + 1), TEXT_XS, if active == Some(i) { pal.text } else if i % 4 == 0 { pal.muted } else { pal.faint }))
        })));
    let gate = lane(ws, dev, Kind::Gate, 11, GATE_H, "GATE", &cells, None, cx);
    let tie = lane(ws, dev, Kind::Switch(Sw::Tie), 15, SWITCH_H, "TIE", &cells, None, cx);
    let div_lane = lane(ws, dev, Kind::Div, 12, DIV_H, "DIV", &cells, None, cx);
    let pitch = lane(ws, dev, Kind::Pitch, 13, PITCH_H, "PITCH", &cells, None, cx);
    let level = lane(ws, dev, Kind::Level, 14, LEVEL_H, "LEVEL", &cells, None, cx);
    let rev = lane(ws, dev, Kind::Switch(Sw::Rev), 16, SWITCH_H, "REV", &cells, None, cx);
    let stop = lane(ws, dev, Kind::Switch(Sw::Stop), 17, SWITCH_H, "STOP", &cells, None, cx);
    let filter = lane(ws, dev, Kind::Switch(Sw::Filter), 18, SWITCH_H, "FILTER", &cells, progress, cx);
    // The length handle sits on the boundary after the last step.
    let region = ws.fx.region(dev.id, 11);
    let (device, set) = (dev.id, dev.set.clone());
    let handle = div()
        .absolute()
        .top(px(16.))
        .bottom_0()
        .left(px(GUTTER + GAP))
        .right_0()
        .child(
            div()
                .id(("stutter-length", dev.key(0)))
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(length as f32 / STEPS as f32))
                .ml(px(-SP2))
                .w(px(SP4))
                .flex()
                .justify_center()
                .cursor_ew_resize()
                .tooltip(widgets::tip_owned(format!("Pattern length: {length} steps — drag")))
                .child(div().w(px(3.)).h_full().rounded(px(2.)).bg(pal.accent))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let g = Gesture {
                            device,
                            kind: Kind::Length,
                            mode: Mode::Draw,
                            set: set.clone(),
                            region: region.clone(),
                            start: (f32::from(e.position.x), f32::from(e.position.y)),
                            step: 0,
                            last: 0,
                            shift: false,
                            v0: 0.,
                            paint: 0.,
                        };
                        start_gesture(this, g, cx);
                    }),
                ),
        );
    div().flex_none().relative().flex().flex_col().gap(px(SP1)).child(numbers).child(gate).child(tie).child(div_lane).child(pitch).child(level).child(rev).child(stop).child(filter).child(handle)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let grid_mode = dev.choice(p::MODE) == 1;
    let live = dev.meter.len() >= METER_LEN;
    let length = dev.choice(p::LENGTH) + 1;
    let active = if live { active_step(dev.m(ACTIVE)) } else { None };
    let (read, state, progress, speed) = (dev.m(READ), dev.m(STATE), dev.m(PROGRESS), dev.m(SPEED));
    let peaks: Vec<(f32, f32)> = if live { dev.meter[PEAKS_AT..PEAKS_AT + PEAKS].iter().map(|v| (-v, *v)).collect() } else { Vec::new() };
    let trace = TRACES.with(|t| {
        let mut t = t.borrow_mut();
        let tr = t.entry(dev.id).or_default();
        if live {
            tr.push(state, progress, speed);
        }
        tr.clone()
    });
    let enabled = dev.enabled;
    let ramps = active_ramps(dev.plain(p::PITCH), dev.plain(p::PITCH_END), dev.plain(p::FILTER_END), dev.plain(p::LEVEL), dev.plain(p::LEVEL_END));
    let pattern = read_pattern(&dev.set, &dev.values);
    let hold_on = dev.on(p::HOLD);
    let seed = dev.plain(p::SEED) as u32;
    let (interval, gate) = classic_lengths(dev.choice(p::INTERVAL), dev.choice(p::GATE), 4.);
    let (share, overlaps) = event_share(interval, gate);
    let classic_title = format!("every {} · event {}{}", dev.text(p::INTERVAL), dev.text(p::GATE), if overlaps { " (runs into the next)" } else { "" });

    // ── Header ──
    let device = dev.id;
    let pad = div()
        .id(("stutter-hold", dev.key(0)))
        .flex_none()
        .w(px(56.))
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .border_1()
        .border_color(if hold_on { accent() } else { border_strong() })
        .bg(if hold_on { accent_alpha(0.35) } else { content(0.06) })
        .tooltip(widgets::tip("Hold: keep it pressed to repeat now; Shift+click latches it", Some("Shift+Alt+H")))
        .child(label("HOLD", TEXT_XS, if hold_on { p().text } else { p().muted }).font_weight(FontWeight::SEMIBOLD))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                press_hold(this, device, hold_on, e.modifiers.shift, cx);
            }),
        )
        .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| gesture_up(this, cx)));
    let cur = pattern;
    let set = dev.set.clone();
    let dice = small_button(("stutter-dice", dev.key(0)), Icon::Dice, "", "Random pattern from a new seed").on_click({
        let set = set.clone();
        cx.listener(move |this, _, _, cx| {
            let seed = fresh_seed();
            apply(this, &set, device, "Randomize pattern", &cur, &randomized(seed), &[(p::SEED, seed as f32)]);
            cx.notify();
        })
    });
    let mutate = small_button(("stutter-mutate", dev.key(0)), Icon::Shuffle, "Mutate", "Change two or three steps a little").on_click({
        let set = set.clone();
        cx.listener(move |this, _, _, cx| {
            apply(this, &set, device, "Mutate pattern", &cur, &mutated(&cur, length, seed), &[(p::SEED, next_seed(seed) as f32)]);
            cx.notify();
        })
    });
    let head = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(kit::pick(dev, p::MODE, cx))
        .child(pad)
        .child(dice)
        .child(mutate)
        .child(kit::num_shown(ws, dev, p::SEED, 56., Some(format!("{seed}")), cx))
        .child(div().flex_1())
        .child(kit::pick(dev, p::MIX_MODE, cx))
        .child(kit::num(ws, dev, p::MIX, cx))
        .child(kit::num(ws, dev, p::OUTPUT, cx));

    // ── Pattern tools ──
    let tools = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(small_button(("st-copy", dev.key(0)), Icon::Copy, "Copy", "Copy the whole pattern").on_click(cx.listener(move |_, _, _, cx| {
            CLIP_PATTERN.with(|c| *c.borrow_mut() = Some(cur));
            cx.notify();
        })))
        .child(small_button(("st-paste", dev.key(0)), Icon::Paste, "Paste", "Paste the copied pattern").on_click({
            let set = set.clone();
            cx.listener(move |this, _, _, cx| {
                if let Some(p) = CLIP_PATTERN.with(|c| *c.borrow()) {
                    apply(this, &set, device, "Paste pattern", &cur, &p, &[]);
                }
                cx.notify();
            })
        }))
        .child(small_button(("st-left", dev.key(0)), Icon::ChevronLeft, "", "Rotate the pattern one step earlier").on_click({
            let set = set.clone();
            cx.listener(move |this, _, _, cx| {
                apply(this, &set, device, "Rotate pattern", &cur, &rotated(&cur, length, -1), &[]);
                cx.notify();
            })
        }))
        .child(small_button(("st-right", dev.key(0)), Icon::ChevronRight, "", "Rotate the pattern one step later").on_click({
            let set = set.clone();
            cx.listener(move |this, _, _, cx| {
                apply(this, &set, device, "Rotate pattern", &cur, &rotated(&cur, length, 1), &[]);
                cx.notify();
            })
        }))
        .child(small_button(("st-double", dev.key(0)), Icon::Add, "×2", "Repeat the pattern after itself and double its length").on_click({
            let set = set.clone();
            cx.listener(move |this, _, _, cx| {
                let (p2, l2) = doubled(&cur, length);
                apply(this, &set, device, "Double pattern", &cur, &p2, &[length_plain(l2)]);
                cx.notify();
            })
        }))
        .child(small_button(("st-clear", dev.key(0)), Icon::Trash, "Clear", "Switch every step off").on_click({
            let set = set.clone();
            cx.listener(move |this, _, _, cx| {
                apply(this, &set, device, "Clear pattern", &cur, &cleared(), &[]);
                cx.notify();
            })
        }));

    // ── Top: the lanes, or the event in its interval ──
    let top = if grid_mode {
        let pr = (live && active.is_some()).then_some(progress.clamp(0., 1.));
        lanes(ws, dev, &pattern, length, active, pr, cx)
    } else {
        let lane_plot = kit::plot(ws, dev, kit::PlotCfg { slot: 1, ..Default::default() }, cx, move |r: Rect, window, cx| {
            let pal = p();
            let colour = if enabled { pal.accent } else { pal.muted };
            kit::bar(window, r.x + 8., r.py(0.4), r.w - 16., r.h * 0.2, 3., content(0.08));
            kit::bar(window, r.x + 8., r.py(0.4), (r.w - 16.) * share, r.h * 0.2, 3., with_alpha(colour, 0.7));
            if live {
                kit::bar(window, r.x + 8. + (r.w - 16.) * share * progress, r.py(0.34), 2., r.h * 0.32, 1., pal.text);
            }
            kit::txt(window, cx, &classic_title, r.x + 8., r.y + 6., TEXT_XS, pal.faint, TextAlign::Left);
        });
        div().flex_none().h(px(96.)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(lane_plot)
    };

    // ── The chunk ──
    let viewer = kit::plot(ws, dev, kit::PlotCfg { slot: 0, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        // Labels get their own strip so they never sit on the waveform.
        let w = Rect { y: r.y + VIEW_LABELS, h: r.h - VIEW_LABELS, ..r };
        kit::bar(window, w.x, w.y, w.w, w.h, 0., with_alpha(colour, 0.06));
        kit::hline(window, w, w.py(0.5), pal.grid_bar);
        kit::paint_wave(window, w, &peaks, with_alpha(colour, 0.65));
        // The loop: its ends and the span between them.
        for fx in [0., 1.] {
            kit::vline(window, w, w.px(fx) + if fx == 0. { 1. } else { -1. }, with_alpha(colour, 0.9));
        }
        if state.round() == 1. {
            kit::vline(window, w, w.px(read.clamp(0., 1.)), pal.text);
        }
        if trace.points.len() > 1 {
            let curve: Vec<gpui::Point<Pixels>> = trace.points.iter().map(|(pr, sp)| w.pt(*pr, speed_frac(*sp))).collect();
            kit::line(window, &curve, 1.2, with_alpha(pal.accent2, 0.9));
        }
        for (ramp, from, to) in &ramps {
            let colour = match ramp {
                Ramp::Pitch => series(2),
                Ramp::Filter => series(3),
                Ramp::Level => series(4),
            };
            kit::line(window, &[w.pt(0., ramp_frac(*ramp, *from)), w.pt(1., ramp_frac(*ramp, *to))], 1.5, with_alpha(colour, 0.95));
            kit::dot(window, w.pt(1., ramp_frac(*ramp, *to)), 3., colour, None);
        }
        let left = if live { format!("↻ loop · {}", state_name(state)) } else { "↻ loop of the chunk".to_owned() };
        kit::txt(window, cx, &left, r.x + 6., r.y + 2., TEXT_XS, pal.faint, TextAlign::Left);
        if !ramps.is_empty() {
            let names: Vec<String> = ramps
                .iter()
                .map(|(ramp, from, to)| match ramp {
                    Ramp::Pitch => format!("pitch {from:+.0} → {to:+.0} st"),
                    Ramp::Filter => format!("filter {to:+.1} oct"),
                    Ramp::Level => format!("level {from:+.0} → {to:+.0} dB"),
                })
                .collect();
            kit::txt(window, cx, &names.join("  ·  "), r.px(0.5), r.y + 2., TEXT_XS, pal.faint, TextAlign::Center);
        }
        if live {
            kit::txt(window, cx, &format!("speed {speed:.2}×"), r.right() - 6., r.y + 2., TEXT_XS, pal.accent2, TextAlign::Right);
        }
    });

    // ── Cards ──
    let mut repeat = Vec::new();
    if grid_mode {
        for id in [p::STEP_RATE, p::LENGTH, p::SWING] {
            repeat.push(kit::ctl(ws, dev, id, cx));
        }
    } else {
        for id in [p::INTERVAL, p::OFFSET, p::CHANCE, p::GATE, p::GRID, p::TRIPLETS, p::VARIATION] {
            repeat.push(kit::ctl(ws, dev, id, cx));
        }
    }
    // Ramps read "from → change reached at the end of the event".
    let mut tone: Vec<AnyElement> = vec![ramp_pair(ws, dev, p::PITCH, p::PITCH_END, cx), ramp_pair(ws, dev, p::FILTER_FREQ, p::FILTER_END, cx), ramp_pair(ws, dev, p::LEVEL, p::LEVEL_END, cx)];
    tone.extend([p::PITCH_DECAY, p::DECAY, p::REVERSE, p::RAMP].into_iter().map(|id| kit::ctl(ws, dev, id, cx)));
    let tape = vec![kit::ctl(ws, dev, p::STOP_TIME, cx)];
    let filter = vec![kit::ctl(ws, dev, p::FILTER_WIDTH, cx)];
    let cards = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .gap(px(SP1))
        .child(kit::section(dev, "REPEAT", None, repeat, cx))
        .child(kit::section(dev, "TONE", None, tone, cx))
        .child(kit::section(dev, "TAPE-STOP", Some(p::TAPE_STOP), tape, cx))
        .child(kit::section(dev, "HALF-TIME", Some(p::HALF_TIME), Vec::new(), cx))
        .child(kit::section(dev, "FILTER", Some(p::FILTER_ON), filter, cx));

    kit::panel(if dev.big { 640. } else { 520. })
        .child(head)
        .child(
            div()
                .id(("stutter-body", dev.key(0)))
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP2))
                .overflow_y_scroll()
                .child(top)
                .when(grid_mode, |d| d.child(tools))
                .child(div().flex_none().h(px(VIEW_H)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(viewer))
                .child(cards),
        )
        .children(capture(dev.id, cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    fn pat(d: &Dev) -> Pattern {
        read_pattern(&d.set, &d.values)
    }

    #[test]
    fn the_meter_says_what_the_plugin_is_doing() {
        assert_eq!((state_name(0.), state_name(1.), state_name(2.), state_name(3.)), ("idle", "repeating", "tape-stop", "gated"));
        assert_eq!((active_step(-1.), active_step(0.), active_step(15.), active_step(16.)), (None, Some(0), Some(15), None));
        assert_eq!(METER_LEN, 134);
    }

    #[test]
    fn a_classic_event_fills_its_share_of_the_interval() {
        let (interval, gate) = classic_lengths(5, 4, 4.);
        assert_eq!(event_share(interval, gate), (0.25, false));
        let (interval, gate) = classic_lengths(5, 10, 4.);
        assert_eq!(event_share(interval, gate), (1., true));
        assert_eq!(event_share(0., 1.), (0., false));
    }

    #[test]
    fn the_pattern_is_read_from_the_real_params_and_defaults_are_the_plugins() {
        let d = dev("fx.stutter", &[(p::step(3, p::STEP_PROB), 40.), (p::step(5, p::STEP_PITCH), -7.), (p::step(9, p::STEP_REV), 1.), (p::step(2, p::STEP_DIV), 4.)]);
        let pt = pat(&d);
        assert_eq!((pt[3].prob, pt[5].pitch, pt[9].rev, pt[2].div), (40., -7, true, 4));
        for (i, s) in pat(&dev("fx.stutter", &[])).iter().enumerate() {
            assert_eq!(*s, default_step(i), "step {i}");
        }
    }

    #[test]
    fn a_diff_only_touches_what_changed_and_carries_the_extras() {
        let d = dev("fx.stutter", &[]);
        let from = pat(&d);
        assert!(diff(&d.set, &from, &from, &[]).is_empty());
        let mut to = from;
        to[4].on = true;
        to[4].pitch = 5;
        let edits = diff(&d.set, &from, &to, &[(p::SEED, 42.)]);
        let ids: Vec<u32> = edits.iter().map(|e| e.0).collect();
        assert_eq!(ids, [p::step(4, p::STEP_ON), p::step(4, p::STEP_PITCH), p::SEED]);
        assert!(edits.iter().all(|(_, v)| (0. ..=1.).contains(v)));
    }

    #[test]
    fn rotate_double_fill_and_clear() {
        let mut base = cleared();
        for i in [0, 2, 3] {
            base[i].on = true;
        }
        let r = rotated(&base, 4, 1);
        assert_eq!((r[0].on, r[1].on, r[2].on, r[3].on), (true, true, false, true), "wraps inside the length");
        assert_eq!(rotated(&r, 4, -1), base);
        assert_eq!(rotated(&base, 4, 4), base);
        let (d, l) = doubled(&base, 4);
        assert_eq!(l, 8);
        assert_eq!((d[4].on, d[6].on, d[7].on), (true, true, true));
        let (_, l) = doubled(&base, 16);
        assert_eq!(l, 16, "a full grid has its halves copied");
        let (d, l) = doubled(&base, 12);
        assert_eq!((l, d[8].on), (16, base[0].on));
        let f = filled(&base, 8, 1, 2);
        assert_eq!((0..8).map(|i| f[i].on).collect::<Vec<_>>(), [false, true, false, true, false, true, false, true]);
        assert!(cleared().iter().all(|s| !s.on && s.prob == 100.));
    }

    #[test]
    fn the_dice_are_repeatable_in_range_and_mutate_changes_a_few_steps() {
        assert_eq!(randomized(7), randomized(7));
        assert_ne!(randomized(7), randomized(8));
        let r = randomized(123);
        assert!(r[0].on);
        assert!(r.iter().all(|s| (0. ..=100.).contains(&s.prob) && s.div < DIV_COUNT && s.pitch.abs() <= 12));
        for seed in 0..40 {
            let m = mutated(&r, 8, seed);
            let changed = (0..STEPS).filter(|i| m[*i] != r[*i]).count();
            assert!((1..=3).contains(&changed), "seed {seed}: {changed}");
            assert!((8..STEPS).all(|i| m[i] == r[i]), "outside the length stays");
        }
        assert!(next_seed(5) < SEEDS);
        assert_eq!(next_seed(5), next_seed(5));
    }

    #[test]
    fn pointers_map_to_columns_pitches_and_lengths() {
        let b = Bounds { origin: gpui::point(px(100.), px(10.)), size: gpui::size(px(460.), px(36.)) };
        // 16 columns with a 4 px gap: a pitch of 29 px.
        assert_eq!((step_at(b, 90.), step_at(b, 101.), step_at(b, 130.), step_at(b, 1000.)), (0, 0, 1, 15));
        assert_eq!((pitch_at(b, 0.), pitch_at(b, 28.), pitch_at(b, 100.)), (12, 0, -12));
        assert_eq!((length_at(b, 100.), length_at(b, 100. + 29. * 8. + 3.), length_at(b, 900.)), (1, 8, 16));
    }

    #[test]
    fn the_new_step_fields_ride_the_pattern_and_the_ties_have_their_own_ids() {
        let d = dev("fx.stutter", &[(p::step(2, p::STEP_LEVEL), -9.), (p::step(2, p::STEP_STOP), 1.), (p::step(5, p::STEP_FILTER), 1.), (p::tie(4), 1.)]);
        let pt = pat(&d);
        assert_eq!((pt[2].level, pt[2].stop, pt[5].filter, pt[4].tie, pt[3].tie), (-9., true, true, true, false));
        assert_eq!(pat(&dev("fx.stutter", &[]))[0].level, 0., "0 dB is the default");
        // A change of the tie costs one edit, on its own id.
        let mut to = pt;
        to[7].tie = true;
        let edits = diff(&d.set, &pt, &to, &[]);
        assert_eq!(edits.iter().map(|e| e.0).collect::<Vec<_>>(), [p::tie(7)]);
        // Rotating carries every field with its step.
        let r = rotated(&pt, 16, 1);
        assert_eq!((r[3].level, r[3].stop, r[5].tie), (-9., true, true));
        assert!(cleared().iter().all(|s| s.level == 0. && !s.stop && !s.filter && !s.tie));
        for i in [0, 15] {
            for sw in [Sw::Tie, Sw::Rev, Sw::Stop, Sw::Filter] {
                assert!(d.info(sw.param(i)).is_some());
            }
        }
        // Drawn levels run from 0 dB at the top to the cut at the bottom.
        let b = Bounds { origin: gpui::point(px(0.), px(10.)), size: gpui::size(px(100.), px(30.)) };
        assert_eq!((level_at(b, 0.), level_at(b, 25.), level_at(b, 500.)), (0., -30., -60.));
    }

    #[test]
    fn ramps_are_the_change_reached_at_the_end_and_zero_is_none() {
        assert!(active_ramps(0., 0., 0., 0., 0.).is_empty());
        let r = active_ramps(3., 7., 0., -6., -12.);
        assert_eq!(r, vec![(Ramp::Pitch, 3., 10.), (Ramp::Level, -6., -18.)]);
        let filter = active_ramps(0., 0., -2., 0., 0.);
        assert_eq!(filter, vec![(Ramp::Filter, 0., -2.)]);
        assert_eq!(ramp_frac(Ramp::Pitch, 0.), 0.5);
        assert!(ramp_frac(Ramp::Pitch, 12.) < 0.5 && ramp_frac(Ramp::Level, 0.) < ramp_frac(Ramp::Level, -30.));
        assert!((0.02..=0.98).contains(&ramp_frac(Ramp::Filter, 100.)));
    }

    #[test]
    fn the_speed_trace_follows_one_event_and_starts_over() {
        let mut t = SpeedTrace::default();
        t.push(0., 0., 1.);
        assert!(t.points.is_empty(), "idle draws nothing");
        for i in 0..10 {
            t.push(2., i as f32 / 10., 1. - i as f32 / 12.);
        }
        assert_eq!(t.points.len(), 10);
        t.push(1., 0.01, 1.);
        assert_eq!(t.points.len(), 1, "a new event starts a new curve");
        assert!(speed_frac(1.) == 0.5 && speed_frac(2.) < 0.5 && speed_frac(0.5) > 0.5);
    }
}
