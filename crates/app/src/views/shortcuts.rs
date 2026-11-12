//! The shortcuts sheet (Ctrl+/ or ?): every command of the key table with
//! its keys, searchable. Also the small Rename dialog (Ctrl+R).

use crate::keys::{Category, Entry, TABLE};
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, StatefulInteractiveElement, Styled, div,
    prelude::FluentBuilder, px,
};

/// A key as a small cap.
fn cap(text: String) -> gpui::Div {
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(SP1))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .bg(content(0.06))
        .border_1()
        .border_color(border())
        .child(mono(text, TEXT_XS, p().text))
}

fn matches(e: &Entry, query: &str) -> bool {
    let hay = format!("{} {} {} {}", e.label, e.help, e.category.label(), e.caps().join(" ")).to_lowercase();
    query.split_whitespace().all(|word| hay.contains(word))
}

fn row(e: &Entry) -> gpui::Div {
    let pal = p();
    let mut keys = div().flex_none().flex().items_center().gap(px(SP1));
    for (i, k) in e.caps().into_iter().enumerate() {
        if i > 0 {
            keys = keys.child(label("or", TEXT_XS, pal.faint));
        }
        keys = keys.child(cap(k));
    }
    div()
        .min_h(px(ROW_H + SP2))
        .px(px(SP2))
        .py(px(SP1))
        .flex()
        .items_center()
        .gap(px(SP3))
        .rounded(px(r_md()))
        .hover(|s| s.bg(hover()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(label(e.label, TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
                // A long help line wraps instead of running under the keys.
                .child(label(e.help, TEXT_SM, pal.muted).whitespace_normal()),
        )
        .child(keys)
}

pub fn render(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let query = ws.ui.shortcuts.read(cx).text().trim().to_lowercase();
    let mut list = div().flex().flex_col().gap(px(SP1)).p(px(SP2));
    let mut shown = 0;
    for cat in Category::ALL {
        let rows: Vec<&Entry> = TABLE.iter().filter(|e| e.category == cat && matches(e, &query)).collect();
        if rows.is_empty() {
            continue;
        }
        shown += rows.len();
        list = list.child(div().pt(px(SP2)).px(px(SP2)).child(label(cat.label().to_uppercase(), TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD)));
        for e in rows {
            list = list.child(row(e));
        }
    }
    div()
        .flex()
        .flex_col()
        // Escape closes the sheet even while the search field has the keys.
        .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
            if e.keystroke.key == "escape" {
                this.close_modal(cx);
                this.focus.focus(window, cx);
                cx.stop_propagation();
            }
        }))
        .child(div().flex_none().p(px(SP3)).border_b_1().border_color(border()).child(ws.ui.shortcuts.clone()))
        .child(div().id("shortcuts-list").max_h(px(560.)).overflow_y_scroll().child(list).when(shown == 0, |d| {
            d.child(div().p(px(SP6)).flex().justify_center().child(label("No command matches. Try a key (\"ctrl+d\") or a word (\"loop\").", TEXT_MD, pal.muted)))
        }))
        .into_any_element()
}

/// The Rename dialog: a text field; Enter applies, Escape cancels.
pub fn render_rename(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let what = match ws.ui.rename_target {
        Some(crate::commands::RenameTarget::Clip(_)) => "Clip name",
        _ => "Track name",
    };
    div()
        .p(px(SP4))
        .flex()
        .flex_col()
        .gap(px(SP3))
        .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| match e.keystroke.key.as_str() {
            "enter" => {
                crate::commands::apply_rename(this, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                this.close_modal(cx);
                this.focus.focus(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }))
        .child(label(what, TEXT_SM, pal.muted))
        .child(ws.ui.rename.clone())
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(SP2))
                .child(widgets::text_button("rename-cancel", "Cancel", None, false).on_click(cx.listener(|this, _, window, cx| {
                    this.close_modal(cx);
                    this.focus.focus(window, cx);
                })))
                .child(widgets::text_button("rename-ok", "Rename", None, true).on_click(cx.listener(|this, _, window, cx| crate::commands::apply_rename(this, window, cx)))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_commands_by_name_help_and_key() {
        let found = |q: &str| TABLE.iter().filter(|e| matches(e, q)).map(|e| e.label).collect::<Vec<_>>();
        assert!(found("paste").contains(&"Paste"));
        assert!(found("insert marker").contains(&"Play / Stop"));
        assert!(found("continue").contains(&"Continue") && found("f11").contains(&"Full Screen"));
        assert_eq!(found("").len(), TABLE.len());
        assert!(found("zzzz").is_empty());
    }
}
