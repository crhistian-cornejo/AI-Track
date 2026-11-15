# 16 · Verificación de plugins VST3/CLAP instalados y del host (`crates/host`)

Fecha: 2026-11-15 · Máquina: Mac arm64 (macOS 27, 24 GB) · Binarios de prueba: `target/debug` (perfil `test`, optimizado + debuginfo).
Alcance: diagnóstico. No se ha modificado código fuente, ni plugins, ni git. Todo lo que sigue está marcado como
**VERIFICADO** (salida real de un comando, pegada o resumida) o **SUPUESTO** (lectura de código / inferencia).

## 0. Resumen ejecutivo

| Pregunta | Respuesta |
|---|---|
| ¿Están bien instalados? | 38 bundles (24 VST3 + 14 CLAP), todos con binario Mach-O universal (x86_64+arm64) salvo los 3 ZL (solo arm64). `codesign --verify --deep --strict` pasa en los 38. Sin atributo de cuarentena. 8 bundles DPF (Dragonfly x6, wolf-shaper x2) **no tienen `Contents/Info.plist`** (rareza de empaquetado; nuestro host los carga bien). `/Library/Audio/Plug-Ins/{VST3,CLAP}` no existen. |
| ¿Escaneo? | 38/38 OK, 0 fallos, reescaneo 100 % desde caché. 39 plugins (sfizz expone 2 clases: `sfizz` y `sfizz-multi`). |
| ¿Carga + process + parámetros + estado (en proceso)? | **39/39 OK**, VST3 y CLAP, a 48 kHz/512 y 44,1 kHz/128 y con bloques de 77 frames. |
| ¿Editor abrir/cerrar? | **39/39 OK** con la pantalla desbloqueada (ventana creada, vista adjunta, abierta 1 s, cerrada, reabierta y cerrada). Captura visual solo de 2 (Surge XT CLAP, ZL Equalizer 2). |
| ¿Sandbox (`aitrack-plugin-host`)? | **39/39 OK en una pasada limpia** (`AITRACK_TEST_ONLY_SANDBOX=1`, 39 plugins ok). Pero en otras 5 corridas (06:03–06:11) el proceso host murió de forma **intermitente y no atribuible** en 1-6 plugins distintos por corrida; ver §3.3. |
| Problema abierto más serio del arnés | El arnés **en proceso con editores** muere por SIGKILL (sin informe de crash) en 4 de 4 corridas completas con pantalla desbloqueada, siempre durante el check en proceso de Surge XT (CLAP o VST3). Cada plugin por separado, y la pasada completa sin editores, pasan. Causa no determinada; ver §3.2. |
| Problemas de host confirmados por código | Reinicios pedidos por el plugin nunca se consumen; estado de plugins sandbox no se guarda en el proyecto; sidechain y salidas múltiples no enrutados; `process::exit` en el escáner ejecuta destructores del plugin (3 SIGSEGV tras el resultado). Detalle y correcciones en §4 y §5. |

## 1. Paso 1 · Instalación de los bundles (VERIFICADO)

Comandos: `lipo -archs`, `codesign -dv --verbose=2`, `codesign --verify --deep --strict`, `xattr -l`, `PlistBuddy`, `spctl --assess`.

- Estructura: 30 bundles con `Contents/Info.plist` (`CFBundlePackageType=BNDL`) + `Contents/MacOS/<binario>`. 8 sin Info.plist:
  `DragonflyHallReverb/PlateReverb/RoomReverb` (.vst3 y .clap) y `wolf-shaper` (.vst3 y .clap). Solo tienen `Contents/MacOS` y `Contents/_CodeSignature`.
  Nuestro cargador VST3 usa `CFBundle::new` (`crates/host/src/vst3/module.rs:150-151`), que tolera la ausencia; otros hosts podrían rechazarlos.
- Arquitecturas: `x86_64 arm64` en todos menos `ZL Compressor`, `ZL Equalizer 2`, `ZL Splitter` (solo `arm64`). Los tres ZL comparten `CFBundleIdentifier=com.zlaudio.plugins` (inocuo: cargamos por ruta; los tres cargaron juntos en un proceso sin problema).
- Firma: Airwindows (VST3+CLAP), Monique, OB-Xf: Team `547U7G7MZC`, `flags=0x10000(runtime)`. chowdsp (BYOD, CHOWTapeModel, ChowCentaur, ChowKick, ChowMatrix): Team `46MK76T24B`, `flags=0x0`. Resto: ad hoc (`flags=0x2(adhoc)`, `TeamIdentifier=not set`). Todos pasan `--verify --deep --strict` (sin salida de error).
- Cuarentena: ningún `com.apple.quarantine`; solo `com.apple.provenance` (inocuo). `spctl --assess` rechaza los ad hoc (p. ej. `Dexed.vst3: rejected`): irrelevante para `dlopen` desde disco sin cuarentena, pero **relevante para la distribución**: la app firmada con runtime endurecido necesitará `com.apple.security.cs.disable-library-validation` (PLAN.md:140 y :412 lo prevén; `xtask` solo tiene `soundbank`, no hay empaquetado aún).
- `/Library/Audio/Plug-Ins/`: solo `Components`, `HAL`, `MAS`, `VST` (vacíos o drivers de audio). No existen `VST3` ni `CLAP`.
- Carpeta "disabled by AI-Track": `DragonflyEarlyReflections`, `Odin2`, `Wavetable` (.vst3 y .clap). No se tocó.
- Caché real de la app (`~/Library/Application Support/AI-Track/plugins.json`): 38 entradas, **0 bloqueados**.

## 2. Pasos 2-3 · Resultados por plugin (VERIFICADO)

Arnés: `crates/host/tests/real_plugins.rs` (`harness = false`). Datos de las corridas con pantalla desbloqueada (ver §3.1 para el origen de cada fila).
"Pico" = pico de salida a 48 kHz/512 (efecto: seno 0,5; instrumento: acorde C-E-G). "Parámetros" = cargados por la instancia real (el escaneo desde `moduleinfo.json` da `0`, ver §5.1).
"Editor" = abre, sigue abierto 1 s, cierra, reabre y cierra. "Estado" = `save_state` + `load_state` sin error (**no** compara valores).

Leyenda sandbox: OK = pasó en la pasada limpia (39/39); `‡` = además falló en alguna otra corrida con muerte del proceso host (§3.3).

| Plugin | Fmt | Escaneo | Carga | Pico 48k/512 | Params | Lat. | Editor | Estado (bytes) | Sandbox | Veredicto |
|---|---|---|---|---|---|---|---|---|---|---|
| Airwindows Consolidated | CLAP | probe | 3,6 ms | 0,3974 | 14 | 0 | OK | 329 | OK ‡ | OK |
| Airwindows Consolidated | VST3 | moduleinfo | 26 ms | 0,3043 | 14 | 0 | OK | 376 | OK | OK |
| BYOD | CLAP | probe | 7,5 ms | 0,5000 | 509 | 4 | OK | 1361 | OK | OK |
| BYOD | VST3 | probe | 7,9 ms | 0,5000 | 511 | 4 | OK | 1437 | OK | OK |
| CHOWTapeModel | CLAP | probe | 7,3 ms | 0,4311 | 52 | 39/37 | OK | 2888 | OK ‡ | OK |
| CHOWTapeModel | VST3 | probe | 29 ms | 0,4311 | 54 | 39/37 | OK | 2964 | OK | OK |
| ChowCentaur | VST3 | probe | 28 ms | 1,0624 | 7 | 0 | OK | 319 | OK | OK (pico > 1: ganancia del plugin) |
| ChowKick | CLAP | probe | 4,5 ms | 1,1364 | 21 | 0 | OK | 1634 | OK ‡ | OK (pico > 1) |
| ChowKick | VST3 | probe | 7,4 ms | 1,1364 | 23 | 0 | OK | 1710 | OK | OK (pico > 1) |
| ChowMatrix | VST3 | probe | 76 ms | 0,3856 | 667 | 0 | OK | 3393 | OK | OK |
| CloudReverb | VST3 | moduleinfo | 13 ms | 0,5784 | 47 | 0 | OK | 2317 | OK | OK |
| Dexed | CLAP | probe | 8,3 ms | 0,3517 | 182 | 0 | OK | 7105 | OK | OK |
| Dexed | VST3 | moduleinfo | 10,6 ms | 0,3517 | 184 | 0 | OK | 7181 | OK | OK |
| Dragonfly Hall Reverb | CLAP | probe | 3,1 ms | 0,5053 | 18 | 0 | OK | 353 | OK ‡ | OK (sin versión en el descriptor CLAP) |
| Dragonfly Hall Reverb | VST3 | probe | 5,9 ms | 0,5053 | 18 | 0 | OK | 369 | OK | OK |
| Dragonfly Plate Reverb | CLAP | probe | 9,6 ms | 0,5007 | 9 | 0 | OK | 232 | OK ‡ | OK |
| Dragonfly Plate Reverb | VST3 | probe | 7,7 ms | 0,5007 | 9 | 0 | OK | 248 | OK | OK |
| Dragonfly Room Reverb | CLAP | probe | 6,3 ms | 0,4829 | 17 | 0 | OK | 350 | OK | OK |
| Dragonfly Room Reverb | VST3 | probe | 5,5 ms | 0,4829 | 17 | 0 | OK | 366 | OK | OK |
| FILT-R | VST3 | moduleinfo | 10 ms | 0,4998 | 57 | 49 | OK | 3225 | OK | OK |
| GATE-12 | VST3 | moduleinfo | 8,8 ms | 0,2985 | 30 | 0 | OK | 2028 | OK | OK (en sandbox pico 0,0053: puerta rítmica) |
| JC303 | CLAP | probe | 11 ms | 0,2263 | 40 | 0 | OK | 2820 | OK | OK |
| JC303 | VST3 | moduleinfo | 15,6 ms | 0,2263 | 41 | 0 | OK | 2896 | OK | OK |
| Monique | CLAP | probe | 24 ms | **0,0124** (0,5768 a 44,1k/128) | 352 | 0 | OK | 44 777 | OK ‡ | OK con nota (§4, P8) |
| Monique | VST3 | moduleinfo | 40 ms | **0,0124** (0,5768) | 354 | 0 | OK | 44 853 | OK ‡ | OK con nota |
| OB-Xf | CLAP | probe | 14,9 ms | 0,2838 | 102 | 0 | OK | 2323 | OK | OK |
| OB-Xf | VST3 | probe | 21,8 ms | 0,2746 | 103 | 0 | OK | 2399 | OK | OK |
| REEV-R | VST3 | moduleinfo | 14,6 ms | 0,5605 | 107 | 0 | OK | 5695 | OK | OK |
| Surge XT | CLAP | probe | 93,6 ms | 0,5622 | 775 | 0 | OK (+captura) | 50 339 | OK | OK |
| Surge XT | VST3 | probe | 54 ms | 0,4722 | 775 | 0 | OK | 50 386 | OK ‡ | OK |
| Surge XT Effects | CLAP | probe | 18 ms | 0,5182 | 13 | 0 | OK | 1092 | OK | OK |
| Surge XT Effects | VST3 | probe | 31 ms | 0,5182 | 14 | 0 | OK | 1168 | OK | OK |
| wolf-shaper (Wolf Shaper) | CLAP | probe | 2,7 ms | 0,5000 | 11 | 0 | OK | 268 | OK | OK |
| wolf-shaper (Wolf Shaper) | VST3 | probe | 6,3 ms | 0,5000 | 11 | 0 | OK | 284 | OK | OK |
| ZL Compressor | VST3 | moduleinfo | 7,5 ms | 0,4730 | 83 | 0 | OK | 3992 | OK | OK (solo arm64) |
| ZL Equalizer 2 | VST3 | moduleinfo | 7,6 ms | 0,5000 | 610 | 0 | OK (+captura) | 25 588 | OK | OK (solo arm64) |
| ZL Splitter | VST3 | moduleinfo | 6,8 ms | 0,5000 | 16 | 0 | OK | 1269 | OK | OK (solo arm64) |
| sfizz | VST3 | probe | 29 ms | 1,1835 | 526 | 0 | OK | 106 | OK ‡ | OK (sin SFZ cargado suena un seno por defecto) |
| sfizz-multi | VST3 | probe | 2,9 ms | 1,1835 | 526 | 0 | OK | 106 | OK | OK |

Cobertura de cada columna (honesta):

- **Escaneo**: `scan: 38 files, 27 scanned, 0 failed in 1.038386167s` / `rescan: 38 cached, 0 scanned in 1.25625ms` (14 CLAP + 13 VST3 por proceso `aitrack-plugin-scanner`; 11 VST3 con `moduleinfo.json`, sin cargar el binario).
- **En proceso**: 39/39 OK repartido en tres corridas con editores (la pasada única con editores no llega al final, §3.2): `full.log` (Airwindows … REEV-R, 25 filas), `full2.log` (Surge XT CLAP) y una corrida dedicada a Surge XT VST3 … sfizz-multi (10 filas, `real_plugins: 10 plugins ok`). Además **una pasada única sin editores** (`AITRACK_TEST_NO_EDITOR=1 AITRACK_TEST_NO_SANDBOX=1`): `real_plugins: 39 plugins ok`, 3,27 s, footprint 567 MB.
- **Sandbox**: una pasada única limpia con editores: `real_plugins: 39 plugins ok` (39 líneas `sandbox: peak …`, 0 `FAIL`).
- **Process "sin NaN"**: **NO verificable con este arnés**. `HostedPlugin::process` sustituye NaN/inf por 0 antes de que el arnés mida el pico (`crates/host/src/plugin.rs:359-362`, `crates/host/src/io.rs:90-100`). Lo único que se ha verificado es que el pico es finito y > 1e-4 (el arnés marca "silent output" por debajo).
- **Estado**: solo se comprueba que `load_state` no devuelve error; no se comparan los valores de parámetros tras recargar.
- **Editor**: el arnés comprueba que la ventana sigue visible tras 1 s y que se cierra. No comprueba dibujo, redimensionado ni foco. Se tomaron dos capturas de ventana (`screencapture -l`): Surge XT CLAP dibuja la GUI completa; ZL Equalizer 2 dibuja rejilla y barra superior (EQ sin bandas, estado por defecto).
- **Pantalla bloqueada**: al empezar el trabajo y en las corridas citadas la pantalla estaba **desbloqueada** (no aparece `screen locked, editors NOT checked`). Más tarde se bloqueó; las corridas posteriores con ese aviso (`inproc_s2_*`, `inproc_ed2`) **no cuentan** como verificación de editores y no se han usado en la tabla.

Observaciones menores de las salidas (VERIFICADO):

- Contadores VST3 vs CLAP: el VST3 lista 1-2 parámetros más (p. ej. Dexed 184/182, BYOD 511/509, Monique 354/352, ChowKick 23/21): los de bypass/programa que el wrapper VST3 añade.
- Surge XT CLAP: texto de valor para 0,65 distinto del VST3 (`0.670410` frente a `0.708524`) en el parámetro `M1: -`: mapeo plano↔normalizado distinto entre formatos (el host CLAP convierte lineal `min..max`, `crates/host/src/clap/instance.rs:116-123`), no un fallo.
- ZL Equalizer 2 expone `Bypass=0.000 "On"` como parámetro normal (semántica invertida del propio plugin).
- Líneas `objc: Class DGL_Pugl… is implemented in both …` entre **todas** las variantes Dragonfly (Hall/Plate/Room, CLAP y VST3): clases ObjC de DPF sin espacio de nombres. Ver P6.

## 3. Fallos y anomalías con triage

### 3.1 Cronología de las corridas (VERIFICADO)

| Corrida | Condiciones | Resultado |
|---|---|---|
| A (`full.log`) | completa, 5 min tras empezar; otros agentes compilando y ejecutando la app | en proceso OK hasta REEV-R; 3 `FAIL` de sandbox (CHOWTapeModel CLAP `open_editor: sandbox process exited`, Dragonfly Hall CLAP y Monique VST3 `close_editor: Broken pipe`); proceso del arnés **SIGKILL** en Surge XT CLAP (`signal: 9, SIGKILL`, exit 137) |
| B (`sandbox.log`) | `ONLY_SANDBOX`, completa | `real_plugins: 11 failure(s)`: 5 plugins con `close_editor: Broken pipe` + `process died` (Airwindows CLAP, ChowKick CLAP, Dragonfly Plate CLAP, Monique CLAP, sfizz VST3) y 1 `sandbox spawn: sandbox process exited` (Surge XT VST3) |
| C | `ONLY_SANDBOX`, 4 corridas x 5 plugins | 1 plugin distinto muere en cada una (Airwindows CLAP/open, Dragonfly Plate/close, Monique CLAP/close, sfizz/close) |
| D | mismas 5 plugins, 6 corridas (sin editor / editor 0,1 s / editor 1 s con sandbox quieto / con 12 procesos `yes` de carga) | **0 fallos en 12 corridas** |
| E | `ONLY_SANDBOX`, completa, sistema quieto (`sandbox2.log`) | `real_plugins: 39 plugins ok` |
| F | en proceso, completa, con editores, 3 corridas (+1 bajo `lldb`) | **SIGKILL** en Surge XT en las 3 (la bajo lldb no llegó por lentitud) |
| G | en proceso, completa, con editores, 1 corrida más | SIGKILL en Surge XT VST3 de nuevo (4.ª vez); la siguiente dio 39 ok pero **con pantalla bloqueada** (editores omitidos: no cuenta) |

### 3.2 SIGKILL del arnés en proceso con editores (NO RESUELTO)

Salida clave (corrida F, con `/usr/bin/time -l`):

```
== Surge XT [Vst3] 1.3.4 by Surge Synth Team — Instrument|Synth (2 in / 2 out, 775 params)
time: command terminated abnormally
36.18 real  ...  1762050048 maximum resident set size
1  signals received
2442218280  peak memory footprint
```

Hechos:

- Sin editores: 39/39 OK, 3,27 s, 567 MB de footprint. Con editores: ~36 s y 2,4 GB de footprint en el momento de morir.
- Cada Surge XT/Effects por separado (con y sin sandbox), CLAP+VST3 juntos y los cuatro Surge juntos pasan (8/8, 3/3, 2/2). Los 10 últimos plugins del orden pasan juntos con editores.
- Sin informe de crash (`~/Library/Logs/DiagnosticReports`: ninguno para `real_plugins` ni `aitrack-plugin-host`): es una muerte por señal 9 externa al proceso.
- No es un límite de memoria del entorno: un proceso Python tocó 10 GB sin problema. `ulimit -a` sin límites; 52 % de memoria libre.
- El log de WindowServer al cerrar la conexión: `ownedWindowCount=59 ownedCAContextCount=71`. Tras ~24 plugins el proceso aún poseía 59 ventanas: apunta a **ventanas de editor que no se destruyen** (SUPUESTO: puede ser de los plugins o de `editor/macos.rs`, `Window::drop` hace `orderOut` + `close` y suelta el `Retained<NSWindow>`, `crates/host/src/editor/macos.rs:72-77`).
- Segundos antes de una muerte hay una ráfaga de `sh`/`bash` lanzados por otro proceso (log del kernel 06:17:54.7, kill 06:17:55.16): no demuestra nada, pero **no se puede descartar una muerte provocada por otro agente** que trabaja en el mismo checkout. En contra: ocurre siempre en Surge XT.

Hipótesis (ninguna confirmada): (a) acumulación de recursos de editores (ventanas/contextos GL/IOSurface) en un solo proceso hasta un presupuesto del WindowServer o el kernel; (b) interferencia externa. Implicación para la app (SUPUESTO): abrir y cerrar muchos editores en una sesión larga podría acumular ventanas; conviene medir el número de `NSWindow` del proceso tras N aperturas (con la pantalla desbloqueada) antes de descartarlo. Severidad: **Media (incertidumbre alta)**; esfuerzo de investigación **M**.

### 3.3 Muerte intermitente del proceso `aitrack-plugin-host` en sandbox (NO ATRIBUIDA)

- Plugin-agnóstico: en las corridas B y C falla un plugin distinto cada vez, siempre CLAP o VST3 que en otra corrida pasan; **todos** los 39 pasaron en la pasada limpia E.
- Reproducida 5/5 en la ventana de tiempo 06:07-06:11 y **0/12 después** con el mismo binario (incluso con 12 procesos `yes` de carga y con editor de 0,1 s o 1 s).
- Conducido a mano el host (script Python que envía `load`/`open_editor`/`close_editor`/`shutdown` por stdin): ChowKick CLAP 8/8 sin fallo, salida 0.
- Solo ocurre con el editor abierto 1 s (con 0,1 s, 0 fallos en 3 corridas): el host muere después de `open_editor` o durante `close_editor`, y el padre ve `Broken pipe` / `sandbox process exited`.
- Candidatos: (a) muerte externa (misma ventana de tiempo que el SIGKILL de §3.2); (b) falso positivo del watchdog (`HANG_TIMEOUT` = 5 s, `crates/host/src/sandbox/mod.rs:74-76`, `kill_pid` con `SIGKILL` en `:132-139`); no se pudo distinguir porque el arnés no imprime `is_hung()` y no hay logger. **Recomendación (S)**: imprimir `SandboxEditor::is_hung()` y la señal de salida del hijo (`Child::try_wait().status.signal()`) en el `FAIL` del arnés y en `Control::call` (`sandbox/mod.rs:180-210`) para poder atribuir la próxima muerte.

### 3.4 Crash del escáner en ChowMatrix (VERIFICADO, causa raíz identificada)

Tres informes `aitrack-plugin-scanner-2026-11-15-060920.ips`, `…061114.ips`, `…061117.ips`: `EXC_BAD_ACCESS / SIGSEGV`, hilo de `libdispatch` worker: `_Block_release` → `_call_dispose_helpers_excp` → `objc_msgSend` → `realizeClassWithoutSwift`; imagen del plugin cargada: `ChowMatrix.vst3`. El proceso ya había impreso su resultado; muere al salir.
Causa raíz en nuestro host: `scanner_main` termina con `std::process::exit(0)` (`crates/host/src/scan/mod.rs:164`, y `crates/host/src/bin/aitrack-plugin-scanner.rs:11`), pero el comentario de la línea 163 dice "Skip plugin destructors/atexit handlers": **`process::exit` de Rust llama a `exit()` de libc**, que **sí** ejecuta `atexit` y destructores estáticos de C++ del plugin mientras hilos de `dispatch` del plugin siguen vivos. Reproducción posterior: 0 fallos en 75 ejecuciones directas del escáner (ChowMatrix, CHOWTapeModel, Dexed x25): es dependiente de la carga/temporización.
Efecto: el resultado ya está escrito (el padre lo lee, `scan/mod.rs:128-134` no mira el estado de salida), así que no se blocklistea, pero genera informes de crash y, en Windows, un cuadro de WER si el crash ocurriese antes. Corrección (S): `libc::_exit(0)` tras `flush` (unix) / `TerminateProcess(GetCurrentProcess(), 0)` (windows). Lo mismo para el host del sandbox (`bin/aitrack-plugin-host.rs:6`, SUPUESTO).

## 4. Hallazgos de conformidad y de integración (triage con causa raíz)

Severidad: A = alta, M = media, B = baja. Esfuerzo: S (< 1 día), M (1-3 días), L (> 3 días). "V" = verificado con comando; "S" = supuesto por lectura de código.

| # | Hallazgo | Evidencia | Sev. | Esf. | Corrección propuesta |
|---|---|---|---|---|---|
| P1 | Los reinicios pedidos por el plugin **no se consumen en la app**. `needs_restart`, `take_latency_changed`, `take_state_dirty` solo se definen en el host (`plugin.rs:291,469,474,478`); `grep` en `crates/app` y `crates/engine` no encuentra ningún uso (solo `take_hung`). El README dice que la UI debe hacerlo. Afecta a VST3 `kReloadComponent`/`kIoChanged`/`kLatencyChanged` (`vst3/instance.rs:526-534`), CLAP `request_restart`, `latency.changed`, `audio_ports.rescan` (`clap/mod.rs:77-79,167-175`, `clap/instance.rs:345-352`). El PDC sí se actualiza porque el motor relee `latency()` tras cada bloque (`engine/src/graph.rs:169`), pero el plugin no se reactiva como exige la especificación. | V | M | M | En `PluginRuntime::idle()` (`app/src/real/plugins.rs:~196`) recorrer controladores, y si `take_needs_restart()` llamar al camino de re-prepare del motor en el hilo principal. En sandbox, reenviar el flag en la cabecera SHM. |
| P2 | **El estado de plugins en sandbox no se guarda en el proyecto**. `capture_states` solo itera `instances.controllers` (`app/src/real/plugins.rs:251-263`); los sandbox están en `sandboxed` (solo `SandboxEditor`: abrir/cerrar). `Processor::save_state` solo se llama en tests del motor. Con la opción de sandbox activa, guardar el proyecto pierde presets/samples del plugin. | S (código) | A (si se usa sandbox) | M | Añadir `save_state` a `SandboxEditor` (Request `SaveState` ya existe, `sandbox/protocol.rs:45`) y recogerlo en `capture_states`. |
| P3 | **Sandbox desactivado por defecto** (`app/src/real/mod.rs:39`, `sandbox: false`). Sin él, un crash de plugin tira la app y un cuelgue de editor congela la UI (caso Dragonfly Early Reflections). Además, el host del sandbox es un proceso por plugin, **sin reinicio tras crash ni sidechain** (`sandbox/mod.rs:9-12`, `server.rs:279` pasa `sidechain: None`). | V | M | M | Activar por defecto para plugins sin historial limpio o para los que figuren en una lista (DPF/GL); implementar reinicio con recuperación de estado. |
| P4 | `has_sidechain: false` **siempre** (`plugin.rs:242`, `sandbox/mod.rs:324`) aunque el audio ya llena el bus 1 desde `ctx.sidechain` (`vst3/instance.rs:751`, `clap/instance.rs:542`). Compresores con entrada lateral (ZL Compressor/ZL Equalizer 2 `External Side`) no pueden recibir sidechain. | V | M | M | Detectar buses auxiliares de entrada (`BusInfo.busType == kAux` / CLAP `port_type == "sidechain"`), poner `has_sidechain` y enrutar. |
| P5 | Solo se usa **la salida principal** (bus 0): `write_to(0, …)` en `vst3/instance.rs:816` y `clap/instance.rs:563`. Instrumentos multisalida (Surge XT escenas, `sfizz-multi`) pierden las demás. `sfizz-multi` aparece además como "plugin" aparte en el navegador. | V | B | L | Exponer salidas auxiliares como pistas/buses; mientras tanto, ocultar o etiquetar las variantes multi. |
| P6 | **Choque de clases ObjC entre plugins DPF cargados en el mismo proceso**: `Class DGL_PuglWindow is implemented in both …DragonflyHallReverb.clap … and …DragonflyPlateReverb.clap` (y Room, y VST3). El aviso del runtime dice "spurious casting failures and mysterious crashes". Hall+Plate+Room son plugins distintos que un usuario cargará juntos. wolf-shaper sí usa prefijo (`WolfShaperDGL_…`), pero CLAP y VST3 chocan entre sí. No es culpa de nuestro host; es un riesgo real de la app. | V | M | S | Ejecutar plugins DPF (detectables por el clash o lista) siempre en sandbox (P3). |
| P7 | `setProcessing(1)` se llama **dentro de `process` en el hilo de audio** (`vst3/instance.rs:809-811`) y `reset` conmuta 0/1 (`:644-647`). Contradice nuestra norma de "nada de asignaciones" si el plugin reserva memoria en `setProcessing` (muchos JUCE lo hacen). | S | B | S | Llamar `setProcessing(1)` desde el hilo principal al final de `prepare` (la especificación no exige el hilo de audio). |
| P8 | Monique: el primer `prepare` (48 kHz/512) da pico 0,0124 (~-38 dBFS) con el mismo acorde que da 0,5768 en el segundo (44,1 kHz/128). Mismo valor en sandbox. No es "silencio", pero sugiere que el plugin necesita calentamiento tras su primera activación o tiene condición de carrera (OB-Xf imprime `Supressing default patch until we sort out race condition`). | V | B | S | Plugin-side; vigilar en la UI. Candidato a prueba de humo propia (nota de 1 s, medir RMS). |
| P9 | Escáner: `process::exit(0)` ejecuta destructores del plugin (3 SIGSEGV tras el resultado, ChowMatrix). Ver §3.4. | V | B-M | S | `libc::_exit(0)`. |
| P10 | CLAP: `request_flush` y `request_process` solo ponen flags (`clap/mod.rs:81,124`) que **nadie lee** (`grep` sin consumidores). Con el motor parado o el plugin dormido, los cambios de parámetros desde la GUI no se vacían (`params.flush`) y `request_process` no tiene efecto. `params.rescan` solo refresca valores (`clap/instance.rs:356-358`), no relee la lista/nombres (flags `INFO/TEXT/ALL` ignoradas; `clear` es no-op `clap/mod.rs:152`). | V | B | S-M | Llamar `PluginParams::flush` desde el hilo principal cuando no hay procesamiento; reconstruir la `ParamTable` en `rescan` con `ALL|INFO`. |
| P11 | CLAP: `ControlChange`, `PitchBend` y `ChannelPressure` siempre se envían como evento MIDI (`clap/instance.rs:516-528`); solo las notas respetan `midi_notes`. Un puerto de notas solo-CLAP ignora CC/pitch bend. | S | B | S | Traducir a `ParamValue`/`NoteExpression` cuando el puerto no admite MIDI. |
| P12 | VST3: `restartComponent` solo trata `kParamValuesChanged`, `kLatencyChanged`, `kReloadComponent|kIoChanged` (`vst3/instance.rs:516-534`). Faltan `kParamTitlesChanged` (nombres/pasos de parámetros), `kMidiCCAssignmentChanged` (el `midi_map` se construye solo en la carga, `:183-192`), `kNoteExpressionChanged`, `kIoTitlesChanged`, `kRoutingInfoChanged`. | V | B | M | Recargar parámetros y `midi_map` en `idle` según flags. |
| P13 | VST3: no hay `IUnitInfo`/listas de programas (ni `IProgramListData`), ni `IComponentHandler3` (menú contextual), `IUnitHandler`, `IComponentHandlerBusActivation`, `IPlugInterfaceSupport`, `IProcessContextRequirements`, `INoteExpressionController`/`IKeyswitchController`. `ComponentHandler` solo expone `IComponentHandler`+`IComponentHandler2` (`vst3/com.rs:95-97`); `HostApp` solo `IHostApplication` (`:25-27`); `setProcessing`/`canProcessSampleSize` no se consultan (`kSample32` fijo). Las listas de programas de Dexed/Surge no se ven. `ProcessContext` no rellena `continousTimeSamples`, `systemTime`, `samplesToNextClock`, `smpteOffset` (no marca sus flags: legal). | V | B | M-L | Priorizar `IPlugInterfaceSupport` (S) y `IUnitInfo` para programas (M). |
| P14 | Eventos: salida de eventos (`out_events`) descartada en VST3 y CLAP (solo se leen `ParamValue`/gestos CLAP, `clap/instance.rs:566-584`); no hay MIDI/notas de salida (arpegiadores, MIDI FX). Eventos VST3 con `busIndex: 0` y polypressure con `channel: 0` fijos (`vst3/instance.rs:701`, `make_event`). | V | B | M | Enrutar notas de salida a pistas MIDI cuando exista el destino. |
| P15 | Extensiones CLAP del host **no ofrecidas**: `thread-check`, `posix-fd-support`, `context-menu`, `track-info`, `preset-load`, `remote-controls` (host), `note-name`, `voice-info`, `audio-ports-config`, `state-context`, `param-indication`, `thread-pool`. Del plugin **no usadas**: `remote-controls`, `note-name`, `voice-info`, `audio-ports-activation`, `configurable-audio-ports`, `preset-discovery`, `render`. Ofrecidas (`clap/mod.rs:58-69`): `log`, `params`, `state`, `latency`, `gui`, `audio-ports`, `note-ports`, `timer-support`, `tail`. `gui.request_show/request_hide/resize_hints_changed` son no-op (`clap/mod.rs:101-115`). | V | B | S-L | `thread-check` es lo más barato (S) y evita avisos/fallbacks en JUCE/Surge; `posix-fd-support` solo importa fuera de macOS/Windows. |
| P16 | Escaneo/caché: clave `mtime+size` del binario (`scan/cache.rs:28-39`); no incluye `moduleinfo.json` ni versión del host/formato. El binario se localiza por `file_stem` del bundle (`info.rs:58-93`), no por `CFBundleExecutable`: bundles cuyo ejecutable difiera del nombre (no es el caso hoy) caen a `metadata(path)` del directorio. Un `moduleinfo.json` da `params: 0`, `inputs: 2` supuesto y `probed: false` (`scan/moduleinfo.rs:65-78`). | V | B | S | Leer `CFBundleExecutable`; añadir hash de `moduleinfo.json` a la clave. |
| P17 | UI: un fallo/bloqueo de escaneo se reduce a "N failed or blocked" (`app/src/real/plugins.rs:632-636`); no hay lista ni forma de `unblock` en la app (`grep unblock/blocklist` sin resultados). Un plugin bloqueado desaparece sin explicación. | V | M | S | Mostrar la lista de bloqueados con motivo y botón "reintentar" (`Scanner::unblock`). |
| P18 | Windows: solo se verificó el tipado (`cargo check -p aitrack-host --all-targets --target x86_64-pc-windows-msvc` → `Finished dev profile … in 3.11s`, exit 0, 0 warnings). Rutas por defecto correctas (`CommonProgramFiles\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3`/CLAP, `CLAP_PATH`: `scan/paths.rs:15-39`), `InitDll/ExitDll`, DPI y `LOAD_WITH_ALTERED_SEARCH_PATH` (`vst3/module.rs:84-130`). No ejecutable aquí. | V (tipado) | - | - | Probar con plugins reales en una máquina Windows. |

## 5. Paso 4 · Revisión de conformidad (resumen por área)

### 5.1 VST3 (S: lectura de `crates/host/src/vst3/`)

Correcto: componente y controlador separados o únicos (`instance.rs:130-148`), `IConnectionPoint` en ambos sentidos (`:150-160`, desconexión en `Drop`), `setComponentHandler` antes de sincronizar estado (`:170-182`), `setComponentState` tras `getState`, buses (principal estéreo forzado con `setBusArrangements`, auxiliares con su layout, todos activados `:317-330`), latencia y cola leídas en `prepare` y en `kLatencyChanged`, `IMidiMapping` para CC/pitch/aftertouch, filtrado de proxies MIDI de JUCE (`listed`, `:264-270`), expresión de nota (tuning/pan/brightness/volume/pressure `:699-722`), `IPlugView` con `IPlugFrame::resizeView` + `onSize` (`com.rs:163-180`), `checkSizeConstraint`/`canResize` (`instance.rs:547-570`), `IBStream` en memoria (read/write/seek/tell), `IHostApplication::createInstance` para `IMessage`/`IAttributeList` (comunicación componente-controlador), contenedor de estado `AIV3` (componente+controlador, `:125-138`).
Huecos: P7, P12, P13, P14; el mono/estéreo del bus se fuerza a estéreo (si el plugin rechaza, usa su layout, `:317-319`); `IAudioProcessor::setProcessing` en el hilo de audio; no hay `IProcessContextRequirements`.
Cobertura: el arnés real no sabe qué plugins del Mac usan controlador separado; el camino dividido está cubierto solo por el fixture `test-gain` (VERIFICADO: `cargo test -p aitrack-host` pasa 44 tests: 26 lib + 9 `hosting` + 5 `sandbox` + 4 `scan`).

### 5.2 CLAP (S: lectura de `crates/host/src/clap/`)

Correcto: `params` (cookies devueltos en cada evento: necesario para plugins JUCE/clap-juce-extensions), `state`, `latency`, `tail`, `audio-ports`, `note-ports` (elige dialecto CLAP o MIDI, `:141-144`), `gui` embebido (`COCOA`/`WIN32`) con respaldo flotante, `timer-support` (mínimo 10 ms), `log`; transporte completo (`make_transport`), eventos con cookies, flags de edición GUI→DAW (`ParamValue`, gestos begin/end).
Huecos: P1, P10, P11, P15.

### 5.3 Sandbox y watchdog (V/S)

Existe y funciona: SHM + semáforos con entrega en el mismo ciclo; latencia, parámetros en ambos sentidos, estado y editor; watchdog por latido de 250 ms con corte a 5 s (`HANG_TIMEOUT`) y `Freeze` de prueba (`tests/sandbox.rs`: 5 tests OK). Faltan: P2, P3, P4, §3.3 (atribución de muertes), reinicio con recuperación, varios plugins por proceso, MMCSS/workgroup.

### 5.4 Caché y blocklist (V)

Funcionan: caché por `mtime+size`, reescaneo 100 % en caché (0,8-1,7 ms), `moduleinfo.json` sin cargar binario, blocklist por clave, guardado atómico (`write` + `rename`, `scan/cache.rs:107-115`), poda de ficheros desaparecidos. Huecos: P16, P17; el escáner paraleliza hasta 4 procesos con tiempo límite 20 s (`scan/mod.rs:39`).

## 6. Paso 5 · Cómo llega a la UI (V/S)

- **¿El navegador lista todos?** Sí. La caché de la app tiene 38 entradas (0 bloqueadas) y `RealCatalog::external` las expone todas (`app/src/real/plugins.rs:540-556`): 39 filas (14 plugins x 2 formatos + 10 solo VST3 + `sfizz-multi`).
- **¿Diferencia instrumentos/efectos?** Sí: `is_instrument` sale de la subcategoría VST3 `Instrument` o la característica CLAP `instrument` (además, un CLAP con puerto de notas y sin entradas de audio se marca instrumento, `clap/instance.rs:135-140`); el menú los separa en "VST / CLAP Synths" y grupos de efectos (`views/menus.rs:482,553-570`, `views/palette/devices.rs:327-365`).
- **Clasificación inconsistente entre formatos del mismo plugin** (S, por la lógica de `classify_external`): ChowKick CLAP (`instrument|drum`) cae en Drums y su VST3 (`Instrument|Synth`) en Multi; Airwindows CLAP (`…|filter|…`) en Filter y su VST3 (`Fx`) en Stereo/Utility; Surge XT Effects CLAP en Filter y VST3 en Stereo/Utility. Además hay coincidencias por subcadena en `KNOWN_OVERRIDES` (`"eq"`, `"xo"`, `"labs"`, `"span"`, `"ott"`) con riesgo de falsos positivos.
- **Duplicados VST3+CLAP**: no se fusionan. Aparecen como dos filas con insignia `VST3`/`CLAP` (`views/menus.rs:734-736,759`), favoritos y recientes por clave `ext:<formato>:<uid>` (`devices.rs:201-207`), así que el mismo plugin ocupa dos entradas y dos favoritos. En el menú se ordenan por grupo y nombre, las dos filas quedan contiguas. No hay preferencia de formato.
- **Escaneo**: se lanza al abrir en segundo plano con progreso; los fallos/bloqueos se resumen en un contador (P17). Estado de plugins de sandbox: P2.

## 7. Qué NO se pudo verificar

- Dibujo correcto, redimensionado, foco y entrada de ratón/teclado de los editores (solo 2 capturas).
- Que `load_state` restaure **valores** (solo que no falla) y la ausencia de NaN antes del saneado del host.
- Sandbox con editores en una sola corrida **continua** durante periodos con otros agentes activos (ver §3.3) y la causa del SIGKILL de §3.2.
- Ejecución real en Windows (solo `cargo check`).
- Plugins sin Info.plist (Dragonfly, wolf-shaper) en otros hosts.
- Qué plugins usan controlador separado (camino `IConnectionPoint` real).
- Comportamiento en Intel (los 3 ZL no son universales).

## 8. Comandos y salidas clave (para reproducir)

```text
$ AITRACK_TEST_PLUGINS="$HOME/Library/Audio/Plug-Ins/VST3:$HOME/Library/Audio/Plug-Ins/CLAP" \
    cargo test -p aitrack-host --test real_plugins -- --nocapture
real_plugins: 38 plugin files
scan: 38 files, 27 scanned, 0 failed in 1.038386167s
rescan: 38 cached, 0 scanned in 1.25625ms
...
   sandbox: peak 0.3974, 0 late blocks, state 314 bytes
   sandbox editor opened and closed
...
error: test failed ... (signal: 9, SIGKILL: kill)            # corrida A, en Surge XT CLAP

$ AITRACK_TEST_ONLY_SANDBOX=1 ... (corrida B, sistema ocupado)
   FAIL sandbox close_editor: sandbox error: sandbox pipe closed: Broken pipe (os error 32)
   FAIL sandbox process died
real_plugins: 11 failure(s)

$ AITRACK_TEST_ONLY_SANDBOX=1 ... (corrida E, sistema quieto)
real_plugins: 39 plugins ok

$ AITRACK_TEST_NO_EDITOR=1 AITRACK_TEST_NO_SANDBOX=1 ...
real_plugins: 39 plugins ok                                    # 3,27 s, footprint 567 MB

$ cargo test -p aitrack-host --lib --test hosting --test scan --test sandbox --test system_plugins
test result: ok. 26 passed (lib) / 9 passed (hosting) / 5 passed (sandbox) / 4 passed (scan)

$ cargo check -p aitrack-host --all-targets --target x86_64-pc-windows-msvc
    Finished `dev` profile [optimized + debuginfo] target(s) in 3.11s     # exit 0
```

Archivos citados: `crates/host/src/{plugin.rs, io.rs, info.rs}`, `crates/host/src/scan/{mod.rs, cache.rs, moduleinfo.rs, paths.rs}`, `crates/host/src/vst3/{instance.rs, com.rs, module.rs, mod.rs}`, `crates/host/src/clap/{mod.rs, instance.rs}`, `crates/host/src/sandbox/{mod.rs, server.rs, protocol.rs}`, `crates/host/src/editor/macos.rs`, `crates/host/tests/real_plugins.rs`, `crates/app/src/real/{plugins.rs, mod.rs, engine.rs}`, `crates/app/src/views/{menus.rs, palette/devices.rs, palette/mod.rs}`, `crates/engine/src/graph.rs`.
