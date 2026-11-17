//! Markdown for the assistant's replies: a small block model parsed with
//! pulldown-cmark, and the repairs a half-streamed reply needs (an open
//! fence, an unclosed `**`) so it renders as it will look when complete.
//!
//! [`parse`] turns text into [`Block`]s. With `streaming` it first runs
//! [`mend`] over the unfinished tail: an open code fence gets its closer,
//! unbalanced emphasis, strikethrough and code spans are closed, a half link
//! shows as its text, and a lone list marker, heading `#` or a table row
//! whose header is not complete yet stays hidden until more arrives.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::iter::Peekable;

/// A run of text with one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

impl Span {
    fn same_style(&self, other: &Span) -> bool {
        self.bold == other.bold && self.italic == other.italic && self.code == other.code && self.strike == other.strike && self.link == other.link
    }
}

/// A table column's alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// One list item: a task checkbox (`- [ ]` / `- [x]`) or none, and its blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading { level: u8, spans: Vec<Span> },
    Code { lang: Option<String>, code: String },
    /// `start` is the first number of an ordered list, `None` for bullets.
    List { start: Option<u64>, items: Vec<ListItem> },
    Quote(Vec<Block>),
    Table { align: Vec<Align>, header: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
    Rule,
}

/// The text of `spans` without styles.
pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Parse `text`; `streaming` mends the unfinished tail first (see [`mend`]).
pub fn parse(text: &str, streaming: bool) -> Vec<Block> {
    let source = if streaming { mend(text) } else { text.to_owned() };
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut b = Builder { ev: Parser::new_ext(&source, options).peekable(), task: None };
    b.blocks(None)
}

// ───────────────────────────── Building blocks ─────────────────────────────

struct Builder<'a> {
    ev: Peekable<Parser<'a>>,
    /// The task marker seen inside the current list item.
    task: Option<bool>,
}

fn is_inline(e: &Event<'_>) -> bool {
    match e {
        Event::Text(_) | Event::Code(_) | Event::SoftBreak | Event::HardBreak | Event::InlineHtml(_) | Event::InlineMath(_) | Event::FootnoteReference(_) | Event::TaskListMarker(_) => true,
        Event::Start(t) => matches!(t, Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Superscript | Tag::Subscript | Tag::Link { .. } | Tag::Image { .. }),
        Event::End(t) => matches!(t, TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Superscript | TagEnd::Subscript | TagEnd::Link | TagEnd::Image),
        _ => false,
    }
}

impl Builder<'_> {
    /// Blocks until `until` ends (or the input does).
    fn blocks(&mut self, until: Option<TagEnd>) -> Vec<Block> {
        let mut out = Vec::new();
        while let Some(peek) = self.ev.peek() {
            if let Event::End(e) = peek {
                let e = *e;
                self.ev.next();
                if Some(e) == until {
                    break;
                }
                continue;
            }
            if is_inline(peek) {
                // Tight list items hold their text without a paragraph.
                let spans = self.inlines(None);
                if !spans.is_empty() {
                    out.push(Block::Paragraph(spans));
                }
                continue;
            }
            let Some(ev) = self.ev.next() else { break };
            match ev {
                Event::Start(Tag::Paragraph) => {
                    let spans = self.inlines(Some(TagEnd::Paragraph));
                    if !spans.is_empty() {
                        out.push(Block::Paragraph(spans));
                    }
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    let spans = self.inlines(Some(TagEnd::Heading(level)));
                    out.push(Block::Heading { level: level as u8, spans });
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info.split_whitespace().next().filter(|l| !l.is_empty()).map(str::to_owned),
                        CodeBlockKind::Indented => None,
                    };
                    let mut code = String::new();
                    for e in self.ev.by_ref() {
                        match e {
                            Event::Text(t) => code.push_str(&t),
                            Event::End(TagEnd::CodeBlock) => break,
                            _ => {}
                        }
                    }
                    let code = code.trim_end_matches('\n').to_owned();
                    out.push(Block::Code { lang, code });
                }
                Event::Start(Tag::List(start)) => {
                    let items = self.items(start.is_some());
                    out.push(Block::List { start, items });
                }
                Event::Start(Tag::BlockQuote(kind)) => {
                    let inner = self.blocks(Some(TagEnd::BlockQuote(kind)));
                    out.push(Block::Quote(inner));
                }
                Event::Start(Tag::Table(align)) => out.push(self.table(align)),
                Event::Start(Tag::HtmlBlock) => {
                    let mut text = String::new();
                    for e in self.ev.by_ref() {
                        match e {
                            Event::Html(t) | Event::Text(t) => text.push_str(&t),
                            Event::End(TagEnd::HtmlBlock) => break,
                            _ => {}
                        }
                    }
                    let text = text.trim_end().to_owned();
                    if !text.is_empty() {
                        out.push(Block::Paragraph(vec![Span { text, ..Default::default() }]));
                    }
                }
                Event::Start(t) => {
                    // Footnotes, definition lists, metadata: their content, flat.
                    let end = t.to_end();
                    out.extend(self.blocks(Some(end)));
                }
                Event::Rule => out.push(Block::Rule),
                Event::Html(t) | Event::Text(t) | Event::DisplayMath(t) => out.push(Block::Paragraph(vec![Span { text: t.trim_end().to_owned(), ..Default::default() }])),
                _ => {}
            }
        }
        out
    }

    fn items(&mut self, ordered: bool) -> Vec<ListItem> {
        let mut items = Vec::new();
        while let Some(ev) = self.ev.next() {
            match ev {
                Event::Start(Tag::Item) => {
                    let outer = self.task.take();
                    let blocks = self.blocks(Some(TagEnd::Item));
                    let task = std::mem::replace(&mut self.task, outer);
                    items.push(ListItem { task, blocks });
                }
                Event::End(TagEnd::List(o)) if o == ordered => break,
                _ => {}
            }
        }
        items
    }

    fn table(&mut self, align: Vec<Alignment>) -> Block {
        let align = align
            .into_iter()
            .map(|a| match a {
                Alignment::None => Align::None,
                Alignment::Left => Align::Left,
                Alignment::Center => Align::Center,
                Alignment::Right => Align::Right,
            })
            .collect();
        let mut header = Vec::new();
        let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
        let mut in_head = false;
        while let Some(ev) = self.ev.next() {
            match ev {
                Event::Start(Tag::TableHead) => in_head = true,
                Event::End(TagEnd::TableHead) => in_head = false,
                Event::Start(Tag::TableRow) => rows.push(Vec::new()),
                Event::Start(Tag::TableCell) => {
                    let cell = self.inlines(Some(TagEnd::TableCell));
                    if in_head {
                        header.push(cell);
                    } else if let Some(row) = rows.last_mut() {
                        row.push(cell);
                    }
                }
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        Block::Table { align, header, rows }
    }

    /// Inline runs until `end` (consumed), or with `None` until the next
    /// event that is not inline (left in place).
    fn inlines(&mut self, end: Option<TagEnd>) -> Vec<Span> {
        let mut out: Vec<Span> = Vec::new();
        let (mut bold, mut italic, mut strike) = (0u32, 0u32, 0u32);
        let mut links: Vec<String> = Vec::new();
        while let Some(peek) = self.ev.peek() {
            if end.is_none() && !is_inline(peek) {
                break;
            }
            let Some(ev) = self.ev.next() else { break };
            let mut push = |text: &str, code: bool| {
                let span = Span { text: text.to_owned(), bold: bold > 0, italic: italic > 0, code, strike: strike > 0, link: links.last().cloned() };
                match out.last_mut() {
                    Some(last) if last.same_style(&span) => last.text.push_str(text),
                    _ if text.is_empty() => {}
                    _ => out.push(span),
                }
            };
            match ev {
                Event::End(e) if Some(e) == end => break,
                Event::Text(t) | Event::InlineHtml(t) | Event::InlineMath(t) | Event::Html(t) => push(&t, false),
                Event::Code(t) => push(&t, true),
                Event::SoftBreak => push(" ", false),
                Event::HardBreak => push("\n", false),
                Event::FootnoteReference(r) => push(&format!("[{r}]"), false),
                Event::TaskListMarker(done) => self.task = Some(done),
                Event::Start(Tag::Strong) => bold += 1,
                Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
                Event::Start(Tag::Emphasis) => italic += 1,
                Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
                Event::Start(Tag::Strikethrough) => strike += 1,
                Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
                Event::Start(Tag::Link { dest_url, .. }) => links.push(dest_url.into_string()),
                Event::End(TagEnd::Link) => {
                    links.pop();
                }
                // An image shows as its alt text.
                _ => {}
            }
        }
        out
    }
}

// ───────────────────────────── Mending a streamed tail ─────────────────────────────

/// Make a half-received reply render as it will once complete: close an open
/// code fence; in the last paragraph close unbalanced `**`, `*`, `_`, `~~`
/// and code spans, show a half link as its text, and hide a lone trailing
/// list marker, heading `#` or a table whose header is not complete yet.
pub fn mend(text: &str) -> String {
    let mut fence: Option<(u8, usize)> = None;
    // Where the last run of prose starts: after a blank line or a fence's end.
    let mut prose_start = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.as_bytes().first().copied().unwrap_or(0);
        let width = trimmed.bytes().take_while(|b| *b == marker).count();
        let is_fence = indent <= 3 && matches!(marker, b'`' | b'~') && width >= 3;
        let end = offset + line.len();
        match fence {
            Some((m, w)) => {
                if is_fence && marker == m && width >= w && trimmed[width..].trim().is_empty() {
                    fence = None;
                    prose_start = end;
                }
            }
            None => {
                if is_fence && (marker == b'~' || !trimmed[width..].contains('`')) {
                    fence = Some((marker, width));
                } else if line.trim().is_empty() {
                    prose_start = end;
                }
            }
        }
        offset = end;
    }
    if let Some((m, w)) = fence {
        let mut s = text.to_owned();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.extend(std::iter::repeat_n(m as char, w));
        return s;
    }
    let (head, tail) = text.split_at(prose_start);
    let mut out = head.to_owned();
    out.push_str(&mend_tail(tail));
    out
}

fn list_marker(t: &str) -> Option<usize> {
    let b = t.as_bytes();
    match b.first() {
        Some(b'-' | b'*' | b'+') => Some(1),
        Some(c) if c.is_ascii_digit() => {
            let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            matches!(b.get(n), Some(b'.' | b')')).then_some(n + 1)
        }
        _ => None,
    }
}

/// A line that would render as an empty or misleading stub on its own.
fn is_stub(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    if t.bytes().all(|b| b == b'#') && t.len() <= 6 || t == ">" {
        return true;
    }
    match list_marker(t) {
        Some(n) => {
            let rest = t[n..].trim();
            rest.is_empty() || matches!(rest, "[" | "[ " | "[ ]" | "[x" | "[x]" | "[X" | "[X]")
        }
        None => false,
    }
}

fn cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').collect()
}

fn is_delimiter_row(line: &str, columns: usize) -> bool {
    let c = cells(line);
    c.len() == columns
        && c.iter().all(|cell| {
            let s = cell.trim();
            let s = s.strip_prefix(':').unwrap_or(s);
            let s = s.strip_suffix(':').unwrap_or(s);
            !s.is_empty() && s.bytes().all(|b| b == b'-')
        })
}

fn mend_tail(tail: &str) -> String {
    let mut lines: Vec<&str> = tail.split_inclusive('\n').collect();
    // A table row without its delimiter row reads as a line of pipes.
    if let Some(i) = lines.iter().position(|l| l.trim_start().starts_with('|')) {
        let columns = cells(lines[i]).len();
        if !lines.get(i + 1).is_some_and(|d| is_delimiter_row(d, columns)) {
            lines.truncate(i);
        }
    }
    if lines.last().is_some_and(|l| is_stub(l)) {
        lines.pop();
    }
    let mut s: String = lines.concat();
    mend_link(&mut s);
    strip_trailing_opener(&mut s);
    balance(&s)
}

/// `[text` → `text`, `[text](https://…` → `text`, a final `[text]` → `text`.
fn mend_link(s: &mut String) {
    let Some(open) = s.rfind('[') else { return };
    if open > 0 && s.as_bytes()[open - 1] == b'\\' {
        return;
    }
    let bang = open > 0 && s.as_bytes()[open - 1] == b'!';
    let start = if bang { open - 1 } else { open };
    let after = &s[open + 1..];
    match after.find(']') {
        None => {
            if after.contains('\n') {
                return;
            }
            let text = after.to_owned();
            s.truncate(start);
            s.push_str(&text);
        }
        Some(close) => {
            let label = after[..close].to_owned();
            let rest = &after[close + 1..];
            let half_url = rest.starts_with('(') && !rest.contains(')');
            let bare = rest.trim_end().is_empty() && !label.is_empty() && !matches!(label.as_str(), " " | "x" | "X");
            if half_url || bare {
                s.truncate(start);
                s.push_str(&label);
            }
        }
    }
}

const MARKS: &[u8] = b"*_~`";

/// A run of marks at the very end, after a space, opens nothing yet: drop it.
fn strip_trailing_opener(s: &mut String) {
    let t = s.trim_end().len();
    let run = s.as_bytes()[..t].iter().rev().take_while(|b| MARKS.contains(b)).count();
    if run == 0 {
        return;
    }
    let before = s[..t - run].chars().next_back();
    if before.is_none_or(char::is_whitespace) {
        s.truncate(t - run);
    }
}

/// Close what the tail left open: a code span, then emphasis and strikethrough.
fn balance(s: &str) -> String {
    let b = s.as_bytes();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut code: Option<usize> = None;
    let mut i = 0;
    let mut line_start = true;
    let char_len = |i: usize| s[i..].chars().next().map_or(1, char::len_utf8);
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if line_start {
            line_start = false;
            let t = s[i..].trim_start();
            // A new block on this line: what the last one left open stays there.
            if code.is_none() && (t.starts_with('#') || t.starts_with('>') || t.starts_with('|') || list_marker(t).is_some_and(|n| t[n..].starts_with(' '))) {
                stack.clear();
            }
        }
        if c == b'\\' {
            i += 1;
            if i < b.len() {
                i += char_len(i);
            }
            continue;
        }
        if c == b'`' {
            let n = b[i..].iter().take_while(|x| **x == b'`').count();
            match code {
                Some(w) if w == n => code = None,
                None => code = Some(n),
                _ => {}
            }
            i += n;
            continue;
        }
        if code.is_some() {
            i += char_len(i);
            continue;
        }
        if matches!(c, b'*' | b'_' | b'~') {
            let n = b[i..].iter().take_while(|x| **x == c).count();
            let prev = s[..i].chars().next_back();
            let next = s[i + n..].chars().next();
            let opens = next.is_some_and(|ch| !ch.is_whitespace());
            let closes = prev.is_some_and(|ch| !ch.is_whitespace());
            let intraword = c == b'_' && prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric);
            if !intraword {
                if closes && stack.last().is_some_and(|(m, _)| *m == c) {
                    stack.pop();
                } else if opens {
                    stack.push((c, n.min(3)));
                }
            }
            i += n;
            continue;
        }
        i += char_len(i);
    }
    let mut closers = String::new();
    if let Some(w) = code {
        closers.extend(std::iter::repeat_n('`', w));
    } else {
        for &(m, n) in stack.iter().rev() {
            let n = if m == b'~' { n.min(2) } else { n };
            closers.extend(std::iter::repeat_n(m as char, n));
        }
    }
    let mut out = s.to_owned();
    let at = out.trim_end().len();
    out.insert_str(at, &closers);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> Span {
        Span { text: text.into(), ..Default::default() }
    }

    #[test]
    fn inline_styles_become_spans() {
        let b = parse("Cut **the mud** at *300 Hz*, try `-3 dB`, ~~boost~~ and [read](https://x.y).", false);
        let Block::Paragraph(s) = &b[0] else { panic!("{b:?}") };
        assert_eq!(plain(s), "Cut the mud at 300 Hz, try -3 dB, boost and read.");
        assert!(s.iter().any(|x| x.bold && x.text == "the mud"));
        assert!(s.iter().any(|x| x.italic && x.text == "300 Hz"));
        assert!(s.iter().any(|x| x.code && x.text == "-3 dB"));
        assert!(s.iter().any(|x| x.strike && x.text == "boost"));
        assert!(s.iter().any(|x| x.link.as_deref() == Some("https://x.y") && x.text == "read"));
    }

    #[test]
    fn breaks_headings_rules_and_quotes() {
        let b = parse("## Low end\nline one\nline two  \nline three\n\n---\n\n> keep the kick\n> mono", false);
        assert_eq!(b[0], Block::Heading { level: 2, spans: vec![span("Low end")] });
        assert_eq!(b[1], Block::Paragraph(vec![span("line one line two\nline three")]));
        assert_eq!(b[2], Block::Rule);
        assert_eq!(b[3], Block::Quote(vec![Block::Paragraph(vec![span("keep the kick mono")])]));
    }

    #[test]
    fn nested_lists_and_tasks() {
        let b = parse("1. Kick\n   - low cut\n   - punch\n2. Bass\n\n- [x] gain staging\n- [ ] master", false);
        let Block::List { start: Some(1), items } = &b[0] else { panic!("{b:?}") };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocks[0], Block::Paragraph(vec![span("Kick")]));
        let Block::List { start: None, items: inner } = &items[0].blocks[1] else { panic!("{items:?}") };
        assert_eq!(inner.len(), 2);
        let Block::List { items: tasks, .. } = &b[1] else { panic!("{b:?}") };
        assert_eq!(tasks[0].task, Some(true));
        assert_eq!(tasks[1].task, Some(false));
        assert_eq!(tasks[1].blocks[0], Block::Paragraph(vec![span("master")]));
    }

    #[test]
    fn tables_and_code_fences() {
        let b = parse("| Band | Gain |\n|:--|--:|\n| 60 Hz | **+2** |\n\n```rust\nlet x = 1;\n```", false);
        let Block::Table { align, header, rows } = &b[0] else { panic!("{b:?}") };
        assert_eq!(align, &vec![Align::Left, Align::Right]);
        assert_eq!(plain(&header[1]), "Gain");
        assert!(rows[0][1][0].bold);
        assert_eq!(b[1], Block::Code { lang: Some("rust".into()), code: "let x = 1;".into() });
    }

    #[test]
    fn streaming_closes_an_open_fence() {
        let b = parse("Here:\n```json\n{\"a\": 1", true);
        assert_eq!(b[1], Block::Code { lang: Some("json".into()), code: "{\"a\": 1".into() });
        assert_eq!(mend("```\ncode\n```\nafter **bold"), "```\ncode\n```\nafter **bold**");
    }

    #[test]
    fn streaming_closes_inline_marks() {
        assert_eq!(mend("cut **the mud"), "cut **the mud**");
        assert_eq!(mend("an *italic word"), "an *italic word*");
        assert_eq!(mend("try `-3 dB"), "try `-3 dB`");
        assert_eq!(mend("so ~~boost"), "so ~~boost~~");
        assert_eq!(mend("**bold *both"), "**bold *both***");
        assert_eq!(mend("snake_case_name stays"), "snake_case_name stays");
        assert_eq!(mend("done **ok** then "), "done **ok** then ");
        // A mark with nothing after it yet is dropped, not closed.
        assert_eq!(mend("more **"), "more ");
        assert_eq!(mend("`code` and `"), "`code` and ");
        let b = parse("cut **the mud", true);
        assert!(matches!(&b[0], Block::Paragraph(s) if s[1].bold && s[1].text == "the mud"));
    }

    #[test]
    fn streaming_hides_stubs_and_half_links() {
        assert_eq!(mend("Steps:\n- "), "Steps:\n");
        assert_eq!(mend("Steps:\n12."), "Steps:\n");
        assert_eq!(mend("intro\n\n##"), "intro\n\n");
        assert_eq!(mend("todo\n- [ ]"), "todo\n");
        assert_eq!(mend("see [the manual](https://exa"), "see the manual");
        assert_eq!(mend("see [the man"), "see the man");
        assert_eq!(mend("see [the manual]"), "see the manual");
        assert_eq!(mend("- [ ] task"), "- [ ] task");
        assert_eq!(mend("| a | b |\n|--"), "");
        assert_eq!(mend("text\n\n| a | b |\n|---|---|\n| 1 |"), "text\n\n| a | b |\n|---|---|\n| 1 |");
        let b = parse("| a | b |\n|---|---|\n| 1 | 2", true);
        assert!(matches!(&b[0], Block::Table { rows, .. } if rows.len() == 1));
    }

    #[test]
    fn every_prefix_parses_without_panicking() {
        let sample = "## Mezcla á ♪\n\nBaja **el bombo** a *-6 dB* y usa `EQ` — ~~no~~ [guía](https://x.y/á).\n\n\
                      1. Kick ♪\n   - corte\n2. Bajo\n\n- [x] hecho\n\n| Banda | dB |\n|:--|--:|\n| 60 Hz | +2 |\n\n\
                      > cita **á**\n\n```rust\nlet nota = \"♪\";\n```\n\n---\nfin_de_línea \\*x\\*";
        for (i, _) in sample.char_indices().chain([(sample.len(), ' ')]) {
            let prefix = &sample[..i];
            let _ = parse(prefix, true);
            let _ = parse(prefix, false);
        }
        assert!(!parse(sample, false).is_empty());
    }
}
