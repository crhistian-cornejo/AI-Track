//! A part in progress: notes tagged with the role and layer that made them,
//! and the report `validate` leaves about what it repaired.

use aitrack_model::Tick;

use super::grid::Grid;
use super::spec::Role;

/// Why a note may be outside the scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Declared {
    /// In the scale.
    No,
    /// A tone of a chord the spec itself altered (the leading tone of a V).
    ChordTone,
    /// A chromatic passing or approach note, capped per role.
    Passing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DraftNote {
    pub key: u8,
    pub start: Tick,
    pub length: Tick,
    pub velocity: f32,
    pub role: Role,
    /// Index of the spec layer that made it.
    pub layer: u8,
    /// Where it sits on the grid before swing and micro-timing.
    pub nominal: Tick,
    pub declared: Declared,
    pub slide: bool,
}

impl DraftNote {
    pub fn end(&self) -> Tick {
        self.start + self.length
    }

    pub fn bar(&self, grid: &Grid) -> u32 {
        grid.bar_of(self.nominal)
    }
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub notes: Vec<DraftNote>,
    pub grid: Grid,
}

impl Draft {
    pub fn new(grid: Grid) -> Self {
        Self { notes: Vec::new(), grid }
    }

    pub fn sort(&mut self) {
        self.notes.sort_by(|a, b| a.start.cmp(&b.start).then(a.key.cmp(&b.key)).then((a.role as u8).cmp(&(b.role as u8))));
    }

    pub fn of_role(&self, role: Role) -> impl Iterator<Item = &DraftNote> {
        self.notes.iter().filter(move |n| n.role == role)
    }
}

/// One repair, counted by what was wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub what: &'static str,
    pub count: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub notes: usize,
    pub lowest: Option<u8>,
    pub highest: Option<u8>,
    /// Linn swing used, 0.5 straight.
    pub swing: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub fixes: Vec<Fix>,
    pub warnings: Vec<String>,
    pub stats: Stats,
}

impl Report {
    pub fn fix(&mut self, what: &'static str, count: usize) {
        if count == 0 {
            return;
        }
        match self.fixes.iter_mut().find(|f| f.what == what) {
            Some(f) => f.count += count,
            None => self.fixes.push(Fix { what, count }),
        }
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        let t = text.into();
        if !self.warnings.contains(&t) {
            self.warnings.push(t);
        }
    }

    pub fn total_fixes(&self) -> usize {
        self.fixes.iter().map(|f| f.count).sum()
    }

    pub fn count(&self, what: &str) -> usize {
        self.fixes.iter().find(|f| f.what == what).map_or(0, |f| f.count)
    }
}
