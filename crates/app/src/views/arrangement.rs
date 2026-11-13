//! Arrangement View: ruler with loop brace, track headers, and one canvas
//! that paints every lane (grid, clips with note minis or waveforms,
//! automation, playhead). Only what is on screen is painted. What is done
//! to a clip in its lane (fades, edges, copies) is in `views::arrange_clips`.

use crate::gesture::{self, Wheel};
use crate::icons::{Icon, icon};
use crate::paint::{self, at};
use crate::state::{AppState, Snap};
use crate::theme::*;
use crate::views::arrange_clips::{self, ClipBox, Zone, draw::Wave};
use crate::views::browser::kind_icon;
use crate::views::sample_editor::geom::TimeBase;
use crate::widgets::{self, chip, icon_chip, label, menu_header, menu_item, mono};
use crate::workspace::{Drag, MenuKind, NumTarget, Workspace};
use aitrack_model::*;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    PinchEvent, Pixels, Point, ScrollWheelEvent, StatefulInteractiveElement, Styled, TextAlign, Window, canvas, div,
    point, prelude::FluentBuilder, px,
};
use gpui::{DispatchPhase, HitboxBehavior, MouseMoveEvent};
use std::rc::Rc;

/// Height of one automation lane.
pub const AUTO_H: f32 = 56.;

/// Pixels between the bottom (0) and top (1) of an automation lane's values.
pub fn auto_value_px() -> f32 {
    AUTO_H - 12.
}
const CLIP_HEAD: f32 = 14.;

#[derive(Clone, Debug)]
pub struct LaneRow {
    pub track: TrackId,
    pub kind: TrackKind,
    pub top: f32,
    pub h: f32,
    /// The automation lanes shown under the track, top to bottom.
    pub autos: Vec<AutomationTarget>,
    /// How many groups the track sits inside: the header indents by it.
    pub depth: usize,
}

impl LaneRow {
    pub fn total(&self) -> f32 {
        self.h + AUTO_H * self.autos.len() as f32
    }

    /// The automation lane `ly` px below the row's top, and `ly` inside it.
    pub fn lane_at(&self, ly: f32) -> Option<(usize, AutomationTarget, f32)> {
        let k = ((ly - self.h) / AUTO_H).floor();
        if k < 0. {
            return None;
        }
        let k = k as usize;
        self.autos.get(k).map(|t| (k, *t, ly - self.h - k as f32 * AUTO_H))
    }
}

/// The visible lanes, top to bottom (children of collapsed groups hidden).
pub fn rows(s: &AppState) -> Vec<LaneRow> {
    let mut out = Vec::new();
    let mut y = 0.;
    for t in s.project.content_tracks() {
        // Return tracks sit with Main in the pinned bus section at the bottom.
        if t.kind == TrackKind::Return {
            continue;
        }
        // Inside a folded group, at any depth.
        if groups::hidden(&s.project, t.id) {
            continue;
        }
        let autos = s.shown_lanes(t.id);
        let h = crate::theme::density(t.height.max(40.).round());
        out.push(LaneRow { track: t.id, kind: t.kind, top: y, h, autos, depth: groups::depth(&s.project, t.id) });
        y += out.last().map_or(0., LaneRow::total);
    }
    out
}

fn total_height(rows: &[LaneRow]) -> f32 {
    rows.last().map_or(0., |r| r.top + r.total())
}

/// The screen-space line of a lane: its breakpoints, with samples along the
/// segments that are curved so the drawing follows what the engine plays.
fn automation_line(lane: &aitrack_model::AutomationLane, px_per_tick: f32, scroll_x: f32) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(lane.points.len());
    for (i, p) in lane.points.iter().enumerate() {
        out.push((p.tick as f32 * px_per_tick - scroll_x, p.value));
        if let Some(next) = lane.points.get(i + 1)
            && p.curve.abs() > 1e-3
        {
            for k in 1..16 {
                let tick = p.tick + (next.tick - p.tick) * k / 16;
                if let Some(v) = lane.value_at(tick) {
                    out.push((tick as f32 * px_per_tick - scroll_x, v));
                }
            }
        }
    }
    out
}

/// Normalised automation value under a window position in `track`'s lane for `target`.
pub fn automation_value_at(ws: &Workspace, track: TrackId, target: AutomationTarget, pos: Point<Pixels>) -> Option<f32> {
    let row = rows(&ws.s).into_iter().find(|r| r.track == track)?;
    let k = row.autos.iter().position(|t| *t == target)?;
    let b = ws.regions.arrange.get();
    let y = f32::from(pos.y - b.origin.y) + ws.s.arrange.scroll_y - row.top - row.h - k as f32 * AUTO_H;
    Some((1. - (y - 6.) / (AUTO_H - 12.)).clamp(0., 1.))
}

/// A device's automatable parameters as (id, name): a stock device's from
/// the catalogue, a hosted plugin's as it reported them once loaded (hidden,
/// read-only and MIDI-CC proxy parameters left out). Racks have macros instead.
pub fn device_params(ws: &Workspace, d: &Device) -> Vec<(u32, String)> {
    match &d.kind {
        DeviceKind::Stock { plugin_id } => {
            ws.b.plugins.device(plugin_id).map(|i| i.params.iter().filter(|p| p.automatable).map(|p| (p.id, p.name.to_owned())).collect()).unwrap_or_default()
        }
        DeviceKind::Plugin { .. } => ws.b.plugins.device_params(d.id).into_iter().filter(|p| p.automatable && !is_midi_proxy(p.name)).map(|p| (p.id, p.name.to_owned())).collect(),
        DeviceKind::Rack(_) => Vec::new(),
    }
}

/// JUCE-wrapped plugins list 16 × 128 "MIDI CC n|m" parameters for MIDI
/// learn; they are not something to draw automation for.
fn is_midi_proxy(name: &str) -> bool {
    name.starts_with("MIDI CC ")
}

/// A parameter's name, or `P<id>` when the device does not list it (a
/// plugin that is not loaded).
fn param_name(ws: &Workspace, d: &Device, param: u32) -> String {
    let stock = match &d.kind {
        DeviceKind::Stock { plugin_id } => ws.b.plugins.device(plugin_id).and_then(|i| i.params.iter().find(|p| p.id == param)).map(|p| p.name.to_owned()),
        DeviceKind::Plugin { .. } => ws.b.plugins.device_params(d.id).into_iter().find(|p| p.id == param).map(|p| p.name.to_owned()),
        DeviceKind::Rack(_) => None,
    };
    stock.unwrap_or_else(|| format!("P{param}"))
}

/// The value a lane holds when it has no breakpoints (normalised as the
/// lane stores it): the fader, the knob, the switch. Drawn as a faint line.
pub fn current_value(p: &Project, track: &Track, target: AutomationTarget) -> Option<f32> {
    let db = |db: f32| if db <= -70. { 0. } else { ((db + 70.) / 76.).clamp(0., 1.) };
    Some(match target {
        AutomationTarget::Volume => db(track.volume_db),
        AutomationTarget::Pan => (track.pan + 1.) / 2.,
        AutomationTarget::Mute => f32::from(u8::from(track.mute)),
        AutomationTarget::Send { index } => db(track.sends.get(index as usize)?.level_db),
        AutomationTarget::Tempo => ((p.tempo.initial_bpm() as f32 - 20.) / 280.).clamp(0., 1.),
        AutomationTarget::DeviceParam { device, param } => *p.device(device)?.params.get(&param)?,
        AutomationTarget::DeviceEnabled { device } => f32::from(u8::from(p.device(device)?.enabled)),
        AutomationTarget::Macro { device, index } => match &p.device(device)?.kind {
            DeviceKind::Rack(r) => r.macros.get(index as usize)?.value,
            _ => return None,
        },
    })
}

/// A lane's value as text for its header (the mappings of `aitrack_engine::automation`).
pub fn value_text(target: AutomationTarget, v: f32) -> String {
    match target {
        AutomationTarget::Volume | AutomationTarget::Send { .. } => {
            if v <= 0. { "-inf dB".into() } else { format!("{} dB", crate::state::format_db(v.min(1.) * 76. - 70.)) }
        }
        AutomationTarget::Pan => pan_text(v * 2. - 1.),
        AutomationTarget::Mute => (if v >= 0.5 { "Muted" } else { "On" }).into(),
        AutomationTarget::DeviceEnabled { .. } => (if v >= 0.5 { "On" } else { "Off" }).into(),
        AutomationTarget::Tempo => format!("{:.1} BPM", 20. + v.clamp(0., 1.) * 280.),
        AutomationTarget::DeviceParam { .. } | AutomationTarget::Macro { .. } => format!("{:.0} %", v * 100.),
    }
}

/// Pan as Live writes it: C, 25L, 50R.
pub fn pan_text(pan: f32) -> String {
    if pan.abs() < 0.01 { "C".to_owned() } else if pan < 0. { format!("{:.0}L", -pan * 50.) } else { format!("{:.0}R", pan * 50.) }
}

/// A target's name inside its chooser group (no device prefix).
fn short_name(ws: &Workspace, t: AutomationTarget) -> String {
    let p = &ws.s.project;
    match t {
        AutomationTarget::DeviceParam { device, param } => p.device(device).map_or(format!("P{param}"), |d| param_name(ws, d, param)),
        AutomationTarget::DeviceEnabled { .. } => "Device On".into(),
        AutomationTarget::Macro { device, index } => match p.device(device).map(|d| &d.kind) {
            Some(DeviceKind::Rack(r)) => r.macros.get(index as usize).map_or(format!("Macro {}", index + 1), |m| m.name.clone()),
            _ => format!("Macro {}", index + 1),
        },
        _ => target_name(ws, t),
    }
}

/// A lane's full name: "Volume", "Send B", "EQ Eight · Gain 1".
pub fn target_name(ws: &Workspace, t: AutomationTarget) -> String {
    let p = &ws.s.project;
    match t {
        AutomationTarget::Volume => "Volume".into(),
        AutomationTarget::Pan => "Pan".into(),
        AutomationTarget::Mute => "Mute".into(),
        AutomationTarget::Send { index } => format!("Send {}", (b'A' + index as u8) as char),
        AutomationTarget::Tempo => "Tempo".into(),
        AutomationTarget::DeviceParam { device, .. } | AutomationTarget::DeviceEnabled { device } | AutomationTarget::Macro { device, .. } => {
            let name = p.device(device).map_or("Device".into(), |d| d.name.clone());
            format!("{name} · {}", short_name(ws, t))
        }
    }
}

enum Body {
    Notes { notes: Vec<(u8, Tick, Tick)>, lo: u8, hi: u8, loop_len: Tick },
    /// The decoded file (`None` while it is read, or when it cannot be) and
    /// what shapes its level: gain, fades, volume envelope, offset and loop.
    Audio { wave: Option<Wave>, shape: Clip },
}

struct ClipPaint {
    id: PlacementId,
    /// The clip's rectangle, its fades and where their handles are.
    bx: ClipBox,
    looping: bool,
    x: f32,
    w: f32,
    y: f32,
    h: f32,
    color: Color,
    name: String,
    selected: bool,
    body: Body,
    px_per_tick: f32,
    offset: Tick,
    /// Deactivated with 0: drawn drained of colour.
    muted: bool,
    /// Where a looping audio clip starts over, in pixels from its left edge.
    repeats: Vec<f32>,
}

/// What a group's lane shows: where each track inside it has clips, one
/// thin bar per track in that track's colour (as in Live).
struct GroupPaint {
    y: f32,
    h: f32,
    color: Color,
    /// Per member: its colour and its merged clip spans in pixels.
    members: Vec<(Color, Vec<(f32, f32)>)>,
}

struct AutoPaint {
    y: f32,
    points: Vec<(f32, f32)>,
    /// The lane as drawn: breakpoints plus samples along curved segments.
    line: Vec<(f32, f32)>,
    color: Hsla,
    /// The selected range of this lane, left and right in pixels (equal for a caret).
    sel: Option<(f32, f32)>,
    /// A lane with no breakpoints: the value it holds (the fader, the knob),
    /// drawn as a faint flat line, as in Live.
    idle: Option<f32>,
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    // Follow Playhead: turn the page when the playhead leaves the lanes.
    if ws.s.ed.follow && ws.b.engine.transport().playing {
        let width = f32::from(ws.regions.arrange.get().size.width);
        if let Some(x) = crate::nav::follow(ws.s.arrange, ws.b.engine.transport().position, width) {
            ws.s.arrange.scroll_x = x;
        }
    }
    let rows = rows(&ws.s);
    let view = ws.s.arrange;
    let ppb = view.px_per_beat;
    let px_per_tick = ppb / PPQ as f32;
    let transport = ws.b.engine.transport();
    let lane_w = f32::from(ws.regions.arrange.get().size.width).max(1200.);
    let lane_h = f32::from(ws.regions.arrange.get().size.height).max(600.);
    let t0 = (view.scroll_x / px_per_tick) as Tick;
    let t1 = ((view.scroll_x + lane_w) / px_per_tick) as Tick;
    let song_end = ws.s.project.arrangement_end();
    let loop_region = ws.s.project.loop_region;
    let selection = ws.s.selected_clip_ids();
    let bpm = ws.s.project.tempo.initial_bpm();

    // ── Gather what the lane canvas paints ──
    let mut clips = Vec::new();
    let mut autos = Vec::new();
    let mut lanes = Vec::new();
    let mut group_lanes = Vec::new();
    for r in &rows {
        let Some(t) = ws.s.project.track(r.track) else { continue };
        let ry = r.top - view.scroll_y;
        lanes.push((ry, r.h, t.color, r.autos.len(), ws.s.is_track_selected(t.id)));
        if ry > lane_h || ry + r.total() < 0. {
            continue;
        }
        if t.kind == TrackKind::Group {
            let members = groups::member_spans(&ws.s.project, t.id)
                .into_iter()
                .filter_map(|(id, spans)| {
                    let spans: Vec<(f32, f32)> = spans
                        .into_iter()
                        .filter(|(a, b)| *b >= t0 && *a <= t1)
                        .map(|(a, b)| (a as f32 * px_per_tick - view.scroll_x, b as f32 * px_per_tick - view.scroll_x))
                        .collect();
                    Some((ws.s.project.track(id)?.color, spans))
                })
                .collect();
            group_lanes.push(GroupPaint { y: ry, h: r.h, color: t.color, members });
        }
        for pl in &t.arrangement {
            let Some(c) = ws.s.project.clips.get(&pl.clip) else { continue };
            if pl.start + c.length < t0 || pl.start > t1 {
                continue;
            }
            let body = match ws.s.project.sources.get(&c.source) {
                Some(Source::Notes(n)) => {
                    let visible: Vec<(u8, Tick, Tick)> = n.notes.iter().filter(|n| n.start >= c.offset).map(|n| (n.key, n.start, n.length)).collect();
                    let lo = visible.iter().map(|n| n.0).min().unwrap_or_else(|| n.notes.iter().map(|n| n.key).min().unwrap_or(60));
                    let hi = visible.iter().map(|n| n.0).max().unwrap_or_else(|| n.notes.iter().map(|n| n.key).max().unwrap_or(60));
                    Body::Notes {
                        notes: visible,
                        lo,
                        hi,
                        loop_len: c.looping.map_or(c.length, |l| l.end - l.start).max(1),
                    }
                }
                Some(Source::Audio(file)) => {
                    // Decoded in the background: flat until the peaks arrive.
                    let wave = ws.waves.peaks(&file.path).map(|peaks| {
                        let base = TimeBase { sample_rate: f64::from(peaks.sample_rate().max(1)), ..TimeBase::of(file, bpm) };
                        Wave {
                            frames_per_tick: base.tick_to_frame(1.),
                            ratio: file.warp.speed_ratio(),
                            reverse: file.warp.reverse,
                            peaks,
                        }
                    });
                    Body::Audio { wave, shape: arrange_clips::draw::shape_of(c) }
                }
                None => Body::Audio { wave: None, shape: arrange_clips::draw::shape_of(c) },
            };
            let audio = matches!(body, Body::Audio { .. });
            // Notes draw their own repeats; audio gets a notch at each one.
            let repeats = if matches!(body, Body::Audio { .. }) { crate::editing::overlay::loop_repeats(c, px_per_tick) } else { Vec::new() };
            clips.push(ClipPaint {
                id: pl.id,
                bx: ClipBox::of(c, audio, pl.start, ry, r.h, px_per_tick, view.scroll_x),
                looping: c.looping.is_some_and(|l| l.end > l.start),
                x: pl.start as f32 * px_per_tick - view.scroll_x,
                w: c.length as f32 * px_per_tick,
                y: ry + 2.,
                h: r.h - 4.,
                color: c.color,
                name: c.name.clone(),
                selected: selection.contains(&pl.id),
                body,
                px_per_tick,
                offset: c.offset,
                muted: c.muted,
                repeats,
            });
        }
        for (k, &target) in r.autos.iter().enumerate() {
            let lane = t.automation.iter().find(|l| l.target == target && !l.points.is_empty());
            autos.push(AutoPaint {
                y: ry + r.h + k as f32 * AUTO_H,
                points: lane.map(|l| l.points.iter().map(|p| (p.tick as f32 * px_per_tick - view.scroll_x, p.value)).collect()).unwrap_or_default(),
                line: lane.map(|l| automation_line(l, px_per_tick, view.scroll_x)).unwrap_or_default(),
                color: crate::theme::mix(track_color(t.color), p().automation, 0.35),
                sel: ws.s.auto_sel.filter(|r| r.track == t.id && r.target == target).map(|r| (r.start as f32 * px_per_tick - view.scroll_x, r.end as f32 * px_per_tick - view.scroll_x)),
                idle: if lane.is_none() { current_value(&ws.s.project, t, target) } else { None },
            });
        }
    }
    // The grid, the insert marker and the time selection.
    let lane_rects: Vec<(TrackId, f32, f32)> = rows.iter().map(|r| (r.track, r.top - view.scroll_y, r.h)).collect();
    let overlay = crate::editing::overlay::Overlay::arrange(&ws.s, &lane_rects);
    let ruler_overlay = overlay.clone();
    let playhead = transport.position as f32 * px_per_tick - view.scroll_x;
    let loop_px = loop_region.map(|(a, b)| (a as f32 * px_per_tick - view.scroll_x, b as f32 * px_per_tick - view.scroll_x));
    let end_px = song_end as f32 * px_per_tick - view.scroll_x;

    // What is under the pointer is worked out while painting, from these.
    let boxes: Rc<[ClipBox]> = clips.iter().map(|c| c.bx).collect();
    let held = arrange_clips::held(ws);
    let copying = arrange_clips::copying(ws);
    let idle = ws.drag.is_none();
    let regions = ws.regions.clone();
    let lane_canvas = canvas(
        move |b, window, _| {
            regions.arrange.set(b);
            window.insert_hitbox(b, HitboxBehavior::Normal)
        },
        move |b, hitbox, window, cx| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            window.paint_quad(paint::fill_rect(b, p().well));
            // Lane backgrounds.
            for (y, lh, _, autos, sel) in &lanes {
                window.paint_quad(paint::fill_rect(at(o, 0., *y, w, *lh), if *sel { content(0.025) } else { gpui::transparent_black() }));
                window.paint_quad(paint::fill_rect(at(o, 0., *y + *lh - 1., w, 1.), border()));
                for k in 0..*autos {
                    let ay = *y + *lh + k as f32 * AUTO_H;
                    window.paint_quad(paint::fill_rect(at(o, 0., ay, w, AUTO_H), content(0.015)));
                    window.paint_quad(paint::fill_rect(at(o, 0., ay + AUTO_H - 1., w, 1.), border()));
                }
            }
            // Grid: bars, beats and the subdivisions the zoom leaves room for.
            overlay.paint_grid(window, o, w, h, true);
            for g in &group_lanes {
                paint_group_lane(window, o, w, g);
            }
            if let Some((a, bb)) = loop_px {
                window.paint_quad(paint::fill_rect(at(o, a, 0., bb - a, h), accent_alpha(0.035)));
            }
            if end_px > 0. && end_px < w {
                window.paint_quad(paint::fill_rect(at(o, end_px, 0., 1., h), content(0.18)));
            }
            // Clips. The one under the pointer, or held by a drag, is lit
            // and shows its handles; the pointer shows what a press does.
            let hot = match held {
                Some((id, zone)) => clips.iter().position(|c| c.id == id).map(|i| (i, zone)),
                None if idle && hitbox.is_hovered(window) => {
                    let at = window.mouse_position();
                    arrange_clips::geom::topmost(&boxes, f32::from(at.x - o.x), f32::from(at.y - o.y))
                }
                None => None,
            };
            for (i, c) in clips.iter().enumerate() {
                paint_clip(window, cx, o, c, w, hot.filter(|h| h.0 == i).map(|h| h.1));
            }
            window.set_cursor_style(arrange_clips::cursor_for(hot.map(|h| h.1), copying), &hitbox);
            if idle {
                // Paint again when the pointer moves on to another target.
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
                    if phase == DispatchPhase::Bubble {
                        let now = hitbox.is_hovered(window).then(|| arrange_clips::geom::topmost(&boxes, f32::from(e.position.x - o.x), f32::from(e.position.y - o.y))).flatten();
                        if now != hot {
                            window.refresh();
                        }
                    }
                });
            }
            // Automation.
            for a in &autos {
                let lane_top = a.y + 6.;
                let lane_h = AUTO_H - 12.;
                // The selected range: a tinted band with edges, under the line.
                if let Some((l, r)) = a.sel {
                    window.paint_quad(paint::fill_rect(at(o, l, a.y, (r - l).max(0.), AUTO_H - 1.), accent_alpha(0.18)));
                    window.paint_quad(paint::fill_rect(at(o, l, a.y, 1., AUTO_H - 1.), accent_alpha(0.8)));
                    if r > l {
                        window.paint_quad(paint::fill_rect(at(o, r - 1., a.y, 1., AUTO_H - 1.), accent_alpha(0.8)));
                    }
                }
                if let Some(v) = a.idle {
                    let y = lane_top + lane_h * (1. - v);
                    window.paint_quad(paint::fill_rect(at(o, 0., y, w, 1.), with_alpha(a.color, 0.35)));
                    continue;
                }
                let mut pts: Vec<Point<Pixels>> = Vec::with_capacity(a.points.len() + 2);
                if let Some(first) = a.points.first() {
                    pts.push(point(o.x, o.y + px(lane_top + lane_h * (1. - first.1))));
                }
                for (x, v) in &a.line {
                    pts.push(point(o.x + px(*x), o.y + px(lane_top + lane_h * (1. - v))));
                }
                if let Some(last) = a.points.last() {
                    pts.push(point(o.x + px(w), o.y + px(lane_top + lane_h * (1. - last.1))));
                }
                paint::polyline(window, &pts, 1.5, a.color);
                for (x, v) in &a.points {
                    let c = point(o.x + px(*x), o.y + px(lane_top + lane_h * (1. - v)));
                    window.paint_quad(paint::rrect_border(
                        gpui::Bounds::new(point(c.x - px(3.5), c.y - px(3.5)), gpui::size(px(7.), px(7.))),
                        3.5,
                        p().well,
                        a.color,
                        1.5,
                    ));
                }
            }
            // Time selection and insert marker.
            overlay.paint_time_selection(window, o);
            overlay.paint_marker(window, o, w, h);
            // Playhead.
            if playhead >= 0. && playhead <= w {
                window.paint_quad(paint::fill_rect(at(o, playhead, 0., 1., h), accent()));
            }
        },
    )
    .size_full();

    let lanes_el = div()
        .id("arrange-lanes")
        .relative()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .overflow_hidden()
        .child(lane_canvas)
        .on_mouse_down(MouseButton::Left, cx.listener(lane_mouse_down))
        .on_mouse_down(MouseButton::Right, cx.listener(lane_right_click))
        .drag_over::<crate::workspace::DraggedSample>(|s, _, _, _| s.bg(accent_alpha(0.06)))
        // A library sample dropped on a lane lands on that track (audio
        // tracks; otherwise a new one) at the beat under the cursor.
        .on_drop(cx.listener(|this, d: &crate::workspace::DraggedSample, window, cx| {
            let pos = window.mouse_position();
            let (tick, row) = this.arrange_pos(pos);
            // On the grid line nearest to where it was dropped.
            let start = this.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), false);
            this.add_sample_clip(&d.0, row, Some(start), cx);
        }))
        .on_scroll_wheel(cx.listener(on_wheel))
        .on_pinch(cx.listener(on_pinch));

    // ── Track headers ──
    let mut headers = div().relative().w_full().top(px(-view.scroll_y)).flex().flex_col();
    for r in &rows {
        headers = headers.child(track_header(ws, r, cx));
    }
    let header_col = div()
        .id("arrange-headers")
        .flex_none()
        .w(px(TRACK_HEADER_W))
        .h_full()
        .overflow_hidden()
        .border_r_1()
        .border_color(border())
        .bg(pal.surface)
        .child(headers)
        .on_scroll_wheel(cx.listener(on_wheel));

    // ── Ruler ──
    let regions = ws.regions.clone();
    let ruler = canvas(
        move |b, _, _| regions.ruler.set(b),
        move |b, _, window, cx| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            window.paint_quad(paint::fill_rect(b, p().surface));
            window.paint_quad(paint::fill_rect(at(o, 0., h - 1., w, 1.), border()));
            if let Some((a, bb)) = loop_px {
                window.paint_quad(paint::rrect(at(o, a, 3., bb - a, 8.), 3., accent_alpha(0.75)));
            }
            // Bar numbers, thinning out when zoomed out; beats when zoomed in.
            ruler_overlay.paint_ruler(window, cx, o, w, h, 12.);
            ruler_overlay.paint_ruler_selection(window, o, h);
            ruler_overlay.paint_marker_flag(window, o, w, h);
            if playhead >= 0. && playhead <= w {
                paint::polygon(
                    window,
                    &[point(o.x + px(playhead - 5.), o.y + px(h - 8.)), point(o.x + px(playhead + 5.), o.y + px(h - 8.)), point(o.x + px(playhead), o.y + px(h - 1.))],
                    accent(),
                );
            }
        },
    )
    .size_full();
    let ruler_el = div()
        .id("ruler")
        .flex_1()
        .h_full()
        // Labels and the loop brace scrolled past the left edge stay out of the toolbar.
        .overflow_hidden()
        .cursor_pointer()
        .child(ruler)
        .on_pinch(cx.listener(on_pinch))
        .tooltip(widgets::tip("Click to place the insert marker · drag the top strip to set the loop", None))
        .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, window, cx| {
            this.focus.focus(window, cx);
            let b = this.regions.ruler.get();
            let y = f32::from(e.position.y - b.origin.y);
            let x = f32::from(e.position.x - b.origin.x) + this.s.arrange.scroll_x;
            let tick = (x / this.s.arrange.px_per_beat * PPQ as f32) as Tick;
            if y < 12. {
                let anchor = this.s.snap_tick(crate::editing::GridView::Arrange, tick, e.modifiers.alt).max(0);
                this.begin_drag(Drag::LoopBrace { anchor }, cx);
            } else {
                crate::editing::gestures::ruler_down(this, tick, e, cx);
            }
        }))
        .on_scroll_wheel(cx.listener(on_wheel));

    let toolbar = div()
        .flex_none()
        .w(px(TRACK_HEADER_W))
        .h_full()
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP1))
        .bg(pal.surface)
        .border_r_1()
        .border_b_1()
        .border_color(border())
        .child(
            div()
                .id("arr-snap")
                .h(px(24.))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Snap grid: adaptive follows the zoom · hold Alt while dragging to bypass", Some("Ctrl+4")))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::Snap, point(at.x - px(16.), at.y + px(14.)), cx);
                }))
                .child(icon(Icon::Magnet, 14., if ws.s.snap == Snap::Off { pal.faint } else { pal.accent }))
                .child(label(ws.s.snap_label(crate::editing::GridView::Arrange), TEXT_SM, pal.text)),
        )
        .child(div().flex_1())
        // Live's Automation Arm: while recording, a control moved in a
        // plugin's window writes its automation.
        .child(
            widgets::sized_icon_button("auto-arm", Icon::Automation, ws.s.automation_arm, 24., 14.)
                .when(ws.s.automation_arm, |d| d.bg(with_alpha(pal.record, 0.18)))
                .tooltip(widgets::tip("Automation Arm: record plugin moves as automation", None))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_arm(cx))),
        )
        .child(widgets::sized_icon_button("zoom-out", Icon::ZoomOut, false, 24., 14.).tooltip(widgets::tip("Zoom out", Some("Ctrl+Wheel"))).on_click(cx.listener(|this, _, _, cx| {
            let centre = f32::from(this.regions.arrange.get().size.width) / 2.;
            this.s.arrange = crate::nav::zoom_at(this.s.arrange, 0.8, centre);
            cx.notify();
        })))
        .child(widgets::sized_icon_button("zoom-in", Icon::ZoomIn, false, 24., 14.).tooltip(widgets::tip("Zoom in", Some("Ctrl+Wheel"))).on_click(cx.listener(|this, _, _, cx| {
            let centre = f32::from(this.regions.arrange.get().size.width) / 2.;
            this.s.arrange = crate::nav::zoom_at(this.s.arrange, 1.25, centre);
            cx.notify();
        })));

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(div().flex_none().h(px(RULER_H)).flex().child(toolbar).child(ruler_el))
        .child(div().flex_1().min_h(px(0.)).flex().child(header_col).child(lanes_el))
        .child(bus_section(ws, cx))
        .into_any_element()
}

/// Height of one row of the pinned bus section.
const BUS_H: f32 = 28.;

/// Return tracks and Main, pinned under the lanes so the whole signal path
/// is always in view: tracks → returns → Main. Clicking one selects it, so
/// its device chain (bus effects, mastering) opens in the Devices tab.
fn bus_section(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let buses: Vec<aitrack_model::Track> =
        ws.s.project.tracks.iter().filter(|t| matches!(t.kind, TrackKind::Return | TrackKind::Main)).cloned().collect();
    let mut headers = div().flex_none().w(px(TRACK_HEADER_W)).flex().flex_col().border_r_1().border_color(border());
    let mut lanes = div().flex_1().min_w(px(0.)).flex().flex_col();
    let mut letter = b'A';
    for t in buses {
        let id = t.id;
        let is_main = t.kind == TrackKind::Main;
        let selected = ws.s.selected_track == Some(id);
        let (mute, solo) = (t.mute, t.solo);
        let tag = if is_main {
            "Main".to_owned()
        } else {
            letter += 1;
            format!("{}", (letter - 1) as char)
        };
        let effects = t.devices.len();
        headers = headers.child(
            div()
                .id(("bus-head", id.0 as usize))
                .h(px(BUS_H))
                .flex()
                .items_center()
                .gap(px(SP2))
                .pr(px(SP2))
                .bg(if selected { pal.raised } else { pal.surface })
                .border_b_1()
                .border_color(border())
                .cursor_pointer()
                .tooltip(widgets::tip(if is_main { "Main output: every track and return ends here" } else { "Return bus: fed by the tracks' sends" }, None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.s.selected_track = Some(id);
                    cx.notify();
                }))
                .child(div().w(px(4.)).h_full().bg(track_color(t.color)))
                .child(icon(if is_main { Icon::Volume } else { Icon::Return }, 14., pal.muted))
                .child(label(paint::fit(&t.name, TRACK_HEADER_W - 118., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
                .child(chip(("bus-m", id.0 as usize), "M", mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.s.set_track(id, TrackProp::Mute(!mute));
                    cx.notify();
                })))
                .when(!is_main, |d| {
                    d.child(chip(("bus-s", id.0 as usize), "S", solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.s.set_track(id, TrackProp::Solo(!solo));
                        cx.notify();
                    })))
                })
                .child(mono(crate::state::format_db(t.volume_db), TEXT_XS, pal.muted))
                .child(widgets::meter(ws.b.engine.meter(id), 24., 6.)),
        );
        lanes = lanes.child(
            div()
                .h(px(BUS_H))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .bg(if selected { accent_alpha(0.05) } else { content(0.02) })
                .border_b_1()
                .border_color(border())
                .child(widgets::badge(tag, track_color(t.color)))
                .child(label(
                    match effects {
                        0 => "no effects".to_owned(),
                        1 => "1 effect".to_owned(),
                        n => format!("{n} effects"),
                    },
                    TEXT_XS,
                    pal.faint,
                )),
        );
    }
    div().flex_none().flex().border_t_1().border_color(border_strong()).bg(pal.surface).child(headers).child(lanes).into_any_element()
}

/// A group's lane: a tint of the group's colour and, for each track inside
/// it that has clips, a bar over the spans where it does. The bars share the
/// lane's height, stacked in track order, at most 8 px each.
fn paint_group_lane(window: &mut Window, o: Point<Pixels>, canvas_w: f32, g: &GroupPaint) {
    window.paint_quad(paint::fill_rect(at(o, 0., g.y, canvas_w, g.h - 1.), with_alpha(track_color(g.color), 0.06)));
    let n = g.members.len();
    if n == 0 {
        return;
    }
    let (top, room) = (g.y + 4., (g.h - 9.).max(2.));
    let bar = (room / n as f32).clamp(1., 8.);
    let used = bar * n as f32;
    let y0 = top + ((room - used) / 2.).max(0.);
    for (i, (color, spans)) in g.members.iter().enumerate() {
        let y = y0 + i as f32 * bar;
        if y + bar > g.y + g.h - 2. {
            break;
        }
        let fill = clip_fill(*color);
        for &(a, b) in spans {
            let (a, b) = (a.max(-2.), b.min(canvas_w + 2.));
            if b - a < 0.5 {
                continue;
            }
            let r = if bar >= 4. { 2. } else { 0. };
            window.paint_quad(paint::rrect(at(o, a, y + 0.5, (b - a).max(1.), (bar - 1.).max(1.)), r, with_alpha(fill, 0.9)));
        }
    }
}

/// One clip. `canvas_w` bounds what is worth drawing; `hot` is what the
/// pointer is over (or a drag holds) when it is this clip.
fn paint_clip(window: &mut Window, cx: &mut gpui::App, o: Point<Pixels>, c: &ClipPaint, canvas_w: f32, hot: Option<Zone>) {
    let body = at(o, c.x + 1., c.y, (c.w - 2.).max(2.), c.h);
    let fill = if c.muted { mix(clip_fill(c.color), p().well, 0.7) } else { clip_fill(c.color) };
    let fg = if c.muted { p().muted } else { clip_text(c.color) };
    window.paint_quad(paint::rrect_border(body, r_sm(), with_alpha(fill, 0.55), if c.selected { accent() } else { with_alpha(fill, 1.) }, if c.selected { 1.5 } else { 1. }));
    window.paint_quad(paint::rrect(at(o, c.x + 1., c.y, (c.w - 2.).max(2.), CLIP_HEAD), r_sm(), fill));
    if hot.is_some() {
        window.paint_quad(paint::rrect(body, r_sm(), hover()));
    }
    let glyph = if c.looping { arrange_clips::draw::loop_glyph(window, cx, o, &c.bx, fg) } else { 0. };
    if c.w > 28. {
        window.with_content_mask(Some(gpui::ContentMask { bounds: body }), |window| {
            paint::text(window, cx, &paint::fit(&c.name, c.w - 10. - glyph, TEXT_XS), o.x + px(c.x + 5.), o.y + px(c.y + 0.5), TEXT_XS, fg, FontWeight::MEDIUM, TextAlign::Left);
        });
    }
    let inner_y = c.y + CLIP_HEAD + 2.;
    let inner_h = c.h - CLIP_HEAD - 4.;
    if inner_h < 4. {
        return;
    }
    window.with_content_mask(Some(gpui::ContentMask { bounds: body }), |window| match &c.body {
        Body::Notes { notes, lo, hi, loop_len } => {
            let span = (*hi as f32 - *lo as f32 + 1.).max(8.);
            let row = (inner_h / span).clamp(1.5, 5.);
            let used = row * (*hi as f32 - *lo as f32 + 1.);
            let base = inner_y + (inner_h - used) / 2.;
            let reps = ((c.w / c.px_per_tick) / *loop_len as f32).ceil() as i64;
            for rep in 0..reps.max(1) {
                let ox = c.x + (rep * loop_len) as f32 * c.px_per_tick - c.offset as f32 * c.px_per_tick;
                if rep > 0 {
                    window.paint_quad(paint::fill_rect(at(o, ox, inner_y, 1., inner_h), with_alpha(fg, 0.15)));
                }
                for (k, s, l) in notes {
                    if *s >= *loop_len {
                        continue;
                    }
                    let nx = ox + *s as f32 * c.px_per_tick;
                    let ny = base + (*hi - *k) as f32 * row;
                    window.paint_quad(paint::fill_rect(at(o, nx, ny, (*l as f32 * c.px_per_tick - 1.).max(1.), (row - 0.5).max(1.)), with_alpha(fg, 0.85)));
                }
            }
        }
        Body::Audio { wave, shape } => {
            // The file's peaks as they will sound, then the fades over them.
            arrange_clips::draw::wave(window, o, &c.bx, shape, wave.as_ref(), canvas_w, crate::theme::mix(fg, p().waveform, 0.5));
            arrange_clips::draw::fades(window, o, &c.bx, canvas_w, hot, fg);
        }
    });
    // Where a looping clip starts over.
    crate::editing::overlay::paint_repeats(window, o, c.x, inner_y, inner_h, &c.repeats, fg);
    if hot.is_some() || c.selected {
        arrange_clips::draw::handles(window, o, &c.bx, hot);
    }
}

/// Thin rules at a track's left edge, one per group it sits in (outermost
/// first), so the tree reads at a glance as in Live.
fn group_rules(ws: &Workspace, id: TrackId) -> Vec<AnyElement> {
    let mut outer: Vec<Color> = groups::ancestors(&ws.s.project, id).filter_map(|g| ws.s.project.track(g)).map(|g| g.color).collect();
    outer.reverse();
    outer.into_iter().take(4).map(|c| div().w(px(4.)).h_full().border_l_1().border_color(with_alpha(track_color(c), 0.8)).into_any_element()).collect()
}

/// From this height a track header also shows a volume slider under its
/// buttons (drag a track's bottom edge to make it taller, as in Live).
const TALL_HEADER: f32 = 80.;
/// The track-height handle along the bottom edge of a header.
const RESIZE_H: f32 = 4.;

fn track_header(ws: &mut Workspace, r: &LaneRow, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(t) = ws.s.project.track(r.track).cloned() else { return div().into_any_element() };
    let id = t.id;
    let selected = ws.s.is_track_selected(id);
    let auto_on = ws.s.automation_shown.contains(&id);
    let has_auto = t.automation.iter().any(|l| !l.points.is_empty());
    let is_group = t.kind == TrackKind::Group;
    let audio = t.kind.carries_audio();
    let tall = r.h >= TALL_HEADER && audio;
    let (mute, solo, arm, collapsed) = (t.mute, t.solo, t.arm, t.collapsed);
    // One step per group the track is in, capped so deep trees keep their names.
    let indent = SP3 * r.depth.min(4) as f32;
    let main = div()
        .id(("arr-head", id.0 as usize))
        .relative()
        .h(px(r.h))
        .flex()
        .bg(if selected { pal.raised } else { pal.surface })
        .border_b_1()
        .border_color(border())
        .cursor_pointer()
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
            this.focus.focus(window, cx);
            let m = e.modifiers();
            this.s.ed.area = crate::editing::Area::Arrange;
            this.s.click_track(id, m.secondary(), m.shift);
            cx.notify();
        }))
        // Right-click (Live): the track joins the selection and the track menu opens.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.s.ed.area = crate::editing::Area::Arrange;
                if !this.s.is_track_selected(id) {
                    this.s.click_track(id, false, false);
                }
                this.menu = None;
                this.open_menu(MenuKind::TrackContext(id), e.position, cx);
            }),
        )
        .child(div().w(px(4.)).h_full().bg(track_color(t.color)))
        // The groups around the track: one rule in each one's colour.
        .children(group_rules(ws, id))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .pl(px(SP2 + indent))
                .pr(px(SP2))
                .when(is_group, |d| d.bg(with_alpha(track_color(t.color), 0.10)))
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(SP1))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .when(is_group, |d| {
                            d.child(
                                widgets::sized_icon_button(("fold", id.0 as usize), if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, false, 16., 12.)
                                    .tooltip(widgets::tip(if collapsed { "Unfold group" } else { "Fold group" }, None))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.s.seal();
                                        this.s.set_track(id, TrackProp::Collapsed(!collapsed));
                                        this.s.seal();
                                        // A track hidden by the fold cannot stay selected.
                                        if let Some(sel) = this.s.selected_track
                                            && sel != id
                                            && groups::within(&this.s.project, sel, id)
                                        {
                                            this.s.click_track(id, false, false);
                                        }
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(icon(if crate::state::is_drum_track(&t) { Icon::Drum } else { kind_icon(t.kind) }, 14., pal.muted))
                        .child(label(paint::fit(&t.name, TRACK_HEADER_W - 104. - indent, TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
                        .when(audio, |d| d.child(volume_box(ws, id, t.volume_db, cx))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(chip(("a-m", id.0 as usize), "M", mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.set_track(id, TrackProp::Mute(!mute));
                            cx.notify();
                        })))
                        .child(chip(("a-s", id.0 as usize), "S", solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.set_track(id, TrackProp::Solo(!solo));
                            cx.notify();
                        })))
                        .when(t.kind.has_clips(), |d| {
                            d.child(chip(("a-r", id.0 as usize), "R", arm, pal.record).on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.s.set_track(id, TrackProp::Arm(!arm));
                                cx.notify();
                            })))
                        })
                        .when(t.kind == TrackKind::Instrument, |d| {
                            let kbd = t.keyboard_input;
                            let kbd_color = if ws.piano_keyboard { pal.accent } else { pal.muted };
                            d.child(
                                icon_chip(("a-kbd", id.0 as usize), Icon::Keyboard, kbd, kbd_color)
                                    .tooltip(widgets::tip("Play this track with the PC/Mac keyboard (Z/X octave, C/V velocity)", None))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_track_keyboard(id, cx);
                                    })),
                            )
                        })
                        .child(div().flex_1())
                        .when(audio, |d| d.child(pan_knob(id, t.pan, cx)))
                        // Always there (Live): shows the track's lanes, or a
                        // Volume lane to draw in when it has none yet.
                        .child(
                            icon_chip(("a-auto", id.0 as usize), Icon::Automation, auto_on, if has_auto { pal.automation } else { pal.accent })
                                .tooltip(widgets::tip(if auto_on { "Hide automation" } else { "Show automation" }, Some("A")))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.s.toggle_automation(id);
                                    cx.notify();
                                })),
                        )
                        .child(widgets::meter(ws.b.engine.meter(id), 24., 6.)),
                )
                .when(tall, |d| d.child(volume_slider(id, t.volume_db, cx))),
        )
        // Drag the bottom edge to change the track's height.
        .child(
            div()
                .id(("arr-resize", id.0 as usize))
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(RESIZE_H))
                .cursor_ns_resize()
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let start_h = this.s.project.track(id).map_or(56., |t| t.height);
                    this.s.seal();
                    this.begin_drag(Drag::TrackHeight { track: id, start_y: f32::from(e.position.y), start_h }, cx);
                })),
        );
    let mut col = div().flex().flex_col().child(main);
    let lanes = r.autos.clone();
    for (k, target) in lanes.into_iter().enumerate() {
        col = col.child(lane_header(ws, &t, k, target, indent, cx));
    }
    col.into_any_element()
}

/// The header of one automation lane (Live): the chooser of what it
/// automates, its value, and buttons to delete its automation, hide the
/// lane and add another lane under it.
fn lane_header(ws: &mut Workspace, t: &Track, k: usize, target: AutomationTarget, indent: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let id = t.id;
    let key = (id.0 as usize) << 8 | k.min(255);
    let lane = t.automation.iter().find(|l| l.target == target && !l.points.is_empty());
    let value = lane.and_then(|l| l.value_at(ws.b.engine.transport().position)).or_else(|| current_value(&ws.s.project, t, target));
    let open = ws.auto_chooser.as_ref().is_some_and(|c| c.track == id && c.index == k);
    div()
        .id(("lane-head", key))
        .h(px(AUTO_H))
        .pl(px(SP2 + 4. + indent))
        .pr(px(SP2))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(SP1))
        .bg(content(0.02))
        .border_b_1()
        .border_color(border())
        .child(
            div()
                .id(("lane-pick", key))
                .h(px(20.))
                .px(px(SP1))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if open { hover() } else { content(0.04) })
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Choose what this lane automates", None))
                .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let at = e.position();
                    open_chooser(this, id, k, point(at.x - px(24.), at.y + px(12.)), window, cx);
                }))
                .child(icon(Icon::Automation, 12., if lane.is_some() { pal.automation } else { pal.faint }))
                .child(label(paint::fit(&target_name(ws, target), TRACK_HEADER_W - 72. - indent, TEXT_SM), TEXT_SM, pal.text).flex_1())
                .child(icon(Icon::ChevronDown, 10., pal.muted)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(mono(value.map_or("—".into(), |v| value_text(target, v)), TEXT_XS, pal.muted).flex_1())
                .when(lane.is_some(), |d| {
                    d.child(
                        widgets::sized_icon_button(("lane-del", key), Icon::Trash, false, 20., 12.)
                            .tooltip(widgets::tip("Delete this automation", None))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.s.delete_automation(id, target);
                                cx.notify();
                            })),
                    )
                })
                .child(
                    widgets::sized_icon_button(("lane-hide", key), Icon::Minus, false, 20., 12.)
                        .tooltip(widgets::tip("Hide this lane", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.hide_auto_lane(id, target);
                            cx.notify();
                        })),
                )
                .child(
                    widgets::sized_icon_button(("lane-add", key), Icon::Add, false, 20., 12.)
                        .tooltip(widgets::tip("Show another automation lane", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.add_auto_lane(id);
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
}

/// The track's volume as a value box (Live's arrangement header): drag up
/// or down, double-click for 0 dB, click to type; right-click shows its
/// automation. The same command and undo step as the mixer's fader.
fn volume_box(ws: &Workspace, id: TrackId, db: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let target = NumTarget::Volume { track: id };
    let editing = ws.num_edit.as_ref().filter(|e| e.target == target).map(|e| e.text.clone());
    div()
        .id(("arr-vol", id.0 as usize))
        .flex_none()
        .w(px(44.))
        .h(px(18.))
        .px(px(SP1))
        .flex()
        .items_center()
        .justify_end()
        .rounded(px(r_sm()))
        .bg(content(0.04))
        .border_1()
        .border_color(if editing.is_some() { accent_alpha(0.7) } else { border() })
        .cursor_ns_resize()
        .tooltip(widgets::tip("Volume — drag, double-click for 0 dB, click to type", None))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            if e.click_count >= 2 {
                this.num_edit = None;
                this.s.seal();
                this.s.set_track(id, TrackProp::VolumeDb(0.));
                this.s.seal();
                cx.notify();
                return;
            }
            let pos = crate::state::db_to_fader(this.s.project.track(id).map_or(0., |t| t.volume_db));
            this.s.seal();
            this.begin_drag(Drag::Fader { track: id, last_y: f32::from(e.position.y), pos, travel: 160. }, cx);
        }))
        // A click that didn't move opens the box for typing.
        .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
            cx.stop_propagation();
            if e.click_count() >= 2 || crate::workspace::click_dragged(e) || this.num_edit.as_ref().is_some_and(|n| n.target == target) {
                return;
            }
            this.focus.focus(window, cx);
            let db = this.s.project.track(id).map_or(0., |t| t.volume_db);
            this.begin_num_edit(target, crate::workspace::plain_text(f64::from((db * 10.).round() / 10.)), cx);
        }))
        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.open_param_menu(id, AutomationTarget::Volume, e.position, cx);
        }))
        .child(match editing {
            Some(text) => div().flex().items_center().child(mono(text, TEXT_XS, pal.accent)).child(div().w(px(1.)).h(px(10.)).ml(px(1.)).bg(pal.accent)).into_any_element(),
            None => mono(crate::state::format_db(db), TEXT_XS, pal.text).into_any_element(),
        })
        .into_any_element()
}

/// The track's pan as a small knob (drag, double-click centres it,
/// right-click shows its automation).
fn pan_knob(id: TrackId, pan: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let value = (pan + 1.) / 2.;
    widgets::knob(("arr-pan", id.0 as usize), value, true, 16., accent())
        .tooltip(widgets::tip_owned(format!("Pan {} — drag, double-click to centre", pan_text(pan))))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            if e.click_count >= 2 {
                this.s.seal();
                this.s.set_track(id, TrackProp::Pan(0.));
                this.s.seal();
                cx.notify();
                return;
            }
            this.s.seal();
            this.begin_drag(Drag::Knob { target: crate::workspace::KnobTarget::Pan { track: id }, last_y: f32::from(e.position.y), value }, cx);
        }))
        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.open_param_menu(id, AutomationTarget::Pan, e.position, cx);
        }))
        .into_any_element()
}

/// Width of the volume slider of a tall header.
const SLIDER_W: f32 = TRACK_HEADER_W - 44.;

/// A tall header's volume slider: drag sideways (fader law, 0 dB at three
/// quarters), double-click for 0 dB.
fn volume_slider(id: TrackId, db: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let pos = crate::state::db_to_fader(db);
    div()
        .id(("arr-slider", id.0 as usize))
        .h(px(12.))
        .flex()
        .items_center()
        .cursor_ew_resize()
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            if e.click_count >= 2 {
                this.s.seal();
                this.s.set_track(id, TrackProp::VolumeDb(0.));
                this.s.seal();
                cx.notify();
                return;
            }
            this.s.seal();
            this.begin_drag(Drag::HFader { track: id, last_x: f32::from(e.position.x), pos, travel: SLIDER_W }, cx);
        }))
        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            this.open_param_menu(id, AutomationTarget::Volume, e.position, cx);
        }))
        .child(widgets::hbar(pos, SLIDER_W, 4., accent_alpha(0.8)))
        .into_any_element()
}

// ───────────────────────────── Lane chooser ─────────────────────────────

/// The open chooser of an automation lane (which lane, and where it opened).
#[derive(Clone, Debug)]
pub struct Chooser {
    pub track: TrackId,
    pub index: usize,
    pub at: Point<Pixels>,
}

/// More items than this and the list asks for a narrower search (plugins
/// can have thousands of parameters).
const CHOOSER_MAX: usize = 400;

fn open_chooser(ws: &mut Workspace, track: TrackId, index: usize, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Workspace>) {
    if ws.auto_chooser.as_ref().is_some_and(|c| c.track == track && c.index == index) {
        ws.auto_chooser = None;
        cx.notify();
        return;
    }
    ws.menu = None;
    ws.auto_search.update(cx, |i, cx| i.set_text("", cx));
    ws.auto_chooser = Some(Chooser { track, index, at });
    let focus = gpui::Focusable::focus_handle(ws.auto_search.read(cx), cx);
    focus.focus(window, cx);
    cx.notify();
}

/// One section of the chooser: title, rack depth, targets with their names.
pub type ChooserGroup = (String, usize, Vec<(AutomationTarget, String)>);

/// What a lane's chooser lists: (group title, rack depth, targets with
/// their names), the mixer first and then each device of the chain, at any
/// rack depth, filtered by `query` (a group's title matching keeps all of it).
pub fn chooser_items(ws: &Workspace, track: TrackId, query: &str) -> Vec<ChooserGroup> {
    let Some(t) = ws.s.project.track(track) else { return Vec::new() };
    let q = query.trim().to_lowercase();
    let groups = automation::targets(t, |d| device_params(ws, d).into_iter().map(|p| p.0).collect());
    groups
        .into_iter()
        .filter_map(|g| {
            let title = g.device.and_then(|d| ws.s.project.device(d)).map_or("Mixer".to_owned(), |d| d.name.clone());
            let all = q.is_empty() || title.to_lowercase().contains(&q);
            let items: Vec<(AutomationTarget, String)> =
                g.targets.into_iter().map(|t| (t, short_name(ws, t))).filter(|(_, n)| all || n.to_lowercase().contains(&q)).collect();
            (!items.is_empty()).then_some((title, g.depth, items))
        })
        .collect()
}

pub fn render_chooser(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    let c = ws.auto_chooser.clone()?;
    let Some(t) = ws.s.project.track(c.track) else {
        ws.auto_chooser = None;
        return None;
    };
    let drawn: Vec<AutomationTarget> = t.automation.iter().filter(|l| !l.points.is_empty()).map(|l| l.target).collect();
    let current = ws.s.shown_lanes(c.track).get(c.index).copied();
    let query = ws.auto_search.read(cx).text().to_owned();
    let groups = chooser_items(ws, c.track, &query);
    let (track, index) = (c.track, c.index);
    let mut list = div().flex().flex_col();
    let (mut shown, mut more) = (0usize, 0usize);
    for (title, depth, items) in groups {
        if shown >= CHOOSER_MAX {
            more += items.len();
            continue;
        }
        list = list.child(menu_header(title.to_uppercase()).pl(px(SP2 + SP3 * depth.min(4) as f32)));
        for (target, name) in items {
            if shown >= CHOOSER_MAX {
                more += 1;
                continue;
            }
            shown += 1;
            list = list.child(
                menu_item(("auto-target", shown), drawn.contains(&target).then_some(Icon::Automation), name, None, Some(target) == current).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.s.set_lane_target(track, index, target);
                        this.auto_chooser = None;
                        cx.notify();
                    },
                )),
            );
        }
    }
    if shown == 0 {
        list = list.child(div().px(px(SP2)).py(px(SP2)).child(label("No parameter matches", TEXT_SM, pal.faint)));
    }
    if more > 0 {
        list = list.child(div().px(px(SP2)).py(px(SP2)).child(label(format!("{more} more — type to narrow the list"), TEXT_XS, pal.faint)));
    }
    let card = widgets::menu_card("auto-chooser", 280.)
        .child(div().px(px(SP1)).pb(px(SP1)).child(ws.auto_search.clone()))
        .child(div().id("auto-chooser-list").max_h(px(360.)).overflow_y_scroll().child(list));
    Some(
        gpui::deferred(
            gpui::anchored().position(c.at).snap_to_window_with_margin(px(SP2)).child(
                div()
                    .id("auto-chooser-layer")
                    .occlude()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.auto_chooser = None;
                        cx.notify();
                    }))
                    .child(card),
            ),
        )
        .with_priority(2)
        .into_any_element(),
    )
}

fn lane_hit(ws: &Workspace, pos: Point<Pixels>) -> Option<(LaneRow, Tick, f32, f32)> {
    let b = ws.regions.arrange.get();
    let x = f32::from(pos.x - b.origin.x) + ws.s.arrange.scroll_x;
    let y = f32::from(pos.y - b.origin.y) + ws.s.arrange.scroll_y;
    let tick = (x / ws.s.arrange.px_per_beat * PPQ as f32) as Tick;
    rows(&ws.s).into_iter().find(|r| y >= r.top && y < r.top + r.total()).map(|r| {
        let ly = y - r.top;
        (r, tick, x, ly)
    })
}

fn lane_mouse_down(ws: &mut Workspace, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Workspace>) {
    ws.focus.focus(window, cx);
    if e.first_mouse {
        // The click that brings the window forward only places the insert
        // marker, so a single click is enough and nothing is edited by it.
        if let Some((row, tick, ..)) = lane_hit(ws, e.position) {
            let at = ws.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), e.modifiers.alt);
            crate::editing::gestures::place_marker(&mut ws.s, ws.b.engine.as_mut(), at, Some(row.track));
            ws.s.auto_sel = None;
            cx.notify();
        }
        return;
    }
    let Some((row, tick, x, ly)) = lane_hit(ws, e.position) else {
        ws.s.selected_placement = None;
        cx.notify();
        return;
    };
    let track = row.track;
    ws.s.selected_track = Some(track);
    // Automation lane.
    if ly >= row.h {
        if let Some((_, target, y)) = row.lane_at(ly) {
            auto_lane_down(ws, track, target, tick, x, y, e, cx);
        }
        return;
    }
    // The insert marker, the time selection and Shift/Ctrl+click are handled
    // there; a plain click on a clip comes back here to open or drag it.
    if crate::editing::gestures::lane_down(ws, track, row.kind, tick, e, cx) {
        return;
    }
    // A plain click on a clip: the fade handles, the edges or the body.
    let Some(pid) = crate::editing::gestures::clip_at(ws, track, tick) else { return };
    let Some((_, pl, _)) = ws.s.placement(pid) else { return };
    ws.s.selected_placement = Some((track, pid));
    ws.s.select_clip(Some(pl.clip));
    arrange_clips::press(ws, &row, pid, tick, e, cx);
}

/// Left button down in an automation lane, `y` px below its top. As in
/// Live: a breakpoint is dragged (double-click deletes it), Alt + drag bends
/// a segment, the line inside the selected range drags that part up or
/// down, a click on the line (or in an empty lane) adds a breakpoint, and a
/// click off the line places the insert marker and drags out a range.
#[allow(clippy::too_many_arguments)]
fn auto_lane_down(ws: &mut Workspace, track: TrackId, target: AutomationTarget, tick: Tick, x: f32, y: f32, e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    let ppt = ws.s.arrange.px_per_beat / PPQ as f32;
    let lane = ws.s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).cloned());
    let line_y = lane.as_ref().and_then(|l| l.value_at(tick.max(0))).map(|v| (1. - v) * auto_value_px() + 6.);
    let points = lane.map(|l| l.points).unwrap_or_default();
    let value = (1. - (y - 6.) / auto_value_px()).clamp(0., 1.);
    let hit = points.iter().position(|p| (p.tick as f32 * ppt - x).abs() < 6. && ((1. - p.value) * auto_value_px() + 6. - y).abs() < 8.);
    let near = |tol: f32| line_y.is_some_and(|ly| (ly - y).abs() < tol);
    let at = ws.s.snap_tick(crate::editing::GridView::Arrange, tick.max(0), e.modifiers.alt);
    let selected = ws.s.auto_sel.filter(|r| r.track == track && r.target == target);
    // Double-click a breakpoint to delete it (as in Live).
    if let Some(i) = hit.filter(|_| e.click_count >= 2) {
        ws.s.delete_automation_point(track, target, i);
        cx.notify();
        return;
    }
    // Alt + drag between two breakpoints bends that segment.
    if hit.is_none() && e.modifiers.alt {
        if let Some(i) = points.windows(2).position(|w| tick >= w[0].tick && tick < w[1].tick) {
            let (a, b) = (points[i], points[i + 1]);
            ws.s.seal();
            ws.begin_drag(Drag::AutoCurve { track, target, index: i, start_y: f32::from(e.position.y), start_curve: a.curve, rising: b.value >= a.value }, cx);
        }
        return;
    }
    // Shift+click stretches the selected range to here.
    if e.modifiers.shift && let Some(r) = selected {
        let anchor = if at >= r.start { r.start } else { r.end };
        ws.s.set_auto_range(track, target, anchor, at);
        ws.begin_drag(Drag::AutoRange { track, target, anchor }, cx);
        return;
    }
    // The line inside the selected range: drag that part up or down.
    if hit.is_none()
        && let Some(r) = selected.filter(|r| !r.is_empty() && tick >= r.start && tick < r.end)
        && near(10.)
    {
        ws.s.seal();
        ws.begin_drag(Drag::AutoShift { range: r, original: points, start_y: f32::from(e.position.y) }, cx);
        return;
    }
    // Off the line: the insert marker goes here and dragging selects a range.
    if hit.is_none() && !points.is_empty() && !near(6.) {
        crate::editing::gestures::place_marker(&mut ws.s, ws.b.engine.as_mut(), at, Some(track));
        ws.s.set_auto_range(track, target, at, at);
        ws.begin_drag(Drag::AutoRange { track, target, anchor: at }, cx);
        return;
    }
    ws.s.auto_sel = None;
    let index = match hit {
        Some(i) => i,
        None => {
            // On the grid the lanes snap to (adaptive by default), like the drag that follows.
            let t = ws.s.snap_tick(crate::editing::GridView::Arrange, tick, false);
            ws.s.set_automation_point(track, target, t, value, false);
            let pts = ws.s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default();
            pts.iter().position(|p| p.tick == t).unwrap_or(0)
        }
    };
    let original = ws.s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default();
    ws.begin_drag(Drag::AutoPoint { track, target, original, index }, cx);
}

fn lane_right_click(ws: &mut Workspace, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    let Some((row, tick, x, ly)) = lane_hit(ws, e.position) else { return };
    if ly >= row.h {
        // Right-click a breakpoint to delete it.
        if let Some((_, target, y)) = row.lane_at(ly) {
            let ppt = ws.s.arrange.px_per_beat / PPQ as f32;
            let points = ws.s.project.track(row.track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default();
            let hit = points.iter().position(|p| (p.tick as f32 * ppt - x).abs() < 7. && ((1. - p.value) * (AUTO_H - 12.) + 6. - y).abs() < 9.);
            if let Some(i) = hit {
                ws.s.delete_automation_point(row.track, target, i);
                cx.notify();
            }
        }
        return;
    }
    // A clip or empty lane space: the context menu.
    crate::editing::gestures::context_down(ws, row.track, tick, e, cx);
}

/// Two-finger pan moves both axes at once by the trackpad's pixel deltas;
/// a mouse wheel scrolls vertically (Shift: sideways); Cmd/Ctrl + wheel
/// zooms horizontally about the pointer.
fn on_wheel(ws: &mut Workspace, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    match gesture::wheel(e) {
        Wheel::Zoom { factor } => zoom_about(ws, e.position, factor),
        Wheel::Pan { dx, dy } => {
            let total = total_height(&rows(&ws.s));
            let view_h = f32::from(ws.regions.arrange.get().size.height);
            ws.s.arrange = pan(ws.s.arrange, dx, dy, total, view_h);
        }
    }
    cx.notify();
}

/// Pinch: zoom horizontally about the pinch's centre.
fn on_pinch(ws: &mut Workspace, e: &PinchEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    zoom_about(ws, e.position, gesture::pinch_factor(e));
    cx.notify();
}

fn zoom_about(ws: &mut Workspace, at: Point<Pixels>, factor: f32) {
    let b = ws.regions.arrange.get();
    let anchor = f32::from(at.x - b.origin.x).clamp(0., f32::from(b.size.width).max(0.));
    let v = &mut ws.s.arrange;
    (v.scroll_x, v.px_per_beat) = gesture::zoom_anchored(v.scroll_x, v.px_per_beat, anchor, factor, crate::nav::MIN_PX_PER_BEAT, crate::nav::MAX_PX_PER_BEAT);
}

/// The view after a pan of `(dx, dy)` content pixels, kept inside the song
/// on the left and the lanes vertically (`total` tall in a `view_h` view).
fn pan(mut v: crate::state::ArrangeView, dx: f32, dy: f32, total: f32, view_h: f32) -> crate::state::ArrangeView {
    v.scroll_x = (v.scroll_x - dx).max(0.);
    v.scroll_y = (v.scroll_y - dy).clamp(0., (total - view_h + 48.).max(0.));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ArrangeView;

    #[test]
    fn pan_moves_both_axes_and_stays_in_bounds() {
        let v = ArrangeView { px_per_beat: 40., scroll_x: 100., scroll_y: 50. };
        let n = pan(v, 30., -20., 1000., 400.);
        assert_eq!((n.scroll_x, n.scroll_y), (70., 70.));
        let n = pan(v, 500., 500., 1000., 400.);
        assert_eq!((n.scroll_x, n.scroll_y), (0., 0.));
        // Shorter than the view: no vertical scroll at all.
        assert_eq!(pan(v, 0., -100., 100., 400.).scroll_y, 0.);
    }

    #[test]
    fn nested_groups_indent_and_fold_at_any_depth() {
        let mut s = AppState::new(Project::new("t"));
        let ids: Vec<TrackId> = (0..2).filter_map(|_| s.add_track(TrackKind::Audio)).collect();
        s.click_track(ids[0], false, false);
        let inner = s.group_tracks().unwrap();
        let outer = s.group_tracks().unwrap();
        let depth = |s: &AppState, id| rows(s).iter().find(|r| r.track == id).map(|r| r.depth);
        assert_eq!((depth(&s, outer), depth(&s, inner), depth(&s, ids[0]), depth(&s, ids[1])), (Some(0), Some(1), Some(2), Some(0)));
        s.set_track(outer, TrackProp::Collapsed(true));
        assert_eq!((depth(&s, outer), depth(&s, inner), depth(&s, ids[0])), (Some(0), None, None));
    }
}
