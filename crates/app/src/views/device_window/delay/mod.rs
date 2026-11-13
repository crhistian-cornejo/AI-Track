//! The delay's window: engine, routing, taps and sync as chips, the echo
//! timeline and the feedback filter at full size, and the controls in
//! sections (Time, Character, Filter, Motion, Output). Rendered through the
//! Workspace like the Device View card, with the same commands.

use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::views::device_ui::delay::{filter_plot, timeline_plot};
use crate::workspace::Workspace;
use aitrack_stock::fx::delay::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
// Shared with the reverb window (`device_window::reverb::parts` once both
// windows are declared from `device_window/mod.rs`).
use crate::views::device_ui::reverb::window::parts::{choice_row, labelled, section};

/// The sections of the window for a synced (`true`) or free delay.
pub fn sections(sync: bool) -> [(&'static str, Vec<u32>); 5] {
    let (tl, tr) = if sync { (p::DIV_L, p::DIV_R) } else { (p::TIME_L, p::TIME_R) };
    [
        ("Time", vec![tl, tr, p::FEEDBACK]),
        ("Character", vec![p::DRIVE, p::AGE, p::WOW, p::FLUTTER]),
        ("Filter", vec![p::LOW_CUT, p::HIGH_CUT]),
        ("Motion", vec![p::MOD_RATE, p::MOD_DEPTH, p::DIFFUSION]),
        ("Output", vec![p::WIDTH, p::DUCK, p::MIX, p::OUTPUT]),
    ]
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let timeline = timeline_plot(ws, dev, cx);
    let filter = filter_plot(ws, dev, cx);
    let top = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP3))
        .child(labelled("Engine", choice_row(dev, p::ENGINE, true, cx)))
        .child(labelled("Routing", choice_row(dev, p::MODE, false, cx)))
        .child(labelled("Taps", choice_row(dev, p::TAPS, false, cx)))
        .child(crate::views::device_ui::kit::pick(dev, p::SYNC, cx))
        .child(crate::views::device_ui::kit::pick(dev, p::FREEZE, cx));
    let mut controls = div().flex_none().flex().flex_wrap().items_start().gap(px(SP2));
    for (title, ids) in sections(dev.on(p::SYNC)) {
        controls = controls.child(section(ws, dev, title, &ids, cx));
    }
    div()
        .size_full()
        .min_w(px(860.))
        .p(px(SP3))
        .gap(px(SP3))
        .flex()
        .flex_col()
        .child(top)
        .child(div().flex_1().min_h(px(160.)).child(timeline))
        .child(div().flex_none().h(px(32.)).child(filter))
        .child(controls)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::fx::delay::params;

    #[test]
    fn every_parameter_has_a_place_in_the_window() {
        let chips = [p::ENGINE, p::MODE, p::TAPS, p::SYNC, p::FREEZE];
        let both: Vec<u32> = sections(true).into_iter().chain(sections(false)).flat_map(|(_, ids)| ids).collect();
        for info in params() {
            assert!(both.contains(&info.id) || chips.contains(&info.id), "{} is not in the window", info.name);
        }
        let synced: Vec<u32> = sections(true).into_iter().flat_map(|(_, ids)| ids).collect();
        assert!(synced.contains(&p::DIV_L) && !synced.contains(&p::TIME_L), "synced shows divisions");
    }
}
