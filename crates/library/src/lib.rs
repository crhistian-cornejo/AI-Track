//! Smart sample library and bundled sound bank.
//!
//! - [`Library`]: the SQLite index (samples, roots, packs, FTS5, embeddings).
//! - [`scan`]: incremental scanning on a background thread, with progress and cancel.
//! - [`watch`]: file watcher that keeps the index in sync.
//! - [`parse`]: key/BPM/category/genre hints from file and folder names (EN + ES).
//! - [`analyze`]: the [`Analyzer`] trait and the built-in [`BasicAnalyzer`].
//! - [`query`]: search with filters, key compatibility and [`Library::recommend`].
//! - [`tree`]: virtual folders (categories, keys, Camelot, BPM, genres, packs…).
//! - [`organize`]: optional copy/move into an organized folder layout, with undo.
//! - [`hub`]: the sound bank manifest (bundled packs + Content Hub links).

pub mod analyze;
pub mod db;
pub mod error;
pub mod hub;
pub mod organize;
pub mod parse;
pub mod query;
pub mod scan;
pub mod taxonomy;
pub mod tree;
pub mod watch;

#[cfg(test)]
pub(crate) mod test_util;

/// Re-exported so callers can run their own queries on [`Library::connection`].
pub use rusqlite;

pub use analyze::{Analysis, Analyzer, AudioInfo, BasicAnalyzer, Source};
pub use db::{Library, PackInfo, Preset, Root, RootId, RootKind, Sample, SampleId};
pub use error::{LibraryError, Result};
pub use hub::{BundledPack, HubEntry, Manifest};
pub use organize::{OrganizeJournal, OrganizeLayout, OrganizeMode, OrganizePlan};
pub use parse::{KeyHint, NameHints, parse_name, parse_path};
pub use query::{
    BpmFilter, Filters, KeyFilter, KeyMatch, RecommendQuery, Recommendation, SearchQuery, SortBy, key_compat,
};
pub use scan::{ScanEvent, ScanJob, ScanOptions, ScanProgress, ScanStats};
pub use taxonomy::{Category, SampleKind};
pub use tree::{TreeNode, VirtualFolder};
pub use watch::{LibraryWatcher, WatchEvent};
