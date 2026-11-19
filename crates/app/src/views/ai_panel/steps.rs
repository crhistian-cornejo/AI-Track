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
        .flex()
        .items_center()
        .gap(px(SP1))
        .when(has_text, |d| d.cursor_pointer())
        .child(label(if done { "Thought" } else { "Thinking…" }, TEXT_SM, pal.faint))
        .when(has_text, |d| d.child(chevron(open)))
        .on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded((i, j), cx)));
    let head: AnyElement = if done { head.into_any_element() } else { shimmer((i, j), head) };
    div()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(head)
        .when(open && has_text, |d| {
            d.child(div().pl(px(SP2)).border_l_1().border_color(border()).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.faint).child(text.trim().to_owned()))
        })
        .into_any_element()
}

/// The opened call: its input and its result.
fn tool_body(name: &str, input: &Value, st: &ToolState) -> AnyElement {
    let pal = p();
    let mut body = div().ml(px(20.)).mt(px(2.)).p(px(SP2)).gap(px(SP1)).flex().flex_col().rounded(px(r_md())).bg(content(0.035)).border_1().border_color(border());
    for (k, v) in input_lines(input) {
        body = body.child(div().flex().gap(px(SP2)).child(label(k, TEXT_XS, pal.faint).w(px(64.)).flex_none()).child(div().flex_1().min_w(px(0.)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(pal.muted).child(v)));
    }
    let result = match st {
        ToolState::Done(t) | ToolState::Failed(t) => t.trim(),
        ToolState::Running => "",
    };
    if !result.is_empty() {
        let lines: Vec<&str> = result.lines().take(14).collect();
        let more = result.lines().count().saturating_sub(lines.len());
        let color = if matches!(st, ToolState::Failed(_)) { with_alpha(pal.record, 0.85) } else { pal.muted };
        let mut out = div().flex().flex_col().when(!input.as_object().is_none_or(|o| o.is_empty()), |d| d.mt(px(SP1)).pt(px(SP1)).border_t_1().border_color(border()));
        for l in lines {
            out = out.child(mono(crate::paint::fit(l, 250., TEXT_XS), TEXT_XS, color));
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
        .min_h(px(22.))
        .px(px(SP1))
        .mx(px(-SP1))
        .flex()
        .items_center()
        .gap(px(SP2))
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
                .child(div().flex_none().max_w(px(CARD_W - 48.)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(title_color).child(crate::paint::fit(&title, CARD_W - 48., TEXT_SM)))
                .when(!tail.is_empty(), |d| d.child(div().flex_1().min_w(px(0.)).child(label(crate::paint::fit(&tail, CARD_W - 120., TEXT_SM), TEXT_SM, pal.faint)))),
        )
        .when(can_open, |d| d.child(div().flex_none().opacity(if open { 1. } else { 0. }).group_hover("tool-row", |s| s.opacity(1.)).child(chevron(open))))
        .when(can_open, |d| d.on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded((i, j), cx))));
    let row: AnyElement = if matches!(st, ToolState::Running) { shimmer((i, j), row) } else { row.into_any_element() };
    let mut col = div().flex().flex_col().child(row);
    if let ToolState::Failed(e) = st
        && !open
    {
        col = col.child(div().pl(px(20.)).text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(with_alpha(pal.record, 0.8)).child(e.lines().next().unwrap_or_default().to_owned()));
    }
    if open && can_open {
        col = col.child(tool_body(name, input, st));
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
        .h(px(22.))
        .px(px(SP1))
        .mx(px(-SP1))
        .flex()
        .items_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.04)))
        .child(icon(Icon::Search, 12., pal.faint))
        .child(label(format!("Explored the project · {}", run.len()), TEXT_SM, pal.muted).flex_none())
        .child(div().flex_1().min_w(px(0.)).child(label(crate::paint::fit(&tail, CARD_W - 190., TEXT_SM), TEXT_SM, pal.faint)))
        .child(div().flex_none().opacity(if open { 1. } else { 0. }).group_hover("lookup-row", |s| s.opacity(1.)).child(chevron(open)))
        .on_click(cx.listener(move |_, _, _, cx| agent::toggle_expanded((i, j), cx)));
    let mut col = div().flex().flex_col().child(head);
    if open {
        let mut inner = div().ml(px(6.)).pl(px(SP2)).border_l_1().border_color(border()).flex().flex_col();
        for (k, s) in run.iter().enumerate() {
            if let Step::Tool { name, input, state: st } = s {
                inner = inner.child(tool_line(i, j + k, name, input, st, expanded.contains(&(i, j + k)), cx));
            }
        }
        col = col.child(inner);
    }
    col.into_any_element()
}

const CARD_W: f32 = AI_PANEL_W - 2. * SP3;

/// An agent reply: no avatar, its steps in order, a quiet footer.
pub fn view(i: usize, r: &Reply, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let streaming = matches!(r.state, ReplyState::Waiting | ReplyState::Streaming);
    let expanded = agent(cx).expanded.clone();
    let mut col = div().flex().flex_col().gap(px(SP1));
    let last = r.steps.len().saturating_sub(1);
    let mut j = 0;
    while j < r.steps.len() {
        match &r.steps[j] {
            Step::Thinking { text, done } => col = col.child(thinking_line(i, j, text, *done, expanded.contains(&(i, j)), cx)),
            Step::Text(t) => col = col.child(div().py(px(SP1)).child(markdown::view(i * 64 + j, t, streaming && j == last, cx))),
            Step::Tool { name, input, state: st } => {
                // A run of finished lookups reads as one quiet line.
                let quiet = |s: &Step| matches!(s, Step::Tool { name, state: ToolState::Done(_), .. } if tools::is_read_only(name));
                let run = r.steps[j..].iter().take_while(|s| quiet(s)).count();
                if run > 1 {
                    col = col.child(lookups(i, j, &r.steps[j..j + run], expanded.contains(&(i, j)), &expanded, cx));
                    j += run;
                    continue;
                }
                col = col.child(tool_line(i, j, name, input, st, expanded.contains(&(i, j)), cx));
            }
        }
        j += 1;
    }
    // While nothing is visibly moving, say that it is working.
    let idle = r.steps.last().is_none_or(|s| matches!(s, Step::Tool { state: ToolState::Done(_) | ToolState::Failed(_), .. } | Step::Thinking { done: true, .. }));
    if streaming && idle && !agent::asking(cx) {
        col = col.child(shimmer((i, 999), div().h(px(20.)).flex().items_center().child(label(if r.steps.is_empty() { format!("{} is thinking…", r.model) } else { "Working…".to_owned() }, TEXT_SM, pal.faint))));
    }
    let footer: Option<AnyElement> = match &r.state {
        ReplyState::Waiting | ReplyState::Streaming => None,
        ReplyState::Failed(m) => Some(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.record).child(if m.is_empty() { "No answer".to_owned() } else { m.clone() }).into_any_element()),
        end => {
            let text = r.text();
            let mut parts = vec![r.model.clone()];
            if let Some(d) = r.elapsed {
                parts.push(format!("{} {:.0} s", if *end == ReplyState::Stopped { "stopped after" } else { "worked" }, d.as_secs_f32()));
            }
            if r.changes > 0 {
                parts.push(format!("{} change{}", r.changes, if r.changes == 1 { "" } else { "s" }));
            }
            let mut row = div().mt(px(SP1)).h(px(20.)).flex().items_center().gap(px(SP2)).child(logo(r.vendor, 14.)).child(label(parts.join(" · "), TEXT_XS, pal.faint)).child(div().flex_1());
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
                .h(px(32.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .child(icon(ic, 14., pal.faint))
                .child(label(title, TEXT_SM, pal.muted))
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
}
