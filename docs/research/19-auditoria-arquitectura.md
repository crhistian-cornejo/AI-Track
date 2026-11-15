# 19 · Auditoría de arquitectura y calidad de implementación

Fecha: 2026-11-15 · Rama `main` limpia (commit `8be9bf2`) · Alcance: ingeniería (tiempo real, Command API, motor, robustez, UI, salud del workspace). No incluye la paridad con Ableton, la calidad de importación de audio, el host de plugins a fondo, Splice ni el sonido de los dispositivos, que auditan otros agentes.

Cada hallazgo lleva **VERIFICADO** (leí el código y el camino que lo provoca) o **SOSPECHA** (el código lo permite, pero no lo reproduje). Esfuerzo: **S** (≤ medio día), **M** (1–3 días), **L** (más de una semana).

---

## 0. Estado real del workspace

Lo ejecuté el 2026-11-15 (la compilación fue incremental y estaba en caché):

| Comando | Resultado |
|---|---|
| `cargo check --workspace --all-targets` | **OK**, 0 warnings propios (solo el aviso future-incompat de la dependencia `block v0.1.6`) |
| `cargo clippy --workspace --all-targets` | **OK**, **0 warnings** de clippy |
| `cargo test --workspace --no-fail-fast` | **1067 pasan, 0 fallan, 11 ignorados** |

Tests por crate: app 344 · stock 281 (+ alloc 4, presets 2, robustness 3) · engine 99 · ai 70 · model 69 · library 66 · dsp 62 · host 26 (+ hosting 9, sandbox 5, scan 4) · splice 18 · xtask 5.
Los 11 ignorados son a propósito: hardware de audio real (`real_device_*`), red o modelos ONNX (basic-pitch, demucs), plugins instalados y benchmarks de CPU. `tests/real_plugins.rs` **no se ejecutó**, porque `AITRACK_TEST_PLUGINS` no está definido; la integración con plugins reales de terceros no la cubre la suite por defecto.

Archivos gigantes: `app/src/workspace.rs` (3055 líneas), `app/src/state.rs` (2537), `app/src/views/palette/mod.rs` (2159), `stock/src/instrument/wavetable/mod.rs` (1752), `app/src/bridge.rs` (1675), `app/src/views/arrangement.rs` (1514).
`crates/splice` existe en el workspace (`Cargo.toml:11`) pero no está en el mapa de `CLAUDE.md`.
Código muerto: 14 `allow(dead_code|unused)` (p. ej. `app/src/grid.rs:6` y `app/src/gesture.rs:34`, que lo silencian para el módulo entero). Hay 2 TODO/FIXME.

**Zonas críticas sin tests:** el comportamiento ante un panic en el hilo de audio, el crecimiento de memoria de `History::apply_coalesced`, la lectura de `format_version` y las migraciones, `Control::open_audio` y `reprepare_all` (solo se testea la máquina de estados `DeviceWatch`), la colisión de la caché de warp, y la exportación con estado de plugin sin guardar.

**Lo que está bien hecho**, para no repetirlo: colas `rtrb` SPSC y un hilo "graveyard" que libera los grafos fuera del hilo RT (`engine.rs:130`), `std::mem::forget` en lugar de un drop en el hilo de audio (`rt.rs:363`), `EventList` y `ActiveNotes` de capacidad fija, ordenación `sort_unstable` (sin asignar), test con contador de asignaciones para todos los plugins de stock (`stock/tests/alloc.rs`), test de salidas finitas (`stock/tests/robustness.rs`), FTZ/DAZ en cada bloque, seqlock para el ancla del playhead, plugins alojados bajo `catch_unwind` y con `sanitize` (`host/src/plugin.rs:348`), `Command::Batch` atómico con rollback (`model/src/command.rs:533`) y guardado mediante archivo temporal + `rename`.

---

## 1. CRÍTICO

### C1 · Un panic en el camino de audio cierra la app, y no hay autosave ni recuperación — VERIFICADO · M
- `engine/src/graph.rs:168` llama a `proc.process(&mut ctx)` de los procesadores de stock sin `catch_unwind`. Lo mismo vale para todo `process_segment`/`play_clips`/`render_audio` (indexado de slices en `graph.rs:882`, `graph.rs:988`, `graph.rs:1082`, `clip.rs`). El único `catch_unwind` del camino RT está en los plugins alojados (`host/src/plugin.rs:359`).
- El callback de cpal (`engine/src/audio_io.rs:481`) corre dentro de un callback C de CoreAudio/WASAPI. Un panic que intenta desenrollar a través de una frontera `extern "C"` hace **abortar** el proceso (Rust ≥ 1.81). `panic = "unwind"` en `Cargo.toml:77` no lo evita.
- `grep -i "autosave|recover"` sobre `crates/app/src` no encuentra nada. `History` dice "the engine and autosave watch it" (`model/src/history.rs:59`), pero el autosave no existe. El único guardado es manual (`app/src/project_io.rs:85`).
- **Consecuencia:** cualquier índice fuera de rango en un DSP de stock, en un clip o en un preset raro mata la app y se pierde todo lo no guardado.
- **Arreglo:** (1) envolver `proc.process` de cada `ProcNode` en `catch_unwind(AssertUnwindSafe)`, marcar el nodo como `crashed` y limpiar su buffer, igual que hace `HostedPlugin`. Envolver además el cuerpo del callback de cpal entero (`process_interleaved_at`) y, si falla, rellenar con silencio y poner un flag. (2) Autosave periódico (por ejemplo cada 60 s si `revision` cambió) a `~/.../Autosave/<nombre>.aitrack` en un hilo de fondo, más un diálogo de recuperación al arrancar si queda un autosave más nuevo que el archivo.

---

## 2. ALTO

### A1 · Cada edición, incluido cada frame de un arrastre de knob o fader, clona el proyecto entero y recompila el grafo completo — VERIFICADO · L
- `app/src/state.rs:1176`: `set_param` → `edit_coalesced` → la revisión sube. En cada render, `app/src/workspace.rs:825-830` (`sync_engine`) llama a `engine.update_project(&self.s.project)`, que hace `Arc::new(project.clone())` (`engine/src/engine.rs:213`): un **clon profundo** en el hilo de UI, con todas las notas, la automatización y los blobs de estado de plugin (`Device::state: Option<Vec<u8>>`, `model/src/project.rs:680`).
- En el hilo de control, `compile()` (`engine/src/compile.rs:245`) reconstruye **todos** los `TrackNode`. Cada uno lleva unos 8 `AudioBuffer` de 2×1024, varias `EventList` de 2048 eventos y `DelayLine`s nuevas. Resuelve cada dispositivo en el pool y hashea **todos los bytes** de cada `Device::state` en cada compilación (`engine/src/shared.rs:111-117`, llamado desde `resolve`, `shared.rs:255`).
- No existe un camino rápido de parámetros: no hay ninguna API `set_param` en `EngineHandle`.
- **Consecuencia:** el coste de mover un knob crece con el tamaño del proyecto (asignaciones de MB por frame a 60 Hz, presión de memoria, latencia de respuesta del knob de 10 ms o más), y el grafo nuevo hace un swap completo por cada frame de arrastre.
- **Arreglo:** (1) añadir una cola RT de "param set" (`RtCommand::SetParam(DeviceId, u32, f32)` y equivalentes para volumen, pan y sends) que el hilo de audio aplique sobre el grafo instalado. La UI la envía durante el arrastre y el `Command` se sigue registrando para undo y guardado. (2) Que el compilador detecte "solo cambiaron parámetros" (diff por revisión o por tipo de comando) y no recompile. (3) A medio plazo, `Arc` estructural en `Project` (`Arc<Vec<Note>>` y `Arc<[u8]>` para el estado) para que el clon sea barato.

### A2 · El time-stretch offline se calcula de forma síncrona dentro de `compile()`, con una caché débil y una clave que puede colisionar — VERIFICADO (camino) / SOSPECHA (colisión real) · M
- `engine/src/compile.rs:484` llama a `AudioClipData::of_bpm`, y este a `cached_warp` (`engine/src/clip.rs:278-323`), que ejecuta `warp_audio` sobre **el archivo entero** en el hilo de control mientras compila.
- La caché es global, de 32 entradas, LRU en un `Vec` (`clip.rs:317`). Cuando hay más de 32 combinaciones (clip × tono × tempo), cada compilación vuelve a hacer el warp de todo, y A1 dispara una compilación por cada edición. Un arrastre del BPM cambia `time_scale_milli` en cada paso y recalcula **todos** los clips warpeados en cada frame. Mientras tanto, el hilo de control no procesa más ediciones ni vigila el dispositivo.
- La clave `audio_fingerprint` (`clip.rs:250-265`) mira solo 9 muestras por canal más `frames`. Dos one-shots o loops de la misma longitud con silencio en esos 9 puntos, algo habitual en packs exportados a longitud fija, **comparten la entrada y se reproduce el audio del otro**.
- La caché retiene hasta 32 copias completas de audio decodificado fuera del presupuesto de `SampleCache` (512 MiB).
- Además, el warp se precalcula a `project.tempo.initial_bpm()` (`compile.rs:484`), así que los cambios de tempo del mapa no se respetan en los modos que preservan el tono (SOSPECHA; se solapa con la auditoría de audio).
- **Arreglo:** hacer el warp en un worker como `SampleCache` (clip en silencio o sin warp hasta que esté listo, y luego `Msg::Recompile`), usar como clave la ruta de la fuente + mtime + parámetros (o un hash completo calculado una vez al decodificar), y contar sus bytes en el presupuesto de la caché.

### A3 · Un error de compilación congela el motor en el grafo anterior sin avisar — VERIFICADO · S
- `compile.rs:101-104` devuelve `Err(InvalidRoute)` si `Output::Track(id)` apunta a una pista que no existe, y `compile.rs:340-343` devuelve `Err(Cycle)`. En `engine.rs:783-786` el error solo se guarda en `last_error` y el audio sigue con el grafo viejo. **Ninguna edición posterior llega a sonar** mientras el error siga ahí.
- `Command::RemoveTrack` (`model/src/command.rs:234`) no limpia los `Output::Track`, los sends, las cadenas de sidechain ni las referencias VCA de otras pistas. `ungroup` solo arregla a los hijos (`model/src/groups.rs:120-133`). Los agentes de IA y la Command API pueden dejar rutas colgando o ciclos, porque no hay validación de ciclos en el modelo.
- La UI solo enseña `last_error` cuando cambia la generación del dispositivo y no hay dispositivo activo (`app/src/real/engine.rs:401-406`).
- **Arreglo:** (1) que la compilación degrade en lugar de fallar: una ruta inválida va a Main con un warning, y un ciclo silencia la arista que lo cierra con un warning. (2) Validar en `Command::apply` (`SetTrack Output/Sends` y `RemoveTrack`) que no se crean ciclos, y que `RemoveTrack` emita los comandos de limpieza. (3) Enseñar los warnings y errores de compilación en la UI.

### A4 · El MIDI por hardware no está conectado en la app, y el MIDI en vivo pierde la precisión de muestra — VERIFICADO · S/M
- `app/src/real/engine.rs:246` crea `EngineConfig { midi_inputs: vec![] .. }` y en `crates/app` no hay ninguna llamada a `open_midi_input`. **Un teclado MIDI externo no llega al motor**; solo funcionan el teclado en pantalla y la audición (`send_midi_to`).
- `engine/src/midi_io.rs:44` descarta el timestamp de midir (`|_stamp, ..|`). `engine/src/graph.rs:785` mete todos los eventos en vivo con `offset 0`, y `graph.rs:924-929` solo los entrega en el primer segmento del bloque. El jitter llega a un buffer entero (21 ms a 1024/48k), y con bucles el evento puede caer en el segmento equivocado.
- **Arreglo:** abrir los puertos según las preferencias (con un listado en Preferencias → MIDI). Guardar el timestamp de midir en `MidiMessage`, convertirlo a offset dentro del bloque con el reloj `SharedStatus::epoch` y el `cb_nanos` del callback (ya existen), y repartir los eventos por segmento.

---

## 3. MEDIO

### M1 · Una sola automatización que no es de un plugin alojado parte toda la cadena de la pista en bloques de 32 muestras — VERIFICADO · M
`engine/src/graph.rs:975-1004`: si algún `ParamAuto` no es `Timed` (parámetro de stock, macro o encendido de dispositivo), **todos** los dispositivos de la pista, incluidos los VST3/CLAP, se procesan en trozos de `CONTROL_BLOCK = 32` (`automation.rs:21`). Para un plugin alojado eso son 32 llamadas `process` por bloque de 1024: el coste fijo por llamada se multiplica, los plugins FFT o con lookahead se comportan mal con bloques diminutos y el sandbox multiplica los round-trips.
**Arreglo:** aplicar `Param` y `Macro` de stock también como eventos `Param` temporizados (los procesadores de stock ya reciben `events`), o partir solo el sub-tramo de la cadena hasta el último dispositivo automatizado de stock.

### M2 · La exportación ignora el estado no guardado de los plugins alojados — VERIFICADO · S
`plugin_states()` y `store_plugin_states` solo se llaman al guardar (`app/src/project_io.rs:237-238`). `start_export` (`app/src/views/export.rs:123`) pasa `&self.s.project` tal cual, y el render offline crea instancias nuevas (`engine/src/export.rs:140-141`) a partir del estado del proyecto. Un sonido ajustado en la ventana del plugin y no guardado **se exporta con el estado anterior**.
**Arreglo:** antes de exportar, capturar los estados (`plugin_states` + `store_plugin_states`, o directamente un clon parcheado sin crear un paso de undo).

### M3 · El modelo y el procesador divergen tras quitar automatización — VERIFICADO · S
`ParamAuto::apply` hace `set_param` directamente en el hilo de audio (`graph.rs:399-401`), pero `PoolEntry::applied` sigue con el valor del modelo (`shared.rs:278`). Si se borra o desactiva el lane, `applied` coincide con el modelo y `init_params` sale vacío, así que **el procesador se queda en el último valor automatizado** mientras el knob enseña otro valor.
**Arreglo:** cuando desaparece un `ParamAuto`, forzar ese parámetro en `init_params` (por ejemplo, llevar un conjunto `automated: BTreeSet<(DeviceId, u32)>` en el pool y reenviar los que salen del conjunto).

### M4 · Formato de proyecto sin versión efectiva, sin migraciones y sin fsync — VERIFICADO · M
`PROJECT_FORMAT_VERSION = 1` se escribe (`model/src/project.rs:22,70`), pero `Project::from_json` (`project.rs:149-154`) no lo lee. Un `.aitrack` de una versión más nueva se abre, serde ignora los campos desconocidos y, al guardar, **se pierden sin aviso**. `save` hace `fs::write` + `rename` (`app/src/project_io.rs:85-93`) sin `sync_all` antes del rename: ante un corte de luz puede quedar un archivo truncado con el nombre bueno. No se guarda copia del archivo anterior.
**Arreglo:** leer `format_version` primero (`serde_json::Value`), rechazar las versiones mayores (o abrirlas en solo lectura) y encadenar `migrate_vN_to_vN+1`. Hacer `File::sync_all()` del temporal y del directorio, y conservar `name.aitrack.bak`.

### M5 · El undo coalescido crece sin límite y guardar añade un paso de undo sorpresa — VERIFICADO · S
`model/src/history.rs:116-123`: en un arrastre, cada frame **añade** sus comandos a `forward` y antepone su inverso a `backward`. Un arrastre de 500 notas durante 2 s a 60 fps guarda unas 120 copias de `UpdateNotes` con las 500 notas, unas 60 000 notas clonadas por paso, y deshacer las reaplica todas. Además, `store_plugin_states` (`app/src/state.rs:1625-1637`) crea un paso "Plugin state" en cada guardado: el primer Cmd+Z después de guardar revierte los estados de los plugins, que se reconstruyen, en vez de deshacer la última edición.
**Arreglo:** en la coalescencia, quedarse con el `backward` del primer frame y sustituir `forward` por el del último cuando los comandos tengan el mismo destino. Escribir los estados al guardar sin historial (como un "echo", ya marcado con `note_applied_state`) o fusionarlos con el paso anterior.

### M6 · Un `prepare` de plugin alojado que caduca deja la pista en silencio para siempre, y el hilo de control se bloquea en la UI — VERIFICADO · M
`MainThreadPlugin::prepare` (`app/src/real/plugins.rs:455-463`) mueve el plugin dentro de un closure para el hilo principal. Si vence `LOAD_TIMEOUT` (30 s), `self.inner` se queda en `None` y desde entonces `process` hace `ctx.buffer.clear()` (`plugins.rs:473-476`): **un efecto así silencia toda su pista**. El job llega a ejecutarse más tarde y su resultado se tira. Mientras espera (`main_thread.rs:45-56`), el hilo de control no compila ni vigila el dispositivo, porque la UI solo bombea la cola desde su timer de housekeeping.
**Arreglo:** si vence el tiempo, conservar un "handle" para recuperar la instancia cuando el job termine (canal de vuelta), y mientras tanto pasar la señal sin tocar en lugar de silenciarla. Mover la carga de plugins fuera de `compile` (precarga asíncrona y luego `Recompile`).

### M7 · No hay protección en la salida ni transiciones suaves al cambiar el grafo — VERIFICADO · S/M
La salida se copia sin `is_finite` ni límite (`engine/src/graph.rs:879-884` y `engine/src/rt.rs:329-338`). Los plugins de stock tienen su `sanitize`, cubierto por `robustness.rs`, y los alojados pasan por `host::io::sanitize`. Aun así, un NaN de cualquier otro origen (la mezcla de clips, una automatización con un valor raro) o +24 dB de fader (Main a +12 sobre una pista a +12) llega tal cual al DAC.
`DelayLine::adopt` solo conserva el audio en vuelo si el retardo es idéntico (`engine/src/delay.rs:62-66`): cuando cambia el PDC (un limitador con lookahead, o la latencia de un plugin que se notifica a los 100 ms, `engine.rs:661`), todas las pistas cuyo retardo cambia pierden ese audio, con un clic o un hueco. Un procesador que se reconstruye por `SetDeviceState` arranca en frío, sin crossfade.
**Arreglo:** en el master, sustituir los valores no finitos por 0 y añadir un clip suave opcional (o un "safety limiter" desactivable), con un contador visible en la UI. Hacer un crossfade corto (unos 5 ms) entre el grafo viejo y el nuevo cuando cambian las latencias.

---

## 4. BAJO

| # | Hallazgo | Dónde | Estado | Esfuerzo |
|---|---|---|---|---|
| B1 | `TempoMap::tick_to_seconds` y `seconds_to_tick` recorren todos los puntos (O(N)) y se llaman muchas veces por bloque en el hilo RT (`sample_of`/`tick_of` por evento, por bloque de control y por clip). `fold_tempo_lane` crea un punto por cada punto del lane, así que una automatización de tempo grabada con miles de puntos dispara la CPU. Las curvas (`curve`) del lane de tempo se ignoran (solo rampas lineales). | `model/src/time.rs:115-160`, `engine/src/automation.rs:67-81` | VERIFICADO (código) / SOSPECHA (impacto) | M: tabla de segundos acumulados y búsqueda binaria |
| B2 | `EventList` llena (2048) descarta eventos en silencio, incluidos los NoteOff, y deja notas colgadas. `ActiveNotes` con 512 notas no registra la nota pero el NoteOn sí sale, así que esa nota nunca recibe NoteOff. | `engine/src/clip.rs:71-78`, `clip.rs:149-151` | VERIFICADO | S: reservar sitio para los NoteOff o no emitir el NoteOn |
| B3 | `adopt` empareja los sends por posición y compara solo `pre_fader` y el retardo, no el destino. Al borrar un send, el audio en vuelo del send A acaba en el retorno B. `adopt_devices` es posicional: al insertar un dispositivo delante, se pierden los estados de sleep y los retardos de las cadenas del rack. | `engine/src/graph.rs:682-687`, `graph.rs:888-891`, `graph.rs:292-311` | VERIFICADO | S |
| B4 | Los `MeterBank` y `SessionStatus` nuevos se publican a la UI antes de que el hilo de audio instale el grafo, lo que da un parpadeo de medidores y de estado de clips tras cada edición estructural. | `engine/src/engine.rs:758-766` | VERIFICADO | S |
| B5 | Al crear un retorno, todas las pistas reciben un send a −70 dB, no a −∞: la señal se filtra al retorno y cada send sigue gastando rampa, retardo y suma. | `app/src/state.rs:1017-1020` | VERIFICADO | S |
| B6 | La entrada `Entry::Failed` de `SampleCache` no se reintenta nunca (por ejemplo, un archivo de iCloud aún no descargado) hasta que se limpia la caché o cambia la frecuencia. | `engine/src/sample_cache.rs:456` | VERIFICADO | S |
| B7 | Las pistas que pasan por la vía de sleep, incluidos los instrumentos alojados con secuenciador o arpegiador interno, se duermen tras 0,5 s de silencio sin eventos y no despiertan con el transporte. | `engine/src/graph.rs:159-176` | SOSPECHA | S: no dormir instrumentos alojados mientras el transporte reproduce |
| B8 | UI: mientras se reproduce, el `Workspace` entero se re-renderiza en cada frame (`workspace.rs:2577-2584`), y en cada frame se copia a un `Vec` cada nota de cada clip MIDI visible, sin recortar al rango visible (`views/arrangement.rs:321`). `group_color_commands` clona el proyecto entero por cada subgrupo (`state.rs:1885`). | ver rutas | VERIFICADO | M: separar el playhead y los medidores en entidades propias y cachear la geometría de notas por revisión |
| B9 | Reglas de interfaz: **0** `rgb(0x…)` fuera de `theme.rs` (bien) y los tiempos de las vistas en general fuera de `motion.rs`, pero **39 de 49** literales de espaciado `p*/m*/gap(px(n))` en `views/` no son múltiplos de 4 (gaps de 1 y 2 px). | `crates/app/src/views/**` | VERIFICADO | S |
| B10 | Al reabrir el dispositivo, la posición inicial sale de `status.position_ticks()` (la audible, compensada por PDC), así que el transporte retrocede la latencia total en cada reapertura. | `engine/src/engine.rs:816-823` | VERIFICADO | S: guardar también la posición de motor |
| B11 | Los comandos de transporte se pierden en silencio cuando no hay cola (dispositivo reabriéndose); por ejemplo, Play no hace nada. | `engine/src/engine.rs:206-208` | VERIFICADO | S |

---

## 5. Regla "toda edición es un Command"

En general se cumple. No encontré mutaciones directas del `Project` en el código de producción de `crates/app` (las de `real/plugins.rs:750` y `views/ai_panel/mix.rs:157` están en tests). Las excepciones toleradas son estas:
- `alloc_*_id()` (unos 41 usos) cambia los contadores de ids fuera de un comando. Es inocuo porque los ids son monótonos y `from_json` los reconstruye.
- `ops::*(&mut Project, …)` solo usa `&mut` para reservar ids y devuelve comandos.
- `project_io::settle_notes` aplica un `Batch` sin historial al abrir, a propósito.

Los huecos reales son de **validación**, no de bypass: `RemoveTrack` deja referencias colgando y no se detectan ciclos (A3), y de **forma del undo** (M5).

---

## 6. Orden de arreglo recomendado

1. **C1** (catch_unwind en stock y en el callback, más autosave): es la mayor pérdida de datos posible, con esfuerzo M.
2. **A3** (compilación degradada y errores visibles) y **A4** (abrir los puertos MIDI): esfuerzo S y gran impacto.
3. **A1** (cola de parámetros en tiempo real, sin recompilar) junto con **M1** (automatización de stock temporizada): rendimiento y respuesta.
4. **A2** (warp asíncrono con clave correcta) y **M2**, **M3**, **M4**, **M5**.
5. El resto, según toque cada zona.
