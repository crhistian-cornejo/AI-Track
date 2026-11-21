# Asistente co-productor, fases 0–2: plan de implementación

> **Para agentes:** SUB-SKILL OBLIGATORIA: usa superpowers:subagent-driven-development (recomendado) o superpowers:executing-plans para ejecutar este plan tarea por tarea. Los pasos usan casillas (`- [ ]`).

**Objetivo:** las herramientas del asistente (chat y MCP) se ejecutan fuera del hilo de la interfaz, sobre un único registro (`Toolbox`) y un borrador compartido (`SharedSession`), sin cambiar el comportamiento visible ni el formato guardado.

**Arquitectura:** la fase 0 deja `generate_part` como herramienta normal de `aitrack-ai` (mueve `midi_gen` y `is_drum_track`), sustituye `is_read_only`/`title()` por un registro `ToolDef` y retira `Engine::Api`. La fase 1 crea `crates/ai/src/llm/runtime/` (borrador compartido, `Toolbox`, `TurnRunner`) y hace que el hilo `aitrack-cli-turn` ejecute las herramientas; la UI solo pinta eventos. La fase 2 parte el broker del MCP en `begin` (UI) → `execute` (hilo del servidor) → `commit` (UI).

**Stack:** Rust 2024, `crossbeam-channel`, `serde_json`, `std::sync::Mutex` (aitrack-ai no tiene `parking_lot`), GPUI (`vendor/gpui-pre`, no se toca), `rmcp`.

**Diseño:** `docs/superpowers/specs/2026-11-19-asistente-co-productor-design.md` (aprobado el 2026-11-21). Se lee junto a este plan; las secciones citadas (§6.2, §6.5…) son de ese documento.

## Restricciones globales

- Toda edición del proyecto es un `Command` de `aitrack-model` aplicado al borrador (`Session::apply`) y ofrecido como propuesta; ningún hilo muta el `Project` real.
- Nada de asignaciones, locks ni E/S en `Processor::process` ni en el callback de audio (estas fases no tocan el motor).
- El candado de `SharedSession` solo se toma para operaciones cortas de CPU: aplicar comandos, leer, clonar el `Project` (§6.2).
- Ningún camino de herramienta bloquea el hilo de la UI más de ~16 ms (anti-objetivo §3).
- No cambia el formato serializado de `Step`/`ToolState` (`crates/app/src/views/ai_panel/agent.rs:37-55`) ni el del `.aitrack`.
- Las herramientas existentes no cambian de nombre, esquema ni orden: `definitions()` produce el mismo JSON (`name`, `description`, `input_schema`) en el mismo orden (la caché del proceso caliente usa su hash, `cli/mod.rs:241-250`).
- Colores de `crates/app/src/theme.rs`, iconos de `crates/app/src/icons.rs`, tiempos de `crates/app/src/motion.rs`; espaciado en múltiplos de 4 px.
- Tests junto al código, en `#[cfg(test)] mod tests` (o el `#[path]` que ya usa `server_tests.rs`).
- **NUNCA `cargo fmt`** (el repo está formateado a mano; reescribe 88 ficheros).
- En zsh, entrecomillar globs (`--include='*.rs'`).
- Capturas: siempre `AITRACK_DEV_HIDDEN=1`, y `AITRACK_STUBS=1` cuando no hace falta audio.
- Cierre de cada fase: `cargo check --workspace`; `cargo test -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app`; `cargo clippy -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app --all-targets -- -D warnings` (en la fase 0 se añade `-p aitrack-model` a test y clippy).
- Sin dependencias nuevas en estas fases.

## Review Focus

Entradas o condiciones que el diseño implica, que ningún test de "camino feliz" cubre, y que más probablemente muerdan a quien use la app. Cada una tiene su test en la tarea dueña.

1. **Parar con una herramienta en vuelo**: el usuario pulsa Parar mientras una herramienta tarda; Parar responde al instante, ofrece lo ya cambiado y lo que la herramienta escriba después no entra en la propuesta. → T1.1 (`take_with_a_tool_in_flight_does_not_wait_for_it`), T1.4 (`a_cancelled_turn_answers_stopped_and_late_edits_are_lost`).
2. **El usuario resuelve la propuesta MCP (o abre otro proyecto) entre `begin` y `commit`**: el resultado de la llamada no debe resucitar un borrador descartado ni proponerse sobre otro proyecto. → T2.1 (`a_draft_resolved_while_the_call_ran_is_not_revived`, `a_reset_while_the_call_ran_drops_its_edits`).
3. **Se borra el chat durante un turno** (`with_reply` devuelve `None`): hoy la herramienta contesta "The chat was cleared."; con el runtime el turno debe pararse, no seguir editando un borrador huérfano. → T1.5 (`a_turn_whose_reply_is_gone_is_stopped`, sobre la función pura `on_event`).
4. **Una pregunta abierta cuando el turno termina o se para**: el hilo del turno no puede quedarse colgado esperando la respuesta. → T1.4 (`an_open_question_unblocks_when_the_reply_is_dropped`).
5. **Chats guardados antiguos**: un `Step::Tool` serializado antes del cambio se sigue leyendo y pintando igual. → T1.5 (`saved_steps_read_the_same`).

---

## Mapa de ficheros

| Fichero | Fase | Qué cambia |
|---|---|---|
| `crates/model/src/project.rs` | 0 | `Track::is_drum()` (tras `can_freeze`, l. 441-444) y su test |
| `crates/ai/src/generate/context.rs` | 0 | el `is_drum_track` privado (l. 181) se sustituye por `Track::is_drum()` |
| `crates/ai/src/generate/part.rs` | 0 | **nuevo**: `midi_gen` movido (`Dest`, `Prepared`, `pick_target`, `region_start`, `free_start`, `prepare`, `commands`) con sus tests |
| `crates/ai/src/generate/mod.rs` | 0 | `pub mod part;` |
| `crates/app/src/views/ai_panel/midi_gen.rs` | 0 | **se borra** |
| `crates/app/src/state.rs`, `views/session.rs`, `views/arrangement.rs` | 0 | `is_drum_track(t)` → `t.is_drum()`; se borra `state::is_drum_track` (l. 2846) |
| `crates/app/src/views/ai_panel/{mod,state,part}.rs`, `views/device_ui/acid_gen.rs` | 0 | importan de `aitrack_ai::generate::part` |
| `crates/ai/src/llm/runtime/mod.rs`, `def.rs` | 0 | **nuevos**: `ToolDef`, `ToolKind` |
| `crates/ai/src/llm/mod.rs` | 0 | `pub mod runtime;` |
| `crates/ai/src/llm/tools/mod.rs` | 0 | `defs()`, `kind()`, `is_read()`, `title()` desde el registro; `generate_part` despachado; fuera `Outcome::Delegate`, `DELEGATED`, `is_read_only` |
| `crates/ai/src/llm/tools/part.rs` | 0 | **nuevo**: la herramienta `generate_part` (antes en `agent.rs:803-830`) |
| `crates/app/src/views/ai_panel/agent.rs` | 0, 1 | fuera `run_tool`, `generate_part`, `Engine::Api` (fase 0); turno sobre el runtime (fase 1) |
| `crates/app/src/views/ai_panel/steps.rs` | 0, 1 | `is_read_only` → `is_read`; texto de progreso (fase 1) |
| `crates/mcp/src/broker.rs` | 0, 2 | sin `Delegate` (fase 0); tres fases (fase 2) |
| `crates/mcp/src/server.rs`, `server_tests.rs`, `gateway.rs` | 0, 2 | anotaciones desde `ToolDef` (fase 0); `begin`/`commit` (fase 2) |
| `crates/app/src/views/mcp.rs`, `crates/app/src/mcp_host.rs` | 0, 2 | sin delegado (fase 0); `ChannelGateway` con `begin`/`commit` (fase 2) |
| `crates/ai/src/llm/runtime/{session,toolbox,turn}.rs` | 1 | **nuevos**: `SharedSession`; `Toolbox`, `ToolSet`, `ToolCtx`, `Services`, `ProjectTools`; `TurnRunner` |
| `crates/ai/src/llm/cli/{mod,claude_code,codex}.rs` | 1 | `ToolRunner`, `CliEvent` nuevo, `start(spec, runner)`, fuera `call_tool` |
| `docs/mcp.md` | 2 | la sección de arquitectura (l. 58) describe las tres fases |

## Carriles y paralelismo

Un carril es dueño exclusivo de sus ficheros mientras dura la tarea. Dos tareas en paralelo nunca comparten fichero.

| Carril | Ficheros |
|---|---|
| **MODEL** | `crates/model/src/project.rs`, `crates/ai/src/generate/context.rs` |
| **GEN** | `crates/ai/src/generate/{part,mod}.rs`, `crates/app/src/views/ai_panel/{midi_gen,mod,state,part}.rs`, `crates/app/src/views/device_ui/acid_gen.rs`, `crates/app/src/{state.rs,views/session.rs,views/arrangement.rs}` |
| **REG** | `crates/ai/src/llm/{mod.rs,runtime/mod.rs,runtime/def.rs}`, `crates/ai/src/llm/tools/mod.rs`, `crates/app/src/views/ai_panel/steps.rs`, `crates/mcp/src/{server.rs,broker.rs}` |
| **TOOLS** | `crates/ai/src/llm/tools/{mod,part}.rs`, `crates/mcp/src/{broker,server_tests}.rs`, `crates/app/src/views/{mcp.rs,ai_panel/agent.rs,ai_panel/mod.rs}` |
| **AGENT** | `crates/app/src/views/ai_panel/agent.rs` |
| **RT** | `crates/ai/src/llm/runtime/{session,toolbox,turn,mod}.rs` |
| **CLI** | `crates/ai/src/llm/cli/{mod,claude_code,codex}.rs` (+ una línea de `agent.rs` en T1.3, ver allí) |
| **STEPS** | `crates/app/src/views/ai_panel/steps.rs` |
| **MCP** | `crates/mcp/src/*`, `crates/app/src/{mcp_host.rs,views/mcp.rs}`, `docs/mcp.md` |

Orden y paralelismo (→ = depende de):

```
Fase 0:  T0.1 (MODEL) → T0.2 (GEN) ─┐
         T0.4 (REG) ────────────────┴→ T0.3 (TOOLS) → T0.5 (AGENT) → T0.6 cierre
         [T0.1+T0.2 en paralelo con T0.4]
Fase 1:  T1.1 (RT) → T1.2 (RT) ─┐
         T1.3 (CLI) ────────────┴→ T1.4 (RT) → T1.5 (AGENT) → T1.6 (STEPS) ∥ T1.7 (CLI) → T1.8 cierre
         [T1.1+T1.2 en paralelo con T1.3; T1.6 en paralelo con T1.7]
Fase 2:  T2.1 (MCP) → T2.2 (MCP) → T2.3 (MCP) ∥ T2.4 (docs) → T2.5 cierre
```

Las fases son secuenciales entre sí (§7). Dentro de cada tarea: test primero, ver que falla, implementar, ver que pasa, commit (solo si el dueño del repo pidió commits en la sesión; estilo de mensaje del repo: `área: frase en minúsculas`).

---

## Fase 0 · Cimientos sin cambio de comportamiento

### T0.1 · `Track::is_drum()` en el modelo · carril MODEL · paralela a T0.4

**Ficheros:**
- Modificar: `crates/model/src/project.rs` (`impl Track`, añadir tras `can_freeze`, l. 441-444; test en `mod tests`, l. 1429)
- Modificar: `crates/ai/src/generate/context.rs` (borrar `fn is_drum_track`, l. 181-183; sus usos en l. 251, 269, 285, 349 pasan a `t.is_drum()`)

**Interfaces:**
- Produce: `impl Track { pub fn is_drum(&self) -> bool }` — verdadero si algún dispositivo es `DeviceKind::Rack(r)` con `r.kind == RackKind::Drum` (mismo cuerpo que `crates/app/src/state.rs:2846`).

- [ ] **Paso 1: test que falla** en `crates/model/src/project.rs`, `mod tests`:

```rust
#[test]
fn a_track_is_a_drum_track_when_it_holds_a_drum_rack() {
    let mut t = Track::new(TrackId(1), TrackKind::Instrument, "Drums", Color(0));
    assert!(!t.is_drum());
    let mut d = Device::stock(DeviceId(2), "rack.instrument", "Instrument Rack");
    d.kind = DeviceKind::Rack(Rack::new(RackKind::Instrument));
    t.devices.push(d.clone());
    assert!(!t.is_drum(), "an instrument rack is not a kit");
    d.kind = DeviceKind::Rack(Rack::new(RackKind::Drum));
    t.devices.push(d);
    assert!(t.is_drum());
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-model a_track_is_a_drum_track` → FALLA al compilar (`no method named is_drum`).
- [ ] **Paso 3:** implementar `Track::is_drum` y cambiar `generate/context.rs` para usarlo.
- [ ] **Paso 4:** `cargo test -p aitrack-model a_track_is_a_drum_track` → PASA; `cargo test -p aitrack-ai generate::` → PASA (sin cambios de resultado).
- [ ] **Paso 5:** commit `model: Track::is_drum, shared by the pattern engine and the app`.

**Hecho:** el test pasa, `context.rs` ya no define `is_drum_track`, `cargo check --workspace` verde.

### T0.2 · `midi_gen` pasa a `aitrack_ai::generate::part` · carril GEN · → T0.1 · paralela a T0.4

**Ficheros:**
- Crear: `crates/ai/src/generate/part.rs` (contenido de `crates/app/src/views/ai_panel/midi_gen.rs`, con sus 12 tests)
- Modificar: `crates/ai/src/generate/mod.rs` (`pub mod part;` junto a los demás `pub mod`, l. 22-38)
- Borrar: `crates/app/src/views/ai_panel/midi_gen.rs`
- Modificar: `crates/app/src/views/ai_panel/mod.rs:15` (fuera `pub(crate) mod midi_gen;`), `state.rs:21`, `part.rs:15` y su test l. 246, `crates/app/src/views/device_ui/acid_gen.rs:217`
- Modificar: `crates/app/src/state.rs` (borrar `is_drum_track`, l. 2846; usos l. 439, 858, 877, 895, 898 → `t.is_drum()`), `crates/app/src/views/session.rs:7,247`, `crates/app/src/views/arrangement.rs:1331`

**Interfaces:**
- Consume: `Track::is_drum()` (T0.1).
- Produce (sin cambiar firmas, solo de ruta): `aitrack_ai::generate::part::{Dest, Prepared, pick_target, region_start, free_start, prepare, commands}`:
  - `pub fn prepare(p: &Project, selected: Option<TrackId>, selected_clip: Option<ClipId>, kind: Option<Kind>, text: &str) -> Result<Prepared, String>`
  - `pub fn commands(p: &mut Project, g: &Generated, dest: Dest, start: Tick) -> (Vec<Command>, Vec<String>)`
  - `pub fn free_start(p: &Project, track: Option<TrackId>, from: Tick, len: Tick) -> Tick`

- [ ] **Paso 1:** copiar el fichero a `generate/part.rs`; cambiar `use aitrack_ai::generate::…` por `use crate::generate::…` (`super::` donde convenga) y `is_drum_track(t)` por `t.is_drum()`; el doc del módulo deja de decir "this file only picks the target" en tono de app (dice lo mismo, ahora en `aitrack-ai`).
- [ ] **Paso 2:** `cargo test -p aitrack-ai generate::part` → PASAN los 12 tests movidos (`a_bass_is_one_proposal_that_leaves_the_project_until_accepted`, `drums_need_a_drum_rack_and_use_its_pads`, `a_clip_with_notes_is_replaced_only_when_asked`…). Son el test de esta tarea: el código movido se comporta igual.
- [ ] **Paso 3:** borrar el fichero de la app y apuntar los imports a `aitrack_ai::generate::part`; sustituir `is_drum_track` en la app.
- [ ] **Paso 4:** `cargo test -p aitrack-app ai_panel` → PASA (incluye `part.rs` tests que usan `prepare`); `cargo check --workspace` verde.
- [ ] **Paso 5:** commit `ai: the generated-part placement moves from the app to aitrack-ai`.

**Hecho:** no queda `midi_gen` ni `is_drum_track` en `crates/app` (`grep -rn "midi_gen\|is_drum_track" --include='*.rs' crates/app` vacío, salvo `generate_part` que sale en T0.3).

### T0.4 · Registro `ToolDef` · carril REG · paralela a T0.1–T0.2

**Ficheros:**
- Crear: `crates/ai/src/llm/runtime/mod.rs` (`pub mod def; pub use def::{ToolDef, ToolKind};`), `crates/ai/src/llm/runtime/def.rs`
- Modificar: `crates/ai/src/llm/mod.rs:10-20` (`pub mod runtime;`)
- Modificar: `crates/ai/src/llm/tools/mod.rs` (l. 92-126 y 210-311)
- Modificar: `crates/app/src/views/ai_panel/steps.rs` (l. 62, 214, 221, 327, 384), `crates/mcp/src/server.rs:289-305`, `crates/mcp/src/broker.rs:107`

**Interfaces:**
- Produce, en `runtime/def.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind { Read, Edit, Ask }

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub kind: ToolKind,
    /// MCP's destructiveHint (delete_track, clear_bars, remove_device).
    pub destructive: bool,
    pub description: &'static str,
    pub schema: Value,
}
impl ToolDef {
    /// `{ "name", "description", "input_schema" }`, exactly what `tools::tool()` builds today.
    pub fn to_json(&self) -> Value;
}
```

- Produce, en `tools/mod.rs`: `pub fn defs() -> Vec<ToolDef>` (las 23, mismo orden); `pub fn ask_user_def() -> ToolDef` (`kind: Ask`, título "Asking you"); `definitions()` y `agent_definitions()` siguen existiendo y devuelven `defs().iter().map(ToolDef::to_json)`; `pub fn kind(name: &str) -> Option<ToolKind>`; `pub fn is_read(name: &str) -> bool` (`kind == Some(Read)`); `pub fn title(name: &str) -> &'static str` (busca en el registro, "Working" si no está). Se borra `is_read_only`. `ask_user_definition()` pasa a `ask_user_def().to_json()` (o se renombra; su único uso está en este fichero).

- [ ] **Paso 1: test que falla** en `tools/mod.rs`, `mod tests`:

```rust
#[test]
fn the_registry_keeps_names_order_kinds_and_titles() {
    let names: Vec<&str> = defs().iter().map(|d| d.name).collect();
    assert_eq!(names, ["get_project", "get_track", "get_device", "get_notes", "list_plugins", "list_presets", "create_track", "update_track", "delete_track", "group_tracks", "add_device", "set_params", "load_preset", "remove_device", "set_send", "set_sidechain", "set_song", "write_clip", "generate_part", "copy_clip", "move_clip", "clear_bars", "set_automation"]);
    for d in defs().iter().chain([ask_user_def()].iter()) {
        assert!(!d.title.is_empty() && d.title != "Working", "{}", d.name);
        let j = d.to_json();
        assert_eq!(j.as_object().unwrap().len(), 3, "{}", d.name);
        assert_eq!(j["input_schema"]["type"], "object");
    }
    assert_eq!(kind("list_plugins"), Some(ToolKind::Read));
    assert_eq!(kind("generate_part"), Some(ToolKind::Edit));
    assert_eq!(kind(ASK_USER), Some(ToolKind::Ask));
    assert!(!is_read(ASK_USER), "the panel never folds a question into the lookups");
    assert_eq!(title("get_project"), "Reading the project");
    assert_eq!(title("nope"), "Working");
    let destructive: Vec<&str> = defs().iter().filter(|d| d.destructive).map(|d| d.name).collect();
    assert_eq!(destructive, ["delete_track", "remove_device", "clear_bars"]);
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-ai the_registry_keeps` → FALLA al compilar.
- [ ] **Paso 3:** crear `def.rs`; reescribir `definitions()` como `defs()` con los mismos textos y esquemas (cada `tool(name, desc, schema)` pasa a una `ToolDef` con su título de la tabla actual l. 98-126 y su `kind`: `Read` para los seis `get_`/`list_`, `Edit` para el resto); borrar la tabla `match` de `title()`. En `steps.rs`, `server.rs` y `broker.rs`: `is_read_only` → `is_read`; en `server.rs::tool_list` el `destructiveHint` sale de `d.destructive` y el `title` de `d.title` (se itera `tools::defs()`).
- [ ] **Paso 4:** `cargo test -p aitrack-ai llm::` → PASA (incluye `every_definition_is_handled`, `questions_read_their_options_and_answer_in_words`); `cargo test -p aitrack-mcp` → PASA (`handshake_and_tool_list_are_the_assistants_tools` comprueba nombres, `readOnlyHint` y `destructiveHint`); `cargo test -p aitrack-app ai_panel` → PASA.
- [ ] **Paso 5:** commit `ai: one tool registry with kind and title instead of name prefixes`.

**Hecho:** `grep -rn "is_read_only" --include='*.rs' crates` vacío; la lista MCP no cambia.

### T0.3 · `generate_part` es una herramienta más; fuera `Outcome::Delegate` · carril TOOLS · → T0.2, T0.4

**Ficheros:**
- Crear: `crates/ai/src/llm/tools/part.rs`
- Modificar: `crates/ai/src/llm/tools/mod.rs` (`mod part;`; `execute` despacha `"generate_part" => part::generate_part(s, input)`; fuera `Outcome::Delegate`, `DELEGATED`; test l. 412)
- Modificar: `crates/mcp/src/broker.rs` (fuera `pub type Delegate`, el parámetro `delegate` de `call` y su uso l. 98-105; `no_delegate` en tests)
- Modificar: `crates/mcp/src/server_tests.rs:26,33-36`, `crates/app/src/views/mcp.rs:48,60-64`
- Modificar: `crates/app/src/views/ai_panel/agent.rs` (borrar `generate_part` l. 803-825, `seed` l. 827-830, `run_tool` l. 578-587 — sus dos llamadas pasan a `match tools::execute(..) { Outcome::Ok(t) => (t, true), Outcome::Err(e) => (e, false) }`; imports `gen_core`, `Kind`, `midi_gen`)
- Modificar: `crates/app/src/views/ai_panel/mod.rs:10-11` (fuera el `pub(crate) use agent::generate_part`)

**Interfaces:**
- Consume: `aitrack_ai::generate::part::{prepare, commands}` (T0.2), `tools::refs::track` (`refs.rs:62`).
- Produce: `pub(super) fn generate_part(s: &mut Session, input: &Value) -> Result<String, String>` en `tools/part.rs` (cuerpo de `agent.rs:804-825`; la semilla sigue siendo `prepared.req.seed.unwrap_or_else(seed)` con `seed()` = `splitmix64(nanos)`); `pub enum Outcome { Ok(String), Err(String) }`; `Broker::call(&mut self, project, history, selected, client, name, input) -> Reply` (sin delegado).

- [ ] **Paso 1: test que falla** — en `tools/mod.rs`, sustituir la última línea de `mixing_moves_on_returns_groups_and_main` (`assert_eq!(…, Outcome::Delegate)`) y añadir:

```rust
#[test]
fn generate_part_writes_into_the_draft() {
    let mut s = Session::new(&Project::new("t"), None);
    ok(execute(&mut s, "create_track", &json!({ "name": "Bass" })));
    let out = ok(execute(&mut s, "generate_part", &json!({ "kind": "bass", "description": "rolling, 2 bars", "track": "Bass" })));
    assert!(out.starts_with("Bass part, 2 bars"), "{out}");
    let bass = s.draft.tracks.iter().find(|t| t.name == "Bass").unwrap();
    assert_eq!(bass.arrangement.len(), 1);
    assert_eq!(s.changes.len(), 2);
    assert!(matches!(execute(&mut s, "generate_part", &json!({ "kind": "beat" })), Outcome::Err(e) if e.contains("drum rack")));
}
```

y en `crates/mcp/src/broker.rs` tests:

```rust
#[test]
fn generate_part_runs_in_the_broker_itself() {
    let (mut p, mut h, mut b) = setup();
    call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" }));
    let r = call(&mut b, &mut p, &mut h, "generate_part", json!({ "kind": "bass", "track": "Bass" }));
    assert!(!r.is_error, "{}", r.text);
    let Effect::Offered(o) = r.effect else { panic!("{:?}", r.effect) };
    assert_eq!(o.summary.len(), 2);
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-ai generate_part_writes` → FALLA (`Outcome::Delegate` ≠ Ok).
- [ ] **Paso 3:** crear `tools/part.rs`, despachar en `execute`, borrar `Delegate`/`DELEGATED`; quitar el delegado del broker, de `server_tests.rs` (`b.call(p, h, None, client, name, &args)`; el `match` de `read_resource` pierde el brazo `Delegate`) y de `views/mcp.rs`; borrar `generate_part`/`run_tool` de la app.
- [ ] **Paso 4:** `cargo test -p aitrack-ai llm::tools` → PASA; `cargo test -p aitrack-mcp` → PASA (todos los de `broker.rs` y `server_tests.rs`); `cargo test -p aitrack-app` → PASA; `grep -rn "Delegate\|DELEGATED" --include='*.rs' crates` vacío.
- [ ] **Paso 5:** commit `ai: generate_part runs with the other tools; the delegated outcome is gone`.

**Hecho:** el MCP ejecuta `generate_part` sin la app; el chat lo ejecuta por `tools::execute` igual que hoy (mismo texto).

(T0.4 va antes que T0.3 en el texto porque puede empezar el primer día, en paralelo a T0.1–T0.2.)

### T0.5 · Retirar `Engine::Api` · carril AGENT · → T0.3

**Ficheros:**
- Modificar: `crates/app/src/views/ai_panel/agent.rs`

**Qué se borra:** `MAX_ROUNDS` (l. 33), `enum Engine` (l. 126-129), la rama `Provider::ClaudeApi` de `submit` (l. 458-474), `base64` (l. 503-513) y su test `base64_matches_the_standard_alphabet`, `api_messages` (l. 379-381), `poll_api` (l. 690-721), `step_done` (l. 723-763), el `match` de `poll` (l. 612-615) y de `finish` (l. 837-841), los imports `api`, `AgentEvent`, `AgentRequest`, `AgentStream`, `json`. `Turn` pasa a guardar `cli: CliTurn` en lugar de `engine`. `Effort` se sigue importando de `aitrack_ai::llm::agent` (es donde vive; ese módulo del crate `ai` **no** se borra, ver D13). `Provider::ClaudeApi` sigue existiendo en `aitrack-ai`.

- [ ] **Paso 1:** comprobar que nada más usa la API del agente: `grep -rn "llm::agent::\(spawn\|AgentRequest\|AgentEvent\|AgentStream\)\|api::spawn" --include='*.rs' crates/app` → solo `agent.rs`. Anotar el resultado en el mensaje de commit.
- [ ] **Paso 2: test** — `history_keeps_user_and_answers_in_turns` (l. 902-922) deja de llamar a `api_messages`; sus asserts sobre `m[0]["role"]`/`m[1]` pasan a `assert!(!h[0].1.is_empty() && h[0].0)` y `assert!(h[1].1.contains("2 change(s)"))`. Ejecutar `cargo test -p aitrack-app ai_panel::agent` antes de borrar → PASA (el test cambiado no depende de la API).
- [ ] **Paso 3:** borrar lo listado.
- [ ] **Paso 4:** `cargo test -p aitrack-app ai_panel::agent` → PASA; `cargo clippy -p aitrack-app --all-targets -- -D warnings` → sin avisos de código muerto.
- [ ] **Paso 5:** commit `app: drop the unreachable API engine from the assistant`.

**Hecho:** `agent.rs` solo conoce turnos de CLI; el selector de modelos no cambia (`PROVIDERS`, l. 98).

### T0.6 · Cierre de la fase 0

- [ ] `cargo check --workspace` → sin errores.
- [ ] `cargo test -p aitrack-model -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app` → `test result: ok` en todos.
- [ ] `cargo clippy -p aitrack-model -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app --all-targets -- -D warnings` → sin avisos.
- [ ] `git diff --stat main -- '*.aitrack' crates/model/src/format.rs` → vacío (formato guardado intacto).

**Hecho:** la app y el MCP se comportan igual; `generate_part` ya no depende de la app.

---

## Fase 1 · Runtime en hilo propio (chat)

### T1.1 · `SharedSession` · carril RT · paralela a T1.3

**Ficheros:**
- Crear: `crates/ai/src/llm/runtime/session.rs`
- Modificar: `crates/ai/src/llm/runtime/mod.rs` (`pub mod session; pub use session::SharedSession;`)

**Interfaces:**
- Produce:

```rust
#[derive(Clone)]
pub struct SharedSession(Arc<Mutex<Option<Session>>>);   // std::sync::Mutex
impl SharedSession {
    pub fn new(session: Session) -> Self;
    /// Run `f` on the draft. `None` once the session was taken (the turn ended).
    pub fn with<R>(&self, f: impl FnOnce(&mut Session) -> R) -> Option<R>;
    /// A copy of the draft project, for work done without the lock.
    pub fn snapshot(&self) -> Option<Project>;
    /// The session, leaving nothing behind; later `with` calls get `None`.
    pub fn take(&self) -> Option<Session>;
}
```

Candado envenenado: `lock().unwrap_or_else(|e| e.into_inner())`, como `cli/mod.rs:281`.

- [ ] **Paso 1: tests que fallan** en `session.rs`:

```rust
#[test]
fn edits_go_to_the_draft_until_it_is_taken() {
    let s = SharedSession::new(Session::new(&Project::new("t"), None));
    let n = s.with(|s| s.draft.tracks.len()).unwrap();
    let t = aitrack_model::Track::new(TrackId(900), aitrack_model::TrackKind::Instrument, "X", aitrack_model::Color(0));
    s.with(|s| s.apply("Add X", vec![Command::AddTrack { track: Box::new(t), index: 0 }])).unwrap().unwrap();
    assert_eq!(s.snapshot().unwrap().tracks.len(), n + 1);
    let taken = s.take().unwrap();
    assert_eq!(taken.changes, ["Add X"]);
    assert!(s.with(|_| ()).is_none() && s.snapshot().is_none() && s.take().is_none());
}

#[test]
fn take_with_a_tool_in_flight_does_not_wait_for_it() {
    let s = SharedSession::new(Session::new(&Project::new("t"), None));
    let worker = s.clone();
    let (started, go) = crossbeam_channel::bounded(1);
    let h = std::thread::spawn(move || {
        let draft = worker.snapshot().unwrap();   // the lock is released here
        started.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        worker.with(|s| s.changes.push(format!("late {}", draft.name)))
    });
    go.recv().unwrap();
    let t0 = std::time::Instant::now();
    assert!(s.take().is_some());
    assert!(t0.elapsed() < std::time::Duration::from_millis(50));
    assert_eq!(h.join().unwrap(), None, "the late edit reaches no one");
}

#[test]
fn the_session_crosses_threads() {
    fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<SharedSession>();
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-ai runtime::session` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-ai runtime::session` → 3 PASAN.
- [ ] **Paso 5:** commit `ai: a shared draft the turn thread edits and the UI takes`.

**Hecho:** tests verdes; si `Session` no fuera `Send`, se para y se anota (no se espera: el `Project` ya viaja a hilos de fondo en freeze/bounce).

### T1.2 · `Toolbox`, `ToolSet`, `ToolCtx`, `Services`, `ProjectTools` · carril RT · → T1.1 · paralela a T1.3

**Ficheros:**
- Crear: `crates/ai/src/llm/runtime/toolbox.rs`
- Modificar: `crates/ai/src/llm/runtime/mod.rs` (re-exportar `Toolbox, ToolSet, ToolCtx, Services, ProjectTools`)

**Interfaces:**
- Consume: `SharedSession` (T1.1), `ToolDef`/`ToolKind` (T0.4), `tools::{defs, ask_user_def, execute, Outcome}`.
- Produce:

```rust
/// What the tools may reach besides the draft. Fields arrive in phases 3 and 4 (D4).
#[derive(Clone, Default)]
pub struct Services {}

pub struct ToolCtx<'a> {
    pub session: &'a SharedSession,
    pub services: &'a Services,
    pub cancel: &'a CancelToken,
    pub progress: &'a dyn Fn(String),
    /// Wall-clock budget of one call: 25 s in the chat, 20 s for MCP clients (D5).
    pub budget: Duration,
}

pub trait ToolSet: Send + Sync {
    fn definitions(&self) -> Vec<ToolDef>;
    fn execute(&self, ctx: &ToolCtx<'_>, name: &str, input: &Value) -> Outcome;
}

/// The 23 project tools: `tools::execute` on the draft, under the lock.
pub struct ProjectTools;

pub struct Toolbox { /* sets + name → index */ }
impl Toolbox {
    pub fn new(sets: Vec<Box<dyn ToolSet>>) -> Self;
    /// ProjectTools today; SampleTools and EarsTools join in phases 3 and 4.
    pub fn standard() -> Self;
    pub fn definitions(&self) -> Vec<ToolDef>;          // stable order, no ask_user
    pub fn agent_definitions(&self) -> Vec<ToolDef>;    // + tools::ask_user_def()
    pub fn kind(&self, name: &str) -> Option<ToolKind>;
    pub fn execute(&self, ctx: &ToolCtx<'_>, name: &str, input: &Value) -> Outcome;
}
pub const CHAT_BUDGET: Duration = Duration::from_secs(25);
pub const MCP_BUDGET: Duration = Duration::from_secs(20);
```

`Toolbox::execute`: nombre desconocido o `ASK_USER` → `Outcome::Err(format!("unknown tool \"{name}\""))` / `Err("ask_user is answered by the app")`; sesión ya tomada (`with` → `None`) → `Outcome::Err("The turn ended before this ran.".into())`. `Toolbox::new` hace `debug_assert!` de que ningún nombre se repite.

- [ ] **Paso 1: tests que fallan** en `toolbox.rs`:

```rust
fn ctx_for<'a>(s: &'a SharedSession, sv: &'a Services, c: &'a CancelToken, p: &'a dyn Fn(String)) -> ToolCtx<'a> {
    ToolCtx { session: s, services: sv, cancel: c, progress: p, budget: CHAT_BUDGET }
}

#[test]
fn every_definition_has_a_kind_a_title_and_a_handler() {
    let tb = Toolbox::standard();
    let names: Vec<&str> = tb.definitions().iter().map(|d| d.name).collect();
    assert_eq!(names, tools::defs().iter().map(|d| d.name).collect::<Vec<_>>(), "same tools, same order");
    assert_eq!(tb.agent_definitions().last().unwrap().name, tools::ASK_USER);
    let (s, sv, c) = (SharedSession::new(Session::new(&Project::new("t"), None)), Services::default(), CancelToken::new());
    for d in tb.definitions() {
        assert!(tb.kind(d.name).is_some() && !d.title.is_empty());
        let out = tb.execute(&ctx_for(&s, &sv, &c, &|_| {}), d.name, &json!({}));
        assert!(!matches!(&out, Outcome::Err(e) if e.starts_with("unknown tool")), "{}", d.name);
    }
    assert!(matches!(tb.execute(&ctx_for(&s, &sv, &c, &|_| {}), "nope", &json!({})), Outcome::Err(e) if e.starts_with("unknown tool")));
}

#[test]
fn a_taken_session_refuses_further_calls() {
    let tb = Toolbox::standard();
    let (s, sv, c) = (SharedSession::new(Session::new(&Project::new("t"), None)), Services::default(), CancelToken::new());
    s.take();
    assert!(matches!(tb.execute(&ctx_for(&s, &sv, &c, &|_| {}), "create_track", &json!({ "name": "A" })), Outcome::Err(e) if e.contains("turn ended")));
}

#[test]
fn the_json_definitions_are_what_the_clis_got_before() {
    let tb = Toolbox::standard();
    let now: Vec<Value> = tb.agent_definitions().iter().map(ToolDef::to_json).collect();
    assert_eq!(now, tools::agent_definitions(), "the warm-process hash does not move");
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-ai runtime::toolbox` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-ai runtime::` → PASAN (5 con los de T1.1).
- [ ] **Paso 5:** commit `ai: the toolbox, one registry and one way to run a tool`.

**Hecho:** tests verdes; `tools::execute` no cambia.

### T1.3 · `ToolRunner` y el `CliEvent` nuevo · carril CLI · paralela a T1.1–T1.2

**Ficheros:**
- Modificar: `crates/ai/src/llm/cli/mod.rs` (l. 197-215 `CliEvent`; l. 321-350 `start`; l. 352-366 `call_tool`; tests l. 412-478)
- Modificar: `crates/ai/src/llm/cli/claude_code.rs:10,192-195`, `crates/ai/src/llm/cli/codex.rs:10,173-178`
- Modificar (una sola línea y un brazo de `match`, sin más lógica): `crates/app/src/views/ai_panel/agent.rs` en `submit` (`cli::start(spec)` → `cli::start(spec, Box::new(cli::ReplyRunner))`) y en `poll_cli` (brazo vacío para las variantes nuevas). Nadie más toca `agent.rs` mientras dura esta tarea.

**Interfaces:**
- Produce:

```rust
pub trait ToolRunner: Send {
    /// Run one tool call; the text for the model and whether it is an error.
    fn call(&mut self, name: &str, input: Value, events: &Sender<CliEvent>, cancel: &CancelToken) -> (String, bool);
}

#[derive(Debug)]
pub enum CliEvent {
    Text(String),
    Thinking(String),
    ToolStart(String),
    ToolCall { name: String, input: Value },
    ToolProgress(String),
    ToolDone { text: String, ok: bool },
    /// Only `ask_user`: the turn waits for the answer text on `reply`.
    Ask { question: Question, reply: Sender<String> },
    /// Transitional (T1.3 → T1.7): the old reply-channel call, sent only by `ReplyRunner`.
    Tool { name: String, input: Value, reply: Sender<(String, bool)> },
    Done(Result<(), String>),
}

/// Transitional runner with today's behaviour: hands the call to the app and waits (T1.7 removes it).
pub struct ReplyRunner;

pub fn start(spec: TurnSpec, runner: Box<dyn ToolRunner>) -> CliTurn;
```

`claude_code::run` y `codex::run` reciben `runner: &mut dyn ToolRunner`. En Claude Code el cierre de `mcp()` es `|name, input| if cancel.is_cancelled() { None } else { Some(runner.call(name, input, tx, cancel)) }` (el tipo `ToolCall<'a>` de `claude_code.rs:17` no cambia: los tests de `mcp()` siguen igual). En Codex, `let (text, err) = runner.call(name, args, tx, cancel)`. `call_tool` pasa a ser el cuerpo de `ReplyRunner::call` (con `unwrap_or_else(|| ("Stopped by the user.".into(), true))`).

- [ ] **Paso 1: test que falla** en `cli/mod.rs`, `mod tests`:

```rust
struct Echo(Vec<String>);
impl ToolRunner for Echo {
    fn call(&mut self, name: &str, input: Value, events: &Sender<CliEvent>, _: &CancelToken) -> (String, bool) {
        let _ = events.send(CliEvent::ToolCall { name: name.into(), input: input.clone() });
        self.0.push(name.into());
        (format!("ran {name}"), false)
    }
}

#[test]
fn the_claude_code_mcp_answer_comes_from_the_runner() {
    let (tx, rx) = unbounded();
    let cancel = CancelToken::new();
    let mut r = Echo(Vec::new());
    let tools = vec![serde_json::json!({"name":"get_project","description":"d","input_schema":{"type":"object","properties":{}}})];
    let mut call = |n: &str, i: Value| Some(r.call(n, i, &tx, &cancel));
    let res = claude_code::mcp(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_project","arguments":{}}}), &tools, &mut call);
    assert_eq!(res["result"]["content"][0]["text"], "ran get_project");
    assert!(matches!(rx.try_recv(), Ok(CliEvent::ToolCall { name, .. }) if name == "get_project"));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-ai cli::tests::the_claude_code_mcp_answer` → FALLA al compilar.
- [ ] **Paso 3:** implementar; adaptar `live()` (l. 427-450) a `start(live_spec(..), Box::new(ReplyRunner))` sin cambiar lo que comprueba; cambiar las dos líneas de `agent.rs`.
- [ ] **Paso 4:** `cargo test -p aitrack-ai llm::cli` → PASA (incluye `mcp_answers_initialize_list_call_and_notifications` sin cambios); `cargo check --workspace` verde; el chat funciona igual (sigue por `CliEvent::Tool`).
- [ ] **Paso 5:** commit `ai: the CLI turn runs tools through a ToolRunner`.

**Hecho:** `call_tool` ya no existe como función libre; `cargo test -p aitrack-app ai_panel` verde.

### T1.4 · `TurnRunner` · carril RT · → T1.2, T1.3

**Ficheros:**
- Crear: `crates/ai/src/llm/runtime/turn.rs`
- Modificar: `crates/ai/src/llm/runtime/mod.rs` (`pub use turn::TurnRunner;`)

**Interfaces:**
- Consume: `ToolRunner`, `CliEvent` (T1.3); `Toolbox`, `ToolCtx`, `Services`, `CHAT_BUDGET` (T1.2); `SharedSession` (T1.1); `tools::{ASK_USER, Question}`.
- Produce:

```rust
pub struct TurnRunner { /* toolbox: Arc<Toolbox>, session: SharedSession, services: Services, budget: Duration */ }
impl TurnRunner {
    pub fn new(toolbox: Arc<Toolbox>, session: SharedSession, services: Services) -> Self;   // budget = CHAT_BUDGET
}
impl ToolRunner for TurnRunner { /* … */ }
```

Comportamiento de `call` (§6.5):
1. Si `cancel` ya está activo → `("Stopped by the user.", true)` sin emitir nada.
2. Emite `ToolCall { name, input }`.
3. `ASK_USER`: `Question::from_input`; si falla → emite `ToolDone { text: e, ok: false }` y devuelve `(e, true)`. Si no, emite `Ask { question, reply }` y espera `answer.recv_timeout(100 ms)` en bucle comprobando `cancel`; respuesta → `(texto, false)` **sin** `ToolDone` (la UI asienta la línea con lo elegido, D7); `Disconnected` o cancelado → `("The question was closed.", true)` sin `ToolDone`.
4. Otra herramienta: `progress` = cierre que envía `ToolProgress(text)`; `Toolbox::execute`; emite `ToolDone { text, ok }`; devuelve `(text, !ok)`.

- [ ] **Paso 1: tests que fallan** en `turn.rs` (con un `ToolSet` falso `Slow` de un solo nombre `"slow_read"`, `kind: Read`, que llama a `ctx.session.snapshot()`, informa `progress("half")`, duerme 300 ms comprobando `ctx.cancel` cada 10 ms y escribe `ctx.session.with(|s| s.changes.push("slow".into()))`):

```rust
#[test]
fn a_slow_tool_runs_on_the_turn_thread_while_events_flow() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let session = SharedSession::new(Session::new(&Project::new("t"), None));
    let mut r = TurnRunner::new(Arc::new(Toolbox::new(vec![Box::new(Slow)])), session.clone(), Services::default());
    let h = std::thread::spawn(move || r.call("slow_read", json!({}), &tx, &CancelToken::new()));
    let t0 = std::time::Instant::now();
    assert!(matches!(rx.recv_timeout(Duration::from_millis(100)), Ok(CliEvent::ToolCall { name, .. }) if name == "slow_read"));
    assert!(matches!(rx.recv_timeout(Duration::from_millis(100)), Ok(CliEvent::ToolProgress(p)) if p == "half"));
    assert!(t0.elapsed() < Duration::from_millis(150), "the poller is not held up");
    assert!(matches!(rx.recv_timeout(Duration::from_secs(2)), Ok(CliEvent::ToolDone { ok: true, .. })));
    assert_eq!(h.join().unwrap().1, false);
    assert_eq!(session.take().unwrap().changes, ["slow"]);
}

#[test]
fn a_cancelled_turn_answers_stopped_and_late_edits_are_lost() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let session = SharedSession::new(Session::new(&Project::new("t"), None));
    let cancel = CancelToken::new();
    let mut r = TurnRunner::new(Arc::new(Toolbox::new(vec![Box::new(Slow)])), session.clone(), Services::default());
    let c = cancel.clone();
    let h = std::thread::spawn(move || r.call("slow_read", json!({}), &tx, &c));
    std::thread::sleep(Duration::from_millis(50));
    cancel.cancel();
    let taken = session.take().unwrap();
    assert!(taken.changes.is_empty());
    let (_, is_error) = h.join().unwrap();
    assert!(is_error || session.with(|_| ()).is_none(), "the tool saw the cancel or wrote to a taken session");
}

#[test]
fn ask_user_waits_for_the_answer_and_bad_questions_fail_at_once() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut r = TurnRunner::new(Arc::new(Toolbox::standard()), SharedSession::new(Session::new(&Project::new("t"), None)), Services::default());
    let bad = r.call(tools::ASK_USER, json!({ "options": [] }), &tx, &CancelToken::new());
    assert!(bad.1);
    let _ = rx.try_recv(); // ToolCall
    assert!(matches!(rx.try_recv(), Ok(CliEvent::ToolDone { ok: false, .. })));
    let h = std::thread::spawn(move || r.call(tools::ASK_USER, json!({ "question": "Key?", "options": ["A minor"] }), &tx, &CancelToken::new()));
    let _ = rx.recv().unwrap(); // ToolCall
    let Ok(CliEvent::Ask { question, reply }) = rx.recv() else { panic!() };
    assert_eq!(question.question, "Key?");
    reply.send("The user answered \"A minor\".".into()).unwrap();
    assert_eq!(h.join().unwrap(), ("The user answered \"A minor\".".to_owned(), false));
    assert!(rx.try_recv().is_err(), "no ToolDone: the panel settles the question itself");
}

#[test]
fn an_open_question_unblocks_when_the_reply_is_dropped() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut r = TurnRunner::new(Arc::new(Toolbox::standard()), SharedSession::new(Session::new(&Project::new("t"), None)), Services::default());
    let h = std::thread::spawn(move || r.call(tools::ASK_USER, json!({ "question": "Key?", "options": ["A"] }), &tx, &CancelToken::new()));
    let _ = rx.recv().unwrap();
    let Ok(CliEvent::Ask { reply, .. }) = rx.recv() else { panic!() };
    drop(reply);
    let t0 = std::time::Instant::now();
    assert!(h.join().unwrap().1);
    assert!(t0.elapsed() < Duration::from_millis(250));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-ai runtime::turn` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-ai runtime::` → PASAN todos.
- [ ] **Paso 5:** commit `ai: TurnRunner runs the turn's tools on its own thread`.

**Hecho:** los cuatro tests verdes, sin `sleep` mayores de 300 ms en el test.

### T1.5 · La app usa el runtime · carril AGENT · → T1.4

**Ficheros:**
- Modificar: `crates/app/src/views/ai_panel/agent.rs`

**Interfaces:**
- Consume: `TurnRunner::new`, `Toolbox::standard`, `Services::default`, `SharedSession`, `CliEvent` (T1.3/T1.4).
- Produce:
  - `struct Turn { item, model_name, session: SharedSession, cli: CliTurn, tool_step: Option<usize>, _poll }`.
  - `Agent.progress: HashMap<(usize, usize), String>` (en el estado del agente, **no** en `ToolState`, §6.5) y `pub(super) fn progress(cx: &mut Context<Workspace>, item: usize, step: usize) -> Option<String>`.
  - `Pending.reply: Sender<String>` (antes `Sender<(String, bool)>`).
  - Función pura para los eventos (testeable sin GPUI):

```rust
/// What one event does to the reply; the caller acts on `Then`.
enum Then { Nothing, Ask(Question, Sender<String>, usize), Progress(usize, String), Settled(usize), Gone, Finish(ReplyState) }
fn on_event(r: Option<&mut Reply>, tool_step: &mut Option<usize>, e: CliEvent) -> Then;
fn settle(r: &mut Reply, step: usize, text: &str, ok: bool);   // the body of settle_tool, without cx
```

  `submit`: `let session = SharedSession::new(Session::new(&ws.s.project, selected));` `let runner = TurnRunner::new(Arc::new(Toolbox::standard()), session.clone(), Services::default());` `cli::start(spec, Box::new(runner))`; `spec.tools` = `Toolbox::standard().agent_definitions()` en JSON (igual a `tools::agent_definitions()`, T1.2). `poll_cli` solo traduce eventos con `on_event`. `finish`: cancela, `turn.session.take()`, borra las entradas de `progress` de ese `item`, y ofrece si hay cambios. `on_event` con `r == None` (chat borrado) → `Then::Gone`, y `poll_cli` llama a `finish(ws, ReplyState::Stopped, cx)`. Se borran los restos de `CliEvent::Tool` en `poll_cli`.

- [ ] **Paso 1: tests que fallan** en `agent.rs`, `mod tests`:

```rust
fn reply() -> Reply { Reply { model: "m".into(), vendor: Vendor::Anthropic, steps: vec![], state: ReplyState::Waiting, started: Instant::now(), elapsed: None, changes: 0 } }

#[test]
fn tool_events_settle_the_line_they_started() {
    let (mut r, mut cur) = (reply(), None);
    on_event(Some(&mut r), &mut cur, CliEvent::ToolStart("create_track".into()));
    on_event(Some(&mut r), &mut cur, CliEvent::ToolCall { name: "create_track".into(), input: json!({ "name": "Bass" }) });
    assert_eq!(cur, Some(0));
    assert!(matches!(on_event(Some(&mut r), &mut cur, CliEvent::ToolProgress("50 %".into())), Then::Progress(0, p) if p == "50 %"));
    assert!(matches!(on_event(Some(&mut r), &mut cur, CliEvent::ToolDone { text: "Created track #3".into(), ok: true }), Then::Settled(0)));
    assert!(matches!(&r.steps[0], Step::Tool { state: ToolState::Done(t), .. } if t == "Created track #3"));
    assert_eq!(cur, None);
}

#[test]
fn a_turn_whose_reply_is_gone_is_stopped() {
    let mut cur = None;
    assert!(matches!(on_event(None, &mut cur, CliEvent::ToolCall { name: "get_project".into(), input: json!({}) }), Then::Gone));
}

#[test]
fn questions_become_pending_cards() {
    let (mut r, mut cur) = (reply(), None);
    on_event(Some(&mut r), &mut cur, CliEvent::ToolCall { name: tools::ASK_USER.into(), input: json!({ "question": "Key?" }) });
    let (tx, _rx) = crossbeam_channel::bounded(1);
    let q = Question { question: "Key?".into(), options: vec![], multi: false };
    assert!(matches!(on_event(Some(&mut r), &mut cur, CliEvent::Ask { question: q, reply: tx }), Then::Ask(_, _, 0)));
}

#[test]
fn saved_steps_read_the_same() {
    let saved = r#"[{"Tool":{"name":"write_clip","input":{"track":"Bass"},"state":{"Done":"Wrote clip #4"}}},{"Text":"Done."}]"#;
    let steps: Vec<Step> = serde_json::from_str(saved).unwrap();
    assert_eq!(serde_json::to_string(&steps).unwrap(), saved);
}
```

  (Antes de escribir `saved_steps_read_the_same`, generar la cadena con el código actual — `serde_json::to_string` de esos dos pasos en `main` — y pegarla tal cual; si difiere de la de arriba, manda la de `main`.)

- [ ] **Paso 2:** `cargo test -p aitrack-app ai_panel::agent` → FALLA al compilar (`on_event`, `Then`).
- [ ] **Paso 3:** implementar; `answer()` envía `p.reply.send(text)`; `finish` limpia `pending` (soltar el `Sender` desbloquea al runner, T1.4).
- [ ] **Paso 4:** `cargo test -p aitrack-app ai_panel` → PASA; `grep -n "tools::execute\|run_tool\|CliEvent::Tool {" crates/app/src/views/ai_panel/agent.rs` → vacío (el hilo de la UI ya no ejecuta herramientas, §6.1).
- [ ] **Paso 5:** commit `app: the assistant's tools run on the turn thread; the panel only draws`.

**Hecho:** tests verdes; la UI no ejecuta herramientas; Parar ofrece lo ya cambiado.

### T1.6 · Progreso en la línea de la herramienta · carril STEPS · → T1.5 · paralela a T1.7

**Ficheros:**
- Modificar: `crates/app/src/views/ai_panel/steps.rs` (`tool_line`, l. 206-221; test en `mod tests`, l. 551)

**Interfaces:**
- Consume: `agent::progress(cx, item, step)` (T1.5).
- Produce: `fn running_title(name: &str, progress: Option<&str>) -> String` → `"{title} · {progress}"` o `title` solo.

- [ ] **Paso 1: test que falla:**

```rust
#[test]
fn a_running_tool_shows_its_progress() {
    assert_eq!(running_title("generate_part", None), "Generating a part");
    assert_eq!(running_title("generate_part", Some("rendering 64 %")), "Generating a part · rendering 64 %");
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-app a_running_tool_shows` → FALLA.
- [ ] **Paso 3:** implementar y usarlo en el brazo `ToolState::Running` de `tool_line` (mismo color `pal.text`; sin colores nuevos).
- [ ] **Paso 4:** `cargo test -p aitrack-app ai_panel::steps` → PASA.
- [ ] **Paso 5:** commit `app: a running tool line shows its progress`.

**Hecho:** test verde. (La comprobación visual va en T1.8.)

### T1.7 · Fuera el camino viejo; pruebas vivas con herramienta lenta · carril CLI · → T1.5 · paralela a T1.6

**Ficheros:**
- Modificar: `crates/ai/src/llm/cli/mod.rs` (borrar `ReplyRunner` y `CliEvent::Tool`; doc del módulo l. 7-9 al día: la herramienta se ejecuta en el hilo del turno)

**Interfaces:**
- Produce: en `mod tests`, `struct TestRunner { delay: Duration, called: bool }` que responde a `get_project` con `"Song \"Night\": 137 BPM, tracks: Kick, Bass"` tras `delay`; `live(provider, model, delay)`.

- [ ] **Paso 1:** reescribir `live()` sobre `TestRunner` (lo que comprueba no cambia: llamó a la herramienta y la respuesta contiene "137"); añadir:

```rust
#[test]
#[ignore = "runs Claude Code on the user's subscription (45 s tool)"]
fn live_claude_code_waits_for_a_slow_tool() { live(Provider::ClaudeCode, "haiku", Duration::from_secs(45)); }

#[test]
#[ignore = "runs Codex on the user's subscription (45 s tool)"]
fn live_codex_waits_for_a_slow_tool() { live(Provider::Codex, "gpt-6-luna", Duration::from_secs(45)); }
```

  `live` imprime con `eprintln!` cuánto tardó la herramienta y si el turno terminó bien (es un test, no el hilo de audio).
- [ ] **Paso 2:** `cargo test -p aitrack-ai llm::cli` → PASA (los vivos quedan ignorados); `grep -rn "CliEvent::Tool {\|ReplyRunner" --include='*.rs' crates` → vacío.
- [ ] **Paso 3:** commit `ai: the CLI turn has one tool path; live tests with a slow tool`.

**Hecho:** compila sin el camino transitorio; los vivos existen y están `#[ignore]`.

### T1.8 · Cierre de la fase 1

- [ ] `cargo check --workspace`; `cargo test -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app`; `cargo clippy -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app --all-targets -- -D warnings` → todo verde.
- [ ] Pruebas vivas (consumen suscripción; pocas): `cargo test -p aitrack-ai live_ -- --ignored --nocapture --test-threads=1` → PASAN las cinco. **Anotar en el commit** cuánto esperaron Claude Code y Codex con la herramienta de 45 s. Si Codex corta antes de 45 s, apuntar el tiempo exacto: decide el presupuesto de ese cliente (§9; ver D15).
- [ ] Comprobación visual (opcional pero recomendada): un sembrado temporal tras una variable de entorno en `ai_panel::render` que pinta una respuesta con una herramienta en curso y progreso "renderizando 64 %" (receta de memoria: `a.scroll.scroll_to_bottom()`), lanzado con `AITRACK_DEV_HIDDEN=1 AITRACK_STUBS=1 AITRACK_DEV_VIEW=ai`, captura por PID con `screencapture -x -o -l <id>`. Se **borra** el sembrado antes del commit.
- [ ] Prueba a mano de humo (si el dueño la pide): un turno real que crea una pista y otro que se para a mitad; la propuesta llega igual que antes.

**Hecho:** chat igual que antes, con la herramienta ejecutada fuera de la UI.

---

## Fase 2 · MCP sobre el mismo runtime

### T2.1 · Broker en tres fases · carril MCP

**Ficheros:**
- Modificar: `crates/mcp/src/broker.rs`

**Interfaces:**
- Consume: `SharedSession`, `Toolbox`, `ToolCtx`, `Services`, `ToolKind`, `MCP_BUDGET` (fase 1).
- Produce:

```rust
/// One call between `begin` and `commit`. Sent to the server's blocking thread.
pub struct Checkout {
    pub session: SharedSession,
    pub services: Services,
    pub kind: ToolKind,
    kept: Option<(u64, String)>,   // the live proposal it builds on
    epoch: u64,                    // Broker::epoch at begin
    before: usize,                 // commands in the draft at begin
}

impl Broker {
    pub fn new(apply_directly: bool) -> Self;                 // + toolbox: Arc<Toolbox::standard()>, services: Services::default(), epoch: 0
    pub fn toolbox(&self) -> &Arc<Toolbox>;
    /// UI, fast: the live draft or a new one from the project.
    pub fn begin(&mut self, project: &Project, history: &History, selected: Option<TrackId>, client: &str, name: &str) -> Checkout;
    /// UI, fast: reserve ids, replace the proposal, apply directly if on. Reads return at once.
    pub fn commit(&mut self, project: &mut Project, history: &mut History, client: &str, checkout: Checkout, outcome: Outcome) -> Reply;
    /// begin → Toolbox::execute (no cancel, no progress, MCP_BUDGET) → commit, on one thread (tests).
    pub fn call(&mut self, project: &mut Project, history: &mut History, selected: Option<TrackId>, client: &str, name: &str, input: &Value) -> Reply;
    pub fn reset(&mut self, history: &mut History);           // also epoch += 1
}
struct Live { session: SharedSession, proposal: u64, origin: String }
```

  Reglas de `begin`: `settle(history)`; `kind = toolbox.kind(name).unwrap_or(ToolKind::Edit)`; con borrador vivo se devuelve **su** `SharedSession` (con `selected` puesto) y `kept = Some((proposal, origin))`; si no, `SharedSession::new(Session::new(project, selected))` (lectura o edición). Reglas de `commit`: lectura → `Reply::read(text, is_error)`; `checkout.epoch != self.epoch` → error "The project was replaced while this ran; read it again." sin efecto; `kept` cuyo `proposal` ya no está pendiente (`self.pending(history) != Some(proposal)`) → error "The user accepted or discarded the draft while this ran; read the project again." sin efecto y sin resucitar el borrador; error o `commands.len() == before` → sin efecto (el borrador vivo sigue); el resto, idéntico a hoy (`broker.rs:138-174`), leyendo del borrador con `with`.

- [ ] **Paso 1: tests que fallan** (los diez actuales se conservan tal cual sobre `call`; se añaden):

```rust
#[test]
fn begin_and_commit_split_one_call() {
    let (mut p, mut h, mut b) = setup();
    let co = b.begin(&p, &h, None, "Claude Desktop", "create_track");
    assert_eq!(co.kind, ToolKind::Edit);
    let out = b.toolbox().clone().execute(&ToolCtx { session: &co.session, services: &co.services, cancel: &CancelToken::new(), progress: &|_| {}, budget: MCP_BUDGET }, "create_track", &json!({ "name": "Bass" }));
    let r = b.commit(&mut p, &mut h, "Claude Desktop", co, out);
    assert!(matches!(r.effect, Effect::Offered(_)), "{}", r.text);
    assert!(p.tracks.iter().all(|t| t.name != "Bass"));
}

#[test]
fn a_draft_resolved_while_the_call_ran_is_not_revived() {
    let (mut p, mut h, mut b) = setup();
    let Effect::Offered(o) = call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" })).effect else { panic!() };
    let co = b.begin(&p, &h, None, "Claude Desktop", "add_device");
    assert!(h.discard(o.id));                               // the user, between the two hops
    let out = b.toolbox().clone().execute(&ToolCtx { session: &co.session, services: &co.services, cancel: &CancelToken::new(), progress: &|_| {}, budget: MCP_BUDGET }, "add_device", &json!({ "track": "Bass", "plugin": "EQ" }));
    let r = b.commit(&mut p, &mut h, "Claude Desktop", co, out);
    assert!(r.is_error && r.effect == Effect::None, "{}", r.text);
    assert!(h.proposals().is_empty() && b.pending(&h).is_none());
}

#[test]
fn a_reset_while_the_call_ran_drops_its_edits() {
    let (mut p, mut h, mut b) = setup();
    let co = b.begin(&p, &h, None, "Claude Desktop", "create_track");
    b.reset(&mut h);
    let out = b.toolbox().clone().execute(&ToolCtx { session: &co.session, services: &co.services, cancel: &CancelToken::new(), progress: &|_| {}, budget: MCP_BUDGET }, "create_track", &json!({ "name": "Bass" }));
    let r = b.commit(&mut p, &mut h, "Claude Desktop", co, out);
    assert!(r.is_error && h.proposals().is_empty(), "{}", r.text);
}

#[test]
fn reads_do_not_commit() {
    let (mut p, mut h, mut b) = setup();
    let co = b.begin(&p, &h, None, "x", "get_project");
    assert_eq!(co.kind, ToolKind::Read);
    let r = b.commit(&mut p, &mut h, "x", co, Outcome::Ok("song".into()));
    assert_eq!((r.text.as_str(), r.effect), ("song", Effect::None));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-mcp broker` → FALLA al compilar.
- [ ] **Paso 3:** implementar; `describe_pending` lee `changes` con `l.session.with(..)`.
- [ ] **Paso 4:** `cargo test -p aitrack-mcp broker` → PASAN los 14 (10 portados + 4 nuevos) — y `generate_part_runs_in_the_broker_itself` de T0.3.
- [ ] **Paso 5:** commit `mcp: the broker begins and commits on the UI thread; the tool runs in between`.

**Hecho:** `Broker::call` compone las tres fases; los tests antiguos pasan sin cambiar lo que comprueban.

### T2.2 · Gateway `begin`/`commit` y servidor · carril MCP · → T2.1

**Ficheros:**
- Modificar: `crates/mcp/src/gateway.rs`, `crates/mcp/src/server.rs` (`Shared`, l. 72-91; `call_tool`, l. 326-339; `tool_list`, l. 289-305), `crates/mcp/src/server_tests.rs` (`Fake`, l. 10-41), `crates/mcp/src/lib.rs` (re-exportar `run_tool`)

**Interfaces:**
- Produce:

```rust
pub trait AppGateway: Send + Sync + 'static {
    fn begin(&self, client: &str, name: &str) -> Result<Checkout, ToolReply>;
    fn commit(&self, client: &str, checkout: Checkout, outcome: Outcome) -> ToolReply;
    fn read_resource(&self, uri: &str) -> Result<String, String>;
}
/// begin → Toolbox::execute on this thread (MCP_BUDGET, no progress, a CancelToken nobody cancels) → commit.
pub fn run_tool(gateway: &dyn AppGateway, toolbox: &Toolbox, client: &str, name: &str, args: Value) -> ToolReply;
```

  `Shared` gana `toolbox: Arc<Toolbox>` (`Toolbox::standard()`) y `serial: parking_lot::Mutex<()>`; `call_tool` comprueba el nombre contra `shared.toolbox.definitions()` y, en `spawn_blocking`, hace `let _g = shared.serial.lock(); run_tool(&*gateway, &shared.toolbox, &c, &n, args)` (§6.6: las llamadas de un mismo servidor se serializan). `tool_list()` sale de `Toolbox::standard().definitions()`. El `Fake` implementa `begin`/`commit` sobre su `Broker`.

- [ ] **Paso 1: test que falla** en `server_tests.rs`:

```rust
#[test]
fn two_calls_at_once_run_one_after_the_other() {
    let fake = Fake::new();
    let (_s, mut c) = start(fake.clone());
    c.connect();
    let url = c.url.clone();
    let session = c.session.clone();
    let other = std::thread::spawn(move || {
        let mut c2 = Client { url, token: Some(TOKEN.to_owned()), session };
        c2.rpc(20, "tools/call", json!({ "name": "create_track", "arguments": { "name": "A" } }))
    });
    let mine = c.rpc(21, "tools/call", json!({ "name": "create_track", "arguments": { "name": "B" } }));
    let theirs = other.join().unwrap();
    assert_eq!((mine["result"]["isError"].clone(), theirs["result"]["isError"].clone()), (json!(false), json!(false)));
    let g = fake.state.lock();
    assert_eq!(g.1.proposals().len(), 1, "both edits grew one proposal");
    assert_eq!(g.2.describe_pending(&g.1).matches("\n- ").count(), 2);
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-mcp server_tests` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-mcp` → PASA (los cinco de `server_tests.rs` sin cambiar lo que comprueban, más el nuevo).
- [ ] **Paso 5:** commit `mcp: tools run on the server's blocking thread between two UI hops`.

**Hecho:** el servidor ya no pide a la UI que ejecute herramientas.

### T2.3 · `ChannelGateway` en la app · carril MCP · → T2.2 · paralela a T2.4

**Ficheros:**
- Modificar: `crates/app/src/mcp_host.rs` (l. 15-54 y test l. 198-218), `crates/app/src/views/mcp.rs` (`mcp_tick` l. 32-41, `mcp_answer` l. 43-69)

**Interfaces:**
- Produce:

```rust
pub enum Ask { Begin { name: String }, Commit { checkout: Checkout, outcome: Outcome }, Resource(String) }
pub enum Answer { Begun(Result<Checkout, ToolReply>), Reply(ToolReply) }
pub struct UiRequest { pub client: String, pub ask: Ask, pub reply: Sender<Answer> }
impl Answer { /// The server is off: the right shape for `ask`.
    pub fn off(ask: &Ask) -> Self; }
```

  `ANSWER_WITHIN` (60 s) cubre cada salto a la UI por separado (§6.6). `mcp_answer`: `Begin` → `Answer::Begun(Ok(self.mcp.broker.begin(&s.project, &s.history, selected, &req.client, name)))`; `Commit` → `broker.commit(..)` y el mismo manejo de `Effect` que hoy (l. 49-56); `Resource` igual que hoy. `mcp_tick` consume `req` por valor (el `Commit` lleva el `Checkout`).

- [ ] **Paso 1: test que falla** — reescribir `calls_reach_the_ui_side_and_the_answer_comes_back` en `mcp_host.rs`: el hilo "UI" contesta un `Begin` con un `Checkout` de un `Broker` local, un `Commit` con `Answer::Reply(ToolReply::ok("committed"))` y un `Resource`; el lado servidor llama a `aitrack_mcp::run_tool(&gw, &Toolbox::standard(), "Cursor", "get_project", json!({}))` y obtiene el texto del `get_project` del proyecto vacío (no "committed": las lecturas no hacen commit… salvo que la UI lo conteste; la UI de prueba solo recibe `Begin` y `Resource`, y el test cuenta exactamente 2 peticiones). Con la UI cerrada, `run_tool` devuelve error al instante.
- [ ] **Paso 2:** `cargo test -p aitrack-app mcp_host` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-app mcp` → PASA (incluye `the_status_text_follows_the_clients`).
- [ ] **Paso 5:** commit `app: the MCP channel carries begin and commit; tools run off the UI thread`.

**Hecho:** `mcp_tick` ya no ejecuta herramientas (`grep -n "execute" crates/app/src/views/mcp.rs` solo encuentra el recurso `PROJECT`, que sigue leyendo en la UI con `tools::execute` de `get_project`: lectura corta, se admite).

### T2.4 · `docs/mcp.md` al día · carril MCP (solo el doc) · paralela a T2.3

- [ ] Reescribir la viñeta de arquitectura (l. 58): `begin` en la UI (tick de 33 ms), ejecución en el hilo bloqueante del servidor con presupuesto de 20 s, `commit` en la UI; las llamadas de un servidor se serializan; `ANSWER_WITHIN` cubre cada salto; no hay progreso ni cancelación para MCP en esta fase. La lista de herramientas de l. 3 no cambia.
- [ ] Commit `docs: the MCP server runs tools between two UI hops`.

### T2.5 · Cierre de la fase 2

- [ ] `cargo check --workspace`; `cargo test -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app`; `cargo clippy -p aitrack-ai -p aitrack-mcp -p aitrack-library -p aitrack-app --all-targets -- -D warnings` → verde.
- [ ] Humo con un cliente MCP real solo si el dueño lo pide (abre un puerto; nunca con `AITRACK_STUBS=1`, que no lo abre, `views/mcp.rs:26`).

**Hecho:** chat y MCP comparten `Toolbox` y camino de ejecución; ninguna herramienta corre en la UI.

---

## Fases 3 a 5 · esquema (se detallan en su propio plan)

### Fase 3 · Servicios y herramientas de samples
- **Objetivo:** `library_overview`, `search_samples`, `analyze_sample`, `get_waveform`, `place_sample` sobre la librería del usuario, ajustados al proyecto; una sola implementación de "colocar un sample".
- **Ficheros:** crear `crates/library/src/place.rs` (extraer de `crates/app/src/workspace.rs` `clip_length` l. 3591, `sample_clip_commands` l. 3674, `audio_track_commands` l. 3738, y de `crates/app/src/views/sounds/logic.rs` `DropFit` l. 132 y `drop_fit` l. 145, más la lectura de cabecera WAV); trait `SampleLibrary` y tipos en `crates/ai/src/llm/runtime/services.rs` (o `samples.rs`); implementación en `crates/library` bajo la característica `ai` (la app ya la activa, `crates/app/Cargo.toml:22`) sobre `Library::search` (`query.rs:394`), `recommend` (`query.rs:532`) y `similar.rs`; `SampleTools` en `crates/ai/src/llm/runtime/`; `LibraryBridge::assistant_samples()` en `crates/app/src/bridge.rs:1903` y sus stubs; cableado de `Services.samples` en `agent.rs::submit` y en `Broker`; persona en `tools/mod.rs` (`AGENT_PERSONA`).
- **Depende de:** fase 1 (y 2 para que el MCP las vea). Independiente de la fase 4.
- **Verifica:** `a_sound_dropped_with_fit_leaves_the_clip_transposed_warped_and_the_right_length` (`workspace.rs:4239`) pasa contra el código movido; tests con una `SampleLibrary` falsa; una muestra `cloud` se rechaza sin abrir el archivo (`Sample.cloud`, `db.rs:297`); resultados sin rutas absolutas y ≤ 4 000 caracteres; prueba viva `--ignored` contra el banco de fábrica real sobre una copia (memoria: el `factory.db` real solo está en el checkout principal).

### Fase 3b · Baterías con one-shots
- **Objetivo:** `build_drum_kit` crea o amplía un Drum Rack con pads `instrument.sampler` de un disparo, notas GM según `role`, hats con el mismo *choke*.
- **Ficheros:** ayudante público del estado del Sampler en `crates/stock` (`{"version":1,"params":{},"extra":{"path":…}}`, junto a `common.rs:115`) **y su inverso** (leer la ruta, ver D11); herramienta en `crates/ai/src/llm/tools/` (o `SampleTools`); persona.
- **Depende de:** fase 3 (`probe_for_placement`).
- **Verifica:** ida y vuelta (kit → ruta de cada pad → `Track::is_drum()` → `generate_part` escribe sobre las notas de los pads); muestra solo en la nube rechazada; captura oculta del Drum Rack con `AITRACK_DEMO=1 AITRACK_DEV_HIDDEN=1 AITRACK_STUBS=1`.

### Fase 4 · Oídos
- **Objetivo:** `measure_mix`/`measure_track` sobre el borrador, con rango y omisiones explícitos.
- **Ficheros:** `crates/ai/src/measure/` (DSP puro; reutiliza `features::loudness`, `features.rs:72`); trait `AudioRender` en `aitrack-ai`; adaptador (~50 líneas) en `crates/app` sobre `aitrack_engine::export::render` (`export.rs:158`) con `RenderOptions.threads` ≤ 2 y la fábrica de `export_factory()` (`bridge.rs:229`, `real/engine.rs:1154`), envuelta para omitir dispositivos de terceros y anotar fallos de carga; `EarsTools`.
- **Depende de:** fase 1; el presupuesto final por cliente depende de la medición viva de T1.8 (D15).
- **Verifica:** seno estéreo 997 Hz a −23 dBFS ⇒ −23,0 LUFS ±0,1 (EBU Tech 3341), pico real con sobremuestreo ×4, bandas y correlación con señales sintéticas; render real sobre el demo; corte por presupuesto informa "medidos los compases 1–N".

### Fase 5 · Co-productor
- **Objetivo:** persona y `producer_context` completos (totales de librería, plugins instalados, sin rutas), tarjetas nuevas en `steps.rs` (`tool_icon`, l. 59-71), chips de sample opcionales (`ChatEntry::Samples`).
- **Depende de:** 3, 3b y 4.
- **Verifica:** capturas ocultas de las tarjetas; pruebas vivas de extremo a extremo ("hazme un drop de house de 16 compases con mis samples y mide la mezcla").

---

## Discrepancias y huecos del diseño

| # | Dónde | Qué pasa | Propuesta | ¿Bloquea? |
|---|---|---|---|---|
| D1 | §7 fase 0 vs §6.3 | La fase 0 solo dice "mover `midi_gen`", pero en cuanto `midi_gen` está en `aitrack-ai`, `Outcome::Delegate`, `DELEGATED` y el `Delegate` del broker sobran y dejarlos obliga a mantener dos caminos durante la fase 1. | Retirarlos ya en la fase 0 (T0.3). El broker pierde el parámetro `delegate` antes de la fase 2. | No |
| D2 | §6.3 `ToolDef { kind: Read \| Edit }` | `ask_user` no es ni lectura ni edición: si fuera `Read`, `steps.rs:384` lo plegaría con las consultas, y si fuera `Edit`, el broker lo trataría como edición. Además el MCP necesita `destructiveHint`, que hoy es una lista fija en `server.rs:295`. | `ToolKind::Ask` como tercera variante y `ToolDef.destructive: bool` (T0.4). | No |
| D3 | §6.3 `crates/ai/src/llm/runtime/` | Ya existe `crates/ai/src/runtime.rs` (el cargador de ONNX Runtime, `aitrack_ai::runtime`). Dos módulos `runtime` distintos confunden. | Mantener la ruta del diseño (`aitrack_ai::llm::runtime`) y decirlo en el doc del módulo nuevo; si se prefiere, renombrarlo a `llm::toolrt` antes de T0.4. | No |
| D4 | §6.4 `Services { samples, render }` | En las fases 1–2 los traits no existen todavía. | `Services` nace vacío (`#[derive(Clone, Default)]`) y gana sus campos en las fases 3 y 4. | No |
| D5 | §6.3 `ToolCtx` / §6.8 | El presupuesto de 25 s (chat) y 20 s (MCP) no tiene sitio en `ToolCtx`. | Campo `budget: Duration`; constantes `CHAT_BUDGET`/`MCP_BUDGET` (T1.2). | No |
| D6 | §6.2 `take()` | "Los cambios tardíos van a una sesión que nadie lee": si `take()` dejara una `Session` vacía en su lugar, una herramienta tardía podría *leer* un proyecto vacío y contestar cosas falsas. | `Arc<Mutex<Option<Session>>>`: tras `take()`, `with` devuelve `None` y la herramienta contesta "The turn ended before this ran." (T1.1). | No |
| D7 | §6.5 `ToolDone` para `ask_user` | Hoy la línea de la pregunta se asienta con lo que eligió el usuario (`answer()`, `agent.rs:775`), y al modelo va otro texto (`Question::answer`). Un `ToolDone` del runner con el texto del modelo pisaría la línea. | `TurnRunner` no emite `ToolDone` para `ask_user` respondida; la UI la asienta como hoy. `Ask.reply` es `Sender<String>` (T1.4). | No |
| D8 | §6.6 `Broker::begin` devuelve "los `Services` del momento" | El broker no tiene de dónde sacarlos. | `Broker.services: Services` que la app actualiza (fase 3/4); `Services::default()` en la fase 2. | No |
| D9 | §6.6 | El diseño no dice qué pasa si entre `begin` y `commit` el usuario acepta/descarta la propuesta o se abre otro proyecto (`broker.reset`). Con el borrador compartido, el `commit` resucitaría un borrador descartado o propondría sobre otro proyecto. | `Checkout` guarda `kept` y `epoch`; `commit` rechaza con un error legible (T2.1, Review Focus 2). | No |
| D10 | §6.3 "`is_drum_track` a `aitrack-model`" | Hay **dos** copias: `crates/app/src/state.rs:2846` y una privada en `crates/ai/src/generate/context.rs:181`. | Las dos pasan a `Track::is_drum()` (T0.1, T0.2). | No |
| D11 | §7 fase 3b "`pad_sample(chain)` devuelve cada ruta" | `pad_sample(chain)` está en la app (`crates/app/src/views/devices/rack_ui.rs:745`), fuera del alcance de un test de `aitrack-ai`. | El ayudante de `aitrack-stock` trae también su inverso (ruta desde el estado) y el test de ida y vuelta lo usa; `pad_sample` puede pasar a llamarlo. | No |
| D12 | §6.7 "como hacen … Bloom (`drum_rack_commands`)" | `drum_rack_commands` vive en la app (`views/device_window/bloom/view.rs:1381`), no en `stock`: sirve de referencia, no se puede reutilizar desde `aitrack-ai`. | `build_drum_kit` construye sus propios `Command`s; si queda muy parecido, se extrae un constructor común a `aitrack-model` (`racks.rs`) en la fase 3b. | No |
| D13 | §6.5 / §6.11 "retirar `Engine::Api` y sus funciones" | `Effort` vive en `aitrack_ai::llm::agent` (el cliente de la API), que usan `cli/*` y `picker.rs`. | Solo se retira lo de la app (T0.5). `llm::agent` y `Provider::ClaudeApi` se quedan; borrarlos o mover `Effort` es otra tarea. | No |
| D14 | §2 "`SoundSearchFn` (… `crates/app/src/real/library.rs`)" | El tipo está en `crates/app/src/bridge.rs:1556`; `real/library.rs:625` lo implementa. Y `drop_fit` (`views/sounds/logic.rs:145`) toma un `LibraryItem` de la app. | En la fase 3 `library::place` define su propio tipo de entrada (`PlaceSource`, §6.4) y la app convierte `LibraryItem` → `PlaceSource`. | No |
| D15 | §9 "si Codex corta por debajo de 25 s" | El tiempo de espera de herramienta de Codex es desconocido; hasta medirlo (T1.8) no se puede fijar el presupuesto de las herramientas largas para Codex. | Medirlo en T1.8 y anotar el valor; la fase 4 lo usa. No afecta a las fases 0–2 (no hay herramientas largas todavía). | **Sí, para la fase 4** (no para 0–2) |
| D16 | §6.6 "`AppGateway` y `ChannelGateway` pasan a ofrecer `begin`/`commit`" | No dice dónde corre `Toolbox::execute` ni quién serializa. | `gateway::run_tool` en `aitrack-mcp`, llamado desde `server.rs` en `spawn_blocking` bajo `Shared.serial` (T2.2). El recurso `aitrack://project` sigue leyéndose en la UI (lectura corta). | No |

Ninguna discrepancia bloquea las fases 0–2.
