//! AI-Track: the desktop app (GPUI). It only paints and wires things up; the
//! project model, engine, plugins, library and AI live in their own crates and
//! reach the UI through `bridge`.

#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod commands;
mod editing;
mod gesture;
mod keys;
mod nav;
mod audition;
mod appearance;
mod assets;
mod bridge;
mod demo;
mod grid;
mod icons;
mod motion;
mod paint;
mod project_io;
mod real;
mod state;
mod theme;
mod usage;
mod views;
mod waveform;
mod widgets;
mod workspace;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px, size};

/// The real engine, plugins and AI; `AITRACK_STUBS=1` runs the UI on the
/// stub bridges instead (no audio device, fake meters).
fn bridges() -> bridge::Bridges {
    if std::env::var_os("AITRACK_STUBS").is_some() { bridge::Bridges::stubs() } else { real::bridges() }
}

fn main() {
    let platform = gpui_platform::current_platform(false);
    gpui::Application::with_platform(platform).with_assets(assets::Assets).run(|cx: &mut App| {
        assets::load_fonts(cx);
        actions::bind(cx);
        keys::bind(cx);
        // Quit with the main window: device windows alone do not keep the app open.
        cx.on_window_closed(|cx, _| {
            if cx.windows().iter().all(|w| w.downcast::<workspace::Workspace>().is_none()) {
                cx.quit();
            }
        })
        .detach();

        // `AITRACK_DEV_SIZE=1480x700` opens a window of that size (visual checks).
        let (w, h) = std::env::var("AITRACK_DEV_SIZE")
            .ok()
            .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))))
            .unwrap_or((1480., 920.));
        let bounds = Bounds::centered(None, size(px(w.max(1024.)), px(h.max(640.))), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(1024.), px(640.))),
                app_id: Some("dev.aitrack.daw".into()),
                // The app draws its own title bar (transport lives in it). On
                // Windows the drag areas hit-test as the caption, so snapping
                // and double-click maximise stay native.
                app_owns_titlebar_drag: cfg!(target_os = "macos"),
                titlebar: Some(TitlebarOptions {
                    title: Some("AI-Track".into()),
                    appears_transparent: cfg!(any(target_os = "macos", target_os = "windows")),
                    traffic_light_position: cfg!(target_os = "macos")
                        .then(|| point(px(14.), px((theme::TOP_BAR_H - 14.) / 2.))),
                }),
                ..Default::default()
            },
            |window, cx| {
                let ws = cx.new(|cx| workspace::Workspace::new(demo::project(), bridges(), cx));
                ws.update(cx, |ws, cx| {
                    ws.focus.focus(window, cx);
                    cx.on_blur(&ws.focus, window, |ws, _, cx| {
                        ws.release_audition(cx);
                    }).detach();
                    cx.observe_window_activation(window, |ws, window, cx| {
                        if !window.is_window_active() {
                            ws.release_audition(cx);
                        }
                    }).detach();
                    // Follow-system themes switch with the OS.
                    cx.observe_window_appearance(window, |ws, _, cx| {
                        if ws.look.app.follow_system {
                            ws.look.pending = Some(true);
                            cx.notify();
                        }
                    })
                    .detach();
                });
                window.activate_window();
                ws
            },
        )
        .expect("open the AI-Track window");
        cx.activate(true);
    });
}
