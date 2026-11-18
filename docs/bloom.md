# Bloom

`instrument.bloom` is a generative scene instrument: one press grows sixteen
percussive and tonal sounds at once, coherent with each other, and a visual
in which every sound is a cell whose colour and shape come from the sound
itself.

| Where | What |
|---|---|
| `crates/stock/src/instrument/bloom/genome.rs` | The 44 genes, `Genome`, `Role`, `Species`, `Scene` (the state blob) |
| `crates/stock/src/instrument/bloom/grow.rs` | Generation, roll, mutate, regrow, re-base, morph (pure, deterministic) |
| `crates/stock/src/instrument/bloom/voice.rs` | The Body and Resonator engines |
| `crates/stock/src/instrument/bloom/space.rs` | The shared ambience |
| `crates/stock/src/instrument/bloom/render.rs` | Offline renders: level normalisation, export, matching |
| `crates/stock/src/instrument/bloom/timbre.rs` | The measures the colours come from |
| `crates/stock/src/instrument/bloom/mod.rs` | The device: params, voices, meter, state |
| `crates/ai/src/generate/bloom.rs` | Matching a sample (evolution search over the embedding) |
| `crates/app/src/views/device_ui/bloom.rs` | The Device View card, dev views |
| `crates/app/src/views/device_window/bloom/` | The window, the field renderer, the look mapping, the DNA panel |

## The scene

Sound *i* plays on MIDI note `Base + i` (C1–D#2 by default, laid out like a
drum rack: C1 kick, D1 snare, F#1 closed hat, A#1 open hat…). *Chromatic*
mode plays the selected sound across the keyboard (C3 is its own pitch),
*Single* plays it on any key (that is how the pads of "To Drum Rack" work).
Velocity scales the level (Velocity parameter) and, by each sound's
`Vel > Tone` gene, its brightness and FM index.

## Engines

**Body**: two PolyBLEP oscillators (sine, triangle, saw, square; the second
at a ratio and detune), an exponential pitch sweep, a sine modulator on
oscillator 1's phase with its own index envelope (2-op FM), filtered noise
(low/band/high SVF) with its own envelope and up to four clap-style bursts,
drive, a filter (SVF low/band/high or the transistor ladder) with an
envelope, and an attack–hold–decay amplifier with a release. Decay genes
are times to −40 dB.

**Resonator**: up to 24 two-pole modes excited by the body. Five materials
(string, membrane, plate, free bar, an inharmonic set) and a continuous
morph between neighbours; decay, damping (high modes die faster), brightness
(tilt of the mode levels) and strike position (`|sin(π k pos)|`). Modes
alternate sides with the sound's width.

**Space**: six early reflections a side into two damped feedback combs,
shared by the scene; each sound sends by its gene, the Space macro sets the
return.

Every sound is normalised by a short render (24 kHz, 150 ms) so a random
scene comes out playable; its Level gene applies after that. Voices: 24,
oldest stolen with a 5 ms fade, at most two per sound in Kit mode; sounds in
the same choke group (the hats) cut each other while Choke is on. Every hit
of a sound uses the same noise seed and nothing reads a clock, so live and
export are identical.

## Generation

Pure functions of the seed (only arithmetic, `sqrt` and rounding, so a seed
grows the same scene on every machine; `det_log2`/`det_exp2` replace `ln`
and `exp`):

1. The **species** (Drum Kit, Tonal Perc, Metallic, Glitch, Mixed) picks the
   sixteen roles and a **character** shared by all sounds: a decay scale,
   grit, brightness, how much resonator, the resonator material, an FM ratio
   and a key for the tuned roles.
2. Each cell's **core** is its role's template shaped by the character.
3. Each cell has a **growth path** (its own seed: a random direction in
   normalised gene space, and the distances at which categorical genes such
   as the waveform switch) and a **distance** along it. The sound is the
   core moved that far, folded back into range at the ends.

`regrow` moves a cell along its path (dragging it in the visual). An edit (the
DNA panel, a match) **re-bases** the core so the edited sound lies on the same
path at the same distance. `roll` grows the next seed's scene and reaches it
from the current one by the Deviance (1 = all new); `mutate` nudges the
unlocked cores. Locked cells are left alone by both. `morph` slides amounts,
switches names halfway and rounds counts; Morph X/Y blends four corner
scenes bilinearly at each note.

The device plays the state's scene when it was grown from the Seed and
Species parameters, else the scene those parameters grow; that is what a
factory preset is (a seed, a species and the macros). Growing on a parameter
change happens on the audio thread without allocating (fixed-size genomes),
the new sounds' levels measured one per block.

## Sound → colour and shape

Each sound is rendered for 200 ms off the UI thread (`timbre::analyze`, cached
by genome hash; an estimate from the genes shows until it arrives) and
mapped by `look::look`:

| Measure | Becomes |
|---|---|
| Spectral centroid, log 60 Hz – 12 kHz | Hue: violet (300°) → red → amber → green → cyan (200°); the top fades toward white |
| Tonality (1 − flatness) | OkLCH chroma: tones saturated, noise greyish |
| Energy (RMS / peak over the 200 ms) | OkLCH lightness (a lower range on light themes) |
| Short decay, inharmonic partials | Spikes: more and sharper lobes for clicks and metal |
| Long tonal sounds | Round, soft blobs |
| FM index × decay, off-integer ratios | Wobble of the outline |

Colours are built in OkLCH and brought into the sRGB gamut by lowering the
chroma, so equal lightness looks equal across hues.

## The field

Sixteen cells orbit a core at a radius set by their distance. Each frame the
cells' kernels `(1 − q/4)³` (q = distance² over the radius² in that
direction; lobes from Chebyshev polynomials of the direction cosine, so no
`atan2` per pixel) are splatted inside their own bounding boxes into a coarse
grid (about one cell per three device pixels, at most 220 wide). The
iso-surface is shaded as a liquid (gradient normal, diffuse, a tight
specular, a rim light) with the cells' glows and transient rings around it,
uploaded as one premultiplied image and scaled up by the GPU. Cost: about
0.6 ms for 160 × 120 with seventeen blobs in a dev build
(`field_cost` test). Frames are requested only while something moves; with
reduced motion the cells sit still and do not swell.

Live reaction comes from the device meter: per-sound envelopes swell the
cells and light their glow, hit counters emit rings. The pointer pulls the
cells and parts them around it like a magnet under a ferrofluid; dragging a
cell regrows it.

## Matching a sample

Drop an audio file (or a library sound) on the field, or use "Match
sample…", to grow the selected cell towards it. The distance is the DSP
timbre embedding (`crates/ai/src/embedding.rs`, 64 values, gain-invariant):
candidates are rendered as long as the target (within 0.15–1.5 s, at
44.1 kHz) and described the same way. The search is a (μ + λ) evolution
strategy: it starts from the cores of every species (and the cell's own
sound), then breeds 32 children a generation from the 8 best (rank-biased
parents, uniform crossover, normal steps shrinking over 24 generations,
occasional switches of categorical genes), keeping the best. Candidates are
scored on worker threads; results do not depend on the thread count. The
cell grows with the progress and shows the best so far; the result fills
the cell (one undo step) and the next-best distinct results are offered as
neighbours. It never runs on the audio thread and stops when cancelled.

## Tools

- **To Drum Rack**: a new track with a Drum Rack of sixteen pads, each a
  Bloom in Single mode holding the scene (so a pad can be switched to
  another sound later), hats in one choke group.
- **Render samples**: sixteen 24-bit WAVs in `AI-Track/Bloom/<species> <seed>`
  of the music folder, added to the library.

## Dev views

`AITRACK_DEV_VIEW=bloom` (a Bloom on a new track, seeded scene, card
visible), `bloomwin` (its window open), `bloomplay` (synthetic activity:
some cells glow and ring).
