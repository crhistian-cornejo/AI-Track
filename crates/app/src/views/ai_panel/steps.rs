//! How an agent reply reads: thinking that opens, one line per tool call
//! (its icon, what it does and on what; a click opens its input and its
//! result), runs of lookups folded into one line, questions with their
//! answers, the Markdown text, and a quiet footer.

use aitrack_ai::llm::cli::Vendor;
use aitrack_ai::llm::tools;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div,
    prelude::FluentBuilder, px, svg,
};
use serde_json::Value;

use super::agent::{self, Reply, Step, ToolState, agent};
use super::markdown;
use super::state::{self, ReplyState};
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;

/// A vendor's logo on its brand tile.
pub fn logo(vendor: Vendor, side: f32) -> Div {
    let glyph = match vendor {
        Vendor::Anthropic => Icon::LogoClaude,
        Vendor::OpenAi => Icon::LogoOpenAi,
    };
    div()
        .size(px(side))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px((side * 0.3).round()))
        .bg(brand(vendor))
        .child(svg().path(glyph.path()).size(px(side * 0.86)).text_color(gpui::white()))
}

/// What a finished tool line says: the result, without the ids the model needs.
pub(super) fn plain(result: &str) -> String {
    let mut s = result.lines().next().unwrap_or_default().to_owned();
    // Drop "(device #41)", "(clip #44, placement #45)" and ": track #40" style tails.
    while let Some(open) = s.find(" (")
        && let Some(close) = s[open..].find(')').map(|c| open + c)
        && s[open..close].contains('#')
    {
        s.replace_range(open..=close, "");
    }
    for tail in [": track #", ": group #", " as device #"] {
        if let Some(i) = s.find(tail) {
            s.truncate(i);
        }
    }
    s
}

/// The tool's icon by what it works on.
fn tool_icon(name: &str) -> Icon {
    match name {
        n if n == tools::ASK_USER => Icon::Help,
        n if tools::is_read_only(n) => Icon::Search,
        "create_track" | "update_track" | "delete_track" | "group_tracks" => Icon::Layers,
        "add_device" | "remove_device" | "set_params" | "load_preset" => Icon::Sliders,
        "set_send" | "set_sidechain" => Icon::Plug,
        "set_song" => Icon::Metronome,
        "write_clip" | "generate_part" => Icon::Piano,
        "copy_clip" | "move_clip" | "clear_bars" => Icon::Timeline,
        "set_automation" => Icon::Automation,
        _ => Icon::Wand,
    }
}

/// A value in a line: strings as they are, numbers trimmed.
fn short(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.as_f64().map_or_else(|| n.to_string(), |f| if f.fract() == 0. { format!("{f:.0}") } else { format!("{f}") })),
        Value::Bool(b) => Some(if *b { "on".into() } else { "off".into() }),
        _ => None,
    }
}

/// What a call works on, from its input: "Bass · Acid 303", "Kick · 16 notes".
pub(super) fn detail(name: &str, input: &Value) -> String {
    let get = |k: &str| input.get(k).and_then(short);
    let mut parts: Vec<String> = Vec::new();
    if name == tools::ASK_USER {
        return get("question").unwrap_or_default();
    }
    for k in ["name", "track", "plugin", "preset", "kind", "to", "source", "query", "category", "description"] {
        if let Some(v) = get(k) {
            let v = if k == "track" && input[k].is_number() { format!("track #{v}") } else { v };
            if !parts.contains(&v) {
                parts.push(v);
            }
        }
    }
    if let Some(n) = input.get("notes").and_then(Value::as_array) {
        parts.push(format!("{} note{}", n.len(), if n.len() == 1 { "" } else { "s" }));
    }
    if let Some(n) = input.get("points").and_then(Value::as_array) {
        parts.push(format!("{} point{}", n.len(), if n.len() == 1 { "" } else { "s" }));
    }
    if let Some(p) = input.get("params").and_then(Value::as_object) {
        let keys: Vec<&str> = p.keys().map(String::as_str).take(3).collect();
        if !keys.is_empty() {
            parts.push(keys.join(", "));
        }
    }
    if let Some(b) = get("bpm") {
        parts.push(format!("{b} BPM"));
    }
    if let Some(k) = get("key") {
        parts.push(format!("{k} {}", get("scale").unwrap_or_default()).trim().to_owned());
    }
    if let Some(b) = get("start_bar") {
        parts.push(format!("bar {b}"));
    }
    parts.truncate(3);
    parts.join(" · ")
}

/// The input as `key  value` lines for the opened call.
fn input_lines(input: &Value) -> Vec<(String, String)> {
    let Some(map) = input.as_object() else { return Vec::new() };
    map.iter()
        .map(|(k, v)| {
            let text = match v {
                Value::Array(a) => format!("{} item{}", a.len(), if a.len() == 1 { "" } else { "s" }),
                Value::Object(o) => o.iter().map(|(k, v)| format!("{k}: {}", short(v).unwrap_or_else(|| v.to_string()))).collect::<Vec<_>>().join(", "),
                v => short(v).unwrap_or_else(|| v.to_string()),
            };
            (k.replace('_', " "), crate::paint::fit(&text, 200., TEXT_XS))
        })
        .collect()
}

fn shimmer(id: (usize, usize), el: impl IntoElement + 'static) -> AnyElement {
    div()
        .child(el)
        .with_animation(("agent-shimmer", id.0 * 1024 + id.1), Animation::new(motion::NORMAL * 5).repeat(), |d, t| d.opacity(0.4 + 0.6 * (1. - (t * 2. - 1.).abs())))
        .into_any_element()
}

fn chevron(open: bool) -> impl IntoElement {
    icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 10., p().faint)
}

fn thinking_line(i: usize, j: usize, text: &str, done: bool, open: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let has_text = !text.trim().is_empty();
    let head = div()
        .id(("think", i * 1024 + j))
        .h(px(20.))
        .min_w(px(0.))
        .flex()
        .items_center()
        .gap(px(SP1))
        .when(has_text, |d| d.cursor_pointer())
        .child(label(if done { "Thought" } else { "Thinking…" }, TEXT_SM, pal.faint))
        .when(has_text, |d| d.child(chevron(open)))
        .on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded((i, j), cx)));
    let head: AnyElement = if done { head.into_any_element() } else { shimmer((i, j), head) };
    div()
        .w_full()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(head)
        .when(open && has_text, |d| {
            d.child(div().min_w(px(0.)).pl(px(SP2)).border_l_1().border_color(border()).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.faint).child(super::wrap_text(text.trim())))
        })
        .into_any_element()
}

/// The opened call: its input and its result.
fn tool_body(i: usize, j: usize, name: &str, input: &Value, st: &ToolState) -> AnyElement {
    let pal = p();
    let mut body = div().ml(px(SP4)).mt(px(2.)).min_w(px(0.)).overflow_hidden().p(px(SP2)).gap(px(SP1)).flex().flex_col().rounded(px(r_md())).bg(content(0.035)).border_1().border_color(border());
    for (k, v) in input_lines(input) {
        body = body.child(div().min_w(px(0.)).flex().items_start().gap(px(SP2)).child(label(k, TEXT_XS, pal.faint).w(px(64.)).flex_none()).child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(pal.muted).child(super::wrap_text(&v))));
    }
    let result = match st {
        ToolState::Done(t) | ToolState::Failed(t) => t.trim(),
        ToolState::Running => "",
    };
    if !result.is_empty() {
        let lines: Vec<&str> = result.lines().take(14).collect();
        let more = result.lines().count().saturating_sub(lines.len());
        let color = if matches!(st, ToolState::Failed(_)) { with_alpha(pal.record, 0.85) } else { pal.muted };
        let mut out = div().id(("tool-out", i * 1024 + j)).min_w(px(0.)).overflow_x_scroll().flex().flex_col().when(!input.as_object().is_none_or(|o| o.is_empty()), |d| d.mt(px(SP1)).pt(px(SP1)).border_t_1().border_color(border()));
        for l in lines {
            out = out.child(mono(l.to_owned(), TEXT_XS, color));
        }
        if more > 0 {
            out = out.child(label(format!("+{more} more lines"), TEXT_XS, pal.faint));
        }
        body = body.child(out);
    }
    let _ = name;
    body.into_any_element()
}

fn tool_line(i: usize, j: usize, name: &str, input: &Value, st: &ToolState, open: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let asking = name == tools::ASK_USER;
    let what = detail(name, input);
    let (ic, ic_color, title, title_color) = match st {
        ToolState::Running => (tool_icon(name), pal.accent, tools::title(name).to_owned(), pal.text),
        ToolState::Done(t) if asking => (Icon::Help, pal.faint, format!("Answered: {t}"), pal.muted),
        ToolState::Done(t) => {
            let shown = if tools::is_read_only(name) || t.is_empty() { tools::title(name).to_owned() } else { plain(t) };
            (tool_icon(name), pal.faint, shown, pal.muted)
        }
        ToolState::Failed(_) => (Icon::X, pal.record, tools::title(name).to_owned(), pal.muted),
    };
    // A finished edit already says what it did; a lookup or a running call says on what.
    let tail = match st {
        ToolState::Done(_) if !tools::is_read_only(name) && !asking => String::new(),
        _ => what,
    };
    let can_open = !asking && (input.as_object().is_some_and(|o| !o.is_empty()) || !matches!(st, ToolState::Running));
    let row = div()
        .id(("tool", i * 1024 + j))
        .group("tool-row")
        .w_full()
        .min_w(px(0.))
        .h(px(22.))
        .px(px(SP1))
        .flex()
        .items_center()
        .gap(px(SP2))
        .overflow_hidden()
        .rounded(px(r_md()))
        .when(can_open, |d| d.cursor_pointer().hover(|s| s.bg(content(0.04))))
        .child(icon(ic, 12., ic_color))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(SP1))
                .overflow_hidden()
                .child(label(title, TEXT_SM, title_color).min_w(px(0.)).when(tail.is_empty(), |d| d.flex_1()).truncate())
                .when(!tail.is_empty(), |d| d.child(label(tail, TEXT_SM, pal.faint).flex_1().min_w(px(0.)).truncate())),
        )
        .when(can_open, |d| d.child(div().flex_none().opacity(if open { 1. } else { 0. }).group_hover("tool-row", |s| s.opacity(1.)).child(chevron(open))))
        .when(can_open, |d| d.on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded((i, j), cx))));
    let row: AnyElement = if matches!(st, ToolState::Running) { shimmer((i, j), row) } else { row.into_any_element() };
    let mut col = div().w_full().min_w(px(0.)).flex().flex_col().child(row);
    if let ToolState::Failed(e) = st
        && !open
    {
        col = col.child(div().min_w(px(0.)).pl(px(20.)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(with_alpha(pal.record, 0.8)).child(super::wrap_text(e.lines().next().unwrap_or_default())));
    }
    if open && can_open {
        col = col.child(tool_body(i, j, name, input, st));
    }
    col.into_any_element()
}

/// A run of finished lookups as one line that opens into each of them.
fn lookups(i: usize, j: usize, run: &[Step], open: bool, expanded: &std::collections::HashSet<(usize, usize)>, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let mut names: Vec<String> = Vec::new();
    for s in run {
        if let Step::Tool { name, input, .. } = s {
            let d = detail(name, input);
            if !d.is_empty() && !names.contains(&d) {
                names.push(d);
            }
        }
    }
    let tail = names.into_iter().take(3).collect::<Vec<_>>().join(", ");
    let head = div()
        .id(("lookups", i * 1024 + j))
        .group("lookup-row")
        .w_full()
        .min_w(px(0.))
        .h(px(22.))
        .px(px(SP1))
        .flex()
        .items_center()
        .gap(px(SP2))
        .overflow_hidden()
        .rounded(px(r_md()))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.04)))
        .child(icon(Icon::Search, 12., pal.faint))
        .child(label(format!("Explored the project · {}", run.len()), TEXT_SM, pal.muted).flex_none())
        .child(label(tail, TEXT_SM, pal.faint).flex_1().min_w(px(0.)).truncate())
        .child(div().flex_none().opacity(if open { 1. } else { 0. }).group_hover("lookup-row", |s| s.opacity(1.)).child(chevron(open)))
        .on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded((i, j), cx)));
    let mut col = div().w_full().min_w(px(0.)).flex().flex_col().child(head);
    if open {
        let mut inner = div().ml(px(6.)).pl(px(SP2)).min_w(px(0.)).border_l_1().border_color(border()).flex().flex_col();
        for (k, s) in run.iter().enumerate() {
            if let Step::Tool { name, input, state: st } = s {
                inner = inner.child(tool_line(i, j + k, name, input, st, expanded.contains(&(i, j + k)), cx));
            }
        }
        col = col.child(inner);
    }
    col.into_any_element()
}

/// Key namespace for collapsed work groups (distinct from individual step indices).
const GROUP_KEY: usize = 1 << 16;

/// Summary icon, label and error flag for a finished run of thinking and tool steps.
pub(super) fn work_summary(run: &[Step]) -> (Icon, String, bool) {
    let mut tools_n = 0usize;
    let mut read_only_n = 0usize;
    let mut failed_n = 0usize;
    let mut thoughts_n = 0usize;
    for s in run {
        match s {
            Step::Thinking { .. } => thoughts_n += 1,
            Step::Tool { name, state, .. } => {
                if name == tools::ASK_USER {
                    continue;
                }
                tools_n += 1;
                if tools::is_read_only(name) {
                    read_only_n += 1;
                }
                if matches!(state, ToolState::Failed(_)) {
                    failed_n += 1;
                }
            }
            Step::Text(_) => {}
        }
    }
    if tools_n == 0 {
        let text = if thoughts_n <= 1 { "Thought".to_owned() } else { format!("{thoughts_n} thoughts") };
        return (Icon::Sparkles, text, false);
    }
    if failed_n > 0 {
        let label = format!("Used {tools_n} {} · {failed_n} failed", if tools_n == 1 { "tool" } else { "tools" });
        return (Icon::Alert, label, true);
    }
    if read_only_n == tools_n {
        let label = format!("Explored project · {tools_n} {}", if tools_n == 1 { "step" } else { "steps" });
        return (Icon::Search, label, false);
    }
    let mut label = format!("Used {tools_n} {}", if tools_n == 1 { "tool" } else { "tools" });
    if thoughts_n > 0 {
        label.push_str(&format!(" · {thoughts_n} {}", if thoughts_n == 1 { "thought" } else { "thoughts" }));
    }
    (Icon::Sliders, label, false)
}

/// Whether a contiguous run of thinking/tool steps `steps[start..end]` is finished
/// and should collapse into a compact summary row (as monoBOT does once tools complete).
pub(super) fn segment_is_finished(steps: &[Step], start: usize, end: usize, streaming: bool) -> bool {
    if start >= end {
        return false;
    }
    let slice = &steps[start..end];
    let has_active = slice.iter().any(|s| match s {
        Step::Thinking { done, .. } => !done,
        Step::Tool { state: ToolState::Running, .. } => true,
        _ => false,
    });
    if has_active {
        return false;
    }
    // Collapse once the reply finished, or once text follows this tool batch.
    !streaming || end < steps.len()
}

/// Render the individual steps of a work run (thinking, tool calls, folded lookups).
fn render_run_steps(i: usize, base: usize, run: &[Step], expanded: &std::collections::HashSet<(usize, usize)>, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < run.len() {
        let j = base + k;
        match &run[k] {
            Step::Thinking { text, done } => out.push(thinking_line(i, j, text, *done, expanded.contains(&(i, j)), cx)),
            Step::Tool { name, input, state: st } => {
                let quiet = |s: &Step| matches!(s, Step::Tool { name, state: ToolState::Done(_), .. } if tools::is_read_only(name));
                let r = run[k..].iter().take_while(|s| quiet(s)).count();
                if r > 1 {
                    out.push(lookups(i, j, &run[k..k + r], expanded.contains(&(i, j)), expanded, cx));
                    k += r;
                    continue;
                }
                out.push(tool_line(i, j, name, input, st, expanded.contains(&(i, j)), cx));
            }
            Step::Text(_) => {}
        }
        k += 1;
    }
    out
}

/// A finished work run collapsed into one compact monoBOT-style disclosure bar.
fn work_group(i: usize, base: usize, run: &[Step], open: bool, expanded: &std::collections::HashSet<(usize, usize)>, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (ic, summary, failed) = work_summary(run);
    let key = (i, GROUP_KEY + base);
    let head = div()
        .id(("work-group", i * 1024 + base))
        .w_full()
        .min_w(px(0.))
        .h(px(26.))
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP2))
        .overflow_hidden()
        .rounded(px(r_md()))
        .bg(content(0.035))
        .border_1()
        .border_color(if failed { with_alpha(pal.record, 0.35) } else { border() })
        .cursor_pointer()
        .hover(|s| s.bg(content(0.065)))
        .child(icon(ic, 12., if failed { pal.record } else { pal.muted }))
        .child(label(summary, TEXT_SM, if failed { pal.record } else { pal.muted }).flex_1().min_w(px(0.)).truncate())
        .child(div().flex_none().child(chevron(open)))
        .on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded(key, cx)));
    let mut col = div().w_full().min_w(px(0.)).flex().flex_col().gap(px(SP1)).child(head);
    if open {
        let mut inner = div().ml(px(SP2)).pl(px(SP2)).min_w(px(0.)).border_l_1().border_color(border()).flex().flex_col().gap(px(2.));
        for el in render_run_steps(i, base, run, expanded, cx) {
            inner = inner.child(el);
        }
        col = col.child(inner);
    }
    col.into_any_element()
}

/// An agent reply: no avatar, finished tool runs collapsed cleanly, a quiet footer.
pub fn view(i: usize, r: &Reply, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let streaming = matches!(r.state, ReplyState::Waiting | ReplyState::Streaming);
    let expanded = agent(cx).expanded.clone();
    let mut col = div().w_full().min_w(px(0.)).overflow_hidden().flex().flex_col().gap(px(SP1));
    let last = r.steps.len().saturating_sub(1);
    let mut j = 0;
    while j < r.steps.len() {
        if let Step::Text(t) = &r.steps[j] {
            col = col.child(div().w_full().min_w(px(0.)).py(px(SP1)).child(markdown::view(i * 64 + j, t, streaming && j == last, cx)));
            j += 1;
            continue;
        }
        // Gather a contiguous run of thinking and tool steps.
        let mut end = j;
        while end < r.steps.len() && !matches!(r.steps[end], Step::Text(_)) {
            end += 1;
        }
        let run = &r.steps[j..end];
        // A single bare thinking step with no tools keeps its quiet inline Thinking line.
        let only_single_thought = run.len() == 1 && matches!(run[0], Step::Thinking { .. });
        if !only_single_thought && segment_is_finished(&r.steps, j, end, streaming) {
            col = col.child(work_group(i, j, run, expanded.contains(&(i, GROUP_KEY + j)), &expanded, cx));
        } else {
            for el in render_run_steps(i, j, run, &expanded, cx) {
                col = col.child(el);
            }
        }
        j = end;
    }
    // While nothing is visibly moving, say that it is working.
    let idle = r.steps.last().is_none_or(|s| matches!(s, Step::Tool { state: ToolState::Done(_) | ToolState::Failed(_), .. } | Step::Thinking { done: true, .. }));
    if streaming && idle && !agent::asking(cx) {
        col = col.child(shimmer((i, 999), div().h(px(20.)).flex().items_center().child(label(if r.steps.is_empty() { format!("{} is thinking…", r.model) } else { "Working…".to_owned() }, TEXT_SM, pal.faint))));
    }
    let footer: Option<AnyElement> = match &r.state {
        ReplyState::Waiting | ReplyState::Streaming => None,
        ReplyState::Failed(m) => Some(div().w_full().min_w(px(0.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.record).child(if m.is_empty() { "No answer".to_owned() } else { super::wrap_text(m) }).into_any_element()),
        end => {
            let text = r.text();
            let mut parts = vec![r.model.clone()];
            if let Some(d) = r.elapsed {
                parts.push(format!("{} {:.0} s", if *end == ReplyState::Stopped { "stopped after" } else { "worked" }, d.as_secs_f32()));
            }
            if r.changes > 0 {
                parts.push(format!("{} change{}", r.changes, if r.changes == 1 { "" } else { "s" }));
            }
            let mut row = div().mt(px(SP1)).w_full().min_w(px(0.)).h(px(20.)).overflow_hidden().flex().items_center().gap(px(SP2)).child(logo(r.vendor, 14.)).child(label(parts.join(" · "), TEXT_XS, pal.faint).flex_1().min_w(px(0.)).truncate());
            if !text.is_empty() {
                row = row.child(widgets::sized_icon_button(("copy-reply", i), Icon::Copy, false, 20., 12.).tooltip(widgets::tip("Copy", None)).on_click(cx.listener(move |ws, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                    ws.toast("Copied", cx);
                })));
            }
            Some(row.into_any_element())
        }
    };
    col.children(footer).font_weight(FontWeight::NORMAL).into_any_element()
}

/// What an empty chat offers: a few requests that show what the agent does.
const SUGGESTIONS: [(Icon, &str, &str); 4] = [
    (Icon::Piano, "Write a bassline that locks with the kick", "Create a bass track and write a rolling 4-bar bassline in the project key that locks with the kick, then loop it for 16 bars."),
    (Icon::Mixer, "Mix the drums", "Balance and process the drum tracks: gain staging, EQ, compression, a drum bus and a shared reverb."),
    (Icon::Timeline, "Turn this into an arrangement", "Turn what is here into an arrangement: 16-bar intro, build, drop, break and outro, with filter automation into the drop."),
    (Icon::Gauge, "Master the song", "Master the song on the Main track for club playback: EQ, glue compression, saturation and a limiter at -1 dBTP."),
];

/// The empty chat.
pub fn welcome(ready: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let c = agent::choice(cx);
    let mut list = div().flex().flex_col().gap(px(SP1));
    for (k, (ic, title, prompt)) in SUGGESTIONS.into_iter().enumerate() {
        list = list.child(
            div()
                .id(("suggest", k))
                .min_w(px(0.))
                .h(px(32.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .overflow_hidden()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .child(icon(ic, 14., pal.faint))
                .child(label(title, TEXT_SM, pal.muted).flex_1().min_w(px(0.)).truncate())
                .on_click(cx.listener(move |ws, _, _, cx| state::submit_text(ws, prompt.to_owned(), cx))),
        );
    }
    div()
        .pt(px(SP6))
        .flex()
        .flex_col()
        .gap(px(SP4))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(SP2))
                .child(logo(c.provider.vendor(), 28.))
                .child(label("What are we making?", TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.faint).child(if ready {
                    "Tracks, parts, arrangement, sound design, mixing and mastering. Every change waits for your review."
                } else {
                    "Pick a model below: it runs on your Claude (Claude Code) or ChatGPT (Codex) subscription."
                })),
        )
        .child(list)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_lines_hide_ids() {
        assert_eq!(plain("New MIDI track \"Bass\" with Acid 303 (device #41): track #40"), "New MIDI track \"Bass\" with Acid 303");
        assert_eq!(plain("Bass: \"Rolling\", 28 notes, bars 1–5 (clip #44, placement #45)"), "Bass: \"Rolling\", 28 notes, bars 1–5");
        assert_eq!(plain("Bass: add Compressor as device #46 at position 1"), "Bass: add Compressor");
        assert_eq!(plain("Kick: fader -6.0 dB (gentle)"), "Kick: fader -6.0 dB (gentle)");
    }

    #[test]
    fn details_say_what_a_call_works_on() {
        assert_eq!(detail("create_track", &json!({ "name": "Bass", "instrument": "x", "preset": "Rubber" })), "Bass · Rubber");
        assert_eq!(detail("write_clip", &json!({ "track": "Bass", "notes": [{}, {}], "start_bar": 5 })), "Bass · 2 notes · bar 5");
        assert_eq!(detail("get_track", &json!({ "track": 40 })), "track #40");
        assert_eq!(detail("set_params", &json!({ "device": 3, "params": { "Ratio": 4, "Threshold": -18 } })), "Ratio, Threshold");
        assert_eq!(detail("set_song", &json!({ "bpm": 124.0, "key": "A", "scale": "minor" })), "124 BPM · A minor");
        assert_eq!(detail("ask_user", &json!({ "question": "Which key?" })), "Which key?");
        assert_eq!(detail("get_project", &json!({})), "");
        let lines = input_lines(&json!({ "start_bar": 2, "notes": [1, 2, 3] }));
        assert!(lines.contains(&("start bar".to_owned(), "2".to_owned())));
        assert!(lines.contains(&("notes".to_owned(), "3 items".to_owned())));
    }

    #[test]
    fn finished_tool_runs_collapse_with_a_clean_summary() {
        let steps = vec![
            Step::Thinking { text: "plan".into(), done: true },
            Step::Tool { name: "create_track".into(), input: json!({}), state: ToolState::Done("ok".into()) },
            Step::Tool { name: "write_clip".into(), input: json!({}), state: ToolState::Done("ok".into()) },
            Step::Text("Done.".into()),
        ];
        // Even while streaming, a batch followed by text is finished and collapses.
        assert!(segment_is_finished(&steps, 0, 3, true));
        assert!(segment_is_finished(&steps, 0, 3, false));
        let (_, summary, failed) = work_summary(&steps[0..3]);
        assert_eq!(summary, "Used 2 tools · 1 thought");
        assert!(!failed);

        // An active tool at the tail stays open while streaming.
        let running = vec![
            Step::Tool { name: "create_track".into(), input: json!({}), state: ToolState::Done("ok".into()) },
            Step::Tool { name: "write_clip".into(), input: json!({}), state: ToolState::Running },
        ];
        assert!(!segment_is_finished(&running, 0, 2, true));

        // Read-only batches report as exploration; failures surface clearly.
        let read_only = vec![
            Step::Tool { name: "get_project".into(), input: json!({}), state: ToolState::Done("ok".into()) },
            Step::Tool { name: "get_track".into(), input: json!({}), state: ToolState::Done("ok".into()) },
        ];
        let (ic, summary, failed) = work_summary(&read_only);
        assert_eq!((ic, summary.as_str(), failed), (Icon::Search, "Explored project · 2 steps", false));

        let with_err = vec![
            Step::Tool { name: "write_clip".into(), input: json!({}), state: ToolState::Failed("boom".into()) },
        ];
        let (ic, summary, failed) = work_summary(&with_err);
        assert_eq!((ic, summary.as_str(), failed), (Icon::Alert, "Used 1 tool · 1 failed", true));
    }
}
