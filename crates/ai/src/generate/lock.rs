//! Locks and variations. A part can be varied (same spec, new seed) or
//! regenerated (new spec) while some of it is held: a layer (the kick, the
//! chords) or a bar. What is held keeps its notes exactly as they were; the
//! rest is written again and checked together with them.
//!
//! The held notes are copied from the previous result rather than derived
//! again, so nothing about the dice (a new seed, a different template) can
//! move them.

use super::draft::DraftNote;
use super::spec::{MidiSpec, Role};
use super::{GenContext, GenRequest, Generated, Grid, Params, Source, templates, validate};

/// What is held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Locks {
    pub roles: Vec<Role>,
    pub bars: Vec<u32>,
}

impl Locks {
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.bars.is_empty()
    }

    pub fn holds(&self, n: &DraftNote, grid: &Grid) -> bool {
        self.roles.contains(&n.role) || self.bars.contains(&n.bar(grid))
    }

    pub fn toggle_role(&mut self, role: Role) {
        match self.roles.iter().position(|r| *r == role) {
            Some(i) => {
                self.roles.remove(i);
            }
            None => self.roles.push(role),
        }
    }

    pub fn toggle_bar(&mut self, bar: u32) {
        match self.bars.iter().position(|b| *b == bar) {
            Some(i) => {
                self.bars.remove(i);
            }
            None => self.bars.push(bar),
        }
    }
}

/// `next` with the held notes of `prev` instead of its own, checked again.
fn merge(prev: &Generated, mut next: Generated, locks: &Locks, ctx: &GenContext) -> Generated {
    if locks.is_empty() {
        return next;
    }
    let grid = next.draft.grid;
    next.draft.notes.retain(|n| !locks.holds(n, &grid));
    next.draft.notes.extend(prev.draft.notes.iter().filter(|n| locks.holds(n, &prev.draft.grid)).cloned());
    next.draft.sort();
    next.report = validate::validate(&mut next.draft, &next.spec, ctx, &next.params);
    next.report.stats.swing = prev.report.stats.swing;
    next
}

/// The same spec with another seed: voicings, ghost notes, which hats fall.
pub fn vary(prev: &Generated, req: &GenRequest, ctx: &GenContext, locks: &Locks, seed: u64) -> Generated {
    let params = Params { seed, ..prev.params };
    let next = super::finish(req, ctx, prev.spec.clone(), params, prev.source);
    merge(prev, next, locks, ctx)
}

/// A new spec: a template picked by the new seed, or `spec` from a model.
/// With something held, the chords stay as they were so the new parts still
/// fit the held ones.
pub fn regenerate(prev: &Generated, req: &GenRequest, ctx: &GenContext, locks: &Locks, seed: u64, spec: Option<MidiSpec>) -> Generated {
    let params = Params { seed, ..prev.params };
    let (mut spec, source) = match spec.map(MidiSpec::clamped).filter(MidiSpec::has_content) {
        Some(s) => (s, Source::Model),
        None => (templates::spec_for(req, ctx, &params), Source::Template),
    };
    if !locks.is_empty() && !prev.spec.chords.is_empty() {
        spec.chords = prev.spec.chords.clone();
    }
    let next = super::finish(req, ctx, spec, params, source);
    merge(prev, next, locks, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{Genre, Kind, generate};
    use aitrack_model::{Key, Scale, ScaleKind};

    fn req(kind: Kind, genre: Genre, bars: u32) -> GenRequest {
        GenRequest { genre, bars, key: Some(Scale::new(Key(9), ScaleKind::Minor)), ..GenRequest::new(kind) }
    }

    /// What identifies a note for the locks: everything but a length a neighbour may trim.
    fn sig(g: &Generated, keep: impl Fn(&DraftNote) -> bool) -> Vec<(u8, i64, i64, i32)> {
        g.draft.notes.iter().filter(|n| keep(n)).map(|n| (n.key, n.start, n.nominal, (n.velocity * 1000.) as i32)).collect()
    }

    #[test]
    fn a_locked_layer_survives_vary_and_regenerate() {
        let ctx = GenContext::default();
        let locks = Locks { roles: vec![Role::Kick, Role::Clap], bars: vec![] };
        let (mut moved_hats, mut moved_fills) = (0, 0);
        for seed in 0..60u64 {
            let r = req(Kind::Drums, Genre::House, 4);
            let first = generate(&r, &ctx, seed);
            let held = |n: &DraftNote| matches!(n.role, Role::Kick | Role::Clap);
            let varied = vary(&first, &r, &ctx, &locks, seed + 1000);
            assert_eq!(sig(&first, held), sig(&varied, held), "vary, seed {seed}");
            let regen = regenerate(&first, &r, &ctx, &locks, seed + 2000, None);
            assert_eq!(sig(&first, held), sig(&regen, held), "regenerate, seed {seed}");
            let hats = |g: &Generated| sig(g, |n| n.role == Role::Hat);
            moved_hats += (hats(&first) != hats(&varied)) as usize;
            moved_fills += (sig(&first, |n| n.role == Role::Snare) != sig(&varied, |n| n.role == Role::Snare)) as usize;
        }
        assert!(moved_hats > 40, "what is not locked changes: {moved_hats} of 60");
        let _ = moved_fills;
    }

    #[test]
    fn a_locked_bar_survives_vary() {
        let ctx = GenContext::default();
        let r = req(Kind::Chords, Genre::DeepHouse, 4);
        let locks = Locks { roles: vec![], bars: vec![1] };
        let mut changed_elsewhere = 0;
        for seed in 0..60u64 {
            let first = generate(&r, &ctx, seed);
            let varied = vary(&first, &r, &ctx, &locks, seed + 500);
            let bar1 = |n: &DraftNote| n.bar(&first.draft.grid) == 1;
            assert_eq!(sig(&first, bar1), sig(&varied, bar1), "seed {seed}");
            assert!(!sig(&varied, bar1).is_empty());
            let others = |n: &DraftNote| n.bar(&first.draft.grid) != 1;
            changed_elsewhere += (sig(&first, others) != sig(&varied, others)) as usize;
        }
        assert!(changed_elsewhere > 0, "the other bars are written again");
    }

    #[test]
    fn vary_keeps_the_spec_and_regenerate_takes_a_new_one() {
        let ctx = GenContext::default();
        let r = req(Kind::Chords, Genre::House, 4);
        let first = generate(&r, &ctx, 3);
        let varied = vary(&first, &r, &ctx, &Locks::default(), 4);
        assert_eq!(varied.spec, first.spec);
        assert_eq!(varied.params.seed, 4);
        assert_ne!(sig(&first, |_| true), sig(&varied, |_| true), "another seed, another voicing or ornament");
        let different = (10..40u64).filter(|s| regenerate(&first, &r, &ctx, &Locks::default(), *s, None).spec != first.spec).count();
        assert!(different > 10, "regenerate writes another spec in most seeds: {different} of 30");
    }

    #[test]
    fn with_a_hold_regenerate_keeps_the_chords() {
        let ctx = GenContext::default();
        let r = req(Kind::Chords, Genre::DeepHouse, 4);
        let first = generate(&r, &ctx, 1);
        let locks = Locks { roles: vec![], bars: vec![0] };
        for seed in 10..40u64 {
            let g = regenerate(&first, &r, &ctx, &locks, seed, None);
            assert_eq!(g.spec.chords, first.spec.chords, "seed {seed}");
        }
    }

    #[test]
    fn a_model_spec_can_replace_the_old_one_around_the_holds() {
        let ctx = GenContext::default();
        let r = req(Kind::Bass, Genre::Techno, 2);
        let first = generate(&r, &ctx, 1);
        let spec = crate::generate::templates::spec_for(&r, &ctx, &Params { seed: 77, ..first.params });
        let g = regenerate(&first, &r, &ctx, &Locks { roles: vec![], bars: vec![0] }, 5, Some(spec));
        assert_eq!(g.source, Source::Model);
        let bar0 = |n: &DraftNote| n.bar(&g.draft.grid) == 0;
        assert_eq!(sig(&first, bar0), sig(&g, bar0));
    }

    #[test]
    fn holds_are_deterministic_and_toggle() {
        let ctx = GenContext::default();
        let r = req(Kind::Drums, Genre::Techno, 2);
        let first = generate(&r, &ctx, 9);
        let locks = Locks { roles: vec![Role::Kick], bars: vec![] };
        let a = vary(&first, &r, &ctx, &locks, 5);
        let b = vary(&first, &r, &ctx, &locks, 5);
        assert_eq!(format!("{:?}", a.draft.notes), format!("{:?}", b.draft.notes));
        let mut l = Locks::default();
        l.toggle_role(Role::Hat);
        l.toggle_bar(2);
        assert_eq!((l.roles.clone(), l.bars.clone(), l.is_empty()), (vec![Role::Hat], vec![2], false));
        l.toggle_role(Role::Hat);
        l.toggle_bar(2);
        assert!(l.is_empty());
    }

    #[test]
    fn everything_held_is_everything_kept() {
        let ctx = GenContext::default();
        let r = req(Kind::Bass, Genre::House, 2);
        let first = generate(&r, &ctx, 4);
        let all = Locks { roles: vec![], bars: vec![0, 1] };
        let g = vary(&first, &r, &ctx, &all, 99);
        assert_eq!(sig(&first, |_| true), sig(&g, |_| true));
    }
}
