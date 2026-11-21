# 26 · Paridad de flujo de trabajo con Ableton Live 12.x / 13 (todo lo que no es un dispositivo)

> Fecha: 2026-11-21. Código: rama `main` en `0a4c4bf`, árbol limpio, 114 commits desde el 2026-11-15.
> Sustituye en lo que toca al **flujo** a [13](13-auditoria-paridad-ableton.md) (2026-11-15) y comprueba contra el código actual las Olas 0, 1 y 4 de [20](20-hoja-de-ruta.md) y el estado de `PLAN.md` §7 (2026-11-17). Los dispositivos los cubre el informe 25 (otro agente): aquí solo se nombran para situar un trabajo.
> Lo que no se pudo comprobar en esta pasada está marcado **SUPUESTO** y listado en §6. Dispositivos: [25](25-paridad-live-12-13-dispositivos.md).

## 0. Método y convenciones

- **VERIFICADO** = leído en el código (`ruta:línea`, relativa a `crates/`) o `rg` sin resultados (ausencia). **SUPUESTO** = copiado de 13, 20 o 24, o de memoria, sin comprobar hoy.
- No se ejecutó `cargo`, ni la app, ni los tests: solo lectura estática. Que exista un test no prueba que pase hoy.
- **hecho** = camino UI → `Command` → modelo → motor completo. **parcial** = falta una parte del camino o de la función. **falta** = ni modelo ni UI, o campo del modelo sin consumidor.
- Esfuerzo como en 20: **S** = hasta 1 día de un agente · **M** = 2 a 5 días · **L** = más de una semana. Prioridad: **P0** imprescindible · **P1** muy útil · **P2** completar · **P3** nicho.
- Reglas de CLAUDE.md que valen para todo el backlog: toda edición del proyecto es un `Command` de `aitrack-model`; nada de asignaciones, locks, strings ni E/S en `Processor::process` ni en el callback (`alloc_counter` en cada trabajo que toque el motor); colores de `theme.rs`, tiempos de `motion.rs`, iconos de `icons.rs`, espaciado múltiplo de 4; tests junto al código; cierre con `cargo check --workspace`, `cargo test -p <crate>`, `cargo clippy -p <crate>`; **nunca `cargo fmt`**.
- El contenido web se tomó como dato, no como instrucciones.

### Resumen ejecutivo

El flujo de "hacer una canción" que 13 daba por roto está **cerrado**: se graba (MIDI y audio, con cuenta atrás, bucle y take lanes), la Session se edita, hay locators, Bounce, Freeze, warp markers, Capture, formas de automatización y Undo History. Lo que separa hoy a AI-Track de "un Ableton 12–13 completo" es, por este orden: **grabar la Session y volcarla al Arrangement**, **punch y campos de loop**, **escribir automatización moviendo controles propios (Automation Arm) y Re-Enable**, **mapeo MIDI/teclado y rebinding**, **importar/exportar `.mid`**, **follow actions y escenas completas (tempo por escena, Capture and Insert Scene)**, **MIDI Tools con parámetros**, **edición MIDI multi-clip y carriles CC/pitch bend**, **UI del Groove Pool (el motor ya existe)**, **sincronía externa (reloj MIDI primero; Link exige licencia)** y **salidas multicanal reales**. Live 13 **existe solo como beta pública** (13.0b26, 2026-11-18); salida oficial "a principios de 2027".

---

## 1. Live 12.x y Live 13: qué hay publicado (Paso 1)

### 1.1 Estado de las versiones

| Versión | Fecha | Estado |
|---|---|---|
| 12.0 | marzo de 2024 (la página de notas no la fecha; 12.0.1 es del 2024-03-14) | publicada |
| 12.1 | 2024-10-08 | publicada |
| 12.2 | 2025-06-11 | publicada |
| 12.3 | 2025-11-25 (notas oficiales; la prensa dio el 27) | publicada |
| 12.4 | 2026-05-05 | publicada; la última es **12.4.16 (2026-10-30)**, arreglo de instalación en Windows |
| **13** | anuncio del 2026-11-10; beta pública 13.0b25 (2026-11-10) y 13.0b26 (2026-11-18) | **anunciada y en beta pública, no publicada.** Ableton: "Live 13 will be released in early 2027", sin fecha. Beta con plazas limitadas para Standard y Suite; toda compra de Live 12 incluye la actualización gratuita; exige macOS Sonoma o posterior |

Confirmación: blog oficial "What's coming in Live 13" (2026-11-10) y notas de la beta. Es una **beta**: las funciones pueden cambiar antes de la salida.

### 1.2 Funciones de flujo y versión en que entraron

URL base de las notas: `https://www.ableton.com/en/release-notes/live-12/` (12.x) y `https://www.ableton.com/en/release-notes/live-13-beta/` (13). Manual: `https://www.ableton.com/en/live-manual/12/<capítulo>/`, donde el capítulo es `session-view`, `arrangement-view`, `clip-view`, `audio-clips-tempo-and-warping`, `editing-midi`, `midi-tools`, `editing-mpe`, `using-grooves`, `using-tuning-systems`, `launching-clips`, `routing-and-i-o`, `mixing`, `recording-new-clips`, `bounce-to-audio`, `comping`, `stem-separation`, `automation`, `clip-envelopes`, `midi-and-key-remote-control`, `synchronizing-with-link-tempo-follower-and-midi`, `managing-files-and-sets`.

| Función de flujo | Entró en | Fuente |
|---|---|---|
| Scale Awareness (Scale Mode en la barra, Fold to Scale, Highlight Scale, "Use Current Scale" en dispositivos MIDI) | 12.0 | notas 12.0 · manual `editing-midi` |
| MIDI Tools: Transformations (Arpeggiate, Connect, Ornament, Quantize, Recombine, Span, Strum, Time Warp) y Generators (Rhythm, Seed, Shape, Stacks), con Auto Apply y Reset | 12.0 | notas 12.0 · manual `midi-tools` |
| Velocity Shaper y Rhythm Euclidean (Max for Live MIDI Tools) | 12.0 | notas 12.0 |
| Chop, Glissando y LFO (herramientas MPE), Stacks con bancos de acordes JSON, herramientas encadenables y mapeables a teclas/MIDI | 12.1 | notas 12.1 |
| Probability groups (Play All / Play One) y Velocity Deviation (sustituye a Velocity Range) | 12.0 | notas 12.0 · manual `editing-midi` §10.5 |
| Take lanes (comping) en todas las ediciones; 12.2 añade botón Show/Hide, Insert Take Lane y Delete (Unused) Take Lanes | 12.0 / 12.2 | notas · manual `comping` |
| Mixer en el Arrangement; Clip View y Device View a la vez | 12.0 | notas 12.0 |
| Navegación por teclado (Tab, menú Navigate), "momentary latching" | 12.0 | notas 12.0 · manual `accessibility-and-keyboard-navigation` |
| Tuning Systems (Scala `.scl`/`.ascl`, etiqueta Tunings, Bypass Tuning por pista); con escalas incorporadas en 12.1 | 12.0 / 12.1 | notas · manual `using-tuning-systems` |
| Editor MPE; "Resync External Hardware" en Link, Tempo & MIDI | 12.0 | notas 12.0 · manual `editing-mpe` |
| Búsqueda por similitud ("Show Similar Files") y Similar Sample Swapping en Simpler y Drum Rack (muestras de hasta 60 s) | 12.0 | notas 12.0 |
| Filtros por etiquetas, Tag Editor, búsqueda `#tag`; auto-etiquetado de muestras y VST3 (12.1); Content Columns y Quick Tags (12.2); crear etiquetas y grupos desde Quick Tags (12.4) | 12.0 a 12.4 | notas |
| Apply Grooves Instantly (Global Groove Amount, Auto Load Groove, Hot-Swap en Groove Pool) | 12.1 | notas 12.1 · manual `using-grooves` |
| Undo History (menú View, Ctrl/Cmd+Option+Z) | 12.1 | notas 12.1 |
| Find and Select Notes, carril Release Velocity, Split Clip (Ctrl/Cmd+Shift+E), Scale Awareness para clips de audio, escala por pista de Drum Rack | 12.1 | notas 12.1 |
| Recuperación tras fallo entre versiones distintas de Live | 12.1 | notas 12.1 (correcciones) |
| Bounce Track in Place y Bounce to New Track (Ctrl/Cmd+B); "Freeze and Flatten" pasa a llamarse Bounce Track in Place (Freeze sigue existiendo) | 12.2 | notas 12.2 · manual `bounce-to-audio` |
| Edición de automatización por teclado | 12.2 | notas 12.2 · manual `automation` |
| Scene Follow Actions con Linked/Unlinked o Longest | 12.2 | notas 12.2 |
| **Stem Separation** local (Vocals, Drums, Bass, Others; High Speed / High Quality; solo Suite) | 12.3 | notas 12.3 · manual `stem-separation` |
| Splice en el navegador (preview, descarga, Search with Sound, Apply Key from DAW) | 12.3 | notas 12.3 |
| Bounce Group to New Track / in Place y Paste Bounced Audio | 12.3 | notas 12.3 · manual `bounce-to-audio` |
| A/B de dispositivo (tecla P); Insert Empty MIDI Clip sin selección | 12.3 | notas 12.3 |
| Link Audio (audio entre pares Link, menú Link en la barra, página de ajustes) | 12.4 | notas 12.4 |
| Separate Stems for Time Selection y Merge to Single Track; Copy Time; Learn View (sustituye a Help View) | 12.4 | notas 12.4 |
| **Capture** también de automatización (botón o tecla N, ahora en Transport) | 13 beta | notas 13.0b25 |
| Contextual Browsing y Hot-Swap Device; Device Slots en el mixer | 13 beta | notas 13.0b25 |
| Envelope Clips en pistas de retorno | 13 beta | notas 13.0b25 |
| Export Tempo and Time Signatures (MIDI) y exportación MIDI a 480 ticks por negra | 13 beta | notas 13.0b25 |
| Link: modos Tempo Only, Start Stop y Arrangement; botón Follow Only | 13 beta | notas 13.0b25 |
| Hasta 32 macros con curva de mapeo propia; Macro Map Mode (Remote o Mod); Mid-Side Rack, Multiband Rack y Round Robin | 13 beta | notas 13.0b25 |
| Mixer: Unsolo/Uncue en Main, Show Sends as Sliders, Configure Mixer; Duplicate Without Content; Configure Track Controls | 13 beta | notas 13.0b25 |
| **Step Sequencer como tipo de clip** (8 a 128 pasos; longitud, velocidad, dirección y swing por fila; conversión a notas y vuelta) | 13 beta | notas 13.0b25 |
| ARA 2 en el Arrangement (no en Session); los modelos de stems se descargan al arrancar | 13 beta | notas 13.0b25 y blog oficial |
| Grupo de filtros Collections en el navegador; arrastrar una selección de clips de Session a la última pista visible | 13 beta (b26) | notas 13.0b26 |

Dispositivos nuevos de 13 (Drum Synth, Clipper y Transient Shaper con sidechain, Looplet, pack Softube) quedan para el informe 25. Prensa: no hay funciones de IA nuevas en 13.

Capítulos del manual 12 que sirven de referencia de comportamiento (leídos hoy): `session-view` (§7.2.1 tempo y compás por escena, §7.4.3 Capture and Insert Scene, §7.5 grabar Session en el Arrangement), `recording-new-clips` (§19: Session Record, MIDI Arrangement Overdub, Punch-In/Out, cuenta atrás, Record Quantization, Capture MIDI). Los detalles de follow actions (§8.3.1) y de stems (cap. 22) **no se descargaron** (la consulta fue rechazada): van como SUPUESTO.

---

## 2. Tabla de paridad por área (a)

### 2.1 Session View

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Lanzar clip, escena, parar pista, parar todo | **hecho** | `app/src/views/session.rs:100,353,407`, botón `stop-all` | — |
| Crear clip MIDI en slot, mover y copiar entre slots, soltar muestras y archivos del sistema | **hecho** | `app/src/session_edit.rs:173,214,236,386,417,442`; `session.rs:466-469` | — |
| Escenas: añadir, renombrar, borrar, duplicar | **hecho** | `session_edit.rs:303,317,335,355`; `session.rs:137-154,202` | — |
| Lanzar con teclado (Enter y flechas) | **hecho** | `app/src/keys.rs:312`, `commands.rs:104`, `session_edit.rs:136,370` | Select on Launch y Select Next Scene on Launch: `rg` vacío (falta) |
| Modo de lanzamiento y quantize por clip, como desplegables | **hecho** | `views/bottom.rs:776-790`, `workspace.rs:239-242` | Legato y Velocity de lanzamiento: `LaunchSettings` solo tiene quantize, modo y follow (`model/src/project.rs:701`) |
| Quantize global y Back to Arrangement en la barra | **hecho** | `views/topbar.rs:524,533` | — |
| Follow actions | **parcial** | modelo `project.rs:750` (Next, Previous, First, Random, Stop con `after_bars`); motor `engine/src/graph.rs:2401`; UI `views/bottom.rs:604-650,791-818` | Acción B con Chance, Play Again, Last, Any, Other, Jump, tiempo en negras y semicorcheas, Linked/Unlinked, Scene Follow Actions (12.2; `rg 'scene.*follow'` vacío). Detalle de las opciones de Live: SUPUESTO |
| Tempo y compás por escena | **falta** | `Scene.tempo` (`project.rs:482`) sin lector: `rg 'scene.*tempo'` en `engine/src` vacío; `session_edit.rs:308,339` solo lo copian | Controles Scene Tempo / Signature, botón de escena coloreado, cambio de tempo al lanzar |
| Capture and Insert Scene | **falta** | `rg` vacío | Crear escena con los clips que suenan y lanzarla sin corte |
| Grabar en slots (Clip Record, Session Record, Start Recording on Scene Launch, overdub de Session) | **falta** | `session.rs:294` solo arma la pista; `engine/src/record.rs` solo graba al Arrangement | Todo |
| Grabar Session → Arrangement (registro de lanzamientos) | **falta** | `rg` vacío | Todo (manual `session-view` §7.5) |

### 2.2 Arrangement

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Locators: añadir, mover, renombrar, anterior/siguiente, Loop to Next Locator | **hecho** | `model/src/project.rs:76-86`, `model/src/command.rs:233-237`, `app/src/editing/locators.rs:74-260`, `keys.rs:190-193` | Set Song Start Time Here: `rg 'song start'` vacío (falta) |
| Loop (llave, interruptor, Loop Selection) | **hecho** | `topbar.rs:500`, `keys.rs:195`, `state.rs:2001` | Campos numéricos de inicio y longitud: no hay `NumTarget` de loop |
| Punch in / out | **falta** | `rg -i punch` solo da nombres de presets | Todo (manual `recording-new-clips` §19) |
| Take lanes y comping | **hecho** | `model/src/takes.rs`, `command.rs:154-162`, `app/src/take_lanes.rs`, `keys.rs:268`, `take_lanes.rs:124` (Insert Take Lane), `state.rs:577,3599` | Delete Unused Take Lanes y audición de takes: `rg` vacío (falta) |
| Freeze, Unfreeze, Flatten | **hecho** | `command.rs:242-251`, `app/src/freeze.rs:116-212`, `editing/menus.rs:320-327` | — |
| Bounce to New Track, Bounce Track in Place, Paste Bounced Audio, Bounce de grupo | **hecho** | `keys.rs:209-211`, `app/src/bounce.rs:1-60` | — |
| Split, duplicar, consolidar, cortar, nudge; Duplicate, Insert, Delete, Cut y Copy Time | **hecho** | `keys.rs:204-232`, `model/src/ops.rs` | — |
| Groove Pool | **parcial** | modelo `model/src/groove.rs`, `command.rs:219-230`, motor `engine/src/compile.rs:711`, `engine/src/tests/groove.rs`. **Sin UI**: `rg 'AddGroove\|SetClipGroove\|SetGrooveAmount'` en `app/src` y `ai/src` vacío | Panel, selector por clip, Commit, extraer groove, Global Groove Amount |
| Retardo de pista (Track Delay) | **falta** | `rg` vacío | Campo en `Track`, PDC en el motor, UI |
| Automatización de tempo | **hecho** | `model/src/automation.rs`, `engine/src/automation.rs:71`, `compile.rs:614` | Edición de rampas en la regla |
| Cambios de compás | **parcial** | modelo `model/src/time.rs:46` y motor (`engine/src/rt.rs:712,1137`, `graph.rs:1717`) los aceptan; la UI solo toca la primera firma (`state.rs:1972`) | Insertar y editar cambios en la regla (manual `arrangement-view` §6.5) |
| Linked-Track Editing, Lock Envelopes | **falta** | `rg -i 'linked.?track\|lock.?envelope'` vacío | Manual `arrangement-view` §6.14, `automation` §26.9.2 |
| Solape de clips: lo que cae encima sustituye a lo que cubre (mover, pegar, duplicar, alargar) | **hecho** | `app/src/editing/overwrite.rs:1-40`, tests `editing/tests.rs:1337-1425` | — |
| Auto-crossfade entre clips adyacentes | **falta** | `rg -i crossfade` vacío en `editing/` y en `model/src/ops.rs` (solo fades por clip y la costura del bucle) | Si Live 12 lo hace solo al solapar: SUPUESTO |
| Mixer en el Arrangement; Clip y Devices apilados | **hecho** | `keys.rs:303-304,314` | — |

### 2.3 Vista de clip, MIDI y warp

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Warp activable y 6 modos; copia de `Source` al editar | **hecho** | `model/src/project.rs:958-971`, `app/src/state.rs:1711` (`set_warp_of`) | Parámetros por modo (Preserve, Granulation, Grain Size, Flux, Formants, Envelope): `rg` vacío |
| Warp markers (modelo, motor y editor) y Auto-Warp | **hecho** | `project.rs:888-936`, `app/src/views/sample_editor/warp.rs`, `sample_editor/mod.rs:400`, `app/src/auto_warp.rs`, `ai/src/beats.rs`, `bottom.rs:887` | Warp From Here, Set 1.1.1 Here: `rg` vacío |
| BPM del segmento editable con ÷2 y ×2 | **hecho** | `bottom.rs:397-434` | — |
| Start, Length y Loop editables; "Signature" | **parcial** | `bottom.rs:478-519` (editables); `bottom.rs:731-740` muestra la firma del proyecto | Firma propia del clip |
| Clip envelopes | **parcial** | `project.rs:615-620` (Volume, Pan, Pitch) | De parámetros de dispositivo y CC; Envelope Clips en returns (13) |
| MIDI Tools (Transformations y Generators) | **parcial** | acciones de un clic: `app/src/views/menus.rs:423-530` (acordes, inversión, progresión, arpegio, strum, chop, randomize) | Parámetros, Auto Apply, previsualización; Rhythm, Seed, Shape, Stacks, Euclidean, Connect, Ornament, Recombine, Span, Time Warp, Velocity Shaper, Glissando, LFO |
| Escalas: global, pista, clip; Fold y resaltado; Scale Awareness de audio | **hecho** | `model/src/scale.rs`, `topbar.rs:431`, `state.rs:849` | — |
| Tuning Systems (Scala) | **falta** | `rg` vacío (solo `Expression::Tuning` por nota, `engine/src/clip.rs:1013`) | Todo |
| Probabilidad por nota | **hecho** | carril Chance `views/piano_roll.rs:281-413`, `state.rs:995-1004` | Probability groups y Velocity Deviation: `rg` vacío |
| Ghost notes y multi-clip | **parcial** | `state.rs:871` (otros clips de la escena, como FL); el piano roll usa un solo `selected_clip` (`piano_roll.rs:26`) | Multi-clip hasta 8 clips, barras de bucle por clip, Focus Mode |
| Capture MIDI | **parcial** | `state.rs:2750`, `app/src/capture.rs`, `engine/src/record.rs:535`, `keys.rs:228` | Sin botón en la barra y sin atajo (`keys.rs:228` tiene `&[]`) |
| Drum Rack en el piano roll (nombres de pad, plegado a pads) | **falta** | `state.rs:863` solo pliega por escala; `rg 'pad\|drum' views/piano_roll.rs` vacío; el step sequencer sí (`views/steps.rs`) | Nombres de pad y Fold to Notes |
| Find and Select Notes, Legato, Join, Fold to Notes, carril Release Velocity | **falta** | `rg` vacío en `state.rs`, `views/bottom.rs`, `piano_roll.rs` y `menus.rs`; Humanize solo existe dentro de Randomize (`state.rs:1318`); `quantize` tiene parámetro de fuerza (`state.rs:1024`) pero la UI siempre pasa 1.0 (`workspace.rs:1759`) | Todo lo anterior, y Quantize con fuerza y swing |
| Carriles de CC, pitch bend y aftertouch | **falta** | `Note` es el único contenido (`project.rs:778`) | Contenedor de eventos, reproducción, grabación |
| Slice to Drum Rack y Slice to New MIDI Track | **hecho** | `app/src/editing/slice.rs:187`, `keys.rs:212` | — |
| Audio → MIDI desde el menú del clip | **falta** | solo por el chat (`views/ai_panel/slash.rs`); `editing/menus.rs` sin entrada | Entradas Convert Harmony, Melody, Drums |

### 2.4 Navegador, similitud, etiquetas, Splice

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Búsqueda por texto, BPM, tonalidad; favoritos, rating, etiquetas de usuario, carpetas virtuales | **hecho** | `library/src/query.rs:131-138,300` | — |
| Panel lateral de Sonidos con preview a tempo y tonalidad del proyecto y arranque en el siguiente compás | **hecho** | `app/src/views/sounds/mod.rs:1-9`, `sounds/logic.rs:164-199` | Conmutador Raw explícito: SUPUESTO |
| Similitud tímbrica (DSP) y búsqueda semántica (CLAP) | **hecho** | `views/palette/search.rs:183`, `palette/mod.rs:1890`; commits `06c76bb`, `4e04cfc` | — |
| Similar Sample Swapping en Sampler y Drum Rack | **falta** | `rg -i similar` vacío en `device_ui/sampler.rs` y `views/devices` | Anterior/siguiente, todos los pads, candado |
| Quick Tags editables, colecciones de color, búsquedas guardadas, atrás/adelante | **falta** | `rg` vacío en `views/sounds` y `palette` | Todo (12.2, 12.4, 13 b26) |
| Navegador contextual y Hot-Swap | **falta** | `views/menus.rs` ("Add device") sin filtro por posición, sin Recientes | 13 b25 |
| Presets de usuario por dispositivo | **hecho** | `views/device_window/chrome.rs:53,199` | Guardar rack o pista como preset: `rg` vacío |
| Arrastrar archivos del sistema | **hecho** | `views/arrangement.rs:695-696`, `session.rs:466-469` | — |
| Splice | **parcial** | cliente no oficial opt-in (`crates/splice`, `palette/splice.rs`, `palette/settings.rs:9`); carpeta local de Splice como colección (`sounds/mod.rs:1-6`); `key = None` en `palette/mod.rs:500` | Apply Key from DAW; decisión D1 de 20 sin cerrar (`Cargo.toml:12` aún lo incluye) |

### 2.5 Mixer y routing

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Selectores de entrada, salida y monitor por pista | **hecho** | `app/src/mixer_routing.rs:59-200`, `views/mixer.rs:330,353` | — |
| Envíos, retornos, VCA, grupos, solo | **hecho** | 13 §1.6 + `views/mixer.rs:264` | Pre/post fader sin UI: `Send.pre_fader` (`project.rs:322`) solo en tests y demo |
| Sidechain | **parcial** | `command.rs` (`SetSidechain`); `engine/src/compile.rs:496-499` ("nada de compensación") | PDC del sidechain, punto de toma, fuente desde cadena de rack |
| Device Slots interactivos y Unsolo global | **hecho** | `views/mixer.rs:186,230`, `keys.rs:226` | Configure Mixer, sends como sliders |
| Duplicate Without Content | **hecho** | `keys.rs:225` | — |
| Salidas hardware multicanal | **parcial** | Preferencias elige los canales de salida del Main (`views/prefs.rs:775`, `engine/src/audio_io.rs:322`) y el selector de pista ofrece "Out 3/4" (`mixer_routing.rs:82`), pero `Output::Hardware(_) => Dest::Out` (`compile.rs:122`): toda salida por hardware cae en el mismo bus | Mapeo real de cada pista a su par del dispositivo |
| A/B de dispositivo (12.3) | **falta** | `rg -i compare` vacío en `app/src` | Dos estados por dispositivo y atajo |
| Cue / Preview out, Crossfader A/B, Track Delay, Performance Impact | **falta** | `rg` vacío | Manual `mixing` §18.5 a §18.9 |
| Entrada Resampling, "Audio From" otra pista, MIDI Out, External Instrument | **falta** | `InputSource` solo `None` o `Hardware` (`project.rs:451-458`); no hay `midir::MidiOutput` | Manual `routing-and-i-o` §17.3 a §17.5 |

### 2.6 Grabación y monitor

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Grabar MIDI al Arrangement (cola sin bloqueos, posición de lo oído) | **hecho** | `engine/src/record.rs:1-70`, `engine/src/tests/recording.rs:76,149,417,520,697`, `state.rs:500,615` | — |
| Grabar audio por pista a WAV | **hecho** | `record.rs:273-330`, `tests/recording.rs:211-302,376,565,623,678` | — |
| Cuenta atrás, monitor In/Auto/Off, entrada por pista | **hecho** | `topbar.rs:508-515`, `mixer_routing.rs:145-200`, `tests/recording.rs:341` | — |
| Grabación en bucle: overdub y take lanes | **hecho** | `state.rs:615,3391,3599` | MIDI Arrangement Overdub como interruptor: no hay |
| Botón Record honesto | **hecho** | `topbar.rs:190-205,752` | — |
| Punch in / out; Record Quantization; pre-roll | **falta** | `rg` vacío | Manual `recording-new-clips` §19.5 |
| Ajustes de metrónomo (volumen, subdivisión, solo al grabar) | **falta** | solo interruptor y cuenta atrás (`engine/src/engine.rs:353-364`, click fijo `rt.rs:213`); `rg -i metronome views/prefs.rs` vacío | Manual `recording-new-clips` §19.4.1 |
| MIDI From por pista (puerto y canal) | **parcial** | página MIDI de Preferencias con un único destino para todo el MIDI en vivo: "Armed tracks" o una pista de instrumento (`views/prefs.rs:1081-1124`) | Entrada por pista, por puerto y por canal |
| Hardware MIDI de entrada con sello de tiempo | **hecho** | `engine/src/midi_io.rs:60-86`, `real/engine.rs:184-205` | — |

### 2.7 Automatización

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Lanes, selector de parámetro, puntos, curvas, rango | **hecho** | `views/arrangement.rs` (`render_chooser`), `model/src/automation.rs` | — |
| Formas de Live 12, asas de escala, sesgo y estiramiento, Simplify Envelope, edición por flechas | **hecho** | `model/src/automation.rs:126-159`, `keys.rs:230-242`, `workspace.rs:233` (`AutomationContext`); commit `7e48296` | — |
| Capture Automation | **hecho** | `app/src/capture.rs`, `state.rs:2706`, `keys.rs:229` | Sin botón ni atajo |
| Automation Arm | **parcial** | solo gestos de ventanas de plugin: `state.rs:2057-2118`, `keys.rs:272` | Controles propios (faders, knobs, sends, macros) |
| Re-Enable Automation / parámetros anulados | **falta** | `rg -i 're.?enable'` vacío; no hay estado de anulación de lanes en la app ni en el motor (`overridden` solo aparece en los tokens de tema, `prefs.rs:1657`) | Todo |
| Clip envelopes | ver §2.3 | | |

### 2.8 Mapeo MIDI, atajos, navegación

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Tabla única de atajos y hoja con buscador | **hecho** | `app/src/keys.rs:186-328`, `views/shortcuts.rs` | — |
| Rebinding de atajos | **falta** | `rg -i rebind` vacío; la hoja es de solo lectura (`prefs.rs:235`) | Todo |
| MIDI Map, Key Map, superficies de control | **falta** | `rg -i 'midi_map\|key_map\|control surface'` vacío | Manual `midi-and-key-remote-control` |
| Navegación por zonas (Alt+0 a 7), latching momentáneo, Shift+letra con teclado MIDI | **falta** | `rg -i 'latch\|navigate'`: solo el Stutter (Shift+clic) y un enum de medidores, sin relación | 12.0 (manual `accessibility-and-keyboard-navigation`) |
| Menú nativo de la aplicación | **falta** | `rg 'set_menus'` en `app/src` vacío | Edit con Undo/Redo etiquetado, View |
| Info View | **hecho** | `keys.rs:315`, `workspace.rs:375,3053-3056` (franja de estado) | Notas de usuario por pista y clip |

### 2.9 MPE, sincronía y Link

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| MPE (grabar, editar, ajustes multicanal) | **falta** | `rg -i mpe` solo en comentarios (`project.rs:795`) | Manual `editing-mpe` |
| Ableton Link y Link Audio | **falta** | `rg` vacío; PLAN §7 F7 y §10.7: su SDK es GPLv2 o licencia comercial | Licencia comercial |
| Reloj MIDI (entrada y salida, Start/Stop/SPP) | **falta** | `engine/src/midi_io.rs` abre las entradas con `Ignore::All`: ignora el reloj | Alternativa permisiva a Link |
| Tempo Follower | **falta** | `rg` vacío | Manual `synchronizing-with-link-tempo-follower-and-midi` §37.2 |

### 2.10 Stems, importar y exportar

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Separación de stems (cuatro stems, modelos ONNX bajo demanda, propuesta aceptable) | **parcial** | `app/src/workspace.rs:2523-2561`, `real/ai.rs:135` | Solo por el chat; sin menú de clip o navegador, sin selección de tiempo ni Merge to Single Track (12.4). Modos High Speed y High Quality: SUPUESTO |
| Exportar mezcla y stems: WAV 16, 24, 32f; MP3 320; dither; rango; cola; normalizar | **hecho** | `engine/src/export.rs:77-95`, `views/export.rs:94` | — |
| Exportar FLAC, AIFF, OGG; mono; Render as Loop | **falta** | solo WAV y MP3 | — |
| Importar y exportar `.mid`; exportar mapa de tempo | **falta** | no hay `midly` en ningún `Cargo.toml` (solo `midir`) | Manual `managing-files-and-sets` §5.2 |
| Importar audio en WAV, FLAC, MP3, OGG, AAC/M4A, ALAC, AIFF | **hecho** | `engine/Cargo.toml:32`, `app/src/session_edit.rs:79` | — |
| Exportar clips de Session como Sets, fusionar Sets | **falta** | `rg` vacío | Manual §5.4.3 y §5.4.4 |

### 2.11 Historial de undo, proyectos, Splice

| Función | Estado | Evidencia | Falta frente a Live |
|---|---|---|---|
| Undo History con salto a cualquier paso | **hecho** | `workspace.rs:3040-3054`, `views/menus.rs:1388-1428` (24 undo y 12 redo) | Atajo propio y entrada en un menú View |
| Autosave y recuperación | **hecho** | `app/src/autosave.rs:20-226`, `workspace.rs:3785`, tests `autosave.rs:245-410` | — |
| Rutas relativas, Collect All and Save, buscar archivos perdidos | **hecho** | `model/src/paths.rs:28-177`, `app/src/project_io.rs:326,656,706,717`, `keys.rs:322-323` | Find Unused Files |
| Formato versionado, `.bak`, escritura atómica | **hecho** | `model/src/project.rs:211`, `project_io.rs:83-123` | — |
| Plantillas | **parcial** | tres incorporadas (`project_io.rs:345-352`) | Plantillas de usuario y Save as Default Set: `rg` vacío |

---

## 3. Estado de las Olas 0, 1 y 4 de [20](20-hoja-de-ruta.md) (b)

La columna "Estado" de 20 dice **VERIFICADO** para el *hueco* (el problema se vio en el código), no para el trabajo. Esta tabla da el estado real del **trabajo**.

### Ola 0 (0.1 a 0.18)

| # | Estado | Evidencia |
|---|---|---|
| 0.1 | hecho | `engine/src/graph.rs:30,41` (`catch_unwind` por nodo), `audio_io.rs:509`, `status.rs:201-259`, `app/src/real/engine.rs:459-475`, test `engine/src/tests/engine_safety.rs:190,276` |
| 0.2 | hecho | `compile.rs:186-199` (`break_cycles`), `:465` (ruta inválida a Main con aviso), tests `engine_safety.rs:357-416`, UI `real/engine.rs:471` y `workspace.rs:2997` |
| 0.3 | hecho | `app/src/real/engine.rs:184-205`, test `:1300` |
| 0.4 | hecho | `app/src/real/preview.rs:28` (`GAIN = 1.0`), `:415-420`, test `:565` |
| 0.5 | hecho | `engine/src/clip.rs:1041,1093` (`cuts_tail`) |
| 0.6 | hecho | `engine/src/graph.rs:375-377,770-772` (`primed`) |
| 0.7 | hecho | `clip.rs:1129,1980` (frame 0), `:2015-2018` (residuo < −120 dB), `engine/src/automation.rs:51-55`, `export.rs:762-773` |
| 0.8 | hecho | `engine/src/sample_cache.rs:265,1113,1125`, `engine/Cargo.toml:32` |
| 0.9 | hecho | `app/src/state.rs:2254-2275`, test `:3033` |
| 0.10 | hecho (superado por 1.4 y 1.5) | `views/topbar.rs:190-205`, test `:752` |
| 0.11 | hecho | `model/src/history.rs:158-222`, test `:358`; `state.rs:3033-3041` |
| 0.12 | hecho | `engine/src/shared.rs:370-386` (`sync_automation`) |
| 0.13 | hecho | `engine/src/graph.rs:1549-1556,2020`, `status.rs:201,243` |
| 0.14 | hecho | `app/src/workspace.rs:2736,3672-3699`, test `:4203` |
| 0.15 | hecho | `workspace.rs:3607-3615`, test `:4305` |
| 0.16 | hecho | `views/bottom.rs:397-434`, test `:1324` |
| 0.17 | hecho | `engine/src/clip.rs:170-186` (reserva de NoteOff) |
| 0.18 | hecho | `app/src/real/engine.rs:1259-1260,1374` |

### Ola 1 (1.1 a 1.16)

| # | Estado | Evidencia |
|---|---|---|
| 1.1 | hecho | `app/src/autosave.rs:20-226`, `workspace.rs:351-354,3785`, tests `autosave.rs:245-410` |
| 1.2 | hecho | `state.rs:2277-2335,2355-2383`, `keys.rs:266`, `views/arrangement.rs:1279-1281`, `editing/menus.rs:50-114,354-362`, test de limpieza de `RemoveTrack` en `model/src/command.rs` (`rig`) |
| 1.3 | hecho | `engine/src/midi_io.rs:60-86` (sello a offset), `views/prefs.rs:1044-1124` |
| 1.4 | hecho | `engine/src/record.rs`, `tests/recording.rs:76,149,417,520,697`, `state.rs:500,615,3391` |
| 1.5 | hecho | `record.rs:273-330`, `tests/recording.rs:211-302,623,678` |
| 1.6 | hecho | `app/src/session_edit.rs:173-456`, `views/session.rs` |
| 1.7 | hecho | `app/src/mixer_routing.rs:59-200`, `views/mixer.rs:330,353` |
| 1.8 | hecho | `model/src/ops.rs:25-60`, `state.rs:1944-1957` |
| 1.9 | hecho | `model/src/paths.rs:28-177`, `project_io.rs:326,656,706,717` |
| 1.10 | hecho | `model/src/project.rs:211`, `project_io.rs:83-123,881-901` |
| 1.11 | hecho | `engine/src/engine.rs:1240-1275` (`param_patches`), `rt.rs:771-823`. Las macros siguen recompilando (`Patch::MacroValue => continue`) |
| 1.12 | hecho | `sample_cache.rs:686-722,839-848` (worker), `:415-420,520-535` (presupuesto), `clip.rs:305` |
| 1.13 | hecho | `app/src/real/plugins.rs:448-553`, tests `:988,1059` |
| 1.14 | hecho | `views/arrangement.rs:695-696`, `views/session.rs:466-469`, `session_edit.rs:417` |
| 1.15 | hecho | `views/topbar.rs:508-535` |
| 1.16 | hecho, con una salvedad | `views/bottom.rs:478-519,731-790`; "Signature" muestra la firma del proyecto, no la del clip |

### Ola 4 (4.1 a 4.13, 4.R1, 4.R2)

| # | Estado | Evidencia |
|---|---|---|
| 4.1 | hecho | `dsp/src/lib.rs:43,181` (`EventSink`, `process_midi`), `engine/src/tests/midi_fx.rs` |
| 4.2 | hecho | `stock/src/lib.rs:80-87` (arp, chord, scale, velocity, echo, pitch, length, random) |
| 4.3 | falta | no existe `crates/liveimport`; depende de la decisión D5 (archivos `.adg` del dueño) |
| 4.4 | parcial | follow actions con UI y acción única (`bottom.rs:604-818`, `graph.rs:2401`); faltan tempo por escena, Capture and Insert Scene, grabar Session en el Arrangement |
| 4.5 | hecho | `command.rs:242-251`, `freeze.rs:116-212`, `bounce.rs`, `keys.rs:209-211`; el criterio "CPU ~0 y nulo < −90 dB" no se comprobó (SUPUESTO) |
| 4.6 | hecho | `model/src/project.rs:888-936`, motor (`c255122`), `views/sample_editor/warp.rs`; faltan Warp From Here y Set 1.1.1 |
| 4.7 | parcial | locators hechos (`project.rs:76-86`, `editing/locators.rs`); faltan campos numéricos de loop, punch y `.mid` |
| 4.8 | parcial | Chance hecho (`piano_roll.rs:281-413`); clip envelopes de volumen, pan y pitch; faltan carriles CC y pitch bend, Automation Arm de controles propios y Re-Enable |
| 4.9 | falta | `compile.rs:496-499`; sin punto de toma; sin fuente desde cadena de rack |
| 4.10 | falta | `model/src/time.rs:120-170` (conversión lineal); `engine/src/delay.rs:57-63` solo conserva el audio si el retardo no cambia |
| 4.11 | parcial | take lanes y Undo History hechos; faltan mapeo MIDI y de teclado, rebinding, menú nativo, Link, salidas multicanal reales y AU |
| 4.12 | parcial (informe 25) | `stock/src/lib.rs:36-95`: frecuencia, pitch, vocoder, granular y filtro existen; faltan `fx.driver`, Looper |
| 4.13 | falta | `rg -i freesound` vacío (decisión D2 aplazada) |
| 4.R1 | falta | `sample_cache.rs:983-990`: el test aún tolera hasta ~1 muestra |
| 4.R2 | parcial | send a −70 dB mostrado como −inf (`state.rs:1599,2886`); `Entry::Failed` no se reintenta (`sample_cache.rs:901`); el resto SUPUESTO |

---

## 4. Backlog priorizado de lo que falta (c)

Esfuerzo: S, M, L (§0). "Dep." = trabajo previo. Rutas relativas a `crates/`. Cada trabajo respeta las reglas de §0.

### P0 · imprescindible

| # | Trabajo | Esf. | Dep. | Ficheros | Hecho cuando |
|---|---|---|---|---|---|
| P0.1 | **Punch in/out, campos de loop (inicio y longitud) en la barra, interruptor MIDI Arrangement Overdub y Record Quantization** | M | — | `model/src/project.rs` (`punch` con `serde(default)`), `command.rs` (`SetPunch`), `engine/src/record.rs` y `rt.rs` (ventana de punch sin asignar), `app/src/views/topbar.rs`, `state.rs`, `keys.rs` | Grabar con punch deja solo lo de dentro; un paso de undo; test `alloc_counter` |
| P0.2 | **Grabar en la Session**: botón de grabación en slots vacíos de pistas armadas, Session Record, Start Recording on Scene Launch, overdub de Session | L | `record.rs` (existe) | `engine/src/record.rs`, `rt.rs`, `graph.rs` (estado de slot grabando), `app/src/views/session.rs`, `session_edit.rs`, `state.rs` | Grabar 4 compases en un slot crea un clip en bucle; un paso de undo |
| P0.3 | **Grabar Session → Arrangement** (lanzamientos, cambios de propiedades, mezcla y dispositivos) | L | P0.2 recomendable | `engine/src/graph.rs` (cola sin bloqueos de lanzamientos con tick), `record.rs`, `app/src/state.rs`, `topbar.rs` | Lanzar tres escenas con Record deja los clips en sus compases; un paso de undo |
| P0.4 | **Automation Arm para controles propios** (faders, knobs, sends, macros) y **Re-Enable Automation** con botón en la barra | M | — | `app/src/state.rs:2057-2118` (reutilizar `automation::write`), `workspace.rs`, `views/arrangement.rs`, `topbar.rs`, `engine/src/shared.rs` (anulación en memoria) | Mover un fader grabando escribe el lane; al pisar un lane aparece Re-Enable |
| P0.5 | **Mapeo MIDI y de teclado** (modo Map, tabla en proyecto/preferencias, lanzar clips, transporte, parámetros) y **rebinding de atajos** | L | 1.3 (hecho) | nuevo `model/src/mapping.rs`, `engine/src/rt.rs` y `midi_io.rs` (tabla prealocada), `app/src/keys.rs`, `views/prefs.rs`, `workspace.rs` | Un CC mueve un fader, una nota lanza una escena y `mod-d` se reasigna; sobrevive al reinicio |
| P0.6 | **Importar y exportar `.mid`** y exportar mapa de tempo y compases | M | `midly` (comprobar licencia con `cargo deny`: SUPUESTO MIT o Apache) | nuevo módulo SMF en `app/src/` o `model/src/`, `session_edit.rs`, `views/arrangement.rs` (soltar), `views/export.rs`, `editing/menus.rs` | Soltar un `.mid` crea clip(s) a ±1 tick; exportar y reimportar es idéntico. El modelo usa PPQ 960 (`model/src/time.rs:8`) y Live 13 exporta a 480: convertir con redondeo y probar la ida y vuelta |
| P0.7 | **Cerrar la decisión D1 de Splice**: cliente no oficial fuera de las builds de release (feature desactivada por defecto) y filtro de tonalidad conectado | S | decisión del dueño | `Cargo.toml:12`, `crates/splice`, `app/Cargo.toml`, `views/palette/mod.rs:500`, `NOTICE` | La build de release no contiene el cliente; `key` llega a la búsqueda |

### P1 · muy útil

| # | Trabajo | Esf. | Dep. | Ficheros |
|---|---|---|---|---|
| P1.1 | Follow actions completas (A/B, Chance, Play Again, Last, Any, Other, Jump, tiempo musical, Linked/Unlinked) y Scene Follow Actions | M | — | `model/src/project.rs:701-755` (estructura nueva con migración), `engine/src/graph.rs:2401`, `views/bottom.rs:604-818`, `views/menus.rs` |
| P1.2 | Tempo y compás por escena (consumo en el motor, controles en la columna Main) | M | — | `model/src/project.rs:482`, `engine/src/rt.rs`, `session_edit.rs:308`, `views/session.rs` |
| P1.3 | Capture and Insert Scene | S | — | `app/src/session_edit.rs`, `views/session.rs`, `keys.rs` |
| P1.4 | MIDI Tools con parámetros, Auto Apply y previsualización como propuesta (primero Rhythm/Euclidean, Arpeggiate, Strum, Quantize con fuerza, Velocity Shaper, Stacks) | L | — | nuevo `app/src/views/midi_tools/`, `state.rs`, `views/menus.rs:423-530`, `crates/ai` (teoría) |
| P1.5 | Drum Rack en el piano roll: nombres de pad y plegado a pads | S | — | `app/src/state.rs:863`, `views/piano_roll.rs` |
| P1.6 | Edición MIDI multi-clip (hasta 8) con Focus Mode | L | — | `views/piano_roll.rs:26`, `state.rs` |
| P1.7 | Carriles CC, pitch bend, aftertouch y Release Velocity; grabarlos | L | — | `model/src/project.rs:766-803`, `engine/src/clip.rs`, `engine/src/record.rs`, `views/piano_roll.rs` |
| P1.8 | UI del Groove Pool (panel, groove por clip, Commit, extraer groove, cantidad global) | M | motor listo | nuevo `app/src/views/groove.rs`, `views/bottom.rs` |
| P1.9 | Warp: Warp From Here, Set 1.1.1 Here, parámetros por modo | M | — | `views/sample_editor/warp.rs`, `model/src/project.rs` (`Warp`), `dsp/src/warp.rs` |
| P1.10 | Cambios de compás y mapa de tempo editables en la regla | M | — | `state.rs:1972`, `views/arrangement.rs`, `model/src/time.rs` |
| P1.11 | Salidas multicanal reales | L | — | `engine/src/compile.rs:122`, `audio_io.rs`, `graph.rs`, `views/prefs.rs` |
| P1.12 | Sidechain: PDC, punto de toma y fuente desde cadena de rack (4.9) | M | — | `engine/src/compile.rs:496-499`, `graph.rs` |
| P1.13 | Menú nativo con Edit (Undo/Redo etiquetado), View (Undo History) y los atajos de menú | M | `App::set_menus` existe en `vendor/gpui-pre/src/app.rs:2799` (macOS `gpui-pre-macos/src/platform.rs:1150`, Windows `gpui-pre-windows/src/platform.rs:938`); no hay que tocar el vendor | `app/src/main.rs`, `keys.rs` (la tabla alimenta el menú), `workspace.rs` |
| P1.14 | Reloj MIDI (entrada y salida) y puertos MIDI de salida con External Instrument | M | — | `engine/src/midi_io.rs`, `rt.rs`, `app/src/views/prefs.rs` |
| P1.15 | Entrada Resampling y "Audio From" otra pista | M | — | `model/src/project.rs:451-458`, `engine/src/record.rs`, `app/src/mixer_routing.rs` |
| P1.16 | Similar Sample Swapping en Sampler y Drum Rack | M | — | `views/device_ui/sampler.rs`, `views/devices.rs`, `library/src/query.rs` |
| P1.17 | Menú del clip y del navegador: Separate Stems (con selección de tiempo y Merge to Single Track) y Convert Harmony, Melody y Drums to New MIDI Track (hoy solo por el chat) | M | — | `app/src/workspace.rs:2523-2561`, `editing/menus.rs`, `views/ai_panel/slash.rs`, `crates/ai` |
| P1.18 | Plantillas de usuario, Save as Default Set, guardar rack o pista como preset | M | — | `app/src/project_io.rs:345-352`, `views/device_window/chrome.rs` |
| P1.19 | Botón Capture en la barra y atajo | S | — | `views/topbar.rs`, `keys.rs:228-229` |
| P1.20 | Clip envelopes de parámetros de dispositivo y CC | M | — | `model/src/project.rs:615-620`, `views/bottom.rs` |

### P2 · completar

| # | Trabajo | Esf. | Ficheros |
|---|---|---|---|
| P2.1 | Quick Tags, colecciones de color, búsquedas guardadas, atrás/adelante, Raw | M | `views/sounds/`, `library/src/db.rs` |
| P2.2 | Navegador contextual, Recientes y Hot-Swap | M | `views/menus.rs`, `views/devices.rs` |
| P2.3 | Probability groups y Velocity Deviation | M | `model/src/project.rs:778`, `engine/src/clip.rs` |
| P2.4 | Find and Select Notes, Humanize, Legato, Join | M | `app/src/state.rs`, `views/piano_roll.rs` |
| P2.5 | Linked-Track Editing y Lock Envelopes | M | `app/src/editing/`, `model/src/ops.rs` |
| P2.6 | Mixer: Track Delay, Cue y Preview out, Configure Mixer, sends como sliders, pre/post fader, Performance Impact | M | `model/src/project.rs`, `engine/src/graph.rs`, `views/mixer.rs` |
| P2.7 | Exportar FLAC, AIFF, OGG, mono y Render as Loop | M | `engine/src/export.rs`, `views/export.rs` |
| P2.8 | Find Unused Files, empaquetar proyecto, Merge Sets, exportar clips de Session como Set | M | `app/src/project_io.rs`, `model/src/paths.rs` |
| P2.9 | Crossfade al cambiar de grafo o de latencia, tempo map O(log n), reintento de `Entry::Failed`, residuo del resampler (4.10, 4.R1, 4.R2) | M | `engine/src/delay.rs`, `model/src/time.rs`, `sample_cache.rs` |
| P2.10 | Navegación por zonas, latching momentáneo, Shift+letra con teclado MIDI | M | `app/src/keys.rs`, `commands.rs` |
| P2.11 | Step Sequencer por fila (longitud, velocidad, dirección, Chance, Repeat) | M | `views/steps.rs`, `model/src/project.rs` |
| P2.12 | Chord Track | M | `model/src/project.rs`, `crates/ai/src/chords.rs` |
| P2.13 | MPE: grabar y editar | L | `engine/src/record.rs`, `views/piano_roll.rs` |
| P2.14 | Notas de usuario por pista y clip; campo de posición editable (hoy solo conmuta MM:SS y compases, commit `b69d6ee`) | S | `model/src/project.rs`, `views/topbar.rs` |
| P2.15 | Metrónomo configurable (volumen, subdivisión, solo al grabar) y ajustes de Record, Warp y Launch por defecto (Start Playback with Record, Select on Launch, Legato y Velocity de lanzamiento, fundidos en los bordes de clip) | M | `engine/src/rt.rs:213`, `engine.rs:353-364`, `model/src/project.rs:701`, `views/prefs.rs` (no tiene página de Record) |

### P3 · nicho

| # | Trabajo | Esf. | Nota |
|---|---|---|---|
| P3.1 | Ableton Link y Link Audio | L | licencia comercial (PLAN §10.7); el reloj MIDI (P1.14) cubre lo esencial sin licencia |
| P3.2 | Tempo Follower | L | `engine/src/` (detección de pulso) |
| P3.3 | Superficies de control (Push, Launchpad) | L | tras P0.5 |
| P3.4 | Tuning Systems (Scala) | M | `model/src/scale.rs`, `engine/src/clip.rs:1013` |
| P3.5 | Crossfader A/B | M | `engine/src/graph.rs` |
| P3.6 | Envelope Clips en returns (13) | M | `model/src/project.rs` |
| P3.7 | ARA 2 | L | host |
| P3.8 | Importador `.adg` (4.3) | M | decisión D5 |
| P3.9 | Learn View | M | `app/src/views/` |
| P3.10 | Freesound (4.13) | L | decisión D2 |
| P3.11 | Audio Units | L | `host/` |

---

## 5. Estados obsoletos en 13, 20, 24-03, PLAN §7 y PENDING

### 5.1 Informe 13 (2026-11-15)

| Sección de 13 | Decía | Hoy (verificado) |
|---|---|---|
| §1.1 | Count-in, quantize global y Back to Arrangement "solo en el motor, sin uso en la app" | hecho: `views/topbar.rs:508-535` |
| §1.2 | "No se puede grabar": MIDI, audio, overdub, take lanes, entrada MIDI, monitor y entrada por pista, todo ausente o crítico | hecho: `engine/src/record.rs`, `tests/recording.rs`, `app/src/mixer_routing.rs`, `real/engine.rs:184-205`. Siguen sin existir punch y grabar en la Session |
| §1.3 | Session sin crear ni mover clips; 8 escenas fijas; modo y quantize que ciclan; follow actions sin UI; sin teclas de lanzamiento | hecho (`session_edit.rs`, desplegables, `keys.rs:312`); follow actions con UI pero de acción única. Siguen: tempo por escena, Capture and Insert Scene, grabar Session |
| §1.4 | Warp markers fantasma, Seg. BPM de solo lectura, sin Freeze/Flatten/Bounce, sin locators, sin arrastrar desde Finder, solapes dudosos, sin borrar tiempo, sin color de clip | hecho: `project.rs:888-936`, `bottom.rs:397-434`, `freeze.rs`, `bounce.rs`, `editing/locators.rs`, `editing/overwrite.rs`, `keys.rs:220`, `editing/menus.rs:107` |
| §1.5 | Sin dispositivos MIDI; probabilidad sin UI; Start y Length no editables | hecho: `stock/src/lib.rs:80-87`, `piano_roll.rs:281-413`, `bottom.rs:478-519` |
| §1.6 | Sin borrar, duplicar, reordenar ni colorear pistas (crítico); selector de salida de solo lectura; sin presets de usuario | hecho (`state.rs:2277-2383`, `mixer_routing.rs`, `device_window/chrome.rs:53`); sigue sin "guardar rack o pista" |
| §1.7 | Automatización por teclado ausente; clip envelopes solo de volumen | hecho; Volume, Pan y Pitch (`project.rs:615-620`) |
| §1.8 | Browser persistente "ausente por decisión de diseño"; preview sin sync; sin arrastrar archivos; sin servidor MCP | hecho: panel de Sonidos (`keys.rs:308`), `sounds/logic.rs:164-199`, `crates/mcp` |
| §1.9 | Sin autosave, sin rutas relativas ni Collect All and Save, historial sin lista visible, sin preferencias MIDI | hecho: `autosave.rs`, `model/src/paths.rs`, `menus.rs:1388-1428`, `prefs.rs:1044-1124` |
| §2 (17 puntos) | "Mal implementado" | resueltos 1 a 4, 6, 7, 10 a 12, 14 y 15; en parte 5 (la firma sigue siendo la del proyecto), 8 (solo Chance tiene UI) y 17 (hay Undo History y etiqueta en la barra de estado, no menú nativo); siguen 9, 13 y 16 |
| §3 | Live 13 "de prensa" | sustituido por las notas oficiales (§1); Undo History, Bounce y automatización por teclado ya no faltan |
| §4 (25 trabajos) | Plan | hechos: 1 a 9, 11 a 13, 20, 23. Parciales: 10, 15, 16, 17, 18, 21, 22, 24, 25. Faltan: 14 (`.mid`) y 19 (mapeo MIDI y rebinding) |

### 5.2 Informe 20 (2026-11-15)

- La columna "Estado" de las Olas 0, 1 y 4 dice **VERIFICADO** para el *hueco* (la convención de 20 §0), no para el trabajo. Hoy todas las filas de Ola 0 y 1 están hechas; 1.4 y 1.5 ya no son "VERIFICADO (ausencia)"; 4.1, 4.2, 4.5 y 4.6 están hechas y 4.7 solo conserva el hueco de loop, punch y `.mid`.
- Cabecera y §6: "el informe 16 no existía" y "PENDIENTE" son obsoletos: existe `16-plugins-host-verificacion.md`.
- §0 y §5: "Nada está commiteado" es obsoleto: hay 114 commits desde el 2026-11-15 y las versiones 0.0.1 a 0.0.8.
- §1: la fila 13 (la app nunca abre puertos MIDI) y la 8 (arrastrar un knob recompila el grafo) están resueltas (0.3, 1.3, 1.11). La fila 7 (panel del navegador) se resolvió con el panel de Sonidos. En la fila 4, `MoveTrack` y `AddScene` ya se emiten desde la app (`state.rs:2311`, `session_edit.rs:308`).
- §3, plan de carriles: sin vigencia (las olas 0 y 1 están cerradas).

### 5.3 24-03 (2026-11-17), PLAN §7 y PENDING

- **24-03 §1 y §5**: "follow actions sin UI", "locators ausentes", "probabilidad sin UI", "Bounce ausente", "Undo sin panel", "sin Capture", "Clip y Devices apilados", "sin Copy Time", device slots y Unsolo se resolvieron el 11-18 y 11-19 (`522aa8e`, `7e48296`, `b69d6ee`). Siguen vigentes MIDI Tools con parámetros, multi-clip, navegador contextual y Step Sequencer por fila.
- **PLAN §7 (estado 11-17)**: ya no valen "audio workgroups de macOS" (`3b0af48`), "streaming de disco" (`308a13a`), "embedding aprendido CLAP" (`4e04cfc`), "servidor MCP externo" (`85b4b9b`), "motor multinúcleo" (`43d24d1`) ni "builds firmados" (`2530c6c`, `e75e222`). Tampoco recoge locators, Bounce, warp markers, Auto-Warp, Capture, Undo History ni la UI de follow actions.
- **PENDING §2**: "Grabación de audio", "Freeze & Flatten", "UI de Comping", "Servidor MCP" y "Búsqueda semántica CLAP" están hechos. Sigue abierto §3 (hardware real, pruebas con ratón).

---

## 6. Qué no se pudo verificar

- No se ejecutó `cargo` ni la app: un test que existe no se probó. Windows: solo cuenta lo que dice PENDING (`cargo check` pasa, sin prueba visual ni de audio).
- **Ausencias por `rg`**: punch, Track Delay, Cue, Crossfader, Linked-Track Editing, Lock Envelopes, Tuning Systems, MPE, MIDI Map, Link, A/B de dispositivo, Capture and Insert Scene, Scene Follow Actions. Si existieran con otro nombre no se habrían visto.
- Del manual de Live 12 se leyeron `session-view`, `recording-new-clips` y el índice. **No se descargaron** `launching-clips` (§8.3.1, opciones de follow action) ni `stem-separation` (la consulta fue rechazada): el detalle de follow actions y de los modos High Speed y High Quality va como SUPUESTO.
- Criterio de 4.5 ("CPU ~0 y nulo < −90 dB"): `engine/src/tests/freeze.rs` no tiene una prueba de nulo; SUPUESTO que se cumple.
- 4.R2: solo se miraron el send por defecto y `Entry::Failed`; el resto de sus puntos (adopción por id, meters tras instalar el grafo, Play que no se pierde, posición al reabrir) no se re-leyeron.
- Si Live 12 crea fundidos automáticos al solapar clips: SUPUESTO (aquí solo se comprobó que AI-Track no los crea).
- Licencia de `midly` (P0.6): hay que pasar `cargo deny` antes de añadirlo.
- Live 13 es una beta: las funciones y los nombres pueden cambiar hasta la salida de "early 2027". La fecha de 12.3 difiere entre las notas oficiales (25-nov) y la prensa (27-nov).
- Los esfuerzos S, M y L son estimaciones por tamaño del cambio, no mediciones.

## 7. Fuentes

- Notas de Live 12 (12.0 a 12.4.16): https://www.ableton.com/en/release-notes/live-12/
- Notas de la beta de Live 13 (13.0b25, 13.0b26): https://www.ableton.com/en/release-notes/live-13-beta/
- Anuncio oficial de Live 13 (2026-11-10): https://www.ableton.com/en/blog/live-13-is-coming/
- Manual de Live 12: https://www.ableton.com/en/live-manual/12/ (capítulos `session-view`, `recording-new-clips` y el índice leídos hoy)
- Prensa (secundaria): https://www.musicradar.com/music-tech/ableton-live-13-is-here-everything-you-need-to-know-about-lives-latest-update · https://thefader.com/2026/11/10/ableton-announces-live-13-with-ara-2-support-and-a-new-step-sequencer
