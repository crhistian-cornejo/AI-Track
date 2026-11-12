//! The bottom panel: a resizable, collapsible dock with four tabs (Devices,
//! Piano Roll, Step Sequencer, Mixer) and the active tab's tools.

use crate::icons::{Icon, icon};
use crate::motion;
use crate::state::{BottomTab, Snap, Tool};
use crate::theme::*;
use crate::widgets::{self, label};
use crate::workspace::{Drag, MenuKind, Workspace};
use gpui::{
    AnimationExt, AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, point, prelude::FluentBuilder, px,
};
use std::rc::Rc;

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let tab = ws.s.bottom_tab;
    let weak = cx.entity().downgrade();
    let on_tab: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |this, cx| {
            let t = BottomTab::ALL[i];
            if this.s.bottom_tab == t && this.bottom_open {
                this.bottom_open = false;
                this.release_audition(cx);
                cx.notify();
            } else {
                this.set_tab(t, cx);
            }
        });
    });
    let tab_width = if tab == BottomTab::PianoRoll && f32::from(window.viewport_size().width) < 1400. { 84. } else { 112. };
    let tabs = widgets::segmented(
        "bottom-tabs",
        &[(Some(Icon::Device), "Devices"), (Some(Icon::Piano), "Piano Roll"), (Some(Icon::Steps), "Steps"), (Some(Icon::Mixer), "Mixer")],
        tab.index(),
        tab_width,
        on_tab,
    );

    // What the panel is showing, on the right of the tab strip.
    let track = ws.s.selected_track.and_then(|t| ws.s.project.track(t)).map(|t| (t.name.clone(), t.color));
    let clip = ws.s.selected_clip.and_then(|c| ws.s.project.clips.get(&c)).map(|c| (c.name.clone(), c.color));
    let context = match tab {
        BottomTab::PianoRoll => clip.clone(),
        BottomTab::Steps => ws.s.step_clip().and_then(|(_, c)| ws.s.project.clips.get(&c)).map(|c| (c.name.clone(), c.color)),
        _ => track.clone(),
    };
    let mut bar = div()
        .flex_none()
        .h(px(TAB_BAR_H + SP2))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .child(tabs)
        .child(div().w(px(SP2)))
        .when_some(context, |d, (name, color)| {
            d.child(div().size(px(8.)).rounded_full().bg(track_color(color)))
                .child(label(crate::paint::fit(&name, 96., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
        })
        .child(div().flex_1());
    if tab == BottomTab::PianoRoll && ws.bottom_open {
        bar = bar.child(piano_tools(ws, cx));
    }
    bar = bar.child(
        widgets::icon_button("collapse-bottom", if ws.bottom_open { Icon::ChevronDown } else { Icon::ChevronUp }, false)
            .tooltip(widgets::tip(if ws.bottom_open { "Collapse panel" } else { "Expand panel" }, None))
            .on_click(cx.listener(|this, _, _, cx| {
                this.bottom_open = !this.bottom_open;
                if !this.bottom_open {
                    this.release_audition(cx);
                }
                cx.notify();
            })),
    );

    let body = if ws.bottom_open {
        let content = match tab {
            BottomTab::Devices => crate::views::devices::render(ws, window, cx),
            BottomTab::PianoRoll => crate::views::piano_roll::render(ws, window, cx),
            BottomTab::Steps => crate::views::steps::render(ws, window, cx),
            BottomTab::Mixer => crate::views::mixer::render(ws, window, cx),
        };
        Some(motion::rise_in(gpui::ElementId::Name(format!("tab-{}", ws.tab_switch).into()), div().size_full().child(content), if ws.tab_switch == 0 { 0. } else { 8. }))
    } else {
        None
    };

    let resizing = matches!(ws.drag, Some(Drag::BottomResize { .. }));
    let target = if ws.bottom_open { ws.bottom_h } else { TAB_BAR_H + SP2 };
    let handle = div()
        .id("bottom-resize")
        .absolute()
        .top(px(-SP1))
        .left_0()
        .right_0()
        .h(px(SP2))
        .cursor_row_resize()
        .when(ws.bottom_open, |d| {
            d.on_mouse_down(MouseButton::Left, cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let start_h = this.bottom_h;
                this.begin_drag(Drag::BottomResize { start_y: f32::from(e.position.y), start_h }, cx);
            }))
        });
    let panel = div()
        .relative()
        .flex_none()
        .w_full()
        .flex()
        .flex_col()
        .bg(pane_bg(crate::appearance::Pane::Bottom))
        .rounded(px(r_lg()))
        .border_1()
        .border_color(border())
        .child(bar)
        .child(div().flex_1().min_h(px(0.)).overflow_hidden().border_t_1().border_color(border()).children(body))
        .child(handle);
    if resizing {
        panel.h(px(target)).into_any_element()
    } else {
        panel.with_spring("bottom-height", motion::spring(motion::PANEL_SPRING, target), |d, h| d.h(px(h))).into_any_element()
    }
}

fn piano_tools(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let weak = cx.entity().downgrade();
    let on_tool: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |this, cx| {
            this.s.tool = if i == 0 { Tool::Select } else { Tool::Draw };
            cx.notify();
        });
    });
    let has_scale = ws.s.clip_scale(ws.s.selected_clip).is_some();
    div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(
            widgets::icon_button("pr-preview", Icon::Headphones, ws.piano_preview)
                .tooltip(widgets::tip("Audition notes when drawing or moving them", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.piano_preview = !this.piano_preview;
                    if !this.piano_preview {
                        this.release_audition(cx);
                    }
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("pr-keyboard", Icon::Keyboard, ws.piano_keyboard)
                .tooltip(widgets::tip("Computer keyboard: A W S E D F T G Y H U J K O L P", None))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.piano_keyboard = !this.piano_keyboard;
                    if !this.piano_keyboard {
                        this.release_audition(cx);
                    } else {
                        this.focus.focus(window, cx);
                    }
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("pr-octave-down", Icon::Minus, false)
                .tooltip(widgets::tip("Lower keyboard octave", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.release_audition(cx);
                    this.piano_octave = this.piano_octave.saturating_sub(1);
                    cx.notify();
                })),
        )
        .child(label(format!("C{}", ws.piano_octave), TEXT_SM, pal.text))
        .child(
            widgets::icon_button("pr-octave-up", Icon::Add, false)
                .tooltip(widgets::tip("Raise keyboard octave", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.release_audition(cx);
                    this.piano_octave = (this.piano_octave + 1).min(9);
                    cx.notify();
                })),
        )
        .child(widgets::divider_v(16.))
        .child(widgets::segmented("pr-tool", &[(Some(Icon::Cursor), ""), (Some(Icon::Pencil), "")], if ws.s.tool == Tool::Select { 0 } else { 1 }, 32., on_tool))
        .child(div().w(px(SP1)))
        .child(
            div()
                .id("pr-snap")
                .h(px(CONTROL_H))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Snap grid", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::Snap, point(at.x - px(16.), at.y - px(300.)), cx);
                }))
                .child(icon(Icon::Magnet, 14., if ws.s.snap == Snap::Off { pal.faint } else { pal.accent }))
                .child(label(ws.s.snap.label(), TEXT_SM, pal.text)),
        )
        .child(widgets::icon_button("pr-quantize", Icon::Quantize, false).tooltip(widgets::tip("Quantize to grid", None)).on_click(cx.listener(|this, _, _, cx| this.quantize_clip(cx))))
        .child(
            widgets::icon_button("pr-scale", Icon::ScaleSnap, false)
                .when(!has_scale, |d| d.opacity(0.4))
                .tooltip(widgets::tip("Snap notes to the key", None))
                .on_click(cx.listener(|this, _, _, cx| this.scale_snap_clip(cx))),
        )
        .child(widgets::icon_button("pr-tools", Icon::Wand, false).tooltip(widgets::tip("Note tools", None)).on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
            let at = e.position();
            this.open_menu(MenuKind::PianoTools, point(at.x - px(200.), at.y - px(236.)), cx);
        })))
        .child(widgets::divider_v(16.))
        .child(widgets::icon_button("pr-zoom-out", Icon::ZoomOut, false).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano.px_per_beat = (this.s.piano.px_per_beat / 1.25).max(12.);
            cx.notify();
        })))
        .child(widgets::icon_button("pr-zoom-in", Icon::ZoomIn, false).on_click(cx.listener(|this, _, _, cx| {
            this.s.piano.px_per_beat = (this.s.piano.px_per_beat * 1.25).min(320.);
            cx.notify();
        })))
        .child(widgets::divider_v(16.))
        .into_any_element()
}
