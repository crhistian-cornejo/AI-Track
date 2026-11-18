# Bloom

`instrument.bloom` is a generative scene instrument: one press grows sixteen
sounds at once, coherent with each other, and a visual in which every sound
is a cell whose colour and shape come from the sound itself. The **Kind**
picks what grows: a drum scene (the original engine) or sixteen patches of a
tonal kind (Bass, Lead, Pad, Pluck, Keys, FX, Texture) played by an
embedded Wavetable Synth (see [Tonal kinds](#tonal-kinds)).

| Where | What |
|---|---|
| `crates/stock/src/instrument/bloom/genome.rs` | The 44 genes, `Genome`, `Role`, `Species`, `Scene` (the state blob) |
| `crates/stock/src/instrument/bloom/grow.rs` | Generation, roll, mutate, regrow, re-base, morph (pure, deterministic) |
| `crates/stock/src/instrument/bloom/voice.rs` | The Body and Resonator engines |
| `crates/stock/src/instrument/bloom/space.rs` | The shared ambience |
| `crates/stock/src/instrument/bloom/render.rs` | Offline renders: level normalisation, export, matching |
| `crates/stock/src/instrument/bloom/timbre.rs` | The measures the colours come from |
| `crates/stock/src/instrument/bloom/patch.rs` | `Kind`, `Patch` (a full Wavetable Synth parameter set, fixed-size), `PatchScene` (the tonal state blob) |
| `crates/stock/src/instrument/bloom/tonal.rs` | Tonal growth: priors from the factory presets, rules per kind, paths, roll/mutate/regrow, names, the knobs' shaping, level measurement |
| `crates/stock/src/instrument/bloom/mod.rs` | The device: params, voices, the embedded synth, the output stage, meter, state, offline renders of patches |
| `crates/stock/examples/bloom_kinds.rs` | Offline audition: per kind, a WAV of sixteen patches playing a phrase, a spectrogram PNG, a stats table |
| `crates/ai/src/generate/bloom.rs` | Matching a sample (evolution search over the embedding) |
| `crates/app/src/views/device_ui/bloom.rs` | The Device View card, dev views |
| `crates/app/src/views/device_window/bloom/` | The window (`layout.rs`), the GPU stage (`stage.rs`, `matter/`), the CPU field fallback, the look mapping, the DNA panel |

## Parameters

| Id | Name | Range (default) | Drums | Tonal kinds |
|---|---|---|---|---|
| 0 | Seed | 0–999 999 (1337) | the scene's seed | the scene's seed |
| 1 | Species | Drum Kit, Tonal Perc, Metallic, Glitch, Mixed | the drum species | — |
| 2 | Deviance | 0–100 % (100) | how far Roll / Mutate move (editor) | same |
| 3–6 | Decay, Body, Tone, Grit | −100–100 % (0) | per-voice macros | amp / mod / env-3 decay and release ×0.35–2.8; sub and unison width; both cutoffs ±2 oct (and the EQ's top); distortion and filter drive |
| 7 | Space | 0–100 % (25) | ambience return | ambience return, and the patch's reverb ×(0.4 + 2.4·Space) |
| 8, 9 | Morph X / Y | 0–1 | corner scenes | — (not yet) |
| 10 | Selected Sound | 0–15 | the sound of Chromatic / Single | the patch that plays |
| 11 | Play Mode | Kit, Chromatic, Single | as below | Kit = Chromatic; Single = key 60 for any key |
| 12, 13 | Base Note, Choke | | kit layout, hat choke | — |
| 14 | Volume | −48–6 dB (0) | output | output |
| 15 | Velocity | 0–100 % (80) | level by velocity | scales the patch's velocity sensitivity (×1 at 80 %) |
| 16 | Tune | ±24 st | transpose | per-note tuning (an expression) |
| 17 | Kind | Drums, Bass, Lead, Pad, Pluck, Keys, FX, Texture (+4 "—") | | |
| 18 | Attack | −100–100 % (0) | AHD attack ×⅛–×8 (+20 ms at the top) | amp and mod env attack, the same scale |
| 19 | Drop | −100–100 % (0) | pitch sweep (−100 none, +100 two more octaves) | a pitch envelope at the note start, up to 12 st from above (+) or below (−), 60–250 ms (env 3, or a one-shot LFO when env 3 is taken) |
| 20 | Motion | 0–200 % (100) | FM index | every LFO and envelope route of the matrices |
| 21 | Width | 0–200 % (100) | output mid/side width | same |
| 22 | Drive | 0–100 % (0) | output saturation, level-compensated | same |
| 23 | Filter | −100–100 % (0) | output DJ filter: − low-pass down to 26 Hz, + high-pass up to 15.5 kHz, resonance rising with the amount, bypassed at 0 | same |
| 24 | Vary | 0–100 % (0) | per hit: ±0.6 st, ±0.7 oct of filter, ±25 % level | per note: ±0.3 st, ±25 % velocity |
| 25 | Glide | 0–1000 ms (0) | — | at least this much glide |
| 26 | React | 0–200 % (100) | the visual's reaction (the audio ignores it) | same |

Every default leaves the sound exactly as it was before these parameters
existed (tested). Width, Drive and Filter are smoothed (20 ms) on the
output; Attack, Drop, Motion and Vary apply when a note starts, so
automating them never clicks. Vary is seeded by the sound, the key and the
song position (a hit counter while the transport stops): live and export
agree.

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

## Tonal kinds

A tonal scene is sixteen **patches** of the Wavetable Synth
(`instrument.wavetable`). A `Patch` is every synth parameter, normalised,
in a fixed-size array (no heap), stored sparsely by parameter id;
`extra.patches` holds the `PatchScene` (seed, kind, patches, cores, paths,
distances, locks). Only the **selected** patch plays: the device embeds one
`WavetableSynth` (allocated at creation) and sends it the selected patch,
shaped by the knobs, whenever something changes (only the values that
changed; no allocation). Kit and Chromatic both play it across the keyboard
(key 60 is the patch's home pitch), Single plays it at key 60 for any key.
The synth's output (after an 8 Hz DC blocker and the patch's level) joins
the drum bus: the shared ambience, Volume and the output stage.

### Growth

1. **Priors** (`tonal::priors`, built once at device creation): per kind,
   the hand-made `instrument.wavetable` factory presets in `tonal::PARENTS`
   (Bass: the 14 basses + Fold Bass, Ladder Reese, Talking Growl; Lead: 12
   + Sync Sweep, Bit Glitch, Vox; Pad: 9 + Harmonic Bloom Pad; Pluck: 8 +
   Trance / House / Bubble Arp + Comb Metal Pluck; Keys: 7 + four Chords +
   PD Keys, Asym Comb Keys, FM Glass Bell; FX: 6; Texture: five pads, Wind
   Atmosphere, Spectral Mist, Harmonic Bloom Pad) and the range each
   parameter spans across them. The list is pinned by name: editing it or
   those presets changes what every tonal seed grows.
2. **Character** (per scene): a lineage parent most cells descend from, a
   wave family from the kind's factory tables, brightness (±0.7 oct),
   unison width, grit, movement, a time scale, and a shared room (reverb,
   delay, chorus) every cell sits in.
3. **Core** (per cell): two parents crossed by module blocks (oscillators;
   filters with the mod envelope that drives them; amp; motion = LFOs, both
   matrices, env 3; voicing), the room, the kind's **design moves** (a bank
   wavetable, a Reese pair, a ladder filter, a vibrato, a pad's slow LFO on
   the table position, a pluck's filter envelope, keys' velocity → cutoff,
   an FX archetype: riser, downer, impact, zap, siren, sweep), the scene's
   colour, a small mutation, then the **rules**.
4. **Path and distance**, as for drums: a fixed direction over ~100
   explorable values (positions, levels, unison, cutoff, resonance, drive,
   envelopes, LFO rates, route depths — pitch and amp routes move a tenth as
   far — and the FX), folded into the kind's band, plus switches of the
   wavetable, the filter model and LFO 1's shape at seeded distances. Every
   grown sound is held to the rules again.

**Rules** (`tonal::rules`, `tonal::constrain`), per kind: Bass — mono or
legato, oscillator 1 at −1/0 oct, a sub at −1 oct (24–60 %), low-pass
140–2600 Hz, attack ≤ 6 ms, short release, almost no reverb; Lead — attack
≤ 30 ms, sustain ≥ 62 %, cutoff 2.2–13 kHz, a harmonic-rich oscillator
(a plain sine or triangle becomes the sine→saw table), glide ≤ 160 ms,
vibrato ≤ 2 %; Pad — poly, attack 180–2200 ms, release 1.3–4.5 s, ≥ 3
unison voices detuned 12–42 % and spread wide, a slow movement route
always, reverb 25–55 %; Pluck — no sustain, decay 120–650 ms, a filter
envelope (28–80 %, mod env decay 60–420 ms) over a 250–3000 Hz cutoff;
Keys — velocity sensitivity ≥ 65 %, velocity → cutoff (lowering a
high-pass instead), attacks ≤ 8 / 12 ms, decay 0.35–2.4 s, sustain ≤ 45 %,
≤ 2 unison voices; FX — wide ranges, decays ≥ 280 ms, synced sweeps
within a bar, always movement (a pitch or filter route) or noise, and only
the parents' timbre crosses over (their envelopes and motion are the
archetype); Texture — noise 15–50 % (white or pink), slow attack and long
release, a slow route always, a big room. Everywhere: enough source level,
no high-pass where the body matters, pitch and amp modulation capped per
kind (free for FX).

Growth, `roll` (Deviance), `mutate`, `regrow`, `replace` and locks work like
the drum scene's. Morph X/Y does not morph tonal scenes in this version.
Names (`tonal::patch_name`, `scene_names`): an adjective from what the
patch is (dark, bright, wide, gritty, moving, noisy, soft) and a noun of its
kind ("Velvet Reese", "Glass Choir"), picked by its fingerprint.

### Real-time safety and determinism

A tonal scene grows on the audio thread when Seed or Kind is automated, like
drums: pure arithmetic over the static priors (`generate_into`, in place;
only `sqrt` and rounding, no `ln`/`exp`, so a seed grows the same patches on
every machine). Levels are measured by `tonal::Leveler`, a second synth that
renders the patch's home note (attack shortened; 360 ms, or 1.2 s for pads,
textures and FX; at 24 kHz) **1440 frames per audio block**, the selected
patch first; the loudest 50 ms window is brought to the kind's RMS target
unless its peak would pass a ceiling (chord kinds sit lower). A loaded
state or a new device measures everything at once. A measurement restarts
the synth's random streams (`WavetableSynth::restart`), so it gives the
same gain on the audio thread and off it. The embedded synth runs over the
whole block (Bloom's parameter changes reach it at the block start, notes
at their sample): two renders with the same blocks are identical; renders
with other block sizes differ only by the synth's control-rate grid.

### API for the editor

- `patch::Kind`, `KIND_NAMES`, `Kind::TONAL`; `p::KIND` (17) and the
  performance ids 18–26 (table above).
- `tonal::generate(seed, kind)`, `generate_into`, `roll(prev, seed, kind,
  deviance)`, `mutate(scene, seed, amount)`, `regrow(scene, i, distance)`,
  `replace(scene, i, patch)`, `PatchScene::set_locked`.
- `tonal::patch_name(patch)`, `tonal::scene_names(scene)`.
- `Patch::to_preset(name, gain)` and `bloom::patch_preset(patch, knobs,
  name)`: an `instrument.wavetable` preset (knobs baked in, Bloom's level in
  its Volume) to open the sound in the full Wavetable Synth.
- `bloom::render_patch(patch, knobs, tune, sr)`: the kind's demo note
  (`demo_note`) through a device (`Bloom::with_patch`), for export and drag.
- `bloom::patch_timbre(patch, knobs, tune)`: the `Timbre` of the first
  750 ms of that render (the patch's modulation as the wobble), for the
  visual's sound → look mapping.
- `tonal::Knobs` (the device's knobs as one struct), `tonal::shape_knobs`.
- State: `state_json_full(params, scene, corners, visual, patches)`,
  `parse_patches(data)`, `effective_patches(stored, seed, kind)`;
  `Bloom::kind()`, `patches()`, `patch_gains()`. The editor's existing
  `state_json_with` writes no `extra.patches` (the device then regrows the
  same scene from Seed and Kind; only edits and locks would be lost).

Audition offline (no window):
`cargo run -p aitrack-stock --release --example bloom_kinds -- <out_dir> [seed]`
writes `<kind>.wav` and `<kind>.png` (spectrogram, 30 Hz–16 kHz log, patch
boundaries marked) per kind and prints each patch's stats.

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

## The matter (the stage)

The window and the card show the scene as one liquid mass, raymarched in a
WGSL shader on the GPU (`bloom/matter/`): glossy black ferrofluid by default,
its colour and shape driven by the sound playing.

| Piece | What |
|---|---|
| `matter/matter.wgsl` | The shader: SDF of six warped balls melted together (smooth-min), quintic value-noise folds and creases, an FM standing wave, ferrofluid spikes on a spherical Fibonacci lattice (cones grown on top and towards the pointer), sphere tracing (≤ 80 steps, bounding sphere, a spike-free bound for long steps), a procedural studio (softboxes, strips, a floor that picks up the panel), Schlick Fresnel, a GGX clearcoat hotspot, thin-film iridescence, a rim, occlusion, a 2.5D vapour plume, a contact shadow, analytic edge AA. The noise lattice is a 32³ volume read with exact loads (hardware filtering grains the highlights). |
| `matter/material.rs` | Obsidian, Mercury, Oil Slick, Pearl, Ember (molten veins), Glass, Ink: uniform sets. Stored by key in the state (`extra.visual`), one undo step (`SetDeviceState`); the sound never depends on it. |
| `matter/mapping.rs` | A timbre → its look (below). |
| `matter/frame.rs` | The motion: the playing sound takes the stage (its look blends in on a hit and decays back to the selected sound), spikes rise at once and sink in 0.3 s, a hit shakes and puffs the vapour, the pointer attracts from outside the mass and pushes in over it. A few µs a frame. |
| `matter/gpu.rs`, `service.rs` | One wgpu device (Metal / DX12) for every Bloom, created lazily on the `bloom-matter` thread. The UI posts the frame's uniforms and paints the newest finished frame (one frame behind, never waiting). |
| `matter/surface.rs` | macOS: frames are NV12 IOSurface `CVPixelBuffer`s that wgpu renders into through Metal textures made from the IOSurface planes, shown with `Window::paint_surface` (zero-copy; three rotate). Elsewhere frames are read back as BGRA and uploaded with `paint_image`. |
| `stage.rs` | The GPUI element: the frame, its pacing and resolution, the gestures. |
| `pads.rs` | The sixteen sounds as pads under the window's stage (colour, number, role, lit while playing). |
| `knobs.rs` | Every knob in one table (parameter, window group, one-line tooltip); the card shows a prefix. |

Several sounds show at once: up to four playing sounds (`frame::Voice`)
each tint a patch of the film and the rim around their own place on the mass
(a golden-angle spiral over the side facing the viewer) while they sound; a
hit sends a ripple ring across its patch and throws a droplet in the sound's
colour that spins, falls back and melts in. Behind the mass a glow in the
playing sounds' colours (the selected sound's, faintly, at rest) swells with
the output level: added light on dark panels, a dyed wash on pale ones. The
vapour takes their colours and fades out before the panel's edges.

Pacing (`stage::pace`, tested as a pure function): every display frame
while the device sounds (an envelope or the output above 1 %, or a hit
counter moving), while the transport plays with hits up to 4 s apart, for
1.5 s after the sound stops, and while the pointer or a drag moves it; about
12 frames a second (the idle breathing) otherwise. A stage that is clipped
away, scrolled off or in an occluded window draws nothing. Reduced motion is
one still frame; low power draws at the breathing rate while it sounds and
not at all when silent.

Resolution: a moving frame gets as many device pixels as the GPU affords,
native when it can: the budget (start 0.9 MP, never under 0.3 MP, more
than twice the old 512 px cap) shrinks while the render thread's frame
costs more than 4 ms and grows back while it costs less, in steps of 1/8 of
the scale so buffers rarely change size; the idle breathing and a still
frame are native. Without a GPU adapter
the CPU field below is drawn instead.

| Measure | Becomes (per sound, on top of the material) |
|---|---|
| Spectral centroid | Thin-film shift (low sounds thick magenta–green films, bright ones thin gold–blue) and the vapour's hue |
| Noisiness, brightness, decay | The vapour's three colours (pale holographic bands around the sound's hue, greyer for noise, deeper on light themes) and its amount (noise and long decays smoke more) |
| Transient speed, inharmonicity | Spike sharpness (cone → needle) and count (36 → 150) |
| Noisiness, FM | Roughness; wobble frequency and depth |
| Energy, decay | Size and how far a hit swells it |
| Live meter (envelope, pulse, centroid, noisiness) | The playing sound's look leans to what it sounds like now; pulses spike, shake and puff |

The knobs reach the matter twice. The timbre is measured as the device plays
it (`timbre::analyze_shaped`: its Tune and the Decay / Body / Tone / Grit
macros, rounded to 4 % steps), so a turned knob is re-analysed on the worker;
meanwhile the sound's newest analysis stands in, so a drag never flickers. On
top, every frame, `mapping::with_controls` moves the look straight away (the
identity at the device's defaults):

| Knob | The matter |
|---|---|
| Decay | Slower, heavier churn and a lingering vapour; hits swell longer |
| Body | A bigger, lumpier mass |
| Tone | The sheen moves towards gold–blue, spikes sharpen |
| Grit | Rougher, denser spikes, a choppier vapour, more tremble |
| Space | More vapour, slower and more turbulent |
| Deviance | The folds and the wobble wander as far as the sounds do |
| Tune | Finer folds and spikes, faster wobble |
| Volume | Louder hits swell further |

Presets are parameter sets, so loading one moves the matter the same way;
Seed, Species and Morph X/Y change the sounds themselves.

Offline preview (headless, no window):
`cargo run -p aitrack-app --release --example bloom_matter_render -- <out_dir>`
writes every material at rest and on a hit, six sounds' looks, light and dark
panels, a 30-frame strip and `timing.txt`. `MATTER_WGSL=<file>` renders with
another copy of the shader, to tune it without rebuilding. A second argument
`knobs` writes only `knobs-{light,dark}.png`: one sound under each knob.

## The field (CPU fallback)

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
- **Export sound** (the chip under the macros): the selected sound as one
  WAV in the same folder; the chip then drags it onto a track or a pad as a
  sample (inside the app: GPUI has no drag out to the OS).
- **Matter**: the material picker in the top bar (previous, the menu, next).

## Dev views

`AITRACK_DEV_VIEW=bloom` (a Bloom on a new track, seeded scene, card
visible), `bloomwin` (its window open), `bloomplay` (synthetic activity:
some cells glow and ring).
