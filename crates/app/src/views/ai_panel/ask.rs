//! The question card: when the model asks with `ask_user`, the card sits
//! on top of the composer with the question and its options. A click (or
//! the option's number while the field is empty) answers; with several
//! options allowed they are ticked and sent together. Whatever the user
//! types in the composer and sends goes as the answer in their own words.

use gpui::{AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};

use super::agent::{self, agent};
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::widgets::{label, mono, text_button};
use crate::workspace::Workspace;

pub fn card(cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    let (question, picked, key) = {
        let p = agent(cx).pending.as_ref()?;
        (p.question.clone(), p.picked.clone(), p.item * 1024 + p.step)
    };
    let mut options = div().flex().flex_col().gap(px(SP1));
    for (i, (title, hint)) in question.options.iter().enumerate() {
        let on = picked.contains(&i);
        options = options.child(
            div()
                .id(("ask-option", i))
                .min_h(px(32.))
                .px(px(SP2))
                .py(px(6.))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .border_1()
                .border_color(if on { accent_alpha(0.55) } else { border() })
                .bg(if on { accent_alpha(0.12) } else { pal.surface })
                .hover(|s| s.border_color(accent_alpha(0.55)))
                .child(
                    div()
                        .size(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(r_sm()))
                        .bg(if on { pal.accent } else { content(0.08) })
                        .child(if on { icon(Icon::Check, 10., pal.on_accent).into_any_element() } else { mono((i + 1).to_string(), TEXT_XS, pal.muted).into_any_element() }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(div().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(super::wrap_text(title)))
                        .when(!hint.is_empty(), |d| d.child(div().min_w(px(0.)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(pal.muted).child(super::wrap_text(hint)))),
                )
                .on_click(cx.listener(move |_, _, _, cx| agent::pick(i, cx))),
        );
    }
    let footer = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(label(if question.options.is_empty() { "Type your answer below" } else { "Or type your own answer below" }, TEXT_XS, pal.faint).flex_1())
        .when(question.multi, |d| {
            let picked = picked.clone();
            d.child(text_button("ask-send", "Send", Some(Icon::Send), !picked.is_empty()).on_click(cx.listener(move |_, _, _, cx| agent::answer(&picked, "", cx))))
        });
    let el = div()
        .flex_none()
        .overflow_hidden()
        .mx(px(SP3))
        .mb(px(SP2))
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(pal.raised)
        .border_1()
        .border_color(accent_alpha(0.45))
        .shadow_lg()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(icon(Icon::Help, 14., pal.accent))
                .child(label(if question.multi { "Question · pick any" } else { "Question" }, TEXT_XS, pal.accent).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(
                    div()
                        .id("ask-skip")
                        .h(px(20.))
                        .px(px(SP2))
                        .flex()
                        .items_center()
                        .rounded(px(r_md()))
                        .cursor_pointer()
                        .hover(|s| s.bg(content(0.08)))
                        .child(label("Skip", TEXT_XS, pal.muted))
                        .on_click(cx.listener(|_, _, _, cx| agent::answer(&[], "", cx))),
                ),
        )
        .child(div().text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(question.question.clone()))
        .child(options)
        .child(footer);
    Some(motion::rise_in(("ask-card", key), el, 8.).into_any_element())
}
