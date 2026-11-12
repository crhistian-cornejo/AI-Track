# 10 — Wavetable synth (`instrument.wavetable`)

Research notes and the design of AI-Track's wavetable synth: architecture ideas
from open synths, what we built, the public API for the editor UI, and free
wavetable content.

> **License rule.** Vital, Surge XT, Dexed, Helm, ZynAddSubFX, Odin 2 and
> Vaporizer2 are GPL-3 (Dexed and Surge are GPL-3; Helm GPL-3; Zyn GPL-2+;
> Odin 2 GPL-3; Vaporizer2 GPL-3). They were read for **ideas only**: no code
> was copied or translated. Everything in `crates/stock/src/instrument/wavetable/`
> is our own implementation.

## 1. What the reference synths do (ideas, not code)

| Synth | Idea we took | Our version |
|---|---|---|
| **Vital** | Wavetables as frames of 2048 samples; per-frame FFT and pre-computed band-limited copies; "spectral morph" between frames; modulation drawn as rings around knobs; drag a source onto any knob; LFOs as editable breakpoint curves with per-segment power curves; macros as plain sources. | `MipTable` (per-frame FFT → one band-limited level per octave), `Morph::Spectral` in the editor, matrix whose destination is *any parameter id*, `LfoCurve` with per-point curve, 4 macros as matrix sources. |
| **Serum** (closed, studied through its file format) | `clm ` RIFF chunk `<!>2048 …` declaring the frame size; up to 256 frames; warp modes (sync, bend, PWM, mirror, quantize, FM from another osc). | `wav::import` reads `clm `; warp list `WARPS`. |
| **Surge XT** | `srge` chunk (version + frame size) for its wavetables; oscillator "drift"; serial/parallel filter configurations with per-osc routing; many filter models behind one "type". | `srge` read in `wav::declared_frame_len`; `Drift`; per-osc `ROUTE` + `FILTER_ROUTING`; filter `MODEL` param. |
| **Helm** | Small, clear voice architecture; step sequencer as a mod source; mono/legato/glide handling. | Voice handling kept from our original synth (stealing with a short fade). |
| **ZynAddSubFX** | Additive / PADsynth thinking: build waves from harmonic amplitudes, then IFFT. | `WaveFrames::from_harmonics`, `set_harmonics`, formant tables in the factory bank. |
| **Dexed** (DX7) | FM as phase modulation between operators; bell/e-piano spectra from index sweeps. | `FM ← Next / Prev` warps (phase modulation by the neighbour oscillator), "FM Bell" factory table. |
| **Odin 2** | Formant and comb filters, ladder model; three oscillators + two filters with routing. | `filter::MODELS`: SVF, Ladder, Notch, Comb ±, Formant; 3 oscillators, 2 filters. |
| **Vaporizer2** | Big mod matrix with "via" (a second source scaling the depth), many sources. | `M_VIA` field per slot. |

Other notes from the reading:

* **Aliasing.** The two classic answers are mipmaps (one band-limited copy per
  octave, pick the copy whose top harmonic stays below Nyquist) and
  oversampling. Mipmaps cost memory, not CPU, and are exact for the table
  itself; warps that create discontinuities (sync, quantize) still alias, as
  in every synth that does them without oversampling. We pick a thinner level
  for warps that speed the read up (`warp_speed`), which removes most of it.
* **Interpolation.** With 4× oversampled levels, 4-point Hermite keeps
  interpolation images about −75 dB at the worst case; thin levels are stored
  at ≥ 256 samples (much more oversampled), which is what pushes the high
  notes past −70 dB.
* **Modulation in normalised space** (Vital, Bitwig) makes "any parameter" a
  single code path: amount 100 % = the whole knob travel, whatever the unit
  or curve. Pitch, amp and pan are better as *virtual* destinations with
  musical units.
* **Real-time hand-over** of big objects (tables): build off the audio thread,
  pass a pointer through a lock-free slot, and give the old one back to be
  freed off-thread. We do this with `table::Mailbox` (two `AtomicPtr`s).

## 2. What `instrument.wavetable` is now

Module: `crates/stock/src/instrument/wavetable/`

| File | Contents |
|---|---|
| `mod.rs` | The processor (`WavetableSynth`), voices, rendering, state, `SynthRemote`. |
| `params.rs` | Parameter ids (`p`), virtual destinations (`vdst`), choice lists, `waves()`, `dest_name`. |
| `table.rs` | `MipTable` (band-limited mipmapped tables), `Mailbox` (RT hand-over). |
| `tablegen.rs` | Non-RT editor: `WaveFrames`, draw, additive, formula, morph, normalize, factory bank. |
| `wav.rs` | Import (Serum/Vital `clm `, Surge `srge`, single cycles, raw banks) and export. |
| `modulation.rs` | Sources, `ModSlot` matrix model, `Polarity`, `LfoCurve`, `CurveEnv` (AHDSR with curves). |
| `filter.rs` | `VoiceFilter`: SVF / Ladder / Notch / Comb ± / Formant. |
| `tests.rs` | Synth tests (pitch, voices, matrix, aliasing, warps, routing, import, hand-over, state). |

Feature list:

* **3 wavetable oscillators**: table (6 original + 14 bank + user), position
  morph, warp (Off, Sync, Bend +, Bend −, Asym/PWM, Mirror, Quantize, Remap,
  FM ← Next, FM ← Prev, RM ← Next) with amount, unison 1–16 with detune,
  spread and blend, start phase and random phase, octave / semi / fine, level,
  pan, filter route (F1 / F2 / Both / Direct).
* **Sub** (sine/square, −1/−2 oct) and **noise** (white, pink, brown, blue,
  violet, crackle), each with route and pan.
* **2 filters**, models SVF (LP/HP/BP 12/24 dB), Ladder, Notch, Comb +,
  Comb −, Formant; cutoff, resonance, drive, key tracking, Env 2 depth;
  enable; serial or parallel.
* **3 envelopes** (AHDSR, attack/decay/release curves −100..100 %). Env 1 is
  the amp; Env 2 drives the filters' `Env` knobs.
* **4 LFOs**: 7 classic shapes or a drawn curve; free Hz or tempo division;
  free-running (global), retrigger (per voice) or one-shot (per voice, holds
  the end); phase offset; fade-in.
* **Matrix**: 32 slots × (source, destination, amount, polarity, via) plus
  the original 8 slots. 20 sources, any parameter or 10 virtual
  destinations. 4 macro knobs.
* **FX**: Distortion (soft, hard, fold, tube, bitcrush) → 3-band EQ →
  chorus (rate, depth) → ping-pong delay (synced) → FDN reverb (decay, tone)
  → volume.
* **Voices**: 16, poly/mono/legato, glide, pitch-bend range, voice stealing
  (oldest released first, 64-sample fade), sustain pedal (CC 64), MPE tuning
  and pressure per note, drift.
* **Real time**: no allocation, lock or I/O in `process` (checked by
  `tests/alloc.rs`, including a live table swap). Coefficients and
  modulation at control rate (32 samples).

### Compatibility

Ids 0–84 keep their meaning, range and names; new parameters default to
neutral (osc 3 at level 0, filter 2 off, empty matrix, FX extras off), so old
projects and presets sound the same. Two "0 = original" switches:
`osc VOICES` (0 → the 1–7 `UNISON` knob) and `osc WAVE` (0 → the six-entry
`TABLE` choice). The original matrix slots are translated into the new
routing with their old depths (cutoff 72 st, LFO rate ±4 oct, pitch 24 st).
Envelopes now use our curved AHDSR; the default curves (70 %) match the old
exponential decay/release closely.

## 3. Public API for the editor UI

All in `aitrack_stock::instrument::wavetable`.

**Parameters.** `params()` lists everything. Block helpers give ids:
`p::osc(n, field)` (n 0..3, fields `TABLE … WAVE`), `p::filt(n, field)`,
`p::env(n, field)`, `p::lfo(n, field)`, `p::slot_x(s, field)` (matrix),
`p::macro_knob(m)`, and globals (`p::FILTER_ROUTING`, `p::DIST_MODE`, …).
Choice names: `waves()`, `TABLES`, `WARPS`, `ROUTES`, `NOISE_COLORS`,
`filter::MODELS`, `modulation::MODES`.

Stepped lists keep a wider range than their names (`MODEL` 0..15, `WARP`
0..15, `WAVE` 0..63, matrix source 0..63 and destination 0..1023) so
normalised values stay stable when entries are appended.

**Matrix (drag and drop).**

```rust
use aitrack_stock::instrument::wavetable::{modulation::ModSlot, src, p};
let slots = ModSlot::read_all(|id| dev.plain(id));          // 32 slots
// Drop LFO 1 on the Osc 1 position knob at +40 %:
if let Some((slot, edits)) = ModSlot::assign(&slots, src::LFO1, p::osc(0, p::POS), 0.4) {
    for (id, plain) in edits { /* SetParam command */ }
}
let rings = ModSlot::targeting(&slots, p::CUTOFF);           // slots on a knob
```

`modulation::is_modulatable(info)` filters destination candidates;
`VIRTUAL_DESTS` / `dest_name(id)` name them; `modulation::SOURCES` names
sources (`source_is_bipolar`, `source_is_per_voice` for colouring).

**Wavetables.**

* Display: `factory_frames(wave)` / `legacy_frames(table)` give raw
  `WaveFrames` (frames of 2048 samples) to draw; `MipTable::preview`.
* Edit (`tablegen`): `draw(&[(x, y)])`, `WaveFrames::from_harmonics`,
  `set_harmonics` / `harmonics` (additive editor), `from_formula("sin(2*pi*x)
  * (1-y) + saw(x)*y")` (variables `x` phase, `y` frame), `morph(count,
  Morph::Spectral | Crossfade)`, `normalize`, `remove_dc`, insert / remove /
  set frames, `build()`.
* Files (`wav`): `import`, `import_file`, `import_with(.., Some(256))`,
  `export` (32-bit float + `clm `).
* Getting it into the synth: `state_with(params, tables, curves)` writes a
  device state (tables stored as base64 16-bit PCM) for a `SetDeviceState`
  command; set the oscillator's `WAVE` to `USER_WAVE` (63). On a live synth
  object: `set_user_table` (not running) or `remote().post_user_table` (running;
  call `remote().collect()` later to free retired tables).

**LFO curves.** `modulation::LfoCurve::new(&[CurvePoint { x, y, curve }])`,
`presets()`, `eval(x)`, JSON via `to_json` / `from_json`; set `LFO_DRAWN` on.
`set_lfo_curve` / `remote().post_lfo_curve` / `state_with`.

**Meter** (`Processor::meter`): `[voices, peak L, peak R, pos 1..3, LFO 1..4,
Env 1..3]` of the newest voice, for animated playheads.

**Gap for the coordinator.** The engine's processor pool calls `load_state`
only when it creates a processor. A `SetDeviceState` on a device that is
already playing does not reach it, so a newly imported table is not heard
until the device is recreated. Options: recreate the processor when the
device state changes, or forward state to a control-thread hook. `SynthRemote`
is ready for the second option, so the audio thread never builds or frees a
table.

## 4. Free wavetable content

Added to `content/soundbank/manifest.toml` (bundled, verified pipeline;
`cargo xtask soundbank fetch --only <id>`):

| Pack | License | Evidence | Size |
|---|---|---|---|
| `akwf-single-cycles`: AKWF selection (Kristoffer Ekstrand) | CC0-1.0 | `LICENSE.md` in the repo at the pinned commit: "CC0 1.0 Universal"; GitHub detects CC0-1.0 | 989 single cycles (600 samples), 1.3 MB |
| `waveedit-online-banks`: WaveEdit Online banks (community, mirrored by smpldsnds) | CC0-1.0 | README at the pinned commit: "All wavetables here are under CC0 1.0 Universal" | 705 banks of 64 × 256, 23 MB |

From AKWF we only take the folders the author synthesized (basic shapes, FM,
hand drawn, linear, overtone, sine harmonics, symmetric, chip, bit-reduced,
pluck). The folders named after other products (Akai MPC, u-he Zebra, Surge,
Alchemy, Reaktor…) and the sampled-instrument folders are left out, because
those sounds may come from third parties.

The AKWF repo is too large for GitHub's recursive tree listing, which comes
back truncated, so `xtask soundbank` now walks only the selected
sub-directories when that happens (`walk_tree`).

Looked at and **not** bundled:

* Kimura Taro Free Wavetables (340 × 2048): free download, but no license is
  stated on the download page. The open-vital-resources repo calls them CC0,
  but that is second-hand evidence and those copies are `.vitaltable`, not WAV.
  This could go in the hub as a link if we confirm the license with the author.
* KRC (Kenneth Crosley) free wavetables, Ocean Swift, Mayer EMI FM pack: these
  are "royalty-free for your music" licenses, which don't allow
  redistribution. They could only go in the hub as links.
* Surge XT and Vital factory tables: these ship inside GPL projects, so they
  are not bundled.

## 5. Tests

`cargo test -p aitrack-stock`:

* `table::tests`: the saw is alias-free from 440 Hz to 8.4 kHz (< −70 dB),
  naive reading does alias (sanity check on the measurement), morphing,
  normalisation, short cycles, the level choice, and the mailbox frees nothing
  on the RT side.
* `tests::synth_output_is_alias_free_at_high_notes`: whole-synth output at
  C7/C8 (< −65 dB) and on the densest bank tables (< −60 dB).
* Oscillator pitch, osc 3, sub, drift, unison at 16 voices, every warp,
  position morph, filter models, serial/parallel/direct routing, noise
  colours, the original matrix (808 drop, synced wobble), the new matrix
  (macro → cutoff, virtual pitch, via, bipolar), the matrix API round trip,
  drawn and one-shot LFOs, env 3, sustain pedal, FX, defaults, the meter.
* `wav::tests`: a synthetic `clm ` WAV with 1024-sample frames, the 2048
  default, AKWF-style 600-sample cycles, a Surge `srge` chunk, a WaveEdit bank
  with an explicit length, and an export round trip.
* `tests/alloc.rs`: no heap traffic in `process` for any patch, including a
  live table and curve swap.
* `tests/presets.rs`: 81 wavetable factory presets (13 in the new
  "Sound Design" category) load and render finite audio.
