# Reverb + Delay (fx.reverb, fx.delay)

## Reverb

**Engines.** Two tanks, both designed for exact RT60:

- *FDN* (8/16 lines, orthonormal fast Walsh–Hadamard mixing; Jot & Chaigne 1991). Each line's loss is
  `g = 10^(−3·L/(fs·T60))` from its own loop length `L`, split into a mid gain and low/high shelf ratios
  (bilinear one-poles), so low / mid / high decay times hold exactly (Jot's absorption-filter design,
  Schlecht & Habets on FDN design). Tank input is normalised by `sqrt(1 − ⟨g²⟩)` so longer decays ring longer,
  not louder. Delay reads are modulated by quadrature phasors through first-order all-pass (Thiran)
  interpolation (flat magnitude, no extra HF loss in the loop).
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

- *Digital*: cubic Hermite reads, HP → LP → ADAA-tanh feedback (original behaviour, unchanged).
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
