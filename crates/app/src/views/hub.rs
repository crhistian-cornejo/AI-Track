//! The Content Hub (bundled packs, optional packs installed in one click,
//! and links to free packs, instruments and plugins, with their licences) and "Organize on disk" (plan preview →
//! run → undo).

use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{self, label, mono, section_title, text_button};
use crate::workspace::Workspace;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, PathPromptOptions, StatefulInteractiveElement,
    Styled, div, prelude::FluentBuilder, px,
};

pub fn render_hub(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let mut body = div().id("hub-body").max_h(px(620.)).overflow_y_scroll().px(px(SP4)).pb(px(SP4)).flex().flex_col().gap(px(SP1));
    body = body.child(div().px(px(SP2)).pb(px(SP2)).text_size(px(fs(TEXT_SM))).line_height(px(16.)).text_color(pal.muted).child(
        "Bundled packs ship with AI-Track. Optional packs download here and are added to your library when done. Everything else downloads from its official page under its own licence; AI-Track never redistributes it.",
    ));
    body = body.children(crate::views::palette::pack_list(ws, cx));
    body.into_any_element()
}

fn human_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    }
}

pub fn render_organize(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let o = ws.organize.clone();
    let dest = o.dest.as_ref().map_or("Choose a destination folder".to_owned(), |d| d.display().to_string());
    let seg = |id: &'static str, items: [&'static str; 2], index: usize, cx: &mut Context<Workspace>, set: fn(&mut Workspace, bool)| {
        let weak = cx.entity().downgrade();
        let on: widgets::OnSelect = std::rc::Rc::new(move |i, _, cx| {
            let _ = weak.update(cx, |ws, cx| {
                set(ws, i == 1);
                ws.organize.preview = None;
                cx.notify();
            });
        });
        widgets::segmented(id, &[(None, items[0]), (None, items[1])], index, 96., on)
    };
    let mut col = div().px(px(SP6)).pb(px(SP4)).flex().flex_col().gap(px(SP2));
    col = col
        .child(label(format!("{} samples from the last search. Only your own samples are organized; the factory bank stays where it is.", o.items.len()), TEXT_SM, pal.muted))
        .child(widgets::form_row("Layout", None, seg("org-layout", ["By category", "By key"], o.by_key as usize, cx, |ws, v| ws.organize.by_key = v)))
        .child(widgets::form_row("Mode", Some("Copy keeps the originals".into()), seg("org-mode", ["Copy", "Move"], o.move_files as usize, cx, |ws, v| ws.organize.move_files = v)))
        .child(widgets::form_row(
            "Destination",
            None,
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().flex_1().min_w(px(0.)).child(label(crate::paint::fit(&dest, 280., TEXT_SM), TEXT_SM, pal.muted)))
                .child(text_button("org-dest", "Choose…", Some(Icon::FolderOpen), false).on_click(cx.listener(|_, _, _, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Organize into".into()) });
                    cx.spawn(async move |this, cx| {
                        if let Ok(Ok(Some(paths))) = rx.await
                            && let Some(p) = paths.into_iter().next()
                        {
                            let _ = this.update(cx, |ws, cx| {
                                ws.organize.dest = Some(p);
                                ws.organize.preview = None;
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }))),
        ));
    if let Some(pv) = &o.preview {
        let mut list = div().flex().flex_col().gap(px(2.)).p(px(SP2)).rounded(px(r_md())).bg(pal.well);
        for (folder, n) in &pv.folders {
            list = list.child(div().flex().gap(px(SP2)).child(icon(Icon::Folder, 12., pal.faint)).child(label(folder.clone(), TEXT_SM, pal.text).flex_1()).child(mono(n.to_string(), TEXT_XS, pal.faint)));
        }
        col = col
            .child(section_title("PREVIEW"))
            .child(label(
                format!(
                    "{} {} files ({}) into {}{}",
                    if pv.moves { "Move" } else { "Copy" },
                    pv.files,
                    human_bytes(pv.bytes),
                    pv.dest.display(),
                    if pv.duplicates > 0 { format!(" · {} duplicates skipped", pv.duplicates) } else { String::new() }
                ),
                TEXT_SM,
                pal.text,
            ))
            .child(list);
    }
    if let Some(r) = &o.result {
        let (text, color) = match r {
            Ok(m) => (m.clone(), pal.play),
            Err(e) => (e.clone(), pal.record),
        };
        col = col.child(label(text, TEXT_SM, color));
    }
    let can_plan = o.dest.is_some() && !o.running;
    let footer = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .when(o.running, |d| d.child(icon(Icon::Loader, 14., pal.accent)).child(label("Organizing…", TEXT_SM, pal.muted)))
        .child(div().flex_1())
        .when(ws.b.library.can_undo_organize() && !o.running, |d| {
            d.child(text_button("org-undo", "Undo last run", Some(Icon::Undo), false).on_click(cx.listener(|this, _, _, cx| {
                this.organize.result = Some(this.b.library.undo_organize());
                cx.notify();
            })))
        })
        .child(text_button("org-plan", "Preview", None, false).when(!can_plan, |d| d.opacity(0.5)).on_click(cx.listener(|this, _, _, cx| {
            let Some(dest) = this.organize.dest.clone() else { return };
            let (items, by_key, mv) = (this.organize.items.clone(), this.organize.by_key, this.organize.move_files);
            match this.b.library.plan_organize(&items, &dest, by_key, mv) {
                Ok(p) => {
                    this.organize.preview = Some(p);
                    this.organize.result = None;
                }
                Err(e) => this.organize.result = Some(Err(e)),
            }
            cx.notify();
        })))
        .child(text_button("org-run", "Organize", Some(Icon::Check), true).when(o.preview.is_none() || o.running, |d| d.opacity(0.5)).on_click(cx.listener(|this, _, _, cx| {
            if this.organize.preview.is_none() || this.organize.running {
                return;
            }
            match this.b.library.execute_organize() {
                Ok(()) => this.organize.running = true,
                Err(e) => this.organize.result = Some(Err(e)),
            }
            cx.notify();
        })));
    div()
        .flex()
        .flex_col()
        .child(div().id("organize-body").max_h(px(520.)).overflow_y_scroll().child(col))
        .child(div().flex_none().px(px(SP6)).py(px(SP3)).border_t_1().border_color(border()).child(footer))
        .into_any_element()
}
