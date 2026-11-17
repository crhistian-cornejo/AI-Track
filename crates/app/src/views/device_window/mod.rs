//! Device windows: a stock device's editor in a window of its own, framed
//! like a plugin window.
//!
//! The frame is shared by every device ([`frame`]): a header with the
//! device, its track, bypass, the preset selector, A/B and reset
//! ([`chrome`]); level meters for the input, the gain reduction and the
//! output ([`meters`]); a footer with latency, DSP load and warnings
//! ([`status`]). The window opens at the size its device needs, can be
//! resized, and remembers its size and place per plugin ([`placement`]).
//!
//! The editor inside gets the [`Layout::Window`](device_ui::Layout)
//! arrangement: it takes the room the window gives and scrolls only when the
//! window is smaller than it needs. The window's root view holds no project
//! data. It renders the device through the [`Workspace`] entity (the same
//! render functions and the same `Command` edits as the Device View), so both
//! places always show the project as it is: an edit in either one, an undo, a
//! preset load all re-render both. The workspace closes the window when the
//! device leaves the project ([`close_orphans`]); closing the window only
//! forgets it.
//!
//! Instruments get their full editor (the wavetable synth's lives in
//! [`synth`]); effects show their expanded editor with room to breathe.

pub mod chrome;
pub mod io;
pub mod meters;
pub mod placement;
pub mod presets;
pub mod status;
pub mod synth;
pub mod vault;

use crate::actions::{PlayStop, Redo, Undo};
use crate::theme::*;
use crate::views::device_ui::{self, Layout, kit};
use crate::widgets::label;
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, DeviceKind};
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement,
    Styled, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle, WindowId, WindowOptions, div, point, prelude::FluentBuilder, px, size,
};
use placement::{Placement, Saved};
use std::time::{Duration, Instant};

/// Whether a device can open in a window (stock devices only; plugins
/// have their own editors and racks live in the chain).
pub fn can_open(kind: &DeviceKind) -> bool {
    matches!(kind, DeviceKind::Stock { .. })
}

/// `AITRACK_DEV_WINSIZE=WxH`: open every device window at exactly that size
/// (screenshots at the smallest and largest sizes).
fn dev_size() -> Option<(f32, f32)> {
    let v = std::env::var("AITRACK_DEV_WINSIZE").ok()?;
    let (w, h) = v.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The displays' rectangles, in the space window bounds use.
fn screens(cx: &App) -> Vec<Placement> {
    cx.displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            Placement { x: f32::from(b.origin.x), y: f32::from(b.origin.y), w: f32::from(b.size.width), h: f32::from(b.size.height) }
        })
        .collect()
}

/// Open the window of `device`, or bring it to the front.
pub fn open(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) {
    if let Some(handle) = ws.device_windows.get(&device).copied() {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
        ws.device_windows.remove(&device);
    }
    let Some(d) = ws.s.project.device(device) else { return };
    let DeviceKind::Stock { plugin_id } = &d.kind else { return };
    let plugin = plugin_id.clone();
    let title = window_title(ws, device);
    let weak = cx.entity().downgrade();
    let entity = cx.entity();
    // Open after this update: the new window's first render reads the workspace.
    cx.defer(move |cx| {
        let displays = screens(cx);
        let centre = cx.primary_display().map_or((800., 500.), |d| {
            let b = d.bounds();
            (f32::from(b.origin.x + b.size.width / 2.), f32::from(b.origin.y + b.size.height / 2.))
        });
        let mut at = placement::resolve(&Saved::load(&Saved::default_file()), &plugin, &displays, centre);
        let mut min = placement::min_size(&plugin);
        if let Some((w, h)) = dev_size() {
            // Development: open at exactly this size, whatever the editor needs.
            (at.w, at.h, min) = (w, h, (240., 200.));
        }
        let bounds = Bounds::new(point(px(at.x), px(at.y)), size(px(at.w), px(at.h)));
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(min.0), px(min.1))),
                app_id: Some("dev.aitrack.daw".into()),
                // Not GPUI's 30 Hz cap for unfocused windows (20 fps on a 60 Hz display): the
                // meters should move as smoothly as in the main window. Low-power mode throttles.
                inactive_frame_interval: None,
                titlebar: Some(TitlebarOptions { title: Some(title.into()), appears_transparent: false, traffic_light_position: None }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DeviceWindow::new(weak, entity.clone(), device, plugin, window, cx)),
        );
        if let Ok(handle) = opened {
            entity.update(cx, |ws, _| {
                ws.device_windows.insert(device, handle);
            });
        }
    });
}

/// `Device — Track`, the window's title.
fn window_title(ws: &Workspace, device: DeviceId) -> String {
    let name = ws.s.project.device(device).map_or_else(|| "Device".to_owned(), |d| d.name.clone());
    match ws.s.project.device_track(device).and_then(|t| ws.s.project.track(t)) {
        Some(t) => format!("{name} — {}", t.name),
        None => name,
    }
}

/// Close the windows of devices that left the project (deleted, or gone
/// with an undo) and forget windows the user closed.
pub fn close_orphans(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if ws.device_windows.is_empty() {
        return;
    }
    let dead: Vec<(DeviceId, WindowHandle<DeviceWindow>)> =
        ws.device_windows.iter().filter(|(id, _)| ws.s.project.device(**id).is_none()).map(|(id, h)| (*id, *h)).collect();
    for (id, handle) in dead {
        ws.device_windows.remove(&id);
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        });
    }
}

/// Whether `device` has a window open.
pub fn is_open(ws: &Workspace, device: DeviceId) -> bool {
    ws.device_windows.contains_key(&device)
}


/// The window of one device.
pub struct DeviceWindow {
    ws: WeakEntity<Workspace>,
    device: DeviceId,
    /// The stock plugin it shows (the key its size is remembered under).
    plugin: String,
    focus: FocusHandle,
    /// Where the window was last seen and the tick of the pending save.
    seen: Option<Placement>,
    /// Development helper: frame states to put the window in on its first
    /// frame, from `AITRACK_DEV_WINDOW` (`presets`, `save`, `next`, `tweak`, `ab`, `bypass`).
    dev_words: Vec<String>,
    save_seq: u64,
    /// Drawer growth: the height of the card while no drawer is open, and how
    /// much the window has grown for the open drawer.
    closed_h: f32,
    grown: f32,
    /// A resize asked for and not yet seen in the window's size: its height and the frames it may still wait.
    pending: Option<(f32, u8)>,
    _subs: Vec<Subscription>,
}

/// How long after the last move or resize the placement is written.
const SAVE_AFTER: Duration = Duration::from_millis(600);

impl DeviceWindow {
    fn new(ws: WeakEntity<Workspace>, entity: Entity<Workspace>, device: DeviceId, plugin: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Redraw whenever the workspace does (edits, undo, meters).
        let subs = vec![
            cx.observe(&entity, |_, _, cx| cx.notify()),
            // Forget the window when it closes.
            cx.on_release(move |this: &mut Self, cx: &mut App| {
                let id = this.device;
                this.remember();
                if let Some(ws) = this.ws.upgrade() {
                    ws.update(cx, |ws, _| {
                        ws.device_windows.remove(&id);
                        ws.fx.synth.entry(id).and_modify(|v| v.drag = None);
                    });
                }
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let dev_words = std::env::var("AITRACK_DEV_WINDOW").map(|v| v.split(',').map(str::to_owned).collect()).unwrap_or_default();
        Self { ws, device, plugin, focus, seen: None, dev_words, save_seq: 0, closed_h: 0., grown: 0., pending: None, _subs: subs }
    }

    /// Make the window as tall as the open drawer needs: grow by its height when
    /// it opens (as far as the screen allows), give it back when it closes. With
    /// no drawer open, remember how tall the card is.
    fn follow_drawer(&mut self, drawer: f32, window: &mut Window, cx: &mut Context<Self>) {
        let content = window.viewport_size();
        let seen = f32::from(content.height);
        // A resize takes a frame or two to show: work from the height asked for, not the stale one.
        let current = match self.pending {
            Some((h, left)) if (seen - h).abs() > 0.5 && left > 0 => {
                self.pending = Some((h, left - 1));
                h
            }
            _ => {
                self.pending = None;
                seen
            }
        };
        let chrome = placement::HEADER_H + placement::FOOTER_H + placement::BODY_PAD;
        if drawer <= 0. && self.grown <= 0. {
            self.closed_h = current - chrome;
            return;
        }
        let cap = window.display(cx).map_or(f32::MAX, |d| {
            let screen = d.bounds();
            let top = f32::from(window.bounds().origin.y - screen.origin.y);
            placement::room_below(f32::from(screen.size.height), top)
        });
        let (height, grown) = placement::follow_drawer(current, self.grown, drawer, cap, placement::min_size(&self.plugin).1);
        self.grown = grown;
        if (height - current).abs() > 0.5 {
            self.pending = Some((height, 12));
            window.resize(size(content.width, px(height)));
        }
    }

    /// Write the window's placement under its plugin.
    fn remember(&self) {
        let Some(at) = self.seen else { return };
        let file = Saved::default_file();
        let mut saved = Saved::load(&file);
        if saved.remember(&self.plugin, at) {
            saved.save(&file);
        }
    }

    /// Notice a move or resize and save it a moment after the last one.
    fn watch_bounds(&mut self, window: &Window, cx: &mut Context<Self>) {
        // The frame's corner but the content's size: `Window::bounds` counts the
        // title bar, and a window opens at the size of its content, so saving
        // the frame would make it taller by a title bar every time it reopens.
        let (b, content) = (window.bounds(), window.viewport_size());
        let now = Placement { x: f32::from(b.origin.x), y: f32::from(b.origin.y), w: f32::from(content.width), h: f32::from(content.height) };
        let first = self.seen.is_none();
        if self.seen == Some(now) {
            return;
        }
        self.seen = Some(now);
        if first {
            // Opening is not a change: only what the user does is remembered.
            return;
        }
        self.save_seq += 1;
        let seq = self.save_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |this, _| {
                if this.save_seq == seq {
                    this.remember();
                }
            });
        })
        .detach();
    }
}

/// The frame around a device's editor, built inside the workspace's update:
/// the middle row (input rail, editor, reduction and output rails) and the footer.
///
/// `closed_h` is the card's height when no drawer is open (0 = not known yet);
/// the third value is the height the editor's open drawer takes (0 when closed).
fn frame(ws: &mut Workspace, device: DeviceId, closed_h: f32, cx: &mut Context<Workspace>) -> (AnyElement, AnyElement, f32) {
    let pal = p();
    let Some(d) = ws.s.project.device(device).cloned() else { return (div().into_any_element(), div().into_any_element(), 0.) };
    let DeviceKind::Stock { plugin_id } = &d.kind else {
        let note = div().p(px(SP4)).child(label("This device has no window editor.", TEXT_SM, pal.muted)).into_any_element();
        return (note, div().into_any_element(), 0.);
    };
    let plugin = plugin_id.clone();
    let bpm = ws.s.project.tempo.initial_bpm();
    let mut dev = ws.fx.dev(ws.b.engine.as_mut(), ws.b.plugins.as_ref(), &d, &plugin, bpm);
    dev.layout = Layout::Window;
    dev.big = dev.layout.is_big();
    let levels = meters::gather(ws, &dev, &plugin);
    let now = Instant::now();
    let (editor, drawer) = editor_area(ws, &dev, &d, &plugin, closed_h, cx);
    let notes = status::notes(ws, &dev, &plugin, &levels.reading);
    let left = meters::left(ws, device, &levels, now);
    let right = meters::right(ws, device, &levels, now);
    let footer = status::footer(ws, &dev, &notes);
    let middle = div().flex_1().min_h(px(0.)).flex().children(left).child(editor).children(right).into_any_element();
    (middle, footer, drawer)
}

/// The editor itself, in the room between the rails.
fn editor_area(ws: &mut Workspace, dev: &device_ui::Dev, d: &aitrack_model::Device, plugin: &str, closed_h: f32, cx: &mut Context<Workspace>) -> (AnyElement, f32) {
    if plugin == aitrack_stock::instrument::wavetable::ID {
        return (div().flex_1().min_w(px(0.)).min_h(px(0.)).overflow_hidden().child(synth::render(ws, dev, d, cx)).into_any_element(), 0.);
    }
    if plugin == aitrack_stock::instrument::vault::ID {
        return (div().flex_1().min_w(px(0.)).min_h(px(0.)).overflow_hidden().child(vault::render(ws, dev, d, cx)).into_any_element(), 0.);
    }
    // Effects and the samplers: their expanded editor, given the window's room.
    let scope = kit::WindowScope::enter();
    let editor = device_ui::editor(ws, plugin, dev, d, cx);
    // What the editor needs: the least it said while building, and the table's
    // figure for the ones that cannot say (fixed-size editors).
    let table = if dev_size().is_some() { (0., 0.) } else { placement::editor_extent(plugin) };
    let drawer = scope.drawer_height();
    // With a drawer open the card keeps the height it had closed plus the drawer's:
    // when the screen cannot give the window that much, the body scrolls.
    let with_drawer = if drawer > 0. { closed_h + drawer } else { 0. };
    let (needs_w, needs_h) = (scope.min_width().max(table.0), scope.min_height().max(table.1).max(kit::WINDOW_PANEL_MIN_H).max(with_drawer));
    drop(scope);
    let area = div()
        .id("device-window-body")
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .p(px(SP3))
        .child(
            div().id("device-window-scroll").size_full().overflow_scroll().child(
                div()
                    .w_full()
                    .min_w(px(needs_w))
                    .h_full()
                    .min_h(px(needs_h))
                    .flex()
                    .justify_center()
                    .rounded(px(r_lg()))
                    .bg(p().surface)
                    .border_1()
                    .border_color(border())
                    .when(!dev.enabled, |d| d.opacity(0.55))
                    .child(editor),
            ),
        )
        .into_any_element();
    (area, drawer)
}

/// Put a window's frame in the states `AITRACK_DEV_WINDOW` names, so they can be
/// looked at without a mouse (screenshots).
fn dev_words(ws: &mut Workspace, device: DeviceId, wid: WindowId, words: &[String], window: &mut Window, cx: &mut Context<Workspace>) {
    ws.fx.surface = Some(wid);
    for word in words {
        match word.as_str() {
            "next" => chrome::step(ws, device, 1, cx),
            "tweak" => {
                if let Some(plugin) = meters::plugin_of(ws, device) {
                    let set = ws.fx.params(ws.b.plugins.as_ref(), &plugin);
                    if let Some(info) = set.list.first() {
                        device_ui::set_now(ws, device, info.id, 0.9);
                    }
                }
            }
            "ab" => chrome::ab_switch(ws, device, 1, cx),
            "bypass" => {
                ws.s.toggle_device(device);
            }
            "presets" => ws.fx.open_pop(device_ui::PopKind::Presets(device), point(px(320.), px(76.))),
            "save" => chrome::open_save(ws, device, point(px(600.), px(76.)), window, cx),
            _ => {}
        }
    }
}

impl Render for DeviceWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        let Some(ws) = self.ws.upgrade() else { return div().into_any_element() };
        let device = self.device;
        let wid: WindowId = window.window_handle().window_id();
        crate::fps::begin(window, || format!("device {}", meters::plugin_of(ws.read(cx), device).unwrap_or_default()));
        if ws.read(cx).s.project.device(device).is_none() {
            // The device is gone; the workspace closes the window.
            return div().size_full().bg(pal.base).into_any_element();
        }
        self.watch_bounds(window, cx);
        if !self.dev_words.is_empty() {
            let words = std::mem::take(&mut self.dev_words);
            ws.update(cx, |ws, cx| dev_words(ws, device, wid, &words, window, cx));
        }
        let title = ws.read(cx).s.project.device(device).map(|_| window_title(ws.read(cx), device)).unwrap_or_default();
        window.set_window_title(&title);
        let closed_h = self.closed_h;
        let (content, captures, frames, num_editing, drawer) = ws.update(cx, |ws, cx| {
            ws.fx.painting = Some(wid);
            let header = chrome::header(ws, device, cx);
            let (middle, footer, drawer) = frame(ws, device, closed_h, cx);
            let mut caps: Vec<AnyElement> = Vec::new();
            caps.extend(ws.drag_capture(cx));
            caps.extend(ws.num_edit_capture(cx));
            caps.extend(device_ui::capture(ws, cx));
            caps.extend(synth::capture(ws, device, cx));
            caps.extend(device_ui::overlay(ws, Some(wid), cx));
            ws.fx.painting = None;
            let playing = ws.b.engine.transport().playing;
            let tick_moving = ws.fx.chrome.get(&device).is_some_and(chrome::ChromeState::animating);
            let raw_frames =
                playing || tick_moving || ws.drag.is_some() || ws.fx.wants_frames() || synth::wants_frames(ws, device) || ws.audition.is_active();
            let throttle_ok = !ws.look.app.low_power || ws.drag.is_some() || ws.last_frame.elapsed() >= Duration::from_millis(50);
            let frames = raw_frames && throttle_ok;
            let num_editing = ws.num_edit.is_some();
            (div().size_full().flex().flex_col().child(header).child(middle).child(footer), caps, frames, num_editing, drawer)
        });
        if frames {
            window.request_animation_frame();
        }
        self.follow_drawer(drawer, window, cx);
        let weak = self.ws.clone();
        let (w1, w2, w3, w4, w5, w6) = (weak.clone(), weak.clone(), weak.clone(), weak.clone(), weak.clone(), weak.clone());
        crate::fps::probe(div()
            .id("device-window")
            .key_context(crate::workspace::workspace_key_context(num_editing))
            .track_focus(&self.focus)
            .size_full()
            .bg(pal.base)
            .font_family(font_ui())
            .text_color(pal.text)
            // Mark this window as the one being clicked, before the controls
            // see the event (popovers open where they were asked for).
            .capture_any_mouse_down(move |_, _, cx| {
                if let Some(ws) = w1.upgrade() {
                    ws.update(cx, |ws, _| ws.fx.surface = Some(wid));
                }
            })
            .capture_key_down(move |e, window, cx| {
                if let Some(ws) = w2.upgrade() {
                    ws.update(cx, |ws, cx| ws.num_edit_key_down(e, window, cx));
                }
            })
            .on_key_down(move |e, _, cx| {
                if let Some(ws) = w5.upgrade() {
                    ws.update(cx, |ws, cx| ws.handle_keyboard_midi_down(e, cx));
                }
            })
            .on_key_up(move |e, window, cx| {
                if let Some(ws) = w6.upgrade() {
                    ws.update(cx, |ws, cx| ws.piano_key_up(e, window, cx));
                }
            })
            .on_action(move |a: &Undo, window, cx| {
                if let Some(ws) = w3.upgrade() {
                    ws.update(cx, |ws, cx| ws.undo(a, window, cx));
                }
            })
            .on_action(move |a: &Redo, window, cx| {
                if let Some(ws) = w4.upgrade() {
                    ws.update(cx, |ws, cx| ws.redo(a, window, cx));
                }
            })
            .on_action(move |a: &PlayStop, window, cx| {
                if let Some(ws) = weak.upgrade() {
                    ws.update(cx, |ws, cx| ws.play_stop(a, window, cx));
                }
            })
            .child(content)
            .children(captures), window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::Device;

    #[test]
    fn stock_devices_open_in_windows_sized_for_them() {
        assert!(can_open(&Device::stock(DeviceId(1), "fx.eq", "EQ").kind));
        let (w, h) = placement::default_size("instrument.wavetable");
        assert!(w >= 1200. && h >= 800., "the synth gets a large window");
        let (w, h) = placement::default_size("fx.reverb");
        assert!(w >= 640. && h >= 360.);
    }
}
