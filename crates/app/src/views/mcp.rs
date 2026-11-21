//! The external MCP server's face: Preferences > MCP server, the status-bar
//! item, and the workspace methods that answer the server's calls on the UI
//! thread (see `mcp_host.rs`). External edits arrive as proposals in the
//! assistant, like the in-app agent's, unless "apply directly" is on.

use std::time::{Duration, SystemTime};

use aitrack_ai::llm::tools::{self, Outcome, Session};
use aitrack_mcp::broker::{Effect, Offer};
use aitrack_mcp::{CallRecord, ToolReply, resources, snippets};
use gpui::{AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, px};

use crate::icons::{Icon, icon};
use crate::mcp_host::{Ask, UiRequest};
use crate::theme::*;
use crate::widgets::{self, label, mono, section_title, setting_row, stepper, stepper_el, switch, text_button};
use crate::workspace::{ChatEntry, ProposalStatus, Workspace};

/// Requests answered per timer tick, so a flood of calls cannot hold up a frame.
const PER_TICK: usize = 8;

impl Workspace {
    /// Start the server at launch when the user left it on.
    pub(crate) fn mcp_autostart(&mut self) {
        // Stub runs (tests, screenshots) never open a port.
        if std::env::var_os("AITRACK_STUBS").is_none() && self.mcp.settings.enabled {
            self.mcp.apply();
        }
    }

    /// Called from the housekeeping timer: answer the server's calls, refresh the status.
    pub(crate) fn mcp_tick(&mut self, cx: &mut Context<Self>) {
        if self.mcp.refresh() {
            cx.notify();
        }
        for _ in 0..PER_TICK {
            let Ok(req) = self.mcp.rx.try_recv() else { break };
            let answer = if self.mcp.running() { self.mcp_answer(&req, cx) } else { ToolReply::err("The MCP server is off in AI-Track.") };
            let _ = req.reply.send(answer);
        }
    }

    fn mcp_answer(&mut self, req: &UiRequest, cx: &mut Context<Self>) -> ToolReply {
        let selected = self.s.selected_track;
        match &req.ask {
            Ask::Tool { name, args } => {
                let s = &mut self.s;
                let r = self.mcp.broker.call(&mut s.project, &mut s.history, selected, &req.client, name, args);
                match r.effect {
                    Effect::Offered(o) => self.mcp_show_offer(o, cx),
                    Effect::Applied { label, origin } => {
                        self.toast(format!("{origin}: {label}"), cx);
                        cx.notify();
                    }
                    Effect::None => {}
                }
                ToolReply { text: r.text, is_error: r.is_error }
            }
            Ask::Resource(uri) => match uri.as_str() {
                resources::PROJECT => match tools::execute(&mut Session::new(&self.s.project, selected), "get_project", &serde_json::json!({})) {
                    Outcome::Ok(t) => ToolReply::ok(t),
                    Outcome::Err(e) => ToolReply::err(e),
                },
                resources::PENDING => ToolReply::ok(self.mcp.broker.describe_pending(&self.s.history)),
                other => ToolReply::err(format!("no resource {other}")),
            },
        }
    }

    /// The agent's proposal as a card in the assistant (replacing the one it grew out of).
    fn mcp_show_offer(&mut self, o: Offer, cx: &mut Context<Self>) {
        let entry = ChatEntry::Proposal { id: o.id, label: o.label.clone(), origin: o.origin.clone(), summary: o.summary, destructive: o.destructive, status: ProposalStatus::Pending };
        let old = o.replaces.and_then(|old| self.chat_log.iter_mut().find(|e| matches!(e, ChatEntry::Proposal { id, .. } if *id == old)));
        match old {
            Some(e) => *e = entry,
            None => {
                self.chat_log.push(entry);
                self.ai_open = true;
                self.toast(format!("{} proposed changes: review them in the assistant", o.origin), cx);
            }
        }
        cx.notify();
    }

    /// Turn the server on or off and keep the choice.
    pub fn mcp_set_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        self.mcp.settings.enabled = on;
        self.mcp.apply();
        self.mcp.save();
        cx.notify();
    }

    pub fn mcp_set_port(&mut self, port: u16, cx: &mut Context<Self>) {
        self.mcp.settings.port = port.max(1024);
        if self.mcp.settings.enabled {
            self.mcp.apply();
        }
        self.mcp.save();
        cx.notify();
    }

    pub fn mcp_set_direct(&mut self, on: bool, cx: &mut Context<Self>) {
        self.mcp.settings.apply_directly = on;
        self.mcp.broker.apply_directly = on;
        self.mcp.save();
        cx.notify();
    }

    fn mcp_copy(&mut self, what: &'static str, cx: &mut Context<Self>) {
        let port = self.mcp.settings.port;
        let text = match what {
            "token" => self.mcp.token(),
            "desktop" => Some(snippets::claude_desktop(&snippets::shim_path())),
            "code" => self.mcp.token().map(|t| snippets::claude_code(port, &t)),
            "code-stdio" => Some(snippets::claude_code_stdio(&snippets::shim_path())),
            _ => self.mcp.token().map(|t| snippets::cursor(port, &t)),
        };
        match text {
            Some(t) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(t));
                self.toast(if what == "token" { "Token copied".to_owned() } else { "Config copied: paste it into the client".to_owned() }, cx);
            }
            None => self.toast("The token could not be read; see the MCP page", cx),
        }
    }
}

/// The dev-view word `mcp`: the page with the server on, a made-up client and calls (nothing is started or saved).
pub fn seed_dev(ws: &mut Workspace) {
    let now = SystemTime::now();
    let ago = |s: u64| now - Duration::from_secs(s);
    let call = |s: u64, client: &str, tool: &str, ok: bool, note: &str| CallRecord { at: ago(s), client: client.into(), tool: tool.into(), ok, note: note.into() };
    ws.mcp.demo = true;
    ws.mcp.settings.enabled = true;
    ws.mcp.set_token_for_demo("aitrack_3f9c0b7a51d84e2c9a6b17f0e8d25c43b1a97e60d4f82c5a0b39e71d6c48f2a5");
    ws.mcp.status = aitrack_mcp::Status { listening: Some(aitrack_mcp::server::bind_addr(ws.mcp.settings.port)), clients: vec!["Claude Desktop".into()], calls: 5 };
    ws.mcp.log = vec![
        call(94, "Claude Desktop", "get_project", true, "\"Night Drive\": 126.00 BPM, 4/4, key A minor"),
        call(71, "Claude Desktop", "list_plugins", true, "instrument.wavetable  Wavetable Synth"),
        call(52, "Claude Desktop", "create_track", true, "Created track #7 \"Rolling Bass\""),
        call(31, "Claude Desktop", "write_clip", true, "Wrote clip #12: 32 notes, bars 1-4"),
        call(8, "Claude Desktop", "add_device", false, "no track \"Lead\""),
    ];
    ws.modal = Some(crate::views::prefs::Modal::Preferences(crate::views::prefs::PrefsPage::Mcp));
}

fn note(text: String, color: gpui::Hsla) -> Div {
    div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(color).child(text)
}

fn ago(at: SystemTime) -> String {
    match SystemTime::now().duration_since(at).map_or(0, |d| d.as_secs()) {
        0..=4 => "now".to_owned(),
        s @ 5..=59 => format!("{s}s ago"),
        s => format!("{}m ago", s / 60),
    }
}

/// `aitrack_3f9c…f2a5` while hidden.
fn masked(token: &str) -> String {
    let tail: String = token.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("aitrack_••••••••{tail}")
}

/// The discreet item in the status bar: only while the server is on.
pub fn status_item(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let text = ws.mcp.status_text()?;
    let pal = p();
    let live = !ws.mcp.status.clients.is_empty();
    let tip = match ws.mcp.status.listening {
        Some(a) => format!("External agents can read and edit the project on {a}. Click for the details."),
        None => "External agents can read and edit the project. Click for the details.".to_owned(),
    };
    Some(
        div()
            .id("mcp-status")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(SP1))
            .cursor_pointer()
            .child(icon(Icon::Plug, 12., if live { pal.accent } else { pal.muted }))
            .child(label(text, TEXT_SM, if live { pal.text } else { pal.muted }))
            .tooltip(widgets::tip_owned(tip))
            .on_click(cx.listener(|this, _, _, cx| this.open_modal(crate::views::prefs::Modal::Preferences(crate::views::prefs::PrefsPage::Mcp), cx)))
            .into_any_element(),
    )
}

pub fn mcp_page(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    // Read once from the config folder, then kept.
    let token = ws.mcp.token().unwrap_or_default();
    let m = &ws.mcp;
    let (enabled, port, direct, running) = (m.settings.enabled, m.settings.port, m.settings.apply_directly, m.running());
    let mut page = div().flex().flex_col().child(section_title("MCP SERVER")).child(setting_row(
        "Let external agents use AI-Track",
        Some("Claude Desktop, Claude Code, Cursor and other MCP clients can read and edit the open project. Only this computer can connect, and every client needs the token.".into()),
        switch("mcp-enabled", enabled).on_click(cx.listener(|this, _, _, cx| {
            let on = !this.mcp.settings.enabled;
            this.mcp_set_enabled(on, cx);
        })),
    ));
    let (shell, dec, inc) = stepper("mcp-port", port.to_string());
    page = page
        .child(setting_row(
            "Port",
            Some("127.0.0.1 only. Changing it restarts the server; clients using the old port need the new config.".into()),
            stepper_el((
                shell,
                dec.on_click(cx.listener(|this, _, _, cx| {
                    let p = this.mcp.settings.port.saturating_sub(1);
                    this.mcp_set_port(p, cx);
                })),
                inc.on_click(cx.listener(|this, _, _, cx| {
                    let p = this.mcp.settings.port.saturating_add(1);
                    this.mcp_set_port(p, cx);
                })),
            )),
        ))
        .child(setting_row(
            "Apply external edits directly",
            Some("Off: edits arrive as a proposal you accept or discard (one undo step). On: each call is applied at once as its own undo step. Deleting always asks.".into()),
            switch("mcp-direct", direct).on_click(cx.listener(|this, _, _, cx| {
                let on = !this.mcp.settings.apply_directly;
                this.mcp_set_direct(on, cx);
            })),
        ));

    // Status.
    let (dot, line) = if let Some(e) = &m.error {
        (pal.record, e.clone())
    } else if running {
        let who = match m.status.clients.len() {
            0 => "no client connected yet".to_owned(),
            1 => format!("1 client connected: {}", m.status.clients[0]),
            n => format!("{n} clients connected: {}", m.status.clients.join(", ")),
        };
        let addr = m.status.listening.map_or_else(|| format!("127.0.0.1:{port}"), |a| a.to_string());
        (pal.accent, format!("Listening on {addr} · {who}"))
    } else {
        (pal.faint, "Off".to_owned())
    };
    page = page.child(div().py(px(SP3)).flex().items_center().gap(px(SP2)).child(div().w(px(8.)).h(px(8.)).rounded_full().bg(dot)).child(note(line, pal.text)));

    // Token.
    let shown = if ws.mcp.reveal_token { token.clone() } else { masked(&token) };
    page = page
        .child(section_title("TOKEN"))
        .child(setting_row(
            "Bearer token",
            Some("Made once for this install and kept in the config folder. Clients send it as Authorization: Bearer. The stdio shim reads it from there, so it never goes in a config file.".into()),
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().max_w(px(220.)).overflow_hidden().child(mono(shown, TEXT_SM, pal.muted)))
                .child(text_button("mcp-token-show", if ws.mcp.reveal_token { "Hide" } else { "Show" }, Some(Icon::Eye), false).on_click(cx.listener(|this, _, _, cx| {
                    this.mcp.reveal_token = !this.mcp.reveal_token;
                    cx.notify();
                })))
                .child(text_button("mcp-token-copy", "Copy", Some(Icon::Copy), false).on_click(cx.listener(|this, _, _, cx| this.mcp_copy("token", cx))))
                .child(text_button("mcp-token-new", "New token", Some(Icon::Refresh), false).on_click(cx.listener(|this, _, _, cx| {
                    if this.mcp.regenerate_token().is_some() {
                        this.toast("New token. Update the clients that use it.", cx);
                    }
                    cx.notify();
                }))),
        ));

    // Config for each client.
    let copy = |id: &'static str, text: &'static str, what: &'static str, cx: &mut Context<Workspace>| text_button(id, text, Some(Icon::Copy), false).on_click(cx.listener(move |this, _, _, cx| this.mcp_copy(what, cx)));
    page = page
        .child(section_title("CONNECT A CLIENT"))
        .child(setting_row("Claude Desktop", Some("A JSON snippet for claude_desktop_config.json. It runs the aitrack-mcp shim, which finds the port and token itself.".into()), copy("mcp-copy-desktop", "Copy config", "desktop", cx)))
        .child(setting_row(
            "Claude Code",
            Some("The claude mcp add command, over HTTP with the token. Or use the stdio form without a token in it.".into()),
            div().flex().gap(px(SP2)).child(copy("mcp-copy-code", "Copy command", "code", cx)).child(copy("mcp-copy-code-stdio", "Copy stdio command", "code-stdio", cx)),
        ))
        .child(setting_row("Cursor", Some("A JSON snippet for .cursor/mcp.json with the URL and token header.".into()), copy("mcp-copy-cursor", "Copy config", "cursor", cx)));

    // The log of external calls.
    page = page.child(section_title("RECENT CALLS"));
    if ws.mcp.log.is_empty() {
        page = page.child(div().py(px(SP2)).child(note("Nothing yet. Calls from external agents are listed here.".into(), pal.faint)));
    } else {
        let mut list = div().py(px(SP2)).flex().flex_col().gap(px(SP1));
        for r in ws.mcp.log.iter().rev().take(8) {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SP2))
                    .child(icon(if r.ok { Icon::Check } else { Icon::X }, 12., if r.ok { pal.accent } else { pal.record }))
                    .child(div().w(px(64.)).flex_none().child(label(ago(r.at), TEXT_XS, pal.faint)))
                    .child(div().w(px(116.)).flex_none().overflow_hidden().child(label(r.client.clone(), TEXT_SM, pal.muted)))
                    .child(div().w(px(104.)).flex_none().child(mono(r.tool.clone(), TEXT_SM, pal.text)))
                    .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label(r.note.clone(), TEXT_SM, pal.faint))),
            );
        }
        page = page.child(list);
    }
    page.into_any_element()
}
