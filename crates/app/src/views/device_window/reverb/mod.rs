//! The reverb's window: every algorithm and colour on view as chips, the
//! decay-over-frequency and impulse plots at full size, and the controls in
//! sections (Space, Tone, Motion, Shimmer, Output). Rendered through the
//! Workspace like the Device View card, with the same commands.

pub mod parts;

use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::views::device_ui::reverb::{decay_plot, impulse_plot};
use crate::workspace::Workspace;
use aitrack_stock::fx::reverb::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use parts::{choice_row, labelled, section};

/// The sections of the window and the parameters in each.
pub const SECTIONS: [(&str, &[u32]); 5] = [
    ("Space", &[p::DECAY, p::SIZE, p::PREDELAY, p::DIFFUSION, p::EARLY, p::LINES]),
    ("Tone", &[p::DAMPING, p::DAMP_FREQ, p::BASS_MULT, p::BASS_FREQ, p::LOW_CUT, p::HIGH_CUT]),
    ("Motion", &[p::MOD_DEPTH, p::MOD_RATE]),
    ("Shimmer", &[p::SHIMMER, p::SHIFT]),
    ("Output", &[p::WIDTH, p::DUCK, p::MIX, p::OUTPUT]),
];

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let decay = decay_plot(ws, dev, cx);
    let impulse = impulse_plot(ws, dev, cx);
    let classic = dev.choice(p::ALGORITHM) == 0;
    let mut top = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP3))
        .child(labelled("Algorithm", choice_row(dev, p::ALGORITHM, true, cx)))
        .child(labelled("Colour", choice_row(dev, p::COLOR, false, cx)))
        .child(crate::views::device_ui::kit::pick(dev, p::FREEZE, cx));
    if classic {
        top = top.child(labelled("Mode", choice_row(dev, p::MODE, false, cx)));
    }
    let mut controls = div().flex_none().flex().flex_wrap().items_start().gap(px(SP2));
    for (title, ids) in SECTIONS {
        controls = controls.child(section(ws, dev, title, ids, cx));
    }
    div()
        .size_full()
        .min_w(px(860.))
        .p(px(SP3))
        .gap(px(SP3))
        .flex()
        .flex_col()
        .child(top)
        .child(
            div()
                .flex_1()
                .min_h(px(160.))
                .flex()
                .gap(px(SP3))
                .child(div().flex_1().min_w(px(0.)).h_full().child(decay))
                .child(div().flex_none().w(px(320.)).h_full().child(impulse)),
        )
        .child(controls)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::fx::reverb::params;

    #[test]
    fn every_parameter_has_a_place_in_the_window() {
        let placed: Vec<u32> = SECTIONS.iter().flat_map(|(_, ids)| ids.iter().copied()).collect();
        // The chips above the plots hold the rest.
        let chips = [p::ALGORITHM, p::COLOR, p::FREEZE, p::MODE];
        for info in params() {
            assert!(placed.contains(&info.id) || chips.contains(&info.id), "{} is not in the window", info.name);
        }
        let mut sorted = placed.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), placed.len(), "no parameter twice");
    }
}
