//! The Bloom stage: the scene as one liquid mass drawn on the GPU
//! (`matter/`), with the sixteen sounds as swatches under it.
//!
//! The UI thread only steps the motion and posts the frame's uniforms to the
//! render thread (`matter::service`), then paints the newest finished frame:
//! an NV12 IOSurface on macOS (zero-copy), else an uploaded image. While the
//! GPU starts the panel is plain; without a GPU the CPU field (`visual.rs`)
//! is drawn instead.
//!
//! Gestures: click the mass to hear the selected sound, drag on it to regrow
//! that sound (right or up: further from its core; one undo step on
//! release), Alt-click locks it; the pointer is a magnet the liquid leans
//! towards and parts under. Click a swatch to select and hear its sound,
//! Alt-click to lock it. Drop an audio file (or a library sound) to match
//! it into the selected sound.

use super::matter::frame::{Live, Panel};
use super::matter::mapping::{Controls, SoundLook, blend, sound_look, with_controls};
use super::matter::service::{self, Image, Status};
use super::matter::uniforms::Uniforms;
use super::view::{self, CellDrag};
use super::visual::audition;
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::widgets::mono;
use crate::workspace::{DraggedSample, Workspace};
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene};
use aitrack_stock::instrument::bloom::p;
use aitrack_stock::instrument::bloom::timbre::Timbre;
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, ExternalPaths, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    StatefulInteractiveElement, Styled, canvas, div, px,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// Height of the swatch row at the bottom of the stage.
pub const SWATCHES_H: f32 = 56.0;

/// How a stage is laid out.
#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// The sixteen swatches under the mass (the window; the card has none).
    pub swatches: bool,
    /// The colour around the panel's corners (what it sits on).
    pub outer: gpui::Hsla,
    /// Which stage of the device this is (several can show in one window).
    pub slot: u8,
    /// How much bigger the mass is than the window's fit (small stages fill up).
    pub zoom: f32,
}

/// The newest frame a window shows.
enum Shown {
    Image(Arc<gpui::RenderImage>),
    #[cfg(target_os = "macos")]
    Surface(super::matter::surface::Frame),
}

/// One stage in one window: what was sent and what is shown.
#[derive(Default)]
struct Slot {
    sent: u64,
    seen: u64,
    last: Option<Uniforms>,
    shown: Option<Shown>,
}

/// Every stage's frames, by panel key (device, window, slot).
#[derive(Default)]
pub struct Frames {
    slots: HashMap<u64, Slot>,
}

/// What the stage keeps per device between frames.
#[derive(Default)]
pub struct StageState {
    pub anim: super::matter::Animator,
    last: Option<Instant>,
    /// The pointer over the mass (units) and when it moved.
    pub cursor: Option<(f32, f32)>,
    /// Looks by timbre (and theme), so mapping runs once per sound.
    looks: HashMap<u64, SoundLook>,
    frames: Rc<RefCell<Frames>>,
    /// When a frame was last painted (the idle refresh runs while recent).
    pub painted: Rc<Cell<Option<Instant>>>,
    pub reduced: bool,
    pub moving: bool,
    /// The material menu is open.
    pub menu: bool,
    /// The species menu is open.
    pub species_menu: bool,
    /// Gene groups folded in the gene editor (bit per group).
    pub folded: u16,
    /// The tab under the stage (DNA, Morph, Play, Tools).
    pub tab: usize,
    /// UI-side cost of the last frame, ms (the stage's own work only).
    pub cost: Rc<Cell<f32>>,
    /// What building the window's editor cost the UI thread last frame, ms.
    pub build_ms: f32,
    /// How often the stage draws now (see [`pace`]).
    pub pace: Pace,
    /// When the device last sounded (a hit, an envelope, output).
    sounded: Option<Instant>,
    seen_hits: Option<[f32; SOUNDS]>,
    /// Device pixels a frame may have while it moves (adapts to the GPU's cost).
    pub budget: Rc<Cell<f32>>,
}

impl StageState {
    /// The panel keys this device holds on the render thread.
    pub fn keys(&self) -> Vec<u64> {
        self.frames.borrow().slots.keys().copied().collect()
    }

    /// Whether the idle breathing wants a frame now (about 12 a second while
    /// the stage was painted, on screen, in the last two seconds).
    pub fn wants_tick(&self) -> bool {
        self.pace == Pace::Breathe && self.painted.get().is_some_and(|t| (0.08..2.0).contains(&t.elapsed().as_secs_f32()))
    }
}

/// How often a stage draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pace {
    /// Every display frame: the device sounds, or the matter still moves.
    Full,
    /// About 12 frames a second (the idle breathing; low power while it sounds).
    #[default]
    Breathe,
    /// No frames (reduced motion, or the stage is not on screen).
    Still,
}

/// What a device's sound is doing this frame (from its meter and the transport).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Activity {
    /// The loudest sound's envelope, 0..1.
    pub loudest: f32,
    /// Output RMS, linear.
    pub level: f32,
    /// A hit counter moved since the last frame.
    pub hit: bool,
    pub playing: bool,
}

/// An envelope or output level above this is sound.
pub const SOUNDING: f32 = 0.01;
/// Seconds the full rate holds after the sound stops (the matter settles).
pub const SETTLE_S: f32 = 1.5;
/// While the transport plays, the full rate holds this long between hits.
pub const BETWEEN_HITS_S: f32 = 4.0;

impl Activity {
    pub fn sounding(&self) -> bool {
        self.hit || self.loudest > SOUNDING || self.level > SOUNDING
    }
}

/// How often a stage draws: every frame while its device sounds (and while
/// the transport plays with hits a few seconds apart, and for a moment after,
/// while the matter settles), the idle breathing otherwise. A stage that is
/// not on screen draws nothing; reduced motion is one still frame; low power
/// draws at the breathing rate at most and not at all when silent.
/// `quiet_for`: seconds since the device last sounded.
pub fn pace(a: Activity, quiet_for: f32, moving: bool, shown: bool, reduced: bool, low_power: bool) -> Pace {
    if !shown || reduced {
        return Pace::Still;
    }
    let live = a.sounding() || moving || quiet_for < SETTLE_S || (a.playing && quiet_for < BETWEEN_HITS_S);
    match (live, low_power) {
        (true, false) => Pace::Full,
        (true, true) | (false, false) => Pace::Breathe,
        (false, true) => Pace::Still,
    }
}

/// The scale (device pixels per logical pixel) of a frame for a stage of
/// `w` × `h` logical px on a display of `native` device px per px, with at
/// most `budget` device pixels: native when it fits, else as much as fits,
/// never under a quarter.
pub fn frame_scale(w: f32, h: f32, native: f32, budget: f32) -> f32 {
    let area = (w * h).max(1.0);
    native.min((budget / area).sqrt()).max(0.25)
}

/// The next pixel budget from the render thread's cost of the last frame:
/// shrinks while frames cost more than `target` ms, grows back while they
/// cost less (bounded, and gently, so it never oscillates frame to frame).
pub fn adapt_budget(budget: f32, cost_ms: f32, target_ms: f32) -> f32 {
    if cost_ms <= 0.0 {
        return budget;
    }
    let k = (target_ms / cost_ms).clamp(0.8, 1.06);
    (budget * k).clamp(MIN_BUDGET, MAX_BUDGET)
}

/// Device pixels a moving frame starts with and its bounds.
pub const START_BUDGET: f32 = 900_000.0;
pub const MIN_BUDGET: f32 = 300_000.0;
pub const MAX_BUDGET: f32 = 4_000_000.0;
/// What a frame may cost the render thread, ms (the raymarch and the NV12
/// conversion; a quarter of a 60 Hz frame).
pub const TARGET_MS: f32 = 4.0;

fn timbre_key(t: &Timbre, light: bool) -> u64 {
    let mut h: u64 = if light { 0x9E37_79B9 } else { 0x85EB_CA6B };
    for v in [t.centroid, t.noisiness, t.energy, t.attack_ms, t.decay_ms, t.inharmonic, t.wobble] {
        h = (h ^ u64::from(v.to_bits())).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The device's knobs as the matter feels them.
pub fn controls(dev: &Dev) -> Controls {
    let pc = |id: u32| dev.plain(id) / 100.0;
    Controls {
        decay: pc(p::DECAY),
        body: pc(p::BODY),
        tone: pc(p::TONE),
        grit: pc(p::GRIT),
        space: pc(p::SPACE),
        deviance: pc(p::DEVIANCE),
        tune: dev.plain(p::TUNE),
        volume_db: dev.plain(p::VOLUME),
    }
}

/// The live meter's centroid (0..1, 50 Hz .. 10 kHz) in Hz.
fn live_centroid(x: f32) -> f32 {
    50.0 * 200f32.powf(x.clamp(0.0, 1.0))
}

fn linear(c: gpui::Hsla) -> [f32; 3] {
    let c = gpui::Rgba::from(c);
    let lin = |v: f32| if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    [lin(c.r), lin(c.g), lin(c.b)]
}

fn encoded(c: gpui::Hsla) -> [f32; 3] {
    let c = gpui::Rgba::from(c);
    [c.r, c.g, c.b]
}

/// The area of the stage the mass is fitted into (logical px, relative to its bounds).
fn area(b: Bounds<Pixels>, swatches: bool) -> (f32, f32, f32, f32) {
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let bottom = if swatches { SWATCHES_H } else { 0.0 };
    (0.0, 0.0, w, (h - bottom).max(8.0))
}

/// The panel of a stage at `k` device pixels per logical pixel.
#[allow(clippy::too_many_arguments)]
fn panel(b: Bounds<Pixels>, swatches: bool, zoom: f32, k: f32, corner: f32, bg: [f32; 3], light: bool, outer: [f32; 3]) -> Panel {
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let (x, y, aw, ah) = area(b, swatches);
    Panel::fit((w * k).round(), (h * k).round(), (x * k, y * k, aw * k, ah * k), corner * k, bg, light, outer).zoomed(zoom)
}

/// The pointer at `pos` in the mass's units, for a stage of bounds `b`.
fn units(b: Bounds<Pixels>, swatches: bool, zoom: f32, pos: Point<Pixels>) -> (f32, f32) {
    let pl = panel(b, swatches, zoom, 1.0, 0.0, [0.0; 3], false, [0.0; 3]);
    pl.units(f32::from(pos.x - b.origin.x), f32::from(pos.y - b.origin.y))
}

/// What the device is doing, and its meter as the matter reads it: the
/// envelopes, pulses and hit counters (or the `bloomplay` dev view's
/// synthetic beat, which plays its lit cells in turn).
fn live_meter(dev: &Dev, fake: Option<[f32; SOUNDS]>, time: f32) -> ([f32; SOUNDS], [f32; SOUNDS], [f32; SOUNDS], f32) {
    let mut env: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(i));
    let mut pulse: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(16 + i));
    let mut hits: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(32 + i));
    let mut level = dev.m(80);
    if let Some(fake) = fake {
        for i in 0..SOUNDS {
            let at = time * 2.0 + i as f32 * 0.37;
            let beat = (at % 2.0) / 2.0;
            env[i] = fake[i] * (-beat * 4.0).exp();
            pulse[i] = fake[i] * (-beat * 14.0).exp();
            hits[i] = if fake[i] > 0.0 { (at / 2.0).floor() } else { 0.0 };
        }
        level = (0.2 * env.iter().sum::<f32>()).min(0.9);
    }
    (env, pulse, hits, level)
}

/// Whether the GPU stage can draw (starts the render thread on first ask).
pub fn available() -> Option<bool> {
    match service::service().status() {
        Status::Ready { .. } => Some(true),
        Status::Starting => None,
        Status::Failed(_) => Some(false),
    }
}

/// The looks of every sound of `scene` on the stage: from its analysed
/// timbre, leaning towards what the meter hears while it plays.
pub fn sound_looks(ws: &mut Workspace, dev: &Dev, scene: &Scene) -> [SoundLook; SOUNDS] {
    let light = !p().dark;
    let knobs = controls(dev);
    let timbres = view::timbres(ws, scene, view::Shape::of(dev));
    let st = &mut ws.fx.bloom.view(dev.id).stage;
    if st.looks.len() > 1_024 {
        st.looks.clear();
    }
    std::array::from_fn(|i| {
        let t = timbres[i];
        let base = *st.looks.entry(timbre_key(&t, light)).or_insert_with(|| sound_look(&t, light));
        let env = dev.m(i).clamp(0.0, 1.0);
        if env > 0.02 {
            let live = Timbre { centroid: live_centroid(dev.m(48 + i)), noisiness: dev.m(64 + i).clamp(0.0, 1.0), ..t };
            with_controls(&blend(&base, &sound_look(&live, light), 0.5 * env), &knobs)
        } else {
            with_controls(&base, &knobs)
        }
    })
}

/// The stage of a Bloom.
pub fn stage(ws: &mut Workspace, dev: &Dev, scene: &Scene, opts: Opts, cx: &mut Context<Workspace>) -> AnyElement {
    let t0 = Instant::now();
    match available() {
        Some(false) => return super::visual::visual(ws, dev, scene, opts.swatches, cx),
        None => {
            // The GPU is starting (a few frames): a plain panel meanwhile.
            let pal = p();
            return div()
                .size_full()
                .rounded(px(r_md()))
                .bg(pal.well)
                .child(canvas(|_, _, _| {}, |_, _, window, _| window.request_animation_frame()).size_full())
                .into_any_element();
        }
        Some(true) => {}
    }
    let device = dev.id;
    let changed = view::poll(ws, dev, cx);
    let reduced = ws.look.app.reduced_motion;
    let low_power = ws.look.app.low_power;
    let playing = ws.b.engine.transport().playing;
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let matching = ws.fx.bloom.view(device).matching.as_ref().map(|m| (m.cell, m.fraction, m.best));
    let looks = match matching {
        Some((cell, _, Some(best))) => {
            let mut shown = view::played(ws, dev, scene);
            shown.sounds[cell] = best;
            sound_looks(ws, dev, &shown)
        }
        _ => {
            let shown = view::played(ws, dev, scene);
            sound_looks(ws, dev, &shown)
        }
    };
    let material = view::parsed(ws, device).material;
    let busy = ws.fx.bloom.busy();
    let v = ws.fx.bloom.view(device);
    let (env, pulse, hits, level) = live_meter(dev, v.fake, v.stage.anim.time);
    let now = Instant::now();
    let st = &mut v.stage;
    let dt = st.last.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32());
    st.last = Some(now);
    st.reduced = reduced;
    let hit = st.seen_hits.is_some_and(|h| h != hits);
    st.seen_hits = Some(hits);
    let activity = Activity { loudest: env.iter().copied().fold(0.0, f32::max), level, hit, playing };
    if activity.sounding() {
        st.sounded = Some(now);
    }
    let quiet_for = st.sounded.map_or(f32::INFINITY, |t| now.duration_since(t).as_secs_f32());
    let cursor = st.cursor;
    let moving = st.anim.step(dt, &Live { env: &env, pulse: &pulse, looks: &looks, selected, hits: &hits, level }, cursor, reduced);
    st.moving = moving;
    let dragging = v.drag.is_some();
    let st = &mut v.stage;
    st.pace = pace(activity, quiet_for, moving || changed || busy || dragging, true, reduced, low_power);
    let rate = st.pace;
    let anim = st.anim;
    let frames = st.frames.clone();
    let painted = st.painted.clone();
    let cost = st.cost.clone();
    let budget = st.budget.clone();
    if budget.get() <= 0.0 {
        budget.set(START_BUDGET);
    }
    let pal = p();
    let light = !pal.dark;
    let bg = linear(pal.well);
    let outer = encoded(opts.outer);
    let seed = scene.seed as f32 % 997.0 + selected as f32 * 0.37;
    let corner = r_md();
    let (swatches, zoom) = (opts.swatches, opts.zoom);
    let key_base = device.0 << 24 | u64::from(opts.slot) << 16;
    let region = ws.fx.region(device, 9 + opts.slot);
    let store = region.clone();
    let well = pal.well;
    let ui_ms = t0.elapsed().as_secs_f32() * 1000.0;
    let surface = canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, _| {
            let p0 = Instant::now();
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            // Clipped away or scrolled off: no work, no frames (an occluded
            // window gets no frames from GPUI in the first place).
            if w < 8.0 || h < 8.0 || !window.content_mask().bounds.intersects(&b) {
                return;
            }
            let svc = service::service();
            // Moving frames get as many pixels as the GPU affords (native when
            // it can); the slow breathing and a still frame are native.
            let native = window.scale_factor();
            let k = match rate {
                Pace::Full => {
                    budget.set(adapt_budget(budget.get(), svc.cost_ms(), TARGET_MS));
                    frame_scale(w, h, native, budget.get())
                }
                Pace::Breathe | Pace::Still => native,
            };
            // In steps of 1/8, so the render thread's buffers rarely change size.
            let k = ((k * 8.0).floor() / 8.0).max(0.25);
            let pl = panel(b, swatches, zoom, k, corner, bg, light, outer);
            let u = anim.uniforms(material, &pl, seed);
            let wid = window.window_handle().window_id().as_u64();
            let key = key_base ^ wid.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut frames = frames.borrow_mut();
            let slot = frames.slots.entry(key).or_default();
            if slot.last != Some(u) {
                slot.sent += 1;
                svc.submit(key, slot.sent, pl.width as u32, pl.height as u32, u);
                slot.last = Some(u);
            }
            if let Some(f) = svc.take(key, slot.seen) {
                slot.seen = f.seq;
                match f.image {
                    Image::Bgra(px) => {
                        let img = image::RgbaImage::from_raw(f.width, f.height, px).map(|buf| Arc::new(gpui::RenderImage::new(vec![image::Frame::new(buf)])));
                        if let Some(img) = img
                            && let Some(Shown::Image(old)) = slot.shown.replace(Shown::Image(img))
                        {
                            let _ = window.drop_image(old);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    Image::Surface(s) => {
                        if let Some(Shown::Image(old)) = slot.shown.replace(Shown::Surface(s)) {
                            let _ = window.drop_image(old);
                        }
                    }
                }
            }
            match &slot.shown {
                Some(Shown::Image(img)) => {
                    let _ = window.paint_image(b, b, gpui::Corners::all(px(corner)), img.clone(), 0, false);
                }
                #[cfg(target_os = "macos")]
                Some(Shown::Surface(s)) => window.paint_surface(b, s.0.clone()),
                None => window.paint_quad(gpui::fill(b, well).corner_radii(px(corner))),
            }
            if rate == Pace::Full || slot.seen < slot.sent {
                window.request_animation_frame();
            }
            painted.set(Some(Instant::now()));
            let ms = ui_ms + p0.elapsed().as_secs_f32() * 1000.0;
            cost.set(if cost.get() == 0.0 { ms } else { cost.get() * 0.9 + ms * 0.1 });
        },
    )
    .size_full();

    // ── Gestures ──
    let d = dev.clone();
    let down = cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        cx.stop_propagation();
        let cell = d.choice(p::SELECTED).min(SOUNDS - 1);
        if e.modifiers.alt {
            view::toggle_lock(this, &d, cell);
            cx.notify();
            return;
        }
        let dist = view::scene(this, &d).distances[cell];
        let v = this.fx.bloom.view(d.id);
        v.drag = Some(CellDrag { cell, from: (f32::from(e.position.x), f32::from(e.position.y)), moved: false, distance: dist, start: dist });
        v.dna_cell = Some(cell);
        audition(this, &d, cell, cx);
    });
    let (d, r) = (dev.clone(), region.clone());
    let hover = cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
        let at = units(r.get(), swatches, zoom, e.position);
        this.fx.bloom.view(d.id).stage.cursor = Some(at);
        cx.notify();
    });
    let d = dev.clone();
    let leave = cx.listener(move |this, hovered: &bool, _, cx| {
        if !*hovered {
            this.fx.bloom.view(d.id).stage.cursor = None;
            cx.notify();
        }
    });
    let d = dev.clone();
    let drop_file = cx.listener(move |this, paths: &ExternalPaths, _, cx| {
        if let Some(path) = paths.paths().first() {
            let cell = d.choice(p::SELECTED).min(SOUNDS - 1);
            view::start_match(this, &d, cell, path.clone());
            cx.notify();
        }
    });
    let d = dev.clone();
    let drop_sample = cx.listener(move |this, s: &DraggedSample, _, cx| {
        let cell = d.choice(p::SELECTED).min(SOUNDS - 1);
        view::start_match(this, &d, cell, s.0.path.clone());
        cx.notify();
    });

    let mut root = div()
        .id(("bloom-stage", dev.key(0xB20 | opts.slot as usize)))
        .relative()
        .size_full()
        .cursor_pointer()
        .child(surface)
        .on_mouse_down(MouseButton::Left, down)
        .on_mouse_move(hover)
        .on_hover(leave)
        .drag_over::<ExternalPaths>(|s, _, _, _| s.opacity(0.85))
        .drag_over::<DraggedSample>(|s, _, _, _| s.opacity(0.85))
        .on_drop(drop_file)
        .on_drop(drop_sample);
    if let Some((cell, f, _)) = matching {
        let name = ws.fx.bloom.view(device).matching.as_ref().map(|m| m.name.clone()).unwrap_or_default();
        root = root.child(
            div()
                .absolute()
                .top(px(SP2))
                .left(px(SP2))
                .px(px(SP2))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .bg(with_alpha(pal.surface, 0.8))
                .child(mono(format!("Matching {name} into {:02} · {:.0}%", cell + 1, f * 100.), TEXT_XS, pal.accent2)),
        );
    }
    if ws.fx.bloom.view(device).drag.is_some() {
        root = root.child(capture(dev, cx));
    }
    root.into_any_element()
}

/// A sound's colour as GPUI paints it.
pub fn colour(look: &super::look::Look) -> gpui::Hsla {
    let e = super::look::encode;
    gpui::Rgba { r: e(look.rgb[0]), g: e(look.rgb[1]), b: e(look.rgb[2]), a: 1. }.into()
}

/// Window-wide listeners while the mass is dragged (regrowing the selected sound).
fn capture(dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let weak = cx.entity().downgrade();
    let d = dev.clone();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let (w, dd) = (weak.clone(), d.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = w.update(cx, |this, cx| {
                    let scene = view::scene(this, &dd);
                    let v = this.fx.bloom.view(dd.id);
                    let Some(mut drag) = v.drag else { return };
                    let (fx, fy) = drag.from;
                    let (dx, dy) = (f32::from(e.position.x) - fx, f32::from(e.position.y) - fy);
                    if dx.hypot(dy) > 4. {
                        drag.moved = true;
                    }
                    if drag.moved {
                        // Right or up grows the sound further from its core; steps
                        // of 0.02, so a drag visits a bounded set of sounds.
                        drag.distance = ((drag.start + (dx - dy) / 260.).clamp(0.0, 1.5) * 50.).round() / 50.;
                        v.drag = Some(drag);
                        v.preview = Some(aitrack_stock::instrument::bloom::grow::regrow(&scene, drag.cell, drag.distance));
                    }
                    cx.notify();
                });
            });
            let (w, dd) = (weak.clone(), d.clone());
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = w.update(cx, |this, cx| {
                    this.release_audition(cx);
                    let v = this.fx.bloom.view(dd.id);
                    let Some(drag) = v.drag.take() else { return };
                    let preview = v.preview.take();
                    if drag.moved
                        && let Some(scene) = preview
                    {
                        view::commit(this, &dd, &scene, None, "Regrow sound");
                    }
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    #[test]
    fn the_pointer_maps_into_the_mass_units() {
        let b = Bounds { origin: point(px(100.), px(50.)), size: gpui::size(px(400.), px(300. + SWATCHES_H)) };
        let pl = panel(b, true, 1.0, 1.0, 0.0, [0.0; 3], false, [0.0; 3]);
        assert_eq!(pl.scale, 300.0 * 0.54, "fitted into the area above the swatches");
        let (x, y) = units(b, true, 1.0, point(px(100. + pl.cx + pl.scale * 0.5), px(50. + pl.cy)));
        assert!((x - 0.5).abs() < 1e-5 && y.abs() < 1e-5);
        // Two device pixels per point double every length.
        let hi = panel(b, true, 1.0, 2.0, 6.0, [0.0; 3], false, [0.0; 3]);
        assert_eq!((hi.width, hi.scale, hi.corner), (800.0, 2.0 * pl.scale, 12.0));
        // A zoomed stage maps the pointer onto the bigger mass.
        let z = panel(b, true, 1.25, 1.0, 0.0, [0.0; 3], false, [0.0; 3]);
        let (x, _) = units(b, true, 1.25, point(px(100. + z.cx + z.scale * 0.5), px(50. + z.cy)));
        assert!((x - 0.5).abs() < 1e-5 && z.scale == pl.scale * 1.25);
    }

    #[test]
    fn a_sounding_device_animates_at_full_rate_then_settles() {
        let quiet = Activity::default();
        let hit = Activity { hit: true, ..quiet };
        let ringing = Activity { loudest: 0.3, ..quiet };
        let output = Activity { level: 0.05, ..quiet };
        let playing = Activity { playing: true, ..quiet };
        // Playing sound animates everywhere it is shown, hovered or not.
        for a in [hit, ringing, output] {
            assert_eq!(pace(a, 0.0, false, true, false, false), Pace::Full, "{a:?}");
        }
        // It settles for a moment after the sound stops, then breathes.
        assert_eq!(pace(quiet, 1.0, false, true, false, false), Pace::Full);
        assert_eq!(pace(quiet, SETTLE_S + 0.1, false, true, false, false), Pace::Breathe);
        // The transport rolling between sparse hits keeps the full rate a while.
        assert_eq!(pace(playing, 3.0, false, true, false, false), Pace::Full);
        assert_eq!(pace(playing, BETWEEN_HITS_S + 1.0, false, true, false, false), Pace::Breathe);
        assert_eq!(pace(playing, f32::INFINITY, false, true, false, false), Pace::Breathe, "a silent track does not burn frames");
        // Motion of its own (a hover, a drag) is full rate too.
        assert_eq!(pace(quiet, f32::INFINITY, true, true, false, false), Pace::Full);
        // Hidden or scrolled away: nothing.
        assert_eq!(pace(hit, 0.0, true, false, false, false), Pace::Still);
        // Reduced motion: one still frame. Low power: the breathing rate while it sounds, nothing when silent.
        assert_eq!(pace(hit, 0.0, true, true, true, false), Pace::Still);
        assert_eq!(pace(hit, 0.0, false, true, false, true), Pace::Breathe);
        assert_eq!(pace(quiet, f32::INFINITY, false, true, false, true), Pace::Still);
        // A quiet meter is not sound.
        assert!(!Activity { loudest: SOUNDING * 0.5, level: SOUNDING * 0.5, ..quiet }.sounding());
    }

    #[test]
    fn a_stage_frame_costs_the_ui_thread_well_under_a_millisecond() {
        // What `stage` does each frame besides the cached lookups: every
        // sound's live look under the knobs, the motion, the uniforms.
        use super::super::matter::Material;
        let base: [SoundLook; SOUNDS] = std::array::from_fn(|i| sound_look(&Timbre { centroid: 100.0 + 400.0 * i as f32, noisiness: 0.3, ..Default::default() }, false));
        let knobs = Controls { decay: 0.3, grit: -0.2, ..Controls::default() };
        let mut anim = super::super::matter::Animator::default();
        let pl = Panel::fit(1800.0, 920.0, (0.0, 0.0, 1800.0, 920.0), 8.0, [0.2; 3], false, [0.0; 3]);
        let n = 2_000;
        let t0 = Instant::now();
        let mut sink = 0.0;
        for k in 0..n {
            let env: [f32; SOUNDS] = std::array::from_fn(|i| ((k + i) % 7) as f32 / 7.0);
            let hits: [f32; SOUNDS] = std::array::from_fn(|i| ((k + i) / 9) as f32);
            let looks: [SoundLook; SOUNDS] = std::array::from_fn(|i| {
                let live = Timbre { centroid: live_centroid(env[i]), noisiness: 0.4, ..Default::default() };
                with_controls(&blend(&base[i], &sound_look(&live, false), 0.5 * env[i]), &knobs)
            });
            anim.step(1.0 / 60.0, &Live { env: &env, pulse: &env, looks: &looks, selected: k % SOUNDS, hits: &hits, level: 0.3 }, None, false);
            let a = Activity { loudest: env[0], level: 0.3, hit: k % 9 == 0, playing: true };
            sink += anim.uniforms(Material::Obsidian, &pl, 0.0).floats()[20] + pace(a, 0.1, false, true, false, false) as u8 as f32;
        }
        let per = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        assert!(sink.is_finite());
        assert!(per < 0.25, "{per} ms per frame");
    }

    #[test]
    fn frames_are_native_when_they_fit_and_adapt_to_the_gpu() {
        // A 900 × 460 stage on a Retina display: 1.66 M device pixels.
        assert_eq!(frame_scale(900., 460., 2.0, 4_000_000.), 2.0, "native when it fits");
        let k = frame_scale(900., 460., 2.0, 700_000.);
        assert!(k < 2.0 && (900. * k * 460. * k - 700_000.).abs() < 1_000.);
        // Even the least budget has twice the pixels the old 512 px cap gave.
        const { assert!(MIN_BUDGET >= 2.0 * 512. * 262.) };
        assert_eq!(frame_scale(10_000., 10_000., 2.0, 1_000.), 0.25, "never under a quarter");
        // Too slow: fewer pixels; fast: more, gently, within bounds.
        assert!(adapt_budget(700_000., 5.0, TARGET_MS) < 700_000.);
        assert!(adapt_budget(700_000., 1.0, TARGET_MS) > 700_000.);
        assert!(adapt_budget(700_000., 1.0, TARGET_MS) <= 700_000. * 1.06 + 1.);
        assert_eq!(adapt_budget(MIN_BUDGET, 50.0, TARGET_MS), MIN_BUDGET);
        assert_eq!(adapt_budget(MAX_BUDGET, 0.1, TARGET_MS), MAX_BUDGET);
        assert_eq!(adapt_budget(700_000., 0.0, TARGET_MS), 700_000., "no measure yet");
    }

    #[test]
    fn live_centroids_cover_the_meter_range() {
        assert!((live_centroid(0.0) - 50.0).abs() < 1e-3);
        assert!((live_centroid(1.0) - 10_000.0).abs() < 1.0);
        assert!(live_centroid(0.5) > live_centroid(0.4));
    }

    #[test]
    fn timbres_key_their_looks_by_value_and_theme() {
        let t = Timbre { centroid: 500.0, ..Default::default() };
        assert_eq!(timbre_key(&t, true), timbre_key(&t, true));
        assert_ne!(timbre_key(&t, true), timbre_key(&t, false));
        assert_ne!(timbre_key(&t, true), timbre_key(&Timbre { centroid: 501.0, ..t }, true));
    }
}
