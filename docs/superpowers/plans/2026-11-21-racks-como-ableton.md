# Racks como en Ableton: plan de implementación

> **Para agentes:** SUB-SKILL OBLIGATORIA: usa superpowers:subagent-driven-development (recomendado) o superpowers:executing-plans para ejecutar este plan tarea por tarea. Los pasos usan casillas (`- [ ]`).

**Objetivo:** que agrupar, desagrupar y plegar no pierdan datos, que un rack nuevo funcione desde el primer momento y que los racks se puedan guardar, ver en Cmd+K y volver a cargar (con 4 racks de fábrica hechos con dispositivos stock).

**Arquitectura:** Group y Ungroup pasan a `AddDevice`/`MoveDevice`/`RemoveDevice` (el `MoveDevice` ya conserva mapeos), y Fold pasa a un comando nuevo, `SetDeviceCollapsed`. Un rack nuevo nace con una cadena vacía; el motor deja pasar audio o MIDI por una cadena vacía de un Audio o MIDI Rack y deja en silencio la de un Instrument o Drum Rack. El formato `.aitrack-rack` es un módulo puro de `aitrack-model` (`rack_preset`); la app guarda los racks del usuario en `<config>/user_racks/<kind>/`, y los de fábrica van embebidos en `aitrack-stock`. Cargar un rack es un único `AddDevice` con ids nuevos.

**Stack:** Rust 2024, `serde`/`serde_json`, `thiserror` (ya en `aitrack-model`), GPUI (`vendor/gpui-pre`, no se toca), `aitrack_library::scan::quick_hash` (blake3, ya en la app).

**Especificación:** `docs/research/28-auditoria-racks.md` (auditoría con evidencia ruta:línea, formato `.aitrack-rack` propuesto, flujo y riesgos F.1–F.5). Se lee junto a este plan. Las rutas con prefijo M/A/G/K de la auditoría son `crates/model/src`, `crates/app/src`, `crates/engine/src` y `crates/stock/src`.

## Diseño

Decisiones que este plan fija. Lo que diverge de la auditoría o del encargo está en «Discrepancias y huecos».

1. **Group** = `AddDevice` del rack con **una cadena vacía** en el sitio del primer dispositivo, más un `MoveDevice` por cada dispositivo hacia esa cadena. **Ungroup** = un `MoveDevice` por cada dispositivo de la cadena hacia la cadena que contiene el rack, detrás de este, más `RemoveDevice` del rack ya vacío. `MoveDevice` (`command.rs:753-771`) solo suelta los mapeos que dejan de ser válidos: los del rack padre sobreviven, porque `valid_links` (`command.rs:1142`) mira dentro del rack a cualquier profundidad, y los de moduladores también, porque los dispositivos siguen en la misma pista. Ungroup de un rack con varias cadenas sigue sin poder hacerse y conserva el aviso (`editing.rs:566`).
2. **Fold** = un comando nuevo, `Command::SetDeviceCollapsed { device, collapsed }`. Es estado de vista: no entra en `frozen_target` ni en `is_destructive`, y el motor no recompila (`diff_devices` ignora `collapsed`, `compile.rs:1169-1170`).
3. **Rack nuevo:** `racks::fresh_rack(kind)` da una cadena vacía «Chain 1» a Audio, Instrument y MIDI. Un Drum Rack nace sin pads, como hasta ahora. En el motor:
   - una cadena vacía de un Audio Rack copia la entrada (ya lo hace hoy, `graph.rs:775`);
   - una de un MIDI Rack pasa las notas (ya lo hace hoy, `graph.rs:649-651`);
   - en un Instrument o Drum Rack, una cadena **sin instrumento** arranca en silencio (cambio nuevo);
   - un Audio o MIDI Rack **sin ninguna cadena** (de proyectos viejos) es transparente, y uno Instrument o Drum sin cadenas sigue mudo.
   Los proyectos viejos abren igual: no cambia el formato del proyecto.
4. **Formato `.aitrack-rack`:** JSON con `format = "aitrack-rack"`, su propio `format_version` (1), `app_version`, `name`, `author`, `tags`, `kind`, `payload` (`{"device": Device}`, un enum de una sola variante que en B4 ganará `chain`) y `requires { plugins, samples }`. Al guardar:
   - las sidechains van a `null`;
   - se quitan los mapeos de macro y de modulador que apuntan fuera del rack;
   - `requires` se calcula a partir del contenido.
   Al cargar, se rechaza una versión más nueva (`TooNew`, mismo patrón que `format.rs:67-80`), se dan ids nuevos a todo con `copy_devices` y se reenlaza con `relink_devices` (`tracks.rs:149-187`). El rack cargado toma el nombre del preset.
5. **Biblioteca:** `<config>/user_racks/<audio|instrument|midi|drum>/<Nombre>.aitrack-rack`. El nombre se limpia con `presets::clean_name` (≤ 48 caracteres, sin `/\:*?"<>|`) y se escribe con `write_atomic` (`project_io.rs:99`); repetir un nombre sobrescribe el archivo. Los racks de fábrica viven en `crates/stock/racks/<kind>/` y se embeben con `build.rs`.
6. **Cmd+K:** sección «Racks» en la pestaña **Presets**, por encima de los presets de stock. En la pestaña All también sale, solo cuando hay texto escrito. Es un `Item::Rack` nuevo: `PresetRow` no cambia (D1). Enter añade el rack a la pista seleccionada y se puede arrastrar a la zona «Add device» del Device View. Los racks del usuario llevan un botón de borrar.
7. **Guardar:** botón Save en la barra de título del rack (Device View) y «Save as Preset…» en su menú contextual. Los dos abren el popover de nombre de `chrome.rs`, generalizado. Antes de guardar se capturan los estados de los plugins (`capture_plugin_states`); si un plugin del rack no devolvió estado (sandbox o error), se avisa con su nombre.
8. **Un paso de undo** en cada acción (Group, Ungroup, Fold, cargar un rack). Guardar no edita el proyecto, salvo la captura de estados de plugins, que no deja paso de undo (`state.rs:2249-2266`).

## Restricciones globales

- Toda edición del proyecto es un `Command` de `aitrack-model`; la UI no muta el `Project` (asignar ids con `alloc_device_id` antes de construir el comando, como `group_devices`, `editing/racks.rs:97`, sí se admite).
- Nada de asignaciones, locks, strings, E/S ni `println!` en `Processor::process` ni en el callback de audio; lo que se decide por cadena se calcula al compilar (`compile.rs`), fuera del hilo RT. `EventList::push` en `run_midi` usa la capacidad fija ya reservada.
- El estado de los plugins se captura fuera del hilo de audio (`EngineBridge::plugin_states`, hilo principal).
- Colores de `crates/app/src/theme.rs`, iconos de `crates/app/src/icons.rs` (existen `Save`, `Trash`, `Grid`, `Drum`, `Preset`), tiempos de `crates/app/src/motion.rs`; espaciado en múltiplos de 4 px (`SP1`…`SP6`).
- Tests junto al código (`#[cfg(test)] mod tests`); los del motor van en `crates/engine/src/tests/racks.rs`, como los demás de racks.
- Todo atajo nuevo lleva su entrada en la `TABLE` de `keys.rs`, que refleja `actions.rs`. Este bloque no añade atajos.
- Sin dependencias nuevas. `aitrack-model` sigue sin E/S; `aitrack-stock` solo usa `aitrack-model` como dev-dependency (ya lo es, `crates/stock/Cargo.toml`).
- **NUNCA `cargo fmt`** (el repo está formateado a mano).
- En zsh, entrecomillar globs (`--include='*.rs'`).
- **Nota de merge:** el implementador de la Fase 0 del asistente cambia ahora `Track::is_drum()` en `crates/model/src/project.rs` y `is_drum_track` en `crates/app/src/state.rs` (l. 2846 y usos l. 439, 858, 877, 895, 898), `views/session.rs` y `views/arrangement.rs`. **No se tocan esas líneas.** Este plan solo toca `state.rs` en l. 1798-1840 (A2.3, A3.4) y su `mod tests` (l. 3018).
- Cierre de cada bloque: `cargo check --workspace`; `cargo test -p aitrack-model -p aitrack-stock -p aitrack-engine -p aitrack-app`; `cargo clippy -p aitrack-model -p aitrack-stock -p aitrack-engine -p aitrack-app --all-targets -- -D warnings`.
- Verificación visual: siempre `AITRACK_DEV_HIDDEN=1 AITRACK_STUBS=1` (sin ventanas a la vista del dueño). Se elige la ventana por el PID de tu propio lanzamiento y se captura con `screencapture -x -o -l <id>`.

## Review Focus

Entradas o condiciones que la especificación implica, que un test de camino feliz no cubre y que más probablemente muerdan a quien use la app. Cada una tiene su test en la tarea dueña.

1. **Agrupar dentro de un rack con macros y moduladores ya mapeados** (lo normal en Live): el macro del rack padre y el LFO de la pista siguen moviendo lo mismo, y deshacer o rehacer deja el proyecto idéntico. → A1.1 (`grouping_and_ungrouping_inside_a_rack_keep_the_macro_and_modulator_links`), A1.3 (`group_fold_and_ungroup_keep_a_macro_mapping_through_undo_and_redo`).
2. **Cargar el mismo rack dos veces, o en el proyecto del que salió:** ni `DuplicateId` ni mapeos cruzados; cada copia mueve solo sus propios dispositivos. → A3.2 (`a_saved_rack_instantiates_with_fresh_ids_and_its_own_links`, `instantiating_twice_gives_two_independent_racks`).
3. **Un rack que usa un plugin no instalado o un sample movido:** se carga igual (el motor marca el plugin `Missing`, `compile.rs:387`) y el usuario ve qué falta, con nombre. → A3.2 (`what_a_rack_needs_is_listed_and_what_is_missing_is_named`), A3.4 (`saving_names_the_plugins_whose_state_was_not_captured_and_loading_names_what_is_missing`).
4. **Guardar con un plugin en sandbox o cuyo estado no se capturó:** el archivo se escribe con el último estado conocido y el aviso nombra el plugin. → A3.4 (mismo test).
5. **Un archivo de rack de una versión más nueva, corrupto o con un nombre inválido o repetido:** mensaje legible («newer AI-Track», «not an AI-Track rack»), sin pánico; repetir el nombre sobrescribe y lo dice («Updated»). → A3.1 (`a_rack_file_reads_back_and_a_newer_one_is_refused`), A3.3 (`a_saved_rack_is_listed_by_kind_reads_back_and_can_be_deleted`, `a_broken_rack_file_is_listed_but_does_not_load`).

---

## Mapa de ficheros

| Fichero | Tarea | Qué cambia |
|---|---|---|
| `crates/model/src/racks.rs` | A1.1, A2.2 | `group_commands` (l. 89-132) y `ungroup_commands` (l. 140-155) con `MoveDevice`; `fresh_rack`; tests |
| `crates/model/src/command.rs` | A1.2 | variante `SetDeviceCollapsed` (enum l. 122-254), `label` (l. 266-350), `apply` (junto a `RenameDevice`, l. 804-807); test |
| `crates/model/src/rack_preset.rs` | A3.1, A3.2 | **nuevo**: `RackPreset`, `Payload`, `Requires`, `PluginNeed`, `SampleNeed`, `Missing`, `RackPresetError`, `kind_slug`, `state_paths` |
| `crates/model/src/lib.rs` | A3.1 | `pub mod rack_preset;` |
| `crates/model/src/tracks.rs` | A3.2 | `copy_devices` (l. 149) y `relink_devices` (l. 168) pasan a `pub(crate)` |
| `crates/engine/src/graph.rs` | A2.1 | `ChainNode.feeds_input` (struct l. 347); `RackNode::process` (l. 720-823) y `run_midi` (l. 631-656) |
| `crates/engine/src/compile.rs` | A2.1 | rellena `feeds_input` en el cierre `node` (l. 301-331) |
| `crates/engine/src/tests/racks.rs` | A2.1 | tests de cadenas vacías y de racks sin cadenas |
| `crates/stock/build.rs` | A4.1 | segunda tabla, `FACTORY_RACKS`, con los `.aitrack-rack` de `racks/` |
| `crates/stock/src/racks.rs` | A4.1, A4.2 | **nuevo**: `factory()`, `racks_dir()`; test de validación |
| `crates/stock/src/lib.rs` | A4.1 | `pub mod racks;` |
| `crates/stock/examples/racks_factory.rs` | A4.2 | **nuevo**: generador de los racks de fábrica |
| `crates/stock/racks/{audio,midi}/*.aitrack-rack` | A4.2 | **nuevos**: 4 racks generados |
| `crates/app/src/editing/racks.rs` | A1.3 | `fold_device`; `ungroup_devices` lee los `MoveDevice`; test |
| `crates/app/src/views/devices.rs` | A1.3, A3.5, A3.6 | `toggle_fold` (l. 562-577) llama a `fold_device`; botón Save en la cabecera (l. 437-519); drop de `DraggedRack` en «add-device» (l. 57-86); `pub mod rack_presets;` (l. 28) |
| `crates/app/src/state.rs` | A2.3, A3.4 | `add_stock_device_in` usa `fresh_rack` (l. 1837); `insert_slot` extraído de l. 1817-1827; test en `mod tests` (l. 3018) |
| `crates/app/src/views/devices/rack_ui.rs` | A2.3 | `chain_column`: zona de soltar también en un rack sin cadenas (l. 375) |
| `crates/app/src/rack_library.rs` | A3.3 | **nuevo**: `RackSource`, `RackEntry`, `root`, `list`, `read`, `save`, `delete`, `stamp_samples` |
| `crates/app/src/main.rs` | A3.3 | `mod rack_library;` (junto a `mod project_io;`, l. 32) |
| `crates/app/src/editing/rack_presets.rs` | A3.4 | **nuevo**: `RackLoad`, `AppState::rack_preset_for_save`, `AppState::add_rack_preset`; tests |
| `crates/app/src/editing.rs` | A3.4 | `mod rack_presets;` (junto a `mod racks;`, l. 22) |
| `crates/app/src/views/devices/rack_presets.rs` | A3.5, A3.6 | **nuevo**: `open_save`, `save_card`, `save`, `add`, `DraggedRack` |
| `crates/app/src/views/device_ui/mod.rs` | A3.5 | `PopKind::SaveRack(DeviceId)` (enum l. 250-266) y su rama en `overlay` (l. 589-597) |
| `crates/app/src/views/device_window/chrome.rs` | A3.5 | `name_field` (l. 212-227) recibe placeholder y función de envío; `pub(crate)` |
| `crates/app/src/editing/menus.rs` | A3.5 | «Save as Preset…» en `device_menu` (l. 503-575) para racks |
| `crates/app/src/views/palette/mod.rs` | A3.6 | `Item::Rack`, `racks` en `PaletteState`, `invalidate_racks`, `rack_rows`, secciones, `activate`, `render_rack`; test |
| `crates/app/src/workspace.rs` | A5.1 | palabras `rack=`, `rackpreset=`, `racksave` en `apply_dev_words` (rama `_`, l. 1348-1365) |

## Carriles y paralelismo

Un carril es dueño exclusivo de sus ficheros mientras dura la tarea; dos tareas en paralelo nunca comparten fichero.

| Carril | Ficheros |
|---|---|
| **M-RACK** | `crates/model/src/{racks.rs,command.rs}` |
| **M-PRESET** | `crates/model/src/{rack_preset.rs,lib.rs,tracks.rs}` |
| **ENGINE** | `crates/engine/src/{graph.rs,compile.rs,tests/racks.rs}` |
| **STOCK** | `crates/stock/{build.rs,src/lib.rs,src/racks.rs,racks/**,examples/racks_factory.rs}` |
| **A-EDIT** | `crates/app/src/editing/racks.rs`, `crates/app/src/views/devices.rs` (solo `toggle_fold`) |
| **A-STATE** | `crates/app/src/state.rs` (l. 1798-1840 y `mod tests`), `crates/app/src/views/devices/rack_ui.rs`, luego `crates/app/src/editing/{rack_presets.rs,editing.rs→mod}` |
| **A-LIB** | `crates/app/src/{rack_library.rs,main.rs}` |
| **A-UI** | `crates/app/src/views/devices/{rack_presets.rs,devices.rs}`, `views/device_ui/mod.rs`, `views/device_window/chrome.rs`, `editing/menus.rs`, `views/palette/mod.rs`, `workspace.rs` |

Orden (→ = depende de):

```
Ola 1:  A1.1 → A1.2 → A2.2   (M-RACK)
        A3.1 → A3.2          (M-PRESET)
        A2.1                 (ENGINE)
        A4.1                 (STOCK)
        [los cuatro carriles en paralelo]
Ola 2:  A1.3 (A-EDIT, → A1.1, A1.2)
        A2.3 (A-STATE, → A2.2)
        A3.3 (A-LIB, → A3.1, A4.1)
        A4.2 (STOCK, → A3.2)
        [los cuatro en paralelo]
Ola 3:  A3.4 (A-STATE, → A2.3, A3.2) → A3.5 (A-UI, → A1.3, A3.3, A3.4) → A3.6 (A-UI) → A5.1 cierre
```

Dentro de cada tarea: test primero, ver que falla, implementar, ver que pasa y hacer commit (solo si el dueño pidió commits en la sesión; estilo `área: frase en minúsculas`, en inglés como los del repo).

---

## Bloque A · Racks que no pierden datos, que funcionan y que se guardan

### A1.1 · Group y Ungroup con `MoveDevice` · carril M-RACK

**Ficheros:**
- Modificar: `crates/model/src/racks.rs` (`group_commands` l. 89-132, `ungroup_commands` l. 140-155, tests l. 157-255)

**Interfaces:**
- Produce (mismas firmas que hoy):
  - `pub fn group_commands(p: &Project, ids: &[DeviceId], rack: DeviceId) -> Option<Vec<Command>>`. Devuelve `[AddDevice { slot: home.slot(primer índice), device: rack con una cadena vacía llamada como el primer dispositivo }, MoveDevice { id: ids_ordenados[k], to: DeviceSlot::Rack { rack, chain: 0, index: k } }…]`. El tipo de rack se elige como hoy (l. 110-117).
  - `pub fn ungroup_commands(p: &Project, id: DeviceId) -> Option<Vec<Command>>`. Devuelve `[MoveDevice { id: interior[k], to: home.slot(at + 1 + k) }…, RemoveDevice { id }]`; con 0 cadenas, solo el `RemoveDevice`.
  - `can_ungroup` no cambia.

- [ ] **Paso 1: test que falla** en `racks.rs`, `mod tests` (usa `project()`, `run()` y `chain()` ya existentes):

```rust
#[test]
fn grouping_and_ungrouping_inside_a_rack_keep_the_macro_and_modulator_links() {
    let (mut p, t, ids) = project(); // EQ, Comp, Reverb, Limiter
    let outer = p.alloc_device_id();
    run(&mut p, group_commands(&p, &[ids[1], ids[2]], outer).unwrap());
    Command::AddMapping { rack: outer, macro_index: 0, index: 0, mapping: MacroMapping { device: ids[1], param: 1, min: 0.2, max: 0.8 } }.apply(&mut p).unwrap();
    let lfo = p.alloc_device_id();
    Command::AddDevice { slot: DeviceSlot::Track { track: t, index: 0 }, device: Box::new(Device::stock(lfo, "mod.lfo", "LFO")) }.apply(&mut p).unwrap();
    Command::AddModMapping { device: lfo, index: 0, mapping: ModMapping::bipolar(ModTarget::Param { device: ids[2], param: 2 }, 0.3) }.apply(&mut p).unwrap();
    let before = p.clone();
    let macro_links = |p: &Project| match &p.device(outer).unwrap().kind { DeviceKind::Rack(r) => r.macros[0].mappings.clone(), _ => unreachable!() };
    // Group Comp + Reverb into a rack nested in `outer`.
    let inner = p.alloc_device_id();
    let undo = run(&mut p, group_commands(&p, &[ids[2], ids[1]], inner).unwrap());
    assert_eq!(device_slot(&p, inner), Some(DeviceSlot::Rack { rack: outer, chain: 0, index: 0 }));
    assert_eq!(device_slot(&p, ids[2]), Some(DeviceSlot::Rack { rack: inner, chain: 0, index: 1 }), "signal order kept");
    assert_eq!(macro_links(&p), macro_links(&before), "the outer macro still drives the Comp");
    assert_eq!(p.device(lfo).unwrap().mods, before.device(lfo).unwrap().mods, "the LFO still drives the Reverb");
    // Undo of the grouping is exact.
    let mut q = p.clone();
    for c in undo.into_iter().rev() {
        c.apply(&mut q).unwrap();
    }
    q.ids = before.ids.clone();
    assert_eq!(q, before);
    // Ungroup puts them back where the rack was, links and all.
    run(&mut p, ungroup_commands(&p, inner).unwrap());
    assert_eq!(chain(&p, Container::Rack { rack: outer, chain: 0 }).unwrap().iter().map(|d| d.id).collect::<Vec<_>>(), vec![ids[1], ids[2]]);
    assert_eq!(macro_links(&p), macro_links(&before));
    assert_eq!(p.device(lfo).unwrap().mods, before.device(lfo).unwrap().mods);
    assert_eq!(p.device(inner), None);
}

#[test]
fn ungrouping_drops_only_the_dissolved_racks_own_macros_and_undo_restores_them() {
    let (mut p, t, ids) = project();
    let rack = p.alloc_device_id();
    run(&mut p, group_commands(&p, &[ids[1]], rack).unwrap());
    Command::AddMapping { rack, macro_index: 3, index: 0, mapping: MacroMapping { device: ids[1], param: 0, min: 0.0, max: 1.0 } }.apply(&mut p).unwrap();
    let before = p.clone();
    let undo = run(&mut p, ungroup_commands(&p, rack).unwrap());
    assert_eq!(names(&p, t), ["EQ", "Comp", "Reverb", "Limiter"]);
    for c in undo.into_iter().rev() {
        c.apply(&mut p).unwrap();
    }
    p.ids = before.ids.clone();
    assert_eq!(p, before, "undo brings the rack back with its macro mapping");
}
```

  (`DeviceSlot`, `MacroMapping`, `ModMapping` y `ModTarget` ya llegan por `super::*`: `racks.rs` importa `crate::command::{Command, DeviceSlot}` y `crate::project::*`, l. 10-12.)

- [ ] **Paso 2:** `cargo test -p aitrack-model racks::` → FALLA `grouping_and_ungrouping_inside_a_rack_keep…` («the outer macro still drives the Comp»: hoy `RemoveDevice` borra el mapeo).
- [ ] **Paso 3:** reescribir `group_commands` y `ungroup_commands` según las Interfaces. Actualizar el doc del módulo (l. 6-8): el rack se crea vacío y los dispositivos entran con `MoveDevice`, que conserva los mapeos.
- [ ] **Paso 4:** `cargo test -p aitrack-model racks::` → PASAN los 7 (incluidos los 5 de antes: `grouping_puts_the_devices_in_one_chain_in_order_and_undoes` sigue comparando `p.tracks` tras deshacer).
- [ ] **Paso 5:** commit `model: group and ungroup move the devices, so macro and modulator mappings survive`.

**Hecho:** `grep -n "RemoveDevice\|AddDevice" crates/model/src/racks.rs` solo encuentra el `AddDevice` del rack nuevo, el `RemoveDevice` del rack disuelto y los tests.

### A1.2 · `Command::SetDeviceCollapsed` · carril M-RACK · → A1.1

**Ficheros:**
- Modificar: `crates/model/src/command.rs` (enum l. 172-180, `label` l. 321-328, `apply` tras `RenameDevice` l. 804-807, tests)

**Interfaces:**
- Produce: `Command::SetDeviceCollapsed { device: DeviceId, collapsed: bool }`. Su inverso lleva el valor anterior; `label()` = `"Fold device"`. `DeviceNotFound` si el dispositivo no existe. No es destructivo y no figura en `frozen_target` (cae en `_ => None`: plegar es vista).

- [ ] **Paso 1: test que falla** en `command.rs`, `mod tests` (usa `rack_rig`, `rack_of` y `round_trip`, l. 2006-2045):

```rust
#[test]
fn folding_a_device_is_one_command_that_keeps_its_mappings() {
    let r = rack_rig();
    let q = round_trip(&r.p, Command::SetDeviceCollapsed { device: r.eq, collapsed: true });
    assert!(q.device(r.eq).unwrap().collapsed);
    assert_eq!(rack_of(&q, r.rack).macros, rack_of(&r.p, r.rack).macros, "the macro on the EQ stays");
    let fold = Command::SetDeviceCollapsed { device: r.eq, collapsed: true };
    assert!(!fold.is_destructive());
    assert_eq!(fold.label(), "Fold device");
    assert_eq!(fold.frozen_target(&r.p), None);
    assert_eq!(Command::SetDeviceCollapsed { device: DeviceId(9999), collapsed: true }.apply(&mut r.p.clone()), Err(CommandError::DeviceNotFound(DeviceId(9999))));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-model folding_a_device` → FALLA al compilar (`no variant SetDeviceCollapsed`).
- [ ] **Paso 3:** añadir la variante, su `label` y su `apply` (`std::mem::replace(&mut d.collapsed, collapsed)`, como `SetDeviceEnabled`, l. 792-795).
- [ ] **Paso 4:** `cargo test -p aitrack-model` → PASA (incluido `every_command_round_trips`, l. 1528).
- [ ] **Paso 5:** commit `model: SetDeviceCollapsed folds a device without removing it`.

**Hecho:** el test pasa y `cargo check --workspace` está verde (ningún `match` exhaustivo sobre `Command` fuera de `command.rs`; comprobado con grep).

### A2.2 · `racks::fresh_rack` · carril M-RACK · → A1.2

**Ficheros:**
- Modificar: `crates/model/src/racks.rs`

**Interfaces:**
- Produce: `pub fn fresh_rack(kind: RackKind) -> Rack`. Es `Rack::new(kind)` más `Chain::new("Chain 1", Color::palette(0))` cuando `kind != RackKind::Drum`. `Rack::new` no cambia: lo usan muchos tests y `slice.rs`.

- [ ] **Paso 1: test que falla:**

```rust
#[test]
fn a_fresh_rack_has_one_empty_chain_but_a_drum_rack_has_no_pads() {
    for kind in [RackKind::Audio, RackKind::Instrument, RackKind::Midi] {
        let r = fresh_rack(kind);
        assert_eq!((r.kind, r.chains.len(), r.chains[0].devices.len(), r.chains[0].name.as_str()), (kind, 1, 0, "Chain 1"));
    }
    assert!(fresh_rack(RackKind::Drum).chains.is_empty());
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-model a_fresh_rack` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-model racks::` → PASA.
- [ ] **Paso 5:** commit `model: a fresh rack comes with one empty chain`.

**Hecho:** el test pasa.

### A2.1 · Cadenas vacías y racks sin cadenas en el motor · carril ENGINE · paralela a la ola 1

**Ficheros:**
- Modificar: `crates/engine/src/graph.rs` (struct `ChainNode` l. 347; `RackNode::run_midi` l. 631-656; `RackNode::process` l. 720-823)
- Modificar: `crates/engine/src/compile.rs` (cierre `node`, l. 301-331)
- Test: `crates/engine/src/tests/racks.rs`

**Interfaces:**
- Produce: `ChainNode { pub feeds_input: bool, … }`, que se calcula al compilar. Es `false` solo cuando `rack.kind` es `Instrument` o `Drum` y ningún nodo de la cadena compilada «suena por sí mismo», es decir, cuando no hay `DeviceNode::Proc(p)` con `p.is_instrument` ni `DeviceNode::Rack(r)` con `r.kind` Instrument o Drum.
- `RackNode::process`:
  - con `feeds_input`, `c.buf.copy_from(&self.input)` como hoy; si no, `c.buf.clear()`;
  - un rack `Audio` sin cadenas ni returns sale antes de `buf.clear()` (l. 724) y deja pasar la señal.
- `run_midi`: sin cadenas, copia `cur` a `self.out` y vuelve.
- Nada asigna en el hilo RT.

- [ ] **Paso 1: tests que fallan** en `tests/racks.rs` (usan `B`, `Probes`, `DC`, `KEYS` y `peak`, l. 18-169):

```rust
/// A DC probe at 0.5 on the track, then `rack`.
fn dc_then(kind: TrackKind, rack: Rack) -> B {
    let mut b = B::new(kind);
    let dc = b.device(DC, &[(0, 0.5)]);
    b.p.track_mut(b.track).unwrap().devices.push(dc);
    b.rack(rack);
    b
}

fn last(b: &B) -> f32 {
    let out = b.render(&Probes::default(), PPQ);
    out.channel(0)[out.frames() - 1]
}

#[test]
fn an_empty_chain_passes_audio_but_an_instrument_or_drum_chain_without_an_instrument_is_silent() {
    for (kind, want) in [(RackKind::Audio, 0.5), (RackKind::Instrument, 0.0), (RackKind::Drum, 0.0)] {
        let mut rack = Rack::new(kind);
        let mut c = Chain::new("Chain 1", Color(0));
        c.note = (kind == RackKind::Drum).then_some(36);
        rack.chains.push(c);
        let b = dc_then(TrackKind::Instrument, rack);
        assert!((last(&b) - want).abs() < 1e-4, "{kind:?}");
    }
}

#[test]
fn a_rack_without_chains_is_transparent_for_audio_and_midi_and_mute_for_instruments() {
    assert!((last(&dc_then(TrackKind::Audio, Rack::new(RackKind::Audio))) - 0.5).abs() < 1e-4);
    assert_eq!(last(&dc_then(TrackKind::Instrument, Rack::new(RackKind::Instrument))), 0.0);
    for chains in [0, 1] {
        let mut b = B::new(TrackKind::Instrument);
        let mut rack = Rack::new(RackKind::Midi);
        rack.chains.extend((0..chains).map(|_| Chain::new("Chain 1", Color(0))));
        b.rack(rack);
        let keys = b.device(KEYS, &[]);
        let k = keys.id;
        b.p.track_mut(b.track).unwrap().devices.push(keys);
        b.notes(&[(60, 0, PPQ / 2)]);
        let f = Probes::default();
        b.render(&f, PPQ);
        assert!(f.notes(k).iter().any(|n| n.0 && n.1 == 60), "{chains} chains: the note reaches the device after the rack");
    }
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-engine tests::racks` → FALLAN los dos (Instrument/Drum con cadena vacía dan 0,5; el Audio Rack sin cadenas da 0; con 0 cadenas, la nota no llega).
- [ ] **Paso 3:** implementar según las Interfaces.
- [ ] **Paso 4:** `cargo test -p aitrack-engine` → PASA todo (incluidos `rack_chains_are_latency_aligned`, `tests.rs:301`, y `a_track_of_the_real_midi_effects_never_allocates`, `tests/stock_midi.rs:452`).
- [ ] **Paso 5:** commit `engine: an empty chain passes audio and MIDI, an instrument chain waits for its instrument`.

**Hecho:** los tests pasan y no aparece ningún aviso del contador de asignaciones RT en los tests de racks.

### A4.1 · Tabla de racks de fábrica en `aitrack-stock` · carril STOCK · paralela a la ola 1

**Ficheros:**
- Modificar: `crates/stock/build.rs` (generaliza `collect(dir, out)` con la extensión como parámetro; escribe además `OUT_DIR/factory_racks.rs` con `pub static FACTORY_RACKS: &[(&str, &str)]` a partir de `racks/**.aitrack-rack`; añade `cargo:rerun-if-changed=racks`)
- Crear: `crates/stock/src/racks.rs`
- Modificar: `crates/stock/src/lib.rs` (`pub mod racks;`)

**Interfaces:**
- Produce, en `aitrack_stock::racks`:
  - `pub fn factory() -> &'static [(&'static str, &'static str)]` → `(ruta relativa "audio/Nombre.aitrack-rack", json)`, ordenada por ruta;
  - `pub fn racks_dir() -> PathBuf` → `CARGO_MANIFEST_DIR/racks`, para el generador.
- Sin carpeta `racks/` la tabla queda vacía.

- [ ] **Paso 1: test que falla** en `src/racks.rs`:

```rust
#[test]
fn factory_racks_sit_in_a_kind_folder() {
    for (rel, json) in factory() {
        let (kind, file) = rel.split_once('/').unwrap_or_else(|| panic!("{rel}"));
        assert!(["audio", "instrument", "midi", "drum"].contains(&kind), "{rel}");
        assert!(file.ends_with(".aitrack-rack") && !json.is_empty(), "{rel}");
    }
    assert!(racks_dir().ends_with("racks"));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-stock racks::` → FALLA al compilar.
- [ ] **Paso 3:** implementar (`include!(concat!(env!("OUT_DIR"), "/factory_racks.rs"));`, como `presets.rs:17`).
- [ ] **Paso 4:** `cargo test -p aitrack-stock racks::` → PASA (la tabla está vacía hasta A4.2); `cargo test -p aitrack-stock presets` → PASA (los presets no ven los racks: siguen filtrando `.json`).
- [ ] **Paso 5:** commit `stock: the build embeds factory racks next to the presets`.

**Hecho:** `cargo check -p aitrack-stock` está verde y `presets::all()` no cambia.

### A3.1 · Formato `.aitrack-rack`: tipos, lectura y versión · carril M-PRESET · paralela a la ola 1

**Ficheros:**
- Crear: `crates/model/src/rack_preset.rs`
- Modificar: `crates/model/src/lib.rs` (`pub mod rack_preset;`, en orden alfabético tras `pub mod racks;`)

**Interfaces:**
- Produce:

```rust
pub const RACK_FORMAT: &str = "aitrack-rack";
pub const RACK_FORMAT_VERSION: u32 = 1;
/// `RACK_MIGRATIONS[n]` takes version n + 1 to n + 2 (format.rs pattern); empty for now.
pub const RACK_MIGRATIONS: &[crate::format::Migration] = &[];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RackPreset {
    pub format: String,
    pub format_version: u32,
    #[serde(default)] pub app_version: String,
    pub name: String,
    #[serde(default)] pub author: String,
    #[serde(default)] pub tags: Vec<String>,
    pub kind: RackKind,
    pub payload: Payload,
    #[serde(default)] pub requires: Requires,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload { Device(Device) }  // B4 adds Chain(Chain)

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Requires { #[serde(default)] pub plugins: Vec<PluginNeed>, #[serde(default)] pub samples: Vec<SampleNeed> }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginNeed { pub format: PluginFormat, pub uid: String, pub name: String, pub vendor: String }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleNeed { pub path: PathBuf, pub hint: String, #[serde(default)] pub hash: Option<String>, #[serde(default)] pub size: Option<u64> }

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RackPresetError {
    #[error("not an AI-Track rack: {0}")]
    Malformed(String),
    #[error("this rack was saved by a newer AI-Track (rack format {found}, this version reads up to {supported}); update AI-Track to load it")]
    TooNew { found: u32, supported: u32 },
    #[error("device {0} is not a rack")]
    NotARack(DeviceId),
}

impl RackPreset {
    pub fn to_json(&self) -> String;                                      // pretty
    pub fn from_json(json: &str) -> Result<Self, RackPresetError>;
    /// The rack device of the payload.
    pub fn device(&self) -> &Device;
}
/// The folder name of a kind: "audio" | "instrument" | "midi" | "drum".
pub fn kind_slug(kind: RackKind) -> &'static str;
```

- `from_json`: comprueba que `format == RACK_FORMAT` (si no, `Malformed("…")`); lee la versión con `format::peek_format_version` (l. 49), que da `TooNew` si es mayor que `RACK_FORMAT_VERSION`; si es menor, migra con `format::migrate_with(.., RACK_MIGRATIONS)` (l. 68). Cualquier otro `LoadError` se convierte en `Malformed(e.to_string())`. Exige que el payload sea `DeviceKind::Rack(r)` con `r.kind == kind`.

- [ ] **Paso 1: test que falla** en `rack_preset.rs`, `mod tests`:

```rust
fn sample() -> RackPreset {
    let mut rack = crate::racks::fresh_rack(RackKind::Audio);
    rack.chains[0].devices.push(Device::stock(DeviceId(2), "fx.compressor", "Comp"));
    let device = Device { kind: DeviceKind::Rack(rack), ..Device::stock(DeviceId(1), "rack.audio", "Glue") };
    RackPreset { format: RACK_FORMAT.into(), format_version: RACK_FORMAT_VERSION, app_version: "test".into(), name: "Glue".into(), author: String::new(), tags: vec!["bus".into()], kind: RackKind::Audio, payload: Payload::Device(device), requires: Requires::default() }
}

#[test]
fn a_rack_file_reads_back_and_a_newer_one_is_refused() {
    let p = sample();
    let json = p.to_json();
    assert_eq!(RackPreset::from_json(&json).unwrap(), p);
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!((v["format"].as_str(), v["payload"]["device"]["kind"]["type"].as_str()), (Some("aitrack-rack"), Some("rack")));
    v["format_version"] = 99.into();
    assert_eq!(RackPreset::from_json(&v.to_string()), Err(RackPresetError::TooNew { found: 99, supported: RACK_FORMAT_VERSION }));
    assert!(RackPresetError::TooNew { found: 99, supported: 1 }.to_string().contains("newer AI-Track"));
    v["format_version"] = 1.into();
    v["format"] = "aitrack-project".into();
    assert!(matches!(RackPreset::from_json(&v.to_string()), Err(RackPresetError::Malformed(_))));
    assert!(matches!(RackPreset::from_json("not json"), Err(RackPresetError::Malformed(_))));
    // The kind must be the payload's.
    let mut w: serde_json::Value = serde_json::from_str(&json).unwrap();
    w["kind"] = "drum".into();
    assert!(matches!(RackPreset::from_json(&w.to_string()), Err(RackPresetError::Malformed(_))));
    // Missing optional fields default; unknown ones are ignored.
    let mut x: serde_json::Value = serde_json::from_str(&json).unwrap();
    x.as_object_mut().unwrap().remove("requires");
    x["later_field"] = 1.into();
    assert_eq!(RackPreset::from_json(&x.to_string()).unwrap().requires, Requires::default());
    assert_eq!((kind_slug(RackKind::Audio), kind_slug(RackKind::Instrument), kind_slug(RackKind::Midi), kind_slug(RackKind::Drum)), ("audio", "instrument", "midi", "drum"));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-model rack_preset` → FALLA al compilar. Este test usa `fresh_rack`, de A2.2: si A2.2 aún no se ha mezclado, se usa `Rack::new` con una cadena a mano.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-model rack_preset` → PASA.
- [ ] **Paso 5:** commit `model: the .aitrack-rack format, versioned like the project`.

**Hecho:** el test pasa y el módulo no hace E/S (`grep -n "std::fs" crates/model/src/rack_preset.rs` no encuentra nada).

### A3.2 · Capturar e instanciar un rack · carril M-PRESET · → A3.1

**Ficheros:**
- Modificar: `crates/model/src/rack_preset.rs`
- Modificar: `crates/model/src/tracks.rs` (`fn copy_devices` l. 149 y `fn relink_devices` l. 168 pasan a `pub(crate) fn`)

**Interfaces:**
- Produce:

```rust
impl RackPreset {
    /// The rack `rack` of `p`, ready to save. Sidechains are cleared, macro
    /// mappings and modulator targets that point outside the rack are dropped,
    /// and `requires` lists every hosted plugin (by format + uid, once) and
    /// every file a device state names (`state_paths`, once, `hint` = file name).
    pub fn from_device(p: &Project, rack: DeviceId, name: &str, app_version: &str) -> Result<Self, RackPresetError>;
    /// A copy with fresh ids from `p` (racks all the way down), its macro and
    /// modulator links pointing at the copies, named after the preset.
    /// Only allocates ids: the caller adds it with one `Command::AddDevice`.
    pub fn instantiate(&self, p: &mut Project) -> Device;
    /// What this rack needs that is not here, in `requires` order.
    pub fn missing(&self, has_plugin: impl Fn(&PluginNeed) -> bool, exists: impl Fn(&Path) -> bool) -> Vec<Missing>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing { Plugin(PluginNeed), Sample(SampleNeed) }
// Display: "plugin {name} ({VST3|CLAP}) is not installed" | "sample {hint} is missing"

/// The files a stock device state names: `extra.path` and `extra.sfz_path`
/// (Sampler, `slice.rs:176`; Granular), when they are non-empty strings.
pub fn state_paths(state: &[u8]) -> Vec<PathBuf>;
```

- `instantiate`: `copy_devices(p, std::slice::from_ref(self.device()), &mut map)`, después `relink_devices(&mut copies, &map, &|t| t)`; el nombre del rack es `self.name` y `preset = Some(self.name.clone())`.

- [ ] **Paso 1: tests que fallan** en `rack_preset.rs`. Antes, un `fn rig()` que arma con `Command`s una pista de audio con:
  - un EQ fuera del rack (`out_eq`) y un Audio Rack `R`, cuyas cadenas son:
    - «A»: `Comp` (`fx.compressor`, sidechain a otra pista) y un Instrument Rack anidado `N` con un Sampler (`state = slice::sampler_state("/s/kick.wav")`);
    - «B»: un plugin VST3 (`uid "ABCD"`, nombre `"Acme Synth"`, vendor `"Acme"`, `state = Some(vec![1, 2, 3])`) y un `mod.lfo` con `mods = [bipolar(Comp p0, 0.5), bipolar(out_eq p1, 0.5)]`;
  - en `R`, `macros[0] → Comp p0` y `variations = [Variation { name: "Soft", values: vec![0.25; 16] }]`; en `N`, `macros[0] → Sampler p3`.

  Devuelve `(Project, TrackId, rack: DeviceId, comp: DeviceId)`.

```rust
#[test]
fn a_saved_rack_instantiates_with_fresh_ids_and_its_own_links() {
    let (p, t, rack, comp) = rig();
    let saved = RackPreset::from_device(&p, rack, "My Bus", "0.0.8").unwrap();
    let back = RackPreset::from_json(&saved.to_json()).unwrap();
    assert_eq!(back, saved);
    let mut q = p.clone();
    let d = back.instantiate(&mut q);
    assert_eq!((d.name.as_str(), d.preset.as_deref()), ("My Bus", Some("My Bus")));
    let ids = |d: &Device| { let mut v = Vec::new(); visit_devices(std::slice::from_ref(d), &mut |x| v.push(x.id)); v };
    let (old, new) = (ids(p.device(rack).unwrap()), ids(&d));
    assert_eq!(new.len(), old.len());
    assert!(new.iter().all(|id| !old.contains(id) && p.device(*id).is_none()), "every id is fresh");
    let DeviceKind::Rack(r) = &d.kind else { panic!() };
    let new_comp = r.chains[0].devices[0].id;
    assert_ne!(new_comp, comp);
    assert_eq!(r.macros[0].mappings[0].device, new_comp);
    assert_eq!(r.variations[0].name, "Soft");
    let DeviceKind::Rack(n) = &r.chains[0].devices[1].kind else { panic!() };
    assert_eq!(n.macros[0].mappings[0].device, n.chains[0].devices[0].id, "the nested rack's macro follows its copy");
    assert_eq!(r.chains[1].devices[0].state.as_deref(), Some(&[1u8, 2, 3][..]));
    assert_eq!(r.chains[1].devices[1].mods, vec![ModMapping::bipolar(ModTarget::Param { device: new_comp, param: 0 }, 0.5)], "the target outside the rack is not saved");
    visit_devices(std::slice::from_ref(&d), &mut |x| assert_eq!(x.sidechain, None));
    assert_eq!(back.requires.plugins, vec![PluginNeed { format: PluginFormat::Vst3, uid: "ABCD".into(), name: "Acme Synth".into(), vendor: "Acme".into() }]);
    assert_eq!(back.requires.samples.iter().map(|s| (s.path.clone(), s.hint.as_str())).collect::<Vec<_>>(), vec![(PathBuf::from("/s/kick.wav"), "kick.wav")]);
    // One AddDevice in the project it came from; undo is exact.
    let before = q.clone();
    let undo = Command::AddDevice { slot: DeviceSlot::Track { track: t, index: 0 }, device: Box::new(d) }.apply(&mut q).unwrap();
    undo.apply(&mut q).unwrap();
    assert_eq!(q.tracks, before.tracks);
}

#[test]
fn instantiating_twice_gives_two_independent_racks() {
    let (mut p, t, rack, _) = rig();
    let saved = RackPreset::from_device(&p, rack, "Bus", "0.0.8").unwrap();
    for i in 0..2 {
        let d = saved.instantiate(&mut p);
        Command::AddDevice { slot: DeviceSlot::Track { track: t, index: i }, device: Box::new(d) }.apply(&mut p).unwrap();
    }
    let racks: Vec<&Rack> = p.track(t).unwrap().devices.iter().take(2).map(|d| match &d.kind { DeviceKind::Rack(r) => r, _ => panic!() }).collect();
    assert_ne!(racks[0].macros[0].mappings[0].device, racks[1].macros[0].mappings[0].device);
    assert!(crate::routing::dangling_references(&p).is_empty());
}

#[test]
fn what_a_rack_needs_is_listed_and_what_is_missing_is_named() {
    let (p, _, rack, _) = rig();
    let saved = RackPreset::from_device(&p, rack, "Bus", "0.0.8").unwrap();
    let gone: Vec<String> = saved.missing(|_| false, |_| false).iter().map(ToString::to_string).collect();
    assert_eq!(gone, ["plugin Acme Synth (VST3) is not installed", "sample kick.wav is missing"]);
    assert!(saved.missing(|n| n.uid == "ABCD", |f| f == Path::new("/s/kick.wav")).is_empty());
    assert_eq!(RackPreset::from_device(&p, p.tracks[1].devices[0].id, "x", "v"), Err(RackPresetError::NotARack(p.tracks[1].devices[0].id)), "the EQ outside is not a rack");
}

#[test]
fn state_paths_reads_the_sampler_file_and_ignores_the_rest() {
    assert_eq!(state_paths(&crate::slice::sampler_state(Path::new("/a/b.wav"))), vec![PathBuf::from("/a/b.wav")]);
    assert_eq!(state_paths(br#"{"version":1,"extra":{"path":null,"sfz_path":"/k.sfz"}}"#), vec![PathBuf::from("/k.sfz")]);
    assert!(state_paths(b"\x00\x01 not json").is_empty() && state_paths(br#"{"extra":{"path":""}}"#).is_empty());
}
```

  (Si `rig()` deja el EQ exterior en otra posición, se ajusta el índice `p.tracks[1].devices[0]`; `dangling_references` está en `routing.rs` y ya lo usa `through_history`, `command.rs:2511`.)

- [ ] **Paso 2:** `cargo test -p aitrack-model rack_preset` → FALLA al compilar.
- [ ] **Paso 3:** implementar; hacer `pub(crate)` las dos funciones de `tracks.rs`.
- [ ] **Paso 4:** `cargo test -p aitrack-model` → PASA.
- [ ] **Paso 5:** commit `model: save a rack as a preset and load it back with fresh ids`.

**Hecho:** los 5 tests de `rack_preset` pasan y `cargo test -p aitrack-model tracks::` sigue verde.

### A1.3 · Group, Ungroup y Fold en la app · carril A-EDIT · → A1.1, A1.2

**Ficheros:**
- Modificar: `crates/app/src/editing/racks.rs` (`ungroup_devices` l. 117-150: `freed` sale de los `Command::MoveDevice { id, .. }`; nuevo `fold_device`; tests)
- Modificar: `crates/app/src/views/devices.rs` (`toggle_fold`, l. 562-577: llama a `ws.s.fold_device(id)`)

**Interfaces:**
- Consume: `Command::SetDeviceCollapsed` (A1.2); `racks::{group_commands, ungroup_commands}` (A1.1).
- Produce: `impl AppState { pub fn fold_device(&mut self, id: DeviceId) -> bool }`. Es un paso sellado con etiqueta «Fold device» o «Show device» que invierte `collapsed`; devuelve `false` si el dispositivo no existe.

- [ ] **Paso 1: test que falla** en `editing/racks.rs`, `mod tests` (usa `rack_with_filter`, l. 376):

```rust
#[test]
fn group_fold_and_ungroup_keep_a_macro_mapping_through_undo_and_redo() {
    let (mut s, rack, filter) = rack_with_filter();
    s.map_macro_param(rack, 0, filter, 0, 0.5).unwrap();
    let mapped = |s: &AppState| s.rack_of(rack).unwrap().macros[0].mappings.iter().any(|m| m.device == filter);
    let steps = s.history.steps().count();
    s.click_device(filter, false, false);
    let inner = s.group_devices().unwrap();
    assert!(mapped(&s), "grouping inside the rack keeps the mapping");
    assert!(s.fold_device(inner));
    assert!(s.project.device(inner).unwrap().collapsed && mapped(&s));
    assert!(s.ungroup_devices());
    assert_eq!(s.selected_devices(), vec![filter]);
    assert!(mapped(&s));
    assert_eq!(s.history.steps().count(), steps + 3, "group, fold and ungroup are a step each");
    for _ in 0..3 {
        s.undo();
        assert!(mapped(&s));
    }
    for _ in 0..3 {
        s.redo();
    }
    assert!(mapped(&s) && s.project.device(inner).is_none());
    assert!(!s.fold_device(DeviceId(9999)));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-app editing::racks` → FALLA al compilar (`no method fold_device`).
- [ ] **Paso 3:** implementar; en `toggle_fold` quitar el `Remove + Add`.
- [ ] **Paso 4:** `cargo test -p aitrack-app editing::racks` → PASA (incluido `clicks_pick_devices_and_cmd_g_makes_a_rack`, l. 356); `cargo test -p aitrack-app views::devices` → PASA.
- [ ] **Paso 5:** commit `app: folding a device no longer removes it; ungroup picks the moved devices`.

**Hecho:** `grep -n "RemoveDevice" crates/app/src/views/devices.rs` no encuentra nada.

### A2.3 · Un rack nuevo con su cadena, y zona de soltar en racks viejos sin cadenas · carril A-STATE · → A2.2

**Ficheros:**
- Modificar: `crates/app/src/state.rs` (`add_stock_device_in`, l. 1816-1840: `device.kind = DeviceKind::Rack(racks::fresh_rack(kind))` en l. 1837; test en `mod tests`, l. 3018)
- Modificar: `crates/app/src/views/devices/rack_ui.rs` (`chain_column`, l. 375: `usable` también es verdadero en un rack no Drum sin cadenas. La zona «Drop devices here» aparece entonces, y `move_device` ya crea la cadena 0 al soltar, `devices.rs:256-265`.)

**Interfaces:**
- Consume: `racks::fresh_rack` (A2.2).

- [ ] **Paso 1: test que falla** en `state.rs`, `mod tests`:

```rust
#[test]
fn a_new_rack_has_one_empty_chain_in_one_undo_step_and_a_drum_rack_none() {
    use aitrack_model::racks::{self, Container};
    let mut s = AppState::new(Project::new("t"));
    let t = s.add_track(TrackKind::Audio).unwrap();
    let steps = s.history.steps().count();
    let mut audio = None;
    for (id, chains) in [("rack.audio", 1), ("rack.instrument", 1), ("rack.midi", 1), ("rack.drum", 0)] {
        let r = s.add_stock_device(t, id, id).unwrap();
        assert_eq!(s.rack_of(r).unwrap().chains.len(), chains, "{id}");
        audio = audio.or(Some(r));
    }
    assert_eq!(s.history.steps().count(), steps + 4);
    let rack = audio.unwrap();
    let eq = s.add_stock_device_in(t, Some(Container::Rack { rack, chain: 0 }), "fx.eq", "EQ").unwrap();
    assert_eq!(racks::device_slot(&s.project, eq), Some(aitrack_model::command::DeviceSlot::Rack { rack, chain: 0, index: 0 }));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-app a_new_rack_has_one_empty_chain` → FALLA (0 cadenas).
- [ ] **Paso 3:** implementar los dos cambios. En `rack_ui.rs` no cambia nada más: el Drum Rack sigue mostrando sus pads vacíos con el nombre de la nota (l. 704).
- [ ] **Paso 4:** `cargo test -p aitrack-app` → PASA.
- [ ] **Paso 5:** commit `app: a new rack comes with an empty chain ready for devices`.

**Hecho:** el test pasa y en la captura de A5.1 (`rack=audio`) se ven la tira «Chain 1» con «+ Device» y la zona «Drop devices here».

### A4.2 · Cuatro racks de fábrica · carril STOCK · → A3.2, A4.1

**Ficheros:**
- Crear: `crates/stock/examples/racks_factory.rs`. Construye cada rack en un `Project` de usar y tirar con `aitrack_model` (dev-dependency) y saca los valores normalizados de los parámetros de `PresetBuilder::new(id, "x", "y").set(param, plain).build().1.params` (`presets.rs:142-180`), con las claves pasadas a `u32`. Escribe `racks_dir()/<kind_slug>/<Nombre>.aitrack-rack` con `RackPreset::from_device(…, env!("CARGO_PKG_VERSION"))`, `author = "AI-Track"` y sus `tags`.
- Crear (generados, se versionan): los 4 ficheros de la tabla.
- Modificar: `crates/stock/src/racks.rs` (test de validación).

**Racks** (solo dispositivos y parámetros que existen; los valores son *plain*, los mapeos de macro van en *plain* y el generador los normaliza con el `ParamInfo` del dispositivo):

| Fichero | Cadenas | Macros |
|---|---|---|
| `audio/Parallel Compression.aitrack-rack` | «Dry» vacía (pasa la señal, A2.1); «Crushed», con volumen −6 dB: `fx.compressor` con `p::THRESHOLD` −40, `RATIO` 10, `ATTACK` 1, `RELEASE` 80, `MAKEUP` 12 | 1 «Squash» → `THRESHOLD` −20…−50; 2 «Blend» → `OUTPUT` −24…0 |
| `audio/Drum Bus Glue.aitrack-rack` | «Bus»: `fx.drumbus` (`DRIVE` 30, `CRUNCH` 10, `BOOM_AMOUNT` 20, `COMPRESS` 40), `fx.compressor` (`THRESHOLD` −18, `RATIO` 2, `ATTACK` 10, `RELEASE` 120), `fx.limiter` (`CEILING` −1) | 1 «Drive» → drumbus `DRIVE` 0…80; 2 «Boom» → `BOOM_AMOUNT` 0…60; 3 «Crunch» → `CRUNCH` 0…60; 4 «Glue» → compressor `THRESHOLD` −6…−30 |
| `audio/Filter Sweep.aitrack-rack` | «Filter»: `fx.filter` (`TYPE` 0 = Low-pass, `CUTOFF` 20 000, `RESONANCE` 20) | 1 «Cutoff» → `CUTOFF` 200…20 000; 2 «Resonance» → `RESONANCE` 0…70 |
| `midi/Chord Arp.aitrack-rack` | «Chords»: `midi.chord` (por defecto: 3 voces), `midi.arp` (`GATE` 60) | 1 «Gate» → arp `GATE` 10…150; 2 «Swing» → `SWING` 0…50; 3 «Octaves» → `OCTAVES` 1…4 |

Los ids de los parámetros salen de `fx::compressor::p` (l. 57-82), `fx::drumbus::p` (l. 61-80), `fx::limiter::p` (l. 44-60), `fx::filter::p` (l. 22-40), `midi::arp::p` (l. 40-57) y `midi::chord::p` (l. 32-40): todos son `pub mod p`. Cada macro mapeado se renombra con su nombre de la tabla (`Macro.name`).

- [ ] **Paso 1: test que falla** en `src/racks.rs`, `mod tests`:

```rust
#[test]
fn every_factory_rack_loads_and_uses_real_stock_devices_and_params() {
    use aitrack_model::rack_preset::{RackPreset, kind_slug};
    use aitrack_model::{DeviceKind, visit_devices};
    let all = factory();
    assert_eq!(all.len(), 4);
    for (rel, json) in all {
        let preset = RackPreset::from_json(json).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(*rel, format!("{}/{}.aitrack-rack", kind_slug(preset.kind), preset.name));
        assert!(preset.requires.plugins.is_empty() && preset.requires.samples.is_empty(), "{rel}");
        let mut inside = Vec::new();
        visit_devices(std::slice::from_ref(preset.device()), &mut |d| {
            inside.push(d.id);
            match &d.kind {
                DeviceKind::Stock { plugin_id } => {
                    let proc = crate::create(plugin_id).unwrap_or_else(|| panic!("{rel}: no stock device {plugin_id}"));
                    for k in d.params.keys() {
                        assert!(proc.info().params.iter().any(|i| i.id == *k), "{rel}: {plugin_id} has no param {k}");
                    }
                }
                DeviceKind::Rack(_) => {}
                DeviceKind::Plugin { .. } => panic!("{rel}: a factory rack uses only stock devices"),
            }
        });
        let DeviceKind::Rack(r) = &preset.device().kind else { panic!("{rel}") };
        let mapped: Vec<_> = r.macros.iter().flat_map(|m| &m.mappings).collect();
        assert!(!mapped.is_empty(), "{rel}: a factory rack has its macros mapped");
        assert!(mapped.iter().all(|m| inside.contains(&m.device) && (0.0..=1.0).contains(&m.min) && (0.0..=1.0).contains(&m.max)), "{rel}");
    }
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-stock racks::` → FALLA (`0 != 4`).
- [ ] **Paso 3:** escribir el generador; `cargo run -p aitrack-stock --example racks_factory` → escribe 4 ficheros en `crates/stock/racks/`.
- [ ] **Paso 4:** `cargo test -p aitrack-stock` → PASA (incluidos `registry_ids_match_infos` y `tests/robustness.rs`).
- [ ] **Paso 5:** commit `stock: four factory racks built from stock devices`.

**Hecho:** `ls crates/stock/racks/*/` lista los 4 ficheros y volver a ejecutar el generador no cambia nada (`git diff --stat crates/stock/racks` vacío).

### A3.3 · Biblioteca de racks del usuario · carril A-LIB · → A3.1, A4.1

**Ficheros:**
- Crear: `crates/app/src/rack_library.rs`
- Modificar: `crates/app/src/main.rs` (`mod rack_library;` tras `mod project_io;`, l. 32)

**Interfaces:**
- Consume: `RackPreset`, `kind_slug` (A3.1); `aitrack_stock::racks::factory()` (A4.1); `crate::views::device_window::presets::clean_name` (`presets.rs:89`); `crate::project_io::write_atomic` (`project_io.rs:99`); `aitrack_library::scan::quick_hash` (`library/src/scan.rs:38`).
- Produce:

```rust
pub const EXT: &str = "aitrack-rack";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RackSource { Factory(&'static str), User(PathBuf) }

#[derive(Clone, Debug, PartialEq)]
pub struct RackEntry { pub source: RackSource, pub name: String, pub kind: RackKind, pub tags: Vec<String> }
impl RackEntry { pub fn user(&self) -> bool; }

/// `<config>/user_racks` (`appearance::config_dir`, so `AITRACK_CONFIG_DIR` applies).
pub fn root() -> PathBuf;
/// The user's racks (by kind folder, then name, case-insensitive), then the
/// factory ones (in `factory()` order). A user file is listed by its stem
/// without being read; factory names and tags come from their JSON.
pub fn list(root: &Path) -> Vec<RackEntry>;
pub fn read(source: &RackSource) -> Result<RackPreset, String>;
/// Writes `<root>/<kind>/<clean name>.aitrack-rack` with the name inside set
/// to the clean one; returns the entry and whether it replaced a file.
pub fn save(root: &Path, preset: &RackPreset) -> Result<(RackEntry, bool), String>;
/// Only a user rack; a factory one is an error.
pub fn delete(entry: &RackEntry) -> Result<(), String>;
/// Size and quick hash (hex) of every sample that exists, before saving.
pub fn stamp_samples(preset: &mut RackPreset);
```

- `save`: «Give the rack a name» si `clean_name` devuelve `None`; «Cannot save the rack: {e}» si falla la E/S. `read` de un fichero roto devuelve el `Display` del `RackPresetError` («not an AI-Track rack: …»).

- [ ] **Paso 1: tests que fallan** en `rack_library.rs` (raíz temporal `std::env::temp_dir().join(format!("aitrack-racks-{}-{name}", std::process::id()))`, borrada al final):

```rust
fn preset(name: &str) -> RackPreset {
    // A minimal audio rack (fresh_rack + an EQ), as in rack_preset's tests.
}

#[test]
fn a_saved_rack_is_listed_by_kind_reads_back_and_can_be_deleted() {
    let root = tmp("save");
    let (e, replaced) = save(&root, &preset("Warm / Bus: v2")).unwrap();
    assert!(!replaced && e.user() && e.name == "Warm  Bus v2" && e.kind == RackKind::Audio);
    assert!(matches!(&e.source, RackSource::User(p) if p.ends_with("audio/Warm  Bus v2.aitrack-rack")));
    assert!(save(&root, &preset("Warm / Bus: v2")).unwrap().1, "the same name replaces");
    std::fs::write(root.join("audio/notes.txt"), "x").unwrap();
    let mine: Vec<RackEntry> = list(&root).into_iter().filter(RackEntry::user).collect();
    assert_eq!(mine.len(), 1, "other files are not racks");
    assert_eq!(read(&mine[0].source).unwrap().name, "Warm  Bus v2");
    assert!(save(&root, &preset("///")).is_err());
    let factory: Vec<RackEntry> = list(&root).into_iter().filter(|e| !e.user()).collect();
    assert_eq!(factory.len(), aitrack_stock::racks::factory().len());
    if let Some(f) = factory.first() {
        assert!(delete(f).is_err(), "a factory rack cannot be deleted");
    }
    delete(&mine[0]).unwrap();
    assert!(list(&root).iter().all(|e| !e.user()));
}

#[test]
fn a_broken_rack_file_is_listed_but_does_not_load() {
    let root = tmp("broken");
    std::fs::create_dir_all(root.join("audio")).unwrap();
    std::fs::write(root.join("audio/Broken.aitrack-rack"), "not json").unwrap();
    let e = list(&root).into_iter().find(RackEntry::user).unwrap();
    assert_eq!(e.name, "Broken");
    assert!(read(&e.source).unwrap_err().contains("not an AI-Track rack"));
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-app rack_library` → FALLA al compilar.
- [ ] **Paso 3:** implementar.
- [ ] **Paso 4:** `cargo test -p aitrack-app rack_library` → PASA.
- [ ] **Paso 5:** commit `app: the user's racks on disk, next to the factory ones`.

**Hecho:** los dos tests pasan; nada escribe fuera de la raíz que recibe `list` o `save`.

### A3.4 · Guardar y cargar en `AppState` · carril A-STATE · → A2.3, A3.2

**Ficheros:**
- Crear: `crates/app/src/editing/rack_presets.rs`
- Modificar: `crates/app/src/editing.rs` (`mod rack_presets;` tras `mod racks;`, l. 22)
- Modificar: `crates/app/src/state.rs`: extraer de `add_stock_device_in` (l. 1817-1827) `pub(crate) fn insert_slot(&self, track: TrackId, into: Option<racks::Container>, plugin_id: &str) -> Option<DeviceSlot>`, que conserva la regla de que un MIDI va tras el último efecto MIDI, y hacer que `add_stock_device_in` lo use.

**Interfaces:**
- Consume: `RackPreset::{from_device, instantiate, missing}` (A3.2); `AppState::capture_plugin_states` (`state.rs:2254`).
- Produce:

```rust
pub struct RackLoad { pub device: DeviceId, pub missing: Vec<String> }

impl AppState {
    /// Store `states` (what the live plugins report) without an undo step,
    /// then the rack as a preset named `name`. Also returns the names of the
    /// hosted plugins in the rack whose state was not among `states`.
    pub fn rack_preset_for_save(&mut self, rack: DeviceId, name: &str, states: Vec<(DeviceId, Vec<u8>)>) -> Result<(RackPreset, Vec<String>), String>;
    /// Add `preset` at the end of `track`'s chain (a MIDI rack after the MIDI
    /// effects) as one sealed undo step "Add rack"; `missing` says, in words,
    /// what it needs that is not installed (`installed`) or not on disk (`exists`).
    pub fn add_rack_preset(&mut self, track: TrackId, preset: &RackPreset, installed: &[crate::bridge::ExternalPlugin], exists: &dyn Fn(&std::path::Path) -> bool) -> Option<RackLoad>;
}
```

  `installed` se compara por `(format, uid)`; `app_version` = `env!("CARGO_PKG_VERSION")`.

- [ ] **Paso 1: tests que fallan** en `editing/rack_presets.rs`:

```rust
#[test]
fn a_rack_saved_from_one_track_loads_on_another_in_one_undo_step_with_its_mapping() {
    let mut s = AppState::new(Project::new("t"));
    let a = s.add_track(TrackKind::Audio).unwrap();
    let rack = s.add_stock_device(a, "rack.audio", "Glue").unwrap();
    let comp = s.add_stock_device_in(a, Some(Container::Rack { rack, chain: 0 }), "fx.compressor", "Comp").unwrap();
    s.map_macro_param(rack, 0, comp, 0, 0.5).unwrap();
    let steps = s.history.steps().count();
    let (preset, uncaptured) = s.rack_preset_for_save(rack, "Glue", Vec::new()).unwrap();
    assert!(uncaptured.is_empty());
    assert_eq!(s.history.steps().count(), steps, "saving is not an edit");
    let b = s.add_track(TrackKind::Audio).unwrap();
    let steps = s.history.steps().count();
    let load = s.add_rack_preset(b, &preset, &[], &|_| true).unwrap();
    assert!(load.missing.is_empty());
    assert_eq!(s.history.steps().count(), steps + 1);
    assert_eq!(s.project.device_track(load.device), Some(b));
    let r = s.rack_of(load.device).unwrap();
    let new_comp = r.chains[0].devices[0].id;
    assert_ne!(new_comp, comp);
    assert_eq!(r.macros[0].mappings[0].device, new_comp);
    s.undo();
    assert!(s.project.device(load.device).is_none());
}

#[test]
fn saving_names_the_plugins_whose_state_was_not_captured_and_loading_names_what_is_missing() {
    let mut s = AppState::new(Project::new("t"));
    let a = s.add_track(TrackKind::Instrument).unwrap();
    let rack = s.add_stock_device(a, "rack.instrument", "Keys").unwrap();
    let into = Some(Container::Rack { rack, chain: 0 });
    let synth = crate::bridge::ExternalPlugin { uid: "ABCD".into(), name: "Acme Synth".into(), vendor: "Acme".into(), format: PluginFormat::Vst3, category: "Instrument".into(), path: "/x.vst3".into(), is_instrument: true };
    let plugin = s.add_plugin_device_in(a, into, &synth).unwrap();
    let sampler = s.add_stock_device_in(a, into, "instrument.sampler", "Sampler").unwrap();
    s.edit("state", vec![Command::SetDeviceState { device: sampler, state: Some(aitrack_model::slice::sampler_state(std::path::Path::new("/nowhere/kick.wav"))), preset: None }]);
    let (_, uncaptured) = s.rack_preset_for_save(rack, "Keys", Vec::new()).unwrap();
    assert_eq!(uncaptured, ["Acme Synth"]);
    let (preset, uncaptured) = s.rack_preset_for_save(rack, "Keys", vec![(plugin, vec![9])]).unwrap();
    assert!(uncaptured.is_empty());
    let DeviceKind::Rack(r) = &preset.device().kind else { panic!() };
    assert_eq!(r.chains[0].devices[0].state.as_deref(), Some(&[9u8][..]), "the captured state is in the file");
    let load = s.add_rack_preset(a, &preset, &[], &|_| false).unwrap();
    assert_eq!(load.missing, ["plugin Acme Synth (VST3) is not installed", "sample kick.wav is missing"]);
    assert!(s.add_rack_preset(a, &preset, &[synth], &|_| true).unwrap().missing.is_empty());
}

#[test]
fn a_midi_rack_preset_goes_after_the_midi_effects() {
    // Track with midi.arp then fx.eq; a MIDI rack preset lands at index 1.
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-app editing::rack_presets` → FALLA al compilar.
- [ ] **Paso 3:** implementar `insert_slot` y los dos métodos.
- [ ] **Paso 4:** `cargo test -p aitrack-app` → PASA (`add_stock_device_in` conserva su comportamiento: `devices_can_be_added_moved_and_removed_inside_a_drum_rack_return`, `editing/racks.rs:550`).
- [ ] **Paso 5:** commit `app: save a rack as a preset and add one to a track in one step`.

**Hecho:** los 3 tests pasan.

### A3.5 · «Save as Preset…» en el rack · carril A-UI · → A1.3, A3.3, A3.4

**Ficheros:**
- Crear: `crates/app/src/views/devices/rack_presets.rs`
- Modificar: `crates/app/src/views/devices.rs`:
  - `pub mod rack_presets;` junto a `pub mod rack_ui;` (l. 28);
  - en la cabecera de `card` (l. 437-519), tras el botón de macros (l. 471-484), un botón `Icon::Save` de 20 px solo para `DeviceKind::Rack`, con la ayuda «Save the rack as a preset of yours».
- Modificar: `crates/app/src/views/device_ui/mod.rs` (`PopKind::SaveRack(DeviceId)`, l. 250-266; rama en `overlay`, l. 589-597)
- Modificar: `crates/app/src/views/device_window/chrome.rs`: `name_field` (l. 212-227) pasa a `pub(crate) fn name_field(ws, device, placeholder: &'static str, submit: fn(&mut Workspace, DeviceId, &str, &mut Context<Workspace>), cx)`. Sus dos usos (l. 235 y l. 466) pasan `"Preset name", save`.
- Modificar: `crates/app/src/editing/menus.rs`: en `device_menu` (l. 503-575), si `device` es un rack, una fila `action("device-save-rack", Icon::Save, "Save as Preset…", …)` antes del divisor. Antes de construir la tarjeta se guarda `ws.menu.as_ref().map(|m| m.at)` para colocar el popover.

**Interfaces:**
- Consume: `AppState::rack_preset_for_save` (A3.4); `rack_library::{root, save, stamp_samples}` (A3.3); `EngineBridge::plugin_states` (`bridge.rs:260`).
- Produce, en `views/devices/rack_presets.rs`:
  - `pub fn open_save(ws: &mut Workspace, rack: DeviceId, at: Point<Pixels>, window: Option<&mut Window>, cx: &mut Context<Workspace>)`: sugiere el nombre del rack y enfoca el campo si hay `window`;
  - `pub fn save_card(ws, rack, cx) -> Option<AnyElement>`: el mismo formato que `chrome::save_card` (l. 463-492), con la cabecera «SAVE RACK» y el texto «Kept with your racks, in every project.»;
  - `pub fn save(ws: &mut Workspace, rack: DeviceId, name: &str, cx: &mut Context<Workspace>)`: `states = ws.b.engine.plugin_states()` → `rack_preset_for_save` → `stamp_samples` → `rack_library::save(&root(), …)` → toast «Saved {name}» o «Updated {name}». Si hubo plugins sin capturar, otra toast: «{names}: the state of {this plugin | these plugins} could not be read (sandbox?), the rack keeps the last one saved». Después, `ws.palette.invalidate_racks()` (A3.6; hasta entonces, un `// A3.6` sin la llamada) y `ws.fx.pop = None`.

- [ ] **Paso 1:** no hay test de vista nuevo: la lógica está probada en A3.4. `cargo test -p aitrack-app device_window` → PASA antes y después; el cambio de `name_field` no debe romper `reset_puts_every_parameter_back_in_one_undo_step` (`chrome.rs:500`).
- [ ] **Paso 2:** implementar.
- [ ] **Paso 3:** `cargo check -p aitrack-app` y `cargo clippy -p aitrack-app --all-targets -- -D warnings` → verdes.
- [ ] **Paso 4:** commit `app: save a rack as a preset from its title bar or its menu`.

**Hecho:** la captura `racksave` de A5.1 muestra el popover con el nombre del rack; al pulsar Save aparece `user_racks/audio/<Nombre>.aitrack-rack` bajo un `AITRACK_CONFIG_DIR` temporal.

### A3.6 · Los racks en Cmd+K: Enter y arrastre · carril A-UI · → A3.5

**Ficheros:**
- Modificar: `crates/app/src/views/palette/mod.rs`:
  - `Item::Rack(RackEntry)` (enum l. 161-173);
  - `racks: Option<Vec<RackEntry>>` en `PaletteState` (junto a `presets`, l. 243) y `pub fn invalidate_racks(&mut self)`;
  - en `ensure` (l. 414-424), `rack_library::list(&rack_library::root())`;
  - `rebuild`: en All, con texto, sección «Racks» con `ALL_PRESETS` filas (antes de «Presets», l. 637-639); en la pestaña Presets (l. 704), secciones «Racks» y «Presets», las dos con cabecera;
  - `activate` (l. 1076-1127);
  - `render_rack` junto a `render_preset` (l. 1643-1658) y su rama en l. 2315-2316;
  - test en `mod tests` (l. 2380).
- Modificar: `crates/app/src/views/devices/rack_presets.rs` (`DraggedRack`, `add`)
- Modificar: `crates/app/src/views/devices.rs` (zona «add-device», l. 57-86: `drag_over::<DraggedRack>` + `on_drop`)

**Interfaces:**
- Consume: `rack_library::{RackEntry, RackSource, list, read, delete}` (A3.3); `AppState::add_rack_preset` (A3.4).
- Produce:
  - `pub(crate) fn rack_rows(list: &[RackEntry], q: &str) -> Vec<RackEntry>` (palette): `text_score(q, name, [etiqueta del tipo («Audio Effect Rack»…), "rack", tags…])`, ordenado con `ranked` (estable: sin texto, el orden de `list`);
  - `#[derive(Clone, Debug)] pub struct DraggedRack { pub source: RackSource, pub name: String }`, con `Render` como `DraggedDevice` (`devices.rs:193-210`) e `Icon::Grid`;
  - `pub fn add(ws: &mut Workspace, source: &RackSource, track: TrackId, cx: &mut Context<Workspace>) -> bool`: `read` y luego `add_rack_preset(track, &p, &ws.b.plugins.external_plugins().to_vec(), &|f| f.exists())`. Toast «Added {name}», o «Added {name} · missing: {…}»; al terminar abre la pestaña Devices (como `palette/mod.rs:1101-1106`).
- Fila de la paleta:
  - `Icon::Drum` si es Drum, si no `Icon::Grid`; título; subtítulo «{tipo} · Factory | Yours»; badge «Rack»;
  - `.on_drag(DraggedRack { … })`;
  - en los racks del usuario, botón `Icon::Trash` de 20 px (detiene la propagación) → `rack_library::delete`, toast «Deleted {name}», `invalidate_racks` + `rebuild`.
- Enter sin pista seleccionada → toast «Select a track first» (igual que los presets, l. 1095-1098).

- [ ] **Paso 1: test que falla** en `palette/mod.rs`, `mod tests`:

```rust
#[test]
fn racks_are_found_by_name_kind_and_tag() {
    use crate::rack_library::{RackEntry, RackSource};
    let e = |name: &str, kind, tags: &[&str], user: bool| RackEntry {
        source: if user { RackSource::User(format!("/r/{name}.aitrack-rack").into()) } else { RackSource::Factory("audio/x.aitrack-rack") },
        name: name.into(), kind, tags: tags.iter().map(|t| t.to_string()).collect(),
    };
    let list = vec![e("My Bus", RackKind::Audio, &[], true), e("Parallel Compression", RackKind::Audio, &["drums"], false), e("Chord Arp", RackKind::Midi, &[], false)];
    let names = |q: &str| rack_rows(&list, q).into_iter().map(|r| r.name).collect::<Vec<_>>();
    assert_eq!(names(""), ["My Bus", "Parallel Compression", "Chord Arp"], "yours first, then the factory ones");
    assert_eq!(names("midi"), ["Chord Arp"]);
    assert_eq!(names("drums"), ["Parallel Compression"]);
    assert_eq!(names("bus"), ["My Bus"]);
    assert!(names("zzz").is_empty());
}
```

- [ ] **Paso 2:** `cargo test -p aitrack-app palette::tests::racks_are_found` → FALLA al compilar.
- [ ] **Paso 3:** implementar; en A3.5, sustituir el `// A3.6` por `ws.palette.invalidate_racks()`.
- [ ] **Paso 4:** `cargo test -p aitrack-app palette` → PASA (incluido el test de cabeceras, l. 2406).
- [ ] **Paso 5:** commit `app: racks in the palette, added with Enter or by dragging into the chain`.

**Hecho:** con `AITRACK_STUBS=1`, `palette=presets` muestra la sección «Racks» con los 4 de fábrica: vienen embebidos y no dependen del catálogo stub, que no tiene presets.

### A5.1 · Palabras de vista, verificación visual y cierre del bloque A · carril A-UI · → A3.6

**Ficheros:**
- Modificar: `crates/app/src/workspace.rs` (`apply_dev_words`, rama `_`, l. 1348-1365, junto a `device=`):
  - `rack=audio|instrument|midi|drum` → `self.s.add_stock_device(track, "rack.<kind>", <nombre de plugins.rs:67-70>)` en la pista seleccionada. `device=rack.audio` no sirve: crea un dispositivo stock, no un rack (D13);
  - `rackpreset=<Nombre>` → busca en `rack_library::list(&root())` por nombre y llama a `views::devices::rack_presets::add`;
  - `racksave` → `rack_presets::open_save(self, <primer rack de la pista>, point(px(480.), px(300.)), None, cx)`.

  Cada palabra lleva su comentario de una línea, como las demás (l. 849, 859-861).

- [ ] **Paso 1:** implementar las tres palabras; `cargo build -p aitrack-app` en segundo plano (puede pasar de 10 min en un worktree nuevo).
- [ ] **Paso 2: capturas** (por PID y con `AITRACK_DEV_SIZE=1480x900`; `AITRACK_CONFIG_DIR` apunta a una carpeta absoluta de usar y tirar con `appearance.json` = `{"reduced_motion":true}`; nunca sin `AITRACK_DEV_HIDDEN=1`):
  1. `AITRACK_DEV_HIDDEN=1 AITRACK_STUBS=1 AITRACK_DEMO=1 AITRACK_DEV_VIEW=track=Vocals,devices,tall,rack=audio` → un Audio Effect Rack con la tira «Chain 1», «+ Device», la zona «Drop devices here» y el botón Save en su barra de título.
  2. `…,rack=audio,rackmacros,racksave` → el popover «SAVE RACK» con el nombre «Audio Effect Rack».
  3. `…,rackpreset=Parallel Compression,rackmacros` → dos cadenas («Dry», «Crushed») y los macros «Squash» y «Blend».
  4. `AITRACK_DEV_HIDDEN=1 AITRACK_STUBS=1 AITRACK_DEV_VIEW=palette=presets` → la sección «Racks» con los 4 de fábrica.
  5. `…,track=Lead,devices,tall,rack=drum` → un Drum Rack con los 16 pads vacíos, sin errores.
- [ ] **Paso 3: cierre** → `cargo check --workspace`; `cargo test -p aitrack-model -p aitrack-stock -p aitrack-engine -p aitrack-app`; `cargo clippy -p aitrack-model -p aitrack-stock -p aitrack-engine -p aitrack-app --all-targets -- -D warnings` → los tres verdes.
- [ ] **Paso 4:** revisión de Opus sobre una copia congelada (`rsync`, sin `target`, `reference` ni `content`), centrada en el Review Focus.
- [ ] **Paso 5:** commit `app: dev-view words to look at new racks, saved racks and the save popover`.

**Hecho:** las 5 capturas coinciden con lo descrito y los tres comandos de cierre están verdes.

---

## Bloque B · esquema (cada tarea se detalla en su propio plan)

### B1 · UI de cadenas
- **Objetivo:**
  - añadir una cadena con «+» en la cabecera «CHAINS» o soltando un dispositivo (o un `DraggedRack`) en una zona «Drop to add a chain» al pie de la lista;
  - renombrar con doble clic;
  - color (el popover de colores de pista, `MenuKind::TrackContext`, `workspace.rs:726`, como referencia);
  - reordenar arrastrando la fila, duplicar y mostrar u ocultar la lista.
- **Ficheros:**
  - `crates/app/src/views/devices/rack_ui.rs` (`chain_list` l. 562-621);
  - `crates/app/src/editing/racks.rs` (`add_chain`, `rename_chain`, `move_chain`, `duplicate_chain`);
  - `crates/model/src/command.rs`: **comando nuevo `MoveChain { rack, from, to }`**, porque no existe y `RemoveChain` + `AddChain` borrarían los mapeos (`command.rs:816-825`);
  - `crates/model/src/racks.rs`: `duplicate_chain_commands` con `copy_devices` + `relink_devices`, ya `pub(crate)` desde A3.2;
  - `RackUi` (l. 51-74): `hidden_lists: BTreeSet<DeviceId>` como estado de vista.
- **Depende de:** A1 y A2. Renombrar y cambiar el color no recompilan el grafo: `chain_diff` ignora `name` y `color`, `compile.rs:1145`.
- **Verifica:**
  - tests de modelo: `MoveChain` ida y vuelta con mapeos; duplicar da ids nuevos y el macro sigue en la cadena original;
  - tests de `AppState`: un paso de undo por acción, y `fx.selected` sigue a la cadena movida;
  - captura `rackgroup` (`workspace.rs:849-858`) con la lista oculta y sin ocultar.

### B2 · Samples en los pads del Drum Rack
- **Objetivo:** soltar un `DraggedSample` (`workspace.rs:471`) o `ExternalPaths` sobre un pad crea o sustituye su Sampler de un disparo, en un paso de undo, y el pad vacío muestra el texto «Drop samples here».
- **Ficheros:**
  - `crates/app/src/views/devices/rack_ui.rs` (`pads_column` l. 625-741: `on_drop` en los dos brazos del `match chain`, l. 640-705);
  - `crates/model/src/slice.rs`: `pub fn one_shot_sampler(id, path, name) -> Device`, que reutiliza `sampler_state` (l. 176) y las constantes privadas `P_MODE`/`MODE_ONE_SHOT` (l. 31…);
  - `crates/model/src/racks.rs`: `pad_sample_commands(p, rack, note, path, id) -> Vec<Command>`, que da `AddChain` con `note`, `key_range` y el Sampler, o, si el pad ya existe, `RemoveDevice` del Sampler viejo + `AddDevice`.
- **Coordinación con el asistente (D7):** en la fase 3b del asistente, D11 y D12 hablan del mismo ayudante del Sampler (`{"version":1,"extra":{"path":…}}`) y de su inverso. Este plan deja el inverso en `aitrack_model::rack_preset::state_paths` (A3.2). Hay que acordar **un solo** constructor: `slice::one_shot_sampler` en `aitrack-model`, que `aitrack-stock` no puede necesitar para esto y que `build_drum_kit` puede llamar desde `aitrack-ai`. `pad_sample` (`rack_ui.rs:745`) y `device_ui::kit::state_paths` pasan a usar `state_paths`.
- **Depende de:** A2.3 y A3.2. Va antes o a la vez que la fase 3b del asistente, nunca con dos ayudantes distintos.
- **Verifica:**
  - test de modelo: soltar en un pad vacío y en uno con Sampler, un paso cada uno;
  - test de `AppState` con una muestra `cloud` (placeholder, nunca se lee, ver memoria «cloud library roots»): se rechaza con una toast;
  - captura de un Drum Rack con 3 pads cargados.

### B3 · Macros: 8/16 guardado, renombrar, variaciones y mapeo MIDI
- **Objetivo:**
  - el botón 8/16 (`rack_ui.rs:49,1076`; hoy solo de vista) se guarda en el proyecto;
  - el macro se renombra con doble clic (`SetMacroDef` existe, `command.rs:211`);
  - la variación tiene Overwrite y Rename (`SetVariations`, `command.rs:212`).
- **Ficheros:**
  - `crates/model/src/project.rs`, en `Rack`: `#[serde(default)] pub shown_macros: u8` (8 o 16);
  - `crates/engine/src/compile.rs:1185-1186`, que desestructura `Rack` de forma exhaustiva y hay que actualizar;
  - `crates/model/src/command.rs` (`SetRackMacroCount`, o el campo dentro de un `SetMacroDef` ampliado);
  - `rack_ui.rs` (`macros_panel`);
  - `crates/app/src/state.rs:1771-1791` (variaciones).
- **Mapeo MIDI:** **no hay infraestructura.** No existe MIDI learn ni mapeo de un CC a un objetivo en el motor; `grep -ri "midi.learn\|midimap"` solo encuentra `host/src/vst3/instance.rs`. Hace falta un subsistema propio (tabla CC→objetivo en el proyecto, enrutado en el motor fuera del callback y modo Learn en la UI), con su propio plan.
- **Depende de:** A1. El mapeo MIDI depende de ese subsistema.
- **Verifica:**
  - proyecto viejo sin `shown_macros` → abre con 8;
  - renombrar es un paso de undo;
  - Overwrite cambia los valores sin cambiar el orden.

### B4 · Hot-Swap con presets y guardar una cadena sola
- **Objetivo:**
  - Hot-Swap: sustituir un rack o un dispositivo por un preset del mismo tipo desde la cabecera; hoy solo existe Alt+↑/↓ en stock (`workspace.rs:3382`);
  - guardar una cadena sola como `.aitrack-rack` y cargarla como cadena nueva de un rack.
- **Ficheros:**
  - `crates/model/src/rack_preset.rs`: `Payload::Chain(Chain)` e `instantiate_chain(&self, p) -> Chain`. `RackPreset::device()` pasa a devolver `Option`, y el test de A4.2 se ajusta;
  - `rack_library.rs` (`user_racks/chain/`);
  - menú de la fila de cadena (B1);
  - palette (filas de cadena: Enter → `AddChain` en el rack seleccionado).
- **Depende de:** A3 y B1.
- **Verifica:**
  - ida y vuelta de una cadena con mapeo de modulador interno;
  - Hot-Swap en un paso de undo;
  - un archivo de cadena abierto por una versión que solo conoce `device` da un error legible.

### B5 · Sin recompilar al arrastrar volumen, pan o zonas de cadena
- **Objetivo:** que arrastrar el volumen o el pan de una cadena, o una zona (key, vel o chain-select y sus fadas), no recompile el grafo (riesgo F.3 de la auditoría).
- **Ficheros:**
  - `crates/app/src/state.rs` (`FastEdit`, l. 89-156): variantes `ChainGain { rack, chain, gain }` y `ChainZone { … }`, y `of_all` acepta `SetChainProp` de `VolumeDb`, `Pan`, `*Range` y `*Fade`;
  - `crates/app/src/bridge.rs` (`fast_chain_gain`, `fast_chain_zone`, junto a `fast_rack_selector`, l. 154);
  - `crates/engine/src/compile.rs` (`chain_diff`, l. 1144-1160: devuelve `Patch` en vez de `false` para esos campos);
  - el enum `Patch` y su aplicación en el hilo RT, sin asignaciones;
  - `crates/engine/src/graph.rs` (`ChainNode.gain`, rangos y fadas).
- **Depende de:** nada del bloque A; se puede adelantar.
- **Verifica:**
  - test como `dragging_the_chain_selector_is_one_undo_step_and_goes_to_the_engine_live` (`editing/racks.rs:528`), con volumen de cadena: 0 `project_updates` durante el arrastre;
  - test de motor como `moving_chain_select_never_compiles` (`tests/racks.rs:495`);
  - `changing_a_chain_volume_does_not_step` (`tests/racks.rs:295`) sigue pasando.

---

## Discrepancias y huecos

| # | Dónde | Qué pasa | Propuesta | ¿Bloquea? |
|---|---|---|---|---|
| D1 | Encargo A3, «`PresetRow` hay que pasarlo de `&'static str` a `String`» | Para los racks no hace falta: el tipo y la etiqueta de un rack son `'static` (`plugins.rs:67-70`), y una fila de rack no es un preset de stock (no lleva `plugin_id`). El cambio a `String` solo hace falta si los presets *de usuario* de stock entran en la paleta. | `Item::Rack(RackEntry)` propio (A3.6); `PresetRow` no cambia. Los presets de usuario de stock en Cmd+K pasan a B4. | No |
| D2 | A1, «MoveDevice/Batch o comandos atómicos nuevos» | Group y Ungroup se resuelven con `AddDevice` + `MoveDevice` + `RemoveDevice`. Fold no puede: ningún comando cambia `collapsed` (enum `command.rs:122-254`). | `SetDeviceCollapsed` es el único comando nuevo (A1.2). | No |
| D3 | A1, Ungroup con varias cadenas | `08-ableton-live-manual.md` y `18-dispositivos-nativos-racks.md` no describen qué hace Live (grep de «ungroup» sin resultados). | Se mantiene el aviso actual (`editing.rs:566`) y `can_ungroup` (`racks.rs:136-138`). | No |
| D4 | A2, «comprueba graph.rs:724, 766-805» | Una cadena vacía ya deja pasar el audio (`graph.rs:775`) y el MIDI (`graph.rs:649-651`); el silencio de un rack nuevo viene de que **no tiene cadenas** (`graph.rs:724`). Lo único que hoy está mal con una cadena vacía es el Instrument o Drum Rack, que copia la entrada. | A2.1: `feeds_input`, y transparencia de los Audio y MIDI Rack sin cadenas (proyectos viejos). | No |
| D5 | A2, «Drum Rack = pads vacíos con "Drop samples here"» | Los pads vacíos ya se pintan (`rack_ui.rs:704`), pero hoy no aceptan nada al soltar. Escribir «Drop samples here» sin que soltar funcione promete algo que no existe. | El texto va con B2, junto con el `on_drop`. | No |
| D6 | Formato propuesto, `samples[].hash` | `aitrack-model` no hace E/S y no puede calcular el hash. | `SampleNeed { path, hint, hash: Option, size: Option }`; la app rellena `size` y `hash` con `quick_hash` al guardar (`rack_library::stamp_samples`, A3.3). Reenlazar por hash al cargar queda fuera (solo se avisa). | No |
| D7 | Plan del asistente, D11/D12 (fase 3b) | El asistente pone el ayudante del Sampler y su inverso en `aitrack-stock`, pero `rack_preset` necesita el inverso en `aitrack-model` (el modelo no depende de stock) y `sampler_state` ya vive en el modelo (`slice.rs:176`). | El inverso es `rack_preset::state_paths` (A3.2), y el constructor de un disparo, `slice::one_shot_sampler` (B2). La fase 3b los reutiliza; hay que avisar al dueño de ese plan antes de que empiece. | No (sí si la fase 3b empieza antes que B2 sin coordinarse) |
| D8 | Racks de fábrica «embebidos» | No pueden ir en `stock/presets/`: `build.rs` mete todos los `.json` en `FACTORY` y `presets::all()` los trataría como presets. Además, `aitrack-stock` no depende de `aitrack-model`. | Van en `stock/racks/<kind>/*.aitrack-rack`, en una tabla propia (A4.1); un generador en `examples/` y un test con la dev-dependency `aitrack-model` los validan (A4.2). | No |
| D9 | `copy_devices`/`relink_devices` (auditoría E) | Son privadas (`tracks.rs:149,168`). | Pasan a `pub(crate)` (A3.2). | No |
| D10 | Nota de merge (Fase 0 del asistente) | A2.3 y A3.4 tocan `state.rs` (l. 1798-1840 y `mod tests`), el mismo fichero que T0.2 del asistente (l. 439, 858…, 2846). Las líneas no se solapan. | El carril A-STATE empieza cuando T0.2 se haya mezclado, o mezcla a mano; no toca `is_drum_track`. | No |
| D11 | Cabecera del rack | Los racks no abren ventana propia (`device_window::can_open` solo admite stock, `mod.rs:48-50`): la «cabecera» es la barra de título del Device View. | Botón Save en `devices.rs` (A3.5). El popover se pinta en la ventana principal con `overlay(…, None)` (`workspace.rs:3880`). | No |
| D12 | Guardar una cadena sola | Necesita `Payload::Chain`, la acción en la fila de cadena (B1, aún no existe) y cargar como `AddChain` en un rack elegido. | Pasa a B4. El `Payload` de A3.1 ya es un enum, así que añadirlo no rompe los archivos de rack. | No |
| D13 | `AITRACK_DEV_VIEW=device=rack.audio` | Crea `Device::stock("rack.audio")`, un dispositivo stock y no un rack (`workspace.rs:1350-1357`). | Palabra nueva `rack=<kind>` (A5.1). | No |
| D14 | B3, mapeo MIDI | No hay infraestructura de MIDI learn ni de CC→objetivo en el motor. | Lo dice B3; necesita su propio plan. | Sí, para el mapeo MIDI de B3 (no para el resto) |
| D15 | Moduladores dentro del rack con objetivos fuera | Al cargar el rack en otra pista, el objetivo apuntaría a un id ajeno o inexistente. | `from_device` los quita (A3.2, con test); los macros que ya colgaban se quitan igual. | No |
| D16 | Encargo A3, «Cmd+K: pestaña Presets o Devices» | Devices lista *tipos* de dispositivo desde el catálogo de plugins; un rack guardado es «un dispositivo con ajustes», como un preset. | Pestaña Presets, más All cuando hay texto escrito (A3.6). | No |

Ninguna discrepancia bloquea el bloque A. D7 es una dependencia de coordinación con el plan del asistente. D14 bloquea solo el mapeo MIDI de B3.
