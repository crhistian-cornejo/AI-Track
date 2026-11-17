# Reverb + Delay (fx.reverb, fx.delay)

## Reverb

**Engines.** Two tanks, both designed for exact RT60:

- *FDN* (8/16 lines, orthonormal fast Walsh–Hadamard mixing; Jot & Chaigne 1991). Each line's loss is
  `g = 10^(−3·L/(fs·T60))` from its own loop length `L`, split into a mid gain and low/high shelf ratios
  (bilinear one-poles), so low / mid / high decay times hold exactly (Jot's absorption-filter design,
  Schlecht & Habets on FDN design). Tank input is normalised by `sqrt(1 − ⟨g²⟩)` so longer decays ring longer,
  not louder. Delay reads are modulated by phasors (golden-ratio start phases, so the lines never sweep together)
  through an 8-tap Kaiser-windowed sinc (`dsp::delay::SincKernel`, cutoff at Nyquist: a whole-sample read is exact,
  flat to 12 kHz at any fraction). The first-order all-pass (Thiran) reader used before clicked each time its
  fraction wrapped: −52 dB at 500 Hz, −32 dB at 5 kHz of broadband residue against −100 / −85 dB now.
  The line lengths are stretched until the modes overlap 1.25× (a mode rings over 2.2/RT60 Hz; Σ delays modes
  per Hz), which stops short rooms ringing like pipes; six input diffusers per side, and a share of the diffused
  input joins the early reflections to bridge the gap before the shortest line answers.
- *Plate*: Dattorro's figure-eight tank (Dattorro 1997, "Effect Design Part 1"), lengths scaled from 29 761 Hz,
  published output taps; decay coefficient solved from the loop length.

**Algorithms** (`voicing.rs`) only re-voice the engines (line lengths, spread, early-pattern span/level, diffusion,
modulation, damping multiplier, late trim, shimmer floor): Classic (old Mode), Concert Hall, Bright Hall, Plate,
Room, Chamber, Ambience, Shimmer, Cloud.
**Colours**: Now (clean); 80s (11 kHz band limit on tank in/out, 14-bit wet words, 1.5× modulation);
70s (7 kHz, 12-bit, 8 sparse lines, 0.4× modulation, 0.7× diffusion) — the converter bandwidth, word
length and density of early digital units (Välimäki et al. 2012 history section).
**Shimmer**: late field → 250 Hz HP → two-grain delay-line pitch shifter (80 ms raised-cosine grains whose
windows sum to 1; Zölzer DAFX §6.4) → rational-tanh limiter → back into the tank input. Loop gain
≤ 0.72 so even 100 % on a 20 s decay dies away (tested). Intervals −12…+24 st.
**Ducking**: peak follower (5/250 ms) on the dry input scales the wet.
Ranges: Decay 0.1–20 s, Pre-delay 0–500 ms, Size 0–100 %, Damping 0–100 %, Shimmer 0–100 %.

## Delay

- *Digital*: 8-tap windowed-sinc reads (`SincKernel`; Hermite left −32 dB of images at 10 kHz when the time
  moved, sinc leaves −73 dB), HP → LP on the input *and* the feedback (so the first echo is already filtered; open
  filters step aside), ADAA-tanh feedback. Time changes use a slope-limited glide (`Glide`): ±0.5 samples of
  delay per sample (±0.35 and slower for tape) instead of an exponential that started a 250 → 500 ms jump at
  a slope of 2.08, i.e. the echo played backwards for a moment.
- *Tape* (`tape.rs`): wow (0.55 Hz sine + random drift, ≤ 5 ms) and flutter (6.3 + 9.7 Hz + 20 Hz-filtered
  jitter, ≤ 0.3 ms) move the read head (pitch deviation = dτ/dt). The record head colours input + feedback
  together: +6 dB HF pre-emphasis → asymmetric ADAA tanh (bias 0.15, even harmonics) → de-emphasis →
  gap-loss LP 16 kHz→2.5 kHz with Age; head bump (90 Hz bell) on the output, outside the loop.
- *BBD* (`bbd.rs`): Holters & Parker DAFx-18 structure. 4096 buckets clocked at `STAGES/delay`
  (≤ 8 ticks per audio sample → min ≈ 10.7 ms at 48 kHz). Input/output filters are 5th-order Butterworth
  low-passes (9 kHz→2.5 kHz with Age) in partial fractions (1 real + 2 conjugate sections). Input uses the
  exact zero-order-hold state evolution `s(τ) = e^{pτT}s + (e^{pτT}−1)/p·u`; the held BBD output is
  filtered as `H(0)·w + Re Σ (r/p)·s̃` with `s̃` jumping by Δ at each tick (step-response form), so DC gain is
  exactly 1 and long delays alias like real chips. `e^{pτT}` from a 64-point table per section.
- Shared: tempo sync (straight / dotted / triplet), multi-tap (2–4 taps at k/n of the main time, crossing
  sides, rising level), 4-stage all-pass diffusion in the loop, ducking, width, freeze (input muted,
  feedback → 1, loop filters/colour bypassed by crossfade).

## CPU (release, 48 kHz stereo, 128 frames, `tests/bench.rs`)
fx.reverb 1.31 % of a core at defaults; fx.delay 0.38 %. Per-module budget tests cover worst cases.

## Bibliography
- J. Dattorro, "Effect Design Part 1: Reverberator and Other Filters", JAES 45(9), 1997.
- J.-M. Jot, A. Chaigne, "Digital Delay Networks for Designing Artificial Reverberators", 90th AES Conv., 1991.
- S. J. Schlecht, E. A. P. Habets, "On Lossless Feedback Delay Networks", IEEE TSP 65(6), 2017; "Feedback Delay
  Networks: Echo Density and Mixing Time", IEEE/ACM TASLP 25(2), 2017.
- V. Välimäki, J. D. Parker, L. Savioja, J. O. Smith, J. S. Abel, "Fifty Years of Artificial Reverberation",
  IEEE TASLP 20(5), 2012.
- M. Holters, J. D. Parker, "A Combined Model for a Bucket Brigade Device and its Input and Output Filters",
  Proc. DAFx-18, Aveiro, 2018.
- U. Zölzer (ed.), *DAFX: Digital Audio Effects*, 2nd ed., Wiley 2011 (§6.4 pitch shifting; ch. 12 tape).
- W. Pirkle, *Designing Audio Effect Plugins in C++*, 2nd ed., Routledge 2019 (delay/reverb chapters).
- J. O. Smith, *Physical Audio Signal Processing* (CCRMA) — FDNs, all-pass interpolation.
- V. Zavalishin, *The Art of VA Filter Design* rev. 2.1 (TPT SVF / one-poles used throughout).
- M. Puckette, *The Theory and Technique of Electronic Music*, §7.9 (delay-line pitch shifting).

## Inspiration (ideas only, no code copied)
CloudSeed / CloudReverb (MIT) — the "Cloud" wash voicing idea; ChowMatrix (BSD-3) — BBD-per-tap concept;
Valhalla-style mode/colour split (commercial, concept only); Dragonfly reverbs (GPL-3, listened to only).
No code ported, so no NOTICE entries are needed.

## Measured quality (tests `reverb::quality`, `delay::quality`)

Run `cargo test -p aitrack-stock --lib reverb::quality::table -- --ignored --nocapture` for the table.

| Measure | Before | After |
|---|---|---|
| Spectral deviation of the tail (Gaussian = 5.6 dB): small room / drum room / chamber / plate | 6.85 / 7.11 / 6.01 / 6.28 dB | 5.8 / 5.9 / 5.9 / 6.0 dB |
| Strongest resonant peak over its surroundings (drum room / plate) | 16.6 / 15.4 dB | 13.5 / 13.7 dB |
| Normalised echo density at 50 ms, big hall / concert hall | 0.02 / 0.59 | 0.32 / 0.77 |
| Echo density at 200 ms, plate | 0.75 | 0.84 |
| Level drift of a frozen tail over one minute, plate / FDN | −8.5 / −2.2 dB | −0.1 / −1.0 dB |
| Modulation coherence of the 16 lines | 1.0 | < 0.2 |
| Images of a 10 kHz tone with the delay time moving | −32 dB | −75 dB (plugin), −73 dB (read) |
| Largest delay-time slope on a big jump | 2.08 | 0.5 (0.35 tape) |
| DC offset of the tape echo on a loud sine | 13 % of the rms | < 1 % |

