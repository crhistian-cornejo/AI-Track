//! Smart sample library and bundled sound bank.
//!
//! - [`Library`]: the SQLite index (samples, roots, packs, FTS5, embeddings).
//! - [`scan`]: incremental scanning on a background thread, with progress and cancel.
//! - [`watch`]: file watcher that keeps the index in sync.
//! - [`cloud`]: Dropbox / OneDrive / Google Drive / iCloud folders, online-only
//!   files indexed by name and downloaded on demand.
//! - [`parse`]: key/BPM/category/genre hints from file and folder names (EN + ES).
//! - [`analyze`]: the [`Analyzer`] trait and the built-in [`BasicAnalyzer`].
//! - [`query`]: search with filters, key compatibility and [`Library::recommend`].
//! - [`tree`]: virtual folders (categories, keys, Camelot, BPM, genres, packs…).
//! - [`organize`]: optional copy/move into an organized folder layout, with undo.
//! - [`hub`]: the sound bank manifest (bundled packs + Content Hub links).
//! - [`install`]: one-click download of the optional packs, with progress.
//! - [`reanalyse`]: key/BPM from audio for samples that miss them, cached by file hash.
//! - [`similar`]: an embedding per sample (the [`Embedder`](similar::Embedder) trait) and "find similar sounds".
//! - [`semantic`]: search by meaning ("dark punchy kick") over the learned vectors, mixed with the name match.
//! - [`cover`]: sample pack artwork found in the pack's folders.
//! - [`text`]: folding and word scoring shared by every search box.
//! - [`labels`]: a static catalogue of popular sample labels by genre.
//! - `ai` (feature `ai`): [`AiDetector`](ai::AiDetector), the `aitrack-ai` detector.

#[cfg(feature = "ai")]
pub mod ai;
pub mod analyze;
pub mod cloud;
pub mod catalog;
pub mod cover;
pub mod db;
pub mod error;
pub mod fit;
pub mod hub;
pub mod install;
pub mod labels;
pub mod organize;
pub mod parse;
pub mod plugins;
pub mod query;
pub mod reanalyse;
pub mod scan;
pub mod semantic;
pub mod similar;
pub mod splice;
pub mod taxonomy;
pub mod text;
pub mod tree;
pub mod watch;

#[cfg(test)]
pub(crate) mod test_util;

/// Re-exported so callers can run their own queries on [`Library::connection`].
pub use rusqlite;

#[cfg(feature = "ai")]
pub use ai::{AiDetector, ClapEmbedder, DspEmbedder};
pub use analyze::{Analysis, Analyzer, AudioInfo, BasicAnalyzer, Source};
pub use cloud::{CloudFolder, HydrateEvent, HydrateJob, OsProbe, PlaceholderProbe, Provider};
pub use db::{Library, PackInfo, Preset, Root, RootId, RootKind, RootStats, Sample, SampleId};
pub use error::{LibraryError, Result};
pub use fit::{Fit, FitKind, TempoFit, fit_key, fit_tempo};
pub use catalog::{Catalog, Lane, PluginEntry};
pub use hub::{BundledPack, HubEntry, Manifest};
pub use labels::{Genre, LABELS, Label};
pub use organize::{OrganizeJournal, OrganizeLayout, OrganizeMode, OrganizePlan};
pub use parse::{KeyHint, NameHints, parse_name, parse_path};
pub use query::{
    BpmFilter, Filters, KeyFilter, KeyMatch, RecommendQuery, Recommendation, SearchQuery, SortBy, browse_folders, key_compat, relevance,
};
pub use reanalyse::{
    Detection, KeyBpmDetector, ReanalyseEvent, ReanalyseJob, ReanalyseOptions, ReanalyseProgress, ReanalyseSource, ReanalyseStats,
};
pub use scan::{ScanEvent, ScanJob, ScanOptions, ScanProgress, ScanStats};
pub use semantic::{SemanticHit, SemanticSearcher, hybrid_score};
pub use similar::{
    EmbedEvent, EmbedJob, EmbedOptions, EmbedProgress, EmbedStats, Embedder, Neighbour, SimilarEntry, SimilarFinder, SimilarHit, SimilarIndex, SimilarOptions, TextEmbedder,
};
pub use taxonomy::{Category, SampleKind};
pub use tree::{TreeNode, VirtualFolder};
pub use watch::{LibraryWatcher, WatchEvent};
