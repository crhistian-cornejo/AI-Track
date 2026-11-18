//! Every Bloom knob in one table: its parameter, the group the window files
//! it under, and its one-line tooltip. The card shows the table's first
//! knobs in order (as many as fit), the window shows them all, grouped. A new
//! parameter is one more row here; a row whose id the device does not have
//! (yet) is skipped, so the table may run ahead of the engine.

use aitrack_stock::instrument::bloom::p;

/// Where a knob sits in the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// What every sound is made of.
    Sound,
    /// How the scene is grown and blended.
    Shape,
    /// The room and the output.
    Space,
    /// How notes play the sounds.
    Play,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Sound, Group::Shape, Group::Space, Group::Play];

    pub fn title(self) -> &'static str {
        match self {
            Group::Sound => "SOUND",
            Group::Shape => "SHAPE",
            Group::Space => "SPACE · OUTPUT",
            Group::Play => "PLAY",
        }
    }

    /// The group's tooltip.
    pub fn tip(self) -> &'static str {
        match self {
            Group::Sound => "Sound — shapes all sixteen at once",
            Group::Shape => "Shape — how far new scenes wander, and the morph",
            Group::Space => "Space — the shared room and the level",
            Group::Play => "Play — how notes reach the sounds",
        }
    }
}

/// One knob of the table.
#[derive(Clone, Copy, Debug)]
pub struct Knob {
    pub id: u32,
    pub group: Group,
    /// One short line: "Name — what it does".
    pub tip: &'static str,
}

const fn k(id: u32, group: Group, tip: &'static str) -> Knob {
    Knob { id, group, tip }
}

/// Every knob, in the card's order (the card shows a prefix of it).
///
/// To add parameters, append rows: `k(<id>, Group::…, "Name — what it does")`.
pub const KNOBS: &[Knob] = &[
    k(p::DECAY, Group::Sound, "Decay — shorter or longer tails"),
    k(p::BODY, Group::Sound, "Body — more resonant ring"),
    k(p::TONE, Group::Sound, "Tone — darker or brighter"),
    k(p::GRIT, Group::Sound, "Grit — drive and noise"),
    k(p::SPACE, Group::Space, "Space — shared room"),
    k(p::VOLUME, Group::Space, "Volume — output level"),
    k(p::TUNE, Group::Play, "Tune — transpose every sound"),
    k(p::VELOCITY, Group::Play, "Velocity — how much hits follow velocity"),
    k(p::DEVIANCE, Group::Shape, "Deviance — how far Roll/Mutate wander"),
    k(p::MORPH_X, Group::Shape, "Morph X — blend corners A↔B, C↔D"),
    k(p::MORPH_Y, Group::Shape, "Morph Y — blend corners A/B↔C/D"),
];

/// The table's row of parameter `id`.
pub fn find(id: u32) -> Option<&'static Knob> {
    KNOBS.iter().find(|k| k.id == id)
}

/// The tooltip of parameter `id`, if the table has it.
pub fn tip(id: u32) -> Option<&'static str> {
    find(id).map(|k| k.tip)
}

/// The rows of `group`, in table order, whose parameter `has` reports.
pub fn of(group: Group, has: impl Fn(u32) -> bool) -> impl Iterator<Item = &'static Knob> {
    KNOBS.iter().filter(move |k| k.group == group && has(k.id))
}

/// The first `n` rows whose parameter `has` reports (the card's knobs).
pub fn first(n: usize, has: impl Fn(u32) -> bool) -> impl Iterator<Item = &'static Knob> {
    KNOBS.iter().filter(move |k| has(k.id)).take(n)
}

/// How many knobs of `cell` px fit `rows` rows in `width` px with `gap` between.
pub fn fit(width: f32, cell: f32, gap: f32, rows: usize) -> usize {
    if width < cell {
        return 0;
    }
    let per_row = ((width + gap) / (cell + gap)).floor() as usize;
    per_row * rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_automatable_parameter_has_one_row() {
        let ids: Vec<u32> = KNOBS.iter().map(|k| k.id).collect();
        let unique: BTreeSet<u32> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "no parameter twice");
        // The scene's identity, the selection and the play switches are not knobs.
        let not_knobs = [p::SEED, p::SPECIES, p::SELECTED, p::MODE, p::BASE, p::CHOKE];
        for info in aitrack_stock::instrument::bloom::params() {
            if !not_knobs.contains(&info.id) {
                assert!(unique.contains(&info.id), "{} has a knob", info.name);
            }
        }
    }

    #[test]
    fn tooltips_are_one_short_line_named_after_the_knob() {
        let params = aitrack_stock::instrument::bloom::params();
        for k in KNOBS {
            assert!(!k.tip.contains('\n') && k.tip.chars().count() <= 44, "{}", k.tip);
            let (name, what) = k.tip.split_once(" — ").expect("Name — what");
            assert!(!what.is_empty());
            if let Some(info) = params.iter().find(|i| i.id == k.id) {
                assert_eq!(name, info.name, "the tip names the knob");
            }
        }
        for g in Group::ALL {
            assert!(g.tip().chars().count() <= 52);
        }
    }

    #[test]
    fn the_card_takes_a_prefix_and_skips_missing_ids() {
        let all: Vec<u32> = first(usize::MAX, |_| true).map(|k| k.id).collect();
        assert_eq!(all.len(), KNOBS.len());
        let four: Vec<u32> = first(4, |_| true).map(|k| k.id).collect();
        assert_eq!(four, vec![p::DECAY, p::BODY, p::TONE, p::GRIT]);
        let no_body: Vec<u32> = first(4, |id| id != p::BODY).map(|k| k.id).collect();
        assert_eq!(no_body, vec![p::DECAY, p::TONE, p::GRIT, p::SPACE]);
        assert!(of(Group::Shape, |_| true).all(|k| k.group == Group::Shape));
        assert_eq!(of(Group::Sound, |_| true).count(), 4);
    }

    #[test]
    fn knobs_fit_their_rows() {
        assert_eq!(fit(200., 40., 4., 2), 8);
        assert_eq!(fit(39., 40., 4., 2), 0);
        assert_eq!(fit(40., 40., 4., 1), 1);
        assert_eq!(fit(84., 40., 4., 1), 2);
    }
}
