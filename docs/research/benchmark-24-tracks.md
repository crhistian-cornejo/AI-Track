# 24-Track Realtime Headroom Benchmark

Measured locally on `aarch64-apple-darwin` (Apple Silicon) after Block D optimizations (audio-thread FTZ/DAZ denormal flush, idle track/device silence & tail sleep gating, bounded LRU `SampleCache` byte budget, `.aipeaks` disk peak cache, and reduced-rate UI/low-power path).

## Test Configuration

- **Test symbol**: `real::engine::tests::benchmark_24_tracks_realtime_headroom` ([engine.rs](file:///Users/corx/Developer/AI-Track/crates/app/src/real/engine.rs#L925-L1024))
- **Command**:
  ```bash
  cargo test -p aitrack-app --release -- benchmark_24_tracks_realtime_headroom --nocapture
  ```
- **Render duration**: 10.0 seconds @ `48_000` Hz, block size `256` frames (`480_000` total frames), `120.0` BPM
- **Track & device load**:
  - **12 Audio tracks**: looping 4-beat stereo audio clip + `fx.eq` + `fx.compressor`
  - **12 Instrument tracks**: 16th/8th-note MIDI pattern + `instrument.wavetable` + `fx.eq` + (`fx.reverb` on every 3rd track, `fx.delay` on the rest)

## Observed Result (`--release`)

- **Wall-clock render time**: `1.190 s`
- **Real-Time Factor (RTF)**: `0.119` (`11.9%` DSP budget / ~`8.4x` faster than real-time)
- **Status**: `ok` (`0` assertions failed)

## Additional Block D Verifications

- **Audio-thread denormals**: `aitrack_dsp::disable_denormals()` (`x86_64` FTZ/DAZ + `aarch64` `FPCR.FZ`) invoked at audio thread entry in `crates/engine/src/rt.rs`.
- **Silence & tail sleep gating**: `tests::idle_device_sleeps_after_tail_and_wakes_on_audio` verified in `aitrack-engine`.
- **Bounded LRU SampleCache**: `sample_cache::tests::lru_evicts_over_byte_budget` verified in `aitrack-engine` (`DEFAULT_CACHE_BUDGET_BYTES = 512 MiB`, runtime configurable from Preferences -> Performance).
- **Disk waveform peak cache**: `waveform::tests::disk_cache_skips_redecode_on_second_load` verified in `aitrack-app`.
- **Windows target check**: `cargo check -p aitrack-model -p aitrack-dsp -p aitrack-stock -p aitrack-engine -p aitrack-host --target x86_64-pc-windows-msvc` exited with code `0`.
