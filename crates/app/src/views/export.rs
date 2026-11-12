//! The Export dialog (Ctrl/Cmd+Shift+E): mixdown or stems, WAV (16-bit
//! dithered, 24-bit, 32-bit float) or MP3 (always 320 kbps CBR, mixdown
//! only), sample rate, range, normalize, tail and destination, with
//! progress and cancel. Renders through `EngineBridge::export`.

use crate::bridge::{ExportFormat, ExportMode, ExportOptions, ExportProgress, ExportRange, WavDepth};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{self, OnSelect, checkbox, form_row, label, mono, section_title, stepper, stepper_el, switch, text_button};
use crate::workspace::Workspace;
use aitrack_model::{Tick, TrackId, TrackKind};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeChoice {
    Song,
    Loop,
    Selection,
}

pub struct Running {
    pub progress: ExportProgress,
    pub cancel: Arc<AtomicBool>,
}

pub struct ExportDialog {
    pub stems: bool,
    pub mp3: bool,
    pub depth: WavDepth,
    pub rate: u32,
    pub range: RangeChoice,
    pub normalize: bool,
    pub tail: f32,
    pub folder: PathBuf,
    pub tracks: BTreeSet<TrackId>,
    pub running: Option<Running>,
    pub last: Option<ExportProgress>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
        Self {
            stems: false,
            mp3: false,
            depth: WavDepth::Int24,
            rate: 48_000,
            range: RangeChoice::Song,
            normalize: false,
            tail: 2.,
            folder: home.join("Music").join("AI-Track Exports"),
            tracks: BTreeSet::new(),
            running: None,
            last: None,
        }
    }
}

/// Tracks that can become stems (audio-carrying, not Main).
fn stem_tracks(ws: &Workspace) -> Vec<(TrackId, String, aitrack_model::Color, TrackKind)> {
    ws.s.project.content_tracks().filter(|t| t.kind.carries_audio()).map(|t| (t.id, t.name.clone(), t.color, t.kind)).collect()
}

fn selection(ws: &Workspace) -> Option<(Tick, Tick)> {
    let (t, pl) = ws.s.selected_placement?;
    let p = ws.s.project.track(t)?.arrangement.iter().find(|p| p.id == pl)?;
    let c = ws.s.project.clips.get(&p.clip)?;
    Some((p.start, p.start + c.length))
}

impl Workspace {
    pub fn open_export(&mut self, cx: &mut Context<Self>) {
        if self.export.tracks.is_empty() {
            self.export.tracks = stem_tracks(self).into_iter().filter(|t| t.3 != TrackKind::Return).map(|t| t.0).collect();
        }
        if self.export_name.read(cx).text().is_empty() {
            let name = self.s.project.name.clone();
            self.export_name.update(cx, |i, cx| i.set_text(&name, cx));
        }
        self.open_modal(crate::views::prefs::Modal::Export, cx);
    }

    pub fn export_options(&self, cx: &gpui::App) -> ExportOptions {
        let e = &self.export;
        ExportOptions {
            format: if e.mp3 && !e.stems { ExportFormat::Mp3 } else { ExportFormat::Wav(e.depth) },
            sample_rate: e.rate,
            range: match e.range {
                RangeChoice::Song => ExportRange::Song,
                RangeChoice::Loop => ExportRange::Loop,
                RangeChoice::Selection => selection(self).map_or(ExportRange::Song, |(start, end)| ExportRange::Selection { start, end }),
            },
            mode: if e.stems { ExportMode::Stems { tracks: e.tracks.iter().copied().collect() } } else { ExportMode::Mixdown },
            normalize: e.normalize,
            tail_seconds: e.tail,
            folder: e.folder.clone(),
            file_name: self.export_name.read(cx).text().trim().to_owned(),
        }
    }

    fn start_export(&mut self, cx: &mut Context<Self>) {
        let options = self.export_options(cx);
        if let Err(e) = options.validate() {
            self.toast(e, cx);
            return;
        }
        if options.format == ExportFormat::Mp3
            && let Err(e) = self.b.engine.mp3_available() {
                self.toast(e, cx);
                return;
            }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded::<ExportProgress>();
        let flag = cancel.clone();
        self.b.engine.export(
            options,
            &self.s.project,
            Box::new(move |p| {
                let _ = tx.send_blocking(p);
                !flag.load(Ordering::Relaxed)
            }),
        );
        self.export.running = Some(Running { progress: ExportProgress { fraction: 0., stage: "Starting".into(), done: false, cancelled: false, error: None, files: Vec::new() }, cancel });
        self.export.last = None;
        cx.spawn(async move |this, cx| {
            while let Ok(p) = rx.recv().await {
                let done = p.done;
                let _ = this.update(cx, |ws, cx| {
                    if done {
                        ws.export.running = None;
                        let msg = if p.cancelled {
                            "Export cancelled".to_owned()
                        } else if let Some(e) = &p.error {
                            format!("Export failed: {e}")
                        } else {
                            format!("Exported {} file{}", p.files.len(), if p.files.len() == 1 { "" } else { "s" })
                        };
                        ws.export.last = Some(p);
                        ws.toast(msg, cx);
                    } else if let Some(r) = &mut ws.export.running {
                        r.progress = p;
                    }
                    cx.notify();
                });
                if done {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }
}

fn seg(id: &'static str, items: &[&'static str], selected: usize, seg_w: f32, cx: &mut Context<Workspace>, set: fn(&mut Workspace, usize)) -> AnyElement {
    let weak = cx.entity().downgrade();
    let on: OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |ws, cx| {
            if ws.export.running.is_none() {
                set(ws, i);
                cx.notify();
            }
        });
    });
    let items: Vec<_> = items.iter().map(|t| (None, *t)).collect();
    widgets::segmented(id, &items, selected, seg_w, on).into_any_element()
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let e = &ws.export;
    let (stems, mp3) = (e.stems, e.mp3 && !e.stems);
    let has_selection = selection(ws).is_some();
    let has_loop = ws.s.project.loop_region.is_some();
    let rates = [44_100u32, 48_000, 88_200, 96_000];
    let rate_index = rates.iter().position(|r| *r == e.rate).unwrap_or(1);
    let depth_index = match e.depth {
        WavDepth::Int16Dithered => 0,
        WavDepth::Int24 => 1,
        WavDepth::Float32 => 2,
    };
    let range_index = match e.range {
        RangeChoice::Song => 0,
        RangeChoice::Loop => 1,
        RangeChoice::Selection => 2,
    };
    let running = e.running.as_ref().map(|r| r.progress.clone());
    let options = ws.export_options(cx);
    let valid = options.validate();
    let mp3_err = if mp3 { ws.b.engine.mp3_available().err() } else { None };

    let format_control = div()
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(seg("exp-format", &["WAV", "MP3"], if mp3 { 1 } else { 0 }, 72., cx, |ws, i| {
            if !ws.export.stems {
                ws.export.mp3 = i == 1;
            }
        }))
        .when(stems, |d| d.child(label("Stems are always WAV", TEXT_SM, pal.faint)));
    let quality: AnyElement = if mp3 {
        div().flex().flex_col().gap(px(SP1)).child(div().flex().items_center().gap(px(SP2)).child(widgets::badge("320 kbps CBR", pal.accent2)).child(label("MP3 is always exported at the highest bitrate", TEXT_SM, pal.faint))).when_some(mp3_err.clone(), |d, e| d.child(div().max_w(px(420.)).text_size(px(fs(TEXT_SM))).text_color(pal.record).child(e))).into_any_element()
    } else {
        seg("exp-depth", &["16-bit · dither", "24-bit", "32-bit float"], depth_index, 112., cx, |ws, i| ws.export.depth = [WavDepth::Int16Dithered, WavDepth::Int24, WavDepth::Float32][i])
    };
    let (shell, dec, inc) = stepper("exp-tail", format!("{:.1} s", e.tail));
    let tail = stepper_el((
        shell,
        dec.on_click(cx.listener(|this, _, _, cx| {
            this.export.tail = (this.export.tail - 0.5).max(0.);
            cx.notify();
        })),
        inc.on_click(cx.listener(|this, _, _, cx| {
            this.export.tail = (this.export.tail + 0.5).min(30.);
            cx.notify();
        })),
    ));

    let mut stem_list = div().flex().flex_wrap().gap(px(SP1));
    if stems {
        for (id, name, color, kind) in stem_tracks(ws) {
            let on = ws.export.tracks.contains(&id);
            let text = match kind {
                TrackKind::Group => format!("{name} (group)"),
                TrackKind::Return => format!("{name} (return)"),
                _ => name,
            };
            stem_list = stem_list.child(div().w(px(196.)).child(checkbox(("stem", id.0 as usize), on, text, Some(track_color(color))).on_click(cx.listener(move |this, _, _, cx| {
                if !this.export.tracks.remove(&id) {
                    this.export.tracks.insert(id);
                }
                cx.notify();
            }))));
        }
    }

    let folder = e.folder.display().to_string();
    let footer: AnyElement = match running {
        Some(p) => div()
            .flex()
            .items_center()
            .gap(px(SP3))
            .child(icon(Icon::Loader, 16., pal.accent))
            .child(div().flex_1().flex().flex_col().gap(px(SP1)).child(div().flex().justify_between().child(label(p.stage.clone(), TEXT_SM, pal.text)).child(mono(format!("{:.0}%", p.fraction * 100.), TEXT_SM, pal.muted))).child(widgets::hbar(p.fraction, 400., 6., pal.accent)))
            .child(text_button("exp-cancel", "Cancel", Some(Icon::X), false).on_click(cx.listener(|this, _, _, cx| {
                if let Some(r) = &this.export.running {
                    r.cancel.store(true, Ordering::Relaxed);
                }
                cx.notify();
            })))
            .into_any_element(),
        None => div()
            .flex()
            .items_center()
            .gap(px(SP2))
            .child(match &valid {
                Err(msg) => label(*msg, TEXT_SM, pal.record).into_any_element(),
                Ok(()) => match &ws.export.last {
                    Some(l) if l.error.is_some() => label(format!("Last export failed: {}", l.error.clone().unwrap_or_default()), TEXT_SM, pal.record).into_any_element(),
                    Some(l) if !l.cancelled && l.error.is_none() => label(format!("Last export: {}", l.files.first().map_or(String::new(), |f| f.display().to_string())), TEXT_SM, pal.play).into_any_element(),
                    _ => label(summary(&options), TEXT_SM, pal.muted).into_any_element(),
                },
            })
            .child(div().flex_1())
            .child(text_button("exp-close", "Close", None, false).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
            .child(text_button("exp-go", "Export", Some(Icon::Send), true).when(valid.is_err() || mp3_err.is_some(), |d| d.opacity(0.5)).on_click(cx.listener(|this, _, _, cx| this.start_export(cx))))
            .into_any_element(),
    };

    div()
        .flex()
        .flex_col()
        .child(
            div()
                .id("export-body")
                .max_h(px(560.))
                .overflow_y_scroll()
                .px(px(SP6))
                .pb(px(SP4))
                .child(section_title("WHAT"))
                .child(form_row("Mode", Some("One file, or one WAV per track".into()), seg("exp-mode", &["Mixdown", "Stems"], if stems { 1 } else { 0 }, 96., cx, |ws, i| {
                    ws.export.stems = i == 1;
                    if ws.export.stems {
                        ws.export.mp3 = false;
                    }
                })))
                .when(stems, |d| d.child(form_row("Tracks", Some(format!("{} selected", ws.export.tracks.len()).into()), stem_list)))
                .child(form_row("Range", Some(if has_selection { "Selection = the selected clip".into() } else { "Select a clip to export a selection".into() }), seg("exp-range", &["Song", "Loop", "Selection"], range_index, 88., cx, |ws, i| {
                    ws.export.range = [RangeChoice::Song, RangeChoice::Loop, RangeChoice::Selection][i]
                })))
                .when(!has_loop && range_index == 1, |d| d.child(label("The project has no loop; the song is exported.", TEXT_SM, pal.faint)))
                .child(section_title("FORMAT"))
                .child(form_row("Format", None, format_control))
                .child(form_row(if mp3 { "Bitrate" } else { "Bit depth" }, if mp3 { None } else { Some("Dither is applied only at 16-bit".into()) }, quality))
                .child(form_row("Sample rate", None, seg("exp-rate", &["44.1 kHz", "48 kHz", "88.2 kHz", "96 kHz"], rate_index, 80., cx, |ws, i| ws.export.rate = [44_100, 48_000, 88_200, 96_000][i])))
                .child(section_title("PROCESSING"))
                .child(form_row("Normalize", Some("Peak to -0.3 dBFS".into()), switch("exp-normalize", ws.export.normalize).on_click(cx.listener(|this, _, _, cx| {
                    this.export.normalize = !this.export.normalize;
                    cx.notify();
                }))))
                .child(form_row("Tail", Some("Let reverbs and delays ring out".into()), tail))
                .child(section_title("DESTINATION"))
                .child(form_row("File name", None, ws.export_name.clone()))
                .child(form_row(
                    "Folder",
                    None,
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .child(div().flex_1().min_w(px(0.)).h(px(CONTROL_H)).px(px(SP2)).flex().items_center().rounded(px(r_md())).bg(pal.well).border_1().border_color(border()).child(label(crate::paint::fit(&folder, 260., TEXT_SM), TEXT_SM, pal.muted)))
                        .child(text_button("exp-folder", "Choose…", Some(Icon::FolderOpen), false).on_click(cx.listener(|_, _, _, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Export folder".into()) });
                            cx.spawn(async move |this, cx| {
                                if let Ok(Ok(Some(paths))) = rx.await
                                    && let Some(p) = paths.into_iter().next() {
                                        let _ = this.update(cx, |ws, cx| {
                                            ws.export.folder = p;
                                            cx.notify();
                                        });
                                    }
                            })
                            .detach();
                        }))),
                )),
        )
        .child(div().flex_none().px(px(SP6)).py(px(SP3)).border_t_1().border_color(border()).child(footer))
        .into_any_element()
}

fn summary(o: &ExportOptions) -> String {
    let fmt = match o.format {
        ExportFormat::Mp3 => "MP3 320 kbps".to_owned(),
        ExportFormat::Wav(WavDepth::Int16Dithered) => "WAV 16-bit".into(),
        ExportFormat::Wav(WavDepth::Int24) => "WAV 24-bit".into(),
        ExportFormat::Wav(WavDepth::Float32) => "WAV 32-bit float".into(),
    };
    let what = match &o.mode {
        ExportMode::Mixdown => "Mixdown".to_owned(),
        ExportMode::Stems { tracks } => format!("{} stems", tracks.len()),
    };
    format!("{what} · {fmt} · {:.1} kHz", o.sample_rate as f32 / 1000.)
}

#[allow(dead_code)]
fn _weight() -> FontWeight {
    FontWeight::MEDIUM
}
