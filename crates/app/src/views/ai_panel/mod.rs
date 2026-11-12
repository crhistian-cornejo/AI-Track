//! The assistant panel: one chat. It slides in on a spring; replies stream
//! in token by token from the chosen model (Claude through the Anthropic
//! API, or the user's own agent CLI). Typing `/` opens every action (extract
//! MIDI, split stems, import, analyse, find samples, mix this track…) as
//! cards; what an action does shows up as cards in the chat: job progress,
//! results, and proposals accepted or discarded through `History::accept` /
//! `discard`. Tracks, clips, samples and screenshots ride along as chips.

mod mix;
pub mod slash;
pub mod state;

use crate::bridge::{AgentKind, ModelNotice};
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::views::browser::short_key;
use crate::widgets::{self, badge, label, mono, text_button};
use crate::workspace::{ChatEntry, ProposalStatus, Workspace};
use aitrack_ai::llm;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, ExternalPaths, FontWeight, HighlightStyle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, StyledText, Window, div, prelude::FluentBuilder, px,
};
use state::{Backend, Item, KeyState, Placed, ReplyState};

/// Whether the assistant panel needs a frame every vsync. The loader and
/// the caret animate on their own and the stream polls on a timer, so no.
pub fn wants_frames(_ws: &Workspace) -> bool {
    false
}

const HEADER_H: f32 = TAB_BAR_H + SP2;
/// Inner width of a card in the log.
const CARD_W: f32 = AI_PANEL_W - 2. * SP3;

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let a = state::get(cx);
    let target = if ws.ai_open { AI_PANEL_W } else { 0. };
    let (backend, key, menu_open, streaming, items, attachments, scroll, input, sel, tab) = {
        let s = a.read(cx);
        (s.backend, s.key, s.model_menu, s.streaming(), s.items.clone(), s.attachments.clone(), s.scroll.clone(), s.input.clone(), s.slash_sel, s.slash_tab)
    };
    let text = input.read(cx).text().to_owned();

    let ready = match backend {
        Backend::Claude(_) => matches!(key, KeyState::Env | KeyState::Keychain),
        Backend::Agent(kind) => ws.b.ai.agents().iter().any(|s| s.kind == kind && s.installed),
    };
    let header = div()
        .flex_none()
        .h(px(HEADER_H))
        .px(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(icon(Icon::Sparkles, 16., pal.accent))
        .child(label("Assistant", TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1())
        .child(
            div()
                .id("model-pick")
                .h(px(24.))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(if menu_open { content(0.12) } else { content(0.05) })
                .hover(|s| s.bg(content(0.12)))
                .child(div().size(px(6.)).rounded_full().bg(if ready { pal.play } else { content(0.25) }))
                .child(label(backend.name(), TEXT_XS, pal.text))
                .child(icon(Icon::ChevronDown, 12., pal.muted))
                .tooltip(widgets::tip("Model", None))
                .on_click(cx.listener(|ws, _, _, cx| state::toggle_model_menu(ws, cx))),
        )
        .child(widgets::icon_button("close-ai", Icon::X, false).tooltip(widgets::tip("Close", Some("Ctrl+I"))).on_click(cx.listener(|this, _, _, cx| {
            this.ai_open = false;
            cx.notify();
        })));

    // The log: our items interleaved with the workspace's cards.
    let mut log = div().flex().flex_col().gap(px(SP3)).p(px(SP3));
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

    let chips = (!attachments.is_empty()).then(|| {
        let mut row = div().flex_none().px(px(SP3)).pt(px(SP2)).flex().flex_wrap().gap(px(SP1));
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

    let send_or_stop = if streaming {
        widgets::icon_button("stop-chat", Icon::Stop, true).tooltip(widgets::tip("Stop", Some("Esc"))).on_click(cx.listener(|ws, _, _, cx| state::stop(ws, cx)))
    } else {
        let input = input.clone();
        widgets::icon_button("send-chat", Icon::Send, false).tooltip(widgets::tip("Send", Some("Enter"))).on_click(cx.listener(move |ws, _, _, cx| {
            let text = input.read(cx).text().trim().to_owned();
            if text.is_empty() {
                return;
            }
            input.update(cx, |i, cx| i.set_text("", cx));
            if slash::query(&text).is_none() {
                state::send(ws, text, None, cx);
            }
        }))
    };
    let menu_is_open = slash::query(&text).is_some();
    let input_row = div()
        .flex_none()
        .p(px(SP3))
        .gap(px(SP1))
        .flex()
        .items_center()
        .child(widgets::icon_button("attach-file", Icon::Add, false).tooltip(widgets::tip("Attach a sample or a screenshot", None)).on_click(cx.listener(|_, _, _, cx| state::pick_files(cx))))
        .child(widgets::icon_button("attach-sel", Icon::Link, false).tooltip(widgets::tip("Attach the selected track and clip", None)).on_click(cx.listener(|ws, _, _, cx| state::attach_selection(ws, cx))))
        .child(
            div()
                .id("chat-input")
                .flex_1()
                .min_w(px(0.))
                // Keep the keys: the workspace takes focus back on clicks outside its own fields.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(move |ws, e: &KeyDownEvent, _, cx| {
                    let k = &e.keystroke;
                    let handled = match k.key.as_str() {
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
                        "escape" if state::get(cx).read(cx).streaming() => {
                            state::stop(ws, cx);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                    }
                }))
                .child(input.clone()),
        )
        .child(send_or_stop);

    let hint = match (backend, key) {
        (Backend::Claude(_), KeyState::Missing) => "No API key · answers say so · / actions still work".to_owned(),
        (Backend::Claude(_), KeyState::Checking) => "Looking for an API key…".to_owned(),
        (Backend::Claude(_), KeyState::Env) => "Key from ANTHROPIC_API_KEY · / for actions · ⌘V pastes a screenshot".to_owned(),
        (Backend::Claude(_), KeyState::Keychain) => "Key in the keychain · / for actions · ⌘V pastes a screenshot".to_owned(),
        (Backend::Agent(k), _) => format!("{} on this computer, with your own sign-in · / for actions", k.name()),
    };
    let footer = div().flex_none().px(px(SP3)).pb(px(SP2)).child(label(hint, TEXT_XS, pal.faint));

    let body = div()
        .w(px(AI_PANEL_W))
        .h_full()
        .relative()
        .flex()
        .flex_col()
        .child(header)
        .child(div().id("chat-log").flex_1().min_h(px(0.)).overflow_y_scroll().track_scroll(&scroll).child(log))
        .children(slash_menu)
        .child(div().flex_none().border_t_1().border_color(border()).children(chips).child(input_row).child(footer))
        .children(menu_open.then(|| model_menu(ws, backend, key, cx)));

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

/// The model picker: the Claude models, the user's agent CLIs, the key.
fn model_menu(ws: &Workspace, current: Backend, key: KeyState, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let mut card = widgets::menu_card("model-menu", 264.).child(widgets::menu_header("Claude"));
    for (i, m) in llm::MODELS.iter().enumerate() {
        card = card.child(
            widgets::menu_item(("model", i), Some(Icon::Sparkles), m.name, None, current == Backend::Claude(i))
                .tooltip(widgets::tip(m.hint, None))
                .on_click(cx.listener(move |ws, _, _, cx| state::set_backend(ws, Backend::Claude(i), cx))),
        );
    }
    card = card.child(widgets::menu_header("Your agent CLI"));
    let found = ws.b.ai.agents();
    for (i, kind) in AgentKind::ALL.into_iter().enumerate() {
        let status = found.iter().find(|s| s.kind == kind);
        let installed = status.is_some_and(|s| s.installed);
        let el = widgets::menu_item(("agent", i), Some(Icon::Bot), kind.name(), if installed { None } else { Some("not found") }, current == Backend::Agent(kind));
        card = card.child(if installed {
            el.on_click(cx.listener(move |ws, _, _, cx| state::set_backend(ws, Backend::Agent(kind), cx)))
        } else {
            el.opacity(0.6).on_click(cx.listener(move |ws, _, _, cx| {
                state::note(ws, format!("{} Then pick {} again. AI-Track only runs that program; it never sees your account.", kind.setup(), kind.name()), cx);
                state::toggle_model_menu(ws, cx);
            }))
        });
    }
    let status = match key {
        KeyState::Checking => "Checking…",
        KeyState::Env => "From ANTHROPIC_API_KEY",
        KeyState::Keychain => "Stored in the system keychain",
        KeyState::Missing => "None: answers say no model is configured",
    };
    card = card
        .child(widgets::menu_header("Anthropic API key"))
        .child(div().px(px(SP2)).pb(px(SP1)).child(label(status, TEXT_XS, pal.muted)))
        .child(widgets::menu_item("key-paste", Some(Icon::Paste), "Use API key from clipboard", None, false).on_click(cx.listener(|ws, _, _, cx| {
            state::key_from_clipboard(ws, cx);
            state::toggle_model_menu(ws, cx);
        })));
    if key == KeyState::Keychain {
        card = card.child(widgets::menu_item("key-forget", Some(Icon::Trash), "Forget the stored key", None, false).on_click(cx.listener(|ws, _, _, cx| {
            state::forget_key(cx);
            state::toggle_model_menu(ws, cx);
        })));
    }
    div()
        .id("model-menu-wrap")
        .absolute()
        .top(px(HEADER_H - SP1))
        .right(px(SP3))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(motion::rise_in("model-menu-in", card, 4.))
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

/// Streaming text with a blinking caret at its end.
fn streaming_text(text: String, id: usize) -> AnyElement {
    let pal = p();
    div()
        .text_size(px(fs(TEXT_MD)))
        .line_height(px(18.))
        .text_color(pal.text)
        .with_animation(("caret", id), Animation::new(motion::NORMAL * 4).repeat(), move |d, t| {
            let caret = "▍";
            let full = format!("{text}{caret}");
            let start = text.len();
            let alpha = if t < 0.5 { 1. } else { 0.15 };
            d.child(StyledText::new(full).with_highlights([(start..start + caret.len(), HighlightStyle { color: Some(pal.accent.opacity(alpha)), ..Default::default() })]))
        })
        .into_any_element()
}

fn reply_row(body: AnyElement) -> AnyElement {
    let pal = p();
    div()
        .flex()
        .gap(px(SP2))
        .child(div().size(px(24.)).flex_none().flex().items_center().justify_center().rounded_full().bg(content(0.06)).child(icon(Icon::Bot, 14., pal.accent)))
        .child(div().flex_1().min_w(px(0.)).child(body))
        .into_any_element()
}

fn item(i: usize, it: Item, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let _ = cx;
    match it {
        Item::User { text, chips } => {
            let mut col = div().flex().flex_col().items_end().gap(px(SP1));
            if !chips.is_empty() {
                let mut row = div().flex().flex_wrap().justify_end().gap(px(SP1)).max_w(px(CARD_W - 32.));
                for (ic, name) in chips {
                    row = row.child(div().h(px(20.)).px(px(SP2)).gap(px(SP1)).flex().items_center().rounded(px(r_md())).bg(content(0.06)).child(icon(ic, 10., pal.muted)).child(label(crate::paint::fit(&name, 120., TEXT_XS), TEXT_XS, pal.muted)));
                }
                col = col.child(row);
            }
            col.child(div().max_w(px(CARD_W - 32.)).px(px(SP3)).py(px(SP2)).rounded(px(r_lg())).bg(accent_alpha(0.16)).child(div().text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).child(text)))
                .into_any_element()
        }
        Item::Reply { text, model, state, mix } => {
            let shown = state::visible_reply(&text, mix.is_some()).to_owned();
            let body: AnyElement = match &state {
                ReplyState::Waiting => thinking(None, i).into_any_element(),
                ReplyState::Streaming => streaming_text(shown.clone(), i),
                _ => div().text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).child(shown.clone()).into_any_element(),
            };
            let footer = match &state {
                ReplyState::Stopped => Some(label("Stopped", TEXT_XS, pal.faint)),
                ReplyState::Failed(m) if !m.is_empty() => Some(label(m.clone(), TEXT_SM, pal.record)),
                ReplyState::Failed(_) => Some(label("No answer", TEXT_SM, pal.record)),
                ReplyState::Done if mix.is_some() => Some(label("Plan below as a proposal", TEXT_XS, pal.faint)),
                _ => None,
            };
            let col = div()
                .flex()
                .flex_col()
                .gap(px(SP1))
                .child(label(model, TEXT_XS, pal.faint))
                .when(!(matches!(state, ReplyState::Failed(_)) && shown.is_empty()), |d| d.child(body))
                .children(footer);
            reply_row(col.into_any_element())
        }
        Item::Action { icon: ic, title, detail } => div()
            .px(px(SP3))
            .py(px(SP2))
            .gap(px(SP2))
            .flex()
            .items_center()
            .rounded(px(r_lg()))
            .border_1()
            .border_color(accent_alpha(0.3))
            .bg(accent_alpha(0.06))
            .child(div().size(px(24.)).flex_none().flex().items_center().justify_center().rounded(px(r_md())).bg(accent_alpha(0.14)).child(icon(ic, 12., pal.accent)))
            .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(label(title, TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM)).child(label(crate::paint::fit(&detail, CARD_W - 64., TEXT_XS), TEXT_XS, pal.muted)))
            .into_any_element(),
        Item::Note(text) => reply_row(div().text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.muted).child(text).into_any_element()),
    }
}

fn license_card(i: usize, notice: ModelNotice, status: ProposalStatus, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let size = notice.size.map_or("unknown size".to_owned(), |b| format!("{:.0} MB", b as f64 / 1_000_000.));
    let line = |k: &'static str, v: String| div().flex().gap(px(SP2)).child(label(k, TEXT_XS, pal.faint).w(px(56.))).child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_SM))).text_color(pal.text).child(v));
    div()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(if status == ProposalStatus::Pending { accent_alpha(0.45) } else { border() })
        .child(div().flex().items_center().gap(px(SP2)).child(icon(Icon::Split, 14., pal.accent)).child(label("Model download", TEXT_XS, pal.accent).font_weight(FontWeight::SEMIBOLD)))
        .child(div().text_size(px(fs(TEXT_MD))).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(notice.name.clone()))
        .child(line("Size", size))
        .child(line("License", notice.license.clone()))
        .child(line("Source", notice.source.clone()))
        .when_some(notice.notice.clone(), |d, n| d.child(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.muted).child(n)))
        .child(match status {
            ProposalStatus::Pending => div()
                .flex()
                .gap(px(SP2))
                .child(text_button(("lic-accept", i), "Accept & download", Some(Icon::Check), true).on_click(cx.listener(move |this, _, _, cx| this.resolve_license(i, true, cx))))
                .child(text_button(("lic-cancel", i), "Cancel", Some(Icon::X), false).on_click(cx.listener(move |this, _, _, cx| this.resolve_license(i, false, cx))))
                .into_any_element(),
            ProposalStatus::Accepted => label("Accepted · downloading with the job below", TEXT_SM, pal.play).into_any_element(),
            ProposalStatus::Discarded => label("Cancelled", TEXT_SM, pal.faint).into_any_element(),
        })
        .into_any_element()
}

/// A card from the workspace's log (jobs, results, proposals, licences).
fn entry(i: usize, e: ChatEntry, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    match e {
        ChatEntry::License { notice, status, .. } => license_card(i, notice, status, cx),
        ChatEntry::Assistant(text) => reply_row(div().text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).child(text).into_any_element()),
        ChatEntry::Job { label: text, fraction, done, .. } => div()
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
                    .flex()
                    .items_center()
                    .gap(px(SP2))
                    .child(icon(if done { Icon::Check } else { Icon::Loader }, 14., if done { pal.play } else { pal.accent }))
                    .child(label(text, TEXT_SM, pal.text).flex_1())
                    .child(mono(format!("{:.0}%", fraction * 100.), TEXT_XS, pal.muted)),
            )
            .child(widgets::hbar(fraction, CARD_W - 2. * SP3 - 2., 4., if done { pal.play } else { pal.accent }))
            .into_any_element(),
        ChatEntry::Samples { title, items } => {
            let mut card = div()
                .p(px(SP2))
                .gap(px(SP1))
                .flex()
                .flex_col()
                .rounded(px(r_lg()))
                .bg(pal.surface)
                .border_1()
                .border_color(border())
                .child(div().px(px(SP1)).flex().items_center().gap(px(SP2)).child(icon(Icon::Search, 12., pal.accent)).child(label(title, TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM)));
            if items.is_empty() {
                card = card.child(div().px(px(SP1)).child(label("Nothing in the library matches yet.", TEXT_SM, pal.faint)));
            }
            for (j, it) in items.iter().enumerate() {
                let name = it.name.clone();
                let sample = it.clone();
                card = card.child(
                    div()
                        .id(("sample-hit", i * 100 + j))
                        .h(px(ROW_H))
                        .px(px(SP1))
                        .gap(px(SP2))
                        .flex()
                        .items_center()
                        .rounded(px(r_md()))
                        .cursor_pointer()
                        .hover(|s| s.bg(hover()))
                        .child(icon(if it.is_loop { Icon::Loop } else { Icon::Waveform }, 12., pal.muted))
                        .child(label(crate::paint::fit(&it.name, 130., TEXT_SM), TEXT_SM, pal.text).flex_1())
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
        ChatEntry::Proposal { id, label: title, origin, summary, destructive, status } => {
            let pending = status == ProposalStatus::Pending;
            let mut list = div().flex().flex_col().gap(px(2.));
            for s in summary.iter().take(8) {
                list = list.child(div().flex().items_center().gap(px(SP2)).child(div().size(px(4.)).flex_none().rounded_full().bg(pal.faint)).child(label(s.clone(), TEXT_SM, pal.muted)));
            }
            div()
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
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .child(icon(Icon::Wand, 14., pal.accent))
                        .child(label("Proposal", TEXT_XS, pal.accent).font_weight(FontWeight::SEMIBOLD))
                        .child(div().flex_1())
                        .child(badge(origin, pal.muted)),
                )
                .child(div().text_size(px(fs(TEXT_MD))).text_color(pal.text).font_weight(FontWeight::MEDIUM).child(title))
                .child(list)
                .when(destructive, |d| d.child(label("Deletes content — review before accepting", TEXT_XS, pal.record)))
                .child(match status {
                    ProposalStatus::Pending => div()
                        .flex()
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
