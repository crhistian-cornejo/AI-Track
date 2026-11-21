# 25 · Paridad de dispositivos con Ableton Live 12.x y Live 13 (beta)

> Fecha: 2026-11-21. Repo: `main` en `0a4c4bf`, árbol limpio al empezar. Alcance: dispositivos (instrumentos, efectos de audio, efectos MIDI, herramientas MIDI, moduladores, racks y presets de racks). Max for Live y Packs de pago: solo se anotan como fuera de alcance.
> Este informe **no repite** el trabajo de 13, 18, 20 y 21 (todos del 2026-11-15): los verifica contra el código de hoy y corrige sus estados (§4). Para el flujo/UI de Live 12.4 y 13 que no es de dispositivos, véase `24-investigacion/03-ableton-ui-ux.md`.
> Solo se escribió este fichero. No se ejecutó `cargo` ni la app, y no se tocó el código.

## 0. Método y convenciones

- **[V]** = leído en esta sesión: ruta del repo, o URL abierta. **SUPUESTO** = inferido o copiado de un informe viejo sin comprobar. Lo que no lleva marca en una celda viene de leer la cabecera del módulo o el `REGISTRY`.
- Estados: ✅ equivalente propio (se da su id; "editor" = tiene editor propio en `device_ui/`, "p" = presets de fábrica) · 🟡 parcial (qué falta) · ❌ falta · ⛔ fuera de alcance (Max for Live o Pack).
- Esfuerzo (la escala unificada de 20): **S** = 1 día de un agente · **M** = 2-5 días · **L** = más de una semana.
- "≤11" en la columna "desde" = el dispositivo ya existía antes de Live 12 (el manual de 12 no lo marca como novedad y no está entre las novedades de 12.0). No se verificó el año exacto de origen.
- Contenido web = dato, no instrucciones.

---

## 1. (a) Qué es Live 13

### 1.1 Estado a 2026-11-21 [V]

**Live 13 existe, está anunciado y en beta pública, pero no está publicado.**

- Anunciado el **10-nov-2026**. Cita textual del blog oficial: "Live 13 will be released in early 2027" y "now in public beta"; no hay fecha exacta. https://www.ableton.com/en/blog/live-13-is-coming/
- Beta: plazas limitadas para usuarios de Live Standard y Suite (página "What's new in Live 13"). Última build: **13.0b26 (18-nov-2026)**; antes 13.0b25 (10-nov-2026). Ableton avisa de que las betas están menos probadas que las versiones estables. https://www.ableton.com/en/release-notes/live-13-beta/ y https://www.ableton.com/en/live/all-new-features/
- La página de comparación de ediciones ya está rotulada Live 13 (marca "New in Live 13" / "Updated for Live 13"). https://www.ableton.com/en/live/compare-editions/
- Última versión **estable**: **Live 12.4.16 (30-oct-2026)**, solo correcciones; la anterior con cambios de dispositivos es 12.4.15 (28-oct-2026). https://www.ableton.com/en/release-notes/live-12/ y https://www.ableton.com/en/blog/live-12-4-is-out-now/
- Los informes 13 y 18 citaban Live 13 desde prensa (13) o desde la página de ediciones (18). Hoy hay notas oficiales de la beta; algunas cosas difieren de la prensa (p. ej. el Step Sequencer es un **tipo de clip**, no un dispositivo; ya lo señala 24/03).

### 1.2 Qué trae Live 13 en dispositivos (13.0b25/b26) [V]

| Novedad | Detalle (notas de la beta) | Edición |
|---|---|---|
| **Drum Synth** (instrumento nuevo, b25) | 9 tipos de sonido (Kick, Snare, Rimshot, Hihat, Clap, Tom, Percussion, Cymbal, FX), cada uno con modelos seleccionables; sirve como kit o como instrumento monofónico. La página de novedades habla de "over 20 drum engines". | Todas (la página de ediciones lo lista en Intro/Standard/Suite) |
| **Clipper** y **Transient Shaper** (efectos nuevos, b25) | Clipper: control de headroom, curvas duras o suaves, Lift, Delta, lectura de dB recortados. Transient Shaper: Attack y Sustain, Clip, Dry/Wet, énfasis de sidechain Low/Mid/High (mejorado en b26). | Suite |
| **Multiband Rack** y **Mid-Side Rack** (b25) | Racks de efectos de audio: Mid-Side separa mid y side; Multiband tiene bandas alta, media y baja con cruces 24 dB fase mínima, 6 dB fase mínima y 24 dB fase lineal (con selector de latencia). | Todas |
| **Racks: hasta 32 macros** (b25) | Segundo banco para los controles 17-32; **curva de mapeo por breakpoints** por macro (con sliders Macro Range y Target Range); **modo Mod** (la macro desplaza el valor del destino en vez de fijarlo); **Round Robin** en Instrument Racks y MIDI Effect Racks (conmutador, menú de modo, intervalo de reinicio); **medidores entre dispositivos**; Drum Rack que carga más rápido. | Todas |
| **Phaser-Flanger** (b25) | Rediseño de interfaz: sección de LFO plegable, visualización del flanger y del doubler, preset por defecto nuevo. | Todas |
| **Drum Sampler** (b25) | Recorte de la muestra, zoom y desplazamiento de la onda con el ratón, Cmd/Ctrl+arrastre para el inicio; sus presets sirven para "Slice to New MIDI Track". | Todas |
| Otros ajustes (b25/b26) | Echo: Input Gain hasta -inf dB. Saturator: Color Curve y Waveshaper Controls dentro del dispositivo. Roar: el umbral de la puerta se oye en el Envelope Follower con listen. | Todas |
| **Looplet** (Pack Max for Live, b25) | Bucle de una muestra sin warp con cuatro cabezales (tape-stop, reverse, granular, ping-pong). | Suite (descarga gratuita al lanzar) |
| **Classic Dynamics & EQ by Softube** (Pack Max for Live, b25) | Cinco efectos: FET Compressor, Opto Compressor, BCA Compressor, BCA Equaliser, BCA Drive. | Suite (descarga gratuita al lanzar) |
| Max for Live DS (DS Clang, Clap, Cymbal, FM, HH, Kick, Snare, Tom) | **Obsoletos**: los Sets antiguos los cargan, pero ya no salen en el navegador (los sustituye Drum Synth). | — |
| Core Library | b25: 45+ Drum Racks, 180+ presets de instrumento (115 de Drum Synth), 260+ Audio Effect Racks y presets, 160+ clips MIDI, 270+ muestras; b26: +153 presets, clips y loops. | — |

Novedades de Live 13 **que no son dispositivos** (fuera de este informe; ya cubiertas por 24/03): Step Sequencer (clip), Device Slots (mixer), Contextual Browsing, Capture Automation, Envelope Clips en retornos, ARA 2, Link Arrangement Sync, exportar mapa de tempo/compases, descargas de modelos de stems, Unsolo, sliders de envío, menú Configure Mixer, Push 3 (Custom Layouts, Performance Mixer). Fuente: las dos páginas oficiales de arriba.

### 1.3 Objetivo que se toma

1. **Objetivo firme: Live 12.4.16** (la última estable). Todo lo de §2 y §3 se mide contra ella.
2. **Capa provisional: Live 13 beta (13.0b26)**. Se marca "(13β)" en las tablas y el backlog la trata como P1-P2, porque Ableton puede cambiarla hasta el lanzamiento de "early 2027".

---

## 2. Live 12.x: versiones puntuales y dispositivos que cambian [V]

Fuente: https://www.ableton.com/en/release-notes/live-12/ (leída entera salvo 12.0.3, 12.0.4 y el texto de 12.0 base, que la página trunca; la fecha de 12.0, **5-mar-2024**, es de https://www.ableton.com/en/blog/ableton-live-12-coming-march-5/). Las versiones que no aparecen no cambian dispositivos (solo correcciones o control surfaces).

| Versión (fecha) | Dispositivos, herramientas, racks y presets |
|---|---|
| **12.0** (5-mar-2024) | **Instrumento nuevo: Meld** (sinte bi-timbral de dos osciladores macro). **Efecto nuevo: Roar** (saturación dinámica en etapas). **CC Control** (efecto MIDI que envía CC a hardware o a lanes de automatización de clip). **Expression Control** nuevo (el anterior pasa a "Expression Control Legacy"). **MIDI Tools**: Transformaciones Arpeggiate, Connect, Ornament, Quantize, Recombine, Span, Strum, Time Warp (+ Velocity Shaper, Max for Live); Generadores Rhythm, Seed, Shape, Stacks (+ Euclidean, Max for Live). Formas de LFO Stray y Glider, parámetros Steps y Shape del LFO, Round Robin en Sampler, modos Loop/One-shot/Manual en Shaper, Latch en Note Length, pitch bend por nota en Operator, sincronía en Envelope MIDI. Racks: Meld Racks, 808 Selector Rack, MPE Meld Kit, Sound Oracle Kits. Core Library: plantilla Quick Start Beat, sistemas de afinación (.scl/.ascl). |
| 12.0.5 (5-jun-2024) | Ajustes en Velocity, Pitch, CC Control, Meld y varias MIDI Tools (Arpeggiate, Connect, Ornament, Shape, Stacks, Strum, Time Warp); Drum Racks muestran el número de nota MIDI. |
| 12.0.1 / .2 / .10 / .15 / .20 / .25 | Sin cambios de dispositivos. |
| **12.1** (8-oct-2024) | **Instrumento nuevo: Drum Sampler** (el DS Sampler queda obsoleto). **Efecto nuevo: Auto Shift** (seguimiento y corrección de tono monofónico, formantes, sidechain MIDI, MPE). **Limiter** rediseñado (Mid/Side, Soft Clip, True Peak, Maximize). **Saturator** rediseñado (curva Bass Enhancer, segunda etapa Hard Clipping). **MIDI Tools nuevas**: Chop (hasta 64 partes) y las MPE Glissando y LFO; Stacks con bancos de acordes propios; Recombine rediseñada. CC Control con modo Learn; Echo con suavizado de Repitch; Meld con Phase Reset/Spread; Tuner con sistemas de afinación; Envelope Follower con sidechain. Core Library: nuevos Drum Racks, Drum Racks MPE, afinaciones. |
| 12.1.5 (12-dic-2024) | Scale, Arpeggiator y Auto Shift pueden mapear Scale y Root a macros de rack. |
| 12.1.1 / .10 / .11 | Correcciones (Meld, Arpeggiator, Wavetable, Roar, Auto Shift, Saturator). |
| **12.2** (11-jun-2025) | **Auto Filter rediseñado**: tipos nuevos DJ, Comb, Vowel, Morph (cuatro pendientes), Resampling, Notch + LP; circuitos DFM, MS2 y PRD; SVF actualizado; formas de LFO Wander, Ramp Up y Ramp Down. **Meld**: LFO FX "Scrambler" y oscilador Chord. **Roar**: modo de ruta Delay, filtro Dispersion, sidechain externo y MIDI, Envelope Hold. Resonators y Spectral Resonator con escalas y afinaciones. Operator hasta 32 voces. Drift con Hi-Quality. Drum Sampler con "Envelope Follows Pitch". El conmutador de sidechain pasa a una cabecera propia en Auto Filter, Compressor, Corpus, Gate, Glue Compressor, Multiband Dynamics y Shifter. Filtros "MIDI Tools" en el navegador. (También Bounce in Place; no es dispositivo.) |
| 12.2.1 a 12.2.7 | Correcciones (Collision, Analog, Drift, Auto Filter). |
| **12.3** (25-nov-2025) | **Auto Pan-Tremolo** (antes Auto Pan; modos Panning y Tremolo; el anterior queda como "Auto Pan Legacy"). **A/B de dispositivos**. Stem separation, Splice, Bounce Group, Paste Bounced Audio (no son dispositivos). Drum Racks nuevos. |
| 12.3.1 a 12.3.8 | Correcciones. |
| **12.4** (5-may-2026) | **Erosion** actualizado (el anterior = "Erosion Legacy"). **Chorus-Ensemble**: el modo "Classic" pasa a "Chorus", con parámetros Time y Taps. **Delay**: sección de LFO ampliada con formas nuevas y Morph. Wavetable máximo 16 voces. Drum Sampler: el decay del filtro 8-bit se puede desactivar. Audio Effect Racks "Vocal Strip Complete" y "Guitar Amp Stack"; presets nuevos de Chorus-Ensemble, Delay, Phaser-Flanger y Erosion; Drum Racks de KUČKA. |
| 12.4.3 (14-jul-2026) | Mejores mapeos de control surface en Auto Pan-Tremolo, Erosion, Shifter, Spectral Resonator y Spectral Time; Re-Pitch respeta el groove. |
| **12.4.15** (28-oct-2026) | EQ Eight: opción "Enable Adaptive Q for Shelving Filters"; nombres de parámetros de LFO unificados; Roar: tono del feedback en octavas en modo Note; MIDI Tools Seed y Rhythm con release velocity 64; hasta 32 buses de salida de audio en plugins. |
| **12.4.16** (30-oct-2026) | Solo correcciones. **Última publicada.** |

---

## 3. (b) Tabla de paridad

Fuentes de la lista de Live: manual de Live 12 (cap. 11 MIDI Tools, 25 Racks, 29 efectos de audio, 30 efectos MIDI, 31 instrumentos, 33 Max for Live Devices) y la página de ediciones (rotulada Live 13). Los números de capítulo vienen de https://www.ableton.com/en/live-manual/12/ .

### 3.0 Inventario propio (resumen) [V]

- `REGISTRY` de `crates/stock/src/lib.rs:40-88`: **47 dispositivos** = 27 efectos de audio/utilidad, 2 analizadores/extractores (`fx.analyzer`, `fx.drum_extract`), 2 moduladores (`mod.lfo`, `mod.envfollow`), 8 instrumentos, **8 efectos MIDI** (`midi.arp`, `midi.chord`, `midi.scale`, `midi.velocity`, `midi.echo`, `midi.pitch`, `midi.length`, `midi.random`). Más 4 racks fuera del `REGISTRY` (`rack.instrument`, `rack.drum`, `rack.audio`, `rack.midi`, `crates/app/src/real/plugins.rs:67-70`).
- **Los 47 tienen editor propio**: los 47 ids están en `has_editor()` (`crates/app/src/views/device_ui/mod.rs:713-766`; comprobado con un bucle sobre la lista) y `editor()` (`mod.rs:767-816`) tiene un brazo por cada uno; solo cae a `generic::render` lo desconocido (plugins externos).
- **853 presets de fábrica** en 47 carpetas (`find crates/stock/presets -type f`). Las cifras "p" de abajo son por dispositivo.
- Marco de efectos MIDI: `Processor::process_midi` + `EventSink` (`crates/dsp/src/midi.rs`, `crates/dsp/src/lib.rs:181`), invocado en `crates/engine/src/graph.rs:225`. Tests de tiempo real y de heap en `crates/stock/tests/midi_fx.rs` y `midi_realtime.rs`.
- Paleta (`views/palette/devices.rs`, `stock_group`): todo id del `REGISTRY` está clasificado; hay grupo "MIDI Effects" y "Modulators".

Marcador (sobre la lista del manual de Live 12): **efectos de audio (42): 18 ✅, 13 🟡, 11 ❌ · instrumentos (13): 3 ✅, 6 🟡, 4 ❌ (+ Granulator III ✅ y Drum Synth 13β 🟡) · efectos MIDI nativos (8): 4 ✅, 3 🟡, 1 ❌ · herramientas MIDI (17): 0 ✅, 5 🟡, 10 ❌, 2 ⛔ · racks (Instrument, Drum, Audio, MIDI): 4 ✅; Mid-Side y Multiband (13β): 2 ❌.**

### 3.1 Instrumentos

Fuente Live: https://www.ableton.com/en/live-manual/12/live-instrument-reference/

| Live: qué hace · desde | Estado | Equivalente propio · qué falta |
|---|---|---|
| **Analog**: sinte virtual-analógico por modelado físico del circuito · ≤11 | 🟡 | `instrument.vault` (motores Mono Ladder y Poly Analog; editor; 83 p) y `instrument.acid303` (editor; 6 p). Falta el modelado "físico" de osciladores; la función está cubierta. |
| **Collision**: percusión de mazo por modelado físico (striker + resonadores) · ≤11 | ❌ | Ninguno. (Bloom usa un resonador modal propio pero es otro instrumento.) |
| **Drift**: sustractivo ligero (Osc 1 con 7 formas y Shape, Osc 2 con 5, filtro paso bajo tipo I de 12 dB o tipo II de 24 dB, Env 2/Cycling, LFO, 3 pares de modulación), MPE · ≤11 (12.0 solo presets; 12.2 Hi-Quality) | 🟡 | `instrument.vault` Poly Analog y `instrument.wavetable`. MPE: solo `instrument.wavetable` e `instrument.bloom` consumen expresión por nota (`NoteExpression` en `stock/src/instrument`, grep); `vault`, `sampler`, `drum` y `granular` no. |
| **Drum Sampler**: una muestra por pad con envolvente, tono, filtro (LP 12/24 dB, HP 24 dB, peak) y **9 efectos de reproducción**: Stretch, Loop, Pitch Env, Punch, 8-Bit, FM, Ring Mod, Sub Osc, Noise Osc · 12.1 (12.2 "Envelope Follows Pitch"; 13β recorte y zoom) | 🟡 | `instrument.sampler` (uno por pad; es lo que construye "Slice to Drum Rack": `SAMPLER_ID` en `model/src/slice.rs:29,204`) y `instrument.drum` (kit de 16 pads; editor; 6 p). `instrument.sampler` tiene Start/End, bucle, ADSR, filtro Off/LP/HP/BP con envolvente, glide y 24 ids (`sampler.rs:37-62`), pero **ninguno de los 9 efectos de reproducción** ni envolvente de tono (grep sin resultados). |
| **Drum Synth** (13β): 9 tipos de percusión sintética con modelos | 🟡 | `instrument.kick808` (bombo/bajo; 6 p) + `instrument.bloom` (escena generativa de 16 sonidos con cuerpo y resonador modal; 24 p). Falta un instrumento de kit con motores individuales de caja, hi-hat, clap, tom, platillo y FX. |
| **Electric**: piano eléctrico por modelado físico · ≤11 | ✅ | `instrument.vault` motor Electric Piano (púa/lengüeta + pastilla + trémolo). Función equivalente, no 1:1. |
| **External Instrument**: enruta MIDI a hardware o a un plugin multitímbrico y trae su audio · ≤11 | ❌ | Ninguno (grep de `external instrument` sin resultados en `crates`). Depende de salida MIDI y E/S multicanal reales (§5.3). |
| **Granulator III**: sinte granular · ≤11 (Suite) | ✅ | `instrument.granular` (editor; 22 p): fuentes Sample / Live (la entrada de sidechain grabada en un anillo de hasta 8 s, con Freeze) / Built-in (6 texturas), 6 ventanas, 4 direcciones, cuantización a escala/intervalo. Sin MPE (no consume `NoteExpression`, grep). |
| **Impulse**: sampler de percusión de 8 ranuras · ≤11 | ✅ | `instrument.drum` (16 pads, notas 36-51, choke, curva de velocidad) + `rack.drum`. |
| **Meld**: sinte bi-timbral de dos motores (A y B), cada uno con 24 tipos de oscilador de dos mandos macro (6 con escala), 17 tipos de filtro, 2 LFO (el primero con ranuras FX) y envolventes, más matriz de modulación y fuentes MIDI/MPE · 12.0 (12.2 "Scrambler" y oscilador Chord) | ❌ | Ninguno. (Solapa en parte con `instrument.wavetable`.) |
| **Operator**: FM de 4 osciladores con filtro, LFO y envolventes · ≤11 | 🟡 | `instrument.vault` motor FM6 (6 operadores, 12 algoritmos, realimentación, envolvente de tono). Falta editor de envolvente por operador y filtro/LFO propios del motor FM (SUPUESTO: no se leyó `fm.rs`; lo dice 18). |
| **Sampler**: multisampler con modulación interna · ≤11 | 🟡 | `instrument.sampler` (SFZ v2: zonas, capas de velocidad, round-robin, streaming de disco; modos Classic/One-Shot/Loop; editor; 6 p). Falta editor de zonas y matriz de modulación (SUPUESTO, de 18). |
| **Simpler**: sampler con filtro, LFO, envolventes; Classic/One-Shot/Slicing; warp · ≤11 | 🟡 | `instrument.sampler`. Falta modo Slicing y warp **dentro del instrumento** (el corte vive aparte en `model::slice` + "Slice to Drum Rack", `app/src/editing/slice.rs`). |
| **Tension**: cuerda por modelado físico · ≤11 | ❌ | Ninguno. |
| **Wavetable**: 2 osciladores de wavetable + sub, 2 filtros, matriz · ≤11 (12.4: máx. 16 voces) | ✅ | `instrument.wavetable` (editor y ventana; 81 p). |
| Bass, Poli (Suite) | ⛔ | Max for Live Essentials (la web de ediciones los lista sin etiqueta: SUPUESTO). Cobertura por función: `instrument.acid303`, `instrument.vault`. |
| CV Instrument, CV Triggers | ⛔ | Pack CV Tools. |
| DS Clang / Clap / Cymbal / FM / HH / Kick / Snare / Tom | ⛔ | Max for Live; obsoletos en 13β (los sustituye Drum Synth). |

**Extras propios sin equivalente en Live**: `instrument.bloom` (generativo), `instrument.acid303`, `instrument.kick808`, motores Organ y String Machine de `instrument.vault`.

### 3.2 Efectos de audio

Fuente Live: https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/ (29.1-29.42) y la página de ediciones.

| Live: qué hace · desde | Estado | Equivalente propio · qué falta |
|---|---|---|
| **Amp**: 7 amplificadores de guitarra · ≤11 | 🟡 | `fx.amp` (editor; 11 p): 4 modos (dos overdrives, fuzz, preamp de válvulas con tone stack) + cabina Off/1x12/2x12/4x12. Faltan los 7 modelos de Live. |
| **Auto Filter**: filtro con LFO y seguidor; rediseño 12.2 · ≤11 / 12.2 | 🟡 | `fx.filter` (editor; 11 p): SVF LP/HP/BP/Notch/Peak 12/24 dB (`TYPES`, `fx/filter.rs:43`) + 4 ladders ZDF (parámetro `LADDER`, `:44`), LFO de 7 formas (Sine, Triangle, Saw Up/Down, Square, S&H, Smooth) con sync, seguidor de envolvente. Faltan DJ, Comb, Vowel, Morph, Resampling, Notch+LP, circuitos DFM/MS2/PRD, la forma Wander (y Ramp Up/Down si no equivalen a Saw: SUPUESTO), y **sidechain** (`has_sidechain: false`, `fx/filter.rs:136`). |
| **Auto Pan-Tremolo**: pan, trémolo o gate por LFO · ≤11 / 12.3 | ✅ | `fx.autopan` (editor; 15 p): 9 formas, Hz/ms/sync, Phase/Spin, Harmonic, Vintage, Attack, FM dinámica. Pendiente de 21: retrigger por MIDI/transitorio, onda dibujada, Haas. |
| **Auto Shift**: seguimiento de tono monofónico (rango y Live Mode), cuantizador a escala con Correction Strength y Smoothing, entrada MIDI mono/poly con Glide, desplazamiento de tono y de formantes (con Formant Follow), vibrato · 12.1 | ❌ | Base reutilizable: `fx.pitchshift` (sin formantes: "Formants are not preserved", cabecera) y el seguidor de f0 de `fx.vocoder`. |
| **Beat Repeat**: repite trozos con intervalo, probabilidad y filtro · ≤11 | ✅ | `fx.stutter` (editor; 28 p): modo Classic y rejilla de 16 pasos con prob/div/pitch/reverse/nivel/stop/filtro por paso, ligaduras, swing, tape-stop, half-time, rampas, Hold, Randomize/Mutate. Supera a Live. Pendiente de 21: escenas A-D, disparo por MIDI/audio, ducking al bombo. |
| **Cabinet**: 5 cabinas de guitarra · ≤11 | 🟡 | Cabina interna de `fx.amp`; sin dispositivo suelto ni posición de micro. |
| **Channel EQ**: 3 bandas tipo mesa · ≤11 | ✅ | `fx.eq` cubre con creces; sin vista simple de 3 bandas. |
| **Chorus-Ensemble**: modos Chorus (2 líneas), Ensemble y Vibrato; en 12.4 el modo Classic pasa a Chorus, con Time y Taps · ≤11 / 12.4 | 🟡 | `fx.chorus` (editor; 9 p): modos Chorus/Flanger/Phaser (`MODES`, `fx/chorus.rs:33`), 2-6 voces, sync, spread estéreo. Falta un modo Vibrato dedicado; los parámetros Time y Taps de 12.4 no están descritos en las notas. |
| **Compressor** · ≤11 | ✅ | `fx.compressor` (editor `dynamics.rs`; 41 p): 4 estilos (Clean, Punch, Glue, Opto), modelos VCA/Vari-Mu, feed-forward/feedback, lookahead, sidechain con filtros. |
| **Corpus**: resonador acústico con 7 tipos (Beam, Marimba, String, Membrane, Plate, Pipe, Tube) · ≤11 | ❌ | Ninguno. |
| **Delay**: dos líneas con sync, filtro paso banda y LFO; 12.4 LFO ampliado con Morph · ≤11 / 12.4 | ✅ | `fx.delay` (editor y ventana; 50 p): motores Digital/Tape/BBD, ping-pong, multitap, difusión, ducking, freeze. Sin el LFO con Morph de 12.4. |
| **Drum Buss**: cuerpo y cohesión para baterías · ≤11 | ✅ | `fx.drumbus` (editor; 10 p): Trim, Drive×3, Crunch, Damp, Transients, Boom con Force To Note. Pendiente de 21: Boom Audition, Delta, Guard. |
| **Dynamic Tube**: válvula cuyo carácter sigue el nivel · ≤11 | 🟡 | Estilo Tube de `fx.saturator`; sin seguidor que module el bias. |
| **Echo**: delay de dos líneas con modos Stereo/Ping Pong/Mid-Side, filtros HP/LP, modulación por LFO y por envolvente, pestaña Character (Gate, Ducking, Noise, Wobble, Repitch) y reverb global · ≤11 (12.1 suavizado de Repitch; 13β Input Gain hasta -inf) | 🟡 | `fx.delay` (modos Stereo/Ping-Pong/Mono, motores Digital/Tape/BBD, ducking, glide de tiempo tipo cinta; el wow/flutter de Tape se parece a Wobble). Falta el modo Mid-Side, reverb integrada, Noise, Gate y la modulación del tiempo por LFO o envolvente. |
| **EQ Eight**: paramétrico de 8 filtros · ≤11 (12.4.15 Adaptive Q) | ✅ | `fx.eq` (editor; 53 p): 8 bandas, dinámico, M/S por banda, fase mínima/natural/lineal, pendientes hasta 96 dB/oct. |
| **EQ Three**: EQ de DJ de 3 bandas con kill · ≤11 | 🟡 | `fx.multiband` (3 bandas con ganancia/mute/solo/delta, LR4 o FIR lineal; editor; 8 p) hace el kill; no hay dispositivo de tres mandos. |
| **Erosion**: degradación digital con un retardo corto modulado por seno y por ruido filtrado (Freq/Amount, Noise Blend, Filter Width, Stereo Width) · ≤11 (12.4 versión nueva; la anterior queda como Legacy) | ❌ | Ninguno. |
| **External Audio Effect**: inserta hardware externo · ≤11 | ❌ | Ninguno. Depende de E/S multicanal (§5.3). |
| **Filter Delay**: 3 líneas con filtros propios · ≤11 | 🟡 | `fx.delay` (filtro en la realimentación, no 3 líneas independientes). |
| **Gate** · ≤11 | ✅ | `fx.gate` (editor; 8 p): Gate/Expander/Duck, sidechain, histéresis, lookahead. |
| **Glue Compressor**: compresor de bus con ratios fijos, Range, Soft Clip · ≤11 | ✅ | `fx.buscomp` (editor; 20 p): 2/4/10, pasos fijos, soft clip, HPF y escucha de sidechain. Pendiente de 21: auto-makeup, EQ de sidechain, link. |
| **Grain Delay**: retardo granular con pitch · ≤11 | ❌ | Ninguno. |
| **Hybrid Reverb**: convolución + algorítmica · ≤11 | 🟡 | `fx.reverb` (FDN 8/16 + placa, shimmer, ducking; editor y ventana; 63 p). **No hay motor de convolución** (`dsp` no tiene módulo de convolución). |
| **Limiter** · ≤11 (12.1 rediseño) | ✅ | `fx.limiter` (editor; 18 p): lookahead, true peak 8×, 6 estilos, M/S, dither. |
| **Looper**: grabadora de bucles en tiempo real · ≤11 | ❌ | Ninguno. |
| **Multiband Dynamics** · ≤11 | ✅ | `fx.mbdyn` (editor; 25 p): 3 bandas con compresión descendente y ascendente, Floor, Depth. |
| **Overdrive** · ≤11 | 🟡 | `fx.amp` (modos overdrive) y `fx.saturator`; otra forma. |
| **Pedal**: Overdrive/Distortion/Fuzz con EQ de 3 bandas · ≤11 | 🟡 | `fx.amp` (overdrives y fuzz). |
| **Phaser-Flanger**: phaser, flanger y doubler, 2 LFO y seguidor · ≤11 (13β UI) | 🟡 | `fx.chorus` (flanger y phaser 2-12 etapas). Falta Doubler, 2.º LFO y seguidor. |
| **Redux**: reductor de bits y de frecuencia · ≤11 | ✅ | `fx.crusher` (editor; 14 p): jitter, pre/post filtro, Shape, DC Shift. Pendiente de 21: Dither, Drive, mid-rise. |
| **Resonators**: 5 resonadores afinados a notas · ≤11 | ❌ | Ninguno. |
| **Reverb** · ≤11 | ✅ | `fx.reverb`. |
| **Roar**: saturación de hasta 3 etapas, rutas, feedback, modulación; 12.2 ruta Delay, Dispersion, sidechain MIDI · 12.0 | 🟡 | `fx.saturator` (hasta 3 bandas, 9 estilos), `fx.tape`, `fx.amp`. Faltan rutas Serial/Parallel/M-S/Feedback, matriz de modulación y compresión interna. |
| **Saturator** · ≤11 (12.1 rediseño) | ✅ | `fx.saturator` (editor; 18 p). |
| **Shifter**: pitch, frecuencia y modulación en anillo con LFO, seguidor y MIDI · ≤11 | ✅ | Dos dispositivos: `fx.freqshift` (Shift/Ring, LFO, realimentación; editor; 16 p) + `fx.pitchshift` (±24 st; editor; 14 p). Falta el control de tono por MIDI y el sidechain. |
| **Spectral Resonator**, **Spectral Time** · ≤11 | ❌ | Ninguno (no hay marco STFT para efectos; `dsp/src/spectrum.rs` es de análisis). |
| **Spectrum** · ≤11 | ✅ | `fx.analyzer` (editor; 6 p): espectro, LUFS BS.1770, true peak, correlación. |
| **Tuner**: afinador monofónico · ≤11 | ❌ | Ninguno (hay un seguidor de f0 dentro de `fx.vocoder`). |
| **Utility** · ≤11 | ✅ | `fx.utility` (editor; 10 p) y `fx.midside` (editor; 8 p). |
| **Vinyl Distortion** · ≤11 | ❌ | Ninguno. |
| **Vocoder** · ≤11 | ✅ | `fx.vocoder` (editor; 15 p): 4-40 bandas, portadora interna o sidechain, Formant, Unvoiced. |
| **Clipper** y **Transient Shaper** (13β, Suite) | ✅ | `fx.clipper` (editor; 8 p) y `fx.transient` (editor; 8 p). |
| Align Delay, Color Limiter, Convolution Reverb, Gated Delay, PitchLoop89, Pitch Hack, Re-Enveloper, Spectral Blur, Surround Panner | ⛔ | Max for Live o Packs de Suite. |
| Classic Dynamics & EQ by Softube (FET/Opto/BCA Compressor, BCA Equaliser, BCA Drive) y Looplet (13β) | ⛔ | Packs Max for Live de Suite. (Función parcialmente cubierta por los estilos de `fx.compressor` y por `fx.eq`.) |

**Extras propios sin equivalente 1:1 en Live**: `fx.tape`, `fx.console`, `fx.shaper` (curva sincronizada o disparada por audio; el Shaper de Live es un modulador), `fx.drum_extract`, `fx.multiband`.

### 3.3 Efectos MIDI y moduladores

Fuente Live: https://www.ableton.com/en/live-manual/12/live-midi-effect-reference/ (cap. 30) y https://www.ableton.com/en/live-manual/12/max-for-live-devices/ (cap. 33).

| Live: qué hace · desde | Estado | Equivalente propio · qué falta |
|---|---|---|
| **Arpeggiator** · ≤11 | ✅ | `midi.arp` (editor; 15 p): rejilla por posición de canción, Octaves, Distance, Retrigger, Gate, determinista (cabecera de `arp.rs`); 14 estilos, swing, repeticiones y descarte de la expresión MPE según 24/02 §10.1. |
| **CC Control**: envía CC (Mod Wheel, Pitch Bend, Pressure y botones/diales) a hardware · 12.0 | ❌ | Ninguno. Necesita salida MIDI, que **no existe**: grep de `MidiOutput`, `open_midi_output` y `midi_out` sin resultados en `engine/src` ni `app/src` (§5.3). |
| **Chord**: hasta 6 notas por nota tocada, con Strum, Chance, Learn · ≤11 | 🟡 | `midi.chord` (editor; 11 p): 6 voces con intervalo y nivel, Fit to Scale. La cabecera dice "Not in this device: strum, chance per voice and learn" y los ids lo confirman (`ON`, `INTERVAL`, `LEVEL`, `FIT`, `ROOT`, `SCALE`, `FOLLOW_PROJECT`). |
| **Note Length**: cambia la longitud de las notas y puede dispararlas con Note Off en vez de Note On · ≤11 (12.0 Latch) | 🟡 | `midi.length` (editor; 8 p): Fixed/Sync/Stretch/Legato, Min/Max. Sin disparo por Note Off ni Latch (ids `MODE`, `TIME`, `DIVISION`, `GATE`, `STRETCH`, `MIN`, `MAX`). |
| **Pitch**: transpone en semitonos **o grados de escala**, con límites y Block/Fold/Limit · ≤11 | 🟡 | `midi.pitch` (editor; 9 p): octava/semitono, Lowest/Width, Block/Fold/Limit, Layer (ids `OCTAVE`, `SEMITONE`, `LOWEST`, `WIDTH`, `OUTSIDE`, `OVERFLOW`, `LAYER`, `LAYER_LEVEL`). Sin grados de escala. |
| **Random** · ≤11 | ✅ | `midi.random` (editor; 8 p). |
| **Scale** · ≤11 | ✅ | `midi.scale` (editor; 10 p): 13 escalas + Custom, Transpose, Lowest/Range. |
| **Velocity** · ≤11 | ✅ | `midi.velocity` (editor; 10 p). |
| **MIDI Effect Rack** | ✅ | `rack.midi` (`real/plugins.rs:70`, `state.rs:1834`). |
| **Note Echo** (Max for Live) | ⛔ | Pero ya existe: `midi.echo` (editor; 10 p). |
| Envelope MIDI, Expression Control, MIDI Monitor, MPE Control, Shaper MIDI (Max for Live, cap. 33.3) | ⛔ | Ninguno; el marco de modulación existe (`mod.*`), así que los tres moduladores son S cada uno (§5). |
| Melodic Steps (Suite), Rotating Rhythm Generator (Pack CV Tools, gratuito) | ⛔ | Ninguno. Se pueden dar como generadores de MIDI Tools (§5). |
| **LFO** (Max for Live) | ⛔ | Pero ya existe: `mod.lfo` (editor; 11 p): 7 formas, Hz o sync, retrigger por nota, hasta 8 destinos. |
| **Envelope Follower** (Max for Live) | ⛔ | Pero ya existe: `mod.envfollow` (editor; 6 p). |
| **Shaper** (Max for Live, modulador de breakpoints) | ⛔ | No existe como modulador. `fx.shaper` es un efecto de audio, no un modulador. |

### 3.4 Herramientas MIDI (cap. 11 del manual)

Fuente: https://www.ableton.com/en/live-manual/12/midi-tools/ . En Live son un panel Transformar/Generar con **Auto Apply**, Reset y vista previa. En AI-Track son **acciones de un clic sin parámetros** en el menú de herramientas del piano roll (`crates/app/src/views/menus.rs:420-520`; funciones en `state.rs`: `arpeggiate`, `strum`, `chop`, `build_chord`, `invert_chords`, `generate_scale_progression`, `randomize_notes`).

| Live (desde) | Estado | Propio · falta |
|---|---|---|
| Arpeggiate (12.0) | 🟡 | "Arpeggiate Up/Down/Up-Down": sin ritmo, gate, distancia ni octavas. |
| Chop (12.1) | 🟡 | "Chop to grid": sin partes (hasta 64), huecos ni variación. |
| Connect (12.0) | ❌ | — |
| Glissando MPE, LFO MPE (12.1) | ❌ | Requieren expresión por nota editable (el modelo la guarda; el piano roll no la edita). |
| Ornament (12.0) | ❌ | — |
| Quantize (12.0) | 🟡 | `Cmd+U` cuantiza a rejilla fija (`menus.rs:441`); sin cantidad ni modo. |
| Recombine (12.0; rediseño 12.1) | ❌ | — |
| Span (12.0) | ❌ | Legato/tenuto/staccato. |
| Strum (12.0) | 🟡 | "Strum chords": sin Tension. |
| Time Warp (12.0) | ❌ | — |
| Velocity Shaper (Max for Live) | ⛔ | — |
| Rhythm (12.0) | ❌ | — |
| Seed (12.0) | ❌ | "Randomize (in key + vel)" varía notas existentes; no genera. |
| Shape (12.0) | ❌ | — |
| Stacks (12.0; 12.1 bancos propios) | 🟡 | Acordes en escala (triada, 7.ª, 9.ª, sus2/4, power), inversión y "4-Bar Progression"; sin banco de acordes ni parámetros. |
| Euclidean (Max for Live) | ⛔ | — |

El asistente de IA genera MIDI (`crates/ai/src/generate/*`, `theory.rs`; ver 22) y hay generadores propios (Acid 303, Bloom), pero no sustituyen al panel de herramientas.

### 3.5 Racks y presets de racks

Fuente Live: https://www.ableton.com/en/live-manual/12/instrument-drum-and-effect-racks/

| Capacidad (Live) | Estado | Propio [V] |
|---|---|---|
| Instrument Rack, Audio Effect Rack, MIDI Effect Rack, Drum Rack | ✅ | `rack.instrument`, `rack.audio`, `rack.midi`, `rack.drum`; anidados; Cmd+G agrupa y Cmd+Shift+G desagrupa (`editing/racks.rs`). |
| Cadenas en paralelo con volumen, pan, mute, solo | ✅ | `Chain` (`model/src/project.rs:1115`); tira de cadena en `views/devices/rack_ui.rs`. |
| Zonas Key / Velocity / Chain Select con **fades** | ✅ | `key_range`, `velocity_range`, `selector_range` + `key_fade`, `vel_fade`, `sel_fade`, `chain_selector`; panel de zonas en `rack_ui.rs`; cadenas dormidas en el motor (commit `fd1718c`). |
| Drum Rack: Pad View con paginación, audición, choke 1-16, Receive/Play, hasta 6 returns con envíos | ✅ | `Chain.choke`, `play_note`, `sends`, `output`, `Rack.returns` (`MAX_RETURNS = 6`); rejilla 4×4 con banco (`default_bank`); la audición suena por la pista y el auricular reproduce el archivo del pad. "Slice to Drum Rack" (`editing/slice.rs`). |
| Macros: 16 (8 visibles por defecto), Map mode, rango min/max invertible, Rand, variaciones con exclusión | ✅ | `MACRO_COUNT = 16` (`project.rs:25`); comandos `AddMapping`, `RemoveMapping`, `SetMappingRange`, `SetVariations` (`command.rs:187-212`); `map_pick`, `randomize_macros`, `recall_variation_except`. |
| 32 macros, curva de mapeo por breakpoints, modo Mod (13β) | ❌ | `MACRO_COUNT` fijo en 16; `MacroMapping` solo tiene `min`/`max`. |
| Round Robin en Instrument y MIDI Effect Racks (13β) | ❌ | No hay campo ni lógica. |
| Medidores entre dispositivos (13β) | ❌ | SUPUESTO (no se buscó). |
| **Mid-Side Rack** y **Multiband Rack** (13β) | ❌ | No hay plantillas (grep sin resultados). Existen las piezas: `fx.midside`, `fx.multiband`. Falta una forma de que cada cadena procese una banda (§5, B6). |
| **Presets de rack** (Core Library: p. ej. "Vocal Strip Complete", "Guitar Amp Stack", 12.4) y guardar cadenas como preset | ❌ | Los presets de usuario guardan parámetros de un dispositivo (`device_window/chrome.rs:53`, `snapshot()` lee los `ParamInfo` del plugin); un rack no tiene parámetros, y no hay formato de rack ni biblioteca. |
| Hot-swap, Auto Select | SUPUESTO | No se comprobó. |

### 3.6 Presets, A/B y flujo de dispositivo

| Capacidad | Estado | Propio [V] |
|---|---|---|
| Presets de fábrica | ✅ | 853 en 47 dispositivos; formato `presets/<id>/<Categoría>/<Nombre>.json` con valores normalizados por id (`stock/src/presets.rs`). |
| Presets de usuario | ✅ | `user_presets/<plugin>/<Nombre>.json` en la carpeta de configuración (`device_window/chrome.rs:53`); tarjeta "Save Preset" (`PopKind::SavePreset`). |
| **A/B de dispositivo** (12.3) | ✅ | Ranuras A/B en `device_window/presets.rs:151` (`Ab`). |
| Sidechain por dispositivo | 🟡 | `Device.sidechain: Option<TrackId>` (`project.rs:1019`): toma fija tras los dispositivos y antes del fader (`graph.rs:1518`), sin punto de toma elegible y sin fuente en una cadena de rack; sin compensación de latencia: SUPUESTO (en `compile.rs` los `side_slots` son buffers simples y no se vio ningún retardo, pero no se leyó el compilador entero). Con `has_sidechain`: `fx.compressor`, `fx.buscomp`, `fx.mbdyn`, `fx.gate`, `fx.eq`, `fx.vocoder`, `instrument.granular`. Sin él: `fx.filter`, `fx.freqshift`, `fx.transient`… |

### 3.7 Fuera de alcance (solo anotado)

- **Max for Live (cap. 33 del manual de 12)**: DS Clang, DS Clap, DS Cymbal, DS FM, DS HH, DS Kick, DS Snare, DS Tom; Align Delay, Envelope Follower, LFO, Shaper; Envelope MIDI, Expression Control, MIDI Monitor, MPE Control, Note Echo, Shaper MIDI. Más, en Suite, PitchLoop89, Convolution Reverb, Color Limiter, Gated Delay, Pitch Hack, Re-Enveloper, Spectral Blur, Bass, Poli, y las MIDI Tools Velocity Shaper y Euclidean.
- **Packs de pago o gratuitos de Live** (página de ediciones): Beats Tools, Drone Lab, Drum Booth, Electric Keyboards, Expressive Chords, Generators by Iftah, Latin Percussion, Lost and Found, MIDI Tools by Philip Meyer, Surround Panner, Inspired by Nature, Orchestral (Brass, Mallets, Strings, Woodwinds), Performance Pack, Sequencers, Synth Essentials, CV Tools, y los nuevos de 13β: **Looplet** y **Classic Dynamics & EQ by Softube**.
- No se compite con estos: lo que AI-Track ya cubre por función se indica en las tablas.

---

## 4. Estados obsoletos de 13, 18, 20 y 21 [V salvo indicación]

Lo cambió sobre todo el commit `fd1718c` ("recording, racks, MIDI effects, six devices…", 2026-11-15 11:46), cuyo mensaje dice que ejecuta "the first three waves of the roadmap" de 20, que entonces decía "Nada está commiteado". Además: `17c8443` (moduladores LFO y Envelope Follower), `d0c53b8` (Slice to Drum Rack y MIDI Effect Rack), `d4690e2` (pitch shifter, frequency shifter, vocoder), `dc2cbf2` (granulador), `7e48296` (Capture Automation/MIDI), `522aa8e` (locators y Bounce), `c255122` (warp markers), `c95588e` y siguientes (Bloom).

### Informe 13 (paridad funcional)

| Estado que decía | Hoy |
|---|---|
| §1.5 "Dispositivos MIDI: **ausente** (`stock::REGISTRY` no registra ninguno)" | **Obsoleto**: 8 efectos MIDI en el `REGISTRY` (`lib.rs:80-87`), `rack.midi` y marco `process_midi`. |
| §1.6 "Guardar presets de usuario / guardar rack / plantillas: **ausente**" | **Parcial**: presets de usuario sí (`chrome.rs:53`); racks y plantillas de pista siguen sin guardarse. |
| §1.6 "A/B de dispositivo (Live 12.3): **ausente**" | **Hecho** (`presets.rs:151`). |
| §1.6 Racks "completo (salvo contenido MIDI)" | **Completo también en MIDI**; ver 3.5. |
| §3 "Auto Pan-Tremolo: lo cubre parcialmente Shaper" | **Hecho**: `fx.autopan`. |
| §3 "`Filter` propio sin tipos Comb/Vowel/DJ (SUPUESTO)" | **Verificado**: tipos LP/HP/BP/Notch/Peak + 4 ladders; sin Comb/Vowel/DJ/Morph (`fx/filter.rs:43-44`). |
| §3 "Drum Synth parcial (`instrument::drum`, `kick808`)" | Sigue parcial; ahora también `instrument.bloom`. |
| §3 "Capture Automation: **ausente**" | **Hecho** (`Cmd::CaptureAutomation`, `keys.rs:498`, commit `7e48296`). |
| §1.2-1.4, 1.6, 1.9 (grabación, entrada MIDI, Session editable, borrar pista, routing de E/S, autosave, relink, freeze, bounce, locators, take lanes, warp markers, arrastrar archivos) | **Hechos** en su mayoría: ver la tabla de 24/03 §1 y los commits de arriba. (No son de dispositivos; no se re-verificó cada uno.) |
| §1.5 "Herramientas MIDI: parcial" | **Sigue igual** (3.4). |
| §3 "macros 16 vs 32" | Sigue (13β). |

### Informe 18 (dispositivos nativos y racks)

| Estado que decía | Hoy |
|---|---|
| §0.2 "Efectos MIDI: ausentes de verdad; `ProcessContext` sin salida" | **Obsoleto**: `Processor::process_midi`, `EventSink`, `MidiContext` (`dsp/src/midi.rs`), 8 dispositivos. |
| §0.3 "20 efectos + 6 instrumentos, ≈ 450 presets" | **47 dispositivos y 853 presets**. |
| §0.4 y §B.2: Auto Pan-Tremolo ausente; Redux, Glue parciales; OTT y Beat Repeat ausentes; Drum Buss ausente; Shifter y Vocoder ausentes | **Todos ✅**: `fx.autopan`, `fx.crusher`, `fx.buscomp`, `fx.mbdyn`, `fx.stutter`, `fx.drumbus`, `fx.freqshift` + `fx.pitchshift`, `fx.vocoder`. |
| §B.3 Arpeggiator, Chord, Scale, Velocity, Pitch, Random, Note Length, Note Echo, MIDI Effect Rack: ausentes | **Existen**: ✅ Arpeggiator, Scale, Velocity, Random, Note Echo y MIDI Effect Rack; 🟡 Chord (sin Strum/Chance/Learn), Note Length (sin disparo por Note Off ni Latch) y Pitch (sin grados de escala). CC Control, MIDI Monitor, MPE Control, Melodic Steps y Rotating Rhythm Generator **siguen ❌**. |
| §B.4 "Granulator III: ausente" | **Hecho**: `instrument.granular`. |
| §A.3 "No hay UI para mapear macros; pads sin clic; zonas sin UI; `selector_range` sin uso; MIDI Rack vacío" | **Hechos**: Map mode, pads con audición, panel de zonas con fades, Chain Select en el motor, `rack.midi`. |
| §D.1 Choke entre cadenas, Receive/Play, returns y envíos, fades de zona, Randomize, exclusión en variaciones | **Hechos**. |
| §D.1 "Hasta 32 macros", curvas, modo Mod | **Siguen ausentes**. |
| §D.3.5 Sidechain: punto de toma, fuente en cadena de rack, PDC | **Siguen ausentes**. |
| Parte E, trabajos 1-16 y 21 (granular) | **Hechos**. Siguen abiertos: 18 (Auto Filter nuevo), 19 (plantillas Mid-Side/Multiband), 20-22 (importador `.adg`: no hay `crates/liveimport`), `fx.driver`, Looper, Echo ampliado, Meld, Spectral, Collision/Tension. |

### Informe 20 (hoja de ruta)

| Trabajo | Hoy |
|---|---|
| Ola 2: 2.1-2.12 (`autopan`, `crusher`, `buscomp`, `mbdyn`, `stutter`, comandos de rack, pads, mapeo de macros, Drum Rack real, zonas/Chain Select, returns, `drumbus`) | **Hechos** (la columna "Estado" del informe, "VERIFICADO (ausencia)", quedó obsoleta). |
| 2.13 Macros en vivo, 8/16/32, Rand, variaciones con exclusión | **Parcial**: camino rápido (commit `fd1718c`), Rand y exclusión hechos; **32 macros no**. |
| Ola 4: 4.1 (marco MIDI), 4.2 (los 8 `midi.*`), 4.5 (Freeze/Bounce), 4.6 (warp markers), 4.7 (locators), 4.11 (take lanes) | **Hechos**. |
| 4.3 (importador `.adg`), 4.9 (sidechain), 4.12 (Auto Filter ampliado, plantillas M-S/Multiband, `fx.driver`, Looper) | **Siguen abiertos**; 4.12 hecho a medias: `fx.freqshift`, `fx.pitchshift`, `fx.vocoder`, `instrument.granular`. |
| Ola 0 y Ola 1 | Casi todas hechas (24/03 §1). |
| §3 "Plan de paralelización" (carriles) | Obsoleto como plan inicial. |

### Informe 21 (referencias de UI de los seis dispositivos)

Ya hecho desde 21: stutter con carriles apilados, nivel/stop/filtro por paso, ligaduras, swing, rampas From-To, Randomize/Mutate y Hold (`stutter.rs` ids `SWING = 26`, `PITCH_END`, `FILTER_END`, `LEVEL_END`, `STEP_LEVEL/STOP/FILTER`, `TIE_BASE`); autopan con asas de Phase/Stereo/Amount; crusher con asa de DC Shift y Nyquist; drumbus con Force To Note; mbdyn con asas Alt de ratio y pico por banda; buscomp con panel de sidechain y respuesta del HPF. **Sigue pendiente (DSP)**: crusher Dither/Drive/mid-rise; buscomp auto-makeup, EQ de sidechain y link; drumbus Boom Audition, Delta, Guard, Attack/Sustain; autopan retrigger, onda dibujada, Haas; mbdyn número de bandas, link, lookahead, espectro; stutter escenas, disparo MIDI/audio y ducking al bombo.

---

## 5. (c) Backlog priorizado para un productor de música electrónica

### 5.1 Reglas de parámetros stock que todo el backlog respeta

1. `choices` mide **exactamente** `max - min + 1`. Ampliar una lista cambia `max` y, con ello, el valor normalizado que ya guardan los presets: **no se amplía**; se añade un id nuevo (precedentes: `fx.filter` `LADDER = 16`; `fx.stutter` `SWING = 26` y los ids `100+`).
2. Los presets guardan el valor **normalizado por id**. Nunca se renumera ni se cambia el rango de un id existente; un id nuevo toma un valor por defecto que deja el sonido de los presets viejos igual.
3. `gen` es palabra reservada (edición 2024): no se usa como nombre de variable, campo ni parámetro (`generator`, `gener`…).
4. `meter()` ≤ 320 valores. Sin asignaciones, locks, strings ni E/S en `process`. Los efectos MIDI son deterministas por posición de canción (hash de semilla + posición), como los ocho actuales.
5. Toda edición del proyecto es un `Command`. Las herramientas MIDI y las plantillas de rack devuelven `Vec<Command>` (un solo paso de undo).
6. Un atajo nuevo exige fila en `keys.rs` (TABLE). Los catálogos "stub" no traen presets (la pestaña Presets requiere el modo real).
7. **Diseños originales**: ni nombres comerciales (Roar, OTT, Drum Buss, Looplet, Softube…) ni el aspecto de plugins de terceros en UI o presets. El DSP es propio; los repos GPL de `reference/` se estudian, no se copian; MIT/Apache con aviso en `NOTICE` si algún bloque queda cercano.

### 5.2 Plantilla de ficheros de un dispositivo stock nuevo [V, a partir de `fx.crusher`]

`crates/stock/src/fx/<x>.rs` (+ `fx/mod.rs`) · `REGISTRY` en `crates/stock/src/lib.rs` · `crates/stock/presets/<id>/<Categoría>/*.json` (los embebe `crates/stock/build.rs`) · `crates/app/src/views/device_ui/<x>.rs` (+ `mod`, el id en `has_editor()` de `device_ui/mod.rs:713` y el brazo en `editor()` de `:767`; sin lo primero el dispositivo cae en la rejilla de mandos) · `views/palette/devices.rs` (`stock_group`) · `views/palette/glyphs.rs` (glifo) · `views/device_window/placement.rs` (tamaño de ventana). Los tests de `crates/stock/tests/{robustness,alloc,presets,bench}.rs` recorren el `REGISTRY` solos.

### 5.3 Marcos y dependencias

| Marco | Estado hoy | Lo necesitan |
|---|---|---|
| **MIDI FX** (`process_midi`) | ✅ existe | todos los `midi.*` |
| **Módulos `mod.*`** (`Processor::modulate`, `ModMapping`, máx. 8 destinos) | ✅ existe | B12, B24 |
| **D-TOOLS**: panel de MIDI Tools con Auto Apply y previsualización | ❌ | B2, B3 |
| **D-RACKFMT**: formato portátil de rack (cadenas, mapeos, variaciones) con remapeo de `DeviceId` | ❌ | B1, B6 |
| **D-SC**: punto de toma, fuente en cadena de rack, PDC del sidechain | ❌ | B11 (y B4 en parte) |
| **D-IR**: convolución particionada en `dsp` (`realfft` ya es dependencia de `dsp` y `stock`) | ❌ | B7, cabina de `fx.amp`, Cabinet |
| **D-F0**: seguidor de f0 compartido (hoy dentro de `fx.vocoder`) | parcial | Auto Shift, Tuner |
| **D-STFT**: marco espectral para efectos | ❌ | Spectral Time/Resonator |
| **D-HWIO**: salida MIDI y E/S multicanal reales (`Output::Hardware(_) => Dest::Out` mezcla todo en un bus, `compile.rs:122`) | ❌ | External Instrument/Audio Effect, CC Control |

### 5.4 Tabla

Cada fila: id de trabajo, qué, **por qué importa a la electrónica**, esfuerzo, dependencias, ficheros. Los ficheros usan la plantilla de 5.2 salvo que se diga otra cosa.

| # | Prio | Trabajo | Esf. | Depende de | Ficheros |
|---|---|---|---|---|---|
| B1 | **P0** | **Presets de rack**: guardar y cargar un rack entero (cadenas, macros, mapeos, variaciones, zonas) y una **biblioteca de racks de fábrica** (≥ 20: sidechain pump, parallel crush, drum bus, vocal chain, bass split…). Sin esto los racks no son reutilizables y el "Core Library" de Live no tiene contrapartida. | M-L (formato M; contenido L) | D-RACKFMT | `model/src/racks.rs` (`RackPreset` y constructor de `Vec<Command>` como `group_commands`), `app/src/views/device_window/{chrome,presets}.rs`, `editing/racks.rs`, `palette/devices.rs` (sección Racks), `stock/presets/rack.*/` (+ el `build.rs` que los embebe) |
| B2 | **P0** | **Panel de MIDI Tools** (Transformar y Generar) con parámetros, Auto Apply, Reset y vista previa, sobre `UpdateNotes`; primeras herramientas: Arpeggiate, Strum (Tension), Quantize (cantidad), Span, Chop (partes), Stacks (banco de acordes), Rhythm y Seed. Es la novedad grande de Live 12 y hoy son acciones ciegas. | L | D-TOOLS | `model/src/midi_tools/` (nuevo; funciones puras, deterministas por semilla), `app/src/state.rs`, `views/piano_roll.rs` (panel), `views/menus.rs` (`tools_menu` pasa a abrir el panel), `keys.rs` + `actions.rs` |
| B3 | P1 | Resto de MIDI Tools: Ornament, Connect, Recombine, Time Warp, Shape, y generadores propios equivalentes a Euclidean y Velocity Shaper | M (S-M cada una) | B2 | `model/src/midi_tools/*` |
| B4 | P1 | **`fx.filter` → Auto Filter 12.2**: tipos DJ, Comb, Vowel, Morph, Resampling y Notch+LP en un **id nuevo** (`EXT_TYPE`, Off = comportamiento actual), formas LFO Wander/Ramp, y **sidechain** para el seguidor (`has_sidechain: true`). | M | sidechain por pista (ya existe) | `stock/src/fx/filter.rs`, `dsp/src/lfo.rs` (formas con ids nuevos), `device_ui/filter.rs`, presets nuevos; sin tocar `REGISTRY` |
| B5 | P1 | **Racks 13β**: 32 macros (`MACRO_COUNT` 16→32 con `serde(default)`), curva de mapeo por breakpoints y modo Mod en `MacroMapping` (campos nuevos con default), banco 2 de macros en la UI. | M | — | `model/src/project.rs`, `model/src/command.rs` (`SetMappingCurve`), `engine/src/graph.rs` (`ParamTarget::Macro`), `views/devices/rack_ui.rs` |
| B6 | P1 | **Plantillas Mid-Side y Multiband** (builders de `Vec<Command>`). El Multiband necesita que cada cadena procese una banda: dispositivo nuevo `fx.bandsplit` (LR4; 24 dB fase mínima, 6 dB, 24 dB lineal con latencia) o zona de frecuencia por cadena. | M | D-RACKFMT (B1) | `model/src/racks.rs`, `stock/src/fx/bandsplit.rs` (+ plantilla 5.2), `app/src/editing/racks.rs` |
| B7 | P1 | **Motor de convolución** (`dsp::convolve`, FFT particionada) y **`fx.convolver`** (IR + motor algorítmico de `fx.reverb`, EQ y mezcla; carga de IR por `load_state`); la cabina de `fx.amp` podrá cargar IR. IR de fábrica **generadas por nosotros**, sin muestras de terceros. | L | D-IR | `dsp/src/convolve.rs` (nuevo), `stock/src/fx/convolver.rs` (+ plantilla 5.2) |
| B8 | P1 | **`instrument.drumsynth`** (Drum Synth 13β): 9 tipos × modelos, modo kit (notas de pad) y monofónico, ≥ 40 presets. Es la pieza de percusión más visible de 13β. | L | motores de `kick808` y de Bloom (`voice.rs`) | `stock/src/instrument/drumsynth/` (+ plantilla 5.2) |
| B9 | P1 | **`instrument.sampler` con Slicing, warp y efectos de reproducción por muestra** (Drum Sampler / Simpler): ids nuevos (`MODES` mide 3 y no se amplía); los 9 efectos de reproducción de Live son la referencia de función (Stretch, Loop, Pitch Env, Punch, 8-Bit, FM, Ring Mod, Sub Osc, Noise Osc; implementación propia), más envolvente de tono y "Envelope Follows Pitch". | M-L | `dsp::warp`, `model::slice` | `stock/src/instrument/sampler.rs`, `device_ui/sampler.rs`, `app/src/editing/slice.rs` |
| B10 | P1 | **`fx.driver`** (equivalente de Roar con diseño propio): hasta 3 etapas, rutas Serial/Parallel/Multiband/M-S/Feedback con línea afinable, 2 LFO + seguidor, compresión de salida. Bass music. | M-L | `dsp::{shaper::Adaa1, crossover, delay}` | `stock/src/fx/driver.rs` (+ plantilla 5.2) |
| B11 | P1 | **Sidechain completo**: punto de toma (Pre FX / Post FX / Post fader), fuente = cadena o pad de un rack, compensación de latencia; y `has_sidechain` en `fx.freqshift` y `fx.transient`. | M | D-SC | `model/src/project.rs` (`Device.sidechain` → estructura con `serde(default)`), `engine/src/{compile,graph}.rs`, cabecera de dispositivo en `device_ui/mod.rs` |
| B12 | P1 | **`mod.shaper`**: Shaper como modulador (envolvente de breakpoints sincronizada o disparada por nota, hasta 8 destinos). El bombeo sobre cualquier parámetro es herramienta diaria. | S-M | `mod.*` (existe), curva de `fx/shaper.rs` | `stock/src/fx/shaper_mod.rs` (id `mod.shaper`), `device_ui/modulator.rs`; la paleta ya enruta `mod.` |
| B13 | P1 | **`midi.chord`: Strum, Chance por voz y Learn** (ids nuevos). El Strum es lo más usado en house. | S | — | `stock/src/midi/chord.rs`, `device_ui/chord.rs` |
| B14 | P1 | **External Instrument y External Audio Effect** (hardware de techno y house): salida MIDI (hoy no existe: grep sin resultados), pares de salida de audio reales (hoy `Output::Hardware(_)` mezcla todo en un bus), entrada con compensación de latencia. | L | D-HWIO | `engine/src/{compile,midi_io,audio_io}.rs`, `app/src/mixer_routing.rs`, `stock/src/instrument/external.rs` y `fx/external.rs` (+ plantilla 5.2) |
| B15 | P2 | Remates de 21 (DSP): crusher Dither/Drive; buscomp auto-makeup y EQ de sidechain; drumbus Boom Audition/Delta; autopan retrigger y Haas; mbdyn link y lookahead; stutter escenas y disparo externo. | M (en total) | D-SC para el disparo | los seis `fx/*.rs` y sus `device_ui/*.rs` |
| B16 | P2 | **`fx.looper`** (sin export a clip en la primera entrega). | M | transporte | plantilla 5.2 |
| B17 | P2 | **`fx.autoshift`** (corrección a escala, formantes) y **`fx.tuner`**. | L (Auto Shift) / S (Tuner) | D-F0, formantes | `dsp/src/pitch.rs` (nuevo: extraer el f0 del vocoder), plantilla 5.2 |
| B18 | P2 | **`fx.erosion`** y **`fx.vinyl`** (lo-fi y desgaste, básicos de house/lo-fi). | S cada uno | — | plantilla 5.2 |
| B19 | P2 | **`fx.resonators`** (5 resonadores afinados) y **`fx.graindelay`** (planificador de granos de `instrument.granular` sobre una línea de retardo). | M cada uno | — | plantilla 5.2 |
| B20 | P2 | **`fx.eq3`** (EQ de DJ con kill) y vista simple de Channel EQ. | S | — | plantilla 5.2 |
| B21 | P2 | **`fx.delay`: reverb en el lazo, Noise, Wobble y gate** (Echo), 3 líneas con filtro (Filter Delay): **ids nuevos**. | M | — | `stock/src/fx/delay.rs`, `device_ui/delay.rs` |
| B22 | P2 | **`fx.chorus`: Vibrato, Doubler, 2.º LFO y seguidor** (ids nuevos). | S-M | — | `stock/src/fx/chorus.rs`, `device_ui/chorus.rs` |
| B23 | P2 | **`mod.envmidi`, `mod.expression`, `mod.shapermidi`** (equivalentes a los moduladores MIDI de Live, que allí son Max for Live). | S cada uno | `mod.*` | plantilla 5.2 |
| B24 | P2 | **Round Robin** en Instrument y MIDI Effect Racks (13β). | S-M | — | `model/src/project.rs` (`Rack`), `engine/src/graph.rs`, `rack_ui.rs` |
| B25 | P2 | **`instrument.meld`** (dos osciladores macro con modulación; diseño propio) y **editor FM** estilo envolventes por operador sobre el motor FM6. | L / M | `instrument.wavetable`, `vault/fm.rs` | `stock/src/instrument/meld/` (+ plantilla 5.2); `device_window/vault/` |
| B26 | P2 | **`midi.pitch` con grados de escala**; **`midi.length` con disparo por Note Off y Latch**; **CC Control** (`midi.cc`). | S cada uno (CC Control: M) | D-HWIO para CC Control | `stock/src/midi/*.rs` |
| B27 | P3 | Física: **Collision-like** y **Tension-like** (reutilizar el resonador modal de Bloom); `fx.corpus`. | L cada uno | — | plantilla 5.2 |
| B28 | P3 | **Spectral Time / Resonator / Blur** | L | D-STFT | plantilla 5.2 |
| B29 | P3 | Dynamic Tube (bias con seguidor en `fx.saturator`, ids nuevos), Cabinet suelto (IR, B7), Channel EQ extra. | S / M | B7 | `stock/src/fx/saturator*.rs` |
| B30 | P3 | MPE Control, MIDI Monitor, Melodic Steps, Rotating Rhythm Generator (como dispositivos o como generadores de B2). | S-M cada uno | B2 | `stock/src/midi/*.rs` |
| B31 | P3 | **Sistemas de afinación** (`.scl`/`.ascl`) en Scale, Chord, Arpeggiator e instrumentos (12.0-12.2). | M | — | `model/src/scale.rs`, `midi/theory.rs` |
| B32 | P3 | MIDI Tools MPE (Glissando, LFO): exigen editar expresión por nota en el piano roll. | L | B2 + editor de expresión | `views/piano_roll.rs` |
| B33 | P3 | Importador de `.adg` (decisión D5 de 20: solo con racks reales del dueño). | L | B1, archivos del dueño | nuevo `crates/liveimport` |

### 5.5 Orden sugerido

1. En paralelo, sin conflictos de archivos: **B1** (modelo + app de racks), **B2** (modelo + piano roll), **B4** (stock `fx/filter.rs`), **B13** (stock `midi/chord.rs`), **B12** (stock). Cada uno es un carril propio.
2. Después: **B5** y **B6** (tocan `project.rs` y racks: van en serie tras B1), **B11** (tocan `compile.rs`/`graph.rs`: un solo agente), **B9**, **B10**.
3. Piezas grandes de sonido: **B7** y **B8** (L), en carriles independientes.
4. P2 y P3 por valor; **B14** puede subir a P0 si el dueño usa hardware.

---

## 6. Qué no se pudo verificar

- Nada se ejecutó: ni `cargo`, ni los tests, ni la app, ni se escuchó un solo dispositivo. Las cifras de presets salen de contar archivos; los parámetros, de las cabeceras de módulo y de listas de ids.
- La lista de controles de Echo, Auto Shift, Corpus, Erosion, Drum Sampler y Meld sale de los resúmenes que devolvió la lectura del manual; algunos capítulos se cortaron (Filter Delay; Meld desde 31.8.11).
- "Hot-swap" y "Auto Select" de racks, y los "medidores entre dispositivos" de 13β: SUPUESTO (no se buscaron).
- Los parámetros Time y Taps de Chorus-Ensemble (12.4): las notas no los detallan. Tampoco se comprobó si el motor FM de `instrument.vault` tiene filtro y LFO propios ni si `instrument.sampler` lleva editor de zonas (SUPUESTO, de 18).
- Los recuentos de la página de ediciones (21 instrumentos, 62 efectos de audio, 14 herramientas MIDI, 14 efectos MIDI, 6 moduladores) son **recuento hecho al leer la página**, no cifras de Ableton.
- Si Bass, Poli y Melodic Steps son Max for Live: SUPUESTO (la página de ediciones no los etiqueta). Rotating Rhythm Generator sí es del Pack CV Tools.
- El año de origen de los dispositivos "≤11" no se comprobó.
- Live 13 es una beta: todo lo marcado 13β puede cambiar antes del lanzamiento de "early 2027".

## 7. Fuentes

- Notas de versión de Live 12 (12.0 a 12.4.16): https://www.ableton.com/en/release-notes/live-12/
- Notas de la beta de Live 13 (13.0b25, 13.0b26): https://www.ableton.com/en/release-notes/live-13-beta/
- "What's coming in Live 13" (10-nov-2026): https://www.ableton.com/en/blog/live-13-is-coming/
- "What's new in Live 13": https://www.ableton.com/en/live/all-new-features/
- Comparación de ediciones (rotulada Live 13): https://www.ableton.com/en/live/compare-editions/
- Live 12 llega el 5-mar-2024: https://www.ableton.com/en/blog/ableton-live-12-coming-march-5/ · Live 12.4 publicado: https://www.ableton.com/en/blog/live-12-4-is-out-now/
- Manual de Live 12: índice https://www.ableton.com/en/live-manual/12/ · MIDI Tools https://www.ableton.com/en/live-manual/12/midi-tools/ · Racks https://www.ableton.com/en/live-manual/12/instrument-drum-and-effect-racks/ · Efectos de audio https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/ · Efectos MIDI https://www.ableton.com/en/live-manual/12/live-midi-effect-reference/ · Instrumentos https://www.ableton.com/en/live-manual/12/live-instrument-reference/ · Max for Live Devices https://www.ableton.com/en/live-manual/12/max-for-live-devices/
- Rotating Rhythm Generator (Pack CV Tools): https://www.ableton.com/en/packs/cv-tools/
- Informes internos: `docs/research/13-auditoria-paridad-ableton.md`, `18-dispositivos-nativos-racks.md`, `20-hoja-de-ruta.md`, `21-referencias-ui-dispositivos.md`, `24-investigacion/03-ableton-ui-ux.md`.
