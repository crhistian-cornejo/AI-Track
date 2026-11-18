//! The updater's face: the status-bar item, the Preferences › Updates page, the
//! crash-report banner, and the workspace methods that connect them to `updater.rs`.

use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::updater::{self, Offer, State};
use crate::widgets::{self, label, mono, section_title, setting_row, switch, text_button};
use crate::workspace::Workspace;
use gpui::{AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, px};
use std::time::Instant;

impl Workspace {
    /// "Check for Updates…" (menu, Cmd+K, Preferences).
    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.updater.check_now(Instant::now());
        cx.notify();
    }

    /// Recording or exporting: no download starts and nothing installs.
    fn update_blocked(&self) -> bool {
        self.b.engine.transport().recording || self.export.running.is_some()
    }

    /// Called from the housekeeping timer.
    pub(crate) fn updater_tick(&mut self, cx: &mut Context<Self>) {
        if self.crash_notice.is_none()
            && let Ok(n) = self.crash_rx.try_recv()
        {
            self.crash_notice = Some(n);
            cx.notify();
        }
        let busy = self.update_blocked();
        if self.updater.tick(Instant::now(), busy) {
            cx.notify();
        }
        if let Some(m) = self.updater.take_notice() {
            self.toast(m, cx);
        }
        if self.updater.take_quit() {
            cx.quit();
        }
    }

    /// "Restart": make the project safe, then install and relaunch.
    pub fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if self.update_blocked() {
            self.toast("Finish recording or exporting first, then restart to update", cx);
            return;
        }
        // The quit path has no save prompt; a changed project is autosaved (and offered back at the next start).
        let pending = if self.s.history.is_dirty() {
            let states = self.b.engine.plugin_states();
            self.s.capture_plugin_states(states);
            let meta = crate::autosave::Meta { original: self.project_path.clone(), name: self.s.project.name.clone() };
            let key = self.autosaver.key(self.project_path.as_deref());
            self.autosaver.write_in_background(crate::autosave::dir(), key, meta, self.s.project.clone())
        } else {
            None
        };
        if !self.updater.install_now(pending) {
            self.toast("No update is ready", cx);
        }
        cx.notify();
    }
}

/// The dev-view words `update=available|ready|failed`.
pub fn seed_dev(ws: &mut Workspace, word: &str) {
    let offer = Offer { version: "0.0.2".into(), notes: "Faster project loading.\nThe Acid 303 generator keeps its seed.\nFixes a click when a device is bypassed.".into() };
    ws.updater.prefs.last_check = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs().saturating_sub(300));
    ws.updater.seed(match word {
        "update=ready" => State::Ready(offer),
        "update=failed" => State::Failed("The update's signature is not valid: the file was changed after it was signed.".into()),
        _ => State::Available(offer),
    });
}

type Click = fn(&mut Workspace, &mut Context<Workspace>);

/// The discreet item in the status bar: only while there is something to say.
pub fn status_item(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    let (text, color, ic, tip, action): (String, gpui::Hsla, Icon, String, Option<Click>) = match &ws.updater.state {
        State::Available(o) => (format!("Update {} available · Download", o.version), pal.accent, Icon::Download, format!("AI-Track {} is available. Click to download it.", o.version), Some(|w, cx| {
            w.updater.start_download();
            cx.notify();
        })),
        State::Downloading { offer, done, total } => (format!("Downloading update {} · {}%", offer.version, updater::percent(*done, *total)), pal.muted, Icon::Download, "Downloading in the background".into(), None),
        State::Ready(o) => (format!("Update {} ready · Restart", o.version), pal.accent, Icon::Check, format!("AI-Track {} is downloaded and verified. Click to restart and install it.", o.version), Some(|w, cx| w.restart_to_update(cx))),
        State::Failed(msg) => ("Update failed · Check again".into(), pal.record, Icon::Refresh, msg.clone(), Some(|w, cx| w.check_for_updates(cx))),
        State::Installing => ("Installing update…".into(), pal.muted, Icon::Loader, "Installing the update".into(), None),
        State::Idle | State::Checking | State::UpToDate => return None,
    };
    let item = div().id("update-status").flex_none().flex().items_center().gap(px(SP1)).child(icon(ic, 12., color)).child(label(text, TEXT_SM, color)).tooltip(widgets::tip_owned(tip));
    Some(match action {
        Some(run) => item.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| run(this, cx))).into_any_element(),
        None => item.into_any_element(),
    })
}

fn note(text: String, color: gpui::Hsla) -> Div {
    div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(color).child(text)
}

pub fn updates_page(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let u = &ws.updater;
    let checking = matches!(u.state, State::Checking);
    let (check_auto, install_auto) = (u.prefs.check_automatically, u.prefs.install_automatically);
    let mut page = div()
        .flex()
        .flex_col()
        .child(section_title("UPDATES"))
        .child(setting_row("Installed version", None, mono(format!("AI-Track {}", u.current_version()), TEXT_MD, pal.text)))
        .child(setting_row(
            "Check for updates automatically",
            Some("Looks 30 seconds after start and every 6 hours".into()),
            switch("update-check-auto", check_auto).on_click(cx.listener(|this, _, _, cx| {
                this.updater.prefs.check_automatically = !this.updater.prefs.check_automatically;
                this.updater.prefs.save_in_background();
                cx.notify();
            })),
        ))
        .child(setting_row(
            "Download and install automatically",
            Some("Downloads in the background and installs when AI-Track closes".into()),
            switch("update-install-auto", install_auto).on_click(cx.listener(|this, _, _, cx| {
                this.updater.prefs.install_automatically = !this.updater.prefs.install_automatically;
                this.updater.prefs.save_in_background();
                cx.notify();
            })),
        ))
        .child(setting_row(
            "Last checked",
            Some(updater::last_check_label(u.prefs.last_check).into()),
            text_button("update-check-now", if checking { "Checking…" } else { "Check for updates…" }, Some(Icon::Refresh), false).on_click(cx.listener(|this, _, _, cx| this.check_for_updates(cx))),
        ));
    let status: Option<AnyElement> = match &u.state {
        State::UpToDate => Some(note("You have the latest version.".into(), pal.muted).into_any_element()),
        State::Available(o) => Some(release_block(o, "Download", Icon::Download, "update-download", cx, |w, cx| {
            w.updater.start_download();
            cx.notify();
        })),
        State::Downloading { offer, done, total } => Some(note(format!("Downloading {} · {}%", offer.version, updater::percent(*done, *total)), pal.muted).into_any_element()),
        State::Ready(o) => Some(release_block(o, "Restart to update", Icon::Check, "update-restart", cx, |w, cx| w.restart_to_update(cx))),
        State::Failed(msg) => Some(note(msg.clone(), pal.record).into_any_element()),
        State::Installing => Some(note("Installing the update…".into(), pal.muted).into_any_element()),
        State::Idle | State::Checking => None,
    };
    if let Some(s) = status {
        page = page.child(div().py(px(SP3)).child(s));
    }
    if let Some(why) = u.cannot_install() {
        page = page.child(note(format!("This build cannot install updates by itself: {why}."), pal.faint));
    }
    page.into_any_element()
}

fn release_block(o: &Offer, button: &'static str, ic: Icon, id: &'static str, cx: &mut Context<Workspace>, run: fn(&mut Workspace, &mut Context<Workspace>)) -> AnyElement {
    let pal = p();
    let mut block = div().flex().flex_col().gap(px(SP2)).child(
        div()
            .flex()
            .items_center()
            .gap(px(SP2))
            .child(label(format!("Version {}", o.version), TEXT_MD, pal.text).font_weight(gpui::FontWeight::SEMIBOLD))
            .child(div().flex_1())
            .child(text_button(id, button, Some(ic), true).on_click(cx.listener(move |this, _, _, cx| run(this, cx)))),
    );
    if !o.notes.trim().is_empty() {
        block = block.child(div().p(px(SP2)).rounded(px(r_md())).bg(content(0.03)).border_1().border_color(border()).child(note(o.notes.clone(), pal.muted)));
    }
    block.into_any_element()
}

/// A dismissable strip offering last run's crash report: copy it, open an issue. Nothing is sent.
pub fn crash_banner(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    ws.crash_notice.as_ref()?;
    let pal = p();
    Some(
        div()
            .flex_none()
            .mx(px(SP1))
            .mt(px(SP1))
            .px(px(SP3))
            .py(px(SP2))
            .flex()
            .items_center()
            .gap(px(SP2))
            .rounded(px(r_lg()))
            .bg(with_alpha(pal.record, 0.12))
            .border_1()
            .border_color(with_alpha(pal.record, 0.4))
            .child(icon(Icon::Help, 14., pal.record))
            .child(div().flex_1().min_w(px(0.)).child(label("AI-Track stopped unexpectedly last time. A report was saved on this computer; nothing was sent.", TEXT_SM, pal.text)))
            .child(text_button("crash-copy", "Copy report", Some(Icon::Copy), false).on_click(cx.listener(|this, _, _, cx| {
                if let Some(n) = &this.crash_notice {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(n.text.clone()));
                }
                this.toast("Report copied: paste it into the issue", cx);
            })))
            .child(text_button("crash-issue", "Open issue", Some(Icon::Globe), false).on_click(cx.listener(|_, _, _, cx| cx.open_url(crate::logging::ISSUES_URL))))
            .child(widgets::icon_button("crash-close", Icon::X, false).on_click(cx.listener(|this, _, _, cx| {
                this.crash_notice = None;
                cx.notify();
            })))
            .into_any_element(),
    )
}
