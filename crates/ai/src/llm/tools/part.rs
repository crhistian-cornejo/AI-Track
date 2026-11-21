//! `generate_part`: the pattern engine writes a part into the draft like any
//! other tool. The musical work is in [`crate::generate`]; where the part goes
//! and the commands that place it are in [`crate::generate::part`].

use serde_json::Value;

use super::{Session, refs};
use crate::generate::part::{commands, prepare};
use crate::generate::rng::splitmix64;
use crate::generate::{generate, spec::Kind};

/// `generate_part`: a part of `kind` from a short description, on a track of
/// the draft (the named one, else the selected one, else a new one).
pub(super) fn generate_part(s: &mut Session, input: &Value) -> Result<String, String> {
    let kind_s = input.get("kind").and_then(Value::as_str).unwrap_or("bass").to_lowercase();
    let kind = match kind_s.as_str() {
        "beat" | "drums" => Kind::Drums,
        other => Kind::ALL.into_iter().find(|k| k.label().to_lowercase() == other).unwrap_or_default(),
    };
    let text = input.get("description").and_then(Value::as_str).unwrap_or_default();
    let track = match input.get("track").filter(|v| !v.is_null()) {
        Some(v) => Some(refs::track(&s.draft, v, s.selected)?),
        None => s.selected,
    };
    let prepared = prepare(&s.draft, track, None, Some(kind), text)?;
    let seed = prepared.req.seed.unwrap_or_else(seed);
    let g = generate(&prepared.req, &prepared.ctx, seed);
    if g.draft.notes.is_empty() {
        return Err("the pattern engine wrote no notes for that; try other words or write_clip".into());
    }
    let (cmds, lines) = commands(&mut s.draft, &g, prepared.dest, prepared.start);
    let label = format!("{} part, {} bars", prepared.req.kind.label(), prepared.req.bars);
    s.apply(&label, cmds)?;
    Ok(format!("{label}. {}", lines.join("; ")))
}

/// A fresh seed: the clock through the mixer, so two calls differ.
fn seed() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    splitmix64(now)
}
