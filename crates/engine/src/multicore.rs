//! Processing the tracks of one segment on several threads.
//!
//! # Levels (compile time)
//! [`Schedule::build`] runs once per compiled graph, on the control thread.
//! Every track that carries audio gets a *level*: 0 when it depends on no
//! other track, otherwise one more than the highest level among the tracks
//! it depends on. A track depends on
//! * every track whose output it is (a group's members, a return's senders):
//!   its input is their sum;
//! * every track one of its devices sidechains from, when that track comes
//!   earlier in the graph order: it reads that track's signal of this block.
//!
//! A VCA adds no edge (VCAs are evaluated in a pre-pass before any track).
//! A sidechain that the graph order could not honour (the source comes
//! *after* the listener, because the routing closes a loop) hears the
//! previous block; it adds no dependency to the listener, but the source
//! must not overwrite that previous block while the listener still reads
//! it, so the source is put at least one level above the listener. Routing
//! itself is acyclic (the compiler cuts cycles), so every edge goes from an
//! earlier track to a later one and one pass in graph order settles the
//! levels. Tracks of one level depend on none of each other: they may run
//! at the same time. Everything here is allocated at compile time.
//!
//! # Two phases per track
//! *Compute* ([`crate::graph::compute_track`]): clips, live input, devices,
//! modulators, sends' buffers, fader, meters, PDC delay. It reads the
//! track's own input buffer and the sidechain slots of earlier levels and
//! writes only the track's own node (its buffer, its sends' buffers, its
//! own sidechain slot, its own tap). *Mix*: adding the outputs and sends
//! into the input buffers of their targets and into the output bus.
//!
//! On one thread ([`crate::graph::CompiledGraph::process_segment`] without a
//! parallel pool, or a graph with nothing worth spreading) the engine runs
//! compute then mix for each track in graph order: exactly the old single
//! path. With a pool, the mix of a track's input is *gathered* at the start
//! of its compute, by whichever thread runs it: the input buffer is cleared
//! and the feeding outputs and sends (all from earlier levels, all finished)
//! are added in the order [`Schedule::feeds`] lists them, which is the order
//! the single-threaded engine adds them in (sources in graph order, each one's
//! pre-fader sends, post-fader sends, then its output). The output bus is
//! gathered last, on the audio thread, in graph order too. Floating-point
//! sums are therefore made of the same terms in the same order: the output
//! is bit-identical whatever the thread count.
//!
//! # Running a level
//! The audio thread hands a level to [`WorkerPool::run`] (see
//! [`crate::workers`] for the claim counter, the countdown and the wake
//! protocol) and takes part itself; it moves to the next level once every
//! track of this one is done. A level with fewer than two tracks worth a
//! thread (devices or clips) runs inline, and a graph without any such level
//! never wakes the pool.
//!
//! # Why the raw pointers are sound
//! [`Slots`] and [`SideBus`] hand out `&mut` to elements of a slice from
//! several threads at once. That is sound because, within one level:
//! * each track index appears once, so each `&mut TrackNode` and each
//!   `&mut` input buffer is created by exactly one thread;
//! * a sidechain slot is written only by its source track and read only by
//!   tracks of other levels (above, for the current block; below, for the
//!   previous one), never by a track of the same level;
//! * the tracks whose buffers a gather reads are of earlier levels: nobody
//!   holds a `&mut` to them any more.
//!
//! Between levels, everything a level wrote happens-before everything the
//! next one reads: the workers' release decrements of the countdown, the
//! audio thread's acquire spin on it, its release publication of the next
//! job and the workers' acquire claim of it.

use crate::graph::{compute_track, for_each_proc, OutputTarget, TrackEnv, TrackNode};
use crate::workers::{WorkerPool, MAX_JOBS};
use aitrack_dsp::AudioBuffer;
use std::collections::HashMap;
use std::marker::PhantomData;

/// One term of an input sum: a track's output (`send: None`) or one of
/// its sends (index in `TrackNode::sends`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Feed {
    pub track: u32,
    pub send: Option<u16>,
}

impl Feed {
    fn buf(self, t: &TrackNode) -> &AudioBuffer {
        match self.send {
            None => &t.buf,
            Some(k) => &t.sends[k as usize].buf,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Level {
    /// Graph indices, the heaviest first (they are claimed in this order).
    pub tracks: Vec<u32>,
    /// At least two of them are worth a thread.
    pub parallel: bool,
}

/// How a graph's tracks are spread over threads (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct Schedule {
    pub levels: Vec<Level>,
    /// Per track: what its input is the sum of, in the order the
    /// single-threaded engine adds it.
    pub feeds: Vec<Vec<Feed>>,
    /// What the output bus is the sum of, in order.
    pub out: Vec<Feed>,
    /// Some level runs on several threads. `false` also when the graph has
    /// a shape the levels do not cover (then it always runs on one thread).
    pub parallel: bool,
}

/// How much work a track is, for the order of a level (a rough guess:
/// devices, clips).
fn weight(t: &TrackNode) -> u32 {
    let mut devices = 0u32;
    for_each_proc(&t.devices, &mut |_| devices += 1);
    devices * 4 + u32::from(!t.clips.is_empty())
}

impl Schedule {
    /// The levels of `tracks` (graph order). Control thread.
    pub fn build(tracks: &[TrackNode]) -> Self {
        let n = tracks.len();
        let mut feeds: Vec<Vec<Feed>> = vec![Vec::new(); n];
        let mut out = Vec::new();
        let mut sound = true;
        for (i, t) in tracks.iter().enumerate() {
            if !t.carries_audio() {
                continue;
            }
            for pass in [true, false] {
                for (k, s) in t.sends.iter().enumerate().filter(|(_, s)| s.pre_fader == pass) {
                    feeds[s.target].push(Feed { track: i as u32, send: Some(k as u16) });
                    sound &= s.target > i;
                }
            }
            match t.output {
                OutputTarget::Track(j) => {
                    feeds[j].push(Feed { track: i as u32, send: None });
                    sound &= j > i;
                }
                OutputTarget::Out => out.push(Feed { track: i as u32, send: None }),
                OutputTarget::None => {}
            }
        }
        // A sum into a track that never reads it, or one fed from a later
        // track, is not what the single path does: keep to it.
        for (j, f) in feeds.iter().enumerate() {
            sound &= f.is_empty() || (tracks[j].has_input && tracks[j].carries_audio());
        }

        // Sidechain slot → the track that writes it.
        let slots = tracks.iter().filter_map(|t| t.side_out).max().map_or(0, |m| m + 1);
        let mut writer = vec![usize::MAX; slots];
        for (i, t) in tracks.iter().enumerate() {
            if let Some(s) = t.side_out
                && t.carries_audio()
            {
                writer[s] = i;
            }
        }

        let mut level = vec![0u32; n];
        for (i, t) in tracks.iter().enumerate() {
            if !t.carries_audio() {
                continue;
            }
            let mut l = level[i];
            for f in &feeds[i] {
                l = l.max(level[f.track as usize] + 1);
            }
            let mut later = Vec::new();
            for_each_proc(&t.devices, &mut |p| {
                if let Some(&src) = p.sidechain.and_then(|s| writer.get(s)) {
                    if src < i {
                        l = l.max(level[src] + 1);
                    } else if src > i && src != usize::MAX {
                        later.push(src);
                    }
                }
            });
            level[i] = l;
            // A listener before its source hears the previous block: the
            // source writes this block's only after the listener has read.
            for src in later {
                level[src] = level[src].max(l + 1);
            }
        }

        // One processor in two tracks would be run by two threads at once.
        let mut owner: HashMap<*const crate::shared::SharedProcessor, usize> = HashMap::new();
        for (i, t) in tracks.iter().enumerate() {
            for_each_proc(&t.devices, &mut |p| {
                if *owner.entry(std::sync::Arc::as_ptr(&p.proc)).or_insert(i) != i {
                    sound = false;
                }
            });
        }

        let depth = tracks.iter().enumerate().filter(|(_, t)| t.carries_audio()).map(|(i, _)| level[i] as usize + 1).max().unwrap_or(0);
        let mut levels: Vec<Level> = (0..depth).map(|_| Level::default()).collect();
        for (i, t) in tracks.iter().enumerate() {
            if t.carries_audio() {
                levels[level[i] as usize].tracks.push(i as u32);
            }
        }
        for lv in &mut levels {
            // Stable: equal weights keep graph order.
            lv.tracks.sort_by_key(|&i| std::cmp::Reverse(weight(&tracks[i as usize])));
            let heavy = lv.tracks.iter().filter(|&&i| weight(&tracks[i as usize]) > 0).count();
            lv.parallel = heavy >= 2 && lv.tracks.len() <= MAX_JOBS;
        }
        let parallel = sound && levels.iter().any(|l| l.parallel);
        Self { levels, feeds, out, parallel }
    }
}

// ───────────────────────────── Disjoint access ─────────────────────────────

/// A slice whose elements several threads use at once, each its own (see
/// the module docs for why that holds).
pub(crate) struct Slots<'a, T> {
    ptr: *mut T,
    len: usize,
    _owns: PhantomData<&'a mut [T]>,
}

// SAFETY: a `Slots` only gives access through the `unsafe` methods below,
// whose callers guarantee that no element is used mutably by two threads,
// nor read while another thread writes it. Elements are handed `&mut` to
// other threads (`T: Send`) and read `&` from several (`T: Sync`).
unsafe impl<T: Send + Sync> Sync for Slots<'_, T> {}
unsafe impl<T: Send + Sync> Send for Slots<'_, T> {}

impl<'a, T> Slots<'a, T> {
    pub fn new(items: &'a mut [T]) -> Self {
        Self { ptr: items.as_mut_ptr(), len: items.len(), _owns: PhantomData }
    }

    /// # Safety
    /// No other reference to element `i` is alive while the result is.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get_mut(&self, i: usize) -> &mut T {
        assert!(i < self.len);
        // SAFETY: in bounds; exclusivity is the caller's promise.
        unsafe { &mut *self.ptr.add(i) }
    }

    /// # Safety
    /// No `&mut` to element `i` is alive while the result is.
    pub unsafe fn get(&self, i: usize) -> &T {
        assert!(i < self.len);
        // SAFETY: in bounds; no writer is the caller's promise.
        unsafe { &*self.ptr.add(i) }
    }
}

/// The sidechain buffers of a graph: each slot is written by its source
/// track, after its devices, and read by the devices that listen to it.
///
/// Built from `&mut` ([`SideBus::new`]) it is an ordinary exclusive borrow:
/// one track at a time, reads through `&self`, the own slot's write through
/// `&mut self`. [`SideBus::alias`] makes more of them for the tracks of one
/// level running at once.
pub(crate) struct SideBus<'a> {
    ptr: *mut AudioBuffer,
    len: usize,
    _owns: PhantomData<&'a mut [AudioBuffer]>,
}

// SAFETY: see `alias`: concurrent copies only touch different slots, or
// read the same slot while nobody writes it.
unsafe impl Sync for SideBus<'_> {}
unsafe impl Send for SideBus<'_> {}

impl<'a> SideBus<'a> {
    pub fn new(side: &'a mut [AudioBuffer]) -> Self {
        Self { ptr: side.as_mut_ptr(), len: side.len(), _owns: PhantomData }
    }

    /// Another handle on the same buffers, for a track running alongside
    /// others.
    ///
    /// # Safety
    /// While the handles live, a slot is written through at most one of
    /// them, and never read through another one at the same time (the
    /// [`Schedule`] levels guarantee it: writer and readers of a slot are
    /// never in the same level).
    pub unsafe fn alias(&self) -> SideBus<'a> {
        SideBus { ptr: self.ptr, len: self.len, _owns: PhantomData }
    }

    pub fn get(&self, slot: usize) -> Option<&AudioBuffer> {
        // SAFETY: in bounds; no writer of this slot runs now (see `alias`;
        // with a single handle the borrow checker sees to it).
        (slot < self.len).then(|| unsafe { &*self.ptr.add(slot) })
    }

    pub fn get_mut(&mut self, slot: usize) -> Option<&mut AudioBuffer> {
        // SAFETY: in bounds; only the slot's own track writes it, and no
        // reader of it runs now (see `alias`).
        (slot < self.len).then(|| unsafe { &mut *self.ptr.add(slot) })
    }
}

// ───────────────────────────── The parallel segment ─────────────────────────────

/// Compute every track of a segment level by level on `pool`, gathering
/// inputs as described in the module docs, then sum the output bus. `false`
/// when the processing of some track panicked (each was caught on its
/// thread; the caller decides what that means).
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_levels(
    sched: &Schedule,
    tracks: &mut [TrackNode],
    inputs: &mut [AudioBuffer],
    side: &mut [AudioBuffer],
    out_bus: &mut AudioBuffer,
    env: &TrackEnv<'_>,
    pool: &mut WorkerPool,
) -> bool {
    let n = env.frames;
    let mut ok = true;
    {
        let ts = Slots::new(tracks);
        let ins = Slots::new(inputs);
        let bus = SideBus::new(side);
        for level in &sched.levels {
            let job = |k: usize| {
                let i = level.tracks[k] as usize;
                // SAFETY: track `i` is in this level once, so this is the
                // only `&mut` to it and to its input buffer; the tracks its
                // feeds read are of earlier levels, finished, and nobody holds
                // a `&mut` to them; the sidechain slots it reads are written
                // by tracks of other levels, the one it writes is read by
                // tracks of other levels (module docs, "Why the raw
                // pointers are sound").
                unsafe {
                    let t = ts.get_mut(i);
                    let input = if t.has_input {
                        let buf = ins.get_mut(i);
                        buf.set_frames(n);
                        buf.clear();
                        for f in &sched.feeds[i] {
                            buf.add_from(f.buf(ts.get(f.track as usize)), 1.0);
                        }
                        Some(&*buf)
                    } else {
                        None
                    };
                    let mut side = bus.alias();
                    compute_track(t, env, input, &mut side);
                }
            };
            if level.parallel {
                ok &= pool.run(level.tracks.len(), &job);
            } else {
                for k in 0..level.tracks.len() {
                    job(k);
                }
            }
        }
    }
    for f in &sched.out {
        out_bus.add_from(f.buf(&tracks[f.track as usize]), 1.0);
    }
    ok
}
