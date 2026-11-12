//! The fallback editor for plugins without a visual one: knobs in named
//! sections (parameters numbered alike, "Band 3 …", "Osc 2 …", share a
//! section; the rest lead as "Main"), the first section only until the
//! card is asked to show everything.

use super::{Dev, caption};
use crate::theme::*;
use crate::views::devices::knob_control;
use crate::widgets::{knob_cell, label};
use crate::workspace::{KnobTarget, Workspace};
use aitrack_dsp::ParamInfo;
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, px};

const CELL_W: f32 = 56.;
/// Parameters shown before "Show all".
const LEAD: usize = 12;

/// The section a parameter belongs to: `"Band 3 Gain"` → `"Band 3"`,
/// anything unnumbered → `"Main"`.
pub fn group_of(name: &str) -> String {
    let mut words = name.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(a), Some(n), Some(_)) if n.chars().all(|c| c.is_ascii_digit()) => format!("{a} {n}"),
        _ => "Main".to_owned(),
    }
}

/// Parameters in sections: "Main" first, then the numbered groups in the
/// order the plugin declares them. Within a section the plugin's own order
/// stands (it lists what matters first).
pub fn sections(params: &[ParamInfo]) -> Vec<(String, Vec<&ParamInfo>)> {
    let mut out: Vec<(String, Vec<&ParamInfo>)> = Vec::new();
    for p in params {
        let g = group_of(p.name);
        match out.iter_mut().find(|(name, _)| *name == g) {
            Some((_, list)) => list.push(p),
            None => out.push((g, vec![p])),
        }
    }
    if let Some(i) = out.iter().position(|(name, _)| name == "Main") {
        let main = out.remove(i);
        out.insert(0, main);
    }
    out
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let all = ws.fx.show_all.contains(&dev.id);
    let device = dev.id;
    let set = dev.set.clone();
    let groups = sections(&set.list);
    let total = set.list.len();
    let rows = if dev.big { 4 } else { 2 };
    let mut row = div().flex_none().h_full().p(px(SP2)).gap(px(SP3)).flex();
    let mut shown = 0;
    for (name, params) in &groups {
        // Collapsed: the leading section, capped.
        let take = if all { params.len() } else { LEAD.saturating_sub(shown).min(params.len()) };
        if take == 0 {
            break;
        }
        shown += take;
        let cols = take.div_ceil(rows).max(1);
        let mut grid = div().w(px(cols as f32 * CELL_W)).flex().flex_wrap().gap_y(px(SP2));
        for info in params.iter().take(take) {
            let value = dev.norm(info.id);
            let default = info.default_normalized();
            let target = KnobTarget::Param { device, param: info.id, default };
            let k = knob_control(("knob", dev.key(info.id as usize)), value, info.min < 0. && info.max > 0., dev.enabled, target, cx);
            // Section names repeat in every label; the header carries them.
            let short = if info.short.len() < info.name.len() { info.short } else { info.name.strip_prefix(name.as_str()).map_or(info.name, str::trim) };
            grid = grid.child(knob_cell(k, short, info.format(value), CELL_W));
        }
        row = row.child(div().flex_none().h_full().flex().flex_col().gap(px(SP2)).child(caption(name.to_uppercase())).child(grid));
        if !all && shown >= LEAD {
            break;
        }
    }
    if total > shown || all && total > LEAD {
        let text = if all { "Show less".to_owned() } else { format!("Show all {total}") };
        row = row.child(
            div()
                .id(("show-all", dev.key(0)))
                .flex_none()
                .w(px(88.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_md()))
                .border_1()
                .border_dashed()
                .border_color(border_strong())
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .child(label(text, TEXT_SM, pal.muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.fx.show_all.remove(&device) {
                        this.fx.show_all.insert(device);
                    }
                    cx.notify();
                })),
        );
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{PluginCatalog, StubCatalog};

    #[test]
    fn numbered_parameters_share_a_section() {
        assert_eq!(group_of("Band 3 Gain"), "Band 3");
        assert_eq!(group_of("Osc 2 Fine"), "Osc 2");
        assert_eq!(group_of("LFO 1 Rate"), "LFO 1");
        assert_eq!(group_of("Pad 12 Tune"), "Pad 12");
        assert_eq!(group_of("Output"), "Main");
        assert_eq!(group_of("Auto Gain"), "Main");
        assert_eq!(group_of("Stereo Link"), "Main");
    }

    #[test]
    fn the_eq_groups_into_main_and_eight_bands() {
        let catalog = StubCatalog::default();
        let eq = catalog.device("fx.eq").unwrap();
        let s = sections(&eq.params);
        assert_eq!(s.len(), 9);
        assert_eq!(s[0].0, "Main");
        assert_eq!(s[0].1.iter().map(|p| p.name).collect::<Vec<_>>(), vec!["Output", "Auto Gain", "Processing"]);
        assert_eq!(s[1].0, "Band 1");
        assert_eq!(s[1].1.len(), 12);
        assert_eq!(s.iter().map(|(_, l)| l.len()).sum::<usize>(), eq.params.len());
        // A plugin without numbered parameters is one section in its own order.
        let comp = catalog.device("fx.compressor").unwrap();
        let s = sections(&comp.params);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].1[0].name, "Threshold");
    }
}
