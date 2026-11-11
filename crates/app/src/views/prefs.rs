//! Preferences (Audio, Appearance) and the modal shell the Export dialog
//! shares. Appearance edits save immediately and cross-fade the theme;
//! audio edits go to `EngineBridge::set_audio_config`.

use crate::appearance::{self, Appearance, Background, BgFit, Pane, ThemeLibrary};
use crate::bridge::{AudioConfig, AudioHost, BUFFER_SIZES, SAMPLE_RATES};
use crate::icons::{Icon, icon};
use crate::motion;
use crate::theme::{self, *};
use crate::widgets::{self, OnSelect, checkbox, form_row, label, mono, section_title, stepper, stepper_el, switch, text_button};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    Preferences(PrefsPage),
    Export,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PickTarget {
    Accent,
    Token(&'static str),
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
    _watcher: Option<notify::RecommendedWatcher>,
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
            _watcher: watcher,
        }
    }
}

impl Workspace {
    /// Change appearance settings: saved at once, applied (cross-faded) next frame.
    pub fn set_look(&mut self, animate: bool, cx: &mut Context<Self>, f: impl FnOnce(&mut Appearance)) {
        f(&mut self.look.app);
        self.look.app.save();
        self.look.pending = Some(animate);
        cx.notify();
    }

    pub fn apply_look(&mut self, window: &mut Window, animate: bool, cx: &mut Context<Self>) {
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        self.look.effective = appearance::apply(&self.look.app, &self.look.lib, system_dark, animate);
        window.set_content_zoom(self.look.app.ui_scale.clamp(0.8, 1.5), cx);
        cx.set_reduce_motion(self.look.app.reduced_motion);
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

    /// The quick light/dark switch in the top bar.
    pub fn toggle_theme(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let dark = theme::is_dark();
        self.set_look(true, cx, |a| {
            a.follow_system = false;
            a.theme = if dark { a.light_theme.clone() } else { a.dark_theme.clone() };
        });
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
                .child(icon(if modal == Modal::Export { Icon::Send } else { Icon::Sliders }, 16., pal.accent))
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
                .child(nav("nav-look", Icon::Sparkles, "Appearance", PrefsPage::Appearance)),
        )
        .child(
            div()
                .id(if page == PrefsPage::Audio { "audio-scroll" } else { "look-scroll" })
                .flex_1()
                .min_w(px(0.))
                .overflow_y_scroll()
                .px(px(SP6))
                .pb(px(SP6))
                .child(motion::fade_in(gpui::ElementId::Name(format!("page-{page:?}").into()), div().child(body))),
        )
        .into_any_element()
}

// ───────────────────────────── Audio ─────────────────────────────

fn chip_row(id: &'static str, items: Vec<(String, bool, bool)>, cx: &mut Context<Workspace>, on: impl Fn(&mut Workspace, usize, &mut Context<Workspace>) + 'static) -> Div {
    let pal = p();
    let on = Rc::new(on);
    let mut row = div().flex().flex_wrap().gap(px(SP1));
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

fn apply_audio(ws: &mut Workspace, c: AudioConfig, cx: &mut Context<Workspace>) {
    match ws.b.engine.set_audio_config(c) {
        Ok(_) => {}
        Err(e) => ws.toast(format!("Audio device: {e}"), cx),
    }
    cx.notify();
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
    let scan = ws.b.plugins.scan_status();
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
        .child(form_row("Audio driver", Some("Host API used to talk to the device".into()), chip_row("host", host_items, cx, move |this, i, cx| {
            let c = AudioConfig { host: hosts_all[i], input: None, output: None, ..this.b.engine.audio_config() };
            apply_audio(this, c, cx);
        })))
        .child(section_title("DEVICES"))
        .child(form_row("Output", None, device_list("out-dev", devices.iter().filter(|d| d.output_channels > 0).map(|d| (d.id.clone(), d.name.clone())).collect(), cfg.output.clone(), cx, true)))
        .child(form_row("Input", None, device_list("in-dev", devices.iter().filter(|d| d.input_channels > 0).map(|d| (d.id.clone(), d.name.clone())).collect(), cfg.input.clone(), cx, false)))
        .child(section_title("FORMAT"))
        .child(form_row("Sample rate", Some("Only rates the output device supports".into()), chip_row("rate", rate_items, cx, |this, i, cx| {
            let c = AudioConfig { sample_rate: SAMPLE_RATES[i], ..this.b.engine.audio_config() };
            apply_audio(this, c, cx);
        })))
        .child(form_row("Buffer size", Some(format!("{one_way:.1} ms buffer · ≈ {:.1} ms round trip", one_way * 2.).into()), chip_row("buffer", buf_items, cx, |this, i, cx| {
            let c = AudioConfig { buffer_size: BUFFER_SIZES[i], ..this.b.engine.audio_config() };
            apply_audio(this, c, cx);
        })))
        .child(section_title("CHANNELS"))
        .child(form_row("Output channels", out_dev.as_ref().map(|d| d.name.clone().into()), channel_chips("out-ch", out_dev.as_ref().map_or(2, |d| d.output_channels), &cfg.output_channels, true, cx)))
        .child(form_row("Input channels", in_dev.as_ref().map(|d| d.name.clone().into()), channel_chips("in-ch", in_dev.as_ref().map_or(0, |d| d.input_channels), &cfg.input_channels, false, cx)))
        .child(section_title("TEST"))
        .child(form_row(
            "Test tone",
            Some("440 Hz on the selected outputs".into()),
            text_button("test-tone", "Play test tone", Some(Icon::Volume), false).on_click(cx.listener(|this, _, _, cx| {
                this.b.engine.test_tone();
                cx.notify();
            })),
        ))
        .child(section_title("PLUGINS"))
        .child(form_row(
            "Sandbox",
            Some("Run VST3/CLAP plugins in a separate process (applies to plugins loaded from now on)".into()),
            switch("sandbox", ws.b.engine.sandbox_plugins()).on_click(cx.listener(|this, _, _, cx| {
                let on = !this.b.engine.sandbox_plugins();
                this.b.engine.set_sandbox_plugins(on);
                cx.notify();
            })),
        ))
        .child(form_row("Plugin folders", Some(scan.message.clone().into()), {
            let mut row = div().flex().items_center().gap(px(SP2));
            if scan.running {
                row = row.child(widgets::hbar(scan.fraction, 160., 6., pal.accent));
            }
            row.child(text_button("rescan", if scan.running { "Scanning…" } else { "Rescan" }, Some(Icon::Refresh), false).on_click(cx.listener(|this, _, _, cx| {
                this.b.plugins.rescan();
                cx.notify();
            })))
        }))
        .child(div().pt(px(SP3)).child(label(format!("Engine: {}", ws.b.engine.status_label()), TEXT_SM, pal.faint)))
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
    let follow = form_row(
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
    let accent_row = form_row(
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
        for name in theme::Colors::NAMES.iter().copied() {
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
    let mut bg_rows = div().flex().flex_col().child(form_row(
        "Image",
        bg.as_ref().map(|b| crate::paint::fit(&b.path.display().to_string(), 300., TEXT_XS).into()),
        div().flex().gap(px(SP2)).child(choose).when(bg.is_some(), |d| {
            d.child(text_button("bg-remove", "Remove", Some(Icon::X), false).on_click(cx.listener(|this, _, _, cx| this.set_look(false, cx, |a| a.background = None))))
        }),
    ));
    if let Some(b) = bg.clone() {
        bg_rows = bg_rows
            .child(form_row("Fit", None, seg("bg-fit", &[(BgFit::Cover, "Cover"), (BgFit::Contain, "Contain"), (BgFit::Tile, "Tile")], b.fit, 80., cx, |a, v| bg_mut(a).fit = v)))
            .child(form_row("Blur", Some("Pre-blurred once, cached".into()), look_stepper("bg-blur", format!("{:.0} px", b.blur), cx, |a, d| bg_mut(a).blur = (bg_mut(a).blur + d * 4.).clamp(0., 40.))))
            .child(form_row("Dim", None, look_stepper("bg-dim", format!("{:.0}%", b.dim * 100.), cx, |a, d| bg_mut(a).dim = (bg_mut(a).dim + d * 0.1).clamp(0., 0.9))))
            .child(form_row("Glass opacity", Some("How solid translucent panes are".into()), look_stepper("bg-glass", format!("{:.0}%", b.glass_opacity * 100.), cx, |a, d| bg_mut(a).glass_opacity = (bg_mut(a).glass_opacity + d * 0.1).clamp(0.3, 1.))));
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
        bg_rows = bg_rows.child(form_row("Translucent panes", None, panes));
    }

    div()
        .flex()
        .flex_col()
        .child(section_title("THEME"))
        .child(cards)
        .child(div().h(px(SP2)))
        .child(follow)
        .child(form_row("Theme files", Some(".aitheme in the config folder · reloads on change".into()), file_actions))
        .child(section_title("COLOUR"))
        .child(accent_row)
        .when(picker_for_accent, |d| d.children(picker))
        .child(form_row(
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
        .child(section_title("BACKGROUND"))
        .child(bg_rows)
        .child(section_title("LAYOUT & TYPE"))
        .child(form_row("UI scale", Some("80–150 %".into()), look_stepper("ui-scale", format!("{:.0}%", a.ui_scale * 100.), cx, |a, d| a.ui_scale = ((a.ui_scale + d * 0.1) * 10.).round().clamp(8., 15.) / 10.)))
        .child(form_row("Density", None, seg("density", &[(Density::Compact, "Compact"), (Density::Comfortable, "Comfortable")], a.density, 104., cx, |a, v| a.density = v)))
        .child(form_row("Corner radius", None, look_stepper("radius", format!("{:.0}%", a.radius * 100.), cx, |a, d| a.radius = (a.radius + d * 0.25).clamp(0., 2.))))
        .child(form_row("Font size", None, look_stepper("font-size", format!("{:.0} px", a.font_size), cx, |a, d| a.font_size = (a.font_size + d).clamp(10., 16.))))
        .child(section_title("DISPLAY STYLES"))
        .child(form_row("Waveforms", None, seg("wave", &[(WaveformStyle::Filled, "Filled"), (WaveformStyle::Outline, "Outline")], a.waveform, 88., cx, |a, v| a.waveform = v)))
        .child(form_row("Meters", None, seg("meter", &[(MeterStyle::Zones, "Zones"), (MeterStyle::Segmented, "LED"), (MeterStyle::Solid, "Solid")], a.meter, 80., cx, |a, v| a.meter = v)))
        .child(form_row("Knobs", None, seg("knob", &[(KnobStyle::Arc, "Arc"), (KnobStyle::Minimal, "Minimal")], a.knob, 88., cx, |a, v| a.knob = v)))
        .child(form_row(
            "Reduce motion",
            Some("Panels and fades jump straight to their end".into()),
            switch("reduce-motion", a.reduced_motion).on_click(cx.listener(|this, _, _, cx| this.set_look(false, cx, |a| a.reduced_motion = !a.reduced_motion))),
        ))
        .into_any_element()
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
