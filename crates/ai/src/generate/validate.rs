//! Validate: the repairs that make any draft (from a template or from a
//! model's spec) obey the hard rules. The order matters, each step assumes
//! the one before. Every repair is counted in the [`Report`]; a result that
//! needed many is a bad spec and the caller tries another.

use aitrack_model::Tick;

use super::draft::{Declared, Draft, DraftNote, Report};
use super::grid::STEP;
use super::harmony::{ChordSym, Parent};
use super::patterns;
use super::realize::{chord_at, timeline};
use super::spec::{MidiSpec, Role};
use super::tables::{self, KickPolicy, MONO_GAP};
use super::{GenContext, Params};

/// Shortest note worth keeping when a clip end or a neighbour trims it.
const SCRAP: Tick = 30;

pub fn validate(d: &mut Draft, spec: &MidiSpec, ctx: &GenContext, p: &Params) -> Report {
    let mut r = Report::default();
    let parent = Parent::of(p.scale);
    let segs = timeline(spec, parent, &d.grid);
    let clip = d.grid.clip_ticks();

    basics(d, clip, &mut r);
    register(d, &mut r);
    scale(d, parent, &segs, &mut r);
    kick(d, spec, ctx, &mut r);
    mono(d, &mut r);
    polyphony(d, &mut r);
    pads(d, spec, ctx, &mut r);
    same_key(d, &mut r);
    d.sort();
    soft_checks(d, spec, &mut r);

    let keys = d.notes.iter().filter(|n| !n.role.is_drum());
    r.stats.notes = d.notes.len();
    r.stats.lowest = keys.clone().map(|n| n.key).min();
    r.stats.highest = keys.map(|n| n.key).max();
    r
}

/// 1 and 2: velocity and length in range, nothing outside the clip.
fn basics(d: &mut Draft, clip: Tick, r: &mut Report) {
    let (mut vel, mut len, mut out) = (0, 0, 0);
    d.notes.retain_mut(|n| {
        if !(0.02..=1.0).contains(&n.velocity) || n.velocity.is_nan() {
            n.velocity = if n.velocity.is_nan() { 0.5 } else { n.velocity.clamp(0.02, 1.) };
            vel += 1;
        }
        if n.start < 0 {
            n.start = 0;
            out += 1;
        }
        if n.start >= clip {
            out += 1;
            return false;
        }
        let min = tables::limits(n.role).min_len;
        if n.length < min {
            n.length = min;
            len += 1;
        }
        if n.start + n.length > clip {
            n.length = clip - n.start;
            out += 1;
            if n.length < SCRAP {
                return false;
            }
        }
        true
    });
    r.fix("velocity", vel);
    r.fix("length", len);
    r.fix("clip", out);
}

/// 3: pitched notes inside the role's register, folded by octaves.
fn register(d: &mut Draft, r: &mut Report) {
    let mut fixed = 0;
    d.notes.retain_mut(|n| {
        if n.role.is_drum() {
            return true;
        }
        let l = tables::limits(n.role);
        if (l.lo..=l.hi).contains(&n.key) {
            return true;
        }
        let mut k = n.key as i32;
        while k > l.hi as i32 {
            k -= 12;
        }
        while k < l.lo as i32 {
            k += 12;
        }
        fixed += 1;
        if (l.lo as i32..=l.hi as i32).contains(&k) {
            n.key = k as u8;
            true
        } else {
            false
        }
    });
    r.fix("register", fixed);
}

/// The note nearest to `key` that is a tone of the chord and in the scale,
/// else just in the scale. Ties go downward.
fn repaired(key: u8, sym: Option<&ChordSym>, parent: Parent) -> (u8, Declared) {
    let near = |ok: &dyn Fn(u8) -> bool| (0..=6i32).flat_map(|d| [-d, d]).map(|d| key as i32 + d).filter(|k| (0..=127).contains(k)).map(|k| k as u8).find(|k| ok(*k));
    let in_scale = |k: u8| parent.contains_pc(k % 12);
    if let Some(s) = sym {
        let pcs = s.pcs();
        if let Some(k) = near(&|k| in_scale(k) && pcs.contains(&(k % 12))) {
            return (k, Declared::No);
        }
    }
    (near(&in_scale).unwrap_or(key), Declared::No)
}

/// 4: every pitched note is in the scale or declared, and passing notes stay
/// a small part of their role.
fn scale(d: &mut Draft, parent: Parent, segs: &[super::realize::Seg], r: &mut Report) {
    let mut fixed = 0;
    for role in [Role::Bass, Role::Sub, Role::Chord, Role::Arp, Role::Lead] {
        let total = d.notes.iter().filter(|n| n.role == role).count();
        let mut allowed = (total as f32 * tables::MAX_PASSING).floor() as usize;
        for n in d.notes.iter_mut().filter(|n| n.role == role) {
            let outside = !parent.contains_pc(n.key % 12);
            let ok = match n.declared {
                Declared::No => !outside,
                Declared::ChordTone => true,
                Declared::Passing => {
                    if allowed > 0 {
                        allowed -= 1;
                        true
                    } else {
                        false
                    }
                }
            };
            if !ok {
                let sym = chord_at(segs, n.nominal).map(|s| &s.sym);
                let (k, decl) = repaired(n.key, sym, parent);
                n.key = k;
                n.declared = decl;
                fixed += 1;
            }
        }
    }
    r.fix("scale", fixed);
}

/// 5: the bass and the kick.
fn kick(d: &mut Draft, spec: &MidiSpec, ctx: &GenContext, r: &mut Report) {
    let policy = tables::row(spec.genre).kick_policy;
    if policy == KickPolicy::Free || !d.notes.iter().any(|n| matches!(n.role, Role::Bass | Role::Sub)) {
        return;
    }
    let flags = patterns::kick_flags(ctx, spec.genre, &d.grid);
    let on_kick = |nominal: Tick| flags.get((nominal / STEP) as usize).copied().unwrap_or(false);
    let bass = |n: &DraftNote| matches!(n.role, Role::Bass | Role::Sub);
    let mut fixed = 0;
    match policy {
        KickPolicy::Avoid => {
            let taken: std::collections::HashSet<Tick> = d.notes.iter().filter(|n| bass(n)).map(|n| n.nominal).collect();
            let mut taken = taken;
            let mut drop = Vec::new();
            for (i, n) in d.notes.iter_mut().enumerate() {
                if !bass(n) || !on_kick(n.nominal) {
                    continue;
                }
                fixed += 1;
                let free = (1..=3i64).map(|k| n.nominal + k * STEP).find(|t| (t / STEP) < flags.len() as i64 && !on_kick(*t) && !taken.contains(t));
                match free {
                    Some(t) => {
                        let delta = t - n.nominal;
                        taken.remove(&n.nominal);
                        taken.insert(t);
                        n.nominal = t;
                        n.start += delta;
                        if n.length - delta >= tables::limits(n.role).min_len {
                            n.length -= delta;
                        }
                    }
                    None => drop.push(i),
                }
            }
            let mut i = 0;
            d.notes.retain(|_| {
                i += 1;
                !drop.contains(&(i - 1))
            });
        }
        KickPolicy::Lock => loop {
            let bass_notes: Vec<usize> = (0..d.notes.len()).filter(|i| bass(&d.notes[*i])).collect();
            let together = bass_notes.iter().filter(|i| on_kick(d.notes[**i].nominal)).count();
            if bass_notes.is_empty() || together as f32 >= 0.8 * bass_notes.len() as f32 {
                break;
            }
            let weakest = bass_notes.iter().copied().filter(|i| !on_kick(d.notes[*i].nominal)).min_by(|a, b| d.notes[*a].velocity.total_cmp(&d.notes[*b].velocity));
            match weakest {
                Some(i) => {
                    d.notes.remove(i);
                    fixed += 1;
                }
                None => break,
            }
        },
        KickPolicy::Free => {}
    }
    r.fix("kick", fixed);
}

/// 6: one note at a time for the single-voice roles, with a small gap.
fn mono(d: &mut Draft, r: &mut Report) {
    let mut fixed = 0;
    for role in [Role::Bass, Role::Sub, Role::Lead, Role::Arp] {
        let mut mine: Vec<DraftNote> = Vec::new();
        let mut rest: Vec<DraftNote> = Vec::new();
        for n in d.notes.drain(..) {
            if n.role == role { mine.push(n) } else { rest.push(n) }
        }
        mine.sort_by(|a, b| a.start.cmp(&b.start).then(b.velocity.total_cmp(&a.velocity)));
        let min = tables::limits(role).min_len;
        let mut kept: Vec<DraftNote> = Vec::new();
        for n in mine {
            while let Some(last) = kept.last_mut() {
                if last.end() <= n.start - MONO_GAP {
                    break;
                }
                fixed += 1;
                let room = n.start - MONO_GAP - last.start;
                if room >= min {
                    last.length = room;
                    break;
                }
                kept.pop();
            }
            kept.push(n);
        }
        rest.extend(kept);
        d.notes = rest;
    }
    r.fix("mono", fixed);
}

/// 7: at most the role's polyphony at any instant. Older notes are cut
/// short first, then inner voices go; the bass line and the top voice stay.
fn polyphony(d: &mut Draft, r: &mut Report) {
    let cap = tables::limits(Role::Chord).poly;
    let mut fixed = 0;
    loop {
        let chords: Vec<usize> = (0..d.notes.len()).filter(|i| d.notes[*i].role == Role::Chord).collect();
        let Some(&at) = chords.iter().find(|i| {
            let t = d.notes[**i].start;
            chords.iter().filter(|j| d.notes[**j].start <= t && d.notes[**j].end() > t).count() > cap
        }) else {
            break;
        };
        let t = d.notes[at].start;
        let active: Vec<usize> = chords.iter().copied().filter(|j| d.notes[*j].start <= t && d.notes[*j].end() > t).collect();
        fixed += 1;
        // Cut the notes that began earlier.
        let older: Vec<usize> = active.iter().copied().filter(|j| d.notes[*j].start < t).collect();
        if !older.is_empty() {
            let min = tables::limits(Role::Chord).min_len;
            let mut gone = Vec::new();
            for j in older {
                let room = t - d.notes[j].start;
                if room >= min {
                    d.notes[j].length = room;
                } else {
                    gone.push(j);
                }
            }
            gone.sort_unstable_by(|a, b| b.cmp(a));
            for j in gone {
                d.notes.remove(j);
            }
            continue;
        }
        let (lo, hi) = (active.iter().map(|j| d.notes[*j].key).min(), active.iter().map(|j| d.notes[*j].key).max());
        let victim = active.iter().copied().filter(|j| Some(d.notes[*j].key) != lo && Some(d.notes[*j].key) != hi).min_by(|a, b| d.notes[*a].velocity.total_cmp(&d.notes[*b].velocity));
        d.notes.remove(victim.unwrap_or(at));
    }
    r.fix("polyphony", fixed);
}

/// 8: drum notes only on pads the target has.
fn pads(d: &mut Draft, spec: &MidiSpec, ctx: &GenContext, r: &mut Report) {
    let kit = ctx.target.pads;
    let keys = kit.keys();
    let before = d.notes.len();
    d.notes.retain(|n| !n.role.is_drum() || keys.contains(&n.key));
    r.fix("pad", before - d.notes.len());
    for role in Role::DRUMS {
        let wanted = spec.layers.iter().any(|l| l.role == role);
        let has = kit.key(role).is_some() || (role == Role::Clap && kit.snare.is_some()) || (role == Role::Snare && kit.clap.is_some());
        if wanted && !has {
            r.warn(format!("The target has no {} pad, so that layer is left out", role.label()));
        }
    }
}

/// 7 and 11: the same key never overlaps itself or repeats at the same start.
fn same_key(d: &mut Draft, r: &mut Report) {
    d.sort();
    let mut fixed = 0;
    let mut out: Vec<DraftNote> = Vec::with_capacity(d.notes.len());
    let mut last_of: std::collections::HashMap<u8, usize> = Default::default();
    for n in d.notes.drain(..) {
        if let Some(&i) = last_of.get(&n.key) {
            let prev = &mut out[i];
            if prev.end() > n.start || n.start - prev.start < tables::limits(n.role).min_len {
                fixed += 1;
                if n.start - prev.start < tables::limits(n.role).min_len.min(STEP / 2) {
                    continue; // the same hit twice: keep the first
                }
                let room = n.start - prev.start;
                if room >= tables::limits(prev.role).min_len {
                    prev.length = prev.length.min(room);
                } else {
                    continue;
                }
            }
        }
        last_of.insert(n.key, out.len());
        out.push(n);
    }
    d.notes = out;
    r.fix("duplicate", fixed);
}

/// Checks that only warn: density, flat dynamics, jumpy top voice.
fn soft_checks(d: &Draft, spec: &MidiSpec, r: &mut Report) {
    for role in [Role::Bass, Role::Sub, Role::Chord, Role::Arp, Role::Lead, Role::Kick, Role::Hat] {
        let n = d.notes.iter().filter(|n| n.role == role).count();
        if n == 0 {
            continue;
        }
        let (lo, hi) = tables::density_per_bar(role);
        let per_bar = n as f32 / d.grid.bars as f32;
        if per_bar < lo as f32 || per_bar > hi as f32 * if role == Role::Chord { 1. } else { 2. } {
            r.warn(format!("{} plays {per_bar:.0} notes per bar, outside the usual {lo}-{hi}", role.label()));
        }
    }
    let _ = spec;
}
