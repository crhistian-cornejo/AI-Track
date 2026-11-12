//! Device windows: a stock device's editor in a window of its own.
//!
//! The window's root view holds no project data. It renders the device
//! through the [`Workspace`] entity (the same render functions and the same
//! `Command` edits as the Device View), so both places always show the
//! project as it is: an edit in either one, an undo, a preset load all
//! re-render both. The workspace closes the window when the device leaves
//! the project ([`close_orphans`]); closing the window only forgets it.
//!
//! Instruments get their full editor (the wavetable synth's lives in
//! [`synth`]); effects show their expanded editor with room to breathe.

pub mod synth;
pub mod vault;

use crate::actions::{PlayStop, Redo, Undo};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui;
use crate::widgets::{self, label};
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, DeviceKind};
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle, WindowId, WindowOptions,
    div, prelude::FluentBuilder, px, size,
};

/// The size a device's window opens at.
pub fn window_size(plugin: &str) -> (f32, f32) {
    match plugin {
        "instrument.wavetable" => (1320., 900.),
        "instrument.vault" => (1360., 860.),
        "instrument.sampler" | "instrument.drum" => (1120., 600.),
        "fx.eq" | "fx.multiband" => (1040., 520.),
        _ => (900., 480.),
    }
}

/// Whether a device can open in a window (stock devices only; plugins
/// have their own editors and racks live in the chain).
pub fn can_open(kind: &DeviceKind) -> bool {
    matches!(kind, DeviceKind::Stock { .. })
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
    let (w, h) = window_size(plugin_id);
    let title = window_title(ws, device);
    let weak = cx.entity().downgrade();
    let entity = cx.entity();
    // Open after this update: the new window's first render reads the workspace.
    cx.defer(move |cx| {
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(640.), px(360.))),
                app_id: Some("dev.aitrack.daw".into()),
                titlebar: Some(TitlebarOptions { title: Some(title.into()), appears_transparent: false, traffic_light_position: None }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DeviceWindow::new(weak, entity.clone(), device, window, cx)),
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
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl DeviceWindow {
    fn new(ws: WeakEntity<Workspace>, entity: Entity<Workspace>, device: DeviceId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Redraw whenever the workspace does (edits, undo, meters).
        let subs = vec![
            cx.observe(&entity, |_, _, cx| cx.notify()),
            // Forget the window when it closes.
            cx.on_release(move |this: &mut Self, cx: &mut App| {
                let id = this.device;
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
        Self { ws, device, focus, _subs: subs }
    }
}

fn header(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(d) = ws.s.project.device(device).cloned() else { return div().into_any_element() };
    let track = ws.s.project.device_track(device).and_then(|t| ws.s.project.track(t)).map(|t| (t.name.clone(), t.color));
    let enabled = d.enabled;
    let ic = match &d.kind {
        DeviceKind::Stock { plugin_id } => crate::views::menus::device_icon(plugin_id, ws),
        _ => Icon::Device,
    };
    let kind = match &d.kind {
        DeviceKind::Stock { plugin_id } => ws.b.plugins.device(plugin_id).map_or("Device", |i| i.category.label()).to_owned(),
        _ => String::new(),
    };
    div()
        .flex_none()
        .h(px(44.))
        .px(px(SP4))
        .gap(px(SP2))
        .flex()
        .items_center()
        .bg(pal.panel)
        .border_b_1()
        .border_color(border())
        .child(
            widgets::icon_chip(("win-power", device.0 as usize), Icon::Power, enabled, pal.accent)
                .tooltip(widgets::tip("Device on/off", None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.s.toggle_device(device);
                    cx.notify();
                })),
        )
        .child(icon(ic, 16., pal.muted))
        .child(label(d.name.clone(), TEXT_LG, if enabled { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
        .child(label(kind, TEXT_SM, pal.faint))
        .when_some(track, |h, (name, color)| {
            h.child(div().flex().items_center().gap(px(SP1)).child(div().size(px(8.)).rounded_full().bg(track_color(color))).child(label(name, TEXT_SM, pal.muted)))
        })
        .child(div().flex_1())
        .when_some(d.preset.clone(), |h, preset| h.child(label(preset, TEXT_SM, pal.muted)))
        .child(label("Same edits as the Device View · Cmd+Z undoes either", TEXT_XS, pal.faint))
        .into_any_element()
}

fn body(ws: &mut Workspace, device: DeviceId, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(d) = ws.s.project.device(device).cloned() else { return div().into_any_element() };
    let DeviceKind::Stock { plugin_id } = &d.kind else {
        return div().p(px(SP4)).child(label("This device has no window editor.", TEXT_SM, p().muted)).into_any_element();
    };
    let bpm = ws.s.project.tempo.initial_bpm();
    let mut dev = ws.fx.dev(ws.b.engine.as_mut(), ws.b.plugins.as_ref(), &d, plugin_id, bpm);
    dev.big = true;
    if plugin_id == aitrack_stock::instrument::wavetable::ID {
        return synth::render(ws, &dev, &d, cx);
    }
    if plugin_id == aitrack_stock::instrument::vault::ID {
        return vault::render(ws, &dev, &d, cx);
    }
    // Effects and the samplers: their expanded editor, tall.
    let editor = device_ui::editor(ws, plugin_id, &dev, &d, cx);
    div()
        .id("device-window-body")
        .size_full()
        .p(px(SP3))
        .overflow_x_scroll()
        .child(div().h_full().min_h(px(300.)).rounded(px(r_lg())).bg(p().surface).border_1().border_color(border()).child(editor))
        .into_any_element()
}

impl Render for DeviceWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        let Some(ws) = self.ws.upgrade() else { return div().into_any_element() };
        let device = self.device;
        let wid: WindowId = window.window_handle().window_id();
        if ws.read(cx).s.project.device(device).is_none() {
            // The device is gone; the workspace closes the window.
            return div().size_full().bg(pal.base).into_any_element();
        }
        let title = ws.read(cx).s.project.device(device).map(|_| window_title(ws.read(cx), device)).unwrap_or_default();
        window.set_window_title(&title);
        let (content, captures, frames) = ws.update(cx, |ws, cx| {
            ws.fx.painting = Some(wid);
            let header = header(ws, device, cx);
            let body = body(ws, device, cx);
            let mut caps: Vec<AnyElement> = Vec::new();
            caps.extend(ws.drag_capture(cx));
            caps.extend(ws.num_edit_capture(cx));
            caps.extend(device_ui::capture(ws, cx));
            caps.extend(synth::capture(ws, device, cx));
            caps.extend(device_ui::overlay(ws, Some(wid), cx));
            ws.fx.painting = None;
            let playing = ws.b.engine.transport().playing;
            let frames = playing || ws.drag.is_some() || ws.fx.wants_frames() || synth::wants_frames(ws, device) || ws.audition.is_active();
            (div().size_full().flex().flex_col().child(header).child(div().flex_1().min_h(px(0.)).child(body)), caps, frames)
        });
        if frames {
            window.request_animation_frame();
        }
        let weak = self.ws.clone();
        let (w1, w2, w3, w4) = (weak.clone(), weak.clone(), weak.clone(), weak.clone());
        div()
            .id("device-window")
            .key_context("Workspace")
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
            .on_key_down(move |e, window, cx| {
                if let Some(ws) = w2.upgrade() {
                    ws.update(cx, |ws, cx| ws.num_edit_key_down(e, window, cx));
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
            .children(captures)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::Device;

    #[test]
    fn stock_devices_open_in_windows_sized_for_them() {
        assert!(can_open(&Device::stock(DeviceId(1), "fx.eq", "EQ").kind));
        let (w, h) = window_size("instrument.wavetable");
        assert!(w >= 1200. && h >= 800., "the synth gets a large window");
        let (w, h) = window_size("fx.reverb");
        assert!(w >= 640. && h >= 360.);
    }
}
