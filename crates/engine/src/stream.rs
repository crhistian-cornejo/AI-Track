//! Disk streaming for long audio files: a clip reads its file through a few
//! pages kept in memory, which a loader thread fills ahead of the playhead.
//!
//! # What streams
//! [`StreamSettings`] decides, per file, when it is loaded: a file whose
//! decoded audio at the device rate is at least `min_seconds` long or
//! `min_megabytes` big streams; shorter ones are decoded into RAM as before
//! ([`crate::sample_cache`]). A streamed file is read from a WAV:
//! * the file itself when it is a 16/24/32-bit integer or 32-bit float PCM
//!   WAV at the device rate (recorded takes, bounces, most stems): only its
//!   header is read when it loads;
//! * otherwise (another format or rate) the cache decodes and resamples it
//!   exactly as the RAM path does and writes the result to a temporary
//!   32-bit float WAV, deleted when the file is no longer used. The decode
//!   still holds the whole file in memory while it runs (once, on the
//!   loader thread); playback does not.
//!
//! Either way a streamed sample has exactly the value the RAM path would
//! hold (the integer conversion is hound's, the float file stores the RAM
//! path's values), so playback is bit-identical as long as no page is late.
//!
//! Warped (pitch-preserving) clips keep rendering through the warp worker
//! into a stretched copy in RAM, as before: the worker reads a streamed
//! source whole for that ([`StreamFile::read_all`]). Streaming covers the
//! clips that read the file itself: native speed, Re-Pitch, transpose.
//!
//! # Pages and readers
//! A file is cut into pages of [`PAGE`] frames. Each playing clip has its
//! own [`StreamReader`] (two clips of one file play different places): a
//! few *pinned* pages, chosen when the graph is compiled (where the clip
//! starts, where its loop starts and the material leading up to it), which
//! are loaded once and kept, so starting and looping never wait for the
//! disk; and a ring of [`RING`] pages the audio thread points at the pages
//! ahead of the playhead ([`LOOKAHEAD`] of them, in the direction it moves,
//! so a reversed clip reads ahead backwards). Readers survive recompiles:
//! the engine's [`StreamHub`] hands the next graph the same reader for the
//! same clip, file and pinned pages, warm.
//!
//! # The audio thread
//! [`StreamReader::frame`] and [`StreamReader::sample`] (the clip player's
//! reads, see `AudioClipData` in `clip.rs`) find the page of a frame through
//! a two-entry cache, so the per-sample cost is a compare and an index.
//! Crossing into another page scans the slots (atomic loads), asks for the
//! pages ahead that are missing (one atomic store per slot and a
//! `Thread::unpark` of the loader: an atomic swap plus, when it sleeps, one
//! futex / ulock wake; no lock, no allocation). A page that is not there yet
//! reads as silence and counts one underrun ([`StreamHub::underruns`], shown
//! in the engine's health); the audio thread never waits.
//!
//! Each slot has one atomic `state`: empty, *wanted* page `p`, or *ready*
//! page `p`. Only the audio thread moves a slot to "wanted" (it picks the
//! victim, never a slot it is reading from), only the loader moves it to
//! "ready" (a compare-and-swap from the exact "wanted" value it filled, so
//! a slot re-aimed meanwhile is not published with the wrong page), and
//! only a "ready" slot's samples are read, only a "wanted" one's written.
//! The release/acquire pairs on `state` order the loader's writes before
//! the audio thread's reads and the reads before the next writes.
//!
//! A reader is used by one thread at a time: its clip is rendered by the
//! thread that computes its track, and the worker pool orders one block
//! after the next. An offline render (export, freeze, bounce) compiles with
//! no hub: its readers are its own and load a missing page on the spot,
//! blocking, so the output is the same as the RAM path's, always.
//!
//! # Seeks and loops
//! While the transport is stopped the engine *cues* the arrangement clips
//! under the playhead every block ([`StreamReader::cue`]): the pages a play
//! from there will read first are loaded before play is pressed. While it
//! plays a loop it cues the clips under the loop start the same way, so the
//! wrap finds its pages (a clip's own loop start is pinned). A jump while
//! playing to a place nothing asked for (a seek, a launch into the middle of
//! a file) plays silence until the page arrives, typically a block or two.

use crate::sample_cache::{interpolate, FrameSource};
use serde::{Deserialize, Serialize};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// Frames per page: 2¹⁴ (≈ 0.34 s at 48 kHz; 128 KB of stereo `f32`).
pub const PAGE_SHIFT: u32 = 14;
pub const PAGE: usize = 1 << PAGE_SHIFT;
const PAGE_MASK: usize = PAGE - 1;
/// Pages of a reader's ring (besides its pinned ones).
pub const RING: usize = 10;
/// Pages asked for ahead of the one playing.
pub const LOOKAHEAD: usize = 3;
/// Most pinned pages per reader.
pub const MAX_PINS: usize = 6;

/// When a file is streamed instead of decoded into RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamSettings {
    /// Stream at all (off: every file is decoded into RAM).
    pub enabled: bool,
    /// Files at least this long stream.
    pub min_seconds: u32,
    /// Files whose decoded audio (`f32` at the device rate) takes at least
    /// this many MB stream, however short.
    pub min_megabytes: u32,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self { enabled: true, min_seconds: 60, min_megabytes: 64 }
    }
}

impl StreamSettings {
    /// Every file is decoded into RAM.
    pub const OFF: Self = Self { enabled: false, min_seconds: 60, min_megabytes: 64 };

    /// Whether a file of `frames` frames and `channels` channels at `rate`
    /// streams.
    pub fn streams(&self, frames: usize, channels: usize, rate: u32) -> bool {
        if !self.enabled || frames == 0 || rate == 0 {
            return false;
        }
        let seconds = frames as f64 / rate as f64;
        let bytes = frames as u64 * channels.max(1) as u64 * 4;
        seconds >= self.min_seconds as f64 || bytes >= self.min_megabytes as u64 * 1024 * 1024
    }
}

// ───────────────────────────── Files ─────────────────────────────

/// How the samples of a WAV are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pcm {
    I16,
    I24,
    I32,
    F32,
}

impl Pcm {
    fn bytes(self) -> usize {
        match self {
            Pcm::I16 => 2,
            Pcm::I24 => 3,
            Pcm::I32 | Pcm::F32 => 4,
        }
    }
}

/// The layout of a WAV's sample data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WavLayout {
    pub channels: usize,
    pub rate: u32,
    pub frames: usize,
    data_offset: u64,
    pcm: Pcm,
}

/// Read exactly `buf.len()` bytes at `offset` (no shared cursor: the file
/// is read from several places).
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            match file.seek_read(&mut buf[done..], offset + done as u64) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The data layout of a WAV that can be read in place: PCM (or
/// `WAVE_FORMAT_EXTENSIBLE` PCM / float) of 16, 24 or 32-bit integers or
/// 32-bit floats, packed (`block_align = channels · bytes`), with a data
/// chunk that fits in the file. `None` for anything else (it is decoded).
pub(crate) fn wav_layout(file: &File) -> Option<WavLayout> {
    let len = file.metadata().ok()?.len();
    let mut head = [0u8; 12];
    read_exact_at(file, &mut head, 0).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12u64;
    let mut fmt: Option<(u16, usize, u32, usize, u16, u16)> = None;
    while pos + 8 <= len {
        let mut ch = [0u8; 8];
        read_exact_at(file, &mut ch, pos).ok()?;
        let size = le32(&ch, 4) as u64;
        let body = pos + 8;
        match &ch[0..4] {
            b"fmt " => {
                let mut f = [0u8; 40];
                let n = size.min(40) as usize;
                if n < 16 {
                    return None;
                }
                read_exact_at(file, &mut f[..n], body).ok()?;
                let tag = le16(&f, 0);
                let channels = le16(&f, 2) as usize;
                let rate = le32(&f, 4);
                let align = le16(&f, 12) as usize;
                let bits = le16(&f, 14);
                let (tag, valid) = if tag == 0xFFFE {
                    if n < 40 {
                        return None;
                    }
                    (le16(&f, 24), le16(&f, 18))
                } else {
                    (tag, bits)
                };
                fmt = Some((tag, channels, rate, align, bits, valid));
            }
            b"data" => {
                let (tag, channels, rate, align, bits, valid) = fmt?;
                let pcm = match (tag, bits) {
                    (1, 16) => Pcm::I16,
                    (1, 24) => Pcm::I24,
                    (1, 32) => Pcm::I32,
                    (3, 32) => Pcm::F32,
                    _ => return None,
                };
                if channels == 0 || rate == 0 || valid != bits || align != channels * pcm.bytes() || body + size > len {
                    return None;
                }
                return Some(WavLayout { channels, rate, frames: (size / align as u64) as usize, data_offset: body, pcm });
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    None
}

/// A WAV a streamed source is read from. Shared by every reader of the
/// file; any thread may read it (positioned reads, no cursor).
pub struct StreamFile {
    path: PathBuf,
    file: File,
    layout: WavLayout,
    /// A temporary conversion: deleted when the last user drops it.
    temp: bool,
}

impl std::fmt::Debug for StreamFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamFile").field("path", &self.path).field("frames", &self.layout.frames).field("channels", &self.layout.channels).finish()
    }
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl StreamFile {
    /// Open `path` in place if it is a WAV that can be read as it is.
    pub fn open_wav(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let layout = wav_layout(&file)?;
        Some(Self { path: path.to_path_buf(), file, layout, temp: false })
    }

    /// Write planar audio to a temporary 32-bit float WAV and stream it
    /// from there (the decoded / resampled form of a file that cannot be
    /// read in place). The file is deleted when this drops.
    pub fn from_decoded(channels: &[Vec<f32>], rate: u32, name: &str) -> io::Result<Self> {
        let dir = std::env::temp_dir().join("aitrack-stream");
        std::fs::create_dir_all(&dir)?;
        let stem: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(40).collect();
        let path = dir.join(format!("{}-{}-{stem}.wav", std::process::id(), TEMP_SEQ.fetch_add(1, Ordering::Relaxed)));
        let spec = hound::WavSpec { channels: channels.len().max(1) as u16, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
        let write = || -> Result<(), hound::Error> {
            let mut w = hound::WavWriter::create(&path, spec)?;
            for i in 0..frames {
                for c in channels {
                    w.write_sample(c[i])?;
                }
            }
            w.finalize()
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&path);
            return Err(io::Error::other(e.to_string()));
        }
        match Self::open_wav(&path) {
            Some(mut f) => {
                f.temp = true;
                Ok(f)
            }
            None => {
                let _ = std::fs::remove_file(&path);
                Err(io::Error::other("the converted file does not read back"))
            }
        }
    }

    pub fn frames(&self) -> usize {
        self.layout.frames
    }

    pub fn channels(&self) -> usize {
        self.layout.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.layout.rate
    }

    /// The WAV the samples are read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pages(&self) -> usize {
        self.layout.frames.div_ceil(PAGE)
    }

    /// Read page `page` into `out` (planar: channel `c` at `c · PAGE`),
    /// zero past the end of the file. `scratch` holds the raw bytes.
    pub fn read_page(&self, page: usize, out: &mut [f32], scratch: &mut Vec<u8>) -> io::Result<()> {
        let ch = self.layout.channels;
        debug_assert!(out.len() >= ch * PAGE);
        let first = page * PAGE;
        let n = self.layout.frames.saturating_sub(first).min(PAGE);
        let bpf = ch * self.layout.pcm.bytes();
        scratch.resize(n * bpf, 0);
        let res = if n > 0 { read_exact_at(&self.file, &mut scratch[..n * bpf], self.layout.data_offset + (first * bpf) as u64) } else { Ok(()) };
        if let Err(e) = res {
            out.fill(0.0);
            return Err(e);
        }
        self.convert(&scratch[..n * bpf], n, |c, i, v| out[c * PAGE + i] = v);
        for c in 0..ch {
            out[c * PAGE + n..(c + 1) * PAGE].fill(0.0);
        }
        Ok(())
    }

    /// The whole file, planar (for a stretched render of a streamed source).
    pub fn read_all(&self) -> io::Result<Vec<Vec<f32>>> {
        let ch = self.layout.channels;
        let frames = self.layout.frames;
        let bpf = ch * self.layout.pcm.bytes();
        let mut out = vec![vec![0.0f32; frames]; ch];
        let mut scratch = vec![0u8; PAGE * bpf];
        let mut first = 0;
        while first < frames {
            let n = (frames - first).min(PAGE);
            read_exact_at(&self.file, &mut scratch[..n * bpf], self.layout.data_offset + (first * bpf) as u64)?;
            self.convert(&scratch[..n * bpf], n, |c, i, v| out[c][first + i] = v);
            first += n;
        }
        Ok(out)
    }

    /// Interleaved bytes → samples, with the RAM path's (hound's) conversion:
    /// integers sign-extended to `i32`, then `v as f32 · 2^-(bits−1)`.
    fn convert(&self, raw: &[u8], n: usize, mut put: impl FnMut(usize, usize, f32)) {
        let ch = self.layout.channels;
        let pcm = self.layout.pcm;
        let b = pcm.bytes();
        let bits = (b * 8) as u32;
        let scale = 1.0 / (1i64 << (bits - 1)) as f32;
        for i in 0..n {
            for c in 0..ch {
                let s = &raw[(i * ch + c) * b..];
                let v = match pcm {
                    Pcm::I16 => i16::from_le_bytes([s[0], s[1]]) as i32 as f32 * scale,
                    Pcm::I24 => ((((s[0] as u32) | (s[1] as u32) << 8 | (s[2] as u32) << 16) << 8) as i32 >> 8) as f32 * scale,
                    Pcm::I32 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 * scale,
                    Pcm::F32 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                };
                put(c, i, v);
            }
        }
    }
}

impl Drop for StreamFile {
    fn drop(&mut self) {
        if self.temp {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ───────────────────────────── Readers ─────────────────────────────

const EMPTY: u64 = 0;
const WANT: u64 = 1;
const READY: u64 = 2;
const NONE: usize = usize::MAX;

#[inline]
fn want(page: usize) -> u64 {
    (page as u64) << 2 | WANT
}

#[inline]
fn ready(page: usize) -> u64 {
    (page as u64) << 2 | READY
}

#[inline]
fn tag(state: u64) -> u64 {
    state & 3
}

#[inline]
fn page_of(state: u64) -> usize {
    (state >> 2) as usize
}

struct Slot {
    state: AtomicU64,
    /// `channels · PAGE` samples once the loader first fills the slot.
    data: UnsafeCell<Vec<f32>>,
}

#[derive(Clone, Copy)]
struct Hot {
    page: usize,
    data: *const f32,
}

const COLD: Hot = Hot { page: NONE, data: std::ptr::null() };

/// The audio thread's side of a reader.
struct RtState {
    hot: [Hot; 2],
    /// Per slot: `clock` when last used (least recently used is re-aimed).
    used: Box<[u64]>,
    clock: u64,
    /// A page being waited for, and its slot.
    pending: (usize, usize),
    /// The page whose miss was counted last (one count per page missed).
    missed: usize,
    /// Raw bytes of a blocking load.
    scratch: Vec<u8>,
}

/// One clip's view of a streamed file (see the module docs).
pub struct StreamReader {
    file: Arc<StreamFile>,
    channels: usize,
    frames: usize,
    slots: Box<[Slot]>,
    /// `slots[..pinned]` hold the pinned pages and are never re-aimed.
    pinned: usize,
    pins: Box<[usize]>,
    /// The way the clip reads the file (`false`: a reversed clip, which
    /// reads ahead towards the start). Loops and seeks jump, but a clip
    /// never turns round.
    forward: bool,
    rt: UnsafeCell<RtState>,
    /// `None`: an offline reader that loads a missing page itself.
    hub: Option<Arc<HubShared>>,
}

// SAFETY: see the module docs. `rt` is touched by one thread at a time (the
// one rendering the clip); a slot's `data` is written only by the loader
// while the slot is "wanted" and read only while it is "ready", the state
// moving between the two with release / acquire.
unsafe impl Send for StreamReader {}
unsafe impl Sync for StreamReader {}

impl std::fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamReader").field("file", &self.file).field("pins", &self.pins).field("live", &self.hub.is_some()).finish()
    }
}

/// The pages to pin for frames `frames` of `file` (deduplicated, in range).
fn pin_pages(file: &StreamFile, frames: &[i64]) -> Vec<usize> {
    let pages = file.pages() as i64;
    let mut out: Vec<usize> = Vec::new();
    for &f in frames {
        let p = f.div_euclid(PAGE as i64);
        if (0..pages).contains(&p) && !out.contains(&(p as usize)) && out.len() < MAX_PINS {
            out.push(p as usize);
        }
    }
    out
}

impl StreamReader {
    fn build(file: Arc<StreamFile>, pins: Vec<usize>, forward: bool, hub: Option<Arc<HubShared>>) -> Self {
        let pinned = if hub.is_some() { pins.len() } else { 0 };
        let n = pinned + RING;
        let slots: Box<[Slot]> = (0..n)
            .map(|k| Slot { state: AtomicU64::new(if k < pinned { want(pins[k]) } else { EMPTY }), data: UnsafeCell::new(Vec::new()) })
            .collect();
        let rt = RtState { hot: [COLD; 2], used: vec![0; n].into_boxed_slice(), clock: 0, pending: (NONE, 0), missed: NONE, scratch: Vec::new() };
        Self { channels: file.channels().max(1), frames: file.frames(), file, slots, pinned, pins: pins.into_boxed_slice(), forward, rt: UnsafeCell::new(rt), hub }
    }

    /// A reader for an offline render: every missing page is loaded on the
    /// spot (blocking), so nothing is ever missed.
    pub fn blocking(file: Arc<StreamFile>) -> Self {
        Self::build(file, Vec::new(), true, None)
    }

    pub fn file(&self) -> &Arc<StreamFile> {
        &self.file
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Pinned pages (frame / [`PAGE`]).
    pub fn pins(&self) -> &[usize] {
        &self.pins
    }

    /// Whether the loader has every page this reader asked for (control /
    /// test side).
    pub fn is_settled(&self) -> bool {
        self.slots.iter().all(|s| tag(s.state.load(Ordering::Acquire)) != WANT)
    }

    /// Pages in memory.
    pub fn ready_pages(&self) -> usize {
        self.slots.iter().filter(|s| tag(s.state.load(Ordering::Acquire)) == READY).count()
    }

    #[inline]
    #[allow(clippy::mut_from_ref)]
    fn rt(&self) -> &mut RtState {
        // SAFETY: one thread at a time (see the `Sync` impl).
        unsafe { &mut *self.rt.get() }
    }

    /// Exact stored sample at an integer frame; 0 outside the file and on a
    /// page that is not loaded yet (counted as an underrun).
    #[inline]
    pub fn frame(&self, ch: usize, index: i64) -> f32 {
        if index < 0 || index as u64 >= self.frames as u64 {
            return 0.0;
        }
        let i = index as usize;
        match self.page(i >> PAGE_SHIFT) {
            // SAFETY: a ready page holds `channels · PAGE` samples.
            Some(p) => unsafe { *p.add(ch.min(self.channels - 1) * PAGE + (i & PAGE_MASK)) },
            None => 0.0,
        }
    }

    /// The windowed-sinc read of [`crate::sample_cache::DecodedAudio::sample`]
    /// on the streamed data (the same kernel, the same result).
    #[inline]
    pub fn sample(&self, ch: usize, pos: f64) -> f32 {
        interpolate(self, ch, pos)
    }

    /// Data of a page, if ready.
    #[inline]
    fn page(&self, page: usize) -> Option<*const f32> {
        let rt = self.rt();
        if rt.hot[0].page == page {
            return Some(rt.hot[0].data);
        }
        if rt.hot[1].page == page {
            rt.hot.swap(0, 1);
            return Some(rt.hot[0].data);
        }
        self.switch(rt, page)
    }

    fn find(&self, page: usize) -> Option<(usize, u64)> {
        self.slots.iter().enumerate().find_map(|(k, s)| {
            let st = s.state.load(Ordering::Acquire);
            (tag(st) != EMPTY && page_of(st) == page).then_some((k, st))
        })
    }

    /// A ready slot `k`: make it the hot page.
    fn land(&self, rt: &mut RtState, k: usize, page: usize) -> *const f32 {
        // SAFETY: the slot is ready: the loader no longer touches its data.
        let data = unsafe { (*self.slots[k].data.get()).as_ptr() };
        rt.used[k] = rt.clock;
        rt.hot[1] = rt.hot[0];
        rt.hot[0] = Hot { page, data };
        if rt.pending.0 == page {
            rt.pending = (NONE, 0);
        }
        data
    }

    #[cold]
    fn switch(&self, rt: &mut RtState, page: usize) -> Option<*const f32> {
        // Waiting for this very page: check its slot only.
        if rt.pending.0 == page {
            let k = rt.pending.1;
            if self.slots[k].state.load(Ordering::Acquire) != ready(page) {
                return None;
            }
        }
        rt.clock += 1;
        if let Some((k, st)) = self.find(page) {
            if tag(st) == READY {
                let data = self.land(rt, k, page);
                self.ahead(rt, page);
                return Some(data);
            }
            // Asked for, not loaded yet.
            self.missed(rt, page, k);
            self.ahead(rt, page);
            return None;
        }
        let k = self.victim(rt);
        if self.hub.is_none() {
            self.load_now(rt, k, page);
            return Some(self.land(rt, k, page));
        }
        self.slots[k].state.store(want(page), Ordering::Release);
        rt.used[k] = rt.clock;
        self.missed(rt, page, k);
        self.ahead(rt, page);
        self.wake();
        None
    }

    fn missed(&self, rt: &mut RtState, page: usize, k: usize) {
        rt.pending = (page, k);
        if rt.missed != page {
            rt.missed = page;
            if let Some(h) = &self.hub {
                h.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Ask for the [`LOOKAHEAD`] pages after `page` (before it, reversed).
    fn ahead(&self, rt: &mut RtState, page: usize) {
        if self.hub.is_some() && self.ask_run(rt, page, 1, LOOKAHEAD) {
            self.wake();
        }
    }

    /// Ask for pages `page + d·dir` for `d` in `from..=to`, `dir` the way
    /// the clip reads; `true` if any slot was re-aimed.
    fn ask_run(&self, rt: &mut RtState, page: usize, from: usize, to: usize) -> bool {
        let pages = self.file.pages();
        let mut posted = false;
        for d in from..=to {
            let p = if self.forward { page + d } else { page.wrapping_sub(d) };
            if p >= pages {
                break;
            }
            posted |= self.ask(rt, p);
        }
        posted
    }

    /// Make sure page `p` is loaded or on its way; `true` if a slot was
    /// re-aimed at it.
    fn ask(&self, rt: &mut RtState, p: usize) -> bool {
        if let Some((k, _)) = self.find(p) {
            rt.used[k] = rt.clock;
            return false;
        }
        let k = self.victim(rt);
        self.slots[k].state.store(want(p), Ordering::Release);
        rt.used[k] = rt.clock;
        true
    }

    /// The ring slot to re-aim: the least recently used one that is not hot.
    fn victim(&self, rt: &mut RtState) -> usize {
        let mut best = self.pinned;
        let mut best_used = u64::MAX;
        for k in self.pinned..self.slots.len() {
            let st = self.slots[k].state.load(Ordering::Relaxed);
            let p = page_of(st);
            if tag(st) == READY && (p == rt.hot[0].page || p == rt.hot[1].page) {
                continue;
            }
            if rt.used[k] < best_used {
                best = k;
                best_used = rt.used[k];
            }
        }
        if rt.pending.1 == best {
            rt.pending = (NONE, 0);
        }
        best
    }

    /// Offline: read page `page` into slot `k` now.
    fn load_now(&self, rt: &mut RtState, k: usize, page: usize) {
        // SAFETY: an offline reader has no loader: this thread owns every slot.
        let data = unsafe { &mut *self.slots[k].data.get() };
        if data.len() != self.channels * PAGE {
            *data = vec![0.0; self.channels * PAGE];
        }
        if let Err(e) = self.file.read_page(page, data, &mut rt.scratch) {
            log::warn!("streaming {}: page {page}: {e}", self.file.path.display());
        }
        self.slots[k].state.store(ready(page), Ordering::Release);
    }

    fn wake(&self) {
        if let Some(h) = &self.hub {
            h.wake();
        }
    }

    /// A play will jump to source frame `frame` of this clip (the transport
    /// is stopped there, or loops back there): have the page of `frame` and
    /// the `pages − 1` after it loaded and kept. Real-time safe (like a
    /// read: atomic stores and a wake).
    pub fn cue(&self, frame: i64, pages: usize) {
        if self.hub.is_none() || frame < 0 || frame as u64 >= self.frames as u64 || pages == 0 {
            return;
        }
        let rt = self.rt();
        rt.clock += 1;
        let page = frame as usize >> PAGE_SHIFT;
        if self.ask_run(rt, page, 0, pages - 1) {
            self.wake();
        }
    }

    /// Loader side: fill every wanted slot. `true` if any was.
    fn fill(&self, scratch: &mut Vec<u8>, bytes: &AtomicUsize) -> bool {
        let mut did = false;
        for s in self.slots.iter() {
            let st = s.state.load(Ordering::Acquire);
            if tag(st) != WANT {
                continue;
            }
            let page = page_of(st);
            // SAFETY: the slot is wanted: the audio thread does not read its
            // data until it sees it ready, which only this thread publishes.
            let data = unsafe { &mut *s.data.get() };
            if data.len() != self.channels * PAGE {
                *data = vec![0.0; self.channels * PAGE];
                bytes.fetch_add(data.len() * 4, Ordering::Relaxed);
            }
            if let Err(e) = self.file.read_page(page, data, scratch) {
                log::warn!("streaming {}: page {page}: {e}", self.file.path.display());
            }
            // Re-aimed meanwhile: the next pass loads the new page.
            let _ = s.state.compare_exchange(st, ready(page), Ordering::AcqRel, Ordering::Relaxed);
            did = true;
        }
        did
    }

    fn buffer_bytes(&self) -> usize {
        // SAFETY: called when the reader is dropped (no other user left).
        self.slots.iter().map(|s| unsafe { (*s.data.get()).len() * 4 }).sum()
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        if let Some(h) = &self.hub {
            h.buffer_bytes.fetch_sub(self.buffer_bytes(), Ordering::Relaxed);
        }
    }
}

impl FrameSource for StreamReader {
    #[inline]
    fn channel_count(&self) -> usize {
        self.channels
    }

    #[inline]
    fn frame_count(&self) -> usize {
        self.frames
    }

    #[inline]
    fn window(&self, ch: usize, first: usize, len: usize) -> Option<&[f32]> {
        let page = first >> PAGE_SHIFT;
        if (first + len - 1) >> PAGE_SHIFT != page {
            return None;
        }
        let p = self.page(page)?;
        let at = ch.min(self.channels - 1) * PAGE + (first & PAGE_MASK);
        // SAFETY: a ready page holds `channels · PAGE` samples and stays
        // ready (unchanged) while this thread reads it.
        Some(unsafe { std::slice::from_raw_parts(p.add(at), len) })
    }

    #[inline]
    fn at(&self, ch: usize, index: usize) -> f32 {
        self.frame(ch, index as i64)
    }
}

// ───────────────────────────── The loader ─────────────────────────────

/// Which clip a reader belongs to: `(track, clip)`. Per track as well,
/// so two tracks never share one (their clips may render at the same time
/// on different DSP threads).
pub type ReaderKey = (u64, u64);

struct HubShared {
    readers: Mutex<Vec<Weak<StreamReader>>>,
    /// The reader of each clip, for the next compile to reuse.
    by_key: Mutex<HashMap<ReaderKey, Weak<StreamReader>>>,
    loader: std::sync::OnceLock<std::thread::Thread>,
    underruns: AtomicU64,
    buffer_bytes: AtomicUsize,
    stop: AtomicBool,
}

impl HubShared {
    fn wake(&self) {
        if let Some(t) = self.loader.get() {
            t.unpark();
        }
    }

    fn live(&self) -> Vec<Arc<StreamReader>> {
        let mut list = self.readers.lock().unwrap_or_else(|p| p.into_inner());
        list.retain(|w| w.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    }
}

/// [`crate::EngineHandle::stream_stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Files of the cache that stream from disk.
    pub files: usize,
    /// Memory the pages of the live readers take.
    pub buffer_bytes: usize,
    /// Pages the audio thread needed before the disk delivered them.
    pub underruns: u64,
}

/// The live engine's streaming: the loader thread and the readers it fills.
/// Cheap to clone (a handle). The thread stops when the last handle and the
/// last reader are gone.
#[derive(Clone)]
pub struct StreamHub {
    shared: Arc<HubShared>,
    _owner: Arc<HubOwner>,
}

/// Stops the loader when the last [`StreamHub`] handle drops.
struct HubOwner(Arc<HubShared>);

impl Drop for HubOwner {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
        self.0.wake();
    }
}

/// How long the loader sleeps when no reader wants anything (it is woken
/// as soon as one does; this is only a backstop).
const IDLE: Duration = Duration::from_millis(50);

impl StreamHub {
    pub fn new() -> Self {
        let shared = Arc::new(HubShared {
            readers: Mutex::new(Vec::new()),
            by_key: Mutex::new(HashMap::new()),
            loader: std::sync::OnceLock::new(),
            underruns: AtomicU64::new(0),
            buffer_bytes: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&shared);
        let spawned = std::thread::Builder::new().name("aitrack-disk-stream".into()).spawn(move || {
            let mut scratch = Vec::new();
            loop {
                let Some(sh) = weak.upgrade() else { break };
                if sh.stop.load(Ordering::Acquire) {
                    break;
                }
                let mut did = false;
                for r in sh.live() {
                    did |= r.fill(&mut scratch, &sh.buffer_bytes);
                }
                drop(sh);
                if !did {
                    std::thread::park_timeout(IDLE);
                }
            }
        });
        match spawned {
            Ok(h) => {
                let _ = shared.loader.set(h.thread().clone());
            }
            Err(e) => log::error!("disk streaming thread failed to start: {e}"),
        }
        Self { _owner: Arc::new(HubOwner(shared.clone())), shared }
    }

    /// The reader of clip `key` for `file` with the pages of `pin_frames`
    /// pinned, reading `forward` (or backwards) from its start: the one the
    /// previous graph used when it is the same file, pages and direction
    /// (warm), else a new one whose pinned pages start loading now. Control
    /// thread.
    pub fn reader(&self, key: ReaderKey, file: &Arc<StreamFile>, pin_frames: &[i64], forward: bool) -> Arc<StreamReader> {
        let pins = pin_pages(file, pin_frames);
        let mut by_key = self.shared.by_key.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(r) = by_key.get(&key).and_then(Weak::upgrade)
            && Arc::ptr_eq(&r.file, file)
            && *r.pins == *pins
            && r.forward == forward
        {
            return r;
        }
        let r = Arc::new(StreamReader::build(file.clone(), pins, forward, Some(self.shared.clone())));
        by_key.retain(|_, w| w.strong_count() > 0);
        by_key.insert(key, Arc::downgrade(&r));
        drop(by_key);
        self.shared.readers.lock().unwrap_or_else(|p| p.into_inner()).push(Arc::downgrade(&r));
        self.shared.wake();
        r
    }

    /// Pages the audio thread found missing since the engine started (each
    /// played as silence).
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Memory the readers' pages take.
    pub fn buffer_bytes(&self) -> usize {
        self.shared.buffer_bytes.load(Ordering::Relaxed)
    }

    /// Readers alive (clips of the current and recent graphs).
    pub fn readers(&self) -> usize {
        self.shared.live().len()
    }

    /// Wait until no reader waits for a page (tests, offline helpers).
    pub fn wait_settled(&self, timeout: Duration) -> bool {
        let t0 = std::time::Instant::now();
        loop {
            if self.shared.live().iter().all(|r| r.is_settled()) {
                return true;
            }
            if t0.elapsed() > timeout {
                return false;
            }
            self.shared.wake();
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

impl Default for StreamHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(name: &str, spec: hound::WavSpec, frames: usize, value: impl Fn(usize, usize) -> f64) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitrack-stream-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..frames {
            for c in 0..spec.channels as usize {
                let v = value(i, c);
                match (spec.sample_format, spec.bits_per_sample) {
                    (hound::SampleFormat::Float, _) => w.write_sample(v as f32).unwrap(),
                    (_, 16) => w.write_sample((v * 32767.0) as i16).unwrap(),
                    (_, b) => w.write_sample((v * ((1i64 << (b - 1)) - 1) as f64) as i32).unwrap(),
                }
            }
        }
        w.finalize().unwrap();
        path
    }

    fn tone(i: usize, c: usize) -> f64 {
        ((i as f64 * 0.013 + c as f64).sin() * 0.8).clamp(-1.0, 1.0)
    }

    #[test]
    fn every_supported_wav_reads_as_the_ram_path_decodes_it() {
        let frames = PAGE * 2 + 1234;
        for (bits, fmt) in [(16, hound::SampleFormat::Int), (24, hound::SampleFormat::Int), (32, hound::SampleFormat::Int), (32, hound::SampleFormat::Float)] {
            let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: bits, sample_format: fmt };
            let path = wav(&format!("fmt-{bits}-{fmt:?}.wav"), spec, frames, tone);
            let (ram, rate) = crate::sample_cache::decode_file(&path).unwrap();
            assert_eq!(rate, 48_000);
            let f = Arc::new(StreamFile::open_wav(&path).expect("read in place"));
            assert_eq!((f.frames(), f.channels()), (frames, 2));
            let r = StreamReader::blocking(f.clone());
            for (c, chan) in ram.iter().enumerate() {
                for i in (0..frames).step_by(7).chain(frames - 3..frames + 2) {
                    let want = chan.get(i).copied().unwrap_or(0.0);
                    assert_eq!(r.frame(c, i as i64).to_bits(), want.to_bits(), "{bits}-bit {fmt:?} ch {c} frame {i}");
                }
            }
            let all = f.read_all().unwrap();
            assert!(all.iter().zip(&ram).all(|(a, b)| a.iter().map(|v| v.to_bits()).eq(b.iter().map(|v| v.to_bits()))));
        }
    }

    #[test]
    fn other_wavs_are_not_read_in_place() {
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 8, sample_format: hound::SampleFormat::Int };
        let path = wav("eight.wav", spec, 100, |_, _| 0.0);
        assert!(StreamFile::open_wav(&path).is_none());
        let junk = path.with_file_name("junk.wav");
        std::fs::write(&junk, b"RIFF\0\0\0\0WAVEjunk").unwrap();
        assert!(StreamFile::open_wav(&junk).is_none());
    }

    #[test]
    fn a_converted_file_holds_the_values_it_was_given_and_goes_away() {
        let chans = vec![(0..PAGE + 10).map(|i| (i as f32 * 0.37).sin() / 3.0).collect::<Vec<_>>(), vec![0.25; PAGE + 10]];
        let f = StreamFile::from_decoded(&chans, 44_100, "song name.mp3").unwrap();
        let path = f.path().to_path_buf();
        assert!(path.exists());
        assert_eq!((f.frames(), f.channels(), f.sample_rate()), (PAGE + 10, 2, 44_100));
        let r = StreamReader::blocking(Arc::new(f));
        for i in [0, 1, PAGE - 1, PAGE, PAGE + 9] {
            assert_eq!(r.frame(0, i as i64).to_bits(), chans[0][i].to_bits());
            assert_eq!(r.frame(1, i as i64), 0.25);
        }
        drop(r);
        assert!(!path.exists(), "the temporary conversion is deleted");
    }

    #[test]
    fn the_settings_pick_long_and_big_files() {
        let s = StreamSettings::default();
        assert!(!s.streams(48_000 * 59, 2, 48_000));
        assert!(s.streams(48_000 * 60, 2, 48_000));
        // 16 channels of 30 s: 92 MB.
        assert!(s.streams(48_000 * 30, 16, 48_000));
        assert!(!StreamSettings::OFF.streams(48_000 * 600, 2, 48_000));
        assert!(!s.streams(0, 2, 48_000));
    }

    #[test]
    fn pins_are_pages_in_range_without_repeats() {
        let chans = vec![vec![0.0f32; PAGE * 3 + 5]];
        let f = StreamFile::from_decoded(&chans, 48_000, "pins").unwrap();
        assert_eq!(pin_pages(&f, &[0, 10, PAGE as i64, -5, (PAGE * 9) as i64, (PAGE * 3) as i64]), vec![0, 1, 3]);
    }

    /// A page that is not loaded reads as silence and counts once; the
    /// loader then brings it in and the same read returns the data.
    #[test]
    fn a_late_page_is_silence_and_counts_one_underrun() {
        let chans = vec![(0..PAGE * 8).map(|i| 1.0 + (i % 100) as f32).collect::<Vec<_>>()];
        let f = Arc::new(StreamFile::from_decoded(&chans, 48_000, "late").unwrap());
        let hub = StreamHub::new();
        let r = hub.reader((1, 1), &f, &[0], true);
        let at = (PAGE * 5 + 17) as i64;
        assert_eq!(r.frame(0, at), 0.0, "not loaded yet: silence");
        assert_eq!(hub.underruns(), 1);
        // Reading on from the same page does not count again.
        for k in 0..100 {
            let _ = r.frame(0, at + k);
        }
        assert_eq!(hub.underruns(), 1);
        assert!(hub.wait_settled(Duration::from_secs(5)));
        assert_eq!(r.frame(0, at), chans[0][at as usize]);
        // The pages ahead were asked for: reading on finds them.
        let before = hub.underruns();
        for i in (at as usize..PAGE * 8).step_by(PAGE / 2) {
            assert!(hub.wait_settled(Duration::from_secs(5)));
            assert_eq!(r.frame(0, i as i64), chans[0][i], "frame {i}");
        }
        assert_eq!(hub.underruns(), before, "reading ahead at playback pace never misses");
        assert!(hub.buffer_bytes() > 0);
        drop(r);
        assert_eq!(hub.buffer_bytes(), 0);
    }

    #[test]
    fn pinned_pages_load_without_being_read_and_readers_are_reused() {
        let chans = vec![vec![0.5f32; PAGE * 10]];
        let f = Arc::new(StreamFile::from_decoded(&chans, 48_000, "pinned").unwrap());
        let hub = StreamHub::new();
        let r = hub.reader((1, 7), &f, &[0, (PAGE * 6 + 3) as i64], true);
        assert_eq!(r.pins(), &[0, 6]);
        assert!(hub.wait_settled(Duration::from_secs(5)));
        assert_eq!(r.ready_pages(), 2);
        assert_eq!(r.frame(0, (PAGE * 6) as i64), 0.5);
        assert_eq!(hub.underruns(), 0);
        let again = hub.reader((1, 7), &f, &[0, (PAGE * 6 + 100) as i64], true);
        assert!(Arc::ptr_eq(&r, &again), "same clip, file and pages: the warm reader");
        let other = hub.reader((1, 7), &f, &[PAGE as i64], true);
        assert!(!Arc::ptr_eq(&r, &other), "other pinned pages: a new reader");
        assert_eq!(hub.reader((1, 8), &f, &[0], true).pins(), &[0]);
        // The same clip on another track: its own reader (the tracks may run at once).
        let r2 = hub.reader((2, 7), &f, &[0, (PAGE * 6 + 3) as i64], true);
        assert!(!Arc::ptr_eq(&r, &r2));
    }

    #[test]
    fn a_reversed_read_asks_for_the_pages_behind() {
        let chans = vec![(0..PAGE * 8).map(|i| i as f32).collect::<Vec<_>>()];
        let f = Arc::new(StreamFile::from_decoded(&chans, 48_000, "rev").unwrap());
        let hub = StreamHub::new();
        let last = (PAGE * 8 - 1) as i64;
        let r = hub.reader((1, 1), &f, &[last], false);
        assert!(hub.wait_settled(Duration::from_secs(5)));
        let mut i = last;
        while i > PAGE as i64 {
            assert_eq!(r.frame(0, i), i as f32, "frame {i}");
            i -= 4096;
            assert!(hub.wait_settled(Duration::from_secs(5)));
        }
        assert_eq!(hub.underruns(), 0);
    }

    #[test]
    fn a_cue_loads_where_play_will_start() {
        let chans = vec![vec![0.75f32; PAGE * 10]];
        let f = Arc::new(StreamFile::from_decoded(&chans, 48_000, "cue").unwrap());
        let hub = StreamHub::new();
        let r = hub.reader((1, 1), &f, &[0], true);
        r.cue((PAGE * 4 + 9) as i64, LOOKAHEAD);
        assert!(hub.wait_settled(Duration::from_secs(5)));
        for k in 0..PAGE as i64 * 2 {
            assert_eq!(r.frame(0, (PAGE * 4) as i64 + k), 0.75);
        }
        assert_eq!(hub.underruns(), 0);
    }
}
