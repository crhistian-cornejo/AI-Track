//! The Bloom window: the field at full size, the DNA of the selected sound
//! beside it, the sixteen pads with their locks, and every control in
//! groups (Grow, Macros, Morph, Play). Rendered through the Workspace with
//! the same commands as the Device View card.

pub mod dna;
pub mod field;
pub mod look;
pub mod view;
pub mod visual;

use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::kit::{self, Handle, PlotCfg, ax};
use crate::views::device_ui::{Dev, instr};
use crate::views::devices::knob_control_sized;
use crate::widgets::{self, knob_cell, label, mono, text_button};
use crate::workspace::{KnobTarget, Workspace};
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene, Species};
use aitrack_stock::instrument::bloom::p;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

/// A knob of `side` px with its name and value (cards use small ones).
pub fn dial(dev: &Dev, id: u32, side: f32, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let value = dev.norm(id);
    let target = KnobTarget::Param { device: dev.id, param: id, default: info.default_normalized() };
    let k = knob_control_sized(("bloom-knob", dev.key(id as usize)), value, info.min < 0. && info.max > 0., dev.enabled, side, target, cx)
        .tooltip(widgets::tip(info.name, Some("Drag · Shift fine · double-click resets")));
    knob_cell(k, info.short, info.format(value), side + 24.).into_any_element()
}

/// A button that runs `f` on the workspace.
pub fn action(id: (&'static str, usize), text: &'static str, i: Icon, primary: bool, tip: &'static str, cx: &mut Context<Workspace>, f: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static) -> AnyElement {
    text_button(id, text, Some(i), primary)
        .tooltip(widgets::tip(tip, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            f(this, cx);
            cx.notify();
        }))
        .into_any_element()
}

/// A small square icon button.
pub fn icon_action(id: (&'static str, usize), i: Icon, enabled: bool, tip: &'static str, cx: &mut Context<Workspace>, f: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static) -> AnyElement {
    widgets::sized_icon_button(id, i, false, 24., 14.)
        .when(!enabled, |d| d.opacity(0.35))
        .tooltip(widgets::tip(tip, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            if enabled {
                f(this, cx);
                cx.notify();
            }
        }))
        .into_any_element()
}

/// Roll, Mutate and the history arrows.
pub fn grow_buttons(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    let v = ws.fx.bloom.view(dev.id);
    let (back, fwd) = (v.at > 0, v.at + 1 < v.history.len());
    let (d1, d2, d3, d4) = (dev.clone(), dev.clone(), dev.clone(), dev.clone());
    vec![
        action(("bloom-roll", dev.key(1)), "Roll", Icon::Dice, true, "Grow a new scene from the next seed (Deviance: how far it moves)", cx, move |ws, _| view::roll(ws, &d1, None)),
        action(("bloom-mutate", dev.key(2)), "Mutate", Icon::Shuffle, false, "Nudge every unlocked sound", cx, move |ws, _| view::mutate(ws, &d2)),
        icon_action(("bloom-back", dev.key(3)), Icon::ChevronLeft, back, "Previous scene", cx, move |ws, _| view::step_history(ws, &d3, -1)),
        icon_action(("bloom-fwd", dev.key(4)), Icon::ChevronRight, fwd, "Next scene", cx, move |ws, _| view::step_history(ws, &d4, 1)),
    ]
}

/// The species as chips: picking one rolls a scene of that species.
pub fn species_chips(dev: &Dev, scene: &Scene, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let mut row = div().flex().flex_wrap().items_center().gap(px(SP1));
    for s in Species::ALL {
        let on = s == scene.species;
        let d = dev.clone();
        row = row.child(
            div()
                .id(("bloom-species", dev.key(s.index())))
                .h(px(24.))
                .px(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.22) } else { content(0.05) })
                .border_1()
                .border_color(if on { accent_alpha(0.6) } else { border() })
                .hover(|st| st.bg(content(0.1)))
                .child(label(s.label(), TEXT_SM, if on { pal.accent } else { pal.text }))
                .tooltip(widgets::tip("Roll a scene of this species", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    view::roll(this, &d, Some(s));
                    cx.notify();
                })),
        );
    }
    row
}

/// The sixteen pads: colour, role, lock. Click selects and plays, the lock toggles.
fn pads(ws: &mut Workspace, dev: &Dev, scene: &Scene, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let looks = view::looks(ws, scene);
    let selected = dev.choice(p::SELECTED);
    let mut row = div().flex_none().w_full().flex().flex_wrap().gap(px(SP1));
    for (i, lk) in looks.iter().enumerate() {
        let rgb = lk.rgb;
        let e = look::encode;
        let color: gpui::Hsla = gpui::Rgba { r: e(rgb[0]), g: e(rgb[1]), b: e(rgb[2]), a: 1. }.into();
        let locked = scene.locked(i);
        let (d1, d2) = (dev.clone(), dev.clone());
        let lit = dev.m(i).clamp(0., 1.);
        row = row.child(
            div()
                .id(("bloom-pad", dev.key(0x100 | i)))
                .flex_1()
                .min_w(px(52.))
                .h(px(36.))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP1))
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(mix(content(0.04), with_alpha(color, 0.35), lit))
                .border_1()
                .border_color(if i == selected { with_alpha(color, 0.9) } else { border() })
                .hover(|st| st.bg(content(0.09)))
                .child(div().size(px(8.)).rounded_full().bg(color))
                .child(div().flex().flex_col().child(mono(format!("{:02}", i + 1), TEXT_XS, pal.faint)).child(label(scene.sounds[i].role.short(), TEXT_XS, pal.text).font_weight(FontWeight::MEDIUM)))
                .child(div().flex_1())
                .child(
                    div()
                        .id(("bloom-pad-lock", dev.key(0x200 | i)))
                        .child(icon(Icon::Lock, 10., if locked { pal.accent } else { with_alpha(pal.faint, 0.6) }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            view::toggle_lock(this, &d1, i);
                            cx.notify();
                        })),
                )
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    this.fx.bloom.view(d2.id).dna_cell = Some(i);
                    visual::audition(this, &d2, i, cx);
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.release_audition(cx))),
        );
    }
    row
}

/// The morph corners and the X/Y pad.
pub fn morph(ws: &mut Workspace, dev: &Dev, pad: f32, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let corners = view::parsed(ws, dev.id).corners.iter().map(Option::is_some).collect::<Vec<_>>();
    let mut slots = div().flex().flex_col().gap(px(SP1));
    for (k, name) in ["A", "B", "C", "D"].into_iter().enumerate() {
        let set = corners[k];
        let d = dev.clone();
        slots = slots.child(
            div()
                .id(("bloom-corner", dev.key(0x300 | k)))
                .w(px(28.))
                .h(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if set { accent_alpha(0.22) } else { content(0.05) })
                .border_1()
                .border_color(if set { accent_alpha(0.6) } else { border() })
                .child(label(name, TEXT_XS, if set { pal.accent } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
                .tooltip(widgets::tip("Store the scene in this corner · Alt-click clears it", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                    view::set_corner(this, &d, k, e.modifiers().alt);
                    cx.notify();
                })),
        );
    }
    let (x, y) = (dev.norm(p::MORPH_X), dev.norm(p::MORPH_Y));
    let reset = vec![(p::MORPH_X, 0.), (p::MORPH_Y, 0.)];
    let handle = Handle { fx: x, fy: y, x: ax(p::MORPH_X, 1.), y: ax(p::MORPH_Y, 1.), index: 0, reset, ..Default::default() };
    let any = corners.iter().any(|c| *c);
    let plot = kit::plot(ws, dev, PlotCfg { slot: 3, handles: vec![handle], keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_beat);
        kit::vline(window, r, r.px(0.5), pal.grid_beat);
        for (k, (fx, fy)) in [(0.0f32, 0.0f32), (1., 0.), (0., 1.), (1., 1.)].into_iter().enumerate() {
            let t = ["A", "B", "C", "D"][k];
            kit::txt(window, cx, t, r.px(fx * 0.8 + 0.1), r.py(fy * 0.76 + 0.06), TEXT_XS, if corners[k] { pal.accent } else { pal.faint }, gpui::TextAlign::Center);
        }
        kit::dot(window, r.pt(x, y), 6., if any { pal.accent } else { pal.faint }, Some(pal.text));
    });
    div().flex().gap(px(SP2)).child(slots).child(div().size(px(pad)).flex_none().child(plot))
}

/// Match, the next-best matches, the Drum Rack and the sample export.
pub fn tools(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let v = ws.fx.bloom.view(dev.id);
    let matching = v.matching.as_ref().map(|m| (m.fraction, m.name.clone(), m.distance, m.started.elapsed().as_secs()));
    let rendering = v.rendering.as_ref().map(|r| r.done);
    let alts = v.alternatives.clone();
    let (d1, d2, d3, d4) = (dev.clone(), dev.clone(), dev.clone(), dev.clone());
    let mut col = div().flex().flex_col().gap(px(SP2));
    let mut row = div().flex().flex_wrap().gap(px(SP1));
    row = row.child(match &matching {
        Some(_) => action(("bloom-match", dev.key(5)), "Stop", Icon::X, false, "Stop matching", cx, move |ws, _| view::cancel_match(ws, d1.id)),
        None => action(("bloom-match", dev.key(5)), "Match sample…", Icon::Waveform, false, "Grow the selected sound towards an audio file (or drop one on the field)", cx, move |ws, cx| pick_and_match(ws, &d1, cx)),
    });
    row = row.child(action(("bloom-rack", dev.key(6)), "To Drum Rack", Icon::Drum, false, "A new track with a Drum Rack: sixteen pads, each a Bloom playing one sound", cx, move |ws, cx| {
        let scene = view::scene(ws, &d2);
        if let Some((cmds, track)) = view::drum_rack_commands(&mut ws.s.project, d2.id, &scene) {
            ws.s.seal();
            if ws.s.edit("Bloom to Drum Rack", cmds) {
                ws.s.selected_track = Some(track);
                ws.toast("Added a Drum Rack of the scene's sixteen sounds", cx);
            }
            ws.s.seal();
        }
    }));
    row = row.child(match rendering {
        Some(n) => div().h(px(m().control_h)).px(px(SP2)).flex().items_center().child(mono(format!("Rendering {n}/16…"), TEXT_XS, pal.muted)).into_any_element(),
        None => action(("bloom-render", dev.key(7)), "Render samples", Icon::Download, false, "Write the sixteen sounds as WAV files and add them to the library", cx, move |ws, _| view::start_render(ws, &d3)),
    });
    col = col.child(row);
    if let Some((f, name, dist, secs)) = matching {
        col = col.child(mono(format!("Matching {name} · {:.0}% · distance {dist:.2} · {secs} s", f * 100.), TEXT_XS, pal.accent2));
    }
    if let Some((cell, list)) = alts {
        let mut r = div().flex().items_center().gap(px(SP1)).child(label("Neighbours", TEXT_XS, pal.muted));
        for (k, g) in list.into_iter().enumerate() {
            let d = d4.clone();
            r = r.child(
                div()
                    .id(("bloom-alt", dev.key(0x400 | k)))
                    .h(px(20.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(content(0.06))
                    .hover(|st| st.bg(content(0.12)))
                    .child(label(format!("{} {}", g.role.short(), k + 2), TEXT_XS, pal.text))
                    .tooltip(widgets::tip("Put this near match in the cell instead", None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let s = view::scene(this, &d);
                        let new = aitrack_stock::instrument::bloom::grow::replace(&s, cell, &g);
                        view::commit(this, &d, &new, None, "Use match neighbour");
                        cx.notify();
                    })),
            );
        }
        col = col.child(r);
    }
    col
}

/// Ask for an audio file, then match it into the selected cell.
pub fn pick_and_match(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) {
    let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Match sample".into()) });
    let d = dev.clone();
    let _ = ws;
    cx.spawn(async move |this, cx| {
        if let Ok(Ok(Some(paths))) = rx.await
            && let Some(path) = paths.into_iter().next()
        {
            let _ = this.update(cx, |ws, cx| {
                let cell = d.choice(p::SELECTED).min(SOUNDS - 1);
                view::start_match(ws, &d, cell, path);
                cx.notify();
            });
        }
    })
    .detach();
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let scene = view::shown(ws, dev);
    let cell = ws.fx.bloom.view(dev.id).dna_cell.unwrap_or_else(|| dev.choice(p::SELECTED)).min(SOUNDS - 1);
    let looks = view::looks(ws, &scene);
    let e = look::encode;
    let rgb = looks[cell].rgb;
    let cell_color: gpui::Hsla = gpui::Rgba { r: e(rgb[0]), g: e(rgb[1]), b: e(rgb[2]), a: 1. }.into();
    let visual = visual::visual(ws, dev, &scene, true, cx);
    let dna = dna::panel(ws, dev, &scene, cell, cell_color, cx);
    let cost = ws.fx.bloom.view(dev.id).cost.get();
    let mut title = vec![
        instr::tag("SEED", format!("{:06}", scene.seed), pal.accent, false),
        instr::tag("VOICES", format!("{:.0}", dev.m(82) * aitrack_stock::instrument::bloom::VOICES as f32), pal.accent2, dev.m(82) > 0.),
    ];
    title.extend(grow_buttons(ws, dev, cx).into_iter().map(|e| div().child(e)));
    let main = div()
        .flex_1()
        .min_h(px(340.))
        .flex()
        .gap(px(SP3))
        .child(instr::stage(340.).min_w(px(420.)).child(visual))
        .child(div().flex_none().w(px(320.)).h_full().child(dna));
    let groups = vec![
        instr::group("GROW", pal.accent, vec![dial(dev, p::DEVIANCE, 36., cx), tools(ws, dev, cx).into_any_element()]),
        instr::group("MACROS", series(1), [p::DECAY, p::BODY, p::TONE, p::GRIT, p::SPACE].iter().map(|&id| dial(dev, id, 36., cx)).collect()),
        instr::group("MORPH", series(2), vec![morph(ws, dev, 88., cx).into_any_element()]),
        instr::group("PLAY", pal.muted, vec![instr::chips(dev, p::MODE, cx), dial(dev, p::BASE, 30., cx), dial(dev, p::SELECTED, 30., cx), instr::chips(dev, p::CHOKE, cx), dial(dev, p::VELOCITY, 30., cx), dial(dev, p::TUNE, 30., cx), dial(dev, p::VOLUME, 30., cx)]),
    ];
    instr::root(1000.)
        .child(div().flex_none().flex().items_center().gap(px(SP3)).child(instr::title("BLOOM · GENERATIVE SCENE", title)).child(species_chips(dev, &scene, cx)))
        .child(main)
        .child(pads(ws, dev, &scene, cx))
        .child(instr::groups(groups))
        .child(instr::hint(&format!(
            "Click a cell to hear it · drag it out to grow it further from its core, in to bring it back · Alt-click locks · drop a sample on the field to match it · field {cost:.2} ms"
        )))
        .into_any_element()
}
