//! The Bloom window's layout: a top bar (title, seed, species, the material
//! picker), the generator on the left (Roll and Mutate, the history,
//! Deviance, the selected sound's envelope, the macros, the export chip),
//! the stage on the right (the matter and the sixteen swatches), and tabs
//! under them (the DNA, Morph, Play, Tools).

use super::matter::{self, Material};
use super::{dial, dna, icon_action, morph, species_chips, stage, tools, view};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::{Dev, caption, instr};
use crate::widgets::{self, label, mono};
use crate::workspace::{DraggedSample, Workspace};
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene, g};
use aitrack_stock::instrument::bloom::p;
use gpui::{AnyElement, AppContext, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, div, point, px};

/// The material picker: previous, the name (a menu of every material), next.
/// A material is a look only; picking one is one undo step.
pub fn material_picker(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let now = view::parsed(ws, dev.id).material;
    let open = ws.fx.bloom.view(dev.id).stage.menu;
    let (d1, d2, d3) = (dev.clone(), dev.clone(), dev.clone());
    let mut name = div()
        .id(("bloom-material", dev.key(0x600)))
        .relative()
        .h(px(24.))
        .min_w(px(148.))
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP2))
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if open { content(0.1) } else { content(0.05) })
        .border_1()
        .border_color(border())
        .hover(|s| s.bg(content(0.1)))
        .child(label("MATTER", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(label(now.name(), TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM))
        .child(div().flex_1())
        .child(icon(Icon::ChevronDown, 12., pal.muted))
        .tooltip(widgets::tip("The material of the matter", Some("A look only: the sound does not change")))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            let st = &mut this.fx.bloom.view(d1.id).stage;
            st.menu = !st.menu;
            cx.notify();
        }));
    if open {
        let mut list = div().w(px(160.)).p(px(SP1)).flex().flex_col().gap(px(SP1)).rounded(px(r_md())).bg(pal.overlay).border_1().border_color(border()).shadow_lg();
        for m in Material::ALL {
            let d = dev.clone();
            let on = m == now;
            list = list.child(
                div()
                    .id(("bloom-material-item", dev.key(0x610 | m.index())))
                    .h(px(24.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .when_on(on)
                    .hover(|s| s.bg(content(0.08)))
                    .child(label(m.name(), TEXT_SM, if on { pal.accent } else { pal.text }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.fx.bloom.view(d.id).stage.menu = false;
                        view::set_material(this, &d, m);
                        cx.notify();
                    })),
            );
        }
        let d = dev.clone();
        name = name.child(
            gpui::deferred(
                gpui::anchored().offset(point(px(0.), px(28.))).snap_to_window_with_margin(px(SP2)).child(
                    div()
                        .id(("bloom-material-menu", dev.key(0x620)))
                        .occlude()
                        .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                            this.fx.bloom.view(d.id).stage.menu = false;
                            cx.notify();
                        }))
                        .child(list),
                ),
            )
            .with_priority(2),
        );
    }
    div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(icon_action(("bloom-material-prev", dev.key(0x601)), Icon::ChevronLeft, true, "Previous material", cx, move |ws, _| {
            let m = view::parsed(ws, d2.id).material.step(-1);
            view::set_material(ws, &d2, m);
        }))
        .child(name)
        .child(icon_action(("bloom-material-next", dev.key(0x602)), Icon::ChevronRight, true, "Next material", cx, move |ws, _| {
            let m = view::parsed(ws, d3.id).material.step(1);
            view::set_material(ws, &d3, m);
        }))
        .into_any_element()
}

/// A lit row of a menu.
trait WhenOn {
    fn when_on(self, on: bool) -> Self;
}

impl WhenOn for gpui::Stateful<Div> {
    fn when_on(self, on: bool) -> Self {
        if on { self.bg(accent_alpha(0.18)) } else { self }
    }
}

/// A round button (Roll, Mutate).
fn round(id: (&'static str, usize), side: f32, primary: bool) -> gpui::Stateful<Div> {
    let pal = p();
    let hover = if primary { mix(pal.accent, pal.text, 0.12) } else { content(0.12) };
    div()
        .id(id)
        .size(px(side))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SP1))
        .rounded_full()
        .cursor_pointer()
        .bg(if primary { pal.accent } else { content(0.06) })
        .border_1()
        .border_color(if primary { accent_alpha(0.8) } else { border() })
        .shadow_sm()
        .hover(move |s| s.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Roll as a big round button, Mutate as a smaller one, the history arrows, Deviance.
fn grow_cluster(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let v = ws.fx.bloom.view(dev.id);
    let (back, fwd) = (v.at > 0, v.at + 1 < v.history.len());
    let (d1, d2, d3, d4) = (dev.clone(), dev.clone(), dev.clone(), dev.clone());
    let roll = round(("bloom-roll-big", dev.key(0x630)), 72., true)
        .child(icon(Icon::Dice, 24., pal.on_accent))
        .child(label("ROLL", TEXT_XS, pal.on_accent).font_weight(FontWeight::BOLD))
        .tooltip(widgets::tip("Grow a new scene from the next seed", Some("Deviance sets how far it moves")))
        .on_click(cx.listener(move |this, _, _, cx| {
            view::roll(this, &d1, None);
            cx.notify();
        }));
    let mutate = round(("bloom-mutate-big", dev.key(0x631)), 48., false)
        .child(icon(Icon::Shuffle, 16., pal.text))
        .tooltip(widgets::tip("Mutate: nudge every unlocked sound", None))
        .on_click(cx.listener(move |this, _, _, cx| {
            view::mutate(this, &d2);
            cx.notify();
        }));
    let history = div()
        .flex()
        .gap(px(SP1))
        .child(icon_action(("bloom-back", dev.key(3)), Icon::ChevronLeft, back, "Previous scene", cx, move |ws, _| view::step_history(ws, &d3, -1)))
        .child(icon_action(("bloom-fwd", dev.key(4)), Icon::ChevronRight, fwd, "Next scene", cx, move |ws, _| view::step_history(ws, &d4, 1)));
    div()
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(roll)
        .child(div().flex().flex_col().items_center().gap(px(SP2)).child(mutate).child(history))
        .child(dial(dev, p::DEVIANCE, 40., cx))
}

/// The points of an attack–hold–decay envelope (decay = time to −40 dB),
/// `n + 1` of them over `total` ms, levels 0..1.
pub fn envelope_points(attack: f32, hold: f32, decay: f32, total: f32, n: usize) -> Vec<(f32, f32)> {
    let (a, d) = (attack.max(0.1), decay.max(1.0));
    (0..=n)
        .map(|k| {
            let t = k as f32 / n as f32 * total;
            let v = if t < a {
                t / a
            } else if t < a + hold {
                1.0
            } else {
                (-4.6 * (t - a - hold) / d).exp()
            };
            (t, v)
        })
        .collect()
}

/// The selected sound's amplitude envelope (the resonator's ring counts when
/// it outlasts the body), drawn in the sound's colour.
fn envelope(scene: &Scene, cell: usize, color: gpui::Hsla) -> AnyElement {
    let pal = p();
    let genome = &scene.sounds[cell.min(SOUNDS - 1)];
    let gn = genome.folded();
    let (a, h) = (gn.plain(g::ATTACK), gn.plain(g::HOLD).max(0.0));
    let ring = if gn.plain(g::RES_MIX) > 0.2 { gn.plain(g::RES_DECAY) * 0.8 } else { 0.0 };
    let decay = gn.plain(g::DECAY).max(ring).max(1.0);
    let total = (a + h + decay * 1.15).max(10.0);
    let pts = envelope_points(a, h, decay, total, 96);
    let readout = format!("{} · A {a:.0} ms · D {decay:.0} ms", genome.role.label());
    let base = pal.grid_beat;
    let plot = gpui::canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let (x0, y0, w, hh) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
            let y = |v: f32| y0 + hh - SP1 - v * (hh - SP2);
            crate::paint::polyline(window, &[point(px(x0), px(y(0.))), point(px(x0 + w), px(y(0.)))], 1., base);
            let line: Vec<_> = pts.iter().map(|(t, v)| point(px(x0 + t / total * w), px(y(*v)))).collect();
            crate::paint::polyline(window, &line, 1.5, color);
        },
    )
    .size_full();
    div()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(div().flex().items_center().gap(px(SP2)).child(div().size(px(8.)).rounded_full().bg(color)).child(mono(readout, TEXT_XS, pal.muted)))
        .child(div().h(px(48.)).w_full().rounded(px(r_sm())).bg(pal.well).border_1().border_color(border()).child(plot))
        .into_any_element()
}

/// The export chip: renders the selected sound to a WAV, then drags onto a
/// track or a pad as a sample.
fn export_chip(ws: &mut Workspace, dev: &Dev, scene: &Scene, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let cell = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let genome = scene.sounds[cell].hash();
    let v = ws.fx.bloom.view(dev.id);
    let busy = v.export.is_some();
    let ready = v.exported.clone().filter(|e| e.genome == genome);
    let chip = div()
        .id(("bloom-export", dev.key(0x640)))
        .h(px(28.))
        .px(px(SP3))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP2))
        .rounded_full()
        .cursor_pointer()
        .bg(content(0.05))
        .border_1()
        .border_color(border())
        .hover(|s| s.bg(content(0.1)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    match (busy, ready) {
        (true, _) => chip.child(mono("Rendering…", TEXT_XS, pal.muted)).into_any_element(),
        (false, Some(e)) => chip
            .border_color(accent_alpha(0.6))
            .child(icon(Icon::Waveform, 12., pal.accent))
            .child(label("Drag the sound", TEXT_SM, pal.text))
            .tooltip(widgets::tip("Drag onto a track or a pad as a sample", None))
            .on_drag(DraggedSample(e.item), |d: &DraggedSample, _, _, cx| cx.new(|_| d.clone()))
            .into_any_element(),
        (false, None) => {
            let d = dev.clone();
            chip.child(icon(Icon::Download, 12., pal.muted))
                .child(label("Export sound", TEXT_SM, pal.text))
                .tooltip(widgets::tip("Render the selected sound to a WAV", Some("Then drag it onto a track or a pad")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    view::start_export(this, &d);
                    cx.notify();
                }))
                .into_any_element()
        }
    }
}

/// The tabs under the stage.
pub const TABS: [&str; 4] = ["Sound", "Morph", "Play", "Tools"];

fn tab_strip(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let now = ws.fx.bloom.view(dev.id).stage.tab;
    let mut row = div().flex().items_center().gap(px(SP1));
    for (k, name) in TABS.iter().enumerate() {
        let d = dev.clone();
        let on = k == now;
        row = row.child(
            div()
                .id(("bloom-tab", dev.key(0x650 | k)))
                .h(px(24.))
                .px(px(SP3))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { content(0.1) } else { content(0.0) })
                .hover(|s| s.bg(content(0.08)))
                .child(label(*name, TEXT_SM, if on { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fx.bloom.view(d.id).stage.tab = k;
                    cx.notify();
                })),
        );
    }
    row
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let scene = view::shown(ws, dev);
    let cell = ws.fx.bloom.view(dev.id).dna_cell.unwrap_or_else(|| dev.choice(p::SELECTED)).min(SOUNDS - 1);
    let looks = view::looks(ws, &scene, view::Shape::of(dev));
    let cell_color = stage::colour(&looks[cell]);
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let sel_color = stage::colour(&looks[selected]);

    // Top bar.
    let top = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(caption("BLOOM"))
        .child(instr::tag("SEED", format!("{:06}", scene.seed), pal.accent, false))
        .child(instr::tag("VOICES", format!("{:.0}", dev.m(82) * aitrack_stock::instrument::bloom::VOICES as f32), pal.accent2, dev.m(82) > 0.))
        .child(species_chips(dev, &scene, cx))
        .child(div().flex_1())
        .child(material_picker(ws, dev, cx));

    // Left: the generator.
    let mut macros = div().flex().flex_wrap().gap(px(SP1));
    for id in [p::DECAY, p::BODY, p::TONE, p::GRIT, p::SPACE] {
        macros = macros.child(dial(dev, id, 36., cx));
    }
    let section = |title: &'static str| label(title, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD);
    let left = div()
        .flex_none()
        .w(px(268.))
        .flex()
        .flex_col()
        .gap(px(SP4))
        .p(px(SP3))
        .rounded(px(r_lg()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(div().flex().flex_col().gap(px(SP2)).child(section("GROW")).child(grow_cluster(ws, dev, cx)))
        .child(div().flex().flex_col().gap(px(SP2)).child(section("ENVELOPE")).child(envelope(&scene, selected, sel_color)))
        .child(div().flex().flex_col().gap(px(SP2)).child(section("MACROS")).child(macros))
        .child(div().flex_1())
        .child(export_chip(ws, dev, &scene, cx));

    // Right: the stage, inset in a bezel whose colour the panel's corners take.
    let bezel = pal.surface;
    let st = stage::stage(ws, dev, &scene, stage::Opts { swatches: true, outer: bezel, slot: 0 }, cx);
    let right = div().flex_1().min_w(px(420.)).min_h(px(400.)).p(px(SP1)).rounded(px(r_lg())).bg(bezel).border_1().border_color(border()).child(st);
    let main = div().flex_1().min_h(px(400.)).flex().gap(px(SP3)).child(left).child(right);

    // Tabs: the DNA of the selected sound, the morph, the play settings, the tools.
    let tab = ws.fx.bloom.view(dev.id).stage.tab.min(TABS.len() - 1);
    let body = match tab {
        0 => div().h(px(260.)).child(dna::panel(ws, dev, &scene, cell, cell_color, cx)).into_any_element(),
        1 => instr::groups(vec![instr::group("MORPH", series(2), vec![morph(ws, dev, 96., cx).into_any_element()])]).into_any_element(),
        2 => instr::groups(vec![instr::group(
            "PLAY",
            pal.muted,
            vec![instr::chips(dev, p::MODE, cx), dial(dev, p::BASE, 32., cx), dial(dev, p::SELECTED, 32., cx), instr::chips(dev, p::CHOKE, cx), dial(dev, p::VELOCITY, 32., cx), dial(dev, p::TUNE, 32., cx), dial(dev, p::VOLUME, 32., cx)],
        )])
        .into_any_element(),
        _ => instr::groups(vec![instr::group("TOOLS", pal.accent, vec![tools(ws, dev, cx).into_any_element()])]).into_any_element(),
    };
    let ui_ms = ws.fx.bloom.view(dev.id).stage.cost.get();
    let gpu_ms = if stage::available() == Some(true) { matter::service::service().cost_ms() } else { 0. };
    instr::root(1000.)
        .child(top)
        .child(main)
        .child(div().flex_none().flex().items_center().gap(px(SP3)).child(tab_strip(ws, dev, cx)).child(div().flex_1()).child(instr::hint(&format!(
            "Click the matter to hear the selected sound · drag on it to grow it · Alt-click locks · drop a sample to match · UI {ui_ms:.2} ms · render {gpu_ms:.2} ms"
        ))))
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_rise_hold_and_fall_to_minus_40_db() {
        let pts = envelope_points(10.0, 5.0, 100.0, 200.0, 200);
        assert_eq!(pts.len(), 201);
        assert_eq!(pts[0].1, 0.0);
        let at = |t: f32| pts.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())).unwrap().1;
        assert!((at(5.0) - 0.5).abs() < 0.02);
        assert_eq!(at(12.0), 1.0);
        assert!((at(115.0) - 0.01).abs() < 0.002, "−40 dB at the decay time");
        assert!(pts.windows(2).skip(20).all(|w| w[1].1 <= w[0].1 + 1e-6), "falls after the hold");
    }

    #[test]
    fn the_layout_sits_on_the_spacing_grid() {
        for v in [72.0f32, 48., 268., 420., 400., 28., 24., 148.] {
            assert_eq!(v % 4., 0., "{v}");
        }
    }
}
