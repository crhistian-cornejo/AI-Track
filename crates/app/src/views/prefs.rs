//! Preferences (Audio, Appearance, Sounds) and the modal shell the Export dialog
//! shares. Appearance edits save immediately and cross-fade the theme;
//! audio edits go to `EngineBridge::set_audio_config`.

use crate::appearance::{self, Appearance, Background, BgFit, Pane, ThemeLibrary};
use crate::bridge::{AudioConfig, AudioHost, AudioLatency, BUFFER_SIZES, SAMPLE_RATES};
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::{self, *};
use crate::widgets::{
    self, OnSelect, checkbox, label, mono, section_title, setting_block, setting_row, stepper,
    stepper_el, switch, text_button,
};
use crate::workspace::{Drag, Workspace};
use gpui::{
    AnyElement, Bounds, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, PathPromptOptions, Pixels, StatefulInteractiveElement, Styled, Window,
    StyledImage, WindowAppearance, div, img, prelude::FluentBuilder, px,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefsPage {
    Audio,
    Appearance,
    /// Sample sources for the palette: Content Hub, Splice (opt-in).
    Sounds,
    /// MIDI input ports and the track live MIDI goes to.
    Midi,
    Performance,
    /// Update checks and the installed version.
    Updates,
    /// VST3/CLAP plugins: folders searched, what was found and what was left out.
    Plugins,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    Preferences(PrefsPage),
    Export,
    /// The searchable list of every keyboard shortcut.
    Shortcuts,
    /// The name field of Rename (a track or a clip).
    Rename,
    ContentHub,
    Organize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PickTarget {
    Accent,
    Token(&'static str),
}

/// A panel height that still sits at the old density's default moves to the
/// new one's; one the user dragged elsewhere stays.
fn follow_default(h: f32, old_default: f32, new_default: f32) -> f32 {
    if (h - old_default).abs() < 0.5 { new_default } else { h }
}

/// Appearance state owned by the workspace.
pub struct Look {
    pub app: Appearance,
    pub lib: ThemeLibrary,
    pub effective: Theme,
    pub picker: Option<(PickTarget, f32, f32, f32)>,
    pub sv_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub hue_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The (pre-blurred) background image on screen.
    pub bg_image: Option<PathBuf>,
    /// Apply on the next frame (`true` = cross-fade); needs the window.
    pub pending: Option<bool>,
    pub tokens_open: bool,
    /// The token editor's search field.
    pub token_search: gpui::Entity<widgets::TextInput>,
    _watcher: Option<notify::RecommendedWatcher>,
    _token_sub: gpui::Subscription,
}

impl Look {
    pub fn load(cx: &mut Context<Workspace>) -> Self {
        let app = Appearance::load();
        let lib = ThemeLibrary::load();
        // Hot reload: the watcher thread pings, a task on the UI thread reloads.
        let (tx, rx) = async_channel::unbounded::<()>();
        let watcher = appearance::watch_themes(move || {
            let _ = tx.send_blocking(());
        });
        cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                // Coalesce bursts of file events (editors write in several steps).
                cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
                while rx.try_recv().is_ok() {}
                if this
                    .update(cx, |ws, cx| {
                        ws.look.lib = ThemeLibrary::load();
                        ws.look.pending = Some(true);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let effective = theme::builtin_themes().remove(0);
        let token_search = gpui::AppContext::new(cx, |cx| widgets::TextInput::new(cx, "Search tokens…", Some(Icon::Search)));
        let token_sub = cx.observe(&token_search, |_, _, cx| cx.notify());
        Self {
            app,
            lib,
            effective,
            picker: None,
            sv_bounds: Rc::default(),
            hue_bounds: Rc::default(),
            bg_image: None,
            pending: Some(false),
            tokens_open: false,
            token_search,
            _watcher: watcher,
            _token_sub: token_sub,
        }
    }
}

impl Workspace {
    /// Change appearance settings: saved at once, applied (cross-faded) next frame.
    pub fn set_look(&mut self, animate: bool, cx: &mut Context<Self>, f: impl FnOnce(&mut Appearance)) {
        let before = self.look.app.density;
        f(&mut self.look.app);
        let after = self.look.app.density;
        if after != before {
            // A panel the user never resized follows the new density's default heights.
            let (from, to) = (Metrics::of(before), Metrics::of(after));
            self.bottom_h = follow_default(self.bottom_h, from.dock_h, to.dock_h);
            self.mixer_h = follow_default(self.mixer_h, from.mixer_h, to.mixer_h);
        }
        self.look.app.save();
        self.look.pending = Some(animate);
        cx.notify();
    }

    pub fn apply_look(&mut self, window: &mut Window, animate: bool, cx: &mut Context<Self>) {
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        self.look.effective = appearance::apply(&self.look.app, &self.look.lib, system_dark, animate);
        self.s.color_prefs = crate::state::ColorPrefs { auto_track: self.look.app.auto_track_colors, clips_take_track: self.look.app.clips_take_track_color };
        window.set_content_zoom(self.look.app.ui_scale.clamp(0.8, 1.5), cx);
        cx.set_reduce_motion(self.look.app.reduced_motion || self.look.app.low_power);
        match self.look.app.background.clone().filter(|b| !b.path.as_os_str().is_empty()) {
            Some(bg) => {
                let task = cx.background_executor().spawn(async move { appearance::prepare_background(&bg) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |ws, cx| {
                        match result {
                            Ok(p) => ws.look.bg_image = Some(p),
                            Err(e) => ws.toast(format!("Background image: {e}"), cx),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            None => self.look.bg_image = None,
        }
        window.refresh();
    }

    pub fn open_modal(&mut self, m: Modal, cx: &mut Context<Self>) {
        self.modal = Some(m);
        self.menu = None;
        cx.notify();
    }

    pub fn close_modal(&mut self, cx: &mut Context<Self>) {
        if self.export.running.is_some() {
            return;
        }
        self.modal = None;
        self.look.picker = None;
        cx.notify();
    }

    fn pick(&mut self, target: PickTarget, cx: &mut Context<Self>) {
        let current = match &target {
            PickTarget::Accent => p().accent,
            PickTarget::Token(name) => self.look.effective.colors.get(name).map_or(p().accent, |c| c.to_hsla()),
        };
        let (h, s, v) = widgets::hsla_to_hsv(current);
        self.look.picker = if self.look.picker.as_ref().is_some_and(|(t, ..)| *t == target) { None } else { Some((target, h, s, v)) };
        cx.notify();
    }

    /// Picker drag: update the colour live (no cross-fade while dragging).
    pub fn pick_at(&mut self, pos: gpui::Point<Pixels>, hue: bool, cx: &mut Context<Self>) {
        let Some((target, mut h, mut s, mut v)) = self.look.picker.clone() else { return };
        if hue {
            let b = self.look.hue_bounds.get();
            h = (f32::from(pos.x - b.origin.x) / f32::from(b.size.width)).clamp(0., 0.999);
        } else {
            let b = self.look.sv_bounds.get();
            s = (f32::from(pos.x - b.origin.x) / f32::from(b.size.width)).clamp(0., 1.);
            v = 1. - (f32::from(pos.y - b.origin.y) / f32::from(b.size.height)).clamp(0., 1.);
        }
        self.look.picker = Some((target.clone(), h, s, v));
        let mut c = Rgba8::from_hsla(widgets::hsv_to_hsla(h, s, v));
        if let PickTarget::Token(name) = &target {
            // Keep the token's own alpha.
            let alpha = self.look.effective.colors.get(name).map_or(0xFF, |x| x.0 & 0xFF);
            c = Rgba8(c.0 & 0xFFFF_FF00 | alpha);
        }
        self.set_look(false, cx, |a| match &target {
            PickTarget::Accent => a.accent = Some(c),
            PickTarget::Token(name) => {
                a.overrides.insert((*name).to_owned(), c);
            }
        });
    }
}

// ───────────────────────────── Modal shell ─────────────────────────────

pub fn render_modal(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let modal = ws.modal?;
    let (title, body, width): (&str, AnyElement, f32) = match modal {
        Modal::Preferences(page) => ("Preferences", preferences(ws, page, window, cx), 880.),
        Modal::Export => ("Export audio", crate::views::export::render(ws, window, cx), 600.),
        Modal::Shortcuts => ("Keyboard shortcuts", crate::views::shortcuts::render(ws, cx), 640.),
        Modal::Rename => ("Rename", crate::views::shortcuts::render_rename(ws, cx), 360.),
        Modal::ContentHub => ("Content Hub", crate::views::hub::render_hub(ws, cx), 760.),
        Modal::Organize => ("Organize on disk", crate::views::hub::render_organize(ws, cx), 600.),
    };
    let pal = p();
    let card = div()
        .id("modal-card")
        .occlude()
        .w(px(width))
        .max_h(px(720.))
        .flex()
        .flex_col()
        .rounded(px(r_lg() + 4.))
        .bg(pal.overlay)
        .border_1()
        .border_color(border_strong())
        .shadow_lg()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex_none()
                .h(px(TOP_BAR_H))
                .px(px(SP4))
                .flex()
                .items_center()
                .gap(px(SP2))
                .border_b_1()
                .border_color(border())
                .child(icon(match modal { Modal::Export => Icon::Send, Modal::Shortcuts => Icon::Keyboard, Modal::Rename => Icon::Pencil, Modal::ContentHub => Icon::Globe, Modal::Organize => Icon::FolderTree, Modal::Preferences(_) => Icon::Sliders }, 16., pal.accent))
                .child(label(title, TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(widgets::icon_button("close-modal", Icon::X, false).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)))),
        )
        .child(div().flex_1().min_h(px(0.)).child(body));
    Some(
        div()
            .id("modal-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(shadow(0.45))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_modal(cx)))
            .child(motion::rise_in("modal-in", card, 12.))
            .into_any_element(),
    )
}

fn preferences(ws: &mut Workspace, page: PrefsPage, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let body = match page {
        PrefsPage::Audio => audio_page(ws, cx),
        PrefsPage::Appearance => appearance_page(ws, window, cx),
        PrefsPage::Sounds => sounds_page(ws, cx),
        PrefsPage::Midi => midi_page(ws, window, cx),
        PrefsPage::Performance => performance_page(ws, cx),
        PrefsPage::Plugins => plugins_page(ws, cx),
        PrefsPage::Updates => super::updates::updates_page(ws, cx),
    };
    let nav = |id: &'static str, i: Icon, text: &'static str, target: PrefsPage| {
        let on = page == target;
        div()
            .id(id)
            .h(px(ROW_H + 4.))
            .px(px(SP3))
            .gap(px(SP2))
            .flex()
            .items_center()
            .rounded(px(r_md()))
            .cursor_pointer()
            .when(on, |d| d.bg(p().selection))
            .when(!on, |d| d.hover(|s| s.bg(hover())))
            .child(icon(i, 16., if on { pal.accent } else { pal.muted }))
            .child(label(text, TEXT_MD, if on { pal.text } else { pal.muted }).font_weight(FontWeight::MEDIUM))
            .on_click(cx.listener(move |this, _, _, cx| this.open_modal(Modal::Preferences(target), cx)))
    };
    div()
        .h(px(640.))
        .flex()
        .child(
            div()
                .w(px(184.))
                .flex_none()
                .p(px(SP2))
                .gap(px(SP1))
                .flex()
                .flex_col()
                .border_r_1()
                .border_color(border())
                .child(nav("nav-audio", Icon::Volume, "Audio", PrefsPage::Audio))
                .child(nav("nav-look", Icon::Sparkles, "Appearance", PrefsPage::Appearance))
                .child(nav("nav-sounds", Icon::Sample, "Sounds", PrefsPage::Sounds))
                .child(nav("nav-midi", Icon::Piano, "MIDI", PrefsPage::Midi))
                .child(nav("nav-plugins", Icon::Plug, "Plugins", PrefsPage::Plugins))
                .child(nav("nav-perf", Icon::Power, "Performance", PrefsPage::Performance))
                .child(nav("nav-updates", Icon::Download, "Updates", PrefsPage::Updates)),
        )
        .child(
            div()
                .id(match page {
                    PrefsPage::Audio => "audio-scroll",
                    PrefsPage::Appearance => "look-scroll",
                    PrefsPage::Sounds => "sounds-scroll",
                    PrefsPage::Midi => "midi-scroll",
                    PrefsPage::Performance => "perf-scroll",
                    PrefsPage::Plugins => "plugins-scroll",
                    PrefsPage::Updates => "updates-scroll",
                })
                .flex_1()
                .min_w(px(0.))
                .overflow_y_scroll()
                .px(px(SP6))
                .pb(px(SP6))
                .child(motion::fade_in(gpui::ElementId::Name(format!("page-{page:?}").into()), div().child(body))),
        )
        .into_any_element()
}

// ───────────────────────────── Sounds ─────────────────────────────

/// A wrapping paragraph (labels stay on one line).
fn para(text: &'static str, color: gpui::Hsla) -> Div {
    div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(color).child(text)
}

/// Index the Splice desktop app's download folder into the library.
fn index_splice_folder(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    match ws.palette.settings.splice_folder() {
        Some(dir) if dir.is_dir() => match ws.b.library.add_folder(&dir) {
            Ok(()) => ws.toast(format!("Indexing {}", dir.display()), cx),
            Err(e) => ws.toast(e, cx),
        },
        Some(dir) => ws.toast(format!("No Splice folder at {} yet: install the Splice desktop app, or choose its folder with “Folder…”", dir.display()), cx),
        None => ws.toast("Choose the Splice folder first", cx),
    }
}

/// Where the Splice folder stands, in words, and whether it needs attention.
fn splice_folder_status(chosen: bool, folder: Option<&std::path::Path>, sounds: Option<u64>) -> (String, bool) {
    match (folder, folder.is_some_and(std::path::Path::is_dir)) {
        (Some(f), true) => match sounds {
            Some(n) => (format!("Found at {} · {n} sounds indexed. New downloads appear on their own.", f.display()), false),
            None => (format!("Found at {}. New downloads appear on their own.", f.display()), false),
        },
        (Some(f), false) if chosen => (format!("{} is not there. Choose the folder again, or use the automatic search.", f.display()), true),
        _ => ("Not found: looked in ~/Splice/sounds, ~/Splice and Ableton's User Library (Samples/Splice). Install the Splice desktop app, or choose its folder; it is picked up as soon as it exists.".to_owned(), false),
    }
}

/// The Splice folder: what was found (or chosen), what it holds, and the buttons.
fn splice_folder_row(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let chosen = ws.palette.settings.splice_folder.is_some();
    let folder = ws.palette.settings.splice_folder();
    let sounds = match &ws.sounds.page.splice {
        crate::bridge::SpliceState::Indexed { sounds, .. } => Some(*sounds),
        _ => None,
    };
    let (text, warn) = splice_folder_status(chosen, folder.as_deref(), sounds);
    div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(div().text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(if warn { pal.solo } else { pal.text }).child(text))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(text_button("splice-folder", "Folder…", Some(Icon::FolderOpen), false).on_click(cx.listener(|_, _, _, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Splice folder".into()) });
                    cx.spawn(async move |this, cx| {
                        if let Ok(Ok(Some(paths))) = rx.await
                            && let Some(p) = paths.into_iter().next()
                        {
                            let _ = this.update(cx, |ws, cx| {
                                ws.palette.settings.splice_folder = Some(p);
                                ws.palette.settings.save();
                                index_splice_folder(ws, cx);
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                })))
                .when(chosen, |d| {
                    d.child(text_button("splice-auto", "Find it automatically", None, false).on_click(cx.listener(|this, _, _, cx| {
                        this.palette.settings.splice_folder = None;
                        this.palette.settings.save();
                        cx.notify();
                    })))
                })
                .child(text_button("splice-index", "Index now", Some(Icon::Refresh), false).on_click(cx.listener(|this, _, _, cx| index_splice_folder(this, cx)))),
        )
        .into_any_element()
}

fn sounds_page(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    ws.palette.load();
    let on = ws.palette.settings.splice;
    // Starts the Splice worker when on, so the stored sign-in is read (off this thread).
    let (signed, signing) = ws.palette.splice_ui().map_or((None, false), |s| {
        let _ = s.poll_sign_in();
        (s.signed_in, s.signing_in)
    });
    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(section_title("CONTENT HUB"))
        .child(para("Free packs: the bundled ones, optional ones AI-Track downloads and indexes for you, and links to packs and plugins you get from their official pages. Also in the search palette (⌘K → Packs).", pal.muted))
        .child(setting_row(
            "Packs",
            None,
            text_button("open-hub", "Open Content Hub", Some(Icon::Package), false).on_click(cx.listener(|this, _, _, cx| this.open_modal(Modal::ContentHub, cx))),
        ))
        .child(section_title("SPLICE FOLDER"))
        .child(para("The sounds you license with the Splice desktop app are files in a folder on this computer. AI-Track finds that folder by itself, indexes it as its own collection (“Splice” in the Sounds panel and the search palette, filterable on its own), and picks up new downloads without a restart. Nothing is sent to Splice.", pal.muted))
        .child(splice_folder_row(ws, cx))
        .child(section_title("SPLICE ONLINE SEARCH (OPTIONAL)"))
        .child(
            div()
                .p(px(SP3))
                .rounded(px(r_md()))
                .bg(content(0.04))
                .border_1()
                .border_color(border())
                .child(para(
                    "Uses Splice's unofficial web API (the one its website uses). It may change or stop working at any time, and it is not endorsed by Splice. Search and previews need no account; sounds you license with your Splice account land in your Splice folder, which AI-Track indexes. Sign-in happens on Splice's own page in your browser: AI-Track never asks for or stores your password, only a token in the system keychain.",
                    pal.muted,
                )),
        )
        .child(setting_row(
            "Search Splice",
            Some("Adds a Splice section to the palette".into()),
            switch("splice-on", on).on_click(cx.listener(|this, _, _, cx| {
                let next = !this.palette.settings.splice;
                this.palette.set_splice(next);
                if next {
                    index_splice_folder(this, cx);
                }
                cx.notify();
            })),
        ));
    if on {
        col = col
            .child(setting_row(
                "Account",
                Some(match signed {
                    Some(true) => "Signed in".into(),
                    Some(false) => "Not signed in".into(),
                    None => "Checking the keychain…".into(),
                }),
                if signed == Some(true) {
                    text_button("splice-out", "Sign out", None, false).on_click(cx.listener(|this, _, _, cx| {
                        if let Some(s) = this.palette.splice_ui() {
                            s.sign_out();
                        }
                        cx.notify();
                    }))
                } else {
                    text_button("splice-in", if signing { "Waiting for the browser…" } else { "Sign in with Splice" }, Some(Icon::Globe), true).on_click(cx.listener(|this, _, _, cx| {
                        if let Some(s) = this.palette.splice_ui()
                            && !s.signing_in
                        {
                            s.sign_in();
                        }
                        cx.notify();
                    }))
                },
            ));
    }
    col.into_any_element()
}

// ───────────────────────────── Audio ─────────────────────────────

fn chip_row(id: &'static str, items: Vec<(String, bool, bool)>, cx: &mut Context<Workspace>, on: impl Fn(&mut Workspace, usize, &mut Context<Workspace>) + 'static) -> Div {
    let pal = p();
    let on = Rc::new(on);
    let mut row = div()
        .max_w(px(480.))
        .flex()
        .flex_wrap()
        .justify_end()
        .gap(px(SP1));
    for (i, (text, selected, enabled)) in items.into_iter().enumerate() {
        let on = on.clone();
        row = row.child(
            div()
                .id((id, i))
                .h(px(CONTROL_H))
                .min_w(px(56.))
                .px(px(SP2))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_md()))
                .border_1()
                .border_color(if selected { pal.accent } else { border() })
                .bg(if selected { pal.selection } else { pal.well })
                .when(!enabled, |d| d.opacity(0.35))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.border_color(border_strong())).on_click(cx.listener(move |this, _, _, cx| on(this, i, cx))))
                .child(mono(text, TEXT_SM, if selected { pal.text } else { pal.muted })),
        );
    }
    row
}

fn device_list(id: &'static str, devices: Vec<(String, String)>, selected: Option<String>, cx: &mut Context<Workspace>, output: bool) -> Div {
    let pal = p();
    let mut list = div().w_full().flex().flex_col().gap(px(2.)).p(px(SP1)).rounded(px(r_md())).bg(pal.well).border_1().border_color(border());
    let mut rows = vec![("".to_owned(), "System default".to_owned())];
    rows.extend(devices);
    for (i, (dev_id, name)) in rows.into_iter().enumerate() {
        let on = selected.as_deref().unwrap_or("") == dev_id;
        list = list.child(
            div()
                .id((id, i))
                .h(px(ROW_H))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .when(on, |d| d.bg(pal.selection))
                .when(!on, |d| d.hover(|s| s.bg(hover())))
                .child(div().size(px(8.)).rounded_full().bg(if on { pal.accent } else { content(0.15) }))
                .child(label(name, TEXT_MD, if on { pal.text } else { pal.muted }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut c = this.b.engine.audio_config();
                    let v = (!dev_id.is_empty()).then(|| dev_id.clone());
                    if output { c.output = v } else { c.input = v }
                    apply_audio(this, c, cx);
                })),
        );
    }
    list
}

/// Device output latency above this is a wireless link (Bluetooth adds
/// 100–300 ms); the page warns that it is no good for playing in.
const WIRELESS_MS: f32 = 40.;

fn ms(v: f32) -> String {
    if v >= 100. { format!("{v:.0} ms") } else { format!("{v:.1} ms") }
}

/// The latency read-out: `(caption, value, detail)` per tile. Measured
/// figures when the engine has them, the buffer estimate otherwise.
fn latency_tiles(lat: Option<AudioLatency>, cfg: &AudioConfig) -> Vec<(&'static str, String, String)> {
    let buffer = cfg.buffer_ms();
    match lat.filter(|l| l.output_ms > 0.) {
        Some(l) => {
            let rate = format!("{:.1} kHz", l.sample_rate as f32 / 1000.).replace(".0 ", " ");
            let buf_ms = if l.sample_rate > 0 { l.buffer_frames as f32 * 1000. / l.sample_rate as f32 } else { buffer };
            let input = if l.input_ms > 0. { (ms(l.input_ms), "microphone to engine".to_owned()) } else { ("—".to_owned(), "no input open".to_owned()) };
            vec![
                ("OUTPUT", ms(l.output_ms), format!("{:.1} buffer + {} device", buf_ms, ms(l.device_output_ms))),
                ("INPUT", input.0, input.1),
                ("ROUND TRIP", ms(l.round_trip_ms() + l.plugin_ms), if l.plugin_ms > 0. { format!("incl. {} plugin delay", ms(l.plugin_ms)) } else { "in → out, monitored".to_owned() }),
                ("FORMAT", rate, format!("{} samples per buffer", l.buffer_frames)),
            ]
        }
        None => vec![
            ("OUTPUT", format!("≈ {}", ms(buffer)), "estimate, waiting for the device".to_owned()),
            ("INPUT", format!("≈ {}", ms(buffer)), "estimate".to_owned()),
            ("ROUND TRIP", format!("≈ {}", ms(buffer * 2.)), "two buffers".to_owned()),
            ("FORMAT", format!("{:.1} kHz", cfg.sample_rate as f32 / 1000.).replace(".0 ", " "), format!("{} samples per buffer", cfg.buffer_size)),
        ],
    }
}

fn latency_readout(lat: Option<AudioLatency>, cfg: &AudioConfig) -> Div {
    let pal = p();
    let mut row = div().flex().gap(px(SP2)).py(px(SP2));
    for (caption, value, detail) in latency_tiles(lat, cfg) {
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .p(px(SP3))
                .rounded(px(r_md()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .flex()
                .flex_col()
                .gap(px(SP1))
                .child(label(caption, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                .child(mono(value, TEXT_LG, pal.text))
                .child(label(detail, TEXT_XS, pal.muted).whitespace_normal()),
        );
    }
    let mut col = div().flex().flex_col().child(row);
    if let Some(l) = lat.filter(|l| l.device_output_ms > WIRELESS_MS) {
        col = col.child(label(
            format!("This output adds {} on its own (Bluetooth or AirPlay?). Fine for listening; plug in wired headphones to play or record in time.", ms(l.device_output_ms)),
            TEXT_SM,
            pal.meter_mid,
        ).whitespace_normal());
    }
    col.child(label("Measured from the device's timestamps. The playhead is drawn where you hear it.", TEXT_SM, pal.faint).whitespace_normal())
}

fn apply_audio(ws: &mut Workspace, c: AudioConfig, cx: &mut Context<Workspace>) {
    match ws.b.engine.set_audio_config(c) {
        Ok(_) => {}
        Err(e) => ws.toast(format!("Audio device: {e}"), cx),
    }
    cx.notify();
}

fn plugins_page(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let scan = ws.b.plugins.scan_status();
    div()
        .flex()
        .flex_col()
        .child(section_title("PLUGINS"))
        .child(para("VST3 and CLAP plugins are looked for in the system's folders and in any folder you add. A plugin that cannot be loaded is not hidden: it is listed below with the reason, and you can try it again.", pal.muted))
        .child(setting_row(
            "Sandbox",
            Some("Run VST3/CLAP plugins in a separate process (applies to plugins loaded from now on)".into()),
            switch("sandbox", ws.b.engine.sandbox_plugins()).on_click(cx.listener(|this, _, _, cx| {
                let on = !this.b.engine.sandbox_plugins();
                this.b.engine.set_sandbox_plugins(on);
                cx.notify();
            })),
        ))
        .child(setting_row("Plugin folders", Some(scan.message.clone().into()), {
            let mut row = div().flex().items_center().gap(px(SP2));
            if scan.running {
                row = row.child(widgets::hbar(scan.fraction, 160., 6., pal.accent));
            }
            row.child(text_button("add-plugin-folder", "Add folder…", Some(Icon::FolderOpen), false).on_click(cx.listener(|_, _, _, cx| {
                let rx = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Custom VST3 / CLAP folder".into()),
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = rx.await
                        && let Some(dir) = paths.into_iter().next()
                    {
                        let _ = this.update(cx, |ws, cx| {
                            if ws.b.plugins.add_plugin_folder(dir.clone()) {
                                ws.b.plugins.rescan();
                                ws.toast(format!("Scanning {}", dir.display()), cx);
                            } else {
                                ws.toast("Folder already in plugin search list", cx);
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            })))
            .child(text_button("rescan", if scan.running { "Scanning…" } else { "Rescan" }, Some(Icon::Refresh), false).on_click(cx.listener(|this, _, _, cx| {
                this.b.plugins.rescan();
                cx.notify();
            })))
        }))
        .child(plugin_report_block(ws, cx))
        .into_any_element()
}

fn audio_page(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let cfg = ws.b.engine.audio_config();
    let hosts = ws.b.engine.audio_hosts();
    let devices: Vec<_> = ws.b.engine.audio_devices().into_iter().filter(|d| d.host == cfg.host).collect();
    let out_dev = cfg.output.as_ref().and_then(|id| devices.iter().find(|d| &d.id == id)).or_else(|| devices.iter().find(|d| d.output_channels > 0)).cloned();
    let in_dev = cfg.input.as_ref().and_then(|id| devices.iter().find(|d| &d.id == id)).or_else(|| devices.iter().find(|d| d.input_channels > 0)).cloned();
    let rates = out_dev.as_ref().map_or(SAMPLE_RATES.to_vec(), |d| d.sample_rates.clone());
    let range = out_dev.as_ref().map_or((32, 4096), |d| d.buffer_range);

    let host_items: Vec<(String, bool, bool)> = [AudioHost::Wasapi, AudioHost::WasapiExclusive, AudioHost::Asio, AudioHost::CoreAudio]
        .iter()
        .map(|h| (h.label().to_owned(), *h == cfg.host, hosts.contains(h)))
        .collect();
    let rate_items: Vec<(String, bool, bool)> =
        SAMPLE_RATES.iter().map(|r| (format!("{:.1} kHz", *r as f32 / 1000.).replace(".0 ", " "), *r == cfg.sample_rate, rates.contains(r))).collect();
    let buf_items: Vec<(String, bool, bool)> =
        BUFFER_SIZES.iter().map(|b| (b.to_string(), *b == cfg.buffer_size, *b >= range.0 && *b <= range.1)).collect();
    let one_way = cfg.buffer_ms();
    let latency = ws.b.engine.audio_latency();
    let channel_chips = |id: &'static str, count: u16, enabled: &[u16], output: bool, cx: &mut Context<Workspace>| {
        let items: Vec<(String, bool, bool)> = (0..count.max(1)).map(|c| (format!("{}", c + 1), enabled.contains(&c), count > 0)).collect();
        chip_row(id, items, cx, move |this, i, cx| {
            let mut c = this.b.engine.audio_config();
            let list = if output { &mut c.output_channels } else { &mut c.input_channels };
            let ch = i as u16;
            if let Some(pos) = list.iter().position(|x| *x == ch) {
                list.remove(pos);
            } else {
                list.push(ch);
                list.sort();
            }
            apply_audio(this, c, cx);
        })
    };
    let hosts_all = [AudioHost::Wasapi, AudioHost::WasapiExclusive, AudioHost::Asio, AudioHost::CoreAudio];
    div()
        .flex()
        .flex_col()
        .child(section_title("DRIVER"))
        .child(setting_row("Audio driver", Some("Host API used to talk to the device".into()), chip_row("host", host_items, cx, move |this, i, cx| {
            let c = AudioConfig { host: hosts_all[i], input: None, output: None, ..this.b.engine.audio_config() };
            apply_audio(this, c, cx);
        })))
        .child(section_title("DEVICES"))
        .child(setting_block("Output", None, device_list("out-dev", devices.iter().filter(|d| d.output_channels > 0).map(|d| (d.id.clone(), d.name.clone())).collect(), cfg.output.clone(), cx, true)))
        .child(setting_block("Input", None, device_list("in-dev", devices.iter().filter(|d| d.input_channels > 0).map(|d| (d.id.clone(), d.name.clone())).collect(), cfg.input.clone(), cx, false)))
        .child(section_title("FORMAT"))
        .child(setting_row("Sample rate", Some("Only rates the output device supports".into()), chip_row("rate", rate_items, cx, |this, i, cx| {
            let c = AudioConfig { sample_rate: SAMPLE_RATES[i], ..this.b.engine.audio_config() };
            apply_audio(this, c, cx);
        })))
        .child(setting_row("Buffer size", Some(format!("{one_way:.1} ms per buffer").into()), chip_row("buffer", buf_items, cx, |this, i, cx| {
            let c = AudioConfig { buffer_size: BUFFER_SIZES[i], ..this.b.engine.audio_config() };
            apply_audio(this, c, cx);
        })))
        .child(section_title("LATENCY"))
        .child(latency_readout(latency, &cfg))
        .child(section_title("CHANNELS"))
        .child(setting_row("Output channels", out_dev.as_ref().map(|d| d.name.clone().into()), channel_chips("out-ch", out_dev.as_ref().map_or(2, |d| d.output_channels), &cfg.output_channels, true, cx)))
        .child(setting_row("Input channels", in_dev.as_ref().map(|d| d.name.clone().into()), channel_chips("in-ch", in_dev.as_ref().map_or(0, |d| d.input_channels), &cfg.input_channels, false, cx)))
        .child(section_title("TEST"))
        .child(setting_row(
            "Test tone",
            Some("440 Hz on the selected outputs".into()),
            text_button("test-tone", "Play test tone", Some(Icon::Volume), false).on_click(cx.listener(|this, _, _, cx| {
                this.b.engine.test_tone();
                cx.notify();
            })),
        ))
        .child(div().pt(px(SP3)).child(label(format!("Engine: {}", ws.b.engine.status_label()), TEXT_SM, pal.faint)))
        .into_any_element()
}

/// Whether Preferences lists every plugin found (kept across redraws).
#[derive(Default)]
struct PluginListOpen(bool);
impl gpui::Global for PluginListOpen {}

/// What the plugin scan did: the folders it searched, the files it left out
/// and why (with a retry), and on request every plugin it found.
fn plugin_report_block(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = theme::p();
    let report = ws.b.plugins.plugin_report();
    let open = cx.try_global::<PluginListOpen>().is_some_and(|g| g.0);
    let mut box_ = div().p(px(SP2)).rounded(px(r_md())).bg(content(0.03)).border_1().border_color(border()).flex().flex_col().gap(px(SP1));
    box_ = box_.child(label("Folders searched", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD));
    if report.folders.is_empty() {
        // Before the first scan has finished: the system's folders in words.
        let defaults: Vec<String> = [aitrack_model::PluginFormat::Vst3, aitrack_model::PluginFormat::Clap]
            .into_iter()
            .flat_map(aitrack_host::scan::default_search_paths)
            .map(|p| p.display().to_string())
            .collect();
        box_ = box_.child(label(defaults.join(" · "), TEXT_XS, pal.faint));
    }
    for (i, f) in report.folders.iter().enumerate() {
        let fmt = plugin_format_name(f.format);
        let note = if !f.exists { "not found".to_owned() } else { format!("{} file{}", f.files, if f.files == 1 { "" } else { "s" }) };
        let path = f.path.clone();
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(SP2))
            .child(widgets::badge(fmt, pal.faint))
            .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(mono(crate::paint::fit(&f.path.display().to_string(), 360., TEXT_XS), TEXT_XS, pal.text)))
            .child(label(note, TEXT_XS, if f.exists { pal.muted } else { pal.solo }));
        if f.custom {
            row = row.child(
                widgets::sized_icon_button(("rm-plug-dir", i), Icon::X, false, 18., 11.).tooltip(widgets::tip("Stop searching this folder", None)).on_click(cx.listener(move |this, _, _, cx| {
                    if this.b.plugins.remove_plugin_folder(&path) {
                        this.b.plugins.rescan();
                        cx.notify();
                    }
                })),
            );
        }
        box_ = box_.child(row);
    }
    // Left out, each with why.
    if !report.problems.is_empty() {
        box_ = box_.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .pt(px(SP2))
                .child(label(format!("Left out ({})", report.problems.len()), TEXT_XS, pal.solo).font_weight(FontWeight::SEMIBOLD))
                .child(text_button("retry-all-plugins", "Retry all", Some(Icon::Refresh), false).on_click(cx.listener(|this, _, _, cx| {
                    for p in this.b.plugins.plugin_report().problems {
                        this.b.plugins.retry_plugin(&p.path);
                    }
                    cx.notify();
                }))),
        );
        for (i, p) in report.problems.iter().enumerate() {
            let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (retry, reveal) = (p.path.clone(), p.path.clone());
            box_ = box_.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(SP2))
                    .pt(px(SP1))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().flex().items_center().gap(px(SP1)).child(widgets::badge(plugin_format_name(p.format), pal.faint)).child(label(name, TEXT_SM, pal.text)).child(label(p.title.clone(), TEXT_XS, pal.solo)))
                            .child(para_wrapped(p.advice.clone(), pal.muted))
                            .when(!p.detail.is_empty(), |d| d.child(mono(crate::paint::fit(&p.detail, 420., TEXT_XS), TEXT_XS, pal.faint))),
                    )
                    .child(widgets::sized_icon_button(("plug-reveal", i), Icon::FolderOpen, false, 22., 13.).tooltip(widgets::tip("Show in the file manager", None)).on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&reveal))))
                    .child(text_button(("plug-retry", i), "Retry", Some(Icon::Refresh), false).on_click(cx.listener(move |this, _, _, cx| {
                        this.b.plugins.retry_plugin(&retry);
                        cx.notify();
                    }))),
            );
        }
    }
    // Found: on request.
    let found = ws.b.plugins.external_plugins();
    let grouped = group_found(found);
    let n = grouped.len();
    box_ = box_.child(
        div().pt(px(SP2)).child(
            text_button("plug-found-toggle", if open { format!("Hide the {n} found") } else { format!("Show the {n} found") }, None, false).on_click(cx.listener(|_, _, _, cx| {
                let now = cx.try_global::<PluginListOpen>().is_some_and(|g| g.0);
                cx.set_global(PluginListOpen(!now));
                cx.notify();
            })),
        ),
    );
    if open {
        for (name, vendor, formats) in grouped {
            box_ = box_.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SP2))
                    .child(div().flex_1().min_w(px(0.)).child(label(crate::paint::fit(&name, 220., TEXT_SM), TEXT_SM, pal.text)))
                    .child(label(crate::paint::fit(&vendor, 160., TEXT_XS), TEXT_XS, pal.faint))
                    .children(formats.into_iter().map(|f| widgets::badge(f, pal.muted))),
            );
        }
    }
    box_.into_any_element()
}

fn plugin_format_name(f: aitrack_model::PluginFormat) -> &'static str {
    match f {
        aitrack_model::PluginFormat::Vst3 => "VST3",
        aitrack_model::PluginFormat::Clap => "CLAP",
    }
}

/// The plugins found, one line per plugin whatever its formats: name, maker, formats.
fn group_found(found: &[crate::bridge::ExternalPlugin]) -> Vec<(String, String, Vec<&'static str>)> {
    crate::views::palette::devices::group_plugins(found.iter())
        .into_iter()
        .map(|g| (g.name().to_owned(), g.vendor().to_owned(), g.formats().into_iter().map(plugin_format_name).collect()))
        .collect()
}

/// Text that wraps onto several lines.
fn para_wrapped(text: String, color: gpui::Hsla) -> Div {
    div().text_size(px(fs(TEXT_XS))).line_height(px(14.)).text_color(color).child(text)
}

// ───────────────────────────── Performance ─────────────────────────────

const CACHE_BUDGET_MB: &[usize] = &[128, 256, 512, 1024, 2048];

/// The "Stream from disk" choices: off, then files at least this many
/// seconds long.
const STREAM_SECONDS: &[Option<u32>] = &[None, Some(30), Some(60), Some(120), Some(300)];

fn stream_choice_text(choice: Option<u32>) -> String {
    match choice {
        None => "Off".into(),
        Some(s) if s < 60 => format!("Over {s} s"),
        Some(s) => format!("Over {} min", s / 60),
    }
}

/// The setting a "Stream from disk" chip stands for (the size threshold is
/// kept as it is).
fn stream_settings_for(choice: Option<u32>, now: aitrack_engine::StreamSettings) -> aitrack_engine::StreamSettings {
    match choice {
        None => aitrack_engine::StreamSettings { enabled: false, ..now },
        Some(s) => aitrack_engine::StreamSettings { enabled: true, min_seconds: s, ..now },
    }
}

/// The detail line under "Stream from disk".
fn stream_detail(settings: aitrack_engine::StreamSettings, stats: aitrack_engine::StreamStats) -> String {
    let base = format!("Long files play from disk through a small buffer instead of RAM (also files over {} MB)", settings.min_megabytes);
    if !settings.enabled {
        return "Every file is loaded into RAM".into();
    }
    if stats.files == 0 {
        return base;
    }
    let mb = stats.buffer_bytes as f64 / (1024. * 1024.);
    let files = if stats.files == 1 { "1 file".to_owned() } else { format!("{} files", stats.files) };
    let late = if stats.underruns == 0 { String::new() } else { format!(" · {} late reads", stats.underruns) };
    format!("{base} · now {files}, {mb:.1} MB of buffers{late}")
}

/// The "Audio threads" choices: Auto (`0`), then fixed counts up to the
/// machine's logical cores.
fn audio_thread_choices() -> Vec<usize> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::iter::once(0).chain([1, 2, 3, 4, 6, 8, 12, 16].into_iter().filter(|&n| n <= cores.max(1))).collect()
}

fn audio_threads_text(setting: usize) -> String {
    if setting == 0 { format!("Auto ({})", aitrack_engine::auto_threads()) } else { setting.to_string() }
}

// ───────────────────────────── MIDI ─────────────────────────────

/// How long the activity light stays on after a MIDI message.
const MIDI_LIGHT: std::time::Duration = std::time::Duration::from_millis(250);

/// Turns the engine's running message count into a light that stays on for
/// a moment after each change.
#[derive(Clone, Copy, Debug, Default)]
struct MidiLight {
    count: u64,
    changed: Option<std::time::Instant>,
}

impl MidiLight {
    fn update(&mut self, count: u64, now: std::time::Instant) -> bool {
        if count != self.count {
            self.count = count;
            self.changed = Some(now);
        }
        self.changed.is_some_and(|at| now.duration_since(at) < MIDI_LIGHT)
    }
}

thread_local! {
    static LIGHT: Cell<MidiLight> = const { Cell::new(MidiLight { count: 0, changed: None }) };
}

/// What a port row says: on and open (messages arrive), on but not open
/// yet (or it refused to open), or switched off.
fn midi_status(port: &crate::bridge::MidiPortState) -> &'static str {
    match (port.enabled, port.open) {
        (true, true) => "Connected",
        (true, false) => "Waiting",
        (false, _) => "Off",
    }
}

/// The choices for the track live MIDI goes to: every armed instrument
/// track, or one instrument track. `(text, track, selected)`.
fn midi_targets(project: &aitrack_model::Project, current: Option<aitrack_model::TrackId>) -> Vec<(String, Option<aitrack_model::TrackId>, bool)> {
    let mut out = vec![("Armed tracks".to_owned(), None, current.is_none())];
    out.extend(project.tracks.iter().filter(|t| t.kind == aitrack_model::TrackKind::Instrument).map(|t| (t.name.clone(), Some(t.id), current == Some(t.id))));
    out
}

fn midi_page(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let ports = ws.b.engine.midi_ports();
    let target = ws.b.engine.midi_target();
    let lit = LIGHT.with(|l| {
        let mut light = l.get();
        let on = light.update(ws.b.engine.midi_message_count(), std::time::Instant::now());
        l.set(light);
        on
    });
    // The light fades on its own clock: keep painting while it is on.
    if lit {
        window.request_animation_frame();
    }
    let dot = |color: gpui::Hsla| div().size(px(8.)).flex_none().rounded_full().bg(color);

    let mut list = div().flex().flex_col();
    for (i, port) in ports.iter().enumerate() {
        let id = port.id.clone();
        let on = port.enabled;
        list = list.child(setting_row(
            port.name.clone(),
            Some(midi_status(port).into()),
            div().flex().items_center().gap(px(SP3)).child(dot(if port.open { pal.play } else { pal.faint })).child(
                switch(("midi-port", i), on).on_click(cx.listener(move |this, _, _, cx| {
                    this.b.engine.set_midi_port_enabled(&id, !on);
                    cx.notify();
                })),
            ),
        ));
    }
    if ports.is_empty() {
        list = list.child(setting_row("No MIDI inputs found", Some("Plug a keyboard or controller in: it opens by itself within a few seconds".into()), div()));
    }

    let choices = midi_targets(&ws.s.project, target);
    let tracks: Vec<Option<aitrack_model::TrackId>> = choices.iter().map(|c| c.1).collect();
    let items: Vec<(String, bool, bool)> = choices.into_iter().map(|(text, _, selected)| (text, selected, true)).collect();

    div()
        .flex()
        .flex_col()
        .child(section_title("INPUT PORTS"))
        .child(setting_row(
            "Activity",
            Some("Lights up when a note, controller or other message arrives from any port".into()),
            dot(if lit { pal.accent } else { pal.faint }),
        ))
        .child(list)
        .child(section_title("LIVE MIDI GOES TO"))
        .child(setting_row(
            "Track",
            Some("Armed tracks, or one instrument track".into()),
            chip_row("midi-target", items, cx, move |this, i, cx| {
                this.b.engine.set_midi_target(tracks.get(i).copied().flatten());
                cx.notify();
            }),
        ))
        .into_any_element()
}

/// The memory tile's detail line: the cache's use of its budget, split into
/// decoded files and warp renders when the renders' share is known (two
/// lines, so it fits a tile).
fn cache_detail(used_bytes: usize, budget_mb: usize, warp_bytes: Option<usize>) -> String {
    const MB: f64 = 1024. * 1024.;
    let used = used_bytes as f64 / MB;
    match warp_bytes {
        Some(warp) => {
            let warp = warp.min(used_bytes);
            format!("Sample cache: {used:.1} / {budget_mb} MB\n{:.1} decoded + {:.1} warp renders", (used_bytes - warp) as f64 / MB, warp as f64 / MB)
        }
        None => format!("Sample cache: {used:.1} / {budget_mb} MB"),
    }
}

fn performance_page(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let dsp_pct = ws.b.engine.cpu_load() * 100.;
    let app_pct = ws.usage.cpu * 100.;
    let ram = ws.usage.memory_label();
    let (used_bytes, budget_bytes) = ws.b.engine.sample_cache_stats();
    let budget_mb = (budget_bytes / (1024 * 1024)).max(1);
    let cache_text = cache_detail(used_bytes, budget_mb, Some(ws.b.engine.sample_cache_warp_bytes()));
    let low_power = ws.look.app.low_power;
    let sandbox = ws.b.engine.sandbox_plugins();
    let streaming = ws.b.engine.streaming();
    let stream_text = stream_detail(streaming, ws.b.engine.stream_stats());
    let stream_items: Vec<(String, bool, bool)> = STREAM_SECONDS
        .iter()
        .map(|c| {
            let on = match c {
                None => !streaming.enabled,
                Some(s) => streaming.enabled && streaming.min_seconds == *s,
            };
            (stream_choice_text(*c), on, true)
        })
        .collect();
    let threads_setting = ws.b.engine.audio_threads();
    let threads_active = ws.b.engine.active_audio_threads();
    let thread_choices = audio_thread_choices();
    let thread_items: Vec<(String, bool, bool)> = thread_choices.iter().map(|&n| (audio_threads_text(n), n == threads_setting, true)).collect();

    let tile = |caption: &'static str, value: String, detail: String| {
        let mut lines = div().flex().flex_col();
        for line in detail.lines() {
            lines = lines.child(label(line.to_owned(), TEXT_XS, pal.muted).whitespace_normal());
        }
        div()
            .flex_1()
            .min_w(px(0.))
            .p(px(SP2))
            .rounded(px(r_md()))
            .bg(content(0.03))
            .border_1()
            .border_color(border())
            .flex()
            .flex_col()
            .gap(px(SP1))
            .child(label(caption, TEXT_XS, pal.faint))
            .child(mono(value, TEXT_LG, pal.text).font_weight(FontWeight::SEMIBOLD))
            .child(lines)
    };
    let tiles = div()
        .py(px(SP2))
        .flex()
        .gap(px(SP2))
        .child(tile("AUDIO DSP", format!("{dsp_pct:.1}%"), "Audio callback load relative to buffer window".into()))
        .child(tile("PROCESS CPU", format!("{app_pct:.1}%"), "Total share across host & UI threads".into()))
        .child(tile("MEMORY", ram, cache_text));

    let cache_items: Vec<(String, bool, bool)> = CACHE_BUDGET_MB
        .iter()
        .map(|mb| {
            let text = if *mb >= 1024 { format!("{} GB", mb / 1024) } else { format!("{mb} MB") };
            (text, *mb == budget_mb, true)
        })
        .collect();

    let mut track_rows = div().flex().flex_col().gap(px(4.));
    for t in &ws.s.project.tracks {
        let mut stock_n = 0usize;
        let mut ext_n = 0usize;
        fn count_devs(devs: &[aitrack_model::Device], stock: &mut usize, ext: &mut usize) {
            for d in devs {
                match &d.kind {
                    aitrack_model::DeviceKind::Stock { .. } => *stock += 1,
                    aitrack_model::DeviceKind::Plugin { .. } => *ext += 1,
                    aitrack_model::DeviceKind::Rack(r) => {
                        for c in &r.chains {
                            count_devs(&c.devices, stock, ext);
                        }
                    }
                }
            }
        }
        count_devs(&t.devices, &mut stock_n, &mut ext_n);

        // Estimate decoded sample memory referenced by this track's audio clips.
        let mut seen_src = std::collections::HashSet::new();
        let mut sample_bytes = 0u64;
        for p in &t.arrangement {
            if let Some(clip) = ws.s.project.clips.get(&p.clip)
                && seen_src.insert(clip.source)
                && let Some(aitrack_model::Source::Audio(a)) = ws.s.project.sources.get(&clip.source)
            {
                sample_bytes += a.frames * a.channels.max(1) as u64 * 4;
            }
        }
        let sample_mb = sample_bytes as f64 / (1024. * 1024.);
        let dev_summary = if ext_n > 0 {
            format!("{stock_n} stock · {ext_n} ext")
        } else {
            format!("{stock_n} stock")
        };
        let ram_summary = if sample_bytes > 0 { format!("{sample_mb:.1} MB") } else { "—".into() };
        track_rows = track_rows.child(
            div()
                .px(px(SP2))
                .py(px(6.))
                .rounded(px(r_sm()))
                .bg(content(0.03))
                .flex()
                .items_center()
                .justify_between()
                .child(label(t.name.clone(), TEXT_SM, pal.text))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP3))
                        .child(label(dev_summary, TEXT_XS, pal.muted))
                        .child(mono(ram_summary, TEXT_XS, pal.faint)),
                ),
        );
    }

    div()
        .flex()
        .flex_col()
        .child(section_title("TELEMETRY"))
        .child(tiles)
        .child(section_title("AUDIO THREADS"))
        .child(setting_row(
            "Audio threads",
            Some(format!("Independent tracks run in parallel, exports too; same sound at any setting · now {threads_active}").into()),
            chip_row("audio-threads", thread_items, cx, move |this, i, cx| {
                if let Some(&n) = thread_choices.get(i) {
                    this.b.engine.set_audio_threads(n);
                }
                cx.notify();
            }),
        ))
        .child(section_title("POWER & UI"))
        .child(setting_row(
            "Low-power / battery mode",
            Some("Throttles meter & playhead refresh to ~20 Hz and skips UI transitions".into()),
            switch("low-power-mode", low_power).on_click(cx.listener(|this, _, _, cx| {
                this.set_look(false, cx, |a| a.low_power = !a.low_power);
            })),
        ))
        .child(section_title("MEMORY & CACHING"))
        .child(setting_row(
            "Sample cache budget",
            Some("LRU cap for decoded audio files kept in RAM (evicts unused clips first)".into()),
            chip_row("sample-cache-mb", cache_items, cx, |this, i, cx| {
                this.b.engine.set_sample_cache_mb(CACHE_BUDGET_MB[i]);
                cx.notify();
            }),
        ))
        .child(setting_row(
            "Stream from disk",
            Some(stream_text.into()),
            chip_row("stream-from-disk", stream_items, cx, move |this, i, cx| {
                if let Some(&choice) = STREAM_SECONDS.get(i) {
                    let now = this.b.engine.streaming();
                    this.b.engine.set_streaming(stream_settings_for(choice, now));
                }
                cx.notify();
            }),
        ))
        .child(setting_row(
            "Idle DSP sleep & FTZ/DAZ",
            Some("Silent effects sleep past their tail and subnormals flush to zero automatically".into()),
            label("Active", TEXT_SM, pal.accent),
        ))
        .child(section_title("PLUGIN ISOLATION"))
        .child(setting_row(
            "Sandbox third-party plugins",
            Some("Run VST3/CLAP plugins in external host processes to isolate crashes".into()),
            switch("sandbox-plugins", sandbox).on_click(cx.listener(|this, _, _, cx| {
                let next = !this.b.engine.sandbox_plugins();
                this.b.engine.set_sandbox_plugins(next);
                this.toast(if next { "Plugin sandbox enabled for newly loaded plugins" } else { "Plugin sandbox disabled for newly loaded plugins" }, cx);
                cx.notify();
            })),
        ))
        .when(!ws.s.project.tracks.is_empty(), |d| {
            d.child(section_title("TRACK BREAKDOWN")).child(track_rows)
        })
        .into_any_element()
}

// ───────────────────────────── Appearance ─────────────────────────────

fn theme_card(t: &Theme, active: bool, i: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let c = |r: Rgba8| r.to_hsla();
    let k = &t.colors;
    let name = t.name.clone();
    let dark = t.dark;
    let pal = p();
    div()
        .id(("theme-card", i))
        .w(px(148.))
        .flex()
        .flex_col()
        .gap(px(SP1))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            let name = name.clone();
            this.set_look(true, cx, |a| {
                if a.follow_system {
                    if dark { a.dark_theme = name } else { a.light_theme = name }
                } else {
                    a.theme = name;
                }
            });
        }))
        .child(
            // A miniature of the app painted in the card's own tokens.
            div()
                .h(px(88.))
                .p(px(SP1))
                .gap(px(SP1))
                .flex()
                .flex_col()
                .rounded(px(r_lg()))
                .bg(c(k.base))
                .border_2()
                .border_color(if active { pal.accent } else { border() })
                .child(div().h(px(10.)).flex().gap(px(3.)).child(div().w(px(10.)).h_full().rounded(px(2.)).bg(c(k.accent))).child(div().w(px(30.)).h_full().rounded(px(2.)).bg(c(k.raised))).child(div().flex_1()).child(div().w(px(14.)).h_full().rounded(px(2.)).bg(c(k.accent2))))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .gap(px(SP1))
                        .child(div().w(px(28.)).h_full().rounded(px(3.)).bg(c(k.panel)).p(px(3.)).flex().flex_col().gap(px(3.)).child(div().h(px(3.)).w(px(18.)).bg(c(k.text))).child(div().h(px(3.)).w(px(14.)).bg(c(k.muted))).child(div().h(px(3.)).w(px(16.)).bg(c(k.muted))))
                        .child(
                            div()
                                .flex_1()
                                .rounded(px(3.))
                                .bg(c(k.well))
                                .p(px(3.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(div().h(px(8.)).w(px(40.)).rounded(px(2.)).bg(c(k.accent)))
                                .child(div().h(px(8.)).w(px(56.)).ml(px(12.)).rounded(px(2.)).bg(c(k.play)))
                                .child(div().h(px(8.)).w(px(30.)).ml(px(24.)).rounded(px(2.)).bg(c(k.solo))),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(icon(if t.dark { Icon::Moon } else { Icon::Sun }, 12., pal.faint))
                .child(label(t.name.clone(), TEXT_SM, if active { pal.text } else { pal.muted }).font_weight(FontWeight::MEDIUM).flex_1())
                .when(active, |d| d.child(icon(Icon::Check, 12., pal.accent))),
        )
        .into_any_element()
}

fn seg<T: Copy + PartialEq + 'static>(id: &'static str, options: &[(T, &'static str)], current: T, seg_w: f32, cx: &mut Context<Workspace>, set: fn(&mut Appearance, T)) -> AnyElement {
    let values: Vec<T> = options.iter().map(|o| o.0).collect();
    let items: Vec<(Option<Icon>, &'static str)> = options.iter().map(|o| (None, o.1)).collect();
    let index = values.iter().position(|v| *v == current).unwrap_or(0);
    let weak = cx.entity().downgrade();
    let on: OnSelect = Rc::new(move |i, _, cx| {
        let v = values[i];
        let _ = weak.update(cx, |ws, cx| ws.set_look(false, cx, |a| set(a, v)));
    });
    widgets::segmented(id, &items, index, seg_w, on).into_any_element()
}

fn look_stepper(id: &'static str, text: String, cx: &mut Context<Workspace>, step: fn(&mut Appearance, f32)) -> Div {
    let (shell, dec, inc) = stepper(id, text);
    stepper_el((
        shell,
        dec.on_click(cx.listener(move |this, _, _, cx| this.set_look(false, cx, |a| step(a, -1.)))),
        inc.on_click(cx.listener(move |this, _, _, cx| this.set_look(false, cx, |a| step(a, 1.)))),
    ))
}

fn picker_panel(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let (target, h, s, v) = ws.look.picker.clone()?;
    let pal = p();
    let current = widgets::hsv_to_hsla(h, s, v);
    let presets: [u32; 12] = [0xF0783C, 0xFF2E97, 0xFF4D4D, 0xFFC857, 0x6BCB77, 0x2EC4D6, 0x4FA3FF, 0x7C6CF2, 0xA77BF3, 0xE0E0E0, 0x9AA0A6, 0x3B3D45];
    let mut swatches = div().flex().flex_wrap().gap(px(SP1));
    for (i, c) in presets.iter().enumerate() {
        let col = Rgba8::rgb(*c);
        let t = target.clone();
        swatches = swatches.child(widgets::swatch(("preset", i), col.to_hsla(), 20., false).on_click(cx.listener(move |this, _, _, cx| {
            let (h, s, v) = widgets::hsla_to_hsv(col.to_hsla());
            this.look.picker = Some((t.clone(), h, s, v));
            let t = t.clone();
            this.set_look(true, cx, |a| match t {
                PickTarget::Accent => a.accent = Some(col),
                PickTarget::Token(n) => {
                    a.overrides.insert(n.to_owned(), col);
                }
            });
        })));
    }
    let what = match &target {
        PickTarget::Accent => "Accent".to_owned(),
        PickTarget::Token(n) => (*n).to_owned(),
    };
    Some(
        div()
            .mt(px(SP2))
            .p(px(SP3))
            .gap(px(SP3))
            .flex()
            .rounded(px(r_lg()))
            .bg(pal.surface)
            .border_1()
            .border_color(border())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SP2))
                    .child(
                        div()
                            .id("sv")
                            .cursor_crosshair()
                            .child(widgets::sv_square(h, s, v, 200., 120., ws.look.sv_bounds.clone()))
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                this.pick_at(e.position, false, cx);
                                this.begin_drag(Drag::Pick { hue: false }, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("hue")
                            .cursor_pointer()
                            .child(widgets::hue_strip(h, 200., 12., ws.look.hue_bounds.clone()))
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                this.pick_at(e.position, true, cx);
                                this.begin_drag(Drag::Pick { hue: true }, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(SP2))
                    .child(label(what, TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
                    .child(div().flex().items_center().gap(px(SP2)).child(div().size(px(28.)).rounded(px(r_md())).bg(current).border_1().border_color(border_strong())).child(mono(Rgba8::from_hsla(current).hex(), TEXT_MD, pal.text)))
                    .child(swatches)
                    .child(text_button("picker-done", "Done", Some(Icon::Check), false).on_click(cx.listener(|this, _, _, cx| {
                        this.look.picker = None;
                        cx.notify();
                    }))),
            )
            .into_any_element(),
    )
}

fn appearance_page(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let a = ws.look.app.clone();
    let active_name = ws.look.effective.name.clone();

    // ── Themes ──
    let mut cards = div().flex().flex_wrap().gap(px(SP3));
    let themes: Vec<(Theme, bool)> = ws.look.lib.themes.iter().map(|(t, p)| (t.clone(), p.is_some())).collect();
    for (i, (t, _custom)) in themes.iter().enumerate() {
        let active = if a.follow_system { t.name == a.dark_theme || t.name == a.light_theme } else { t.name == active_name };
        cards = cards.child(theme_card(t, active, i, cx));
    }
    let follow = setting_row(
        "Follow system",
        Some(format!("Dark: {} · Light: {}", a.dark_theme, a.light_theme).into()),
        switch("follow-system", a.follow_system).on_click(cx.listener(|this, _, _, cx| this.set_look(true, cx, |a| a.follow_system = !a.follow_system))),
    );
    let file_actions = div()
        .flex()
        .gap(px(SP2))
        .child(text_button("dup-theme", "Duplicate", Some(Icon::Layers), false).on_click(cx.listener(|this, _, _, cx| {
            let mut t = this.look.effective.clone();
            t.name = appearance::copy_name(&this.look.lib, &t.name);
            t.author = "You".into();
            match appearance::save_theme(&t) {
                Ok(_) => {
                    this.look.lib = ThemeLibrary::load();
                    let name = t.name.clone();
                    this.set_look(true, cx, |a| {
                        a.theme = name;
                        a.follow_system = false;
                        a.overrides.clear();
                        a.accent = None;
                        a.adjust = theme::ThemeAdjust::default();
                        a.clip_intensity = 1.;
                    });
                    this.toast(format!("Created “{}”", t.name), cx);
                }
                Err(e) => this.toast(format!("Cannot save theme: {e}"), cx),
            }
        })))
        .child(text_button("import-theme", "Import…", Some(Icon::FolderOpen), false).on_click(cx.listener(|_, _, _, cx| {
            let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import theme".into()) });
            cx.spawn(async move |this, cx| {
                if let Ok(Ok(Some(paths))) = rx.await
                    && let Some(path) = paths.first() {
                        let result = appearance::import_theme(path);
                        let _ = this.update(cx, |ws, cx| match result {
                            Ok(t) => {
                                ws.look.lib = ThemeLibrary::load();
                                let name = t.name.clone();
                                ws.set_look(true, cx, |a| a.theme = name);
                            }
                            Err(e) => ws.toast(e, cx),
                        });
                    }
            })
            .detach();
        })))
        .child(text_button("export-theme", "Export…", Some(Icon::Send), false).on_click(cx.listener(|this, _, _, cx| {
            let t = this.look.effective.clone();
            let rx = cx.prompt_for_new_path(&appearance::themes_dir(), Some(&format!("{}.aitheme", t.name)));
            cx.spawn(async move |this, cx| {
                if let Ok(Ok(Some(path))) = rx.await {
                    let result = std::fs::write(&path, t.to_json());
                    let _ = this.update(cx, |ws, cx| match result {
                        Ok(()) => ws.toast(format!("Saved {}", path.display()), cx),
                        Err(e) => ws.toast(format!("Cannot save: {e}"), cx),
                    });
                }
            })
            .detach();
        })))
        .child(text_button("open-themes", "Open folder", Some(Icon::Folder), false).on_click(cx.listener(|_, _, _, cx| {
            let dir = appearance::themes_dir();
            let _ = std::fs::create_dir_all(&dir);
            cx.open_with_system(&dir);
        })));

    // ── Colour ──
    let accent_now = p().accent;
    let accent_row = setting_row(
        "Accent colour",
        Some(if a.accent.is_some() { "Custom".into() } else { "From the theme".into() }),
        div()
            .flex()
            .items_center()
            .gap(px(SP2))
            .child(widgets::swatch("accent-swatch", accent_now, 28., ws.look.picker.as_ref().is_some_and(|(t, ..)| *t == PickTarget::Accent)).on_click(cx.listener(|this, _, _, cx| this.pick(PickTarget::Accent, cx))))
            .child(mono(Rgba8::from_hsla(accent_now).hex(), TEXT_SM, pal.muted))
            .when(a.accent.is_some(), |d| {
                d.child(text_button("accent-reset", "Reset", None, false).on_click(cx.listener(|this, _, _, cx| this.set_look(true, cx, |a| a.accent = None))))
            }),
    );
    let picker_for_accent = ws.look.picker.as_ref().is_some_and(|(t, ..)| *t == PickTarget::Accent);
    let picker = picker_panel(ws, cx);

    let mut tokens = div().flex().flex_col();
    if ws.look.tokens_open {
        let query = ws.look.token_search.read(cx).text().trim().to_lowercase();
        tokens = tokens.child(div().py(px(SP2)).child(ws.look.token_search.clone()));
        for group in theme::TOKEN_GROUPS.iter().copied() {
            let names: Vec<&'static str> = theme::Colors::NAMES
                .iter()
                .copied()
                .filter(|n| theme::token_group(n) == group)
                .filter(|n| query.is_empty() || n.contains(query.as_str()) || theme::token_doc(n).to_lowercase().contains(query.as_str()) || group.to_lowercase().contains(query.as_str()))
                .collect();
            if names.is_empty() {
                continue;
            }
            let in_group: Vec<&'static str> = names.iter().copied().filter(|n| a.overrides.contains_key(*n)).collect();
            tokens = tokens.child(
                div()
                    .h(px(ROW_H))
                    .mt(px(SP2))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .gap(px(SP2))
                    .child(label(group.to_uppercase(), TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD).flex_1())
                    .when(!in_group.is_empty(), |d| {
                        d.child(text_button(gpui::ElementId::Name(format!("group-reset-{group}").into()), "Reset group", None, false).on_click(cx.listener(move |this, _, _, cx| {
                            let names = in_group.clone();
                            this.set_look(true, cx, |a| {
                                for n in &names {
                                    a.overrides.remove(*n);
                                }
                            })
                        })))
                    }),
            );
            for name in names {
                let value = ws.look.effective.colors.get(name).unwrap_or(Rgba8::rgb(0));
                let overridden = a.overrides.contains_key(name);
                let picking = ws.look.picker.as_ref().is_some_and(|(t, ..)| *t == PickTarget::Token(name));
                tokens = tokens.child(
                    div()
                        .h(px(ROW_H + 4.))
                        .px(px(SP2))
                        .gap(px(SP3))
                        .flex()
                        .items_center()
                        .rounded(px(r_md()))
                        .when(picking, |d| d.bg(pal.selection))
                        .child(widgets::swatch(gpui::ElementId::Name(format!("tok-{name}").into()), value.to_hsla(), 20., picking).on_click(cx.listener(move |this, _, _, cx| this.pick(PickTarget::Token(name), cx))))
                        .child(mono(name, TEXT_SM, pal.text).w(px(128.)))
                        .child(label(theme::token_doc(name), TEXT_SM, pal.faint).flex_1())
                        .child(mono(value.hex(), TEXT_XS, if overridden { pal.accent } else { pal.faint }))
                        .when(overridden, |d| {
                            d.child(widgets::sized_icon_button(gpui::ElementId::Name(format!("tok-reset-{name}").into()), Icon::Undo, false, 20., 12.).on_click(cx.listener(move |this, _, _, cx| {
                                this.set_look(true, cx, |a| {
                                    a.overrides.remove(name);
                                })
                            })))
                        }),
                );
                if picking
                    && let Some(p) = picker_panel(ws, cx) {
                        tokens = tokens.child(p);
                    }
            }
        }
    }

    // ── Theme adjustments (Live's Themes & Colors) ──
    let adj = a.adjust;
    let adj_row = |title: &'static str, hint: &'static str, id: &'static str, text: String, step: fn(&mut Appearance, f32), cx: &mut Context<Workspace>| setting_row(title, Some(hint.into()), look_stepper(id, text, cx, step));
    let adjustments = div()
        .flex()
        .flex_col()
        .child(adj_row("Hue", "Turns every colour round the wheel", "adj-hue", format!("{:+.0}°", adj.hue), |a, d| a.adjust.hue = wrap_hue(a.adjust.hue + d * 15.), cx))
        .child(adj_row("Saturation", "Greyer to more vivid", "adj-sat", format!("{:+.0}%", adj.saturation * 100.), |a, d| a.adjust.saturation = step_unit(a.adjust.saturation, d * 0.1), cx))
        .child(adj_row("Brightness", "Darker to lighter", "adj-bright", format!("{:+.0}%", adj.brightness * 100.), |a, d| a.adjust.brightness = step_unit(a.adjust.brightness, d * 0.05), cx))
        .child(adj_row("Contrast", "Flatter to harder", "adj-contrast", format!("{:+.0}%", adj.contrast * 100.), |a, d| a.adjust.contrast = step_unit(a.adjust.contrast, d * 0.1), cx))
        .child(adj_row(
            "Grid line intensity",
            "Arrangement, piano roll and session grids",
            "adj-grid",
            format!("{:.0}%", adj.grid * 100.),
            |a, d| a.adjust.grid = ((a.adjust.grid + d * 0.1) * 10.).round().clamp(0., 30.) / 10.,
            cx,
        ))
        .when(!adj.is_neutral(), |d| {
            d.child(div().py(px(SP2)).flex().justify_end().child(text_button("adj-reset", "Reset adjustments", Some(Icon::Undo), false).on_click(cx.listener(|this, _, _, cx| this.set_look(true, cx, |a| a.adjust = theme::ThemeAdjust::default())))))
        });

    // ── Clip & track colours ──
    let colour_rows = div()
        .flex()
        .flex_col()
        .child(setting_row(
            "Clip color intensity",
            Some("How saturated and strong clips are".into()),
            look_stepper("clip-intensity", format!("{:.0}%", a.clip_intensity * 100.), cx, |a, d| a.clip_intensity = ((a.clip_intensity + d * 0.1) * 10.).round().clamp(2., 20.) / 10.),
        ))
        .child(setting_row(
            "Track color intensity",
            Some("How much track headers wear their colour".into()),
            look_stepper("track-intensity", format!("{:.0}%", a.track_tint * 100.), cx, |a, d| a.track_tint = ((a.track_tint + d * 0.1) * 10.).round().clamp(0., 10.) / 10.),
        ))
        .child(setting_row(
            "Auto-assign track colors",
            Some("New tracks take the next palette colour".into()),
            switch("auto-track-colors", a.auto_track_colors).on_click(cx.listener(|this, _, _, cx| this.set_look(false, cx, |a| a.auto_track_colors = !a.auto_track_colors))),
        ))
        .child(setting_row(
            "New clips take the track color",
            Some("Off: each new clip picks its own colour".into()),
            switch("clips-take-track", a.clips_take_track_color).on_click(cx.listener(|this, _, _, cx| this.set_look(false, cx, |a| a.clips_take_track_color = !a.clips_take_track_color))),
        ))
        .child(setting_row(
            "Piano roll note color",
            Some("What colours the notes".into()),
            seg(
                "note-color",
                &NoteColor::ALL.map(|n| (n, n.label())),
                a.note_color,
                72.,
                cx,
                |a, v| a.note_color = v,
            ),
        ));

    // ── Background ──
    let bg = a.background.clone().filter(|b| !b.path.as_os_str().is_empty());
    let choose = text_button("bg-choose", if bg.is_some() { "Change image…" } else { "Choose image…" }, Some(Icon::FolderOpen), false).on_click(cx.listener(|_, _, _, cx| {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Background image (png, jpg, webp)".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.first().cloned() {
                    let ok = path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"));
                    let _ = this.update(cx, |ws, cx| {
                        if ok {
                            ws.set_look(false, cx, |a| {
                                let mut b = a.background.clone().unwrap_or_default();
                                b.path = path;
                                a.background = Some(b);
                            });
                        } else {
                            ws.toast("Choose a PNG, JPG or WebP image", cx);
                        }
                    });
                }
        })
        .detach();
    }));
    let mut bg_rows = div().flex().flex_col().child(setting_row(
        "Image",
        bg.as_ref().map(|b| crate::paint::fit(&b.path.display().to_string(), 300., TEXT_XS).into()),
        div().flex().gap(px(SP2)).child(choose).when(bg.is_some(), |d| {
            d.child(text_button("bg-remove", "Remove", Some(Icon::X), false).on_click(cx.listener(|this, _, _, cx| this.set_look(false, cx, |a| a.background = None))))
        }),
    ));
    if let Some(b) = bg.clone() {
        bg_rows = bg_rows
            .child(setting_row(
                "Fit",
                None,
                seg(
                    "bg-fit",
                    &[
                        (BgFit::Cover, "Cover"),
                        (BgFit::Contain, "Contain"),
                        (BgFit::Tile, "Tile"),
                    ],
                    b.fit,
                    80.,
                    cx,
                    |a, v| bg_mut(a).fit = v,
                ),
            ))
            .child(setting_row(
                "Blur",
                Some("Pre-blurred once, cached".into()),
                look_stepper("bg-blur", format!("{:.0} px", b.blur), cx, |a, d| {
                    bg_mut(a).blur = (bg_mut(a).blur + d * 4.).clamp(0., 40.)
                }),
            ))
            .child(setting_row(
                "Dim",
                None,
                look_stepper("bg-dim", format!("{:.0}%", b.dim * 100.), cx, |a, d| {
                    bg_mut(a).dim = (bg_mut(a).dim + d * 0.1).clamp(0., 0.9)
                }),
            ))
            .child(setting_row(
                "Glass opacity",
                Some("How solid translucent panes are".into()),
                look_stepper(
                    "bg-glass",
                    format!("{:.0}%", b.glass_opacity * 100.),
                    cx,
                    |a, d| {
                        bg_mut(a).glass_opacity = (bg_mut(a).glass_opacity + d * 0.1).clamp(0.3, 1.)
                    },
                ),
            ));
        let mut panes = div().flex().flex_wrap().gap(px(SP1));
        for (i, pane) in Pane::ALL.iter().copied().enumerate() {
            let on = b.glass.contains(&pane);
            panes = panes.child(checkbox(("glass", i), on, pane.label(), None).on_click(cx.listener(move |this, _, _, cx| {
                this.set_look(false, cx, |a| {
                    let g = &mut bg_mut(a).glass;
                    if let Some(i) = g.iter().position(|p| *p == pane) {
                        g.remove(i);
                    } else {
                        g.push(pane);
                    }
                })
            })));
        }
        bg_rows = bg_rows.child(setting_block("Translucent panes", None, panes));
    }

    div()
        .flex()
        .flex_col()
        .child(section_title("THEME"))
        .child(cards)
        .child(div().h(px(SP2)))
        .child(follow)
        .child(setting_row(
            "Theme files",
            Some(".aitheme in the config folder · reloads on change".into()),
            file_actions,
        ))
        .child(section_title("COLOUR"))
        .child(accent_row)
        .when(picker_for_accent, |d| d.children(picker))
        .child(setting_row(
            "Token overrides",
            Some(format!("{} overridden", a.overrides.len()).into()),
            div()
                .flex()
                .gap(px(SP2))
                .child(text_button("tokens-toggle", if ws.look.tokens_open { "Hide tokens" } else { "Edit tokens" }, Some(Icon::Sliders), false).on_click(cx.listener(|this, _, _, cx| {
                    this.look.tokens_open = !this.look.tokens_open;
                    cx.notify();
                })))
                .when(!a.overrides.is_empty(), |d| {
                    d.child(text_button("tokens-reset", "Reset all", None, false).on_click(cx.listener(|this, _, _, cx| this.set_look(true, cx, |a| a.overrides.clear()))))
                }),
        ))
        .child(tokens)
        .child(section_title("THEME ADJUSTMENTS"))
        .child(adjustments)
        .child(section_title("CLIP & TRACK COLOURS"))
        .child(colour_rows)
        .child(section_title("BACKGROUND"))
        .child(bg_rows)
        .child(section_title("LAYOUT & TYPE"))
        .child(setting_row(
            "UI scale",
            Some("80–150 %".into()),
            look_stepper(
                "ui-scale",
                format!("{:.0}%", a.ui_scale * 100.),
                cx,
                |a, d| a.ui_scale = ((a.ui_scale + d * 0.1) * 10.).round().clamp(8., 15.) / 10.,
            ),
        ))
        .child(setting_row(
            "Density",
            Some("Heights of bars, tracks and strips".into()),
            seg(
                "density",
                &[
                    (Density::Compact, "Compact"),
                    (Density::Comfortable, "Comfortable"),
                ],
                a.density,
                120.,
                cx,
                |a, v| a.density = v,
            ),
        ))
        .child(setting_row(
            "Corner radius",
            None,
            look_stepper("radius", format!("{:.0}%", a.radius * 100.), cx, |a, d| {
                a.radius = (a.radius + d * 0.25).clamp(0., 2.)
            }),
        ))
        .child(setting_row(
            "Font size",
            None,
            look_stepper("font-size", format!("{:.0} px", a.font_size), cx, |a, d| {
                a.font_size = (a.font_size + d).clamp(10., 16.)
            }),
        ))
        .child(section_title("DISPLAY STYLES"))
        .child(setting_row(
            "Waveforms",
            None,
            seg(
                "wave",
                &[
                    (WaveformStyle::Filled, "Filled"),
                    (WaveformStyle::Outline, "Outline"),
                ],
                a.waveform,
                120.,
                cx,
                |a, v| a.waveform = v,
            ),
        ))
        .child(setting_row(
            "Meters",
            None,
            seg(
                "meter",
                &[
                    (MeterStyle::Zones, "Zones"),
                    (MeterStyle::Segmented, "LED"),
                    (MeterStyle::Solid, "Solid"),
                ],
                a.meter,
                80.,
                cx,
                |a, v| a.meter = v,
            ),
        ))
        .child(setting_row(
            "Knobs",
            None,
            seg(
                "knob",
                &[(KnobStyle::Arc, "Arc"), (KnobStyle::Minimal, "Minimal")],
                a.knob,
                120.,
                cx,
                |a, v| a.knob = v,
            ),
        ))
        .child(setting_row(
            "Reduce motion",
            Some("Panels and fades jump straight to their end".into()),
            switch("reduce-motion", a.reduced_motion).on_click(cx.listener(|this, _, _, cx| this.set_look(false, cx, |a| a.reduced_motion = !a.reduced_motion))),
        ))
        .into_any_element()
}

/// A hue shift in degrees wrapped into -180..=180.
fn wrap_hue(h: f32) -> f32 {
    let w = (h + 180.).rem_euclid(360.) - 180.;
    if w <= -180. { 180. } else { w }.round()
}

/// A -1..=1 slider stepped by `d`, kept tidy to two decimals.
fn step_unit(v: f32, d: f32) -> f32 {
    ((v + d) * 100.).round().clamp(-100., 100.) / 100.
}

fn bg_mut(a: &mut Appearance) -> &mut Background {
    a.background.get_or_insert_with(Background::default)
}

/// The window backdrop: the background image (fit, dim) under every panel.
pub fn backdrop(ws: &Workspace) -> Option<AnyElement> {
    let path = ws.look.bg_image.clone()?;
    let b = ws.look.app.background.clone()?;
    let pal = p();
    let layer = match b.fit {
        BgFit::Tile => {
            let mut grid = div().size_full().flex().flex_wrap().overflow_hidden();
            for _ in 0..96 {
                grid = grid.child(img(path.clone()).size(px(256.)).flex_none());
            }
            grid.into_any_element()
        }
        fit => img(path).size_full().object_fit(if fit == BgFit::Cover { gpui::ObjectFit::Cover } else { gpui::ObjectFit::Contain }).into_any_element(),
    };
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(layer)
            .child(div().absolute().top_0().left_0().size_full().bg(with_alpha(pal.base, b.dim.clamp(0., 0.9))))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::splice_folder_status;
    use std::path::Path;

    #[test]
    fn the_splice_folder_status_says_found_missing_or_where_it_looked() {
        let here = std::env::temp_dir();
        let (t, warn) = splice_folder_status(false, Some(&here), Some(12));
        assert!(t.contains("12 sounds") && t.contains("on their own") && !warn);
        let (t, warn) = splice_folder_status(false, Some(&here), None);
        assert!(t.starts_with("Found at") && !warn);
        // Nothing chosen and nothing there: a hint, not an alarm, and where it looked.
        let (t, warn) = splice_folder_status(false, Some(Path::new("/no/such/Splice")), None);
        assert!(t.contains("~/Splice") && t.contains("Ableton") && !warn);
        // A folder the user chose that is gone is a problem.
        let (t, warn) = splice_folder_status(true, Some(Path::new("/no/such/Splice")), None);
        assert!(t.contains("/no/such/Splice") && warn);
    }

    use super::*;

    #[test]
    fn panels_follow_the_density_only_while_at_its_default() {
        let (c, k) = (Metrics::COMFORTABLE, Metrics::COMPACT);
        assert_eq!(follow_default(c.dock_h, c.dock_h, k.dock_h), k.dock_h);
        assert_eq!(follow_default(k.mixer_h, k.mixer_h, c.mixer_h), c.mixer_h);
        // A height the user chose stays put.
        assert_eq!(follow_default(333., c.dock_h, k.dock_h), 333.);
    }

    #[test]
    fn the_cache_line_splits_decoded_files_from_warp_renders() {
        const MB: usize = 1024 * 1024;
        assert_eq!(cache_detail(30 * MB, 512, None), "Sample cache: 30.0 / 512 MB");
        assert_eq!(cache_detail(30 * MB, 512, Some(10 * MB)), "Sample cache: 30.0 / 512 MB\n20.0 decoded + 10.0 warp renders");
        // The renders are part of the total, never more than it.
        assert_eq!(cache_detail(MB, 64, Some(5 * MB)), "Sample cache: 1.0 / 64 MB\n0.0 decoded + 1.0 warp renders");
    }

    #[test]
    fn the_stream_choices_set_the_length_and_keep_the_size() {
        let now = aitrack_engine::StreamSettings { enabled: true, min_seconds: 60, min_megabytes: 80 };
        assert_eq!(stream_settings_for(None, now), aitrack_engine::StreamSettings { enabled: false, ..now });
        assert_eq!(stream_settings_for(Some(300), aitrack_engine::StreamSettings { enabled: false, ..now }), aitrack_engine::StreamSettings { min_seconds: 300, ..now });
        let texts: Vec<String> = STREAM_SECONDS.iter().map(|c| stream_choice_text(*c)).collect();
        assert_eq!(texts, ["Off", "Over 30 s", "Over 1 min", "Over 2 min", "Over 5 min"]);
        // The default is one of the chips.
        let d = aitrack_engine::StreamSettings::default();
        assert!(STREAM_SECONDS.contains(&Some(d.min_seconds)));
    }

    #[test]
    fn the_stream_line_says_what_streams_and_what_came_late() {
        let on = aitrack_engine::StreamSettings::default();
        assert_eq!(stream_detail(aitrack_engine::StreamSettings::OFF, Default::default()), "Every file is loaded into RAM");
        assert!(!stream_detail(on, Default::default()).contains("now"));
        let busy = aitrack_engine::StreamStats { files: 3, buffer_bytes: 6 * 1024 * 1024, underruns: 0 };
        assert!(stream_detail(on, busy).ends_with("now 3 files, 6.0 MB of buffers"));
        let late = aitrack_engine::StreamStats { underruns: 2, ..busy };
        assert!(stream_detail(on, late).ends_with("· 2 late reads"));
    }

    #[test]
    fn the_midi_light_stays_on_a_moment_after_each_message() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut light = MidiLight::default();
        assert!(!light.update(0, t0), "no messages yet");
        assert!(light.update(3, t0 + Duration::from_millis(10)));
        assert!(light.update(3, t0 + Duration::from_millis(200)), "still on");
        assert!(!light.update(3, t0 + Duration::from_millis(400)), "faded");
        assert!(light.update(4, t0 + Duration::from_millis(500)), "a new message lights it again");
    }

    #[test]
    fn midi_ports_say_what_they_are_doing_and_targets_list_instruments() {
        let port = |enabled, open| crate::bridge::MidiPortState { id: "p".into(), name: "Keys".into(), enabled, open };
        assert_eq!(midi_status(&port(true, true)), "Connected");
        assert_eq!(midi_status(&port(true, false)), "Waiting");
        assert_eq!(midi_status(&port(false, true)), "Off");
        let mut s = crate::state::AppState::new(aitrack_model::Project::new("Midi"));
        let synth = s.add_track(aitrack_model::TrackKind::Instrument).unwrap();
        s.add_track(aitrack_model::TrackKind::Audio).unwrap();
        let none = midi_targets(&s.project, None);
        assert_eq!(none.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["Armed tracks", "MIDI 1"]);
        assert!(none[0].2 && !none[1].2);
        let picked = midi_targets(&s.project, Some(synth));
        assert!(!picked[0].2 && picked[1].2 && picked[1].1 == Some(synth));
    }

    #[test]
    fn latency_tiles_show_measured_figures() {
        let cfg = AudioConfig { sample_rate: 48_000, buffer_size: 256, ..AudioConfig::default() };
        let l = AudioLatency { sample_rate: 48_000, buffer_frames: 256, output_ms: 10., input_ms: 6., device_output_ms: 4.67, plugin_ms: 0. };
        let t = latency_tiles(Some(l), &cfg);
        assert_eq!(t[0], ("OUTPUT", "10.0 ms".to_owned(), "5.3 buffer + 4.7 ms device".to_owned()));
        assert_eq!(t[1].1, "6.0 ms");
        assert_eq!(t[2].1, "16.0 ms");
        assert_eq!((t[3].1.as_str(), t[3].2.as_str()), ("48 kHz", "256 samples per buffer"));
        // Plugin delay counts in the round trip; no input shows a dash.
        let t = latency_tiles(Some(AudioLatency { input_ms: 0., plugin_ms: 2., ..l }), &cfg);
        assert_eq!((t[1].1.as_str(), t[2].1.as_str()), ("—", "12.0 ms"));
        // Bluetooth-sized numbers drop the decimal.
        assert_eq!(latency_tiles(Some(AudioLatency { output_ms: 215.4, ..l }), &cfg)[0].1, "215 ms");
    }

    #[test]
    fn latency_tiles_estimate_before_the_first_callback() {
        let cfg = AudioConfig { sample_rate: 44_100, buffer_size: 441, ..AudioConfig::default() };
        let t = latency_tiles(None, &cfg);
        assert_eq!(t[0].1, "≈ 10.0 ms");
        assert_eq!(t[2].1, "≈ 20.0 ms");
        assert_eq!(t[3].1, "44.1 kHz");
        // A device that has not called back yet reports zeros: still an estimate.
        assert_eq!(latency_tiles(Some(AudioLatency::default()), &cfg)[0].1, "≈ 10.0 ms");
    }
}
