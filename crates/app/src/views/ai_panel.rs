//! The assistant panel: slides in on a spring, holds the chat, job
//! progress and proposal cards (Accept / Discard through
//! `History::accept` / `discard`), plus the built-in AI actions.

use crate::bridge::{MidiMode, ModelNotice};
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::views::browser::short_key;
use crate::widgets::{self, badge, label, mono, text_button};
use crate::workspace::{ChatEntry, ProposalStatus, Workspace};
use gpui::{
    AnimationExt, AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let target = if ws.ai_open { AI_PANEL_W } else { 0. };
    let inner_w = AI_PANEL_W;

    let header = div()
        .flex_none()
        .h(px(TAB_BAR_H + SP2))
        .px(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(icon(Icon::Sparkles, 16., pal.accent))
        .child(label("Assistant", TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
        .child(badge("local", pal.muted))
        .child(div().flex_1())
        .child(widgets::icon_button("close-ai", Icon::X, false).tooltip(widgets::tip("Close", Some("Ctrl+I"))).on_click(cx.listener(|this, _, _, cx| {
            this.ai_open = false;
            cx.notify();
        })));

    let actions = div()
        .flex_none()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(border())
        .child(action("ai-extract", Icon::Extract, "Extract MIDI from audio", "selected audio clip").on_click(cx.listener(|this, _, _, cx| this.extract_midi(cx))))
        .child(midi_mode(ws, cx))
        .child(action("ai-stems", Icon::Split, "Split stems", "vocals · drums · bass · other").on_click(cx.listener(|this, _, _, cx| this.split_stems(cx))))
        .child(action("ai-import", Icon::FolderOpen, "Import audio", "new track · detects key and BPM").on_click(cx.listener(|this, _, _, cx| this.pick_audio_file(cx))))
        .child(action("ai-samples", Icon::Search, "Find samples in key", "uses the project key").on_click(cx.listener(|this, _, _, cx| this.find_samples_in_key(cx))));

    let mut log = div().flex().flex_col().gap(px(SP3)).p(px(SP3));
    let entries = ws.chat_log.clone();
    let n = entries.len();
    for (i, e) in entries.into_iter().enumerate() {
        let el = entry(i, e, cx);
        // The newest entry rises in; the rest stay put.
        log = log.child(if i + 1 == n { motion::rise_in(("chat", i), div().child(el), 8.).into_any_element() } else { el });
    }

    let input = div()
        .flex_none()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .border_t_1()
        .border_color(border())
        .child(div().flex_1().child(ws.chat.clone()))
        .child(widgets::icon_button("send-chat", Icon::Send, false).tooltip(widgets::tip("Send", Some("Enter"))));

    let body = div()
        .w(px(inner_w))
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(actions)
        .child(div().id("chat-log").flex_1().min_h(px(0.)).overflow_y_scroll().child(log))
        .child(input);

    div()
        .flex_none()
        .h_full()
        .overflow_hidden()
        .rounded(px(r_lg()))
        .bg(pane_bg(crate::appearance::Pane::Assistant))
        .border_1()
        .border_color(if ws.ai_open { border() } else { gpui::transparent_black() })
        .child(body)
        .with_spring("ai-panel-width", motion::spring(motion::PANEL_SPRING, target), |d, w| d.w(px(w.max(0.))))
        .into_any_element()
}

/// Poly (basic-pitch) / Mono / Drums for "Extract MIDI".
fn midi_mode(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let weak = cx.entity().downgrade();
    let on: widgets::OnSelect = std::rc::Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |ws, cx| {
            ws.midi_mode = MidiMode::ALL[i];
            cx.notify();
        });
    });
    let items: Vec<_> = MidiMode::ALL.iter().map(|m| (None, m.label())).collect();
    let index = MidiMode::ALL.iter().position(|m| *m == ws.midi_mode).unwrap_or(0);
    div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .pl(px(SP3))
        .child(label("MIDI mode", TEXT_XS, pal.faint))
        .child(widgets::segmented("midi-mode", &items, index, 56., on))
        .into_any_element()
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

fn action(id: &'static str, i: Icon, title: &'static str, hint: &'static str) -> gpui::Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .h(px(40.))
        .px(px(SP3))
        .gap(px(SP3))
        .flex()
        .items_center()
        .rounded(px(r_lg()))
        .bg(pal.surface)
        .border_1()
        .border_color(border())
        .cursor_pointer()
        .hover(|s| s.border_color(accent_alpha(0.5)).bg(pal.raised))
        .child(div().size(px(24.)).flex().items_center().justify_center().rounded(px(r_md())).bg(accent_alpha(0.14)).child(icon(i, 14., pal.accent)))
        .child(div().flex().flex_col().child(label(title, TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM)).child(label(hint, TEXT_XS, pal.faint)))
}

fn entry(i: usize, e: ChatEntry, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    match e {
        ChatEntry::License { notice, status, .. } => license_card(i, notice, status, cx),
        ChatEntry::User(text) => div()
            .flex()
            .justify_end()
            .child(div().max_w(px(240.)).px(px(SP3)).py(px(SP2)).rounded(px(r_lg())).bg(accent_alpha(0.16)).child(div().text_size(px(fs(TEXT_MD))).text_color(pal.text).child(text)))
            .into_any_element(),
        ChatEntry::Assistant(text) => div()
            .flex()
            .gap(px(SP2))
            .child(div().size(px(24.)).flex_none().flex().items_center().justify_center().rounded_full().bg(content(0.06)).child(icon(Icon::Bot, 14., pal.accent)))
            .child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_MD))).line_height(px(18.)).text_color(pal.text).child(text))
            .into_any_element(),
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
            .child(widgets::hbar(fraction, AI_PANEL_W - 2. * SP3 - 2. * SP3 - 2., 4., if done { pal.play } else { pal.accent }))
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
            for (j, it) in items.iter().enumerate() {
                let name = it.name.clone();
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
                        .child(label(crate::paint::fit(&it.name, 150., TEXT_SM), TEXT_SM, pal.text).flex_1())
                        .when_some(it.key, |d, k| d.child(badge(short_key(k), pal.accent)))
                        .when_some(it.bpm, |d, b| d.child(badge(format!("{b:.0}"), pal.solo)))
                        .on_click(cx.listener(move |this, _, _, cx| this.toast(format!("Preview: {name}"), cx))),
                );
            }
            card.into_any_element()
        }
        ChatEntry::Proposal { id, label: title, origin, summary, destructive, status } => {
            let pending = status == ProposalStatus::Pending;
            let mut list = div().flex().flex_col().gap(px(2.));
            for s in summary.iter().take(5) {
                list = list.child(div().flex().items_center().gap(px(SP2)).child(div().size(px(4.)).rounded_full().bg(pal.faint)).child(label(s.clone(), TEXT_SM, pal.muted)));
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
