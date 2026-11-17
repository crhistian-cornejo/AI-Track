//! Grooves in the engine: a clip's notes stay on their grid and are moved
//! when played, to the sample, the same live and in an export, whatever the
//! block size or where playback starts.

use crate::alloc_counter::count_allocs;
use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use crate::{AudioProcessor, Engine, EngineConfig, EngineHandle};
use aitrack_dsp::{AudioBuffer, Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::groove::{Groove, GrooveSettings, GrooveSlot, PoolGroove};
use aitrack_model::*;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const S16: Tick = PPQ / 4;
/// 120 BPM at 48 kHz: 25 samples per tick.
const SPT: i64 = 25;
/// Two bars: the end of the clips of `rough()`.
const END: Tick = 8 * PPQ;
const KEYS: &str = "probe.keys";

/// `(is note-on, key, velocity 0..=127, position in samples)`.
type Seen = (bool, u8, u8, i64);

struct Keys {
    info: PluginInfo,
    log: Arc<Mutex<Vec<Seen>>>,
}

impl Processor for Keys {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let mut log = self.log.lock().unwrap();
        for e in ctx.events {
            let at = ctx.transport.position_samples + e.offset as i64;
            match e.kind {
                EventKind::NoteOn { key, velocity, .. } => log.push((true, key, (velocity * 127.0).round() as u8, at)),
                EventKind::NoteOff { key, .. } => log.push((false, key, 0, at)),
                EventKind::AllNotesOff => log.push((false, 255, 0, at)),
                _ => {}
            }
        }
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

struct Probe {
    log: Arc<Mutex<Vec<Seen>>>,
}

impl Default for Probe {
    fn default() -> Self {
        // (Room for every event of the longest test: pushing must not allocate.)
        Self { log: Arc::new(Mutex::new(Vec::with_capacity(200_000))) }
    }
}

impl ProcessorFactory for Probe {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        (plugin_id == KEYS).then(|| {
            let info = PluginInfo { id: KEYS, name: "Probe", vendor: "AI-Track", category: Category::Effect, tags: &[], params: vec![ParamInfo::new(0, "Unused", 0.0, 1.0, 0.0)], inputs: 2, outputs: 2, has_sidechain: false };
            Box::new(Keys { info, log: self.log.clone() }) as Box<dyn Processor>
        })
    }
}

impl Probe {
    fn notes(&self) -> Vec<Seen> {
        self.log.lock().unwrap().clone()
    }
}

/// A swing on 16ths: the odd cells `late` of a cell later and `soft` softer.
fn swing(late: f32, soft: f32, cells: usize) -> Groove {
    let mut g = Groove::new("test", S16, cells);
    for (i, s) in g.slots.iter_mut().enumerate() {
        if i % 2 == 1 {
            *s = GrooveSlot { timing: late, velocity: -soft, length: 0.0 };
        }
    }
    g
}

struct B {
    p: Project,
    clip: ClipId,
}

impl B {
    /// An instrument track with the probe, and a clip of `notes`
    /// `(key, start, length, velocity)` using `groove` from the pool.
    fn new(notes: &[(u8, Tick, Tick, f32)], clip_len: Tick, groove: Option<(Groove, GrooveSettings)>) -> Self {
        let mut p = Project::new("groove");
        let track = p.alloc_track_id();
        let mut t = Track::new(track, TrackKind::Instrument, "t", Color(0));
        let did = p.alloc_device_id();
        t.devices.push(Device::stock(did, KEYS, KEYS));
        p.tracks.push(t);
        let sid = p.alloc_source_id();
        let mut src = NoteSource::default();
        for &(key, start, len, vel) in notes {
            let nid = p.alloc_note_id();
            src.notes.push(Note::new(nid, key, start, len, vel));
        }
        src.notes.sort_by_key(|n| (n.start, n.key));
        p.sources.insert(sid, Source::Notes(src));
        let clip = p.alloc_clip_id();
        p.clips.insert(clip, Clip::new("c", Color(0), sid, clip_len));
        if let Some((groove, settings)) = groove {
            let gid = p.alloc_groove_id();
            p.grooves.push(PoolGroove { id: gid, groove, settings });
            p.clip_grooves.insert(clip, gid);
        }
        let pid = p.alloc_placement_id();
        p.track_mut(track).unwrap().arrangement.push(Placement { id: pid, clip, start: 0, lane: 0 });
        Self { p, clip }
    }

    fn render(&self, block: usize, ticks: (Tick, Tick)) -> Vec<Seen> {
        let f = Probe::default();
        render_offline(&self.p, &f, ticks, SR, block).unwrap();
        f.notes()
    }
}

/// Sixteenth notes (key 42, half a cell long) over `cells` cells.
fn sixteenths(cells: i64, vel: f32) -> Vec<(u8, Tick, Tick, f32)> {
    (0..cells).map(|i| (42, i * S16, S16 / 2, vel)).collect()
}

fn start(p: &Project, f: Arc<Probe>) -> (EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), f).unwrap();
    h.update_project(p);
    h.flush();
    let mut proc_ = proc_.unwrap();
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    (h, proc_)
}

fn blocks(proc_: &mut AudioProcessor, n: usize) {
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..n {
        proc_.process_planar(&mut buf);
    }
}

fn open_keys(notes: &[Seen]) -> usize {
    let mut on = [0i32; 128];
    for (is_on, key, _, _) in notes {
        if *key == 255 {
            on = [0; 128];
        } else if *is_on {
            on[*key as usize] += 1;
        } else {
            on[*key as usize] = (on[*key as usize] - 1).max(0);
        }
    }
    on.iter().filter(|c| **c > 0).count()
}

// ───────────────────────────── Tests ─────────────────────────────

#[test]
fn a_swing_lands_on_the_exact_samples_and_softens_the_off_beats() {
    // 62 % swing: the odd cells 0.24 of a cell late (57.6 ticks = 1 440 samples), 50 % softer.
    let b = B::new(&sixteenths(16, 0.8), 16 * S16, Some((swing(0.24, 0.5, 16), GrooveSettings::default())));
    let got = b.render(BLOCK, (0, 4 * PPQ));
    let ons: Vec<Seen> = got.iter().filter(|n| n.0).copied().collect();
    let offs: Vec<i64> = got.iter().filter(|n| !n.0).map(|n| n.3).collect();
    assert_eq!(ons.len(), 16);
    for (i, on) in ons.iter().enumerate() {
        let (at, vel) = if i % 2 == 1 { (i as i64 * 6000 + 1440, 51) } else { (i as i64 * 6000, 102) };
        assert_eq!((on.3, on.2), (at, vel), "cell {i}");
    }
    // Same length, so each note ends the same time after it began.
    for (i, off) in offs.iter().enumerate() {
        let shift = if i % 2 == 1 { 1440 } else { 0 };
        assert_eq!(*off, i as i64 * 6000 + shift + 3000, "note-off {i}");
    }
}

#[test]
fn the_notes_of_the_clip_are_not_touched() {
    let b = B::new(&sixteenths(16, 0.8), 16 * S16, Some((swing(0.24, 0.5, 16), GrooveSettings::default())));
    let before = b.p.clone();
    b.render(BLOCK, (0, 4 * PPQ));
    assert_eq!(b.p, before);
}

fn rough() -> B {
    // Off-grid notes, a random and a quantize in the groove, over two bars.
    let notes: Vec<_> = (0..32).map(|i| (36 + (i % 5) as u8, i * S16 + (i % 7) * 9, S16 / 2 + (i % 3) * 20, 0.5 + (i % 4) as f32 * 0.1)).collect();
    B::new(&notes, 32 * S16, Some((swing(0.3, 0.3, 16), GrooveSettings { quantize: 0.4, timing: 1.0, random: 0.5, velocity: 1.0 })))
}

#[test]
fn live_and_offline_give_the_same_notes_for_any_block_size_and_a_late_start() {
    let b = rough();
    let whole = b.render(BLOCK, (0, END));
    let ons = |v: &[Seen]| v.iter().filter(|n| n.0).copied().collect::<Vec<_>>();
    assert_eq!(ons(&whole).len(), 32);
    for block in [32, 100, 1000, 4096] {
        assert_eq!(b.render(block, (0, END)), whole, "block {block}");
    }
    // Live, block by block.
    let f = Arc::new(Probe::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    blocks(&mut proc_, (END * SPT) as usize / BLOCK + 8);
    let live: Vec<Seen> = f.notes().into_iter().filter(|n| n.3 < END * SPT).collect();
    assert_eq!(live, whole);
    // Starting late: every note-on that sounds after the start is the same one, at the same sample.
    for from in [1, 700, 2_000, 2_431] {
        let at = from * SPT;
        let late = b.render(BLOCK, (from, END));
        let want: Vec<Seen> = ons(&whole).into_iter().filter(|n| n.3 >= at).collect();
        assert_eq!(ons(&late), want, "starting at tick {from}");
    }
}

#[test]
fn the_random_is_a_function_of_position_and_stays_within_its_range() {
    let g = Groove::new("flat", S16, 4);
    let settings = GrooveSettings { random: 1.0, ..GrooveSettings::default() };
    let a = B::new(&sixteenths(64, 0.8), 64 * S16, Some((g.clone(), settings)));
    let first = a.render(BLOCK, (0, 16 * PPQ));
    assert_eq!(a.render(BLOCK, (0, 16 * PPQ)), first, "the same every take");
    let ons: Vec<Seen> = first.iter().filter(|n| n.0).copied().collect();
    let shifts: Vec<i64> = ons.iter().enumerate().map(|(i, n)| n.3 - i as i64 * 6000).collect();
    assert!(shifts.iter().all(|d| d.abs() <= 3000), "within half a cell: {shifts:?}");
    assert!(shifts.iter().any(|d| d.abs() > 1000) && shifts.iter().any(|d| *d < 0) && shifts.iter().any(|d| *d > 0), "and it does move things: {shifts:?}");
    // Another groove in the pool has another sequence.
    let mut b = B::new(&sixteenths(64, 0.8), 64 * S16, Some((g, settings)));
    b.p.grooves[0].id = GrooveId(777);
    b.p.clip_grooves.insert(b.clip, GrooveId(777));
    assert_ne!(b.render(BLOCK, (0, 16 * PPQ)), first);
}

#[test]
fn quantize_pulls_loose_notes_onto_the_grid() {
    let loose: Vec<_> = (0..16).map(|i| (42, i * S16 + 40, S16 / 2, 0.8)).collect();
    let flat = Groove::new("flat", S16, 16);
    let full = B::new(&loose, 16 * S16, Some((flat.clone(), GrooveSettings { quantize: 1.0, ..GrooveSettings::default() })));
    let ons: Vec<i64> = full.render(BLOCK, (0, 4 * PPQ)).iter().filter(|n| n.0).map(|n| n.3).collect();
    assert_eq!(ons, (0..16).map(|i| i * 6000).collect::<Vec<_>>());
    let half = B::new(&loose, 16 * S16, Some((flat, GrooveSettings { quantize: 0.5, ..GrooveSettings::default() })));
    let ons: Vec<i64> = half.render(BLOCK, (0, 4 * PPQ)).iter().filter(|n| n.0).map(|n| n.3).collect();
    assert_eq!(ons, (0..16).map(|i| i * 6000 + 500).collect::<Vec<_>>(), "half way (20 of the 40 ticks)");
}

#[test]
fn a_note_pushed_before_the_start_or_past_the_end_stays_in_the_clip_and_nothing_hangs() {
    // Four cells; the first pulled a whole cell early, the last pushed a whole cell late.
    let mut g = Groove::new("edge", S16, 4);
    g.slots[0].timing = -1.0;
    g.slots[3].timing = 1.0;
    let notes = [(40u8, 0, S16, 1.0), (41, 3 * S16, 3 * S16 / 2, 1.0)];
    let b = B::new(&notes, 4 * S16, Some((g, GrooveSettings::default())));
    let got = b.render(BLOCK, (0, 2 * PPQ));
    let on = |key: u8| got.iter().find(|n| n.0 && n.1 == key).map(|n| n.3);
    let off = |key: u8| got.iter().find(|n| !n.0 && n.1 == key).map(|n| n.3);
    assert_eq!(on(40), Some(0), "plays at the start of the clip, not before it");
    assert_eq!(on(41), Some((4 * S16 - 1) * SPT), "plays on the last tick of the clip");
    assert!(off(41).is_some_and(|o| o <= 4 * S16 * SPT), "and is cut at the end: {got:?}");
    assert_eq!(open_keys(&got), 0);
}

#[test]
fn a_looping_clip_keeps_its_groove_on_every_pass() {
    let mut b = B::new(&sixteenths(8, 0.8), 64 * S16, Some((swing(0.24, 0.0, 8), GrooveSettings::default())));
    b.p.clips.get_mut(&b.clip).unwrap().looping = Some(LoopRegion { start: 0, end: 8 * S16 });
    let got = b.render(BLOCK, (0, 4 * PPQ));
    let ons: Vec<i64> = got.iter().filter(|n| n.0).map(|n| n.3).collect();
    assert_eq!(ons.len(), 16, "eight cells, twice in four beats");
    for (i, at) in ons.iter().enumerate() {
        let shift = if i % 2 == 1 { 1440 } else { 0 };
        assert_eq!(*at, i as i64 * 6000 + shift, "note {i}");
    }
    assert_eq!(open_keys(&got), 0);
}

#[test]
fn the_amount_moves_while_it_plays_without_a_compile_a_double_hit_or_a_hanging_note() {
    let b = B::new(&sixteenths(16, 0.8), 16 * S16, Some((swing(0.24, 0.0, 16), GrooveSettings::default())));
    let f = Arc::new(Probe::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    let compiles = h.compile_count();
    h.play();
    blocks(&mut proc_, 50);
    assert!(h.set_groove_amount(0.0));
    blocks(&mut proc_, 200);
    assert!(h.set_groove_amount(1.3));
    blocks(&mut proc_, 125);
    h.stop();
    blocks(&mut proc_, 4);
    assert_eq!(h.compile_count(), compiles, "no compile");
    let got = f.notes();
    let ons: Vec<i64> = got.iter().filter(|n| n.0).map(|n| n.3).collect();
    assert_eq!(ons.len(), 16, "every note once, none lost or doubled: {ons:?}");
    assert_eq!(open_keys(&got), 0);
    // Before the first change: the swing as set; in the middle: straight; after the second: 130 %.
    assert_eq!(ons[1], 6000 + 1440);
    for i in [9usize, 11, 13, 15] {
        assert_eq!(ons[i], i as i64 * 6000, "cell {i} played with the amount at 0");
    }
}

#[test]
fn the_project_amount_and_the_settings_take_the_same_fast_path() {
    let mut b = B::new(&sixteenths(16, 0.8), 16 * S16, Some((swing(0.24, 0.0, 16), GrooveSettings::default())));
    let f = Arc::new(Probe::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    let compiles = h.compile_count();
    h.play();
    blocks(&mut proc_, 5);
    b.p.groove_amount = 0.5;
    b.p.grooves[0].settings.timing = 0.5;
    h.update_project(&b.p);
    h.flush();
    blocks(&mut proc_, 380);
    assert_eq!(h.compile_count(), compiles, "a project that differs by the groove amount and settings does not compile");
    assert_eq!(h.patch_count(), 1);
    let ons: Vec<i64> = f.notes().iter().filter(|n| n.0).map(|n| n.3).collect();
    // Timing 50 % × amount 50 % = a quarter of the swing: cells far enough on follow it.
    assert_eq!(ons.len(), 16);
    assert_eq!(ons[15], 15 * 6000 + (0.24 * 0.25 * 6000.0) as i64, "{ons:?}");
    // A different template (or another groove for the clip) is a compile.
    b.p.grooves[0].groove.slots[1].timing = 0.1;
    h.update_project(&b.p);
    h.flush();
    blocks(&mut proc_, 2);
    assert_eq!(h.compile_count(), compiles + 1);
}

#[test]
fn changing_the_clips_groove_mid_clip_leaves_no_note_open() {
    let mut b = B::new(&sixteenths(16, 0.8), 16 * S16, Some((swing(0.24, 0.0, 16), GrooveSettings::default())));
    let f = Arc::new(Probe::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    blocks(&mut proc_, 120);
    // The clip loses its groove while notes are sounding.
    b.p.clip_grooves.remove(&b.clip);
    h.update_project(&b.p);
    h.flush();
    blocks(&mut proc_, 100);
    h.stop();
    blocks(&mut proc_, 8);
    assert_eq!(open_keys(&f.notes()), 0, "{:?}", f.notes());
}

#[test]
fn a_groove_on_the_audio_thread_never_allocates() {
    let b = rough();
    let (h, mut proc_) = start(&b.p, Arc::new(Probe::default()));
    h.set_loop(Some((0, 4 * PPQ)));
    h.play();
    blocks(&mut proc_, 8);
    // The handle's side of the commands allocates; queue them first.
    for i in 0..200 {
        h.set_groove_amount(0.2 + (i % 10) as f32 * 0.1);
        h.set_groove_settings(b.p.grooves[0].id, GrooveSettings { random: (i % 5) as f32 / 5.0, ..GrooveSettings::default() });
    }
    let mut buf = AudioBuffer::new(2, BLOCK);
    let ((), allocs) = count_allocs(|| {
        for _ in 0..1500 {
            proc_.process_planar(&mut buf);
        }
    });
    assert_eq!(allocs, 0);
}
