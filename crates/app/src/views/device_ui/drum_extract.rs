//! `fx.drum_extract`: RX-inspired drum stem, one-shot & loop extractor UI.
//!
//! Shows real-time percussive band meters (Kick / Snare / Hat / Transient)
//! plus a 64-band spectrum from the pass-through analyzer, lets the user pick
//! any WAV/MP3/FLAC/OGG file (or load the selected timeline audio clip), runs
//! offline clean extraction (`aitrack-ai::drum_extract`), and lets the user
//! audition or drop each isolated one-shot, loop, and clean drum stem onto
//! tracks one by one.

use super::Dev;
use super::kit::{self, Rect};
use crate::bridge::{DrumClassTag, DrumExtractMode, DrumExtractRequest, DrumExtraction, JobId};
use crate::icons::{Icon, icon};
use crate::paint;
use crate::theme::*;
use crate::views::meters::draw_spectrum;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_model::{DeviceId, Source};
use aitrack_stock::fx::drum_extract::{BANDS, p as param};
use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, PathPromptOptions,
    StatefulInteractiveElement, Styled, canvas, div, prelude::FluentBuilder, px,
};
use std::path::{Path, PathBuf};

/// Per-device persistent UI state for `fx.drum_extract`.
#[derive(Clone, Debug, Default)]
pub struct ExtractViewState {
    pub source_path: Option<PathBuf>,
    pub active_job: Option<JobId>,
    pub progress: f32,
    pub progress_label: String,
    pub error: Option<String>,
    pub result: Option<DrumExtraction>,
    pub filter: Option<DrumClassTag>,
    pub playing_path: Option<PathBuf>,
    pub export_note: Option<String>,
}

/// Build an extraction request from the stock device's current parameters.
pub fn build_request(dev: &Dev, path: PathBuf) -> DrumExtractRequest {
    let de_bleed = (dev.plain(param::DE_BLEED) / 100.0).clamp(0.0, 1.0);
    let de_reverb = (dev.plain(param::DE_REVERB) / 100.0).clamp(0.0, 1.0);
    let transient = (dev.plain(param::TRANSIENT) / 100.0).clamp(0.0, 1.0);
    let sensitivity = (dev.plain(param::SENSITIVITY) / 100.0).clamp(0.0, 1.0);
    let loop_bars = match dev.choice(param::LOOP_BARS) {
        0 => 1,
        2 => 4,
        _ => 2,
    };
    let engine = match dev.choice(param::ENGINE) {
        1 => DrumExtractMode::PureDsp,
        2 => DrumExtractMode::DemucsHybrid,
        _ => DrumExtractMode::Auto,
    };
    let normalize_db = match dev.choice(param::NORMALIZE) {
        1 => Some(-3.0),
        2 => Some(-0.3),
        3 => None,
        _ => Some(-1.0),
    };
    DrumExtractRequest { path, engine, de_bleed, de_reverb, transient, sensitivity, loop_bars, normalize_db }
}

/// Find the currently selected audio clip's file path (or the first audio clip).
pub fn selected_audio_clip_path(ws: &Workspace) -> Option<PathBuf> {
    let p = &ws.s.project;
    let audio_of = |clip: &aitrack_model::ClipId| {
        let c = p.clips.get(clip)?;
        match p.sources.get(&c.source)? {
            Source::Audio(a) => Some(a.path.clone()),
            _ => None,
        }
    };
    ws.s.selected_clip.as_ref().and_then(audio_of).or_else(|| p.clips.keys().find_map(audio_of))
}

/// Kick off an extraction job for `device`.
pub fn trigger_extract(ws: &mut Workspace, device: DeviceId, req: DrumExtractRequest, cx: &mut Context<Workspace>) {
    let path = req.path.clone();
    let job = ws.b.ai.extract_drums(req);
    let st = ws.fx.drum_extract.entry(device).or_default();
    st.source_path = Some(path);
    st.active_job = Some(job);
    st.progress = 0.0;
    st.progress_label = "Analyzing spectrum & isolating drums…".into();
    st.error = None;
    st.export_note = None;
    ws.ensure_ai_poll(cx);
    cx.notify();
}

/// Copy clean stem, loops and one-shots (organised by drum class) to `dest_dir`.
pub fn export_all_wavs(res: &DrumExtraction, dest_dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dest_dir)?;
    let mut copied = 0usize;
    if res.clean_stem_path.exists()
        && let Some(name) = res.clean_stem_path.file_name()
    {
        std::fs::copy(&res.clean_stem_path, dest_dir.join(name))?;
        copied += 1;
    }
    let loops_dir = dest_dir.join("Loops");
    if !res.loops.is_empty() {
        std::fs::create_dir_all(&loops_dir)?;
    }
    for l in &res.loops {
        if l.path.exists()
            && let Some(name) = l.path.file_name()
        {
            std::fs::copy(&l.path, loops_dir.join(name))?;
            copied += 1;
        }
    }
    let shots_dir = dest_dir.join("OneShots");
    for s in &res.oneshots {
        if s.path.exists()
            && let Some(name) = s.path.file_name()
        {
            let sub = shots_dir.join(s.class.label());
            std::fs::create_dir_all(&sub)?;
            std::fs::copy(&s.path, sub.join(name))?;
            copied += 1;
        }
    }
    Ok(copied)
}

fn class_color(c: DrumClassTag, pal: &Palette) -> Hsla {
    match c {
        DrumClassTag::Kick => pal.series1,
        DrumClassTag::Snare => pal.series2,
        DrumClassTag::Clap => pal.series3,
        DrumClassTag::ClosedHat => pal.series4,
        DrumClassTag::OpenHat => pal.series5,
        DrumClassTag::Tom => pal.series6,
        DrumClassTag::Perc => pal.series7,
        DrumClassTag::Crash => pal.series8,
    }
}

fn count_class(res: &DrumExtraction, c: DrumClassTag) -> usize {
    res.oneshots.iter().filter(|o| o.class == c).count()
}

fn mini_waveform(peaks: Vec<f32>, color: Hsla) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let r = Rect::of(b);
            if peaks.is_empty() || r.w <= 2. || r.h <= 2. {
                return;
            }
            let n = peaks.len().max(1);
            let step = r.w / n as f32;
            let mid = r.y + r.h * 0.5;
            for (i, &pk) in peaks.iter().enumerate() {
                let amp = pk.clamp(0.05, 1.0) * (r.h * 0.45);
                let bw = (step * 0.68).max(1.2);
                let x = r.x + i as f32 * step;
                window.paint_quad(gpui::fill(paint::rect(x, mid - amp, bw, amp * 2.0), with_alpha(color, 0.85)));
            }
        },
    )
    .w(px(56.))
    .h(px(18.))
    .flex_none()
    .into_any_element()
}

fn action_btn(id: (&'static str, usize), icon_kind: Icon, text: &str, active: bool, accent: bool, _cx: &mut Context<Workspace>) -> gpui::Stateful<gpui::Div> {
    let pal = p();
    let bg = if accent {
        pal.accent
    } else if active {
        accent_alpha(0.22)
    } else {
        content(0.07)
    };
    let hover = if accent {
        with_alpha(pal.accent, 0.86)
    } else if active {
        accent_alpha(0.30)
    } else {
        content(0.13)
    };
    let fg = if accent {
        pal.on_accent
    } else if active {
        pal.accent
    } else {
        pal.text
    };
    div()
        .id(id)
        .flex_none()
        .h(px(22.))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .bg(bg)
        .hover(move |s| s.bg(hover))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon(icon_kind, 11., fg))
        .child(label(text.to_owned(), TEXT_XS, fg).font_weight(FontWeight::MEDIUM))
}

fn band_meter_pill(name: &str, val: f32, color: Hsla) -> AnyElement {
    let pal = p();
    let pct = val.clamp(0.0, 1.0);
    div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .px(px(SP1 + 2.))
        .h(px(18.))
        .rounded(px(r_sm()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .child(label(name.to_owned(), TEXT_XS, pal.faint))
        .child(
            div()
                .w(px(36.))
                .h(px(5.))
                .rounded(px(2.))
                .bg(content(0.08))
                .overflow_hidden()
                .child(div().h_full().w(px(36. * pct)).bg(color)),
        )
        .into_any_element()
}

fn toggle_preview(ws: &mut Workspace, device: DeviceId, path: &Path, cx: &mut Context<Workspace>) {
    let st = ws.fx.drum_extract.entry(device).or_default();
    let same = st.playing_path.as_deref() == Some(path);
    if same {
        ws.b.engine.preview(None);
        st.playing_path = None;
    } else {
        ws.b.engine.preview(Some(path));
        st.playing_path = Some(path.to_path_buf());
    }
    cx.notify();
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let device = dev.id;
    let state = ws.fx.drum_extract.get(&device).cloned().unwrap_or_default();
    let clip_fallback = selected_audio_clip_path(ws);
    let current_path = state.source_path.clone().or(clip_fallback.clone());
    let file_label = current_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Drop or pick WAV / MP3 / FLAC…".into());
    let running = state.active_job.is_some();

    // Clone values needed inside click closures.
    let req_preview = current_path.clone().map(|path| build_request(dev, path));
    let can_from_clip = clip_fallback.is_some();
    let clip_path_for_btn = clip_fallback;

    // ── Header toolbar: Source picker, From Clip, Extract button, Export All ──
    let pick_btn = action_btn(("de-pick", dev.key(1)), Icon::FolderOpen, "Load WAV / MP3…", false, false, cx)
        .tooltip(widgets::tip("Choose any WAV, MP3, FLAC, or OGG song/loop to isolate clean drums", None))
        .on_click(cx.listener(move |_this, _, _, cx| {
            let rx = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Select WAV / MP3 audio file".into()),
            });
            cx.spawn(async move |weak, cx| {
                if let Ok(Ok(Some(paths))) = rx.await
                    && let Some(path) = paths.into_iter().next()
                {
                    let _ = weak.update(cx, |ws, cx| {
                        let st = ws.fx.drum_extract.entry(device).or_default();
                        st.source_path = Some(path.clone());
                        st.error = None;
                        cx.notify();
                    });
                }
            })
            .detach();
        }));

    let from_clip_btn = action_btn(("de-clip", dev.key(2)), Icon::Waveform, "From Clip", false, false, cx)
        .tooltip(widgets::tip("Load source audio from the selected timeline clip", None))
        .when(!can_from_clip, |d| d.opacity(0.45))
        .on_click(cx.listener(move |this, _, _, cx| {
            let Some(path) = clip_path_for_btn.clone() else {
                this.toast("No audio clip in project", cx);
                return;
            };
            let st = this.fx.drum_extract.entry(device).or_default();
            st.source_path = Some(path);
            st.error = None;
            cx.notify();
        }));

    let extract_btn = action_btn(
        ("de-run", dev.key(3)),
        if running { Icon::Loader } else { Icon::Sparkles },
        if running { "Extracting…" } else { "Extract Drums" },
        running,
        !running && req_preview.is_some(),
        cx,
    )
    .when(req_preview.is_none() && !running, |d| d.opacity(0.45))
    .on_click(cx.listener(move |this, _, _, cx| {
        if let Some(req) = req_preview.clone() {
            trigger_extract(this, device, req, cx);
        } else {
            this.toast("Select a WAV or MP3 track first", cx);
        }
    }));

    let export_res = state.result.clone();
    let export_btn = action_btn(("de-export", dev.key(4)), Icon::Download, "Export All…", false, false, cx)
        .tooltip(widgets::tip("Save clean stem, loops, and individual one-shots organized by folder", None))
        .when(export_res.is_none(), |d| d.opacity(0.45))
        .on_click(cx.listener(move |_this, _, _, cx| {
            let Some(res) = export_res.clone() else { return };
            let rx = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("Export clean drums folder".into()),
            });
            cx.spawn(async move |weak, cx| {
                if let Ok(Ok(Some(paths))) = rx.await
                    && let Some(dest) = paths.into_iter().next()
                {
                    let count = export_all_wavs(&res, &dest);
                    let _ = weak.update(cx, |ws, cx| {
                        let st = ws.fx.drum_extract.entry(device).or_default();
                        match count {
                            Ok(n) => {
                                st.export_note = Some(format!("Exported {n} WAVs"));
                                ws.toast(format!("Exported {n} clean drum WAVs to {}", dest.display()), cx);
                            }
                            Err(e) => {
                                st.error = Some(format!("Export failed: {e}"));
                            }
                        }
                        cx.notify();
                    });
                }
            })
            .detach();
        }));

    let header = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(pick_btn)
        .child(from_clip_btn)
        .child(
            div()
                .flex_1()
                .min_w(px(80.))
                .h(px(22.))
                .px(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .overflow_hidden()
                .child(mono(file_label, TEXT_XS, if current_path.is_some() { pal.text } else { pal.faint })),
        )
        .child(extract_btn)
        .child(export_btn);

    // ── Live spectrum + percussive activity meters + parameter strip ──
    let meter = dev.meter.clone();
    let enabled = dev.enabled;
    let spectrum = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let r = Rect::of(b);
            kit::freq_grid(window, cx, r, false);
            if meter.len() >= BANDS {
                let pal = p();
                draw_spectrum(window, r, &meter[..BANDS], -90., if enabled { pal.accent } else { pal.muted });
            }
        },
    )
    .size_full();

    let controls_row = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(
            div()
                .w(px(if dev.big { 280. } else { 180. }))
                .h(px(58.))
                .rounded(px(r_md()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .overflow_hidden()
                .child(spectrum),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(SP1))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(SP1))
                        .child(band_meter_pill("KICK", dev.m(BANDS), pal.series1))
                        .child(band_meter_pill("SNARE", dev.m(BANDS + 1), pal.series2))
                        .child(band_meter_pill("HAT", dev.m(BANDS + 2), pal.series4))
                        .child(band_meter_pill("PUNCH", dev.m(BANDS + 3), pal.accent))
                        .when_some(state.export_note.clone(), |d, note| {
                            d.child(label(note, TEXT_XS, pal.play))
                        }),
                )
                .child(kit::strip(
                    ws,
                    dev,
                    &[param::DE_BLEED, param::DE_REVERB, param::TRANSIENT, param::SENSITIVITY, param::LOOP_BARS],
                    &[param::ENGINE, param::NORMALIZE],
                    cx,
                )),
        );

    // ── Progress / Error banner ──
    let status_banner = if running {
        let frac = state.progress.clamp(0.02, 1.0);
        Some(
            div()
                .flex_none()
                .px(px(SP2))
                .py(px(SP1))
                .rounded(px(r_sm()))
                .bg(pal.surface)
                .border_1()
                .border_color(border())
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(label(state.progress_label.clone(), TEXT_XS, pal.text))
                        .child(mono(format!("{:.0}%", frac * 100.0), TEXT_XS, pal.accent)),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(content(0.08))
                        .overflow_hidden()
                        .child(div().h_full().w(gpui::relative(frac)).bg(pal.accent)),
                )
                .into_any_element(),
        )
    } else {
        state.error.clone().map(|err| {
            div()
                .flex_none()
                .px(px(SP2))
                .py(px(SP1))
                .rounded(px(r_sm()))
                .bg(with_alpha(pal.record, 0.14))
                .border_1()
                .border_color(with_alpha(pal.record, 0.45))
                .child(label(err, TEXT_XS, pal.record))
                .into_any_element()
        })
    };

    // ── Results split view: Loops + Stem (left) & One-Shots one by one (right) ──
    let body: AnyElement = if let Some(res) = state.result.clone() {
        let bpm_badge = res.bpm.map(|b| format!("{b:.1} BPM")).unwrap_or_else(|| "Free Tempo".into());
        let stem_playing = state.playing_path.as_deref() == Some(res.clean_stem_path.as_path());
        let stem_path_play = res.clean_stem_path.clone();
        let stem_path_add = res.clean_stem_path.clone();
        let stem_name = format!("{} (Clean Drums)", res.source_name);
        let stem_bpm = res.bpm;

        // Left column: Clean Stem card + Bar-aligned Loops list.
        let mut left_list = div()
            .id(("de-loops", dev.key(10)))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(SP1))
            .child(
                div()
                    .p(px(SP1 + 2.))
                    .rounded(px(r_sm()))
                    .bg(pal.surface)
                    .border_1()
                    .border_color(border())
                    .flex()
                    .items_center()
                    .gap(px(SP1))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(label("Full Clean Drum Stem", TEXT_XS, pal.text).font_weight(FontWeight::SEMIBOLD))
                            .child(mono(format!("{} · {:.1}s", res.engine_used, res.duration_sec), TEXT_XS, pal.faint)),
                    )
                    .child(
                        action_btn(("de-stem-play", dev.key(11)), if stem_playing { Icon::Stop } else { Icon::Play }, if stem_playing { "Stop" } else { "Play" }, stem_playing, false, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                toggle_preview(this, device, &stem_path_play, cx);
                            })),
                    )
                    .child(
                        action_btn(("de-stem-add", dev.key(12)), Icon::Add, "Track", false, false, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.insert_extracted_audio(stem_path_add.clone(), stem_name.clone(), stem_bpm, cx);
                            })),
                    ),
            );

        for (idx, lp) in res.loops.iter().enumerate() {
            let playing = state.playing_path.as_deref() == Some(lp.path.as_path());
            let play_p = lp.path.clone();
            let add_p = lp.path.clone();
            let add_n = lp.name.clone();
            let lp_bpm = Some(lp.bpm);
            left_list = left_list.child(
                div()
                    .p(px(SP1 + 2.))
                    .rounded(px(r_sm()))
                    .bg(pal.surface)
                    .border_1()
                    .border_color(border())
                    .flex()
                    .items_center()
                    .gap(px(SP1))
                    .child(mini_waveform(lp.peaks.clone(), pal.accent2))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(label(lp.name.clone(), TEXT_XS, pal.text).font_weight(FontWeight::MEDIUM))
                            .child(mono(format!("{} bars · {:.1} BPM · {:.0}% clean", lp.bars, lp.bpm, lp.isolation_score * 100.0), TEXT_XS, pal.faint)),
                    )
                    .child(
                        action_btn(("de-lp-play", dev.key(100 + idx)), if playing { Icon::Stop } else { Icon::Play }, if playing { "Stop" } else { "Play" }, playing, false, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                toggle_preview(this, device, &play_p, cx);
                            })),
                    )
                    .child(
                        action_btn(("de-lp-add", dev.key(200 + idx)), Icon::Add, "Track", false, false, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.insert_extracted_audio(add_p.clone(), add_n.clone(), lp_bpm, cx);
                            })),
                    ),
            );
        }

        // Right column: Filter chips + Individual one-shots (one by one).
        let mut filter_bar = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .gap(px(3.))
            .child(
                div()
                    .id(("de-f-all", dev.key(20)))
                    .px(px(6.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(if state.filter.is_none() { accent_alpha(0.24) } else { content(0.06) })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.fx.drum_extract.entry(device).or_default().filter = None;
                        cx.notify();
                    }))
                    .child(mono(format!("All {}", res.oneshots.len()), TEXT_XS, if state.filter.is_none() { pal.accent } else { pal.muted })),
            );

        for (ci, &cls) in DrumClassTag::ALL.iter().enumerate() {
            let n = count_class(&res, cls);
            if n == 0 {
                continue;
            }
            let active = state.filter == Some(cls);
            let col = class_color(cls, &pal);
            filter_bar = filter_bar.child(
                div()
                    .id(("de-f-cls", dev.key(30 + ci)))
                    .px(px(6.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(if active { with_alpha(col, 0.24) } else { content(0.06) })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let st = this.fx.drum_extract.entry(device).or_default();
                        st.filter = if st.filter == Some(cls) { None } else { Some(cls) };
                        cx.notify();
                    }))
                    .child(div().size(px(6.)).rounded(px(3.)).bg(col))
                    .child(mono(format!("{} {n}", cls.label()), TEXT_XS, if active { col } else { pal.muted })),
            );
        }

        let mut shots_list = div()
            .id(("de-shots", dev.key(40)))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(3.));

        for (idx, shot) in res.oneshots.iter().filter(|o| state.filter.is_none_or(|f| f == o.class)).enumerate() {
            let col = class_color(shot.class, &pal);
            let playing = state.playing_path.as_deref() == Some(shot.path.as_path());
            let play_p = shot.path.clone();
            let add_p = shot.path.clone();
            let add_n = shot.name.clone();
            shots_list = shots_list.child(
                div()
                    .px(px(SP1 + 2.))
                    .py(px(3.))
                    .rounded(px(r_sm()))
                    .bg(pal.surface)
                    .border_1()
                    .border_color(border())
                    .flex()
                    .items_center()
                    .gap(px(SP1))
                    .child(div().w(px(3.)).h(px(20.)).rounded(px(2.)).bg(col))
                    .child(mini_waveform(shot.peaks.clone(), col))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(label(shot.name.clone(), TEXT_XS, pal.text).font_weight(FontWeight::MEDIUM))
                            .child(mono(
                                format!("@{:.2}s · {:.0}ms · {:.0}% clean", shot.time_sec, shot.duration_sec * 1000.0, shot.isolation_score * 100.0),
                                TEXT_XS,
                                pal.faint,
                            )),
                    )
                    .child(
                        action_btn(("de-sh-play", ((device.0 as usize) << 16) + idx), if playing { Icon::Stop } else { Icon::Play }, if playing { "Stop" } else { "Play" }, playing, false, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                toggle_preview(this, device, &play_p, cx);
                            })),
                    )
                    .child(
                        action_btn(("de-sh-add", ((device.0 as usize) << 16) + idx), Icon::Add, "Track", false, false, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.insert_extracted_audio(add_p.clone(), add_n.clone(), None, cx);
                            })),
                    ),
            );
        }

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .gap(px(SP2))
            .child(
                div()
                    .w(px(if dev.big { 360. } else { 290. }))
                    .h_full()
                    .p(px(SP1 + 2.))
                    .rounded(px(r_md()))
                    .bg(pal.well)
                    .border_1()
                    .border_color(border())
                    .flex()
                    .flex_col()
                    .gap(px(SP1))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(label("LOOPS & CLEAN STEM", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                            .child(mono(bpm_badge, TEXT_XS, pal.accent2)),
                    )
                    .child(left_list),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .p(px(SP1 + 2.))
                    .rounded(px(r_md()))
                    .bg(pal.well)
                    .border_1()
                    .border_color(border())
                    .flex()
                    .flex_col()
                    .gap(px(SP1))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(label("CLEAN ONE-SHOTS (ONE BY ONE)", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                            .child(mono(format!("{} slices", res.oneshots.len()), TEXT_XS, pal.accent)),
                    )
                    .child(filter_bar)
                    .child(shots_list),
            )
            .into_any_element()
    } else {
        div()
            .flex_1()
            .min_h(px(0.))
            .rounded(px(r_md()))
            .bg(pal.well)
            .border_1()
            .border_color(border())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SP1))
            .child(icon(Icon::Drum, 22., pal.accent))
            .child(label("Load a WAV or MP3 track and click Extract Drums", TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM))
            .child(label("Isolates clean Kick, Snare, Clap, Hats, Toms & bar-aligned Drum Loops using RX-style HPSS + Spectral De-Bleed", TEXT_XS, pal.faint))
            .into_any_element()
    };

    kit::panel(if dev.big { 1040. } else { 760. })
        .child(header)
        .child(controls_row)
        .when_some(status_banner, |d, b| d.child(b))
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{DrumLoop, DrumOneShot};

    #[test]
    fn export_all_organizes_subfolders_by_class() {
        let dir = std::env::temp_dir().join(format!("aitrack-ui-de-test-{}", std::process::id()));
        let src = dir.join("src");
        let dst = dir.join("dst");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&src).unwrap();

        let stem = src.join("clean.wav");
        let lp = src.join("loop.wav");
        let kick = src.join("kick_01.wav");
        for f in [&stem, &lp, &kick] {
            std::fs::write(f, b"RIFF").unwrap();
        }
        let res = DrumExtraction {
            source_path: src.join("song.mp3"),
            source_name: "song".into(),
            engine_used: "PureDsp".into(),
            sample_rate: 44_100,
            duration_sec: 4.0,
            bpm: Some(120.0),
            clean_stem_path: stem,
            oneshots: vec![DrumOneShot {
                id: 1,
                name: "Kick 01".into(),
                class: DrumClassTag::Kick,
                time_sec: 0.0,
                duration_sec: 0.25,
                peak_db: -1.0,
                isolation_score: 0.9,
                path: kick,
                peaks: vec![1.0, 0.5],
            }],
            loops: vec![DrumLoop {
                name: "Loop 1".into(),
                bpm: 120.0,
                bars: 2,
                start_sec: 0.0,
                duration_sec: 4.0,
                isolation_score: 0.92,
                path: lp,
                peaks: vec![1.0, 0.4],
            }],
            out_dir: src.clone(),
        };

        let n = export_all_wavs(&res, &dst).unwrap();
        assert_eq!(n, 3);
        assert!(dst.join("clean.wav").exists());
        assert!(dst.join("Loops/loop.wav").exists());
        assert!(dst.join("OneShots/Kick/kick_01.wav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
