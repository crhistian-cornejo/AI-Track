# AI-Track — Plan maestro del DAW

> DAW de música electrónica escrito 100 % en Rust, con interfaz por GPU, para **macOS y Windows**, que aloja **VST3, CLAP y VST2**, trae plugins propios y tiene IA integrada (BYO-subscription + ML local).
>
> Fecha: 2026-11-11 · Basado en 6 investigaciones en [`docs/research/`](docs/research/).

| # | Informe | Tema |
|---|---|---|
| 01 | [repos vibez + Lemon-Studio](docs/research/01-repos-vibez-lemon.md) | Arquitectura de referencia (GPL-3, solo ideas) |
| 02 | [repos futureboard + Pro-Wave](docs/research/02-repos-futureboard-prowave.md) | futureboard es MIT → módulos reutilizables |
| 03 | [Workflow y UX de DAWs](docs/research/03-daw-workflow-ux.md) | Ableton, FL Studio, Logic Pro, Pro Tools |
| 04 | [Plugins y DSP](docs/research/04-plugins-dsp-fabfilter-waves.md) | FabFilter, Waves, algoritmos, suite propia |
| 05 | [Stack técnico Rust](docs/research/05-rust-tech-stack.md) | UI GPU, audio, hosting, empaquetado, riesgos |
| 06 | [Integración de IA](docs/research/06-ai-integration.md) | MCP, suscripciones, ML local, librería inteligente |

---

## 1. Principios de diseño (no negociables)

1. **Command API en el centro.** Toda edición del proyecto es un comando serializable. Undo/redo, scripting, tests, autoguardado y los agentes de IA son clientes de la misma API. (Es lo que hizo posible que Gopher de FL Studio 2026 *actúe*; ver 03.)
2. **Hilo de audio sagrado.** Cero asignaciones, cero locks, cero strings, cero syscalls bloqueantes en el callback. Se verifica con un test (`assert_no_alloc`) en CI. Lo retirado vuelve a otro hilo para liberarse (patrón *graveyard*).
3. **Procesamiento por bloques con SIMD** y eventos con *offset* de muestra (automatización precisa a la muestra). Nunca muestra a muestra (defecto de futureboard y Pro-Wave).
4. **Plugins de terceros fuera de proceso** (sandbox estilo Bitwig) para que un crash no tumbe el DAW. Los plugins propios corren nativos en el grafo.
5. **Recursos mínimos.** UI que solo redibuja cuando algo cambia (sin tick permanente), un único backend GPU por SO (Metal en macOS, DX12 en Windows), modelos de IA descargados bajo demanda, perfil `release` con LTO + `codegen-units=1` + `strip`.
   - Objetivos (a validar en el prototipo): ejecutable 15–25 MB, instalador ≤ 20 MB, RAM en reposo ≤ 150 MB, frame UI < 4 ms con 200 pistas / 10 000 clips / 100 000 notas.
6. **Licencias limpias.** El producto no hereda GPL. `cargo-deny` bloquea GPL/LGPL/AGPL en CI (ver §9).
7. **Tempo map y compases desde el día 1**, I/O multicanal (no solo estéreo) desde el día 1.

---

## 2. Producto: el workflow unificado

Qué tomamos de cada DAW (detalle y los 13 conflictos resueltos en 03):

| De | Tomamos |
|---|---|
| **Ableton Live** | Interfaz general: Session View (clips lanzables) + Arrangement View sobre los **mismos** clips, Device View abajo, **Racks** (Instrument/Audio/Drum/MIDI) con cadenas y **16 macros con variaciones**, envolventes de clip, follow actions, warp, conciencia de escala global |
| **FL Studio** | **Piano Roll** (ghost notes, escala resaltada, chop/arp/strum/randomize/LFO, slides/portamento, propiedades por nota, stamp de acordes), **step sequencer** (dentro del Drum Rack), generadores/instrumentos, Patterns multipista opcionales |
| **Logic Pro** | **Lógica de mezcla**: channel strip, aux/bus, Track Stacks (Group = suma, Folder = organiza), **VCA**, Smart Controls (= vista de las macros), asistente de mastering, Chord Track |
| **Pro Tools** | I/O Setup con buses con nombre, modos de edición Slip/Grid/Shuffle/Spot (solo en Arrangement), playlists/comping, clip gain, freeze/commit, bounce offline |

### Modelo de datos unificado

```
Project
├─ TempoMap / TimeSignatureMap / Scale(global, opcional por clip)
├─ SourcePool        ── NoteSource | AudioSource  (contenido reutilizable)
├─ Tracks[]          ── Instrument | Audio | Group | Folder | Return | VCA | Main
│   ├─ SessionSlots[] ── ClipInstance → Source (enlazado o único)
│   ├─ Lanes/Playlists ─ ClipInstance[] en el Arrangement (tomas para comping)
│   ├─ DeviceChain   ── Device | Rack{ chains[], macros[16], variations[] }
│   └─ Automation    ── pista (absoluta) | envolvente de clip (absoluta o modulación relativa)
├─ Mixer             ── routing, sends, buses con nombre (I/O Setup)
└─ Patterns[]        ── opcional, multipista (estilo FL) → se aplanan a clips
```

### Ventana principal (borrador)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Transporte · BPM · Compás · Escala: A minor · CPU · [✦ Asistente IA]          │
├────────────┬─────────────────────────────────────────────────┬───────────────┤
│ Navegador  │  Session View  ⇄  Arrangement View  (Tab)       │ Panel IA      │
│ ─ Librería │  pistas × escenas  |  timeline con lanes         │ chat/propues- │
│   inteligente (Key/Tipo/BPM)   |  automatización inline      │ tas · aceptar │
│ ─ Plugins  │                                                 │ / descartar   │
│ ─ Proyecto │                                                 │               │
├────────────┴─────────────────────────────────────────────────┴───────────────┤
│ Panel inferior (conmutable): Device View/Racks │ Piano Roll │ Step Seq │ Mixer │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Arquitectura de procesos e hilos

```mermaid
flowchart LR
  subgraph MAIN["Proceso principal · crates/app"]
    UI["Hilo UI<br/>ui · ui-kit · ui-render (wgpu)"]:::nuevo
    CMD["Command API + Undo<br/>model · undo · project"]:::nuevo
    CTRL["Control del motor<br/>engine::control (compila grafo, PDC)"]:::reuso
    RT["Hilo de audio RT<br/>engine::rt + audio-io"]:::nuevo
    WK["Workers RT<br/>engine::sched"]:::reuso
    DISK["Streaming de disco<br/>media (creek, symphonia)"]:::ext
    STOCK["Plugins propios<br/>stock-plugins · dsp"]:::reuso
    MCP["Servidor MCP local<br/>ai-mcp (rmcp, 127.0.0.1 + token)"]:::ia
    LIB["Librería inteligente<br/>library (SQLite)"]:::ia
  end
  subgraph SBX["Procesos sandbox · plugin-sandbox"]
    HOST["host-vst3 · host-clap · host-vst2*"]:::reuso
    EDIT["Ventanas de editor del plugin"]:::ext
  end
  SCAN["Proceso escáner<br/>plugin-scanner + caché"]:::reuso
  ML["Proceso ML worker<br/>ml-worker (ort / tract)"]:::ia
  AGENT["CLI del usuario<br/>Claude Code · Codex · Droid"]:::ia

  UI -- comandos --> CMD
  CMD -- snapshot del modelo --> CTRL
  CTRL -- "grafo nuevo (rtrb)" --> RT
  RT -- "grafo viejo → graveyard" --> CTRL
  RT --> WK
  WK --> STOCK
  WK <-- "memoria compartida + semáforo · ipc" --> HOST
  DISK --> RT
  RT -- "medidores (triple buffer)" --> UI
  SCAN --> CMD
  AGENT -- MCP --> MCP
  MCP -- "propuestas" --> CMD
  LIB <--> ML
  MCP --> LIB

  classDef nuevo fill:#f59e0b,stroke:#b45309,color:#000
  classDef reuso fill:#22c55e,stroke:#15803d,color:#000
  classDef ia fill:#a78bfa,stroke:#6d28d9,color:#000
  classDef ext fill:#94a3b8,stroke:#475569,color:#000
```

**Leyenda:** 🟧 nuevo, diseño propio · 🟩 nuevo, inspirado o reutilizado de futureboard-studio (MIT) · 🟪 capa de IA nueva · ⬜ dependencia externa / SO. *`host-vst2` solo tras la decisión legal (§9).

---

## 4. Stack tecnológico

| Área | Elección | Por qué / alternativas |
|---|---|---|
| Ventanas | `winit` 0.30 + `raw-window-handle` | Embeber editores de plugin (HWND/NSView) |
| Render GPU | `wgpu` 30 (solo Metal y DX12) + renderer propio (quads SDF, paths, atlas de glifos); `vello` opcional para vectores | Un solo stack GPU; el mismo toolkit sirve a las UI de nuestros plugins |
| Toolkit UI | **Propio y fino**: `taffy` (layout), `parley`/`swash` (texto), `accesskit` (accesibilidad); `egui` para herramientas internas | Alternativa evaluada en F0: **GPUI** (lo usa futureboard; rápido, pero pre-1.0, sin accesibilidad y sin uso probado dentro de ventanas de plugin) |
| Audio I/O | `cpal` 0.18 + `wasapi` (exclusivo) + ASIO (licencia propietaria gratuita) + CoreAudio | FFI propia para MMCSS "Pro Audio" (Win) y audio workgroups (macOS) |
| Motor | Propio: grafo compilado fuera del hilo RT, `rtrb` + `basedrop`, scheduler multinúcleo determinista, PDC, tempo map con rampas | Referencias: futureboard (`parallel.rs`, `graveyard.rs`, tempo map, planner de PDC) |
| Hosting | `vst3` 0.3 (MIT/Apache), `clack-host` 0.2, VST2 con cabecera clean-room BSD-3 tras *feature flag*; AU en fase 2 | **No** `vst3-sys` (GPLv3), **no** VeSTige/FST (GPL) |
| MIDI | `midir`, `wmidi`, `midly` (SMF), MPE; Windows MIDI Services | MIDI en vivo directo al hilo RT, nunca vía UI |
| Medios | `symphonia` (MPL-2.0), `hound`, `rubato`, `signalsmith-stretch` (MIT), `creek` | **No** Rubber Band (GPL) |
| Proyecto | `serde` con formato versionado + migraciones; contenedor SQLite con medios verificados por hash (idea de vibez) | Autoguardado incremental |
| DSP propio | `wide` (SIMD), `realfft`, núcleo independiente del formato | Exportar como CLAP primero, luego VST3 |
| IA | `rmcp` 3.9 (MCP), `ort` (ONNX Runtime, en proceso aparte), `tract` (modelos pequeños), `rusqlite` + `sqlite-vec` | Ver §6 |
| Distribución | Velopack o `cargo-packager`; firma + notarización en macOS (con entitlement `disable-library-validation`), NSIS + Azure Artifact Signing en Windows | `crash-handler` + `minidumper` |

### Workspace de Cargo

```
ai-track/
├─ Cargo.toml · deny.toml · xtask/          # perfiles, política de licencias, empaquetado
└─ crates/
   ├─ app/            bin "aitrack": composición y arranque
   ├─ model/ time/ undo/ project/           # modelo puro, tempo map, comandos, formato
   ├─ engine/ dsp/ stock-plugins/           # motor, primitivas DSP, plugins propios
   ├─ audio-io/ midi-io/                    # E/S con prioridades RT del SO
   ├─ plugin-api/ host-clap/ host-vst3/ host-vst2/ host-au/
   ├─ ipc/ plugin-sandbox/ plugin-scanner/  # aislamiento de plugins
   ├─ media/                                # decode, stretch, picos, streaming
   ├─ ui-render/ ui-kit/ ui/                # renderer wgpu, toolkit, vistas
   ├─ library/                              # librería inteligente (SQLite, carpetas virtuales)
   ├─ ml-worker/                            # bin aparte: inferencia ONNX
   ├─ ai-mcp/ ai-agents/                    # servidor MCP, adaptadores CLI/API-key
   ├─ platform/ crash/ test-utils/
```

Reglas de capas: `engine` no depende de `ui`; `model` no hace E/S; `ui` → `ui-kit` → `ui-render`; todo lo une `app`.

---

## 5. Plugins propios (suite v1)

Diseños **originales** inspirados en técnicas publicadas; nada de marcas, nombres ni aspecto visual de FabFilter/Waves (ver 04 §no-copiar). UX inspirada en la idea de que *la pantalla es el control*: bandas arrastrables sobre el espectro, panel flotante junto a la banda, modulación por drag & drop, UI vectorial redimensionable.

| Plugin | Algoritmo base | Latencia | Meta CPU* |
|---|---|---|---|
| EQ 8 bandas (dinámico, M/S, modo lineal) | Biquads matched (Vicanek) / SVF TPT (Simper); lineal por FFT particionada | 0 (natural) | ≤ 0,3 % |
| Compresor (varios estilos, sidechain, lookahead) | Giannoulis/Massberg/Reiss | 0–lookahead | ≤ 0,5 % |
| Limitador true-peak | Lookahead + mínimo deslizante, TP 4× (ITU-R BS.1770-5) | 48–240 muestras | ≤ 1 % |
| Saturador multibanda | ADAA + oversampling IIR polifásico, crossover LR4 | baja | ≤ 1,5 % |
| Reverb | FDN con decaimiento por frecuencia + placa Dattorro | 0 | ≤ 2 % |
| Delay, Filtro, Gate, Transient, Chorus/Phaser, Utility, Analizador | Estándar documentado en 04 | 0 | ≤ 0,5 % |
| Sampler + Drum Sampler (con step sequencer) | Streaming + interpolación sinc | 0 | — |
| Sintetizador wavetable/VA, 16 voces | PolyBLEP + wavetables band-limited por octava, filtros ZDF | 0 | ≤ 3 % |

\*Un núcleo, 48 kHz estéreo; son metas de diseño, se validan con benchmarks en M1 y Zen.

Framework de exportación: prototipo de 2 semanas con **clack** vs **truce** (nih-plug está en mantenimiento; `nice-plug` aún poco probado en macOS). El DSP no depende del framework.

---

## 6. Capa de IA

### 6.1 "Trae tu propia IA" (cumpliendo los términos de cada proveedor)

| Proveedor | Vía permitida | Lo que **no** podemos hacer |
|---|---|---|
| **Claude** | El usuario usa su **Claude Code** sin modificar (con su plan) y lo conecta al MCP del DAW, o el DAW lanza `claude -p` como subproceso. O bien API key propia | Ofrecer login de Claude.ai dentro de la app ni tocar sus tokens |
| **Codex / OpenAI** | `CODEX_API_KEY` del usuario; "Sign in with ChatGPT" solo si OpenAI aprueba la app (formulario) | Usar el login de ChatGPT vía app-server en un producto comercial |
| **Factory Droid** | Modo headless con `FACTORY_API_KEY` del usuario | — (revisar ToS antes de publicar) |
| **Devin** | API en la nube con token; solo tareas asíncronas | No llega al MCP local |

**Arquitectura:** el DAW expone un **servidor MCP local** (`rmcp`) con herramientas como `search_samples`, `get_project_key`, `create_midi_clip`, `add_device`, `set_param`, `organize_library`. Cualquier agente se conecta; el DAW nunca maneja credenciales de suscripción.

```mermaid
sequenceDiagram
  participant U as Usuario
  participant A as Agente (Claude Code / Codex / Droid)
  participant M as ai-mcp (servidor MCP)
  participant C as Command API + undo
  participant L as library + ml-worker
  U->>A: "busca un kick oscuro en A menor y haz un bajo"
  A->>M: get_project_key()
  M-->>A: A minor, 128 BPM
  A->>M: search_samples("dark punchy kick", key=Am)
  M->>L: búsqueda CLAP + filtros
  L-->>M: 10 resultados
  A->>M: create_midi_clip(bajo en escala)
  M->>C: PROPUESTA (staged, no aplicada)
  C-->>U: vista previa · Aceptar / Descartar
  U->>C: Aceptar → 1 paso de undo con nombre del agente
```

**Seguridad:** las ediciones del agente son *propuestas* que se aplican como un solo paso de undo; operaciones destructivas siempre piden confirmación (lo impone el DAW, no el agente); MCP solo en `127.0.0.1` con token; nombres y metadatos de samples van al modelo como datos, nunca como instrucciones; nada de IA en el hilo de audio.

### 6.2 ML local (sin suscripción)

| Función | Modelo / método | Licencia |
|---|---|---|
| Tonalidad y escala | Chroma + plantillas (Krumhansl), raíz para one-shots | propio |
| BPM de loops | Duración + onsets + nombre del archivo | propio |
| Clasificación (FX, Drums→Kick/Snare/Hat/Clap/Perc, Synth, Bass, Vocal, One-shot vs Loop, Chop) | Embeddings **CLAP** (~34 MB cuantizado) + clasificador ligero | verificar pesos |
| Búsqueda por texto / similitud | CLAP + índice plano o `sqlite-vec` | — |
| Audio → MIDI | **basic-pitch** (Spotify) | Apache-2.0 |
| Acordes, generación de MIDI en escala | Motor de teoría basado en reglas (+ modelos pequeños después) | propio |
| Separación de stems | HTDemucs: **bloqueado** hasta aclarar licencia de pesos o tener modelo propio | ⚠️ |

Bloqueados por licencia: Essentia (AGPL) y sus modelos (no comerciales), ADTOF, YourMT3, `bliss-audio`, `aubio` (GPL).

### 6.3 Librería inteligente

```mermaid
flowchart TD
  A["Carpetas del usuario<br/>(se añaden en Ajustes)"]:::ext --> B["Escaneo incremental<br/>library::scan (mtime + blake3)"]:::ia
  B --> C["Cola de análisis<br/>ml-worker"]:::ia
  C --> D["DSP: tonalidad, BPM, loudness,<br/>one-shot vs loop"]:::ia
  C --> E["CLAP: embedding + categoría"]:::ia
  D --> F[("SQLite + sqlite-vec<br/>library.db")]:::ia
  E --> F
  F --> G["Carpetas virtuales en el navegador<br/>Key/A minor/Drums/Kicks · FX · Synths · One-shots · Loops · Chops"]:::nuevo
  F --> H["Recomendaciones según la escala,<br/>tempo y contexto del proyecto"]:::nuevo
  G --> I{"¿Organizar en disco?"}:::nuevo
  I -- "sí (opcional, con undo)" --> J["Copiar/mover a<br/>Library/Key/&lt;tonalidad&gt;/&lt;tipo&gt;/"]:::nuevo
  I -- no --> K["Solo vista virtual"]:::nuevo
  F --> M["Herramientas MCP<br/>search_samples · organize_library"]:::ia

  classDef nuevo fill:#f59e0b,stroke:#b45309,color:#000
  classDef ia fill:#a78bfa,stroke:#6d28d9,color:#000
  classDef ext fill:#94a3b8,stroke:#475569,color:#000
```

---

## 7. Hoja de ruta por fases

Cada fase termina con un criterio de salida verificable. Las duraciones dependen del tamaño del equipo y no se estiman aquí.

**Estado al 2026-11-11** (rama `main`, 343 tests en verde, clippy limpio). Leyenda: ✅ completa · 🟡 parcial · ⬜ sin empezar. Dentro de cada fase, ✔ = hecho y ✘ = falta.

| Estado | Fase | Alcance | Criterio de salida |
|---|---|---|---|
| 🟡 | **F0 · Prototipos de riesgo** | ✔ UI: decidido GPUI (sin el benchmark de 10 000 clips) · ✔ motor + `cpal` + MMCSS (✘ audio workgroups de macOS) · ✔ VST3 y CLAP en sandbox en Windows (14 µs por bloque) · ✘ editor embebido (se abre en ventana propia) · ✘ macOS sin ejecutar · ✘ clack vs truce · ✘ medir tamaño de binario | Decisiones de UI y framework de plugins tomadas con números; latencia de sandbox medida a 64 muestras |
| ✅ | **F1 · Núcleo** | ✔ `model`, tempo map con rampas, undo/redo, Command API · ✔ motor por bloques con PDC y automatización precisa a la muestra · ✔ transporte · ✔ render y export offline (WAV 16/24/32f, MP3 320, stems) | ✔ Renders golden y test de cero asignaciones en verde |
| 🟡 | **F2 · MVP de producción** | ✔ Arrangement, Session, Piano Roll con escala y ghost notes, Drum Rack con step sequencer, cadena de dispositivos, mixer con sends, hosting VST3 + CLAP, sampler + synth propios, clip gain, export · ✘ **guardar/abrir proyecto desde la app** · ✘ **grabar audio/MIDI a disco** · ✘ warp con preservación de tono (solo repitch) · ✘ streaming de disco | Se puede producir un track completo de principio a fin |
| 🟡 | **F3 · Profundidad** | ✔ Racks con 16 macros (✘ UI de variaciones) · ✔ follow actions básicas · ✔ suite completa de efectos (15 dispositivos, 202 presets) · ✔ Folder/VCA (✘ VCA anidados) · ✘ envolventes de clip en el motor · ✘ Chop/Arp/Strum/Randomize del Piano Roll · ✘ comping · ✘ freeze · ✘ EQ de fase lineal · ✘ motor multinúcleo | Paridad de workflow con las referencias en lo esencial |
| 🟡 | **F4 · Librería inteligente** | ✔ escaneo incremental + vigilancia de cambios · ✔ análisis DSP (tonalidad, BPM, tipo) · ✔ carpetas virtuales por Key/Tipo/BPM/Género · ✔ organización en disco con undo · ✔ banco CC0 de 2 641 samples + Content Hub · ✘ búsqueda por texto/similitud con CLAP · ✘ prueba con 10 000 samples | 10 000 samples analizados sin afectar al audio |
| 🟡 | **F5 · IA conectada** | ✔ propuestas con aceptar/descartar como 1 paso de undo · ✘ servidor MCP · ✘ modo API key · ✘ adaptadores headless (Claude Code, Droid, Codex) · ✘ chat real | Un agente crea un bajo en escala y se deshace en 1 paso |
| 🟡 | **F6 · IA musical** | ✔ audio→MIDI (basic-pitch, mono, batería) · ✔ detección de acordes · ✔ motor de teoría (progresiones, bajos, arpegios, patrones de batería) · ✘ Chord Track en la interfaz | — |
| 🟡 | **F7 · Extra** | ✔ stems: pipeline Demucs completo y salida WAV (✘ sin ejecutar con el modelo real: la red bloquea Hugging Face) · ✘ asistente de mezcla/mastering · ✘ MPE completo · ✘ Ableton Link · ✘ AU · VST2 descartado | — |

**Distribución:** ⬜ CI con builds firmados para macOS (universal) y Windows, `cargo-deny` en CI, fuzzing del host de plugins. Los tests golden de audio ya existen (✔).

### 7.1 Prioridad inmediata: poder hacer canciones (añadido 2026-11-11)

Hoy se puede abrir el programa, pero no se puede terminar una canción cómodamente: el sample no se edita bien, el arrangement se queda corto, el navegador es incómodo y las automatizaciones cuestan. Esta lista pasa por delante del resto de F2/F3/F4 hasta que se cierre.

**Qué hay ya en curso en otras sesiones** (revisado el 2026-11-11; no repetir su trabajo, integrarlo):

| Dónde | Qué cubre | Estado |
|---|---|---|
| `main`, sin commit: `model/src/ops.rs`, `Clip::new`, `Fade`, `Clip::gain_at`, `engine/src/clip.rs` | Fundamentos del editor de samples: fades, envolvente de volumen, bucle con crossfade anti-clic, recorte, división, crop, duplicar como comandos puros | Escrito y probado, falta commit |
| `main`, sin commit: `app/src/grid.rs` | Rejilla **adaptativa** (más fina al hacer zoom, 8 compases → 1/128, tresillos, densidad como Narrowest…Widest de Live) | Escrita; **ninguna vista la usa todavía** |
| Worktree `…-t4` (`views/sample_editor/`, `waveform.rs`, ~3 300 líneas) | Editor de samples en la pestaña Clip: marcadores de inicio/fin, fades con curva, envolvente, llave de bucle, cursor y selección, picos de forma de onda | En curso |
| Worktree `…-t5` (`keys.rs`, `commands.rs`, `editing/`, `nav.rs`, `views/shortcuts.rs`) | Tabla única de atajos + hoja de atajos con buscador, selección/portapapeles/marcador de inserción de tipo Live, zoom del arrangement (1–640 px por negra), zoom a la selección, seguir el playhead | En curso |
| Worktree `sidechain` y `agent-a8b3de1…` | Sidechain y Shaper; atajos/layout/abrir-guardar (WIP que no compila) | Fuera de este alcance |

**Qué no cubre nadie** (lo que sigue es lo que hay que construir):

```mermaid
flowchart TD
  subgraph FLUJO["Flujo de producir una canción"]
    direction TB
    A["Buscar sonido<br/>Cmd+K · views/palette.rs"]:::cambio --> B["Escuchar en contexto<br/>audition.rs + engine preview"]:::cambio
    B --> C["Arrastrar al track<br/>views/arrangement.rs"]
    C --> D["Mover fuera de rejilla<br/>grid.rs adaptativa + Alt"]:::cambio
    D --> E["Editar el sample<br/>views/sample_editor/"]:::cambio
    E --> F["Pitch, tonalidad, stretch<br/>engine/src/clip.rs + stretch"]:::cambio
    F --> G["Automatizar lo que sea<br/>views/arrangement.rs · lane picker"]:::cambio
    G --> H["Mezcla y export<br/>mixer.rs · export.rs"]
  end

  GRID["grid.rs<br/>(escrita, sin adoptar)"]:::wip -.-> D
  OPS["model/ops.rs + Command API<br/>(fundamentos listos)"]:::wip -.-> E
  T4["worktree t4: sample_editor"]:::wip -.-> E
  T5["worktree t5: keys, editing, nav"]:::wip -.-> D
  LIB["crates/library<br/>(índice, key, favoritos)"] --> A
  HUB["Content Hub + soundbank<br/>views/hub.rs"]:::cambio --> A
  CMD["Command API<br/>(toda edición)"] --> E
  CMD --> G

  classDef cambio fill:#f59e0b,stroke:#b45309,color:#000
  classDef wip fill:#38bdf8,stroke:#0369a1,color:#000
```

Leyenda: 🟧 naranja = lo que hay que construir o cambiar · 🟦 azul = ya en curso en otra sesión (hay que integrarlo, no rehacerlo) · sin color = existe y se queda igual.

#### P0 · Editar y colocar samples (cierra lo que ya está a medias)

1. **Integrar el trabajo de t4 y t5 en `main`**: commit de los fundamentos (`ops.rs`, `clip.rs`, `grid.rs`), merge de los dos worktrees y resolver choques en `state.rs`, `workspace.rs`, `arrangement.rs` y `views/mod.rs`. Hasta que esto esté hecho, cualquier trabajo nuevo choca.
2. **Adoptar `grid.rs` en el arrangement, el piano roll y el editor de samples.** Sustituye el menú de snap fijo (`state::Snap`: Off/Bar/Beat/1/8/1/16/1/32/1/16T) por un modo **Adaptativo** (por defecto, como Live) más una rejilla fija opcional hasta **1/64 y 1/128**, con tresillos y densidad. Las líneas de la rejilla se pintan según el zoom.
3. **Zoom estilo Ableton/FL**: zoom horizontal anclado al puntero (rueda con Ctrl/Cmd), zoom vertical de alto de pista, arrastrar en la regla para escalar, zoom a la selección y "ajustar todo". Mantener los límites de `nav.rs` (1–640 px por negra).
4. **Mover fuera de tiempo (off-beat)**: mantener `Alt` mientras se arrastra desactiva el snap (como Live), y con el zoom alto el snap adaptativo ya permite posiciones finas. `Cmd+Alt+←/→` empuja el clip en pasos de muestra/tick; flechas ← → mueven por la rejilla actual. Todo son `Command` (un solo paso de undo por arrastre).
5. **Pitch, tonalidad y stretch del sample (la pieza grande que falta)**:
   - Modelo: `Warp` ya existe; añadir `pitch_semitones`, `pitch_cents`, modo de warp (Beats / Tones / Texture / Re-Pitch / Complex) y **`Command::SetClipWarp`**.
   - Motor: hoy solo hay repitch. Implementar un time-stretch con preservación de tono (WSOLA o phase vocoder para empezar; **sin Rubber Band**, es GPL) y pitch-shift independiente, en `crates/dsp`, sin asignaciones en `process` (render del stretch fuera del hilo RT y cacheado, o procesado por bloques con búfer previo).
   - Interfaz: en la pestaña Clip, controles `Warp`, `Pitch`, `Key`, `BPM original`, `×2 / ÷2`, marcadores de warp arrastrables sobre la forma de onda, detección automática del BPM y la tonalidad (ya existe en `crates/ai`).
   - **Todo lo que dijo el usuario**: ataque/decay/sustain/release del sample (envolvente de amplitud del clip, no solo la de volumen), reversa, ganancia, fade in/out, normalizar, inicio/fin, loop, y "Consolidar" a un nuevo archivo.
6. **Atajos de manipulación**: `Cmd+E` dividir, `Cmd+D` duplicar (ya están), `Cmd+J` consolidar, `R` reversa, `Cmd+Shift+↑/↓` transponer, doble clic abre el editor, `Cmd+B` duplicar en el mismo sitio.

#### P1 · Arrangement más completo

7. Pistas redimensionables y colapsables con grupos; vista de **lanes** (take lanes) para comping.
8. **Marcadores de bucle/loop brace y regiones** (arrangement locators) y un **Scrub/Preview** que respete la rejilla adaptativa.
9. Selección con rubber-band, multi-clip, mover entre pistas con un solo undo, copiar con `Alt+arrastrar`.
10. Pintar una forma de onda **más fiel** (picos por zoom, estéreo/mono) desde `waveform.rs` (t4).

#### P1 · Automatización (todo se puede automatizar, como en Live)

11. **Selector de parámetro por pista**: dos menús como Live ("Device" → "Parameter") en la cabecera de la lane; hoy solo se muestra `automation.first()` y no se elige. Campo de búsqueda en el selector.
12. **Crear automatización desde cualquier parámetro**: clic derecho sobre un knob, fader, macro o botón → "Automatizar" (crea la lane con `Command::SetAutomation` y la muestra). Esto cubre el pedido de "todo debe poder automatizarse".
13. **Clips de automatización**: la lane no es solo puntos sueltos; se pueden dibujar segmentos, curvas (ya hay `curve` por punto), rampas y formas (seno, triángulo, escalón) con un lápiz, y **agruparlos en un clip** que se puede mover, duplicar y alargar con el resto del arrangement.
14. **Colapsar y ver**: botón para plegar todas las lanes a una banda de 12 px con una miniatura; "Mostrar solo automatizado"; conmutador "Armar automatización" (write/touch/latch).
15. **Envolventes de clip** (volumen/pan/pitch dentro del clip) editables en el editor de samples; t4 ya cubre el volumen.

#### P0 · Navegador y búsqueda: Cmd+K contextual (sustituye la barra lateral de búsqueda)

16. **Paleta global `Cmd+K`** (`views/palette.rs`, nuevo): un solo campo tipo Spotlight que filtra *todo*: samples, plugins/dispositivos, presets, comandos, pistas, clips, ajustes. Filtros rápidos con prefijos y chips: `s:` samples, `p:` plugins, `>` comandos, `@` pistas, y un conmutador **"Solo en la tonalidad del proyecto"** (reutiliza `Matches project` y `keys_match`).
    - La barra lateral de búsqueda se quita; el navegador queda como **explorador de carpetas** (el árbol), no como lista de resultados.
    - **Contextual**: si hay un clip o una pista seleccionados, sugiere samples de la misma tonalidad/BPM/tipo (por ejemplo, pistas de batería → ofrece kicks). Los resultados se ordenan por coincidencia de key + BPM + categoría + favoritos + uso reciente.
    - Se alimenta de `crates/library` (SQLite). Búsqueda por texto primero; similitud con CLAP después (F4).
    - **Las acciones del proyecto se ejecutan como `Command` de la tabla de `commands.rs` de t5**; así el Cmd+K y los atajos comparten una sola lista de comandos.
17. **Audición estilo Splice**: al moverse por la lista con ↑/↓ el sample **suena solo** (sin Enter), con retardo de ~80 ms para no tartamudear; `Espacio` pausa; `Enter` lo añade; `Tab` lo arrastra a la pista seleccionada. El sample de audición **se sincroniza al BPM y a la tonalidad del proyecto** (el stretch/pitch del punto 5 se reutiliza en el motor de preview) con un conmutador "Sync" y un control de volumen de preview. Forma de onda con cabezal y clic para saltar a un punto.
18. **Buscar sin salir del Cmd+K**: el comando `Buscar samples gratis…` abre el Content Hub (`views/hub.rs`) filtrado y deja descargar un pack sin cambiar de pantalla, con barra de progreso y aviso de licencia. Hay que localizar con más rapidez los packs gratuitos ya descubiertos: un índice local `content/hub.json` con nombre, licencia, tamaño, géneros y enlace, y búsqueda por texto sobre ese índice.
19. **Futuro, no ahora**: proveedor de Splice como un adaptador en `crates/library` (misma interfaz que la librería local) para que sus resultados aparezcan en el Cmd+K; solo con las vías oficiales y el login del usuario, sin tocar credenciales.

#### P0 · Navegador: arreglos de UX concretos

20. **Todas las filas con el mismo ancho**. Hoy `browser.rs` pinta nombre + badges + duración en una fila flex y el ancho lo manda el texto. Pasar a **columnas de ancho fijo**: icono (16 px) · nombre (flex, con elipsis y tooltip) · key (40) · BPM (36) · duración (40) · favorito (20). Cabeceras de columna ordenables (nombre, key, BPM, duración).
21. **Estrellas de favorito alineadas a la derecha**, en una columna propia de ancho fijo y siempre en la misma posición; el área de clic es la celda entera (24×24 px), no el icono. Estrella apagada visible solo al pasar el cursor, para no ensuciar la lista.
22. **Carpetas debajo, samples debajo de la carpeta**: el árbol de carpetas queda arriba con altura recordada y los samples ocupan el resto en una lista a ancho completo (hoy el árbol tiene `max_h(220)` y se reparte mal).
23. **Arrancar limpio**: `main.rs:66` abre `demo::project()` (con contenido). Cambiarlo a `demo::empty()` (ya existe en `project_io.rs`), y dejar la demo y el starter electrónico solo en `File → New from template` (ya existe).

#### Orden sugerido

```mermaid
flowchart LR
  I["1. Integrar t4 + t5<br/>commit de ops/clip/grid"]:::cambio --> G["2. Grid adaptativo<br/>en las tres vistas"]:::cambio
  G --> Z["3. Zoom y off-beat"]:::cambio
  I --> B["4. Browser:<br/>columnas, estrellas, carpetas"]:::cambio
  B --> K["5. Cmd+K + audición"]:::cambio
  Z --> S["6. Pitch / stretch / ADSR"]:::cambio
  K --> S
  S --> A["7. Automatización:<br/>picker + clips"]:::cambio
  A --> H["8. Hub rápido,<br/>Splice más adelante"]:::cambio
  classDef cambio fill:#f59e0b,stroke:#b45309,color:#000
```

**Criterio de salida:** se puede hacer una canción de 3 minutos desde un proyecto vacío usando solo samples de la librería: buscarlos con `Cmd+K`, oírlos a tempo y tonalidad, colocarlos fuera de tiempo, estirarlos y cambiarles el tono, automatizar filtro y volumen, y exportar, sin salir del teclado más que para arrastrar.

---

## 8. Qué reutilizamos de las referencias

| Repo | Licencia | Uso |
|---|---|---|
| **futureboard-studio** | MIT | **Código reutilizable con atribución:** cabecera VST2 clean-room, puente de memoria compartida (`audio_bridge.rs`, `rt_handoff.rs`), aislamiento del escáner + BD de plugins, `parallel.rs`, `graveyard.rs`, tempo map, planner de PDC, arnés de test de asignaciones, oversampler, reverb, crossover, wavetable/filtros, convolver, medidor de loudness. **No:** wrappers `vstplugins/*` (bindings GPL), pesos de drumsilencer (dataset no comercial), `SphereAudioPlugins`. |
| **vibez** | GPL-3 | Solo ideas: colas separadas para transporte, verificación de PDC con plugins de prueba, proyecto en SQLite con medios por hash |
| **Lemon-Studio** | GPL-3 | Solo ideas: plano de control con niveles read/propose/edit para IA, plugins de serie duales, pool multinúcleo determinista |
| **Pro-Wave** | GPL-3 | Solo ideas: cola de retorno con créditos, `IAudioClient3`, un solo backend GPU |

Errores a **no** repetir: `Mutex` sobre plugins o parámetros, reconstruir el motor en cada edición, tick de UI permanente, procesamiento muestra a muestra, MIDI vía hilo de UI, latencia de sandbox acumulada por plugin, dos stacks GPU, archivos de 24 000 líneas.

---

## 9. Riesgos principales

| # | Riesgo | Mitigación |
|---|---|---|
| 1 | **VST2**: Steinberg no da licencias desde 2018 | Cabecera clean-room BSD-3, solo en el sandbox, *feature flag*, sin usar el nombre "VST", **opinión legal escrita antes de publicar** |
| 2 | Contaminación GPL (vst3-sys, ASIO GPL, Rubber Band, FFTW…) | `cargo-deny` en CI; sustitutos listados en 05 |
| 3 | Coste de construir el toolkit UI propio | Prototipo F0 contra GPUI; `egui` para herramientas internas |
| 4 | Latencia/overhead del sandbox a buffers pequeños | Handoff en el mismo ciclo por memoria compartida + semáforo; agrupar plugins por proceso; opción "en proceso" |
| 5 | Compatibilidad con plugins comerciales | Matriz de pruebas con los 50 plugins más usados; lista negra persistente |
| 6 | Términos de uso de suscripciones de IA | Solo vías oficiales (§6.1); nunca manejar credenciales de suscripción |
| 7 | Licencias de pesos de modelos | Tabla de licencias por modelo; stems bloqueados hasta resolverse |
| 8 | Crates clave con un solo mantenedor (`vst3`, `clack-host`, `basedrop`, `wasapi`, `creek`) | Fijar versiones, estar listos para hacer fork |
| 9 | Firma/notarización en macOS bloquea plugins | Entitlement `disable-library-validation` desde el primer build firmado |
| 10 | Marcas y trade dress (FabFilter, Waves, SSL, Ableton…) | Nombres y diseño visual propios; revisión antes de publicar |

---

## 10. Decisiones tomadas (2026-11-11)

1. **Licencia: MIT, código abierto.** Seguimos sin poder copiar código GPL (vibez, Lemon-Studio, Pro-Wave): obligaría a relicenciar todo como GPL. futureboard-studio (MIT) sí se adapta con atribución en `NOTICE`.
2. **Interfaz: GPUI**, el mismo `gpui-pre` 0.3.9 parcheado que usa monoBOT (copiado en `vendor/`, Apache-2.0), con su mismo enfoque de tema, `motion` (curvas y duraciones compartidas) e iconos Hugeicons (MIT). Sustituye al toolkit propio de §4.
3. **Formatos: solo VST3 y CLAP.** Sin VST2 ni AU por ahora.
4. **IA desde el inicio:** audio→MIDI (basic-pitch, Apache-2.0) y separación de stems (Demucs ONNX). Los pesos de stems se descargan bajo demanda desde su origen, con aviso de licencia, en vez de redistribuirlos.
5. **Banco de sonidos incluido:** samples y presets gratuitos de EDM, pop, reggaetón, etc. Solo se empaquetan los packs con licencia que permita redistribuir (CC0, CC-BY o permiso explícito), descargados y verificados con `cargo xtask soundbank`. Los packs gratis que prohíben redistribución se ofrecen como enlaces en un "Content Hub" para que el usuario los descargue él mismo.
6. **Nombre:** AI-Track (provisional).
