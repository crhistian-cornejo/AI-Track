# 24.04 · Repos open source que resuelven mejor que nosotros el motor, el host de plugins, la librería y la IA

Fecha: 17-nov-2026. Trabajo de investigación: no se tocó código de producción, no hay commits.

**Marcas.**
* **[V]** = verificado: lo leí en el código (cito `ruta:línea`) o en la fuente externa (cito la URL; versiones y fechas sacadas de la API de crates.io, de la página de releases o de la API de GitHub el 17-nov-2026).
* **[S]** = supuesto: inferencia mía o conocimiento previo que no comprobé hoy en una fuente primaria.

**Método.** Leí `CLAUDE.md`, `PLAN.md` (§3, §4, §5, §7, §9), los informes 12 (y su carpeta), 13, 16, 19, 20, 21 y 23, y lo justo de `crates/engine`, `crates/host`, `crates/library`, `crates/ai` y `crates/model` para saber qué hacemos. Fuera, consulté READMEs, páginas de releases, crates.io, docs.rs, Hugging Face y la documentación generada de Ardour y Tracktion. Las capturas se hicieron con Chrome sin cabeza en esta máquina (`--headless --screenshot`), así que muestran la página tal como carga, sin interacción; dos se recortaron con `sips`.

**Límites honestos.** No compilé ni medí ningún repo externo: no hay benchmarks propios en este documento, y no copio cifras de rendimiento de terceros salvo las que publican en su propia página. Las licencias de *pesos* de modelos son terreno jurídico poco claro: digo lo que dice la fuente y marco el riesgo, no doy opinión legal.

---

## 0. Resumen

La arquitectura que tenemos ya es la que usan los referentes (grafo compilado fuera del hilo RT, cola sin espera, cementerio de grafos, PDC con automatización desplazada, escáner y sandbox fuera de proceso, ORT cargado en tiempo de ejecución). Donde nos ganan con claridad es en cinco cosas concretas:

1. **Procesamiento multinúcleo del grafo.** Ardour y Tracktion Engine reparten el grafo entre hilos RT, y en Apple Silicon eso exige unir los hilos al *audio workgroup* del dispositivo. Nosotros procesamos todas las pistas en un solo bucle secuencial (`crates/engine/src/graph.rs:1970-1975`) y el soporte de workgroups es un TODO (`crates/engine/src/thread_priority.rs:5-8`). [V]
2. **Streaming de disco.** `creek` (MIT/Apache, del autor de Meadowlark) resuelve exactamente lo que `sample_cache.rs` deja para "más adelante": hoy todo archivo se decodifica entero a RAM (`crates/engine/src/sample_cache.rs:1-7`). [V]
3. **Aislamiento de plugins por grupos y validación automática.** Bitwig agrupa plugins por fabricante o por plugin en procesos sandbox; nosotros tenemos un proceso por plugin, sin reinicio, y el sandbox está apagado por defecto (`crates/host/src/sandbox/mod.rs:9-12`, `crates/app/src/real/mod.rs:39`). `clap-validator` 0.4.1 (MIT) y `pluginval` 2.0 (GPL-3, solo como herramienta externa) automatizan pruebas que el informe 16 hizo a mano. [V]
4. **Time-stretch.** Signalsmith Stretch (MIT) es la referencia permisiva de más calidad que encontré; el PLAN ya la lista en el stack (`PLAN.md:136`) pero `crates/dsp/src/warp.rs` es implementación propia y nunca se comparó con ella. [V]
5. **Embeddings aprendidos con licencia limpia.** LAION publicó CLAP2 (cinco tamaños, 27 M a 340 M parámetros, pesos Apache-2.0) hace diez días. Es la pieza que faltaba para F4 ("✘ embedding aprendido", `PLAN.md:271`) sin el problema de licencia de los modelos anteriores. [V]

En separación de stems no encontré ningún modelo de calidad comparable a HT-Demucs cuyos pesos tengan licencia comercial clara: la situación de `PLAN.md` §6.2 sigue igual y lo correcto es no cambiar el modelo (§5.3).

---

## 1. Qué hacemos hoy (lo justo para comparar)

| Área | Qué hay | Dónde |
|---|---|---|
| Hilo de audio | Por bloque: instala grafo nuevo si la cola de retiro tiene sitio, aplica transporte y MIDI, procesa segmentos, publica medidores. Sin asignaciones, locks ni E/S. | `crates/engine/src/rt.rs:1-12`, `rt.rs:605-622` [V] |
| Grafo | Compilado en el hilo de control: orden topológico, PDC, rutas, solo/mute; el viejo se libera en el hilo `aitrack-engine-graveyard`. Procesadores compartidos entre grafos con `Arc`. | `crates/engine/src/lib.rs:4-9`, `engine.rs:187-188`, `shared.rs:1-19` [V] |
| Paralelismo | Ninguno en el motor: un `for` sobre `tracks` llama a `process_track` pista a pista. | `crates/engine/src/graph.rs:1970-1975` [V] |
| Topología disponible | El compilador ya calcula sucesores (`succ`) y un orden (`order`) por pista: la materia prima de un planificador paralelo. | `crates/engine/src/compile.rs:479-487`, `compile.rs:526` [V] |
| PDC | Latencia de cadena acumulada por la ruta; cada arista al punto de suma recibe un retardo hasta la llegada más lenta; la automatización se desplaza por la latencia de entrada de la cadena. | `compile.rs:23-27`, `compile.rs:639-650`, `graph.rs:2089` [V] |
| Prioridad RT | Windows: MMCSS "Pro Audio" crítico. macOS: no hace nada (CoreAudio ya da hilo de tiempo restringido); workgroups pendiente. | `crates/engine/src/thread_priority.rs:1-42` [V] |
| Prueba de cero asignaciones | Asignador contador solo en tests. | `crates/engine/src/alloc_counter.rs:1-3`, `lib.rs:31-32` [V] |
| Audio de clips | Decodificación completa (hound/symphonia), remuestreo con rubato a la frecuencia del dispositivo, todo en memoria; `creek` "más adelante". | `crates/engine/src/sample_cache.rs:1-7`, `sample_cache.rs:256` [V] |
| MIDI | `midir` 0.11, mensajes cortos con marca de tiempo del reloj del motor; sysex, reloj y sensing ignorados (`Ignore::All`). | `crates/engine/src/midi_io.rs:60-69` [V] |
| Eventos | `EventKind` ya tiene `NoteExpression` (afinación, presión, pan, brillo, volumen) y `note_id`, es decir, el modelo de CLAP/MPE. | `crates/dsp/src/event.rs:11-36` [V] |
| Host | VST3 con `vst3` 0.3 (bindings MIT/Apache del SDK 3.8), CLAP con `clack-host` 0.2; `catch_unwind` en cada llamada; sandbox con memoria compartida y entrega en el mismo ciclo (presupuesto 70 % del bloque); vigilante de 5 s; escáner fuera de proceso con 20 s de timeout y lista de bloqueo. | `crates/host/Cargo.toml:20-23`, `plugin.rs:20-31`, `sandbox/protocol.rs:11-23`, `sandbox/mod.rs:76`, `scan/mod.rs:3-14,39` [V] |
| Librería | SQLite, escaneo incremental por mtime+tamaño, `notify`, archivos de nube por nombre; "similares" con un vector DSP (MFCC + forma espectral + envolvente) en una matriz contigua, búsqueda por producto escalar. | `crates/library/src/scan.rs:1-10`, `watch.rs:1-11`, `similar.rs:1-20`, `crates/ai/src/embedding.rs:1-22` [V] |
| IA local | ORT 2.0.0-rc.15 con `load-dynamic`, CoreML/DirectML y CPU de respaldo; basic-pitch v0.4.0 (Apache-2.0); HT-Demucs 4 y 6 stems con aviso y aceptación de licencia; HPSS propio para batería; proceso `aitrack-ml-worker` aparte. | `crates/ai/Cargo.toml:48-54`, `ai/src/runtime.rs:1-10`, `models.rs:129-199`, `stems.rs:1-17`, `drum_extract.rs:1-9`, `worker.rs:1-8` [V] |
| Agentes | Claude Code recibe nuestras herramientas como servidor MCP "sdk" por su propio stdio; no hay servidor MCP externo ni `rmcp` en `Cargo.lock`. | `crates/ai/src/llm/cli/claude_code.rs:1-3,142-146`; `grep rmcp Cargo.lock` vacío [V] |

### 1.1 Qué cambió respecto a informes anteriores

| Informe | Hallazgo | Estado hoy |
|---|---|---|
| 16 · P4 | `has_sidechain: false` siempre | **Sigue abierto**: `crates/host/src/plugin.rs:258`, `sandbox/mod.rs:334` [V] |
| 16 · P7 | `setProcessing(1)` en el hilo de audio | **Cambió de forma, no de fondo**: ahora se llama de forma perezosa la primera vez dentro de `process` (`crates/host/src/vst3/instance.rs:886-889`); sigue siendo el hilo RT [V] |
| 16 · P9 | `process::exit` en el escáner ejecuta destructores | **Sigue abierto**: `crates/host/src/bin/aitrack-plugin-scanner.rs:11` [V] |
| 16 · P3 | Sandbox apagado por defecto | **Sigue igual**: `crates/app/src/real/mod.rs:39` [V] |
| 19 · B3 | `adopt` empareja sends solo por `pre_fader` y retardo | **Corregido en parte**: `SendNode::target_id_matches` ya compara `target_id` (`crates/engine/src/graph.rs:1989-1993`); no re-audité `adopt_devices` [V/S] |
| PLAN §4 | `creek` y `signalsmith-stretch` en el stack (`PLAN.md:136`) | Ninguno de los dos está en el código; `warp.rs` es propio (`crates/dsp/src/warp.rs:1-14`) [V] |
| PLAN §5 | Prototipo clack vs truce (`PLAN.md:183`) | `truce` tiene licencia propia (§3.3) que `deny.toml` no admite; la decisión cambia [V] |
| PLAN §6.1 | MCP con `rmcp` en 127.0.0.1 | Aún no; solo el MCP embebido para Claude Code (arriba) [V] |

---

## 2. Motor

### 2.1 Procesamiento multinúcleo del grafo

**Qué hacen mejor.** Ardour tiene una clase `Graph` que dispara nodos listos (`trigger(ProcessNode*)`), cuenta hilos (`n_threads()`), procesa rutas en cadenas (`process_routes(GraphChain…)`) y tareas sueltas (`process_tasklist(RTTaskList)`) [V, captura]. Tracktion Engine separó su grafo en `tracktion_graph` con un `LockFreeMultiThreadedNodePlayer`: el cambio de grafo y el procesado son sin locks, el pool de hilos es intercambiable (estrategias `conditionVariable`, `realTime`, `hybrid`, `semaphore`, `lightweightSemaphore`…) y el constructor recibe un `juce::AudioWorkgroup` [V, captura y https://tracktion.github.io/tracktion_engine/tracktion__NodePlayerThreadPools_8h.html]. Bitwig ofrece cinco modos de alojamiento de plugins, desde "dentro del motor" hasta "cada instancia sola" [V, captura en §3.1].

![Ardour::Graph: trigger, n_threads, process_routes y process_tasklist](img/04-ardour-graph-class.png)
*Referencia de la clase `ARDOUR::Graph` (Ardour 9.10): métodos `trigger`, `n_threads`, `process_routes` sobre `GraphChain` y `process_tasklist`. https://ardour.org/files/doxygen/classARDOUR_1_1Graph.html*

![Tracktion LockFreeMultiThreadedNodePlayer](img/04-tracktion-lockfree-multithreaded-player.png)
*`graph::LockFreeMultiThreadedNodePlayer` de Tracktion Engine: pool de hilos personalizable (`ThreadPoolCreator`), constructor con `juce::AudioWorkgroup`, `setNumThreads`. https://tracktion.github.io/tracktion_engine/classgraph_1_1LockFreeMultiThreadedNodePlayer.html*

**El problema de Apple Silicon.** Blue Cat Audio documentó en noviembre de 2023 que en Apple Silicon (desde Ventura) las apps con procesamiento de audio en paralelo sufren cortes aleatorios *con poca carga de CPU*, que la solución pasa por los *Audio Workgroups* (macOS 11+) y que "ya no se puede prescindir" de ellos en apps de audio [V, captura]. Mozilla tiene abierto (abril-mayo 2026) un PR para exponer el workgroup de E/S del dispositivo en `cubeb-coreaudio-rs` (https://github.com/mozilla/cubeb-coreaudio-rs/pull/280) [V]; `audio_thread_priority` 0.38.0 (MPL-2.0) sube prioridad con políticas Mach, pero su documentación no menciona workgroups (https://docs.rs/audio_thread_priority/latest/audio_thread_priority/) [V].

![Blue Cat Audio: multicore en Apple Silicon y Audio Workgroups](img/04-bluecat-audio-workgroups.png)
*Artículo de Blue Cat Audio (21-nov-2023) sobre cortes de audio en Apple Silicon con procesamiento paralelo y poca carga. https://www.bluecataudio.com/Blog/announcements/realtime-audio-multicore-issues-for-apple-silicon-end-of-the-story/*

**Dónde lo notaríamos.** `graph.rs:1970-1975` procesa pistas en serie, así que un proyecto con muchas pistas con plugins pesados usa un núcleo y el resto espera. `PLAN.md:270` lo marca como ✘. Con sandbox el coste crece: cada plugin en sandbox es una espera del hilo RT sobre una señal (`sandbox/protocol.rs:18-23`), y en macOS esa espera es un sondeo con `sem_trywait` + spin + yield (`sandbox/shm.rs:5-7`) [V]. En serie, esas esperas se suman; en paralelo se solaparían.

**Diseño recomendado (propio).** No hace falta work-stealing general (rayon no es RT-safe [S]). Ardour y Tracktion usan una cola de nodos listos con contadores de dependencias, hilos de trabajo creados al abrir el dispositivo y despertados por semáforo, con un spin corto antes de dormir [V las estrategias de Tracktion; S el detalle interno de Ardour]. Nosotros tenemos `succ` y `order` en `compile.rs:479-530`; basta con:
1. Guardar en el `CompiledGraph` el número de predecesores y la lista de sucesores de cada pista (se calcula en el hilo de control, sin coste RT).
2. Un pool de N-1 hilos RT (MMCSS en Windows; en macOS, `os_workgroup_join` con el workgroup que da `kAudioDevicePropertyIOThreadOSWorkgroup` para el dispositivo abierto con cpal), cada uno con un *slot* de trabajo y un semáforo.
3. Las sumas a la entrada de grupos y retornos (`inputs[i]`) son el punto delicado: hoy `process_track` escribe en el buffer del destino. Hay que cambiarlo a "cada pista escribe su salida en su propio buffer y el destino suma al empezar", que además permite arrancar el destino en cuanto llega su último predecesor.
4. Volver al camino de un hilo con pocas pistas activas o bloques muy cortos [S: umbral a medir].

Licencias: Ardour (GPL-2.0-or-later [S]) y Tracktion Engine (GPL-3 o comercial [V, https://github.com/Tracktion/tracktion_engine/blob/develop/LICENSE.md]) solo como referencia de algoritmo. Implementación propia.

### 2.2 Streaming de disco: `creek`

**Qué hace mejor.** `creek` 1.2.3 (MIT OR Apache-2.0, 22-sep-2025) lee y escribe audio en disco desde el hilo RT sin bloquear: un hilo "IO server" hace el trabajo no RT; el lector tiene *caches* precargadas (inicio del archivo, región de bucle) para saltos instantáneos y un *look-ahead* que se rellena por delante del cursor; si el dato no está, el usuario elige entre silencio o pausa. El escritor recicla bloques entre el hilo RT y el de E/S. Decodifica con Symphonia, salvo AAC/MP4, que no soporta [V, captura y https://crates.io/crates/creek].

![creek: cómo funciona el stream de lectura](img/04-creek-read-stream.png)
*docs.rs de `creek` 1.2.3: cache buffers, look-ahead, hilo IO server y stream de escritura. Licencia MIT OR Apache-2.0. https://docs.rs/crate/creek/latest*

**Dónde lo notaríamos.** `sample_cache.rs:1-7` decodifica y remuestrea cada archivo entero y lo guarda en memoria; una grabación de una hora a 48 kHz estéreo en f32 son unos 1,4 GB [S, cálculo: 3600·48000·2·4 B]. También en la grabación: `record.rs:280-284` ya tiene un hilo escritor propio, el mismo papel que el `WriteDiskStream` de creek [V]. PLAN F2 lo marca ✘ (`PLAN.md:269`).

**Uso.** Dependencia directa compatible con `deny.toml` (MIT/Apache). Riesgos: un solo mantenedor (ya en `PLAN.md:413`), última versión hace un año, y `creek-decode-symphonia` depende de una versión concreta de Symphonia que habrá que alinear con la nuestra [S]. Encaja detrás de `DecodedAudio`, como ya prevé `sample_cache.rs:6-7`. Para clips con warp o remuestreo, el streaming obliga a remuestrear en tiempo real; lo sensato es mantener el camino actual para clips cortos y usar creek solo por encima de un umbral de duración [S].

### 2.3 Seguridad en tiempo real: comprobación y liberación diferida

| Recurso | Licencia | Qué hace mejor | Nuestro equivalente | ¿Usar? |
|---|---|---|---|---|
| `basedrop` 0.1.3 | MIT/Apache [V] | `Owned`/`Shared` que encolan su liberación a otro hilo; sirve para pasar datos sueltos (un sample nuevo, una tabla) al hilo RT sin cambiar de grafo | El cementerio de grafos completos (`engine.rs:187-188`) y `SharedProcessor` (`shared.rs:1-19`) | Solo si aparece un caso frecuente de "dato suelto"; hoy el cementerio cubre lo que hay. Referencia. |
| `assert_no_alloc` 1.1.2 | **BSD-1-Clause** [V] | Asignador global que aborta o avisa si el hilo RT asigna, también en builds de depuración de la app | `alloc_counter.rs`, solo en tests (`alloc_counter.rs:1-3`) | **No sin tocar `deny.toml`**: BSD-1-Clause no está en la lista (`deny.toml:8-24`). Es fácil hacerlo propio sobre nuestro contador. |
| `rtsan-standalone` 0.3.0 | Apache-2.0 [V] | RealtimeSanitizer de LLVM para Rust: intercepta `malloc`, `pthread_mutex_lock`, E/S… dentro de funciones marcadas, también en código C/C++ de plugins | Nada para código de terceros | **Sí, en tests y en un modo de diagnóstico**: ve lo que nuestro contador no ve (locks, syscalls, plugins). https://github.com/realtime-sanitizer/rtsan-standalone-rs |
| `rtcheck` (Tracktion) | GPL-3 [V] | Lo mismo para C++; pluginval 2.0 lo usa con `--rtcheck` en macOS | — | Solo como herramienta externa vía pluginval, nunca enlazado. https://github.com/Tracktion/rtcheck |
| `audio_thread_priority` 0.38.0 | MPL-2.0 [V] | Promoción RT multiplataforma, también de hilos de otro proceso | `thread_priority.rs` propio (MMCSS) | Útil para promover el hilo RT del proceso sandbox, que hoy no se promueve (`sandbox/mod.rs:9-12`); MPL-2.0 está permitida (`deny.toml:21`). |

### 2.4 Crates base: dónde estamos frente a la última versión

| Crate | Usamos | Última (17-nov-2026) | Licencia | Comentario |
|---|---|---|---|---|
| `cpal` | 0.18 (`engine/Cargo.toml:28`) | 0.18.2 (16-ago-2026) | Apache-2.0 | Al día. 0.18.2 añade estado de xrun en la salida por defecto de CoreAudio y quita pánicos de WASAPI al perder el dispositivo [V, https://github.com/rustaudio/cpal/releases] |
| `rtrb` | 0.3 (`Cargo.toml:50`) | 0.4.1 (25-oct-2026) | MIT/Apache | Una versión por detrás [V] |
| `rubato` | 0.16 (`engine/Cargo.toml:31`, `ai/Cargo.toml:31`) | **5.1.0** (12-nov-2026) | MIT/Apache | 1.0 eliminó `SincFixedIn` (que usamos en `sample_cache.rs:256`) en favor de un tipo `Async` unificado con buffers de `audioadapter`; 5.1 hace públicos los interpoladores sinc [V, captura y https://github.com/Recappi/sdk/pull/13] |
| `symphonia` | 0.5 (`engine/Cargo.toml:32`) | 0.6.1 (13-ago-2026) | MPL-2.0 | 0.6 rediseñó las primitivas de audio y de metadatos [V, https://github.com/pdeljanov/Symphonia/releases] |
| `midir` | 0.11 | 0.11.1 | MIT | Al día [V] |
| `ort` | =2.0.0-rc.15 | 2.0.0-rc.15 (19-oct-2026) | MIT/Apache | Al día; rc.15 corrige más de 30 casos de comportamiento indefinido y el autor pide actualizar [V, https://github.com/pykeio/ort/releases] |
| `clack-host` | 0.2 | 0.2.0 (12-sep-2026) | MIT/Apache | Al día; 0.2 pasa a referencias compartidas `&` en el hilo principal para admitir reentrada [V, captura] |
| `vst3` | 0.3 | 0.3.0 (7-dic-2025) | MIT/Apache | Al día [V] |

![rubato: releases hasta 5.1.0](img/04-rubato-releases.png)
*Releases de rubato: v5.1.0 (12-nov-2026) hace públicos los interpoladores sinc; la lista lateral muestra el salto 0.16.2 → 1.0 → 5.1. https://github.com/HEnquist/rubato/releases*

![clack v0.2](img/04-clack-v0-2-reentrancy.png)
*clack v0.2 "Embracing re-entrancy" (12-sep-2026): las interfaces del hilo principal pasan de `&mut` a `&` y el `Window` del GUI gana una vida `'w`. https://github.com/prokopyl/clack/releases*

### 2.5 PDC

Nuestro PDC es de grafo completo y ya desplaza la automatización (§1). Lo que no verifiqué es si las tomas de sidechain (`side`, `graph.rs:1511-1513`) se alinean con la latencia de la cadena que las alimenta: el compilador solo avisa de bucles de sidechain (`compile.rs:554`) [V el aviso; S la posible falta de alineación]. Mejora: un test con `test.latency` (`fallback.rs:10-13`) en la pista que alimenta un sidechain y, si falla, retrasar la toma como ya se hace con los sends. Esfuerzo S, sin dependencias.

### 2.6 Time-stretch: Signalsmith Stretch

**Qué hace mejor.** Biblioteca C++11 de un solo encabezado (MIT, último push 22-oct-2026) para pitch-shift y time-stretch polifónico, con "tonality limit" para conservar timbre al transponer; el autor dice que el stretch suena mejor entre 0,75× y 1,5× [V, captura y https://github.com/Signalsmith-Audio/signalsmith-stretch]. En Rust existen `signalsmith-stretch` 0.1.3 (MIT, envoltorio) y `ssstretch` 0.1.0 (MIT) [V].

![Signalsmith Stretch](img/04-signalsmith-stretch.png)
*Página del proyecto: C++11 header-only, extensión del enfoque de la charla ADC22 "Four Ways To Write A Pitch-Shifter"; presets y `configure()`. https://signalsmith-audio.co.uk/code/stretch/*

**Dónde lo notaríamos.** Los modos Complex, Complex Pro y Tones de `crates/dsp/src/warp.rs:1-14` (vocoder de fase con bloqueo de picos Laroche–Dolson, WSOLA, formantes por cepstrum). Es la parte más audible del motor al hacer warp de loops con armonía. Sin Rubber Band (`PLAN.md` §10.7), Signalsmith es la única referencia permisiva de primer nivel que encontré.

**Uso.** Dependencia posible (MIT, compatible) pero trae un compilador C++ al build. Alternativa: usarla solo como **oráculo en tests** (render A/B de los mismos loops, medir error espectral y conservación de transitorios) y portar las ideas que ganen. Recomiendo empezar por el oráculo.

### 2.7 MIDI 2.0, MPE y sincronización

* **MIDI 2.0.** `midi2` 0.11.1 (MIT/Apache, abril de 2026) tipa todos los mensajes UMP (Channel Voice 1/2, Sysex7/8, Flex Data, UMP Stream) y convierte bytes MIDI 1 a UMP [V, https://docs.rs/midi2/latest/midi2/]. Nuestro `EventKind` ya tiene `note_id` y `NoteExpression` (`dsp/src/event.rs:11-36`), así que el modelo interno ya admite expresión por nota; lo que falta es E/S UMP del sistema operativo (`midir` solo da MIDI 1 [S]). Prioridad baja (PLAN F7 "✘ MPE completo", `PLAN.md:274`).
* **Ableton Link.** Sigue con licencia doble GPLv2+ o propietaria; **Link 5.0 beta 1** salió la semana pasada y añade sincronía de posición de arreglo y audio compartido entre apps (Link Audio) [V, captura y https://github.com/ableton/link]. `rusty_link` 0.4.9 envuelve el SDK y es GPL-2.0-or-later [V crates.io]. No encontré ninguna reimplementación permisiva del protocolo [V: búsqueda sin resultados]. Alternativa sin licencia: **reloj MIDI de entrada y salida** (24 ppq, Start/Stop/SPP) en `midi_io.rs`, que hoy ignora esos mensajes (`midi_io.rs:69`) [V]. Link, solo con acuerdo comercial con Ableton (lo que ya dice `PLAN.md` §10.7).

![Ableton Link: licencia](img/04-ableton-link-licencia.png)
*README de Ableton Link: sincroniza tempo, fase, start/stop y posición de arreglo, y comparte audio; licencia doble GPLv2+ y propietaria. https://github.com/ableton/link*

### 2.8 Motores y DAWs de referencia

| Proyecto | Licencia | Estado 2025-2026 | Qué hace mejor | Cómo usarlo |
|---|---|---|---|---|
| Ardour | GPL-2.0+ [S] | Doxygen generado hoy para 9.10 [V captura] | Grafo multihilo, PDC completo, butler de disco | Referencia de algoritmos |
| Tracktion Engine | GPL-3 / comercial [V] | Activo | `tracktion_graph` sin locks, pools RT, workgroups; pluginval es suyo | Referencia |
| Zrythm | AGPL/GPL [V, https://manual.zrythm.org/nl/getting-started/faq.html] | v2 alpha (reescritura en C++23 con nueva arquitectura de host), agosto de 2026 [V, https://bedroomproducersblog.com/2026/08/06/zrythm-2-alpha/] | Automatización y host modernos | Referencia de ideas |
| LMMS | GPL-2.0 [V API GitHub] | Activo | Poco relevante para nuestro motor [S] | — |
| openDAW | AGPL-3.0+ [V, https://github.com/andremichelle/openDAW] | Activo | DAW web; motor en AudioWorklet | Referencia de UX |
| DawDreamer | GPL-3 [V, https://github.com/DBraun/DawDreamer] | v0.9.0 en PyPI | Render offline programático con plugins, Faust y warp markers | Idea para un render headless de pruebas |
| Meadowlark / rainout | AGPL-3.0 [V] / archivado [V] | Pausado: último push 22-sep-2025; el autor anunció una pausa (https://billydm.github.io/blog/why-im-taking-a-break-from-meadowlark/) [V] | Diseño de motor en Rust | Sus crates sueltos (creek, firewheel) sí son MIT/Apache |
| Firewheel 0.15.0 | MIT/Apache [V] | Activo (13-nov-2026); se integrará en Bevy | Grafo de audio sin mutex, tolerancia a desconexión del dispositivo, optimización de silencio; **declara que no es un motor de DAW** | Referencia de API de nodos, no motor [V captura] |
| fundsp 0.23.0 | MIT/Apache [V] | Activo | DSP combinatorio | Prototipos y tests de DSP [S] |
| rodio 0.23.0 | MIT/Apache [V] | Activo | Reproducción simple | No aporta a un DAW [S] |

![Firewheel README](img/04-firewheel-readme.png)
*README de Firewheel 0.15.0: futuro dentro de Bevy, características (grafo dirigido acíclico, nodos propios, optimización de silencio, sin mutex) y "Non-features": no pretende ser un motor de DAW. MIT OR Apache-2.0. https://github.com/BillyDM/firewheel*

---

## 3. Host de plugins

### 3.1 Aislamiento y sandbox

**Qué hace mejor Bitwig.** Cinco modos: dentro del motor, todos juntos en un sandbox, por fabricante (permite que los plugins de un fabricante se comuniquen), por plugin (todas las instancias de un plugin juntas, ahorra memoria) e individual. En la misma versión dejaron de reservar una zona de memoria compartida por plugin y redujeron los diálogos durante el escaneo [V, captura].

![Bitwig 2.5: modos de alojamiento](img/04-bitwig-25-modos-sandbox.png)
*Notas de Bitwig Studio 2.5 (7-mar-2019), "Plug-in hosting overhaul": modos By Manufacturer, By Plug-in, Within Bitwig, Together e Individually. https://downloads.bitwig.com/stable/2.5/Release-Notes-2.5.html*

**Dónde lo notaríamos.** Nuestro sandbox es "Individually" sin alternativa (`sandbox/mod.rs:9-12`): 40 instancias son 40 procesos y 40 regiones SHM. El modo "por fabricante" ayudaría además con el choque de clases Objective-C de los plugins DPF que encontró el informe 16 (P6): Dragonfly Hall/Plate/Room irían juntos en su propio proceso, separados del resto [V el choque en 16; S que lo resuelva del todo]. Carla (GPL-2.0+ [V, https://kx.studio/Applications:Carla]) y yabridge (GPL-3 [V, https://github.com/robbert-vdh/yabridge/]) son referencias de puentes por proceso; solo referencia.

**Mejora (propia).** Que `aitrack-plugin-host` aloje N instancias (un hilo RT que recorre sus instancias por bloque, una señal por proceso), con clave de agrupado configurable (`individual | fabricante | plugin | todos`), reinicio con el último estado guardado tras un crash y promoción RT de su hilo (§2.3). Esfuerzo L.

### 3.2 Validación automática

* **clap-validator 0.4.1** (MIT; 22-jul-2026; binarios para macOS universal, Windows y Linux) [V, captura]. Tiene modo `fuzz` con duración y paralelismo (`clap-validator fuzz -j4 -d10m30s plugin.clap`) [V, https://github.com/free-audio/clap-validator]. Un hilo de KVR anuncia un fork con pruebas de ciclo de vida de DAW y cuenta que el original vuelve a mantenerse [V, https://www.kvraudio.com/forum/viewtopic.php?t=631693].
* **pluginval 2.0.0** (GPL-3; beta3 del 14-oct-2026): subcomandos, pruebas de aceptación contra un WAV de referencia (`pluginval test`), salida JUnit/JSON para CI, validador VST3 integrado y `--rtcheck` en macOS para detectar asignaciones y locks en el `process` del plugin [V, captura y https://github.com/Tracktion/pluginval/releases].

![pluginval 2.0.0](img/04-pluginval-2-changelist.png)
*CHANGELIST de pluginval 2.0.0: subcomandos, pruebas de aceptación con WAV de referencia, `--junit-xml`/`--json-results` y acciones de GitHub. https://github.com/Tracktion/pluginval/blob/develop/CHANGELIST.md*

![clap-validator 0.4.1](img/04-clap-validator-0-4-1.png)
*Releases de clap-validator: 0.4.1 (22-jul-2026) con binarios para macOS, Linux y Windows. https://github.com/free-audio/clap-validator/releases*

**Dónde lo notaríamos.** El informe 16 hizo a mano lo que estas herramientas automatizan, y su problema más serio (SIGKILL intermitente en Surge XT con editores) quedó sin causa. Dos usos: en CI, contra nuestros propios plugins si algún día se exportan como CLAP/VST3; y como **pre-chequeo opcional** del escáner, de modo que un plugin que falla `clap-validator` vaya a sandbox por defecto. pluginval es GPL-3: se puede ejecutar como herramienta externa en CI sin enlazarlo ni distribuirlo [S]; no se empaqueta con la app.

### 3.3 Framework para exportar nuestros dispositivos (PLAN §5)

| Opción | Licencia | Estado | Veredicto |
|---|---|---|---|
| `clack` (plugin) | MIT/Apache [V] | 0.2.0, sep-2026 | Bajo nivel, sin GUI propia; compatible con `deny.toml` |
| `truce` | **Truce License 1.0**: MIT/Apache con una cláusula que prohíbe ofrecer truce como framework comercial a otros desarrolladores [V, https://raw.githubusercontent.com/truce-audio/truce/main/LICENSE] | 6.3.0, jul-2026 [V] | La cláusula no nos afecta (somos un DAW), pero su SPDX es `LicenseRef-TruceLicense-1.0` y `cargo deny` lo rechaza: habría que añadir una excepción explícita. Riesgo medio. |
| `nih-plug` | ISC [S] | En modo mantenimiento; el README remite al fork comunitario https://codeberg.org/BillyDM/nih-plug [V] | No elegir el original |
| `nice-plug` | — | "Experimental" según su README [V, https://codeberg.org/RustAudio/nice-plug] | Esperar |

Recomendación: `clack` para CLAP (ya lo usamos en el host) y decidir VST3 aparte; si se prefiere `truce`, añadir la excepción en `deny.toml` de forma consciente.

### 3.4 Escaneo

El escáner (`scan/mod.rs:3-14`) ya hace lo que hacen los buenos: `moduleinfo.json` sin cargar binario, proceso aparte con timeout, caché por mtime/tamaño y lista de bloqueo. De fuera solo queda lo de Bitwig 2.5 (menos diálogos al escanear); el `_exit` de P9 sigue pendiente. No hay nada que adoptar.

---

## 4. Librería

**Embeddings e índice.** Hoy `similar.rs` usa un vector DSP y una matriz contigua (10 000 muestras en unos 1 ms según `similar.rs:18-20`) [V]. El índice no es el cuello de botella: un índice aproximado (HNSW con `usearch` 2.26.6, Apache-2.0, o `sqlite-vec` 0.1.9, MIT/Apache [V crates.io]) solo compensa con cientos de miles de vectores [S]. Lo que falta es un embedding aprendido para **búsqueda por texto** ("kick oscuro y seco") y mejor similitud tímbrica: ver CLAP2 en §5.4. El trait `Embedder` (`similar.rs:59-70`) ya admite versiones y varios embebedores en la misma tabla, así que no hay que tocar el esquema.

**Tonalidad y BPM.** Essentia y aubio siguen bloqueados por licencia (`PLAN.md` §6.2) y no encontré alternativas permisivas mejores que nuestro chroma + perfiles Krumhansl–Kessler/Temperley (`ai/src/key.rs:1-2`) y autocorrelación de onsets (`ai/src/tempo.rs:1-3`) [V el código; S que no existan]. Sin cambios.

---

## 5. IA

### 5.1 Runtime: `ort` frente a `candle`

`ort` 2.0.0-rc.15 (MIT/Apache) es la última y la tenemos fijada; la rama 2.0 usa ONNX Runtime 1.31 [V, https://ort.pyke.io/misc/version-mapping]. `candle` 0.11.1 (MIT/Apache, oct-2026) es Rust puro, sin binario externo que descargar [V crates.io]. Simplificaría la distribución para modelos pequeños, pero perderíamos CoreML/DirectML y las exportaciones ONNX existentes [S]. Recomendación: seguir con `ort` y mirar `candle` solo si la descarga de ORT (`runtime.rs:3-6`) da problemas a usuarios.

### 5.2 Audio → MIDI

basic-pitch sigue en v0.4.0 (esa release añadió el código de entrenamiento), con commits de mantenimiento en noviembre de 2025 [V, https://github.com/spotify/basic-pitch/releases, https://github.com/spotify/basic-pitch/commits/main/]; la tenemos completa (`midi/basic_pitch.rs:1-8`). La alternativa multi-instrumento es **MT3** de Magenta (Apache-2.0 [V, https://github.com/magenta/mt3/]): transcribe varios instrumentos a la vez, pero es un T5 codificador-decodificador mucho más pesado y no tiene exportación ONNX oficial (hay una conversión GGUF de terceros) [V la licencia y la conversión; S el coste]. Recomendación: no ahora; para "audio → MIDI por instrumento" basta combinar stems + basic-pitch, que ya tenemos.

### 5.3 Separación de stems

| Modelo | Código | Pesos | Fuente |
|---|---|---|---|
| HT-Demucs | MIT | "Not covered by the MIT license… only for scientific purposes" (autor, 2022); repo archivado el 1-ene-2025 | https://github.com/facebookresearch/demucs/issues/327 [V captura] |
| MDX23C, BS-RoFormer | MIT (implementación de lucidrains) | Entrenados en parte con MUSDB18-HQ, de uso educativo y no comercial según su co-autor | https://audioshake.ai/post/can-demucs-be-used-commercially-musdb18-model-weights-licensing [V] |
| Open-Unmix UMX-L | MIT | CC-BY-NC-SA-4.0 | https://github.com/sigsep/open-unmix-pytorch [V] |
| Spleeter | MIT | Sin aclarar; la pregunta "¿los modelos también son MIT?" sigue abierta (jul-ago 2026) | https://github.com/deezer/spleeter/issues/957 [V] |

![Demucs: licencia de los pesos](img/04-demucs-issue-327-pesos.png)
*Issue #327 de Demucs: el autor responde que los pesos no están cubiertos por la licencia MIT y se ofrecen solo para fines científicos; el repo está archivado desde el 1-ene-2025. https://github.com/facebookresearch/demucs/issues/327*

Conclusión: **no hay sustituto con licencia más clara**. Nuestro enfoque (no redistribuir, aviso y aceptación explícita en `models.rs:129-132`, HPSS propio como respaldo en `drum_extract.rs:1-9`) coincide con el que plantean en público otras apps comerciales (https://www.reddit.com/r/OpenSourceeAI/comments/1x7d9xw/can_a_commercial_dj_app_support_optional/) [V la pregunta; S que baste jurídicamente]. Mixxx documentó cómo exportar HT-Demucs v4 a ONNX reescribiendo STFT/iSTFT para que el modelo sea autocontenido (https://mixxx.org/news/2025-10-27-gsoc2025-demucs-to-onnx-dhunstack/) [V]: es exactamente el formato que espera `stems.rs:3-7` y serviría para exportarlo nosotros en vez de depender de un repo de terceros en Hugging Face (`models.rs:170`).

### 5.4 Embeddings de audio: CLAP y alternativas

| Modelo | Pesos | Tamaño | Fuente |
|---|---|---|---|
| **CLAP2 (LAION, nov-2026)** | **Apache-2.0** | 27,3 M (XXS), 48,3 M (XS), 103,9 M (S), 152,8 M (M), 340,1 M (L); la torre de audio acepta cualquier longitud | https://huggingface.co/laion/clap2-large-audioxl-s1B [V captura]; código MIT, https://github.com/LAION-AI/clap2 [V] |
| LAION `larger_clap_music` | Apache-2.0 (etiqueta de HF) | — | https://huggingface.co/laion/larger_clap_music [V API HF] |
| Microsoft CLAP (msclap 2022/2023) | **MS-PL** (etiqueta de HF); código MIT | — | https://huggingface.co/microsoft/msclap [V API HF] |
| PANNs | Código MIT [S]; pesos en Zenodo | CNN14 | https://github.com/qiuqiangkong/audioset_tagging_cnn [V el repo; S la licencia de los pesos] |

![CLAP2: licencia Apache-2.0 y tamaños](img/04-laion-clap2-licencia.png)
*Ficha de `laion/clap2-large-audioxl-s1B`: licencia apache-2.0, cinco tamaños con sus puntuaciones MAEB-30/HEAR-17/MSEB y torre de audio sin longitud fija. https://huggingface.co/laion/clap2-large-audioxl-s1B*

**Dónde lo notaríamos.** F4 "✘ embedding aprendido (CLAP/ONNX)" (`PLAN.md:271`) y las herramientas del agente que buscan samples por texto. CLAP2-XS o XXS están cerca del presupuesto de "~34 MB cuantizado" del PLAN (§6.2) [S: 48 M parámetros en int8 ≈ 48 MB]. Riesgos: los pesos son Apache-2.0, pero el corpus mezcla licencias por clip (94 % Creative Commons, con un subconjunto no comercial) [V, https://projects.laion.ai/clap2/]; el modelo se publica en formato OpenCLIP/Transformers, no ONNX, así que hay que exportarlo y comprobar paridad numérica [S]; y salió hace diez días, con benchmarks que mezclan habla, sonido y música [V la mezcla]. Conviene probarlo con nuestra librería antes de cambiar el embebedor por defecto.

### 5.5 Servidor MCP: `rmcp`

`rmcp` 3.11.0 (Apache-2.0, publicado hoy) es el SDK oficial; implementa la especificación MCP estable **2026-07-28** y sigue siendo compatible con 2025-11-25 [V, captura]. Nuestro MCP embebido para Claude Code negocia `2025-11-25` en sus tests (`claude_code.rs:248`) [V]. El servidor externo de F5 (`PLAN.md:272`) se puede construir con `rmcp` por stdio reutilizando las herramientas de `crates/ai/src/llm/tools/` [S el grado de reutilización]. Coste: trae `tokio` (ya está en `Cargo.lock` [V]) y la serie 3.x tuvo cambios incompatibles con guía de migración (https://github.com/modelcontextprotocol/rust-sdk/discussions/969) [V]: hay que fijar la versión.

![rmcp 3.11.0](img/04-rmcp-crate.png)
*crates.io de `rmcp` 3.11.0: SDK oficial, especificación MCP 2026-07-28 compatible con 2025-11-25, licencia Apache-2.0. https://crates.io/crates/rmcp*

---

## 6. Licencias frente a `deny.toml`

| Recurso | Licencia | ¿Pasa `deny.toml`? | Uso recomendado |
|---|---|---|---|
| creek, firewheel, basedrop, rtrb, rubato, clack, vst3, fundsp, rodio, candle, ort, midi2, sqlite-vec | MIT y/o Apache-2.0 | Sí | Dependencia si aporta |
| symphonia, audio_thread_priority | MPL-2.0 | Sí (`deny.toml:21`) | Dependencia |
| cpal, rmcp, usearch, rtsan-standalone | Apache-2.0 | Sí | Dependencia |
| signalsmith-stretch (crate y C++) | MIT | Sí, pero añade toolchain C++ | Oráculo de tests primero |
| assert_no_alloc | BSD-1-Clause | **No** (no está en la lista) | Hacerlo propio sobre `alloc_counter` |
| truce | Truce License 1.0 | **No** sin excepción | Decidir conscientemente |
| Ableton Link, rusty_link | GPLv2+ / propietaria | **No** | Solo con licencia comercial |
| Ardour, LMMS, Carla, Tracktion Engine, pluginval, rtcheck, DawDreamer, yabridge | GPL | **No** | Referencia / herramienta externa en CI |
| Zrythm, openDAW, Meadowlark | AGPL | **No** | Solo referencia de ideas |
| Element (Kushview) | No la verifiqué: GitHub no declara SPDX y se vende una licencia 1.x (https://kushview.net/product/element/) [S] | — | Referencia |
| Pesos: basic-pitch, CLAP2, larger_clap_music | Apache-2.0 | n/a (descarga) | Usables |
| Pesos: msclap | MS-PL | n/a | Evitar: copyleft débil al redistribuir [S] |
| Pesos: Demucs, derivados de MUSDB18, UMX-L, Spleeter | Investigación / NC / sin aclarar | n/a | Como hoy: descarga del usuario con aviso |

---

## 7. Mejoras priorizadas

Esfuerzo: S = días, M = 1-2 semanas, L = más. "Riesgo de licencia" se refiere a lo que se adopta, no al código de referencia.

| # | Qué cambiar | Dónde | Referencia / algoritmo | Esf. | Impacto | Riesgo de licencia |
|---|---|---|---|---|---|---|
| 1 | **Procesar pistas en paralelo** con contadores de dependencias y pool de hilos RT despertados por semáforo + spin corto; cada pista escribe en su buffer y el destino suma al empezar | `engine/src/graph.rs:1929-1985`, `compile.rs:479-530` (usar `succ`/`order`) | Ardour `Graph`, Tracktion `LockFreeMultiThreadedNodePlayer` (estrategia `hybrid`) | L | Alto en sesiones grandes y con sandbox (las esperas IPC dejan de sumarse) | Ninguno: implementación propia |
| 2 | **Unir los hilos de trabajo al audio workgroup** del dispositivo en macOS (`kAudioDevicePropertyIOThreadOSWorkgroup` + `os_workgroup_join`) y promover el hilo RT del sandbox | `engine/src/thread_priority.rs:5-8,37-42`, `host/src/sandbox/server.rs` | WWDC20 "Meet Audio Workgroups", Blue Cat, cubeb PR #280 | S | Evita cortes en Apple Silicon en cuanto exista #1; ya hoy en el sandbox | Ninguno (FFI propio) o MPL-2.0 con `audio_thread_priority` |
| 3 | **Streaming de disco** para clips de más de N minutos y para grabación | `engine/src/sample_cache.rs`, `record.rs:280-284` | `creek` 1.2.3 | M | Sesiones con stems largos dejan de ocupar GB de RAM; carga más rápida | Bajo (MIT/Apache; un mantenedor) |
| 4 | **Sandbox por grupos** (individual / fabricante / plugin / todos), reinicio con estado, `setProcessing` fuera del hilo RT y sidechain | `host/src/sandbox/{mod,server}.rs`, `vst3/instance.rs:886-889`, `plugin.rs:258` | Modos de Bitwig 2.5; P3/P4/P6/P7 del informe 16 | L | Menos procesos y memoria, DPF sin choques ObjC, recuperación tras crash | Ninguno |
| 5 | **Comparar el warp con Signalsmith Stretch** como oráculo (tests A/B por modo) y portar lo que gane | `dsp/src/warp.rs` | Signalsmith Stretch (MIT) | M | Alto y audible en warp de loops tonales | Bajo si solo tests; MIT si dependencia |
| 6 | **Embedding CLAP2-XS/XXS** como segundo `Embedder` (exportar a ONNX, verificar paridad) y búsqueda por texto en la librería y en las herramientas del agente | `library/src/similar.rs:59-70`, `ai/src/models.rs`, nuevo módulo en `crates/ai` | LAION CLAP2 (Apache-2.0) | M | Alto en uso: "busca un kick oscuro" funciona sin etiquetas | Bajo (pesos Apache; corpus con licencias por clip) |
| 7 | **RealtimeSanitizer en los tests del motor y del host**, incluidas las llamadas a plugins reales del arnés | `engine/src/tests/engine_safety.rs`, `host/tests/real_plugins.rs` | `rtsan-standalone` 0.3.0 | S | Detecta locks y syscalls que el contador de asignaciones no ve | Ninguno (Apache-2.0) |
| 8 | **Validación automática de plugins**: `clap-validator` en CI y como pre-chequeo opcional del escáner (fallo ⇒ sandbox por defecto); `pluginval 2 --rtcheck` solo en CI | `host/src/scan/mod.rs`, CI | clap-validator 0.4.1 (MIT), pluginval 2.0 (GPL-3, herramienta externa) | S | Menos cuelgues de usuarios; repite lo del informe 16 sin trabajo manual | Bajo (no se enlaza GPL) |
| 9 | **Servidor MCP externo** por stdio con las herramientas existentes | `crates/ai/src/llm/` y un modo o binario de la app | `rmcp` 3.11 | M | Cierra F5: cualquier agente externo opera el DAW | Ninguno (Apache-2.0); fijar versión |
| 10 | **Actualizar crates base**: rubato 0.16 → 5.x (migrar `SincFixedIn`), symphonia 0.5 → 0.6, rtrb 0.3 → 0.4 | `engine/Cargo.toml:31-32`, `ai/Cargo.toml:29-31`, `Cargo.toml:50`, `sample_cache.rs:177-256`, `ai/src/audio.rs` | Notas de versión | M | Mantenimiento; prepara creek con la Symphonia correcta | Ninguno |
| 11 | **Reloj MIDI de entrada y salida** como sustituto de Link | `engine/src/midi_io.rs:69` (dejar pasar Clock/Start/Stop/SPP) y transporte en `rt.rs` | Especificación MIDI 1.0 | S-M | Sincronía con cajas de ritmos y otros DAW | Ninguno (Link solo con licencia comercial) |
| 12 | **Alinear el sidechain con el PDC** (test con `test.latency` y corrección si falla) | `engine/src/compile.rs:554`, `graph.rs:1511-1513` | Práctica habitual de DAW [S] | S | Compresión lateral a tiempo con plugins con latencia | Ninguno |
| 13 | **Decidir framework de exportación**: `clack` en lugar de `truce` salvo excepción explícita en `deny.toml` | `PLAN.md:183`, `deny.toml` | Licencias de §3.3 | S | Desbloquea F0 sin riesgo | Medio si se elige truce |
| 14 | **Exportar HT-Demucs a ONNX nosotros** con el método de Mixxx en vez de depender de un repo de terceros | `ai/src/models.rs:164-199` | GSoC 2025 de Mixxx | M | Descarga y forma del grafo bajo nuestro control | Igual que hoy (pesos de investigación) |
| 15 | MIDI 2.0 (UMP) cuando haya demanda: parsear con `midi2` y mapear a `NoteExpression` | `engine/src/midi_io.rs`, `dsp/src/event.rs:11-36` | `midi2` 0.11.1 | M | Bajo hoy | Ninguno |

Orden sugerido: 7 y 2 primero (baratos y preparan el terreno), luego 1, 3 y 5 (motor y sonido), después 6 y 9 (IA de uso diario) y 4 cuando el sandbox pase a estar activo por defecto.

---

## 8. Capturas

Todas en `docs/research/24-investigacion/img/`, tomadas el 17-nov-2026: `04-ardour-graph-class.png`, `04-tracktion-lockfree-multithreaded-player.png`, `04-bluecat-audio-workgroups.png`, `04-creek-read-stream.png`, `04-rubato-releases.png`, `04-clack-v0-2-reentrancy.png`, `04-signalsmith-stretch.png`, `04-ableton-link-licencia.png`, `04-firewheel-readme.png`, `04-bitwig-25-modos-sandbox.png`, `04-pluginval-2-changelist.png`, `04-clap-validator-0-4-1.png`, `04-demucs-issue-327-pesos.png`, `04-laion-clap2-licencia.png`, `04-rmcp-crate.png` (15).

## 9. Fuentes

Código: las rutas citadas en cada sección (árbol de trabajo actual, 17-nov-2026).

Externas (consultadas el 17-nov-2026): las URL citadas en línea, más la API de crates.io (`https://crates.io/api/v1/crates/<nombre>`) para versión, fecha y licencia de rmcp, rubato, symphonia, clack-host, vst3, cpal, rtrb, basedrop, assert_no_alloc, audio_thread_priority, creek, firewheel, fundsp, rodio, midi2, signalsmith-stretch, ssstretch, truce-plugin, candle-core, ort, sqlite-vec, midir, rusty_link, usearch y rtsan-standalone; la API de GitHub (`https://api.github.com/repos/<repo>`) para licencia y último push de LMMS, Zrythm, Meadowlark, Signalsmith Stretch, sqlite-vec y candle; y la API de Hugging Face (`https://huggingface.co/api/models/<id>`) para la licencia de msclap, larger_clap_music y CLAP2.
