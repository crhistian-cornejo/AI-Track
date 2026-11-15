# Pendiente

Estado actualizado de lo que queda por hacer frente a `PLAN.md` y los READMEs de cada crate. Las reglas de `CLAUDE.md` siguen valiendo: toda edición es un `Command`, nada de asignaciones en el hilo de audio, colores de `theme.rs`, tests junto al código, nada de dependencias GPL, y **nunca `cargo fmt`** (el código está formateado a mano).

## Completado recientemente (2026-11-12 – 2026-11-14)

- **Paleta Cmd+K y menú contextual:** agrupación en Instrumentos y Efectos con subgrupos (incl. `fx.drum_extract` en Analysis), chips compactos en una sola fila con `+N` desplegable, badges de fabricante/formato, favoritos/recientes y "Show automation" con clic derecho en knobs/faders/sends.
- **Suite propia de plugins:** Acid 303, Kick 808, Sampler SFZ v2, Shaper multicurva, **Drum Extractor (`fx.drum_extract`)**, presupuestos de CPU en tiempo real por plugin (`cpu_budgets_stay_realtime_capable`), variaciones de macros en Rack (`Save` / recall / delete), e índice bibliográfico unificado en [12-dsp-plugin-suite.md](file:///Users/corx/Developer/AI-Track/docs/research/12-dsp-plugin-suite.md).
- **Extractor limpio de batería (`aitrack-ai::drum_extract` + `/drums`):** separación HPSS multi-resolución (Wiener soft masking) + De-bleed Mid/Side + De-reverb espectral + soporte híbrido HT-Demucs opcional, corte y clasificación automática de one-shots uno a uno (`Kick`, `Snare`, `Clap`, `Closed Hat`, `Open Hat`, `Tom`, `Perc`, `Crash`) y extracción de loops alineados al compás desde cualquier WAV/MP3/FLAC.
- **Rendimiento base y UX:** arranque con proyecto limpio por defecto (`demo::empty()`, demo completo con `AITRACK_DEMO=1`), pausa de frames cuando la ventana está oculta/minimizada (`window.is_visible()`), desglose por pista de dispositivos y RAM de clips en Preferencias → Rendimiento, flush FTZ/DAZ en hilo de audio, sleep por silencio/tail, caché LRU acotada de samples, caché de picos en disco `.aipeaks`, modo bajo consumo y banner del watchdog.

## 1. Rendimiento avanzado (motor y sandbox)

- **Streaming / mmap de samples largos:** evitar decodificar archivos largos completos a `f32` en `SampleCache` y permitir streaming desde disco para instrumentos SFZ muy pesados (pianos multi-GB).
- **SIMD explícito en bucles calientes:** vectorizar mezcla de buses, ganancia/paneo y filtros críticos (`portable_simd` o kernels por arquitectura).
- **Sandbox multiproceso configurable:** permitir agrupar varios plugins externos por proceso sandbox para ahorrar memoria y arranque de subprocesos.
- **QoS de hilos en Apple Silicon y telemetría por plugin:** asignar clase de eficiencia en tareas secundarias y mostrar CPU medida por plugin individual en Preferencias → Rendimiento.

## 2. Funciones de producción pendientes del roadmap (`PLAN.md` y READMEs)

- **Grabación de audio a disco:** capturar entrada de micrófono/línea hacia archivo WAV y crear fuente/clip al detener la grabación.
- **Track Freeze & Flatten:** renderizar pistas pesadas con instrumentos/efectos en segundo plano y poder descongelar sin perder dispositivos.
- **UI de Comping (tomas):** grabar tomas en ciclo y dibujar la pista desplegable de tomas para elegir fragmentos comping.
- **Servidor / transporte MCP real:** exponer el catálogo de herramientas Command/Project sobre stdio/JSON-RPC para conectar Claude Code / Codex directamente sin pasar solo por heurísticas locales.
- **Búsqueda semántica en librería (CLAP embeddings):** indexar similitud tímbrical de samples además de la coincidencia de tonalidad/BPM/nombre.

## 3. Detalles de edición y QA manual pendiente

- **Recorte izquierdo MIDI:** conservar notas ocultas al recortar el borde izquierdo de un clip MIDI (comportamiento tipo Ableton Live).
- **Pruebas con hardware real:** latencia real con auriculares/altavoces Bluetooth, archivos en carpetas sincronizadas en la nube y comprobación visual/audio completa en Windows (`cargo check` multiplataforma sí pasa).
- **Pruebas interactivas de ratón:** validar en sesión gráfica real todos los gestos de arrastre fino, rangos de automatización y menús contextuales.

## Plugins externos de referencia instalados localmente

Están en `~/Library/Audio/Plug-Ins/{VST3,CLAP}` (por usuario, sin sudo):

- **Instrumentos:** Surge XT, Dexed, Monique, OB-Xf, JC303, sfizz, ChowKick.
- **Efectos:** Surge XT Effects, Airwindows Consolidated, CloudReverb, Dragonfly Hall/Room/Plate, ZL Equalizer 2, ZL Compressor, ZL Splitter, ChowMatrix, ChowTapeModel, ChowCentaur, BYOD, gate12, filtr, reevr, Wolf Shaper.

Retirados por inestables a `~/Library/Audio/Plug-Ins (disabled by AI-Track)/`: Dragonfly Early Reflections, Wavetable de Socalabs y Odin 2.
