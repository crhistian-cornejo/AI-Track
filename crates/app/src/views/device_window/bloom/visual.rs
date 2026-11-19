//! The Bloom visual: the field of sixteen cells around their core, painted
//! as one image per frame (see `field.rs`), with the cells' labels, the
//! selection and the matching progress drawn on top.
//!
//! Gestures: click a cell to select and hear it; drag it outwards or inwards
//! to regrow it further from or nearer to its core (one undo step on
//! release); Alt-click locks it; the pointer pulls the cells and parts them
//! like a magnet under a ferrofluid. Drop an audio file (or a library sound)
//! on the field to match it into the selected cell.

use super::field::{Blob, Ring, grid_size};
use super::look::Look;
use super::view::{self, CellDrag, RING_LIFE};
use crate::theme::*;
use crate::views::device_ui::{Dev, kit, set_now};
use crate::workspace::{DraggedSample, Workspace};
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene};
use aitrack_stock::instrument::bloom::p;
use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, ExternalPaths, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Styled, TextAlign, canvas, div, point, px,
};
use std::rc::Rc;
use std::sync::Arc;

/// One cell as painted this frame (visual units).
#[derive(Clone)]
struct CellPaint {
    x: f32,
    y: f32,
    r: f32,
    look: Look,
    selected: bool,
    locked: bool,
    label: &'static str,
    phase: f32,
    swell: f32,
    progress: Option<f32>,
}

struct Snapshot {
    cells: Vec<CellPaint>,
    core: (f32, [f32; 3]),
    rings: Vec<(f32, f32, f32, f32, [f32; 3])>,
    time: f32,
    labels: bool,
    animate: bool,
    light: bool,
    guide: gpui::Hsla,
}

/// Visual units → pixels inside `b`: an ellipse that fills the box.
fn axes(b: Bounds<Pixels>) -> (f32, f32, f32, f32, f32) {
    let (x, y, w, h) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
    let (sx, sy) = (w * 0.44, h * 0.42);
    (x + w * 0.5, y + h * 0.5, sx, sy, sx.min(sy))
}

fn to_linear(c: gpui::Hsla) -> [f32; 3] {
    let c = gpui::Rgba::from(c);
    let lin = |v: f32| if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    [lin(c.r), lin(c.g), lin(c.b)]
}

fn to_hsla(rgb: [f32; 3], alpha: f32) -> gpui::Hsla {
    let e = super::look::encode;
    gpui::Rgba { r: e(rgb[0]), g: e(rgb[1]), b: e(rgb[2]), a: alpha }.into()
}

/// The cell under `pos` in a visual of bounds `b`, if any.
fn hit(ws: &mut Workspace, device: aitrack_model::DeviceId, b: Bounds<Pixels>, pos: Point<Pixels>) -> Option<usize> {
    let v = ws.fx.bloom.view(device);
    let (cx, cy, sx, sy, s) = axes(b);
    let s = s * (90. / s.max(1.)).clamp(1., 1.7);
    let (mx, my) = (f32::from(pos.x), f32::from(pos.y));
    (0..SOUNDS)
        .map(|i| {
            let a = v.anim[i];
            (i, ((cx + a.x * sx - mx).powi(2) + (cy + a.y * sy - my).powi(2)).sqrt())
        })
        .filter(|(_, d)| *d < s * 0.16)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|x| x.0)
}

/// Pointer position in visual units.
fn units(b: Bounds<Pixels>, pos: Point<Pixels>) -> (f32, f32) {
    let (cx, cy, sx, sy, _) = axes(b);
    ((f32::from(pos.x) - cx) / sx.max(1.), (f32::from(pos.y) - cy) / sy.max(1.))
}

/// Play cell `i` (and select it): the note it answers to in the device's mode.
pub fn audition(ws: &mut Workspace, dev: &Dev, i: usize, cx: &mut Context<Workspace>) {
    if dev.choice(p::SELECTED) != i {
        set_now(ws, dev.id, p::SELECTED, dev.to_norm(p::SELECTED, i as f32));
    }
    let Some(track) = ws.s.project.device_track(dev.id) else { return };
    let kind = view::kind(dev);
    let key = match dev.choice(p::MODE) {
        // A tonal patch is heard at its kind's demo note (a bass low, a lead high).
        _ if kind.is_tonal() => aitrack_stock::instrument::bloom::demo_note(kind).0,
        0 => (dev.plain(p::BASE).round() as i32 + i as i32).clamp(0, 127) as u8,
        _ => 60,
    };
    let crate::workspace::Workspace { audition, b, .. } = ws;
    audition.press(crate::audition::MOUSE, track, key, |t, k, v| b.engine.audition_note(t, k, v));
    cx.notify();
}

/// The visual of a Bloom. `labels`: role names under the cells (the big
/// layouts).
pub fn visual(ws: &mut Workspace, dev: &Dev, scene: &Scene, labels: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let device = dev.id;
    let changed = view::poll(ws, dev, cx);
    let reduced = ws.look.app.reduced_motion || ws.look.app.low_power;
    let env: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(i));
    let hits: [f32; SOUNDS] = std::array::from_fn(|i| dev.m(32 + i));
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    // The field shows what plays (the morph blend), and a cell being matched
    // the search's best so far.
    let mut shown = view::played(ws, dev, scene);
    let matching = ws.fx.bloom.view(device).matching.as_ref().map(|m| (m.cell, m.fraction, m.best));
    if let Some((cell, _, Some(best))) = matching {
        shown.sounds[cell] = best;
    }
    let looks = view::looks(ws, dev, &shown);
    // A tonal kind's cells rest at its patches' distances, labelled by kind.
    let tonal = view::kind(dev).is_tonal();
    let (distances, locks) = if tonal {
        let ps = view::shown_patches(ws, dev);
        (ps.distances, std::array::from_fn::<bool, SOUNDS, _>(|i| ps.locked(i)))
    } else {
        (scene.distances, std::array::from_fn(|i| scene.locked(i)))
    };
    let kind_label = view::kind(dev).label();
    let busy = ws.fx.bloom.busy();
    let v = ws.fx.bloom.view(device);
    let moving = view::step(v, &distances, &env, &hits, reduced);
    let cells: Vec<CellPaint> = (0..SOUNDS)
        .map(|i| {
            let a = v.anim[i];
            let look = looks[i];
            let base = 0.07 + 0.035 * look.glow;
            let progress = matching.filter(|m| m.0 == i).map(|m| m.1);
            let grow = progress.map_or(1.0, |f| 0.55 + 0.6 * f);
            CellPaint {
                x: a.x,
                y: a.y,
                r: base * (1.0 + 0.6 * a.swell) * grow,
                look,
                selected: i == selected,
                locked: locks[i],
                label: if tonal { kind_label } else { scene.sounds[i].role.short() },
                phase: i as f32 * 0.7 + v.time * 0.25 * look.wobble,
                swell: a.swell,
                progress,
            }
        })
        .collect();
    let rings = v
        .rings
        .iter()
        .map(|r| {
            let a = v.anim[r.cell];
            let t = r.age / RING_LIFE;
            // Rings rush out and settle (the app's decelerating curve) as they fade.
            (a.x, a.y, cells[r.cell].r + crate::motion::bezier(t, crate::motion::DECELERATE) * 0.5, (1.0 - t) * (1.0 - t) * 0.85, looks[r.cell].rgb)
        })
        .collect();
    let pal = p();
    let rms = dev.m(80).clamp(0., 1.);
    let snap = Rc::new(Snapshot {
        cells,
        core: (0.1 * (1.0 + rms * 0.8), to_linear(mix(pal.muted, pal.text, 0.35))),
        rings,
        time: v.time,
        labels,
        animate: moving || changed || busy || v.drag.is_some() || v.gene_drag.is_some(),
        light: !pal.dark,
        guide: with_alpha(pal.faint, 0.18),
    });
    let painter = v.painter.clone();
    let cost = v.cost.clone();
    let enabled = dev.enabled;
    // Kept per window (the card and the device window may both show it).
    let region = ws.fx.region(device, 7);
    let store = region.clone();
    let surface = canvas(
        move |b, _, _| store.set(b),
        move |b, _, window, cx| {
            window.with_content_mask(Some(gpui::ContentMask { bounds: b }), |window| paint(&snap, &painter, &cost, b, enabled, window, cx));
        },
    )
    .size_full();

    // ── Gestures ──
    let (d, r) = (dev.clone(), region.clone());
    let down = cx.listener(move |this, e: &MouseDownEvent, _, cx| {
        cx.stop_propagation();
        let Some(i) = hit(this, d.id, r.get(), e.position) else { return };
        if e.modifiers.alt {
            view::toggle_lock(this, &d, i);
            cx.notify();
            return;
        }
        let dist = view::distance(this, &d, i);
        this.fx.bloom.view(d.id).drag = Some(CellDrag { cell: i, from: (f32::from(e.position.x), f32::from(e.position.y)), moved: false, distance: dist, start: dist });
        this.fx.bloom.view(d.id).dna_cell = Some(i);
        audition(this, &d, i, cx);
    });
    let (d, r) = (dev.clone(), region.clone());
    let hover = cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
        let at = units(r.get(), e.position);
        let v = this.fx.bloom.view(d.id);
        v.cursor = Some(at);
        v.cursor_at = Some(std::time::Instant::now());
        cx.notify();
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
        .id(("bloom-visual", dev.key(0xB10)))
        .relative()
        .size_full()
        .rounded(px(r_md()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .overflow_hidden()
        .cursor_pointer()
        .child(surface)
        .on_mouse_down(MouseButton::Left, down)
        .on_mouse_move(hover)
        .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(accent()).bg(accent_alpha(0.06)))
        .drag_over::<DraggedSample>(|s, _, _, _| s.border_color(accent()).bg(accent_alpha(0.06)))
        .on_drop(drop_file)
        .on_drop(drop_sample);
    if ws.fx.bloom.view(device).drag.is_some() {
        root = root.child(capture(dev, region, cx));
    }
    root.into_any_element()
}

/// Window-wide listeners while a cell is dragged.
fn capture(dev: &Dev, region: crate::views::device_ui::Region, cx: &mut Context<Workspace>) -> AnyElement {
    let weak = cx.entity().downgrade();
    let d = dev.clone();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let (w, dd, rr) = (weak.clone(), d.clone(), region.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = w.update(cx, |this, cx| {
                    let (ux, uy) = units(rr.get(), e.position);
                    let v = this.fx.bloom.view(dd.id);
                    let Some(mut drag) = v.drag else { return };
                    let (fx, fy) = drag.from;
                    if (f32::from(e.position.x) - fx).hypot(f32::from(e.position.y) - fy) > 4. {
                        drag.moved = true;
                    }
                    if drag.moved {
                        // Steps of 0.02: a drag visits a bounded set of sounds.
                        drag.distance = (view::distance_of((ux * ux + uy * uy).sqrt()) * 50.).round() / 50.;
                        v.drag = Some(drag);
                        view::preview_regrow(this, &dd, drag.cell, drag.distance);
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
                    let Some(drag) = this.fx.bloom.view(dd.id).drag.take() else { return };
                    view::finish_regrow(this, &dd, drag.moved);
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn paint(snap: &Snapshot, painter: &std::cell::RefCell<view::Painter>, cost: &std::cell::Cell<f32>, b: Bounds<Pixels>, enabled: bool, window: &mut gpui::Window, cx: &mut gpui::App) {
    let (cx0, cy0, sx, sy, s) = axes(b);
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    if w < 8. || h < 8. {
        return;
    }
    let pal = p();
    // Orbit guides: where distance 0.3, 0.6 and 1.0 sit.
    for d in [0.3f32, 0.6, 1.0] {
        let r = view::radius_of(d);
        let pts: Vec<Point<Pixels>> = (0..=72)
            .map(|k| {
                let a = k as f32 / 72. * std::f32::consts::TAU;
                point(px(cx0 + a.cos() * r * sx), px(cy0 + a.sin() * r * sy))
            })
            .collect();
        crate::paint::polyline(window, &pts, 1., snap.guide);
    }

    // The field.
    let scale = window.scale_factor();
    let (gw, gh) = grid_size(w, h, scale);
    let (kx, ky) = (gw as f32 / w, gh as f32 / h);
    // Small visuals (the card) keep their cells readable.
    let grow = (90. / s.max(1.)).clamp(1., 1.7);
    let unit = s * kx * grow;
    let mut blobs: Vec<Blob> = Vec::with_capacity(SOUNDS + 1);
    blobs.push(Blob { x: (cx0 - f32::from(b.origin.x)) * kx, y: (cy0 - f32::from(b.origin.y)) * ky, r: snap.core.0 * unit, rgb: snap.core.1, lobes: 6, spikes: 0.08, sharp: 2.0, wobble: 0.2, phase: snap.time * 0.1, glow: 0.12 });
    for c in &snap.cells {
        let l = &c.look;
        let rgb = if enabled { l.rgb } else { snap.core.1 };
        blobs.push(Blob {
            x: (cx0 + c.x * sx - f32::from(b.origin.x)) * kx,
            y: (cy0 + c.y * sy - f32::from(b.origin.y)) * ky,
            r: c.r * unit,
            rgb,
            lobes: l.lobes,
            spikes: l.spikes,
            sharp: l.sharp,
            wobble: l.wobble,
            phase: c.phase,
            glow: (0.12 + 0.9 * c.swell) * l.glow,
        });
    }
    let rings: Vec<Ring> = snap
        .rings
        .iter()
        .map(|&(x, y, r, a, rgb)| Ring { x: (cx0 + x * sx - f32::from(b.origin.x)) * kx, y: (cy0 + y * sy - f32::from(b.origin.y)) * ky, radius: r * unit, width: (0.022 * unit).max(1.2), alpha: a, rgb })
        .collect();
    let t0 = std::time::Instant::now();
    let image = {
        let mut pt = painter.borrow_mut();
        let pixels = pt.field.render(gw, gh, &blobs, &rings, snap.time, snap.light).to_vec();
        let img = image::RgbaImage::from_raw(gw as u32, gh as u32, pixels).map(|buf| Arc::new(gpui::RenderImage::new(vec![image::Frame::new(buf)])));
        let wid = window.window_handle().window_id().as_u64();
        if let Some(img) = &img
            && let Some(old) = pt.images.insert(wid, img.clone())
        {
            let _ = window.drop_image(old);
        }
        img
    };
    let ms = t0.elapsed().as_secs_f32() * 1000.;
    cost.set(if cost.get() == 0. { ms } else { cost.get() * 0.9 + ms * 0.1 });
    if let Some(img) = image {
        let _ = window.paint_image(b, b, gpui::Corners::default(), img, 0, false);
    }

    // Overlays: selection, locks, labels, matching progress.
    for c in &snap.cells {
        let center = point(px(cx0 + c.x * sx), px(cy0 + c.y * sy));
        let r = c.r * s * grow;
        if c.selected {
            crate::paint::arc(window, center, r + 5., 0., std::f32::consts::TAU, 1.5, with_alpha(pal.text, 0.85));
        }
        if let Some(f) = c.progress {
            crate::paint::arc(window, center, r + 9., -std::f32::consts::FRAC_PI_2, -std::f32::consts::FRAC_PI_2 + f.max(0.02) * std::f32::consts::TAU, 2., pal.accent2);
        }
        if c.locked {
            let at = point(center.x + px(r * 0.75), center.y - px(r * 0.95));
            kit::dot(window, at, 4., pal.well, Some(pal.text));
        }
        if snap.labels || c.selected {
            let color = if c.selected { pal.text } else { with_alpha(to_hsla(c.look.rgb, 1.), 0.9) };
            kit::txt(window, cx, c.label, f32::from(center.x), f32::from(center.y) + r + 4., TEXT_XS, mix(color, pal.muted, 0.35), TextAlign::Center);
        }
    }
    if snap.animate {
        window.request_animation_frame();
    }
}
