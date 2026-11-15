//! Pieces the stock MIDI effects of this family share: deterministic random
//! draws keyed by song position, the tempo-division table, scale masks, note
//! id allocation and a ledger that keeps the notes a device starts and ends
//! strictly paired. Nothing here allocates after construction.

use aitrack_dsp::{EventKind, EventSink, Transport};
use std::sync::atomic::{AtomicU32, Ordering};

// ───────────────────────────── Random draws ─────────────────────────────

/// SplitMix64 finaliser: a cheap, well-mixed 64-bit hash.
#[inline]
pub fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A short stream of random numbers derived from one hash.
#[derive(Clone, Copy, Debug)]
pub struct Draw(u64);

impl Draw {
    pub fn new(hash: u64) -> Self {
        Self(hash)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix64(self.0)
    }

    /// Uniform in `[0, 1)`.
    #[inline]
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }

    /// Uniform in `[-1, 1)`.
    #[inline]
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    /// Uniform integer in `lo..=hi` (`hi >= lo`).
    #[inline]
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        let span = (hi - lo + 1).max(1) as u64;
        lo + (self.next_u64() % span) as i32
    }
}

/// Turns "a note arrived here" into a hash that depends only on the seed and
/// the song position of the note, never on how the timeline was cut into
/// blocks nor on how long the device has been running. Two notes at the very
/// same position (a chord, or a stopped transport) are told apart by their
/// order of arrival.
#[derive(Clone, Debug)]
pub struct PositionHash {
    salt: u64,
    last: i64,
    count: u32,
}

impl PositionHash {
    pub fn new(salt: u64) -> Self {
        Self { salt: mix64(salt), last: i64::MIN, count: 0 }
    }

    pub fn reset(&mut self) {
        self.last = i64::MIN;
        self.count = 0;
    }

    /// The hash for a note at `offset` of a block. With `loop_beats > 0` the
    /// position is taken modulo that many beats, so a pattern of that length
    /// gets the same draws every time round.
    pub fn next(&mut self, seed: u32, t: &Transport, offset: u32, sample_rate: f64, loop_beats: f64) -> u64 {
        let pos = if loop_beats > 0.0 {
            let beats = t.position_beats + offset as f64 * t.bpm / 60.0 / sample_rate;
            let period = ((loop_beats * 960.0).round() as i64).max(1);
            ((beats * 960.0).round() as i64).rem_euclid(period)
        } else {
            t.position_samples + offset as i64
        };
        if pos == self.last {
            self.count = self.count.wrapping_add(1);
        } else {
            self.last = pos;
            self.count = 0;
        }
        mix64(mix64(self.salt ^ (seed as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)).wrapping_add(pos as u64) ^ ((self.count as u64) << 40))
    }
}

// ───────────────────────────── Tempo ─────────────────────────────

/// Tempo-synced lengths, shortest first. The last one is a bar.
pub const DIVISION_NAMES: &[&str] = &["1/32", "1/16T", "1/32.", "1/16", "1/8T", "1/16.", "1/8", "1/4T", "1/8.", "1/4", "1/2T", "1/4.", "1/2", "1 bar"];

const DIVISION_BEATS: [f64; 13] = [0.125, 1.0 / 6.0, 0.1875, 0.25, 1.0 / 3.0, 0.375, 0.5, 2.0 / 3.0, 0.75, 1.0, 4.0 / 3.0, 1.5, 2.0];

/// Length of division `index` in quarter notes (a bar follows the time signature).
pub fn division_beats(index: usize, time_sig: (u8, u8)) -> f64 {
    DIVISION_BEATS.get(index).copied().unwrap_or_else(|| bar_beats(time_sig))
}

pub fn bar_beats(time_sig: (u8, u8)) -> f64 {
    time_sig.0.max(1) as f64 * 4.0 / time_sig.1.max(1) as f64
}

/// Samples in one quarter note (the tempo is kept in a sane range).
pub fn samples_per_beat(bpm: f64, sample_rate: f64) -> f64 {
    let bpm = if bpm.is_finite() { bpm.clamp(10.0, 999.0) } else { 120.0 };
    60.0 / bpm * sample_rate
}

/// Pattern lengths for the seeded random devices.
pub const LOOP_NAMES: &[&str] = &["Off", "1 bar", "2 bars", "4 bars", "8 bars"];

/// Beats of loop length `index` (0 = off).
pub fn loop_beats(index: usize, time_sig: (u8, u8)) -> f64 {
    match index {
        0 => 0.0,
        n => (1u32 << (n - 1).min(3)) as f64 * bar_beats(time_sig),
    }
}

// ───────────────────────────── Keys ─────────────────────────────

/// Move `key` by whole octaves into `0..=127`.
pub fn fold_octaves(mut key: i32) -> u8 {
    if key < 0 {
        key += (-key + 11) / 12 * 12;
    } else if key > 127 {
        key -= (key - 127 + 11) / 12 * 12;
    }
    key.clamp(0, 127) as u8
}

// ───────────────────────────── Note ids ─────────────────────────────

/// Next unclaimed block of ids, shared by every device of the process.
static NEXT_BLOCK: AtomicU32 = AtomicU32::new(0);

const ID_FLOOR: i32 = 0x4000_0000;
/// Ids per block: a device takes a new block when it has used one up.
const BLOCK_IDS: u32 = 1 << 12;
/// Blocks before the range wraps (2^30 ids in all, a billion notes).
const BLOCKS: u32 = 1 << 18;

/// Ids for the notes a device invents: a high range that no other device
/// (nor the notes of clips, which sit below it) can hand out. Every device
/// claims blocks of ids from one shared counter, so two devices never
/// get the same id, however many there are in a project, until a billion
/// ids have been given out in total. Taking a block is one atomic add: no
/// allocation, no lock, safe on the audio thread.
#[derive(Clone, Debug)]
pub struct IdAlloc {
    block: u32,
    used: u32,
}

impl IdAlloc {
    pub fn new() -> Self {
        Self { block: Self::claim(), used: 0 }
    }

    fn claim() -> u32 {
        NEXT_BLOCK.fetch_add(1, Ordering::Relaxed) % BLOCKS
    }

    #[inline]
    pub fn next(&mut self) -> i32 {
        if self.used == BLOCK_IDS {
            self.block = Self::claim();
            self.used = 0;
        }
        let id = ID_FLOOR + (self.block * BLOCK_IDS + self.used) as i32;
        self.used += 1;
        id
    }
}

impl Default for IdAlloc {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────────────── Ledger ─────────────────────────────

const SILENT: i32 = i32::MIN;
const DEFER: usize = 32;

/// What a device has started and not yet ended, per channel × key. Writing
/// notes through it keeps the output strictly paired whatever the device
/// computes:
///
/// * a note started on a key that still sounds closes the old one first (so
///   there is never more than one note per key and the engine's own count of
///   open notes is exact);
/// * a note-off only ends the note with its id: if a retrigger already
///   replaced it, it is dropped instead of cutting the newer note;
/// * a note-off that falls on the sample its note began is held back to the
///   start of the next block (note-offs sort before note-ons at equal
///   offsets, so it could never come after its own note-on).
pub struct Ledger {
    id: Box<[i32]>,
    at: Box<[i64]>,
    /// Keys this device has written a note on since the last `forget`: a
    /// note-off for one of them that matches nothing was already answered by
    /// a retrigger and is dropped.
    seen: Box<[bool]>,
    deferred: [(u16, i32); DEFER],
    n_deferred: usize,
}

impl Ledger {
    pub fn new() -> Self {
        Self { id: vec![SILENT; 2048].into_boxed_slice(), at: vec![0; 2048].into_boxed_slice(), seen: vec![false; 2048].into_boxed_slice(), deferred: [(0, 0); DEFER], n_deferred: 0 }
    }

    #[inline]
    fn slot(channel: u8, key: u8) -> usize {
        (channel as usize & 15) * 128 + (key as usize & 127)
    }

    /// Forget everything (an all-notes-off went downstream, or a reset).
    pub fn forget(&mut self) {
        self.id.fill(SILENT);
        self.seen.fill(false);
        self.n_deferred = 0;
    }

    pub fn is_sounding(&self, channel: u8, key: u8) -> bool {
        self.id[Self::slot(channel, key)] != SILENT
    }

    /// Notes started and not ended.
    pub fn sounding(&self) -> usize {
        self.id.iter().filter(|i| **i != SILENT).count()
    }

    /// Call first in every block: sends the note-offs held back from the last.
    pub fn begin_block(&mut self, out: &mut EventSink<'_>) {
        for i in 0..self.n_deferred {
            let (slot, id) = self.deferred[i];
            let s = slot as usize;
            if self.id[s] == id && out.push(0, EventKind::NoteOff { channel: (s / 128) as u8, key: (s % 128) as u8, velocity: 0.0, note_id: id }) {
                self.id[s] = SILENT;
            }
        }
        self.n_deferred = 0;
    }

    /// Start a note at `abs` (the device's own sample clock). `false` when it
    /// was not written (the sink is full, or another note began on this key
    /// at this very sample).
    #[allow(clippy::too_many_arguments)]
    pub fn on(&mut self, out: &mut EventSink<'_>, offset: u32, abs: i64, channel: u8, key: u8, velocity: f32, note_id: i32) -> bool {
        let s = Self::slot(channel, key);
        if self.id[s] != SILENT {
            if self.at[s] == abs {
                return false;
            }
            if !out.push(offset, EventKind::NoteOff { channel, key, velocity: 0.0, note_id: self.id[s] }) {
                return false;
            }
            self.id[s] = SILENT;
        }
        if out.push(offset, EventKind::NoteOn { channel, key, velocity, note_id }) {
            self.id[s] = note_id;
            self.at[s] = abs;
            self.seen[s] = true;
            true
        } else {
            false
        }
    }

    /// End the note `note_id` on a key, for a note-off that arrives from
    /// upstream. A key this device never played gets it anyway (the note
    /// began before the device saw it).
    #[allow(clippy::too_many_arguments)]
    pub fn off(&mut self, out: &mut EventSink<'_>, offset: u32, abs: i64, channel: u8, key: u8, velocity: f32, note_id: i32) -> bool {
        self.end(out, offset, abs, channel, key, velocity, note_id, false)
    }

    /// End a note this device started itself: only if it still sounds.
    #[allow(clippy::too_many_arguments)]
    pub fn off_own(&mut self, out: &mut EventSink<'_>, offset: u32, abs: i64, channel: u8, key: u8, velocity: f32, note_id: i32) -> bool {
        self.end(out, offset, abs, channel, key, velocity, note_id, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn end(&mut self, out: &mut EventSink<'_>, offset: u32, abs: i64, channel: u8, key: u8, velocity: f32, note_id: i32, own: bool) -> bool {
        let s = Self::slot(channel, key);
        let cur = self.id[s];
        if cur != note_id && (cur != SILENT || own || self.seen[s]) {
            return false;
        }
        if cur == note_id && self.at[s] == abs && self.n_deferred < DEFER {
            self.deferred[self.n_deferred] = (s as u16, note_id);
            self.n_deferred += 1;
            return true;
        }
        if out.push(offset, EventKind::NoteOff { channel, key, velocity, note_id }) {
            self.id[s] = SILENT;
            true
        } else {
            false
        }
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

/// An event kind with no timing of its own that a MIDI effect should let
/// through untouched (everything but notes).
#[inline]
pub fn is_note(kind: &EventKind) -> bool {
    matches!(kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. } | EventKind::AllNotesOff)
}

/// A fixed-size set of `(channel, key)` flags.
pub struct KeySet(Box<[u64]>);

impl KeySet {
    pub fn new() -> Self {
        Self(vec![0; 32].into_boxed_slice())
    }

    #[inline]
    pub fn set(&mut self, channel: u8, key: u8, on: bool) {
        let s = (channel as usize & 15) * 128 + (key as usize & 127);
        if on {
            self.0[s >> 6] |= 1 << (s & 63);
        } else {
            self.0[s >> 6] &= !(1 << (s & 63));
        }
    }

    #[inline]
    pub fn get(&self, channel: u8, key: u8) -> bool {
        let s = (channel as usize & 15) * 128 + (key as usize & 127);
        self.0[s >> 6] >> (s & 63) & 1 == 1
    }

    pub fn clear(&mut self) {
        self.0.fill(0);
    }
}

impl Default for KeySet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_dsp::Event;

    fn sink_run(f: impl FnOnce(&mut EventSink<'_>)) -> Vec<(u32, EventKind)> {
        let mut v = Vec::with_capacity(64);
        let mut dropped = 0;
        f(&mut EventSink::new(&mut v, &mut dropped));
        v.into_iter().map(|e: Event| (e.offset, e.kind)).collect()
    }

    #[test]
    fn draws_are_uniform_enough_and_in_range() {
        let mut d = Draw::new(mix64(1));
        let mut sum = 0.0;
        for _ in 0..10_000 {
            let u = d.unit();
            assert!((0.0..1.0).contains(&u));
            sum += u;
            let r = d.range(-3, 4);
            assert!((-3..=4).contains(&r));
        }
        assert!((sum / 10_000.0 - 0.5).abs() < 0.02);
    }

    #[test]
    fn the_position_hash_depends_on_seed_and_position_only() {
        let t = |s: i64| Transport { position_samples: s, ..Transport::default() };
        let (mut a, mut b) = (PositionHash::new(1), PositionHash::new(1));
        // A different history before the note changes nothing.
        a.next(5, &t(100), 0, 48_000.0, 0.0);
        let x = a.next(5, &t(900), 12, 48_000.0, 0.0);
        let y = b.next(5, &t(900), 12, 48_000.0, 0.0);
        assert_eq!(x, y);
        // The block cut does not matter: 900 + 12 either way.
        let z = PositionHash::new(1).next(5, &t(912), 0, 48_000.0, 0.0);
        assert_eq!(x, z);
        assert_ne!(x, PositionHash::new(1).next(6, &t(912), 0, 48_000.0, 0.0));
        // The second note of a chord is told apart from the first.
        let mut c = PositionHash::new(1);
        let first = c.next(5, &t(912), 0, 48_000.0, 0.0);
        assert_ne!(first, c.next(5, &t(912), 0, 48_000.0, 0.0));
    }

    #[test]
    fn a_loop_wraps_the_position() {
        let t = |beats: f64| Transport { position_beats: beats, ..Transport::default() };
        let h = |beats: f64| PositionHash::new(9).next(1, &t(beats), 0, 48_000.0, 4.0);
        assert_eq!(h(1.5), h(5.5));
        assert_eq!(h(1.5), h(401.5));
        assert_ne!(h(1.5), h(2.5));
    }

    #[test]
    fn divisions_and_loops_follow_the_time_signature() {
        assert_eq!(division_beats(9, (4, 4)), 1.0);
        assert_eq!(division_beats(6, (4, 4)), 0.5);
        assert_eq!(division_beats(13, (4, 4)), 4.0);
        assert_eq!(division_beats(13, (3, 4)), 3.0);
        assert_eq!(division_beats(13, (6, 8)), 3.0);
        assert_eq!(loop_beats(0, (4, 4)), 0.0);
        assert_eq!(loop_beats(3, (4, 4)), 16.0);
        assert_eq!(DIVISION_NAMES.len(), 14);
        assert!(samples_per_beat(f64::NAN, 48_000.0) > 0.0);
    }

    #[test]
    fn folding_keeps_the_pitch_class() {
        for k in -300..=300 {
            let f = fold_octaves(k) as i32;
            assert!((0..=127).contains(&f));
            assert_eq!((f - k).rem_euclid(12), 0, "{k} -> {f}");
        }
    }

    #[test]
    fn the_ledger_never_stacks_two_notes_on_a_key() {
        let mut l = Ledger::new();
        let got = sink_run(|out| {
            assert!(l.on(out, 0, 0, 0, 60, 0.5, 1));
            // A second note on a sounding key closes the first.
            assert!(l.on(out, 10, 10, 0, 60, 0.5, 2));
            // The first one's own release arrives late: dropped.
            assert!(!l.off(out, 20, 20, 0, 60, 0.0, 1));
            assert!(l.off(out, 30, 30, 0, 60, 0.0, 2));
        });
        let kinds: Vec<_> = got.iter().map(|(o, k)| (*o, matches!(k, EventKind::NoteOn { .. }))).collect();
        assert_eq!(kinds, vec![(0, true), (10, false), (10, true), (30, false)]);
        assert!(!l.is_sounding(0, 60));
    }

    #[test]
    fn the_ledger_holds_back_a_release_that_would_sort_before_its_note() {
        let mut l = Ledger::new();
        let first = sink_run(|out| {
            l.on(out, 5, 105, 0, 60, 0.5, 1);
            // The release on the very sample of the note-on.
            assert!(l.off(out, 5, 105, 0, 60, 0.0, 1));
        });
        assert_eq!(first.len(), 1);
        assert!(l.is_sounding(0, 60));
        let next = sink_run(|out| l.begin_block(out));
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0, 0);
        assert!(!l.is_sounding(0, 60));
    }

    #[test]
    fn a_release_of_a_key_never_played_passes_but_one_for_an_own_note_that_ended_does_not() {
        let mut l = Ledger::new();
        let got = sink_run(|out| {
            assert!(l.off(out, 0, 0, 0, 40, 0.0, 7), "began before this device");
            assert!(!l.off_own(out, 0, 0, 0, 41, 0.0, 7));
            l.on(out, 1, 1, 0, 42, 0.5, 8);
            l.off(out, 2, 2, 0, 42, 0.0, 8);
            assert!(!l.off(out, 3, 3, 0, 42, 0.0, 8), "already ended");
        });
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn ids_never_repeat_across_many_devices_in_one_project() {
        // More instances than any fixed-width device number could tell apart.
        let mut allocs: Vec<IdAlloc> = (0..300).map(|_| IdAlloc::new()).collect();
        let mut seen = std::collections::HashSet::new();
        for round in 0..6_000 {
            for (i, a) in allocs.iter_mut().enumerate() {
                let id = a.next();
                assert!(id >= 0x4000_0000, "ids stay in the high range");
                assert!(seen.insert(id), "id {id:#x} repeated (instance {i}, round {round})");
            }
        }
    }

    #[test]
    fn layers_of_many_pitch_devices_never_share_a_note_id() {
        use super::super::fxtest::{Rig, off, on};
        let mut ids = std::collections::HashSet::new();
        for _ in 0..140 {
            let mut r = Rig::new(super::super::pitch::create());
            let set = |r: &mut Rig, id: u32, plain: f32| {
                let n = r.p.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
                r.p.set_param(id, n);
            };
            set(&mut r, super::super::pitch::p::LAYER, 1.0);
            set(&mut r, super::super::pitch::p::OCTAVE, 1.0);
            for i in 0..3 {
                r.run(&[(0, on(60 + i, 0.8)), (10, off(60 + i))], 64, 64);
            }
            for (_, k) in &r.log {
                if let EventKind::NoteOn { note_id, .. } = k
                    && *note_id >= 0x4000_0000
                {
                    assert!(ids.insert(*note_id), "note id {note_id:#x} given to two notes");
                }
            }
        }
        assert_eq!(ids.len(), 140 * 3);
    }

    #[test]
    fn ids_are_high_and_distinct_between_devices() {
        let (mut a, mut b) = (IdAlloc::new(), IdAlloc::new());
        let (x, y) = (a.next(), b.next());
        assert!(x >= 0x4000_0000 && y >= 0x4000_0000);
        assert_ne!(x, y);
        assert_ne!(a.next(), x);
    }

    #[test]
    fn key_sets_hold_every_channel() {
        let mut s = KeySet::new();
        s.set(15, 127, true);
        s.set(0, 0, true);
        assert!(s.get(15, 127) && s.get(0, 0) && !s.get(1, 0));
        s.clear();
        assert!(!s.get(15, 127));
    }
}
