//! The composer's model picker (after monoBOT's): the user's two
//! subscriptions, Claude through Claude Code and ChatGPT through Codex,
//! each with its newest models. It floats above the composer, outside the
//! panel's clip. Hovering a model opens a card beside the list with what
//! it is good at and its reasoning effort; changing the effort there
//! switches to that model at once, a click on the row picks it and closes.

use aitrack_ai::llm::agent::Effort;
use aitrack_ai::llm::cli::{ModelInfo, Provider};
use gpui::{
    Anchor, AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, deferred, div, prelude::FluentBuilder, px,
};

use super::agent::{self, PROVIDERS, agent};
use super::state;
use super::steps::logo;
use crate::bridge::AgentKind;
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::widgets::{self, label};
use crate::workspace::Workspace;

const LIST_W: f32 = 232.;
const CARD_W: f32 = 212.;
const GAP: f32 = 6.;

/// Four bars filled up to `effort`: the pill's and the card's gauge.
fn bars(effort: Effort, h: f32, w: f32, gap: f32, on: gpui::Hsla, off: gpui::Hsla) -> impl IntoElement {
    let lit = Effort::ALL.iter().position(|e| *e == effort).unwrap_or(0);
    let mut row = div().flex().items_end().gap(px(gap));
    for k in 0..Effort::ALL.len() {
        row = row.child(div().w(px(w)).h(px(h * (0.4 + 0.2 * k as f32))).rounded(px(1.)).bg(if k <= lit { on } else { off }));
    }
    row
}

/// The composer's model button: logo, name and the effort gauge. The
/// picker hangs from it.
pub fn pill(cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let c = agent::choice(cx);
    let effort = agent(cx).effort;
    let open = agent::menu_open(cx);
    div()
        .id("agent-model")
        .relative()
        .h(px(24.))
        .pl(px(SP1))
        .pr(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if open { content(0.1) } else { gpui::transparent_black() })
        .hover(|s| s.bg(content(0.08)))
        .child(logo(c.provider.vendor(), 16.))
        .child(label(crate::paint::fit(c.name.trim_start_matches("Claude "), 76., TEXT_XS), TEXT_XS, pal.text))
        .child(bars(effort, 10., 2., 1., pal.muted, content(0.18)))
        .child(icon(Icon::ChevronDown, 10., pal.faint))
        .when(!open, |d| d.tooltip(widgets::tip("Model and effort", None)))
        .on_click(cx.listener(|ws, _, _, cx| agent::toggle_menu(ws, cx)))
        .when(open, |d| d.child(menu(cx)))
        .into_any_element()
}

/// What each provider row says about whether it can run.
fn status(p: Provider, installed: bool, loading: bool) -> (&'static str, &'static str) {
    let title = match p {
        Provider::Codex => "ChatGPT",
        _ => "Claude",
    };
    let status = match (installed, loading) {
        (true, true) => "Loading…",
        (true, false) => "Your subscription",
        (false, _) if p == Provider::Codex => "Codex not installed",
        (false, _) => "Claude Code not installed",
    };
    (title, status)
}

fn setup_note(p: Provider) -> String {
    let kind = if p == Provider::Codex { AgentKind::Codex } else { AgentKind::ClaudeCode };
    format!("{} Then open the model menu again.", kind.setup())
}

/// The list and the card, floating above the pill.
fn menu(cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let current = agent::choice(cx);
    let mut rows = div().id("agent-model-list").max_h(px(320.)).overflow_y_scroll().flex().flex_col();
    let mut shown: Option<ModelInfo> = None;
    for (k, provider) in PROVIDERS.into_iter().enumerate() {
        let a = agent(cx);
        let installed = a.installed.get(&provider).copied().unwrap_or(false);
        let list = agent::models(a, provider);
        let loading = installed && a.fetching && !a.catalogs.contains_key(&provider);
        let (title, status) = status(provider, installed, loading);
        rows = rows.child(
            div()
                .when(k > 0, |d| d.mt(px(SP1)).pt(px(SP1)).border_t_1().border_color(border()))
                .h(px(28.))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(logo(provider.vendor(), 14.))
                .child(label(title, TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(label(status, TEXT_XS, pal.faint)),
        );
        for (n, m) in list.into_iter().enumerate() {
            let on = current.provider == provider && current.model == m.id;
            if on && shown.is_none() {
                shown = Some(m.clone());
            }
            let hovered = agent(cx).hover.as_ref().is_some_and(|h| h.provider == provider && h.id == m.id);
            let hm = m.clone();
            let row = div()
                .id(("agent-model-row", k * 16 + n))
                .h(px(30.))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .rounded(px(r_md()))
                .cursor_pointer()
                .when(hovered, |d| d.bg(content(0.07)))
                .when(!installed, |d| d.opacity(0.45))
                .on_hover(cx.listener(move |_, h: &bool, _, cx| {
                    if *h {
                        let a = agent(cx);
                        if a.hover.as_ref() != Some(&hm) {
                            a.hover = Some(hm.clone());
                            cx.notify();
                        }
                    }
                }))
                .child(label(m.name.clone(), TEXT_SM, pal.text).flex_1().min_w(px(0.)))
                .when(on, |d| d.child(icon(Icon::Check, 12., pal.accent)));
            rows = rows.child(if installed {
                row.on_click(cx.listener(move |ws, _, _, cx| agent::choose(ws, m.clone(), None, true, cx)))
            } else {
                row.on_click(cx.listener(move |ws, _, _, cx| {
                    state::note(ws, setup_note(provider), cx);
                    agent::close_menu(cx);
                }))
            });
        }
    }
    let a = agent(cx);
    let card_for = a.hover.clone().or(shown).unwrap_or_else(|| agent::models(a, current.provider).into_iter().next().unwrap_or_else(|| aitrack_ai::llm::cli::fallback(Provider::ClaudeCode)[0].clone()));
    let card = card(card_for, cx);
    let list = widgets::menu_card("agent-model-menu", LIST_W).child(rows);
    // Hung from the pill's top left: the list lines up with the pill and the card sits to its left.
    div()
        .absolute()
        .top(px(-GAP))
        .left(px(-(CARD_W + GAP)))
        .child(deferred(
            gpui::anchored().anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(SP2)).child(
                div()
                    .id("agent-model-menu-wrap")
                    .occlude()
                    .flex()
                    .items_end()
                    .gap(px(GAP))
                    .on_mouse_down_out(cx.listener(|_, _, _, cx| agent::close_menu(cx)))
                    .child(motion::rise_in("agent-model-card-in", card, 4.))
                    .child(motion::rise_in("agent-model-menu-in", list, 4.)),
            ),
        ))
        .into_any_element()
}

/// The card beside the list: the hovered model, what it is for, and its effort.
fn card(m: ModelInfo, cx: &mut Context<Workspace>) -> gpui::Div {
    let pal = p();
    let a = agent(cx);
    let installed = a.installed.get(&m.provider).copied().unwrap_or(false);
    let current = a.choice.as_ref().is_some_and(|c| c.provider == m.provider && c.model == m.id);
    let effort = a.effort;
    let mut steps = div().flex().gap(px(2.));
    for (k, e) in Effort::ALL.into_iter().enumerate() {
        let lit = Effort::ALL.iter().position(|x| *x == effort).unwrap_or(0) >= k;
        let mm = m.clone();
        steps = steps.child(
            div()
                .id(("effort-step", k))
                .flex_1()
                .h(px(28.))
                .flex()
                .flex_col()
                .items_center()
                .justify_end()
                .gap(px(3.))
                .rounded(px(r_sm()))
                .cursor_pointer()
                .hover(|s| s.bg(content(0.06)))
                .child(div().w_full().px(px(2.)).child(div().h(px(4.)).rounded(px(2.)).bg(if lit { pal.accent } else { content(0.14) })))
                .child(label(e.label(), TEXT_XS, if e == effort { pal.text } else { pal.faint }))
                .when(installed, |d| d.on_click(cx.listener(move |ws, _, _, cx| agent::choose(ws, mm.clone(), Some(e), false, cx)))),
        );
    }
    let provider = match m.provider {
        Provider::Codex => "Codex · ChatGPT subscription",
        _ => "Claude Code · Claude subscription",
    };
    widgets::menu_card("agent-model-card", CARD_W)
        .p(px(SP3))
        .gap(px(SP2))
        .child(div().flex().items_center().gap(px(SP2)).child(logo(m.provider.vendor(), 20.)).child(label(m.name.clone(), TEXT_MD, pal.text).font_weight(FontWeight::SEMIBOLD).flex_1().min_w(px(0.))))
        .when(!m.hint.is_empty(), |d| d.child(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.muted).child(m.hint.clone())))
        .child(
            div()
                .pt(px(SP2))
                .border_t_1()
                .border_color(border())
                .flex()
                .flex_col()
                .gap(px(SP1))
                .child(div().flex().items_center().child(label("Reasoning", TEXT_XS, pal.faint).font_weight(FontWeight::MEDIUM)).child(div().flex_1()).child(label(if current { "In use" } else { "Click to switch" }, TEXT_XS, if current { pal.accent } else { pal.faint })))
                .child(steps),
        )
        .child(label(if installed { provider } else { "Not installed on this computer" }, TEXT_XS, pal.faint))
}
