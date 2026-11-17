//! Realize: a `MidiSpec` becomes notes. Chords get voiced with voice
//! leading, the bass picks its pitches from the chord in force and leaves the
//! kick steps free, arpeggios walk the chord, melodies walk the scale in
//! small steps landing on chord tones on the beat, drums map to the pads.
//! Then the feel (swing, micro-timing, velocity by accent class) is applied.
//!
//! The output is a [`Draft`]: it still has to go through `validate`, which
//! guarantees the hard rules whatever the spec said.

use aitrack_model::Tick;

use super::draft::{Declared, Draft, DraftNote};
use super::feel::{Timing, accent_of, velocity};
use super::grid::{Grid, STEP};
use super::harmony::{ChordSym, Parent, VoiceStyle, resolve, voice_lead};
use super::patterns::{self, ACCENT_332};
use super::rng::{Rng, part_seed};
use super::spec::{Gate, Genre, Layer, MidiSpec, Pattern, Role};
use super::tables::{self, BASS_CENTRE, KICK_CLEARANCE, LEAD_CENTRE, SUB_CENTRE, TOP_VOICE_LIFT};
use super::{GenContext, Params};

/// A chord in force over a stretch of the clip.
#[derive(Clone, Debug)]
pub struct Seg {
    pub start: Tick,
    pub end: Tick,
    pub sym: ChordSym,
}

/// The chords of a spec on the timeline, in order.
pub fn timeline(spec: &MidiSpec, parent: Parent, grid: &Grid) -> Vec<Seg> {
    let beat = aitrack_model::PPQ as f32;
    let clip = grid.clip_ticks();
    let mut segs: Vec<Seg> = spec
        .chords
        .iter()
        .map(|c| {
            let start = (c.bar.min(grid.bars - 1) as Tick * grid.bar_ticks + (c.beat * beat).round() as Tick).clamp(0, clip - 1);
            let end = (start + (c.beats * beat).round() as Tick).clamp(start + 1, clip);
            Seg { start, end, sym: resolve(c, parent) }
        })
        .collect();
    segs.sort_by_key(|s| s.start);
    // Pitched layers with no chords (a model can leave them out) follow the tonic chord.
    if segs.is_empty() && spec.layers.iter().any(|l| !l.role.is_drum()) {
        let (quality, ext) = super::harmony::diatonic(parent, 0, super::harmony::Level::Seventh);
        let spec0 = super::spec::ChordSpec { quality, ext, ..Default::default() };
        segs.push(Seg { start: 0, end: clip, sym: resolve(&spec0, parent) });
    }
    segs
}

/// The chord in force at `t`: the latest that has started, else the first.
pub fn chord_at(segs: &[Seg], t: Tick) -> Option<&Seg> {
    segs.iter().rev().find(|s| s.start <= t).or(segs.first())
}

fn next_seg(segs: &[Seg], t: Tick) -> Option<&Seg> {
    segs.iter().find(|s| s.start > t)
}

/// The MIDI note with pitch class `pc` closest to `centre`.
fn nearest(pc: u8, centre: i32) -> i32 {
    let base = centre - centre.rem_euclid(12) + pc as i32;
    [base - 12, base, base + 12].into_iter().min_by_key(|n| (n - centre).abs()).unwrap_or(base)
}

/// A hit of a layer's step pattern.
#[derive(Clone, Copy, Debug)]
struct Hit {
    step: u32,
    accent: u8,
}

/// The hits of a layer over the clip. Patterns repeat every `len` steps
/// when `len` covers whole bars, else every bar.
fn hits(layer: &Layer, grid: &Grid) -> Vec<Hit> {
    let steps: Vec<char> = layer.steps.chars().collect();
    let accents: Vec<char> = layer.accents.chars().collect();
    let spb = grid.steps_per_bar as usize;
    if steps.is_empty() {
        return Vec::new();
    }
    (0..grid.total_steps())
        .filter_map(|g| {
            let idx = if steps.len().is_multiple_of(spb) { g as usize % steps.len() } else { (g as usize % spb) % steps.len() };
            let c = steps[idx];
            let own = accent_of(c)?;
            let accent = accents.get(idx).copied().and_then(accent_of).unwrap_or(own);
            Some(Hit { step: g, accent })
        })
        .collect()
}

fn declared(parent: Parent, sym: Option<&ChordSym>, key: i32) -> Declared {
    let pc = key.rem_euclid(12) as u8;
    if parent.contains_pc(pc) {
        Declared::No
    } else if sym.is_some_and(|s| s.pcs().contains(&pc)) {
        Declared::ChordTone
    } else {
        Declared::Passing
    }
}

struct Ctx<'a> {
    spec: &'a MidiSpec,
    ctx: &'a GenContext,
    p: &'a Params,
    grid: Grid,
    parent: Parent,
    timing: Timing,
    segs: Vec<Seg>,
    kick: Vec<bool>,
    out: Draft,
}

pub fn realize(spec: &MidiSpec, ctx: &GenContext, p: &Params) -> Draft {
    let grid = Grid::new(ctx.signature, spec.bars);
    let parent = Parent::of(p.scale);
    let mut r = Ctx {
        spec,
        ctx,
        p,
        grid,
        parent,
        timing: Timing { genre: spec.genre, feel: spec.feel, bpm: ctx.bpm, humanize: p.humanize },
        segs: timeline(spec, parent, &grid),
        kick: patterns::kick_flags(ctx, spec.genre, &grid),
        out: Draft::new(grid),
    };
    let voiced = r.voice_chords();
    for (li, layer) in spec.layers.iter().enumerate() {
        let li = li as u8;
        match layer.role {
            Role::Chord => r.chords(layer, li, &voiced),
            Role::Bass | Role::Sub => r.bass(layer, li),
            Role::Arp => r.arp(layer, li),
            Role::Lead => r.lead(layer, li),
            role if role.is_drum() => r.drums(layer, li),
            _ => {}
        }
    }
    r.drum_extras();
    r.out.sort();
    r.out
}

impl Ctx<'_> {
    fn rng(&self, role: Role, bar: u64, salt: u64) -> Rng {
        Rng::part(self.p.seed, role.id(), bar, salt)
    }

    /// Add a note. Its length was worked out from the grid position, so a
    /// late start (swing, micro-timing, strum) shortens it instead of
    /// pushing its end into the next hit.
    fn push(&mut self, mut n: DraftNote) {
        let late = (n.start - n.nominal).max(0);
        let min = tables::limits(n.role).min_len;
        n.length = (n.length - late).max(min.min(n.length));
        self.out.notes.push(n);
    }

    /// Ticks from `from` to the next hit start (or `fallback`), as the room a note has.
    fn gap(&self, hits: &[Hit], i: usize, fallback: Tick) -> Tick {
        hits.get(i + 1).map_or(fallback, |h| self.grid.tick(h.step)).min(fallback)
    }

    fn in_bar(&self, step: u32) -> u32 {
        self.grid.in_bar(step)
    }

    // ───────────── Chords ─────────────

    fn voice_chords(&self) -> Vec<Vec<u8>> {
        if !self.spec.layers.iter().any(|l| l.role == Role::Chord) {
            return Vec::new();
        }
        let row = tables::row(self.spec.genre);
        let syms: Vec<ChordSym> = self.segs.iter().map(|s| s.sym.clone()).collect();
        let style = VoiceStyle { voices: row.voices, compact: row.compact };
        voice_lead(&syms, style, part_seed(self.p.seed, Role::Chord.id(), 0, 7))
    }

    fn chords(&mut self, layer: &Layer, li: u8, voiced: &[Vec<u8>]) {
        let all = hits(layer, &self.grid);
        let mut rng = self.rng(Role::Chord, 0, 10);
        for (si, seg) in self.segs.clone().iter().enumerate() {
            let Some(voicing) = voiced.get(si) else { continue };
            let from = ((seg.start + STEP - 1) / STEP) as u32;
            let upto = ((seg.end + STEP - 1) / STEP) as u32;
            let mut mine: Vec<Hit> = all.iter().copied().filter(|h| (from..upto).contains(&h.step)).collect();
            if mine.is_empty() {
                mine.push(Hit { step: (seg.start / STEP) as u32, accent: 3 });
            }
            let strum = self.timing.strum(voicing.len());
            for (i, h) in mine.iter().enumerate() {
                let nominal = if i == 0 && layer.steps.is_empty() { seg.start } else { self.grid.tick(h.step) };
                let room = mine.get(i + 1).map_or(seg.end, |n| self.grid.tick(n.step)).min(seg.end) - nominal;
                let length = ((room as f32 * layer.gate.fraction()).round() as Tick).max(tables::limits(Role::Chord).min_len);
                let base = self.timing.start(nominal, self.in_bar(h.step), Role::Chord, &mut rng);
                let vel = velocity(Role::Chord, h.accent, &mut rng);
                for (vi, &key) in voicing.iter().enumerate() {
                    let top = vi + 1 == voicing.len();
                    let v = (vel * (1. + 0.03 * rng.signed()) * if top { TOP_VOICE_LIFT } else { 1. }).clamp(0.02, 1.);
                    let d = declared(self.parent, Some(&seg.sym), key as i32);
                    self.push(DraftNote { key, start: base + strum * vi as Tick, length, velocity: v, role: Role::Chord, layer: li, nominal, declared: d, slide: false });
                }
            }
        }
    }

    // ───────────── Bass ─────────────

    /// The kick start after `t` (ticks), if the bass should clear it.
    fn next_kick_after(&self, step: u32) -> Option<Tick> {
        (step + 1..self.grid.total_steps()).find(|s| self.kick[*s as usize]).map(|s| self.grid.tick(s))
    }

    fn bass(&mut self, layer: &Layer, li: u8) {
        let role = layer.role;
        let mut hs = hits(layer, &self.grid);
        if hs.is_empty() {
            // A named pattern with no steps: take its lane.
            let mut r = self.rng(role, 0, 3);
            let (steps, accents) = patterns::bass_lane(layer.pattern, &self.kick[..self.grid.steps_per_bar as usize], self.p.density, &mut r);
            let l = Layer { steps, accents, ..layer.clone() };
            hs = hits(&l, &self.grid);
        }
        let mut rng = self.rng(role, 0, 10);
        let centre = if role == Role::Sub { SUB_CENTRE } else { BASS_CENTRE } as i32 + 12 * layer.octave;
        let lim = tables::limits(role);
        let pitches: Vec<String> = if layer.pitches.is_empty() { vec!["r".into()] } else { layer.pitches.clone() };
        let policy = tables::row(self.spec.genre).kick_policy;
        let rolling = layer.pattern == Pattern::Rolling;
        let mut until_alt = 2 + rng.below(3);
        let mut approaches_left = (hs.len() as f32 * 0.08).floor() as usize;
        let total = self.grid.clip_ticks();
        for i in 0..hs.len() {
            let h = hs[i];
            let nominal = self.grid.tick(h.step);
            let Some(seg) = chord_at(&self.segs, nominal).cloned() else { continue };
            let next = next_seg(&self.segs, nominal).cloned();
            let mut token = pitches[i % pitches.len()].as_str();
            if rolling {
                until_alt -= 1;
                if until_alt == 0 {
                    until_alt = 2 + rng.below(3);
                    token = ["8", "5", "8", "b7"][rng.below(4)];
                }
            }
            let last_before_change = next.as_ref().is_some_and(|n| n.start - nominal <= 2 * STEP + 1) && h.accent <= 2 && hs.get(i + 1).is_none_or(|n| self.grid.tick(n.step) >= next.as_ref().map_or(Tick::MAX, |x| x.start));
            if last_before_change && approaches_left > 0 && rng.chance(0.5) {
                token = "a";
                approaches_left -= 1;
            }
            let root = nearest(seg.sym.root, centre);
            let mut key = match token {
                "3" => root + ((seg.sym.third_pc() + 12 - seg.sym.root) % 12) as i32,
                "5" => root + ((seg.sym.fifth_pc() + 12 - seg.sym.root) % 12) as i32,
                "7" => root + seg.sym.seventh_pc().map_or(0, |s| ((s + 12 - seg.sym.root) % 12) as i32),
                "b7" => root + 10,
                "8" => root + 12,
                "a" => match next.as_ref() {
                    Some(n) => {
                        let target = nearest(n.sym.root, root);
                        // A diatonic neighbour when there is one, else a semitone below.
                        let cands = [target - 1, target + 1, target - 2];
                        *cands.iter().find(|k| self.parent.contains_pc(k.rem_euclid(12) as u8) && (**k - target).abs() <= 2).unwrap_or(&cands[0])
                    }
                    None => root,
                },
                _ => root,
            };
            while key > lim.hi as i32 {
                key -= 12;
            }
            while key < lim.lo as i32 {
                key += 12;
            }
            let decl = declared(self.parent, Some(&seg.sym), key);
            let room = self.gap(&hs, i, total) - nominal;
            let room = if i + 1 == hs.len() { total - nominal } else { room };
            let gate = if role == Role::Sub { Gate::Long } else { layer.gate };
            let mut end = nominal + ((room as f32 * gate.fraction()).round() as Tick).max(lim.min_len);
            if policy == tables::KickPolicy::Avoid
                && let Some(k) = self.next_kick_after(h.step)
                && k - KICK_CLEARANCE < end
                && k - KICK_CLEARANCE - nominal >= lim.min_len
            {
                end = k - KICK_CLEARANCE;
            }
            let start = self.timing.start(nominal, self.in_bar(h.step), role, &mut rng);
            let vel = velocity(role, h.accent, &mut rng);
            self.push(DraftNote { key: key as u8, start, length: end - nominal, velocity: vel, role, layer: li, nominal, declared: decl, slide: false });
        }
    }

    // ───────────── Arp ─────────────

    fn arp(&mut self, layer: &Layer, li: u8) {
        let hs = hits(layer, &self.grid);
        let mut rng = self.rng(Role::Arp, 0, 10);
        let lim = tables::limits(Role::Arp);
        let octs = if self.spec.energy == super::spec::Energy::Low { 1 } else { 2 };
        let eighths = hs.iter().all(|h| h.step % 2 == 0);
        let spb = self.grid.steps_per_bar;
        let total = self.grid.clip_ticks();
        for (si, seg) in self.segs.clone().iter().enumerate() {
            // The lowest root inside the arp's register.
            let root = (lim.lo as i32..lim.lo as i32 + 12).find(|k| (*k).rem_euclid(12) as u8 == seg.sym.root).unwrap_or(57) + 12 * layer.octave;
            let mut tones = seg.sym.tones();
            tones.sort_by_key(|t| t % 12);
            tones.dedup_by_key(|t| *t % 12);
            let mut ladder: Vec<i32> = (0..octs).flat_map(|o| tones.iter().map(move |t| root + (*t % 12) as i32 + 12 * o)).collect();
            ladder.push(root + 12 * octs);
            ladder.retain(|k| *k >= lim.lo as i32 && *k <= lim.hi as i32);
            if ladder.is_empty() {
                continue;
            }
            let seq: Vec<i32> = match layer.pattern {
                Pattern::Down => ladder.iter().rev().copied().collect(),
                Pattern::UpDown => ladder.iter().copied().chain(ladder.iter().rev().skip(1).take(ladder.len().saturating_sub(2)).copied()).collect(),
                _ => ladder.clone(),
            };
            let from = ((seg.start + STEP - 1) / STEP) as u32;
            let upto = ((seg.end + STEP - 1) / STEP) as u32;
            let mine: Vec<(usize, Hit)> = hs.iter().copied().enumerate().filter(|(_, h)| (from..upto).contains(&h.step)).collect();
            // One hit of each odd bar to vary (never the first of the bar).
            let mut varied: std::collections::HashMap<u32, usize> = Default::default();
            for bar in (1..self.grid.bars).step_by(2) {
                let in_bar: Vec<usize> = mine.iter().enumerate().filter(|(_, (_, h))| self.grid.bar_of(self.grid.tick(h.step)) == bar).map(|(n, _)| n).collect();
                if in_bar.len() > 1 {
                    varied.insert(bar, in_bar[1 + self.rng(Role::Arp, bar as u64, 40).below(in_bar.len() - 1)]);
                }
            }
            let mut prev = -1;
            let mut srng = self.rng(Role::Arp, si as u64, 20);
            for (n, (gi, h)) in mine.iter().enumerate() {
                let mut key = if layer.pattern == Pattern::Random {
                    let mut k = ladder[srng.below(ladder.len())];
                    if k == prev && ladder.len() > 1 {
                        k = ladder[(ladder.iter().position(|x| *x == k).unwrap_or(0) + 1) % ladder.len()];
                    }
                    k
                } else {
                    seq[n % seq.len()]
                };
                prev = key;
                // In every second bar one note jumps an octave, so the loop is not a copy.
                let bar = self.grid.bar_of(self.grid.tick(h.step));
                if bar % 2 == 1 && Some(n) == varied.get(&bar).copied() {
                    key += if key + 12 <= lim.hi as i32 { 12 } else { -12 };
                }
                let in_bar = self.in_bar(h.step);
                let accent = if !layer.accents.is_empty() {
                    h.accent
                } else if eighths {
                    if in_bar.is_multiple_of(4) { 3 } else { 2 }
                } else if ACCENT_332.contains(&(in_bar as usize)) && spb == 16 {
                    3
                } else if in_bar.is_multiple_of(2) {
                    2
                } else {
                    1
                };
                let nominal = self.grid.tick(h.step);
                let room = mine.get(n + 1).map_or(seg.end, |(_, x)| self.grid.tick(x.step)).min(seg.end) - nominal;
                let length = ((room as f32 * layer.gate.fraction()).round() as Tick).max(lim.min_len);
                let start = self.timing.start(nominal, in_bar, Role::Arp, &mut rng);
                let vel = velocity(Role::Arp, accent, &mut rng);
                let decl = declared(self.parent, Some(&seg.sym), key);
                let _ = (gi, total);
                self.push(DraftNote { key: key.clamp(0, 127) as u8, start, length, velocity: vel, role: Role::Arp, layer: li, nominal, declared: decl, slide: false });
            }
        }
    }

    // ───────────── Lead ─────────────

    fn lead(&mut self, layer: &Layer, li: u8) {
        let hs = hits(layer, &self.grid);
        if hs.is_empty() {
            return;
        }
        let mut rng = self.rng(Role::Lead, 0, 10);
        let lim = tables::limits(Role::Lead);
        let parent = self.parent;
        let t0 = nearest(parent.root, LEAD_CENTRE as i32 - 7);
        let midi = |p: i32| t0 + parent.degree_iv(p);
        let (lo, hi) = (LEAD_CENTRE as i32 - 6, LEAD_CENTRE as i32 + 8);
        let motif: Vec<i32> = if layer.motif.is_empty() { vec![0, 1, 1, -1, -1, 0, 1, -1] } else { layer.motif.clone() };
        let in_chord = |p: i32, sym: &ChordSym| sym.pcs().contains(&(midi(p).rem_euclid(12) as u8));
        // The chord tone within two degrees of `p` that is closest to the previous note.
        let nearest_chord_pos = |p: i32, prev: i32, sym: &ChordSym| -> i32 {
            let mut cands: Vec<i32> = (p - 2..=p + 2).filter(|q| in_chord(*q, sym) && (lo..=hi).contains(&midi(*q))).collect();
            cands.sort_by_key(|q| ((midi(*q) - midi(prev)).abs(), (q - p).abs()));
            cands.first().copied().unwrap_or(p)
        };
        // Start on the chord tone of the first chord nearest the middle of the register.
        let first = chord_at(&self.segs, 0).map(|s| s.sym.clone());
        let mut pos = (-8..20).filter(|p| first.as_ref().is_none_or(|s| in_chord(*p, s))).min_by_key(|p| (midi(*p) - LEAD_CENTRE as i32).abs()).unwrap_or(7);
        let mut by_bar: Vec<Vec<usize>> = vec![Vec::new(); self.grid.bars as usize];
        for (i, h) in hs.iter().enumerate() {
            by_bar[self.grid.bar_of(self.grid.tick(h.step)) as usize].push(i);
        }
        let total = self.grid.clip_ticks();
        for (bar, idxs) in by_bar.iter().enumerate() {
            for (n, &i) in idxs.iter().enumerate() {
                let h = hs[i];
                let nominal = self.grid.tick(h.step);
                let Some(seg) = chord_at(&self.segs, nominal).cloned() else { continue };
                let last_of_clip = bar + 1 == self.grid.bars as usize && n + 1 == idxs.len();
                let mut mv = motif[n % motif.len()];
                // Bar 3 of a phrase turns the last move round; bar 4 and the end of the clip resolve.
                if bar % 4 == 2 && n + 1 == idxs.len() {
                    mv = -mv.signum() * mv.abs().max(1);
                }
                let prev = pos;
                if n > 0 || bar > 0 {
                    // A step wider than a whole tone (the augmented second of harmonic minor) is held instead.
                    pos += if (midi(pos + mv) - midi(pos)).abs() > 2 { 0 } else { mv };
                }
                while midi(pos) > hi {
                    pos -= 1;
                }
                while midi(pos) < lo {
                    pos += 1;
                }
                let in_bar = self.in_bar(h.step);
                let strong = in_bar.is_multiple_of(4) || h.accent >= 3;
                if (strong && !in_chord(pos, &seg.sym)) || last_of_clip || (bar % 4 == 3 && n + 1 == idxs.len()) {
                    pos = nearest_chord_pos(pos, prev, &seg.sym);
                }
                let key = midi(pos);
                let room = if i + 1 == hs.len() { total - nominal } else { self.grid.tick(hs[i + 1].step) - nominal };
                let length = ((room as f32 * layer.gate.fraction()).round() as Tick).max(lim.min_len);
                let start = self.timing.start(nominal, in_bar, Role::Lead, &mut rng);
                let vel = velocity(Role::Lead, h.accent, &mut rng);
                let decl = declared(self.parent, Some(&seg.sym), key);
                self.push(DraftNote { key: key.clamp(0, 127) as u8, start, length, velocity: vel, role: Role::Lead, layer: li, nominal, declared: decl, slide: false });
            }
        }
    }

    // ───────────── Drums ─────────────

    /// The pad a role plays; a missing clap or snare borrows the other.
    fn pad(&self, role: Role) -> Option<u8> {
        let pads = &self.ctx.target.pads;
        pads.key(role).or(match role {
            Role::Clap => pads.snare,
            Role::Snare => pads.clap,
            _ => None,
        })
    }

    fn drums(&mut self, layer: &Layer, li: u8) {
        let role = layer.role;
        let Some(key) = self.pad(role) else { return };
        let mut hs = hits(layer, &self.grid);
        if hs.is_empty() {
            // A role named with no steps: the genre's lane for it, if it has one.
            let mut r = self.rng(role, 0, 4);
            let base = patterns::drum_layers(self.spec.genre, self.p.density, self.spec.energy, &mut r);
            if let Some(l) = base.iter().find(|l| l.role == role) {
                hs = hits(l, &self.grid);
            }
        }
        let mut rng = self.rng(role, 0, 10);
        let min = tables::limits(role).min_len;
        let trap = matches!(self.spec.genre, Genre::Trap);
        let hat_steps = self.hat_group_steps();
        for h in hs {
            let nominal = self.grid.tick(h.step);
            let in_bar = self.in_bar(h.step);
            let length = match role {
                Role::OpenHat => {
                    let next = hat_steps.iter().copied().find(|s| *s > h.step).unwrap_or(h.step + 4);
                    (((next - h.step) as Tick * STEP * 9) / 10).clamp(STEP, 4 * STEP)
                }
                Role::Crash => 8 * STEP,
                Role::Kick | Role::Snare | Role::Clap => STEP,
                _ => min.max(STEP / 2),
            };
            // Trap rolls: a hat hit near the end of a bar splits into 1/32 notes with rising velocity.
            let roll = trap && role == Role::Hat && self.grid.is_four_four() && in_bar >= 10 && rng.chance(0.18);
            let start = self.timing.start(nominal, in_bar, role, &mut rng);
            let vel = velocity(role, h.accent, &mut rng);
            self.push(DraftNote { key, start, length: if roll { STEP / 2 } else { length }, velocity: vel, role, layer: li, nominal, declared: Declared::No, slide: false });
            if roll {
                for k in 1..=2i64 {
                    let at = nominal + k * (STEP / 2);
                    if at < nominal + STEP {
                        let v = velocity(role, (h.accent + k as u8).min(4), &mut rng);
                        self.push(DraftNote { key, start: at, length: STEP / 2, velocity: v, role, layer: li, nominal: at, declared: Declared::No, slide: false });
                    }
                }
            }
        }
    }

    fn hat_group_steps(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.spec.layers.iter().filter(|l| matches!(l.role, Role::Hat | Role::OpenHat)).flat_map(|l| hits(l, &self.grid)).map(|h| h.step).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// A fill on the last beat of every fourth bar and a crash on the downbeat after it.
    fn drum_extras(&mut self) {
        if !self.grid.is_four_four() || !self.spec.layers.iter().any(|l| l.role == Role::Kick) || self.spec.kind == super::spec::Kind::Perc {
            return;
        }
        let Some(fill_pad) = self.pad(Role::Snare).or(self.pad(Role::Clap)) else { return };
        let fill_role = if self.pad(Role::Snare).is_some() { Role::Snare } else { Role::Clap };
        let chance = 0.25 + 0.6 * self.spec.energy.level();
        let genre_fills = !matches!(self.spec.genre, Genre::Techno | Genre::Dubstep);
        for bar in (3..self.grid.bars).step_by(4) {
            let mut rng = self.rng(Role::Snare, bar as u64, 30);
            if !genre_fills || !rng.chance(chance) {
                continue;
            }
            for (s, a) in [(13u32, 2u8), (14, 3), (15, 3)] {
                if rng.chance(0.85) {
                    let nominal = self.grid.tick(bar * 16 + s);
                    let start = self.timing.start(nominal, s, fill_role, &mut rng);
                    let vel = velocity(fill_role, a, &mut rng);
                    self.push(DraftNote { key: fill_pad, start, length: STEP, velocity: vel, role: fill_role, layer: 0, nominal, declared: Declared::No, slide: false });
                }
            }
            if bar + 1 < self.grid.bars && let Some(crash) = self.pad(Role::Crash) && rng.chance(chance) {
                let nominal = self.grid.tick((bar + 1) * 16);
                let start = self.timing.start(nominal, 0, Role::Crash, &mut rng);
                let vel = velocity(Role::Crash, 4, &mut rng);
                self.push(DraftNote { key: crash, start, length: 8 * STEP, velocity: vel, role: Role::Crash, layer: 0, nominal, declared: Declared::No, slide: false });
            }
        }
    }
}
