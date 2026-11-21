//! The MIDI generator, without a language model.
//!
//! ```text
//! GenRequest + GenContext ──► templates::spec_for ──► MidiSpec ─┐
//!                      (later: a model fills the same MidiSpec) ─┤
//!                                                                ▼
//!                                          realize ──► validate ──► Generated
//! ```
//!
//! A `MidiSpec` is the musical intent (chords, step patterns with accent
//! classes, a motif); `realize` turns it into notes (voicing, swing,
//! micro-timing, velocities by accent class) and `validate` repairs what a
//! good part must not do (out of key, out of register, overlapping, on top
//! of the kick, past the end of the clip). Templates and a model both only
//! produce the spec, so what plays with or without a connection passes
//! through the same two functions.
//!
//! Everything here is a pure function of (spec, context, seed, version): no
//! I/O, no clock, no globals. It never edits a `Project`; [`part`] turns a
//! [`Generated`] into commands and only reserves ids on the project it gets.

pub mod acid;
pub mod bloom;
pub mod context;
pub mod draft;
pub mod feel;
pub mod grid;
pub mod harmony;
pub mod lock;
pub mod pack;
pub mod part;
pub mod patterns;
pub mod realize;
pub mod request;
pub mod rng;
pub mod spec;
pub mod tables;
pub mod templates;
pub mod validate;

#[cfg(test)]
mod quality;

use aitrack_model::{Key, Scale, ScaleKind, Tick};

pub use context::{GenContext, PadMap, Target, TargetKind, digest};
pub use draft::{Draft, DraftNote, Report};
pub use grid::Grid;
pub use spec::{GENERATOR_VERSION, GenRequest, Genre, Kind, MidiSpec};

/// What `realize` and `validate` need besides the spec and the context.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub seed: u64,
    /// Micro-timing on (swing is part of the style and stays).
    pub humanize: bool,
    /// The key the notes must fit.
    pub scale: Scale,
    /// 0 sparse .. 1 busy; only the templates and rolling bass read it.
    pub density: f32,
}

/// The key a request plays in: its own, else the project's, else A minor.
pub fn effective_scale(req: &GenRequest, ctx: &GenContext) -> Scale {
    req.key.or(ctx.scale).unwrap_or(Scale::new(Key(9), ScaleKind::Minor))
}

/// Where the spec came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Template,
    Model,
}

/// A finished part, ready for the app to turn into commands.
#[derive(Clone, Debug)]
pub struct Generated {
    pub spec: MidiSpec,
    pub draft: Draft,
    pub report: Report,
    pub params: Params,
    pub source: Source,
    pub bars: u32,
    /// Length of the clip in ticks.
    pub clip_ticks: Tick,
    /// "Bass · tech house · A minor · 2 bars · rolling".
    pub interpretation: String,
    /// Why the rules wrote this when a model's spec was expected.
    pub fallback: Option<String>,
}

impl Generated {
    /// The key to stamp on the clip when the project has none.
    pub fn scale(&self) -> Scale {
        self.params.scale
    }
}

/// A part from a request, by templates only.
pub fn generate(req: &GenRequest, ctx: &GenContext, seed: u64) -> Generated {
    let params = Params { seed, humanize: req.humanize, scale: effective_scale(req, ctx), density: req.density.clamp(0., 1.) };
    let spec = templates::spec_for(req, ctx, &params);
    finish(req, ctx, spec, params, Source::Template)
}

/// A part from a spec some other source wrote (a language model). The spec
/// is trimmed first; an empty one falls back to a template.
pub fn generate_from_spec(req: &GenRequest, ctx: &GenContext, spec: MidiSpec, seed: u64) -> Generated {
    let spec = spec.clamped();
    if !spec.has_content() {
        let mut g = generate(req, ctx, seed);
        g.fallback = Some("The model's reply had nothing usable in it".to_owned());
        return g;
    }
    let params = Params { seed, humanize: req.humanize, scale: effective_scale(req, ctx), density: req.density.clamp(0., 1.) };
    finish(req, ctx, spec, params, Source::Model)
}

pub(crate) fn finish(req: &GenRequest, ctx: &GenContext, mut spec: MidiSpec, params: Params, source: Source) -> Generated {
    if !req.only.is_empty() && spec.layers.iter().any(|l| req.only.contains(&l.role)) {
        spec.layers.retain(|l| req.only.contains(&l.role));
    }
    let mut draft = realize::realize(&spec, ctx, &params);
    let mut report = validate::validate(&mut draft, &spec, ctx, &params);
    // A spec that needed many repairs was a bad one: try the templates once.
    if source == Source::Model && (draft.notes.is_empty() || report.total_fixes() as f32 > tables::MAX_FIX_RATE * draft.notes.len().max(4) as f32) {
        let alt = Params { seed: rng::splitmix64(params.seed), ..params };
        let tspec = templates::spec_for(req, ctx, &alt);
        let mut d = realize::realize(&tspec, ctx, &alt);
        let r = validate::validate(&mut d, &tspec, ctx, &alt);
        if !d.notes.is_empty() {
            let why = if draft.notes.is_empty() { "The model's spec played no notes".to_owned() } else { format!("The model's spec needed {} repairs, more than a good one should", report.total_fixes()) };
            return Generated { interpretation: interpretation(req, &tspec, &alt), bars: tspec.bars, clip_ticks: d.grid.clip_ticks(), spec: tspec, draft: d, report: r, params: alt, source: Source::Template, fallback: Some(why) };
        }
    }
    let bars = spec.bars.max(1);
    report.stats.swing = tables::swing(spec.genre, req.feel.unwrap_or(spec.feel));
    Generated { interpretation: interpretation(req, &spec, &params), bars, clip_ticks: draft.grid.clip_ticks(), spec, draft, report, params, source, fallback: None }
}

fn interpretation(req: &GenRequest, spec: &MidiSpec, params: &Params) -> String {
    let mut parts = vec![req.kind.label().to_owned(), spec.genre.label().to_owned(), params.scale.display(), format!("{} bar{}", spec.bars, if spec.bars == 1 { "" } else { "s" })];
    if let Some(p) = spec.layers.iter().find(|l| l.role.is_mono() || l.role == spec::Role::Chord).map(|l| l.pattern).filter(|p| *p != spec::Pattern::Custom) {
        parts.push(format!("{p:?}").to_lowercase());
    }
    parts.join(" · ")
}
