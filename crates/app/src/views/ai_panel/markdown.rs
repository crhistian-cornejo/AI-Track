//! Paints an assistant reply's Markdown (`aitrack_ai::llm::markdown`) as
//! GPUI elements: paragraphs and headings with inline styles, nested and
//! task lists, code cards with a copy button, quotes, tables and rules.
//! While a reply streams, its unfinished tail is mended first and a caret
//! blinks at the end of the last block.

use std::ops::Range;

use aitrack_ai::llm::markdown::{self as md, Align, Block, ListItem, Span};
use gpui::{
    Animation, AnimationExt, AnyElement, ClipboardItem, Context, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveElement, InteractiveText,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, StrikethroughStyle, Styled, StyledText, UnderlineStyle, div, prelude::FluentBuilder, px,
};

use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::widgets::{self, label};
use crate::workspace::Workspace;

const CARET: &str = "▍";

/// A reply's Markdown. `key` keeps element ids unique per reply; `streaming`
/// mends the unfinished tail and draws the caret.
pub fn view(key: usize, text: &str, streaming: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let blocks = md::parse(text, streaming);
    let mut r = Render { key, seq: 0 };
    let pal = p();
    let mut col = div().w_full().min_w(px(0.)).flex().flex_col().gap(px(SP2));
    if blocks.is_empty() && streaming {
        col = col.child(r.text(&[], Tone::body(pal.text), true));
    }
    let n = blocks.len();
    for (i, b) in blocks.iter().enumerate() {
        col = col.child(r.block(b, pal.text, streaming && i + 1 == n, cx));
    }
    col.into_any_element()
}

/// Size, line height, colour and weight of a run of text.
#[derive(Clone, Copy)]
struct Tone {
    size: f32,
    line: f32,
    color: Hsla,
    weight: FontWeight,
}

impl Tone {
    fn body(color: Hsla) -> Self {
        Self { size: TEXT_MD, line: 18., color, weight: FontWeight::NORMAL }
    }
}

struct Render {
    key: usize,
    /// Counts the elements that need an id, so ids stay unique per reply.
    seq: usize,
}

/// The text of `spans` and its styles as highlight ranges.
struct Runs {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    mono: Vec<(Range<usize>, SharedString)>,
    links: Vec<(Range<usize>, String)>,
}

fn styled(spans: &[Span], tone: Tone) -> Runs {
    let pal = p();
    let mut s = Runs { text: String::new(), highlights: Vec::new(), mono: Vec::new(), links: Vec::new() };
    for sp in spans {
        let start = s.text.len();
        s.text.push_str(&super::wrap_text(&sp.text));
        let range = start..s.text.len();
        if range.is_empty() {
            continue;
        }
        let mut h = HighlightStyle::default();
        let mut any = false;
        if sp.bold {
            h.font_weight = Some(FontWeight::SEMIBOLD);
            any = true;
        }
        if sp.italic {
            h.font_style = Some(FontStyle::Italic);
            any = true;
        }
        if sp.strike {
            h.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: Some(pal.muted) });
            h.color = Some(pal.muted);
            any = true;
        }
        if sp.code {
            h.background_color = Some(content(0.08));
            h.color = Some(tone.color);
            s.mono.push((range.clone(), FONT_MONO.into()));
            any = true;
        }
        if let Some(url) = &sp.link {
            h.color = Some(pal.accent);
            h.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(accent_alpha(0.5)), wavy: false });
            s.links.push((range.clone(), url.clone()));
            any = true;
        }
        if any {
            s.highlights.push((range, h));
        }
    }
    s
}

fn text_box(tone: Tone) -> gpui::Div {
    div().w_full().min_w(px(0.)).text_size(px(fs(tone.size))).line_height(px(tone.line)).text_color(tone.color).font_weight(tone.weight)
}

/// The caret's blink: on for the first half of each cycle.
fn caret_alpha(t: f32) -> f32 {
    if t < 0.5 { 1. } else { 0.15 }
}

impl Render {
    fn next_id(&mut self) -> usize {
        self.seq += 1;
        self.key * 4096 + self.seq
    }

    /// Inline text; with `caret` a blinking ▍ follows it.
    fn text(&mut self, spans: &[Span], tone: Tone, caret: bool) -> AnyElement {
        let s = styled(spans, tone);
        let id = self.next_id();
        if caret {
            let Runs { text, highlights, mono, .. } = s;
            let accent = p().accent;
            return text_box(tone)
                .with_animation(("md-caret", id), Animation::new(motion::NORMAL * 4).repeat(), move |d, t| {
                    let start = text.len();
                    let full = format!("{text}{CARET}");
                    let mut hl = highlights.clone();
                    hl.push((start..start + CARET.len(), HighlightStyle { color: Some(accent.opacity(caret_alpha(t))), ..Default::default() }));
                    d.child(StyledText::new(full).with_highlights(hl).with_font_family_overrides(mono.clone()))
                })
                .into_any_element();
        }
        let Runs { text, highlights, mono, links } = s;
        let st = StyledText::new(text).with_highlights(highlights).with_font_family_overrides(mono);
        if links.is_empty() {
            return text_box(tone).child(st).into_any_element();
        }
        let (ranges, urls): (Vec<_>, Vec<_>) = links.into_iter().unzip();
        text_box(tone)
            .child(InteractiveText::new(("md-link", id), st).on_click(ranges, move |i, _, cx| {
                if let Some(url) = urls.get(i) {
                    cx.open_url(url);
                }
            }))
            .into_any_element()
    }

    fn block(&mut self, b: &Block, color: Hsla, caret: bool, cx: &mut Context<Workspace>) -> AnyElement {
        let pal = p();
        match b {
            Block::Paragraph(spans) => self.text(spans, Tone::body(color), caret),
            Block::Heading { level, spans } => {
                let (size, line) = match level {
                    1 => (TEXT_LG + 2., 24.),
                    2 => (TEXT_LG, 20.),
                    _ => (TEXT_MD, 18.),
                };
                let tone = Tone { size, line, color, weight: FontWeight::SEMIBOLD };
                div().when(*level <= 2, |d| d.pt(px(SP1))).child(self.text(spans, tone, caret)).into_any_element()
            }
            Block::Code { lang, code } => self.code(lang.as_deref(), code, caret, cx),
            Block::List { start, items } => self.list(*start, items, color, caret, cx),
            Block::Quote(inner) => {
                let mut col = div().w_full().min_w(px(0.)).flex().flex_col().gap(px(SP2));
                let n = inner.len();
                for (i, b) in inner.iter().enumerate() {
                    col = col.child(self.block(b, pal.muted, caret && i + 1 == n, cx));
                }
                div().pl(px(SP3)).border_l_2().border_color(accent_alpha(0.4)).child(col).into_any_element()
            }
            Block::Table { align, header, rows } => {
                let table = self.table(align, header, rows);
                if caret {
                    div().flex().flex_col().gap(px(SP1)).child(table).child(self.text(&[], Tone::body(color), true)).into_any_element()
                } else {
                    table
                }
            }
            Block::Rule => div().my(px(SP1)).h(px(1.)).bg(border()).into_any_element(),
        }
    }

    fn list(&mut self, start: Option<u64>, items: &[ListItem], color: Hsla, caret: bool, cx: &mut Context<Workspace>) -> AnyElement {
        let pal = p();
        let digits = start.map_or(1, |s| (s + items.len() as u64).max(1).ilog10() as usize + 1);
        let number_w = (8. * digits as f32 + SP2).max(SP4);
        let mut col = div().flex().flex_col().gap(px(SP1)).min_w(px(0.));
        let n = items.len();
        for (i, it) in items.iter().enumerate() {
            let marker = match (it.task, start) {
                (Some(done), _) => div()
                    .w(px(SP4))
                    .h(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .size(px(12.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(r_sm()))
                            .border_1()
                            .border_color(if done { accent_alpha(0.6) } else { border_strong() })
                            .when(done, |d| d.bg(accent_alpha(0.2)).child(icon(Icon::Check, 10., pal.accent))),
                    ),
                (None, Some(s)) => div().w(px(number_w)).h(px(18.)).flex_none().flex().items_center().justify_end().pr(px(SP1)).child(label(format!("{}.", s + i as u64), TEXT_MD, pal.muted)),
                (None, None) => div().w(px(SP3)).h(px(18.)).flex_none().flex().items_center().child(div().size(px(4.)).rounded_full().bg(pal.muted)),
            };
            let mut body = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(SP1));
            let m = it.blocks.len();
            let done_task = it.task == Some(true);
            for (j, b) in it.blocks.iter().enumerate() {
                let tone = if done_task { pal.muted } else { color };
                body = body.child(self.block(b, tone, caret && i + 1 == n && j + 1 == m, cx));
            }
            if it.blocks.is_empty() && caret && i + 1 == n {
                body = body.child(self.text(&[], Tone::body(color), true));
            }
            col = col.child(div().flex().gap(px(SP1)).child(marker).child(body));
        }
        col.into_any_element()
    }

    fn code(&mut self, lang: Option<&str>, code: &str, caret: bool, cx: &mut Context<Workspace>) -> AnyElement {
        let pal = p();
        let id = self.next_id();
        let copy = code.to_owned();
        let header = div()
            .h(px(24.))
            .pl(px(SP2))
            .pr(px(SP1))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(border())
            .child(label(lang.unwrap_or("code").to_owned(), TEXT_XS, pal.faint).flex_1())
            .child(widgets::sized_icon_button(("md-copy", id), Icon::Copy, false, 20., 12.).tooltip(widgets::tip("Copy", None)).on_click(cx.listener(move |ws, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                ws.toast("Copied", cx);
            })));
        let tone = Tone { size: TEXT_SM, line: 16., color: pal.text, weight: FontWeight::NORMAL };
        let mut lines = div().flex().flex_col().font_family(FONT_MONO);
        let all: Vec<&str> = code.split('\n').collect();
        let n = all.len();
        for (i, line) in all.into_iter().enumerate() {
            // An empty line still takes its height.
            let shown = if line.is_empty() { " ".to_owned() } else { line.to_owned() };
            let row = text_box(tone).whitespace_nowrap();
            lines = lines.child(if caret && i + 1 == n {
                let accent = pal.accent;
                let text = if line.is_empty() { String::new() } else { shown };
                row.with_animation(("md-code-caret", id), Animation::new(motion::NORMAL * 4).repeat(), move |d, t| {
                    let start = text.len();
                    let full = format!("{text}{CARET}");
                    d.child(StyledText::new(full).with_highlights([(start..start + CARET.len(), HighlightStyle { color: Some(accent.opacity(caret_alpha(t))), ..Default::default() })]))
                })
                .into_any_element()
            } else {
                row.child(shown).into_any_element()
            });
        }
        div()
            .flex()
            .flex_col()
            .min_w(px(0.))
            .rounded(px(r_md()))
            .bg(readout_bg())
            .border_1()
            .border_color(border())
            .child(header)
            .child(div().id(("md-code-scroll", id)).overflow_x_scroll().px(px(SP2)).py(px(SP2)).child(lines))
            .into_any_element()
    }

    fn table(&mut self, align: &[Align], header: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) -> AnyElement {
        let pal = p();
        let id = self.next_id();
        let columns = header.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
        // Column widths from the longest cell, in whole 4 px steps.
        let widths: Vec<f32> = (0..columns)
            .map(|c| {
                let chars = std::iter::once(header.get(c)).chain(rows.iter().map(|r| r.get(c))).flatten().map(|cell| md::plain(cell).chars().count()).max().unwrap_or(0);
                let w = (chars as f32 * 6.5 + 2. * SP2).clamp(48., 200.);
                (w / 4.).ceil() * 4.
            })
            .collect();
        let empty: Vec<Span> = Vec::new();
        let mut grid = div().flex().flex_col().flex_none().rounded(px(r_md())).border_1().border_color(border());
        let all_rows = std::iter::once((true, header)).chain(rows.iter().map(|r| (false, r.as_slice())));
        let total = rows.len() + 1;
        for (ri, (head, cells)) in all_rows.enumerate() {
            let mut row = div().flex().when(ri + 1 < total, |d| d.border_b_1().border_color(border())).when(head, |d| d.bg(content(0.04)));
            for (c, &w) in widths.iter().enumerate() {
                let cell = cells.get(c).unwrap_or(&empty);
                let tone = Tone { size: TEXT_SM, line: 16., color: if head { pal.text } else { pal.muted }, weight: if head { FontWeight::SEMIBOLD } else { FontWeight::NORMAL } };
                let text = self.text(cell, tone, false);
                let a = align.get(c).copied().unwrap_or(Align::None);
                row = row.child(
                    div()
                        .w(px(w))
                        .flex_none()
                        .px(px(SP2))
                        .py(px(SP1))
                        .flex()
                        .when(a == Align::Right, |d| d.justify_end().text_right())
                        .when(a == Align::Center, |d| d.justify_center().text_center())
                        .when(c + 1 < columns, |d| d.border_r_1().border_color(border()))
                        .child(text),
                );
            }
            grid = grid.child(row);
        }
        div().id(("md-table", id)).min_w(px(0.)).overflow_x_scroll().child(grid).into_any_element()
    }
}
