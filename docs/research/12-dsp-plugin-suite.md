# Suite propia de DSP y plugins: arquitectura y bibliografía

Índice técnico de los plugins propios de `crates/stock`, sus primitivas en `crates/dsp` y la bibliografía de referencia usada en el diseño.

## Documentos por familia

| Documento | Contenido |
|---|---|
| [eq-compressor.md](12-dsp-plugin-suite/eq-compressor.md) | `fx.eq` (8 bandas, dinámico, M/S, fase mínima/natural/lineal FIR) y `fx.compressor` (VCA/Opto/FET/Bus, lookahead, sidechain) |
| [reverb-delay.md](12-dsp-plugin-suite/reverb-delay.md) | `fx.reverb` (FDN + placa Dattorro + shimmer/freeze) y `fx.delay` (estéreo, ping-pong, BBD, cabezal de cinta, difusión, ducking) |
| [saturation.md](12-dsp-plugin-suite/saturation.md) | `fx.saturator`, `fx.tape` (histéresis/wow/flutter), `fx.amp` (preamp/tone stack/cab), `fx.console` (bus/channel summing) y `fx.clipper` (ADAA1) |
| [acid-808-shaper-sfz.md](12-dsp-plugin-suite/acid-808-shaper-sfz.md) | `instrument.acid303` (diode ladder 18 dB/oct + secuenciador), `instrument.kick808`, `fx.shaper` (Volume/Filter LP/Pan + trigger audio) e `instrument.sampler` (SFZ v2 + streaming) |

## Resto de la suite

- **Dinámica / Master:** `fx.limiter` (true-peak 4× ITU-R BS.1770-5, lookahead + mínimo deslizante), `fx.gate`, `fx.transient`, `fx.multiband` (crossover LR4 / fase lineal FIR), `fx.midside`, `fx.utility`.
- **Modulación / Filtro:** `fx.filter` (SVF TPT / ladder), `fx.chorus` (chorus / flanger / phaser).
- **Análisis / Extracción:** `fx.analyzer` (espectro logarítmico + BS.1770 LUFS + correlación + true peak), `fx.drum_extract` (análisis y extracción de one-shots y loops limpios de batería desde WAV/MP3 mediante HPSS + De-bleed/De-reverb estilo RX + HT-Demucs opcional).
- **Instrumentos:** `instrument.wavetable`, `instrument.vault`, `instrument.drum`, `instrument.sampler`, `instrument.acid303`, `instrument.kick808`.

## Bibliografía principal

- **Zölzer, U. (ed.)**, *DAFX: Digital Audio Effects*, 2.ª ed., Wiley, 2011 (filtros, modulación, pitch-shifting §6.4, no linealidades cap. 4, modelado virtual analógico cap. 12).
- **Zavalishin, V.**, *The Art of VA Filter Design*, rev. 2.1.2, Native Instruments, 2020 (filtros TPT/SVF, escalera ZDF, diode ladder).
- **Simper, A. (Cytomic)**, *"Linear Trapezoidal Integrated SVF"* (topología TPT estable bajo modulación rápida).
- **Pirkle, W.**, *Designing Audio Effect Plugins in C++*, 2.ª ed., Focal Press / Routledge, 2019.
- **Smith, J. O. (CCRMA, Stanford)**:
  - *Introduction to Digital Filters with Audio Applications*
  - *Spectral Audio Signal Processing* (STFT, OLA, phase vocoder, diseño FIR por ventanas)
  - *Physical Audio Signal Processing* (guías de onda, líneas de retardo fraccionario)
- **Reiss, J. D. & McPherson, A.**, *Audio Effects: Theory, Implementation and Application*, CRC Press, 2014.
- **Välimäki, V. & Huovilainen, A.**, modelos discretos de filtros analógicos y síntesis anti-aliased (PolyBLEP / PTR).
- **Parker, J., Zavalishin, V. & Le Bivic, E.**, *"Reducing the Aliasing of Nonlinear Waveshaping Using Continuous-Time Convolution"*, DAFx-16, 2016 (ADAA1).
- **Bilbao, S., Esqueda, F., Parker, J. & Välimäki, V.**, *"Antiderivative Antialiasing for Memoryless Nonlinearities"*, IEEE SPL 24(7), 2017.
- **Giannoulis, D., Massberg, M. & Reiss, J. D.**, *"Digital Dynamic Range Compressor Design—A Tutorial and Analysis"*, JAES 60(6), 2012.
- **Dattorro, J.**, *"Effect Design, Part 1: Reverberator and Other Filters"*, JAES 45(9), 1997.
- **Puckette, M.**, *The Theory and Technique of Electronic Music*, World Scientific, 2007 (phase vocoder, granular y síntesis).
- **Roads, C.**, *Microsound*, MIT Press, 2001 (síntesis y time-stretch granular).
- **FitzGerald, D.**, *"Harmonic/Percussive Separation Using Median Filtering"*, DAFx-10, 2010, y **Driedger, J., Müller, M. & Disch, S.**, *"Extending Harmonic-Percussive Separation of Audio Signals"*, ISMIR 2014 (separación percussive/harmonic usada en extracción de drums y de-bleed).
- **Stinchcombe, T.**, *"Analysis of the Transistor Ladder and Diode Ladder Filters"* (asimetría y respuesta del filtro diode ladder 303).
