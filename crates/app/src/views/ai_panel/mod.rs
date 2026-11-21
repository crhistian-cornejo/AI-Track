//! The assistant panel: one chat. It slides in on a spring; replies stream
//! in token by token from the chosen model (Claude through the Anthropic
//! API, or the user's own agent CLI). Typing `/` opens every action (extract
//! MIDI, split stems, import, analyse, find samples, mix this track…) as
//! cards; what an action does shows up as cards in the chat: job progress,
//! results, and proposals accepted or discarded through `History::accept` /
//! `discard`. Tracks, clips, samples and screenshots ride along as chips.

mod agent;
/// The pattern engine writing a part into a draft (the external MCP tools use it too).
pub(crate) use agent::generate_part;
mod ask;
pub mod history;
mod markdown;
mod mix;
pub mod part;
mod picker;
pub mod slash;
pub mod state;
mod steps;

use crate::bridge::ModelNotice;
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::views::browser::short_key;
use crate::widgets::{self, badge, label, mono, text_button};
use crate::workspace::{ChatEntry, PartCard, ProposalStatus, Workspace};
use gpui::{
    Animation, AnimationExt, AnyElement, Context, ExternalPaths, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, linear_color_stop, linear_gradient, prelude::FluentBuilder, px,
};
use state::{Backend, Item, PartAction, Placed, ReplyState};

/// Whether the assistant panel needs a frame every vsync. The loader and
/// the caret animate on their own and the stream polls on a timer, so no.
pub fn wants_frames(_ws: &Workspace) -> bool {
    false
}

const HEADER_H: f32 = TAB_BAR_H + SP2;
/// Inner width of a card in the log.
const CARD_W: f32 = AI_PANEL_W - 2. * SP3;
/// How far the log fades under the header and above the composer.
const TOP_FADE: f32 = 20.;
const BOTTOM_FADE: f32 = 28.;

/// Insert zero-width break opportunities inside long unbroken tokens
/// (sample filenames with underscores, paths, URLs) so GPUI's word wrapper
/// never lets a single token push past the narrow assistant panel.
pub(super) fn wrap_text(s: &str) -> String {
    const BREAK_MIN: usize = 18;
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            run = 0;
            out.push(ch);
            continue;
        }
        run += 1;
        out.push(ch);
        if run >= 6 && matches!(ch, '_' | '/' | '-' | ':' | '.') && chars.peek().is_some_and(|n| !n.is_whitespace()) {
            // Only insert soft breaks when the surrounding token is actually long.
            let ahead = chars.clone().take_while(|n| !n.is_whitespace()).count();
            if run + ahead >= BREAK_MIN {
                out.push('\u{200B}');
                run = 0;
            }
        }
    }
    out
}

/// A band that fades the log out under the header (`down`) or above the composer.
fn fade(down: bool, h: f32) -> impl IntoElement {
    let bg = pane_bg(crate::appearance::Pane::Assistant);
    let solid = linear_color_stop(bg, 0.);
    let clear = linear_color_stop(with_alpha(bg, 0.), 1.);
    div().absolute().left_0().right_0().h(px(h)).when(down, |d| d.top_0()).when(!down, |d| d.bottom_0()).bg(linear_gradient(if down { 180. } else { 0. }, solid, clear))
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    state::sync_parts(ws, cx);
    agent::init(ws, cx);
    let a = state::get(cx);
    let target = if ws.ai_open { AI_PANEL_W } else { 0. };
    let (backend, streaming, items, attachments, scroll, input, sel, tab, history_open) = {
        let s = a.read(cx);
        (s.backend, s.streaming(), s.items.clone(), s.attachments.clone(), s.scroll.clone(), s.input.clone(), s.slash_sel, s.slash_tab, s.history_open)
    };
    let streaming = streaming || agent::running(cx);
    let text = input.read(cx).text().to_owned();
    let chats = state::all_chats(cx);
    let chat_title = if items.is_empty() { "Assistant".to_owned() } else { history::title(&items) };

    let ready = agent::ready(cx);
    let header = div()
        .flex_none()
        .h(px(HEADER_H))
        .pl(px(SP3))
        .pr(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .child(div().flex_1().min_w(px(0.)).child(label(crate::paint::fit(&chat_title, AI_PANEL_W - 120., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::SEMIBOLD)))
        .child(
            widgets::icon_button("chat-history", Icon::Clock, history_open)
                .tooltip(widgets::tip("This project's chats", None))
                .on_click(cx.listener(|_, _, _, cx| state::toggle_history(cx))),
        )
        .child(widgets::icon_button("new-chat", Icon::Add, false).tooltip(widgets::tip("New chat", None)).on_click(cx.listener(|ws, _, _, cx| state::new_chat(ws, cx))))
        .child(widgets::icon_button("close-ai", Icon::X, false).tooltip(widgets::tip("Close", Some("Ctrl+I"))).on_click(cx.listener(|this, _, _, cx| {
            this.ai_open = false;
            cx.notify();
        })));

    // The log: our items interleaved with the workspace's cards.
    let mut log = div().w_full().min_w(px(0.)).overflow_x_hidden().flex().flex_col().gap(px(SP3)).px(px(SP3)).pt(px(SP4)).pb(px(BOTTOM_FADE));
    let entries = ws.chat_log.clone();
    let n_entries = entries.len();
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut by_anchor: Vec<(usize, usize, Placed)> = items.into_iter().enumerate().map(|(i, p)| (p.anchor.min(n_entries), i, p)).collect();
    by_anchor.sort_by_key(|(anchor, i, _)| (*anchor, *i));
    let mut mine = by_anchor.into_iter().peekable();
    for at in 0..=n_entries {
        while let Some((_, i, p)) = mine.next_if(|(anchor, _, _)| *anchor == at) {
            rows.push(item(i, p.item, cx));
        }
        if let Some(e) = entries.get(at) {
            rows.push(entry(at, e.clone(), cx));
        }
    }
    if rows.is_empty() {
        rows.push(steps::welcome(ready, cx));
    }
    let total = rows.len();
    for (i, el) in rows.into_iter().enumerate() {
        // The newest card rises in; the rest stay put.
        log = log.child(if i + 1 == total { motion::rise_in(("chat", total), div().child(el), 8.).into_any_element() } else { el });
    }
    // The agent CLI has no token stream: its loader shows while it works.
    if let Backend::Agent(kind) = backend
        && ws.b.ai.agent() == Some(kind)
        && ws.b.ai.busy()
    {
        log = log.child(reply_row(div().child(thinking(Some(kind.name()), 0)).into_any_element()));
    }

    let slash_menu = slash::query(&text).map(|q| slash_menu(q, tab, sel, cx));
    let question = ask::card(cx);
    let asking = question.is_some();

    let chips = (!attachments.is_empty()).then(|| {
        let mut row = div().flex_none().px(px(SP2)).pt(px(SP2)).flex().flex_wrap().gap(px(SP1));
        for (i, x) in attachments.iter().enumerate() {
            let (ic, name) = x.chip();
            row = row.child(
                div()
                    .h(px(24.))
                    .pl(px(SP2))
                    .pr(px(SP1))
                    .gap(px(SP1))
                    .flex()
                    .items_center()
                    .rounded(px(r_md()))
                    .bg(accent_alpha(0.12))
                    .border_1()
                    .border_color(accent_alpha(0.3))
                    .child(icon(ic, 12., pal.accent))
                    .child(label(crate::paint::fit(&name, 140., TEXT_XS), TEXT_XS, pal.text))
                    .child(widgets::sized_icon_button(("detach", i), Icon::X, false, 16., 10.).on_click(cx.listener(move |_, _, _, cx| state::detach(i, cx)))),
            );
        }
        row
    });

    let send_or_stop = if streaming && !(asking && !text.trim().is_empty()) {
        widgets::icon_button("stop-chat", Icon::Stop, true).tooltip(widgets::tip("Stop", Some("Esc"))).on_click(cx.listener(|ws, _, _, cx| state::stop(ws, cx)))
    } else {
        let input = input.clone();
        widgets::icon_button("send-chat", Icon::Send, !text.trim().is_empty()).tooltip(widgets::tip(if asking { "Answer" } else { "Send" }, Some("Enter"))).on_click(cx.listener(move |ws, _, _, cx| {
            let text = input.read(cx).text().trim().to_owned();
            if text.is_empty() {
                return;
            }
            input.update(cx, |i, cx| i.set_text("", cx));
            if agent::asking(cx) {
                let picked = agent::agent(cx).pending.as_ref().map(|p| p.picked.clone()).unwrap_or_default();
                agent::answer(&picked, &text, cx);
            } else if slash::query(&text).is_none() {
                state::submit_text(ws, text, cx);
            }
        }))
    };
    let menu_is_open = slash::query(&text).is_some();
    let field_empty = text.is_empty();
    let input_field = div()
                .id("chat-input")
                .flex_1()
                .min_w(px(0.))
                // Keep the keys: the workspace takes focus back on clicks outside its own fields.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(move |ws, e: &KeyDownEvent, _, cx| {
                    let k = &e.keystroke;
                    let digit = k.key.parse::<usize>().ok().filter(|d| (1..=9).contains(d) && k.modifiers.number_of_modifiers() == 0);
                    let handled = match k.key.as_str() {
                        // A question is open and nothing is typed: its numbers pick.
                        _ if asking && field_empty && digit.is_some() => {
                            agent::pick(digit.unwrap_or(1) - 1, cx);
                            true
                        }
                        "up" if menu_is_open => state::move_selection(ws, -1, cx),
                        "down" if menu_is_open => state::move_selection(ws, 1, cx),
                        "tab" if menu_is_open => {
                            let next = state::get(cx).read(cx).slash_tab.next();
                            state::set_tab(next, cx);
                            true
                        }
                        "v" if k.modifiers.secondary() => {
                            state::paste(ws, cx);
                            true
                        }
                        "escape" if agent::menu_open(cx) => {
                            agent::close_menu(cx);
                            true
                        }
                        "escape" if state::get(cx).read(cx).streaming() || agent::running(cx) => {
                            state::stop(ws, cx);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                    }
                }))
                .child(input.clone());
    let toolbar = div()
        .px(px(SP1))
        .pb(px(SP1))
        .flex()
        .items_center()
        .gap(px(2.))
        .child(widgets::sized_icon_button("attach-file", Icon::Add, false, 24., 14.).tooltip(widgets::tip("Attach a sample or a screenshot", None)).on_click(cx.listener(|_, _, _, cx| state::pick_files(cx))))
        .child(widgets::sized_icon_button("attach-sel", Icon::Link, false, 24., 14.).tooltip(widgets::tip("Attach the selected track and clip", None)).on_click(cx.listener(|ws, _, _, cx| state::attach_selection(ws, cx))))
        .child(picker::pill(cx))
        .child(auto_switch(cx))
        .child(div().flex_1())
        .child(send_or_stop);

    // Scrolled up to read: a button brings the newest message back.
    let away = !state::at_bottom(&scroll);
    let jump = away.then(|| {
        div().absolute().bottom(px(SP2)).left_0().right_0().flex().justify_center().child(
            motion::rise_in(
                "jump-latest-in",
                div()
                    .id("jump-latest")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(pal.raised)
                    .border_1()
                    .border_color(border_strong())
                    .shadow_md()
                    .hover(|s| s.bg(pal.surface))
                    .child(icon(Icon::ChevronDown, 14., pal.text))
                    .tooltip(widgets::tip("Jump to the latest message", None))
                    .on_click(cx.listener(|_, _, _, cx| state::jump_to_latest(cx))),
                4.,
            ),
        )
    });
    let log_area = div()
        .relative()
        .flex_1()
        .min_h(px(0.))
        .child(
            div()
                .id("chat-log")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                // Redraw while scrolling so the jump button shows and hides.
                .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                .child(log),
        )
        .child(fade(true, TOP_FADE))
        .child(fade(false, BOTTOM_FADE))
        .children(jump);

    let body = div()
        .w(px(AI_PANEL_W))
        .h_full()
        .relative()
        .flex()
        .flex_col()
        .child(header)
        .child(log_area)
        .children(slash_menu)
        .children(question)
        .child(
            div()
                .flex_none()
                .mx(px(SP3))
                .mb(px(SP3))
                .flex()
                .flex_col()
                .rounded(px(r_lg()))
                .border_1()
                .border_color(if streaming || asking { accent_alpha(0.4) } else { border() })
                .bg(content(0.03))
                .children(chips)
                .child(div().px(px(SP1)).pt(px(SP1)).child(input_field))
                .child(toolbar),
        )
        .children(history_open.then(|| history_menu(chats, cx)));

    div()
        .id("ai-panel")
        .flex_none()
        .h_full()
        .overflow_hidden()
        .rounded(px(r_lg()))
        .bg(pane_bg(crate::appearance::Pane::Assistant))
        .border_1()
        .border_color(if ws.ai_open { border() } else { gpui::transparent_black() })
        .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| {
            state::attach_paths(paths.paths().to_vec(), cx);
            cx.stop_propagation();
        }))
        .child(body)
        .with_spring("ai-panel-width", motion::spring(motion::PANEL_SPRING, target), |d, w| d.w(px(w.max(0.))))
        .into_any_element()
}

/// Review / auto-apply, as one quiet toggle.
fn auto_switch(cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let auto = agent::agent(cx).auto_apply;
    div()
        .id("agent-auto")
        .h(px(24.))
        .px(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.08)))
        .child(icon(if auto { Icon::Check } else { Icon::Eye }, 12., if auto { pal.accent } else { pal.faint }))
        .tooltip(widgets::tip(if auto { "Auto-apply: edits land at once (one undo step); deletions still wait" } else { "Review: edits wait for you on a card" }, None))
        .on_click(cx.listener(|_, _, _, cx| agent::toggle_auto(cx)))
        .into_any_element()
}

/// The project's chats, newest first, under the header.
fn history_menu(chats: Vec<history::Chat>, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let current = state::get(cx).read(cx).chat_id;
    let now = history::now();
    let mut list = div().id("chat-history-list").max_h(px(320.)).overflow_y_scroll().flex().flex_col();
    if chats.is_empty() {
        list = list.child(div().p(px(SP2)).child(label("No chats in this project yet. They are saved with the project file.", TEXT_SM, pal.faint)));
    }
    for (k, c) in chats.into_iter().enumerate() {
        let id = c.id;
        let on = id == current;
        list = list.child(
            div()
                .id(("chat-row", k))
                .group("chat-row")
                .h(px(30.))
                .pl(px(SP2))
                .pr(px(SP1))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .when(on, |d| d.bg(accent_alpha(0.12)))
                .hover(|s| s.bg(hover()))
                .child(icon(Icon::Chat, 12., if on { pal.accent } else { pal.faint }))
                .child(label(crate::paint::fit(&c.title, 170., TEXT_SM), TEXT_SM, pal.text).flex_1().min_w(px(0.)))
                .child(label(history::ago(c.updated, now), TEXT_XS, pal.faint))
                .child(
                    div().opacity(0.).group_hover("chat-row", |s| s.opacity(1.)).child(
                        widgets::sized_icon_button(("chat-delete", k), Icon::Trash, false, 20., 11.).tooltip(widgets::tip("Delete this chat", None)).on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            state::delete_chat(ws, id, cx);
                        })),
                    ),
                )
                .on_click(cx.listener(move |ws, _, _, cx| state::open_chat(ws, id, cx))),
        );
    }
    div()
        .absolute()
        .top(px(HEADER_H))
        .right(px(SP2))
        .occlude()
        .on_mouse_down_out(cx.listener(|_, _, _, cx| state::close_history(cx)))
        .child(motion::rise_in(
            "chat-history-in",
            widgets::menu_card("chat-history", AI_PANEL_W - 2. * SP2)
                .child(div().flex().items_center().child(widgets::menu_header("This project's chats")).child(div().flex_1()).child(
                    div()
                        .id("history-new")
                        .h(px(22.))
                        .px(px(SP2))
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .rounded(px(r_md()))
                        .cursor_pointer()
                        .hover(|s| s.bg(hover()))
                        .child(icon(Icon::Add, 10., pal.muted))
                        .child(label("New chat", TEXT_XS, pal.muted))
                        .on_click(cx.listener(|ws, _, _, cx| state::new_chat(ws, cx))),
                ))
                .child(list),
            4.,
        ))
        .into_any_element()
}

/// The `/` menu: tabs, then every matching action as a card.
fn slash_menu(q: &str, tab: slash::Tab, sel: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let hits = slash::filter(q, tab);
    let sel = sel.min(hits.len().saturating_sub(1));
    let mut tabs = div().flex().gap(px(SP1)).px(px(SP1));
    for (i, t) in slash::Tab::ALL.into_iter().enumerate() {
        let on = t == tab;
        tabs = tabs.child(
            div()
                .id(("slash-tab", i))
                .h(px(20.))
                .px(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.18) } else { gpui::transparent_black() })
                .hover(|s| s.bg(content(0.08)))
                .child(label(t.label(), TEXT_XS, if on { pal.text } else { pal.muted }))
                .on_click(cx.listener(move |_, _, _, cx| state::set_tab(t, cx))),
        );
    }
    let mut list = div().id("slash-list").max_h(px(264.)).overflow_y_scroll().flex().flex_col().gap(px(SP1));
    if hits.is_empty() {
        list = list.child(div().p(px(SP2)).child(label("No action matches. Press Enter to do nothing, or clear the slash to chat.", TEXT_SM, pal.faint)));
    }
    for (i, e) in hits.iter().enumerate() {
        let on = i == sel;
        let action = e.action;
        list = list.child(
            div()
                .id(("slash", i))
                .p(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_lg()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.14) } else { pal.surface })
                .border_1()
                .border_color(if on { accent_alpha(0.5) } else { border() })
                .hover(|s| s.border_color(accent_alpha(0.5)))
                .child(div().size(px(28.)).flex_none().flex().items_center().justify_center().rounded(px(r_md())).bg(accent_alpha(0.14)).child(icon(e.icon, 14., pal.accent)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(div().flex().items_center().gap(px(SP2)).child(label(e.title, TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM)).child(mono(format!("/{}", e.command), TEXT_XS, pal.faint)))
                        .child(label(e.hint, TEXT_XS, pal.muted)),
                )
                .on_click(cx.listener(move |ws, _, _, cx| state::choose(ws, action, cx))),
        );
    }
    div()
        .flex_none()
        .mx(px(SP3))
        .mb(px(SP2))
        .p(px(SP2))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(pal.raised)
        .border_1()
        .border_color(border_strong())
        .shadow_lg()
        .child(tabs)
        .child(list)
        .into_any_element()
}

/// The animated loader shown until the first token arrives.
fn thinking(who: Option<&str>, id: usize) -> impl IntoElement {
    let pal = p();
    let cycle = motion::NORMAL * 5;
    let dot = move |i: usize| {
        div().size(px(6.)).rounded_full().bg(pal.accent).with_animation(("think-dot", id * 8 + i), Animation::new(cycle).repeat().with_easing(motion::ease(motion::EMPHASIZED)), move |d, t| {
            // Each dot peaks a little after the one before it.
            let phase = (t + 1. - i as f32 * 0.22).fract();
            let lift = 1. - (phase * 2. - 1.).abs();
            d.opacity(0.25 + 0.75 * lift).mt(px(-2. * lift))
        })
    };
    div().h(px(18.)).flex().items_center().gap(px(SP1)).child(dot(0)).child(dot(1)).child(dot(2)).child(div().w(px(SP1))).child(label(who.map_or_else(|| "Thinking…".to_owned(), |w| format!("{w} is thinking…")), TEXT_SM, pal.muted))
}

/// An answer: full width, no avatar.
fn reply_row(body: AnyElement) -> AnyElement {
    div().w_full().min_w(px(0.)).overflow_hidden().child(body).into_any_element()
}

fn item(i: usize, it: Item, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let _ = cx;
    match it {
        Item::User { text, chips } => {
            let mut col = div().w_full().min_w(px(0.)).flex().flex_col().items_end().gap(px(SP1));
            if !chips.is_empty() {
                let mut row = div().flex().flex_wrap().justify_end().gap(px(SP1)).max_w(px(CARD_W - 24.));
                for (ic, name) in chips {
                    row = row.child(div().h(px(20.)).px(px(SP2)).gap(px(SP1)).flex().items_center().rounded(px(r_md())).bg(content(0.06)).child(icon(ic, 10., pal.muted)).child(label(crate::paint::fit(&name, 120., TEXT_XS), TEXT_XS, pal.muted)));
                }
                col = col.child(row);
            }
            col.child(
                div()
                    .max_w(px(CARD_W - 24.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .px(px(SP3))
                    .py(px(SP2))
                    .rounded(px(r_lg()))
                    .bg(content(0.07))
                    .child(div().min_w(px(0.)).text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).child(wrap_text(&text))),
            )
            .into_any_element()
        }
        Item::Reply { text, model, state, mix, is_part } => {
            // A part's reply is its JSON spec: the card below shows the result.
            let shown = if is_part { String::new() } else { state::visible_reply(&text, mix.is_some()).to_owned() };
            let body: AnyElement = match &state {
                ReplyState::Waiting => thinking(None, i).into_any_element(),
                ReplyState::Streaming => markdown::view(100_000 + i, &shown, true, cx),
                _ => markdown::view(100_000 + i, &shown, false, cx),
            };
            let footer = match &state {
                ReplyState::Stopped => Some(label("Stopped", TEXT_XS, pal.faint)),
                ReplyState::Failed(m) if !m.is_empty() => Some(label(wrap_text(m), TEXT_SM, pal.record).whitespace_normal()),
                ReplyState::Failed(_) => Some(label("No answer", TEXT_SM, pal.record)),
                ReplyState::Done if mix.is_some() => Some(label("Plan below as a proposal", TEXT_XS, pal.faint)),
                ReplyState::Done if is_part => Some(label("Part below", TEXT_XS, pal.faint)),
                _ => None,
            };
            let col = div()
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP1))
                .child(label(model, TEXT_XS, pal.faint))
                .when(!(matches!(state, ReplyState::Failed(_)) && shown.is_empty()), |d| d.child(body))
                .children(footer);
            reply_row(col.into_any_element())
        }
        Item::Action { icon: ic, title, detail } => div()
            .w_full()
            .min_w(px(0.))
            .overflow_hidden()
            .px(px(SP3))
            .py(px(SP2))
            .gap(px(SP2))
            .flex()
            .items_start()
            .rounded(px(r_lg()))
            .border_1()
            .border_color(accent_alpha(0.3))
            .bg(accent_alpha(0.06))
            .child(div().size(px(24.)).flex_none().flex().items_center().justify_center().rounded(px(r_md())).bg(accent_alpha(0.14)).child(icon(ic, 12., pal.accent)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().min_w(px(0.)).text_size(px(fs(TEXT_XS))).line_height(px(15.)).text_color(pal.muted).child(wrap_text(&detail))),
            )
            .into_any_element(),
        Item::Agent(r) => steps::view(i, &r, cx),
        Item::Note(text) => reply_row(div().w_full().min_w(px(0.)).text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.muted).child(wrap_text(&text)).into_any_element()),
    }
}

fn license_card(i: usize, notice: ModelNotice, status: ProposalStatus, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let size = notice.size.map_or("unknown size".to_owned(), |b| format!("{:.0} MB", b as f64 / 1_000_000.));
    let line = |k: &'static str, v: String| div().min_w(px(0.)).flex().gap(px(SP2)).child(label(k, TEXT_XS, pal.faint).w(px(56.)).flex_none()).child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.text).child(wrap_text(&v)));
    div()
        .w_full()
        .min_w(px(0.))
        .overflow_hidden()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(if status == ProposalStatus::Pending { accent_alpha(0.45) } else { border() })
        .child(div().flex().items_center().gap(px(SP2)).child(icon(Icon::Split, 14., pal.accent)).child(label("Model download", TEXT_XS, pal.accent).font_weight(FontWeight::SEMIBOLD)))
        .child(div().min_w(px(0.)).text_size(px(fs(TEXT_MD))).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(wrap_text(&notice.name)))
        .child(line("Size", size))
        .child(line("License", notice.license.clone()))
        .child(line("Source", notice.source.clone()))
        .when_some(notice.notice.clone(), |d, n| d.child(div().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.muted).child(wrap_text(&n))))
        .child(match status {
            ProposalStatus::Pending => div()
                .flex()
                .flex_wrap()
                .gap(px(SP2))
                .child(text_button(("lic-accept", i), "Accept & download", Some(Icon::Check), true).on_click(cx.listener(move |this, _, _, cx| this.resolve_license(i, true, cx))))
                .child(text_button(("lic-cancel", i), "Cancel", Some(Icon::X), false).on_click(cx.listener(move |this, _, _, cx| this.resolve_license(i, false, cx))))
                .into_any_element(),
            ProposalStatus::Accepted => label("Accepted · downloading with the job below", TEXT_SM, pal.play).whitespace_normal().into_any_element(),
            ProposalStatus::Discarded => label("Cancelled", TEXT_SM, pal.faint).into_any_element(),
        })
        .into_any_element()
}

/// A card from the workspace's log (jobs, results, proposals, licences).
/// The card of a generated part: what it is, what can be held, and the
/// buttons that depend on where it stands (offered, being listened to, kept).
fn part_card(c: PartCard, cx: &mut Context<Workspace>) -> AnyElement {
    use part::Status;
    let pal = p();
    let sid = c.sid;
    let open = matches!(c.status, Status::Pending | Status::Trying | Status::Undone);
    let act = |action: PartAction| cx.listener(move |this: &mut Workspace, _, _, cx| state::part_action(this, sid, action, cx));
    let mut list = div().w_full().min_w(px(0.)).flex().flex_col().gap(px(SP1));
    for s in c.summary.iter().take(6) {
        list = list.child(
            div()
                .min_w(px(0.))
                .flex()
                .items_start()
                .gap(px(SP2))
                .child(div().mt(px(6.)).size(px(4.)).flex_none().rounded_full().bg(pal.faint))
                .child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.muted).child(wrap_text(s))),
        );
    }
    let mut card = div()
        .w_full()
        .min_w(px(0.))
        .overflow_hidden()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(if open { accent_alpha(0.45) } else { border() })
        .child(
            div()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(icon(Icon::Wand, 14., pal.accent))
                .child(label("Part", TEXT_XS, pal.accent).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(badge(format!("Seed {}", c.seed), pal.faint))
                .child(badge(c.origin.clone(), pal.muted)),
        )
        .child(div().min_w(px(0.)).text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(wrap_text(&c.label)))
        .child(list);
    if let Some(n) = &c.notice {
        card = card.child(label(format!("{n}; the rules wrote this instead"), TEXT_XS, pal.record).whitespace_normal());
    }
    if c.destructive {
        card = card.child(label("Replaces the notes of the clip — review before keeping", TEXT_XS, pal.record).whitespace_normal());
    }
    if open && !c.busy {
        if c.layers.len() > 1 || (c.layers.len() == 1 && c.bars.is_empty()) {
            let mut row = div().flex().flex_wrap().items_center().gap(px(SP1)).child(label("Hold", TEXT_XS, pal.faint));
            for (i, (role, held)) in c.layers.iter().enumerate() {
                row = row.child(text_button(("part-layer", sid as usize * 64 + i), role.label(), held.then_some(Icon::Lock), *held).on_click(act(PartAction::HoldLayer(*role))));
            }
            card = card.child(row);
        }
        if !c.bars.is_empty() {
            let mut row = div().flex().flex_wrap().items_center().gap(px(SP1)).child(label("Bars", TEXT_XS, pal.faint));
            for (b, held) in c.bars.iter().enumerate() {
                row = row.child(text_button(("part-bar", sid as usize * 64 + b), (b + 1).to_string(), held.then_some(Icon::Lock), *held).on_click(act(PartAction::HoldBar(b as u32))));
            }
            card = card.child(row);
        }
    }
    let main = match c.status {
        Status::Pending => div()
            .flex()
            .flex_wrap()
            .gap(px(SP2))
            .child(text_button(("part-listen", sid as usize), "Listen", Some(Icon::Headphones), true).on_click(act(PartAction::Listen)))
            .child(text_button(("part-accept", sid as usize), "Accept", Some(Icon::Check), false).on_click(act(PartAction::Accept)))
            .child(text_button(("part-discard", sid as usize), "Discard", Some(Icon::X), false).on_click(act(PartAction::Discard)))
            .into_any_element(),
        Status::Trying => div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(SP2))
            .child(text_button(("part-keep", sid as usize), "Keep", Some(Icon::Check), true).on_click(act(PartAction::Keep)))
            .child(text_button(("part-undo", sid as usize), "Undo", Some(Icon::Undo), false).on_click(act(PartAction::Undo)))
            .child(label("Playing on the track · Cmd+Z also removes it", TEXT_XS, pal.faint).whitespace_normal())
            .into_any_element(),
        Status::Undone => div().flex().flex_wrap().items_center().gap(px(SP2)).child(label("Taken off with Cmd+Z", TEXT_SM, pal.faint)).child(text_button(("part-discard", sid as usize), "Close", Some(Icon::X), false).on_click(act(PartAction::Discard))).into_any_element(),
        Status::Accepted => div().flex().items_center().gap(px(SP1)).child(icon(Icon::Check, 12., pal.play)).child(label("Kept · one undo step", TEXT_SM, pal.play)).into_any_element(),
        Status::Discarded => label("Discarded", TEXT_SM, pal.faint).into_any_element(),
    };
    card = card.child(main);
    if open {
        card = card.child(if c.busy {
            label("Writing another take…", TEXT_SM, pal.accent).into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(px(SP2))
                .child(text_button(("part-vary", sid as usize), "Vary", Some(Icon::Dice), false).on_click(act(PartAction::Vary)))
                .child(text_button(("part-regen", sid as usize), "Regenerate", Some(Icon::Refresh), false).on_click(act(PartAction::Regenerate)))
                .into_any_element()
        });
    }
    card.into_any_element()
}

fn entry(i: usize, e: ChatEntry, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    match e {
        ChatEntry::License { notice, status, .. } => license_card(i, notice, status, cx),
        ChatEntry::Assistant(text) => reply_row(markdown::view(200_000 + i, &text, false, cx)),
        ChatEntry::Job { label: text, fraction, done, .. } => div()
            .w_full()
            .min_w(px(0.))
            .overflow_hidden()
            .p(px(SP3))
            .gap(px(SP2))
            .flex()
            .flex_col()
            .rounded(px(r_lg()))
            .bg(pal.surface)
            .border_1()
            .border_color(border())
            .child(
                div()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap(px(SP2))
                    .child(icon(if done { Icon::Check } else { Icon::Loader }, 14., if done { pal.play } else { pal.accent }))
                    .child(label(text, TEXT_SM, pal.text).flex_1().min_w(px(0.)).truncate())
                    .child(mono(format!("{:.0}%", fraction * 100.), TEXT_XS, pal.muted).flex_none()),
            )
            .child(widgets::hbar(fraction, CARD_W - 2. * SP3 - 2., 4., if done { pal.play } else { pal.accent }))
            .into_any_element(),
        ChatEntry::Samples { title, items } => {
            let mut card = div()
                .w_full()
                .min_w(px(0.))
                .overflow_hidden()
                .p(px(SP2))
                .gap(px(SP1))
                .flex()
                .flex_col()
                .rounded(px(r_lg()))
                .bg(pal.surface)
                .border_1()
                .border_color(border())
                .child(div().min_w(px(0.)).px(px(SP1)).flex().items_center().gap(px(SP2)).child(icon(Icon::Search, 12., pal.accent)).child(label(title, TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM).flex_1().min_w(px(0.)).truncate()));
            if items.is_empty() {
                card = card.child(div().px(px(SP1)).child(label("Nothing in the library matches yet.", TEXT_SM, pal.faint).whitespace_normal()));
            }
            for (j, it) in items.iter().enumerate() {
                let name = it.name.clone();
                let sample = it.clone();
                card = card.child(
                    div()
                        .id(("sample-hit", i * 100 + j))
                        .min_w(px(0.))
                        .h(px(ROW_H))
                        .px(px(SP1))
                        .gap(px(SP2))
                        .flex()
                        .items_center()
                        .rounded(px(r_md()))
                        .cursor_pointer()
                        .hover(|s| s.bg(hover()))
                        .child(icon(if it.is_loop { Icon::Loop } else { Icon::Waveform }, 12., pal.muted))
                        .child(label(it.name.clone(), TEXT_SM, pal.text).flex_1().min_w(px(0.)).truncate())
                        .when_some(it.key, |d, k| d.child(badge(short_key(k), pal.accent)))
                        .when_some(it.bpm, |d, b| d.child(badge(format!("{b:.0}"), pal.solo)))
                        .child(
                            widgets::sized_icon_button(("sample-attach", i * 100 + j), Icon::Link, false, 20., 12.)
                                .tooltip(widgets::tip("Attach to the next message", None))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    state::attach_sample(&sample, cx);
                                })),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.toast(format!("Preview: {name}"), cx))),
                );
            }
            card.into_any_element()
        }
        ChatEntry::Part(c) => part_card(c, cx),
        ChatEntry::Proposal { id, label: title, origin, summary, destructive, status } => {
            let pending = status == ProposalStatus::Pending;
            let mut list = div().w_full().min_w(px(0.)).flex().flex_col().gap(px(SP1));
            for s in summary.iter().take(8) {
                list = list.child(
                    div()
                        .min_w(px(0.))
                        .flex()
                        .items_start()
                        .gap(px(SP2))
                        .child(div().mt(px(6.)).size(px(4.)).flex_none().rounded_full().bg(pal.faint))
                        .child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.muted).child(wrap_text(s))),
                );
            }
            if summary.len() > 8 {
                list = list.child(div().pl(px(SP3)).child(label(format!("+{} more", summary.len() - 8), TEXT_XS, pal.faint)));
            }
            div()
                .w_full()
                .min_w(px(0.))
                .overflow_hidden()
                .p(px(SP3))
                .gap(px(SP2))
                .flex()
                .flex_col()
                .rounded(px(r_lg()))
                .bg(pal.surface)
                .border_1()
                .border_color(if pending { accent_alpha(0.45) } else { border() })
                .child(
                    div()
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .child(icon(Icon::Wand, 14., pal.accent))
                        .child(label("Proposal", TEXT_XS, pal.accent).font_weight(FontWeight::SEMIBOLD))
                        .child(div().flex_1())
                        .child(badge(origin, pal.muted)),
                )
                .child(div().min_w(px(0.)).text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(wrap_text(&title)))
                .child(list)
                .when(destructive, |d| d.child(label("Deletes content — review before accepting", TEXT_XS, pal.record).whitespace_normal()))
                .child(match status {
                    ProposalStatus::Pending => div()
                        .flex()
                        .flex_wrap()
                        .gap(px(SP2))
                        .child(text_button(("accept", id as usize), "Accept", Some(Icon::Check), true).on_click(cx.listener(move |this, _, _, cx| this.resolve_proposal(id, true, cx))))
                        .child(text_button(("discard", id as usize), "Discard", Some(Icon::X), false).on_click(cx.listener(move |this, _, _, cx| this.resolve_proposal(id, false, cx))))
                        .into_any_element(),
                    ProposalStatus::Accepted => div().flex().items_center().gap(px(SP1)).child(icon(Icon::Check, 12., pal.play)).child(label("Accepted · one undo step", TEXT_SM, pal.play)).into_any_element(),
                    ProposalStatus::Discarded => label("Discarded", TEXT_SM, pal.faint).into_any_element(),
                })
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_text_breaks_long_identifiers_without_touching_short_words() {
        assert_eq!(wrap_text("Short_name stays intact"), "Short_name stays intact");
        let long = wrap_text("789_TN_V1_124_OS_MUSIC_LOOP_2");
        assert!(long.contains('\u{200B}'), "long underscore sample name gets soft break points: {long:?}");
        assert_eq!(long.replace('\u{200B}', ""), "789_TN_V1_124_OS_MUSIC_LOOP_2");
    }
}
