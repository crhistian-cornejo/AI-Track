# 13 · Auditoría de paridad funcional con Ableton Live 12 / 12.x / 13

> Fecha: 2026-11-15. Estado del código: rama `main`, árbol limpio al empezar (commit `8be9bf2`).
> Alcance: qué funciones básicas de un DAW tipo Live faltan, cuáles existen a medias o mal resueltas, y cuáles están bien.
> Este informe **construye sobre** [08-ableton-live-manual.md](08-ableton-live-manual.md) (estructura del manual y control bar) y [03-daw-workflow-ux.md](03-daw-workflow-ux.md); no repite su contenido. Nota: la comparativa de §4 del informe 08 sobre el topbar está ya en parte superada (existen tap tempo, selector de escala y CPU); aquí se reevalúa contra el código actual.

## 0. Método y convenciones

- **VERIFICADO** = leído en el código (ruta:línea) o confirmado con `grep` sin resultados (ausencia). **SUPUESTO** = inferido, no ejecutado. No se ha ejecutado la app ni los tests; no se ha modificado código ni tocado git.
- Una función cuenta como **completo** solo si hay camino UI → `Command` → modelo → motor (y salida audible/visible). **Parcial** = existe una parte del camino. **Ausente** = ni modelo ni UI, o solo campo de modelo sin consumidor.
- Severidad: **crítico** (un DAW no es usable sin ello), **alto**, **medio**, **bajo**. Esfuerzo: S (días), M (1-2 semanas), L (>2 semanas).
- Rutas relativas a `crates/`. Fuentes web al final (§6).

### Resumen ejecutivo

Fortalezas reales: Command API con undo/propuestas, mezcla/routing con grupos-retornos-VCA-sidechain y PDC en el motor, exportación completa (WAV 16/24/32f, MP3, stems, dither, normalizado), automatización con chooser y lanes, edición de clips en el arrangement (fades, trim, stretch, split, consolidate), conciencia de escala global/pista/clip, suite de plugins propios y librería inteligente. Huecos decisivos: **no se puede grabar** (el botón Record es un indicador sin captura), **Session View no permite crear ni mover clips**, **no se pueden borrar/duplicar/reordenar pistas**, **la entrada MIDI de hardware no está conectada**, **el routing de E/S del mixer es de solo lectura**, **no hay autosave/recuperación ni "Collect All and Save"**, y varios campos de modelo (take lanes, warp markers, tempo de escena, probabilidad/expresión de nota) existen sin consumidor.

---

## 1. Tabla de paridad

### 1.1 Transporte y control bar

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Play/Stop/Continue/ir al inicio | completo | `app/src/keys.rs:159` (TABLE: Space, Shift-Space, Home); `engine/src/engine.rs:230-245` | — | — |
| Loop (brace en regla, Cmd+L) | completo | `app/src/views/topbar.rs` (toggle "loop"); `Command::SetLoop` `model/src/command.rs:83` | — | — |
| Loop con campos numéricos inicio/longitud | ausente | solo botón; sin campos en `topbar.rs` (grep) | medio | S |
| Punch in/out | ausente | `grep -i punch` en model/engine/app: solo nombres de presets | alto | M |
| Metrónomo on/off | completo | `topbar.rs` toggle "metronome"; `app/src/real/engine.rs:477` | — | — |
| Ajustes de metrónomo (volumen, subdivisión, sonido) | ausente | solo `set_metronome(bool)` | medio | S |
| Count-in | parcial (solo motor) | `engine/src/engine.rs:266` `set_count_in` + test `engine/src/tests.rs:848`; **ningún uso en `app/`** (grep) | alto | S |
| Tap tempo | completo | `app/src/views/topbar.rs:57` (`tap`) | — | — |
| Tempo editable por arrastre/teclado | completo | `Drag::Tempo`, `NumTarget::Tempo` (`workspace.rs`) | — | — |
| Compás (un único compás global) | parcial | `app/src/state.rs:1353` `set_time_signature` solo reescribe la primera firma; el modelo admite `signatures: Vec<(Tick,_)>` (`model/src/time.rs`) pero no hay UI para cambios a mitad de canción | medio | M |
| Posición editable (clic y escribir) | ausente | el campo de posición solo lee (`topbar.rs`, tooltip "Arrangement position") | bajo | S |
| Follow | completo | `Cmd::Follow` `keys.rs`; `app/src/commands.rs:148` | — | — |
| Selector Session/Arrangement + Tab | completo | `ToggleView` `actions.rs`; `segmented` en `topbar.rs` | — | — |
| Alt/Option+1/2 para cambiar de vista | ausente | no está en `keys.rs` TABLE | bajo | S |
| Back to Arrangement / Stop All Clips visibles | parcial | `engine.rs:294` `back_to_arrangement` sin uso en app (grep); "Stop all" sí está en `session.rs` | medio | S |
| Selector de quantización de lanzamiento global | parcial (solo motor) | `engine.rs:299` `set_global_quantize`, sin uso en app (grep) | medio | S |
| Overdub MIDI / Arm automation / Re-enable automation / Capture MIDI en la barra | ausente (Automation Arm sí existe en la cabecera de lanes) | `views/arrangement.rs:623` (Automation Arm); resto grep vacío | alto | M |
| Link / Tempo Follower / sync por reloj MIDI | ausente | grep `Link` en model/engine/app sin soporte; sin MIDI clock | medio | L |
| Medidor de CPU | completo | `topbar.rs:408` ("Audio engine load"); `engine.cpu_load()` | — | — |
| Barra de menús nativa (macOS/Windows) | ausente | menú "File" es un popover (`views/menus.rs:59`); sin `set_menus` (grep). Sin menú Edit con etiqueta de undo | medio | M |

### 1.2 Grabación

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Grabar MIDI a clip (Arrangement) | **ausente** | `toggle_record` → `real/engine.rs:467` → `engine.rs:272` `set_recording` → `rt.rs:437`: solo pone un flag que se pasa a los procesadores (`graph.rs:595`). Nada crea fuente/clip. `PENDING.md` §2 lo confirma para audio | **crítico** | L |
| Grabar MIDI/audio en Session (slot) | ausente | sin botón de grabación en `session.rs`; `session.rs:211` solo lanza | crítico | L |
| Grabar audio a disco desde entrada | ausente | el motor entrega entrada estéreo única al monitoreo (`graph.rs:945`, `audio_io.rs:581`), sin escritor a WAV | crítico | L |
| Overdub / grabación en bucle / Capture MIDI | ausente | grep `capture`, `overdub` vacío | alto | M (sobre grabación) |
| Punch, comping, take lanes | ausente | `Placement.lane` (`model/src/project.rs:344`) lo ignora el motor: `engine/src/compile.rs:495` filtra `lane == 0`; sin UI | alto | L |
| Entrada MIDI de hardware | **ausente en la app** | motor: `engine.rs:470-480` `midi_inputs/open_midi_input`; **la app no los llama** (grep `midi_inputs` en `app/src` vacío); `EngineConfig.midi_inputs` queda vacío (`engine.rs:55,71`); Preferencias no tiene sección MIDI | **crítico** | M |
| Teclado del ordenador como teclado MIDI | completo | `Cmd::ComputerKeyboard` (tecla M), `audition.rs`, `real/engine.rs:519` | — | — |
| Monitorización (In/Auto/Off) | parcial | modelo `Monitor` + motor `graph.rs:945` (solo pistas de audio armadas); **sin UI** (`TrackProp::Monitor` no se usa en `app/`) | medio | S |
| Selección de entrada por pista (Audio From / MIDI From, canal) | ausente | mixer pinta "In 1/2" fijo `views/mixer.rs:152`; motor tiene una sola entrada estéreo | alto | M |
| Armar pista (R, tecla C) | completo (pero sin efecto de grabación) | `Cmd::ArmTrack`, chip "R" `mixer.rs`, `session.rs:189` | medio | — |
| Automation Arm (grabar movimientos de plugin) | parcial | `state.rs:1447` `apply_plugin_edits`: solo gestos en la ventana del plugin; no knobs/faders de la propia app | medio | S |

### 1.3 Session View

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Matriz de clips y lanzamiento (clip/escena/stop pista/stop all) | completo para lo que existe | `views/session.rs:73,211`; `engine.rs:277-290` | — | — |
| **Crear clips en slots, mover/copiar entre slots, soltar muestras** | **ausente** | `Command::SetSessionSlot` solo se emite desde `state.rs:1810` (pistas MIDI generadas por IA) y `demo.rs`; sin `on_drop` ni "insertar clip" en `session.rs` | **crítico** | M |
| Añadir/renombrar/borrar/duplicar escenas | ausente | `AddScene/RenameScene/RemoveScene` solo en `demo.rs`; `Project::new` fija 8 escenas (`project.rs:82`) | alto | S |
| Tempo/compás por escena | ausente | `Scene.tempo` (`project.rs:333`) sin consumidor (grep en engine) | bajo | M |
| Modos de lanzamiento (Trigger/Gate/Toggle/Repeat) | parcial | motor sí; UI es un botón que cicla (`views/bottom.rs:495`), sin menú desplegable | medio | S |
| Quantize de lanzamiento por clip | parcial | ídem `bottom.rs:506` (cicla) | medio | S |
| Follow actions | parcial | modelo `project.rs:488` + motor `graph.rs:1104`; **sin UI** (grep `launch.follow` en app solo en tests); acción única, sin probabilidad/segunda acción ni "jump" | alto | M |
| Lanzar con teclado (Enter, flechas, teclas de escena) | ausente | no existe en `keys.rs` | alto | S |
| Capturar e insertar escena / Consolidar a escena | ausente | grep vacío | medio | M |
| Grabar Session → Arrangement (arrangement recording de lo lanzado) | ausente | depende de grabación | alto | L |

### 1.4 Arrangement y edición de clips

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Split, duplicar, consolidar, cortar/copiar/pegar, nudge | completo | `keys.rs` TABLE; `model/src/ops.rs:320-499`; `editing/clipboard.rs` | — | — |
| Selección de tiempo, duplicar tiempo, insertar silencio | completo | `Cmd::DuplicateTime/InsertSilence` | — | — |
| Borrar tiempo (ripple) | ausente | `delete_arrangement` (`editing/clipboard.rs:445`) borra contenido sin cerrar hueco | bajo | S |
| Fades in/out con curva + gain + envolvente de volumen de clip | completo | `model/src/project.rs:373`; `views/arrange_clips/` ; motor `Clip::gain_at` | — | — |
| Crossfade entre clips adyacentes | ausente (sí hay crossfade interno al bucle) | `engine/src/clip.rs:499` solo costura de loop; sin auto-crossfade en solapes | medio | M |
| Solapar clips: sobrescritura estilo Live | parcial/dudoso | `drag_clips` (`app/src/editing.rs:477`) emite `MovePlacement` sin recortar al vecino; `compile.rs:495` toma todas las placements lane 0 → **SUPUESTO**: suenan ambos | alto | M |
| Trim/resize/stretch por borde (Cmd/Ctrl) | completo | `views/arrange_clips/mod.rs` | — | — |
| Reverse, transpose, detune | completo | `Warp.reverse/transpose/detune` `project.rs:583-598`; `bottom.rs:600-630` | — | — |
| Warp on/off y 6 modos (Beats, Tones, Texture, Complex, Complex Pro, Re-Pitch) | completo | `WarpMode` `project.rs:628`; `dsp/src/warp.rs`; `engine/src/clip.rs:35` | — | — |
| Warp **per clip** como Live (copia de Source al editar) | completo | `state.rs:1131` `set_warp_of` | — | — |
| **Warp markers** (crear/mover, "warp from here") | ausente | `Warp.markers` (`project.rs:598`) sin lector en motor/UI (grep `.markers`: solo `ops.rs:222` al estirar) | alto | L |
| Corregir BPM detectado (Seg. BPM, ÷2/×2, Set 1.1.1) | ausente | `bottom.rs:575` es una `value_box` de solo lectura | alto | S |
| Auto-warp al importar (long samples) | parcial | solo si la librería trae BPM (`workspace.rs:~1910`); import normal no activa warp (`workspace.rs:1841`) | medio | S |
| Quantize de audio / slice a MIDI / convertir a MIDI en menú de clip | parcial | audio→MIDI existe como acción de chat (`ai_panel/slash.rs`: midi, midi-mono, midi-drums); no en menú de clip; sin "Slice to New MIDI Track" | medio | M |
| Freeze / Flatten / Bounce in Place / Bounce to New Track / Paste Bounced Audio / Resample | **ausente** | `grep -i freeze\|bounce` en app: nada funcional; solo `export.rs` offline | alto | L |
| Reutilizar render offline para consolidar | completo | `engine/src/export.rs:391` `consolidate_to_wav`; `Cmd::Consolidate` | — | — |
| Grooves / Groove Pool | ausente | grep `groove` solo en etiquetas | medio | L |
| Crear/renombrar clip, color de clip | parcial | renombrar sí (`Cmd::Rename`); selector de color de clip/pista no (grep `TrackProp::Color` solo en `state.rs:1875`, color de grupo) | medio | S |
| Marcadores/locators/cue points | **ausente** | grep `locator\|cue` vacío; `Project` sin campo (`project.rs:45-58`) | alto | M |
| Zoom, fit, zoom a selección, snap/triplets | completo | `keys.rs` | — | — |

### 1.5 Piano roll y MIDI

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Dibujar/seleccionar/mover/redimensionar/borrar, ghost notes, fold a escala, velocidad | completo | `views/piano_roll.rs`; `state.rs:348` | — | — |
| Carril de velocidad | completo | `piano_roll.rs:22` | — | — |
| Carriles de CC / pitch bend / aftertouch | ausente | el motor reproduce CC/PB de entrada (`rt.rs:79`) pero el clip solo guarda `Note` (`project.rs:516`) | alto | L |
| Probabilidad ("chance"), release velocity, pan por nota, fine pitch, slide: edición | parcial | campos y reproducción: `project.rs:526-540`, `engine/src/clip.rs:155-199`; **sin UI** (grep `.probability` en app vacío) | medio | M |
| MPE (grabar/editar) | ausente | solo campos de expresión por nota | medio | L |
| Escala global/pista/clip + "Fit to Scale" + chord stamping | completo y superior a Live en alcance | `model/src/scale.rs`; `menus.rs:333` | — | — |
| Herramientas estilo MIDI Tools de Live 12 (Transformations/Generators) | parcial | acciones de un clic sin parámetros ni previsualización: acordes, inversión, progresión, arpegio, strum, chop, randomize (`state.rs:646-773`, `views/menus.rs:333`). Faltan Rhythm/Seed/Shape/Stacks/Euclidean, Connect, Ornament, Recombine, Span, Time Warp, Velocity Shaper | medio | L |
| Cuantización con fuerza/swing/groove, "Quantize Settings" | ausente | `state.rs:478` `quantize` a rejilla fija | medio | S |
| Legato, Reverse, Join, Humanize, Fixed Length, Find/Select Notes | ausente | grep en `state.rs` | medio | M |
| Edición multi-clip (varios clips a la vez) | ausente | el piano roll trabaja con `selected_clip` único | medio | M |
| Step sequencer (drum rack) | completo | `views/steps.rs`; `state.rs` `toggle_step` | — | — |
| Dispositivos MIDI (arpegiador, acordes, escala, longitud de nota, velocidad, random, pitch) | **ausente** | `RackKind::Midi` existe, `dsp::Category::MidiEffect` existe (`dsp/src/lib.rs:80`) pero `stock::REGISTRY` (`stock/src/lib.rs:37`) no registra ninguno | alto | M |
| Importar/exportar `.mid` (SMF) | ausente | ni `midly` ni lectura/escritura en el workspace (grep) | alto | M |
| Afinaciones (Tuning Systems) | ausente | grep: solo `Expression::Tuning` por nota | bajo | M |
| Clip View: Start/Length/Signature editables | ausente | `value_box` estático (`bottom.rs:300,470-480`) | medio | S |

### 1.6 Pistas, mezcla y routing

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Tipos: Instrument, Audio, Group, Folder, Return, VCA, Main | completo | `project.rs:210`; `compile.rs:1-40` | — | — |
| Agrupar/desagrupar pistas, plegar grupos | completo | `model/src/groups.rs:71,119`; `editing/menus.rs:112` | — | — |
| **Borrar pista** | **ausente** | `Command::RemoveTrack` solo lo emite `groups.rs:133` (al desagrupar); sin menú ni tecla (grep `RemoveTrack` en app: nada) | **crítico** | S |
| **Duplicar pista / reordenar pistas (arrastrar) / color de pista** | **ausente** | `Command::MoveTrack` sin uso en app (grep) | crítico | S-M |
| Retornos y envíos (nivel) | completo | `mixer.rs:171`; `state.rs:1009` crea envío a cada retorno | — | — |
| Envío pre/post fader | parcial | `Send.pre_fader` (`project.rs:250`) sin UI (grep) | bajo | S |
| Sidechain por dispositivo | completo | `Command::SetSidechain` `command.rs:119`; UI `device_ui/mod.rs:585` | — | — |
| **Selector de salida de pista (Main/Group/Hardware/None)** | **ausente (UI)** | motor lo soporta (`compile.rs:113`); el slot de salida del mixer es una etiqueta sin `on_click` (`mixer.rs:205,252`); `TrackProp::Output` no se emite desde la UI (grep) | alto | M |
| Salidas multicanal reales | parcial | `Output::Hardware(_) => Dest::Out`: todas las salidas hardware van al mismo bus estéreo (`compile.rs:113`) | medio | L |
| Solo (con rutas, VCA, hijos) / Mute / Cue-Preview output | parcial | lógica de solo sólida `compile.rs:355-368`; **sin salida de cue/preview** (grep) | medio | M |
| VCA | completo | `TrackProp::Vca`; mixer | — | — |
| PDC (compensación de latencia de plugins) | completo | `compile.rs:19,421`; `Track.reported_latency`; latencia en Preferencias | — | — |
| Retardo de pista (Track Delay en ms) | ausente | sin campo en `Track` | bajo | S |
| Racks (Instrument/Audio/Drum/MIDI) con 16 macros y variaciones | completo (salvo contenido MIDI) | `model/src/racks.rs`; `Command::SetVariations` | — | — |
| Guardar presets de usuario / guardar rack / plantillas de pista | **ausente** | solo `load_preset` de fábrica (`real/plugins.rs:597`); sin "Save Preset"; sin `Save Track` | alto | M |
| A/B de dispositivo (Live 12.3) | ausente | grep | bajo | M |
| Alojamiento VST3 y CLAP en sandbox, escáner, editor | completo | crate `host`; `bridge.rs:163` sandbox; memoria del proyecto indica pruebas con plugins reales | — | — |
| Audio Units en macOS | ausente | `PLAN.md` lo deja para fase 2 | medio | L |

### 1.7 Automatización y envolventes

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Lanes de automatización de pista (volumen, pan, mute, envíos, parámetros, On, macros) + chooser con búsqueda | completo | `model/src/automation.rs:181`; `views/arrangement.rs:1225` | — | — |
| Tempo automation (en Main) | completo | `automation.rs:190`; motor `engine/src/automation.rs:67` `fold_tempo_lane` | — | — |
| Edición: puntos, curvas, rango, desplazar rango, borrar | completo | `Drag::AutoPoint/AutoCurve/AutoRange/AutoShift` | — | — |
| Automatización con plugins (parámetros externos) | completo | `engine/src/tests/plugin_automation.rs`; catálogo de parámetros | — | — |
| Grabación de automatización (Automation Arm) | parcial (ver §1.2) | `state.rs:1447` | medio | S |
| Re-Enable Automation / parámetros anulados | ausente | grep | medio | M |
| Clip envelopes más allá de volumen (pan, parámetros, MIDI CC) | parcial | `Clip.envelopes` admite cualquier `AutomationTarget`, pero la UI solo crea volumen (doble clic en forma de onda) y el panel solo lista/borra (`bottom.rs:~715`) | alto | M |
| Teclado para automatización (Live 12.2) | ausente | grep | bajo | S |
| Envelope Clips por retorno (Live 13) | ausente | — | bajo | M |

### 1.8 Librería, navegador, IA

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Búsqueda por texto/BPM/tonalidad/tags, favoritos, rating, tags de usuario, carpetas virtuales | completo | `library/src/db.rs:137-139`; `query.rs`; `app/src/views/palette/search.rs` | — | — |
| Compatibilidad armónica (Camelot) y recomendaciones | completo (supera a Live) | `library/src/query.rs:42,372` | — | — |
| Browser persistente en pantalla (panel lateral, Ctrl+Alt+B) | ausente por decisión de diseño | `views/browser.rs` solo conserva helpers; todo va por Cmd+K (`palette/mod.rs`) | medio | M |
| Previsualización sincronizada al tempo/tonalidad del proyecto | parcial | `real/preview.rs`: voz propia a ratio fijo (solo transposición por tonalidad via `preview_pitched`), sin sync de tempo ni quantize de arranque | medio | M |
| Arrastrar muestra a arrangement | completo | `views/arrangement.rs:512` | — | — |
| Arrastrar archivos desde Finder/Explorador al arrangement/Session | **ausente** | `on_drop` de arrangement solo acepta `DraggedSample`; `ExternalPaths` solo en sintetizador y panel IA (grep) | alto | S |
| Importar audio (selector de archivos) | parcial | `workspace.rs:1841`: crea siempre pista nueva; longitud solo se lee de cabeceras WAV, el resto 0 hasta el análisis | medio | S |
| Colecciones de color, "Quick Tags" editables, búsqueda por similitud | parcial | `user_tags` en BD; similitud tímbrica pendiente (`PENDING.md` §2); sin colecciones | medio | L |
| Integración Splice | completo | crate `splice`, `palette/splice.rs` | — | — |
| Separación de stems | completo (vía chat) | `workspace.rs:1747,2036` → propuesta → `Add N stems as audio tracks`; modelos ONNX bajo demanda con aceptación de licencia | — | — |
| Separación de stems desde menú de clip/ruta contextual | ausente | solo `/stems` en el panel IA | bajo | S |
| Audio→MIDI melodía/armonía/percusión | parcial | acciones de chat; en Live es menú contextual de clip | medio | S |
| Servidor MCP real | ausente | `PENDING.md` §2 | medio | M |

### 1.9 Proyecto, archivos, preferencias, atajos

| Función | Estado | Evidencia | Sev. | Esf. |
|---|---|---|---|---|
| Guardar/abrir/recientes/plantillas, aviso de cambios sin guardar | completo | `app/src/project_io.rs:28,255` (escritura atómica con `.tmp`+rename) | — | — |
| **Autosave y recuperación tras fallo** | **ausente** | grep: `History::revision` menciona "autosave" (`history.rs:59`) pero nada lo consume | **alto** | M |
| Rutas de audio relativas / "Collect All and Save" / reparar archivos perdidos | **ausente** | `AudioSource.path: PathBuf` absoluto (`project.rs:555`); `hash: Option` sin relink (grep) | alto | M |
| Formato de proyecto | parcial | JSON único versionado (`project.rs:145`); sin contenedor ni medios embebidos (PLAN lo preveía) | medio | M |
| Historial de undo (500) | parcial | `history.rs:68`; sin lista visible ni etiquetas en menú (`Command::label` existe) | medio | S |
| Exportar mezcla: WAV 16/24/32f, TPDF y shaped dither, MP3 320, rango (canción/loop/selección), SR, tail, normalizar, stems | completo | `engine/src/export.rs:53,552`; `bridge.rs:760`; `views/export.rs` | — | — |
| Exportar FLAC/AIFF/OGG, normalizar a otro objetivo, mono, "Render as Loop" | ausente | solo WAV/MP3 | bajo | S |
| Exportar MIDI | ausente | ver §1.5 | alto | S |
| Preferencias de audio (driver, dispositivos, canales, SR, buffer, latencia medida, tono de prueba) | completo | `views/prefs.rs`; `bridge.rs:95` | — | — |
| Preferencias MIDI, Record/Warp/Launch (defaults), Archivos/Carpetas, autosave | ausente | `prefs.rs` solo tiene Audio, Appearance, Performance, Plugins, Library, Sounds, Packs, Shortcuts, Splice | alto | M |
| Mapeo de teclado y de MIDI (Key/MIDI Map), superficies de control | **ausente** | grep `midi_map\|key_map` vacío; tabla de atajos fija (`keys.rs`); sin rebinding (grep `rebind` vacío) | alto | L |
| Atajos tipo Live | parcial | coinciden: Space, Shift-Space, Tab, B, 0, Cmd+E/J/D/R/U/L/T/G, Cmd+1..4, Cmd+Shift+M/D, F1-F8 activadores. Faltan: F9 grabar, Alt+1/2, Enter lanzar, Cmd+Alt+B browser, Cmd+Shift+Delete borrar tiempo. Divergen: Cmd+I = asistente (en Live es Insertar Silencio; aquí `Cmd+Shift+I`). SUPUESTO sobre defaults exactos de Live | medio | S |
| Medidor de latencia/CPU en barra de estado | completo | `topbar.rs:408`; `status_label` | — | — |
| Pruebas de rendimiento RT sin asignaciones | completo | `engine/src/alloc_counter.rs`, 100 tests de motor; `docs/research/benchmark-24-tracks.md` | — | — |

Recuento de tests por crate (VERIFICADO con grep `#[test]`): model 69, engine 100, dsp 62, stock 285, host 26, library 66, ai 73, app 344.

---

## 2. Mal implementado o por refinar (casos concretos)

1. **El botón Record miente.** `topbar.rs:350` enciende el botón y `rt.rs:437` guarda el flag; no hay captura de eventos MIDI ni de audio, ni se crea clip. El usuario arma una pista (chip R, tecla C), pulsa grabar, toca, y no queda nada. Hasta que exista la grabación, el botón debería estar deshabilitado o mostrar un aviso. (VERIFICADO)
2. **Slots de Input/Output del mixer parecen interactivos y no lo son.** `mixer.rs:152` pinta "In 1/2" fijo para toda pista de audio y `mixer.rs:205-252` pinta la salida con el mismo estilo de slot que un selector, sin `on_click`. El motor sí entiende `Output::{Main,Track,Hardware,None}` (`compile.rs:113`). (VERIFICADO)
3. **Session View solo sirve para reproducir lo que ya está ahí.** No hay forma de colocar un clip en un slot, ni arrastrar muestras, ni añadir escenas; las 8 escenas son fijas (`project.rs:82`). El estado vacío del slot solo tiene un icono de stop al pasar el ratón (`session.rs:~218`). (VERIFICADO)
4. **Modo de lanzamiento y quantize son botones que ciclan** (`bottom.rs:495-506`) sin mostrar las opciones; con 7 valores de quantize cada cambio exige varios clics y no hay deshacer visible del ciclo. Además "Quantize" en el panel de clip significa dos cosas distintas (lanzamiento en audio y MIDI quantize en notas). Debe ser un desplegable.
5. **Cajas con aspecto de campo que no se pueden editar**: `Signature`, `Start`, `Length`, `Seg. BPM` (`bottom.rs:462-480,575`). La única vía de corregir un BPM mal detectado es estirar el clip a mano; tampoco hay ÷2/×2.
6. **Warp markers fantasma**: el modelo los serializa (`project.rs:598`) pero ni el motor ni la UI los usan; la edición de warp se reduce a modo + BPM de segmento.
7. **Take lanes fantasma**: `Placement.lane` se serializa y el motor descarta toda lane ≠ 0 (`compile.rs:495`). Un proyecto que alguna vez tuviera lanes ≠ 0 (agentes/scripts) quedaría silenciado sin aviso.
8. **Campos de nota sin UI**: `probability`, `release`, `pan`, `fine_pitch`, `slide` se reproducen (`engine/src/clip.rs:155-199`) pero no se pueden editar; el usuario no puede descubrir que existen.
9. **Cambio de compás solo global**: el menú de compás reescribe la primera firma (`state.rs:1353`). Tampoco hay edición del mapa de tempo con rampas salvo vía automatización en Main.
10. **Solapes en una pista**: mover/arrastrar emite `MovePlacement` sin recortar (`editing.rs:486`); en Live el clip movido sobrescribe. (SUPUESTO sobre el sonido resultante.)
11. **Importación de audio**: crea siempre pista nueva en la posición del playhead redondeada a compás (`workspace.rs:1841-1853`); formatos distintos de WAV entran con longitud 0 hasta que acaba el análisis; no hay arrastrar-soltar desde el sistema; no activa warp ni detecta si es loop salvo que llegue de la librería.
12. **Palette de comandos incompleta**: el modo "Commands" de Cmd+K lista solo filas de `TABLE` (`palette/mod.rs:540`), es decir solo acciones con atajo registrado; "Delete track", "Add scene", "Freeze" no podrían estar ahí aunque existieran sin añadir antes una fila.
13. **Automation Arm solo para ventanas de plugin** (`state.rs:1447`); mover faders/knobs de la propia app durante la grabación no escribe automatización, y no existe "Re-Enable Automation" cuando se pisa un lane.
14. **Previsualización del navegador**: voz independiente con su propio stream cpal (`real/preview.rs:1-12`), sin sincronía con el transporte ni con el tempo del proyecto; en un DAW tipo Live el "preview sync" es el flujo principal de elección de loops. (SUPUESTO sobre la experiencia; la arquitectura es verificada.)
15. **Pérdida de datos**: guardar es manual (`project_io.rs:28`), sin autosave, sin archivo de recuperación, con rutas de audio absolutas. Mover la carpeta del proyecto rompe todos los clips sin diálogo de reparación.
16. **Salida hardware**: `Output::Hardware(i)` se muestra como "Out 3/4" (`mixer.rs:208`) pero el motor mezcla todas en un único bus (`compile.rs:113`); engañoso para interfaces multicanal.
17. **Sin menú Edit ni etiqueta de undo/redo**: el historial guarda etiquetas (`Command::label`) pero la UI nunca las enseña.

Lo que **ya está bien resuelto** y no hay que tocar: copia de `Source` al editar warp para que cada clip conserve su ajuste (`state.rs:1131`), composición de la lógica de solo con VCA/grupos (`compile.rs:355`), crossfade de costura de loop y envolvente de clip evaluada por muestra, undo coalescido por gesto (`edit_coalesced`), tabla de atajos única que alimenta menús y hoja de ayuda (`keys.rs`), exportación con PDC y dither, chooser de automatización con búsqueda y profundidad de racks, y la integración de stems como propuestas aceptables.

---

## 3. Novedades de Live 12.x / 13 y su contraste

(Fuentes en §6. Información de prensa y notas de versión; lo que sólo aparece en prensa se marca [P].)

| Versión | Novedad | Estado en AI-Track |
|---|---|---|
| 12.0/12.1 | MIDI Tools (Transformations/Generators), Chop, MPE tools (Glissando, LFO), Stacks, mapeo Key/MIDI de herramientas | parcial (§1.5): acciones sueltas sin parámetros |
| 12.1 | Undo History, Apply Grooves instantly, Scale Awareness para audio, Auto Shift, Drum Sampler, Limiter/Saturator rediseñados, auto-tagging de muestras/VST3, Tuning Systems | Undo History ausente; Scale Awareness de audio sí (clip.scale + `AudioSource.key`); Drum Sampler sí (`instrument::drum`); Limiter sí con True Peak; tagging automático sí (`library/src/reanalyse.rs`); tuning ausente |
| 12.2 | Bounce Track in Place y Bounce to New Track, edición de automatización por teclado, filtros nuevos en Auto Filter, Quick Tags | bounce ausente; automatización por teclado ausente; `Filter` propio sin tipos Comb/Vowel/DJ (SUPUESTO por la lista `fx/filter.rs`) |
| 12.3 | **Stem Separation local** (Vocals/Drums/Bass/Others, High Speed/High Quality), **Splice** en el navegador, Auto Pan-Tremolo (modos Pan/Tremolo), **Bounce Group Tracks**, **Paste Bounced Audio**, A/B de dispositivos | stems y Splice: sí (paridad; AI-Track añade extractor de batería); Auto Pan-Tremolo: lo cubre parcialmente `Shaper` (volumen/trémolo) pero sin panning LFO dedicado; bounce/paste bounced: ausente; A/B: ausente |
| 12.4 [web] | Link Audio (según la página de notas) | Link ausente |
| 13 (beta, salida prevista inicios de 2027) [P] | Step Sequencer nuevo con longitud/probabilidad/pitch bend por paso, Capture Automation, Contextual Browsing, Drum Synth, Racks con hasta 32 macros como moduladores, Envelope Clips para retornos, Racks multibanda y Mid-Side, Clipper y Transient Shaper en Suite, Device Slots en el mixer, ARA 2 | Clipper y Transient Shaper ya existen (`fx::clipper`, `fx::transient`); Drum Synth parcial (`instrument::drum`, `kick808`); Step Sequencer existe para Drum Rack pero sin chance/longitud por fila; ARA 2: ausente; Capture Automation: ausente; macros 16 vs 32 |

Lectura: en ofertas de IA (stems, Splice, extractor de batería, audio→MIDI, asistente con propuestas) AI-Track está a la par o por delante; la brecha está en el flujo de **grabar, lanzar y rematar** (bounce/freeze/comping).

---

## 4. Plan: los 25 trabajos siguientes, priorizados

Orden por impacto en "poder terminar un tema" y por dependencias. (C) = crítico, (A) = alto, (M) = medio.

| # | Trabajo | Sev. | Esf. | Depende de |
|---|---|---|---|---|
| 1 | **Grabación MIDI al arrangement** (nueva `Command` de creación de clip desde eventos; el motor captura eventos con timestamp en una cola lock-free; al parar crea fuente `NoteSource` y clip; incluye overdub y count-in) | C | L | 2 |
| 2 | **Conectar MIDI de hardware**: sección MIDI en Preferencias (lista de puertos, track/remote/sync), llamar a `open_midi_input`, enrutar a la pista armada | C | M | — |
| 3 | **Session View editable**: soltar muestras/clips en slots, copiar/mover entre slots, "crear clip MIDI" en slot vacío, añadir/renombrar/borrar/duplicar escenas, teclas de lanzamiento (Enter, flechas) | C | M | — |
| 4 | **Gestión de pistas**: borrar, duplicar, reordenar por arrastre, color de pista y de clip (usar `RemoveTrack/MoveTrack/AddTrack/SetTrack`) | C | S-M | — |
| 5 | **Grabación de audio** a WAV (entrada por pista, escritor en hilo no RT, creación de clip al parar; monitor In/Auto/Off) y selector de entrada por pista | C | L | 2 (patrón de cola) |
| 6 | **Routing de E/S en el mixer**: selector de salida (Main/Grupo/Hardware/None), selector de entrada y monitor; mapear `Output::Hardware(i)` a pares reales del dispositivo | A | M | — |
| 7 | **Autosave + recuperación** (archivo `.recover` consumiendo `History::revision`) y diálogo de recuperación al arrancar | A | M | — |
| 8 | **Collect All and Save + rutas relativas + reparar archivos perdidos** (usar `AudioSource.hash`) | A | M | — |
| 9 | **Autorun del warp**: Seg. BPM editable con ÷2/×2, auto-warp al importar, y luego **warp markers** (motor: mapear marcadores → rampas de tiempo) | A | L | — |
| 10 | **Locators/marcadores** en la regla (`Project.locators`, comandos, navegación, saltos desde teclado) y **campos de loop + punch in/out** en la barra | A | M | — |
| 11 | **Freeze / Flatten / Bounce in Place / Bounce to New Track / Paste Bounced Audio** (reutilizar `render_clips_with` y `consolidate_to_wav`) | A | L | — |
| 12 | **Arrastrar archivos desde el sistema** (`ExternalPaths`) al arrangement y Session + importación sin crear pista nueva siempre; metadatos para formatos no WAV | A | S | 3 |
| 13 | **Dispositivos MIDI stock**: Arpeggiator, Chord, Scale, Note Length, Velocity, Pitch, Random (categoría `MidiEffect` + cadena en `RackKind::Midi`) | A | M | — |
| 14 | **Importar/exportar `.mid`** (SMF) con comandos de creación de clip | A | M | — |
| 15 | **Presets de usuario y plantillas**: Save Preset de dispositivo/rack, Save Track, plantilla de proyecto por defecto | A | M | — |
| 16 | **Panel de clip completo**: Start/Length/Signature editables, desplegables de launch mode/quantize, UI de **follow actions** (con segunda acción y probabilidad) | A | M | — |
| 17 | **Carriles de piano roll**: probabilidad, release, CC y pitch bend (nuevo contenedor de eventos en `NoteSource`, motor lo reproduce) | A | L | — |
| 18 | **Clip envelopes completos**: crear envolventes de pan/parámetros/CC, y Automation Arm para knobs/faders internos + Re-Enable Automation | A | M | — |
| 19 | **Mapeo MIDI/teclado y rebinding de atajos** (`keys.rs` ya centraliza); superficie de control genérica | A | L | 2 |
| 20 | **Solape de clips estilo Live** (sobrescribir al mover/pegar) y auto-crossfade entre adyacentes | M | M | — |
| 21 | **Cambios de compás y mapa de tempo** editables (múltiples firmas, rampas), tempo por escena (`Scene.tempo` en motor) | M | M | — |
| 22 | **Barra de menús nativa** (Edit con undo/redo etiquetado, View, Help) y panel de **Undo History** | M | M | — |
| 23 | **Comping/take lanes**: grabación en bucle a lanes (usa `Placement.lane`), vista desplegable y comp por rangos | M | L | 1, 5 |
| 24 | **Navegador mejorado**: previsualización sincronizada (tempo + quantize), panel persistente opcional, colecciones y similitud (CLAP embeddings) | M | L | — |
| 25 | **Link / reloj MIDI** y metrónomo configurable; utilidades menores: Alt+1/2, F9, Back to Arrangement, quantize global visible, MIDI Tools con parámetros | M | L | — |

Sugerencia de oleadas: **Ola 1** (usabilidad básica): 4, 2, 3, 16, 12, 7. **Ola 2** (grabar y componer): 1, 5, 6, 10, 14, 13. **Ola 3** (pulido y paridad avanzada): 9, 11, 15, 17, 18, 19, 20-25.

---

## 5. Qué no se pudo verificar

- No se ejecutó la aplicación ni la suite de tests (solo lectura estática). Comportamiento audible de los solapes de clips, de la sincronía de la previsualización y del "Re-enable" son **SUPUESTOS**.
- Los atajos exactos por defecto de Live (F9, Alt+1/2, Enter, Cmd+Alt+B, Cmd+I = Insert Silence) se citan de memoria y del informe 08; no se contrastaron con la tabla oficial de atajos.
- Windows: `PENDING.md` indica que `cargo check` multiplataforma pasa pero no hay prueba visual/audio; la auditoría solo cubre el comportamiento del código común.
- La lista de Live 13 procede de prensa, no de las notas oficiales (la página de notas de la beta no se obtuvo); puede haber diferencias hasta el lanzamiento (prensa: primera mitad de 2027).
- Qué hace exactamente `Processor::process` con `transport.recording` en los plugins propios no se revisó crate por crate (solo se confirmó que ningún camino de la app crea clips).
- El comportamiento real de `Tempo` en la lista de lanes de Main solo se verificó en el modelo (`model/src/automation.rs:190`), no en pantalla.

---

## 6. Fuentes

- Manual de Live 12 (estructura y capítulos): https://www.ableton.com/en/live-manual/12/ (ver informe 08 §6).
- Notas de versión de Live 12 (12.1 Undo History, MIDI Tools, Tuning; 12.2 Bounce in Place, Auto Filter, teclado de automatización; 12.3 Stem Separation, Splice, Auto Pan-Tremolo, Bounce Group, Paste Bounced Audio, A/B): https://www.ableton.com/en/release-notes/live-12/
- Live 12.3 está llegando (anuncio oficial): https://www.ableton.com/en/blog/live-12-3-is-coming/
- Cobertura de 12.3 (stem separation solo en Suite, local): https://mixdownmag.com.au/news/ableton-live-12-3-drops-overnight-with-stem-separation-and-splice-integration/ y https://www.musicradar.com/music-tech/stem-separation-splice-integration-ableton-is-playing-catch-up-with-live-12-3-but-its-best-new-features-are-the-ones-hidden-beyond-the-headlines
- Anuncio de Live 13 (beta pública, ARA 2, Step Sequencer, Capture Automation, Contextual Browsing, Drum Synth, Racks 32 macros, Envelope Clips, previsto inicios de 2027): https://thefader.com/2026/11/10/ableton-announces-live-13-with-ara-2-support-and-a-new-step-sequencer y https://djmag.com/tech/ableton-announces-live-13-featuring-new-step-sequencer-updated-racks-and-ara-2-support
- Resumen en prensa de la beta de Live 13: https://sonicstate.com/news/2026/11/10/ableton-live-13-beta-many-new-features-soniclab/
