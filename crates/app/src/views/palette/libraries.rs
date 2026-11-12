//! The user's own libraries in the palette.
//!
//! - **Add library…** sits on top of the All, Samples and Packs tabs. It
//!   opens a picker: "Local folder…" (the folder dialog) and every cloud
//!   provider — iCloud Drive, Dropbox, OneDrive, Google Drive — with the
//!   folders of it found on this computer as one-click choices, greyed when
//!   the provider is not installed.
//! - **Your libraries** (Packs tab) lists the folders added, with their
//!   sample count, how many are online-only, the scan's progress, and
//!   Rescan / Remove.
//!
//! Adding goes through the library bridge (it scans in the background);
//! counts and removal use their own connection on a worker thread, so the
//! UI never waits on SQLite or on a cloud folder.

use super::cloud::{CloudRow, covered};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_library::cloud::{CloudFolder, Provider, provider_of};
use aitrack_library::{Library, RootId, RootKind, RootStats};
use crossbeam_channel::{Receiver, unbounded};
use gpui::{AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How often the counts are re-read while a scan runs.
const LIVE_COUNTS: Duration = Duration::from_millis(1500);

/// One added folder.
#[derive(Clone, Debug, PartialEq)]
pub struct RootRow {
    pub id: RootId,
    pub name: String,
    pub path: PathBuf,
    pub provider: Option<Provider>,
    pub samples: u64,
    /// Of those, online-only.
    pub cloud: u64,
}

/// The user's folders (factory roots are the sound bank, not "yours").
pub fn rows_from(stats: Vec<RootStats>) -> Vec<RootRow> {
    stats
        .into_iter()
        .filter(|s| s.root.kind == RootKind::User)
        .map(|s| {
            let provider = provider_of(&s.root.path);
            let leaf = s.root.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.root.path.to_string_lossy().into_owned());
            // "OneDrive-Personal", "GoogleDrive-me@x/My Drive": the provider says it better.
            let in_drive = s.root.path.parent().and_then(|p| p.file_name()).is_some_and(|n| n.to_string_lossy().starts_with("GoogleDrive-"));
            let generic = leaf.starts_with("OneDrive-") || leaf.starts_with("GoogleDrive-") || leaf == "com~apple~CloudDocs" || in_drive;
            let name = match provider {
                Some(p) if generic => p.name().to_string(),
                _ => leaf,
            };
            RootRow { id: s.root.id, name, path: s.root.path, provider, samples: s.samples, cloud: s.cloud }
        })
        .collect()
}

fn dev_roots() -> Vec<RootRow> {
    let row = |id, name: &str, path: &str, provider, samples, cloud| RootRow { id: RootId(id), name: name.into(), path: PathBuf::from(path), provider, samples, cloud };
    vec![
        row(1, "Splice", "~/Splice/sounds", None, 1_284, 0),
        row(2, "Dropbox", "~/Library/CloudStorage/Dropbox", Some(Provider::Dropbox), 642, 518),
    ]
}

#[derive(Default)]
pub struct Libraries {
    /// The "Add library…" picker is open.
    pub picker: bool,
    pub roots: Vec<RootRow>,
    rx: Option<Receiver<Vec<RootRow>>>,
    loaded: bool,
    last_load: Option<Instant>,
    scanning: bool,
}

impl Libraries {
    pub fn load(&mut self, user_db: Option<PathBuf>) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if super::cloud::dev_view() {
            self.roots = dev_roots();
            self.picker = true;
            return;
        }
        self.reload(user_db, None);
    }

    /// Re-reads the folders and their counts, removing `remove` first.
    pub fn reload(&mut self, user_db: Option<PathBuf>, remove: Option<RootId>) {
        let Some(db) = user_db else { return };
        self.last_load = Some(Instant::now());
        let (tx, rx) = unbounded();
        let run = move || {
            if !db.is_file() {
                let _ = tx.send(Vec::new());
                return;
            }
            let lib = match remove {
                Some(_) => Library::open(&db),
                None => Library::open_read_only(&db),
            };
            let rows = lib
                .and_then(|lib| {
                    if let Some(id) = remove {
                        lib.remove_root(id)?;
                    }
                    lib.root_stats()
                })
                .map(rows_from)
                .unwrap_or_else(|e| {
                    log::warn!("palette: libraries: {e}");
                    Vec::new()
                });
            let _ = tx.send(rows);
        };
        if std::thread::Builder::new().name("palette-libraries".into()).spawn(run).is_ok() {
            self.rx = Some(rx);
        }
    }

    /// New counts; and while a scan runs, a fresh read now and then.
    /// `true` when a scan just finished (the search should run again).
    pub fn poll(&mut self, scanning: bool, user_db: Option<PathBuf>) -> (bool, bool) {
        let mut changed = false;
        if let Some(rows) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.rx = None;
            self.roots = rows;
            changed = true;
        }
        let finished = self.scanning && !scanning;
        self.scanning = scanning;
        let stale = scanning && self.last_load.is_none_or(|t| t.elapsed() >= LIVE_COUNTS);
        if self.rx.is_none() && (finished || stale) {
            self.reload(user_db, None);
        }
        (changed, finished)
    }

    pub fn total(&self) -> (u64, u64) {
        self.roots.iter().fold((0, 0), |(a, b), r| (a + r.samples, b + r.cloud))
    }
}

// ───────────────────────────── Actions ─────────────────────────────

pub fn add_cloud(ws: &mut Workspace, folder: &CloudFolder, cx: &mut Context<Workspace>) {
    let path = folder.path.clone();
    ws.palette.cloud.mark_added(&path);
    ws.palette.libraries.picker = false;
    if super::cloud::dev_view() {
        cx.notify();
        return;
    }
    match ws.b.library.add_folder(&path) {
        Ok(()) => {
            ws.toast(format!("Adding {}: online-only files are listed by name and download when you play them", folder.label), cx);
            let db = ws.palette.user_db.clone();
            ws.palette.libraries.reload(db, None);
        }
        Err(e) => ws.toast(e, cx),
    }
    cx.notify();
}

pub fn add_local(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    ws.palette.libraries.picker = false;
    ws.pick_library_folder(cx);
    cx.notify();
}

fn remove(ws: &mut Workspace, row: &RootRow, cx: &mut Context<Workspace>) {
    let db = ws.palette.user_db.clone();
    if db.is_none() {
        ws.palette.libraries.roots.retain(|r| r.id != row.id);
    }
    ws.palette.libraries.reload(db, Some(row.id));
    ws.toast(format!("Removed {} from the library (the files stay where they are)", row.name), cx);
    super::refresh(ws);
    cx.notify();
}

// ───────────────────────────── Drawing ─────────────────────────────

/// A provider's (or a local folder's) badge: its initials on its colour.
fn badge(provider: Option<Provider>, dim: bool) -> AnyElement {
    let name = provider.map_or("", Provider::name);
    let el = super::artwork(provider.map(|_| name), None, 24.);
    if provider.is_none() {
        return div().size(px(24.)).flex_none().flex().items_center().justify_center().rounded(px(r_md())).bg(content(0.08)).child(icon(Icon::Folder, 14., p().muted)).into_any_element();
    }
    div().flex_none().when(dim, |d| d.opacity(0.4)).child(el).into_any_element()
}

/// Whether the "Add library…" bar shows on the current tab.
pub fn bar_shown(ws: &Workspace) -> bool {
    match ws.palette.tab {
        super::Tab::Packs | super::Tab::Samples => true,
        super::Tab::All => ws.palette.query.trim().is_empty(),
        _ => false,
    }
}

fn scan_line(ws: &Workspace) -> Option<Div> {
    let pal = p();
    let st = ws.b.library.scan_status();
    st.running.then(|| div().flex().items_center().gap(px(SP2)).child(label(crate::paint::fit(&st.message, 220., TEXT_XS), TEXT_XS, pal.muted)).child(widgets::hbar(st.fraction, 80., 3., pal.accent)))
}

/// "Add library…" and, when open, the picker under it.
pub fn add_bar(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !bar_shown(ws) {
        return None;
    }
    let pal = p();
    let lib = &ws.palette.libraries;
    let (samples, online) = lib.total();
    let summary = match lib.roots.len() {
        0 => "Index your own sample folders, on this computer or in the cloud".to_string(),
        n => {
            let online = if online > 0 { format!(" · {online} online only") } else { String::new() };
            format!("{n} {} · {samples} samples{online}", if n == 1 { "library" } else { "libraries" })
        }
    };
    let open = lib.picker;
    let bar = div()
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(
            widgets::text_button("lib-add-open", "Add library…", Some(Icon::Add), true)
                .tooltip(widgets::tip("A folder on this computer, or Google Drive, Dropbox, OneDrive or iCloud Drive", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.palette.libraries.picker = !this.palette.libraries.picker;
                    cx.notify();
                })),
        )
        .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label(summary, TEXT_XS, pal.faint)))
        .children(scan_line(ws))
        .child(icon(if open { Icon::ChevronUp } else { Icon::ChevronDown }, 12., pal.faint));
    Some(
        div()
            .px(px(SP3))
            .py(px(SP2))
            .mb(px(SP1))
            .flex()
            .flex_col()
            .gap(px(SP2))
            .border_b_1()
            .border_color(border())
            .child(bar)
            .when(open, |d| d.child(picker(ws, cx)))
            .into_any_element(),
    )
}

fn choice_shell(id: (&'static str, usize)) -> gpui::Stateful<Div> {
    div().id(id).h(px(36.)).px(px(SP2)).flex().items_center().gap(px(SP3)).rounded(px(r_md())).bg(content(0.04))
}

fn picker(ws: &Workspace, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let roots: Vec<PathBuf> = ws.palette.libraries.roots.iter().map(|r| r.path.clone()).collect();
    let mut list = div().flex().flex_col().gap(px(SP1)).child(
        choice_shell(("lib-choice-local", 0))
            .cursor_pointer()
            .hover(|s| s.bg(hover()))
            .child(badge(None, false))
            .child(div().flex_1().flex().flex_col().child(label("Local folder…", TEXT_MD, pal.text)).child(label("Any folder on this computer or an external drive", TEXT_XS, pal.faint)))
            .child(icon(Icon::FolderOpen, 14., pal.muted))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                add_local(this, cx);
            })),
    );
    let mut i = 0;
    for provider in Provider::ALL {
        let found: Vec<CloudRow> = ws.palette.cloud.rows.iter().filter(|r| r.folder.provider == provider).cloned().collect();
        if found.is_empty() {
            i += 1;
            list = list.child(
                choice_shell(("lib-choice-none", i))
                    .child(badge(Some(provider), true))
                    .child(div().flex_1().flex().flex_col().child(label(provider.name(), TEXT_MD, pal.faint)).child(label("Not found on this computer", TEXT_XS, pal.faint)))
                    .tooltip(widgets::tip_owned(format!("Install {} and sign in; its folder then shows up here", provider.name()))),
            );
            continue;
        }
        for row in found {
            i += 1;
            let added = row.added || covered(&row.folder.path, &roots);
            let path = row.folder.path.to_string_lossy().into_owned();
            let folder = row.folder.clone();
            list = list.child(
                choice_shell(("lib-choice", i))
                    .when(!added, |d| d.cursor_pointer().hover(|s| s.bg(hover())))
                    .child(badge(Some(provider), false))
                    .child(div().flex_1().min_w(px(0.)).flex().flex_col().overflow_hidden().child(label(row.folder.label.clone(), TEXT_MD, pal.text)).child(label(crate::paint::fit(&path, 480., TEXT_XS), TEXT_XS, pal.faint)))
                    .child(if added {
                        div().flex_none().flex().items_center().gap(px(SP1)).child(icon(Icon::Check, 12., pal.play)).child(label("Added", TEXT_SM, pal.play)).into_any_element()
                    } else {
                        div().flex_none().flex().items_center().gap(px(SP1)).child(icon(Icon::Add, 12., pal.accent)).child(label("Add", TEXT_SM, pal.accent)).into_any_element()
                    })
                    .tooltip(widgets::tip_owned(format!("{path}\nOnline-only files are indexed by name; nothing downloads until you play or drag a sample")))
                    .when(!added, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            add_cloud(this, &folder, cx);
                        }))
                    }),
            );
        }
    }
    list
}

/// "Your libraries" on the Packs tab.
pub fn list_section(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if ws.palette.tab != super::Tab::Packs || ws.palette.libraries.roots.is_empty() {
        return None;
    }
    let pal = p();
    let mut list = div().flex().flex_col().gap(px(SP1));
    for (i, row) in ws.palette.libraries.roots.iter().enumerate() {
        let path = row.path.to_string_lossy().into_owned();
        let r = row.clone();
        list = list.child(
            choice_shell(("lib-root", i))
                .child(badge(row.provider, false))
                .child(div().flex_1().min_w(px(0.)).flex().flex_col().overflow_hidden().child(label(row.name.clone(), TEXT_MD, pal.text)).child(label(crate::paint::fit(&path, 360., TEXT_XS), TEXT_XS, pal.faint)))
                .child(mono(format!("{} samples", row.samples), TEXT_XS, pal.muted))
                .when(row.cloud > 0, |d| {
                    d.child(
                        div()
                            .id(("lib-online", i))
                            .flex_none()
                            .h(px(20.))
                            .px(px(SP2))
                            .flex()
                            .items_center()
                            .gap(px(SP1))
                            .rounded(px(r_md()))
                            .bg(content(0.06))
                            .child(icon(super::CLOUD_ICON, 12., pal.muted))
                            .child(label(format!("{} online only", row.cloud), TEXT_XS, pal.muted))
                            .tooltip(widgets::tip("Listed by name (key and tempo from the file name). Each downloads when you play or drag it, and is analysed then.", None)),
                    )
                })
                .child(widgets::sized_icon_button(("lib-rescan", i), Icon::Refresh, false, 24., 14.).tooltip(widgets::tip("Rescan: pick up new and changed files (online-only files are not downloaded)", None)).on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.b.library.rescan();
                    cx.notify();
                })))
                .child(widgets::sized_icon_button(("lib-remove", i), Icon::Trash, false, 24., 14.).tooltip(widgets::tip("Remove from the library (the files stay where they are)", None)).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    remove(this, &r, cx);
                }))),
        );
    }
    Some(
        div()
            .px(px(SP3))
            .py(px(SP2))
            .mb(px(SP1))
            .flex()
            .flex_col()
            .gap(px(SP2))
            .border_b_1()
            .border_color(border())
            .child(div().flex().items_center().gap(px(SP2)).child(label("Your libraries", TEXT_SM, pal.muted)).child(label("Their samples show under “Yours” in All and Samples", TEXT_XS, pal.faint)).child(div().flex_1()).children(scan_line(ws)))
            .child(list)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_library::Root;

    fn stats(id: i64, path: &str, kind: RootKind, samples: u64, cloud: u64) -> RootStats {
        RootStats { root: Root { id: RootId(id), stored_path: path.into(), path: PathBuf::from(path), kind, watch: true, last_scan: None }, samples, cloud }
    }

    #[test]
    fn rows_name_cloud_roots_by_provider_and_skip_the_factory_bank() {
        let rows = rows_from(vec![
            stats(1, "content/soundbank", RootKind::Factory, 900, 0),
            stats(2, "/Users/a/Library/CloudStorage/OneDrive-Personal", RootKind::User, 10, 4),
            stats(3, "/Users/a/Library/CloudStorage/GoogleDrive-a@b.c/My Drive", RootKind::User, 5, 5),
            stats(4, "/Users/a/Library/CloudStorage/Dropbox/Drum Kits", RootKind::User, 3, 0),
            stats(5, "/Volumes/Samples/Vinyl", RootKind::User, 7, 0),
        ]);
        let got: Vec<(&str, Option<Provider>, u64)> = rows.iter().map(|r| (r.name.as_str(), r.provider, r.cloud)).collect();
        assert_eq!(got, vec![
            ("OneDrive", Some(Provider::OneDrive), 4),
            ("Google Drive", Some(Provider::GoogleDrive), 5),
            ("Drum Kits", Some(Provider::Dropbox), 0),
            ("Vinyl", None, 0),
        ]);
    }

    #[test]
    fn scan_end_reloads_once() {
        let mut l = Libraries::default();
        assert_eq!(l.poll(true, None), (false, false));
        assert_eq!(l.poll(true, None), (false, false));
        assert_eq!(l.poll(false, None), (false, true), "the scan just finished");
        assert_eq!(l.poll(false, None), (false, false));
    }

    #[test]
    fn totals_add_up() {
        let l = Libraries { roots: dev_roots(), ..Default::default() };
        assert_eq!(l.total(), (1_926, 518));
    }
}
