# Stock DSP Additions: Acid 303, Kick 808, Shaper Modes, and SFZ v2 Streaming

## New Stock Instruments

### `instrument.acid303` (`crates/stock/src/instrument/acid303.rs`)
- **Topology**: `BlepOsc` (Saw / hollow Square) + optional sub-octave (`SUB`) -> 2x-oversampled 18 dB/oct non-linear diode-ladder low-pass filter (`Diode18`) -> diode overdrive + VCA.
- **Play modes (`SEQ_MODE`)**:
  - `MIDI`: Monophonic last-note priority stack with overlapping-legato slide and velocity >= 0.78 accent triggering.
  - `Pattern`: Transport-synced 16-step sequencer stored directly in undoable step parameters (`STEP0..`, 4 parameters per step: `NOTE`, `GATE`, `S_ACCENT`, `S_SLIDE`). Held MIDI keys transpose the pattern relative to C2 (MIDI 36).
- **UI**: `crates/app/src/views/device_ui/acid303.rs` — interactive diode cutoff/resonance plot with live envelope sweep indicator plus clickable/scrollable 16-step pattern strip.
- **Factory presets**: 6 presets under `crates/stock/presets/instrument.acid303/`.

### `instrument.kick808` (`crates/stock/src/instrument/kick808.rs`)
- **Topology**: Phase-continuous sine core + exponential pitch punch sweep (`SWEEP`, `SWEEP_DECAY`) + damped click beater (`CLICK`) + asymmetric soft-clip saturation (`DRIVE`) + 1-pole LP tone control (`TONE`).
- **Play modes**: `Fixed` vs `Chromatic` `KEY_TRACK` with monophonic `GLIDE`, plus `One-Shot` vs `Gate` release behaviour.
- **UI**: `crates/app/src/views/device_ui/kick808.rs` — amplitude decay + pitch-sweep contour visualizer with live readout and chromatic/glide controls.
- **Factory presets**: 6 presets under `crates/stock/presets/instrument.kick808/`.

## Extended Stock Effect & Sampler

### `fx.shaper` (`crates/stock/src/fx/shaper.rs`)
- Preserves existing parameter IDs `0..=14` (`Volume` + `Sync` default).
- Adds `MODE` (`Volume`, `Filter LP` via stereo `SvfState`, equal-power `Pan`), `TRIGGER` (`Sync`, `Audio` transient detection with 40 ms holdoff), `THRESHOLD`, `CUTOFF`, and `RESONANCE`.
- **UI**: `crates/app/src/views/device_ui/shaper.rs` adapts mode labels (`level` / `cutoff` / `L / R`) and contextual trigger/filter controls.

### `instrument.sampler` (`crates/stock/src/instrument/sampler.rs`, `sfz.rs`, `sample.rs`)
- Retains full single-WAV mode compatibility (`load_wav`, `set_sample_data`, waveform trim UI).
- Adds SFZ v2 instrument support (`load_sfz`, `load_sfz_with_threshold`):
  - `<control>` (`default_path`), `<global>`, `<group>`, `<region>` inheritance
  - Key splits (`lokey`, `hikey`, `key`, `pitch_keycenter`, note names like `c4` / `f#3`), velocity layers (`lovel`, `hivel`), round-robin (`seq_length`, `seq_position`), `trigger=attack|release`, loop modes, transpose/tune/volume/pan.
  - Windowed disk streaming (`StreamBacking`, `StreamWorker`) with `DEFAULT_PREROLL_FRAMES = 16_384`, `STREAM_PAGE_FRAMES = 16_384`, and `STREAM_SLOTS = 6` pre-allocated slots per streamed zone (zero allocations and zero locks on the audio thread).
