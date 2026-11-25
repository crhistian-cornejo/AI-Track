//! `instrument.drumsynth`: the nine types as a row of pads (each lights while
//! it sounds; a click selects it), and below it the selected type's engine,
//! tuning and shape. The card is compact (knob strip); the window has the
//! knobs in groups and the key layout of the type.

use super::instr;
use super::kit;
use super::{Dev, set_now};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_stock::instrument::drumsynth::{KINDS, Kind, keys_label, p};
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, px};

/// The parameter ids of the selected type, in the order of the editor.
pub fn type_ids(kind: usize) -> [u32; 7] {
    [p::MODEL, p::TUNE, p::DECAY, p::TONE, p::SNAP, p::LEVEL, p::PAN].map(|f| p::param(kind, f))
}

/// The row of pads; `h` is a pad's height.
fn pads(dev: &Dev, h: f32, cx: &mut Context<Workspace>) -> gpui::Div {
    let pal = p();
    let selected = dev.choice(p::SELECT).min(KINDS - 1);
    let device = dev.id;
    let mut row = div().flex_none().w_full().flex().gap(px(SP1));
    for kind in Kind::ALL {
        let i = kind.index();
        let on = i == selected;
        let level = dev.m(i).clamp(0., 1.);
        let norm = dev.to_norm(p::SELECT, i as f32);
        let name = kind.label();
        row = row.child(
            div()
                .id(("ds-pad", dev.key(i)))
                .relative()
                .flex_1()
                .min_w(px(0.))
                .h(px(h))
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.22) } else { content(0.06) })
                .border_1()
                .border_color(if on { pal.accent } else { border() })
                .hover(|s| s.bg(if on { accent_alpha(0.3) } else { content(0.12) }))
                .child(div().absolute().bottom(px(0.)).left(px(0.)).w_full().h(px(h * level)).bg(with_alpha(pal.accent2, 0.35)))
                .child(label(name, TEXT_XS, if on { pal.accent } else if dev.enabled { pal.text } else { pal.muted }).font_weight(if on { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL }))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    set_now(this, device, p::SELECT, norm);
                    cx.notify();
                })),
        );
    }
    row
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let kind = Kind::from_index(dev.choice(p::SELECT));
    let ids = type_ids(kind.index());
    let mono_mode = dev.choice(p::MODE) == 1;
    let voices = dev.m(KINDS + 1);
    if !kit::in_window() {
        let w = if dev.big { 720. } else { 600. };
        return kit::panel(w)
            .child(pads(dev, 32., cx))
            .child(kit::strip(ws, dev, &ids, &[p::MODE, p::VOLUME, p::VEL_SENS], cx))
            .into_any_element();
    }
    let keys = if mono_mode { "every key, chromatic from C3".to_owned() } else { keys_label(kind).to_owned() };
    let color = if dev.enabled { pal.accent } else { pal.faint };
    let body = [
        instr::group("ENGINE", color, vec![instr::chips(dev, ids[0], cx)]),
        instr::group("PITCH & SHAPE", color, instr::dials(dev, &[ids[1], ids[2]], cx)),
        instr::group("TIMBRE", color, instr::dials(dev, &[ids[3], ids[4]], cx)),
        instr::group("MIX", color, instr::dials(dev, &[ids[5], ids[6]], cx)),
        instr::group("PLAY", pal.muted, vec![instr::chips(dev, p::MODE, cx), instr::dial(dev, p::VOLUME, cx), instr::dial(dev, p::VEL_SENS, cx)]),
    ];
    instr::root(720.)
        .child(instr::title(
            format!("DRUM SYNTH · {}", kind.label().to_uppercase()),
            vec![instr::tag("KEYS", keys, pal.muted, false), instr::tag("VOICES", format!("{voices:.0}"), pal.accent2, voices > 0.5)],
        ))
        .child(pads(dev, 48., cx))
        .child(instr::groups(body.into()))
        .child(div().flex_none().child(mono(format!("{} · {} models", kind.label(), kind.models().len()), TEXT_XS, pal.faint)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn editor_reads_the_selected_type() {
        let d = dev("instrument.drumsynth", &[(p::SELECT, Kind::Snare.index() as f32), (p::param(1, p::TUNE), 3.)]);
        assert_eq!(d.choice(p::SELECT), Kind::Snare.index());
        let ids = type_ids(1);
        assert!((d.plain(ids[1]) - 3.).abs() < 0.1);
        assert!(ids.iter().all(|&id| d.info(id).is_some()));
    }

    #[test]
    fn every_type_has_all_its_controls() {
        let d = dev("instrument.drumsynth", &[]);
        for kind in Kind::ALL {
            assert!(type_ids(kind.index()).iter().all(|&id| d.info(id).is_some()), "{}", kind.label());
        }
    }
}
