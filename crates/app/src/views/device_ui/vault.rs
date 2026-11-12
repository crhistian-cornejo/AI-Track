//! The Vault instrument's card in the Device View: the loaded preset with
//! its sound type and engine, previous / next preset (through the window's
//! browser filters), the four macros, and a button to open the window with
//! the full browser and the engine's editor.

use super::Dev;
use crate::icons::Icon;
use crate::theme::*;
use crate::views::device_window::vault as vw;
use crate::widgets::{self, label};
use crate::workspace::Workspace;
use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};

/// Width of the compact card.
pub const CARD_W: f32 = 296.;

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let device = dev.id;
    let (list, shown, now) = vw::shown(ws, device);
    let eng = vw::engine(dev);
    let name = ws.s.project.device(device).and_then(|d| d.preset.clone()).unwrap_or_else(|| "Init".into());
    let preset = now.map(|i| &list[i]);
    let kind = preset.map_or_else(|| "No preset".to_owned(), |p| p.kind.clone());
    let fav = preset.map(|p| (p.path.clone(), ws.fx.vault.favourites().contains(&p.path)));
    let position = now.and_then(|i| shown.iter().position(|&s| s == i)).map_or_else(|| format!("{} presets", shown.len()), |k| format!("{} / {}", k + 1, shown.len()));
    let nav = |id: &'static str, i: Icon, dir: i32, tip: &'static str, cx: &mut Context<Workspace>| {
        widgets::sized_icon_button((id, dev.key(0)), i, false, 24., 14.).tooltip(widgets::tip(tip, None)).on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            vw::step_preset(this, device, dir, cx);
        }))
    };
    let title = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(nav("vault-card-prev", Icon::ChevronLeft, -1, "Previous preset", cx))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .items_center()
                .child(label(crate::paint::fit(&name, CARD_W - 96., TEXT_LG), TEXT_LG, if dev.enabled { pal.text } else { pal.muted }).font_weight(FontWeight::BOLD))
                .child(div().flex().items_center().gap(px(SP1)).child(label(kind, TEXT_XS, pal.muted)).child(vw::engine_pill(eng))),
        )
        .child(nav("vault-card-next", Icon::ChevronRight, 1, "Next preset", cx))
        .when_some(fav, |d, (path, on)| d.child(vw::star(("vault-card-star", dev.key(0)), path, on, cx)));
    let open = widgets::text_button(("vault-open", dev.key(0)), "Browse & edit", Some(Icon::OpenWindow), false)
        .tooltip(widgets::tip("Open the preset browser and the engine's editor in a window", None))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            crate::views::device_window::open(this, device, cx);
            cx.notify();
        }));
    div()
        .flex_none()
        .w(px(CARD_W))
        .h_full()
        .p(px(SP2))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .child(div().p(px(SP2)).rounded(px(r_md())).bg(pal.well).child(title))
        .child(div().flex().justify_center().child(vw::macro_row(dev, 44., cx)))
        .child(div().flex_1())
        .child(div().flex().items_center().gap(px(SP2)).child(open).child(div().flex_1()).child(label(position, TEXT_XS, pal.faint)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use crate::views::device_window::vault::browse;
    use aitrack_stock::instrument::vault::{ID, macros, params};

    #[test]
    fn the_card_reads_engine_and_macro_names_from_the_device() {
        let d = dev(ID, &[(params::ENGINE, 4.)]);
        assert_eq!(vw::engine(&d), params::engine::ORGAN);
        assert_eq!(browse::engine_name(vw::engine(&d)), "Drawbar Organ");
        let names: Vec<&str> = macros::macros(vw::engine(&d)).iter().map(|m| m.name).collect();
        assert_eq!(names.len(), 4);
        assert_eq!(names[3], "Space", "macro 4 is Space on every engine");
        // Out-of-range engine values clamp to the last engine.
        assert_eq!(vw::engine(&dev(ID, &[(params::ENGINE, 6.)])), params::engine::WAVETABLE);
    }
}
