# 18 · Dispositivos nativos propios y racks (equivalentes de la suite de Ableton Live)

> Fecha: 2026-11-15. Encargo del dueño: tener como dispositivos PROPIOS (Rust, `crates/stock`) los equivalentes de los dispositivos nativos de Ableton Live Suite (Auto Pan, Redux, Glue Compressor, OTT/Multiband Dynamics, herramientas de stutter house, instrumentos) y que los RACKS funcionen como en Live (cadenas paralelas, zonas, macros, variaciones, Drum Rack).
> Nada de este informe modifica código. No se copia código ni apariencia de productos comerciales: se replica la FUNCIÓN y el flujo de trabajo, con nombres, interfaz y DSP propios.

**Leyenda de fiabilidad.** `[V]` = verificado: ruta:línea del repo (leída en esta sesión) o URL (descargada en esta sesión). `[S]` = supuesto, de memoria o inferido; hay que confirmarlo antes de apoyarse en ello.

---

## 0. Resumen ejecutivo

1. **Los racks ya existen y funcionan más de lo que se creía** `[V]`: modelo (cadenas, 16 macros, variaciones, zonas), comandos, motor (cadenas paralelas con suma y compensación de latencia entre cadenas) y UI (lista de cadenas, panel de macros, variaciones, rejilla de pads). Lo que falta es **la mitad "profunda"**: mapear macros desde la UI, zonas de *chain select* y *fades*, choke/`Play note`/returns del Drum Rack, y **efectos MIDI** (no existe ninguno ni el motor puede encadenarlos).
2. **Efectos MIDI: ausentes de verdad** `[V]`. `Category::MidiEffect` existe en el enum (`crates/dsp/src/lib.rs:75-81`) pero ningún plugin lo usa, y `ProcessContext` solo recibe eventos (`events: &[Event]`, `lib.rs:63-71`), sin salida. Todos los dispositivos de una cadena ven el MISMO slice de eventos (`crates/engine/src/graph.rs:138-189`). Hace falta ampliar el contrato `Processor` y `process_devices` (L).
3. **Inventario de stock**: 20 efectos + 6 instrumentos propios, cada uno con editor propio (ninguno cae al editor genérico salvo los no listados) y presets de fábrica (≈ 450 en total).
4. **De la lista citada por el dueño**: Auto Pan-Tremolo = ausente (solo se aproxima con `fx.shaper`); Redux = parcial (estilo "Bitcrush" dentro de `fx.saturator`, sin jitter ni filtros); Glue = parcial (estilo "Glue" del `fx.compressor`, sin pasos fijos ni soft-clip); OTT/Multiband Dynamics = **ausente** (`fx.multiband` es solo un divisor con ganancia/mute/solo); Beat Repeat/stutter = ausente.
5. **Importar .adg/.adv**: viable y de riesgo medio. Contenedor = gzip + XML `[V]` (fuentes en §D.8); `flate2` y `roxmltree` ya están en `Cargo.lock` con licencias permitidas `[V]`. Lo que se mapea bien: estructura de rack (cadenas, macros, zonas, nombres) y ~12 dispositivos de efecto con equivalente directo. Lo que no: samples de Packs, Max for Live, estados opacos de plugins, y casi todos los instrumentos de Live. Sin un .adg real a mano para fixtures no se debe empezar (no hay ninguno en este equipo, §D.8).

---

# PARTE A · Inventario real (código leído)

## A.1 Registro de stock

Fuente: `crates/stock/src/lib.rs:37-62` `[V]`. Presets: `crates/stock/presets/<id>/` (conteo propio, ver tabla). "Params" = número de parámetros guardados en el preset más completo (los presets guardan todos los ids, ver `presets.rs:3-9`), por tanto el valor real puede ser mayor en dispositivos con parámetros dinámicos.

### Efectos

| id | Qué es / parámetros principales | Editor propio (`views/device_ui/`) | Presets | Params |
|---|---|---|---|---|
| `fx.eq` | EQ 8 bandas, dinámico, M/S, fase mínima/natural/lineal (`fx/eq/mod.rs:1-40`) | `eq.rs`, `eq/` | 22 | 133 |
| `fx.compressor` | Umbral, ratio, rodilla, ataque, release/auto, estilos Clean/Punch/**Glue**/Opto, modelos VCA/Vari-Mu, feedback/feed-forward, lookahead, sidechain (SC HPF/LPF/EQ/listen), range, makeup, mix (`fx/compressor.rs:1-60`, ids `:55-110`) | `dynamics.rs` | 17 | 23 |
| `fx.limiter` | Limitador lookahead, true peak 4×, 6 estilos, M/S, dither, LUFS (`fx/limiter.rs:1-37`) | `limiter.rs` | 8 | 8+ |
| `fx.gate` | Gate/expansor/ducker con histéresis, hold, lookahead (`fx/gate.rs:1-25`) | `dynamics.rs` | 8 | 12 |
| `fx.transient` | Attack/Sustain, ventanas, clip, mix (`fx/transient.rs`) | `dynamics.rs` | 8 | 7 |
| `fx.clipper` | Clipper de máster, ADAA, 1×/2×/4×, lift, delta (`fx/clipper.rs:1-25`) | `clipper.rs` | 8 | 6 |
| `fx.saturator` | Saturación multibanda (hasta 3 bandas LR4); estilos Tape, Diode, Tube, Transistor, Clip, Rectify, Fold, Tri Fold, **Bitcrush**; ADAA1 + oversampling (`fx/saturator.rs:1-30`) | `saturator.rs` | 18 | 26 |
| `fx.tape` | Cinta: histéresis, wow/flutter, head bump (`fx/saturator/tape.rs`) | `saturator/` | 10 | 13 |
| `fx.amp` | Preamp/tone stack/cabina, 4 modos, 4 cabinas (`fx/saturator/amp.rs:68-79`) | `saturator/` | 11 | 12 |
| `fx.console` | Bus/channel summing, Glue/Iron (`fx/saturator/console.rs:56-62`) | `saturator/` | 8 | 7 |
| `fx.multiband` | **Solo divisor de 3 bandas** (LR4 o FIR lineal) con ganancia/mute/solo/delta por banda; NO tiene dinámica (`fx/multiband.rs:1-3`, ids `:26-57`) | `multiband.rs` | 8 | 16 |
| `fx.midside` | Ganancias Mid/Side, solos, width | `midside.rs` | 8 | 5 |
| `fx.reverb` | FDN 8/16 líneas + placa Dattorro, shimmer, freeze, ducking, algoritmos y "colour" (`fx/reverb.rs:1-40`) | `reverb.rs` | 30 | 24 |
| `fx.delay` | Digital/Tape/BBD, sync recto/puntillo/tresillo, multitap, difusión, ducking, freeze (`fx/delay.rs:1-30`) | `delay.rs` | 23 | 23 |
| `fx.filter` | SVF 12/24 dB LP/HP/BP/Notch/Peak, drive, LFO (7 formas, sync), seguidor de envolvente (`fx/filter.rs:1-60`) | `filter.rs` | 11 | 16 |
| `fx.chorus` | Chorus (2-6 voces), flanger y phaser (2-12 etapas), sync (`fx/chorus.rs:1-12`) | `chorus.rs` | 9 | 13 |
| `fx.shaper` | Curva de 6 nodos sincronizada al tempo o disparada por audio, destino Volume / Filter LP / Pan (`fx/shaper.rs:1-10`) | `shaper.rs` | 10 | 15 |
| `fx.utility` | Gain, pan, width, mono, bass mono, polaridad, swap, DC (`fx/utility.rs:1-12`) | `utility.rs` | 10 | 12 |
| `fx.analyzer` | Espectro, LUFS BS.1770, true peak, correlación (`fx/analyzer.rs:1-20`) | `analyzer.rs` | 6 | 5 |
| `fx.drum_extract` | Extractor de batería (HPSS + de-bleed/de-reverb) | `drum_extract.rs` | 6 | 7 |

### Instrumentos

| id | Qué es | Editor | Presets |
|---|---|---|---|
| `instrument.wavetable` | Sinte wavetable de 16 voces: 3 osc + sub + ruido, unison 16, 2 filtros (SVF/ladder/comb/formant…), 3 envolventes, 4 LFO, matriz de 32 slots, FX internos (`instrument/wavetable/mod.rs:1-60`) | `wavetable.rs` | 81 |
| `instrument.vault` | Siete motores por preset: Mono Ladder, Poly Analog, FM6 (6 operadores, 12 algoritmos), Piano eléctrico, Órgano, String machine, Wavetable; 4 macros (`instrument/vault/mod.rs:1-40`) | `vault.rs`, `device_window/vault/` | 83 |
| `instrument.acid303` | Bajo monofónico acid con ladder de diodos y secuenciador de 16 pasos (`instrument/acid303.rs:1-20`) | `acid303.rs` | 6 |
| `instrument.kick808` | Bombo/bajo 808 (`instrument/kick808.rs:1-12`) | `kick808.rs` | 6 |
| `instrument.sampler` | Sampler 16 voces: Classic/One-Shot/Loop, AHDSR, filtro, loop con crossfade, SFZ v2 con streaming de disco (`instrument/sampler.rs:1-14`, params `:61-89`) | `sampler.rs` | 6 |
| `instrument.drum` | Sampler de 16 pads (notas 36-51): tune, decay, cutoff, pan, level, choke y gate por pad; curva de velocidad (`instrument/drum.rs:1-20`) | `drum.rs` | 6 |

`[V]` también: hay secuenciador de pasos en la app (`app/src/views/steps.rs`), no es dispositivo.

## A.2 Pruebas y reglas que cualquier dispositivo nuevo debe cumplir `[V]`

* `crates/stock/tests/robustness.rs`: sin NaN/inf con silencio, impulso, ruido, DC y señal fuerte, con parámetros al mínimo, máximo y mezclados.
* `crates/stock/tests/alloc.rs`: cuenta asignaciones y liberaciones durante `process`.
* `crates/stock/tests/presets.rs` y `bench.rs`: presets válidos y presupuesto de CPU (`cpu_budgets_stay_realtime_capable`).
* Nota de memoria `stock-param-rules` `[V]`: en parámetros escalonados, `choices.len() == max - min + 1`; ids nuevos en vez de cambiar rangos; `gen` es palabra reservada (edición 2024); categorías del banco de sonidos limitadas a `library::taxonomy::Category`.
* Un dispositivo nuevo = módulo en `crates/stock/src/fx/`, entrada en `REGISTRY` (`lib.rs:37-62`), editor en `views/device_ui/` con su `match` en `editor()` (`device_ui/mod.rs:652-680`), palette (`views/palette/devices.rs`), carpeta de presets.

## A.3 Racks, cadenas, macros, sidechain: qué hay

### Modelo `[V]`

* `Rack { kind, chains, macros (16), variations, chain_selector }` (`crates/model/src/project.rs:734-742`); `MACRO_COUNT = 16` (`:23`).
* `RackKind { Instrument, Audio, Drum, Midi }` (`:760-766`). `Midi` solo existe como nombre: la UI lo rotula "MIDI Rack" (`app/src/views/devices.rs:313`) pero no hay forma de crearlo ni dispositivos MIDI que meter.
* `Chain { name, color, devices, volume_db, pan, mute, solo, note, key_range, velocity_range, selector_range }` (`:769-782`). Los rangos son `(u8,u8)` duros: **sin fades**.
* `Macro { name, value, mappings }`, `MacroMapping { device, param, min, max }` con min > max para invertir (`:803-817`). `Variation { name, values }` (`:820`).
* Sidechain: `Device.sidechain: Option<TrackId>` (`:686-689`), comando `SetSidechain` (`command.rs:118`).
* Comandos de rack: `AddChain`, `RemoveChain`, `UpdateChain` (chain entera), `SetMacro`, `SetMacroDef`, `SetVariations` (`command.rs:123-128`, aplicación `:479-516`), con inverso para undo. Agrupar/desagrupar (Cmd+G) como constructores de comandos en `model/src/racks.rs:1-215` con tests.

### Motor `[V]`

* Compilación: cada cadena se compila aparte; `max_lat` = latencia máxima; cada cadena lleva un `DelayLine` de `max_lat - lat` muestras; la latencia del rack suma `max_lat` (`engine/src/compile.rs:161-189`). Test: `rack_chains_are_latency_aligned` (`engine/src/tests.rs:300`).
* Proceso: el rack copia la entrada a `input`, limpia el buffer, y por cada cadena filtra los eventos (`accepts`), copia la entrada, procesa sus dispositivos, aplica el retardo de alineación y suma con ganancia (volumen×pan, 0 si mute/solo de otra) (`graph.rs:231-282`). Todo con buffers preasignados en la compilación (`ChainNode.buf`, `events` con capacidad `EVENT_CAPACITY = 2048`, `graph.rs:26,91-107`).
* Filtrado por zonas: Drum Rack → nota == `chain.note`; otros → `key_range` y, para NoteOn, `velocity_range` (`graph.rs:232-244`). **El `selector_range` y `chain_selector` NO se usan en el motor** (búsqueda de `selector` en `engine/src` y `app/src`: sin resultados).
* Macros: al compilar, `collect_macro_overrides` recorre racks anidados y fija `min + (max-min)*valor` en los parámetros destino (`compile.rs:118-143`). La automatización de macros usa `ParamTarget::Macro` (`graph.rs:~269`, `compile.rs:542-553`).
* Un cambio de proyecto reenvía el `Project` y marca `dirty`; el hilo de control recompila agrupando ráfagas (`engine.rs:631-700`). Los procesadores se reutilizan por `DeviceId` (`shared.rs:243-300`). `[S]` Mover una macro con el ratón dispara una recompilación por cada gesto coalescido; funciona, pero conviene un camino directo (ver §D.5).
* Eventos: **el mismo slice `events` va a todos los dispositivos de la cadena** (`graph.rs:138-189`) y los efectos de audio también lo reciben (por eso los plugins pueden reaccionar a MIDI). **No hay salida de eventos de ningún dispositivo.**
* Sidechain entre pistas: sí, con orden topológico y aviso si cierra un bucle (`compile.rs:282-345`); la señal publicada es la de la pista tras sus dispositivos y antes del fader (`graph.rs:1006-1012`). No hay punto de toma elegible (Pre FX / Post FX / Post mixer), ni sidechain desde una cadena de rack o un pad, ni compensación de latencia del sidechain (el comentario de `compile.rs:282-285` dice que no añade nada a la PDC).

### UI `[V]`

* Rack en la vista de dispositivos: lista de cadenas con selección, dispositivos de la cadena elegida como cadena propia con arrastrar/soltar (`devices.rs:722-860`), panel de 16 macros con perillas y botón mostrar/ocultar (`:567-660`), variaciones Guardar / recuperar / borrar (`:587-640`, lógica `app/src/state.rs:1183-1230`).
* Drum Rack: rejilla 4×4 de pads **solo de visualización** (nombre, color, nota; sin `on_click`, `devices.rs:~678-706`); `drum_pads` lee `chain.note` (`state.rs:1828`).
* Alta desde la paleta: `rack.instrument`, `rack.drum`, `rack.audio` (`app/src/real/plugins.rs:84-86`); `add_stock_device` crea el `Rack` vacío (`state.rs:1237-1250`). Demo con 8 pads, uno `instrument.drum` por pad (`app/src/demo.rs:63-85`).
* **No hay UI para mapear macros** (`SetMacroDef` nunca se emite desde `app/src`; solo lo usan tests del modelo), ni para editar zonas, ni para volumen/mute/solo de cadena, ni choke/Receive/Play.

### Respuestas directas a las preguntas del encargo

| Pregunta | Respuesta |
|---|---|
| ¿Existe MIDI effect como categoría? | Solo el valor del enum (`dsp/src/lib.rs:75-81`) y `RackKind::Midi`. **Ningún dispositivo ni soporte en el motor.** Lo más parecido son herramientas de edición offline del piano roll (acordes, arpegio: `app/src/views/menus.rs:334-381`, `ArpPattern`/`ChordKind` en `state.rs`), que escriben notas, no procesan en vivo. |
| ¿Drum rack? | Sí, parcial: filtro por nota `chain.note`, pads visibles, `instrument.drum` con choke interno de 16 pads. Faltan: Receive/Play note, choke entre cadenas, returns y envíos, pads clicables, arrastrar sample a pad. |
| ¿Macros? | Sí: 16 por rack, variaciones, rango min/max invertible, automatizables. Falta UI de mapeo, macros a propiedades de cadena (volumen, selector…), aleatorizar, excluir de variaciones. |
| ¿Sidechain entre pistas? | Sí (`Device.sidechain`), sin punto de toma, sin sidechain interno de rack. |

---

# PARTE B · Matriz contra Live Suite

## B.1 Fuentes de la lista

* Manual de Live 12, referencias de efectos de audio, MIDI e instrumentos (índices y secciones descargados): https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/ · https://www.ableton.com/en/live-manual/12/live-midi-effect-reference/ · https://www.ableton.com/en/live-manual/12/live-instrument-reference/ `[V]`.
* Manual de racks (cap. 25): https://www.ableton.com/en/live-manual/12/instrument-drum-and-effect-racks/ `[V]`.
* Página de comparación de ediciones, **rotulada Live 13** (contiene el superconjunto Live 12 + novedades de la 13: Drum Synth, Transient Shaper, Clipper, Color Limiter, Re-Enveloper, MIDI Tools…): https://www.ableton.com/en/live/compare-editions/ `[V]`; novedades de la 13 (Multiband y Mid-Side Racks, hasta 32 macros, Step Sequencer): https://www.ableton.com/en/live/all-new-features/ `[V]`.
* Recuento de Suite (20 instrumentos, 58 efectos de audio, 14 MIDI effects, 14 MIDI tools, 6 moduladores) citado por un distribuidor: https://www.sweetwater.com/store/detail/Live12Ste `[S, tercero]`. Crónica de las novedades de la 12 (Meld, Roar, Granulator): https://www.musicradar.com/news/ableton-live-12-whats-new-devices-midi-workflow y https://www.engadget.com/ableton-live-12-whats-new-meld-roar-094528196.html `[V, solo titulares/resumen]`.
* No se pudo leer el cuerpo de Drum Buss, Echo, Auto Shift ni Beat Repeat en el manual (el volcado de la página se cortó); sus controles salen de la sección de Beat Repeat sí leída, y de memoria para Drum Buss y Echo `[S]`.

## B.2 Efectos de audio

Estado: **Completo** (equivalente o superior), **Parcial** (qué falta), **Ausente**. Prioridad **P1-P3** para house/techno/stutter house/bass (P1 = abrir cada sesión).

| Dispositivo Live | Nuestro | Estado | Qué falta / nota | Prio |
|---|---|---|---|---|
| Amp | `fx.amp` | Completo | 4 modos + cabina | P3 |
| Cabinet | dentro de `fx.amp` (4 cabinas) | Parcial | Sin dispositivo suelto ni mezcla de micrófonos | P3 |
| Auto Filter | `fx.filter` | Parcial | Faltan tipos Morph, DJ, Comb, Resampling, Notch+LP, Vowel; circuitos SVF/DFM/MS2/PRD; LFO con Spin, Steps y S&H, Wander; envolvente por sidechain externo con EQ de sidechain `[V manual 29.2]` | P1 |
| **Auto Pan-Tremolo** | `fx.shaper` (modo Pan/Volume) y `fx.utility` (pan fijo) | **Ausente** (aprox. débil) | El shaper es curva de 6 nodos solo en divisiones 1/16-4 compases; sin Hz libres, sin desfase estéreo, sin formas, sin tremolo armónico, sin ataque ni FM dinámica | **P1** |
| Auto Shift | — | Ausente | Corrección de tono en tiempo real (pitch detect + shifter) | P3 |
| **Beat Repeat** | — | **Ausente** | Ver §C.5 | **P1** |
| Channel EQ | `fx.eq` | Parcial | Cubierto con creces; falta una vista simple de 3 bandas | P3 |
| Chorus-Ensemble | `fx.chorus` | Completo | Chorus 2-6 voces, flanger, phaser | P3 |
| Compressor | `fx.compressor` | Completo (superset) | | — |
| Corpus | — | Ausente | Resonador de modelado físico (cuerda, tubo, placa) | P3 |
| Delay | `fx.delay` | Completo | | — |
| **Drum Buss** | — | Ausente | Se compone con piezas existentes (§C.7) | P2 |
| Dynamic Tube | `fx.saturator` (Tube) | Parcial | Sin seguidor de envolvente que modula el bias | P3 |
| Echo | `fx.delay` (Tape/BBD) | Parcial `[S]` | Falta la reverb integrada, Noise, modo Repitch/Fade, ganancia de puerta por eco | P2 |
| EQ Eight | `fx.eq` | Completo (superset) | | — |
| EQ Three | `fx.multiband` + `fx.eq` | Parcial | El "kill" de DJ se logra con mute por banda de `fx.multiband`; falta el dispositivo de 3 knobs | P3 |
| Erosion | — | Ausente | Modulación de retardo con ruido/seno (S) | P3 |
| External Audio Effect | — | Ausente | Inserto de hardware; depende de E/S | P3 |
| Filter Delay | `fx.delay` | Parcial | Sin 3 líneas con filtro propio | P3 |
| Gate | `fx.gate` | Completo | | — |
| **Glue Compressor** | `fx.compressor` estilo Glue + `fx.console` | Parcial | Sin pasos fijos de ratio/ataque/release (2/4/10; 0,01-30 ms; 0,1-1,2 s + Auto), sin Soft Clip a −0,5 dB, sin rejilla de ganancia en aguja, sin filtro de sidechain fijo `[V manual 29.21]` | **P1** |
| Grain Delay | — | Ausente | Retardo granular con pitch | P3 |
| Hybrid Reverb | `fx.reverb` | Parcial | Falta el motor de convolución y el cruce convolución/algorítmica | P2 |
| Limiter | `fx.limiter` | Completo (superset) | | — |
| Looper | — | Ausente | Looper de audio sincronizado | P2 |
| **Multiband Dynamics** (OTT) | `fx.multiband` (solo divisor) | **Ausente** | Dinámica por banda arriba y abajo; especificado en doc 07 §4.3.3 pero no implementado | **P1** |
| Overdrive / Pedal | `fx.amp` / `fx.saturator` | Parcial | Cubierto en sonido, no en forma | P3 |
| Phaser-Flanger | `fx.chorus` | Completo | | — |
| **Redux** | `fx.saturator` (estilo Bitcrush por banda) | **Parcial** | Falta jitter, filtros pre/post, Shape del cuantizador, DC Shift `[V manual 29.30]`; el reductor actual no tiene jitter (`dsp/src/shaper.rs:121-144`) | **P1** |
| Resonators | — | Ausente | 5 resonadores en paralelo con afinación MIDI | P3 |
| Reverb | `fx.reverb` | Completo | | — |
| **Roar** | `fx.saturator`, `fx.tape`, `fx.amp` | Parcial | Faltan routing Serial/Parallel/M-S/Feedback, matriz de modulación, compresión interna `[V manual 29.33]` | P2 |
| Saturator | `fx.saturator` | Completo (superset) | | — |
| **Shifter** | (pitch shifter interno de shimmer: `fx/reverb/shimmer.rs:21`) | Ausente como dispositivo | Pitch / frecuencia / ring mod con LFO y MIDI `[V manual 29.35]` | P2 |
| Spectral Resonator / Spectral Time / Spectral Blur | — | Ausente | STFT con congelado y retardo espectral | P3 |
| Spectrum | `fx.analyzer` | Completo | | — |
| Tuner | — | Ausente | Detector de tono (el motor de `aitrack-ai` ya tiene detección de tonalidad/BPM, no afinador) | P3 |
| Utility | `fx.utility` | Completo | | — |
| Vinyl Distortion | — | Ausente | Distorsión de aguja + crackle (S) | P3 |
| Vocoder | — | Ausente | 40 bandas, carrier interno/externo | P3 |
| *Live 13 (página de comparación):* Transient Shaper, Clipper | `fx.transient`, `fx.clipper` | Completo | | — |
| *Live 13:* Color Limiter, Re-Enveloper, Gated Delay, Align Delay, Convolution Reverb, PitchLoop89, Pitch Hack | — | Ausente | Ninguno es P1 | P3 |
| Audio Effect Rack / Mid-Side Rack / Multiband Rack | `rack.audio` | Parcial | Faltan plantillas Mid-Side y Multiband (§D.7) | P2 |

## B.3 Efectos MIDI

Lista de la 12: Arpeggiator, CC Control, Chord, Note Length, Pitch, Random, Scale, Velocity `[V manual 30.1-30.8]`; la página rotulada Live 13 añade Melodic Steps, MIDI Monitor, MPE Control, Note Echo, Rotating Rhythm Generator y MIDI Effect Rack `[V]`. Los "MIDI Tools" (Arpeggiate, Span, Connect, Ornament, Quantize, Recombine, Strum, Time Warp, Velocity Shaper, Stacks, Rhythm, Seed, Shape, Euclidean Generator) son herramientas del editor de clips, no dispositivos `[V compare-editions]`.

| Dispositivo | Nuestro | Estado | Prio |
|---|---|---|---|
| **Arpeggiator** | Solo constructor offline de arpegios en el piano roll | **Ausente** (como dispositivo en vivo) | **P1** |
| **Chord** | Constructor offline de acordes | Ausente (en vivo) | P1 |
| **Scale** | `model::scale` (escala del proyecto, resalte en piano roll) | Ausente como filtro en vivo | P1 |
| **Velocity** | — | Ausente | P2 |
| **Pitch** | — | Ausente | P2 |
| **Random** | — | Ausente | P2 |
| **Note Length** | — | Ausente | P2 |
| Note Echo | — | Ausente | P2 |
| CC Control, MIDI Monitor, MPE Control | — | Ausente | P3 |
| Melodic Steps, Rotating Rhythm Generator | — | Ausente (en la 13) | P3 |
| MIDI Effect Rack | `RackKind::Midi` sin contenido | Ausente | P1 (llega con el marco, §C.6) |
| MIDI Tools (generadores/transformadores de clip) | Parcial: acordes, inversiones, arpegio, humanizar en piano roll | Parcial | P3 |

## B.4 Instrumentos

| Dispositivo Live | Nuestro | Estado | Qué falta | Prio |
|---|---|---|---|---|
| Wavetable | `instrument.wavetable` | Completo (superset) | | — |
| Operator | `instrument.vault` motor FM6 | Completo-parcial | 6 operadores/12 algoritmos; falta editor de envolventes por operador estilo Operator si se quiere | P3 |
| Analog | `instrument.vault` Mono Ladder / Poly Analog + `acid303` | Parcial | Sin modelado "físico" de osciladores de Analog | P3 |
| Drift | `instrument.vault` Poly Analog | Parcial | Sin MPE | P3 |
| Electric | `instrument.vault` piano eléctrico | Completo | | — |
| Impulse | `instrument.drum` | Completo (superset: 16 pads) | | — |
| **Drum Sampler** (12.x) | `instrument.sampler` (uno por pad) | Parcial | Un sample por pad con AHD, filtro y efectos de reproducción (Stretch, Loop, FM) `[V manual 31.4]`; nuestro sampler no tiene Stretch ni FM | P1 (para el Drum Rack) |
| Simpler | `instrument.sampler` | Parcial | Falta modo Slicing, warp, LFO, tipos de filtro (solo LP/HP/BP) | P1 |
| Sampler | `instrument.sampler` (SFZ v2, streaming) | Parcial | Falta editor de zonas y matriz de modulación; el motor de zonas sí existe | P2 |
| **Meld** (12.0) | — | Ausente | Sinte de dos osciladores "macro" con modulación `[V manual 31.8]` | P2 |
| **Granulator III** | — | Ausente | Rotulado entre los instrumentos de Suite en la página de comparación `[V]` | P2 |
| Collision, Tension | — | Ausente | Modelado físico (mazo/cuerda) | P3 |
| External Instrument | — | Ausente | Enrutado a hardware | P3 |
| Drum Synth (Live 13) | `instrument.kick808` | Parcial | Un solo motor (bombo) de ~20 | P2 |
| Bass, Poli (instrumentos Max de Suite) | `acid303`, `vault` | Parcial | | P3 |
| Instrument Rack / Drum Rack | `rack.instrument`, `rack.drum` | Parcial | §A.3 y Parte D | P1 |

---

# PARTE C · Especificación de los 10 ausentes más importantes

Orden de prioridad (producción electrónica):

1. `fx.autopan` Auto Pan-Tremolo (S)
2. `fx.crusher` Redux (S)
3. `fx.stutter` Beat Repeat + stutter house (M)
4. `fx.mbdyn` dinámica multibanda tipo OTT (M)
5. `fx.buscomp` compresor de bus "glue" (S-M)
6. Marco de **efectos MIDI** + Arpeggiator, Chord, Scale, Velocity, Pitch, Random, Note Length, Note Echo (L)
7. `fx.drumbus` (M)
8. `fx.shifter` (M)
9. `instrument.granular` (L)
10. `fx.driver` distorsión multietapa con feedback (M)

**Nombres.** Los ids/nombres de arriba son propios y deliberadamente genéricos; "Redux", "Glue Compressor", "Beat Repeat", "Drum Buss", "Roar", "OTT" son nombres de producto (OTT es de Xfer) y no se usan en la UI ni en presets (doc 04 §8.1). Presets con nombres descriptivos.

**Reglas comunes a todos** (CLAUDE.md y `stock-param-rules`): sin asignaciones en `process`; ids de parámetro estables (nunca renumerar); presets guardados con todos los ids; editor propio con colores de `theme.rs`, tiempos de `motion.rs`, iconos de `icons.rs`, espaciado en múltiplos de 4; tests en `#[cfg(test)]`; `cargo check --workspace`, `cargo test -p aitrack-stock`, `cargo clippy -p aitrack-stock` (nunca `cargo fmt`).

## C.1 `fx.autopan` — pan automático y trémolo (S)

**Función** (Ableton Auto Pan-Tremolo `[V manual 29.3]`): modo Pan o Tremolo; formas Sine, Triangle, Shark Tooth, Saw Up, Saw Down, Square, Random, Wander, S&H; tiempo en Hz, ms (100 ms-200 s) o sincronizado (recto, puntillo, tresillo); desfase estéreo por Phase (0-180°) o Spin (%); Invert; Amount; Shape; Attack; FM dinámica por envolvente de entrada; en Tremolo, Harmonic (bandas alta/baja alternadas, corte fijo 600 Hz) y Vintage (curva no lineal).

**DSP propio**

1. **LFO**: acumulador de fase `φ ∈ [0,1)`; reutiliza `dsp::lfo::Lfo` (7 formas, `lfo.rs:6-21`) y añade dos: *Shark tooth* (diente de sierra con flanco de bajada convexo, `y = 1 − 2·(1 − φ)^1.7`… ajustable a oído) y *Wander* (ruido de paso bajo con ley 1/f: suma de dos S&H suavizados a frecuencias f y f/3). Salida normalizada a [−1, 1].
2. **Shape** (0-100 %): sobre Sine/Triangle, aplanado hacia cuadrada con `y = tanh(g·x)/tanh(g)`, `g = 0,01 + 8·shape`; sobre Saw, sesgo del punto de quiebre; sobre Square, ancho de pulso (10-90 %). Suavizado de 0,5-1 ms con un polo para evitar clics en flancos.
3. **Estéreo**: LFO derecho = `φ + offset/360` (modo Phase, 0-180°; 180° = pan clásico en contrafase). Modo *Spin*: el LFO derecho corre a `rate·(1 + spin·0,05·k)` y se desliza cíclicamente respecto del izquierdo. Con Random/Wander/S&H solo Phase (otra secuencia aleatoria por canal, como en el manual).
4. **Modo Pan**: `m = amount·lfo ∈ [−1,1]`; ley de potencia constante con ganancia unidad al centro: `θ = (m+1)·π/4`, `gL = min(1, √2·cos θ)`, `gR = min(1, √2·sin θ)`. La señal estéreo se trata como balance (cada canal con su ganancia). Compatible en mono porque en el centro es 1/1.
5. **Modo Tremolo**: `g = 1 − amount·(1 − (lfo+1)/2)` por canal, con su fase. *Vintage*: `(lfo+1)/2` se eleva a 1,6 y se añade saturación suave asimétrica `tanh` con drive 0,3·amount (calidez de válvula/fotocélula). *Harmonic*: LR4 (`dsp::crossover::Lr4`) a 600 Hz; banda grave con `g` y agudos en contrafase (`g_hi = 2 − g`), y suma; como las bandas LR4 suman a un paso-todo, no hay coloración al recombinar con amount 0.
6. **Attack**: al detectar un ataque tras silencio (seguidor de envolvente por encima de −50 dBFS tras ≥ 100 ms por debajo), la amplitud del LFO sube de 0 a 1 en `attack` ms (0-2000).
7. **FM dinámica** (−100..+100 %): `rate_eff = rate · 2^(dyn · env · 2)`; `env` del seguidor 0..1 con atk 5 ms / rel 80 ms. Con sync activo, sigue el reloj salvo el desplazamiento (se documenta: con sync solo actúa como cuantización de fase).
8. **Sincronía**: fase absoluta = `fract(position_beats / div_beats + phase_offset)`; si el transporte está parado, corre libre desde un contador de muestras. Usa `SYNC_DIVISIONS` (18 divisiones, `dsp/src/lfo.rs:114-118`) más 1/64 si se quiere.

Referencias: Zölzer (ed.), *DAFX* 2.ª ed., cap. de efectos de modulación (trémolo/panoramizador) `[S, de memoria]`; Roads, *The Computer Music Tutorial*, MIT Press 1996, leyes de panning de potencia constante `[S]`; Pirkle, *Designing Audio Effect Plugins in C++* 2.ª ed., módulos de trémolo y autopan `[S]`; esquema del vibrato/trémolo "armónico" de amplificadores Fender de los años 60, concepto público `[S]`. LR4: ya en el repo (`dsp/src/crossover.rs`).

**Parámetros** (ids estables 0..):

| id | Nombre | Rango | Defecto |
|---|---|---|---|
| 0 | Mode | Pan, Tremolo | Pan |
| 1 | Wave | 9 formas | Sine |
| 2 | Time Mode | Hz, Time, Sync | Sync |
| 3 | Rate | 0,01-40 Hz (log) | 1 |
| 4 | Time | 100 ms-20 s (log; el límite de 200 s de Live no aporta) | 1 s |
| 5 | Division | 18 divisiones | 1/4 |
| 6 | Phase | 0-360° | 0 |
| 7 | Stereo Mode | Phase, Spin | Phase |
| 8 | Stereo | 0-180° / 0-100 % | 180° |
| 9 | Invert | off/on | off |
| 10 | Amount | 0-100 % | 100 |
| 11 | Shape | 0-100 % | 0 |
| 12 | Attack | 0-2000 ms | 0 |
| 13 | Dynamic FM | −100..100 % | 0 |
| 14 | Harmonic | off/on | off |
| 15 | Vintage | off/on | off |
| 16 | Mix | 0-100 % | 100 |
| 17 | Output | −24..+12 dB | 0 |

`meter()`: `[lfo_L, lfo_R, φ, env]`. Latencia 0. Cola 0.

**UI propia** (`device_ui/autopan.rs`): visor central de dos ciclos de LFO con dos curvas (L en el color de acento, R en el secundario del tema) y puntos vivos que recorren la forma; debajo, una barra de campo estéreo (−1…+1) con un marcador que se mueve en Pan y una barra de ganancia en Tremolo. Selector de forma como siete chips con icono de onda; arrastrar sobre el visor cambia Shape (vertical) y Phase (horizontal); doble clic restablece. Fila de perillas: Rate/Time/Division (una sola perilla que cambia con Time Mode), Stereo, Amount, Attack, Dyn FM, Mix. Interruptores Invert/Harmonic/Vintage como píldoras.

**Presets de fábrica** (≈ 12): Pan clásico 1/4; Pan lento envolvente (Hz 0,15, Wander); Trémolo trance 1/16; Compuerta tremolo (Square, Shape 100); Trémolo armónico vintage; Spin ancho; Pan en tresillo; Auto-pan desfasado 90°; Techno stab (1/8T, 60 %); Pad respiratorio (Triangle 0,3 Hz, 35 %).

**Tests**: LFO periódico en la fase correcta con transporte en marcha; a 180° y amount 100 % el balance L/R suma potencia constante ±0,5 dB; el modo Harmonic conserva la suma de bandas con amount 0; sin NaN (robustness); alloc.

## C.2 `fx.crusher` — reductor de bits y de frecuencia de muestreo (S)

**Función** (Redux `[V manual 29.30]`): Rate (downsample), Jitter (ruido de reloj, aumenta el ruido y la anchura estéreo), Filter Pre (paso bajo antes), Filter Post (paso bajo después, con octava), Bits, Shape (curva del cuantizador), DC Shift, Dry/Wet.

**DSP propio**

1. **Reloj de muestreo con jitter**: fase `p` que avanza `r = rate/sr` por muestra; al cruzar 1 se toma una nueva muestra (S&H de orden cero). El periodo de cada tick es `T_k = (1/r)·(1 + j·u_k)` con `u_k ∈ [−1,1]` de un xorshift por canal (canales independientes → ensanche estéreo). Mejora sobre `Bitcrusher` actual (`dsp/src/shaper.rs:121-144`), que no tiene jitter ni la medición de tiempo fraccionario. `[S]` Límite inherente: la retención cae en rejilla de la tasa del anfitrión (jitter de ±1 muestra a tasas altas); se acepta, "aliasing es el punto".
2. **Pre-filtro** (Filter Pre): paso bajo Butterworth 2.º orden a `0,45·rate` (reduce el aliasing del downsample, da el sonido "limpio"); opcional.
3. **Cuantizador**: `x → compander(x)` con Shape ∈ [−1,1]: `shape>0` ley μ (`sign·ln(1+μ|x|)/ln(1+μ)`, μ = 255·shape) que da más resolución a las señales débiles; `shape<0` curva inversa; luego `q = round(x·L)/L`, `L = 2^(bits−1)` (bits fraccionarios admitidos interpolando L), y descompresión inversa. Mid-tread.
4. **DC Shift** (−100..+100 %): `x + d·Δ` antes del cuantizador y `−d·Δ` después (la zona muerta se mueve; sin cambiar la DC de salida); el offset no restado queda como opción "Keep DC" con bloqueador de 10 Hz en la salida.
5. **Post-filtro**: paso bajo 2.º orden a `rate/2 · 2^oct`, `oct ∈ [−2, +1]`.
6. Mezcla seco/húmedo en fase (latencia 0 salvo que Pre filtre; los filtros IIR no añaden latencia reportable).

Referencias: Zölzer, *DAFX* (cuantización, ruido de cuantización, capítulo de no linealidades) `[S]`; Pirkle (bit crusher / decimator) `[S]`; ley μ de ITU-T G.711 `[S]`; Pohlmann, *Principles of Digital Audio* (dither/cuantización) `[S]`.

**Parámetros**: Rate 200-48000 Hz (log; máximo = bypass del submuestreo); Jitter 0-100 %; Pre (off/on); Post (off/on); Post Octave −2..+1 (paso 1/12); Bits 1-16 (paso 0,1; 16 = sin cuantización); Shape −100..+100 %; DC Shift −100..+100 %; Keep DC (off/on); Stereo Jitter (linked/independent); Mix 0-100 %; Output −24..+12 dB. 12 parámetros, ids 0-11.

**UI** (`device_ui/crusher.rs`): a la izquierda la **curva de transferencia** (escalera de cuantización) dibujada en vivo desde los parámetros, con un asa vertical que cambia Bits y otra horizontal que mueve DC Shift; a la derecha un osciloscopio de una senoide de prueba interna (siempre visible, sin depender del audio) que muestra los escalones temporales del Rate y el jitter como bordes difusos; barra del Nyquist resultante. Perillas Rate, Jitter, Bits, Shape, DC, Post Oct, Mix; píldoras Pre/Post.

**Presets** (≈ 10): Lo-fi 12 bits; Arcade 8 bits; Teléfono; Sampler barato 22 kHz; Hats aliased; Bajo crujiente; Ruido digital (jitter 60 %); Cassette sucio; Textura 4 bits suave; Master vintage 14 bits.

**Tests**: con Bits = 16 y Rate = máximo la salida iguala la entrada (±1e-6); con bits = 1 sólo dos niveles; con jitter 0 la salida es periódica y determinista; alias medible (patrón de `dsp/src/shaper.rs` `alias_energy` en tests); sin NaN.

## C.3 `fx.buscomp` — compresor de bus tipo "glue" (S-M)

**Qué es la topología "VCA de bus tipo SSL"** (conocimiento público, el circuito exacto no está publicado en detalle `[S]`): compresor *feed-forward* con celda de ganancia VCA (de la familia Blackmer, de respuesta logarítmica) y detector de nivel de tipo RMS-aproximado en dominio logarítmico. Rasgos que producen el "pegamento": (i) ratios bajos fijos de 2:1, 4:1 y 10:1; (ii) ataques lentos escalonados (de 0,1 a 30 ms) que dejan pasar el transitorio; (iii) *auto-release* de doble constante de tiempo (un release rápido, ≈ 0,1 s, para el material percutivo y otro lento, ≈ 1,2 s, para la reducción sostenida, combinados según la profundidad de la reducción), lo que evita el bombeo con graves; (iv) sidechain con filtro paso alto fijo (≈ 60-100 Hz) para que el grave no dispare la compresión; (v) detector enlazado en estéreo (el bus se comprime como una sola señal); (vi) rodilla blanda corta; (vii) la distorsión suave del VCA, que Live reproduce con *Soft Clip*; (viii) mezcla seco/húmedo en paralelo ("New York compression").

**Por qué dispositivo nuevo y no tocar `fx.compressor`**: el compresor ya tiene un estilo Glue (RMS ≥ 60 % y release dependiente del programa, `fx/compressor.rs:9`) y 23 parámetros; los ids son estables y no deben reinterpretarse. Un dispositivo de 8 mandos, de pasos fijos, es otro flujo de trabajo. Se construye sobre `dsp::envelope::{GainSmoother, gain_computer}` (los usa el compresor, `compressor.rs:34-37`) y el módulo SVF para el filtro de sidechain.

**DSP propio**

1. Detector: `s = max(|L|,|R|)` mezclado con RMS de 10 ms (50/50), en dB.
2. Computador de ganancia en dB con rodilla cuadrática de 4 dB (Giannoulis et al. 2012), `slope = 1 − 1/ratio`.
3. Suavizado de la reducción: ataque con ramificación (*branching*) en el tiempo elegido; release fijo o **Auto**: `rel = lerp(0,1 s, 1,2 s, clamp(GR_dB/10))` con memoria de dos etapas (dos polos, el lento se alimenta del rápido), como el "program-dependent" del compresor existente.
4. Range (0-40 dB): tope de reducción.
5. Sidechain interno o externo (`ctx.sidechain`), HPF de 2.º orden con frecuencia elegible entre Off/30/60/90/120/180/240 Hz, "Sidechain Listen".
6. Makeup (0-24 dB); Soft Clip: waveshaper tanh con techo a −0,5 dBFS, con ADAA1 (`dsp::shaper::Adaa1`, `filter.rs:6` lo usa) a 2× (`dsp::oversample::Oversampler`); latencia declarada del oversampler solo si Soft Clip está activo (`latency()` cambia como en `compressor.rs`).
7. Mezcla en paralelo; el seco se retrasa la latencia del sobremuestreador cuando hace falta.

Referencias: Giannoulis, Massberg y Reiss, "Digital Dynamic Range Compressor Design — A Tutorial and Analysis", JAES 60(6), 2012 (ya citada en doc 12); McNally, "Dynamic Range Control of Digital Audio Signals", JAES 32(5), 1984 `[S]`; Reiss y McPherson, *Audio Effects*, CRC 2014 `[S]`; manual de Live, Glue Compressor `[V]` para el conjunto de controles.

**Parámetros**: Threshold −40..0 dB; Ratio {2, 4, 10} (discreto, `choices` 3 entradas); Attack {0,01; 0,1; 0,3; 1; 3; 10; 30} ms; Release {0,1; 0,2; 0,4; 0,6; 0,8; 1,2 s; Auto}; Range 0-40 dB; Makeup 0-24 dB; Soft Clip off/on; Mix 0-100 %; Sidechain Internal/External; SC HPF; SC Listen; Output. 12 ids. (Las `choices.len()` deben igualar `max-min+1`, regla de `stock-param-rules`.)

**UI** (`device_ui/buscomp.rs`): protagonista, un **medidor horizontal de reducción de ganancia** a lo ancho (barra con escala en dB, retención del pico y marcador de pico de salida; indicador de clip), no una aguja imitando un VU comercial. Debajo, mandos grandes de Threshold y Makeup; los parámetros escalonados como **segmentos con detente** (Ratio: tres botones; Attack/Release: rueda con muescas). Un panel de sidechain plegable con curva de HPF.

**Presets** (≈ 8): Bus de batería 4:1; Mix bus suave 2:1 Auto; Pegamento house; Paralelo NY 10:1 40 %; Techno bombeante (ataque 30 ms, Auto); Master 1,5 dB; Percusión viva; Sidechain externo de bajo.

## C.4 `fx.mbdyn` — dinámica multibanda ascendente y descendente, con preset "denso" (M)

El diseño completo ya está en doc 07 §4.3.3 (tabla de parámetros, DSP, editor, presets) y no se repite; este apartado lo **concreta y corrige** con lo averiguado en el código:

* **Estado `[V]`**: `fx.multiband` es solo divisor (`fx/multiband.rs:1-3`). Hay que crear un **dispositivo nuevo con id `fx.mbdyn`**, no reinterpretar `fx.multiband` (ids estables). `Crossover3` (LR4) y `LinearPhaseCrossover3` existen (`dsp/src/crossover.rs`).
* **Curva estática por banda** (dB): con nivel `L`, umbral superior `Ta`, inferior `Tb < Ta`, ratios `Ra ≥ 1` (descendente) y `Rb ≥ 1` (ascendente):
  `g = −(L−Ta)(1−1/Ra)` si `L>Ta`; `g = +(Tb−L)(1−1/Rb)` si `L<Tb`, acotada a `MaxUp` (0-36 dB); 0 dB entre ambos. Rodilla cuadrática en ambos codos. Más un **Floor** (umbral de suelo −80..−40 dB) por debajo del cual la ganancia ascendente se atenúa a 0, para no amplificar silencio (el ruido de la compresión ascendente es el fallo clásico).
* **Suavizado**: la ganancia en dB baja con `attack` y sube con `release` (`GainSmoother`), por banda.
* **Time** escala todos los ataques/releases (10-1000 %); **Amount** escala las pendientes (como en Live `[V manual 29.26]`); además **Upward %** y **Downward %** independientes (estilo OTT, doc 07).
* **Depth (mezcla) en fase**: no mezclar la entrada sin procesar con las bandas, porque la suma de LR4 es un paso-todo de 2.º orden (`fx/multiband.rs:3-5` documenta esto) y el seco no tiene esa fase: aparecería un cancelamiento en los cruces. Se mezcla **por banda**: `out = Σ banda_i · (1 − d + d·g_i)`. Conserva fase y es exacto en `d = 0`.
* **Sidechain externo opcional** para el detector, Peak/RMS, ganancia de entrada y salida por banda y globales, solo/bypass por banda. Latencia 0 en modo mínimo; en modo Lineal usa la latencia del FIR (`LinearPhaseCrossover3::latency`) con el seco ya alineado por construcción.
* **Preset "denso" (el sonido OTT)** `[S, de doc 07]`: cruces 88,3 Hz y 2,5 kHz; descendente Ra ∞:1 agudos y ≈ 66:1 graves/medios, ascendente Rb ≈ 4:1, entradas +5,2 dB por banda y salidas +10,3 / +5,7 / +10,3 dB; umbrales y tiempos aproximados de memoria (doc 07 los marca como sin verificar). Se parte de ahí y se ajusta a oído; no se vende como "OTT".
* Parámetros: 2 cruces, modo, Amount, Time, Up %, Down %, Detección, Knee, In, Out, Depth, Sidechain; por banda ×3: Ta, Ra, Tb, Rb, MaxUp, Attack, Release, In, Out, Solo, Bypass. ≈ 45 ids.
* UI: espectro de fondo, cruces arrastrables, y en cada banda una columna con **dos asas** (umbral superior e inferior); la zona sobre el superior se tiñe con el color de "baja", la inferior con el de "sube"; barra de nivel en vivo y de ganancia aplicada (subir verde / bajar naranja del tema). `Alt`+arrastre = ratio. Vista compacta con Depth, Time, Up, Down y tres ganancias.
* Referencias: Giannoulis et al. 2012; Zölzer, *DAFX* cap. 4 (dinámica); Linkwitz-Riley (Linkwitz 1976, "Active Crossover Networks for Noncoincident Drivers", JAES) `[S]`; manual de Live Multiband Dynamics `[V]`.

## C.5 `fx.stutter` — repetidor de rejilla para stutter house, glitch y tape-stop (M)

**Función de Live** (Beat Repeat, `[V manual 29.5]`): Interval (1/32 a 4 compases), Offset, Chance (0-100 %), Gate (longitud total en 1/16), Grid (tamaño del trozo repetido), No Triplets, Variation (Trigger, 1/4, 1/8, 1/16, Auto), Pitch y Pitch Decay, Filter (centro/anchura), Volume y Decay, modos de mezcla Mix / Insert / Gate, botón Repeat. Nuestro dispositivo cubre eso y añade lo que pide el stutter house: **rejilla de 16 pasos con probabilidad**, **tape-stop**, **half-time**, **rampa de subida (build-up)** y reproducción inversa.

**Arquitectura (sin asignaciones en `process`)**

1. **Captura permanente**: buffer circular estéreo preasignado en `prepare` para 16 s a la tasa de muestreo (≈ 6 MB en f32 a 48 kHz) que siempre graba la entrada. Cubre 4 compases a 60 BPM.
2. **Reloj**: cada bloque convierte `ctx.transport.position_beats` y `bpm` en posición de rejilla por muestra; con el transporte parado, corre un contador libre al `bpm` actual. El disparo ocurre en límites de `Interval` (+ `Offset`).
3. **Máquina de estados**: `Idle → Armed → Repeating(k) → Release`. Al disparar (y pasar `Chance`), se **congela** la región `[t−grid, t)` del buffer (el trozo justo anterior al disparo, como Beat Repeat) y se reproduce en bucle durante `Gate` (o hasta que `Hold` se suelte).
4. **Decisiones deterministas**: toda probabilidad (Chance, Variation, pasos de la rejilla, reverse aleatorio) sale de un generador **función pura de `(índice_de_evento, semilla)`** y no de un estado que dependa de dónde empezó la reproducción, así que el *bounce*/exportación suena igual que en directo y el *undo/redo* no cambia el patrón. `[S]` es una decisión de diseño, no algo que Live documente.
5. **Lectura**: posición de lectura fraccionaria con interpolación Hermite (`dsp::delay::hermite`, `delay.rs:85`); velocidad `r_k = 2^((pitch + decay·k)/12)` con `k` = número de repeticiones transcurridas; la posición se envuelve dentro de `[inicio, fin)` del trozo, así el bucle sigue anclado a la rejilla aunque `r<1` (se oye un trozo más grave y más corto, el sonido habitual del stutter con *pitch decay*). Alternativa futura: estiramiento temporal WSOLA (Verhelst y Roelands, ICASSP 1993 `[S]`) para tono sin cambio de duración.
6. **Tape-stop / half-time**: `r(t)` desciende exponencialmente a 0 en `Stop` (1/16 a 4 compases) al final del evento (tape-stop) o se fija a 0,5 durante todo el evento (half-time: octava abajo y a media velocidad; se congela y relee los `grid` previos). Con `Reverse` la dirección es hacia atrás, hacia delante o ping-pong.
7. **Rampa (build-up)**: `Ramp` acorta el `grid` dentro del evento (1/4 → 1/8 → 1/16 → 1/32) para los redobles de subida.
8. **Salida**: Decay (dB por repetición), filtro paso banda opcional (SVF), declick (fundido de 64-128 muestras en entrada/salida; crossfade de 1-2 ms en la junta del bucle); modo **Insert** (la repetición sustituye al seco), **Mix** (suma) o **Duck** (seco atenuado hasta cerrar). Latencia 0.
9. **Rejilla de 16 pasos** (la pieza propia): cada paso tiene `On`, `Prob` (0-100 %), `Div` (1/8, 1/16, 1/16T, 1/32, 1/32T), `Pitch` (−12..+12) y `Rev`. Cuando el reloj entra en un paso, tira su probabilidad y, si pasa, dispara un evento de esa longitud. Es el modo principal para stutter house; el modo "Interval/Chance" de Live queda como modo clásico.

**Parámetros** (≈ 30): Mode (Classic / Grid); Interval, Offset, Chance, Gate, Grid, Triplets, Variation, Pitch, Pitch Decay, Decay, Reverse (Fwd/Rev/Ping-pong/Random), Filter On/Freq/Width, Mix Mode, Tape-Stop (off/on + tiempo), Half-Time, Ramp, Hold (automatizable/macro, 0/1), Seed, Mix, Output, y 16×(On, Prob, Div, Pitch, Rev) = 80 ids más con un `step(b, off)` como en `fx/multiband.rs:36-39`.

**Meter**: `[pos del buffer 0..1, estado, paso activo, r actual]` (≤ 320 valores, regla de `device-meter-cap`).

**UI** (`device_ui/stutter.rs`): arriba, la **rejilla de 16 pasos** con barras de probabilidad arrastrables, el paso activo iluminado y un chip por paso para la división; en el centro, la forma de onda del trozo capturado con las marcas de bucle y la curva de pitch superpuesta; abajo perillas y píldoras (Mode, Tape-Stop, Half-Time, Reverse). Un botón grande **HOLD** (momentáneo) para tocar a mano y un conmutador de modo en vivo.

**Presets** (≈ 12): Stutter House 1/16 70 %; Roll de caja 1/32 con rampa; Tape-stop de final de compás; Half-time en el drop; Glitch de tresillos; Reverse de snare; Subida de 4 compases; Stutter vocal con pitch-down; Trap hat repeat 1/32T; Gate rítmico; Cabeceo ping-pong.

**Tests**: bucle exacto de `grid` muestras con `r=1`; determinismo con la misma semilla en dos ejecuciones; con `Chance=0` la salida es idéntica a la entrada bit a bit; el estado no acumula tras `reset`; alloc; sin NaN.

**Dependencia de UX**: la forma de onda congelada exige que el buffer sea visible en el hilo de UI; usar `meter()` solo para un resumen de 128 picos.

## C.6 Marco de efectos MIDI + 8 dispositivos (L)

### Qué hay que cambiar en el motor (hoy no existe nada de esto) `[V]`

* `Processor` solo procesa audio con eventos de entrada (`dsp/src/lib.rs:63-71, 99-139`). Hace falta que un dispositivo de categoría `MidiEffect` pueda **emitir eventos**:
  ```text
  trait Processor { ...
      /// Solo para Category::MidiEffect. Eventos de salida con offset ordenado y capacidad fija.
      fn process_midi(&mut self, ctx: &MidiContext, out: &mut EventSink) {}
  }
  ```
  con `MidiContext { sample_rate, frames, events: &[Event], transport }` y `EventSink` sobre un `Vec` preasignado (patrón de `EventList::push`, `engine/src/clip.rs:60-77`; al desbordar descarta y cuenta `dropped`).
* `process_devices` (`graph.rs:138-189`): mantener un puntero `cur_events`; un nodo `is_midi` lee `cur_events`, escribe en un `EventList` propio y hace `cur_events = &node.out.events`. Los dispositivos siguientes (otros MIDI FX, el instrumento y los efectos de audio que miran eventos) usan `cur_events`. Un MIDI FX desactivado deja pasar los eventos sin tocarlos. Un MIDI FX que suprime notas debe liberar las que sonaban (NoteOff pendiente al desactivarlo/parar el transporte): mantener una máscara de 128 teclas × 16 canales en el dispositivo.
* **Sleep del motor** (`graph.rs:159-178`): un MIDI FX con cola pendiente (arpegiador con notas retenidas) no debe dormirse; basta marcarlos como "no duermen" (coste mínimo).
* **Tiempos futuros**: arpegiador, strum y Note Echo emiten notas después del instante en que llegó el evento; el dispositivo guarda una cola interna de tamaño fijo en `prepare` y la vacía por posición musical (transporte) y no por tiempo de pared, para que el render offline coincida.
* **`note_id`**: las notas generadas necesitan ids únicos para que los instrumentos emparejen NoteOn/NoteOff (el campo existe: `EventKind::NoteOn{note_id}`, `dsp/src/event.rs:11-16`); asignar ids de un rango alto por dispositivo.
* **Posición en la cadena**: reglas de Live `[V manual 25.2]`: efectos MIDI antes del instrumento; en pistas MIDI. Un `RackKind::Midi` suma en paralelo las salidas de sus cadenas (unión ordenada de eventos con `merge_window`, `graph.rs:194-207` ya sirve de modelo).
* **Model**: dispositivos normales (`Device::stock(id, "midi.arp", …)`); `is_instrument()` ya distingue por prefijo `instrument.`; añadir `midi.` para ubicar la UI y la paleta. Edición por `Command::AddDevice/SetParam` sin comandos nuevos.
* **Compatibilidad VST3/CLAP**: los plugins externos que emiten MIDI (`Event` de salida) quedan fuera de alcance; el marco debe poder alojarlos más adelante.
* **Escala del proyecto**: Scale/Chord/Arp necesitan la tonalidad; no pasar el `Project` al motor. Se inyecta como parámetro `Root` + `Scale` (índices de `model::scale`) y un conmutador "Usar escala del proyecto" que la app resuelve en el comando de la UI (mismo patrón que el resto), documentando que cambiar la escala del proyecto no reescribe parámetros ya guardados.

### Dispositivos (ids `midi.*`, categoría `MidiEffect`)

| Id | Función | Parámetros clave (rangos) |
|---|---|---|
| `midi.arp` | Arpegiador `[V manual 30.1]` | Style (Up, Down, UpDown, DownUp, Up&Down, Down&Up, Converge, Diverge, Con&Div, Pinky Up, Pinky Up&Down, Thumb Up, Thumb Up&Down, Play Order, Chord Trigger, Random, Random Other, Random Once), Rate (18 divisiones o Hz), Gate 1-200 %, Steps 1-8, Distance −12..+12 st, Repeats 1-∞, Retrigger Off/Note/Beat + Interval, Hold, Offset, Groove 0-100 %, Velocity Target/Decay, Octaves 1-4 |
| `midi.chord` | Acorde por nota `[V manual 30.3]` | Shift 1-6 (±36 st) con Velocidad por voz, Chance por voz, Strum 0-400 ms, Tension, Crescendo, Learn, Use Scale |
| `midi.scale` | Remapea a una escala `[V manual 30.7]` | Base, Escala (de `model::scale`), Transpose ±36, Fold, Lowest, Range, matriz 13×13 opcional |
| `midi.velocity` | Remapeo de velocidad `[V manual 30.8]` | Curva de 9 puntos, Mode Clip/Gate/Fixed, Random, Drive, Compand, Lowest, Range, Out Hi/Low |
| `midi.pitch` | Transposición `[V manual 30.5]` | Pitch ±48, Step Up/Down, Width, Lowest/Range, Mode Block/Fold/Limit |
| `midi.random` | Pitch aleatorio `[V manual 30.6]` | Chance 0-100 %, Choices 1-24, Interval ±, Mode Random/Alt, Sign Add/Sub/Bi |
| `midi.notelength` | Longitud de nota `[V manual 30.4]` | Trigger Note On/Off, Gate 1-400 %, Length (ms o sync), Release Velocity, Decay, Latch |
| `midi.echo` | Eco de notas (Live 13) | Time (sync), Feedback 0-100 %, Gate, Pitch por eco, Velocity decay, Mix |

**Algoritmo del arpegiador (el más complejo)**: mantiene un conjunto ordenado de teclas retenidas (array fijo de 16); avanza una fase de paso por posición de transporte (`position_beats / div_beats`), y en cada paso elige la nota según Style con un índice sobre la lista (patrones precalculados para Converge/Diverge/Pinky/Thumb); Gate fija el NoteOff a `gate%·paso`; Groove retrasa los pasos impares hasta ±50 %; Retrigger reinicia el índice. Estado `Random` determinista por posición.

**UI**: cada MIDI FX con un editor propio breve (`device_ui/midi.rs` con subvistas): el arpegiador muestra una rejilla de pasos con altura = nota relativa y velocidad; Chord muestra seis filas con desplazamientos y un teclado; Scale, un teclado de 12 teclas con las notas permitidas coloreadas (misma escala visual que el piano roll); Velocity, la curva arrastrable. Aparecen en la paleta como grupo "MIDI Effects" (hoy sólo hay Instrumentos y Efectos, `palette/devices.rs`).

**Esfuerzo desglosado**: marco del motor y trait (L, riesgoso por el hilo RT y por `adopt_devices`/PDC), `midi.scale`+`midi.pitch`+`midi.velocity`+`midi.random`+`midi.notelength` (S cada uno una vez que existe el marco), `midi.chord` (S-M), `midi.arp` (M), `midi.echo` (M). Referencias: especificación MIDI 1.0 (notas, canales) `[S]`; manual de Live cap. 30 `[V]`.

## C.7 `fx.drumbus` — bus de batería (M)

**Función de Live** `[S, de memoria]`: Trim, compresión fija, Drive con tres curvas (Soft/Medium/Hard), Crunch, Damp (paso bajo), Transients (±), Boom (resonancia grave con frecuencia y decay), Dry/Wet.

**DSP propio, compuesto con piezas existentes**: Trim → Drive (3 curvas con `fx/saturator/shapes.rs`, ADAA1, sobremuestreo 2×) → Crunch (waveshaper senoidal de las bandas > 500 Hz en paralelo) → compresor fijo 4:1 ataque 10 ms release automático (núcleo del `fx.buscomp`) → transient shaper (`fx/transient.rs`) → **Boom**: oscilador senoidal afinado (30-120 Hz, con ajuste a la nota/tonalidad del proyecto como valor de ayuda en UI) cuya amplitud sigue un seguidor de envolvente de la banda < 150 Hz de la entrada, con `Decay` 50-800 ms y fase bloqueada al disparo (evita cancelación con el bombo) → Damp (SVF paso bajo 1-20 kHz) → mezcla. Mezcla en paralelo con latencia alineada.

**Parámetros** ≈ 13: Trim, Drive amount, Drive type, Crunch, Damp, Transients, Boom Amount, Boom Freq, Boom Decay, Compress, Mix, Output, Bypass por etapa.

**UI**: cadena de 6 mini-paneles en fila (Entrada → Drive → Crunch → Comp → Trans → Boom) estilo "etapas", cada una con su perilla principal, medidor de reducción y un visor del grave generado.

**Presets** (≈ 8): Bus house, Bus techno duro, Batería pegada, Kit de 808 con boom, Breakbeat sucio, Toms con cuerpo, Parallel smash, Hats brillantes.

## C.8 `fx.shifter` — desplazador de tono / frecuencia / modulador en anillo (M)

**Función** `[V manual 29.35]`: modos Pitch, Freq (frecuencia) y Ring; Coarse, Fine, Wide (estéreo), Window, Tone, Delay y feedback, LFO (varias formas, duty, fase/spin, amount), seguidor de envolvente, Drive en Ring, pitch por MIDI, Glide, Dry/Wet.

**DSP propio**: *Pitch*: dos lecturas moduladas en una línea de retardo con ventana Hann cruzada (el mismo método del `PitchShifter` ya existente en `fx/reverb/shimmer.rs:21`, que se extrae a `dsp`); *Freq*: banda lateral única con un par de filtros paso-todo IIR de Hilbert (diseño de Olli Niemitalo, 12 polos) y modulación por `e^{j2πft}` (Bode 1956; Niemitalo, "Polyphase IIR Hilbert transformer", sitio yehar.com `[S, de memoria]`); *Ring*: producto con senoide y Drive. LFO y seguidor modulan la frecuencia. *Wide*: desplazamientos opuestos en L y R. Origen de pitch desde MIDI usando `ctx.events` (los efectos ya reciben eventos, `graph.rs:138-189`).

**Parámetros** ≈ 16; **UI**: visor de espectro simple antes/después, dos perillas grandes Coarse/Fine, LFO miniatura. Presets: Octava abajo, Ensanchador, Efecto metálico, Acorde con MIDI, Drone de sub, etc.

## C.9 `instrument.granular` (L)

**Función**: sintetizador granular a partir de un sample, equivalente funcional del Granulator III de Suite `[V, listado en la página de comparación; sin lectura de su manual]`. 

**DSP propio**: buffer de sample cargado en el hilo de control (mismo mecanismo que `instrument.sampler`, `load_state`); planificador de granos con hasta 128 granos preasignados, cada uno con posición, tamaño, ventana (Hann, Tukey, Gauss), tono y panorama; densidad como granos/s o solape; `Spray` (aleatorio de posición) y `Scan` (movimiento de posición con LFO o velocidad), tono aleatorio, dirección (adelante/atrás/aleatoria), freeze, filtro y envolvente AHDSR por voz, polifonía MIDI con pitch por tecla. Referencias: Roads, *Microsound*, MIT Press 2001 (ya en doc 12); Puckette, *The Theory and Technique of Electronic Music* (ya en doc 12); Truax, "Real-time granular synthesis with a digital signal processor", *Computer Music Journal* 12(2), 1988 `[S]`.

**Parámetros** ≈ 24; **UI**: forma de onda con ventana de lectura arrastrable y nube de granos animada (puntos que nacen y mueren); mandos Position, Spray, Size, Density, Pitch, Shape, Scan. Presets: Textura de pad, Rocío, Voz congelada, Ruido granular, Tren de granos rítmico.

## C.10 `fx.driver` — distorsión multietapa con feedback (M)

**Función** (Roar `[V manual 29.33]`): rutas Single, Serial, Parallel, Multi Band, Mid Side, Feedback; dos fuentes de modulación (LFO, envolvente, ruido) con matriz; compresión integrada.

**DSP propio**: reutiliza los ADAA1 de `fx/saturator/shapes.rs` y el crossover de `Crossover3`. Se centra en lo que `fx.saturator` no tiene: (a) **ruta Serial/Parallel** de dos etapas con tipos propios; (b) **Feedback**: línea de retardo corta afinable por nota o por Hz (`dsp::delay::DelayLine` con lectura cúbica), con la etapa de distorsión y filtro en el lazo y un gate de envolvente (resonancia estilo Karplus-Strong, Karplus y Strong 1983 `[S]`) para texturas resonantes afinadas a la nota MIDI; (c) **modulación**: 2 LFO + seguidor, 6 slots de matriz sobre drive/filtro/feedback; (d) compresor de salida suave. Parámetros ≈ 40; UI con diagrama de ruta clicable y gráficos de la curva de cada etapa. Presets orientados a bass music (growl, reese, screech controlado).

---

# PARTE D · Racks

## D.1 Qué hay y qué falta, en una tabla

| Capacidad (Live) | Modelo | Motor | UI |
|---|---|---|---|
| Cadenas paralelas, suma | Sí | Sí (`graph.rs:247-282`) | Sí |
| Compensación de latencia entre cadenas | — | Sí (`compile.rs:162-187`, test `tests.rs:300`) | — |
| Zonas Key / Velocity | Sí (duras) | Sí (`graph.rs:232-244`) | No |
| Fades de zona | No | No | No |
| Chain Select (selector + zonas) | Campos sin uso | No | No |
| Macros 16 + rango min/max | Sí | Sí (a la compilación) | Perillas sí; **mapeo no** |
| Macros a propiedades de cadena (volumen, selector, enabled) | No | No | No |
| Variaciones de macros | Sí | — | Guardar/recuperar/borrar; sin exclusión ni morph |
| Randomize | No | — | No |
| Hasta 32 macros (Live 13) | No (16 fijo) | — | — |
| Volumen / pan / mute / solo de cadena | Sí | Sí | **No** |
| Drum Rack: nota por pad | `chain.note` | Sí | Rejilla visual, sin clic |
| Receive / Play note, choke entre pads, returns y envíos | No | No | No |
| MIDI Effect Rack | `RackKind::Midi` vacío | No | No |
| Anidación | Sí | Sí | Sí |
| Agrupar / desagrupar | Sí (`racks.rs`) | — | Cmd+G |
| Sidechain dentro del rack / "Audio To" | No | No | No |

## D.2 Modelo (toda edición = `Command`)

Cambios con `#[serde(default)]` para no romper proyectos guardados; todo cambio con su inverso, como el patrón actual de `command.rs:479-516`.

* **`Chain`**: añadir `key_fade: (u8,u8)`, `vel_fade: (u8,u8)`, `sel_fade: (u8,u8)` (anchos de fade a cada lado), `choke: u8` (0 = ninguno, 1-16), `play_note: Option<u8>` (el `note` existente pasa a ser Receive), `sends: Vec<f32>` (dB hacia cada return), `output: ChainOut::{Rack, Return(i)}`.
* **`Rack`**: añadir `returns: Vec<Chain>` (máx. 6, solo Drum), `selector: f32` (hoy `chain_selector: u8`, `project.rs:741`; mantener el campo y leerlo como migración), `macro_count: u8` (8/16/32), `wet: f32` opcional.
* **`MacroMapping`**: añadir `target: MacroTarget::{DeviceParam, DeviceEnabled, ChainVolume(chain), ChainPan(chain), ChainMute(chain), Selector}` con `Default = DeviceParam`, `curve: Lin/Exp/S`, y para parámetros escalonados `steps: Option<u8>`. Ampliar `AutomationTarget`/`ParamTarget::Macro` (que hoy sólo mueve procesadores, `graph.rs:~269`) a esos destinos.
* **`Macro`**: `default: f32`, `in_variations: bool` (excluir de variaciones, `[V manual 25.7.3]`).
* **Comandos nuevos** (granulares para poder coalescer arrastres como `SetParam`): `SetChainProp { rack, chain, prop, value }` con `ChainProp::{KeyRange, VelRange, SelRange, KeyFade, VelFade, SelFade, Volume, Pan, Mute, Solo, Choke, ReceiveNote, PlayNote, Send(i), Output}`; `AddMapping/RemoveMapping/SetMappingRange { rack, macro, … }` (en lugar de reemplazar el vector entero con `SetMacroDef`); `SetMacroCount`; `SetRackSelector`; `AddReturn/RemoveReturn`. La aleatorización se resuelve en la UI como un lote de `SetMacro` (un único paso de undo).

## D.3 Motor: grafo de cadenas paralelas sin asignaciones

Estado actual `[V]` ya cumple: buffers de cadena y listas de eventos preasignados en la compilación, suma con ganancias, retardo de alineación con `DelayLine`. Mejoras:

1. **Zonas con fades** (todo aritmética, sin memoria): para NoteOn con tecla `k` y velocidad `v`: `v' = v · fade_gain(k, key_range, key_fade) · fade_gain(v, vel_range, vel_fade)` y descarta si 0 (Live: los fades atenúan la velocidad, `[V manual 25.5.2-25.5.3]`). Chain Select: `gain_c = fade_gain(selector, sel_range, sel_fade)` aplicado a la **salida** de la cadena (en Instrument/Audio Rack los fades atenúan el volumen, `[V manual 25.5.4]`); si 0 durante ≥ N bloques y sin cola, la cadena se duerme (ahorra CPU: es la razón de ser de los bancos de presets con Chain Select).
2. **Rampa de ganancia por cadena**: `ChainNode.gain` se hornea al compilar (`compile.rs:166-170`) y no se arrastra entre compilaciones (`adopt_devices` solo copia retardo y sleep, `graph.rs:~328-347`), así que un cambio de volumen de cadena es un escalón `[S, riesgo de clic, a comprobar con un test de escalón]`. Copiar el valor previo en `adopt` y rampar linealmente durante el bloque. Mismo mecanismo para el gate por selector.
3. **Drum Rack**:
   * **Receive/Play**: al reenviar el NoteOn/NoteOff a la cadena, sustituir `key` por `play_note` (por defecto Do3 = 60, `[S]` valor por defecto de Live); un NoteOff debe volver por el mismo camino: máscara de 128 bits por cadena (array fijo en la pila) para emparejar aunque se edite la cadena a mitad de nota.
   * **Choke**: un NoteOn a la cadena `c` con `choke = g ≠ 0` ordena una **rampa de 5 ms a 0** en la ganancia de salida de las otras cadenas del grupo `g` y, al terminar, un `AllNotesOff` a su lista de eventos (los instrumentos propios ya lo tratan: `sampler.rs:566`, `drum.rs:319`); así el choke funciona igual con cualquier instrumento, incluso VST. Reglas: la cadena que dispara no se choquea a sí misma, a diferencia del choke interno de `instrument.drum`.
   * **Returns y envíos**: `returns[i]` con un buffer preasignado; tras sumar las cadenas, cada una añade `buf · sends[i]` (post-fader) al buffer del return; los dispositivos del return se procesan sobre ese buffer y su salida se suma a la del rack (o al destino "Audio To" más adelante). **PDC**: el camino seco de las cadenas se retrasa `max_lat_return` para que no se adelante al camino húmedo, y la latencia del rack pasa a `max_lat_cadenas + max_lat_returns`.
4. **MIDI Rack y Rack con efectos MIDI**: la cadena entrega la unión de salidas (`merge_window`-style) como eventos al resto de la pista (§C.6).
5. **Sidechain**: (a) punto de toma configurable por fuente (Pre FX / Post FX / Post fader), hoy fijo Post FX (`graph.rs:1006-1012`); (b) fuente = una **cadena de rack/pad**, útil en un Drum Rack para ducking del bajo por el bombo dentro del mismo rack: buffer de sidechain interno al rack, preasignado, publicado tras cada cadena; (c) compensación de latencia del sidechain con un `DelayLine` si la fuente tiene menos latencia que el destino (`compile.rs:282-285` hoy lo ignora).
6. **Macros en vivo**: hoy cada cambio de macro sigue el camino de proyecto → recompilación (`engine.rs:631-700`). Para mover macros durante la reproducción sin recompilar, usar el camino ya existente de automatización: `ParamTarget::Macro(Vec<(Arc<SharedProcessor>, id, min, max)>)` (`graph.rs:~269`) con una cola SPSC desde el hilo de UI (`EngineBridge::set_macro_live(rack, i, v)`), y que el comando `SetMacro` siga reflejando el valor en el proyecto al soltar. `[S]` hay que medir si la recompilación en proyectos grandes es audible antes de acometerlo; el benchmark `docs/research/benchmark-24-tracks.md` sirve de línea base.
7. **Macros → plugins de terceros**: los ids de parámetro de VST3/CLAP son los mismos que usa `effective_params` (`compile.rs:118-127`), así que el mapeo ya funciona para ellos `[S]`; la UI debe resolver el nombre del parámetro vía catálogo (patrón de la nota `automation-lanes-ui`).

## D.4 Macros: diseño de mapeo, rango, 8-16-32 y variaciones

* **Modo Map** (como `[V manual 25.7.1]`): botón "Map" en la cabecera del rack; mientras está activo, **cualquier perilla de cualquier dispositivo del rack se vuelve seleccionable** (hover con borde de acento); clic en la perilla + elegir la macro `1…16` en un pequeño selector → `AddMapping` con `min = valor actual`, `max = 1` (o `min=0,max=1`). Reutilizar `KnobTarget` (`workspace.rs:33`) añadiendo una variante `MapPick`.
* **Lista de mapeos por macro**: panel inferior de la macro seleccionada con una fila por destino: nombre `Dispositivo › Parámetro`, **control de rango de dos asas** (min/max, invertible arrastrando una por encima de la otra), curva, botón borrar. Es lo que pide el dueño: "mapeo con rango min/max".
* **Rand**: botón que, en una sola transacción, escribe valores aleatorios en las macros no bloqueadas (cada macro tiene candado; `Macro.in_variations` aparte); el resultado respeta `steps`.
* **8 / 16 / 32**: `macro_count` cambia la vista (8 por defecto como Live, `[V manual 25.7]`; 16 ya es la capacidad del modelo; 32 como Live 13 `[V all-new-features]`). Aumentar `MACRO_COUNT` (`project.rs:23`) cuidando el límite del ID de widget `(id.0 << 8 | i)` usado en `devices.rs` (con 32 macros sigue cabiendo en 8 bits).
* **Variaciones**: ya hay Guardar/recuperar/borrar. Añadir: nombre editable, exclusión por macro, **morph** (interpolar entre la actual y la elegida durante N compases por el hilo de control con `SetMacro` coalescidos) y asignar una variación a un clip de sesión (follow actions existentes). Recuperar ya es un lote de `SetMacro` (`state.rs:1203-1226`), un paso de undo.
* **Automatización**: ya existe `AutomationTarget::Macro` (`project.rs:894`); verlo en los carriles `shown_lanes` de `automation-lanes-ui`.

## D.5 Zonas (UI)

Panel plegable "Zones" a la derecha de la lista de cadenas con tres pestañas **Key / Vel / Chain**. Cada cadena es una fila con una **barra de rango** sobre un eje de 0-127 (Key con teclado de referencia, nombres de nota) con asas en los extremos y **segundo par de asas más pequeñas** para el fade (triángulo sombreado). La pestaña Chain muestra además la **barra del selector** arrastrable, igual que el selector de Live (`[V manual 25.5.4]`). Drag con `Shift` = fino. Todo emite `SetChainProp`.

## D.6 Drum Rack (UI)

* **Pad View** de 4×4 con paginación (128 notas, banco de 16, `[V manual 25.6.1]`): clic = **audición** (NoteOn/Off por el puente de audición `app/src/audition.rs`), pad con el color de la cadena, nombre, nota, **minimedidor** y ranuras de Mute/Solo. Arrastrar un sample del navegador a un pad crea una cadena con `instrument.sampler` (o un Drum Sampler) ya cargado (el sampler ya carga WAV por `load_state`); arrastrar un dispositivo lo añade a esa cadena.
* **Menú contextual de pad**: Choke group (1-16), Receive note, Play note, Send 1-6, Borrar, Duplicar, Extraer a pista nueva.
* **Panel de returns**: hasta 6 cadenas de efecto bajo la rejilla (reverb, delay…) con su nivel de envío por pad en la lista de cadenas.
* **Enlace con el secuenciador**: el Step Sequencer ya existe en la app (`views/steps.rs`); su fila por pad debe mapear a las `chain.note`.
* Rendimiento `[V]`: la demo usa **un `instrument.drum` de 16 pads por cada pad** (`demo.rs:63-85`), ineficiente para 16 pads reales; la plantilla de Drum Rack de verdad debe usar `instrument.sampler` (monopad) por cadena y reservar `instrument.drum` para el modo "kit en una sola pieza".

## D.7 Rack de MIDI y plantillas

* **MIDI Effect Rack**: `RackKind::Midi` con cadenas paralelas de efectos MIDI; salida = unión (D.3.4). Depende de §C.6.
* **Plantillas de rack** (equivalentes de los Mid-Side Rack y Multiband Rack de Live 13 `[V all-new-features]`): no son un tipo nuevo, sino **constructores de comandos** en `model::racks` (patrón de `group_commands`, `racks.rs:77-102`): *Mid-Side* = dos cadenas con `fx.midside` decodificando (Mid/Side solo) para procesar el lado aparte; *Multiband* = tres cadenas, cada una con un filtro paso banda LR4 (hay que añadir **zona de frecuencia** a `Chain` o un dispositivo `fx.bandsplit` que emita la banda). Esfuerzo M.

## D.8 Importar racks y presets de Ableton (.adg / .adv / .alc)

### Lo verificado

* **Contenedor `[V]`**: los `.als`/`.adv` son XML comprimido con gzip. Fuentes: PRONOM de los Archivos Nacionales del Reino Unido sobre `.als` (gzip+XML desde Live 8.2.1) https://pronom.nationalarchives.gov.uk/fmt/1463 ; herramienta que lee `.adv` con `gzip` y `ElementTree` https://nurykabe.com/dump/tools/samplerProcessor/ ; hilo del foro "Decoding ALS file format" https://forum.ableton.com/viewtopic.php?p=1246935 . Ableton documenta `.adg` = device group (racks) y `.adv` = preset de dispositivo (resultado de búsqueda sobre su ayuda https://help.ableton.com/hc/en-us/articles/209769625 — la página devolvió 403 al descargarla, por lo que se cita solo el extracto del buscador). `.alc` = clip de Live con referencias a samples.
* **Estructura de un Set `[V]`** (documento comunitario https://tangled.org/aesthetic.computer/core/blob/main/docs/ableton-als-structure.md): raíz `<Ableton MajorVersion MinorVersion SchemaChangeCount Creator>` que envuelve un `<LiveSet>`; cada dispositivo es un elemento con el nombre de su tipo (`<Wavetable Id="0">`, `<Eq8 Id="0">`) con hijos `LomId`, `On`, `ParametersListWrapper`…; el valor de un parámetro vive en `<Manual Value="…"/>`. Ese documento **no cubre** racks, ramas, macros ni modulación.
* **No hay especificación oficial `[V]`**: un hilo del foro pregunta si existe documentación del esquema `.adg/.adv` y no se halló respuesta.
* **Parsers de código abierto MIT `[V, por resultados de búsqueda; revisar la licencia de cada repo antes de leer su código]`**: `@pohy/ableton` (npm), AbleSharp (.NET, https://github.com/theokyr/ablesharp), `ableton-live-tools` (https://github.com/danielbayley/ableton-live-tools). Sirven de referencia de formato; la implementación se escribe propia.
* **Dependencias ya presentes `[V]`**: `flate2 1.1.11` y `roxmltree 0.20/0.21` en `Cargo.lock`; MIT/Apache, permitidas por `deny.toml`.
* **En este equipo no hay Live ni ningún `.adg/.adv/.alc`**: se buscó en `/Applications` y `~/Music` (no se encontró nada). No se descargó ninguno (regla del encargo).

### Estructura esperada de un `.adg` `[S, de memoria; confirmar con un archivo real]`

Un grupo de dispositivos (`AudioEffectGroupDevice`, `InstrumentGroupDevice`, `DrumGroupDevice`, `MidiEffectGroupDevice`) con:
* `Branches` → una rama por cadena (nombre, color, volumen/pan/mute/solo, `ZoneSettings` con rangos de tecla/velocidad/selector y sus fades, `ChokeGroup`, `ReceivingNote`, `SendNote`);
* `DeviceChain` → `Devices` → los elementos de dispositivo nativos (`Compressor2`, `Eq8`, `Reverb`, `OriginalSimpler`, `InstrumentVector`(Wavetable), `GlueCompressor`…);
* `MacroControls.N`, `MacroDisplayNames.N`, `MacroDefaults.N`, `MacroVariations`, `ChainSelector`;
* los parámetros mapeados a macros con `<AutomationTarget Id>` / `<ModulationTarget Id>` y rangos `MidiControllerRange`/`Min`/`Max` (**el vínculo exacto macro↔parámetro no está confirmado; sin un archivo real no se debe escribir el importador**).
* Los valores de los parámetros se guardan **en unidades nativas** (Hz, dB, ms, índices de enumeración), no normalizadas.

### Qué se podría mapear

| Origen | Destino | Dificultad |
|---|---|---|
| Rack (tipos, cadenas, nombres, colores, volumen/pan/mute/solo) | `Rack` + `Chain` | Baja |
| Zonas Key/Vel (rangos) y Chain Select | `key_range`, `velocity_range`, `selector_range` (+ fades tras D.2) | Baja-media |
| Macros (nombres, valores, defaults) y variaciones | `Macro`, `Variation` | Baja |
| Mapeos macro→parámetro con min/max | `MacroMapping` | Media (necesita el mapeo de ids de parámetro, lo siguiente) |
| Drum Rack: nota Receive, Play, Choke, sends | `Chain.note/choke/play_note` (tras D.2) | Media |
| EQ Eight, Compressor, Glue Compressor, Limiter, Gate, Reverb, Delay, Saturator, Auto Filter, Chorus-Ensemble, Utility, Channel EQ | `fx.eq`, `fx.compressor`, `fx.buscomp`, `fx.limiter`, `fx.gate`, `fx.reverb`, `fx.delay`, `fx.saturator`, `fx.filter`, `fx.chorus`, `fx.utility` | Media por dispositivo: tabla de conversión por parámetro (unidad nativa → normalizado), con aviso de lo aproximado |
| Wavetable | `instrument.wavetable` | Alta, **aproximada** (otra forma de modulación y de tablas) |
| Operator | `instrument.vault` FM6 | Alta, aproximada |
| Simpler/Sampler/Drum Sampler | `instrument.sampler` | Alta: el sample es una **ruta** a un fichero del usuario o del Core Library de Live |
| Arpeggiator, Chord, Scale, Velocity… | `midi.*` (tras §C.6) | Media |
| Plugins VST3 | `DeviceKind::Plugin` si el UID coincide y está instalado; estado opaco | Media; el *chunk* de estado es binario propio del plugin |
| Max for Live (`.amxd` embebido) | — | Imposible |
| AU y VST2 | — | VST2 fuera de alcance; AU sin soporte |
| Automatización de clip, Packs, samples de Packs | — | No |

### Riesgos

1. **Formato sin esquema oficial y cambiante entre versiones** (`MinorVersion`, `SchemaChangeCount`, `Creator="Ableton Live 12.1.5 Suite"` en el ejemplo del documento comunitario): el importador debe ser tolerante a elementos desconocidos y registrar un *informe de importación* (mapeado / aproximado / descartado).
2. **Derechos**: leer un `.adg` propio del usuario es interoperabilidad; **no se redistribuyen conversiones** de racks/presets de Packs de Ableton ni su contenido (samples, diseño de sonido). Revisar el EULA de Live respecto a ingeniería inversa del formato `[S, consulta legal pendiente]`. No incluir fixtures de Ableton en el repositorio salvo racks creados por el propio proyecto.
3. **Fidelidad**: sonar distinto es la norma, sobre todo con instrumentos; el informe debe decirlo. Promesa realista: "importa tus racks de efectos y la estructura/macros de tus racks de instrumento; los sonidos de sinte se aproximan o se dejan vacíos con el nombre original".
4. **Tablas de conversión**: son muchas y frágiles; mantenerlas como datos (`.ron`/JSON) versionados con tests golden. Cada dispositivo nuevo del roadmap (`fx.buscomp`, `fx.autopan`, `fx.mbdyn`) añade una entrada.
5. **Seguridad**: un `.adg` descargado es entrada no confiable: límite de tamaño tras descomprimir (anti zip-bomb), profundidad máxima de anidación, nada de resolver rutas fuera de carpetas permitidas, importación como `Vec<Command>` que se aplica en bloque (un solo undo).
6. **Alcance**: empezar por **.adg de efectos** (el caso más rentable y más seguro), luego racks de instrumento con samples en carpetas del usuario, y tratar `.als` (importar proyectos enteros) como proyecto aparte.

### Diseño propuesto

Crate nuevo `crates/liveimport` (sin dependencias de GPUI ni del motor; solo `aitrack-model` + `flate2` + `roxmltree`): `import(bytes) -> ImportResult { commands: Vec<Command>, report: Vec<ReportLine> }`, que devuelve **comandos** y no muta el `Project` (regla "toda edición es un Command"). La UI lo muestra como "Importar rack de Ableton…" con el informe en un diálogo. Esfuerzo L (M para solo efectos). **Prerrequisito: que el dueño aporte 3-4 `.adg` reales suyos** (un Audio Effect Rack con macros, un Instrument Rack con zonas, un Drum Rack) para fijar las tablas con datos y no de memoria.

---

# PARTE E · Orden de implementación (S ≤ 1 sesión, M ≈ 2-4, L ≈ 1-2 semanas)

Los esfuerzos son estimaciones `[S]`.

| # | Trabajo | Esf. | Depende de | Motivo |
|---|---|---|---|---|
| 1 | Pads clicables + audición; volumen/pan/mute/solo de cadena en la UI (`SetChainProp`) | S | — | Hace usable el Drum Rack hoy, sin motor nuevo |
| 2 | **UI de mapeo de macros** (modo Map, rango min/max, lista de mapeos) | M | 1 (comandos `AddMapping`…) | Lo más pedido: sin esto los racks no se "sienten" como los de Live |
| 3 | `fx.autopan` | S | — | Citado por el dueño; DSP sencillo; extiende `dsp::lfo` |
| 4 | `fx.crusher` | S | — | Citado; sustituye al estilo Bitcrush mínimo |
| 5 | `fx.buscomp` | S-M | — | Citado; reaprovecha `GainSmoother` |
| 6 | `fx.mbdyn` | M | — | Citado (OTT); especificación existente en doc 07 |
| 7 | `fx.stutter` | M | — | Citado (stutter house); alto valor creativo |
| 8 | Rack: Receive/Play note + choke entre cadenas + rampas de ganancia por cadena | M | 1 | Drum Rack de verdad |
| 9 | Rack: zonas con fades y **Chain Select** (motor + UI de zonas) | M | 1 | Bancos de presets y capas |
| 10 | Rack: returns y envíos en Drum Rack (+ PDC) | M | 8 | Reverb común por kit |
| 11 | `fx.drumbus` | M | 5 | Compone piezas existentes |
| 12 | **Marco de efectos MIDI** (trait, `process_devices`, sleep, `note_id`, MIDI Rack) | L | — | Desbloquea los siete dispositivos |
| 13 | `midi.scale`, `midi.pitch`, `midi.velocity`, `midi.random`, `midi.notelength` | S cada uno | 12 | |
| 14 | `midi.chord`, `midi.arp`, `midi.echo` | M cada uno | 12 | El arpegiador y el acorde son los más usados |
| 15 | Macros en vivo (camino directo) + 8/16/32 + Rand + variaciones con exclusión/morph | M | 2 | Pulido |
| 16 | Sidechain: punto de toma + desde cadena de rack | M | 8 | |
| 17 | `fx.shifter` | M | — | |
| 18 | Auto Filter nuevo (tipos Morph/DJ/Comb/Vowel, S&H, Spin, sidechain externo): ampliación de `fx.filter` con ids nuevos | M | — | |
| 19 | Plantillas Mid-Side y Multiband Rack | M | 9 | |
| 20 | **Importador de .adg de efectos** | M | datos del dueño | |
| 21 | `instrument.granular`, `fx.driver`, Looper, Echo ampliado | L / M / M / M | — | |
| 22 | Importador de racks de instrumento | L | 20 | |
| 23 | Meld-like, Drum Synth, Spectral, Vocoder, Collision/Tension | L | — | Cola larga, baja prioridad para house/techno/stutter |

**Camino crítico**: 1 → 2 → 8 → 9 → 10 (racks) en paralelo con 3-7 (dispositivos de audio, independientes) y 12 (MIDI).

**Coste de cada dispositivo nuevo, comprobaciones antes de cerrar** (regla de CLAUDE.md): `cargo check --workspace`, `cargo test -p aitrack-stock` (robustness, alloc, presets, bench), `cargo clippy -p aitrack-stock`, y para UI `cargo test -p aitrack-app` (los editores tienen tests, p. ej. `dynamics.rs:470`). Ver memorias de integración: tabla de teclas en `keys.rs` si hay atajos nuevos; límite de 320 valores en `meter()`; los stubs de catálogo no tienen presets (la pestaña Presets requiere modo real).

---

# Anexo · Lo no verificado, a resolver pronto

1. Controles reales de Drum Buss, Echo y Auto Shift de Live 12 (el volcado del manual se cortó antes). Se recomienda releer https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/ por secciones antes de fijar los parámetros de §C.7.
2. Estructura exacta de `.adg` y vínculo macro→parámetro (necesita archivos reales).
3. Si una variación de volumen de cadena produce un clic en el motor actual (hipótesis de D.3.2: añadir un test de escalón).
4. Coste de recompilación al arrastrar una macro en proyectos grandes.
5. Valor por defecto de *Play note* en el Drum Rack de Live (se asume Do3).
6. Cifras del preset "denso" tipo OTT (ya señaladas como de memoria en doc 07 §0, líneas 735 y 1021).
7. El manual de Granulator III y de Meld (solo se confirmó su existencia/descripción breve).
8. Licencias exactas de los parsers MIT citados: abrir cada `LICENSE` antes de consultar su código.

# Fuentes

* Manual de Live 12, efectos de audio: https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/
* Manual de Live 12, efectos MIDI: https://www.ableton.com/en/live-manual/12/live-midi-effect-reference/
* Manual de Live 12, instrumentos: https://www.ableton.com/en/live-manual/12/live-instrument-reference/
* Manual de Live 12, racks (cap. 25): https://www.ableton.com/en/live-manual/12/instrument-drum-and-effect-racks/
* Comparación de ediciones (rotulada Live 13): https://www.ableton.com/en/live/compare-editions/
* Novedades de Live 13 (racks Multiband/Mid-Side, 32 macros, Drum Synth): https://www.ableton.com/en/live/all-new-features/
* Novedades de Live 12: https://www.musicradar.com/news/ableton-live-12-whats-new-devices-midi-workflow · https://www.engadget.com/ableton-live-12-whats-new-meld-roar-094528196.html
* Recuento de dispositivos de Suite (tercero): https://www.sweetwater.com/store/detail/Live12Ste
* Formato de archivo: https://pronom.nationalarchives.gov.uk/fmt/1463 · https://nurykabe.com/dump/tools/samplerProcessor/ · https://forum.ableton.com/viewtopic.php?p=1246935 · https://tangled.org/aesthetic.computer/core/blob/main/docs/ableton-als-structure.md · https://help.ableton.com/hc/en-us/articles/209769625 (extracto; 403 al abrir)
* Parsers (consulta de formato, licencia a revisar): https://www.npmjs.org/package/@pohy/ableton · https://github.com/theokyr/ablesharp · https://github.com/danielbayley/ableton-live-tools
* Documentos internos previos: `docs/research/04-plugins-dsp-fabfilter-waves.md` (§8 nombres y trade dress), `07-mezcla-mastering-plugins.md` (§4.3.3 multibanda, §4.3.10 otros), `12-dsp-plugin-suite.md` y su carpeta, `08-ableton-live-manual.md`, `docs/PENDING.md`.
* Bibliografía de DSP citada (la mayoría ya en doc 12): Giannoulis, Massberg y Reiss (JAES 2012); Zölzer (ed.), *DAFX* 2.ª ed.; Pirkle 2.ª ed.; Roads, *Microsound*; Puckette; Reiss y McPherson; Bilbao et al. (ADAA). Citas de esta ficha marcadas `[S]` (McNally 1984, Linkwitz 1976, Verhelst y Roelands 1993, Truax 1988, Karplus y Strong 1983, Niemitalo, Bode 1956, Roads 1996) están hechas de memoria: comprobar autor, año y revista antes de ponerlas en documentación pública.
