# El asistente como co-productor: runtime en hilo propio, samples y oídos

Fecha: 2026-11-19 · Estado: aprobado para implementar el 2026-11-21 (el usuario delegó la revisión de las decisiones del autor de §12) · Tamaño: proyecto (varias fases)

## 1. Intención y por qué

El usuario quiere que el chat de IA de AI-Track sea un **co-productor**: que sepa
generar canciones completas usando *todo lo que el usuario tiene* (sus samples,
sus sintes y plugins, el banco de fábrica) y que mezcle y masterice con criterio.
Hoy no puede, por cuatro razones concretas:

1. **No ve la librería.** No hay herramienta para buscar ni colocar samples.
2. **No ve el contenido del audio.** De un clip de audio sabe el nombre de archivo
   (`get_track`) y, si está seleccionado o adjunto, su duración, tonalidad de la
   fuente y ajuste de warp (`describe_clip` en el contexto), pero nada medido: ni
   BPM detectado, ni volumen, ni forma de onda.
3. **No puede oír.** Mezcla y masteriza razonando a ciegas, sin medir el resultado
   antes ni después de cada cambio.
4. **No controla los plugins de terceros** (VST3/CLAP): `get_device` dice que sus
   parámetros "no son visibles".

El orden acordado de entrega es: **1) samples y análisis, 2) oídos, 3) plugins
del usuario, 4) guía de canción completa.** Este documento diseña la base técnica
(el *runtime*) y las piezas 1 y 2. Las piezas 3 y 4 solo quedan como puntos de
extensión (§11) y tendrán su propio diseño.

El usuario eligió el enfoque **B**: mover la ejecución de las herramientas fuera del
hilo de la interfaz, a un hilo propio con servicios inyectados, "de forma correcta".
La razón técnica es que analizar un archivo o renderizar el borrador tarda
segundos, y hoy las herramientas se ejecutan *en el hilo de la interfaz*.

## 2. Cómo funciona hoy (verificado en el código)

**Chat en la app** (`crates/app/src/views/ai_panel/agent.rs`, `crates/ai/src/llm/cli/`):
- Claude Code o Codex corren como proceso hijo. Cada turno tiene su hilo
  (`aitrack-cli-turn`) que lee el proceso y reenvía eventos (`CliEvent`).
- Una llamada a herramienta llega como `CliEvent::Tool { name, input, reply }`.
  El hilo de la interfaz la recoge en `poll_cli` (temporizador de 16 ms), la
  ejecuta *síncrona* con `run_tool(&mut Session, …)` y contesta por `reply`.
- `Session` (`crates/ai/src/llm/tools/mod.rs`) es un clon del `Project` (el
  "borrador") más la lista de `Command`s y de líneas de cambio. Al terminar el
  turno, `offer()` los propone como una sola propuesta (un paso de deshacer).
- `ask_user` es la única herramienta que *espera*: la interfaz guarda un
  `Pending` con el canal de respuesta y el hilo del turno bloquea hasta la respuesta.
- `generate_part` es la única "delegada": usa `midi_gen` (en `crates/app`, 455 líneas,
  **sin dependencias de GPUI**) y el motor de patrones de `aitrack-ai`.
- La rama `Engine::Api` de `agent.rs` es **inalcanzable**: el proveedor de API está
  oculto (`PROVIDERS` solo ofrece Claude Code y Codex).

**Servidor MCP externo** (`crates/mcp/`, `crates/app/src/mcp_host.rs`, `views/mcp.rs`):
- Las llamadas llegan por hilos del servidor, pasan por un canal al hilo de la
  interfaz y se ejecutan en `mcp_tick` (cada 33 ms, hasta 8 por tick) con
  `Broker::call(&mut Project, &mut History, …)`. El broker conserva un borrador
  vivo entre llamadas y reemplaza la propuesta en cada edición.

**Piezas ya existentes que se reutilizan:**
- Búsqueda de sonidos del panel Sounds: `SoundSearchFn` (cierre `Send` con
  conexiones SQLite propias, `crates/app/src/real/library.rs`), `fit_key`/`fit_tempo`
  (`crates/library/src/fit.rs`), recomendaciones, similares (`similar.rs`).
- Análisis: `aitrack_ai::analyze` (tonalidad, BPM, bucle/one-shot, volumen,
  `Features`, categoría), `detect_chords`, `Loudness` (`crates/ai/src/features.rs`).
- Renderizado offline: `aitrack_engine::export::render(project, &dyn ProcessorFactory, opts, stems, progress)`,
  ya usado desde hilos de fondo por freeze/bounce con `engine.export_factory()` (un `Arc`).
- Colocar un sample ajustado a la canción: `sample_clip_commands`,
  `audio_track_commands` y `drop_fit` (hoy en `crates/app/src/workspace.rs` y
  `views/sounds/logic.rs`, atadas a `AppState`/`LibraryItem` pero sin GPUI).

**Dependencias entre crates** (relevantes para dónde puede vivir cada cosa):
`ai → model, stock, dsp`; `library → model, ai (opcional)`; `engine → model, dsp`;
`host → model, dsp`; `mcp → model, ai`. Por tanto `ai` **no puede** depender de
`library` (habría un ciclo con su dependencia opcional de `ai`) y no debe depender de
`engine` (capas: `ai` es análisis y ML, `engine` es el motor de audio). Lo que `ai`
necesite de ellos entra por *traits* definidos en `ai`. La regla del repo es que
`crates/app` solo pinta y conecta, así que la lógica de cada implementación vive en
el crate que ya sabe del tema (`library` para samples, §6.4) y `app` solo la cablea.

## 3. Objetivos, no-objetivos y anti-objetivos

**Objetivos**
- Cualquier herramienta puede tardar segundos sin congelar la interfaz.
- El asistente puede **buscar, inspeccionar y colocar** samples de la librería del
  usuario (usuario, banco de fábrica y carpeta Splice) ya ajustados a la tonalidad
  y al tempo del proyecto.
- El asistente puede **analizar** un sample o un clip de audio del proyecto
  (tonalidad, BPM, tipo, volumen, forma de onda resumida) y **medir** la mezcla
  completa o una pista (volumen LUFS, pico real, reparto de frecuencias, anchura
  estéreo, energía por compás) *sobre el borrador*, para decidir y verificar.
- El chat y el servidor MCP comparten **un único registro de herramientas** y un
  único camino de ejecución.
- La UI del chat muestra el progreso de las herramientas largas y se puede parar.

**No-objetivos (de este documento)**
- Control de parámetros de plugins VST3/CLAP (pieza 3) y la guía de canción
  completa (pieza 4): solo puntos de extensión.
- Que el modelo "escuche" audio: no se sube audio al LLM, solo números y resúmenes.
- Cambiar el modelo de propuestas/deshacer, el trait `Processor` o el hilo de audio.
- Rediseñar el chat: solo tarjetas nuevas para las herramientas nuevas.

(Cargar one-shots en pads de un Drum Rack con Sampler **sí** está dentro: es la fase 3b, §6.7 y §7.)

**Anti-objetivos (fallaría aunque "funcione")**
- Cualquier camino de herramienta que bloquee el hilo de la interfaz más de ~16 ms.
- Un hilo que mute el `Project` real: toda edición sigue siendo un `Command` aplicado
  al borrador y ofrecido como propuesta.
- Resultados de herramienta que revienten el contexto del modelo o filtren rutas
  absolutas del disco del usuario.
- Dos implementaciones distintas de "colocar un sample" (la de la UI y la del asistente).

## 4. Restricciones

- Reglas de `CLAUDE.md`: toda edición es un `Command`; nada de asignaciones/locks/E-S
  en el hilo de audio (el render offline corre en hilos de fondo, nunca en el RT);
  tests junto al código; comprobar con `cargo check --workspace`,
  `cargo test -p <crate>` y `cargo clippy -p <crate>`; **no ejecutar `cargo fmt`**
  (el repo está formateado a mano); colores/tiempos/iconos de la UI salen de `theme.rs`,
  `motion.rs`, `icons.rs`.
- Sin dependencias GPL/LGPL/AGPL (`deny.toml`).
- Las herramientas siguen siendo las que ven Claude Code (MCP "sdk" por stdio) y
  Codex (`dynamicTools`): mismos esquemas JSON, sin puertos nuevos.
- Los chats guardados (`"assistant"` en el `.aitrack`) deben seguir leyéndose: no
  cambia el formato serializado de `Step`/`ToolState`.

## 5. Enfoques considerados

**A. Herramientas asíncronas delegadas a la app.** Las nuevas herramientas devuelven
"pendiente", la app trabaja en segundo plano y entrega el resultado. Cambio acotado,
pero hay dos modelos de ejecución (síncrono en la UI y asíncrono delegado) y el MCP
tendría que replicar el mecanismo. Descartado por el usuario.

**B. Ejecutar las herramientas en el hilo del turno con servicios inyectados.
(Elegido.)** Todas las herramientas pasan a ser síncronas *en un hilo propio*, con un
conjunto de servicios inyectado. Un solo modelo de ejecución para el chat y el MCP.
Coste: toca el bucle del agente y el broker del MCP. Se mitiga entregándolo por
fases en las que la app sigue funcionando y con tests entre cada una.

**C. Volcar librería y análisis en el prompt.** No escala con librerías de miles de
samples y no permite medir tras cada cambio. Descartado.

Dentro de B se descartó mover *todo el bucle del agente* a un hilo nuevo: el hilo
`aitrack-cli-turn` ya existe, ya espera las herramientas y ya es dueño del proceso
hijo. La pieza que se mueve es solo **quién ejecuta la herramienta**.

## 6. Diseño

### 6.1 Vista general

```
 hilo UI (GPUI)                    hilo del turno (aitrack-cli-turn)              servicios
 ─────────────                     ─────────────────────────────────              ─────────
 submit() ── TurnSpec + runner ──▶ cli::run ◀── stdout/stdin ──▶ Claude Code / Codex
   │                                  │ tools/call
   │                                  ▼
   │                            TurnRunner::call ──▶ Toolbox::execute(ctx, name, input)
   │                                  │                 │  ctx.session   (borrador compartido, lock corto)
   ◀── CliEvent (Text, Thinking,      │                 │  ctx.services  ──▶ SampleLibrary  (SQLite propia)
       ToolCall, ToolProgress,        │                 │                ──▶ AudioRender   (engine::export)
       ToolDone, Ask, Done) ◀─────────┘                 │  ctx.cancel / ctx.progress
 poll_cli: solo pinta y atiende Ask                      ▼
 finish(): toma el borrador y lo ofrece                 Outcome
```

El hilo de la interfaz **nunca ejecuta una herramienta**: solo pinta eventos, atiende
las preguntas al usuario (`Ask`) y, al terminar, toma el borrador y lo ofrece.

### 6.2 El borrador compartido

El borrador deja de vivir en el `Turn` de la UI y pasa a un manejador compartido:
`SharedSession` (un `Arc<Mutex<Session>>` con métodos como `with(|s| …)` y
`snapshot() -> Project`).

Reglas, que son parte del diseño:
- El candado se toma **solo para operaciones cortas y de CPU**: aplicar comandos,
  leer tonalidad/BPM, clonar el `Project`. Nunca se mantiene durante una búsqueda,
  un análisis o un render: esas herramientas toman un `snapshot()` o los datos que
  necesitan, sueltan el candado y trabajan.
- Al terminar (o al parar) la UI toma el contenido con `SharedSession::take()`. Si
  una herramienta seguía en vuelo, ve la señal de cancelación, termina y sus
  cambios tardíos van a una sesión que nadie lee.
- El borrador parte de un clon del proyecto al empezar el turno, como hoy. **No** se
  usa `export_snapshot`: ese método pregunta por el estado de los plugins en el hilo
  de la UI y además lo escribe en el proyecto real sin deshacer. Los plugins de
  terceros llegan al borrador con el estado que el proyecto ya tiene.

### 6.3 El registro de herramientas

Nuevo módulo `crates/ai/src/llm/runtime/`:

- `ToolDef { name, description, schema, kind: Read | Edit, title }`: sustituye al
  `is_read_only(name)` por prefijo (`get_`/`list_`) y a la tabla `title()`. Las
  herramientas nuevas (`search_samples`, `measure_mix`…) son de lectura aunque no
  empiecen por `get_`. Un test exige que toda definición tenga tipo y título.
- `Toolbox`: composición de `ToolSet`s (`ProjectTools` — las 23 actuales (24 con
  `ask_user`), sin cambios de comportamiento —, `SampleTools`, `EarsTools`; más adelante `PluginTools`).
  `definitions()` mantiene un orden estable (la caché del proceso caliente se
  invalida por hash de herramientas). `agent_definitions()` añade `ask_user`, que el
  MCP no recibe.
- `ToolCtx<'a> { session: &SharedSession, services: &Services, cancel: &CancelToken, progress: &dyn Fn(String) }`.
- `Toolbox::execute(&self, ctx, name, input) -> Outcome`. Síncrona: el hilo que
  llama espera. `Outcome::Delegate` y `DELEGATED` **desaparecen**.
- `generate_part` pasa a ser una herramienta normal de `ProjectTools`: se mueve
  `midi_gen` (`prepare`, `commands`, `free_start`, `Dest`, `Prepared`, `pick_target`)
  a `aitrack-ai` (junto a `generate`), y `is_drum_track` a `aitrack-model`
  (`Track::is_drum()`). La app importa desde ahí (`state.rs`, `part.rs`, `acid_gen.rs`).

### 6.4 Servicios

`Services { samples: Option<Arc<dyn SampleLibrary>>, render: Option<Arc<dyn AudioRender>> }`,
definidos como traits en `aitrack-ai` con tipos de datos propios (`Send + Sync`).
Si falta un servicio (modo stub, sin librería) la herramienta devuelve un error
claro, no desaparece de la lista.

`SampleLibrary` (trait en `ai`; implementado en `crates/library` bajo su
característica `ai`, sobre `Library::search`/`recommend`/similares; `app` solo lo
cablea con las rutas de las bases):
- `overview() -> LibraryOverview`: cuántos samples hay por origen (banco de fábrica,
  del usuario, Splice) y por categoría, y qué tonalidades/rangos de BPM cubren.
- `search(&SampleQuery) -> SampleHits`: texto, categoría/tipo, tonalidad
  (compatible o exacta), rango de BPM, bucle/one-shot, carpeta/pack, favoritos,
  `similar_to: SampleRef`, `limit`. Cada `SampleCard` lleva `ref` estable
  (`"f:123"` fábrica / `"u:456"` usuario), nombre, categoría, tonalidad, BPM y su
  fiabilidad, duración, origen, pack (ruta relativa, **nunca absoluta**) y una nota
  de ajuste al proyecto (`transpuesta +2 st`, `tempo ×1, +3,1 %`) cuando la consulta
  trae la tonalidad y el BPM del proyecto.
- `get(ref) -> Option<SampleCard>`.
- `analyze(target, cancel) -> SampleAnalysisSummary`: devuelve lo que la librería ya
  sabe al instante; ejecuta `aitrack_ai::analyze` solo si falta algo o se pide
  `deep`. Admite también la ruta de un clip de audio del proyecto. Caché en memoria
  por (ruta, mtime, tamaño). Analiza como máximo los primeros 120 s.
- Colocar un sample son **dos pasos**, porque construir los comandos reserva ids del
  proyecto (necesita `&mut Project`, es decir, el candado del borrador) y leer la
  cabecera del archivo es E/S (no debe ocurrir con el candado puesto):
  1. `probe_for_placement(ref) -> PlaceSource { path, name, key, bpm, bpm_trust, is_loop, facts }`
     (sin candado): resuelve el sample, lee la cabecera WAV y **rechaza** las muestras
     solo en la nube (ver abajo).
  2. `place::plan(&mut Project, &PlaceRequest) -> Placement { commands, summary }`
     (con el candado, puro y rápido): construye los `Command`s (pista nueva o
     existente, compás, ajuste de tonalidad y tempo con warp).
  **Una sola implementación**: `plan` se extrae de la app a `crates/library/src/place.rs`
  (`sample_clip_commands`, `audio_track_commands`, `drop_fit`, `DropFit`, `clip_length`,
  lectura de cabecera WAV) y la usan tanto el arrastre de la UI como esta herramienta.
  Su test existente
  (`a_sound_dropped_with_fit_leaves_the_clip_transposed_warped_and_the_right_length`)
  debe seguir pasando contra el código movido.
- **Muestras solo en la nube** (`Sample.cloud`: iCloud, Dropbox, OneDrive, Google
  Drive sin descargar). Regla del repo: nada que abra un archivo de sample puede
  tocar un marcador de posición, porque leerlo lo descarga. Por tanto: `SampleCard`
  lleva `online_only`; `search` las **excluye por defecto** y con `include_online: true`
  las devuelve marcadas; `analyze`, `get_waveform` y `probe_for_placement` las
  rechazan con un mensaje claro ("solo en la nube: pídele al usuario que la
  descargue o la arrastre"). El asistente **nunca** descarga por su cuenta (descargar
  es un efecto visible que hoy solo dispara el usuario al previsualizar o arrastrar).
  *(Decisión mía, derivada de la regla del repo.)*

`AudioRender` (en `ai`, implementado en `app` sobre `aitrack_engine::export::render`):
- `render(&Project, &RenderRequest, &CancelToken, &mut dyn FnMut(f32)) -> Result<Rendered>`
  con `Rendered { sample_rate, mix: AudioBuffer, stems: Vec<(TrackId, String, AudioBuffer)> }`
  (`AudioBuffer` de `aitrack-dsp`). `RenderRequest` lleva el rango en ticks y las
  pistas a extraer como stems.
- La app construye `Services` **al empezar cada turno** en el hilo de la UI con
  `Arc`s baratos: la fábrica de procesadores actual (`export_factory()`) y el
  manejador de librería. El manejador de librería es de larga vida: guarda las
  rutas de las bases, abre sus conexiones SQLite al primer uso tras un `Mutex`
  (`Connection` es `Send` pero no `Sync`) y la caché de análisis. El adaptador de
  `AudioRender` (≈50 líneas en `app`) renderiza con `RenderOptions.threads` acotado
  (2 como máximo) y prioridad baja, para no quitarle núcleos a la reproducción.
- Los stubs (demo, capturas, tests) implementan `SampleLibrary` sobre su catálogo
  en memoria vía un método nuevo `LibraryBridge::assistant_samples()`.

### 6.5 Turno en la app

Cambios en `crates/ai/src/llm/cli/`:
- `cli::start(spec, runner: Box<dyn ToolRunner>)`. `trait ToolRunner: Send` con
  `fn call(&mut self, name: &str, input: Value, events: &Sender<CliEvent>, cancel: &CancelToken) -> (String, bool)`.
  `claude_code::run` y `codex::run` llaman a `runner.call` donde hoy llaman a
  `call_tool` (la función y `CliEvent::Tool { reply }` se eliminan). Los tests de
  `claude_code::mcp()` no cambian: su cierre `ToolCall` sigue igual.
- `CliEvent` nuevo: `Text`, `Thinking`, `ToolStart(name)` (el modelo empezó a
  escribir la llamada), `ToolCall { name, input }` (empieza a ejecutarse),
  `ToolProgress(String)`, `ToolDone { text, ok }`, `Ask { question, reply }`
  (solo `ask_user`) y `Done(Result<(), String>)`.
- `TurnRunner` (en `runtime/turn.rs`) implementa `ToolRunner`: emite `ToolCall`,
  ejecuta `Toolbox::execute` con su `ToolCtx`, emite `ToolDone`. Para `ask_user` envía
  `Ask` y bloquea esperando la respuesta, comprobando la cancelación cada 100 ms
  (como hace hoy `call_tool`).
- Las pruebas vivas de `cli/mod.rs` (`live_*`) hoy responden a `CliEvent::Tool`; se
  adaptan para usar un `ToolRunner` de prueba. Lo que comprueban no cambia.

Cambios en `crates/app/src/views/ai_panel/agent.rs`:
- `Turn` guarda el `SharedSession` y el `CliTurn`. `poll_cli` solo traduce eventos a
  pasos del chat (`begin_tool`, `settle_tool`, tarjeta de pregunta). `run_tool`,
  `generate_part` y la rama `Engine::Api` (con `poll_api`, `step_done`, `MAX_ROUNDS`)
  se eliminan. El constructor comprobará que nada más use la API antes de borrarla.
- Parar (`stop`): cancela, toma el borrador con `take()` y ofrece lo ya cambiado,
  igual que hoy. Una pregunta abierta se cierra soltando su `reply`.
- El progreso (`ToolProgress`) se guarda en el estado del agente como
  `(item, paso) → texto`, **no** en `ToolState`, así el formato guardado no cambia.

### 6.6 MCP sobre el mismo runtime

El broker deja de ejecutar herramientas. Pasa a tres fases, y solo dos van por el
hilo de la UI (cada salto cuesta como mucho un tick de 33 ms):

1. `Broker::begin(project, history, selected, client, name) -> Checkout`
   (UI, rápida): resuelve el borrador vivo o crea uno desde el proyecto, y devuelve
   su `SharedSession` y los `Services` del momento.
2. El hilo bloqueante del servidor ejecuta `Toolbox::execute`. Las llamadas de un
   mismo servidor se serializan con un `Mutex` para no mezclar borradores.
3. `Broker::commit(project, history, checkout, outcome) -> Reply`
   (UI, rápida): reserva los ids del borrador, reemplaza la propuesta, aplica en
   directo si está activado (y no es destructivo) y devuelve el `Effect` actual.
   Las herramientas de lectura no hacen commit.

`Broker::call` se conserva como composición síncrona de las tres fases para los
tests del broker. Esos tests, el `Fake` de `server_tests.rs` (que hoy pasa un
`delegate` y reconoce `Outcome::Delegate`) y los de `mcp.rs` se **adaptan** a la API
nueva; el comportamiento que prueban (propuesta, reemplazo, aplicar en directo,
destructivo siempre pide permiso, ids reservados, "el proyecto cambió debajo") no
cambia. `AppGateway` y `ChannelGateway` pasan a ofrecer `begin`/`commit` como
peticiones a la UI. `ANSWER_WITHIN` (60 s) solo cubre ya los saltos a la UI.

Las herramientas largas llegan al MCP con el mismo presupuesto de tiempo de pared
que en el chat (§6.8), recortado a 20 s para clientes externos, y sin progreso (el
MCP actual no lo transmite). No hay origen de cancelación para el MCP en esta fase.

### 6.7 Herramientas nuevas (esquemas orientativos)

Todas devuelven texto compacto, con tope de tamaño (≤ 25 resultados, ≤ 4 000
caracteres por llamada salvo que el constructor justifique otro) y sin rutas absolutas.

**Samples (pieza 1)**
- `library_overview {}` → lo que el usuario tiene: totales por origen y categoría,
  tonalidades y rangos de BPM que cubre. Se pide al empezar un encargo creativo.
- `search_samples { query?, category?, key?, key_match?, bpm?, loops_only?, one_shots_only?, pack?, favorites?, similar_to?, meaning?, include_online?, limit? }`
  → tarjetas con `ref`. `meaning: true` usa búsqueda por significado (CLAP) si el
  modelo está instalado; si no, cae a palabras clave y **lo dice en el resultado**.
  *(La búsqueda semántica usa hoy el worker de la paleta; el constructor decide si
  se comparte por canal o se difiere. No bloquea el resto.)*
- `analyze_sample { sample | clip, deep? }` (`clip` es un clip de audio del proyecto) → tonalidad (con candidatas y
  confianza), nota raíz, BPM y fiabilidad, compases, bucle/one-shot, categoría,
  volumen (LUFS, pico, RMS), brillo (centroide), ataque/decay, y reparto bajo/medio/agudo.
- `get_waveform { sample | clip, buckets? }` → picos y RMS por tramos (dBFS),
  transitorios y silencios: "la forma de onda" para razonar sobre ella.
- `place_sample { sample, track?, start_bar, length_bars?, fit?, replace?, name? }` →
  coloca el clip en el borrador por `Command`s (pista de audio nueva o existente),
  ajustado a la tonalidad y al tempo del proyecto con `fit` (por defecto activo).
  Valida el `ref` y explica el ajuste aplicado y si el warp quedó desactivado por
  BPM poco fiable. Semántica, igual que `write_clip`:
  - `replace` (por defecto `true`) retira los clips que se solapan, y eso es una
    edición destructiva: la propuesta pide aprobación y no se aplica sola.
  - `length_bars` en un **bucle**: si es mayor que el bucle ajustado, lo repite
    seguido (como `copy_clip`, con la última repetición recortada); si es menor, lo
    recorta. Nunca lo estira más allá de lo que fija el warp.
  - Un **one-shot** suelto se coloca como un solo clip en su compás (un efecto, un
    impacto). Las baterías con muchos golpes **no** se montan así: ver `build_drum_kit`.

**Baterías con one-shots (fase 3b)**
- `build_drum_kit { track?, name?, pads: [{ sample, role?, note?, gain_db?, choke? }] }`
  → crea (o amplía, si `track` ya tiene un Drum Rack) un Drum Rack cuyos pads son
  `instrument.sampler` en modo de un disparo, cada uno con el one-shot indicado, en
  la nota que le toca (por defecto la del mapa General MIDI que ya usa la persona:
  36 kick, 38 snare, 39 clap, 42 hat cerrado, 46 hat abierto…, según `role`). Los
  hats comparten grupo de *choke*. Después, `write_clip` y `generate_part` escriben el
  MIDI como ya hacen con cualquier pista de batería (`pick_target` y `is_drum_track`
  la reconocen), así que el resultado queda editable y con velocidades.
- Se apoya en cosas que ya existen: `Command::AddDevice` con un `Rack` de cadenas
  (`Chain { note, key_range, choke, devices }`), como hacen el demo (`demo.rs`) y
  Bloom (`drum_rack_commands`), y el estado del Sampler, que guarda la ruta del
  archivo en `{"version":1,"params":{},"extra":{"path":"…"}}`. Se añade en
  `aitrack-stock` un ayudante público que construye ese estado (hoy solo lo escribe
  el propio Sampler al guardarse), para no repetir el JSON a mano.
- La herramienta vive en `aitrack-ai` (depende de `stock` y `model`); las rutas y el
  rechazo de muestras solo en la nube vienen de `probe_for_placement` (§6.4), igual
  que en `place_sample`.
- Variante opcional que comparte código: un Sampler de una sola nota para one-shots
  tonales (un bajo, un 808), con la nota raíz tomada del análisis. Lo decide el
  constructor.

**Oídos (pieza 2)** — se ejecutan sobre el **borrador**, así el asistente mide
*después* de editar:
- `measure_mix { bars?, range?, include_third_party? }` y
  `measure_track { track, bars?, include_third_party? }` → ver §6.8.

Las herramientas existentes no cambian de nombre ni de esquema.

### 6.8 Mediciones

Nuevo módulo `crates/ai/src/measure/`, DSP puro sobre `AudioBuffer` (sin E/S):

- **Volumen:** integrado (LUFS, con puertas), a corto plazo (3 s) máximo,
  momentáneo máximo, rango de volumen (LRA), pico de muestra y **pico real**
  (sobremuestreo ×4, BS.1770-4), RMS, factor de cresta y PLR. Reutiliza y verifica
  `features::loudness`.
- **Espectro:** energía por bandas (sub 20–60, bajo 60–120, bajo-medio 120–250,
  medio-bajo 250–500, medio 0,5–2 k, presencia 2–4 k, brillo 4–8 k, aire 8–16 k Hz)
  en dB relativos, y banderas simples (exceso/falta de graves, barro en 200–400 Hz).
- **Estéreo:** correlación, relación mid/side total y por banda, y energía lateral
  por debajo de 120 Hz (compatibilidad mono).
- **Tiempo:** volumen a corto plazo por compás (la curva de energía de la canción).
- **Por pista:** `render` con `stems = [pistas]` y las mismas medidas, para comparar
  balance entre pistas y solapamientos en graves.

Rango por defecto: la región de bucle si existe; si no, los compases 1–33. Tope de
90 s de audio por llamada salvo `range` explícito y mayor. El resultado indica
**siempre qué rango se midió**. El render comprueba la cancelación en cada bloque y
reporta progreso (`ToolProgress`).

**Presupuesto de tiempo de pared.** Además del tope de audio, cada llamada tiene un
presupuesto de 25 s (20 s para clientes MCP), porque un borrador con muchos
dispositivos puede renderizar más lento que tiempo real y cada llamada vuelve a
decodificar el audio e instanciar los dispositivos. Si se agota, el render se corta,
se miden los compases ya renderizados y el resultado lo dice ("medidos los compases
1–17 de 65; pide `range` para medir el resto"). No hay herramientas de "lanzar y
consultar": un presupuesto honesto y recortable evita el segundo modelo de
ejecución que descartamos.

**Plugins de terceros (VST3/CLAP).** Cargar un plugin alojado en un render offline
usa el hilo principal de la UI (`rt.main.run(LOAD_TIMEOUT, …)`), y el modo sandbox
está desactivado por defecto: es el único punto donde este diseño bloquearía la
ventana. **Decidido (§12):** por defecto el render **omite** los dispositivos de
terceros, y todo resultado lista cuáles ha omitido y marca la medición como parcial
("sin: ZL Equalizer 2 (VST3), Dragonfly Hall (CLAP)"); con `include_third_party: true`
se renderizan como Exportar, asumiendo una pausa de la ventana mientras cargan. La
persona le dice al asistente que, si la medición es parcial y la decisión depende de
esos plugins (sobre todo en mastering), avise al usuario y proponga repetirla
incluyéndolos.

**Formato del resultado.** Los números se devuelven con el contexto que un productor
necesita (p. ej. "−9,4 LUFS integrado, pico real −0,8 dBTP; objetivo club: −9…−7 LUFS,
techo −1 dBTP").

### 6.9 Prompt y contexto

- `AGENT_PERSONA` crece con el método del co-productor: pedir `library_overview` y
  buscar samples antes de componer; elegir sonidos que encajen (categoría, tonalidad,
  tempo); colocar y escribir por secciones; **medir antes y después** de cada
  decisión de mezcla o mastering y no fiarse de la intuición; explicar con números;
  seguir sin poder "oír" y decirlo cuando importe. Sigue siendo estática (la caché
  del proceso caliente depende de ella).
- La instantánea del proyecto que va en el mensaje (`producer_context`) añade una
  línea con lo que el usuario tiene (totales de librería) y la lista de nombres de
  plugins instalados. Sin rutas.

### 6.10 UI del chat

- Títulos, iconos y tarjetas para las herramientas nuevas en `ai_panel/steps.rs`
  (`tools::title` sale del registro). Iconos y colores desde `icons.rs`/`theme.rs`.
- Una herramienta en curso muestra su texto de progreso ("renderizando 64 %").
- Los resultados de `search_samples` pueden mostrarse como los chips de sample que
  el chat ya sabe pintar (`ChatEntry::Samples`); es un pulido opcional de la fase 5.

### 6.11 Retiradas

`Outcome::Delegate`, `DELEGATED`, `call_tool` (cli), `CliEvent::Tool { reply }`,
`Engine::Api` y sus funciones, `generate_part` como función de la app, y la tabla
`title()`/`is_read_only` por prefijo (sustituidas por `ToolDef`).

## 7. Entrega por fases

Cada fase deja la app funcionando y con tests en verde; ninguna cambia el formato guardado.

0. **Cimientos sin cambio de comportamiento.** Mover `midi_gen` e `is_drum_track`;
   `ToolDef` con tipo y título; retirar `Engine::Api`. *Verifica:* los tests
   actuales de `ai`, `mcp` y `app` pasan igual; el chat se comporta igual.
1. **Runtime en hilo propio (chat).** `SharedSession`, `ToolRunner`/`TurnRunner`,
   `CliEvent` nuevo, `poll_cli` reducido, progreso, parada y preguntas.
   *Verifica:* tests con una herramienta falsa lenta (el hilo de la UI sigue
   leyendo eventos mientras tanto), de parada con herramienta en vuelo y de
   `ask_user`; prueba viva `--ignored` con un turno real de cada CLI y una
   herramienta de 45 s (confirma que ni Claude Code ni Codex agotan el tiempo de
   la herramienta: Claude Code ya lleva `MCP_TOOL_TIMEOUT`; Codex hay que medirlo).
2. **MCP sobre el mismo runtime.** Broker en tres fases, `ChannelGateway`.
   *Verifica:* los tests del broker y de `server_tests.rs` pasan; `docs/mcp.md` al día.
3. **Servicios y herramientas de samples.** `library::place` (extracción),
   `SampleLibrary` en `library` y su cableado, `LibraryBridge::assistant_samples`,
   las cinco herramientas (`library_overview`, `search_samples`, `analyze_sample`,
   `get_waveform`, `place_sample`), y la persona. *Verifica:* el test de arrastre
   con ajuste sigue verde; tests con una librería falsa y con una muestra solo en
   la nube (se rechaza sin abrir el archivo); prueba viva con el banco de fábrica
   real (solo existe en el checkout principal; probar sobre una copia).
   **3b. Baterías con one-shots.** Ayudante público en `aitrack-stock` para el
   estado del Sampler, herramienta `build_drum_kit` y su entrada en la persona.
   *Verifica:* test de ida y vuelta (se construye un kit, `pad_sample(chain)` devuelve
   cada ruta, `is_drum_track` es verdadero y `generate_part` escribe un ritmo sobre
   esa pista con las notas de los pads); una muestra solo en la nube se rechaza; el
   Drum Rack resultante se ve bien en la interfaz (captura oculta con el proyecto demo).
4. **Oídos.** `measure`, `AudioRender` y su adaptador, `measure_mix`/`measure_track`.
   *Verifica:* calibración con señales sintéticas de valor conocido (p. ej. seno
   estéreo de 997 Hz a −23 dBFS ⇒ −23,0 LUFS ±0,1 según EBU Tech 3341) y con el
   renderer real sobre el proyecto demo.
5. **Co-productor.** Persona y contexto completos, tarjetas de la UI, pruebas vivas
   de extremo a extremo ("hazme un drop de house de 16 compases con mis samples y
   mide la mezcla").

Las fases 0 a 2 son secuenciales (tocan los mismos ficheros). La 3 y la 4 son
independientes entre sí tras la 1 y pueden ir en paralelo.

## 8. Pruebas y verificación

- Unitarias junto al código (`#[cfg(test)] mod tests`): registro de herramientas
  (toda definición tiene tipo, título y manejador), `TurnRunner` con canales,
  broker de tres fases (portar los tests actuales), `place` (golden contra el
  comportamiento actual de arrastrar), `measure` (señales sintéticas),
  `SampleLibrary` falsa.
- Tests de concurrencia: una herramienta de 300 ms no retrasa `poll`; `take()` con
  una herramienta en vuelo no se bloquea ni entra en deadlock.
- Pruebas vivas `--ignored` (consumen suscripción, pocas y pequeñas): turno real con
  herramienta lenta; turno real que llama a `search_samples` contra el banco de fábrica.
- Comprobación visual de las tarjetas con la receta de capturas ocultas
  (`AITRACK_DEV_HIDDEN=1`, `AITRACK_STUBS=1`), sin ventanas emergentes.
- Antes de cada entrega: `cargo check --workspace`, `cargo test -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app`
  y `cargo clippy` de los mismos con `-D warnings`.

## 9. Riesgos

- **SQLite entre hilos.** Las conexiones se abren por hilo o tras un `Mutex`; nunca se
  comparten sin protección. Mitigación: el adaptador las abre al primer uso, como ya
  hace `sound_searcher`.
- **Candado del borrador.** Un candado largo bloquearía parada y ofertas. Mitigación:
  la regla de §6.2 y un test.
- **Plugins de terceros en el render.** Cargarlos usa el hilo principal de la UI y
  puede tardar o fallar. Mitigación: por defecto se omiten y se avisa (§6.8); con
  `include_third_party` usa el mismo `export_factory()` que Exportar. Un fallo de
  carga hoy solo va a `rt.message`: el adaptador envuelve la fábrica para recoger
  qué dispositivos no cargaron y decirlo en el resultado.
- **Render compitiendo con la reproducción.** Mitigación: hilos acotados y prioridad
  baja (§6.4) y presupuesto de tiempo de pared.
- **Muestras solo en la nube.** Abrirlas las descarga. Mitigación: las herramientas
  nunca las abren (§6.4) y hay un test que lo comprueba.
- **Tiempo de las herramientas en los clientes.** Claude Code y Codex tienen
  tiempos de espera propios, y los clientes MCP externos pueden tener los suyos.
  Mitigación: tope de 90 s de audio, presupuesto de 25 s de pared (20 s para MCP) con
  resultado parcial explícito (§6.8), progreso, y la prueba viva de la fase 1. Si esa
  prueba mostrara que Codex corta por debajo de 25 s, se baja el presupuesto de ese
  cliente; no se vuelve a un modelo de lanzar y consultar.
- **Contexto del modelo.** Resultados grandes lo llenan. Mitigación: topes por herramienta.
- **Coste de arranque.** Cambiar las herramientas cambia el hash de la caché del
  proceso caliente; solo afecta al primer turno tras actualizar.

## 10. Lo que queda al constructor

Nombres exactos de tipos y campos; el esquema JSON definitivo de cada herramienta
nueva; los bordes finos de bandas y umbrales de las banderas de `measure`; los topes
exactos de tamaño; la caché de análisis (tamaño y política); cómo compartir (o
diferir) la búsqueda semántica; los detalles de `build_drum_kit` (nombres de pad,
límite de pads, qué ocurre si la nota ya está ocupada, el modo exacto del Sampler);
el
aspecto de las tarjetas nuevas; el orden de las sub-tareas dentro de cada fase.

## 11. Puntos de extensión (fuera de este documento)

- **Pieza 3, plugins del usuario:** un `PluginTools` en el `Toolbox` y un servicio
  `plugins` en `Services` (catálogo del host, parámetros y presets de VST3/CLAP).
  Hoy `get_device` ya distingue plugin propio de plugin de terceros.
- **Pieza 4, guía de canción completa:** plan por secciones y presupuesto de turno
  sobre el mismo runtime; no necesita cambios de arquitectura.
- Ajuste a una pista de referencia (medir contra otra canción).
- Cargar samples en cadenas de Racks de instrumento más allá de las baterías, y en
  Wavetable/Granular.

## 12. Decisiones

### Decididas por el usuario (2026-11-19)

1. **Enfoque B** (§5): las herramientas se ejecutan fuera del hilo de la interfaz,
   con servicios inyectados, para el chat y para el MCP.
2. **Orden de entrega:** 1) samples y análisis, 2) oídos, 3) plugins del usuario,
   4) guía de canción completa. Este documento cubre la base y las piezas 1 y 2.
3. **Plugins de terceros en las mediciones:** se omiten por defecto y se avisa de
   cuáles; `include_third_party: true` los incluye aceptando una pausa de la ventana
   (§6.8).
4. **Baterías con one-shots:** `build_drum_kit` carga los one-shots en los pads de un
   Drum Rack con Sampler y el MIDI lo escriben `write_clip`/`generate_part` (§6.7,
   fase 3b).

### Decisiones tomadas por el autor (revisar al leer)

- Las herramientas se ejecutan en el hilo del turno, no en un hilo nuevo del agente (§5).
- El borrador es un `Arc<Mutex<Session>>` con candado solo para operaciones cortas (§6.2);
  no se usa `export_snapshot` (§6.2).
- `SampleLibrary` se implementa en `crates/library`, no en `app` (§6.4); `AudioRender`
  sí en `app`, porque `ai` no debe depender de `engine`.
- Las muestras solo en la nube se marcan y se rechazan: el asistente nunca las
  descarga (§6.4).
- `place_sample` reemplaza solapes por defecto, como `write_clip`, y por tanto exige
  aprobación del usuario (§6.7).
- Presupuesto de 25 s de pared por herramienta larga (20 s en MCP), con resultado
  parcial explícito, en vez de herramientas de lanzar y consultar (§6.8).
- Los renders del asistente usan como mucho 2 hilos y prioridad baja (§6.4).
- `midi_gen` pasa a `aitrack-ai` y `is_drum_track` a `aitrack-model` para poder
  ejecutar `generate_part` en el hilo del turno y retirar `Outcome::Delegate` (§6.3).
- La rama `Engine::Api` del agente se retira por inalcanzable (§6.5).
- `build_drum_kit` vive en `aitrack-ai` y usa un ayudante nuevo de `aitrack-stock` para
  el estado del Sampler; los pads son `instrument.sampler` en modo de un disparo, con
  las notas del mapa General MIDI de la persona y los hats en un mismo grupo de
  *choke* (§6.7).
