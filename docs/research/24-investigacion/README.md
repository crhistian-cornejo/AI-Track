# 24 · Investigación de algoritmos, interfaz y referencias open source

Fecha: 17-nov-2026. Investigación: no se tocó código de producción. Cuatro informes hechos en paralelo, cada uno con capturas en [img/](img/) (58 PNG, prefijo = número de informe). Marcas [V]/[S] y licencias como en los informes anteriores; los repos GPL/LGPL/AGPL solo se estudian.

| Informe | Tema | Capturas |
|---|---|---|
| [01-algoritmos-efectos.md](01-algoritmos-efectos.md) | Algoritmos de los efectos (EQ, dinámica, limitador, reverb, delay, saturación, pitch/stretch) con mediciones de una sonda propia | 14 |
| [02-instrumentos.md](02-instrumentos.md) | Sintes, sampler, SFZ, granular, Acid 303 y efectos MIDI frente a Vital, Surge XT, Open303, sfizz… | 14 |
| [03-ableton-ui-ux.md](03-ableton-ui-ux.md) | Interfaz y flujo de Live 12.4 y Live 13 beta que aún nos faltan; Bitwig 6 y Logic donde lo hacen mejor | 15 |
| [04-repos-referencia.md](04-repos-referencia.md) | Repos que resuelven mejor el motor, el host, la librería y la IA (Ardour, Tracktion, creek, Signalsmith, CLAP2, rmcp…) | 15 |

Las capturas son © de cada fabricante o proyecto: sirven solo como referencia interna y **no se suben al repositorio** (es público); están en la copia local de referencia, `jarhead-reference/ai-track/docs/research/24-investigacion/img/`.

## Prioridad combinada

Primero los arreglos baratos y audibles (S), después las piezas grandes.

| # | Mejora | Informe | Esf. |
|---|---|---|---|
| 1 | Quitar el clamp final a la tasa base del clipper (`clipper.rs:236`), que anula el oversampling: −80,9 dB de aliasing antes, −26 dB a la salida | 01 | S |
| 2 | Remuestreo con antialiasing al transponer: warp, Pitch Shifter, shimmer, sampler y drum (+12 st pliega un seno de 13 kHz a nivel completo) | 01, 02 | S–M |
| 3 | Limitador true peak: interpolador 8× más largo y 2× por defecto con True Peak (hoy −0,65 dBTP con techo −1 dB) | 01 | S |
| 4 | Audio workgroups de macOS en `thread_priority.rs` (requisito para el motor multinúcleo) | 04 | S |
| 5 | Locators en la regla, con Commands, saltos y "Loop to Next Locator" | 03 | S–M |
| 6 | EQ sin cramping cerca de Nyquist: SVF de Cytomic o biquads de Vicanek (errores de hasta 13,5 dB a 20 kHz) | 01 | M |
| 7 | Signalsmith Stretch (MIT) para Complex/Complex Pro, primero en A/B contra `warp.rs` | 01, 04 | M |
| 8 | Módulo único de filtros VA (`dsp/src/ladder.rs`): ladder, OTA y diodo con no linealidad dentro del lazo ZDF | 02 | M |
| 9 | Rehacer el Acid 303 siguiendo Open303 (MIT, anotar en `NOTICE`) | 02 | S–M |
| 10 | Streaming de disco (creek, MIT/Apache) para clips largos, grabación y sampler | 02, 04 | M |
| 11 | Automatización rápida: formas sobre la selección, escalar/sesgar, simplificar (RDP); Capture Automation/MIDI | 03 | M |
| 12 | Bounce a pista nueva y "Paste Bounced Audio" reutilizando `freeze.rs` | 03 | M |
| 13 | CLAP2 de LAION (pesos Apache-2.0) como embedding para buscar por texto, cierra F4 | 04 | M |
| 14 | Servidor MCP externo con `rmcp`, cierra F5 | 04 | M |
| 15 | Motor multinúcleo: pistas en paralelo con contador de dependencias (el orden ya está en `compile.rs`) | 04 | L |

## Correcciones a informes y al PLAN

- **12:** `fx.filter` no tiene ladder (es un SVF lineal con `tanh` delante). El "diode ladder 2×" del 303 son tres polos con realimentación retrasada y sin filtro de diezmado. Chorus, flanger, Pitch Shifter y shimmer usan interpolación Hermite o lineal, no sinc.
- **PLAN §5:** el sampler no tiene la interpolación sinc que se promete.
- **13, 18 y 20:** los efectos MIDI y el granular ya existen. Del 13 ya están resueltos también Session editable, routing, grabación, autosave, freeze, take lanes y rejilla adaptativa.
- **19:** A2 resuelto (el warp se renderiza en un worker) y B3 resuelto en parte.
- **Licencias:** `truce` tiene licencia propia y `assert_no_alloc` es BSD-1-Clause; `deny.toml` rechaza las dos. Ableton Link 5 sigue siendo GPLv2+ o comercial. No hay modelo de stems con pesos de licencia comercial clara.
