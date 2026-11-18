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
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::widgets::{self, label, mono};
use crate::workspace::{DraggedSample, Workspace};
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene};
use aitrack_stock::instrument::bloom::p;
use aitrack_stock::instrument::bloom::timbre::Timbre;
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, ExternalPaths, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    StatefulInteractiveElement, Styled, canvas, div, point, px,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// The longest side of a frame, device pixels (frames are scaled up by the GPU).
pub const MAX_SIDE: f32 = 512.0;
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
    /// The tab under the stage (DNA, Morph, Play, Tools).
    pub tab: usize,
    /// UI-side cost of the last frame, ms.
    pub cost: Rc<Cell<f32>>,
}

impl StageState {
    /// The panel keys this device holds on the render thread.
    pub fn keys(&self) -> Vec<u64> {
        self.frames.borrow().slots.keys().copied().collect()
    }

    /// Whether the idle breathing wants a frame now (about 12 a second while
    /// the stage was painted in the last two seconds).
    pub fn wants_tick(&self) -> bool {
        !self.reduced && self.painted.get().is_some_and(|t| (0.08..2.0).contains(&t.elapsed().as_secs_f32()))
    }
}

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
fn panel(b: Bounds<Pixels>, swatches: bool, k: f32, corner: f32, bg: [f32; 3], light: bool, outer: [f32; 3]) -> Panel {
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let (x, y, aw, ah) = area(b, swatches);
    Panel::fit((w * k).round(), (h * k).round(), (x * k, y * k, aw * k, ah * k), corner * k, bg, light, outer)
}

/// The pointer at `pos` in the mass's units, for a stage of bounds `b`.
fn units(b: Bounds<Pixels>, swatches: bool, pos: Point<Pixels>) -> (f32, f32) {
    let pl = panel(b, swatches, 1.0, 0.0, [0.0; 3], false, [0.0; 3]);
    pl.units(f32::from(pos.x - b.origin.x), f32::from(pos.y - b.origin.y))
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
    let reduced = ws.look.app.reduced_motion || ws.look.app.low_power;
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let mut shown = view::played(ws, dev, scene);
    let matching = ws.fx.bloom.view(device).matching.as_ref().map(|m| (m.cell, m.fraction, m.best));
    if let Some((cell, _, Some(best))) = matching {
        shown.sounds[cell] = best;
    }
    let looks = sound_looks(ws, dev, &shown);
    let colours = view::looks(ws, &shown, view::Shape::of(dev));
    let material = view::parsed(ws, device).material;
    let busy = ws.fx.bloom.busy();
    let v = ws.fx.bloom.view(device);
    // The meter, or the synthetic activity of the `bloomplay` dev view.
    let mut env: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(i));
    let mut pulse: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(16 + i));
    if let Some(fake) = v.fake {
        let phase = v.stage.anim.time;
        for i in 0..SOUNDS {
            let beat = ((phase * 2.0 + i as f32 * 0.37) % 2.0) / 2.0;
            env[i] = fake[i] * (-beat * 4.0).exp();
            pulse[i] = fake[i] * (-beat * 14.0).exp();
        }
    }
    let now = Instant::now();
    let st = &mut v.stage;
    let dt = st.last.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32());
    st.last = Some(now);
    st.reduced = reduced;
    let cursor = st.cursor;
    let moving = st.anim.step(dt, &Live { env: &env, pulse: &pulse, looks: &looks, selected }, cursor, reduced);
    st.moving = moving;
    let anim = st.anim;
    let frames = st.frames.clone();
    let painted = st.painted.clone();
    let cost = st.cost.clone();
    let dragging = v.drag.is_some();
    let animate = !reduced && (moving || changed || busy || dragging);
    let pal = p();
    let light = !pal.dark;
    let bg = linear(pal.well);
    let outer = encoded(opts.outer);
    let seed = scene.seed as f32 % 997.0 + selected as f32 * 0.37;
    let corner = r_md();
    let swatches = opts.swatches;
    let key_base = device.0 << 24 | u64::from(opts.slot) << 16;
    let region = ws.fx.region(device, 9 + opts.slot);
    let store = region.clone();
    let well = pal.well;
    let surface = canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, _| {
            let p0 = Instant::now();
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            if w < 8.0 || h < 8.0 {
                return;
            }
            let k = window.scale_factor().min(MAX_SIDE / w.max(h)).max(0.25);
            let pl = panel(b, swatches, k, corner, bg, light, outer);
            let u = anim.uniforms(material, &pl, seed);
            let wid = window.window_handle().window_id().as_u64();
            let key = key_base ^ wid.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let svc = service::service();
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
            if animate || slot.seen < slot.sent {
                window.request_animation_frame();
            }
            painted.set(Some(Instant::now()));
            let ms = (p0.elapsed() + t0.elapsed()).as_secs_f32() * 1000.0;
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
        let at = units(r.get(), swatches, e.position);
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
    if swatches {
        root = root.child(swatch_row(ws, dev, scene, &colours, &env, cx));
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

/// The sixteen sounds as glossy swatches in their own colours: click to
/// select and hear one, Alt-click to lock it.
fn swatch_row(ws: &mut Workspace, dev: &Dev, scene: &Scene, colours: &[super::look::Look; SOUNDS], env: &[f32; SOUNDS], cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let matching = ws.fx.bloom.view(dev.id).matching.as_ref().map(|m| (m.cell, m.fraction));
    let mut row = div().flex().items_start().justify_center().gap(px(SP1));
    for i in 0..SOUNDS {
        let c = colour(&colours[i]);
        let on = i == selected;
        let lit = env[i].clamp(0.0, 1.0);
        let locked = scene.locked(i);
        let deep = gpui::Hsla { l: (c.l * 0.45).max(0.08), ..c };
        let shine = gpui::Hsla { l: (c.l + 0.3).min(0.95), s: c.s * 0.6, ..c };
        let d = dev.clone();
        let mut pebble = div()
            .relative()
            .size(px(20.))
            .rounded_full()
            .bg(gpui::linear_gradient(150., gpui::linear_color_stop(c, 0.), gpui::linear_color_stop(deep, 1.)))
            .border_1()
            .border_color(if on { pal.text } else { with_alpha(c, 0.4) })
            .child(div().absolute().top(px(3.)).left(px(4.)).w(px(6.)).h(px(4.)).rounded_full().bg(with_alpha(shine, 0.85)));
        if lit > 0.02 {
            pebble = pebble.shadow(vec![gpui::BoxShadow { color: with_alpha(c, 0.7 * lit), offset: point(px(0.), px(0.)), blur_radius: px(4. + 8. * lit), spread_radius: px(1.), inset: false }]);
        }
        if locked {
            pebble = pebble.child(div().absolute().top(px(-4.)).right(px(-4.)).child(icon(Icon::Lock, 8., pal.accent)));
        }
        let caption = match matching {
            Some((cell, f)) if cell == i => mono(format!("{:.0}%", f * 100.), TEXT_XS, pal.accent2),
            _ => label(scene.sounds[i].role.short(), TEXT_XS, if on { pal.text } else { pal.faint }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }),
        };
        row = row.child(
            div()
                .id(("bloom-swatch", dev.key(0x500 | i)))
                .w(px(28.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(SP1))
                .cursor_pointer()
                .child(pebble)
                .child(caption)
                .tooltip(widgets::tip_owned(format!("{:02} {}\nClick to hear · Alt-click locks", i + 1, scene.sounds[i].role.label())))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if e.modifiers.alt {
                        view::toggle_lock(this, &d, i);
                        cx.notify();
                        return;
                    }
                    this.fx.bloom.view(d.id).dna_cell = Some(i);
                    audition(this, &d, i, cx);
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.release_audition(cx))),
        );
    }
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(SP2))
        .h(px(SWATCHES_H - SP2))
        .flex()
        .justify_center()
        .child(div().px(px(SP2)).py(px(SP1)).rounded(px(r_lg())).bg(with_alpha(pal.surface, 0.55)).child(row))
        .into_any_element()
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

    #[test]
    fn the_pointer_maps_into_the_mass_units() {
        let b = Bounds { origin: point(px(100.), px(50.)), size: gpui::size(px(400.), px(300. + SWATCHES_H)) };
        let pl = panel(b, true, 1.0, 0.0, [0.0; 3], false, [0.0; 3]);
        assert_eq!(pl.scale, 300.0 * 0.54, "fitted into the area above the swatches");
        let (x, y) = units(b, true, point(px(100. + pl.cx + pl.scale * 0.5), px(50. + pl.cy)));
        assert!((x - 0.5).abs() < 1e-5 && y.abs() < 1e-5);
        // Two device pixels per point double every length.
        let hi = panel(b, true, 2.0, 6.0, [0.0; 3], false, [0.0; 3]);
        assert_eq!((hi.width, hi.scale, hi.corner), (800.0, 2.0 * pl.scale, 12.0));
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
